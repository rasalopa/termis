//! The diff of the selected row, shown in the changes pane in place of
//! the lists.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::changes::{cut, printable};
use super::palette::{ADDED, MUTED, REMOVED};
use super::render_empty;
use crate::git::diff::Request;
use crate::strings;

/// What a diff line is, which sets its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// File headers and the like, before the first hunk of a file.
    Header,
    /// A `@@` line.
    Hunk,
    Added,
    Removed,
    Context,
}

/// One line of the diff, ready to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffLine {
    text: String,
    kind: Kind,
}

/// The diff on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// What was asked of git, to tell its answer from an older one.
    pub request: Request,
    /// The path, as the row shows it.
    pub path: String,
    /// The lines, once git answered.
    lines: Option<Vec<DiffLine>>,
}

impl Diff {
    /// A diff waiting for git's answer to `request`.
    pub fn new(request: Request, path: String) -> Self {
        Self {
            request,
            path,
            lines: None,
        }
    }

    /// Takes `text` as the answer to `request` when it is the one waited
    /// for; an answer to another request is ignored. Returns whether it
    /// was taken.
    pub fn accept(&mut self, request: &Request, text: &str) -> bool {
        if self.request != *request {
            return false;
        }
        self.lines = Some(parse(text));
        true
    }
}

/// The lines of `text` with their kinds: everything before the first
/// `@@` of a file is a header, then `+` and `-` lines are additions and
/// removals until the next `diff` line.
fn parse(text: &str) -> Vec<DiffLine> {
    let mut in_hunk = false;
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("diff ") {
                in_hunk = false;
                Kind::Header
            } else if line.starts_with("@@") {
                in_hunk = true;
                Kind::Hunk
            } else if !in_hunk {
                Kind::Header
            } else if line.starts_with('+') {
                Kind::Added
            } else if line.starts_with('-') {
                Kind::Removed
            } else {
                Kind::Context
            };
            DiffLine {
                text: printable(&line.replace('\t', "    ")),
                kind,
            }
        })
        .collect()
}

/// Draws the header and the diff lines from `scroll` down, moved back
/// so that the last line stays at the bottom; the offset used is
/// returned for the next frame. The header is bold; added and removed
/// lines take their colours, hunk and file headers the muted one.
pub fn render(frame: &mut Frame, area: Rect, diff: &Diff, scroll: usize) -> usize {
    if area.height == 0 || area.width == 0 {
        return scroll;
    }
    let width = usize::from(area.width);
    let title = strings::diff_header(&printable(&diff.path), diff.request.staged);
    let header = Line::styled(
        cut(&title, width),
        Style::new().add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Paragraph::new(header), area);
    let Some(lines) = &diff.lines else {
        return 0;
    };
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    if lines.is_empty() {
        render_empty(frame, body, strings::DIFF_EMPTY);
        return 0;
    }
    let height = usize::from(body.height);
    let scroll = scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(height)
        .map(|line| Line::styled(cut(&line.text, width), style(line.kind)))
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    scroll
}

