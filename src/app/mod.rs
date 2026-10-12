//! The application: wires the agent's pty, the emulator, the host input
//! and the key encoder together.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::event::{
    Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::git::status::Change;
use crate::git::{self, worker::Job, worker::Snapshot, worker::Worker, worker::Write};
use crate::input::{self, Reader};
use crate::instance::{Instance, Watch};
use crate::keys::{self, Modes};
use crate::projects;
use crate::pty::{self, ExitStatus, Session};
use crate::strings;
use crate::ui::changes::{self, Section, Selected, Step};
use crate::ui::commit::{Draft, Edit};
use crate::ui::diff::Diff;
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
/// How often the agent's telemetry files are looked at.
const TELEMETRY_EVERY: Duration = Duration::from_millis(250);
/// Lines a view moves for one notch of the mouse wheel.
const WHEEL_LINES: usize = 3;
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
    /// The user quit: an exit noted from now on is the one the shutdown
    /// caused, not the agent's own.
    quit: bool,
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
            pty::Event::Exited(_) if self.quit => {}
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

    /// Notes that the user quit. From then on the status is the one the
    /// agent exited with before, if any.
    fn quit(&mut self) {
        self.quit = true;
    }

    /// The exit status of the current session, once it exited on its own.
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

    /// The whole center pane, where the wheel moves the agent's view: the
    /// screen itself when the agent is drawn over the whole terminal.
    fn agent_pane(self) -> Rect {
        if self.full_screen {
            self.area()
        } else {
            ui::layout::panes_with(self.area(), self.side_panes()).center
        }
    }

    /// The agent's screen inside the center pane, where a position the
    /// agent is told about is counted from.
    fn agent_screen(self) -> Rect {
        ui::agent_area(self.area(), self.full_screen, self.side_panes())
    }

    /// The whole changes pane, while the side panes are shown.
    fn changes_pane(self) -> Option<Rect> {
        self.side_panes()
            .then(|| ui::layout::panes_with(self.area(), true).right)
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
        let size: Size = self.agent_screen().into();
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
    /// Show or hide the key list.
    Help,
    /// Show the diff of the selected row, or hide it.
    Diff,
    /// Scroll the diff.
    Scroll(Step),
    /// Move the agent pane's view through its scrollback.
    Scrollback(Movement),
    /// The mouse wheel over a pane: move its view.
    Wheel(Pane, Step),
    Nothing,
}

/// A movement of the agent pane's view through its scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Movement {
    Line(Step),
    Page(Step),
    /// Back to the live screen, where new output appears.
    Live,
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

/// The session's time as the agent section shows it: the duration the
/// agent last reported plus the time since it was read, stopped at the
/// agent's exit, and never behind what was shown before in the session.
#[derive(Debug, Default)]
struct SessionClock {
    reported: Option<Duration>,
    read: Option<Instant>,
    shown: Option<Duration>,
}

impl SessionClock {
    /// Notes the duration of a status read at `now`. Only a new one moves
    /// the base: an event alone brings none. None, as after a relaunch,
    /// starts over.
    fn read(&mut self, duration: Option<Duration>, now: Instant) {
        if duration != self.reported {
            self.reported = duration;
            self.read = Some(now);
        }
        if duration.is_none() {
            self.shown = None;
        }
    }

