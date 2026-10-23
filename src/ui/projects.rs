//! The projects pane: from two sessions on, the list of sessions; the
//! current project's path and stack; and, under a heading of its own at
//! the foot of the column, the agent working in it. Git lives in the
//! changes pane.

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::changes::{cut, pad, printable};
use super::palette::{ACCENT, ALERT, ASKING, FAINT, INK, MODIFIED, MUTED, TEXT};
use super::{lines_added, lines_removed};
use crate::agent::{State, Status};
use crate::projects::Stack;
use crate::strings;

/// Where the share of the context in use turns to the asking colour,
/// then to the alert colour, in percent.
const CONTEXT_LEVELS: Levels = Levels {
    warn: 70,
    alert: 90,
};
/// Where the share of a plan's limit used does, in percent.
const LIMIT_LEVELS: Levels = Levels {
    warn: 80,
    alert: 95,
};

/// What the pane shows: the project and its agent.
#[derive(Clone, Copy, Debug)]
pub struct Card<'a> {
    /// The repository root, or the directory the agent runs in outside
    /// one, with the home directory as `~`.
    pub path: &'a str,
    /// What the project is built with, when a manifest said.
    pub stack: Option<&'a Stack>,
    /// Seconds since the Unix epoch, for the time left on the plan's
    /// limits; the changes pane reads it for the last commit's age.
    pub now: i64,
    /// The agent's section, while its telemetry reaches the app.
    pub agent: Option<AgentCard<'a>>,
}

/// What the agent's section shows.
#[derive(Clone, Copy, Debug)]
pub struct AgentCard<'a> {
    /// The agent's name, the section's heading.
    pub name: &'a str,
    /// What its telemetry said last.
    pub status: &'a Status,
    /// How long the session has run by now, or ran until it ended.
    pub time: Option<Duration>,
    /// Whether the session is over.
    pub ended: bool,
}

/// The list of sessions, drawn above the current project from two
/// sessions on.
#[derive(Clone, Copy, Debug)]
pub struct List<'a> {
    /// One row per session, in the order they were opened.
    pub rows: &'a [Row<'a>],
    /// The index of the session shown, which carries the marker.
    pub shown: usize,
    /// The index of the row the list keys act on: kept in view when not
    /// every row fits, its name in the accent while the pane is
    /// highlighted.
    pub cursor: usize,
}

impl List<'_> {
    /// What the list shows of each session besides its name: the glyph,
    /// and whether the dot shows. The app watches it to redraw when it
    /// changes.
    pub fn marks(&self) -> Vec<(Option<&'static str>, bool)> {
        self.rows
            .iter()
            .map(|row| (row.glyph().map(|(glyph, _)| glyph), row.changed))
            .collect()
    }

    /// How many sessions other than the one shown are asking the user
    /// something.
    pub fn asking(&self) -> usize {
        self.rows
            .iter()
            .enumerate()
            .filter(|&(index, row)| index != self.shown && row.asking())
            .count()
    }
}

/// One session in the list.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    /// The session's project.
    pub project: &'a str,
    /// What its agent's telemetry said last; none while the agent runs
    /// without telemetry.
    pub status: Option<&'a Status>,
    /// Whether the session is over.
    pub ended: bool,
    /// Whether its repository has uncommitted changes, as last read.
    pub changed: bool,
}

impl Row<'_> {
    /// What the agent is doing while the session runs with telemetry.
    fn state(&self) -> Option<State> {
        self.status.filter(|_| !self.ended).map(current_state)
    }

    /// Whether the agent is asking the user something.
    fn asking(&self) -> bool {
        self.state() == Some(State::Attention)
    }

    /// The glyph of the row with its style: what the agent is doing, or
    /// that the session ended; none, a blank, for an agent without
    /// telemetry.
    fn glyph(&self) -> Option<(&'static str, Style)> {
        if self.ended {
            return Some((strings::ENDED_GLYPH, Style::new().fg(MUTED)));
        }
        self.state()
            .map(|state| (strings::state_glyph(state), state_style(state)))
    }
}

/// What an agent's telemetry says it is doing: its last event's state,
/// waiting for a prompt before the first.
fn current_state(status: &Status) -> State {
    status.state.unwrap_or(State::Waiting)
}

/// The style of a state: working in the accent, waiting in the
/// terminal's colour, asking in the asking colour and bold.
fn state_style(state: State) -> Style {
    match state {
        State::Working => Style::new().fg(ACCENT),
        State::Waiting => Style::new(),
        State::Attention => Style::new().fg(ASKING).add_modifier(Modifier::BOLD),
    }
}

/// Two shares, in percent, from which a share that is running out takes
/// the asking colour and then the alert colour.
#[derive(Clone, Copy, Debug)]
struct Levels {
    warn: u8,
    alert: u8,
}

impl Levels {
    /// The colour of `share`: `normal` under the first level.
    fn colour(self, share: u8, normal: Color) -> Color {
        if share >= self.alert {
            ALERT
        } else if share >= self.warn {
            ASKING
        } else {
            normal
        }
    }
}

/// A row of the plan's limits as the agent's section shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitRow {
    /// The share of the window used, in percent, and the time left until
    /// it resets, whole and in its first unit alone.
    Left {
        used: u8,
        left: String,
        short: String,
    },
    /// The window reset, and the agent has not reported since.
    Reset,
}

