//! A session's file tree in the projects pane: whether the pane shows it,
//! the files as last read, its folds and cursor, and when to read the
//! files again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::git::files::Files;
use crate::ui::changes::Step;
use crate::ui::tree::{Folds, Tree};

/// How often the files are read at most while the tree is on screen,
/// counted from the last answer.
const FILES_EVERY: Duration = Duration::from_secs(2);

/// A session's file tree.
#[derive(Debug, Default)]
pub(super) struct FileTree {
    /// Whether the projects pane shows it, in place of the list of
    /// sessions and the card.
    pub(super) shown: bool,
    /// The tree as last read, or why the files could not be; none before
    /// the first answer.
    pub(super) content: Option<Result<Tree, String>>,
    /// The directories unfolded, the cursor and the scroll.
    pub(super) folds: Folds,
    /// When the last answer came; none before the first.
    answered: Option<Instant>,
    /// Whether an answer is awaited.
    in_flight: bool,
    /// Whether the files are to be read again: the tree came on screen,
    /// or a refresh of its repository found the status changed.
    wanted: bool,
}

impl FileTree {
    /// Shows the tree in the projects pane, or the list of sessions again.
    pub(super) fn toggle(&mut self) {
        self.shown = !self.shown;
    }

    /// Asks for the files to be read again, once due.
    pub(super) fn want(&mut self) {
        self.wanted = true;
    }

    /// Whether to ask for the files at `now`, with the tree `on_screen` or
    /// not: never while it is hidden or an answer is awaited; then before
    /// the first answer, and when wanted again `FILES_EVERY` after the
    /// last answer.
    pub(super) fn due(&self, now: Instant, on_screen: bool) -> bool {
        let passed = self
            .answered
            .is_none_or(|answered| now.duration_since(answered) >= FILES_EVERY);
        on_screen && !self.in_flight && (self.content.is_none() || self.wanted) && passed
    }

    /// Notes that the files were asked for.
    pub(super) fn sent(&mut self) {
        self.in_flight = true;
        self.wanted = false;
    }

    /// Takes `files`, which came at `now`, relative to the repository's
    /// root or the walked directory, keeping those under `below`,
    /// relative to it; or why they could not be read. The tree's note on
    /// a cut list counts the files of the list.
    pub(super) fn answer(&mut self, files: Result<&Files, &str>, below: &Path, now: Instant) {
        self.answered = Some(now);
        self.in_flight = false;
        self.content = Some(
            files
                .map(|files| {
                    let cut = files.cut.then_some(files.paths.len());
                    Tree::new(&under(files, below), cut)
                })
                .map_err(str::to_owned),
        );
    }

    /// Takes why the files could not be asked for, at `now`, as the
    /// answer.
    pub(super) fn failed(&mut self, error: String, now: Instant) {
        self.answered = Some(now);
        self.in_flight = false;
        self.content = Some(Err(error));
    }

    /// Moves the cursor a row, once there is a tree.
    pub(super) fn step(&mut self, step: Step) {
        if let Some(Ok(tree)) = &self.content {
            self.folds.step(tree, step);
        }
    }

    /// Unfolds the directory under the cursor, once there is a tree.
    pub(super) fn open(&mut self) {
        if let Some(Ok(tree)) = &self.content {
            self.folds.open(tree);
        }
    }

    /// Folds the directory under the cursor, or goes to the one the entry
    /// is in, once there is a tree.
    pub(super) fn close(&mut self) {
        if let Some(Ok(tree)) = &self.content {
            self.folds.close(tree);
        }
    }
}

