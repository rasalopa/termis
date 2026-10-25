//! Repositories: the git worker of each one in use, and what the main
//! loop keeps of it for the changes pane.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{Msg, TOAST_LASTS};
use crate::git;
use crate::git::status::Change;
use crate::git::worker::{Event, Job, Snapshot, Worker, Write};
use crate::strings;
use crate::ui::changes::{self, Section, Selected, Step};
use crate::ui::commit::Draft;
use crate::ui::diff::Diff;
use crate::ui::toast::{self, Toast};

/// How often the changes pane is refreshed while it is on screen.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How often the status of a repository is read while only the list of
/// sessions shows it.
const STATUS_EVERY: Duration = Duration::from_secs(10);

/// A repository's id, from a counter that only grows: never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RepoId(u32);

/// A repository in use: its git worker with the poll that asks it to
/// read the repository, the snapshot it read last and whether it has
/// uncommitted changes, and the changes pane's selection, scroll, diff
/// view, commit draft and commit in flight.
pub(super) struct Repo {
    /// What the worker's events are tagged with.
    id: RepoId,
    /// The root of the repository's working tree.
    root: PathBuf,
    /// Runs git in the repository, off the main thread.
    worker: Worker,
    /// When to ask the worker to read the repository.
    poll: Poll,
    /// The repository as last refreshed, once the worker read it.
    pub(super) snapshot: Option<Snapshot>,
    /// Whether the index or the work tree differ from `HEAD`, untracked
    /// files included, as the last refresh or status read; false before
    /// the first.
    pub(super) uncommitted: bool,
    /// The row of the changes pane that the list keys act on.
    pub(super) selected: Option<Selected>,
    /// How far the changes pane, or the diff in it, was scrolled.
    pub(super) scroll: usize,
    /// The diff shown in the changes pane in place of the lists.
    pub(super) diff: Option<Diff>,
    /// The commit message being written.
    pub(super) draft: Draft,
    /// The draft of the commit running, brought back if it fails.
    pub(super) in_flight: Option<Draft>,
    /// The last refresh failure shown, so that it shows once.
    refresh_error: Option<String>,
}

/// What a git event changed for the main loop.
#[derive(Debug, Default)]
pub(super) struct Applied {
    /// Whether the screen needs a new frame.
    pub(super) dirty: bool,
    /// A notice to show.
    pub(super) toast: Option<Toast>,
}

impl Repo {
    /// Repository `id` at `root`, whose git runs on `worker`; nothing
    /// read yet.
    fn new(id: RepoId, root: PathBuf, worker: Worker) -> Self {
        Self {
            id,
            root,
            worker,
            poll: Poll::default(),
            snapshot: None,
            uncommitted: false,
            selected: None,
            scroll: 0,
            diff: None,
            draft: Draft::default(),
            in_flight: None,
            refresh_error: None,
        }
    }

    /// Asks the worker for the read due at `now` for what shows the
    /// repository, if one is.
    fn read_when_due(&mut self, now: Instant, shown_in: Shown) {
        if let Some(read) = self.poll.due(now, shown_in) {
            self.worker.send(match read {
                Read::Refresh => Job::Refresh,
                Read::Status => Job::Status,
            });
            self.poll.sent(read);
        }
    }

    /// Makes the next refresh due at once. A refresh in flight stands for
    /// it; a status in flight does not, so the refresh follows it.
    pub(super) fn refresh_soon(&mut self) {
        self.poll.refreshed = None;
    }

    /// Records a refresh sent and finished at `now`, for tests.
    #[cfg(test)]
    pub(super) fn refreshed(&mut self, now: Instant) {
        self.poll.sent(Read::Refresh);
        self.poll.done(Read::Refresh, now);
    }

    /// Whether a refresh is due at `now` with the changes pane showing
    /// the repository, for tests.
    #[cfg(test)]
    pub(super) fn refresh_due(&self, now: Instant) -> bool {
        self.poll.due(now, Shown::Changes) == Some(Read::Refresh)
    }

