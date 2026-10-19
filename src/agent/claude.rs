//! Claude Code as an agent.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use super::{Agent, Launch, Limit, Limits, State, Status};
use crate::Size;
use crate::instance::Instance;
use crate::pty::Spec;
use crate::telemetry;

/// Variables the child must not inherit: they would make Claude Code
/// believe it runs inside the host terminal, a multiplexer or another
/// Claude session. What it learns about its terminal comes from `TERM`,
/// `COLORTERM` and the emulator's query replies.
const REMOVED: [&str; 15] = [
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
];

/// The variable that moves Claude's user configuration away from
/// `~/.claude`.
const CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";

/// Claude's global configuration file, which lists the projects it ran
/// in among the account's data.
const GLOBAL_CONFIG: &str = ".claude.json";

/// The global configuration file's old name, in Claude's configuration
/// directory, which Claude still reads instead when it is not empty.
const LEGACY_CONFIG: &str = ".config.json";

/// Claude's flag for settings on the command line.
const SETTINGS_FLAG: &str = "--settings";

/// Claude's flag that continues the directory's last conversation.
const CONTINUE_FLAG: &str = "--continue";

/// Claude's long options that choose the conversation, or ask for none
/// to continue.
const CHOOSING: [&str; 7] = [
    "continue",
    "resume",
    "session-id",
    "from-pr",
    "teleport",
    "cloud",
    "print",
];

/// The short forms among them.
const CHOOSING_SHORT: [char; 3] = ['c', 'r', 'p'];

/// The longest folder name Claude gives a directory's conversations
/// whole.
const KEY_MAX: usize = 200;

/// Claude Code, started as `claude` from the `PATH`.
pub struct Claude;

impl Agent for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    /// Keeps the status line command Claude would have run for `launch`
    /// in the instance, for the status line shim to chain; removes a kept
    /// one when there is none now.
    fn prepare(&self, launch: &Launch<'_>, instance: &Instance) -> Result<()> {
        let config = config_dir(std::env::var_os(CONFIG_DIR_VAR), std::env::home_dir());
        keep_previous(instance, previous_status_line(launch, config.as_deref()))
    }

    /// Reads Claude's status line input for the model and its effort, the
    /// context in use, the cost, the plan's usage limits, the lines and how
    /// long the session has run, and its hook events for the state and the
    /// tool it runs.
    fn status(&self, status: &[u8], events: &[u8]) -> Status {
        let input = serde_json::from_slice::<Value>(status)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(Value::Null);
        let cost = &input["cost"];
        let lines = cost["total_lines_added"]
            .as_u64()
            .zip(cost["total_lines_removed"].as_u64());
        let (state, activity) = state(events, input["session_id"].as_str())
            .map_or((None, None), |(state, activity)| (Some(state), activity));
        Status {
            model: input["model"]["display_name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            effort: input["effort"]["level"]
                .as_str()
                .filter(|level| !level.is_empty())
                .map(str::to_owned),
            context: input["context_window"]["used_percentage"]
                .as_f64()
                .filter(|share| share.is_finite())
                .map(|share| share.clamp(0.0, 100.0).round() as u8),
            cost: cost["total_cost_usd"]
                .as_f64()
                .filter(|usd| usd.is_finite() && *usd >= 0.0),
            limits: Limits {
                five_hours: limit(&input["rate_limits"]["five_hour"]),
                week: limit(&input["rate_limits"]["seven_day"]),
            },
            lines,
            duration: cost["total_duration_ms"]
                .as_u64()
                .map(std::time::Duration::from_millis),
            state,
            activity,
        }
    }

    /// `claude` with `--continue` when the launch continues a
    /// conversation, the session's settings merged into the user's own
    /// `--settings`, if any, then the user's other arguments, and the
    /// instance directory in its environment. Without an instance the
    /// user's arguments go through as they are.
    fn spec(&self, launch: &Launch<'_>, size: Size, instance: Option<&Instance>) -> Spec {
        let mut env = vec![
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
            // The app owns the pane title.
            (
                "CLAUDE_CODE_DISABLE_TERMINAL_TITLE".to_owned(),
                "1".to_owned(),
            ),
        ];
        if let Some(instance) = instance {
            env.push((
                telemetry::INSTANCE_DIR_VAR.to_owned(),
                instance.dir().to_string_lossy().into_owned(),
            ));
        }
        let config = config_dir(std::env::var_os(CONFIG_DIR_VAR), std::env::home_dir());
        let args = arguments(launch, instance, config.as_deref());
        Spec {
            program: "claude".to_owned(),
            args,
            cwd: launch.cwd.to_path_buf(),
            env,
            env_default: vec![("LC_CTYPE".to_owned(), "UTF-8".to_owned())],
            env_remove: REMOVED.iter().map(|&name| name.to_owned()).collect(),
            size,
        }
    }

    /// The projects Claude's global configuration lists that are still
    /// directories, the last started first.
    fn recent_projects(&self) -> Vec<PathBuf> {
        let file = projects_file(std::env::var_os(CONFIG_DIR_VAR), std::env::home_dir());
        recent_in(file.as_deref())
    }
}

/// Claude's arguments for `launch`, with `config` its configuration
/// directory: `--continue` first when the launch continues a
/// conversation, before any `--` of the user's, then with an instance one
/// `--settings` and the user's other arguments, else theirs as they are.
fn arguments(
    launch: &Launch<'_>,
    instance: Option<&Instance>,
    config: Option<&Path>,
) -> Vec<String> {
    let mut args = Vec::with_capacity(launch.args.len() + 3);
    if continues(launch, config) {
        args.push(CONTINUE_FLAG.to_owned());
    }
    match instance {
        Some(instance) => args.extend(with_settings(launch, instance.exe())),
        None => args.extend_from_slice(launch.args),
    }
    args
}

/// Whether `launch` continues the last conversation Claude saved for its
/// directory under `config`: it is not asked for a new one, the user's
/// arguments choose none, and there is one.
fn continues(launch: &Launch<'_>, config: Option<&Path>) -> bool {
    !launch.new_conversation
        && !chooses_conversation(launch.args)
        && config.is_some_and(|config| has_conversation(config, launch.cwd))
}

