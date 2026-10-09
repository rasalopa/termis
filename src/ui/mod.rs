//! Drawing: the three panes, the status bar, and the full-screen view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use tui_term::widget::Cursor;

use crate::emu::Emulator;
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

/// What the three-pane view shows.
pub struct View<'a> {
    /// Name of the agent in the center pane title.
    pub agent: &'a str,
    /// Name of the project in the center pane title and the status bar.
    pub project: &'a str,
    pub emulator: &'a Emulator,
    /// The pane that receives keys. Always [`Pane::Agent`] while
    /// `side_panes` is false; the app enforces it and the renderer assumes
    /// it.
    pub focus: Pane,
    /// Whether the side panes are on screen.
    pub side_panes: bool,
    /// Whether the side panes are hidden by the zoom rather than by the
    /// width.
    pub zoomed: bool,
    /// Whether `Ctrl+Q` was pressed and the next key is awaited.
    pub prefix_pending: bool,
    /// The line to draw over the agent's screen once it ended.
    pub ended: Option<&'a str>,
    /// Whether the directory is inside a git repository.
    pub repo: bool,
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
/// has it, and the `ended` line at the bottom once it ended.
pub fn render_full(frame: &mut Frame, emulator: &Emulator, ended: Option<&str>) {
    let area = frame.area();
    render_agent(frame, area, emulator, ended.is_none());
    render_ended(frame, area, ended);
}

/// Draws the three panes and the status bar.
pub fn render(frame: &mut Frame, view: &View<'_>) {
    let panes = layout::panes_with(frame.area(), view.side_panes);
    let focus = if view.side_panes {
        view.focus
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
        focus == Pane::Agent && view.ended.is_none(),
    );
    render_ended(frame, inner, view.ended);

    // Keys on the left, the project flush right.
    // Unzoom is offered only when the width would show the sides again.
    let zoom = if view.zoomed && layout::sides_visible(frame.area()) {
        strings::Zoom::Out
    } else if view.side_panes {
        strings::Zoom::In
    } else {
        strings::Zoom::None
    };
    let keys = if view.prefix_pending {
        strings::status_prefix_pending(view.side_panes, zoom, focus == Pane::Agent)
    } else if view.ended.is_some() && focus == Pane::Agent {
        strings::status_ended(view.side_panes, zoom)
    } else {
        match focus {
            Pane::Agent => strings::status_agent(view.agent, view.side_panes, zoom),
            Pane::Projects => strings::status_side(strings::PANE_PROJECTS),
            Pane::Changes => strings::status_side(strings::PANE_CHANGES),
        }
    };
    let project = Line::from(strings::status_project(view.project)).right_aligned();
    let width = u16::try_from(project.width()).unwrap_or(u16::MAX);
    let [left, right] = Layout::horizontal([Constraint::Min(0), Constraint::Length(width)])
        .spacing(1)
        .areas(panes.status);
    frame.render_widget(Paragraph::new(keys).style(Style::new().fg(ACCENT)), left);
    frame.render_widget(Paragraph::new(project), right);
}

