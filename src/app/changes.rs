//! Whether the changes pane is on screen beside the projects pane: while
//! the repository it shows has something to commit or to push, while
//! something holds it, and for 10 s after.

use std::time::{Duration, Instant};

use super::repo::RepoId;
use super::session::SessionId;
use crate::git;
use crate::git::worker::Snapshot;
use crate::ui::{Mode, Pane};

/// How long the changes pane stays on screen once nothing needs it.
const HIDE_AFTER: Duration = Duration::from_secs(10);

/// What the changes pane shows: the session shown, and the repository its
/// changes pane shows, none outside any.
pub(super) type Key = (SessionId, Option<RepoId>);

/// What the changes pane needs on one pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Need {
    /// To be on screen.
    Shown,
    /// Nothing: it goes once [`HIDE_AFTER`] passed since it was last
    /// needed.
    Nothing,
    /// To stay as it is, its repository not refreshed since the pane
    /// came to show it, or since the side panes came back.
    Unread,
}

/// Whether `snapshot` has something for the changes pane: a path not
/// committed, staged or not, untracked or in conflict, or commits that
/// `p` would push, as the status bar offers them.
fn has_something(snapshot: &Snapshot) -> bool {
    let branch = &snapshot.status.branch;
    !snapshot.status.entries.is_empty()
        || git::push::plan(branch, snapshot.origin, snapshot.push_remote.as_deref()).is_ok()
}

/// Whether the changes pane is held on screen, whatever its repository
/// has: `mode` highlights it or has its commit box open, a `diff` shows
/// in it, or its repository is `busy` with a commit, a push or a subject
/// the agent writes.
pub(super) fn held(mode: Mode, diff: bool, busy: bool) -> bool {
    mode.pane() == Pane::Changes || diff || busy
}

/// What the changes pane needs: to be on screen while `held`; else, with
/// a `repo` to show, as `snapshot` says it stands: on screen while it has
/// something, as [`has_something`] says, nothing once it has none, and
/// to stay as it is without a snapshot to go by, before a read; nothing
/// outside any repository.
pub(super) fn need(repo: bool, snapshot: Option<&Snapshot>, held: bool) -> Need {
    match snapshot {
        _ if held => Need::Shown,
        _ if !repo => Need::Nothing,
        None => Need::Unread,
        Some(snapshot) if has_something(snapshot) => Need::Shown,
        Some(_) => Need::Nothing,
    }
}

/// Whether the changes pane is on screen while the side panes are, and
/// since when nothing needs it. Off at first, until the repository it
/// shows is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Showing {
    /// Whether it is on screen.
    on: bool,
    /// What it showed on the last pass.
    key: Option<Key>,
    /// When it was last needed; none since it showed something else.
    needed: Option<Instant>,
}

impl Showing {
    /// Shows the pane for `key` at `now`, as if it were needed then.
    pub(super) fn show(&mut self, key: Key, now: Instant) {
        self.on = true;
        self.key = Some(key);
        self.needed = Some(now);
    }

    /// Follows what the pane needs, `need`, at `now`, showing `key`: on
    /// screen while it is needed; once nothing needs it, off when
    /// [`HIDE_AFTER`] passed since it was last needed, at once when that
    /// was before it showed `key`; as it is while its repository was not
    /// read. Returns whether it is on screen.
    pub(super) fn update(&mut self, key: Key, need: Need, now: Instant) -> bool {
        if self.key != Some(key) {
            self.key = Some(key);
            self.needed = None;
        }
        match need {
            Need::Shown => {
                self.on = true;
                self.needed = Some(now);
            }
            Need::Nothing => {
                self.on = self
                    .needed
                    .is_some_and(|needed| now.duration_since(needed) < HIDE_AFTER);
            }
            Need::Unread => {}
        }
        self.on
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::session::Sessions;
    use crate::git::status::{Branch, Change, Entry};

    /// A snapshot of a repository on `main`, with an upstream and `ahead`
    /// commits it lacks, and `paths` changed in the work tree, each with
    /// `change`.
    fn read(ahead: u32, paths: &[&str], change: Change) -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.status.branch = Branch {
            head: Some("main".into()),
            oid: Some("abc1234".into()),
            upstream: Some("origin/main".into()),
            ahead_behind: Some((ahead, 0)),
        };
        snapshot.status.entries = paths
            .iter()
            .map(|path| Entry {
                path: path.into(),
                original: None,
                index: None,
                worktree: Some(change),
                submodule: false,
            })
            .collect();
        snapshot
    }

    /// A clean repository, pushed.
    fn clean() -> Snapshot {
        read(0, &[], Change::Modified)
    }

    /// The key of session 0 or 1, from [`Sessions`], with repository
    /// `repo` or none.
    fn keys() -> impl Fn(usize, Option<u32>) -> Key {
        let mut sessions = Sessions::new();
        let ids = [sessions.next_id(), sessions.next_id()];
        move |session, repo| (ids[session], repo.map(RepoId::for_tests))
    }

