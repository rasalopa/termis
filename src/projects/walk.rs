//! The files of a directory outside a repository, found by a walk off
//! the main thread that answers over the app's channel (ADR-0002).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::FileType;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

use crate::git::files::{Files, LIMIT};

/// The walk's answer for one directory.
#[derive(Debug)]
pub struct Walked {
    /// The directory walked.
    pub dir: PathBuf,
    /// Its files, relative to it, or why it could not be read.
    pub files: Result<Files>,
}

/// Walks `dir` on a thread of its own, which answers once on `tx`,
/// wrapped by `tag` into the owner's message type, and ends.
pub fn spawn<E>(
    dir: PathBuf,
    tx: Sender<E>,
    tag: impl FnOnce(Walked) -> E + Send + 'static,
) -> Result<()>
where
    E: Send + 'static,
{
    thread::Builder::new()
        .name("file-walker".into())
        .spawn(move || {
            let files = list(&dir);
            let _ = tx.send(tag(Walked { dir, files }));
        })
        .context("spawn the file walker")?;
    Ok(())
}

/// The files under `dir`, relative to it, up to [`LIMIT`], sorted by
/// their bytes as git sorts them. A file is a regular file or a symbolic
/// link to anything but a directory. Directories whose names start with
/// `.` are skipped, links to directories are not followed, and a
/// directory under `dir` that cannot be read is skipped. When there are
/// more files, those nearer `dir` are kept: a directory's own before
/// those of the directories in it. Fails when `dir` cannot be read.
pub fn list(dir: &Path) -> Result<Files> {
    list_up_to(dir, LIMIT)
}

/// Like [`list`], up to `limit` files.
fn list_up_to(dir: &Path, limit: usize) -> Result<Files> {
    let mut files = Files::default();
    // Directories to read, relative to `dir`, each level before the next.
    let mut queue = VecDeque::from([PathBuf::new()]);
    'walk: while let Some(relative) = queue.pop_front() {
        let entries = match std::fs::read_dir(dir.join(&relative)) {
            Ok(entries) => entries,
            Err(error) if relative.as_os_str().is_empty() => {
                return Err(error).with_context(|| format!("read {}", dir.display()));
            }
            Err(_) => continue,
        };
        let mut found: Vec<(OsString, Kind)> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let kind = Kind::of(entry.file_type().ok()?, &entry.path());
                Some((entry.file_name(), kind))
            })
            .collect();
        found.sort_unstable();
        for (name, kind) in found {
            match kind {
                Kind::Directory if name.as_bytes().starts_with(b".") => {}
                Kind::Directory => queue.push_back(relative.join(name)),
                Kind::File if files.paths.len() == limit => {
                    files.cut = true;
                    break 'walk;
                }
                Kind::File => files.paths.push(relative.join(name)),
                Kind::Other => {}
            }
        }
    }
    files
        .paths
        .sort_unstable_by(|a, b| a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes()));
    Ok(files)
}

/// What the walk makes of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A directory to walk into.
    Directory,
    /// A file to list.
    File,
    /// Neither: a link to a directory, a socket, a pipe, a device.
    Other,
}

