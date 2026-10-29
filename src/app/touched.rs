//! The repositories an agent writes in besides its session's own: those
//! of the files its tools write and of the directories it was given. The
//! repository of each directory comes from the finder, cached per
//! directory in [`Roots`], so git never runs on the main thread.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::open::{Lookup, Roots};
use super::repo::RepoId;
use super::session::{Sessions, project_name};
use crate::strings;

/// A repository an agent wrote in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Touched {
    /// The root of its working tree.
    pub(super) root: PathBuf,
    /// Its id among the repositories in use.
    pub(super) id: RepoId,
    /// Its name, the last component of its root.
    pub(super) name: String,
}

/// What a session's agent wrote in besides the session's own repository:
/// the repositories, and the directories whose repository is not known
/// yet.
#[derive(Debug, Default)]
pub(super) struct Writes {
    /// The repositories written in, the last written first, each once.
    pub(super) repos: Vec<Touched>,
    /// The directories written in whose repository is not known yet,
    /// canonical, in the order written.
    queue: VecDeque<PathBuf>,
    /// The directories the agent was given, as its status said last.
    given: Vec<PathBuf>,
}

impl Writes {
    /// Notes that the agent was `given` these directories and wrote
    /// `files`: each directory given that was not before, then the
    /// directory of each file, made canonical, waits for its repository
    /// in that order. One that is gone is passed over, and one just
    /// queued is not queued again.
    pub(super) fn wrote(&mut self, files: &[PathBuf], given: &[PathBuf]) {
        let new = given.iter().filter(|dir| !self.given.contains(dir));
        let parents = files.iter().filter_map(|file| file.parent());
        for dir in new.map(PathBuf::as_path).chain(parents) {
            let Ok(dir) = dir.canonicalize() else {
                continue;
            };
            if self.queue.back() != Some(&dir) {
                self.queue.push_back(dir);
            }
        }
        given.clone_into(&mut self.given);
    }

    /// Moves the queue on: each directory whose repository `roots` knows
    /// goes into the list as [`Writes::merge`] puts it there, with the
    /// session's `own` root and `spawn`; the first one it does not know
    /// stops the queue, after `ask` asked the finder about it unless it
    /// was asked before, so that the list keeps the order of the writes.
    /// Returns whether the list changed, or why a repository could not be
    /// listed, the directory that led to it gone from the queue.
    pub(super) fn resolve(
        &mut self,
        own: Option<&Path>,
        roots: &mut Roots,
        mut ask: impl FnMut(PathBuf),
        mut spawn: impl FnMut(&Path) -> Result<RepoId>,
    ) -> Result<bool, String> {
        let mut changed = false;
        while let Some(dir) = self.queue.front() {
            let root = match roots.written(dir) {
                Lookup::Found(root) => root,
                Lookup::Ask => {
                    ask(dir.clone());
                    break;
                }
                Lookup::Waiting => break,
            };
            self.queue.pop_front();
            if let Some(root) = root {
                changed |= self.merge(root, own, &mut spawn)?;
            }
        }
        Ok(changed)
    }

    /// Lets go of `dir` where it waits in the queue: git could not tell
    /// its repository.
    pub(super) fn forget(&mut self, dir: &Path) {
        self.queue.retain(|queued| queued != dir);
    }

    /// Puts the repository at `root` first in the list, from where it was
    /// or with the id `spawn` gives one not listed yet; one at the
    /// session's `own` root is left out. Returns whether the list changed,
    /// or why `spawn` gave no id, naming the repository.
    fn merge(
        &mut self,
        root: PathBuf,
        own: Option<&Path>,
        spawn: impl FnOnce(&Path) -> Result<RepoId>,
    ) -> Result<bool, String> {
        if own == Some(root.as_path()) {
            return Ok(false);
        }
        let touched = match self.repos.iter().position(|repo| repo.root == root) {
            Some(0) => return Ok(false),
            Some(at) => self.repos.remove(at),
            None => {
                let name = project_name(&root);
                let id = spawn(&root)
                    .map_err(|error| strings::for_project(&name, &format!("{error:#}")))?;
                Touched { root, id, name }
            }
        };
        self.repos.insert(0, touched);
        Ok(true)
    }

