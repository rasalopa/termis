//! Agent sessions: the agent in its tab, on a pty with the emulator it
//! draws on, launched in a place with the user's arguments, the
//! terminals and the viewer opened beside it, and what the main loop
//! keeps of a session.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::host::{Host, OnPanic};
use super::repo::RepoId;
use super::shell::Shell;
use super::tree::FileTree;
use super::viewer::{ReadKey, Viewer};
use super::{Msg, SCROLLBACK};
use crate::Size;
use crate::agent::{self, State};
use crate::emu::Emulator;
use crate::git::files::Files;
use crate::instance::{Instance, Root, Watch};
use crate::projects::walk::Walked;
use crate::projects::{self, Stack};
use crate::pty::{self, ExitStatus};
use crate::strings;
use crate::ui;
use crate::ui::changes::{Movement, Step};
use crate::ui::viewer::Content;

/// After the child exited, how long to wait for its output to end.
const CLOSE_GRACE: Duration = Duration::from_millis(200);
/// How long a hung-up child gets before it is killed.
const GRACE: Duration = Duration::from_secs(2);
/// How long a killed child gets to be reaped, and its process group to
/// let go of the terminal.
const KILL_GRACE: Duration = Duration::from_millis(500);

/// A session's id, from a counter that only grows: never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionId(u32);

/// A tab's id in its session, never reused there. The agent's tab has
/// the first, and the others' grow in the order they open, which is the
/// order of the tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TabId(u32);

impl TabId {
    /// The id of the agent's tab.
    const AGENT: Self = Self(0);

    /// Whether it is the agent's tab.
    pub(super) fn is_agent(self) -> bool {
        self == Self::AGENT
    }
}

/// A row of the list of sessions: a session, by its agent's tab, one of
/// its terminals, or its viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowId {
    pub(super) session: SessionId,
    pub(super) tab: TabId,
}

/// Who sent a pty event: the session, its tab and the generation of the
/// tab's launch, which a relaunch moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TabKey {
    pub(super) session: SessionId,
    pub(super) tab: TabId,
    pub(super) generation: u32,
}

impl TabKey {
    /// The key of the first launch of tab `tab` of session `session`.
    fn first(session: SessionId, tab: TabId) -> Self {
        Self {
            session,
            tab,
            generation: Lifecycle::default().generation,
        }
    }

    /// The key of the first launch of session `session`'s agent.
    pub(super) fn agent(session: SessionId) -> Self {
        Self::first(session, TabId::AGENT)
    }
}

/// A tab of a session with a child, the agent's or a terminal's: the
/// child on its pty, the emulator it draws on with the lines that
/// scrolled off, and the lifecycle of its current launch. The viewer's
/// tab has no child; it is a [`Viewer`].
pub(super) struct Tab {
    /// What the child's events are tagged with, with its session's id.
    pub(super) id: TabId,
    /// A terminal's name, given as it opened: its shell's, or its editor's
    /// with the file's, with the lowest number no other terminal of the
    /// session had then. None for the agent's tab.
    pub(super) name: Option<String>,
    /// Whether the terminal runs the user's editor on a file rather than
    /// the user's shell.
    editor: bool,
    /// Whether the child's current launch exited and its output ended.
    pub(super) life: Lifecycle,
    /// The child's current launch.
    pty: pty::Session,
    /// The current launch's entry with the panic hook. Declared after
    /// `pty`: the hook knows the child for as long as its pty lives.
    on_panic: OnPanic,
    /// The child's screen and the lines that scrolled off it.
    pub(super) emu: Emulator,
}

impl Tab {
    /// Tab `id`, named `name` when it is a terminal, its child on the pty
    /// and panic hook entry that [`start_child`] returned, drawing on
    /// `emu`.
    fn new(
        id: TabId,
        name: Option<String>,
        (pty, on_panic): (pty::Session, OnPanic),
        emu: Emulator,
    ) -> Self {
        Self {
            id,
            name,
            editor: false,
            life: Lifecycle::default(),
            pty,
            on_panic,
            emu,
        }
    }

    /// Applies `event` from the child's launch `generation`, at `now`.
    /// The current launch's output goes to the emulator, and the replies
    /// it asks for, when there are some, to the child, with a focus-out
    /// when it turned focus events on while not `focused`. The lifecycle
    /// notes the rest and drops what an older launch sent.
    fn on_pty(&mut self, generation: u32, event: pty::Event, focused: bool, now: Instant) {
        match event {
            pty::Event::Output(bytes) if self.life.current(generation) => {
                let asked = self.emu.focus_events();
                self.emu.process(&bytes);
                let mut replies = self.emu.take_replies();
                if focus_out_due(asked, self.emu.focus_events(), focused) {
                    replies.extend_from_slice(b"\x1b[O");
                }
                if !replies.is_empty() {
                    self.pty.write(&replies);
                }
            }
            event => self.life.apply(generation, &event, now),
        }
    }

    /// Notes that the user quit. A child that exited before keeps its own
    /// exit, which may still be on its way; for any other, the exit that
    /// follows is the hangup's.
    fn quit(&mut self) {
        if !self.pty.has_exited() {
            self.life.quit();
        }
    }

    /// Resizes the emulator to `size`, then tells the child.
    fn resize(&mut self, size: Size) -> Result<()> {
        self.emu.resize(size);
        self.pty.resize(size)
    }

    /// Starts counting launch `generation`, on the pty and panic hook
    /// entry that [`start_child`] returned, with an empty emulator of
    /// `size`.
    fn relaunch(&mut self, generation: u32, launched: (pty::Session, OnPanic), size: Size) {
        self.life.relaunch(generation);
        (self.pty, self.on_panic) = launched;
        self.emu = Emulator::new(size, SCROLLBACK);
    }

    /// Queues bytes for the child as if typed.
    fn write(&self, bytes: &[u8]) {
        self.pty.write(bytes);
    }

    /// The tab's row in the list of sessions, under its session's: a
    /// terminal's. None for the agent's tab, which the session's row
    /// stands for.
    pub(super) fn row(&self) -> Option<ui::projects::TerminalRow<'_>> {
        self.name.as_deref().map(|name| ui::projects::TerminalRow {
            name,
            running: self.runs_a_program(),
        })
    }

    /// Whether the tab is a terminal with a program other than its shell
    /// in the foreground, as the terminal's foreground group says, or one
    /// that runs the editor, which may hold unsaved work until it exits.
    fn runs_a_program(&self) -> bool {
        self.name.is_some()
            && (self.editor
                || self
                    .pty
                    .foreground_group()
                    .is_some_and(|group| Some(group) != self.pty.pid()))
    }

    /// Whether `Ctrl+Z` reaches its child: a terminal's shell. Not the
    /// agent, which would wait forever for a resume, nor the editor, which
    /// `sh -c` runs without job control to resume it.
    pub(super) fn takes_suspend(&self) -> bool {
        self.name.is_some() && !self.editor
    }

    /// What a terminal that ran the editor says once it ended with a
    /// status other than success, as `vi a.txt exited (code 127)`: one
    /// that could not start too, whose error the shell wrote to the tab
    /// that closes. None for a shell, or an editor that ended well.
    fn editor_failure(&self) -> Option<String> {
        let status = self
            .life
            .ended
            .as_ref()
            .filter(|status| !status.success())?;
        let name = self.name.as_deref().filter(|_| self.editor)?;
        Some(strings::editor_failed(
            name,
            status.exit_code(),
            status.signal(),
        ))
    }
}

/// An agent session: the agent's tab and the terminals and the viewer
/// opened beside it, the agent's instance directory with the watch on its
/// telemetry, and where it runs.
pub(super) struct Session {
    /// What the session's events are tagged with.
    pub(super) id: SessionId,
    /// Where the agent runs, and the terminals.
    pub(super) place: Place,
    /// The repository the changes pane shows with the session; none
    /// outside one.
    pub(super) repo: Option<RepoId>,
    /// The user's extra arguments for the agent.
    args: Vec<String>,
    /// The agent's tab, the session's first: the agent's pty and screen,
    /// and the lifecycle of its exit and relaunch. Declared before
    /// `instance`: the agent is hung up before its directory goes.
    pub(super) agent: Tab,
    /// The terminals' tabs, after the agent's, in the order they were
    /// opened.
    terminals: Vec<Tab>,
    /// The terminals closed with `x`, while their shells are hung up.
    hanging: Vec<Hangup>,
    /// The viewer's tab, from the first file viewed until it is closed.
    viewer: Option<Viewer>,
    /// The tab on screen while the session is.
    shown: TabId,
    /// The id the next terminal or viewer gets.
    next_tab: u32,
    /// Where the agent's shims write its telemetry; without one the agent
    /// runs without telemetry.
    instance: Option<Instance>,
    /// Notices when the telemetry files change.
    watch: Watch,
    /// What the agent's telemetry said last.
    pub(super) status: agent::Status,
    /// The session's time in the agent's section.
    pub(super) clock: SessionClock,
    /// The files of the session's directory, as the projects pane shows
    /// them in place of the list of sessions when asked to.
    pub(super) tree: FileTree,
}

impl Session {
    /// Session `id` in `place`, its changes pane on `repo`: the agent, run
    /// with `args` on the pty and panic hook entry that [`spawn`] returned,
    /// with `instance` for its telemetry, draws on `emu` in its tab, the
    /// one shown.
    pub(super) fn new(
        id: SessionId,
        place: Place,
        repo: Option<RepoId>,
        args: Vec<String>,
        instance: Option<Instance>,
        launched: (pty::Session, OnPanic),
        emu: Emulator,
    ) -> Self {
        Self {
            id,
            place,
            repo,
            args,
            agent: Tab::new(TabId::AGENT, None, launched, emu),
            terminals: Vec::new(),
            hanging: Vec::new(),
            viewer: None,
            shown: TabId::AGENT,
            next_tab: TabId::AGENT.0 + 1,
            instance,
            watch: Watch::default(),
            status: agent::Status::default(),
            clock: SessionClock::default(),
            tree: FileTree::default(),
        }
    }

    /// The tab on screen while the session is, when it has a child: the
    /// agent's or a terminal's; none while the viewer is.
    pub(super) fn tab(&self) -> Option<&Tab> {
        self.viewing().is_none().then(|| self.child())
    }

    /// The terminal on screen, else the agent's tab: the tab shown unless
    /// the viewer is.
    fn child(&self) -> &Tab {
        self.terminals
            .iter()
            .find(|tab| tab.id == self.shown)
            .unwrap_or(&self.agent)
    }

