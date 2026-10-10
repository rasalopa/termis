//! The changes pane: staged and unstaged files with their line counts.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::commit::{self, Draft};
use super::{ACCENT, DIM, render_empty};
use crate::git::numstat::Lines;
use crate::git::status::{Change, Entry};
use crate::git::worker::Snapshot;
use crate::strings;

/// The two lists of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
}

/// One file in one of the lists.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row<'a> {
    section: Section,
    entry: &'a Entry,
    /// The lines on the list's side: the index for staged, the work tree
    /// for unstaged; none while not counted.
    lines: Option<Lines>,
}

impl Row<'_> {
    /// The change on the list's side.
    fn change(&self) -> Option<Change> {
        match self.section {
            Section::Staged => self.entry.index,
            Section::Unstaged => self.entry.worktree,
        }
    }
}

/// The rows of the pane: the staged list, then the unstaged one, each in
/// git's order.
fn rows(snapshot: &Snapshot) -> Vec<Row<'_>> {
    let staged = snapshot.status.staged().map(|entry| Row {
        section: Section::Staged,
        entry,
        lines: snapshot.index.get(&entry.path).copied(),
    });
    let unstaged = snapshot.status.unstaged().map(|entry| Row {
        section: Section::Unstaged,
        entry,
        lines: snapshot.worktree.get(&entry.path).copied(),
    });
    staged.chain(unstaged).collect()
}

/// The selected row, kept by what it is rather than where it is, so a
/// refresh that reorders the lists keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub section: Section,
    pub path: PathBuf,
}

impl Selected {
    fn of(row: &Row<'_>) -> Self {
        Self {
            section: row.section,
            path: row.entry.path.clone(),
        }
    }

    fn matches(&self, row: &Row<'_>) -> bool {
        self.section == row.section && self.path == row.entry.path
    }
}

/// Where `selected` is among `rows`.
fn index_of(rows: &[Row<'_>], selected: &Selected) -> Option<usize> {
    rows.iter().position(|row| selected.matches(row))
}

/// A step of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
}

/// The selection after `step` from `selected` over the rows of
/// `snapshot`: the first row when nothing was selected, and no further
/// than the ends.
pub fn moved(snapshot: &Snapshot, selected: Option<&Selected>, step: Step) -> Option<Selected> {
    let rows = rows(snapshot);
    let index = match (selected.and_then(|s| index_of(&rows, s)), step) {
        (None, _) => 0,
        (Some(index), Step::Down) => (index + 1).min(rows.len().saturating_sub(1)),
        (Some(index), Step::Up) => index.saturating_sub(1),
    };
    rows.get(index).map(Selected::of)
}

/// The selection once `snapshot` replaced `previous`: the same row when
/// it is still listed, else the row now at its position in its list, so
/// that staging file after file walks down the unstaged list, else the
/// first row.
pub fn reselect(
    previous: Option<&Snapshot>,
    snapshot: &Snapshot,
    selected: Option<&Selected>,
) -> Option<Selected> {
    let rows = rows(snapshot);
    let Some(selected) = selected else {
        return rows.first().map(Selected::of);
    };
    if index_of(&rows, selected).is_some() {
        return Some(selected.clone());
    }
    let position = previous.map_or(0, |previous| {
        self::rows(previous)
            .iter()
            .filter(|row| row.section == selected.section)
            .position(|row| selected.matches(row))
            .unwrap_or(0)
    });
    let in_list: Vec<&Row<'_>> = rows
        .iter()
        .filter(|row| row.section == selected.section)
        .collect();
    in_list
        .get(position.min(in_list.len().saturating_sub(1)))
        .copied()
        .or(rows.first())
        .map(Selected::of)
}

/// Draws the lists in `area`, with the commit box under them while
/// `draft` is open: the totals, then each section with its rows. The
/// selected row carries a marker and, while `focused`, the accent. The
/// lists start `scroll` lines down, moved just enough to keep the
/// selected row on screen; the offset used is returned for the next
/// frame, with where the box wants the cursor.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    selected: Option<&Selected>,
    focused: bool,
    scroll: usize,
    draft: Option<&Draft>,
) -> (usize, Option<Position>) {
    let (area, cursor) = match draft {
        Some(draft) => {
            let wanted = commit::height(draft).min(area.height);
            let [lists, boxed] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(wanted)]).areas(area);
            (lists, commit::render(frame, boxed, draft))
        }
        None => (area, None),
    };
    (
        render_lists(frame, area, snapshot, selected, focused, scroll),
        cursor,
    )
}