fn style(kind: Kind) -> Style {
    match kind {
        Kind::Header | Kind::Hunk => Style::new().fg(MUTED),
        Kind::Added => Style::new().fg(ADDED),
        Kind::Removed => Style::new().fg(REMOVED),
        Kind::Context => Style::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use std::path::PathBuf;

    const TEXT: &str = "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n one\n-two\n+two!\n+three\n";

    fn request(staged: bool) -> Request {
        Request {
            paths: vec![PathBuf::from("f.txt")],
            staged,
            untracked: false,
        }
    }

    fn diff(text: Option<&str>) -> Diff {
        let mut diff = Diff::new(request(false), "f.txt".into());
        if let Some(text) = text {
            assert!(diff.accept(&request(false), text));
        }
        diff
    }

    fn draw(width: u16, height: u16, diff: &Diff, scroll: usize) -> (Terminal<TestBackend>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut offset = 0;
        terminal
            .draw(|frame| offset = render(frame, frame.area(), diff, scroll))
            .expect("draw");
        (terminal, offset)
    }

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
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
    fn lines_are_kinds_by_position_not_by_prefix_alone() {
        let kinds: Vec<Kind> = parse(TEXT).into_iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Header,
                Kind::Header,
                Kind::Header,
                Kind::Header,
                Kind::Hunk,
                Kind::Context,
                Kind::Removed,
                Kind::Added,
                Kind::Added,
            ]
        );
        // Content that looks like a header is content inside a hunk.
        let tricky = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n--- note\n+++i;\n\\ No newline at end of file\ndiff --git a/y b/y\n--- a/y\n";
        let kinds: Vec<Kind> = parse(tricky).into_iter().map(|line| line.kind).collect();
        assert_eq!(kinds[4], Kind::Removed);
        assert_eq!(kinds[5], Kind::Added);
        assert_eq!(kinds[6], Kind::Context);
        assert_eq!(kinds[7], Kind::Header, "a new file starts headers again");
        assert_eq!(kinds[8], Kind::Header);
        let tab = parse("+\tx\n");
        assert_eq!(tab[0].text, "+    x");
    }

    #[test]
    fn answers_to_other_requests_are_ignored() {
        let mut d = Diff::new(request(false), "f.txt".into());
        assert!(!d.accept(&request(true), TEXT), "another side of the row");
        assert!(d.lines.is_none());
        assert!(d.accept(&request(false), TEXT));
        assert_eq!(d.lines.as_ref().map(Vec::len), Some(9));
    }

    #[test]
    fn colours_the_lines_and_scrolls_to_the_end() {
        let d = diff(Some(TEXT));
        let (terminal, offset) = draw(30, 6, &d, 0);
        assert_eq!(offset, 0);
        assert!(
            row(&terminal, 0).starts_with("diff f.txt"),
            "{}",
            row(&terminal, 0)
        );
        assert_eq!(fg(&terminal, 0, 0), Color::Reset, "header");
        assert!(
            terminal
                .backend()
                .buffer()
                .cell((0, 0))
                .is_some_and(|c| c.modifier.contains(Modifier::BOLD)),
            "the header is bold"
        );
        assert!(row(&terminal, 1).starts_with("diff --git"));
        assert_eq!(fg(&terminal, 0, 1), MUTED);
        assert!(
            row(&terminal, 5).starts_with("@@ -1,2"),
            "{}",
            row(&terminal, 5)
        );

        let (terminal, offset) = draw(30, 6, &d, 100);
        assert_eq!(offset, 4, "no further than the last line");
        assert!(row(&terminal, 1).starts_with("@@"));
        assert_eq!(row(&terminal, 2).trim_end(), " one");
        assert_eq!(fg(&terminal, 0, 2), Color::Reset, "context");
        assert_eq!(row(&terminal, 3).trim_end(), "-two");
        assert_eq!(fg(&terminal, 0, 3), REMOVED);
        assert_eq!(row(&terminal, 4).trim_end(), "+two!");
        assert_eq!(fg(&terminal, 0, 4), ADDED);
        assert_eq!(row(&terminal, 5).trim_end(), "+three");

        let mut staged = Diff::new(request(true), "f.txt".into());
        staged.accept(&request(true), TEXT);
        let (terminal, _) = draw(30, 6, &staged, 0);
        assert!(row(&terminal, 0).starts_with("diff f.txt (staged)"));
    }

    #[test]
    fn says_when_there_is_nothing_and_waits_for_git() {
        let (terminal, offset) = draw(30, 6, &diff(Some("")), 3);
        assert_eq!(offset, 0);
        let all: String = (0..6).map(|y| row(&terminal, y)).collect();
        assert!(all.contains("nothing to show"), "{all}");
        let (terminal, offset) = draw(30, 6, &diff(None), 3);
        assert_eq!(offset, 0);
        assert!(row(&terminal, 0).starts_with("diff f.txt"));
        assert!(row(&terminal, 1).trim().is_empty());
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (5, 1), (3, 2)] {
            draw(w, h, &diff(Some(TEXT)), 1);
        }
    }

    #[test]
    fn long_lines_fit_the_width() {
        let d = diff(Some("+\tindented and quite a long line here\n"));
        let (terminal, _) = draw(20, 3, &d, 0);
        let line = row(&terminal, 1);
        assert!(line.starts_with("+    indented"), "{line}");
        assert!(line.ends_with('…'), "{line}");
    }
}
