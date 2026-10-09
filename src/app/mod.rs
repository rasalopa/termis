//! The application: wires the agent's pty, the emulator, the host input
//! and the key encoder together.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event as HostEvent, KeyCode, KeyEvent, KeyModifiers};
use tui_term::widget::Cursor;

use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::input::{self, Reader};
use crate::keys::{self, Modes};
use crate::pty::{self, ExitStatus, Session};

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

/// What a host event asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Send(Vec<u8>),
    Resize(Size),
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

/// Options for [`spike`].
pub struct SpikeOptions<'a> {
    /// Directory the agent runs in.
    pub cwd: &'a Path,
    /// Arguments passed to the agent after `--`.
    pub args: &'a [String],
    /// Debug aid: panic from a worker thread after this many seconds.
    pub panic_after: Option<u64>,
}

/// Runs the agent over the whole terminal, with no side panes. `Ctrl+Q`
/// then `q` quits; `Ctrl+Q` twice sends a literal `Ctrl+Q`. Returns the
/// child's exit status when it ended on its own.
pub fn spike(options: SpikeOptions<'_>) -> Result<Option<ExitStatus>> {
    let agent = agent::default_agent();
    let mut host = Host::enter()?;
    let size = terminal_size(&host)?;

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
                    match translate(event, &mut prefix, emu.modes(), emu.focus_events()) {
                        Action::Quit => outcome = Ok(true),
                        Action::Send(bytes) => session.write(&bytes),
                        Action::Resize(size) => {
                            emu.resize(size);
                            session.resize(size)?;
                        }
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
            draw(&mut host, &emu)?;
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

/// Turns one host event into an action, keeping the prefix state. A
/// pending prefix is consumed by the next key: `q` quits, `Ctrl+Q` is sent
/// literally, anything else is dropped. A paste clears it.
fn translate(event: HostEvent, prefix: &mut Prefix, modes: Modes, focus_events: bool) -> Action {
    let mut bytes = Vec::new();
    match event {
        HostEvent::Key(key) if prefix.pending() => {
            prefix.since = None;
            match key.code {
                KeyCode::Char('q') if key.modifiers.is_empty() => return Action::Quit,
                KeyCode::Char('q') if key.modifiers == KeyModifiers::CONTROL => {
                    let _ = keys::encode(&key, modes, &mut bytes);
                }
                _ => {}
            }
        }
        HostEvent::Key(key) if is_prefix(&key) => prefix.since = Some(Instant::now()),
        HostEvent::Key(key) => {
            let _ = keys::encode(&key, modes, &mut bytes);
        }
        HostEvent::Paste(text) => {
            prefix.since = None;
            keys::encode_paste(&text, modes, &mut bytes);
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

fn draw(host: &mut Host, emu: &Emulator) -> Result<()> {
    host.terminal
        .draw(|frame| {
            let widget = emu.widget().cursor(Cursor::default().visibility(false));
            frame.render_widget(widget, frame.area());
            if let Some((row, col)) = emu.cursor() {
                frame.set_cursor_position((col, row));
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

    #[test]
    fn ctrl_q_then_q_quits() {
        let mut prefix = Prefix::default();
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, Modes::default(), false),
            Action::Nothing
        );
        assert!(prefix.pending());
        assert_eq!(
            translate(key(Q), &mut prefix, Modes::default(), false),
            Action::Quit
        );
        assert!(!prefix.pending());
    }

    #[test]
    fn ctrl_q_twice_sends_a_literal() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, Modes::default(), false);
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, Modes::default(), false),
            Action::Send(vec![0x11])
        );
        let kitty = Modes {
            protocol: Protocol::Kitty(KittyFlags::DISAMBIGUATE),
            ..Modes::default()
        };
        let _ = translate(key(CTRL_Q), &mut prefix, kitty, false);
        assert_eq!(
            translate(key(CTRL_Q), &mut prefix, kitty, false),
            Action::Send(b"\x1b[113;5u".to_vec())
        );
    }

    #[test]
    fn ctrl_q_then_another_key_is_dropped() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, Modes::default(), false);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            translate(key(esc), &mut prefix, Modes::default(), false),
            Action::Nothing
        );
        assert!(!prefix.pending());
        assert_eq!(
            translate(key(esc), &mut prefix, Modes::default(), false),
            Action::Send(vec![0x1b])
        );
    }

    #[test]
    fn paste_clears_the_prefix_and_is_sent() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, Modes::default(), false);
        let modes = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(
            translate(HostEvent::Paste("a\nb".into()), &mut prefix, modes, false),
            Action::Send(b"\x1b[200~a\rb\x1b[201~".to_vec())
        );
        assert!(!prefix.pending());
    }

    #[test]
    fn prefix_expires() {
        let mut prefix = Prefix::default();
        let _ = translate(key(CTRL_Q), &mut prefix, Modes::default(), false);
        assert!(!prefix.expire(Instant::now()));
        assert!(prefix.expire(Instant::now() + PREFIX_TIMEOUT));
        assert!(!prefix.pending());
    }

    #[test]
    fn focus_events_only_when_requested() {
        let mut prefix = Prefix::default();
        assert_eq!(
            translate(HostEvent::FocusGained, &mut prefix, Modes::default(), false),
            Action::Nothing
        );
        assert_eq!(
            translate(HostEvent::FocusGained, &mut prefix, Modes::default(), true),
            Action::Send(b"\x1b[I".to_vec())
        );
        assert_eq!(
            translate(HostEvent::FocusLost, &mut prefix, Modes::default(), true),
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
                Modes::default(),
                false
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
            translate(key(enter), &mut prefix, Modes::default(), false),
            Action::Send(b"\r".to_vec())
        );
        let kitty = Modes {
            protocol: Protocol::Kitty(KittyFlags(5)),
            ..Modes::default()
        };
        assert_eq!(
            translate(key(enter), &mut prefix, kitty, false),
            Action::Send(b"\x1b[13;2u".to_vec())
        );
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
