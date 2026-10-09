//! The application: wires the agent's pty, the emulator, the host input
//! and the key encoder together.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::git::status::Change;
use crate::git::{self, worker::Job, worker::Snapshot, worker::Worker, worker::Write};
use crate::input::{self, Reader};
use crate::keys::{self, Modes};
use crate::pty::{self, ExitStatus, Session};
use crate::strings;
use crate::ui::changes::{self, Section, Selected, Step};
use crate::ui::commit::{Draft, Edit};
use crate::ui::toast::{self, Toast};
use crate::ui::{self, Mode, Pane};

pub mod host;

use host::Host;

/// Lines of output kept above the screen.
const SCROLLBACK: usize = 5000;
/// Longest wait between two passes of the main loop.
const TICK: Duration = Duration::from_millis(16);
/// Messages handled per pass before drawing, so a chatty child cannot
/// starve the screen.
const DRAIN: usize = 256;
/// How often the changes pane is refreshed while it is on screen.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long a notice stays in the status bar.
const TOAST_LASTS: Duration = Duration::from_secs(5);
/// After the child exited, how long to wait for its output to end.
const CLOSE_GRACE: Duration = Duration::from_millis(200);
/// How long a hung-up child gets before it is killed.
const GRACE: Duration = Duration::from_secs(2);
/// How long a killed child gets to be reaped.
const KILL_GRACE: Duration = Duration::from_millis(500);

/// Everything the main loop receives. Pty events carry the generation of
/// the session that produced them, so a relaunch can ignore stragglers.
enum Msg {
    Pty(u32, pty::Event),
    Input(input::Event),
    Git(git::worker::Event),
}

impl From<input::Event> for Msg {
    fn from(event: input::Event) -> Self {
        Self::Input(event)
    }
}

impl From<git::worker::Event> for Msg {
    fn from(event: git::worker::Event) -> Self {
        Self::Git(event)
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
}

impl Mode {
    /// The mode once the side panes are gone: the highlight falls back to
    /// the agent's pane, the mode stays.
    fn without_side_panes(self) -> Self {
        match self {
            Self::Agent => Self::Agent,
            Self::Term(_) | Self::Commit => Self::Term(Pane::Agent),
        }
    }
}

/// What the main loop knows about the current agent session.
#[derive(Debug, Default)]
struct Lifecycle {
    /// Counts sessions; pty events from older ones are ignored.
    generation: u32,
    /// The session's reader saw the end of its output.
    closed: bool,
    /// The session's exit status and when it arrived.
    exited: Option<(ExitStatus, Instant)>,
    /// The session is over for the user: exited, and its output ended or
    /// the grace passed.
    ended: Option<ExitStatus>,
}

impl Lifecycle {
    /// Notes a `Closed` or `Exited` event; events from an older session
    /// are dropped. Output is the caller's to apply.
    fn apply(&mut self, from: u32, event: &pty::Event, now: Instant) {
        if from != self.generation {
            return;
        }
        match event {
            pty::Event::Output(_) => {}
            pty::Event::Closed => self.closed = true,
            pty::Event::Exited(status) => self.exited = Some((status.clone(), now)),
        }
    }

    /// Whether `from` is the current session.
    fn current(&self, from: u32) -> bool {
        from == self.generation
    }

    /// Marks the session ended once it exited and its output ended or
    /// `CLOSE_GRACE` passed. Returns whether it just did.
    fn settle(&mut self, now: Instant) -> bool {
        let Some((status, at)) = &self.exited else {
            return false;
        };
        if self.ended.is_some() || !(self.closed || now.duration_since(*at) >= CLOSE_GRACE) {
            return false;
        }
        self.ended = Some(status.clone());
        true
    }

    /// Starts counting a new session.
    fn relaunch(&mut self) {
        self.generation += 1;
        self.closed = false;
        self.exited = None;
        self.ended = None;
    }

    fn alive(&self) -> bool {
        self.ended.is_none()
    }

    /// The exit status of the last session, when it ended on its own.
    fn status(&self) -> Option<ExitStatus> {
        self.exited.as_ref().map(|(status, _)| status.clone())
    }
}

/// The layout state the main loop keeps, and what follows from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    terminal: Size,
    full_screen: bool,
    zoomed: bool,
}

impl Layout {
    fn area(self) -> ratatui::layout::Rect {
        ratatui::layout::Rect::new(0, 0, self.terminal.cols, self.terminal.rows)
    }

    /// Whether the terminal is wide enough for the side panes.
    fn wide(self) -> bool {
        ui::layout::sides_visible(self.area())
    }

    /// Whether the side panes are on screen: never in full-screen mode or
    /// zoomed, and only when wide enough otherwise.
    fn side_panes(self) -> bool {
        !self.full_screen && !self.zoomed && self.wide()
    }

    /// The agent's screen size, at least one cell each way so the pty and
    /// the emulator always agree.
    fn agent_size(self) -> Size {
        let size: Size = ui::agent_area(self.area(), self.full_screen, self.side_panes()).into();
        Size {
            rows: size.rows.max(1),
            cols: size.cols.max(1),
        }
    }

    /// Whether `z` changes something on screen: side panes to hide,
    /// or hidden ones the width allows back.
    fn zoomable(self) -> bool {
        !self.full_screen && (self.side_panes() || (self.zoomed && self.wide()))
    }
}

/// Resizes the agent to `layout` when its size changed, and drops the
/// highlight to the agent's pane when the side panes are gone.
fn apply_layout(
    layout: Layout,
    emu: &mut Emulator,
    session: &Session,
    side_panes: &mut bool,
    mode: &mut Mode,
) -> Result<()> {
    let size = layout.agent_size();
    if size != emu.size() {
        emu.resize(size);
        session.resize(size)?;
    }
    *side_panes = layout.side_panes();
    if !*side_panes {
        *mode = mode.without_side_panes();
    }
    Ok(())
}

/// What a host event asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    Send(Vec<u8>),
    Resize(Size),
    Mode(Mode),
    Zoom,
    Relaunch,
    /// Move the selection of the changes pane.
    Select(Step),
    /// Stage the selected row.
    Stage,
    /// Take the selected row out of the index.
    Unstage,
    /// Stage everything.
    StageAll,
    /// Open the commit box.
    OpenCommit,
    /// Change the commit draft.
    Edit(Edit),
    /// Commit the draft.
    Commit,
    Nothing,
}

