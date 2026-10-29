//! The config file: where it is, and what it sets, read once at start
//! (ADR-0011).

use std::ffi::OsString;
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use ratatui::style::Color;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use super::repo::POLL_EVERY;
use crate::keys::ModeKey;
use crate::projects::read::open_regular;
use crate::ui::layout::Widths;
use crate::ui::palette::{self, Palette};

/// The variable that names the user's config directory.
pub(super) const CONFIG_HOME_VAR: &str = "XDG_CONFIG_HOME";
/// The most bytes the file holds: 64 KiB.
const LIMIT: usize = 64 * 1024;

/// What the config file sets, each to its default where it says
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Config {
    /// The key that switches between agent mode and termis mode.
    pub(super) mode_key: ModeKey,
    /// The widths of the side panes.
    pub(super) widths: Widths,
    /// How often the repository the changes pane shows is refreshed.
    pub(super) poll: Duration,
    /// The colours termis draws with.
    pub(super) palette: Palette,
}

impl Default for Config {
    fn default() -> Self {
        File::default().into()
    }
}

impl From<File> for Config {
    fn from(file: File) -> Self {
        Self {
            mode_key: file.keys.termis_mode,
            widths: file.widths,
            poll: file.git.poll_seconds,
            palette: file.theme.palette(),
        }
    }
}

/// The file as written; a table or a key it leaves out keeps its
/// default.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", default)]
struct File {
    keys: Keys,
    #[serde(deserialize_with = "widths")]
    widths: Widths,
    git: Git,
    theme: Theme,
}

/// The `[keys]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", default)]
struct Keys {
    #[serde(deserialize_with = "termis_mode")]
    termis_mode: ModeKey,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            termis_mode: ModeKey::DEFAULT,
        }
    }
}

/// `termis-mode`: a key as [`ModeKey`] reads it.
fn termis_mode<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ModeKey, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(D::Error::custom)
}

/// The `[widths]` table, each value a side pane's percent, which
/// [`widths`] checks together.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", default)]
struct WidthsTable {
    #[serde(deserialize_with = "projects")]
    projects: u16,
    #[serde(deserialize_with = "changes")]
    changes: u16,
}

impl Default for WidthsTable {
    fn default() -> Self {
        Self {
            projects: Widths::DEFAULT.projects(),
            changes: Widths::DEFAULT.changes(),
        }
    }
}

/// `[widths]`: the side panes' percents, each checked where it is
/// written, then together, which an error puts at the table.
fn widths<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Widths, D::Error> {
    let table = WidthsTable::deserialize(deserializer)?;
    Widths::new(table.projects, table.changes).map_err(D::Error::custom)
}

/// `projects` in `[widths]`, read as any integer so that one out of
/// range gets the message of [`Widths::side`].
fn projects<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Widths::side("projects", i64::deserialize(deserializer)?).map_err(D::Error::custom)
}

/// `changes` in `[widths]`, as [`projects`].
fn changes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Widths::side("changes", i64::deserialize(deserializer)?).map_err(D::Error::custom)
}

/// The `[git]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", default)]
struct Git {
    #[serde(deserialize_with = "poll_seconds")]
    poll_seconds: Duration,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            poll_seconds: POLL_EVERY,
        }
    }
}

/// `poll-seconds`: a whole number of seconds from 1 to 60, read as any
/// integer so that one out of range gets this message too.
fn poll_seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    match u64::try_from(i64::deserialize(deserializer)?) {
        Ok(seconds @ 1..=60) => Ok(Duration::from_secs(seconds)),
        _ => Err(D::Error::custom(
            "poll-seconds is a whole number of seconds from 1 to 60",
        )),
    }
}

/// The `[theme]` table: the colours it sets, by the names of the
/// palette's.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", default)]
struct Theme {
    text: Option<Colour>,
    accent: Option<Colour>,
    asking: Option<Colour>,
    alert: Option<Colour>,
    added: Option<Colour>,
    removed: Option<Colour>,
    modified: Option<Colour>,
    muted: Option<Colour>,
    faint: Option<Colour>,
    chip: Option<Colour>,
    chip_text: Option<Colour>,
    ink: Option<Colour>,
}

