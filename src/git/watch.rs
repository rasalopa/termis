//! A repository's files watched through FSEvents (ADR-0010): the
//! directories to watch, which changed paths count, and the watch.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// The directories of a repository, canonicalized, as FSEvents reports
/// paths in them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// The root of the work tree.
    pub work: PathBuf,
    /// The git directory: the work tree's `.git`, or for a linked
    /// worktree its directory under the common one's `worktrees/`.
    pub git: PathBuf,
    /// The directory every worktree of the repository shares: the git
    /// directory itself, but for a linked worktree.
    pub common: PathBuf,
}

impl Dirs {
    /// The directories of the repository containing `dir`, as `git
    /// rev-parse` names them. Fails when git does, when it prints other
    /// than three lines, or when a directory cannot be resolved.
    pub fn read(dir: &Path) -> Result<Self> {
        let output = super::run(
            dir,
            [
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
            ],
        )
        .context("git rev-parse")?;
        let lines: Vec<&[u8]> = output
            .strip_suffix(b"\n")
            .unwrap_or(&output)
            .split(|&byte| byte == b'\n')
            .collect();
        let [work, git, common] = lines.as_slice() else {
            bail!("git rev-parse printed {} lines, not 3", lines.len());
        };
        let resolve = |path: &Path| {
            path.canonicalize()
                .with_context(|| format!("resolve {}", path.display()))
        };
        Ok(Self {
            work: resolve(Path::new(OsStr::from_bytes(work)))?,
            git: resolve(Path::new(OsStr::from_bytes(git)))?,
            // Relative to the directory git ran in, not to the root.
            common: resolve(&dir.join(OsStr::from_bytes(common)))?,
        })
    }

    /// The directories to watch recursively so that all three are: each
    /// one not inside another, once.
    fn cover(&self) -> Vec<&Path> {
        let all = [&self.work, &self.git, &self.common].map(PathBuf::as_path);
        let mut cover: Vec<&Path> = Vec::new();
        for dir in all {
            let inside = all
                .iter()
                .any(|other| *other != dir && dir.starts_with(other));
            if !inside && !cover.contains(&dir) {
                cover.push(dir);
            }
        }
        cover
    }
}

/// Whether a change at `path` can change what a refresh reads of the
/// repository in `dirs`. Never a cookie of watchman's, made on each
/// query git's `fsmonitor-watchman` hook sends. Under the git directory,
/// looked at first as the most specific: not a `.lock` file, the other
/// worktrees, the file monitor's files, nor, outside `refs/`, objects
/// and logs. Under the common directory of a linked worktree: only the
/// refs and the configuration, `.lock` files excepted. Under the work
/// tree: not under a `target`, `node_modules` or `.git` directory. A path
/// under none of them counts.
pub fn relevant(dirs: &Dirs, path: &Path) -> bool {
    let name = path.file_name().map(OsStr::as_bytes);
    if name.is_some_and(|name| name.starts_with(b".watchman-cookie-")) {
        return false;
    }
    let locked = name.is_some_and(|name| name.ends_with(b".lock"));
    if let Ok(inside) = path.strip_prefix(&dirs.git) {
        let first = inside.iter().next().map(OsStr::as_bytes);
        let elsewhere = first
            .is_some_and(|first| first == b"worktrees" || first.starts_with(b"fsmonitor--daemon"));
        let stored = !matches!(first, Some(b"refs"))
            && inside
                .iter()
                .any(|part| matches!(part.as_bytes(), b"objects" | b"logs"));
        return !locked && !elsewhere && !stored;
    }
    if dirs.common != dirs.git
        && let Ok(inside) = path.strip_prefix(&dirs.common)
    {
        let mut parts = inside.iter().map(OsStr::as_bytes);
        let shared = match parts.next() {
            Some(b"refs" | b"reftable" | b"info") => true,
            Some(b"packed-refs" | b"config") => parts.next().is_none(),
            _ => false,
        };
        return shared && !locked;
    }
    if let Ok(inside) = path.strip_prefix(&dirs.work) {
        return !inside
            .iter()
            .any(|part| matches!(part.as_bytes(), b"target" | b"node_modules" | b".git"));
    }
    true
}

