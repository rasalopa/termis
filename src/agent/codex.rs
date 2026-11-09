//! The OpenAI Codex CLI as an agent.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::{AccountEnv, Agent, Launch, Recent, State};
use crate::Size;
use crate::emu;
use crate::instance::Instance;
use crate::projects::read::open_regular;
use crate::pty::Spec;

/// Codex's program, looked up on the `PATH`.
const PROGRAM: &str = "codex";

/// Codex's flag that keeps its app server in the TUI's own process,
/// away from the background server every `codex` shares, so that the
/// conversation stops with the session's process group.
const NO_DAEMON: &str = "--no-daemon";

/// Codex's subcommand that continues the directory's last interactive
/// conversation, or starts a new one when there is none.
const RESUME: [&str; 2] = ["resume", "--last"];

/// Codex's long options with which [`NO_DAEMON`] is not passed: Codex
/// refuses it twice, and with `--remote`, which names another server.
const SERVER_OPTIONS: [&str; 2] = ["no-daemon", "remote"];

/// Codex 0.167's long options that take a value, which follows as the
/// next argument unless it comes after `=`.
const VALUE_OPTIONS: [&str; 12] = [
    "config",
    "enable",
    "disable",
    "model",
    "profile",
    "sandbox",
    "ask-for-approval",
    "cd",
    "add-dir",
    "local-provider",
    "remote",
    "remote-auth-token-env",
];

/// The short forms among them, whose value may also follow the letter.
const VALUE_SHORTS: [char; 6] = ['c', 'm', 'p', 's', 'a', 'C'];

/// Codex's option that takes values up to the next option, and its
/// short form.
const LIST_OPTION: (&str, char) = ("image", 'i');

/// Variables Codex 0.167 tells its terminal by besides those of
/// [`emu::HOST_VARIABLES`]: with any of them it takes `termis` for the
/// terminal they name, `ITERM_PROFILE` for iTerm2.
const TERMINAL_HINTS: [&str; 11] = [
    "ITERM_PROFILE",
    "ITERM_PROFILE_NAME",
    "WEZTERM_VERSION",
    "ALACRITTY_SOCKET",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_VERSION",
    "VTE_VERSION",
    "KONSOLE_VERSION",
    "GNOME_TERMINAL_SCREEN",
    "WT_SESSION",
];

/// The variable that moves Codex's home, which holds its login, its
/// configuration and its conversations, away from `~/.codex`.
const HOME_VAR: &str = "CODEX_HOME";

/// The variable that moves Codex's databases, the list of its
/// conversations among them, out of its home. An account's session runs
/// without it, so that its conversations are the account's.
const SQLITE_HOME_VAR: &str = "CODEX_SQLITE_HOME";

/// Codex's home in the user's home, unless `CODEX_HOME` names another.
const OWN_DIR: &str = ".codex";

/// The name Codex gives itself in the program status reports it sends
/// its terminal.
const REPORTING_APP: &str = "codex";

/// The folder of Codex's home that holds a file per conversation, in a
/// folder per year, month and day.
const SESSIONS: &str = "sessions";

/// The most session files the recent projects are read from.
const SCANNED: usize = 1000;

/// The longest first line of a session file read, in bytes; Codex 0.167
/// writes about 15 KiB.
const FIRST_LINE_MAX: usize = 256 * 1024;

/// The sources of the conversations `codex resume --last` continues, as
/// a session file names them: Codex's TUI and its IDE extension.
const INTERACTIVE: [&str; 2] = ["cli", "vscode"];

/// The others, which a session file names as `{"custom": name}`: the
/// Atlas and ChatGPT apps.
const INTERACTIVE_CUSTOM: [&str; 2] = ["atlas", "chatgpt"];

/// The Codex CLI, started as `codex` from the `PATH`.
#[derive(Default)]
pub struct Codex {
    /// What the first line of each session file read says, by the home
    /// it is in, then by its path: only the files of the last read of
    /// each home, which reads a file's first line only when it is not
    /// here yet.
    firsts: Mutex<HashMap<PathBuf, HashMap<PathBuf, First>>>,
}

/// What the whole first line of a session file says, which stays as it
/// is: Codex adds to the file and never writes that line again.
#[derive(Clone, Debug, PartialEq, Eq)]
struct First {
    /// The file's inode when it was read: another file at that path is
    /// read again.
    inode: u64,
    /// The directory of the conversation, when it is an interactive one.
    dir: Option<PathBuf>,
}

impl Agent for Codex {
    fn name(&self) -> &str {
        "codex"
    }

