//! Staging through the `git` binary: add, take out of the index, add all.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Stages `paths` in the repository at `root`, as status prints them:
/// edits, deletions and untracked files alike. Nothing happens for no
/// paths.
pub fn stage(root: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let args = ["add", "--"]
        .map(OsStr::new)
        .into_iter()
        .chain(paths.iter().map(|path| path.as_os_str()));
    super::run(root, args).context("git add")?;
    Ok(())
}

/// Takes `paths` out of the index, back to what `HEAD` has, in the
/// repository at `root`; a staged rename needs both of its paths. Works
/// before the first commit too, which `restore --staged` does not.
/// Nothing happens for no paths.
pub fn unstage(root: &Path, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let args = ["reset", "-q", "--"]
        .map(OsStr::new)
        .into_iter()
        .chain(paths.iter().map(|path| path.as_os_str()));
    super::run(root, args).context("git reset")?;
    Ok(())
}

/// Stages every change in the repository at `root`.
pub fn stage_all(root: &Path) -> Result<()> {
    super::run(root, ["add", "-A"]).context("git add")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::{self, Change};
    use crate::git::tests::{git, init};
    use tempfile::TempDir;

    /// `(path, index, worktree)` for every entry, in git's order.
    fn entries(root: &Path) -> Vec<(String, Option<Change>, Option<Change>)> {
        status::read(root)
            .expect("status")
            .entries
            .into_iter()
            .map(|entry| {
                (
                    entry.path.to_string_lossy().into_owned(),
                    entry.index,
                    entry.worktree,
                )
            })
            .collect()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn stages_and_unstages_before_the_first_commit() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        stage(dir.path(), &paths(&["a.txt"])).expect("stages");
        assert_eq!(
            entries(dir.path()),
            vec![("a.txt".into(), Some(Change::Added), None)]
        );
        unstage(dir.path(), &paths(&["a.txt"])).expect("unstages");
        assert_eq!(
            entries(dir.path()),
            vec![("a.txt".into(), None, Some(Change::Untracked))]
        );
    }

    #[test]
    fn stages_edits_deletions_and_renames_and_takes_them_back() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        for (name, content) in [("keep.txt", "k\n"), ("del.txt", "d\n"), ("old.txt", "o\n")] {
            std::fs::write(dir.path().join(name), content).expect("write");
        }
        stage_all(dir.path()).expect("stages all");
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        assert!(entries(dir.path()).is_empty());

        std::fs::write(dir.path().join("keep.txt"), "k\nmore\n").expect("write");
        std::fs::remove_file(dir.path().join("del.txt")).expect("rm");
        std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).expect("mv");
        stage(dir.path(), &paths(&["keep.txt", "del.txt"])).expect("stages");
        assert_eq!(
            entries(dir.path()),
            vec![
                ("del.txt".into(), Some(Change::Deleted), None),
                ("keep.txt".into(), Some(Change::Modified), None),
                ("old.txt".into(), None, Some(Change::Deleted)),
                ("new.txt".into(), None, Some(Change::Untracked)),
            ]
        );
        stage(dir.path(), &paths(&["old.txt", "new.txt"])).expect("stages the rename");
        assert_eq!(
            entries(dir.path())[2],
            ("new.txt".into(), Some(Change::Renamed), None)
        );
        // Both paths of the rename leave the index together.
        unstage(dir.path(), &paths(&["new.txt", "old.txt"])).expect("unstages");
        assert_eq!(
            entries(dir.path()),
            vec![
                ("del.txt".into(), Some(Change::Deleted), None),
                ("keep.txt".into(), Some(Change::Modified), None),
                ("old.txt".into(), None, Some(Change::Deleted)),
                ("new.txt".into(), None, Some(Change::Untracked)),
            ]
        );
        unstage(dir.path(), &paths(&["keep.txt", "del.txt"])).expect("unstages");
        assert_eq!(
            entries(dir.path()),
            vec![
                ("del.txt".into(), None, Some(Change::Deleted)),
                ("keep.txt".into(), None, Some(Change::Modified)),
                ("old.txt".into(), None, Some(Change::Deleted)),
                ("new.txt".into(), None, Some(Change::Untracked)),
            ]
        );
        stage_all(dir.path()).expect("stages all");
        assert!(
            entries(dir.path())
                .iter()
                .all(|(_, _, worktree)| worktree.is_none())
        );
    }

    #[test]
    fn takes_paths_literally() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        for name in ["[ab]", "a", "b", ":x"] {
            std::fs::write(dir.path().join(name), "1\n").expect("write");
        }
        stage(dir.path(), &paths(&["[ab]", ":x"])).expect("stages");
        let staged: Vec<_> = entries(dir.path())
            .into_iter()
            .filter(|(_, index, _)| index.is_some())
            .map(|(path, _, _)| path)
            .collect();
        assert_eq!(staged, [":x", "[ab]"]);
        unstage(dir.path(), &paths(&["[ab]"])).expect("unstages");
        let staged: Vec<_> = entries(dir.path())
            .into_iter()
            .filter(|(_, index, _)| index.is_some())
            .map(|(path, _, _)| path)
            .collect();
        assert_eq!(staged, [":x"]);
    }

    #[test]
    fn reports_git_errors_and_ignores_empty_lists() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        stage_all(dir.path()).expect("stages all");
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        let error = stage(dir.path(), &paths(&["nope.txt"])).expect_err("missing");
        assert!(format!("{error:#}").contains("pathspec"), "{error:#}");
        // Git ignores paths it does not know in a reset; a reset that cannot
        // run is still an error.
        unstage(dir.path(), &paths(&["nope.txt"])).expect("nothing to do");
        let outside = TempDir::new().expect("temp dir");
        let error = unstage(outside.path(), &paths(&["a.txt"])).expect_err("no repository");
        assert!(format!("{error:#}").contains("git reset"), "{error:#}");
        stage(dir.path(), &[]).expect("nothing to do");
        unstage(dir.path(), &[]).expect("nothing to do");
        assert!(entries(dir.path()).is_empty());
    }
}
