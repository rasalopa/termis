//! The diff of one path, as `git diff` prints it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What to diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The path, as status prints it, with the source of a rename after
    /// it so that git can pair them.
    pub paths: Vec<PathBuf>,
    /// The index against `HEAD` rather than the work tree against the
    /// index.
    pub staged: bool,
    /// An untracked file, shown whole against nothing.
    pub untracked: bool,
}

/// Reads the diff `request` asks for in the repository at `root`, with
/// no colour and no external diff driver; a rename or copy is paired
/// across its two paths. Empty when nothing differs. Fails for an
/// untracked path that is a directory or cannot be read.
pub fn read(root: &Path, request: &Request) -> Result<String> {
    let output = if request.untracked {
        let path = request
            .paths
            .first()
            .context("an untracked diff needs its path")?;
        let metadata = std::fs::symlink_metadata(root.join(path))
            .with_context(|| format!("read {}", path.display()))?;
        if metadata.is_dir() {
            bail!("{} is a directory", path.display());
        }
        // Prefixed so that a file named `-` is not read as standard input.
        let local = Path::new(".").join(path);
        let args = [
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "--",
            "/dev/null",
        ]
        .map(OsStr::new)
        .into_iter()
        .chain([local.as_os_str()]);
        // Git exits with 1 when the two sides differ, which is the point;
        // it prints at least the file headers, so nothing means it failed.
        let output = super::run_accepting(root, args, &[1]).context("git diff --no-index")?;
        if output.is_empty() {
            bail!("git diff --no-index printed nothing for {}", path.display());
        }
        output
    } else {
        let mut args: Vec<&OsStr> = [
            "--no-optional-locks",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-relative",
        ]
        .map(OsStr::new)
        .to_vec();
        if request.staged {
            args.push(OsStr::new("--cached"));
        }
        if request.paths.len() > 1 {
            args.extend(["--find-renames", "--find-copies"].map(OsStr::new));
        }
        args.push(OsStr::new("--"));
        args.extend(request.paths.iter().map(|path| path.as_os_str()));
        super::run(root, args).context("git diff")?
    };
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, init};
    use tempfile::TempDir;

    fn request(paths: &[&str], staged: bool, untracked: bool) -> Request {
        Request {
            paths: paths.iter().map(PathBuf::from).collect(),
            staged,
            untracked,
        }
    }

    #[test]
    fn reads_the_index_the_work_tree_an_untracked_file_and_a_rename() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write");
        std::fs::write(dir.path().join("old.txt"), "o\n").expect("write");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);

        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").expect("write");
        let worktree = read(dir.path(), &request(&["a.txt"], false, false)).expect("diff");
        assert!(worktree.contains("@@ -1,2 +1,3 @@"), "{worktree}");
        assert!(worktree.contains("\n+three\n"), "{worktree}");
        assert!(!worktree.contains('\x1b'), "no colour");
        let staged = read(dir.path(), &request(&["a.txt"], true, false)).expect("diff");
        assert!(staged.is_empty(), "nothing staged yet: {staged}");

        git(dir.path(), &["add", "a.txt"]);
        let staged = read(dir.path(), &request(&["a.txt"], true, false)).expect("diff");
        assert!(staged.contains("\n+three\n"), "{staged}");
        let worktree = read(dir.path(), &request(&["a.txt"], false, false)).expect("diff");
        assert!(worktree.is_empty(), "{worktree}");

        std::fs::write(dir.path().join("new.txt"), "n\n").expect("write");
        let untracked = read(dir.path(), &request(&["new.txt"], false, true)).expect("diff");
        assert!(untracked.contains("--- /dev/null"), "{untracked}");
        assert!(untracked.contains("\n+n\n"), "{untracked}");

        git(dir.path(), &["mv", "old.txt", "moved.txt"]);
        let renamed =
            read(dir.path(), &request(&["moved.txt", "old.txt"], true, false)).expect("diff");
        assert!(renamed.contains("rename from old.txt"), "{renamed}");
        assert!(renamed.contains("rename to moved.txt"), "{renamed}");

        let error = read(dir.path(), &request(&[], false, true)).expect_err("needs a path");
        assert!(error.to_string().contains("path"), "{error:#}");
        std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
        assert!(
            read(dir.path(), &request(&["nested"], false, true)).is_err(),
            "a directory"
        );
        assert!(
            read(dir.path(), &request(&["missing"], false, true)).is_err(),
            "gone"
        );
        std::fs::write(dir.path().join("empty"), "").expect("write");
        let empty = read(dir.path(), &request(&["empty"], false, true)).expect("reads");
        assert!(
            empty.contains("diff --git") && !empty.contains("@@"),
            "{empty}"
        );
        let outside = TempDir::new().expect("temp dir");
        assert!(read(outside.path(), &request(&["a.txt"], false, false)).is_err());
    }
}
