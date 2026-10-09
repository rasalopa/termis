//! The application: wires the agent's pty, the emulator, the host input
//! and the key encoder together.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::input::{self, Reader};
use crate::keys::{self, Modes};
use crate::pty::{self, ExitStatus, Session};
use crate::ui::{self, Pane};

pub mod host;

use host::Host;

/// Lines of output kept above the screen.
const SCROLLBACK: usize = 5000;
/// Longest wait between two passes of the main loop.
const TICK: Duration = Duration::from_millis(16);
/// Messages handled per pass before drawing, so a chatty child cannot
/// starve the screen.
const DRAIN: usize = 256;
/// A pending prefix is dropped after this.
const PREFIX_TIMEOUT: Duration = Duration::from_secs(1);
/// After the child exited, how long to wait for its output to end.
const CLOSE_GRACE: Duration = Duration::from_millis(200);
/// How long a hung-up child gets before it is killed.
const GRACE: Duration = Duration::from_secs(2);
/// How long a killed child gets to be reaped.
const KILL_GRACE: Duration = Duration::from_millis(500);

/// Everything the main loop receives.
enum Msg {
    Pty(pty::Event),
    Input(input::Event),
}

impl From<pty::Event> for Msg {
    fn from(event: pty::Event) -> Self {
        Self::Pty(event)
    }
}

impl From<input::Event> for Msg {
    fn from(event: input::Event) -> Self {
        Self::Input(event)
    }
}

impl Pane {
    /// The pane after this one, wrapping around.
    fn next(self) -> Self {
        match self {
            Self::Projects => Self::Agent,
            Self::Agent => Self::Changes,
            Self::Changes => Self::Projects,
        }
    }

    /// The pane before this one, wrapping around.
    fn previous(self) -> Self {
        match self {
            Self::Projects => Self::Changes,
            Self::Agent => Self::Projects,
            Self::Changes => Self::Agent,
        }
    }

    /// The pane selected by `Ctrl+Q` plus a digit.
    fn numbered(digit: char) -> Option<Self> {
        match digit {
            '1' => Some(Self::Projects),
            '2' => Some(Self::Agent),
            '3' => Some(Self::Changes),
            _ => None,
        }
    }
}

/// What a host event asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Send(Vec<u8>),
    Resize(Size),
    Focus(Pane),
    Nothing,
}

/// The prefix key state: when it was pressed, if it is pending.
#[derive(Debug, Default)]
struct Prefix {
    since: Option<Instant>,
}

impl Prefix {
    fn pending(&self) -> bool {
        self.since.is_some()
    }

    /// Drops a prefix older than the timeout. Returns whether it did.
    fn expire(&mut self, now: Instant) -> bool {
        match self.since {
            Some(since) if now.duration_since(since) >= PREFIX_TIMEOUT => {
                self.since = None;
                true
            }
            _ => false,
        }
    }
}

/// Options for [`run`].
pub struct Options<'a> {
    /// Directory the agent runs in.
    pub cwd: &'a Path,
    /// Arguments passed to the agent after `--`.
    pub args: &'a [String],
    /// Draw the agent over the whole terminal, with no side panes.
    pub full_screen: bool,
    /// Debug aid: panic from a worker thread after this many seconds.
    pub panic_after: Option<u64>,
}

