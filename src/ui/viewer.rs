//! The viewer in the center pane: a file's lines as they were when it
//! was read, with their numbers, or why it is not shown.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::changes::{cut, printable};
use super::palette::{ALERT, MUTED, TEXT};
use super::render_empty;
use crate::strings;

/// A file's text as the viewer draws it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Text {
    /// Its lines, tabs expanded and control characters shown.
    lines: Vec<String>,
}

impl Text {
    /// The lines of `text`, where a final newline adds no empty last
    /// line: tabs as four spaces, as the diff view has them, and control
    /// characters shown as the changes pane shows them.
    pub fn new(text: &str) -> Self {
        Self {
            lines: text
                .lines()
                .map(|line| printable(&line.replace('\t', "    ")))
                .collect(),
        }
    }

    /// How many lines it has.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether it has no lines: an empty file.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// What the viewer shows of its file.
#[derive(Debug, PartialEq, Eq)]
pub enum Content {
    /// Nothing yet: the file is being read.
    Reading,
    /// The file's text.
    Text(Text),
    /// Why the file is not shown: binary, too large, not a regular file.
    Note(&'static str),
    /// Why the file could not be read.
    Failed(String),
}

impl Content {
    /// How many lines it has to scroll through: none but a text's.
    pub fn lines(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Reading | Self::Note(_) | Self::Failed(_) => 0,
        }
    }
}

/// What the center pane shows of the viewer.
#[derive(Clone, Copy, Debug)]
pub struct Pane<'a> {
    /// The file's text, or why there is none.
    pub content: &'a Content,
    /// The first line on screen.
    pub scroll: usize,
}

impl Pane<'_> {
    /// Where [`render`] draws the lines of the text in `area`: right of
    /// their numbers and the space after them. None without lines: no
    /// text, or an empty file.
    pub fn text_area(&self, area: Rect) -> Option<Rect> {
        let Content::Text(text) = self.content else {
            return None;
        };
        if text.is_empty() {
            return None;
        }
        let gutter = u16::try_from(digits(text) + 1)
            .unwrap_or(u16::MAX)
            .min(area.width);
        Some(Rect {
            x: area.x + gutter,
            width: area.width - gutter,
            ..area
        })
    }

    /// The line [`render`] draws on row `row` of `area`, without its
    /// number and cut as drawn. None past the last line, and without
    /// text.
    pub fn line(&self, row: u16, area: Rect) -> Option<String> {
        let Content::Text(text) = self.content else {
            return None;
        };
        let top = top(text, self.scroll, usize::from(area.height));
        let line = text.lines.get(top + usize::from(row))?;
        let room = self.text_area(area).map_or(0, |text| text.width);
        Some(cut(line, usize::from(room)))
    }
}

/// Draws `pane` in `area`: the text from line `scroll` on, moved back so
/// that the last line stays at the bottom, each line after its number;
/// or, muted and centred, that the file is being read, is empty or is
/// not shown and why; or why it could not be read, in the alert colour.
pub fn render(frame: &mut Frame, area: Rect, pane: &Pane<'_>) {
    match pane.content {
        Content::Reading => render_empty(frame, area, strings::FILE_READING),
        Content::Text(text) if text.is_empty() => render_empty(frame, area, strings::FILE_EMPTY),
        Content::Text(text) => render_text(frame, area, text, pane.scroll),
        Content::Note(note) => render_empty(frame, area, note),
        Content::Failed(error) => {
            let text = printable(&strings::file_trouble(error));
            let trouble = Paragraph::new(text)
                .style(Style::new().fg(ALERT))
                .wrap(Wrap { trim: true });
            frame.render_widget(trouble, area);
        }
    }
}

