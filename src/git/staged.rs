//! What is staged, read for a commit subject to be written from it: the
//! subjects of the branch's last commits, and the summary and the patch
//! of the index against `HEAD`.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use nix::sys::signal::{Signal, killpg};

use super::group;

/// A diff of the index against `HEAD` as git prints it for a reader
/// other than a terminal: no colour, no external diff program, no
/// textconv filter.
const DIFF: [&str; 5] = [
    "diff",
    "--cached",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
];

/// The staged changes of a repository, with the subjects of its last
/// commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staged {
    /// The subjects of the branch's last commits, merges left out, the
    /// newest first; none before the first commit.
    pub subjects: Vec<String>,
    /// What `git diff --cached --stat` prints: a line for each of the
    /// first files, ` ...` when there are more, then the totals of them
    /// all. Empty when nothing is staged.
    pub stat: String,
    /// What `git diff --cached` prints, as far as the budget it was read
    /// with, cut at a line end.
    pub patch: String,
    /// Whether the patch went on past the budget.
    pub cut: bool,
}

/// Reads, in the repository at `root`, the subjects of its `count` last
/// commits and what is staged: the stat with a line for each of the
/// first `files` files, and the patch up to `budget` bytes. Git runs as
/// [`super::run`] runs it; the patch's git is stopped once the budget is
/// read, with whatever it started.
pub fn read(root: &Path, count: usize, files: usize, budget: usize) -> Result<Staged> {
    let subjects = subjects(root, count)?;
    let stat_count = format!("--stat-count={files}");
    let stat = super::run(root, DIFF.iter().chain(&["--stat", stat_count.as_str()]))
        .context("git diff --stat")?;
    let (patch, cut) = patch(root, budget).context("git diff")?;
    Ok(Staged {
        subjects,
        stat: String::from_utf8_lossy(&stat).into_owned(),
        patch,
        cut,
    })
}

