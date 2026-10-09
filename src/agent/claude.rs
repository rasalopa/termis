//! Claude Code as an agent.

use std::path::Path;

use super::Agent;
use crate::Size;
use crate::pty::Spec;

/// Variables the child must not inherit: they would make Claude Code
/// believe it runs inside the host terminal, a multiplexer or another
/// Claude session. What it learns about its terminal comes from `TERM`,
/// `COLORTERM` and the emulator's query replies.
const REMOVED: [&str; 15] = [
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
];

/// Claude Code, started as `claude` from the `PATH`.
pub struct Claude;

impl Agent for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    fn spec(&self, cwd: &Path, args: &[String], size: Size) -> Spec {
        Spec {
            program: "claude".to_owned(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
            env: vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
                // The app owns the pane title.
                (
                    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE".to_owned(),
                    "1".to_owned(),
                ),
            ],
            env_default: vec![("LC_CTYPE".to_owned(), "UTF-8".to_owned())],
            env_remove: REMOVED.iter().map(|&name| name.to_owned()).collect(),
            size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_launch_spec() {
        let size = Size {
            rows: 40,
            cols: 120,
        };
        let args = vec!["-c".to_owned(), "--model".to_owned(), "opus".to_owned()];
        let spec = Claude.spec(Path::new("/tmp/repo"), &args, size);
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, args);
        assert_eq!(spec.cwd, Path::new("/tmp/repo"));
        assert_eq!(spec.size, size);
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            env,
            [
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1"),
            ]
        );
        assert_eq!(
            spec.env_default,
            [("LC_CTYPE".to_owned(), "UTF-8".to_owned())]
        );
        for name in REMOVED {
            assert!(
                spec.env_remove.iter().any(|n| n == name),
                "{name} is removed"
            );
        }
    }

    #[test]
    fn has_a_name() {
        assert_eq!(Claude.name(), "claude");
        assert_eq!(super::super::default_agent().name(), "claude");
    }
}
