//! Drawing: the three panes, the status bar, and the full-screen view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use tui_term::widget::Cursor;

use crate::emu::Emulator;
use crate::git::worker::Snapshot;
use crate::strings;

pub mod layout;

/// Colour of the focused pane's border and title, and of key hints.
/// Truecolour, like the theme the emulator reports to the agent; a
/// 256-colour multiplexer approximates it. Becomes configurable with the
/// theme, after 0.1.
const ACCENT: Color = Color::Rgb(0x62, 0xc3, 0xb1);
/// Colour of the borders of the panes without focus.
const DIM: Color = Color::DarkGray;

/// One of the three panes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    Projects,
    #[default]
    Agent,
    Changes,
}

/// Where keys go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// To the agent, as typed.
    #[default]
    Agent,
    /// To term's own commands, with the pane they act on highlighted.
    Term(Pane),
}

impl Mode {
    /// The highlighted pane: the agent's while keys go to it.
    pub fn pane(self) -> Pane {
        match self {
            Self::Agent => Pane::Agent,
            Self::Term(pane) => pane,
        }
    }
}

/// What the three-pane view shows.
pub struct View<'a> {
    /// Name of the agent in the center pane title.
    pub agent: &'a str,
    /// Name of the project in the center pane title and the status bar.
    pub project: &'a str,
    pub emulator: &'a Emulator,
    /// Where keys go, with the highlighted pane. The app keeps the
    /// highlight on the agent's pane while `side_panes` is false.
    pub mode: Mode,
    /// Whether the side panes are on screen.
    pub side_panes: bool,
    /// Whether the side panes are hidden by the zoom rather than by the
    /// width.
    pub zoomed: bool,
    /// The line to draw over the agent's screen once it ended.
    pub ended: Option<&'a str>,
    /// Whether the directory is inside a git repository.
    pub repo: bool,
    /// The repository as last read, once the worker read it.
    pub changes: Option<&'a Snapshot>,
}

/// Where the agent's screen goes for a terminal of `area`: the whole area
/// when `full_screen`, else inside the center pane's border, which spans
/// the width when the side panes are not shown.
pub fn agent_area(area: Rect, full_screen: bool, side_panes: bool) -> Rect {
    if full_screen {
        area
    } else {
        pane_block("", false).inner(layout::panes_with(area, side_panes).center)
    }
}

/// The border every pane uses, highlighted when focused; the interior it
/// leaves is the agent's screen in the center.
fn pane_block(title: &str, focused: bool) -> Block<'static> {
    if focused {
        let accent = Style::new().fg(ACCENT);
        Block::bordered()
            .border_style(accent)
            .title_style(accent)
            .title(format!(" ● {title} "))
    } else {
        Block::bordered()
            .border_style(Style::new().fg(DIM))
            .title(format!(" {title} "))
    }
}

/// Draws the agent over the whole frame, with the cursor where the agent
/// has it while keys go to it. The last row shows the term-mode tag and
/// commands while that mode is on, else the `ended` line once the agent
/// ended.
pub fn render_full(frame: &mut Frame, emulator: &Emulator, ended: Option<&str>, mode: Mode) {
    let area = frame.area();
    render_agent(
        frame,
        area,
        emulator,
        mode == Mode::Agent && ended.is_none(),
    );
    match mode {
        Mode::Term(_) => {
            let keys = mode_keys(mode, "", false, strings::Zoom::None, ended.is_some());
            render_last_row(frame, area, keys, Style::new());
        }
        Mode::Agent => render_ended(frame, area, ended),
    }
}

