//! The projects pane: a card for the current repository and, in a box of
//! its own at the foot of the column, the agent working in it.

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::changes::{cut, pad, printable};
use super::{ACCENT, ALERT, DIM};
use crate::agent::{State, Status};
use crate::git::worker::Snapshot;
use crate::projects::Stack;
use crate::strings;

/// What the card shows besides the repository as last read.
#[derive(Clone, Copy, Debug)]
pub struct Card<'a> {
    /// The repository root, or the directory the agent runs in outside
    /// one, with the home directory as `~`.
    pub path: &'a str,
    /// What the project is built with, when a manifest said.
    pub stack: Option<&'a Stack>,
    /// Seconds since the Unix epoch, for the age of the last commit.
    pub now: i64,
    /// The agent's box, while its telemetry reaches the app.
    pub agent: Option<AgentCard<'a>>,
}

/// What the agent's box shows.
#[derive(Clone, Copy, Debug)]
pub struct AgentCard<'a> {
    /// The agent's name, the box's title.
    pub name: &'a str,
    /// What its telemetry said last.
    pub status: &'a Status,
    /// How long the session has run by now, or ran until it ended.
    pub time: Option<Duration>,
    /// Whether the session is over.
    pub ended: bool,
}

/// The rows of the plan's limits as the agent's box shows them at `now`:
/// the five-hour window, then the week, each while the agent reports it.
/// The app watches them to redraw when they change.
pub fn limit_rows(status: &Status, now: i64) -> [Option<String>; 2] {
    [status.limits.five_hours, status.limits.week].map(|limit| {
        limit.map(|limit| strings::limit(limit.used, limit.resets_at.saturating_sub(now)))
    })
}

/// The age of the last commit as the card shows it at `now`, while the
/// snapshot has one. The app watches it to redraw when it changes.
pub fn last_age(snapshot: Option<&Snapshot>, now: i64) -> Option<String> {
    let last = snapshot?.last.as_ref().ok()?.as_ref()?;
    Some(strings::age(now.saturating_sub(last.time)))
}

/// The rows the card's box keeps when the agent's box takes the rest:
/// its borders and the path.
const CARD_MIN_HEIGHT: u16 = 3;

/// Draws the pane in `area`, the whole left column: the card in a box
/// titled Projects, highlighted when `focused`, and while the agent's
/// telemetry reaches the app, the agent in a box at the foot of the
/// column, as tall as its rows. When rows run short the card gives way
/// first, down to its path, see [`card_lines`]; then the agent's box
/// loses its last rows down to the state; then it goes.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    card: &Card<'_>,
    snapshot: Option<&Snapshot>,
    focused: bool,
) {
    let width = usize::from(area.width.saturating_sub(2));
    let agent = card
        .agent
        .as_ref()
        .map(|agent| (agent.name, agent_rows(agent, width, card.now)));
    let agent_height = agent.as_ref().map_or(0, |(_, rows)| {
        let wanted = u16::try_from(rows.len() + 2).unwrap_or(u16::MAX);
        let height = wanted.min(area.height.saturating_sub(CARD_MIN_HEIGHT));
        if height < 3 { 0 } else { height }
    });
    let [top, bottom] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(agent_height)]).areas(area);
    let block = super::pane_block(strings::PANE_PROJECTS, focused);
    let inner = block.inner(top);
    frame.render_widget(block, top);
    frame.render_widget(Paragraph::new(card_lines(card, snapshot, inner)), inner);
    if let Some((name, rows)) = agent
        && agent_height > 0
    {
        let block = super::pane_block(name, false);
        let inner = block.inner(bottom);
        frame.render_widget(block, bottom);
        frame.render_widget(Paragraph::new(rows), inner);
    }
}