/// Draws the agent's screen in `area`. The host cursor follows the agent's
/// only while `focused`; the screen keeps updating either way.
fn render_agent(frame: &mut Frame, area: Rect, emulator: &Emulator, focused: bool) {
    let widget = emulator
        .widget()
        .cursor(Cursor::default().visibility(false));
    frame.render_widget(widget, area);
    if focused && let Some(position) = cursor_position(area, emulator.cursor()) {
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

/// Draws `ended`, if any, over the last row of the agent's area.
fn render_ended(frame: &mut Frame, area: Rect, ended: Option<&str>) {
    let Some(text) = ended else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let last = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Clear, last);
    let bar = Paragraph::new(text).style(Style::new().fg(ACCENT).reversed());
    frame.render_widget(bar, last);
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

    fn view<'a>(emulator: &'a Emulator, focus: Pane, prefix_pending: bool) -> View<'a> {
        View {
            agent: "claude",
            project: "term",
            emulator,
            focus,
            side_panes: true,
            zoomed: false,
            prefix_pending,
            ended: None,
            repo: true,
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
        let terminal = draw(100, 10, &view(&emulator, Pane::Agent, false));
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
        assert!(status.starts_with(" focus: claude"), "{status}");
        assert!(
            status.contains("Ctrl+Q then 1/2/3 pane · z zoom · q quit"),
            "{status}"
        );
        assert!(status.ends_with("term · term "), "{status}");
        assert_eq!(fg(&terminal, 1, 9), ACCENT);
    }

    #[test]
    fn zoomed_view_hides_the_sides_and_offers_unzoom() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let mut v = view(&emulator, Pane::Agent, false);
        v.side_panes = false;
        v.zoomed = true;
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ ● claude · term ─"), "{top}");
        assert!(top.ends_with('┐') && !top.contains("Projects"), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        let status = row(&terminal, 9);
        assert!(status.contains("Ctrl+Q then z unzoom · q quit"), "{status}");
        assert!(!status.contains("1/2/3"), "{status}");
        v.prefix_pending = true;
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.contains("z unzoom"), "{status}");
    }

    #[test]
    fn narrow_zoomed_view_offers_no_zoom_key() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        let mut v = view(&emulator, Pane::Agent, false);
        v.side_panes = false;
        v.zoomed = true;
        let status = row(&draw(80, 10, &v), 9);
        assert!(!status.contains("zoom"), "{status}");
        assert!(status.contains("q quit"), "{status}");
    }

    #[test]
    fn focused_pane_is_highlighted_and_the_bar_shows_its_keys() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let terminal = draw(100, 10, &view(&emulator, Pane::Changes, false));
        let top = row(&terminal, 0);
        assert!(top.contains("┌ ● Changes ─"), "{top}");
        assert!(top.contains("┌ claude · term ─"), "{top}");
        assert_eq!(fg(&terminal, panes.right.x, 0), ACCENT, "right border");
        assert_eq!(fg(&terminal, panes.center.x, 0), DIM, "center border");
        assert_eq!(fg(&terminal, panes.left.x, 0), DIM, "left border");
        let status = row(&terminal, 9);
        assert!(status.starts_with(" focus: Changes"), "{status}");
        assert!(status.contains("Esc back"), "{status}");

        let terminal = draw(100, 10, &view(&emulator, Pane::Projects, false));
        assert!(row(&terminal, 0).starts_with("┌ ● Projects ─"));
        assert_eq!(fg(&terminal, panes.left.x, 0), ACCENT);
        assert_eq!(fg(&terminal, panes.center.x, 0), DIM);
    }

    #[test]
    fn pending_prefix_is_shown_with_the_keys_that_apply() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let status = row(&draw(100, 10, &view(&emulator, Pane::Agent, true)), 9);
        assert!(status.starts_with(" Ctrl+Q …"), "{status}");
        assert!(status.contains("1/2/3 pane"), "{status}");
        assert!(status.contains("Ctrl+Q literal"), "{status}");
        assert!(status.contains("Esc cancel"), "{status}");

        let status = row(&draw(100, 10, &view(&emulator, Pane::Changes, true)), 9);
        assert!(status.starts_with(" Ctrl+Q …"), "{status}");
        assert!(!status.contains("literal"), "{status}");
    }

    #[test]
    fn status_bar_keeps_a_gap_before_the_project() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Pane::Projects, false);
        v.project = "a-rather-long-project-name";
        let status = row(&draw(100, 10, &v), 9);
        // The hints fill the left area and stop one blank column before
        // the right-aligned project.
        assert!(status.starts_with(" focus: Projects"), "{status}");
        assert!(
            status.ends_with(" term · a-rather-long-project-name "),
            "{status}"
        );
        let chars: Vec<char> = status.chars().collect();
        let gap = chars.len() - "term · a-rather-long-project-name ".chars().count() - 1;
        assert_eq!(chars[gap], ' ', "{status}");
        assert_ne!(chars[gap - 1], ' ', "{status}");
    }

    #[test]
    fn cursor_follows_the_agent_only_when_it_is_focused() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut terminal = draw(100, 10, &view(&emulator, Pane::Agent, false));
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 23, y: 1 }
        );
        let terminal = draw(100, 10, &view(&emulator, Pane::Changes, false));
        assert!(
            !terminal.backend().cursor_visible(),
            "cursor hidden while the agent is unfocused"
        );
    }

    #[test]
    fn changes_pane_says_when_there_is_no_repository() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Pane::Agent, false);
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
        let mut v = view(&emulator, Pane::Agent, false);
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
        assert!(
            status.starts_with(" focus: claude   Ctrl+Q then q quit"),
            "{status}"
        );
        assert!(status.ends_with("term · term "));
    }

    #[test]
    fn collapsed_bar_never_offers_the_pane_digits() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        for pending in [false, true] {
            let mut v = view(&emulator, Pane::Agent, pending);
            v.side_panes = false;
            let status = row(&draw(80, 10, &v), 9);
            assert!(!status.contains("1/2/3"), "{status}");
            assert!(status.contains("q quit"), "{status}");
        }
    }

    #[test]
    fn full_screen_puts_the_cursor_where_the_agent_has_it() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 20 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, None))
            .expect("draw");
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 2, y: 0 }
        );
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
            let mut v = view(&emulator, Pane::Agent, true);
            v.side_panes = false;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
            terminal.draw(|frame| render(frame, &v)).expect("draw");
            terminal
                .draw(|frame| render_full(frame, &emulator, Some("x")))
                .expect("draw");
        }
    }

    #[test]
    fn ended_line_covers_the_agent_bottom_row_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut v = view(&emulator, Pane::Agent, false);
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
            status.starts_with(" agent ended   r relaunch · q quit"),
            "{status}"
        );
        assert!(status.contains("1/2/3 pane"), "{status}");

        // With a side pane focused the line stays, the cursor stays hidden
        // and the bar keeps the pane's keys.
        let mut side = view(&emulator, Pane::Changes, false);
        side.ended = v.ended;
        let terminal = draw(100, 10, &side);
        assert!(row(&terminal, 7).contains("r relaunch"));
        assert!(row(&terminal, 9).starts_with(" focus: Changes"));
        assert!(!terminal.backend().cursor_visible());

        let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended ")))
            .expect("draw");
        assert!(row(&terminal, 2).starts_with(" ended "));
        assert!(!terminal.backend().cursor_visible());
    }
}