/// Draws the three panes and the status bar.
pub fn render(frame: &mut Frame, view: &View<'_>) {
    let panes = layout::panes_with(frame.area(), view.side_panes);
    let focus = if view.side_panes {
        view.mode.pane()
    } else {
        Pane::Agent
    };

    if view.side_panes {
        let projects = pane_block(strings::PANE_PROJECTS, focus == Pane::Projects);
        let changes = pane_block(strings::PANE_CHANGES, focus == Pane::Changes);
        let changes_inner = changes.inner(panes.right);
        frame.render_widget(projects, panes.left);
        frame.render_widget(changes, panes.right);
        if !view.repo {
            render_empty(frame, changes_inner, strings::NOT_A_REPOSITORY);
        }
    }

    let title = strings::agent_pane_title(view.agent, view.project);
    let center = pane_block(&title, focus == Pane::Agent);
    let inner = center.inner(panes.center);
    frame.render_widget(center, panes.center);
    render_agent(
        frame,
        inner,
        view.emulator,
        view.mode == Mode::Agent && view.ended.is_none(),
    );
    render_ended(frame, inner, view.ended);

    // The mode tag and its keys on the left, the project flush right.
    // Unzoom is offered only when the width would show the sides again.
    let zoom = if view.zoomed && layout::sides_visible(frame.area()) {
        strings::Zoom::Out
    } else if view.side_panes {
        strings::Zoom::In
    } else {
        strings::Zoom::None
    };
    let keys = mode_keys(
        view.mode,
        view.agent,
        view.side_panes,
        zoom,
        view.ended.is_some(),
    );
    let project = Line::from(strings::status_project(view.project)).right_aligned();
    let width = u16::try_from(project.width()).unwrap_or(u16::MAX);
    let [left, right] = Layout::horizontal([Constraint::Min(0), Constraint::Length(width)])
        .spacing(1)
        .areas(panes.status);
    frame.render_widget(Paragraph::new(keys), left);
    frame.render_widget(Paragraph::new(project), right);
}

/// The mode tag followed by the keys that apply: the agent's name in a dim
/// block and how to enter term mode, or `TERM` in an accent block and the
/// commands.
fn mode_keys(
    mode: Mode,
    agent: &str,
    side_panes: bool,
    zoom: strings::Zoom,
    ended: bool,
) -> Line<'static> {
    let (tag, tag_style, hints) = match mode {
        Mode::Agent => (agent, Style::new().bg(DIM), strings::agent_hints(ended)),
        Mode::Term(_) => (
            strings::MODE_TERM,
            Style::new().fg(ACCENT).reversed(),
            strings::term_hints(side_panes, zoom, ended),
        ),
    };
    Line::from(vec![
        Span::styled(strings::mode_tag(tag), tag_style),
        Span::styled(format!("  {hints}"), Style::new().fg(ACCENT)),
    ])
}

/// Draws the agent's screen in `area`. The host cursor follows the agent's
/// only while `cursor`; the screen keeps updating either way.
fn render_agent(frame: &mut Frame, area: Rect, emulator: &Emulator, cursor: bool) {
    let widget = emulator
        .widget()
        .cursor(Cursor::default().visibility(false));
    frame.render_widget(widget, area);
    if cursor && let Some(position) = cursor_position(area, emulator.cursor()) {
        frame.set_cursor_position(position);
    }
}

/// Draws `text` dimmed and centred in an otherwise empty pane.
fn render_empty(frame: &mut Frame, area: Rect, text: &str) {
    if area.height == 0 {
        return;
    }
    let middle = Rect::new(area.x, area.y + area.height / 2, area.width, 1);
    let line = Line::from(text).centered().style(Style::new().fg(DIM));
    frame.render_widget(Paragraph::new(line), middle);
}

/// Draws `ended`, if any, as a reversed bar over the last row of the
/// agent's area.
fn render_ended(frame: &mut Frame, area: Rect, ended: Option<&str>) {
    if let Some(text) = ended {
        let bar = Style::new().fg(ACCENT).reversed();
        render_last_row(frame, area, Line::from(text), bar);
    }
}

/// Draws `line` over the last row of `area`, cleared first, with `style`
/// for the whole row.
fn render_last_row(frame: &mut Frame, area: Rect, line: Line<'_>, style: Style) {
    if area.height == 0 {
        return;
    }
    let last = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Clear, last);
    frame.render_widget(Paragraph::new(line).style(style), last);
}

