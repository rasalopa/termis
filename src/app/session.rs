//! Agent sessions: the agent on its pty, launched in a place with the
//! user's arguments, and what the main loop keeps of it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::host::{Host, OnPanic};
use super::repo::RepoId;
use super::{Msg, SCROLLBACK};
use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::instance::{Instance, Root, Watch};
use crate::projects::{self, Stack};
use crate::pty::{self, ExitStatus};
use crate::strings;
use crate::ui;
use crate::ui::changes::Step;

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

/// Who sent a pty event: the session and the generation of its launch,
/// which a relaunch moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AgentKey {
    pub(super) session: SessionId,
    pub(super) generation: u32,
}

impl AgentKey {
    /// The key of session `session`'s first launch.
    pub(super) fn first(session: SessionId) -> Self {
        Self {
            session,
            generation: Lifecycle::default().generation,
        }
    }
}

/// An agent session: the agent's pty with the emulator it draws on, the
/// lifecycle of its exit and relaunch, and its instance directory with
/// the watch on its telemetry.
pub(super) struct Session {
    /// What the agent's events are tagged with.
    pub(super) id: SessionId,
    /// Where the agent runs.
    pub(super) place: Place,
    /// The repository the changes pane shows with the session; none
    /// outside one.
    pub(super) repo: Option<RepoId>,
    /// The user's extra arguments for the agent.
    args: Vec<String>,
    /// Whether the agent's current launch exited and its output ended.
    pub(super) life: Lifecycle,
    /// The agent's current launch. Declared before `instance`: the agent
    /// is hung up before its directory goes.
    pty: pty::Session,
    /// The current launch's entry with the panic hook. Declared after
    /// `pty`: the hook knows the agent for as long as its pty lives.
    on_panic: OnPanic,
    /// The agent's screen and the lines that scrolled off it.
    pub(super) emu: Emulator,
    /// Where the agent's shims write its telemetry; without one the agent
    /// runs without telemetry.
    instance: Option<Instance>,
    /// Notices when the telemetry files change.
    watch: Watch,
    /// What the agent's telemetry said last.
    pub(super) status: agent::Status,
    /// The session's time in the agent's box.
    pub(super) clock: SessionClock,
}

impl Session {
    /// Session `id` in `place`, its changes pane on `repo`: the agent, run
    /// with `args` on the pty and panic hook entry that [`spawn`] returned,
    /// with `instance` for its telemetry, draws on `emu`.
    pub(super) fn new(
        id: SessionId,
        place: Place,
        repo: Option<RepoId>,
        args: Vec<String>,
        instance: Option<Instance>,
        (pty, on_panic): (pty::Session, OnPanic),
        emu: Emulator,
    ) -> Self {
        Self {
            id,
            place,
            repo,
            args,
            life: Lifecycle::default(),
            pty,
            on_panic,
            emu,
            instance,
            watch: Watch::default(),
            status: agent::Status::default(),
            clock: SessionClock::default(),
        }
    }

