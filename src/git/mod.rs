//! Git, through the `git` binary (ADR-0001).

use std::ffi::OsStr;
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, killpg};

pub mod commit;
pub mod diff;
pub mod files;
pub mod find;
pub(crate) mod group;
pub mod last;
pub mod numstat;
pub mod push;
pub mod stage;
pub mod staged;
pub mod status;
pub mod today;
pub mod watch;
pub mod worker;

/// Variables that would point git somewhere else than the directory it
/// runs in, or read the paths it is given as patterns.
const CALLER_ENVIRONMENT: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// A `git` command for the repository containing `dir`: it runs there,
/// cannot prompt, takes paths as written, and ignores the variables that
/// would redirect it. Everything else in the environment reaches git as
/// it would from the same shell, configuration included.
pub(crate) fn git_in(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1");
    for variable in CALLER_ENVIRONMENT {
        command.env_remove(variable);
    }
    command
}

/// The root of the working tree that contains `dir`, or `None` when `dir`
/// is not inside one. Fails only when `git` cannot be run.
pub fn repo_root(dir: &Path) -> Result<Option<PathBuf>> {
    let output = git_in(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| not_run(dir, error))?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8(output.stdout).context("git printed non-UTF-8")?;
    let root = root.trim_end_matches(['\n', '\r']);
    Ok((!root.is_empty()).then(|| PathBuf::from(root)))
}

/// Runs `git` with `args` in `dir` and returns what it printed. Paths in
/// `args` are relative to `dir`. When git fails, the error carries the
/// first non-empty line of its stderr, or its exit status when it printed
/// nothing. Git never gets a terminal to prompt on.
pub(crate) fn run<I, S>(dir: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_accepting(dir, args, &[])
}

/// Like [`run`], but the exit codes in `accepted` count as success too.
pub(crate) fn run_accepting<I, S>(dir: &Path, args: I, accepted: &[i32]) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_in(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| not_run(dir, error))?;
    let accepted_code = output
        .status
        .code()
        .is_some_and(|code| accepted.contains(&code));
    if output.status.success() || accepted_code {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match stderr.lines().map(clean).find(|line| !line.is_empty()) {
        Some(line) => bail!("{line}"),
        None => bail!("git exited with {}", output.status),
    }
}

/// Like [`run_accepting`], but reads what git prints up to `budget`
/// bytes, cut after the last whole line, and says whether it printed
/// more. Past the budget git is not read further: its process group is
/// killed, with whatever it started.
pub(crate) fn run_up_to<I, S>(
    dir: &Path,
    args: I,
    accepted: &[i32],
    budget: usize,
) -> Result<(Vec<u8>, bool)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = git_in(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| not_run(dir, error))?;
    let group = group::of(&child)?;
    let stderr = group::drain(child.stderr.take());
    let limit = u64::try_from(budget).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(limit)
            .read_to_end(&mut bytes)
            .context("read what git printed")?;
    }
    if bytes.len() > budget {
        // Killed before it is reaped, so the group's id is still its own.
        let _ = killpg(group, Signal::SIGKILL);
        let _ = child.wait();
        bytes.truncate(budget);
        let end = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at + 1);
        bytes.truncate(end);
        return Ok((bytes, true));
    }
    let status = child.wait().context("wait for git")?;
    let accepted_code = status.code().is_some_and(|code| accepted.contains(&code));
    if status.success() || accepted_code {
        return Ok((bytes, false));
    }
    let stderr = String::from_utf8_lossy(&group::collect(&stderr, group)).into_owned();
    match group::first_line(&stderr) {
        Some(line) => bail!("{line}"),
        None => bail!("git exited with {status}"),
    }
}

