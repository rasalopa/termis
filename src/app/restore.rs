//! The sessions of the last run: the file that keeps their directories,
//! read at start and written as the set of sessions changes, and the
//! order the start opens them in around the project asked for.

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::session::{self, Place};
use crate::git;
use crate::strings;
use crate::telemetry;
use crate::ui::projects::display_path;

/// The variable that names the user's state directory.
pub(super) const STATE_HOME_VAR: &str = "XDG_STATE_HOME";
/// The variable that names the user's home directory.
pub(super) const HOME_VAR: &str = "HOME";
/// The file's format; a file with another one is not read.
const VERSION: u64 = 1;

/// The file that keeps the sessions: `termis/sessions.json` under
/// `state_home`, the value of `XDG_STATE_HOME`, else under
/// `.local/state` in `home`, the value of `HOME`. A value that is empty
/// or relative counts as unset, as the XDG specification says; with
/// neither there is no file.
pub(super) fn file(state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute =
        |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
    let state =
        absolute(state_home).or_else(|| absolute(home).map(|home| home.join(".local/state")))?;
    Some(state.join("termis").join("sessions.json"))
}

/// The directories `file` keeps, in order: none when there is no file.
/// Fails when it cannot be read, is not JSON, has another version, or
/// keeps something other than absolute paths.
pub(super) fn read(file: &Path) -> Result<Vec<PathBuf>> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let kept: Value = serde_json::from_slice(&bytes)?;
    match kept["version"].as_u64() {
        Some(VERSION) => {}
        Some(version) => bail!("version {version} is unknown"),
        None => bail!("no version"),
    }
    let sessions = kept["sessions"].as_array().context("no sessions")?;
    sessions
        .iter()
        .map(|dir| {
            let dir = PathBuf::from(dir.as_str().context("a session that is not a path")?);
            ensure!(
                dir.is_absolute(),
                "{} is not an absolute path",
                dir.display()
            );
            Ok(dir)
        })
        .collect()
}

/// Writes `dirs` to `file`, in order, those that are UTF-8, which JSON
/// can hold: as [`telemetry::write_atomically`] does, so a crash of
/// `termis` leaves the old file or the new one, readable by its owner
/// only, in a directory made readable by its owner only when it is
/// missing. Nothing is synced to disk: the file is under a kilobyte for
/// ten sessions, which the main thread writes and renames in a fraction
/// of a millisecond, where a sync, `F_FULLFSYNC` on macOS, can take tens
/// of milliseconds. After a power loss the file may be an older one, or
/// empty, which the next start says it cannot read and writes again.
fn write(file: &Path, dirs: &[PathBuf]) -> Result<()> {
    let parent = file.parent().context("no directory")?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .context("make its directory")?;
    let sessions: Vec<&str> = dirs.iter().filter_map(|dir| dir.to_str()).collect();
    let kept = json!({ "version": VERSION, "sessions": sessions });
    let mut bytes = serde_json::to_vec_pretty(&kept)?;
    bytes.push(b'\n');
    telemetry::write_atomically(file, &bytes)?;
    Ok(())
}

/// The file that keeps the sessions, and what it keeps as far as this
/// process knows: what it read there, then what it wrote last.
pub(super) struct Kept {
    /// The file; none when there is none, or with `--spike`, which keeps
    /// nothing.
    file: Option<PathBuf>,
    /// The file as the notices name it, with the home as `~`.
    name: String,
    /// The directories the file keeps; none until it was read or
    /// written, so the first [`Kept::keep`] writes it.
    dirs: Option<Vec<PathBuf>>,
}

impl Kept {
    /// The keeper of `file`, none to keep nothing, which names it with
    /// `home` as `~`, and the directories it keeps when it is to
    /// `reopen` them, else none; or why they could not be read. When it
    /// did not read them, the first [`Kept::keep`] writes the file.
    pub(super) fn open(
        file: Option<PathBuf>,
        home: Option<&Path>,
        reopen: bool,
    ) -> (Self, Result<Vec<PathBuf>, String>) {
        let name = file
            .as_deref()
            .map(|file| display_path(file, home))
            .unwrap_or_default();
        let mut kept = Self {
            file,
            name,
            dirs: None,
        };
        let dirs = match kept.file.as_deref().filter(|_| reopen).map(read) {
            None => Ok(Vec::new()),
            Some(Ok(dirs)) => {
                kept.dirs = Some(dirs.clone());
                Ok(dirs)
            }
            Some(Err(error)) => Err(strings::sessions_unread(&kept.name, &format!("{error:#}"))),
        };
        (kept, dirs)
    }

