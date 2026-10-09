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

/// Shown in the right pane when the directory is not inside a repository.
pub const NOT_A_REPOSITORY: &str = "not a git repository";

/// Title of the center pane.
pub fn agent_pane_title(agent: &str, project: &str) -> String {
    format!("{agent} · {project}")
}

/// Status bar tag while keys go to term's commands. While they go to the
/// agent, its name takes the place.
pub const MODE_TERM: &str = "TERM";

/// The status bar tag for a mode, padded into a block.
pub fn mode_tag(mode: &str) -> String {
    format!(" {mode} ")
}

/// Status bar hints while keys go to the agent.
pub fn agent_hints(ended: bool) -> String {
    if ended {
        "r relaunch · q quit · Ctrl+Q term mode".to_owned()
    } else {
        "Ctrl+Q term mode".to_owned()
    }
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

/// Status bar hints in term mode: the commands that apply.
pub fn term_hints(side_panes: bool, zoom: Zoom, ended: bool) -> String {
    let mut hints = Vec::with_capacity(5);
    if side_panes {
        hints.push("1/2/3 pane");
    }
    match zoom {
        Zoom::In => hints.push("z zoom"),
        Zoom::Out => hints.push("z unzoom"),
        Zoom::None => {}
    }
    if ended {
        hints.push("r relaunch");
    }
    hints.push("q quit");
    hints.push("Esc back");
    hints.join(" · ")
}

/// Right side of the status bar.
pub fn status_project(project: &str) -> String {
    format!("term · {project} ")
}

/// The line drawn over the agent's screen after it ended.
pub fn agent_ended_line(agent: &str, code: u32, signal: Option<&str>) -> String {
    match signal {
        Some(signal) => format!(" {agent} was killed by {signal} · r relaunch · q quit "),
        None => format!(" {agent} exited (code {code}) · r relaunch · q quit "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_line_names_the_signal_or_the_code() {
        assert_eq!(
            agent_ended_line("claude", 0, Some("SIGKILL")),
            " claude was killed by SIGKILL · r relaunch · q quit "
        );
        assert_eq!(
            agent_ended_line("claude", 3, None),
            " claude exited (code 3) · r relaunch · q quit "
        );
    }

    #[test]
    fn hints_follow_the_panes_the_zoom_and_the_agent() {
        assert_eq!(
            term_hints(true, Zoom::In, false),
            "1/2/3 pane · z zoom · q quit · Esc back"
        );
        assert_eq!(
            term_hints(false, Zoom::Out, false),
            "z unzoom · q quit · Esc back"
        );
        assert_eq!(
            term_hints(false, Zoom::None, true),
            "r relaunch · q quit · Esc back"
        );
        assert_eq!(agent_hints(false), "Ctrl+Q term mode");
        assert_eq!(agent_hints(true), "r relaunch · q quit · Ctrl+Q term mode");
        assert_eq!(mode_tag(MODE_TERM), " TERM ");
    }
}
