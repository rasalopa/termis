//! Selecting text in the center pane with the left button, where the
//! child of the tab shown does not take the mouse: a press, its drags and
//! its release, double and triple clicks, and the tab a selection is made
//! in.

use std::time::{Duration, Instant};

use ratatui::layout::{Position, Rect};

use super::session::{SessionId, TabId};
use crate::ui::select::{Grid, Marked, Point, Unit};

/// How soon a press on the cell of the press before it counts as the
/// second of a double click, or the third of a triple one.
const CLICK_WITHIN: Duration = Duration::from_millis(400);

/// What the left button does over the center pane while it selects text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pointer {
    /// It is pressed: a selection starts.
    Press,
    /// It moves, held: the selection follows.
    Drag,
    /// It is released: the selection ends there.
    Release,
}

/// Where a selection is made: the tab shown, with its session, and where
/// its screen is. Another tab shown, or a screen moved or resized, ends
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Over {
    pub(super) session: SessionId,
    pub(super) tab: TabId,
    pub(super) screen: Rect,
}

/// Text selected in the center pane, from the cell pressed to the one the
/// pointer is on, or was on at the release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    over: Over,
    /// The cell pressed.
    anchor: Point,
    /// The cell the pointer is on, or was on at the release.
    head: Point,
    /// What it takes from the cells: each, or the word or line of each.
    unit: Unit,
    /// Whether the pointer left the cell pressed: until then a single
    /// press selects nothing.
    dragged: bool,
    /// Whether the button is still held.
    held: bool,
    /// The cells selected as of the last press, drag or release.
    marked: Marked,
}

impl Selection {
    /// The selection a press at `position` starts on `grid`, taking each
    /// `unit` there, made `over` a tab.
    pub(super) fn press(over: Over, grid: &Grid<'_>, position: Position, unit: Unit) -> Self {
        let anchor = grid.point(position);
        Self {
            over,
            anchor,
            head: anchor,
            unit,
            dragged: false,
            held: true,
            marked: grid.marked(anchor, anchor, unit),
        }
    }

    /// Follows the pointer to `position` on `grid`; past an edge, to the
    /// cell at that edge.
    pub(super) fn drag(&mut self, grid: &Grid<'_>, position: Position) {
        self.head = grid.point(position);
        self.dragged |= self.head != self.anchor;
        self.marked = grid.marked(self.anchor, self.head, self.unit);
    }

    /// Ends it at `position` on `grid`, the button released. None when it
    /// selects nothing: a single press released on the cell it pressed.
    pub(super) fn release(mut self, grid: &Grid<'_>, position: Position) -> Option<Self> {
        self.drag(grid, position);
        self.held = false;
        self.selects().then_some(self)
    }

    /// Where it was made.
    pub(super) fn over(&self) -> Over {
        self.over
    }

    /// Whether the button that makes it is still held.
    pub(super) fn held(&self) -> bool {
        self.held
    }

    /// The cells it selects, drawn reversed; none for a single press that
    /// has not left the cell it pressed.
    pub(super) fn marked(&self) -> Option<Marked> {
        self.selects().then_some(self.marked)
    }

    /// Whether it selects anything: a word or a line does at once, cells
    /// once the pointer left the one pressed.
    fn selects(&self) -> bool {
        self.unit != Unit::Cell || self.dragged
    }
}

/// The last press that started a selection, for double and triple
/// clicks: when and where it was, and how many presses on that cell led
/// up to it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Clicks {
    last: Option<(Instant, Position, u8)>,
}

