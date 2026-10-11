//! Coding agents the center pane can run. The core knows only a command
//! on a pty; everything specific to an agent lives behind [`Agent`].

use std::path::Path;

use anyhow::Result;

use crate::Size;
use crate::instance::Instance;
use crate::pty::Spec;

pub mod claude;

/// Where and how a session runs.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// The directory the agent runs in.
    pub cwd: &'a Path,
    /// The repository root around `cwd`, or `cwd` outside a repository.
    pub root: &'a Path,
    /// The user's extra arguments for the agent.
    pub args: &'a [String],
}

/// What the core needs to know about an agent.
pub trait Agent {
    /// Name shown in the pane title.
    fn name(&self) -> &str;

    /// Readies `instance` for the next session of `launch`: whatever the
    /// agent's shims will need there. Runs before every launch. Nothing,
    /// unless the agent says otherwise.
    fn prepare(&self, launch: &Launch<'_>, instance: &Instance) -> Result<()> {
        let _ = (launch, instance);
        Ok(())
    }

    /// Command for a session of `launch` on a terminal of `size`, with
    /// `instance` for its telemetry, or none when there is no instance.
    fn spec(&self, launch: &Launch<'_>, size: Size, instance: Option<&Instance>) -> Spec;
}

/// The agent used when none is selected.
pub fn default_agent() -> Box<dyn Agent> {
    Box::new(claude::Claude)
}