    /// What the center pane shows of the tab on screen while the session
    /// is: its child's screen, or the viewer.
    pub(super) fn center(&self) -> ui::Center<'_> {
        match self.viewing() {
            Some(viewer) => ui::Center::File(viewer.pane()),
            None => ui::Center::Screen(&self.child().emu),
        }
    }

    /// The tab on screen while the session is, to change, when it has a
    /// child.
    pub(super) fn tab_mut(&mut self) -> Option<&mut Tab> {
        if self.viewing().is_some() {
            return None;
        }
        let shown = self.shown;
        Some(
            match self.terminals.iter_mut().find(|tab| tab.id == shown) {
                Some(tab) => tab,
                None => &mut self.agent,
            },
        )
    }

    /// The id of the tab on screen while the session is.
    pub(super) fn shown_tab(&self) -> TabId {
        self.shown
    }

    /// The viewer, while its tab is the one on screen.
    pub(super) fn viewing(&self) -> Option<&Viewer> {
        self.viewer
            .as_ref()
            .filter(|viewer| viewer.id == self.shown)
    }

    /// The viewer, while its tab is the one on screen, to change.
    pub(super) fn viewing_mut(&mut self) -> Option<&mut Viewer> {
        let shown = self.shown;
        self.viewer.as_mut().filter(|viewer| viewer.id == shown)
    }

    /// The ids of the session's tabs, in order: its agent's, then its
    /// terminals' and its viewer's in the order they were opened.
    pub(super) fn tab_ids(&self) -> Vec<TabId> {
        let mut others: Vec<TabId> = self
            .terminals
            .iter()
            .map(|tab| tab.id)
            .chain(self.viewer.as_ref().map(|viewer| viewer.id))
            .collect();
        others.sort_unstable();
        std::iter::once(self.agent.id).chain(others).collect()
    }

    /// The rows of the tabs after the agent's in the list of sessions,
    /// in order: each terminal's, and the viewer's, named after its file,
    /// where no program runs.
    pub(super) fn tab_rows(&self) -> Vec<ui::projects::TerminalRow<'_>> {
        self.tab_ids()
            .into_iter()
            .skip(1)
            .filter_map(|id| match &self.viewer {
                Some(viewer) if viewer.id == id => Some(ui::projects::TerminalRow {
                    name: &viewer.name,
                    running: false,
                }),
                _ => self
                    .terminals
                    .iter()
                    .find(|tab| tab.id == id)
                    .and_then(Tab::row),
            })
            .collect()
    }

    /// The session's tabs with a child, its agent's first.
    fn tabs(&self) -> impl Iterator<Item = &Tab> {
        std::iter::once(&self.agent).chain(&self.terminals)
    }

    /// The session's tabs with a child, its agent's first, to change.
    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        std::iter::once(&mut self.agent).chain(self.terminals.iter_mut())
    }

    /// Tab `id`, while the session has it and it has a child.
    fn get_tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs_mut().find(|tab| tab.id == id)
    }

    /// An id no tab of the session had before.
    fn next_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        id
    }

    /// Whether the child of the tab shown asked for focus events; the
    /// viewer has no child to ask.
    fn focus_events(&self) -> bool {
        self.tab().is_some_and(|tab| tab.emu.focus_events())
    }

    /// Starts `shell`, the user's shell or editor, in a terminal of `size`
    /// in the session's directory, its events going to `tx`, in a tab
    /// after the others, and shows it as [`Session::show_tab`] does with
    /// the host `focused` or not. The tab is named after the shell, with
    /// the lowest number no other terminal of the session has. When the
    /// shell cannot start, the id it was to have stays unused.
    pub(super) fn open_terminal(
        &mut self,
        shell: &Shell,
        size: Size,
        tx: &mpsc::Sender<Msg>,
        focused: bool,
    ) -> Result<()> {
        let id = self.next_tab_id();
        let spec = shell.spec(&self.place.cwd, size);
        let launched = start_child(&spec, tx, TabKey::first(self.id, id))
            .with_context(|| format!("start {}", shell.name()))?;
        let name = (1..)
            .map(|number| strings::terminal_name(shell.name(), number))
            .find(|name| {
                self.terminals
                    .iter()
                    .all(|tab| tab.name.as_ref() != Some(name))
            })
            .unwrap_or_default();
        let emu = Emulator::new(size, SCROLLBACK);
        let mut tab = Tab::new(id, Some(name), launched, emu);
        tab.editor = shell.is_editor();
        self.terminals.push(tab);
        self.show_tab(id, focused);
        Ok(())
    }

    /// Puts tab `id` on screen while the session is. While `focused`, the
    /// host having the focus with the session on screen, the child of the
    /// tab hidden gets a focus-out and that of the tab shown a focus-in,
    /// each when it asked for focus events. Returns whether the tab shown
    /// changed.
    pub(super) fn show_tab(&mut self, id: TabId, focused: bool) -> bool {
        if id == self.shown || !self.tab_ids().contains(&id) {
            return false;
        }
        let asked = self
            .tabs()
            .find(|tab| tab.id == id)
            .is_some_and(|tab| tab.emu.focus_events());
        let (out, into) = focus_bytes(self.focus_events(), asked, focused);
        if !out.is_empty() {
            self.write(out);
        }
        self.shown = id;
        if !into.is_empty() {
            self.write(into);
        }
        true
    }

    /// The tab after the one shown, or before it, wrapping around.
    pub(super) fn beside_tab(&self, step: Step) -> TabId {
        let ids = self.tab_ids();
        let shown = ids.iter().position(|&id| id == self.shown).unwrap_or(0);
        ids[wrapped(shown, ids.len(), step)]
    }

    /// Shows the file at `path`, absolute, in the session's viewer, read
    /// again as [`Viewer::open`] does with its answer reaching `tx`, and
    /// shows the viewer's tab as [`Session::show_tab`] does with the host
    /// `focused` or not. The first file viewed opens the viewer in a tab
    /// after the others; the next ones take its place there. Returns
    /// whether the tab shown changed.
    pub(super) fn view(&mut self, path: PathBuf, tx: &mpsc::Sender<Msg>, focused: bool) -> bool {
        let id = match &self.viewer {
            Some(viewer) => viewer.id,
            None => self.next_tab_id(),
        };
        let session = self.id;
        self.viewer
            .get_or_insert_with(|| Viewer::new(id))
            .open(path, session, tx);
        self.show_tab(id, focused)
    }

    /// Gives `content`, the answer to the read `key` names, to the
    /// viewer, which takes it when it waits for that read. Returns
    /// whether the viewer took it while its tab is the one shown.
    fn answer_read(&mut self, key: ReadKey, content: Content) -> bool {
        let shown = self.shown;
        self.viewer
            .as_mut()
            .is_some_and(|viewer| viewer.answer(key, content) && viewer.id == shown)
    }

    /// How the agent is launched.
    fn launch(&self) -> agent::Launch<'_> {
        self.place.launch(&self.args)
    }

    /// A key for the agent's next launch, whether it starts or not: no
    /// launch tagged its events with it before.
    fn next_key(&mut self) -> TabKey {
        TabKey {
            session: self.id,
            tab: self.agent.id,
            generation: self.agent.life.next_generation(),
        }
    }

    /// Applies `event` from the tab and launch that `key` names, at `now`,
    /// as [`Tab::on_pty`] does; the tab has the focus when it is the one
    /// shown while the session is `focused`. For a terminal being hung up
    /// only the end of the output and the exit are noted. An event for a
    /// tab the session does not have is dropped.
    fn on_pty(&mut self, key: TabKey, event: pty::Event, focused: bool, now: Instant) {
        let focused = focused && self.shown == key.tab;
        if let Some(tab) = self.get_tab_mut(key.tab) {
            tab.on_pty(key.generation, event, focused, now);
        } else if let Some(hangup) = self.hanging.iter_mut().find(|hangup| hangup.tab == key.tab) {
            hangup.life.apply(key.generation, &event, now);
        }
    }

    /// The terminals with a program other than their shell in the
    /// foreground, or that run the editor, in the order they were opened.
    pub(super) fn running(&self) -> impl Iterator<Item = &Tab> {
        self.terminals.iter().filter(|tab| tab.runs_a_program())
    }

    /// Closes tab `id` at `now`: the viewer at once, a terminal as
    /// [`Session::close_terminal`] does. When it was shown, the tab before
    /// it is, as [`Session::show_before`] does with `focused`. Returns
    /// whether the session had that tab; the agent's is not one.
    fn close_tab(&mut self, id: TabId, focused: bool, now: Instant) -> bool {
        let viewer = self.viewer.as_ref().is_some_and(|viewer| viewer.id == id);
        if viewer {
            self.viewer = None;
        } else if !self.close_terminal(id, now) {
            return false;
        }
        if id == self.shown {
            self.show_before(id, focused);
        }
        true
    }

    /// Closes terminal `id` at `now`: takes its tab off with its screen and
    /// hangs its shell up, which goes on through its deadlines on each
    /// [`Session::move_hangups`]. Returns whether the session had that
    /// terminal.
    fn close_terminal(&mut self, id: TabId, now: Instant) -> bool {
        let Some(index) = self.terminals.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let mut tab = self.terminals.remove(index);
        tab.quit();
        let mut hangup = Hangup::new(tab);
        hangup.step(now);
        self.hanging.push(hangup);
        true
    }

    /// Moves the hangup of each terminal closed with `x` on at `now`,
    /// never waiting, and lets go of each one done.
    fn move_hangups(&mut self, now: Instant) {
        for hangup in &mut self.hanging {
            hangup.step(now);
        }
        self.hanging.retain(|hangup| !hangup.done());
    }

    /// Shows the tab before tab `id`, which is gone: the last of the others
    /// opened before it, else the agent's. Its child gets a focus-in when
    /// it asked for focus events while `focused`, the host having the
    /// focus with the session on screen.
    fn show_before(&mut self, id: TabId, focused: bool) {
        self.shown = self
            .tab_ids()
            .into_iter()
            .rev()
            .find(|&tab| tab < id)
            .unwrap_or(self.agent.id);
        let (_, into) = focus_bytes(false, self.focus_events(), focused);
        if !into.is_empty() {
            self.write(into);
        }
    }

    /// Reads the agent's telemetry when its files changed, at `now`.
    /// Returns whether the status changed.
    fn read_telemetry(&mut self, agent: &dyn agent::Agent, now: Instant) -> bool {
        if let Some(instance) = &self.instance
            && let Some((status, events)) = self.watch.changed(instance)
        {
            let fresh = agent.status(&status, &events);
            self.clock.read(fresh.duration, now);
            if fresh != self.status {
                self.status = fresh;
                return true;
            }
        }
        false
    }

    /// Notes on each of its tabs that the user quit the session, as
    /// [`Tab::quit`] does.
    fn quit(&mut self) {
        for tab in self.tabs_mut() {
            tab.quit();
        }
    }

    /// Marks the launch of each of its tabs ended once it exited and its
    /// output ended or `CLOSE_GRACE` passed, at `now`, and closes the
    /// terminals that did, as [`Session::close_ended_terminals`] does
    /// with the host `focused` or not. Returns whether a tab just ended,
    /// and the notice of an editor among them that failed.
    fn settle(&mut self, now: Instant, focused: bool) -> (bool, Option<String>) {
        let mut ended = false;
        for tab in self.tabs_mut() {
            ended |= tab.life.settle(now);
        }
        let failed = if ended {
            self.close_ended_terminals(focused)
        } else {
            None
        };
        (ended, failed)
    }

    /// Lets each terminal whose shell ended go, after a kill of what the
    /// shell left in its process group. When it was shown, the tab before
    /// it is, as [`Session::show_before`] does with `focused`. Returns a
    /// notice naming the project when an editor among them failed, as
    /// [`Tab::editor_failure`] says, the last one when several did.
    fn close_ended_terminals(&mut self, focused: bool) -> Option<String> {
        let mut failed = None;
        while let Some(index) = self.terminals.iter().position(|tab| !tab.life.alive()) {
            let tab = self.terminals.remove(index);
            let _ = tab.pty.kill_group_now();
            if tab.id == self.shown {
                self.show_before(tab.id, focused);
            }
            if let Some(failure) = tab.editor_failure() {
                failed = Some(strings::for_project(&self.place.project, &failure));
            }
        }
        failed
    }

    /// What the agent is doing while it runs, as its telemetry said
    /// last; none before its first event and without telemetry.
    fn state(&self) -> Option<State> {
        self.status.state.filter(|_| self.agent.life.alive())
    }

    /// What the agent is doing while it runs and its telemetry says it is
    /// working or asking; none otherwise, without telemetry too.
    pub(super) fn busy(&self) -> Option<State> {
        self.state()
            .filter(|state| matches!(state, State::Working | State::Attention))
    }

    /// Takes the session apart to close it: each tab's pty with the entry
    /// with the panic hook and the lifecycle, and the instance, stay until
    /// the hangups are done, those of the terminals closed with `x`
    /// included; the emulators with their scrollback and the rest go now.
    fn into_closed(self) -> Closed {
        let Self {
            id,
            repo,
            agent,
            terminals,
            hanging,
            instance,
            ..
        } = self;
        Closed {
            id,
            repo,
            agent: Hangup::new(agent),
            terminals: terminals
                .into_iter()
                .map(Hangup::new)
                .chain(hanging)
                .collect(),
            _instance: instance,
        }
    }

    /// Resizes each of its tabs to `size` where it differs, the emulator
    /// first. Fails when a pty refused the size, once every tab was told.
    fn resize(&mut self, size: Size) -> Result<()> {
        let mut resized = Ok(());
        for tab in self.tabs_mut() {
            if tab.emu.size() != size
                && let Err(error) = tab.resize(size)
            {
                resized = Err(error);
            }
        }
        resized
    }

    /// Kills what is left of the last launch's process group, then
    /// launches the agent again on a pty of `size`, as a new generation
    /// whose events go to `tx`, with an empty emulator and card. Returns a
    /// notice when its telemetry could not be prepared again, or when it
    /// could not start: the session then stays ended on its last launch,
    /// card included. Does nothing while the last launch runs, as the kill
    /// would reach it.
    pub(super) fn relaunch(
        &mut self,
        agent: &dyn agent::Agent,
        size: Size,
        tx: &mpsc::Sender<Msg>,
    ) -> Option<String> {
        if !self.agent.pty.has_exited() {
            return None;
        }
        // What is left of the old launch's group, such as a child that
        // ignores the hangup, goes: nothing else will reach it once its
        // launch is replaced. It goes even when the new launch then cannot
        // start.
        let _ = self.agent.pty.kill_group_now();
        // The new launch starts without the old one's status and events,
        // and with the status line the settings name now.
        let mut notice = None;
        if let Some(instance) = &self.instance {
            instance.clear();
            if let Err(error) = agent.prepare(&self.launch(), instance) {
                notice = Some(strings::telemetry_trouble(&format!("{error:#}")));
            }
        }
        let key = self.next_key();
        let launched = match spawn(agent, &self.launch(), size, tx, key, self.instance.as_ref()) {
            Ok(launched) => launched,
            Err(error) => {
                // The files just cleared count as read, so the card keeps
                // the last launch's status and time.
                if let Some(instance) = &self.instance {
                    let _ = self.watch.changed(instance);
                }
                return Some(format!("{error:#}"));
            }
        };
        self.watch = Watch::default();
        self.agent.relaunch(key.generation, launched, size);
        notice
    }

    /// Queues bytes for the child of the tab shown as if typed; the
    /// viewer has none, and drops them.
    pub(super) fn write(&self, bytes: &[u8]) {
        if let Some(tab) = self.tab() {
            tab.write(bytes);
        }
    }

    /// Kills the agent's process group at once and lets the session go,
    /// its directory and its entry with the panic hook with it: for a
    /// session never shown.
    pub(super) fn discard(self) {
        let _ = self.agent.pty.kill_group_now();
    }

    /// The line to draw over the agent's screen once it ended, while its
    /// tab is shown, naming it `agent`, with `others` sessions besides
    /// this one or not.
    pub(super) fn ended_line(&self, agent: &str, others: bool) -> Option<String> {
        if self.shown != TabId::AGENT {
            return None;
        }
        self.agent.life.ended.as_ref().map(|status| {
            strings::agent_ended_line(agent, status.exit_code(), status.signal(), others)
        })
    }

    /// The card of the left pane at `now`, in seconds since the Unix
    /// epoch, with the section of the agent named `agent` and the
    /// session's `time` while the session has an instance.
    pub(super) fn card<'a>(
        &'a self,
        agent: &'a str,
        now: i64,
        time: Option<Duration>,
    ) -> ui::projects::Card<'a> {
        ui::projects::Card {
            path: &self.place.path,
            stack: self.place.stack.as_ref(),
            now,
            agent: self.instance.as_ref().map(|_| ui::projects::AgentCard {
                name: agent,
                status: &self.status,
                time,
                ended: !self.agent.life.alive(),
            }),
        }
    }

    /// The session's row in the list of sessions, its repository
    /// `changed` or not: the agent's state while it has telemetry.
    pub(super) fn row(&self, changed: bool) -> ui::projects::SessionRow<'_> {
        ui::projects::SessionRow {
            project: &self.place.project,
            status: self.instance.as_ref().map(|_| &self.status),
            ended: !self.agent.life.alive(),
            changed,
        }
    }

    /// The rows the session has in the list of sessions, its own first,
    /// then those of its other tabs, in order.
    fn rows(&self) -> impl Iterator<Item = RowId> {
        let session = self.id;
        self.tab_ids()
            .into_iter()
            .map(move |tab| RowId { session, tab })
    }
}

/// The sessions of the process, the one on screen, and those whose tabs
/// are being hung up. The list is never empty while the main loop runs.
pub(super) struct Sessions {
    list: Vec<Session>,
    /// The index of the session on screen.
    shown: usize,
    /// Sessions taken off the list to be closed, each with how far the
    /// hangup of each of its tabs went. One closed while the main loop
    /// runs goes once its hangups are done; at quit they stay, with their
    /// children's entries with the panic hook, until `Sessions` is
    /// dropped.
    closing: Vec<Closed>,
    /// The id the next session gets.
    next: u32,
}

impl Sessions {
    /// No sessions yet.
    pub(super) fn new() -> Self {
        Self {
            list: Vec::new(),
            shown: 0,
            closing: Vec::new(),
            next: 0,
        }
    }

    /// An id no session had before.
    pub(super) fn next_id(&mut self) -> SessionId {
        let id = SessionId(self.next);
        self.next += 1;
        id
    }

    /// Adds `session` after the others.
    pub(super) fn push(&mut self, session: Session) {
        self.list.push(session);
    }

    /// The sessions, in the order they were added.
    pub(super) fn iter(&self) -> impl Iterator<Item = &Session> {
        self.list.iter()
    }

    /// The session whose agent runs in `dir`, if one does.
    pub(super) fn find(&self, dir: &Path) -> Option<SessionId> {
        self.list
            .iter()
            .find(|session| session.place.cwd == dir)
            .map(|session| session.id)
    }

