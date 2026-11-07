//! Coding agents the center pane can run. The core knows only a command
//! on a pty; everything specific to an agent lives behind [`Agent`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::Size;
use crate::instance::Instance;
use crate::pty::Spec;

pub mod claude;
pub mod subject;

/// A run of the agent outside any session: it reads its prompt on its
/// stdin, prints its answer and ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The program, looked up on the `PATH` when it is a bare name.
    pub program: String,
    pub args: Vec<String>,
    /// Variables set over the inherited environment.
    pub env: Vec<(String, String)>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<String>,
}

/// Variables set over the inherited environment and removed from it so
/// that the agent, or a program that starts it, runs as an account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountEnv {
    /// Variables set over the inherited environment.
    pub env: Vec<(String, String)>,
    /// Variables removed from the inherited environment.
    pub env_remove: Vec<String>,
}

/// Where and how a session runs.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// The directory the agent runs in.
    pub cwd: &'a Path,
    /// The repository root around `cwd`, or `cwd` outside a repository.
    pub root: &'a Path,
    /// The user's extra arguments for the agent.
    pub args: &'a [String],
    /// Start a new conversation even when the agent could continue one.
    pub new_conversation: bool,
    /// The directory of the account the agent runs as, which holds its
    /// login and its data; none for the agent's own.
    pub account: Option<&'a Path>,
}

/// A directory the agent ran in lately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recent {
    /// The directory, absolute.
    pub dir: PathBuf,
    /// When the agent last started there, in milliseconds since the Unix
    /// epoch, when it says.
    pub started: Option<i64>,
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
    /// What the agent is doing, from its last event; the app puts the
    /// state its program status report gives in its place, when it gives
    /// one (see [`Agent::reported`]).
    pub state: Option<State>,
    /// The tool the agent is running while it works, as it names it, with
    /// the file the tool works on.
    pub activity: Option<String>,
    /// The agent's id for its conversation, which its events carry.
    pub session: Option<String>,
    /// The directories the agent was given to work in besides its own,
    /// absolute, in its order.
    pub added_dirs: Vec<PathBuf>,
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

    /// Readies `instance` for `launch`: whatever the agent's shims will
    /// need there. Runs before every launch. Nothing, unless the agent
    /// says otherwise.
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

    /// What the agent is doing as `report` says: the last program status
    /// report it sent its terminal, as the emulator kept it. A state from
    /// it takes the place of the one the events give. None when the
    /// report says nothing the agent's adapter knows, and unless the
    /// agent says otherwise.
    fn reported(&self, report: &[u8]) -> Option<State> {
        let _ = report;
        None
    }

    /// The files the agent wrote, absolute, in the order `lines` name
    /// them: complete lines of the hook events file, each read once, of
    /// the conversation `session` when the status names one. None, unless
    /// the agent says otherwise.
    fn written(&self, lines: &[u8], session: Option<&str>) -> Vec<PathBuf> {
        let _ = (lines, session);
        Vec::new()
    }

    /// The directories the agent ran in lately as the account whose
    /// directory is `account`, none for its own, that still exist, the
    /// latest first. None, unless the agent says otherwise.
    fn recent_projects(&self, account: Option<&Path>) -> Vec<Recent> {
        let _ = account;
        Vec::new()
    }

    /// The variables set and removed so that the agent, or a program that
    /// starts it, runs as the account whose directory is `account`, none
    /// for the agent's own. None, unless the agent says otherwise.
    fn account_env(&self, account: Option<&Path>) -> AccountEnv {
        let _ = account;
        AccountEnv::default()
    }

    /// The run that writes the subject of a commit of what is staged, as
    /// the account whose directory is `account`, none for the agent's
    /// own, which [`subject::suggest`] gives its prompt and reads. None,
    /// unless the agent says otherwise.
    fn subject(&self, account: Option<&Path>) -> Option<Ask> {
        let _ = account;
        None
    }

    /// The directories that hold the agent's own account, which no other
    /// account may name. None, unless the agent says otherwise.
    fn own_dirs(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// The agent used when none is selected.
pub fn default_agent() -> Box<dyn Agent> {
    Box::new(claude::Claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent that says only what it must.
    struct Bare;

    impl Agent for Bare {
        fn name(&self) -> &str {
            "bare"
        }

        fn spec(&self, launch: &Launch<'_>, size: Size, _: Option<&Instance>) -> Spec {
            Spec {
                program: "true".into(),
                args: vec![],
                cwd: launch.cwd.to_path_buf(),
                env: vec![],
                env_default: vec![],
                env_remove: vec![],
                size,
            }
        }
    }

    #[test]
    fn an_agent_lists_no_recent_projects_unless_it_says() {
        assert!(Bare.recent_projects(None).is_empty());
        assert!(Bare.recent_projects(Some(Path::new("/a"))).is_empty());
    }

    #[test]
    fn an_agent_changes_no_variable_for_an_account_unless_it_says() {
        assert_eq!(Bare.account_env(None), AccountEnv::default());
        assert_eq!(
            Bare.account_env(Some(Path::new("/a"))),
            AccountEnv::default()
        );
    }

    #[test]
    fn an_agent_writes_no_subject_unless_it_says() {
        assert_eq!(Bare.subject(None), None);
        assert_eq!(Bare.subject(Some(Path::new("/a"))), None);
    }

    #[test]
    fn an_agent_has_no_directories_of_its_own_unless_it_says() {
        assert!(Bare.own_dirs().is_empty());
    }

    #[test]
    fn an_agent_reports_no_state_unless_it_says() {
        assert_eq!(Bare.reported(b"state=working:app=claude-code"), None);
    }

    #[test]
    fn an_agent_writes_no_file_unless_it_says() {
        let line = br#"{"hook":{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"/p/a"}}}"#;
        assert!(Bare.written(line, None).is_empty());
    }
}
