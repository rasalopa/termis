//! The diff of the selected row, shown in the changes pane in place of
//! the lists, or under them as a preview.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::changes::{cut, printable};
use super::palette;
use super::{heading, render_empty};
use crate::git::diff::{Read, Request};
use crate::strings;

/// The least rows the preview takes: its heading, a hunk's first line,
/// a line of the hunk and the line saying how many more there are.
pub const PREVIEW_ROWS: u16 = 4;

/// What a diff line is, which sets its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// File headers and the like, before the first hunk of a file.
    Header,
    /// A `@@` line.
    Hunk,
    Added,
    Removed,
    Context,
}

/// One line of the diff, ready to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffLine {
    text: String,
    kind: Kind,
}

/// The diff on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// What was asked of git, to tell its answer from an older one.
    pub request: Request,
    /// The path, as the row shows it.
    pub path: String,
    /// The lines, once git answered.
    lines: Option<Vec<DiffLine>>,
}

impl Diff {
    /// A diff waiting for git's answer to `request`.
    pub fn new(request: Request, path: String) -> Self {
        Self {
            request,
            path,
            lines: None,
        }
    }

    /// Takes `text` as the answer to `request` when it is the one waited
    /// for; an answer to another request is ignored. Returns whether it
    /// was taken.
    pub fn accept(&mut self, request: &Request, text: &str) -> bool {
        if self.request != *request {
            return false;
        }
        self.lines = Some(parse(text));
        true
    }

    /// How many lines it has to scroll through: none until the answer.
    pub fn lines(&self) -> usize {
        self.lines.as_ref().map_or(0, Vec::len)
    }
}

/// The lines of `text` with their kinds: everything before the first
/// `@@` of a file is a header, then `+` and `-` lines are additions and
/// removals until the next `diff` line.
fn parse(text: &str) -> Vec<DiffLine> {
    let mut in_hunk = false;
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("diff ") {
                in_hunk = false;
                Kind::Header
            } else if line.starts_with("@@") {
                in_hunk = true;
                Kind::Hunk
            } else if !in_hunk {
                Kind::Header
            } else if line.starts_with('+') {
                Kind::Added
            } else if line.starts_with('-') {
                Kind::Removed
            } else {
                Kind::Context
            };
            DiffLine {
                text: printable(&line.replace('\t', "    ")),
                kind,
            }
        })
        .collect()
}

/// Draws the header and the diff lines from `scroll` down, moved back
/// so that the last line stays at the bottom; the offset used is
/// returned for the next frame, with how many lines there was room for
/// under the header. The header is bold; added and removed lines take
/// their colours, hunk and file headers the muted one.
pub fn render(frame: &mut Frame, area: Rect, diff: &Diff, scroll: usize) -> (usize, usize) {
    if area.height == 0 || area.width == 0 {
        return (scroll, 0);
    }
    let width = usize::from(area.width);
    let title = strings::diff_header(&printable(&diff.path), diff.request.staged);
    let header = Line::styled(
        cut(&title, width),
        Style::new().add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Paragraph::new(header), area);
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    let height = usize::from(body.height);
    let Some(lines) = &diff.lines else {
        return (0, height);
    };
    if lines.is_empty() {
        render_empty(frame, body, strings::DIFF_EMPTY);
        return (0, height);
    }
    let scroll = scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(height)
        .map(|line| Line::styled(cut(&line.text, width), style(line.kind)))
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    (scroll, height)
}

/// The diff of the selected row as the preview under the lists shows
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    /// What was asked of git, to tell its answer from an older one.
    pub request: Request,
    /// The file's name, for the heading.
    name: String,
    /// What git answered, once it did, or why it could not.
    read: Option<Result<Body, String>>,
}

/// What the preview shows of git's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Body {
    /// The lines from the first hunk on, see [`preview_lines`].
    lines: Vec<DiffLine>,
    /// Whether git printed more than was read.
    cut: bool,
}

impl Preview {
    /// A preview waiting for git's answer to `request`, headed by `name`.
    pub fn new(request: Request, name: String) -> Self {
        Self {
            request,
            name,
            read: None,
        }
    }

