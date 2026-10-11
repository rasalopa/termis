//! The instance directory (ADR-0003): what the agent's session writes for
//! the app through the `statusline` subcommand, in files the app reads.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// The environment variable naming the instance directory, which the
/// agent's status line and hooks inherit.
pub const INSTANCE_DIR_VAR: &str = "TERMIS_DIR";
/// The agent's last status line input, as it piped it.
pub const STATUS_FILE: &str = "status.json";
/// The status line command the user had before the session, if any.
pub const PREVIOUS_STATUS_LINE_FILE: &str = "statusline.prev";

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

    #[test]
    fn statusline_still_runs_the_previous_command_when_the_instance_is_gone() {
        let dir = TempDir::new().expect("temp dir");
        let gone = dir.path().join("gone");
        // Nothing to keep the status in and no previous command to find.
        assert_eq!(statusline(&gone, b"{}").expect("runs"), 0);
        assert!(!gone.exists());
    }
}
