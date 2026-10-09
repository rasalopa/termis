//! Where the panes go.

use ratatui::layout::{Constraint, Layout, Rect};

/// Width of each column, in percent, left to right.
const COLUMNS: [u16; 3] = [20, 55, 25];

/// The areas of the three panes and the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes {
    pub left: Rect,
    pub center: Rect,
    pub right: Rect,
    pub status: Rect,
}

/// Splits `area` into three columns over a one-line status bar.
pub fn panes(area: Rect) -> Panes {
    let [body, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, center, right] = Layout::horizontal(COLUMNS.map(Constraint::Percentage)).areas(body);
    Panes {
        left,
        center,
        right,
        status,
    }
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
        for width in [40u16, 81, 99, 133, 200] {
            let p = panes(Rect::new(0, 0, width, 10));
            assert_eq!(p.left.x, 0);
            assert_eq!(p.left.width + p.center.width + p.right.width, width);
            assert_eq!(p.center.x, p.left.width);
            assert_eq!(p.right.x, p.left.width + p.center.width);
        }
    }
}
