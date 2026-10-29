//! Repositories: the git worker of each one in use, and what the main
//! loop keeps of it for the changes pane.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{Msg, TOAST_LASTS};
use crate::agent::Ask;
use crate::agent::subject::{self, Running};
use crate::git;
use crate::git::files::Files;
use crate::git::push::Push;
use crate::git::status::Change;
use crate::git::worker::{Event, Job, Snapshot, Worker, Write};
use crate::strings;
use crate::ui::changes::{self, Movement, Section, Selected};
use crate::ui::commit::Draft;
use crate::ui::diff::Diff;
use crate::ui::toast::{self, Toast};

/// How often the repository the changes pane shows is refreshed while
/// the side panes are on screen, the changes pane among them or not,
/// unless the config file sets another interval: the default of its
/// `poll-seconds`.
pub(super) const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long after a change to its files is first seen that repository
/// is refreshed: a tool's write, a commit or a checkout lands in one go.
const AFTER_CHANGE: Duration = Duration::from_millis(100);
/// The least time from the end of a refresh to one a change makes due,
/// at first; doubled after each refresh a change started that read
/// nothing new, up to `LONGEST_GAP`.
const CHANGE_GAP: Duration = Duration::from_millis(250);
/// The longest the gap after a change grows to, whatever the poll
/// interval: changes that read nothing new cost about what a 2 s poll
/// costs, and a change waits at most this long while they last.
const LONGEST_GAP: Duration = Duration::from_secs(2);
/// How often the status of a repository is read while only the list of
/// sessions shows it.
const STATUS_EVERY: Duration = Duration::from_secs(10);

/// A repository's id, from a counter that only grows: never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RepoId(u32);

impl RepoId {
    /// Id `n`, for tests that need no repository behind it.
    #[cfg(test)]
    pub(super) fn for_tests(n: u32) -> Self {
        Self(n)
    }
}

/// A repository in use: its git worker with the poll that asks it to
/// read the repository and whether it watches its files, the snapshot it
/// read last and whether it has uncommitted changes, the changes pane's
/// selection, scroll, diff view, commit draft, the subject the agent
/// writes for it and the commit in flight, and the push running.
pub(super) struct Repo {
    /// What the worker's events are tagged with.
    id: RepoId,
    /// The root of the repository's working tree.
    root: PathBuf,
    /// Runs git in the repository, off the main thread.
    worker: Worker,
    /// When to ask the worker to read the repository.
    poll: Poll,
    /// Whether the worker was last asked to watch the repository's files,
    /// as it is while the changes pane shows it.
    watched: bool,
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
    /// The subject the agent writes for the draft, on a thread of its
    /// own.
    pub(super) suggestion: Option<Suggestion>,
    /// The draft of the commit running, brought back if it fails.
    pub(super) in_flight: Option<Draft>,
    /// The push running, on a thread of its own.
    pub(super) pushing: Option<Pushing>,
    /// The last refresh failure shown, so that it shows once.
    refresh_error: Option<String>,
}

/// A subject the agent writes for a repository's draft.
pub(super) struct Suggestion {
    /// The project of the session it was asked from, which names the
    /// repository in a failure's notice while no session using it is
    /// shown.
    project: String,
    /// Whether the draft went into a commit before the answer came: the
    /// run is then stopped, and its answer goes nowhere.
    committed: bool,
    /// The run, stopped once the suggestion is dropped, as when the
    /// repository is let go.
    run: Running,
}

/// A push running from a repository.
pub(super) struct Pushing {
    /// What it pushes.
    push: Push,
    /// The project of the session it was started from, which names the
    /// repository in its notice while no session using it is shown.
    project: String,
}

/// What a git event changed for the main loop.
#[derive(Debug, Default)]
pub(super) struct Applied {
    /// Whether the screen needs a new frame.
    pub(super) dirty: bool,
    /// A notice to show.
    pub(super) toast: Option<Toast>,
    /// Whether a refresh found the status other than the one it replaced,
    /// so that the files may have changed too.
    pub(super) status_changed: bool,
    /// The directory whose files were listed, relative to the root, and
    /// its files, relative to the root too, or why they could not be
    /// listed, for the sessions that run in it.
    pub(super) files: Option<(PathBuf, Result<Files, String>)>,
}

impl Repo {
    /// Repository `id` at `root`, whose git runs on `worker`, refreshed
    /// `every` so long while its changes pane shows it; nothing read
    /// yet.
    fn new(id: RepoId, root: PathBuf, worker: Worker, every: Duration) -> Self {
        Self {
            id,
            root,
            worker,
            poll: Poll::new(every),
            watched: false,
            snapshot: None,
            uncommitted: false,
            selected: None,
            scroll: 0,
            diff: None,
            draft: Draft::default(),
            suggestion: None,
            in_flight: None,
            pushing: None,
            refresh_error: None,
        }
    }

    /// Asks the worker for the read due at `now` for what shows the
    /// repository, if one is, a change to its files included. As
    /// [`Shown::Changes`] it is watched, the watch asked for before the
    /// read. Shown otherwise, it is not, a change seen is dropped, and it
    /// gets no refresh: its snapshot is no longer [`Repo::fresh`], and the
    /// next refresh is due at once.
    fn read_when_due(&mut self, now: Instant, shown_in: Shown) {
        let watch = shown_in == Shown::Changes;
        if watch != self.watched {
            self.worker.send(Job::Watch(watch));
            self.watched = watch;
        }
        if shown_in != Shown::Changes {
            self.refresh_soon();
            self.poll.change = None;
        }
        if let Some(read) = self.poll.due(now, shown_in) {
            self.worker.send(match read {
                Read::Refresh => Job::Refresh,
                Read::Status => Job::Status,
            });
            self.poll.sent(read);
        }
    }

    /// Makes the next refresh due at once. A refresh in flight stands for
    /// it; a status in flight does not, so the refresh follows it. A
    /// change seen no longer waits, unless a refresh in flight may not
    /// have read it.
    pub(super) fn refresh_soon(&mut self) {
        self.poll.refreshed = None;
        if self.poll.in_flight != Some(Read::Refresh) {
            self.poll.change = None;
        }
    }

    /// Whether a refresh finished, since the last [`Repo::refresh_soon`]
    /// if any, which [`Repo::read_when_due`] calls while nothing
    /// refreshes the repository: the snapshot then tells how it stands,
    /// rather than how it stood when the changes pane last showed it.
    pub(super) fn fresh(&self) -> bool {
        self.poll.refreshed.is_some()
    }

    /// Asks the worker for the files of the repository's directory `dir`,
    /// relative to its root.
    pub(super) fn list_files(&self, dir: &Path) {
        self.worker.send(Job::Files(dir.to_path_buf()));
    }