/// Fails when `git diff --no-index` would not read the untracked `path`,
/// relative to `root`, at once. Git diffs a link as the link it is,
/// unless a pipe or a directory is at its end: it reads the pipe,
/// waiting for a writer that may never come, and looks for a file inside
/// the directory. A directory, such as an embedded repository, fails as
/// one; a pipe, or a link to a pipe or to a directory, fails as not a
/// regular file.
pub(crate) fn check_untracked(root: &Path, path: &Path) -> Result<()> {
    let full = root.join(path);
    let metadata =
        std::fs::symlink_metadata(&full).with_context(|| format!("read {}", path.display()))?;
    if metadata.is_dir() {
        bail!("{} is a directory", path.display());
    }
    // Following a link to nothing or a loop of links fails; the link goes
    // to git as it is.
    if std::fs::metadata(&full).is_ok_and(|end| end.is_dir() || end.file_type().is_fifo()) {
        bail!("{} is not a regular file", path.display());
    }
    Ok(())
}

/// Why `git` could not be started in `dir`: that it is not installed
/// when `dir` exists and git was not found, else `error` with `dir`
/// named.
fn not_run(dir: &Path, error: io::Error) -> anyhow::Error {
    match error.kind() {
        ErrorKind::NotFound if dir.is_dir() => anyhow!("git is not installed"),
        _ => anyhow!(error).context(format!("run git in {}", dir.display())),
    }
}

