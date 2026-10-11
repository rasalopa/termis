//! The instance directory of a session (ADR-0003): a private directory
//! for the files the agent's shims write and the app reads, made for
//! the process and removed on an orderly quit or a panic. Directories
//! of processes that died otherwise are swept at the next start.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::telemetry;

/// What an instance directory is called: this prefix and the process id.
const PREFIX: &str = "termis-";

/// A session's instance: its directory, removed with the value, and the
/// app's own executable, which the agent's shims are. Both paths are
/// UTF-8, so they can go into a command line as they are.
#[derive(Debug)]
pub struct Instance {
    dir: PathBuf,
    exe: PathBuf,
}

impl Instance {
    /// Creates the directory for this process, readable by its owner
    /// only, under the temporary directory. One left behind by an earlier
    /// process with the same id is replaced, and those of this user's
    /// processes that are gone are removed.
    pub fn create() -> Result<Self> {
        let exe = std::env::current_exe().context("find the running executable")?;
        if exe.to_str().is_none() {
            bail!("the executable's path {} is not UTF-8", exe.display());
        }
        let temporary = std::env::temp_dir();
        if !temporary.is_absolute() {
            bail!(
                "the temporary directory {:?} is not an absolute path",
                temporary
            );
        }
        let dir = temporary.join(format!("{PREFIX}{}", std::process::id()));
        if dir.to_str().is_none() {
            bail!("the temporary directory {} is not UTF-8", dir.display());
        }
        match fs::symlink_metadata(&dir) {
            Ok(found) if found.is_dir() => fs::remove_dir_all(&dir),
            Ok(_) => fs::remove_file(&dir),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
        .with_context(|| format!("replace {}", dir.display()))?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("create {}", dir.display()))?;
        if let Ok(own) = fs::metadata(&dir) {
            sweep(&temporary, own.uid());
        }
        Ok(Self { dir, exe })
    }

    /// The instance directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The running executable.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Removes what a session left: its status and its events, not what
    /// the agent prepared, which it prepares again for the next session.
    pub fn clear(&self) {
        for name in [telemetry::STATUS_FILE, telemetry::EVENTS_FILE] {
            let _ = fs::remove_file(self.dir.join(name));
        }
    }

    /// An instance at `dir` that is not created here; it is still
    /// removed when dropped.
    #[cfg(test)]
    pub(crate) fn at(dir: PathBuf, exe: PathBuf) -> Self {
        Self { dir, exe }
    }
}

/// Notices when the files the shims write change.
#[derive(Debug, Default)]
pub struct Watch {
    seen: Option<[Signature; 2]>,
}

/// What tells a file's content apart without reading it: none for a
/// file that is not there.
type Signature = Option<(u64, u64, Option<SystemTime>)>;

impl Watch {
    /// The status and the events of `instance` when either file changed
    /// since the last call, or on the first; a missing file reads empty.
    pub fn changed(&mut self, instance: &Instance) -> Option<(Vec<u8>, Vec<u8>)> {
        let files =
            [telemetry::STATUS_FILE, telemetry::EVENTS_FILE].map(|name| instance.dir.join(name));
        let now = files.clone().map(|path| {
            fs::metadata(path)
                .ok()
                .map(|found| (found.ino(), found.len(), found.modified().ok()))
        });
        if self.seen == Some(now) {
            return None;
        }
        self.seen = Some(now);
        let [status, events] = files.map(|path| fs::read(path).unwrap_or_default());
        Some((status, events))
    }
}

