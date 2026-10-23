//! The colours termis draws with, named by what they mean. Truecolour,
//! like the theme the emulator reports to the agent; a 256-colour
//! multiplexer approximates them.

use ratatui::style::Color;

/// The terminal's own text colour, for text that would otherwise take the
/// colour around it: a title's subject on the accent frame.
pub(super) const TEXT: Color = Color::Reset;
/// The highlighted pane's frame and name, the commit box's frame, the
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
/// Labels, headings, the names of the panes without a frame, the details
/// of a title and the text of an empty pane: what recedes.
pub(super) const MUTED: Color = Color::Rgb(0x8b, 0x92, 0x9c);
/// The rule between two panes without a frame, the rule after a heading
/// and the separators of a title: lines that only give structure.
pub(super) const FAINT: Color = Color::Rgb(0x46, 0x4b, 0x53);
/// Behind the agent's name in the status bar.
pub(super) const CHIP: Color = Color::DarkGray;
