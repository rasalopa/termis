//! Committing through the `git` binary.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long the output of a failed commit gets to arrive once git is
/// gone, before whatever still holds the pipes is killed.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// The message for `subject` and `body`: the body, when there is one,
/// goes after one blank line whatever it starts with. The subject must
/// not be empty, or git takes the body's first line for it.
pub fn message(subject: &str, body: &str) -> String {
    let body = body.trim_start_matches('\n').trim_end();
    if body.is_empty() {
        format!("{subject}\n")
    } else {
        format!("{subject}\n\n{body}\n")
    }
}

/// Commits what is staged in the repository at `root` with `message`,
/// taken as written, and returns the short hash of the new commit.
///
/// Git and its hooks run in their own process group and get `timeout`,
/// after which the group is killed; a commit that was made by then still
/// counts. The message goes in through a pipe that then closes. Git
/// cannot prompt, and a curses pinentry cannot take the screen since
/// `GPG_TTY` is withheld; a hook that reads the terminal stops until the
/// timeout kills it. On failure the error is the first line git or a
/// hook printed (hooks' stdout arrives on stderr too), else the last
/// line of git's stdout, else the exit status.
pub fn commit(root: &Path, message: &str, timeout: Duration) -> Result<String> {
    let before = head(root).ok();
    let mut child = Command::new("git")
        .args(["-c", "advice.statusHints=false", "commit", "-q", "-F", "-"])
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GPG_TTY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("run git commit")?;
    let group = Pid::from_raw(i32::try_from(child.id()).context("pid out of range")?);
    let deadline = Instant::now() + timeout;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    // Written on its own thread: git runs the pre-commit hook before it
    // reads the message, and a long message would block a hook away.
    // A write that fails shows as git's own failure.
    feed(child.stdin.take(), message.as_bytes().to_vec());
    match wait(&mut child, deadline, group)? {
        Waited::Exited(status) if status.success() => head(root),
        Waited::Exited(status) => Err(failure(status, &stdout, &stderr, group)),
        Waited::Killed => match (before, head(root).ok()) {
            (before, Some(now)) if before.as_ref() != Some(&now) => Ok(now),
            _ => bail!("git commit took longer than {timeout:?} and was killed"),
        },
    }
}

/// The short hash of `HEAD` in the repository at `root`.
fn head(root: &Path) -> Result<String> {
    let hash = super::run(root, ["rev-parse", "--short", "HEAD"]).context("git rev-parse")?;
    Ok(String::from_utf8_lossy(&hash).trim().to_owned())
}

/// How git ended.
enum Waited {
    Exited(ExitStatus),
    /// Killed by the timeout.
    Killed,
}

/// Waits for `child` until `deadline`; past it, kills the process group
/// `group` and reaps the child. A child that had exited on its own just
/// before counts as exited.
fn wait(child: &mut Child, deadline: Instant, group: Pid) -> Result<Waited> {
    loop {
        if let Some(status) = child.try_wait().context("wait for git commit")? {
            return Ok(Waited::Exited(status));
        }
        if Instant::now() >= deadline {
            let _ = killpg(group, Signal::SIGKILL);
            let status = child.wait().context("reap git commit")?;
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
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
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

/// Writes `bytes` to `pipe` on a thread and closes it.
fn feed<W: Write + Send + 'static>(pipe: Option<W>, bytes: Vec<u8>) {
    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let _ = pipe.write_all(&bytes);
        }
    });
}

/// The error for a failed commit, from what git and its hooks printed.
fn failure(
    status: ExitStatus,
    stdout: &Receiver<Vec<u8>>,
    stderr: &Receiver<Vec<u8>>,
    group: Pid,
) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&collect(stderr, group)).into_owned();
    let stdout = String::from_utf8_lossy(&collect(stdout, group)).into_owned();
    match first_line(&stderr).or_else(|| last_line(&stdout)) {
        Some(line) => anyhow::anyhow!("{line}"),
        None => anyhow::anyhow!("git commit exited with {status}"),
    }
}

/// What a drained pipe carried, waiting for its end at most the grace;
/// past it, whatever a hook left holding the pipe is killed with the
/// group, and what arrived until then is used.
fn collect(rx: &Receiver<Vec<u8>>, group: Pid) -> Vec<u8> {
    rx.recv_timeout(OUTPUT_GRACE).unwrap_or_else(|_| {
        let _ = killpg(group, Signal::SIGKILL);
        rx.recv_timeout(Duration::from_millis(200))
            .unwrap_or_default()
    })
}

/// The first line with something on it, as the screen can show it.
fn first_line(text: &str) -> Option<String> {
    text.lines().map(super::clean).find(|line| !line.is_empty())
}

