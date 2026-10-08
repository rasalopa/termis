//! Library half of `term`. Everything the binary calls lives here, so a
//! module can land with its tests before the UI uses it.

/// Package version, as reported by `term --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod pty;
