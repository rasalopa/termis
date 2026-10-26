//! Pushing the branch through the `git` binary.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::unistd::Pid;

use super::group::{self, Waited};
use super::status::Branch;

/// How long a push gets, hooks and the network included, before it is
/// killed.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The remote a branch without an upstream is pushed to.
pub const ORIGIN: &str = "origin";

/// The askpass program ssh gets when the user has none of their own: it
/// fails at once, so ssh gives up where it would prompt. Git asks it too
/// for a user name or a password without `GIT_ASKPASS` or
/// `core.askPass`, and fails there as it would without it.
const NO_ASKPASS: &str = "/usr/bin/false";

/// The variables that say how git reads pathspecs, which
/// [`super::git_in`] decides for what termis runs. `git push` reads
/// none, and its hooks get the caller's own, as from the shell.
const PATHSPEC_ENVIRONMENT: [&str; 4] = [
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// The starts of the lines that git and ssh print on a failed push that
/// say nothing of why: that some refs failed, that an askpass program
/// gave no answer, that a host key was added.
const NOT_WHY: [&str; 3] = [
    "error: failed to push some refs",
    "error: unable to read askpass response from ",
    "Warning: Permanently added ",
];

/// What a push does, as the branch's state asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Push {
    /// `git push`, the branch having an upstream, named as git status
    /// names it, `origin/main`; with the commits the branch has that the
    /// upstream lacks, when git counted them.
    Upstream {
        branch: String,
        upstream: String,
        ahead: Option<u32>,
    },
    /// `git push`, the branch having an upstream but going to `remote`,
    /// another remote, as `branch.<name>.pushRemote` or
    /// `remote.pushDefault` say; what it lacks is not counted.
    Remote { branch: String, remote: String },
    /// `git push -u origin <branch>`: the branch has no upstream, and
    /// tracks the one the push makes.
    Track { branch: String },
}

/// Why there is nothing to push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nothing {
    /// `HEAD` is detached, or cannot be read.
    Detached,
    /// The branch has no commit yet.
    Unborn,
    /// The branch has no commit its upstream lacks.
    UpToDate,
    /// The branch has no upstream, and the repository no remote named
    /// `origin`.
    NoOrigin,
}

/// What to push for `branch`, the repository having a remote named
/// `origin` when `origin`, and `git push` sending the branch to
/// `push_remote` when that is not its upstream's remote. A branch whose
/// upstream's ref is gone, whose distance git did not count, or that
/// goes to another remote, is pushed.
pub fn plan(branch: &Branch, origin: bool, push_remote: Option<&str>) -> Result<Push, Nothing> {
    if branch.oid.is_none() {
        return Err(Nothing::Unborn);
    }
    let head = branch.head.clone().ok_or(Nothing::Detached)?;
    match (&branch.upstream, push_remote, branch.ahead_behind) {
        (Some(_), Some(remote), _) => Ok(Push::Remote {
            branch: head,
            remote: remote.to_owned(),
        }),
        (Some(_), None, Some((0, _))) => Err(Nothing::UpToDate),
        (Some(upstream), None, ahead_behind) => Ok(Push::Upstream {
            branch: head,
            upstream: upstream.clone(),
            ahead: ahead_behind.map(|(ahead, _)| ahead),
        }),
        (None, ..) if origin => Ok(Push::Track { branch: head }),
        (None, ..) => Err(Nothing::NoOrigin),
    }
}

/// Whether the repository at `root` has a remote named `origin`.
pub fn has_origin(root: &Path) -> Result<bool> {
    let remotes = super::run(root, ["remote"]).context("git remote")?;
    Ok(String::from_utf8_lossy(&remotes)
        .lines()
        .any(|remote| remote == ORIGIN))
}

