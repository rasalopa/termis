//! The start screen, before the first session: the logo, the version,
//! the sessions of the last run and the recent projects, each on a row
//! with the key that opens it, and the status bar with the screen's keys.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::changes::{cut, pad, printable};
use super::palette::{self, Palette};
use super::picker::{self, Item, Picker};
use super::projects::fit_path;
use super::toast::Toast;
use super::{accent_chip, bar_line, heading};
use crate::strings;

/// The widest the column of rows gets.
const WIDTH: u16 = 72;
/// The columns left free at least on each side of the column of rows.
const MARGIN: u16 = 2;
/// The columns of the keys.
const KEY_WIDTH: usize = 7;
/// The widest a label or a project's name is drawn.
const LABEL_MAX: usize = 20;
/// The columns between a label and the detail after it.
const LABEL_GAP: usize = 2;
/// The narrowest a numbered row's detail gets beside its account; below,
/// the account goes.
const DETAIL_BESIDE_ACCOUNT: usize = 12;
/// The narrowest a row's detail gets; below, the detail goes.
const DETAIL_MIN: usize = 8;
/// How many of the picker's rows the screen numbers, for `1` to `9`.
const RECENT: usize = 9;

/// What the start screen shows.
pub struct View<'a> {
    /// The version, as `termis --version` prints it.
    pub version: &'a str,
    /// The names of the projects of the last run, in its order; none
    /// without one.
    pub run: &'a [String],
    /// The current directory, with the home as `~`.
    pub here: &'a str,
    /// The project picker with no session open, whose first rows are the
    /// recent projects the screen numbers.
    pub list: &'a Picker,
    /// The picker `o` opened over the screen, if any.
    pub picker: Option<&'a Picker>,
    /// A notice in place of the hints.
    pub notice: Option<&'a Toast>,
    /// The colours the frame is drawn with.
    pub palette: Palette,
}

/// What a row of the start screen opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The sessions of the last run: the row of `Enter` while there is
    /// one.
    Run,
    /// The current directory: the row of `.`, or the row of `Enter`
    /// without a last run.
    Here,
    /// The recent project at this index among the picker's rows, from 0,
    /// on the row of the digit after it.
    Recent(usize),
}

/// Draws the start screen in the view's palette, centred above the status
/// bar: the logo, the version, the row of `Enter`, the row of `.` beside a
/// last run, then under a heading the first nine of the picker's rows,
/// numbered. A short terminal drops the gap under the logo, the logo, the
/// numbered rows from the last, the gap under the version, the version
/// and the row of `.`, in that order; a narrow one the logo, then each
/// numbered row's account and the details. The bar has `START` and the
/// screen's keys, or the notice in their place; the picker, while it is
/// open, goes over the screen with its keys in the bar. Returns where each
/// row was drawn and what it opens.
pub fn render(frame: &mut Frame, view: &View<'_>) -> Vec<(Rect, Row)> {
    palette::drawing(view.palette, || render_screen(frame, view))
}