/// The card's lines for `area`: the path, the stack, then the branch, the
/// upstream, today's lines, the last commit and its subject once the
/// repository was read. When rows run short the commit subject goes
/// first, then the stack, then the last rows.
fn card_lines(card: &Card<'_>, snapshot: Option<&Snapshot>, area: Rect) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let mut lines = vec![Line::raw(fit_path(card.path, width))];
    let mut stack_line = None;
    if let Some(stack) = card.stack {
        stack_line = Some(lines.len());
        lines.push(Line::raw(cut(&strings::stack_line(stack), width)));
    }
    let mut subject_line = None;
    if let Some(snapshot) = snapshot {
        let branch = &snapshot.status.branch;
        let head = super::branch_label(branch).unwrap_or(strings::CARD_NONE);
        lines.push(row(strings::CARD_BRANCH, head, Style::new(), width));
        // The label names the upstream: its remote, or itself without one.
        let (label, value) = match &branch.upstream {
            Some(upstream) => (
                upstream
                    .split_once('/')
                    .map_or(upstream.as_str(), |(remote, _)| remote),
                strings::upstream_state(branch.ahead_behind),
            ),
            None => (strings::CARD_REMOTE, strings::CARD_NONE.to_owned()),
        };
        lines.push(row(label, &value, Style::new(), width));
        let (value, style) = match &snapshot.today {
            Ok(today) => (strings::today_lines(*today), Style::new()),
            Err(_) => (strings::CARD_UNREAD.to_owned(), Style::new().fg(ALERT)),
        };
        lines.push(row(strings::CARD_TODAY, &value, style, width));
        let (value, style) = match &snapshot.last {
            // The age is what the row is for: the hash gives way to it.
            Ok(Some(last)) => {
                let age = last_age(Some(snapshot), card.now).unwrap_or_default();
                let room = width.saturating_sub(strings::CARD_LABEL_WIDTH);
                let hash = cut(&last.hash, room.saturating_sub(age.width() + 1));
                let value = if hash.is_empty() {
                    age
                } else {
                    format!("{hash} {age}")
                };
                (value, Style::new())
            }
            Ok(None) => (strings::CARD_NONE.to_owned(), Style::new()),
            Err(_) => (strings::CARD_UNREAD.to_owned(), Style::new().fg(ALERT)),
        };
        lines.push(row(strings::CARD_LAST, &value, style, width));
        if let Ok(Some(last)) = &snapshot.last {
            subject_line = Some(lines.len());
            lines.push(Line::raw(cut(&printable(&last.subject), width)));
        }
    }
    // The lines that matter least give way first, the later one first so
    // the earlier index still holds.
    for line in [subject_line, stack_line].into_iter().flatten() {
        if lines.len() > height {
            lines.remove(line);
        }
    }
    lines.truncate(height);
    lines
}

/// The agent's rows at `now`: what it is doing, then the rows its
/// telemetry filled: model, effort, context, the plan's limits or else the
/// cost, the lines it changed and time. Before its first event the
/// agent is waiting for a prompt; once the session is over it says so,
/// whatever its last event was.
fn agent_rows(agent: &AgentCard<'_>, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let status = agent.status;
    let (text, style) = if agent.ended {
        (strings::AGENT_ENDED, Style::new().fg(DIM))
    } else {
        let state = status.state.unwrap_or(State::Waiting);
        let style = match state {
            State::Working => Style::new().fg(ACCENT),
            State::Waiting => Style::new(),
            State::Attention => Style::new().fg(ALERT),
        };
        (strings::agent_state(state), style)
    };
    lines.push(row(strings::AGENT_STATE, text, style, width));
    let room = width.saturating_sub(strings::CARD_LABEL_WIDTH);
    if let Some(model) = &status.model {
        let model = strings::model_name(model, room);
        lines.push(row(strings::AGENT_MODEL, model, Style::new(), width));
    }
    if let Some(effort) = &status.effort {
        lines.push(row(strings::AGENT_EFFORT, effort, Style::new(), width));
    }
    if let Some(percent) = status.context {
        let bar = strings::context_bar(percent, room);
        lines.push(row(strings::AGENT_CONTEXT, &bar, Style::new(), width));
    }
    let [five_hours, week] = limit_rows(status, now);
    for (label, text) in [
        (strings::AGENT_FIVE_HOURS, five_hours),
        (strings::AGENT_WEEK, week),
    ] {
        if let Some(text) = text {
            lines.push(row(label, &text, Style::new(), width));
        }
    }
    // On a plan the limits are what runs out; the cost is what usage
    // billing would charge.
    if !status.limits.any()
        && let Some(usd) = status.cost
    {
        lines.push(row(
            strings::AGENT_COST,
            &strings::cost(usd),
            Style::new(),
            width,
        ));
    }
    if let Some((added, removed)) = status.lines {
        let text = strings::agent_lines(added, removed);
        lines.push(row(strings::AGENT_LINES, &text, Style::new(), width));
    }
    if let Some(time) = agent.time {
        let text = strings::session_time(time);
        lines.push(row(strings::AGENT_TIME, &text, Style::new(), width));
    }
    lines
}

