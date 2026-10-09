//! Where the panes go.

use ratatui::layout::{Constraint, Layout, Rect};

/// Width of each column, in percent, left to right.
const COLUMNS: [u16; 3] = [20, 55, 25];
/// Below this width the side panes collapse and the center takes it all.
pub const COLLAPSE_BELOW: u16 = 100;

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
    let [body, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    if !sides_visible(area) {
        let empty = |x: u16| Rect::new(x, body.y, 0, body.height);
        return Panes {
            left: empty(body.x),
            center: body,
            right: empty(body.right()),
            status,
        };
    }
    let [left, center, right] = Layout::horizontal(COLUMNS.map(Constraint::Percentage)).areas(body);
    Panes {
        left,
        center,
        right,
        status,
    }
}

/// Whether the side panes are shown for a terminal of `area`.
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