    /// `codex` with `--no-daemon`, unless the user's arguments name a
    /// server, then `resume --last` when the launch continues a
    /// conversation, then the user's arguments, on the emulator's
    /// terminal and without the variables by which Codex would tell the
    /// host terminal. The instance goes unused: Codex reports its state
    /// unasked. An account's session gets its directory as `CODEX_HOME`
    /// and runs without `CODEX_SQLITE_HOME`; Codex's own account keeps
    /// both as `termis` has them.
    fn spec(&self, launch: &Launch<'_>, size: Size, _: Option<&Instance>) -> Spec {
        let account = self.account_env(launch.account);
        let env = emu::ENVIRONMENT
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .chain(account.env)
            .collect();
        Spec {
            program: PROGRAM.to_owned(),
            args: arguments(launch),
            cwd: launch.cwd.to_path_buf(),
            env,
            env_default: emu::ENVIRONMENT_DEFAULTS
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            env_remove: emu::HOST_VARIABLES
                .iter()
                .chain(&TERMINAL_HINTS)
                .map(|&name| name.to_owned())
                .chain(account.env_remove)
                .collect(),
            size,
        }
    }

    /// The state a program status report of Codex's says, as Codex 0.163
    /// and later send them unasked whenever their output is a terminal:
    /// `state=working` is working; `blocked`, on a permission, a question
    /// or a sign-in, is asking; `done`, `idle` and `error` are waiting.
    /// None for a report without `app=codex`, such as `state=clear`,
    /// which Codex sends as it exits, and one with any other state.
    fn reported(&self, report: &[u8]) -> Option<State> {
        super::program_status(report, REPORTING_APP)
    }

    /// The directories of the conversations Codex had as the account
    /// whose directory is `account`, else in the home `CODEX_HOME` names,
    /// else in `~/.codex`, that still exist, each with the last time one
    /// of their session files changed, the latest first. Only the
    /// interactive ones count, those `codex resume --last` continues, and
    /// none that one of them started, as a subagent's. Only the first
    /// lines of the 1000 session files with the latest names are read,
    /// each file's once; a name starts with the time its conversation
    /// began, so a conversation begun before those and continued since
    /// is left out.
    fn recent_projects(&self, account: Option<&Path>) -> Vec<Recent> {
        codex_home(account, std::env::var_os(HOME_VAR), std::env::home_dir())
            .map_or_else(Vec::new, |home| self.scan(&home))
    }

    /// `~/.codex` and the directory `CODEX_HOME` names when `termis` has
    /// it: an account in one of them would share Codex's own login and
    /// conversations.
    fn own_dirs(&self) -> Vec<PathBuf> {
        own_dirs(std::env::var_os(HOME_VAR), std::env::home_dir())
    }

    /// `CODEX_HOME` set to the account's directory as given, and
    /// `CODEX_SQLITE_HOME` removed. None for Codex's own, which keeps
    /// both as `termis` has them.
    fn account_env(&self, account: Option<&Path>) -> AccountEnv {
        account.map_or_else(AccountEnv::default, |dir| AccountEnv {
            env: vec![(HOME_VAR.to_owned(), dir.to_string_lossy().into_owned())],
            env_remove: vec![SQLITE_HOME_VAR.to_owned()],
        })
    }
}

impl Codex {
    /// The recent projects of [`Agent::recent_projects`] in the Codex
    /// home `home`: the session files of [`session_files`], each first
    /// line read only when the last scan of `home` did not read it, and
    /// kept for the next.
    fn scan(&self, home: &Path) -> Vec<Recent> {
        let mut known = self
            .firsts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(home)
            .unwrap_or_default();
        let mut read = HashMap::new();
        let mut newest: HashMap<PathBuf, Option<i64>> = HashMap::new();
        for file in session_files(&home.join(SESSIONS), SCANNED) {
            let Ok(metadata) = fs::metadata(&file) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let inode = metadata.ino();
            let first = match known.remove(&file).filter(|first| first.inode == inode) {
                Some(first) => first,
                None => match first_line(&file) {
                    Some(Line::Whole(line)) => First {
                        inode,
                        dir: session_dir(&line),
                    },
                    Some(Line::TooLong) => First { inode, dir: None },
                    None => continue,
                },
            };
            if let Some(dir) = &first.dir {
                let changed = modified(&metadata);
                let time = newest.entry(dir.clone()).or_insert(changed);
                *time = (*time).max(changed);
            }
            read.insert(file, first);
        }
        self.firsts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(home.to_path_buf(), read);
        let mut found: Vec<Recent> = newest
            .into_iter()
            .filter(|(dir, _)| dir.is_dir())
            .map(|(dir, started)| Recent { dir, started })
            .collect();
        // No time sorts below every time, so last.
        found.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| a.dir.cmp(&b.dir)));
        found
    }
}

