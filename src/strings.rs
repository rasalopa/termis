//! User-facing strings, in one place.

use crate::git::numstat::Lines;
use crate::git::today::Today;
use crate::projects::{Framework, Stack};

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

/// Label of the branch row of the project card. Every label is cut to
/// one column under [`CARD_LABEL_WIDTH`] and padded to it, so a space
/// separates it from the value and the values line up.
pub const CARD_BRANCH: &str = "branch";
/// Label of the upstream row while no upstream is set. Otherwise the
/// row is labelled with what precedes the upstream's first `/`, its
/// remote, or with the whole name of a local upstream without one; a
/// local upstream with a `/` in its name shows its first part.
pub const CARD_REMOTE: &str = "remote";
/// Label of the row with today's lines.
pub const CARD_TODAY: &str = "today";
/// Label of the row with the last commit.
pub const CARD_LAST: &str = "last";
/// Columns a card row's label takes, its trailing space included.
pub const CARD_LABEL_WIDTH: usize = 8;
/// Value of a card row that has nothing yet: no commit, no upstream.
pub const CARD_NONE: &str = "none";
/// Value of a card row whose source could not be read.
pub const CARD_UNREAD: &str = "?";

/// The stack line of the card: the language, then the framework with
/// the version it asks for.
pub fn stack_line(stack: &Stack) -> String {
    match &stack.framework {
        Some(Framework {
            name,
            version: Some(version),
        }) => format!("{} · {name} {version}", stack.language),
        Some(Framework {
            name,
            version: None,
        }) => format!("{} · {name}", stack.language),
        None => stack.language.to_owned(),
    }
}

/// The upstream row: commits ahead and behind, or `gone` while the
/// upstream is set but its ref is missing.
pub fn upstream_state(ahead_behind: Option<(u32, u32)>) -> String {
    match ahead_behind {
        Some((ahead, behind)) => format!("↑{ahead} ↓{behind}"),
        None => "gone".to_owned(),
    }
}

/// Today's lines for the card, the commits being counted in the
/// changes pane; `none` on a day without commits.
pub fn today_lines(today: Today) -> String {
    if today.commits == 0 {
        return CARD_NONE.to_owned();
    }
    lines(
        Lines::Text {
            added: today.added,
            deleted: today.deleted,
        },
        false,
    )
}

/// An age in seconds as the card shows it: `now` under a minute, then
/// whole minutes, hours, days under two weeks, weeks under a year, and
/// years. A time ahead of the clock is `now`.
pub fn age(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;
    match seconds {
        s if s < MINUTE => "now".to_owned(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < 2 * WEEK => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}w", s / WEEK),
        s => format!("{}y", s / YEAR),
    }
}

/// Shown in the right pane when the directory is not inside a repository.
pub const NOT_A_REPOSITORY: &str = "not a git repository";

/// Shown in the right pane when there is nothing to stage or commit.
pub const NO_CHANGES: &str = "no changes";

/// The branch the panes show while `HEAD` is detached.
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
pub fn agent_pane_title(agent: &str, project: &str, scrolled: usize) -> String {
    if scrolled == 0 {
        format!("{agent} · {project}")
    } else {
        format!("{agent} · {project} ↑ {scrolled}")
    }
}

/// Status bar tag while keys go to termis's commands. While they go to the
/// agent, its name takes the place.
pub const MODE_TERM: &str = "TERMIS";

/// Status bar tag while keys go to the commit box.
pub const MODE_COMMIT: &str = "COMMIT";

/// Title of the today section in the changes pane.
pub const TODAY_SECTION: &str = "today";

/// The today line: the commits since midnight with their lines, or that
/// there are none.
pub fn today_line(today: Today) -> String {
    let lines = lines(
        Lines::Text {
            added: today.added,
            deleted: today.deleted,
        },
        false,
    );
    match today.commits {
        0 => "no commits".to_owned(),
        1 => format!("1 commit · {lines}"),
        count => format!("{count} commits · {lines}"),
    }
}

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

/// First line of the diff view: the path, and which side of the row.
pub fn diff_header(path: &str, staged: bool) -> String {
    if staged {
        format!("diff {path} (staged)")
    } else {
        format!("diff {path}")
    }
}

/// Shown in the diff view when git printed nothing for the row.
pub const DIFF_EMPTY: &str = "nothing to show";

/// Status bar hints while the diff view is open.
pub fn diff_hints() -> Vec<&'static str> {
    vec!["? keys", "j/k scroll", "d / Esc back", "q quit"]
}

/// Title of the key list `?` shows.
pub const HELP_TITLE: &str = "keys";

/// On the bottom border of the key list when the screen cuts it.
pub const HELP_CUT: &str = " … ";

