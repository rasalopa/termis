//! The account each directory last opened as, kept in a file beside the
//! one of the sessions of the last run while there are several accounts
//! (ADR-0014).

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{config, restore};
use crate::projects::read::open_regular;
use crate::strings;
use crate::ui::projects::display_path;

/// The file's name, beside the sessions'.
const NAME: &str = "accounts.json";
/// The file's format; a file with another one is not read.
const VERSION: u64 = 1;
/// The most bytes the file holds: 1 MiB.
const LIMIT: u64 = 1024 * 1024;
/// The most directories it keeps, those set last; past them the oldest
/// go.
const KEPT: usize = 1000;

/// The file: `accounts.json` beside the file of the sessions, as
/// [`restore::file`] finds it under `state_home` or `home`, the values
/// of `XDG_STATE_HOME` and `HOME`; none when there is none.
pub(super) fn file(state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    restore::file(state_home, home).map(|sessions| sessions.with_file_name(NAME))
}

/// The directories `file` keeps, each with the name of its account, the
/// one set last first: none when there is no file. Fails when it cannot
/// be read, is not a regular file, holds more than [`LIMIT`] bytes, is
/// not JSON, has another version, or keeps a directory that is not an
/// absolute path or an account's name the config file would not take.
/// A directory kept twice counts once, the first time.
fn read(file: &Path) -> Result<Vec<(PathBuf, String)>> {
    let opened = match open_regular(file) {
        Ok(Some(opened)) => opened,
        Ok(None) => bail!("not a regular file"),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    // A byte past the limit, since the file can grow after it was opened.
    let mut bytes = Vec::new();
    opened.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len()).is_ok_and(|len| len <= LIMIT),
        "larger than 1 MiB"
    );
    let kept: Value = serde_json::from_slice(&bytes)?;
    match kept["version"].as_u64() {
        Some(VERSION) => {}
        Some(version) => bail!("version {version} is unknown"),
        None => bail!("no version"),
    }
    let entries = kept["accounts"].as_array().context("no accounts")?;
    let mut found: Vec<(PathBuf, String)> = Vec::with_capacity(entries.len());
    let mut seen = HashSet::with_capacity(entries.len());
    for entry in entries {
        let dir = PathBuf::from(entry["dir"].as_str().context("an entry without a dir")?);
        ensure!(
            dir.is_absolute(),
            "{} is not an absolute path",
            dir.display()
        );
        let account = entry["account"]
            .as_str()
            .context("an entry without an account")?;
        ensure!(
            config::is_account_name(account),
            "an entry with an account's name termis does not take"
        );
        if seen.insert(dir.clone()) {
            found.push((dir, account.to_owned()));
        }
    }
    Ok(found)
}

/// Writes `entries` to `file`, in order, the first [`KEPT`] of those
/// whose directory is UTF-8, which JSON can hold, as the sessions' file
/// is written: readable by its owner only.
fn write(file: &Path, entries: &[(PathBuf, String)]) -> Result<()> {
    let accounts: Vec<Value> = entries
        .iter()
        .filter_map(|(dir, account)| Some(json!({ "dir": dir.to_str()?, "account": account })))
        .take(KEPT)
        .collect();
    let kept = json!({ "version": VERSION, "accounts": accounts });
    let mut bytes = serde_json::to_vec_pretty(&kept)?;
    bytes.push(b'\n');
    restore::write_kept(file, &bytes)
}

/// The account each directory last opened as, as far as this process
/// knows: what it read in the file, then what it wrote.
#[derive(Debug, Default)]
pub(super) struct Remembered {
    /// The file; none to keep nothing, as with one account.
    file: Option<PathBuf>,
    /// The file as the notices name it, with the home as `~`.
    name: String,
    /// Each directory with its account's name, the one set last first.
    entries: Vec<(PathBuf, String)>,
    /// Whether [`Remembered::remember`] writes the file; not with
    /// `--spike`, which only reads it.
    writes: bool,
}

impl Remembered {
    /// What `file` keeps, none to keep nothing, which notices name with
    /// `home` as `~`, and which [`Remembered::remember`] `writes` or not.
    /// Fails, naming the file, when it cannot be read: the accounts of
    /// the sessions would not be known.
    pub(super) fn open(file: Option<PathBuf>, home: Option<&Path>, writes: bool) -> Result<Self> {
        let Some(file) = file else {
            return Ok(Self::default());
        };
        let name = display_path(&file, home);
        let entries = read(&file).with_context(|| format!("read {name}"))?;
        Ok(Self {
            file: Some(file),
            name,
            entries,
            writes,
        })
    }

