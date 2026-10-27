//! The instance directory (ADR-0003): what the agent's session writes for
//! the app through the `statusline` and `hook` subcommands, in files the
//! app reads.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// The environment variable naming the instance directory, which the
/// agent's status line and hooks inherit.
pub const INSTANCE_DIR_VAR: &str = "TERMIS_DIR";
/// The agent's last status line input, as it piped it.
pub const STATUS_FILE: &str = "status.json";
/// The status line command the user had before the session, if any.
pub const PREVIOUS_STATUS_LINE_FILE: &str = "statusline.prev";
/// The agent's hook events, one JSON object per line, in the order
/// received: `{"hook": <the kept input fields>, "received": <seconds
/// since the epoch, zero when unknown>}`. A reader takes only lines
/// ended by a newline and passes over one that does not parse, since a
/// write cut short can leave a torn line.
pub const EVENTS_FILE: &str = "events.jsonl";
/// The hook input fields kept in the events file: what the app reads.
/// Prompts, message bodies and the like stay out, so the file does not
/// grow with the conversation.
const KEPT_FIELDS: [&str; 9] = [
    "hook_event_name",
    "session_id",
    "agent_id",
    "stop_hook_active",
    "notification_type",
    "message",
    "title",
    "tool_name",
    "tool_use_id",
];
/// The fields of a tool's input kept with it: the file the tool works
/// on, or the notebook. Commands, contents and edits stay out.
const KEPT_TOOL_INPUT: [&str; 2] = ["file_path", "notebook_path"];
/// The longest file path kept, in bytes; a longer one is left out.
const KEPT_PATH_MAX: usize = 4096;

/// The instance directory the agent's shims were given, when the
/// variable names an absolute path; a relative one is not trusted, as a
/// hook runs in the agent's working directory.
pub fn instance_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(INSTANCE_DIR_VAR)?);
    dir.is_absolute().then_some(dir)
}

/// Runs the `statusline` subcommand in the instance at `dir`: keeps
/// `input`, the JSON the agent piped, as the instance's status, then runs
/// the status line command the user had before the session with the same
/// input, its output going straight through. Returns the exit code to end
/// with: that command's, 128 plus the signal when one ended it, or zero
/// without a command. A status that could not be kept is reported on
/// stderr and does not stop the user's command: the agent shows its
/// output only when the exit code is zero.
pub fn statusline(dir: &Path, input: &[u8]) -> Result<i32> {
    let status = dir.join(STATUS_FILE);
    if let Err(error) = write_atomically(&status, input) {
        eprintln!("termis statusline: write {}: {error}", status.display());
    }
    chain(dir, input)
}

