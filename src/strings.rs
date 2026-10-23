//! The UI's titles, labels, hints and notices, and the line printed when
//! the agent exits, in English.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use crate::agent::State;
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

/// What a part of a pane's title is, which sets how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitlePart {
    /// The pane's name, or the agent's in the center pane.
    Name,
    /// What the pane shows: the project, the branch.
    Subject,
    /// More about the subject: its upstream, how far back the view is.
    Detail,
    /// Between two parts.
    Separator,
}

/// A pane's title, as its parts in order.
pub type Title = Vec<(TitlePart, String)>;

/// Between the name of a pane's title and its subject, and before a
/// detail that is a word of its own.
const TITLE_SEPARATOR: &str = " · ";

/// A title from its parts.
fn title(parts: &[(TitlePart, &str)]) -> Title {
    parts
        .iter()
        .map(|&(part, text)| (part, text.to_owned()))
        .collect()
}

/// Columns a row's label takes in the agent's section, its trailing
/// space included. Every label is cut to one column under it and padded
/// to it, so a space separates it from the value and the values line up.
pub const AGENT_LABEL_WIDTH: usize = 7;
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

/// Label of the agent section's state row; the section's labels line
/// up.
pub const AGENT_STATE: &str = "state";
/// Label of the agent section's model row.
pub const AGENT_MODEL: &str = "model";
/// Label of the agent section's effort row.
pub const AGENT_EFFORT: &str = "effort";
/// Label of the agent section's context row.
pub const AGENT_CONTEXT: &str = "ctx";
/// Label of the agent section's cost row.
pub const AGENT_COST: &str = "cost";
/// Label of the agent section's row for the plan's five-hour limit.
pub const AGENT_FIVE_HOURS: &str = "5h";
/// Label of the agent section's row for the plan's weekly limit.
pub const AGENT_WEEK: &str = "week";
/// Label of the agent section's row of lines the agent changed.
pub const AGENT_LINES: &str = "lines";
/// Label of the agent section's session time row.
pub const AGENT_TIME: &str = "time";

/// The glyph of what an agent is doing, in the agent's section and the
/// list of sessions.
pub fn state_glyph(state: State) -> &'static str {
    match state {
        State::Working => "✶",
        State::Waiting => "·",
        State::Attention => "!",
    }
}

/// The glyph of a session that is over, in the agent's section and the
/// list of sessions.
pub const ENDED_GLYPH: &str = "×";

/// The state row once the session is over.
pub fn agent_ended() -> String {
    format!("{ENDED_GLYPH} ended")
}

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

/// The word for what an agent is doing.
fn state_word(state: State) -> &'static str {
    match state {
        State::Working => "working",
        State::Waiting => "waiting",
        State::Attention => "asking",
    }
}