/// Whether `args`, up to a `--`, choose the conversation or ask for none
/// to continue: `-c`, `-r`, `-p`, their long forms, `--session-id`,
/// `--from-pr`, `--teleport` or `--cloud`, a value after `=` or more
/// letters after a short one included.
fn chooses_conversation(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| match arg.strip_prefix("--") {
            Some(long) => {
                let name = long.split_once('=').map_or(long, |(name, _)| name);
                CHOOSING.contains(&name)
            }
            None => arg
                .strip_prefix('-')
                .and_then(|short| short.chars().next())
                .is_some_and(|letter| CHOOSING_SHORT.contains(&letter)),
        })
}

/// Whether Claude saved a conversation for the directory `cwd` under
/// `config`: a `*.jsonl` file right in the directory's folder under
/// `projects`. Only names are looked at, never what the files hold.
fn has_conversation(config: &Path, cwd: &Path) -> bool {
    let Ok(real) = cwd.canonicalize() else {
        return false;
    };
    let Some(path) = real.to_str() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(config.join("projects").join(project_key(path))) else {
        return false;
    };
    entries.flatten().any(|entry| {
        Path::new(&entry.file_name())
            .extension()
            .is_some_and(|extension| extension == "jsonl")
            && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

/// The name of the folder Claude 2.1.301 keeps the conversations of the
/// directory at the canonical `path` in: each UTF-16 unit outside
/// `[A-Za-z0-9]` made `-`, and past `KEY_MAX` units the first `KEY_MAX`
/// and a hash of the path. Claude puts the path in NFC first, which this
/// does not: a path with decomposed accents finds no folder, and its
/// conversation starts anew.
fn project_key(path: &str) -> String {
    let mut key = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else {
            key.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if key.len() > KEY_MAX {
        key.truncate(KEY_MAX);
        key.push('-');
        key.push_str(&base36(path_hash(path)));
    }
    key
}

/// Claude's hash of a path for a folder name: Java's string hash over the
/// UTF-16 units, without its sign.
fn path_hash(path: &str) -> u32 {
    path.encode_utf16()
        .fold(0i32, |hash, unit| {
            hash.wrapping_shl(5)
                .wrapping_sub(hash)
                .wrapping_add(i32::from(unit))
        })
        .unsigned_abs()
}

/// `n` in base 36, with lowercase letters.
fn base36(mut n: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut digits = Vec::new();
    loop {
        digits.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits
        .iter()
        .rev()
        .map(|&digit| char::from(digit))
        .collect()
}

/// The user's arguments with one `--settings` first: the session's, merged
/// into the user's own when they gave one Claude can read. One the user
/// gave that cannot be read stays after the session's, so Claude reports
/// it as it would without termis.
fn with_settings(launch: &Launch<'_>, exe: &Path) -> Vec<String> {
    let (given, rest) = split_settings(launch.args);
    let ours = settings(exe);
    let (merged, unread) = match given {
        Some(value) => match load_settings(&value, launch.cwd) {
            Some(mut theirs) => {
                merge(&mut theirs, ours);
                (theirs, None)
            }
            None => (ours, Some(value)),
        },
        None => (ours, None),
    };
    let mut args = vec![SETTINGS_FLAG.to_owned(), merged.to_string()];
    if let Some(value) = unread {
        args.push(SETTINGS_FLAG.to_owned());
        args.push(value);
    }
    args.extend(rest);
    args
}

/// The settings a session gets on the command line, which apply to it
/// alone: a status line and hooks that run the shims of `exe`. The status
/// line takes the place of the user's, which the shim chains; the hooks
/// add to theirs and run in the background.
fn settings(exe: &Path) -> Value {
    let shim = |subcommand: &str| format!("{} {subcommand}", quote(exe));
    let hook =
        json!([{ "hooks": [{ "type": "command", "command": shim("hook"), "async": true }] }]);
    json!({
        "statusLine": { "type": "command", "command": shim("statusline") },
        "hooks": {
            "UserPromptSubmit": hook,
            "PreToolUse": hook,
            "PostToolUse": hook,
            "PostToolUseFailure": hook,
            "Stop": hook,
            "StopFailure": hook,
            "Notification": hook,
        },
    })
}

/// One window of `rate_limits`, which Claude sends for an account on a
/// Claude.ai plan once a response came back: the share used, rounded down
/// so that 100 means used up, and when the window resets.
fn limit(window: &Value) -> Option<Limit> {
    let used = window["used_percentage"]
        .as_f64()
        .filter(|share| share.is_finite())?;
    let resets_at = window["resets_at"].as_i64()?;
    Some(Limit {
        used: used.clamp(0.0, 100.0).floor() as u8,
        resets_at,
    })
}

/// What a tool event names as running: the tool, with its MCP server's
/// prefix left out, and the name of the file it works on, if any.
fn activity(hook: &Value) -> Option<String> {
    let tool = hook["tool_name"].as_str().filter(|tool| !tool.is_empty())?;
    let tool = tool
        .strip_prefix("mcp__")
        .and_then(|rest| rest.rsplit_once("__"))
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
        .unwrap_or(tool);
    let file = hook["tool_input"]["file_path"]
        .as_str()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy());
    Some(match file {
        Some(file) => format!("{tool} {file}"),
        None => tool.to_owned(),
    })
}

/// How many lines before a tool's start [`state`] looks for its end: the
/// hooks run in parallel, so a quick tool's end can be written first.
const END_LOOKBACK: usize = 32;

/// The state the session's events leave it in, with the tool the main
/// agent is running, from the last event that sets one: a prompt, or a
/// tool of the main agent about to run, that ran or failed, means working,
/// and a tool about to run whose end is not written is the activity; a
/// stop, a stop by an API error or the idle notice, waiting; a permission
/// or a question, attention; an answered question, working again. Tool
/// events of subagents set nothing, so a dialog one of them left open is
/// not hidden by another's work. Lines of another session than `session`,
/// when it is known, are passed over, and so are a last line not yet
/// ended and any line that does not parse. A tool waiting for permission
/// shows as running until Claude's permission notice, a few seconds
/// later. An interrupt between tools (Esc) or a denied permission has no
/// event: the state it leaves, and the tool shown, hold until the next
/// one, the idle notice at the latest.
fn state(events: &[u8], session: Option<&str>) -> Option<(State, Option<String>)> {
    let ended = events
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(&events[..0], |end| &events[..end]);
    let parse = |line: &[u8]| {
        let event = serde_json::from_slice::<Value>(line).ok()?;
        let hook = &event["hook"];
        if let (Some(want), Some(got)) = (session, hook["session_id"].as_str())
            && want != got
        {
            return None;
        }
        Some(event)
    };
    // Every event either sets a state outright or sets none, so the last
    // one that sets one decides: read from the end.
    let lines: Vec<&[u8]> = ended.rsplit(|&byte| byte == b'\n').collect();
    lines.iter().enumerate().find_map(|(at, line)| {
        let event = parse(line)?;
        let hook = &event["hook"];
        let main_agent = hook.get("agent_id").is_none();
        let state = match hook["hook_event_name"].as_str()? {
            "PreToolUse" if main_agent => {
                let id = hook["tool_use_id"].as_str();
                let over = id.is_some_and(|id| {
                    lines[at + 1..]
                        .iter()
                        .take(END_LOOKBACK)
                        .filter_map(|line| parse(line))
                        .any(|earlier| {
                            let earlier = &earlier["hook"];
                            matches!(
                                earlier["hook_event_name"].as_str(),
                                Some("PostToolUse" | "PostToolUseFailure")
                            ) && earlier["tool_use_id"].as_str() == Some(id)
                        })
                });
                return Some((State::Working, if over { None } else { activity(hook) }));
            }
            "UserPromptSubmit" => State::Working,
            "PostToolUse" | "PostToolUseFailure" if main_agent => State::Working,
            "Stop" | "StopFailure" => State::Waiting,
            "Notification" => match hook["notification_type"].as_str()? {
                "idle_prompt" => State::Waiting,
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input" => State::Attention,
                "elicitation_response" => State::Working,
                _ => return None,
            },
            _ => return None,
        };
        Some((state, None))
    })
}

/// Merges `overlay` into `base` as Claude merges its settings sources:
/// objects key by key, arrays one after the other, anything else replaced.
fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(base), Value::Array(overlay)) => base.extend(overlay),
        (base, overlay) => *base = overlay,
    }
}

/// Takes the user's `--settings` out of `args`: the value of the last one,
/// which is the one Claude would use, and the other arguments in order.
/// Arguments after `--` are left as they are, and so is a `--settings`
/// with no value.
fn split_settings(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut value = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            rest.push(arg.clone());
            rest.extend(args.by_ref().cloned());
            break;
        }
        if let Some(inline) = arg.strip_prefix("--settings=") {
            value = Some(inline.to_owned());
        } else if arg == SETTINGS_FLAG {
            match args.next() {
                Some(next) => value = Some(next.clone()),
                None => rest.push(arg.clone()),
            }
        } else {
            rest.push(arg.clone());
        }
    }
    (value, rest)
}

