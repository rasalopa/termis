//! A commit subject written by the agent: what is staged goes in on its
//! stdin, one line comes back.

use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::unistd::Pid;
use unicode_segmentation::UnicodeSegmentation;

use super::Ask;
use crate::git::group::{self, Waited};
use crate::git::staged::{self, Staged};

/// How long the agent gets to answer, after which its process group is
/// killed.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// How many of the branch's last subjects show the agent the
/// repository's style.
const SUBJECTS: usize = 10;

/// How many staged files the stat lists; its totals count them all.
const STAT_FILES: usize = 100;

/// The most bytes of the staged patch the agent reads.
const PATCH_BUDGET: usize = 64 * 1024;

/// The most graphemes an answer keeps.
const LONGEST: usize = 100;

/// What the agent is asked, before what is staged.
const INSTRUCTION: &str = "Write the subject line of a commit message for the staged changes \
below. When recent subjects are given, write it as they are written: in their language and \
their form, so when they start with a type and a colon, such as \"feat: \" or \"fix(ui): \", \
start it with the type that fits these changes. Answer with that line alone: in the \
imperative mood, at most 72 characters, with no quotes, no trailing period and nothing else.";

/// Pairs of quotes an answer may come wrapped in.
const QUOTES: [(char, char); 5] = [
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
    ('\u{201c}', '\u{201d}'),
    ('\u{2018}', '\u{2019}'),
];

/// A suggestion running on a thread of its own. Stopping it, or
/// dropping it, kills the agent's process group if the agent runs, and
/// the thread then answers and ends without waiting for the timeout.
#[must_use = "dropping it stops the run"]
pub struct Running {
    /// Set once the run is to stop.
    stop: Arc<AtomicBool>,
    /// The thread that runs the agent and answers.
    thread: JoinHandle<()>,
}

