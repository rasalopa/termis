//! Committing through the `git` binary.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::unistd::Pid;

use super::group::{self, Waited};

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
/// cannot prompt, ignores the caller's `GIT_DIR` and the like, and a
/// curses pinentry cannot take the screen since `GPG_TTY` is withheld; a
/// hook that reads the terminal stops until the timeout kills it. On
/// failure the error is the first line git or a hook printed (hooks'
/// stdout arrives on stderr too), else the last line of git's stdout,
/// else the exit status.
pub fn commit(root: &Path, message: &str, timeout: Duration) -> Result<String> {
    let before = head(root).ok();
    let mut child = super::git_in(root)
        .args(["-c", "advice.statusHints=false", "commit", "-q", "-F", "-"])
        .env_remove("GPG_TTY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("run git commit")?;
    let group = group::of(&child)?;
    let deadline = Instant::now() + timeout;
    let stdout = group::drain(child.stdout.take());
    let stderr = group::drain(child.stderr.take());
    // Written on its own thread: git runs the pre-commit hook before it
    // reads the message, and a long message would block a hook away.
    // A write that fails shows as git's own failure.
    feed(child.stdin.take(), message.as_bytes().to_vec());
    match group::wait(&mut child, deadline, group, "git commit")? {
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
    let stderr = String::from_utf8_lossy(&group::collect(stderr, group)).into_owned();
    let stdout = String::from_utf8_lossy(&group::collect(stdout, group)).into_owned();
    match first_line(&stderr).or_else(|| last_line(&stdout)) {
        Some(line) => anyhow::anyhow!("{line}"),
        None => anyhow::anyhow!("git commit exited with {status}"),
    }
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
    use nix::sys::signal::{Signal, kill};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::TempDir;

    /// A timeout meant to fire while a hook sleeps.
    const QUICK: Duration = Duration::from_millis(300);
    /// A timeout meant never to fire: only a hang comes near it.
    const AMPLE: Duration = Duration::from_secs(60);
    /// How soon a commit returns when it must not wait for what a hook
    /// runs for 30 s, with room for a loaded machine.
    const SOON: Duration = Duration::from_secs(10);

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
        let hash =
            commit(dir.path(), &message("feat: first", "body\nmore"), AMPLE).expect("commits");
        assert_eq!(hash, head(dir.path()).expect("head"));
        assert!(hash.len() >= 7, "{hash}");
        assert_eq!(last_message(dir.path()), "feat: first\n\nbody\nmore");

        let error = commit(dir.path(), "again\n", AMPLE).expect_err("nothing staged");
        assert!(format!("{error:#}").contains("to commit"), "{error:#}");
        stage_file(dir.path(), "b.txt", "b\n");
        let error = commit(dir.path(), "", AMPLE).expect_err("empty message");
        assert!(
            format!("{error:#}").contains("empty commit message"),
            "{error:#}"
        );
        // A subject starting with # is a subject, not a comment.
        commit(dir.path(), "# not a comment\n", AMPLE).expect("commits");
        assert_eq!(last_message(dir.path()), "# not a comment");
    }

    #[test]
    fn hooks_can_rewrite_the_message_or_refuse_the_commit() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "commit-msg", "echo rewritten > \"$1\"");
        stage_file(dir.path(), "a.txt", "a\n");
        commit(dir.path(), "original\n", AMPLE).expect("commits");
        assert_eq!(last_message(dir.path()), "rewritten");

        // The first line the hook printed wins, whichever stream it used,
        // and the index is kept.
        hook(
            dir.path(),
            "pre-commit",
            "printf '\\033[31mhook says no\\033[0m\\n'\necho 'and more' >&2\nexit 1",
        );
        stage_file(dir.path(), "b.txt", "b\n");
        let error = commit(dir.path(), "subject\n", AMPLE).expect_err("hook fails");
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
        assert!(started.elapsed() < SOON, "did not hang");
        assert!(format!("{error:#}").contains("killed"), "{error:#}");
        assert_eq!(commits(dir.path()), 0, "nothing was committed");

        config(dir.path(), "core.hooksPath", "/dev/null");
        commit(dir.path(), "subject\n", AMPLE).expect("commits once the hook is gone");
        assert_eq!(commits(dir.path()), 1);
    }

    #[test]
    fn a_hook_child_left_behind_does_not_hold_the_commit() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let left = dir.path().join(".git/left.pid");
        let script = format!("sleep 30 &\necho $! > '{}'\nexit 0", left.display());
        hook(dir.path(), "pre-commit", &script);
        stage_file(dir.path(), "a.txt", "a\n");
        let started = Instant::now();
        commit(dir.path(), "subject\n", AMPLE).expect("commits");
        let elapsed = started.elapsed();
        // The child, when it is still the sleep the hook left, goes now.
        let pid: i32 = std::fs::read_to_string(&left)
            .expect("the hook wrote its child")
            .trim()
            .parse()
            .expect("a pid");
        let ps = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let child = String::from_utf8_lossy(&ps.stdout).trim().to_owned();
        if child == "sleep 30" {
            kill(Pid::from_raw(pid), Signal::SIGKILL).expect("kill the child");
        }
        assert!(elapsed < SOON, "returned without the child: {elapsed:?}");
        assert_eq!(child, "sleep 30", "the child outlives the commit");
    }

    #[test]
    fn a_long_message_behind_a_hanging_hook_is_cut_by_the_timeout() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "pre-commit", "sleep 30\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        let long = message("subject", &"x".repeat(200 * 1024));
        let started = Instant::now();
        let error = commit(dir.path(), &long, QUICK).expect_err("killed");
        assert!(started.elapsed() < SOON, "the write did not block");
        assert!(format!("{error:#}").contains("killed"), "{error:#}");
    }

    #[test]
    fn a_commit_made_before_a_slow_post_commit_hook_is_killed_still_counts() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        hook(dir.path(), "post-commit", "sleep 30\nexit 0");
        stage_file(dir.path(), "a.txt", "a\n");
        // Time for git to make the commit on a loaded machine; the hook
        // still sleeps when it runs out.
        let made_by = Duration::from_secs(5);
        let hash = commit(dir.path(), "subject\n", made_by).expect("the commit was made");
        assert_eq!(hash, head(dir.path()).expect("head"));
        assert_eq!(commits(dir.path()), 1);
    }
}
