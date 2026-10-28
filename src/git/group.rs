//! A child, git, the agent asked for a commit subject or `pbcopy`, run in
//! a process group of its own against a deadline or a stop, with its
//! input written and its output drained on threads.

use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long the output of a command gets to arrive once the child is
/// gone, before whatever still holds the pipes is killed.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// How the child ended.
pub(crate) enum Waited {
    Exited(ExitStatus),
    /// Killed by the timeout, or once stopped.
    Killed,
}

/// The name of `program`, without its directory.
pub(crate) fn name(program: &Path) -> String {
    program.file_name().map_or_else(
        || program.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The first line of `text` with something on it, as the screen can
/// show it.
pub(crate) fn first_line(text: &str) -> Option<String> {
    text.lines().map(super::clean).find(|line| !line.is_empty())
}

/// The process group of `child`, spawned as the leader of one.
pub(crate) fn of(child: &Child) -> Result<Pid> {
    Ok(Pid::from_raw(
        i32::try_from(child.id()).context("pid out of range")?,
    ))
}

/// Waits for `child`, which runs `what`, until `deadline`; past it, kills
/// the process group `group` and reaps the child. A child that had
/// exited on its own just before counts as exited.
pub(crate) fn wait(child: &mut Child, deadline: Instant, group: Pid, what: &str) -> Result<Waited> {
    wait_or_stop(child, deadline, &AtomicBool::new(false), group, what)
}

/// Waits as [`wait`] does, and kills the group as past the deadline once
/// `stop` is set.
pub(crate) fn wait_or_stop(
    child: &mut Child,
    deadline: Instant,
    stop: &AtomicBool,
    group: Pid,
    what: &str,
) -> Result<Waited> {
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("wait for {what}"))?
        {
            return Ok(Waited::Exited(status));
        }
        if Instant::now() >= deadline || stop.load(Ordering::SeqCst) {
            let _ = killpg(group, Signal::SIGKILL);
            let status = child.wait().with_context(|| format!("reap {what}"))?;
            return Ok(if status.signal() == Some(Signal::SIGKILL as i32) {
                Waited::Killed
            } else {
                Waited::Exited(status)
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Writes `bytes` to `pipe` on a thread and closes it, so that a child
/// that reads late never blocks the caller. A write that fails shows as
/// the child's own failure.
pub(crate) fn feed<W: Write + Send + 'static>(pipe: Option<W>, bytes: Vec<u8>) {
    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let _ = pipe.write_all(&bytes);
        }
    });
}

/// Reads `pipe` to its end on a thread and hands the bytes over; the
/// child never blocks on a full pipe, and nothing waits on the thread.
pub(crate) fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut output);
        }
        let _ = tx.send(output);
    });
    rx
}

/// What a drained pipe carried, waiting for its end at most the grace;
/// past it, whatever a hook or the child left holding the pipe is killed
/// with the group, and what arrived until then is used.
pub(crate) fn collect(rx: &Receiver<Vec<u8>>, group: Pid) -> Vec<u8> {
    rx.recv_timeout(OUTPUT_GRACE).unwrap_or_else(|_| {
        let _ = killpg(group, Signal::SIGKILL);
        rx.recv_timeout(Duration::from_millis(200))
            .unwrap_or_default()
    })
}