/// The settings a `--settings` value names, as Claude reads it: JSON when
/// it looks like an object, else a file relative to `cwd`. None when it
/// cannot be read or is not an object.
fn load_settings(value: &str, cwd: &Path) -> Option<Value> {
    let trimmed = value.trim();
    let text = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        trimmed.to_owned()
    } else {
        fs::read_to_string(cwd.join(value)).ok()?
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// `path` as one word for `sh`: single-quoted, with each single quote of
/// its own closed, escaped and reopened.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Claude's user configuration directory: `CLAUDE_CONFIG_DIR` when it is
/// set and not empty, else `.claude` in the home directory.
fn config_dir(variable: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    match variable {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| home.join(".claude")),
    }
}

/// Claude's global configuration file: `.claude.json` in
/// `CLAUDE_CONFIG_DIR` when it is set and not empty, else in the home
/// directory itself, not in `.claude`.
fn global_config(variable: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    match variable {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir).join(GLOBAL_CONFIG)),
        _ => home.map(|home| home.join(GLOBAL_CONFIG)),
    }
}

/// The file Claude 2.1.301 lists its projects in: [`LEGACY_CONFIG`] in
/// its configuration directory when that is a file that is not empty,
/// else [`global_config`]. An empty legacy file, which Claude may still
/// read, lists nothing either way. A custom OAuth URL, which gives
/// Claude's file another name, is not followed.
fn projects_file(variable: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    config_dir(variable.clone(), home.clone())
        .map(|dir| dir.join(LEGACY_CONFIG))
        .filter(|file| fs::metadata(file).is_ok_and(|found| found.is_file() && found.len() > 0))
        .or_else(|| global_config(variable, home))
}

/// The projects the global configuration `file` lists that are still
/// directories, in the order of [`recent_from`]; none when there is no
/// file or it cannot be read.
fn recent_in(file: Option<&Path>) -> Vec<PathBuf> {
    file.and_then(|file| fs::read(file).ok())
        .map_or_else(Vec::new, |bytes| recent_from(&bytes, Path::is_dir))
}

/// The keys of `projects` in Claude's global configuration `bytes` that
/// are absolute paths of directories by `is_dir`: the highest
/// `lastStartTime` first, then those without one by path. Only those keys
/// and their `lastStartTime` are looked at, and nothing is kept of the
/// rest. None when the file does not parse or `projects` is not an
/// object.
fn recent_from(bytes: &[u8], is_dir: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(config) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let Some(projects) = config.get("projects").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut found: Vec<(Option<i64>, PathBuf)> = projects
        .iter()
        .map(|(key, project)| {
            let started = project.get("lastStartTime").and_then(Value::as_i64);
            (started, PathBuf::from(key))
        })
        .filter(|(_, path)| path.is_absolute() && is_dir(path))
        .collect();
    // No time sorts below every time, so last.
    found.sort_by(|(a_started, a_path), (b_started, b_path)| {
        b_started.cmp(a_started).then_with(|| a_path.cmp(b_path))
    });
    found.into_iter().map(|(_, path)| path).collect()
}

