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

/// What git printed for a diff, as far as the budget it was read with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// The text, cut after its last whole line when git printed more.
    pub text: String,
    /// Whether git printed more than the budget.
    pub cut: bool,
}

/// Reads the diff `request` asks for in the repository at `root`, with
/// no colour and no external diff driver, up to `budget` bytes; git is
/// stopped past it. A rename or copy is paired across its two paths.
/// Empty when nothing differs. Fails for an untracked path that is a
/// directory, a pipe or a link to either (`super::check_untracked`), or
/// that cannot be read.
pub fn read(root: &Path, request: &Request, budget: usize) -> Result<Read> {
    let (bytes, cut) = if request.untracked {
        let path = request
            .paths
            .first()
            .context("an untracked diff needs its path")?;
        super::check_untracked(root, path)?;
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
        let (bytes, cut) =
            super::run_up_to(root, args, &[1], budget).context("git diff --no-index")?;
        if bytes.is_empty() && !cut {
            bail!("git diff --no-index printed nothing for {}", path.display());
        }
        (bytes, cut)
    } else {
        let mut args: Vec<&OsStr> = [
            "--no-optional-locks",
            // --no-optional-locks does not stop porcelain diff's refresh of the index.
            "-c",
            "diff.autoRefreshIndex=false",
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
        super::run_up_to(root, args, &[], budget).context("git diff")?
    };
    Ok(Read {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        cut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{REFUSED, at_once, git, init, odd_paths, stat_dirty};
    use tempfile::TempDir;

    fn request(paths: &[&str], staged: bool, untracked: bool) -> Request {
        Request {
            paths: paths.iter().map(PathBuf::from).collect(),
            staged,
            untracked,
        }
    }

    /// The text of the diff `asked` for in the repository at `root`, read
    /// with no budget.
    fn whole(root: &Path, asked: &Request) -> Result<String> {
        read(root, asked, usize::MAX).map(|read| read.text)
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
        let worktree = whole(dir.path(), &request(&["a.txt"], false, false)).expect("diff");
        assert!(worktree.contains("@@ -1,2 +1,3 @@"), "{worktree}");
        assert!(worktree.contains("\n+three\n"), "{worktree}");
        assert!(!worktree.contains('\x1b'), "no colour");
        let staged = whole(dir.path(), &request(&["a.txt"], true, false)).expect("diff");
        assert!(staged.is_empty(), "nothing staged yet: {staged}");

        git(dir.path(), &["add", "a.txt"]);
        let staged = whole(dir.path(), &request(&["a.txt"], true, false)).expect("diff");
        assert!(staged.contains("\n+three\n"), "{staged}");
        let worktree = whole(dir.path(), &request(&["a.txt"], false, false)).expect("diff");
        assert!(worktree.is_empty(), "{worktree}");

        std::fs::write(dir.path().join("new.txt"), "n\n").expect("write");
        let untracked = whole(dir.path(), &request(&["new.txt"], false, true)).expect("diff");
        assert!(untracked.contains("--- /dev/null"), "{untracked}");
        assert!(untracked.contains("\n+n\n"), "{untracked}");

        git(dir.path(), &["mv", "old.txt", "moved.txt"]);
        let renamed =
            whole(dir.path(), &request(&["moved.txt", "old.txt"], true, false)).expect("diff");
        assert!(renamed.contains("rename from old.txt"), "{renamed}");
        assert!(renamed.contains("rename to moved.txt"), "{renamed}");

        let error = whole(dir.path(), &request(&[], false, true)).expect_err("needs a path");
        assert!(error.to_string().contains("path"), "{error:#}");
        std::fs::create_dir(dir.path().join("nested")).expect("mkdir");
        assert!(
            whole(dir.path(), &request(&["nested"], false, true)).is_err(),
            "a directory"
        );
        assert!(
            whole(dir.path(), &request(&["missing"], false, true)).is_err(),
            "gone"
        );
        std::fs::write(dir.path().join("empty"), "").expect("write");
        let empty = whole(dir.path(), &request(&["empty"], false, true)).expect("reads");
        assert!(
            empty.contains("diff --git") && !empty.contains("@@"),
            "{empty}"
        );
        let outside = TempDir::new().expect("temp dir");
        assert!(whole(outside.path(), &request(&["a.txt"], false, false)).is_err());
    }

    #[test]
    fn a_read_stops_at_its_budget_on_a_line_end() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        let long: String = (0..2000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), &long).expect("write");
        std::fs::write(dir.path().join("new.txt"), &long).expect("write");
        for asked in [
            request(&["a.txt"], false, false),
            request(&["new.txt"], false, true),
        ] {
            let all = read(dir.path(), &asked, usize::MAX).expect("diff");
            assert!(!all.cut);
            assert!(all.text.ends_with("\n+line 1999\n"), "{asked:?}");
            let part = read(dir.path(), &asked, 1000).expect("diff");
            assert!(part.cut);
            assert!(part.text.len() <= 1000 && part.text.ends_with('\n'));
            assert!(all.text.starts_with(&part.text), "{asked:?}");
        }
    }

    #[test]
    fn an_untracked_link_git_looks_through_fails_at_once() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        odd_paths(dir.path());
        for path in REFUSED {
            let root = dir.path().to_path_buf();
            let error = at_once(dir.path(), move || {
                whole(&root, &request(&[path], false, true))
            })
            .expect_err(path);
            assert_eq!(
                format!("{error:#}"),
                format!("{path} is not a regular file")
            );
        }
        // Git diffs a link to a file, a device or nothing as the link it is.
        for (path, target) in [
            ("to-file", "file.txt"),
            ("to-null", "/dev/null"),
            ("broken", "nowhere"),
        ] {
            let diff = whole(dir.path(), &request(&[path], false, true)).expect("diff");
            assert!(diff.contains("new file mode 120000"), "{diff}");
            assert!(diff.contains(&format!("\n+{target}\n")), "{diff}");
        }
    }

    #[test]
    fn a_file_touched_with_the_same_bytes_leaves_the_index_unwritten() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\n").expect("write");
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        stat_dirty(&file);
        let index = dir.path().join(".git/index");
        let before = std::fs::read(&index).expect("read");
        let diff = whole(dir.path(), &request(&["a.txt"], false, false)).expect("diff");
        assert!(diff.is_empty(), "{diff}");
        assert_eq!(std::fs::read(&index).expect("read"), before);
    }
}
