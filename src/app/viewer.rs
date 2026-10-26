//! A session's viewer: the tab that shows a file of the project as it
//! was when read, the reads that fill it, and how far it is scrolled.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;

use super::Msg;
use super::session::{SessionId, TabId};
use crate::projects::read::{self, Read};
use crate::strings;
use crate::ui::changes::Movement;
use crate::ui::viewer::{Content, Pane, Text};

/// Which read an answer is for: the session, its viewer's tab and the
/// read's number there, which grows with each read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReadKey {
    pub(super) session: SessionId,
    pub(super) tab: TabId,
    read: u32,
}

/// The viewer of a session.
#[derive(Debug)]
pub(super) struct Viewer {
    /// Its tab's id.
    pub(super) id: TabId,
    /// The file it shows, absolute.
    pub(super) path: PathBuf,
    /// The file's name, which names the tab.
    pub(super) name: String,
    /// The file's text as read, or why there is none.
    content: Content,
    /// The first line on screen.
    scroll: usize,
    /// The number of the last read asked for; an answer to any other is
    /// dropped.
    read: u32,
}

impl Viewer {
    /// The viewer of tab `id`, showing nothing yet.
    pub(super) fn new(id: TabId) -> Self {
        Self {
            id,
            path: PathBuf::new(),
            name: String::new(),
            content: Content::Reading,
            scroll: 0,
            read: 0,
        }
    }

    /// Shows the file at `path`, absolute, read again on a thread of its
    /// own whose answer reaches `tx` tagged for session `session`. The
    /// tab says it is reading until then, at the top of another file and
    /// where it was in the same one. The lines are made ready to draw on
    /// that thread. When the thread cannot start, the tab says why.
    pub(super) fn open(&mut self, path: PathBuf, session: SessionId, tx: &Sender<Msg>) {
        if path != self.path {
            self.scroll = 0;
        }
        self.name = file_name(&path);
        self.read += 1;
        self.content = Content::Reading;
        let key = ReadKey {
            session,
            tab: self.id,
            read: self.read,
        };
        let spawned = read::spawn(path.clone(), tx.clone(), move |read| {
            Msg::Read(key, content(read))
        });
        if let Err(error) = spawned {
            self.content = Content::Failed(format!("{error:#}"));
        }
        self.path = path;
    }

    /// Takes `content`, the answer to the read `key` names, when it is
    /// the last one this tab asked for. Returns whether it took it.
    pub(super) fn answer(&mut self, key: ReadKey, content: Content) -> bool {
        let wanted = key.tab == self.id && key.read == self.read;
        if wanted {
            self.content = content;
        }
        wanted
    }

    /// Moves the view by `movement` in a pane `height` lines tall, no
    /// further than the first line at the top and the last at the
    /// bottom: a line, half a page or a page up or down, to the end, or
    /// to the top.
    pub(super) fn scroll(&mut self, movement: Movement, height: usize) {
        self.scroll = movement.offset(self.scroll, self.content.lines(), height);
    }

    /// What the center pane draws of it.
    pub(super) fn pane(&self) -> Pane<'_> {
        Pane {
            content: &self.content,
            scroll: self.scroll,
        }
    }
}

