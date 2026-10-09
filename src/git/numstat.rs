//! `git diff --numstat -z`, run and parsed: lines added and removed per
//! path, in the index or in the work tree.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Lines added and removed in one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lines {
    Text {
        added: u32,
        deleted: u32,
    },
    /// Git did not count: the content is binary.
    Binary,
}

impl Lines {
    /// No lines either way.
    pub const NONE: Self = Self::Text {
        added: 0,
        deleted: 0,
    };
}

/// The lines of one path. For a submodule the counts describe its
/// recorded commit, not its files: one line each way when the commit
/// changed, none when only its work tree did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Numstat {
    /// The path, relative to the repository root; the new one for a
    /// rename or a copy.
    pub path: PathBuf,
    pub lines: Lines,
}

/// Lines staged in the index against `HEAD`, for the repository
/// containing `dir`. A path in conflict is listed with no lines.
pub fn index(dir: &Path) -> Result<Vec<Numstat>> {
    read(dir, &["--cached"])
}

/// Lines changed in the work tree against the index, for the repository
/// containing `dir`. A path in conflict counts against its own side.
pub fn worktree(dir: &Path) -> Result<Vec<Numstat>> {
    read(dir, &[])
}

/// Renames follow `diff.renames`, as they do in status, so both agree on
/// which paths exist. Paths are root-relative whatever `diff.relative`
/// says.
fn read(dir: &Path, extra: &[&str]) -> Result<Vec<Numstat>> {
    let args = [
        "--no-optional-locks",
        "diff",
        "--numstat",
        "-z",
        "--no-relative",
    ]
    .into_iter()
    .chain(extra.iter().copied());
    let output = super::run(dir, args).context("git diff --numstat")?;
    parse(&output)
}

/// Lines of the untracked file at `path`, as status prints it, relative
/// to `root`, the repository root as [`super::repo_root`] returns it;
/// counted against nothing. Fails for a directory, such as an embedded
/// repository: git would look for a file inside it instead.
pub fn untracked(root: &Path, path: &Path) -> Result<Lines> {
    let metadata = std::fs::symlink_metadata(root.join(path))
        .with_context(|| format!("read {}", path.display()))?;
    if metadata.is_dir() {
        bail!("{} is a directory", path.display());
    }
    // Prefixed so that a file named `-` is not read as standard input.
    let local = Path::new(".").join(path);
    let args = ["diff", "--no-index", "--numstat", "-z", "--", "/dev/null"]
        .map(OsStr::new)
        .into_iter()
        .chain([local.as_os_str()]);
    // Git exits with 1 when the two sides differ, which is the point; an
    // empty file still gets its record, with no lines.
    let output = super::run_accepting(root, args, &[1]).context("git diff --no-index")?;
    let stats = parse(&output)?;
    match stats.as_slice() {
        [stat] => Ok(stat.lines),
        // Git exits with 1 and nothing on stdout when it cannot read the
        // file; its stderr is lost with the exit code accepted.
        [] => bail!(
            "git diff --no-index printed no record for {}",
            path.display()
        ),
        _ => bail!(
            "git diff --no-index printed {} records for {}",
            stats.len(),
            path.display()
        ),
    }
}

/// Parses the output of `git diff --numstat -z`.
///
/// Each record is `<added>\t<deleted>\t<path>` ended by NUL, with `-` for
/// both counts when the content is binary. A rename or copy has an empty
/// path and is followed by two more fields: the old path and the new one.
/// A path in conflict is printed twice, the unmerged pair first with no
/// lines and then the real count; the last record for a path wins.
pub fn parse(output: &[u8]) -> Result<Vec<Numstat>> {
    let mut stats: Vec<Numstat> = Vec::new();
    let mut fields = output.split(|&byte| byte == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            // The terminator of the last record.
            continue;
        }
        let mut parts = record.splitn(3, |&byte| byte == b'\t');
        let (Some(added), Some(deleted), Some(path_field)) =
            (parts.next(), parts.next(), parts.next())
        else {
            bail!(
                "malformed numstat record {:?}",
                String::from_utf8_lossy(record)
            );
        };
        let lines = match (added, deleted) {
            (b"-", b"-") => Lines::Binary,
            _ => Lines::Text {
                added: count(added)?,
                deleted: count(deleted)?,
            },
        };
        let path = if path_field.is_empty() {
            let mut next = || fields.next().filter(|field| !field.is_empty());
            let _old = next().context("rename without the old path")?;
            let new = next().context("rename without the new path")?;
            path(new)
        } else {
            path(path_field)
        };
        match stats.last_mut() {
            Some(last) if last.path == path => last.lines = lines,
            _ => stats.push(Numstat { path, lines }),
        }
    }
    Ok(stats)
}

/// A line count field.
fn count(field: &[u8]) -> Result<u32> {
    std::str::from_utf8(field)
        .ok()
        .and_then(|text| text.parse().ok())
        .with_context(|| {
            format!(
                "malformed numstat count {:?}",
                String::from_utf8_lossy(field)
            )
        })
}

