//! Text selected in the center pane: the cells a selection covers, the
//! text they show, and the selection drawn reversed over them.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::Center;

/// A cell of what the center pane shows, as (row, column) from its top
/// left. Points compare in reading order.
pub type Point = (u16, u16);

/// The cells of a selection, from `start` to `end`, both included, in
/// reading order: the rest of the first row, whole rows between, the
/// start of the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marked {
    pub start: Point,
    pub end: Point,
}

/// What a selection takes from the cells it is pressed and dragged over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Each cell.
    Cell,
    /// The word under each: a run of characters that are not spaces.
    Word,
    /// The line of each, with the rows it wraps onto.
    Line,
}

/// What the center pane shows of the tab shown, cell by cell, where text
/// is selected: a child's screen, or the viewer's text right of the line
/// numbers.
#[derive(Clone, Copy)]
pub struct Grid<'a> {
    center: Center<'a>,
    /// Where the tab is drawn.
    pane: Rect,
    /// Where the cells are, inside `pane`.
    area: Rect,
}

impl<'a> Grid<'a> {
    /// The cells `center` shows drawn in `area`: the screen's, as far as
    /// both reach, or those of the viewer's lines. None without any: the
    /// viewer without lines, an empty area.
    pub fn new(center: Center<'a>, area: Rect) -> Option<Self> {
        let cells = match center {
            Center::Screen(emulator) => {
                let size = emulator.size();
                Rect {
                    width: area.width.min(size.cols),
                    height: area.height.min(size.rows),
                    ..area
                }
            }
            Center::File(pane) => pane.text_area(area)?,
        };
        (!cells.is_empty()).then_some(Self {
            center,
            pane: area,
            area: cells,
        })
    }

    /// Where its cells are on screen.
    pub fn area(&self) -> Rect {
        self.area
    }

    /// The cell at `position` on screen; a place past an edge counts as
    /// the cell at that edge.
    pub fn point(&self, position: Position) -> Point {
        let last = |size: u16| size.saturating_sub(1);
        let row = position.y.saturating_sub(self.area.y);
        let col = position.x.saturating_sub(self.area.x);
        (
            row.min(last(self.area.height)),
            col.min(last(self.area.width)),
        )
    }

    /// The cells from `anchor` to `head`, whichever comes first, each
    /// taken by `unit`, with a wide character at either end whole.
    pub fn marked(&self, anchor: Point, head: Point, unit: Unit) -> Marked {
        let bounds = |point: Point| {
            let point = self.clamp(point);
            match unit {
                Unit::Cell => (point, point),
                Unit::Word => self.word(point),
                Unit::Line => {
                    let (first, last) = self.line(point.0);
                    ((first, 0), (last, self.area.width - 1))
                }
            }
        };
        let (from, to) = (bounds(anchor), bounds(head));
        self.whole(from.0.min(to.0), from.1.max(to.1))
    }

    /// The text of the cells of `marked`: each row's without the spaces
    /// at its end, then a line end, unless its text goes on in the next
    /// row, where the spaces stay.
    pub fn text(&self, marked: Marked) -> String {
        let (start, end) = (self.clamp(marked.start), self.clamp(marked.end));
        let (start, end) = (start.min(end), start.max(end));
        let mut text = String::new();
        for row in start.0..=end.0 {
            let cells = self.cells(row);
            let first = if row == start.0 { start.1 } else { 0 };
            let last = if row == end.0 {
                end.1
            } else {
                self.area.width - 1
            };
            let part = cells[usize::from(first)..=usize::from(last)].concat();
            if row < end.0 && self.wraps(row) {
                text.push_str(&part);
            } else {
                text.push_str(part.trim_end_matches(' '));
                if row < end.0 {
                    text.push('\n');
                }
            }
        }
        text
    }

    /// `point` moved onto the cells, at their edge when past it.
    fn clamp(&self, (row, col): Point) -> Point {
        (row.min(self.area.height - 1), col.min(self.area.width - 1))
    }

