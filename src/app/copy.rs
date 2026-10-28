//! The text selected in the center pane copied to the clipboard by
//! `pbcopy`, off the main thread, which answers over the app's channel
//! (ADR-0002).

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use unicode_segmentation::UnicodeSegmentation;

use crate::git::group::{self, Waited};
use crate::strings;

/// The program that puts what it reads on the clipboard: macOS's, by its
/// absolute path.
pub(super) const PBCOPY: &str = "/usr/bin/pbcopy";

/// The locale the program runs in, whatever the user's: it reads its
/// input in the locale's encoding, and the text is UTF-8.
const LOCALE: (&str, &str) = ("LC_ALL", "en_US.UTF-8");

/// How long the program gets before its process group is killed.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long quitting waits, at most, for the copies told to stop to end.
const STOPPED_WITHIN: Duration = Duration::from_millis(200);

/// The copies to the clipboard, one at a time, the last one asked for
/// last: a double or a triple click copies at each release.
#[derive(Debug, Default)]
pub(super) struct Clipboard {
    /// The number of the last copy asked for.
    last: Arc<AtomicU64>,
    /// Held by the copy that runs.
    turn: Arc<Mutex<()>>,
    /// Set once `termis` quits: the copy that runs kills its program's
    /// group, and those waiting end without a copy.
    stop: Arc<AtomicBool>,
    /// The threads of the copies asked for, until they end.
    threads: Vec<JoinHandle<()>>,
}

impl Clipboard {
    /// Copies `text` with `program`, as [`copy`] does with [`TIMEOUT`], on
    /// a thread of its own, which answers once on `tx`, wrapped by `tag`
    /// into the owner's message type, with the notice saying what was
    /// copied, and ends. The thread waits for the copy that runs; once its
    /// turn comes, it ends without a copy or an answer when another copy
    /// was asked for since, or once stopped. Nothing runs for a `text`
    /// with nothing but spaces and line ends. Fails when the thread cannot
    /// start.
    pub(super) fn copy<E>(
        &mut self,
        program: &Path,
        text: String,
        tx: Sender<E>,
        tag: impl FnOnce(Result<String>) -> E + Send + 'static,
    ) -> Result<()>
    where
        E: Send + 'static,
    {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.threads.retain(|thread| !thread.is_finished());
        let number = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        let last = Arc::clone(&self.last);
        let turn = Arc::clone(&self.turn);
        let stop = Arc::clone(&self.stop);
        let program = program.to_path_buf();
        let thread = thread::Builder::new()
            .name("clipboard-writer".into())
            .spawn(move || {
                let _turn = turn.lock().unwrap_or_else(PoisonError::into_inner);
                if last.load(Ordering::SeqCst) != number || stop.load(Ordering::SeqCst) {
                    return;
                }
                let copied = copy(&program, &text, TIMEOUT, &stop).map(|()| notice(&text));
                let _ = tx.send(tag(copied));
            })
            .context("spawn the copy")?;
        self.threads.push(thread);
        Ok(())
    }

    /// Lets the copies asked for end until `deadline`, then stops those
    /// left: the one that runs kills its program's group, and those
    /// waiting end without a copy. Waits for them [`STOPPED_WITHIN`] at
    /// most.
    pub(super) fn finish(self, deadline: Instant) {
        self.wait(deadline);
        self.stop.store(true, Ordering::SeqCst);
        self.wait(Instant::now() + STOPPED_WITHIN);
    }

