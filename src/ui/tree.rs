//! The file tree of the projects pane: a session's files folded by
//! directory, with a cursor, each changed file's status letter and a
//! mark on the directories that hold changes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::changes::{Movement, cut, letter_style, pad, printable, scrolled};
use super::{Target, Targets, draw_rows, palette};
use crate::git::status::{Change, Status};
use crate::strings;

/// A directory of the tree: its directories and its files, each by
/// name.
#[derive(Debug, Default, PartialEq, Eq)]
struct Dir {
    dirs: BTreeMap<OsString, Dir>,
    files: BTreeSet<OsString>,
}

/// The files of a directory as a tree, built once from their list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tree {
    root: Dir,
    /// How many files the list held when it was cut, with files left
    /// out; none when it was whole.
    cut: Option<usize>,
}

impl Tree {
    /// The tree of `paths`, relative to its directory, from a list cut at
    /// `cut` files or whole. A path that is not plainly relative, such as
    /// one with `..`, is left out.
    pub fn new(paths: &[PathBuf], cut: Option<usize>) -> Self {
        let mut tree = Self {
            cut,
            ..Self::default()
        };
        for path in paths {
            let mut names = Vec::new();
            for component in path.components() {
                match component {
                    Component::Normal(name) => names.push(name),
                    Component::CurDir => {}
                    _ => {
                        names.clear();
                        break;
                    }
                }
            }
            let Some((file, dirs)) = names.split_last() else {
                continue;
            };
            let mut dir = &mut tree.root;
            for name in dirs {
                dir = dir.dirs.entry((*name).to_owned()).or_default();
            }
            dir.files.insert((*file).to_owned());
        }
        tree
    }

    /// The rows of the tree with the directories in `open` unfolded: in
    /// each directory its directories first, then its files, each by
    /// name, the entries of an open directory right under it.
    pub fn rows(&self, open: &HashSet<PathBuf>) -> Vec<Row> {
        let mut rows = Vec::new();
        push_rows(&self.root, Path::new(""), 0, open, &mut rows);
        rows
    }
}

/// Adds the rows of the entries of `dir`, at `path`, `depth` directories
/// in, to `rows`.
fn push_rows(dir: &Dir, path: &Path, depth: usize, open: &HashSet<PathBuf>, rows: &mut Vec<Row>) {
    for (name, inner) in &dir.dirs {
        let path = path.join(name);
        let unfolded = open.contains(&path);
        rows.push(Row {
            path: path.clone(),
            depth,
            kind: Kind::Dir { open: unfolded },
        });
        if unfolded {
            push_rows(inner, &path, depth + 1, open, rows);
        }
    }
    for name in &dir.files {
        rows.push(Row {
            path: path.join(name),
            depth,
            kind: Kind::File,
        });
    }
}

/// A row of the tree as it is unfolded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The entry's path, relative to the tree's directory.
    pub path: PathBuf,
    /// How many directories in from the top level.
    pub depth: usize,
    /// What the entry is.
    pub kind: Kind,
}

impl Row {
    /// The entry's name: the last part of its path.
    fn name(&self) -> &OsStr {
        self.path.file_name().unwrap_or_default()
    }
}

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A directory, unfolded or not.
    Dir {
        /// Whether it is unfolded.
        open: bool,
    },
    /// A file of the list.
    File,
}

/// What a session keeps of its tree: the directories unfolded, the entry
/// under the cursor and how far the tree is scrolled. A tree read again
/// keeps them; an entry that went loses the cursor to the row at its
/// place.
#[derive(Debug, Default)]
pub struct Folds {
    /// The directories unfolded; the top level always is.
    open: HashSet<PathBuf>,
    /// The entry under the cursor, whether it is a directory, and the
    /// index of its row when it was put there; none at first, for the
    /// first row.
    cursor: Option<(PathBuf, bool, usize)>,
    /// The first row on screen.
    pub scroll: usize,
}

