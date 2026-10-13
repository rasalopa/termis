//! Encodes host key events into the bytes a child expects from its
//! terminal, following the protocol the child asked for: legacy xterm,
//! xterm `modifyOtherKeys`, or the kitty keyboard protocol.

use std::ops::BitOr;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventState, KeyModifiers, MouseButton, MouseEventKind,
};

const ESC: u8 = 0x1b;

/// kitty keyboard protocol flags, as pushed by the child with
/// `CSI > flags u`. Only [`KittyFlags::DISAMBIGUATE`] and
/// [`KittyFlags::REPORT_ALL_KEYS_AS_ESCAPES`] change the encoding; event
/// types, alternate keys and associated text are accepted but not
/// reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KittyFlags(pub u8);

impl KittyFlags {
    pub const DISAMBIGUATE: Self = Self(1);
    pub const REPORT_EVENT_TYPES: Self = Self(2);
    pub const REPORT_ALTERNATE_KEYS: Self = Self(4);
    pub const REPORT_ALL_KEYS_AS_ESCAPES: Self = Self(8);
    pub const REPORT_ASSOCIATED_TEXT: Self = Self(16);

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets of flags.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr for KittyFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// Keyboard protocol the child enabled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// Plain xterm encoding.
    #[default]
    Legacy,
    /// xterm `CSI > 4 ; 2 m`: keys with modifiers as `CSI 27 ; m ; code ~`.
    ModifyOtherKeys,
    /// kitty `CSI > flags u`. Without `DISAMBIGUATE` or
    /// `REPORT_ALL_KEYS_AS_ESCAPES` the encoding is [`Protocol::Legacy`].
    Kitty(KittyFlags),
}

/// Terminal modes that change how keys, pastes and the mouse are
/// encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub protocol: Protocol,
    /// DECCKM: cursor keys as `SS3 x` instead of `CSI x` in the xterm
    /// protocols. The kitty protocol never uses `SS3`.
    pub application_cursor: bool,
    /// DECSET 2004: pastes wrapped in `CSI 200 ~` and `CSI 201 ~`.
    pub bracketed_paste: bool,
    /// The mouse reports the child asked for with DECSET 1000 to 1003;
    /// the X10 mode 9 reports no wheel and does not count.
    pub mouse: Option<Mouse>,
}

/// Mouse reports a child asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    /// Which events it wants.
    pub tracking: Tracking,
    /// How a report is written.
    pub encoding: MouseEncoding,
}

/// Which mouse events a child wants reported, each level adding to the
/// one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tracking {
    /// DECSET 1000: button presses and releases, and the wheel.
    Buttons,
    /// DECSET 1002: also motion while a button is held.
    Drags,
    /// DECSET 1003: also motion with no button held.
    Motion,
}

impl Tracking {
    /// Whether a child tracking this way wants `kind` reported.
    pub fn reports(self, kind: MouseEventKind) -> bool {
        match kind {
            MouseEventKind::Drag(_) => self >= Self::Drags,
            MouseEventKind::Moved => self == Self::Motion,
            _ => true,
        }
    }
}

/// How a mouse report is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    /// `CSI M` and three bytes, each value plus 32; positions up to 223.
    X10,
    /// As [`MouseEncoding::X10`] with values past 95 as UTF-8 (DECSET
    /// 1005); positions up to 2015.
    Utf8,
    /// `CSI < b ; x ; y M` (DECSET 1006).
    Sgr,
}

