//! The files of a repository, as `git ls-files` lists them.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The most paths a list of files holds.
pub const LIMIT: usize = 20_000;

/// A list of files, relative to the directory it was read in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Files {
    /// The paths, in the order of their bytes, as git sorts them.
    pub paths: Vec<PathBuf>,
    /// Whether there were more than the list holds; it keeps those with
    /// the fewest directories above them.
    pub cut: bool,
}

/// Lists the files of the repository at `root` in its directory `dir`,
/// the whole repository when `dir` is empty, relative to the root: those
/// git tracks that are still there and the untracked ones it does not
/// ignore, up to [`LIMIT`]. When there are more, those with the fewest
/// directories above them are kept, then the first in the order of
/// their bytes. A path that is a directory on disk, such as a submodule
/// or a repository nested in an untracked one, is left out.
pub fn list(root: &Path, dir: &Path) -> Result<Files> {
    list_up_to(root, dir, LIMIT)
}

/// Like [`list`], up to `limit` files.
fn list_up_to(root: &Path, dir: &Path, limit: usize) -> Result<Files> {
    // Git takes no empty path; with none it lists the whole repository.
    let pathspec = (!dir.as_os_str().is_empty()).then_some(dir.as_os_str());
    let args = [
        "--no-optional-locks",
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ]
    .map(OsStr::new)
    .into_iter()
    .chain(pathspec);
    let output = super::run(root, args).context("git ls-files")?;
    Ok(collect(&output, limit, |path| is_file(&root.join(path))))
}

/// The paths of `output`, NUL-terminated as `git ls-files -z` prints
/// them, each once (an unmerged path comes once per stage), keeping
/// those `keep` says are there, up to `limit`: those with the fewest
/// directories above them first, then in the order of their bytes. The
/// paths kept are sorted by their bytes.
fn collect(output: &[u8], limit: usize, mut keep: impl FnMut(&Path) -> bool) -> Files {
    let mut listed: Vec<&[u8]> = output
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    listed.sort_by_cached_key(|path| (depth(path), *path));
    listed.dedup();
    let mut files = Files::default();
    for bytes in listed {
        let path = Path::new(OsStr::from_bytes(bytes));
        if !keep(path) {
            continue;
        }
        if files.paths.len() == limit {
            files.cut = true;
            break;
        }
        files.paths.push(path.to_path_buf());
    }
    files
        .paths
        .sort_unstable_by(|a, b| a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes()));
    files
}

/// How many directories are above `path`: the slashes in it.
fn depth(path: &[u8]) -> usize {
    path.iter().filter(|&&byte| byte == b'/').count()
}

