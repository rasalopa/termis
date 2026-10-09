//! Library half of `term`. Everything the binary calls lives here, so a
//! module can land with its tests before the UI uses it.

/// Package version, as reported by `term --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent;
pub mod app;
pub mod emu;
pub mod git;
pub mod input;
pub mod keys;
pub mod pty;
pub mod strings;
pub mod ui;

/// Size of a terminal, in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

impl From<ratatui::layout::Rect> for Size {
    fn from(area: ratatui::layout::Rect) -> Self {
        Self {
            rows: area.height,
            cols: area.width,
        }
    }
}
