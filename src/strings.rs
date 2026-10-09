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

/// What the zoom key does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    /// Nothing: no side panes to hide and none hidden by the zoom.
    None,
    /// Hide the side panes.
    In,
    /// Bring the side panes back.
    Out,
}

/// The prefix hints that apply: panes, zoom, quit.
fn prefix_hints(side_panes: bool, zoom: Zoom) -> Vec<&'static str> {
    let mut hints = Vec::with_capacity(3);
    if side_panes {
        hints.push("1/2/3 pane");
    }
    match zoom {
        Zoom::In => hints.push("z zoom"),
        Zoom::Out => hints.push("z unzoom"),
        Zoom::None => {}
    }
    hints.push("q quit");
    hints
}

/// Left side of the status bar with the agent focused.
pub fn status_agent(agent: &str, side_panes: bool, zoom: Zoom) -> String {
    let hints = prefix_hints(side_panes, zoom).join(" · ");
    format!(" focus: {agent}   Ctrl+Q then {hints}")
}

/// Left side of the status bar with a side pane focused.
pub fn status_side(pane: &str) -> String {
    format!(" focus: {pane}   Esc back · Tab next · Ctrl+Q then 1/2/3 pane · z zoom · q quit")
}

/// Left side of the status bar while the prefix waits for its key. The
/// literal `Ctrl+Q` only reaches the agent, so it is offered only then.
pub fn status_prefix_pending(side_panes: bool, zoom: Zoom, agent_focused: bool) -> String {
    let mut hints = prefix_hints(side_panes, zoom);
    if agent_focused {
        hints.push("Ctrl+Q literal");
    }
    hints.push("Esc cancel");
    format!(" Ctrl+Q …   {}", hints.join(" · "))
}

/// Right side of the status bar.
pub fn status_project(project: &str) -> String {
    format!("term · {project} ")
}
