//! Opening a project from the picker: the directory chosen, made
//! canonical; its repository, from the finder and cached per directory;
//! and a session started there and shown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::repo::Repos;
use super::session::{Place, SessionId, Sessions, Starter};
use crate::Size;
use crate::git::find::Found;
use crate::strings;
use crate::ui::picker::Choice;

/// Where a choice in the picker leads.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// A session already open.
    Session(SessionId),
    /// A directory no session runs in, canonical.
    Dir(PathBuf),
}

/// Where `choice` leads: the session it names, or the directory it names
/// made canonical, which is a session's when one runs there. `~` stands
/// for `home`, and a relative path counts from it. Fails when the
/// directory cannot be opened.
pub(super) fn target(
    choice: Choice<'_>,
    sessions: &Sessions,
    home: Option<&Path>,
) -> Result<Target> {
    let path = match choice {
        Choice::Item(item) => {
            if item.open
                && let Some(id) = sessions.find(&item.dir)
            {
                return Ok(Target::Session(id));
            }
            item.dir.as_path()
        }
        Choice::Typed(text) => Path::new(text),
    };
    let dir = resolve_dir(path, home)?;
    Ok(sessions
        .find(&dir)
        .map_or(Target::Dir(dir), Target::Session))
}

/// The directory `path` names, canonical: `~` is `home`, and a path
/// under `~` or a relative one counts from it. Fails when there is no
/// home to count from, when nothing is there, or when it is not a
/// directory.
fn resolve_dir(path: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let home = home.context("no home directory to start from")?;
        home.join(path.strip_prefix("~").unwrap_or(path))
    };
    let dir = full
        .canonicalize()
        .with_context(|| format!("open {}", path.display()))?;
    ensure!(dir.is_dir(), "{} is not a directory", path.display());
    Ok(dir)
}

/// The repository roots the finder found, by directory, and the
/// directories waiting for an answer, each with whether its session
/// starts on a new conversation.
#[derive(Debug, Default)]
pub(super) struct Roots {
    found: HashMap<PathBuf, Option<PathBuf>>,
    waiting: Vec<(PathBuf, bool)>,
}

/// What [`Roots::lookup`] knows of a directory's repository.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Lookup {
    /// Its root; none outside a repository.
    Found(Option<PathBuf>),
    /// Nothing yet: the finder is to be asked.
    Ask,
    /// The finder was asked already.
    Waiting,
}

impl Roots {
    /// What is known of the repository of `dir`. When nothing is, `dir`
    /// waits for the finder's answer, its session to start on a new
    /// conversation when `new_conversation`; a directory already waiting
    /// keeps its first choice.
    pub(super) fn lookup(&mut self, dir: &Path, new_conversation: bool) -> Lookup {
        if let Some(root) = self.found.get(dir) {
            return Lookup::Found(root.clone());
        }
        if self.waiting.iter().any(|(waiting, _)| waiting == dir) {
            return Lookup::Waiting;
        }
        self.waiting.push((dir.to_path_buf(), new_conversation));
        Lookup::Ask
    }

    /// Notes the finder's answer, kept unless git failed. Returns whether
    /// the session of the directory that waited for it starts on a new
    /// conversation; none when no directory waited.
    pub(super) fn answer(&mut self, found: &Found) -> Option<bool> {
        if let Ok(root) = &found.root {
            self.found.insert(found.dir.clone(), root.clone());
        }
        let at = self
            .waiting
            .iter()
            .position(|(waiting, _)| *waiting == found.dir)?;
        Some(self.waiting.remove(at).1)
    }

    /// Lets the directories waiting go: their answers, when they come,
    /// are only kept.
    pub(super) fn forget_waiting(&mut self) {
        self.waiting.clear();
    }
}