    /// How the agent is launched.
    fn launch(&self) -> agent::Launch<'_> {
        self.place.launch(&self.args)
    }

    /// A key for the agent's next launch, whether it starts or not: no
    /// launch tagged its events with it before.
    fn next_key(&mut self) -> AgentKey {
        AgentKey {
            session: self.id,
            generation: self.life.next_generation(),
        }
    }

    /// Applies `event` from the agent's launch `generation`, at `now`. The
    /// current launch's output goes to the emulator, and the replies it
    /// asks for, when there are some, to the agent, with a focus-out when
    /// it turned focus events on while not `focused`. The lifecycle notes
    /// the rest and drops what an older launch sent.
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

    /// Moves the hangup of the agent on from `closing` at `now`, sending
    /// its process group the signal of the stage it reaches.
    fn step_closing(&self, closing: Closing, now: Instant) -> Closing {
        let next = closing.step(self.pty.has_exited(), self.life.closed, now);
        if next != closing {
            let _ = match next {
                Closing::HungUp(_) => self.pty.hangup(),
                Closing::Killed(_) => self.pty.kill_now(),
                Closing::GroupKilled(_) => self.pty.kill_group_now(),
                Closing::Start | Closing::Done => Ok(()),
            };
        }
        next
    }

    /// Whether the agent exited before the user quit and its exit has not
    /// arrived yet.
    fn exit_on_its_way(&self) -> bool {
        self.pty.has_exited() && !self.life.quit && self.life.exited.is_none()
    }

    /// Resizes the emulator to `size`, then tells the agent.
    fn resize(&mut self, size: Size) -> Result<()> {
        self.emu.resize(size);
        self.pty.resize(size)
    }

    /// Launches the agent again on a pty of `size`, as a new generation
    /// whose events go to `tx`, with an empty emulator and card. Returns a
    /// notice when its telemetry could not be prepared again, or when it
    /// could not start: the session then stays ended on its last launch,
    /// card included.
    pub(super) fn relaunch(
        &mut self,
        agent: &dyn agent::Agent,
        size: Size,
        tx: &mpsc::Sender<Msg>,
    ) -> Option<String> {
        // The old group may still hold the pty; nothing else will reach it
        // once its launch is replaced. It goes even when the new launch
        // then cannot start.
        if !self.life.closed {
            let _ = self.pty.kill_group_now();
        }
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
        self.life.relaunch(key.generation);
        (self.pty, self.on_panic) = launched;
        self.emu = Emulator::new(size, SCROLLBACK);
        notice
    }

    /// Queues bytes for the agent as if typed.
    pub(super) fn write(&self, bytes: &[u8]) {
        self.pty.write(bytes);
    }

    /// Kills the agent at once and lets the session go, its directory and
    /// its entry with the panic hook with it: for a session never shown.
    pub(super) fn discard(self) {
        let _ = self.pty.kill_now();
    }

    /// The line to draw over the agent's screen once it ended, naming it
    /// `agent`.
    pub(super) fn ended_line(&self, agent: &str) -> Option<String> {
        self.life
            .ended
            .as_ref()
            .map(|status| strings::agent_ended_line(agent, status.exit_code(), status.signal()))
    }

    /// The card of the left pane at `now`, in seconds since the Unix
    /// epoch, with the box of the agent named `agent` and the session's
    /// `time` while the session has an instance.
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
                ended: !self.life.alive(),
            }),
        }
    }

    /// The session's row in the list of sessions, its repository
    /// `changed` or not: the agent's state while it has telemetry.
    pub(super) fn row(&self, changed: bool) -> ui::projects::Row<'_> {
        ui::projects::Row {
            project: &self.place.project,
            status: self.instance.as_ref().map(|_| &self.status),
            ended: !self.life.alive(),
            changed,
        }
    }
}

/// The sessions of the process, the one on screen, and those whose agent
/// is being hung up. The list is never empty while the main loop runs.
pub(super) struct Sessions {
    list: Vec<Session>,
    /// The index of the session on screen.
    shown: usize,
    /// Sessions taken off the list to be closed, with how far the hangup
    /// of each went. A session stays here once done, with its agent's
    /// entry with the panic hook, until `Sessions` is dropped.
    closing: Vec<(Session, Closing)>,
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
    /// agent hidden gets a focus-out and the one shown a focus-in, each
    /// when it asked for focus events. Returns whether the session shown
    /// changed.
    pub(super) fn show(&mut self, id: SessionId, focused: bool) -> bool {
        let Some(index) = self.index(id) else {
            return false;
        };
        if index == self.shown {
            return false;
        }
        let (hidden, shown) = (&self.list[self.shown], &self.list[index]);
        let (out, into) = focus_bytes(hidden.emu.focus_events(), shown.emu.focus_events(), focused);
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

    /// The session after session `id`, or before it, stopping at both
    /// ends; counted from the one shown when `id` is not on the list.
    pub(super) fn toward(&self, id: SessionId, step: Step) -> SessionId {
        let from = self.index(id).unwrap_or(self.shown);
        self.list[clamped(from, self.list.len(), step)].id
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

    /// Session `id`, while there is one.
    fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.list.iter_mut().find(|session| session.id == id)
    }

    /// Applies `event` from the agent's launch that `key` names, at
    /// `now`: as [`Session::on_pty`] does for a session on the list,
    /// which has the focus when shown, noting only the end of the output
    /// and the exit for one being closed. An event for a session no
    /// longer there is dropped.
    pub(super) fn on_pty(&mut self, key: AgentKey, event: pty::Event, now: Instant) {
        let focused = self
            .list
            .get(self.shown)
            .is_some_and(|shown| shown.id == key.session);
        if let Some(session) = self.get_mut(key.session) {
            session.on_pty(key.generation, event, focused, now);
        } else if let Some((session, _)) = self
            .closing
            .iter_mut()
            .find(|(session, _)| session.id == key.session)
        {
            session.life.apply(key.generation, &event, now);
        }
    }

    /// Reads every agent's telemetry whose files changed, at `now`.
    /// Returns whether the shown session's status changed.
    pub(super) fn read_telemetry(&mut self, agent: &dyn agent::Agent, now: Instant) -> bool {
        self.on_each(|session| session.read_telemetry(agent, now))
    }

    /// Marks each agent's launch ended once it exited and its output
    /// ended or `CLOSE_GRACE` passed, at `now`. Returns whether the shown
    /// one just did.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        self.on_each(|session| session.life.settle(now))
    }