/// When to ask the worker for a refresh: every `every` while the side
/// panes are shown, and never while one is in flight. The interval runs
/// from the end of the last refresh, so a slow repository leaves gaps.
#[derive(Debug)]
struct Poll {
    every: Duration,
    last: Option<Instant>,
    in_flight: bool,
}

impl Poll {
    /// A schedule with nothing sent yet: the first refresh is due at once.
    fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            in_flight: false,
        }
    }

    /// Whether a refresh should be sent now.
    fn due(&self, now: Instant, wanted: bool) -> bool {
        wanted
            && !self.in_flight
            && self
                .last
                .is_none_or(|last| now.duration_since(last) >= self.every)
    }

    /// Records that a refresh was sent at `now`.
    fn sent(&mut self, now: Instant) {
        self.last = Some(now);
        self.in_flight = true;
    }

    /// Records that the refresh in flight finished at `now`.
    fn done(&mut self, now: Instant) {
        self.last = Some(now);
        self.in_flight = false;
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
/// enters termis mode, where `q` quits. Once the agent ended, `r` relaunches
/// it and `q` quits. Returns the exit status of the last session when it
/// ended on its own and the user then quit.
pub fn run(options: Options<'_>) -> Result<Option<ExitStatus>> {
    let agent = agent::default_agent();
    // Outside a repository the agent still runs; the changes pane says so.
    // Without git at all, termis does not start: the right pane needs it.
    // Checked once; the git worker refreshes it from M3 on.
    let repo = git::repo_root(options.cwd)?;
    // The project is the repository, not the subdirectory the agent runs in.
    let project = project_name(repo.as_deref().unwrap_or(options.cwd));
    let mut host = Host::enter()?;
    let mut layout = Layout {
        terminal: terminal_size(&host)?,
        full_screen: options.full_screen,
        zoomed: false,
    };
    let size = layout.agent_size();

    let (tx, rx) = mpsc::channel::<Msg>();
    let mut life = Lifecycle::default();
    let mut session = start_agent(agent.as_ref(), &options, size, &tx, life.generation)?;
    let reader = Reader::spawn(tx.clone())?;
    let worker = match &repo {
        Some(root) => Some(Worker::spawn(root.clone(), tx.clone())?),
        None => None,
    };
    let mut poll = Poll::new(POLL_EVERY);
    let mut snapshot: Option<Snapshot> = None;
    let mut selected: Option<Selected> = None;
    let mut scroll = 0;
    let mut draft = Draft::default();
    let mut in_flight: Option<Draft> = None;
    let mut toast: Option<Toast> = None;
    let mut refresh_error: Option<String> = None;
    if let Some(seconds) = options.panic_after {
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            panic!("termis --panic-after {seconds}: deliberate panic");
        });
    }

    let mut emu = Emulator::new(size, SCROLLBACK);
    let mut mode = Mode::default();
    let mut side_panes = layout.side_panes();
    let mut dirty = true;
    let mut outcome: Result<bool> = Ok(false);

    loop {
        let now = Instant::now();
        if let Some(worker) = &worker
            && poll.due(now, side_panes)
        {
            worker.send(Job::Refresh);
            poll.sent(now);
        }
        if toast.as_ref().is_some_and(|toast| !toast.live(now)) {
            toast = None;
            dirty = true;
        }
        let first = match rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let rest = std::iter::from_fn(|| rx.try_recv().ok()).take(DRAIN);
        for msg in first.into_iter().chain(rest) {
            // A refresh dirties the screen only when something changed.
            dirty |= !matches!(msg, Msg::Git(_));
            if let Msg::Git(event) = &msg
                && let Some((text, kind)) = notice(event, &mut refresh_error)
            {
                toast = Some(Toast::new(text, kind, Instant::now(), TOAST_LASTS));
                dirty = true;
            }
            match msg {
                // The draft served; a failed commit brings it back for
                // another go, unless a new one was started meanwhile.
                Msg::Git(git::worker::Event::Committed(_)) => in_flight = None,
                Msg::Git(git::worker::Event::Failed {
                    write: Write::Commit(_),
                    ..
                }) => {
                    if let Some(previous) = in_flight.take()
                        && draft.is_empty()
                    {
                        draft = previous;
                    }
                }
                Msg::Git(git::worker::Event::Failed { .. }) => {}
                Msg::Git(git::worker::Event::Refreshed(result)) => {
                    poll.done(Instant::now());
                    // A failed refresh keeps the last snapshot; notice() reported it.
                    if let Ok(fresh) = result
                        && snapshot.as_ref() != Some(&fresh)
                    {
                        selected = changes::reselect(snapshot.as_ref(), &fresh, selected.as_ref());
                        snapshot = Some(fresh);
                        dirty |= side_panes;
                    }
                }
                Msg::Pty(from, pty::Event::Output(bytes)) if life.current(from) => {
                    emu.process(&bytes);
                    session.write(&emu.take_replies());
                }
                Msg::Pty(from, event) => {
                    if life.current(from) && matches!(event, pty::Event::Exited(_)) {
                        Host::forget_child();
                    }
                    life.apply(from, &event, Instant::now());
                }
                Msg::Input(input::Event::Failed(error)) => {
                    outcome = Err(error).context("read host input");
                }
                Msg::Input(input::Event::Host(event)) => {
                    let context = KeyContext {
                        mode,
                        side_panes,
                        zoomable: layout.zoomable(),
                        agent_alive: life.alive(),
                        modes: emu.modes(),
                        focus_events: emu.focus_events(),
                    };
                    let action = translate(event, context);
                    match action {
                        Action::Quit => outcome = Ok(true),
                        Action::Send(bytes) => session.write(&bytes),
                        Action::Resize(terminal) => {
                            layout.terminal = terminal;
                            apply_layout(layout, &mut emu, &session, &mut side_panes, &mut mode)?;
                        }
                        Action::Zoom => {
                            layout.zoomed = !layout.zoomed;
                            apply_layout(layout, &mut emu, &session, &mut side_panes, &mut mode)?;
                        }
                        Action::Mode(new_mode) => mode = new_mode,
                        Action::Select(step) => {
                            if let Some(current) = &snapshot {
                                selected = changes::moved(current, selected.as_ref(), step);
                            }
                        }
                        Action::Stage | Action::Unstage => {
                            let unstage = action == Action::Unstage;
                            if let Some(worker) = &worker
                                && let Some(current) = &snapshot
                                && let Some(paths) =
                                    selected_paths(current, selected.as_ref(), unstage)
                            {
                                worker.send(Job::Write(if unstage {
                                    Write::Unstage(paths)
                                } else {
                                    Write::Stage(paths)
                                }));
                            }
                        }
                        Action::StageAll => {
                            let something_to_stage = snapshot
                                .as_ref()
                                .is_some_and(|current| current.status.unstaged().next().is_some());
                            if let Some(worker) = &worker
                                && something_to_stage
                            {
                                worker.send(Job::Write(Write::StageAll));
                            }
                        }
                        Action::Relaunch => {
                            // The old group may still hold the pty; nothing
                            // else will reach it once the session is replaced.
                            if !life.closed {
                                let _ = session.kill_group_now();
                            }
                            life.relaunch();
                            session = start_agent(
                                agent.as_ref(),
                                &options,
                                layout.agent_size(),
                                &tx,
                                life.generation,
                            )?;
                            emu = Emulator::new(layout.agent_size(), SCROLLBACK);
                        }
                        Action::OpenCommit if side_panes => {
                            let staged = can_commit(snapshot.as_ref(), side_panes);
                            match open_refusal(worker.is_some(), staged, in_flight.is_some()) {
                                None => mode = Mode::Commit,
                                Some(text) => toast = Some(refusal(text)),
                            }
                        }
                        Action::OpenCommit => {}
                        Action::Edit(edit) => draft.apply(&edit),
                        Action::Commit => {
                            let message = draft.message();
                            let staged = can_commit(snapshot.as_ref(), side_panes);
                            match (send_refusal(staged, message.is_some()), &worker, message) {
                                (None, Some(worker), Some(message)) => {
                                    worker.send(Job::Write(Write::Commit(message)));
                                    in_flight = Some(std::mem::take(&mut draft));
                                    mode = Mode::Term(Pane::Changes);
                                }
                                (Some(text), _, _) => toast = Some(refusal(text)),
                                _ => {}
                            }
                        }
                        Action::Nothing => {}
                    }
                }
            }
        }
        if life.settle(Instant::now()) {
            dirty = true;
        }
        if !matches!(outcome, Ok(false)) {
            break;
        }
        if dirty {
            let ended_line = life.ended.as_ref().map(|status| {
                strings::agent_ended_line(agent.name(), status.exit_code(), status.signal())
            });
            let view = ui::View {
                agent: agent.name(),
                project: &project,
                emulator: &emu,
                mode,
                side_panes,
                zoomed: layout.zoomed,
                ended: ended_line.as_deref(),
                repo: repo.is_some(),
                changes: snapshot.as_ref(),
                selected: selected.as_ref(),
                scroll,
                draft: (mode == Mode::Commit).then_some(&draft),
                toast: toast.as_ref(),
            };
            scroll = draw(&mut host, layout.full_screen, &view)?;
            dirty = false;
        }
    }

    drop(reader);
    if !(life.closed && life.exited.is_some()) {
        shutdown(&session, &rx, &mut life);
    }
    Host::forget_child();
    drop(host);
    outcome.map(|_| life.status())
}

