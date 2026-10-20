//! The git worker: runs git off the main thread, one job at a time, and
//! reports back over the app's channel (ADR-0002).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::last::{self, Last};
use super::numstat::{self, Lines};
use super::status::{self, Change, Status};
use super::today::{self, Today};
use super::{commit, diff, stage};

/// Untracked files counted anew in one refresh at most; each count runs
/// a git process. The rest get their count on a later refresh.
const UNTRACKED_BUDGET: usize = 50;
/// How long a commit gets, hooks included, before it is killed.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Something for the worker to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Job {
    /// Read the status, the line counts, today's totals and the last
    /// commit again.
    Refresh,
    /// Read the status alone; a refresh in the same batch answers it.
    Status,
    /// Change the repository, then read it again.
    Write(Write),
    /// Read the diff of a path; only the last one queued is read.
    Diff(diff::Request),
}

/// A change to the repository.
#[derive(Debug, PartialEq, Eq)]
pub enum Write {
    /// Stage these paths, as status prints them.
    Stage(Vec<PathBuf>),
    /// Take these paths out of the index; both paths of a rename.
    Unstage(Vec<PathBuf>),
    /// Stage everything.
    StageAll,
    /// Commit what is staged with this message, as written.
    Commit(String),
}

/// What a write that worked has to report.
enum Outcome {
    /// Nothing beyond the refresh that follows.
    Changed,
    /// A commit, with its short hash.
    Committed(String),
}

impl Write {
    fn run(&self, root: &Path) -> Result<Outcome> {
        match self {
            Self::Stage(paths) => stage::stage(root, paths)?,
            Self::Unstage(paths) => stage::unstage(root, paths)?,
            Self::StageAll => stage::stage_all(root)?,
            Self::Commit(message) => {
                let hash = commit::commit(root, message, COMMIT_TIMEOUT)?;
                return Ok(Outcome::Committed(hash));
            }
        }
        Ok(Outcome::Changed)
    }
}

/// What the worker reports.
#[derive(Debug)]
pub enum Event {
    /// A refresh finished, with what it read or why it failed. Boxed:
    /// the snapshot is many times the size of the other events.
    Refreshed(Result<Box<Snapshot>>),
    /// A status read alone finished, with what it read or why it failed.
    Status(Result<Status>),
    /// A write failed; the refresh that follows it still comes.
    Failed { write: Write, error: anyhow::Error },
    /// A commit was made, with this short hash.
    Committed(String),
    /// Git's answer to a diff request, or why it could not be read.
    Diff {
        request: diff::Request,
        result: Result<String>,
    },
}

/// The repository as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub status: Status,
    /// Lines per path with something staged.
    pub index: HashMap<PathBuf, Lines>,
    /// Lines per path changed in the work tree; untracked files included
    /// once counted.
    pub worktree: HashMap<PathBuf, Lines>,
    /// What was committed since midnight, or why it could not be read;
    /// the lists stand without it.
    pub today: Result<Today, String>,
    /// The commit `HEAD` points to, none before the first commit, or why
    /// it could not be read; the lists stand without it.
    pub last: Result<Option<Last>, String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            status: Status::default(),
            index: HashMap::new(),
            worktree: HashMap::new(),
            today: Ok(Today::default()),
            last: Ok(None),
        }
    }
}

/// A handle to the worker thread. Dropping it ends the thread after its
/// current job; a git command still running finishes on its own.
pub struct Worker {
    jobs: Sender<Job>,
}

impl Worker {
    /// Starts the worker for the repository at `root`; its events reach
    /// `tx`, wrapped by `tag` into the owner's message type.
    pub fn spawn<E>(
        root: PathBuf,
        tx: Sender<E>,
        tag: impl Fn(Event) -> E + Send + 'static,
    ) -> Result<Self>
    where
        E: Send + 'static,
    {
        let (jobs, inbox) = mpsc::channel();
        thread::Builder::new()
            .name("git-worker".into())
            .spawn(move || serve(&root, &inbox, &tx, tag))
            .context("spawn the git worker")?;
        Ok(Self { jobs })
    }