/// The subjects of the `count` last commits of `HEAD` in the repository
/// at `root`, merges left out, the newest first; none before the first
/// commit.
fn subjects(root: &Path, count: usize) -> Result<Vec<String>> {
    // Quietly nothing, with exit code 1, before the first commit.
    let head = super::run_accepting(root, ["rev-parse", "--verify", "-q", "HEAD"], &[1])
        .context("git rev-parse")?;
    if head.is_empty() {
        return Ok(Vec::new());
    }
    let count = format!("--max-count={count}");
    let out = super::run(
        root,
        [
            "log",
            "--no-merges",
            "--no-show-signature",
            "--format=%s",
            count.as_str(),
            "HEAD",
            "--",
        ],
    )
    .context("git log")?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// What `git diff --cached` prints in the repository at `root`, up to
/// `budget` bytes, cut after the last whole line, and whether it went on.
/// Past the budget git is not read further: its process group is killed.
fn patch(root: &Path, budget: usize) -> Result<(String, bool)> {
    let mut child = super::git_in(root)
        .args(DIFF)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| super::not_run(root, error))?;
    let group = group::of(&child)?;
    let stderr = group::drain(child.stderr.take());
    let limit = u64::try_from(budget).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(limit)
            .read_to_end(&mut bytes)
            .context("read the patch")?;
    }
    if bytes.len() > budget {
        // Killed before it is reaped, so the group's id is still its own.
        let _ = killpg(group, Signal::SIGKILL);
        let _ = child.wait();
        bytes.truncate(budget);
        let end = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at + 1);
        bytes.truncate(end);
        return Ok((String::from_utf8_lossy(&bytes).into_owned(), true));
    }
    let status = child.wait().context("wait for git diff")?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&group::collect(&stderr, group)).into_owned();
        return Err(
            match stderr
                .lines()
                .map(super::clean)
                .find(|line| !line.is_empty())
            {
                Some(line) => anyhow!("{line}"),
                None => anyhow!("git diff exited with {status}"),
            },
        );
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{config, git, git_with, init};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn commit(dir: &Path, subject: &str) {
        git(dir, &["commit", "-q", "--allow-empty", "-m", subject]);
    }

    /// Runs `git` in `dir` at `seconds` past the epoch, so that the log's
    /// order does not hang on commits made in the same second.
    fn git_at(dir: &Path, seconds: u32, args: &[&str]) {
        let date = format!("@{seconds} +0000");
        let dates = [
            ("GIT_AUTHOR_DATE", date.as_str()),
            ("GIT_COMMITTER_DATE", &date),
        ];
        git_with(dir, args, &dates);
    }

    fn stage(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).expect("write");
        git(dir, &["add", name]);
    }

    #[test]
    fn reads_the_last_subjects_newest_first_without_merges() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        stage(dir.path(), "a.txt", "a\n");
        let first = read(dir.path(), 10, 100, 1024).expect("reads");
        assert!(first.subjects.is_empty(), "none before the first commit");
        assert!(first.stat.contains("a.txt"), "{}", first.stat);
        assert!(first.patch.contains("+a"), "{}", first.patch);

        let empty = |subject| ["commit", "-q", "--allow-empty", "-m", subject];
        git_at(dir.path(), 1_000, &empty("feat: one"));
        git_at(dir.path(), 2_000, &empty("fix: two\n\nwith a body"));
        git(dir.path(), &["checkout", "-q", "-b", "topic"]);
        git_at(dir.path(), 3_000, &empty("feat: on topic"));
        git(dir.path(), &["checkout", "-q", "main"]);
        git_at(dir.path(), 4_000, &empty("docs: three"));
        let merge = ["merge", "-q", "--no-ff", "-m", "Merge topic", "topic"];
        git_at(dir.path(), 5_000, &merge);
        let staged = read(dir.path(), 10, 100, 1024).expect("reads");
        assert_eq!(
            staged.subjects,
            ["docs: three", "feat: on topic", "fix: two", "feat: one"]
        );
        assert_eq!(
            read(dir.path(), 2, 100, 1024).expect("reads").subjects,
            ["docs: three", "feat: on topic"]
        );
    }

    #[test]
    fn the_stat_and_the_patch_are_of_the_index_alone() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        stage(dir.path(), "a.txt", "a\n");
        commit(dir.path(), "first");
        stage(dir.path(), "a.txt", "a\nb\n");
        std::fs::write(dir.path().join("a.txt"), "a\nb\nnot staged\n").expect("write");
        std::fs::write(dir.path().join("loose.txt"), "untracked\n").expect("write");
        let staged = read(dir.path(), 10, 100, 1024).expect("reads");
        assert!(staged.stat.contains("a.txt | 1 +"), "{}", staged.stat);
        assert!(staged.stat.contains("1 file changed"), "{}", staged.stat);
        assert!(!staged.stat.contains("loose.txt"), "{}", staged.stat);
        assert!(staged.patch.contains("\n+b\n"), "{}", staged.patch);
        assert!(!staged.patch.contains("not staged"), "{}", staged.patch);
        assert!(!staged.cut);

        git(dir.path(), &["reset", "-q"]);
        let nothing = read(dir.path(), 10, 100, 1024).expect("reads");
        assert_eq!((nothing.stat.as_str(), nothing.patch.as_str()), ("", ""));
    }

    #[test]
    fn the_stat_lists_the_first_files_and_the_totals_of_them_all() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        for n in 0..5 {
            stage(dir.path(), &format!("f{n}.txt"), "a\n");
        }
        let staged = read(dir.path(), 10, 2, 4096).expect("reads");
        let lines: Vec<&str> = staged.stat.lines().collect();
        assert_eq!(lines.len(), 4, "{}", staged.stat);
        assert!(lines[0].starts_with(" f0.txt |"), "{}", staged.stat);
        assert!(lines[1].starts_with(" f1.txt |"), "{}", staged.stat);
        assert_eq!(lines[2], " ...");
        assert!(
            lines[3].contains("5 files changed, 5 insertions(+)"),
            "{}",
            staged.stat
        );
        assert!(staged.patch.contains("f4.txt"), "the patch is not cut");
    }

    #[test]
    fn the_patch_is_cut_at_a_line_end_within_the_budget() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let long: String = (0..2000).map(|n| format!("line {n}\n")).collect();
        stage(dir.path(), "long.txt", &long);
        let whole = read(dir.path(), 10, 100, usize::MAX / 2).expect("reads");
        assert!(!whole.cut);
        assert!(whole.patch.len() > 4096);

        let staged = read(dir.path(), 10, 100, 1000).expect("reads");
        assert!(staged.cut);
        assert!(staged.patch.len() <= 1000, "{}", staged.patch.len());
        assert!(staged.patch.ends_with('\n'));
        assert!(whole.patch.starts_with(&staged.patch));
        assert!(
            staged.patch.len() > 1000 - "+line 1999\n".len(),
            "as much as fits"
        );
        assert!(staged.stat.contains("long.txt"), "the stat stays whole");
    }

    #[test]
    fn no_colour_external_diff_or_textconv_reaches_the_reader() {
        let dir = TempDir::new().expect("temp dir");
        init(dir.path());
        let marker = dir.path().join("ran");
        let program = dir.path().join("program.sh");
        let script = format!(
            "#!/bin/sh\necho ran >> '{}'\ncat \"$1\"\n",
            marker.display()
        );
        std::fs::write(&program, script).expect("write");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let program = program.to_str().expect("utf-8");
        config(dir.path(), "color.ui", "always");
        config(dir.path(), "color.diff", "always");
        config(dir.path(), "diff.external", program);
        config(dir.path(), "diff.shown.textconv", program);
        std::fs::write(dir.path().join(".gitattributes"), "*.txt diff=shown\n").expect("write");
        stage(dir.path(), "a.txt", "a\n");
        let staged = read(dir.path(), 10, 100, 4096).expect("reads");
        assert!(staged.patch.contains("\n+a\n"), "{}", staged.patch);
        assert!(!staged.patch.contains('\x1b'), "{:?}", staged.patch);
        assert!(!staged.stat.contains('\x1b'), "{:?}", staged.stat);
        assert!(!marker.exists(), "neither program ran");
    }

    #[test]
    fn outside_a_repository_git_says_why() {
        let dir = TempDir::new().expect("temp dir");
        let error = read(dir.path(), 10, 100, 1024).expect_err("not a repository");
        assert!(format!("{error:#}").contains("git"), "{error:#}");
    }
}
