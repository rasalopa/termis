//! Where the panes go.

use std::ops::RangeInclusive;

use ratatui::layout::{Constraint, Layout, Rect};

/// Below this width the side panes collapse and the center takes it all.
pub const COLLAPSE_BELOW: u16 = 100;
/// The percents a side pane can take of the terminal's width.
const SIDE: RangeInclusive<u16> = 15..=40;
/// The most percent both side panes take together, which leaves the
/// center at least 40 columns at [`COLLAPSE_BELOW`].
const SIDES: u16 = 60;

/// The widths of the side panes, in percent of the terminal's width; the
/// center takes the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widths {
    projects: u16,
    changes: u16,
}

impl Widths {
    /// The projects pane at 20% and the changes pane at 25%, which leave
    /// the center 55%.
    pub const DEFAULT: Self = Self {
        projects: 20,
        changes: 25,
    };

    /// Side panes of `projects` and `changes` percent, each a percent
    /// [`Widths::side`] takes, together at most 60, so that at
    /// [`COLLAPSE_BELOW`] columns each has at least 15 and the center at
    /// least 40. Else why not.
    pub fn new(projects: u16, changes: u16) -> Result<Self, String> {
        Self::side("projects", projects.into())?;
        Self::side("changes", changes.into())?;
        if projects + changes > SIDES {
            return Err(format!(
                "projects and changes together leave the agent less than {}% of the width",
                100 - SIDES
            ));
        }
        Ok(Self { projects, changes })
    }

    /// `percent` as the width of the side pane `name`: from 15 to 40.
    /// Else why not, naming it.
    pub fn side(name: &str, percent: i64) -> Result<u16, String> {
        u16::try_from(percent)
            .ok()
            .filter(|percent| SIDE.contains(percent))
            .ok_or_else(|| {
                format!(
                    "{name} is a percent from {} to {}",
                    SIDE.start(),
                    SIDE.end()
                )
            })
    }

    /// The projects pane's percent.
    pub fn projects(self) -> u16 {
        self.projects
    }

    /// The changes pane's percent.
    pub fn changes(self) -> u16 {
        self.changes
    }

    /// The percents of the three columns, left to right.
    fn columns(self) -> [u16; 3] {
        [
            self.projects,
            100 - self.projects - self.changes,
            self.changes,
        ]
    }
}

impl Default for Widths {
    fn default() -> Self {
        Self::DEFAULT
    }
}

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

/// Splits `area` into three columns of the default widths over a
/// one-line status bar. Narrower than [`COLLAPSE_BELOW`], the side panes
/// are empty and the center spans the width.
pub fn panes(area: Rect) -> Panes {
    let sides = if sides_visible(area) {
        Sides::Both
    } else {
        Sides::None
    };
    panes_with(area, sides, Widths::DEFAULT)
}

/// Like [`panes`], with the side panes `sides` asks for, of `widths`. A
/// pane not shown is empty, at the edge of the center, which takes its
/// width.
pub fn panes_with(area: Rect, sides: Sides, widths: Widths) -> Panes {
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
        Layout::horizontal(widths.columns().map(Constraint::Percentage)).areas(body);
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
        let p = panes_with(area, Sides::None, Widths::DEFAULT);
        assert_eq!(p.center, Rect::new(0, 0, 120, 9));
        assert_eq!(p.left.width, 0);
        assert_eq!(p.right.width, 0);
        assert_eq!(panes_with(area, Sides::Both, Widths::DEFAULT), panes(area));
    }

    #[test]
    fn the_center_takes_the_width_of_the_changes_pane_hidden() {
        // Width, then the left pane's width and the center's with both
        // side panes, and the center's with the projects pane alone.
        for (width, left, both, alone) in [(100u16, 20, 55, 80), (140, 28, 77, 112)] {
            let area = Rect::new(0, 0, width, 10);
            let p = panes_with(area, Sides::Both, Widths::DEFAULT);
            assert_eq!((p.left.width, p.center.width), (left, both), "{width}");
            let alone_panes = panes_with(area, Sides::Projects, Widths::DEFAULT);
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

    #[test]
    fn a_side_pane_takes_15_to_40_percent_and_both_at_most_60() {
        assert_eq!(Widths::default(), Widths::DEFAULT);
        assert_eq!(Widths::new(20, 25), Ok(Widths::DEFAULT));
        for (projects, changes) in [(15, 15), (15, 40), (40, 20), (20, 40), (30, 30)] {
            let widths = Widths::new(projects, changes).expect("allowed");
            assert_eq!((widths.projects(), widths.changes()), (projects, changes));
        }
        let out = |name: &str| format!("{name} is a percent from 15 to 40");
        assert_eq!(Widths::new(14, 25), Err(out("projects")));
        assert_eq!(Widths::new(41, 15), Err(out("projects")));
        assert_eq!(Widths::new(20, 14), Err(out("changes")));
        assert_eq!(Widths::new(20, 41), Err(out("changes")));
        let together = "projects and changes together leave the agent less than 40% of the width";
        assert_eq!(Widths::new(40, 25), Err(together.to_owned()));
        assert_eq!(Widths::new(21, 40), Err(together.to_owned()));
        for percent in [-3, 0, 14, 41, 70000] {
            let side = Widths::side("changes", percent);
            assert_eq!(side, Err(out("changes")), "{percent}");
        }
        assert_eq!(Widths::side("changes", 15), Ok(15));
        assert_eq!(Widths::side("changes", 40), Ok(40));
    }

    #[test]
    fn every_allowed_pair_covers_the_width_and_leaves_room_at_the_fold() {
        let mut pairs = 0;
        for projects in SIDE {
            for changes in SIDE.filter(|changes| projects + changes <= SIDES) {
                pairs += 1;
                let widths = Widths::new(projects, changes).expect("allowed");
                for width in [COLLAPSE_BELOW, 133] {
                    let p = panes_with(Rect::new(0, 0, width, 10), Sides::Both, widths);
                    let case = format!("{projects} {changes} at {width}");
                    assert_eq!(p.left.x, 0, "{case}");
                    assert_eq!(p.center.x, p.left.right(), "{case}");
                    assert_eq!(p.right.x, p.center.right(), "{case}");
                    assert_eq!(p.right.right(), width, "{case}");
                    // Each within a column of its share, rounded down.
                    let center = 100 - projects - changes;
                    for (pane, percent) in
                        [(p.left, projects), (p.center, center), (p.right, changes)]
                    {
                        let share = percent * width / 100;
                        assert!(pane.width.abs_diff(share) <= 1, "{case}: {pane:?}");
                    }
                    if width == COLLAPSE_BELOW {
                        assert!(p.left.width >= 15 && p.right.width >= 15, "{case}");
                        assert!(p.center.width >= 40, "{case}");
                    }
                }
            }
        }
        assert_eq!(pairs, 466);
    }

    #[test]
    fn the_center_takes_the_hidden_changes_panes_width_whatever_the_widths() {
        let area = Rect::new(0, 0, 100, 10);
        let widths = Widths::new(40, 15).expect("allowed");
        let both = panes_with(area, Sides::Both, widths);
        assert_eq!(
            (both.left.width, both.center.width, both.right.width),
            (40, 45, 15)
        );
        let alone = panes_with(area, Sides::Projects, widths);
        assert_eq!(alone.left, both.left);
        assert_eq!(alone.center, Rect::new(40, 0, 60, 9));
        assert_eq!(alone.right, Rect::new(100, 0, 0, 9));
    }
}
