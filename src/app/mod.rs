//! The application: wires the agent sessions (`session`), their
//! repositories (`repo`), the host input and the key encoder together.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::event::{
    Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::Size;
use crate::agent;
use crate::emu;
use crate::git::find::{Finder, Found};
use crate::git::{self, worker::Snapshot};
use crate::input::{self, Reader};
use crate::instance::Root;
use crate::keys::{self, Modes, Tracking};
use crate::projects::walk::{self, Walked};
use crate::pty::{self, ExitStatus};
use crate::strings;
use crate::ui::changes::{Movement, Step};
use crate::ui::commit::Edit;
use crate::ui::picker::{Item, Picker};
use crate::ui::toast::{self, Toast};
use crate::ui::{self, Mode, Pane};

pub mod host;
mod open;
mod repo;
mod session;
mod shell;
mod tree;
mod viewer;

use host::Host;
use open::{Lookup, Roots, Target};
use repo::{RepoId, Repos};
use session::{Place, RowId, Session, SessionId, Sessions, Starter, Tab, TabId, TabKey};
use shell::Shell;
use strings::Closes;
use viewer::ReadKey;

/// Lines of output kept above the screen.
const SCROLLBACK: usize = 5000;
/// Longest wait between two passes of the main loop.
const TICK: Duration = Duration::from_millis(16);
/// Messages handled per pass before drawing, so a chatty child cannot
/// starve the screen.
const DRAIN: usize = 256;
/// How often the agent's telemetry files are looked at.
const TELEMETRY_EVERY: Duration = Duration::from_millis(250);
/// Lines a view moves for one notch of the mouse wheel.
const WHEEL_LINES: usize = 3;
/// How long a notice stays in the status bar.
const TOAST_LASTS: Duration = Duration::from_secs(5);
/// How long the second press of `x` or `q` has to come, while an agent
/// it would stop is working or asking; the notice asking for it lasts as
/// long.
const CONFIRM_WITHIN: Duration = Duration::from_secs(3);

/// Everything the main loop receives. Pty events carry the session and
/// the tab that produced them and the generation of the tab's launch, so
/// a relaunch can ignore stragglers; git events the repository whose
/// worker sent them; the finder's answers and the walks' the directory
/// they answer; a file read for a viewer the session, the viewer's tab
/// and the read it answers.
enum Msg {
    Pty(TabKey, pty::Event),
    Input(input::Event),
    Git(RepoId, git::worker::Event),
    Found(Found),
    Walked(Walked),
    Read(ReadKey, ui::viewer::Content),
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
}

impl Mode {
    /// The mode once the side panes are gone: the highlight falls back to
    /// the agent's pane, the mode stays; the picker, over the whole
    /// screen, stays open.
    fn without_side_panes(self) -> Self {
        match self {
            Self::Agent => Self::Agent,
            Self::Picker => Self::Picker,
            Self::Term(_) | Self::Commit => Self::Term(Pane::Agent),
        }
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

    /// The agent's screen size, at least [`emu::MIN_SIZE`] each way so the
    /// pty and the emulator always agree.
    fn agent_size(self) -> Size {
        let size: Size = self.agent_screen().into();
        Size {
            rows: size.rows.max(emu::MIN_SIZE.rows),
            cols: size.cols.max(emu::MIN_SIZE.cols),
        }
    }

    /// Whether `z` changes something on screen: side panes to hide,
    /// or hidden ones the width allows back.
    fn zoomable(self) -> bool {
        !self.full_screen && (self.side_panes() || (self.zoomed && self.wide()))
    }
}

/// Resizes every agent to `layout` where its size changed, and drops
/// the highlight to the agent's pane when the side panes are gone.
/// Returns a notice when an agent's pty refused its size.
fn apply_layout(
    layout: Layout,
    sessions: &mut Sessions,
    side_panes: &mut bool,
    mode: &mut Mode,
) -> Option<String> {
    let notice = sessions.resize(layout.agent_size());
    *side_panes = layout.side_panes();
    if !*side_panes {
        *mode = mode.without_side_panes();
    }
    notice
}

/// What a host event asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    /// Close the tab under the list's cursor once it moved, else the tab
    /// shown: a terminal, the viewer, or an agent's with its session.
    /// Closing the last session quits.
    Close,
    Send(Vec<u8>),
    Resize(Size),
    Mode(Mode),
    Zoom,
    Relaunch,
    /// Move the selection of the changes pane.
    Select(Movement),
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
    Scroll(Movement),
    /// Move the agent pane's view through its scrollback, or through the
    /// file the viewer shows.
    Scrollback(Movement),
    /// The mouse wheel over a pane: move its view.
    Wheel(Pane, Step),
    /// Open the project picker.
    OpenPicker,
    /// Add text to the picker's query.
    Type(String),
    /// Take the last grapheme off the picker's query.
    Erase,
    /// Choose the picker's row above or below.
    Choose(Step),
    /// Open what the picker chose: a directory with no session on a new
    /// conversation when `new_conversation`, else continuing its last
    /// one when there is one.
    Open {
        new_conversation: bool,
    },
    /// Show the next session, or the previous one.
    Switch(Step),
    /// Start the user's shell in a terminal of the session shown, and
    /// show it.
    OpenTerminal,
    /// Start the user's editor on a file in a terminal of the session
    /// shown, and show it: the file under the file tree's cursor with the
    /// projects pane highlighted, else the one the viewer shows.
    OpenEditor,
    /// Show the next tab of the session shown, or the previous one.
    SwitchTab(Step),
    /// Move the cursor of the list of sessions.
    Cursor(Movement),
    /// Show the tab under the list's cursor, a terminal, the viewer or an
    /// agent's, with its session.
    Show,
    /// Show the file tree of the session shown in the projects pane, or
    /// the list of sessions again.
    Files,
    /// Move the file tree's cursor.
    TreeCursor(Movement),
    /// Unfold the directory under the file tree's cursor.
    TreeOpen,
    /// Unfold the directory under the file tree's cursor, or show the
    /// file under it in the viewer.
    TreeEnter,
    /// Fold the directory under the file tree's cursor, or go to the one
    /// the entry is in.
    TreeClose,
    Nothing,
}

/// What a press of `x` or `q` ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// `termis`, with every session.
    Quit,
    /// Session `id`, or `termis` when it is the last one.
    Close(SessionId),
    /// Tab `tab` of session `id`: a terminal, or the viewer.
    CloseTab(SessionId, TabId),
}

/// A first press of `x` or `q` waiting for its second: what it ends,
/// until when the second press counts, and the notice asking for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Armed {
    ending: Ending,
    until: Instant,
    notice: String,
}

/// The first press `armed` while its notice is the `toast` shown; none
/// once the notice went or another one took its place.
fn waiting(armed: Option<Armed>, toast: Option<&Toast>) -> Option<Armed> {
    armed.filter(|armed| toast.is_some_and(|toast| toast.text == armed.notice))
}

/// Whether a press that ends `ending` at `now` acts: at once while
/// nothing it would stop is `busy`, else only as the second press of the
/// same within [`CONFIRM_WITHIN`] of the first, `armed`.
fn acts(armed: Option<&Armed>, ending: Ending, busy: bool, now: Instant) -> bool {
    !busy || armed.is_some_and(|armed| armed.ending == ending && now < armed.until)
}

/// What `x` ends on `row` of the list of sessions: its terminal or its
/// viewer, or its session on the agent's tab.
fn close_ending(row: RowId) -> Ending {
    if row.tab.is_agent() {
        Ending::Close(row.session)
    } else {
        Ending::CloseTab(row.session, row.tab)
    }
}

/// Whether `ending`, once it acts, quits: `q` does, and so does closing
/// the `last` session.
fn quits(ending: Ending, last: bool) -> bool {
    match ending {
        Ending::Quit => true,
        Ending::Close(_) => last,
        Ending::CloseTab(..) => false,
    }
}

/// The notice a first press that ends `ending` shows while an agent it
/// would stop is working or asking, or a program other than the shell
/// runs in a terminal it would close: `q` stops every agent and closes
/// every terminal, `x` the agent and the terminals of the session it
/// closes, or the terminal it closes. An agent is named before a
/// terminal. None while there is neither, and the press acts.
fn confirmation(sessions: &Sessions, ending: Ending) -> Option<String> {
    match ending {
        Ending::Quit => {
            let mut busy = sessions
                .iter()
                .filter_map(|session| session.busy().map(|state| (session, state)));
            if let Some((first, state)) = busy.next() {
                let project = &first.place.project;
                return Some(strings::confirm_quit(project, state, 1 + busy.count()));
            }
            let running = sessions
                .iter()
                .flat_map(|session| session.running().map(move |tab| (session, tab)));
            program_notice(running, 'q', Closes::Quit)
        }
        Ending::Close(id) => {
            let session = sessions.get(id)?;
            let last = sessions.position().is_none();
            if let Some(state) = session.busy() {
                return Some(strings::confirm_close(&session.place.project, state, last));
            }
            let closes = if last { Closes::Quit } else { Closes::Session };
            program_notice(session.running().map(|tab| (session, tab)), 'x', closes)
        }
        Ending::CloseTab(id, tab) => {
            let session = sessions.get(id)?;
            let running = session.running().filter(|terminal| terminal.id == tab);
            program_notice(running.map(|tab| (session, tab)), 'x', Closes::Terminal)
        }
    }
}

/// The notice a first press of `key` that `closes` shows while a program
/// other than the shell runs in the terminals of `running`, each with
/// its session: the first one named, the others counted. None while
/// there are none.
fn program_notice<'a>(
    mut running: impl Iterator<Item = (&'a Session, &'a Tab)>,
    key: char,
    closes: Closes,
) -> Option<String> {
    let (session, first) = running.next()?;
    let name = first.name.as_deref().unwrap_or_default();
    let project = &session.place.project;
    Some(strings::confirm_program(
        project,
        name,
        running.count(),
        key,
        closes,
    ))
}

/// The mouse buttons whose press went to the agent and that are still
/// held: their drags and their release go to it too, wherever they
/// happen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held(u8);

impl Held {
    fn bit(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 4,
        }
    }

    fn has(self, button: MouseButton) -> bool {
        self.0 & Self::bit(button) != 0
    }

    /// The buttons held once `kind` became `action`: a press sent to the
    /// agent adds its button; a press kept from it and a release take
    /// the button away.
    fn after(self, kind: MouseEventKind, action: &Action) -> Self {
        match kind {
            MouseEventKind::Down(button) if matches!(action, Action::Send(_)) => {
                Self(self.0 | Self::bit(button))
            }
            MouseEventKind::Down(button) | MouseEventKind::Up(button) => {
                Self(self.0 & !Self::bit(button))
            }
            _ => self,
        }
    }
}

/// Options for [`run`].
pub struct Options<'a> {
    /// Directory the agent runs in.
    pub cwd: &'a Path,
    /// Arguments passed to the agent after `--`.
    pub args: &'a [String],
    /// Start the first launch on a new conversation instead of continuing
    /// the last one.
    pub new_conversation: bool,
    /// Draw the agent over the whole terminal, with no side panes.
    pub full_screen: bool,
    /// Debug aid: panic from a worker thread after this many seconds.
    pub panic_after: Option<u64>,
}