    /// Applies `event` from the worker, at `now`. A commit's end lets its
    /// draft go, or brings it back; the answer to the diff on screen
    /// fills it or closes it; a refresh that read something new replaces
    /// the snapshot, and the selection and the diff follow; a refresh or
    /// a status read tells whether there are uncommitted changes. What
    /// changed in the changes pane needs a frame only while `side_panes`
    /// are on screen; the list of sessions watches the rest.
    fn apply(&mut self, event: Event, side_panes: bool, now: Instant) -> Applied {
        let mut applied = Applied::default();
        if let Some((text, kind)) = notice(&event, &mut self.refresh_error) {
            applied.toast = Some(Toast::new(text, kind, now, TOAST_LASTS));
            applied.dirty = true;
        }
        match event {
            // The draft served; a failed commit brings it back for another
            // go, unless a new one was started meanwhile.
            Event::Committed(_) => self.in_flight = None,
            Event::Failed {
                write: Write::Commit(_),
                ..
            } => {
                if let Some(previous) = self.in_flight.take()
                    && self.draft.is_empty()
                {
                    self.draft = previous;
                }
            }
            Event::Failed { .. } => {}
            // An answer to the diff on screen; older ones are ignored.
            Event::Diff { request, result } => {
                if let Some(open) = &mut self.diff
                    && open.request == request
                {
                    match result {
                        Ok(text) => {
                            open.accept(&request, &text);
                            applied.dirty |= side_panes;
                        }
                        Err(error) => {
                            applied.toast = Some(Toast::new(
                                format!("{error:#}"),
                                toast::Kind::Error,
                                now,
                                TOAST_LASTS,
                            ));
                            self.close_diff();
                            applied.dirty = true;
                        }
                    }
                }
            }
            Event::Refreshed(result) => {
                self.poll.done(Read::Refresh, now);
                if let Ok(fresh) = &result {
                    self.uncommitted = !fresh.status.entries.is_empty();
                }
                // A failed refresh keeps the last snapshot; notice() reported it.
                if let Ok(fresh) = result
                    && self.snapshot.as_ref() != Some(&*fresh)
                {
                    self.selected =
                        changes::reselect(self.snapshot.as_ref(), &fresh, self.selected.as_ref());
                    // An open diff follows its row, or goes when the row went.
                    if self.diff.is_some() {
                        match diff_request(&fresh, self.selected.as_ref()) {
                            Some((request, path)) => {
                                self.diff = Some(Diff::new(request.clone(), path));
                                self.worker.send(Job::Diff(request));
                            }
                            None => self.close_diff(),
                        }
                    }
                    self.snapshot = Some(*fresh);
                    applied.dirty |= side_panes;
                }
            }
            // Read for the list of sessions: the snapshot stays. One that
            // could not be read leaves the dot as it was; the refresh once
            // the repository is shown says why.
            Event::Status(result) => {
                self.poll.done(Read::Status, now);
                if let Ok(status) = result {
                    self.uncommitted = !status.entries.is_empty();
                }
            }
            // Nothing asks for the files yet.
            Event::Files { .. } => {}
        }
        applied
    }

    /// Moves the selection a row, once there is a snapshot.
    pub(super) fn select(&mut self, step: Step) {
        if let Some(current) = &self.snapshot {
            self.selected = changes::moved(current, self.selected.as_ref(), step);
        }
    }

    /// Stages the selected row, when it is in the unstaged list.
    pub(super) fn stage(&self) {
        if let Some(current) = &self.snapshot
            && let Some(paths) = selected_paths(current, self.selected.as_ref(), false)
        {
            self.worker.send(Job::Write(Write::Stage(paths)));
        }
    }

    /// Takes the selected row out of the index, when it is in the staged
    /// list.
    pub(super) fn unstage(&self) {
        if let Some(current) = &self.snapshot
            && let Some(paths) = selected_paths(current, self.selected.as_ref(), true)
        {
            self.worker.send(Job::Write(Write::Unstage(paths)));
        }
    }

    /// Stages everything, when something is unstaged.
    pub(super) fn stage_all(&self) {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.status.unstaged().next().is_some())
        {
            self.worker.send(Job::Write(Write::StageAll));
        }
    }

    /// Closes the diff, or opens it on the selected row and asks the
    /// worker for it.
    pub(super) fn toggle_diff(&mut self) {
        self.scroll = 0;
        if self.diff.is_some() {
            self.diff = None;
        } else if let Some(current) = &self.snapshot
            && let Some((request, path)) = diff_request(current, self.selected.as_ref())
        {
            self.diff = Some(Diff::new(request.clone(), path));
            self.worker.send(Job::Diff(request));
        }
    }

    /// Closes the diff, and the pane's scroll starts over.
    pub(super) fn close_diff(&mut self) {
        self.diff = None;
        self.scroll = 0;
    }

    /// Commits what is staged with `message`, keeping the draft until the
    /// commit ends.
    pub(super) fn commit(&mut self, message: String) {
        self.worker.send(Job::Write(Write::Commit(message)));
        self.in_flight = Some(std::mem::take(&mut self.draft));
    }
}

/// The repositories in use, each with its own worker.
pub(super) struct Repos {
    list: Vec<Repo>,
    /// The id the next repository gets.
    next: u32,
}

impl Repos {
    /// No repositories yet.
    pub(super) fn new() -> Self {
        Self {
            list: Vec::new(),
            next: 0,
        }
    }

    /// The id of the repository at `root`. One not in use yet gets its
    /// worker started first, its events reaching `tx`.
    pub(super) fn get_or_spawn(&mut self, root: &Path, tx: &mpsc::Sender<Msg>) -> Result<RepoId> {
        if let Some(repo) = self.list.iter().find(|repo| repo.root == root) {
            return Ok(repo.id);
        }
        let id = RepoId(self.next);
        self.next += 1;
        let worker = Worker::spawn(root.to_path_buf(), tx.clone(), move |event| {
            Msg::Git(id, event)
        })?;
        self.list.push(Repo::new(id, root.to_path_buf(), worker));
        Ok(id)
    }

    /// Repository `id`, while there is one.
    pub(super) fn get(&self, id: RepoId) -> Option<&Repo> {
        self.list.iter().find(|repo| repo.id == id)
    }

    /// Repository `id`, while there is one, to change.
    pub(super) fn get_mut(&mut self, id: RepoId) -> Option<&mut Repo> {
        self.list.iter_mut().find(|repo| repo.id == id)
    }

    /// Lets go of each repository that no session uses, as `used` tells,
    /// and that has no commit in flight: its worker stops once done with
    /// what it was asked, and a late event of it reaches nothing. One
    /// with a commit in flight goes on a call once the commit ended.
    pub(super) fn release_unused(&mut self, used: impl Fn(RepoId) -> bool) {
        self.list
            .retain(|repo| kept(used(repo.id), repo.in_flight.is_some()));
    }

