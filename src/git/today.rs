//! `git log --since=midnight --numstat`, run and parsed: what the
//! current author committed since local midnight, and in which hours.

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

/// Commits made since local midnight, the lines they changed and the
/// hours they were made in.
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
    /// The same commits by the hour of their committer date in the local
    /// time zone: `hours[9]` counts those made from 09:00 to 09:59. One
    /// dated on a later day, as a clock set ahead can date it, counts in
    /// its hour of that day.
    pub hours: [u32; 24],
    /// The local hour they were read in.
    pub hour: u8,
}

/// What the author of the next commit, as `git var GIT_AUTHOR_IDENT`
/// names them, committed since local midnight in the history of `HEAD`,
/// for the repository containing `dir`. The address is matched whole
/// against the author header as written, case ignored, mailmap left
/// aside. Merges are not counted. The walk stops at the first commit
/// whose committer date is older than midnight, as `git log --since`
/// has it: a rebased commit counts on the day it was recommitted, and a
/// commit made today under one with an older date is not seen. Renames
/// follow `diff.renames`. The hour it is read in comes from the time and
/// zone that end the same ident. Fails before the first commit, as `git
/// log` does.
pub fn read(dir: &Path) -> Result<Today> {
    let ident = super::run(dir, ["var", "GIT_AUTHOR_IDENT"]).context("git var")?;
    let email = email_of(&ident).context("git var printed no author email")?;
    let hour = hour_of(&ident).context("git var printed no date")?;
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
        // The committer hour after the hash, in the local zone.
        "--date=format-local:%H",
        "--format=%x00%H %cd",
    ]
    .map(OsStr::new)
    .into_iter()
    .chain([author.as_os_str()]);
    let output = super::run(dir, args).context("git log --numstat")?;
    Ok(Today {
        hour,
        ..parse(&output)?
    })
}

/// The address between the angle brackets of an ident line, `Name
/// <address> timestamp zone`. Bytes, since the name need not be UTF-8.
fn email_of(ident: &[u8]) -> Option<&[u8]> {
    let end = ident.iter().rposition(|&byte| byte == b'>')?;
    let start = ident[..end].iter().rposition(|&byte| byte == b'<')?;
    Some(&ident[start + 1..end])
}

/// The local hour of an ident line, `Name <address> timestamp zone`: its
/// time in seconds moved by its `±hhmm` zone, within the day.
fn hour_of(ident: &[u8]) -> Option<u8> {
    let end = ident.iter().rposition(|&byte| byte == b'>')?;
    let date = std::str::from_utf8(&ident[end + 1..]).ok()?;
    let mut fields = date.split_whitespace();
    let seconds: i64 = fields.next()?.parse().ok()?;
    let (sign, zone) = match fields.next()?.split_at_checked(1)? {
        ("+", zone) => (1, zone),
        ("-", zone) => (-1, zone),
        _ => return None,
    };
    if zone.len() != 4 || !zone.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = zone[..2].parse().ok()?;
    let minutes: i64 = zone[2..].parse().ok()?;
    let local = seconds.checked_add(sign * (hours * 3600 + minutes * 60))?;
    u8::try_from(local.rem_euclid(86_400) / 3600).ok()
}

/// The hour of a commit's record, `<hash> <hh>`, as the log's format
/// prints it; none when the record is not one.
fn commit_hour(record: &[u8]) -> Option<usize> {
    let (hash, hour) = record.split_at_checked(record.len().checked_sub(3)?)?;
    let &[b' ', tens, ones] = hour else {
        return None;
    };
    if hash.is_empty()
        || !hash.iter().all(u8::is_ascii_hexdigit)
        || !tens.is_ascii_digit()
        || !ones.is_ascii_digit()
    {
        return None;
    }
    let hour = usize::from(tens - b'0') * 10 + usize::from(ones - b'0');
    (hour < 24).then_some(hour)
}