/// The Codex home of the account whose directory is `account`: that
/// directory, else `variable`, the value of `CODEX_HOME`, when it is not
/// empty, else `.codex` in `home`.
fn codex_home(
    account: Option<&Path>,
    variable: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match (account, variable) {
        (Some(dir), _) => Some(dir.to_path_buf()),
        (None, Some(dir)) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| home.join(OWN_DIR)),
    }
}

/// The session files under `sessions`, at most `most` of them, the day
/// folder and the name that come last first.
fn session_files(sessions: &Path, most: usize) -> Vec<PathBuf> {
    numbered(sessions)
        .into_iter()
        .flat_map(|year| numbered(&year))
        .flat_map(|month| numbered(&month))
        .flat_map(|day| {
            let mut files: Vec<PathBuf> = fs::read_dir(&day)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("rollout-") && name.ends_with(".jsonl")
                })
                .map(|entry| entry.path())
                .collect();
            files.sort_unstable_by(|a, b| b.cmp(a));
            files
        })
        .take(most)
        .collect()
}

/// The folders in `dir` named by a number, as Codex names the year,
/// month and day folders, the highest first.
fn numbered(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let number = entry.file_name().to_str()?.parse().ok()?;
            Some((number, entry.path()))
        })
        .collect();
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.into_iter().map(|(_, path)| path).collect()
}

/// The first line of a session file, once it ends.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// The line, without its newline.
    Whole(Vec<u8>),
    /// A line longer than [`FIRST_LINE_MAX`] bytes, which is not read.
    TooLong,
}

/// The first line of the session file at `path`. None when the file
/// cannot be read, is not a regular file once open, such as a pipe,
/// which would hold up the main thread, or its first line does not end
/// yet, which a later read may find.
fn first_line(path: &Path) -> Option<Line> {
    let file = open_regular(path).ok()??;
    let mut line = Vec::new();
    BufReader::new(file.take(FIRST_LINE_MAX as u64 + 1))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.pop() == Some(b'\n') {
        Some(Line::Whole(line))
    } else if line.len() >= FIRST_LINE_MAX {
        Some(Line::TooLong)
    } else {
        None
    }
}

/// The directory a session file's first `line` names, when it is the
/// `session_meta` of an interactive conversation, by [`INTERACTIVE`] and
/// [`INTERACTIVE_CUSTOM`] or by having no source, which Codex reads as
/// the IDE extension's, and no `parent_thread_id`; none for a line that
/// does not parse or a directory that is not absolute.
fn session_dir(line: &[u8]) -> Option<PathBuf> {
    let meta = serde_json::from_slice::<Value>(line).ok()?;
    if meta["type"] != "session_meta" {
        return None;
    }
    let payload = &meta["payload"];
    let interactive = match &payload["source"] {
        Value::Null => true,
        Value::String(source) => INTERACTIVE.contains(&source.as_str()),
        Value::Object(source) => {
            source.len() == 1
                && source
                    .get("custom")
                    .and_then(Value::as_str)
                    .is_some_and(|name| INTERACTIVE_CUSTOM.contains(&name))
        }
        _ => false,
    };
    if !interactive || !payload["parent_thread_id"].is_null() {
        return None;
    }
    let dir = PathBuf::from(payload["cwd"].as_str()?);
    dir.is_absolute().then_some(dir)
}

/// When the file of `metadata` last changed, in milliseconds since the
/// Unix epoch.
fn modified(metadata: &fs::Metadata) -> Option<i64> {
    let since = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Codex's arguments for `launch`: [`NO_DAEMON`] unless the user's
/// arguments name a server, then [`RESUME`] when the launch continues a
/// conversation, then the user's arguments.
fn arguments(launch: &Launch<'_>) -> Vec<String> {
    let mut args = Vec::with_capacity(launch.args.len() + 3);
    if !given(launch.args).server {
        args.push(NO_DAEMON.to_owned());
    }
    if continues(launch) {
        args.extend(RESUME.map(str::to_owned));
    }
    args.extend_from_slice(launch.args);
    args
}

/// Whether `launch` continues the directory's last conversation: it is
/// not asked for a new one, and the user's arguments choose none.
fn continues(launch: &Launch<'_>) -> bool {
    !launch.new_conversation && !chooses_conversation(launch.args)
}

/// Whether `args` hold a word that is neither an option nor an option's
/// value: a subcommand, such as `exec` or `resume`, or a prompt, which
/// only Codex's list of subcommands tells apart, so either starts the
/// session without [`RESUME`].
fn chooses_conversation(args: &[String]) -> bool {
    given(args).word
}

/// What the user's arguments hold, as Codex's command line reads them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Given {
    /// Whether one of [`SERVER_OPTIONS`] is among them.
    server: bool,
    /// Whether a word is among them, as [`chooses_conversation`] says.
    word: bool,
}