    /// Asks each worker for the read due at `now`: the repository of the
    /// session `shown` is refreshed while the `side_panes` show the
    /// changes pane; every other one has its status read while the
    /// `list` of sessions is on screen.
    pub(super) fn read_when_due(
        &mut self,
        now: Instant,
        shown: Option<RepoId>,
        side_panes: bool,
        list: bool,
    ) {
        for repo in &mut self.list {
            let shown_in = if shown == Some(repo.id) {
                if side_panes {
                    Shown::Changes
                } else {
                    Shown::Nowhere
                }
            } else if list {
                Shown::List
            } else {
                Shown::Nowhere
            };
            repo.read_when_due(now, shown_in);
        }
    }

    /// Applies `event` from repository `id`'s worker, as
    /// [`Repo::apply`] does; an event for an unknown repository changes
    /// nothing.
    pub(super) fn apply(
        &mut self,
        id: RepoId,
        event: Event,
        side_panes: bool,
        now: Instant,
    ) -> Applied {
        self.get_mut(id)
            .map_or_else(Applied::default, |repo| repo.apply(event, side_panes, now))
    }
}

/// Whether a repository stays in use: while a session uses it, or a
/// commit runs in it, whose end has to reach it.
fn kept(used: bool, committing: bool) -> bool {
    used || committing
}

/// What shows a repository, which decides how it is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// The changes pane: everything, every `POLL_EVERY`.
    Changes,
    /// Only the list of sessions, with its dot: the status alone, every
    /// `STATUS_EVERY`.
    List,
    /// Nothing on screen: no reads.
    Nowhere,
}

/// A read the poll asks the worker for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    /// Everything the changes pane shows, the status included.
    Refresh,
    /// The status alone.
    Status,
}

/// When to ask the worker to read the repository: as what shows it
/// needs, and never while a read is in flight. The intervals run from
/// the end of the last read, so a slow repository leaves gaps; a refresh
/// counts as a status read too. None read yet: the first is due at once.
#[derive(Debug, Default)]
struct Poll {
    /// When the last refresh finished; none before the first, or while
    /// the next is due at once.
    refreshed: Option<Instant>,
    /// When the last read of the status finished, alone or in a refresh.
    read: Option<Instant>,
    /// The read sent and not answered yet.
    in_flight: Option<Read>,
}

impl Poll {
    /// The read to send at `now` for what shows the repository, if one is
    /// due.
    fn due(&self, now: Instant, shown_in: Shown) -> Option<Read> {
        let passed = |last: Option<Instant>, every: Duration| {
            last.is_none_or(|last| now.duration_since(last) >= every)
        };
        match shown_in {
            _ if self.in_flight.is_some() => None,
            Shown::Changes if passed(self.refreshed, POLL_EVERY) => Some(Read::Refresh),
            Shown::List if passed(self.read, STATUS_EVERY) => Some(Read::Status),
            Shown::Changes | Shown::List | Shown::Nowhere => None,
        }
    }

    /// Records that `read` was sent.
    fn sent(&mut self, read: Read) {
        self.in_flight = Some(read);
    }

    /// Records that `read` finished at `now`. A refresh answers any read
    /// in flight, as the worker folds a status queued with it into it; a
    /// status answers only a status.
    fn done(&mut self, read: Read, now: Instant) {
        self.read = Some(now);
        match read {
            Read::Refresh => {
                self.refreshed = Some(now);
                self.in_flight = None;
            }
            Read::Status => {
                if self.in_flight == Some(Read::Status) {
                    self.in_flight = None;
                }
            }
        }
    }
}

/// The paths a stage or unstage acts on: the selected row's, when it is
/// in the list the action takes from (unstaged for a stage, staged for
/// an unstage), plus the source of a staged rename for an unstage, which
/// git needs to take both out of the index. None otherwise, or when the
/// path left the snapshot.
fn selected_paths(
    snapshot: &Snapshot,
    selected: Option<&Selected>,
    unstage: bool,
) -> Option<Vec<PathBuf>> {
    let selected = selected?;
    let from = if unstage {
        Section::Staged
    } else {
        Section::Unstaged
    };
    if selected.section != from {
        return None;
    }
    let entry = snapshot
        .status
        .entries
        .iter()
        .find(|entry| entry.path == selected.path)?;
    let mut paths = vec![entry.path.clone()];
    if unstage
        && entry.index == Some(Change::Renamed)
        && let Some(original) = &entry.original
    {
        paths.push(original.clone());
    }
    Some(paths)
}

/// The notice a git event deserves: the hash of a commit, the words of
/// a failed write (a failed commit says so, the others carry their
/// command), or a refresh failure the first time it reads so (a refresh
/// that works forgets it). None otherwise.
fn notice(event: &Event, refresh_error: &mut Option<String>) -> Option<(String, toast::Kind)> {
    match event {
        Event::Committed(hash) => Some((strings::toast_committed(hash), toast::Kind::Info)),
        Event::Failed {
            write: Write::Commit(_),
            error,
        } => Some((
            strings::commit_failed(&format!("{error:#}")),
            toast::Kind::Error,
        )),
        Event::Failed { error, .. } => Some((format!("{error:#}"), toast::Kind::Error)),
        Event::Refreshed(Ok(_)) => {
            *refresh_error = None;
            None
        }
        Event::Refreshed(Err(error)) => {
            let text = format!("{error:#}");
            if refresh_error.as_ref() == Some(&text) {
                return None;
            }
            *refresh_error = Some(text.clone());
            Some((text, toast::Kind::Error))
        }
        // Handled with the diff on screen, so that an old answer says nothing.
        Event::Diff { .. } => None,
        // Read for the list of sessions, which shows no failure.
        Event::Status(_) => None,
        // Nothing asks for the files yet.
        Event::Files { .. } => None,
    }
}

