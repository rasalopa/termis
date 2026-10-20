//! The host terminal: raw mode, alternate screen and the modes the app
//! needs, restored on quit and on panic.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{Command, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// How many times the panic hook tries a lock that is held.
const HOOK_TRIES: u32 = 100;
/// How long the panic hook waits between two tries.
const HOOK_RETRY: Duration = Duration::from_millis(1);

/// A child the panic hook hangs up.
struct Entry {
    /// Tells the entry from the others, so its [`OnPanic`] removes it.
    key: u64,
    /// The child's pid, which is also its process group's id.
    pid: u32,
    /// Set once the child exited, after which its pid is not signalled.
    exited: Arc<AtomicBool>,
}

/// The children the panic hook hangs up, one per agent's pty.
static CHILDREN: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// The key the next entry in [`CHILDREN`] gets.
static NEXT_KEY: AtomicU64 = AtomicU64::new(0);
/// The process's instance directory, which the panic hook removes with
/// the sessions' in it, as no destructor would once it exits.
static INSTANCE: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Whether a panic reached the hook already; one in another thread waits
/// for the first one's exit.
static HOOK_RAN: AtomicBool = AtomicBool::new(false);

/// The host terminal while the app owns it.
pub struct Host {
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
    keyboard_flags: bool,
}

impl Host {
    /// Enters raw mode and the alternate screen, enables bracketed paste,
    /// focus events and mouse reporting, and pushes the kitty disambiguate
    /// flag when the host supports it. Must run before any input thread
    /// starts. On failure the terminal is left as it was.
    pub fn enter() -> Result<Self> {
        let supports_flags = terminal::supports_keyboard_enhancement().unwrap_or(false);
        terminal::enable_raw_mode().context("enable raw mode")?;
        let terminal = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => terminal,
            Err(e) => {
                let _ = terminal::disable_raw_mode();
                return Err(e).context("create the terminal");
            }
        };
        // From here on a drop restores the terminal.
        let mut host = Self {
            terminal,
            keyboard_flags: false,
        };
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableFocusChange,
            EnableMouse
        )
        .context("enable host modes")?;
        if supports_flags {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .context("push keyboard flags")?;
            host.keyboard_flags = true;
        }
        install_panic_hook(host.keyboard_flags);
        Ok(host)
    }

    /// Reports mouse motion with no button held too, or stops reporting
    /// it.
    pub fn report_motion(&mut self, on: bool) -> Result<()> {
        execute!(io::stdout(), ReportMotion(on)).context("set host mouse motion")
    }

    /// Rings the terminal's bell.
    pub fn bell(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        stdout
            .write_all(b"\x07")
            .and_then(|()| stdout.flush())
            .context("ring the host bell")
    }

    /// Registers a child for the panic hook to hang up, with the flag
    /// that says it already exited, until the guard is dropped. Without a
    /// pid nothing is registered.
    pub fn watch_child(pid: Option<u32>, exited: Arc<AtomicBool>) -> OnPanic {
        OnPanic(pid.map(|pid| {
            let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
            lock_children().push(Entry { key, pid, exited });
            key
        }))
    }

    /// Registers the process's instance directory, which the panic hook
    /// removes with everything in it.
    pub fn watch_instance(dir: &Path) {
        *lock_instance() = Some(dir.to_path_buf());
    }

    /// Forgets the instance directory.
    pub fn forget_instance() {
        *lock_instance() = None;
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        restore(self.keyboard_flags);
    }
}

/// A child's entry with the panic hook, from [`Host::watch_child`]: until
/// this is dropped, a panic hangs the child up.
#[must_use = "dropping it forgets the child"]
pub struct OnPanic(Option<u64>);

impl Drop for OnPanic {
    fn drop(&mut self) {
        if let Some(key) = self.0 {
            lock_children().retain(|entry| entry.key != key);
        }
    }
}

fn lock_children() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    CHILDREN.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_instance() -> std::sync::MutexGuard<'static, Option<PathBuf>> {
    INSTANCE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The pids of the `children` that have not exited.
fn to_hang_up(children: &[Entry]) -> impl Iterator<Item = u32> {
    children
        .iter()
        .filter(|entry| !entry.exited.load(Ordering::SeqCst))
        .map(|entry| entry.pid)
}

/// Whether the panic hook knows the child behind `exited`.
#[cfg(test)]
pub(super) fn is_watched(exited: &Arc<AtomicBool>) -> bool {
    lock_children()
        .iter()
        .any(|entry| Arc::ptr_eq(&entry.exited, exited))
}

/// `mutex` locked for the panic hook, taken as it is when poisoned. None
/// when it stays held for [`HOOK_TRIES`] tries, as when this thread
/// panicked holding it.
fn lock_for_hook<T>(mutex: &Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    for _ in 0..HOOK_TRIES {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => thread::sleep(HOOK_RETRY),
        }
    }
    None
}

/// Turns on mouse reporting of button presses and releases, the wheel and
/// motion while a button is held, in SGR form. Motion with no button held
/// waits for [`ReportMotion`]; crossterm's own command would report it
/// from the start.
struct EnableMouse;