impl Kind {
    /// The kind of the entry at `path` of type `file_type`, read without
    /// following a link; a link is a file unless it leads to a directory,
    /// a broken one included.
    fn of(file_type: FileType, path: &Path) -> Self {
        if file_type.is_dir() {
            Self::Directory
        } else if file_type.is_file() || (file_type.is_symlink() && !path.is_dir()) {
            Self::File
        } else {
            Self::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long an answer gets.
    const WAIT: Duration = Duration::from_secs(10);

    /// The paths of `files` as text.
    fn names(files: &Files) -> Vec<String> {
        files
            .paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// Writes an empty file at each of `paths` under `dir`, with the
    /// directories they need.
    fn touch(dir: &Path, paths: &[&str]) {
        for path in paths {
            let path = dir.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(path, "").expect("write");
        }
    }

    #[test]
    fn lists_nested_files_sorted_and_skips_hidden_directories() {
        let dir = TempDir::new().expect("temp dir");
        touch(
            dir.path(),
            &[
                "z.txt",
                ".env",
                "a-b",
                "a/b/c.rs",
                "a/x y.md",
                "new\nline",
                ".git/HEAD",
                ".cache/a/b",
                "src/.hidden/inside.rs",
            ],
        );
        std::fs::create_dir(dir.path().join("empty")).expect("mkdir");
        let files = list(dir.path()).expect("lists");
        assert_eq!(
            names(&files),
            [".env", "a-b", "a/b/c.rs", "a/x y.md", "new\nline", "z.txt"],
            "hidden files stay, as git sorts paths"
        );
        assert!(!files.cut);
    }

    #[test]
    fn links_to_directories_are_not_followed() {
        let dir = TempDir::new().expect("temp dir");
        touch(dir.path(), &["a/file.txt"]);
        // Loops: to the directory itself and to its parent.
        symlink(".", dir.path().join("self")).expect("link");
        symlink("..", dir.path().join("a/up")).expect("link");
        // A link to a file is a file; one that leads nowhere too.
        symlink("a/file.txt", dir.path().join("to-file")).expect("link");
        symlink("missing", dir.path().join("broken")).expect("link");
        let files = list(dir.path()).expect("lists");
        assert_eq!(names(&files), ["a/file.txt", "broken", "to-file"]);
    }

    #[test]
    fn a_long_walk_keeps_the_files_nearer_the_top_and_says_it_was_cut() {
        let dir = TempDir::new().expect("temp dir");
        touch(dir.path(), &["a/1", "a/2", "b/deep/3", "y", "z"]);
        let files = list_up_to(dir.path(), 3).expect("lists");
        assert_eq!(names(&files), ["a/1", "y", "z"]);
        assert!(files.cut);
        let files = list_up_to(dir.path(), 5).expect("lists");
        assert_eq!(names(&files), ["a/1", "a/2", "b/deep/3", "y", "z"]);
        assert!(!files.cut, "exactly the limit");
    }

    #[test]
    fn an_unreadable_directory_is_skipped_unless_it_is_the_one_walked() {
        let dir = TempDir::new().expect("temp dir");
        touch(dir.path(), &["open.txt", "locked/secret.txt"]);
        let locked = dir.path().join("locked");
        let mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        mode(&locked, 0o000);
        // A superuser reads it all the same.
        let blocked = std::fs::read_dir(&locked).is_err();
        let files = list(dir.path());
        let walked = list(&locked);
        mode(&locked, 0o755);
        let files = files.expect("lists the rest");
        if blocked {
            assert_eq!(names(&files), ["open.txt"]);
            let error = walked.expect_err("cannot read it");
            let message = format!("{error:#}");
            assert!(message.contains("locked"), "{message}");
        }
        let missing = dir.path().join("missing");
        assert!(list(&missing).is_err(), "nothing there");
    }

    #[test]
    fn the_walk_answers_on_a_thread_of_its_own_with_its_directory() {
        let dir = TempDir::new().expect("temp dir");
        touch(dir.path(), &["a.txt"]);
        let (tx, rx) = mpsc::channel();
        spawn(dir.path().to_path_buf(), tx, |walked| {
            (thread::current().name().map(str::to_owned), walked)
        })
        .expect("spawns");
        let (thread, walked) = rx.recv_timeout(WAIT).expect("an answer");
        assert_eq!(thread.as_deref(), Some("file-walker"));
        assert_eq!(walked.dir, dir.path());
        assert_eq!(names(&walked.files.expect("lists")), ["a.txt"]);
        assert_eq!(
            rx.recv_timeout(WAIT).expect_err("once"),
            RecvTimeoutError::Disconnected,
            "the thread ends"
        );
    }
}
