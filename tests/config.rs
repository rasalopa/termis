//! The config file, as `termis` reads it at start.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// The directories a run gets in place of the user's: its home, state,
/// config, temporary and Claude directories, and a `PATH` with nothing
/// on it, so that no agent or git can start.
struct Places {
    home: TempDir,
    state: TempDir,
    config: TempDir,
    tmp: TempDir,
    claude: TempDir,
    path: TempDir,
}

impl Places {
    fn new() -> Self {
        let dir = || TempDir::new().expect("temp dir");
        Self {
            home: dir(),
            state: dir(),
            config: dir(),
            tmp: dir(),
            claude: dir(),
            path: dir(),
        }
    }

    /// Writes `text` as the config file.
    fn config(&self, text: &str) {
        let dir = self.config.path().join("termis");
        fs::create_dir_all(&dir).expect("config dir");
        fs::write(dir.join("config.toml"), text).expect("config");
    }

    /// The binary with these directories alone in its environment, and
    /// `args`, run in the home with `input` on stdin.
    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_termis"))
            .args(args)
            .env_clear()
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("TMPDIR", self.tmp.path())
            .env("CLAUDE_CONFIG_DIR", self.claude.path())
            .env("PATH", self.path.path())
            .current_dir(self.home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawns");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("writes");
        child.wait_with_output().expect("ends")
    }
}

/// Whether `dir` holds anything.
fn empty(dir: &Path) -> bool {
    fs::read_dir(dir).expect("read").next().is_none()
}

#[test]
fn a_bad_file_stops_termis_before_anything_starts() {
    let places = Places::new();
    places.config("[git]\npoll-seconds = 0\n");
    let output = places.run(&["--no-restore"], b"");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("termis/config.toml"), "{stderr}");
    assert!(stderr.contains("line 2, column 16"), "{stderr}");
    assert!(stderr.contains("from 1 to 60"), "{stderr}");
    // The screen was never taken, and nothing was written.
    assert!(!output.stdout.contains(&0x1b), "{:?}", output.stdout);
    assert!(empty(places.state.path()));
    assert!(empty(places.tmp.path()));
}

#[test]
fn the_subcommands_do_not_read_the_file() {
    let places = Places::new();
    places.config("[git]\npoll-seconds = 0\n");
    let output = places.run(&["hook"], b"{}");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn a_good_file_lets_termis_go_on_to_look_for_git() {
    let places = Places::new();
    places.config("[git]\npoll-seconds = 5\n");
    let output = places.run(&["--no-restore"], b"");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("git is not installed"), "{stderr}");
    assert!(!stderr.contains("config"), "{stderr}");
}