/// [`render`] with the view's palette current.
fn render_screen(frame: &mut Frame, view: &View<'_>) -> Vec<(Rect, Row)> {
    let palette = palette::current();
    let area = frame.area();
    let mut drawn = Vec::new();
    if area.is_empty() {
        return drawn;
    }
    let width = WIDTH.min(area.width.saturating_sub(2 * MARGIN));
    let column = usize::from(width);
    let recent: Vec<&Item> = view.list.rows().take(RECENT).collect();
    let logo = strings::LOGO.map(UnicodeWidthStr::width).into_iter().max();
    let logo = logo.unwrap_or_default();
    let run = !view.run.is_empty();
    let rows = column > 0;
    let parts = Parts {
        logo: column >= logo,
        logo_gap: column >= logo,
        version: true,
        version_gap: true,
        enter: rows,
        here: rows && run,
        recent: if rows { recent.len() } else { 0 },
    }
    .fit(usize::from(area.height - 1));
    let height = u16::try_from(parts.height()).unwrap_or(u16::MAX);
    let mut y = area.y + (area.height - 1).saturating_sub(height) / 2;
    if parts.logo {
        let logo = u16::try_from(logo).unwrap_or(u16::MAX);
        let at = Rect::new(area.x + (area.width - logo) / 2, y, logo, 1);
        for row in strings::LOGO {
            let line = Line::styled(row, Style::new().fg(palette.accent));
            frame.render_widget(Paragraph::new(line), Rect { y, ..at });
            y += 1;
        }
    }
    if parts.logo_gap {
        y += 1;
    }
    if parts.version {
        let text = cut(
            &strings::start_version(view.version),
            usize::from(area.width),
        );
        let text_width = u16::try_from(text.width()).unwrap_or(u16::MAX);
        let at = Rect::new(area.x + (area.width - text_width) / 2, y, text_width, 1);
        let line = Line::styled(text, Style::new().fg(palette.muted));
        frame.render_widget(Paragraph::new(line), at);
        y += 1;
    }
    if parts.version_gap {
        y += 1;
    }
    let x = area.x + (area.width - width) / 2;
    let row_at = |y| Rect::new(x, y, width, 1);
    let enter_label = if run {
        strings::start_run(view.run.len())
    } else {
        strings::START_HERE.to_owned()
    };
    let labels = [enter_label.as_str(), strings::START_HERE]
        .into_iter()
        .chain(recent.iter().map(|item| item.name.as_str()))
        .map(|label| printable(label).width());
    let accounts = recent
        .iter()
        .filter_map(|item| item.account.as_deref())
        .map(|account| printable(account).width());
    let columns = Columns::new(column, labels, accounts);
    let muted = Style::new().fg(palette.muted);
    let here = || vec![Span::styled(fit_path(view.here, columns.top), muted)];
    if parts.enter {
        let (detail, row) = if run {
            (run_detail(view.run, columns.top), Row::Run)
        } else {
            (here(), Row::Here)
        };
        let line = row_line(strings::START_RUN_KEY, &enter_label, columns.label, detail);
        frame.render_widget(Paragraph::new(line), row_at(y));
        drawn.push((row_at(y), row));
        y += 1;
    }
    if parts.here {
        let line = row_line(
            strings::START_HERE_KEY,
            strings::START_HERE,
            columns.label,
            here(),
        );
        frame.render_widget(Paragraph::new(line), row_at(y));
        drawn.push((row_at(y), Row::Here));
        y += 1;
    }
    if parts.recent > 0 {
        y += 1;
        let line = heading(strings::START_RECENT, column);
        frame.render_widget(Paragraph::new(line), row_at(y));
        y += 1;
        for (index, item) in recent.into_iter().take(parts.recent).enumerate() {
            let line = recent_line(index, item, columns, view.list.accounts());
            frame.render_widget(Paragraph::new(line), row_at(y));
            drawn.push((row_at(y), Row::Recent(index)));
            y += 1;
        }
    }
    let (tag, hints) = match view.picker {
        Some(picker) => (
            strings::MODE_OPEN,
            strings::picker_hints(!picker.accounts().is_empty()),
        ),
        None => (
            strings::MODE_START,
            strings::start_hints(run, parts.recent > 0),
        ),
    };
    let bar = bar_line(
        tag,
        accent_chip(),
        hints,
        view.notice,
        usize::from(area.width),
    );
    let last = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Paragraph::new(bar), last);
    if let Some(picker) = view.picker
        && let Some(position) = picker::render(frame, area, picker)
    {
        frame.set_cursor_position(position);
    }
    drawn
}

/// Which parts of the screen are drawn, from the top.
#[derive(Clone, Copy, Debug)]
struct Parts {
    logo: bool,
    /// The blank row under the logo.
    logo_gap: bool,
    version: bool,
    /// The blank row under the version.
    version_gap: bool,
    /// The row of `Enter`.
    enter: bool,
    /// The row of `.`.
    here: bool,
    /// How many numbered rows; while there are some, the heading and a
    /// blank row go above them.
    recent: usize,
}

impl Parts {
    /// How many rows the parts take.
    fn height(self) -> usize {
        let logo = if self.logo { strings::LOGO.len() } else { 0 };
        let recent = if self.recent > 0 { self.recent + 2 } else { 0 };
        let single = [
            self.logo_gap,
            self.version,
            self.version_gap,
            self.enter,
            self.here,
        ];
        logo + single.into_iter().filter(|&drawn| drawn).count() + recent
    }

