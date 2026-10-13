//! User-facing strings, in one place.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use crate::agent::State;
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

/// Columns a row's label takes in the agent's box, its trailing space
/// included. Every label is cut to one column under it and padded to it,
/// so a space separates it from the value and the values line up.
pub const AGENT_LABEL_WIDTH: usize = 8;
/// Starts the changes pane's line with the last commit.
pub const LAST_COMMIT: &str = "last";
/// Stands for a value that could not be read.
pub const UNREAD: &str = "?";

/// The stack line of the projects pane: the language, then the framework
/// with the version it asks for.
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

/// How the branch stands against its upstream: commits ahead and behind,
/// or `gone` while the upstream is set but its ref is missing.
pub fn upstream_state(ahead_behind: Option<(u32, u32)>) -> String {
    match ahead_behind {
        Some((ahead, behind)) => format!("↑{ahead} ↓{behind}"),
        None => "gone".to_owned(),
    }
}

/// An age in seconds, as the last commit shows it: `now` under a minute,
/// then whole minutes, hours, days under two weeks, weeks under a year,
/// and years. A time ahead of the clock is `now`.
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

/// Label of the agent box's state row; the box's labels line up.
pub const AGENT_STATE: &str = "state";
/// Label of the agent box's model row.
pub const AGENT_MODEL: &str = "model";
/// Label of the agent box's effort row.
pub const AGENT_EFFORT: &str = "effort";
/// Label of the agent box's context row.
pub const AGENT_CONTEXT: &str = "ctx";
/// Label of the agent box's cost row.
pub const AGENT_COST: &str = "cost";
/// Label of the agent box's row for the plan's five-hour limit.
pub const AGENT_FIVE_HOURS: &str = "5h";
/// Label of the agent box's row for the plan's weekly limit.
pub const AGENT_WEEK: &str = "week";
/// Label of the agent box's row of lines the agent changed.
pub const AGENT_LINES: &str = "lines";
/// Label of the agent box's session time row.
pub const AGENT_TIME: &str = "time";
/// The state row once the session is over.
pub const AGENT_ENDED: &str = "× ended";

/// A model's name in `width` columns: without a trailing parenthesised
/// note, such as the context size, when the whole name does not fit.
pub fn model_name(name: &str, width: usize) -> &str {
    if name.chars().count() <= width {
        return name;
    }
    match name
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
    {
        Some((short, _)) => short,
        None => name,
    }
}

/// What the agent is doing, as the agent's box says it.
pub fn agent_state(state: State) -> &'static str {
    match state {
        State::Working => "✶ working",
        State::Waiting => "· waiting",
        State::Attention => "! asking",
    }
}

/// A usage limit of the agent's plan: the share used and the time left
/// until the window resets, rounded up to the last unit shown: minutes
/// under an hour, then hours and minutes under a day, then days and
/// hours; `reset` once it did, until the agent reports again.
pub fn limit(used: u8, resets_in: i64) -> String {
    if resets_in <= 0 {
        return "reset".to_owned();
    }
    let up_to = |unit: i64| resets_in.saturating_add(unit - 1) / unit;
    let minutes = up_to(60);
    let left = if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    } else {
        let hours = up_to(3600);
        format!("{}d{}h", hours / 24, hours % 24)
    };
    format!("{used}% {left}")
}

/// The lines the agent added and removed in its session.
pub fn agent_lines(added: u64, removed: u64) -> String {
    format!("+{added} −{removed}")
}

/// The state row while the agent runs a tool: the tool in place of
/// `working`.
pub fn agent_running(activity: &str) -> String {
    format!("✶ {activity}")
}

/// A cost in US dollars, to the cent.
pub fn cost(usd: f64) -> String {
    format!("${usd:.2}")
}

