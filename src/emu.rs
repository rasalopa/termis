//! Terminal emulation for the agent pane: feeds pty output to a vt100
//! screen, answers the queries the child sends, tracks the input modes it
//! asks for, and renders the screen as a ratatui widget.

use tui_term::widget::PseudoTerminal;
use vt100::{Callbacks, Parser, Screen};

use crate::Size;
use crate::keys::{KittyFlags, Modes, Protocol};

/// Foreground reported to the child's colour queries (OSC 10).
const FOREGROUND: (u8, u8, u8) = (0xd3, 0xd6, 0xdd);
/// Background reported to the child's colour queries (OSC 11).
const BACKGROUND: (u8, u8, u8) = (0x13, 0x15, 0x1b);
/// Highest kitty flag value the child may push.
const KITTY_MASK: u8 = 0b1_1111;
/// Entries kept per kitty flag stack; a push on a full stack drops the
/// oldest, as the protocol requires.
const KITTY_STACK_DEPTH: usize = 16;

/// Requests from the child that are not part of the screen contents:
/// title, bells, replies to its queries, and the input modes it set. A
/// full reset (`ESC c`) does not clear them: vt100 handles it without a
/// callback.
#[derive(Debug, Default)]
struct Requests {
    title: Option<String>,
    bells: u32,
    /// Bytes to write back to the child.
    replies: Vec<u8>,
    /// kitty flag stacks for the main and the alternate screen, at most
    /// `KITTY_STACK_DEPTH` entries each.
    kitty: [Vec<u8>; 2],
    /// xterm modifyOtherKeys level, 0 to 2. Only level 2 changes the
    /// encoding; level 1 (keys without a legacy encoding) is treated as
    /// legacy.
    modify_other_keys: u8,
    /// DECSET 1004: the child wants focus events.
    focus_events: bool,
}

impl Requests {
    fn kitty_stack(&mut self, screen: &Screen) -> &mut Vec<u8> {
        &mut self.kitty[usize::from(screen.alternate_screen())]
    }

    fn kitty_top(&self, alternate: bool) -> u8 {
        self.kitty[usize::from(alternate)]
            .last()
            .copied()
            .unwrap_or(0)
    }

    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }

    /// First parameter. vte always supplies one, 0 when none was typed.
    fn first(params: &[&[u16]]) -> Option<u16> {
        params.first().and_then(|p| p.first()).copied()
    }
}