/// Starts a session in `place` with the agent at `size`, on a new
/// conversation when `new_conversation`, its changes pane on the
/// repository of `place`, started if no session uses it yet, and shows
/// it. The agent gets none of the arguments given after `--`, which are
/// the first session's. Returns why the session runs without telemetry,
/// when it does; or why it could not start, naming the project, with
/// nothing left of it: no directory, no entry with the panic hook, no
/// repository started for it.
pub(super) fn open(
    starter: &Starter<'_>,
    sessions: &mut Sessions,
    repos: &mut Repos,
    place: Place,
    new_conversation: bool,
    size: Size,
) -> Result<Option<String>, String> {
    let project = place.project.clone();
    let failed = |error: anyhow::Error| strings::session_trouble(&project, &format!("{error:#}"));
    let root = place.repo.clone();
    let id = sessions.next_id();
    let (mut session, notice) = starter
        .start(id, place, Vec::new(), new_conversation, size)
        .map_err(failed)?;
    if let Some(root) = root {
        match repos.get_or_spawn(&root, starter.tx) {
            Ok(repo) => session.repo = Some(repo),
            Err(error) => {
                session.discard();
                return Err(failed(error));
            }
        }
    }
    sessions.push(session);
    show(sessions, repos, id);
    Ok(notice.map(|reason| strings::telemetry_trouble(&reason)))
}

