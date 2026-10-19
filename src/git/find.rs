//! The repository finder: finds the repository of a directory off the
//! main thread (ADR-0008) and answers over the app's channel (ADR-0002).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};

/// The finder's answer for one directory.
#[derive(Debug)]
pub struct Found {
    /// The directory asked about.
    pub dir: PathBuf,
    /// The root of the working tree that contains `dir`, none when `dir`
    /// is not inside one, or why git could not tell.
    pub root: Result<Option<PathBuf>>,
}

/// A handle to the finder thread. Dropping it ends the thread once it
/// answered what was asked before; a git command still running finishes
/// on its own.
pub struct Finder {
    asks: Sender<PathBuf>,
}

impl Finder {
    /// Starts the finder; its answers reach `tx`, wrapped by `tag` into
    /// the owner's message type.
    pub fn spawn<E>(tx: Sender<E>, tag: impl Fn(Found) -> E + Send + 'static) -> Result<Self>
    where
        E: Send + 'static,
    {
        let (asks, inbox) = mpsc::channel();
        thread::Builder::new()
            .name("git-finder".into())
            .spawn(move || serve(&inbox, &tx, tag))
            .context("spawn the git finder")?;
        Ok(Self { asks })
    }

    /// Asks for the repository of `dir`; the answer names it. Asks are
    /// answered in order. Nothing happens once the finder is gone.
    pub fn ask(&self, dir: PathBuf) {
        let _ = self.asks.send(dir);
    }
}

/// Answers until the owner drops the handle or stops listening; each
/// answer goes to `tx` wrapped by `tag`.
fn serve<E>(inbox: &Receiver<PathBuf>, tx: &Sender<E>, tag: impl Fn(Found) -> E) {
    for dir in inbox {
        let root = super::repo_root(&dir);
        if tx.send(tag(Found { dir, root })).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::init;
    use std::path::Path;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long an answer gets.
    const WAIT: Duration = Duration::from_secs(10);

    /// `path` with its links resolved, as git prints a root.
    fn real(path: &Path) -> PathBuf {
        path.canonicalize().expect("exists")
    }

    #[test]
    fn answers_each_directory_with_its_repository() {
        let dir = TempDir::new().expect("temp dir");
        let outer = dir.path().join("outer");
        let sub = outer.join("a/b");
        let inner = outer.join("vendor/inner");
        let outside = dir.path().join("plain");
        let missing = dir.path().join("missing");
        for path in [&sub, &inner, &outside] {
            std::fs::create_dir_all(path).expect("mkdir");
        }
        init(&outer);
        init(&inner);
        // Each answer tagged with the name of the thread that sent it.
        let (tx, rx) = mpsc::channel();
        let finder = Finder::spawn(tx, |found| {
            (thread::current().name().map(str::to_owned), found)
        })
        .expect("spawns");
        for path in [&sub, &inner, &outside, &missing] {
            finder.ask(path.clone());
        }
        let answer = |asked: &Path| {
            let (thread, found) = rx.recv_timeout(WAIT).expect("an answer");
            assert_eq!(thread.as_deref(), Some("git-finder"));
            assert_eq!(found.dir, asked, "names the directory it answers");
            found.root
        };
        let root = answer(&sub).expect("git runs").expect("in the outer one");
        assert_eq!(real(&root), real(&outer));
        let root = answer(&inner).expect("git runs").expect("in the inner one");
        assert_eq!(real(&root), real(&inner), "the nested repository");
        // A temporary directory is not inside a repository unless the
        // system's is, which would be unusual.
        let root = answer(&outside).expect("git runs");
        let mine = real(dir.path());
        assert!(root.is_none_or(|root| !real(&root).starts_with(&mine)));
        // The directory is named: it is not git that is missing.
        let error = answer(&missing).expect_err("no such directory");
        let message = format!("{error:#}");
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn the_thread_ends_with_its_handle_or_its_listener() {
        let dir = TempDir::new().expect("temp dir");
        // Dropping the handle: the thread drops its end of the channel.
        let (tx, rx) = mpsc::channel();
        let finder = Finder::spawn(tx, |found| found).expect("spawns");
        finder.ask(dir.path().to_path_buf());
        drop(finder);
        assert_eq!(
            rx.recv_timeout(WAIT).expect("what was asked before").dir,
            dir.path()
        );
        assert_eq!(
            rx.recv_timeout(WAIT).expect_err("no more"),
            RecvTimeoutError::Disconnected,
            "the thread ends with the handle"
        );

        // No one listening: the first answer that cannot be sent ends the
        // thread, and with it the tag, which holds `tagged`.
        let (tx, rx) = mpsc::channel::<Found>();
        let (tagged, tags) = mpsc::channel::<()>();
        let finder = Finder::spawn(tx, move |found| {
            let _ = tagged.send(());
            found
        })
        .expect("spawns");
        drop(rx);
        finder.ask(dir.path().to_path_buf());
        assert!(tags.recv_timeout(WAIT).is_ok(), "the answer is made");
        assert_eq!(
            tags.recv_timeout(WAIT).expect_err("no more"),
            RecvTimeoutError::Disconnected,
            "the thread ends with its listener"
        );
        drop(finder);
    }
}
