//! Drawing: the three panes, the status bar, and the full-screen view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use tui_term::widget::Cursor;

use crate::emu::Emulator;
use crate::strings;

pub mod layout;

/// What the three-pane view shows.
pub struct View<'a> {
    /// Name of the agent in the center pane title.
    pub agent: &'a str,
    /// Name of the project in the center pane title and the status bar.
    pub project: &'a str,
    pub emulator: &'a Emulator,
}

/// Where the agent's screen goes for a terminal of `area`: the whole area
/// when `full_screen`, else inside the center pane's border.
pub fn agent_area(area: Rect, full_screen: bool) -> Rect {
    if full_screen {
        area
    } else {
        pane_block("").inner(layout::panes(area).center)
    }
}

/// The border every pane uses; the interior it leaves is the agent's
/// screen in the center.
fn pane_block(title: &str) -> Block<'static> {
    Block::bordered().title(format!(" {title} "))
}

/// Draws the agent over the whole frame.
pub fn render_full(frame: &mut Frame, emulator: &Emulator) {
    let area = frame.area();
    render_agent(frame, area, emulator);
}

/// Draws the three panes and the status bar.
pub fn render(frame: &mut Frame, view: &View<'_>) {
    let panes = layout::panes(frame.area());

    if layout::sides_visible(frame.area()) {
        frame.render_widget(pane_block(strings::PANE_PROJECTS), panes.left);
        frame.render_widget(pane_block(strings::PANE_CHANGES), panes.right);
    }

    let center = pane_block(&strings::agent_pane_title(view.agent, view.project));
    let inner = center.inner(panes.center);
    frame.render_widget(center, panes.center);
    render_agent(frame, inner, view.emulator);

    // Keys on the left, the project flush right.
    let project = Line::from(strings::status_project(view.project)).right_aligned();
    let width = u16::try_from(project.width()).unwrap_or(u16::MAX);
    let [keys, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(panes.status);
    frame.render_widget(Paragraph::new(strings::STATUS_KEYS), keys);
    frame.render_widget(Paragraph::new(project), right);
}

fn render_agent(frame: &mut Frame, area: Rect, emulator: &Emulator) {
    let widget = emulator
        .widget()
        .cursor(Cursor::default().visibility(false));
    frame.render_widget(widget, area);
    if let Some(position) = cursor_position(area, emulator.cursor()) {
        frame.set_cursor_position(position);
    }
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

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    #[test]
    fn draws_titled_panes_and_the_status_bar() {
        let mut emulator = Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false).into(), 0);
        emulator.process(b"hello");
        let view = View {
            agent: "claude",
            project: "term",
            emulator: &emulator,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).expect("terminal");
        terminal.draw(|frame| render(frame, &view)).expect("draw");

        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ Projects ─"), "{top}");
        assert!(top.contains("┌ claude · term ─"), "{top}");
        assert!(top.contains("┌ Changes ─"), "{top}");
        assert_eq!(top.chars().count(), 100);

        let second = row(&terminal, 1);
        assert!(second.starts_with('│'), "{second}");
        assert!(second.contains("│hello"), "{second}");

        let bottom = row(&terminal, 8);
        assert!(bottom.starts_with('└'), "{bottom}");
        let status = row(&terminal, 9);
        assert!(status.starts_with(" Ctrl+Q then q quit"), "{status}");
        assert!(status.ends_with("term · term "), "{status}");
    }

    #[test]
    fn agent_area_is_the_center_interior() {
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(agent_area(area, true), area);
        assert_eq!(agent_area(area, false), Rect::new(21, 1, 53, 37));
        // Collapsed: the whole width inside one border.
        let narrow = Rect::new(0, 0, 80, 40);
        assert_eq!(agent_area(narrow, false), Rect::new(1, 1, 78, 37));
    }

    #[test]
    fn collapsed_view_shows_only_the_agent_pane() {
        let mut emulator = Emulator::new(agent_area(Rect::new(0, 0, 80, 10), false).into(), 0);
        emulator.process(b"hello");
        let view = View {
            agent: "claude",
            project: "term",
            emulator: &emulator,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).expect("terminal");
        terminal.draw(|frame| render(frame, &view)).expect("draw");
        let top = row(&terminal, 0);
        assert!(top.starts_with("┌ claude · term ─"), "{top}");
        assert!(
            !top.contains("Projects") && !top.contains("Changes"),
            "{top}"
        );
        assert!(top.ends_with('┐'), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        assert!(row(&terminal, 9).ends_with("term · term "));
    }

    #[test]
    fn full_screen_puts_the_cursor_where_the_agent_has_it() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 20 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator))
            .expect("draw");
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
            let view = View {
                agent: "a",
                project: "p",
                emulator: &emulator,
            };
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
            terminal.draw(|frame| render(frame, &view)).expect("draw");
            terminal
                .draw(|frame| render_full(frame, &emulator))
                .expect("draw");
        }
    }
}