/// The rows of the plan's limits as the agent's section shows them at
/// `now`: the five-hour window, then the week, each while the agent
/// reports it. The app watches them to redraw when they change.
pub fn limit_rows(status: &Status, now: i64) -> [Option<LimitRow>; 2] {
    [status.limits.five_hours, status.limits.week].map(|limit| {
        limit.map(|limit| {
            let resets_in = limit.resets_at.saturating_sub(now);
            match (
                strings::limit_left(resets_in),
                strings::limit_left_short(resets_in),
            ) {
                (Some(left), Some(short)) => LimitRow::Left {
                    used: limit.used,
                    left,
                    short,
                },
                _ => LimitRow::Reset,
            }
        })
    })
}

/// The rows the card keeps when the agent's section takes the rest: the
/// path. A list of sessions keeps one more, for the row of its cursor.
const CARD_MIN_HEIGHT: u16 = 1;

/// Draws the pane's content in `area`, inside the column's margins and
/// padding: the `list` of sessions when there is one, its cursor shown
/// while `focused`, then the card; and while the agent's telemetry
/// reaches the app, the agent's section at the foot of the column: a
/// blank row, a heading with the agent's name and its rows. When rows
/// run short the blank row over the card gives way first, then the
/// stack, then the heading over the card, then the sessions' rows other
/// than the cursor's; then the agent's section loses its last rows down
/// to the state; then it goes.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    card: &Card<'_>,
    list: Option<&List<'_>>,
    focused: bool,
) {
    let width = usize::from(area.width);
    let agent = card
        .agent
        .as_ref()
        .map(|agent| (agent.name, agent_rows(agent, width, card.now)));
    let card_height = CARD_MIN_HEIGHT + u16::from(list.is_some());
    let agent_height = agent.as_ref().map_or(0, |(_, rows)| {
        let wanted = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX);
        let height = wanted.min(area.height.saturating_sub(card_height));
        if height < 3 { 0 } else { height }
    });
    let [top, bottom] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(agent_height)]).areas(area);
    let mut lines = list.map_or_else(Vec::new, |list| list_lines(list, top, focused));
    lines.extend(card_lines(card, top, lines.len()));
    frame.render_widget(Paragraph::new(lines), top);
    if let Some((name, rows)) = agent
        && agent_height > 0
    {
        let mut lines = vec![Line::raw(""), super::heading(name, width)];
        lines.extend(rows);
        frame.render_widget(Paragraph::new(lines), bottom);
    }
}

/// The card's lines for `area`, under `above` lines of the list of
/// sessions: the path, then the stack, muted, when a manifest said and
/// there is room; under a list, a blank row and its heading first when
/// there is room.
fn card_lines(card: &Card<'_>, area: Rect, above: usize) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let room = usize::from(area.height).saturating_sub(above);
    let muted = Style::new().fg(MUTED);
    let mut lines = Vec::new();
    if above > 0 && room >= 4 {
        lines.push(Line::raw(""));
    }
    if above > 0 && room >= 2 {
        lines.push(Line::styled(cut(strings::SESSIONS_CURRENT, width), muted));
    }
    lines.push(Line::raw(fit_path(card.path, width)));
    if let Some(stack) = card.stack
        && (above == 0 || room >= 3)
    {
        lines.push(Line::styled(cut(&strings::stack_line(stack), width), muted));
    }
    lines
}

/// The list's lines for `area`, leaving a row for the path under them:
/// every session's row while they fit, else as many as fit, at least one,
/// from the first, or ending at the cursor's when it would be left out;
/// the name under the cursor in the accent when `focused`.
fn list_lines(list: &List<'_>, area: Rect, focused: bool) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let count = list.rows.len();
    let room = usize::from(area.height)
        .saturating_sub(1)
        .clamp(1, count.max(1));
    let first = (list.cursor + 1)
        .saturating_sub(room)
        .min(count.saturating_sub(room));
    list.rows
        .iter()
        .enumerate()
        .skip(first)
        .take(room)
        .map(|(index, row)| {
            let under_cursor = focused && index == list.cursor;
            session_line(row, index == list.shown, under_cursor, width)
        })
        .collect()
}