    /// Writes `dirs`, the directories of the sessions open in the order
    /// of the list, when they are not what the file keeps. Returns why
    /// they could not be written; the next change tries again.
    pub(super) fn keep<'a>(
        &mut self,
        dirs: impl Iterator<Item = &'a Path> + Clone,
    ) -> Option<String> {
        let file = self.file.as_deref()?;
        let same = |kept: &Vec<PathBuf>| kept.iter().map(PathBuf::as_path).eq(dirs.clone());
        if self.dirs.as_ref().is_some_and(same) {
            return None;
        }
        let dirs: Vec<PathBuf> = dirs.map(Path::to_path_buf).collect();
        let written = write(file, &dirs);
        self.dirs = Some(dirs);
        written
            .err()
            .map(|error| strings::sessions_unsaved(&self.name, &format!("{error:#}")))
    }
}

/// Where the sessions of the last run open around the one asked for, in
/// the order of the list: the saved directories that are still
/// directories, made canonical as `o` makes them, each once, in their
/// order; the one asked for in its place among them, else after them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Order {
    /// The saved directories before the one asked for.
    before: Vec<PathBuf>,
    /// The saved directories after it.
    after: Vec<PathBuf>,
    /// The saved directories that are not directories any more.
    gone: Vec<PathBuf>,
}

impl Order {
    /// Where the `saved` directories go around `requested`, the
    /// directory asked for, which opens whether it was saved or not.
    fn new(saved: Vec<PathBuf>, requested: &Path) -> Self {
        let requested = requested
            .canonicalize()
            .unwrap_or_else(|_| requested.to_path_buf());
        let mut order = Self::default();
        let mut passed = false;
        for dir in saved {
            match dir.canonicalize().ok().filter(|dir| dir.is_dir()) {
                None => order.gone.push(dir),
                Some(dir) if dir == requested => passed = true,
                Some(dir) if order.before.contains(&dir) || order.after.contains(&dir) => {}
                Some(dir) if passed => order.after.push(dir),
                Some(dir) => order.before.push(dir),
            }
        }
        order
    }
}

/// The sessions of the last run as the start opens them around the one
/// asked for, and what is said of those it does not open.
pub(super) struct Reopen {
    /// The places of the sessions before the one asked for, in order.
    pub(super) before: Vec<Place>,
    /// The places of those after it.
    pub(super) after: Vec<Place>,
    /// Why some do not open, in order: the file could not be read; the
    /// directories gone, the first named; a directory git could not tell
    /// about, by its project.
    pub(super) notices: Vec<String>,
}

impl Reopen {
    /// Where the directories the file kept, `saved`, or why it could
    /// not be read, open around `requested`, the directory asked for,
    /// each inside the repository git finds around it, with `home` shown
    /// as `~`. Git runs here, before the screen is taken, as it does for
    /// the directory asked for.
    pub(super) fn new(
        saved: Result<Vec<PathBuf>, String>,
        requested: &Path,
        home: Option<&Path>,
    ) -> Self {
        let mut notices = Vec::new();
        let saved = saved.unwrap_or_else(|text| {
            notices.push(text);
            Vec::new()
        });
        let order = Order::new(saved, requested);
        if let Some((first, others)) = order.gone.split_first() {
            let path = display_path(first, home);
            notices.push(strings::sessions_gone(&path, others.len()));
        }
        let mut place = |dir: PathBuf| match git::repo_root(&dir) {
            Ok(repo) => Some(Place::new(dir, repo, home)),
            Err(error) => {
                let project = session::project_name(&dir);
                notices.push(strings::for_project(&project, &format!("{error:#}")));
                None
            }
        };
        let before = order.before.into_iter().filter_map(&mut place).collect();
        let after = order.after.into_iter().filter_map(&mut place).collect();
        Self {
            before,
            after,
            notices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::init;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use tempfile::TempDir;

    /// A temporary directory, and its canonical path, which the sessions'
    /// directories are.
    fn temporary() -> (TempDir, PathBuf) {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        (temporary, base)
    }

    /// The names of what `dir` holds, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
            .collect();
        names.sort();
        names
    }

    /// The mode bits of what is at `path`.
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("there").permissions().mode() & 0o777
    }