/// Draws the lines of `text` that fit `area` from line `scroll` on, as
/// [`render`] says: the number muted, right-aligned to the width of the
/// last line's, a space, then the line in the terminal's colour, cut to
/// the room left.
fn render_text(frame: &mut Frame, area: Rect, text: &Text, scroll: usize) {
    let height = usize::from(area.height);
    let scroll = top(text, scroll, height);
    let digits = digits(text);
    let room = usize::from(area.width).saturating_sub(digits + 1);
    let lines: Vec<Line> = text
        .lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, line)| {
            Line::from(vec![
                Span::styled(format!("{:>digits$} ", index + 1), Style::new().fg(MUTED)),
                Span::styled(cut(line, room), Style::new().fg(TEXT)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The line of `text` drawn first, `scroll` wanting it, in a pane
/// `height` lines tall: moved back so that the last line stays at the
/// bottom.
fn top(text: &Text, scroll: usize, height: usize) -> usize {
    scroll.min(text.len().saturating_sub(height))
}

/// How many digits the numbers of the lines of `text` take: those of the
/// last one's.
fn digits(text: &Text) -> usize {
    text.len().to_string().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn draw(width: u16, height: u16, content: &Content, scroll: usize) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let pane = Pane { content, scroll };
        terminal
            .draw(|frame| render(frame, frame.area(), &pane))
            .expect("draw");
        terminal
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

    /// The rows of `terminal`, their trailing spaces trimmed.
    fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let height = terminal.backend().buffer().area.height;
        (0..height)
            .map(|y| row(terminal, y).trim_end().to_owned())
            .collect()
    }

    /// `count` numbered lines of text.
    fn numbered(count: usize) -> Content {
        let text: String = (1..=count).map(|n| format!("line {n}\n")).collect();
        Content::Text(Text::new(&text))
    }

    #[test]
    fn a_final_newline_adds_no_line_and_tabs_and_controls_show() {
        assert_eq!(Text::new("a\nb\n").len(), 2);
        assert_eq!(Text::new("a\nb").len(), 2);
        assert_eq!(Text::new("\n").len(), 1, "one empty line");
        assert!(Text::new("").is_empty());
        let text = Text::new("\tx\r\ny\x1b[1mz\x07\n");
        assert_eq!(text.lines, ["    x", "y\u{FFFD}[1mz\u{FFFD}"]);
        assert_eq!(numbered(3).lines(), 3);
        assert_eq!(Content::Reading.lines(), 0);
        assert_eq!(Content::Note(strings::FILE_BINARY).lines(), 0);
    }

    #[test]
    fn numbers_are_muted_and_right_aligned_to_the_last_one() {
        let terminal = draw(12, 3, &numbered(9), 0);
        assert_eq!(rows(&terminal), ["1 line 1", "2 line 2", "3 line 3"]);
        assert_eq!(fg(&terminal, 0, 0), MUTED, "the number");
        assert_eq!(fg(&terminal, 2, 0), TEXT, "the text");
        let terminal = draw(12, 3, &numbered(10), 0);
        assert_eq!(rows(&terminal), [" 1 line 1", " 2 line 2", " 3 line 3"]);
        let terminal = draw(14, 2, &numbered(100), 98);
        assert_eq!(rows(&terminal), [" 99 line 99", "100 line 100"]);
    }

    #[test]
    fn long_lines_are_cut_at_the_width() {
        let content = Content::Text(Text::new("short\n\tindented and quite long\n"));
        let terminal = draw(12, 2, &content, 0);
        assert_eq!(rows(&terminal), ["1 short", "2     inden…"]);
        // Wide characters are cut whole; the second cell of each reads
        // as a space here.
        let wide = Content::Text(Text::new("日本語のテキスト\n"));
        let terminal = draw(9, 1, &wide, 0);
        assert_eq!(rows(&terminal), ["1 日 本 語 …"]);
    }

    #[test]
    fn the_scroll_stops_with_the_last_line_at_the_bottom() {
        let content = numbered(5);
        let terminal = draw(10, 3, &content, 1);
        assert_eq!(rows(&terminal), ["2 line 2", "3 line 3", "4 line 4"]);
        let terminal = draw(10, 3, &content, 100);
        assert_eq!(rows(&terminal), ["3 line 3", "4 line 4", "5 line 5"]);
        let terminal = draw(10, 8, &content, 3);
        assert_eq!(
            rows(&terminal)[..6],
            [
                "1 line 1", "2 line 2", "3 line 3", "4 line 4", "5 line 5", ""
            ]
        );
    }

    #[test]
    fn the_text_goes_right_of_the_numbers_each_line_cut_as_drawn() {
        let content = numbered(12);
        let pane = Pane {
            content: &content,
            scroll: 100,
        };
        // Two digits and a space before the text.
        let area = Rect::new(5, 2, 10, 3);
        assert_eq!(pane.text_area(area), Some(Rect::new(8, 2, 7, 3)));
        assert_eq!(pane.line(0, area).as_deref(), Some("line 10"));
        assert_eq!(pane.line(2, area).as_deref(), Some("line 12"));
        assert_eq!(pane.line(3, area), None, "past the last line");
        let narrow = Rect::new(0, 0, 8, 3);
        assert_eq!(pane.line(0, narrow).as_deref(), Some("line…"));
        let terminal = draw(8, 3, &content, 100);
        assert_eq!(rows(&terminal)[0], "10 line…");
        // Narrower than the numbers, the text has no room.
        assert_eq!(
            pane.text_area(Rect::new(0, 0, 2, 3)),
            Some(Rect::new(2, 0, 0, 3))
        );
        let reading = Pane {
            content: &Content::Reading,
            scroll: 0,
        };
        assert_eq!(reading.text_area(area), None);
        assert_eq!(reading.line(0, area), None);
        let empty = Content::Text(Text::new(""));
        let empty = Pane {
            content: &empty,
            scroll: 0,
        };
        assert_eq!(empty.text_area(area), None, "an empty file");
    }

    #[test]
    fn says_when_there_is_nothing_to_show_and_why() {
        let all = |terminal: &Terminal<TestBackend>| rows(terminal).join("\n");
        let terminal = draw(40, 3, &Content::Reading, 0);
        assert_eq!(rows(&terminal)[1].trim(), strings::FILE_READING);
        assert_eq!(fg(&terminal, 12, 1), MUTED);
        let terminal = draw(40, 3, &Content::Text(Text::new("")), 0);
        assert_eq!(rows(&terminal)[1].trim(), strings::FILE_EMPTY);
        for note in [
            strings::FILE_BINARY,
            strings::FILE_TOO_LARGE,
            strings::FILE_NOT_REGULAR,
        ] {
            let terminal = draw(40, 3, &Content::Note(note), 0);
            assert_eq!(rows(&terminal)[1].trim(), note);
        }
        let failed = Content::Failed("read /p/a.txt: Permission denied".into());
        let terminal = draw(30, 3, &failed, 0);
        assert_eq!(
            all(&terminal),
            "file unavailable: read\n/p/a.txt: Permission denied\n"
        );
        assert_eq!(fg(&terminal, 0, 0), ALERT);
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 3), (3, 1)] {
            for content in [numbered(12), Content::Reading, Content::Failed("x".into())] {
                draw(w, h, &content, 5);
            }
        }
    }
}