    /// The cells from `start` to `end`, moved out to take a wide
    /// character at either end whole.
    fn whole(&self, mut start: Point, mut end: Point) -> Marked {
        if start.1 > 0 && self.cells(start.0)[usize::from(start.1)].is_empty() {
            start.1 -= 1;
        }
        if self
            .cells(end.0)
            .get(usize::from(end.1) + 1)
            .is_some_and(String::is_empty)
        {
            end.1 += 1;
        }
        Marked { start, end }
    }

    /// What each cell of row `row` shows, as wide as the cells: its text;
    /// a space for a blank cell; nothing for the second cell of a wide
    /// character, which the first one holds whole.
    fn cells(&self, row: u16) -> Vec<String> {
        let width = self.area.width;
        match self.center {
            Center::Screen(emulator) => (0..width)
                .map(|col| emulator.symbol(row, col).to_owned())
                .collect(),
            Center::File(pane) => {
                let line = pane.line(row, self.pane).unwrap_or_default();
                let mut cells: Vec<String> = Vec::with_capacity(usize::from(width));
                for grapheme in line.graphemes(true) {
                    match grapheme.width() {
                        // A mark that takes no room goes with the
                        // character before it.
                        0 => {
                            if let Some(cell) = cells.iter_mut().rev().find(|cell| !cell.is_empty())
                            {
                                cell.push_str(grapheme);
                            }
                        }
                        1 => cells.push(grapheme.to_owned()),
                        _ => {
                            cells.push(grapheme.to_owned());
                            cells.push(String::new());
                        }
                    }
                }
                cells.resize(usize::from(width), " ".to_owned());
                cells
            }
        }
    }

    /// Whether the text of row `row` goes on in the next row: a screen's
    /// row that wrapped; never a line of the viewer, which cuts it.
    fn wraps(&self, row: u16) -> bool {
        match self.center {
            Center::Screen(emulator) => emulator.wraps(row),
            Center::File(_) => false,
        }
    }

    /// The first and the last row of the line on row `row`: the rows
    /// before it that wrap onto it, and those it wraps onto.
    fn line(&self, row: u16) -> (u16, u16) {
        let mut first = row;
        while first > 0 && self.wraps(first - 1) {
            first -= 1;
        }
        let mut last = row;
        while last + 1 < self.area.height && self.wraps(last) {
            last += 1;
        }
        (first, last)
    }

    /// The first and the last cell of the word under `at`, on the rows of
    /// its line; `at` alone when it is a space.
    fn word(&self, at: Point) -> (Point, Point) {
        let (first, last) = self.line(at.0);
        let cells: Vec<(Point, String)> = (first..=last)
            .flat_map(|row| {
                let cells = self.cells(row).into_iter();
                (0..).zip(cells).map(move |(col, text)| ((row, col), text))
            })
            .collect();
        let blank = |index: usize| cells[index].1 == " ";
        let Some(index) = cells.iter().position(|(point, _)| *point == at) else {
            return (at, at);
        };
        if blank(index) {
            return (at, at);
        }
        let mut start = index;
        while start > 0 && !blank(start - 1) {
            start -= 1;
        }
        let mut end = index;
        while end + 1 < cells.len() && !blank(end + 1) {
            end += 1;
        }
        (cells[start].0, cells[end].0)
    }
}

