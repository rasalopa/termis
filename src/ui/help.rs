//! The list of every key, shown over the screen by `?`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::ACCENT;
use crate::strings;

/// Draws the key list in a box centred in `area`, over whatever is
/// there. The modes are set apart by blank lines when the height allows;
/// when even the packed list does not fit, the bottom border says so.
pub fn render(frame: &mut Frame, area: Rect) {
    let mut lines = lines(true);
    if lines.len() + 2 > usize::from(area.height) {
        lines = self::lines(false);
    }
    let cut = lines.len() + 2 > usize::from(area.height);
    let widest = lines.iter().map(Line::width).max().unwrap_or(0);
    // Borders and one column of padding on each side.
    let width = u16::try_from(widest + 4)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height = u16::try_from(lines.len() + 2)
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
    let mut block = Block::bordered()
        .border_style(accent)
        .title_style(accent)
        .title(format!(" {} ", strings::HELP_TITLE))
        .padding(Padding::horizontal(1));
    if cut {
        block = block.title_bottom(strings::HELP_CUT);
    }
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One line per key under its mode, keys in one column, the modes set
/// apart by a blank line when `spaced`.
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
        lines.push(Line::styled(
            *mode,
            Style::new().add_modifier(Modifier::BOLD),
        ));
        for (key, action) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<key_width$}"), Style::new().fg(ACCENT)),
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
            self::lines(false).len() + 2 <= 24,
            "the packed list fits a 24-row terminal"
        );
    }

    #[test]
    fn draws_a_centred_box_with_room_to_spare() {
        let terminal = draw(100, 40);
        let top = (0..40u16)
            .find(|&y| row(&terminal, y).contains("┌ keys ─"))
            .expect("the box");
        assert!(top > 0, "centred, not at the top");
        let body = row(&terminal, top + 1);
        assert!(body.trim().starts_with("│ agent mode"), "{body}");
        let bottom = (top..40)
            .find(|&y| row(&terminal, y).contains('└'))
            .expect("closed");
        assert_eq!(usize::from(bottom - top), lines(true).len() + 1);
        assert!(!row(&terminal, bottom).contains('…'), "nothing cut");
    }

    #[test]
    fn packs_the_list_on_a_short_terminal_and_says_when_it_is_cut() {
        let terminal = draw(80, 24);
        assert!(
            row(&terminal, 0).contains("┌ keys ─"),
            "{}",
            row(&terminal, 0)
        );
        let last = row(&terminal, 23);
        assert!(last.contains('└') && !last.contains('…'), "{last}");
        let all: Vec<String> = (0..24).map(|y| row(&terminal, y)).collect();
        assert!(
            all.iter().any(|line| line.contains("commit box")),
            "last mode shown"
        );
        assert!(
            !all.iter().any(|line| line.trim() == "│"
                || line.trim_matches(['│', ' ']).is_empty() && line.contains('│')),
            "no blank lines inside"
        );

        let terminal = draw(80, 12);
        assert!(row(&terminal, 0).contains("┌ keys ─"));
        assert!(row(&terminal, 11).contains('…'), "{}", row(&terminal, 11));

        for (w, h) in [(0, 0), (2, 2)] {
            let terminal = draw(w, h);
            assert!(
                !(0..h).any(|y| row(&terminal, y).contains('┌')),
                "nothing at {w}x{h}"
            );
        }
        let terminal = draw(3, 3);
        assert!(row(&terminal, 0).contains('┌'));
    }
}