    /// Waits until the threads of the copies asked for have ended, or
    /// until `deadline`.
    fn wait(&self, deadline: Instant) {
        let running = || self.threads.iter().any(|thread| !thread.is_finished());
        while running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Runs `program` in a process group of its own, in [`LOCALE`], with
/// `text` on its standard input, killing the group past `timeout` or once
/// `stop` is set. Fails when it cannot start, takes longer, is stopped,
/// or exits with a status other than 0, saying the first line it printed
/// on its standard error, else the status.
fn copy(program: &Path, text: &str, timeout: Duration, stop: &AtomicBool) -> Result<()> {
    let name = group::name(program);
    let (variable, locale) = LOCALE;
    let mut child = Command::new(program)
        .env(variable, locale)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("run {}", program.display()))?;
    let group = group::of(&child)?;
    let deadline = Instant::now() + timeout;
    let stderr = group::drain(child.stderr.take());
    group::feed(child.stdin.take(), text.as_bytes().to_vec());
    match group::wait_or_stop(&mut child, deadline, stop, group, &name)? {
        Waited::Exited(status) if status.success() => Ok(()),
        Waited::Exited(status) => {
            let printed = String::from_utf8_lossy(&group::collect(&stderr, group)).into_owned();
            match group::first_line(&printed) {
                Some(line) => bail!("{line}"),
                None => bail!("{name} exited with {status}"),
            }
        }
        Waited::Killed if stop.load(Ordering::SeqCst) => bail!("{name} was stopped"),
        Waited::Killed => bail!("{name} took longer than {timeout:?} and was killed"),
    }
}

/// The notice once `text` is on the clipboard: how many lines it has, or
/// how many characters when it has one.
fn notice(text: &str) -> String {
    strings::copied(text.split('\n').count(), text.graphemes(true).count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::errno::Errno;
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use tempfile::TempDir;

    /// How long a fake copy gets to answer.
    const WAIT: Duration = Duration::from_secs(10);

    /// A program in `dir` that runs `script` with `sh`, standing in for
    /// `pbcopy`.
    fn fake(dir: &Path, script: &str) -> PathBuf {
        let program = dir.join("pbcopy");
        fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("write");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod");
        program
    }

    /// The answer of a copy, the error as text.
    type Answer = Result<String, String>;

    /// Copies `text` with `program` as the app does, its answer reaching
    /// `tx`.
    fn ask(clipboard: &mut Clipboard, program: &Path, text: &str, tx: &mpsc::Sender<Answer>) {
        let text = text.to_owned();
        let tag = |copied: Result<String>| copied.map_err(|error| format!("{error:#}"));
        clipboard
            .copy(program, text, tx.clone(), tag)
            .expect("spawned");
    }

    /// Copies `text` with `program` as the app does, and returns the
    /// answer; none when nothing answers.
    fn answer(program: &Path, text: &str) -> Option<Answer> {
        let (tx, rx) = mpsc::channel();
        ask(&mut Clipboard::default(), program, text, &tx);
        rx.recv_timeout(WAIT).ok()
    }

    /// Waits until the file at `path` has `line` among its lines, at most
    /// [`WAIT`], and returns them.
    fn wait_for(path: &Path, line: &str) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = fs::read_to_string(path).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if lines.iter().any(|written| written == line) {
                return lines;
            }
            assert!(Instant::now() < deadline, "{line} before timeout");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_program_gets_the_text_on_its_input_in_a_utf8_locale() {
        let dir = TempDir::new().expect("temp dir");
        let program = fake(
            dir.path(),
            r#"d=$(dirname "$0"); printf %s "$LC_ALL" > "$d/locale"; cat > "$d/copied""#,
        );
        let copied = dir.path().join("copied");
        let text = "alpha beta\n  delta\nzeta 日本";
        assert_eq!(answer(&program, text), Some(Ok("copied 3 lines".into())));
        assert_eq!(fs::read_to_string(&copied).expect("copied"), text);
        let locale = fs::read_to_string(dir.path().join("locale")).expect("locale");
        assert_eq!(locale, "en_US.UTF-8");
        assert_eq!(
            answer(&program, "epsilon 日本"),
            Some(Ok("copied 10 characters".into()))
        );
        assert_eq!(fs::read_to_string(&copied).expect("copied"), "epsilon 日本");
    }

    #[test]
    fn a_failure_says_the_first_line_it_printed_else_its_status() {
        let dir = TempDir::new().expect("temp dir");
        let says = fake(
            dir.path(),
            "cat >/dev/null; printf '\\n\\033[31mno pasteboard\\033[m\\nmore\\n' >&2; exit 1",
        );
        assert_eq!(answer(&says, "x"), Some(Err("no pasteboard".into())));
        let silent = fake(dir.path(), "exit 3");
        assert_eq!(
            answer(&silent, "x"),
            Some(Err("pbcopy exited with exit status: 3".into()))
        );
        let missing = dir.path().join("none").join("pbcopy");
        let Some(Err(error)) = answer(&missing, "x") else {
            panic!("a failure");
        };
        assert!(
            error.starts_with(&format!("run {}: ", missing.display())),
            "{error}"
        );
        // One that hangs is killed.
        let hangs = fake(dir.path(), "exec sleep 30");
        let started = Instant::now();
        let go = AtomicBool::new(false);
        let error = copy(&hangs, "x", Duration::from_millis(200), &go).expect_err("killed");
        assert_eq!(
            format!("{error:#}"),
            "pbcopy took longer than 200ms and was killed"
        );
        assert!(started.elapsed() < WAIT);
    }

    #[test]
    fn nothing_runs_for_spaces_and_line_ends() {
        let dir = TempDir::new().expect("temp dir");
        let program = fake(dir.path(), r#"cat > "$(dirname "$0")/copied""#);
        let (tx, rx) = mpsc::channel();
        ask(&mut Clipboard::default(), &program, " \n  \n", &tx);
        drop(tx);
        assert_eq!(rx.recv_timeout(WAIT).ok(), None);
        assert!(!dir.path().join("copied").exists());
    }

    #[test]
    fn copies_run_one_at_a_time_and_the_last_one_asked_for_wins() {
        let dir = TempDir::new().expect("temp dir");
        // Each copy takes a while, and logs when it starts and ends.
        let program = fake(
            dir.path(),
            r#"d=$(dirname "$0"); t=$(cat); echo "start $t" >> "$d/log"; sleep 0.5; echo "end $t" >> "$d/log"; printf %s "$t" > "$d/copied""#,
        );
        let log = dir.path().join("log");
        let mut clipboard = Clipboard::default();
        let (tx, rx) = mpsc::channel();
        // Two more are asked for while the first runs: they wait for it,
        // and the one asked for before the last does not run.
        ask(&mut clipboard, &program, "word", &tx);
        wait_for(&log, "start word");
        ask(&mut clipboard, &program, "the line", &tx);
        ask(&mut clipboard, &program, "last one", &tx);
        drop(tx);
        let answers: Vec<Answer> = rx.iter().collect();
        let expected: Vec<Answer> = vec![
            Ok("copied 4 characters".into()),
            Ok("copied 8 characters".into()),
        ];
        assert_eq!(answers, expected);
        assert_eq!(
            fs::read_to_string(dir.path().join("copied")).expect("copied"),
            "last one"
        );
        assert_eq!(
            wait_for(&log, "end last one"),
            ["start word", "end word", "start last one", "end last one"]
        );
    }

    #[test]
    fn quitting_lets_a_copy_land_then_kills_one_that_hangs() {
        let dir = TempDir::new().expect("temp dir");
        let program = fake(dir.path(), r#"cat > "$(dirname "$0")/copied""#);
        let mut clipboard = Clipboard::default();
        let (tx, rx) = mpsc::channel();
        ask(&mut clipboard, &program, "just before", &tx);
        clipboard.finish(Instant::now() + WAIT);
        assert_eq!(
            rx.try_recv().ok(),
            Some(Ok("copied 11 characters".into())),
            "it landed"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("copied")).expect("copied"),
            "just before"
        );
        // One that hangs is killed with its group past the deadline, and
        // the one waiting for it does not run.
        let log = dir.path().join("log");
        let pid = dir.path().join("pid");
        let hangs = fake(
            dir.path(),
            &format!(
                "echo $$ > '{0}.tmp'\nmv '{0}.tmp' '{0}'\necho started >> '{1}'\nexec sleep 30",
                pid.display(),
                log.display()
            ),
        );
        let mut clipboard = Clipboard::default();
        ask(&mut clipboard, &hangs, "hangs", &tx);
        wait_for(&log, "started");
        ask(&mut clipboard, &hangs, "waits", &tx);
        drop(tx);
        clipboard.finish(Instant::now());
        let answers: Vec<Answer> = rx.iter().collect();
        let expected: Vec<Answer> = vec![Err("pbcopy was stopped".into())];
        assert_eq!(answers, expected);
        let group: i32 = fs::read_to_string(&pid)
            .expect("pid")
            .trim()
            .parse()
            .expect("a pid");
        assert_eq!(killpg(Pid::from_raw(group), None), Err(Errno::ESRCH));
        assert_eq!(wait_for(&log, "started"), ["started"], "once");
    }
}