/// One session's row in `width` columns: the marker when it is the one
/// `shown`, the glyph of its state, its project's name cut to the room
/// left, and the dot, in the colour of modified files, when its
/// repository has uncommitted changes. The name is muted once the
/// session ended, in the asking colour and bold while its agent asks,
/// with `asking` flush right after it when there is room; bold when it
/// is the one shown; in the accent while `under_cursor`.
fn session_line(row: &Row<'_>, shown: bool, under_cursor: bool, width: usize) -> Line<'static> {
    let marker = if shown {
        strings::SELECTED_MARKER
    } else {
        strings::UNSELECTED_MARKER
    };
    let (glyph, style) = row.glyph().unwrap_or((" ", Style::new()));
    let dot = if row.changed {
        strings::UNCOMMITTED
    } else {
        " "
    };
    // The marker, the glyph and a space, the name, a space and the dot.
    let room = width.saturating_sub(marker.width() + glyph.width() + 1 + 1 + dot.width());
    let name = cut(&printable(row.project), room);
    let asking = row.asking();
    let tag = format!(" {}", strings::state_word(State::Attention));
    let tagged = asking && name.width() + tag.width() <= room;
    let mut text = if row.ended {
        Style::new().fg(MUTED)
    } else if asking {
        Style::new().fg(ASKING).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    if shown {
        text = text.add_modifier(Modifier::BOLD);
    }
    if under_cursor {
        text = text.fg(ACCENT);
    }
    let mut spans = vec![
        Span::styled(marker, Style::new().fg(ACCENT)),
        Span::styled(glyph, style),
        Span::raw(" "),
    ];
    if tagged {
        spans.push(Span::styled(pad(&name, room - tag.width()), text));
        spans.push(Span::styled(tag, Style::new().fg(ASKING)));
    } else {
        spans.push(Span::styled(pad(&name, room), text));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(dot, Style::new().fg(MODIFIED)));
    Line::from(spans)
}

/// The agent's rows at `now`: what it is doing, the tool it runs while
/// there is one, then the rows its telemetry filled: model, effort,
/// context, the plan's limits or else the cost, the lines it changed and
/// time. Before its first event the agent is waiting for a prompt; once
/// the session is over it says so, whatever its last event was.
fn agent_rows(agent: &AgentCard<'_>, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let status = agent.status;
    let room = width.saturating_sub(strings::AGENT_LABEL_WIDTH);
    let state = if agent.ended {
        Span::styled(strings::agent_ended(), Style::new().fg(MUTED))
    } else {
        match (current_state(status), &status.activity) {
            (State::Working, Some(activity)) => Span::styled(
                strings::agent_running(activity),
                state_style(State::Working),
            ),
            (State::Attention, _) => asking_chip(room),
            (state, _) => Span::styled(strings::agent_state(state), state_style(state)),
        }
    };
    lines.push(row(strings::AGENT_STATE, vec![state], width));
    if let Some(model) = &status.model {
        let model = strings::model_name(model, room).to_owned();
        lines.push(row(strings::AGENT_MODEL, vec![Span::raw(model)], width));
    }
    if let Some(effort) = &status.effort {
        let effort = vec![Span::raw(effort.clone())];
        lines.push(row(strings::AGENT_EFFORT, effort, width));
    }
    if let Some(share) = status.context {
        lines.push(row(strings::AGENT_CONTEXT, gauge(share, room), width));
    }
    let [five_hours, week] = limit_rows(status, now);
    for (label, limit) in [
        (strings::AGENT_FIVE_HOURS, five_hours),
        (strings::AGENT_WEEK, week),
    ] {
        if let Some(limit) = limit {
            lines.push(row(label, limit_spans(limit, room), width));
        }
    }
    // On a plan the limits are what runs out; the cost is what usage
    // billing would charge.
    if !status.limits.any()
        && let Some(usd) = status.cost
    {
        let cost = vec![Span::raw(strings::cost(usd))];
        lines.push(row(strings::AGENT_COST, cost, width));
    }
    if let Some((added, removed)) = status.lines {
        let changed = vec![lines_added(added), Span::raw(" "), lines_removed(removed)];
        lines.push(row(strings::AGENT_LINES, changed, width));
    }
    if let Some(time) = agent.time {
        let time = vec![Span::raw(strings::session_time(time))];
        lines.push(row(strings::AGENT_TIME, time, width));
    }
    lines
}

/// The state of an agent that asks, as a chip in the asking colour, with
/// a space on each side while `room` columns allow.
fn asking_chip(room: usize) -> Span<'static> {
    let text = strings::agent_state(State::Attention);
    let chip = Style::new().fg(INK).bg(ASKING).add_modifier(Modifier::BOLD);
    if text.width() + 2 <= room {
        Span::styled(format!(" {text} "), chip)
    } else {
        Span::styled(text, chip)
    }
}

/// The share of the context in use as a gauge of `width` columns: a
/// heavy line as long as the share over a faint one, then the share in
/// bold; both in the accent, in the asking colour from the first of
/// [`CONTEXT_LEVELS`], in the alert colour from the second. The share
/// alone when the gauge would be shorter than three columns.
fn gauge(share: u8, width: usize) -> Vec<Span<'static>> {
    let share = share.min(100);
    let colour = CONTEXT_LEVELS.colour(share, ACCENT);
    let label = strings::percent(share);
    let cells = width.saturating_sub(label.width() + 1);
    let label = Span::styled(label, Style::new().fg(colour).add_modifier(Modifier::BOLD));
    if cells < 3 {
        return vec![label];
    }
    let full = (cells * usize::from(share) + 50) / 100;
    vec![
        Span::styled(
            symbols::line::THICK_HORIZONTAL.repeat(full),
            Style::new().fg(colour),
        ),
        Span::styled(
            symbols::line::HORIZONTAL.repeat(cells - full),
            Style::new().fg(FAINT),
        ),
        Span::raw(" "),
        label,
    ]
}

/// A row of the plan's limits as spans for `room` columns: the share
/// used, in the terminal's colour, in the asking colour from the first of
/// [`LIMIT_LEVELS`] and in the alert colour from the second, then the
/// time left, muted, in its first unit alone when the whole does not fit;
/// `reset` muted once the window reset.
fn limit_spans(limit: LimitRow, room: usize) -> Vec<Span<'static>> {
    let muted = Style::new().fg(MUTED);
    match limit {
        LimitRow::Left { used, left, short } => {
            let share = strings::percent(used);
            // The share, a space and the time left.
            let left = if share.width() + 1 + left.width() <= room {
                left
            } else {
                short
            };
            vec![
                Span::styled(share, Style::new().fg(LIMIT_LEVELS.colour(used, TEXT))),
                Span::styled(format!(" {left}"), muted),
            ]
        }
        LimitRow::Reset => vec![Span::styled(strings::LIMIT_RESET, muted)],
    }
}

/// One row of the agent's section: the muted label, cut to one column
/// under [`strings::AGENT_LABEL_WIDTH`] and padded to it so a space
/// always precedes the value, then the value's spans cut to the room
/// left.
fn row(label: &str, value: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let label = pad(
        &cut(label, strings::AGENT_LABEL_WIDTH - 1),
        strings::AGENT_LABEL_WIDTH,
    );
    let mut room = width.saturating_sub(label.width());
    let mut spans = vec![Span::styled(label, Style::new().fg(MUTED))];
    for span in value {
        let text = printable(&span.content);
        let shown = cut(&text, room);
        let short = shown != text;
        room = room.saturating_sub(shown.width());
        spans.push(Span::styled(shown, span.style));
        if short {
            break;
        }
    }
    Line::from(spans)
}