impl Running {
    /// Has the run kill the agent with its group, or not start it, and
    /// end.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Whether the thread ended, its answer sent.
    pub fn ended(&self) -> bool {
        self.thread.is_finished()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Asks the agent through `ask` for the subject of a commit of what is
/// staged in the repository at `root`, and returns it.
///
/// The agent runs in `root`, in a process group of its own, with the
/// prompt on its stdin, which then closes, and gets `timeout`, after
/// which the group is killed, as it is once `stop` is set. The prompt
/// says what is wanted, then gives the branch's last subjects,
/// `git diff --cached --stat`, listing [`STAT_FILES`] files at most, and
/// the patch, cut past [`PATCH_BUDGET`]. The subject is [`answer`]'s
/// line. Fails when nothing is staged, when git fails, when the agent
/// cannot start, is stopped, exits with a status other than 0, saying
/// the first line it printed on stderr, else on stdout, or gives no
/// subject.
pub fn suggest(ask: &Ask, root: &Path, timeout: Duration, stop: &AtomicBool) -> Result<String> {
    let staged = staged::read(root, SUBJECTS, STAT_FILES, PATCH_BUDGET)?;
    if staged.stat.trim().is_empty() {
        bail!("nothing staged");
    }
    run(ask, root, &prompt(&staged), timeout, stop)
}

/// Suggests as [`suggest`] does, with [`TIMEOUT`], on a thread of its
/// own, which answers once on `tx`, wrapped by `tag` into the owner's
/// message type, and ends. Returns the run, which stops it.
pub fn spawn<E>(
    ask: Ask,
    root: PathBuf,
    tx: Sender<E>,
    tag: impl FnOnce(Result<String>) -> E + Send + 'static,
) -> Result<Running>
where
    E: Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("agent-subject".into())
        .spawn(move || {
            let result = suggest(&ask, &root, TIMEOUT, &stopped);
            let _ = tx.send(tag(result));
        })
        .context("spawn the suggestion")?;
    Ok(Running { stop, thread })
}

/// The prompt for what is `staged`: the instruction, the recent subjects
/// when there are some, the stat and the patch, with a line saying where
/// it was cut.
fn prompt(staged: &Staged) -> String {
    let mut prompt = format!("{INSTRUCTION}\n");
    if !staged.subjects.is_empty() {
        prompt.push_str("\nThe recent subjects, newest first:\n");
        for subject in &staged.subjects {
            prompt.push_str(subject);
            prompt.push('\n');
        }
    }
    prompt.push_str("\nThe staged files (git diff --cached --stat):\n");
    prompt.push_str(&staged.stat);
    prompt.push_str("\nThe staged changes (git diff --cached):\n");
    prompt.push_str(&staged.patch);
    if staged.cut {
        let budget = PATCH_BUDGET / 1024;
        prompt.push_str(&format!(
            "[The rest of the patch, past {budget} KiB, is left out.]\n"
        ));
    }
    prompt
}

/// Runs `ask` in `root` with `input` on its stdin until `timeout`, or
/// until `stop` is set, and returns the subject it printed.
fn run(
    ask: &Ask,
    root: &Path,
    input: &str,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<String> {
    let name = group::name(Path::new(&ask.program));
    if stop.load(Ordering::SeqCst) {
        bail!("{name} was stopped");
    }
    let mut child = command(ask, root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| not_run(&name, root, error))?;
    let group = group::of(&child)?;
    let deadline = Instant::now() + timeout;
    let stdout = group::drain(child.stdout.take());
    let stderr = group::drain(child.stderr.take());
    group::feed(child.stdin.take(), input.as_bytes().to_vec());
    match group::wait_or_stop(&mut child, deadline, stop, group, &name)? {
        Waited::Exited(status) if status.success() => {
            let printed = String::from_utf8_lossy(&group::collect(&stdout, group)).into_owned();
            answer(&printed).ok_or_else(|| anyhow!("{name} gave no subject"))
        }
        Waited::Exited(status) => Err(failure(&name, status, &stdout, &stderr, group)),
        Waited::Killed if stop.load(Ordering::SeqCst) => bail!("{name} was stopped"),
        Waited::Killed => bail!("{name} took longer than {timeout:?} and was killed"),
    }
}

/// The command for `ask` in `root`.
fn command(ask: &Ask, root: &Path) -> Command {
    let mut command = Command::new(&ask.program);
    command.args(&ask.args).current_dir(root);
    for variable in &ask.env_remove {
        command.env_remove(variable);
    }
    command
}

/// Why the agent `name` could not be started in `root`: that it is not
/// installed when `root` exists and the program was not found, else
/// `error` with `root` named.
fn not_run(name: &str, root: &Path, error: std::io::Error) -> anyhow::Error {
    match error.kind() {
        ErrorKind::NotFound if root.is_dir() => anyhow!("{name} is not installed"),
        _ => anyhow!(error).context(format!("run {name} in {}", root.display())),
    }
}

/// The error for the agent `name` that exited with `status`: the first
/// line it printed on stderr, else on stdout, else the status.
fn failure(
    name: &str,
    status: ExitStatus,
    stdout: &Receiver<Vec<u8>>,
    stderr: &Receiver<Vec<u8>>,
    group: Pid,
) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&group::collect(stderr, group)).into_owned();
    let stdout = String::from_utf8_lossy(&group::collect(stdout, group)).into_owned();
    match group::first_line(&stderr).or_else(|| group::first_line(&stdout)) {
        Some(line) => anyhow!("{line}"),
        None => anyhow!("{name} exited with {status}"),
    }
}

/// The subject in what the agent printed: its first line with something
/// on it, past the fences of a code block, with escape sequences and
/// other control characters removed, the quotes or backticks around it
/// taken off, trimmed and cut to [`LONGEST`] graphemes. None when there
/// is no such line.
fn answer(printed: &str) -> Option<String> {
    let line = printed
        .lines()
        .map(crate::git::clean)
        .find(|line| !line.is_empty() && !line.starts_with("```"))?;
    let subject: String = unquoted(&line).graphemes(true).take(LONGEST).collect();
    let subject = subject.trim_end();
    (!subject.is_empty()).then(|| subject.to_owned())
}

