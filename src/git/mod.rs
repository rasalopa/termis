//! Git, through the `git` binary (ADR-0001).

use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

pub mod commit;
pub mod diff;
pub mod files;
pub mod find;
pub mod last;
pub mod numstat;
pub mod stage;
pub mod status;
pub mod today;
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
    use std::os::unix::fs::{OpenOptionsExt, symlink};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// The untracked paths [`odd_paths`] makes that git looks through:
    /// links to a pipe and to a directory.
    pub(crate) const REFUSED: [&str; 2] = ["to-fifo", "to-dir"];

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
    fn hooks_off(dir: &Path) {
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
