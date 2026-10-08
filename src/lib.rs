//! Library half of `term`. Everything the binary calls lives here, so a
//! module can land with its tests before the UI uses it.

/// Package version, as reported by `term --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod emu;
pub mod input;
pub mod pty;

/// Size of a terminal, in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}