/// A title as text: control characters, which the host terminal would
/// execute, are dropped.
fn title_from(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// kitty flags from a parameter, masked to the five defined bits.
fn kitty_flags(first: Option<u16>) -> u8 {
    u8::try_from(first.unwrap_or(0) & u16::from(KITTY_MASK)).unwrap_or(0)
}

/// DECRPM state value: 1 set, 2 reset.
fn mode_state(on: bool) -> u8 {
    if on { 1 } else { 2 }
}

impl Callbacks for Requests {
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bells = self.bells.saturating_add(1);
    }

    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        self.title = Some(title_from(title));
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = Self::first(params);
        match (i1, c) {
            // kitty keyboard protocol: query, push, pop and set.
            (Some(b'?'), 'u') => {
                let flags = self.kitty_top(screen.alternate_screen());
                self.reply(format!("\x1b[?{flags}u").as_bytes());
            }
            (Some(b'>'), 'u') => {
                let flags = kitty_flags(first);
                let stack = self.kitty_stack(screen);
                if stack.len() == KITTY_STACK_DEPTH {
                    stack.remove(0);
                }
                stack.push(flags);
            }
            (Some(b'<'), 'u') => {
                let count = usize::from(first.unwrap_or(1).max(1));
                let stack = self.kitty_stack(screen);
                let keep = stack.len().saturating_sub(count);
                stack.truncate(keep);
            }
            (Some(b'='), 'u') => {
                let flags = kitty_flags(first);
                let mode = params.get(1).and_then(|p| p.first()).copied().unwrap_or(1);
                let stack = self.kitty_stack(screen);
                if stack.is_empty() {
                    stack.push(0);
                }
                if let Some(top) = stack.last_mut() {
                    *top = match mode {
                        2 => *top | flags,
                        3 => *top & !flags,
                        _ => flags,
                    };
                }
            }
            // xterm XTMODKEYS: `CSI > 4 ; v m` sets modifyOtherKeys, `CSI > m`
            // resets everything. vte reports a missing parameter as 0.
            (Some(b'>'), 'm') => {
                let second = params.get(1).and_then(|p| p.first()).copied();
                match (first.unwrap_or(0), second) {
                    (4, value) => {
                        self.modify_other_keys =
                            u8::try_from(value.unwrap_or(0).min(2)).unwrap_or(2);
                    }
                    (0, None) => self.modify_other_keys = 0,
                    _ => {}
                }
            }
            // XTQMODKEYS: `CSI ? 4 m` asks for the value.
            (Some(b'?'), 'm') if first == Some(4) => {
                let value = self.modify_other_keys;
                self.reply(format!("\x1b[>4;{value}m").as_bytes());
            }
            // DECSET / DECRST 1004: focus events.
            (Some(b'?'), 'h') | (Some(b'?'), 'l') => {
                if params.iter().any(|p| p.first() == Some(&1004)) {
                    self.focus_events = c == 'h';
                }
            }
            // Device attributes: a VT220 with colour, firmware 0.
            (None, 'c') if first.unwrap_or(0) == 0 => self.reply(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') if first.unwrap_or(0) == 0 => self.reply(b"\x1b[>1;0;0c"),
            // Device status: ok, and the cursor position. The position is
            // absolute: vt100 does not expose origin mode, so a child that
            // set DECSET 6 gets the unshifted row.
            (None, 'n') if first == Some(5) => self.reply(b"\x1b[0n"),
            (None, 'n') if first == Some(6) => {
                let (row, col) = screen.cursor_position();
                self.reply(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            (Some(b'?'), 'n') if first == Some(6) => {
                let (row, col) = screen.cursor_position();
                self.reply(format!("\x1b[?{};{};1R", row + 1, col + 1).as_bytes());
            }
            // DECRQM: report the modes this emulator tracks, 0 for the rest.
            (Some(b'?'), 'p') if i2 == Some(b'$') => {
                let mode = first.unwrap_or(0);
                let state = match mode {
                    1 => mode_state(screen.application_cursor()),
                    25 => mode_state(!screen.hide_cursor()),
                    1004 => mode_state(self.focus_events),
                    1049 => mode_state(screen.alternate_screen()),
                    2004 => mode_state(screen.bracketed_paste()),
                    _ => 0,
                };
                self.reply(format!("\x1b[?{mode};{state}$y").as_bytes());
            }
            // XTVERSION.
            (Some(b'>'), 'q') if first.unwrap_or(0) == 0 => {
                self.reply(format!("\x1bP>|term {}\x1b\\", crate::VERSION).as_bytes());
            }
            _ => {}
        }
    }

    /// vt100 splits an OSC payload on every `;`, so a title containing one
    /// arrives here instead of in `set_window_title`. Colour queries arrive
    /// here too.
    fn unhandled_osc(&mut self, _: &mut Screen, params: &[&[u8]]) {
        match params {
            [b"0" | b"2", rest @ ..] => self.title = Some(title_from(&rest.join(&b';'))),
            [b"10", b"?"] => self.reply(colour_reply(10, FOREGROUND).as_bytes()),
            [b"11", b"?"] => self.reply(colour_reply(11, BACKGROUND).as_bytes()),
            _ => {}
        }
    }
}

/// `OSC code ; rgb:rrrr/gggg/bbbb ST`. xterm mirrors the query's terminator
/// (BEL or ST); vt100 does not pass it on, so the reply always ends in ST.
fn colour_reply(code: u8, (r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\")
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
    /// across calls. Replies the child asked for are collected; see
    /// [`Emulator::take_replies`].
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

    /// Bytes the child must receive in answer to its queries (device
    /// attributes, cursor position, modes, colours, kitty flags), collected
    /// since the last call. A child can make this grow at about four times
    /// its output; drain it every pass and write it without blocking the
    /// main loop.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.parser.callbacks_mut().replies)
    }

    /// Input modes the child asked for, for the key encoder. kitty flags
    /// take precedence over modifyOtherKeys when both are set; only
    /// modifyOtherKeys level 2 counts.
    pub fn modes(&self) -> Modes {
        let screen = self.parser.screen();
        let requests = self.parser.callbacks();
        let kitty = requests.kitty_top(screen.alternate_screen());
        let protocol = if kitty != 0 {
            Protocol::Kitty(KittyFlags(kitty))
        } else if requests.modify_other_keys == 2 {
            Protocol::ModifyOtherKeys
        } else {
            Protocol::Legacy
        };
        Modes {
            protocol,
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
        }
    }

    /// Whether the child asked for focus events (DECSET 1004).
    pub fn focus_events(&self) -> bool {
        self.parser.callbacks().focus_events
    }

    /// Cursor position as (row, column), or `None` while the child hides it.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        (!screen.hide_cursor()).then(|| screen.cursor_position())
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

    fn replies(emu: &mut Emulator, input: &[u8]) -> String {
        emu.process(input);
        String::from_utf8(emu.take_replies()).expect("replies are text")
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
        emu.process(b"\x1b]2;agent\x07\x07\x07");
        assert_eq!(emu.title(), Some("agent"));
        assert_eq!(emu.take_bells(), 2);
        assert_eq!(emu.take_bells(), 0);
        emu.process(b"\x1b]0;both\x07");
        assert_eq!(emu.title(), Some("both"));
        emu.process(b"\x1b]2;a;b\x07");
        assert_eq!(emu.title(), Some("a;b"));
        emu.process("\x1b]2;a\u{9b}31mb\x07".as_bytes());
        assert_eq!(emu.title(), Some("a31mb"));
        emu.process(b"\x1b]0;x\x7fy\x07");
        assert_eq!(emu.title(), Some("xy"));
    }

    #[test]
    fn answers_device_and_status_queries() {
        let mut emu = emulator(5, 20);
        assert_eq!(replies(&mut emu, b"\x1b[c"), "\x1b[?62;22c");
        assert_eq!(replies(&mut emu, b"\x1b[0c"), "\x1b[?62;22c");
        assert!(replies(&mut emu, b"\x1b[1c").is_empty());
        assert_eq!(replies(&mut emu, b"\x1b[>c"), "\x1b[>1;0;0c");
        assert_eq!(replies(&mut emu, b"\x1b[5n"), "\x1b[0n");
        assert_eq!(replies(&mut emu, b"ab\x1b[6n"), "\x1b[1;3R");
        assert_eq!(replies(&mut emu, b"\x1b[?6n"), "\x1b[?1;3;1R");
        assert_eq!(
            replies(&mut emu, b"\x1b[>0q"),
            format!("\x1bP>|term {}\x1b\\", crate::VERSION)
        );
        assert!(replies(&mut emu, b"\x1b[>1q").is_empty());
        assert_eq!(replies(&mut emu, b"\x1b[>4;2m\x1b[?4m"), "\x1b[>4;2m");
        assert!(emu.take_replies().is_empty());
    }

    #[test]
    fn answers_mode_requests() {
        let mut emu = emulator(5, 20);
        assert_eq!(replies(&mut emu, b"\x1b[?2026$p"), "\x1b[?2026;0$y");
        assert_eq!(replies(&mut emu, b"\x1b[?2004$p"), "\x1b[?2004;2$y");
        emu.process(b"\x1b[?2004h\x1b[?1004h\x1b[?25l");
        assert_eq!(replies(&mut emu, b"\x1b[?2004$p"), "\x1b[?2004;1$y");
        assert_eq!(replies(&mut emu, b"\x1b[?1004$p"), "\x1b[?1004;1$y");
        assert_eq!(replies(&mut emu, b"\x1b[?25$p"), "\x1b[?25;2$y");
        assert_eq!(replies(&mut emu, b"\x1b[?1049$p"), "\x1b[?1049;2$y");
        assert_eq!(replies(&mut emu, b"\x1b[?1$p"), "\x1b[?1;2$y");
    }

    #[test]
    fn answers_colour_queries() {
        let mut emu = emulator(2, 10);
        assert_eq!(
            replies(&mut emu, b"\x1b]10;?\x07\x1b]11;?\x1b\\"),
            "\x1b]10;rgb:d3d3/d6d6/dddd\x1b\\\x1b]11;rgb:1313/1515/1b1b\x1b\\"
        );
    }

    #[test]
    fn tracks_kitty_flags_per_screen() {
        let mut emu = emulator(2, 10);
        assert_eq!(replies(&mut emu, b"\x1b[?u"), "\x1b[?0u");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);

        emu.process(b"\x1b[>1u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(1)));
        assert_eq!(replies(&mut emu, b"\x1b[?u"), "\x1b[?1u");

        // Set: mode 2 adds bits, mode 3 clears them, mode 1 replaces.
        emu.process(b"\x1b[=8;2u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(9)));
        emu.process(b"\x1b[=1;3u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(8)));
        emu.process(b"\x1b[=3;1u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(3)));

        // The alternate screen has its own stack.
        emu.process(b"\x1b[?1049h");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
        emu.process(b"\x1b[>31u\x1b[>1u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(1)));
        emu.process(b"\x1b[<u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(31)));
        emu.process(b"\x1b[?1049l");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(3)));

        // Popping more than pushed empties the stack; flags are masked to
        // five bits, also above 255.
        emu.process(b"\x1b[<5u");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
        emu.process(b"\x1b[>255u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(31)));
        emu.process(b"\x1b[>256u");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
        assert_eq!(replies(&mut emu, b"\x1b[?u"), "\x1b[?0u");
        emu.process(b"\x1b[>257u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(1)));

        // Set on an empty stack creates the entry.
        emu.process(b"\x1b[<9u\x1b[=1u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(1)));
        emu.process(b"\x1b[<u");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
    }

    #[test]
    fn kitty_stack_drops_the_oldest_entry_when_full() {
        let mut emu = emulator(2, 10);
        for flags in 1..=17u8 {
            emu.process(format!("\x1b[>{flags}u").as_bytes());
        }
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(17)));
        for _ in 0..15 {
            emu.process(b"\x1b[<u");
        }
        // The first push (1) was evicted; 2 is the bottom entry.
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(2)));
        emu.process(b"\x1b[<u");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
    }

    #[test]
    fn tracks_modify_other_keys_and_terminal_modes() {
        let mut emu = emulator(2, 10);
        let modes = emu.modes();
        assert_eq!(modes, Modes::default());

        emu.process(b"\x1b[>4;2m");
        assert_eq!(emu.modes().protocol, Protocol::ModifyOtherKeys);
        emu.process(b"\x1b[>4;1m");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
        emu.process(b"\x1b[>4;2m\x1b[>4m");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);
        emu.process(b"\x1b[>4;2m\x1b[>m");
        assert_eq!(emu.modes().protocol, Protocol::Legacy);

        // kitty flags win over modifyOtherKeys.
        emu.process(b"\x1b[>4;2m\x1b[>1u");
        assert_eq!(emu.modes().protocol, Protocol::Kitty(KittyFlags(1)));

        emu.process(b"\x1b[?1h\x1b[?1004;2004h");
        let modes = emu.modes();
        assert!(modes.application_cursor);
        assert!(modes.bracketed_paste);
        assert!(emu.focus_events());
        emu.process(b"\x1b[?1l\x1b[?2004;1004l");
        let modes = emu.modes();
        assert!(!modes.application_cursor);
        assert!(!modes.bracketed_paste);
        assert!(!emu.focus_events());
    }

    #[test]
    fn replays_a_recorded_agent_startup() {
        const STARTUP: &[u8] = include_bytes!("../tests/fixtures/claude-startup.bin");
        let mut emu = Emulator::new(
            Size {
                rows: 30,
                cols: 100,
            },
            0,
        );
        let answers = replies(&mut emu, STARTUP);
        assert!(answers.contains("\x1b[?62;22c"), "DA1 answered");
        assert!(answers.contains("\x1b[?0u"), "kitty query answered");
        assert!(
            answers.contains(&format!("\x1bP>|term {}\x1b\\", crate::VERSION)),
            "XTVERSION answered"
        );
        // After the answers the agent pushed kitty flags 1 and 4 and set
        // modifyOtherKeys 2, bracketed paste and focus events.
        let modes = emu.modes();
        assert_eq!(modes.protocol, Protocol::Kitty(KittyFlags(5)));
        assert!(modes.bracketed_paste);
        assert!(emu.focus_events());
        assert_eq!(emu.title(), None);
    }

    #[test]
    fn reports_the_cursor_unless_hidden() {
        let mut emu = emulator(3, 10);
        emu.process(b"ab");
        assert_eq!(emu.cursor(), Some((0, 2)));
        emu.process(b"\x1b[?25l");
        assert_eq!(emu.cursor(), None);
        emu.process(b"\x1b[?25h");
        assert_eq!(emu.cursor(), Some((0, 2)));
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