/// How long a session has run: minutes and seconds, with the hours
/// first once there are any.
pub fn session_time(time: Duration) -> String {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// The share of the context in use as a bar of `width` cells and the
/// percentage after it, or the percentage alone when the bar would be
/// shorter than three cells.
pub fn context_bar(percent: u8, width: usize) -> String {
    let percent = percent.min(100);
    let label = format!("{percent}%");
    let cells = width.saturating_sub(label.len() + 1);
    if cells < 3 {
        return label;
    }
    let full = (cells * usize::from(percent) + 50) / 100;
    format!("{}{} {label}", "▓".repeat(full), "░".repeat(cells - full))
}

/// Shown in the right pane when the directory is not inside a repository.
pub const NOT_A_REPOSITORY: &str = "not a git repository";

/// Shown in the right pane when there is nothing to stage or commit.
pub const NO_CHANGES: &str = "no changes";

/// The branch the panes show while `HEAD` is detached.
pub const DETACHED_HEAD: &str = "detached";

/// Title of the right pane: the branch once it is known and, when it has
/// an upstream, the upstream's remote and how the branch stands against
/// it. The longest form that fits `width` columns is used: the remote
/// goes first, then the pane's name. When none fits, the shortest is,
/// for the pane to cut at its end.
pub fn changes_pane_title(
    branch: Option<&str>,
    upstream: Option<&str>,
    ahead_behind: Option<(u32, u32)>,
    width: usize,
) -> String {
    let Some(branch) = branch else {
        return PANE_CHANGES.to_owned();
    };
    let named = format!("{PANE_CHANGES} · {branch}");
    let forms = match upstream {
        Some(upstream) => {
            // The remote is what precedes the first `/`; a local upstream
            // has none and is named whole.
            let remote = upstream
                .split_once('/')
                .map_or(upstream, |(remote, _)| remote);
            let state = upstream_state(ahead_behind);
            vec![
                format!("{named} · {remote} {state}"),
                format!("{named} {state}"),
                format!("{branch} {state}"),
            ]
        }
        None => vec![named, branch.to_owned()],
    };
    let shortest = forms.len() - 1;
    let fits = forms
        .iter()
        .position(|title| title.width() <= width)
        .unwrap_or(shortest);
    forms.into_iter().nth(fits).unwrap_or_default()
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
                "scroll the view; the mouse goes to an agent that asks for it",
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
    fn stack_upstream_and_age_strings() {
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
        let title = |upstream, ahead_behind, width| {
            changes_pane_title(Some("main"), upstream, ahead_behind, width)
        };
        assert_eq!(title(None, None, 80), "Changes · main");
        assert_eq!(
            changes_pane_title(None, Some("origin/main"), None, 80),
            "Changes"
        );
        let ahead = Some((2, 1));
        assert_eq!(
            title(Some("origin/main"), ahead, 29),
            "Changes · main · origin ↑2 ↓1",
            "an exact fit"
        );
        assert_eq!(
            title(Some("upstream/main"), Some((0, 0)), 80),
            "Changes · main · upstream ↑0 ↓0"
        );
        assert_eq!(
            title(Some("main"), ahead, 80),
            "Changes · main · main ↑2 ↓1",
            "a local upstream is named whole"
        );
        // Narrower: the remote goes, then the pane's name; with no room
        // for any, the shortest, for the pane to cut.
        assert_eq!(
            title(Some("origin/main"), ahead, 28),
            "Changes · main ↑2 ↓1"
        );
        assert_eq!(
            title(Some("origin/main"), ahead, 20),
            "Changes · main ↑2 ↓1"
        );
        assert_eq!(title(Some("origin/main"), ahead, 19), "main ↑2 ↓1");
        assert_eq!(title(Some("origin/main"), ahead, 5), "main ↑2 ↓1");
        // A gone upstream says so in every form.
        assert_eq!(
            title(Some("origin/main"), None, 28),
            "Changes · main · origin gone"
        );
        assert_eq!(title(Some("origin/main"), None, 27), "Changes · main gone");
        assert_eq!(title(Some("origin/main"), None, 18), "main gone");
        // Without an upstream the branch outlasts the pane's name; widths
        // are columns, not characters.
        assert_eq!(
            changes_pane_title(Some("feat/agent-box"), None, None, 19),
            "feat/agent-box"
        );
        assert_eq!(
            changes_pane_title(Some("機能"), Some("origin/機能"), ahead, 19),
            "機能 ↑2 ↓1"
        );
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

    #[test]
    fn agent_box_values() {
        assert_eq!(agent_state(State::Working), "✶ working");
        assert_eq!(agent_state(State::Attention), "! asking");
        assert_eq!(agent_running("Edit a.rs"), "✶ Edit a.rs");
        assert_eq!(cost(1.8449), "$1.84");
        assert_eq!(cost(0.0), "$0.00");
        assert_eq!(agent_lines(212, 38), "+212 −38");
        assert_eq!(limit(9, 2 * 3600 + 600), "9% 2h10m");
        assert_eq!(limit(9, 3600 + 5 * 60), "9% 1h05m");
        assert_eq!(limit(9, 3600), "9% 1h00m");
        assert_eq!(limit(30, 4 * 86400 + 3 * 3600), "30% 4d3h");
        assert_eq!(limit(30, 4 * 86400 + 50 * 60), "30% 4d1h", "hours up");
        assert_eq!(limit(30, 86400), "30% 1d0h");
        assert_eq!(limit(30, 86400 - 30), "30% 1d0h", "a minute up");
        assert_eq!(limit(5, 1), "5% 1m", "a minute started counts");
        assert_eq!(limit(100, 59 * 60), "100% 59m");
        assert_eq!(limit(100, 6 * 86400 + 23 * 3600), "100% 6d23h");
        assert_eq!(limit(5, 0), "reset");
        assert_eq!(limit(5, -40), "reset");
        assert_eq!(session_time(Duration::from_secs(42)), "00:42");
        assert_eq!(session_time(Duration::from_secs(61 * 60 + 5)), "1:01:05");
        assert_eq!(context_bar(41, 12), "▓▓▓░░░░░ 41%");
        assert_eq!(context_bar(41, 10), "▓▓░░░░ 41%");
        assert_eq!(context_bar(100, 10), "▓▓▓▓▓ 100%");
        assert_eq!(context_bar(0, 9), "░░░░░░ 0%");
        assert_eq!(context_bar(41, 5), "41%", "no room for a bar");
        assert_eq!(context_bar(250, 8), "▓▓▓ 100%");
        assert_eq!(model_name("Sonnet 5.5 (1M context)", 10), "Sonnet 5.5");
        assert_eq!(
            model_name("Opus 5.5 (1M)", 20),
            "Opus 5.5 (1M)",
            "kept when it fits"
        );
        assert_eq!(model_name("Fable 5.1", 4), "Fable 5.1", "nothing to strip");
    }
}