/// Draws the cells of `marked`, counted from the top left of `area`,
/// reversed: a cell drawn reversed already shows plain.
pub fn draw(buffer: &mut Buffer, area: Rect, marked: Marked) {
    if area.is_empty() {
        return;
    }
    let Marked { start, end } = marked;
    for row in start.0..=end.0.min(area.height - 1) {
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 { end.1 } else { u16::MAX };
        for col in first..=last.min(area.width - 1) {
            if let Some(cell) = buffer.cell_mut((area.x + col, area.y + row)) {
                cell.modifier.toggle(Modifier::REVERSED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Size;
    use crate::emu::Emulator;
    use crate::ui::viewer::{Content, Pane, Text};

    /// An emulator of `rows` and `cols` fed `bytes`.
    fn screen(rows: u16, cols: u16, bytes: &str) -> Emulator {
        let mut emulator = Emulator::new(Size { rows, cols }, 0);
        emulator.process(bytes.as_bytes());
        emulator
    }

    /// The grid of `emulator` drawn at the top left of the screen.
    fn drawn(emulator: &Emulator) -> Grid<'_> {
        let Size { rows, cols } = emulator.size();
        Grid::new(Center::Screen(emulator), Rect::new(0, 0, cols, rows)).expect("cells")
    }

    fn marked(start: Point, end: Point) -> Marked {
        Marked { start, end }
    }

    #[test]
    fn the_text_joins_rows_unless_they_wrap_and_cuts_the_spaces_at_their_end() {
        let emulator = screen(5, 6, "one two three\r\nx  y  \r\n日本x");
        let grid = drawn(&emulator);
        assert_eq!(
            grid.text(marked((0, 0), (4, 5))),
            "one two three\nx  y\n日本x",
            "every row"
        );
        // The rest of the first row and the start of the last.
        assert_eq!(grid.text(marked((0, 4), (1, 1))), "two");
        assert_eq!(grid.text(marked((0, 2), (1, 0))), "e two");
        assert_eq!(grid.text(marked((2, 0), (3, 0))), "e\nx");
        assert_eq!(grid.text(marked((3, 0), (3, 0))), "x", "one cell");
        assert_eq!(grid.text(marked((2, 1), (2, 5))), "", "blanks only");
        // The space at the end of a row that wraps stays.
        let spaced = screen(3, 6, "hello world");
        let grid = drawn(&spaced);
        assert_eq!(grid.text(marked((0, 0), (1, 4))), "hello world");
        let line = grid.marked((1, 2), (1, 2), Unit::Line);
        assert_eq!(grid.text(line), "hello world");
        // Blank rows give line ends.
        let short = screen(3, 4, "a");
        assert_eq!(drawn(&short).text(marked((0, 0), (2, 3))), "a\n\n");
    }

    #[test]
    fn wide_characters_are_taken_whole() {
        let emulator = screen(2, 6, "a日本x");
        let grid = drawn(&emulator);
        // From the second cell of 日 to the first of 本.
        let wide = grid.marked((0, 2), (0, 3), Unit::Cell);
        assert_eq!(wide, marked((0, 1), (0, 4)));
        assert_eq!(grid.text(wide), "日本");
        assert_eq!(
            grid.marked((0, 4), (0, 2), Unit::Cell),
            marked((0, 1), (0, 4)),
            "either way"
        );
        assert_eq!(grid.text(marked((0, 0), (0, 0))), "a");
    }

    #[test]
    fn a_word_is_a_run_of_characters_that_are_not_spaces_and_a_line_its_rows() {
        let emulator = screen(4, 6, "one two three\r\n a.b c");
        let grid = drawn(&emulator);
        // `two` wraps from the first row onto the second.
        let two = grid.marked((0, 5), (0, 5), Unit::Word);
        assert_eq!(two, marked((0, 4), (1, 0)));
        assert_eq!(grid.text(two), "two");
        assert_eq!(grid.text(grid.marked((3, 2), (3, 2), Unit::Word)), "a.b");
        let space = grid.marked((3, 0), (3, 0), Unit::Word);
        assert_eq!(space, marked((3, 0), (3, 0)), "a space alone");
        assert_eq!(grid.text(space), "");
        // A drag from a word takes every word up to the one under it.
        let words = grid.marked((0, 5), (1, 4), Unit::Word);
        assert_eq!(grid.text(words), "two three");
        // The line of the second row starts on the first and ends on the
        // third, the rows it wraps onto.
        let line = grid.marked((1, 2), (1, 2), Unit::Line);
        assert_eq!(line, marked((0, 0), (2, 5)));
        assert_eq!(grid.text(line), "one two three");
        assert_eq!(
            grid.text(grid.marked((3, 0), (2, 0), Unit::Line)),
            "one two three\n a.b c",
            "up to another line"
        );
    }

    #[test]
    fn a_place_past_an_edge_counts_as_the_cell_at_that_edge() {
        let emulator = screen(3, 5, "");
        let area = Rect::new(10, 2, 5, 3);
        let grid = Grid::new(Center::Screen(&emulator), area).expect("cells");
        assert_eq!(grid.point(Position::new(12, 3)), (1, 2));
        assert_eq!(grid.point(Position::new(0, 0)), (0, 0));
        assert_eq!(grid.point(Position::new(90, 90)), (2, 4));
        // Only as far as the screen reaches.
        let wider = Grid::new(Center::Screen(&emulator), Rect::new(0, 0, 9, 9)).expect("cells");
        assert_eq!(wider.area(), Rect::new(0, 0, 5, 3));
        assert!(Grid::new(Center::Screen(&emulator), Rect::new(0, 0, 0, 3)).is_none());
    }

    #[test]
    fn the_viewer_gives_its_text_as_drawn_without_the_line_numbers() {
        let content = Content::Text(Text::new("\tlet a = 1;\nshort\n\n日本\n"));
        let pane = Pane {
            content: &content,
            scroll: 0,
        };
        // One digit and a space before the text: 12 columns of it.
        let area = Rect::new(20, 1, 14, 5);
        let grid = Grid::new(Center::File(pane), area).expect("text");
        assert_eq!(grid.area(), Rect::new(22, 1, 12, 5));
        assert_eq!(grid.point(Position::new(20, 2)), (1, 0), "a number");
        // Tabs as four spaces, a line cut as drawn.
        assert_eq!(
            grid.text(marked((0, 0), (4, 11))),
            "    let a =…\nshort\n\n日本\n"
        );
        assert_eq!(grid.text(grid.marked((3, 1), (3, 1), Unit::Cell)), "日");
        assert_eq!(grid.text(grid.marked((1, 3), (1, 3), Unit::Line)), "short");
        assert_eq!(grid.text(grid.marked((0, 5), (0, 5), Unit::Word)), "let");
        // Scrolled, from the line drawn first.
        let scrolled = Pane {
            content: &content,
            scroll: 1,
        };
        let short = Rect::new(0, 0, 14, 2);
        let grid = Grid::new(Center::File(scrolled), short).expect("text");
        assert_eq!(grid.text(marked((0, 0), (1, 11))), "short\n");
        // Without text there is nothing to select.
        let reading = Pane {
            content: &Content::Reading,
            scroll: 0,
        };
        assert!(Grid::new(Center::File(reading), area).is_none());
    }

    #[test]
    fn the_cells_drawn_are_reversed_from_the_start_to_the_end() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        buffer[(3, 2)].modifier = Modifier::REVERSED;
        let area = Rect::new(1, 1, 6, 3);
        draw(&mut buffer, area, marked((0, 4), (2, 1)));
        let reversed: Vec<String> = (0..4)
            .map(|y| {
                (0..8)
                    .map(|x| {
                        let on = buffer[(x, y)].modifier.contains(Modifier::REVERSED);
                        if on { '#' } else { '.' }
                    })
                    .collect()
            })
            .collect();
        assert_eq!(reversed, ["........", ".....##.", ".##.###.", ".##....."]);
        // Nothing outside the area.
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 2));
        draw(&mut buffer, Rect::new(0, 0, 4, 1), marked((0, 2), (5, 9)));
        assert!(buffer[(0, 1)].modifier.is_empty());
        assert!(buffer[(3, 0)].modifier.contains(Modifier::REVERSED));
        draw(&mut buffer, Rect::new(0, 0, 0, 0), marked((0, 0), (0, 0)));
        assert!(buffer[(0, 0)].modifier.is_empty());
    }
}
