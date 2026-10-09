//! User-facing strings, in one place.

use crate::git::numstat::Lines;

/// Printed after the agent ended on its own.
pub fn agent_exited(code: u32) -> String {
    format!("agent exited with code {code}")
}

/// Printed after the agent was ended by a signal.
pub fn agent_killed(signal: &str) -> String {
    format!("agent was killed by {signal}")
}

/// Title of the left pane.
pub const PANE_PROJECTS: &str = "Projects";

/// Title of the right pane.
pub const PANE_CHANGES: &str = "Changes";

/// Shown in the right pane when the directory is not inside a repository.
pub const NOT_A_REPOSITORY: &str = "not a git repository";

/// Shown in the right pane when there is nothing to stage or commit.
pub const NO_CHANGES: &str = "no changes";

/// The branch shown in the right pane title while `HEAD` is detached.
pub const DETACHED_HEAD: &str = "detached";

/// Title of the right pane, with the branch once it is known.
pub fn changes_pane_title(branch: Option<&str>) -> String {
    match branch {
        Some(branch) => format!("{PANE_CHANGES} · {branch}"),
        None => PANE_CHANGES.to_owned(),
    }
}

/// First line of the changes pane: how many paths changed.
pub fn changes_files(files: usize) -> String {
    if files == 1 {
        "1 file".to_owned()
    } else {
        format!("{files} files")
    }
}

/// Title of the staged list in the changes pane, with its length.
pub fn changes_staged(count: usize) -> String {
    format!("staged ({count})")
}

/// Title of the unstaged list in the changes pane, with its length.
pub fn changes_unstaged(count: usize) -> String {
    format!("unstaged ({count})")
}

/// Lines added and removed, as `+a −d`; an untracked file only adds, so
/// only `+a`; `bin` when git did not count.
pub fn lines(lines: Lines, untracked: bool) -> String {
    match lines {
        Lines::Binary => "bin".to_owned(),
        Lines::Text { added, .. } if untracked => format!("+{added}"),
        Lines::Text { added, deleted } => format!("+{added} −{deleted}"),
    }
}

/// Marks the selected row of a list, and the room the others leave.
pub const SELECTED_MARKER: &str = "▸ ";
pub const UNSELECTED_MARKER: &str = "  ";

/// Title of the center pane.
pub fn agent_pane_title(agent: &str, project: &str) -> String {
    format!("{agent} · {project}")
}

/// Status bar tag while keys go to termis's commands. While they go to the
/// agent, its name takes the place.
pub const MODE_TERM: &str = "TERMIS";

/// Status bar tag while keys go to the commit box.
pub const MODE_COMMIT: &str = "COMMIT";

/// Title of the commit box in the changes pane.
pub const COMMIT_SECTION: &str = "commit";

/// Status bar hints while the commit box is open.
pub fn commit_hints() -> Vec<&'static str> {
    vec!["Enter commit", "Ctrl+J body line", "Esc cancel"]
}

/// Notice after a commit was made.
pub fn toast_committed(hash: &str) -> String {
    format!("committed {hash}")
}

/// Notice when the commit box cannot open or send: the index is empty.
pub const NOTHING_STAGED: &str = "nothing staged";

/// Notice when `Enter` finds no subject in the commit box.
pub const SUBJECT_NEEDED: &str = "a subject is needed";

/// Notice when `c` is pressed while a commit is still running.
pub const COMMIT_IN_PROGRESS: &str = "commit in progress";

/// Notice when a commit failed, with the line git or a hook printed.
pub fn commit_failed(line: &str) -> String {
    format!("commit failed: {line}")
}

/// Title of the key list `?` shows.
pub const HELP_TITLE: &str = "keys";

/// On the bottom border of the key list when the screen cuts it.
pub const HELP_CUT: &str = " … ";

/// Status bar hints while the key list is open.
pub fn help_hints() -> Vec<&'static str> {
    vec!["? or Esc close"]
}

/// Every key, under the mode it works in, for the list `?` shows.
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "agent mode",
        &[
            ("Ctrl+Q", "termis mode"),
            ("Ctrl+Z", "never reaches the agent"),
            ("r / q", "relaunch / quit once the agent ended"),
        ],
    ),
    (
        "termis mode",
        &[
            ("1 / 2 / 3", "highlight a pane"),
            ("Tab / Shift+Tab", "next / previous pane"),
            ("c", "open the commit box"),
            ("z", "zoom / unzoom the center pane"),
            ("q", "quit"),
            ("r", "relaunch the agent once it ended"),
            ("Esc / Ctrl+Q", "back to the agent"),
            ("?", "this list"),
        ],
    ),
    (
        "changes pane",
        &[
            ("j / k / Up / Down", "move the selection"),
            ("s / u", "stage / unstage the selected row"),
            ("a", "stage everything"),
        ],
    ),
    (
        "commit box",
        &[
            ("typing / Backspace / Ctrl+H", "edit the draft at its end"),
            ("Ctrl+J", "body line"),
            ("Enter", "commit"),
            (
                "Esc / Ctrl+Q",
                "close keeping the draft / back to the agent",
            ),
        ],
    ),
];