/// The remote `git push` sends `branch` of the repository at `root` to,
/// as `branch.<name>.pushRemote` or `remote.pushDefault` say, when it is
/// not the remote of the branch's upstream; none when it is, or when the
/// branch has no upstream.
pub fn push_remote(root: &Path, branch: &str) -> Result<Option<String>> {
    let format = "--format=%(upstream:remotename)%0a%(push:remotename)";
    let name = format!("refs/heads/{branch}");
    let out =
        super::run(root, ["for-each-ref", format, name.as_str()]).context("git for-each-ref")?;
    let out = String::from_utf8_lossy(&out);
    let mut lines = out.lines();
    let (upstream, push) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
    Ok((!upstream.is_empty() && !push.is_empty() && push != upstream).then(|| push.to_owned()))
}

/// Pushes as `push` says from the repository at `root`.
///
/// Git, ssh and the hooks run in their own process group with no input
/// and get `timeout`, after which the group is killed. Git cannot
/// prompt and ignores the caller's `GIT_DIR` and the like; the hooks
/// read pathspecs as they would from the shell. ssh asks the user's
/// `SSH_ASKPASS` for a passphrase or a host key, else a program that
/// fails, so it gives up instead of opening the terminal; a curses
/// pinentry for a signed push cannot take the screen since `GPG_TTY` is
/// withheld. A hook or an ssh command that opens the terminal itself
/// can still write to it, and one that reads it stops until the timeout
/// kills it. Pre-push hooks run, and nothing is forced. On failure the
/// error is the line [`reason`] picks, else the exit status.
pub fn push(root: &Path, push: &Push, timeout: Duration) -> Result<()> {
    run(command(root, push, |name| std::env::var_os(name)), timeout)
}

/// Pushes as [`push`] does, with [`TIMEOUT`], on a thread of its own,
/// which answers once on `tx`, wrapped by `tag` into the owner's message
/// type, and ends.
pub fn spawn<E>(
    root: PathBuf,
    push: Push,
    tx: Sender<E>,
    tag: impl FnOnce(Result<()>) -> E + Send + 'static,
) -> Result<()>
where
    E: Send + 'static,
{
    thread::Builder::new()
        .name("git-push".into())
        .spawn(move || {
            let result = self::push(&root, &push, TIMEOUT);
            let _ = tx.send(tag(result));
        })
        .context("spawn the push")?;
    Ok(())
}

/// The `git push` that does `push` from the repository at `root`,
/// `caller` giving the value of a variable in the caller's environment:
/// its `SSH_ASKPASS`, and the pathspec variables, which the hooks get.
fn command(root: &Path, push: &Push, caller: impl Fn(&str) -> Option<OsString>) -> Command {
    let mut command = super::git_in(root);
    command.args(["push", "--porcelain"]);
    if let Push::Track { branch } = push {
        // The ref's full name, which starts with neither `-` nor the `+`
        // that forces.
        command
            .args(["-u", ORIGIN])
            .arg(format!("refs/heads/{branch}"));
    }
    for variable in PATHSPEC_ENVIRONMENT {
        match caller(variable) {
            Some(value) => command.env(variable, value),
            None => command.env_remove(variable),
        };
    }
    let askpass = caller("SSH_ASKPASS")
        .filter(|askpass| !askpass.is_empty())
        .unwrap_or_else(|| NO_ASKPASS.into());
    command
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", askpass)
        .env_remove("GPG_TTY");
    command
}

/// Runs `command`, a push, in a process group of its own with no input,
/// until `timeout`, after which the group is killed.
fn run(mut command: Command, timeout: Duration) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("run git push")?;
    let group = group::of(&child)?;
    let deadline = Instant::now() + timeout;
    let stdout = group::drain(child.stdout.take());
    let stderr = group::drain(child.stderr.take());
    match group::wait(&mut child, deadline, group, "git push")? {
        Waited::Exited(status) if status.success() => Ok(()),
        Waited::Exited(status) => Err(failure(status, &stdout, &stderr, group)),
        Waited::Killed => bail!("git push took longer than {timeout:?} and was killed"),
    }
}

/// The error for a failed push, from what git, ssh and the hooks
/// printed.
fn failure(
    status: ExitStatus,
    stdout: &Receiver<Vec<u8>>,
    stderr: &Receiver<Vec<u8>>,
    group: Pid,
) -> anyhow::Error {
    let stdout = String::from_utf8_lossy(&group::collect(stdout, group)).into_owned();
    let stderr = String::from_utf8_lossy(&group::collect(stderr, group)).into_owned();
    match reason(&stdout, &stderr) {
        Some(line) => anyhow!("{line}"),
        None => anyhow!("git push exited with {status}"),
    }
}