    /// Puts session `id` on screen. While the host is `focused`, the
    /// child of the tab hidden gets a focus-out and that of the tab shown
    /// a focus-in, each when it asked for focus events. Returns whether
    /// the session shown changed.
    pub(super) fn show(&mut self, id: SessionId, focused: bool) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        if index == self.shown {
            return false;
        }
        let (hidden, shown) = (&self.list[self.shown], &self.list[index]);
        let (out, into) = focus_bytes(hidden.focus_events(), shown.focus_events(), focused);
        for (session, bytes) in [(hidden, out), (shown, into)] {
            if !bytes.is_empty() {
                session.write(bytes);
            }
        }
        self.shown = index;
        true
    }

    /// The session after the one shown, or before it, wrapping around.
    pub(super) fn beside(&self, step: Step) -> SessionId {
        self.list[wrapped(self.shown, self.list.len(), step)].id
    }

    /// The rows of the list of sessions, in order: each session's, then
    /// those of its terminals.
    pub(super) fn rows(&self) -> Vec<RowId> {
        self.list.iter().flat_map(Session::rows).collect()
    }

    /// The row of the tab on screen.
    pub(super) fn shown_row(&self) -> RowId {
        let shown = self.shown();
        RowId {
            session: shown.id,
            tab: shown.shown,
        }
    }

    /// The row under the list's cursor: `cursor`, the row it moved to,
    /// while the list has it; else the row of the tab on screen, where
    /// the cursor starts and lands once its row left the list.
    pub(super) fn cursor_row(&self, cursor: Option<RowId>) -> RowId {
        cursor
            .filter(|row| self.rows().contains(row))
            .unwrap_or_else(|| self.shown_row())
    }

    /// The row `movement` leads to from `row` in the list of sessions,
    /// `page` rows of which show at once, stopping at both ends; counted
    /// from the row of the tab on screen when `row` is not on the list.
    pub(super) fn toward(&self, row: RowId, movement: Movement, page: usize) -> RowId {
        let rows = self.rows();
        let index = |row| rows.iter().position(|&listed| listed == row);
        let from = index(row).or_else(|| index(self.shown_row())).unwrap_or(0);
        rows[movement.index(from, rows.len(), page)]
    }

    /// Makes tab `tab` the one session `id` shows, as
    /// [`Session::show_tab`] does, the host `focused` or not while the
    /// session is on screen. Returns whether the tab it shows changed.
    pub(super) fn show_tab(&mut self, id: SessionId, tab: TabId, focused: bool) -> bool {
        let shown = self.shown().id == id;
        self.get_mut(id)
            .is_some_and(|session| session.show_tab(tab, focused && shown))
    }

    /// Where session `id` is on the list, from 0.
    pub(super) fn index(&self, id: SessionId) -> Option<usize> {
        self.list.iter().position(|session| session.id == id)
    }

    /// Where the session shown is among several, from 1, and how many
    /// there are; none while there is one.
    pub(super) fn position(&self) -> Option<(usize, usize)> {
        (self.list.len() > 1).then(|| (self.shown + 1, self.list.len()))
    }

    /// The session on screen.
    pub(super) fn shown(&self) -> &Session {
        &self.list[self.shown]
    }

    /// The session on screen, to change.
    pub(super) fn shown_mut(&mut self) -> &mut Session {
        &mut self.list[self.shown]
    }

    /// Session `id`, while it is on the list.
    pub(super) fn get(&self, id: SessionId) -> Option<&Session> {
        self.list.iter().find(|session| session.id == id)
    }

    /// Session `id`, while there is one.
    fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.list.iter_mut().find(|session| session.id == id)
    }

    /// Closes session `id` at `now`, unless it is the last on the list:
    /// takes it off the list with its screens, notes that the user quit it
    /// and hangs up the children of its tabs, which goes on through their
    /// deadlines on each [`Sessions::move_closing`]. When it was on screen
    /// the session after it is shown, else the one before, and the child
    /// of the tab that one shows gets a focus-in when it asked for focus
    /// events while the host is `focused`. Returns whether it was closed.
    pub(super) fn close(&mut self, id: SessionId, focused: bool, now: Instant) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        if self.list.len() < 2 {
            return false;
        }
        let was_shown = index == self.shown;
        self.shown = next_shown(self.shown, index, self.list.len());
        let mut session = self.list.remove(index);
        session.quit();
        let mut closed = session.into_closed();
        closed.step(now);
        self.closing.push(closed);
        if was_shown {
            let shown = self.shown();
            let (_, into) = focus_bytes(false, shown.focus_events(), focused);
            if !into.is_empty() {
                shown.write(into);
            }
        }
        true
    }

    /// Closes tab `tab` of session `id` at `now`, a terminal or the
    /// viewer, as [`Session::close_tab`] does, the child of the tab shown
    /// in its place getting the focus as the host is `focused` while the
    /// session is shown. Returns whether there was such a tab.
    pub(super) fn close_tab(
        &mut self,
        id: SessionId,
        tab: TabId,
        focused: bool,
        now: Instant,
    ) -> bool {
        let shown = self
            .list
            .get(self.shown)
            .is_some_and(|shown| shown.id == id);
        self.get_mut(id)
            .is_some_and(|session| session.close_tab(tab, focused && shown, now))
    }

    /// Gives `content`, the answer to the read `key` names, to the viewer
    /// of its session, which takes it when it waits for that read; one for
    /// a session or a viewer no longer there is dropped. Returns whether
    /// the viewer on screen took it.
    pub(super) fn file_read(&mut self, key: ReadKey, content: Content) -> bool {
        let shown = self
            .list
            .get(self.shown)
            .is_some_and(|shown| shown.id == key.session);
        self.get_mut(key.session)
            .is_some_and(|session| session.answer_read(key, content))
            && shown
    }

    /// Moves the hangup of every session being closed, and of every
    /// terminal closed with `x`, on at `now`, never waiting, and lets go
    /// of each one done: its ptys with their entries with the panic hook,
    /// and a session's instance directory.
    pub(super) fn move_closing(&mut self, now: Instant) {
        for session in &mut self.list {
            session.move_hangups(now);
        }
        self.step_closing(now);
        self.closing.retain(|closed| !closed.done());
    }

    /// Gives each session on the list that uses repository `repo` and
    /// runs in its directory `dir`, relative to the root, its tree of
    /// `files`, the files of `dir` or why they could not be listed, which
    /// came at `now`. Returns whether the session shown took them.
    pub(super) fn repo_files(
        &mut self,
        repo: RepoId,
        dir: &Path,
        files: &Result<Files, String>,
        now: Instant,
    ) -> bool {
        let files = files.as_ref().map_err(String::as_str);
        self.on_each(|session, shown| {
            let ran = session.repo == Some(repo) && session.place.below_root() == dir;
            if ran {
                session.tree.answer(files, dir, now);
            }
            ran && shown
        })
    }

    /// Gives each session on the list that runs outside a repository in
    /// the directory `walked` its tree of what the walk found, which came
    /// at `now`. Returns whether the session shown took it.
    pub(super) fn dir_files(&mut self, walked: &Walked, now: Instant) -> bool {
        let trouble;
        let files = match &walked.files {
            Ok(files) => Ok(files),
            Err(error) => {
                trouble = format!("{error:#}");
                Err(trouble.as_str())
            }
        };
        self.on_each(|session, shown| {
            let ran = session.repo.is_none() && session.place.cwd == walked.dir;
            if ran {
                session.tree.answer(files, Path::new(""), now);
            }
            ran && shown
        })
    }

    /// Has each session on the list that uses repository `repo` read its
    /// files again, once its tree is on screen.
    pub(super) fn files_changed(&mut self, repo: RepoId) {
        for session in &mut self.list {
            if session.repo == Some(repo) {
                session.tree.want();
            }
        }
    }

    /// Whether a session uses repository `repo`: one on the list, or one
    /// being closed.
    pub(super) fn uses(&self, repo: RepoId) -> bool {
        let repo = Some(repo);
        self.list.iter().any(|session| session.repo == repo)
            || self.closing.iter().any(|closed| closed.repo == repo)
    }

    /// Applies `event` from the tab and launch that `key` names, at
    /// `now`: as [`Session::on_pty`] does for a session on the list,
    /// which has the focus when shown, noting only the end of the output
    /// and the exit for one being closed. An event for a session or a tab
    /// no longer there is dropped.
    pub(super) fn on_pty(&mut self, key: TabKey, event: pty::Event, now: Instant) {
        let focused = self
            .list
            .get(self.shown)
            .is_some_and(|shown| shown.id == key.session);
        if let Some(session) = self.get_mut(key.session) {
            session.on_pty(key, event, focused, now);
        } else if let Some(tab) = self
            .closing
            .iter_mut()
            .filter(|closed| closed.id == key.session)
            .find_map(|closed| closed.get_tab_mut(key.tab))
        {
            tab.life.apply(key.generation, &event, now);
        }
    }

    /// Reads every agent's telemetry whose files changed, at `now`.
    /// Returns whether the shown session's status changed, the projects
    /// of the sessions whose agent started asking off screen: in a session
    /// not shown, or behind a terminal of the one shown; and those whose
    /// agent started asking or finished its turn, shown or not.
    pub(super) fn read_telemetry(
        &mut self,
        agent: &dyn agent::Agent,
        now: Instant,
    ) -> TelemetryRead {
        let mut read = TelemetryRead::default();
        for (index, session) in self.list.iter_mut().enumerate() {
            let before = session.state();
            if session.read_telemetry(agent, now) {
                let shown = index == self.shown;
                read.shown |= shown;
                let agent_shown = shown && session.shown.is_agent();
                let after = session.state();
                if should_tell(before, after, agent_shown) {
                    read.asking.push(session.place.project.clone());
                }
                if let Some(state) = turned(before, after) {
                    read.turns.push((session.place.project.clone(), state));
                }
            }
        }
        read
    }

    /// Marks the launch of each tab of every session ended once it
    /// exited and its output ended or `CLOSE_GRACE` passed, at `now`,
    /// and closes the terminals that did, as [`Session::settle`] does with
    /// the host `focused` or not. Returns whether one of the shown
    /// session's just did, and the notice of an editor that failed, in
    /// any session, the last one when several did.
    pub(super) fn settle(&mut self, now: Instant, focused: bool) -> Settled {
        let mut failed = None;
        let shown = self.on_each(|session, shown| {
            let (ended, failure) = session.settle(now, focused && shown);
            failed = failure.or(failed.take());
            ended
        });
        Settled { shown, failed }
    }

    /// Resizes every tab to `size` where it differs, the emulator first.
    /// Returns a notice naming the project of a session whose pty refused
    /// the size, the last one when several did.
    pub(super) fn resize(&mut self, size: Size) -> Option<String> {
        let mut notice = None;
        for session in &mut self.list {
            if let Err(error) = session.resize(size) {
                let error = format!("{error:#}");
                notice = Some(strings::for_project(&session.place.project, &error));
            }
        }
        notice
    }

    /// Runs `act` on every session on the list, with whether it is the
    /// one shown. Returns what it returned for the shown one.
    fn on_each(&mut self, mut act: impl FnMut(&mut Session, bool) -> bool) -> bool {
        let mut shown = false;
        for (index, session) in self.list.iter_mut().enumerate() {
            let answer = act(session, index == self.shown);
            if index == self.shown {
                shown = answer;
            }
        }
        shown
    }

    /// Notes on every session that the user quit. An agent that exited
    /// before keeps its own exit, which may still be on its way; for any
    /// other, the exit that follows is the hangup's.
    pub(super) fn quit(&mut self) {
        for session in &mut self.list {
            session.quit();
        }
    }

    /// Takes every session off the list and hangs up the children of its
    /// tabs, all at once, then drains `rx` until each hangup is done and
    /// an exit of the shown agent still on its way arrived or had
    /// `KILL_GRACE`: at most `GRACE` plus twice `KILL_GRACE`, however many
    /// sessions and terminals there are. Returns the shown session's exit
    /// status when its agent exited on its own before the quit.
    pub(super) fn close_all(&mut self, rx: &Receiver<Msg>) -> Option<ExitStatus> {
        let shown = self.shown().id;
        let exit_by = Instant::now() + KILL_GRACE;
        let list = self.list.drain(..);
        self.closing.extend(list.map(Session::into_closed));
        loop {
            let now = Instant::now();
            // An exit still on its way keeps the drain going once every
            // hangup is done.
            let exit_due =
                now < exit_by && self.get_closing(shown).is_some_and(Closed::exit_on_its_way);
            let Some(deadline) = self.step_closing(now).or(exit_due.then_some(exit_by)) else {
                break;
            };
            self.receive(rx, deadline);
        }
        self.get_closing(shown)
            .and_then(|closed| closed.agent.life.status())
    }

    /// Session `id`, while it is being closed.
    fn get_closing(&self, id: SessionId) -> Option<&Closed> {
        self.closing.iter().find(|closed| closed.id == id)
    }

    /// Moves the hangup of every session being closed on at `now`.
    /// Returns when the next step is due; none once all are done.
    fn step_closing(&mut self, now: Instant) -> Option<Instant> {
        for closed in &mut self.closing {
            closed.step(now);
        }
        self.closing.iter().filter_map(Closed::deadline).min()
    }

    /// Waits for a message from `rx` until `deadline`, and applies it
    /// when it is a pty event; anything else is dropped.
    fn receive(&mut self, rx: &Receiver<Msg>, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Pty(key, event)) => self.on_pty(key, event, Instant::now()),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            // Nothing can arrive any more; only the deadline can pass.
            Err(RecvTimeoutError::Disconnected) => thread::sleep(left),
        }
    }
}

/// What a read of every agent's telemetry found.
#[derive(Debug, Default)]
pub(super) struct TelemetryRead {
    /// Whether the shown session's status changed.
    pub(super) shown: bool,
    /// The projects of the sessions whose agent started asking off
    /// screen, in the order of the list.
    pub(super) asking: Vec<String>,
    /// The projects of the sessions whose agent started asking or
    /// finished its turn, shown or not, each with the state it went to,
    /// in the order of the list.
    pub(super) turns: Vec<(String, State)>,
}

/// What settling every session found.
#[derive(Debug)]
pub(super) struct Settled {
    /// Whether a tab of the shown session just ended.
    pub(super) shown: bool,
    /// The notice of an editor that ended with a failure, in any session.
    pub(super) failed: Option<String>,
}

/// What is left of a session taken off the list to be closed while the
/// children of its tabs are hung up: no screen, no telemetry, only what
/// the hangups and the agent's exit need, and its directory until the
/// session goes.
struct Closed {
    /// What its children's events are tagged with, with their tab's id.
    id: SessionId,
    /// The repository the session showed, kept in use until it goes.
    repo: Option<RepoId>,
    /// The agent's tab. Declared before `_instance`: the agent is hung up
    /// before its directory goes.
    agent: Hangup,
    /// The terminals' tabs.
    terminals: Vec<Hangup>,
    /// Where the agent's shims wrote its telemetry, kept for its drop,
    /// which removes the directory.
    _instance: Option<Instance>,
}

impl Closed {
    /// The session's tabs, its agent's first.
    fn tabs(&self) -> impl Iterator<Item = &Hangup> {
        std::iter::once(&self.agent).chain(&self.terminals)
    }

    /// Tab `id`, while it is being closed.
    fn get_tab_mut(&mut self, id: TabId) -> Option<&mut Hangup> {
        std::iter::once(&mut self.agent)
            .chain(self.terminals.iter_mut())
            .find(|tab| tab.tab == id)
    }

    /// Moves the hangup of every tab on at `now`, as [`Hangup::step`]
    /// does.
    fn step(&mut self, now: Instant) {
        self.agent.step(now);
        for terminal in &mut self.terminals {
            terminal.step(now);
        }
    }

    /// Whether every tab's hangup is done.
    fn done(&self) -> bool {
        self.tabs().all(Hangup::done)
    }

    /// When the next step of a tab's hangup is due; none once all are
    /// done, and before the first step.
    fn deadline(&self) -> Option<Instant> {
        self.tabs().filter_map(Hangup::deadline).min()
    }

    /// Whether the agent exited before the user quit and its exit has not
    /// arrived yet.
    fn exit_on_its_way(&self) -> bool {
        let agent = &self.agent;
        agent.pty.has_exited() && !agent.life.quit && agent.life.exited.is_none()
    }
}

/// A tab taken off its session to be closed while its child is hung up:
/// no screen, only what the hangup and the child's exit need.
struct Hangup {
    /// The tab's id, which its child's events carry.
    tab: TabId,
    /// Whether the child's launch exited and its output ended.
    life: Lifecycle,
    /// The child's launch.
    pty: pty::Session,
    /// The launch's entry with the panic hook, kept for its drop. Declared
    /// after `pty`: the hook knows the child for as long as its pty lives.
    _on_panic: OnPanic,
    /// Whether the child is a terminal's shell, whose hangup reaches what
    /// runs in the terminal's foreground too.
    shell: bool,
    /// How far the hangup of the child's process group went.
    stage: Closing,
    /// The terminal's foreground group while it is not the shell's, as
    /// read at the hangup, with when it gets killed unless it is gone by
    /// then; none once it was.
    foreground: Option<(u32, Instant)>,
}

impl Hangup {
    /// `tab`, its hangup not started.
    fn new(tab: Tab) -> Self {
        let Tab {
            id,
            name,
            life,
            pty,
            on_panic,
            ..
        } = tab;
        Self {
            tab: id,
            life,
            pty,
            _on_panic: on_panic,
            shell: name.is_some(),
            stage: Closing::Start,
            foreground: None,
        }
    }

    /// Moves the hangup on at `now` through every stage it reaches,
    /// sending the child's process group the signal of each. A shell's
    /// terminal has its foreground group read at the first step, while
    /// the shell may still run: when it is not the shell's, it gets the
    /// hangup too, and SIGKILL `GRACE` later unless it is gone by then.
    fn step(&mut self, now: Instant) {
        loop {
            let next = self
                .stage
                .step(self.pty.has_exited(), self.life.closed, now);
            if next == self.stage {
                break;
            }
            if self.stage == Closing::Start
                && self.shell
                && let Some(group) = self.pty.foreground_group()
                && Some(group) != self.pty.pid()
            {
                let _ = self.pty.hangup_foreground(group);
                self.foreground = Some((group, now + GRACE));
            }
            let _ = match next {
                Closing::HungUp(_) => self.pty.hangup(),
                Closing::Killed(_) => self.pty.kill_now(),
                Closing::GroupKilled(_) => self.pty.kill_group_now(),
                Closing::Start | Closing::Done => Ok(()),
            };
            self.stage = next;
        }
        if let Some((group, kill_at)) = self.foreground {
            if !self.pty.foreground_runs(group) {
                self.foreground = None;
            } else if now >= kill_at {
                let _ = self.pty.kill_foreground_now(group);
                self.foreground = None;
            }
        }
    }

    /// Whether the hangup is done: the child's process group's, and the
    /// foreground group's.
    fn done(&self) -> bool {
        self.stage == Closing::Done && self.foreground.is_none()
    }

    /// When the next step is due; none once done, and before the first
    /// step.
    fn deadline(&self) -> Option<Instant> {
        let foreground = self.foreground.map(|(_, kill_at)| kill_at);
        self.stage.deadline().into_iter().chain(foreground).min()
    }
}

/// How far the hangup of a tab's child went: the signal its process
/// group got last, with when the wait after it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    /// Nothing sent yet.
    Start,
    /// Hung up: until then for the child to exit.
    HungUp(Instant),
    /// Killed, as the child outlived the hangup: until then for it to be
    /// reaped.
    Killed(Instant),
    /// The group killed, as the child exited and may have left something
    /// in it, or was not reaped after its kill while something in it held
    /// the terminal: until then for the output to end.
    GroupKilled(Instant),
    /// Nothing left to wait for.
    Done,
}

impl Closing {
    /// The stage after this one at `now`, given whether the child
    /// `exited` and its output `closed`. The child is hung up and gets
    /// `GRACE` to exit, then is killed with its group and gets
    /// `KILL_GRACE`. Once it exited, the group is killed, so nothing the
    /// child left in it outlives its tab; once that passed with the
    /// child still there, the group is killed again unless the output
    /// ended. After a group kill the output gets `KILL_GRACE` more to end.
    fn step(self, exited: bool, closed: bool, now: Instant) -> Self {
        let group_killed = Self::GroupKilled(now + KILL_GRACE);
        match self {
            Self::Start | Self::HungUp(_) | Self::Killed(_) if exited => group_killed,
            Self::Start => Self::HungUp(now + GRACE),
            Self::HungUp(deadline) if now >= deadline => Self::Killed(now + KILL_GRACE),
            // The kill reached the whole group.
            Self::Killed(deadline) if now >= deadline && closed => Self::Done,
            Self::Killed(deadline) if now >= deadline => group_killed,
            Self::GroupKilled(deadline) if closed || now >= deadline => Self::Done,
            Self::HungUp(_) | Self::Killed(_) | Self::GroupKilled(_) | Self::Done => self,
        }
    }

    /// When the wait of the stage reached ends; none before the first
    /// step and once done.
    fn deadline(self) -> Option<Instant> {
        match self {
            Self::HungUp(deadline) | Self::Killed(deadline) | Self::GroupKilled(deadline) => {
                Some(deadline)
            }
            Self::Start | Self::Done => None,
        }
    }
}

/// Where a session runs.
pub(super) struct Place {
    /// The directory the agent runs in.
    pub(super) cwd: PathBuf,
    /// The root of the repository around `cwd`; none outside one.
    pub(super) repo: Option<PathBuf>,
    /// The project's directory: the repository, else `cwd`.
    root: PathBuf,
    /// The project's name, the last component of `root`.
    pub(super) project: String,
    /// `root` with the home directory as `~`, for the card.
    path: String,
    /// What the project is built with, when a manifest said.
    stack: Option<Stack>,
}

impl Place {
    /// The place of a session in `cwd`, inside `repo` when there is one,
    /// with `home` shown as `~`. Runs no git: the caller found `repo`.
    pub(super) fn new(cwd: PathBuf, repo: Option<PathBuf>, home: Option<&Path>) -> Self {
        // The project is the repository, not the subdirectory the agent runs in.
        let root = repo.clone().unwrap_or_else(|| cwd.clone());
        let project = project_name(&root);
        let stack = projects::detect(&root);
        let path = ui::projects::display_path(&root, home);
        Self {
            cwd,
            repo,
            root,
            project,
            path,
            stack,
        }
    }

    /// Where the agent runs, relative to the repository's root: empty at
    /// the root and outside a repository.
    pub(super) fn below_root(&self) -> &Path {
        self.repo
            .as_deref()
            .and_then(|root| self.cwd.strip_prefix(root).ok())
            .unwrap_or(Path::new(""))
    }

    /// How the agent is launched here, with `args`, continuing a
    /// conversation when it can.
    pub(super) fn launch<'a>(&'a self, args: &'a [String]) -> agent::Launch<'a> {
        agent::Launch {
            cwd: &self.cwd,
            root: &self.root,
            args,
            new_conversation: false,
        }
    }
}