/// `path` as the card names it: with `home` as `~` when it is inside
/// it, else as it is. A home that is empty or the root is not used.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    let home = home.filter(|home| home.parent().is_some());
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.to_string_lossy()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// `path` in `width` columns: whole when it fits, else its first
/// component, `…` and as many of the last components as fit, else the
/// last component cut.
pub(crate) fn fit_path(path: &str, width: usize) -> String {
    let path = printable(path);
    if path.width() <= width {
        return path;
    }
    let (first, rest) = match path.strip_prefix('/') {
        Some(rest) => ("/", rest),
        None => path
            .split_once('/')
            .map_or((path.as_str(), ""), |(first, rest)| (first, rest)),
    };
    let components: Vec<&str> = rest.split('/').filter(|c| !c.is_empty()).collect();
    let last = components.last().copied().unwrap_or(first);
    // The first component, "/…/" or "…/", then a tail of components.
    let head = if first == "/" {
        "/…/".to_owned()
    } else {
        format!("{first}/…/")
    };
    for start in 1..components.len() {
        let tail = components[start..].join("/");
        let candidate = format!("{head}{tail}");
        if candidate.width() <= width {
            return candidate;
        }
    }
    cut(last, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Limit, Limits};
    use crate::projects::Framework;
    use crate::ui::palette::{ADDED, REMOVED};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn stack() -> Stack {
        Stack {
            language: "Rust",
            framework: Some(Framework {
                name: "ratatui",
                version: Some("0.30".into()),
            }),
        }
    }

    /// Draws the pane's content in `width` by `height` cells.
    fn column(width: u16, height: u16, card: &Card<'_>, focused: bool) -> Terminal<TestBackend> {
        listed(width, height, card, None, focused)
    }

    /// Draws the pane's content with `list` in `width` by `height`
    /// cells.
    fn listed(
        width: u16,
        height: u16,
        card: &Card<'_>,
        list: Option<&List<'_>>,
        focused: bool,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), card, list, focused))
            .expect("draw");
        terminal
    }

    /// Draws the pane's content, with no list and no highlight, in `width`
    /// by `height` cells.
    fn draw(width: u16, height: u16, card: &Card<'_>) -> Terminal<TestBackend> {
        column(width, height, card, false)
    }

    /// Row `y` of the content.
    fn line(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    /// Every row of the content.
    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        (0..terminal.backend().buffer().area.height)
            .map(|y| line(terminal, y))
            .collect()
    }

    /// The colour of the cell at `x`, `y` of the content.
    fn cell_fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.fg)
    }

    /// The background of the cell at `x`, `y` of the content.
    fn cell_bg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.bg)
    }

    /// Whether the cell at `x`, `y` of the content is bold.
    fn bold(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> bool {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::BOLD))
    }

    #[test]
    fn the_card_shows_the_path_and_the_stack() {
        let stack = stack();
        let card = Card {
            path: "~/WebProjects/Personal/termis",
            stack: Some(&stack),
            now: 0,
            agent: None,
        };
        let terminal = draw(22, 3, &card);
        assert_eq!(line(&terminal, 0), "~/…/Personal/termis   ");
        assert_eq!(line(&terminal, 1), "Rust · ratatui 0.30   ");
        assert_eq!(line(&terminal, 2).trim(), "", "nothing of git");
        // The path in the terminal's colour, the stack muted.
        assert_eq!(cell_fg(&terminal, 0, 0), Color::Reset);
        assert_eq!(cell_fg(&terminal, 0, 1), MUTED);
        // Narrow, the stack is cut; with one row, it gives way.
        let terminal = draw(14, 2, &card);
        assert_eq!(line(&terminal, 0), "~/…/termis    ");
        assert_eq!(line(&terminal, 1), "Rust · ratatu…");
        assert_eq!(line(&draw(14, 1, &card), 0), "~/…/termis    ");
        let bare = Card {
            stack: None,
            ..card
        };
        assert_eq!(line(&draw(14, 2, &bare), 1).trim(), "");
        draw(0, 0, &card);
        draw(3, 1, &card);
    }

    #[test]
    fn the_home_directory_is_a_tilde() {
        let home = Path::new("/Users/x");
        assert_eq!(
            display_path(Path::new("/Users/x/dev/t"), Some(home)),
            "~/dev/t"
        );
        assert_eq!(display_path(Path::new("/Users/x"), Some(home)), "~");
        assert_eq!(
            display_path(Path::new("/Users/xy/t"), Some(home)),
            "/Users/xy/t"
        );
        assert_eq!(display_path(Path::new("/opt/t"), None), "/opt/t");
        assert_eq!(
            display_path(Path::new("/a/b"), Some(Path::new(""))),
            "/a/b",
            "an empty home"
        );
        assert_eq!(
            display_path(Path::new("/a/b"), Some(Path::new("/"))),
            "/a/b",
            "the root as home"
        );
    }

    #[test]
    fn paths_keep_their_ends() {
        assert_eq!(fit_path("~/a/b/c", 10), "~/a/b/c");
        assert_eq!(
            fit_path("~/WebProjects/Personal/termis", 20),
            "~/…/Personal/termis"
        );
        assert_eq!(fit_path("~/WebProjects/Personal/termis", 12), "~/…/termis");
        assert_eq!(fit_path("~/WebProjects/Personal/termis", 8), "termis");
        assert_eq!(fit_path("~/WebProjects/Personal/termis", 4), "ter…");
        assert_eq!(fit_path("/Users/x/very-long-name", 12), "very-long-n…");
        assert_eq!(fit_path("/Users/x/dev", 9), "/…/x/dev");
        assert_eq!(fit_path("single", 3), "si…");
        assert_eq!(fit_path("a\tb", 5), "a\u{FFFD}b");
        assert_eq!(fit_path("", 5), "");
    }

    #[test]
    fn the_agent_section_shows_what_the_telemetry_said() {
        let status = Status {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            context: Some(41),
            cost: Some(1.844),
            limits: Limits::default(),
            lines: Some((12, 3)),
            duration: Some(Duration::from_secs(42)),
            state: Some(State::Working),
            activity: None,
        };
        let card = Card {
            path: "~/p",
            stack: None,
            now: 0,
            agent: Some(AgentCard {
                name: "claude",
                status: &status,
                time: Some(Duration::from_secs(42)),
                ended: false,
            }),
        };
        let terminal = column(20, 12, &card, false);
        assert_eq!(
            lines(&terminal),
            [
                "~/p                 ",
                "                    ",
                "                    ",
                "                    ",
                "claude ─────────────",
                "state  ✶ working    ",
                "model  Opus 5.5     ",
                "effort xhigh        ",
                "ctx    ━━━━───── 41%",
                "cost   $1.84        ",
                "lines  +12 −3       ",
                "time   00:42        ",
            ]
        );
        assert_eq!(cell_fg(&terminal, 7, 5), ACCENT, "working in the accent");
        assert_eq!(cell_fg(&terminal, 0, 5), MUTED, "the label");
        // The heading is muted, its rule faint; no box.
        assert_eq!(cell_fg(&terminal, 0, 4), MUTED);
        assert_eq!(cell_fg(&terminal, 7, 4), FAINT);
        // The gauge: the share in use over a faint track, the share in
        // bold.
        assert_eq!(cell_fg(&terminal, 7, 8), ACCENT);
        assert_eq!(cell_fg(&terminal, 11, 8), FAINT);
        assert_eq!(cell_fg(&terminal, 17, 8), ACCENT);
        assert!(bold(&terminal, 17, 8));
        // The lines in their colours.
        assert_eq!(cell_fg(&terminal, 7, 10), ADDED);
        assert_eq!(cell_fg(&terminal, 11, 10), REMOVED);
        // The highlight is the pane's frame, drawn around this content.
        assert_eq!(lines(&column(20, 12, &card, true)), lines(&terminal));

        // Asking stands out, as a chip; before any telemetry only the state
        // shows, as waiting for a prompt, and the section is that short.
        let asking = Status {
            state: Some(State::Attention),
            ..Status::default()
        };
        let card = Card {
            agent: Some(AgentCard {
                name: "claude",
                status: &asking,
                time: None,
                ended: false,
            }),
            ..card
        };
        let terminal = column(18, 6, &card, false);
        assert_eq!(line(&terminal, 3).trim(), "");
        assert_eq!(line(&terminal, 4), "claude ───────────");
        assert_eq!(line(&terminal, 5), "state   ! asking  ");
        for x in 7..17 {
            assert_eq!(cell_bg(&terminal, x, 5), ASKING, "the chip, column {x}");
            assert_eq!(cell_fg(&terminal, x, 5), INK);
            assert!(bold(&terminal, x, 5));
        }
        assert_eq!(cell_bg(&terminal, 17, 5), Color::Reset, "past the chip");
        // Narrower, the chip loses its padding before its text.
        let terminal = column(16, 6, &card, false);
        assert_eq!(line(&terminal, 5), "state  ! asking ");
        assert_eq!(cell_bg(&terminal, 7, 5), ASKING);
        assert_eq!(cell_bg(&terminal, 15, 5), Color::Reset);
        let empty = Status::default();
        let card = Card {
            agent: Some(AgentCard {
                name: "claude",
                status: &empty,
                time: None,
                ended: false,
            }),
            ..card
        };
        let terminal = column(18, 6, &card, false);
        assert_eq!(line(&terminal, 5), "state  ○ waiting  ");
        assert_eq!(
            cell_fg(&terminal, 7, 5),
            Color::Reset,
            "the terminal's colour"
        );
        // While it runs a tool, the tool takes the place of `working`.
        let running = Status {
            state: Some(State::Working),
            activity: Some("Edit projects.rs".into()),
            ..Status::default()
        };
        let card = Card {
            agent: Some(AgentCard {
                name: "claude",
                status: &running,
                time: None,
                ended: false,
            }),
            ..card
        };
        let terminal = column(18, 6, &card, false);
        assert_eq!(line(&terminal, 5), "state  ✶ Edit pro…");
        assert_eq!(cell_fg(&terminal, 7, 5), ACCENT);
        for (w, h) in [(0, 0), (1, 1), (3, 2), (6, 3), (2, 6), (4, 8)] {
            column(w, h, &card, false);
        }
    }

    #[test]
    fn a_plans_limits_take_the_place_of_the_cost() {
        let now = 1_000_000;
        let status = Status {
            cost: Some(1.844),
            limits: Limits {
                five_hours: Some(Limit {
                    used: 9,
                    resets_at: now + 2 * 3600 + 600,
                }),
                week: Some(Limit {
                    used: 30,
                    resets_at: now + 4 * 86400 + 3 * 3600,
                }),
            },
            ..Status::default()
        };
        let card = Card {
            path: "~/p",
            stack: None,
            now,
            agent: Some(AgentCard {
                name: "claude",
                status: &status,
                time: None,
                ended: false,
            }),
        };
        let terminal = column(20, 6, &card, false);
        assert_eq!(
            lines(&terminal),
            [
                "~/p                 ",
                "                    ",
                "claude ─────────────",
                "state  ○ waiting    ",
                "5h     9% 2h10m     ",
                "week   30% 4d3h     ",
            ]
        );
        // The share in the terminal's colour, the time left muted.
        assert_eq!(cell_fg(&terminal, 7, 4), Color::Reset);
        assert_eq!(cell_fg(&terminal, 10, 4), MUTED);
        // At the real width the value has nine columns: enough for the
        // time left but for the longest, which shows in its first unit
        // alone, rounded up.
        let narrow = Status {
            limits: Limits {
                five_hours: Some(Limit {
                    used: 23,
                    resets_at: now + 3 * 3600 + 12 * 60,
                }),
                week: Some(Limit {
                    used: 100,
                    resets_at: now + 6 * 86400 + 23 * 3600,
                }),
            },
            ..Status::default()
        };
        let real = Card {
            agent: Some(AgentCard {
                status: &narrow,
                ..card.agent.expect("agent")
            }),
            ..card
        };
        let terminal = column(16, 6, &real, false);
        assert_eq!(line(&terminal, 4), "5h     23% 3h12m");
        assert_eq!(line(&terminal, 5), "week   100% 7d  ");
        assert_eq!(cell_fg(&terminal, 7, 5), ALERT, "a limit used up");
        assert_eq!(cell_fg(&terminal, 12, 5), MUTED);
        let full = |room| {
            let spans = limit_spans(
                LimitRow::Left {
                    used: 100,
                    left: "4h59m".into(),
                    short: "5h".into(),
                },
                room,
            );
            spans[1].content.to_string()
        };
        assert_eq!(full(9), " 5h");
        assert_eq!(full(10), " 4h59m", "the whole once it fits");
        let reset = now + 2 * 3600 + 600;
        assert_eq!(
            limit_rows(&status, reset),
            [
                Some(LimitRow::Reset),
                Some(LimitRow::Left {
                    used: 30,
                    left: "4d1h".into(),
                    short: "5d".into(),
                }),
            ],
            "the five hours ran out"
        );
        let after = Card { now: reset, ..card };
        let terminal = column(20, 6, &after, false);
        assert_eq!(line(&terminal, 4), "5h     reset        ");
        assert_eq!(cell_fg(&terminal, 7, 4), MUTED);
        // Without them the cost shows; with one of them it does not.
        let billed = Status {
            limits: Limits::default(),
            ..status.clone()
        };
        let card = Card {
            agent: Some(AgentCard {
                status: &billed,
                ..card.agent.expect("agent")
            }),
            ..card
        };
        let terminal = column(20, 6, &card, false);
        assert_eq!(line(&terminal, 4), "state  ○ waiting    ");
        assert_eq!(line(&terminal, 5), "cost   $1.84        ");
        assert_eq!(limit_rows(&billed, now), [None, None]);
        let week_only = Status {
            limits: Limits {
                five_hours: None,
                ..status.limits
            },
            ..status.clone()
        };
        let card = Card {
            agent: Some(AgentCard {
                status: &week_only,
                ..card.agent.expect("agent")
            }),
            ..card
        };
        let terminal = column(20, 6, &card, false);
        assert_eq!(line(&terminal, 5), "week   30% 4d3h     ");
        assert!(
            lines(&terminal).iter().all(|row| !row.contains("cost")),
            "no cost row"
        );
    }

    #[test]
    fn shares_running_out_take_the_asking_then_the_alert_colour() {
        let used = |used| {
            let spans = limit_spans(
                LimitRow::Left {
                    used,
                    left: "1m".into(),
                    short: "1m".into(),
                },
                9,
            );
            spans[0].style.fg
        };
        assert_eq!(used(79), Some(Color::Reset));
        assert_eq!(used(80), Some(ASKING));
        assert_eq!(used(94), Some(ASKING));
        assert_eq!(used(95), Some(ALERT));
        let context = |share| gauge(share, 13)[0].style.fg;
        assert_eq!(context(69), Some(ACCENT));
        assert_eq!(context(70), Some(ASKING));
        assert_eq!(context(89), Some(ASKING));
        assert_eq!(context(90), Some(ALERT));
        // The gauge's track stays faint and its share takes the colour.
        let full = gauge(95, 13);
        assert_eq!(full[1].style.fg, Some(FAINT));
        assert_eq!(full[3].content, "95%");
        assert_eq!(full[3].style.fg, Some(ALERT));
        // A share past the whole is the whole; with no room for a gauge,
        // the share alone.
        assert_eq!(
            gauge(250, 13)
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "━━━━━━━━ 100%"
        );
        let alone = gauge(41, 6);
        assert_eq!(alone.len(), 1);
        assert_eq!(alone[0].content, "41%");
        assert_eq!(
            gauge(0, 9)
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "────── 0%"
        );
    }

    #[test]
    fn the_agent_section_at_the_real_width_and_when_rows_run_short() {
        let status = Status {
            model: Some("Sonnet 5.5 (1M context)".into()),
            effort: None,
            context: Some(41),
            cost: Some(1.845),
            limits: Limits::default(),
            lines: None,
            duration: None,
            state: Some(State::Attention),
            activity: None,
        };
        let stack = stack();
        let agent = AgentCard {
            name: "claude",
            status: &status,
            time: Some(Duration::from_secs(3725)),
            ended: false,
        };
        let card = Card {
            path: "~/p",
            stack: Some(&stack),
            now: 0,
            agent: Some(agent),
        };
        // 16 columns: the left pane of a 100-column terminal inside its
        // margins and padding. The agent's section sits at the foot of
        // the column, the card at its top.
        let terminal = column(16, 30, &card, false);
        let shown = lines(&terminal);
        assert_eq!(
            shown[..2],
            ["~/p             ", "Rust · ratatui …"],
            "the card"
        );
        assert_eq!(
            shown[23..],
            [
                "                ",
                "claude ─────────",
                "state  ! asking ",
                "model  Sonnet 5…",
                "ctx    ━━─── 41%",
                "cost   $1.84    ",
                "time   1:02:05  ",
            ],
            "the suffix of the model goes before the name is cut"
        );
        assert_eq!(cell_fg(&terminal, 0, 1), MUTED, "the stack");

        // Eight rows: the stack gives way first.
        let terminal = column(16, 8, &card, false);
        let shown = lines(&terminal);
        assert!(shown.iter().all(|row| !row.contains("Rust")), "{shown:?}");
        assert_eq!(shown[0], "~/p             ");
        assert_eq!(shown[2], "claude ─────────");
        assert_eq!(shown[7], "time   1:02:05  ");
        // Seven rows: the agent's section loses its last row.
        let terminal = column(16, 7, &card, false);
        let shown = lines(&terminal);
        assert_eq!(shown[0], "~/p             ");
        assert_eq!(shown[2], "claude ─────────");
        assert_eq!(shown[6], "cost   $1.84    ", "no time row");
        // Four rows: the card keeps its path, the agent its state.
        let terminal = column(16, 4, &card, false);
        assert_eq!(
            lines(&terminal),
            [
                "~/p             ",
                "                ",
                "claude ─────────",
                "state  ! asking ",
            ]
        );
        // Three rows: no room for the state, so no agent's section.
        let terminal = column(16, 3, &card, false);
        let shown = lines(&terminal);
        assert!(shown.iter().all(|row| !row.contains("claude")), "{shown:?}");
        assert_eq!(shown[0], "~/p             ");

        // An ended session says so, whatever its last event was.
        let over = Card {
            agent: Some(AgentCard {
                ended: true,
                ..agent
            }),
            ..card
        };
        let terminal = column(16, 30, &over, false);
        assert_eq!(line(&terminal, 25), "state  × ended  ");
        assert_eq!(cell_fg(&terminal, 7, 25), MUTED);
        assert_eq!(cell_bg(&terminal, 7, 25), Color::Reset, "no chip");
    }

    /// A session in `project` without telemetry, running, its repository
    /// clean.
    fn session(project: &str) -> Row<'_> {
        Row {
            project,
            status: None,
            ended: false,
            changed: false,
        }
    }

    #[test]
    fn the_list_shows_each_session_with_its_state_and_dot() {
        let working = Status {
            state: Some(State::Working),
            ..Status::default()
        };
        let asking = Status {
            state: Some(State::Attention),
            ..Status::default()
        };
        let quiet = Status::default();
        let rows = [
            Row {
                status: Some(&working),
                changed: true,
                ..session("termis")
            },
            Row {
                status: Some(&asking),
                ..session("api")
            },
            Row {
                status: Some(&quiet),
                changed: true,
                ..session("a-long-project-name")
            },
            Row {
                status: Some(&asking),
                ended: true,
                ..session("web")
            },
            session("no\ttelemetry"),
        ];
        let list = List {
            rows: &rows,
            shown: 0,
            cursor: 0,
        };
        let stack = stack();
        let card = Card {
            path: "~/dev/api",
            stack: Some(&stack),
            now: 0,
            agent: None,
        };
        let terminal = listed(18, 9, &card, Some(&list), false);
        assert_eq!(
            lines(&terminal),
            [
                "▸ ✶ termis       ●",
                "  ! api   asking  ",
                "  ○ a-long-proj… ●",
                "  × web           ",
                "    no\u{FFFD}telemetry  ",
                "                  ",
                "current           ",
                "~/dev/api         ",
                "Rust · ratatui 0.…",
            ]
        );
        // The glyphs take the state's colour: working in the accent,
        // asking in the asking colour and bold, waiting in the terminal's,
        // ended muted. The marker is in the accent.
        assert_eq!(cell_fg(&terminal, 2, 0), ACCENT);
        assert_eq!(cell_fg(&terminal, 2, 1), ASKING);
        assert!(bold(&terminal, 2, 1));
        assert_eq!(cell_fg(&terminal, 2, 2), Color::Reset);
        assert_eq!(cell_fg(&terminal, 2, 3), MUTED);
        assert_eq!(cell_fg(&terminal, 0, 0), ACCENT);
        // The names: the one shown bold, one asking bold in the asking
        // colour with `asking` after it, one that ended muted.
        assert_eq!(cell_fg(&terminal, 4, 0), Color::Reset);
        assert!(bold(&terminal, 4, 0));
        assert_eq!(cell_fg(&terminal, 4, 1), ASKING);
        assert!(bold(&terminal, 4, 1));
        assert_eq!(cell_fg(&terminal, 10, 1), ASKING);
        assert!(!bold(&terminal, 10, 1), "the tag");
        assert_eq!(cell_fg(&terminal, 4, 2), Color::Reset);
        assert!(!bold(&terminal, 4, 2));
        assert_eq!(cell_fg(&terminal, 4, 3), MUTED);
        // The dot in the colour of modified files; under the list, the
        // heading and the stack muted.
        assert_eq!(cell_fg(&terminal, 17, 0), MODIFIED);
        assert_eq!(cell_fg(&terminal, 0, 6), MUTED);
        assert_eq!(cell_fg(&terminal, 0, 7), Color::Reset);
        assert_eq!(cell_fg(&terminal, 0, 8), MUTED);
        // Without room for it after the name, the tag goes.
        let long = Row {
            status: Some(&asking),
            ..session("a-long-project")
        };
        assert_eq!(
            session_line(&long, false, false, 18).to_string(),
            "  ! a-long-proj…  "
        );
        // What the app watches: each one's glyph and dot, and how many of
        // the others ask; one that ended asks nothing.
        assert_eq!(
            list.marks(),
            [
                (Some("✶"), true),
                (Some("!"), false),
                (Some("○"), true),
                (Some("×"), false),
                (None, false),
            ]
        );
        assert_eq!(list.asking(), 1);
        let other = List { shown: 1, ..list };
        assert_eq!(other.asking(), 0, "the one shown is not counted");
    }

    #[test]
    fn under_a_list_the_card_and_then_the_rows_give_way() {
        let asking = Status {
            state: Some(State::Attention),
            duration: Some(Duration::from_secs(1)),
            ..Status::default()
        };
        let rows = [session("one"), session("two"), session("three")];
        let list = List {
            rows: &rows,
            shown: 2,
            cursor: 2,
        };
        let stack = stack();
        let card = Card {
            path: "~/p",
            stack: Some(&stack),
            now: 0,
            agent: Some(AgentCard {
                name: "claude",
                status: &asking,
                time: Some(Duration::from_secs(1)),
                ended: false,
            }),
        };
        // The rows over the agent's section, at each height of the
        // column; the section's four rows sit under them.
        let card_rows = |height: u16| -> Vec<String> {
            let terminal = listed(18, height, &card, Some(&list), false);
            let shown = lines(&terminal);
            let section = shown
                .iter()
                .position(|row| row.starts_with("claude ─"))
                .map_or(shown.len(), |heading| heading - 1);
            shown[..section]
                .iter()
                .map(|row| row.trim_end().to_owned())
                .collect()
        };
        let all = [
            "    one",
            "    two",
            "▸   three",
            "",
            "current",
            "~/p",
            "Rust · ratatui 0.…",
        ];
        assert_eq!(card_rows(11), all);
        // The blank row goes first, then the stack, then the heading.
        assert_eq!(card_rows(10), [&all[..3], &all[4..]].concat());
        assert_eq!(card_rows(9), [&all[..3], &all[4..6]].concat());
        assert_eq!(card_rows(8), ["    one", "    two", "▸   three", "~/p"]);
        // Then the other rows, the list keeping the shown one in view.
        assert_eq!(card_rows(7), ["    two", "▸   three", "~/p"]);
        assert_eq!(card_rows(6), ["▸   three", "~/p"]);
        // Then the agent's section loses its rows, and goes.
        let terminal = listed(18, 5, &card, Some(&list), false);
        assert_eq!(
            lines(&terminal),
            [
                "▸   three         ",
                "~/p               ",
                "                  ",
                "claude ───────────",
                "state   ! asking  ",
            ],
            "no time row"
        );
        assert_eq!(card_rows(4), ["    one", "    two", "▸   three", "~/p"]);
        // The first session shown keeps the list from the top.
        let first = List {
            shown: 0,
            cursor: 0,
            ..list
        };
        let terminal = listed(18, 7, &card, Some(&first), false);
        assert_eq!(line(&terminal, 0), "▸   one           ");
        assert_eq!(line(&terminal, 1), "    two           ");
        assert_eq!(line(&terminal, 2), "~/p               ");
        for (w, h) in [(0, 0), (1, 1), (3, 2), (6, 3), (2, 6), (4, 8), (9, 4)] {
            listed(w, h, &card, Some(&list), false);
        }
    }

    #[test]
    fn the_cursor_shows_while_the_pane_is_highlighted_and_stays_in_view() {
        let rows = [session("one"), session("two"), session("three")];
        let list = List {
            rows: &rows,
            shown: 2,
            cursor: 0,
        };
        let card = Card {
            path: "~/p",
            stack: None,
            now: 0,
            agent: None,
        };
        let terminal = listed(18, 5, &card, Some(&list), true);
        assert_eq!(
            lines(&terminal),
            [
                "    one           ",
                "    two           ",
                "▸   three         ",
                "current           ",
                "~/p               ",
            ]
        );
        // The name under the cursor is in the accent; the marker stays on
        // the session shown, whose name is bold.
        assert_eq!(cell_fg(&terminal, 4, 0), ACCENT);
        assert!(!bold(&terminal, 4, 0));
        assert_eq!(cell_fg(&terminal, 4, 2), Color::Reset);
        assert!(bold(&terminal, 4, 2));
        let terminal = listed(18, 5, &card, Some(&list), false);
        assert_eq!(cell_fg(&terminal, 4, 0), Color::Reset, "not highlighted");
        // Short of rows, the list keeps the cursor in view.
        let terminal = listed(18, 2, &card, Some(&list), true);
        assert_eq!(
            lines(&terminal),
            ["    one           ", "~/p               "]
        );
        let moved = List { cursor: 1, ..list };
        let terminal = listed(18, 2, &card, Some(&moved), true);
        assert_eq!(line(&terminal, 0), "    two           ");
        // The dot is not in the accent under the cursor.
        let rows = [
            Row {
                changed: true,
                ..session("one")
            },
            session("two"),
        ];
        let changed = List {
            rows: &rows,
            shown: 1,
            cursor: 0,
        };
        let terminal = listed(18, 5, &card, Some(&changed), true);
        assert_eq!(line(&terminal, 0), "    one          ●");
        assert_eq!(cell_fg(&terminal, 4, 0), ACCENT);
        assert_eq!(cell_fg(&terminal, 17, 0), MODIFIED, "the dot");
    }
}
