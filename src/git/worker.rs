//! The git worker: runs git off the main thread, one job at a time, and
//! reports back over the app's channel (ADR-0002).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};

use super::numstat::{self, Lines};
use super::status::{self, Change, Status};

/// Untracked files counted anew in one refresh at most; each count runs
/// a git process. The rest get their count on a later refresh.
const UNTRACKED_BUDGET: usize = 50;

/// Something for the worker to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Job {
    /// Read the status and the line counts again.
    Refresh,
}

/// What the worker reports.
#[derive(Debug)]
pub enum Event {
    /// A refresh finished, with what it read or why it failed.
    Refreshed(Result<Snapshot>),
}

/// The repository as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub status: Status,
    /// Lines per path with something staged.
    pub index: HashMap<PathBuf, Lines>,
    /// Lines per path changed in the work tree; untracked files included
    /// once counted.
    pub worktree: HashMap<PathBuf, Lines>,
}

/// A handle to the worker thread. Dropping it ends the thread after its
/// current job; a git command still running finishes on its own.
pub struct Worker {
    jobs: Sender<Job>,
}

impl Worker {
    /// Starts the worker for the repository at `root`; its events reach
    /// `tx`.
    pub fn spawn<E>(root: PathBuf, tx: Sender<E>) -> Result<Self>
    where
        E: From<Event> + Send + 'static,
    {
        let (jobs, inbox) = mpsc::channel();
        thread::Builder::new()
            .name("git-worker".into())
            .spawn(move || serve(&root, &inbox, &tx))
            .context("spawn the git worker")?;
        Ok(Self { jobs })
    }

    /// Queues `job`. Refreshes queued together run once. Nothing happens
    /// once the worker is gone.
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Runs jobs until the app drops the handle or stops listening.
fn serve<E: From<Event>>(root: &Path, inbox: &Receiver<Job>, tx: &Sender<E>) {
    let mut untracked = UntrackedCounts::default();
    while let Some(job) = next_job(inbox) {
        let event = match job {
            Job::Refresh => Event::Refreshed(refresh(root, &mut untracked)),
        };
        if tx.send(event.into()).is_err() {
            break;
        }
    }
}

/// Waits for the next job, folding the refreshes queued behind it into
/// it; none once the handle is gone.
fn next_job(inbox: &Receiver<Job>) -> Option<Job> {
    let job = inbox.recv().ok()?;
    while let Ok(Job::Refresh) = inbox.try_recv() {}
    Some(job)
}

/// Reads the status and the line counts of the repository at `root`.
/// The reads are separate commands; when the status changed under them,
/// they are done once more.
fn refresh(root: &Path, untracked: &mut UntrackedCounts) -> Result<Snapshot> {
    let mut status = status::read(root)?;
    loop {
        let snapshot = read_counts(root, untracked, status)?;
        let again = status::read(root)?;
        if again == snapshot.status {
            return Ok(snapshot);
        }
        status = again;
    }
}

/// The line counts that go with `status`.
fn read_counts(root: &Path, untracked: &mut UntrackedCounts, status: Status) -> Result<Snapshot> {
    let index = numstat::index(root)?
        .into_iter()
        .map(|stat| (stat.path, stat.lines))
        .collect();
    let mut worktree: HashMap<PathBuf, Lines> = numstat::worktree(root)?
        .into_iter()
        .map(|stat| (stat.path, stat.lines))
        .collect();
    let paths = status
        .entries
        .iter()
        .filter(|entry| entry.worktree == Some(Change::Untracked))
        .map(|entry| entry.path.as_path());
    worktree.extend(untracked.counts(root, paths, UNTRACKED_BUDGET, |path| {
        numstat::untracked(root, path)
    }));
    Ok(Snapshot {
        status,
        index,
        worktree,
    })
}

/// What identifies a file's content without reading it: size, inode and
/// the inode change time, which every write and rename-over bumps and
/// no user tool sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Signature {
    len: u64,
    ino: u64,
    ctime: (i64, i64),
}

impl Signature {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            ino: metadata.ino(),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

/// Line counts of untracked files, kept between refreshes and counted
/// again only when a file's signature changed.
#[derive(Default)]
struct UntrackedCounts {
    known: HashMap<PathBuf, (Signature, Lines)>,
}

impl UntrackedCounts {
    /// The counts of `paths`, relative to `root`, counting at most
    /// `budget` of them with `count`. A path gets no entry when it is a
    /// directory, cannot be read or counted, or is over the budget; only
    /// the paths with an entry stay known.
    fn counts<'a>(
        &mut self,
        root: &Path,
        paths: impl Iterator<Item = &'a Path>,
        mut budget: usize,
        mut count: impl FnMut(&Path) -> Result<Lines>,
    ) -> HashMap<PathBuf, Lines> {
        let mut counted = HashMap::new();
        for path in paths {
            let Ok(metadata) = std::fs::symlink_metadata(root.join(path)) else {
                continue;
            };
            if metadata.is_dir() {
                continue;
            }
            let signature = Signature::of(&metadata);
            let lines = match self.known.get(path) {
                Some((known, lines)) if *known == signature => *lines,
                _ if budget == 0 => continue,
                _ => {
                    budget -= 1;
                    let Ok(lines) = count(path) else {
                        continue;
                    };
                    self.known.insert(path.to_path_buf(), (signature, lines));
                    lines
                }
            };
            counted.insert(path.to_path_buf(), lines);
        }
        self.known.retain(|path, _| counted.contains_key(path));
        counted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, init};
    use std::cell::Cell;
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    const ONE: Lines = Lines::Text {
        added: 1,
        deleted: 0,
    };

