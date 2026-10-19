//! Instance directories (ADR-0003, ADR-0008): a private directory for
//! the process, and inside it one per session for the files the agent's
//! shims write and the app reads. A session's goes with the session, the
//! process's with everything in it on an orderly quit or a panic.
//! Directories of processes that died otherwise are swept at the next
//! start.

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

/// What a process's instance directory is called: this prefix and the
/// process id.
const PREFIX: &str = "termis-";

/// The process's instance directory, which holds the sessions' and is
/// removed with the value, everything in it included, and the app's own
/// executable, which the sessions' instances get. Both paths are UTF-8.
#[derive(Debug)]
pub struct Root {
    dir: PathBuf,
    exe: PathBuf,
}

impl Root {
    /// Creates the directory for this process, readable by its owner
    /// only, under the temporary directory. One left behind by an earlier
    /// process with the same id is replaced, and those of this user's
    /// processes that are gone are removed.
    pub fn create() -> Result<Self> {
        let exe = std::env::current_exe().context("find the running executable")?;
        Self::create_in(&std::env::temp_dir(), exe)
    }

    /// Like [`Root::create`], under `temporary`, with `exe` as the
    /// executable the sessions' shims run.
    pub(crate) fn create_in(temporary: &Path, exe: PathBuf) -> Result<Self> {
        if exe.to_str().is_none() {
            bail!("the executable's path {} is not UTF-8", exe.display());
        }
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
        make_private(&dir)?;
        if let Ok(own) = fs::metadata(&dir) {
            sweep(temporary, own.uid());
        }
        Ok(Self { dir, exe })
    }

    /// The process's instance directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Creates the directory of session `n` in this one, readable by its
    /// owner only, in place of anything of that name.
    pub fn session(&self, n: u32) -> Result<Instance> {
        let dir = self.dir.join(n.to_string());
        make_private(&dir)?;
        Ok(Instance {
            dir,
            exe: self.exe.clone(),
        })
    }
}

impl Drop for Root {
    /// Removes the directory and every session's in it.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Makes `dir` a new directory readable by its owner only. What had its
/// name goes first: a directory with what is in it, anything else, a
/// link included, as the entry alone.
fn make_private(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(found) if found.is_dir() => fs::remove_dir_all(dir),
        Ok(_) => fs::remove_file(dir),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    .with_context(|| format!("replace {}", dir.display()))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

/// A session's instance: its directory, removed with the value, and the
/// app's own executable, which the agent's shims are. Both paths are
/// UTF-8, so they can go into a command line as they are.
#[derive(Debug)]
pub struct Instance {
    dir: PathBuf,
    exe: PathBuf,
}

impl Instance {
    /// The instance directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The running executable.
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Removes what a launch left: its status and its events, not what
    /// the agent prepared, which it prepares again for the next launch.
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

    /// Whether `dir` is a directory, not a link to one, that only its
    /// owner can read.
    fn private(dir: &Path) -> bool {
        let found = fs::symlink_metadata(dir).expect("stat");
        found.is_dir() && found.mode() & 0o777 == 0o700
    }

    #[test]
    fn creates_the_process_directory_under_the_temporary_one() {
        // A dangling link where the directory goes is replaced.
        let name = format!("{PREFIX}{}", std::process::id());
        let planted = std::env::temp_dir().join(&name);
        let _ = fs::remove_dir_all(&planted);
        let _ = fs::remove_file(&planted);
        symlink("/nonexistent/termis-target", &planted).expect("plant a link");

        let root = Root::create().expect("creates");
        let dir = root.dir().to_path_buf();
        assert!(private(&dir));
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some(name.as_str())
        );
        let instance = root.session(0).expect("a session");
        assert!(instance.exe().is_file());
        drop(root);
        assert!(!dir.exists(), "removed with the root");
        drop(instance);
    }

    #[test]
    fn each_session_has_a_directory_of_its_own() {
        let temporary = TempDir::new().expect("temp dir");
        let exe = PathBuf::from("/bin/termis");
        let root = Root::create_in(temporary.path(), exe.clone()).expect("creates");
        let one = root.session(0).expect("a session");
        let two = root.session(1).expect("another");
        assert_eq!(one.dir(), root.dir().join("0"));
        assert_eq!(two.dir(), root.dir().join("1"));
        assert!(private(one.dir()) && private(two.dir()));
        assert_eq!((one.exe(), two.exe()), (exe.as_path(), exe.as_path()));

        let dir = one.dir().to_path_buf();
        fs::write(dir.join("status.json"), "{}").expect("write");
        fs::write(dir.join("events.jsonl"), "{}\n").expect("write");
        fs::write(dir.join("statusline.prev"), "cmd").expect("write");
        fs::write(two.dir().join("status.json"), "{}").expect("write");
        one.clear();
        assert!(!dir.join("status.json").exists() && !dir.join("events.jsonl").exists());
        assert!(
            dir.join("statusline.prev").exists(),
            "kept for the next launch"
        );
        assert!(two.dir().join("status.json").exists(), "the other's stays");

        drop(one);
        assert!(!dir.exists(), "removed with its session");
        assert!(two.dir().join("status.json").exists(), "the other stays");
        let left = two.dir().to_path_buf();
        let top = root.dir().to_path_buf();
        drop(root);
        assert!(!top.exists() && !left.exists(), "removed with the root");
        drop(two);
    }

    #[test]
    fn a_dangling_link_is_replaced_for_the_root_and_a_session() {
        let temporary = TempDir::new().expect("temp dir");
        let planted = temporary
            .path()
            .join(format!("{PREFIX}{}", std::process::id()));
        symlink("/nonexistent/termis-target", &planted).expect("plant a link");
        let root = Root::create_in(temporary.path(), PathBuf::from("/x")).expect("creates");
        assert_eq!(root.dir(), planted);
        assert!(private(root.dir()));

        symlink("/nonexistent/termis-target", root.dir().join("2")).expect("plant a link");
        let stale = root.dir().join("3");
        fs::create_dir(&stale).expect("mkdir");
        fs::write(stale.join("status.json"), "{}").expect("write");
        let linked = root.session(2).expect("replaces the link");
        assert!(private(linked.dir()));
        let replaced = root.session(3).expect("replaces the directory");
        assert!(private(replaced.dir()));
        assert!(!stale.join("status.json").exists(), "replaced");

        let error = Root::create_in(Path::new("relative"), PathBuf::from("/x"))
            .expect_err("a relative temporary directory");
        assert!(format!("{error:#}").contains("not an absolute path"));
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
        // Each with a session's directory and its files inside.
        for dir in [&dead_dir, &alive_dir] {
            fs::create_dir(dir.join("0")).expect("mkdir");
            fs::write(dir.join("0/events.jsonl"), "{}\n").expect("write");
        }

        sweep(temporary.path(), uid + 1);
        assert!(dead_dir.exists(), "another user's uid: left alone");
        sweep(temporary.path(), uid);
        assert!(!dead_dir.exists(), "a dead process's directory goes whole");
        assert!(
            alive_dir.join("0/events.jsonl").exists(),
            "a live one's stays"
        );
        assert!(lookalike.exists() && file.exists());
        sweep(&temporary.path().join("missing"), uid);
    }
}
