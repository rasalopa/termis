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

/// A repository's id, from a counter that only grows: never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RepoId(u32);

/// A repository in use: its git worker with the poll that asks it for
/// refreshes and the snapshot it read last, and the changes pane's
/// selection, scroll, diff view, commit draft and commit in flight.
pub(super) struct Repo {
    /// What the worker's events are tagged with.
    id: RepoId,
    /// The root of the repository's working tree.
    root: PathBuf,
    /// Runs git in the repository, off the main thread.
    worker: Worker,
    /// When to ask the worker for a refresh.
    poll: Poll,
    /// The repository as last read, once the worker read it.
    pub(super) snapshot: Option<Snapshot>,
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
            poll: Poll::new(POLL_EVERY),
            snapshot: None,
            selected: None,
            scroll: 0,
            diff: None,
            draft: Draft::default(),
            in_flight: None,
            refresh_error: None,
        }
    }

    /// Asks the worker for a refresh when one is due at `now`; `wanted`
    /// while the changes pane is on screen.
    pub(super) fn refresh_when_due(&mut self, now: Instant, wanted: bool) {
        if self.poll.due(now, wanted) {
            self.worker.send(Job::Refresh);
            self.poll.sent(now);
        }
    }

    /// Applies `event` from the worker, at `now`. A commit's end lets its
    /// draft go, or brings it back; the answer to the diff on screen
    /// fills it or closes it; a refresh that read something new replaces
    /// the snapshot, and the selection and the diff follow. What changed
    /// in the changes pane needs a frame only while `side_panes` are on
    /// screen.
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
                self.poll.done(now);
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
    pub(super) fn get_mut(&mut self, id: RepoId) -> Option<&mut Repo> {
        self.list.iter_mut().find(|repo| repo.id == id)
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

/// When to ask the worker for a refresh: every `every` while the side
/// panes are shown, and never while one is in flight. The interval runs
/// from the end of the last refresh, so a slow repository leaves gaps.
#[derive(Debug)]
struct Poll {
    every: Duration,
    last: Option<Instant>,
    in_flight: bool,
}

impl Poll {
    /// A schedule with nothing sent yet: the first refresh is due at once.
    fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            in_flight: false,
        }
    }

    /// Whether a refresh should be sent now.
    fn due(&self, now: Instant, wanted: bool) -> bool {
        wanted
            && !self.in_flight
            && self
                .last
                .is_none_or(|last| now.duration_since(last) >= self.every)
    }

    /// Records that a refresh was sent at `now`.
    fn sent(&mut self, now: Instant) {
        self.last = Some(now);
        self.in_flight = true;
    }

    /// Records that the refresh in flight finished at `now`.
    fn done(&mut self, now: Instant) {
        self.last = Some(now);
        self.in_flight = false;
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
        repo.poll.sent(now);
        let same = Event::Refreshed(Ok(Box::new(modified(&["a.txt"]))));
        let applied = repo.apply(same, true, now);
        assert!(!applied.dirty && applied.toast.is_none());
        assert!(!repo.poll.in_flight, "the refresh still finished");
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
        repo.refresh_when_due(Instant::now(), true);
        assert!(repo.poll.in_flight, "one refresh at a time");
        let Ok(Msg::Git(from, Event::Refreshed(Ok(_)))) = rx.recv_timeout(WAIT) else {
            panic!("the worker answers the refresh");
        };
        assert_eq!(from, id);
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
        repo.poll.sent(Instant::now());
        let applied = repo.apply(broken(), true, Instant::now());
        assert!(!repo.poll.in_flight, "a failed refresh still finished");
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
    fn poll_is_due_at_once_then_every_interval_after_the_last_refresh() {
        let t0 = Instant::now();
        let every = Duration::from_secs(2);
        let mut poll = Poll::new(every);
        assert!(!poll.due(t0, false), "not wanted");
        assert!(poll.due(t0, true), "first at once");
        poll.sent(t0);
        assert!(!poll.due(t0 + every, true), "in flight");
        // A slow refresh pushes the next one out: the gap stays.
        poll.done(t0 + every * 2);
        assert!(
            !poll.due(t0 + every * 3 - Duration::from_millis(1), true),
            "too soon"
        );
        assert!(poll.due(t0 + every * 3, true));
        assert!(!poll.due(t0 + every * 3, false), "hidden panes");
    }
}
