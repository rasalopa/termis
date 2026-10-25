//! A file of a project read for the viewer, off the main thread, which
//! answers over the app's channel (ADR-0002).

use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

/// The most bytes of a file the viewer shows: 1 MiB.
pub const LIMIT: u64 = 1024 * 1024;

/// How many bytes at the start of a file are looked at for a NUL byte,
/// which makes it binary, as git does.
const PROBE: usize = 8000;

/// What a read of a file found.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// Its text, decoded lossily where it is not UTF-8.
    Text(String),
    /// A NUL byte in its first 8,000 bytes.
    Binary,
    /// More than [`LIMIT`] bytes.
    TooLarge,
    /// Not a regular file once links are followed: a directory, a pipe,
    /// a device or a socket.
    NotAFile,
}

/// Reads the file at `path` on a thread of its own, which answers once
/// on `tx`, wrapped by `tag` into the owner's message type, and ends.
/// `tag` runs on that thread.
pub fn spawn<E>(
    path: PathBuf,
    tx: Sender<E>,
    tag: impl FnOnce(Result<Read>) -> E + Send + 'static,
) -> Result<()>
where
    E: Send + 'static,
{
    thread::Builder::new()
        .name("file-reader".into())
        .spawn(move || {
            let read = read(&path);
            let _ = tx.send(tag(read));
        })
        .context("spawn the file reader")?;
    Ok(())
}

/// Reads the file at `path`. Only a regular file, once links are
/// followed, is opened, and without waiting for a writer, so a pipe put
/// in its place meanwhile is found once it is open, before a read; one
/// over [`LIMIT`] is not opened. Fails when the file cannot be looked
/// at, opened or read.
pub fn read(path: &Path) -> Result<Read> {
    let failed = || format!("read {}", path.display());
    let metadata = std::fs::metadata(path).with_context(failed)?;
    if !metadata.is_file() {
        return Ok(Read::NotAFile);
    }
    if metadata.len() > LIMIT {
        return Ok(Read::TooLarge);
    }
    let Some(file) = open_regular(path).with_context(failed)? else {
        return Ok(Read::NotAFile);
    };
    contents(file).with_context(failed)
}

/// Opens the file at `path` to read without waiting for a writer, and
/// keeps it only when it is a regular file once open; none for anything
/// else.
fn open_regular(path: &Path) -> std::io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    Ok(file.metadata()?.is_file().then_some(file))
}