/// Starts the agent on a pty of `size` and forwards its events, tagged
/// with `generation`, to `tx` from a thread.
fn start_agent(
    agent: &dyn agent::Agent,
    options: &Options<'_>,
    size: Size,
    tx: &mpsc::Sender<Msg>,
    generation: u32,
) -> Result<Session> {
    let (events, received) = mpsc::channel::<pty::Event>();
    let spec = agent.spec(options.cwd, options.args, size);
    let session =
        Session::spawn(&spec, events).with_context(|| format!("start {}", agent.name()))?;
    let tx = tx.clone();
    thread::Builder::new()
        .name("pty-forward".into())
        .spawn(move || {
            for event in received {
                if tx.send(Msg::Pty(generation, event)).is_err() {
                    break;
                }
            }
        })
        .context("start pty forwarding thread")?;
    Host::watch_child(session.pid(), session.exited_flag());
    Ok(session)
}

/// What [`translate`] needs to know besides the event.
#[derive(Clone, Copy, Debug)]
struct KeyContext {
    /// Where keys go, with the highlighted pane in termis mode.
    mode: Mode,
    /// Whether the side panes are on screen; hidden ones cannot be
    /// focused.
    side_panes: bool,
    /// Whether `z` changes something on screen: side panes to hide, or
    /// hidden ones the width allows back.
    zoomable: bool,
    /// Whether the agent is still running; once it ended, `r` relaunches it
    /// and `q` quits.
    agent_alive: bool,
    /// Modes the child set, for the key encoder.
    modes: Modes,
    /// Whether the child asked for focus events.
    focus_events: bool,
}