impl Drop for Instance {
    /// Removes the directory and what the session left in it.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Removes the instance directories under `temporary` that belong to
/// `uid` and whose process is gone. Anything else is left alone: other
/// users' directories, live processes, names that only look alike.
fn sweep(temporary: &Path, uid: u32) {
    let Ok(entries) = fs::read_dir(temporary) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(PREFIX))
            .and_then(|digits| digits.parse::<i32>().ok())
            .filter(|&pid| pid > 0)
        else {
            continue;
        };
        let path = entry.path();
        let ours =
            fs::symlink_metadata(&path).is_ok_and(|found| found.is_dir() && found.uid() == uid);
        if ours && kill(Pid::from_raw(pid), None) == Err(Errno::ESRCH) {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn creates_a_private_directory_clears_it_and_removes_it() {
        // A dangling link where the directory goes is replaced.
        let name = format!("{PREFIX}{}", std::process::id());
        let planted = std::env::temp_dir().join(&name);
        let _ = fs::remove_dir_all(&planted);
        let _ = fs::remove_file(&planted);
        symlink("/nonexistent/termis-target", &planted).expect("plant a link");

        let instance = Instance::create().expect("creates");
        let dir = instance.dir().to_path_buf();
        assert!(dir.is_dir() && !fs::symlink_metadata(&dir).expect("stat").is_symlink());
        assert_eq!(fs::metadata(&dir).expect("stat").mode() & 0o777, 0o700);
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some(name.as_str())
        );
        assert!(instance.exe().is_file());

        fs::write(dir.join("status.json"), "{}").expect("write");
        fs::write(dir.join("events.jsonl"), "{}\n").expect("write");
        fs::write(dir.join("statusline.prev"), "cmd").expect("write");
        instance.clear();
        assert!(!dir.join("status.json").exists() && !dir.join("events.jsonl").exists());
        assert!(
            dir.join("statusline.prev").exists(),
            "kept for the next session"
        );

        // A second instance of the same process replaces the first.
        fs::write(dir.join("status.json"), "{}").expect("write");
        let again = Instance::create().expect("creates again");
        assert!(!dir.join("status.json").exists(), "replaced");
        drop(again);
        assert!(!dir.exists(), "removed with the instance");
        drop(instance);
    }

    #[test]
    fn the_watch_reads_again_only_after_a_change() {
        let dir = TempDir::new().expect("temp dir");
        let instance = Instance::at(dir.path().join("i"), PathBuf::from("/x"));
        fs::create_dir(instance.dir()).expect("mkdir");
        let mut watch = Watch::default();
        assert_eq!(
            watch.changed(&instance),
            Some((Vec::new(), Vec::new())),
            "the first call reads"
        );
        assert_eq!(watch.changed(&instance), None);
        telemetry::write_atomically(&instance.dir().join(telemetry::STATUS_FILE), b"{}")
            .expect("write");
        assert_eq!(watch.changed(&instance), Some((b"{}".to_vec(), Vec::new())));
        // The same length again, through a new file: still a change.
        telemetry::write_atomically(&instance.dir().join(telemetry::STATUS_FILE), b"[]")
            .expect("write");
        assert_eq!(watch.changed(&instance), Some((b"[]".to_vec(), Vec::new())));
        fs::write(instance.dir().join(telemetry::EVENTS_FILE), "a\n").expect("write");
        assert_eq!(
            watch.changed(&instance).map(|(_, events)| events),
            Some(b"a\n".to_vec())
        );
        assert_eq!(watch.changed(&instance), None);
        instance.clear();
        assert_eq!(
            watch.changed(&instance),
            Some((Vec::new(), Vec::new())),
            "cleared"
        );
    }

    #[test]
    fn sweeps_only_this_users_directories_of_dead_processes() {
        let temporary = TempDir::new().expect("temp dir");
        let uid = fs::metadata(temporary.path()).expect("stat").uid();
        // A process that has ended: its id is free for a while.
        let mut gone = Command::new("true").spawn().expect("spawns");
        let dead = gone.id();
        gone.wait().expect("ends");
        let alive = std::process::id();
        let make = |name: &str| {
            let path = temporary.path().join(name);
            fs::create_dir(&path).expect("mkdir");
            path
        };
        let dead_dir = make(&format!("{PREFIX}{dead}"));
        let alive_dir = make(&format!("{PREFIX}{alive}"));
        let lookalike = make(&format!("{PREFIX}x{dead}"));
        let file = temporary.path().join(format!("{PREFIX}{}", dead + 1));
        fs::write(&file, "").expect("write");

        sweep(temporary.path(), uid + 1);
        assert!(dead_dir.exists(), "another user's uid: left alone");
        sweep(temporary.path(), uid);
        assert!(!dead_dir.exists(), "a dead process's directory goes");
        assert!(alive_dir.exists() && lookalike.exists() && file.exists());
        sweep(&temporary.path().join("missing"), uid);
    }
}