    /// Resizes every agent to `size` where it differs, the emulator
    /// first. Returns a notice naming the project of an agent whose pty
    /// refused the size, the last one when several did.
    pub(super) fn resize(&mut self, size: Size) -> Option<String> {
        let mut notice = None;
        for session in &mut self.list {
            if session.emu.size() != size
                && let Err(error) = session.resize(size)
            {
                let error = format!("{error:#}");
                notice = Some(strings::session_trouble(&session.place.project, &error));
            }
        }
        notice
    }

    /// Runs `act` on every session on the list. Returns what it returned
    /// for the shown one.
    fn on_each(&mut self, mut act: impl FnMut(&mut Session) -> bool) -> bool {
        let mut shown = false;
        for (index, session) in self.list.iter_mut().enumerate() {
            let answer = act(session);
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
            if !session.pty.has_exited() {
                session.life.quit();
            }
        }
    }

    /// Takes every session off the list and hangs its agent up, all at
    /// once, then drains `rx` until each hangup is done and an exit of
    /// the shown agent still on its way arrived or had `KILL_GRACE`: at
    /// most `GRACE` plus twice `KILL_GRACE`, however many sessions there
    /// are. Returns the shown session's exit status when its agent exited
    /// on its own before the quit.
    pub(super) fn close_all(&mut self, rx: &Receiver<Msg>) -> Option<ExitStatus> {
        let shown = self.shown().id;
        let exit_by = Instant::now() + KILL_GRACE;
        let list = self.list.drain(..);
        self.closing
            .extend(list.map(|session| (session, Closing::Start)));
        loop {
            let now = Instant::now();
            // An exit still on its way keeps the drain going once every
            // hangup is done.
            let exit_due = now < exit_by
                && self
                    .get_closing(shown)
                    .is_some_and(Session::exit_on_its_way);
            let Some(deadline) = self.step_closing(now).or(exit_due.then_some(exit_by)) else {
                break;
            };
            self.receive(rx, deadline);
        }
        self.get_closing(shown)
            .and_then(|session| session.life.status())
    }

    /// Session `id`, while it is being closed.
    fn get_closing(&self, id: SessionId) -> Option<&Session> {
        self.closing
            .iter()
            .map(|(session, _)| session)
            .find(|session| session.id == id)
    }