/// Turns one host event into an action.
///
/// `Ctrl+Q` switches between agent mode and termis mode, and leaves the
/// commit box for agent mode. In agent mode keys and pastes go to the
/// agent, except `Ctrl+Z`, which would make the agent try to suspend
/// itself and wait forever; once the agent ended, `r` relaunches it and
/// `q` quits. In termis mode keys are commands, see [`term_command`], and
/// pastes are dropped. With the commit box open keys and pastes edit the
/// draft, see [`commit_key`]. Host focus events describe the window, so
/// they reach the agent in every mode. Key releases and repeats are
/// ignored.
fn translate(event: HostEvent, context: KeyContext) -> Action {
    let KeyContext {
        mode,
        side_panes,
        zoomable,
        agent_alive,
        modes,
        focus_events,
    } = context;
    let mut bytes = Vec::new();
    match event {
        HostEvent::Key(key) if key.kind != KeyEventKind::Press => {}
        HostEvent::Key(key) if is_mode_key(&key) => {
            return Action::Mode(match mode {
                Mode::Agent => Mode::Term(Pane::Agent),
                Mode::Term(_) | Mode::Commit => Mode::Agent,
            });
        }
        HostEvent::Key(key) if is_suspend(&key) => {}
        HostEvent::Key(key) => match mode {
            Mode::Commit => return commit_key(&key),
            Mode::Term(pane) => {
                return term_command(&key, pane, side_panes, zoomable, agent_alive);
            }
            Mode::Agent if agent_alive => {
                let _ = keys::encode(&key, modes, &mut bytes);
            }
            Mode::Agent => match key.code {
                KeyCode::Char('r') if key.modifiers.is_empty() => return Action::Relaunch,
                KeyCode::Char('q') if key.modifiers.is_empty() => return Action::Quit,
                _ => {}
            },
        },
        HostEvent::Paste(text) if mode == Mode::Agent && agent_alive => {
            keys::encode_paste(&text, modes, &mut bytes);
        }
        HostEvent::Paste(text) if mode == Mode::Commit => {
            return Action::Edit(Edit::Insert(pasted(&text)));
        }
        HostEvent::Paste(_) => {}
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

/// The termis-mode command bound to `key`, with `pane` highlighted.
///
/// `1`, `2` and `3` highlight a pane and `Tab` and `Shift+Tab` the next
/// and the previous one, the side panes only while they are shown; the
/// mode stays until `Esc` returns to the agent. With the changes pane
/// highlighted, `j`/`k` and the arrows move its selection, `s` and `u`
/// stage and unstage the selected row, `a` stages everything. `c` opens
/// the commit box. `z` toggles the zoom when that changes anything, `r`
/// relaunches an ended agent, `q` quits. Anything else is ignored.
fn term_command(
    key: &KeyEvent,
    pane: Pane,
    side_panes: bool,
    zoomable: bool,
    agent_alive: bool,
) -> Action {
    let plain = key.modifiers.is_empty();
    match key.code {
        KeyCode::Esc if plain => Action::Mode(Mode::Agent),
        KeyCode::Char('1') if plain && side_panes => Action::Mode(Mode::Term(Pane::Projects)),
        KeyCode::Char('2') if plain => Action::Mode(Mode::Term(Pane::Agent)),
        KeyCode::Char('3') if plain && side_panes => Action::Mode(Mode::Term(Pane::Changes)),
        KeyCode::Tab if plain && side_panes => Action::Mode(Mode::Term(pane.next())),
        KeyCode::BackTab if side_panes => Action::Mode(Mode::Term(pane.previous())),
        KeyCode::Char('j') | KeyCode::Down if plain && pane == Pane::Changes => {
            Action::Select(Step::Down)
        }
        KeyCode::Char('k') | KeyCode::Up if plain && pane == Pane::Changes => {
            Action::Select(Step::Up)
        }
        KeyCode::Char('s') if plain && pane == Pane::Changes => Action::Stage,
        KeyCode::Char('u') if plain && pane == Pane::Changes => Action::Unstage,
        KeyCode::Char('a') if plain && pane == Pane::Changes => Action::StageAll,
        KeyCode::Char('c') if plain => Action::OpenCommit,
        KeyCode::Char('z') if plain && zoomable => Action::Zoom,
        KeyCode::Char('r') if plain && !agent_alive => Action::Relaunch,
        KeyCode::Char('q') if plain => Action::Quit,
        _ => Action::Nothing,
    }
}

/// The commit box key `key`: text goes into the draft, `Backspace` or
/// `Ctrl+H` take a grapheme back, `Ctrl+J` starts a body line (not
/// `Shift+Enter`, which is a plain `Enter` without the kitty protocol),
/// `Enter` commits, `Esc` closes the box and keeps the draft. Anything
/// else is ignored.
fn commit_key(key: &KeyEvent) -> Action {
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let control = key.modifiers == KeyModifiers::CONTROL;
    match key.code {
        KeyCode::Esc if plain => Action::Mode(Mode::Term(Pane::Changes)),
        KeyCode::Enter if plain => Action::Commit,
        KeyCode::Char('j') if control => Action::Edit(Edit::Newline),
        KeyCode::Backspace if plain => Action::Edit(Edit::Backspace),
        KeyCode::Char('h') if control => Action::Edit(Edit::Backspace),
        KeyCode::Char(ch) if plain || shifted => Action::Edit(Edit::Insert(ch.to_string())),
        _ => Action::Nothing,
    }
}

/// Pasted text for the draft: line ends as newlines, tabs as four
/// spaces, other control characters dropped.
fn pasted(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect()
}

/// `Ctrl+Q`, which switches between agent mode and termis mode.
fn is_mode_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::CONTROL
}

/// `Ctrl+Z` with any other modifiers, which the app never forwards.
fn is_suspend(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('z' | 'Z')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn terminal_size(host: &Host) -> Result<Size> {
    let size = host.terminal.size().context("read terminal size")?;
    Ok(Size {
        rows: size.height,
        cols: size.width,
    })
}

/// The paths a stage or unstage acts on: the selected row's, when it is
/// in the list the action takes from (unstaged for a stage, staged for
/// an unstage), plus the source of a staged rename for an unstage, which
/// git needs to take both out of the index. None otherwise, or when the
/// path left the snapshot.
fn selected_paths(
    snapshot: &Snapshot,
    selected: Option<&Selected>,
    unstage: bool,
) -> Option<Vec<PathBuf>> {
    let selected = selected?;
    let from = if unstage {
        Section::Staged
    } else {
        Section::Unstaged
    };
    if selected.section != from {
        return None;
    }
    let entry = snapshot
        .status
        .entries
        .iter()
        .find(|entry| entry.path == selected.path)?;
    let mut paths = vec![entry.path.clone()];
    if unstage
        && entry.index == Some(Change::Renamed)
        && let Some(original) = &entry.original
    {
        paths.push(original.clone());
    }
    Some(paths)
}

/// The notice a git event deserves: the hash of a commit, the words of
/// a failed write (a failed commit says so, the others carry their
/// command), or a refresh failure the first time it reads so (a refresh
/// that works forgets it). None otherwise.
fn notice(
    event: &git::worker::Event,
    refresh_error: &mut Option<String>,
) -> Option<(String, toast::Kind)> {
    use git::worker::Event;
    match event {
        Event::Committed(hash) => Some((strings::toast_committed(hash), toast::Kind::Info)),
        Event::Failed {
            write: Write::Commit(_),
            error,
        } => Some((
            strings::commit_failed(&format!("{error:#}")),
            toast::Kind::Error,
        )),
        Event::Failed { error, .. } => Some((format!("{error:#}"), toast::Kind::Error)),
        Event::Refreshed(Ok(_)) => {
            *refresh_error = None;
            None
        }
        Event::Refreshed(Err(error)) => {
            let text = format!("{error:#}");
            if refresh_error.as_ref() == Some(&text) {
                return None;
            }
            *refresh_error = Some(text.clone());
            Some((text, toast::Kind::Error))
        }
    }
}

/// A notice refusing what the user asked, shown from now.
fn refusal(text: &str) -> Toast {
    Toast::new(
        text.to_owned(),
        toast::Kind::Error,
        Instant::now(),
        TOAST_LASTS,
    )
}

/// Why `c` cannot open the commit box: no repository, a commit still
/// running, or nothing staged. None when it can.
fn open_refusal(repo: bool, staged: bool, in_flight: bool) -> Option<&'static str> {
    if !repo {
        Some(strings::NOT_A_REPOSITORY)
    } else if in_flight {
        Some(strings::COMMIT_IN_PROGRESS)
    } else if !staged {
        Some(strings::NOTHING_STAGED)
    } else {
        None
    }
}

/// Why `Enter` cannot send the draft: no subject, or nothing staged any
/// more. None when it can.
fn send_refusal(staged: bool, has_subject: bool) -> Option<&'static str> {
    if !has_subject {
        Some(strings::SUBJECT_NEEDED)
    } else if !staged {
        Some(strings::NOTHING_STAGED)
    } else {
        None
    }
}