/// Where the host cursor goes for an agent cursor at (row, column) inside
/// `area`; none when the agent hides it or it falls outside the area.
fn cursor_position(area: Rect, cursor: Option<(u16, u16)>) -> Option<Position> {
    let (row, col) = cursor?;
    let x = area.x.saturating_add(col);
    let y = area.y.saturating_add(row);
    (x < area.right() && y < area.bottom()).then_some(Position { x, y })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Size;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

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

    fn bg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.bg)
    }

    fn reversed(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> bool {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::REVERSED))
    }

    fn view(emulator: &Emulator, mode: Mode) -> View<'_> {
        View {
            agent: "claude",
            project: "term",
            emulator,
            mode,
            side_panes: true,
            zoomed: false,
            ended: None,
            repo: true,
            changes: None,
        }
    }

    fn draw(width: u16, height: u16, view: &View<'_>) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| render(frame, view)).expect("draw");
        terminal
    }

    #[test]
    fn draws_titled_panes_and_the_status_bar() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let terminal = draw(100, 10, &view(&emulator, Mode::Agent));
        let panes = layout::panes(Rect::new(0, 0, 100, 10));

        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ Projects ─"), "{top}");
        assert!(top.contains("┌ ● claude · term ─"), "{top}");
        assert!(top.contains("┌ Changes ─"), "{top}");
        assert_eq!(top.chars().count(), 100);
        assert_eq!(fg(&terminal, panes.center.x, 0), ACCENT, "center border");
        assert_eq!(fg(&terminal, panes.left.x, 0), DIM, "left border");
        assert_eq!(fg(&terminal, panes.right.x, 0), DIM, "right border");

        let second = row(&terminal, 1);
        assert!(second.starts_with('│'), "{second}");
        assert!(second.contains("│hello"), "{second}");

        let bottom = row(&terminal, 8);
        assert!(bottom.starts_with('└'), "{bottom}");
        let status = row(&terminal, 9);
        assert!(status.starts_with(" claude   Ctrl+Q term mode"), "{status}");
        assert!(status.ends_with("term · term "), "{status}");
        // The agent's name is a dim block; the hints are in the accent.
        assert_eq!(bg(&terminal, 1, 9), DIM);
        assert!(!reversed(&terminal, 1, 9));
        assert_eq!(fg(&terminal, 10, 9), ACCENT);
        assert_eq!(bg(&terminal, 10, 9), Color::Reset);
    }

    #[test]
    fn term_mode_shows_its_tag_and_commands_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" TERM   1/2/3 pane · z zoom · q quit · Esc back"),
            "{status}"
        );
        assert!(status.ends_with("term · term "), "{status}");
        // The tag is an accent block; the hints are plain accent.
        assert_eq!(fg(&terminal, 1, 9), ACCENT);
        assert!(reversed(&terminal, 1, 9));
        assert_eq!(fg(&terminal, 8, 9), ACCENT);
        assert!(!reversed(&terminal, 8, 9));
        // The agent's pane stays highlighted, but keys do not reach it.
        assert!(row(&terminal, 0).contains("┌ ● claude · term ─"));
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn zoomed_view_hides_the_sides_and_offers_unzoom() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.side_panes = false;
        v.zoomed = true;
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ ● claude · term ─"), "{top}");
        assert!(top.ends_with('┐') && !top.contains("Projects"), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        let status = row(&terminal, 9);
        assert!(status.starts_with(" claude   Ctrl+Q term mode"), "{status}");
        v.mode = Mode::Term(Pane::Agent);
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.contains("z unzoom · q quit"), "{status}");
        assert!(!status.contains("1/2/3"), "{status}");
    }

    #[test]
    fn narrow_zoomed_view_offers_no_zoom_key() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.side_panes = false;
        v.zoomed = true;
        let status = row(&draw(80, 10, &v), 9);
        assert!(!status.contains("zoom"), "{status}");
        assert!(status.contains("q quit"), "{status}");
    }

    #[test]
    fn highlighted_pane_follows_the_mode() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Changes)));
        let top = row(&terminal, 0);
        assert!(top.contains("┌ ● Changes ─"), "{top}");
        assert!(top.contains("┌ claude · term ─"), "{top}");
        assert_eq!(fg(&terminal, panes.right.x, 0), ACCENT, "right border");
        assert_eq!(fg(&terminal, panes.center.x, 0), DIM, "center border");
        assert_eq!(fg(&terminal, panes.left.x, 0), DIM, "left border");
        assert!(row(&terminal, 9).starts_with(" TERM "));

        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Projects)));
        assert!(row(&terminal, 0).starts_with("┌ ● Projects ─"));
        assert_eq!(fg(&terminal, panes.left.x, 0), ACCENT);
        assert_eq!(fg(&terminal, panes.center.x, 0), DIM);
    }

    #[test]
    fn status_bar_keeps_a_gap_before_the_project() {
        const PROJECT: &str = "a-project-name-long-enough-to-push-the-hints-off-the-bar";
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Projects));
        v.project = PROJECT;
        let status = row(&draw(100, 10, &v), 9);
        // The keys are cut at the left area, which ends one blank column
        // before the right-aligned project.
        let project = strings::status_project(PROJECT);
        let left_width = 100 - project.chars().count() - 1;
        let full_keys = format!(
            "{}  {}",
            strings::mode_tag(strings::MODE_TERM),
            strings::term_hints(true, strings::Zoom::In, false)
        );
        assert!(full_keys.chars().count() > left_width, "the keys overflow");
        let cut: String = full_keys.chars().take(left_width).collect();
        assert_eq!(status, format!("{cut} {project}"));
    }

    #[test]
    fn cursor_follows_the_agent_only_in_agent_mode() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut terminal = draw(100, 10, &view(&emulator, Mode::Agent));
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 23, y: 1 }
        );
        for mode in [Mode::Term(Pane::Agent), Mode::Term(Pane::Changes)] {
            let terminal = draw(100, 10, &view(&emulator, mode));
            assert!(
                !terminal.backend().cursor_visible(),
                "cursor hidden in term mode ({mode:?})"
            );
        }
    }

    #[test]
    fn changes_pane_says_when_there_is_no_repository() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Agent);
        v.repo = false;
        let terminal = draw(100, 10, &v);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let inner = pane_block("", false).inner(panes.right);
        let y = inner.y + inner.height / 2;
        let middle = row(&terminal, y);
        assert!(middle.contains("not a git repository"), "{middle}");
        let x = middle.find("not a git").expect("text") as u16;
        assert!(x > inner.x && x < inner.right(), "inside the right pane");
        assert_eq!(fg(&terminal, x, y), DIM);

        v.repo = true;
        let terminal = draw(100, 10, &v);
        for y in 0..10 {
            assert!(!row(&terminal, y).contains("not a git repository"));
        }
    }

    #[test]
    fn agent_area_is_the_center_interior() {
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(agent_area(area, true, true), area);
        assert_eq!(agent_area(area, true, false), area);
        assert_eq!(agent_area(area, false, true), Rect::new(21, 1, 53, 37));
        // Without the sides, zoomed or collapsed: the whole width inside one
        // border.
        assert_eq!(agent_area(area, false, false), Rect::new(1, 1, 98, 37));
        let narrow = Rect::new(0, 0, 80, 40);
        assert_eq!(agent_area(narrow, false, false), Rect::new(1, 1, 78, 37));
    }

    #[test]
    fn collapsed_view_shows_only_the_agent_pane() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 80, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.side_panes = false;
        let terminal = draw(80, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ ● claude · term ─"), "{top}");
        assert!(
            !top.contains("Projects") && !top.contains("Changes"),
            "{top}"
        );
        assert!(top.ends_with('┐'), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        let status = row(&terminal, 9);
        assert!(status.starts_with(" claude   Ctrl+Q term mode"), "{status}");
        assert!(status.ends_with("term · term "));
    }

    #[test]
    fn collapsed_bar_never_offers_the_pane_digits() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
            let mut v = view(&emulator, mode);
            v.side_panes = false;
            let status = row(&draw(80, 10, &v), 9);
            assert!(!status.contains("1/2/3"), "{status}");
        }
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.side_panes = false;
        let status = row(&draw(80, 10, &v), 9);
        assert!(status.starts_with(" TERM   q quit · Esc back"), "{status}");
    }

    #[test]
    fn full_screen_puts_the_cursor_where_the_agent_has_it() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 20 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, None, Mode::Agent))
            .expect("draw");
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 2, y: 0 }
        );
    }

    #[test]
    fn full_screen_shows_term_mode_on_the_last_row_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 40 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, None, Mode::Term(Pane::Agent)))
            .expect("draw");
        assert!(!terminal.backend().cursor_visible());
        let last = row(&terminal, 3);
        assert!(last.starts_with(" TERM   q quit · Esc back"), "{last}");
        assert_eq!(fg(&terminal, 1, 3), ACCENT);
        assert!(reversed(&terminal, 1, 3));
        // Once the agent ended, term mode keeps its row and offers the
        // relaunch; agent mode shows the ended line instead.
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended "), Mode::Term(Pane::Agent)))
            .expect("draw");
        let last = row(&terminal, 3);
        assert!(
            last.starts_with(" TERM   r relaunch · q quit · Esc back"),
            "{last}"
        );
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended "), Mode::Agent))
            .expect("draw");
        assert!(row(&terminal, 3).starts_with(" ended "));
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn cursor_stays_inside_the_area() {
        let area = Rect::new(21, 1, 53, 37);
        assert_eq!(cursor_position(area, None), None);
        assert_eq!(
            cursor_position(area, Some((0, 0))),
            Some(Position { x: 21, y: 1 })
        );
        assert_eq!(
            cursor_position(area, Some((36, 52))),
            Some(Position { x: 73, y: 37 })
        );
        assert_eq!(cursor_position(area, Some((37, 0))), None);
        assert_eq!(cursor_position(area, Some((0, 53))), None);
        assert_eq!(cursor_position(Rect::new(21, 1, 0, 0), Some((0, 0))), None);
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(0u16, 0u16), (1, 1), (2, 2), (3, 3), (5, 2)] {
            let mut emulator = Emulator::new(Size { rows: 1, cols: 1 }, 0);
            emulator.process(b"x");
            let mut v = view(&emulator, Mode::Term(Pane::Agent));
            v.side_panes = false;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
            terminal.draw(|frame| render(frame, &v)).expect("draw");
            for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
                terminal
                    .draw(|frame| render_full(frame, &emulator, Some("x"), mode))
                    .expect("draw");
            }
        }
    }

    #[test]
    fn ended_line_covers_the_agent_bottom_row_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut v = view(&emulator, Mode::Agent);
        v.ended = Some(" claude exited (code 0) · r relaunch · q quit ");
        let terminal = draw(100, 10, &v);
        // The agent's last row is the one above the bottom border.
        let bottom = row(&terminal, 7);
        assert!(row(&terminal, 8).starts_with('└'));
        assert!(
            bottom.contains("claude exited (code 0) · r relaunch · q quit"),
            "{bottom}"
        );
        assert!(!terminal.backend().cursor_visible());
        // The whole row is one reversed bar, past the text too.
        let past = terminal.backend().buffer().cell((70, 7)).expect("cell");
        assert!(past.modifier.contains(Modifier::REVERSED));
        assert_eq!(past.fg, ACCENT);
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   r relaunch · q quit · Ctrl+Q term mode"),
            "{status}"
        );

        // In term mode the line stays, the cursor stays hidden and the bar
        // offers the relaunch among the commands.
        let mut side = view(&emulator, Mode::Term(Pane::Changes));
        side.ended = v.ended;
        let terminal = draw(100, 10, &side);
        assert!(row(&terminal, 7).contains("r relaunch"));
        let status = row(&terminal, 9);
        assert!(status.starts_with(" TERM "), "{status}");
        assert!(
            status.contains("z zoom · r relaunch · q quit · Esc back"),
            "{status}"
        );
        assert!(!terminal.backend().cursor_visible());

        let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended "), Mode::Agent))
            .expect("draw");
        assert!(row(&terminal, 2).starts_with(" ended "));
        assert!(!terminal.backend().cursor_visible());
    }
}
