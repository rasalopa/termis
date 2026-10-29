//! The changes pane: staged and unstaged files with their line counts,
//! what was committed today and the last commit.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::commit;
use super::palette;
use super::{Target, Targets, draw_rows, heading, lines_added, lines_removed, render_empty_line};
use crate::git::numstat::Lines;
use crate::git::status::{Change, Entry};
use crate::git::worker::Snapshot;
use crate::strings;

/// The least rows of the today section: its heading and its line. The
/// last commit's two rows come when there is room.
const TODAY_ROWS: u16 = 2;
/// The least rows the lists keep next to the today section: the totals,
/// a blank row, a title and a row.
const MIN_LIST_ROWS: u16 = 4;

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

/// A move through a list or a view: a row or a line, half of what shows
/// at once or all of it, up or down; or to an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    Line(Step),
    Half(Step),
    Page(Step),
    /// To the top: a list's first row, the first line of a file or a
    /// diff, the oldest line a screen's history keeps.
    Top,
    /// To the end: a list's last row, the last line of a file or a diff
    /// at the bottom, a screen's live view, where new output appears.
    End,
}

impl Movement {
    /// Which way it moves and how many rows or lines, through a list or a
    /// view that shows `page` of them at once: one, half the page or all
    /// of it, at least one; none for a move to an end.
    pub fn by(self, page: usize) -> Option<(Step, usize)> {
        match self {
            Self::Line(step) => Some((step, 1)),
            Self::Half(step) => Some((step, (page / 2).max(1))),
            Self::Page(step) => Some((step, page.max(1))),
            Self::Top | Self::End => None,
        }
    }

    /// The index it leads to from `index` among `len`, `page` of which
    /// show at once: no further than the first and the last.
    pub fn index(self, index: usize, len: usize, page: usize) -> usize {
        let last = len.saturating_sub(1);
        let to = match self.by(page) {
            Some((Step::Up, rows)) => index.saturating_sub(rows),
            Some((Step::Down, rows)) => index.saturating_add(rows),
            None if self == Self::Top => 0,
            None => last,
        };
        to.min(last)
    }

    /// The first line on screen once it moved a view from `offset`
    /// through `lines`, `page` of which show at once: no further than the
    /// first line at the top and the last at the bottom.
    pub fn offset(self, offset: usize, lines: usize, page: usize) -> usize {
        let last = lines.saturating_sub(page);
        self.index(offset.min(last), last + 1, page)
    }
}

/// The selection after `movement` from `selected` over the rows of
/// `snapshot`, in lists that show `page` lines at once: the first row
/// when nothing was selected, and no further than the ends.
pub fn moved(
    snapshot: &Snapshot,
    selected: Option<&Selected>,
    movement: Movement,
    page: usize,
) -> Option<Selected> {
    let rows = rows(snapshot);
    let lines = row_lines(&rows);
    let index = selected
        .and_then(|s| index_of(&rows, s))
        .map_or(0, |index| moved_index(&lines, index, movement, page));
    rows.get(index).map(Selected::of)
}

/// The row `movement` leads to from the row at `index`, the rows drawn
/// on `lines` and `page` lines shown at once. A move counts lines, with
/// the blank line and the title between the lists, so that paging shows
/// every row: down, it goes to the last row at or before the line it
/// reaches; up, to the first at or after it. A row at least, and no
/// further than the ends.
fn moved_index(lines: &[usize], index: usize, movement: Movement, page: usize) -> usize {
    let (Some(&line), Some((step, by))) = (lines.get(index), movement.by(page)) else {
        return movement.index(index, lines.len(), page);
    };
    let last = lines.len().saturating_sub(1);
    match step {
        Step::Down => {
            let to = line.saturating_add(by);
            let reached = lines.partition_point(|&row| row <= to).saturating_sub(1);
            reached.max(index + 1).min(last)
        }
        Step::Up => {
            let to = line.saturating_sub(by);
            let reached = lines.partition_point(|&row| row < to);
            reached.min(index.saturating_sub(1))
        }
    }
}

/// Whether the lists of `snapshot` have the file `selected`.
pub fn lists(snapshot: &Snapshot, selected: &Selected) -> bool {
    index_of(&rows(snapshot), selected).is_some()
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

/// How the lists are drawn.
#[derive(Clone, Copy, Debug)]
pub struct Lists<'a> {
    /// The row that carries the marker.
    pub selected: Option<&'a Selected>,
    /// Whether the pane has focus, which gives the selected row the
    /// accent.
    pub focused: bool,
    /// The line the lists start at, moved just enough to keep the
    /// selected row on screen.
    pub scroll: usize,
}