    /// The time to show, counted up to `until`; none before the agent
    /// reported a duration.
    fn time(&mut self, until: Instant) -> Option<Duration> {
        let time = self.reported? + until.saturating_duration_since(self.read?);
        let time = self.shown.map_or(time, |shown| shown.max(time));
        self.shown = Some(time);
        Some(time)
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
/// it and `q` quits. Returns the exit status of the current session when
/// it exited on its own before the user quit.
pub fn run(options: Options<'_>) -> Result<Option<ExitStatus>> {
    let agent = agent::default_agent();
    // Outside a repository the agent still runs; the changes pane says so.
    // Without git at all, termis does not start: the right pane needs it.
    // Checked once; the git worker refreshes it from M3 on.
    let repo = git::repo_root(options.cwd)?;
    // The project is the repository, not the subdirectory the agent runs in.
    let project = project_name(repo.as_deref().unwrap_or(options.cwd));
    let root = repo.clone().unwrap_or_else(|| options.cwd.to_path_buf());
    let stack = projects::detect(&root);
    // The root is a physical path, so the home is resolved the same way.
    let home = std::env::home_dir().map(|home| home.canonicalize().unwrap_or(home));
    let path = ui::projects::display_path(&root, home.as_deref());
    let launch = agent::Launch {
        cwd: options.cwd,
        root: &root,
        args: options.args,
    };
    // The instance outlives the host: declared first, dropped last, so
    // the terminal is restored before its directory goes. Without one the
    // agent runs without telemetry, and a toast says why.
    let (instance, telemetry_off) = match Instance::create().and_then(|instance| {
        agent.prepare(&launch, &instance)?;
        Ok(instance)
    }) {
        Ok(instance) => (Some(instance), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let mut host = Host::enter()?;
    if let Some(instance) = &instance {
        Host::watch_instance(instance.dir());
    }
    let mut layout = Layout {
        terminal: terminal_size(&host)?,
        full_screen: options.full_screen,
        zoomed: false,
    };
    let size = layout.agent_size();

    let (tx, rx) = mpsc::channel::<Msg>();
    let mut life = Lifecycle::default();
    let mut session = start_agent(
        agent.as_ref(),
        &launch,
        size,
        &tx,
        life.generation,
        instance.as_ref(),
    )?;
    let reader = Reader::spawn(tx.clone())?;
    let worker = match &repo {
        Some(root) => Some(Worker::spawn(root.clone(), tx.clone())?),
        None => None,
    };
    let mut poll = Poll::new(POLL_EVERY);
    let mut watch = Watch::default();
    let mut agent_status = agent::Status::default();
    let mut session_clock = SessionClock::default();
    let mut time_shown: Option<String> = None;
    let mut telemetry_next = Instant::now();
    let mut snapshot: Option<Snapshot> = None;
    let mut selected: Option<Selected> = None;
    let mut scroll: usize = 0;
    let mut draft = Draft::default();
    let mut in_flight: Option<Draft> = None;
    let mut toast: Option<Toast> = telemetry_off.map(|reason| {
        Toast::new(
            strings::telemetry_trouble(&reason),
            toast::Kind::Error,
            Instant::now(),
            TOAST_LASTS,
        )
    });
    let mut refresh_error: Option<String> = None;
    // Only true in termis mode: every way out of it clears it.
    let mut help = false;
    let mut diff: Option<Diff> = None;
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
    let mut age_shown: Option<String> = None;
    let mut outcome: Result<bool> = Ok(false);

    loop {
        let now = Instant::now();
        if let Some(instance) = &instance
            && now >= telemetry_next
        {
            telemetry_next = now + TELEMETRY_EVERY;
            if let Some((status, events)) = watch.changed(instance) {
                let fresh = agent.status(&status, &events);
                session_clock.read(fresh.duration, now);
                if fresh != agent_status {
                    agent_status = fresh;
                    dirty |= side_panes;
                }
            }
        }
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
            // A refresh dirties the screen only when something changed; a
            // mouse event only when it does something.
            dirty |= !matches!(
                msg,
                Msg::Git(_) | Msg::Input(input::Event::Host(HostEvent::Mouse(_)))
            );
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
                // An answer to the diff on screen; older ones are ignored.
                Msg::Git(git::worker::Event::Diff { request, result }) => {
                    if let Some(open) = &mut diff
                        && open.request == request
                    {
                        match result {
                            Ok(text) => {
                                open.accept(&request, &text);
                                dirty |= side_panes;
                            }
                            Err(error) => {
                                toast = Some(Toast::new(
                                    format!("{error:#}"),
                                    toast::Kind::Error,
                                    Instant::now(),
                                    TOAST_LASTS,
                                ));
                                diff = None;
                                scroll = 0;
                                dirty = true;
                            }
                        }
                    }
                }
                Msg::Git(git::worker::Event::Refreshed(result)) => {
                    poll.done(Instant::now());
                    // A failed refresh keeps the last snapshot; notice() reported it.
                    if let Ok(fresh) = result
                        && snapshot.as_ref() != Some(&*fresh)
                    {
                        selected = changes::reselect(snapshot.as_ref(), &fresh, selected.as_ref());
                        // An open diff follows its row, or goes when the row went.
                        if let Some(open) = &mut diff
                            && let Some(worker) = &worker
                        {
                            match diff_request(&fresh, selected.as_ref()) {
                                Some((request, path)) => {
                                    *open = Diff::new(request.clone(), path);
                                    worker.send(Job::Diff(request));
                                }
                                None => {
                                    diff = None;
                                    scroll = 0;
                                }
                            }
                        }
                        snapshot = Some(*fresh);
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
                    let typed = matches!(event, HostEvent::Key(_) | HostEvent::Paste(_));
                    let mouse = matches!(event, HostEvent::Mouse(_));
                    let context = KeyContext {
                        mode,
                        side_panes,
                        zoomable: layout.zoomable(),
                        agent_alive: life.alive(),
                        modes: emu.modes(),
                        focus_events: emu.focus_events(),
                        help,
                        diff: diff.is_some(),
                        agent_pane: layout.agent_pane(),
                        agent_screen: layout.agent_screen(),
                        changes_pane: layout.changes_pane(),
                        alternate_screen: emu.alternate_screen(),
                    };
                    let action = translate(event, context);
                    if mouse {
                        dirty |= action != Action::Nothing;
                    }
                    // Any command closes the list; the command still runs.
                    if help && closes_help(&action) {
                        help = false;
                    }
                    match action {
                        Action::Quit => outcome = Ok(true),
                        Action::Send(ref bytes) => session.write(bytes),
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
                        // Over the lists the wheel moves the selection; over
                        // an open diff it scrolls the diff, further down.
                        Action::Wheel(Pane::Changes, step) if diff.is_none() => {
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
                            // The new session starts without the old one's
                            // status and events, and with the status line
                            // the settings name now.
                            if let Some(instance) = &instance {
                                instance.clear();
                                if let Err(error) = agent.prepare(&launch, instance) {
                                    toast = Some(Toast::new(
                                        strings::telemetry_trouble(&format!("{error:#}")),
                                        toast::Kind::Error,
                                        Instant::now(),
                                        TOAST_LASTS,
                                    ));
                                }
                            }
                            session = start_agent(
                                agent.as_ref(),
                                &launch,
                                layout.agent_size(),
                                &tx,
                                life.generation,
                                instance.as_ref(),
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
                        Action::Help => help = !help,
                        Action::Diff => {
                            scroll = 0;
                            if diff.is_some() {
                                diff = None;
                            } else if let Some(worker) = &worker
                                && let Some(current) = &snapshot
                                && let Some((request, path)) =
                                    diff_request(current, selected.as_ref())
                            {
                                diff = Some(Diff::new(request.clone(), path));
                                worker.send(Job::Diff(request));
                            }
                        }
                        Action::Scroll(step) => {
                            scroll = match step {
                                Step::Down => scroll.saturating_add(1),
                                Step::Up => scroll.saturating_sub(1),
                            };
                        }
                        Action::Scrollback(movement) => {
                            let page = usize::from(emu.size().rows);
                            match movement {
                                Movement::Line(Step::Up) => emu.scroll_back(1),
                                Movement::Line(Step::Down) => emu.scroll_forward(1),
                                Movement::Page(Step::Up) => emu.scroll_back(page),
                                Movement::Page(Step::Down) => emu.scroll_forward(page),
                                Movement::Live => emu.scroll_to_live(),
                            }
                        }
                        Action::Wheel(Pane::Agent, Step::Up) => emu.scroll_back(WHEEL_LINES),
                        Action::Wheel(Pane::Agent, Step::Down) => emu.scroll_forward(WHEEL_LINES),
                        Action::Wheel(_, step) if diff.is_some() => {
                            scroll = match step {
                                Step::Down => scroll.saturating_add(WHEEL_LINES),
                                Step::Up => scroll.saturating_sub(WHEEL_LINES),
                            };
                        }
                        Action::Wheel(..) => {}
                        Action::Edit(ref edit) => draft.apply(edit),
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
                    // The diff lives only while the changes pane is the one
                    // highlighted, and goes when its lists change.
                    if diff.is_some() && closes_diff(&action, mode) {
                        diff = None;
                        scroll = 0;
                    }
                    // The view returns to the live screen when keys go
                    // back to the agent; the wheel and the window's own
                    // events leave it where it is.
                    if mode == Mode::Agent && typed {
                        emu.scroll_to_live();
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
        // The age of the last commit moves on its own: a frame is due
        // when the text the card shows for it changes.
        let clock = unix_now();
        let age = ui::projects::last_age(snapshot.as_ref(), clock);
        if age != age_shown {
            age_shown = age;
            dirty |= side_panes;
        }
        // So does the session's time, by the second, until the agent exits.
        let until = life
            .exited
            .as_ref()
            .map_or_else(Instant::now, |(_, at)| *at);
        let session_time = session_clock.time(until);
        let time_text = session_time.map(strings::session_time);
        if time_text != time_shown {
            time_shown = time_text;
            dirty |= side_panes;
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
                help,
                diff: diff.as_ref(),
                card: ui::projects::Card {
                    path: &path,
                    stack: stack.as_ref(),
                    now: clock,
                    agent: instance.as_ref().map(|_| ui::projects::AgentCard {
                        name: agent.name(),
                        status: &agent_status,
                        time: session_time,
                        ended: !life.alive(),
                    }),
                },
            };
            scroll = draw(&mut host, layout.full_screen, &view)?;
            dirty = false;
        }
    }

    // Only an exit the agent came to before the user quit is reported:
    // the hangup the shutdown sends ends it too. One that happened in the
    // same pass as the quit may still be on its way to the loop.
    if session.has_exited() && life.exited.is_none() {
        wait_until(&rx, &mut life, &session, KILL_GRACE, |_, life| {
            life.exited.is_some()
        });
    }
    let exited_first = life.exited.is_some();
    life.quit();
    drop(reader);
    if !(life.closed && exited_first) {
        shutdown(&session, &rx, &mut life);
    }
    Host::forget_child();
    Host::forget_instance();
    drop(host);
    outcome.map(|_| life.status())
}

/// Starts the agent on a pty of `size` and forwards its events, tagged
/// with `generation`, to `tx` from a thread.
fn start_agent(
    agent: &dyn agent::Agent,
    launch: &agent::Launch<'_>,
    size: Size,
    tx: &mpsc::Sender<Msg>,
    generation: u32,
    instance: Option<&Instance>,
) -> Result<Session> {
    let (events, received) = mpsc::channel::<pty::Event>();
    let spec = agent.spec(launch, size, instance);
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

/// What [`translate`] needs to know besides the event, for keys and for
/// the mouse alike.
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
    /// Whether the key list is open, so that `Esc` closes it first.
    help: bool,
    /// Whether the diff view is open, so that `j`/`k` scroll it and
    /// `Esc` closes it.
    diff: bool,
    /// The center pane, where the wheel moves the agent's view.
    agent_pane: Rect,
    /// The agent's screen inside it, which mouse reports count from.
    agent_screen: Rect,
    /// The changes pane while the side panes are shown, where the wheel
    /// moves the selection or the diff.
    changes_pane: Option<Rect>,
    /// Whether the agent is on its alternate screen, which keeps no
    /// history: there the wheel becomes arrow keys for it.
    alternate_screen: bool,
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
/// they reach the agent in every mode. The mouse wheel over the center
/// pane reaches the agent as a mouse report when it asked for the mouse,
/// in every mode; else as arrow keys while it is on its alternate screen
/// in agent mode; else it moves the agent's view. Over the changes pane
/// it moves the selection or the diff; under the key list it does
/// nothing. Other mouse events, key releases and repeats are ignored.
fn translate(event: HostEvent, context: KeyContext) -> Action {
    let KeyContext {
        mode,
        side_panes,
        zoomable,
        agent_alive,
        modes,
        focus_events,
        help,
        diff,
        agent_pane,
        agent_screen,
        changes_pane,
        alternate_screen,
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
                return term_command(&key, pane, side_panes, zoomable, agent_alive, help, diff);
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
        HostEvent::Mouse(_) if help => {}
        HostEvent::Mouse(mouse) => {
            let step = match mouse.kind {
                MouseEventKind::ScrollUp => Step::Up,
                MouseEventKind::ScrollDown => Step::Down,
                _ => return Action::Nothing,
            };
            let at = Position::new(mouse.column, mouse.row);
            if agent_pane.contains(at) {
                match modes.mouse {
                    // An agent that asked for the mouse scrolls itself: it
                    // gets the notch where it happened on its screen, a
                    // border counting as the cell next to it.
                    Some(encoding) if agent_alive => {
                        let last = |size: u16| size.saturating_sub(1);
                        let column =
                            at.x.saturating_sub(agent_screen.x)
                                .min(last(agent_screen.width));
                        let row =
                            at.y.saturating_sub(agent_screen.y)
                                .min(last(agent_screen.height));
                        keys::encode_wheel(
                            step == Step::Up,
                            column,
                            row,
                            mouse.modifiers,
                            encoding,
                            &mut bytes,
                        );
                    }
                    // The alternate screen keeps no history: the agent gets
                    // the arrow keys a terminal would turn the wheel into.
                    None if mode == Mode::Agent && agent_alive && alternate_screen => {
                        let code = match step {
                            Step::Up => KeyCode::Up,
                            Step::Down => KeyCode::Down,
                        };
                        let key = KeyEvent::new(code, KeyModifiers::NONE);
                        for _ in 0..WHEEL_LINES {
                            let _ = keys::encode(&key, modes, &mut bytes);
                        }
                    }
                    _ => return Action::Wheel(Pane::Agent, step),
                }
            } else if changes_pane.is_some_and(|pane| pane.contains(at)) {
                return Action::Wheel(Pane::Changes, step);
            }
        }
        HostEvent::FocusGained | HostEvent::FocusLost => {}
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
/// the commit box, `d` the diff of the selected row, which `j`/`k` then
/// scroll and `d` or `Esc` close. With the agent pane highlighted,
/// `j`/`k` and the arrows move its view a line through its scrollback,
/// `PgUp`/`PgDn` a screen, shifted or not, `G` back to the live
/// screen; the alternate screen has no history to move through. `z`
/// toggles the
/// zoom when that changes anything, `r` relaunches an ended agent, `q`
/// quits, `?` shows or hides the key list, which `Esc` closes first
/// while `help` is on. Anything else is ignored.
fn term_command(
    key: &KeyEvent,
    pane: Pane,
    side_panes: bool,
    zoomable: bool,
    agent_alive: bool,
    help: bool,
    diff: bool,
) -> Action {
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Char('?') if plain || shifted => Action::Help,
        KeyCode::Esc if plain && help => Action::Help,
        KeyCode::Esc if plain && diff => Action::Diff,
        KeyCode::Esc if plain => Action::Mode(Mode::Agent),
        KeyCode::Char('j') | KeyCode::Down if plain && diff => Action::Scroll(Step::Down),
        KeyCode::Char('k') | KeyCode::Up if plain && diff => Action::Scroll(Step::Up),
        KeyCode::Char('j') | KeyCode::Down if plain && pane == Pane::Agent => {
            Action::Scrollback(Movement::Line(Step::Down))
        }
        KeyCode::Char('k') | KeyCode::Up if plain && pane == Pane::Agent => {
            Action::Scrollback(Movement::Line(Step::Up))
        }
        KeyCode::PageDown if (plain || shifted) && pane == Pane::Agent => {
            Action::Scrollback(Movement::Page(Step::Down))
        }
        KeyCode::PageUp if (plain || shifted) && pane == Pane::Agent => {
            Action::Scrollback(Movement::Page(Step::Up))
        }
        KeyCode::Char('G') if (plain || shifted) && pane == Pane::Agent => {
            Action::Scrollback(Movement::Live)
        }
        KeyCode::Char('g') if shifted && pane == Pane::Agent => Action::Scrollback(Movement::Live),
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
        KeyCode::Char('d') if plain && pane == Pane::Changes => Action::Diff,
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
        // Handled with the diff on screen, so that an old answer says nothing.
        Event::Diff { .. } => None,
    }
}

/// Whether the diff view closes after `action` left the app in `mode`:
/// it lives only while the changes pane is the one highlighted, so a
/// mode change, the commit box, a zoom or a narrow terminal end it, and
/// so does a change to the lists.
fn closes_diff(action: &Action, mode: Mode) -> bool {
    mode != Mode::Term(Pane::Changes)
        || matches!(action, Action::Stage | Action::Unstage | Action::StageAll)
}

/// What to diff for the selected row, with the path to show: the staged
/// side with the source of a rename or copy, the work tree side, or an
/// untracked file whole. None when nothing is selected or the row is
/// gone from the snapshot.
fn diff_request(
    snapshot: &Snapshot,
    selected: Option<&Selected>,
) -> Option<(git::diff::Request, String)> {
    let selected = selected?;
    let entry = snapshot
        .status
        .entries
        .iter()
        .find(|entry| entry.path == selected.path)?;
    let staged = selected.section == Section::Staged;
    let mut paths = vec![entry.path.clone()];
    if staged
        && matches!(entry.index, Some(Change::Renamed | Change::Copied))
        && let Some(original) = &entry.original
    {
        paths.push(original.clone());
    }
    let request = git::diff::Request {
        paths,
        staged,
        untracked: !staged && entry.worktree == Some(Change::Untracked),
    };
    Some((request, entry.path.to_string_lossy().into_owned()))
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

/// Whether `action`, run while the key list is open, closes it: every
/// command does, while nothing, a resize and the bytes a focus event
/// sends leave it open.
fn closes_help(action: &Action) -> bool {
    !matches!(
        action,
        Action::Help | Action::Nothing | Action::Resize(_) | Action::Send(_) | Action::Wheel(..)
    )
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

/// Seconds since the Unix epoch; zero when the clock is before it.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Draws one frame. Returns the scroll offset of the changes pane for
/// the next one.
fn draw(host: &mut Host, full_screen: bool, view: &ui::View<'_>) -> Result<usize> {
    let mut scroll = view.scroll;
    host.terminal
        .draw(|frame| {
            if full_screen {
                ui::render_full(frame, view.emulator, view.ended, view.mode, view.help);
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
        wait_until(rx, life, session, KILL_GRACE, |_, life| life.closed);
    }
}

/// Drains the channel until `done` holds or `limit` passes, noting the
/// current session's `Closed` event on the way.
fn wait_until(
    rx: &Receiver<Msg>,
    life: &mut Lifecycle,
    session: &Session,
    limit: Duration,
    done: impl Fn(&Session, &Lifecycle) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !done(session, life) {
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
            help: false,
            diff: false,
            agent_pane: Rect::new(20, 0, 55, 39),
            agent_screen: Rect::new(21, 1, 53, 37),
            changes_pane: Some(Rect::new(75, 0, 25, 39)),
            alternate_screen: false,
        }
    }

    fn wheel(kind: MouseEventKind, column: u16, row: u16) -> HostEvent {
        HostEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn the_wheel_moves_the_view_under_it_in_every_mode() {
        use crossterm::event::MouseButton;
        let up = MouseEventKind::ScrollUp;
        let down = MouseEventKind::ScrollDown;
        for ctx in [
            agent(Modes::default(), false),
            termis(Pane::Changes),
            KeyContext {
                mode: Mode::Commit,
                ..agent(Modes::default(), false)
            },
        ] {
            assert_eq!(
                translate(wheel(up, 30, 5), ctx),
                Action::Wheel(Pane::Agent, Step::Up)
            );
            assert_eq!(
                translate(wheel(down, 74, 38), ctx),
                Action::Wheel(Pane::Agent, Step::Down),
                "last cell of the center pane"
            );
            assert_eq!(
                translate(wheel(down, 80, 5), ctx),
                Action::Wheel(Pane::Changes, Step::Down)
            );
            assert_eq!(
                translate(wheel(up, 5, 5), ctx),
                Action::Nothing,
                "projects pane"
            );
            assert_eq!(
                translate(wheel(up, 30, 39), ctx),
                Action::Nothing,
                "status bar"
            );
            assert_eq!(
                translate(wheel(up, 20, 0), ctx),
                Action::Wheel(Pane::Agent, Step::Up),
                "the pane's border counts"
            );
            assert_eq!(
                translate(wheel(up, 75, 0), ctx),
                Action::Wheel(Pane::Changes, Step::Up)
            );
            let click = MouseEventKind::Down(MouseButton::Left);
            assert_eq!(translate(wheel(click, 30, 5), ctx), Action::Nothing);
            assert_eq!(
                translate(wheel(MouseEventKind::Moved, 30, 5), ctx),
                Action::Nothing
            );
        }
        // Without side panes the agent's pane is the whole width.
        let wide = KeyContext {
            side_panes: false,
            agent_pane: Rect::new(0, 0, 100, 39),
            changes_pane: None,
            ..agent(Modes::default(), false)
        };
        assert_eq!(
            translate(wheel(up, 80, 5), wide),
            Action::Wheel(Pane::Agent, Step::Up)
        );
        assert!(!closes_help(&Action::Wheel(Pane::Agent, Step::Up)));
        // Under the key list the wheel does nothing.
        let listing = KeyContext {
            help: true,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(wheel(up, 30, 5), listing), Action::Nothing);
        // On the agent's alternate screen the wheel reaches it as arrows,
        // in the encoding it asked for; not in termis mode, not once ended.
        let paging = KeyContext {
            alternate_screen: true,
            ..agent(Modes::default(), false)
        };
        assert_eq!(
            translate(wheel(up, 30, 5), paging),
            Action::Send(b"\x1b[A\x1b[A\x1b[A".to_vec())
        );
        let application = KeyContext {
            modes: Modes {
                application_cursor: true,
                ..Modes::default()
            },
            ..paging
        };
        assert_eq!(
            translate(wheel(down, 30, 5), application),
            Action::Send(b"\x1bOB\x1bOB\x1bOB".to_vec())
        );
        let paging_term = KeyContext {
            alternate_screen: true,
            ..termis(Pane::Agent)
        };
        assert_eq!(
            translate(wheel(up, 30, 5), paging_term),
            Action::Wheel(Pane::Agent, Step::Up)
        );
        let paging_ended = KeyContext {
            agent_alive: false,
            ..paging
        };
        assert_eq!(
            translate(wheel(up, 30, 5), paging_ended),
            Action::Wheel(Pane::Agent, Step::Up)
        );
    }

    #[test]
    fn an_agent_that_asked_for_the_mouse_gets_the_wheel() {
        // Claude Code in its fullscreen view: the alternate screen and SGR
        // mouse reports.
        let mouse = Modes {
            mouse: Some(keys::MouseEncoding::Sgr),
            ..Modes::default()
        };
        let fullscreen = KeyContext {
            alternate_screen: true,
            ..agent(mouse, false)
        };
        let up = MouseEventKind::ScrollUp;
        let down = MouseEventKind::ScrollDown;
        // Cell (30, 5) is column 9, row 4 of the agent's screen at (21, 1).
        assert_eq!(
            translate(wheel(up, 30, 5), fullscreen),
            Action::Send(b"\x1b[<64;10;5M".to_vec()),
            "one report per notch, where it happened"
        );
        assert_eq!(
            translate(wheel(down, 20, 0), fullscreen),
            Action::Send(b"\x1b[<65;1;1M".to_vec()),
            "the border counts as the cell next to it"
        );
        assert_eq!(
            translate(wheel(down, 74, 38), fullscreen),
            Action::Send(b"\x1b[<65;53;37M".to_vec())
        );
        // In termis mode and the commit box too; not under the key list,
        // not once the agent ended, not over the changes pane.
        for mode in [Mode::Term(Pane::Changes), Mode::Commit] {
            let ctx = KeyContext { mode, ..fullscreen };
            assert_eq!(
                translate(wheel(up, 30, 5), ctx),
                Action::Send(b"\x1b[<64;10;5M".to_vec())
            );
        }
        let listing = KeyContext {
            help: true,
            mode: Mode::Term(Pane::Agent),
            ..fullscreen
        };
        assert_eq!(translate(wheel(up, 30, 5), listing), Action::Nothing);
        let ended = KeyContext {
            agent_alive: false,
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(up, 30, 5), ended),
            Action::Wheel(Pane::Agent, Step::Up)
        );
        assert_eq!(
            translate(wheel(up, 80, 5), fullscreen),
            Action::Wheel(Pane::Changes, Step::Up)
        );
        // On the main screen too, and in the encoding the agent asked for.
        let main = KeyContext {
            alternate_screen: false,
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(up, 30, 5), main),
            Action::Send(b"\x1b[<64;10;5M".to_vec())
        );
        let x10 = KeyContext {
            modes: Modes {
                mouse: Some(keys::MouseEncoding::X10),
                ..Modes::default()
            },
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(down, 30, 5), x10),
            Action::Send(vec![0x1b, b'[', b'M', 32 + 65, 32 + 10, 32 + 5])
        );
        // With the shift key held, as the agent would see it.
        let shifted = HostEvent::Mouse(crossterm::event::MouseEvent {
            kind: up,
            column: 30,
            row: 5,
            modifiers: KeyModifiers::SHIFT,
        });
        assert_eq!(
            translate(shifted, fullscreen),
            Action::Send(b"\x1b[<68;10;5M".to_vec())
        );
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
            Action::Scrollback(Movement::Line(Step::Down)),
            "the agent pane scrolls instead"
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
            notice(&Event::Refreshed(Ok(Box::default())), &mut last),
            None
        );
        assert!(last.is_none(), "a refresh that works forgets the failure");
        assert!(
            notice(&broken(), &mut last).is_some(),
            "and the next one shows again"
        );
    }

    #[test]
    fn the_key_list_opens_with_a_question_mark_and_esc_closes_it_first() {
        let ctx = termis(Pane::Agent);
        assert_eq!(translate(plain(KeyCode::Char('?')), ctx), Action::Help);
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shifted), ctx), Action::Help);
        assert_eq!(
            translate(plain(KeyCode::Char('?')), agent(Modes::default(), false)),
            Action::Send(b"?".to_vec())
        );
        let open = KeyContext { help: true, ..ctx };
        assert_eq!(translate(plain(KeyCode::Esc), open), Action::Help);
        assert_eq!(
            translate(plain(KeyCode::Esc), ctx),
            Action::Mode(Mode::Agent)
        );
        assert_eq!(translate(key(CTRL_Q), open), Action::Mode(Mode::Agent));
        assert_eq!(
            translate(plain(KeyCode::Char('q')), open),
            Action::Quit,
            "commands still run"
        );
        let ctrl = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl), ctx), Action::Nothing);
        let boxed = KeyContext {
            mode: Mode::Commit,
            ..agent(Modes::default(), false)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('?')), boxed),
            Action::Edit(Edit::Insert("?".into())),
            "text in the box"
        );
        for action in [
            Action::Quit,
            Action::Zoom,
            Action::Mode(Mode::Agent),
            Action::Select(Step::Down),
        ] {
            assert!(closes_help(&action), "{action:?}");
        }
        for action in [
            Action::Help,
            Action::Nothing,
            Action::Resize(Size { rows: 1, cols: 1 }),
            Action::Send(vec![]),
        ] {
            assert!(!closes_help(&action), "{action:?}");
        }
    }

    #[test]
    fn the_diff_opens_with_d_scrolls_with_j_and_k_and_closes_with_esc() {
        let ctx = termis(Pane::Changes);
        assert_eq!(translate(plain(KeyCode::Char('d')), ctx), Action::Diff);
        assert_eq!(
            translate(plain(KeyCode::Char('d')), termis(Pane::Agent)),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Char('d')), agent(Modes::default(), false)),
            Action::Send(b"d".to_vec())
        );
        let open = KeyContext { diff: true, ..ctx };
        assert_eq!(
            translate(plain(KeyCode::Char('j')), open),
            Action::Scroll(Step::Down)
        );
        assert_eq!(
            translate(plain(KeyCode::Up), open),
            Action::Scroll(Step::Up)
        );
        assert_eq!(translate(plain(KeyCode::Char('d')), open), Action::Diff);
        assert_eq!(translate(plain(KeyCode::Esc), open), Action::Diff);
        assert_eq!(
            translate(plain(KeyCode::Char('s')), open),
            Action::Stage,
            "still a command"
        );
        let both = KeyContext { help: true, ..open };
        assert_eq!(
            translate(plain(KeyCode::Esc), both),
            Action::Help,
            "the list closes first"
        );
        let changes = Mode::Term(Pane::Changes);
        for action in [Action::Stage, Action::Unstage, Action::StageAll] {
            assert!(closes_diff(&action, changes), "{action:?}");
        }
        for action in [
            Action::Scroll(Step::Up),
            Action::Help,
            Action::Zoom,
            Action::Nothing,
            Action::Diff,
        ] {
            assert!(!closes_diff(&action, changes), "{action:?}");
        }
        for mode in [
            Mode::Term(Pane::Agent),
            Mode::Term(Pane::Projects),
            Mode::Agent,
            Mode::Commit,
        ] {
            assert!(
                closes_diff(&Action::Nothing, mode),
                "{mode:?} has no pane for it"
            );
        }
    }

    #[test]
    fn scroll_keys_move_the_agent_view_only_with_its_pane_highlighted() {
        let ctx = termis(Pane::Agent);
        let back = Action::Scrollback(Movement::Line(Step::Up));
        let forward = Action::Scrollback(Movement::Line(Step::Down));
        assert_eq!(translate(plain(KeyCode::Char('k')), ctx), back);
        assert_eq!(translate(plain(KeyCode::Up), ctx), back);
        assert_eq!(translate(plain(KeyCode::Char('j')), ctx), forward);
        assert_eq!(translate(plain(KeyCode::Down), ctx), forward);
        assert_eq!(
            translate(plain(KeyCode::PageUp), ctx),
            Action::Scrollback(Movement::Page(Step::Up))
        );
        assert_eq!(
            translate(plain(KeyCode::PageDown), ctx),
            Action::Scrollback(Movement::Page(Step::Down))
        );
        let shift_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_g), ctx),
            Action::Scrollback(Movement::Live)
        );
        let shifted_lower = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shifted_lower), ctx),
            Action::Scrollback(Movement::Live),
            "a host that reports the shift and the base letter"
        );
        assert_eq!(translate(plain(KeyCode::Char('g')), ctx), Action::Nothing);
        let shift_page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_page_up), ctx),
            Action::Scrollback(Movement::Page(Step::Up)),
            "the host terminal's habit works too"
        );
        for other in [termis(Pane::Projects), termis(Pane::Changes)] {
            assert_eq!(translate(plain(KeyCode::PageUp), other), Action::Nothing);
            assert_eq!(translate(key(shift_g), other), Action::Nothing);
        }
        assert_eq!(
            translate(plain(KeyCode::PageUp), agent(Modes::default(), false)),
            Action::Send(b"\x1b[5~".to_vec()),
            "the agent gets the key in agent mode"
        );
        assert!(closes_help(&Action::Scrollback(Movement::Live)));
    }

    #[test]
    fn the_diff_request_follows_the_row_and_its_list() {
        let entry = |path: &str, original: Option<&str>, index, worktree| git::status::Entry {
            path: path.into(),
            original: original.map(PathBuf::from),
            index,
            worktree,
            submodule: false,
        };
        let mut snapshot = Snapshot::default();
        snapshot.status.entries = vec![
            entry(
                "new.txt",
                Some("old.txt"),
                Some(Change::Renamed),
                Some(Change::Modified),
            ),
            entry("fresh.txt", None, None, Some(Change::Untracked)),
        ];
        let row = |section, path: &str| Selected {
            section,
            path: path.into(),
        };
        let (request, path) =
            diff_request(&snapshot, Some(&row(Section::Staged, "new.txt"))).expect("staged");
        assert_eq!(path, "new.txt");
        assert!(request.staged && !request.untracked);
        assert_eq!(
            request.paths,
            [PathBuf::from("new.txt"), PathBuf::from("old.txt")]
        );
        let (request, _) =
            diff_request(&snapshot, Some(&row(Section::Unstaged, "new.txt"))).expect("unstaged");
        assert!(!request.staged && !request.untracked);
        assert_eq!(
            request.paths,
            [PathBuf::from("new.txt")],
            "the work tree side knows one path"
        );
        let (request, _) =
            diff_request(&snapshot, Some(&row(Section::Unstaged, "fresh.txt"))).expect("untracked");
        assert!(request.untracked && !request.staged);
        assert_eq!(
            diff_request(&snapshot, Some(&row(Section::Unstaged, "gone"))),
            None
        );
        assert_eq!(diff_request(&snapshot, None), None);
        let mut last = None;
        let answer = git::worker::Event::Diff {
            request: request.clone(),
            result: Err(anyhow::anyhow!("fatal: bad")),
        };
        assert_eq!(
            notice(&answer, &mut last),
            None,
            "diff answers are not notices"
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
    fn the_session_clock_runs_between_reads_and_stops_at_the_exit() {
        let t0 = Instant::now();
        let second = Duration::from_secs(1);
        let mut clock = SessionClock::default();
        assert_eq!(clock.time(t0), None, "nothing reported yet");
        clock.read(Some(10 * second), t0);
        assert_eq!(clock.time(t0 + 3 * second), Some(13 * second));
        // An event alone brings the same duration: the base stays.
        clock.read(Some(10 * second), t0 + 4 * second);
        assert_eq!(clock.time(t0 + 5 * second), Some(15 * second));
        // A new duration read late would step back: it never shows less.
        clock.read(Some(14 * second), t0 + 5 * second);
        assert_eq!(clock.time(t0 + 5 * second), Some(15 * second));
        assert_eq!(clock.time(t0 + 7 * second), Some(16 * second));
        // Stopped at the exit: counting up to it, however late.
        assert_eq!(clock.time(t0 + 7 * second), Some(16 * second));
        // A relaunch clears the files: no duration, the row goes.
        clock.read(None, t0 + 8 * second);
        assert_eq!(clock.time(t0 + 9 * second), None);
        clock.read(Some(second), t0 + 9 * second);
        assert_eq!(clock.time(t0 + 9 * second), Some(second), "starts over");
    }

    #[test]
    fn an_exit_after_the_quit_is_not_the_agents_own() {
        let now = Instant::now();
        // Alive at the quit: the hangup's exit is not reported.
        let mut life = Lifecycle::default();
        life.quit();
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(129)), now);
        assert!(life.status().is_none());
        life.apply(0, &pty::Event::Closed, now);
        assert!(life.closed, "the end of output still counts");
        // Exited before the quit: that status stays.
        let mut life = Lifecycle::default();
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(3)), now);
        life.quit();
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(129)), now);
        assert_eq!(life.status().map(|status| status.exit_code()), Some(3));
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
