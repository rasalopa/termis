//! Terminal emulation for the agent pane: feeds pty output to a vt100
//! screen and renders it as a ratatui widget.

use tui_term::widget::PseudoTerminal;
use vt100::{Callbacks, Parser, Screen};

use crate::Size;

/// Requests from the child that are not part of the screen contents.
#[derive(Debug, Default)]
struct Requests {
    title: Option<String>,
    bells: u32,
}

impl Callbacks for Requests {
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bells = self.bells.saturating_add(1);
    }

    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }

    /// vt100 splits an OSC payload on every `;`, so a title containing one
    /// arrives here instead of in `set_window_title`.
    fn unhandled_osc(&mut self, _: &mut Screen, params: &[&[u8]]) {
        if let [b"0" | b"2", rest @ ..] = params {
            self.title = Some(String::from_utf8_lossy(&rest.join(&b';')).into_owned());
        }
    }
}

/// vt100 cannot represent an empty screen; a zero dimension becomes one cell.
fn clamp(size: Size) -> (u16, u16) {
    (size.rows.max(1), size.cols.max(1))
}

/// A vt100 screen fed by the child's output.
pub struct Emulator {
    parser: Parser<Requests>,
}

impl Emulator {
    /// Creates a screen of `size` with `scrollback` lines of history. A zero
    /// dimension is raised to one cell.
    pub fn new(size: Size, scrollback: usize) -> Self {
        let (rows, cols) = clamp(size);
        Self {
            parser: Parser::new_with_callbacks(rows, cols, scrollback, Requests::default()),
        }
    }

    /// Feeds bytes written by the child. Escape sequences may be split
    /// across calls.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Changes the screen size without reflowing: columns past the new
    /// width are cut, rows past the new height are dropped from the bottom,
    /// and the scrollback is kept. A zero dimension is raised to one cell.
    /// The child must be told separately.
    pub fn resize(&mut self, size: Size) {
        let (rows, cols) = clamp(size);
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Current screen size in cells.
    pub fn size(&self) -> Size {
        let (rows, cols) = self.parser.screen().size();
        Size { rows, cols }
    }

    /// The emulated screen, for tests. Render through [`Emulator::widget`].
    #[cfg(test)]
    fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// Title the child set with OSC 0 or 2, if any.
    pub fn title(&self) -> Option<&str> {
        self.parser.callbacks().title.as_deref()
    }

    /// Number of bells rung since the last call.
    pub fn take_bells(&mut self) -> u32 {
        std::mem::take(&mut self.parser.callbacks_mut().bells)
    }

    /// The screen as a widget. Cursor and block are set by the caller;
    /// tui-term 0.3.4 ignores `PseudoTerminal::style`, so pane colours go
    /// on the block.
    pub fn widget(&self) -> PseudoTerminal<'_, impl tui_term::widget::Screen> {
        PseudoTerminal::new(self.parser.screen())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_term::widget::Cursor;

    fn emulator(rows: u16, cols: u16) -> Emulator {
        Emulator::new(Size { rows, cols }, 0)
    }

    #[test]
    fn shows_processed_text() {
        let mut emu = emulator(3, 10);
        emu.process(b"hello\r\nworld");
        assert_eq!(emu.screen().contents(), "hello\nworld");
        assert_eq!(emu.screen().cursor_position(), (1, 5));
    }

    #[test]
    fn decodes_sequences_split_across_calls() {
        let mut emu = emulator(2, 10);
        emu.process(b"a\x1b[");
        emu.process(b"2;3Hb");
        assert_eq!(emu.screen().contents(), "a\n  b");
    }

    #[test]
    fn resize_keeps_the_contents() {
        let mut emu = emulator(3, 10);
        emu.process(b"hello");
        emu.resize(Size { rows: 5, cols: 20 });
        assert_eq!(emu.size(), Size { rows: 5, cols: 20 });
        assert_eq!(emu.screen().contents(), "hello");
    }

    #[test]
    fn zero_dimensions_become_one_cell() {
        let mut emu = emulator(0, 0);
        assert_eq!(emu.size(), Size { rows: 1, cols: 1 });
        emu.resize(Size { rows: 0, cols: 5 });
        assert_eq!(emu.size(), Size { rows: 1, cols: 5 });
    }

    #[test]
    fn records_title_and_bells() {
        let mut emu = emulator(3, 10);
        assert_eq!(emu.title(), None);
        emu.process(b"\x1b]2;claude\x07\x07\x07");
        assert_eq!(emu.title(), Some("claude"));
        assert_eq!(emu.take_bells(), 2);
        assert_eq!(emu.take_bells(), 0);
        emu.process(b"\x1b]0;both\x07");
        assert_eq!(emu.title(), Some("both"));
        emu.process(b"\x1b]2;a;b\x07");
        assert_eq!(emu.title(), Some("a;b"));
    }

    #[test]
    fn renders_into_a_buffer() {
        let mut emu = emulator(2, 8);
        emu.process(b"ab\r\ncd");
        let mut terminal = Terminal::new(TestBackend::new(8, 2)).expect("terminal");
        terminal
            .draw(|frame| {
                let widget = emu.widget().cursor(Cursor::default().visibility(false));
                frame.render_widget(widget, frame.area());
            })
            .expect("draw");
        terminal
            .backend()
            .assert_buffer_lines(["ab      ", "cd      "]);
    }

    #[test]
    fn renders_wide_characters_in_two_columns() {
        let mut emu = emulator(1, 4);
        emu.process("日a".as_bytes());
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                let widget = emu.widget().cursor(Cursor::default().visibility(false));
                frame.render_widget(widget, frame.area());
            })
            .expect("draw");
        terminal.backend().assert_buffer_lines(["日a "]);
    }
}