impl Folds {
    /// The index of the row under the cursor among `rows`: the entry's
    /// own while it is listed, else the row at its index, else the last.
    pub fn cursor(&self, rows: &[Row]) -> usize {
        let Some((path, dir, index)) = &self.cursor else {
            return 0;
        };
        rows.iter()
            .position(|row| row.path == *path && matches!(row.kind, Kind::Dir { .. }) == *dir)
            .unwrap_or_else(|| (*index).min(rows.len().saturating_sub(1)))
    }

    /// The row of `tree` under the cursor; none when the tree is empty.
    pub fn row(&self, tree: &Tree) -> Option<Row> {
        let rows = tree.rows(&self.open);
        let at = self.cursor(&rows);
        rows.into_iter().nth(at)
    }

    /// Puts the cursor on the entry of `tree` at `path`, a directory when
    /// `dir`, while the tree lists it as it is unfolded; elsewhere it
    /// stays.
    pub fn point(&mut self, tree: &Tree, path: &Path, dir: bool) {
        let rows = tree.rows(&self.open);
        let listed = rows
            .iter()
            .position(|row| row.path == path && matches!(row.kind, Kind::Dir { .. }) == dir);
        if let Some(index) = listed {
            self.put(&rows, index);
        }
    }

    /// Puts the cursor on row `index` of `rows`.
    fn put(&mut self, rows: &[Row], index: usize) {
        if let Some(row) = rows.get(index) {
            let dir = matches!(row.kind, Kind::Dir { .. });
            self.cursor = Some((row.path.clone(), dir, index));
        }
    }

    /// Moves the cursor by `movement` through the rows of `tree`, `page`
    /// of which show at once, no further than the ends.
    pub fn step(&mut self, tree: &Tree, movement: Movement, page: usize) {
        let rows = tree.rows(&self.open);
        let at = self.cursor(&rows);
        self.put(&rows, movement.index(at, rows.len(), page));
    }

    /// Unfolds the directory under the cursor; nothing on a file.
    pub fn open(&mut self, tree: &Tree) {
        let rows = tree.rows(&self.open);
        let at = self.cursor(&rows);
        if let Some(row) = rows.get(at)
            && row.kind == (Kind::Dir { open: false })
        {
            self.open.insert(row.path.clone());
            self.put(&rows, at);
        }
    }

    /// Folds the directory under the cursor when it is unfolded, else
    /// moves the cursor to the directory the entry is in, unless that is
    /// the top level.
    pub fn close(&mut self, tree: &Tree) {
        let rows = tree.rows(&self.open);
        let at = self.cursor(&rows);
        let Some(row) = rows.get(at) else {
            return;
        };
        if row.kind == (Kind::Dir { open: true }) {
            self.open.remove(&row.path);
            self.put(&rows, at);
        } else if let Some(parent) = row.path.parent()
            && !parent.as_os_str().is_empty()
            && let Some(index) = rows
                .iter()
                .position(|row| row.path == parent && matches!(row.kind, Kind::Dir { .. }))
        {
            self.put(&rows, index);
        }
    }
}

/// The changes the tree shows: each changed file's, and the directories
/// that hold one, by their paths in the tree.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    files: HashMap<PathBuf, Change>,
    dirs: HashSet<PathBuf>,
}

impl Changes {
    /// The changes of `status` under `below`, the tree's directory
    /// relative to the repository's root: a file's in the work tree, else
    /// in the index. A deleted file is not in the tree, but its
    /// directories hold a change.
    pub fn new(status: &Status, below: &Path) -> Self {
        let mut changes = Self::default();
        for entry in &status.entries {
            let Some(change) = entry.worktree.or(entry.index) else {
                continue;
            };
            let Ok(path) = entry.path.strip_prefix(below) else {
                continue;
            };
            changes.dirs.extend(
                path.ancestors()
                    .skip(1)
                    .filter(|dir| !dir.as_os_str().is_empty())
                    .map(Path::to_path_buf),
            );
            changes.files.insert(path.to_path_buf(), change);
        }
        changes
    }
}

