//! The `termis hook` subcommand, as the agent runs it.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

/// Runs the subcommand with `input` on stdin and `dir` as the instance,
/// or no instance at all.
fn run(dir: Option<&std::path::Path>, input: &[u8]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_termis"));
    command
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("TERMIS_DIR");
    if let Some(dir) = dir {
        command.env("TERMIS_DIR", dir);
    }
    let mut child = command.spawn().expect("spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("writes");
    child.wait_with_output().expect("ends")
}

#[test]
fn appends_events_in_order_and_never_fails_the_agent() {
    let dir = TempDir::new().expect("temp dir");
    for input in [
        &b"{\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"hi\"}\n"[..],
        b"{\"hook_event_name\":\"Stop\",\"last_assistant_message\":\"x\"}\n",
        b"not json",
    ] {
        let output = run(Some(dir.path()), input);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
    }
    let text = fs::read_to_string(dir.path().join("events.jsonl")).expect("events");
    assert!(text.ends_with('\n'));
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("one object per line"))
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0]["hook"]["hook_event_name"], "UserPromptSubmit");
    assert!(lines[0]["hook"].get("prompt").is_none());
    assert!(lines[0]["received"].as_u64().is_some());
    assert_eq!(lines[1]["hook"]["hook_event_name"], "Stop");
    assert!(lines[1]["hook"].get("last_assistant_message").is_none());

    // Without an instance the input is still drained, and nothing is
    // printed: stdout would become context for the agent.
    let big = vec![b'{'; 4 * 1024 * 1024];
    let output = run(None, &big);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let output = run(Some(&dir.path().join("gone")), b"{}");
    assert_eq!(
        output.status.code(),
        Some(0),
        "a lost instance is reported, not fatal"
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("events.jsonl"));
    // A relative instance is not used: nothing lands in the working
    // directory, which for a hook is the project.
    let cwd = TempDir::new().expect("temp dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_termis"));
    let mut child = command
        .arg("hook")
        .current_dir(cwd.path())
        .env("TERMIS_DIR", "relative")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"{\"hook_event_name\":\"Stop\"}")
        .expect("writes");
    let output = child.wait_with_output().expect("ends");
    assert_eq!(output.status.code(), Some(0));
    assert!(!cwd.path().join("relative").exists());
}
