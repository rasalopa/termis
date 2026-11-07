//! What a terminal tab runs: the user's shell, or a program in its
//! place, the user's editor on a file or lazygit in a repository.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::Size;
use crate::agent::AccountEnv;
use crate::emu;
use crate::pty;

/// The shell run when `$SHELL` names none, and the one that runs the
/// editor.
const FALLBACK: &str = "/bin/sh";

/// The script `/bin/sh` runs for the editor: the value of `VISUAL`, else
/// of `EDITOR`, read as shell words, as git reads it, so it can carry
/// arguments, as `code --wait`, then the script's first argument, the
/// file, as one word whatever it holds. `exec` puts the editor in the
/// shell's place.
const EDIT: &str = r#"eval exec "${VISUAL:-$EDITOR}" '"$1"'"#;

/// The program `l` runs, looked up on the `PATH`, and the name of its
/// tab.
const LAZYGIT: &str = "lazygit";

/// What a terminal runs, which decides what reaches it and how it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Runs {
    /// The user's shell: `Ctrl+Z` reaches it, and it runs a program while
    /// another one is in its foreground.
    Shell,
    /// A program in the shell's place: the user's editor on a file, or
    /// lazygit. No shell resumes it, so `Ctrl+Z` does not reach it; it
    /// counts as a program running until it exits, and an exit other than
    /// a success leaves a notice.
    Program,
}

/// A program to run in a terminal: the program, its arguments, the name
/// its tab takes, the directory it runs in, and whether it is the user's
/// shell or a program in its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shell {
    program: String,
    args: Vec<String>,
    name: String,
    /// The directory it runs in; none for the session's.
    dir: Option<PathBuf>,
    runs: Runs,
}

impl Shell {
    /// The user's shell as a login shell: `$SHELL` when it is set to an
    /// absolute path, else `/bin/sh`.
    pub(super) fn of_user() -> Self {
        Self::login(std::env::var("SHELL").ok().as_deref())
    }

