//! The project picker, over the screen: the open sessions and the
//! agent's recent projects, filtered as a query is typed, or a directory
//! typed whole.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::changes::{Step, cut, pad, printable};
use super::commit::tail;
use super::framed;
use super::palette::{ACCENT, FAINT, MUTED};
use super::projects::{display_path, fit_path};
use crate::strings;

/// The widest the box gets, its frame included.
const WIDTH: u16 = 72;
/// The narrowest box with room for the prompt and the cursor after it:
/// the frame, a column of padding on each side, the prompt and a column.
const MIN_WIDTH: u16 = 7;
/// The most rows the box lists at once.
const ROWS: usize = 15;

/// A row of the picker: a project the agent ran in lately, or a session
/// open in one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The project's name.
    pub name: String,
    /// The directory, with the home directory as `~`.
    pub path: String,
    /// The directory.
    pub dir: PathBuf,
    /// Whether a session is open in the directory.
    pub open: bool,
}

impl Item {
    /// The row of the session open in `dir`, on the project `name`, with
    /// `home` shown as `~`.
    pub fn open(name: String, dir: PathBuf, home: Option<&Path>) -> Self {
        Self::new(name, dir, home, true)
    }

    /// The row of the project `name` in `dir`, with `home` shown as `~`.
    pub fn recent(name: String, dir: PathBuf, home: Option<&Path>) -> Self {
        Self::new(name, dir, home, false)
    }

    fn new(name: String, dir: PathBuf, home: Option<&Path>, open: bool) -> Self {
        Self {
            name,
            path: display_path(&dir, home),
            dir,
            open,
        }
    }

    /// Whether `query`, in lower case, is in the name or else in the
    /// path, case aside.
    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query) || self.path.to_lowercase().contains(query)
    }
}

/// What the picker opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    /// The chosen row.
    Item(&'a Item),
    /// The query, as typed: a directory relative to the home directory
    /// unless absolute, `~` being the home. The caller resolves it.
    Typed(&'a str),
}

/// The picker: the query typed, the rows it filters, and the chosen one
/// among those the query matches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picker {
    query: String,
    items: Vec<Item>,
    chosen: usize,
}

impl Picker {
    /// A picker with nothing typed and the first row chosen, listing the
    /// sessions `open`, then the `recent` projects in a directory with no
    /// session open, each in the order given.
    pub fn new(open: Vec<Item>, recent: Vec<Item>) -> Self {
        let mut items = open;
        for item in recent {
            if !items.iter().any(|listed| listed.dir == item.dir) {
                items.push(item);
            }
        }
        Self {
            query: String::new(),
            items,
            chosen: 0,
        }
    }

    /// What was typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows the query matches, in the order listed.
    pub fn rows(&self) -> impl Iterator<Item = &Item> {
        let query = self.query.to_lowercase();
        self.items.iter().filter(move |item| item.matches(&query))
    }

    /// Adds `text` to the query, its control characters left out.
    pub fn insert(&mut self, text: &str) {
        self.query
            .extend(text.chars().filter(|ch| !ch.is_control()));
        self.clamp();
    }

    /// Takes the last grapheme off the query.
    pub fn backspace(&mut self) {
        if let Some((start, _)) = self.query.grapheme_indices(true).next_back() {
            self.query.truncate(start);
        }
        self.clamp();
    }

    /// Chooses the row above or below, among the rows the query matches.
    pub fn step(&mut self, step: Step) {
        self.chosen = match step {
            Step::Up => self.chosen.saturating_sub(1),
            Step::Down => self.chosen.saturating_add(1),
        };
        self.clamp();
    }

    /// What the picker opens: the query when it starts with `/`, `~` or
    /// `.`, holds a `/` or matches no row, else the chosen row. None when
    /// nothing is typed and there is no row.
    pub fn choice(&self) -> Option<Choice<'_>> {
        let query = self.query.as_str();
        let typed = query.starts_with(['/', '~', '.']) || query.contains('/');
        match self.rows().nth(self.chosen) {
            Some(item) if !typed => Some(Choice::Item(item)),
            _ => (!query.is_empty()).then_some(Choice::Typed(query)),
        }
    }

    /// Keeps the chosen row among those the query matches.
    fn clamp(&mut self) {
        self.chosen = self.chosen.min(self.rows().count().saturating_sub(1));
    }
}