    /// The name of the account `dir` last opened as, if any.
    pub(super) fn get(&self, dir: &Path) -> Option<&str> {
        self.entries
            .iter()
            .find(|(kept, _)| kept == dir)
            .map(|(_, account)| account.as_str())
    }

    /// Notes that `dir` opened as the account named `account`. When that
    /// is new, the file is read again, so that what another `termis`
    /// wrote since stays, and written with `dir` first. Returns why it
    /// could not be written; the next change tries again.
    pub(super) fn remember(&mut self, dir: &Path, account: &str) -> Option<String> {
        let file = self.file.as_deref().filter(|_| self.writes)?;
        if self.get(dir) == Some(account) {
            return None;
        }
        if let Ok(now) = read(file) {
            self.entries = now;
        }
        self.entries.retain(|(kept, _)| kept != dir);
        self.entries
            .insert(0, (dir.to_path_buf(), account.to_owned()));
        self.entries.truncate(KEPT);
        write(file, &self.entries)
            .err()
            .map(|error| strings::accounts_unsaved(&self.name, &format!("{error:#}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// A temporary directory, and its canonical path, which the sessions'
    /// directories are.
    fn temporary() -> (TempDir, PathBuf) {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        (temporary, base)
    }

    /// The mode bits of what is at `path`.
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("there").permissions().mode() & 0o777
    }

    #[test]
    fn the_file_is_beside_the_sessions() {
        let var = |value: &str| Some(OsString::from(value));
        assert_eq!(
            file(var("/state"), None),
            Some(PathBuf::from("/state/termis/accounts.json"))
        );
        assert_eq!(
            file(None, var("/home/me")),
            Some(PathBuf::from("/home/me/.local/state/termis/accounts.json"))
        );
        assert_eq!(file(None, None), None);
    }

    #[test]
    fn an_account_is_remembered_and_read_back_the_last_set_first() {
        let (_temporary, base) = temporary();
        let path = base.join("state/termis/accounts.json");
        let (api, notes) = (base.join("api"), base.join("notes"));
        let mut kept = Remembered::open(Some(path.clone()), None, true).expect("no file yet");
        assert_eq!(kept.get(&api), None);
        assert_eq!(kept.remember(&api, "work"), None);
        assert_eq!(kept.remember(&notes, "personal"), None);
        assert_eq!(kept.get(&api), Some("work"));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&base.join("state/termis")), 0o700);
        let again = Remembered::open(Some(path.clone()), None, true).expect("reads");
        assert_eq!(
            again.entries,
            [
                (notes.clone(), "personal".to_owned()),
                (api.clone(), "work".to_owned()),
            ]
        );
        let text = fs::read_to_string(&path).expect("text");
        let written: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(written["version"], 1);
        assert_eq!(written["accounts"][1]["account"], "work");
        // The same account again writes nothing; another moves it first.
        fs::remove_file(&path).expect("rm");
        assert_eq!(kept.remember(&api, "work"), None);
        assert!(!path.exists(), "nothing changed, nothing written");
        assert_eq!(kept.remember(&api, "personal"), None);
        let again = Remembered::open(Some(path.clone()), None, true).expect("reads");
        assert_eq!(again.entries, [(api.clone(), "personal".to_owned())]);
        // Read only, as with `--spike`, nothing is written.
        let mut read_only = Remembered::open(Some(path.clone()), None, false).expect("reads");
        assert_eq!(read_only.get(&api), Some("personal"));
        fs::remove_file(&path).expect("rm");
        assert_eq!(read_only.remember(&base.join("new"), "work"), None);
        assert!(!path.exists());
        // Without a file nothing is kept.
        let mut none = Remembered::open(None, None, true).expect("none");
        assert_eq!(none.remember(&api, "work"), None);
        assert_eq!(none.get(&api), None);
        // One that cannot be written says why, by the file's name.
        let blocked = base.join("blocked");
        fs::create_dir(&blocked).expect("mkdir");
        let mut kept =
            Remembered::open(Some(blocked.join("accounts.json")), Some(&base), true).expect("none");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).expect("chmod");
        let notice = kept.remember(&api, "work");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("chmod");
        let notice = notice.expect("a notice");
        assert!(
            notice.starts_with("account not saved: ~/blocked/accounts.json: "),
            "{notice}"
        );
        assert!(notice.contains("Permission denied"), "{notice}");
    }

