//! Run a command on a pseudo-terminal and stream what it writes.
//!
//! A [`Session`] owns the master side of the pty. Two threads deliver
//! [`Event`]s over a channel: one reads output until the child closes the
//! terminal, the other waits for the child to exit. The two threads are not
//! ordered with respect to each other.

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;

use anyhow::{Context, Result, ensure};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::Size;

pub use portable_pty::ExitStatus;

impl From<Size> for PtySize {
    fn from(size: Size) -> Self {
        PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// What to run and where.
#[derive(Clone, Debug)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    /// Must be an existing directory.
    pub cwd: PathBuf,
    /// Variables set on top of the inherited environment.
    pub env: Vec<(String, String)>,
    /// Variables set only when the inherited environment lacks them.
    pub env_default: Vec<(String, String)>,
    /// Variables removed from the inherited environment. `SHELL` cannot be
    /// removed; portable-pty sets it.
    pub env_remove: Vec<String>,
    pub size: Size,
}

/// Something the child did.
#[derive(Debug)]
pub enum Event {
    /// Bytes the child wrote to its terminal.
    Output(Vec<u8>),
    /// The child closed its side of the terminal; no more output follows.
    Closed,
    /// The child exited. May arrive before or after [`Event::Closed`]; keep
    /// reading until both have been seen.
    Exited(ExitStatus),
}

/// A running child and the handles needed to talk to it.
///
/// Input is written by a thread, so [`Session::write`] never blocks the
/// caller when the child stops reading. Dropping the session sends a
/// hangup to the child's process group if it has not exited, then closes
/// this side's handles; closing the writer makes portable-pty send a
/// newline and the terminal's end-of-file character to the child first. A
/// child that ignores hangups survives the drop; call [`Session::kill_now`]
/// before dropping to end it. The reader, writer and waiter threads live
/// until the child closes the terminal and exits.
pub struct Session {
    master: Box<dyn MasterPty + Send>,
    writes: Sender<Vec<u8>>,
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
}

impl Session {
    /// Spawns `spec` on a new pty. Events go to `events`, converted into
    /// the caller's message type, from two threads until the child closes
    /// its terminal and exits.
    pub fn spawn<E>(spec: &Spec, events: Sender<E>) -> Result<Self>
    where
        E: From<Event> + Send + 'static,
    {
        ensure!(
            spec.cwd.is_dir(),
            "{} is not a directory",
            spec.cwd.display()
        );

        let pair = native_pty_system()
            .openpty(spec.size.into())
            .context("open pty")?;
        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for name in &spec.env_remove {
            cmd.env_remove(name);
        }
        for (name, value) in &spec.env_default {
            if std::env::var_os(name).is_none_or(|v| v.is_empty()) {
                cmd.env(name, value);
            }
        }
        for (name, value) in &spec.env {
            cmd.env(name, value);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn {}", spec.program))?;
        // The slave stays open in the child only; keeping it here would stop
        // the reader from ever seeing end of file.
        drop(pair.slave);

        let pid = child.process_id();
        let exited = Arc::new(AtomicBool::new(false));
        let (writes, queued) = mpsc::channel::<Vec<u8>>();

        let session = Self {
            master: pair.master,
            writes,
            pid,
            exited: Arc::clone(&exited),
        };

        let mut writer = writer;
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                // Ends when the session drops the sender or the pty rejects
                // a write, which happens once the child is gone.
                for bytes in queued {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("start pty writer thread")?;

        let waiter_events = events.clone();
        thread::Builder::new()
            .name("pty-waiter".into())
            .spawn(move || {
                let status = child.wait();
                exited.store(true, Ordering::SeqCst);
                if let Ok(status) = status {
                    let _ = waiter_events.send(E::from(Event::Exited(status)));
                }
            })
            .context("start pty waiter thread")?;

        let reader_thread = thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        // Zero bytes is end of file; portable-pty already
                        // turns the EIO some platforms return into it.
                        Ok(0) => break,
                        Ok(n) => {
                            if events
                                .send(E::from(Event::Output(buf[..n].to_vec())))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = events.send(E::from(Event::Closed));
            });
        if let Err(e) = reader_thread {
            let _ = session.hangup();
            return Err(e).context("start pty reader thread");
        }

        Ok(session)
    }

    /// Queues bytes for the child as if typed. Never blocks; bytes queued
    /// after the child exited, or after the terminal rejected a write, are
    /// dropped.
    pub fn write(&self, bytes: &[u8]) {
        if !self.has_exited() {
            let _ = self.writes.send(bytes.to_vec());
        }
    }

    /// Tells the child its terminal changed size.
    pub fn resize(&self, size: Size) -> Result<()> {
        self.master.resize(size.into()).context("resize pty")
    }

    /// Process id of the child, when the platform reports one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Whether the waiter thread has seen the child exit.
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// The flag behind [`Session::has_exited`], for code that must check it
    /// without a session at hand, such as a panic hook.
    pub fn exited_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.exited)
    }

