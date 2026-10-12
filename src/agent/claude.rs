//! Claude Code as an agent.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use super::{Agent, Launch, State, Status};
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

/// Claude's flag for settings on the command line.
const SETTINGS_FLAG: &str = "--settings";

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

    /// Reads Claude's status line input for the model, the context in use,
    /// the cost, the lines and how long the session has run, and its hook
    /// events for the state.
    fn status(&self, status: &[u8], events: &[u8]) -> Status {
        let input = serde_json::from_slice::<Value>(status)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(Value::Null);
        let cost = &input["cost"];
        let lines = cost["total_lines_added"]
            .as_u64()
            .zip(cost["total_lines_removed"].as_u64());
        Status {
            model: input["model"]["display_name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            context: input["context_window"]["used_percentage"]
                .as_f64()
                .filter(|share| share.is_finite())
                .map(|share| share.clamp(0.0, 100.0).round() as u8),
            cost: cost["total_cost_usd"]
                .as_f64()
                .filter(|usd| usd.is_finite() && *usd >= 0.0),
            lines,
            duration: cost["total_duration_ms"]
                .as_u64()
                .map(std::time::Duration::from_millis),
            state: state(events, input["session_id"].as_str()),
        }
    }

    /// `claude` with the session's settings merged into the user's own
    /// `--settings`, if any, then the user's other arguments, and the
    /// instance directory in its environment. Without an instance the
    /// user's arguments go through untouched.
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
        let args = match instance {
            Some(instance) => {
                env.push((
                    telemetry::INSTANCE_DIR_VAR.to_owned(),
                    instance.dir().to_string_lossy().into_owned(),
                ));
                with_settings(launch, instance.exe())
            }
            None => launch.args.to_vec(),
        };
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
            "PostToolUse": hook,
            "PostToolUseFailure": hook,
            "Stop": hook,
            "StopFailure": hook,
            "Notification": hook,
        },
    })
}

/// The state the session's events leave it in, from the last event that
/// sets one: a prompt, or a tool of the main agent that ran or failed,
/// means working; a stop, a stop by an API error or the idle notice,
/// waiting; a permission or a question, attention; an answered question,
/// working again. Tool events of subagents set nothing, so a dialog one
/// of them left open is not hidden by another's work. Lines of another
/// session than `session`, when it is known, are passed over, and so are
/// a last line not yet ended and any line that does not parse. An
/// interrupt (Esc, or a denied permission) has no event: the state it
/// leaves holds until the next one, the idle notice at the latest.
fn state(events: &[u8], session: Option<&str>) -> Option<State> {
    let ended = events
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(&events[..0], |end| &events[..end]);
    // Every event either sets a state outright or sets none, so the last
    // one that sets one decides: read from the end.
    ended.rsplit(|&byte| byte == b'\n').find_map(|line| {
        let event = serde_json::from_slice::<Value>(line).ok()?;
        let hook = &event["hook"];
        if let (Some(want), Some(got)) = (session, hook["session_id"].as_str())
            && want != got
        {
            return None;
        }
        let main_agent = hook.get("agent_id").is_none();
        match hook["hook_event_name"].as_str()? {
            "UserPromptSubmit" => Some(State::Working),
            "PostToolUse" | "PostToolUseFailure" if main_agent => Some(State::Working),
            "Stop" | "StopFailure" => Some(State::Waiting),
            "Notification" => match hook["notification_type"].as_str()? {
                "idle_prompt" => Some(State::Waiting),
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input" => Some(State::Attention),
                "elicitation_response" => Some(State::Working),
                _ => None,
            },
            _ => None,
        }
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
        Launch { cwd, root, args }
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
        assert_eq!(settings["hooks"].as_object().map(Map::len), Some(6));
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
    const STATUS: &[u8] = br#"{"session_id":"7adb","model":{"id":"claude-opus-5-5","display_name":"Opus 5.5"},
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
                context: Some(4),
                cost: Some(0.1020596),
                lines: Some((12, 3)),
                duration: Some(std::time::Duration::from_millis(5066)),
                state: None,
            }
        );
        assert_eq!(Claude.status(b"", b""), Status::default());
        assert_eq!(Claude.status(b"not json", b""), Status::default());
        let early = br#"{"model":{"display_name":""},"context_window":{"used_percentage":null},"cost":{"total_cost_usd":-1}}"#;
        assert_eq!(
            Claude.status(early, b""),
            Status::default(),
            "nothing usable yet"
        );
        let over = br#"{"context_window":{"used_percentage":180.6}}"#;
        assert_eq!(Claude.status(over, b"").context, Some(100));
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
    fn has_a_name() {
        assert_eq!(Claude.name(), "claude");
        assert_eq!(super::super::default_agent().name(), "claude");
    }
}