/// What `file` holds, read up to one byte past [`LIMIT`], since a file
/// can grow after its size was looked at.
fn contents(file: impl std::io::Read) -> std::io::Result<Read> {
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > LIMIT) {
        return Ok(Read::TooLarge);
    }
    if bytes[..bytes.len().min(PROBE)].contains(&0) {
        return Ok(Read::Binary);
    }
    Ok(Read::Text(String::from_utf8_lossy(&bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long an answer gets.
    const WAIT: Duration = Duration::from_secs(10);

    /// Writes `bytes` to `name` in `dir`. Returns its path.
    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    #[test]
    fn reads_text_and_shows_what_is_not_utf8_lossily() {
        let dir = TempDir::new().expect("temp dir");
        let text = file(dir.path(), "a.txt", b"one\ntwo\n");
        assert_eq!(read(&text).expect("reads"), Read::Text("one\ntwo\n".into()));
        let latin = file(dir.path(), "b.txt", b"caf\xe9\n");
        assert_eq!(
            read(&latin).expect("reads"),
            Read::Text("caf\u{FFFD}\n".into())
        );
        let empty = file(dir.path(), "empty", b"");
        assert_eq!(read(&empty).expect("reads"), Read::Text(String::new()));
        // A link to a file reads the file.
        symlink(&text, dir.path().join("link")).expect("link");
        assert_eq!(
            read(&dir.path().join("link")).expect("reads"),
            Read::Text("one\ntwo\n".into())
        );
    }

    #[test]
    fn a_nul_byte_near_the_start_makes_a_file_binary() {
        let dir = TempDir::new().expect("temp dir");
        let png = file(dir.path(), "a.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        assert_eq!(read(&png).expect("reads"), Read::Binary);
        let mut late = vec![b'a'; PROBE];
        late.push(0);
        let late = file(dir.path(), "late", &late);
        let Read::Text(text) = read(&late).expect("reads") else {
            panic!("text past the probe");
        };
        assert_eq!(text.len(), PROBE + 1);
        let last = vec![b'a'; PROBE - 1]
            .into_iter()
            .chain([0])
            .collect::<Vec<_>>();
        let last = file(dir.path(), "last", &last);
        assert_eq!(read(&last).expect("reads"), Read::Binary, "the last probed");
    }

    #[test]
    fn a_file_over_the_limit_is_too_large() {
        let dir = TempDir::new().expect("temp dir");
        let limit = usize::try_from(LIMIT).expect("fits");
        let whole = file(dir.path(), "whole", &vec![b'a'; limit]);
        assert!(matches!(read(&whole).expect("reads"), Read::Text(_)));
        let over = file(dir.path(), "over", &vec![b'a'; limit + 1]);
        assert_eq!(read(&over).expect("reads"), Read::TooLarge);
        // One that grows past its size is read no further than the limit.
        let endless = std::io::repeat(b'a');
        assert_eq!(contents(endless).expect("reads"), Read::TooLarge);
    }

    #[test]
    fn what_is_not_a_regular_file_is_never_read() {
        let dir = TempDir::new().expect("temp dir");
        assert_eq!(read(dir.path()).expect("looks"), Read::NotAFile);
        // A pipe, through a link: opening it to read would wait for a
        // writer.
        let fifo = dir.path().join("fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
        assert!(made.success());
        symlink(&fifo, dir.path().join("to-fifo")).expect("link");
        assert_eq!(
            read(&dir.path().join("to-fifo")).expect("looks"),
            Read::NotAFile
        );
        assert_eq!(read(Path::new("/dev/null")).expect("looks"), Read::NotAFile);
        let missing = read(&dir.path().join("missing")).expect_err("nothing there");
        let message = format!("{missing:#}");
        assert!(message.starts_with("read "), "{message}");
        assert!(message.contains("missing"), "{message}");
    }

    #[test]
    fn a_pipe_is_opened_without_waiting_for_a_writer_and_let_go() {
        let dir = TempDir::new().expect("temp dir");
        let fifo = dir.path().join("fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
        assert!(made.success());
        let link = dir.path().join("to-fifo");
        symlink(&fifo, &link).expect("link");
        for path in [fifo, link] {
            // On a thread, so a regression that waits fails the test
            // rather than hanging it.
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(open_regular(&path).map(|file| file.is_some()));
            });
            let kept = rx.recv_timeout(WAIT).expect("no wait for a writer");
            assert!(!kept.expect("opens"), "not a regular file");
        }
        let text = file(dir.path(), "a.txt", b"a\n");
        assert!(open_regular(&text).expect("opens").is_some());
    }

    #[test]
    fn the_read_answers_on_a_thread_of_its_own() {
        let dir = TempDir::new().expect("temp dir");
        let path = file(dir.path(), "a.txt", b"a\n");
        let (tx, rx) = mpsc::channel();
        spawn(path, tx, |read| {
            (thread::current().name().map(str::to_owned), read)
        })
        .expect("spawns");
        let (thread, read) = rx.recv_timeout(WAIT).expect("an answer");
        assert_eq!(thread.as_deref(), Some("file-reader"));
        assert_eq!(read.expect("reads"), Read::Text("a\n".into()));
        assert_eq!(
            rx.recv_timeout(WAIT).expect_err("once"),
            RecvTimeoutError::Disconnected,
            "the thread ends"
        );
    }
}