/// Runs the agent, in the center pane or over the whole terminal. `Ctrl+Q`
/// then `q` quits; `Ctrl+Q` twice sends a literal `Ctrl+Q`. Returns the
/// child's exit status when it ended on its own.
pub fn run(options: Options<'_>) -> Result<Option<ExitStatus>> {
    let agent = agent::default_agent();
    let project = project_name(options.cwd);
    let mut host = Host::enter()?;
    let full_screen = options.full_screen;
    let size = agent_size(terminal_size(&host)?, full_screen);

    let (tx, rx) = mpsc::channel::<Msg>();
    let spec = agent.spec(options.cwd, options.args, size);
    let session =
        Session::spawn(&spec, tx.clone()).with_context(|| format!("start {}", agent.name()))?;
    Host::watch_child(session.pid(), session.exited_flag());
    let reader = Reader::spawn(tx)?;
    if let Some(seconds) = options.panic_after {
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            panic!("term --panic-after {seconds}: deliberate panic");
        });
    }

    let mut emu = Emulator::new(size, SCROLLBACK);
    let mut prefix = Prefix::default();
    let mut focus = Pane::default();
    let mut side_panes = side_panes_shown(terminal_size(&host)?, full_screen);
    let mut closed = false;
    let mut exited: Option<(ExitStatus, Instant)> = None;
    let mut dirty = true;
    let mut outcome: Result<bool> = Ok(false);

    loop {
        let now = Instant::now();
        if prefix.expire(now) {
            dirty = true;
        }
        let first = match rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let rest = std::iter::from_fn(|| rx.try_recv().ok()).take(DRAIN);
        for msg in first.into_iter().chain(rest) {
            dirty = true;
            match msg {
                Msg::Pty(pty::Event::Output(bytes)) => {
                    emu.process(&bytes);
                    session.write(&emu.take_replies());
                }
                Msg::Pty(pty::Event::Closed) => closed = true,
                Msg::Pty(pty::Event::Exited(status)) => {
                    Host::forget_child();
                    exited = Some((status, Instant::now()));
                }
                Msg::Input(input::Event::Failed(error)) => {
                    outcome = Err(error).context("read host input");
                }
                Msg::Input(input::Event::Host(event)) => {
                    let context = KeyContext {
                        focus,
                        side_panes,
                        modes: emu.modes(),
                        focus_events: emu.focus_events(),
                    };
                    match translate(event, &mut prefix, context) {
                        Action::Quit => outcome = Ok(true),
                        Action::Send(bytes) => session.write(&bytes),
                        Action::Resize(terminal) => {
                            let size = agent_size(terminal, full_screen);
                            emu.resize(size);
                            session.resize(size)?;
                            side_panes = side_panes_shown(terminal, full_screen);
                            if !side_panes {
                                focus = Pane::Agent;
                            }
                        }
                        Action::Focus(pane) => focus = pane,
                        Action::Nothing => {}
                    }
                }
            }
        }
        let ended = match exited {
            Some((_, at)) => closed || at.elapsed() >= CLOSE_GRACE,
            None => false,
        };
        if ended || !matches!(outcome, Ok(false)) {
            break;
        }
        if dirty {
            let view = ui::View {
                agent: agent.name(),
                project: &project,
                emulator: &emu,
                focus,
                side_panes,
                prefix_pending: prefix.pending(),
            };
            draw(&mut host, full_screen, &view)?;
            dirty = false;
        }
    }

    drop(reader);
    if !(closed && exited.is_some()) {
        shutdown(&session, &rx, &mut closed);
    }
    Host::forget_child();
    drop(host);
    outcome.map(|_| exited.map(|(status, _)| status))
}

/// What [`translate`] needs to know besides the event.
#[derive(Clone, Copy, Debug)]
struct KeyContext {
    /// The pane that receives keys.
    focus: Pane,
    /// Whether the side panes are on screen; when they are not, the agent
    /// keeps the focus.
    side_panes: bool,
    /// Modes the child set, for the key encoder.
    modes: Modes,
    /// Whether the child asked for focus events.
    focus_events: bool,
}