/// Draws the lists alone; see [`render`].
fn render_lists(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    selected: Option<&Selected>,
    focused: bool,
    scroll: usize,
) -> usize {
    if area.height == 0 || area.width == 0 {
        return scroll;
    }
    let rows = rows(snapshot);
    if rows.is_empty() {
        render_empty(frame, area, strings::NO_CHANGES);
        return 0;
    }
    let staged = rows
        .iter()
        .filter(|row| row.section == Section::Staged)
        .count();
    let unstaged = rows.len() - staged;
    // The header, the staged title and rows, the unstaged title and rows.
    let line_of_row = |index: usize| if index < staged { 2 + index } else { 3 + index };
    let total = 3 + rows.len();
    let selected_index = selected.and_then(|s| index_of(&rows, s));
    let height = usize::from(area.height);
    let scroll = scrolled(scroll, selected_index.map(line_of_row), height, total);
    let width = usize::from(area.width);
    let lines: Vec<Line> = (scroll..(scroll + height).min(total))
        .map(|line| match line {
            0 => header(&rows, snapshot.status.entries.len(), width),
            1 => title(strings::changes_staged(staged)),
            _ if line == 2 + staged => title(strings::changes_unstaged(unstaged)),
            _ => {
                let index = if line < 2 + staged {
                    line - 2
                } else {
                    line - 3
                };
                let is_selected = selected_index == Some(index);
                row_line(&rows[index], width, is_selected, focused)
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    scroll
}

/// The offset to draw from: `current` kept within the `total` lines, then
/// moved just enough to show the `selected` line in `height` lines.
fn scrolled(current: usize, selected: Option<usize>, height: usize, total: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let mut offset = current.min(total.saturating_sub(height));
    if let Some(line) = selected {
        if line < offset {
            offset = line;
        } else if line >= offset + height {
            offset = line + 1 - height;
        }
    }
    offset
}

fn title(text: String) -> Line<'static> {
    Line::styled(text, Style::new().fg(DIM))
}

/// The first line: how many paths changed, and the lines over all rows
/// flush right.
fn header(rows: &[Row<'_>], files: usize, width: usize) -> Line<'static> {
    let (mut added, mut deleted) = (0, 0);
    for row in rows {
        if let Some(Lines::Text {
            added: more,
            deleted: fewer,
        }) = row.lines
        {
            added += more;
            deleted += fewer;
        }
    }
    let left = strings::changes_files(files);
    let right = strings::lines(Lines::Text { added, deleted }, false);
    Line::from(vec![
        Span::raw(pad(&left, width.saturating_sub(right.width() + 1))),
        Span::raw(right),
        Span::raw(" "),
    ])
}

/// One row: the marker, the change letter, the path cut to the room
/// left, and the lines flush right.
fn row_line(row: &Row<'_>, width: usize, selected: bool, focused: bool) -> Line<'static> {
    let marker = if selected {
        strings::SELECTED_MARKER
    } else {
        strings::UNSELECTED_MARKER
    };
    let letter = row.change().map_or(' ', Change::letter);
    let untracked = row.change() == Some(Change::Untracked);
    let counts = row
        .lines
        .map_or_else(String::new, |lines| strings::lines(lines, untracked));
    // Marker, letter, a space, the path, two spaces, the counts, a margin.
    let fixed = marker.width() + 2 + 2 + counts.width() + 1;
    let room = width.saturating_sub(fixed);
    let path = printable(&row.entry.path.to_string_lossy());
    let path = cut(&path, room);
    let style = if selected && focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(ACCENT)),
        Span::styled(format!("{letter} {}", pad(&path, room)), style),
        Span::styled(format!("  {counts} "), style),
    ])
}

/// `text` with control characters, which the screen would drop, shown
/// as the replacement character so the columns stay put.
pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { '\u{FFFD}' } else { ch })
        .collect()
}

/// `text` padded with spaces to `width` columns.
fn pad(text: &str, width: usize) -> String {
    let mut padded = text.to_owned();
    padded.extend(std::iter::repeat_n(' ', width.saturating_sub(text.width())));
    padded
}

