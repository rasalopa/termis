//! The host terminal: raw mode, alternate screen and the modes the app
//! needs, restored on quit and on panic.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// The child the panic hook hangs up: its pid and the flag that says it
/// already exited.
static CHILD: Mutex<Option<(u32, Arc<AtomicBool>)>> = Mutex::new(None);
/// The instance directory the panic hook removes, which no destructor
/// would once it exits.
static INSTANCE: Mutex<Option<PathBuf>> = Mutex::new(None);

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

    /// Registers the child the panic hook hangs up, with the flag that
    /// says it already exited.
    pub fn watch_child(pid: Option<u32>, exited: Arc<AtomicBool>) {
        *lock_child() = pid.map(|pid| (pid, exited));
    }

    /// Forgets the child.
    pub fn forget_child() {
        *lock_child() = None;
    }

    /// Registers the instance directory the panic hook removes.
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

fn lock_child() -> std::sync::MutexGuard<'static, Option<(u32, Arc<AtomicBool>)>> {
    CHILD.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_instance() -> std::sync::MutexGuard<'static, Option<PathBuf>> {
    INSTANCE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Turns on mouse reporting of button presses and the wheel, in SGR
/// form, but not of motion: moving the mouse sends nothing. Crossterm's
/// own command would report every motion.
struct EnableMouse;

impl Command for EnableMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1006h")
    }
}

/// Undoes [`EnableMouse`].
struct DisableMouse;

impl Command for DisableMouse {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1006l\x1b[?1000l")
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

/// On a panic in any thread: hang up the child unless it already exited,
/// restore the terminal, print the panic, and exit, so a dead worker never
/// leaves a half-drawn screen behind. Uses `try_lock` so a panic while the
/// main thread holds the child lock cannot deadlock.
fn install_panic_hook(keyboard_flags: bool) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut child) = CHILD.try_lock()
            && let Some((pid, exited)) = child.take()
            && !exited.load(Ordering::SeqCst)
        {
            let _ = crate::pty::hangup_group(pid);
        }
        if let Ok(mut instance) = INSTANCE.try_lock()
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

    #[test]
    fn remembers_and_forgets_the_child() {
        let exited = Arc::new(AtomicBool::new(false));
        Host::watch_child(Some(4242), Arc::clone(&exited));
        assert_eq!(lock_child().as_ref().map(|(pid, _)| *pid), Some(4242));
        Host::forget_child();
        assert!(lock_child().is_none());
        Host::watch_child(None, exited);
        assert!(lock_child().is_none());
    }
}
