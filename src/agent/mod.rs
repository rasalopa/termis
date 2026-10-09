//! Coding agents the center pane can run. The core knows only a command
//! on a pty; everything specific to an agent lives behind [`Agent`].

use std::path::Path;

use crate::Size;
use crate::pty::Spec;

pub mod claude;

/// What the core needs to know about an agent.
pub trait Agent {
    /// Name shown in the pane title.
    fn name(&self) -> &str;

    /// Command to run in `cwd` with the user's extra arguments, on a
    /// terminal of `size`.
    fn spec(&self, cwd: &Path, args: &[String], size: Size) -> Spec;
}

/// The agent used when none is selected.
pub fn default_agent() -> Box<dyn Agent> {
    Box::new(claude::Claude)
}
