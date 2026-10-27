//! The `termis statusline` subcommand, as the agent runs it.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

const INPUT: &[u8] = b"{\"model\":{\"display_name\":\"Opus\"}}\n";

/// Runs the subcommand with `input` on stdin and `dir` as the instance,
/// or no instance at all.
fn run(dir: Option<&std::path::Path>, input: &[u8]) -> std::process::Output {
    // A state directory of the test's own, never the user's.
    let state = TempDir::new().expect("temp dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_termis"));
    command
        .env("XDG_STATE_HOME", state.path())
        .arg("statusline")
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
fn passes_the_previous_command_output_through_and_keeps_the_input() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(
        dir.path().join("statusline.prev"),
        "echo LINE1; echo LINE2; echo ERR >&2; exit 0\n",
    )
    .expect("write");
    let output = run(Some(dir.path()), INPUT);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"LINE1\nLINE2\n");
    assert_eq!(output.stderr, b"ERR\n");
    assert_eq!(
        fs::read(dir.path().join("status.json")).expect("status"),
        INPUT
    );
}

#[test]
fn reports_the_previous_command_exit_code() {
    let dir = TempDir::new().expect("temp dir");
    fs::write(dir.path().join("statusline.prev"), "no-such-command-xyz\n").expect("write");
    let output = run(Some(dir.path()), INPUT);
    assert_eq!(output.status.code(), Some(127));
    assert!(output.stdout.is_empty());
}

#[test]
fn does_nothing_without_an_instance_and_drains_its_input() {
    let big = vec![b'{'; 4 * 1024 * 1024];
    let output = run(None, &big);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn a_lost_instance_does_not_fail_the_command() {
    let dir = TempDir::new().expect("temp dir");
    let gone = dir.path().join("gone");
    let output = run(Some(&gone), INPUT);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("status.json"),
        "the lost status is reported"
    );
}