impl Command for EnableMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1002h\x1b[?1006h")
    }
}

/// Adds motion with no button held to what [`EnableMouse`] reports, or
/// takes it away again. Turning the mode off ends all reporting, so the
/// drags mode comes back after it.
struct ReportMotion(bool);

impl Command for ReportMotion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str(if self.0 {
            "\x1b[?1003h"
        } else {
            "\x1b[?1003l\x1b[?1002h"
        })
    }
}

/// Undoes [`EnableMouse`] and [`ReportMotion`].
struct DisableMouse;

impl Command for DisableMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l")
    }
}

/// Undoes [`Host::enter`] and shows the cursor. Safe to call more than
/// once.
fn restore(keyboard_flags: bool) {
    let mut stdout = io::stdout();
    if keyboard_flags {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout,
        DisableMouse,
        DisableBracketedPaste,
        DisableFocusChange,
        Show,
        LeaveAlternateScreen
    );
    let _ = stdout.flush();
    let _ = terminal::disable_raw_mode();
}

/// On a panic in any thread: hang up every child that has not exited,
/// restore the terminal, print the panic, and exit, so a dead worker never
/// leaves a half-drawn screen behind. A panic in another thread meanwhile
/// waits for that exit. Locks are tried a bounded number of times, so a
/// panic while a thread holds one cannot deadlock.
fn install_panic_hook(keyboard_flags: bool) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if HOOK_RAN.swap(true, Ordering::SeqCst) {
            loop {
                thread::park();
            }
        }
        if let Some(children) = lock_for_hook(&CHILDREN) {
            for pid in to_hang_up(&children) {
                let _ = crate::pty::hangup_group(pid);
            }
        }
        if let Some(mut instance) = lock_for_hook(&INSTANCE)
            && let Some(dir) = instance.take()
        {
            let _ = std::fs::remove_dir_all(dir);
        }
        restore(keyboard_flags);
        default(info);
        std::process::exit(101);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_forgets_the_instance() {
        Host::watch_instance(Path::new("/tmp/termis-0"));
        assert_eq!(lock_instance().as_deref(), Some(Path::new("/tmp/termis-0")));
        Host::forget_instance();
        assert!(lock_instance().is_none());
    }

    /// Pids that cannot be a process: past `i32::MAX`, which the hangup
    /// refuses.
    const FAKE: [u32; 3] = [u32::MAX - 1, u32::MAX - 2, u32::MAX - 3];

    /// The pid registered under `key`. Tests that spawn an agent register
    /// theirs meanwhile, so each test looks only at its own keys.
    fn pid_of(key: u64) -> Option<u32> {
        lock_children()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.pid)
    }

    #[test]
    fn each_guard_keeps_its_own_child() {
        let exited = Arc::new(AtomicBool::new(false));
        let one = Host::watch_child(Some(FAKE[0]), Arc::clone(&exited));
        let two = Host::watch_child(Some(FAKE[1]), exited);
        let (key_one, key_two) = (one.0.expect("registered"), two.0.expect("registered"));
        assert_ne!(key_one, key_two);
        assert_eq!(pid_of(key_one), Some(FAKE[0]));
        assert_eq!(pid_of(key_two), Some(FAKE[1]));
        drop(one);
        assert_eq!(pid_of(key_one), None);
        assert_eq!(pid_of(key_two), Some(FAKE[1]), "the other stays");
        drop(two);
        assert_eq!(pid_of(key_two), None);
    }

    #[test]
    fn a_child_without_a_pid_is_not_registered() {
        let exited = Arc::new(AtomicBool::new(false));
        let on_panic = Host::watch_child(None, Arc::clone(&exited));
        assert_eq!(on_panic.0, None);
        assert!(!is_watched(&exited));
    }

    #[test]
    fn only_children_that_have_not_exited_are_hung_up() {
        let running = Arc::new(AtomicBool::new(false));
        let entry = |key, pid, exited| Entry { key, pid, exited };
        let children = [
            entry(0, FAKE[0], Arc::clone(&running)),
            entry(1, FAKE[1], Arc::new(AtomicBool::new(true))),
            entry(2, FAKE[2], Arc::clone(&running)),
        ];
        assert_eq!(
            to_hang_up(&children).collect::<Vec<_>>(),
            [FAKE[0], FAKE[2]]
        );
        running.store(true, Ordering::SeqCst);
        assert_eq!(to_hang_up(&children).count(), 0, "all exited since");
    }

    #[test]
    fn the_hook_takes_a_poisoned_lock_and_gives_up_on_a_held_one() {
        let mutex = Arc::new(Mutex::new(1));
        let poisoner = Arc::clone(&mutex);
        let _ = thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poisons the lock");
        })
        .join();
        assert!(mutex.is_poisoned());
        assert_eq!(lock_for_hook(&mutex).map(|value| *value), Some(1));
        let _held = mutex.lock().unwrap_or_else(|e| e.into_inner());
        assert!(lock_for_hook(&mutex).is_none(), "held by this thread");
    }
}