/// Whether the commit box can open or send: the side panes are shown
/// and something is staged.
fn can_commit(snapshot: Option<&Snapshot>, side_panes: bool) -> bool {
    side_panes && snapshot.is_some_and(|current| current.status.staged().next().is_some())
}

/// Last path component, or the path itself when it has none.
fn project_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Draws one frame. Returns the scroll offset of the changes pane for
/// the next one.
fn draw(host: &mut Host, full_screen: bool, view: &ui::View<'_>) -> Result<usize> {
    let mut scroll = view.scroll;
    host.terminal
        .draw(|frame| {
            if full_screen {
                ui::render_full(frame, view.emulator, view.ended, view.mode);
            } else {
                scroll = ui::render(frame, view);
            }
        })
        .context("draw")?;
    Ok(scroll)
}

/// Hangs the child up, waits for it to exit, kills it if it does not, and
/// kills its process group if something in it still holds the terminal.
fn shutdown(session: &Session, rx: &Receiver<Msg>, life: &mut Lifecycle) {
    let _ = session.hangup();
    wait_until(rx, life, session, GRACE, |session, _| session.has_exited());
    if !session.has_exited() {
        let _ = session.kill_now();
        wait_until(rx, life, session, KILL_GRACE, |session, _| {
            session.has_exited()
        });
    }
    if !life.closed {
        let _ = session.kill_group_now();
        wait_until(rx, life, session, KILL_GRACE, |_, closed| closed);
    }
}