    /// Whether repository `id` is on the list.
    pub(super) fn has(&self, id: RepoId) -> bool {
        self.repos.iter().any(|repo| repo.id == id)
    }

    /// Takes repository `id` off the list, until a write lists it again.
    /// Returns whether it was there.
    pub(super) fn remove(&mut self, id: RepoId) -> bool {
        let listed = self.repos.len();
        self.repos.retain(|repo| repo.id != id);
        self.repos.len() < listed
    }
}

/// Moves the queue of each session of `sessions` on, as
/// [`Writes::resolve`] does with the root of the session's own
/// repository, `ask` and `spawn`. Returns whether a list changed, and why
/// a repository could not be listed, the last one when several could not.
pub(super) fn resolve(
    sessions: &mut Sessions,
    roots: &mut Roots,
    mut ask: impl FnMut(PathBuf),
    mut spawn: impl FnMut(&Path) -> Result<RepoId>,
) -> (bool, Option<String>) {
    let mut changed = false;
    let mut trouble = None;
    for session in sessions.iter_mut() {
        let own = session.place.repo.as_deref();
        match session.writes.resolve(own, roots, &mut ask, &mut spawn) {
            Ok(listed) => changed |= listed,
            Err(text) => trouble = Some(text),
        }
    }
    (changed, trouble)
}

