//! Coding agents the center pane can run. The core knows only a command
//! on a pty; everything specific to an agent lives behind [`Agent`].

use std::path::Path;
use std::time::Duration;

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

/// What the agent's own telemetry says about its session; anything it
/// did not say is none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The model's display name.
    pub model: Option<String>,
    /// The reasoning effort the agent works at, as it names it.
    pub effort: Option<String>,
    /// Share of the context window in use, 0 to 100.
    pub context: Option<u8>,
    /// What the session cost so far, in US dollars.
    pub cost: Option<f64>,
    /// How much of its plan's usage limits the account has used, when the
    /// agent says.
    pub limits: Limits,
    /// Lines the agent added and removed in the session.
    pub lines: Option<(u64, u64)>,
    /// How long the session had run when the agent wrote its status.
    pub duration: Option<Duration>,
    /// What the agent is doing, from its last event.
    pub state: Option<State>,
    /// The tool the agent is running while it works, as it names it, with
    /// the file the tool works on.
    pub activity: Option<String>,
}

/// The usage limits of the plan the agent's account is on, each while
/// the agent reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// The five-hour window.
    pub five_hours: Option<Limit>,
    /// The seven-day window.
    pub week: Option<Limit>,
}

impl Limits {
    /// Whether the agent reported any limit.
    pub fn any(&self) -> bool {
        self.five_hours.is_some() || self.week.is_some()
    }
}

/// One usage window of a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// Share of the window used, 0 to 100.
    pub used: u8,
    /// When the window starts over, in seconds since the Unix epoch.
    pub resets_at: i64,
}

/// What the agent is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Working on a prompt.
    Working,
    /// Done, waiting for the next prompt.
    Waiting,
    /// Asking the user something: a permission, an answer.
    Attention,
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

    /// The session's status from what its shims wrote: `status`, the last
    /// status line input, and `events`, the hook events file. Either may
    /// be empty. Nothing, unless the agent says otherwise.
    fn status(&self, status: &[u8], events: &[u8]) -> Status {
        let _ = (status, events);
        Status::default()
    }
}

/// The agent used when none is selected.
pub fn default_agent() -> Box<dyn Agent> {
    Box::new(claude::Claude)
}
