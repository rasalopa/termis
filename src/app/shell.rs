//! The user's shell, which a terminal tab runs.

use std::path::Path;

use crate::Size;
use crate::emu;
use crate::pty;

/// The shell run when `$SHELL` names none.
const FALLBACK: &str = "/bin/sh";

/// A shell to run in a terminal: the program, its arguments and the name
/// its tab takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shell {
    program: String,
    args: Vec<String>,
    name: String,
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
        }
    }

    /// `/bin/sh` running `script`, named `sh`, for tests.
    #[cfg(test)]
    pub(super) fn running(script: &str) -> Self {
        Self {
            program: FALLBACK.to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            name: "sh".to_owned(),
        }
    }

    /// The name of its program's file, which its tab takes.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// The command for the shell in `cwd` on a terminal of `size`, with
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_is_an_absolute_shell_else_bin_sh() {
        let zsh = Shell::login(Some("/bin/zsh"));
        assert_eq!(
            (zsh.program.as_str(), zsh.args.as_slice(), zsh.name()),
            ("/bin/zsh", &["-l".to_owned()][..], "zsh")
        );
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
}