    /// Queues `job`. Jobs queued together run as one batch: the writes in
    /// order, one refresh when any of them asked for one or wrote, else
    /// the status when one asked for it, then the last diff asked for.
    /// Nothing happens once the worker is gone.
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Runs jobs until the app drops the handle or stops listening; each
/// event goes to `tx` wrapped by `tag`.
fn serve<E>(root: &Path, inbox: &Receiver<Job>, tx: &Sender<E>, tag: impl Fn(Event) -> E) {
    let mut untracked = UntrackedCounts::default();
    let mut last = LastCommit::default();
    while let Some(batch) = next_batch(inbox) {
        let plan = plan(batch);
        for write in plan.writes {
            let event = match write.run(root) {
                Ok(Outcome::Committed(hash)) => Event::Committed(hash),
                Ok(Outcome::Changed) => continue,
                Err(error) => Event::Failed { write, error },
            };
            if tx.send(tag(event)).is_err() {
                return;
            }
        }
        let read = if plan.refresh {
            let snapshot = refresh(root, &mut untracked, &mut last);
            Some(Event::Refreshed(snapshot.map(Box::new)))
        } else if plan.status {
            Some(Event::Status(status::read(root)))
        } else {
            None
        };
        if let Some(event) = read
            && tx.send(tag(event)).is_err()
        {
            return;
        }
        if let Some(request) = plan.diff {
            let result = diff::read(root, &request);
            if tx.send(tag(Event::Diff { request, result })).is_err() {
                return;
            }
        }
    }
}

/// Waits for the next job and takes the ones queued behind it; none once
/// the handle is gone.
fn next_batch(inbox: &Receiver<Job>) -> Option<Vec<Job>> {
    let mut batch = vec![inbox.recv().ok()?];
    while let Ok(job) = inbox.try_recv() {
        batch.push(job);
    }
    Some(batch)
}

/// What a batch of jobs comes down to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// The writes, in order.
    writes: Vec<Write>,
    /// Whether to read the repository afterwards: asked for, or needed
    /// after a write.
    refresh: bool,
    /// Whether to read the status alone afterwards: asked for, with no
    /// refresh to answer it.
    status: bool,
    /// The last diff asked for.
    diff: Option<diff::Request>,
}

/// Folds a batch: every write in order, one refresh when any job asked
/// for one or wrote, else the status when a job asked for it, and only
/// the last diff.
fn plan(batch: Vec<Job>) -> Plan {
    let mut plan = Plan::default();
    for job in batch {
        match job {
            Job::Refresh => plan.refresh = true,
            Job::Status => plan.status = true,
            Job::Write(write) => {
                plan.writes.push(write);
                plan.refresh = true;
            }
            Job::Diff(request) => plan.diff = Some(request),
        }
    }
    // The refresh reads the status too.
    plan.status &= !plan.refresh;
    plan
}

/// Reads the status, the line counts, today's totals and the last
/// commit of the repository at `root`. The reads are separate commands;
/// when the status changed under them, they are done once more.
fn refresh(
    root: &Path,
    untracked: &mut UntrackedCounts,
    last: &mut LastCommit,
) -> Result<Snapshot> {
    let mut status = status::read(root)?;
    loop {
        let snapshot = read_counts(root, untracked, last, status)?;
        let again = status::read(root)?;
        if again == snapshot.status {
            return Ok(snapshot);
        }
        status = again;
    }
}

/// The line counts, today's totals and the last commit that go with
/// `status`.
fn read_counts(
    root: &Path,
    untracked: &mut UntrackedCounts,
    last: &mut LastCommit,
    status: Status,
) -> Result<Snapshot> {
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
    // Before the first commit there is nothing to count or show, and git
    // log would fail.
    let (today, last) = match &status.branch.oid {
        Some(oid) => (
            today::read(root).map_err(|error| format!("{error:#}")),
            last.at(oid, || last::read(root))
                .map(Some)
                .map_err(|error| format!("{error:#}")),
        ),
        None => (Ok(Today::default()), Ok(None)),
    };
    Ok(Snapshot {
        status,
        index,
        worktree,
        today,
        last,
    })
}

