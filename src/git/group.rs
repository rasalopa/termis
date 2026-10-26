//! Git run in a process group of its own against a deadline, with its
//! output drained on threads.

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long the output of a failed command gets to arrive once git is
/// gone, before whatever still holds the pipes is killed.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// How git ended.
pub(super) enum Waited {
    Exited(ExitStatus),
    /// Killed by the timeout.
    Killed,
}

/// The process group of `child`, spawned as the leader of one.
pub(super) fn of(child: &Child) -> Result<Pid> {
    Ok(Pid::from_raw(
        i32::try_from(child.id()).context("pid out of range")?,
    ))
}

/// Waits for `child`, `what` git runs, until `deadline`; past it, kills
/// the process group `group` and reaps the child. A child that had
/// exited on its own just before counts as exited.
pub(super) fn wait(child: &mut Child, deadline: Instant, group: Pid, what: &str) -> Result<Waited> {
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("wait for {what}"))?
        {
            return Ok(Waited::Exited(status));
        }
        if Instant::now() >= deadline {
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

/// Reads `pipe` to its end on a thread and hands the bytes over; git
/// never blocks on a full pipe, and nothing waits on the thread.
pub(super) fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
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
/// past it, whatever a hook left holding the pipe is killed with the
/// group, and what arrived until then is used.
pub(super) fn collect(rx: &Receiver<Vec<u8>>, group: Pid) -> Vec<u8> {
    rx.recv_timeout(OUTPUT_GRACE).unwrap_or_else(|_| {
        let _ = killpg(group, Signal::SIGKILL);
        rx.recv_timeout(Duration::from_millis(200))
            .unwrap_or_default()
    })
}