/// Whether what notify hands over can change what a refresh reads: an
/// error or a rescan notice, as events may have been lost, an event with
/// no path, or one with a path that is [`relevant`]; not an access.
fn counts(dirs: &Dirs, result: &notify::Result<Event>) -> bool {
    match result {
        Err(_) => true,
        Ok(event) if event.need_rescan() => true,
        Ok(event) if event.kind.is_access() => false,
        Ok(event) => event.paths.is_empty() || event.paths.iter().any(|path| relevant(dirs, path)),
    }
}

/// A watch on a repository's directories, through one FSEvents stream on
/// a thread of notify's. Dropping it stops the stream and joins the
/// thread.
pub struct Watch {
    /// Watches until dropped.
    _watcher: RecommendedWatcher,
}

impl Watch {
    /// Starts watching the directories of `dirs`, calling `on_change`
    /// from notify's thread for an error, a rescan notice, an event with
    /// no path and one with a [`relevant`] path; a write comes as two to
    /// four events. Fails when a directory is not UTF-8, on which notify
    /// would panic, or is gone.
    pub fn start(dirs: Dirs, on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let cover: Vec<PathBuf> = dirs.cover().into_iter().map(Path::to_path_buf).collect();
        if let Some(dir) = cover.iter().find(|dir| dir.to_str().is_none()) {
            bail!("{} is not UTF-8", dir.display());
        }
        // Runs inside notify's callback from FSEvents, where a panic
        // aborts the process: it checks paths and calls `on_change`.
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            if counts(&dirs, &result) {
                on_change();
            }
        })
        .context("create the watcher")?;
        // One stream for every directory, started once.
        let mut paths = watcher.paths_mut();
        for dir in &cover {
            paths
                .add(dir, RecursiveMode::Recursive)
                .with_context(|| format!("watch {}", dir.display()))?;
        }
        paths.commit().context("start the watch")?;
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{QUIET, drain, git, init};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long FSEvents gets to report a change.
    const WAIT: Duration = Duration::from_secs(10);

    /// The directories of a repository at `/r`.
    fn plain() -> Dirs {
        Dirs {
            work: PathBuf::from("/r"),
            git: PathBuf::from("/r/.git"),
            common: PathBuf::from("/r/.git"),
        }
    }

    /// The directories of a linked worktree at `/w/linked` of the
    /// repository at `/w/main`.
    fn linked() -> Dirs {
        Dirs {
            work: PathBuf::from("/w/linked"),
            git: PathBuf::from("/w/main/.git/worktrees/linked"),
            common: PathBuf::from("/w/main/.git"),
        }
    }

    #[test]
    fn the_paths_that_count_in_a_plain_repository() {
        let dirs = plain();
        let table = [
            ("src/main.rs", true),
            ("target/debug/x", false),
            ("crates/a/target/x", false),
            ("web/node_modules/x", false),
            ("vendor/x/.git/HEAD", false),
            // Whole components, never prefixes; lockfiles are tracked.
            ("Cargo.lock", true),
            (".github/workflows/ci.yml", true),
            ("targets/x", true),
            ("src/target.rs", true),
            (".gitignore", true),
            ("", true),
            (".git/index", true),
            (".git/index.lock", false),
            (".git/objects/ab/cd", false),
            (".git/logs/HEAD", false),
            (".git/refs/heads/main.lock", false),
            (".git/fsmonitor--daemon/cookies/1-2", false),
            (".git/fsmonitor--daemon.ipc", false),
            (".git/.watchman-cookie-host-1-2", false),
            (".watchman-cookie-host-1-2", false),
            ("src/.watchman-cookie-host-1-2", false),
            (".git/worktrees/other/index", false),
            (".git/modules/s/objects/x", false),
            (".git/modules/s/logs/HEAD", false),
            (".git/refs/heads/main", true),
            (".git/refs/heads/fix/logs", true),
            (".git/refs/remotes/origin/objects", true),
            (".git/reftable/tables.list", true),
            (".git/modules/s/index", true),
            (".git/HEAD", true),
            (".git/packed-refs", true),
            (".git/config", true),
        ];
        for (path, counts) in table {
            let full = dirs.work.join(path);
            assert_eq!(relevant(&dirs, &full), counts, "{}", full.display());
        }
        assert!(relevant(&dirs, Path::new("/elsewhere/x")), "outside");
    }

    #[test]
    fn the_paths_that_count_in_a_linked_worktree() {
        let dirs = linked();
        let table = [
            ("/w/linked/src/main.rs", true),
            ("/w/linked/target/x", false),
            ("/w/main/.git/worktrees/linked/index", true),
            ("/w/main/.git/worktrees/linked/HEAD", true),
            ("/w/main/.git/worktrees/linked/index.lock", false),
            ("/w/main/.git/worktrees/linked/logs/HEAD", false),
            ("/w/linked/.watchman-cookie-host-1-2", false),
            ("/w/main/.git/refs/heads/x", true),
            ("/w/main/.git/reftable/tables.list", true),
            ("/w/main/.git/packed-refs", true),
            ("/w/main/.git/config", true),
            ("/w/main/.git/info/exclude", true),
            ("/w/main/.git/index", false),
            ("/w/main/.git/HEAD", false),
            ("/w/main/.git/ORIG_HEAD", false),
            ("/w/main/.git/worktrees/other/index", false),
            ("/w/main/.git/objects/x", false),
            ("/w/main/.git/refs/heads/x.lock", false),
            ("/w/main/.git", false),
            ("/w/main/src/main.rs", true),
            ("/elsewhere/x", true),
        ];
        for (path, counts) in table {
            assert_eq!(relevant(&dirs, Path::new(path)), counts, "{path}");
        }
    }

    #[test]
    fn a_plain_repository_is_watched_from_its_root() {
        assert_eq!(plain().cover(), [Path::new("/r")]);
        assert_eq!(
            linked().cover(),
            [Path::new("/w/linked"), Path::new("/w/main/.git")]
        );
        let separate = Dirs {
            work: PathBuf::from("/r"),
            git: PathBuf::from("/g.git"),
            common: PathBuf::from("/g.git"),
        };
        assert_eq!(separate.cover(), [Path::new("/r"), Path::new("/g.git")]);
    }

    #[test]
    fn events_count_by_their_paths_unless_events_were_lost() {
        use notify::event::{AccessKind, CreateKind, EventKind, Flag};
        let dirs = plain();
        let at = |kind, path: &str| Event::new(kind).add_path(PathBuf::from(path));
        let create = EventKind::Create(CreateKind::File);
        assert!(counts(&dirs, &Ok(at(create, "/r/a.txt"))));
        assert!(!counts(&dirs, &Ok(at(create, "/r/target/x"))));
        assert!(counts(&dirs, &Ok(Event::new(create))), "no path");
        let rescan = at(EventKind::Other, "/r/target").set_flag(Flag::Rescan);
        assert!(counts(&dirs, &Ok(rescan)));
        assert!(counts(&dirs, &Err(notify::Error::generic("lost"))));
        let access = EventKind::Access(AccessKind::Any);
        assert!(!counts(&dirs, &Ok(at(access, "/r/a.txt"))));
        let both = at(create, "/r/target/x").add_path(PathBuf::from("/r/a.txt"));
        assert!(counts(&dirs, &Ok(both)), "a rename's other path");
    }

    /// `path` resolved, as [`Dirs::read`] gives it.
    fn real(path: &Path) -> PathBuf {
        path.canonicalize().expect("exists")
    }

    #[test]
    fn the_directories_of_a_repository_and_of_a_linked_worktree() {
        let dir = TempDir::new().expect("temp dir");
        let main = dir.path().join("main");
        std::fs::create_dir_all(main.join("sub")).expect("mkdir");
        init(&main);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let root = Dirs {
            work: real(&main),
            git: real(&main.join(".git")),
            common: real(&main.join(".git")),
        };
        assert_eq!(Dirs::read(&main).expect("reads"), root);
        // Git prints the common directory as `../.git` from `sub`.
        assert_eq!(Dirs::read(&main.join("sub")).expect("reads"), root);
        let linked = dir.path().join("linked");
        let path = linked.to_str().expect("UTF-8");
        git(&main, &["worktree", "add", "-q", "-b", "feature", path]);
        assert_eq!(
            Dirs::read(&linked).expect("reads"),
            Dirs {
                work: real(&linked),
                git: real(&main.join(".git/worktrees/linked")),
                common: real(&main.join(".git")),
            }
        );
        let outside = TempDir::new().expect("temp dir");
        assert!(Dirs::read(outside.path()).is_err(), "not a repository");
    }

    /// A watch on the repository containing `dir`, with the channel its
    /// calls reach.
    fn watched(dir: &Path) -> (Watch, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let dirs = Dirs::read(dir).expect("reads");
        let watch = Watch::start(dirs, move || {
            let _ = tx.send(());
        })
        .expect("starts");
        (watch, rx)
    }

    #[test]
    fn a_watch_calls_for_the_changes_that_count() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let (watch, rx) = watched(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        rx.recv_timeout(WAIT).expect("a call for a.txt");
        drain(&rx);
        std::fs::create_dir_all(dir.path().join("target")).expect("mkdir");
        std::fs::write(dir.path().join("target/x"), "x").expect("write");
        std::fs::write(dir.path().join(".git/objects/x"), "x").expect("write");
        for name in [".git/index.lock", ".git/.watchman-cookie-host-1-2"] {
            let made = dir.path().join(name);
            std::fs::write(&made, "").expect("write");
            std::fs::remove_file(&made).expect("rm");
        }
        assert_eq!(
            rx.recv_timeout(QUIET),
            Err(RecvTimeoutError::Timeout),
            "nothing that counts"
        );
        std::fs::write(dir.path().join("b.txt"), "b\n").expect("write");
        rx.recv_timeout(WAIT).expect("a call for b.txt");
        drain(&rx);
        drop(watch);
        std::fs::write(dir.path().join("c.txt"), "c\n").expect("write");
        assert_eq!(
            rx.recv_timeout(QUIET),
            Err(RecvTimeoutError::Disconnected),
            "the call went with the watch"
        );
    }

    #[test]
    fn a_linked_worktree_is_watched_through_its_git_directory() {
        let dir = TempDir::new().expect("temp dir");
        let main = dir.path().join("main");
        std::fs::create_dir(&main).expect("mkdir");
        init(&main);
        std::fs::write(main.join("a.txt"), "a\n").expect("write");
        git(&main, &["add", "a.txt"]);
        git(&main, &["commit", "-q", "-m", "one"]);
        let linked = dir.path().join("linked");
        let path = linked.to_str().expect("UTF-8");
        git(&main, &["worktree", "add", "-q", "-b", "feature", path]);
        let (watch, rx) = watched(&linked);
        std::fs::write(linked.join("a.txt"), "a\nb\n").expect("write");
        rx.recv_timeout(WAIT).expect("a call for the edit");
        drain(&rx);
        // Only its index, outside the work tree, and the common objects
        // change.
        git(&linked, &["add", "a.txt"]);
        rx.recv_timeout(WAIT).expect("a call for the stage");
        drain(&rx);
        drop(watch);
        std::fs::write(linked.join("b.txt"), "b\n").expect("write");
        assert_eq!(rx.recv_timeout(QUIET), Err(RecvTimeoutError::Disconnected));
    }
}