/// Reads `args` as Codex's command line does: a long option's value
/// after `=` or as the next argument, a short one's after its letter or
/// as the next argument, the values of [`LIST_OPTION`] up to the next
/// option, short flags run together, and everything after `--` a word.
fn given(args: &[String]) -> Given {
    let mut given = Given::default();
    let mut args = args.iter().map(String::as_str).peekable();
    while let Some(arg) = args.next() {
        if arg == "--" {
            given.word |= args.next().is_some();
            break;
        }
        let takes = if let Some(long) = arg.strip_prefix("--") {
            let (name, takes) = match long.split_once('=') {
                Some((name, _)) => (name, Takes::Nothing),
                None => (long, long_takes(long)),
            };
            given.server |= SERVER_OPTIONS.contains(&name);
            takes
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            short_takes(shorts)
        } else {
            given.word = true;
            Takes::Nothing
        };
        match takes {
            Takes::Nothing => {}
            Takes::One => {
                args.next();
            }
            Takes::List => while args.next_if(|value| !value.starts_with('-')).is_some() {},
        }
    }
    given
}

/// What an option takes from the arguments that follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Takes {
    /// None: it takes no value, or its value came with it.
    Nothing,
    /// The next argument.
    One,
    /// The arguments up to the next option.
    List,
}

/// What the long option `name`, with no value after `=`, takes from
/// the next arguments.
fn long_takes(name: &str) -> Takes {
    if name == LIST_OPTION.0 {
        Takes::List
    } else if VALUE_OPTIONS.contains(&name) {
        Takes::One
    } else {
        Takes::Nothing
    }
}

/// What the short options `shorts`, run together after one `-`, take
/// from the next arguments: the first letter that takes a value takes
/// the rest of `shorts` as it, else what follows.
fn short_takes(shorts: &str) -> Takes {
    for (at, letter) in shorts.char_indices() {
        let takes = if letter == LIST_OPTION.1 {
            Takes::List
        } else if VALUE_SHORTS.contains(&letter) {
            Takes::One
        } else {
            continue;
        };
        let attached = at + letter.len_utf8() < shorts.len();
        return if attached { Takes::Nothing } else { takes };
    }
    Takes::Nothing
}

