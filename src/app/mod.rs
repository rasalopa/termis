//! The application: wires the agent sessions (`session`), their
//! repositories (`repo`), the host input and the key encoder together.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::event::{
    Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::Size;
use crate::agent;
use crate::emu;
use crate::git::find::{Finder, Found};
use crate::git::push::Push;
use crate::git::{self, worker::Snapshot};
use crate::input::{self, Reader};
use crate::instance::Root;
use crate::keys::{self, ModeKey, Modes, Tracking};
use crate::projects::walk::{self, Walked};
use crate::pty::{self, ExitStatus};
use crate::strings;
use crate::ui::changes::{Movement, Step};
use crate::ui::commit::Edit;
use crate::ui::layout::{Sides, Widths};
use crate::ui::picker::{Item, Picker};
use crate::ui::toast::{self, Toast};
use crate::ui::{self, Mode, Pane};

mod changes;
mod config;
mod copy;
pub mod host;
mod open;
mod repo;
mod restore;
mod select;
mod session;
mod shell;
mod touched;
mod tree;
mod viewer;

use config::Config;
use host::Host;
use open::{Lookup, Roots, Target};
use repo::{Repo, RepoId, Repos};
use restore::{Kept, Reopen};
use select::{CLICK_WITHIN, Clicks, Over, Pointer, Selection};
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
/// it would stop is working or asking, and of `p`; the notice asking for
/// it lasts as long.
const CONFIRM_WITHIN: Duration = Duration::from_secs(3);
/// How long after the host window gains the focus a click still changes
/// no mode, so that the click that brings the window forward does not.
const FOCUS_SETTLES: Duration = Duration::from_millis(300);
/// How long quitting waits, at most, from the quit, for the agents
/// writing a subject to be killed with their groups, and for a copy to
/// the clipboard to end before its program is killed.
const STOP_GRACE: Duration = Duration::from_millis(500);

/// Everything the main loop receives. Pty events carry the session and
/// the tab that produced them and the generation of the tab's launch, so
/// a relaunch can ignore stragglers; git events the repository whose
/// worker sent them, the end of a push the repository it pushed from,
/// and the subject the agent wrote the repository whose draft it is for;
/// the finder's answers and the walks' the directory they answer; a file
/// read for a viewer the session, the viewer's tab and the read it
/// answers. The end of a copy carries the notice saying what was
/// copied.
enum Msg {
    Pty(TabKey, pty::Event),
    Input(input::Event),
    Git(RepoId, git::worker::Event),
    Pushed(RepoId, Result<()>),
    Suggested(RepoId, Result<String>),
    Found(Found),
    Walked(Walked),
    Read(ReadKey, ui::viewer::Content),
    Copied(Result<String>),
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
    /// Whether the changes pane is on screen while the side panes are, as
    /// [`changes::Showing`] says; else the center takes its width.
    changes: bool,
    /// The widths of the side panes, as the config file sets them.
    widths: Widths,
}

impl Layout {
    fn area(self) -> ratatui::layout::Rect {
        ratatui::layout::Rect::new(0, 0, self.terminal.cols, self.terminal.rows)
    }

    /// The side panes on screen.
    fn sides(self) -> Sides {
        match (self.side_panes(), self.changes) {
            (false, _) => Sides::None,
            (true, false) => Sides::Projects,
            (true, true) => Sides::Both,
        }
    }

    /// The whole center pane, where the wheel moves the agent's view: the
    /// screen itself when the agent is drawn over the whole terminal.
    fn agent_pane(self) -> Rect {
        if self.full_screen {
            self.area()
        } else {
            ui::layout::panes_with(self.area(), self.sides(), self.widths).center
        }
    }

    /// The agent's screen inside the center pane, where a position the
    /// agent is told about is counted from.
    fn agent_screen(self) -> Rect {
        ui::agent_area(self.area(), self.full_screen, self.sides(), self.widths)
    }

    /// The rows of the agent's screen drawn, where text is selected: all
    /// but the last while a line covers it, the bar of termis mode over
    /// the whole terminal outside agent mode `mode`, or the line saying
    /// the agent `ended`.
    fn drawn_screen(self, mode: Mode, ended: bool) -> Rect {
        let screen = self.agent_screen();
        let covered = ended || (self.full_screen && mode != Mode::Agent);
        Rect {
            height: screen.height.saturating_sub(u16::from(covered)),
            ..screen
        }
    }

    /// The whole changes pane, while it is on screen.
    fn changes_pane(self) -> Option<Rect> {
        (self.sides() == Sides::Both)
            .then(|| ui::layout::panes_with(self.area(), Sides::Both, self.widths).right)
    }

    /// The whole projects pane, while the side panes are on screen.
    fn projects_pane(self) -> Option<Rect> {
        self.side_panes()
            .then(|| ui::layout::panes_with(self.area(), self.sides(), self.widths).left)
    }

    /// Whether the terminal is wide enough for the side panes.
    fn wide(self) -> bool {
        ui::layout::sides_visible(self.area())
    }

    /// Whether the side panes are on screen, the changes pane among them
    /// or not: never in full-screen mode or zoomed, and only when wide
    /// enough otherwise.
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

/// What a click on a row or a tab's name the last frame drew acts on,
/// kept by what it is rather than where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Spot {
    /// A row of the list of sessions.
    Row(RowId),
    /// An entry of the file tree of session `SessionId`: its path,
    /// relative to the tree's directory, and whether it is a directory.
    Entry(SessionId, PathBuf, bool),
    /// A file of the lists of the changes pane while it showed
    /// repository `RepoId`.
    Change(RepoId, ui::changes::Selected),
    /// Tab `TabId` of session `SessionId`, named in the center pane's
    /// title.
    Tab(SessionId, TabId),
}

/// Where the last frame drew what a click can land on, and the layout
/// it was drawn in.
#[derive(Debug, Default)]
struct Spots {
    /// The layout the frame was drawn in; none before the first frame.
    layout: Option<Layout>,
    /// Each spot with the cells it took.
    spots: Vec<(Rect, Spot)>,
}

impl Spots {
    /// The spots of `targets`, which a frame drew in `layout` from
    /// `sessions` as they are now: each row of the list of sessions but
    /// the one counting the repositories left out, which the keys pass
    /// over; each entry of the file tree of the session shown; each file
    /// of the repository its changes pane shows; each of its tabs named
    /// in the center pane's title.
    fn new(targets: &ui::Targets, layout: Layout, sessions: &Sessions) -> Self {
        let shown = sessions.shown();
        let rows = sessions.rows();
        let spots = targets
            .iter()
            .filter_map(|(area, target)| {
                let spot = match target {
                    ui::Target::Listed(index) => rows
                        .get(*index)
                        .filter(|row| !matches!(row, RowId::More(_)))
                        .map(|&row| Spot::Row(row)),
                    ui::Target::Entry(path, dir) => Some(Spot::Entry(shown.id, path.clone(), *dir)),
                    ui::Target::Change(selected) => shown
                        .changes()
                        .map(|repo| Spot::Change(repo, selected.clone())),
                    ui::Target::Tab(tab) => {
                        let at = tab.map_or(0, |index| index + 1);
                        shown.tab_ids().get(at).map(|&id| Spot::Tab(shown.id, id))
                    }
                };
                spot.map(|spot| (*area, spot))
            })
            .collect();
        Self {
            layout: Some(layout),
            spots,
        }
    }

