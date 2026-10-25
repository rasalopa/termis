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

/// What a part of a pane's title or of the status bar's right end is,
/// which sets how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitlePart {
    /// The pane's name, the agent's in the center pane, or `termis` at the
    /// status bar's right end.
    Name,
    /// What the pane or the status bar shows: the project, the branch.
    Subject,
    /// More about the subject: its upstream, how far back the view is,
    /// where its session is among several.
    Detail,
    /// Between two parts.
    Separator,
}

/// A pane's title or the status bar's right end, as its parts in order.
pub type Title = Vec<(TitlePart, String)>;

/// Between the name and the subject of a pane's title or of the status
/// bar's right end, and before a detail that is a word of its own.
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
        State::Waiting => "○",
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

/// The word for what an agent is doing; the list of sessions puts the
/// one for asking after the name.
pub fn state_word(state: State) -> &'static str {
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

/// A share in percent: of the context in use, of a usage limit.
pub fn percent(share: u8) -> String {
    format!("{share}%")
}

/// The time left until a usage limit's window resets, rounded up to the
/// last unit shown: minutes under an hour, then hours and minutes under
/// a day, then days and hours; none once it did.
pub fn limit_left(resets_in: i64) -> Option<String> {
    if resets_in <= 0 {
        return None;
    }
    let up_to = |unit: i64| resets_in.saturating_add(unit - 1) / unit;
    let minutes = up_to(60);
    Some(if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    } else {
        let hours = up_to(3600);
        format!("{}d{}h", hours / 24, hours % 24)
    })
}

/// The time left until a usage limit's window resets in its first unit
/// alone, rounded up, for a row too narrow for [`limit_left`]: `5h` for
/// `4h59m`, `7d` for `6d23h`; none once it did.
pub fn limit_left_short(resets_in: i64) -> Option<String> {
    if resets_in <= 0 {
        return None;
    }
    let up_to = |unit: i64| resets_in.saturating_add(unit - 1) / unit;
    let minutes = up_to(60);
    Some(if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", up_to(3600))
    } else {
        format!("{}d", up_to(86400))
    })
}

/// In place of a usage limit whose window reset, until the agent reports
/// again.
pub const LIMIT_RESET: &str = "reset";

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
pub const UNCOMMITTED: &str = "●";

/// The status bar's count of the sessions not shown that are asking,
/// while the list of sessions is hidden.
pub fn sessions_asking(count: usize) -> String {
    format!("{} {count} asking", state_glyph(State::Attention))
}

/// Between two tabs in the center pane's title.
const TAB_SEPARATOR: &str = "   ";

/// A terminal's name: its `shell`'s, with its `number` from 2 on,
/// `zsh 2`.
pub fn terminal_name(shell: &str, number: u32) -> String {
    if number < 2 {
        shell.to_owned()
    } else {
        format!("{shell} {number}")
    }
}