/// Draws the picker in a rounded frame centred in `area`, over whatever
/// is there, a column in from each side: the prompt and the query on the
/// first row, a faint rule under them when there is room, then the rows
/// the query matches, the chosen one marked while it is what the picker
/// opens, or a text saying that none does. The rows of open sessions are
/// marked, and long paths keep their ends. The box keeps its size while
/// the query changes. Returns where the cursor goes, after the query;
/// none when the box does not fit.
pub fn render(frame: &mut Frame, area: Rect, picker: &Picker) -> Option<Position> {
    let listed = picker.items.len().clamp(1, ROWS);
    let width = WIDTH.min(area.width);
    // The frame, the query, the rule under it and the rows.
    let height = u16::try_from(listed + 4)
        .unwrap_or(u16::MAX)
        .min(area.height);
    if width < MIN_WIDTH || height < 3 {
        return None;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let accent = Style::new().fg(ACCENT);
    let bold_accent = accent.add_modifier(Modifier::BOLD);
    let title = Span::styled(strings::PICKER_TITLE, bold_accent);
    let block = framed(vec![title]).padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let width = usize::from(inner.width);
    let prompt = Span::styled(strings::PICKER_PROMPT, bold_accent);
    let room = width.saturating_sub(strings::PICKER_PROMPT.width());
    let typed = tail(&picker.query, room);
    let query = if picker.query.is_empty() {
        let placeholder = cut(strings::PICKER_PLACEHOLDER, room);
        Span::styled(placeholder, Style::new().fg(MUTED))
    } else {
        Span::raw(typed.clone())
    };
    let mut lines = vec![Line::from(vec![prompt, query])];
    if inner.height > 2 {
        lines.push(Line::styled(
            symbols::line::HORIZONTAL.repeat(width),
            Style::new().fg(FAINT),
        ));
    }
    let room = usize::from(inner.height).saturating_sub(lines.len());
    let rows: Vec<&Item> = picker.rows().collect();
    if rows.is_empty() {
        lines.push(Line::styled(
            cut(strings::PICKER_EMPTY, width),
            Style::new().fg(MUTED),
        ));
    } else {
        let marked = matches!(picker.choice(), Some(Choice::Item(_)));
        let columns = Columns::new(&picker.items, width);
        // The chosen row stays in view, at the bottom once it has to.
        let first = picker.chosen.saturating_sub(room.saturating_sub(1));
        for (index, item) in rows.into_iter().enumerate().skip(first).take(room) {
            lines.push(row_line(item, columns, marked && index == picker.chosen));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
    let after = strings::PICKER_PROMPT.width() + typed.width();
    let x = inner.x + u16::try_from(after).unwrap_or(u16::MAX);
    Some(Position { x, y: inner.y })
}

/// How a row's `width` columns are shared: the names get the widest
/// name's width, up to a third of the row, and the open mark its room
/// when a row has it; the path gets the rest.
#[derive(Clone, Copy, Debug)]
struct Columns {
    width: usize,
    name: usize,
    mark: usize,
}

impl Columns {
    /// The columns for every row of `items`, whatever the query shows,
    /// so they stay put while it changes.
    fn new(items: &[Item], width: usize) -> Self {
        let name = items
            .iter()
            .map(|item| printable(&item.name).width())
            .max()
            .unwrap_or(0)
            .min(width / 3);
        let mark = if items.iter().any(|item| item.open) {
            strings::PICKER_OPEN.width() + 1
        } else {
            0
        };
        Self { width, name, mark }
    }
}

/// One row: the marker, the name, the path keeping its ends, and the
/// open mark flush right. The chosen row's name is in the accent and
/// bold and its path in the terminal's colour; the other rows' paths are
/// muted.
fn row_line(item: &Item, columns: Columns, chosen: bool) -> Line<'static> {
    let marker = if chosen {
        strings::SELECTED_MARKER
    } else {
        strings::UNSELECTED_MARKER
    };
    let (name_style, path_style) = if chosen {
        (
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            Style::new(),
        )
    } else {
        (Style::new(), Style::new().fg(MUTED))
    };
    let name = pad(&cut(&printable(&item.name), columns.name), columns.name);
    // The marker, the name, two spaces, the path, then the mark.
    let room = columns
        .width
        .saturating_sub(marker.width() + columns.name + 2 + columns.mark);
    let path = pad(&fit_path(&item.path, room), room);
    let mark = if item.open {
        format!(" {}", strings::PICKER_OPEN)
    } else {
        String::new()
    };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(ACCENT)),
        Span::styled(name, name_style),
        Span::raw("  "),
        Span::styled(path, path_style),
        Span::styled(mark, Style::new().fg(ACCENT)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    const HOME: &str = "/Users/x";

    fn open(name: &str, dir: &str) -> Item {
        Item::open(name.into(), dir.into(), Some(Path::new(HOME)))
    }

    fn recent(name: &str, dir: &str) -> Item {
        Item::recent(name.into(), dir.into(), Some(Path::new(HOME)))
    }

    /// Two open sessions and three recent projects, one of them in a
    /// session's directory.
    fn sample() -> Picker {
        Picker::new(
            vec![
                open("termis", "/Users/x/dev/termis"),
                open("notes", "/Users/x/notes"),
            ],
            vec![
                recent("api", "/Users/x/dev/api"),
                recent("termis", "/Users/x/dev/termis"),
                recent("Website", "/srv/www/Website"),
            ],
        )
    }

    fn names(picker: &Picker) -> Vec<&str> {
        picker.rows().map(|item| item.name.as_str()).collect()
    }

    fn chosen(picker: &Picker) -> Option<&str> {
        match picker.choice() {
            Some(Choice::Item(item)) => Some(item.name.as_str()),
            _ => None,
        }
    }

    fn draw(width: u16, height: u16, picker: &Picker) -> (Terminal<TestBackend>, Option<Position>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut cursor = None;
        terminal
            .draw(|frame| cursor = render(frame, frame.area(), picker))
            .expect("draw");
        (terminal, cursor)
    }

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    /// Columns `from` to `to`, not included, of row `y`.
    fn cells(terminal: &Terminal<TestBackend>, y: u16, from: u16, to: u16) -> String {
        let buffer = terminal.backend().buffer();
        (from..to)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    #[test]
    fn open_sessions_come_first_and_a_project_is_listed_once() {
        let picker = sample();
        assert_eq!(names(&picker), ["termis", "notes", "api", "Website"]);
        let rows: Vec<(&str, bool)> = picker
            .rows()
            .map(|item| (item.path.as_str(), item.open))
            .collect();
        assert_eq!(
            rows,
            [
                ("~/dev/termis", true),
                ("~/notes", true),
                ("~/dev/api", false),
                ("/srv/www/Website", false),
            ]
        );
        assert_eq!(picker.query(), "");
        assert_eq!(chosen(&picker), Some("termis"));
    }

    #[test]
    fn the_query_filters_on_the_name_then_the_path_case_aside() {
        let mut picker = sample();
        picker.insert("WEB");
        assert_eq!(names(&picker), ["Website"]);
        let mut picker = sample();
        picker.insert("dev");
        assert_eq!(names(&picker), ["termis", "api"], "on the path, in order");
        let mut picker = sample();
        picker.insert("o\tt\u{7}");
        assert_eq!(picker.query(), "ot", "control characters left out");
        assert_eq!(names(&picker), ["notes"]);
        picker.backspace();
        picker.backspace();
        picker.backspace();
        assert_eq!(picker.query(), "");
        assert_eq!(names(&picker).len(), 4);
    }

    #[test]
    fn the_chosen_row_moves_and_is_clamped_when_the_list_shrinks() {
        let mut picker = sample();
        picker.step(Step::Up);
        assert_eq!(chosen(&picker), Some("termis"), "stays at the top");
        for _ in 0..4 {
            picker.step(Step::Down);
        }
        assert_eq!(chosen(&picker), Some("Website"), "stays at the bottom");
        picker.insert("s");
        assert_eq!(names(&picker), ["termis", "notes", "Website"]);
        assert_eq!(chosen(&picker), Some("Website"), "clamped to the last");
        picker.insert("i");
        assert_eq!(names(&picker), ["Website"]);
        picker.backspace();
        assert_eq!(chosen(&picker), Some("termis"), "not brought back");
        picker.step(Step::Down);
        assert_eq!(chosen(&picker), Some("notes"));
    }

    #[test]
    fn a_query_that_looks_like_a_path_or_matches_nothing_is_typed() {
        let typed = |text: &str| {
            let mut picker = sample();
            picker.insert(text);
            match picker.choice() {
                Some(Choice::Typed(query)) => Some(query.to_owned()),
                _ => None,
            }
        };
        for path in ["/srv", "~", "~/dev", "./x", "../x", "dev/api", "zzz"] {
            assert_eq!(typed(path).as_deref(), Some(path), "{path}");
        }
        let mut picker = sample();
        picker.insert("/srv");
        assert_eq!(names(&picker), ["Website"], "still filtered");
        assert_eq!(typed("api"), None, "a row");
        let mut picker = sample();
        picker.insert("api");
        assert_eq!(chosen(&picker), Some("api"));
        // With no rows at all, only something typed is opened.
        let mut empty = Picker::default();
        assert_eq!(empty.choice(), None);
        empty.insert("x");
        assert_eq!(empty.choice(), Some(Choice::Typed("x")));
    }

    /// An open session, a recent project and one deep in the home.
    fn three() -> Picker {
        Picker::new(
            vec![open("termis", "/Users/x/dev/termis")],
            vec![
                recent("api", "/Users/x/dev/api"),
                recent(
                    "deep",
                    "/Users/x/WebProjects/Personal/clients/acme/backend-services/deep",
                ),
            ],
        )
    }

    #[test]
    fn draws_a_centred_box_with_the_query_and_the_rows() {
        let picker = three();
        let (terminal, cursor) = draw(100, 30, &picker);
        // 72 columns wide: a rounded frame, a column of padding each side;
        // the query, a rule and three rows high inside.
        let (left, right) = (14, 86);
        assert_eq!(
            cells(&terminal, 11, left, right),
            format!("╭─ open a project {}╮", "─".repeat(53))
        );
        let inside = |y| cells(&terminal, y, left + 1, right - 1);
        assert_eq!(
            inside(12),
            format!(" › {:<66} ", "a project's name or a directory")
        );
        assert_eq!(inside(13), format!(" {} ", "─".repeat(68)));
        assert_eq!(
            inside(14),
            format!(" ▸ termis  {:<53} open ", "~/dev/termis")
        );
        assert_eq!(inside(15), format!("   api     {:<53}      ", "~/dev/api"));
        assert_eq!(
            inside(16),
            format!(
                "   deep    {:<53}      ",
                "~/…/Personal/clients/acme/backend-services/deep"
            ),
            "a long path keeps its ends"
        );
        assert!(row(&terminal, 17).contains('╰'));
        assert!(row(&terminal, 10).trim().is_empty() && row(&terminal, 18).trim().is_empty());
        assert_eq!(cursor, Some(Position { x: 18, y: 12 }));
        // The frame and the prompt in the accent, the title and the prompt
        // bold; the placeholder muted, the rule faint; the chosen row's
        // name in the accent and bold, its path plain; the other paths
        // muted.
        let style = |x: u16, y: u16| {
            let cell = terminal.backend().buffer().cell((x, y)).expect("cell");
            (cell.fg, cell.modifier.contains(Modifier::BOLD))
        };
        assert_eq!(style(left, 11), (ACCENT, false), "the frame");
        assert_eq!(style(left + 3, 11), (ACCENT, true), "the title");
        assert_eq!(style(left + 2, 12), (ACCENT, true), "the prompt");
        assert_eq!(style(left + 4, 12), (MUTED, false), "the placeholder");
        assert_eq!(style(left + 2, 13), (FAINT, false), "the rule");
        assert_eq!(style(left + 2, 14), (ACCENT, false), "the marker");
        assert_eq!(style(left + 4, 14), (ACCENT, true), "the chosen name");
        assert_eq!(style(left + 12, 14), (Color::Reset, false), "its path");
        assert_eq!(style(left + 4, 15), (Color::Reset, false), "a name");
        assert_eq!(style(left + 12, 15), (MUTED, false), "a path");
        assert_eq!(style(right - 6, 14), (ACCENT, false), "open");
    }

    #[test]
    fn draws_what_was_typed_and_the_empty_text_on_a_small_terminal() {
        let mut picker = three();
        picker.insert("ter");
        let (terminal, cursor) = draw(80, 24, &picker);
        // The box keeps its size; it is centred in the 80 columns.
        let (left, right) = (4, 76);
        assert!(cells(&terminal, 8, left, right).starts_with("╭─ open a project "));
        let inside = |y| cells(&terminal, y, left + 1, right - 1);
        assert_eq!(inside(9), format!(" › {:<66} ", "ter"));
        assert_eq!(
            inside(11),
            format!(" ▸ termis  {:<53} open ", "~/dev/termis")
        );
        assert_eq!(inside(12).trim(), "");
        assert!(row(&terminal, 14).contains('╰'));
        assert_eq!(cursor, Some(Position { x: 11, y: 9 }));
        // A path typed: the rows it matches show, none is marked.
        picker.backspace();
        picker.backspace();
        picker.backspace();
        picker.insert("~/dev");
        let (terminal, _) = draw(80, 24, &picker);
        let inside = |y| cells(&terminal, y, left + 1, right - 1);
        assert!(inside(11).starts_with("   termis  ~/dev/termis"));
        assert!(inside(12).starts_with("   api     ~/dev/api"));
        // Nothing matches.
        picker.insert("zzz");
        let (terminal, cursor) = draw(80, 24, &picker);
        let inside = |y| cells(&terminal, y, left + 1, right - 1);
        assert_eq!(inside(11), format!(" {:<68} ", "no project matches"));
        assert_eq!(cursor, Some(Position { x: 16, y: 9 }));
    }

    #[test]
    fn names_take_up_to_a_third_of_the_row_and_the_columns_stay_put() {
        let picker = Picker::new(
            vec![],
            vec![
                recent("an-uncommonly-long-project-name", "/Users/x/dev/long"),
                recent("api", "/Users/x/dev/api"),
            ],
        );
        let (terminal, _) = draw(100, 30, &picker);
        let inside = |y| cells(&terminal, y, 16, 84);
        // A third of the 68 columns inside the padding, the name cut to
        // fit.
        assert_eq!(
            inside(15),
            format!("▸ {:<22}  {:<42}", "an-uncommonly-long-pr…", "~/dev/long")
        );
        assert_eq!(inside(16), format!("  {:<22}  {:<42}", "api", "~/dev/api"));
        // Rows the query hides keep their room: the name column and the
        // open mark of `termis`.
        let mut picker = three();
        picker.insert("api");
        let (terminal, _) = draw(100, 30, &picker);
        let inside = |y| cells(&terminal, y, 16, 84);
        assert_eq!(inside(14), format!("▸ api     {:<53}     ", "~/dev/api"));
    }

    #[test]
    fn the_chosen_row_stays_in_view_and_a_long_query_its_end() {
        let recents = (0..20)
            .map(|n| recent(&format!("p{n:02}"), &format!("/Users/x/p{n:02}")))
            .collect();
        let mut picker = Picker::new(vec![], recents);
        for _ in 0..17 {
            picker.step(Step::Down);
        }
        let (terminal, _) = draw(100, 30, &picker);
        // Fifteen rows listed: the chosen one is the last of them.
        let top = (0..30)
            .find(|&y| row(&terminal, y).contains('╭'))
            .expect("box");
        assert!(row(&terminal, top + 3).contains("│   p03 "));
        assert!(row(&terminal, top + 17).contains("│ ▸ p17 "));
        assert!(row(&terminal, top + 18).contains('╰'));
        // The end of a long query, and the cursor after it.
        picker.insert(&"x".repeat(100));
        let (terminal, cursor) = draw(100, 30, &picker);
        let query = row(&terminal, top + 1);
        assert!(
            query.contains(&format!("│ › {}  │", "x".repeat(65))),
            "{query}"
        );
        assert_eq!(cursor, Some(Position { x: 83, y: top + 1 }));
    }

    #[test]
    fn nothing_is_drawn_without_room_for_a_box() {
        let picker = three();
        for (width, height) in [(0, 0), (2, 2), (2, 9), (9, 2), (6, 9)] {
            let (terminal, cursor) = draw(width, height, &picker);
            assert!(
                !(0..height).any(|y| row(&terminal, y).contains('╭')),
                "nothing at {width}x{height}"
            );
            assert_eq!(cursor, None);
        }
        // The narrowest box keeps the cursor inside, after the prompt.
        let (terminal, cursor) = draw(7, 3, &picker);
        assert_eq!(row(&terminal, 0), "╭─ ope╮");
        assert_eq!(row(&terminal, 1), "│ › … │");
        assert!(row(&terminal, 2).starts_with('╰'));
        assert_eq!(cursor, Some(Position { x: 4, y: 1 }));
    }
}