/// Turns one host event into an action, keeping the prefix state.
///
/// A pending prefix is consumed by the next key from any pane: `1`, `2`
/// and `3` focus a pane while the side panes are shown, `q` quits, `Ctrl+Q`
/// is sent literally when the agent is focused and dropped otherwise,
/// anything else is dropped. A paste clears it. With the agent focused,
/// keys and pastes go to it. With a side pane focused, `Esc` returns to the
/// agent, `Tab` and `Shift+Tab` move to the next and previous pane, and
/// other keys and pastes do nothing yet. Host focus events describe the
/// window, so they reach the agent whichever pane is focused. Key releases
/// and repeats are ignored.
fn translate(event: HostEvent, prefix: &mut Prefix, context: KeyContext) -> Action {
    let KeyContext {
        focus,
        side_panes,
        modes,
        focus_events,
    } = context;
    let mut bytes = Vec::new();
    match event {
        HostEvent::Key(key) if key.kind != KeyEventKind::Press => {}
        HostEvent::Key(key) if prefix.pending() => {
            prefix.since = None;
            match key.code {
                KeyCode::Char('q') if key.modifiers.is_empty() => return Action::Quit,
                KeyCode::Char('q') if key.modifiers == KeyModifiers::CONTROL => {
                    if focus == Pane::Agent {
                        let _ = keys::encode(&key, modes, &mut bytes);
                    }
                }
                KeyCode::Char(digit) if key.modifiers.is_empty() && side_panes => {
                    if let Some(pane) = Pane::numbered(digit) {
                        return Action::Focus(pane);
                    }
                }
                _ => {}
            }
        }
        HostEvent::Key(key) if is_prefix(&key) => prefix.since = Some(Instant::now()),
        HostEvent::Key(key) if focus == Pane::Agent => {
            let _ = keys::encode(&key, modes, &mut bytes);
        }
        HostEvent::Key(key) => match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => return Action::Focus(Pane::Agent),
            KeyCode::Tab if key.modifiers.is_empty() => return Action::Focus(focus.next()),
            KeyCode::BackTab => return Action::Focus(focus.previous()),
            _ => {}
        },
        HostEvent::Paste(text) => {
            prefix.since = None;
            if focus == Pane::Agent {
                keys::encode_paste(&text, modes, &mut bytes);
            }
        }
        HostEvent::Resize(cols, rows) => return Action::Resize(Size { rows, cols }),
        HostEvent::FocusGained if focus_events => bytes.extend_from_slice(b"\x1b[I"),
        HostEvent::FocusLost if focus_events => bytes.extend_from_slice(b"\x1b[O"),
        HostEvent::FocusGained | HostEvent::FocusLost | HostEvent::Mouse(_) => {}
    }
    if bytes.is_empty() {
        Action::Nothing
    } else {
        Action::Send(bytes)
    }
}

/// Whether the side panes are on screen for a terminal of `terminal`
/// cells: never in full-screen mode, and only when wide enough otherwise.
fn side_panes_shown(terminal: Size, full_screen: bool) -> bool {
    let area = ratatui::layout::Rect::new(0, 0, terminal.cols, terminal.rows);
    !full_screen && ui::layout::sides_visible(area)
}

/// `Ctrl+Q`, the one key the app keeps.
fn is_prefix(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::CONTROL
}

fn terminal_size(host: &Host) -> Result<Size> {
    let size = host.terminal.size().context("read terminal size")?;
    Ok(Size {
        rows: size.height,
        cols: size.width,
    })
}

/// The agent's screen size for a terminal of `terminal` cells, at least
/// one cell each way so the pty and the emulator always agree.
fn agent_size(terminal: Size, full_screen: bool) -> Size {
    let area = ratatui::layout::Rect::new(0, 0, terminal.cols, terminal.rows);
    let size: Size = ui::agent_area(area, full_screen).into();
    Size {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
    }
}

