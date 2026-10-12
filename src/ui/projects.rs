//! The projects pane: a card for the current repository and a section
//! for the agent working in it.

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
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
    /// The agent's section, while its telemetry reaches the app.
    pub agent: Option<AgentCard<'a>>,
}

/// What the agent's section shows.
#[derive(Clone, Copy, Debug)]
pub struct AgentCard<'a> {
    /// The agent's name, on the section's divider.
    pub name: &'a str,
    /// What its telemetry said last.
    pub status: &'a Status,
    /// How long the session has run by now, or ran until it ended.
    pub time: Option<Duration>,
    /// Whether the session is over.
    pub ended: bool,
}

/// The age of the last commit as the card shows it at `now`, while the
/// snapshot has one. The app watches it to redraw when it changes.
pub fn last_age(snapshot: Option<&Snapshot>, now: i64) -> Option<String> {
    let last = snapshot?.last.as_ref().ok()?.as_ref()?;
    Some(strings::age(now.saturating_sub(last.time)))
}

/// Draws the card in `area`: the path, the stack, then the branch, the
/// upstream, today's lines, the last commit and its subject once the
/// repository was read, then the agent's section. When rows run short the
/// commit subject goes first, then the stack, then the card's last rows,
/// so the agent's divider always comes with its state.
pub fn render(frame: &mut Frame, area: Rect, card: &Card<'_>, snapshot: Option<&Snapshot>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
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
    let mut agent = Vec::new();
    if let Some(card) = &card.agent {
        agent_section(&mut agent, card, width);
    }
    // The lines that matter least give way first, the later one first so
    // the earlier index still holds.
    for line in [subject_line, stack_line].into_iter().flatten() {
        if lines.len() + agent.len() > height {
            lines.remove(line);
        }
    }
    let keep = agent.len().min(2);
    if lines.len() + keep > height {
        lines.truncate(height.saturating_sub(keep));
    }
    agent.truncate(height.saturating_sub(lines.len()));
    if agent.len() == 1 {
        agent.clear();
    }
    lines.extend(agent);
    lines.truncate(height);
    frame.render_widget(Paragraph::new(lines), area);
}