    /// Takes `result` as the answer to `request` when it is the one
    /// waited for; an answer to another request is ignored. Returns
    /// whether what it shows changed.
    pub fn accept(&mut self, request: &Request, result: Result<&Read, String>) -> bool {
        if self.request != *request {
            return false;
        }
        let read = Some(result.map(|read| Body {
            lines: preview_lines(&read.text),
            cut: read.cut,
        }));
        let changed = self.read != read;
        self.read = read;
        changed
    }
}

/// The lines of `text` the preview shows: from the first hunk on, the
/// headers of each file left out. With no hunk, as for a binary file, a
/// mode change, a rename alone or an empty new file, the headers that
/// say what changed: all but the `diff` and `index` lines.
fn preview_lines(text: &str) -> Vec<DiffLine> {
    let lines = parse(text);
    if lines.iter().any(|line| line.kind == Kind::Hunk) {
        lines
            .into_iter()
            .skip_while(|line| line.kind != Kind::Hunk)
            .filter(|line| line.kind != Kind::Header)
            .collect()
    } else {
        lines
            .into_iter()
            .filter(|line| !line.text.starts_with("diff ") && !line.text.starts_with("index "))
            .collect()
    }
}

/// Draws `preview` in `area`: its heading, then as many of its lines as
/// fit, the last row saying how many are left when they do not all fit;
/// nothing under the heading until git answers, the reason in the alert
/// colour when it could not.
pub fn render_preview(frame: &mut Frame, area: Rect, preview: &Preview) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let palette = palette::current();
    let width = usize::from(area.width);
    let mut shown = vec![heading(&strings::preview_heading(&preview.name), width)];
    let room = usize::from(area.height - 1);
    match &preview.read {
        None => {}
        Some(Err(error)) => shown.push(Line::styled(
            cut(&printable(error), width),
            Style::new().fg(palette.alert),
        )),
        Some(Ok(body)) if body.lines.is_empty() && !body.cut => shown.push(Line::styled(
            cut(strings::DIFF_EMPTY, width),
            Style::new().fg(palette.muted),
        )),
        Some(Ok(body)) => {
            let fits = !body.cut && body.lines.len() <= room;
            let take = if fits { room } else { room.saturating_sub(1) };
            shown.extend(
                body.lines
                    .iter()
                    .take(take)
                    .map(|line| Line::styled(cut(&line.text, width), style(line.kind))),
            );
            if !fits {
                // After a read cut short, git printed the rest of a line
                // at least.
                let left = body.lines.len().saturating_sub(take);
                let left = if body.cut { left.max(1) } else { left };
                shown.push(more_line(left, body.cut, width));
            }
        }
    }
    frame.render_widget(Paragraph::new(shown), area);
}

/// The preview's last row when its lines do not all fit: how many are
/// `left`, muted, then the key that shows them all after a faint
/// separator, when it fits the `width`.
fn more_line(left: usize, read_cut: bool, width: usize) -> Line<'static> {
    let palette = palette::current();
    let text = strings::preview_more(left, read_cut);
    let key = strings::PREVIEW_KEY;
    if text.width() + strings::HINT_SEPARATOR.width() + key.width() <= width {
        Line::from(vec![
            Span::styled(text, Style::new().fg(palette.muted)),
            Span::styled(strings::HINT_SEPARATOR, Style::new().fg(palette.faint)),
            Span::raw(key),
        ])
    } else {
        Line::styled(cut(&text, width), Style::new().fg(palette.muted))
    }
}