/// The settings files Claude 2.1.296 reads for `launch`, lowest
/// precedence first: the user's in `config`, the project's in the working
/// directory, its local one there, then the local one at the repository
/// root, which wins when the two differ. Managed settings, which win over
/// all, are not read.
fn settings_files(config: Option<&Path>, launch: &Launch<'_>) -> Vec<PathBuf> {
    let mut files = Vec::with_capacity(4);
    if let Some(config) = config {
        files.push(config.join("settings.json"));
    }
    files.push(launch.cwd.join(".claude/settings.json"));
    files.push(launch.cwd.join(".claude/settings.local.json"));
    if launch.root != launch.cwd {
        files.push(launch.root.join(".claude/settings.local.json"));
    }
    files
}

/// The status line command Claude would run for `launch`: the
/// `statusLine` objects of every settings source merged, lowest first,
/// with the user's own `--settings` last; its command when its type is
/// `command`. Sources that are missing or not JSON are passed over.
fn previous_status_line(launch: &Launch<'_>, config: Option<&Path>) -> Option<String> {
    let files = settings_files(config, launch);
    let from_files = files.iter().filter_map(|file| {
        fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    });
    let from_args = split_settings(launch.args)
        .0
        .and_then(|value| load_settings(&value, launch.cwd));
    let mut line = Value::Object(Map::new());
    for source in from_files.chain(from_args) {
        if let Some(found @ Value::Object(_)) = source.get("statusLine") {
            merge(&mut line, found.clone());
        }
    }
    if line.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    line.get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned)
}

