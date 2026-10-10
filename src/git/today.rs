//! `git log --since=midnight --numstat`, run and parsed: what the
//! current author committed since local midnight.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::numstat::{self, Lines};

/// Files that record resolved dependency versions, generated and not
/// written by hand, whose lines say nothing about the day's work. Any
/// `.lock` file counts as one too.
const LOCKFILES: [&str; 10] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lockb",
    "go.sum",
    "go.work.sum",
    "packages.lock.json",
    "gradle.lockfile",
    "Package.resolved",
    ".terraform.lock.hcl",
];

/// Commits made since local midnight and the lines they changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Today {
    /// Commits by the current author in the history of `HEAD`, merges
    /// left out.
    pub commits: u32,
    /// Lines added over those commits, lockfiles and binaries left out;
    /// a submodule's recorded commit counts one line each way, as in
    /// [`numstat`].
    pub added: u32,
    /// Lines deleted over the same.
    pub deleted: u32,
}

/// What the author of the next commit, as `git var GIT_AUTHOR_IDENT`
/// names them, committed since local midnight in the history of `HEAD`,
/// for the repository containing `dir`. The address is matched whole
/// against the author header as written, case ignored, mailmap left
/// aside. Merges are not counted. The walk stops at the first commit
/// whose committer date is older than midnight, as `git log --since`
/// has it: a rebased commit counts on the day it was recommitted, and a
/// commit made today under one with an older date is not seen. Renames
/// follow `diff.renames`. Fails before the first commit, as `git log`
/// does.
pub fn read(dir: &Path) -> Result<Today> {
    let ident = super::run(dir, ["var", "GIT_AUTHOR_IDENT"]).context("git var")?;
    let email = email_of(&ident).context("git var printed no author email")?;
    let mut author = OsString::from("--author=<");
    author.push(OsStr::from_bytes(email));
    author.push(">");
    let args = [
        "--no-optional-locks",
        "log",
        "--since=midnight",
        "--no-merges",
        "--fixed-strings",
        "--regexp-ignore-case",
        "--no-use-mailmap",
        "--numstat",
        "-z",
        "--no-relative",
        "--no-ext-diff",
        "--no-show-signature",
        "--no-notes",
        "--format=%x00%H",
    ]
    .map(OsStr::new)
    .into_iter()
    .chain([author.as_os_str()]);
    let output = super::run(dir, args).context("git log --numstat")?;
    parse(&output)
}

/// The address between the angle brackets of an ident line, `Name
/// <address> timestamp zone`. Bytes, since the name need not be UTF-8.
fn email_of(ident: &[u8]) -> Option<&[u8]> {
    let end = ident.iter().rposition(|&byte| byte == b'>')?;
    let start = ident[..end].iter().rposition(|&byte| byte == b'<')?;
    Some(&ident[start + 1..end])
}

/// Parses the output of `git log --numstat -z --format=%x00%H`.
///
/// Each commit is an empty record, the hash, then its numstat records
/// as [`numstat::parse`] reads them, the first one starting with the
/// newline that separates a commit from its diff; a commit that changed
/// nothing has none. Records are NUL-terminated.
pub fn parse(output: &[u8]) -> Result<Today> {
    let mut today = Today::default();
    let mut records = output.split(|&byte| byte == 0).peekable();
    while let Some(record) = records.next() {
        if !record.is_empty() {
            bail!("malformed log record {:?}", String::from_utf8_lossy(record));
        }
        let Some(hash) = records.next() else {
            // The terminator of the last record, or no commits at all.
            break;
        };
        if hash.is_empty() || !hash.iter().all(u8::is_ascii_hexdigit) {
            bail!("malformed commit hash {:?}", String::from_utf8_lossy(hash));
        }
        today.commits = today.commits.saturating_add(1);
        let mut stats = Vec::new();
        while let Some(record) = records.next_if(|record| !record.is_empty()) {
            let record = match stats.is_empty() {
                true => record.strip_prefix(b"\n").unwrap_or(record),
                false => record,
            };
            stats.extend_from_slice(record);
            stats.push(0);
        }
        for stat in numstat::parse(&stats)? {
            if lockfile(&stat.path) {
                continue;
            }
            if let Lines::Text { added, deleted } = stat.lines {
                today.added = today.added.saturating_add(added);
                today.deleted = today.deleted.saturating_add(deleted);
            }
        }
    }
    Ok(today)
}

