//! The commit box: a draft with a subject line and body lines, edited at
//! its end.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::palette::{ACCENT, MUTED};
use crate::git::commit;
use crate::strings;

/// Draft lines the box shows at most; earlier ones scroll out of view.
const MAX_LINES: u16 = 5;

/// A change to the draft, from a key or a paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Text at the end; a newline in it starts a body line.
    Insert(String),
    /// Remove the last grapheme, a newline included.
    Backspace,
    /// Start a body line.
    Newline,
}

/// The commit message being written: the first line is the subject, the
/// rest the body. It is edited at its end only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    text: String,
}

impl Draft {
    /// Applies `edit` at the end of the text.
    pub fn apply(&mut self, edit: &Edit) {
        match edit {
            Edit::Insert(text) => self.text.push_str(text),
            Edit::Backspace => {
                if let Some((start, _)) = self.text.grapheme_indices(true).next_back() {
                    self.text.truncate(start);
                }
            }
            Edit::Newline => self.text.push('\n'),
        }
    }

    /// The first line, trimmed.
    pub fn subject(&self) -> &str {
        self.text.lines().next().unwrap_or("").trim()
    }

    /// Everything after the first line.
    pub fn body(&self) -> &str {
        self.text.split_once('\n').map_or("", |(_, body)| body)
    }

    /// The message for git; none without a subject.
    pub fn message(&self) -> Option<String> {
        let subject = self.subject();
        (!subject.is_empty()).then(|| commit::message(subject, self.body()))
    }

    /// Forgets the text.
    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Whether nothing was typed.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many lines were typed, the last one possibly empty.
    fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// The last `count` lines as typed, first to last.
    fn last_lines(&self, count: usize) -> Vec<&str> {
        let mut lines: Vec<&str> = self.text.rsplit('\n').take(count).collect();
        lines.reverse();
        lines
    }
}

/// Rows the box takes for `draft`: a title, the border and the shown
/// lines.
pub fn height(draft: &Draft) -> u16 {
    let lines = u16::try_from(draft.line_count()).unwrap_or(u16::MAX);
    3 + lines.min(MAX_LINES)
}

/// Draws the section title and the bordered box with the last lines of
/// `draft` in `area`, each line cut from the left to fit. Returns where
/// the cursor goes, after the last line, when it is on screen.
pub fn render(frame: &mut Frame, area: Rect, draft: &Draft) -> Option<Position> {
    if area.height < 3 || area.width < 3 {
        return None;
    }
    let [title, boxed] = Layout::vertical([Constraint::Length(1), Constraint::Min(2)]).areas(area);
    frame.render_widget(
        Paragraph::new(strings::COMMIT_SECTION).style(Style::new().fg(MUTED)),
        title,
    );
    let block = Block::bordered().border_style(Style::new().fg(ACCENT));
    let inner = block.inner(boxed);
    frame.render_widget(block, boxed);
    if inner.height == 0 || inner.width == 0 {
        return None;
    }
    let shown = draft.last_lines(usize::from(inner.height));
    let width = usize::from(inner.width);
    let tails: Vec<String> = shown.iter().map(|line| tail(line, width)).collect();
    let last = tails.last().map_or(0, |line| line.width());
    let lines: Vec<Line> = tails.into_iter().map(Line::raw).collect();
    frame.render_widget(Paragraph::new(lines), inner);
    let y = inner.y + u16::try_from(shown.len().saturating_sub(1)).unwrap_or(u16::MAX);
    let x = inner.x + u16::try_from(last).unwrap_or(u16::MAX);
    Some(Position {
        x: x.min(inner.right() - 1),
        y,
    })
}