    /// The parts that fit in `rows`: the gap under the logo goes first,
    /// then the logo, the numbered rows from the last, the gap under the
    /// version, the version, the row of `.` and the row of `Enter`.
    fn fit(mut self, rows: usize) -> Self {
        if self.height() > rows {
            self.logo_gap = false;
        }
        if self.height() > rows {
            self.logo = false;
        }
        while self.height() > rows && self.recent > 0 {
            self.recent -= 1;
        }
        if self.height() > rows {
            self.version_gap = false;
        }
        if self.height() > rows {
            self.version = false;
        }
        if self.height() > rows {
            self.here = false;
        }
        if self.height() > rows {
            self.enter = false;
        }
        self
    }
}

/// How a row's columns are shared after the key's: the label or the
/// project's name with the gap after it, then the detail, and on a
/// numbered row its account, flush right.
#[derive(Clone, Copy, Debug)]
struct Columns {
    label: usize,
    /// The detail's columns on the rows of `Enter` and `.`; none when it
    /// would be too narrow.
    top: usize,
    /// The detail's columns on a numbered row; none when it would be too
    /// narrow.
    detail: usize,
    /// The account's columns on a numbered row; none without accounts,
    /// and when the detail beside it would be too narrow.
    account: usize,
}

impl Columns {
    /// The columns of rows `width` columns wide: the label as wide as the
    /// widest of `labels`, up to [`LABEL_MAX`], and the account as the
    /// widest of `accounts` and a space.
    fn new(
        width: usize,
        labels: impl Iterator<Item = usize>,
        accounts: impl Iterator<Item = usize>,
    ) -> Self {
        let widest = labels.max().unwrap_or_default().min(LABEL_MAX);
        let label = (widest + LABEL_GAP).min(width.saturating_sub(KEY_WIDTH));
        let rest = width.saturating_sub(KEY_WIDTH + label);
        let mut account = accounts.max().map_or(0, |widest| widest + 1);
        if rest.saturating_sub(account) < DETAIL_BESIDE_ACCOUNT {
            account = 0;
        }
        let shown = |detail: usize| if detail < DETAIL_MIN { 0 } else { detail };
        Self {
            label,
            top: shown(rest),
            detail: shown(rest - account),
            account,
        }
    }
}

/// A row: `key` in bold, `label` in `width` columns, the gap after it
/// included, then `detail`.
fn row_line(key: &str, label: &str, width: usize, detail: Vec<Span<'static>>) -> Line<'static> {
    let label = cut(&printable(label), width.saturating_sub(LABEL_GAP));
    let mut spans = vec![
        Span::styled(key.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(KEY_WIDTH.saturating_sub(key.width()))),
        Span::raw(pad(&label, width)),
    ];
    spans.extend(detail);
    Line::from(spans)
}

/// The names of the projects of the last run in `room` columns, muted,
/// with faint separators; cut with `…` when they do not fit.
fn run_detail(names: &[String], room: usize) -> Vec<Span<'static>> {
    let palette = palette::current();
    let name = Style::new().fg(palette.muted);
    let separator = Style::new().fg(palette.faint);
    let pieces = names.iter().enumerate().flat_map(|(index, project)| {
        let before = (index > 0).then(|| (strings::HINT_SEPARATOR.to_owned(), separator));
        before.into_iter().chain([(printable(project), name)])
    });
    let mut room = room;
    let mut spans = Vec::new();
    for (text, style) in pieces {
        if room == 0 {
            break;
        }
        let shown = cut(&text, room);
        let short = shown != text;
        room -= shown.width();
        spans.push(Span::styled(shown, style));
        if short {
            break;
        }
    }
    spans
}