/// What the main loop knows about the current launch of a tab's child.
#[derive(Debug, Default)]
pub(super) struct Lifecycle {
    /// The current launch's number; pty events from any other are
    /// ignored.
    generation: u32,
    /// The last number a launch took, one that could not start too: no
    /// two launches share one.
    launched: u32,
    /// The launch's reader saw the end of its output.
    closed: bool,
    /// The launch's exit status and when it arrived.
    pub(super) exited: Option<(ExitStatus, Instant)>,
    /// The launch is over for the user: exited, and its output ended or
    /// the grace passed.
    ended: Option<ExitStatus>,
    /// The user quit: an exit noted from now on is the one the shutdown
    /// caused, not the child's own.
    quit: bool,
}

impl Lifecycle {
    /// Notes a `Closed` or `Exited` event; events from any other launch
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

    /// Whether `from` is the current launch.
    fn current(&self, from: u32) -> bool {
        from == self.generation
    }

    /// Marks the launch ended once it exited and its output ended or
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

    /// A number for the next launch that no launch took before.
    fn next_generation(&mut self) -> u32 {
        self.launched += 1;
        self.launched
    }

    /// Starts counting launch `generation`.
    fn relaunch(&mut self, generation: u32) {
        self.generation = generation;
        self.closed = false;
        self.exited = None;
        self.ended = None;
    }

    pub(super) fn alive(&self) -> bool {
        self.ended.is_none()
    }

    /// Notes that the user quit. From then on the status is the one the
    /// agent exited with before, if any.
    fn quit(&mut self) {
        self.quit = true;
    }

    /// The exit status of the current launch, once it exited on its own.
    fn status(&self) -> Option<ExitStatus> {
        self.exited.as_ref().map(|(status, _)| status.clone())
    }
}

/// The session's time as the agent's section shows it: the duration
/// the agent last reported plus the time since it was read, stopped at
/// the agent's exit, and never behind what was shown before in the
/// launch.
#[derive(Debug, Default)]
pub(super) struct SessionClock {
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
    pub(super) fn time(&mut self, until: Instant) -> Option<Duration> {
        let time = self.reported? + until.saturating_duration_since(self.read?);
        let time = self.shown.map_or(time, |shown| shown.max(time));
        self.shown = Some(time);
        Some(time)
    }
}

/// What a switch sends the agent it hides and the one it shows: a
/// focus-out to the first and a focus-in to the second, each when its
/// agent asked for focus events, while the host is focused. With the
/// host unfocused, both agents already have no focus.
fn focus_bytes(
    hidden_asked: bool,
    shown_asked: bool,
    host_focused: bool,
) -> (&'static [u8], &'static [u8]) {
    let out: &[u8] = if hidden_asked && host_focused {
        b"\x1b[O"
    } else {
        b""
    };
    let into: &[u8] = if shown_asked && host_focused {
        b"\x1b[I"
    } else {
        b""
    };
    (out, into)
}

/// The index `step` leads to from `index` in a list of `len`: the next,
/// or the previous, wrapping around at both ends.
fn wrapped(index: usize, len: usize, step: Step) -> usize {
    let len = len.max(1);
    match step {
        Step::Down => (index + 1) % len,
        Step::Up => (index + len - 1) % len,
    }
}

/// The index of the session shown once the one at `closed` leaves a list
/// of `len` where the one at `shown` was shown: the same session when
/// another one leaves, else the one after it, or the one before when it
/// was the last.
fn next_shown(shown: usize, closed: usize, len: usize) -> usize {
    if closed < shown {
        shown - 1
    } else if closed > shown || closed + 1 < len {
        shown
    } else {
        shown.saturating_sub(1)
    }
}

/// Whether an agent that went from `previous` to `next` gets a notice and
/// the bell: when it starts asking while its tab is not `shown`, once;
/// not again while it keeps asking.
fn should_tell(previous: Option<State>, next: Option<State>, shown: bool) -> bool {
    !shown && next == Some(State::Attention) && previous != Some(State::Attention)
}

/// The state an agent that went from `previous` to `next` turned to, for
/// a desktop notification: asking, when it starts asking, once, not again
/// while it keeps asking; waiting, when it finishes its turn, working
/// before. None otherwise, and for an agent that ended, which has no
/// state.
fn turned(previous: Option<State>, next: Option<State>) -> Option<State> {
    let asks = next == Some(State::Attention) && previous != Some(State::Attention);
    let done = previous == Some(State::Working) && next == Some(State::Waiting);
    if asks || done { next } else { None }
}

/// Whether an agent is due a focus-out once its output turned its focus
/// events from `before` to `after`: once, when they turn on while it is
/// not `focused`.
fn focus_out_due(before: bool, after: bool, focused: bool) -> bool {
    !before && after && !focused
}

/// What sessions start with: the agent, the process's instance
/// directory or why it could not be made, and the channel the agents'
/// events reach.
pub(super) struct Starter<'a> {
    pub(super) agent: &'a dyn agent::Agent,
    pub(super) instances: Result<&'a Root, &'a anyhow::Error>,
    pub(super) tx: &'a mpsc::Sender<Msg>,
}

impl Starter<'_> {
    /// Starts session `id` in `place`: prepares its instance, then
    /// launches the agent with `args` on a pty of `size`, on a new
    /// conversation when `new_conversation`. The session has no
    /// repository yet. Returns it, with why it runs without telemetry when
    /// it does. When the agent cannot start, nothing is left of the
    /// session: its directory goes, and the panic hook never knew it.
    pub(super) fn start(
        &self,
        id: SessionId,
        place: Place,
        args: Vec<String>,
        new_conversation: bool,
        size: Size,
    ) -> Result<(Session, Option<String>)> {
        let launch = agent::Launch {
            new_conversation,
            ..place.launch(&args)
        };
        let (instance, notice) = instance_for(self.instances, id, self.agent, &launch);
        let key = TabKey::agent(id);
        let launched = spawn(self.agent, &launch, size, self.tx, key, instance.as_ref())?;
        let emu = Emulator::new(size, SCROLLBACK);
        let session = Session::new(id, place, None, args, instance, launched, emu);
        Ok((session, notice))
    }
}