/// `text` cut to `width` columns, ending in `…` when it did not fit;
/// whole graphemes only.
pub(crate) fn cut(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut cut = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let next = used + grapheme.width();
        if next + 1 > width {
            break;
        }
        cut.push_str(grapheme);
        used = next;
    }
    if width > 0 {
        cut.push('…');
    }
    cut
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::git::status::{Branch, Status};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use std::collections::HashMap;

    fn entry(path: &str, index: Option<Change>, worktree: Option<Change>) -> Entry {
        Entry {
            path: PathBuf::from(path),
            original: None,
            index,
            worktree,
            submodule: false,
        }
    }

    fn text(added: u32, deleted: u32) -> Lines {
        Lines::Text { added, deleted }
    }

    /// Two staged, three unstaged (one of them staged too), one not
    /// counted yet.
    pub(crate) fn sample() -> Snapshot {
        Snapshot {
            status: Status {
                branch: Branch {
                    head: Some("main".into()),
                    ..Branch::default()
                },
                entries: vec![
                    entry("src/app.rs", Some(Change::Modified), Some(Change::Modified)),
                    entry("src/ui/layout.rs", Some(Change::Added), None),
                    entry("README.md", None, Some(Change::Deleted)),
                    entry("new.txt", None, Some(Change::Untracked)),
                ],
            },
            index: HashMap::from([
                (PathBuf::from("src/app.rs"), text(31, 4)),
                (PathBuf::from("src/ui/layout.rs"), text(9, 0)),
            ]),
            worktree: HashMap::from([
                (PathBuf::from("src/app.rs"), text(61, 4)),
                (PathBuf::from("README.md"), text(0, 12)),
            ]),
            ..Snapshot::default()
        }
    }

    fn selected(section: Section, path: &str) -> Selected {
        Selected {
            section,
            path: PathBuf::from(path),
        }
    }

    fn draw(
        width: u16,
        height: u16,
        snapshot: &Snapshot,
        selected: Option<&Selected>,
        focused: bool,
        scroll: usize,
    ) -> (Terminal<TestBackend>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut offset = 0;
        terminal
            .draw(|frame| {
                (offset, _) = render(
                    frame,
                    frame.area(),
                    snapshot,
                    selected,
                    focused,
                    scroll,
                    None,
                );
            })
            .expect("draw");
        (terminal, offset)
    }

    #[test]
    fn the_commit_box_takes_the_bottom_of_the_pane() {
        let snapshot = sample();
        let first = selected(Section::Staged, "src/app.rs");
        let mut draft = Draft::default();
        draft.apply(&commit::Edit::Insert("feat: x".into()));
        let mut terminal = Terminal::new(TestBackend::new(23, 10)).expect("terminal");
        let mut result = (0, None);
        terminal
            .draw(|frame| {
                result = render(
                    frame,
                    frame.area(),
                    &snapshot,
                    Some(&first),
                    true,
                    0,
                    Some(&draft),
                );
            })
            .expect("draw");
        assert!(row(&terminal, 0).starts_with("4 files"));
        assert!(
            row(&terminal, 6).starts_with("commit"),
            "{}",
            row(&terminal, 6)
        );
        assert!(row(&terminal, 7).starts_with('┌'));
        assert!(
            row(&terminal, 8).starts_with("│feat: x"),
            "{}",
            row(&terminal, 8)
        );
        assert!(row(&terminal, 9).starts_with('└'));
        assert_eq!(result, (0, Some(Position { x: 8, y: 8 })));
        // Too short for both: the box wins what there is.
        let mut terminal = Terminal::new(TestBackend::new(23, 3)).expect("terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &snapshot,
                    Some(&first),
                    true,
                    0,
                    Some(&draft),
                );
            })
            .expect("draw");
        assert!(row(&terminal, 0).starts_with("commit"));
    }

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    fn fg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.fg)
    }

    #[test]
    fn rows_list_staged_then_unstaged_with_their_side_of_the_lines() {
        let snapshot = sample();
        let rows = rows(&snapshot);
        let summary: Vec<_> = rows
            .iter()
            .map(|row| (row.section, row.entry.path.to_str().unwrap(), row.lines))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Section::Staged, "src/app.rs", Some(text(31, 4))),
                (Section::Staged, "src/ui/layout.rs", Some(text(9, 0))),
                (Section::Unstaged, "src/app.rs", Some(text(61, 4))),
                (Section::Unstaged, "README.md", Some(text(0, 12))),
                (Section::Unstaged, "new.txt", None),
            ]
        );
    }

    #[test]
    fn selection_moves_within_the_rows_and_starts_at_the_first() {
        let snapshot = sample();
        let first = moved(&snapshot, None, Step::Down);
        assert_eq!(first, Some(selected(Section::Staged, "src/app.rs")));
        let second = moved(&snapshot, first.as_ref(), Step::Down);
        assert_eq!(second, Some(selected(Section::Staged, "src/ui/layout.rs")));
        let third = moved(&snapshot, second.as_ref(), Step::Down);
        assert_eq!(third, Some(selected(Section::Unstaged, "src/app.rs")));
        assert_eq!(moved(&snapshot, second.as_ref(), Step::Up), first);
        assert_eq!(
            moved(&snapshot, first.as_ref(), Step::Up),
            first,
            "stays at the top"
        );
        let last = selected(Section::Unstaged, "new.txt");
        assert_eq!(
            moved(&snapshot, Some(&last), Step::Down),
            Some(last.clone()),
            "stays at the end"
        );
        assert_eq!(moved(&Snapshot::default(), None, Step::Down), None);
        assert_eq!(moved(&Snapshot::default(), Some(&last), Step::Up), None);
    }

    #[test]
    fn reselect_keeps_the_row_or_its_position() {
        let before = sample();
        let kept = selected(Section::Unstaged, "README.md");
        assert_eq!(
            reselect(None, &before, None),
            Some(selected(Section::Staged, "src/app.rs")),
            "first by default"
        );
        assert_eq!(reselect(None, &before, Some(&kept)), Some(kept.clone()));

        // README.md gets staged: its unstaged row is gone, the row now at
        // its position is selected.
        let mut after = sample();
        after.status.entries[2] = entry("README.md", Some(Change::Deleted), None);
        assert_eq!(
            reselect(Some(&before), &after, Some(&kept)),
            Some(selected(Section::Unstaged, "new.txt"))
        );
        // A list reordered by git keeps the row, not the position.
        let mut reordered = sample();
        reordered.status.entries.swap(2, 3);
        assert_eq!(
            reselect(Some(&before), &reordered, Some(&kept)),
            Some(kept.clone())
        );
        // Its list emptied: the first row of the other one.
        let mut all_staged = sample();
        for entry in &mut all_staged.status.entries {
            entry.index = entry.index.or(Some(Change::Added));
            entry.worktree = None;
        }
        assert_eq!(
            reselect(Some(&before), &all_staged, Some(&kept)),
            Some(selected(Section::Staged, "src/app.rs"))
        );
        // Everything committed: nothing to select.
        assert_eq!(
            reselect(Some(&before), &Snapshot::default(), Some(&kept)),
            None
        );
        // A row gone with no previous snapshot: the first of its list.
        let gone = selected(Section::Unstaged, "missing");
        assert_eq!(
            reselect(None, &before, Some(&gone)),
            Some(selected(Section::Unstaged, "src/app.rs"))
        );
    }

    #[test]
    fn renders_the_totals_the_sections_and_the_rows() {
        let snapshot = sample();
        let last = selected(Section::Unstaged, "new.txt");
        let (terminal, offset) = draw(23, 8, &snapshot, Some(&last), true, 0);
        assert_eq!(offset, 0);
        let rows: Vec<String> = (0..8).map(|y| row(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "4 files       +101 −20 ",
                "staged (2)             ",
                "  M src/app.rs  +31 −4 ",
                "  A src/ui/lay…  +9 −0 ",
                "unstaged (3)           ",
                "  M src/app.rs  +61 −4 ",
                "  D README.md   +0 −12 ",
                "▸ ? new.txt            ",
            ]
        );
        assert_eq!(fg(&terminal, 0, 7), ACCENT, "marker");
        assert_eq!(fg(&terminal, 2, 7), ACCENT, "selected row, focused");
        assert_eq!(fg(&terminal, 2, 6), Color::Reset, "other rows");
        assert_eq!(fg(&terminal, 0, 1), DIM, "section title");

        let (terminal, _) = draw(23, 8, &snapshot, Some(&last), false, 0);
        assert_eq!(fg(&terminal, 0, 7), ACCENT, "marker stays");
        assert_eq!(
            fg(&terminal, 2, 7),
            Color::Reset,
            "row plain when unfocused"
        );
        assert!(row(&terminal, 7).starts_with("▸ ? new.txt"));
    }

    #[test]
    fn renders_binaries_wide_paths_and_control_characters() {
        let mut snapshot = sample();
        snapshot.status.entries = vec![
            entry("img.png", Some(Change::Added), None),
            entry("日本語/道.txt", None, Some(Change::Modified)),
            entry("tab\there", None, Some(Change::Untracked)),
        ];
        snapshot.index = HashMap::from([(PathBuf::from("img.png"), Lines::Binary)]);
        snapshot.worktree = HashMap::from([(PathBuf::from("日本語/道.txt"), text(1, 1))]);
        let (terminal, _) = draw(20, 6, &snapshot, None, false, 0);
        assert_eq!(row(&terminal, 2), "  A img.png     bin ");
        // Each wide character takes two cells; the counts stay flush right.
        let cell = |x: u16| {
            terminal
                .backend()
                .buffer()
                .cell((x, 4))
                .map_or("", |c| c.symbol())
                .to_owned()
        };
        let cells: String = [4, 6, 8, 10, 11].into_iter().map(cell).collect();
        assert_eq!(cells, "日本語/…");
        assert!(
            row(&terminal, 4).ends_with("  +1 −1 "),
            "{}",
            row(&terminal, 4)
        );
        let tab = row(&terminal, 5);
        assert!(tab.starts_with("  ? tab\u{FFFD}here "), "{tab}");
        assert_eq!(tab.chars().count(), 20);
    }

    #[test]
    fn scrolls_just_enough_to_keep_the_selection_on_screen() {
        assert_eq!(scrolled(0, None, 4, 8), 0);
        assert_eq!(scrolled(0, Some(7), 4, 8), 4, "down to show the last line");
        assert_eq!(
            scrolled(4, Some(5), 4, 8),
            4,
            "already on screen: unchanged"
        );
        assert_eq!(scrolled(4, Some(2), 4, 8), 2, "up to show the line");
        assert_eq!(
            scrolled(9, Some(3), 4, 8),
            3,
            "an offset past the end is pulled back"
        );
        assert_eq!(scrolled(9, None, 4, 8), 4);
        assert_eq!(scrolled(3, Some(5), 0, 8), 0);
        assert_eq!(scrolled(3, Some(5), 1, 8), 5);

        let snapshot = sample();
        let last = selected(Section::Unstaged, "new.txt");
        let (terminal, offset) = draw(23, 4, &snapshot, Some(&last), true, 0);
        assert_eq!(offset, 4);
        assert!(row(&terminal, 0).starts_with("unstaged (3)"));
        assert!(row(&terminal, 3).starts_with("▸ ? new.txt"));
        // Moving up within the window keeps the offset.
        let readme = selected(Section::Unstaged, "README.md");
        let (terminal, offset) = draw(23, 4, &snapshot, Some(&readme), true, 4);
        assert_eq!(offset, 4);
        assert!(row(&terminal, 2).starts_with("▸ D README.md"));
        let (terminal, offset) = draw(23, 1, &snapshot, Some(&readme), true, 0);
        assert_eq!(offset, 6);
        assert!(
            row(&terminal, 0).starts_with("▸ D README.md"),
            "one line: the selection"
        );
        let (terminal, offset) = draw(23, 4, &snapshot, None, true, 0);
        assert_eq!(offset, 0);
        assert!(
            row(&terminal, 0).starts_with("4 files"),
            "no selection: the top"
        );
    }

    #[test]
    fn says_no_changes_and_survives_tiny_areas() {
        let (terminal, offset) = draw(23, 5, &Snapshot::default(), None, false, 3);
        assert_eq!(offset, 0);
        let all: String = (0..5).map(|y| row(&terminal, y)).collect();
        assert!(all.contains("no changes"), "{all}");
        let snapshot = sample();
        let last = selected(Section::Unstaged, "new.txt");
        for (w, h) in [(0, 0), (1, 1), (3, 2), (5, 1), (8, 3)] {
            draw(w, h, &snapshot, Some(&last), true, 2);
            draw(w, h, &Snapshot::default(), None, true, 2);
        }
    }

    #[test]
    fn cut_and_pad_count_columns() {
        assert_eq!(cut("src/ui/changes.rs", 10), "src/ui/ch…");
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("exact-fit!", 10), "exact-fit!");
        assert_eq!(cut("日本語の道", 5), "日本…");
        assert_eq!(cut("abc", 1), "…");
        assert_eq!(cut("abc", 0), "");
        assert_eq!(
            cut("ae\u{301}bc", 3),
            "ae\u{301}…",
            "a grapheme stays whole"
        );
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("abcdef", 4), "abcdef");
        assert_eq!(pad("日本", 5), "日本 ");
        assert_eq!(printable("a\tb\u{7f}"), "a\u{FFFD}b\u{FFFD}");
    }
}