/// The numbered row of `item`, at `index` among the picker's rows: its
/// digit in bold, its name, its path muted, and its account, muted, or in
/// the alert colour when it is not among `accounts`.
fn recent_line(index: usize, item: &Item, columns: Columns, accounts: &[String]) -> Line<'static> {
    let palette = palette::current();
    let mut detail = Vec::new();
    if columns.detail > 0 {
        let path = pad(&fit_path(&item.path, columns.detail), columns.detail);
        detail.push(Span::styled(path, Style::new().fg(palette.muted)));
    }
    if columns.account > 0
        && let Some(account) = item.account.as_deref()
    {
        let colour = if accounts.iter().any(|declared| declared == account) {
            palette.muted
        } else {
            palette.alert
        };
        let text = format!(" {}", printable(account));
        let text = format!("{text:>width$}", width = columns.account);
        detail.push(Span::styled(text, Style::new().fg(colour)));
    }
    let digit = (index + 1).to_string();
    row_line(&digit, &item.name, columns.label, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::palette::{ACCENT, ALERT, FAINT, INK, MUTED};
    use crate::ui::toast::Kind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::Color;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const HOME: &str = "/Users/x";

    /// The 100x30 mockup, row by row, trailing blanks trimmed.
    const GRID_100_BY_30: [&str; 30] = [
        "",
        "",
        "",
        "",
        "                                ██                         ▀▀",
        "                                ██▀▀▀ ▄█▀█▄ ██▄█▀ ██▀██▀█▄ ██ ▄█▀▀▀",
        "                                ██    ██▀▀▀ ██    ██ ██ ██ ██  ▀▀█▄",
        "                                ▀█▄▄▄ ▀█▄▄▄ ██    ██ ██ ██ ██ ▄▄▄█▀",
        "",
        "                                           termis 0.10.0",
        "",
        "              Enter  reopen 3 sessions  termis · dictamen · api",
        "              .      open here          ~/WebProjects/Personal/termis",
        "",
        "              Recent ─────────────────────────────────────────────────────────────────",
        "              1      termis             ~/WebProjects/Personal/termis         personal",
        "              2      api                ~/WebProjects/Work/api                    work",
        "              3      dictamen           ~/WebProjects/Personal/dictamen       personal",
        "              4      web                ~/WebProjects/Work/web                    work",
        "              5      radar              ~/WebProjects/Personal/radar          personal",
        "              6      dotfiles           ~/dotfiles                            personal",
        "              7      infra              ~/WebProjects/Work/infra                  work",
        "              8      notes              ~/notes                               personal",
        "              9      blog               ~/WebProjects/Personal/blog           personal",
        "",
        "",
        "",
        "",
        "",
        " START   Enter reopen · . here · 1-9 recent · o open · q quit",
    ];

    /// The 80x24 mockup, row by row, trailing blanks trimmed.
    const GRID_80_BY_24: [&str; 24] = [
        "",
        "                      ██                         ▀▀",
        "                      ██▀▀▀ ▄█▀█▄ ██▄█▀ ██▀██▀█▄ ██ ▄█▀▀▀",
        "                      ██    ██▀▀▀ ██    ██ ██ ██ ██  ▀▀█▄",
        "                      ▀█▄▄▄ ▀█▄▄▄ ██    ██ ██ ██ ██ ▄▄▄█▀",
        "",
        "                                 termis 0.10.0",
        "",
        "    Enter  reopen 3 sessions  termis · dictamen · api",
        "    .      open here          ~/WebProjects/Personal/termis",
        "",
        "    Recent ─────────────────────────────────────────────────────────────────",
        "    1      termis             ~/WebProjects/Personal/termis         personal",
        "    2      api                ~/WebProjects/Work/api                    work",
        "    3      dictamen           ~/WebProjects/Personal/dictamen       personal",
        "    4      web                ~/WebProjects/Work/web                    work",
        "    5      radar              ~/WebProjects/Personal/radar          personal",
        "    6      dotfiles           ~/dotfiles                            personal",
        "    7      infra              ~/WebProjects/Work/infra                  work",
        "    8      notes              ~/notes                               personal",
        "    9      blog               ~/WebProjects/Personal/blog           personal",
        "",
        "",
        " START   Enter reopen · . here · 1-9 recent · o open · q quit",
    ];

    /// The 60x14 mockup, row by row, trailing blanks trimmed.
    const GRID_60_BY_14: [&str; 14] = [
        "                       termis 0.10.0",
        "",
        "  Enter  reopen 3 sessions  termis · dictamen · api",
        "  .      open here          ~/WebProjects/Personal/termis",
        "",
        "  Recent ─────────────────────────────────────────────────",
        "  1      termis             ~/…/Personal/termis   personal",
        "  2      api                ~/…/Work/api              work",
        "  3      dictamen           ~/…/Personal/dictamen personal",
        "  4      web                ~/…/Work/web              work",
        "  5      radar              ~/…/Personal/radar    personal",
        "  6      dotfiles           ~/dotfiles            personal",
        "  7      infra              ~/…/Work/infra            work",
        " START   Enter reopen · . here · o open · q quit",
    ];

    /// The 40x10 mockup, row by row, trailing blanks trimmed.
    const GRID_40_BY_10: [&str; 10] = [
        "             termis 0.10.0",
        "",
        "  Enter  reopen 3 sessions  termis · …",
        "  .      open here          ~/…/termis",
        "",
        "  Recent ─────────────────────────────",
        "  1      termis             ~/…/termis",
        "  2      api                ~/…/api",
        "  3      dictamen           dictamen",
        " START   Enter reopen · . here · q quit",
    ];

    /// A recent project in `dir` under the home, opening as `account`.
    fn recent(name: &str, dir: &str, account: Option<&str>) -> Item {
        let home = Path::new(HOME);
        Item::recent(name.into(), home.join(dir), Some(home))
            .with_account(account.map(str::to_owned))
    }

    /// The mockups' recent projects, the latest first, each opening as
    /// one of two accounts.
    fn list() -> Picker {
        let (personal, work) = (Some("personal"), Some("work"));
        Picker::new(
            vec![],
            vec![
                recent("termis", "WebProjects/Personal/termis", personal),
                recent("api", "WebProjects/Work/api", work),
                recent("dictamen", "WebProjects/Personal/dictamen", personal),
                recent("web", "WebProjects/Work/web", work),
                recent("radar", "WebProjects/Personal/radar", personal),
                recent("dotfiles", "dotfiles", personal),
                recent("infra", "WebProjects/Work/infra", work),
                recent("notes", "notes", personal),
                recent("blog", "WebProjects/Personal/blog", personal),
            ],
        )
        .with_accounts(vec!["personal".into(), "work".into()])
    }

    /// The mockups' last run.
    fn run() -> Vec<String> {
        ["termis", "dictamen", "api"].map(str::to_owned).to_vec()
    }

    /// The mockups' screen, over `run` and `list`.
    fn view<'a>(run: &'a [String], list: &'a Picker) -> View<'a> {
        View {
            version: "0.10.0",
            run,
            here: "~/WebProjects/Personal/termis",
            list,
            picker: None,
            notice: None,
            palette: Palette::DEFAULT,
        }
    }

    /// `view` drawn on a terminal of `width` by `height`, and the rows it
    /// returned.
    fn draw(width: u16, height: u16, view: &View<'_>) -> (Terminal<TestBackend>, Vec<(Rect, Row)>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut rows = Vec::new();
        terminal
            .draw(|frame| rows = render(frame, view))
            .expect("draw");
        (terminal, rows)
    }

    /// Row `y` of the screen, trailing blanks trimmed.
    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        text.trim_end().to_owned()
    }

    /// Checks the screen against `grid`, row by row.
    fn assert_grid(terminal: &Terminal<TestBackend>, grid: &[&str]) {
        let height = terminal.backend().buffer().area.height;
        assert_eq!(usize::from(height), grid.len());
        for (y, expected) in (0..height).zip(grid) {
            assert_eq!(row(terminal, y), *expected, "row {y}");
        }
    }

    /// The foreground and background of the cell at `x`, `y`, and whether
    /// it is bold.
    fn style(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> (Color, Color, bool) {
        let cell = terminal.backend().buffer().cell((x, y)).expect("cell");
        (cell.fg, cell.bg, cell.modifier.contains(Modifier::BOLD))
    }

    #[test]
    fn the_logo_is_four_rows_of_block_elements_35_columns_wide() {
        for (index, line) in strings::LOGO.into_iter().enumerate() {
            assert!(
                line.chars().all(|ch| matches!(ch, '█' | '▀' | '▄' | ' ')),
                "{line}"
            );
            let width = if index == 0 { 29 } else { 35 };
            assert_eq!(line.width(), width, "{line}");
        }
    }

    #[test]
    fn the_start_screen_at_100_by_30_draws_the_mockup() {
        let (run, list) = (run(), list());
        let (terminal, _) = draw(100, 30, &view(&run, &list));
        assert_grid(&terminal, &GRID_100_BY_30);
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn the_start_screen_at_80_by_24_draws_the_mockup() {
        let (run, list) = (run(), list());
        let (terminal, _) = draw(80, 24, &view(&run, &list));
        assert_grid(&terminal, &GRID_80_BY_24);
    }

    #[test]
    fn the_logo_is_in_the_accent_the_keys_bold_and_the_paths_muted() {
        let (run, list) = (run(), list());
        let (terminal, _) = draw(100, 30, &view(&run, &list));
        let plain = (Color::Reset, Color::Reset, false);
        let muted = (MUTED, Color::Reset, false);
        let faint = (FAINT, Color::Reset, false);
        assert_eq!(
            style(&terminal, 32, 4),
            (ACCENT, Color::Reset, false),
            "logo"
        );
        assert_eq!(
            style(&terminal, 66, 7),
            (ACCENT, Color::Reset, false),
            "logo"
        );
        assert_eq!(style(&terminal, 43, 9), muted, "the version");
        assert_eq!(
            style(&terminal, 14, 11),
            (Color::Reset, Color::Reset, true),
            "Enter"
        );
        assert_eq!(style(&terminal, 21, 11), plain, "its label");
        assert_eq!(style(&terminal, 40, 11), muted, "a name of the run");
        assert_eq!(style(&terminal, 47, 11), faint, "a separator");
        assert_eq!(
            style(&terminal, 14, 12),
            (Color::Reset, Color::Reset, true),
            "."
        );
        assert_eq!(style(&terminal, 40, 12), muted, "the current directory");
        assert_eq!(style(&terminal, 14, 14), muted, "the heading");
        assert_eq!(style(&terminal, 21, 14), faint, "its rule");
        assert_eq!(
            style(&terminal, 14, 15),
            (Color::Reset, Color::Reset, true),
            "1"
        );
        assert_eq!(style(&terminal, 21, 15), plain, "a name");
        assert_eq!(style(&terminal, 40, 15), muted, "its path");
        assert_eq!(style(&terminal, 78, 15), muted, "its account");
        // Only the tag has a background: ink on the accent, bold.
        assert_eq!(style(&terminal, 1, 29), (INK, ACCENT, true), "START");
        assert_eq!(
            style(&terminal, 9, 29),
            (Color::Reset, Color::Reset, true),
            "a key"
        );
        assert_eq!(style(&terminal, 15, 29), muted, "what it does");
        let buffer = terminal.backend().buffer();
        let backgrounds = (0..30u16)
            .flat_map(|y| (0..100u16).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.bg != Color::Reset)
            })
            .count();
        assert_eq!(backgrounds, " START ".len());
    }

    #[test]
    fn without_a_last_run_enter_opens_here_and_the_dot_row_goes() {
        let list = list();
        let (terminal, rows) = draw(100, 30, &view(&[], &list));
        let (enter, row_of) = rows[0];
        assert_eq!(row_of, Row::Here);
        assert_eq!(
            row(&terminal, enter.y),
            "              Enter  open here  ~/WebProjects/Personal/termis"
        );
        assert_eq!(row(&terminal, enter.y + 1), "");
        assert!(row(&terminal, enter.y + 2).starts_with("              Recent ─"));
        let first = format!(
            "              1      termis     ~/WebProjects/Personal/termis{}personal",
            " ".repeat(17)
        );
        assert_eq!(row(&terminal, enter.y + 3), first);
        assert!(!rows[1..].iter().any(|(_, row)| *row == Row::Here));
        assert_eq!(
            row(&terminal, 29),
            " START   Enter here · 1-9 recent · o open · q quit"
        );
        assert_eq!(style(&terminal, 32, enter.y), (MUTED, Color::Reset, false));
    }

    #[test]
    fn with_one_account_the_rows_name_no_account() {
        let run = run();
        let home = Path::new(HOME);
        let list = Picker::new(
            vec![],
            vec![
                Item::recent(
                    "termis".into(),
                    home.join("WebProjects/Personal/termis"),
                    Some(home),
                ),
                Item::recent("api".into(), home.join("WebProjects/Work/api"), Some(home)),
            ],
        )
        .with_accounts(vec!["default".into()]);
        let (terminal, rows) = draw(100, 30, &view(&run, &list));
        let first = rows.iter().find(|(_, row)| *row == Row::Recent(0));
        let (first, _) = first.expect("drawn");
        assert_eq!(
            row(&terminal, first.y),
            "              1      termis             ~/WebProjects/Personal/termis"
        );
        assert_eq!(
            row(&terminal, first.y + 1),
            "              2      api                ~/WebProjects/Work/api"
        );
    }

    #[test]
    fn an_account_the_config_file_does_not_declare_is_in_the_alert_colour() {
        let run = run();
        let home = Path::new(HOME);
        let list = Picker::new(
            vec![],
            vec![
                recent("termis", "WebProjects/Personal/termis", Some("personal")),
                Item::recent("notes".into(), home.join("notes"), Some(home))
                    .with_account(Some("old".into())),
            ],
        )
        .with_accounts(vec!["personal".into(), "work".into()]);
        let (terminal, rows) = draw(100, 30, &view(&run, &list));
        let (notes, _) = rows[3];
        assert_eq!(rows[3].1, Row::Recent(1));
        let end = format!("~/notes{}old", " ".repeat(36));
        assert!(row(&terminal, notes.y).ends_with(&end));
        assert_eq!(style(&terminal, 83, notes.y), (ALERT, Color::Reset, false));
        assert_eq!(
            style(&terminal, 78, notes.y - 1),
            (MUTED, Color::Reset, false)
        );
    }

    #[test]
    fn a_short_terminal_drops_the_logo_then_the_last_recent_rows() {
        let (run, list) = (run(), list());
        let (terminal, _) = draw(60, 14, &view(&run, &list));
        assert_grid(&terminal, &GRID_60_BY_14);
        // One row more shows one more recent project; the logo needs four.
        let (terminal, _) = draw(60, 15, &view(&run, &list));
        assert!(row(&terminal, 13).starts_with("  8      notes"));
        assert!(!(0..15).any(|y| row(&terminal, y).contains('█')));
    }

    #[test]
    fn a_narrow_terminal_shortens_the_paths_then_drops_the_accounts() {
        let (run, list) = (run(), list());
        let (terminal, _) = draw(40, 10, &view(&run, &list));
        assert_grid(&terminal, &GRID_40_BY_10);
        // Below 39 columns the logo goes, whatever the height.
        let (terminal, _) = draw(39, 30, &view(&run, &list));
        assert!((0..30).any(|y| row(&terminal, y).contains('█')));
        let (terminal, _) = draw(38, 30, &view(&run, &list));
        assert!(!(0..30).any(|y| row(&terminal, y).contains('█')));
        // A detail of 8 columns stays; under 8 it goes.
        let (terminal, _) = draw(38, 30, &view(&run, &list));
        let rows: Vec<String> = (0..30).map(|y| row(&terminal, y)).collect();
        let first = "  1      termis             termis".to_owned();
        assert!(rows.contains(&first), "{rows:#?}");
        let (terminal, _) = draw(37, 30, &view(&run, &list));
        let rows: Vec<String> = (0..30).map(|y| row(&terminal, y)).collect();
        assert!(rows.contains(&"  1      termis".to_owned()), "{rows:#?}");
        let enter = "  Enter  reopen 3 sessions".to_owned();
        assert!(rows.contains(&enter), "{rows:#?}");
    }

    #[test]
    fn every_size_up_to_100_by_30_draws_without_a_panic() {
        let (run, list) = (run(), list());
        let none = Picker::default();
        let notice = Toast::new("x".repeat(200), Kind::Error, Instant::now(), Duration::ZERO);
        let views = [
            view(&run, &list),
            view(&[], &none),
            View {
                picker: Some(&list),
                notice: Some(&notice),
                ..view(&run, &list)
            },
        ];
        for view in &views {
            let _ = draw(0, 0, view);
            for width in 1..=100 {
                for height in 1..=30 {
                    let (_, rows) = draw(width, height, view);
                    let area = Rect::new(0, 0, width, height - 1);
                    for (rect, _) in rows {
                        assert_eq!(rect.intersection(area), rect, "{width}x{height}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_rows_drawn_are_returned_with_what_they_open() {
        let (run, list) = (run(), list());
        let at = |y| Rect::new(14, y, 72, 1);
        let (_, rows) = draw(100, 30, &view(&run, &list));
        let mut expected = vec![(at(11), Row::Run), (at(12), Row::Here)];
        expected.extend((0..9).map(|index| (at(15 + index), Row::Recent(usize::from(index)))));
        assert_eq!(rows, expected);
        // The rows a short terminal drops are not returned.
        let (_, rows) = draw(60, 14, &view(&run, &list));
        assert_eq!(rows.len(), 2 + 7);
        assert_eq!(
            rows.last(),
            Some(&(Rect::new(2, 12, 56, 1), Row::Recent(6)))
        );
        let (_, rows) = draw(100, 3, &view(&run, &list));
        assert_eq!(rows, [(at(0), Row::Run), (at(1), Row::Here)]);
        let (_, rows) = draw(100, 2, &view(&run, &list));
        assert_eq!(rows, [(at(0), Row::Run)]);
        let (terminal, rows) = draw(100, 1, &view(&run, &list));
        assert!(rows.is_empty());
        assert!(row(&terminal, 0).starts_with(" START"));
        // Without a last run the row of `Enter` opens the current
        // directory.
        let (_, rows) = draw(100, 30, &view(&[], &list));
        assert_eq!(rows[0].1, Row::Here);
        assert_eq!(rows[1].1, Row::Recent(0));
    }

    #[test]
    fn a_notice_takes_the_place_of_the_hints_in_the_alert_colour() {
        let (run, list) = (run(), list());
        let text = "notes: opened last as account old, which the config file does not declare";
        let notice = Toast::new(text.into(), Kind::Error, Instant::now(), Duration::ZERO);
        let view = View {
            notice: Some(&notice),
            ..view(&run, &list)
        };
        let (terminal, _) = draw(100, 30, &view);
        assert_eq!(row(&terminal, 29), format!(" START   {text}"));
        assert_eq!(style(&terminal, 9, 29), (ALERT, Color::Reset, false));
        // It stays whole on screen whatever its time, and is cut to the bar.
        let (terminal, _) = draw(40, 10, &view);
        assert_eq!(
            row(&terminal, 9),
            " START   notes: opened last as account …"
        );
    }

    #[test]
    fn the_hints_drop_1_9_then_o_as_the_bar_narrows() {
        let (run, list) = (run(), list());
        for (width, hints) in [
            (61, "Enter reopen · . here · 1-9 recent · o open · q quit"),
            (60, "Enter reopen · . here · o open · q quit"),
            (48, "Enter reopen · . here · o open · q quit"),
            (47, "Enter reopen · . here · q quit"),
        ] {
            let (terminal, _) = draw(width, 30, &view(&run, &list));
            assert_eq!(row(&terminal, 29), format!(" START   {hints}"), "{width}");
        }
        // Without the numbered rows the digits are not offered.
        let (terminal, rows) = draw(100, 5, &view(&run, &list));
        assert!(!rows.iter().any(|(_, row)| matches!(row, Row::Recent(_))));
        assert_eq!(
            row(&terminal, 4),
            " START   Enter reopen · . here · o open · q quit"
        );
    }

    #[test]
    fn the_picker_draws_over_the_start_screen_with_open_in_the_bar() {
        let (run, list) = (run(), list());
        let mut picker = list.clone();
        picker.insert("ap");
        let view = View {
            picker: Some(&picker),
            ..view(&run, &list)
        };
        let (mut terminal, _) = draw(100, 30, &view);
        // Fourteen rows high, the accounts under the query, centred.
        assert!(
            row(&terminal, 8).contains("╭─ open a project ─"),
            "{}",
            row(&terminal, 8)
        );
        assert!(
            row(&terminal, 9).contains("│ › ap "),
            "{}",
            row(&terminal, 9)
        );
        assert!(row(&terminal, 10).contains("account  personal  work"));
        assert!(
            row(&terminal, 12).contains("▸ api "),
            "{}",
            row(&terminal, 12)
        );
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 20, y: 9 }
        );
        assert_eq!(
            row(&terminal, 29),
            " OPEN   Enter open · Tab new conversation · Up/Down move · Left/Right account · Esc back"
        );
        assert_eq!(style(&terminal, 1, 29), (INK, ACCENT, true));
        // The screen stays around the box.
        assert_eq!(row(&terminal, 4), GRID_100_BY_30[4]);
    }
}