/// The directories of Codex's own account: `.codex` in `home`, and
/// `variable`, the value of `CODEX_HOME`, when it is set, not empty, as
/// Codex takes it, and another.
fn own_dirs(variable: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(home) = home {
        dirs.push(home.join(OWN_DIR));
    }
    if let Some(dir) = variable.filter(|dir| !dir.is_empty()).map(PathBuf::from)
        && !dirs.contains(&dir)
    {
        dirs.push(dir);
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    fn launch<'a>(cwd: &'a Path, args: &'a [String]) -> Launch<'a> {
        Launch {
            cwd,
            root: cwd,
            args,
            new_conversation: false,
            account: None,
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    const SIZE: Size = Size {
        rows: 40,
        cols: 120,
    };

    #[test]
    fn codex_runs_from_the_path_with_the_emulators_terminal() {
        let cwd = Path::new("/tmp/repo");
        let none = Vec::new();
        let spec = Codex::default().spec(&launch(cwd, &none), SIZE, None);
        assert_eq!(
            (Codex::default().name(), spec.program.as_str()),
            ("codex", "codex")
        );
        assert_eq!((spec.cwd.as_path(), spec.size), (cwd, SIZE));
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            env,
            [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
        );
        assert_eq!(
            spec.env_default,
            [("LC_CTYPE".to_owned(), "UTF-8".to_owned())]
        );
        // With an instance too: Codex gets nothing of it.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let instance = Instance::at(dir.path().join("i"), PathBuf::from("/bin/termis"));
        let with = Codex::default().spec(&launch(cwd, &none), SIZE, Some(&instance));
        assert_eq!((with.args, with.env), (spec.args, spec.env));
    }

    #[test]
    fn codex_does_not_learn_the_host_terminal() {
        let none = Vec::new();
        let spec = Codex::default().spec(&launch(Path::new("/p"), &none), SIZE, None);
        for name in emu::HOST_VARIABLES.iter().chain(&TERMINAL_HINTS) {
            assert!(
                spec.env_remove.iter().any(|removed| removed == name),
                "{name} is removed"
            );
        }
        assert!(TERMINAL_HINTS.contains(&"ITERM_PROFILE"));
        assert!(
            spec.env_remove
                .iter()
                .all(|name| name != "CODEX_HOME" && name != "CODEX_SQLITE_HOME"),
            "Codex's own account keeps its variables"
        );
    }

    #[test]
    fn codex_runs_without_its_background_server() {
        let cwd = Path::new("/p");
        let args = |given: &[&str], new_conversation: bool| {
            let given = strings(given);
            let at = Launch {
                new_conversation,
                ..launch(cwd, &given)
            };
            Codex::default().spec(&at, SIZE, None).args
        };
        assert_eq!(args(&[], false), ["--no-daemon", "resume", "--last"]);
        assert_eq!(args(&[], true), ["--no-daemon"]);
        assert_eq!(
            args(&["-m", "o3"], false),
            ["--no-daemon", "resume", "--last", "-m", "o3"]
        );
        assert_eq!(args(&["fix it"], false), ["--no-daemon", "fix it"]);
        assert_eq!(args(&["exec", "x"], false), ["--no-daemon", "exec", "x"]);
        // Not passed again, and not with another server.
        assert_eq!(
            args(&["--no-daemon"], false),
            ["resume", "--last", "--no-daemon"]
        );
        assert_eq!(
            args(&["--remote", "ws://h:1"], false),
            ["resume", "--last", "--remote", "ws://h:1"]
        );
        assert_eq!(args(&["--remote=unix://"], true), ["--remote=unix://"]);
        // A prompt that reads as the option is not it.
        assert_eq!(
            args(&["--", "--remote"], true),
            ["--no-daemon", "--", "--remote"]
        );
    }

    #[test]
    fn a_launch_continues_the_last_conversation_unless_new() {
        let none = Vec::new();
        let at = launch(Path::new("/p"), &none);
        assert!(continues(&at));
        let new = Launch {
            new_conversation: true,
            ..at
        };
        assert!(!continues(&new));
        let prompt = strings(&["fix it"]);
        assert!(!continues(&launch(Path::new("/p"), &prompt)));
    }

    #[test]
    fn a_subcommand_or_a_prompt_chooses_the_conversation() {
        let continuing: &[&[&str]] = &[
            &[],
            &["-m", "o3"],
            &["--model=o3"],
            &["-mo3"],
            &["-c", "model=\"o3\""],
            &["-i", "a.png", "b.png", "--search"],
            &["--image", "a.png"],
            &["-ia.png"],
            &["--enable", "x"],
            &["--search", "--no-alt-screen", "-C", "sub"],
            &["--"],
        ];
        for args in continuing {
            assert!(!chooses_conversation(&strings(args)), "{args:?}");
        }
        let choosing: &[&[&str]] = &[
            &["exec"],
            &["resume", "x"],
            &["fix it"],
            &["-m", "o3", "fix it"],
            &["--model=o3", "fix it"],
            &["-mo3", "fix it"],
            &["--search", "fix it"],
            &["-ia.png", "fix it"],
            &["--", "x"],
            &["-"],
        ];
        for args in choosing {
            assert!(chooses_conversation(&strings(args)), "{args:?}");
        }
    }

    #[test]
    fn an_account_runs_with_its_dir_as_codex_home() {
        let none = Vec::new();
        let dir = PathBuf::from("/Users/me/.codex-work/");
        let at = Launch {
            account: Some(&dir),
            ..launch(Path::new("/p"), &none)
        };
        let spec = Codex::default().spec(&at, SIZE, None);
        let set: Vec<&(String, String)> = spec
            .env
            .iter()
            .filter(|(name, _)| name == "CODEX_HOME")
            .collect();
        assert_eq!(
            set,
            [&("CODEX_HOME".to_owned(), "/Users/me/.codex-work/".to_owned())],
            "as written"
        );
        assert!(
            spec.env_remove
                .iter()
                .any(|name| name == "CODEX_SQLITE_HOME")
        );
        assert_eq!(
            Codex::default().account_env(Some(&dir)),
            AccountEnv {
                env: vec![("CODEX_HOME".to_owned(), "/Users/me/.codex-work/".to_owned())],
                env_remove: vec!["CODEX_SQLITE_HOME".to_owned()],
            }
        );
        assert_eq!(Codex::default().account_env(None), AccountEnv::default());
    }

    #[test]
    fn a_codex_report_gives_its_state() {
        let state = |report: &str| Codex::default().reported(report.as_bytes());
        // As Codex 0.167 sends them; 0.163 sends `blocked` without a kind.
        assert_eq!(state("state=working:app=codex"), Some(State::Working));
        for blocked in [
            "state=blocked:app=codex",
            "state=blocked:app=codex:kind=permission",
            "state=blocked:app=codex:kind=question",
            "state=blocked:app=codex:kind=auth",
        ] {
            assert_eq!(state(blocked), Some(State::Attention), "{blocked}");
        }
        for ended in ["done", "idle", "error"] {
            let report = format!("state={ended}:app=codex");
            assert_eq!(state(&report), Some(State::Waiting), "{ended}");
        }
        assert_eq!(state("state=clear"), None, "as it exits");
    }

    #[test]
    fn a_report_of_another_app_says_nothing_to_codex() {
        for other in [
            "state=working:app=claude-code",
            "state=blocked:app=claude-code:kind=permission",
            "state=working:app=Codex",
            "state=working",
            "app=codex",
        ] {
            assert_eq!(Codex::default().reported(other.as_bytes()), None, "{other}");
        }
    }

    #[test]
    fn codex_own_dirs_are_dot_codex_and_codex_home() {
        let home = Some(PathBuf::from("/home/u"));
        let default = [Path::new("/home/u/.codex")];
        assert_eq!(own_dirs(None, home.clone()), default);
        assert_eq!(own_dirs(Some("".into()), home.clone()), default, "empty");
        for same in ["/home/u/.codex", "/home/u/.codex/"] {
            assert_eq!(own_dirs(Some(same.into()), home.clone()), default, "{same}");
        }
        assert_eq!(
            own_dirs(Some("/w".into()), home),
            [default[0], Path::new("/w")]
        );
        assert_eq!(own_dirs(Some("/w".into()), None), [PathBuf::from("/w")]);
        assert!(own_dirs(None, None).is_empty());
    }

    /// The first line Codex 0.167 writes to a session file, abridged, for
    /// a conversation in `cwd` from `source`, padded with `padding`, and
    /// its newline.
    fn meta(cwd: &Path, source: Value, padding: usize) -> String {
        let line = json!({
            "timestamp": "2026-11-07T10:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": "019a5e3c-0000-7000-8000-000000000001",
                "timestamp": "2026-11-07T10:00:00.000Z",
                "cwd": cwd,
                "originator": "codex-tui",
                "cli_version": "0.167.0",
                "source": source,
                "model_provider": "openai",
                "base_instructions": { "text": "x".repeat(padding) },
            },
        });
        format!("{line}\n")
    }

    /// Writes `text` to the session file `name` of `home`'s day folder
    /// `day`, changed `at` seconds after the epoch. Returns its path.
    fn session(home: &Path, day: &str, name: &str, text: &str, at: u64) -> PathBuf {
        let folder = home.join(SESSIONS).join(day);
        fs::create_dir_all(&folder).expect("mkdir");
        let path = folder.join(name);
        fs::write(&path, text).expect("write");
        let file = fs::File::options().write(true).open(&path).expect("open");
        let time = UNIX_EPOCH + Duration::from_secs(at);
        file.set_modified(time).expect("set the time");
        path
    }

    /// A home for Codex and the project directories `names` beside it.
    fn places<const N: usize>(names: [&str; N]) -> (TempDir, PathBuf, [PathBuf; N]) {
        let dir = TempDir::new().expect("temp dir");
        let base = dir.path().canonicalize().expect("exists");
        let home = base.join("codex");
        fs::create_dir(&home).expect("mkdir");
        let projects = names.map(|name| {
            let project = base.join(name);
            fs::create_dir(&project).expect("mkdir");
            project
        });
        (dir, home, projects)
    }

    fn recent(dir: &Path, seconds: i64) -> Recent {
        Recent {
            dir: dir.to_path_buf(),
            started: Some(seconds * 1000),
        }
    }

    #[test]
    fn recent_projects_come_from_the_first_line_of_each_session_file() {
        let (_dir, home, [api, web, other]) = places(["api", "web", "other"]);
        let cli = json!("cli");
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-01.jsonl",
            &meta(&web, cli.clone(), 0),
            100,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T11-00-00-02.jsonl",
            &meta(&api, json!("vscode"), 0),
            200,
        );
        // A later line naming another directory is not read.
        let later = format!(
            "{}{}\n",
            meta(&web, cli.clone(), 0),
            json!({ "type": "turn_context", "payload": { "cwd": other } })
        );
        session(
            &home,
            "2026/11/06",
            "rollout-2026-11-06T09-00-00-03.jsonl",
            &later,
            50,
        );
        // A file Codex does not name so is not a session file.
        session(
            &home,
            "2026/11/07",
            "notes.jsonl",
            &meta(&other, cli.clone(), 0),
            300,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T12-00-00-04.json",
            &meta(&other, cli, 0),
            300,
        );
        let codex = Codex::default();
        assert_eq!(
            codex.recent_projects(Some(&home)),
            [recent(&api, 200), recent(&web, 100)]
        );
    }

    #[test]
    fn the_newest_session_file_of_a_directory_gives_its_time() {
        let (_dir, home, [api, web]) = places(["api", "web"]);
        let cli = json!("cli");
        // An old conversation continued today, and a newer one left.
        session(
            &home,
            "2026/10/01",
            "rollout-2026-10-01T09-00-00-01.jsonl",
            &meta(&api, cli.clone(), 0),
            500,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T09-00-00-02.jsonl",
            &meta(&api, cli.clone(), 0),
            300,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-03.jsonl",
            &meta(&web, cli, 0),
            400,
        );
        let codex = Codex::default();
        assert_eq!(codex.scan(&home), [recent(&api, 500), recent(&web, 400)]);
    }

    #[test]
    fn exec_and_subagent_sessions_are_not_projects() {
        let (_dir, home, [app, old, chatgpt, ide]) = places(["app", "old", "chatgpt", "ide"]);
        let not_interactive = [
            json!("exec"),
            json!("mcp"),
            json!("unknown"),
            json!({ "custom": "other" }),
            json!({ "subagent": "review" }),
            json!({ "subagent": { "thread_spawn": { "parent_thread_id": "019a", "depth": 1 } } }),
        ];
        for (at, source) in not_interactive.into_iter().enumerate() {
            let name = format!("rollout-2026-11-07T10-00-0{at}-1.jsonl");
            session(&home, "2026/11/07", &name, &meta(&app, source, 0), 900);
        }
        // An interactive source with a parent is a subagent's too.
        let mut child: Value = serde_json::from_str(&meta(&app, json!("cli"), 0)).expect("json");
        child["payload"]["parent_thread_id"] = json!("019a5e3c-0000-7000-8000-000000000002");
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T11-00-00-1.jsonl",
            &format!("{child}\n"),
            900,
        );
        // Interactive: the apps', and a line with no source, as Codex
        // reads one from before it wrote them.
        let mut before: Value = serde_json::from_str(&meta(&old, json!("cli"), 0)).expect("json");
        before["payload"]
            .as_object_mut()
            .expect("object")
            .remove("source");
        session(
            &home,
            "2026/01/02",
            "rollout-2026-01-02T10-00-00-1.jsonl",
            &format!("{before}\n"),
            10,
        );
        session(
            &home,
            "2026/11/06",
            "rollout-2026-11-06T10-00-00-1.jsonl",
            &meta(&chatgpt, json!({ "custom": "chatgpt" }), 0),
            30,
        );
        session(
            &home,
            "2026/11/06",
            "rollout-2026-11-06T11-00-00-1.jsonl",
            &meta(&ide, json!({ "custom": "atlas" }), 0),
            20,
        );
        let codex = Codex::default();
        assert_eq!(
            codex.scan(&home),
            [recent(&chatgpt, 30), recent(&ide, 20), recent(&old, 10)]
        );
    }

    #[test]
    fn a_long_or_broken_first_line_is_passed_over() {
        let (_dir, home, [long, whole]) = places(["long", "whole"]);
        let cli = json!("cli");
        // Padded to the most bytes read, newline aside, and one past it.
        let padded = |dir: &Path, length: usize| {
            let short = meta(dir, cli.clone(), 0).len() - 1;
            meta(dir, cli.clone(), length - short)
        };
        let fits = padded(&whole, FIRST_LINE_MAX);
        assert_eq!(fits.len(), FIRST_LINE_MAX + 1);
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-1.jsonl",
            &fits,
            100,
        );
        let over = padded(&long, FIRST_LINE_MAX + 1);
        assert_eq!(over.len(), FIRST_LINE_MAX + 2);
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-2.jsonl",
            &over,
            100,
        );
        for (at, text) in [
            "{ \"type\": \"session_meta\", \"payload\": \n",
            "[]\n",
            "\n",
            "\u{feff}{}\n",
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("rollout-2026-11-07T11-00-0{at}-1.jsonl");
            session(&home, "2026/11/07", &name, text, 100);
        }
        let without_dir =
            json!({ "type": "session_meta", "payload": { "source": "cli", "cwd": 7 } });
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T12-00-00-1.jsonl",
            &format!("{without_dir}\n"),
            100,
        );
        let codex = Codex::default();
        assert_eq!(codex.scan(&home), [recent(&whole, 100)]);
        assert_eq!(
            first_line(
                &home
                    .join(SESSIONS)
                    .join("2026/11/07/rollout-2026-11-07T10-00-00-2.jsonl")
            ),
            Some(Line::TooLong)
        );
    }

    #[test]
    fn a_fifo_in_place_of_a_session_file_is_passed_over() {
        let (_dir, home, [api]) = places(["api"]);
        let day = home.join(SESSIONS).join("2026/11/07");
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-1.jsonl",
            &meta(&api, json!("cli"), 0),
            100,
        );
        let made = Command::new("mkfifo")
            .arg(day.join("rollout-2026-11-07T11-00-00-2.jsonl"))
            .status()
            .expect("mkfifo");
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Codex::default().scan(&home));
        });
        let found = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("no wait for a writer");
        assert_eq!(found, [recent(&api, 100)]);
    }

    #[test]
    fn a_directory_gone_is_not_listed() {
        let (_dir, home, [api]) = places(["api"]);
        let gone = api.with_file_name("gone");
        let cli = json!("cli");
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-1.jsonl",
            &meta(&api, cli.clone(), 0),
            100,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T11-00-00-2.jsonl",
            &meta(&gone, cli.clone(), 0),
            200,
        );
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T12-00-00-3.jsonl",
            &meta(Path::new("api"), cli, 0),
            300,
        );
        let codex = Codex::default();
        assert_eq!(codex.scan(&home), [recent(&api, 100)]);
        fs::create_dir(&gone).expect("mkdir");
        assert_eq!(
            codex.scan(&home),
            [recent(&gone, 200), recent(&api, 100)],
            "back"
        );
    }

    #[test]
    fn no_sessions_folder_lists_nothing() {
        let (_dir, home, []) = places([]);
        let codex = Codex::default();
        assert!(codex.scan(&home).is_empty());
        assert!(codex.recent_projects(Some(&home.join("none"))).is_empty());
        fs::write(home.join(SESSIONS), "").expect("write");
        assert!(codex.scan(&home).is_empty(), "a file in its place");
        // The home: the account's, else the variable's, else the default.
        let user = Some(PathBuf::from("/home/u"));
        let work = Path::new("/w/codex");
        assert_eq!(
            codex_home(Some(work), Some("/v".into()), user.clone()),
            Some(work.to_path_buf())
        );
        assert_eq!(
            codex_home(None, Some("/v".into()), user.clone()),
            Some(PathBuf::from("/v"))
        );
        let own = Some(PathBuf::from("/home/u/.codex"));
        assert_eq!(codex_home(None, Some("".into()), user.clone()), own);
        assert_eq!(codex_home(None, None, user), own);
        assert_eq!(codex_home(None, None, None), None);
    }

    #[test]
    fn at_most_the_newest_1000_files_are_read() {
        let (_dir, home, [new, old]) = places(["new", "old"]);
        let cli = json!("cli");
        let line = meta(&new, cli.clone(), 0);
        for at in 0..SCANNED {
            let name = format!("rollout-2026-11-07T10-00-00-{at:04}.jsonl");
            session(&home, "2026/11/07", &name, &line, 100);
        }
        // Older by its folder, which sorts by number, so its later change
        // does not count.
        session(
            &home,
            "2026/9/30",
            "rollout-2026-09-30T10-00-00-1.jsonl",
            &meta(&old, cli, 0),
            900,
        );
        let codex = Codex::default();
        assert_eq!(codex.scan(&home), [recent(&new, 100)]);
        let files = session_files(&home.join(SESSIONS), SCANNED);
        assert_eq!(files.len(), SCANNED);
        assert!(files[0].ends_with("2026/11/07/rollout-2026-11-07T10-00-00-0999.jsonl"));
        assert_eq!(
            session_files(&home.join(SESSIONS), SCANNED + 1).len(),
            SCANNED + 1
        );
    }

    #[test]
    fn a_first_line_is_read_once_per_file() {
        let (_dir, home, [api, web]) = places(["api", "web"]);
        let cli = json!("cli");
        let first = session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-1.jsonl",
            &meta(&api, cli.clone(), 0),
            100,
        );
        let codex = Codex::default();
        assert_eq!(codex.scan(&home), [recent(&api, 100)]);
        // The same file written again keeps what its first line said.
        fs::write(&first, meta(&web, cli.clone(), 0)).expect("write");
        let file = fs::File::options().write(true).open(&first).expect("open");
        file.set_modified(UNIX_EPOCH + Duration::from_secs(150))
            .expect("set the time");
        assert_eq!(codex.scan(&home), [recent(&api, 150)], "not read again");
        // Another file at its path is read.
        fs::remove_file(&first).expect("remove");
        session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T10-00-00-1.jsonl",
            &meta(&web, cli.clone(), 0),
            160,
        );
        assert_eq!(codex.scan(&home), [recent(&web, 160)]);
        // A first line not ended yet is read again until it is.
        let line = meta(&api, cli, 0);
        let path = session(
            &home,
            "2026/11/07",
            "rollout-2026-11-07T11-00-00-2.jsonl",
            line.trim_end(),
            200,
        );
        assert_eq!(codex.scan(&home), [recent(&web, 160)]);
        fs::write(&path, &line).expect("write");
        let at = SystemTime::now();
        let file = fs::File::options().write(true).open(&path).expect("open");
        file.set_modified(at).expect("set the time");
        let found = codex.scan(&home);
        assert_eq!(
            found
                .iter()
                .map(|recent| recent.dir.as_path())
                .collect::<Vec<_>>(),
            [api.as_path(), web.as_path()]
        );
        // Only the files of the last scan are kept.
        fs::remove_file(&path).expect("remove");
        assert_eq!(codex.scan(&home), [recent(&web, 160)]);
        let kept = codex.firsts.lock().expect("not poisoned");
        assert_eq!(kept[&home].len(), 1);
    }
}