    #[test]
    fn the_file_is_under_the_state_home_else_under_the_home() {
        let var = |value: &str| Some(OsString::from(value));
        let in_state = Some(PathBuf::from("/state/termis/sessions.json"));
        assert_eq!(file(var("/state"), var("/home/me")), in_state);
        assert_eq!(file(var("/state"), None), in_state);
        let in_home = Some(PathBuf::from("/home/me/.local/state/termis/sessions.json"));
        assert_eq!(file(None, var("/home/me")), in_home);
        // Empty or relative counts as unset.
        assert_eq!(file(var(""), var("/home/me")), in_home);
        assert_eq!(file(var("state"), var("/home/me")), in_home);
        assert_eq!(file(None, None), None);
        assert_eq!(file(var(""), var("")), None);
        assert_eq!(file(None, var("home")), None);
    }

    #[test]
    fn what_is_written_is_read_back_in_order() {
        let (_temporary, base) = temporary();
        let file = base.join("state/termis/sessions.json");
        assert_eq!(read(&file).expect("no file"), Vec::<PathBuf>::new());
        let dirs = [base.join("web"), PathBuf::from("/a b/ü"), base.join("api")];
        write(&file, &dirs).expect("writes");
        assert_eq!(read(&file).expect("reads"), dirs);
        let text = fs::read_to_string(&file).expect("text");
        let kept: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(kept["version"], 1);
        assert_eq!(kept["sessions"][1], "/a b/ü");
        assert!(text.ends_with("\n}\n"), "{text}");
        write(&file, &[]).expect("writes");
        assert_eq!(read(&file).expect("reads"), Vec::<PathBuf>::new());
    }