/// Writes `previous` to the instance for the status line shim, or removes
/// the one kept before when there is none.
fn keep_previous(instance: &Instance, previous: Option<String>) -> Result<()> {
    let path = instance.dir().join(telemetry::PREVIOUS_STATUS_LINE_FILE);
    match previous {
        Some(command) => telemetry::write_atomically(&path, command.as_bytes()),
        None => match fs::remove_file(&path) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
    .with_context(|| format!("keep {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::TempDir;

    fn launch<'a>(cwd: &'a Path, root: &'a Path, args: &'a [String]) -> Launch<'a> {
        Launch {
            cwd,
            root,
            args,
            new_conversation: false,
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    const SIZE: Size = Size {
        rows: 40,
        cols: 120,
    };

    #[test]
    fn builds_the_launch_spec() {
        let dir = TempDir::new().expect("temp dir");
        let instance = Instance::at(
            dir.path().join("i"),
            PathBuf::from("/usr/local/bin/ter mis"),
        );
        let args = strings(&["-c", "--model", "opus"]);
        let cwd = Path::new("/tmp/repo");
        let spec = Claude.spec(&launch(cwd, cwd, &args), SIZE, Some(&instance));
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args[0], "--settings");
        assert_eq!(spec.args[2..], args[..]);
        assert_eq!(spec.cwd, cwd);
        assert_eq!(spec.size, SIZE);
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let instance_dir = dir.path().join("i");
        assert_eq!(
            env,
            [
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1"),
                ("TERMIS_DIR", instance_dir.to_str().expect("utf-8")),
            ]
        );
        assert_eq!(
            spec.env_default,
            [("LC_CTYPE".to_owned(), "UTF-8".to_owned())]
        );
        for name in REMOVED {
            assert!(
                spec.env_remove.iter().any(|n| n == name),
                "{name} is removed"
            );
        }
        // Without an instance: the user's arguments as given, no instance.
        let plain = Claude.spec(&launch(cwd, cwd, &args), SIZE, None);
        assert_eq!(plain.args, args);
        assert!(plain.env.iter().all(|(name, _)| name != "TERMIS_DIR"));
    }

    #[test]
    fn the_settings_run_the_shims_for_the_session() {
        let settings = settings(Path::new("/usr/local/bin/ter mis"));
        assert_eq!(
            settings["statusLine"],
            json!({ "type": "command", "command": "'/usr/local/bin/ter mis' statusline" })
        );
        for event in [
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "Stop",
            "StopFailure",
            "Notification",
        ] {
            let hooks = &settings["hooks"][event];
            assert_eq!(hooks.as_array().map(Vec::len), Some(1), "{event}");
            let hook = &hooks[0]["hooks"][0];
            assert_eq!(hook["type"], "command");
            assert_eq!(hook["command"], "'/usr/local/bin/ter mis' hook");
            assert_eq!(hook["async"], true);
        }
        assert_eq!(settings["hooks"].as_object().map(Map::len), Some(7));
    }

    #[test]
    fn the_shim_commands_survive_the_shell() {
        // An executable at a path with a space and a quote that prints its
        // argument: the command line must reach it whole.
        let dir = TempDir::new().expect("temp dir");
        let exe = dir.path().join("it's a shim");
        fs::write(&exe, "#!/bin/sh\nprintf '%s' \"$1\"\n").expect("write");
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).expect("chmod");
        let settings = settings(&exe);
        for (command, expected) in [
            (&settings["statusLine"]["command"], "statusline"),
            (&settings["hooks"]["Stop"][0]["hooks"][0]["command"], "hook"),
        ] {
            let output = Command::new("/bin/sh")
                .arg("-c")
                .arg(command.as_str().expect("a string"))
                .output()
                .expect("runs");
            assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
        }
        assert_eq!(quote(Path::new("/it's here/t")), "'/it'\\''s here/t'");
    }

    #[test]
    fn a_settings_flag_of_the_user_is_merged_into_one() {
        let (value, rest) =
            split_settings(&strings(&["-c", "--settings", "a.json", "--model", "x"]));
        assert_eq!(value.as_deref(), Some("a.json"));
        assert_eq!(rest, strings(&["-c", "--model", "x"]));
        let (value, rest) = split_settings(&strings(&["--settings=one", "--settings", "two"]));
        assert_eq!(
            value.as_deref(),
            Some("two"),
            "the last one, as Claude takes it"
        );
        assert!(rest.is_empty());
        let (value, rest) = split_settings(&strings(&["-p", "--", "--settings", "x"]));
        assert_eq!(value, None, "after -- it is the agent's argument");
        assert_eq!(rest, strings(&["-p", "--", "--settings", "x"]));
        let (value, rest) = split_settings(&strings(&["--settings"]));
        assert_eq!((value, rest), (None, strings(&["--settings"])));

        let dir = TempDir::new().expect("temp dir");
        fs::write(
            dir.path().join("team.json"),
            r#"{"model": "opus", "statusLine": {"type": "command", "command": "mine", "padding": 2},
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "their-stop"}]}]}}"#,
        )
        .expect("write");
        let args = strings(&["--settings", "team.json", "-c"]);
        let exe = Path::new("/bin/termis");
        let built = with_settings(&launch(dir.path(), dir.path(), &args), exe);
        assert_eq!(built.len(), 3, "one --settings, then the rest: {built:?}");
        assert_eq!((built[0].as_str(), built[2].as_str()), ("--settings", "-c"));
        let merged: Value = serde_json::from_str(&built[1]).expect("json");
        assert_eq!(merged["model"], "opus", "their keys stay");
        assert_eq!(merged["statusLine"]["command"], "'/bin/termis' statusline");
        assert_eq!(
            merged["statusLine"]["padding"], 2,
            "their status line options stay"
        );
        let stops = merged["hooks"]["Stop"].as_array().expect("array");
        assert_eq!(stops.len(), 2, "their hook and ours");
        assert_eq!(stops[0]["hooks"][0]["command"], "their-stop");
        assert_eq!(
            merged["hooks"]["Notification"].as_array().map(Vec::len),
            Some(1)
        );

        // Inline JSON works too; one that cannot be read stays after ours.
        let inline = strings(&["--settings={\"model\":\"haiku\"}"]);
        let built = with_settings(&launch(dir.path(), dir.path(), &inline), exe);
        let merged: Value = serde_json::from_str(&built[1]).expect("json");
        assert_eq!((merged["model"].as_str(), built.len()), (Some("haiku"), 2));
        let unread = strings(&["--settings", "missing.json"]);
        let built = with_settings(&launch(dir.path(), dir.path(), &unread), exe);
        assert_eq!(built[2..], strings(&["--settings", "missing.json"])[..]);
    }

    #[test]
    fn the_config_dir_follows_the_variable() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            config_dir(Some("/work/claude".into()), home.clone()),
            Some(PathBuf::from("/work/claude"))
        );
        assert_eq!(
            config_dir(Some("".into()), home.clone()),
            Some(PathBuf::from("/home/u/.claude"))
        );
        assert_eq!(
            config_dir(None, home),
            Some(PathBuf::from("/home/u/.claude"))
        );
        assert_eq!(config_dir(None, None), None);
    }

    #[test]
    fn the_global_config_follows_the_variable_out_of_the_config_dir() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            global_config(Some("/work/claude".into()), home.clone()),
            Some(PathBuf::from("/work/claude/.claude.json"))
        );
        assert_eq!(
            global_config(Some("".into()), home.clone()),
            Some(PathBuf::from("/home/u/.claude.json"))
        );
        assert_eq!(
            global_config(None, home),
            Some(PathBuf::from("/home/u/.claude.json"))
        );
        assert_eq!(global_config(None, None), None);
    }

    #[test]
    fn the_projects_file_is_the_legacy_one_while_it_is_not_empty() {
        let home = TempDir::new().expect("temp dir");
        let config = home.path().join(".claude");
        fs::create_dir(&config).expect("mkdir");
        let legacy = config.join(LEGACY_CONFIG);
        let file = || projects_file(None, Some(home.path().to_path_buf()));
        let current = Some(home.path().join(GLOBAL_CONFIG));
        assert_eq!(file(), current, "no legacy file");
        fs::write(&legacy, "").expect("write");
        assert_eq!(file(), current, "an empty one");
        fs::write(&legacy, "{}").expect("write");
        assert_eq!(file(), Some(legacy));

        // With the variable, both are in its directory.
        let moved = TempDir::new().expect("temp dir");
        let legacy = moved.path().join(LEGACY_CONFIG);
        fs::create_dir(&legacy).expect("mkdir");
        let file = || projects_file(Some(moved.path().into()), Some(home.path().to_path_buf()));
        assert_eq!(file(), Some(moved.path().join(GLOBAL_CONFIG)), "not a file");
        fs::remove_dir(&legacy).expect("rmdir");
        fs::write(&legacy, "{}").expect("write");
        assert_eq!(file(), Some(legacy));
        assert_eq!(projects_file(None, None), None);
    }

    #[test]
    fn recent_projects_are_existing_directories_last_started_first() {
        // A sample, with keys of the kind the real file has around them.
        let config = br#"{
            "numStartups": 12,
            "oauthAccount": { "emailAddress": "someone@example.com" },
            "projects": {
                "/work/old": { "lastStartTime": 1760000000000 },
                "/work/new": { "lastStartTime": 1760900000000, "allowedTools": [] },
                "/work/a-b": {},
                "/work/a/b": { "lastCost": 1.5 },
                "/work/gone": { "lastStartTime": 1761000000000 },
                "relative/dir": { "lastStartTime": 1762000000000 },
                "": { "lastStartTime": 1763000000000 }
            }
        }"#;
        let exists = |path: &Path| path != Path::new("/work/gone");
        // Those without a time by path, component by component, where
        // `a` comes before `a-b`.
        assert_eq!(
            recent_from(config, exists),
            ["/work/new", "/work/old", "/work/a/b", "/work/a-b"].map(PathBuf::from)
        );
        for unusable in [
            &b"{\"projects\": {"[..],
            b"",
            b"[]",
            br#"{"projects": []}"#,
            br#"{"projects": "/work/old"}"#,
            br#"{"numStartups": 1}"#,
        ] {
            assert!(
                recent_from(unusable, |_| true).is_empty(),
                "{}",
                String::from_utf8_lossy(unusable)
            );
        }
    }

    #[test]
    fn recent_projects_are_read_from_the_file_when_there_is_one() {
        let dir = TempDir::new().expect("temp dir");
        let project = dir.path().join("project");
        fs::create_dir(&project).expect("mkdir");
        let file = dir.path().join(GLOBAL_CONFIG);
        let mut projects = Map::new();
        for (path, started) in [(&project, 1), (&dir.path().join("gone"), 2)] {
            projects.insert(
                path.to_string_lossy().into_owned(),
                json!({ "lastStartTime": started }),
            );
        }
        fs::write(&file, json!({ "projects": projects }).to_string()).expect("write");
        assert_eq!(recent_in(Some(&file)), [project]);
        assert!(recent_in(Some(&dir.path().join("none.json"))).is_empty());
        assert!(recent_in(None).is_empty(), "no home");
    }

    #[test]
    fn the_folder_name_is_the_one_claude_gives() {
        // Expected values from Claude 2.1.301's own functions, run in node.
        assert_eq!(
            project_key("/Users/me/My Project.v2"),
            "-Users-me-My-Project-v2"
        );
        assert_eq!(project_key("/tmp/a\u{1F600}b"), "-tmp-a--b", "two units");
        assert_eq!(project_key("/tmp/caf\u{e9}"), "-tmp-caf-");
        let long = format!("/Users/me/{}project", "deep/".repeat(40));
        let key = project_key(&long);
        assert_eq!(key.len(), KEY_MAX + 7);
        assert!(key.starts_with("-Users-me-deep-deep-"), "{key}");
        assert!(key.ends_with("-deep-deep--ot7opk"), "{key}");
        assert_eq!(base36(path_hash("")), "0");
        assert_eq!(base36(path_hash("a")), "2p");
        // Its hash is the lowest i32, whose sign an i32 cannot drop.
        assert_eq!(base36(path_hash("polygenelubricants")), "zik0zk");
    }

    #[test]
    fn arguments_that_choose_a_conversation_keep_it() {
        let choosing: &[&[&str]] = &[
            &["-c"],
            &["--continue"],
            &["-r"],
            &["-r", "7adb"],
            &["-r7adb"],
            &["--resume=7adb"],
            &["--session-id", "7adb"],
            &["--session-id=7adb"],
            &["--from-pr", "12"],
            &["--teleport"],
            &["--cloud=fix the build"],
            &["-p", "hi"],
            &["--print"],
            &["--model", "opus", "-c"],
        ];
        for args in choosing {
            assert!(chooses_conversation(&strings(args)), "{args:?}");
        }
        let not_choosing: &[&[&str]] = &[
            &[],
            &["--model", "opus"],
            &["-d"],
            &["-"],
            &["--resumed"],
            &["--fork-session"],
            &["--", "-c"],
        ];
        for args in not_choosing {
            assert!(!chooses_conversation(&strings(args)), "{args:?}");
        }
    }

    #[test]
    fn a_launch_continues_the_directorys_last_conversation() {
        let dir = TempDir::new().expect("temp dir");
        let cwd = dir.path().join("my project");
        fs::create_dir(&cwd).expect("mkdir");
        let config = dir.path().join("config");
        let real = cwd.canonicalize().expect("canonical");
        let folder = config
            .join("projects")
            .join(project_key(real.to_str().expect("utf-8")));
        let none = Vec::new();
        let first = launch(&cwd, &cwd, &none);
        assert!(!continues(&first, Some(&config)), "no folder");
        fs::create_dir_all(folder.join("a.jsonl")).expect("mkdir");
        fs::write(folder.join("notes.txt"), "").expect("write");
        assert!(!continues(&first, Some(&config)), "no conversation file");
        fs::write(folder.join("7adb.jsonl"), "").expect("write");
        assert!(continues(&first, Some(&config)));
        assert!(!continues(&first, None), "no configuration directory");
        let new = Launch {
            new_conversation: true,
            ..first
        };
        assert!(!continues(&new, Some(&config)));
        let resume = strings(&["--resume", "7adb"]);
        assert!(!continues(&launch(&cwd, &cwd, &resume), Some(&config)));
        // Through a link, the conversations are the real directory's.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&cwd, &link).expect("symlink");
        assert!(continues(&launch(&link, &link, &none), Some(&config)));
        // The flag comes before the user's arguments, which may end the
        // options.
        let args = strings(&["--model", "opus", "--", "-c"]);
        let at = launch(&cwd, &cwd, &args);
        assert_eq!(
            arguments(&at, None, Some(&config)),
            strings(&["--continue", "--model", "opus", "--", "-c"])
        );
        let instance = Instance::at(dir.path().join("i"), PathBuf::from("/bin/termis"));
        let built = arguments(&at, Some(&instance), Some(&config));
        assert_eq!(built[..2], strings(&["--continue", "--settings"])[..]);
        assert_eq!(built[3..], args[..]);
    }

    #[test]
    fn the_status_line_is_merged_across_sources_as_claude_does() {
        let dir = TempDir::new().expect("temp dir");
        let config = dir.path().join("config");
        let root = dir.path().join("repo");
        let cwd = root.join("src");
        for path in [&config, &root.join(".claude"), &cwd.join(".claude")] {
            fs::create_dir_all(path).expect("mkdir");
        }
        let no_args: Vec<String> = Vec::new();
        let here = launch(&cwd, &root, &no_args);
        assert_eq!(
            settings_files(Some(&config), &here),
            [
                config.join("settings.json"),
                cwd.join(".claude/settings.json"),
                cwd.join(".claude/settings.local.json"),
                root.join(".claude/settings.local.json"),
            ]
        );
        assert_eq!(
            settings_files(None, &launch(&root, &root, &no_args)).len(),
            2
        );
        assert_eq!(
            previous_status_line(&here, Some(&config)),
            None,
            "no file yet"
        );

        let write = |path: PathBuf, text: &str| fs::write(path, text).expect("write");
        write(
            config.join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "user-line"}}"#,
        );
        assert_eq!(
            previous_status_line(&here, Some(&config)).as_deref(),
            Some("user-line")
        );
        write(cwd.join(".claude/settings.json"), "{ not json");
        write(
            cwd.join(".claude/settings.local.json"),
            r#"{"statusLine": {"padding": 1}}"#,
        );
        assert_eq!(
            previous_status_line(&here, Some(&config)).as_deref(),
            Some("user-line"),
            "a broken file is passed over and padding alone keeps the command"
        );
        write(
            root.join(".claude/settings.local.json"),
            r#"{"statusLine": {"type": "command", "command": "root-local"}}"#,
        );
        assert_eq!(
            previous_status_line(&here, Some(&config)).as_deref(),
            Some("root-local"),
            "the repository's local file wins"
        );
        let args =
            strings(&["--settings={\"statusLine\":{\"type\":\"command\",\"command\":\"flag\"}}"]);
        assert_eq!(
            previous_status_line(&launch(&cwd, &root, &args), Some(&config)).as_deref(),
            Some("flag"),
            "the user's own --settings wins over every file"
        );
        write(
            root.join(".claude/settings.local.json"),
            r#"{"statusLine": {"type": "static"}}"#,
        );
        assert_eq!(
            previous_status_line(&here, Some(&config)),
            None,
            "not a command"
        );
    }

    #[test]
    fn prepare_keeps_or_drops_the_previous_command() {
        let dir = TempDir::new().expect("temp dir");
        let instance = Instance::at(dir.path().join("i"), PathBuf::from("/x"));
        fs::create_dir(instance.dir()).expect("mkdir");
        let kept = instance.dir().join(telemetry::PREVIOUS_STATUS_LINE_FILE);
        keep_previous(&instance, Some("printf local".to_owned())).expect("keeps");
        assert_eq!(fs::read_to_string(&kept).expect("kept"), "printf local");
        keep_previous(&instance, None).expect("drops");
        assert!(
            !kept.exists(),
            "a session without one does not chain the old one"
        );
        keep_previous(&instance, None).expect("nothing to drop");
        let gone = Instance::at(dir.path().join("gone"), PathBuf::from("/x"));
        assert!(keep_previous(&gone, Some("x".to_owned())).is_err());
    }

    /// Abridged from what Claude Code 2.1.296 piped after one turn.
    const STATUS: &[u8] = br#"{"session_id":"7adb","model":{"id":"claude-opus-5-5","display_name":"Opus 5.5"},"effort":{"level":"xhigh"},
        "rate_limits":{"five_hour":{"used_percentage":9,"resets_at":1791870600},"seven_day":{"used_percentage":30.4,"resets_at":1792296000}},
        "context_window":{"total_input_tokens":39223,"context_window_size":1000000,"used_percentage":4,"remaining_percentage":96},
        "cost":{"total_cost_usd":0.1020596,"total_duration_ms":5066,"total_lines_added":12,"total_lines_removed":3}}"#;

    fn event(name: &str, session: &str, extra: &str) -> String {
        format!(
            "{{\"hook\":{{\"hook_event_name\":\"{name}\",\"session_id\":\"{session}\"{extra}}},\"received\":1}}\n"
        )
    }

    #[test]
    fn reads_the_status_line_input() {
        let status = Claude.status(STATUS, b"");
        assert_eq!(
            status,
            Status {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                context: Some(4),
                cost: Some(0.1020596),
                limits: Limits {
                    five_hours: Some(Limit {
                        used: 9,
                        resets_at: 1_791_870_600,
                    }),
                    week: Some(Limit {
                        used: 30,
                        resets_at: 1_792_296_000,
                    }),
                },
                lines: Some((12, 3)),
                duration: Some(std::time::Duration::from_millis(5066)),
                state: None,
                activity: None,
            }
        );
        assert_eq!(Claude.status(b"", b""), Status::default());
        assert_eq!(Claude.status(b"not json", b""), Status::default());
        let early = br#"{"model":{"display_name":""},"effort":{"level":""},"context_window":{"used_percentage":null},"cost":{"total_cost_usd":-1}}"#;
        assert_eq!(
            Claude.status(early, b""),
            Status::default(),
            "nothing usable yet"
        );
        let over = br#"{"context_window":{"used_percentage":180.6}}"#;
        assert_eq!(Claude.status(over, b"").context, Some(100));
        let almost = br#"{"rate_limits":{"five_hour":{"used_percentage":99.6,"resets_at":5}}}"#;
        assert_eq!(
            Claude
                .status(almost, b"")
                .limits
                .five_hours
                .map(|limit| limit.used),
            Some(99),
            "100 only once used up"
        );
        // A window without both fields, or out of range, as far as it can.
        let partial = br#"{"rate_limits":{"five_hour":{"used_percentage":12},"seven_day":{"used_percentage":101,"resets_at":5}}}"#;
        assert_eq!(
            Claude.status(partial, b"").limits,
            Limits {
                five_hours: None,
                week: Some(Limit {
                    used: 100,
                    resets_at: 5,
                }),
            }
        );
    }

    #[test]
    fn the_last_event_of_the_session_sets_the_state() {
        let events = |list: &[String]| list.concat().into_bytes();
        let notice = |kind: &str| {
            event(
                "Notification",
                "7adb",
                &format!(",\"notification_type\":\"{kind}\""),
            )
        };
        let prompt = event("UserPromptSubmit", "7adb", "");
        let stop = event("Stop", "7adb", ",\"stop_hook_active\":false");
        let failed_stop = event("StopFailure", "7adb", "");
        let tool = event("PostToolUse", "7adb", "");
        let failed_tool = event("PostToolUseFailure", "7adb", "");
        let sub_tool = event("PostToolUse", "7adb", ",\"agent_id\":\"sub-1\"");
        let ask = notice("permission_prompt");
        let state_after = |list: &[String]| Claude.status(STATUS, &events(list)).state;
        let working = Some(State::Working);
        let waiting = Some(State::Waiting);
        let attention = Some(State::Attention);

        assert_eq!(state_after(std::slice::from_ref(&prompt)), working);
        assert_eq!(state_after(&[prompt.clone(), stop.clone()]), waiting);
        assert_eq!(
            state_after(&[prompt.clone(), failed_stop]),
            waiting,
            "an API error ends the turn"
        );
        assert_eq!(
            state_after(&[prompt.clone(), notice("idle_prompt")]),
            waiting
        );
        for kind in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
        ] {
            assert_eq!(
                state_after(&[prompt.clone(), notice(kind)]),
                attention,
                "{kind}"
            );
        }
        assert_eq!(
            state_after(&[prompt.clone(), ask.clone(), tool.clone()]),
            working,
            "a tool that ran after the question means it was answered"
        );
        assert_eq!(
            state_after(&[prompt.clone(), ask.clone(), failed_tool]),
            working,
            "so does one that failed"
        );
        assert_eq!(
            state_after(&[
                prompt.clone(),
                notice("elicitation_dialog"),
                notice("elicitation_response")
            ]),
            working
        );
        // A subagent's tools change nothing: an open dialog stays shown, a
        // stopped turn stays stopped.
        assert_eq!(
            state_after(&[prompt.clone(), ask.clone(), sub_tool.clone()]),
            attention
        );
        assert_eq!(
            state_after(&[prompt.clone(), stop.clone(), sub_tool]),
            waiting
        );
        assert_eq!(
            state_after(&[prompt.clone(), notice("auth_success")]),
            working,
            "other notices change nothing"
        );

        // Another session's events, a torn last line and garbage are passed
        // over; without a known session every event counts.
        let other = event("Stop", "old", "");
        assert_eq!(state_after(&[prompt.clone(), other.clone()]), working);
        let torn = format!("{prompt}{}", &stop[..stop.len() - 10]);
        assert_eq!(Claude.status(STATUS, torn.as_bytes()).state, working);
        let garbage = format!("{prompt}not json\n");
        assert_eq!(Claude.status(STATUS, garbage.as_bytes()).state, working);
        assert_eq!(Claude.status(b"", other.as_bytes()).state, waiting);
        assert_eq!(Claude.status(STATUS, b"").state, None);
        let ignored = [other, notice("auth_success")].concat();
        assert_eq!(Claude.status(STATUS, ignored.as_bytes()).state, None);
    }

    #[test]
    fn a_tool_about_to_run_is_the_activity() {
        let events = |list: &[String]| list.concat().into_bytes();
        let pre = |tool: &str, extra: &str| {
            event(
                "PreToolUse",
                "7adb",
                &format!(",\"tool_name\":\"{tool}\"{extra}"),
            )
        };
        let after = |list: &[String]| {
            let status = Claude.status(STATUS, &events(list));
            (status.state, status.activity)
        };
        let prompt = event("UserPromptSubmit", "7adb", "");
        let edit = pre(
            "Edit",
            ",\"tool_input\":{\"file_path\":\"/p/src/ui/projects.rs\"}",
        );
        let working = |activity: Option<&str>| (Some(State::Working), activity.map(str::to_owned));
        assert_eq!(after(std::slice::from_ref(&prompt)), working(None));
        assert_eq!(
            after(&[prompt.clone(), edit.clone()]),
            working(Some("Edit projects.rs"))
        );
        assert_eq!(
            after(&[prompt.clone(), pre("Bash", "")]),
            working(Some("Bash"))
        );
        assert_eq!(
            after(&[
                prompt.clone(),
                pre("mcp__codebase-memory__search_graph", "")
            ]),
            working(Some("search_graph")),
            "an MCP tool without its server"
        );
        // Done, asking or stopped: no activity.
        let done = event("PostToolUse", "7adb", "");
        assert_eq!(after(&[prompt.clone(), edit.clone(), done]), working(None));
        let ask = event(
            "Notification",
            "7adb",
            ",\"notification_type\":\"permission_prompt\"",
        );
        assert_eq!(
            after(&[prompt.clone(), edit.clone(), ask]),
            (Some(State::Attention), None)
        );
        let stop = event("Stop", "7adb", "");
        assert_eq!(
            after(&[prompt.clone(), edit.clone(), stop]),
            (Some(State::Waiting), None)
        );
        // A subagent's tools are not the main agent's activity.
        let sub = pre("Read", ",\"agent_id\":\"sub-1\"");
        assert_eq!(
            after(&[prompt.clone(), edit.clone(), sub]),
            working(Some("Edit projects.rs"))
        );
        assert_eq!(
            after(&[prompt.clone(), pre("mcp__srv__", "")]),
            working(Some("mcp__srv__")),
            "a name that strips to nothing stays whole"
        );
        // An interrupt during the tool ends it.
        let interrupted = event("PostToolUseFailure", "7adb", ",\"is_interrupt\":true");
        assert_eq!(
            after(&[prompt.clone(), edit.clone(), interrupted]),
            working(None)
        );
        // The hooks run in parallel: a quick tool's end can be written
        // before its start, and then it is not running.
        let start = pre("Read", ",\"tool_use_id\":\"t1\"");
        let end = event("PostToolUse", "7adb", ",\"tool_use_id\":\"t1\"");
        let other_end = event("PostToolUse", "7adb", ",\"tool_use_id\":\"t0\"");
        assert_eq!(after(&[prompt.clone(), end, start.clone()]), working(None));
        assert_eq!(
            after(&[prompt.clone(), other_end, start]),
            working(Some("Read"))
        );
        assert_eq!(after(&[prompt, pre("", "")]), working(None), "no name");
    }

    #[test]
    fn has_a_name() {
        assert_eq!(Claude.name(), "claude");
        assert_eq!(super::super::default_agent().name(), "claude");
    }
}