/// Appends a report of the mouse event `kind` at `column` and `row` of
/// the child's screen, counted from zero, with the shift, alt and control
/// `modifiers`. Only the SGR encoding says which button was released. A
/// position the encoding cannot carry writes nothing.
pub fn encode_mouse(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseEncoding,
    out: &mut Vec<u8>,
) {
    let code = |button: MouseButton| match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let mut release = false;
    let mut button: u32 = match kind {
        MouseEventKind::Down(button) => code(button),
        MouseEventKind::Up(button) if encoding == MouseEncoding::Sgr => {
            release = true;
            code(button)
        }
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(button) => code(button) + 32,
        MouseEventKind::Moved => 3 + 32,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };
    if modifiers.contains(KeyModifiers::SHIFT) {
        button += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        button += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        button += 16;
    }
    let (x, y) = (u32::from(column) + 1, u32::from(row) + 1);
    match encoding {
        MouseEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            out.extend_from_slice(format!("\x1b[<{button};{x};{y}{end}").as_bytes());
        }
        MouseEncoding::X10 => {
            let values = [button, x, y].map(|value| u8::try_from(value + 32).ok());
            // A value past 255 has no byte: the position cannot be told.
            if let [Some(b), Some(x), Some(y)] = values {
                out.extend_from_slice(&[0x1b, b'[', b'M', b, x, y]);
            }
        }
        MouseEncoding::Utf8 => {
            let values = [button, x, y]
                .map(|value| char::from_u32(value + 32).filter(|_| value + 32 < 2048));
            if let [Some(b), Some(x), Some(y)] = values {
                out.extend_from_slice(b"\x1b[M");
                for value in [b, x, y] {
                    let mut buffer = [0; 4];
                    out.extend_from_slice(value.encode_utf8(&mut buffer).as_bytes());
                }
            }
        }
    }
}

/// Appends the bytes for `key` to `out`. Returns `false` when the key has
/// no encoding and nothing was written.
#[must_use = "false means the key was not encoded"]
pub fn encode(key: &KeyEvent, modes: Modes, out: &mut Vec<u8>) -> bool {
    match modes.protocol {
        Protocol::Legacy => legacy(key, modes, out),
        Protocol::ModifyOtherKeys => modify_other_keys(key, modes, out),
        Protocol::Kitty(flags)
            if flags.contains(KittyFlags::DISAMBIGUATE)
                || flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPES) =>
        {
            kitty(key, flags, out)
        }
        Protocol::Kitty(_) => legacy(key, modes, out),
    }
}

/// Appends pasted text. Line ends become carriage returns, as terminals
/// send them. Control characters other than tab and line ends, including
/// escape, are dropped, so pasted text can neither end the bracketed paste
/// early nor be read as key sequences. The text is wrapped in the
/// bracketed paste markers when the child asked for them.
pub fn encode_paste(text: &str, modes: Modes, out: &mut Vec<u8>) {
    let text: String = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .chars()
        .filter(|&c| c == '\t' || c == '\r' || !c.is_control())
        .collect();
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
}

/// Modifiers of `key`, with `SHIFT` restored for an upper-case character:
/// a kitty host that reports alternate keys hands over the shifted
/// character and clears the modifier.
fn modifiers(key: &KeyEvent) -> KeyModifiers {
    match key.code {
        KeyCode::Char(c) if c.is_uppercase() => key.modifiers | KeyModifiers::SHIFT,
        _ => key.modifiers,
    }
}

/// xterm modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4, Meta 8.
/// Super and Hyper have no xterm encoding and are ignored.
fn xterm_param(mods: KeyModifiers) -> u16 {
    let mut bits = 0;
    if mods.contains(KeyModifiers::SHIFT) {
        bits |= 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        bits |= 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        bits |= 4;
    }
    if mods.contains(KeyModifiers::META) {
        bits |= 8;
    }
    1 + bits
}

/// kitty modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4, Super 8,
/// Hyper 16, Meta 32, and, when `locks` is set, Caps Lock 64 and Num Lock
/// 128 from the key state.
fn kitty_param(mods: KeyModifiers, state: KeyEventState, locks: bool) -> u16 {
    let mut bits = 0;
    if mods.contains(KeyModifiers::SHIFT) {
        bits |= 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        bits |= 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        bits |= 4;
    }
    if mods.contains(KeyModifiers::SUPER) {
        bits |= 8;
    }
    if mods.contains(KeyModifiers::HYPER) {
        bits |= 16;
    }
    if mods.contains(KeyModifiers::META) {
        bits |= 32;
    }
    if locks {
        if state.contains(KeyEventState::CAPS_LOCK) {
            bits |= 64;
        }
        if state.contains(KeyEventState::NUM_LOCK) {
            bits |= 128;
        }
    }
    1 + bits
}

