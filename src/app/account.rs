//! The accounts the agent runs as: its own, and those the config file
//! declares, each with the directory of the agent's configuration that
//! holds its login and its data (ADR-0014).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::config::{Declared, OWN_ACCOUNT};
use crate::agent::Agent;
use crate::strings;

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

    /// The account named `name`, case aside. [`OWN_ACCOUNT`], which no
    /// account with a directory can be named, is the agent's own under
    /// whatever name the file gives it.
    pub(super) fn named(&self, name: &str) -> Option<&Account> {
        self.0
            .iter()
            .find(|account| account.name.eq_ignore_ascii_case(name))
            .or_else(|| name.eq_ignore_ascii_case(OWN_ACCOUNT).then(|| self.own()))
    }

    /// The account a session in a directory runs as: the one named
    /// `remembered`, which the directory last opened as, else the one
    /// named `last_ran`, which the agent last ran there as, else the
    /// agent's own. Fails, saying why, when `remembered` names no
    /// account any more: the session would run as another account than
    /// the user chose for it.
    pub(super) fn for_dir(
        &self,
        remembered: Option<&str>,
        last_ran: Option<&str>,
    ) -> Result<&Account, String> {
        match remembered {
            Some(name) => self.named(name).ok_or_else(|| strings::account_gone(name)),
            None => Ok(last_ran
                .and_then(|name| self.named(name))
                .unwrap_or_else(|| self.own())),
        }
    }

    /// The accounts, in order.
    pub(super) fn iter(&self) -> impl Iterator<Item = &Account> {
        self.0.iter()
    }
}

/// The name of the account `agent` last ran each directory as, among
/// `accounts`, as [`recent`] gives it. None with one account, which
/// leaves nothing to choose.
pub(super) fn last_ran(agent: &dyn Agent, accounts: &Accounts) -> HashMap<PathBuf, String> {
    if !accounts.several() {
        return HashMap::new();
    }
    recent(agent, accounts).into_iter().collect()
}

/// The directories `agent` ran in lately as any of `accounts`, the one
/// it started in last first, then those it gives no time for by path;
/// each once, with the name of the account it started there as last,
/// else of the first account that lists it.
pub(super) fn recent(agent: &dyn Agent, accounts: &Accounts) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, Option<i64>, &str)> = Vec::new();
    let mut at: HashMap<PathBuf, usize> = HashMap::new();
    for account in accounts.iter() {
        for recent in agent.recent_projects(account.dir.as_deref()) {
            match at.get(&recent.dir) {
                Some(&index) => {
                    let kept = &mut found[index];
                    if recent.started > kept.1 {
                        *kept = (recent.dir, recent.started, &account.name);
                    }
                }
                None => {
                    at.insert(recent.dir.clone(), found.len());
                    found.push((recent.dir, recent.started, &account.name));
                }
            }
        }
    }
    // No time sorts below every time, so last.
    found.sort_by(|(a_dir, a_started, _), (b_dir, b_started, _)| {
        b_started.cmp(a_started).then_with(|| a_dir.cmp(b_dir))
    });
    found
        .into_iter()
        .map(|(dir, _, name)| (dir, name.to_owned()))
        .collect()
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
    fn a_directory_runs_as_its_remembered_account_else_the_last_ran_else_the_own() {
        let declared = [declared("work", Some("/w")), declared("personal", None)];
        let accounts = new(&declared, None).expect("two");
        let name = |found: Result<&Account, String>| found.map(|account| account.name.clone());
        assert_eq!(name(accounts.for_dir(None, None)), Ok("personal".into()));
        assert_eq!(
            name(accounts.for_dir(None, Some("work"))),
            Ok("work".into())
        );
        assert_eq!(
            name(accounts.for_dir(None, Some("gone"))),
            Ok("personal".into()),
            "the agent ran there as an account no longer declared"
        );
        assert_eq!(
            name(accounts.for_dir(Some("Personal"), Some("work"))),
            Ok("personal".into()),
            "the remembered one wins, case aside"
        );
        assert_eq!(
            name(accounts.for_dir(Some("old"), Some("work"))),
            Err("opened last as account old, which the config file does not declare".into())
        );
        let named = |name: &str| accounts.named(name).map(|account| account.name.as_str());
        assert_eq!(named("WORK"), Some("work"));
        assert_eq!(named("default"), Some("personal"), "the own one, named");
        assert_eq!(named("old"), None);
        assert_eq!(
            name(accounts.for_dir(Some("default"), None)),
            Ok("personal".into())
        );
    }

    /// An agent that ran in the directories each account's directory
    /// lists, with when; it is never launched.
    struct Ran(Vec<(Option<&'static str>, &'static str, Option<i64>)>);

    impl Agent for Ran {
        fn name(&self) -> &str {
            "ran"
        }

        fn spec(
            &self,
            _: &crate::agent::Launch<'_>,
            _: crate::Size,
            _: Option<&crate::instance::Instance>,
        ) -> crate::pty::Spec {
            unreachable!("never launched")
        }

        fn recent_projects(&self, account: Option<&Path>) -> Vec<crate::agent::Recent> {
            self.0
                .iter()
                .filter(|(of, _, _)| of.map(Path::new) == account)
                .map(|&(_, dir, started)| crate::agent::Recent {
                    dir: dir.into(),
                    started,
                })
                .collect()
        }
    }

    #[test]
    fn the_last_ran_account_is_the_one_that_started_there_last() {
        let ran = Ran(vec![
            (None, "/p/api", Some(1)),
            (Some("/w"), "/p/api", Some(2)),
            (None, "/p/notes", Some(5)),
            (Some("/w"), "/p/notes", None),
            (Some("/w"), "/p/both", None),
            (None, "/p/both", None),
        ]);
        let accounts = new(&[declared("work", Some("/w"))], None).expect("two");
        let found = last_ran(&ran, &accounts);
        let of = |dir: &str| found.get(Path::new(dir)).map(String::as_str);
        assert_eq!(of("/p/api"), Some("work"));
        assert_eq!(of("/p/notes"), Some("default"));
        assert_eq!(of("/p/both"), Some("default"), "no time: the first account");
        assert_eq!(of("/p/none"), None);
        assert!(
            last_ran(&ran, &Accounts::default()).is_empty(),
            "one account"
        );
    }

    #[test]
    fn the_recent_projects_of_every_account_are_merged_the_latest_first() {
        let ran = Ran(vec![
            (None, "/p/a", Some(1)),
            (Some("/w"), "/p/b", Some(4)),
            (None, "/p/b", Some(2)),
            (Some("/w"), "/p/c", None),
            (None, "/p/d", Some(3)),
        ]);
        let accounts = new(&[declared("work", Some("/w"))], None).expect("two");
        let found = recent(&ran, &accounts);
        let found: Vec<(&str, &str)> = found
            .iter()
            .map(|(dir, name)| (dir.to_str().expect("utf-8"), name.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("/p/b", "work"),
                ("/p/d", "default"),
                ("/p/a", "default"),
                ("/p/c", "work"),
            ]
        );
        // With one account, its own list as the agent gives it.
        let own = recent(&ran, &Accounts::default());
        let dirs: Vec<&Path> = own.iter().map(|(dir, _)| dir.as_path()).collect();
        assert_eq!(
            dirs,
            [Path::new("/p/d"), Path::new("/p/b"), Path::new("/p/a")]
        );
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