/// Drains the channel until `done` holds or `limit` passes, noting the
/// current session's `Closed` event on the way.
fn wait_until(
    rx: &Receiver<Msg>,
    life: &mut Lifecycle,
    session: &Session,
    limit: Duration,
    done: impl Fn(&Session, bool) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !done(session, life.closed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(Msg::Pty(from, event)) => life.apply(from, &event, Instant::now()),
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

    fn key(key: KeyEvent) -> HostEvent {
        HostEvent::Key(key)
    }

    fn plain(code: KeyCode) -> HostEvent {
        HostEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Agent mode with the given encoder modes.
    fn agent(modes: Modes, focus_events: bool) -> KeyContext {
        KeyContext {
            mode: Mode::Agent,
            side_panes: true,
            zoomable: true,
            agent_alive: true,
            modes,
            focus_events,
        }
    }

    /// Termis mode with `pane` highlighted and default modes.
    fn termis(pane: Pane) -> KeyContext {
        KeyContext {
            mode: Mode::Term(pane),
            ..agent(Modes::default(), false)
        }
    }

    #[test]
    fn ctrl_q_enters_term_mode_and_ctrl_q_or_esc_leave_it() {
        let ctx = agent(Modes::default(), false);
        assert_eq!(
            translate(key(CTRL_Q), ctx),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        assert_eq!(
            translate(key(CTRL_Q), termis(Pane::Changes)),
            Action::Mode(Mode::Agent)
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), termis(Pane::Changes)),
            Action::Mode(Mode::Agent)
        );
        let alt_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::ALT);
        assert_eq!(
            translate(key(alt_esc), termis(Pane::Changes)),
            Action::Nothing
        );
        let collapsed = KeyContext {
            side_panes: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(key(CTRL_Q), collapsed), Action::Mode(Mode::Agent));
        assert_eq!(
            translate(plain(KeyCode::Esc), collapsed),
            Action::Mode(Mode::Agent)
        );
    }

    #[test]
    fn agent_mode_sends_keys_and_pastes_to_the_agent() {
        let ctx = agent(Modes::default(), false);
        for (code, bytes) in [
            (KeyCode::Char('q'), &b"q"[..]),
            (KeyCode::Char('1'), b"1"),
            (KeyCode::Char('z'), b"z"),
            (KeyCode::Tab, b"\t"),
            (KeyCode::Esc, b"\x1b"),
        ] {
            assert_eq!(
                translate(plain(code), ctx),
                Action::Send(bytes.to_vec()),
                "{code:?}"
            );
        }
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(back_tab), ctx),
            Action::Send(b"\x1b[Z".to_vec())
        );
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(translate(key(enter), ctx), Action::Send(b"\r".to_vec()));
        let kitty = Modes {
            protocol: Protocol::Kitty(KittyFlags(5)),
            ..Modes::default()
        };
        assert_eq!(
            translate(key(enter), agent(kitty, false)),
            Action::Send(b"\x1b[13;2u".to_vec())
        );
        let paste = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(
            translate(HostEvent::Paste("a\nb".into()), agent(paste, false)),
            Action::Send(b"\x1b[200~a\rb\x1b[201~".to_vec())
        );
    }

    #[test]
    fn term_mode_focuses_panes_with_digits_and_tab() {
        let ctx = termis(Pane::Agent);
        assert_eq!(
            translate(plain(KeyCode::Char('1')), ctx),
            Action::Mode(Mode::Term(Pane::Projects))
        );
        assert_eq!(
            translate(plain(KeyCode::Char('3')), ctx),
            Action::Mode(Mode::Term(Pane::Changes))
        );
        // The agent's pane is highlighted like the others: the mode stays.
        assert_eq!(
            translate(plain(KeyCode::Char('2')), termis(Pane::Changes)),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        assert_eq!(translate(plain(KeyCode::Char('4')), ctx), Action::Nothing);
        assert_eq!(
            translate(plain(KeyCode::Tab), ctx),
            Action::Mode(Mode::Term(Pane::Changes))
        );
        assert_eq!(
            translate(plain(KeyCode::Tab), termis(Pane::Projects)),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(back_tab), ctx),
            Action::Mode(Mode::Term(Pane::Projects))
        );
        assert_eq!(
            translate(key(back_tab), termis(Pane::Changes)),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        let ctrl_1 = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_1), ctx), Action::Nothing);
    }

    #[test]
    fn term_mode_runs_commands_and_drops_the_rest() {
        let ctx = termis(Pane::Agent);
        assert_eq!(translate(plain(KeyCode::Char('z')), ctx), Action::Zoom);
        assert_eq!(translate(plain(KeyCode::Char('q')), ctx), Action::Quit);
        assert_eq!(
            translate(plain(KeyCode::Char('q')), termis(Pane::Changes)),
            Action::Quit
        );
        // r relaunches only an ended agent.
        assert_eq!(translate(plain(KeyCode::Char('r')), ctx), Action::Nothing);
        for event in [
            plain(KeyCode::Char('x')),
            plain(KeyCode::Enter),
            HostEvent::Paste("secret".into()),
        ] {
            assert_eq!(translate(event, ctx), Action::Nothing);
        }
        let fixed = KeyContext {
            zoomable: false,
            ..ctx
        };
        assert_eq!(translate(plain(KeyCode::Char('z')), fixed), Action::Nothing);
        let shift_q = KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_q), ctx), Action::Nothing);
    }

    #[test]
    fn without_side_panes_only_the_agent_pane_can_be_focused() {
        let ctx = KeyContext {
            side_panes: false,
            zoomable: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(plain(KeyCode::Char('1')), ctx), Action::Nothing);
        assert_eq!(translate(plain(KeyCode::Char('3')), ctx), Action::Nothing);
        assert_eq!(translate(plain(KeyCode::Tab), ctx), Action::Nothing);
        let back_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(translate(key(back_tab), ctx), Action::Nothing);
        assert_eq!(
            translate(plain(KeyCode::Char('2')), ctx),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        assert_eq!(translate(plain(KeyCode::Char('z')), ctx), Action::Nothing);
        assert_eq!(translate(plain(KeyCode::Char('q')), ctx), Action::Quit);
    }

    #[test]
    fn after_the_agent_ended_r_relaunches_and_q_quits_in_both_modes() {
        let gone = KeyContext {
            agent_alive: false,
            ..agent(Modes::default(), false)
        };
        assert_eq!(translate(plain(KeyCode::Char('r')), gone), Action::Relaunch);
        assert_eq!(translate(plain(KeyCode::Char('q')), gone), Action::Quit);
        for event in [
            plain(KeyCode::Char('x')),
            plain(KeyCode::Enter),
            HostEvent::Paste("text".into()),
        ] {
            assert_eq!(translate(event, gone), Action::Nothing);
        }
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_r), gone), Action::Nothing);
        assert_eq!(
            translate(key(CTRL_Q), gone),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        let gone_term = KeyContext {
            agent_alive: false,
            ..termis(Pane::Changes)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('r')), gone_term),
            Action::Relaunch
        );
        assert_eq!(
            translate(plain(KeyCode::Char('q')), gone_term),
            Action::Quit
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), gone_term),
            Action::Mode(Mode::Agent)
        );
    }

    #[test]
    fn ctrl_z_is_never_forwarded() {
        let ctx = agent(Modes::default(), false);
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            for code in [KeyCode::Char('z'), KeyCode::Char('Z')] {
                let variant = KeyEvent::new(code, modifiers);
                assert_eq!(translate(key(variant), ctx), Action::Nothing);
                assert_eq!(
                    translate(key(variant), termis(Pane::Agent)),
                    Action::Nothing
                );
            }
        }
        // Plain z keeps its meanings: a key for the agent, the zoom in termis
        // mode.
        assert_eq!(
            translate(plain(KeyCode::Char('z')), ctx),
            Action::Send(b"z".to_vec())
        );
        assert_eq!(
            translate(plain(KeyCode::Char('z')), termis(Pane::Agent)),
            Action::Zoom
        );
    }

    #[test]
    fn host_focus_events_reach_the_agent_in_both_modes() {
        assert_eq!(
            translate(HostEvent::FocusGained, agent(Modes::default(), false)),
            Action::Nothing
        );
        assert_eq!(
            translate(HostEvent::FocusGained, agent(Modes::default(), true)),
            Action::Send(b"\x1b[I".to_vec())
        );
        assert_eq!(
            translate(HostEvent::FocusLost, agent(Modes::default(), true)),
            Action::Send(b"\x1b[O".to_vec())
        );
        let in_term = KeyContext {
            focus_events: true,
            ..termis(Pane::Projects)
        };
        assert_eq!(
            translate(HostEvent::FocusLost, in_term),
            Action::Send(b"\x1b[O".to_vec())
        );
        assert_eq!(
            translate(HostEvent::FocusLost, termis(Pane::Projects)),
            Action::Nothing
        );
    }

    #[test]
    fn key_releases_and_repeats_are_ignored() {
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        );
        assert_eq!(
            translate(key(release), agent(Modes::default(), false)),
            Action::Nothing
        );
        assert_eq!(
            translate(key(release), termis(Pane::Agent)),
            Action::Nothing
        );
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(
            translate(key(repeat), agent(Modes::default(), false)),
            Action::Nothing
        );
    }

    #[test]
    fn resize_maps_columns_and_rows() {
        assert_eq!(
            translate(HostEvent::Resize(120, 40), termis(Pane::Agent)),
            Action::Resize(Size {
                rows: 40,
                cols: 120
            })
        );
    }

    #[test]
    fn list_keys_move_the_selection_only_with_the_changes_pane_highlighted() {
        let ctx = termis(Pane::Changes);
        assert_eq!(
            translate(plain(KeyCode::Char('j')), ctx),
            Action::Select(Step::Down)
        );
        assert_eq!(
            translate(plain(KeyCode::Down), ctx),
            Action::Select(Step::Down)
        );
        assert_eq!(
            translate(plain(KeyCode::Char('k')), ctx),
            Action::Select(Step::Up)
        );
        assert_eq!(translate(plain(KeyCode::Up), ctx), Action::Select(Step::Up));
        let shift_j = KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_j), ctx), Action::Nothing);
        assert_eq!(
            translate(plain(KeyCode::Char('j')), termis(Pane::Agent)),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Down), termis(Pane::Projects)),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Char('j')), agent(Modes::default(), false)),
            Action::Send(b"j".to_vec())
        );
    }

    #[test]
    fn stage_keys_act_only_with_the_changes_pane_highlighted() {
        let ctx = termis(Pane::Changes);
        assert_eq!(translate(plain(KeyCode::Char('s')), ctx), Action::Stage);
        assert_eq!(translate(plain(KeyCode::Char('u')), ctx), Action::Unstage);
        assert_eq!(translate(plain(KeyCode::Char('a')), ctx), Action::StageAll);
        for code in ['s', 'u', 'a'] {
            assert_eq!(
                translate(plain(KeyCode::Char(code)), termis(Pane::Agent)),
                Action::Nothing
            );
            assert_eq!(
                translate(plain(KeyCode::Char(code)), agent(Modes::default(), false)),
                Action::Send(code.to_string().into_bytes())
            );
        }
        let shift_s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_s), ctx), Action::Nothing);
    }

    #[test]
    fn selected_paths_follow_the_row_and_its_list() {
        let entry = |path: &str, original: Option<&str>, index, worktree| git::status::Entry {
            path: path.into(),
            original: original.map(PathBuf::from),
            index,
            worktree,
            submodule: false,
        };
        let mut snapshot = Snapshot::default();
        snapshot.status.entries = vec![
            entry("new.txt", Some("old.txt"), Some(Change::Renamed), None),
            entry("dup.txt", Some("src.txt"), Some(Change::Copied), None),
            entry("plain.txt", None, None, Some(Change::Modified)),
            entry(
                "both.txt",
                None,
                Some(Change::Modified),
                Some(Change::Modified),
            ),
            entry("conflict.txt", None, None, Some(Change::Unmerged)),
        ];
        let row = |section, path: &str| Selected {
            section,
            path: path.into(),
        };
        let paths = |names: &[&str]| Some(names.iter().map(PathBuf::from).collect::<Vec<_>>());
        let (staged, unstaged) = (Section::Staged, Section::Unstaged);

        // A staged row unstages, with the source of a rename but not of a
        // copy; it does not stage.
        let renamed = row(staged, "new.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&renamed), true),
            paths(&["new.txt", "old.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&renamed), false), None);
        let copied = row(staged, "dup.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&copied), true),
            paths(&["dup.txt"])
        );
        // An unstaged row stages; it does not unstage, so a conflict keeps
        // its record and a partial stage of the same path stays.
        let plain = row(unstaged, "plain.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&plain), false),
            paths(&["plain.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&plain), true), None);
        let conflict = row(unstaged, "conflict.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&conflict), false),
            paths(&["conflict.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&conflict), true), None);
        assert_eq!(
            selected_paths(&snapshot, Some(&row(unstaged, "both.txt")), true),
            None
        );
        assert_eq!(
            selected_paths(&snapshot, Some(&row(staged, "both.txt")), true),
            paths(&["both.txt"])
        );
        assert_eq!(
            selected_paths(&snapshot, Some(&row(unstaged, "gone.txt")), false),
            None
        );
        assert_eq!(selected_paths(&snapshot, None, false), None);
    }

    #[test]
    fn the_commit_box_takes_text_until_enter_or_esc() {
        let ctx = KeyContext {
            mode: Mode::Commit,
            ..agent(Modes::default(), false)
        };
        let insert = |text: &str| Action::Edit(Edit::Insert(text.into()));
        assert_eq!(translate(plain(KeyCode::Char('x')), ctx), insert("x"));
        assert_eq!(
            translate(plain(KeyCode::Char('q')), ctx),
            insert("q"),
            "q is text here"
        );
        let shift_x = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_x), ctx), insert("X"));
        assert_eq!(
            translate(plain(KeyCode::Backspace), ctx),
            Action::Edit(Edit::Backspace)
        );
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_j), ctx), Action::Edit(Edit::Newline));
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_enter), ctx),
            Action::Nothing,
            "not a body line"
        );
        let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_h), ctx), Action::Edit(Edit::Backspace));
        assert_eq!(translate(plain(KeyCode::Enter), ctx), Action::Commit);
        assert_eq!(
            translate(plain(KeyCode::Esc), ctx),
            Action::Mode(Mode::Term(Pane::Changes))
        );
        assert_eq!(translate(key(CTRL_Q), ctx), Action::Mode(Mode::Agent));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_c), ctx), Action::Nothing);
        assert_eq!(translate(plain(KeyCode::Tab), ctx), Action::Nothing);
        assert_eq!(
            translate(HostEvent::Paste("a\r\nb\x07c\td\r".into()), ctx),
            insert("a\nbc    d\n")
        );
        // c opens the box from any pane in termis mode.
        assert_eq!(
            translate(plain(KeyCode::Char('c')), termis(Pane::Agent)),
            Action::OpenCommit
        );
        assert_eq!(
            translate(plain(KeyCode::Char('c')), termis(Pane::Changes)),
            Action::OpenCommit
        );
        assert_eq!(
            translate(plain(KeyCode::Char('c')), agent(Modes::default(), false)),
            Action::Send(b"c".to_vec())
        );
    }

    #[test]
    fn the_commit_box_needs_the_side_panes_and_something_staged() {
        let mut snapshot = Snapshot::default();
        assert!(!can_commit(None, true));
        assert!(!can_commit(Some(&snapshot), true), "nothing staged");
        snapshot.status.entries = vec![git::status::Entry {
            path: "a".into(),
            original: None,
            index: Some(Change::Added),
            worktree: None,
            submodule: false,
        }];
        assert!(can_commit(Some(&snapshot), true));
        assert!(!can_commit(Some(&snapshot), false), "no pane to show it in");
        assert_eq!(Mode::Commit.pane(), Pane::Changes);
    }

    #[test]
    fn the_commit_box_refuses_with_a_reason() {
        assert_eq!(
            open_refusal(false, false, false),
            Some(strings::NOT_A_REPOSITORY)
        );
        assert_eq!(
            open_refusal(true, true, true),
            Some(strings::COMMIT_IN_PROGRESS)
        );
        assert_eq!(
            open_refusal(true, false, false),
            Some(strings::NOTHING_STAGED)
        );
        assert_eq!(open_refusal(true, true, false), None);
        assert_eq!(send_refusal(true, false), Some(strings::SUBJECT_NEEDED));
        assert_eq!(send_refusal(false, true), Some(strings::NOTHING_STAGED));
        assert_eq!(send_refusal(false, false), Some(strings::SUBJECT_NEEDED));
        assert_eq!(send_refusal(true, true), None);
        let notice = refusal(strings::NOTHING_STAGED);
        assert_eq!(notice.kind, toast::Kind::Error);
        assert!(notice.live(Instant::now()));
    }

    #[test]
    fn git_events_become_notices_and_refresh_failures_only_once() {
        use git::worker::Event;
        let mut last = None;
        assert_eq!(
            notice(&Event::Committed("abc1234".into()), &mut last),
            Some(("committed abc1234".into(), toast::Kind::Info))
        );
        let failed = Event::Failed {
            write: Write::StageAll,
            error: anyhow::anyhow!("fatal: no").context("git add"),
        };
        assert_eq!(
            notice(&failed, &mut last),
            Some(("git add: fatal: no".into(), toast::Kind::Error))
        );
        let hook = Event::Failed {
            write: Write::Commit("m\n".into()),
            error: anyhow::anyhow!("hook says no"),
        };
        assert_eq!(
            notice(&hook, &mut last),
            Some(("commit failed: hook says no".into(), toast::Kind::Error))
        );
        let broken = || {
            Event::Refreshed(Err(
                anyhow::anyhow!("not a git repository").context("git status")
            ))
        };
        assert_eq!(
            notice(&broken(), &mut last),
            Some((
                "git status: not a git repository".into(),
                toast::Kind::Error
            ))
        );
        assert_eq!(notice(&broken(), &mut last), None, "the same failure again");
        let other = Event::Refreshed(Err(anyhow::anyhow!("index.lock").context("git status")));
        assert!(
            notice(&other, &mut last).is_some(),
            "a different failure shows"
        );
        assert_eq!(
            notice(&Event::Refreshed(Ok(Snapshot::default())), &mut last),
            None
        );
        assert!(last.is_none(), "a refresh that works forgets the failure");
        assert!(
            notice(&broken(), &mut last).is_some(),
            "and the next one shows again"
        );
    }

    #[test]
    fn panes_cycle_both_ways() {
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            assert_eq!(pane.next().previous(), pane);
            assert_ne!(pane.next(), pane);
        }
        assert_eq!(Pane::Projects.next(), Pane::Agent);
        assert_eq!(Pane::Changes.next(), Pane::Projects);
        assert_eq!(Mode::Agent.pane(), Pane::Agent);
        assert_eq!(Mode::Term(Pane::Projects).pane(), Pane::Projects);
    }

    #[test]
    fn losing_the_side_panes_drops_the_highlight_and_keeps_the_mode() {
        assert_eq!(Mode::Agent.without_side_panes(), Mode::Agent);
        assert_eq!(
            Mode::Term(Pane::Changes).without_side_panes(),
            Mode::Term(Pane::Agent)
        );
        assert_eq!(
            Mode::Term(Pane::Agent).without_side_panes(),
            Mode::Term(Pane::Agent)
        );
        assert_eq!(Mode::Commit.without_side_panes(), Mode::Term(Pane::Agent));
    }

    #[test]
    fn layout_follows_zoom_width_and_full_screen() {
        let wide = Layout {
            terminal: Size {
                rows: 40,
                cols: 100,
            },
            full_screen: false,
            zoomed: false,
        };
        assert!(wide.side_panes() && wide.zoomable());
        assert_eq!(wide.agent_size(), Size { rows: 37, cols: 53 });

        let zoomed = Layout {
            zoomed: true,
            ..wide
        };
        assert!(!zoomed.side_panes() && zoomed.zoomable());
        assert_eq!(zoomed.agent_size(), Size { rows: 37, cols: 98 });

        // Narrowed while zoomed: one pane, and z would change nothing.
        let narrow_zoomed = Layout {
            terminal: Size { rows: 40, cols: 80 },
            ..zoomed
        };
        assert!(!narrow_zoomed.side_panes() && !narrow_zoomed.zoomable());
        assert_eq!(narrow_zoomed.agent_size(), Size { rows: 37, cols: 78 });
        let narrow = Layout {
            zoomed: false,
            ..narrow_zoomed
        };
        assert_eq!(narrow.agent_size(), narrow_zoomed.agent_size());
        assert!(!narrow.zoomable());

        let full = Layout {
            full_screen: true,
            ..wide
        };
        assert!(!full.side_panes() && !full.zoomable());
        assert_eq!(
            full.agent_size(),
            Size {
                rows: 40,
                cols: 100
            }
        );
        assert_eq!(
            Layout {
                terminal: Size { rows: 0, cols: 0 },
                ..full
            }
            .agent_size(),
            Size { rows: 1, cols: 1 }
        );
        assert_eq!(
            Layout {
                terminal: Size { rows: 2, cols: 2 },
                ..wide
            }
            .agent_size(),
            Size { rows: 1, cols: 1 }
        );
    }

    #[test]
    fn poll_is_due_at_once_then_every_interval_after_the_last_refresh() {
        let t0 = Instant::now();
        let every = Duration::from_secs(2);
        let mut poll = Poll::new(every);
        assert!(!poll.due(t0, false), "not wanted");
        assert!(poll.due(t0, true), "first at once");
        poll.sent(t0);
        assert!(!poll.due(t0 + every, true), "in flight");
        // A slow refresh pushes the next one out: the gap stays.
        poll.done(t0 + every * 2);
        assert!(
            !poll.due(t0 + every * 3 - Duration::from_millis(1), true),
            "too soon"
        );
        assert!(poll.due(t0 + every * 3, true));
        assert!(!poll.due(t0 + every * 3, false), "hidden panes");
    }

    #[test]
    fn project_name_is_the_last_component() {
        assert_eq!(project_name(Path::new("/Users/x/dev/term")), "term");
        assert_eq!(project_name(Path::new("/Users/x/dev/term/")), "term");
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[test]
    fn old_generation_events_are_ignored() {
        let now = Instant::now();
        let mut life = Lifecycle::default();
        life.relaunch();
        life.apply(0, &pty::Event::Closed, now);
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(0)), now);
        assert!(!life.closed && life.exited.is_none() && life.alive());
        assert!(!life.current(0) && life.current(1));
        life.apply(1, &pty::Event::Closed, now);
        assert!(life.closed);
    }

    #[test]
    fn ended_waits_for_closed_or_the_grace() {
        let t0 = Instant::now();
        let mut life = Lifecycle::default();
        assert!(!life.settle(t0));
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(3)), t0);
        assert!(!life.settle(t0) && life.alive());
        assert!(life.settle(t0 + CLOSE_GRACE) && !life.alive());
        assert!(!life.settle(t0 + CLOSE_GRACE), "settles once");
        assert_eq!(life.status().map(|s| s.exit_code()), Some(3));

        let mut life = Lifecycle::default();
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(0)), t0);
        life.apply(0, &pty::Event::Closed, t0);
        assert!(life.settle(t0));
        life.relaunch();
        assert!(life.alive() && life.status().is_none());
    }

    #[test]
    fn messages_convert_from_input_events() {
        let failed = input::Event::Failed(std::io::Error::other("x"));
        assert!(matches!(
            Msg::from(failed),
            Msg::Input(input::Event::Failed(_))
        ));
    }
}