/// What the agent is doing, as the agent's section says it: the glyph,
/// then a word.
pub fn agent_state(state: State) -> String {
    format!("{} {}", state_glyph(state), state_word(state))
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
    format!("{} {activity}", state_glyph(State::Working))
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

/// Before [`NO_CHANGES`].
pub const NO_CHANGES_MARK: &str = "✓";

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
) -> Title {
    use TitlePart::{Detail, Name, Separator, Subject};
    let Some(branch) = branch else {
        return title(&[(Name, PANE_CHANGES)]);
    };
    let named = [
        (Name, PANE_CHANGES),
        (Separator, TITLE_SEPARATOR),
        (Subject, branch),
    ];
    let forms = match upstream {
        Some(upstream) => {
            // The remote is what precedes the first `/`; a local upstream
            // has none and is named whole.
            let remote = upstream
                .split_once('/')
                .map_or(upstream, |(remote, _)| remote);
            let state = upstream_state(ahead_behind);
            let far = format!("{remote} {state}");
            vec![
                title(&[&named[..], &[(Separator, TITLE_SEPARATOR), (Detail, &far)]].concat()),
                title(&[&named[..], &[(Separator, " "), (Detail, &state)]].concat()),
                title(&[(Subject, branch), (Separator, " "), (Detail, &state)]),
            ]
        }
        None => vec![title(&named), title(&[(Subject, branch)])],
    };
    let shortest = forms.len() - 1;
    let fits = forms
        .iter()
        .position(|form| form.iter().map(|(_, text)| text.width()).sum::<usize>() <= width)
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

/// Title of the staged list in the changes pane; its length goes flush
/// right.
pub const CHANGES_STAGED: &str = "staged";

/// Title of the unstaged list in the changes pane; its length goes flush
/// right.
pub const CHANGES_UNSTAGED: &str = "unstaged";

/// A count of lines added.
pub fn added(count: u64) -> String {
    format!("+{count}")
}

/// A count of lines removed.
pub fn removed(count: u64) -> String {
    format!("−{count}")
}

/// In place of the lines of a file git did not count.
pub const BINARY: &str = "bin";

/// Marks the selected row of a list, or the session shown in the list
/// of sessions, and the room the others leave.
pub const SELECTED_MARKER: &str = "▸ ";
pub const UNSELECTED_MARKER: &str = "  ";

/// Heading of the current project's path and stack, under the list of
/// sessions.
pub const SESSIONS_CURRENT: &str = "current";

/// Marks a session in the list whose repository has uncommitted changes.
pub const UNCOMMITTED: &str = "•";

/// The status bar's count of the sessions not shown that are asking,
/// while the list of sessions is hidden.
pub fn sessions_asking(count: usize) -> String {
    format!("{} {count} asking", state_glyph(State::Attention))
}

/// Title of the center pane: the agent, the project and, while the view
/// is `scrolled` back, by how many lines.
pub fn agent_pane_title(agent: &str, project: &str, scrolled: usize) -> Title {
    use TitlePart::{Detail, Name, Separator, Subject};
    let named = [
        (Name, agent),
        (Separator, TITLE_SEPARATOR),
        (Subject, project),
    ];
    if scrolled == 0 {
        title(&named)
    } else {
        let back = format!("↑ {scrolled}");
        title(&[&named[..], &[(Separator, " "), (Detail, &back)]].concat())
    }
}

/// Status bar tag while keys go to termis's commands. While they go to the
/// agent, its name takes the place.
pub const MODE_TERM: &str = "TERMIS";

/// Status bar tag while keys go to the commit box.
pub const MODE_COMMIT: &str = "COMMIT";

/// Status bar tag while keys go to the project picker.
pub const MODE_OPEN: &str = "OPEN";

/// Title of the today section in the changes pane.
pub const TODAY_SECTION: &str = "today";

/// The commits since midnight, as the today line counts them; their
/// lines go flush right while there are some.
pub fn today_commits(count: u32) -> String {
    match count {
        0 => "no commits".to_owned(),
        1 => "1 commit".to_owned(),
        count => format!("{count} commits"),
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

/// Title of the project picker.
pub const PICKER_TITLE: &str = "open a project";

/// In the picker's query row while nothing is typed.
pub const PICKER_PLACEHOLDER: &str = "a project's name or a directory";

/// In the picker when no project matches what was typed.
pub const PICKER_EMPTY: &str = "no project matches";

/// Marks the picker's rows whose project has a session open.
pub const PICKER_OPEN: &str = "open";

/// Status bar hints while the project picker is open.
pub fn picker_hints() -> Vec<&'static str> {
    vec![
        "Enter open",
        "Tab new conversation",
        "Up/Down move",
        "Esc back",
    ]
}

/// Toast when the agent's telemetry could not be set up: the agent runs
/// without it, or keeps the status line chained from before.
pub fn telemetry_trouble(reason: &str) -> String {
    format!("agent status unavailable: {reason}")
}

/// Toast when something failed for the session of `project`, which may
/// not be the one shown.
pub fn session_trouble(project: &str, error: &str) -> String {
    format!("{project}: {error}")
}

/// Toast after a first `x` on the session of `project` while its agent
/// is in `state`, working or asking: why the key waits for a second
/// press, and what that press does, which on the `last` session is to
/// quit.
pub fn confirm_close(project: &str, state: State, last: bool) -> String {
    let what = if last { "quit" } else { "close it" };
    format!(
        "{project} is {} · press x again to {what}",
        state_word(state)
    )
}

/// Toast after a first `q` while `busy` agents are working or asking:
/// the one of `project`, in `state`, when it is the only one, else how
/// many; and what a second press does.
pub fn confirm_quit(project: &str, state: State, busy: usize) -> String {
    if busy > 1 {
        format!("{busy} agents are working or asking · press q again to quit")
    } else {
        format!("{project} is {} · press q again to quit", state_word(state))
    }
}

/// Toast when the agent of `project`, in a session not shown, starts
/// asking, with how many `others` started at the same time.
pub fn started_asking(project: &str, others: usize) -> String {
    let glyph = state_glyph(State::Attention);
    if others == 0 {
        format!("{glyph} {project} is asking")
    } else {
        format!("{glyph} {project} and {others} more are asking")
    }
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
            (
                "r / x or q",
                "relaunch / close the session, once the agent ended",
            ),
        ],
    ),
    (
        "termis mode",
        &[
            ("1 / 2 / 3", "highlight a pane"),
            ("Tab / Shift+Tab", "next / previous pane"),
            ("c", "open the commit box"),
            ("z", "zoom / unzoom the center pane"),
            ("o / n / N", "open a project / next / previous session"),
            (
                "r / x / q",
                "relaunch an ended agent / close the session / quit",
            ),
            ("Esc / Ctrl+Q", "back to the agent"),
            ("?", "this list"),
        ],
    ),
    (
        "projects pane",
        &[(
            "j / k / Up / Down / Enter",
            "move the cursor / show its session; x closes the cursor's session",
        )],
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
pub const HINTS_TO_DROP: [&str; 13] = [
    "PgUp/PgDn page",
    "G end",
    "o open",
    "Esc back",
    "n/N session",
    "x close",
    "1/2/3 pane",
    "z zoom",
    "z unzoom",
    "c commit",
    "a all",
    "j/k move",
    "j/k scroll",
];

/// What the keys do once the agent ended, as the agent hints and the
/// ended line say it: `x` and `q` close its session while `others`
/// remain, and `q` quits on the last.
fn ended_keys(others: bool) -> &'static [&'static str] {
    if others {
        &["r relaunch", "x close", "q close"]
    } else {
        &["r relaunch", "q quit"]
    }
}

/// Status bar hints while keys go to the agent, with `others` sessions
/// besides its own or not.
pub fn agent_hints(ended: bool, others: bool) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(4);
    if ended {
        hints.extend_from_slice(ended_keys(others));
    }
    hints.push("Ctrl+Q termis mode");
    hints
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
    /// The list keys of the projects pane.
    Projects,
    /// The scrollback keys of the agent pane.
    Agent,
    /// The list keys of the changes pane.
    Changes,
}