/// A path field, kept as the bytes git printed.
fn path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{config, git, init};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Recorded from git 2.54: the index with a staged binary, a staged
    /// line and a staged rename (`--cached`), and the work tree with a
    /// deleted file and two edited ones.
    const INDEX: &[u8] = b"-\t-\tbin.dat\x001\t0\tkeep.txt\x000\t0\t\x00old.txt\x00new.txt\x00";
    const WORKTREE: &[u8] = b"0\t1\tdel.txt\x001\t0\tkeep.txt\x002\t0\tnew.txt\x00";
    /// `git diff --no-index --numstat -z -- /dev/null newdir/f.txt`.
    const UNTRACKED: &[u8] = b"1\t0\t\x00/dev/null\x00newdir/f.txt\x00";
    /// The work tree during a merge conflict on `keep.txt`.
    const CONFLICT: &[u8] = b"0\t0\tkeep.txt\x004\t0\tkeep.txt\x001\t0\tother.txt\x00";

    fn stat(path: &str, added: u32, deleted: u32) -> Numstat {
        Numstat {
            path: PathBuf::from(path),
            lines: Lines::Text { added, deleted },
        }
    }

    #[test]
    fn parses_the_recorded_output() {
        assert_eq!(
            parse(INDEX).expect("parses"),
            vec![
                Numstat {
                    path: PathBuf::from("bin.dat"),
                    lines: Lines::Binary,
                },
                stat("keep.txt", 1, 0),
                stat("new.txt", 0, 0),
            ]
        );
        assert_eq!(
            parse(WORKTREE).expect("parses"),
            vec![
                stat("del.txt", 0, 1),
                stat("keep.txt", 1, 0),
                stat("new.txt", 2, 0)
            ]
        );
        assert_eq!(
            parse(UNTRACKED).expect("parses"),
            vec![stat("newdir/f.txt", 1, 0)]
        );
        assert_eq!(
            parse(CONFLICT).expect("parses"),
            vec![stat("keep.txt", 4, 0), stat("other.txt", 1, 0)],
            "the last record for a path wins"
        );
        assert_eq!(parse(b"").expect("empty"), vec![]);
        assert_eq!(
            parse(b"3\t4\tsp ace\ttab.txt\x00").expect("parses"),
            vec![stat("sp ace\ttab.txt", 3, 4)],
            "the path keeps its tabs"
        );
    }

    #[test]
    fn rejects_malformed_records() {
        assert!(parse(b"1\t2\x00").is_err(), "no path");
        assert!(parse(b"x\t2\tf\x00").is_err(), "bad count");
        assert!(parse(b"-\t2\tf\x00").is_err(), "half binary");
        assert!(
            parse(b"0\t0\t\x00old\x00").is_err(),
            "rename without the new path"
        );
        assert!(parse(b"garbage").is_err());
    }

    #[test]
    fn counts_untracked_files_from_the_root() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/f.txt"), "a\nb\n").expect("write");
        assert_eq!(
            untracked(dir.path(), Path::new("sub/f.txt")).expect("counts"),
            Lines::Text {
                added: 2,
                deleted: 0
            }
        );
        std::fs::write(dir.path().join("empty"), "").expect("write");
        assert_eq!(
            untracked(dir.path(), Path::new("empty")).expect("counts"),
            Lines::NONE
        );
        std::fs::write(dir.path().join("b.dat"), b"\x00\x01\x02").expect("write");
        assert_eq!(
            untracked(dir.path(), Path::new("b.dat")).expect("counts"),
            Lines::Binary
        );
        std::fs::write(dir.path().join("-"), "dash\n").expect("write");
        assert_eq!(
            untracked(dir.path(), Path::new("-")).expect("counts"),
            Lines::Text {
                added: 1,
                deleted: 0
            },
            "a file named - is not standard input"
        );
        assert!(
            untracked(dir.path(), Path::new("sub")).is_err(),
            "directory"
        );
        assert!(untracked(dir.path(), Path::new("missing")).is_err());

        // Git itself failing is an error, not a count of nothing.
        let unreadable = dir.path().join("secret");
        std::fs::write(&unreadable, "s\n").expect("write");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
        let error = untracked(dir.path(), Path::new("secret")).expect_err("unreadable");
        assert!(format!("{error:#}").contains("secret"), "{error:#}");
    }

    #[test]
    fn reads_the_index_and_the_work_tree_root_relative() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("mkdir");
        let file = sub.join("f.txt");
        std::fs::write(&file, "a\nb\n").expect("write");
        git(dir.path(), &["add", "sub/f.txt"]);
        assert_eq!(
            index(dir.path()).expect("index"),
            vec![stat("sub/f.txt", 2, 0)]
        );
        assert_eq!(worktree(dir.path()).expect("worktree"), vec![]);

        git(dir.path(), &["commit", "-q", "-m", "one"]);
        std::fs::write(&file, "a\nc\nd\n").expect("write");
        std::fs::write(dir.path().join("top.txt"), "t\n").expect("write");
        git(dir.path(), &["add", "-N", "top.txt"]);
        assert_eq!(index(dir.path()).expect("index"), vec![]);
        // From a subdirectory, with diff.relative set, the paths and the
        // set of files stay those of the root.
        config(dir.path(), "diff.relative", "true");
        assert_eq!(
            worktree(&sub).expect("worktree"),
            vec![stat("sub/f.txt", 2, 1), stat("top.txt", 1, 0)]
        );

        git(dir.path(), &["mv", "sub/f.txt", "g.txt"]);
        assert_eq!(index(&sub).expect("index"), vec![stat("g.txt", 0, 0)]);
        assert_eq!(
            worktree(dir.path()).expect("worktree"),
            vec![stat("g.txt", 2, 1), stat("top.txt", 1, 0)]
        );
    }
}