/// Shows session `id`, with its repository's refresh due at once.
/// Returns whether the session shown changed.
pub(super) fn show(sessions: &mut Sessions, repos: &mut Repos, id: SessionId) -> bool {
    if !sessions.show(id) {
        return false;
    }
    if let Some(repo) = sessions.shown().repo.and_then(|repo| repos.get_mut(repo)) {
        repo.refresh_soon();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::session::tests::Fake;
    use crate::instance::Root;
    use crate::pty;
    use crate::ui::picker::Item;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::time::Instant;
    use tempfile::TempDir;

    const SIZE: Size = Size { rows: 2, cols: 10 };

    #[test]
    fn a_typed_directory_counts_from_the_home_and_is_made_canonical() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let home = base.join("home");
        let project = home.join("dev/termis");
        fs::create_dir_all(&project).expect("mkdir");
        fs::write(home.join("notes.txt"), "").expect("write");
        symlink(&project, home.join("link")).expect("link");
        let resolve = |typed: &str| resolve_dir(Path::new(typed), Some(&home));
        assert_eq!(resolve("~").expect("the home"), home);
        assert_eq!(resolve("~/dev/termis").expect("under it"), project);
        assert_eq!(resolve("dev/termis").expect("relative"), project);
        assert_eq!(resolve("./dev/../dev/termis").expect("dots"), project);
        assert_eq!(resolve("~/..").expect("above it"), base);
        assert_eq!(resolve("link").expect("a link"), project, "followed");
        let absolute = project.to_str().expect("UTF-8");
        assert_eq!(resolve(absolute).expect("absolute"), project);
        let error = resolve("~/missing").expect_err("nothing there");
        assert!(
            format!("{error:#}").starts_with("open ~/missing: "),
            "{error:#}"
        );
        let error = resolve("notes.txt").expect_err("a file");
        assert_eq!(format!("{error:#}"), "notes.txt is not a directory");
        let error = resolve_dir(Path::new("dev"), None).expect_err("no home");
        assert_eq!(format!("{error:#}"), "no home directory to start from");
        assert_eq!(resolve_dir(Path::new(absolute), None).ok(), Some(project));
    }

    #[test]
    fn a_directory_waits_once_for_its_root_and_a_found_root_is_kept() {
        let mut roots = Roots::default();
        let (dir, other) = (Path::new("/p/a"), Path::new("/p/b"));
        assert_eq!(roots.lookup(dir, true), Lookup::Ask);
        assert_eq!(roots.lookup(dir, false), Lookup::Waiting, "asked once");
        assert_eq!(roots.lookup(other, false), Lookup::Ask);
        let found = |dir: &Path, root: Result<Option<PathBuf>>| Found {
            dir: dir.to_path_buf(),
            root,
        };
        // The first choice holds; the answer is kept.
        let root = Some(PathBuf::from("/p"));
        assert_eq!(roots.answer(&found(dir, Ok(root.clone()))), Some(true));
        assert_eq!(roots.lookup(dir, false), Lookup::Found(root.clone()));
        assert_eq!(roots.answer(&found(dir, Ok(root))), None, "none waits");
        // A failure is not kept: the next choice asks again.
        let failed = found(other, Err(anyhow::anyhow!("git is not installed")));
        assert_eq!(roots.answer(&failed), Some(false));
        assert_eq!(roots.lookup(other, true), Lookup::Ask);
        // Outside a repository is an answer too.
        assert_eq!(roots.answer(&found(other, Ok(None))), Some(true));
        assert_eq!(roots.lookup(other, false), Lookup::Found(None));
        // A directory let go of is asked again, and its answer only kept.
        let third = Path::new("/p/c");
        assert_eq!(roots.lookup(third, true), Lookup::Ask);
        roots.forget_waiting();
        assert_eq!(roots.lookup(third, false), Lookup::Ask, "asked again");
        roots.forget_waiting();
        assert_eq!(roots.answer(&found(third, Ok(None))), None, "none waits");
        assert_eq!(roots.lookup(third, true), Lookup::Found(None));
    }

    /// Opens a session of `agent` in `cwd`, inside `root` if any, as the
    /// picker does, holding the lock tests spawn under.
    fn open_in(
        starter: &Starter<'_>,
        sessions: &mut Sessions,
        repos: &mut Repos,
        cwd: &Path,
        root: Option<&Path>,
    ) -> Result<Option<String>, String> {
        let place = Place::new(cwd.to_path_buf(), root.map(Path::to_path_buf), None);
        let _spawn = pty::SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        open(starter, sessions, repos, place, false, SIZE)
    }

    #[test]
    fn sessions_in_one_repository_share_it_and_each_is_shown() {
        let temporary = TempDir::new().expect("temp dir");
        let root = temporary.path().join("repo");
        let (one, two) = (root.join("a"), root.join("b"));
        let outside = temporary.path().join("notes");
        for dir in [&one, &two, &outside] {
            fs::create_dir_all(dir).expect("mkdir");
        }
        let instances = Root::create_in(temporary.path(), "/bin/sh".into()).expect("creates");
        let (tx, _rx) = mpsc::channel::<Msg>();
        let agent = Fake("exec sleep 30");
        let starter = Starter {
            agent: &agent,
            instances: Ok(&instances),
            tx: &tx,
        };
        let (mut sessions, mut repos) = (Sessions::new(), Repos::new());
        for (cwd, root) in [(&one, Some(&root)), (&two, Some(&root)), (&outside, None)] {
            let notice = open_in(
                &starter,
                &mut sessions,
                &mut repos,
                cwd,
                root.map(|r| r.as_path()),
            );
            assert_eq!(notice, Ok(None));
            assert_eq!(sessions.shown().place.cwd, *cwd, "shown");
        }
        let repos_of: Vec<_> = sessions.iter().map(|session| session.repo).collect();
        assert!(
            repos_of[0].is_some() && repos_of[0] == repos_of[1],
            "{repos_of:?}"
        );
        assert_eq!(repos_of[2], None, "outside a repository");
        // Showing an open one, by its directory, makes its repository's
        // refresh due at once.
        let first = sessions.find(&one).expect("open");
        let repo = repos_of[0].expect("a repository");
        let now = Instant::now();
        let due = |repos: &mut Repos| repos.get_mut(repo).expect("its repo").refresh_due(now);
        repos.get_mut(repo).expect("its repo").refreshed(now);
        assert!(!due(&mut repos));
        assert!(show(&mut sessions, &mut repos, first));
        assert!(due(&mut repos));
        assert!(!show(&mut sessions, &mut repos, first), "shown already");
        assert_eq!(sessions.shown().place.cwd, one);
    }

    #[test]
    fn a_choice_leads_to_the_session_open_in_its_directory() {
        let temporary = TempDir::new().expect("temp dir");
        let home = temporary.path().canonicalize().expect("exists");
        let (open_dir, gone, new) = (home.join("a"), home.join("gone"), home.join("new"));
        for dir in [&open_dir, &gone, &new] {
            fs::create_dir(dir).expect("mkdir");
        }
        let link = home.join("link");
        symlink(&open_dir, &link).expect("link");
        let instances = Root::create_in(temporary.path(), "/bin/sh".into()).expect("creates");
        let (tx, _rx) = mpsc::channel::<Msg>();
        let agent = Fake("exec sleep 30");
        let starter = Starter {
            agent: &agent,
            instances: Ok(&instances),
            tx: &tx,
        };
        let (mut sessions, mut repos) = (Sessions::new(), Repos::new());
        for dir in [&open_dir, &gone] {
            let notice = open_in(&starter, &mut sessions, &mut repos, dir, None);
            assert_eq!(notice, Ok(None));
        }
        let in_a = sessions.find(&open_dir).expect("open");
        let in_gone = sessions.find(&gone).expect("open");
        fs::remove_dir(&gone).expect("rmdir");
        let home = Some(home.as_path());
        let target_of = |choice| target(choice, &sessions, home).expect("a target");
        // The row of an open session shows it, its directory gone or not.
        let row = Item::open("gone".into(), gone.clone(), home);
        assert_eq!(target_of(Choice::Item(&row)), Target::Session(in_gone));
        // A recent project or a typed path, made canonical, shows the
        // session open there.
        let linked = Item::recent("link".into(), link, home);
        assert_eq!(target_of(Choice::Item(&linked)), Target::Session(in_a));
        for typed in ["a", "~/a", "./a/../a", "link"] {
            assert_eq!(
                target_of(Choice::Typed(typed)),
                Target::Session(in_a),
                "{typed}"
            );
        }
        // Elsewhere it leads to the directory, canonical.
        let recent = Item::recent("new".into(), new.clone(), home);
        assert_eq!(target_of(Choice::Item(&recent)), Target::Dir(new.clone()));
        assert_eq!(target_of(Choice::Typed("~/new/../new")), Target::Dir(new));
        assert!(target(Choice::Typed("gone"), &sessions, home).is_err());
    }

    #[test]
    fn a_session_that_cannot_start_leaves_nothing_behind() {
        let temporary = TempDir::new().expect("temp dir");
        let root = temporary.path().join("repo");
        fs::create_dir(&root).expect("mkdir");
        let file = root.join("file");
        fs::write(&file, "").expect("write");
        let instances = Root::create_in(temporary.path(), "/bin/sh".into()).expect("creates");
        let (tx, _rx) = mpsc::channel::<Msg>();
        let agent = Fake("exec sleep 30");
        let starter = Starter {
            agent: &agent,
            instances: Ok(&instances),
            tx: &tx,
        };
        let (mut sessions, mut repos) = (Sessions::new(), Repos::new());
        let error = open_in(&starter, &mut sessions, &mut repos, &file, Some(&root))
            .expect_err("not a directory");
        assert!(error.starts_with("repo: start fake: "), "{error}");
        assert!(error.ends_with("is not a directory"), "{error}");
        assert_eq!(sessions.iter().count(), 0);
        let left = fs::read_dir(instances.dir()).expect("read").count();
        assert_eq!(left, 0, "its directory went");
        // No repository was started for it: one for another root is the
        // first.
        let next = repos.get_or_spawn(temporary.path(), &tx).expect("spawn");
        let first = Repos::new().get_or_spawn(temporary.path(), &tx).ok();
        assert_eq!(Some(next), first);
    }
}
