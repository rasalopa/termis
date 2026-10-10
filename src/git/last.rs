//! `git log -1`, run and parsed: the commit `HEAD` points to, for the
//! project card.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// The commit `HEAD` points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Last {
    /// The full hash.
    pub oid: String,
    /// The short hash: the shortest prefix that names one object in the
    /// repository, at least as long as `core.abbrev` asks, seven by
    /// default and more in a large repository. It can grow as objects
    /// are added.
    pub hash: String,
    /// When it was committed, in seconds since the Unix epoch; an amend
    /// or a rebase sets it anew.
    pub time: i64,
    /// The subject line, re-coded to UTF-8 from the encoding the commit
    /// records, with tabs as spaces, other control characters and colour
    /// sequences removed and the ends trimmed; empty when the message
    /// is.
    pub subject: String,
}

/// The commit `HEAD` points to in the repository containing `dir`.
/// Fails before the first commit, as `git log` does.
pub fn read(dir: &Path) -> Result<Last> {
    let output = super::run(
        dir,
        [
            "--no-optional-locks",
            "log",
            "-1",
            "--no-show-signature",
            "--no-notes",
            "--encoding=UTF-8",
            "--format=%H%x00%h%x00%ct%x00%s",
        ],
    )
    .context("git log -1")?;
    parse(&output)
}

/// Parses `<oid> NUL <hash> NUL <seconds> NUL <subject> LF`. A subject
/// that is still not UTF-8 has what is not replaced.
pub fn parse(output: &[u8]) -> Result<Last> {
    let mut fields = output.splitn(4, |&byte| byte == 0);
    let (Some(oid), Some(hash), Some(time), Some(subject)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        bail!("malformed log record {:?}", String::from_utf8_lossy(output));
    };
    let oid = hex(oid).context("malformed commit hash")?;
    let hash = hex(hash).context("malformed short hash")?;
    let time = std::str::from_utf8(time)
        .ok()
        .and_then(|time| time.parse().ok())
        .with_context(|| format!("malformed commit time {:?}", String::from_utf8_lossy(time)))?;
    let subject = String::from_utf8_lossy(subject).replace('\t', " ");
    Ok(Last {
        oid: oid.to_owned(),
        hash: hash.to_owned(),
        time,
        subject: super::clean(&subject),
    })
}

/// `field` when it is a non-empty hexadecimal string.
fn hex(field: &[u8]) -> Result<&str> {
    std::str::from_utf8(field)
        .ok()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .with_context(|| format!("{:?} is not hexadecimal", String::from_utf8_lossy(field)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, git_with, init};
    use tempfile::TempDir;

    const OID: &str = "3fe4913d6f2a9a9f4e2d2b8d7c1b0a9e8f7d6c5b";

    #[test]
    fn parses_the_record() {
        let record = format!("{OID}\x003fe4913\x001791540000\x00first subject\n");
        assert_eq!(
            parse(record.as_bytes()).expect("parses"),
            Last {
                oid: OID.into(),
                hash: "3fe4913".into(),
                time: 1_791_540_000,
                subject: "first subject".into(),
            }
        );
        let last = parse(b"abc\x00abc\x001\x00\n").expect("an empty message");
        assert_eq!(last.subject, "");
        let last = parse(b"abc\x00abc\x001\x00  tab\tand esc \x1b[31mred \x07\n").expect("parses");
        assert_eq!(last.subject, "tab and esc red", "cleaned and trimmed");
        let last = parse(b"abc\x00abc\x001\x00caf\xe9\n").expect("not UTF-8");
        assert_eq!(last.subject, "caf\u{FFFD}");
        assert_eq!(
            parse(b"abc\x00abc\x00-5\x00x").expect("no newline").time,
            -5
        );
    }

    #[test]
    fn rejects_malformed_records() {
        assert!(parse(b"").is_err());
        assert!(parse(b"abc\x00abc\x001\n").is_err(), "three fields");
        assert!(parse(b"\x00abc\x001\x00s\n").is_err(), "an empty hash");
        assert!(
            parse(b"abc\x00\x001\x00s\n").is_err(),
            "an empty short hash"
        );
        assert!(parse(b"xyz\x00abc\x001\x00s\n").is_err(), "not hex");
        assert!(parse(b"abc\x00abc\x00soon\x00s\n").is_err(), "not a time");
        assert!(
            parse(b"abc\x00abc\x00\xff\x00s\n").is_err(),
            "a time that is not text"
        );
    }

    #[test]
    fn reads_the_commit_head_points_to() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        assert!(read(dir.path()).is_err(), "no commits yet");
        std::fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        git(dir.path(), &["add", "a.txt"]);
        let dated = [("GIT_COMMITTER_DATE", "2026-10-09T10:00:00+0000")];
        git_with(
            dir.path(),
            &["commit", "-q", "-m", "first subject", "-m", "body"],
            &dated,
        );
        let last = read(dir.path()).expect("reads");
        let oid = crate::git::run(dir.path(), ["rev-parse", "HEAD"]).expect("rev-parse");
        let oid = String::from_utf8(oid).expect("hex");
        assert_eq!(last.oid, oid.trim_end());
        assert!(
            oid.starts_with(&last.hash),
            "{} is short for {oid}",
            last.hash
        );
        assert_eq!(last.hash.len(), 7, "core.abbrev as init() pins it");
        assert_eq!(last.time, 1_791_540_000, "the committer date");
        assert_eq!(last.subject, "first subject");

        // A subject in the encoding the commit records comes back as
        // UTF-8.
        let message = dir.path().join("message");
        std::fs::write(&message, b"caf\xe9\n").expect("write");
        git(
            dir.path(),
            &[
                "-c",
                "i18n.commitEncoding=ISO-8859-1",
                "commit",
                "-q",
                "--allow-empty",
                "-F",
                "message",
            ],
        );
        assert_eq!(read(dir.path()).expect("reads").subject, "café");

        git(dir.path(), &["checkout", "-q", "--detach"]);
        let detached = read(dir.path()).expect("detached HEAD too");
        assert_eq!(detached.subject, "café");
    }
}