/// What to diff for the selected row, with the path to show: the staged
/// side with the source of a rename or copy, the work tree side, or an
/// untracked file whole. None when nothing is selected or the row is
/// gone from the snapshot.
fn diff_request(
    snapshot: &Snapshot,
    selected: Option<&Selected>,
) -> Option<(git::diff::Request, String)> {
    let selected = selected?;
    let entry = snapshot
        .status
        .entries
        .iter()
        .find(|entry| entry.path == selected.path)?;
    let staged = selected.section == Section::Staged;
    let mut paths = vec![entry.path.clone()];
    if staged
        && matches!(entry.index, Some(Change::Renamed | Change::Copied))
        && let Some(original) = &entry.original
    {
        paths.push(original.clone());
    }
    let request = git::diff::Request {
        paths,
        staged,
        untracked: !staged && entry.worktree == Some(Change::Untracked),
    };
    Some((request, entry.path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::commit::Edit;
    use tempfile::TempDir;

    /// How long a worker gets to answer a job.
    const WAIT: Duration = Duration::from_secs(10);

    /// Repositories with one, in `dir`, and the channel its worker's
    /// events reach.
    fn one_repo(dir: &TempDir) -> (Repos, RepoId, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new();
        let id = repos.get_or_spawn(dir.path(), &tx).expect("spawn");
        (repos, id, rx)
    }

    /// A snapshot where each of `paths` is modified in the work tree.
    fn modified(paths: &[&str]) -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.status.entries = paths
            .iter()
            .map(|path| git::status::Entry {
                path: path.into(),
                original: None,
                index: None,
                worktree: Some(Change::Modified),
                submodule: false,
            })
            .collect();
        snapshot
    }

    /// A draft with `text` typed.
    fn typed(text: &str) -> Draft {
        let mut draft = Draft::default();
        draft.apply(&Edit::Insert(text.into()));
        draft
    }

    /// A diff request for the work tree side of `path`.
    fn request(path: &str) -> git::diff::Request {
        git::diff::Request {
            paths: vec![PathBuf::from(path)],
            staged: false,
            untracked: false,
        }
    }

    #[test]
    fn a_repository_is_spawned_once_per_root() {
        let (one, other) = (TempDir::new().expect("dir"), TempDir::new().expect("dir"));
        let (mut repos, id, _rx) = one_repo(&one);
        let (tx, _) = mpsc::channel::<Msg>();
        assert_eq!(repos.get_or_spawn(one.path(), &tx).expect("spawn"), id);
        let next = repos.get_or_spawn(other.path(), &tx).expect("spawn");
        assert_eq!([id, next], [RepoId(0), RepoId(1)]);
        assert!(
            repos.get_mut(RepoId(2)).is_none(),
            "an id is not a repository"
        );
    }

    #[test]
    fn a_commit_lets_its_draft_go() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        repo.in_flight = Some(typed("fix"));
        let applied = repo.apply(Event::Committed("abc1234".into()), false, Instant::now());
        assert!(repo.in_flight.is_none());
        assert!(applied.dirty);
        let toast = applied.toast.expect("a notice");
        assert_eq!(toast.text, "committed abc1234");
        assert_eq!(toast.kind, toast::Kind::Info);
    }

    #[test]
    fn a_failed_commit_brings_its_draft_back_only_into_an_empty_box() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let failed = || Event::Failed {
            write: Write::Commit("fix\n".into()),
            error: anyhow::anyhow!("hook says no"),
        };
        repo.in_flight = Some(typed("fix"));
        // Another write failing leaves the commit and the box alone.
        let stage_all = Event::Failed {
            write: Write::StageAll,
            error: anyhow::anyhow!("fatal: no").context("git add"),
        };
        let applied = repo.apply(stage_all, false, Instant::now());
        assert_eq!(repo.in_flight, Some(typed("fix")));
        assert!(repo.draft.is_empty());
        assert_eq!(
            applied.toast.map(|toast| toast.text).as_deref(),
            Some("git add: fatal: no")
        );
        let applied = repo.apply(failed(), false, Instant::now());
        assert_eq!(repo.draft, typed("fix"));
        assert!(repo.in_flight.is_none());
        assert_eq!(
            applied.toast.map(|toast| toast.text).as_deref(),
            Some("commit failed: hook says no")
        );
        // A draft started meanwhile stays.
        repo.in_flight = Some(typed("fix"));
        repo.draft = typed("next");
        repo.apply(failed(), false, Instant::now());
        assert_eq!(repo.draft, typed("next"));
        assert!(repo.in_flight.is_none());
    }

    #[test]
    fn a_diff_answer_to_another_request_is_ignored() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let open = Diff::new(request("a.txt"), "a.txt".into());
        repo.diff = Some(open.clone());
        let old = |result| Event::Diff {
            request: request("b.txt"),
            result,
        };
        for result in [Ok("+b\n".into()), Err(anyhow::anyhow!("fatal: bad"))] {
            let applied = repo.apply(old(result), true, Instant::now());
            assert!(!applied.dirty && applied.toast.is_none());
            assert_eq!(repo.diff.as_ref(), Some(&open));
        }
        // The answer waited for fills it, and needs a frame only while the
        // side panes show.
        let answer = || Event::Diff {
            request: request("a.txt"),
            result: Ok("+a\n".into()),
        };
        assert!(!repo.apply(answer(), false, Instant::now()).dirty);
        assert_ne!(repo.diff.as_ref(), Some(&open));
        assert!(repo.apply(answer(), true, Instant::now()).dirty);
    }

    #[test]
    fn the_diff_toggles_on_the_selected_row_and_resets_the_scroll() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        repo.snapshot = Some(modified(&["a.txt"]));
        repo.selected = Some(Selected {
            section: Section::Unstaged,
            path: "a.txt".into(),
        });
        repo.scroll = 2;
        repo.toggle_diff();
        assert_eq!(repo.diff, Some(Diff::new(request("a.txt"), "a.txt".into())));
        assert_eq!(repo.scroll, 0);
        let Ok(Msg::Git(from, Event::Diff { request: asked, .. })) = rx.recv_timeout(WAIT) else {
            panic!("the worker answers the diff");
        };
        assert_eq!((from, asked), (id, request("a.txt")));
        repo.scroll = 3;
        repo.toggle_diff();
        assert!(repo.diff.is_none());
        assert_eq!(repo.scroll, 0);
    }

    #[test]
    fn a_diff_error_closes_the_diff_with_a_notice() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        repo.diff = Some(Diff::new(request("a.txt"), "a.txt".into()));
        repo.scroll = 4;
        let answer = Event::Diff {
            request: request("a.txt"),
            result: Err(anyhow::anyhow!("fatal: bad")),
        };
        let applied = repo.apply(answer, false, Instant::now());
        assert!(repo.diff.is_none());
        assert_eq!(repo.scroll, 0);
        assert!(applied.dirty, "the notice shows without the side panes");
        let toast = applied.toast.expect("a notice");
        assert_eq!(toast.text, "fatal: bad");
        assert_eq!(toast.kind, toast::Kind::Error);
    }

    #[test]
    fn the_same_snapshot_again_needs_no_frame() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        repo.snapshot = Some(modified(&["a.txt"]));
        repo.poll.sent(Read::Refresh);
        let same = Event::Refreshed(Ok(Box::new(modified(&["a.txt"]))));
        let applied = repo.apply(same, true, now);
        assert!(!applied.dirty && applied.toast.is_none());
        assert_eq!(repo.poll.in_flight, None, "the refresh still finished");
        assert!(repo.uncommitted);
    }

    #[test]
    fn a_status_read_alone_changes_the_dot_not_the_snapshot() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        assert!(!repo.uncommitted, "nothing read yet");
        repo.snapshot = Some(modified(&["a.txt"]));
        repo.poll.sent(Read::Status);
        let read = |paths: &[&str]| Event::Status(Ok(modified(paths).status));
        let applied = repo.apply(read(&["a.txt", "b.txt"]), true, now);
        assert!(!applied.dirty && applied.toast.is_none());
        assert_eq!(repo.poll.in_flight, None, "the status finished");
        assert!(repo.uncommitted);
        assert_eq!(repo.snapshot, Some(modified(&["a.txt"])), "it stays");
        repo.apply(read(&[]), true, now);
        assert!(!repo.uncommitted, "all committed");
        // One that fails says nothing and leaves the dot.
        let failed = Event::Status(Err(anyhow::anyhow!("index.lock").context("git status")));
        repo.uncommitted = true;
        let applied = repo.apply(failed, true, now);
        assert!(!applied.dirty && applied.toast.is_none());
        assert!(repo.uncommitted);
        // A refresh tells it too.
        repo.apply(Event::Refreshed(Ok(Box::default())), true, now);
        assert!(!repo.uncommitted);
    }

    #[test]
    fn a_new_snapshot_moves_the_selection_and_needs_a_frame_while_shown() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let row = |path: &str| Selected {
            section: Section::Unstaged,
            path: path.into(),
        };
        repo.snapshot = Some(modified(&["a.txt", "b.txt"]));
        repo.selected = Some(row("b.txt"));
        let fresh = |paths: &[&str]| Event::Refreshed(Ok(Box::new(modified(paths))));
        let applied = repo.apply(fresh(&["a.txt", "c.txt"]), true, Instant::now());
        assert!(applied.dirty && applied.toast.is_none());
        assert_eq!(repo.selected, Some(row("c.txt")), "the row at its place");
        assert_eq!(repo.snapshot, Some(modified(&["a.txt", "c.txt"])));
        // With the side panes hidden it is kept for later, drawing nothing.
        let applied = repo.apply(fresh(&["c.txt"]), false, Instant::now());
        assert!(!applied.dirty);
        assert_eq!(repo.snapshot, Some(modified(&["c.txt"])));
    }

    #[test]
    fn an_open_diff_follows_its_row_through_a_new_snapshot() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let mut open = Diff::new(request("a.txt"), "a.txt".into());
        open.accept(&request("a.txt"), "+a\n");
        repo.diff = Some(open);
        repo.snapshot = Some(modified(&["a.txt"]));
        repo.selected = Some(Selected {
            section: Section::Unstaged,
            path: "a.txt".into(),
        });
        let fresh = Event::Refreshed(Ok(Box::new(modified(&["a.txt", "b.txt"]))));
        repo.apply(fresh, true, Instant::now());
        assert_eq!(
            repo.diff,
            Some(Diff::new(request("a.txt"), "a.txt".into())),
            "read again"
        );
        let Ok(Msg::Git(from, Event::Diff { request: asked, .. })) = rx.recv_timeout(WAIT) else {
            panic!("the worker answers the diff");
        };
        assert_eq!((from, asked), (id, request("a.txt")));
        // A snapshot without the row closes it.
        repo.scroll = 5;
        repo.apply(
            Event::Refreshed(Ok(Box::new(modified(&[])))),
            true,
            Instant::now(),
        );
        assert!(repo.diff.is_none());
        assert_eq!(repo.scroll, 0);
    }

    #[test]
    fn a_repository_tags_its_events_with_its_id() {
        let (first, second) = (TempDir::new().expect("dir"), TempDir::new().expect("dir"));
        git::tests::init(second.path());
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new();
        repos.get_or_spawn(first.path(), &tx).expect("spawn");
        let id = repos.get_or_spawn(second.path(), &tx).expect("spawn");
        assert_eq!(id, RepoId(1));
        let repo = repos.get_mut(id).expect("its repository");
        repo.read_when_due(Instant::now(), Shown::Changes);
        assert_eq!(repo.poll.in_flight, Some(Read::Refresh), "one at a time");
        let Ok(Msg::Git(from, Event::Refreshed(Ok(_)))) = rx.recv_timeout(WAIT) else {
            panic!("the worker answers the refresh");
        };
        assert_eq!(from, id);
    }

    #[test]
    fn the_shown_repository_is_refreshed_and_the_others_read_their_status() {
        let dirs = [(); 3].map(|()| TempDir::new().expect("dir"));
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new();
        let ids = dirs.each_ref().map(|dir| {
            git::tests::init(dir.path());
            repos.get_or_spawn(dir.path(), &tx).expect("spawn")
        });
        std::fs::write(dirs[1].path().join("new.txt"), "x\n").expect("write");
        let in_flight =
            |repos: &Repos| ids.map(|id| repos.get(id).expect("its repo").poll.in_flight);
        // Hidden side panes: nothing is read.
        let now = Instant::now();
        repos.read_when_due(now, Some(ids[0]), false, false);
        assert_eq!(in_flight(&repos), [None; 3]);
        // The shown one alone while the list is not on screen.
        repos.read_when_due(now, Some(ids[0]), true, false);
        assert_eq!(in_flight(&repos), [Some(Read::Refresh), None, None]);
        // The others too while it is.
        repos.read_when_due(now, Some(ids[0]), true, true);
        let status = Some(Read::Status);
        assert_eq!(in_flight(&repos), [Some(Read::Refresh), status, status]);
        for _ in ids {
            let Ok(Msg::Git(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("each worker answers");
            };
            let read = match &event {
                Event::Refreshed(Ok(_)) => Read::Refresh,
                Event::Status(Ok(_)) => Read::Status,
                _ => panic!("a read"),
            };
            assert_eq!(read == Read::Refresh, from == ids[0], "{event:?}");
            repos.apply(from, event, true, Instant::now());
        }
        assert_eq!(in_flight(&repos), [None; 3]);
        let dots = ids.map(|id| repos.get(id).expect("its repo").uncommitted);
        assert_eq!(dots, [false, true, false]);
        // Outside a repository the shown session has none: every one is read
        // for the list, once its interval passed.
        repos.read_when_due(Instant::now(), None, true, true);
        assert_eq!(in_flight(&repos), [None; 3], "read just now");
        repos.read_when_due(Instant::now() + STATUS_EVERY, None, true, true);
        assert_eq!(in_flight(&repos), [status; 3]);
    }

    #[test]
    fn the_same_refresh_failure_shows_once() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        repo.snapshot = Some(modified(&["a.txt"]));
        let broken = || {
            Event::Refreshed(Err(
                anyhow::anyhow!("not a git repository").context("git status")
            ))
        };
        repo.poll.sent(Read::Refresh);
        let applied = repo.apply(broken(), true, Instant::now());
        assert_eq!(repo.poll.in_flight, None, "a failed refresh still finished");
        assert!(applied.dirty);
        assert_eq!(
            applied.toast.map(|toast| toast.text).as_deref(),
            Some("git status: not a git repository")
        );
        let applied = repo.apply(broken(), true, Instant::now());
        assert!(!applied.dirty && applied.toast.is_none());
        assert_eq!(
            repo.snapshot,
            Some(modified(&["a.txt"])),
            "the last one stays"
        );
    }

    #[test]
    fn an_event_for_an_unknown_repository_changes_nothing() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        repos.get_mut(id).expect("its repository").in_flight = Some(typed("fix"));
        let applied = repos.apply(
            RepoId(1),
            Event::Committed("abc1234".into()),
            true,
            Instant::now(),
        );
        assert!(!applied.dirty && applied.toast.is_none());
        let repo = repos.get_mut(id).expect("its repository");
        assert_eq!(repo.in_flight, Some(typed("fix")));
        let applied = repos.apply(id, Event::Committed("abc1234".into()), true, Instant::now());
        assert!(
            applied.toast.is_some(),
            "its own repository's event applies"
        );
        assert!(
            repos
                .get_mut(id)
                .expect("its repository")
                .in_flight
                .is_none()
        );
    }

    #[test]
    fn a_repository_stays_while_used_or_committing() {
        // Used, committing, kept.
        let table = [
            (true, false, true),
            (true, true, true),
            (false, true, true),
            (false, false, false),
        ];
        for (used, committing, stays) in table {
            assert_eq!(kept(used, committing), stays, "{used} {committing}");
        }
    }

    #[test]
    fn an_unused_repository_goes_once_its_commit_ended() {
        let dirs = [(); 3].map(|()| TempDir::new().expect("dir"));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new();
        let [used, committing, idle] =
            [0, 1, 2].map(|n| repos.get_or_spawn(dirs[n].path(), &tx).expect("spawn"));
        repos.get_mut(committing).expect("its repository").in_flight = Some(typed("fix"));
        repos.release_unused(|id| id == used);
        assert!(repos.get(used).is_some(), "used");
        assert!(repos.get(committing).is_some(), "its commit runs");
        assert!(repos.get(idle).is_none(), "let go");
        // A late event of the one let go reaches nothing.
        let late = repos.apply(
            idle,
            Event::Committed("abc1234".into()),
            true,
            Instant::now(),
        );
        assert!(!late.dirty && late.toast.is_none());
        // The commit's end reaches its repository, which goes next.
        let ended = repos.apply(
            committing,
            Event::Committed("abc1234".into()),
            true,
            Instant::now(),
        );
        assert!(ended.toast.is_some(), "the commit's notice");
        repos.release_unused(|id| id == used);
        assert!(repos.get(committing).is_none());
        assert!(repos.get(used).is_some());
        // Its root, used again, gets a repository of a new id.
        let again = repos.get_or_spawn(dirs[2].path(), &tx).expect("spawn");
        assert_eq!(again, RepoId(3));
    }

    #[test]
    fn selected_paths_follow_the_row_and_its_list() {
        let entry = |path: &str, original: Option<&str>, index, worktree| git::status::Entry {
            path: path.into(),
            original: original.map(PathBuf::from),
            index,
            worktree,
            submodule: false,
        };
        let mut snapshot = Snapshot::default();
        snapshot.status.entries = vec![
            entry("new.txt", Some("old.txt"), Some(Change::Renamed), None),
            entry("dup.txt", Some("src.txt"), Some(Change::Copied), None),
            entry("plain.txt", None, None, Some(Change::Modified)),
            entry(
                "both.txt",
                None,
                Some(Change::Modified),
                Some(Change::Modified),
            ),
            entry("conflict.txt", None, None, Some(Change::Unmerged)),
        ];
        let row = |section, path: &str| Selected {
            section,
            path: path.into(),
        };
        let paths = |names: &[&str]| Some(names.iter().map(PathBuf::from).collect::<Vec<_>>());
        let (staged, unstaged) = (Section::Staged, Section::Unstaged);

        // A staged row unstages, with the source of a rename but not of a
        // copy; it does not stage.
        let renamed = row(staged, "new.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&renamed), true),
            paths(&["new.txt", "old.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&renamed), false), None);
        let copied = row(staged, "dup.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&copied), true),
            paths(&["dup.txt"])
        );
        // An unstaged row stages; it does not unstage, so a conflict keeps
        // its record and a partial stage of the same path stays.
        let plain = row(unstaged, "plain.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&plain), false),
            paths(&["plain.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&plain), true), None);
        let conflict = row(unstaged, "conflict.txt");
        assert_eq!(
            selected_paths(&snapshot, Some(&conflict), false),
            paths(&["conflict.txt"])
        );
        assert_eq!(selected_paths(&snapshot, Some(&conflict), true), None);
        assert_eq!(
            selected_paths(&snapshot, Some(&row(unstaged, "both.txt")), true),
            None
        );
        assert_eq!(
            selected_paths(&snapshot, Some(&row(staged, "both.txt")), true),
            paths(&["both.txt"])
        );
        assert_eq!(
            selected_paths(&snapshot, Some(&row(unstaged, "gone.txt")), false),
            None
        );
        assert_eq!(selected_paths(&snapshot, None, false), None);
    }

    #[test]
    fn git_events_become_notices_and_refresh_failures_only_once() {
        let mut last = None;
        assert_eq!(
            notice(&Event::Committed("abc1234".into()), &mut last),
            Some(("committed abc1234".into(), toast::Kind::Info))
        );
        let failed = Event::Failed {
            write: Write::StageAll,
            error: anyhow::anyhow!("fatal: no").context("git add"),
        };
        assert_eq!(
            notice(&failed, &mut last),
            Some(("git add: fatal: no".into(), toast::Kind::Error))
        );
        let hook = Event::Failed {
            write: Write::Commit("m\n".into()),
            error: anyhow::anyhow!("hook says no"),
        };
        assert_eq!(
            notice(&hook, &mut last),
            Some(("commit failed: hook says no".into(), toast::Kind::Error))
        );
        let broken = || {
            Event::Refreshed(Err(
                anyhow::anyhow!("not a git repository").context("git status")
            ))
        };
        assert_eq!(
            notice(&broken(), &mut last),
            Some((
                "git status: not a git repository".into(),
                toast::Kind::Error
            ))
        );
        assert_eq!(notice(&broken(), &mut last), None, "the same failure again");
        let other = Event::Refreshed(Err(anyhow::anyhow!("index.lock").context("git status")));
        assert!(
            notice(&other, &mut last).is_some(),
            "a different failure shows"
        );
        assert_eq!(
            notice(&Event::Refreshed(Ok(Box::default())), &mut last),
            None
        );
        assert!(last.is_none(), "a refresh that works forgets the failure");
        assert!(
            notice(&broken(), &mut last).is_some(),
            "and the next one shows again"
        );
    }

    #[test]
    fn the_diff_request_follows_the_row_and_its_list() {
        let entry = |path: &str, original: Option<&str>, index, worktree| git::status::Entry {
            path: path.into(),
            original: original.map(PathBuf::from),
            index,
            worktree,
            submodule: false,
        };
        let mut snapshot = Snapshot::default();
        snapshot.status.entries = vec![
            entry(
                "new.txt",
                Some("old.txt"),
                Some(Change::Renamed),
                Some(Change::Modified),
            ),
            entry("fresh.txt", None, None, Some(Change::Untracked)),
        ];
        let row = |section, path: &str| Selected {
            section,
            path: path.into(),
        };
        let (request, path) =
            diff_request(&snapshot, Some(&row(Section::Staged, "new.txt"))).expect("staged");
        assert_eq!(path, "new.txt");
        assert!(request.staged && !request.untracked);
        assert_eq!(
            request.paths,
            [PathBuf::from("new.txt"), PathBuf::from("old.txt")]
        );
        let (request, _) =
            diff_request(&snapshot, Some(&row(Section::Unstaged, "new.txt"))).expect("unstaged");
        assert!(!request.staged && !request.untracked);
        assert_eq!(
            request.paths,
            [PathBuf::from("new.txt")],
            "the work tree side knows one path"
        );
        let (request, _) =
            diff_request(&snapshot, Some(&row(Section::Unstaged, "fresh.txt"))).expect("untracked");
        assert!(request.untracked && !request.staged);
        assert_eq!(
            diff_request(&snapshot, Some(&row(Section::Unstaged, "gone"))),
            None
        );
        assert_eq!(diff_request(&snapshot, None), None);
        let mut last = None;
        let answer = Event::Diff {
            request: request.clone(),
            result: Err(anyhow::anyhow!("fatal: bad")),
        };
        assert_eq!(
            notice(&answer, &mut last),
            None,
            "diff answers are not notices"
        );
    }

    #[test]
    fn poll_is_due_at_once_then_every_interval_after_the_last_read() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let (refresh, status) = (Some(Read::Refresh), Some(Read::Status));
        let mut poll = Poll::default();
        assert_eq!(poll.due(t0, Shown::Nowhere), None, "not on screen");
        assert_eq!(poll.due(t0, Shown::Changes), refresh, "first at once");
        assert_eq!(poll.due(t0, Shown::List), status, "first at once");
        poll.sent(Read::Refresh);
        for shown_in in [Shown::Changes, Shown::List] {
            let later = t0 + STATUS_EVERY * 2;
            assert_eq!(poll.due(later, shown_in), None, "one at a time");
        }
        // A slow refresh pushes the next one out: the gap stays.
        let t1 = t0 + POLL_EVERY * 2;
        poll.done(Read::Refresh, t1);
        assert_eq!(poll.due(t1 + POLL_EVERY - ms, Shown::Changes), None);
        assert_eq!(poll.due(t1 + POLL_EVERY, Shown::Changes), refresh);
        assert_eq!(poll.due(t1 + POLL_EVERY, Shown::Nowhere), None);
        // The refresh read the status too: the list waits its interval.
        assert_eq!(poll.due(t1 + STATUS_EVERY - ms, Shown::List), None);
        assert_eq!(poll.due(t1 + STATUS_EVERY, Shown::List), status);
        poll.sent(Read::Status);
        let t2 = t1 + STATUS_EVERY;
        poll.done(Read::Status, t2);
        assert_eq!(poll.due(t2 + STATUS_EVERY - ms, Shown::List), None);
        assert_eq!(poll.due(t2 + STATUS_EVERY, Shown::List), status);
        // A status read does not move the refresh's interval.
        assert_eq!(poll.due(t2, Shown::Changes), refresh);
    }

    #[test]
    fn a_read_answers_what_it_reads() {
        let t0 = Instant::now();
        let mut poll = Poll::default();
        // The worker folded a status queued with a write's refresh into it.
        poll.sent(Read::Status);
        poll.done(Read::Refresh, t0);
        assert_eq!(poll.in_flight, None);
        // A status does not answer a refresh.
        poll.sent(Read::Refresh);
        poll.done(Read::Status, t0);
        assert_eq!(poll.in_flight, Some(Read::Refresh));
        poll.done(Read::Refresh, t0);
        assert_eq!(poll.in_flight, None);
    }

    #[test]
    fn a_repository_shown_again_is_refreshed_at_once() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        let refresh = Some(Read::Refresh);
        // A refresh in flight stands for it.
        repo.poll.sent(Read::Refresh);
        repo.refresh_soon();
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "one at a time");
        repo.poll.done(Read::Refresh, now);
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "it ran");
        repo.refresh_soon();
        assert_eq!(repo.poll.due(now, Shown::Changes), refresh);
        assert_eq!(repo.poll.due(now, Shown::Nowhere), None, "not shown");
        // Shown while its status is read for the list: the refresh follows
        // as soon as the status is in.
        repo.poll.done(Read::Refresh, now);
        repo.poll.sent(Read::Status);
        repo.refresh_soon();
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "one at a time");
        repo.poll.done(Read::Status, now);
        assert_eq!(repo.poll.due(now, Shown::Changes), refresh);
    }
}