/// Status bar hints in termis mode: the commands that apply, the
/// highlighted pane's own keys first; `o` while it opens the picker,
/// `n`, `N` and `x` while there are other `sessions`.
pub fn term_hints(
    side_panes: bool,
    zoom: Zoom,
    ended: bool,
    pane: PaneKeys,
    open: bool,
    sessions: bool,
) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(12);
    hints.push("? keys");
    match pane {
        PaneKeys::Projects => hints.extend(["j/k move", "Enter show"]),
        PaneKeys::Agent => hints.extend(["j/k scroll", "PgUp/PgDn page", "G end"]),
        PaneKeys::Changes => {
            hints.extend(["j/k move", "s stage", "u unstage", "a all", "c commit"]);
        }
        PaneKeys::None => {}
    }
    if open {
        hints.push("o open");
    }
    if sessions {
        hints.push("n/N session");
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
    if sessions {
        hints.push("x close");
    }
    hints.push("q quit");
    hints.push("Esc back");
    hints
}

/// Right side of the status bar: the project, and where its session is
/// among several, as `2/3`.
pub fn status_project(project: &str, position: Option<(usize, usize)>) -> String {
    match position {
        Some((at, of)) => format!("termis · {project} {at}/{of} "),
        None => format!("termis · {project} "),
    }
}

