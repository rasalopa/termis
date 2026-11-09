//! The OpenAI Codex CLI as an agent.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{AccountEnv, Agent, Launch};
use crate::Size;
use crate::emu;
use crate::instance::Instance;
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

/// The Codex CLI, started as `codex` from the `PATH`.
pub struct Codex;

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
        let spec = Codex.spec(&launch(cwd, &none), SIZE, None);
        assert_eq!((Codex.name(), spec.program.as_str()), ("codex", "codex"));
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
        let with = Codex.spec(&launch(cwd, &none), SIZE, Some(&instance));
        assert_eq!((with.args, with.env), (spec.args, spec.env));
    }

    #[test]
    fn codex_does_not_learn_the_host_terminal() {
        let none = Vec::new();
        let spec = Codex.spec(&launch(Path::new("/p"), &none), SIZE, None);
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
            Codex.spec(&at, SIZE, None).args
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
        let spec = Codex.spec(&at, SIZE, None);
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
            Codex.account_env(Some(&dir)),
            AccountEnv {
                env: vec![("CODEX_HOME".to_owned(), "/Users/me/.codex-work/".to_owned())],
                env_remove: vec!["CODEX_SQLITE_HOME".to_owned()],
            }
        );
        assert_eq!(Codex.account_env(None), AccountEnv::default());
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
}
