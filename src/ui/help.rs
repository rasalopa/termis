//! The list of every key, shown over the screen by `?`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::framed;
use super::palette::{ACCENT, MUTED};
use crate::strings;

/// Draws the key list in a rounded frame centred in `area`, over whatever
/// is there, two columns in from each side. The modes are set apart by
/// blank lines, and the list by a blank row over and under it, when the
/// height allows; when even the packed list does not fit, the bottom of
/// the frame says so.
pub fn render(frame: &mut Frame, area: Rect) {
    let mut lines = lines(true);
    if lines.len() + 2 > usize::from(area.height) {
        lines = self::lines(false);
    }
    let cut = lines.len() + 2 > usize::from(area.height);
    let airy = lines.len() + 4 <= usize::from(area.height);
    let widest = lines.iter().map(Line::width).max().unwrap_or(0);
    // The frame and two columns of padding on each side.
    let width = u16::try_from(widest + 6)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height = u16::try_from(lines.len() + if airy { 4 } else { 2 })
        .unwrap_or(u16::MAX)
        .min(area.height);
    if width < 3 || height < 3 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let accent = Style::new().fg(ACCENT);
    let title = Span::styled(strings::HELP_TITLE, accent.add_modifier(Modifier::BOLD));
    let blank = u16::from(airy);
    let mut block = framed(vec![title]).padding(Padding::new(2, 2, blank, blank));
    if cut {
        block = block.title_bottom(Line::styled(strings::HELP_CUT, accent));
    }
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One line per key under its mode, muted, the keys in bold in one
/// column, the modes set apart by a blank line when `spaced`.
fn lines(spaced: bool) -> Vec<Line<'static>> {
    let key_width = strings::HELP
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(key, _)| key.width())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (index, (mode, keys)) in strings::HELP.iter().enumerate() {
        if spaced && index > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(*mode, Style::new().fg(MUTED)));
        for (key, action) in *keys {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{key:<key_width$}"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {action}")),
            ]));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
            .collect()
    }

    fn draw(width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area()))
            .expect("draw");
        terminal
    }

    #[test]
    fn lists_every_mode_with_its_keys_in_a_column() {
        let lines = lines(true);
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
        for (mode, keys) in strings::HELP {
            assert!(text.contains(&(*mode).to_owned()), "{mode}");
            for (key, action) in *keys {
                assert!(
                    text.iter()
                        .any(|line| line.starts_with(key) && line.ends_with(action)),
                    "{key}: {action}"
                );
            }
        }
        let starts: Vec<usize> = lines
            .iter()
            .filter(|line| line.spans.len() == 2)
            .map(|line| line.spans[0].width())
            .collect();
        assert!(starts.windows(2).all(|pair| pair[0] == pair[1]), "aligned");
        assert_eq!(
            lines.len() - self::lines(false).len(),
            strings::HELP.len() - 1,
            "one blank line between modes"
        );
        assert!(
            self::lines(false).len() + 2 <= 30,
            "the packed list fits a 30-row terminal"
        );
        // `f` and the file tree's keys are on rows of their own modes;
        // `p` shares the row of `c`.
        let row = |key: &str| text.iter().find(|line| line.starts_with(key)).cloned();
        let files = row("c / p / z / f").expect("f in termis mode");
        assert!(
            files.ends_with("commit box / push / zoom / files or sessions"),
            "{files}"
        );
        let tree = row("l or Enter / h").expect("the tree's keys");
        assert!(tree.contains("open a dir"), "{tree}");
        assert!(tree.contains("(Enter: view a file)"), "{tree}");
        // The ends and the pages of every pane's list or view are on
        // termis mode's rows; the diff takes the agent pane's page keys.
        let ends = row("g / G").expect("the ends");
        assert!(
            ends.ends_with("the end of the pane's list or view"),
            "{ends}"
        );
        let pages = row("Ctrl+D / U, Ctrl+F / B").expect("the pages");
        assert!(
            pages.ends_with("half a page down / up, a page down / up"),
            "{pages}"
        );
        let page_keys = row("PgUp / PgDn").expect("the page keys");
        assert!(
            page_keys.ends_with("scroll the view or the diff a page"),
            "{page_keys}"
        );
        let panes = row("1 / 2 / 3 / Tab / Shift+Tab").expect("the panes");
        assert!(panes.ends_with("the next / previous one"), "{panes}");
        // `i` goes back to the agent with `Esc` and `Ctrl+Q`.
        let back = row("Esc, i or Ctrl+Q / ?").expect("the way back");
        assert!(back.ends_with("back to the agent / this list"), "{back}");
        // The commit box's `Ctrl+G` has a row; `Ctrl+J` shares `Enter`'s.
        let suggest = row("Ctrl+G").expect("the suggestion");
        assert!(
            suggest.ends_with("suggest a subject for what is staged"),
            "{suggest}"
        );
        let send = row("Ctrl+J / Enter").expect("a body line or the commit");
        assert!(send.ends_with("body line / commit"), "{send}");
    }

    fn cell(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> (Color, bool) {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or((Color::Reset, false), |c| {
                (c.fg, c.modifier.contains(Modifier::BOLD))
            })
    }

    #[test]
    fn draws_a_centred_box_with_room_to_spare() {
        let terminal = draw(100, 40);
        let top = (0..40u16)
            .find(|&y| row(&terminal, y).contains("╭─ keys ─"))
            .expect("the box");
        assert!(top > 0, "centred, not at the top");
        let left = u16::try_from(row(&terminal, top).find('╭').expect("corner")).expect("x");
        assert_eq!(cell(&terminal, left, top), (ACCENT, false), "the frame");
        assert_eq!(cell(&terminal, left + 3, top), (ACCENT, true), "the title");
        // A blank row over the list, two columns in from the frame.
        let blank = row(&terminal, top + 1);
        assert!(blank.trim_matches(['│', ' ']).is_empty(), "{blank}");
        let body = row(&terminal, top + 2);
        assert!(body.trim().starts_with("│  agent mode"), "{body}");
        // The modes muted, the keys in bold, what they do plain.
        assert_eq!(cell(&terminal, left + 3, top + 2), (MUTED, false));
        assert_eq!(cell(&terminal, left + 3, top + 3), (Color::Reset, true));
        let keys = row(&terminal, top + 3);
        let action = keys[..keys.find("termis mode").expect("an action")]
            .chars()
            .count();
        let action = u16::try_from(action).expect("x");
        assert_eq!(cell(&terminal, action, top + 3), (Color::Reset, false));
        let bottom = (top..40)
            .find(|&y| row(&terminal, y).contains('╰'))
            .expect("closed");
        assert_eq!(usize::from(bottom - top), lines(true).len() + 3);
        let blank = row(&terminal, bottom - 1);
        assert!(blank.trim_matches(['│', ' ']).is_empty(), "{blank}");
        assert!(!row(&terminal, bottom).contains('…'), "nothing cut");
    }

    #[test]
    fn packs_the_list_on_a_short_terminal_and_says_when_it_is_cut() {
        // Thirty rows hold the packed list, without the blank rows.
        let terminal = draw(80, 30);
        assert!(
            row(&terminal, 0).contains("╭─ keys ─"),
            "{}",
            row(&terminal, 0)
        );
        assert!(row(&terminal, 1).contains("│  agent mode"));
        let last = row(&terminal, 29);
        assert!(last.contains('╰') && !last.contains('…'), "{last}");
        let all: Vec<String> = (0..30).map(|y| row(&terminal, y)).collect();
        assert!(
            all.iter().any(|line| line.contains("commit box")),
            "last mode shown"
        );
        // Every row fits 80 columns whole.
        for (key, action) in strings::HELP.iter().flat_map(|(_, keys)| *keys) {
            assert!(
                all.iter()
                    .any(|line| line.contains(key) && line.contains(action)),
                "{key}: {action}"
            );
        }
        assert!(
            !all.iter().any(|line| line.trim() == "│"
                || line.trim_matches(['│', ' ']).is_empty() && line.contains('│')),
            "no blank lines inside"
        );

        let terminal = draw(80, 12);
        assert!(row(&terminal, 0).contains("╭─ keys ─"));
        assert!(row(&terminal, 11).contains('…'), "{}", row(&terminal, 11));

        for (w, h) in [(0, 0), (2, 2)] {
            let terminal = draw(w, h);
            assert!(
                !(0..h).any(|y| row(&terminal, y).contains('╭')),
                "nothing at {w}x{h}"
            );
        }
        let terminal = draw(3, 3);
        assert!(row(&terminal, 0).contains('╭'));
    }
}