/// `line` as the screen can show it: CSI escape sequences (colours) and
/// other control characters removed, the ends trimmed.
pub(crate) fn clean(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next_if_eq(&'[').is_some() {
                // Parameters and intermediates, then one final byte.
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// The untracked paths [`odd_paths`] makes that git looks through:
    /// links to a pipe and to a directory.
    pub(crate) const REFUSED: [&str; 2] = ["to-fifo", "to-dir"];

    /// How long a channel stays quiet before nothing more is expected on
    /// it: FSEvents reports a write within about 140 ms.
    pub(crate) const QUIET: Duration = Duration::from_millis(300);

    /// Runs `git` in `dir` with the developer's configuration and git
    /// environment kept out and a fixed identity.
    pub(crate) fn git(dir: &Path, args: &[&str]) {
        git_with(dir, args, &[]);
    }

    /// Like [`git`], with `envs` set on top: a date, another author.
    /// The dates of the caller's environment are kept out.
    pub(crate) fn git_with(dir: &Path, args: &[&str], envs: &[(&str, &str)]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE")
            .envs(envs.iter().copied())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} in {}", dir.display());
    }

    /// Sets one local configuration value in the repository at `dir`.
    pub(crate) fn config(dir: &Path, key: &str, value: &str) {
        git(dir, &["config", key, value]);
    }

    /// Installs, in the repository at `dir`, a hook named `name` running
    /// `script` under `sh`.
    pub(crate) fn hook(dir: &Path, name: &str, script: &str) {
        let hooks = dir.join("hooks");
        std::fs::create_dir_all(&hooks).expect("mkdir");
        let path = hooks.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        config(dir, "core.hooksPath", "hooks");
    }

    /// Creates a repository in `dir` on branch `main`, with the local
    /// configuration that keeps the developer's excludes, hooks, signing,
    /// file monitor, rename detection, hash length and identity out of
    /// what [`run`] sees there; [`hooks_off`] turns off the hooks
    /// defined by name.
    pub(crate) fn init(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("core.excludesFile", "/dev/null"),
            ("core.fsmonitor", "false"),
            ("core.hooksPath", "/dev/null"),
            ("commit.gpgsign", "false"),
            ("diff.renames", "true"),
            ("status.renames", "true"),
            ("diff.context", "3"),
            ("core.abbrev", "7"),
            ("user.name", "t"),
            ("user.email", "t@example.com"),
        ] {
            config(dir, key, value);
        }
        hooks_off(dir);
    }

    /// Turns off, in the repository at `dir`, each hook that the
    /// configuration [`run`] reads there defines by name with
    /// `hook.<name>.command`; `core.hooksPath` leaves those on.
    pub(crate) fn hooks_off(dir: &Path) {
        let keys = run_accepting(
            dir,
            [
                "config",
                "--name-only",
                "--get-regexp",
                r"^hook\..*\.command$",
            ],
            &[1],
        )
        .expect("git config");
        let keys = String::from_utf8(keys).expect("utf-8");
        for name in keys
            .lines()
            .filter_map(|key| key.strip_prefix("hook.")?.strip_suffix(".command"))
        {
            config(dir, &format!("hook.{name}.enabled"), "false");
        }
    }

    /// Takes what reaches `rx` until it stays [`QUIET`]: one write
    /// reaches a watch two to four times.
    pub(crate) fn drain<T>(rx: &mpsc::Receiver<T>) {
        while rx.recv_timeout(QUIET).is_ok() {}
    }

    /// Moves the modification time of the file at `path` 10 s ahead: its
    /// content stays, and the index's record of it no longer matches.
    pub(crate) fn stat_dirty(path: &Path) {
        let ahead = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .expect("mtime")
            + Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(ahead)
            .expect("set mtime");
    }

    /// Makes in `dir` a pipe, `fifo`, which git does not list, and the
    /// links in [`REFUSED`]; beside them a file of two lines,
    /// `file.txt`, one of one line, `dir/f.txt`, a link to the first,
    /// `to-file`, a link to a device, `to-null`, and a link to nothing,
    /// `broken`.
    pub(crate) fn odd_paths(dir: &Path) {
        let made = Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo");
        std::fs::create_dir(dir.join("dir")).expect("mkdir");
        std::fs::write(dir.join("dir/f.txt"), "f\n").expect("write");
        std::fs::write(dir.join("file.txt"), "a\nb\n").expect("write");
        for (link, target) in [
            ("to-fifo", "fifo"),
            ("to-dir", "dir"),
            ("to-null", "/dev/null"),
            ("to-file", "file.txt"),
            ("broken", "nowhere"),
        ] {
            symlink(target, dir.join(link)).expect("link");
        }
    }

    /// What `call` returns, from a thread of its own, so that a call that
    /// waits fails the test instead of hanging it. Before the test fails,
    /// a git left reading the pipe [`odd_paths`] made in `dir` is let go.
    pub(crate) fn at_once<T>(dir: &Path, call: impl FnOnce() -> T + Send + 'static) -> T
    where
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(call());
        });
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| {
                // Opened for writing and closed, the pipe ends its reader's
                // read; with no reader, the open fails at once.
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(dir.join("fifo"));
                panic!("no answer: {error}");
            })
    }

    #[test]
    fn finds_the_root_from_a_subdirectory() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let root = repo_root(&nested)
            .expect("git runs")
            .expect("inside a repo");
        assert_eq!(
            root.canonicalize().expect("exists"),
            dir.path().canonicalize().expect("exists")
        );
    }

    #[test]
    fn reports_none_outside_a_repository() {
        let dir = TempDir::new().expect("temp dir");
        // A temp dir is not inside a repository unless the system temp dir
        // is, which would be unusual.
        let root = repo_root(dir.path()).expect("git runs");
        assert!(root.is_none() || !root.unwrap().starts_with(dir.path()));
    }

    #[test]
    fn clean_strips_colours_and_controls() {
        assert_eq!(clean("\x1b[31merror\x1b[0m: x\t"), "error: x");
        assert_eq!(clean("  plain  "), "plain");
        assert_eq!(clean("\x1b[1;32mok\x1b[m"), "ok");
        assert_eq!(clean("a\x1bMb"), "ab", "a two-byte escape");
        assert_eq!(clean("\x07\x1b["), "", "nothing printable");
    }

    #[test]
    fn commands_ignore_the_variables_that_would_redirect_git() {
        let command = git_in(Path::new("."));
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        for variable in CALLER_ENVIRONMENT {
            assert!(removed.contains(&variable.to_owned()), "{variable} stays");
        }
        let set = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(set("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(set("GIT_LITERAL_PATHSPECS").as_deref(), Some("1"));
        assert_eq!(command.get_current_dir(), Some(Path::new(".")));
    }

    #[test]
    fn hooks_set_by_name_in_the_configuration_are_turned_off() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        config(dir.path(), "hook.a.b.event", "pre-commit");
        config(dir.path(), "hook.a.b.command", "exit 1");
        hooks_off(dir.path());
        let enabled = run(dir.path(), ["config", "hook.a.b.enabled"]).expect("set");
        assert_eq!(String::from_utf8_lossy(&enabled), "false\n");
    }

    #[test]
    fn untracked_paths_are_checked_at_the_end_of_their_links() {
        let dir = TempDir::new().expect("temp dir");
        odd_paths(dir.path());
        for path in ["file.txt", "dir/f.txt", "to-file", "to-null", "broken"] {
            check_untracked(dir.path(), Path::new(path))
                .unwrap_or_else(|error| panic!("{path}: {error:#}"));
        }
        let refused = |path: &str| {
            let error = check_untracked(dir.path(), Path::new(path)).expect_err(path);
            format!("{error:#}")
        };
        assert_eq!(refused("dir"), "dir is a directory");
        assert_eq!(refused("fifo"), "fifo is not a regular file");
        for path in REFUSED {
            assert_eq!(refused(path), format!("{path} is not a regular file"));
        }
        assert!(refused("missing").starts_with("read missing: "));
    }

    #[test]
    fn git_is_read_up_to_a_budget_and_stopped_past_it() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let long: String = (0..2000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("long.txt"), &long).expect("write");
        git(dir.path(), &["add", "long.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "long"]);
        let show = ["show", "HEAD:long.txt"];
        let (whole, cut) = run_up_to(dir.path(), show, &[], usize::MAX).expect("reads");
        assert_eq!((whole.as_slice(), cut), (long.as_bytes(), false));
        let (part, cut) = run_up_to(dir.path(), show, &[], 1000).expect("reads");
        assert!(cut);
        assert!(part.len() <= 1000, "{}", part.len());
        assert!(part.len() > 1000 - "line 1999\n".len(), "as much as fits");
        assert!(part.ends_with(b"\n") && long.as_bytes().starts_with(&part));
        let (nothing, cut) = run_up_to(dir.path(), show, &[], 0).expect("reads");
        assert_eq!((nothing.as_slice(), cut), (&b""[..], true));
        // A git still running past the budget, here an alias that prints a
        // line longer than it and sleeps, is killed with its group, so the
        // read does not wait for it.
        let root = dir.path().to_path_buf();
        let slow = ["-c", "alias.slow=!printf '%01999d\\n' 0; sleep 30", "slow"];
        let read = at_once(dir.path(), move || run_up_to(&root, slow, &[], 1000));
        let (none, cut) = read.expect("reads");
        assert_eq!((none.as_slice(), cut), (&b""[..], true), "no whole line");
        // An exit code counts as success only when accepted; a failure
        // says git's first line.
        let no_index = ["diff", "--no-index", "--", "/dev/null", "./long.txt"];
        assert!(run_up_to(dir.path(), no_index, &[], usize::MAX).is_err());
        let (diff, cut) = run_up_to(dir.path(), no_index, &[1], usize::MAX).expect("reads");
        assert!(diff.starts_with(b"diff --git") && !cut);
        let error = run_up_to(dir.path(), ["rev-parse", "--verify", "nope"], &[], 10)
            .expect_err("no such revision");
        assert!(format!("{error:#}").starts_with("fatal:"), "{error:#}");
    }

    #[test]
    fn run_returns_stdout_or_the_first_stderr_line() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let out = run(dir.path(), ["rev-parse", "--is-inside-work-tree"]).expect("runs");
        assert_eq!(out, b"true\n");

        let error = run(dir.path(), ["rev-parse", "--verify", "nope"]).expect_err("fails");
        let message = error.to_string();
        assert!(message.starts_with("fatal:"), "{message}");
        assert!(!message.contains('\n'), "{message}");

        let missing = dir.path().join("missing");
        let error = run(&missing, ["status"]).expect_err("no such directory");
        assert!(format!("{error:#}").contains("missing"), "{error:#}");
    }
}