    /// Sends SIGHUP to the child's process group. A child that handles or
    /// ignores it keeps running; wait for [`Event::Exited`] and fall back to
    /// [`Session::kill_now`].
    pub fn hangup(&self) -> Result<()> {
        self.signal_group(Signal::SIGHUP)
    }

    /// Sends SIGKILL to the child's process group.
    pub fn kill_now(&self) -> Result<()> {
        self.signal_group(Signal::SIGKILL)
    }

    /// Sends SIGKILL to the child's process group even after the child
    /// exited, for grandchildren that keep the terminal open. A process
    /// group id stays reserved while the group has members, so this cannot
    /// reach a stranger.
    pub fn kill_group_now(&self) -> Result<()> {
        let pid = self.pid.context("child has no pid")?;
        signal_pid_group(pid, Signal::SIGKILL)
    }

    /// Does nothing once the child has exited, so the pid is never reused
    /// against a stranger.
    fn signal_group(&self, signal: Signal) -> Result<()> {
        if self.has_exited() {
            return Ok(());
        }
        let pid = self.pid.context("child has no pid")?;
        signal_pid_group(pid, signal)
    }
}

/// Sends `signal` to the process group led by `pid`. A child spawned here
/// is a session leader, so its process group id is its pid. A group that is
/// already gone counts as success.
fn signal_pid_group(pid: u32, signal: Signal) -> Result<()> {
    let pgid = Pid::from_raw(i32::try_from(pid).context("pid out of range")?);
    match killpg(pgid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("send {signal} to child")),
    }
}