/// The last line with something on it, as the screen can show it.
fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(super::clean)
        .rfind(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{config, git, init};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    const QUICK: Duration = Duration::from_millis(300);

    fn stage_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).expect("write");
        git(dir, &["add", name]);
    }

    /// The last commit's message, without the newline git appends.
    fn last_message(dir: &Path) -> String {
        let out = crate::git::run(dir, ["log", "-1", "--format=%B"]).expect("log");
        String::from_utf8(out).expect("utf-8").trim_end().to_owned()
    }

    fn commits(dir: &Path) -> usize {
        crate::git::run(dir, ["rev-list", "--count", "HEAD"])
            .ok()
            .and_then(|out| String::from_utf8_lossy(&out).trim().parse().ok())
            .unwrap_or(0)
    }

    /// Installs a hook named `name` running `script` under `sh`.
    fn hook(dir: &Path, name: &str, script: &str) {
        let hooks = dir.join("hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        let path = hooks.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        config(dir, "core.hooksPath", "hooks");
    }

    #[test]
    fn message_puts_the_body_after_a_blank_line() {
        assert_eq!(message("feat: x", ""), "feat: x\n");
        assert_eq!(message("feat: x", "  \n"), "feat: x\n");
        assert_eq!(message("feat: x", "one\ntwo\n\n"), "feat: x\n\none\ntwo\n");
        assert_eq!(
            message("feat: x", "\n\n  indented"),
            "feat: x\n\n  indented\n"
        );
    }

    #[test]
    fn commits_what_is_staged_and_returns_the_short_hash() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        stage_file(dir.path(), "a.txt", "a\n");
        let hash = commit(
            dir.path(),
            &message("feat: first", "body\nmore"),
            QUICK * 20,
        )
        .expect("commits");
        assert_eq!(hash, head(dir.path()).expect("head"));
        assert!(hash.len() >= 7, "{hash}");
        assert_eq!(last_message(dir.path()), "feat: first\n\nbody\nmore");

        let error = commit(dir.path(), "again\n", QUICK * 20).expect_err("nothing staged");
        assert!(format!("{error:#}").contains("to commit"), "{error:#}");
        stage_file(dir.path(), "b.txt", "b\n");
        let error = commit(dir.path(), "", QUICK * 20).expect_err("empty message");
        assert!(
            format!("{error:#}").contains("empty commit message"),
            "{error:#}"
        );
        // A subject starting with # is a subject, not a comment.
        commit(dir.path(), "# not a comment\n", QUICK * 20).expect("commits");
        assert_eq!(last_message(dir.path()), "# not a comment");
    }

    #[test]
    fn hooks_can_rewrite_the_message_or_refuse_the_commit() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "commit-msg", "echo rewritten > \"$1\"");
        stage_file(dir.path(), "a.txt", "a\n");
        commit(dir.path(), "original\n", QUICK * 20).expect("commits");
        assert_eq!(last_message(dir.path()), "rewritten");

        // The first line the hook printed wins, whichever stream it used,
        // and the index is kept.
        hook(
            dir.path(),
            "pre-commit",
            "printf '\\033[31mhook says no\\033[0m\\n'\necho 'and more' >&2\nexit 1",
        );
        stage_file(dir.path(), "b.txt", "b\n");
        let error = commit(dir.path(), "subject\n", QUICK * 20).expect_err("hook fails");
        assert_eq!(error.to_string(), "hook says no");
        let staged =
            crate::git::run(dir.path(), ["diff", "--cached", "--name-only"]).expect("diff");
        assert_eq!(staged, b"b.txt\n");
    }

    #[test]
    fn a_hanging_hook_is_killed_after_the_timeout_and_the_repository_stays_usable() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "pre-commit", "sleep 30\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        let started = Instant::now();
        let error = commit(dir.path(), "subject\n", QUICK).expect_err("killed");
        assert!(started.elapsed() < Duration::from_secs(5), "did not hang");
        assert!(format!("{error:#}").contains("killed"), "{error:#}");
        assert_eq!(commits(dir.path()), 0, "nothing was committed");

        config(dir.path(), "core.hooksPath", "/dev/null");
        commit(dir.path(), "subject\n", QUICK * 20).expect("commits once the hook is gone");
        assert_eq!(commits(dir.path()), 1);
    }

    #[test]
    fn a_hook_child_left_behind_does_not_hold_the_commit() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "pre-commit", "sleep 3 &\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        let started = Instant::now();
        commit(dir.path(), "subject\n", QUICK * 20).expect("commits");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "returned without the child"
        );
    }

    #[test]
    fn a_long_message_behind_a_hanging_hook_is_cut_by_the_timeout() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "pre-commit", "sleep 3\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        let long = message("subject", &"x".repeat(200 * 1024));
        let started = Instant::now();
        let error = commit(dir.path(), &long, QUICK).expect_err("killed");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the write did not block"
        );
        assert!(format!("{error:#}").contains("killed"), "{error:#}");
    }

    #[test]
    fn a_commit_made_before_a_slow_post_commit_hook_is_killed_still_counts() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "post-commit", "sleep 3\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        let hash = commit(dir.path(), "subject\n", QUICK).expect("the commit was made");
        assert_eq!(hash, head(dir.path()).expect("head"));
        assert_eq!(commits(dir.path()), 1);
    }
}