/// The last commit as read, kept between refreshes and read again only
/// once `HEAD` moved: the time and the subject of a commit do not
/// change, and its short hash only grows with the repository, which the
/// next move of `HEAD` picks up.
#[derive(Default)]
struct LastCommit {
    known: Option<Last>,
}

impl LastCommit {
    /// The commit `oid`: the last read when it was of `oid`, else what
    /// `read` returns, kept under the hash git printed. A read of another
    /// commit, `HEAD` having moved since the status was read, is kept too
    /// but fails this time, so that the next refresh reads again.
    fn at(&mut self, oid: &str, read: impl FnOnce() -> Result<Last>) -> Result<Last> {
        if let Some(known) = &self.known
            && known.oid == oid
        {
            return Ok(known.clone());
        }
        let last = read()?;
        self.known = Some(last.clone());
        if last.oid != oid {
            bail!("HEAD moved while its commit was read");
        }
        Ok(last)
    }
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
    use std::time::SystemTime;
    use tempfile::TempDir;

    const ONE: Lines = Lines::Text {
        added: 1,
        deleted: 0,
    };

    #[test]
    fn a_batch_takes_everything_queued() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            tx.send(Job::Refresh).expect("queued");
        }
        tx.send(Job::Write(Write::StageAll)).expect("queued");
        assert_eq!(
            next_batch(&rx),
            Some(vec![
                Job::Refresh,
                Job::Refresh,
                Job::Refresh,
                Job::Write(Write::StageAll)
            ])
        );
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        drop(tx);
        assert_eq!(next_batch(&rx), None, "ends with the handle");
    }

    #[test]
    fn a_batch_folds_into_writes_one_refresh_and_the_last_diff() {
        let stage = || Write::Stage(vec![PathBuf::from("a")]);
        let request = |name: &str| diff::Request {
            paths: vec![PathBuf::from(name)],
            staged: false,
            untracked: false,
        };
        assert_eq!(
            plan(vec![Job::Refresh, Job::Refresh]),
            Plan {
                writes: vec![],
                refresh: true,
                status: false,
                diff: None,
            }
        );
        assert_eq!(
            plan(vec![
                Job::Diff(request("a")),
                Job::Write(stage()),
                Job::Diff(request("b")),
                Job::Write(Write::StageAll),
            ]),
            Plan {
                writes: vec![stage(), Write::StageAll],
                refresh: true,
                status: false,
                diff: Some(request("b")),
            }
        );
        assert_eq!(
            plan(vec![Job::Diff(request("a"))]),
            Plan {
                writes: vec![],
                refresh: false,
                status: false,
                diff: Some(request("a")),
            },
            "a diff alone reads nothing else"
        );
    }

    #[test]
    fn a_status_alone_reads_only_the_status_and_a_refresh_answers_it() {
        let request = diff::Request {
            paths: vec![PathBuf::from("a")],
            staged: false,
            untracked: false,
        };
        assert_eq!(
            plan(vec![Job::Status, Job::Status]),
            Plan {
                writes: vec![],
                refresh: false,
                status: true,
                diff: None,
            }
        );
        assert_eq!(
            plan(vec![Job::Status, Job::Diff(request.clone())]),
            Plan {
                writes: vec![],
                refresh: false,
                status: true,
                diff: Some(request),
            },
            "the diff still comes after it"
        );
        for batch in [
            vec![Job::Status, Job::Refresh],
            vec![Job::Refresh, Job::Status],
        ] {
            assert_eq!(
                plan(batch),
                Plan {
                    writes: vec![],
                    refresh: true,
                    status: false,
                    diff: None,
                },
                "the refresh answers both"
            );
        }
        assert_eq!(
            plan(vec![Job::Status, Job::Write(Write::StageAll)]),
            Plan {
                writes: vec![Write::StageAll],
                refresh: true,
                status: false,
                diff: None,
            },
            "the refresh after the write answers it"
        );
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
    fn the_last_commit_is_read_once_per_head() {
        let calls = Cell::new(0);
        let read = |oid: &'static str, subject: &'static str| {
            let calls = &calls;
            move || {
                calls.set(calls.get() + 1);
                Ok(Last {
                    oid: oid.to_owned(),
                    hash: oid[..3].to_owned(),
                    time: 1,
                    subject: subject.into(),
                })
            }
        };
        let mut cache = LastCommit::default();
        let first = cache.at("aaaa", read("aaaa", "a")).expect("reads");
        assert_eq!(first.subject, "a");
        assert_eq!(
            cache.at("aaaa", read("aaaa", "x")).expect("cached").subject,
            "a"
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            cache.at("bbbb", read("bbbb", "b")).expect("reads").subject,
            "b"
        );
        assert_eq!(calls.get(), 2);
        assert!(cache.at("cccc", || anyhow::bail!("no")).is_err());
        assert_eq!(
            cache
                .at("bbbb", read("bbbb", "x"))
                .expect("still known")
                .subject,
            "b",
            "a failed read keeps the last one known"
        );
        // HEAD moved between the status and the log: the read is kept
        // under its own hash and this refresh gets an error.
        let error = cache
            .at("aaaa", read("dddd", "d"))
            .expect_err("another commit than asked for");
        assert!(error.to_string().contains("moved"), "{error}");
        assert_eq!(
            cache.at("dddd", read("dddd", "x")).expect("kept").subject,
            "d"
        );
        assert_eq!(calls.get(), 3, "the failed read ran no closure of ours");
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

    /// The next event, which must be a refresh.
    fn refreshed(rx: &mpsc::Receiver<Event>) -> Result<Snapshot> {
        match rx.recv_timeout(Duration::from_secs(10)).expect("event") {
            Event::Refreshed(result) => result.map(|snapshot| *snapshot),
            Event::Status(_) => panic!("unexpected status alone"),
            Event::Failed { error, .. } => panic!("unexpected failure: {error:#}"),
            Event::Committed(hash) => panic!("unexpected commit {hash}"),
            Event::Diff { .. } => panic!("unexpected diff"),
        }
    }

    #[test]
    fn worker_refreshes_a_repository_and_reports_failures() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").expect("write");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker = Worker::spawn(dir.path().to_path_buf(), tx, |event| event).expect("spawns");
        worker.send(Job::Refresh);
        let snapshot = refreshed(&rx).expect("reads");
        assert_eq!(snapshot.status.entries.len(), 1);
        assert_eq!(snapshot.last, Ok(None), "no commit yet");
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
        let snapshot = refreshed(&rx).expect("reads");
        assert_eq!(
            snapshot.index.get(Path::new("f.txt")),
            Some(&Lines::Text {
                added: 2,
                deleted: 0
            })
        );
        assert!(snapshot.worktree.is_empty());

        // A write that fails is reported, then the refresh follows; a
        // write that works shows in the refresh.
        worker.send(Job::Write(Write::Stage(vec![PathBuf::from("nope.txt")])));
        let Event::Failed { write, error } =
            rx.recv_timeout(Duration::from_secs(10)).expect("event")
        else {
            panic!("the stage of a missing path fails");
        };
        assert_eq!(write, Write::Stage(vec![PathBuf::from("nope.txt")]));
        assert!(format!("{error:#}").contains("pathspec"), "{error:#}");
        assert!(refreshed(&rx).is_ok(), "the refresh after the failure");
        worker.send(Job::Diff(diff::Request {
            paths: vec![PathBuf::from("f.txt")],
            staged: true,
            untracked: false,
        }));
        let Event::Diff { request, result } =
            rx.recv_timeout(Duration::from_secs(10)).expect("event")
        else {
            panic!("the diff is reported, with no refresh before it");
        };
        assert_eq!(request.paths, [PathBuf::from("f.txt")]);
        assert!(result.expect("reads").contains("+a\n+b"), "the staged file");
        worker.send(Job::Write(Write::Commit("feat: f\n".into())));
        let Event::Committed(hash) = rx.recv_timeout(Duration::from_secs(10)).expect("event")
        else {
            panic!("the commit is reported");
        };
        assert!(hash.len() >= 7, "{hash}");
        let snapshot = refreshed(&rx).expect("reads");
        assert!(snapshot.status.entries.is_empty(), "committed");
        assert_eq!(
            snapshot
                .status
                .branch
                .oid
                .as_deref()
                .map(|oid| &oid[..hash.len()]),
            Some(hash.as_str())
        );
        let last = snapshot
            .last
            .clone()
            .expect("read")
            .expect("the commit HEAD points to");
        assert_eq!(last.hash, hash);
        assert_eq!(last.subject, "feat: f");
        // A refresh with HEAD unmoved keeps it; a new commit replaces it.
        worker.send(Job::Refresh);
        assert_eq!(refreshed(&rx).expect("reads").last, Ok(Some(last.clone())));
        git(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "chore: two"],
        );
        worker.send(Job::Refresh);
        let next = refreshed(&rx)
            .expect("reads")
            .last
            .expect("read")
            .expect("a commit");
        assert_ne!(next.oid, last.oid);
        assert_eq!(next.subject, "chore: two");
        drop(worker);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect_err("no event"),
            RecvTimeoutError::Disconnected,
            "the thread ends with the handle"
        );

        let outside = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker =
            Worker::spawn(outside.path().to_path_buf(), tx, |event| event).expect("spawns");
        worker.send(Job::Refresh);
        let error = refreshed(&rx).expect_err("not a repository");
        assert!(
            format!("{error:#}").contains("not a git repository"),
            "{error:#}"
        );
    }

    #[test]
    fn the_owner_tags_the_workers_events() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let (tx, rx) = mpsc::channel::<(&str, Event)>();
        let worker =
            Worker::spawn(dir.path().to_path_buf(), tx, |event| ("repo", event)).expect("spawns");
        worker.send(Job::Refresh);
        let (tag, event) = rx.recv_timeout(Duration::from_secs(10)).expect("event");
        assert_eq!(tag, "repo");
        let Event::Refreshed(result) = event else {
            panic!("a refresh is reported");
        };
        assert!(result.expect("reads").status.entries.is_empty());
    }

    #[test]
    fn worker_reads_the_status_alone_on_request() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("f.txt"), "a\n").expect("write");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker = Worker::spawn(dir.path().to_path_buf(), tx, |event| event).expect("spawns");
        worker.send(Job::Status);
        let Event::Status(result) = rx.recv_timeout(Duration::from_secs(10)).expect("event") else {
            panic!("the status alone is reported");
        };
        let status = result.expect("reads");
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        let paths: Vec<_> = status
            .entries
            .iter()
            .map(|entry| (entry.path.as_path(), entry.worktree))
            .collect();
        assert_eq!(paths, [(Path::new("f.txt"), Some(Change::Untracked))]);
        drop(worker);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect_err("nothing else"),
            RecvTimeoutError::Disconnected,
            "no refresh after it"
        );

        let outside = TempDir::new().expect("temp dir");
        let (tx, rx) = mpsc::channel::<Event>();
        let worker =
            Worker::spawn(outside.path().to_path_buf(), tx, |event| event).expect("spawns");
        worker.send(Job::Status);
        let Event::Status(result) = rx.recv_timeout(Duration::from_secs(10)).expect("event") else {
            panic!("the status alone is reported");
        };
        let error = result.expect_err("not a repository");
        assert!(
            format!("{error:#}").contains("not a git repository"),
            "{error:#}"
        );
    }
}