/// The line that says why a push failed, as the screen can show it: a
/// ref the remote or git refused, as git prints it without
/// `--porcelain`, `! [rejected] main -> main (fetch first)`; else the
/// first line on `stderr`, which ssh, a hook or git printed, past those
/// that say nothing of why ([`NOT_WHY`]); else the first line on
/// `stdout` that is not the porcelain's, which a pre-push hook printed;
/// else the first line on `stderr` after all, such as git's note that
/// some refs failed when a hook that failed printed nothing. A hook that
/// passed may have printed on `stdout` too, so `stderr` comes first.
fn reason(stdout: &str, stderr: &str) -> Option<String> {
    let refused = stdout.lines().find_map(refused);
    let said = |past_not_why: bool| {
        stderr
            .lines()
            .map(super::clean)
            .filter(|line| !past_not_why || !NOT_WHY.iter().any(|start| line.starts_with(start)))
            .find(|line| !line.is_empty())
    };
    let printed = || {
        stdout
            .lines()
            .filter(|line| !porcelain(line))
            .map(super::clean)
            .find(|line| !line.is_empty())
    };
    refused
        .or_else(|| said(true))
        .or_else(printed)
        .or_else(|| said(false))
}

/// Whether `line` of `--porcelain`'s output is the porcelain's own: the
/// remote's URL, a ref's status, or the end.
fn porcelain(line: &str) -> bool {
    line.starts_with("To ") || line == "Done" || line.as_bytes().get(1) == Some(&b'\t')
}

/// The refused ref of a `--porcelain` status `line`, which is `!`, a
/// tab, `<from>:<to>`, a tab and `<summary> (<reason>)`, as git prints
/// it without `--porcelain`: `! <summary> <from> -> <to> (<reason>)`,
/// the refs named short. None for another line.
fn refused(line: &str) -> Option<String> {
    let (refs, summary) = line.strip_prefix("!\t")?.split_once('\t')?;
    let (from, to) = refs.split_once(':')?;
    let refs = if from.is_empty() {
        short(to).to_owned()
    } else {
        format!("{} -> {}", short(from), short(to))
    };
    let line = match summary.split_once(" (") {
        Some((summary, reason)) => format!("! {summary} {refs} ({reason}"),
        None => format!("! {summary} {refs}"),
    };
    Some(super::clean(&line))
}