impl Theme {
    /// The default palette with the colours the table sets.
    fn palette(&self) -> Palette {
        let default = Palette::DEFAULT;
        let or = |set: Option<Colour>, default| set.map_or(default, |Colour(colour)| colour);
        Palette {
            text: or(self.text, default.text),
            accent: or(self.accent, default.accent),
            asking: or(self.asking, default.asking),
            alert: or(self.alert, default.alert),
            added: or(self.added, default.added),
            removed: or(self.removed, default.removed),
            modified: or(self.modified, default.modified),
            muted: or(self.muted, default.muted),
            faint: or(self.faint, default.faint),
            chip: or(self.chip, default.chip),
            chip_text: or(self.chip_text, default.chip_text),
            ink: or(self.ink, default.ink),
        }
    }
}

/// A colour of `[theme]`, as [`palette::colour`] reads it.
#[derive(Clone, Copy)]
struct Colour(Color);

impl<'de> Deserialize<'de> for Colour {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        palette::colour(&String::deserialize(deserializer)?)
            .map(Self)
            .map_err(D::Error::custom)
    }
}

/// The config file: `termis/config.toml` under `config_home`, the value
/// of `XDG_CONFIG_HOME`, else under `.config` in `home`, the value of
/// `HOME`. A value that is empty or relative counts as unset, as the XDG
/// specification says; with neither there is no file.
pub(super) fn file(config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute =
        |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
    let config =
        absolute(config_home).or_else(|| absolute(home).map(|home| home.join(".config")))?;
    Some(config.join("termis").join("config.toml"))
}

/// What `file` sets: the defaults when there is no file. Fails when it
/// is a link to a missing file, cannot be looked at or read, is not a
/// regular file once links are followed, holds more than [`LIMIT`] bytes
/// or is not UTF-8, or when [`parse`] fails. Only a regular file is
/// opened, and without waiting for a writer, so a pipe in its place
/// cannot hold up the start.
pub(super) fn read(file: &Path) -> Result<Config> {
    let metadata = match std::fs::metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            ensure!(
                std::fs::symlink_metadata(file).is_err(),
                "a link to a missing file"
            );
            return Ok(Config::default());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(metadata.is_file(), "not a regular file");
    let opened = open_regular(file)?.context("not a regular file")?;
    // A byte past the limit, since the file can grow after it was looked
    // at.
    let mut bytes = Vec::new();
    opened.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "larger than 64 KiB");
    let text = String::from_utf8(bytes).map_err(|error| {
        let valid = &error.as_bytes()[..error.utf8_error().valid_up_to()];
        let (line, column) = place(&String::from_utf8_lossy(valid));
        anyhow!("not UTF-8 at line {line}, column {column}")
    })?;
    Ok(parse(&text)?)
}

/// The line and the column, from 1, of the character after `before`,
/// the columns counted in characters as toml counts them.
fn place(before: &str) -> (usize, usize) {
    let line = before.matches('\n').count() + 1;
    let last = before.rsplit('\n').next().unwrap_or_default();
    (line, last.chars().count() + 1)
}