/// The paths of `files` under `below`, relative to it; all of them when
/// it is empty.
fn under(files: &Files, below: &Path) -> Vec<PathBuf> {
    files
        .paths
        .iter()
        .filter_map(|path| path.strip_prefix(below).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::Kind;

    /// A list of `paths`, not cut.
    fn files(paths: &[&str]) -> Files {
        Files {
            paths: paths.iter().map(PathBuf::from).collect(),
            cut: false,
        }
    }

    #[test]
    fn the_files_are_read_when_the_tree_shows_and_once_per_change() {
        let t0 = Instant::now();
        let ms = Duration::from_millis(1);
        let top = Path::new("");
        let mut tree = FileTree::default();
        assert!(!tree.due(t0, false), "never while hidden");
        assert!(tree.due(t0, true), "at once the first time");
        tree.sent();
        assert!(!tree.due(t0 + FILES_EVERY * 2, true), "one at a time");
        let t1 = t0 + ms * 100;
        tree.answer(Ok(&files(&["a"])), top, t1);
        assert!(!tree.due(t1 + FILES_EVERY * 2, true), "nothing changed");
        // A change asks again, no sooner than FILES_EVERY after the last
        // answer.
        tree.want();
        assert!(!tree.due(t1 + FILES_EVERY - ms, true));
        assert!(!tree.due(t1 + FILES_EVERY, false), "not while hidden");
        assert!(tree.due(t1 + FILES_EVERY, true));
        let t2 = t1 + FILES_EVERY;
        tree.sent();
        // A change while the answer is awaited asks once more after it,
        // FILES_EVERY after it however long it took.
        tree.want();
        assert!(!tree.due(t2 + FILES_EVERY, true));
        let t3 = t2 + FILES_EVERY * 3;
        tree.answer(Ok(&files(&["a", "b"])), top, t3);
        assert!(!tree.due(t3, true), "not at once after a slow answer");
        assert!(!tree.due(t3 + FILES_EVERY - ms, true));
        assert!(tree.due(t3 + FILES_EVERY, true));
        let t4 = t3 + FILES_EVERY;
        tree.sent();
        tree.answer(Err("fatal: bad"), top, t4);
        assert!(
            !tree.due(t4 + FILES_EVERY * 3, true),
            "a failure is an answer"
        );
        assert!(matches!(&tree.content, Some(Err(error)) if error == "fatal: bad"));
        // One that cannot be asked for says why.
        let mut walked = FileTree::default();
        walked.sent();
        walked.failed("spawn the file walker".into(), t0);
        assert!(!walked.due(t0 + FILES_EVERY, true));
        assert!(walked.content.is_some());
    }

    #[test]
    fn a_tree_below_the_root_shows_only_its_own_files() {
        let mut tree = FileTree::default();
        let list = Files {
            paths: ["README.md", "app/src/main.rs", "app/x.txt", "app2/y", "app"]
                .map(PathBuf::from)
                .to_vec(),
            cut: true,
        };
        tree.answer(Ok(&list), Path::new("app"), Instant::now());
        let Some(Ok(built)) = &tree.content else {
            panic!("a tree");
        };
        // The note counts the list, not what is left of it.
        let own = ["src/main.rs", "x.txt"].map(PathBuf::from);
        assert_eq!(*built, Tree::new(&own, Some(5)));
        let rows: Vec<(PathBuf, Kind)> = built
            .rows(&Default::default())
            .into_iter()
            .map(|row| (row.path, row.kind))
            .collect();
        assert_eq!(
            rows,
            [
                (PathBuf::from("src"), Kind::Dir { open: false }),
                (PathBuf::from("x.txt"), Kind::File),
            ]
        );
        assert_eq!(under(&list, Path::new("")).len(), 5, "at the root, all");
    }

    #[test]
    fn the_keys_move_the_tree_once_there_is_one() {
        let mut tree = FileTree::default();
        tree.step(Step::Down);
        tree.open();
        tree.close();
        tree.answer(Ok(&files(&["a/1", "b"])), Path::new(""), Instant::now());
        tree.open();
        tree.step(Step::Down);
        // The entry under the cursor, with the directories in `open`.
        let under = |tree: &FileTree, open: &[&str]| {
            let Some(Ok(built)) = &tree.content else {
                panic!("a tree");
            };
            let rows = built.rows(&open.iter().map(PathBuf::from).collect());
            rows[tree.folds.cursor(&rows)].path.clone()
        };
        assert_eq!(under(&tree, &["a"]), Path::new("a/1"));
        tree.close();
        tree.close();
        assert_eq!(under(&tree, &[]), Path::new("a"));
        assert!(!tree.shown);
        tree.toggle();
        assert!(tree.shown);
        tree.toggle();
        assert!(!tree.shown);
    }
}
