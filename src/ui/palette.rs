//! The colours termis draws with, named by what they mean. The constants
//! are the defaults: truecolour, like the theme the emulator reports to
//! the agent, which a 256-colour multiplexer approximates, and chosen for
//! a dark background, where on a light one the amber, green and teal
//! text is hard to read and the faint lines turn dark, while the chips
//! keep their contrast. A frame is drawn with the palette of its view,
//! which the drawing code reads through [`current`].

use std::cell::Cell;

use ratatui::style::Color;

/// The terminal's own text colour, for text that would otherwise take
/// another: a title's subject on the accent frame, the lines of the
/// viewer, the share of a limit that is not running out.
pub(super) const TEXT: Color = Color::Reset;
/// The highlighted pane's frame and name, the frames of the picker, the
/// key list and the commit box, the mode tags, the selection markers, a
/// working agent, the context in use, a notice that something worked.
pub(super) const ACCENT: Color = Color::Rgb(0x62, 0xc3, 0xb1);
/// An agent asking the user something; also a share of the context or
/// of a limit that is running out.
pub(super) const ASKING: Color = Color::Rgb(0xff, 0xb5, 0x47);
/// Something that failed, the letter of a file in conflict, and a share
/// of the context or of a limit that is nearly used up.
pub(super) const ALERT: Color = Color::Rgb(0xe8, 0x6e, 0x6e);
/// Lines added, and the letter of a file added or untracked.
pub(super) const ADDED: Color = Color::Rgb(0x8c, 0xcf, 0x7e);
/// Lines removed, and the letter of a file deleted.
pub(super) const REMOVED: Color = Color::Rgb(0xe8, 0x83, 0x88);
/// The letter of a file modified, renamed, copied or changed in type, and
/// the dot of a session whose repository has uncommitted changes.
pub(super) const MODIFIED: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
/// Labels, headings, the names of the panes without a frame, the details
/// of a title, what the hints' keys do and the text of an empty pane:
/// what recedes.
pub(super) const MUTED: Color = Color::Rgb(0x8b, 0x92, 0x9c);
/// The rule between two panes without a frame, the rule after a heading
/// and under the picker's query, the separators of a title and of the
/// hints, the gauge's track: lines that only give structure.
pub(super) const FAINT: Color = Color::Rgb(0x46, 0x4b, 0x53);
/// Behind the grey chips: the agent's name and a push running in the
/// status bar, a subject being suggested after the commit box's title,
/// the line over the screen of an agent that ended.
pub(super) const CHIP: Color = Color::Rgb(0x3a, 0x3f, 0x47);
/// Text on a grey chip.
pub(super) const CHIP_TEXT: Color = Color::Rgb(0xe6, 0xe8, 0xeb);
/// Text on a chip in a colour: the mode tags, the state of an agent that
/// asks and the count of those asking.
pub(super) const INK: Color = Color::Rgb(0x1a, 0x1c, 0x20);

/// The colours of a frame, one for each meaning: each field is what the
/// constant of the same name in upper case marks, and defaults to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub accent: Color,
    pub asking: Color,
    pub alert: Color,
    pub added: Color,
    pub removed: Color,
    pub modified: Color,
    pub muted: Color,
    pub faint: Color,
    pub chip: Color,
    pub chip_text: Color,
    pub ink: Color,
}

impl Palette {
    /// The default colours, for a dark background.
    pub const DEFAULT: Self = Self {
        text: TEXT,
        accent: ACCENT,
        asking: ASKING,
        alert: ALERT,
        added: ADDED,
        removed: REMOVED,
        modified: MODIFIED,
        muted: MUTED,
        faint: FAINT,
        chip: CHIP,
        chip_text: CHIP_TEXT,
        ink: INK,
    };
}

impl Default for Palette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

thread_local! {
    /// The palette of the frame this thread draws, the default outside
    /// one.
    static CURRENT: Cell<Palette> = const { Cell::new(Palette::DEFAULT) };
}

/// Runs `draw`, which draws a frame, with `palette` as [`current`], and
/// puts the palette before it back once it returns or unwinds.
pub(super) fn drawing<T>(palette: Palette, draw: impl FnOnce() -> T) -> T {
    /// Puts the palette it holds back as the current one when dropped.
    struct Restore(Palette);

    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.set(self.0);
        }
    }

    let _restore = Restore(CURRENT.replace(palette));
    draw()
}

/// The palette of the frame being drawn, as [`drawing`] set it; the
/// default outside a frame, as when a test draws a pane on its own.
pub(super) fn current() -> Palette {
    CURRENT.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_draws_with_its_palette_and_puts_the_one_before_back() {
        let red = Palette {
            accent: Color::Red,
            ..Palette::DEFAULT
        };
        assert_eq!(Palette::default(), Palette::DEFAULT);
        assert_eq!(current(), Palette::DEFAULT, "outside a frame");
        let inside = drawing(red, || {
            // A frame drawn inside another one, as no caller does.
            let nested = drawing(Palette::DEFAULT, current);
            (current(), nested)
        });
        assert_eq!(inside, (red, Palette::DEFAULT));
        assert_eq!(current(), Palette::DEFAULT, "after the frame");
        // Also when the drawing panics.
        let unwound = std::panic::catch_unwind(|| drawing(red, || panic!("a bug")));
        assert!(unwound.is_err());
        assert_eq!(current(), Palette::DEFAULT, "after a panic");
    }
}