/// Parses the output of `git log --numstat -z '--format=%x00%H %cd'
/// --date=format-local:%H`.
///
/// Each commit is an empty record, the hash and the hour of its
/// committer date, then its numstat records as [`numstat::parse`] reads
/// them, the first one starting with the newline that separates a
/// commit from its diff; a commit that changed nothing has none. Records
/// are NUL-terminated.
pub fn parse(output: &[u8]) -> Result<Today> {
    let mut today = Today::default();
    let mut records = output.split(|&byte| byte == 0).peekable();
    while let Some(record) = records.next() {
        if !record.is_empty() {
            bail!("malformed log record {:?}", String::from_utf8_lossy(record));
        }
        let Some(commit) = records.next() else {
            // The terminator of the last record, or no commits at all.
            break;
        };
        let hour = commit_hour(commit)
            .with_context(|| format!("malformed commit {:?}", String::from_utf8_lossy(commit)))?;
        today.commits = today.commits.saturating_add(1);
        today.hours[hour] = today.hours[hour].saturating_add(1);
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

    /// Hours with these counts, the others none.
    fn hours(counts: &[(usize, u32)]) -> [u32; 24] {
        let mut hours = [0; 24];
        for &(hour, count) in counts {
            hours[hour] = count;
        }
        hours
    }

    #[test]
    fn parses_the_recorded_output() {
        // Four commits: the lockfile and the binary are not counted, the
        // empty commit is, the rename counts its edited lines only. The
        // hour of the read is read's to fill in.
        assert_eq!(
            parse(LOG).expect("parses"),
            Today {
                commits: 4,
                added: 5,
                deleted: 1,
                hours: hours(&[(9, 2), (14, 1), (23, 1)]),
                hour: 0,
            }
        );
        assert_eq!(parse(b"").expect("no commits"), Today::default());
        assert_eq!(
            parse(b"\x00abc 09\x00").expect("one empty commit"),
            Today {
                commits: 1,
                added: 0,
                deleted: 0,
                hours: hours(&[(9, 1)]),
                hour: 0,
            }
        );
        assert_eq!(
            parse(b"\x00abc 09\x00\n1\t2\ta\x003\t4\tb\x00\x00def 10\x00").expect("parses"),
            Today {
                commits: 2,
                added: 4,
                deleted: 6,
                hours: hours(&[(9, 1), (10, 1)]),
                hour: 0,
            },
            "a commit with changes, then one without"
        );
        assert_eq!(
            parse(b"\x00abc 09\x00\n0\t0\t\x00old\x00new\x001\t0\tc\x00").expect("parses"),
            Today {
                commits: 1,
                added: 1,
                deleted: 0,
                hours: hours(&[(9, 1)]),
                hour: 0,
            },
            "a rename first, with the newline before its counts"
        );
        assert_eq!(
            parse(b"\x00abc 09\x00\n1\t0\t\nnl.txt\x00").expect("parses"),
            Today {
                commits: 1,
                added: 1,
                deleted: 0,
                hours: hours(&[(9, 1)]),
                hour: 0,
            },
            "only the separating newline goes, a path keeps its own"
        );
    }

    #[test]
    fn rejects_malformed_output() {
        assert!(parse(b"1\t2\ta\x00").is_err(), "no commit boundary");
        assert!(parse(b"\x00not a hash 09\x00").is_err());
        assert!(parse(b"\x00\x00").is_err(), "an empty record");
        assert!(parse(b"\x00 09\x00").is_err(), "an empty hash");
        assert!(parse(b"\x00abc\x00").is_err(), "no hour");
        assert!(parse(b"\x00abc 24\x00").is_err(), "no such hour");
        assert!(parse(b"\x00abc 9\x00").is_err(), "one digit");
        assert!(parse(b"\x00abc x9\x00").is_err(), "not a number");
        assert!(
            parse(b"\x00abc 09\x00\nx\t2\ta\x00").is_err(),
            "a bad count"
        );
    }

    #[test]
    fn the_hour_is_the_idents_time_in_its_zone() {
        // 1791586383 is 2026-10-09 22:53:03 UTC.
        let at = |date: &str| hour_of(format!("t <t@example.com> {date}\n").as_bytes());
        assert_eq!(at("1791586383 +0000"), Some(22));
        assert_eq!(at("1791586383 -0600"), Some(16));
        assert_eq!(at("1791586383 +0230"), Some(1), "past midnight");
        assert_eq!(at("1791586383 +0545"), Some(4));
        assert_eq!(hour_of(b"a <b> c <d@e> 0 -0100"), Some(23), "the last >");
        for date in ["1791586383", "x -0600", "1 0600", "1 -06", "1 -06x0"] {
            assert_eq!(at(date), None, "{date}");
        }
        assert_eq!(hour_of(b"no brackets 1 +0000"), None);
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
        let none = read(dir.path()).expect("reads");
        assert_eq!(
            none,
            Today {
                hour: none.hour,
                ..Today::default()
            }
        );
        assert!(none.hour < 24, "{}", none.hour);
        let totals = |today: Today| (today.commits, today.added, today.deleted);

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
        assert_eq!(totals(read(dir.path()).expect("reads")), (3, 2, 1));

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
        assert_eq!(totals(read(dir.path()).expect("reads")), (4, 3, 1));

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
        let today = read(dir.path()).expect("reads");
        assert_eq!(today.commits, 5);
        // Made now, each in an hour of today.
        assert_eq!(today.hours.iter().sum::<u32>(), 5, "{:?}", today.hours);
        std::fs::write(
            dir.path().join(".mailmap"),
            format!("Other <other@example.com> <{mine}>\n"),
        )
        .expect("write");
        assert_eq!(read(dir.path()).expect("reads").commits, 5);
    }
}