/// Runs the agent, in the center pane or over the whole terminal. `Ctrl+Q`
/// enters termis mode, where `q` quits, `o` opens another project in a
/// session of its own, `t` a terminal in the session shown, and `x`
/// closes the tab shown, the agent's with its session, or the one under
/// the cursor of the projects pane's list, quitting on the last;
/// while an agent they would stop is working or asking, or a program
/// runs in a terminal they would close, `x` and `q` take a second press.
/// Once the agent ended, `r` relaunches it and `x` or `q` close its
/// session, quitting on the last. Returns the exit status of the shown
/// session's current launch when it exited on its own before the user
/// quit.
pub fn run(options: Options<'_>) -> Result<Option<ExitStatus>> {
    let agent = agent::default_agent();
    // Outside a repository the agent still runs; the changes pane says so.
    // Without git at all, termis does not start: the right pane needs it.
    // Checked once; the git worker refreshes it from M3 on.
    let repo = git::repo_root(options.cwd)?;
    // The root is a physical path, so the home is resolved the same way.
    let home = std::env::home_dir().map(|home| home.canonicalize().unwrap_or(home));
    let place = Place::new(options.cwd.to_path_buf(), repo, home.as_deref());
    // Declared before the sessions and the host: dropped after them, so
    // the terminal is restored and the sessions are closed before it goes.
    // Without it every session runs without telemetry, and a toast says
    // why.
    let instances = Root::create();
    // Declared before the host: dropped after it, so the terminal is
    // restored before a session hangs up or its directory goes.
    let mut sessions = Sessions::new();
    let mut host = Host::enter()?;
    if let Ok(instances) = &instances {
        Host::watch_instance(instances.dir());
    }
    let mut layout = Layout {
        terminal: terminal_size(&host)?,
        full_screen: options.full_screen,
        zoomed: false,
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let starter = Starter {
        agent: agent.as_ref(),
        instances: instances.as_ref(),
        tx: &tx,
    };
    // Without an instance the agent runs without telemetry, and a toast
    // says why.
    let (first, telemetry_off) = starter.start(
        sessions.next_id(),
        place,
        options.args.to_vec(),
        options.new_conversation,
        layout.agent_size(),
    )?;
    sessions.push(first);
    let reader = Reader::spawn(tx.clone())?;
    let finder = Finder::spawn(tx.clone(), Msg::Found)?;
    let mut repos = Repos::new();
    if let Some(root) = sessions.shown().place.repo.clone() {
        sessions.shown_mut().repo = Some(repos.get_or_spawn(&root, &tx)?);
    }
    // The repositories found for directories chosen in the picker.
    let mut roots = Roots::default();
    // Some only in picker mode: every way out of it drops it.
    let mut picker: Option<Picker> = None;
    // The row under the list's cursor once the cursor moved, and only
    // while the projects pane is highlighted; until then, and once that
    // row left the list, the cursor is on the row of the tab shown, as
    // `Sessions::cursor_row` reads it.
    let mut cursor: Option<RowId> = None;
    let mut time_shown: Option<String> = None;
    let mut telemetry_next = Instant::now();
    let mut toast: Option<Toast> =
        telemetry_off.map(|reason| trouble(strings::telemetry_trouble(&reason)));
    // Only true in termis mode: every way out of it clears it.
    let mut help = false;
    // A first press of `x` or `q` that waits for its second while its
    // notice is up.
    let mut armed: Option<Armed> = None;
    if let Some(seconds) = options.panic_after {
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            panic!("termis --panic-after {seconds}: deliberate panic");
        });
    }

    let mut mode = Mode::default();
    let mut side_panes = layout.side_panes();
    let mut dirty = true;
    let mut held = Held::default();
    // Whether the host window has the focus, as its focus events last
    // said; it has until one says otherwise.
    let mut host_focused = true;
    // Whether the host reports motion with no button held.
    let mut host_motion = false;
    let mut age_shown: Option<String> = None;
    let mut limits_shown: [Option<ui::projects::LimitRow>; 2] = [None, None];
    let mut marks_shown: Option<Vec<(Option<&str>, bool)>> = None;
    let mut asking_shown = 0;
    // The session whose file tree was on screen on the last pass.
    let mut tree_seen: Option<SessionId> = None;
    // How many rows of the side panes' lists and views the last frame
    // showed, which half and whole pages move by.
    let mut pages = ui::Pages::default();
    let mut outcome: Result<bool> = Ok(false);

    loop {
        let now = Instant::now();
        if now >= telemetry_next {
            telemetry_next = now + TELEMETRY_EVERY;
            let read = sessions.read_telemetry(agent.as_ref(), now);
            if read.shown {
                dirty |= side_panes;
            }
            // An agent not shown that starts asking says so once, with
            // the bell.
            if let Some((first, others)) = read.asking.split_first() {
                let text = strings::started_asking(first, others.len());
                toast = Some(Toast::new(text, toast::Kind::Attention, now, TOAST_LASTS));
                dirty = true;
                host.bell()?;
            }
        }
        // The list of sessions shows beside the changes pane from two
        // sessions on, unless the session shown has its file tree in its
        // place.
        let tree_shown = side_panes && sessions.shown().tree.shown;
        let list_shown = side_panes && sessions.position().is_some() && !tree_shown;
        repos.read_when_due(now, sessions.shown().repo, side_panes, list_shown);
        // A file tree reads the files as it comes on screen, and again as
        // they change while it is there.
        let on_screen = tree_shown.then(|| sessions.shown().id);
        if on_screen.is_some() && on_screen != tree_seen {
            sessions.shown_mut().tree.want();
        }
        tree_seen = on_screen;
        ask_files(&mut sessions, &repos, &tx, now, tree_shown);
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
            let shown = sessions.shown();
            dirty |= dirties(&msg, shown.id, shown.shown_tab());
            match msg {
                Msg::Git(id, event) => {
                    let applied = repos.apply(id, event, side_panes, Instant::now());
                    dirty |= applied.dirty;
                    if applied.toast.is_some() {
                        toast = applied.toast;
                    }
                    if applied.status_changed {
                        sessions.files_changed(id);
                    }
                    // A tree not on screen keeps them for later.
                    if let Some((dir, files)) = &applied.files
                        && sessions.repo_files(id, dir, files, Instant::now())
                    {
                        dirty |= side_panes && sessions.shown().tree.shown;
                    }
                }
                Msg::Walked(walked) => {
                    if sessions.dir_files(&walked, Instant::now()) {
                        dirty |= side_panes && sessions.shown().tree.shown;
                    }
                }
                // An answer for a read no viewer waits for is dropped.
                Msg::Read(key, content) => dirty |= sessions.file_read(key, content),
                Msg::Pty(key, event) => sessions.on_pty(key, event, Instant::now()),
                // The directory chosen in the picker that waited for it opens
                // while that picker is still open; once it closed, the
                // choice is dropped.
                Msg::Found(found) => {
                    if let Some(new_conversation) = roots.answer(&found)
                        && mode == Mode::Picker
                    {
                        let outcome = match found.root {
                            Ok(root) => {
                                let place = Place::new(found.dir, root, home.as_deref());
                                let size = layout.agent_size();
                                open::open(
                                    &starter,
                                    &mut sessions,
                                    &mut repos,
                                    place,
                                    new_conversation,
                                    size,
                                    host_focused,
                                )
                            }
                            Err(error) => Err(strings::session_trouble(
                                &session::project_name(&found.dir),
                                &format!("{error:#}"),
                            )),
                        };
                        opened(outcome, &mut mode, &mut held, &mut toast);
                    }
                }
                Msg::Input(input::Event::Failed(error)) => {
                    outcome = Err(error).context("read host input");
                }
                Msg::Input(input::Event::Host(event)) => {
                    match event {
                        HostEvent::FocusGained => host_focused = true,
                        HostEvent::FocusLost => host_focused = false,
                        _ => {}
                    }
                    let typed = matches!(event, HostEvent::Key(_) | HostEvent::Paste(_));
                    let mouse = match &event {
                        HostEvent::Mouse(mouse) => Some(mouse.kind),
                        _ => None,
                    };
                    let shown = sessions.shown();
                    // None on the viewer, which has no child.
                    let tab = shown.tab();
                    let mut repo = shown.repo.and_then(|id| repos.get_mut(id));
                    let context = KeyContext {
                        mode,
                        side_panes,
                        full_screen: layout.full_screen,
                        other_sessions: sessions.position().is_some(),
                        tree: shown.tree.shown,
                        terminal: tab.is_some_and(Tab::takes_suspend),
                        viewer: tab.is_none(),
                        edit: editable(shown, mode.pane()).is_some(),
                        other_tabs: shown.tab_ids().len() > 1,
                        zoomable: layout.zoomable(),
                        agent_alive: tab.is_none_or(|tab| tab.life.alive()),
                        modes: tab.map(|tab| tab.emu.modes()).unwrap_or_default(),
                        focus_events: tab.is_some_and(|tab| tab.emu.focus_events()),
                        help,
                        diff: repo.as_deref().is_some_and(|repo| repo.diff.is_some()),
                        agent_pane: layout.agent_pane(),
                        agent_screen: layout.agent_screen(),
                        changes_pane: layout.changes_pane(),
                        alternate_screen: tab.is_some_and(|tab| tab.emu.alternate_screen()),
                        scrolled: tab.is_some_and(|tab| tab.emu.scrolled() > 0),
                        held,
                    };
                    let action = translate(event, context);
                    // What the picker or the list of sessions chose, and
                    // the session `n` or `N` leads to, acted on once the
                    // shown repository is done with.
                    let mut chosen = None;
                    let mut switch = None;
                    // What `x` or `q` ends, once the shown repository is
                    // done with.
                    let mut ending = None;
                    if let Some(kind) = mouse {
                        held = held.after(kind, &action);
                        // What the agent gets shows once it draws.
                        dirty |= !matches!(action, Action::Nothing | Action::Send(_));
                    }
                    // Any command closes the list; the command still runs.
                    if help && closes_help(&action) {
                        help = false;
                    }
                    match action {
                        Action::Quit => ending = Some(Ending::Quit),
                        Action::Close => {
                            ending = Some(close_ending(sessions.cursor_row(cursor)));
                        }
                        Action::Send(ref bytes) => sessions.shown().write(bytes),
                        Action::Resize(terminal) => {
                            layout.terminal = terminal;
                            if let Some(text) =
                                apply_layout(layout, &mut sessions, &mut side_panes, &mut mode)
                            {
                                toast = Some(trouble(text));
                            }
                        }
                        Action::Zoom => {
                            layout.zoomed = !layout.zoomed;
                            if let Some(text) =
                                apply_layout(layout, &mut sessions, &mut side_panes, &mut mode)
                            {
                                toast = Some(trouble(text));
                            }
                        }
                        Action::Mode(new_mode) => mode = new_mode,
                        Action::Select(movement) => {
                            if let Some(repo) = &mut repo {
                                repo.select(movement, pages.changes);
                            }
                        }
                        // Over the lists the wheel moves the selection; over
                        // an open diff it scrolls the diff, further down.
                        Action::Wheel(Pane::Changes, step) if !context.diff => {
                            if let Some(repo) = &mut repo {
                                repo.select(Movement::Line(step), pages.changes);
                            }
                        }
                        Action::Stage => {
                            if let Some(repo) = &repo {
                                repo.stage();
                            }
                        }
                        Action::Unstage => {
                            if let Some(repo) = &repo {
                                repo.unstage();
                            }
                        }
                        Action::StageAll => {
                            if let Some(repo) = &repo {
                                repo.stage_all();
                            }
                        }
                        Action::Relaunch => {
                            if let Some(text) = sessions.shown_mut().relaunch(
                                agent.as_ref(),
                                layout.agent_size(),
                                &tx,
                            ) {
                                toast = Some(trouble(text));
                            }
                            held = Held::default();
                        }
                        Action::OpenCommit if side_panes => {
                            let shown_repo = repo.as_deref();
                            let staged = can_commit(
                                shown_repo.and_then(|repo| repo.snapshot.as_ref()),
                                side_panes,
                            );
                            let in_flight = shown_repo.is_some_and(|repo| repo.in_flight.is_some());
                            match open_refusal(shown_repo.is_some(), staged, in_flight) {
                                None => mode = Mode::Commit,
                                Some(text) => toast = Some(refusal(text)),
                            }
                        }
                        Action::OpenCommit => {}
                        Action::Help => help = !help,
                        Action::Diff => {
                            if let Some(repo) = &mut repo {
                                repo.toggle_diff();
                            }
                        }
                        Action::Scroll(movement) => {
                            if let Some(repo) = &mut repo {
                                repo.scroll_diff(movement, pages.changes);
                            }
                        }
                        Action::Scrollback(movement) => {
                            let height = usize::from(layout.agent_screen().height);
                            scroll_center(sessions.shown_mut(), movement, height);
                        }
                        Action::Wheel(Pane::Agent, step) => {
                            let height = usize::from(layout.agent_screen().height);
                            for _ in 0..WHEEL_LINES {
                                scroll_center(sessions.shown_mut(), Movement::Line(step), height);
                            }
                        }
                        Action::Wheel(_, step) if context.diff => {
                            if let Some(repo) = &mut repo {
                                repo.scroll = match step {
                                    Step::Down => repo.scroll.saturating_add(WHEEL_LINES),
                                    Step::Up => repo.scroll.saturating_sub(WHEEL_LINES),
                                };
                            }
                        }
                        Action::Wheel(..) => {}
                        Action::OpenPicker => {
                            picker = Some(picker_for(&sessions, agent.as_ref(), home.as_deref()));
                            mode = Mode::Picker;
                            // A choice of a picker closed before opens nothing.
                            roots.forget_waiting();
                        }
                        Action::Type(ref text) => {
                            if let Some(picker) = &mut picker {
                                picker.insert(text);
                            }
                        }
                        Action::Erase => {
                            if let Some(picker) = &mut picker {
                                picker.backspace();
                            }
                        }
                        Action::Choose(step) => {
                            if let Some(picker) = &mut picker {
                                picker.step(step);
                            }
                        }
                        Action::Open { new_conversation } => {
                            if let Some(choice) = picker.as_ref().and_then(Picker::choice) {
                                match open::target(choice, &sessions, home.as_deref()) {
                                    Ok(target) => chosen = Some((target, new_conversation)),
                                    // The picker stays, to change what was
                                    // typed.
                                    Err(error) => toast = Some(trouble(format!("{error:#}"))),
                                }
                            }
                        }
                        Action::Switch(step) => switch = Some(sessions.beside(step)),
                        // Keys go to the terminal once it started.
                        Action::OpenTerminal => {
                            let size = layout.agent_size();
                            let shell = Shell::of_user();
                            let shown = sessions.shown_mut();
                            match shown.open_terminal(&shell, size, &tx, host_focused) {
                                Ok(()) => {
                                    mode = Mode::Agent;
                                    held = Held::default();
                                }
                                Err(error) => toast = Some(trouble(format!("{error:#}"))),
                            }
                        }
                        // Keys go to the editor once it started.
                        Action::OpenEditor => {
                            let shown = sessions.shown_mut();
                            let size = layout.agent_size();
                            let editor = Shell::editor_on;
                            match open_editor(shown, mode.pane(), editor, size, &tx, host_focused) {
                                Ok(true) => {
                                    mode = Mode::Agent;
                                    held = Held::default();
                                }
                                Ok(false) => {}
                                Err(text) => toast = Some(trouble(text)),
                            }
                        }
                        // `,` and `.` keep termis mode, as `n` and `N` do.
                        Action::SwitchTab(step) => {
                            let shown = sessions.shown_mut();
                            let tab = shown.beside_tab(step);
                            if shown.show_tab(tab, host_focused) {
                                held = Held::default();
                            }
                        }
                        Action::Cursor(movement) => {
                            let from = sessions.cursor_row(cursor);
                            cursor = Some(sessions.toward(from, movement, pages.projects));
                        }
                        // Its tab is the one its session shows, and the
                        // session shows as if the picker chose it.
                        Action::Show => {
                            let row = sessions.cursor_row(cursor);
                            sessions.show_tab(row.session, row.tab, host_focused);
                            chosen = Some((Target::Session(row.session), false));
                        }
                        // The list's cursor starts over when the list
                        // comes back.
                        Action::Files => {
                            sessions.shown_mut().tree.toggle();
                            cursor = None;
                        }
                        Action::TreeCursor(movement) => {
                            sessions.shown_mut().tree.step(movement, pages.projects);
                        }
                        Action::TreeOpen => sessions.shown_mut().tree.open(),
                        // The mode stays, so the tree's keys keep browsing.
                        Action::TreeEnter => {
                            if tree_enter(sessions.shown_mut(), &tx, host_focused) {
                                held = Held::default();
                            }
                        }
                        Action::TreeClose => sessions.shown_mut().tree.close(),
                        Action::Edit(ref edit) => {
                            if let Some(repo) = &mut repo {
                                repo.draft.apply(edit);
                            }
                        }
                        Action::Commit => {
                            if let Some(repo) = &mut repo {
                                let message = repo.draft.message();
                                let staged = can_commit(repo.snapshot.as_ref(), side_panes);
                                match (send_refusal(staged, message.is_some()), message) {
                                    (None, Some(message)) => {
                                        repo.commit(message);
                                        mode = Mode::Term(Pane::Changes);
                                    }
                                    (Some(text), _) => toast = Some(refusal(text)),
                                    _ => {}
                                }
                            }
                        }
                        Action::Nothing => {}
                    }
                    // The diff lives only while the changes pane is the one
                    // highlighted, and goes when its lists change.
                    if let Some(repo) = &mut repo
                        && repo.diff.is_some()
                        && closes_diff(&action, mode)
                    {
                        repo.close_diff();
                    }
                    // A choice shows the session open there, else starts one
                    // once the directory's repository is known, which the
                    // finder may have to tell first. The picker closes once
                    // a session is shown; until then what is typed goes to
                    // it.
                    if let Some((target, new_conversation)) = chosen {
                        match target {
                            Target::Session(id) => {
                                open::show(&mut sessions, &mut repos, id, host_focused);
                                opened(Ok(None), &mut mode, &mut held, &mut toast);
                            }
                            Target::Dir(dir) => match roots.lookup(&dir, new_conversation) {
                                Lookup::Found(root) => {
                                    let place = Place::new(dir, root, home.as_deref());
                                    let outcome = open::open(
                                        &starter,
                                        &mut sessions,
                                        &mut repos,
                                        place,
                                        new_conversation,
                                        layout.agent_size(),
                                        host_focused,
                                    );
                                    opened(outcome, &mut mode, &mut held, &mut toast);
                                }
                                Lookup::Ask => finder.ask(dir),
                                Lookup::Waiting => {}
                            },
                        }
                    }
                    // `n` and `N` keep termis mode; no button stays held
                    // over the agent hidden, and the list's cursor starts
                    // over on the session shown.
                    if let Some(id) = switch
                        && open::show(&mut sessions, &mut repos, id, host_focused)
                    {
                        held = Held::default();
                        cursor = None;
                    }
                    // `x` and `q` act at once, or as the second press while
                    // an agent they would stop is working or asking, or a
                    // program runs in a terminal they would close, which
                    // the first one says; a first press counts while that
                    // notice is up. Closing a session or a terminal keeps
                    // the mode, as `n` does, and shows the next session,
                    // or the tab before the terminal, in its place.
                    if let Some(ending) = ending {
                        let now = Instant::now();
                        let first = waiting(armed.take(), toast.as_ref());
                        let asked = confirmation(&sessions, ending);
                        if acts(first.as_ref(), ending, asked.is_some(), now) {
                            // The notice asking for a second press goes too.
                            if first.is_some() {
                                toast = None;
                            }
                            let closed = match ending {
                                _ if quits(ending, sessions.position().is_none()) => {
                                    outcome = Ok(true);
                                    false
                                }
                                Ending::Close(id) => {
                                    open::close(&mut sessions, &mut repos, id, host_focused, now)
                                }
                                Ending::CloseTab(id, tab) => {
                                    sessions.close_tab(id, tab, host_focused, now)
                                }
                                Ending::Quit => false,
                            };
                            if closed {
                                held = Held::default();
                                cursor = None;
                            }
                        } else if let Some(text) = asked {
                            toast = Some(Toast::new(
                                text.clone(),
                                toast::Kind::Error,
                                now,
                                CONFIRM_WITHIN,
                            ));
                            armed = Some(Armed {
                                ending,
                                until: now + CONFIRM_WITHIN,
                                notice: text,
                            });
                        }
                    }
                    // The list's cursor starts on the row of the tab shown
                    // each time the projects pane is highlighted.
                    if mode != Mode::Term(Pane::Projects) {
                        cursor = None;
                    }
                    // The view returns to the live screen when keys go
                    // back to the agent; the wheel and the window's own
                    // events leave it where it is.
                    if mode == Mode::Agent
                        && typed
                        && let Some(tab) = sessions.shown_mut().tab_mut()
                    {
                        tab.emu.scroll_to_live();
                    }
                }
            }
        }
        if mode != Mode::Picker {
            picker = None;
        }
        // A terminal whose shell ended goes, and the tab before it takes
        // its place when it was shown; an editor that failed, as one that
        // could not start, says so, since its error went with its tab.
        let settled = sessions.settle(Instant::now(), host_focused);
        if settled.shown {
            dirty = true;
        }
        if let Some(text) = settled.failed {
            toast = Some(trouble(text));
            dirty = true;
        }
        // The sessions closed move on through their hangup, never
        // waiting; one done lets go of its agent and its directory, and of
        // its repository once no session uses it and no commit runs in it.
        sessions.move_closing(Instant::now());
        repos.release_unused(|id| sessions.uses(id));
        if !matches!(outcome, Ok(false)) {
            break;
        }
        // The session's time moves on its own, by the second, until the
        // agent exits: a frame is due when the text the agent's section
        // shows for it changes.
        let shown = sessions.shown_mut();
        let until = shown
            .agent
            .life
            .exited
            .as_ref()
            .map_or_else(Instant::now, |(_, at)| *at);
        let session_time = shown.clock.time(until);
        let time_text = session_time.map(strings::session_time);
        if time_text != time_shown {
            time_shown = time_text;
            dirty |= side_panes;
        }
        let position = sessions.position();
        let shown = sessions.shown();
        let repo = shown.repo.and_then(|id| repos.get(id));
        // So does the age of the last commit in the changes pane.
        let clock = unix_now();
        let age = ui::changes::last_age(repo.and_then(|repo| repo.snapshot.as_ref()), clock);
        if age != age_shown {
            age_shown = age;
            dirty |= side_panes;
        }
        // So do the plan's limits, as their windows run out.
        let limits = ui::projects::limit_rows(&shown.status, clock);
        if limits != limits_shown {
            limits_shown = limits;
            dirty |= side_panes;
        }
        // So does what the list of sessions, from two sessions on, shows
        // of the others: each one's glyph and dot, and whether a program
        // runs in each terminal, while the list is on screen, how many ask
        // while it is hidden or the file tree is in its place.
        let list_on_screen = side_panes && !shown.tree.shown;
        let rows: Vec<ui::projects::Row<'_>> = if position.is_some() {
            sessions
                .iter()
                .flat_map(|session| {
                    let repo = session.repo.and_then(|id| repos.get(id));
                    let row = session.row(repo.is_some_and(|repo| repo.uncommitted));
                    let tabs = session.tab_rows();
                    std::iter::once(ui::projects::Row::Session(row))
                        .chain(tabs.into_iter().map(ui::projects::Row::Terminal))
                })
                .collect()
        } else {
            Vec::new()
        };
        let list = position.map(|_| {
            let ids = sessions.rows();
            let index = |row| ids.iter().position(|&listed| listed == row);
            let shown = index(sessions.shown_row()).unwrap_or_default();
            ui::projects::List {
                rows: &rows,
                shown,
                cursor: index(sessions.cursor_row(cursor)).unwrap_or(shown),
            }
        });
        let marks = list.as_ref().map(ui::projects::List::marks);
        if marks != marks_shown {
            marks_shown = marks;
            dirty |= list_on_screen;
        }
        let asking = list.as_ref().map_or(0, ui::projects::List::asking);
        if asking != asking_shown {
            asking_shown = asking;
            dirty |= !list_on_screen;
        }
        // Motion with no button held is reported only while the child of
        // the tab shown asks for it; the viewer has none.
        let tab = shown.tab();
        let motion = tab.is_some_and(|tab| {
            tab.life.alive()
                && tab
                    .emu
                    .modes()
                    .mouse
                    .is_some_and(|mouse| mouse.tracking == Tracking::Motion)
        });
        if motion != host_motion {
            host.report_motion(motion)?;
            host_motion = motion;
        }
        if dirty {
            let ended_line = shown.ended_line(agent.name(), position.is_some());
            // The tabs after the agent's, in order, and the one shown
            // among them.
            let names: Vec<&str> = shown.tab_rows().iter().map(|row| row.name).collect();
            let mut later = shown.tab_ids().into_iter().skip(1);
            // The changed files under the session's directory, for the
            // letters of the file tree.
            let tree_changes = repo
                .and_then(|repo| repo.snapshot.as_ref())
                .filter(|_| shown.tree.shown)
                .map(|snapshot| ui::tree::Changes::new(&snapshot.status, shown.place.below_root()))
                .unwrap_or_default();
            let tree = shown.tree.shown.then(|| ui::tree::Pane {
                heading: &shown.place.project,
                content: shown
                    .tree
                    .content
                    .as_ref()
                    .map(|content| content.as_ref().map_err(String::as_str)),
                folds: &shown.tree.folds,
                changes: &tree_changes,
            });
            let view = ui::View {
                agent: agent.name(),
                project: &shown.place.project,
                terminals: &names,
                terminal: later.position(|id| id == shown.shown_tab()),
                position,
                center: shown.center(),
                mode,
                side_panes,
                zoomed: layout.zoomed,
                ended: ended_line.as_deref(),
                repo: repo.is_some(),
                changes: repo.and_then(|repo| repo.snapshot.as_ref()),
                selected: repo.and_then(|repo| repo.selected.as_ref()),
                scroll: repo.map_or(0, |repo| repo.scroll),
                draft: repo
                    .filter(|_| mode == Mode::Commit)
                    .map(|repo| &repo.draft),
                toast: toast.as_ref(),
                help,
                diff: repo.and_then(|repo| repo.diff.as_ref()),
                card: shown.card(agent.name(), clock, session_time),
                list,
                tree,
                picker: picker.as_ref(),
                edit: editable(shown, mode.pane()).is_some(),
            };
            let scrolls = draw(&mut host, layout.full_screen, &view)?;
            pages = scrolls.pages;
            let shown = sessions.shown_mut();
            if shown.tree.shown {
                shown.tree.folds.scroll = scrolls.tree;
            }
            if let Some(repo) = shown.repo.and_then(|id| repos.get_mut(id)) {
                repo.scroll = scrolls.changes;
            }
            dirty = false;
        }
    }

    sessions.quit();
    drop(reader);
    drop(finder);
    let status = sessions.close_all(&rx);
    Host::forget_instance();
    drop(host);
    outcome.map(|_| status)
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
    /// Whether the agent is drawn over the whole terminal, where `o`
    /// opens nothing.
    full_screen: bool,
    /// Whether there are other sessions for `n` and `N` to show, and for
    /// `q` to close an ended agent's session rather than quit.
    other_sessions: bool,
    /// Whether the projects pane shows the file tree of the session
    /// shown, which its keys then move through, rather than the list.
    tree: bool,
    /// Whether the tab shown is a terminal that `Ctrl+Z` reaches: one
    /// running the user's shell, not the editor.
    terminal: bool,
    /// Whether the tab shown is the viewer, which has no child to send
    /// keys to: the scroll keys move it in agent mode too.
    viewer: bool,
    /// Whether `e` has a file to open in the editor, as [`editable`]
    /// finds one with the mode's pane highlighted.
    edit: bool,
    /// Whether the session shown has other tabs for `,` and `.` to show.
    other_tabs: bool,
    /// Whether `z` changes something on screen: side panes to hide, or
    /// hidden ones the width allows back.
    zoomable: bool,
    /// Whether the child of the tab shown is still running; the viewer,
    /// which has none, counts as running. A terminal's tab goes when its
    /// shell ends, so only the agent's is shown ended: then `r` relaunches
    /// the agent, and in agent mode `x` and `q` close its session,
    /// quitting on the last.
    agent_alive: bool,
    /// Modes the child set, for the key encoder.
    modes: Modes,
    /// Whether the child asked for focus events.
    focus_events: bool,
    /// Whether the key list is open, so that `Esc` closes it first and
    /// `i` does nothing.
    help: bool,
    /// Whether the diff view is open, so that the movement keys scroll it
    /// and `Esc` closes it.
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
    /// Whether the agent pane shows lines that scrolled off, where the
    /// cells under the pointer are not the agent's.
    scrolled: bool,
    /// The buttons pressed over the agent that are still held.
    held: Held,
}