/// Draws the lists in `area`, with the today section under them when
/// there is room for both, its last commit as old as it is at `now`, and
/// the commit box under those while it is `open`: the totals, then each
/// section with its rows, drawn as `lists` says, noting in `targets`
/// where it drew each file. Returns the lists' offset for the next frame
/// and how many lines they had room for, with where the box wants the
/// cursor.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    lists: Lists<'_>,
    open: Option<commit::Open<'_>>,
    now: i64,
    targets: &mut Targets,
) -> (usize, usize, Option<Position>) {
    let (area, cursor) = match open {
        Some(open) => {
            let wanted = commit::height(open.draft).min(area.height);
            let [lists, boxed] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(wanted)]).areas(area);
            (lists, commit::render(frame, boxed, open))
        }
        None => (area, None),
    };
    // The section's last rows give way first, down to its title and line.
    let room = area.height.saturating_sub(MIN_LIST_ROWS);
    let area = if room >= TODAY_ROWS {
        let today = today_lines(snapshot, now, usize::from(area.width));
        let rows = u16::try_from(today.len()).unwrap_or(u16::MAX).min(room);
        let [lists, section] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(rows)]).areas(area);
        frame.render_widget(Paragraph::new(today), section);
        lists
    } else {
        area
    };
    let height = usize::from(area.height);
    let scroll = render_lists(frame, area, snapshot, lists, targets);
    (scroll, height, cursor)
}

/// The age of the last commit at `now`, while the snapshot has one. The
/// app watches it to redraw when it changes.
pub fn last_age(snapshot: Option<&Snapshot>, now: i64) -> Option<String> {
    let last = snapshot?.last.as_ref().ok()?.as_ref()?;
    Some(strings::age(now.saturating_sub(last.time)))
}

/// The today section: its heading, the day's commits with their lines
/// flush right, that there are none, or why they could not be read; then,
/// while the repository has one, the last commit at `now`, its hash muted
/// and giving way to its age flush right, and its subject.
fn today_lines(snapshot: &Snapshot, now: i64, width: usize) -> Vec<Line<'static>> {
    let palette = palette::current();
    let mut lines = vec![heading(strings::TODAY_SECTION, width)];
    lines.push(match &snapshot.today {
        Ok(today) if today.commits > 0 => spread(
            vec![Span::raw(strings::today_commits(today.commits))],
            vec![
                lines_added(today.added.into()),
                Span::raw(" "),
                lines_removed(today.deleted.into()),
            ],
            width,
        ),
        Ok(today) => Line::styled(
            cut(&strings::today_commits(today.commits), width),
            Style::new().fg(palette.muted),
        ),
        Err(error) => Line::styled(
            cut(&printable(error), width),
            Style::new().fg(palette.alert),
        ),
    });
    let muted = Style::new().fg(palette.muted);
    let label = Span::styled(format!("{} ", strings::LAST_COMMIT), muted);
    match &snapshot.last {
        Ok(Some(last)) => {
            let age = strings::age(now.saturating_sub(last.time));
            lines.push(spread(
                vec![label, Span::styled(last.hash.clone(), muted)],
                vec![Span::styled(age, muted)],
                width,
            ));
            lines.push(Line::raw(cut(&printable(&last.subject), width)));
        }
        Ok(None) => {}
        Err(_) => lines.push(Line::from(vec![
            label,
            Span::styled(strings::UNREAD, Style::new().fg(palette.alert)),
        ])),
    }
    lines
}

/// Draws the lists alone; see [`render`].
fn render_lists(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    lists: Lists<'_>,
    targets: &mut Targets,
) -> usize {
    let palette = palette::current();
    let Lists {
        selected,
        focused,
        scroll,
    } = lists;
    if area.height == 0 || area.width == 0 {
        return scroll;
    }
    let rows = rows(snapshot);
    if rows.is_empty() {
        let line = Line::from(vec![
            Span::styled(strings::NO_CHANGES_MARK, Style::new().fg(palette.added)),
            Span::raw(" "),
            Span::styled(strings::NO_CHANGES, Style::new().fg(palette.muted)),
        ]);
        render_empty_line(frame, area, line);
        return 0;
    }
    let items = items(&rows);
    let selected_index = selected.and_then(|s| index_of(&rows, s));
    let selected_line =
        selected_index.and_then(|index| items.iter().position(|item| *item == Item::Row(index)));
    let height = usize::from(area.height);
    let scroll = scrolled(scroll, selected_line, height, items.len());
    let width = usize::from(area.width);
    let lines = items
        .iter()
        .skip(scroll)
        .take(height)
        .map(|item| match *item {
            Item::Header => (header(&rows, snapshot.status.entries.len(), width), None),
            Item::Gap => (Line::raw(""), None),
            Item::Title(name, count) => {
                let muted = Style::new().fg(palette.muted);
                let line = spread(
                    vec![Span::styled(name, muted)],
                    vec![Span::styled(count.to_string(), muted)],
                    width,
                );
                (line, None)
            }
            Item::Row(index) => {
                let is_selected = selected_index == Some(index);
                let line = row_line(&rows[index], width, is_selected, focused);
                (line, Some(Target::Change(Selected::of(&rows[index]))))
            }
        })
        .collect();
    draw_rows(frame, area, lines, targets);
    scroll
}