/// An instance prepared in `instances` for session `id` of `launch`, or
/// why there is none: without one the agent runs without telemetry.
/// `instances` is the process's instance directory, or why it could not
/// be made.
fn instance_for(
    instances: Result<&Root, &anyhow::Error>,
    id: SessionId,
    agent: &dyn agent::Agent,
    launch: &agent::Launch<'_>,
) -> (Option<Instance>, Option<String>) {
    let instances = match instances {
        Ok(instances) => instances,
        Err(error) => return (None, Some(format!("{error:#}"))),
    };
    match instances.session(id.0).and_then(|instance| {
        agent.prepare(launch, &instance)?;
        Ok(instance)
    }) {
        Ok(instance) => (Some(instance), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    }
}

/// Starts the agent on a pty of `size`; its events reach `tx` tagged
/// with `key`. Returns the pty and the agent's entry with the panic
/// hook.
pub(super) fn spawn(
    agent: &dyn agent::Agent,
    launch: &agent::Launch<'_>,
    size: Size,
    tx: &mpsc::Sender<Msg>,
    key: TabKey,
    instance: Option<&Instance>,
) -> Result<(pty::Session, OnPanic)> {
    let spec = agent.spec(launch, size, instance);
    start_child(&spec, tx, key).with_context(|| format!("start {}", agent.name()))
}

/// Starts `spec` on a pty; its events reach `tx` tagged with `key`.
/// Returns the pty and the child's entry with the panic hook.
fn start_child(
    spec: &pty::Spec,
    tx: &mpsc::Sender<Msg>,
    key: TabKey,
) -> Result<(pty::Session, OnPanic)> {
    let pty = pty::Session::spawn(spec, tx.clone(), move |event| Msg::Pty(key, event))?;
    let on_panic = Host::watch_child(pty.pid(), pty.exited_flag());
    Ok((pty, on_panic))
}

/// Last path component, or the path itself when it has none.
pub(super) fn project_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::app::host::is_watched;
    use crate::app::repo::Repos;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;
    use tui_term::widget::Cursor;

    /// How long the fake agent gets for each of its events.
    const WAIT: Duration = Duration::from_secs(10);

    /// The id of a tab no session has.
    pub(in crate::app) const NO_TAB: TabId = TabId(u32::MAX);

    /// The id the `n`th tab opened after the agent's gets.
    pub(in crate::app) fn tab(n: u32) -> TabId {
        TabId(TabId::AGENT.0 + n)
    }

    /// An agent that runs this shell script and exits.
    pub(in crate::app) struct Fake(pub(in crate::app) &'static str);

    impl agent::Agent for Fake {
        fn name(&self) -> &str {
            "fake"
        }

        fn spec(&self, launch: &agent::Launch<'_>, size: Size, _: Option<&Instance>) -> pty::Spec {
            pty::Spec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), self.0.into()],
                cwd: launch.cwd.to_path_buf(),
                env: vec![("TERM".into(), "dumb".into())],
                env_default: vec![],
                env_remove: vec![],
                size,
            }
        }

        /// The model is the status file's content, when there is one.
        fn status(&self, status: &[u8], _: &[u8]) -> agent::Status {
            agent::Status {
                model: (!status.is_empty()).then(|| String::from_utf8_lossy(status).into()),
                ..agent::Status::default()
            }
        }
    }

    /// The first session, running `agent` in `dir` on a pty of `size`,
    /// the key of its launch, and both ends of the channel its events
    /// reach.
    fn start(
        agent: &Fake,
        dir: &Path,
        size: Size,
    ) -> (Sessions, TabKey, mpsc::Sender<Msg>, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = Sessions::new();
        let key = add(&mut sessions, agent, dir, size, &tx);
        (sessions, key, tx, rx)
    }

    /// Adds a session running `agent` in `dir` on a pty of `size`, its
    /// events reaching `tx`. Returns the key of its launch.
    fn add(
        sessions: &mut Sessions,
        agent: &Fake,
        dir: &Path,
        size: Size,
        tx: &mpsc::Sender<Msg>,
    ) -> TabKey {
        let place = Place::new(dir.to_path_buf(), None, None);
        let args = Vec::new();
        let id = sessions.next_id();
        let key = TabKey::agent(id);
        let launched = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            spawn(agent, &place.launch(&args), size, tx, key, None).expect("spawn")
        };
        let emu = Emulator::new(size, SCROLLBACK);
        sessions.push(Session::new(id, place, None, args, None, launched, emu));
        key
    }

    /// Applies the events of a tab's child as the main loop does, until
    /// the current launch of the tab `key` names closed its terminal and
    /// exited. Each event must be tagged `key`.
    fn run_until_ended(sessions: &mut Sessions, rx: &mpsc::Receiver<Msg>, key: TabKey) {
        let ended = |session: &mut Session| {
            session
                .get_tab_mut(key.tab)
                .is_some_and(|tab| tab.life.closed && tab.life.exited.is_some())
        };
        while !sessions.get_mut(key.session).is_some_and(ended) {
            let Msg::Pty(from, event) = rx.recv_timeout(WAIT).expect("event before timeout") else {
                panic!("only the children of tabs send here");
            };
            assert_eq!(from, key);
            sessions.on_pty(from, event, Instant::now());
        }
    }

    /// Draws `emu` at its size, without the cursor.
    fn screen(emu: &Emulator) -> Terminal<TestBackend> {
        let size = emu.size();
        let mut terminal = Terminal::new(TestBackend::new(size.cols, size.rows)).expect("terminal");
        terminal
            .draw(|frame| {
                let widget = emu.widget().cursor(Cursor::default().visibility(false));
                frame.render_widget(widget, frame.area());
            })
            .expect("draw");
        terminal
    }

    /// Opens a terminal running `script` in the session shown, on a pty
    /// of `size`, its events reaching `tx`. Returns the key of its launch.
    fn open(sessions: &mut Sessions, script: &str, size: Size, tx: &mpsc::Sender<Msg>) -> TabKey {
        let session = sessions.shown_mut();
        {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            let shell = Shell::running(script);
            session
                .open_terminal(&shell, size, tx, false)
                .expect("opens");
        }
        let terminal = session.terminals.last().expect("a terminal");
        TabKey::first(session.id, terminal.id)
    }

    /// Applies the events of `rx` as the main loop does, settling the
    /// sessions after each wait, until `done` says so.
    fn run_until(
        sessions: &mut Sessions,
        rx: &mpsc::Receiver<Msg>,
        mut done: impl FnMut(&mut Sessions) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        while !done(sessions) {
            assert!(Instant::now() < deadline, "done before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            sessions.settle(Instant::now(), false);
        }
    }

    /// Applies the events of `rx` as the main loop does until a read of a
    /// file answers, and returns the answer.
    fn read_answer(sessions: &mut Sessions, rx: &mpsc::Receiver<Msg>) -> (ReadKey, Content) {
        loop {
            match rx.recv_timeout(WAIT).expect("an answer before timeout") {
                Msg::Pty(key, event) => sessions.on_pty(key, event, Instant::now()),
                Msg::Read(key, content) => return (key, content),
                _ => panic!("only children and reads send here"),
            }
        }
    }

    /// What the viewer of the session shown shows, by line count, or
    /// none without a viewer.
    fn viewed(sessions: &Sessions) -> Option<(String, usize)> {
        let viewer = sessions.shown().viewer.as_ref()?;
        Some((viewer.name.clone(), viewer.pane().content.lines()))
    }

    #[test]
    fn the_viewer_is_one_tab_opened_after_the_others_and_reused() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let quiet = "exec sleep 30";
        let (mut sessions, agent, tx, _rx) = start(&Fake(quiet), dir.path(), size);
        let first = open(&mut sessions, quiet, size, &tx);
        let session = sessions.shown_mut();
        assert!(session.view(dir.path().join("a.txt"), &tx, false));
        let viewer = session.shown_tab();
        assert_eq!(session.tab_ids(), [agent.tab, first.tab, viewer]);
        assert!(session.tab().is_none(), "no child");
        assert_eq!(session.viewing().map(|viewer| viewer.id), Some(viewer));
        // Another file takes its place; the same file is read again.
        assert!(session.show_tab(agent.tab, false));
        assert!(session.view(dir.path().join("b.txt"), &tx, false), "shown");
        assert!(!session.view(dir.path().join("b.txt"), &tx, false));
        assert_eq!(session.tab_ids(), [agent.tab, first.tab, viewer]);
        assert_eq!(
            viewed(&sessions).map(|(name, _)| name).as_deref(),
            Some("b.txt")
        );
        // A terminal opened next goes after it; the viewer is listed as a
        // terminal where no program runs, and named in the title.
        let second = open(&mut sessions, quiet, size, &tx);
        let session = sessions.shown_mut();
        assert_eq!(
            session.tab_ids(),
            [agent.tab, first.tab, viewer, second.tab]
        );
        let rows: Vec<(&str, bool)> = session
            .tab_rows()
            .iter()
            .map(|row| (row.name, row.running))
            .collect();
        assert_eq!(rows, [("sh", false), ("b.txt", false), ("sh 2", false)]);
        assert_eq!(session.beside_tab(Step::Up), viewer);
        assert!(session.show_tab(viewer, false));
        assert_eq!(session.beside_tab(Step::Up), first.tab);
        let listed = |sessions: &Sessions| -> Vec<TabId> {
            sessions.rows().into_iter().map(|row| row.tab).collect()
        };
        assert_eq!(
            listed(&sessions),
            [agent.tab, first.tab, viewer, second.tab]
        );
        assert_eq!(sessions.shown_row().tab, viewer);
        // Closing it is at once, and the tab before it takes its place.
        let id = agent.session;
        assert!(sessions.close_tab(id, viewer, false, Instant::now()));
        assert!(sessions.shown().hanging.is_empty(), "nothing to hang up");
        assert_eq!(viewed(&sessions), None);
        assert_eq!(sessions.shown().shown_tab(), first.tab);
        assert!(
            !sessions.close_tab(id, viewer, false, Instant::now()),
            "gone"
        );
        // Opened again, it is a new tab, after the others.
        let session = sessions.shown_mut();
        assert!(session.view(dir.path().join("a.txt"), &tx, false));
        let again = session.shown_tab();
        assert!(again > second.tab);
        assert_eq!(session.tab_ids(), [agent.tab, first.tab, second.tab, again]);
        // Closing the terminal shown goes back past it.
        assert!(session.show_tab(second.tab, false));
        assert!(sessions.close_tab(id, second.tab, false, Instant::now()));
        assert_eq!(sessions.shown().shown_tab(), first.tab);
    }

    #[test]
    fn a_read_reaches_the_viewer_that_waits_for_it_and_nothing_else() {
        let temporary = TempDir::new().expect("temp dir");
        let dir = temporary.path().join("proj");
        fs::create_dir(&dir).expect("mkdir");
        fs::write(dir.join("a.txt"), "1\n2\n").expect("write");
        fs::write(dir.join("b.txt"), "b\n").expect("write");
        let size = Size { rows: 2, cols: 10 };
        let quiet = Fake("exec sleep 30");
        let (mut sessions, agent, tx, rx) = start(&quiet, &dir, size);
        sessions.shown_mut().view(dir.join("a.txt"), &tx, false);
        assert_eq!(viewed(&sessions), Some(("a.txt".into(), 0)), "reading");
        let (key, content) = read_answer(&mut sessions, &rx);
        assert!(sessions.file_read(key, content), "on screen");
        assert_eq!(viewed(&sessions), Some(("a.txt".into(), 2)));
        // An answer to an earlier read is dropped.
        sessions.shown_mut().view(dir.join("b.txt"), &tx, false);
        let (later, content) = read_answer(&mut sessions, &rx);
        assert!(!sessions.file_read(key, Content::Note(strings::FILE_BINARY)));
        assert_eq!(viewed(&sessions), Some(("b.txt".into(), 0)));
        // One taken off screen is kept.
        let session = sessions.shown_mut();
        assert!(session.show_tab(agent.tab, false));
        assert!(!sessions.file_read(later, content), "not on screen");
        assert_eq!(viewed(&sessions), Some(("b.txt".into(), 1)));
        // Once the viewer is closed, or its session, its reads reach
        // nothing.
        let viewer = later.tab;
        sessions.shown_mut().view(dir.join("a.txt"), &tx, false);
        let (closed, content) = read_answer(&mut sessions, &rx);
        assert!(sessions.close_tab(agent.session, viewer, false, Instant::now()));
        assert!(!sessions.file_read(closed, content));
        assert_eq!(viewed(&sessions), None);
        let other = add(&mut sessions, &quiet, &dir, size, &tx);
        // One for the viewer a session not on screen shows is kept,
        // without a redraw.
        sessions.shown_mut().view(dir.join("a.txt"), &tx, false);
        let (hidden, content) = read_answer(&mut sessions, &rx);
        assert!(sessions.show(other.session, false));
        assert!(
            !sessions.file_read(hidden, content),
            "its session is hidden"
        );
        let first = sessions.get(agent.session).expect("its session");
        assert_eq!(first.shown_tab(), hidden.tab);
        let viewer = first.viewer.as_ref().expect("its viewer");
        assert_eq!(viewer.pane().content.lines(), 2, "a.txt taken");
        assert!(sessions.show(agent.session, false));
        sessions.shown_mut().view(dir.join("b.txt"), &tx, false);
        let (gone, content) = read_answer(&mut sessions, &rx);
        assert!(sessions.show(other.session, false));
        assert!(sessions.close(agent.session, false, Instant::now()));
        assert!(!sessions.file_read(gone, content), "its session is gone");
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn an_editor_that_fails_says_so_as_its_tab_closes() {
        let temporary = TempDir::new().expect("temp dir");
        let dir = temporary.path().join("proj");
        fs::create_dir(&dir).expect("mkdir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, _agent, tx, rx) = start(&Fake("exec sleep 30"), &dir, size);
        // Runs `shell` in a terminal until its tab closes, and returns
        // the notice the settle that closed it gave.
        let mut run = |shell: &Shell| {
            let session = sessions.shown_mut();
            {
                let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
                session
                    .open_terminal(shell, size, &tx, false)
                    .expect("opens");
            }
            let tab = session.shown_tab();
            let deadline = Instant::now() + WAIT;
            loop {
                assert!(Instant::now() < deadline, "closes before timeout");
                if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                    sessions.on_pty(key, event, Instant::now());
                }
                let settled = sessions.settle(Instant::now(), false);
                if !sessions.shown().tab_ids().contains(&tab) {
                    return settled.failed;
                }
            }
        };
        // As an editor that cannot start: sh exits 127.
        assert_eq!(
            run(&Shell::editing("exit 127")),
            Some("proj: vi a.txt exited (code 127)".to_owned())
        );
        assert_eq!(run(&Shell::editing("exit 0")), None, "ended well");
        assert_eq!(run(&Shell::running("exit 3")), None, "a shell");
        sessions.quit();
        sessions.close_all(&rx);
    }

    #[test]
    fn a_terminal_runs_in_a_tab_of_its_own_until_its_shell_exits() {
        let temporary = TempDir::new().expect("temp dir");
        let dir = temporary.path().join("proj");
        fs::create_dir(&dir).expect("mkdir");
        let size = Size { rows: 2, cols: 10 };
        // The agent stays quiet until it is hung up.
        let (mut sessions, agent, tx, rx) = start(&Fake("exec sleep 30"), &dir, size);
        // Prints the name of the directory it runs in, and exits.
        let key = open(&mut sessions, r#"printf "${PWD##*/}""#, size, &tx);
        assert_eq!(key.session, agent.session);
        assert_ne!(key.tab, agent.tab);
        let session = sessions.shown();
        assert_eq!(session.shown_tab(), key.tab, "shown");
        assert_eq!(session.terminals[0].name.as_deref(), Some("sh"));
        let flag = session.terminals[0].pty.exited_flag();
        assert!(is_watched(&flag), "the panic hook knows the shell");
        run_until_ended(&mut sessions, &rx, key);
        let session = sessions.get_mut(key.session).expect("its session");
        let terminal = session.get_tab_mut(key.tab).expect("not settled yet");
        screen(&terminal.emu)
            .backend()
            .assert_buffer_lines(["proj      ", "          "]);
        // Once it settled the tab goes, and the agent's is shown again.
        assert!(sessions.settle(Instant::now(), false).shown, "ended");
        let session = sessions.shown();
        assert!(session.terminals.is_empty());
        assert_eq!(session.shown_tab(), agent.tab);
        assert!(!is_watched(&flag), "forgotten with its tab");
        // What it sends late reaches nothing, the agent's screen least.
        sessions.on_pty(key, pty::Event::Output(b"late".to_vec()), Instant::now());
        let exited = pty::Event::Exited(ExitStatus::with_exit_code(0));
        sessions.on_pty(key, exited, Instant::now());
        screen(&sessions.shown().agent.emu)
            .backend()
            .assert_buffer_lines(["          ", "          "]);
        assert!(
            sessions.shown().agent.life.exited.is_none(),
            "not the agent's"
        );
    }

    #[test]
    fn a_terminal_that_closes_shows_the_tab_before_it_and_kills_its_group() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        let first = open(&mut sessions, "read line", size, &tx);
        // Leaves a child that ignores the hangup in its group, and exits.
        let second = open(&mut sessions, "trap '' HUP; sleep 60 & exit 0", size, &tx);
        let shown = |sessions: &Sessions| sessions.shown().shown_tab();
        assert_eq!(shown(&sessions), second.tab);
        let group = sessions.shown().terminals[1].pty.pid().expect("a pid");
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().terminals.len() == 1
        });
        assert_eq!(shown(&sessions), first.tab, "the one before it");
        assert!(pty::group_ends_within(group, WAIT), "what it left went");
        // One closing while another tab is shown leaves that one.
        assert!(sessions.shown_mut().show_tab(agent.tab, false));
        sessions.shown().terminals[0].write(b"\r");
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().terminals.is_empty()
        });
        assert_eq!(shown(&sessions), agent.tab);
    }

    #[test]
    fn the_tabs_follow_one_another_wrapping_around() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let quiet = "exec sleep 30";
        let (mut sessions, agent, tx, _rx) = start(&Fake(quiet), dir.path(), size);
        let session = sessions.shown_mut();
        assert_eq!(session.beside_tab(Step::Down), agent.tab, "one tab");
        assert_eq!(session.beside_tab(Step::Up), agent.tab);
        assert!(!session.show_tab(agent.tab, true), "shown already");
        let first = open(&mut sessions, quiet, size, &tx);
        let second = open(&mut sessions, quiet, size, &tx);
        let session = sessions.shown_mut();
        assert_eq!(session.shown_tab(), second.tab, "the last opened");
        assert_eq!(session.beside_tab(Step::Down), agent.tab, "wrapping");
        assert_eq!(session.beside_tab(Step::Up), first.tab);
        assert!(session.show_tab(agent.tab, true));
        assert_eq!(session.beside_tab(Step::Up), second.tab, "wrapping");
        assert_eq!(session.beside_tab(Step::Down), first.tab);
        assert!(!session.show_tab(NO_TAB, true), "not a tab");
        let names: Vec<_> = session
            .terminals
            .iter()
            .map(|tab| tab.name.as_deref())
            .collect();
        assert_eq!(names, [Some("sh"), Some("sh 2")]);
        assert_eq!(session.agent.name, None);
    }

    #[test]
    fn a_terminal_takes_the_lowest_number_no_other_has() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let quiet = "exec sleep 30";
        let (mut sessions, _agent, tx, rx) = start(&Fake(quiet), dir.path(), size);
        let names = |sessions: &Sessions| -> Vec<String> {
            let terminals = &sessions.shown().terminals;
            terminals
                .iter()
                .filter_map(|tab| tab.name.clone())
                .collect()
        };
        open(&mut sessions, quiet, size, &tx);
        let middle = open(&mut sessions, "read line", size, &tx);
        let last = open(&mut sessions, quiet, size, &tx);
        assert_eq!(names(&sessions), ["sh", "sh 2", "sh 3"]);
        sessions.shown().terminals[1].write(b"\r");
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().terminals.len() == 2
        });
        let again = open(&mut sessions, quiet, size, &tx);
        assert_eq!(names(&sessions), ["sh", "sh 3", "sh 2"]);
        assert!(again.tab != middle.tab && again.tab != last.tab, "a new id");
    }

    #[test]
    fn a_tab_hidden_gets_a_focus_out_and_one_shown_a_focus_in() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        let (mut sessions, agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        // Asks for focus events, then prints the six bytes it gets with
        // the escape as `E`.
        let script =
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=6 2>/dev/null | tr '\033' E";
        let key = open(&mut sessions, script, size, &tx);
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().tab().expect("a child").emu.focus_events()
        });
        // Shown, it got no focus-out; hidden, it gets one, and shown
        // again, a focus-in.
        assert!(sessions.shown_mut().show_tab(agent.tab, true));
        assert!(sessions.shown_mut().show_tab(key.tab, true));
        run_until_ended(&mut sessions, &rx, key);
        screen(&sessions.shown().tab().expect("a child").emu)
            .backend()
            .assert_buffer_lines(["E[OE[I      ", "            "]);
    }

    #[test]
    fn the_tab_shown_once_a_terminal_closes_gets_a_focus_in() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        let (mut sessions, _agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        // Asks for focus events, then prints the three bytes it gets with
        // the escape as `E`.
        let script =
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=3 2>/dev/null | tr '\033' E";
        let before = open(&mut sessions, script, size, &tx);
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().tab().expect("a child").emu.focus_events()
        });
        open(&mut sessions, "read line", size, &tx);
        sessions.shown().write(b"\r");
        // Settled with the host focused, the terminal that closes leaves
        // its place to the one before it, which gets a focus-in.
        let deadline = Instant::now() + WAIT;
        while sessions.shown().terminals.len() > 1 {
            assert!(Instant::now() < deadline, "closes before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            sessions.settle(Instant::now(), true);
        }
        assert_eq!(sessions.shown().shown_tab(), before.tab);
        run_until(&mut sessions, &rx, |sessions| {
            let terminal = screen(&sessions.shown().tab().expect("a child").emu);
            let cells = &terminal.backend().buffer().content()[..3];
            cells.iter().map(|cell| cell.symbol()).collect::<String>() == "E[I"
        });
    }

    /// A shell that runs, in its terminal's foreground, a child that
    /// ignores the hangup, and says so once it does.
    const SERVES: &str = "set -m; sh -c 'trap \"\" HUP; printf serving; exec sleep 60'";

    /// Applies events until the terminal `key` names has a foreground
    /// group other than its shell's, which said it is serving. Returns the
    /// shell's group and that one.
    fn foreground_job(
        sessions: &mut Sessions,
        rx: &mpsc::Receiver<Msg>,
        key: TabKey,
    ) -> (u32, u32) {
        let job = |sessions: &mut Sessions| {
            let session = sessions.get_mut(key.session)?;
            let tab = session.get_tab_mut(key.tab)?;
            let text: String = screen(&tab.emu)
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            let shell = tab.pty.pid()?;
            tab.pty
                .foreground_group()
                .filter(|&group| group != shell && text.contains("serving"))
                .map(|group| (shell, group))
        };
        let mut found = None;
        run_until(sessions, rx, |sessions| {
            found = job(sessions);
            found.is_some()
        });
        found.expect("a foreground job")
    }

    #[test]
    fn quitting_hangs_up_what_runs_in_a_terminals_foreground() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, _agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        let key = open(&mut sessions, SERVES, size, &tx);
        let (shell, job) = foreground_job(&mut sessions, &rx, key);
        let started = Instant::now();
        sessions.quit();
        sessions.close_all(&rx);
        let took = started.elapsed();
        assert!(took >= GRACE, "the job outlived the hangup: {took:?}");
        assert!(took < GRACE + 2 * KILL_GRACE, "killed at GRACE: {took:?}");
        assert!(pty::group_ends_within(job, WAIT), "the job went");
        assert!(pty::group_ends_within(shell, WAIT), "the shell went");
    }

    #[test]
    fn closing_a_session_hangs_up_what_runs_in_its_terminals_foreground() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let quiet = Fake("exec sleep 30");
        let (mut sessions, agent, tx, rx) = start(&quiet, dir.path(), size);
        let key = open(&mut sessions, SERVES, size, &tx);
        let (shell, job) = foreground_job(&mut sessions, &rx, key);
        // Another session, so this one can close.
        add(&mut sessions, &quiet, dir.path(), size, &tx);
        let started = Instant::now();
        assert!(sessions.close(agent.session, false, started));
        // Whether the end of the shell's output or its exit reached its
        // tab's hangup.
        let mut heard = false;
        while sessions.get_closing(agent.session).is_some() {
            assert!(started.elapsed() < WAIT, "done before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            let closed = sessions.get_closing(agent.session).expect("closing");
            let life = &closed.terminals[0].life;
            heard |= life.closed || life.exited.is_some();
            sessions.move_closing(Instant::now());
        }
        let took = started.elapsed();
        assert!(took >= GRACE, "the job outlived the hangup: {took:?}");
        assert!(heard, "the shell's end reached its tab's hangup");
        assert!(pty::group_ends_within(job, WAIT), "the job went");
        assert!(pty::group_ends_within(shell, WAIT), "the shell went");
    }

    #[test]
    fn closing_a_terminal_hangs_up_what_runs_in_its_foreground() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        let before = open(&mut sessions, "read line", size, &tx);
        let key = open(&mut sessions, SERVES, size, &tx);
        let (shell, job) = foreground_job(&mut sessions, &rx, key);
        let flag = sessions.shown().terminals[1].pty.exited_flag();
        let running: Vec<TabId> = sessions.shown().running().map(|tab| tab.id).collect();
        assert_eq!(running, [key.tab], "a program runs in it alone");
        assert!(
            !sessions.close_tab(agent.session, agent.tab, false, Instant::now()),
            "the agent's is not a terminal"
        );
        let started = Instant::now();
        assert!(sessions.close_tab(key.session, key.tab, false, started));
        let session = sessions.shown();
        assert_eq!(session.shown_tab(), before.tab, "the one before it");
        assert_eq!(session.terminals.len(), 1);
        assert!(is_watched(&flag), "known to the panic hook while hung up");
        // Whether the end of the shell's output or its exit reached its
        // hangup.
        let mut heard = false;
        while !sessions.shown().hanging.is_empty() {
            assert!(started.elapsed() < WAIT, "done before timeout");
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                sessions.on_pty(key, event, Instant::now());
            }
            let life = &sessions.shown().hanging[0].life;
            heard |= life.closed || life.exited.is_some();
            sessions.move_closing(Instant::now());
        }
        let took = started.elapsed();
        assert!(took >= GRACE, "the job outlived the hangup: {took:?}");
        assert!(heard, "the shell's end reached its hangup");
        assert!(!is_watched(&flag), "forgotten once hung up");
        assert!(pty::group_ends_within(job, WAIT), "the job went");
        assert!(pty::group_ends_within(shell, WAIT), "the shell went");
        // The session and its other terminal run on.
        assert!(sessions.get(agent.session).is_some());
        assert!(!sessions.shown().terminals[0].life.closed);
    }

    #[test]
    fn a_terminal_being_hung_up_goes_with_its_session() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, _agent, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        let key = open(&mut sessions, SERVES, size, &tx);
        let (shell, job) = foreground_job(&mut sessions, &rx, key);
        assert!(sessions.close_tab(key.session, key.tab, true, Instant::now()));
        // Quitting at once takes the hangup over.
        sessions.quit();
        sessions.close_all(&rx);
        assert!(sessions.closing.iter().all(Closed::done));
        assert!(pty::group_ends_within(job, WAIT), "the job went");
        assert!(pty::group_ends_within(shell, WAIT), "the shell went");
    }

    #[test]
    fn a_spawned_agent_reaches_its_session_through_its_key() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("printf hi");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, tx, rx) = start(&agent, dir.path(), size);
        let first = sessions.shown().agent.pty.exited_flag();
        assert!(is_watched(&first), "the panic hook knows the agent");
        let other = sessions.next_id();
        assert!(sessions.get_mut(other).is_none(), "an id is not a session");
        run_until_ended(&mut sessions, &rx, key);
        let session = sessions.shown();
        assert_eq!(session.agent.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.agent.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        // The next launch tags its events with the next generation and
        // draws on an empty screen.
        let notice = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            sessions.shown_mut().relaunch(&agent, size, &tx)
        };
        assert_eq!(notice, None, "no instance to prepare");
        let second = sessions.shown().agent.pty.exited_flag();
        assert!(!is_watched(&first), "the old launch is forgotten");
        assert!(is_watched(&second));
        let next = TabKey {
            generation: key.generation + 1,
            ..key
        };
        // Output an older launch sends late is not drawn.
        sessions.on_pty(key, pty::Event::Output(b"old".to_vec()), Instant::now());
        run_until_ended(&mut sessions, &rx, next);
        let session = sessions.shown();
        assert_eq!(session.agent.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.agent.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        drop(sessions);
        assert!(!is_watched(&second), "a closed session is forgotten");
    }

    #[test]
    fn an_event_for_a_tab_that_is_not_there_reaches_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, _tx, rx) = start(&Fake("printf hi"), dir.path(), size);
        let elsewhere = TabKey { tab: NO_TAB, ..key };
        let now = Instant::now();
        sessions.on_pty(elsewhere, pty::Event::Output(b"other".to_vec()), now);
        let exited = pty::Event::Exited(ExitStatus::with_exit_code(1));
        sessions.on_pty(elsewhere, exited, now);
        sessions.on_pty(elsewhere, pty::Event::Closed, now);
        let life = &sessions.shown().agent.life;
        assert!(life.exited.is_none() && !life.closed, "not the agent's");
        run_until_ended(&mut sessions, &rx, key);
        let session = sessions.shown();
        let status = session.agent.life.status();
        assert_eq!(status.map(|s| s.exit_code()), Some(0));
        screen(&session.agent.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        // Nor does it reach a session being closed. Its agent stays quiet
        // until it is hung up.
        let (quiet_tx, _quiet_rx) = mpsc::channel::<Msg>();
        let quiet = add(
            &mut sessions,
            &Fake("exec sleep 30"),
            dir.path(),
            size,
            &quiet_tx,
        );
        assert!(sessions.close(quiet.session, false, Instant::now()));
        let closed = |sessions: &Sessions| {
            let closing = sessions.get_closing(quiet.session).expect("being closed");
            closing.agent.life.closed
        };
        let elsewhere = TabKey {
            tab: NO_TAB,
            ..quiet
        };
        sessions.on_pty(elsewhere, pty::Event::Closed, Instant::now());
        assert!(!closed(&sessions), "not its agent's");
        sessions.on_pty(quiet, pty::Event::Closed, Instant::now());
        assert!(closed(&sessions), "its agent's");
    }

    #[test]
    fn a_relaunch_that_cannot_start_leaves_the_session_ended() {
        let dir = TempDir::new().expect("temp dir");
        let cwd = dir.path().join("project");
        fs::create_dir(&cwd).expect("mkdir");
        let agent = Fake("printf hi");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, tx, rx) = start(&agent, &cwd, size);
        let telemetry = dir.path().join("instance");
        fs::create_dir(&telemetry).expect("mkdir");
        sessions.shown_mut().instance = Some(Instance::at(telemetry.clone(), "/bin/sh".into()));
        let relaunch = |sessions: &mut Sessions| {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            sessions.shown_mut().relaunch(&agent, size, &tx)
        };
        let model = |sessions: &mut Sessions| {
            sessions.read_telemetry(&agent, Instant::now());
            sessions.shown().status.model.clone()
        };
        run_until_ended(&mut sessions, &rx, key);
        // A launch that starts first, so that there is an older one.
        assert_eq!(relaunch(&mut sessions), None);
        let next = TabKey {
            generation: key.generation + 1,
            ..key
        };
        run_until_ended(&mut sessions, &rx, next);
        assert!(sessions.settle(Instant::now(), false).shown, "ended");
        fs::write(telemetry.join(crate::telemetry::STATUS_FILE), "M").expect("write");
        assert_eq!(model(&mut sessions).as_deref(), Some("M"));
        let last = sessions.shown().agent.pty.exited_flag();
        // The directory went: the spawn refuses it.
        fs::remove_dir(&cwd).expect("rmdir");
        let notice = relaunch(&mut sessions).expect("a notice");
        assert!(notice.contains("is not a directory"), "{notice}");
        assert_eq!(model(&mut sessions).as_deref(), Some("M"), "the card stays");
        let session = sessions.shown();
        assert_eq!(session.agent.life.generation, next.generation, "unchanged");
        assert!(!session.agent.life.alive(), "still ended");
        assert!(is_watched(&last), "the last launch is still known");
        // Stragglers of the older launch are still ignored.
        sessions.on_pty(key, pty::Event::Output(b"old".to_vec()), Instant::now());
        sessions.on_pty(
            key,
            pty::Event::Exited(ExitStatus::with_exit_code(1)),
            Instant::now(),
        );
        let session = sessions.shown();
        assert_eq!(session.agent.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.agent.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        // The launch that could not start kept its number.
        fs::create_dir(&cwd).expect("mkdir");
        assert_eq!(relaunch(&mut sessions), None);
        assert_eq!(model(&mut sessions), None, "a new launch starts empty");
        let failed = TabKey {
            generation: next.generation + 1,
            ..key
        };
        let third = TabKey {
            generation: next.generation + 2,
            ..key
        };
        assert_eq!(sessions.shown().agent.life.generation, third.generation);
        sessions.on_pty(
            failed,
            pty::Event::Exited(ExitStatus::with_exit_code(1)),
            Instant::now(),
        );
        assert!(
            sessions.shown().agent.life.exited.is_none(),
            "not the new launch's"
        );
        run_until_ended(&mut sessions, &rx, third);
        let status = sessions.shown().agent.life.status();
        assert_eq!(status.map(|s| s.exit_code()), Some(0));
    }

    #[test]
    fn a_row_has_the_agents_state_only_while_it_has_telemetry() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("printf hi");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, _tx, rx) = start(&agent, dir.path(), size);
        let row = sessions.shown().row(false);
        assert_eq!(row.project, project_name(dir.path()));
        assert!(row.status.is_none(), "no telemetry");
        assert!(!row.ended && !row.changed);
        assert!(sessions.shown().row(true).changed);
        let telemetry = dir.path().join("instance");
        sessions.shown_mut().instance = Some(Instance::at(telemetry, "/bin/sh".into()));
        assert!(sessions.shown().row(false).status.is_some(), "telemetry");
        run_until_ended(&mut sessions, &rx, key);
        assert!(!sessions.shown().row(false).ended, "not settled yet");
        assert!(sessions.settle(Instant::now(), false).shown);
        let row = sessions.shown().row(false);
        assert!(row.ended && row.status.is_some(), "ended");
    }

    #[test]
    fn an_agent_gets_the_reply_to_its_query() {
        let dir = TempDir::new().expect("temp dir");
        // Asks for the primary device attributes, then prints the nine
        // bytes of the reply with the escape as `E`.
        let agent =
            Fake(r"stty raw -echo; printf '\033[c'; dd bs=1 count=9 2>/dev/null | tr '\033' E");
        let size = Size { rows: 2, cols: 12 };
        let (mut sessions, key, _tx, rx) = start(&agent, dir.path(), size);
        run_until_ended(&mut sessions, &rx, key);
        let session = sessions.shown();
        assert_eq!(session.agent.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.agent.emu)
            .backend()
            .assert_buffer_lines(["E[?62;22c   ", "            "]);
    }

    #[test]
    fn every_agent_follows_a_resize_and_settles() {
        let dir = TempDir::new().expect("temp dir");
        // Prints its size once told that it changed.
        let agent = Fake("trap 'stty size; exit' WINCH; echo ready; while :; do sleep 0.05; done");
        let size = Size { rows: 3, cols: 12 };
        let (mut sessions, shown, _tx, rx) = start(&agent, dir.path(), size);
        // The hidden session's events come on a channel of their own.
        let (hidden_tx, hidden_rx) = mpsc::channel::<Msg>();
        let hidden = add(&mut sessions, &agent, dir.path(), size, &hidden_tx);
        // Each trap is set once its agent prints.
        for (key, rx) in [(shown, &rx), (hidden, &hidden_rx)] {
            let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("output before timeout");
            };
            assert_eq!(from, key);
            sessions.on_pty(from, event, Instant::now());
        }
        let size = Size { rows: 4, cols: 14 };
        assert_eq!(sessions.resize(size), None, "no notice");
        run_until_ended(&mut sessions, &hidden_rx, hidden);
        // The shown agent's events are still queued.
        assert!(
            !sessions.settle(Instant::now(), false).shown,
            "only the shown one counts"
        );
        let session = sessions.get_mut(hidden.session).expect("its session");
        assert!(!session.agent.life.alive(), "the hidden one ended");
        run_until_ended(&mut sessions, &rx, shown);
        assert!(
            sessions.settle(Instant::now(), false).shown,
            "the shown one ended"
        );
        for key in [shown, hidden] {
            let session = sessions.get_mut(key.session).expect("its session");
            assert_eq!(session.agent.emu.size(), size);
            screen(&session.agent.emu).backend().assert_buffer_lines([
                "ready         ",
                "4 14          ",
                "              ",
                "              ",
            ]);
        }
    }

    #[test]
    fn the_shown_agent_that_turns_focus_events_on_gets_no_focus_out() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        // Asks for focus events, then prints the three bytes it gets with
        // the escape as `E`.
        let agent = Fake(
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=3 2>/dev/null | tr '\033' E",
        );
        let (mut sessions, key, _tx, rx) = start(&agent, dir.path(), size);
        while !sessions.shown().agent.emu.focus_events() {
            let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("output before timeout");
            };
            sessions.on_pty(from, event, Instant::now());
        }
        // Queued after a focus-out, had there been one.
        sessions.shown().write(b"abc");
        run_until_ended(&mut sessions, &rx, key);
        screen(&sessions.shown().agent.emu)
            .backend()
            .assert_buffer_lines(["abc         ", "            "]);
    }

    #[test]
    fn a_hidden_agent_that_turns_focus_events_on_gets_a_focus_out() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        // The shown session's agent stays quiet until it is hung up.
        let (mut sessions, _, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        // Asks for focus events, then prints the three bytes it gets with
        // the escape as `E`.
        let agent = Fake(
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=3 2>/dev/null | tr '\033' E",
        );
        let key = add(&mut sessions, &agent, dir.path(), size, &tx);
        run_until_ended(&mut sessions, &rx, key);
        let hidden = sessions.get_mut(key.session).expect("its session");
        screen(&hidden.agent.emu)
            .backend()
            .assert_buffer_lines(["E[O         ", "            "]);
    }

    #[test]
    fn a_hidden_agent_gets_the_reply_to_its_query() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        // The shown session's agent stays quiet until it is hung up.
        let (mut sessions, _, tx, rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        // Asks for the primary device attributes, then prints the nine
        // bytes of the reply with the escape as `E`.
        let agent =
            Fake(r"stty raw -echo; printf '\033[c'; dd bs=1 count=9 2>/dev/null | tr '\033' E");
        let key = add(&mut sessions, &agent, dir.path(), size, &tx);
        run_until_ended(&mut sessions, &rx, key);
        let hidden = sessions.get_mut(key.session).expect("its session");
        screen(&hidden.agent.emu)
            .backend()
            .assert_buffer_lines(["E[?62;22c   ", "            "]);
    }

    #[test]
    fn showing_another_session_sends_the_one_hidden_a_focus_out() {
        let (dir, other) = (
            TempDir::new().expect("temp dir"),
            TempDir::new().expect("temp dir"),
        );
        let size = Size { rows: 2, cols: 12 };
        // Asks for focus events, then prints the three bytes it gets with
        // the escape as `E`.
        let agent = Fake(
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=3 2>/dev/null | tr '\033' E",
        );
        let (mut sessions, key, tx, rx) = start(&agent, dir.path(), size);
        while !sessions.shown().agent.emu.focus_events() {
            let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("output before timeout");
            };
            sessions.on_pty(from, event, Instant::now());
        }
        let quiet = add(
            &mut sessions,
            &Fake("exec sleep 30"),
            other.path(),
            size,
            &tx,
        );
        assert_eq!(sessions.find(other.path()), Some(quiet.session));
        assert_eq!(sessions.find(dir.path()), Some(key.session));
        let unknown = sessions.next_id();
        assert!(!sessions.show(unknown, true), "not a session");
        assert_eq!(sessions.position(), Some((1, 2)));
        assert_eq!(sessions.beside(Step::Down), quiet.session);
        assert_eq!(sessions.beside(Step::Up), quiet.session, "wrapping");
        assert!(sessions.show(quiet.session, true));
        assert!(!sessions.show(quiet.session, true), "shown already");
        assert_eq!(sessions.shown().id, quiet.session);
        assert_eq!(sessions.position(), Some((2, 2)));
        run_until_ended(&mut sessions, &rx, key);
        let hidden = sessions.get_mut(key.session).expect("its session");
        screen(&hidden.agent.emu)
            .backend()
            .assert_buffer_lines(["E[O         ", "            "]);
    }

    #[test]
    fn showing_an_agent_that_asked_for_focus_events_sends_it_a_focus_in() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        // The shown session's agent stays quiet until it is hung up.
        let (mut sessions, _, _tx, _rx) = start(&Fake("exec sleep 30"), dir.path(), size);
        assert_eq!(sessions.position(), None, "one session");
        // Asks for focus events, then prints the six bytes it gets with
        // the escape as `E`.
        let agent = Fake(
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=6 2>/dev/null | tr '\033' E",
        );
        // Each on a channel of its own.
        let (focused_tx, focused_rx) = mpsc::channel::<Msg>();
        let (unfocused_tx, unfocused_rx) = mpsc::channel::<Msg>();
        let focused = add(&mut sessions, &agent, dir.path(), size, &focused_tx);
        let unfocused = add(&mut sessions, &agent, dir.path(), size, &unfocused_tx);
        // Each turns focus events on while hidden, and gets a focus-out.
        for (key, rx) in [(focused, &focused_rx), (unfocused, &unfocused_rx)] {
            while !sessions
                .get_mut(key.session)
                .is_some_and(|session| session.agent.emu.focus_events())
            {
                let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                    panic!("output before timeout");
                };
                sessions.on_pty(from, event, Instant::now());
            }
        }
        assert_eq!(sessions.position(), Some((1, 3)));
        assert_eq!(sessions.beside(Step::Up), unfocused.session, "wrapping");
        // Shown while the host has the focus: a focus-in.
        assert!(sessions.show(focused.session, true));
        run_until_ended(&mut sessions, &focused_rx, focused);
        // Shown while it has not: nothing, so what it reads next is typed.
        assert!(sessions.show(unfocused.session, false));
        sessions.shown().write(b"xyz");
        run_until_ended(&mut sessions, &unfocused_rx, unfocused);
        for (key, text) in [(focused, "E[OE[I      "), (unfocused, "E[Oxyz      ")] {
            let session = sessions.get_mut(key.session).expect("its session");
            screen(&session.agent.emu)
                .backend()
                .assert_buffer_lines([text, "            "]);
        }
    }

    #[test]
    fn closing_the_shown_session_sends_a_focus_in_to_the_one_in_its_place() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        // Asks for focus events, then prints the six bytes it gets with
        // the escape as `E`.
        let agent = Fake(
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=6 2>/dev/null | tr '\033' E",
        );
        for (focused, text) in [(true, "E[OE[I      "), (false, "E[Oxyz      ")] {
            // The shown session's agent stays quiet until it is hung up.
            let (mut sessions, _, _tx, _rx) = start(&Fake("exec sleep 30"), dir.path(), size);
            let (tx, rx) = mpsc::channel::<Msg>();
            let key = add(&mut sessions, &agent, dir.path(), size, &tx);
            // It turns focus events on while hidden, and gets a focus-out.
            while !sessions
                .get_mut(key.session)
                .is_some_and(|session| session.agent.emu.focus_events())
            {
                let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                    panic!("output before timeout");
                };
                sessions.on_pty(from, event, Instant::now());
            }
            // Shown in place of the one closed: a focus-in while the host
            // has the focus, else nothing, so what it reads next is typed.
            let closed = sessions.shown().id;
            assert!(sessions.close(closed, focused, Instant::now()));
            assert_eq!(sessions.shown().id, key.session);
            sessions.shown().write(b"xyz");
            run_until_ended(&mut sessions, &rx, key);
            screen(&sessions.shown().agent.emu)
                .backend()
                .assert_buffer_lines([text, "            "]);
        }
    }

    #[test]
    fn a_switch_sends_focus_only_while_the_host_is_focused() {
        const OUT: &[u8] = b"\x1b[O";
        const IN: &[u8] = b"\x1b[I";
        const NONE: &[u8] = b"";
        // Hidden asked, shown asked, host focused: out, in.
        let table = [
            (true, true, true, OUT, IN),
            (true, false, true, OUT, NONE),
            (false, true, true, NONE, IN),
            (false, false, true, NONE, NONE),
            (true, true, false, NONE, NONE),
        ];
        for (hidden, shown, focused, out, into) in table {
            assert_eq!(
                focus_bytes(hidden, shown, focused),
                (out, into),
                "{hidden} {shown} {focused}"
            );
        }
    }

    /// The row of the tab `key` names.
    fn row(key: TabKey) -> RowId {
        RowId {
            session: key.session,
            tab: key.tab,
        }
    }

    #[test]
    fn the_list_cursor_moves_from_its_row_or_from_the_tab_shown() {
        let dir = TempDir::new().expect("temp dir");
        let quiet = "exec sleep 30";
        let agent = Fake(quiet);
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, first, tx, _rx) = start(&agent, dir.path(), size);
        let middle = add(&mut sessions, &agent, dir.path(), size, &tx);
        let last = add(&mut sessions, &agent, dir.path(), size, &tx);
        let unknown = sessions.next_id();
        assert_eq!(sessions.index(last.session), Some(2));
        assert_eq!(sessions.index(unknown), None);
        assert!(sessions.show(middle.session, false));
        // The middle session's terminals are listed under it; its first
        // is on screen.
        let one = open(&mut sessions, quiet, size, &tx);
        let two = open(&mut sessions, quiet, size, &tx);
        assert!(sessions.shown_mut().show_tab(one.tab, false));
        let rows = [first, middle, one, two, last].map(row);
        assert_eq!(sessions.rows(), rows);
        assert_eq!(sessions.shown_row(), row(one));
        let [first, middle, one, two, last] = rows;
        assert_eq!(
            sessions.toward(first, Movement::Line(Step::Down), 1),
            middle
        );
        assert_eq!(sessions.toward(middle, Movement::Line(Step::Down), 1), one);
        assert_eq!(sessions.toward(last, Movement::Line(Step::Up), 1), two);
        assert_eq!(
            sessions.toward(first, Movement::Line(Step::Up), 1),
            first,
            "at the start"
        );
        assert_eq!(
            sessions.toward(last, Movement::Line(Step::Down), 1),
            last,
            "at the end"
        );
        // A row not on the list counts from the tab shown.
        let gone = RowId {
            session: unknown,
            tab: TabId::AGENT,
        };
        assert_eq!(sessions.toward(gone, Movement::Line(Step::Down), 1), two);
        assert_eq!(sessions.toward(gone, Movement::Line(Step::Up), 1), middle);
        // Half or all of the rows shown, stopping at the ends, or to them.
        let (up, down) = (Step::Up, Step::Down);
        assert_eq!(sessions.toward(first, Movement::Half(down), 4), one);
        assert_eq!(sessions.toward(middle, Movement::Page(down), 4), last);
        assert_eq!(sessions.toward(last, Movement::Half(up), 4), one);
        assert_eq!(sessions.toward(two, Movement::Page(up), 4), first);
        assert_eq!(sessions.toward(middle, Movement::End, 4), last);
        assert_eq!(sessions.toward(two, Movement::Top, 4), first);
        assert_eq!(sessions.toward(gone, Movement::End, 4), last);
        // Showing a tab of a session not shown makes it the one that
        // session shows, for when it is.
        assert!(
            !sessions.show_tab(last.session, last.tab, true),
            "shown already"
        );
        assert!(sessions.show(first.session, false));
        assert!(sessions.show_tab(middle.session, two.tab, true));
        assert_eq!(sessions.shown_row(), first, "still on screen");
        assert!(sessions.show(middle.session, false));
        assert_eq!(sessions.shown_row(), two);
        assert!(!sessions.show_tab(unknown, TabId::AGENT, true));
    }

    #[test]
    fn the_list_cursor_leaves_a_terminal_that_closes_for_the_tab_shown() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let agent = Fake("exec sleep 30");
        let (mut sessions, first, tx, rx) = start(&agent, dir.path(), size);
        let terminal = open(&mut sessions, "read line", size, &tx);
        let other = add(&mut sessions, &agent, dir.path(), size, &tx);
        assert!(sessions.show(other.session, false));
        assert_eq!(sessions.cursor_row(None), row(other), "where it starts");
        let cursor = Some(row(terminal));
        assert_eq!(sessions.cursor_row(cursor), row(terminal));
        // The terminal's shell exits while its session is hidden.
        let session = sessions.get(first.session).expect("open");
        session.terminals[0].write(b"\r");
        run_until(&mut sessions, &rx, |sessions| {
            sessions
                .get(first.session)
                .is_some_and(|session| session.terminals.is_empty())
        });
        assert_eq!(sessions.cursor_row(cursor), row(other));
    }

    #[test]
    fn a_tab_of_a_session_not_shown_gets_no_focus_event() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 12 };
        let quiet = "exec sleep 30";
        let agent = Fake(quiet);
        let (mut sessions, first, tx, rx) = start(&agent, dir.path(), size);
        // Asks for focus events, then prints the six bytes it gets with
        // the escape as `E`.
        let script =
            r"stty raw -echo; printf '\033[?1004h'; dd bs=1 count=6 2>/dev/null | tr '\033' E";
        let asks = open(&mut sessions, script, size, &tx);
        run_until(&mut sessions, &rx, |sessions| {
            sessions.shown().tab().expect("a child").emu.focus_events()
        });
        let beside = open(&mut sessions, quiet, size, &tx);
        let other = add(&mut sessions, &agent, dir.path(), size, &tx);
        assert!(sessions.show(other.session, false));
        // Its session hidden, the tabs it shows change with the host
        // focused, and it gets nothing.
        assert!(sessions.show_tab(first.session, asks.tab, true));
        assert!(sessions.show_tab(first.session, beside.tab, true));
        assert!(sessions.close_tab(first.session, beside.tab, true, Instant::now()));
        let session = sessions.get(first.session).expect("open");
        assert_eq!(session.shown_tab(), asks.tab, "the one before it");
        // Shown with its session, it gets a focus-in, then what is typed.
        assert!(sessions.show(first.session, true));
        sessions.shown().write(b"end");
        run_until(&mut sessions, &rx, |sessions| {
            let terminal = screen(&sessions.shown().tab().expect("a child").emu);
            let cells = &terminal.backend().buffer().content()[..6];
            cells.iter().map(|cell| cell.symbol()).collect::<String>() == "E[Iend"
        });
    }

    #[test]
    fn an_agent_is_told_when_it_starts_asking_unseen() {
        let (working, waiting, asking) = (
            Some(State::Working),
            Some(State::Waiting),
            Some(State::Attention),
        );
        // Previous, next, shown: told.
        let table = [
            (working, asking, false, true),
            (waiting, asking, false, true),
            (None, asking, false, true),
            // Not again while it keeps asking.
            (asking, asking, false, false),
            // Not for the agent on screen.
            (working, asking, true, false),
            (None, asking, true, false),
            // Nor for anything but asking.
            (asking, working, false, false),
            (working, waiting, false, false),
            (asking, None, false, false),
        ];
        for (previous, next, shown, told) in table {
            assert_eq!(
                should_tell(previous, next, shown),
                told,
                "{previous:?} {next:?} {shown}"
            );
        }
    }

    /// An agent that stays quiet, whose telemetry Claude's adapter reads.
    struct Quiet;

    impl agent::Agent for Quiet {
        fn name(&self) -> &str {
            "quiet"
        }

        fn spec(&self, launch: &agent::Launch<'_>, size: Size, _: Option<&Instance>) -> pty::Spec {
            Fake("exec sleep 30").spec(launch, size, None)
        }

        fn status(&self, status: &[u8], events: &[u8]) -> agent::Status {
            crate::agent::claude::Claude.status(status, events)
        }
    }

    /// Sessions of [`Quiet`] in `api` and `web` under `base`, made first,
    /// with their directories in `instances`, on ptys of `size`; `api`,
    /// with the directory `0`, is shown. Returns them, and both ends of
    /// the channel their events reach.
    fn quiet_sessions(
        instances: &Root,
        base: &Path,
        size: Size,
    ) -> (Sessions, mpsc::Sender<Msg>, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel::<Msg>();
        let starter = Starter {
            agent: &Quiet,
            instances: Ok(instances),
            tx: &tx,
        };
        let mut sessions = Sessions::new();
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
            sessions.push(session);
        }
        (sessions, tx, rx)
    }

    /// Writes `events`, one per hook event in order, as the events of the
    /// session whose directory in `instances` is `n`.
    fn write_events(instances: &Root, n: &str, events: &[&str]) {
        let lines: String = events
            .iter()
            .map(|event| format!("{{\"hook\":{event},\"received\":1}}\n"))
            .collect();
        let file = instances.dir().join(n).join(crate::telemetry::EVENTS_FILE);
        fs::write(file, lines).expect("write");
    }

    /// Applies the events of `rx` as the main loop does until the agent
    /// of session `id` ended.
    fn run_until_agent_ended(sessions: &mut Sessions, rx: &mpsc::Receiver<Msg>, id: SessionId) {
        run_until(sessions, rx, |sessions| {
            sessions
                .get(id)
                .is_none_or(|session| !session.agent.life.alive())
        });
    }

    #[test]
    fn a_read_names_the_hidden_sessions_that_start_asking() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let instances = Root::create_in(&base, "/bin/sh".into()).expect("creates");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, tx, rx) = quiet_sessions(&instances, &base, size);
        let write = |n: &str, events: &[&str]| write_events(&instances, n, events);
        let prompt = r#"{"hook_event_name":"UserPromptSubmit"}"#;
        let request = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#;
        let notice =
            r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#;
        let ran = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#;
        let read = |sessions: &mut Sessions| sessions.read_telemetry(&Quiet, Instant::now());
        // Nothing written yet: a first read, which tells nothing.
        let first = read(&mut sessions);
        assert!(first.asking.is_empty());
        // The hidden one starts asking: named once.
        write("1", &[prompt, request]);
        let told = read(&mut sessions);
        assert_eq!(told.asking, ["web"]);
        assert!(!told.shown, "the shown one did not change");
        // Claude's notice after the request keeps it asking: not again.
        write("1", &[prompt, request, notice]);
        assert!(read(&mut sessions).asking.is_empty());
        // The one on screen asking is drawn, not told.
        write("0", &[prompt, request]);
        let shown = read(&mut sessions);
        assert!(shown.shown && shown.asking.is_empty());
        // Answered, then asking again: told again.
        write("1", &[prompt, request, notice, ran]);
        assert!(read(&mut sessions).asking.is_empty());
        write("1", &[prompt, request, notice, ran, request]);
        assert_eq!(read(&mut sessions).asking, ["web"]);
        // Shown now, it is not told; the other one, hidden, is.
        assert!(sessions.show(sessions.find(&base.join("web")).expect("open"), false));
        write("1", &[prompt]);
        write("0", &[prompt, request, ran]);
        assert!(read(&mut sessions).asking.is_empty());
        write("1", &[prompt, request]);
        write("0", &[prompt, request, ran, request]);
        assert_eq!(read(&mut sessions).asking, ["api"]);
        // Behind a terminal of the session shown, its agent is off screen:
        // told.
        open(&mut sessions, "exec sleep 30", size, &tx);
        write("1", &[prompt, request, ran]);
        assert!(read(&mut sessions).asking.is_empty());
        write("1", &[prompt, request, ran, request]);
        let behind = read(&mut sessions);
        assert_eq!(behind.asking, ["web"]);
        assert!(behind.shown, "its card changed");
        // An agent that ended asks nothing, whatever its events say last.
        write("0", &[prompt, request, ran, request, ran]);
        assert!(read(&mut sessions).asking.is_empty());
        let api = sessions.find(&base.join("api")).expect("open");
        let session = sessions.get(api).expect("open");
        session.agent.pty.kill_now().expect("kill");
        run_until_agent_ended(&mut sessions, &rx, api);
        write("0", &[prompt, request, ran, request, ran, request]);
        assert!(read(&mut sessions).asking.is_empty());
    }

    #[test]
    fn an_agent_turns_to_asking_or_waiting_once_per_turn() {
        let (working, waiting, asking) = (
            Some(State::Working),
            Some(State::Waiting),
            Some(State::Attention),
        );
        // Previous, next: what it turned to.
        let table = [
            (working, asking, asking),
            (waiting, asking, asking),
            (None, asking, asking),
            (working, waiting, waiting),
            // Not again while the state holds.
            (asking, asking, None),
            (waiting, waiting, None),
            (working, working, None),
            // Nor for work, a wait that follows no work, or an end.
            (waiting, working, None),
            (asking, working, None),
            (asking, waiting, None),
            (None, waiting, None),
            (working, None, None),
            (asking, None, None),
        ];
        for (previous, next, state) in table {
            assert_eq!(turned(previous, next), state, "{previous:?} {next:?}");
        }
    }

    #[test]
    fn a_read_names_every_agent_that_starts_asking_or_finishes_its_turn() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let instances = Root::create_in(&base, "/bin/sh".into()).expect("creates");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, _tx, rx) = quiet_sessions(&instances, &base, size);
        let write = |n: &str, events: &[&str]| write_events(&instances, n, events);
        let prompt = r#"{"hook_event_name":"UserPromptSubmit"}"#;
        let request = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#;
        let notice =
            r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#;
        let ran = r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#;
        let stop = r#"{"hook_event_name":"Stop"}"#;
        let read = |sessions: &mut Sessions| sessions.read_telemetry(&Quiet, Instant::now());
        let turn = |project: &str, state| (project.to_owned(), state);
        assert!(read(&mut sessions).turns.is_empty(), "a first read");
        // The one shown starts asking: named, though not told off screen.
        write("0", &[prompt, request]);
        let asks = read(&mut sessions);
        assert_eq!(asks.turns, [turn("api", State::Attention)]);
        assert!(asks.asking.is_empty());
        // Not again while it keeps asking.
        write("0", &[prompt, request, notice]);
        assert!(read(&mut sessions).turns.is_empty());
        // Answered, it works, then finishes its turn: named waiting, once.
        write("0", &[prompt, request, notice, ran]);
        assert!(read(&mut sessions).turns.is_empty());
        write("0", &[prompt, request, notice, ran, stop]);
        assert_eq!(read(&mut sessions).turns, [turn("api", State::Waiting)]);
        write("0", &[prompt, request, notice, ran, stop, stop]);
        assert!(read(&mut sessions).turns.is_empty());
        // Both at once, in the order of the list; the hidden one asking
        // is told too.
        write("0", &[prompt]);
        write("1", &[prompt]);
        assert!(read(&mut sessions).turns.is_empty());
        write("0", &[prompt, stop]);
        write("1", &[prompt, request]);
        let both = read(&mut sessions);
        assert_eq!(
            both.turns,
            [turn("api", State::Waiting), turn("web", State::Attention)]
        );
        assert_eq!(both.asking, ["web"]);
        // An agent that ended turns to nothing, whatever its events say.
        write("1", &[prompt, request, ran]);
        assert!(read(&mut sessions).turns.is_empty());
        let web = sessions.find(&base.join("web")).expect("open");
        let session = sessions.get(web).expect("open");
        session.agent.pty.kill_now().expect("kill");
        run_until_agent_ended(&mut sessions, &rx, web);
        write("1", &[prompt, request, ran, stop]);
        assert!(read(&mut sessions).turns.is_empty());
        write("1", &[prompt, request, ran, stop, request]);
        assert!(read(&mut sessions).turns.is_empty());
    }

    #[test]
    fn closing_shows_the_next_session_else_the_one_before() {
        // Shown, closed, length before: shown after.
        let table = [
            (0, 0, 3, 0),
            (1, 1, 3, 1),
            (2, 2, 3, 1),
            (1, 1, 2, 0),
            (0, 0, 2, 0),
            // Another one leaves: the same session stays shown.
            (2, 0, 3, 1),
            (0, 2, 3, 0),
            (1, 2, 3, 1),
        ];
        for (shown, closed, len, after) in table {
            assert_eq!(
                next_shown(shown, closed, len),
                after,
                "{shown} {closed} {len}"
            );
        }
    }

    #[test]
    fn closing_a_session_hangs_it_up_while_the_others_run() {
        let temporary = TempDir::new().expect("temp dir");
        let instances = Root::create_in(temporary.path(), "/bin/sh".into()).expect("creates");
        let (tx, rx) = mpsc::channel::<Msg>();
        // A ignores the hangup; B and C print until they are hung up.
        let stubborn = Fake("trap '' HUP; printf ready; while :; do sleep 0.05; done");
        let chatty = Fake("while :; do echo b; sleep 0.05; done");
        let size = Size { rows: 2, cols: 10 };
        let mut sessions = Sessions::new();
        for agent in [&stubborn, &chatty, &chatty] {
            let starter = Starter {
                agent,
                instances: Ok(&instances),
                tx: &tx,
            };
            let place = Place::new(temporary.path().to_path_buf(), None, None);
            let (session, _) = {
                let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
                starter
                    .start(sessions.next_id(), place, Vec::new(), false, size)
                    .expect("starts")
            };
            sessions.push(session);
        }
        let ids: Vec<SessionId> = sessions.iter().map(|session| session.id).collect();
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        let exited = sessions.get(a).expect("A").agent.pty.exited_flag();
        let dir = instances.dir().join("0");
        // A's trap is set once it prints.
        loop {
            let Ok(Msg::Pty(key, event)) = rx.recv_timeout(WAIT) else {
                panic!("output before timeout");
            };
            let from_a = key.session == a && matches!(event, pty::Event::Output(_));
            sessions.on_pty(key, event, Instant::now());
            if from_a {
                break;
            }
        }
        // A shows a repository no other session uses.
        let repo = Repos::new()
            .get_or_spawn(temporary.path(), &tx)
            .expect("spawn");
        assert_eq!(sessions.shown().id, a);
        sessions.shown_mut().repo = Some(repo);
        let started = Instant::now();
        assert!(sessions.close(a, true, started));
        assert_eq!(sessions.shown().id, b, "the next one is shown");
        assert_eq!(sessions.index(a), None, "off the list");
        assert!(sessions.get_closing(a).is_some(), "being closed");
        assert!(sessions.uses(repo), "until it goes");
        // A list cursor left on it counts from the session shown.
        let [a_row, b_row, c_row] = [a, b, c].map(|id| row(TabKey::agent(id)));
        assert_eq!(sessions.toward(a_row, Movement::Line(Step::Down), 1), c_row);
        assert_eq!(sessions.toward(a_row, Movement::Line(Step::Up), 1), b_row);
        // B's output reaches its screen while A has its grace.
        let mut b_during_grace = 0;
        while sessions.get_closing(a).is_some() {
            if let Ok(Msg::Pty(key, event)) = rx.recv_timeout(Duration::from_millis(20)) {
                if key.session == b
                    && matches!(event, pty::Event::Output(_))
                    && started.elapsed() < GRACE
                {
                    b_during_grace += 1;
                }
                sessions.on_pty(key, event, Instant::now());
            }
            sessions.move_closing(Instant::now());
            assert!(started.elapsed() < GRACE + 3 * KILL_GRACE, "done in time");
        }
        let took = started.elapsed();
        assert!(b_during_grace > 0, "B ran during A's grace");
        let shown = screen(&sessions.shown().agent.emu);
        assert!(
            shown
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol() == "b"),
            "B's screen has its output"
        );
        assert!(took >= GRACE, "hung up first: {took:?}");
        assert!(exited.load(Ordering::SeqCst), "A was killed");
        assert!(!is_watched(&exited), "its entry with the panic hook went");
        assert!(!dir.exists(), "its directory went");
        assert!(instances.dir().join("1").exists(), "B's stays");
        assert!(!sessions.uses(repo), "no session uses the repository");
        // A late event of A reaches nothing, not even the session shown.
        let late = TabKey::agent(a);
        sessions.on_pty(late, pty::Event::Output(b"late".to_vec()), Instant::now());
        sessions.on_pty(late, pty::Event::Closed, Instant::now());
        assert!(sessions.get(a).is_none() && sessions.get_closing(a).is_none());
        let shown: String = screen(&sessions.shown().agent.emu)
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!shown.contains("late"), "{shown:?}");
        assert!(!sessions.shown().agent.life.closed, "B's output goes on");
        // Closing the last row shows the one before it; the last session
        // is not closed, the app quits instead.
        assert!(sessions.show(c, false));
        assert!(sessions.close(c, false, Instant::now()));
        assert_eq!(sessions.shown().id, b, "the one before");
        assert!(!sessions.close(b, false, Instant::now()), "the last one");
        assert_eq!(sessions.shown().id, b);
        // Quitting hangs up both the one left and the one being closed.
        sessions.quit();
        sessions.close_all(&rx);
        assert!(sessions.closing.iter().all(Closed::done));
    }

    #[test]
    fn the_next_and_previous_sessions_wrap_around() {
        assert_eq!(wrapped(0, 3, Step::Down), 1);
        assert_eq!(wrapped(2, 3, Step::Down), 0);
        assert_eq!(wrapped(0, 3, Step::Up), 2);
        assert_eq!(wrapped(2, 3, Step::Up), 1);
        assert_eq!(wrapped(0, 1, Step::Down), 0, "one session");
        assert_eq!(wrapped(0, 1, Step::Up), 0);
        assert_eq!(wrapped(0, 0, Step::Up), 0, "none");
    }

    #[test]
    fn each_session_started_has_its_own_directory_and_quitting_ends_them_all() {
        let temporary = TempDir::new().expect("temp dir");
        let instances = Root::create_in(temporary.path(), "/bin/sh".into()).expect("creates");
        let (tx, rx) = mpsc::channel::<Msg>();
        let agent = Fake("printf ready; exec sleep 60");
        let starter = Starter {
            agent: &agent,
            instances: Ok(&instances),
            tx: &tx,
        };
        let size = Size { rows: 2, cols: 10 };
        let mut sessions = Sessions::new();
        let mut exited = Vec::new();
        for n in ["0", "1"] {
            let id = sessions.next_id();
            let place = Place::new(temporary.path().to_path_buf(), None, None);
            let (session, notice) = {
                let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
                starter
                    .start(id, place, Vec::new(), false, size)
                    .expect("starts")
            };
            assert_eq!(notice, None);
            let dir = session.instance.as_ref().map(Instance::dir);
            assert_eq!(dir, Some(instances.dir().join(n).as_path()));
            let flag = session.agent.pty.exited_flag();
            assert!(is_watched(&flag), "the panic hook knows its agent");
            exited.push(flag);
            sessions.push(session);
        }
        sessions.quit();
        sessions.close_all(&rx);
        assert!(
            exited.iter().all(|flag| flag.load(Ordering::SeqCst)),
            "both reaped"
        );
        drop(sessions);
        assert!(exited.iter().all(|flag| !is_watched(flag)));
        for n in ["0", "1"] {
            assert!(!instances.dir().join(n).exists(), "gone with its session");
        }
        let top = instances.dir().to_path_buf();
        drop(instances);
        assert!(!top.exists(), "the process's directory went");
    }

    #[test]
    fn a_focus_out_is_due_once_when_focus_events_turn_on_unfocused() {
        // Before, after, focused, due.
        let table = [
            (false, true, false, true),
            (false, true, true, false),
            (true, true, false, false),
            (true, false, false, false),
            (false, false, false, false),
        ];
        for (before, after, focused, due) in table {
            assert_eq!(
                focus_out_due(before, after, focused),
                due,
                "{before} {after} {focused}"
            );
        }
    }

    #[test]
    fn quitting_kills_an_agent_that_ignores_the_hangup() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("trap '' HUP; printf ready; exec sleep 60");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, _tx, rx) = start(&agent, dir.path(), size);
        // The trap is set once the agent prints.
        let Ok(Msg::Pty(from, pty::Event::Output(_))) = rx.recv_timeout(WAIT) else {
            panic!("output before timeout");
        };
        assert_eq!(from, key);
        let exited = sessions.shown().agent.pty.exited_flag();
        let started = Instant::now();
        sessions.quit();
        assert!(sessions.close_all(&rx).is_none(), "the agent did not exit");
        let took = started.elapsed();
        assert!(took >= GRACE, "hung up first: {took:?}");
        // Killed at `GRACE`, not left to the group kill after it.
        assert!(took < GRACE + KILL_GRACE, "killed: {took:?}");
        assert!(exited.load(Ordering::SeqCst), "reaped");
        assert!(sessions.closing[0].agent.life.closed, "its output ended");
        assert!(is_watched(&exited), "known to the panic hook until dropped");
        drop(sessions);
        assert!(!is_watched(&exited));
    }

    #[test]
    fn quitting_hangs_up_an_agent_that_exits_on_it() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("printf ready; exec sleep 60");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, _tx, rx) = start(&agent, dir.path(), size);
        let Ok(Msg::Pty(from, pty::Event::Output(_))) = rx.recv_timeout(WAIT) else {
            panic!("output before timeout");
        };
        assert_eq!(from, key);
        let exited = sessions.shown().agent.pty.exited_flag();
        let started = Instant::now();
        sessions.quit();
        let status = sessions.close_all(&rx);
        let took = started.elapsed();
        assert!(status.is_none(), "the hangup's exit is not the agent's own");
        // No kill, and no wait for an exit that came before the quit.
        assert!(took < KILL_GRACE, "hung up: {took:?}");
        assert!(exited.load(Ordering::SeqCst), "reaped");
        assert!(sessions.closing[0].agent.life.closed, "its output ended");
    }

    #[test]
    fn quitting_reports_an_exit_still_on_its_way() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("exit 3");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, _key, _tx, rx) = start(&agent, dir.path(), size);
        // Exited, its events not read yet: as when the quit comes in the
        // same pass.
        let deadline = Instant::now() + WAIT;
        while !sessions.shown().agent.pty.has_exited() {
            assert!(Instant::now() < deadline, "the agent exits");
            thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        sessions.quit();
        let status = sessions.close_all(&rx);
        assert_eq!(status.map(|status| status.exit_code()), Some(3));
        assert!(started.elapsed() < GRACE, "no hangup to wait for");
    }

    /// An agent that leaves a child that ignores the hangup in its process
    /// group, and exits.
    const LEAVES_A_CHILD: Fake = Fake("trap '' HUP; sleep 60 & exit 0");

    /// Runs [`LEAVES_A_CHILD`] in `dir` until it ended, as the first
    /// session. Returns what [`start`] does and the agent's process group,
    /// where the child still runs.
    fn left_a_child(
        dir: &Path,
    ) -> (
        Sessions,
        TabKey,
        mpsc::Sender<Msg>,
        mpsc::Receiver<Msg>,
        u32,
    ) {
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, tx, rx) = start(&LEAVES_A_CHILD, dir, size);
        let group = sessions.shown().agent.pty.pid().expect("a pid");
        run_until_ended(&mut sessions, &rx, key);
        assert!(pty::group_alive(group), "the child outlived the agent");
        (sessions, key, tx, rx, group)
    }

    #[test]
    fn closing_a_session_kills_what_its_agent_left_in_its_group() {
        let dir = TempDir::new().expect("temp dir");
        let (mut sessions, key, _tx, _rx, group) = left_a_child(dir.path());
        // Another session, so this one can close; its agent stays quiet
        // until it is hung up.
        let (quiet_tx, _quiet_rx) = mpsc::channel::<Msg>();
        let quiet = Fake("exec sleep 30");
        add(
            &mut sessions,
            &quiet,
            dir.path(),
            Size { rows: 2, cols: 10 },
            &quiet_tx,
        );
        assert!(sessions.close(key.session, false, Instant::now()));
        sessions.move_closing(Instant::now());
        assert!(sessions.get_closing(key.session).is_none(), "done at once");
        assert!(pty::group_ends_within(group, WAIT), "the child went");
    }

    #[test]
    fn quitting_kills_what_an_agent_left_in_its_group() {
        let dir = TempDir::new().expect("temp dir");
        let (mut sessions, _key, _tx, rx, group) = left_a_child(dir.path());
        let started = Instant::now();
        sessions.quit();
        let status = sessions.close_all(&rx);
        assert_eq!(status.map(|status| status.exit_code()), Some(0));
        assert!(started.elapsed() < KILL_GRACE, "nothing to wait for");
        assert!(pty::group_ends_within(group, WAIT), "the child went");
    }

    #[test]
    fn a_relaunch_kills_what_the_last_launch_left_in_its_group() {
        let dir = TempDir::new().expect("temp dir");
        let (mut sessions, _key, tx, _rx, group) = left_a_child(dir.path());
        let notice = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            let next = Fake("exec sleep 30");
            sessions
                .shown_mut()
                .relaunch(&next, Size { rows: 2, cols: 10 }, &tx)
        };
        assert_eq!(notice, None);
        assert!(pty::group_ends_within(group, WAIT), "the child went");
    }

    #[test]
    fn a_relaunch_while_the_agent_runs_does_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let size = Size { rows: 2, cols: 10 };
        let agent = Fake("exec sleep 30");
        let (mut sessions, key, tx, _rx) = start(&agent, dir.path(), size);
        let group = sessions.shown().agent.pty.pid().expect("a pid");
        let notice = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            sessions.shown_mut().relaunch(&agent, size, &tx)
        };
        assert_eq!(notice, None);
        let life = &sessions.shown().agent.life;
        assert!(life.current(key.generation), "no launch");
        assert!(pty::group_alive(group), "the agent runs");
    }

    #[test]
    fn discarding_a_session_kills_what_its_agent_left_in_its_group() {
        let dir = TempDir::new().expect("temp dir");
        let (mut sessions, _key, _tx, _rx, group) = left_a_child(dir.path());
        sessions.list.pop().expect("a session").discard();
        assert!(pty::group_ends_within(group, WAIT), "the child went");
    }

    #[test]
    fn closing_moves_on_by_the_agent_and_its_deadlines() {
        use Closing::{Done, GroupKilled, HungUp, Killed, Start};
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        // When the waits after the hangup, the kill and the group kill end.
        let t1 = t0 + GRACE;
        let t2 = t1 + KILL_GRACE;
        let t3 = t2 + KILL_GRACE;
        // From, exited, closed, at, to.
        let table = [
            (Start, false, false, t0, HungUp(t1)),
            // Exits within GRACE: the group is killed, for what the agent
            // left in it, even with the output closed.
            (HungUp(t1), false, false, t1 - ms, HungUp(t1)),
            (
                HungUp(t1),
                true,
                true,
                t0 + ms,
                GroupKilled(t0 + ms + KILL_GRACE),
            ),
            // Ignores the hangup: killed at GRACE.
            (HungUp(t1), false, false, t1, Killed(t2)),
            // Exits right at GRACE: the group kill, not the agent's.
            (HungUp(t1), true, false, t1, GroupKilled(t2)),
            (HungUp(t1), true, true, t1, GroupKilled(t2)),
            (Killed(t2), false, false, t2 - ms, Killed(t2)),
            (Killed(t2), true, true, t1 + ms, GroupKilled(t2 + ms)),
            // Exited with the output still open: group kill.
            (HungUp(t1), true, false, t0, GroupKilled(t0 + KILL_GRACE)),
            (Killed(t2), true, false, t1, GroupKilled(t1 + KILL_GRACE)),
            (GroupKilled(t3), true, false, t3 - ms, GroupKilled(t3)),
            // The output ended: done at once.
            (GroupKilled(t3), true, true, t2 + ms, Done),
            // Not reaped by the kill's deadline: group kill, unless the
            // output ended, as the kill reached the whole group.
            (Killed(t2), false, false, t2, GroupKilled(t3)),
            (Killed(t2), false, true, t2, Done),
            // Last deadline passed: done anyway.
            (GroupKilled(t3), false, false, t3, Done),
            // Exited before the close, the output closed or not: group
            // kill, then done once the output ended.
            (Start, true, true, t0, GroupKilled(t0 + KILL_GRACE)),
            (Start, true, false, t0, GroupKilled(t0 + KILL_GRACE)),
            (GroupKilled(t0 + KILL_GRACE), true, true, t0, Done),
            (Done, false, false, t0, Done),
        ];
        for (row, (from, exited, closed, at, to)) in table.into_iter().enumerate() {
            assert_eq!(from.step(exited, closed, at), to, "row {row}");
        }
        assert_eq!(GroupKilled(t3).deadline(), Some(t3));
        assert_eq!(Start.deadline(), None);
        assert_eq!(Done.deadline(), None);
    }

    #[test]
    fn session_ids_are_never_reused() {
        let mut sessions = Sessions::new();
        assert_eq!(
            [sessions.next_id(), sessions.next_id(), sessions.next_id()],
            [SessionId(0), SessionId(1), SessionId(2)]
        );
        assert!(
            sessions.get_mut(SessionId(1)).is_none(),
            "an id is not a session"
        );
    }

    #[test]
    fn a_session_gets_the_directory_of_its_id_or_says_why_not() {
        let temporary = TempDir::new().expect("temp dir");
        let instances = Root::create_in(temporary.path(), "/bin/termis".into()).expect("creates");
        let place = Place::new(temporary.path().to_path_buf(), None, None);
        let launch = place.launch(&[]);
        let agent = Fake("true");
        let (instance, notice) = instance_for(Ok(&instances), SessionId(4), &agent, &launch);
        assert_eq!(notice, None);
        let dir = instance.as_ref().map(Instance::dir);
        assert_eq!(dir, Some(instances.dir().join("4").as_path()));
        // Without the process's directory: none, and its reason.
        let error = anyhow::anyhow!("no room");
        let (none, notice) = instance_for(Err(&error), SessionId(5), &agent, &launch);
        assert!(none.is_none());
        assert_eq!(notice.as_deref(), Some("no room"));
        // When the session's own cannot be made: none, and why.
        fs::remove_dir_all(instances.dir()).expect("rm");
        let (none, notice) = instance_for(Ok(&instances), SessionId(6), &agent, &launch);
        assert!(none.is_none());
        let notice = notice.expect("a reason");
        assert!(notice.starts_with("create "), "{notice}");
    }

    #[test]
    fn a_place_inside_a_repository_is_the_repository() {
        let home = TempDir::new().expect("temp dir");
        let root = home.path().join("termis");
        let cwd = root.join("src");
        fs::create_dir_all(&cwd).expect("mkdir");
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"termis\"\n").expect("write");
        let place = Place::new(cwd.clone(), Some(root.clone()), Some(home.path()));
        assert_eq!(place.cwd, cwd, "the agent runs where asked");
        assert_eq!(place.repo.as_deref(), Some(root.as_path()));
        assert_eq!(place.below_root(), Path::new("src"));
        let at_root = Place::new(root.clone(), Some(root.clone()), None);
        assert_eq!(at_root.below_root(), Path::new(""));
        assert_eq!(place.root, root);
        assert_eq!(place.project, "termis");
        assert_eq!(place.path, "~/termis");
        assert_eq!(
            place.stack.as_ref().map(|stack| stack.language),
            Some("Rust")
        );
        let args = vec!["-c".to_owned()];
        let launch = place.launch(&args);
        assert_eq!(launch.cwd, cwd);
        assert_eq!(launch.root, root);
        assert_eq!(launch.args, args);
        assert!(!launch.new_conversation, "a relaunch continues");
    }

    #[test]
    fn a_place_outside_a_repository_is_its_directory() {
        let dir = TempDir::new().expect("temp dir");
        let cwd = dir.path().join("notes");
        fs::create_dir(&cwd).expect("mkdir");
        let place = Place::new(cwd.clone(), None, None);
        assert!(place.repo.is_none());
        assert_eq!(place.below_root(), Path::new(""));
        assert_eq!(place.root, cwd);
        assert_eq!(place.project, "notes");
        assert_eq!(place.path, cwd.to_string_lossy(), "no home to shorten");
        assert_eq!(place.stack, None);
        assert_eq!(place.launch(&[]).root, cwd);
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
        let next = life.next_generation();
        life.relaunch(next);
        life.apply(0, &pty::Event::Closed, now);
        life.apply(0, &pty::Event::Exited(ExitStatus::with_exit_code(0)), now);
        assert!(!life.closed && life.exited.is_none() && life.alive());
        assert!(!life.current(0) && life.current(1));
        life.apply(1, &pty::Event::Closed, now);
        assert!(life.closed);
        // A launch that could not start keeps its number.
        let failed = life.next_generation();
        let next = life.next_generation();
        life.relaunch(next);
        assert_eq!((failed, next), (2, 3));
        life.apply(failed, &pty::Event::Closed, now);
        assert!(!life.closed && life.current(3));
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
        let next = life.next_generation();
        life.relaunch(next);
        assert!(life.alive() && life.status().is_none());
    }
}
