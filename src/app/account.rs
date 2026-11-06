//! The accounts the agent runs as: its own, and those the config file
//! declares, each with the directory of the agent's configuration that
//! holds its login and its data (ADR-0014).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::config::{Declared, OWN_ACCOUNT};

/// An account the agent runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Account {
    /// Its name, as the picker and the agent's section show it.
    pub(super) name: String,
    /// The directory of the agent's configuration, with `~/` made the
    /// home and the rest as written; none for the agent's own, which
    /// runs with the environment as `termis` has it.
    pub(super) dir: Option<PathBuf>,
}

impl Default for Account {
    /// The agent's own, as [`OWN_ACCOUNT`].
    fn default() -> Self {
        Self {
            name: OWN_ACCOUNT.to_owned(),
            dir: None,
        }
    }
}

/// The accounts, in the config file's order, the agent's own among them:
/// where the file names it, else first, as [`OWN_ACCOUNT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Accounts(Vec<Account>);

impl Default for Accounts {
    /// The agent's own alone, as without a config file.
    fn default() -> Self {
        Self(vec![Account::default()])
    }
}

impl Accounts {
    /// The accounts `declared` in the config file, with `~/` made `home`,
    /// the value of `HOME`, and the agent's own. Fails when a directory
    /// starts with `~/` and `home` is not an absolute path, when two
    /// accounts name one directory, as paths compare or once links are
    /// followed, or when one names a directory of the agent's own
    /// account, among `own`, which the agent named `agent` gives.
    pub(super) fn new(
        declared: &[Declared],
        home: Option<&OsStr>,
        own: &[PathBuf],
        agent: &str,
    ) -> Result<Self> {
        let home = home.map(Path::new).filter(|home| home.is_absolute());
        let mut accounts: Vec<Account> = Vec::with_capacity(declared.len() + 1);
        if declared.iter().all(|account| account.dir.is_some()) {
            accounts.push(Account::default());
        }
        for account in declared {
            let name = &account.name;
            let dir = match account.dir.as_deref() {
                None => None,
                Some(written) => {
                    let Some(dir) = expand(written, home) else {
                        bail!("{name}: {written} starts with ~/ and HOME is not an absolute path");
                    };
                    if own.iter().any(|own| same_dir(own, &dir)) {
                        bail!(
                            "{name}: {} holds {agent}'s own account; leave dir out for it",
                            dir.display()
                        );
                    }
                    let clashes = |other: &&Account| {
                        other
                            .dir
                            .as_deref()
                            .is_some_and(|other| same_dir(other, &dir))
                    };
                    if let Some(other) = accounts.iter().find(clashes) {
                        bail!("{} and {name} name the same dir", other.name);
                    }
                    Some(dir)
                }
            };
            accounts.push(Account {
                name: name.clone(),
                dir,
            });
        }
        Ok(Self(accounts))
    }

    /// Whether there is more than the agent's own: only then does
    /// anything show or ask for an account.
    pub(super) fn several(&self) -> bool {
        self.0.len() > 1
    }

    /// The agent's own account.
    pub(super) fn own(&self) -> &Account {
        self.0
            .iter()
            .find(|account| account.dir.is_none())
            .unwrap_or(&self.0[0])
    }
}

/// `written` with a leading `~/` made `home`, the rest kept as written;
/// none when it has one and there is no home.
fn expand(written: &str, home: Option<&Path>) -> Option<PathBuf> {
    match written.strip_prefix("~/") {
        Some(rest) => home.map(|home| home.join(rest)),
        None => Some(PathBuf::from(written)),
    }
}

