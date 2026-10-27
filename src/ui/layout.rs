//! Where the panes go.

use ratatui::layout::{Constraint, Layout, Rect};

/// Width of each column, in percent, left to right.
const COLUMNS: [u16; 3] = [20, 55, 25];
/// Below this width the side panes collapse and the center takes it all.
pub const COLLAPSE_BELOW: u16 = 100;

/// The side panes on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sides {
    /// Neither: the center spans the width.
    None,
    /// The projects pane alone: the center takes the changes pane's
    /// width too.
    Projects,
    /// Both.
    Both,
}

/// The areas of the three panes and the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes {
    pub left: Rect,
    pub center: Rect,
    pub right: Rect,
    pub status: Rect,
}

/// Splits `area` into three columns over a one-line status bar. Narrower
/// than [`COLLAPSE_BELOW`], the side panes are empty and the center spans
/// the width.
pub fn panes(area: Rect) -> Panes {
    let sides = if sides_visible(area) {
        Sides::Both
    } else {
        Sides::None
    };
    panes_with(area, sides)
}

/// Like [`panes`], with the side panes `sides` asks for. A pane not
/// shown is empty, at the edge of the center, which takes its width.
pub fn panes_with(area: Rect, sides: Sides) -> Panes {
    let [body, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let empty = |x: u16| Rect::new(x, body.y, 0, body.height);
    if sides == Sides::None {
        return Panes {
            left: empty(body.x),
            center: body,
            right: empty(body.right()),
            status,
        };
    }
    let [left, mut center, mut right] =
        Layout::horizontal(COLUMNS.map(Constraint::Percentage)).areas(body);
    if sides == Sides::Projects {
        center.width += right.width;
        right = empty(body.right());
    }
    Panes {
        left,
        center,
        right,
        status,
    }
}

/// Whether a terminal of `area` is wide enough for the side panes.
pub fn sides_visible(area: Rect) -> bool {
    area.width >= COLLAPSE_BELOW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_hundred_columns_exactly() {
        let p = panes(Rect::new(0, 0, 100, 40));
        assert_eq!(p.left, Rect::new(0, 0, 20, 39));
        assert_eq!(p.center, Rect::new(20, 0, 55, 39));
        assert_eq!(p.right, Rect::new(75, 0, 25, 39));
        assert_eq!(p.status, Rect::new(0, 39, 100, 1));
    }

    #[test]
    fn columns_always_cover_the_width() {
        for width in [100u16, 101, 133, 200, 333] {
            let area = Rect::new(0, 0, width, 10);
            let p = panes(area);
            assert_eq!(p.left.x, 0);
            assert_eq!(p.left.width + p.center.width + p.right.width, width);
            assert_eq!(p.center.x, p.left.width);
            assert_eq!(p.right.x, p.left.width + p.center.width);
            assert!(p.left.width > 0 && p.right.width > 0);
            assert!(sides_visible(area));
        }
    }

    #[test]
    fn sides_can_be_hidden_on_a_wide_terminal() {
        let area = Rect::new(0, 0, 120, 10);
        let p = panes_with(area, Sides::None);
        assert_eq!(p.center, Rect::new(0, 0, 120, 9));
        assert_eq!(p.left.width, 0);
        assert_eq!(p.right.width, 0);
        assert_eq!(panes_with(area, Sides::Both), panes(area));
    }

    #[test]
    fn the_center_takes_the_width_of_the_changes_pane_hidden() {
        // Width, then the left pane's width and the center's with both
        // side panes, and the center's with the projects pane alone.
        for (width, left, both, alone) in [(100u16, 20, 55, 80), (140, 28, 77, 112)] {
            let area = Rect::new(0, 0, width, 10);
            let p = panes_with(area, Sides::Both);
            assert_eq!((p.left.width, p.center.width), (left, both), "{width}");
            let alone_panes = panes_with(area, Sides::Projects);
            assert_eq!(alone_panes.left, p.left, "the projects pane stays");
            assert_eq!(alone_panes.center, Rect::new(left, 0, alone, 9));
            assert_eq!(alone_panes.right, Rect::new(width, 0, 0, 9));
            assert_eq!(alone_panes.status, p.status);
        }
    }

    #[test]
    fn narrow_terminals_collapse_the_sides() {
        for width in [0u16, 1, 40, 80, 99] {
            let area = Rect::new(3, 2, width, 10);
            let p = panes(area);
            assert_eq!(p.center, Rect::new(3, 2, width, 9), "width {width}");
            assert_eq!(p.left, Rect::new(3, 2, 0, 9));
            assert_eq!(p.right, Rect::new(3 + width, 2, 0, 9));
            assert_eq!(p.status, Rect::new(3, 11, width, 1));
            assert!(!sides_visible(area));
        }
    }
}