    /// The spot on the cell at `position` while the screen is in
    /// `layout`; none in another layout than the frame's.
    fn at(&self, position: Position, layout: Layout) -> Option<&Spot> {
        if self.layout != Some(layout) {
            return None;
        }
        self.spots
            .iter()
            .find(|(area, _)| area.contains(position))
            .map(|(_, spot)| spot)
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

/// What the changes pane of the session shown needs in `mode`, as
/// [`changes::need`] says, with what it shows: its repository's snapshot
/// counts once [`Repo::fresh`], so that one older than the last switch,
/// or than the return of the side panes, decides nothing.
fn changes_need(sessions: &Sessions, repos: &Repos, mode: Mode) -> (changes::Key, changes::Need) {
    let shown = sessions.shown();
    let key = (shown.id, shown.changes());
    let repo = key.1.and_then(|id| repos.get(id));
    let held = changes::held(
        mode,
        repo.is_some_and(|repo| repo.diff.is_some()),
        repo.is_some_and(Repo::busy),
    );
    let snapshot = repo
        .filter(|repo| repo.fresh())
        .and_then(|repo| repo.snapshot.as_ref());
    (key, changes::need(repo.is_some(), snapshot, held))
}

/// What a host event asks the app to do.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Quit,
    /// Close the tab under the list's cursor once it moved, else the tab
    /// shown: a terminal, the viewer, or an agent's with its session.
    /// Closing the last session quits. A repository an agent wrote in
    /// under the cursor goes off the list.
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
    /// Have the agent write a subject for the draft from what is staged.
    Suggest,
    /// Push the branch of the session shown, at the second press.
    Push,
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
    /// The left button at a place on the terminal, selecting text in the
    /// center pane.
    Selection(Pointer, Position),
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
    /// Show the session at this place on the list, from 0, and go to
    /// agent mode.
    SwitchTo(usize),
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
    /// agent's, with its session, or have the changes pane follow the
    /// repository an agent wrote in under it.
    Show,
    /// Show the file tree of the session shown in the projects pane, or
    /// the list of sessions again.
    Files,
    /// Put the cursor of the list of sessions or of the file tree, or the
    /// selection of the changes pane, on the row clicked; or show the tab
    /// whose name was clicked.
    Point(Spot),
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

/// A first press waiting for its second: what it acts on, an [`Ending`]
/// for `x` or `q` and the repository for `p`, until when the second
/// press counts, and the notice asking for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Armed<T> {
    what: T,
    until: Instant,
    notice: String,
}

/// The first press `armed` while its notice is the `toast` shown; none
/// once the notice went or another one took its place.
fn waiting<T>(armed: Option<Armed<T>>, toast: Option<&Toast>) -> Option<Armed<T>> {
    armed.filter(|armed| toast.is_some_and(|toast| toast.text == armed.notice))
}

/// Whether a press that ends `ending` at `now` acts: at once while
/// nothing it would stop is `busy`, else only as the second press of the
/// same within [`CONFIRM_WITHIN`] of the first, `armed`.
fn acts(armed: Option<&Armed<Ending>>, ending: Ending, busy: bool, now: Instant) -> bool {
    !busy || armed.is_some_and(|armed| armed.what == ending && now < armed.until)
}

/// What a press of `p` does.
#[derive(Debug, PartialEq, Eq)]
enum Pressed {
    /// Nothing, the repository not being read yet.
    Unread,
    /// Say why there is no push.
    Refused(String),
    /// Wait for a second press, this one being the first.
    Armed(Armed<RepoId>),
    /// Push.
    Push(Push),
}

/// What `p` pushes from `repo`, which the status bar then offers; else
/// what a press does instead: say why not while a push from it runs or
/// there is nothing to push, and nothing before its first read.
fn push_from(repo: &Repo) -> Result<Push, Pressed> {
    if repo.pushing.is_some() {
        return Err(Pressed::Refused(strings::PUSH_RUNNING.to_owned()));
    }
    let Some(snapshot) = &repo.snapshot else {
        return Err(Pressed::Unread);
    };
    let branch = &snapshot.status.branch;
    git::push::plan(branch, snapshot.origin, snapshot.push_remote.as_deref())
        .map_err(|why| Pressed::Refused(strings::nothing_to_push(why, branch.upstream.as_deref())))
}

/// What a press of `p` at `now` does in the repository of the session
/// shown, `repo` with its id, `first` being a first press waiting for
/// its second: say why not while there is no repository, or what
/// [`push_from`] says while there is no push; else push when it is the
/// second press of the same push, its notice unchanged, within
/// [`CONFIRM_WITHIN`] of the first, or wait for one, naming what goes
/// where.
fn press_push(
    repo: Option<(RepoId, &Repo)>,
    first: Option<&Armed<RepoId>>,
    now: Instant,
) -> Pressed {
    let Some((id, repo)) = repo else {
        return Pressed::Refused(strings::NOT_A_REPOSITORY.to_owned());
    };
    let push = match push_from(repo) {
        Ok(push) => push,
        Err(pressed) => return pressed,
    };
    let notice = strings::confirm_push(&push);
    match first {
        Some(first) if first.what == id && first.notice == notice && now < first.until => {
            Pressed::Push(push)
        }
        _ => Pressed::Armed(Armed {
            what: id,
            until: now + CONFIRM_WITHIN,
            notice,
        }),
    }
}

/// Lets a first press of `p` waiting for its second, `armed`, go when
/// a key that does not push was pressed, as `key` and its `action`
/// tell, with its notice when that is the `toast` shown.
fn disarm_push(
    armed: &mut Option<Armed<RepoId>>,
    toast: &mut Option<Toast>,
    key: bool,
    action: &Action,
) {
    if !key || *action == Action::Push {
        return;
    }
    if let Some(armed) = armed.take()
        && toast
            .as_ref()
            .is_some_and(|toast| toast.text == armed.notice)
    {
        *toast = None;
    }
}

/// What `x` ends on `row` of the list of sessions: its terminal or its
/// viewer, or its session on the agent's tab; nothing on the row of a
/// repository an agent wrote in, nor on the count of those left out.
fn close_ending(row: RowId) -> Option<Ending> {
    match row {
        RowId::Tab { session, tab } if tab.is_agent() => Some(Ending::Close(session)),
        RowId::Tab { session, tab } => Some(Ending::CloseTab(session, tab)),
        RowId::Repo { .. } | RowId::More(_) => None,
    }
}

/// Takes repository `repo` off the list of those the agent of session
/// `id` wrote in, for `x` on its row, as [`open::forget_written`] does.
/// Returns the row the list's cursor goes to: the session's repository
/// row that takes its place, else the one above it, so that another `x`
/// takes another repository off rather than ending something; none, for
/// the row of the tab shown, when the session has no other.
fn unlist(
    sessions: &mut Sessions,
    repos: &mut Repos,
    id: SessionId,
    repo: RepoId,
) -> Option<RowId> {
    let row = RowId::Repo { session: id, repo };
    let at = sessions.rows().iter().position(|&listed| listed == row)?;
    open::forget_written(sessions, repos, id, repo);
    let rows = sessions.rows();
    let written = |at: usize| {
        rows.get(at)
            .copied()
            .filter(|row| matches!(row, RowId::Repo { session, .. } if *session == id))
    };
    written(at).or_else(|| written(at.checked_sub(1)?))
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

/// Whether `ending`, once it acts and quits, leaves no session for the
/// next start to reopen: closing the last one with `x` does, while `q`
/// leaves them all.
fn forgets(ending: Ending) -> bool {
    matches!(ending, Ending::Close(_))
}

/// The keeper of `file`, which keeps the sessions of the last run, and
/// where they open around the directory `options` ask for, with `home`
/// as `~`: with `--spike` there is no file to read or write, and with
/// `--no-restore` none of them opens.
fn last_run(file: Option<PathBuf>, options: &Options<'_>, home: Option<&Path>) -> (Kept, Reopen) {
    let file = file.filter(|_| !options.full_screen);
    let (kept, saved) = Kept::open(file, home, options.restore);
    (kept, Reopen::new(saved, options.cwd, home))
}

/// Saves the directories of the sessions open, in the order of the list,
/// in `kept` for the next start when they are not what it keeps; none
/// once the user chose to `forget` them. Returns why they could not be
/// saved.
fn keep(kept: &mut Kept, sessions: &Sessions, forget: bool) -> Option<String> {
    let dirs = sessions
        .iter()
        .filter(|_| !forget)
        .map(|session| session.place.cwd.as_path());
    kept.keep(dirs)
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
    /// Reopen the sessions of the last run around the one in `cwd`;
    /// else the sessions open replace them in the file that keeps them.
    pub restore: bool,
    /// Draw the agent over the whole terminal, with no side panes and one
    /// session; the sessions of the last run are neither reopened nor
    /// saved.
    pub full_screen: bool,
    /// Debug aid: panic from a worker thread after this many seconds.
    pub panic_after: Option<u64>,
}

/// Runs the agent, in the center pane or over the whole terminal. The
/// termis-mode key, `Ctrl+Q` unless the config file sets another, enters
/// termis mode, where `q` quits, `o` opens another project in a
/// session of its own, `t` a terminal in the session shown, and `x`
/// closes the tab shown, the agent's with its session, or the one under
/// the cursor of the projects pane's list, quitting on the last;
/// while an agent they would stop is working or asking, or a program
/// runs in a terminal they would close, `x` and `q` take a second press.
/// Once the agent ended, `r` relaunches it and `x` or `q` close its
/// session, quitting on the last. The sessions open when `termis` last
/// ran open again around the one in `cwd`, which is shown, unless the
/// options say otherwise, and the set of sessions open is saved for the
/// next start as it changes. The config file is read first, and one that
/// cannot be taken fails the run before anything starts. Returns the
/// exit status of the shown session's current launch when it exited on
/// its own before the user quit.
pub fn run(options: Options<'_>) -> Result<Option<ExitStatus>> {
    // The repository's root is a physical path, so the home is resolved
    // the same way.
    let home = std::env::home_dir().map(|home| home.canonicalize().unwrap_or(home));
    // The config file first: one termis cannot take stops it before git
    // is looked for and the screen is taken.
    let config = match config::file(
        std::env::var_os(config::CONFIG_HOME_VAR),
        std::env::var_os(restore::HOME_VAR),
    ) {
        Some(file) => config::read(&file).with_context(|| {
            format!(
                "read {}",
                ui::projects::display_path(&file, home.as_deref())
            )
        })?,
        None => Config::default(),
    };
    let agent = agent::default_agent();
    // Outside a repository the agent still runs; the changes pane says so.
    // Without git at all, termis does not start: the right pane needs it.
    // Checked once; the git worker refreshes it from M3 on.
    let repo = git::repo_root(options.cwd)?;
    let place = Place::new(options.cwd.to_path_buf(), repo, home.as_deref());
    // The sessions of the last run, read from the file that keeps them.
    let file = restore::file(
        std::env::var_os(restore::STATE_HOME_VAR),
        std::env::var_os(restore::HOME_VAR),
    );
    let (mut kept, last) = last_run(file, &options, home.as_deref());
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
    // The changes pane is off until the repository it shows is read.
    let mut showing = changes::Showing::default();
    let mut layout = Layout {
        terminal: terminal_size(&host)?,
        full_screen: options.full_screen,
        zoomed: false,
        changes: false,
        widths: config.widths,
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let reader = Reader::spawn(tx.clone())?;
    let finder = Finder::spawn(tx.clone(), Msg::Found)?;
    let mut repos = Repos::new(config.poll);
    let starter = Starter {
        agent: agent.as_ref(),
        instances: instances.as_ref(),
        tx: &tx,
    };
    // Without an instance the agent runs without telemetry, and a toast
    // says why. The session asked for starts first: when it cannot,
    // termis does not start.
    let (first, telemetry_off) = starter.start(
        sessions.next_id(),
        place,
        options.args.to_vec(),
        options.new_conversation,
        layout.agent_size(),
    )?;
    // The sessions of the last run start at once, as `o` starts them, in
    // the order of the list, with the one asked for in its place among
    // them, shown. Nothing that can fail runs while that one is off the
    // list, so it is dropped after the host. Of what went wrong as they
    // opened, the first thing shows in a toast, the agent status first:
    // without an instance directory every session runs without it.
    let mut notices: Vec<String> = telemetry_off
        .map(|reason| strings::telemetry_trouble(&reason))
        .into_iter()
        .collect();
    notices.extend(open::open_around(
        &starter,
        &mut sessions,
        &mut repos,
        first,
        last,
        layout.agent_size(),
    ));
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
    // The first row of the list of sessions the last frame drew, where
    // the next one starts.
    let mut list_scroll = 0;
    // Where the last frame drew the rows a click moves a cursor to, and
    // the tabs' names a click shows.
    let mut spots = Spots::default();
    // When the last click on a tab's name was, or the last press that
    // `repeats_tab_click` dropped after one.
    let mut tab_clicked: Option<Instant> = None;
    let mut time_shown: Option<String> = None;
    let mut telemetry_next = Instant::now();
    let mut toast: Option<Toast> = notices.into_iter().next().map(trouble);
    // Whether `x` closed the last session, which quits and leaves none to
    // reopen at the next start.
    let mut forget = false;
    // Only true in termis mode: every way out of it clears it.
    let mut help = false;
    // A first press of `x` or `q` that waits for its second while its
    // notice is up.
    let mut armed: Option<Armed<Ending>> = None;
    // A first press of `p` that waits for its second while its notice is
    // up and no other key comes.
    let mut push_armed: Option<Armed<RepoId>> = None;
    if let Some(seconds) = options.panic_after {
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            panic!("termis --panic-after {seconds}: deliberate panic");
        });
    }

    let mut mode = Mode::default();
    let mut side_panes = layout.side_panes();
    let mut dirty = true;
    // Whether the next frame draws every cell, over what a hook or ssh
    // wrote on the terminal while a push ran, or the agent while it wrote
    // a subject.
    let mut repaint = false;
    let mut held = Held::default();
    // The text selected in the center pane, the presses that started
    // selections, for double and triple clicks, and the copies of what
    // they selected.
    let mut selection: Option<Selection> = None;
    let mut clicks = Clicks::default();
    let mut clipboard = copy::Clipboard::default();
    // Whether the host window has the focus, as its focus events last
    // said; it has until one says otherwise. When it last gained it, so
    // that the click that brings it forward changes no mode.
    let mut host_focused = true;
    let mut focus_gained: Option<Instant> = None;
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
            // While the window does not have the focus, any agent that
            // starts asking or finishes its turn says so in a desktop
            // notification, between two frames.
            for text in notifications(&read.turns, host_focused) {
                host.notify(&text)?;
            }
        }
        // The list of sessions shows beside the changes pane from two
        // sessions on, or while the session shown has repositories its
        // agent wrote in, unless the session shown has its file tree in
        // its place. The repository its changes pane shows is refreshed,
        // with the pane hidden too, so that it can come back, and so is
        // its own while its tree shows its files, every poll interval and
        // after a change to their files; the repositories whose dot the
        // list has are read for it.
        let shown = sessions.shown();
        let tree_shown = side_panes && shown.tree.shown;
        let list_shown = side_panes && sessions.listed() && !tree_shown;
        let read = [shown.changes(), shown.repo.filter(|_| tree_shown)];
        let listed = |id| list_shown && sessions.lists(id);
        repos.read_when_due(now, read, side_panes, listed);
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
            // New output in the tab shown ends the selection there, once
            // its button is released.
            if output_ends(&msg, shown.id, shown.shown_tab(), selection.as_ref()) {
                selection = None;
            }
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
                Msg::Pushed(id, result) => {
                    let shown = sessions.shown().changes();
                    if let Some(notice) = repos.pushed(id, result, shown, Instant::now()) {
                        toast = Some(notice);
                        dirty = true;
                        repaint = true;
                    }
                }
                // The subject lands in the draft, the box open or not.
                Msg::Suggested(id, result) => {
                    let shown = sessions.shown().changes();
                    let applied = repos.suggested(id, result, shown, Instant::now());
                    dirty |= applied.dirty;
                    repaint |= applied.dirty;
                    if applied.toast.is_some() {
                        toast = applied.toast;
                    }
                }
                Msg::Walked(walked) => {
                    if sessions.dir_files(&walked, Instant::now()) {
                        dirty |= side_panes && sessions.shown().tree.shown;
                    }
                }
                // An answer for a read no viewer waits for is dropped; the
                // viewer shown that takes one has new text, which ends
                // the selection there.
                Msg::Read(key, content) => {
                    if sessions.file_read(key, content) {
                        dirty = true;
                        selection = None;
                    }
                }
                Msg::Pty(key, event) => sessions.on_pty(key, event, Instant::now()),
                // A copy says how it went.
                Msg::Copied(copied) => {
                    toast = Some(match copied {
                        Ok(notice) => {
                            Toast::new(notice, toast::Kind::Info, Instant::now(), TOAST_LASTS)
                        }
                        Err(error) => trouble(strings::copy_failed(&format!("{error:#}"))),
                    });
                }
                // The directory chosen in the picker that waited for it opens
                // while that picker is still open; once it closed, the
                // choice is dropped. A directory an agent wrote in whose
                // repository git could not tell is let go.
                Msg::Found(found) => {
                    if found.root.is_err() {
                        touched::forget(&mut sessions, &found.dir);
                    }
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
                            Err(error) => Err(strings::for_project(
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
                    host_focused = focused_after(host_focused, &event);
                    focus_gained = gained_after(focus_gained, &event, Instant::now());
                    let typed = matches!(event, HostEvent::Key(_) | HostEvent::Paste(_));
                    let key_pressed =
                        matches!(&event, HostEvent::Key(key) if key.kind == KeyEventKind::Press);
                    let mouse = match &event {
                        HostEvent::Mouse(mouse) => Some(*mouse),
                        _ => None,
                    };
                    let shown = sessions.shown();
                    // None on the viewer, which has no child.
                    let tab = shown.tab();
                    // The repository the changes pane shows.
                    let repo_id = shown.changes();
                    let mut repo = repo_id.and_then(|id| repos.get_mut(id));
                    let context = KeyContext {
                        mode,
                        side_panes,
                        full_screen: layout.full_screen,
                        sessions: sessions.iter().count(),
                        list: sessions.listed(),
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
                        projects_pane: layout.projects_pane(),
                        focus_settled: focus_settled(host_focused, focus_gained, Instant::now()),
                        alternate_screen: tab.is_some_and(|tab| tab.emu.alternate_screen()),
                        scrolled: tab.is_some_and(|tab| tab.emu.scrolled() > 0),
                        held,
                        selecting: selection.as_ref().is_some_and(Selection::held),
                        mode_key: config.mode_key,
                    };
                    // A click on a row or a tab's name the last frame drew
                    // points at it; the second press of a double click on a
                    // tab's name does nothing.
                    let (action, repeated, clicked_at) =
                        route(event, context, &spots, layout, tab_clicked, Instant::now());
                    tab_clicked = clicked_at;
                    // What the picker or the list of sessions chose, and
                    // the session `n`, `N` or `Alt+1` to `Alt+9` lead to,
                    // acted on once the shown repository is done with.
                    let mut chosen = None;
                    let mut switch = None;
                    // What `x` or `q` ends, once the shown repository is
                    // done with.
                    let mut ending = None;
                    // The repository the changes pane of a session is to
                    // follow, none for its own, with whether the pane
                    // shows for it, and the one `x` takes off a session's
                    // list, once the shown repository is done with.
                    let mut follow = None;
                    let mut unlisted = None;
                    if let Some(mouse) = mouse {
                        held = held.after(mouse.kind, &action);
                        // What the agent gets shows once it draws.
                        dirty |= !matches!(action, Action::Nothing | Action::Send(_));
                    }
                    // A press over the center pane ends the selection there,
                    // and so do a key or a paste sent to the tab, a scroll
                    // of its view and a relaunch.
                    let pressed = mouse.is_some_and(|mouse| presses_center(&mouse, &context));
                    if (pressed || clears_selection(&action, typed)) && selection.take().is_some() {
                        dirty = true;
                    }
                    // Any command closes the list; the command still runs.
                    if help && closes_help(&action) {
                        help = false;
                    }
                    disarm_push(&mut push_armed, &mut toast, key_pressed, &action);
                    // A click sets the mode before the press acts, so that
                    // a selection it starts is over the screen drawn in the
                    // new mode; the press dropped after a tab click sets
                    // none.
                    if let Some(focus) = mouse
                        .filter(|_| !repeated)
                        .and_then(|mouse| clicked(&mouse, &context))
                    {
                        mode = focus;
                        dirty = true;
                    }
                    match action {
                        Action::Quit => ending = Some(Ending::Quit),
                        Action::Close => {
                            let row = sessions.cursor_row(cursor);
                            if let RowId::Repo { session, repo } = row {
                                unlisted = Some((session, repo));
                            }
                            ending = close_ending(row);
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
                        // A push runs off the main thread; its end comes
                        // back as a message.
                        Action::Push if side_panes => {
                            let now = Instant::now();
                            let first = waiting(push_armed.take(), toast.as_ref());
                            let shown_repo = repo_id.zip(repo.as_deref());
                            match press_push(shown_repo, first.as_ref(), now) {
                                Pressed::Unread => {}
                                Pressed::Refused(text) => toast = Some(trouble(text)),
                                Pressed::Armed(waits) => {
                                    toast = Some(Toast::new(
                                        waits.notice.clone(),
                                        toast::Kind::Error,
                                        now,
                                        CONFIRM_WITHIN,
                                    ));
                                    push_armed = Some(waits);
                                }
                                Pressed::Push(push) => {
                                    // The notice asking for it goes.
                                    toast = None;
                                    let project = sessions.shown().changes_name().to_owned();
                                    if let Some(repo) = &mut repo
                                        && let Err(error) = repo.push(push, project, &tx)
                                    {
                                        toast = Some(trouble(format!("{error:#}")));
                                    }
                                }
                            }
                        }
                        Action::Push => {}
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
                        // A press starts a selection on what the tab shown
                        // shows, a word at a second press on its cell and a
                        // line at a third; its drags and its release move
                        // it. Its text goes to the clipboard at the
                        // release, off the main thread.
                        Action::Selection(pointer, at) => {
                            let shown = sessions.shown();
                            let over = over_shown(shown, layout, mode);
                            if let Some(grid) = ui::select::Grid::new(shown.center(), over.screen) {
                                match pointer {
                                    Pointer::Press => {
                                        let unit = clicks.press(at, Instant::now());
                                        selection = Some(Selection::press(over, &grid, at, unit));
                                    }
                                    Pointer::Drag => {
                                        if let Some(selection) = &mut selection {
                                            selection.drag(&grid, at);
                                        }
                                    }
                                    Pointer::Release => {
                                        selection = selection
                                            .take()
                                            .and_then(|selection| selection.release(&grid, at));
                                        if let Some(marked) =
                                            selection.as_ref().and_then(Selection::marked)
                                        {
                                            let text = grid.text(marked);
                                            let program = Path::new(copy::PBCOPY);
                                            let tx = tx.clone();
                                            if let Err(error) =
                                                clipboard.copy(program, text, tx, Msg::Copied)
                                            {
                                                let text = format!("{error:#}");
                                                toast = Some(trouble(strings::copy_failed(&text)));
                                            }
                                        }
                                    }
                                }
                            }
                        }
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
                        Action::SwitchTo(index) => switch = switch_to(&sessions, index, &mut mode),
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
                        // session shows as if the picker chose it; the
                        // session's own row brings its changes pane back
                        // to its own repository, and shows the pane when
                        // the session was already shown. A repository's
                        // row has the changes pane follow it.
                        Action::Show => match sessions.cursor_row(cursor) {
                            RowId::Tab { session, tab } => {
                                let was_shown = session == sessions.shown().id;
                                sessions.show_tab(session, tab, host_focused);
                                chosen = Some((Target::Session(session), false));
                                if tab.is_agent() {
                                    follow = Some((session, None, was_shown));
                                }
                            }
                            RowId::Repo { session, repo } => {
                                follow = Some((session, Some(repo), true));
                            }
                            RowId::More(_) => {}
                        },
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
                        // A file shown in the viewer ends the selection
                        // there; a directory unfolded leaves it.
                        Action::TreeEnter => {
                            if sessions.shown().tree.file().is_some() {
                                selection = None;
                            }
                            if tree_enter(sessions.shown_mut(), &tx, host_focused) {
                                held = Held::default();
                            }
                        }
                        Action::TreeClose => sessions.shown_mut().tree.close(),
                        // On the row clicked, as the keys move them there;
                        // `clicked` highlighted its pane.
                        Action::Point(Spot::Row(row)) => cursor = Some(row),
                        Action::Point(Spot::Entry(session, ref path, dir)) => {
                            if session == sessions.shown().id {
                                sessions.shown_mut().tree.point(path, dir);
                            }
                        }
                        Action::Point(Spot::Change(id, ref selected)) => {
                            if repo_id == Some(id)
                                && let Some(repo) = &mut repo
                            {
                                repo.point(selected);
                            }
                        }
                        // As `,` and `.` show it, in agent mode, as `clicked`
                        // says of a click on the center pane.
                        Action::Point(Spot::Tab(session, tab)) => {
                            if session == sessions.shown().id
                                && sessions.shown_mut().show_tab(tab, host_focused)
                            {
                                held = Held::default();
                            }
                        }
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
                        // The agent writes off the main thread; its answer
                        // comes back as a message.
                        Action::Suggest => {
                            if let Some(repo) = &mut repo {
                                if let Some(text) = suggest_refusal(repo, side_panes) {
                                    toast = Some(refusal(text));
                                } else if let Some(ask) = agent.subject() {
                                    let project = sessions.shown().changes_name().to_owned();
                                    if let Err(error) = repo.suggest(ask, project, &tx) {
                                        toast = Some(trouble(format!("{error:#}")));
                                    }
                                } else {
                                    let text = strings::cannot_suggest(agent.name());
                                    toast = Some(trouble(text));
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
                    // `n` and `N` keep termis mode, `Alt+1` to `Alt+9` left
                    // it; no button stays held over the agent hidden, and
                    // the list's cursor starts over on the session shown.
                    if let Some(id) = switch
                        && open::show(&mut sessions, &mut repos, id, host_focused)
                    {
                        held = Held::default();
                        cursor = None;
                    }
                    // The changes pane follows the repository picked in the
                    // list, highlighted, its session shown; the session's
                    // own row brings it back to its own repository. Either
                    // way it shows for 10 s at least, with a repository to
                    // show, unless the row was another session's own,
                    // which goes by its state as `n` does. `x` on a
                    // repository's row takes it off the list, the cursor
                    // going to the repository's row in its place.
                    if let Some((id, repo, shows)) = follow {
                        open::show(&mut sessions, &mut repos, id, host_focused);
                        open::follow(&mut sessions, &mut repos, id, repo);
                        let shown = sessions.shown();
                        if shows && shown.changes().is_some() {
                            showing.show((shown.id, shown.changes()), Instant::now());
                        }
                        if repo.is_some() {
                            mode = Mode::Term(Pane::Changes);
                            held = Held::default();
                            cursor = None;
                        }
                    }
                    if let Some((id, repo)) = unlisted {
                        cursor = unlist(&mut sessions, &mut repos, id, repo);
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
                                    forget = forgets(ending);
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
                                what: ending,
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
        // While the changes pane is highlighted, the repository it shows
        // stays, as if picked, even in a session outside any repository,
        // which shows the last one its agent wrote in otherwise: what the
        // keys stage or commit is what is on screen.
        if matches!(mode, Mode::Term(Pane::Changes) | Mode::Commit) {
            sessions.shown_mut().hold();
        }
        // The directories the agents wrote in find their repositories in
        // what the finder answered before, or ask it; the list shows them.
        let (listed, unlisted) = touched::resolve(
            &mut sessions,
            &mut roots,
            |dir| finder.ask(dir),
            |root| repos.get_or_spawn(root, &tx),
        );
        dirty |= listed && side_panes;
        if let Some(text) = unlisted {
            toast = Some(trouble(text));
            dirty = true;
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
        // its repository once no session uses it and no commit or push
        // runs in it.
        sessions.move_closing(Instant::now());
        repos.release_unused(|id| sessions.uses(id));
        // The changes pane shows while the repository it shows has a file
        // not committed or a commit to push, while it is highlighted, its
        // commit box or a diff is open, or a commit, a push or a subject
        // of that repository runs, and for 10 s after. Another session or
        // repository, or the same once the side panes are back, goes by
        // its own state at once, from its first refresh since; until then
        // the pane stays as it was. Each change of the center's width
        // resizes every tab, as `z` does.
        let (key, need) = changes_need(&sessions, &repos, mode);
        let on = showing.update(key, need, Instant::now());
        if on != layout.changes {
            layout.changes = on;
            if let Some(text) = apply_layout(layout, &mut sessions, &mut side_panes, &mut mode) {
                toast = Some(trouble(text));
                dirty = true;
            }
            dirty |= side_panes;
        }
        // A selection ends once another tab is shown, or the rows of the
        // screen drawn moved or changed.
        let over = over_shown(sessions.shown(), layout, mode);
        if selection
            .as_ref()
            .is_some_and(|selection| selection.over() != over)
        {
            selection = None;
            dirty = true;
        }
        // The next start reopens the sessions open now: they are saved
        // when they are not what the file keeps, on the first pass when
        // it was not read or a saved one did not open, then as one opens
        // or `x` closes one, and as `x` closes the last one, which leaves
        // none; `q` leaves them.
        if let Some(text) = keep(&mut kept, &sessions, forget) {
            toast = Some(trouble(text));
            dirty = true;
        }
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
        // The repository the changes pane shows.
        let repo = shown.changes().and_then(|id| repos.get(id));
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
        // So does what the list of sessions, from two sessions on or with
        // repositories the agent shown wrote in, shows of the others: each
        // one's glyph and dot, whether a program runs in each terminal, and
        // the dot of each repository written in, while the list is on
        // screen, how many ask while it is hidden or the file tree is in
        // its place.
        let list_on_screen = side_panes && !shown.tree.shown;
        let listed = sessions.listed();
        let uncommitted = |id| repos.get(id).is_some_and(|repo| repo.uncommitted);
        let rows: Vec<ui::projects::Row<'_>> = if listed {
            sessions
                .iter()
                .flat_map(|session| {
                    let row = session.row(session.repo.is_some_and(uncommitted));
                    let tabs = session.tab_rows();
                    let written = session.written_rows(uncommitted, session.id == shown.id);
                    std::iter::once(ui::projects::Row::Session(row))
                        .chain(tabs.into_iter().map(ui::projects::Row::Terminal))
                        .chain(written)
                })
                .collect()
        } else {
            Vec::new()
        };
        let list = listed.then(|| {
            let ids = sessions.rows();
            let index = |row| ids.iter().position(|&listed| listed == row);
            let shown = index(sessions.shown_row()).unwrap_or_default();
            ui::projects::List {
                rows: &rows,
                shown,
                cursor: index(sessions.cursor_row(cursor)).unwrap_or(shown),
                scroll: list_scroll,
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
            // letters of the file tree, from the session's own repository.
            let tree_changes = shown
                .repo
                .and_then(|id| repos.get(id))
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
                marked: selection.as_ref().and_then(Selection::marked),
                mode,
                sides: layout.sides(),
                widths: layout.widths,
                zoomed: layout.zoomed,
                ended: ended_line.as_deref(),
                repo: repo.is_some(),
                written_repo: shown.written_changes(),
                changes: repo.and_then(|repo| repo.snapshot.as_ref()),
                selected: repo.and_then(|repo| repo.selected.as_ref()),
                scroll: repo.map_or(0, |repo| repo.scroll),
                draft: repo
                    .filter(|_| mode == Mode::Commit)
                    .map(|repo| &repo.draft),
                suggesting: repo.is_some_and(Repo::suggesting),
                toast: toast.as_ref(),
                help,
                diff: repo.and_then(|repo| repo.diff.as_ref()),
                card: shown.card(agent.name(), clock, session_time),
                list,
                tree,
                picker: picker.as_ref(),
                edit: editable(shown, mode.pane()).is_some(),
                push: side_panes && repo.is_some_and(|repo| push_from(repo).is_ok()),
                pushing: repo.is_some_and(|repo| repo.pushing.is_some()),
                mode_key: config.mode_key,
                palette: config.palette,
            };
            if repaint {
                host.repaint()?;
                repaint = false;
            }
            let (scrolls, targets) = draw(&mut host, layout.full_screen, &view)?;
            // The rows the frame drew, by what they are, while its indexes
            // still match the sessions.
            spots = Spots::new(&targets, layout, &sessions);
            pages = scrolls.pages;
            list_scroll = scrolls.list;
            let shown = sessions.shown_mut();
            if shown.tree.shown {
                shown.tree.folds.scroll = scrolls.tree;
            }
            if let Some(repo) = shown.changes().and_then(|id| repos.get_mut(id)) {
                repo.scroll = scrolls.changes;
            }
            dirty = false;
        }
    }

    // The agents writing a subject are killed while the sessions close.
    repos.stop_suggestions();
    let stopping = Instant::now() + STOP_GRACE;
    sessions.quit();
    drop(reader);
    drop(finder);
    let status = sessions.close_all(&rx);
    repos.await_suggestions(stopping);
    // A copy to the clipboard gets the same grace to land; one still
    // running past it is killed with its group.
    clipboard.finish(stopping);
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
    /// opens nothing and `Alt+1` to `Alt+9` reach the agent.
    full_screen: bool,
    /// How many sessions there are. `Alt+1` to `Alt+9` show the first
    /// nine; from two on, `n` and `N` show the others and `q` closes an
    /// ended agent's session rather than quit.
    sessions: usize,
    /// Whether the projects pane has the list of sessions, from two
    /// sessions on or with repositories the agent shown wrote in, for its
    /// keys to move through.
    list: bool,
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
    /// The center pane, where the wheel moves the agent's view and a
    /// click in termis mode goes back to agent mode.
    agent_pane: Rect,
    /// The agent's screen inside it, which mouse reports count from.
    agent_screen: Rect,
    /// The changes pane while the side panes are shown, where the wheel
    /// moves the selection or the diff and a click highlights it.
    changes_pane: Option<Rect>,
    /// The projects pane while the side panes are shown, which a click
    /// highlights.
    projects_pane: Option<Rect>,
    /// Whether the host window has had the focus long enough for a click
    /// to change the mode, as [`focus_settled`] says.
    focus_settled: bool,
    /// Whether the agent is on its alternate screen, which keeps no
    /// history: there the wheel becomes arrow keys for it.
    alternate_screen: bool,
    /// Whether the agent pane shows lines that scrolled off, where the
    /// cells under the pointer are not the agent's.
    scrolled: bool,
    /// The buttons pressed over the agent that are still held.
    held: Held,
    /// Whether the left button, pressed over the center pane where it
    /// selects text, is still held.
    selecting: bool,
    /// The termis-mode key, `Ctrl+Q` unless the config file sets another.
    mode_key: ModeKey,
}

/// Turns one host event into an action.
///
/// The termis-mode key, `Ctrl+Q` unless the config file sets another,
/// switches between agent mode and termis mode, and leaves the commit
/// box and the picker for agent mode; no child gets it. `Alt+1` to
/// `Alt+9` show a session in both modes, see [`switch_key`]. In agent
/// mode keys and pastes go to the child of the tab shown, except `Ctrl+Z`
/// on the agent's tab, which would make the agent try to suspend itself
/// and wait forever, and on the editor's, which `sh -c` runs without job
/// control to resume it; once the agent ended, `r` relaunches it, `x`
/// closes its session, and so does `q` while there are other sessions,
/// else it quits. On the viewer, which has no child, agent mode keeps the
/// scroll keys, see [`view_key`], and drops the other keys and pastes. In
/// termis mode keys are commands, see [`term_command`], and pastes are
/// dropped. With the commit box open keys and pastes edit the draft, see
/// [`commit_key`]; with the picker open, keys and the first line of a
/// paste its query, see [`picker_key`]. Host focus events describe the
/// window, so they reach the child shown in every mode. A child that
/// asked for the mouse gets, in every mode, the reports it asked for of
/// what happens over the center pane, presses and motion only while the
/// pane shows its live screen, and the drags and release of a button
/// pressed there wherever they happen. Otherwise a press of the left
/// button over the center pane selects text there, and the drags and the
/// release of that button move the selection wherever they happen; the
/// wheel over the center pane becomes arrow keys while the child is on
/// its alternate screen in agent mode, else it moves the view of the tab
/// shown. Over the changes pane the wheel moves the selection or the
/// diff. A press of the left button may also change the mode, as
/// [`clicked`] says; one on a row or a tab's name the last frame drew is
/// [`pointed`]'s instead. Under the key list or the picker the mouse only
/// finishes a drag the child saw begin, or a selection. Other mouse
/// events, key releases and repeats are ignored.
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
        selecting,
        mode_key,
        ..
    } = context;
    let mut bytes = Vec::new();
    match event {
        HostEvent::Key(key) if key.kind != KeyEventKind::Press => {}
        HostEvent::Key(key) if mode_key.matches(&key) => {
            return Action::Mode(match mode {
                Mode::Agent => Mode::Term(Pane::Agent),
                Mode::Term(_) | Mode::Commit | Mode::Picker => Mode::Agent,
            });
        }
        HostEvent::Key(key) if is_suspend(&key) && !(terminal && mode == Mode::Agent) => {}
        HostEvent::Key(key) => {
            if let Some(action) = switch_key(&key, &context) {
                return action;
            }
            match mode {
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
                    KeyCode::Char('q') if key.modifiers.is_empty() && context.sessions > 1 => {
                        return Action::Close;
                    }
                    KeyCode::Char('q') if key.modifiers.is_empty() => return Action::Quit,
                    _ => {}
                },
            }
        }
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
                    MouseEventKind::Drag(button) | MouseEventKind::Up(button)
                        if held.has(button) || (selecting && button == MouseButton::Left)
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
                // Elsewhere the left button selects text: a press over the
                // center pane starts it, and its drags and its release
                // follow wherever they happen.
                (_, None)
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) && over_agent =>
                {
                    return Action::Selection(Pointer::Press, at);
                }
                (_, None) if selecting && mouse.kind == MouseEventKind::Drag(MouseButton::Left) => {
                    return Action::Selection(Pointer::Drag, at);
                }
                (_, None) if selecting && mouse.kind == MouseEventKind::Up(MouseButton::Left) => {
                    return Action::Selection(Pointer::Release, at);
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

/// What `Alt+1` to `Alt+9`, with Alt the only modifier, do in agent and
/// termis mode as `context` says: show the session at that place on the
/// list, or nothing past the last one, so that they never reach the
/// child of the tab shown. Over the whole terminal, where there is one
/// session, they are keys like the others. None for any other key, and
/// in the commit box and the picker, whose keys drop them.
fn switch_key(key: &KeyEvent, context: &KeyContext) -> Option<Action> {
    let KeyCode::Char(digit @ '1'..='9') = key.code else {
        return None;
    };
    if key.modifiers != KeyModifiers::ALT
        || context.full_screen
        || matches!(context.mode, Mode::Commit | Mode::Picker)
    {
        return None;
    }
    let index = digit as usize - '1' as usize;
    Some(if index < context.sessions {
        Action::SwitchTo(index)
    } else {
        Action::Nothing
    })
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
/// with the list shown, from two sessions on or with repositories the
/// agent shown wrote in, they move the list's cursor and `Enter` shows
/// the tab under it, or has the changes pane follow the repository under
/// it. With the changes pane highlighted, they move its selection, `s`
/// and `u` stage and unstage the selected row, `a` stages everything,
/// `c` opens the commit box, `p` pushes the branch at its second press,
/// and `d` opens the diff of the selected row, which they and
/// `PgUp`/`PgDn`, shifted or not, then scroll and `d` or `Esc` close.
/// With the agent pane highlighted, they and `PgUp`/`PgDn` move its view
/// through its scrollback, `G` back to the live screen and `g` to the
/// oldest line kept; the alternate screen has no history to move
/// through. On the viewer they move through its file the same way, `G`
/// to its end and `g` to its top. `z` toggles the zoom when that changes
/// anything; unless the agent is drawn over the whole terminal, `o`
/// opens the project picker, `t` a terminal and `e`, while it has one, a
/// file in the user's editor; while the side panes are shown, `f` shows
/// the file tree in the projects pane in place of the list of sessions
/// and the card, or takes it away; `n` and `N`, shifted or not, show the
/// next and the previous session while there are others, `,` and `.` the
/// previous and the next tab while the session has others, `r`
/// relaunches an ended agent shown, `x` closes the tab shown, the
/// agent's with its session, or the one under the list's cursor with the
/// projects pane highlighted, where it takes a repository off the list,
/// `q` quits, `?` shows or hides the key list, which `Esc` closes first
/// while it is open. `i` returns to the agent as the termis-mode key
/// does, the diff closing with the mode, except while the key list is
/// open. Anything else is ignored.
fn term_command(key: &KeyEvent, pane: Pane, context: &KeyContext) -> Action {
    let KeyContext {
        side_panes,
        full_screen,
        sessions,
        list,
        tree,
        edit,
        other_tabs,
        zoomable,
        agent_alive,
        help,
        diff,
        ..
    } = *context;
    let other_sessions = sessions > 1;
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let moved = if diff {
        movement(key, true).map(Action::Scroll)
    } else {
        match pane {
            Pane::Agent => movement(key, true).map(Action::Scrollback),
            Pane::Changes => movement(key, false).map(Action::Select),
            Pane::Projects if tree => movement(key, false).map(Action::TreeCursor),
            Pane::Projects if list => movement(key, false).map(Action::Cursor),
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
        KeyCode::Enter if plain && pane == Pane::Projects && list => Action::Show,
        KeyCode::Char('s') if plain && pane == Pane::Changes => Action::Stage,
        KeyCode::Char('u') if plain && pane == Pane::Changes => Action::Unstage,
        KeyCode::Char('a') if plain && pane == Pane::Changes => Action::StageAll,
        KeyCode::Char('c') if plain && pane == Pane::Changes => Action::OpenCommit,
        KeyCode::Char('p') if plain && pane == Pane::Changes => Action::Push,
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
/// `Ctrl+G` has the agent write the subject, `Enter` commits, `Esc`
/// closes the box and keeps the draft. Anything else is ignored.
fn commit_key(key: &KeyEvent) -> Action {
    let plain = key.modifiers.is_empty();
    let shifted = key.modifiers == KeyModifiers::SHIFT;
    let control = key.modifiers == KeyModifiers::CONTROL;
    match key.code {
        KeyCode::Esc if plain => Action::Mode(Mode::Term(Pane::Changes)),
        KeyCode::Enter if plain => Action::Commit,
        KeyCode::Char('j') if control => Action::Edit(Edit::Newline),
        KeyCode::Char('g') if control => Action::Suggest,
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

/// The session `Alt+1` to `Alt+9` lead to, the one at `index` on the
/// list, from 0, for the switch of `n` and `N` to show; none past the
/// last one. Unlike `n` and `N` they go to agent mode, also on the
/// session already shown, so that keys reach it at once.
fn switch_to(sessions: &Sessions, index: usize, mode: &mut Mode) -> Option<SessionId> {
    let id = sessions.iter().nth(index)?.id;
    *mode = Mode::Agent;
    Some(id)
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

/// Whether the host window has the focus after `event`, having it before
/// when `focused`: a focus event says, anything else leaves it as it was.
fn focused_after(focused: bool, event: &HostEvent) -> bool {
    match event {
        HostEvent::FocusGained => true,
        HostEvent::FocusLost => false,
        _ => focused,
    }
}

/// When the host window last gained the focus after `event` at `now`,
/// having last gained it at `gained` before: a gain of the focus says
/// `now`, anything else leaves it as it was.
fn gained_after(gained: Option<Instant>, event: &HostEvent, now: Instant) -> Option<Instant> {
    match event {
        HostEvent::FocusGained => Some(now),
        _ => gained,
    }
}

/// Whether the host window, `focused` and having last gained the focus
/// at `gained` if it ever did, has had it long enough at `now` for a
/// click to change the mode: [`FOCUS_SETTLES`] at least, so that the
/// click that brings the window forward changes none, whether it comes
/// before the focus event or after it. A window that never reported its
/// focus has had it.
fn focus_settled(focused: bool, gained: Option<Instant>, now: Instant) -> bool {
    focused && gained.is_none_or(|at| now.saturating_duration_since(at) >= FOCUS_SETTLES)
}

/// The desktop notifications for the agents that started asking or
/// finished their turn, `turns` with their projects, one each, while the
/// host window is not `focused`; none while it is, where the screen
/// shows them.
fn notifications(turns: &[(String, agent::State)], focused: bool) -> Vec<String> {
    if focused {
        return Vec::new();
    }
    turns
        .iter()
        .map(|(project, state)| strings::notification(project, *state))
        .collect()
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
/// is on it. A refresh, the end of a push, the agent's subject, a walk or
/// a read does only when it changed something on screen, which applying
/// it tells; a mouse event only when it changes something here; a pty
/// event only when it comes from the tab shown. The finder's answer may
/// open a session or bring a notice, and the end of a copy brings one.
fn dirties(msg: &Msg, shown: SessionId, tab: TabId) -> bool {
    match msg {
        Msg::Pty(key, _) => key.session == shown && key.tab == tab,
        Msg::Git(..)
        | Msg::Pushed(..)
        | Msg::Suggested(..)
        | Msg::Walked(_)
        | Msg::Read(..)
        | Msg::Input(input::Event::Host(HostEvent::Mouse(_))) => false,
        Msg::Input(_) | Msg::Found(_) | Msg::Copied(_) => true,
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

/// Why `Ctrl+G` cannot have the agent write a subject for the draft of
/// `repo`: one is being written for it already, or nothing is staged
/// any more, or the `side_panes` are not shown, as [`can_commit`] says.
/// None when it can.
fn suggest_refusal(repo: &Repo, side_panes: bool) -> Option<&'static str> {
    if repo.suggestion.is_some() {
        Some(strings::SUGGESTION_RUNNING)
    } else if !can_commit(repo.snapshot.as_ref(), side_panes) {
        Some(strings::NOTHING_STAGED)
    } else {
        None
    }
}

/// Whether `action`, run while the key list is open, closes it: every
/// command does, while nothing, a resize, the bytes a focus event sends
/// and the end of a drag leave it open.
fn closes_help(action: &Action) -> bool {
    !matches!(
        action,
        Action::Help
            | Action::Nothing
            | Action::Resize(_)
            | Action::Send(_)
            | Action::Wheel(..)
            | Action::Selection(..)
    )
}

/// Whether `mouse` is a press over the center pane where the mouse acts,
/// as `context` says: not under the key list or the picker.
fn presses_center(mouse: &MouseEvent, context: &KeyContext) -> bool {
    matches!(mouse.kind, MouseEventKind::Down(_))
        && !context.help
        && context.mode != Mode::Picker
        && context
            .agent_pane
            .contains(Position::new(mouse.column, mouse.row))
}

/// The mode a press of the left button `mouse` leads to, as `context`
/// says: over the projects or the changes pane, termis mode with that
/// pane highlighted; over the center pane in termis mode, agent mode.
/// None where it changes nothing: over the side pane already
/// highlighted, over the center pane in agent mode, over the status
/// bar, in the commit box or the picker, under the key list, and while
/// the host window has not had the focus long enough, as
/// [`focus_settled`] says.
fn clicked(mouse: &MouseEvent, context: &KeyContext) -> Option<Mode> {
    if !left_click(mouse, context) {
        return None;
    }
    let at = Position::new(mouse.column, mouse.row);
    let over = |pane: Option<Rect>| pane.is_some_and(|pane| pane.contains(at));
    let mode = if context.agent_pane.contains(at) {
        Mode::Agent
    } else if over(context.changes_pane) {
        Mode::Term(Pane::Changes)
    } else if over(context.projects_pane) {
        Mode::Term(Pane::Projects)
    } else {
        return None;
    };
    (mode != context.mode).then_some(mode)
}

/// Whether `mouse` is a press of the left button that a click acts on,
/// as `context` says: in agent or termis mode, not under the key list,
/// once the host window has had the focus long enough, as
/// [`focus_settled`] says.
fn left_click(mouse: &MouseEvent, context: &KeyContext) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && !context.help
        && context.focus_settled
        && matches!(context.mode, Mode::Agent | Mode::Term(_))
}

/// The row or the tab's name among `spots`, the last frame's, that a
/// press `mouse` points at while the screen is in `layout`: a click, as
/// [`left_click`] says, on one of them. Its pane is highlighted as
/// [`clicked`] says; on a tab's name the press reaches no child and
/// starts no selection.
fn pointed(
    mouse: &MouseEvent,
    context: &KeyContext,
    spots: &Spots,
    layout: Layout,
) -> Option<Spot> {
    if !left_click(mouse, context) {
        return None;
    }
    spots
        .at(Position::new(mouse.column, mouse.row), layout)
        .cloned()
}

/// What `event` does at `now`, as `context` says, with `tab_clicked`
/// when the last click on a tab's name, or the last press dropped after
/// one, was: nothing for a press that [`repeats_tab_click`]; a click on
/// a row or a tab's name among `spots`, the last frame's, while the
/// screen is in `layout`, points at it, as [`pointed`] says; anything
/// else is what [`translate`] says. With whether the press was dropped,
/// and `tab_clicked` after the event.
fn route(
    event: HostEvent,
    context: KeyContext,
    spots: &Spots,
    layout: Layout,
    tab_clicked: Option<Instant>,
    now: Instant,
) -> (Action, bool, Option<Instant>) {
    if let HostEvent::Mouse(mouse) = &event {
        if repeats_tab_click(mouse, &context, tab_clicked, now) {
            return (Action::Nothing, true, Some(now));
        }
        if let Some(spot) = pointed(mouse, &context, spots, layout) {
            let clicked_at = matches!(spot, Spot::Tab(..)).then_some(now).or(tab_clicked);
            return (Action::Point(spot), false, clicked_at);
        }
    }
    (translate(event, context), false, tab_clicked)
}

/// Whether `mouse` is the second press of a double click on a tab's
/// name: a click, as [`left_click`] says, on the center pane's title row
/// as `context` places it, at `now` within [`CLICK_WITHIN`] of
/// `tab_clicked`, when the last click on a tab's name, or the last press
/// dropped after one, was. The title may have moved under it since, with
/// the highlight or the place of a view, so that the press would land on
/// another tab's name, reach the child or start a selection.
fn repeats_tab_click(
    mouse: &MouseEvent,
    context: &KeyContext,
    tab_clicked: Option<Instant>,
    now: Instant,
) -> bool {
    let pane = context.agent_pane;
    left_click(mouse, context)
        && mouse.row == pane.y
        && pane.contains(Position::new(mouse.column, mouse.row))
        && tab_clicked.is_some_and(|at| now.saturating_duration_since(at) <= CLICK_WITHIN)
}

/// Whether `action`, from a key or a paste when `typed`, ends the
/// selection in the center pane: bytes it sends to the tab shown, a
/// scroll of its view, or the agent relaunched.
fn clears_selection(action: &Action, typed: bool) -> bool {
    match action {
        Action::Send(_) => typed,
        Action::Scrollback(_) | Action::Wheel(Pane::Agent, _) | Action::Relaunch => true,
        _ => false,
    }
}

/// Whether `msg` ends `selection`: output of the child of tab `tab` of
/// session `shown`, the tab on screen, once the button that made the
/// selection is released.
fn output_ends(msg: &Msg, shown: SessionId, tab: TabId, selection: Option<&Selection>) -> bool {
    selection.is_some_and(|selection| !selection.held())
        && matches!(
            msg,
            Msg::Pty(key, pty::Event::Output(_)) if key.session == shown && key.tab == tab
        )
}

/// Where a selection made now is: the tab `session` shows, and the rows
/// of its screen drawn in `layout` and `mode`.
fn over_shown(session: &Session, layout: Layout, mode: Mode) -> Over {
    Over {
        session: session.id,
        tab: session.shown_tab(),
        screen: layout.drawn_screen(mode, session.shows_ended()),
    }
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

/// Draws one frame. Returns where the changes pane, the file tree and
/// the list of sessions start on the next one and how many rows of them
/// showed, and where it drew what a click can land on: nothing over the
/// whole terminal.
fn draw(
    host: &mut Host,
    full_screen: bool,
    view: &ui::View<'_>,
) -> Result<(ui::Scrolls, ui::Targets)> {
    let mut drawn = (view.scrolls(), ui::Targets::default());
    host.terminal
        .draw(|frame| {
            if full_screen {
                ui::render_full(frame, view);
            } else {
                drawn = ui::render(frame, view);
            }
        })
        .context("draw")?;
    Ok(drawn)
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
    use repo::POLL_EVERY;
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
            sessions: 1,
            list: false,
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
            projects_pane: Some(Rect::new(0, 0, 20, 39)),
            focus_settled: true,
            alternate_screen: false,
            scrolled: false,
            held: Held::default(),
            selecting: false,
            mode_key: ModeKey::DEFAULT,
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
            assert_eq!(
                translate(wheel(click, 30, 5), ctx),
                Action::Selection(Pointer::Press, Position::new(30, 5)),
                "a press selects text"
            );
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
            projects_pane: None,
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
        // Not over the side panes, not once the agent ended, where the
        // press selects text, not under the key list.
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 80, 5), fullscreen),
            Action::Nothing
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 5, 5), fullscreen),
            Action::Nothing
        );
        let ended = KeyContext {
            agent_alive: false,
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 30, 5), ended),
            Action::Selection(Pointer::Press, Position::new(30, 5))
        );
        let listing = KeyContext {
            help: true,
            mode: Mode::Term(Pane::Agent),
            ..fullscreen
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 30, 5), listing),
            Action::Nothing
        );
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
        // presses, which select text there, or motion, but the wheel and
        // a held button's release.
        let history = KeyContext {
            scrolled: true,
            ..dragging
        };
        assert_eq!(
            translate(wheel(MouseEventKind::Down(left), 30, 5), history),
            Action::Selection(Pointer::Press, Position::new(30, 5))
        );
        assert_eq!(
            translate(wheel(MouseEventKind::Moved, 30, 5), history),
            Action::Nothing
        );
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
            Action::Selection(Pointer::Press, Position::new(30, 5))
        );
    }

    #[test]
    fn a_click_highlights_the_pane_under_it() {
        let mouse = |kind, x, y| crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let press = |x, y| mouse(MouseEventKind::Down(MouseButton::Left), x, y);
        let projects = Some(Mode::Term(Pane::Projects));
        let changes = Some(Mode::Term(Pane::Changes));
        let agent_mode = Some(Mode::Agent);
        // Agent mode: a side pane, its title row and its edges included.
        let plain = agent(Modes::default(), false);
        assert_eq!(clicked(&press(5, 5), &plain), projects);
        assert_eq!(clicked(&press(0, 0), &plain), projects);
        assert_eq!(clicked(&press(19, 38), &plain), projects);
        assert_eq!(clicked(&press(80, 5), &plain), changes);
        assert_eq!(clicked(&press(75, 0), &plain), changes);
        assert_eq!(clicked(&press(99, 38), &plain), changes);
        assert_eq!(clicked(&press(30, 5), &plain), None, "already there");
        assert_eq!(clicked(&press(30, 39), &plain), None, "status bar");
        let shifted = crossterm::event::MouseEvent {
            modifiers: KeyModifiers::SHIFT | KeyModifiers::ALT,
            ..press(80, 5)
        };
        assert_eq!(clicked(&shifted, &plain), changes, "any modifiers");
        // Termis mode: the center, its frame included, goes back to agent
        // mode, whichever pane is highlighted.
        for pane in [Pane::Agent, Pane::Projects, Pane::Changes] {
            let ctx = termis(pane);
            let to = |target| (pane != target).then_some(Mode::Term(target));
            assert_eq!(clicked(&press(30, 5), &ctx), agent_mode, "{pane:?}");
            assert_eq!(clicked(&press(20, 0), &ctx), agent_mode, "{pane:?}");
            assert_eq!(clicked(&press(5, 5), &ctx), to(Pane::Projects));
            assert_eq!(clicked(&press(80, 5), &ctx), to(Pane::Changes));
            assert_eq!(clicked(&press(30, 39), &ctx), None, "status bar");
        }
        let diff = KeyContext {
            diff: true,
            ..termis(Pane::Changes)
        };
        assert_eq!(clicked(&press(80, 5), &diff), None);
        assert_eq!(clicked(&press(5, 5), &diff), projects);
        // Not in the commit box, the picker, or under the key list.
        for ctx in [
            KeyContext {
                mode: Mode::Commit,
                ..plain
            },
            KeyContext {
                mode: Mode::Picker,
                ..plain
            },
            KeyContext {
                help: true,
                ..termis(Pane::Changes)
            },
        ] {
            for (x, y) in [(5, 5), (30, 5), (80, 5)] {
                assert_eq!(clicked(&press(x, y), &ctx), None, "{:?}", ctx.mode);
            }
        }
        // Only a press of the left button.
        let ctx = termis(Pane::Agent);
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
        ] {
            for (x, y) in [(5, 5), (30, 5)] {
                assert_eq!(clicked(&mouse(kind, x, y), &ctx), None, "{kind:?}");
            }
        }
        // With the side panes folded, or the agent over the whole
        // terminal, the bar's row included, every press is on the center.
        let folded = KeyContext {
            side_panes: false,
            agent_pane: Rect::new(0, 0, 100, 39),
            changes_pane: None,
            projects_pane: None,
            ..termis(Pane::Agent)
        };
        assert_eq!(clicked(&press(5, 5), &folded), agent_mode);
        assert_eq!(clicked(&press(80, 5), &folded), agent_mode);
        let full = KeyContext {
            full_screen: true,
            agent_pane: Rect::new(0, 0, 100, 40),
            ..folded
        };
        assert_eq!(clicked(&press(30, 39), &full), agent_mode, "the bar's row");
        // The changes pane hidden: the center takes its place.
        let alone = KeyContext {
            agent_pane: Rect::new(20, 0, 80, 39),
            changes_pane: None,
            ..termis(Pane::Projects)
        };
        assert_eq!(clicked(&press(80, 5), &alone), agent_mode);
        // A child that asks for the mouse still gets the press.
        let asked = KeyContext {
            modes: Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Buttons,
                    encoding: keys::MouseEncoding::Sgr,
                }),
                ..Modes::default()
            },
            ..termis(Pane::Changes)
        };
        assert_eq!(clicked(&press(30, 5), &asked), agent_mode);
        assert_eq!(
            translate(HostEvent::Mouse(press(30, 5)), asked),
            Action::Send(b"\x1b[<0;10;5M".to_vec())
        );
    }

    /// The layout of the panes of [`agent`]'s context: 100 by 40, the
    /// changes pane on screen.
    fn drawn_layout() -> Layout {
        Layout {
            terminal: Size {
                rows: 40,
                cols: 100,
            },
            full_screen: false,
            zoomed: false,
            changes: true,
            widths: Widths::DEFAULT,
        }
    }

    #[test]
    fn a_click_on_a_row_points_at_it_in_the_layout_it_was_drawn_in() {
        let mouse = |kind, x, y| crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let press = |x, y| mouse(MouseEventKind::Down(MouseButton::Left), x, y);
        let layout = drawn_layout();
        let session = Sessions::new().next_id();
        let row = Spot::Row(RowId::Tab {
            session,
            tab: NO_TAB,
        });
        let entry = Spot::Entry(session, "src".into(), true);
        let file = Spot::Change(
            RepoId::for_tests(0),
            ui::changes::Selected {
                section: ui::changes::Section::Staged,
                path: "a.rs".into(),
            },
        );
        let spots = Spots {
            layout: Some(layout),
            spots: vec![
                (Rect::new(2, 3, 16, 1), row.clone()),
                (Rect::new(2, 4, 16, 1), entry.clone()),
                (Rect::new(77, 4, 21, 1), file.clone()),
            ],
        };
        let plain = agent(Modes::default(), false);
        // In agent and termis mode, on any cell of the row; none beside
        // it, above it or below it.
        for ctx in [
            plain,
            termis(Pane::Agent),
            termis(Pane::Projects),
            termis(Pane::Changes),
        ] {
            let at = |x, y| pointed(&press(x, y), &ctx, &spots, layout);
            assert_eq!(at(2, 3), Some(row.clone()), "{:?}", ctx.mode);
            assert_eq!(at(17, 3), Some(row.clone()));
            assert_eq!(at(9, 4), Some(entry.clone()));
            assert_eq!(at(77, 4), Some(file.clone()));
            assert_eq!(at(97, 4), Some(file.clone()));
            for (x, y) in [(1, 3), (18, 3), (9, 2), (9, 5), (30, 3), (76, 4), (98, 4)] {
                assert_eq!(at(x, y), None, "({x}, {y})");
            }
        }
        // Not in the commit box, the picker, under the key list, or before
        // the focus settled.
        for ctx in [
            KeyContext {
                mode: Mode::Commit,
                ..plain
            },
            KeyContext {
                mode: Mode::Picker,
                ..plain
            },
            KeyContext {
                help: true,
                ..termis(Pane::Projects)
            },
            KeyContext {
                focus_settled: false,
                ..plain
            },
        ] {
            for (x, y) in [(2, 3), (77, 4)] {
                let pointing = pointed(&press(x, y), &ctx, &spots, layout);
                assert_eq!(pointing, None, "{:?}", ctx.mode);
            }
        }
        // Only a press of the left button.
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollDown,
        ] {
            let pointing = pointed(&mouse(kind, 2, 3), &plain, &spots, layout);
            assert_eq!(pointing, None, "{kind:?}");
        }
        // Not once the screen is in another layout, until a frame is drawn
        // in it; nor before the first frame.
        let resized = Layout {
            terminal: Size {
                rows: 40,
                cols: 120,
            },
            ..layout
        };
        let zoomed = Layout {
            zoomed: true,
            ..layout
        };
        let hidden = Layout {
            changes: false,
            ..layout
        };
        for other in [resized, zoomed, hidden] {
            assert_eq!(pointed(&press(2, 3), &plain, &spots, other), None);
        }
        let none = Spots::default();
        assert_eq!(pointed(&press(2, 3), &plain, &none, layout), None);
        // A tab's name in the center title, where a child that asks for
        // the mouse gets a press as on its first row.
        let tab = Spot::Tab(session, NO_TAB);
        let spots = Spots {
            layout: Some(layout),
            spots: vec![(Rect::new(31, 0, 3, 1), tab.clone())],
        };
        let asked = KeyContext {
            modes: Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Buttons,
                    encoding: keys::MouseEncoding::Sgr,
                }),
                ..Modes::default()
            },
            ..plain
        };
        for mode in [
            Mode::Agent,
            Mode::Term(Pane::Projects),
            Mode::Term(Pane::Agent),
        ] {
            let ctx = KeyContext { mode, ..asked };
            assert_eq!(
                pointed(&press(32, 0), &ctx, &spots, layout),
                Some(tab.clone()),
                "{mode:?}"
            );
            // Past it the press does what it did.
            assert_eq!(pointed(&press(35, 0), &ctx, &spots, layout), None);
            assert_eq!(
                translate(HostEvent::Mouse(press(35, 0)), ctx),
                Action::Send(b"\x1b[<0;15;1M".to_vec())
            );
        }
        let changes = KeyContext {
            mode: Mode::Term(Pane::Changes),
            ..asked
        };
        assert_eq!(
            clicked(&press(32, 0), &changes),
            Some(Mode::Agent),
            "a click on the center pane"
        );
        // In the commit box the child gets it, as before.
        let commit = KeyContext {
            mode: Mode::Commit,
            ..asked
        };
        assert_eq!(pointed(&press(32, 0), &commit, &spots, layout), None);
        assert_eq!(
            translate(HostEvent::Mouse(press(32, 0)), commit),
            Action::Send(b"\x1b[<0;12;1M".to_vec())
        );
    }

    #[test]
    fn a_click_on_a_row_points_at_it_and_the_rest_is_translated() {
        let press = |x, y| {
            HostEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let layout = drawn_layout();
        let session = Sessions::new().next_id();
        let row = Spot::Row(RowId::Tab {
            session,
            tab: NO_TAB,
        });
        let spots = Spots {
            layout: Some(layout),
            spots: vec![(Rect::new(2, 3, 16, 1), row.clone())],
        };
        // An agent that asks for the mouse.
        let ctx = agent(
            Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Buttons,
                    encoding: keys::MouseEncoding::Sgr,
                }),
                ..Modes::default()
            },
            false,
        );
        let now = Instant::now();
        let routed = |event, layout| route(event, ctx, &spots, layout, None, now).0;
        assert_eq!(routed(press(2, 3), layout), Action::Point(row));
        // The agent gets a press on its screen, and a key does what it does.
        assert_eq!(
            routed(press(40, 10), layout),
            Action::Send(b"\x1b[<0;20;10M".to_vec())
        );
        assert_eq!(
            routed(plain(KeyCode::Char('a')), layout),
            Action::Send(b"a".to_vec())
        );
        // Not once the screen is in another layout.
        let zoomed = Layout {
            zoomed: true,
            ..layout
        };
        assert_eq!(routed(press(2, 3), zoomed), translate(press(2, 3), ctx));
    }

    #[test]
    fn a_press_on_the_title_soon_after_a_tab_click_is_dropped() {
        let mouse = |kind, x, y| crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let press = |x, y| mouse(MouseEventKind::Down(MouseButton::Left), x, y);
        let ctx = agent(Modes::default(), false);
        let pane = ctx.agent_pane;
        let title = |x| press(x, pane.y);
        let tab_clicked = Instant::now();
        let after = |millis| tab_clicked + Duration::from_millis(millis);
        let soon = tab_clicked + CLICK_WITHIN;
        // On the center pane's title row, its corners included, as long as
        // the second press of a double click comes.
        for x in [pane.x, pane.x + 5, pane.right() - 1] {
            assert!(repeats_tab_click(
                &title(x),
                &ctx,
                Some(tab_clicked),
                tab_clicked
            ));
            assert!(repeats_tab_click(&title(x), &ctx, Some(tab_clicked), soon));
        }
        let x = pane.x + 5;
        assert!(repeats_tab_click(
            &title(x),
            &termis(Pane::Changes),
            Some(tab_clicked),
            after(10)
        ));
        // Not later, nor without a tab click.
        let late = soon + Duration::from_millis(1);
        assert!(!repeats_tab_click(&title(x), &ctx, Some(tab_clicked), late));
        assert!(!repeats_tab_click(&title(x), &ctx, None, after(10)));
        // Only on the title row of the center pane.
        for (x, y) in [
            (pane.x + 5, pane.y + 1),
            (pane.x - 1, pane.y),
            (pane.right(), pane.y),
        ] {
            let pressed = press(x, y);
            assert!(!repeats_tab_click(
                &pressed,
                &ctx,
                Some(tab_clicked),
                after(10)
            ));
        }
        // Only a press of the left button.
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            let event = mouse(kind, x, pane.y);
            assert!(
                !repeats_tab_click(&event, &ctx, Some(tab_clicked), after(10)),
                "{kind:?}"
            );
        }
        // Only where a click acts: not in the commit box, where the press
        // reaches the child or starts a selection, as before.
        let commit = KeyContext {
            mode: Mode::Commit,
            ..ctx
        };
        assert!(!repeats_tab_click(
            &title(x),
            &commit,
            Some(tab_clicked),
            after(10)
        ));
    }

    #[test]
    fn a_tab_click_reaches_no_child_and_drops_the_press_soon_after_it() {
        let press = |x, y| {
            HostEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let layout = drawn_layout();
        let session = Sessions::new().next_id();
        let tab = Spot::Tab(session, NO_TAB);
        let row = Spot::Row(RowId::Tab {
            session,
            tab: NO_TAB,
        });
        let spots = Spots {
            layout: Some(layout),
            spots: vec![
                (Rect::new(31, 0, 3, 1), tab.clone()),
                (Rect::new(2, 3, 16, 1), row.clone()),
            ],
        };
        // An agent that asks for the mouse.
        let ctx = agent(
            Modes {
                mouse: Some(keys::Mouse {
                    tracking: Tracking::Buttons,
                    encoding: keys::MouseEncoding::Sgr,
                }),
                ..Modes::default()
            },
            false,
        );
        let routed = |event, tab_clicked, now| route(event, ctx, &spots, layout, tab_clicked, now);
        let first = Instant::now();
        assert_eq!(
            routed(press(32, 0), None, first),
            (Action::Point(tab), false, Some(first))
        );
        // A press on the title soon after is dropped, and the next one is
        // timed from it.
        let second = first + Duration::from_millis(100);
        assert_eq!(
            routed(press(35, 0), Some(first), second),
            (Action::Nothing, true, Some(second))
        );
        // Later the agent gets it.
        let later = second + CLICK_WITHIN + Duration::from_millis(1);
        assert_eq!(
            routed(press(35, 0), Some(second), later),
            (Action::Send(b"\x1b[<0;15;1M".to_vec()), false, Some(second))
        );
        // A click on a row is no tab click.
        assert_eq!(
            routed(press(2, 3), Some(first), later),
            (Action::Point(row), false, Some(first))
        );
    }

    #[test]
    fn the_spots_are_the_rows_drawn_by_what_they_are() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web"], &tx);
        terminal(&mut sessions, "exec sleep 30", &tx);
        let [api, shell, web] = sessions.rows()[..] else {
            panic!("three rows");
        };
        let layout = drawn_layout();
        let at = |y| Rect::new(2, y, 16, 1);
        let selected = ui::changes::Selected {
            section: ui::changes::Section::Unstaged,
            path: "a.rs".into(),
        };
        let drawn = [
            ui::Target::Listed(0),
            ui::Target::Listed(1),
            ui::Target::Listed(2),
            ui::Target::Listed(3),
            ui::Target::Entry("src".into(), true),
            ui::Target::Change(selected.clone()),
            ui::Target::Tab(None),
            ui::Target::Tab(Some(0)),
            ui::Target::Tab(Some(1)),
        ];
        let targets: ui::Targets = (1..).map(at).zip(drawn).collect();
        let spots = Spots::new(&targets, layout, &sessions);
        let shown = sessions.shown().id;
        let [agent_tab, shell_tab] = sessions.shown().tab_ids()[..] else {
            panic!("two tabs");
        };
        assert_eq!(spots.layout, Some(layout));
        assert_eq!(
            spots.spots,
            [
                (at(1), Spot::Row(api)),
                (at(2), Spot::Row(shell)),
                (at(3), Spot::Row(web)),
                (at(5), Spot::Entry(shown, "src".into(), true)),
                (at(7), Spot::Tab(shown, agent_tab)),
                (at(8), Spot::Tab(shown, shell_tab)),
            ],
            "no row past the list, no file without a repository, no tab past the last"
        );
        // With repositories the agent wrote in, more than the list shows:
        // no spot on the row counting the others, and a file is one of
        // the repository the changes pane shows.
        let ids = [1, 2, 3, 4].map(RepoId::for_tests);
        let written = ids.map(|id| touched::Touched {
            root: PathBuf::from("/r").join(format!("{id:?}")),
            id,
            name: format!("{id:?}"),
        });
        sessions.shown_mut().writes.repos = written.into();
        let rows = sessions.rows();
        assert!(matches!(rows[5], RowId::More(_)), "{rows:?}");
        let listed = (0..rows.len()).map(ui::Target::Listed);
        let file = ui::Target::Change(selected.clone());
        let targets: ui::Targets = (1..).map(at).zip(listed.chain([file])).collect();
        let spots = Spots::new(&targets, layout, &sessions);
        let mut expected: Vec<(Rect, Spot)> = (1..)
            .map(at)
            .zip(rows.iter().map(|&row| Spot::Row(row)))
            .filter(|(_, spot)| !matches!(spot, Spot::Row(RowId::More(_))))
            .collect();
        let shows = sessions.shown().changes().expect("a repository written in");
        assert_eq!(shows, ids[0]);
        expected.push((at(8), Spot::Change(shows, selected)));
        assert_eq!(spots.spots, expected);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn a_click_that_brings_the_window_forward_changes_no_mode() {
        let press = |x, y| crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let now = Instant::now();
        let after = |millis| now + Duration::from_millis(millis);
        // A gain of the focus stamps its time; any other event leaves the
        // stamp.
        let gained = gained_after(None, &HostEvent::FocusGained, now);
        assert_eq!(gained, Some(now));
        let later = after(100);
        for event in [
            HostEvent::FocusLost,
            plain(KeyCode::Char('q')),
            HostEvent::Mouse(press(5, 5)),
        ] {
            assert_eq!(gained_after(gained, &event, later), gained, "{event:?}");
            assert_eq!(gained_after(None, &event, later), None, "{event:?}");
        }
        let again = gained_after(gained, &HostEvent::FocusGained, later);
        assert_eq!(again, Some(later));
        // A window that never reported its focus has it.
        assert!(focus_settled(true, None, now));
        // In the background, so a click that comes before the focus event.
        let lost = focused_after(true, &HostEvent::FocusLost);
        assert!(!focus_settled(lost, None, now));
        assert!(!focus_settled(lost, gained, after(1000)));
        // Just focused, so a click that comes after it.
        let back = focused_after(lost, &HostEvent::FocusGained);
        assert!(!focus_settled(back, gained, now));
        assert!(!focus_settled(back, gained, after(299)));
        // From 300 ms on.
        assert!(focus_settled(back, gained, after(300)));
        assert!(focus_settled(back, gained, after(5000)));
        // Until then a click changes no mode, and the press does what it
        // does once the focus settled.
        for (mode, at, settled) in [
            (Mode::Agent, (80, 5), Some(Mode::Term(Pane::Changes))),
            (Mode::Agent, (5, 5), Some(Mode::Term(Pane::Projects))),
            (Mode::Term(Pane::Changes), (30, 5), Some(Mode::Agent)),
        ] {
            let ctx = KeyContext {
                mode,
                ..agent(Modes::default(), false)
            };
            let click = press(at.0, at.1);
            assert_eq!(clicked(&click, &ctx), settled);
            let unsettled = KeyContext {
                focus_settled: false,
                ..ctx
            };
            assert_eq!(clicked(&click, &unsettled), None, "{mode:?}");
            assert_eq!(
                translate(HostEvent::Mouse(click), unsettled),
                translate(HostEvent::Mouse(click), ctx)
            );
        }
        // Over the center, in termis mode, it starts a selection.
        let unsettled = KeyContext {
            focus_settled: false,
            ..termis(Pane::Changes)
        };
        assert_eq!(
            translate(HostEvent::Mouse(press(30, 5)), unsettled),
            Action::Selection(Pointer::Press, Position::new(30, 5))
        );
    }

    #[test]
    fn the_left_button_selects_text_where_the_child_does_not_take_it() {
        let left = MouseButton::Left;
        let press = |x, y| wheel(MouseEventKind::Down(left), x, y);
        let drag = |x, y| wheel(MouseEventKind::Drag(left), x, y);
        let release = |x, y| wheel(MouseEventKind::Up(left), x, y);
        let selection = |pointer, x, y| Action::Selection(pointer, Position::new(x, y));
        // A press over the center pane, its frame included, in every mode
        // but the picker.
        let plain = agent(Modes::default(), false);
        for mode in [Mode::Agent, Mode::Term(Pane::Projects), Mode::Commit] {
            let ctx = KeyContext { mode, ..plain };
            assert_eq!(
                translate(press(30, 5), ctx),
                selection(Pointer::Press, 30, 5)
            );
            assert_eq!(
                translate(press(20, 0), ctx),
                selection(Pointer::Press, 20, 0)
            );
        }
        let picking = KeyContext {
            mode: Mode::Picker,
            ..plain
        };
        assert_eq!(translate(press(30, 5), picking), Action::Nothing);
        // Not over the side panes, nor with another button.
        assert_eq!(translate(press(5, 5), plain), Action::Nothing);
        assert_eq!(translate(press(80, 5), plain), Action::Nothing);
        let right = wheel(MouseEventKind::Down(MouseButton::Right), 30, 5);
        assert_eq!(translate(right, plain), Action::Nothing);
        // Its drags and its release follow while it selects, past the pane
        // too; without a selection they do nothing.
        assert_eq!(translate(drag(40, 6), plain), Action::Nothing);
        assert_eq!(translate(release(40, 6), plain), Action::Nothing);
        let selecting = KeyContext {
            selecting: true,
            ..plain
        };
        assert_eq!(
            translate(drag(40, 6), selecting),
            selection(Pointer::Drag, 40, 6)
        );
        assert_eq!(
            translate(drag(90, 39), selecting),
            selection(Pointer::Drag, 90, 39)
        );
        assert_eq!(
            translate(release(2, 3), selecting),
            selection(Pointer::Release, 2, 3)
        );
        let middle = wheel(MouseEventKind::Drag(MouseButton::Middle), 40, 6);
        assert_eq!(translate(middle, selecting), Action::Nothing);
        // Under the key list or the picker, a selection still ends.
        for ctx in [
            KeyContext {
                help: true,
                mode: Mode::Term(Pane::Agent),
                ..selecting
            },
            KeyContext {
                mode: Mode::Picker,
                ..selecting
            },
        ] {
            assert_eq!(translate(drag(40, 6), ctx), selection(Pointer::Drag, 40, 6));
            assert_eq!(
                translate(release(40, 6), ctx),
                selection(Pointer::Release, 40, 6)
            );
            assert_eq!(translate(press(30, 5), ctx), Action::Nothing);
        }
        assert!(!closes_help(&selection(Pointer::Release, 40, 6)));
        // A child that asked for the mouse still gets the press, in the
        // encoding it asked for, and no selection starts.
        let mouse = Modes {
            mouse: Some(keys::Mouse {
                tracking: Tracking::Drags,
                encoding: keys::MouseEncoding::Sgr,
            }),
            ..Modes::default()
        };
        let asked = agent(mouse, false);
        assert_eq!(
            translate(press(30, 5), asked),
            Action::Send(b"\x1b[<0;10;5M".to_vec())
        );
        let pressed = selection(Pointer::Press, 30, 5);
        let held = Held::default().after(MouseEventKind::Down(left), &pressed);
        assert!(!held.has(left), "the agent did not get it");
    }

    #[test]
    fn a_press_a_key_a_scroll_or_new_output_ends_the_selection() {
        let plain = agent(Modes::default(), false);
        let mouse = |kind, x, y| crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        // Any press over the center pane, the agent's or not.
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
        ] {
            assert!(presses_center(&mouse(kind, 30, 5), &plain));
            assert!(presses_center(&mouse(kind, 20, 0), &plain), "the frame");
            assert!(!presses_center(&mouse(kind, 80, 5), &plain), "Changes");
            assert!(!presses_center(&mouse(kind, 5, 5), &plain), "Projects");
        }
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
        ] {
            assert!(!presses_center(&mouse(kind, 30, 5), &plain), "{kind:?}");
        }
        // Not under the key list or the picker, where it does nothing.
        let left = MouseEventKind::Down(MouseButton::Left);
        let listing = KeyContext {
            help: true,
            ..termis(Pane::Agent)
        };
        let picking = KeyContext {
            mode: Mode::Picker,
            ..plain
        };
        assert!(!presses_center(&mouse(left, 30, 5), &listing));
        assert!(!presses_center(&mouse(left, 30, 5), &picking));
        // A key or a paste sent to the tab, a scroll of its view, a
        // relaunch; Enter in the file tree only on a file, which the
        // arm of the action sees.
        let typed = Action::Send(b"a".to_vec());
        assert!(clears_selection(&typed, true));
        assert!(!clears_selection(&typed, false), "a focus event, a report");
        for action in [
            Action::Scrollback(Movement::Line(Step::Up)),
            Action::Wheel(Pane::Agent, Step::Down),
            Action::Relaunch,
        ] {
            assert!(clears_selection(&action, false), "{action:?}");
        }
        for action in [
            Action::Nothing,
            Action::Mode(Mode::Agent),
            Action::Wheel(Pane::Changes, Step::Down),
            Action::Help,
            Action::TreeEnter,
            Action::Selection(Pointer::Drag, Position::new(30, 5)),
        ] {
            assert!(!clears_selection(&action, true), "{action:?}");
        }
        // Output of the tab shown once the button is released; not of
        // another tab, nor its end, nor while the button is held.
        let mut sessions = Sessions::new();
        let session = sessions.next_id();
        let shown = TabKey::agent(session);
        let other = TabKey {
            tab: session::tests::tab(1),
            ..shown
        };
        let screen = Rect::new(0, 0, 4, 2);
        let emulator = emu::Emulator::new(Size::from(screen), 0);
        let grid = ui::select::Grid::new(ui::Center::Screen(&emulator), screen).expect("cells");
        let over = Over {
            session,
            tab: shown.tab,
            screen,
        };
        let at = Position::new(1, 1);
        let held = Selection::press(over, &grid, at, ui::select::Unit::Word);
        let released = held.clone().release(&grid, at).expect("a word");
        let output = |key| Msg::Pty(key, pty::Event::Output(b"x".to_vec()));
        let ends = |msg: &Msg, selection| output_ends(msg, session, shown.tab, selection);
        assert!(ends(&output(shown), Some(&released)));
        assert!(!ends(&output(shown), Some(&held)), "held");
        assert!(!ends(&output(shown), None));
        assert!(!ends(&output(other), Some(&released)));
        let elsewhere = sessions.next_id();
        let away = output_ends(&output(shown), elsewhere, shown.tab, Some(&released));
        assert!(!away, "another session shown");
        assert!(!ends(&Msg::Pty(shown, pty::Event::Closed), Some(&released)));
    }

    #[test]
    fn a_selection_takes_the_rows_drawn_without_the_line_over_the_last() {
        let wide = Layout {
            terminal: Size {
                rows: 40,
                cols: 100,
            },
            full_screen: false,
            zoomed: false,
            changes: true,
            widths: Widths::DEFAULT,
        };
        let screen = wide.agent_screen();
        let shorter = Rect {
            height: screen.height - 1,
            ..screen
        };
        // Beside the side panes the status bar is a row of its own: only
        // the line saying the agent ended covers one of the screen.
        for mode in [
            Mode::Agent,
            Mode::Term(Pane::Changes),
            Mode::Commit,
            Mode::Picker,
        ] {
            assert_eq!(wide.drawn_screen(mode, false), screen, "{mode:?}");
            assert_eq!(wide.drawn_screen(mode, true), shorter, "{mode:?}");
        }
        // Over the whole terminal, so does the bar of termis mode.
        let full = Layout {
            full_screen: true,
            ..wide
        };
        let whole = Rect::new(0, 0, 100, 40);
        let shorter = Rect::new(0, 0, 100, 39);
        assert_eq!(full.drawn_screen(Mode::Agent, false), whole);
        assert_eq!(full.drawn_screen(Mode::Agent, true), shorter);
        for mode in [Mode::Term(Pane::Agent), Mode::Commit, Mode::Picker] {
            assert_eq!(full.drawn_screen(mode, false), shorter, "{mode:?}");
        }
        // A drag past the bottom stops at the last row drawn.
        let emulator = emu::Emulator::new(Size::from(whole), 0);
        let grid = ui::select::Grid::new(ui::Center::Screen(&emulator), shorter).expect("cells");
        assert_eq!(grid.point(Position::new(5, 39)), (38, 5));
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
    fn the_termis_mode_key_set_takes_ctrl_qs_place_and_ctrl_q_reaches_the_child() {
        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        let set = |ctx: KeyContext| KeyContext {
            mode_key: "ctrl+o".parse().expect("allowed"),
            ..ctx
        };
        let ctx = set(agent(Modes::default(), false));
        assert_eq!(
            translate(key(ctrl_o), ctx),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        for mode in [
            Mode::Term(Pane::Agent),
            Mode::Term(Pane::Changes),
            Mode::Commit,
            Mode::Picker,
        ] {
            let ctx = KeyContext { mode, ..ctx };
            assert_eq!(
                translate(key(ctrl_o), ctx),
                Action::Mode(Mode::Agent),
                "{mode:?}"
            );
            assert_eq!(translate(key(CTRL_Q), ctx), Action::Nothing, "{mode:?}");
        }
        // Ctrl+Q is an ordinary key: the agent or a terminal gets it, as
        // its protocol writes it.
        assert_eq!(translate(key(CTRL_Q), ctx), Action::Send(vec![0x11]));
        let shell = KeyContext {
            terminal: true,
            ..ctx
        };
        assert_eq!(translate(key(CTRL_Q), shell), Action::Send(vec![0x11]));
        let kitty = set(agent(
            Modes {
                protocol: Protocol::Kitty(KittyFlags::DISAMBIGUATE),
                ..Modes::default()
            },
            false,
        ));
        assert_eq!(
            translate(key(CTRL_Q), kitty),
            Action::Send(b"\x1b[113;5u".to_vec())
        );
        assert_eq!(
            translate(key(ctrl_o), kitty),
            Action::Mode(Mode::Term(Pane::Agent))
        );
        // With the default, Ctrl+O reaches the agent.
        let default = agent(Modes::default(), false);
        assert_eq!(translate(key(ctrl_o), default), Action::Send(vec![0x0f]));
    }

    #[test]
    fn i_goes_back_to_agent_mode_as_ctrl_q_does_but_not_under_the_key_list() {
        let i = || plain(KeyCode::Char('i'));
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            for sessions in [1, 2] {
                let ctx = KeyContext {
                    sessions,
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
            for sessions in [1, 2] {
                let ctx = KeyContext {
                    sessions,
                    ..termis(pane)
                };
                assert_eq!(translate(plain(KeyCode::Char('x')), ctx), Action::Close);
            }
        }
        let shift_x = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_x), ctx), Action::Nothing);
        assert!(closes_help(&Action::Close), "a command");
        // p pushes with the changes pane highlighted, see
        // `commit_and_push_keys_act_only_with_the_changes_pane_highlighted`;
        // the diff stays open.
        assert_eq!(
            translate(plain(KeyCode::Char('p')), termis(Pane::Changes)),
            Action::Push
        );
        let diff = KeyContext {
            diff: true,
            ..termis(Pane::Changes)
        };
        assert_eq!(translate(plain(KeyCode::Char('p')), diff), Action::Push);
        assert!(!closes_diff(&Action::Push, Mode::Term(Pane::Changes)));
        assert!(closes_help(&Action::Push), "a command");
        let shift_p = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
        assert_eq!(translate(key(shift_p), ctx), Action::Nothing);
        // In agent mode it is the agent's, in the commit box text.
        assert_eq!(
            translate(plain(KeyCode::Char('p')), agent(Modes::default(), false)),
            Action::Send(b"p".to_vec())
        );
        let boxed = KeyContext {
            mode: Mode::Commit,
            ..ctx
        };
        assert_eq!(
            translate(plain(KeyCode::Char('p')), boxed),
            Action::Edit(Edit::Insert("p".into()))
        );
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
            sessions: 2,
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
            sessions: 2,
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
    fn notifications_go_out_only_while_the_window_has_no_focus() {
        use agent::State;
        let turns = [
            ("api".to_owned(), State::Attention),
            ("web".to_owned(), State::Waiting),
        ];
        // The window has the focus until it says otherwise.
        let mut focused = true;
        assert!(notifications(&turns, focused).is_empty());
        focused = focused_after(focused, &plain(KeyCode::Char('a')));
        assert!(notifications(&turns, focused).is_empty(), "a key");
        focused = focused_after(focused, &HostEvent::FocusLost);
        assert_eq!(
            notifications(&turns, focused),
            ["api is asking", "web is waiting"],
            "one each"
        );
        assert!(notifications(&[], focused).is_empty(), "no turn");
        focused = focused_after(focused, &HostEvent::Resize(80, 24));
        assert_eq!(notifications(&turns[1..], focused), ["web is waiting"]);
        focused = focused_after(focused, &HostEvent::FocusGained);
        assert!(notifications(&turns, focused).is_empty(), "back");
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
    fn commit_and_push_keys_act_only_with_the_changes_pane_highlighted() {
        // Whatever else holds: the key list open, other sessions, the
        // list of sessions or the file tree in the projects pane.
        let around = |pane| {
            let ctx = termis(pane);
            [
                ctx,
                KeyContext { help: true, ..ctx },
                KeyContext {
                    sessions: 2,
                    list: true,
                    ..ctx
                },
                KeyContext { tree: true, ..ctx },
            ]
        };
        let diff = KeyContext {
            diff: true,
            ..termis(Pane::Changes)
        };
        for ctx in around(Pane::Changes).into_iter().chain([diff]) {
            assert_eq!(
                translate(plain(KeyCode::Char('c')), ctx),
                Action::OpenCommit
            );
            assert_eq!(translate(plain(KeyCode::Char('p')), ctx), Action::Push);
        }
        let hidden = KeyContext {
            side_panes: false,
            ..termis(Pane::Agent)
        };
        let elsewhere = around(Pane::Projects)
            .into_iter()
            .chain(around(Pane::Agent));
        for ctx in elsewhere.chain([hidden]) {
            for code in ['c', 'p'] {
                assert_eq!(
                    translate(plain(KeyCode::Char(code)), ctx),
                    Action::Nothing,
                    "{code} in {:?}",
                    ctx.mode
                );
            }
        }
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
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(translate(key(ctrl_g), ctx), Action::Suggest);
        assert_eq!(
            translate(key(ctrl_g), termis(Pane::Changes)),
            Action::Nothing,
            "only in the box"
        );
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
        // c opens the box with the changes pane highlighted.
        assert_eq!(
            translate(plain(KeyCode::Char('c')), termis(Pane::Agent)),
            Action::Nothing
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
    fn ctrl_g_needs_something_staged_and_no_suggestion_running() {
        let dir = TempDir::new().expect("dir");
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let id = repos.get_or_spawn(dir.path(), &tx).expect("spawn");
        let repo = repos.get_mut(id).expect("its repository");
        assert_eq!(
            suggest_refusal(repo, true),
            Some(strings::NOTHING_STAGED),
            "nothing read yet"
        );
        let mut snapshot = Snapshot::default();
        repo.snapshot = Some(snapshot.clone());
        assert_eq!(suggest_refusal(repo, true), Some(strings::NOTHING_STAGED));
        snapshot.status.entries = vec![git::status::Entry {
            path: "a".into(),
            original: None,
            index: Some(Change::Added),
            worktree: None,
            submodule: false,
        }];
        repo.snapshot = Some(snapshot);
        assert_eq!(suggest_refusal(repo, true), None);
        assert_eq!(
            suggest_refusal(repo, false),
            Some(strings::NOTHING_STAGED),
            "no box to write in"
        );
        // One at a time. This one fails, there being no agent and no
        // repository, and its end is never read.
        let nowhere = agent::Ask {
            program: dir.path().join("nowhere").to_string_lossy().into_owned(),
            args: Vec::new(),
            env_remove: Vec::new(),
        };
        repo.suggest(nowhere, "api".into(), &tx).expect("starts");
        assert_eq!(
            suggest_refusal(repo, true),
            Some(strings::SUGGESTION_RUNNING)
        );
        assert_eq!(
            suggest_refusal(repo, false),
            Some(strings::SUGGESTION_RUNNING)
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
            list: true,
            ..termis(Pane::Projects)
        };
        let list = KeyContext {
            list: true,
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
            changes: true,
            widths: Widths::DEFAULT,
        };
        assert!(wide.side_panes() && wide.zoomable());
        assert_eq!(wide.sides(), Sides::Both);
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
        // A click lands on the projects pane only while it is on screen;
        // over the whole terminal the center has the bar's row too.
        assert_eq!(wide.projects_pane(), Some(Rect::new(0, 0, 20, 39)));
        for layout in [zoomed, narrow, full] {
            assert_eq!(layout.projects_pane(), None, "{layout:?}");
        }
        assert_eq!(full.agent_pane(), Rect::new(0, 0, 100, 40));
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
    fn the_agent_takes_the_width_of_the_changes_pane_hidden() {
        // Width, then the agent's columns with the changes pane and
        // without it.
        for (cols, both, alone) in [(100u16, 53, 78), (140, 75, 110)] {
            let shown = Layout {
                terminal: Size { rows: 40, cols },
                full_screen: false,
                zoomed: false,
                changes: true,
                widths: Widths::DEFAULT,
            };
            let hidden = Layout {
                changes: false,
                ..shown
            };
            assert_eq!(
                shown.agent_size(),
                Size {
                    rows: 37,
                    cols: both
                }
            );
            assert_eq!(
                hidden.agent_size(),
                Size {
                    rows: 37,
                    cols: alone
                }
            );
            assert_eq!(hidden.sides(), Sides::Projects);
            assert!(hidden.side_panes() && hidden.zoomable(), "{cols}");
            // The mouse goes by the panes on screen.
            let right = shown.changes_pane().expect("on screen");
            assert_eq!(right.right(), cols);
            assert_eq!(shown.agent_pane().right(), right.x);
            assert_eq!(hidden.changes_pane(), None);
            assert_eq!(hidden.agent_pane().right(), cols);
            assert_eq!(hidden.agent_pane().x, shown.agent_pane().x);
            // A click finds one pane on every column, none on the bar.
            for layout in [shown, hidden] {
                let panes = [
                    layout.projects_pane(),
                    Some(layout.agent_pane()),
                    layout.changes_pane(),
                ];
                let under = |x, y| {
                    let at = Position::new(x, y);
                    panes
                        .iter()
                        .flatten()
                        .filter(|pane| pane.contains(at))
                        .count()
                };
                for x in 0..cols {
                    assert_eq!(under(x, 5), 1, "{cols}, column {x}");
                    assert_eq!(under(x, 39), 0, "{cols}, the bar at {x}");
                }
            }
            // Zoomed, the changes pane counts for nothing.
            let zoomed = |layout: Layout| Layout {
                zoomed: true,
                ..layout
            };
            assert_eq!(zoomed(hidden).agent_size(), zoomed(shown).agent_size());
            assert_eq!(zoomed(hidden).sides(), Sides::None);
            assert_eq!(zoomed(shown).changes_pane(), None);
        }
        let mouse = KeyContext {
            agent_pane: Rect::new(20, 0, 80, 39),
            agent_screen: Rect::new(21, 1, 78, 37),
            changes_pane: None,
            ..agent(Modes::default(), false)
        };
        assert_eq!(
            translate(wheel(MouseEventKind::ScrollDown, 90, 5), mouse),
            Action::Wheel(Pane::Agent, Step::Down),
            "where the changes pane would be"
        );
    }

    #[test]
    fn the_panes_follow_the_widths_and_a_click_lands_on_the_one_drawn() {
        let layout = Layout {
            terminal: Size {
                rows: 40,
                cols: 100,
            },
            full_screen: false,
            zoomed: false,
            changes: true,
            widths: Widths::new(40, 15).expect("allowed"),
        };
        assert_eq!(layout.projects_pane(), Some(Rect::new(0, 0, 40, 39)));
        assert_eq!(layout.agent_pane(), Rect::new(40, 0, 45, 39));
        assert_eq!(layout.changes_pane(), Some(Rect::new(85, 0, 15, 39)));
        assert_eq!(layout.agent_screen(), Rect::new(41, 1, 43, 37));
        assert_eq!(layout.agent_size(), Size { rows: 37, cols: 43 });
        let ctx = KeyContext {
            agent_pane: layout.agent_pane(),
            agent_screen: layout.agent_screen(),
            changes_pane: layout.changes_pane(),
            projects_pane: layout.projects_pane(),
            ..agent(Modes::default(), false)
        };
        let press = |column| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(clicked(&press(30), &ctx), Some(Mode::Term(Pane::Projects)));
        assert_eq!(clicked(&press(90), &ctx), Some(Mode::Term(Pane::Changes)));
        assert_eq!(clicked(&press(50), &ctx), None, "already there");
        assert_eq!(
            translate(HostEvent::Mouse(press(50)), ctx),
            Action::Selection(Pointer::Press, Position::new(50, 5))
        );
        // With the changes pane hidden, the center takes its width.
        let hidden = Layout {
            changes: false,
            ..layout
        };
        assert_eq!(hidden.agent_pane(), Rect::new(40, 0, 60, 39));
        assert_eq!(hidden.projects_pane(), layout.projects_pane());
    }

    #[test]
    fn the_changes_pane_goes_by_a_fresh_snapshot_unless_held() {
        use changes::Need;
        fn repo(repos: &mut Repos, id: RepoId) -> &mut Repo {
            repos.get_mut(id).expect("its repository")
        }
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api"], &tx);
        let mut repos = Repos::new(POLL_EVERY);
        let api = sessions.shown().id;
        let highlighted = Mode::Term(Pane::Changes);
        // Outside any repository nothing needs it, unless highlighted.
        assert_eq!(
            changes_need(&sessions, &repos, Mode::Agent),
            ((api, None), Need::Nothing)
        );
        assert_eq!(changes_need(&sessions, &repos, highlighted).1, Need::Shown);
        // In a repository, its snapshot decides once refreshed.
        let id = repos
            .get_or_spawn(&temporary.path().join("api"), &tx)
            .expect("spawn");
        sessions.shown_mut().repo = Some(id);
        let need = |repos: &Repos| changes_need(&sessions, repos, Mode::Agent);
        assert_eq!(need(&repos), ((api, Some(id)), Need::Unread), "not read");
        repo(&mut repos, id).snapshot = Some(ui::changes::tests::sample());
        assert_eq!(need(&repos).1, Need::Unread, "not refreshed");
        let now = Instant::now();
        repo(&mut repos, id).refreshed(now);
        assert_eq!(need(&repos).1, Need::Shown, "files not committed");
        repo(&mut repos, id).snapshot = Some(Snapshot::default());
        assert_eq!(need(&repos).1, Need::Nothing, "clean");
        // A snapshot older than a switch, or than the return of the side
        // panes, decides nothing until the next refresh.
        repo(&mut repos, id).refresh_soon();
        assert_eq!(need(&repos).1, Need::Unread, "switched");
        repo(&mut repos, id).refreshed(now);
        repos.read_when_due(now, [Some(id), None], false, |_| false);
        assert_eq!(need(&repos).1, Need::Unread, "side panes hidden");
        repo(&mut repos, id).refreshed(now);
        assert_eq!(need(&repos).1, Need::Nothing);
        // A diff, or a commit, a push or a subject running, holds it.
        let request = git::diff::Request {
            paths: vec![PathBuf::from("a.txt")],
            staged: false,
            untracked: false,
        };
        repo(&mut repos, id).diff = Some(ui::diff::Diff::new(request, "a.txt".into()));
        assert_eq!(need(&repos).1, Need::Shown, "a diff");
        repo(&mut repos, id).diff = None;
        repo(&mut repos, id).in_flight = Some(ui::commit::Draft::default());
        assert_eq!(need(&repos).1, Need::Shown, "a commit");
        repo(&mut repos, id).in_flight = None;
        assert_eq!(need(&repos).1, Need::Nothing);
        assert_eq!(changes_need(&sessions, &repos, highlighted).1, Need::Shown);
        sessions.quit();
        sessions.close_all(&rx);
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
        let copied = Msg::Copied(Ok(strings::copied(3, 40)));
        assert!(dirties(&copied, shown, tab), "it says so");
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
                sessions: 2,
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
            sessions: 2,
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

    fn alt(ch: char) -> HostEvent {
        key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::ALT))
    }

    /// What `Alt+1` to `Alt+9` do with `count` sessions: show those there
    /// are, nothing past the last one.
    fn switches(count: usize) -> Vec<Action> {
        (0..9)
            .map(|index| {
                if index < count {
                    Action::SwitchTo(index)
                } else {
                    Action::Nothing
                }
            })
            .collect()
    }

    #[test]
    fn alt_and_a_digit_show_that_session_and_never_reach_the_child() {
        let digits = || ('1'..='9').map(alt);
        // In agent mode, whatever the number of sessions.
        for count in [1, 2, 3] {
            let ctx = KeyContext {
                sessions: count,
                ..agent(Modes::default(), false)
            };
            let actions: Vec<Action> = digits().map(|event| translate(event, ctx)).collect();
            assert_eq!(actions, switches(count), "{count} sessions");
            // Alt+0, the digits with another modifier too, and alone,
            // reach the agent.
            assert_eq!(translate(alt('0'), ctx), Action::Send(b"\x1b0".to_vec()));
            assert_eq!(
                translate(plain(KeyCode::Char('1')), ctx),
                Action::Send(b"1".to_vec())
            );
            let kitty = KeyContext {
                modes: Modes {
                    protocol: Protocol::Kitty(KittyFlags::DISAMBIGUATE),
                    ..Modes::default()
                },
                ..ctx
            };
            for (modifiers, bytes) in [
                (
                    KeyModifiers::ALT | KeyModifiers::CONTROL,
                    &b"\x1b[49;7u"[..],
                ),
                (KeyModifiers::ALT | KeyModifiers::SHIFT, b"\x1b[49;4u"),
            ] {
                let event = key(KeyEvent::new(KeyCode::Char('1'), modifiers));
                assert_eq!(translate(event, kitty), Action::Send(bytes.to_vec()));
            }
        }
        // In termis mode from every pane, with the key list open or not;
        // the digits alone still highlight a pane.
        for pane in [Pane::Projects, Pane::Agent, Pane::Changes] {
            for help in [false, true] {
                let ctx = KeyContext {
                    sessions: 3,
                    help,
                    ..termis(pane)
                };
                let actions: Vec<Action> = digits().map(|event| translate(event, ctx)).collect();
                assert_eq!(actions, switches(3), "{pane:?}");
                assert_eq!(
                    translate(plain(KeyCode::Char('1')), ctx),
                    Action::Mode(Mode::Term(Pane::Projects))
                );
                assert_eq!(translate(alt('0'), ctx), Action::Nothing);
            }
        }
        // A command: the key list closes and the command runs.
        assert!(closes_help(&Action::SwitchTo(0)));
        // The viewer, a terminal and an agent that ended take them too.
        for ctx in [
            KeyContext {
                viewer: true,
                ..agent(Modes::default(), false)
            },
            KeyContext {
                terminal: true,
                ..agent(Modes::default(), false)
            },
            KeyContext {
                agent_alive: false,
                ..agent(Modes::default(), false)
            },
        ] {
            let ctx = KeyContext { sessions: 2, ..ctx };
            let actions: Vec<Action> = digits().map(|event| translate(event, ctx)).collect();
            assert_eq!(actions, switches(2));
        }
        // The commit box and the picker drop them: no text.
        for mode in [Mode::Commit, Mode::Picker] {
            let ctx = KeyContext {
                mode,
                sessions: 3,
                ..agent(Modes::default(), false)
            };
            for event in digits() {
                assert_eq!(translate(event, ctx), Action::Nothing, "{mode:?}");
            }
        }
        // Over the whole terminal the agent gets them, as before; termis
        // mode drops them.
        let spike = KeyContext {
            full_screen: true,
            ..agent(Modes::default(), false)
        };
        assert_eq!(translate(alt('1'), spike), Action::Send(b"\x1b1".to_vec()));
        assert_eq!(translate(alt('2'), spike), Action::Send(b"\x1b2".to_vec()));
        let spike_term = KeyContext {
            mode: Mode::Term(Pane::Agent),
            ..spike
        };
        assert_eq!(translate(alt('1'), spike_term), Action::Nothing);
    }

    #[test]
    fn alt_and_a_digit_show_the_session_at_that_place_in_agent_mode() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api", "web", "docs"], &tx);
        let mut repos = Repos::new(POLL_EVERY);
        let ids: Vec<SessionId> = sessions.iter().map(|session| session.id).collect();
        // From termis mode, Alt+3 shows the third session in agent mode,
        // the third of three in the status bar.
        let ctx = KeyContext {
            sessions: 3,
            ..termis(Pane::Projects)
        };
        let Action::SwitchTo(index) = translate(alt('3'), ctx) else {
            panic!("a switch");
        };
        let mut mode = ctx.mode;
        let id = switch_to(&sessions, index, &mut mode).expect("a session");
        assert!(open::show(&mut sessions, &mut repos, id, false));
        assert_eq!((sessions.shown().id, mode), (ids[2], Mode::Agent));
        assert_eq!(sessions.position(), Some((3, 3)));
        // On the session shown, termis mode still goes.
        let mut mode = Mode::Term(Pane::Changes);
        let id = switch_to(&sessions, 2, &mut mode).expect("a session");
        assert!(!open::show(&mut sessions, &mut repos, id, false));
        assert_eq!((sessions.shown().id, mode), (ids[2], Mode::Agent));
        // In agent mode the mode stays.
        let mut mode = Mode::Agent;
        let id = switch_to(&sessions, 0, &mut mode).expect("a session");
        assert!(open::show(&mut sessions, &mut repos, id, false));
        assert_eq!((sessions.shown().id, mode), (ids[0], Mode::Agent));
        // Past the last one nothing happens, the mode included.
        let mut mode = Mode::Term(Pane::Agent);
        assert_eq!(switch_to(&sessions, 3, &mut mode), None);
        assert_eq!(mode, Mode::Term(Pane::Agent));
        // Closing a session renumbers the ones after it.
        assert!(open::close(
            &mut sessions,
            &mut repos,
            ids[1],
            false,
            Instant::now()
        ));
        assert_eq!(switch_to(&sessions, 1, &mut mode), Some(ids[2]));
        assert_eq!(switch_to(&sessions, 2, &mut mode), None);
        sessions.quit();
        sessions.close_all(&rx);
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
        // Without the list, as with one session whose agent wrote in no
        // other repository, these keys do nothing there.
        for event in keys.clone() {
            assert_eq!(translate(event, termis(Pane::Projects)), Action::Nothing);
        }
        // With it, from two sessions on or with such repositories, they
        // move its cursor and show the row under it.
        let ctx = KeyContext {
            list: true,
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
            sessions: 2,
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
            sessions: 2,
            list: true,
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
            wheel(MouseEventKind::Down(MouseButton::Right), 30, 5),
        ] {
            assert_eq!(translate(event, viewing), Action::Nothing);
        }
        // The left button selects its text.
        assert_eq!(
            translate(
                wheel(MouseEventKind::Down(MouseButton::Left), 30, 5),
                viewing
            ),
            Action::Selection(Pointer::Press, Position::new(30, 5))
        );
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
        let ending = close_ending(sessions.shown_row()).expect("an ending");
        assert_eq!(ending, Ending::CloseTab(api, viewer));
        assert_eq!(confirmation(&sessions, ending), None, "nothing runs in it");
        assert!(!quits(ending, true));
        // So does its row in the list.
        let [_, row, _] = sessions.rows()[..] else {
            panic!("three rows");
        };
        assert_eq!(close_ending(row), Some(ending));
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
        let ending = close_ending(sessions.shown_row()).expect("an ending");
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
        let mut repos = Repos::new(POLL_EVERY);
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
        let repos = Repos::new(POLL_EVERY);
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
        let mut repos = Repos::new(POLL_EVERY);
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
    fn the_sessions_are_saved_as_they_open_and_close_and_kept_at_q() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let (one, two) = (base.join("one"), base.join("two"));
        for dir in [&one, &two] {
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
        let file = base.join("state/termis/sessions.json");
        let (mut kept, saved) = Kept::open(Some(file.clone()), None, true);
        assert_eq!(saved, Ok(Vec::new()));
        let saved = || restore::read(&file).expect("reads");
        let (mut sessions, mut repos) = (Sessions::new(), Repos::new(POLL_EVERY));
        let size = Size { rows: 2, cols: 10 };
        for dir in [&one, &two] {
            let place = Place::new(dir.clone(), None, None);
            let opened = {
                let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
                open::open(
                    &starter,
                    &mut sessions,
                    &mut repos,
                    place,
                    false,
                    size,
                    true,
                )
            };
            assert!(opened.is_ok());
            assert_eq!(keep(&mut kept, &sessions, false), None);
            assert_eq!(saved().last(), Some(dir), "saved as it opens");
        }
        assert_eq!(saved(), [one.clone(), two.clone()]);
        // `x` on a session closes it, and it is saved without it.
        let second = sessions.find(&two).expect("open");
        assert!(open::close(
            &mut sessions,
            &mut repos,
            second,
            true,
            Instant::now()
        ));
        assert_eq!(keep(&mut kept, &sessions, false), None);
        assert_eq!(saved(), [one.as_path()]);
        // `q`, or `x` on a terminal, leaves the sessions as they are.
        let last = sessions.shown().id;
        assert!(!forgets(Ending::Quit));
        assert!(!forgets(Ending::CloseTab(last, session::tests::tab(1))));
        assert_eq!(keep(&mut kept, &sessions, forgets(Ending::Quit)), None);
        assert_eq!(saved(), [one.as_path()]);
        // `x` on the last session, which quits, leaves none.
        assert_eq!(
            keep(&mut kept, &sessions, forgets(Ending::Close(last))),
            None
        );
        assert_eq!(saved(), Vec::<PathBuf>::new());
    }

    #[test]
    fn spike_keeps_no_sessions_and_no_restore_reopens_none_but_saves_its_own() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let (one, two) = (base.join("one"), base.join("two"));
        for dir in [&one, &two] {
            fs::create_dir(dir).expect("mkdir");
        }
        let file = base.join("sessions.json");
        let (mut kept, _) = Kept::open(Some(file.clone()), None, true);
        assert_eq!(kept.keep([one.as_path()].into_iter()), None);
        let options = |restore, full_screen| Options {
            cwd: &two,
            args: &[],
            new_conversation: false,
            restore,
            full_screen,
            panic_after: None,
        };
        let reopened = |last: &Reopen| -> Vec<PathBuf> {
            let places = last.before.iter().chain(&last.after);
            places.map(|place| place.cwd.clone()).collect()
        };
        let saved = || restore::read(&file).expect("reads");
        // By default the saved sessions open again.
        let (_, last) = last_run(Some(file.clone()), &options(true, false), None);
        assert_eq!(reopened(&last), [one.as_path()]);
        // `--spike` neither reads the file nor writes it.
        let (mut kept, last) = last_run(Some(file.clone()), &options(true, true), None);
        assert!(reopened(&last).is_empty());
        assert_eq!(kept.keep([two.as_path()].into_iter()), None);
        assert_eq!(saved(), [one.as_path()]);
        // `--no-restore` opens none of them, and its session takes their
        // place in the file.
        let (mut kept, last) = last_run(Some(file.clone()), &options(false, false), None);
        assert!(reopened(&last).is_empty());
        assert_eq!(kept.keep([two.as_path()].into_iter()), None);
        assert_eq!(saved(), [two.as_path()]);
    }

    #[test]
    fn x_and_q_take_a_second_press_while_an_agent_they_stop_works_or_asks() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let mut ids = Sessions::new();
        let (one, two) = (ids.next_id(), ids.next_id());
        let close = Ending::Close(one);
        let armed = |ending, at: Instant| Armed {
            what: ending,
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
        assert_eq!(waiting::<Ending>(None, Some(&shown)), None);
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

    #[test]
    fn p_pushes_at_the_second_press_of_the_same_push() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let refused = |text: &str| Pressed::Refused(text.to_owned());
        assert_eq!(
            press_push(None, None, t0),
            refused(strings::NOT_A_REPOSITORY)
        );
        let dirs = [(); 2].map(|()| TempDir::new().expect("dir"));
        // A repository with no remote, where the push this test starts
        // fails; git looks for none in the directories above.
        git::tests::init(dirs[0].path());
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let other = repos.get_or_spawn(dirs[1].path(), &tx).expect("spawn");
        let id = repos.get_or_spawn(dirs[0].path(), &tx).expect("spawn");
        let repo = repos.get_mut(id).expect("its repository");
        assert_eq!(press_push(Some((id, &*repo)), None, t0), Pressed::Unread);
        assert_eq!(push_from(repo), Err(Pressed::Unread), "no p offered");
        let mut snapshot = Snapshot::default();
        snapshot.status.branch = git::status::Branch {
            head: Some("main".into()),
            oid: Some("abc".into()),
            upstream: Some("origin/main".into()),
            ahead_behind: Some((0, 2)),
        };
        repo.snapshot = Some(snapshot.clone());
        assert_eq!(
            press_push(Some((id, &*repo)), None, t0),
            refused("nothing to push to origin/main")
        );
        assert!(push_from(repo).is_err(), "no p offered");
        snapshot.status.branch.ahead_behind = Some((2, 1));
        repo.snapshot = Some(snapshot.clone());
        assert!(push_from(repo).is_ok(), "p offered");
        let Pressed::Armed(first) = press_push(Some((id, &*repo)), None, t0) else {
            panic!("a first press");
        };
        assert_eq!(
            first.notice,
            "push 2 commits to origin/main · press p again"
        );
        assert_eq!(first.until, t0 + CONFIRM_WITHIN);
        let upstream = Push::Upstream {
            branch: "main".into(),
            upstream: "origin/main".into(),
            ahead: Some(2),
        };
        assert_eq!(
            press_push(Some((id, &*repo)), Some(&first), t0 + ms),
            Pressed::Push(upstream)
        );
        // Too late, from another repository, or for another push: a
        // first press again.
        let late = press_push(Some((id, &*repo)), Some(&first), t0 + CONFIRM_WITHIN);
        assert!(matches!(late, Pressed::Armed(_)), "{late:?}");
        let elsewhere = Armed {
            what: other,
            ..first.clone()
        };
        let moved = press_push(Some((id, &*repo)), Some(&elsewhere), t0 + ms);
        assert!(matches!(moved, Pressed::Armed(_)), "{moved:?}");
        snapshot.status.branch.ahead_behind = Some((3, 1));
        repo.snapshot = Some(snapshot.clone());
        let Pressed::Armed(again) = press_push(Some((id, &*repo)), Some(&first), t0 + ms) else {
            panic!("a first press again");
        };
        assert_eq!(
            again.notice,
            "push 3 commits to origin/main · press p again"
        );
        // To another remote than the upstream's, whatever the upstream
        // has.
        snapshot.status.branch.ahead_behind = Some((0, 0));
        snapshot.push_remote = Some("fork".into());
        repo.snapshot = Some(snapshot.clone());
        let Pressed::Armed(fork) = press_push(Some((id, &*repo)), None, t0) else {
            panic!("a first press");
        };
        assert_eq!(fork.notice, "push main to fork · press p again");
        snapshot.push_remote = None;
        // Without an upstream, to origin and tracking it, or nowhere.
        snapshot.status.branch.upstream = None;
        snapshot.status.branch.ahead_behind = None;
        repo.snapshot = Some(snapshot.clone());
        assert_eq!(
            press_push(Some((id, &*repo)), None, t0),
            refused("no upstream, and no remote named origin")
        );
        snapshot.origin = true;
        repo.snapshot = Some(snapshot.clone());
        let Pressed::Armed(track) = press_push(Some((id, &*repo)), None, t0) else {
            panic!("a first press");
        };
        assert_eq!(
            track.notice,
            "push main to origin and track it · press p again"
        );
        snapshot.status.branch.head = None;
        repo.snapshot = Some(snapshot.clone());
        assert_eq!(
            press_push(Some((id, &*repo)), None, t0),
            refused("nothing to push: HEAD is detached")
        );
        // While a push runs, one at a time, and `p` is not offered. This
        // one fails, there being no origin, and its end is never read.
        snapshot.status.branch.head = Some("main".into());
        repo.snapshot = Some(snapshot);
        let topic = Push::Track {
            branch: "topic".into(),
        };
        repo.push(topic, "api".into(), &tx).expect("starts");
        assert_eq!(
            press_push(Some((id, &*repo)), Some(&track), t0 + ms),
            refused(strings::PUSH_RUNNING)
        );
        assert_eq!(push_from(repo), Err(refused(strings::PUSH_RUNNING)));
    }

    #[test]
    fn any_key_but_p_lets_a_first_p_go_with_its_notice() {
        let t0 = Instant::now();
        let dir = TempDir::new().expect("dir");
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let id = repos.get_or_spawn(dir.path(), &tx).expect("spawn");
        let first = Armed {
            what: id,
            until: t0 + CONFIRM_WITHIN,
            notice: "push 1 commit to origin/main · press p again".to_owned(),
        };
        let notice = Toast::new(first.notice.clone(), toast::Kind::Error, t0, CONFIRM_WITHIN);
        let (mut armed, mut toast) = (Some(first.clone()), Some(notice.clone()));
        // p itself, and what is not a key, keep it.
        disarm_push(&mut armed, &mut toast, true, &Action::Push);
        disarm_push(
            &mut armed,
            &mut toast,
            false,
            &Action::Wheel(Pane::Agent, Step::Up),
        );
        assert_eq!(armed, Some(first.clone()));
        assert_eq!(toast, Some(notice.clone()));
        // Any other key lets it go, with its notice.
        disarm_push(&mut armed, &mut toast, true, &Action::Nothing);
        assert_eq!(armed, None);
        assert_eq!(toast, None);
        // After a first p on the changes pane, a key to another pane, or
        // a p there, which does nothing, is not a second press.
        let changes = termis(Pane::Changes);
        for (event, ctx) in [
            (plain(KeyCode::Char('1')), changes),
            (plain(KeyCode::Char('2')), changes),
            (plain(KeyCode::Tab), changes),
            (
                key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                changes,
            ),
            (plain(KeyCode::Char('c')), changes),
            (plain(KeyCode::Char('p')), termis(Pane::Projects)),
            (plain(KeyCode::Char('p')), termis(Pane::Agent)),
        ] {
            let action = translate(event, ctx);
            let (mut armed, mut toast) = (Some(first.clone()), Some(notice.clone()));
            disarm_push(&mut armed, &mut toast, true, &action);
            assert_eq!((armed, toast), (None, None), "{action:?}");
        }
        // A notice that took its place stays.
        let failed = trouble("a push is running".into());
        let (mut armed, mut toast) = (Some(first), Some(failed.clone()));
        disarm_push(&mut armed, &mut toast, true, &Action::Mode(Mode::Agent));
        assert_eq!(armed, None);
        assert_eq!(toast, Some(failed));
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
        assert_eq!(close_ending(sessions.shown_row()), Some(Ending::Close(api)));
        let zsh = terminal(&mut sessions, "exec sleep 30", &tx);
        assert_eq!(
            close_ending(sessions.shown_row()),
            Some(Ending::CloseTab(api, zsh))
        );
        // The cursor's row, wherever it is.
        let [api_row, zsh_row, web_row] = sessions.rows()[..] else {
            panic!("three rows");
        };
        assert_eq!(close_ending(api_row), Some(Ending::Close(api)));
        assert_eq!(close_ending(zsh_row), Some(Ending::CloseTab(api, zsh)));
        assert_eq!(close_ending(web_row), Some(Ending::Close(web)));
        // A repository the agent wrote in, or the count of those left
        // out, ends nothing.
        let repo = RowId::Repo {
            session: api,
            repo: RepoId::for_tests(0),
        };
        assert_eq!(close_ending(repo), None);
        assert_eq!(close_ending(RowId::More(api)), None);
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn x_takes_the_repositories_off_the_list_one_after_the_other() {
        let temporary = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = quiet_sessions(temporary.path(), &["api"], &tx);
        let mut repos = Repos::new(POLL_EVERY);
        let api = sessions.shown().id;
        let [a, b, c, d] = [1, 2, 3, 4].map(RepoId::for_tests);
        let written = [(a, "a"), (b, "b"), (c, "c"), (d, "d")].map(|(id, name)| touched::Touched {
            root: PathBuf::from("/r").join(name),
            id,
            name: name.to_owned(),
        });
        sessions.shown_mut().writes.repos = written.into();
        let repo_row = |repo| RowId::Repo { session: api, repo };
        // `x` again and again from the third: the fourth, counted until
        // then, takes its place, then the cursor goes up through the
        // others, and on the tab shown once none is left. Nothing ends on
        // the way.
        let mut cursor = Some(repo_row(c));
        for (gone, next) in [(c, Some(d)), (d, Some(b)), (b, Some(a)), (a, None)] {
            let row = sessions.cursor_row(cursor);
            assert_eq!(row, repo_row(gone));
            assert_eq!(close_ending(row), None);
            cursor = unlist(&mut sessions, &mut repos, api, gone);
            assert_eq!(cursor, next.map(repo_row), "after {gone:?}");
        }
        assert!(sessions.shown().writes.repos.is_empty());
        assert_eq!(sessions.iter().count(), 1, "the session stays");
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