/// Runs the previous status line command, read from the instance, with
/// `input` on its stdin; zero when there is none.
fn chain(dir: &Path, input: &[u8]) -> Result<i32> {
    let path = dir.join(PREVIOUS_STATUS_LINE_FILE);
    let command = match fs::read_to_string(&path) {
        Ok(command) => command,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let command = command.trim();
    if command.is_empty() {
        return Ok(0);
    }
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .spawn()
        .context("run the previous status line command")?;
    if let Some(mut stdin) = child.stdin.take() {
        // A command that does not read its input is fine.
        let _ = stdin.write_all(input);
    }
    let status = child
        .wait()
        .context("wait for the previous status line command")?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// Runs the `hook` subcommand in the instance at `dir`: appends `input`,
/// the JSON object the agent piped to the hook, as one line of the events
/// file, keeping the fields in [`KEPT_FIELDS`], those of a tool's input in
/// [`KEPT_TOOL_INPUT`] that are strings of at most [`KEPT_PATH_MAX`]
/// bytes, and the time it arrived.
/// An input that is not a JSON object is left out. Each line goes in one
/// write to a file opened for appending, which keeps the lines of hooks
/// the agent runs in parallel apart as far as the kernel allows.
pub fn hook(dir: &Path, input: &[u8]) -> Result<()> {
    let mut fields = match serde_json::from_slice::<Value>(input) {
        Ok(Value::Object(fields)) => fields,
        Ok(_) => anyhow::bail!("the hook input is not a JSON object"),
        Err(error) => return Err(error).context("parse the hook input"),
    };
    let input: Map<String, Value> = KEPT_TOOL_INPUT
        .iter()
        .filter_map(|&name| {
            let path = fields.get("tool_input")?.get(name)?.as_str()?;
            (path.len() <= KEPT_PATH_MAX).then(|| (name.to_owned(), Value::from(path)))
        })
        .collect();
    fields.retain(|field, _| KEPT_FIELDS.contains(&field.as_str()));
    if !input.is_empty() {
        fields.insert("tool_input".to_owned(), Value::Object(input));
    }
    let received = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut line = Map::new();
    line.insert("received".to_owned(), Value::from(received));
    line.insert("hook".to_owned(), Value::Object(fields));
    let mut bytes = serde_json::to_vec(&Value::Object(line)).context("encode the event")?;
    bytes.push(b'\n');
    let path = dir.join(EVENTS_FILE);
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(&bytes))
        .with_context(|| format!("append to {}", path.display()))
}

/// Writes `bytes` to `path` through a temporary file of this process,
/// created anew next to it and readable by its owner only, then renamed
/// into place: a reader sees the old file or the new one, never a
/// half-written one, and two writers never share the temporary file.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "no file name"))?;
    let mut temporary = name.to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[test]
    fn writes_atomically_and_replaces() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("status.json");
        write_atomically(&path, b"{\"a\":1}").expect("writes");
        assert_eq!(fs::read(&path).expect("read"), b"{\"a\":1}");
        assert_eq!(fs::metadata(&path).expect("stat").mode() & 0o777, 0o600);
        write_atomically(&path, b"{\"a\":2}").expect("replaces");
        assert_eq!(fs::read(&path).expect("read"), b"{\"a\":2}");
        let names: Vec<_> = fs::read_dir(dir.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["status.json"], "no temporary file left");
        // A planted temporary file is never followed or reused.
        let planted = dir
            .path()
            .join(format!("status.json.{}.tmp", std::process::id()));
        fs::write(&planted, "x").expect("plant");
        assert!(write_atomically(&path, b"{\"a\":3}").is_err());
        assert_eq!(fs::read(&path).expect("read"), b"{\"a\":2}", "untouched");
        assert!(write_atomically(Path::new("/"), b"x").is_err());
        let missing = dir.path().join("gone/status.json");
        assert_eq!(
            write_atomically(&missing, b"x")
                .expect_err("no directory")
                .kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn statusline_keeps_the_input_and_runs_the_previous_command_with_it() {
        let dir = TempDir::new().expect("temp dir");
        let copy = dir.path().join("copy.json");
        // The previous command gets the same input and sets the exit code.
        let previous = format!("cat > '{}'; exit 3\n", copy.display());
        fs::write(dir.path().join(PREVIOUS_STATUS_LINE_FILE), previous).expect("write");
        let code = statusline(dir.path(), b"{\"model\":{}}").expect("runs");
        assert_eq!(code, 3);
        assert_eq!(
            fs::read(dir.path().join(STATUS_FILE)).expect("status"),
            b"{\"model\":{}}"
        );
        assert_eq!(fs::read(&copy).expect("copy"), b"{\"model\":{}}");

        // A command that ignores its input still works; one ended by a
        // signal reports it as a shell would.
        fs::write(dir.path().join(PREVIOUS_STATUS_LINE_FILE), "exit 0\n").expect("write");
        assert_eq!(statusline(dir.path(), b"{}").expect("runs"), 0);
        fs::write(
            dir.path().join(PREVIOUS_STATUS_LINE_FILE),
            "kill -TERM $$\n",
        )
        .expect("write");
        assert_eq!(statusline(dir.path(), b"{}").expect("runs"), 143);
        // An empty command and no command both count as nothing to run.
        fs::write(dir.path().join(PREVIOUS_STATUS_LINE_FILE), " \n").expect("write");
        assert_eq!(statusline(dir.path(), b"{}").expect("runs"), 0);
        fs::remove_file(dir.path().join(PREVIOUS_STATUS_LINE_FILE)).expect("rm");
        assert_eq!(statusline(dir.path(), b"{}").expect("runs"), 0);
        assert_eq!(
            fs::read(dir.path().join(STATUS_FILE)).expect("status"),
            b"{}"
        );
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn events(dir: &Path) -> Vec<Value> {
        let text = fs::read_to_string(dir.join(EVENTS_FILE)).expect("events");
        assert!(text.ends_with('\n'), "every line ends");
        text.lines()
            .map(|line| serde_json::from_str(line).expect("one object per line"))
            .collect()
    }

    #[test]
    fn hook_appends_one_line_per_event_with_the_fields_the_app_reads() {
        let dir = TempDir::new().expect("temp dir");
        let before = now();
        hook(
            dir.path(),
            b"{\n \"hook_event_name\": \"Stop\",\n \"last_assistant_message\": \"long\",\n \"stop_hook_active\": false\n}\n",
        )
        .expect("appends");
        let prompt = format!(
            "{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"s1\",\"prompt\":\"{}\"}}",
            "x".repeat(64 * 1024)
        );
        hook(dir.path(), prompt.as_bytes()).expect("appends");
        hook(dir.path(), b"{\"hook_event_name\":\"Notification\",\"message\":\"m\",\"title\":\"t\",\"notification_type\":\"idle_prompt\"}").expect("appends");
        let after = now();
        let lines = events(dir.path());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["hook"]["hook_event_name"], "Stop");
        assert_eq!(lines[0]["hook"]["stop_hook_active"], false);
        assert!(lines[0]["hook"].get("last_assistant_message").is_none());
        let received = lines[0]["received"].as_u64().expect("seconds");
        assert!(before <= received && received <= after);
        assert_eq!(lines[1]["hook"]["session_id"], "s1");
        assert!(
            lines[1]["hook"].get("prompt").is_none(),
            "the prompt stays out"
        );
        let raw = fs::read_to_string(dir.path().join(EVENTS_FILE)).expect("events");
        assert!(
            raw.lines().nth(1).expect("line").len() < 1024,
            "short without the prompt"
        );
        assert_eq!(lines[2]["hook"]["notification_type"], "idle_prompt");
        assert_eq!(lines[2]["hook"]["title"], "t");
        assert_eq!(
            fs::metadata(dir.path().join(EVENTS_FILE))
                .expect("stat")
                .mode()
                & 0o777,
            0o600
        );
        // Not an object: left out, reported.
        assert!(hook(dir.path(), b"[1,2]").is_err());
        assert!(
            hook(dir.path(), b"nonsense")
                .expect_err("reported")
                .to_string()
                .contains("parse")
        );
        assert_eq!(events(dir.path()).len(), 3);
        // A tool keeps its name and its file, nothing else of its input.
        hook(
            dir.path(),
            br#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/p/src/a.rs","old_string":"secret","new_string":"x"},"tool_use_id":"t1"}"#,
        )
        .expect("appends");
        hook(
            dir.path(),
            br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"export TOKEN=x"}}"#,
        )
        .expect("appends");
        let lines = events(dir.path());
        assert_eq!(
            lines[3]["hook"],
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Edit",
                "tool_use_id": "t1",
                "tool_input": { "file_path": "/p/src/a.rs" },
            })
        );
        assert_eq!(
            lines[4]["hook"],
            serde_json::json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash" })
        );
        // A path that is not a string, or too long to be one, stays out.
        hook(
            dir.path(),
            br#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":7}}"#,
        )
        .expect("appends");
        let long = format!(
            "{{\"hook_event_name\":\"PreToolUse\",\"tool_input\":{{\"file_path\":\"/{}\"}}}}",
            "a".repeat(KEPT_PATH_MAX)
        );
        hook(dir.path(), long.as_bytes()).expect("appends");
        let lines = events(dir.path());
        assert_eq!(lines.len(), 7);
        assert!(lines[5]["hook"].get("tool_input").is_none());
        assert!(lines[6]["hook"].get("tool_input").is_none());
        // A notebook keeps its path, not its cell's source.
        hook(
            dir.path(),
            br#"{"hook_event_name":"PostToolUse","tool_name":"NotebookEdit","tool_input":{"notebook_path":"/p/n.ipynb","new_source":"secret","cell_id":"c1"}}"#,
        )
        .expect("appends");
        assert_eq!(
            events(dir.path())[7]["hook"]["tool_input"],
            serde_json::json!({ "notebook_path": "/p/n.ipynb" })
        );
        // No instance directory: an error, not a crash.
        assert!(hook(&dir.path().join("gone"), b"{}").is_err());
    }

    #[test]
    fn hook_lines_survive_their_length_and_parallel_writers() {
        let dir = TempDir::new().expect("temp dir");
        let long = format!(
            "{{\"hook_event_name\":\"Notification\",\"message\":\"{}\"}}",
            "m".repeat(6 * 1024)
        );
        hook(dir.path(), long.as_bytes()).expect("appends");
        let lines = events(dir.path());
        assert_eq!(
            lines[0]["hook"]["message"].as_str().map(str::len),
            Some(6 * 1024)
        );

        let writers: Vec<_> = (0..8)
            .map(|writer| {
                let dir = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    for n in 0..200 {
                        let input = format!(
                            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{writer}-{n}\",\"message\":\"{}\"}}",
                            "p".repeat(300)
                        );
                        hook(&dir, input.as_bytes()).expect("appends");
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("writer ends");
        }
        let lines = events(dir.path());
        assert_eq!(lines.len(), 1 + 8 * 200, "every line parses");
    }

    #[test]
    fn the_instance_dir_must_be_absolute() {
        // The variable belongs to the process; only what holds whatever it
        // says can be checked here.
        assert!(instance_dir().is_none_or(|dir| dir.is_absolute()));
    }

    #[test]
    fn statusline_still_runs_the_previous_command_when_the_instance_is_gone() {
        let dir = TempDir::new().expect("temp dir");
        let gone = dir.path().join("gone");
        // Nothing to keep the status in and no previous command to find.
        assert_eq!(statusline(&gone, b"{}").expect("runs"), 0);
        assert!(!gone.exists());
    }
}