/// A line of the lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// How many paths changed, and their lines.
    Header,
    /// The blank line before a list.
    Gap,
    /// A list's title and length.
    Title(&'static str, usize),
    /// The row at this index among the rows of both lists.
    Row(usize),
}

/// The lines of the lists over `rows`: the header, then each list after
/// a blank line, its title and its rows.
fn items(rows: &[Row<'_>]) -> Vec<Item> {
    let staged = rows
        .iter()
        .filter(|row| row.section == Section::Staged)
        .count();
    let mut items = vec![
        Item::Header,
        Item::Gap,
        Item::Title(strings::CHANGES_STAGED, staged),
    ];
    items.extend((0..staged).map(Item::Row));
    items.extend([
        Item::Gap,
        Item::Title(strings::CHANGES_UNSTAGED, rows.len() - staged),
    ]);
    items.extend((staged..rows.len()).map(Item::Row));
    items
}

/// The line each of `rows` is drawn on, in order.
fn row_lines(rows: &[Row<'_>]) -> Vec<usize> {
    items(rows)
        .iter()
        .enumerate()
        .filter_map(|(line, item)| matches!(item, Item::Row(_)).then_some(line))
        .collect()
}

/// The offset to draw from: `current` kept within the `total` lines, then
/// moved just enough to show the `selected` line in `height` lines.
pub(super) fn scrolled(
    current: usize,
    selected: Option<usize>,
    height: usize,
    total: usize,
) -> usize {
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

/// `left` and `right` on one line of `width` columns: `right` flush right,
/// `left` cut to the room it leaves before it, a column at least.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let right_width: usize = right.iter().map(Span::width).sum();
    let mut room = width.saturating_sub(right_width + 1);
    let mut spans = Vec::with_capacity(left.len() + right.len() + 1);
    for span in left {
        let text = cut(&span.content, room);
        room = room.saturating_sub(text.width());
        spans.push(Span::styled(text, span.style));
    }
    spans.push(Span::raw(" ".repeat(room + 1)));
    spans.extend(right);
    Line::from(spans)
}

/// The lines of a row as spans: the lines added, and removed unless the
/// file is `untracked`, each in its colour; `bin` muted when git did not
/// count them; nothing while they are not counted.
fn counts(lines: Option<Lines>, untracked: bool) -> Vec<Span<'static>> {
    let palette = palette::current();
    match lines {
        None => Vec::new(),
        Some(Lines::Binary) => vec![Span::styled(
            strings::BINARY,
            Style::new().fg(palette.muted),
        )],
        Some(Lines::Text { added, .. }) if untracked => vec![lines_added(added.into())],
        Some(Lines::Text { added, deleted }) => vec![
            lines_added(added.into()),
            Span::raw(" "),
            lines_removed(deleted.into()),
        ],
    }
}

/// The style of a change's letter, here and in the file tree: added or
/// untracked in the colour of lines added, deleted in that of lines
/// removed, in conflict in the alert colour and bold, any other change in
/// the colour of modified files.
pub(super) fn letter_style(change: Change) -> Style {
    let palette = palette::current();
    match change {
        Change::Added | Change::Untracked => Style::new().fg(palette.added),
        Change::Deleted => Style::new().fg(palette.removed),
        Change::Unmerged => Style::new().fg(palette.alert).add_modifier(Modifier::BOLD),
        Change::Modified | Change::TypeChanged | Change::Renamed | Change::Copied => {
            Style::new().fg(palette.modified)
        }
    }
}

/// The first line: how many paths changed, in bold, and the lines over
/// all rows flush right.
fn header(rows: &[Row<'_>], files: usize, width: usize) -> Line<'static> {
    let (mut added, mut deleted) = (0, 0);
    for row in rows {
        if let Some(Lines::Text {
            added: more,
            deleted: fewer,
        }) = row.lines
        {
            added += u64::from(more);
            deleted += u64::from(fewer);
        }
    }
    let files = Span::styled(
        strings::changes_files(files),
        Style::new().add_modifier(Modifier::BOLD),
    );
    let lines = vec![lines_added(added), Span::raw(" "), lines_removed(deleted)];
    spread(vec![files], lines, width)
}

/// One row: the marker, the change letter in its colour, the path cut to
/// the room left, in the accent while selected in the focused pane, and
/// the lines flush right.
fn row_line(row: &Row<'_>, width: usize, selected: bool, focused: bool) -> Line<'static> {
    let palette = palette::current();
    let marker = if selected {
        strings::SELECTED_MARKER
    } else {
        strings::UNSELECTED_MARKER
    };
    let change = row.change();
    let letter = match change {
        Some(change) => Span::styled(format!("{} ", change.letter()), letter_style(change)),
        None => Span::raw("  "),
    };
    let path = printable(&row.entry.path.to_string_lossy());
    let style = if selected && focused {
        Style::new().fg(palette.accent)
    } else {
        Style::new()
    };
    let left = vec![
        Span::styled(marker, Style::new().fg(palette.accent)),
        letter,
        Span::styled(path, style),
    ];
    spread(
        left,
        counts(row.lines, change == Some(Change::Untracked)),
        width,
    )
}

/// `text` with control characters, which the screen would drop, shown
/// as the replacement character so the columns stay put.
pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { '\u{FFFD}' } else { ch })
        .collect()
}

/// `text` padded with spaces to `width` columns.
pub(crate) fn pad(text: &str, width: usize) -> String {
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
    use crate::git::last::Last;
    use crate::git::status::{Branch, Status};
    use crate::git::today::Today;
    use crate::ui::palette::{ACCENT, ADDED, ALERT, FAINT, MODIFIED, MUTED, REMOVED};
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
            today: Ok(Today {
                commits: 3,
                added: 312,
                deleted: 58,
            }),
            last: Ok(None),
            origin: false,
            push_remote: None,
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
                (offset, _, _) = render(
                    frame,
                    frame.area(),
                    snapshot,
                    Lists {
                        selected,
                        focused,
                        scroll,
                    },
                    None,
                    0,
                    &mut Targets::default(),
                );
            })
            .expect("draw");
        (terminal, offset)
    }

    #[test]
    fn the_commit_box_takes_the_bottom_of_the_pane() {
        let snapshot = sample();
        let first = selected(Section::Staged, "src/app.rs");
        let mut draft = commit::Draft::default();
        draft.apply(&commit::Edit::Insert("feat: x".into()));
        let mut terminal = Terminal::new(TestBackend::new(23, 10)).expect("terminal");
        let mut result = (0, 0, None);
        terminal
            .draw(|frame| {
                result = render(
                    frame,
                    frame.area(),
                    &snapshot,
                    Lists {
                        selected: Some(&first),
                        focused: true,
                        scroll: 0,
                    },
                    Some(commit::Open {
                        draft: &draft,
                        suggesting: false,
                    }),
                    0,
                    &mut Targets::default(),
                );
            })
            .expect("draw");
        assert!(row(&terminal, 0).starts_with("4 files"));
        assert!(
            row(&terminal, 3).starts_with("▸ M src/app.rs"),
            "{}",
            row(&terminal, 3)
        );
        assert!(
            row(&terminal, 4).starts_with("today ─"),
            "{}",
            row(&terminal, 4)
        );
        assert_eq!(row(&terminal, 5), "3 commits      +312 −58");
        assert!(
            row(&terminal, 6).starts_with("commit"),
            "{}",
            row(&terminal, 6)
        );
        assert!(row(&terminal, 7).starts_with('╭'));
        assert!(
            row(&terminal, 8).starts_with("│feat: x"),
            "{}",
            row(&terminal, 8)
        );
        assert!(row(&terminal, 9).starts_with('╰'));
        assert_eq!(
            result,
            (0, 4, Some(Position { x: 8, y: 8 })),
            "four lines for the lists"
        );
        // Too short for both: the box wins what there is.
        let mut terminal = Terminal::new(TestBackend::new(23, 3)).expect("terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &snapshot,
                    Lists {
                        selected: Some(&first),
                        focused: true,
                        scroll: 0,
                    },
                    Some(commit::Open {
                        draft: &draft,
                        suggesting: false,
                    }),
                    0,
                    &mut Targets::default(),
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

    fn bold(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> bool {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::BOLD))
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
        let first = moved(&snapshot, None, Movement::Line(Step::Down), 1);
        assert_eq!(first, Some(selected(Section::Staged, "src/app.rs")));
        let second = moved(&snapshot, first.as_ref(), Movement::Line(Step::Down), 1);
        assert_eq!(second, Some(selected(Section::Staged, "src/ui/layout.rs")));
        let third = moved(&snapshot, second.as_ref(), Movement::Line(Step::Down), 1);
        assert_eq!(third, Some(selected(Section::Unstaged, "src/app.rs")));
        assert_eq!(
            moved(&snapshot, second.as_ref(), Movement::Line(Step::Up), 1),
            first
        );
        assert_eq!(
            moved(&snapshot, first.as_ref(), Movement::Line(Step::Up), 1),
            first,
            "stays at the top"
        );
        let last = selected(Section::Unstaged, "new.txt");
        assert_eq!(
            moved(&snapshot, Some(&last), Movement::Line(Step::Down), 1),
            Some(last.clone()),
            "stays at the end"
        );
        assert_eq!(
            moved(&Snapshot::default(), None, Movement::Line(Step::Down), 1),
            None
        );
        assert_eq!(
            moved(
                &Snapshot::default(),
                Some(&last),
                Movement::Line(Step::Up),
                1
            ),
            None
        );
    }

    #[test]
    fn a_movement_goes_a_row_half_the_page_or_all_of_it_and_stops_at_the_ends() {
        let (up, down) = (Step::Up, Step::Down);
        // Ten rows, four of them on screen.
        let index = |movement: Movement, from| movement.index(from, 10, 4);
        assert_eq!(index(Movement::Line(down), 0), 1);
        assert_eq!(index(Movement::Line(up), 5), 4);
        assert_eq!(index(Movement::Half(down), 0), 2);
        assert_eq!(index(Movement::Page(down), 0), 4);
        assert_eq!(
            index(Movement::Page(down), 8),
            9,
            "no further than the last"
        );
        assert_eq!(index(Movement::Half(up), 9), 7);
        assert_eq!(index(Movement::Page(up), 3), 0, "no further than the first");
        assert_eq!(index(Movement::Top, 6), 0);
        assert_eq!(index(Movement::End, 6), 9);
        assert_eq!(index(Movement::Line(up), 20), 9, "from past the last");
        // Half of one row, or a page of none, still moves a row.
        assert_eq!(Movement::Half(down).index(0, 10, 1), 1);
        assert_eq!(Movement::Page(down).index(0, 10, 0), 1);
        assert_eq!(Movement::Half(up).by(5), Some((up, 2)));
        assert_eq!(Movement::Page(down).by(5), Some((down, 5)));
        assert_eq!(Movement::Top.by(5), None);
        assert_eq!(Movement::End.by(5), None);
        // One row, or none.
        for movement in [
            Movement::Line(down),
            Movement::Page(up),
            Movement::Top,
            Movement::End,
        ] {
            assert_eq!(movement.index(0, 1, 4), 0, "{movement:?}");
            assert_eq!(movement.index(0, 0, 4), 0, "{movement:?}");
        }
    }

    #[test]
    fn the_selection_moves_by_half_or_all_the_lines_shown_and_to_the_ends() {
        let snapshot = sample();
        // The rows on lines 3 and 4, then 7, 8 and 9, past the blank line
        // and the title of the unstaged list.
        let first = selected(Section::Staged, "src/app.rs");
        let second = selected(Section::Staged, "src/ui/layout.rs");
        let middle = selected(Section::Unstaged, "src/app.rs");
        let readme = selected(Section::Unstaged, "README.md");
        let last = selected(Section::Unstaged, "new.txt");
        let (up, down) = (Step::Up, Step::Down);
        // Four lines shown: half is two lines, all of them four.
        let from = |selected: &Selected, movement| moved(&snapshot, Some(selected), movement, 4);
        assert_eq!(
            from(&first, Movement::Half(down)),
            Some(second.clone()),
            "the last row before the blank line"
        );
        assert_eq!(from(&first, Movement::Page(down)), Some(middle.clone()));
        assert_eq!(
            from(&second, Movement::Page(down)),
            Some(readme.clone()),
            "from the staged list into the unstaged one"
        );
        assert_eq!(
            from(&middle, Movement::Page(down)),
            Some(last.clone()),
            "stops at the last"
        );
        assert_eq!(from(&last, Movement::Half(up)), Some(middle.clone()));
        assert_eq!(
            from(&last, Movement::Page(up)),
            Some(middle.clone()),
            "the first row after the blank line"
        );
        assert_eq!(
            from(&readme, Movement::Page(up)),
            Some(second.clone()),
            "from the unstaged list into the staged one"
        );
        assert_eq!(
            from(&second, Movement::Page(up)),
            Some(first.clone()),
            "stops at the first"
        );
        // Half of three lines is one: past the blank line and the title,
        // still a row.
        let one =
            |selected: &Selected, step| moved(&snapshot, Some(selected), Movement::Half(step), 3);
        assert_eq!(one(&second, down), Some(middle.clone()));
        assert_eq!(one(&middle, up), Some(second));
        assert_eq!(from(&last, Movement::Top), Some(first.clone()));
        assert_eq!(from(&first, Movement::End), Some(last));
        assert_eq!(moved(&Snapshot::default(), None, Movement::End, 4), None);
    }

    #[test]
    fn a_page_down_shows_the_lines_after_the_last_page_across_the_lists() {
        // Six files staged and six not, four lines at once: the staged
        // rows on lines 3 to 8, the unstaged ones on 11 to 16.
        let staged = (1..=6).map(|n| entry(&format!("a{n}.txt"), Some(Change::Modified), None));
        let unstaged = (1..=6).map(|n| entry(&format!("b{n}.txt"), None, Some(Change::Modified)));
        let snapshot = Snapshot {
            status: Status {
                entries: staged.chain(unstaged).collect(),
                ..Status::default()
            },
            ..Snapshot::default()
        };
        let mut selection = moved(&snapshot, None, Movement::Top, 4);
        let (mut scroll, mut offsets, mut shown) = (0, Vec::new(), String::new());
        for _ in 0..5 {
            let (terminal, offset) = draw(23, 4, &snapshot, selection.as_ref(), true, scroll);
            offsets.push(offset);
            shown.extend((0..4).map(|y| row(&terminal, y)));
            scroll = offset;
            selection = moved(&snapshot, selection.as_ref(), Movement::Page(Step::Down), 4);
        }
        assert_eq!(offsets, [0, 4, 8, 12, 13], "each page right after the last");
        for path in snapshot.status.entries.iter().map(|entry| &entry.path) {
            let path = path.to_string_lossy();
            assert!(shown.contains(path.as_ref()), "{path} shown");
        }
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
    fn renders_the_totals_the_sections_the_rows_and_today() {
        let snapshot = sample();
        let last = selected(Section::Unstaged, "new.txt");
        let (terminal, offset) = draw(23, 12, &snapshot, Some(&last), true, 0);
        assert_eq!(offset, 0);
        let rows: Vec<String> = (0..12).map(|y| row(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "4 files        +101 −20",
                "                       ",
                "staged                2",
                "  M src/app.rs   +31 −4",
                "  A src/ui/layou… +9 −0",
                "                       ",
                "unstaged              3",
                "  M src/app.rs   +61 −4",
                "  D README.md    +0 −12",
                "▸ ? new.txt            ",
                "today ─────────────────",
                "3 commits      +312 −58",
            ]
        );
        // The totals: the files in bold, the lines in their colours.
        assert_eq!(fg(&terminal, 0, 0), Color::Reset);
        assert!(bold(&terminal, 0, 0));
        assert_eq!(fg(&terminal, 15, 0), ADDED);
        assert_eq!(fg(&terminal, 20, 0), REMOVED);
        // The titles and their lengths muted.
        assert_eq!(fg(&terminal, 0, 2), MUTED);
        assert_eq!(fg(&terminal, 22, 2), MUTED);
        assert_eq!(fg(&terminal, 22, 6), MUTED);
        // Each letter in the colour of its change.
        assert_eq!(fg(&terminal, 2, 3), MODIFIED, "M");
        assert_eq!(fg(&terminal, 2, 4), ADDED, "A");
        assert_eq!(fg(&terminal, 2, 8), REMOVED, "D");
        assert_eq!(fg(&terminal, 2, 9), ADDED, "?");
        // The lines of a row, a count of zero muted.
        assert_eq!(fg(&terminal, 17, 3), ADDED);
        assert_eq!(fg(&terminal, 21, 3), REMOVED);
        assert_eq!(fg(&terminal, 21, 4), MUTED, "−0");
        assert_eq!(fg(&terminal, 17, 8), MUTED, "+0");
        // The selection: the marker and, in the focused pane, the path in
        // the accent.
        assert_eq!(fg(&terminal, 0, 9), ACCENT, "marker");
        assert_eq!(fg(&terminal, 4, 9), ACCENT, "selected row, focused");
        assert_eq!(fg(&terminal, 4, 7), Color::Reset, "other rows");
        // Today: a muted heading over a faint rule, the commits in the
        // terminal's colour and their lines in theirs.
        assert_eq!(fg(&terminal, 0, 10), MUTED);
        assert_eq!(fg(&terminal, 6, 10), FAINT);
        assert_eq!(fg(&terminal, 0, 11), Color::Reset);
        assert_eq!(fg(&terminal, 15, 11), ADDED);
        assert_eq!(fg(&terminal, 20, 11), REMOVED);

        let (terminal, _) = draw(23, 12, &snapshot, Some(&last), false, 0);
        assert_eq!(fg(&terminal, 0, 9), ACCENT, "marker stays");
        assert_eq!(
            fg(&terminal, 4, 9),
            Color::Reset,
            "path plain when unfocused"
        );
        assert!(row(&terminal, 9).starts_with("▸ ? new.txt"));
    }

    #[test]
    fn letters_take_the_colour_of_their_change() {
        for change in [
            Change::Modified,
            Change::TypeChanged,
            Change::Renamed,
            Change::Copied,
        ] {
            assert_eq!(
                letter_style(change),
                Style::new().fg(MODIFIED),
                "{change:?}"
            );
        }
        assert_eq!(letter_style(Change::Added), Style::new().fg(ADDED));
        assert_eq!(letter_style(Change::Untracked), Style::new().fg(ADDED));
        assert_eq!(letter_style(Change::Deleted), Style::new().fg(REMOVED));
        assert_eq!(
            letter_style(Change::Unmerged),
            Style::new().fg(ALERT).add_modifier(Modifier::BOLD),
            "a conflict stands out"
        );
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
        let (terminal, _) = draw(20, 10, &snapshot, None, false, 0);
        assert_eq!(row(&terminal, 3), "  A img.png      bin");
        assert_eq!(fg(&terminal, 17, 3), MUTED, "bin");
        // Each wide character takes two cells; the counts stay flush right.
        let cell = |x: u16| {
            terminal
                .backend()
                .buffer()
                .cell((x, 6))
                .map_or("", |c| c.symbol())
                .to_owned()
        };
        let cells: String = [4, 6, 8, 10, 11, 13].into_iter().map(cell).collect();
        assert_eq!(cells, "日本語/道…");
        assert!(
            row(&terminal, 6).ends_with(" +1 −1"),
            "{}",
            row(&terminal, 6)
        );
        let tab = row(&terminal, 7);
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
        // The lines: the totals, a blank line, the staged title and its
        // two rows, a blank line, the unstaged title and its three rows.
        let (terminal, offset) = draw(23, 4, &snapshot, Some(&last), true, 0);
        assert_eq!(offset, 6);
        assert!(row(&terminal, 0).starts_with("unstaged "));
        assert!(row(&terminal, 3).starts_with("▸ ? new.txt"));
        // Moving up within the window keeps the offset.
        let readme = selected(Section::Unstaged, "README.md");
        let (terminal, offset) = draw(23, 4, &snapshot, Some(&readme), true, 6);
        assert_eq!(offset, 6);
        assert!(row(&terminal, 2).starts_with("▸ D README.md"));
        let (terminal, offset) = draw(23, 1, &snapshot, Some(&readme), true, 0);
        assert_eq!(offset, 8);
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
    fn today_shows_the_error_when_it_could_not_be_read() {
        let mut snapshot = sample();
        snapshot.today = Err("git var: fatal: unable to auto-detect email address".into());
        let (terminal, _) = draw(30, 10, &snapshot, None, false, 0);
        assert_eq!(row(&terminal, 8), format!("today {}", "─".repeat(24)));
        assert_eq!(row(&terminal, 9), "git var: fatal: unable to aut…");
        assert_eq!(fg(&terminal, 0, 9), ALERT);
        // No commits yet today: muted.
        snapshot.today = Ok(Today::default());
        let (terminal, _) = draw(30, 10, &snapshot, None, false, 0);
        assert_eq!(row(&terminal, 9).trim_end(), "no commits");
        assert_eq!(fg(&terminal, 0, 9), MUTED);
        // Too short for the lists, with their first title and row, and the
        // section: the lists win.
        for height in [4, 5] {
            let (terminal, _) = draw(30, height, &snapshot, None, false, 0);
            let all: String = (0..height).map(|y| row(&terminal, y)).collect();
            assert!(!all.contains("today"), "{all}");
        }
        let (terminal, _) = draw(30, 6, &snapshot, None, false, 0);
        assert!(row(&terminal, 3).starts_with("  M src/app.rs"));
        assert!(row(&terminal, 4).starts_with("today ─"));
    }

    #[test]
    fn the_last_commit_follows_today() {
        let mut snapshot = sample();
        snapshot.last = Ok(Some(Last {
            oid: "3a2bd68a9f12".into(),
            hash: "3a2bd68a9f12".into(),
            time: 1_000_000,
            subject: "feat: x".into(),
        }));
        let draw_at = |width: u16, height: u16, snapshot: &Snapshot, now: i64| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        frame.area(),
                        snapshot,
                        Lists {
                            selected: None,
                            focused: false,
                            scroll: 0,
                        },
                        None,
                        now,
                        &mut Targets::default(),
                    );
                })
                .expect("draw");
            terminal
        };
        let terminal = draw_at(24, 12, &snapshot, 1_000_000 + 2 * 3600);
        let rows: Vec<String> = (8..12).map(|y| row(&terminal, y)).collect();
        assert_eq!(
            rows,
            [
                "today ──────────────────",
                "3 commits       +312 −58",
                "last 3a2bd68a9f12     2h",
                "feat: x                 ",
            ]
        );
        assert_eq!(fg(&terminal, 0, 10), MUTED, "the label");
        assert_eq!(fg(&terminal, 5, 10), MUTED, "the hash");
        assert_eq!(fg(&terminal, 22, 10), MUTED, "the age");
        assert_eq!(fg(&terminal, 0, 11), Color::Reset, "the subject");
        assert_eq!(
            last_age(Some(&snapshot), 1_000_000 + 2 * 3600).as_deref(),
            Some("2h")
        );
        // Narrow, the hash gives way to the age.
        let terminal = draw_at(18, 12, &snapshot, 1_000_000);
        assert_eq!(row(&terminal, 10), "last 3a2bd68a… now");
        // Short, the subject goes first, then the commit; the lists keep
        // their rows.
        let terminal = draw_at(24, 7, &snapshot, 1_000_000);
        assert!(
            row(&terminal, 6).starts_with("last 3a2bd"),
            "{}",
            row(&terminal, 6)
        );
        let terminal = draw_at(24, 6, &snapshot, 1_000_000);
        assert!(
            row(&terminal, 5).starts_with("3 commits"),
            "{}",
            row(&terminal, 5)
        );
        // With the commit box open it comes first; tiny areas are no
        // trouble.
        let mut draft = commit::Draft::default();
        draft.apply(&commit::Edit::Insert("feat: y".into()));
        for (width, height) in [(0, 0), (1, 1), (3, 2), (6, 5), (24, 8), (24, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|frame| {
                    let lists = Lists {
                        selected: None,
                        focused: false,
                        scroll: 0,
                    };
                    render(
                        frame,
                        frame.area(),
                        &snapshot,
                        lists,
                        Some(commit::Open {
                            draft: &draft,
                            suggesting: false,
                        }),
                        0,
                        &mut Targets::default(),
                    );
                })
                .expect("draw");
        }
        // Unreadable, it says so; before the first commit there is none.
        snapshot.last = Err("git log: fatal".into());
        let terminal = draw_at(24, 12, &snapshot, 1_000_000);
        assert_eq!(row(&terminal, 11), "last ?                  ");
        assert_eq!(fg(&terminal, 5, 11), ALERT);
        assert_eq!(last_age(Some(&snapshot), 0), None);
        snapshot.last = Ok(None);
        assert_eq!(last_age(Some(&snapshot), 0), None);
        assert_eq!(last_age(None, 0), None);
        let terminal = draw_at(24, 12, &snapshot, 1_000_000);
        assert!(
            row(&terminal, 11).starts_with("3 commits"),
            "no commit yet, so the section ends with today's line"
        );
    }

    #[test]
    fn says_no_changes_and_survives_tiny_areas() {
        let (terminal, offset) = draw(23, 6, &Snapshot::default(), None, false, 3);
        assert_eq!(offset, 0);
        // The check in the colour of lines added, the words muted, in the
        // middle of the lists' rows.
        assert_eq!(row(&terminal, 2), "     ✓ no changes      ");
        assert_eq!(fg(&terminal, 5, 2), ADDED);
        assert_eq!(fg(&terminal, 7, 2), MUTED);
        assert_eq!(row(&terminal, 5).trim_end(), "no commits");
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