/// Toast when the agent's telemetry could not be set up: the agent runs
/// without it, or keeps the status line chained from before.
pub fn telemetry_trouble(reason: &str) -> String {
    format!("agent status unavailable: {reason}")
}

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
        "agent pane",
        &[
            (
                "j / k / Up / Down / wheel",
                "scroll the view a line, or three with the wheel in any mode",
            ),
            ("PgUp / PgDn", "scroll it a page"),
            ("G", "back to the end, where new output appears"),
        ],
    ),
    (
        "changes pane",
        &[
            ("j / k / Up / Down / wheel", "move the selection"),
            ("s / u / a", "stage / unstage the row, stage everything"),
            (
                "d",
                "diff of the row; j / k or the wheel scroll, d / Esc back",
            ),
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
pub const HINTS_TO_DROP: [&str; 10] = [
    "PgUp/PgDn page",
    "G end",
    "Esc back",
    "1/2/3 pane",
    "z zoom",
    "z unzoom",
    "c commit",
    "a all",
    "j/k move",
    "j/k scroll",
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

/// Whose keys the termis-mode hints start with: the highlighted pane's
/// own, when it has some that apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneKeys {
    None,
    /// The scrollback keys of the agent pane.
    Agent,
    /// The list keys of the changes pane.
    Changes,
}

/// Status bar hints in termis mode: the commands that apply, the
/// highlighted pane's own keys first.
pub fn term_hints(side_panes: bool, zoom: Zoom, ended: bool, pane: PaneKeys) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(10);
    hints.push("? keys");
    match pane {
        PaneKeys::Agent => hints.extend(["j/k scroll", "PgUp/PgDn page", "G end"]),
        PaneKeys::Changes => {
            hints.extend(["j/k move", "s stage", "u unstage", "a all", "c commit"]);
        }
        PaneKeys::None => {}
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
            joined(term_hints(true, Zoom::In, false, PaneKeys::None)),
            "? keys · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(false, Zoom::Out, false, PaneKeys::None)),
            "? keys · z unzoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(false, Zoom::None, true, PaneKeys::None)),
            "? keys · r relaunch · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(true, Zoom::In, false, PaneKeys::Changes)),
            "? keys · j/k move · s stage · u unstage · a all · c commit · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(true, Zoom::Out, false, PaneKeys::Agent)),
            "? keys · j/k scroll · PgUp/PgDn page · G end · 1/2/3 pane · z unzoom · q quit · Esc back"
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
        assert_eq!(
            joined(diff_hints()),
            "? keys · j/k scroll · d / Esc back · q quit"
        );
        assert_eq!(diff_header("src/a.rs", true), "diff src/a.rs (staged)");
        assert_eq!(diff_header("src/a.rs", false), "diff src/a.rs");
        assert_eq!(toast_committed("abc1234"), "committed abc1234");
        assert_eq!(commit_failed("hook says no"), "commit failed: hook says no");
        for hint in HINTS_TO_DROP {
            assert!(
                term_hints(true, Zoom::In, false, PaneKeys::Changes).contains(&hint)
                    || term_hints(true, Zoom::Out, false, PaneKeys::Agent).contains(&hint),
                "{hint} is a real hint"
            );
        }
    }

    #[test]
    fn the_agent_title_marks_a_scrolled_view() {
        assert_eq!(agent_pane_title("claude", "termis", 0), "claude · termis");
        assert_eq!(
            agent_pane_title("claude", "termis", 120),
            "claude · termis ↑ 120"
        );
    }

    #[test]
    fn card_strings_say_the_stack_the_upstream_and_the_age() {
        let stack = |framework| Stack {
            language: "Rust",
            framework,
        };
        assert_eq!(stack_line(&stack(None)), "Rust");
        assert_eq!(
            stack_line(&stack(Some(Framework {
                name: "ratatui",
                version: Some("0.30".into()),
            }))),
            "Rust · ratatui 0.30"
        );
        assert_eq!(
            stack_line(&stack(Some(Framework {
                name: "axum",
                version: None,
            }))),
            "Rust · axum"
        );
        assert_eq!(upstream_state(Some((2, 1))), "↑2 ↓1");
        assert_eq!(upstream_state(None), "gone");
        assert_eq!(
            today_lines(Today {
                commits: 3,
                added: 312,
                deleted: 58,
            }),
            "+312 −58"
        );
        assert_eq!(age(-5), "now");
        assert_eq!(age(59), "now");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3599), "59m");
        assert_eq!(age(2 * 3600 + 1), "2h");
        assert_eq!(age(13 * 86_400), "13d");
        assert_eq!(age(14 * 86_400), "2w");
        assert_eq!(age(364 * 86_400), "52w");
        assert_eq!(age(365 * 86_400), "1y");
        assert_eq!(age(800 * 86_400), "2y");
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
        assert_eq!(today_line(Today::default()), "no commits");
        assert_eq!(
            today_line(Today {
                commits: 1,
                added: 5,
                deleted: 0,
            }),
            "1 commit · +5 −0"
        );
        assert_eq!(
            today_line(Today {
                commits: 3,
                added: 312,
                deleted: 58,
            }),
            "3 commits · +312 −58"
        );
    }
}