/// The last component of `path`, or the path itself when it has none.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What the viewer shows for `read`: the file's text, ready to draw, or
/// why it is not shown or could not be read.
fn content(read: Result<Read>) -> Content {
    match read {
        Ok(Read::Text(text)) => Content::Text(Text::new(&text)),
        Ok(Read::Binary) => Content::Note(strings::FILE_BINARY),
        Ok(Read::TooLarge) => Content::Note(strings::FILE_TOO_LARGE),
        Ok(Read::NotAFile) => Content::Note(strings::FILE_NOT_REGULAR),
        Err(error) => Content::Failed(format!("{error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::session::Sessions;
    use crate::app::session::tests::tab;
    use crate::ui::changes::Step;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    /// A viewer of `lines` numbered lines, read for `session`.
    fn viewing(lines: usize) -> Viewer {
        let mut sessions = Sessions::new();
        let id = sessions.next_id();
        let mut viewer = Viewer::new(tab(1));
        let text: String = (1..=lines).map(|n| format!("{n}\n")).collect();
        let key = ReadKey {
            session: id,
            tab: viewer.id,
            read: viewer.read,
        };
        assert!(viewer.answer(key, Content::Text(Text::new(&text))));
        viewer
    }

    #[test]
    fn the_scroll_stops_at_the_top_and_with_the_last_line_at_the_bottom() {
        let mut viewer = viewing(10);
        let height = 4;
        let mut moves = |movement| {
            viewer.scroll(movement, height);
            viewer.scroll
        };
        assert_eq!(moves(Movement::Line(Step::Up)), 0, "at the top");
        assert_eq!(moves(Movement::Line(Step::Down)), 1);
        assert_eq!(moves(Movement::Page(Step::Down)), 5);
        assert_eq!(
            moves(Movement::Page(Step::Down)),
            6,
            "the last line at the bottom"
        );
        assert_eq!(moves(Movement::Line(Step::Down)), 6);
        assert_eq!(moves(Movement::Page(Step::Up)), 2);
        assert_eq!(moves(Movement::Top), 0);
        assert_eq!(moves(Movement::End), 6, "the end");
        assert_eq!(moves(Movement::Half(Step::Up)), 4, "half the pane");
        assert_eq!(moves(Movement::Half(Step::Up)), 2);
        assert_eq!(moves(Movement::Half(Step::Down)), 4);
        assert_eq!(moves(Movement::Half(Step::Down)), 6);
        assert_eq!(moves(Movement::Half(Step::Down)), 6);
        // A pane that grew moves from where it would draw.
        viewer.scroll(Movement::Line(Step::Up), 8);
        assert_eq!(viewer.scroll, 1);
        // Shorter than the pane, nothing moves.
        let mut short = viewing(3);
        short.scroll(Movement::End, 4);
        short.scroll(Movement::Page(Step::Down), 4);
        assert_eq!(short.scroll, 0);
        // Nor without text.
        let mut empty = Viewer::new(tab(1));
        empty.scroll(Movement::Line(Step::Down), 4);
        assert_eq!(empty.scroll, 0);
    }

    #[test]
    fn an_answer_counts_only_for_the_last_read_of_its_tab() {
        let temporary = TempDir::new().expect("temp dir");
        let (one, two) = (
            temporary.path().join("one.rs"),
            temporary.path().join("two.rs"),
        );
        std::fs::write(&one, "1\n2\n3\n").expect("write");
        std::fs::write(&two, "two\n").expect("write");
        let (tx, rx) = mpsc::channel();
        let mut sessions = Sessions::new();
        let session = sessions.next_id();
        let mut viewer = Viewer::new(tab(1));
        viewer.open(one.clone(), session, &tx);
        assert_eq!(
            (viewer.name.as_str(), &viewer.content),
            ("one.rs", &Content::Reading)
        );
        let answer = |rx: &mpsc::Receiver<Msg>| match rx
            .recv_timeout(Duration::from_secs(10))
            .expect("an answer")
        {
            Msg::Read(key, content) => (key, content),
            _ => panic!("only the read answers"),
        };
        let (first, content) = answer(&rx);
        assert_eq!((first.session, first.tab), (session, viewer.id));
        assert!(viewer.answer(first, content));
        assert_eq!(viewer.content.lines(), 3);
        viewer.scroll(Movement::Line(Step::Down), 1);
        // The same file read again keeps its place; another starts at its
        // top. An answer to an earlier read is dropped.
        viewer.open(one.clone(), session, &tx);
        assert_eq!(viewer.scroll, 1);
        let (again, content) = answer(&rx);
        assert!(!viewer.answer(first, Content::Note(strings::FILE_BINARY)));
        assert_eq!(viewer.content, Content::Reading);
        assert!(viewer.answer(again, content));
        viewer.open(two.clone(), session, &tx);
        assert_eq!((viewer.name.as_str(), viewer.scroll), ("two.rs", 0));
        let (last, content) = answer(&rx);
        assert!(!viewer.answer(again, Content::Reading), "an earlier read");
        let other = Viewer::new(tab(2));
        let elsewhere = ReadKey {
            tab: other.id,
            ..last
        };
        assert!(!viewer.answer(elsewhere, Content::Reading), "another tab");
        assert!(viewer.answer(last, content));
        assert_eq!(viewer.pane().content, &Content::Text(Text::new("two\n")));
        // What it found when the file is not shown, or not there.
        viewer.open(temporary.path().to_path_buf(), session, &tx);
        let (key, content) = answer(&rx);
        assert_eq!(content, Content::Note(strings::FILE_NOT_REGULAR));
        assert!(viewer.answer(key, content));
        viewer.open(temporary.path().join("gone"), session, &tx);
        let (_, content) = answer(&rx);
        assert!(matches!(&content, Content::Failed(error) if error.contains("gone")));
    }

    #[test]
    fn a_read_shows_what_it_found() {
        assert_eq!(
            content(Ok(Read::Text("a\tb\n".into()))),
            Content::Text(Text::new("a    b"))
        );
        assert_eq!(
            content(Ok(Read::Binary)),
            Content::Note(strings::FILE_BINARY)
        );
        assert_eq!(
            content(Ok(Read::TooLarge)),
            Content::Note(strings::FILE_TOO_LARGE)
        );
        assert_eq!(
            content(Err(anyhow::anyhow!("no"))),
            Content::Failed("no".into())
        );
        assert_eq!(file_name(Path::new("/p/src/main.rs")), "main.rs");
        assert_eq!(file_name(Path::new("/")), "/");
    }
}