/// Title of the center pane: the agent and the project, which name the
/// agent's tab, then the session's `terminals`, with how many lines the
/// view of the tab shown is `scrolled` back after that tab while it is.
/// The tab shown is the subject, `shown` its index among the terminals
/// or none on the agent's tab, and the others are details. When the
/// title does not fit `width` columns, the terminals not shown go from
/// the end, a count of them in their place at the end; when that still
/// does not fit, the pane cuts it at its end.
pub fn agent_pane_title(
    agent: &str,
    project: &str,
    terminals: &[&str],
    shown: Option<usize>,
    scrolled: usize,
    width: usize,
) -> Title {
    use TitlePart::{Detail, Name, Separator, Subject};
    let back = (scrolled > 0).then(|| format!("↑ {scrolled}"));
    let tab = |title: &mut Title, text: &str, on_screen: bool| {
        title.push((if on_screen { Subject } else { Detail }, text.to_owned()));
        if on_screen && let Some(back) = &back {
            title.extend([(Separator, " ".to_owned()), (Detail, back.clone())]);
        }
    };
    let mut named: Vec<usize> = (0..terminals.len()).collect();
    loop {
        let mut title = vec![
            (Name, agent.to_owned()),
            (Separator, TITLE_SEPARATOR.to_owned()),
        ];
        tab(&mut title, project, shown.is_none());
        for &index in &named {
            title.push((Separator, TAB_SEPARATOR.to_owned()));
            tab(&mut title, terminals[index], shown == Some(index));
        }
        let unnamed = terminals.len() - named.len();
        if unnamed > 0 {
            title.push((Separator, TAB_SEPARATOR.to_owned()));
            title.push((Detail, format!("+{unnamed}")));
        }
        let fits = title.iter().map(|(_, text)| text.width()).sum::<usize>() <= width;
        match named.iter().rposition(|&index| Some(index) != shown) {
            Some(last) if !fits => {
                named.remove(last);
            }
            _ => return title,
        }
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
pub fn commit_hints() -> Vec<Hint> {
    vec![
        ("Enter", "commit"),
        ("Ctrl+J", "body line"),
        ("Esc", "cancel"),
    ]
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
pub fn diff_hints() -> Vec<Hint> {
    vec![
        ("?", "keys"),
        ("j/k", "scroll"),
        ("d / Esc", "back"),
        ("q", "quit"),
    ]
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

/// Before the query in the picker.
pub const PICKER_PROMPT: &str = "› ";

/// Status bar hints while the project picker is open.
pub fn picker_hints() -> Vec<Hint> {
    vec![
        ("Enter", "open"),
        ("Tab", "new conversation"),
        ("Up/Down", "move"),
        ("Esc", "back"),
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

/// What the second press of `x` or `q` ends, as the notice asking for it
/// says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closes {
    /// The terminal it names.
    Terminal,
    /// The session of the terminal it names.
    Session,
    /// `termis`.
    Quit,
}

/// Toast after a first `key`, `x` or `q`, while a program other than the
/// shell runs in terminal `tab` of the session of `project`, and in
/// `others` terminals besides: why the key waits for a second press, and
/// what that press `closes`.
pub fn confirm_program(
    project: &str,
    tab: &str,
    others: usize,
    key: char,
    closes: Closes,
) -> String {
    let what = match closes {
        Closes::Terminal => "close it",
        Closes::Session => "close the session",
        Closes::Quit => "quit",
    };
    if others == 0 {
        format!("{project} · {tab} runs a program · press {key} again to {what}")
    } else {
        format!("{project} · {tab} and {others} more run programs · press {key} again to {what}")
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
pub fn help_hints() -> Vec<Hint> {
    vec![("? or Esc", "close")]
}

/// Every key, under the mode it works in, for the list `?` shows.
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "agent mode",
        &[
            ("Ctrl+Q", "termis mode"),
            ("Ctrl+Z", "reaches a terminal, never the agent"),
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
            ("c / z", "open the commit box / zoom in or out"),
            ("o / n / N", "open a project / next / previous session"),
            ("t / , / .", "open a terminal / previous / next tab"),
            (
                "r / x / q",
                "relaunch an ended agent / close the terminal or the session / quit",
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

/// A hint of the status bar: the keys, then what they do.
pub type Hint = (&'static str, &'static str);

/// A hint as one piece of text: the keys, a space and what they do.
pub fn hint_text((keys, action): Hint) -> String {
    format!("{keys} {action}")
}

/// Between the hints of the status bar.
pub const HINT_SEPARATOR: &str = " · ";

/// The hints a narrow status bar does without, in the order they go.
pub const HINTS_TO_DROP: [Hint; 15] = [
    ("PgUp/PgDn", "page"),
    ("G", "end"),
    ("t", "terminal"),
    ("o", "open"),
    ("Esc", "back"),
    (",/.", "tab"),
    ("n/N", "session"),
    ("x", "close"),
    ("1/2/3", "pane"),
    ("z", "zoom"),
    ("z", "unzoom"),
    ("c", "commit"),
    ("a", "all"),
    ("j/k", "move"),
    ("j/k", "scroll"),
];

/// What the keys do once the agent ended, as the agent hints and the
/// ended line say it: `x` and `q` close its session while `others`
/// remain, and `q` quits on the last.
fn ended_keys(others: bool) -> &'static [Hint] {
    if others {
        &[("r", "relaunch"), ("x", "close"), ("q", "close")]
    } else {
        &[("r", "relaunch"), ("q", "quit")]
    }
}

/// Status bar hints while keys go to the agent, with `others` sessions
/// besides its own or not.
pub fn agent_hints(ended: bool, others: bool) -> Vec<Hint> {
    let mut hints = Vec::with_capacity(4);
    if ended {
        hints.extend_from_slice(ended_keys(others));
    }
    hints.push(("Ctrl+Q", "termis mode"));
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

/// The commands the termis-mode hints offer besides the highlighted
/// pane's own keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offers {
    /// Whether the side panes are shown, for the pane digits.
    pub side_panes: bool,
    /// What the zoom key does.
    pub zoom: Zoom,
    /// Whether the agent shown ended, for its relaunch.
    pub ended: bool,
    /// Whether `o` and `t` open the picker and a terminal.
    pub open: bool,
    /// Whether there are other sessions for `n` and `N`.
    pub sessions: bool,
    /// Whether the session shown has other tabs for `,` and `.`.
    pub tabs: bool,
    /// Whether `x` closes something and leaves `termis` running: the
    /// terminal shown, or a session among others.
    pub close: bool,
}

/// Status bar hints in termis mode: the highlighted `pane`'s own keys
/// first, then the commands that apply, as `offers` says.
pub fn term_hints(pane: PaneKeys, offers: Offers) -> Vec<Hint> {
    let Offers {
        side_panes,
        zoom,
        ended,
        open,
        sessions,
        tabs,
        close,
    } = offers;
    let mut hints = Vec::with_capacity(15);
    hints.push(("?", "keys"));
    match pane {
        PaneKeys::Projects => hints.extend([("j/k", "move"), ("Enter", "show")]),
        PaneKeys::Agent => hints.extend([("j/k", "scroll"), ("PgUp/PgDn", "page"), ("G", "end")]),
        PaneKeys::Changes => hints.extend([
            ("j/k", "move"),
            ("s", "stage"),
            ("u", "unstage"),
            ("a", "all"),
            ("c", "commit"),
        ]),
        PaneKeys::None => {}
    }
    if open {
        hints.extend([("o", "open"), ("t", "terminal")]);
    }
    if sessions {
        hints.push(("n/N", "session"));
    }
    if tabs {
        hints.push((",/.", "tab"));
    }
    if side_panes {
        hints.push(("1/2/3", "pane"));
    }
    match zoom {
        Zoom::In => hints.push(("z", "zoom")),
        Zoom::Out => hints.push(("z", "unzoom")),
        Zoom::None => {}
    }
    if ended {
        hints.push(("r", "relaunch"));
    }
    if close {
        hints.push(("x", "close"));
    }
    hints.push(("q", "quit"));
    hints.push(("Esc", "back"));
    hints
}

/// The right end of the status bar, in parts drawn as a title's are:
/// `termis`, the project, and where its session is among several, as
/// `2/3`.
pub fn status_end(project: &str, position: Option<(usize, usize)>) -> Title {
    use TitlePart::{Detail, Name, Separator, Subject};
    let named = [
        (Name, "termis"),
        (Separator, TITLE_SEPARATOR),
        (Subject, project),
    ];
    match position {
        Some((at, of)) => {
            let position = format!("{at}/{of}");
            title(&[&named[..], &[(Separator, " "), (Detail, &position)]].concat())
        }
        None => title(&named),
    }
}

/// The line drawn over the agent's screen after it ended, with `others`
/// sessions besides its own or not.
pub fn agent_ended_line(agent: &str, code: u32, signal: Option<&str>, others: bool) -> String {
    let keys = ended_keys(others)
        .iter()
        .map(|&hint| hint_text(hint))
        .collect::<Vec<_>>()
        .join(HINT_SEPARATOR);
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
        assert_eq!(
            confirm_program("api", "zsh 2", 0, 'x', Closes::Terminal),
            "api · zsh 2 runs a program · press x again to close it"
        );
        assert_eq!(
            confirm_program("api", "zsh", 0, 'x', Closes::Session),
            "api · zsh runs a program · press x again to close the session"
        );
        assert_eq!(
            confirm_program("api", "zsh", 1, 'x', Closes::Quit),
            "api · zsh and 1 more run programs · press x again to quit"
        );
        assert_eq!(
            confirm_program("web", "bash", 2, 'q', Closes::Quit),
            "web · bash and 2 more run programs · press q again to quit"
        );
    }

    #[test]
    fn hints_follow_the_panes_the_zoom_and_the_agent() {
        let joined = |hints: Vec<Hint>| {
            let texts: Vec<String> = hints.into_iter().map(hint_text).collect();
            texts.join(HINT_SEPARATOR)
        };
        let alone = Offers {
            side_panes: true,
            zoom: Zoom::In,
            ended: false,
            open: true,
            sessions: false,
            tabs: false,
            close: false,
        };
        let several = Offers {
            sessions: true,
            close: true,
            ..alone
        };
        assert_eq!(
            joined(term_hints(PaneKeys::None, alone)),
            "? keys · o open · t terminal · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        let narrow = Offers {
            side_panes: false,
            zoom: Zoom::Out,
            ..several
        };
        assert_eq!(
            joined(term_hints(PaneKeys::None, narrow)),
            "? keys · o open · t terminal · n/N session · z unzoom · x close · q quit · Esc back"
        );
        let spike = Offers {
            side_panes: false,
            zoom: Zoom::None,
            ended: true,
            open: false,
            ..alone
        };
        assert_eq!(
            joined(term_hints(PaneKeys::None, spike)),
            "? keys · r relaunch · q quit · Esc back",
            "no picker, no terminal"
        );
        assert_eq!(
            joined(term_hints(PaneKeys::Changes, alone)),
            "? keys · j/k move · s stage · u unstage · a all · c commit · o open · t terminal · 1/2/3 pane · z zoom · q quit · Esc back"
        );
        let everything = Offers {
            zoom: Zoom::Out,
            tabs: true,
            ..several
        };
        assert_eq!(
            joined(term_hints(PaneKeys::Agent, everything)),
            "? keys · j/k scroll · PgUp/PgDn page · G end · o open · t terminal · n/N session · ,/. tab · 1/2/3 pane · z unzoom · x close · q quit · Esc back"
        );
        assert_eq!(
            joined(term_hints(PaneKeys::Projects, several)),
            "? keys · j/k move · Enter show · o open · t terminal · n/N session · 1/2/3 pane · z zoom · x close · q quit · Esc back"
        );
        // One session: `x` closes the terminal shown, and quits on the
        // agent's tab.
        let terminal = Offers {
            side_panes: false,
            zoom: Zoom::None,
            tabs: true,
            close: true,
            ..alone
        };
        assert_eq!(
            joined(term_hints(PaneKeys::None, terminal)),
            "? keys · o open · t terminal · ,/. tab · x close · q quit · Esc back",
        );
        assert_eq!(
            joined(term_hints(
                PaneKeys::None,
                Offers {
                    close: false,
                    ..terminal
                }
            )),
            "? keys · o open · t terminal · ,/. tab · q quit · Esc back",
        );
        {
            use TitlePart::{Detail, Name, Separator, Subject};
            assert_eq!(
                status_end("api", None),
                title(&[(Name, "termis"), (Separator, " · "), (Subject, "api")])
            );
            let several = status_end("api", Some((2, 3)));
            assert_eq!(text(&several), "termis · api 2/3");
            assert_eq!(several[3..], title(&[(Separator, " "), (Detail, "2/3")]));
        }
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
            joined(term_hints(
                PaneKeys::None,
                Offers {
                    ended: true,
                    ..several
                }
            )),
            "? keys · o open · t terminal · n/N session · 1/2/3 pane · z zoom · r relaunch · x close · q quit · Esc back"
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
            let tabs = Offers {
                tabs: true,
                ..several
            };
            assert!(
                term_hints(PaneKeys::Changes, tabs).contains(&hint)
                    || term_hints(PaneKeys::Agent, everything).contains(&hint),
                "{hint:?} is a real hint"
            );
        }
        assert_eq!(hint_text(("d / Esc", "back")), "d / Esc back");
    }

    /// A title as one line of text.
    fn text(title: &Title) -> String {
        title.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn the_agent_title_marks_a_scrolled_view() {
        use TitlePart::{Detail, Name, Separator, Subject};
        assert_eq!(
            agent_pane_title("claude", "termis", &[], None, 0, 50),
            title(&[(Name, "claude"), (Separator, " · "), (Subject, "termis")])
        );
        let scrolled = agent_pane_title("claude", "termis", &[], None, 120, 50);
        assert_eq!(text(&scrolled), "claude · termis ↑ 120");
        assert_eq!(scrolled[3..], title(&[(Separator, " "), (Detail, "↑ 120")]));
    }

    #[test]
    fn the_agent_title_names_the_tabs_and_marks_the_one_shown() {
        use TitlePart::{Detail, Name, Separator, Subject};
        let terminals = ["zsh", "zsh 2"];
        let parts = |shown, scrolled, width| {
            agent_pane_title("claude", "api", &terminals, shown, scrolled, width)
        };
        let agent = parts(None, 0, 50);
        assert_eq!(text(&agent), "claude · api   zsh   zsh 2");
        assert_eq!(
            agent,
            title(&[
                (Name, "claude"),
                (Separator, " · "),
                (Subject, "api"),
                (Separator, "   "),
                (Detail, "zsh"),
                (Separator, "   "),
                (Detail, "zsh 2"),
            ])
        );
        // A terminal shown is the subject, the agent's tab a detail; the
        // view's place goes after the tab shown.
        let terminal = parts(Some(1), 0, 50);
        assert_eq!(terminal[2], (Detail, "api".to_owned()));
        assert_eq!(terminal[6], (Subject, "zsh 2".to_owned()));
        let back = parts(Some(0), 12, 50);
        assert_eq!(text(&back), "claude · api   zsh ↑ 12   zsh 2");
        assert_eq!(
            back[4..7],
            title(&[(Subject, "zsh"), (Separator, " "), (Detail, "↑ 12")])
        );
        // Too narrow: the terminals not shown go from the end, counted.
        let many = ["zsh", "zsh 2", "zsh 3", "zsh 4"];
        let narrow =
            |shown, width| text(&agent_pane_title("claude", "api", &many, shown, 0, width));
        assert_eq!(
            narrow(None, 42),
            "claude · api   zsh   zsh 2   zsh 3   zsh 4"
        );
        assert_eq!(narrow(None, 41), "claude · api   zsh   zsh 2   zsh 3   +1");
        assert_eq!(narrow(None, 32), "claude · api   zsh   zsh 2   +2");
        assert_eq!(narrow(Some(3), 32), "claude · api   zsh   zsh 4   +2");
        assert_eq!(narrow(Some(0), 20), "claude · api   zsh   +3");
        assert_eq!(
            narrow(Some(0), 5),
            "claude · api   zsh   +3",
            "the shown one stays, for the pane to cut"
        );
        assert_eq!(terminal_name("zsh", 1), "zsh");
        assert_eq!(terminal_name("zsh", 2), "zsh 2");
        assert_eq!(terminal_name("bash", 12), "bash 12");
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
        assert_eq!(agent_state(State::Waiting), "○ waiting");
        assert_eq!(agent_state(State::Attention), "! asking");
        assert_eq!(agent_running("Edit a.rs"), "✶ Edit a.rs");
        assert_eq!(agent_ended(), "× ended");
        // The list of sessions shows the same glyphs.
        for (state, glyph) in [
            (State::Working, "✶"),
            (State::Waiting, "○"),
            (State::Attention, "!"),
        ] {
            assert_eq!(state_glyph(state), glyph);
            assert!(agent_state(state).starts_with(glyph));
        }
        assert_eq!(state_word(State::Attention), "asking");
        assert_eq!(sessions_asking(2), "! 2 asking");
        assert_eq!(started_asking("api", 0), "! api is asking");
        assert_eq!(started_asking("api", 2), "! api and 2 more are asking");
        assert_eq!(cost(1.8449), "$1.84");
        assert_eq!(cost(0.0), "$0.00");
        assert_eq!(percent(9), "9%");
        assert_eq!(percent(100), "100%");
        let left = |resets_in| limit_left(resets_in).expect("time left");
        assert_eq!(left(2 * 3600 + 600), "2h10m");
        assert_eq!(left(3600 + 5 * 60), "1h05m");
        assert_eq!(left(3600), "1h00m");
        assert_eq!(left(4 * 86400 + 3 * 3600), "4d3h");
        assert_eq!(left(4 * 86400 + 50 * 60), "4d1h", "hours up");
        assert_eq!(left(86400), "1d0h");
        assert_eq!(left(86400 - 30), "1d0h", "a minute up");
        assert_eq!(left(1), "1m", "a minute started counts");
        assert_eq!(left(59 * 60), "59m");
        assert_eq!(left(6 * 86400 + 23 * 3600), "6d23h");
        assert_eq!(limit_left(0), None, "reset");
        assert_eq!(limit_left(-40), None);
        let short = |resets_in| limit_left_short(resets_in).expect("time left");
        assert_eq!(short(59 * 60), "59m");
        assert_eq!(short(1), "1m");
        assert_eq!(short(3600), "1h");
        assert_eq!(short(4 * 3600 + 59 * 60), "5h", "hours up");
        assert_eq!(short(23 * 3600 + 30 * 60), "24h");
        assert_eq!(short(86400), "1d");
        assert_eq!(short(6 * 86400 + 23 * 3600), "7d", "days up");
        assert_eq!(limit_left_short(0), None, "reset");
        assert_eq!(limit_left_short(-40), None);
        assert_eq!(session_time(Duration::from_secs(42)), "00:42");
        assert_eq!(session_time(Duration::from_secs(61 * 60 + 5)), "1:01:05");
        assert_eq!(model_name("Sonnet 5.5 (1M context)", 10), "Sonnet 5.5");
        assert_eq!(
            model_name("Opus 5.5 (1M)", 20),
            "Opus 5.5 (1M)",
            "kept when it fits"
        );
        assert_eq!(model_name("Fable 5.1", 4), "Fable 5.1", "nothing to strip");
    }
}
