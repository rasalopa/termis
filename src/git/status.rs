//! `git status --porcelain=v2 --branch -z`, run and parsed.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What happened to a path, in the index or in the work tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Modified,
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
    /// In conflict, whatever left it so: a merge, a rebase, a cherry-pick
    /// or a stash; only in the work tree.
    Unmerged,
    /// Not in the index; only in the work tree.
    Untracked,
}

impl Change {
    /// The change for one letter of the `XY` field; none for `.`.
    fn from_code(code: u8) -> Result<Option<Self>> {
        Ok(Some(match code {
            b'.' => return Ok(None),
            b'M' => Self::Modified,
            b'T' => Self::TypeChanged,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'U' => Self::Unmerged,
            other => bail!("unknown status code {:?}", other as char),
        }))
    }

    /// The letter git uses for the change.
    pub fn letter(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::TypeChanged => 'T',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::Unmerged => 'U',
            Self::Untracked => '?',
        }
    }
}

/// One path in the status output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The path, relative to the repository root; the new one for a
    /// rename or a copy.
    pub path: PathBuf,
    /// The path before a rename or a copy.
    pub original: Option<PathBuf>,
    /// The change in the index against `HEAD`; none when they agree.
    pub index: Option<Change>,
    /// The change in the work tree against the index; none when they
    /// agree.
    pub worktree: Option<Change>,
    /// Whether the path is a submodule. Its work tree change may be
    /// content or commits inside it, which `git add` of the path does not
    /// stage.
    pub submodule: bool,
}

impl Entry {
    /// Whether the path has something staged.
    pub fn staged(&self) -> bool {
        self.index.is_some()
    }

    /// Whether the path has something to stage.
    pub fn unstaged(&self) -> bool {
        self.worktree.is_some()
    }
}

/// The `# branch.*` headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Branch {
    /// The branch name; none while `HEAD` is detached or cannot be read.
    pub head: Option<String>,
    /// The commit `HEAD` points to; none before the first commit.
    pub oid: Option<String>,
    /// The upstream branch, when one is configured.
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream, when its ref exists and
    /// git counted them.
    pub ahead_behind: Option<(u32, u32)>,
}

/// The status of a working tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Branch,
    /// Changed, unmerged and untracked paths, in git's order.
    pub entries: Vec<Entry>,
}

impl Status {
    /// The entries with something staged.
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.staged())
    }

    /// The entries with something to stage.
    pub fn unstaged(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.unstaged())
    }
}