/// Whether `a` and `b` name one directory: the same path, as paths
/// compare, so a trailing `/` aside, or the same directory once links
/// are followed.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn declared(name: &str, dir: Option<&str>) -> Declared {
        Declared {
            name: name.to_owned(),
            dir: dir.map(str::to_owned),
        }
    }

    fn account(name: &str, dir: Option<&str>) -> Account {
        Account {
            name: name.to_owned(),
            dir: dir.map(PathBuf::from),
        }
    }

    /// The accounts `declared`, with `home` and a sample of the agent's
    /// own directories.
    fn new(declared: &[Declared], home: Option<&str>) -> Result<Accounts> {
        let own = [PathBuf::from("/home/me/.claude"), PathBuf::from("/home/me")];
        Accounts::new(declared, home.map(OsStr::new), &own, "claude")
    }

    #[test]
    fn the_agents_own_account_is_first_unless_the_file_names_it() {
        let none = new(&[], Some("/home/me")).expect("none");
        assert_eq!(none, Accounts::default());
        assert!(!none.several());
        assert_eq!(none.own(), &account("default", None));
        let work = new(&[declared("work", Some("/w"))], Some("/home/me")).expect("one");
        assert_eq!(
            work.0,
            [account("default", None), account("work", Some("/w"))]
        );
        assert!(work.several());
        assert_eq!(work.own().name, "default");
        let named = [declared("work", Some("/w")), declared("personal", None)];
        let named = new(&named, Some("/home/me")).expect("named");
        assert_eq!(
            named.0,
            [account("work", Some("/w")), account("personal", None)]
        );
        assert_eq!(named.own().name, "personal");
        // Its own named alone is one account: nothing shows.
        let alone = new(&[declared("me", None)], None).expect("alone");
        assert!(!alone.several());
        assert_eq!(alone.own().name, "me");
    }

    #[test]
    fn a_dir_under_the_home_is_expanded_and_kept_as_written() {
        let accounts = [
            declared("w", Some("~/.claude-work/")),
            declared("v", Some("/v//x")),
        ];
        let accounts = new(&accounts, Some("/home/me")).expect("expands");
        let dirs: Vec<Option<&OsStr>> = accounts
            .0
            .iter()
            .map(|account| account.dir.as_deref().map(Path::as_os_str))
            .collect();
        assert_eq!(
            dirs,
            [
                None,
                Some(OsStr::new("/home/me/.claude-work/")),
                Some(OsStr::new("/v//x")),
            ]
        );
        for home in [None, Some("home/me"), Some("")] {
            let error = new(&[declared("w", Some("~/x"))], home).expect_err("no home");
            assert_eq!(
                format!("{error:#}"),
                "w: ~/x starts with ~/ and HOME is not an absolute path"
            );
        }
        assert!(new(&[declared("w", Some("/x"))], None).is_ok(), "no ~/");
    }

    #[test]
    fn the_agents_own_directory_is_refused() {
        for dir in [
            "/home/me/.claude",
            "~/.claude",
            "~/.claude/",
            "/home/me//.claude",
            "~/",
        ] {
            let error = new(&[declared("w", Some(dir))], Some("/home/me")).expect_err(dir);
            let error = format!("{error:#}");
            assert!(error.starts_with("w: /home/me/"), "{error}");
            assert!(
                error.ends_with(" holds claude's own account; leave dir out for it"),
                "{error}"
            );
        }
    }

    #[test]
    fn two_accounts_in_one_directory_are_refused() {
        let temporary = TempDir::new().expect("temp dir");
        let base = temporary.path().canonicalize().expect("exists");
        let real = base.join("real");
        fs::create_dir(&real).expect("mkdir");
        let link = base.join("link");
        symlink(&real, &link).expect("link");
        let home = base.to_str().expect("utf-8");
        let accounts = [
            declared("a", Some("~/real")),
            declared("b", Some("~/link/")),
        ];
        let error = new(&accounts, Some(home)).expect_err("one directory");
        assert_eq!(format!("{error:#}"), "a and b name the same dir");
        // On a volume that ignores case, by a name in another case too.
        if base.join("REAL").is_dir() {
            let cased = [declared("a", Some("~/real")), declared("b", Some("~/REAL"))];
            let error = new(&cased, Some(home)).expect_err("one directory");
            assert_eq!(format!("{error:#}"), "a and b name the same dir");
        }
        // Through a link to the agent's own, too.
        let error = Accounts::new(
            &[declared("b", Some("~/link"))],
            Some(OsStr::new(home)),
            std::slice::from_ref(&real),
            "claude",
        )
        .expect_err("its own");
        assert!(format!("{error:#}").ends_with("holds claude's own account; leave dir out for it"));
        // Two directories that are not there yet differ by their path.
        let apart = [declared("a", Some("~/one")), declared("b", Some("~/two"))];
        assert!(new(&apart, Some(home)).is_ok());
    }
}