/// Sends SIGHUP to the process group led by `pid`, for code that has no
/// [`Session`] at hand, such as a panic hook. The caller checks the flag
/// from [`Session::exited_flag`] first, so a reused pid is never signalled.
pub fn hangup_group(pid: u32) -> Result<()> {
    signal_pid_group(pid, Signal::SIGHUP)
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.hangup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

    /// Serialises spawns: portable-pty installs a pre-exec hook, so std forks
    /// instead of posix_spawn, and concurrent forks from several test threads
    /// make the child abort in libSystem's atfork handler on macOS.
    static SPAWN: Mutex<()> = Mutex::new(());

    fn spawn(spec: &Spec) -> (Session, Receiver<Event>) {
        let (tx, rx) = mpsc::channel::<Event>();
        let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        (Session::spawn(spec, tx).expect("spawn"), rx)
    }

    fn sh(script: &str, size: Size) -> Spec {
        Spec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: vec![("TERM".into(), "dumb".into())],
            env_default: vec![],
            env_remove: vec![],
            size,
        }
    }

    /// Collects events until the child has both closed its terminal and
    /// exited. Returns the output and the exit status.
    fn run(spec: &Spec) -> (Vec<u8>, ExitStatus) {
        let (session, rx) = spawn(spec);
        let (output, status) = drain(&rx);
        assert!(session.has_exited());
        (output, status)
    }

    fn drain(rx: &Receiver<Event>) -> (Vec<u8>, ExitStatus) {
        let mut output = Vec::new();
        let mut status = None;
        let mut closed = false;
        while !(closed && status.is_some()) {
            match rx.recv_timeout(WAIT).expect("event before timeout") {
                Event::Output(bytes) => output.extend(bytes),
                Event::Closed => closed = true,
                Event::Exited(s) => status = Some(s),
            }
        }
        (output, status.expect("exit status"))
    }

    /// Reads output until it ends with `marker`.
    fn wait_for(rx: &Receiver<Event>, marker: &str) {
        let mut output = Vec::new();
        loop {
            match rx.recv_timeout(WAIT).expect("event before timeout") {
                Event::Output(bytes) => {
                    output.extend(bytes);
                    if String::from_utf8_lossy(&output).ends_with(marker) {
                        return;
                    }
                }
                other => panic!("unexpected {other:?} before {marker:?}"),
            }
        }
    }

    fn exit_status(rx: &Receiver<Event>, within: Duration) -> Option<ExitStatus> {
        let deadline = std::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(Event::Exited(status)) => return Some(status),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    #[test]
    fn streams_output_and_reports_exit_code() {
        let spec = sh("printf 'hello'; exit 3", Size { rows: 24, cols: 80 });
        let (output, status) = run(&spec);
        assert_eq!(String::from_utf8_lossy(&output), "hello");
        assert!(!status.success());
        assert_eq!(status.exit_code(), 3);
    }

    #[test]
    fn child_sees_the_requested_size() {
        let spec = sh(
            "stty size",
            Size {
                rows: 31,
                cols: 113,
            },
        );
        let (output, status) = run(&spec);
        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&output).trim(), "31 113");
    }

    #[test]
    fn applies_environment_changes() {
        let mut spec = sh(
            "printf '%s|%s|%s|%s' \"$TERM_TUI_TEST\" \"${HOME:-unset}\" \"$TERM_TUI_DEFAULT\" \"$PATH\"",
            Size { rows: 24, cols: 80 },
        );
        spec.env.push(("TERM_TUI_TEST".into(), "yes".into()));
        spec.env_remove.push("HOME".into());
        spec.env_default
            .push(("TERM_TUI_DEFAULT".into(), "filled".into()));
        spec.env_default.push(("PATH".into(), "ignored".into()));
        let (output, _) = run(&spec);
        let output = String::from_utf8_lossy(&output);
        let path = std::env::var("PATH").expect("PATH is set in tests");
        assert_eq!(output, format!("yes|unset|filled|{path}"));
    }

    #[test]
    fn rejects_a_missing_cwd() {
        let mut spec = sh("true", Size { rows: 24, cols: 80 });
        spec.cwd = std::env::temp_dir().join("term-tui-does-not-exist");
        let (tx, _rx) = mpsc::channel::<Event>();
        let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        assert!(Session::spawn(&spec, tx).is_err());
    }

    #[test]
    fn writes_reach_the_child() {
        let spec = sh(
            "read line; printf 'got %s' \"$line\"",
            Size { rows: 24, cols: 80 },
        );
        let (session, rx) = spawn(&spec);
        session.write(b"ping\r");
        let (output, status) = drain(&rx);
        assert!(status.success());
        // The terminal echoes the typed line before the child's reply.
        assert!(String::from_utf8_lossy(&output).ends_with("got ping"));
    }

    #[test]
    fn hangup_ends_a_child_that_never_exits() {
        let spec = sh("printf ready; exec sleep 60", Size { rows: 24, cols: 80 });
        let (session, rx) = spawn(&spec);
        wait_for(&rx, "ready");
        assert!(!session.has_exited());
        session.hangup().expect("hangup");
        let status = exit_status(&rx, WAIT).expect("child exits after hangup");
        assert!(!status.success());
        assert!(session.has_exited());
    }

    #[test]
    fn kill_now_ends_a_child_that_ignores_hangup() {
        let spec = sh(
            "trap '' HUP; printf ready; exec sleep 60",
            Size { rows: 24, cols: 80 },
        );
        let (session, rx) = spawn(&spec);
        wait_for(&rx, "ready");
        session.hangup().expect("hangup");
        assert!(
            exit_status(&rx, Duration::from_millis(300)).is_none(),
            "a child that ignores SIGHUP must survive hangup"
        );
        session.kill_now().expect("kill");
        let status = exit_status(&rx, WAIT).expect("child exits after kill");
        assert!(!status.success());
    }

    #[test]
    fn writes_after_exit_are_dropped() {
        let spec = sh("true", Size { rows: 24, cols: 80 });
        let (session, rx) = spawn(&spec);
        drain(&rx);
        assert!(session.has_exited());
        session.write(b"never read");
    }

    #[test]
    fn signals_after_exit_are_ignored() {
        let spec = sh("true", Size { rows: 24, cols: 80 });
        let (output, status) = {
            let (session, rx) = spawn(&spec);
            let result = drain(&rx);
            assert!(session.has_exited());
            session.hangup().expect("hangup after exit is a no-op");
            session.kill_now().expect("kill after exit is a no-op");
            result
        };
        assert!(output.is_empty());
        assert!(status.success());
    }
}
