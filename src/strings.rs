//! User-facing strings, in one place.

/// Printed after the agent ended on its own.
pub fn agent_exited(code: u32) -> String {
    format!("agent exited with code {code}")
}

/// Printed after the agent was ended by a signal.
pub fn agent_killed(signal: &str) -> String {
    format!("agent was killed by {signal}")
}

/// Title of the left pane.
pub const PANE_PROJECTS: &str = "Projects";

/// Title of the right pane.
pub const PANE_CHANGES: &str = "Changes";

/// Title of the center pane.
pub fn agent_pane_title(agent: &str, project: &str) -> String {
    format!("{agent} · {project}")
}

/// Left side of the status bar: the keys that work.
pub const STATUS_KEYS: &str = " Ctrl+Q then q quit";

/// Right side of the status bar.
pub fn status_project(project: &str) -> String {
    format!("term · {project} ")
}