fn style(kind: Kind) -> Style {
    let palette = palette::current();
    match kind {
        Kind::Header | Kind::Hunk => Style::new().fg(palette.muted),
        Kind::Added => Style::new().fg(palette.added),
        Kind::Removed => Style::new().fg(palette.removed),
        Kind::Context => Style::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::palette::{ADDED, ALERT, FAINT, MUTED, REMOVED};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use std::path::PathBuf;

    const TEXT: &str = "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n one\n-two\n+two!\n+three\n";

    fn request(staged: bool) -> Request {
        Request {
            paths: vec![PathBuf::from("f.txt")],
            staged,
            untracked: false,
        }
    }

    fn diff(text: Option<&str>) -> Diff {
        let mut diff = Diff::new(request(false), "f.txt".into());
        if let Some(text) = text {
            assert!(diff.accept(&request(false), text));
        }
        diff
    }

    fn draw(width: u16, height: u16, diff: &Diff, scroll: usize) -> (Terminal<TestBackend>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut offset = 0;
        terminal
            .draw(|frame| (offset, _) = render(frame, frame.area(), diff, scroll))
            .expect("draw");
        (terminal, offset)
    }

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

    #[test]
    fn lines_are_kinds_by_position_not_by_prefix_alone() {
        let kinds: Vec<Kind> = parse(TEXT).into_iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Header,
                Kind::Header,
                Kind::Header,
                Kind::Header,
                Kind::Hunk,
                Kind::Context,
                Kind::Removed,
                Kind::Added,
                Kind::Added,
            ]
        );
        // Content that looks like a header is content inside a hunk.
        let tricky = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n--- note\n+++i;\n\\ No newline at end of file\ndiff --git a/y b/y\n--- a/y\n";
        let kinds: Vec<Kind> = parse(tricky).into_iter().map(|line| line.kind).collect();
        assert_eq!(kinds[4], Kind::Removed);
        assert_eq!(kinds[5], Kind::Added);
        assert_eq!(kinds[6], Kind::Context);
        assert_eq!(kinds[7], Kind::Header, "a new file starts headers again");
        assert_eq!(kinds[8], Kind::Header);
        let tab = parse("+\tx\n");
        assert_eq!(tab[0].text, "+    x");
    }

    #[test]
    fn answers_to_other_requests_are_ignored() {
        let mut d = Diff::new(request(false), "f.txt".into());
        assert!(!d.accept(&request(true), TEXT), "another side of the row");
        assert!(d.lines.is_none());
        assert_eq!(d.lines(), 0);
        assert!(d.accept(&request(false), TEXT));
        assert_eq!(d.lines.as_ref().map(Vec::len), Some(9));
        assert_eq!(d.lines(), 9);
    }

    #[test]
    fn has_room_for_the_lines_under_its_header() {
        for (text, height, room) in [
            (Some(TEXT), 6, 5),
            (Some(""), 6, 5),
            (None, 6, 5),
            (Some(TEXT), 1, 0),
            (Some(TEXT), 0, 0),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(30, height)).expect("terminal");
            let mut drawn = (9, 9);
            terminal
                .draw(|frame| drawn = render(frame, frame.area(), &diff(text), 0))
                .expect("draw");
            assert_eq!(drawn.1, room, "{text:?} in {height} rows");
        }
    }

    #[test]
    fn colours_the_lines_and_scrolls_to_the_end() {
        let d = diff(Some(TEXT));
        let (terminal, offset) = draw(30, 6, &d, 0);
        assert_eq!(offset, 0);
        assert!(
            row(&terminal, 0).starts_with("diff f.txt"),
            "{}",
            row(&terminal, 0)
        );
        assert_eq!(fg(&terminal, 0, 0), Color::Reset, "header");
        assert!(
            terminal
                .backend()
                .buffer()
                .cell((0, 0))
                .is_some_and(|c| c.modifier.contains(Modifier::BOLD)),
            "the header is bold"
        );
        assert!(row(&terminal, 1).starts_with("diff --git"));
        assert_eq!(fg(&terminal, 0, 1), MUTED);
        assert!(
            row(&terminal, 5).starts_with("@@ -1,2"),
            "{}",
            row(&terminal, 5)
        );

        let (terminal, offset) = draw(30, 6, &d, 100);
        assert_eq!(offset, 4, "no further than the last line");
        assert!(row(&terminal, 1).starts_with("@@"));
        assert_eq!(row(&terminal, 2).trim_end(), " one");
        assert_eq!(fg(&terminal, 0, 2), Color::Reset, "context");
        assert_eq!(row(&terminal, 3).trim_end(), "-two");
        assert_eq!(fg(&terminal, 0, 3), REMOVED);
        assert_eq!(row(&terminal, 4).trim_end(), "+two!");
        assert_eq!(fg(&terminal, 0, 4), ADDED);
        assert_eq!(row(&terminal, 5).trim_end(), "+three");

        let mut staged = Diff::new(request(true), "f.txt".into());
        staged.accept(&request(true), TEXT);
        let (terminal, _) = draw(30, 6, &staged, 0);
        assert!(row(&terminal, 0).starts_with("diff f.txt (staged)"));
    }

    #[test]
    fn says_when_there_is_nothing_and_waits_for_git() {
        let (terminal, offset) = draw(30, 6, &diff(Some("")), 3);
        assert_eq!(offset, 0);
        let all: String = (0..6).map(|y| row(&terminal, y)).collect();
        assert!(all.contains("nothing to show"), "{all}");
        let (terminal, offset) = draw(30, 6, &diff(None), 3);
        assert_eq!(offset, 0);
        assert!(row(&terminal, 0).starts_with("diff f.txt"));
        assert!(row(&terminal, 1).trim().is_empty());
    }

    /// The preview of f.txt with git's `answer`, the text read or why it
    /// could not be, once there is one; `cut` when git printed more.
    fn preview(answer: Option<Result<&str, &str>>, cut: bool) -> Preview {
        let mut preview = Preview::new(request(false), "f.txt".into());
        if let Some(answer) = answer {
            let read = answer.map(|text| Read {
                text: text.to_owned(),
                cut,
            });
            let result = read.as_ref().map_err(|error| (*error).to_owned());
            assert!(preview.accept(&request(false), result));
        }
        preview
    }

    /// The rows of `preview` drawn in `width` by `height`, their ends
    /// trimmed, with the terminal for the colours.
    fn draw_preview(
        width: u16,
        height: u16,
        preview: &Preview,
    ) -> (Terminal<TestBackend>, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render_preview(frame, frame.area(), preview))
            .expect("draw");
        let rows = (0..height)
            .map(|y| row(&terminal, y).trim_end().to_owned())
            .collect();
        (terminal, rows)
    }

    /// A link turned into a file: two sections, each with a hunk.
    const RETYPED: &str = "diff --git a/l b/l\ndeleted file mode 120000\nindex 8d14cbf..0000000\n--- a/l\n+++ /dev/null\n@@ -1 +0,0 @@\n-a.txt\n\\ No newline at end of file\ndiff --git a/l b/l\nnew file mode 100644\nindex 0000000..3bcf9fd\n--- /dev/null\n+++ b/l\n@@ -0,0 +1 @@\n+notlink\n";

    #[test]
    fn a_preview_starts_at_the_first_hunk() {
        let texts = |text| -> Vec<String> {
            preview_lines(text)
                .into_iter()
                .map(|line| line.text)
                .collect()
        };
        assert_eq!(
            texts(TEXT),
            ["@@ -1,2 +1,3 @@", " one", "-two", "+two!", "+three"]
        );
        assert_eq!(
            texts(RETYPED),
            [
                "@@ -1 +0,0 @@",
                "-a.txt",
                "\\ No newline at end of file",
                "@@ -0,0 +1 @@",
                "+notlink"
            ],
            "the second file's headers left out"
        );
    }

    #[test]
    fn a_diff_without_a_hunk_says_what_changed() {
        for (text, wanted) in [
            (
                "diff --git a/b.bin b/b.bin\nindex 6772730..8d0cd2a 100644\nBinary files a/b.bin and b/b.bin differ\n",
                &["Binary files a/b.bin and b/b.bin differ"][..],
            ),
            (
                "diff --git a/m.sh b/m.sh\nold mode 100644\nnew mode 100755\n",
                &["old mode 100644", "new mode 100755"],
            ),
            (
                "diff --git a/r.txt b/r2.txt\nsimilarity index 100%\nrename from r.txt\nrename to r2.txt\n",
                &[
                    "similarity index 100%",
                    "rename from r.txt",
                    "rename to r2.txt",
                ],
            ),
            (
                "diff --git a/./e b/./e\nnew file mode 100644\nindex 0000000..e69de29\n",
                &["new file mode 100644"],
            ),
        ] {
            let lines = preview_lines(text);
            let shown: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
            assert_eq!(shown, wanted);
            assert!(lines.iter().all(|line| line.kind == Kind::Header));
            let (terminal, _) = draw_preview(30, 4, &preview(Some(Ok(text)), false));
            assert_eq!(fg(&terminal, 0, 1), MUTED, "{text}");
        }
    }

    #[test]
    fn a_preview_fits_or_says_how_many_lines_are_left() {
        let whole = preview(Some(Ok(TEXT)), false);
        let (_, rows) = draw_preview(30, 6, &whole);
        assert_eq!(
            rows,
            [
                format!("diff f.txt {}", "─".repeat(19)),
                "@@ -1,2 +1,3 @@".into(),
                " one".into(),
                "-two".into(),
                "+two!".into(),
                "+three".into(),
            ]
        );
        let (terminal, rows) = draw_preview(30, 4, &whole);
        assert_eq!(rows[1..], ["@@ -1,2 +1,3 @@", " one", "↓ 3 more lines · d"]);
        assert_eq!(fg(&terminal, 0, 0), MUTED, "the heading");
        assert_eq!(fg(&terminal, 0, 1), MUTED, "the hunk's line");
        assert_eq!(fg(&terminal, 0, 3), MUTED, "the count");
        assert_eq!(fg(&terminal, 15, 3), FAINT, "the separator");
        assert_eq!(fg(&terminal, 17, 3), Color::Reset, "the key");
        let (terminal, _) = draw_preview(30, 5, &whole);
        assert_eq!(fg(&terminal, 0, 3), REMOVED);
        // Narrow, the key goes first.
        assert_eq!(draw_preview(17, 4, &whole).1[3], "↓ 3 more lines");
        // Cut short: at least one more, even when the lines read fit.
        let cut = preview(Some(Ok(TEXT)), true);
        assert_eq!(draw_preview(30, 4, &cut).1[3], "↓ 3+ more lines · d");
        assert_eq!(draw_preview(30, 10, &cut).1[6], "↓ 1+ more lines · d");
    }

    #[test]
    fn a_preview_waits_says_why_or_that_nothing_differs() {
        let (_, waiting) = draw_preview(30, 3, &preview(None, false));
        assert!(waiting[0].starts_with("diff f.txt ─"), "{}", waiting[0]);
        assert_eq!(waiting[1..], ["", ""]);
        let failed = preview(Some(Err("nested is a directory")), false);
        let (terminal, rows) = draw_preview(14, 3, &failed);
        assert_eq!(rows[1], "nested is a d…");
        assert_eq!(fg(&terminal, 0, 1), ALERT);
        let (terminal, rows) = draw_preview(30, 3, &preview(Some(Ok("")), false));
        assert_eq!(rows[1], "nothing to show");
        assert_eq!(fg(&terminal, 0, 1), MUTED);
        for (w, h) in [(0, 0), (1, 1), (5, 1), (3, 2)] {
            draw_preview(w, h, &preview(Some(Ok(TEXT)), false));
        }
    }

    #[test]
    fn a_preview_takes_answers_to_its_own_request_once() {
        let mut p = Preview::new(request(false), "f.txt".into());
        let read = |text: &str| Read {
            text: text.to_owned(),
            cut: false,
        };
        assert!(!p.accept(&request(true), Ok(&read(TEXT))), "another side");
        assert_eq!(p.read, None);
        assert!(p.accept(&request(false), Ok(&read(TEXT))));
        assert!(
            !p.accept(&request(false), Ok(&read(TEXT))),
            "the same again"
        );
        assert!(p.accept(&request(false), Ok(&read("+x\n"))), "another text");
        assert!(p.accept(&request(false), Err("gone".into())), "a failure");
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (5, 1), (3, 2)] {
            draw(w, h, &diff(Some(TEXT)), 1);
        }
    }

    #[test]
    fn long_lines_fit_the_width() {
        let d = diff(Some("+\tindented and quite a long line here\n"));
        let (terminal, _) = draw(20, 3, &d, 0);
        let line = row(&terminal, 1);
        assert!(line.starts_with("+    indented"), "{line}");
        assert!(line.ends_with('…'), "{line}");
    }
}