/// Control byte for `Ctrl` plus a character, when one exists.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 & 0x1f),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' | '~' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// The lower-cased code point of a text key, as close as a host event gets
/// to the unshifted key the kitty protocol wants: `A` becomes `a`, but `!`
/// stays `!` because the host does not say which key produced it.
fn base_code(c: char) -> u32 {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(first), None) => first as u32,
        _ => c as u32,
    }
}

fn push_utf8(c: char, out: &mut Vec<u8>) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// `CSI <params> <final>`.
fn csi(params: &str, final_byte: u8, out: &mut Vec<u8>) {
    out.push(ESC);
    out.push(b'[');
    out.extend_from_slice(params.as_bytes());
    out.push(final_byte);
}

/// Keys that are not text: cursor, editing and function keys. `m` is the
/// modifier parameter. With `legacy_forms`, unmodified cursor keys use
/// `SS3` in application cursor mode and F1 to F4 always do; the kitty
/// protocol has neither and encodes F3 as `CSI 13 ~`.
fn functional(
    code: KeyCode,
    m: u16,
    application_cursor: bool,
    legacy_forms: bool,
    out: &mut Vec<u8>,
) -> bool {
    let letter = |out: &mut Vec<u8>, letter: u8, ss3: bool| {
        if m != 1 {
            csi(&format!("1;{m}"), letter, out);
        } else if ss3 {
            out.extend_from_slice(&[ESC, b'O', letter]);
        } else {
            csi("", letter, out);
        }
    };
    let tilde = |out: &mut Vec<u8>, number: u8| {
        if m != 1 {
            csi(&format!("{number};{m}"), b'~', out);
        } else {
            csi(&number.to_string(), b'~', out);
        }
    };
    let cursor_ss3 = legacy_forms && application_cursor;
    match code {
        KeyCode::Up => letter(out, b'A', cursor_ss3),
        KeyCode::Down => letter(out, b'B', cursor_ss3),
        KeyCode::Right => letter(out, b'C', cursor_ss3),
        KeyCode::Left => letter(out, b'D', cursor_ss3),
        KeyCode::Home => letter(out, b'H', cursor_ss3),
        KeyCode::End => letter(out, b'F', cursor_ss3),
        KeyCode::Insert => tilde(out, 2),
        KeyCode::Delete => tilde(out, 3),
        KeyCode::PageUp => tilde(out, 5),
        KeyCode::PageDown => tilde(out, 6),
        KeyCode::F(3) if !legacy_forms => tilde(out, 13),
        KeyCode::F(n @ 1..=4) => letter(out, b'P' + (n - 1), legacy_forms),
        KeyCode::F(n @ 5..=20) => {
            const CODES: [u8; 16] = [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
            ];
            tilde(out, CODES[usize::from(n - 5)]);
        }
        _ => return false,
    }
    true
}

fn legacy(key: &KeyEvent, modes: Modes, out: &mut Vec<u8>) -> bool {
    let mods = modifiers(key);
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let with_alt = |out: &mut Vec<u8>, bytes: &[u8]| {
        if alt {
            out.push(ESC);
        }
        out.extend_from_slice(bytes);
    };
    match key.code {
        KeyCode::Char(c) => match control_byte(c) {
            Some(byte) if ctrl => with_alt(out, &[byte]),
            _ => {
                let mut buf = [0u8; 4];
                with_alt(out, c.encode_utf8(&mut buf).as_bytes());
            }
        },
        KeyCode::Enter => with_alt(out, b"\r"),
        KeyCode::Tab => with_alt(out, b"\t"),
        KeyCode::BackTab => with_alt(out, b"\x1b[Z"),
        KeyCode::Backspace => with_alt(out, if ctrl { b"\x08" } else { b"\x7f" }),
        KeyCode::Esc => with_alt(out, b"\x1b"),
        KeyCode::Null => out.push(0),
        _ => {
            return functional(
                key.code,
                xterm_param(mods),
                modes.application_cursor,
                true,
                out,
            );
        }
    }
    true
}