/// The agent's section: a divider with its name, then what it is doing
/// and the rows its telemetry filled: model, context, cost and time.
/// Before its first event the agent is waiting for a prompt; once the
/// session is over it says so, whatever its last event was.
fn agent_section(lines: &mut Vec<Line<'static>>, agent: &AgentCard<'_>, width: usize) {
    let name = cut(agent.name, width.saturating_sub(4));
    let divider = if name.width() < 2 {
        "─".repeat(width)
    } else {
        let rule = width.saturating_sub(name.width() + 3);
        format!("─ {name} {}", "─".repeat(rule))
    };
    lines.push(Line::styled(divider, Style::new().fg(DIM)));
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
    if let Some(percent) = status.context {
        let bar = strings::context_bar(percent, room);
        lines.push(row(strings::AGENT_CONTEXT, &bar, Style::new(), width));
    }
    if let Some(usd) = status.cost {
        lines.push(row(
            strings::AGENT_COST,
            &strings::cost(usd),
            Style::new(),
            width,
        ));
    }
    if let Some(time) = agent.time {
        let text = strings::session_time(time);
        lines.push(row(strings::AGENT_TIME, &text, Style::new(), width));
    }
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

    fn draw(
        width: u16,
        height: u16,
        card: &Card<'_>,
        snapshot: Option<&Snapshot>,
    ) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), card, snapshot))
            .expect("draw");
        terminal
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    fn fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
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
    fn the_agent_section_shows_what_the_telemetry_said() {
        let status = Status {
            model: Some("Opus 5.5".into()),
            context: Some(41),
            cost: Some(1.844),
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
        let terminal = draw(20, 8, &card, None);
        let rows: Vec<String> = (0..7).map(|y| row_text(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "~/p                 ",
                "─ claude ───────────",
                "state   ✶ working   ",
                "model   Opus 5.5    ",
                "ctx     ▓▓▓░░░░░ 41%",
                "cost    $1.84       ",
                "time    00:42       ",
            ]
        );
        assert_eq!(fg(&terminal, 8, 2), ACCENT, "working in the accent");
        assert_eq!(fg(&terminal, 0, 1), DIM, "the divider is dim");

        // Asking stands out; before any telemetry only the state shows,
        // as waiting for a prompt.
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
        let terminal = draw(18, 6, &card, None);
        assert_eq!(row_text(&terminal, 2), "state   ! asking  ");
        assert_eq!(fg(&terminal, 8, 2), ALERT);
        assert_eq!(row_text(&terminal, 3).trim(), "", "nothing else known");
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
        assert_eq!(row_text(&draw(18, 4, &card, None), 2), "state   · waiting ");
        for (w, h) in [(0, 0), (1, 1), (3, 2), (6, 3)] {
            draw(w, h, &card, None);
        }
        // Too narrow for the name: a plain rule.
        assert_eq!(row_text(&draw(4, 3, &card, None), 1), "────");
        assert_eq!(row_text(&draw(5, 3, &card, None), 1), "─────");
        assert_eq!(row_text(&draw(6, 3, &card, None), 1), "─ c… ─");
    }

    #[test]
    fn the_agent_section_at_the_real_width_and_when_rows_run_short() {
        let status = Status {
            model: Some("Sonnet 5.5 (1M context)".into()),
            context: Some(41),
            cost: Some(1.845),
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
        // 18 columns: the room of a 100-column terminal.
        let terminal = draw(18, 20, &card, Some(&snapshot));
        let rows: Vec<String> = (0..14).map(|y| row_text(&terminal, y)).collect();
        let divider = rows
            .iter()
            .position(|row| row.starts_with("─ claude"))
            .expect("divider");
        assert_eq!(rows[divider], "─ claude ─────────");
        assert_eq!(rows[divider + 1], "state   ! asking  ");
        assert_eq!(
            rows[divider + 2],
            "model   Sonnet 5.5",
            "the suffix goes before the name is cut"
        );
        assert_eq!(rows[divider + 3], "ctx     ▓▓░░░░ 41%");
        assert_eq!(rows[divider + 4], "cost    $1.84     ");
        assert_eq!(rows[divider + 5], "time    1:02:05   ");

        // Eight rows: the subject and the stack give way, the state stays.
        let terminal = draw(18, 8, &card, Some(&snapshot));
        let rows: Vec<String> = (0..8).map(|y| row_text(&terminal, y)).collect();
        assert!(
            rows.iter()
                .all(|row| !row.starts_with("feat: x") && !row.starts_with("Rust")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("state   ! asking")),
            "{rows:?}"
        );
        // Three rows: the path, the divider and the state.
        let terminal = draw(18, 3, &card, Some(&snapshot));
        assert!(row_text(&terminal, 1).starts_with("─ claude"));
        assert!(row_text(&terminal, 2).starts_with("state"));
        // Two rows: the divider never comes alone.
        let terminal = draw(18, 2, &card, Some(&snapshot));
        assert!(row_text(&terminal, 0).starts_with("─ claude"));
        assert!(row_text(&terminal, 1).starts_with("state"));

        // An ended session says so, whatever its last event was.
        let over = Card {
            agent: Some(AgentCard {
                ended: true,
                ..agent
            }),
            ..card
        };
        let terminal = draw(18, 20, &over, Some(&snapshot));
        let state_row = (0..20u16)
            .find(|&y| row_text(&terminal, y).starts_with("state"))
            .expect("state row");
        assert_eq!(row_text(&terminal, state_row), "state   × ended   ");
        assert_eq!(fg(&terminal, 8, state_row), DIM);
    }
}
