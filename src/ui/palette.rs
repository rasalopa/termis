//! The colours termis draws with, named by what they mean. Truecolour,
//! like the theme the emulator reports to the agent; a 256-colour
//! multiplexer approximates them.

use ratatui::style::Color;

/// The focused pane's border and title, the commit box's border, the
/// mode tags and the key hints, the selection markers, a working agent, a
/// notice that something worked.
pub(super) const ACCENT: Color = Color::Rgb(0x62, 0xc3, 0xb1);
/// An agent asking the user something.
pub(super) const ASKING: Color = Color::Rgb(0xe8, 0x6e, 0x6e);
/// Something that failed.
pub(super) const ALERT: Color = Color::Rgb(0xe8, 0x6e, 0x6e);
/// Lines added, in the diff.
pub(super) const ADDED: Color = ACCENT;
/// Lines removed, in the diff.
pub(super) const REMOVED: Color = ALERT;
/// Labels, headings and the text of an empty pane: what recedes.
pub(super) const MUTED: Color = Color::DarkGray;
/// The borders of the panes without focus.
pub(super) const FAINT: Color = Color::DarkGray;
/// Behind the agent's name in the status bar.
pub(super) const CHIP: Color = Color::DarkGray;