    /// Moves the hangup of every session being closed on at `now`.
    /// Returns when the next step is due; none once all are done.
    fn step_closing(&mut self, now: Instant) -> Option<Instant> {
        for (session, closing) in &mut self.closing {
            *closing = session.step_closing(*closing, now);
        }
        self.closing
            .iter()
            .filter_map(|(_, closing)| closing.deadline())
            .min()
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

/// How far the hangup of a session's agent went: the signal its process
/// group got last, with when the wait after it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    /// Nothing sent yet.
    Start,
    /// Hung up: until then for the agent to exit.
    HungUp(Instant),
    /// Killed, as the agent outlived the hangup: until then for it to be
    /// reaped.
    Killed(Instant),
    /// The group killed, as something in it held the terminal: until
    /// then for the output to end.
    GroupKilled(Instant),
    /// Nothing left to wait for.
    Done,
}

impl Closing {
    /// The stage after this one at `now`, given whether the agent
    /// `exited` and its output `closed`. The agent is hung up and gets
    /// `GRACE` to exit, then is killed and gets `KILL_GRACE`. Once it
    /// exited, or that passed, the group is killed while the output is
    /// open and gets `KILL_GRACE` more. An agent that exited with its
    /// output closed is done at once.
    fn step(self, exited: bool, closed: bool, now: Instant) -> Self {
        // The agent is gone, or had its last chance to go.
        let gone = if closed {
            Self::Done
        } else {
            Self::GroupKilled(now + KILL_GRACE)
        };
        match self {
            Self::Start | Self::HungUp(_) | Self::Killed(_) if exited => gone,
            Self::Start => Self::HungUp(now + GRACE),
            Self::HungUp(deadline) if now >= deadline => Self::Killed(now + KILL_GRACE),
            Self::Killed(deadline) if now >= deadline => gone,
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

/// What the main loop knows about the agent's current launch.
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
    /// caused, not the agent's own.
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

/// The session's time as the agent's box shows it: the duration the
/// agent last reported plus the time since it was read, stopped at the
/// agent's exit, and never behind what was shown before in the launch.
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

/// The index `step` leads to from `index` in a list of `len`: the next,
/// or the previous, staying at either end.
fn clamped(index: usize, len: usize, step: Step) -> usize {
    match step {
        Step::Down => (index + 1).min(len.saturating_sub(1)),
        Step::Up => index.saturating_sub(1),
    }
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
        let key = AgentKey::first(id);
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
    key: AgentKey,
    instance: Option<&Instance>,
) -> Result<(pty::Session, OnPanic)> {
    let spec = agent.spec(launch, size, instance);
    let pty = pty::Session::spawn(&spec, tx.clone(), move |event| Msg::Pty(key, event))
        .with_context(|| format!("start {}", agent.name()))?;
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;
    use tui_term::widget::Cursor;

    /// How long the fake agent gets for each of its events.
    const WAIT: Duration = Duration::from_secs(10);

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
    ) -> (Sessions, AgentKey, mpsc::Sender<Msg>, mpsc::Receiver<Msg>) {
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
    ) -> AgentKey {
        let place = Place::new(dir.to_path_buf(), None, None);
        let args = Vec::new();
        let id = sessions.next_id();
        let key = AgentKey::first(id);
        let launched = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            spawn(agent, &place.launch(&args), size, tx, key, None).expect("spawn")
        };
        let emu = Emulator::new(size, SCROLLBACK);
        sessions.push(Session::new(id, place, None, args, None, launched, emu));
        key
    }

    /// Applies the agent's events as the main loop does, until the
    /// current launch of the session `key` names closed its terminal and
    /// exited. Each event must be tagged `key`.
    fn run_until_ended(sessions: &mut Sessions, rx: &mpsc::Receiver<Msg>, key: AgentKey) {
        let ended = |session: &mut Session| session.life.closed && session.life.exited.is_some();
        while !sessions.get_mut(key.session).is_some_and(ended) {
            let Msg::Pty(from, event) = rx.recv_timeout(WAIT).expect("event before timeout") else {
                panic!("only the agent sends here");
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

    #[test]
    fn a_spawned_agent_reaches_its_session_through_its_key() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("printf hi");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, tx, rx) = start(&agent, dir.path(), size);
        let first = sessions.shown().pty.exited_flag();
        assert!(is_watched(&first), "the panic hook knows the agent");
        let other = sessions.next_id();
        assert!(sessions.get_mut(other).is_none(), "an id is not a session");
        run_until_ended(&mut sessions, &rx, key);
        let session = sessions.shown();
        assert_eq!(session.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        // The next launch tags its events with the next generation and
        // draws on an empty screen.
        let notice = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            sessions.shown_mut().relaunch(&agent, size, &tx)
        };
        assert_eq!(notice, None, "no instance to prepare");
        let second = sessions.shown().pty.exited_flag();
        assert!(!is_watched(&first), "the old launch is forgotten");
        assert!(is_watched(&second));
        let next = AgentKey {
            generation: key.generation + 1,
            ..key
        };
        // Output an older launch sends late is not drawn.
        sessions.on_pty(key, pty::Event::Output(b"old".to_vec()), Instant::now());
        run_until_ended(&mut sessions, &rx, next);
        let session = sessions.shown();
        assert_eq!(session.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        drop(sessions);
        assert!(!is_watched(&second), "a closed session is forgotten");
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
        let next = AgentKey {
            generation: key.generation + 1,
            ..key
        };
        run_until_ended(&mut sessions, &rx, next);
        assert!(sessions.settle(Instant::now()), "ended");
        fs::write(telemetry.join(crate::telemetry::STATUS_FILE), "M").expect("write");
        assert_eq!(model(&mut sessions).as_deref(), Some("M"));
        let last = sessions.shown().pty.exited_flag();
        // The directory went: the spawn refuses it.
        fs::remove_dir(&cwd).expect("rmdir");
        let notice = relaunch(&mut sessions).expect("a notice");
        assert!(notice.contains("is not a directory"), "{notice}");
        assert_eq!(model(&mut sessions).as_deref(), Some("M"), "the card stays");
        let session = sessions.shown();
        assert_eq!(session.life.generation, next.generation, "unchanged");
        assert!(!session.life.alive(), "still ended");
        assert!(is_watched(&last), "the last launch is still known");
        // Stragglers of the older launch are still ignored.
        sessions.on_pty(key, pty::Event::Output(b"old".to_vec()), Instant::now());
        sessions.on_pty(
            key,
            pty::Event::Exited(ExitStatus::with_exit_code(1)),
            Instant::now(),
        );
        let session = sessions.shown();
        assert_eq!(session.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
        // The launch that could not start kept its number.
        fs::create_dir(&cwd).expect("mkdir");
        assert_eq!(relaunch(&mut sessions), None);
        assert_eq!(model(&mut sessions), None, "a new launch starts empty");
        let failed = AgentKey {
            generation: next.generation + 1,
            ..key
        };
        let third = AgentKey {
            generation: next.generation + 2,
            ..key
        };
        assert_eq!(sessions.shown().life.generation, third.generation);
        sessions.on_pty(
            failed,
            pty::Event::Exited(ExitStatus::with_exit_code(1)),
            Instant::now(),
        );
        assert!(
            sessions.shown().life.exited.is_none(),
            "not the new launch's"
        );
        run_until_ended(&mut sessions, &rx, third);
        let status = sessions.shown().life.status();
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
        assert!(sessions.settle(Instant::now()));
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
        assert_eq!(session.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.emu)
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
            !sessions.settle(Instant::now()),
            "only the shown one counts"
        );
        let session = sessions.get_mut(hidden.session).expect("its session");
        assert!(!session.life.alive(), "the hidden one ended");
        run_until_ended(&mut sessions, &rx, shown);
        assert!(sessions.settle(Instant::now()), "the shown one ended");
        for key in [shown, hidden] {
            let session = sessions.get_mut(key.session).expect("its session");
            assert_eq!(session.emu.size(), size);
            screen(&session.emu).backend().assert_buffer_lines([
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
        while !sessions.shown().emu.focus_events() {
            let Ok(Msg::Pty(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("output before timeout");
            };
            sessions.on_pty(from, event, Instant::now());
        }
        // Queued after a focus-out, had there been one.
        sessions.shown().write(b"abc");
        run_until_ended(&mut sessions, &rx, key);
        screen(&sessions.shown().emu)
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
        screen(&hidden.emu)
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
        screen(&hidden.emu)
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
        while !sessions.shown().emu.focus_events() {
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
        screen(&hidden.emu)
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
                .is_some_and(|session| session.emu.focus_events())
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
            screen(&session.emu)
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

    #[test]
    fn the_list_cursor_moves_from_its_session_or_from_the_one_shown() {
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("exec sleep 30");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, first, tx, _rx) = start(&agent, dir.path(), size);
        let middle = add(&mut sessions, &agent, dir.path(), size, &tx);
        let last = add(&mut sessions, &agent, dir.path(), size, &tx);
        let [first, middle, last] = [first, middle, last].map(|key| key.session);
        let unknown = sessions.next_id();
        assert_eq!(sessions.index(last), Some(2));
        assert_eq!(sessions.index(unknown), None);
        assert!(sessions.show(middle, false));
        assert_eq!(sessions.toward(first, Step::Down), middle);
        assert_eq!(sessions.toward(last, Step::Up), middle);
        assert_eq!(sessions.toward(first, Step::Up), first, "at the start");
        assert_eq!(sessions.toward(last, Step::Down), last, "at the end");
        // An id not on the list counts from the session shown.
        assert_eq!(sessions.toward(unknown, Step::Down), last);
        assert_eq!(sessions.toward(unknown, Step::Up), first);
    }

    #[test]
    fn the_list_cursor_stops_at_both_ends() {
        assert_eq!(clamped(0, 3, Step::Down), 1);
        assert_eq!(clamped(2, 3, Step::Down), 2);
        assert_eq!(clamped(0, 3, Step::Up), 0);
        assert_eq!(clamped(2, 3, Step::Up), 1);
        assert_eq!(clamped(0, 1, Step::Down), 0, "one session");
        assert_eq!(clamped(0, 0, Step::Down), 0, "none");
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
            let flag = session.pty.exited_flag();
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
        let exited = sessions.shown().pty.exited_flag();
        let started = Instant::now();
        sessions.quit();
        assert!(sessions.close_all(&rx).is_none(), "the agent did not exit");
        let took = started.elapsed();
        assert!(took >= GRACE, "hung up first: {took:?}");
        // Killed at `GRACE`, not left to the group kill after it.
        assert!(took < GRACE + KILL_GRACE, "killed: {took:?}");
        assert!(exited.load(Ordering::SeqCst), "reaped");
        assert!(sessions.closing[0].0.life.closed, "its output ended");
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
        let exited = sessions.shown().pty.exited_flag();
        let started = Instant::now();
        sessions.quit();
        let status = sessions.close_all(&rx);
        let took = started.elapsed();
        assert!(status.is_none(), "the hangup's exit is not the agent's own");
        // No kill, and no wait for an exit that came before the quit.
        assert!(took < KILL_GRACE, "hung up: {took:?}");
        assert!(exited.load(Ordering::SeqCst), "reaped");
        assert!(sessions.closing[0].0.life.closed, "its output ended");
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
        while !sessions.shown().pty.has_exited() {
            assert!(Instant::now() < deadline, "the agent exits");
            thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        sessions.quit();
        let status = sessions.close_all(&rx);
        assert_eq!(status.map(|status| status.exit_code()), Some(3));
        assert!(started.elapsed() < GRACE, "no hangup to wait for");
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
            // Exits within GRACE and closes: done.
            (HungUp(t1), false, false, t1 - ms, HungUp(t1)),
            (HungUp(t1), true, true, t0 + ms, Done),
            // Ignores the hangup: killed at GRACE.
            (HungUp(t1), false, false, t1, Killed(t2)),
            // Exits right at GRACE: not killed.
            (HungUp(t1), true, false, t1, GroupKilled(t2)),
            (HungUp(t1), true, true, t1, Done),
            (Killed(t2), false, false, t2 - ms, Killed(t2)),
            (Killed(t2), true, true, t1 + ms, Done),
            // Exited with the output still open: group kill.
            (HungUp(t1), true, false, t0, GroupKilled(t0 + KILL_GRACE)),
            (Killed(t2), true, false, t1, GroupKilled(t1 + KILL_GRACE)),
            (GroupKilled(t3), true, false, t3 - ms, GroupKilled(t3)),
            (GroupKilled(t3), true, true, t2 + ms, Done),
            // Not reaped by the kill's deadline: group kill, unless the
            // output ended.
            (Killed(t2), false, false, t2, GroupKilled(t3)),
            (Killed(t2), false, true, t2, Done),
            // Last deadline passed: done anyway.
            (GroupKilled(t3), false, false, t3, Done),
            // Exited and closed: done at once.
            (Start, true, true, t0, Done),
            (Start, true, false, t0, GroupKilled(t0 + KILL_GRACE)),
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