    #[test]
    fn what_another_termis_wrote_stays_and_the_oldest_go_past_the_most_kept() {
        let (_temporary, base) = temporary();
        let path = base.join("accounts.json");
        let mut one = Remembered::open(Some(path.clone()), None, true).expect("no file");
        let mut two = Remembered::open(Some(path.clone()), None, true).expect("no file");
        assert_eq!(one.remember(&base.join("a"), "work"), None);
        assert_eq!(two.remember(&base.join("b"), "personal"), None);
        let both = Remembered::open(Some(path.clone()), None, true).expect("reads");
        assert_eq!(both.get(&base.join("a")), Some("work"), "one's stays");
        assert_eq!(both.get(&base.join("b")), Some("personal"));
        // A full file: the next directory set lets the oldest go.
        let full: Vec<(PathBuf, String)> = (0..KEPT)
            .map(|n| (base.join(format!("p{n}")), "work".to_owned()))
            .collect();
        write(&path, &full).expect("writes");
        let mut many = Remembered::open(Some(path.clone()), None, true).expect("reads");
        assert_eq!(many.entries, full);
        assert_eq!(many.remember(&base.join("new"), "personal"), None);
        let read = Remembered::open(Some(path), None, true).expect("reads");
        assert_eq!(read.entries.len(), KEPT);
        assert_eq!(read.entries[0], (base.join("new"), "personal".to_owned()));
        assert_eq!(read.get(&base.join("p0")), Some("work"));
        let last = base.join(format!("p{}", KEPT - 1));
        assert_eq!(read.get(&last), None, "the oldest went");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_refused_by_name() {
        let (_temporary, base) = temporary();
        let path = base.join("accounts.json");
        for (text, reason) in [
            ("not json", "expected ident at line 1 column 2"),
            ("[]", "no version"),
            (r#"{"version":2,"accounts":[]}"#, "version 2 is unknown"),
            (r#"{"version":1}"#, "no accounts"),
            (
                r#"{"version":1,"accounts":[{"account":"w"}]}"#,
                "an entry without a dir",
            ),
            (
                r#"{"version":1,"accounts":[{"dir":"a","account":"w"}]}"#,
                "a is not an absolute path",
            ),
            (
                r#"{"version":1,"accounts":[{"dir":"/a","account":7}]}"#,
                "an entry without an account",
            ),
            (
                r#"{"version":1,"accounts":[{"dir":"/a","account":"w\u001b[2J"}]}"#,
                "an entry with an account's name termis does not take",
            ),
            (
                r#"{"version":1,"accounts":[{"dir":"/a","account":"a234567890123456x"}]}"#,
                "an entry with an account's name termis does not take",
            ),
        ] {
            fs::write(&path, text).expect("write");
            let error = Remembered::open(Some(path.clone()), Some(&base), true).expect_err(text);
            assert_eq!(
                format!("{error:#}"),
                format!("read ~/accounts.json: {reason}")
            );
        }
        fs::write(&path, vec![b' '; 1024 * 1024 + 1]).expect("write");
        let error = Remembered::open(Some(path.clone()), None, true).expect_err("too large");
        assert!(format!("{error:#}").ends_with("larger than 1 MiB"));
        fs::remove_file(&path).expect("rm");
        fs::create_dir(&path).expect("mkdir");
        let error = Remembered::open(Some(path.clone()), None, true).expect_err("a directory");
        assert!(format!("{error:#}").ends_with("not a regular file"));
        // A directory kept twice counts once, the first time.
        fs::remove_dir(&path).expect("rmdir");
        let twice =
            r#"{"version":1,"accounts":[{"dir":"/a","account":"w"},{"dir":"/a","account":"p"}]}"#;
        fs::write(&path, twice).expect("write");
        let kept = Remembered::open(Some(path), None, true).expect("reads");
        assert_eq!(kept.entries, [(PathBuf::from("/a"), "w".to_owned())]);
    }
}
