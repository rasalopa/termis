//! Agent sessions: the agent on its pty, launched in a place with the
//! user's arguments, and what the main loop keeps of it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::host::Host;
use super::{Msg, SCROLLBACK};
use crate::Size;
use crate::agent;
use crate::emu::Emulator;
use crate::instance::{Instance, Watch};
use crate::projects::{self, Stack};
use crate::pty::{self, ExitStatus};
use crate::strings;
use crate::ui;

/// After the child exited, how long to wait for its output to end.
const CLOSE_GRACE: Duration = Duration::from_millis(200);

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
    /// The user's extra arguments for the agent.
    args: Vec<String>,
    /// Whether the agent's current launch exited and its output ended.
    pub(super) life: Lifecycle,
    /// The agent's current launch. Declared before `instance`: the agent
    /// is hung up before its directory goes.
    pty: pty::Session,
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
    /// Session `id` in `place`: the agent, run with `args` on `pty` with
    /// `instance` for its telemetry, draws on `emu`.
    pub(super) fn new(
        id: SessionId,
        place: Place,
        args: Vec<String>,
        instance: Option<Instance>,
        pty: pty::Session,
        emu: Emulator,
    ) -> Self {
        Self {
            id,
            place,
            args,
            life: Lifecycle::default(),
            pty,
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

    /// The key the agent's current launch tags its events with.
    fn key(&self) -> AgentKey {
        AgentKey {
            session: self.id,
            generation: self.life.generation,
        }
    }

    /// The agent's current launch.
    pub(super) fn pty(&self) -> &pty::Session {
        &self.pty
    }

    /// Applies `event` from the agent's launch `generation`, at `now`. The
    /// current launch's output goes to the emulator, and the replies it
    /// asks for, when there are some, to the agent; once the current
    /// launch exited, the panic hook has no child to hang up. The
    /// lifecycle notes the rest and drops what an older launch sent.
    pub(super) fn on_pty(&mut self, generation: u32, event: pty::Event, now: Instant) {
        match event {
            pty::Event::Output(bytes) if self.life.current(generation) => {
                self.emu.process(&bytes);
                let replies = self.emu.take_replies();
                if !replies.is_empty() {
                    self.pty.write(&replies);
                }
            }
            event => {
                if self.life.current(generation) && matches!(event, pty::Event::Exited(_)) {
                    Host::forget_child();
                }
                self.life.apply(generation, &event, now);
            }
        }
    }

    /// Reads the agent's telemetry when its files changed, at `now`.
    /// Returns whether the status changed.
    pub(super) fn read_telemetry(&mut self, agent: &dyn agent::Agent, now: Instant) -> bool {
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

    /// Resizes the emulator to `size`, then tells the agent.
    pub(super) fn resize(&mut self, size: Size) -> Result<()> {
        self.emu.resize(size);
        self.pty.resize(size)
    }

    /// Launches the agent again on a pty of `size`, as a new generation
    /// whose events go to `tx`, with an empty emulator. Returns why its
    /// telemetry could not be prepared again, if it could not.
    pub(super) fn relaunch(
        &mut self,
        agent: &dyn agent::Agent,
        size: Size,
        tx: &mpsc::Sender<Msg>,
    ) -> Result<Option<String>> {
        // The old group may still hold the pty; nothing else will reach it
        // once its launch is replaced.
        if !self.life.closed {
            let _ = self.pty.kill_group_now();
        }
        self.life.relaunch();
        // The new launch starts without the old one's status and events,
        // and with the status line the settings name now.
        let mut trouble = None;
        if let Some(instance) = &self.instance {
            instance.clear();
            if let Err(error) = agent.prepare(&self.launch(), instance) {
                trouble = Some(format!("{error:#}"));
            }
        }
        self.pty = spawn(
            agent,
            &self.launch(),
            size,
            tx,
            self.key(),
            self.instance.as_ref(),
        )?;
        self.emu = Emulator::new(size, SCROLLBACK);
        Ok(trouble)
    }

    /// Queues bytes for the agent as if typed.
    pub(super) fn write(&self, bytes: &[u8]) {
        self.pty.write(bytes);
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
}

/// The sessions of the process and the one on screen. Never empty while
/// the main loop runs.
pub(super) struct Sessions {
    list: Vec<Session>,
    /// The index of the session on screen.
    shown: usize,
    /// The id the next session gets.
    next: u32,
}

impl Sessions {
    /// No sessions yet.
    pub(super) fn new() -> Self {
        Self {
            list: Vec::new(),
            shown: 0,
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

    /// The session on screen.
    pub(super) fn shown(&self) -> &Session {
        &self.list[self.shown]
    }

    /// The session on screen, to change.
    pub(super) fn shown_mut(&mut self) -> &mut Session {
        &mut self.list[self.shown]
    }

    /// Session `id`, while there is one.
    pub(super) fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.list.iter_mut().find(|session| session.id == id)
    }
}

/// Where a session runs.
pub(super) struct Place {
    /// The directory the agent runs in.
    cwd: PathBuf,
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

    /// How the agent is launched here, with `args`.
    pub(super) fn launch<'a>(&'a self, args: &'a [String]) -> agent::Launch<'a> {
        agent::Launch {
            cwd: &self.cwd,
            root: &self.root,
            args,
        }
    }
}

/// What the main loop knows about the agent's current launch.
#[derive(Debug, Default)]
pub(super) struct Lifecycle {
    /// Counts launches; pty events from older ones are ignored.
    generation: u32,
    /// The launch's reader saw the end of its output.
    pub(super) closed: bool,
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
    /// Notes a `Closed` or `Exited` event; events from an older launch
    /// are dropped. Output is the caller's to apply.
    pub(super) fn apply(&mut self, from: u32, event: &pty::Event, now: Instant) {
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
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        let Some((status, at)) = &self.exited else {
            return false;
        };
        if self.ended.is_some() || !(self.closed || now.duration_since(*at) >= CLOSE_GRACE) {
            return false;
        }
        self.ended = Some(status.clone());
        true
    }

    /// Starts counting a new launch.
    fn relaunch(&mut self) {
        self.generation += 1;
        self.closed = false;
        self.exited = None;
        self.ended = None;
    }

    pub(super) fn alive(&self) -> bool {
        self.ended.is_none()
    }

    /// Notes that the user quit. From then on the status is the one the
    /// agent exited with before, if any.
    pub(super) fn quit(&mut self) {
        self.quit = true;
    }

    /// The exit status of the current launch, once it exited on its own.
    pub(super) fn status(&self) -> Option<ExitStatus> {
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

/// An instance prepared for a session of `launch`, or why there is
/// none: without one the agent runs without telemetry.
pub(super) fn instance_for(
    agent: &dyn agent::Agent,
    launch: &agent::Launch<'_>,
) -> (Option<Instance>, Option<String>) {
    match Instance::create().and_then(|instance| {
        agent.prepare(launch, &instance)?;
        Ok(instance)
    }) {
        Ok(instance) => (Some(instance), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    }
}

/// Starts the agent on a pty of `size`; its events reach `tx` tagged
/// with `key`.
pub(super) fn spawn(
    agent: &dyn agent::Agent,
    launch: &agent::Launch<'_>,
    size: Size,
    tx: &mpsc::Sender<Msg>,
    key: AgentKey,
    instance: Option<&Instance>,
) -> Result<pty::Session> {
    let spec = agent.spec(launch, size, instance);
    let pty = pty::Session::spawn(&spec, tx.clone(), move |event| Msg::Pty(key, event))
        .with_context(|| format!("start {}", agent.name()))?;
    Host::watch_child(pty.pid(), pty.exited_flag());
    Ok(pty)
}

/// Last path component, or the path itself when it has none.
fn project_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use tempfile::TempDir;
    use tui_term::widget::Cursor;

    /// How long the fake agent gets for each of its events.
    const WAIT: Duration = Duration::from_secs(10);

    /// An agent that runs this shell script and exits.
    struct Fake(&'static str);

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
    }

    /// The first session, running `agent` in `dir` on a pty of `size`,
    /// the key of its launch, and both ends of the channel its events
    /// reach.
    fn start(
        agent: &Fake,
        dir: &Path,
        size: Size,
    ) -> (Sessions, AgentKey, mpsc::Sender<Msg>, mpsc::Receiver<Msg>) {
        let place = Place::new(dir.to_path_buf(), None, None);
        let args = Vec::new();
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut sessions = Sessions::new();
        let id = sessions.next_id();
        let key = AgentKey::first(id);
        let pty = spawn(agent, &place.launch(&args), size, &tx, key, None).expect("spawn");
        let emu = Emulator::new(size, SCROLLBACK);
        sessions.push(Session::new(id, place, args, None, pty, emu));
        (sessions, key, tx, rx)
    }

    /// Applies the agent's events as the main loop does, until the shown
    /// session's launch closed its terminal and exited. Each event must
    /// be tagged `key`.
    fn run_until_ended(sessions: &mut Sessions, rx: &mpsc::Receiver<Msg>, key: AgentKey) {
        while !(sessions.shown().life.closed && sessions.shown().life.exited.is_some()) {
            let Msg::Pty(from, event) = rx.recv_timeout(WAIT).expect("event before timeout") else {
                panic!("only the agent sends here");
            };
            assert_eq!(from, key);
            let session = sessions.get_mut(from.session).expect("its session");
            session.on_pty(from.generation, event, Instant::now());
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
        // Held to the end: the spawn registers the agent with the panic
        // hook and its exit forgets it, which a test of the host reads.
        let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().expect("temp dir");
        let agent = Fake("printf hi");
        let size = Size { rows: 2, cols: 10 };
        let (mut sessions, key, tx, rx) = start(&agent, dir.path(), size);
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
        let trouble = sessions
            .shown_mut()
            .relaunch(&agent, size, &tx)
            .expect("relaunch");
        assert_eq!(trouble, None, "no instance to prepare");
        let next = AgentKey {
            generation: key.generation + 1,
            ..key
        };
        // Output an older launch sends late is not drawn.
        sessions.shown_mut().on_pty(
            key.generation,
            pty::Event::Output(b"old".to_vec()),
            Instant::now(),
        );
        run_until_ended(&mut sessions, &rx, next);
        let session = sessions.shown();
        assert_eq!(session.life.status().map(|s| s.exit_code()), Some(0));
        screen(&session.emu)
            .backend()
            .assert_buffer_lines(["hi        ", "          "]);
    }

    #[test]
    fn an_agent_gets_the_reply_to_its_query() {
        let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
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
}