/// Turns one host event into an action.
///
/// `Ctrl+Q` switches between agent mode and termis mode, and leaves the
/// commit box and the picker for agent mode. In agent mode keys and
/// pastes go to the child of the tab shown, except `Ctrl+Z` on the
/// agent's tab, which would make the agent try to suspend itself and wait
/// forever, and on the editor's, which `sh -c` runs without job control
/// to resume it; once the agent ended, `r` relaunches it, `x` closes its
/// session, and so does `q` while there are other sessions, else it
/// quits. On the viewer, which has no child, agent mode keeps the scroll
/// keys, see [`view_key`], and drops the other keys and pastes. In
/// termis mode keys are commands, see [`term_command`], and pastes are
/// dropped. With the commit box open keys and pastes edit the draft, see
/// [`commit_key`]; with the picker open, keys and the first line of a
/// paste its query, see [`picker_key`]. Host focus events describe the
/// window, so they reach the child shown in every mode. A child that
/// asked for the mouse gets, in every mode, the reports it asked for of
/// what happens over the center pane, presses and motion only while the
/// pane shows its live screen, and the drags and release of a button
/// pressed there wherever they happen. Otherwise the wheel over the
/// center pane becomes arrow keys while the child is on its alternate
/// screen in agent mode, else it moves the view of the tab shown. Over
/// the changes pane the wheel moves the selection or the diff. Under the
/// key list or the picker the mouse only finishes a drag the child saw
/// begin. Other mouse events, key releases and repeats are ignored.
fn translate(event: HostEvent, context: KeyContext) -> Action {
    let KeyContext {
        mode,
        terminal,
        viewer,
        agent_alive,
        modes,
        focus_events,
        help,
        agent_pane,
        agent_screen,
        changes_pane,
        alternate_screen,
        scrolled,
        held,
        ..
    } = context;
    let mut bytes = Vec::new();
    match event {
        HostEvent::Key(key) if key.kind != KeyEventKind::Press => {}
        HostEvent::Key(key) if is_mode_key(&key) => {
            return Action::Mode(match mode {
                Mode::Agent => Mode::Term(Pane::Agent),
                Mode::Term(_) | Mode::Commit | Mode::Picker => Mode::Agent,
            });
        }
        HostEvent::Key(key) if is_suspend(&key) && !(terminal && mode == Mode::Agent) => {}
        HostEvent::Key(key) => match mode {
            Mode::Commit => return commit_key(&key),
            Mode::Picker => return picker_key(&key),
            Mode::Term(pane) => return term_command(&key, pane, &context),
            Mode::Agent if viewer => return view_key(&key),
            Mode::Agent if agent_alive => {
                let _ = keys::encode(&key, modes, &mut bytes);
            }
            Mode::Agent => match key.code {
                KeyCode::Char('r') if key.modifiers.is_empty() => return Action::Relaunch,
                KeyCode::Char('x') if key.modifiers.is_empty() => return Action::Close,
                KeyCode::Char('q') if key.modifiers.is_empty() && context.other_sessions => {
                    return Action::Close;
                }
                KeyCode::Char('q') if key.modifiers.is_empty() => return Action::Quit,
                _ => {}
            },
        },
        HostEvent::Paste(text) if mode == Mode::Agent && agent_alive && !viewer => {
            keys::encode_paste(&text, modes, &mut bytes);
        }
        HostEvent::Paste(text) if mode == Mode::Commit => {
            return Action::Edit(Edit::Insert(pasted(&text)));
        }
        HostEvent::Paste(text) if mode == Mode::Picker => {
            let line = text.split(['\r', '\n']).next().unwrap_or_default();
            if !line.is_empty() {
                return Action::Type(line.to_owned());
            }
        }
        HostEvent::Paste(_) => {}
        HostEvent::Resize(cols, rows) => return Action::Resize(Size { rows, cols }),
        HostEvent::FocusGained if focus_events => bytes.extend_from_slice(b"\x1b[I"),
        HostEvent::FocusLost if focus_events => bytes.extend_from_slice(b"\x1b[O"),
        HostEvent::Mouse(mouse)
            if (help || mode == Mode::Picker)
                && !matches!(
                    mouse.kind,
                    MouseEventKind::Drag(button) | MouseEventKind::Up(button) if held.has(button)
                ) => {}
        HostEvent::Mouse(mouse) => {
            let at = Position::new(mouse.column, mouse.row);
            let over_agent = agent_pane.contains(at);
            let wheel = match mouse.kind {
                MouseEventKind::ScrollUp => Some(Step::Up),
                MouseEventKind::ScrollDown => Some(Step::Down),
                _ => None,
            };
            let reported = match mouse.kind {
                MouseEventKind::Drag(button) | MouseEventKind::Up(button) => held.has(button),
                MouseEventKind::Down(_) | MouseEventKind::Moved => over_agent && !scrolled,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => over_agent,
                MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => false,
            };
            match (modes.mouse, wheel) {
                // At the cell under the pointer on the agent's screen; a
                // border, or a place past the pane, counts as the nearest
                // cell.
                (Some(asked), _)
                    if agent_alive && reported && asked.tracking.reports(mouse.kind) =>
                {
                    let last = |size: u16| size.saturating_sub(1);
                    let column =
                        at.x.saturating_sub(agent_screen.x)
                            .min(last(agent_screen.width));
                    let row =
                        at.y.saturating_sub(agent_screen.y)
                            .min(last(agent_screen.height));
                    keys::encode_mouse(
                        mouse.kind,
                        column,
                        row,
                        mouse.modifiers,
                        asked.encoding,
                        &mut bytes,
                    );
                }
                // The alternate screen keeps no history: the agent gets
                // the arrow keys a terminal would turn the wheel into.
                (None, Some(step))
                    if over_agent && mode == Mode::Agent && agent_alive && alternate_screen =>
                {
                    let code = match step {
                        Step::Up => KeyCode::Up,
                        Step::Down => KeyCode::Down,
                    };
                    let key = KeyEvent::new(code, KeyModifiers::NONE);
                    for _ in 0..WHEEL_LINES {
                        let _ = keys::encode(&key, modes, &mut bytes);
                    }
                }
                (_, Some(step)) if over_agent => return Action::Wheel(Pane::Agent, step),
                (_, Some(step)) if changes_pane.is_some_and(|pane| pane.contains(at)) => {
                    return Action::Wheel(Pane::Changes, step);
                }
                _ => {}
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
/// mode stays until `Esc` returns to the agent. The movement keys, see
/// [`movement`], move through the highlighted pane's list or view, a
/// page being what it shows. With the projects pane highlighted and its
/// file tree shown, they move the tree's cursor, `l` or `Enter` unfold
/// the directory under it, `Enter` shows the file under it in the
/// viewer, and `h` folds it, or goes to the directory the entry is in;
/// with the list shown and other sessions, they move the list's cursor
/// and `Enter` shows the tab under it. With the changes pane
/// highlighted, they move its selection, `s` and `u` stage and unstage
/// the selected row, `a` stages everything. `c` opens the commit box,
/// `d` the diff of the selected row, which they and `PgUp`/`PgDn`,
/// shifted or not, then scroll and `d` or `Esc` close. With the agent
/// pane highlighted, they and `PgUp`/`PgDn` move its view through its
/// scrollback, `G` back to the live screen and `g` to the oldest line
/// kept; the alternate screen has no history to move through. On the
/// viewer they move through its file the same way, `G` to its end and
/// `g` to its top. `z` toggles the zoom when that changes anything;
/// unless the agent is drawn over the whole terminal, `o` opens the
/// project picker, `t` a terminal and `e`, while it has one, a file in
/// the user's editor; while the side panes are shown, `f` shows the file
/// tree in the projects pane in place of the list of sessions and the
/// card, or takes it away; `n` and `N`, shifted or not, show the next
/// and the previous session while there are others, `,` and `.` the
/// previous and the next tab while the session has others, `r`
/// relaunches an ended agent shown, `x` closes the tab shown, the
/// agent's with its session, or the one under the list's cursor with the
/// projects pane highlighted, `q` quits, `?` shows or hides the key list,
/// which `Esc` closes first while it is open. `i` returns to the agent
/// as `Ctrl+Q` does, the diff closing with the mode, except while the
/// key list is open. Anything else is ignored.
fn term_command(key: &KeyEvent, pane: Pane, context: &KeyContext) -> Action {
    let KeyContext {
        side_panes,
        full_screen,
        other_sessions,
        tree,
        edit,
        other_tabs,
        zoomable,
        agent_alive,
        help,
        diff,
        ..
    } = *context;
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let moved = if diff {
        movement(key, true).map(Action::Scroll)
    } else {
        match pane {
            Pane::Agent => movement(key, true).map(Action::Scrollback),
            Pane::Changes => movement(key, false).map(Action::Select),
            Pane::Projects if tree => movement(key, false).map(Action::TreeCursor),
            Pane::Projects if other_sessions => movement(key, false).map(Action::Cursor),
            Pane::Projects => None,
        }
    };
    if let Some(action) = moved {
        return action;
    }
    match key.code {
        KeyCode::Char('?') if plain || shifted => Action::Help,
        KeyCode::Esc if plain && help => Action::Help,
        KeyCode::Esc if plain && diff => Action::Diff,
        KeyCode::Esc if plain => Action::Mode(Mode::Agent),
        KeyCode::Char('i') if plain && !help => Action::Mode(Mode::Agent),
        KeyCode::Char('1') if plain && side_panes => Action::Mode(Mode::Term(Pane::Projects)),
        KeyCode::Char('2') if plain => Action::Mode(Mode::Term(Pane::Agent)),
        KeyCode::Char('3') if plain && side_panes => Action::Mode(Mode::Term(Pane::Changes)),
        KeyCode::Tab if plain && side_panes => Action::Mode(Mode::Term(pane.next())),
        KeyCode::BackTab if side_panes => Action::Mode(Mode::Term(pane.previous())),
        KeyCode::Char('l') if plain && pane == Pane::Projects && tree => Action::TreeOpen,
        KeyCode::Enter if plain && pane == Pane::Projects && tree => Action::TreeEnter,
        KeyCode::Char('h') if plain && pane == Pane::Projects && tree => Action::TreeClose,
        KeyCode::Enter if plain && pane == Pane::Projects && other_sessions => Action::Show,
        KeyCode::Char('s') if plain && pane == Pane::Changes => Action::Stage,
        KeyCode::Char('u') if plain && pane == Pane::Changes => Action::Unstage,
        KeyCode::Char('a') if plain && pane == Pane::Changes => Action::StageAll,
        KeyCode::Char('c') if plain => Action::OpenCommit,
        KeyCode::Char('d') if plain && pane == Pane::Changes => Action::Diff,
        KeyCode::Char('z') if plain && zoomable => Action::Zoom,
        KeyCode::Char('o') if plain && !full_screen => Action::OpenPicker,
        KeyCode::Char('t') if plain && !full_screen => Action::OpenTerminal,
        KeyCode::Char('e') if plain && edit && !full_screen => Action::OpenEditor,
        KeyCode::Char('f') if plain && side_panes => Action::Files,
        KeyCode::Char(',') if plain && other_tabs => Action::SwitchTab(Step::Up),
        KeyCode::Char('.') if plain && other_tabs => Action::SwitchTab(Step::Down),
        KeyCode::Char('n') if plain && other_sessions => Action::Switch(Step::Down),
        KeyCode::Char('N') if (plain || shifted) && other_sessions => Action::Switch(Step::Up),
        KeyCode::Char('n') if shifted && other_sessions => Action::Switch(Step::Up),
        KeyCode::Char('r') if plain && !agent_alive => Action::Relaunch,
        KeyCode::Char('x') if plain => Action::Close,
        KeyCode::Char('q') if plain => Action::Quit,
        _ => Action::Nothing,
    }
}

/// The movement `key` makes through a list or a view in termis mode, and
/// on the viewer in agent mode: `j`/`k` and the arrows a row or a line,
/// `Ctrl+D`/`Ctrl+U` half of what shows and `Ctrl+F`/`Ctrl+B` all of it,
/// as `PgDn`/`PgUp` do, shifted or not, where `page_keys`; `g` to the top
/// and `G`, shifted or not, to the end. None for any other key.
fn movement(key: &KeyEvent, page_keys: bool) -> Option<Movement> {
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let control = key.modifiers == KeyModifiers::CONTROL;
    let movement = match key.code {
        KeyCode::Char('j') | KeyCode::Down if plain => Movement::Line(Step::Down),
        KeyCode::Char('k') | KeyCode::Up if plain => Movement::Line(Step::Up),
        KeyCode::Char('d') if control => Movement::Half(Step::Down),
        KeyCode::Char('u') if control => Movement::Half(Step::Up),
        KeyCode::Char('f') if control => Movement::Page(Step::Down),
        KeyCode::Char('b') if control => Movement::Page(Step::Up),
        KeyCode::PageDown if page_keys && (plain || shifted) => Movement::Page(Step::Down),
        KeyCode::PageUp if page_keys && (plain || shifted) => Movement::Page(Step::Up),
        KeyCode::Char('G') if plain || shifted => Movement::End,
        // A host that reports the shift and the base letter.
        KeyCode::Char('g') if shifted => Movement::End,
        KeyCode::Char('g') if plain => Movement::Top,
        _ => return None,
    };
    Some(movement)
}

/// The key `key` in agent mode on the viewer, which has no child to take
/// it: the movement keys of the agent pane in termis mode, see
/// [`movement`]. Anything else is ignored.
fn view_key(key: &KeyEvent) -> Action {
    movement(key, true).map_or(Action::Nothing, Action::Scrollback)
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

/// The picker key `key`: text goes into the query, `Backspace` or
/// `Ctrl+H` take a grapheme back, `Up` and `Down` choose a row, `Enter`
/// opens the choice, continuing the project's last conversation when
/// there is one, and `Tab` opens it on a new conversation; on an open
/// session both show it. `Esc` closes the picker for termis mode.
/// Anything else is ignored.
fn picker_key(key: &KeyEvent) -> Action {
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let control = key.modifiers == KeyModifiers::CONTROL;
    match key.code {
        KeyCode::Esc if plain => Action::Mode(Mode::Term(Pane::Agent)),
        KeyCode::Enter if plain => Action::Open {
            new_conversation: false,
        },
        KeyCode::Tab if plain => Action::Open {
            new_conversation: true,
        },
        KeyCode::Up if plain => Action::Choose(Step::Up),
        KeyCode::Down if plain => Action::Choose(Step::Down),
        KeyCode::Backspace if plain => Action::Erase,
        KeyCode::Char('h') if control => Action::Erase,
        KeyCode::Char(ch) if plain || shifted => Action::Type(ch.to_string()),
        _ => Action::Nothing,
    }
}

/// The picker over the sessions, in the order they were opened, and the
/// projects `agent` ran in lately, with `home` shown as `~`.
fn picker_for(sessions: &Sessions, agent: &dyn agent::Agent, home: Option<&Path>) -> Picker {
    let open = sessions
        .iter()
        .map(|session| {
            let place = &session.place;
            Item::open(place.project.clone(), place.cwd.clone(), home)
        })
        .collect();
    let recent = agent
        .recent_projects()
        .into_iter()
        .map(|dir| Item::recent(session::project_name(&dir), dir, home))
        .collect();
    Picker::new(open, recent)
}

/// Applies what opening a session from the picker, or showing one from
/// the list of sessions, gave: on success, agent mode on the session
/// shown, which closes the picker, with no mouse button held over it; on
/// failure the mode stays. A notice goes to the status bar either way.
fn opened(
    outcome: Result<Option<String>, String>,
    mode: &mut Mode,
    held: &mut Held,
    toast: &mut Option<Toast>,
) {
    let notice = match outcome {
        Ok(notice) => {
            *mode = Mode::Agent;
            *held = Held::default();
            notice
        }
        Err(text) => Some(text),
    };
    if let Some(text) = notice {
        *toast = Some(trouble(text));
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

/// Whether the diff view closes after `action` left the app in `mode`:
/// it lives only while the changes pane is the one highlighted, so a
/// mode change, the commit box, a zoom or a narrow terminal end it, and
/// so do a change to the lists and another session shown.
fn closes_diff(action: &Action, mode: Mode) -> bool {
    mode != Mode::Term(Pane::Changes)
        || matches!(
            action,
            Action::Stage | Action::Unstage | Action::StageAll | Action::Switch(_)
        )
}

/// Whether `msg` may change the screen while tab `tab` of session `shown`
/// is on it. A refresh, a walk or a read does only when it changed
/// something on screen, which applying it tells; a mouse event only when
/// it changes something here; a pty event only when it comes from the tab
/// shown. The finder's answer may open a session or bring a notice.
fn dirties(msg: &Msg, shown: SessionId, tab: TabId) -> bool {
    match msg {
        Msg::Pty(key, _) => key.session == shown && key.tab == tab,
        Msg::Git(..)
        | Msg::Walked(_)
        | Msg::Read(..)
        | Msg::Input(input::Event::Host(HostEvent::Mouse(_))) => false,
        Msg::Input(_) | Msg::Found(_) => true,
    }
}

/// A notice refusing what the user asked, shown from now.
fn refusal(text: &str) -> Toast {
    trouble(text.to_owned())
}

/// A notice that something went wrong, shown from now.
fn trouble(text: String) -> Toast {
    Toast::new(text, toast::Kind::Error, Instant::now(), TOAST_LASTS)
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

/// Seconds since the Unix epoch; zero when the clock is before it.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Draws one frame. Returns where the changes pane and the file tree
/// start on the next one, and how many rows of them showed.
fn draw(host: &mut Host, full_screen: bool, view: &ui::View<'_>) -> Result<ui::Scrolls> {
    let mut scrolls = view.scrolls();
    host.terminal
        .draw(|frame| {
            if full_screen {
                ui::render_full(
                    frame,
                    view.center,
                    view.ended,
                    view.mode,
                    view.help,
                    view.toast,
                );
            } else {
                scrolls = ui::render(frame, view);
            }
        })
        .context("draw")?;
    Ok(scrolls)
}

/// The file, absolute, that `e` opens in the editor in `session` with
/// `pane` highlighted in termis mode: with the projects pane highlighted
/// and the file tree shown, the file under the tree's cursor, none on a
/// directory; else the file the viewer shows while its tab is on screen.
fn editable(session: &Session, pane: Pane) -> Option<PathBuf> {
    if pane == Pane::Projects && session.tree.shown {
        return session.tree.file().map(|file| session.place.cwd.join(file));
    }
    session.viewing().map(|viewer| viewer.path.clone())
}

/// Starts the editor `editor` gives for the file [`editable`] finds in
/// `session` with `pane` highlighted, none when the user has no editor,
/// in a terminal of `size` whose events reach `tx`, shown as
/// [`Session::open_terminal`] shows it with the host `focused` or not.
/// Returns whether it started, nothing happening without a file, or the
/// notice saying why not.
fn open_editor(
    session: &mut Session,
    pane: Pane,
    editor: impl FnOnce(&Path) -> Option<Shell>,
    size: Size,
    tx: &mpsc::Sender<Msg>,
    focused: bool,
) -> Result<bool, String> {
    let Some(file) = editable(session, pane) else {
        return Ok(false);
    };
    let editor = editor(&file).ok_or_else(|| strings::NO_EDITOR.to_owned())?;
    session
        .open_terminal(&editor, size, tx, focused)
        .map_err(|error| format!("{error:#}"))?;
    Ok(true)
}

/// Unfolds the directory under the cursor of the file tree of `session`,
/// or shows the file under it in the viewer, as [`Session::view`] does
/// with the read answering on `tx` and the host `focused` or not.
/// Returns whether the tab shown changed.
fn tree_enter(session: &mut Session, tx: &mpsc::Sender<Msg>, focused: bool) -> bool {
    match session.tree.file() {
        Some(file) => {
            let path = session.place.cwd.join(file);
            session.view(path, tx, focused)
        }
        None => {
            session.tree.open();
            false
        }
    }
}

/// Moves the view of the tab `session` shows by `movement`: the viewer's
/// through its file, in a pane `height` lines tall, else the screen's
/// through its scrollback, a page being as tall as the screen, from the
/// oldest line the history keeps to the live screen.
fn scroll_center(session: &mut Session, movement: Movement, height: usize) {
    if let Some(viewer) = session.viewing_mut() {
        viewer.scroll(movement, height);
    } else if let Some(tab) = session.tab_mut() {
        let emu = &mut tab.emu;
        let page = usize::from(emu.size().rows);
        match movement.by(page) {
            Some((Step::Up, lines)) => emu.scroll_back(lines),
            Some((Step::Down, lines)) => emu.scroll_forward(lines),
            None if movement == Movement::Top => emu.scroll_back(usize::MAX),
            None => emu.scroll_to_live(),
        }
    }
}

/// Asks for the files of the directory of the session shown when its
/// tree is `on_screen` and due at `now`: its repository's worker lists
/// them, or outside a repository a walk of the directory, whose answer
/// reaches `tx`.
fn ask_files(
    sessions: &mut Sessions,
    repos: &Repos,
    tx: &mpsc::Sender<Msg>,
    now: Instant,
    on_screen: bool,
) {
    let shown = sessions.shown_mut();
    if !shown.tree.due(now, on_screen) {
        return;
    }
    match shown.repo {
        Some(id) => {
            // A session's repository stays while the session uses it.
            let Some(repo) = repos.get(id) else {
                return;
            };
            repo.list_files(shown.place.below_root());
            shown.tree.sent();
        }
        None => {
            shown.tree.sent();
            if let Err(error) = walk::spawn(shown.place.cwd.clone(), tx.clone(), Msg::Walked) {
                shown.tree.failed(format!("{error:#}"), now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::Change;
    use crate::instance::Instance;
    use crate::keys::{KittyFlags, Protocol};
    use session::tests::{Fake, NO_TAB};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

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
            full_screen: false,
            other_sessions: false,
            tree: false,
            terminal: false,
            viewer: false,
            edit: false,
            other_tabs: false,
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
            scrolled: false,
            held: Held::default(),
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
            mouse: Some(keys::Mouse {
                tracking: Tracking::Motion,
                encoding: keys::MouseEncoding::Sgr,
            }),
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
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Buttons,
                    encoding: keys::MouseEncoding::X10,
                }),
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

    #[test]
    fn an_agent_that_asked_for_the_mouse_gets_clicks_drags_and_motion() {
        let tracking = |tracking| KeyContext {
            alternate_screen: true,
            ..agent(
                Modes {
                    mouse: Some(keys::Mouse {
                        tracking,
                        encoding: keys::MouseEncoding::Sgr,
                    }),
                    ..Modes::default()
                },
                false,
            )
        };
        let fullscreen = tracking(Tracking::Motion);
        let left = MouseButton::Left;
        let send = |bytes: &[u8]| Action::Send(bytes.to_vec());
        // A press over the center pane reaches the agent, in every mode.
        for mode in [Mode::Agent, Mode::Term(Pane::Changes), Mode::Commit] {
            let ctx = KeyContext { mode, ..fullscreen };
            assert_eq!(
                translate(wheel(MouseEventKind::Down(left), 30, 5), ctx),
                send(b"\x1b[<0;10;5M")
            );
        }
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 30, 5), fullscreen),
            send(b"\x1b[<35;10;5M")
        );
        // Not over the side panes, not once the agent ended, not under the
        // key list.
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 80, 5), fullscreen),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 5, 5), fullscreen),
            Action::Nothing
        );
        for ctx in [
            KeyContext {
                agent_alive: false,
                ..fullscreen
            },
            KeyContext {
                help: true,
                mode: Mode::Term(Pane::Agent),
                ..fullscreen
            },
        ] {
            assert_eq!(
                translate(wheel(MouseEventKind::Down(left), 30, 5), ctx),
                Action::Nothing
            );
        }
        // Drags and the release go only with a button pressed over the
        // agent, and then also past the pane, at the nearest cell.
        assert_eq!(
            translate(wheel(MouseEventKind::Drag(left), 30, 5), fullscreen),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 30, 5), fullscreen),
            Action::Nothing
        );
        let dragging = KeyContext {
            held: Held::default().after(MouseEventKind::Down(left), &send(b"")),
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Drag(left), 40, 6), dragging),
            send(b"\x1b[<32;20;6M")
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Drag(left), 90, 39), dragging),
            send(b"\x1b[<32;53;37M")
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 2, 3), dragging),
            send(b"\x1b[<0;1;3m")
        );
        assert_eq!(
            translate(
                wheel(MouseEventKind::Drag(MouseButton::Right), 40, 6),
                dragging
            ),
            Action::Nothing,
            "another button"
        );
        // Only what the agent's tracking asked for.
        let buttons = KeyContext {
            held: dragging.held,
            ..tracking(Tracking::Buttons)
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Drag(left), 40, 6), buttons),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 30, 5), buttons),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 40, 6), buttons),
            send(b"\x1b[<0;20;6m")
        );
        let drags = tracking(Tracking::Drags);
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 30, 5), drags),
            Action::Nothing
        );
        // While the pane shows history the cells are not the agent's: no
        // presses or motion, but the wheel and a held button's release.
        let history = KeyContext {
            scrolled: true,
            ..dragging
        };
        for kind in [MouseEventKind::Down(left), MouseEventKind::Moved] {
            assert_eq!(translate(wheel(kind, 30, 5), history), Action::Nothing);
        }
        assert_eq!(
            translate(wheel(MouseEventKind::ScrollUp, 30, 5), history),
            send(b"\x1b[<64;10;5M")
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 30, 5), history),
            send(b"\x1b[<0;10;5m")
        );
        // Under the key list a drag the agent saw begin still ends there.
        let listing = KeyContext {
            help: true,
            mode: Mode::Term(Pane::Agent),
            ..dragging
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 30, 5), listing),
            send(b"\x1b[<0;10;5m")
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 30, 5), listing),
            Action::Nothing
        );
        // Sideways scrolling has no report here.
        assert_eq!(
            translate(wheel(MouseEventKind::ScrollLeft, 30, 5), fullscreen),
            Action::Nothing
        );
        // A press the encoding cannot carry is not sent, and its button is
        // not held.
        let far = KeyContext {
            agent_pane: Rect::new(0, 0, 300, 39),
            agent_screen: Rect::new(1, 1, 298, 37),
            modes: Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Motion,
                    encoding: keys::MouseEncoding::X10,
                }),
                ..Modes::default()
            },
            ..fullscreen
        };
        let press = MouseEventKind::Down(left);
        let action = translate(wheel(press, 250, 5), far);
        assert_eq!(action, Action::Nothing);
        assert!(!Held::default().after(press, &action).has(left));
        // Without the mouse the agent gets none of it.
        let plain = agent(Modes::default(), false);
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 30, 5), plain),
            Action::Nothing
        );
    }

    #[test]
    fn a_button_is_held_from_a_press_the_agent_got_to_its_release() {
        let left = MouseButton::Left;
        let right = MouseButton::Right;
        let sent = Action::Send(b"x".to_vec());
        let none = Held::default();
        let held = none.after(MouseEventKind::Down(left), &sent);
        assert!(held.has(left) && !held.has(right));
        let both = held.after(MouseEventKind::Down(right), &sent);
        assert!(both.has(left) && both.has(right));
        assert_eq!(both.after(MouseEventKind::Drag(left), &sent), both);
        assert_eq!(both.after(MouseEventKind::Moved, &Action::Nothing), both);
        assert_eq!(both.after(MouseEventKind::Up(right), &sent), held);
        assert_eq!(held.after(MouseEventKind::Up(left), &Action::Nothing), none);
        assert_eq!(
            held.after(MouseEventKind::Down(left), &Action::Nothing),
            none,
            "a press the agent did not get"
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
    fn i_goes_back_to_agent_mode_as_ctrl_q_does_but_not_under_the_key_list() {
        let i = || plain(KeyCode::Char('i'));
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            for other_sessions in [false, true] {
                let ctx = KeyContext {
                    other_sessions,
                    ..termis(pane)
                };
                assert_eq!(translate(i(), ctx), Action::Mode(Mode::Agent));
                assert_eq!(translate(i(), ctx), translate(key(CTRL_Q), ctx));
            }
        }
        // With the tree or an ended agent, too.
        for ctx in [
            KeyContext {
                tree: true,
                ..termis(Pane::Projects)
            },
            KeyContext {
                agent_alive: false,
                ..termis(Pane::Agent)
            },
        ] {
            assert_eq!(translate(i(), ctx), Action::Mode(Mode::Agent));
        }
        // On the viewer that is agent mode on the viewer, where `i` is
        // dropped.
        let viewing = KeyContext {
            viewer: true,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(i(), viewing), Action::Mode(Mode::Agent));
        let on_viewer = KeyContext {
            mode: Mode::Agent,
            ..viewing
        };
        assert_eq!(translate(i(), on_viewer), Action::Nothing);
        // From the diff it goes as `Ctrl+Q` does, and the diff with the
        // mode; `Esc` there closes the diff alone.
        let diff = KeyContext {
            diff: true,
            ..termis(Pane::Changes)
        };
        assert_eq!(translate(i(), diff), translate(key(CTRL_Q), diff));
        assert!(closes_diff(&Action::Mode(Mode::Agent), Mode::Agent));
        assert_eq!(translate(plain(KeyCode::Esc), diff), Action::Diff);
        // Under the key list it does nothing, and the list stays.
        let listing = KeyContext {
            help: true,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(i(), listing), Action::Nothing);
        assert!(!closes_help(&Action::Nothing));
        // Shifted, or with another modifier, it does nothing; `Ctrl+I`
        // as hosts send it, `Tab`, highlights the next pane.
        for modifiers in [
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ] {
            for ch in ['i', 'I'] {
                let event = key(KeyEvent::new(KeyCode::Char(ch), modifiers));
                assert_eq!(
                    translate(event, termis(Pane::Agent)),
                    Action::Nothing,
                    "{modifiers:?} {ch}"
                );
            }
        }
        assert_eq!(
            translate(plain(KeyCode::Char('I')), termis(Pane::Agent)),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Tab), termis(Pane::Agent)),
            Action::Mode(Mode::Term(Pane::Changes))
        );
        // In agent mode the child gets it; the boxes take it as text.
        let ctx = agent(Modes::default(), false);
        assert_eq!(translate(i(), ctx), Action::Send(b"i".to_vec()));
        let shell = KeyContext {
            terminal: true,
            ..ctx
        };
        assert_eq!(translate(i(), shell), Action::Send(b"i".to_vec()));
        let boxed = KeyContext {
            mode: Mode::Commit,
            ..ctx
        };
        assert_eq!(
            translate(i(), boxed),
            Action::Edit(Edit::Insert("i".into()))
        );
        let picking = KeyContext {
            mode: Mode::Picker,
            ..ctx
        };
        assert_eq!(translate(i(), picking), Action::Type("i".into()));
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
        // x closes a session, alone or not, with any pane highlighted.
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            for other_sessions in [false, true] {
                let ctx = KeyContext {
                    other_sessions,
                    ..termis(pane)
                };
                assert_eq!(translate(plain(KeyCode::Char('x')), ctx), Action::Close);
            }
        }
        let shift_x = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_x), ctx), Action::Nothing);
        assert!(closes_help(&Action::Close), "a command");
        for event in [plain(KeyCode::Enter), HostEvent::Paste("secret".into())] {
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
    fn after_the_agent_ended_r_relaunches_and_x_and_q_close_or_quit() {
        let gone = KeyContext {
            agent_alive: false,
            ..agent(Modes::default(), false)
        };
        assert_eq!(translate(plain(KeyCode::Char('r')), gone), Action::Relaunch);
        // Alone, q quits; x closes the last session, which quits too.
        assert_eq!(translate(plain(KeyCode::Char('q')), gone), Action::Quit);
        assert_eq!(translate(plain(KeyCode::Char('x')), gone), Action::Close);
        // With other sessions, both close this one.
        let others = KeyContext {
            other_sessions: true,
            ..gone
        };
        assert_eq!(translate(plain(KeyCode::Char('q')), others), Action::Close);
        assert_eq!(translate(plain(KeyCode::Char('x')), others), Action::Close);
        // While the agent runs they are its keys.
        let alive = KeyContext {
            agent_alive: true,
            ..others
        };
        for ch in ['x', 'q'] {
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), alive),
                Action::Send(ch.to_string().into_bytes())
            );
        }
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_x), others), Action::Nothing);
        for event in [plain(KeyCode::Enter), HostEvent::Paste("text".into())] {
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
        // In termis mode q still quits while other sessions remain.
        let others_term = KeyContext {
            other_sessions: true,
            ..gone_term
        };
        assert_eq!(
            translate(plain(KeyCode::Char('q')), others_term),
            Action::Quit
        );
        assert_eq!(
            translate(plain(KeyCode::Char('x')), others_term),
            Action::Close
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), gone_term),
            Action::Mode(Mode::Agent)
        );
    }

    #[test]
    fn ctrl_z_reaches_a_terminal_and_never_the_agent() {
        let ctx = agent(Modes::default(), false);
        let shell = KeyContext {
            terminal: true,
            ..ctx
        };
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            for code in [KeyCode::Char('z'), KeyCode::Char('Z')] {
                let variant = KeyEvent::new(code, modifiers);
                assert_eq!(translate(key(variant), ctx), Action::Nothing);
                assert!(
                    matches!(translate(key(variant), shell), Action::Send(_)),
                    "{variant:?} reaches a terminal"
                );
                for mode in [Mode::Term(Pane::Agent), Mode::Commit, Mode::Picker] {
                    for context in [ctx, shell] {
                        let context = KeyContext { mode, ..context };
                        assert_eq!(translate(key(variant), context), Action::Nothing);
                    }
                }
            }
        }
        let suspend = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(
            translate(key(suspend), shell),
            Action::Send(b"\x1a".to_vec()),
            "as the terminal asked"
        );
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
            Action::Select(Movement::Line(Step::Down))
        );
        assert_eq!(
            translate(plain(KeyCode::Down), ctx),
            Action::Select(Movement::Line(Step::Down))
        );
        assert_eq!(
            translate(plain(KeyCode::Char('k')), ctx),
            Action::Select(Movement::Line(Step::Up))
        );
        assert_eq!(
            translate(plain(KeyCode::Up), ctx),
            Action::Select(Movement::Line(Step::Up))
        );
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
            Action::Select(Movement::Line(Step::Down)),
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
            Action::Scroll(Movement::Line(Step::Down))
        );
        assert_eq!(
            translate(plain(KeyCode::Up), open),
            Action::Scroll(Movement::Line(Step::Up))
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
            Action::Scroll(Movement::Line(Step::Up)),
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
            Action::Scrollback(Movement::End)
        );
        let shifted_lower = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shifted_lower), ctx),
            Action::Scrollback(Movement::End),
            "a host that reports the shift and the base letter"
        );
        assert_eq!(
            translate(plain(KeyCode::Char('g')), ctx),
            Action::Scrollback(Movement::Top),
            "the oldest line kept"
        );
        let shift_page_up = KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_page_up), ctx),
            Action::Scrollback(Movement::Page(Step::Up)),
            "the host terminal's habit works too"
        );
        // The lists do without the page keys; `G` moves their own.
        for other in [termis(Pane::Projects), termis(Pane::Changes)] {
            assert_eq!(translate(plain(KeyCode::PageUp), other), Action::Nothing);
        }
        assert_eq!(
            translate(key(shift_g), termis(Pane::Projects)),
            Action::Nothing,
            "a lone session without its tree"
        );
        assert_eq!(
            translate(key(shift_g), termis(Pane::Changes)),
            Action::Select(Movement::End)
        );
        assert_eq!(
            translate(plain(KeyCode::PageUp), agent(Modes::default(), false)),
            Action::Send(b"\x1b[5~".to_vec()),
            "the agent gets the key in agent mode"
        );
        assert!(closes_help(&Action::Scrollback(Movement::End)));
    }

    #[test]
    fn ctrl_d_u_f_b_and_g_page_and_jump_through_the_pane_highlighted() {
        let ctrl = |ch| key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
        let shift_g = key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let (up, down) = (Step::Up, Step::Down);
        let moves = [
            (ctrl('d'), Movement::Half(down)),
            (ctrl('u'), Movement::Half(up)),
            (ctrl('f'), Movement::Page(down)),
            (ctrl('b'), Movement::Page(up)),
            (plain(KeyCode::Char('g')), Movement::Top),
            (plain(KeyCode::Char('G')), Movement::End),
            (shift_g, Movement::End),
        ];
        let viewer = KeyContext {
            viewer: true,
            ..termis(Pane::Agent)
        };
        let diff = KeyContext {
            diff: true,
            ..termis(Pane::Changes)
        };
        let tree = KeyContext {
            tree: true,
            other_sessions: true,
            ..termis(Pane::Projects)
        };
        let list = KeyContext {
            other_sessions: true,
            ..termis(Pane::Projects)
        };
        // Each view, with the action that moves it.
        type Moves = fn(Movement) -> Action;
        let views: [(KeyContext, Moves); 6] = [
            (termis(Pane::Agent), Action::Scrollback),
            (viewer, Action::Scrollback),
            (termis(Pane::Changes), Action::Select),
            (diff, Action::Scroll),
            (tree, Action::TreeCursor),
            (list, Action::Cursor),
        ];
        for (ctx, action) in views {
            for (event, movement) in moves.clone() {
                assert_eq!(
                    translate(event, ctx),
                    action(movement),
                    "{movement:?} in {:?}",
                    ctx.mode
                );
            }
        }
        // The diff takes the page keys too, as the agent pane does; the
        // lists do without them.
        let page_up = key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT));
        assert_eq!(
            translate(plain(KeyCode::PageDown), diff),
            Action::Scroll(Movement::Page(down))
        );
        assert_eq!(
            translate(page_up.clone(), diff),
            Action::Scroll(Movement::Page(up))
        );
        for ctx in [termis(Pane::Changes), tree, list] {
            assert_eq!(translate(page_up.clone(), ctx), Action::Nothing);
        }
        // Nothing to move through in the projects pane of a lone session
        // without its tree.
        for (event, _) in moves.clone() {
            assert_eq!(translate(event, termis(Pane::Projects)), Action::Nothing);
        }
        // Exactly `Ctrl`; `g` without a modifier.
        for modifiers in [
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyModifiers::ALT,
        ] {
            for ch in ['d', 'u', 'f', 'b', 'g'] {
                let event = key(KeyEvent::new(KeyCode::Char(ch), modifiers));
                for ctx in [termis(Pane::Agent), termis(Pane::Changes), list] {
                    assert_eq!(
                        translate(event.clone(), ctx),
                        Action::Nothing,
                        "{modifiers:?} {ch}"
                    );
                }
            }
        }
        assert_eq!(translate(ctrl('g'), termis(Pane::Agent)), Action::Nothing);
        // The letters alone keep their meanings.
        let changes = termis(Pane::Changes);
        assert_eq!(translate(plain(KeyCode::Char('d')), changes), Action::Diff);
        assert_eq!(
            translate(plain(KeyCode::Char('u')), changes),
            Action::Unstage
        );
        assert_eq!(translate(plain(KeyCode::Char('f')), changes), Action::Files);
        assert_eq!(
            translate(plain(KeyCode::Char('b')), changes),
            Action::Nothing
        );
        // Commands: the key list closes, the diff stays; under the list
        // they still move, as `j` and `k` do.
        for movement in [Movement::Half(down), Movement::Top] {
            for action in [
                Action::Scrollback(movement),
                Action::Select(movement),
                Action::Scroll(movement),
                Action::TreeCursor(movement),
                Action::Cursor(movement),
            ] {
                assert!(closes_help(&action), "{action:?}");
            }
            let changes = Mode::Term(Pane::Changes);
            assert!(!closes_diff(&Action::Scroll(movement), changes));
        }
        let listing = KeyContext {
            help: true,
            ..termis(Pane::Agent)
        };
        assert_eq!(
            translate(ctrl('d'), listing),
            Action::Scrollback(Movement::Half(down))
        );
    }

    #[test]
    fn the_vi_keys_reach_the_child_in_agent_mode_and_are_text_in_the_boxes() {
        let ctx = agent(Modes::default(), false);
        let shell = KeyContext {
            terminal: true,
            ..ctx
        };
        let ctrl = |ch| key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
        for (event, bytes) in [
            (ctrl('d'), &b"\x04"[..]),
            (ctrl('u'), b"\x15"),
            (ctrl('f'), b"\x06"),
            (ctrl('b'), b"\x02"),
            (plain(KeyCode::Char('g')), b"g"),
            (plain(KeyCode::Char('G')), b"G"),
        ] {
            for ctx in [ctx, shell] {
                assert_eq!(
                    translate(event.clone(), ctx),
                    Action::Send(bytes.to_vec()),
                    "{event:?}"
                );
            }
        }
        let boxed = KeyContext {
            mode: Mode::Commit,
            ..ctx
        };
        let picking = KeyContext {
            mode: Mode::Picker,
            ..ctx
        };
        for ch in ['g', 'G'] {
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), boxed),
                Action::Edit(Edit::Insert(ch.to_string()))
            );
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), picking),
                Action::Type(ch.to_string())
            );
        }
        for ch in ['d', 'u', 'f', 'b'] {
            assert_eq!(translate(ctrl(ch), boxed), Action::Nothing);
            assert_eq!(translate(ctrl(ch), picking), Action::Nothing);
        }
    }

    #[test]
    fn the_agent_pane_pages_through_the_history_and_goes_to_either_end() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api"], &tx);
        // A screen four rows tall over sixteen lines of history.
        let emu = &mut sessions.shown_mut().tab_mut().expect("the agent's").emu;
        emu.resize(Size { rows: 4, cols: 10 });
        let lines: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
        emu.process(lines.join("\r\n").as_bytes());
        let mut moves = |movement| {
            scroll_center(sessions.shown_mut(), movement, 4);
            sessions.shown().tab().expect("the agent's").emu.scrolled()
        };
        let (up, down) = (Step::Up, Step::Down);
        assert_eq!(moves(Movement::Half(up)), 2, "half the screen");
        assert_eq!(moves(Movement::Page(up)), 6, "the screen");
        assert_eq!(moves(Movement::Half(down)), 4);
        assert_eq!(moves(Movement::Top), 16, "the oldest line kept");
        assert_eq!(moves(Movement::Page(up)), 16);
        assert_eq!(moves(Movement::Page(down)), 12);
        assert_eq!(moves(Movement::End), 0, "the live screen");
        assert_eq!(moves(Movement::Half(down)), 0);
        // The alternate screen keeps no history to move through.
        let emu = &mut sessions.shown_mut().tab_mut().expect("the agent's").emu;
        emu.process(b"\x1b[?1049h");
        let mut moves = |movement| {
            scroll_center(sessions.shown_mut(), movement, 4);
            sessions.shown().tab().expect("the agent's").emu.scrolled()
        };
        for movement in [Movement::Top, Movement::Half(up), Movement::Page(up)] {
            assert_eq!(moves(movement), 0, "{movement:?}");
        }
        sessions.quit();
        sessions.close_all(&rx);
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
            emu::MIN_SIZE
        );
        assert_eq!(
            Layout {
                terminal: Size { rows: 2, cols: 2 },
                ..wide
            }
            .agent_size(),
            emu::MIN_SIZE
        );
    }

    #[test]
    fn only_the_output_of_the_tab_shown_dirties_the_screen() {
        let mut sessions = Sessions::new();
        let (shown, hidden) = (sessions.next_id(), sessions.next_id());
        let tab = TabKey::agent(shown).tab;
        let output = |key| Msg::Pty(key, pty::Event::Output(b"x".to_vec()));
        assert!(dirties(&output(TabKey::agent(shown)), shown, tab));
        assert!(!dirties(&output(TabKey::agent(hidden)), shown, tab));
        // Nor does another tab of the session shown.
        let other = TabKey {
            tab: NO_TAB,
            ..TabKey::agent(shown)
        };
        assert!(!dirties(&output(other), shown, tab));
        let closed = Msg::Pty(TabKey::agent(shown), pty::Event::Closed);
        assert!(dirties(&closed, shown, tab));
        assert!(dirties(
            &Msg::Input(input::Event::Host(plain(KeyCode::Enter))),
            shown,
            tab
        ));
        let moved = wheel(MouseEventKind::Moved, 30, 5);
        assert!(!dirties(&Msg::Input(input::Event::Host(moved)), shown, tab));
        let found = Found {
            dir: "/p".into(),
            root: Ok(None),
        };
        assert!(
            dirties(&Msg::Found(found), shown, tab),
            "it may open a session"
        );
    }

    #[test]
    fn o_opens_the_picker_in_termis_mode_but_not_over_the_whole_terminal() {
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            assert_eq!(
                translate(plain(KeyCode::Char('o')), termis(pane)),
                Action::OpenPicker
            );
        }
        assert_eq!(
            translate(plain(KeyCode::Char('o')), agent(Modes::default(), false)),
            Action::Send(b"o".to_vec()),
            "the agent gets it in agent mode"
        );
        let spike = KeyContext {
            full_screen: true,
            side_panes: false,
            zoomable: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(plain(KeyCode::Char('o')), spike), Action::Nothing);
        let shift_o = KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_o), termis(Pane::Agent)),
            Action::Nothing
        );
        // A command: it closes the key list, and the diff with the mode.
        let listing = KeyContext {
            help: true,
            diff: true,
            ..termis(Pane::Changes)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('o')), listing),
            Action::OpenPicker
        );
        assert!(closes_help(&Action::OpenPicker));
        assert!(closes_diff(&Action::OpenPicker, Mode::Picker));
        assert_eq!(Mode::Picker.pane(), Pane::Agent);
        assert_eq!(Mode::Picker.without_side_panes(), Mode::Picker);
    }

    #[test]
    fn t_opens_a_terminal_and_comma_and_period_show_the_tabs_beside() {
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            assert_eq!(
                translate(plain(KeyCode::Char('t')), termis(pane)),
                Action::OpenTerminal
            );
        }
        let spike = KeyContext {
            full_screen: true,
            side_panes: false,
            zoomable: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(plain(KeyCode::Char('t')), spike), Action::Nothing);
        // With one tab, `,` and `.` do nothing.
        for ch in [',', '.'] {
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), termis(Pane::Agent)),
                Action::Nothing
            );
        }
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            let ctx = KeyContext {
                other_tabs: true,
                ..termis(pane)
            };
            assert_eq!(
                translate(plain(KeyCode::Char(',')), ctx),
                Action::SwitchTab(Step::Up)
            );
            assert_eq!(
                translate(plain(KeyCode::Char('.')), ctx),
                Action::SwitchTab(Step::Down)
            );
            let ctrl = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL);
            assert_eq!(translate(key(ctrl), ctx), Action::Nothing);
        }
        // The child of the tab shown gets them in agent mode.
        let shell = KeyContext {
            terminal: true,
            other_tabs: true,
            ..agent(Modes::default(), false)
        };
        for ch in ['t', ',', '.'] {
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), shell),
                Action::Send(ch.to_string().into_bytes())
            );
        }
        // Commands: the key list closes.
        assert!(closes_help(&Action::OpenTerminal));
        assert!(closes_help(&Action::SwitchTab(Step::Down)));
    }

    #[test]
    fn the_picker_takes_a_query_and_opens_with_enter_or_tab() {
        let ctx = KeyContext {
            mode: Mode::Picker,
            ..agent(Modes::default(), false)
        };
        let typed = |text: &str| Action::Type(text.into());
        for ch in ['a', 'q', 'o', 'j', '?', '/', '~', '.', '1', ' '] {
            assert_eq!(
                translate(plain(KeyCode::Char(ch)), ctx),
                typed(&ch.to_string()),
                "{ch}"
            );
        }
        let shift_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_a), ctx), typed("A"));
        assert_eq!(translate(plain(KeyCode::Backspace), ctx), Action::Erase);
        let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_h), ctx), Action::Erase);
        assert_eq!(translate(plain(KeyCode::Up), ctx), Action::Choose(Step::Up));
        assert_eq!(
            translate(plain(KeyCode::Down), ctx),
            Action::Choose(Step::Down)
        );
        // Enter continues the project's last conversation, Tab starts a
        // new one; neither completes a path.
        assert_eq!(
            translate(plain(KeyCode::Enter), ctx),
            Action::Open {
                new_conversation: false
            }
        );
        assert_eq!(
            translate(plain(KeyCode::Tab), ctx),
            Action::Open {
                new_conversation: true
            }
        );
        assert_eq!(
            translate(plain(KeyCode::Esc), ctx),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        assert_eq!(translate(key(CTRL_Q), ctx), Action::Mode(Mode::Agent));
        // A paste brings its first line.
        assert_eq!(
            translate(HostEvent::Paste("~/dev/api\r\nrest".into()), ctx),
            typed("~/dev/api")
        );
        assert_eq!(translate(HostEvent::Paste("a\rb".into()), ctx), typed("a"));
        assert_eq!(
            translate(HostEvent::Paste("\nb".into()), ctx),
            Action::Nothing
        );
        // Every other key does nothing; Ctrl+Z still never reaches the
        // agent.
        for event in [
            key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
            key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            plain(KeyCode::Left),
            plain(KeyCode::PageDown),
            plain(KeyCode::F(1)),
        ] {
            assert_eq!(translate(event, ctx), Action::Nothing);
        }
        // The window's focus still reaches the agent.
        let focused = KeyContext {
            focus_events: true,
            ..ctx
        };
        assert_eq!(
            translate(HostEvent::FocusLost, focused),
            Action::Send(b"\x1b[O".to_vec())
        );
        // Under it the mouse only ends a drag the agent saw begin.
        assert_eq!(
            translate(wheel(MouseEventKind::ScrollUp, 30, 5), ctx),
            Action::Nothing
        );
        let left = MouseButton::Left;
        let dragging = KeyContext {
            modes: Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Motion,
                    encoding: keys::MouseEncoding::Sgr,
                }),
                ..Modes::default()
            },
            held: Held::default().after(MouseEventKind::Down(left), &Action::Send(vec![])),
            ..ctx
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 30, 5), dragging),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Up(left), 30, 5), dragging),
            Action::Send(b"\x1b[<0;10;5m".to_vec())
        );
    }

    #[test]
    fn n_and_shift_n_show_the_next_and_previous_session_when_there_are_others() {
        let shift = |ch| key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT));
        let alone = termis(Pane::Agent);
        for event in [
            plain(KeyCode::Char('n')),
            plain(KeyCode::Char('N')),
            shift('N'),
            shift('n'),
        ] {
            assert_eq!(translate(event, alone), Action::Nothing, "one session");
        }
        let next = Action::Switch(Step::Down);
        let previous = Action::Switch(Step::Up);
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            let ctx = KeyContext {
                other_sessions: true,
                ..termis(pane)
            };
            assert_eq!(translate(plain(KeyCode::Char('n')), ctx), next);
            assert_eq!(translate(plain(KeyCode::Char('N')), ctx), previous);
            assert_eq!(translate(shift('N'), ctx), previous);
            assert_eq!(
                translate(shift('n'), ctx),
                previous,
                "a host that reports the shift and the base letter"
            );
            let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
            assert_eq!(translate(key(ctrl_n), ctx), Action::Nothing);
        }
        // The agent gets them in agent mode; the picker takes them as text.
        let others = KeyContext {
            other_sessions: true,
            ..agent(Modes::default(), false)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('n')), others),
            Action::Send(b"n".to_vec())
        );
        let picking = KeyContext {
            mode: Mode::Picker,
            ..others
        };
        assert_eq!(
            translate(plain(KeyCode::Char('n')), picking),
            Action::Type("n".into())
        );
        // A command: the key list closes, and so does the diff.
        assert!(closes_help(&next));
        assert!(closes_diff(&previous, Mode::Term(Pane::Changes)));
    }

    #[test]
    fn the_projects_pane_moves_through_the_sessions_and_shows_one() {
        let shift_j = KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT);
        let keys = [
            plain(KeyCode::Char('j')),
            plain(KeyCode::Down),
            plain(KeyCode::Char('k')),
            plain(KeyCode::Up),
            plain(KeyCode::Enter),
            key(shift_j),
        ];
        // With one session these keys do nothing there.
        for event in keys.clone() {
            assert_eq!(translate(event, termis(Pane::Projects)), Action::Nothing);
        }
        let ctx = KeyContext {
            other_sessions: true,
            ..termis(Pane::Projects)
        };
        let (down, up) = (Movement::Line(Step::Down), Movement::Line(Step::Up));
        let actions = [down, down, up, up].map(Action::Cursor);
        let actions = actions.into_iter().chain([Action::Show]);
        for (event, action) in keys.into_iter().zip(actions) {
            assert_eq!(translate(event, ctx), action);
        }
        assert_eq!(translate(key(shift_j), ctx), Action::Nothing);
        // The other panes keep their own.
        let changes = KeyContext {
            other_sessions: true,
            ..termis(Pane::Changes)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('j')), changes),
            Action::Select(Movement::Line(Step::Down))
        );
        assert_eq!(translate(plain(KeyCode::Enter), changes), Action::Nothing);
        // A command: the key list closes.
        assert!(closes_help(&Action::Cursor(Movement::Line(Step::Down))));
        assert!(closes_help(&Action::Show));
    }

    #[test]
    fn f_swaps_the_list_for_the_tree_whose_keys_then_move_it() {
        // f in termis mode, whatever the pane, while the side panes are
        // shown: not zoomed, not over the whole terminal, not in agent
        // mode.
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            for tree in [false, true] {
                let ctx = KeyContext {
                    tree,
                    ..termis(pane)
                };
                assert_eq!(translate(plain(KeyCode::Char('f')), ctx), Action::Files);
            }
        }
        let spike = KeyContext {
            full_screen: true,
            side_panes: false,
            zoomable: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(translate(plain(KeyCode::Char('f')), spike), Action::Nothing);
        let zoomed = KeyContext {
            side_panes: false,
            ..termis(Pane::Agent)
        };
        assert_eq!(
            translate(plain(KeyCode::Char('f')), zoomed),
            Action::Nothing
        );
        assert_eq!(
            translate(plain(KeyCode::Char('f')), agent(Modes::default(), false)),
            Action::Send(b"f".to_vec())
        );
        let shift_f = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT);
        assert_eq!(
            translate(key(shift_f), termis(Pane::Projects)),
            Action::Nothing
        );
        // With the tree shown and the projects pane highlighted, its keys,
        // in place of the list's even with other sessions.
        let tree = KeyContext {
            tree: true,
            other_sessions: true,
            ..termis(Pane::Projects)
        };
        let (down, up) = (Movement::Line(Step::Down), Movement::Line(Step::Up));
        for (event, action) in [
            (plain(KeyCode::Char('j')), Action::TreeCursor(down)),
            (plain(KeyCode::Down), Action::TreeCursor(down)),
            (plain(KeyCode::Char('k')), Action::TreeCursor(up)),
            (plain(KeyCode::Up), Action::TreeCursor(up)),
            (plain(KeyCode::Char('l')), Action::TreeOpen),
            (plain(KeyCode::Enter), Action::TreeEnter),
            (plain(KeyCode::Char('h')), Action::TreeClose),
            (plain(KeyCode::Char('x')), Action::Close),
        ] {
            assert_eq!(translate(event, tree), action);
        }
        let shift_h = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_h), tree), Action::Nothing);
        // Not with the list shown, nor in the other panes.
        let list = KeyContext {
            tree: false,
            ..tree
        };
        assert_eq!(translate(plain(KeyCode::Enter), list), Action::Show);
        for ch in ['h', 'l'] {
            assert_eq!(translate(plain(KeyCode::Char(ch)), list), Action::Nothing);
        }
        let changes = KeyContext {
            mode: Mode::Term(Pane::Changes),
            ..tree
        };
        assert_eq!(
            translate(plain(KeyCode::Char('j')), changes),
            Action::Select(down)
        );
        assert_eq!(
            translate(plain(KeyCode::Char('l')), changes),
            Action::Nothing
        );
        // Commands: the key list closes; the diff stays with its pane.
        for action in [
            Action::Files,
            Action::TreeCursor(down),
            Action::TreeOpen,
            Action::TreeEnter,
            Action::TreeClose,
        ] {
            assert!(closes_help(&action), "{action:?}");
        }
        assert!(!closes_diff(&Action::Files, Mode::Term(Pane::Changes)));
    }

    #[test]
    fn the_viewer_takes_the_scroll_keys_in_both_modes_and_drops_the_rest() {
        let viewing = KeyContext {
            viewer: true,
            other_tabs: true,
            ..agent(Modes::default(), false)
        };
        let center = KeyContext {
            mode: Mode::Term(Pane::Agent),
            ..viewing
        };
        let shift = |code| key(KeyEvent::new(code, KeyModifiers::SHIFT));
        let ctrl = |ch| key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
        let (down, up) = (Step::Down, Step::Up);
        for ctx in [viewing, center] {
            for (event, movement) in [
                (plain(KeyCode::Char('j')), Movement::Line(down)),
                (plain(KeyCode::Down), Movement::Line(down)),
                (plain(KeyCode::Char('k')), Movement::Line(up)),
                (plain(KeyCode::Up), Movement::Line(up)),
                (ctrl('d'), Movement::Half(down)),
                (ctrl('u'), Movement::Half(up)),
                (ctrl('f'), Movement::Page(down)),
                (ctrl('b'), Movement::Page(up)),
                (plain(KeyCode::PageDown), Movement::Page(down)),
                (shift(KeyCode::PageUp), Movement::Page(up)),
                (plain(KeyCode::Char('G')), Movement::End),
                (shift(KeyCode::Char('G')), Movement::End),
                (plain(KeyCode::Char('g')), Movement::Top),
            ] {
                assert_eq!(
                    translate(event, ctx),
                    Action::Scrollback(movement),
                    "{:?}",
                    ctx.mode
                );
            }
        }
        // In agent mode there is no child for the other keys, pastes and
        // the window's focus; `Ctrl+Q` still leaves.
        for event in [
            plain(KeyCode::Char('x')),
            plain(KeyCode::Char('q')),
            plain(KeyCode::Enter),
            key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            HostEvent::Paste("text".into()),
            HostEvent::FocusLost,
            wheel(MouseEventKind::Down(MouseButton::Left), 30, 5),
        ] {
            assert_eq!(translate(event, viewing), Action::Nothing);
        }
        assert_eq!(
            translate(key(CTRL_Q), viewing),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        // The wheel over the center pane moves it in every mode.
        let changes = KeyContext {
            mode: Mode::Term(Pane::Changes),
            ..viewing
        };
        for ctx in [viewing, center, changes] {
            assert_eq!(
                translate(wheel(MouseEventKind::ScrollUp, 30, 5), ctx),
                Action::Wheel(Pane::Agent, Step::Up)
            );
        }
        // Termis mode keeps its commands; `g` goes to the top of the pane
        // highlighted.
        assert_eq!(translate(plain(KeyCode::Char('x')), center), Action::Close);
        assert_eq!(
            translate(plain(KeyCode::Char(',')), center),
            Action::SwitchTab(Step::Up)
        );
        assert_eq!(
            translate(plain(KeyCode::Char('g')), changes),
            Action::Select(Movement::Top)
        );
        // With the shift or another modifier, the letters do nothing.
        for modifiers in [
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::ALT,
        ] {
            let event = key(KeyEvent::new(KeyCode::Char('d'), modifiers));
            assert_eq!(translate(event, viewing), Action::Nothing, "{modifiers:?}");
        }
        assert_eq!(translate(ctrl('g'), viewing), Action::Nothing);
    }

    #[test]
    fn enter_views_the_file_under_the_trees_cursor_and_opens_a_directory() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api"], &tx);
        let dir = sessions.shown().place.cwd.clone();
        fs::create_dir(dir.join("src")).expect("mkdir");
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("write");
        // Before there are files, nothing.
        assert!(!tree_enter(sessions.shown_mut(), &tx, false));
        let walked = Walked {
            dir: dir.clone(),
            files: walk::list(&dir),
        };
        assert!(sessions.dir_files(&walked, Instant::now()));
        // On a directory, it unfolds.
        let session = sessions.shown_mut();
        assert!(!tree_enter(session, &tx, false), "a directory");
        assert!(session.viewing().is_none());
        session.tree.step(Movement::Line(Step::Down), 1);
        assert_eq!(session.tree.file(), Some(PathBuf::from("src/main.rs")));
        // On a file, the viewer shows it, read from its absolute path.
        assert!(tree_enter(session, &tx, false));
        let viewer = session.viewing().expect("shown");
        assert_eq!(viewer.name, "main.rs");
        assert_eq!(viewer.path, dir.join("src/main.rs"));
        let (key, content) = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an answer") {
                Msg::Read(key, content) => break (key, content),
                Msg::Pty(..) => {}
                _ => panic!("only the read answers"),
            }
        };
        assert!(sessions.file_read(key, content));
        let viewer = sessions.shown().viewing().expect("shown");
        assert_eq!(viewer.pane().content.lines(), 1);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn x_closes_the_viewer_at_once() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        let api = sessions.shown().id;
        let path = sessions.shown().place.cwd.join("a.txt");
        assert!(sessions.shown_mut().view(path, &tx, false));
        let viewer = sessions.shown().shown_tab();
        let ending = close_ending(sessions.shown_row());
        assert_eq!(ending, Ending::CloseTab(api, viewer));
        assert_eq!(confirmation(&sessions, ending), None, "nothing runs in it");
        assert!(!quits(ending, true));
        // So does its row in the list.
        let [_, row, _] = sessions.rows()[..] else {
            panic!("three rows");
        };
        assert_eq!(close_ending(row), ending);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn e_opens_the_editor_while_it_has_a_file() {
        let editing = |pane| KeyContext {
            edit: true,
            ..termis(pane)
        };
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            assert_eq!(
                translate(plain(KeyCode::Char('e')), editing(pane)),
                Action::OpenEditor
            );
            assert_eq!(
                translate(plain(KeyCode::Char('e')), termis(pane)),
                Action::Nothing,
                "no file"
            );
        }
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let held = KeyEvent::new(KeyCode::Char('e'), modifiers);
            assert_eq!(translate(key(held), editing(Pane::Agent)), Action::Nothing);
        }
        // Not with `--spike`, where the agent is drawn over the whole
        // terminal.
        let spike = KeyContext {
            full_screen: true,
            side_panes: false,
            zoomable: false,
            ..editing(Pane::Agent)
        };
        assert_eq!(translate(plain(KeyCode::Char('e')), spike), Action::Nothing);
        // The child of the tab shown gets it in agent mode.
        assert_eq!(
            translate(plain(KeyCode::Char('e')), agent(Modes::default(), false)),
            Action::Send(b"e".to_vec())
        );
        assert!(closes_help(&Action::OpenEditor));
    }

    #[test]
    fn e_has_the_file_under_the_trees_cursor_else_the_viewers() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api"], &tx);
        let dir = sessions.shown().place.cwd.clone();
        fs::create_dir(dir.join("src")).expect("mkdir");
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("write");
        fs::write(dir.join("README.md"), "# api\n").expect("write");
        let walked = Walked {
            dir: dir.clone(),
            files: walk::list(&dir),
        };
        assert!(sessions.dir_files(&walked, Instant::now()));
        let session = sessions.shown_mut();
        // Neither the tree nor the viewer shown: none.
        assert_eq!(editable(session, Pane::Projects), None);
        session.tree.toggle();
        assert_eq!(editable(session, Pane::Projects), None, "a directory");
        session.tree.step(Movement::Line(Step::Down), 1);
        let readme = dir.join("README.md");
        assert_eq!(editable(session, Pane::Projects), Some(readme.clone()));
        assert_eq!(editable(session, Pane::Agent), None, "no viewer");
        // With the viewer shown, its file from the other panes; the tree
        // keeps its own with the projects pane.
        assert!(tree_enter(session, &tx, false));
        session.tree.step(Movement::Line(Step::Up), 1);
        for pane in [Pane::Agent, Pane::Changes] {
            assert_eq!(editable(session, pane), Some(readme.clone()), "{pane:?}");
        }
        assert_eq!(editable(session, Pane::Projects), None, "a directory");
        session.tree.toggle();
        assert_eq!(editable(session, Pane::Projects), Some(readme.clone()));
        // Another tab shown: none.
        let agent = session.tab_ids()[0];
        assert!(session.show_tab(agent, false));
        assert_eq!(editable(session, Pane::Agent), None);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn e_starts_the_editor_in_a_terminal_or_says_there_is_none() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        let size = Size { rows: 2, cols: 10 };
        let path = sessions.shown().place.cwd.join("a.txt");
        let session = sessions.shown_mut();
        assert!(session.view(path.clone(), &tx, false));
        let viewer = session.shown_tab();
        // With neither VISUAL nor EDITOR set, a notice and no tab.
        let none = open_editor(session, Pane::Agent, |_| None, size, &tx, false);
        assert_eq!(none, Err(strings::NO_EDITOR.to_owned()));
        assert_eq!(session.tab_ids().len(), 2, "the agent's and the viewer's");
        // Without a file, nothing.
        let agent = session.tab_ids()[0];
        assert!(session.show_tab(agent, false));
        let not_asked = |_: &Path| -> Option<Shell> { panic!("no file to edit") };
        assert_eq!(
            open_editor(session, Pane::Agent, not_asked, size, &tx, false),
            Ok(false)
        );
        // The editor gets the viewer's file in a terminal shown after the
        // others, where a program runs until it exits and Ctrl+Z does not
        // reach.
        assert!(session.show_tab(viewer, false));
        let mut given = None;
        let editor = |file: &Path| {
            given = Some(file.to_path_buf());
            Some(Shell::editing("read line"))
        };
        let opened = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            open_editor(session, Pane::Agent, editor, size, &tx, false)
        };
        assert_eq!(opened, Ok(true));
        assert_eq!(given, Some(path));
        let tab = session.tab().expect("the editor's");
        assert_eq!(tab.name.as_deref(), Some("vi a.txt"));
        assert!(!tab.takes_suspend());
        assert!(!session.agent.takes_suspend());
        let ending = close_ending(sessions.shown_row());
        assert_eq!(
            confirmation(&sessions, ending).as_deref(),
            Some("api · vi a.txt runs a program · press x again to close it")
        );
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("api · vi a.txt runs a program · press q again to quit")
        );
        // A terminal's shell takes Ctrl+Z.
        let shell = terminal(&mut sessions, "exec sleep 30", &tx);
        assert!(sessions.shown().tab().is_some_and(Tab::takes_suspend));
        // Once the editor exits, its tab closes.
        let session = sessions.shown_mut();
        let editor = session.tab_ids()[2];
        assert!(session.show_tab(editor, false));
        session.write(b"\r");
        let deadline = Instant::now() + Duration::from_secs(10);
        while sessions.shown().tab_ids().contains(&editor) {
            assert!(Instant::now() < deadline, "closes before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            sessions.settle(Instant::now(), false);
        }
        assert_eq!(sessions.shown().tab_ids(), [agent, viewer, shell]);
        assert_eq!(sessions.shown().shown_tab(), viewer, "the tab before it");
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn each_session_keeps_its_own_tree_and_takes_its_own_files() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        let [api, web] = [0, 1].map(|index| sessions.iter().nth(index).expect("open").id);
        let mut repos = Repos::new();
        let repo = repos.get_or_spawn(temporary.path(), &tx).expect("spawn");
        // api runs in its directory of the repository.
        sessions.shown_mut().repo = Some(repo);
        sessions.shown_mut().place.repo = Some(temporary.path().to_path_buf());
        // f on api leaves web's list.
        sessions.shown_mut().tree.toggle();
        assert!(sessions.shown().tree.shown);
        assert!(sessions.show(web, false));
        assert!(!sessions.shown().tree.shown, "web keeps the list");
        // The repository's files reach api's tree, the walk of web's
        // directory web's; neither is the other's.
        let list = |paths: &[&str]| git::files::Files {
            paths: paths.iter().map(PathBuf::from).collect(),
            cut: false,
        };
        let now = Instant::now();
        let api_dir = Path::new("api");
        assert!(
            !sessions.repo_files(repo, api_dir, &Ok(list(&["api/src/a.rs"])), now),
            "not shown"
        );
        // The top rows of a session's tree, or why it has none.
        let paths = |sessions: &Sessions, id| -> Vec<String> {
            match &sessions.get(id).expect("open").tree.content {
                Some(Ok(tree)) => tree
                    .rows(&Default::default())
                    .into_iter()
                    .map(|row| row.path.to_string_lossy().into_owned())
                    .collect(),
                Some(Err(error)) => vec![error.clone()],
                None => Vec::new(),
            }
        };
        assert_eq!(paths(&sessions, api), ["src"]);
        assert!(paths(&sessions, web).is_empty());
        // Another directory's files are not api's.
        let root_list = Ok(list(&["README.md", "api/src/a.rs"]));
        assert!(!sessions.repo_files(repo, Path::new(""), &root_list, now));
        assert_eq!(paths(&sessions, api), ["src"]);
        let dir = |sessions: &Sessions, id| sessions.get(id).expect("open").place.cwd.clone();
        let walked = Walked {
            dir: dir(&sessions, web),
            files: Ok(list(&["notes.md"])),
        };
        assert!(sessions.dir_files(&walked, now), "web is shown");
        let walked = Walked {
            dir: dir(&sessions, api),
            files: Err(anyhow::anyhow!("no")),
        };
        assert!(!sessions.dir_files(&walked, now), "api has a repository");
        assert_eq!(paths(&sessions, api), ["src"]);
        assert_eq!(paths(&sessions, web), ["notes.md"]);
        // A change of the repository's status wants api's files again,
        // once FILES_EVERY passed since the answer.
        let later = now + Duration::from_secs(5);
        sessions.files_changed(repo);
        assert!(sessions.get(api).expect("open").tree.due(later, true));
        assert!(!sessions.get(web).expect("open").tree.due(later, true));
        assert!(!sessions.repo_files(repo, api_dir, &Err("fatal: bad".into()), later));
        assert_eq!(paths(&sessions, api), ["fatal: bad"]);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn the_tree_asks_its_repository_or_walks_its_directory() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["plain"], &tx);
        let plain_dir = sessions.shown().place.cwd.clone();
        fs::write(plain_dir.join("a.txt"), "a\n").expect("write");
        let repos = Repos::new();
        let t0 = Instant::now();
        // Hidden, nothing is asked.
        ask_files(&mut sessions, &repos, &tx, t0, false);
        // On screen, a walk of the directory answers once.
        ask_files(&mut sessions, &repos, &tx, t0, true);
        ask_files(&mut sessions, &repos, &tx, t0, true);
        let walked = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an answer") {
                Msg::Walked(walked) => break walked,
                Msg::Pty(..) => {}
                _ => panic!("only the walk answers"),
            }
        };
        assert_eq!(walked.dir, plain_dir);
        assert!(sessions.dir_files(&walked, t0));
        assert!(matches!(&sessions.shown().tree.content, Some(Ok(_))));
        // Inside a repository its worker lists those of the session's
        // directory, once wanted again, one list at a time.
        let mut repos = Repos::new();
        git::tests::init(&plain_dir);
        fs::create_dir(plain_dir.join("sub")).expect("mkdir");
        fs::write(plain_dir.join("sub/b.txt"), "b\n").expect("write");
        let repo = repos.get_or_spawn(&plain_dir, &tx).expect("spawn");
        sessions.shown_mut().repo = Some(repo);
        sessions.shown_mut().place = Place::new(plain_dir.join("sub"), Some(plain_dir), None);
        let t1 = t0 + Duration::from_secs(5);
        ask_files(&mut sessions, &repos, &tx, t1, true);
        sessions.shown_mut().tree.want();
        ask_files(&mut sessions, &repos, &tx, t1, true);
        sessions.shown_mut().tree.want();
        ask_files(&mut sessions, &repos, &tx, t1, true);
        assert!(!sessions.shown().tree.due(t1, true), "one in flight");
        let (dir, files) = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an answer") {
                Msg::Git(from, git::worker::Event::Files { dir, result }) => {
                    assert_eq!(from, repo);
                    break (dir, result);
                }
                Msg::Pty(..) => {}
                _ => panic!("only the worker answers"),
            }
        };
        assert_eq!(dir, Path::new("sub"));
        assert_eq!(files.expect("lists").paths, [PathBuf::from("sub/b.txt")]);
        while let Ok(msg) = rx.recv_timeout(Duration::from_millis(200)) {
            let listed = matches!(msg, Msg::Git(_, git::worker::Event::Files { .. }));
            assert!(!listed, "asked once");
        }
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn opening_a_session_goes_to_agent_mode_or_says_why_not() {
        let held = Held::default().after(
            MouseEventKind::Down(MouseButton::Left),
            &Action::Send(vec![]),
        );
        let (mut mode, mut pressed, mut toast) = (Mode::Picker, held, None);
        opened(Ok(None), &mut mode, &mut pressed, &mut toast);
        assert_eq!((mode, pressed), (Mode::Agent, Held::default()));
        assert!(toast.is_none());
        let notice = strings::telemetry_trouble("no room");
        opened(
            Ok(Some(notice.clone())),
            &mut mode,
            &mut pressed,
            &mut toast,
        );
        assert_eq!(toast.map(|toast| toast.text), Some(notice));
        let (mut mode, mut pressed, mut toast) = (Mode::Picker, held, None);
        opened(Err("api: x".into()), &mut mode, &mut pressed, &mut toast);
        assert_eq!((mode, pressed), (Mode::Picker, held), "as it was");
        let toast = toast.expect("a notice");
        assert_eq!(
            (toast.text.as_str(), toast.kind),
            ("api: x", toast::Kind::Error)
        );
    }

    /// An agent that ran lately in the directories it holds; it is never
    /// launched.
    struct Recent(Vec<PathBuf>);

    impl agent::Agent for Recent {
        fn name(&self) -> &str {
            "recent"
        }

        fn spec(&self, _: &agent::Launch<'_>, _: Size, _: Option<&Instance>) -> pty::Spec {
            unreachable!("never launched")
        }

        fn recent_projects(&self) -> Vec<PathBuf> {
            self.0.clone()
        }
    }

    #[test]
    fn the_picker_lists_the_open_sessions_then_the_other_recent_projects() {
        let temporary = TempDir::new().expect("temp dir");
        let home = temporary.path().canonicalize().expect("exists");
        let (open, recent) = (home.join("open"), home.join("recent"));
        for dir in [&open, &recent] {
            fs::create_dir(dir).expect("mkdir");
        }
        let (tx, _rx) = mpsc::channel::<Msg>();
        let fake = Fake("exec sleep 30");
        let none = anyhow::anyhow!("no instance directory");
        let starter = Starter {
            agent: &fake,
            instances: Err(&none),
            tx: &tx,
        };
        let mut sessions = Sessions::new();
        let place = Place::new(open.clone(), None, Some(&home));
        let size = Size { rows: 2, cols: 10 };
        let (session, _) = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            starter
                .start(sessions.next_id(), place, Vec::new(), false, size)
                .expect("starts")
        };
        sessions.push(session);
        // The agent lists the open project too, last.
        let picker = picker_for(&sessions, &Recent(vec![recent, open]), Some(&home));
        let rows: Vec<(&str, &str, bool)> = picker
            .rows()
            .map(|item| (item.name.as_str(), item.path.as_str(), item.open))
            .collect();
        assert_eq!(
            rows,
            [("open", "~/open", true), ("recent", "~/recent", false)]
        );
    }

    #[test]
    fn x_and_q_take_a_second_press_while_an_agent_they_stop_works_or_asks() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let mut ids = Sessions::new();
        let (one, two) = (ids.next_id(), ids.next_id());
        let close = Ending::Close(one);
        let armed = |ending, at: Instant| Armed {
            ending,
            until: at + CONFIRM_WITHIN,
            notice: "api is working · press x again to close it".into(),
        };
        let (first, quit) = (armed(close, t0), armed(Ending::Quit, t0));
        // With no agent to stop that works or asks: at once.
        assert!(acts(None, close, false, t0));
        assert!(acts(None, Ending::Quit, false, t0));
        assert!(acts(Some(&quit), close, false, t0));
        // Else the first press waits.
        assert!(!acts(None, close, true, t0));
        assert!(!acts(None, Ending::Quit, true, t0));
        // The second press of the same acts inside the deadline only.
        let late = t0 + CONFIRM_WITHIN;
        assert!(acts(Some(&first), close, true, late - ms));
        assert!(!acts(Some(&first), close, true, late));
        assert!(acts(Some(&quit), Ending::Quit, true, t0 + ms));
        assert!(!acts(Some(&quit), Ending::Quit, true, late));
        // A press that ends something else waits again.
        assert!(!acts(Some(&first), Ending::Quit, true, t0 + ms));
        assert!(!acts(Some(&quit), close, true, t0 + ms));
        assert!(!acts(Some(&first), Ending::Close(two), true, t0 + ms));
        // A first press counts only while its notice is the one shown; a
        // notice that took its place stays when a later press acts.
        let notice = |text: &str| Toast::new(text.into(), toast::Kind::Error, t0, CONFIRM_WITHIN);
        let shown = notice(&first.notice);
        assert_eq!(
            waiting(Some(first.clone()), Some(&shown)),
            Some(first.clone())
        );
        let failed = notice("git: commit failed");
        assert_eq!(waiting(Some(first.clone()), Some(&failed)), None);
        assert_eq!(waiting(Some(first), None), None, "it went");
        assert_eq!(waiting(None, Some(&shown)), None);
        // q quits; x quits on the last session only, never on a terminal.
        assert!(quits(Ending::Quit, false));
        assert!(quits(Ending::Quit, true));
        assert!(!quits(close, false));
        assert!(quits(close, true), "x on the last session quits");
        let tab = Ending::CloseTab(one, NO_TAB);
        assert!(!quits(tab, true));
        assert!(!acts(Some(&armed(tab, t0)), close, true, t0 + ms));
        assert!(acts(Some(&armed(tab, t0)), tab, true, t0 + ms));
    }

    /// Starts sessions of an agent that stays quiet in new directories of
    /// `base` named `names`, without telemetry, their events reaching
    /// `tx`.
    fn quiet_sessions(base: &Path, names: &[&str], tx: &mpsc::Sender<Msg>) -> Sessions {
        let fake = Fake("exec sleep 30");
        let none = anyhow::anyhow!("no instance directory");
        let starter = Starter {
            agent: &fake,
            instances: Err(&none),
            tx,
        };
        let mut sessions = Sessions::new();
        for name in names {
            let dir = base.join(name);
            fs::create_dir(&dir).expect("mkdir");
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            let place = Place::new(dir, None, None);
            let size = Size { rows: 2, cols: 10 };
            let (session, _) = starter
                .start(sessions.next_id(), place, Vec::new(), false, size)
                .expect("starts");
            sessions.push(session);
        }
        sessions
    }

    /// Opens a terminal running `script` in the session shown. Returns
    /// its tab's id.
    fn terminal(sessions: &mut Sessions, script: &str, tx: &mpsc::Sender<Msg>) -> TabId {
        let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let size = Size { rows: 2, cols: 10 };
        let session = sessions.shown_mut();
        session
            .open_terminal(&Shell::running(script), size, tx, false)
            .expect("opens");
        session.shown_tab()
    }

    #[test]
    fn x_closes_the_tab_of_its_row_with_the_agents_session() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        let [api, web] = [0, 1].map(|index| sessions.iter().nth(index).expect("open").id);
        assert_eq!(close_ending(sessions.shown_row()), Ending::Close(api));
        let zsh = terminal(&mut sessions, "exec sleep 30", &tx);
        assert_eq!(
            close_ending(sessions.shown_row()),
            Ending::CloseTab(api, zsh)
        );
        // The cursor's row, wherever it is.
        let [api_row, zsh_row, web_row] = sessions.rows()[..] else {
            panic!("three rows");
        };
        assert_eq!(api_row.session, api);
        assert_eq!(close_ending(api_row), Ending::Close(api));
        assert_eq!(close_ending(zsh_row), Ending::CloseTab(api, zsh));
        assert_eq!(close_ending(web_row), Ending::Close(web));
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn the_first_press_names_the_terminal_where_a_program_runs() {
        use crate::agent::State;
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        let [api, web] = [0, 1].map(|index| sessions.iter().nth(index).expect("open").id);
        // Job control runs the program in the terminal's foreground.
        let serves = "set -m; sh -c 'printf serving; exec sleep 60'";
        let job = terminal(&mut sessions, serves, &tx);
        let idle = terminal(&mut sessions, "read line", &tx);
        let runs = |sessions: &Sessions, id| {
            sessions
                .get(id)
                .is_some_and(|session| session.running().next().is_some())
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !runs(&sessions, api) {
            assert!(Instant::now() < deadline, "the program starts");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(confirmation(&sessions, Ending::CloseTab(api, idle)), None);
        assert_eq!(
            confirmation(&sessions, Ending::CloseTab(api, job)).as_deref(),
            Some("api · sh runs a program · press x again to close it")
        );
        assert_eq!(
            confirmation(&sessions, Ending::Close(api)).as_deref(),
            Some("api · sh runs a program · press x again to close the session")
        );
        assert_eq!(confirmation(&sessions, Ending::Close(web)), None);
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("api · sh runs a program · press q again to quit")
        );
        // An agent that works is named first.
        sessions.shown_mut().status.state = Some(State::Working);
        assert_eq!(
            confirmation(&sessions, Ending::Close(api)).as_deref(),
            Some("api is working · press x again to close it")
        );
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("api is working · press q again to quit")
        );
        sessions.shown_mut().status.state = Some(State::Waiting);
        // Programs in several terminals are counted.
        assert!(sessions.show(web, false));
        terminal(&mut sessions, serves, &tx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !runs(&sessions, web) {
            assert!(Instant::now() < deadline, "the program starts");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("api · sh and 1 more run programs · press q again to quit")
        );
        // On the last session, x quits.
        assert!(sessions.close(api, false, Instant::now()));
        assert_eq!(
            confirmation(&sessions, Ending::Close(web)).as_deref(),
            Some("web · sh runs a program · press x again to quit")
        );
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn the_first_press_says_which_agent_works_or_asks() {
        use crate::agent::State;
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let (tx, rx) = mpsc::channel::<Msg>();
        let fake = Fake("exec sleep 30");
        let none = anyhow::anyhow!("no instance directory");
        let starter = Starter {
            agent: &fake,
            instances: Err(&none),
            tx: &tx,
        };
        let mut sessions = Sessions::new();
        let size = Size { rows: 2, cols: 10 };
        let mut ids = Vec::new();
        for name in ["api", "web"] {
            let dir = base.join(name);
            fs::create_dir(&dir).expect("mkdir");
            let (session, _) = {
                let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
                let place = Place::new(dir, None, None);
                starter
                    .start(sessions.next_id(), place, Vec::new(), false, size)
                    .expect("starts")
            };
            ids.push(session.id);
            sessions.push(session);
        }
        let (api, web) = (ids[0], ids[1]);
        let set = |sessions: &mut Sessions, id, state| {
            sessions.show(id, false);
            sessions.shown_mut().status.state = state;
        };
        // Waiting, or not said yet: nothing to ask.
        set(&mut sessions, web, Some(State::Waiting));
        assert_eq!(confirmation(&sessions, Ending::Quit), None);
        assert_eq!(confirmation(&sessions, Ending::Close(web)), None);
        // q asks for any agent that works or asks; x for its own.
        set(&mut sessions, api, Some(State::Attention));
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("api is asking · press q again to quit")
        );
        assert_eq!(
            confirmation(&sessions, Ending::Close(api)).as_deref(),
            Some("api is asking · press x again to close it")
        );
        assert_eq!(confirmation(&sessions, Ending::Close(web)), None);
        set(&mut sessions, web, Some(State::Working));
        assert_eq!(
            confirmation(&sessions, Ending::Quit).as_deref(),
            Some("2 agents are working or asking · press q again to quit")
        );
        // On the last session, x quits, and says so.
        sessions.show(api, false);
        assert!(sessions.close(api, false, Instant::now()));
        assert_eq!(
            confirmation(&sessions, Ending::Close(web)).as_deref(),
            Some("web is working · press x again to quit")
        );
        assert_eq!(confirmation(&sessions, Ending::Close(api)), None, "gone");
        // An agent that ended stops nothing, whatever its telemetry said
        // last.
        let exits = Fake("exit 3");
        let dir = base.join("old");
        fs::create_dir(&dir).expect("mkdir");
        let (session, _) = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            let starter = Starter {
                agent: &exits,
                ..starter
            };
            let place = Place::new(dir, None, None);
            starter
                .start(sessions.next_id(), place, Vec::new(), false, size)
                .expect("starts")
        };
        let old = session.id;
        sessions.push(session);
        let deadline = Instant::now() + Duration::from_secs(10);
        while sessions
            .get(old)
            .is_some_and(|session| session.agent.life.alive())
        {
            assert!(Instant::now() < deadline, "ends before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            sessions.settle(Instant::now(), false);
        }
        set(&mut sessions, web, Some(State::Waiting));
        set(&mut sessions, old, Some(State::Working));
        assert_eq!(confirmation(&sessions, Ending::Close(old)), None);
        assert_eq!(confirmation(&sessions, Ending::Quit), None);
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
