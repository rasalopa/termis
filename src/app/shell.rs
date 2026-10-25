//! What a terminal tab runs: the user's shell, or the user's editor on
//! a file.

use std::ffi::OsStr;
use std::path::Path;

use crate::Size;
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

/// A program to run in a terminal: the program, its arguments and the
/// name its tab takes, and whether it is the user's editor on a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shell {
    program: String,
    args: Vec<String>,
    name: String,
    editor: bool,
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
            editor: false,
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
            editor: true,
        })
    }

    /// `/bin/sh` running `script`, named `sh`, for tests.
    #[cfg(test)]
    pub(super) fn running(script: &str) -> Self {
        Self {
            program: FALLBACK.to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            name: "sh".to_owned(),
            editor: false,
        }
    }

    /// `/bin/sh` running `script` as the editor on a file, named
    /// `vi a.txt`, for tests.
    #[cfg(test)]
    pub(super) fn editing(script: &str) -> Self {
        Self {
            name: "vi a.txt".to_owned(),
            editor: true,
            ..Self::running(script)
        }
    }

    /// The name of its program's file, which its tab takes; for the
    /// editor, with the file's name.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// Whether it is the user's editor on a file.
    pub(super) fn is_editor(&self) -> bool {
        self.editor
    }

    /// The command for the program in `cwd` on a terminal of `size`, with
    /// the terminal environment an agent gets.
    pub(super) fn spec(&self, cwd: &Path, size: Size) -> pty::Spec {
        pty::Spec {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: cwd.to_path_buf(),
            env: emu::ENVIRONMENT
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            env_default: emu::ENVIRONMENT_DEFAULTS
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            env_remove: emu::HOST_VARIABLES
                .iter()
                .map(|&name| name.to_owned())
                .collect(),
            size,
        }
    }
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
        assert!(!zsh.is_editor());
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
        let spec = Shell::login(Some("/bin/zsh")).spec(Path::new("/tmp/project"), size);
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
    }

    #[test]
    fn the_editor_is_visual_else_editor_and_names_its_tab() {
        let os = |text: &'static str| Some(OsStr::new(text));
        let path = Path::new("/p/src/main.rs");
        let edit = |visual, editor| Shell::edit(visual, editor, path);
        let nvim = edit(os("/usr/local/bin/nvim"), os("vi")).expect("an editor");
        assert_eq!(nvim.name(), "nvim main.rs");
        assert!(nvim.is_editor());
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

    /// Runs `shell` on a pty in `dir` with `visual` and `editor` set, and
    /// returns what it printed once it ended.
    fn output(shell: &Shell, dir: &Path, visual: &str, editor: &str) -> String {
        let mut spec = shell.spec(dir, Size { rows: 4, cols: 200 });
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