    #[test]
    fn the_file_is_replaced_whole_and_readable_by_its_owner_only() {
        let (_temporary, base) = temporary();
        let dir = base.join("state/termis");
        let file = dir.join("sessions.json");
        write(&file, &[base.join("a")]).expect("writes");
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&base.join("state")), 0o700);
        assert_eq!(mode(&file), 0o600);
        // A new file takes the old one's name: nothing is written in place.
        let first = fs::metadata(&file).expect("there").ino();
        write(&file, &[base.join("b")]).expect("writes");
        assert_ne!(fs::metadata(&file).expect("there").ino(), first);
        assert_eq!(read(&file).expect("reads"), [base.join("b")]);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(names(&dir), ["sessions.json"], "no temporary file left");
        // A temporary file a crashed run with this pid left fails one
        // write, which takes it away.
        let left = dir.join(format!("sessions.json.{}.tmp", std::process::id()));
        fs::write(&left, "half").expect("write");
        assert!(
            write(&file, &[base.join("c")]).is_err(),
            "its name is taken"
        );
        assert_eq!(names(&dir), ["sessions.json"], "it went");
        assert_eq!(read(&file).expect("reads"), [base.join("b")]);
        // One that cannot be written leaves the last one whole.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("chmod");
        let failed = write(&file, &[base.join("c")]);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(failed.is_err());
        assert_eq!(read(&file).expect("reads"), [base.join("b")]);
        assert_eq!(names(&dir), ["sessions.json"]);
        // A directory that is there keeps its mode.
        let other = base.join("other");
        fs::create_dir(&other).expect("mkdir");
        fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).expect("chmod");
        write(&other.join("sessions.json"), &[]).expect("writes");
        assert_eq!(mode(&other), 0o755);
    }

    #[test]
    fn a_path_that_is_not_utf8_is_left_out() {
        let (_temporary, base) = temporary();
        let file = base.join("sessions.json");
        let odd = PathBuf::from(OsStr::from_bytes(b"/tmp/\xffodd"));
        write(&file, &[base.join("a"), odd.clone(), base.join("b")]).expect("writes");
        assert_eq!(
            read(&file).expect("reads"),
            [base.join("a"), base.join("b")]
        );
        // The same sessions open, that one among them, write nothing again.
        let (mut kept, _) = Kept::open(Some(file.clone()), None, false);
        let open = [base.join("a"), odd];
        assert_eq!(kept.keep(open.iter().map(PathBuf::as_path)), None);
        assert_eq!(read(&file).expect("reads"), [base.join("a")]);
        fs::remove_file(&file).expect("rm");
        assert_eq!(kept.keep(open.iter().map(PathBuf::as_path)), None);
        assert!(!file.exists(), "nothing changed, nothing written");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_said_and_written_at_the_first_keep() {
        let (_temporary, base) = temporary();
        let file = base.join("sessions.json");
        for (text, reason) in [
            ("not json", "expected ident at line 1 column 2"),
            (r#"{"version":2,"sessions":[]}"#, "version 2 is unknown"),
            (r#"{"sessions":["/a"]}"#, "no version"),
            (r#"{"version":1}"#, "no sessions"),
            (
                r#"{"version":1,"sessions":[3]}"#,
                "a session that is not a path",
            ),
            (
                r#"{"version":1,"sessions":["a"]}"#,
                "a is not an absolute path",
            ),
        ] {
            fs::write(&file, text).expect("write");
            let (mut kept, saved) = Kept::open(Some(file.clone()), Some(&base), true);
            let notice = format!("last sessions not reopened: ~/sessions.json: {reason}");
            assert_eq!(saved, Err(notice), "{text}");
            // The first keep writes it, even with no session open.
            assert_eq!(kept.keep(std::iter::empty()), None);
            assert_eq!(read(&file).expect("reads"), Vec::<PathBuf>::new());
        }
        // One this user cannot read.
        fs::write(&file, "{}").expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("chmod");
        let (_, saved) = Kept::open(Some(file.clone()), None, true);
        let notice = saved.expect_err("unreadable");
        assert!(
            notice.ends_with("Permission denied (os error 13)"),
            "{notice}"
        );
    }

    #[test]
    fn the_sessions_are_written_only_as_they_change() {
        let (_temporary, base) = temporary();
        let file = base.join("state/termis/sessions.json");
        let (a, b) = (base.join("a"), base.join("b"));
        let ino = |file: &Path| fs::metadata(file).expect("there").ino();
        // No file yet: nothing to reopen, and the first change writes.
        let (mut kept, saved) = Kept::open(Some(file.clone()), None, true);
        assert_eq!(saved, Ok(Vec::new()));
        assert_eq!(kept.keep([a.as_path()].into_iter()), None);
        assert_eq!(read(&file).expect("reads"), [a.as_path()]);
        // The same sessions are not written again.
        fs::remove_file(&file).expect("rm");
        assert_eq!(kept.keep([a.as_path()].into_iter()), None);
        assert!(!file.exists());
        // Another order is a change.
        assert_eq!(kept.keep([a.as_path(), b.as_path()].into_iter()), None);
        assert_eq!(kept.keep([b.as_path(), a.as_path()].into_iter()), None);
        assert_eq!(read(&file).expect("reads"), [b.clone(), a.clone()]);
        // Read at start, the same sessions write nothing.
        let (mut kept, saved) = Kept::open(Some(file.clone()), None, true);
        assert_eq!(saved, Ok(vec![b.clone(), a.clone()]));
        let before = ino(&file);
        assert_eq!(kept.keep([b.as_path(), a.as_path()].into_iter()), None);
        assert_eq!(ino(&file), before);
        // With `--no-restore` the file is not read, and the sessions open
        // are written at once.
        let (mut kept, saved) = Kept::open(Some(file.clone()), None, false);
        assert_eq!(saved, Ok(Vec::new()));
        assert_eq!(kept.keep([b.as_path(), a.as_path()].into_iter()), None);
        assert_ne!(ino(&file), before);
        // Without a file, as with `--spike`, nothing is read or written.
        let (mut kept, saved) = Kept::open(None, None, true);
        assert_eq!(saved, Ok(Vec::new()));
        assert_eq!(kept.keep([a.as_path()].into_iter()), None);
        assert_eq!(read(&file).expect("reads"), [b.clone(), a.clone()]);
        // A file that cannot be written says why, and is tried again at
        // the next change.
        let blocked = base.join("blocked");
        fs::write(&blocked, "").expect("write");
        let (mut kept, _) = Kept::open(Some(blocked.join("sessions.json")), Some(&base), true);
        let notice = kept.keep([a.as_path()].into_iter()).expect("a notice");
        assert!(
            notice.starts_with("sessions not saved: ~/blocked/sessions.json: make its directory: "),
            "{notice}"
        );
        assert_eq!(kept.keep([a.as_path()].into_iter()), None, "no change");
        assert!(
            kept.keep([b.as_path()].into_iter()).is_some(),
            "tried again"
        );
    }

    #[test]
    fn the_saved_sessions_open_in_their_order_around_the_one_asked_for() {
        let (_temporary, base) = temporary();
        let [a, b, c, d] = ["a", "b", "c", "d"].map(|name| base.join(name));
        for dir in [&a, &b, &c, &d] {
            fs::create_dir(dir).expect("mkdir");
        }
        let saved = vec![a.clone(), b.clone(), c.clone()];
        let around = |order: Order| (order.before, order.after);
        // A new one goes after them.
        let order = Order::new(saved.clone(), &d);
        assert!(order.gone.is_empty());
        assert_eq!(around(order), (saved.clone(), vec![]));
        // One among them keeps its place, and does not open twice.
        let order = Order::new(saved.clone(), &b);
        assert_eq!(around(order), (vec![a.clone()], vec![c.clone()]));
        let order = Order::new(saved.clone(), &a);
        assert_eq!(around(order), (vec![], vec![b.clone(), c.clone()]));
        // Whatever path names it.
        let link = base.join("link");
        symlink(&b, &link).expect("link");
        let order = Order::new(saved.clone(), &link);
        assert_eq!(around(order), (vec![a.clone()], vec![c.clone()]));
        // A saved one opens once, by its canonical path.
        let twice = vec![a.clone(), base.join("c/../a"), link.clone(), c.clone()];
        assert_eq!(around(Order::new(twice, &d)).0, saved);
        // Nothing saved: only the one asked for.
        assert_eq!(Order::new(Vec::new(), &d), Order::default());
        // One gone, or not a directory any more, is left out.
        let file = base.join("file");
        fs::write(&file, "").expect("write");
        let gone = base.join("gone");
        let order = Order::new(vec![gone.clone(), a.clone(), file.clone(), c.clone()], &c);
        assert_eq!(order.gone, [gone, file]);
        assert_eq!(around(order), (vec![a.clone()], vec![]));
    }

    #[test]
    fn the_places_have_their_repository_and_the_notices_say_what_does_not_open() {
        let (_temporary, base) = temporary();
        let repo = base.join("repo");
        let inside = repo.join("src");
        let plain = base.join("plain");
        for dir in [&inside, &plain] {
            fs::create_dir_all(dir).expect("mkdir");
        }
        init(&repo);
        let saved = vec![
            inside.clone(),
            base.join("gone"),
            base.join("old"),
            plain.clone(),
        ];
        let reopen = Reopen::new(Ok(saved), &base, Some(&base));
        let cwds: Vec<&Path> = reopen
            .before
            .iter()
            .map(|place| place.cwd.as_path())
            .collect();
        assert_eq!(cwds, [inside.as_path(), plain.as_path()]);
        assert_eq!(reopen.before[0].repo.as_deref(), Some(repo.as_path()));
        assert_eq!(reopen.before[0].project, "repo");
        assert_eq!(reopen.before[1].repo, None);
        assert!(reopen.after.is_empty());
        assert_eq!(reopen.notices, ["not reopened: ~/gone and 1 more are gone"]);
        // The one asked for in their middle.
        let reopen = Reopen::new(Ok(vec![inside.clone(), plain.clone()]), &inside, None);
        assert!(reopen.before.is_empty());
        assert_eq!(reopen.after.len(), 1);
        assert!(reopen.notices.is_empty());
        let gone = base.join("gone");
        let reopen = Reopen::new(Ok(vec![plain.clone(), gone.clone()]), &plain, None);
        assert!(reopen.before.is_empty() && reopen.after.is_empty());
        let notice = format!("not reopened: {} is gone", gone.display());
        assert_eq!(reopen.notices, [notice]);
        // A file that could not be read is said, and nothing reopens.
        let unread = Err("last sessions not reopened: why".to_owned());
        let reopen = Reopen::new(unread, &plain, None);
        assert!(reopen.before.is_empty() && reopen.after.is_empty());
        assert_eq!(reopen.notices, ["last sessions not reopened: why"]);
        // With `--no-restore`, or without a file as with `--spike`, only
        // the one asked for opens.
        let file = base.join("sessions.json");
        write(&file, std::slice::from_ref(&plain)).expect("writes");
        for (file, reopen) in [(Some(file), false), (None, true)] {
            let (_, saved) = Kept::open(file, None, reopen);
            let reopen = Reopen::new(saved, &inside, None);
            assert!(reopen.before.is_empty() && reopen.after.is_empty());
            assert!(reopen.notices.is_empty());
        }
    }
}