/// Whether `path` names a lockfile: one of [`LOCKFILES`] or any `.lock`
/// file, wherever it is.
fn lockfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".lock") || LOCKFILES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, git_with, init};
    use tempfile::TempDir;

    /// Recorded from git 2.54; see tests/fixtures/README.md.
    const LOG: &[u8] = include_bytes!("../../tests/fixtures/git/log-numstat.bin");

    #[test]
    fn parses_the_recorded_output() {
        // Four commits: the lockfile and the binary are not counted, the
        // empty commit is, the rename counts its edited lines only.
        assert_eq!(
            parse(LOG).expect("parses"),
            Today {
                commits: 4,
                added: 5,
                deleted: 1,
            }
        );
        assert_eq!(parse(b"").expect("no commits"), Today::default());
        assert_eq!(
            parse(b"\x00abc\x00").expect("one empty commit"),
            Today {
                commits: 1,
                added: 0,
                deleted: 0,
            }
        );
        assert_eq!(
            parse(b"\x00abc\x00\n1\t2\ta\x003\t4\tb\x00\x00def\x00").expect("parses"),
            Today {
                commits: 2,
                added: 4,
                deleted: 6,
            },
            "a commit with changes, then one without"
        );
        assert_eq!(
            parse(b"\x00abc\x00\n0\t0\t\x00old\x00new\x001\t0\tc\x00").expect("parses"),
            Today {
                commits: 1,
                added: 1,
                deleted: 0,
            },
            "a rename first, with the newline before its counts"
        );
        assert_eq!(
            parse(b"\x00abc\x00\n1\t0\t\nnl.txt\x00").expect("parses"),
            Today {
                commits: 1,
                added: 1,
                deleted: 0,
            },
            "only the separating newline goes, a path keeps its own"
        );
    }

    #[test]
    fn rejects_malformed_output() {
        assert!(parse(b"1\t2\ta\x00").is_err(), "no commit boundary");
        assert!(parse(b"\x00not a hash\x00").is_err());
        assert!(parse(b"\x00\x00").is_err(), "an empty hash");
        assert!(parse(b"\x00abc\x00\nx\t2\ta\x00").is_err(), "a bad count");
    }

    #[test]
    fn lockfiles_are_known_by_name() {
        for name in [
            "Cargo.lock",
            "sub/Cargo.lock",
            "yarn.lock",
            "package-lock.json",
            "a/b/pnpm-lock.yaml",
            "go.sum",
            "flake.lock",
            "Package.resolved",
            "infra/.terraform.lock.hcl",
        ] {
            assert!(lockfile(Path::new(name)), "{name}");
        }
        for name in [
            "Cargo.toml",
            "lock.rs",
            "locked.txt",
            "sum.go",
            "go.sum.bak",
        ] {
            assert!(!lockfile(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn the_email_is_the_last_bracketed_field() {
        assert_eq!(
            email_of(b"t <t@example.com> 1791586383 -0600\n"),
            Some(&b"t@example.com"[..])
        );
        assert_eq!(
            email_of(b"Jos\xe9 <j@example.com> 1 +0000"),
            Some(&b"j@example.com"[..]),
            "a name in another encoding"
        );
        assert_eq!(email_of(b"a <b> c <d@e> 1 +0000"), Some(&b"d@e"[..]));
        assert_eq!(email_of(b"t <> 1 +0000"), Some(&b""[..]));
        assert_eq!(email_of(b"no brackets"), None);
    }

    #[test]
    fn counts_the_authors_commits_since_midnight() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        assert!(read(dir.path()).is_err(), "no commits yet");

        // The address git would commit with here, whatever the
        // environment of the test says; the commits below are made
        // under it or under another one.
        let ident = crate::git::run(dir.path(), ["var", "GIT_AUTHOR_IDENT"]).expect("git var");
        let mine = String::from_utf8(email_of(&ident).expect("an address").to_vec())
            .expect("an ASCII address in this test");
        let as_me = [("GIT_AUTHOR_EMAIL", mine.as_str())];

        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\n").expect("write");
        git(dir.path(), &["add", "a.txt"]);
        let yesterday = [
            ("GIT_AUTHOR_DATE", "2000-01-02T03:04:05+0000"),
            ("GIT_COMMITTER_DATE", "2000-01-02T03:04:05+0000"),
        ];
        git_with(dir.path(), &["commit", "-q", "-m", "old"], &yesterday);
        assert_eq!(read(dir.path()).expect("reads"), Today::default());

        std::fs::write(&file, "a\nb\nc\n").expect("write");
        std::fs::write(dir.path().join("Cargo.lock"), "x\ny\n").expect("write");
        git(dir.path(), &["add", "-A"]);
        git_with(dir.path(), &["commit", "-q", "-m", "one"], &as_me);
        git_with(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "empty"],
            &as_me,
        );
        std::fs::write(&file, "a\nc\n").expect("write");
        git_with(dir.path(), &["commit", "-q", "-am", "two"], &as_me);
        assert_eq!(
            read(dir.path()).expect("reads"),
            Today {
                commits: 3,
                added: 2,
                deleted: 1,
            }
        );

        // Another author's commit and a merge are left out.
        std::fs::write(dir.path().join("o.txt"), "o\n").expect("write");
        git(dir.path(), &["add", "o.txt"]);
        let other = [("GIT_AUTHOR_EMAIL", "other@example.com")];
        git_with(dir.path(), &["commit", "-q", "-m", "theirs"], &other);
        git(dir.path(), &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.path().join("s.txt"), "s\n").expect("write");
        git(dir.path(), &["add", "s.txt"]);
        git_with(dir.path(), &["commit", "-q", "-m", "side"], &as_me);
        git(dir.path(), &["checkout", "-q", "main"]);
        git_with(
            dir.path(),
            &["merge", "-q", "--no-ff", "-m", "merge", "side"],
            &as_me,
        );
        assert_eq!(
            read(dir.path()).expect("reads"),
            Today {
                commits: 4,
                added: 3,
                deleted: 1,
            }
        );

        // The address is matched whole, whatever its case, and as
        // written: a mailmap that renames it changes nothing.
        let longer = format!("x{mine}");
        let near = [("GIT_AUTHOR_EMAIL", longer.as_str())];
        git_with(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "near"],
            &near,
        );
        let upper = mine.to_uppercase();
        let upper = [("GIT_AUTHOR_EMAIL", upper.as_str())];
        git_with(
            dir.path(),
            &["commit", "-q", "--allow-empty", "-m", "upper"],
            &upper,
        );
        assert_eq!(read(dir.path()).expect("reads").commits, 5);
        std::fs::write(
            dir.path().join(".mailmap"),
            format!("Other <other@example.com> <{mine}>\n"),
        )
        .expect("write");
        assert_eq!(read(dir.path()).expect("reads").commits, 5);
    }
}
