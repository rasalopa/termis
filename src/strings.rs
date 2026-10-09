//! User-facing strings, in one place.

/// Printed after the agent ended on its own.
pub fn agent_exited(code: u32) -> String {
    format!("agent exited with code {code}")
}

/// Printed after the agent was ended by a signal.
pub fn agent_killed(signal: &str) -> String {
    format!("agent was killed by {signal}")
}

/// Printed when `term` is run without a mode that exists yet.
pub fn nothing_to_run(version: &str) -> String {
    format!("term {version}: only --spike runs yet, see ROADMAP.md")
}