/// What the projects pane shows of a session's files, in place of the
/// list of sessions and the card.
#[derive(Clone, Copy, Debug)]
pub struct Pane<'a> {
    /// The heading over the tree: the project's name.
    pub heading: &'a str,
    /// The tree as last read, or why the files could not be; none until
    /// the first answer.
    pub content: Option<Result<&'a Tree, &'a str>>,
    /// The directories unfolded, the cursor and the scroll.
    pub folds: &'a Folds,
    /// The changed files and the directories that hold them.
    pub changes: &'a Changes,
}

/// Draws `pane` in `area`: the heading, muted, then the tree, that it is
/// being read, or why it could not be, in the alert colour. The tree's
/// rows start where `folds` was scrolled, moved just enough to keep the
/// cursor's in view, and the cursor's name is in the accent while
/// `focused`. A cut list says so on the last row. Notes in `targets`
/// where it drew each entry. Returns the scroll for the next frame, with
/// how many rows of the tree there was room for: none while it is read
/// or when it could not be.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane<'_>,
    focused: bool,
    targets: &mut Targets,
) -> (usize, usize) {
    let palette = palette::current();
    if area.height == 0 {
        return (pane.folds.scroll, 0);
    }
    let width = usize::from(area.width);
    let muted = Style::new().fg(palette.muted);
    let [top, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let heading = Line::styled(cut(&printable(pane.heading), width), muted);
    frame.render_widget(Paragraph::new(heading), top);
    match pane.content {
        None => {
            let reading = Line::styled(cut(strings::FILES_READING, width), muted);
            frame.render_widget(Paragraph::new(reading), body);
            (pane.folds.scroll, 0)
        }
        Some(Err(error)) => {
            let text = printable(&strings::files_trouble(error));
            let trouble = Paragraph::new(text)
                .style(Style::new().fg(palette.alert))
                .wrap(Wrap { trim: true });
            frame.render_widget(trouble, body);
            (pane.folds.scroll, 0)
        }
        Some(Ok(tree)) => render_tree(frame, body, tree, pane, focused, targets),
    }
}

/// Draws the rows of `tree` in `area`, as [`render`] says.
fn render_tree(
    frame: &mut Frame,
    area: Rect,
    tree: &Tree,
    pane: &Pane<'_>,
    focused: bool,
    targets: &mut Targets,
) -> (usize, usize) {
    let palette = palette::current();
    let width = usize::from(area.width);
    let muted = Style::new().fg(palette.muted);
    let rows = tree.rows(&pane.folds.open);
    // The note of a cut list takes the last row while another is left.
    let note = tree.cut.filter(|_| area.height > 1);
    let room = usize::from(area.height) - usize::from(note.is_some());
    let scroll = if rows.is_empty() {
        let empty = Line::styled(cut(strings::FILES_EMPTY, width), muted);
        frame.render_widget(Paragraph::new(empty), area);
        0
    } else {
        let cursor = pane.folds.cursor(&rows);
        let scroll = scrolled(pane.folds.scroll, Some(cursor), room, rows.len());
        let lines = rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(room)
            .map(|(index, row)| {
                let line = row_line(row, pane.changes, focused && index == cursor, width);
                let dir = matches!(row.kind, Kind::Dir { .. });
                (line, Some(Target::Entry(row.path.clone(), dir)))
            })
            .collect();
        draw_rows(frame, area, lines, targets);
        scroll
    };
    if let Some(count) = note
        && let Some(last) = area.rows().next_back()
    {
        let text = Line::styled(cut(&strings::files_cut(count), width), muted);
        frame.render_widget(Paragraph::new(text), last);
    }
    (scroll, room)
}

/// One row in `width` columns: two columns of indent per level, the fold
/// marker of a directory, muted, the name cut to the room left, and,
/// flush right, a file's status letter in the colour of its change or
/// the muted mark of a directory that holds changes. The name, and a
/// directory's marker, are in the accent `under_cursor`.
fn row_line(row: &Row, changes: &Changes, under_cursor: bool, width: usize) -> Line<'static> {
    let palette = palette::current();
    let muted = Style::new().fg(palette.muted);
    let indent = "  ".repeat(row.depth);
    let (fold, mark) = match row.kind {
        Kind::Dir { open } => {
            let fold = if open {
                strings::TREE_OPEN
            } else {
                strings::TREE_FOLDED
            };
            let held = changes.dirs.contains(&row.path);
            (
                fold,
                held.then(|| Span::styled(strings::UNCOMMITTED, muted)),
            )
        }
        Kind::File => {
            let letter = changes
                .files
                .get(&row.path)
                .map(|&change| Span::styled(change.letter().to_string(), letter_style(change)));
            (" ", letter)
        }
    };
    let mark = mark.unwrap_or_else(|| Span::raw(" "));
    // The indent, the marker and a space, the name, a space and the mark.
    let room = width.saturating_sub(indent.width() + fold.width() + 1 + 1 + mark.width());
    let name = cut(&printable(&row.name().to_string_lossy()), room);
    let (fold_style, name_style) = if under_cursor {
        (
            Style::new().fg(palette.accent),
            Style::new().fg(palette.accent),
        )
    } else {
        (muted, Style::new())
    };
    Line::from(vec![
        Span::raw(indent),
        Span::styled(fold, fold_style),
        Span::raw(" "),
        Span::styled(pad(&name, room), name_style),
        Span::raw(" "),
        mark,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::status::Entry;
    use crate::ui::changes::Step;
    use crate::ui::palette::{ACCENT, ADDED, ALERT, MODIFIED, MUTED};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};

    /// A tree of `paths`, not cut.
    fn tree(paths: &[&str]) -> Tree {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        Tree::new(&paths, None)
    }

    /// The rows of `tree` with `folds`, as text: indent, marker, name.
    fn shown(tree: &Tree, folds: &Folds) -> Vec<String> {
        tree.rows(&folds.open)
            .iter()
            .map(|row| {
                let marker = match row.kind {
                    Kind::Dir { open: true } => "v ",
                    Kind::Dir { open: false } => "> ",
                    Kind::File => "",
                };
                format!(
                    "{}{marker}{}",
                    "  ".repeat(row.depth),
                    row.name().to_string_lossy()
                )
            })
            .collect()
    }

    /// The path of the row under the cursor.
    fn under(tree: &Tree, folds: &Folds) -> String {
        let rows = tree.rows(&folds.open);
        rows[folds.cursor(&rows)]
            .path
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn directories_come_first_then_files_each_by_name_folded_but_the_top() {
        let tree = tree(&[
            "src/ui/mod.rs",
            "README.md",
            "src/main.rs",
            "Cargo.toml",
            "docs/adr/0001.md",
            "a-b",
            "a/b/c/d/e/deep.txt",
            "./dot/x",
            "../out",
            "/abs",
        ]);
        let mut folds = Folds::default();
        assert_eq!(
            shown(&tree, &folds),
            [
                "> a",
                "> docs",
                "> dot",
                "> src",
                "Cargo.toml",
                "README.md",
                "a-b"
            ],
            "the paths that are not plainly relative are left out"
        );
        // A deep path opens a level at a time; each level keeps its order.
        for _ in 0..4 {
            folds.open(&tree);
            folds.step(&tree, Movement::Line(Step::Down), 1);
        }
        assert_eq!(
            shown(&tree, &folds),
            [
                "v a",
                "  v b",
                "    v c",
                "      v d",
                "        > e",
                "> docs",
                "> dot",
                "> src",
                "Cargo.toml",
                "README.md",
                "a-b",
            ]
        );
        folds.open(&tree);
        assert_eq!(shown(&tree, &folds)[5], "          deep.txt");
        assert_eq!(tree.cut, None);
        assert_eq!(Tree::new(&[PathBuf::from("a")], Some(3)).cut, Some(3));
    }

    #[test]
    fn the_cursor_goes_to_an_entry_pointed_at_while_it_is_listed() {
        let tree = tree(&["src/app/mod.rs", "src/main.rs", "z.txt"]);
        let mut folds = Folds::default();
        folds.point(&tree, Path::new("z.txt"), false);
        assert_eq!(under(&tree, &folds), "z.txt");
        // Not a file of a folded directory, nor an entry of the other
        // kind; the cursor stays.
        folds.point(&tree, Path::new("src/main.rs"), false);
        assert_eq!(under(&tree, &folds), "z.txt");
        folds.point(&tree, Path::new("z.txt"), true);
        assert_eq!(under(&tree, &folds), "z.txt");
        folds.point(&tree, Path::new("src"), false);
        assert_eq!(under(&tree, &folds), "z.txt");
        // A directory, which stays folded.
        folds.point(&tree, Path::new("src"), true);
        assert_eq!(under(&tree, &folds), "src");
        assert_eq!(shown(&tree, &folds), ["> src", "z.txt"]);
        folds.open(&tree);
        folds.point(&tree, Path::new("src/main.rs"), false);
        assert_eq!(under(&tree, &folds), "src/main.rs");
    }

    #[test]
    fn the_cursor_moves_opens_closes_and_goes_up() {
        let tree = tree(&["src/app/mod.rs", "src/main.rs", "z.txt"]);
        let mut folds = Folds::default();
        assert_eq!(under(&tree, &folds), "src", "the first row at first");
        folds.step(&tree, Movement::Line(Step::Up), 1);
        assert_eq!(under(&tree, &folds), "src", "no further than the top");
        // l or Enter opens a directory; on a file nothing happens.
        folds.open(&tree);
        assert_eq!(
            shown(&tree, &folds),
            ["v src", "  > app", "  main.rs", "z.txt"]
        );
        folds.step(&tree, Movement::Line(Step::Down), 1);
        folds.open(&tree);
        folds.step(&tree, Movement::Line(Step::Down), 1);
        assert_eq!(under(&tree, &folds), "src/app/mod.rs");
        let before = shown(&tree, &folds);
        folds.open(&tree);
        assert_eq!(shown(&tree, &folds), before, "a file does not open");
        // h on a file goes to its directory, then folds it, then goes up.
        folds.close(&tree);
        assert_eq!(under(&tree, &folds), "src/app");
        folds.close(&tree);
        assert_eq!(
            shown(&tree, &folds),
            ["v src", "  > app", "  main.rs", "z.txt"]
        );
        assert_eq!(under(&tree, &folds), "src/app");
        folds.close(&tree);
        assert_eq!(under(&tree, &folds), "src");
        folds.close(&tree);
        assert_eq!(shown(&tree, &folds), ["> src", "z.txt"]);
        folds.close(&tree);
        assert_eq!(under(&tree, &folds), "src", "the top level has no parent");
        for _ in 0..3 {
            folds.step(&tree, Movement::Line(Step::Down), 1);
        }
        assert_eq!(under(&tree, &folds), "z.txt", "no further than the end");
        folds.close(&tree);
        assert_eq!(under(&tree, &folds), "z.txt");
        // Opened again, the directory shows its own folds as they were.
        folds.step(&tree, Movement::Line(Step::Up), 1);
        folds.open(&tree);
        assert_eq!(
            shown(&tree, &folds),
            ["v src", "  > app", "  main.rs", "z.txt"]
        );
    }

    #[test]
    fn a_tree_read_again_keeps_the_folds_and_the_cursor() {
        let old = tree(&["a/1", "b/2", "c"]);
        let mut folds = Folds::default();
        folds.step(&old, Movement::Line(Step::Down), 1);
        folds.open(&old);
        assert_eq!(under(&old, &folds), "b");
        // A file added above: the cursor stays on its entry.
        let new = tree(&["a/1", "a0", "b/2", "b/3", "c"]);
        assert_eq!(shown(&new, &folds), ["> a", "v b", "  2", "  3", "a0", "c"]);
        assert_eq!(under(&new, &folds), "b");
        // Its entry gone: the row at its place.
        let gone = tree(&["a/1", "c"]);
        assert_eq!(under(&gone, &folds), "c");
        let empty = tree(&[]);
        assert!(empty.rows(&folds.open).is_empty());
        assert_eq!(folds.cursor(&[]), 0);
        folds.step(&empty, Movement::Line(Step::Down), 1);
        folds.open(&empty);
        folds.close(&empty);
    }

    /// A status entry for `path` with these changes.
    fn entry(path: &str, index: Option<Change>, worktree: Option<Change>) -> Entry {
        Entry {
            path: PathBuf::from(path),
            original: None,
            index,
            worktree,
            submodule: false,
        }
    }

    #[test]
    fn changes_are_the_files_and_the_directories_that_hold_them() {
        let status = Status {
            entries: vec![
                entry("app/src/a.rs", None, Some(Change::Modified)),
                entry("app/src/deep/b.rs", Some(Change::Added), None),
                entry("app/new.txt", Some(Change::Added), Some(Change::Modified)),
                entry("app/gone.txt", Some(Change::Deleted), None),
                entry("other/x.rs", None, Some(Change::Untracked)),
            ],
            ..Status::default()
        };
        let changes = Changes::new(&status, Path::new("app"));
        let files: HashMap<PathBuf, Change> = [
            ("src/a.rs", Change::Modified),
            ("src/deep/b.rs", Change::Added),
            ("new.txt", Change::Modified),
            ("gone.txt", Change::Deleted),
        ]
        .into_iter()
        .map(|(path, change)| (PathBuf::from(path), change))
        .collect();
        assert_eq!(changes.files, files, "the work tree's change first");
        let dirs: HashSet<PathBuf> = ["src", "src/deep"].map(PathBuf::from).into();
        assert_eq!(changes.dirs, dirs, "outside app nothing");
        let whole = Changes::new(&status, Path::new(""));
        assert!(whole.dirs.contains(Path::new("other")));
        assert_eq!(
            whole.files.get(Path::new("other/x.rs")),
            Some(&Change::Untracked)
        );
    }

    /// Draws `pane` in `width` by `height` cells. Returns the terminal and
    /// the scroll for the next frame.
    fn draw(
        width: u16,
        height: u16,
        pane: &Pane<'_>,
        focused: bool,
    ) -> (Terminal<TestBackend>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut scroll = 0;
        terminal
            .draw(|frame| {
                (scroll, _) = render(frame, frame.area(), pane, focused, &mut Targets::default())
            })
            .expect("draw");
        (terminal, scroll)
    }

    /// Every row drawn.
    fn lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    /// The colour of the cell at `x`, `y`, and whether it is bold.
    fn style(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> (Color, bool) {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or((Color::Reset, false), |c| {
                (c.fg, c.modifier.contains(Modifier::BOLD))
            })
    }

    #[test]
    fn draws_the_heading_and_the_rows_with_their_letters_and_marks() {
        let tree = tree(&[
            "src/main.rs",
            "src/ui/mod.rs",
            "Cargo.toml",
            "notes.md",
            "a-very-long-file-name.txt",
        ]);
        let mut folds = Folds::default();
        folds.open(&tree);
        folds.step(&tree, Movement::Line(Step::Down), 1);
        let status = Status {
            entries: vec![
                entry("src/ui/mod.rs", None, Some(Change::Modified)),
                entry("notes.md", None, Some(Change::Untracked)),
            ],
            ..Status::default()
        };
        let changes = Changes::new(&status, Path::new(""));
        let pane = Pane {
            heading: "termis",
            content: Some(Ok(&tree)),
            folds: &folds,
            changes: &changes,
        };
        let (terminal, scroll) = draw(18, 7, &pane, true);
        assert_eq!(
            lines(&terminal),
            [
                "termis            ",
                "▾ src            ●",
                "  ▸ ui           ●",
                "    main.rs       ",
                "  Cargo.toml      ",
                "  a-very-long-f…  ",
                "  notes.md       ?",
            ]
        );
        assert_eq!(scroll, 0);
        // The heading and the fold markers muted, the names in the
        // terminal's colour; the cursor's in the accent and bold.
        assert_eq!(style(&terminal, 0, 0), (MUTED, false));
        assert_eq!(style(&terminal, 0, 1), (MUTED, false));
        assert_eq!(style(&terminal, 2, 1), (Color::Reset, false), "src");
        assert_eq!(
            style(&terminal, 2, 2),
            (ACCENT, false),
            "the cursor's marker"
        );
        assert_eq!(style(&terminal, 4, 2), (ACCENT, false), "ui");
        assert_eq!(
            style(&terminal, 17, 2),
            (MUTED, false),
            "a directory with changes"
        );
        assert_eq!(style(&terminal, 17, 6), (ADDED, false), "untracked");
        assert_eq!(style(&terminal, 2, 6), (Color::Reset, false));
        // Not highlighted, the cursor does not show.
        let (terminal, _) = draw(18, 7, &pane, false);
        assert_eq!(style(&terminal, 4, 2), (Color::Reset, false));
        // A modified file in its colour, once its directory is open.
        folds.open(&tree);
        let pane = Pane {
            heading: "termis",
            content: Some(Ok(&tree)),
            folds: &folds,
            changes: &changes,
        };
        let (terminal, _) = draw(18, 8, &pane, true);
        assert_eq!(lines(&terminal)[2], "  ▾ ui           ●");
        assert_eq!(lines(&terminal)[3], "      mod.rs     M");
        assert_eq!(style(&terminal, 17, 3), (MODIFIED, false));
    }

    #[test]
    fn the_cursor_stays_in_view_and_a_cut_list_says_so_last() {
        let names: Vec<String> = (0..10).map(|n| format!("f{n}")).collect();
        let paths: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        let tree = Tree::new(&paths, Some(10));
        let mut folds = Folds::default();
        let changes = Changes::default();
        // Draws the tree with `folds` in `width` by `height` cells.
        let drawn = |folds: &Folds, width, height| {
            let pane = Pane {
                heading: "p",
                content: Some(Ok(&tree)),
                folds,
                changes: &changes,
            };
            draw(width, height, &pane, true)
        };
        // A heading, four rows and the note.
        let (terminal, scroll) = drawn(&folds, 16, 6);
        assert_eq!(scroll, 0);
        assert_eq!(lines(&terminal)[1], "  f0            ");
        assert_eq!(lines(&terminal)[5], "cut at 10 files ");
        assert_eq!(style(&terminal, 0, 5), (MUTED, false));
        for _ in 0..5 {
            folds.step(&tree, Movement::Line(Step::Down), 1);
        }
        let (terminal, scroll) = drawn(&folds, 16, 6);
        assert_eq!(scroll, 2, "down to the cursor's row");
        assert_eq!(lines(&terminal)[4], "  f5            ");
        folds.scroll = scroll;
        folds.step(&tree, Movement::Line(Step::Up), 1);
        let (_, scroll) = drawn(&folds, 16, 6);
        assert_eq!(scroll, 2, "the view stays while the cursor is in it");
        folds.scroll = scroll;
        for _ in 0..3 {
            folds.step(&tree, Movement::Line(Step::Up), 1);
        }
        let (terminal, scroll) = drawn(&folds, 16, 6);
        assert_eq!(scroll, 1);
        assert_eq!(lines(&terminal)[1], "  f1            ");
        // With two rows the cursor's keeps its place over the note's.
        let (terminal, _) = drawn(&folds, 16, 2);
        assert_eq!(lines(&terminal)[1], "  f1            ");
        for (width, height) in [(0, 0), (1, 1), (3, 2), (4, 8)] {
            drawn(&folds, width, height);
        }
    }

    #[test]
    fn the_cursor_moves_by_half_or_all_the_rows_shown_and_to_the_ends() {
        let names: Vec<String> = (0..10).map(|n| format!("f{n}")).collect();
        let paths: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        let tree = Tree::new(&paths, None);
        let mut folds = Folds::default();
        let changes = Changes::default();
        // A heading and room for four rows.
        let mut terminal = Terminal::new(TestBackend::new(16, 5)).expect("terminal");
        let pane = Pane {
            heading: "p",
            content: Some(Ok(&tree)),
            folds: &folds,
            changes: &changes,
        };
        let mut drawn = (9, 9);
        terminal
            .draw(|frame| drawn = render(frame, frame.area(), &pane, true, &mut Targets::default()))
            .expect("draw");
        assert_eq!(drawn, (0, 4));
        let (up, down) = (Step::Up, Step::Down);
        folds.step(&tree, Movement::Half(down), 4);
        assert_eq!(under(&tree, &folds), "f2");
        folds.step(&tree, Movement::Page(down), 4);
        assert_eq!(under(&tree, &folds), "f6");
        folds.step(&tree, Movement::Page(down), 4);
        assert_eq!(under(&tree, &folds), "f9", "no further than the last");
        folds.step(&tree, Movement::Half(up), 4);
        assert_eq!(under(&tree, &folds), "f7");
        folds.step(&tree, Movement::Top, 4);
        assert_eq!(under(&tree, &folds), "f0");
        folds.step(&tree, Movement::End, 4);
        assert_eq!(under(&tree, &folds), "f9");
        // The cursor's row comes into view at the bottom.
        let pane = Pane {
            heading: "p",
            content: Some(Ok(&tree)),
            folds: &folds,
            changes: &changes,
        };
        let (terminal, scroll) = draw(16, 5, &pane, true);
        assert_eq!(scroll, 6);
        assert_eq!(lines(&terminal)[4], "  f9            ");
        // Nothing to move through before there are rows.
        let empty = Tree::new(&[], None);
        folds.step(&empty, Movement::End, 4);
        assert_eq!(under(&tree, &folds), "f9");
    }

    #[test]
    fn says_while_it_reads_when_it_cannot_and_when_there_is_nothing() {
        let folds = Folds::default();
        let changes = Changes::default();
        let reading = Pane {
            heading: "termis",
            content: None,
            folds: &folds,
            changes: &changes,
        };
        let (terminal, _) = draw(18, 3, &reading, true);
        assert_eq!(lines(&terminal)[1], "reading files     ");
        assert_eq!(style(&terminal, 0, 1), (MUTED, false));
        let failed = Pane {
            content: Some(Err("git ls-files: fatal: bad")),
            ..reading
        };
        let (terminal, _) = draw(18, 4, &failed, true);
        assert_eq!(
            lines(&terminal)[1..],
            [
                "files unavailable:",
                "git ls-files:     ",
                "fatal: bad        "
            ],
            "the reason wraps"
        );
        assert_eq!(style(&terminal, 0, 1), (ALERT, false));
        let empty = tree(&[]);
        let nothing = Pane {
            content: Some(Ok(&empty)),
            ..reading
        };
        let (terminal, _) = draw(18, 3, &nothing, true);
        assert_eq!(lines(&terminal)[1], "no files          ");
        // A cut list says so even with nothing to show.
        let none_kept = Tree::new(&[], Some(20_000));
        let cut = Pane {
            content: Some(Ok(&none_kept)),
            ..reading
        };
        let (terminal, _) = draw(18, 3, &cut, true);
        assert_eq!(
            lines(&terminal)[1..],
            ["no files          ", "cut at 20000 files"]
        );
    }
}