/// One row of the card: the dim label, cut to one column under
/// [`strings::CARD_LABEL_WIDTH`] and padded to it so a space always
/// precedes the value, then the value cut to the room left.
fn row(label: &str, value: &str, style: Style, width: usize) -> Line<'static> {
    let label = pad(
        &cut(label, strings::CARD_LABEL_WIDTH - 1),
        strings::CARD_LABEL_WIDTH,
    );
    let room = width.saturating_sub(label.width());
    Line::from(vec![
        Span::styled(label, Style::new().fg(DIM)),
        Span::styled(cut(&printable(value), room), style),
    ])
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
fn fit_path(path: &str, width: usize) -> String {
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
    use crate::git::last::Last;
    use crate::projects::Framework;
    use crate::ui::changes::tests::sample;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn stack() -> Stack {
        Stack {
            language: "Rust",
            framework: Some(Framework {
                name: "ratatui",
                version: Some("0.30".into()),
            }),
        }
    }

    /// Draws the pane in a column of `width` by `height` cells.
    fn column(
        width: u16,
        height: u16,
        card: &Card<'_>,
        snapshot: Option<&Snapshot>,
        focused: bool,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), card, snapshot, focused))
            .expect("draw");
        terminal
    }

    /// Draws the pane with `width` by `height` cells inside the card's
    /// borders.
    fn draw(
        width: u16,
        height: u16,
        card: &Card<'_>,
        snapshot: Option<&Snapshot>,
    ) -> Terminal<TestBackend> {
        column(width + 2, height + 2, card, snapshot, false)
    }

    /// Row `y` of the column, borders included.
    fn line(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    /// Row `y` of the column inside the borders.
    fn inside(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (1..buffer.area.width.saturating_sub(1))
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    /// Row `y` inside the card's box.
    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        inside(terminal, y + 1)
    }

    /// The colour of the cell at `x`, `y` inside the card's box.
    fn fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        cell_fg(terminal, x + 1, y + 1)
    }

    /// The colour of the cell at `x`, `y` of the column.
    fn cell_fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.fg)
    }

    #[test]
    fn renders_the_card_rows() {
        let mut snapshot = sample();
        snapshot.status.branch.upstream = Some("origin/main".into());
        snapshot.status.branch.ahead_behind = Some((2, 1));
        snapshot.last = Ok(Some(Last {
            oid: "a3f9c1e000".into(),
            hash: "a3f9c1e".into(),
            time: 1_000_000,
            subject: "feat: x".into(),
        }));
        let stack = stack();
        let card = Card {
            path: "~/WebProjects/Personal/termis",
            stack: Some(&stack),
            now: 1_000_000 + 2 * 3600,
            agent: None,
        };
        let terminal = draw(22, 8, &card, Some(&snapshot));
        let rows: Vec<String> = (0..7).map(|y| row_text(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "~/…/Personal/termis   ",
                "Rust · ratatui 0.30   ",
                "branch  main          ",
                "origin  ↑2 ↓1         ",
                "today   +312 −58      ",
                "last    a3f9c1e 2h    ",
                "feat: x               ",
            ]
        );
        assert_eq!(fg(&terminal, 0, 2), DIM, "label");
        assert_eq!(fg(&terminal, 8, 2), Color::Reset, "value");

        // Detached, no upstream, nothing read today, before the first
        // commit, at a width that cuts.
        snapshot.status.branch.head = None;
        snapshot.status.branch.oid = Some("a3f9c1e000".into());
        snapshot.status.branch.upstream = None;
        snapshot.today = Err("git var: fatal".into());
        snapshot.last = Ok(None);
        let terminal = draw(14, 8, &card, Some(&snapshot));
        let rows: Vec<String> = (0..6).map(|y| row_text(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "~/…/termis    ",
                "Rust · ratatu…",
                "branch  detac…",
                "remote  none  ",
                "today   ?     ",
                "last    none  ",
            ]
        );
        assert_eq!(fg(&terminal, 8, 4), ALERT);
        // A last commit that could not be read.
        snapshot.last = Err("git log: fatal".into());
        let terminal = draw(14, 8, &card, Some(&snapshot));
        assert_eq!(row_text(&terminal, 5), "last    ?     ");
        assert_eq!(fg(&terminal, 8, 5), ALERT);
        assert_eq!(row_text(&terminal, 6).trim(), "", "no subject line");
    }

    #[test]
    fn the_age_follows_the_clock() {
        let mut snapshot = Snapshot::default();
        assert_eq!(last_age(None, 10), None);
        assert_eq!(last_age(Some(&snapshot), 10), None, "no commit yet");
        snapshot.last = Ok(Some(Last {
            oid: "abc".into(),
            hash: "abc".into(),
            time: 100,
            subject: String::new(),
        }));
        assert_eq!(last_age(Some(&snapshot), 100).as_deref(), Some("now"));
        assert_eq!(last_age(Some(&snapshot), 7300).as_deref(), Some("2h"));
        snapshot.last = Err("unreadable".into());
        assert_eq!(last_age(Some(&snapshot), 100), None);
    }

    #[test]
    fn the_label_keeps_its_column_and_the_hash_gives_way() {
        let mut snapshot = sample();
        snapshot.status.branch.ahead_behind = Some((2, 1));
        snapshot.last = Ok(Some(Last {
            oid: "3a2bd68a9f12".into(),
            hash: "3a2bd68a9f12".into(),
            time: 1_000_000,
            subject: "feat: x".into(),
        }));
        let card = Card {
            path: "p",
            stack: None,
            now: 1_000_000,
            agent: None,
        };
        // A remote wider than the label column is cut before its space.
        snapshot.status.branch.upstream = Some("upstream/main".into());
        let terminal = draw(18, 6, &card, Some(&snapshot));
        assert_eq!(row_text(&terminal, 2), "upstre… ↑2 ↓1     ");
        assert_eq!(fg(&terminal, 8, 2), Color::Reset, "the value column");
        // The hash gives way to the age in a narrow pane; the subject
        // follows on its own line.
        assert_eq!(row_text(&terminal, 4), "last    3a2bd… now");
        assert_eq!(row_text(&terminal, 5), "feat: x           ");
        // A local upstream is named as it is.
        snapshot.status.branch.upstream = Some("main".into());
        let terminal = draw(18, 6, &card, Some(&snapshot));
        assert_eq!(row_text(&terminal, 2), "main    ↑2 ↓1     ");
        // With room for both, the hash stays whole.
        let terminal = draw(24, 6, &card, Some(&snapshot));
        assert_eq!(row_text(&terminal, 4), "last    3a2bd68a9f12 now");
    }

    #[test]
    fn shows_only_the_path_and_the_stack_without_a_snapshot() {
        let stack = stack();
        let card = Card {
            path: "/tmp/x",
            stack: Some(&stack),
            now: 0,
            agent: None,
        };
        let terminal = draw(20, 4, &card, None);
        assert_eq!(row_text(&terminal, 0), "/tmp/x              ");
        assert_eq!(row_text(&terminal, 1), "Rust · ratatui 0.30 ");
        assert_eq!(row_text(&terminal, 2).trim(), "");
        let card = Card {
            path: "/tmp/x",
            stack: None,
            now: 0,
            agent: None,
        };
        let terminal = draw(20, 2, &card, Some(&sample()));
        assert_eq!(row_text(&terminal, 1).trim_end(), "branch  main");
        draw(0, 0, &card, None);
        draw(3, 1, &card, Some(&sample()));
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
    fn the_agent_box_shows_what_the_telemetry_said() {
        let status = Status {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            context: Some(41),
            cost: Some(1.844),
            limits: Limits::default(),
            lines: Some((12, 3)),
            duration: Some(Duration::from_secs(42)),
            state: Some(State::Working),
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
        let terminal = column(22, 13, &card, None, false);
        let rows: Vec<String> = (0..13).map(|y| line(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "┌ Projects ──────────┐",
                "│~/p                 │",
                "│                    │",
                "└────────────────────┘",
                "┌ claude ────────────┐",
                "│state   ✶ working   │",
                "│model   Opus 5.5    │",
                "│effort  xhigh       │",
                "│ctx     ▓▓▓░░░░░ 41%│",
                "│cost    $1.84       │",
                "│lines   +12 −3      │",
                "│time    00:42       │",
                "└────────────────────┘",
            ]
        );
        assert_eq!(cell_fg(&terminal, 9, 5), ACCENT, "working in the accent");
        assert_eq!(cell_fg(&terminal, 0, 4), DIM, "the box is dim");
        // Focus marks the card's box, not the agent's.
        let terminal = column(22, 13, &card, None, true);
        assert!(line(&terminal, 0).starts_with("┌ ● Projects "));
        assert_eq!(cell_fg(&terminal, 0, 0), ACCENT);
        assert_eq!(line(&terminal, 4), "┌ claude ────────────┐");
        assert_eq!(cell_fg(&terminal, 0, 4), DIM);

        // Asking stands out; before any telemetry only the state shows,
        // as waiting for a prompt, and the box is that short.
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
        let terminal = column(20, 8, &card, None, false);
        assert!(line(&terminal, 5).starts_with("┌ claude "));
        assert_eq!(inside(&terminal, 6), "state   ! asking  ");
        assert_eq!(cell_fg(&terminal, 9, 6), ALERT);
        assert!(line(&terminal, 7).starts_with("└"));
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
        assert_eq!(
            inside(&column(20, 8, &card, None, false), 6),
            "state   · waiting "
        );
        for (w, h) in [(0, 0), (1, 1), (3, 2), (6, 3), (2, 6), (4, 8)] {
            column(w, h, &card, None, false);
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
        let terminal = column(22, 9, &card, None, false);
        let rows: Vec<String> = (4..9).map(|y| line(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "┌ claude ────────────┐",
                "│state   · waiting   │",
                "│5h      9% 2h10m    │",
                "│week    30% 4d3h    │",
                "└────────────────────┘",
            ]
        );
        // At the real width the value has ten columns, enough for the
        // time left.
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
        let terminal = column(20, 9, &real, None, false);
        assert_eq!(inside(&terminal, 6), "5h      23% 3h12m ");
        assert_eq!(inside(&terminal, 7), "week    100% 6d23h");
        assert_eq!(
            limit_rows(&status, now + 2 * 3600 + 600),
            [Some("reset".to_owned()), Some("30% 4d1h".to_owned())],
            "the five hours ran out"
        );
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
        let terminal = column(22, 9, &card, None, false);
        assert_eq!(inside(&terminal, 6), "state   · waiting   ");
        assert_eq!(inside(&terminal, 7), "cost    $1.84       ");
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
        let terminal = column(22, 9, &card, None, false);
        assert_eq!(inside(&terminal, 7), "week    30% 4d3h    ");
        assert!(
            (0..9).all(|y| !line(&terminal, y).contains("cost")),
            "no cost row"
        );
    }

    #[test]
    fn the_agent_box_at_the_real_width_and_when_rows_run_short() {
        let status = Status {
            model: Some("Sonnet 5.5 (1M context)".into()),
            effort: None,
            context: Some(41),
            cost: Some(1.845),
            limits: Limits::default(),
            lines: None,
            duration: None,
            state: Some(State::Attention),
        };
        let stack = stack();
        let mut snapshot = sample();
        snapshot.last = Ok(Some(Last {
            oid: "a3f9c1e".into(),
            hash: "a3f9c1e".into(),
            time: 0,
            subject: "feat: x".into(),
        }));
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
        let rows = |terminal: &Terminal<TestBackend>, height: u16| -> Vec<String> {
            (0..height).map(|y| line(terminal, y)).collect()
        };
        // 20 columns: the left pane of a 100-column terminal. The agent's
        // box sits at the foot of the column, the card's box above it.
        let terminal = column(20, 30, &card, Some(&snapshot), false);
        assert_eq!(line(&terminal, 29), format!("└{}┘", "─".repeat(18)));
        assert_eq!(line(&terminal, 23), format!("┌ claude {}┐", "─".repeat(10)));
        assert_eq!(inside(&terminal, 24), "state   ! asking  ");
        assert_eq!(
            inside(&terminal, 25),
            "model   Sonnet 5.5",
            "the suffix goes before the name is cut"
        );
        assert_eq!(inside(&terminal, 26), "ctx     ▓▓░░░░ 41%");
        assert_eq!(inside(&terminal, 27), "cost    $1.84     ");
        assert_eq!(inside(&terminal, 28), "time    1:02:05   ");
        assert_eq!(inside(&terminal, 7), "feat: x           ", "the subject");
        assert!(line(&terminal, 22).starts_with("└"), "the card's box ends");

        // Twelve rows: the card's subject and stack give way first.
        let terminal = column(20, 12, &card, Some(&snapshot), false);
        let shown = rows(&terminal, 12);
        assert!(
            shown
                .iter()
                .all(|row| !row.contains("feat: x") && !row.contains("Rust")),
            "{shown:?}"
        );
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert_eq!(inside(&terminal, 6), "state   ! asking  ");
        assert_eq!(inside(&terminal, 10), "time    1:02:05   ");
        // Nine rows: the card is down to its path; the agent's box loses
        // its last row.
        let terminal = column(20, 9, &card, Some(&snapshot), false);
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert!(line(&terminal, 3).starts_with("┌ claude "));
        assert_eq!(inside(&terminal, 4), "state   ! asking  ");
        assert_eq!(inside(&terminal, 7), "cost    $1.84     ");
        assert!(line(&terminal, 8).starts_with("└"), "no time row");
        // Six rows: the card keeps its path, the agent its state.
        let terminal = column(20, 6, &card, Some(&snapshot), false);
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert!(line(&terminal, 3).starts_with("┌ claude "));
        assert_eq!(inside(&terminal, 4), "state   ! asking  ");
        // Five rows: no room for the state, so no agent's box.
        let terminal = column(20, 5, &card, Some(&snapshot), false);
        let shown = rows(&terminal, 5);
        assert!(shown.iter().all(|row| !row.contains("claude")), "{shown:?}");
        assert_eq!(inside(&terminal, 1), "~/p               ");

        // An ended session says so, whatever its last event was.
        let over = Card {
            agent: Some(AgentCard {
                ended: true,
                ..agent
            }),
            ..card
        };
        let terminal = column(20, 30, &over, Some(&snapshot), false);
        assert_eq!(inside(&terminal, 24), "state   × ended   ");
        assert_eq!(cell_fg(&terminal, 9, 24), DIM);
    }
}
