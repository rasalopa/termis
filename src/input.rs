//! Host terminal input: reads crossterm events on a thread and forwards
//! them over a channel.
//!
//! crossterm has one global event reader; nothing else may call
//! `crossterm::event::{poll, read}`, `cursor::position` or
//! `terminal::supports_keyboard_enhancement` while a [`Reader`] is alive.
//! The caller owns the terminal modes: enable raw mode before
//! [`Reader::spawn`] (otherwise bytes arrive line by line and Ctrl+C kills
//! the process) and drop the reader before restoring the terminal. Paste,
//! focus and key release events appear only when the caller enabled them.
//! Resize events come from SIGWINCH, which crossterm handles itself.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event;

/// How often the thread checks whether it should stop, at least.
const POLL: Duration = Duration::from_millis(50);

/// Something the host terminal did.
#[derive(Debug)]
pub enum Event {
    /// A key, resize, paste or focus change from the host.
    Host(event::Event),
    /// Reading failed; no more events follow. Arrives at once when neither
    /// stdin nor `/dev/tty` is a terminal.
    Failed(io::Error),
}

/// The reader thread. Dropping it asks the thread to stop and waits
/// briefly; see [`Drop`].
pub struct Reader {
    stop: Arc<AtomicBool>,
    done: Receiver<()>,
    handle: Option<JoinHandle<()>>,
}

impl Reader {
    /// Starts reading host events and sending them to `events`, converted
    /// into the caller's message type, until dropped. A read error is
    /// reported as [`Event::Failed`] and ends the thread; a send that fails
    /// because the receiver is gone ends it silently.
    pub fn spawn<E>(events: Sender<E>) -> Result<Self>
    where
        E: From<Event> + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let (done_tx, done) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("input-reader".into())
            .spawn(move || {
                // Dropped when the thread ends, whichever way it ends.
                let _done = done_tx;
                let error = loop {
                    // Relaxed is enough: the flag is a hint and the done
                    // channel orders the rest.
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    match event::poll(POLL) {
                        Ok(false) => continue,
                        Ok(true) => match event::read() {
                            Ok(ev) => {
                                if events.send(E::from(Event::Host(ev))).is_err() {
                                    return;
                                }
                            }
                            Err(e) => break e,
                        },
                        Err(e) => break e,
                    }
                };
                let _ = events.send(E::from(Event::Failed(error)));
            })
            .context("start input thread")?;
        Ok(Self {
            stop,
            done,
            handle: Some(handle),
        })
    }
}

impl Drop for Reader {
    /// Waits up to a few polls. crossterm reads the tty with a blocking
    /// `read(2)` and can sit in `poll` past its timeout (half an escape
    /// sequence) or spin on end of file (terminal hangup); a thread that
    /// does not come back is left to die with the process.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.done.recv_timeout(POLL * 4) == Err(RecvTimeoutError::Timeout) {
            return;
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Only one reader may exist per process: crossterm's reader is global.
    #[test]
    fn stops_promptly_on_drop() {
        let (tx, rx) = mpsc::channel::<Event>();
        let started = Instant::now();
        let reader = Reader::spawn(tx).expect("spawn");
        drop(reader);
        assert!(started.elapsed() < Duration::from_secs(1), "drop blocked");
        // Without a terminal the thread reports the failure; with one it
        // just stops. Either way no sender is left.
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Event::Failed(_)) | Err(RecvTimeoutError::Disconnected) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}