impl Clicks {
    /// What a press at `position` at `now` selects: the word under it when
    /// it is the second on its cell, each within [`CLICK_WITHIN`] of the
    /// one before, the line the third, and cells again the fourth; cells
    /// otherwise.
    pub(super) fn press(&mut self, position: Position, now: Instant) -> Unit {
        let count = match self.last {
            Some((at, last, count))
                if last == position && now.saturating_duration_since(at) <= CLICK_WITHIN =>
            {
                count % 3 + 1
            }
            _ => 1,
        };
        self.last = Some((now, position, count));
        match count {
            1 => Unit::Cell,
            2 => Unit::Word,
            _ => Unit::Line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Size;
    use crate::app::session::Sessions;
    use crate::app::session::tests::tab;
    use crate::emu::Emulator;
    use crate::ui::Center;

    /// Where the screen is drawn: a cell in from the top left, as in a
    /// framed pane.
    const SCREEN: Rect = Rect::new(1, 1, 6, 4);

    fn over() -> Over {
        Over {
            session: Sessions::new().next_id(),
            tab: tab(0),
            screen: SCREEN,
        }
    }

    /// A screen of [`SCREEN`]'s size fed `bytes`.
    fn screen(bytes: &str) -> Emulator {
        let mut emulator = Emulator::new(Size::from(SCREEN), 0);
        emulator.process(bytes.as_bytes());
        emulator
    }

    /// Position `(x, y)` on the terminal.
    fn at(x: u16, y: u16) -> Position {
        Position::new(x, y)
    }

    #[test]
    fn a_drag_selects_from_the_cell_pressed_to_the_one_released_on() {
        let emulator = screen("one\r\ntwo\r\nthree");
        let grid = Grid::new(Center::Screen(&emulator), SCREEN).expect("cells");
        // From `n` of `one` to `h` of `three`, across the rows between.
        let mut selection = Selection::press(over(), &grid, at(2, 1), Unit::Cell);
        assert!(selection.held());
        assert_eq!(selection.marked(), None, "nothing before a drag");
        selection.drag(&grid, at(3, 2));
        let marked = Marked {
            start: (0, 1),
            end: (1, 2),
        };
        assert_eq!(selection.marked(), Some(marked));
        let selection = selection.release(&grid, at(2, 3)).expect("a selection");
        assert!(!selection.held());
        let marked = selection.marked().expect("cells");
        assert_eq!(grid.text(marked), "ne\ntwo\nth");
        // Upwards it reads the same way.
        let mut up = Selection::press(over(), &grid, at(2, 3), Unit::Cell);
        up.drag(&grid, at(2, 1));
        assert_eq!(
            up.marked().map(|marked| grid.text(marked)).as_deref(),
            Some("ne\ntwo\nth")
        );
    }

    #[test]
    fn a_drag_past_the_edges_stops_at_them() {
        let emulator = screen("one\r\ntwo\r\nthree");
        let grid = Grid::new(Center::Screen(&emulator), SCREEN).expect("cells");
        let mut selection = Selection::press(over(), &grid, at(2, 2), Unit::Cell);
        selection.drag(&grid, at(90, 30));
        assert_eq!(
            selection.marked(),
            Some(Marked {
                start: (1, 1),
                end: (3, 5)
            }),
            "to the last cell"
        );
        selection.drag(&grid, at(0, 0));
        assert_eq!(
            selection.marked(),
            Some(Marked {
                start: (0, 0),
                end: (1, 1)
            }),
            "to the first cell, over the frame"
        );
    }

    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        let emulator = screen("one");
        let grid = Grid::new(Center::Screen(&emulator), SCREEN).expect("cells");
        let selection = Selection::press(over(), &grid, at(2, 1), Unit::Cell);
        assert_eq!(selection.release(&grid, at(2, 1)), None);
        // One that left its cell and came back keeps that cell.
        let mut back = Selection::press(over(), &grid, at(2, 1), Unit::Cell);
        back.drag(&grid, at(3, 1));
        let back = back.release(&grid, at(2, 1)).expect("a selection");
        assert_eq!(
            back.marked().map(|marked| grid.text(marked)).as_deref(),
            Some("n")
        );
        // A word or a line is selected at the press.
        let word = Selection::press(over(), &grid, at(2, 1), Unit::Word);
        let word = word.release(&grid, at(2, 1)).expect("a word");
        assert_eq!(
            word.marked().map(|marked| grid.text(marked)).as_deref(),
            Some("one")
        );
        assert_eq!(word.over(), over());
    }

    #[test]
    fn a_second_press_on_the_same_cell_soon_takes_a_word_and_a_third_a_line() {
        let start = Instant::now();
        let later = |ms| start + Duration::from_millis(ms);
        let mut clicks = Clicks::default();
        assert_eq!(clicks.press(at(3, 2), start), Unit::Cell);
        assert_eq!(clicks.press(at(3, 2), later(300)), Unit::Word);
        assert_eq!(
            clicks.press(at(3, 2), later(700)),
            Unit::Line,
            "from the second"
        );
        assert_eq!(clicks.press(at(3, 2), later(800)), Unit::Cell, "the fourth");
        assert_eq!(clicks.press(at(3, 2), later(900)), Unit::Word);
        // Too late, or on another cell, it starts over.
        assert_eq!(clicks.press(at(3, 2), later(1301)), Unit::Cell, "too late");
        assert_eq!(clicks.press(at(4, 2), later(1400)), Unit::Cell, "elsewhere");
        assert_eq!(clicks.press(at(4, 2), later(1500)), Unit::Word);
    }
}
