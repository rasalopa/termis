//! The projects pane: a card for the current repository.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::changes::{cut, pad, printable};
use super::{ALERT, DIM};
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
}

/// The age of the last commit as the card shows it at `now`, while the
/// snapshot has one. The app watches it to redraw when it changes.
pub fn last_age(snapshot: Option<&Snapshot>, now: i64) -> Option<String> {
    let last = snapshot?.last.as_ref().ok()?.as_ref()?;
    Some(strings::age(now.saturating_sub(last.time)))
}

/// Draws the card in `area`: the path, the stack, then the branch, the
/// upstream, today's lines, the last commit and its subject once the
/// repository was read. Rows the area cannot hold are left out from the
/// bottom.
pub fn render(frame: &mut Frame, area: Rect, card: &Card<'_>, snapshot: Option<&Snapshot>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = usize::from(area.width);
    let mut lines = vec![Line::raw(fit_path(card.path, width))];
    if let Some(stack) = card.stack {
        lines.push(Line::raw(cut(&strings::stack_line(stack), width)));
    }
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
            lines.push(Line::raw(cut(&printable(&last.subject), width)));
        }
    }
    lines.truncate(usize::from(area.height));
    frame.render_widget(Paragraph::new(lines), area);
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
        };
        let terminal = draw(20, 4, &card, None);
        assert_eq!(row_text(&terminal, 0), "/tmp/x              ");
        assert_eq!(row_text(&terminal, 1), "Rust · ratatui 0.30 ");
        assert_eq!(row_text(&terminal, 2).trim(), "");
        let card = Card {
            path: "/tmp/x",
            stack: None,
            now: 0,
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
}