/// The end of `text` that fits in `width` columns, less one so that the
/// cursor fits after it; whole graphemes only.
pub(crate) fn tail(text: &str, width: usize) -> String {
    let room = width.saturating_sub(1);
    let mut used = 0;
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let next = used + grapheme.width();
        if next > room {
            break;
        }
        used = next;
        start = index;
    }
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draft(text: &str) -> Draft {
        Draft {
            text: text.to_owned(),
        }
    }

    #[test]
    fn edits_apply_at_the_end() {
        let mut d = Draft::default();
        assert!(d.is_empty() && d.message().is_none());
        d.apply(&Edit::Insert("feat: x".into()));
        d.apply(&Edit::Newline);
        d.apply(&Edit::Insert("body".into()));
        assert_eq!(d.subject(), "feat: x");
        assert_eq!(d.body(), "body");
        assert_eq!(d.message().as_deref(), Some("feat: x\n\nbody\n"));
        d.apply(&Edit::Backspace);
        d.apply(&Edit::Backspace);
        d.apply(&Edit::Backspace);
        d.apply(&Edit::Backspace);
        d.apply(&Edit::Backspace);
        assert_eq!(
            d.last_lines(5),
            vec!["feat: x"],
            "backspace crosses the newline"
        );
        d.apply(&Edit::Insert("é".into()));
        d.apply(&Edit::Backspace);
        assert_eq!(d.subject(), "feat: x", "a whole character goes");
        d.apply(&Edit::Insert("e\u{301}".into()));
        d.apply(&Edit::Backspace);
        assert_eq!(d.subject(), "feat: x", "a whole grapheme goes");
        d.clear();
        assert!(d.is_empty());
        d.apply(&Edit::Backspace);
        assert!(d.is_empty(), "nothing to take back");
        assert_eq!(
            draft("   \nbody").message(),
            None,
            "a blank subject is none"
        );
        assert_eq!(draft("\n\n").message(), None, "newlines only");
        assert_eq!(height(&draft("\n\n")), 6);
        assert_eq!(draft("subject\n").message().as_deref(), Some("subject\n"));
        assert_eq!(
            draft("subject\n\n\nbody").message().as_deref(),
            Some("subject\n\nbody\n"),
            "extra blank lines before the body fold"
        );
        assert_eq!(draft("a\nb\nc").last_lines(2), vec!["b", "c"]);
    }

    #[test]
    fn height_follows_the_lines_up_to_a_limit() {
        assert_eq!(height(&Draft::default()), 4);
        assert_eq!(height(&draft("a\nb")), 5);
        assert_eq!(height(&draft("a\n\n\n\n\n\n\n")), 3 + MAX_LINES);
    }

    #[test]
    fn tail_keeps_room_for_the_cursor() {
        assert_eq!(tail("abcdef", 4), "def");
        assert_eq!(tail("ab", 4), "ab");
        assert_eq!(tail("日本語", 4), "語");
        assert_eq!(tail("abc", 1), "");
        assert_eq!(tail("abc", 0), "");
        assert_eq!(tail("ae\u{301}", 2), "e\u{301}", "a grapheme stays whole");
    }

    fn draw(width: u16, height: u16, d: &Draft) -> (Terminal<TestBackend>, Option<Position>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut cursor = None;
        terminal
            .draw(|frame| {
                cursor = render(frame, frame.area(), d);
            })
            .expect("draw");
        (terminal, cursor)
    }

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    #[test]
    fn renders_the_title_the_box_and_the_cursor() {
        let d = draft("feat: a long subject that overflows\nbody");
        let (terminal, cursor) = draw(20, 5, &d);
        assert!(row(&terminal, 0).starts_with("commit"));
        assert!(row(&terminal, 1).starts_with('┌'));
        assert_eq!(row(&terminal, 2), "│ct that overflows │");
        assert_eq!(row(&terminal, 3), "│body              │");
        assert!(row(&terminal, 4).starts_with('└'));
        assert_eq!(cursor, Some(Position { x: 5, y: 3 }));

        // Only the last lines fit; the cursor stays on the last one.
        let many = draft("1\n2\n3\n4");
        let (terminal, cursor) = draw(20, 5, &many);
        assert_eq!(row(&terminal, 2), "│3                 │");
        assert_eq!(row(&terminal, 3), "│4                 │");
        assert_eq!(cursor, Some(Position { x: 2, y: 3 }));

        let (_, cursor) = draw(20, 4, &Draft::default());
        assert_eq!(cursor, Some(Position { x: 1, y: 2 }), "empty draft");
        for (w, h) in [(0, 0), (2, 2), (3, 2), (20, 2), (2, 5)] {
            let (_, cursor) = draw(w, h, &d);
            assert_eq!(cursor, None, "no room at {w}x{h}");
        }
        // A line filling the width exactly scrolls one column to keep
        // the cursor inside.
        let (terminal, cursor) = draw(20, 4, &draft("123456789012345678"));
        assert_eq!(row(&terminal, 2), "│23456789012345678 │");
        assert_eq!(cursor, Some(Position { x: 18, y: 2 }));
    }
}