/// Whether something that is not a directory is at `path`; a symbolic
/// link counts as itself, whatever it points to.
fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, init};
    use tempfile::TempDir;

    /// The paths of `files` as text.
    fn names(files: &Files) -> Vec<String> {
        files
            .paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn collects_each_path_once_in_the_order_of_its_bytes() {
        // Untracked files come first, then the index, an unmerged path once
        // per stage.
        let output = b"z.txt\0a/b\0a-b\0both\0both\0both\0gone\0";
        let files = collect(output, 10, |path| path != Path::new("gone"));
        assert_eq!(names(&files), ["a-b", "a/b", "both", "z.txt"]);
        assert!(!files.cut);
        // The limit keeps those with the fewest directories above them,
        // then the first, and says the rest were left out; a list that
        // only reaches it is whole.
        let files = collect(output, 2, |_| true);
        assert_eq!(names(&files), ["a-b", "both"]);
        assert!(files.cut);
        let files = collect(b"a\0b\0", 2, |_| true);
        assert_eq!(names(&files), ["a", "b"]);
        assert!(!files.cut);
        // A deep directory that sorts first leaves room for a file at the
        // top that sorts after it.
        let files = collect(b"a/b/1\0a/2\0a/b/3\0z.txt\0", 2, |_| true);
        assert_eq!(names(&files), ["a/2", "z.txt"]);
        assert!(files.cut);
        // What is left out does not count towards the limit, nor cuts the
        // list when it comes after it.
        let files = collect(b"a\0b\0c\0", 2, |path| path != Path::new("a"));
        assert_eq!(names(&files), ["b", "c"]);
        assert!(!files.cut);
        let files = collect(b"a\0b\0c\0", 2, |path| path != Path::new("c"));
        assert_eq!(names(&files), ["a", "b"]);
        assert!(!files.cut, "a path left out after the limit cuts nothing");
        assert_eq!(collect(b"", 2, |_| true), Files::default());
    }

    #[test]
    fn lists_tracked_and_untracked_files_but_not_ignored_or_deleted_ones() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        init(root);
        std::fs::create_dir_all(root.join("src/deep")).expect("mkdir");
        for (name, text) in [
            (".gitignore", "*.log\nbuild/\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("src/deep/a.rs", "\n"),
            ("gone.txt", "x\n"),
            ("sp ace.txt", "x\n"),
            ("new\nline.txt", "x\n"),
        ] {
            std::fs::write(root.join(name), text).expect("write");
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "one"]);
        std::fs::remove_file(root.join("gone.txt")).expect("rm");
        std::fs::create_dir(root.join("build")).expect("mkdir");
        for name in [
            "untracked.md",
            "debug.log",
            "build/out.bin",
            "src/deep/b.rs",
        ] {
            std::fs::write(root.join(name), "x\n").expect("write");
        }
        // A repository nested in an untracked directory is no file.
        std::fs::create_dir(root.join("vendor")).expect("mkdir");
        init(&root.join("vendor"));
        std::fs::write(root.join("vendor/x.txt"), "x\n").expect("write");
        let files = list(root, Path::new("")).expect("lists");
        assert_eq!(
            names(&files),
            [
                ".gitignore",
                "new\nline.txt",
                "sp ace.txt",
                "src/deep/a.rs",
                "src/deep/b.rs",
                "src/main.rs",
                "untracked.md",
            ]
        );
        assert!(!files.cut);
    }

    #[test]
    fn a_long_list_is_cut_and_says_so() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        init(root);
        for name in ["a", "b", "c", "d"] {
            std::fs::write(root.join(name), "x\n").expect("write");
        }
        git(root, &["add", "a", "b"]);
        let top = Path::new("");
        let files = list_up_to(root, top, 3).expect("lists");
        assert_eq!(names(&files), ["a", "b", "c"]);
        assert!(files.cut);
        let files = list_up_to(root, top, 4).expect("lists");
        assert_eq!(files.paths.len(), 4);
        assert!(!files.cut, "exactly the limit");
    }

    #[test]
    fn a_directory_lists_its_own_files_relative_to_the_root() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        init(root);
        std::fs::write(root.join(".gitignore"), "*.log\n").expect("write");
        for name in ["src/deep", "src2", "s*"] {
            std::fs::create_dir_all(root.join(name)).expect("mkdir");
        }
        for name in ["src/a.rs", "src/deep/b.rs", "src2/c.rs", "srcfile", "s*/x"] {
            std::fs::write(root.join(name), "x\n").expect("write");
        }
        git(root, &["add", "-A"]);
        for name in ["src/new.txt", "src/debug.log"] {
            std::fs::write(root.join(name), "x\n").expect("write");
        }
        let files = list(root, Path::new("src")).expect("lists");
        assert_eq!(
            names(&files),
            ["src/a.rs", "src/deep/b.rs", "src/new.txt"],
            "not src2 nor srcfile"
        );
        assert!(!files.cut);
        // The path is taken as written, not as a pattern.
        let files = list(root, Path::new("s*")).expect("lists");
        assert_eq!(names(&files), ["s*/x"]);
        let files = list(root, Path::new("missing")).expect("lists");
        assert_eq!(files, Files::default());
        // Cut, a directory keeps its own files before those of the
        // directories in it.
        let files = list_up_to(root, Path::new("src"), 2).expect("lists");
        assert_eq!(names(&files), ["src/a.rs", "src/new.txt"]);
        assert!(files.cut);
    }

    #[test]
    fn outside_a_repository_it_fails() {
        let outside = TempDir::new().expect("temp dir");
        let error = list(outside.path(), Path::new("")).expect_err("not a repository");
        let message = format!("{error:#}");
        assert!(message.starts_with("git ls-files"), "{message}");
        assert!(message.contains("not a git repository"), "{message}");
    }
}