/// xterm modifyOtherKeys mode 2: text keys, Enter, Tab, Backspace and Esc
/// with a modifier other than plain Shift on text are reported as
/// `CSI 27 ; m ; code ~`, with the character as the host delivered it.
/// Backspace with Control alone keeps its legacy byte, as in xterm. Cursor,
/// editing and function keys keep their `CSI 1 ; m` forms.
fn modify_other_keys(key: &KeyEvent, modes: Modes, out: &mut Vec<u8>) -> bool {
    let mods = modifiers(key);
    let m = xterm_param(mods);
    if m == 1 {
        return legacy(key, modes, out);
    }
    let shift_only = mods == KeyModifiers::SHIFT;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let code = match key.code {
        KeyCode::Char(c) if shift_only && c != ' ' => {
            push_utf8(c, out);
            return true;
        }
        KeyCode::Char(c) => c as u32,
        KeyCode::Enter => 13,
        KeyCode::Tab | KeyCode::BackTab => 9,
        KeyCode::Backspace if mods == KeyModifiers::CONTROL => return legacy(key, modes, out),
        KeyCode::Backspace if ctrl => 8,
        KeyCode::Backspace => 127,
        KeyCode::Esc => 27,
        KeyCode::Null => return legacy(key, modes, out),
        _ => return functional(key.code, m, modes.application_cursor, true, out),
    };
    csi(&format!("27;{m};{code}"), b'~', out);
    true
}