    /// `shell` as a login shell when it is an absolute path, else
    /// `/bin/sh`.
    fn login(shell: Option<&str>) -> Self {
        let program = shell
            .filter(|shell| Path::new(shell).is_absolute())
            .unwrap_or(FALLBACK);
        let name = Path::new(program).file_name().map_or_else(
            || program.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self {
            program: program.to_owned(),
            args: vec!["-l".to_owned()],
            name,
            dir: None,
            runs: Runs::Shell,
        }
    }

    /// The user's editor on the file at `path`, absolute, as
    /// [`Shell::edit`] runs it with `$VISUAL` and `$EDITOR`; none while
    /// neither is set to something.
    pub(super) fn editor_on(path: &Path) -> Option<Self> {
        let visual = std::env::var_os("VISUAL");
        let editor = std::env::var_os("EDITOR");
        Self::edit(visual.as_deref(), editor.as_deref(), path)
    }

    /// `/bin/sh` running [`EDIT`] with `path` as its first argument: the
    /// editor `visual` names, else the one `editor` names, on `path`,
    /// which is only ever an argument; an absolute one starts with `/`,
    /// so it does not read as an option either. Its tab is named after the
    /// file name of the [`first_word`] of the variable used and the file's
    /// name, as `nvim main.rs`. None while neither is set to something.
    fn edit(visual: Option<&OsStr>, editor: Option<&OsStr>, path: &Path) -> Option<Self> {
        let used = [visual, editor]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty())?
            .to_string_lossy();
        let first = first_word(&used);
        let program = Path::new(first)
            .file_name()
            .map_or(first.into(), OsStr::to_string_lossy);
        let file = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), OsStr::to_string_lossy);
        Some(Self {
            program: FALLBACK.to_owned(),
            args: vec![
                "-c".to_owned(),
                EDIT.to_owned(),
                "sh".to_owned(),
                path.to_string_lossy().into_owned(),
            ],
            name: format!("{program} {file}"),
            dir: None,
            runs: Runs::Program,
        })
    }

    /// lazygit in the repository whose work tree is at `root`, as
    /// [`Shell::lazygit`] finds it on the `PATH`; none when it is not
    /// there.
    pub(super) fn lazygit_in(root: &Path) -> Option<Self> {
        Self::lazygit(std::env::var_os("PATH").as_deref(), root)
    }

    /// lazygit, the first executable file of that name in the absolute
    /// directories of `path`, a value of `PATH`, run with no arguments in
    /// `root`, where it finds the repository as git does, a linked work
    /// tree's too. Its tab is named `lazygit`. None when no directory has
    /// it. A relative directory would count from `root`, inside the
    /// repository, so it is skipped.
    pub(super) fn lazygit(path: Option<&OsStr>, root: &Path) -> Option<Self> {
        Some(Self {
            program: on_path(LAZYGIT, path?)?,
            args: Vec::new(),
            name: LAZYGIT.to_owned(),
            dir: Some(root.to_path_buf()),
            runs: Runs::Program,
        })
    }

    /// `/bin/sh` running `script`, named `sh`, for tests.
    #[cfg(test)]
    pub(super) fn running(script: &str) -> Self {
        Self {
            program: FALLBACK.to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            name: "sh".to_owned(),
            dir: None,
            runs: Runs::Shell,
        }
    }

    /// `/bin/sh` running `script` as the editor on a file, named
    /// `vi a.txt`, for tests.
    #[cfg(test)]
    pub(super) fn editing(script: &str) -> Self {
        Self {
            name: "vi a.txt".to_owned(),
            runs: Runs::Program,
            ..Self::running(script)
        }
    }

    /// The name of its program's file, which its tab takes; for the
    /// editor, with the file's name.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// What it runs: the user's shell, or a program in its place.
    pub(super) fn runs(&self) -> Runs {
        self.runs
    }

    /// The command for the program in its directory, else in `cwd`, on a
    /// terminal of `size`, with the terminal environment an agent gets
    /// and the variables of `account`, its session's account.
    pub(super) fn spec(&self, cwd: &Path, size: Size, account: &AccountEnv) -> pty::Spec {
        pty::Spec {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.dir.as_deref().unwrap_or(cwd).to_path_buf(),
            env: emu::ENVIRONMENT
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .chain(account.env.iter().cloned())
                .collect(),
            env_default: emu::ENVIRONMENT_DEFAULTS
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            env_remove: emu::HOST_VARIABLES
                .iter()
                .map(|&name| name.to_owned())
                .chain(account.env_remove.iter().cloned())
                .collect(),
            size,
        }
    }
}