/// The line drawn over the agent's screen after it ended, with `others`
/// sessions besides its own or not.
pub fn agent_ended_line(agent: &str, code: u32, signal: Option<&str>, others: bool) -> String {
    let keys = ended_keys(others).join(HINT_SEPARATOR);
    match signal {
        Some(signal) => format!(" {agent} was killed by {signal} · {keys} "),
        None => format!(" {agent} exited (code {code}) · {keys} "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_line_names_the_signal_or_the_code() {
        assert_eq!(
            agent_ended_line("claude", 0, Some("SIGKILL"), false),
            " claude was killed by SIGKILL · r relaunch · q quit "
        );
        assert_eq!(
            agent_ended_line("claude", 3, None, false),
            " claude exited (code 3) · r relaunch · q quit "
        );
        // While other sessions remain, x and q close this one.
        assert_eq!(
            agent_ended_line("claude", 3, None, true),
            " claude exited (code 3) · r relaunch · x close · q close "
        );
        assert_eq!(
            agent_ended_line("claude", 0, Some("SIGHUP"), true),
            " claude was killed by SIGHUP · r relaunch · x close · q close "
        );
    }

    #[test]
    fn a_second_press_says_why_it_is_needed_and_what_it_does() {
        assert_eq!(
            confirm_close("api", State::Working, false),
            "api is working · press x again to close it"
        );
        assert_eq!(
            confirm_close("api", State::Attention, true),
            "api is asking · press x again to quit"
        );
        assert_eq!(
            confirm_quit("web", State::Attention, 1),
            "web is asking · press q again to quit"
        );
        assert_eq!(
            confirm_quit("web", State::Working, 3),
            "3 agents are working or asking · press q again to quit"
        );
    }

    #[test]
    fn hints_follow_the_panes_the_zoom_and_the_agent() {
        let joined = |hints: Vec<&str>| hints.join(HINT_SEPARATOR);
        assert_eq!(
            joined(term_hints(
                true,
                Zoom::In,
                false,
                PaneKeys::None,
                true,
                false
            )),
            "? keys · o open · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(
                false,
                Zoom::Out,
                false,
                PaneKeys::None,
                true,
                true
            )),
            "? keys · o open · n/N session · z unzoom · x close · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(
                false,
                Zoom::None,
                true,
                PaneKeys::None,
                false,
                false
            )),
            "? keys · r relaunch · q quit · Esc back",
            "no picker"
        );
        assert_eq!(
            joined(term_hints(
                true,
                Zoom::In,
                false,
                PaneKeys::Changes,
                true,
                false
            )),
            "? keys · j/k move · s stage · u unstage · a all · c commit · o open · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(
                true,
                Zoom::Out,
                false,
                PaneKeys::Agent,
                true,
                true
            )),
            "? keys · j/k scroll · PgUp/PgDn page · G end · o open · n/N session · 1/2/3 pane · z unzoom · x close · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(
                true,
                Zoom::In,
                false,
                PaneKeys::Projects,
                true,
                true
            )),
            "? keys · j/k move · Enter show · o open · n/N session · 1/2/3 pane · z zoom · x close · q quit · Esc back"
        );
        assert_eq!(status_project("termis", None), "termis · termis ");
        assert_eq!(status_project("api", Some((2, 3))), "termis · api 2/3 ");
        assert_eq!(joined(agent_hints(false, false)), "Ctrl+Q termis mode");
        assert_eq!(joined(agent_hints(false, true)), "Ctrl+Q termis mode");
        assert_eq!(
            joined(agent_hints(true, false)),
            "r relaunch · q quit · Ctrl+Q termis mode"
        );
        assert_eq!(
            joined(agent_hints(true, true)),
            "r relaunch · x close · q close · Ctrl+Q termis mode"
        );
        assert_eq!(
            joined(term_hints(true, Zoom::In, true, PaneKeys::None, true, true)),
            "? keys · o open · n/N session · 1/2/3 pane · z zoom · r relaunch · x close · q quit · Esc back"
        );
        assert_eq!(mode_tag(MODE_TERM), " TERMIS ");
        assert_eq!(mode_tag(MODE_COMMIT), " COMMIT ");
        assert_eq!(
            joined(commit_hints()),
            "Enter commit · Ctrl+J body line · Esc cancel"
        );
        assert_eq!(joined(help_hints()), "? or Esc close");
        assert_eq!(mode_tag(MODE_OPEN), " OPEN ");
        assert_eq!(
            joined(picker_hints()),
            "Enter open · Tab new conversation · Up/Down move · Esc back"
        );
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
                term_hints(true, Zoom::In, false, PaneKeys::Changes, true, true).contains(&hint)
                    || term_hints(true, Zoom::Out, false, PaneKeys::Agent, true, true)
                        .contains(&hint),
                "{hint} is a real hint"
            );
        }
    }

    /// A title as one line of text.
    fn text(title: &Title) -> String {
        title.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn the_agent_title_marks_a_scrolled_view() {
        use TitlePart::{Detail, Name, Separator, Subject};
        assert_eq!(
            agent_pane_title("claude", "termis", 0),
            title(&[(Name, "claude"), (Separator, " · "), (Subject, "termis")])
        );
        let scrolled = agent_pane_title("claude", "termis", 120);
        assert_eq!(text(&scrolled), "claude · termis ↑ 120");
        assert_eq!(scrolled[3..], title(&[(Separator, " "), (Detail, "↑ 120")]));
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
            text(&changes_pane_title(
                Some("main"),
                upstream,
                ahead_behind,
                width,
            ))
        };
        assert_eq!(title(None, None, 80), "Changes · main");
        assert_eq!(
            text(&changes_pane_title(None, Some("origin/main"), None, 80)),
            "Changes"
        );
        let ahead = Some((2, 1));
        // The branch is the subject, the upstream a detail, in every form.
        use TitlePart::{Detail, Name, Separator, Subject};
        let parts = |width| {
            changes_pane_title(Some("main"), Some("origin/main"), ahead, width)
                .into_iter()
                .map(|(part, _)| part)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parts(80),
            [Name, Separator, Subject, Separator, Detail],
            "Changes · main · origin ↑2 ↓1"
        );
        assert_eq!(
            parts(20),
            [Name, Separator, Subject, Separator, Detail],
            "Changes · main ↑2 ↓1"
        );
        assert_eq!(
            changes_pane_title(Some("main"), Some("origin/main"), ahead, 10),
            super::title(&[(Subject, "main"), (Separator, " "), (Detail, "↑2 ↓1")])
        );
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
            text(&changes_pane_title(Some("feat/agent-box"), None, None, 19)),
            "feat/agent-box"
        );
        assert_eq!(
            text(&changes_pane_title(
                Some("機能"),
                Some("origin/機能"),
                ahead,
                19
            )),
            "機能 ↑2 ↓1"
        );
        assert_eq!(changes_files(1), "1 file");
        assert_eq!(changes_files(0), "0 files");
        assert_eq!(changes_files(4), "4 files");
        assert_eq!(added(31), "+31");
        assert_eq!(removed(4), "−4");
        assert_eq!(added(0), "+0");
        assert_eq!(today_commits(0), "no commits");
        assert_eq!(today_commits(1), "1 commit");
        assert_eq!(today_commits(3), "3 commits");
    }

    #[test]
    fn agent_section_values() {
        assert_eq!(agent_state(State::Working), "✶ working");
        assert_eq!(agent_state(State::Waiting), "· waiting");
        assert_eq!(agent_state(State::Attention), "! asking");
        assert_eq!(agent_running("Edit a.rs"), "✶ Edit a.rs");
        assert_eq!(agent_ended(), "× ended");
        // The list of sessions shows the same glyphs.
        for (state, glyph) in [
            (State::Working, "✶"),
            (State::Waiting, "·"),
            (State::Attention, "!"),
        ] {
            assert_eq!(state_glyph(state), glyph);
            assert!(agent_state(state).starts_with(glyph));
        }
        assert_eq!(sessions_asking(2), "! 2 asking");
        assert_eq!(started_asking("api", 0), "! api is asking");
        assert_eq!(started_asking("api", 2), "! api and 2 more are asking");
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