    #[test]
    fn a_file_not_committed_or_a_commit_to_push_is_something() {
        assert!(!has_something(&clean()));
        for change in [Change::Modified, Change::Untracked, Change::Unmerged] {
            assert!(has_something(&read(0, &["a.txt"], change)), "{change:?}");
        }
        let mut staged = clean();
        staged.status.entries = vec![Entry {
            path: "a.txt".into(),
            original: None,
            index: Some(Change::Added),
            worktree: None,
            submodule: false,
        }];
        assert!(has_something(&staged));
        // Ahead only: what `p` would push.
        assert!(has_something(&read(2, &[], Change::Modified)));
        // A branch without an upstream goes to origin, when there is one.
        let mut untracked_branch = clean();
        untracked_branch.status.branch.upstream = None;
        untracked_branch.status.branch.ahead_behind = None;
        assert!(!has_something(&untracked_branch), "no origin");
        untracked_branch.origin = true;
        assert!(has_something(&untracked_branch));
        // An upstream gone, which git counts no distance to: `p` offers
        // the push.
        let mut gone = clean();
        gone.status.branch.ahead_behind = None;
        assert!(has_something(&gone));
        // Before the first commit, or on a detached HEAD, nothing to push.
        let mut unborn = clean();
        unborn.status.branch.oid = None;
        assert!(!has_something(&unborn));
        let mut detached = read(1, &[], Change::Modified);
        detached.status.branch.head = None;
        assert!(!has_something(&detached));
    }

    #[test]
    fn the_highlight_the_box_a_diff_and_what_runs_hold_the_pane() {
        assert!(held(Mode::Term(Pane::Changes), false, false));
        assert!(held(Mode::Commit, false, false));
        assert!(held(Mode::Agent, true, false), "a diff");
        assert!(held(Mode::Agent, false, true), "a commit, push or subject");
        for mode in [
            Mode::Agent,
            Mode::Picker,
            Mode::Term(Pane::Agent),
            Mode::Term(Pane::Projects),
        ] {
            assert!(!held(mode, false, false), "{mode:?}");
        }
    }

    #[test]
    fn the_pane_needs_what_its_repository_has_unless_held() {
        let dirty = read(0, &["a.txt"], Change::Modified);
        assert_eq!(need(true, Some(&dirty), false), Need::Shown);
        assert_eq!(need(true, Some(&clean()), false), Need::Nothing);
        assert_eq!(need(true, None, false), Need::Unread);
        assert_eq!(need(false, None, false), Need::Nothing, "no repository");
        for (repo, snapshot) in [(true, Some(&clean())), (true, None), (false, None)] {
            assert_eq!(need(repo, snapshot, true), Need::Shown, "held");
        }
    }

    #[test]
    fn it_shows_at_once_and_hides_after_ten_seconds_of_nothing() {
        let key = keys();
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let mut pane = Showing::default();
        let here = key(0, Some(0));
        assert!(!pane.update(here, Need::Unread, t0), "off until read");
        assert!(pane.update(here, Need::Shown, t0), "at once");
        // Nothing from now on: it stays 10 s, then goes.
        let t1 = t0 + Duration::from_secs(3);
        assert!(pane.update(here, Need::Shown, t1));
        assert!(pane.update(here, Need::Nothing, t1 + ms));
        assert!(pane.update(here, Need::Nothing, t1 + HIDE_AFTER - ms));
        assert!(!pane.update(here, Need::Nothing, t1 + HIDE_AFTER));
        // Something again: back at once; a read on its way changes
        // nothing.
        let t2 = t1 + HIDE_AFTER * 2;
        assert!(!pane.update(here, Need::Unread, t2));
        assert!(pane.update(here, Need::Shown, t2));
        assert!(pane.update(here, Need::Unread, t2 + HIDE_AFTER * 2));
        // A file that comes and goes within the 10 s keeps it: the count
        // starts again from the last time something was there.
        let seconds = Duration::from_secs;
        let t3 = t2 + HIDE_AFTER * 3;
        assert!(pane.update(here, Need::Shown, t3));
        assert!(pane.update(here, Need::Nothing, t3 + seconds(6)));
        assert!(pane.update(here, Need::Shown, t3 + seconds(8)));
        assert!(pane.update(here, Need::Nothing, t3 + seconds(17)));
        assert!(!pane.update(here, Need::Nothing, t3 + seconds(18)));
    }

    #[test]
    fn another_session_or_repository_shows_its_own_state_at_once() {
        let key = keys();
        let t0 = Instant::now();
        let mut pane = Showing::default();
        let (here, there) = (key(0, Some(0)), key(1, Some(1)));
        assert!(pane.update(here, Need::Shown, t0));
        assert!(!pane.update(there, Need::Nothing, t0), "no hold from here");
        assert!(pane.update(here, Need::Shown, t0));
        assert!(pane.update(there, Need::Unread, t0), "as it was until read");
        assert!(!pane.update(there, Need::Nothing, t0));
        assert!(!pane.update(there, Need::Unread, t0));
        // The same repository in another session, and another repository
        // in the same session, count as another.
        assert!(pane.update(here, Need::Shown, t0));
        assert!(!pane.update(key(1, Some(0)), Need::Nothing, t0));
        assert!(pane.update(here, Need::Shown, t0));
        assert!(!pane.update(key(0, Some(2)), Need::Nothing, t0));
        // Outside any repository it goes.
        assert!(pane.update(here, Need::Shown, t0));
        assert!(!pane.update(key(0, None), Need::Nothing, t0));
    }

    #[test]
    fn showing_it_by_hand_starts_the_ten_seconds() {
        let key = keys();
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let mut pane = Showing::default();
        let here = key(0, Some(0));
        assert!(!pane.update(here, Need::Nothing, t0));
        pane.show(here, t0);
        assert!(pane.update(here, Need::Nothing, t0 + HIDE_AFTER - ms));
        assert!(!pane.update(here, Need::Nothing, t0 + HIDE_AFTER));
        // Shown for another repository, it is that one's hold.
        let there = key(0, Some(1));
        pane.show(there, t0);
        assert!(pane.update(there, Need::Nothing, t0 + ms));
        // Before the read, it stays past the 10 s.
        pane.show(here, t0);
        assert!(pane.update(here, Need::Unread, t0 + HIDE_AFTER * 2));
    }
}