/// Last path component, or the path itself when it has none.
fn project_name(cwd: &Path) -> String {
    cwd.file_name().map_or_else(
        || cwd.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn draw(host: &mut Host, full_screen: bool, view: &ui::View<'_>) -> Result<()> {
    host.terminal
        .draw(|frame| {
            if full_screen {
                ui::render_full(frame, view.emulator);
            } else {
                ui::render(frame, view);
            }
        })
        .context("draw")?;
    Ok(())
}

/// Hangs the child up, waits for it to exit, kills it if it does not, and
/// kills its process group if something in it still holds the terminal.
fn shutdown(session: &Session, rx: &Receiver<Msg>, closed: &mut bool) {
    let _ = session.hangup();
    wait_until(rx, closed, session, GRACE, |session, _| {
        session.has_exited()
    });
    if !session.has_exited() {
        let _ = session.kill_now();
        wait_until(rx, closed, session, KILL_GRACE, |session, _| {
            session.has_exited()
        });
    }
    if !*closed {
        let _ = session.kill_group_now();
        wait_until(rx, closed, session, KILL_GRACE, |_, closed| closed);
    }
}

/// Drains the channel until `done` holds or `limit` passes, noting a
/// `Closed` event on the way.
fn wait_until(
    rx: &Receiver<Msg>,
    closed: &mut bool,
    session: &Session,
    limit: Duration,
    done: impl Fn(&Session, bool) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !done(session, *closed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(Msg::Pty(pty::Event::Closed)) => *closed = true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{KittyFlags, Protocol};

    const CTRL_Q: KeyEvent = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    const Q: KeyEvent = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);

    fn key(key: KeyEvent) -> HostEvent {
        HostEvent::Key(key)
    }

    /// A context with the agent focused.
    fn agent(modes: Modes, focus_events: bool) -> KeyContext {
        KeyContext {
            focus: Pane::Agent,
            side_panes: true,
            modes,
            focus_events,
        }
    }

    /// A context with `pane` focused and default modes.
    fn focused(pane: Pane) -> KeyContext {
        KeyContext {
            focus: pane,
            side_panes: true,
            modes: Modes::default(),
            focus_events: false,
        }
    }

    fn plain(code: KeyCode) -> HostEvent {
        HostEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn ctrl_q_then_q_quits() {
        let mut prefix = Prefix::default();
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false)),
            Action::Nothing
        );
        assert!(prefix.pending());
        assert_eq!(
            translate(key(Q), &mut prefix, agent(Modes::default(), false)),
            Action::Quit
        );
        assert!(!prefix.pending());
    }

    #[test]
    fn ctrl_q_twice_sends_a_literal() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false));
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false)),
            Action::Send(vec![0x11])
        );
        let kitty = Modes {
            protocol: Protocol::Kitty(KittyFlags::DISAMBIGUATE),
            ..Modes::default()
        };
        let _ = translate(key(CTRL_Q), &mut prefix, agent(kitty, false));
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, agent(kitty, false)),
            Action::Send(b"\x1b[113;5u".to_vec())
        );
    }

    #[test]
    fn ctrl_q_then_another_key_is_dropped() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false));
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            translate(key(esc), &mut prefix, agent(Modes::default(), false)),
            Action::Nothing
        );
        assert!(!prefix.pending());
        assert_eq!(
            translate(key(esc), &mut prefix, agent(Modes::default(), false)),
            Action::Send(vec![0x1b])
        );
    }

    #[test]
    fn paste_clears_the_prefix_and_is_sent() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false));
        let modes = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(
            translate(
                HostEvent::Paste("a\nb".into()),
                &mut prefix,
                agent(modes, false)
            ),
            Action::Send(b"\x1b[200~a\rb\x1b[201~".to_vec())
        );
        assert!(!prefix.pending());
    }

    #[test]
    fn prefix_expires() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, agent(Modes::default(), false));
        assert!(!prefix.expire(Instant::now()));
        assert!(prefix.expire(Instant::now() + PREFIX_TIMEOUT));
        assert!(!prefix.pending());
    }

    #[test]
    fn focus_events_only_when_requested() {
        let mut prefix = Prefix::default();
        assert_eq!(
            translate(
                HostEvent::FocusGained,
                &mut prefix,
                agent(Modes::default(), false)
            ),
            Action::Nothing
        );
        assert_eq!(
            translate(
                HostEvent::FocusGained,
                &mut prefix,
                agent(Modes::default(), true)
            ),
            Action::Send(b"\x1b[I".to_vec())
        );
        assert_eq!(
            translate(
                HostEvent::FocusLost,
                &mut prefix,
                agent(Modes::default(), true)
            ),
            Action::Send(b"\x1b[O".to_vec())
        );
    }

    #[test]
    fn resize_maps_columns_and_rows() {
        let mut prefix = Prefix::default();
        assert_eq!(
            translate(
                HostEvent::Resize(120, 40),
                &mut prefix,
                agent(Modes::default(), false)
            ),
            Action::Resize(Size {
                rows: 40,
                cols: 120
            })
        );
    }

    #[test]
    fn keys_are_encoded_with_the_modes() {
        let mut prefix = Prefix::default();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(enter), &mut prefix, agent(Modes::default(), false)),
            Action::Send(b"\r".to_vec())
        );
        let kitty = Modes {
            protocol: Protocol::Kitty(KittyFlags(5)),
            ..Modes::default()
        };
        assert_eq!(
            translate(key(enter), &mut prefix, agent(kitty, false)),
            Action::Send(b"\x1b[13;2u".to_vec())
        );
    }

    #[test]
    fn agent_size_follows_the_view() {
        let terminal = Size {
            rows: 40,
            cols: 100,
        };
        assert_eq!(agent_size(terminal, true), terminal);
        assert_eq!(agent_size(terminal, false), Size { rows: 37, cols: 53 });
        assert_eq!(
            agent_size(Size { rows: 2, cols: 2 }, false),
            Size { rows: 1, cols: 1 }
        );
        assert_eq!(
            agent_size(Size { rows: 0, cols: 0 }, true),
            Size { rows: 1, cols: 1 }
        );
    }

    #[test]
    fn project_name_is_the_last_component() {
        assert_eq!(project_name(Path::new("/Users/x/dev/term")), "term");
        assert_eq!(project_name(Path::new("/Users/x/dev/term/")), "term");
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[test]
    fn prefix_and_digit_focus_a_pane() {
        let mut prefix = Prefix::default();
        for (digit, pane) in [
            ('1', Pane::Projects),
            ('2', Pane::Agent),
            ('3', Pane::Changes),
        ] {
            let _ = translate(key(CTRL_Q), &mut prefix, focused(Pane::Agent));
            assert_eq!(
                translate(
                    plain(KeyCode::Char(digit)),
                    &mut prefix,
                    focused(Pane::Agent)
                ),
                Action::Focus(pane)
            );
        }
        // From a side pane too, and an unknown digit is dropped.
        let _ = translate(key(CTRL_Q), &mut prefix, focused(Pane::Changes));
        assert_eq!(
            translate(
                plain(KeyCode::Char('1')),
                &mut prefix,
                focused(Pane::Changes)
            ),
            Action::Focus(Pane::Projects)
        );
        let _ = translate(key(CTRL_Q), &mut prefix, focused(Pane::Agent));
        assert_eq!(
            translate(plain(KeyCode::Char('4')), &mut prefix, focused(Pane::Agent)),
            Action::Nothing
        );
    }

    #[test]
    fn side_panes_keep_keys_and_pastes_from_the_agent() {
        let mut prefix = Prefix::default();
        let ctx = focused(Pane::Projects);
        assert_eq!(
            translate(plain(KeyCode::Char('x')), &mut prefix, ctx),
            Action::Nothing
        );
        assert_eq!(
            translate(HostEvent::Paste("secret".into()), &mut prefix, ctx),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), &mut prefix, ctx),
            Action::Focus(Pane::Agent)
        );
        assert_eq!(
            translate(plain(KeyCode::Tab), &mut prefix, ctx),
            Action::Focus(Pane::Agent)
        );
        assert_eq!(
            translate(plain(KeyCode::BackTab), &mut prefix, ctx),
            Action::Focus(Pane::Changes)
        );
        assert_eq!(
            translate(plain(KeyCode::Tab), &mut prefix, focused(Pane::Changes)),
            Action::Focus(Pane::Projects)
        );
        // A literal Ctrl+Q only makes sense for the agent.
        let _ = translate(key(CTRL_Q), &mut prefix, ctx);
        assert_eq!(translate(key(CTRL_Q), &mut prefix, ctx), Action::Nothing);
        // Resizes and quitting work from anywhere.
        assert_eq!(
            translate(HostEvent::Resize(80, 24), &mut prefix, ctx),
            Action::Resize(Size { rows: 24, cols: 80 })
        );
        let _ = translate(key(CTRL_Q), &mut prefix, ctx);
        assert_eq!(translate(key(Q), &mut prefix, ctx), Action::Quit);
    }

    #[test]
    fn panes_cycle_both_ways() {
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            assert_eq!(pane.next().previous(), pane);
            assert_ne!(pane.next(), pane);
        }
        assert_eq!(Pane::Projects.next(), Pane::Agent);
        assert_eq!(Pane::Changes.next(), Pane::Projects);
        assert_eq!(Pane::numbered('1'), Some(Pane::Projects));
        assert_eq!(Pane::numbered('2'), Some(Pane::Agent));
        assert_eq!(Pane::numbered('3'), Some(Pane::Changes));
        assert_eq!(Pane::numbered('4'), None);
    }

    #[test]
    fn agent_pane_passes_tab_esc_and_digits_through() {
        let mut prefix = Prefix::default();
        let ctx = agent(Modes::default(), false);
        assert_eq!(
            translate(plain(KeyCode::Tab), &mut prefix, ctx),
            Action::Send(b"\t".to_vec())
        );
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(back_tab), &mut prefix, ctx),
            Action::Send(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), &mut prefix, ctx),
            Action::Send(vec![0x1b])
        );
        assert_eq!(
            translate(plain(KeyCode::Char('1')), &mut prefix, ctx),
            Action::Send(b"1".to_vec())
        );
    }

    #[test]
    fn without_side_panes_the_agent_keeps_the_focus() {
        let mut prefix = Prefix::default();
        let ctx = KeyContext {
            side_panes: false,
            ..agent(Modes::default(), false)
        };
        let _ = translate(key(CTRL_Q), &mut prefix, ctx);
        assert_eq!(
            translate(plain(KeyCode::Char('1')), &mut prefix, ctx),
            Action::Nothing
        );
        assert!(!prefix.pending());
        let _ = translate(key(CTRL_Q), &mut prefix, ctx);
        assert_eq!(translate(key(Q), &mut prefix, ctx), Action::Quit);
    }

    #[test]
    fn prefix_is_consumed_by_a_focus_change_and_esc_is_not_a_return() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, focused(Pane::Agent));
        assert_eq!(
            translate(plain(KeyCode::Char('3')), &mut prefix, focused(Pane::Agent)),
            Action::Focus(Pane::Changes)
        );
        assert!(!prefix.pending());
        let side = focused(Pane::Changes);
        let _ = translate(key(CTRL_Q), &mut prefix, side);
        assert_eq!(
            translate(plain(KeyCode::Esc), &mut prefix, side),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), &mut prefix, side),
            Action::Focus(Pane::Agent)
        );
        let alt_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT);
        assert_eq!(translate(key(alt_esc), &mut prefix, side), Action::Nothing);
        assert_eq!(
            translate(plain(KeyCode::Char('2')), &mut prefix, side),
            Action::Nothing
        );
    }

    #[test]
    fn host_focus_events_reach_the_agent_from_any_pane() {
        let mut prefix = Prefix::default();
        let ctx = KeyContext {
            focus_events: true,
            ..focused(Pane::Projects)
        };
        assert_eq!(
            translate(HostEvent::FocusLost, &mut prefix, ctx),
            Action::Send(b"\x1b[O".to_vec())
        );
    }

    #[test]
    fn key_releases_and_repeats_are_ignored() {
        let mut prefix = Prefix::default();
        let ctx = agent(Modes::default(), false);
        let _ = translate(key(CTRL_Q), &mut prefix, ctx);
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        );
        assert_eq!(translate(key(release), &mut prefix, ctx), Action::Nothing);
        assert!(prefix.pending());
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(translate(key(repeat), &mut prefix, ctx), Action::Nothing);
    }

    #[test]
    fn messages_convert_from_both_event_kinds() {
        assert!(matches!(
            Msg::from(pty::Event::Closed),
            Msg::Pty(pty::Event::Closed)
        ));
        let failed = input::Event::Failed(std::io::Error::other("x"));
        assert!(matches!(
            Msg::from(failed),
            Msg::Input(input::Event::Failed(_))
        ));
    }
}