/// Lets go of `dir` where it waits in the queue of each session of
/// `sessions`: git could not tell its repository.
pub(super) fn forget(sessions: &mut Sessions, dir: &Path) {
    for session in sessions.iter_mut() {
        session.writes.forget(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::repo::{POLL_EVERY, Repos};
    use crate::git::find::Finder;
    use crate::git::tests::init;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long the finder gets for an answer.
    const WAIT: Duration = Duration::from_secs(10);

    /// An id for each root, from a counter, and how many were given.
    struct Ids(u32);

    impl Ids {
        fn spawn(&mut self, _: &Path) -> Result<RepoId> {
            self.0 += 1;
            Ok(RepoId::for_tests(self.0))
        }
    }

    /// The roots on the list, in order.
    fn on_list(writes: &Writes) -> Vec<&Path> {
        writes
            .repos
            .iter()
            .map(|repo| repo.root.as_path())
            .collect()
    }

    #[test]
    fn the_last_written_comes_first_once_and_the_own_root_stays_out() {
        let mut writes = Writes::default();
        let mut ids = Ids(0);
        let own = Some(Path::new("/s"));
        let mut merge = |writes: &mut Writes, root: &str| {
            writes
                .merge(PathBuf::from(root), own, |root| ids.spawn(root))
                .expect("merges")
        };
        assert!(merge(&mut writes, "/a"));
        assert!(merge(&mut writes, "/b"));
        assert_eq!(on_list(&writes), [Path::new("/b"), Path::new("/a")]);
        assert!(merge(&mut writes, "/a"), "moved first");
        assert_eq!(on_list(&writes), [Path::new("/a"), Path::new("/b")]);
        assert!(!merge(&mut writes, "/a"), "first already");
        assert!(!merge(&mut writes, "/s"), "the session's own");
        assert_eq!(on_list(&writes), [Path::new("/a"), Path::new("/b")]);
        // An id for each repository when it is first listed.
        assert_eq!(ids.0, 2);
        let named: Vec<&str> = writes.repos.iter().map(|repo| repo.name.as_str()).collect();
        assert_eq!(named, ["a", "b"]);
        assert!(writes.has(RepoId::for_tests(1)) && !writes.has(RepoId::for_tests(3)));
        // One that gets no id is not listed, and says why by its name.
        let failed = writes.merge(PathBuf::from("/c"), own, |_| {
            Err(anyhow::anyhow!("no thread"))
        });
        assert_eq!(failed, Err("c: no thread".to_owned()));
        assert_eq!(writes.repos.len(), 2);
    }

    #[test]
    fn written_directories_wait_canonical_and_once_in_order() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let [a, b, given] = ["a", "b", "given"].map(|name| base.join(name));
        for dir in [&a, &b, &given] {
            fs::create_dir(dir).expect("mkdir");
        }
        let mut writes = Writes::default();
        // Through `..`, twice in a row, and in a directory that is gone.
        let files = [
            a.join("x"),
            b.join("../a/y"),
            b.join("z"),
            base.join("gone/w"),
        ];
        writes.wrote(&files, std::slice::from_ref(&given));
        assert_eq!(writes.queue, [given.clone(), a.clone(), b.clone()]);
        // A directory given before is not queued again.
        writes.wrote(&[], std::slice::from_ref(&given));
        assert_eq!(writes.queue.len(), 3);
        writes.forget(&a);
        assert_eq!(writes.queue, [given, b]);
    }

    /// Asks are counted; each answer of `finder`, through `rx`, goes to
    /// `roots`, until `writes` waits for nothing.
    fn settle(
        writes: &mut Writes,
        own: Option<&Path>,
        roots: &mut Roots,
        finder: &Finder,
        repos: &mut Repos,
        tx: &mpsc::Sender<Msg>,
        rx: &mpsc::Receiver<Msg>,
    ) -> usize {
        let mut asked = 0;
        loop {
            let ask = |dir| {
                asked += 1;
                finder.ask(dir);
            };
            writes
                .resolve(own, roots, ask, |root| repos.get_or_spawn(root, tx))
                .expect("resolves");
            if writes.queue.is_empty() {
                return asked;
            }
            match rx.recv_timeout(WAIT).expect("an answer") {
                Msg::Found(found) => {
                    roots.answer(&found);
                }
                Msg::Git(..) => {}
                _ => panic!("only the finder and the workers send here"),
            }
        }
    }

    #[test]
    fn the_repositories_of_the_files_come_from_the_finder_once_per_directory() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let (a, b) = (base.join("a"), base.join("b"));
        let inner = a.join("vendor/inner");
        let outside = base.join("notes");
        for dir in [&a.join("src"), &b, &inner, &outside] {
            fs::create_dir_all(dir).expect("mkdir");
        }
        init(&a);
        init(&b);
        init(&inner);
        let (tx, rx) = mpsc::channel::<Msg>();
        let finder = Finder::spawn(tx.clone(), Msg::Found).expect("spawns");
        let (mut roots, mut repos) = (Roots::default(), Repos::new(POLL_EVERY));
        let real = |path: &Path| path.canonicalize().expect("exists");

        // A session outside any repository writes two files in A, one at
        // its root and one below, and a note outside any: A, once.
        let mut writes = Writes::default();
        let files = [a.join("x"), a.join("src/y"), outside.join("n")];
        writes.wrote(&files, &[]);
        let asked = settle(&mut writes, None, &mut roots, &finder, &mut repos, &tx, &rx);
        assert_eq!(asked, 3, "one ask per directory");
        assert_eq!(on_list(&writes).len(), 1);
        assert_eq!(real(on_list(&writes)[0]), a);
        // Again in A, then in B: the cache answers for A, and B comes
        // first.
        writes.wrote(&[a.join("z"), b.join("w")], &[]);
        let asked = settle(&mut writes, None, &mut roots, &finder, &mut repos, &tx, &rx);
        assert_eq!(asked, 1, "only B's directory");
        let listed: Vec<PathBuf> = on_list(&writes).into_iter().map(real).collect();
        assert_eq!(listed, [b.clone(), a.clone()]);

        // A session in A: its own writes list nothing, those in the
        // repository nested in it list that one, and B's the shared one.
        let own = writes.repos[1].root.clone();
        let mut in_a = Writes::default();
        in_a.wrote(&[a.join("src/y"), inner.join("f"), b.join("v")], &[]);
        let asked = settle(
            &mut in_a,
            Some(&own),
            &mut roots,
            &finder,
            &mut repos,
            &tx,
            &rx,
        );
        assert_eq!(asked, 1, "only the nested repository's directory");
        let listed: Vec<PathBuf> = on_list(&in_a).into_iter().map(real).collect();
        assert_eq!(listed, [b, inner]);
        assert_eq!(in_a.repos[0].id, writes.repos[0].id, "one worker for B");
    }
}
