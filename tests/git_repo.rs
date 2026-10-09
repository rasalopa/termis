//! Stage, unstage and commit in a temporary repository through the
//! library's public API. The setup runs git without the developer's
//! configuration; what the library runs reads it as always, so the
//! repository pins locally every setting that could change the outcome.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use termis::git::status::{self, Change};
use termis::git::{commit, stage};

/// Runs `git` in `dir` with the developer's configuration and git
/// environment kept out and a fixed identity, for the setup the test
/// needs.
fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} in {}", dir.display());
    output.stdout
}

/// A repository on `main` whose local configuration pins what the
/// library would otherwise read from the developer's settings.
fn init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@example.com"),
        ("core.excludesFile", "/dev/null"),
        ("core.fsmonitor", "false"),
        ("core.hooksPath", "/dev/null"),
        ("commit.gpgsign", "false"),
        ("diff.renames", "true"),
        ("status.renames", "true"),
    ] {
        git(dir, &["config", key, value]);
    }
}

/// `(path, index, worktree)` for every entry, in git's order.
fn entries(root: &Path) -> Vec<(String, Option<Change>, Option<Change>)> {
    status::read(root)
        .expect("status")
        .entries
        .into_iter()
        .map(|entry| {
            (
                entry.path.to_string_lossy().into_owned(),
                entry.index,
                entry.worktree,
            )
        })
        .collect()
}

#[test]
fn stage_commit_and_read_back() {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path();
    init(root);
    std::fs::write(root.join("a.txt"), "a\n").expect("write");
    std::fs::write(root.join("b.txt"), "b\nb\n").expect("write");
    assert_eq!(
        entries(root),
        vec![
            ("a.txt".into(), None, Some(Change::Untracked)),
            ("b.txt".into(), None, Some(Change::Untracked)),
        ]
    );

    stage::stage(root, &["a.txt".into()]).expect("stages one");
    assert_eq!(
        entries(root),
        vec![
            ("a.txt".into(), Some(Change::Added), None),
            ("b.txt".into(), None, Some(Change::Untracked)),
        ]
    );
    stage::stage_all(root).expect("stages the rest");
    assert_eq!(
        entries(root),
        vec![
            ("a.txt".into(), Some(Change::Added), None),
            ("b.txt".into(), Some(Change::Added), None),
        ]
    );

    let message = commit::message("feat: two files", "one line of body");
    let hash = commit::commit(root, &message, Duration::from_secs(10)).expect("commits");
    let status = status::read(root).expect("status");
    assert!(status.entries.is_empty(), "nothing left after the commit");
    let oid = status.branch.oid.expect("a commit exists");
    assert!(oid.starts_with(&hash), "{hash} is the short form of {oid}");
    let log = git(root, &["log", "-1", "--format=%B"]);
    assert_eq!(String::from_utf8_lossy(&log).trim_end(), message.trim_end());

    std::fs::write(root.join("a.txt"), "a\nmore\n").expect("write");
    stage::stage(root, &["a.txt".into()]).expect("stages the edit");
    assert_eq!(
        entries(root),
        vec![("a.txt".into(), Some(Change::Modified), None)]
    );
    stage::unstage(root, &["a.txt".into()]).expect("takes it back");
    assert_eq!(
        entries(root),
        vec![("a.txt".into(), None, Some(Change::Modified))]
    );
}