/// The file named `name` in the first absolute directory of `path`, a
/// value of `PATH`, that has it as a regular file with an execute bit
/// set, and whose path is UTF-8, as the program a terminal runs is.
fn on_path(name: &str, path: &OsStr) -> Option<String> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .filter(|file| {
            fs::metadata(file)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .find_map(|file| file.into_os_string().into_string().ok())
}

/// The first shell word of `value`, without its quotes when it starts
/// with one, as `/opt/my editor/ed` for `'/opt/my editor/ed' -w`.
fn first_word(value: &str) -> &str {
    let value = value.trim_start();
    match value.chars().next() {
        Some(quote @ ('\'' | '"')) => value[1..].split(quote).next().unwrap_or_default(),
        _ => value.split_whitespace().next().unwrap_or(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn the_shell_is_an_absolute_shell_else_bin_sh() {
        let zsh = Shell::login(Some("/bin/zsh"));
        assert_eq!(
            (zsh.program.as_str(), zsh.args.as_slice(), zsh.name()),
            ("/bin/zsh", &["-l".to_owned()][..], "zsh")
        );
        assert_eq!(zsh.runs(), Runs::Shell);
        assert_eq!(Shell::login(Some("/opt/homebrew/bin/fish")).name(), "fish");
        for unusable in [None, Some(""), Some("zsh"), Some("bin/zsh")] {
            let shell = Shell::login(unusable);
            assert_eq!(
                (shell.program.as_str(), shell.name()),
                (FALLBACK, "sh"),
                "{unusable:?}"
            );
            assert_eq!(shell.args, ["-l"]);
        }
    }

    #[test]
    fn the_shell_runs_in_the_terminal_an_agent_gets() {
        let size = Size { rows: 3, cols: 7 };
        let zsh = Shell::login(Some("/bin/zsh"));
        let spec = zsh.spec(Path::new("/tmp/project"), size, &AccountEnv::default());
        assert_eq!(spec.program, "/bin/zsh");
        assert_eq!(spec.args, ["-l"]);
        assert_eq!(spec.cwd, Path::new("/tmp/project"));
        assert_eq!(spec.size, size);
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(env, emu::ENVIRONMENT);
        let defaults: Vec<(&str, &str)> = spec
            .env_default
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(defaults, emu::ENVIRONMENT_DEFAULTS);
        assert_eq!(spec.env_remove, emu::HOST_VARIABLES);
        // As an account, with its variables after those, the editor too.
        let account = AccountEnv {
            env: vec![("AGENT_DIR".to_owned(), "/w/".to_owned())],
            env_remove: vec!["AGENT_LOGIN".to_owned()],
        };
        let editor = Shell::edit(None, Some(OsStr::new("vi")), Path::new("/p/a.txt"));
        for shell in [zsh, editor.expect("an editor")] {
            let spec = shell.spec(Path::new("/tmp/project"), size, &account);
            let name = shell.name();
            assert_eq!(spec.env[emu::ENVIRONMENT.len()..], account.env, "{name}");
            let removed = &spec.env_remove[emu::HOST_VARIABLES.len()..];
            assert_eq!(removed, account.env_remove, "{name}");
        }
    }

    #[test]
    fn the_editor_is_visual_else_editor_and_names_its_tab() {
        let os = |text: &'static str| Some(OsStr::new(text));
        let path = Path::new("/p/src/main.rs");
        let edit = |visual, editor| Shell::edit(visual, editor, path);
        let nvim = edit(os("/usr/local/bin/nvim"), os("vi")).expect("an editor");
        assert_eq!(nvim.name(), "nvim main.rs");
        assert_eq!(nvim.runs(), Runs::Program);
        assert_eq!(
            (nvim.program.as_str(), nvim.args.as_slice()),
            (
                FALLBACK,
                &[
                    "-c".to_owned(),
                    r#"eval exec "${VISUAL:-$EDITOR}" '"$1"'"#.to_owned(),
                    "sh".to_owned(),
                    "/p/src/main.rs".to_owned(),
                ][..]
            )
        );
        // An empty VISUAL counts as unset, as the shell has it; the first
        // word names the tab.
        let code = edit(os(""), os("/opt/bin/code --wait")).expect("an editor");
        assert_eq!(code.name(), "code main.rs");
        // A quoted first word is taken whole.
        for quoted in [r#"'/opt/my editor/ed' -w"#, r#""/opt/my editor/ed""#] {
            let ed = edit(None, Some(OsStr::new(quoted))).expect("an editor");
            assert_eq!(ed.name(), "ed main.rs", "{quoted}");
        }
        assert_eq!(
            edit(None, os("hx")).map(|shell| shell.name),
            Some("hx main.rs".into())
        );
        assert_eq!(edit(None, None), None);
        assert_eq!(edit(os(""), os("")), None);
    }

    #[test]
    fn lazygit_is_the_first_executable_on_the_path_and_runs_in_the_root() {
        let dir = TempDir::new().expect("temp dir");
        // A directory for the PATH, with a `lazygit` file of `mode` in it
        // when there is one.
        let bin = |name: &str, mode: Option<u32>| {
            let at = dir.path().join(name);
            fs::create_dir(&at).expect("mkdir");
            if let Some(mode) = mode {
                let file = at.join(LAZYGIT);
                fs::write(&file, "#!/bin/sh\n").expect("write");
                fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("chmod");
            }
            at
        };
        // A relative directory with one, counted from where the tests run;
        // a terminal would count it from the root.
        let up: PathBuf = std::env::current_dir()
            .expect("cwd")
            .components()
            .skip(1)
            .map(|_| "..")
            .collect();
        let held = bin("relative", Some(0o755));
        let relative = up.join(held.strip_prefix("/").expect("absolute"));
        assert!(relative.is_relative() && relative.join(LAZYGIT).is_file());
        let empty = bin("empty", None);
        let unusable = bin("unusable", Some(0o644));
        let holder = bin("holder", None);
        fs::create_dir(holder.join(LAZYGIT)).expect("a directory");
        let first = bin("first", Some(0o755));
        let second = bin("second", Some(0o755));
        let path = |dirs: &[&Path]| std::env::join_paths(dirs).expect("a PATH");
        let root = Path::new("/p/web");
        let found = path(&[&relative, &empty, &unusable, &holder, &first, &second]);
        let lazygit = Shell::lazygit(Some(&found), root).expect("found");
        assert_eq!(
            Some(lazygit.program.as_str()),
            first.join(LAZYGIT).to_str(),
            "the first absolute directory with an executable one"
        );
        assert!(lazygit.args.is_empty());
        assert_eq!(lazygit.name(), "lazygit");
        assert_eq!(lazygit.runs(), Runs::Program);
        let cwd = Path::new("/p/api");
        let size = Size { rows: 3, cols: 7 };
        let none = AccountEnv::default();
        let spec = lazygit.spec(cwd, size, &none);
        assert_eq!(spec.program, lazygit.program);
        assert_eq!(spec.cwd, root, "in the root, not the session's directory");
        // Nowhere on the PATH, or no PATH.
        let missing = path(&[&relative, &empty, &unusable, &holder]);
        assert_eq!(Shell::lazygit(Some(&missing), root), None);
        assert_eq!(Shell::lazygit(None, root), None);
        // The shell and the editor run in the session's directory.
        assert_eq!(
            Shell::login(Some("/bin/zsh")).spec(cwd, size, &none).cwd,
            cwd
        );
        let editor = Shell::edit(None, Some(OsStr::new("vi")), Path::new("/p/a.txt"));
        assert_eq!(editor.expect("an editor").spec(cwd, size, &none).cwd, cwd);
    }

    /// Runs `shell` on a pty in `dir` with `visual` and `editor` set, and
    /// returns what it printed once it ended.
    fn output(shell: &Shell, dir: &Path, visual: &str, editor: &str) -> String {
        let mut spec = shell.spec(dir, Size { rows: 4, cols: 200 }, &AccountEnv::default());
        spec.env.push(("VISUAL".into(), visual.into()));
        spec.env.push(("EDITOR".into(), editor.into()));
        let (tx, rx) = mpsc::channel();
        let _session = {
            let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            pty::Session::spawn(&spec, tx, |event| event).expect("spawn")
        };
        let mut printed = Vec::new();
        let (mut closed, mut exited) = (false, false);
        while !(closed && exited) {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an event") {
                pty::Event::Output(bytes) => printed.extend(bytes),
                pty::Event::Closed => closed = true,
                pty::Event::Exited(_) => exited = true,
            }
        }
        String::from_utf8_lossy(&printed).into_owned()
    }

    #[test]
    fn the_path_reaches_the_editor_as_one_argument_whatever_it_holds() {
        let dir = TempDir::new().expect("temp dir");
        // Prints how many arguments it got, then each one in brackets.
        let printer = dir.path().join("print args");
        std::fs::write(
            &printer,
            "#!/bin/sh\nprintf '%s' \"$#\"; printf '[%s]' \"$@\"\n",
        )
        .expect("write");
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&printer, mode).expect("chmod");
        // Its path has a space, so the variable quotes it, as git needs.
        let printer = format!("'{}'", printer.display());
        let name = r#"-n it's "a" $HOME `id` ; b.txt"#;
        let path = dir.path().join(name);
        let shell = Shell::edit(None, Some(OsStr::new(&printer)), &path).expect("an editor");
        let expected = format!("1[{}]", path.display());
        assert_eq!(output(&shell, dir.path(), "", &printer), expected);
        // VISUAL first.
        assert_eq!(
            output(&shell, dir.path(), &printer, "/no/such/editor"),
            expected
        );
        // The variable's own arguments come before the path.
        let waiting = format!("{printer} --wait");
        assert_eq!(
            output(&shell, dir.path(), "", &waiting),
            format!("2[--wait][{}]", path.display())
        );
    }
}