    /// Records a refresh sent and finished at `now`, for tests.
    #[cfg(test)]
    pub(super) fn refreshed(&mut self, now: Instant) {
        self.poll.sent(Read::Refresh);
        self.poll.done(Read::Refresh, now, false);
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
    /// a status read tells whether there are uncommitted changes; a
    /// change to the repository's files makes a refresh due. What
    /// changed in the changes pane needs a frame only while `side_panes`
    /// are on screen; the list of sessions watches the rest. The files,
    /// and whether a refresh changed the status, go to the sessions.
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
                // A failed refresh read nothing new.
                let new = result
                    .as_ref()
                    .is_ok_and(|fresh| self.snapshot.as_ref() != Some(&**fresh));
                self.poll.done(Read::Refresh, now, new);
                if let Ok(fresh) = &result {
                    self.uncommitted = !fresh.status.entries.is_empty();
                }
                // A failed refresh keeps the last snapshot; notice() reported it.
                if let Ok(fresh) = result
                    && new
                {
                    applied.status_changed = self
                        .snapshot
                        .as_ref()
                        .is_some_and(|old| old.status != fresh.status);
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
                self.poll.done(Read::Status, now, false);
                if let Ok(status) = result {
                    self.uncommitted = !status.entries.is_empty();
                }
            }
            // Shown in the file tree, failure included.
            Event::Files { dir, result } => {
                applied.files = Some((dir, result.map_err(|error| format!("{error:#}"))));
            }
            // A refresh comes due; nothing to draw until it ends.
            Event::Changed => self.poll.saw_change(now),
        }
        applied
    }

    /// Moves the selection by `movement` through lists that show `page`
    /// lines at once, once there is a snapshot.
    pub(super) fn select(&mut self, movement: Movement, page: usize) {
        if let Some(current) = &self.snapshot {
            self.selected = changes::moved(current, self.selected.as_ref(), movement, page);
        }
    }

    /// Selects the file `selected` while the lists have it and no diff is
    /// open in their place.
    pub(super) fn point(&mut self, selected: &Selected) {
        let listed = self
            .snapshot
            .as_ref()
            .is_some_and(|current| changes::lists(current, selected));
        if listed && self.diff.is_none() {
            self.selected = Some(selected.clone());
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

    /// Moves the diff by `movement` under its header, which shows `page`
    /// lines at once: no further than its first line at the top and its
    /// last at the bottom.
    pub(super) fn scroll_diff(&mut self, movement: Movement, page: usize) {
        let lines = self.diff.as_ref().map_or(0, Diff::lines);
        self.scroll = movement.offset(self.scroll, lines, page);
    }

    /// Closes the diff, and the pane's scroll starts over.
    pub(super) fn close_diff(&mut self) {
        self.diff = None;
        self.scroll = 0;
    }

    /// Commits what is staged with `message`, keeping the draft until the
    /// commit ends. A subject the agent still writes is stopped, and its
    /// answer goes nowhere.
    pub(super) fn commit(&mut self, message: String) {
        self.worker.send(Job::Write(Write::Commit(message)));
        self.in_flight = Some(std::mem::take(&mut self.draft));
        if let Some(suggestion) = &mut self.suggestion {
            suggestion.committed = true;
            suggestion.run.stop();
        }
    }

    /// Whether a commit, a push or a subject the agent writes runs for
    /// the repository, which holds the changes pane on screen.
    pub(super) fn busy(&self) -> bool {
        self.in_flight.is_some() || self.pushing.is_some() || self.suggestion.is_some()
    }

    /// Whether the agent writes a subject for the draft as it is, which
    /// the commit box says.
    pub(super) fn suggesting(&self) -> bool {
        self.suggestion
            .as_ref()
            .is_some_and(|suggestion| !suggestion.committed)
    }

    /// Has the agent write a subject for the draft through `ask`, for the
    /// session of `project`, on a thread of its own whose answer reaches
    /// `tx`. Fails when the thread cannot start.
    pub(super) fn suggest(
        &mut self,
        ask: Ask,
        project: String,
        tx: &mpsc::Sender<Msg>,
    ) -> Result<()> {
        let id = self.id;
        let run = subject::spawn(ask, self.root.clone(), tx.clone(), move |result| {
            Msg::Suggested(id, result)
        })?;
        self.suggestion = Some(Suggestion {
            project,
            committed: false,
            run,
        });
        Ok(())
    }

    /// Applies the agent's answer, `result`, at `now`: the subject takes
    /// the place of the draft's first line, the body staying, whether the
    /// box is open or not; a failure leaves the draft and gives a notice,
    /// after the project's name while no session using the repository is
    /// shown, when `elsewhere`. An answer for a draft that went into a
    /// commit meanwhile leaves the next draft as it is. The end of a run
    /// needs a new frame, over what the agent may have written on the
    /// terminal; an answer with no suggestion running changes nothing.
    fn suggested(&mut self, result: Result<String>, elsewhere: bool, now: Instant) -> Applied {
        let mut applied = Applied::default();
        let Some(Suggestion {
            project, committed, ..
        }) = self.suggestion.take()
        else {
            return applied;
        };
        applied.dirty = true;
        if committed {
            return applied;
        }
        match result {
            Ok(subject) => self.draft.set_subject(&subject),
            Err(error) => {
                let text = strings::suggestion_failed(&format!("{error:#}"));
                let text = if elsewhere {
                    strings::for_project(&project, &text)
                } else {
                    text
                };
                applied.toast = Some(Toast::new(text, toast::Kind::Error, now, TOAST_LASTS));
            }
        }
        applied
    }

    /// Pushes as `push` says, for the session of `project`, off the
    /// worker's queue so that the reads go on: on a thread of its own,
    /// whose answer reaches `tx`. Fails when the thread cannot start.
    pub(super) fn push(
        &mut self,
        push: Push,
        project: String,
        tx: &mpsc::Sender<Msg>,
    ) -> Result<()> {
        let id = self.id;
        git::push::spawn(self.root.clone(), push.clone(), tx.clone(), move |result| {
            Msg::Pushed(id, result)
        })?;
        self.pushing = Some(Pushing { push, project });
        Ok(())
    }

    /// Applies the end of the push running, `result`, at `now`: the push
    /// goes, and a refresh is queued so that the branch's distance to its
    /// upstream follows. Returns the notice saying how it went, after the
    /// project's name while no session using the repository is shown,
    /// when `elsewhere`; none with no push running.
    fn pushed(&mut self, result: Result<()>, elsewhere: bool, now: Instant) -> Option<Toast> {
        let Pushing { push, project } = self.pushing.take()?;
        self.worker.send(Job::Refresh);
        let (text, kind) = match result {
            Ok(()) => (strings::pushed(&push), toast::Kind::Info),
            Err(error) => (
                strings::push_failed(&format!("{error:#}")),
                toast::Kind::Error,
            ),
        };
        let text = if elsewhere {
            strings::for_project(&project, &text)
        } else {
            text
        };
        Some(Toast::new(text, kind, now, TOAST_LASTS))
    }
}

/// The repositories in use, each with its own worker.
pub(super) struct Repos {
    list: Vec<Repo>,
    /// The id the next repository gets.
    next: u32,
    /// How often each is refreshed while a changes pane shows it.
    every: Duration,
}

impl Repos {
    /// No repositories yet; each one is refreshed `every` so long while
    /// a changes pane shows it.
    pub(super) fn new(every: Duration) -> Self {
        Self {
            list: Vec::new(),
            next: 0,
            every,
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
        self.list
            .push(Repo::new(id, root.to_path_buf(), worker, self.every));
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
    /// and that has no commit or push in flight: its worker stops once
    /// done with what it was asked, a subject the agent writes for it is
    /// stopped, and a late event of either reaches nothing. One with a
    /// commit or a push in flight goes on a call once it ended.
    pub(super) fn release_unused(&mut self, used: impl Fn(RepoId) -> bool) {
        self.list.retain(|repo| {
            kept(
                used(repo.id),
                repo.in_flight.is_some() || repo.pushing.is_some(),
            )
        });
    }

    /// Asks each worker for the read due at `now`: the repositories the
    /// session shown shows, `shown`, in its changes pane and in its file
    /// tree, are watched and refreshed, also after a change to their
    /// files, while the `side_panes` are on screen, the changes pane
    /// among them or not, so that it can come back; every other one has
    /// its status read while it is `listed`, with its dot in the list of
    /// sessions on screen. One left without refreshes is refreshed at
    /// once when it is shown again.
    pub(super) fn read_when_due(
        &mut self,
        now: Instant,
        shown: [Option<RepoId>; 2],
        side_panes: bool,
        listed: impl Fn(RepoId) -> bool,
    ) {
        for repo in &mut self.list {
            let shown_in = if shown.contains(&Some(repo.id)) {
                if side_panes {
                    Shown::Changes
                } else {
                    Shown::Nowhere
                }
            } else if listed(repo.id) {
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

    /// Applies the end of the push from repository `id`, `result`, at
    /// `now`, as [`Repo::pushed`] does, the session shown using
    /// repository `shown`. Returns the notice saying how it went; none
    /// for an unknown repository.
    pub(super) fn pushed(
        &mut self,
        id: RepoId,
        result: Result<()>,
        shown: Option<RepoId>,
        now: Instant,
    ) -> Option<Toast> {
        self.get_mut(id)?.pushed(result, shown != Some(id), now)
    }

    /// Stops the subject the agent writes for each repository, if any.
    pub(super) fn stop_suggestions(&self) {
        for suggestion in self.list.iter().filter_map(|repo| repo.suggestion.as_ref()) {
            suggestion.run.stop();
        }
    }

    /// Waits until the thread of each subject being written has ended,
    /// or until `deadline`.
    pub(super) fn await_suggestions(&self, deadline: Instant) {
        let running = || {
            self.list
                .iter()
                .filter_map(|repo| repo.suggestion.as_ref())
                .any(|suggestion| !suggestion.run.ended())
        };
        while running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Applies the subject the agent wrote for repository `id`, or why it
    /// wrote none, `result`, at `now`, as [`Repo::suggested`] does, the
    /// session shown using repository `shown`; an answer for an unknown
    /// repository changes nothing.
    pub(super) fn suggested(
        &mut self,
        id: RepoId,
        result: Result<String>,
        shown: Option<RepoId>,
        now: Instant,
    ) -> Applied {
        self.get_mut(id).map_or_else(Applied::default, |repo| {
            repo.suggested(result, shown != Some(id), now)
        })
    }
}

/// Whether a repository stays in use: while a session uses it, or a
/// commit or a push runs in it, whose end has to reach it.
fn kept(used: bool, running: bool) -> bool {
    used || running
}

/// What shows a repository, which decides how it is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// The changes pane, on screen or hidden beside the projects pane:
    /// everything, every poll interval and after a change to its files.
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
/// In the changes pane, a change to the repository's files makes a
/// refresh due `AFTER_CHANGE` after it was first seen, and no sooner
/// than the gap after the last refresh ended. The gap starts at
/// `CHANGE_GAP` and doubles, up to `LONGEST_GAP`, after each refresh
/// sent while a change waited that read nothing new; a refresh that read
/// something new sets it back, and so does one sent with no change
/// waiting, the files having been quiet since the last refresh was sent.
/// A poll interval shorter than the gap makes the refresh due first.
#[derive(Debug)]
struct Poll {
    /// How often the changes pane's repository is refreshed: the poll
    /// interval.
    every: Duration,
    /// When the last refresh finished; none before the first, or while
    /// the next is due at once.
    refreshed: Option<Instant>,
    /// When the last read of the status finished, alone or in a refresh.
    read: Option<Instant>,
    /// The read sent and not answered yet.
    in_flight: Option<Read>,
    /// When the first change to the repository's files since the last
    /// refresh was sent was seen; none while no change waits.
    change: Option<Instant>,
    /// Whether the refresh in flight was sent while a change waited.
    for_change: bool,
    /// How long after the end of a refresh a change can make the next
    /// one due.
    gap: Duration,
}

impl Poll {
    /// A poll that refreshes the changes pane's repository `every` so
    /// long; nothing read yet.
    fn new(every: Duration) -> Self {
        Self {
            every,
            refreshed: None,
            read: None,
            in_flight: None,
            change: None,
            for_change: false,
            gap: CHANGE_GAP,
        }
    }

    /// The read to send at `now` for what shows the repository, if one is
    /// due.
    fn due(&self, now: Instant, shown_in: Shown) -> Option<Read> {
        let passed = |last: Option<Instant>, every: Duration| {
            last.is_none_or(|last| now.duration_since(last) >= every)
        };
        let changed = self
            .change
            .is_some_and(|seen| now.duration_since(seen) >= AFTER_CHANGE)
            && passed(self.refreshed, self.gap);
        match shown_in {
            _ if self.in_flight.is_some() => None,
            Shown::Changes if passed(self.refreshed, self.every) || changed => Some(Read::Refresh),
            Shown::List if passed(self.read, STATUS_EVERY) => Some(Read::Status),
            Shown::Changes | Shown::List | Shown::Nowhere => None,
        }
    }

    /// Records a change to the repository's files seen at `now`; the
    /// first since the last refresh was sent counts.
    fn saw_change(&mut self, now: Instant) {
        self.change.get_or_insert(now);
    }

    /// Records that `read` was sent. A refresh takes the change waiting,
    /// if any.
    fn sent(&mut self, read: Read) {
        self.in_flight = Some(read);
        if read == Read::Refresh {
            self.for_change = self.change.take().is_some();
        }
    }

    /// Records that `read` finished at `now`, and for a refresh whether
    /// it read something `new`. A refresh answers any read in flight, as
    /// the worker folds a status queued with it into it; a status answers
    /// only a status. A refresh that was not sent here, as the one after
    /// a write, moves the gap only when it read something new.
    fn done(&mut self, read: Read, now: Instant, new: bool) {
        self.read = Some(now);
        match read {
            Read::Refresh => {
                let sent = self.in_flight == Some(Read::Refresh);
                if new || (sent && !self.for_change) {
                    self.gap = CHANGE_GAP;
                } else if sent {
                    self.gap = (self.gap * 2).min(LONGEST_GAP);
                }
                self.for_change = false;
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
        // The file tree says why they could not be listed.
        Event::Files { .. } => None,
        // A change only makes a refresh due.
        Event::Changed => None,
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
    use crate::ui::changes::Step;
    use crate::ui::commit::Edit;
    use tempfile::TempDir;

    /// How long a worker gets to answer a job.
    const WAIT: Duration = Duration::from_secs(10);

    /// Repositories with one, in `dir`, and the channel its worker's
    /// events reach.
    fn one_repo(dir: &TempDir) -> (Repos, RepoId, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
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
    fn a_file_pointed_at_is_selected_while_listed_and_no_diff_is_open() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let unstaged = |path: &str| Selected {
            section: Section::Unstaged,
            path: path.into(),
        };
        repo.point(&unstaged("a.txt"));
        assert_eq!(repo.selected, None, "nothing read yet");
        repo.snapshot = Some(modified(&["a.txt", "b.txt"]));
        repo.point(&unstaged("b.txt"));
        assert_eq!(repo.selected, Some(unstaged("b.txt")));
        // A file the lists do not have, or not in that list, is not
        // selected.
        repo.point(&unstaged("c.txt"));
        let staged = Selected {
            section: Section::Staged,
            path: "a.txt".into(),
        };
        repo.point(&staged);
        assert_eq!(repo.selected, Some(unstaged("b.txt")));
        // Nor one under the diff.
        repo.diff = Some(Diff::new(request("b.txt"), "b.txt".into()));
        repo.point(&unstaged("a.txt"));
        assert_eq!(repo.selected, Some(unstaged("b.txt")));
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
    fn the_selection_and_the_diff_move_by_half_or_all_the_lines_shown() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let row = |path: &str| Selected {
            section: Section::Unstaged,
            path: path.into(),
        };
        // Before a snapshot there is nothing to select.
        repo.select(Movement::End, 4);
        assert_eq!(repo.selected, None);
        repo.snapshot = Some(modified(&["a", "b", "c", "d", "e", "f"]));
        repo.select(Movement::Line(Step::Down), 4);
        assert_eq!(repo.selected, Some(row("a")), "the first row first");
        repo.select(Movement::Half(Step::Down), 4);
        assert_eq!(repo.selected, Some(row("c")));
        repo.select(Movement::Page(Step::Down), 4);
        assert_eq!(repo.selected, Some(row("f")), "no further than the last");
        repo.select(Movement::Half(Step::Up), 4);
        assert_eq!(repo.selected, Some(row("d")));
        repo.select(Movement::Top, 4);
        assert_eq!(repo.selected, Some(row("a")));
        repo.select(Movement::End, 4);
        assert_eq!(repo.selected, Some(row("f")));
        // Ten lines of diff, four of them under its header.
        let mut diff = Diff::new(request("a"), "a".into());
        let text: String = (0..10).map(|n| format!("+{n}\n")).collect();
        assert!(diff.accept(&request("a"), &text));
        repo.diff = Some(diff);
        repo.scroll_diff(Movement::Half(Step::Down), 4);
        assert_eq!(repo.scroll, 2);
        repo.scroll_diff(Movement::Page(Step::Down), 4);
        assert_eq!(repo.scroll, 6, "the last line at the bottom");
        repo.scroll_diff(Movement::Line(Step::Down), 4);
        assert_eq!(repo.scroll, 6);
        repo.scroll_diff(Movement::Page(Step::Up), 4);
        assert_eq!(repo.scroll, 2);
        repo.scroll_diff(Movement::End, 4);
        assert_eq!(repo.scroll, 6);
        repo.scroll_diff(Movement::Top, 4);
        assert_eq!(repo.scroll, 0);
        // Before the answer there is nothing to move through.
        repo.diff = Some(Diff::new(request("a"), "a".into()));
        repo.scroll_diff(Movement::End, 4);
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
    fn a_refresh_says_when_the_status_changed_and_the_files_go_to_the_sessions() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        let fresh = |snapshot: Snapshot| Event::Refreshed(Ok(Box::new(snapshot)));
        let changed =
            |repo: &mut Repo, snapshot| repo.apply(fresh(snapshot), true, now).status_changed;
        assert!(!changed(repo, modified(&["a.txt"])), "the first read");
        assert!(!changed(repo, modified(&["a.txt"])), "the same");
        assert!(changed(repo, modified(&["a.txt", "b.txt"])));
        // New counts alone leave the status as it was.
        let mut counted = modified(&["a.txt", "b.txt"]);
        counted.worktree.insert(
            "a.txt".into(),
            git::numstat::Lines::Text {
                added: 1,
                deleted: 0,
            },
        );
        assert!(!changed(repo, counted));
        let failed = Event::Refreshed(Err(anyhow::anyhow!("index.lock")));
        assert!(!repo.apply(failed, true, now).status_changed);
        // The files, or why they could not be listed, with no notice.
        let files = Files {
            paths: vec![PathBuf::from("a.txt")],
            cut: true,
        };
        let listed = |result| Event::Files {
            dir: PathBuf::from("src"),
            result,
        };
        let applied = repo.apply(listed(Ok(files.clone())), true, now);
        assert_eq!(applied.files, Some((PathBuf::from("src"), Ok(files))));
        assert!(!applied.dirty && applied.toast.is_none());
        let error = anyhow::anyhow!("fatal: bad").context("git ls-files");
        let applied = repo.apply(listed(Err(error)), true, now);
        assert_eq!(
            applied.files,
            Some((PathBuf::from("src"), Err("git ls-files: fatal: bad".into())))
        );
        assert!(!applied.dirty && applied.toast.is_none());
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
        let mut repos = Repos::new(POLL_EVERY);
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
    fn the_shown_repository_is_refreshed_and_the_listed_ones_read_their_status() {
        let dirs = [(); 4].map(|()| TempDir::new().expect("dir"));
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let ids = dirs.each_ref().map(|dir| {
            git::tests::init(dir.path());
            repos.get_or_spawn(dir.path(), &tx).expect("spawn")
        });
        std::fs::write(dirs[1].path().join("new.txt"), "x\n").expect("write");
        let in_flight =
            |repos: &Repos| ids.map(|id| repos.get(id).expect("its repo").poll.in_flight);
        // The last one has no row on the list.
        let listed = |id: RepoId| id != ids[3];
        let nowhere = |_: RepoId| false;
        // Hidden side panes: nothing is read.
        let now = Instant::now();
        repos.read_when_due(now, [Some(ids[0]), None], false, nowhere);
        assert_eq!(in_flight(&repos), [None; 4]);
        // The shown one alone while the list is not on screen.
        repos.read_when_due(now, [Some(ids[0]), None], true, nowhere);
        assert_eq!(in_flight(&repos), [Some(Read::Refresh), None, None, None]);
        // Those on the list too while it is.
        repos.read_when_due(now, [Some(ids[0]), None], true, listed);
        let status = Some(Read::Status);
        let read = [Some(Read::Refresh), status, status, None];
        assert_eq!(in_flight(&repos), read);
        for _ in &ids[..3] {
            let Ok(Msg::Git(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("each worker asked answers");
            };
            let read = match &event {
                Event::Refreshed(Ok(_)) => Read::Refresh,
                Event::Status(Ok(_)) => Read::Status,
                _ => panic!("a read"),
            };
            assert_eq!(read == Read::Refresh, from == ids[0], "{event:?}");
            repos.apply(from, event, true, Instant::now());
        }
        assert_eq!(in_flight(&repos), [None; 4]);
        let dots = ids.map(|id| repos.get(id).expect("its repo").uncommitted);
        assert_eq!(dots, [false, true, false, false]);
        // Outside a repository the shown session has none: every one on
        // the list is read for it, once its interval passed.
        repos.read_when_due(Instant::now(), [None, None], true, listed);
        assert_eq!(in_flight(&repos), [None; 4], "read just now");
        let later = Instant::now() + STATUS_EVERY;
        repos.read_when_due(later, [None, None], true, listed);
        assert_eq!(in_flight(&repos), [status, status, status, None]);
    }

    #[test]
    fn the_repositories_of_the_changes_pane_and_of_the_file_tree_are_refreshed() {
        let dirs = [(); 3].map(|()| TempDir::new().expect("dir"));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let ids = dirs.each_ref().map(|dir| {
            git::tests::init(dir.path());
            repos.get_or_spawn(dir.path(), &tx).expect("spawn")
        });
        let shown = [Some(ids[2]), Some(ids[0])];
        repos.read_when_due(Instant::now(), shown, true, |_| true);
        let in_flight = ids.map(|id| repos.get(id).expect("its repo").poll.in_flight);
        let refresh = Some(Read::Refresh);
        assert_eq!(in_flight, [refresh, Some(Read::Status), refresh]);
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
    fn a_repository_stays_while_used_or_running_a_commit_or_a_push() {
        // Used, running, kept.
        let table = [
            (true, false, true),
            (true, true, true),
            (false, true, true),
            (false, false, false),
        ];
        for (used, running, stays) in table {
            assert_eq!(kept(used, running), stays, "{used} {running}");
        }
    }

    #[test]
    fn a_push_answers_from_its_thread_and_ends_with_a_notice_and_a_refresh() {
        let dir = TempDir::new().expect("dir");
        git::tests::init(dir.path());
        git::tests::git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "a"]);
        let (mut repos, id, rx) = one_repo(&dir);
        let track = Push::Track {
            branch: "main".into(),
        };
        let (tx, ends) = mpsc::channel::<Msg>();
        let repo = repos.get_mut(id).expect("its repository");
        repo.push(track.clone(), "api".into(), &tx).expect("starts");
        assert!(repo.pushing.is_some());
        // Kept while it runs, though no session uses it.
        repos.release_unused(|_| false);
        assert!(repos.get(id).is_some(), "its push runs");
        // There is no origin to push to; git says so, from the thread.
        let Ok(Msg::Pushed(from, result)) = ends.recv_timeout(WAIT) else {
            panic!("the push's end");
        };
        assert_eq!(from, id);
        let now = Instant::now();
        let toast = repos.pushed(id, result, Some(id), now).expect("a notice");
        assert_eq!(toast.kind, toast::Kind::Error);
        assert!(toast.text.starts_with("push failed: "), "{}", toast.text);
        assert!(toast.text.contains("origin"), "{}", toast.text);
        assert!(repos.get(id).expect("its repository").pushing.is_none());
        // A refresh follows, for the distance to the upstream.
        let Ok(Msg::Git(from, Event::Refreshed(_))) = rx.recv_timeout(WAIT) else {
            panic!("a refresh");
        };
        assert_eq!(from, id);
        assert!(
            repos.pushed(id, Ok(()), Some(id), now).is_none(),
            "no push runs"
        );
        // While no session using it is shown, the notice names the
        // project of the session that pushed.
        repos.get_mut(id).expect("its repository").pushing = Some(Pushing {
            push: track,
            project: "api".into(),
        });
        let toast = repos.pushed(id, Ok(()), None, now).expect("a notice");
        assert_eq!(
            toast.text,
            "api: pushed main to origin, tracking origin/main"
        );
        assert_eq!(toast.kind, toast::Kind::Info);
        repos.release_unused(|_| false);
        assert!(repos.get(id).is_none(), "let go once it ended");
        assert!(repos.pushed(id, Ok(()), None, now).is_none(), "gone");
    }

    /// A repository in `dir` with a file staged.
    fn staged_repo(dir: &TempDir) {
        git::tests::init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git::tests::git(dir.path(), &["add", "a.txt"]);
    }

    /// A fake agent in `bin` that runs `script` under `sh`.
    fn fake_agent(bin: &TempDir, script: &str) -> Ask {
        use std::os::unix::fs::PermissionsExt;
        let program = bin.path().join("claude");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("write");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Ask {
            program: program.to_string_lossy().into_owned(),
            args: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    /// An agent that is not there: its suggestion fails at once.
    fn missing_agent(bin: &TempDir) -> Ask {
        Ask {
            program: bin.path().join("nowhere").to_string_lossy().into_owned(),
            args: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    /// A fake agent that hangs for longer than a test waits.
    const HANGS: &str = "cat > /dev/null\nsleep 30";

    #[test]
    fn a_suggestion_answers_from_its_thread_and_takes_the_subjects_place() {
        let dir = TempDir::new().expect("dir");
        staged_repo(&dir);
        let bin = TempDir::new().expect("dir");
        let ask = fake_agent(&bin, "cat > /dev/null\necho 'feat: add a'");
        let (mut repos, id, _rx) = one_repo(&dir);
        let (tx, answers) = mpsc::channel::<Msg>();
        let repo = repos.get_mut(id).expect("its repository");
        repo.draft = typed("wip\n\nthe body");
        repo.suggest(ask, "api".into(), &tx).expect("starts");
        assert!(repo.suggesting(), "the box says so");
        let Ok(Msg::Suggested(from, result)) = answers.recv_timeout(WAIT) else {
            panic!("the agent's answer");
        };
        assert_eq!(from, id);
        let applied = repos.suggested(id, result, Some(id), Instant::now());
        assert!(applied.dirty && applied.toast.is_none());
        let repo = repos.get(id).expect("its repository");
        assert_eq!(
            repo.draft,
            typed("feat: add a\n\nthe body"),
            "the body stays"
        );
        assert!(repo.suggestion.is_none() && !repo.suggesting());
        let late = repos.suggested(id, Ok("fix: b".into()), Some(id), Instant::now());
        assert!(!late.dirty, "none runs");
        assert!(
            !repos
                .suggested(RepoId(9), Ok("fix: b".into()), None, Instant::now())
                .dirty,
            "an unknown repository"
        );
    }

    #[test]
    fn a_failed_suggestion_leaves_the_draft_and_says_why() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        // Its own answer is never read; the test gives one.
        let (tx, _answers) = mpsc::channel::<Msg>();
        let bin = TempDir::new().expect("dir");
        let repo = repos.get_mut(id).expect("its repository");
        repo.draft = typed("mine");
        repo.suggest(missing_agent(&bin), "api".into(), &tx)
            .expect("starts");
        let error = || Err(anyhow::anyhow!("Not logged in · Please run /login"));
        let applied = repos.suggested(id, error(), Some(id), Instant::now());
        let toast = applied.toast.expect("a notice");
        assert_eq!(
            toast.text,
            "suggestion failed: Not logged in · Please run /login"
        );
        assert_eq!(toast.kind, toast::Kind::Error);
        let repo = repos.get_mut(id).expect("its repository");
        assert_eq!(repo.draft, typed("mine"));
        assert!(repo.suggestion.is_none());
        // While no session using it is shown, the project comes first.
        repo.suggest(missing_agent(&bin), "api".into(), &tx)
            .expect("starts");
        let applied = repos.suggested(id, error(), None, Instant::now());
        assert_eq!(
            applied.toast.map(|toast| toast.text).as_deref(),
            Some("api: suggestion failed: Not logged in · Please run /login")
        );
    }

    #[test]
    fn a_commit_a_push_or_a_subject_keeps_the_repository_busy() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let (tx, _answers) = mpsc::channel::<Msg>();
        let bin = TempDir::new().expect("dir");
        let repo = repos.get_mut(id).expect("its repository");
        assert!(!repo.busy());
        repo.in_flight = Some(typed("fix"));
        assert!(repo.busy(), "a commit");
        repo.in_flight = None;
        repo.pushing = Some(Pushing {
            push: Push::Track {
                branch: "main".into(),
            },
            project: "api".into(),
        });
        assert!(repo.busy(), "a push");
        repo.pushing = None;
        repo.suggest(missing_agent(&bin), "api".into(), &tx)
            .expect("starts");
        assert!(repo.busy(), "a subject");
        let failed = Err(anyhow::anyhow!("no agent"));
        repos.suggested(id, failed, Some(id), Instant::now());
        assert!(!repos.get(id).expect("its repository").busy());
    }

    #[test]
    fn a_commit_stops_the_suggestion_and_its_answer_goes_nowhere() {
        let dir = TempDir::new().expect("dir");
        staged_repo(&dir);
        let bin = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let (tx, answers) = mpsc::channel::<Msg>();
        let repo = repos.get_mut(id).expect("its repository");
        // Committed once the agent runs, what is staged read already.
        let started = bin.path().join("started");
        let script = format!("cat > /dev/null\ntouch '{}'\nsleep 30", started.display());
        repo.suggest(fake_agent(&bin, &script), "api".into(), &tx)
            .expect("starts");
        let deadline = Instant::now() + WAIT;
        while !started.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        repo.draft = typed("fix: mine");
        repo.commit("fix: mine\n".into());
        assert!(!repo.suggesting(), "the box no longer waits for it");
        assert!(repo.suggestion.is_some(), "one at a time until it ends");
        let Ok(Msg::Suggested(_, result)) = answers.recv_timeout(WAIT) else {
            panic!("the stopped run's answer, well before the agent's end");
        };
        assert_eq!(
            format!("{:#}", result.expect_err("stopped")),
            "claude was stopped"
        );
        let applied = repos.suggested(id, Ok("feat: late".into()), Some(id), Instant::now());
        assert!(applied.dirty, "a frame over what the agent wrote");
        assert!(applied.toast.is_none());
        let repo = repos.get(id).expect("its repository");
        assert!(repo.draft.is_empty(), "the next draft starts empty");
        assert!(repo.suggestion.is_none());
    }

    #[test]
    fn suggestions_stop_on_quit_and_with_their_repository() {
        let dirs = [(); 2].map(|()| {
            let dir = TempDir::new().expect("dir");
            staged_repo(&dir);
            dir
        });
        let bin = TempDir::new().expect("dir");
        let (tx, answers) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
        let ids = [0, 1].map(|n| repos.get_or_spawn(dirs[n].path(), &tx).expect("spawn"));
        for id in ids {
            let repo = repos.get_mut(id).expect("its repository");
            repo.suggest(fake_agent(&bin, HANGS), "api".into(), &tx)
                .expect("starts");
        }
        let started = Instant::now();
        repos.stop_suggestions();
        repos.await_suggestions(started + WAIT);
        assert!(started.elapsed() < WAIT, "not after the agents' end");
        for id in ids {
            let suggestion = repos.get(id).and_then(|repo| repo.suggestion.as_ref());
            assert!(suggestion.expect("not applied yet").run.ended());
        }
        // A repository let go stops its own.
        let id = ids[0];
        let repo = repos.get_mut(id).expect("its repository");
        repo.suggest(fake_agent(&bin, HANGS), "api".into(), &tx)
            .expect("starts");
        repos.release_unused(|_| false);
        assert!(repos.get(id).is_none());
        // The workers answer on the same channel.
        let deadline = Instant::now() + WAIT;
        let mut stopped = 0;
        while stopped < 3 {
            let left = deadline.saturating_duration_since(Instant::now());
            match answers.recv_timeout(left) {
                Ok(Msg::Suggested(_, result)) => {
                    let error = result.expect_err("stopped");
                    assert_eq!(format!("{error:#}"), "claude was stopped");
                    stopped += 1;
                }
                Ok(_) => {}
                Err(error) => panic!("{stopped} answers: {error}"),
            }
        }
    }

    #[test]
    fn an_unused_repository_goes_once_its_commit_ended() {
        let dirs = [(); 3].map(|()| TempDir::new().expect("dir"));
        let (tx, _rx) = mpsc::channel::<Msg>();
        let mut repos = Repos::new(POLL_EVERY);
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
        let mut poll = Poll::new(POLL_EVERY);
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
        poll.done(Read::Refresh, t1, false);
        assert_eq!(poll.due(t1 + POLL_EVERY - ms, Shown::Changes), None);
        assert_eq!(poll.due(t1 + POLL_EVERY, Shown::Changes), refresh);
        assert_eq!(poll.due(t1 + POLL_EVERY, Shown::Nowhere), None);
        // The refresh read the status too: the list waits its interval.
        assert_eq!(poll.due(t1 + STATUS_EVERY - ms, Shown::List), None);
        assert_eq!(poll.due(t1 + STATUS_EVERY, Shown::List), status);
        poll.sent(Read::Status);
        let t2 = t1 + STATUS_EVERY;
        poll.done(Read::Status, t2, false);
        assert_eq!(poll.due(t2 + STATUS_EVERY - ms, Shown::List), None);
        assert_eq!(poll.due(t2 + STATUS_EVERY, Shown::List), status);
        // A status read does not move the refresh's interval.
        assert_eq!(poll.due(t2, Shown::Changes), refresh);
    }

    #[test]
    fn a_read_answers_what_it_reads() {
        let t0 = Instant::now();
        let mut poll = Poll::new(POLL_EVERY);
        // The worker folded a status queued with a write's refresh into it.
        poll.sent(Read::Status);
        poll.done(Read::Refresh, t0, false);
        assert_eq!(poll.in_flight, None);
        // A status does not answer a refresh.
        poll.sent(Read::Refresh);
        poll.done(Read::Status, t0, false);
        assert_eq!(poll.in_flight, Some(Read::Refresh));
        poll.done(Read::Refresh, t0, false);
        assert_eq!(poll.in_flight, None);
    }

    #[test]
    fn a_repository_shown_again_is_refreshed_at_once() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        let refresh = Some(Read::Refresh);
        assert!(!repo.fresh(), "not read yet");
        // A refresh in flight stands for it.
        repo.poll.sent(Read::Refresh);
        repo.refresh_soon();
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "one at a time");
        repo.poll.done(Read::Refresh, now, false);
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "it ran");
        assert!(repo.fresh());
        repo.refresh_soon();
        assert!(!repo.fresh(), "until the refresh ends");
        assert_eq!(repo.poll.due(now, Shown::Changes), refresh);
        assert_eq!(repo.poll.due(now, Shown::Nowhere), None, "not shown");
        // Shown while its status is read for the list: the refresh follows
        // as soon as the status is in.
        repo.poll.done(Read::Refresh, now, false);
        repo.poll.sent(Read::Status);
        repo.refresh_soon();
        assert_eq!(repo.poll.due(now, Shown::Changes), None, "one at a time");
        repo.poll.done(Read::Status, now, false);
        assert!(!repo.fresh(), "a status read is not enough");
        assert_eq!(repo.poll.due(now, Shown::Changes), refresh);
        // Hidden with the side panes, or on the list alone, it gets no
        // refresh: its snapshot gets old, and is read again at once when
        // it shows again.
        for shown_in in [Shown::Nowhere, Shown::List] {
            repo.refreshed(now);
            repo.read_when_due(now, Shown::Changes);
            assert!(repo.fresh(), "refreshed just now");
            repo.read_when_due(now, shown_in);
            assert!(!repo.fresh(), "{shown_in:?}");
            assert_eq!(repo.poll.due(now, Shown::Changes), refresh);
        }
    }

    #[test]
    fn a_change_makes_a_refresh_due_once_no_read_is_in_flight() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let refresh = Some(Read::Refresh);
        let mut poll = Poll::new(POLL_EVERY);
        poll.sent(Read::Refresh);
        poll.done(Read::Refresh, t0, true);
        assert_eq!(poll.due(t0 + POLL_EVERY - ms, Shown::Changes), None);
        // Due AFTER_CHANGE after the first change seen.
        let t1 = t0 + Duration::from_secs(1);
        poll.saw_change(t1);
        poll.saw_change(t1 + ms * 50);
        assert_eq!(poll.due(t1 + AFTER_CHANGE - ms, Shown::Changes), None);
        assert_eq!(poll.due(t1 + AFTER_CHANGE, Shown::Changes), refresh);
        // Read for the changes pane alone.
        assert_eq!(poll.due(t1 + AFTER_CHANGE, Shown::List), None);
        assert_eq!(poll.due(t1 + AFTER_CHANGE, Shown::Nowhere), None);
        // Not while a read is in flight; one seen meanwhile waits for it.
        poll.sent(Read::Refresh);
        let t2 = t1 + AFTER_CHANGE + ms * 20;
        poll.saw_change(t2);
        assert_eq!(poll.due(t2 + AFTER_CHANGE * 5, Shown::Changes), None);
        let t3 = t2 + ms * 300;
        poll.done(Read::Refresh, t3, true);
        // Then the gap after its end.
        assert_eq!(poll.due(t3 + CHANGE_GAP - ms, Shown::Changes), None);
        assert_eq!(poll.due(t3 + CHANGE_GAP, Shown::Changes), refresh);
        // With no change waiting, the poll alone.
        poll.sent(Read::Refresh);
        poll.done(Read::Refresh, t3 + CHANGE_GAP, false);
        let t4 = t3 + CHANGE_GAP;
        assert_eq!(poll.due(t4 + POLL_EVERY - ms, Shown::Changes), None);
        assert_eq!(poll.due(t4 + POLL_EVERY, Shown::Changes), refresh);
    }

    #[test]
    fn the_gap_doubles_while_changes_read_nothing_new() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let refresh = Some(Read::Refresh);
        let mut poll = Poll::new(POLL_EVERY);
        poll.sent(Read::Refresh);
        poll.done(Read::Refresh, t0, true);
        // Each change seen as the last refresh ends; the same snapshot
        // read, or a failure.
        let mut end = t0;
        for gap in [250, 500, 1000, 2000, 2000].map(Duration::from_millis) {
            poll.saw_change(end);
            assert_eq!(poll.due(end + gap - ms, Shown::Changes), None, "{gap:?}");
            assert_eq!(poll.due(end + gap, Shown::Changes), refresh, "{gap:?}");
            poll.sent(Read::Refresh);
            end += gap + ms * 120;
            poll.done(Read::Refresh, end, false);
        }
        // Something new sets it back.
        poll.saw_change(end);
        poll.sent(Read::Refresh);
        end += ms * 120;
        poll.done(Read::Refresh, end, true);
        assert_eq!(poll.gap, CHANGE_GAP);
        // POLL_EVERY made it due while a change waited: the change's.
        poll.saw_change(end + POLL_EVERY - ms * 50);
        assert_eq!(poll.due(end + POLL_EVERY - ms, Shown::Changes), None);
        assert_eq!(poll.due(end + POLL_EVERY, Shown::Changes), refresh);
        poll.sent(Read::Refresh);
        end += POLL_EVERY + ms * 120;
        poll.done(Read::Refresh, end, false);
        assert_eq!(poll.gap, CHANGE_GAP * 2);
        // A refresh sent with no change waiting sets it back too.
        poll.sent(Read::Refresh);
        poll.done(Read::Refresh, end + POLL_EVERY, false);
        assert_eq!(poll.gap, CHANGE_GAP);
        // One not sent here moves it only when it read something new.
        poll.gap = CHANGE_GAP * 4;
        poll.done(Read::Refresh, end + POLL_EVERY, false);
        assert_eq!(poll.gap, CHANGE_GAP * 4, "the one after a write");
        poll.done(Read::Refresh, end + POLL_EVERY, true);
        assert_eq!(poll.gap, CHANGE_GAP);
    }

    #[test]
    fn the_poll_follows_its_interval_and_the_gap_stops_at_two_seconds_or_it() {
        let ms = Duration::from_millis(1);
        let refresh = Some(Read::Refresh);
        for (every, gaps) in [
            (5, &[250, 500, 1000, 2000, 2000][..]),
            // The poll comes before a gap of 2 s.
            (1, &[250, 500, 1000, 1000]),
            (60, &[250, 500, 1000, 2000, 2000]),
        ] {
            let every = Duration::from_secs(every);
            let t0 = Instant::now();
            let mut poll = Poll::new(every);
            poll.sent(Read::Refresh);
            poll.done(Read::Refresh, t0, true);
            // With no change, the interval alone.
            assert_eq!(poll.due(t0 + every - ms, Shown::Changes), None, "{every:?}");
            assert_eq!(poll.due(t0 + every, Shown::Changes), refresh, "{every:?}");
            let mut end = t0;
            for &gap in gaps {
                let gap = Duration::from_millis(gap);
                poll.saw_change(end);
                assert_eq!(poll.due(end + gap - ms, Shown::Changes), None, "{gap:?}");
                assert_eq!(poll.due(end + gap, Shown::Changes), refresh, "{gap:?}");
                poll.sent(Read::Refresh);
                end += gap + ms * 120;
                poll.done(Read::Refresh, end, false);
            }
        }
    }

    #[test]
    fn a_change_waits_no_more_once_a_refresh_is_due_at_once_or_the_pane_goes() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        repo.refreshed(now);
        let applied = repo.apply(Event::Changed, true, now);
        assert!(!applied.dirty && applied.toast.is_none());
        assert_eq!(repo.poll.change, Some(now));
        repo.refresh_soon();
        assert_eq!(repo.poll.change, None);
        // Kept while a refresh in flight may not have read it.
        repo.poll.sent(Read::Refresh);
        repo.apply(Event::Changed, true, now);
        repo.refresh_soon();
        assert_eq!(repo.poll.change, Some(now));
        // Leaving the changes pane drops it, also while a refresh is in
        // flight.
        for shown_in in [Shown::List, Shown::Nowhere] {
            repo.apply(Event::Changed, true, now);
            repo.read_when_due(now, shown_in);
            assert_eq!(repo.poll.change, None, "{shown_in:?}");
        }
    }

    #[test]
    fn a_refresh_for_a_change_doubles_the_gap_unless_it_read_something_new() {
        let dir = TempDir::new().expect("dir");
        let (mut repos, id, _rx) = one_repo(&dir);
        let repo = repos.get_mut(id).expect("its repository");
        let now = Instant::now();
        repo.snapshot = Some(modified(&["a.txt"]));
        let read = |paths: &[&str]| Event::Refreshed(Ok(Box::new(modified(paths))));
        // A failed refresh read nothing new, nor did one that read the
        // snapshot there was; one that read another sets the gap back.
        for (event, gap) in [
            (
                Event::Refreshed(Err(anyhow::anyhow!("index.lock"))),
                CHANGE_GAP * 2,
            ),
            (read(&["a.txt"]), CHANGE_GAP * 4),
            (read(&["b.txt"]), CHANGE_GAP),
        ] {
            repo.apply(Event::Changed, true, now);
            repo.poll.sent(Read::Refresh);
            repo.apply(event, true, now);
            assert_eq!(repo.poll.gap, gap);
        }
    }

    #[test]
    fn the_repository_shown_is_watched_and_read_after_a_change() {
        let dir = TempDir::new().expect("dir");
        git::tests::init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git::tests::git(dir.path(), &["add", "a.txt"]);
        git::tests::git(dir.path(), &["commit", "-q", "-m", "one"]);
        let (mut repos, id, rx) = one_repo(&dir);
        let shown = [Some(id), None];
        // The next event, applied, and whether it was a refresh or a
        // change; other events are applied on the way.
        let next = |repos: &mut Repos| loop {
            let Ok(Msg::Git(from, event)) = rx.recv_timeout(WAIT) else {
                panic!("the worker answers");
            };
            assert_eq!(from, id);
            let kind = match event {
                Event::Refreshed(_) => "refreshed",
                Event::Changed => "changed",
                _ => "",
            };
            repos.apply(id, event, true, Instant::now());
            if !kind.is_empty() {
                break kind;
            }
        };
        // Nothing more until the next refresh begins: the rest of a
        // write's events come and go meanwhile.
        let quiet = || {
            matches!(
                rx.recv_timeout(git::tests::QUIET),
                Err(mpsc::RecvTimeoutError::Timeout)
            )
        };
        // Watched before the first refresh.
        repos.read_when_due(Instant::now(), shown, true, |_| false);
        assert_eq!(next(&mut repos), "refreshed");
        std::fs::write(dir.path().join("b.txt"), "b\n").expect("write");
        assert_eq!(next(&mut repos), "changed");
        assert!(quiet(), "one change until the next refresh");
        // Due within the gap, well before the poll: sent for the change.
        let sent = |repos: &Repos| {
            let poll = &repos.get(id).expect("its repo").poll;
            poll.in_flight == Some(Read::Refresh) && poll.for_change
        };
        repos.read_when_due(Instant::now() + CHANGE_GAP, shown, true, |_| false);
        assert!(sent(&repos));
        assert_eq!(next(&mut repos), "refreshed");
        // A file touched with the same bytes changes, once: the refresh
        // it makes leaves the index alone.
        git::tests::stat_dirty(&dir.path().join("a.txt"));
        assert_eq!(next(&mut repos), "changed");
        assert!(quiet(), "one change until the next refresh");
        repos.read_when_due(Instant::now() + CHANGE_GAP, shown, true, |_| false);
        assert!(sent(&repos));
        assert_eq!(next(&mut repos), "refreshed");
        assert!(quiet(), "no change after the refresh");
    }
}