/// `name` as git prints a ref: without `refs/heads/`, `refs/tags/` or
/// `refs/remotes/`.
fn short(name: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{config, git, hook, hooks_off, init};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use tempfile::TempDir;

    /// A timeout meant to fire while a hook sleeps.
    const QUICK: Duration = Duration::from_millis(300);
    /// A timeout meant never to fire: only a hang comes near it.
    const AMPLE: Duration = Duration::from_secs(60);
    /// How soon a push returns when it must not wait for what a hook
    /// runs for 30 s, with room for a loaded machine.
    const SOON: Duration = Duration::from_secs(10);

    fn branch(
        head: Option<&str>,
        oid: Option<&str>,
        upstream: Option<&str>,
        ahead_behind: Option<(u32, u32)>,
    ) -> Branch {
        Branch {
            head: head.map(str::to_owned),
            oid: oid.map(str::to_owned),
            upstream: upstream.map(str::to_owned),
            ahead_behind,
        }
    }

    /// A bare repository in `dir/remote.git`, on branch `main`, with the
    /// developer's hooks off.
    fn bare(dir: &Path) -> PathBuf {
        git(dir, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        let remote = dir.join("remote.git");
        config(&remote, "core.hooksPath", "/dev/null");
        hooks_off(&remote);
        remote
    }

    /// A repository in `dir/work` with one commit on `main` and `remote`
    /// as `origin`, which `main` does not track yet; a push from it does
    /// what `git push` does by default, unsigned.
    fn work(dir: &Path, remote: &Path) -> PathBuf {
        let work = dir.join("work");
        std::fs::create_dir(&work).expect("mkdir");
        init(&work);
        config(&work, "push.default", "simple");
        config(&work, "push.gpgSign", "false");
        commit(&work, "first");
        let url = remote.to_str().expect("utf-8");
        git(&work, &["remote", "add", ORIGIN, url]);
        work
    }

    fn commit(dir: &Path, subject: &str) {
        git(dir, &["commit", "-q", "--allow-empty", "-m", subject]);
    }

    /// The commit `name` names in the repository at `dir`.
    fn rev(dir: &Path, name: &str) -> String {
        let out = crate::git::run(dir, ["rev-parse", name]).expect("rev-parse");
        String::from_utf8(out).expect("utf-8").trim().to_owned()
    }

    fn upstream(branch: &str) -> Push {
        Push::Upstream {
            branch: branch.into(),
            upstream: format!("origin/{branch}"),
            ahead: Some(1),
        }
    }

    #[test]
    fn the_branch_says_what_to_push() {
        let oid = Some("abc");
        let up = Some("origin/main");
        assert_eq!(
            plan(&branch(Some("main"), None, None, None), true, None),
            Err(Nothing::Unborn)
        );
        assert_eq!(
            plan(&branch(None, oid, None, None), true, None),
            Err(Nothing::Detached)
        );
        assert_eq!(
            plan(&branch(Some("main"), oid, up, Some((0, 3))), true, None),
            Err(Nothing::UpToDate),
            "behind only"
        );
        assert_eq!(
            plan(&branch(Some("main"), oid, up, Some((2, 1))), false, None),
            Ok(Push::Upstream {
                branch: "main".into(),
                upstream: "origin/main".into(),
                ahead: Some(2),
            }),
            "ahead and behind at once still pushes"
        );
        assert_eq!(
            plan(&branch(Some("main"), oid, up, None), false, None),
            Ok(Push::Upstream {
                branch: "main".into(),
                upstream: "origin/main".into(),
                ahead: None,
            }),
            "an upstream whose ref is gone"
        );
        let fork = Ok(Push::Remote {
            branch: "main".into(),
            remote: "fork".into(),
        });
        assert_eq!(
            plan(
                &branch(Some("main"), oid, up, Some((0, 3))),
                false,
                Some("fork")
            ),
            fork,
            "the upstream has every commit, the push remote may not"
        );
        assert_eq!(
            plan(
                &branch(Some("main"), oid, up, Some((2, 0))),
                false,
                Some("fork")
            ),
            fork
        );
        assert_eq!(
            plan(&branch(Some("topic"), oid, None, None), true, None),
            Ok(Push::Track {
                branch: "topic".into()
            })
        );
        assert_eq!(
            plan(&branch(Some("topic"), oid, None, None), false, None),
            Err(Nothing::NoOrigin)
        );
    }

    #[test]
    fn pushes_to_the_upstream_and_answers_from_a_thread() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        git(&work, &["push", "-q", "-u", ORIGIN, "main"]);
        commit(&work, "second");
        push(&work, &upstream("main"), AMPLE).expect("pushes");
        assert_eq!(rev(&remote, "main"), rev(&work, "HEAD"));

        commit(&work, "third");
        let (tx, rx) = mpsc::channel();
        spawn(work.clone(), upstream("main"), tx, |result| {
            ("tagged", result)
        })
        .expect("spawns");
        let (tag, result) = rx.recv_timeout(AMPLE).expect("an answer");
        assert_eq!(tag, "tagged");
        result.expect("pushes");
        assert_eq!(rev(&remote, "main"), rev(&work, "HEAD"));
    }

    #[test]
    fn a_branch_without_an_upstream_goes_to_origin_and_tracks_it() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        assert!(has_origin(&work).expect("git remote"));
        let track = Push::Track {
            branch: "main".into(),
        };
        push(&work, &track, AMPLE).expect("pushes");
        assert_eq!(rev(&remote, "main"), rev(&work, "HEAD"));
        let tracked = crate::git::run(&work, ["rev-parse", "--abbrev-ref", "main@{upstream}"])
            .expect("an upstream");
        assert_eq!(String::from_utf8_lossy(&tracked).trim(), "origin/main");
    }

    #[test]
    fn a_branch_named_with_a_plus_goes_as_itself_and_forces_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        // The remote's `foo` has a commit the local `foo` lacks; a refspec
        // `+foo` would force the local one over it.
        git(&work, &["branch", "foo"]);
        commit(&work, "theirs");
        git(&work, &["push", "-q", ORIGIN, "main:foo"]);
        let theirs = rev(&remote, "refs/heads/foo");
        git(&work, &["checkout", "-q", "-b", "+foo", "foo"]);
        let track = Push::Track {
            branch: "+foo".into(),
        };
        push(&work, &track, AMPLE).expect("pushes");
        assert_eq!(rev(&remote, "refs/heads/foo"), theirs, "nothing forced");
        assert_eq!(rev(&remote, "refs/heads/+foo"), rev(&work, "HEAD"));
        let tracked = crate::git::run(&work, ["rev-parse", "--abbrev-ref", "+foo@{upstream}"])
            .expect("an upstream");
        assert_eq!(String::from_utf8_lossy(&tracked).trim(), "origin/+foo");
    }

    #[test]
    fn the_push_remote_counts_when_it_is_not_the_upstreams() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        assert_eq!(
            push_remote(&work, "main").expect("reads"),
            None,
            "no upstream"
        );
        git(&work, &["push", "-q", "-u", ORIGIN, "main"]);
        assert_eq!(push_remote(&work, "main").expect("reads"), None);
        git(&work, &["remote", "add", "fork", "/nowhere.git"]);
        config(&work, "remote.pushDefault", "fork");
        assert_eq!(
            push_remote(&work, "main").expect("reads").as_deref(),
            Some("fork")
        );
        config(&work, "branch.main.pushRemote", ORIGIN);
        assert_eq!(
            push_remote(&work, "main").expect("reads"),
            None,
            "the branch's own"
        );
        assert_eq!(
            push_remote(&work, "gone").expect("reads"),
            None,
            "no such branch"
        );
    }

    #[test]
    fn without_a_remote_named_origin_there_is_nowhere_to_track() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        git(&work, &["remote", "rename", ORIGIN, "upstream"]);
        assert!(!has_origin(&work).expect("git remote"));
        git(&work, &["remote", "remove", "upstream"]);
        assert!(!has_origin(&work).expect("git remote"), "no remote at all");
        // Pushed anyway, git says why.
        let track = Push::Track {
            branch: "main".into(),
        };
        let error = push(&work, &track, AMPLE).expect_err("no origin");
        assert!(format!("{error:#}").contains("origin"), "{error:#}");
        let outside = TempDir::new().expect("temp dir");
        assert!(has_origin(outside.path()).is_err(), "not a repository");
    }

    #[test]
    fn a_rejected_push_names_the_ref_and_the_reason() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        git(&work, &["push", "-q", "-u", ORIGIN, "main"]);
        let url = remote.to_str().expect("utf-8");
        git(dir.path(), &["clone", "-q", url, "other"]);
        let other = dir.path().join("other");
        commit(&other, "theirs");
        git(&other, &["push", "-q"]);
        commit(&work, "ours");
        let error = push(&work, &upstream("main"), AMPLE).expect_err("rejected");
        assert_eq!(error.to_string(), "! [rejected] main -> main (fetch first)");
        git(&work, &["fetch", "-q"]);
        let error = push(&work, &upstream("main"), AMPLE).expect_err("rejected");
        assert_eq!(
            error.to_string(),
            "! [rejected] main -> main (non-fast-forward)"
        );
        assert_eq!(rev(&remote, "main"), rev(&other, "HEAD"), "nothing forced");
    }

    #[test]
    fn a_pre_push_hook_that_fails_gives_its_first_line() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        hook(&work, "pre-push", "echo 'tests fail'\necho more\nexit 1");
        let track = Push::Track {
            branch: "main".into(),
        };
        let error = push(&work, &track, AMPLE).expect_err("the hook says no");
        assert_eq!(error.to_string(), "tests fail");
        hook(&work, "pre-push", "echo 'on stderr' >&2\nexit 1");
        let error = push(&work, &track, AMPLE).expect_err("the hook says no");
        assert_eq!(error.to_string(), "on stderr");
        assert!(
            crate::git::run(&remote, ["rev-parse", "--verify", "-q", "main"]).is_err(),
            "nothing pushed"
        );
    }

    #[test]
    fn what_a_pre_push_hook_that_passed_printed_is_not_why_the_push_failed() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        hook(&work, "pre-push", "echo 'lint ok'\nexit 0");
        // The remote hangs up: its receive-pack is killed.
        hook(&remote, "pre-receive", "kill -9 $PPID");
        let track = Push::Track {
            branch: "main".into(),
        };
        let error = push(&work, &track, AMPLE).expect_err("the remote hangs up");
        let error = error.to_string();
        assert!(
            error.contains("hung up") || error.contains("disconnect"),
            "{error}"
        );
    }

    #[test]
    fn a_hanging_hook_is_killed_after_the_timeout() {
        let dir = TempDir::new().expect("temp dir");
        let remote = bare(dir.path());
        let work = work(dir.path(), &remote);
        hook(&work, "pre-push", "sleep 30\nexit 0");
        let track = Push::Track {
            branch: "main".into(),
        };
        let started = Instant::now();
        let error = push(&work, &track, QUICK).expect_err("killed");
        assert!(started.elapsed() < SOON, "did not hang");
        assert!(format!("{error:#}").contains("killed"), "{error:#}");
        assert!(
            crate::git::run(&remote, ["rev-parse", "--verify", "-q", "main"]).is_err(),
            "nothing pushed"
        );
    }

    #[test]
    fn ssh_gets_an_askpass_that_fails_and_no_terminal() {
        let dir = TempDir::new().expect("temp dir");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).expect("mkdir");
        init(&work);
        commit(&work, "first");
        git(&work, &["remote", "add", ORIGIN, "ssh://fakehost/x.git"]);
        // A fake ssh, so named that git takes it for OpenSSH, which
        // writes down what it got and fails.
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("mkdir");
        let ssh = bin.join("ssh");
        let script = format!(
            "#!/bin/sh\nenv > '{0}/env'\nprintf '%s\\n' \"$@\" > '{0}/argv'\n\
             echo 'fake ssh: no way through' >&2\nexit 255\n",
            dir.path().display()
        );
        std::fs::write(&ssh, script).expect("write");
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let track = Push::Track {
            branch: "main".into(),
        };
        let mut command = command(&work, &track, |_| None);
        // Only GIT_SSH picks the fake: the developer's ssh settings are
        // kept out.
        command
            .env("GIT_SSH", &ssh)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_SSH_COMMAND");
        let error = run(command, AMPLE).expect_err("the fake fails");
        assert_eq!(error.to_string(), "fake ssh: no way through");
        let env = std::fs::read_to_string(dir.path().join("env")).expect("ssh ran");
        for set in [
            "SSH_ASKPASS_REQUIRE=force",
            "SSH_ASKPASS=/usr/bin/false",
            "GIT_TERMINAL_PROMPT=0",
        ] {
            assert!(env.lines().any(|line| line == set), "{set} in\n{env}");
        }
        // Nor the pathspec variable termis sets for itself.
        for unset in ["GPG_TTY=", "GIT_LITERAL_PATHSPECS="] {
            assert!(!env.lines().any(|line| line.starts_with(unset)), "{env}");
        }
        let argv = std::fs::read_to_string(dir.path().join("argv")).expect("ssh ran");
        assert!(argv.lines().any(|arg| arg == "fakehost"), "{argv}");

        // The user's own askpass and pathspec variables are kept; an
        // empty askpass is none.
        let caller = |name: &str| match name {
            "SSH_ASKPASS" => Some("/my/askpass".into()),
            "GIT_ICASE_PATHSPECS" => Some("1".into()),
            _ => None,
        };
        let command = super::command(&work, &track, caller);
        let set = |command: &Command, name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(set(&command, "SSH_ASKPASS").as_deref(), Some("/my/askpass"));
        assert_eq!(
            set(&command, "SSH_ASKPASS_REQUIRE").as_deref(),
            Some("force")
        );
        assert_eq!(set(&command, "GIT_ICASE_PATHSPECS").as_deref(), Some("1"));
        for withheld in ["GPG_TTY", "GIT_LITERAL_PATHSPECS"] {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == withheld && value.is_none()),
                "{withheld} is withheld"
            );
        }
        let empty = |name: &str| (name == "SSH_ASKPASS").then(OsString::new);
        let command = super::command(&work, &track, empty);
        assert_eq!(set(&command, "SSH_ASKPASS").as_deref(), Some(NO_ASKPASS));
        let args = |command: &Command| -> Vec<String> {
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(&command),
            ["push", "--porcelain", "-u", ORIGIN, "refs/heads/main"]
        );
        let plain = super::command(&work, &upstream("main"), |_| None);
        assert_eq!(
            args(&plain),
            ["push", "--porcelain"],
            "no force, no refspec"
        );
    }

    #[test]
    fn nothing_is_read_from_stdin() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "if read -r line; then echo \"read $line\" >&2; else echo 'end of input' >&2; fi\nexit 1",
        ]);
        let started = Instant::now();
        let error = run(command, AMPLE).expect_err("fails");
        assert_eq!(error.to_string(), "end of input");
        assert!(started.elapsed() < SOON, "did not wait for input");
    }

    #[test]
    fn the_reason_is_the_refused_ref_else_what_a_hook_or_ssh_printed() {
        let refused = "To /tmp/remote.git\n\
                       =\trefs/heads/a:refs/heads/a\t[up to date]\n\
                       !\trefs/heads/main:refs/heads/main\t[remote rejected] (pre-receive hook declined)\n\
                       Done\n";
        let failed = "error: failed to push some refs to '/tmp/remote.git'\n";
        assert_eq!(
            reason(refused, &format!("remote: no\n{failed}")).as_deref(),
            Some("! [remote rejected] main -> main (pre-receive hook declined)")
        );
        assert_eq!(
            reason("!\t:refs/tags/v1\t[rejected]\n", "").as_deref(),
            Some("! [rejected] v1"),
            "a deletion has no source"
        );
        assert_eq!(
            reason("\n\x1b[31mtests fail\x1b[0m\n", failed).as_deref(),
            Some("tests fail"),
            "a hook that failed, past git's note"
        );
        assert_eq!(
            reason(
                "running checks\n",
                "fatal: the remote end hung up unexpectedly\n"
            )
            .as_deref(),
            Some("fatal: the remote end hung up unexpectedly"),
            "not what a hook that passed printed"
        );
        assert_eq!(
            reason(
                "",
                "Warning: Permanently added 'example.com' (ED25519) to the list of known hosts.\n\
                 git@example.com: Permission denied (publickey).\n"
            )
            .as_deref(),
            Some("git@example.com: Permission denied (publickey).")
        );
        assert_eq!(
            reason(
                "",
                "error: unable to read askpass response from '/usr/bin/false'\n\
                 fatal: could not read Username for 'https://example.com': terminal prompts disabled\n"
            )
            .as_deref(),
            Some(
                "fatal: could not read Username for 'https://example.com': terminal prompts disabled"
            )
        );
        assert_eq!(
            reason(
                "",
                "Host key verification failed.\nfatal: Could not read from remote repository.\n"
            )
            .as_deref(),
            Some("Host key verification failed.")
        );
        assert_eq!(
            reason("To x\nDone\n", failed).as_deref(),
            Some("error: failed to push some refs to '/tmp/remote.git'"),
            "a hook that failed printed nothing"
        );
        assert_eq!(reason("To x\nDone\n", "\n"), None);
        assert_eq!(short("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short("HEAD"), "HEAD");
    }
}