    #[test]
    fn queued_refreshes_fold_into_one_job() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            tx.send(Job::Refresh).expect("queued");
        }
        assert_eq!(next_job(&rx), Some(Job::Refresh));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        drop(tx);
        assert_eq!(next_job(&rx), None, "ends with the handle");
    }

    #[test]
    fn untracked_counts_are_cached_by_signature_and_bounded() {
        let dir = TempDir::new().expect("temp dir");
        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(name), "1\n").expect("write");
        }
        std::fs::create_dir(dir.path().join("d")).expect("mkdir");
        let paths = ["a", "b", "c", "d", "missing"].map(Path::new);
        let calls = Cell::new(0);
        let count = |path: &Path| {
            calls.set(calls.get() + 1);
            if path == Path::new("c") {
                anyhow::bail!("cannot count c");
            }
            Ok(ONE)
        };
        let mut cache = UntrackedCounts::default();

        let counted = cache.counts(dir.path(), paths.into_iter(), 2, count);
        assert_eq!(calls.get(), 2, "budget of two");
        assert_eq!(counted.len(), 2);
        assert!(counted.contains_key(Path::new("a")) && counted.contains_key(Path::new("b")));

        let counted = cache.counts(dir.path(), paths.into_iter(), 10, count);
        assert_eq!(calls.get(), 3, "only c is counted, and it fails");
        assert_eq!(counted.len(), 2, "c has no entry, d and missing neither");

        // A rewrite of the same size still changes the signature.
        let a = dir.path().join("a");
        std::fs::write(&a, "2\n").expect("write");
        let counted = cache.counts(dir.path(), paths.into_iter(), 10, count);
        assert_eq!(calls.get(), 5, "a changed, c fails again");
        assert_eq!(counted.len(), 2);

        // A changed file over the budget keeps no stale count.
        std::fs::write(&a, "3\n4\n").expect("write");
        let counted = cache.counts(dir.path(), [Path::new("a")].into_iter(), 0, count);
        assert_eq!(calls.get(), 5);
        assert!(counted.is_empty() && cache.known.is_empty());

        // A file turned directory is no longer counted.
        std::fs::write(dir.path().join("b"), "1\n").expect("write");
        let counted = cache.counts(dir.path(), [Path::new("b")].into_iter(), 10, count);
        assert_eq!(counted.len(), 1);
        std::fs::remove_file(dir.path().join("b")).expect("rm");
        std::fs::create_dir(dir.path().join("b")).expect("mkdir");
        let counted = cache.counts(dir.path(), [Path::new("b")].into_iter(), 10, count);
        assert!(counted.is_empty() && cache.known.is_empty());
    }

    #[test]
    fn signature_follows_the_inode_not_the_mtime() {
        let dir = TempDir::new().expect("temp dir");
        let file = dir.path().join("f");
        std::fs::write(&file, "x\n").expect("write");
        let before = Signature::of(&std::fs::symlink_metadata(&file).expect("stat"));
        let moment = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open")
            .set_modified(moment)
            .expect("set mtime");
        let after = Signature::of(&std::fs::symlink_metadata(&file).expect("stat"));
        assert_ne!(before, after, "touching the inode changes the signature");
        assert_eq!(after.len, 2);
    }

    #[test]
    fn worker_refreshes_a_repository_and_reports_failures() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").expect("write");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker = Worker::spawn(dir.path().to_path_buf(), tx).expect("spawns");
        worker.send(Job::Refresh);
        let Event::Refreshed(result) = rx.recv_timeout(Duration::from_secs(10)).expect("event");
        let snapshot = result.expect("reads");
        assert_eq!(snapshot.status.entries.len(), 1);
        assert_eq!(
            snapshot.worktree.get(Path::new("f.txt")),
            Some(&Lines::Text {
                added: 2,
                deleted: 0
            }),
            "untracked files are counted"
        );
        assert!(snapshot.index.is_empty());

        git(dir.path(), &["add", "f.txt"]);
        worker.send(Job::Refresh);
        let Event::Refreshed(result) = rx.recv_timeout(Duration::from_secs(10)).expect("event");
        let snapshot = result.expect("reads");
        assert_eq!(
            snapshot.index.get(Path::new("f.txt")),
            Some(&Lines::Text {
                added: 2,
                deleted: 0
            })
        );
        assert!(snapshot.worktree.is_empty());
        drop(worker);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect_err("no event"),
            RecvTimeoutError::Disconnected,
            "the thread ends with the handle"
        );

        let outside = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker = Worker::spawn(outside.path().to_path_buf(), tx).expect("spawns");
        worker.send(Job::Refresh);
        let Event::Refreshed(result) = rx.recv_timeout(Duration::from_secs(10)).expect("event");
        let error = result.expect_err("not a repository");
        assert!(
            format!("{error:#}").contains("not a git repository"),
            "{error:#}"
        );
    }
}