/// kitty protocol. `DISAMBIGUATE` moves Esc and modified keys to
/// `CSI code ; m u` and keeps unmodified Enter, Tab and Backspace as legacy
/// bytes; `REPORT_ALL_KEYS_AS_ESCAPES` moves every key there. Lock
/// modifiers are reported for keys that do not produce text.
fn kitty(key: &KeyEvent, flags: KittyFlags, out: &mut Vec<u8>) -> bool {
    let all = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPES);
    let mods = modifiers(key);
    let text = matches!(key.code, KeyCode::Char(_))
        && mods.difference(KeyModifiers::SHIFT).is_empty()
        && !all;
    let m = kitty_param(mods, key.state, !text);
    let csi_u = |out: &mut Vec<u8>, code: u32, m: u16| {
        if m != 1 {
            csi(&format!("{code};{m}"), b'u', out);
        } else {
            csi(&code.to_string(), b'u', out);
        }
    };
    match key.code {
        KeyCode::Char(c) if text => push_utf8(c, out),
        KeyCode::Char(c) => csi_u(out, base_code(c), m),
        KeyCode::Esc => csi_u(out, 27, m),
        KeyCode::Enter if !all && mods.is_empty() => out.push(b'\r'),
        KeyCode::Tab if !all && mods.is_empty() => out.push(b'\t'),
        KeyCode::Backspace if !all && mods.is_empty() => out.push(0x7f),
        KeyCode::Enter => csi_u(out, 13, m),
        KeyCode::Tab => csi_u(out, 9, m),
        KeyCode::BackTab => {
            let m = kitty_param(mods | KeyModifiers::SHIFT, key.state, true);
            csi_u(out, 9, m);
        }
        KeyCode::Backspace => csi_u(out, 127, m),
        KeyCode::Null => {
            let m = kitty_param(mods | KeyModifiers::CONTROL, key.state, true);
            csi_u(out, 32, m);
        }
        KeyCode::F(n @ 13..=20) => csi_u(out, 57376 + u32::from(n) - 13, m),
        _ => return functional(key.code, m, false, false, out),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn enc(key: KeyEvent, modes: Modes) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(encode(&key, modes, &mut out), "{key:?} has no encoding");
        out
    }

    fn check(modes: Modes, table: &[(KeyEvent, &[u8])]) {
        for (key, expected) in table {
            assert_eq!(enc(*key, modes), *expected, "{key:?} under {modes:?}");
        }
    }

    fn with(protocol: Protocol) -> Modes {
        Modes {
            protocol,
            ..Modes::default()
        }
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const META: KeyModifiers = KeyModifiers::META;
    const SUPER: KeyModifiers = KeyModifiers::SUPER;
    const KITTY: Protocol = Protocol::Kitty(KittyFlags::DISAMBIGUATE);

    #[test]
    fn legacy_text_and_control_keys() {
        check(
            Modes::default(),
            &[
                (key(KeyCode::Char('a'), NONE), b"a"),
                (key(KeyCode::Char('A'), SHIFT), b"A"),
                (key(KeyCode::Char('A'), NONE), b"A"),
                (key(KeyCode::Char('ñ'), NONE), "ñ".as_bytes()),
                (key(KeyCode::Char('c'), CTRL), b"\x03"),
                (key(KeyCode::Char(' '), CTRL), b"\x00"),
                (key(KeyCode::Char('['), CTRL), b"\x1b"),
                (key(KeyCode::Char('/'), CTRL), b"\x1f"),
                (key(KeyCode::Char('~'), CTRL), b"\x1e"),
                (key(KeyCode::Char('-'), CTRL), b"-"),
                (key(KeyCode::Char('a'), ALT), b"\x1ba"),
                (key(KeyCode::Char('c'), CTRL | ALT), b"\x1b\x03"),
                (key(KeyCode::Char('1'), CTRL), b"1"),
                (key(KeyCode::Char('a'), SUPER), b"a"),
                (key(KeyCode::Enter, NONE), b"\r"),
                (key(KeyCode::Enter, SHIFT), b"\r"),
                (key(KeyCode::Enter, ALT), b"\x1b\r"),
                (key(KeyCode::Tab, NONE), b"\t"),
                (key(KeyCode::BackTab, SHIFT), b"\x1b[Z"),
                (key(KeyCode::Backspace, NONE), b"\x7f"),
                (key(KeyCode::Backspace, CTRL), b"\x08"),
                (key(KeyCode::Esc, NONE), b"\x1b"),
                (key(KeyCode::Esc, ALT), b"\x1b\x1b"),
                (key(KeyCode::Null, NONE), b"\x00"),
            ],
        );
    }

    #[test]
    fn legacy_functional_keys() {
        check(
            Modes::default(),
            &[
                (key(KeyCode::Up, NONE), b"\x1b[A"),
                (key(KeyCode::Left, SHIFT), b"\x1b[1;2D"),
                (key(KeyCode::Right, ALT), b"\x1b[1;3C"),
                (key(KeyCode::Up, META), b"\x1b[1;9A"),
                (key(KeyCode::Up, SUPER), b"\x1b[A"),
                (key(KeyCode::Home, NONE), b"\x1b[H"),
                (key(KeyCode::End, CTRL), b"\x1b[1;5F"),
                (key(KeyCode::Insert, NONE), b"\x1b[2~"),
                (key(KeyCode::Delete, SHIFT), b"\x1b[3;2~"),
                (key(KeyCode::PageUp, NONE), b"\x1b[5~"),
                (key(KeyCode::PageDown, NONE), b"\x1b[6~"),
                (key(KeyCode::F(1), NONE), b"\x1bOP"),
                (key(KeyCode::F(3), NONE), b"\x1bOR"),
                (key(KeyCode::F(4), SHIFT), b"\x1b[1;2S"),
                (key(KeyCode::F(5), NONE), b"\x1b[15~"),
                (key(KeyCode::F(12), CTRL), b"\x1b[24;5~"),
                (key(KeyCode::F(13), NONE), b"\x1b[25~"),
                (key(KeyCode::F(20), SHIFT), b"\x1b[34;2~"),
            ],
        );
        let app = Modes {
            application_cursor: true,
            ..Modes::default()
        };
        check(
            app,
            &[
                (key(KeyCode::Up, NONE), b"\x1bOA"),
                (key(KeyCode::Up, SHIFT), b"\x1b[1;2A"),
                (key(KeyCode::Home, NONE), b"\x1bOH"),
            ],
        );
    }

    #[test]
    fn keys_without_an_encoding_are_reported() {
        let mut out = Vec::new();
        assert!(!encode(
            &key(KeyCode::CapsLock, NONE),
            Modes::default(),
            &mut out
        ));
        assert!(out.is_empty());
    }

    #[test]
    fn modify_other_keys_reports_modified_keys() {
        let modes = with(Protocol::ModifyOtherKeys);
        check(
            modes,
            &[
                (key(KeyCode::Char('a'), NONE), b"a"),
                (key(KeyCode::Char('A'), SHIFT), b"A"),
                (key(KeyCode::Char(' '), SHIFT), b"\x1b[27;2;32~"),
                (key(KeyCode::Char('c'), CTRL), b"\x1b[27;5;99~"),
                (key(KeyCode::Char('a'), ALT), b"\x1b[27;3;97~"),
                (key(KeyCode::Char('A'), CTRL | SHIFT), b"\x1b[27;6;65~"),
                (key(KeyCode::Char('A'), CTRL), b"\x1b[27;6;65~"),
                (key(KeyCode::Char('a'), META), b"\x1b[27;9;97~"),
                (key(KeyCode::Char('a'), SUPER), b"a"),
                (key(KeyCode::Enter, NONE), b"\r"),
                (key(KeyCode::Enter, SHIFT), b"\x1b[27;2;13~"),
                (key(KeyCode::Enter, ALT), b"\x1b[27;3;13~"),
                (key(KeyCode::BackTab, SHIFT), b"\x1b[27;2;9~"),
                (key(KeyCode::Backspace, CTRL), b"\x08"),
                (key(KeyCode::Backspace, SHIFT), b"\x1b[27;2;127~"),
                (key(KeyCode::Backspace, CTRL | SHIFT), b"\x1b[27;6;8~"),
                (key(KeyCode::Esc, NONE), b"\x1b"),
                (key(KeyCode::Esc, SHIFT), b"\x1b[27;2;27~"),
                (key(KeyCode::Null, NONE), b"\x00"),
                (key(KeyCode::Up, CTRL), b"\x1b[1;5A"),
                (key(KeyCode::F(1), NONE), b"\x1bOP"),
            ],
        );
        let app = Modes {
            application_cursor: true,
            ..modes
        };
        check(app, &[(key(KeyCode::Up, NONE), b"\x1bOA")]);
    }

    #[test]
    fn kitty_disambiguate_moves_esc_and_modified_keys() {
        check(
            with(KITTY),
            &[
                (key(KeyCode::Char('a'), NONE), b"a"),
                (key(KeyCode::Char('A'), SHIFT), b"A"),
                (key(KeyCode::Char('c'), CTRL), b"\x1b[99;5u"),
                (key(KeyCode::Char('a'), ALT), b"\x1b[97;3u"),
                (key(KeyCode::Char('A'), SHIFT | ALT), b"\x1b[97;4u"),
                (key(KeyCode::Char('A'), CTRL), b"\x1b[97;6u"),
                (key(KeyCode::Char('c'), SUPER), b"\x1b[99;9u"),
                (key(KeyCode::Esc, NONE), b"\x1b[27u"),
                (key(KeyCode::Esc, CTRL), b"\x1b[27;5u"),
                (key(KeyCode::Enter, NONE), b"\r"),
                (key(KeyCode::Enter, SHIFT), b"\x1b[13;2u"),
                (key(KeyCode::Enter, ALT), b"\x1b[13;3u"),
                (key(KeyCode::Enter, CTRL), b"\x1b[13;5u"),
                (key(KeyCode::Tab, NONE), b"\t"),
                (key(KeyCode::BackTab, SHIFT), b"\x1b[9;2u"),
                (key(KeyCode::Backspace, NONE), b"\x7f"),
                (key(KeyCode::Backspace, CTRL), b"\x1b[127;5u"),
                (key(KeyCode::Null, NONE), b"\x1b[32;5u"),
                (key(KeyCode::Up, NONE), b"\x1b[A"),
                (key(KeyCode::Up, SHIFT), b"\x1b[1;2A"),
                (key(KeyCode::Up, META), b"\x1b[1;33A"),
                (key(KeyCode::Home, NONE), b"\x1b[H"),
                (key(KeyCode::F(1), NONE), b"\x1b[P"),
                (key(KeyCode::F(3), SHIFT), b"\x1b[13;2~"),
                (key(KeyCode::F(5), NONE), b"\x1b[15~"),
                (key(KeyCode::F(13), NONE), b"\x1b[57376u"),
                (key(KeyCode::F(20), SHIFT), b"\x1b[57383;2u"),
            ],
        );
        let app = Modes {
            application_cursor: true,
            ..with(KITTY)
        };
        check(app, &[(key(KeyCode::Up, NONE), b"\x1b[A")]);
    }

    #[test]
    fn kitty_reports_lock_modifiers_for_non_text_keys() {
        let mut shifted_arrow = key(KeyCode::Up, SHIFT);
        shifted_arrow.state = KeyEventState::CAPS_LOCK;
        let mut text = key(KeyCode::Char('a'), NONE);
        text.state = KeyEventState::CAPS_LOCK;
        check(with(KITTY), &[(shifted_arrow, b"\x1b[1;66A"), (text, b"a")]);
    }

    #[test]
    fn kitty_all_keys_as_escapes() {
        for flags in [
            KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPES,
            KittyFlags::REPORT_ALL_KEYS_AS_ESCAPES,
        ] {
            check(
                with(Protocol::Kitty(flags)),
                &[
                    (key(KeyCode::Char('a'), NONE), b"\x1b[97u"),
                    (key(KeyCode::Char('A'), SHIFT), b"\x1b[97;2u"),
                    (key(KeyCode::Enter, NONE), b"\x1b[13u"),
                    (key(KeyCode::Enter, SHIFT), b"\x1b[13;2u"),
                    (key(KeyCode::Tab, NONE), b"\x1b[9u"),
                    (key(KeyCode::BackTab, SHIFT), b"\x1b[9;2u"),
                    (key(KeyCode::Backspace, NONE), b"\x1b[127u"),
                ],
            );
        }
    }

    #[test]
    fn kitty_without_an_encoding_flag_is_legacy() {
        for flags in [
            KittyFlags(0),
            KittyFlags::REPORT_EVENT_TYPES,
            KittyFlags::REPORT_ALTERNATE_KEYS | KittyFlags::REPORT_ASSOCIATED_TEXT,
        ] {
            check(
                with(Protocol::Kitty(flags)),
                &[
                    (key(KeyCode::Esc, NONE), b"\x1b"),
                    (key(KeyCode::Char('c'), CTRL), b"\x03"),
                ],
            );
        }
    }

    #[test]
    fn pastes_convert_line_ends() {
        let mut out = Vec::new();
        encode_paste("a\r\nb\nc\td", Modes::default(), &mut out);
        assert_eq!(out, b"a\rb\rc\td");
    }

    #[test]
    fn pastes_cannot_carry_control_sequences() {
        let modes = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        let mut out = Vec::new();
        encode_paste("x\x1b[20\x1b[201~1~y\x07\u{9b}z\n", modes, &mut out);
        assert_eq!(out, b"\x1b[200~x[20[201~1~yz\r\x1b[201~");

        let mut out = Vec::new();
        encode_paste("\x1b[Aq", Modes::default(), &mut out);
        assert_eq!(out, b"[Aq");
    }

    #[test]
    fn encodes_wheel_reports_as_the_child_asked() {
        let wheel = |up, column, modifiers, encoding| {
            let kind = if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            };
            let mut out = Vec::new();
            encode_mouse(kind, column, 4, modifiers, encoding, &mut out);
            out
        };
        let none = KeyModifiers::NONE;
        assert_eq!(wheel(true, 9, none, MouseEncoding::Sgr), b"\x1b[<64;10;5M");
        assert_eq!(
            wheel(
                false,
                0,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                MouseEncoding::Sgr
            ),
            b"\x1b[<85;1;5M"
        );
        assert_eq!(
            wheel(true, 9, KeyModifiers::ALT, MouseEncoding::X10),
            [0x1b, b'[', b'M', 32 + 72, 32 + 10, 32 + 5]
        );
        // X10 carries columns up to 223, one byte each.
        assert_eq!(
            wheel(true, 222, none, MouseEncoding::X10).last(),
            Some(&(32 + 5))
        );
        assert_eq!(wheel(true, 222, none, MouseEncoding::X10)[4], 0xff);
        assert!(wheel(true, 223, none, MouseEncoding::X10).is_empty());
        // UTF-8 carries values past 95 as two bytes, up to 2015.
        assert_eq!(
            wheel(false, 300, none, MouseEncoding::Utf8),
            b"\x1b[M\x61\xc5\x8d\x25"
        );
        assert!(!wheel(false, 2014, none, MouseEncoding::Utf8).is_empty());
        assert!(wheel(false, 2015, none, MouseEncoding::Utf8).is_empty());
    }

    #[test]
    fn encodes_buttons_and_motion_as_the_child_asked() {
        let report = |kind, modifiers, encoding| {
            let mut out = Vec::new();
            encode_mouse(kind, 2, 1, modifiers, encoding, &mut out);
            out
        };
        let none = KeyModifiers::NONE;
        let sgr = MouseEncoding::Sgr;
        let x10 = MouseEncoding::X10;
        let left = MouseButton::Left;
        assert_eq!(
            report(MouseEventKind::Down(left), none, sgr),
            b"\x1b[<0;3;2M"
        );
        assert_eq!(
            report(
                MouseEventKind::Down(MouseButton::Right),
                KeyModifiers::CONTROL,
                sgr
            ),
            b"\x1b[<18;3;2M"
        );
        assert_eq!(
            report(MouseEventKind::Drag(left), none, sgr),
            b"\x1b[<32;3;2M"
        );
        assert_eq!(
            report(
                MouseEventKind::Drag(MouseButton::Middle),
                KeyModifiers::SHIFT,
                sgr
            ),
            b"\x1b[<37;3;2M"
        );
        assert_eq!(report(MouseEventKind::Moved, none, sgr), b"\x1b[<35;3;2M");
        // SGR names the released button and ends in `m`; the older
        // encodings report a release as button 3.
        assert_eq!(
            report(MouseEventKind::Up(MouseButton::Right), none, sgr),
            b"\x1b[<2;3;2m"
        );
        assert_eq!(
            report(MouseEventKind::Up(left), none, x10),
            [0x1b, b'[', b'M', 32 + 3, 32 + 3, 32 + 2]
        );
        assert_eq!(
            report(MouseEventKind::Up(left), KeyModifiers::CONTROL, x10),
            [0x1b, b'[', b'M', 32 + 19, 32 + 3, 32 + 2]
        );
        assert_eq!(
            report(MouseEventKind::Drag(left), none, MouseEncoding::Utf8),
            b"\x1b[M\x40\x23\x22"
        );
    }

    #[test]
    fn tracking_levels_add_drags_then_motion() {
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let down = MouseEventKind::Down(MouseButton::Left);
        for kind in [
            down,
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::ScrollUp,
        ] {
            assert!(Tracking::Buttons.reports(kind), "{kind:?}");
        }
        assert!(!Tracking::Buttons.reports(drag));
        assert!(Tracking::Drags.reports(drag));
        assert!(!Tracking::Drags.reports(MouseEventKind::Moved));
        assert!(Tracking::Motion.reports(drag));
        assert!(Tracking::Motion.reports(MouseEventKind::Moved));
    }
}
