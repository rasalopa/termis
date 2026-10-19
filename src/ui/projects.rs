//! The projects pane: the current project's path and stack and, in a box
//! of its own at the foot of the column, the agent working in it. Git
//! lives in the changes pane.

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
use crate::projects::Stack;
use crate::strings;

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

/// The rows the card's box keeps when the agent's box takes the rest:
/// its borders and the path.
const CARD_MIN_HEIGHT: u16 = 3;

/// Draws the pane in `area`, the whole left column: the card in a box
/// titled Projects, highlighted when `focused`, and while the agent's
/// telemetry reaches the app, the agent in a box at the foot of the
/// column, as tall as its rows. When rows run short the stack gives way
/// first; then the agent's box loses its last rows down to the state;
/// then it goes.
pub fn render(frame: &mut Frame, area: Rect, card: &Card<'_>, focused: bool) {
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
    frame.render_widget(Paragraph::new(card_lines(card, inner)), inner);
    if let Some((name, rows)) = agent
        && agent_height > 0
    {
        let block = super::pane_block(name, false);
        let inner = block.inner(bottom);
        frame.render_widget(block, bottom);
        frame.render_widget(Paragraph::new(rows), inner);
    }
}

/// The card's lines for `area`: the path, then the stack when a manifest
/// said.
fn card_lines(card: &Card<'_>, area: Rect) -> Vec<Line<'static>> {
    let width = usize::from(area.width);
    let mut lines = vec![Line::raw(fit_path(card.path, width))];
    if let Some(stack) = card.stack {
        lines.push(Line::raw(cut(&strings::stack_line(stack), width)));
    }
    lines
}

/// The agent's rows at `now`: what it is doing, the tool it runs while
/// there is one, then the rows its telemetry filled: model, effort,
/// context, the plan's limits or else the cost, the lines it changed and
/// time. Before its first event the agent is waiting for a prompt; once
/// the session is over it says so, whatever its last event was.
fn agent_rows(agent: &AgentCard<'_>, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let status = agent.status;
    let (text, style) = if agent.ended {
        (strings::AGENT_ENDED.to_owned(), Style::new().fg(DIM))
    } else {
        let state = status.state.unwrap_or(State::Waiting);
        let style = match state {
            State::Working => Style::new().fg(ACCENT),
            State::Waiting => Style::new(),
            State::Attention => Style::new().fg(ALERT),
        };
        let text = match (state, &status.activity) {
            (State::Working, Some(activity)) => strings::agent_running(activity),
            _ => strings::agent_state(state).to_owned(),
        };
        (text, style)
    };
    lines.push(row(strings::AGENT_STATE, &text, style, width));
    let room = width.saturating_sub(strings::AGENT_LABEL_WIDTH);
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

/// One row of the agent's box: the dim label, cut to one column under
/// [`strings::AGENT_LABEL_WIDTH`] and padded to it so a space always
/// precedes the value, then the value cut to the room left.
fn row(label: &str, value: &str, style: Style, width: usize) -> Line<'static> {
    let label = pad(
        &cut(label, strings::AGENT_LABEL_WIDTH - 1),
        strings::AGENT_LABEL_WIDTH,
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
    fn column(width: u16, height: u16, card: &Card<'_>, focused: bool) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), card, focused))
            .expect("draw");
        terminal
    }

    /// Draws the pane with `width` by `height` cells inside the card's
    /// borders.
    fn draw(width: u16, height: u16, card: &Card<'_>) -> Terminal<TestBackend> {
        column(width + 2, height + 2, card, false)
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

    /// The colour of the cell at `x`, `y` of the column.
    fn cell_fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.fg)
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
        assert_eq!(row_text(&terminal, 0), "~/…/Personal/termis   ");
        assert_eq!(row_text(&terminal, 1), "Rust · ratatui 0.30   ");
        assert_eq!(row_text(&terminal, 2).trim(), "", "nothing of git");
        // Narrow, the stack is cut; with one row, it gives way.
        let terminal = draw(14, 2, &card);
        assert_eq!(row_text(&terminal, 0), "~/…/termis    ");
        assert_eq!(row_text(&terminal, 1), "Rust · ratatu…");
        assert_eq!(row_text(&draw(14, 1, &card), 0), "~/…/termis    ");
        let bare = Card {
            stack: None,
            ..card
        };
        assert_eq!(row_text(&draw(14, 2, &bare), 1).trim(), "");
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
        let terminal = column(22, 13, &card, false);
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
        let terminal = column(22, 13, &card, true);
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
        let terminal = column(20, 8, &card, false);
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
            inside(&column(20, 8, &card, false), 6),
            "state   · waiting "
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
        let terminal = column(20, 8, &card, false);
        assert_eq!(inside(&terminal, 6), "state   ✶ Edit pr…");
        assert_eq!(cell_fg(&terminal, 9, 6), ACCENT);
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
        let terminal = column(22, 9, &card, false);
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
        let terminal = column(20, 9, &real, false);
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
        let terminal = column(22, 9, &card, false);
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
        let terminal = column(22, 9, &card, false);
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
        let rows = |terminal: &Terminal<TestBackend>, height: u16| -> Vec<String> {
            (0..height).map(|y| line(terminal, y)).collect()
        };
        // 20 columns: the left pane of a 100-column terminal. The agent's
        // box sits at the foot of the column, the card's box above it.
        let terminal = column(20, 30, &card, false);
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
        assert_eq!(inside(&terminal, 2), "Rust · ratatui 0.…", "the stack");
        assert!(line(&terminal, 22).starts_with("└"), "the card's box ends");

        // Ten rows: the stack gives way first.
        let terminal = column(20, 10, &card, false);
        let shown = rows(&terminal, 10);
        assert!(shown.iter().all(|row| !row.contains("Rust")), "{shown:?}");
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert_eq!(inside(&terminal, 4), "state   ! asking  ");
        assert_eq!(inside(&terminal, 8), "time    1:02:05   ");
        // Nine rows: the card is down to its path; the agent's box loses
        // its last row.
        let terminal = column(20, 9, &card, false);
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert!(line(&terminal, 3).starts_with("┌ claude "));
        assert_eq!(inside(&terminal, 4), "state   ! asking  ");
        assert_eq!(inside(&terminal, 7), "cost    $1.84     ");
        assert!(line(&terminal, 8).starts_with("└"), "no time row");
        // Six rows: the card keeps its path, the agent its state.
        let terminal = column(20, 6, &card, false);
        assert_eq!(inside(&terminal, 1), "~/p               ");
        assert!(line(&terminal, 3).starts_with("┌ claude "));
        assert_eq!(inside(&terminal, 4), "state   ! asking  ");
        // Five rows: no room for the state, so no agent's box.
        let terminal = column(20, 5, &card, false);
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
        let terminal = column(20, 30, &over, false);
        assert_eq!(inside(&terminal, 24), "state   × ended   ");
        assert_eq!(cell_fg(&terminal, 9, 24), DIM);
    }
}