/// The status bar tag for a mode, padded into a block.
pub fn mode_tag(mode: &str) -> String {
    format!(" {mode} ")
}

/// Between the hints of the status bar.
pub const HINT_SEPARATOR: &str = " · ";

/// The hints a narrow status bar does without, in the order they go.
pub const HINTS_TO_DROP: [&str; 7] = [
    "Esc back",
    "1/2/3 pane",
    "z zoom",
    "z unzoom",
    "c commit",
    "a all",
    "j/k move",
];

/// Status bar hints while keys go to the agent.
pub fn agent_hints(ended: bool) -> Vec<&'static str> {
    if ended {
        vec!["r relaunch", "q quit", "Ctrl+Q termis mode"]
    } else {
        vec!["Ctrl+Q termis mode"]
    }
}

/// What the zoom key does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    /// Nothing: no side panes to hide and none hidden by the zoom.
    None,
    /// Hide the side panes.
    In,
    /// Bring the side panes back.
    Out,
}

/// Status bar hints in termis mode: the commands that apply, the list
/// keys first while the changes pane is highlighted.
pub fn term_hints(side_panes: bool, zoom: Zoom, ended: bool, changes: bool) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(10);
    hints.push("? keys");
    if changes {
        hints.extend(["j/k move", "s stage", "u unstage", "a all", "c commit"]);
    }
    if side_panes {
        hints.push("1/2/3 pane");
    }
    match zoom {
        Zoom::In => hints.push("z zoom"),
        Zoom::Out => hints.push("z unzoom"),
        Zoom::None => {}
    }
    if ended {
        hints.push("r relaunch");
    }
    hints.push("q quit");
    hints.push("Esc back");
    hints
}

/// Right side of the status bar.
pub fn status_project(project: &str) -> String {
    format!("termis · {project} ")
}

/// The line drawn over the agent's screen after it ended.
pub fn agent_ended_line(agent: &str, code: u32, signal: Option<&str>) -> String {
    match signal {
        Some(signal) => format!(" {agent} was killed by {signal} · r relaunch · q quit "),
        None => format!(" {agent} exited (code {code}) · r relaunch · q quit "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_line_names_the_signal_or_the_code() {
        assert_eq!(
            agent_ended_line("claude", 0, Some("SIGKILL")),
            " claude was killed by SIGKILL · r relaunch · q quit "
        );
        assert_eq!(
            agent_ended_line("claude", 3, None),
            " claude exited (code 3) · r relaunch · q quit "
        );
    }

    #[test]
    fn hints_follow_the_panes_the_zoom_and_the_agent() {
        let joined = |hints: Vec<&str>| hints.join(HINT_SEPARATOR);
        assert_eq!(
            joined(term_hints(true, Zoom::In, false, false)),
            "? keys · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(false, Zoom::Out, false, false)),
            "? keys · z unzoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(false, Zoom::None, true, false)),
            "? keys · r relaunch · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(true, Zoom::In, false, true)),
            "? keys · j/k move · s stage · u unstage · a all · c commit · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(joined(agent_hints(false)), "Ctrl+Q termis mode");
        assert_eq!(
            joined(agent_hints(true)),
            "r relaunch · q quit · Ctrl+Q termis mode"
        );
        assert_eq!(mode_tag(MODE_TERM), " TERMIS ");
        assert_eq!(mode_tag(MODE_COMMIT), " COMMIT ");
        assert_eq!(
            joined(commit_hints()),
            "Enter commit · Ctrl+J body line · Esc cancel"
        );
        assert_eq!(joined(help_hints()), "? or Esc close");
        assert_eq!(toast_committed("abc1234"), "committed abc1234");
        assert_eq!(commit_failed("hook says no"), "commit failed: hook says no");
        for hint in HINTS_TO_DROP {
            assert!(
                term_hints(true, Zoom::In, false, true).contains(&hint)
                    || term_hints(true, Zoom::Out, false, true).contains(&hint),
                "{hint} is a real hint"
            );
        }
    }

    #[test]
    fn changes_strings_count_and_format() {
        assert_eq!(changes_pane_title(Some("main")), "Changes · main");
        assert_eq!(changes_pane_title(None), "Changes");
        assert_eq!(changes_files(1), "1 file");
        assert_eq!(changes_files(0), "0 files");
        assert_eq!(changes_files(4), "4 files");
        assert_eq!(changes_staged(2), "staged (2)");
        assert_eq!(changes_unstaged(0), "unstaged (0)");
        let text = Lines::Text {
            added: 31,
            deleted: 4,
        };
        assert_eq!(lines(text, false), "+31 −4");
        assert_eq!(lines(text, true), "+31");
        assert_eq!(lines(Lines::Binary, true), "bin");
    }
}