/// What the file's `text` sets, with the defaults where it says nothing.
/// Fails on what is not TOML, a table or a key termis does not take, or
/// a value of the wrong type or out of range; the error names the line
/// and the column.
pub(super) fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str::<File>(text).map(Config::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use tempfile::TempDir;

    /// How long a read gets.
    const WAIT: Duration = Duration::from_secs(10);

    /// The error `text` gives: its message, and where toml puts it.
    fn error(text: &str) -> (String, String) {
        let error = parse(text).expect_err(text);
        (error.message().to_owned(), error.to_string())
    }

    #[test]
    fn the_file_is_under_the_config_home_else_under_the_home() {
        let var = |value: &str| Some(OsString::from(value));
        let in_config = Some(PathBuf::from("/config/termis/config.toml"));
        assert_eq!(file(var("/config"), var("/home/me")), in_config);
        assert_eq!(file(var("/config"), None), in_config);
        let in_home = Some(PathBuf::from("/home/me/.config/termis/config.toml"));
        assert_eq!(file(None, var("/home/me")), in_home);
        // Empty or relative counts as unset.
        assert_eq!(file(var(""), var("/home/me")), in_home);
        assert_eq!(file(var("config"), var("/home/me")), in_home);
        assert_eq!(file(None, None), None);
        assert_eq!(file(var(""), var("")), None);
        assert_eq!(file(None, var("home")), None);
    }

    #[test]
    fn what_the_file_leaves_out_keeps_its_default() {
        let defaults = Config {
            mode_key: "ctrl+q".parse().expect("allowed"),
            widths: Widths::new(20, 25).expect("allowed"),
            poll: Duration::from_secs(2),
            palette: Palette::DEFAULT,
        };
        assert_eq!(Config::default(), defaults);
        let empty = ["[git]\n", "[widths]\n", "[keys]\n", "[theme]\n"];
        for text in ["", "\n", "# nothing\n"].into_iter().chain(empty) {
            assert_eq!(parse(text).expect(text), defaults, "{text:?}");
        }
        let five = parse("[git]\npoll-seconds = 5\n").expect("five");
        assert_eq!(five.poll, Duration::from_secs(5));
        for seconds in [1, 60] {
            let text = format!("[git]\npoll-seconds = {seconds}\n");
            assert_eq!(parse(&text).expect(&text).poll.as_secs(), seconds);
        }
    }

    #[test]
    fn the_theme_sets_the_colours_it_names_and_leaves_the_others() {
        let config = parse("[theme]\naccent = \"cyan\"\n").expect("cyan");
        let cyan = Palette {
            accent: Color::Cyan,
            ..Palette::DEFAULT
        };
        assert_eq!(config.palette, cyan);
        assert_eq!(config.mode_key, ModeKey::DEFAULT, "the rest stays");
        // The palette the frame of ui's tests draws its accent red with.
        let red = parse("[theme]\naccent = \"red\"\n").expect("red").palette;
        let accent_red = Palette {
            accent: Color::Red,
            ..Palette::DEFAULT
        };
        assert_eq!(red, accent_red);
        let text =
            "[theme]\nchip-text = \"#FF00ff\"\nmuted = \"bright-black\"\ntext = \"default\"\n";
        let config = parse(text).expect("three");
        let three = Palette {
            chip_text: Color::Rgb(255, 0, 255),
            muted: Color::DarkGray,
            text: Color::Reset,
            ..Palette::DEFAULT
        };
        assert_eq!(config.palette, three);
        let every: String = [
            "text",
            "accent",
            "asking",
            "alert",
            "added",
            "removed",
            "modified",
            "muted",
            "faint",
            "chip",
            "chip-text",
            "ink",
        ]
        .map(|key| format!("{key} = \"red\"\n"))
        .concat();
        let red = parse(&format!("[theme]\n{every}"))
            .expect("every key")
            .palette;
        assert_eq!(
            red,
            Palette {
                text: Color::Red,
                accent: Color::Red,
                asking: Color::Red,
                alert: Color::Red,
                added: Color::Red,
                removed: Color::Red,
                modified: Color::Red,
                muted: Color::Red,
                faint: Color::Red,
                chip: Color::Red,
                chip_text: Color::Red,
                ink: Color::Red,
            }
        );
    }

    #[test]
    fn a_colour_termis_does_not_take_is_refused_where_it_is() {
        let (message, shown) = error("[theme]\naccent = \"teal\"\n");
        assert_eq!(
            message,
            "use #rrggbb, default, or a name such as cyan or bright-black"
        );
        assert!(shown.contains("line 2, column 10"), "{shown}");
        let (message, _) = error("[theme]\naccent = 3\n");
        assert!(message.starts_with("invalid type"), "{message}");
        let (message, shown) = error("[theme]\naccnt = \"red\"\n");
        assert!(message.contains("unknown field `accnt`"), "{message}");
        assert!(
            message.contains(
                "`text`, `accent`, `asking`, `alert`, `added`, `removed`, `modified`, `muted`, \
                 `faint`, `chip`, `chip-text`, `ink`"
            ),
            "{message}"
        );
        assert!(shown.contains("line 2, column 1"), "{shown}");
        let (message, _) = error("[theme]\nchip_text = \"red\"\n");
        assert!(message.contains("unknown field `chip_text`"), "{message}");
    }

    #[test]
    fn the_termis_mode_key_is_read_as_the_keys_read_it() {
        for text in ["ctrl+o", "Ctrl+O", "CTRL+o"] {
            let config = parse(&format!("[keys]\ntermis-mode = \"{text}\"\n")).expect(text);
            assert_eq!(config.mode_key.label(), "Ctrl+O", "{text}");
            assert_eq!(config.poll, POLL_EVERY, "the rest stays");
        }
        let (message, shown) = error("[keys]\ntermis-mode = \"ctrl+c\"\n");
        assert_eq!(
            message,
            "ctrl+c cannot be the termis-mode key: it interrupts the agent and the programs in \
             terminals"
        );
        assert!(shown.contains("line 2, column 15"), "{shown}");
        let (message, _) = error("[keys]\ntermis-mode = \"alt+q\"\n");
        assert_eq!(message, "write ctrl and a letter, as ctrl+q");
        let (message, _) = error("[keys]\ntermis-mode = 17\n");
        assert!(message.starts_with("invalid type"), "{message}");
        let (message, _) = error("[keys]\nquit = \"ctrl+w\"\n");
        assert!(message.contains("unknown field `quit`"), "{message}");
    }

    #[test]
    fn a_poll_out_of_range_or_of_another_type_is_refused_where_it_is() {
        for value in ["0", "61", "-3", "70000"] {
            let (message, shown) = error(&format!("[git]\npoll-seconds = {value}\n"));
            assert_eq!(
                message, "poll-seconds is a whole number of seconds from 1 to 60",
                "{value}"
            );
            assert!(shown.contains("line 2, column 16"), "{shown}");
        }
        for value in ["\"5\"", "1.5", "true"] {
            let (message, shown) = error(&format!("[git]\npoll-seconds = {value}\n"));
            assert!(message.starts_with("invalid type"), "{value}: {message}");
            assert!(shown.contains("line 2, column 16"), "{shown}");
        }
    }

    #[test]
    fn the_widths_are_checked_where_each_is_then_together_at_the_table() {
        for (text, projects, changes) in [
            ("projects = 15\nchanges = 40\n", 15, 40),
            ("projects = 40\nchanges = 20\n", 40, 20),
            ("changes = 40\n", 20, 40),
            ("projects = 15\n", 15, 25),
        ] {
            let text = format!("[widths]\n{text}");
            let widths = parse(&text).expect(&text).widths;
            assert_eq!(widths, Widths::new(projects, changes).expect("allowed"));
        }
        for (key, value) in [
            ("projects", "14"),
            ("projects", "41"),
            ("changes", "14"),
            ("changes", "-3"),
            ("changes", "70000"),
        ] {
            let (message, shown) = error(&format!("[widths]\n{key} = {value}\n"));
            assert_eq!(message, format!("{key} is a percent from 15 to 40"));
            let column = key.len() + 4;
            assert!(
                shown.contains(&format!("line 2, column {column}")),
                "{shown}"
            );
        }
        let (message, shown) = error("[widths]\nprojects = 40\nchanges = 25\n");
        assert_eq!(
            message,
            "projects and changes together leave the agent less than 40% of the width"
        );
        assert!(shown.contains("line 1, column 1"), "{shown}");
        let (message, _) = error("[widths]\nprojects = 20.5\n");
        assert!(message.starts_with("invalid type"), "{message}");
        let (message, _) = error("[widths]\ncenter = 40\n");
        assert!(message.contains("unknown field `center`"), "{message}");
    }

    #[test]
    fn a_table_or_a_key_termis_does_not_take_is_refused_by_name() {
        let (message, shown) = error("[git]\npol = 5\n");
        assert!(message.contains("unknown field `pol`"), "{message}");
        assert!(message.contains("poll-seconds"), "{message}");
        assert!(shown.contains("line 2, column 1"), "{shown}");
        let (message, shown) = error("[colours]\naccent = \"red\"\n");
        assert!(message.contains("unknown field `colours`"), "{message}");
        assert!(shown.contains("line 1, column 2"), "{shown}");
        // A key before any table is the file's own.
        let (message, _) = error("poll = 5\n");
        assert!(message.contains("unknown field `poll`"), "{message}");
        let (_, shown) = error("[git\npoll-seconds = 5\n");
        assert!(shown.contains("line 1"), "{shown}");
    }

    #[test]
    fn a_missing_file_gives_the_defaults_and_a_file_its_settings() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        assert_eq!(read(&path).expect("no file"), Config::default());
        fs::write(&path, "[git]\npoll-seconds = 7\n").expect("write");
        assert_eq!(read(&path).expect("reads").poll, Duration::from_secs(7));
        // Up to the limit; one byte more is too much.
        let mut text = "[git]\npoll-seconds = 7\n".to_owned();
        text.push_str(&"#".repeat(LIMIT - text.len() - 1));
        text.push('\n');
        assert_eq!(text.len(), LIMIT);
        fs::write(&path, &text).expect("write");
        assert_eq!(read(&path).expect("whole").poll, Duration::from_secs(7));
        text.push('\n');
        fs::write(&path, &text).expect("write");
        let error = format!("{:#}", read(&path).expect_err("too large"));
        assert_eq!(error, "larger than 64 KiB");
        let error = read(&path.join("below")).expect_err("not a directory");
        assert!(
            format!("{error:#}").contains("Not a directory"),
            "{error:#}"
        );
        // A link is followed; one to a file that moved is refused.
        let (link, moved) = (dir.path().join("link.toml"), dir.path().join("moved.toml"));
        symlink(&moved, &link).expect("symlink");
        let error = format!("{:#}", read(&link).expect_err("a link to nothing"));
        assert_eq!(error, "a link to a missing file");
        fs::write(&moved, "[git]\npoll-seconds = 9\n").expect("write");
        assert_eq!(read(&link).expect("linked").poll, Duration::from_secs(9));
    }

    #[test]
    fn what_is_not_utf8_or_toml_is_refused_where_it_is() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, b"[git]\npoll-seconds = \xff\n").expect("write");
        let error = format!("{:#}", read(&path).expect_err("not UTF-8"));
        assert_eq!(error, "not UTF-8 at line 2, column 16");
        // An é cut after its first byte, on the line after a whole one.
        fs::write(&path, b"# \xc3\xa9\n\xc3").expect("write");
        let error = format!("{:#}", read(&path).expect_err("cut"));
        assert_eq!(error, "not UTF-8 at line 2, column 1");
        fs::write(&path, "[git]\npoll-seconds = 0\n").expect("write");
        let error = format!("{:#}", read(&path).expect_err("out of range"));
        assert!(error.contains("line 2, column 16"), "{error}");
        assert!(error.contains("from 1 to 60"), "{error}");
    }

    #[test]
    fn what_is_not_a_regular_file_is_refused_without_waiting() {
        let dir = TempDir::new().expect("temp dir");
        let error = format!("{:#}", read(dir.path()).expect_err("a directory"));
        assert_eq!(error, "not a regular file");
        // A pipe: opening it to read would wait for a writer.
        let fifo = dir.path().join("fifo");
        let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
        assert!(made.success());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(read(&fifo).map_err(|error| format!("{error:#}")));
        });
        match rx.recv_timeout(WAIT) {
            Ok(read) => assert_eq!(read, Err("not a regular file".to_owned())),
            Err(RecvTimeoutError::Timeout) => panic!("the read waits for a writer"),
            Err(RecvTimeoutError::Disconnected) => panic!("the read panicked"),
        }
    }

    #[test]
    fn a_place_counts_lines_and_characters_from_one() {
        assert_eq!(place(""), (1, 1));
        assert_eq!(place("ab"), (1, 3));
        assert_eq!(place("ab\n"), (2, 1));
        assert_eq!(place("a\n\u{e9}t\u{e9} "), (2, 5));
        assert_eq!(place("\n\n"), (3, 1));
    }
}