/// `line` without the pairs of quotes or backticks around it, trimmed.
fn unquoted(line: &str) -> &str {
    let mut line = line.trim();
    while let Some((open, close)) = QUOTES.iter().find(|(open, close)| {
        line.len() >= open.len_utf8() + close.len_utf8()
            && line.starts_with(*open)
            && line.ends_with(*close)
    }) {
        line = line[open.len_utf8()..line.len() - close.len_utf8()].trim();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::agent::claude::Claude;
    use crate::git::tests::{git, git_with, init};
    use nix::errno::Errno;
    use nix::sys::signal::killpg;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use tempfile::TempDir;

    /// A timeout meant to fire while a fake agent sleeps.
    const QUICK: Duration = Duration::from_millis(500);
    /// A timeout meant never to fire: only a hang comes near it.
    const AMPLE: Duration = Duration::from_secs(60);
    /// How soon a run returns when it must not wait for what a fake
    /// agent runs for 30 s, with room for a loaded machine.
    const SOON: Duration = Duration::from_secs(10);
    /// A stop never set.
    static GO: AtomicBool = AtomicBool::new(false);

    /// A fake agent named `claude` in `dir/bin` that runs `script` under
    /// `sh`, with Claude's arguments.
    fn fake(dir: &Path, script: &str) -> Ask {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).expect("mkdir");
        let program = bin.join("claude");
        fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("write");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod");
        Ask {
            program: program.to_string_lossy().into_owned(),
            ..Claude.subject().expect("Claude writes subjects")
        }
    }

    /// A repository in `dir/repo` with two commits and a file staged.
    fn repo(dir: &Path) -> PathBuf {
        let root = dir.join("repo");
        fs::create_dir(&root).expect("mkdir");
        init(&root);
        for (seconds, subject) in [
            (1_000, "feat: add the parser"),
            (2_000, "fix: read empty input"),
        ] {
            let date = format!("@{seconds} +0000");
            let dates = [
                ("GIT_AUTHOR_DATE", date.as_str()),
                ("GIT_COMMITTER_DATE", &date),
            ];
            git_with(
                &root,
                &["commit", "-q", "--allow-empty", "-m", subject],
                &dates,
            );
        }
        fs::write(root.join("a.txt"), "a\n").expect("write");
        git(&root, &["add", "a.txt"]);
        root
    }

    #[test]
    fn the_prompt_says_what_is_wanted_then_gives_what_is_staged() {
        let mut staged = Staged {
            subjects: vec!["fix: two".into(), "feat: one".into()],
            stat: " a.txt | 1 +\n 1 file changed, 1 insertion(+)\n".into(),
            patch: "diff --git a/a.txt b/a.txt\n+a\n".into(),
            cut: false,
        };
        let text = prompt(&staged);
        assert!(text.starts_with(INSTRUCTION), "{text}");
        let at = |part: &str| {
            text.find(part)
                .unwrap_or_else(|| panic!("{part} in {text}"))
        };
        assert!(at("fix: two\nfeat: one\n") < at(" a.txt | 1 +"));
        assert!(at(" 1 file changed") < at("diff --git"));
        assert!(text.ends_with("+a\n"), "{text}");
        assert!(INSTRUCTION.contains("72 characters"));

        staged.cut = true;
        assert!(
            prompt(&staged).ends_with("+a\n[The rest of the patch, past 64 KiB, is left out.]\n")
        );
        staged.subjects.clear();
        assert!(
            !prompt(&staged).contains("recent subjects, newest"),
            "none yet"
        );
    }

    #[test]
    fn the_answer_is_its_first_line_cleaned() {
        let cases = [
            ("feat: add x\n", "feat: add x"),
            ("\n  \n  feat: add x  \nmore\n", "feat: add x"),
            ("\"feat: add x\"", "feat: add x"),
            ("'feat: add x'", "feat: add x"),
            ("`feat: add x`", "feat: add x"),
            ("\u{201c}feat: add x\u{201d}", "feat: add x"),
            ("\" `feat: add x` \"", "feat: add x"),
            ("```\nfeat: add x\n```\n", "feat: add x"),
            ("```text\nfeat: add x\n```\n", "feat: add x"),
            ("\x1b[1mfeat\x1b[0m: add\x07 x\r\n", "feat: add x"),
            ("fix: handle \"a\" and 'b'", "fix: handle \"a\" and 'b'"),
            ("'", "'"),
        ];
        for (printed, subject) in cases {
            assert_eq!(answer(printed).as_deref(), Some(subject), "{printed:?}");
        }
        for nothing in ["", "\n \n\t\n", "\"\"", "``", "```\n```\n", "\x1b[0m\n"] {
            assert_eq!(answer(nothing), None, "{nothing:?}");
        }
        // Cut to whole graphemes.
        let long = "e\u{301}".repeat(LONGEST + 20);
        let cut = answer(&long).expect("a subject");
        assert_eq!(cut.graphemes(true).count(), LONGEST);
        assert!(long.starts_with(&cut));
    }

    #[test]
    fn the_agent_runs_in_the_root_with_the_prompt_on_its_stdin_and_no_terminal() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        let out = dir.path().join("out");
        fs::create_dir(&out).expect("mkdir");
        let script = format!(
            "pwd -P > '{0}/cwd'\n\
             printf '%s\\n' \"$@\" > '{0}/argv'\n\
             if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty; else echo pipes; fi > '{0}/tty'\n\
             cat > '{0}/stdin'\n\
             ps -o pgid= -p $$ | tr -d ' ' > '{0}/pgid'\n\
             echo $$ > '{0}/pid'\n\
             printf '\\n  \"feat: add a\"\\nmore\\n'",
            out.display()
        );
        let ask = fake(dir.path(), &script);
        let subject = suggest(&ask, &root, AMPLE, &GO).expect("a subject");
        assert_eq!(subject, "feat: add a");
        let read = |name: &str| fs::read_to_string(out.join(name)).expect(name);
        let argv: Vec<String> = read("argv").lines().map(str::to_owned).collect();
        assert_eq!(
            argv, ask.args,
            "the arguments, the empty tool list included"
        );
        assert_eq!(
            read("cwd").trim(),
            root.canonicalize().expect("real").to_str().expect("utf-8")
        );
        let staged = staged::read(&root, SUBJECTS, STAT_FILES, PATCH_BUDGET).expect("reads");
        assert_eq!(read("stdin"), prompt(&staged));
        assert!(read("stdin").contains("fix: read empty input\nfeat: add the parser\n"));
        assert!(read("stdin").contains("+a\n"));
        assert_eq!(read("tty").trim(), "pipes", "no terminal");
        assert_eq!(read("pgid"), read("pid"), "a process group of its own");
    }

    #[test]
    fn the_command_withholds_what_would_make_claude_a_child_session() {
        let ask = Claude.subject().expect("Claude writes subjects");
        let command = command(&ask, Path::new("/tmp/repo"));
        assert_eq!(command.get_program(), "claude");
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp/repo")));
        for variable in &ask.env_remove {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == variable.as_str() && value.is_none()),
                "{variable} is withheld"
            );
        }
        assert!(
            command.get_envs().all(|(_, value)| value.is_none()),
            "nothing set, the rest inherited"
        );
    }

    #[test]
    fn an_empty_answer_or_a_failure_says_why() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        let error = |script: &str| {
            let ask = fake(dir.path(), &format!("cat > /dev/null\n{script}"));
            format!("{:#}", suggest(&ask, &root, AMPLE, &GO).expect_err(script))
        };
        assert_eq!(error("printf '\\n  \\n'"), "claude gave no subject");
        assert_eq!(
            error("echo 'Not logged in · Please run /login' >&2\necho 'on stdout'\nexit 1"),
            "Not logged in · Please run /login"
        );
        assert_eq!(
            error("echo 'Invalid API key'\nexit 1"),
            "Invalid API key",
            "stdout when stderr is empty"
        );
        assert_eq!(error("exit 3"), "claude exited with exit status: 3");
    }

    #[test]
    fn a_missing_agent_is_not_installed() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        let ask = Ask {
            program: dir
                .path()
                .join("nowhere/claude")
                .to_string_lossy()
                .into_owned(),
            ..Claude.subject().expect("Claude writes subjects")
        };
        let error = suggest(&ask, &root, AMPLE, &GO).expect_err("not there");
        assert_eq!(format!("{error:#}"), "claude is not installed");
    }

    #[test]
    fn nothing_staged_asks_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        git(&root, &["reset", "-q"]);
        let ran = dir.path().join("ran");
        let ask = fake(dir.path(), &format!("touch '{}'", ran.display()));
        let error = suggest(&ask, &root, AMPLE, &GO).expect_err("nothing staged");
        assert_eq!(format!("{error:#}"), "nothing staged");
        assert!(!ran.exists(), "the agent did not run");
    }

    /// A fake agent that writes its pid in `out/pid`, starts a 30 s sleep
    /// in its group, writes the sleep's pid in `out/child` and waits; each
    /// file shows up whole.
    fn hanging(dir: &Path, out: &Path) -> Ask {
        fs::create_dir_all(out).expect("mkdir");
        let script = format!(
            "echo $$ > '{0}/pid.tmp'\nmv '{0}/pid.tmp' '{0}/pid'\n\
             sleep 30 &\necho $! > '{0}/child.tmp'\nmv '{0}/child.tmp' '{0}/child'\nwait",
            out.display()
        );
        fake(dir, &script)
    }

    /// Asserts that the group of the fake agent of [`hanging`] that wrote
    /// in `out` is gone, the sleep in it with it.
    fn assert_gone(out: &Path) {
        let pid = |name: &str| -> i32 {
            fs::read_to_string(out.join(name))
                .expect(name)
                .trim()
                .parse()
                .expect("a pid")
        };
        let group = Pid::from_raw(pid("pid"));
        let child = Pid::from_raw(pid("child"));
        // The sleep, reparented once its shell died, is reaped by init.
        let deadline = Instant::now() + SOON;
        while killpg(group, None) != Err(Errno::ESRCH) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(killpg(group, None), Err(Errno::ESRCH), "the group is gone");
        assert_eq!(
            nix::sys::signal::kill(child, None),
            Err(Errno::ESRCH),
            "and the sleep in it"
        );
    }

    #[test]
    fn a_hanging_agent_is_killed_with_its_group_after_the_timeout() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        // The timeout runs from the spawn, and a loaded machine can kill
        // the fake agent before it starts its sleep: such a run is tried
        // again with twice the timeout.
        let mut timeout = QUICK;
        loop {
            let out = dir.path().join(format!("out-{}", timeout.as_millis()));
            let ask = hanging(dir.path(), &out);
            let started = Instant::now();
            let error = suggest(&ask, &root, timeout, &GO).expect_err("killed");
            assert!(started.elapsed() < timeout + SOON, "did not hang");
            assert_eq!(
                format!("{error:#}"),
                format!("claude took longer than {timeout:?} and was killed")
            );
            if out.join("child").exists() {
                assert_gone(&out);
                return;
            }
            timeout *= 2;
            assert!(timeout < SOON, "the fake agent never started its sleep");
        }
    }

    #[test]
    fn a_suggestion_answers_from_a_thread() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        let ask = fake(dir.path(), "cat > /dev/null\necho 'docs: add a'");
        let (tx, rx) = mpsc::channel();
        let running = spawn(ask, root, tx, |result| ("tagged", result)).expect("spawns");
        let (tag, result) = rx.recv_timeout(AMPLE).expect("an answer");
        assert_eq!(tag, "tagged");
        assert_eq!(result.expect("a subject"), "docs: add a");
        let deadline = Instant::now() + SOON;
        while !running.ended() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(running.ended(), "the thread ends once it answered");
    }

    #[test]
    fn a_stopped_or_dropped_suggestion_kills_the_agent_with_its_group_at_once() {
        let dir = TempDir::new().expect("temp dir");
        let root = repo(dir.path());
        for drop_it in [false, true] {
            let out = dir.path().join(format!("out-{drop_it}"));
            let ask = hanging(dir.path(), &out);
            let (tx, rx) = mpsc::channel();
            let running = spawn(ask, root.clone(), tx, |result| result).expect("spawns");
            let deadline = Instant::now() + SOON;
            while !out.join("child").exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(out.join("child").exists(), "the agent runs");
            let started = Instant::now();
            if drop_it {
                drop(running);
            } else {
                running.stop();
            }
            let error = rx
                .recv_timeout(SOON)
                .expect("an answer")
                .expect_err("stopped");
            assert!(started.elapsed() < SOON, "not after the timeout");
            assert_eq!(format!("{error:#}"), "claude was stopped");
            assert_gone(&out);
        }
        // Stopped before it starts, the agent does not run.
        let ran = dir.path().join("ran");
        let ask = fake(dir.path(), &format!("touch '{}'", ran.display()));
        let error = suggest(&ask, &root, AMPLE, &AtomicBool::new(true)).expect_err("stopped");
        assert_eq!(format!("{error:#}"), "claude was stopped");
        assert!(!ran.exists(), "the agent did not run");
    }
}