/// Reads the status of the repository containing `dir`; the paths come
/// back relative to its root wherever `dir` is inside it. Untracked files
/// are listed one by one, not folded into their directory.
pub fn read(dir: &Path) -> Result<Status> {
    let output = super::run(
        dir,
        [
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    )
    .context("git status")?;
    parse(&output)
}

/// Parses the output of `git status --porcelain=v2 --branch -z`.
///
/// Records are NUL-terminated; a rename or copy record is followed by one
/// more field holding the original path. Ignored paths (`!`) and headers
/// other than `branch.*` are skipped.
pub fn parse(output: &[u8]) -> Result<Status> {
    let mut status = Status::default();
    let mut records = output
        .split(|&byte| byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let (kind, rest) = match record {
            [kind, b' ', rest @ ..] => (*kind, rest),
            _ => bail!(
                "malformed status record {:?}",
                String::from_utf8_lossy(record)
            ),
        };
        match kind {
            b'#' => header(&mut status.branch, rest)?,
            b'1' => status.entries.push(ordinary(rest)?),
            b'2' => {
                let original = records
                    .next()
                    .context("rename record without the original path")?;
                status.entries.push(renamed_or_copied(rest, original)?);
            }
            b'u' => status.entries.push(unmerged(rest)?),
            b'?' => status.entries.push(Entry {
                path: path(rest),
                original: None,
                index: None,
                worktree: Some(Change::Untracked),
                submodule: false,
            }),
            b'!' => {}
            other => bail!("unknown status record type {:?}", other as char),
        }
    }
    Ok(status)
}

/// Applies one `# <key> <value>` header; keys other than `branch.*` are
/// ignored.
fn header(branch: &mut Branch, rest: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(rest).context("non-UTF-8 status header")?;
    let (key, value) = text.split_once(' ').unwrap_or((text, ""));
    match key {
        "branch.oid" => branch.oid = (value != "(initial)").then(|| value.to_owned()),
        "branch.head" => {
            branch.head = (!matches!(value, "(detached)" | "(unknown)")).then(|| value.to_owned());
        }
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        // `+? -?` when git was told not to count.
        "branch.ab" if value == "+? -?" => branch.ahead_behind = None,
        "branch.ab" => {
            let (ahead, behind) = value
                .split_once(' ')
                .context("malformed branch.ab header")?;
            let count = |field: &str, sign: char| -> Result<u32> {
                field
                    .strip_prefix(sign)
                    .and_then(|digits| digits.parse().ok())
                    .with_context(|| format!("malformed branch.ab field {field:?}"))
            };
            branch.ahead_behind = Some((count(ahead, '+')?, count(behind, '-')?));
        }
        _ => {}
    }
    Ok(())
}

/// A `1` record: `<XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`.
fn ordinary(rest: &[u8]) -> Result<Entry> {
    let fields: Vec<&[u8]> = rest.splitn(8, |&byte| byte == b' ').collect();
    let [xy, sub, _, _, _, _, _, path_field] = fields[..] else {
        bail!("malformed changed record");
    };
    let (index, worktree) = changes(xy)?;
    Ok(Entry {
        path: path(path_field),
        original: None,
        index,
        worktree,
        submodule: submodule(sub),
    })
}

/// A `2` record: `<XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>`,
/// with the original path in the next field.
fn renamed_or_copied(rest: &[u8], original: &[u8]) -> Result<Entry> {
    let fields: Vec<&[u8]> = rest.splitn(9, |&byte| byte == b' ').collect();
    let [xy, sub, _, _, _, _, _, _, path_field] = fields[..] else {
        bail!("malformed rename record");
    };
    let (index, worktree) = changes(xy)?;
    Ok(Entry {
        path: path(path_field),
        original: Some(path(original)),
        index,
        worktree,
        submodule: submodule(sub),
    })
}

/// A `u` record: `<XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`.
/// The conflict is something to resolve in the work tree, whatever the
/// letters say about each side.
fn unmerged(rest: &[u8]) -> Result<Entry> {
    let fields: Vec<&[u8]> = rest.splitn(10, |&byte| byte == b' ').collect();
    let [_, sub, _, _, _, _, _, _, _, path_field] = fields[..] else {
        bail!("malformed unmerged record");
    };
    Ok(Entry {
        path: path(path_field),
        original: None,
        index: None,
        worktree: Some(Change::Unmerged),
        submodule: submodule(sub),
    })
}

/// The index and work tree changes of an `XY` field.
fn changes(xy: &[u8]) -> Result<(Option<Change>, Option<Change>)> {
    let [x, y] = xy else {
        bail!("malformed XY field {:?}", String::from_utf8_lossy(xy));
    };
    Ok((Change::from_code(*x)?, Change::from_code(*y)?))
}

/// Whether a `<sub>` field marks a submodule: `S...` rather than `N...`.
fn submodule(sub: &[u8]) -> bool {
    sub.first() == Some(&b'S')
}

/// A path field, kept as the bytes git printed.
fn path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, init};
    use tempfile::TempDir;

    /// Recorded from git 2.54; see tests/fixtures/README.md.
    const RECORDED: &[u8] = include_bytes!("../../tests/fixtures/git/status.bin");
    const CONFLICT: &[u8] = include_bytes!("../../tests/fixtures/git/status-conflict.bin");

    fn entry(path: &str, index: Option<Change>, worktree: Option<Change>) -> Entry {
        Entry {
            path: PathBuf::from(path),
            original: None,
            index,
            worktree,
            submodule: false,
        }
    }

    #[test]
    fn parses_the_recorded_output() {
        let status = parse(RECORDED).expect("parses");
        assert_eq!(
            status.branch,
            Branch {
                head: Some("main".into()),
                oid: Some("a0951918416dbe24501444f7c0c9644c5f5d49cb".into()),
                upstream: None,
                ahead_behind: None,
            }
        );
        assert_eq!(
            status.entries,
            vec![
                entry("bin.dat", Some(Change::Added), None),
                entry("del.txt", None, Some(Change::Deleted)),
                entry("keep.txt", Some(Change::Modified), Some(Change::Modified)),
                Entry {
                    original: Some(PathBuf::from("old.txt")),
                    ..entry("new.txt", Some(Change::Renamed), Some(Change::Modified))
                },
                entry("newdir/f.txt", None, Some(Change::Untracked)),
                entry("sp ace.txt", None, Some(Change::Untracked)),
            ]
        );
        let staged: Vec<_> = status.staged().map(|e| e.path.clone()).collect();
        assert_eq!(
            staged,
            ["bin.dat", "keep.txt", "new.txt"].map(PathBuf::from)
        );
        let unstaged: Vec<_> = status.unstaged().map(|e| e.path.clone()).collect();
        assert_eq!(
            unstaged,
            [
                "del.txt",
                "keep.txt",
                "new.txt",
                "newdir/f.txt",
                "sp ace.txt"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn parses_the_branch_headers() {
        let status = parse(b"# branch.oid (initial)\0# branch.head main\0? f\0").expect("parses");
        assert_eq!(status.branch.oid, None);
        assert_eq!(status.branch.head.as_deref(), Some("main"));

        let status = parse(
            b"# branch.oid abc\0# branch.head (detached)\0# branch.upstream origin/main\0# branch.ab +2 -1\0",
        )
        .expect("parses");
        assert_eq!(status.branch.head, None);
        assert_eq!(status.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.branch.ahead_behind, Some((2, 1)));
        assert!(status.entries.is_empty());

        let status =
            parse(b"# branch.head (unknown)\0# branch.ab +? -?\0# stash 2\0").expect("parses");
        assert_eq!(status.branch.head, None, "an unreadable HEAD is no branch");
        assert_eq!(status.branch.ahead_behind, None, "uncounted");

        // Unknown headers are ignored; a bad count is not.
        assert!(parse(b"# branch.future x y\0").is_ok());
        assert!(parse(b"# branch.ab +x -1\0").is_err());
        assert!(parse(b"# branch.ab +1\0").is_err());
    }

    #[test]
    fn parses_unmerged_submodule_and_ignored_records() {
        let status = parse(
            b"u UU N... 100644 100644 100644 100644 a b c both.txt\0\
1 .M S.M. 160000 160000 160000 a b sm\0\
! build/\0\
? new\0",
        )
        .expect("parses");
        assert_eq!(
            status.entries,
            vec![
                entry("both.txt", None, Some(Change::Unmerged)),
                Entry {
                    submodule: true,
                    ..entry("sm", None, Some(Change::Modified))
                },
                entry("new", None, Some(Change::Untracked)),
            ]
        );
    }

    #[test]
    fn a_recorded_conflict_is_unmerged() {
        let status = parse(CONFLICT).expect("parses");
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        assert_eq!(
            status.entries,
            vec![entry("both.txt", None, Some(Change::Unmerged))]
        );
    }

    #[test]
    fn letters_match_git() {
        let letters: String = [
            Change::Modified,
            Change::TypeChanged,
            Change::Added,
            Change::Deleted,
            Change::Renamed,
            Change::Copied,
            Change::Unmerged,
            Change::Untracked,
        ]
        .map(Change::letter)
        .into_iter()
        .collect();
        assert_eq!(letters, "MTADRCU?");
    }

    #[test]
    fn rejects_malformed_records() {
        assert!(parse(b"x something\0").is_err(), "unknown type");
        assert!(parse(b"1 MM\0").is_err(), "too few fields");
        assert!(
            parse(b"1 MMM N... 100644 100644 100644 a b f\0").is_err(),
            "bad XY"
        );
        assert!(
            parse(b"1 ZM N... 100644 100644 100644 a b f\0").is_err(),
            "bad code"
        );
        assert!(
            parse(b"2 R. N... 100644 100644 100644 a b R100 new\0").is_err(),
            "no original"
        );
        assert!(parse(b"garbage").is_err());
        assert_eq!(parse(b"").expect("empty"), Status::default());
    }

    #[test]
    fn reads_a_temporary_repository() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/a.txt"), "a\n").expect("write");
        let status = read(dir.path()).expect("status");
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        assert_eq!(status.branch.oid, None);
        assert_eq!(
            status.entries,
            vec![entry("sub/a.txt", None, Some(Change::Untracked))],
            "untracked files are listed one by one"
        );

        git(dir.path(), &["add", "sub/a.txt"]);
        let from_subdirectory = read(&dir.path().join("sub")).expect("status");
        assert_eq!(
            from_subdirectory.entries,
            vec![entry("sub/a.txt", Some(Change::Added), None)],
            "paths are relative to the root"
        );

        git(dir.path(), &["commit", "-q", "-m", "one"]);
        let status = read(dir.path()).expect("status");
        assert!(status.entries.is_empty());
        assert!(status.branch.oid.is_some());

        let outside = TempDir::new().expect("temp dir");
        let error = read(outside.path()).expect_err("not a repository");
        assert!(
            format!("{error:#}").contains("not a git repository"),
            "{error:#}"
        );
    }
}
