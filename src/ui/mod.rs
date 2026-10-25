//! Drawing: the three panes, the status bar, and the full-screen view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use tui_term::widget::Cursor;
use unicode_width::UnicodeWidthStr;

use crate::emu::Emulator;
use crate::git::status::Branch;
use crate::git::worker::Snapshot;
use crate::strings::{self, TitlePart};
use palette::{ACCENT, ADDED, ALERT, ASKING, CHIP, CHIP_TEXT, FAINT, INK, MUTED, REMOVED, TEXT};

pub mod changes;
pub mod commit;
pub mod diff;
pub mod help;
pub mod layout;
mod palette;
pub mod picker;
pub mod projects;
pub mod toast;

/// One of the three panes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    Projects,
    #[default]
    Agent,
    Changes,
}

/// Where keys go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// To the agent, as typed.
    #[default]
    Agent,
    /// To termis's own commands, with the pane they act on highlighted.
    Term(Pane),
    /// To the commit box in the changes pane.
    Commit,
    /// To the project picker, over the screen.
    Picker,
}

impl Mode {
    /// The highlighted pane: the agent's while keys go to it or to the
    /// picker.
    pub fn pane(self) -> Pane {
        match self {
            Self::Agent | Self::Picker => Pane::Agent,
            Self::Term(pane) => pane,
            Self::Commit => Pane::Changes,
        }
    }
}

/// What the three-pane view shows.
pub struct View<'a> {
    /// Name of the agent in the center pane title, and in the status bar
    /// while keys go to it.
    pub agent: &'a str,
    /// Name of the project in the center pane title and the status bar.
    pub project: &'a str,
    /// The names of the session's terminals, after the agent's tab in the
    /// center pane title.
    pub terminals: &'a [&'a str],
    /// The index of the terminal shown among them, whose name the status
    /// bar shows while keys go to it; none on the agent's tab.
    pub terminal: Option<usize>,
    /// Where the session shown is among several, from 1, and how many
    /// there are, for the status bar; none while there is one.
    pub position: Option<(usize, usize)>,
    /// The screen of the tab shown.
    pub emulator: &'a Emulator,
    /// Where keys go, with the highlighted pane. The app keeps the
    /// highlight on the agent's pane while `side_panes` is false.
    pub mode: Mode,
    /// Whether the side panes are on screen.
    pub side_panes: bool,
    /// Whether the side panes are hidden by the zoom rather than by the
    /// width.
    pub zoomed: bool,
    /// The line to draw over the agent's screen once it ended.
    pub ended: Option<&'a str>,
    /// Whether the directory is inside a git repository.
    pub repo: bool,
    /// The repository as last read, once the worker read it.
    pub changes: Option<&'a Snapshot>,
    /// The row of the changes pane that the list keys act on.
    pub selected: Option<&'a changes::Selected>,
    /// How far the changes pane was scrolled on the previous frame; the
    /// renderer returns the offset to pass back on the next one.
    pub scroll: usize,
    /// The commit draft while its box is open.
    pub draft: Option<&'a commit::Draft>,
    /// A notice shown in place of the key hints while it lasts.
    pub toast: Option<&'a toast::Toast>,
    /// Whether the key list is open over the screen.
    pub help: bool,
    /// The diff shown in the changes pane in place of the lists.
    pub diff: Option<&'a diff::Diff>,
    /// The card of the left pane.
    pub card: projects::Card<'a>,
    /// The list of sessions, from two sessions on: in the left pane while
    /// the side panes are shown, else as the count of those asking in
    /// the status bar.
    pub list: Option<projects::List<'a>>,
    /// The project picker while it is open over the screen.
    pub picker: Option<&'a picker::Picker>,
}

/// Where the agent's screen goes for a terminal of `area`: the whole area
/// when `full_screen`, else the center pane's interior, which spans the
/// width when the side panes are not shown.
pub fn agent_area(area: Rect, full_screen: bool, side_panes: bool) -> Rect {
    if full_screen {
        area
    } else {
        pane_inner(layout::panes_with(area, side_panes).center)
    }
}

/// The interior of a pane in `area`: a cell in from each side, where the
/// highlighted pane draws its frame and the others leave a margin, so
/// that moving the highlight moves nothing.
fn pane_inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

/// What a side pane draws in, inside its interior `inner`: a column of
/// padding on each side.
fn padded(inner: Rect) -> Rect {
    Rect {
        x: inner.x.saturating_add(1).min(inner.right()),
        width: inner.width.saturating_sub(2),
        ..inner
    }
}

/// Draws a pane's frame and `title` in `area` and returns its interior:
/// while `highlighted`, a rounded frame in the accent with the title on
/// its top edge; else the title alone, in the top margin, `indent`
/// columns into the interior.
fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    title: strings::Title,
    highlighted: bool,
    indent: u16,
) -> Rect {
    let inner = pane_inner(area);
    let title = title_line(title, highlighted);
    if highlighted {
        frame.render_widget(framed(title.spans), area);
    } else if area.height > 0 {
        let indent = indent.min(inner.width);
        let row = Rect::new(inner.x + indent, area.y, inner.width - indent, 1);
        frame.render_widget(Paragraph::new(title), row);
    }
    inner
}

/// A rounded frame in the accent with `title` on its top edge, after a
/// stretch of the frame: the highlighted pane's, the picker's and the key
/// list's.
fn framed(title: Vec<Span<'static>>) -> Block<'static> {
    let accent = Style::new().fg(ACCENT);
    let mut spans = vec![Span::styled(
        format!("{} ", symbols::line::HORIZONTAL),
        accent,
    )];
    spans.extend(title);
    spans.push(Span::styled(" ", accent));
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(accent)
        .title_style(Style::new().fg(TEXT))
        .title(Line::from(spans))
}

/// `title` as a pane draws it: the name in the accent and bold while the
/// pane is `highlighted`, else muted; the subject in bold; the details
/// muted; the separators faint.
fn title_line(title: strings::Title, highlighted: bool) -> Line<'static> {
    let spans: Vec<Span> = title
        .into_iter()
        .map(|(part, text)| {
            let style = match part {
                TitlePart::Name if highlighted => {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                }
                TitlePart::Name | TitlePart::Detail => Style::new().fg(MUTED),
                TitlePart::Subject => Style::new().add_modifier(Modifier::BOLD),
                TitlePart::Separator => Style::new().fg(FAINT),
            };
            Span::styled(text, style)
        })
        .collect();
    Line::from(spans)
}

/// Draws a faint rule down column `x` of `area`: the line between two
/// panes when neither has a frame.
fn draw_rule(frame: &mut Frame, area: Rect, x: u16) {
    let style = Style::new().fg(FAINT);
    for y in area.top()..area.bottom() {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_symbol(symbols::line::VERTICAL).set_style(style);
        }
    }
}

/// A count of lines added, in their colour; muted when it is zero.
fn lines_added(count: u64) -> Span<'static> {
    let colour = if count == 0 { MUTED } else { ADDED };
    Span::styled(strings::added(count), Style::new().fg(colour))
}

/// A count of lines removed, in their colour; muted when it is zero.
fn lines_removed(count: u64) -> Span<'static> {
    let colour = if count == 0 { MUTED } else { REMOVED };
    Span::styled(strings::removed(count), Style::new().fg(colour))
}

/// A section's heading in `width` columns: `name` muted, then a faint
/// rule to the end of the row.
fn heading(name: &str, width: usize) -> Line<'static> {
    let name = changes::cut(&changes::printable(name), width);
    let rule = width.saturating_sub(name.width() + 1);
    Line::from(vec![
        Span::styled(name, Style::new().fg(MUTED)),
        Span::raw(" "),
        Span::styled(
            symbols::line::HORIZONTAL.repeat(rule),
            Style::new().fg(FAINT),
        ),
    ])
}

/// Draws the agent over the whole frame, with the cursor where the agent
/// has it while keys go to it. The last row shows the termis-mode tag and
/// commands, or the `toast` in their place while it lasts, while that
/// mode is on, else the `ended` line once the agent ended.
pub fn render_full(
    frame: &mut Frame,
    emulator: &Emulator,
    ended: Option<&str>,
    mode: Mode,
    help: bool,
    toast: Option<&toast::Toast>,
) {
    let area = frame.area();
    render_agent(
        frame,
        area,
        emulator,
        mode == Mode::Agent && ended.is_none(),
    );
    match mode {
        Mode::Term(_) | Mode::Commit | Mode::Picker => {
            let bar = Bar {
                agent: "",
                side_panes: false,
                zoom: strings::Zoom::None,
                open: false,
                sessions: false,
                tabs: false,
                ended: ended.is_some(),
                selection: false,
                toast,
                help,
                diff: false,
                scrollable: !emulator.alternate_screen(),
            };
            let keys = mode_keys(mode, bar, usize::from(area.width));
            render_last_row(frame, area, keys, Style::new());
        }
        Mode::Agent => render_ended(frame, area, ended),
    }
    if help {
        self::help::render(frame, area);
    }
}

/// The branch the panes name: its name, `detached` on a detached
/// `HEAD`, none while `HEAD` cannot be read.
fn branch_label(branch: &Branch) -> Option<&str> {
    branch
        .head
        .as_deref()
        .or_else(|| branch.oid.is_some().then_some(strings::DETACHED_HEAD))
}

/// Draws the three panes and the status bar. Returns the scroll offset
/// of the changes pane for the next frame.
pub fn render(frame: &mut Frame, view: &View<'_>) -> usize {
    let panes = layout::panes_with(frame.area(), view.side_panes);
    let focus = if view.side_panes {
        view.mode.pane()
    } else {
        Pane::Agent
    };
    let mut scroll = view.scroll;

    if view.side_panes {
        let branch = view.changes.map(|snapshot| &snapshot.status.branch);
        // Room inside the frame: its corners, the stretch before the
        // title and the space after it.
        let room = usize::from(panes.right.width).saturating_sub(5);
        let title = strings::changes_pane_title(
            branch.and_then(branch_label),
            branch.and_then(|branch| branch.upstream.as_deref()),
            branch.and_then(|branch| branch.ahead_behind),
            room,
        );
        let right_inner = padded(draw_pane(
            frame,
            panes.right,
            title,
            focus == Pane::Changes,
            1,
        ));
        let projects_title = vec![(TitlePart::Name, strings::PANE_PROJECTS.to_owned())];
        let left_inner = padded(draw_pane(
            frame,
            panes.left,
            projects_title,
            focus == Pane::Projects,
            1,
        ));
        // One line between neighbours: the highlighted pane's frame, else
        // a faint rule in the side pane's margin.
        match focus {
            Pane::Changes => draw_rule(frame, panes.left, panes.left.right().saturating_sub(1)),
            Pane::Projects => draw_rule(frame, panes.right, panes.right.x),
            Pane::Agent => {}
        }
        projects::render(
            frame,
            left_inner,
            &view.card,
            view.list.as_ref(),
            focus == Pane::Projects,
        );
        match view.changes {
            _ if !view.repo => render_empty(frame, right_inner, strings::NOT_A_REPOSITORY),
            Some(_) if view.diff.is_some() => {
                if let Some(diff) = view.diff {
                    scroll = diff::render(frame, right_inner, diff, view.scroll);
                }
            }
            Some(snapshot) => {
                let cursor;
                (scroll, cursor) = changes::render(
                    frame,
                    right_inner,
                    snapshot,
                    changes::Lists {
                        selected: view.selected,
                        focused: focus == Pane::Changes,
                        scroll: view.scroll,
                    },
                    view.draft,
                    view.card.now,
                );
                if let Some(position) = cursor
                    && view.mode == Mode::Commit
                {
                    frame.set_cursor_position(position);
                }
            }
            None => {}
        }
    }

    // Room inside the frame, as for the changes pane.
    let room = usize::from(panes.center.width).saturating_sub(5);
    let title = strings::agent_pane_title(
        view.agent,
        view.project,
        view.terminals,
        view.terminal,
        view.emulator.scrolled(),
        room,
    );
    let inner = draw_pane(frame, panes.center, title, focus == Pane::Agent, 0);
    render_agent(
        frame,
        inner,
        view.emulator,
        view.mode == Mode::Agent && view.ended.is_none(),
    );
    render_ended(frame, inner, view.ended);

    // The mode tag and its keys on the left, the project flush right,
    // after the count of the other sessions asking, in a chip, while the
    // list is hidden. Unzoom is offered only when the width would show
    // the sides again.
    let zoom = if view.zoomed && layout::sides_visible(frame.area()) {
        strings::Zoom::Out
    } else if view.side_panes {
        strings::Zoom::In
    } else {
        strings::Zoom::None
    };
    let asking = view
        .list
        .filter(|_| !view.side_panes)
        .map_or(0, |list| list.asking());
    let mut spans = Vec::new();
    if asking > 0 {
        let chip = Style::new().fg(INK).bg(ASKING).add_modifier(Modifier::BOLD);
        let count = format!(" {} ", strings::sessions_asking(asking));
        spans.push(Span::styled(count, chip));
        spans.push(Span::raw("  "));
    }
    // The right end reads like a title that is never highlighted: the
    // name muted, the project in bold, its position muted.
    let end = strings::status_end(view.project, view.position);
    spans.extend(title_line(end, false).spans);
    spans.push(Span::raw(" "));
    let project = Line::from(spans).right_aligned();
    let width = u16::try_from(project.width()).unwrap_or(u16::MAX);
    let [left, right] = Layout::horizontal([Constraint::Min(0), Constraint::Length(width)])
        .spacing(1)
        .areas(panes.status);
    let tab = view
        .terminal
        .and_then(|index| view.terminals.get(index).copied());
    let bar = Bar {
        agent: tab.unwrap_or(view.agent),
        side_panes: view.side_panes,
        zoom,
        open: true,
        sessions: view.position.is_some(),
        tabs: !view.terminals.is_empty(),
        ended: view.ended.is_some(),
        selection: view.selected.is_some(),
        toast: view.toast,
        help: view.help,
        diff: view.diff.is_some(),
        scrollable: !view.emulator.alternate_screen(),
    };
    let keys = mode_keys(view.mode, bar, usize::from(left.width));
    frame.render_widget(Paragraph::new(keys), left);
    frame.render_widget(Paragraph::new(project), right);
    if view.help {
        help::render(frame, frame.area());
    }
    if let Some(picker) = view.picker
        && let Some(position) = picker::render(frame, frame.area(), picker)
    {
        frame.set_cursor_position(position);
    }
    scroll
}

/// What the status bar shows besides the mode.
struct Bar<'a> {
    /// The name of the agent, or of the terminal shown, for the tag
    /// while keys go to it.
    agent: &'a str,
    side_panes: bool,
    zoom: strings::Zoom,
    /// Whether `o` opens the project picker and `t` a terminal.
    open: bool,
    /// Whether there are other sessions for `n` and `N` to show, which
    /// `x` leaves on screen.
    sessions: bool,
    /// Whether the session shown has other tabs for `,` and `.` to show.
    tabs: bool,
    ended: bool,
    /// Whether the changes pane has a selected row.
    selection: bool,
    toast: Option<&'a toast::Toast>,
    /// Whether the key list is open.
    help: bool,
    /// Whether the diff view is open.
    diff: bool,
    /// Whether the agent pane has a history to scroll through: not while
    /// the agent is on the alternate screen.
    scrollable: bool,
}

/// The mode tag followed by the keys that apply, within `width` columns:
/// the name of the agent, or of the terminal shown, in a grey chip and
/// how to enter termis mode, `TERMIS` in a chip in the accent and the
/// commands (how to close the key list while it is open), `COMMIT` and
/// the box's keys, or `OPEN` and the picker's. A toast takes the place of
/// the keys while it lasts: a notice that something worked in the
/// accent, a failure in the alert colour, an agent that asks in the
/// asking colour and bold.
fn mode_keys(mode: Mode, bar: Bar<'_>, width: usize) -> Line<'static> {
    let Bar {
        agent,
        side_panes,
        zoom,
        open,
        sessions,
        tabs,
        ended,
        selection,
        toast,
        help,
        diff,
        scrollable,
    } = bar;
    let accent_chip = Style::new().fg(INK).bg(ACCENT).add_modifier(Modifier::BOLD);
    let (tag, tag_style, hints) = match mode {
        Mode::Agent => (
            agent,
            Style::new().fg(CHIP_TEXT).bg(CHIP),
            strings::agent_hints(ended, sessions),
        ),
        Mode::Term(_) if help => (strings::MODE_TERM, accent_chip, strings::help_hints()),
        Mode::Term(Pane::Changes) if diff => {
            (strings::MODE_TERM, accent_chip, strings::diff_hints())
        }
        Mode::Term(pane) => (
            strings::MODE_TERM,
            accent_chip,
            strings::term_hints(
                side_panes,
                zoom,
                ended,
                match pane {
                    Pane::Projects if side_panes && sessions => strings::PaneKeys::Projects,
                    Pane::Agent if scrollable => strings::PaneKeys::Agent,
                    Pane::Changes if side_panes && selection => strings::PaneKeys::Changes,
                    _ => strings::PaneKeys::None,
                },
                open,
                sessions,
                tabs,
            ),
        ),
        Mode::Commit => (strings::MODE_COMMIT, accent_chip, strings::commit_hints()),
        Mode::Picker => (strings::MODE_OPEN, accent_chip, strings::picker_hints()),
    };
    let tag = strings::mode_tag(tag);
    let room = width.saturating_sub(tag.width() + 2);
    let mut spans = vec![Span::styled(tag, tag_style), Span::raw("  ")];
    match toast {
        Some(toast) => {
            let style = match toast.kind {
                toast::Kind::Info => Style::new().fg(ACCENT),
                toast::Kind::Error => Style::new().fg(ALERT),
                toast::Kind::Attention => Style::new().fg(ASKING).add_modifier(Modifier::BOLD),
            };
            let text = changes::cut(&changes::printable(&toast.text), room);
            spans.push(Span::styled(text, style));
        }
        None => spans.extend(hint_spans(&fit_hints(hints, room))),
    }
    Line::from(spans)
}

/// `hints`, dropping the ones a narrow bar does without, in
/// [`strings::HINTS_TO_DROP`] order, until they fit `width` columns
/// joined or none of those is left.
fn fit_hints(mut hints: Vec<strings::Hint>, width: usize) -> Vec<strings::Hint> {
    let joined_width = |hints: &[strings::Hint]| {
        let texts: usize = hints
            .iter()
            .map(|&hint| strings::hint_text(hint).width())
            .sum();
        texts + strings::HINT_SEPARATOR.width() * hints.len().saturating_sub(1)
    };
    for drop in strings::HINTS_TO_DROP {
        if joined_width(&hints) <= width {
            break;
        }
        hints.retain(|hint| *hint != drop);
    }
    hints
}

/// The hints as the status bar draws them: the keys in bold, what they do
/// muted, faint separators between.
fn hint_spans(hints: &[strings::Hint]) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, &(keys, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                strings::HINT_SEPARATOR,
                Style::new().fg(FAINT),
            ));
        }
        spans.push(Span::styled(
            keys,
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {action}"), Style::new().fg(MUTED)));
    }
    spans
}

/// Draws the agent's screen in `area`. The host cursor follows the agent's
/// only while `cursor`; the screen keeps updating either way.
fn render_agent(frame: &mut Frame, area: Rect, emulator: &Emulator, cursor: bool) {
    let widget = emulator
        .widget()
        .cursor(Cursor::default().visibility(false));
    frame.render_widget(widget, area);
    if cursor && let Some(position) = cursor_position(area, emulator.cursor()) {
        frame.set_cursor_position(position);
    }
}

/// Draws `text` muted and centred in an otherwise empty pane.
pub(crate) fn render_empty(frame: &mut Frame, area: Rect, text: &str) {
    let line = Line::styled(text.to_owned(), Style::new().fg(MUTED));
    render_empty_line(frame, area, line);
}

/// Draws `line` centred in an otherwise empty pane.
fn render_empty_line(frame: &mut Frame, area: Rect, line: Line<'_>) {
    if area.height == 0 {
        return;
    }
    let middle = Rect::new(area.x, area.y + area.height / 2, area.width, 1);
    frame.render_widget(Paragraph::new(line.centered()), middle);
}

/// Draws `ended`, if any, as a grey chip over the last row of the agent's
/// area, after the muted glyph of a session that is over, cut to the row.
fn render_ended(frame: &mut Frame, area: Rect, ended: Option<&str>) {
    if let Some(text) = ended {
        let bar = Style::new().fg(CHIP_TEXT).bg(CHIP);
        let glyph = format!(" {}", strings::ENDED_GLYPH);
        let room = usize::from(area.width).saturating_sub(glyph.width());
        let line = Line::from(vec![
            Span::styled(glyph, Style::new().fg(MUTED)),
            Span::raw(changes::cut(text, room)),
        ]);
        render_last_row(frame, area, line, bar);
    }
}

/// Draws `line` over the last row of `area`, cleared first, with `style`
/// for the whole row.
fn render_last_row(frame: &mut Frame, area: Rect, line: Line<'_>, style: Style) {
    if area.height == 0 {
        return;
    }
    let last = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Clear, last);
    frame.render_widget(Paragraph::new(line).style(style), last);
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
    use ratatui::style::{Color, Modifier};

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

    fn bg(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or(Color::Reset, |c| c.bg)
    }

    fn reversed(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> bool {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::REVERSED))
    }

    fn bold(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> bool {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .is_some_and(|c| c.modifier.contains(Modifier::BOLD))
    }

    fn symbol(terminal: &Terminal<TestBackend>, x: u16, y: u16) -> &str {
        terminal
            .backend()
            .buffer()
            .cell((x, y))
            .map_or("", |c| c.symbol())
    }

    /// The corners of `pane`: top left, top right, bottom left, bottom
    /// right.
    fn corners(terminal: &Terminal<TestBackend>, pane: Rect) -> String {
        let (left, right) = (pane.left(), pane.right() - 1);
        let (top, bottom) = (pane.top(), pane.bottom() - 1);
        [(left, top), (right, top), (left, bottom), (right, bottom)]
            .into_iter()
            .map(|(x, y)| symbol(terminal, x, y))
            .collect()
    }

    /// Column `x` of the panes' rows.
    fn column(terminal: &Terminal<TestBackend>, x: u16, rows: u16) -> String {
        (0..rows).map(|y| symbol(terminal, x, y)).collect()
    }

    fn view(emulator: &Emulator, mode: Mode) -> View<'_> {
        View {
            agent: "claude",
            project: "term",
            terminals: &[],
            terminal: None,
            position: None,
            emulator,
            mode,
            side_panes: true,
            zoomed: false,
            ended: None,
            repo: true,
            changes: None,
            selected: None,
            scroll: 0,
            draft: None,
            toast: None,
            help: false,
            diff: None,
            card: projects::Card {
                path: "~/dev/term",
                stack: None,
                now: 0,
                agent: None,
            },
            list: None,
            picker: None,
        }
    }

    fn draw(width: u16, height: u16, view: &View<'_>) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| {
                render(frame, view);
            })
            .expect("draw");
        terminal
    }

    #[test]
    fn draws_titled_panes_and_the_status_bar() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let terminal = draw(100, 10, &view(&emulator, Mode::Agent));
        let panes = layout::panes(Rect::new(0, 0, 100, 10));

        // The highlighted pane has a rounded frame in the accent with its
        // title on it; the others show their title alone, the side panes'
        // a column into the interior, over their content.
        let top = row(&terminal, 0);
        assert!(top.starts_with("  Projects "), "{top}");
        assert!(top.contains(" ╭─ claude · term ─"), "{top}");
        assert!(top.contains("─╮  Changes "), "{top}");
        assert_eq!(top.chars().count(), 100);
        assert_eq!(corners(&terminal, panes.center), "╭╮╰╯");
        assert_eq!(fg(&terminal, panes.center.x, 0), ACCENT, "the frame");
        assert_eq!(corners(&terminal, panes.left), "    ", "no frame");
        assert_eq!(corners(&terminal, panes.right), "    ", "no frame");
        // The title's parts: the highlighted pane's name in the accent and
        // bold, a faint separator, the subject in bold; another pane's
        // name muted.
        let name = panes.center.x + 3;
        assert_eq!(fg(&terminal, name, 0), ACCENT);
        assert!(bold(&terminal, name, 0));
        assert_eq!(fg(&terminal, name + 6, 0), FAINT, "the separator");
        assert_eq!(fg(&terminal, name + 9, 0), Color::Reset, "the project");
        assert!(bold(&terminal, name + 9, 0));
        assert_eq!(fg(&terminal, 2, 0), MUTED, "Projects");
        assert!(!bold(&terminal, 2, 0));
        assert_eq!(fg(&terminal, panes.right.x + 2, 0), MUTED, "Changes");

        let second = row(&terminal, 1);
        assert!(second.starts_with(' '), "{second}");
        assert!(second.contains("│hello"), "{second}");
        // The agent's frame is the line between it and its neighbours.
        let rows = panes.center.height;
        assert_eq!(column(&terminal, panes.left.right() - 1, rows).trim(), "");
        assert_eq!(column(&terminal, panes.right.x, rows).trim(), "");

        let bottom = row(&terminal, 8);
        assert!(bottom.starts_with(' '), "{bottom}");
        assert_eq!(symbol(&terminal, panes.center.x, 8), "╰");
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   Ctrl+Q termis mode"),
            "{status}"
        );
        assert!(status.ends_with("termis · term "), "{status}");
        // The agent's name is a grey chip; a hint's keys are in bold and
        // what they do muted.
        assert_eq!(bg(&terminal, 1, 9), CHIP);
        assert_eq!(fg(&terminal, 1, 9), CHIP_TEXT);
        assert!(!reversed(&terminal, 1, 9));
        assert_eq!(fg(&terminal, 10, 9), Color::Reset, "Ctrl+Q");
        assert!(bold(&terminal, 10, 9));
        assert_eq!(bg(&terminal, 10, 9), Color::Reset);
        assert_eq!(fg(&terminal, 17, 9), MUTED, "termis mode");
        assert!(!bold(&terminal, 17, 9));
        // The right end: `termis` muted, a faint separator, the project
        // in bold.
        let end = 100 - u16::try_from("termis · term ".chars().count()).expect("width");
        assert_eq!(fg(&terminal, end, 9), MUTED);
        assert!(!bold(&terminal, end, 9));
        assert_eq!(fg(&terminal, end + 7, 9), FAINT);
        assert_eq!(fg(&terminal, end + 9, 9), Color::Reset);
        assert!(bold(&terminal, end + 9, 9));
    }

    #[test]
    fn term_mode_shows_its_tag_and_commands_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(
                " TERMIS   ? keys · j/k scroll · o open · 1/2/3 pane · z zoom · q quit · Esc back"
            ),
            "{status}"
        );
        assert!(status.ends_with("termis · term "), "{status}");
        // The tag is a chip in the accent, its text dark and bold; the
        // hints' keys in bold, what they do muted, faint separators.
        for x in 0..8 {
            assert_eq!(bg(&terminal, x, 9), ACCENT, "the tag, column {x}");
            assert_eq!(fg(&terminal, x, 9), INK);
            assert!(bold(&terminal, x, 9));
            assert!(!reversed(&terminal, x, 9));
        }
        assert_eq!(bg(&terminal, 8, 9), Color::Reset);
        assert!(bold(&terminal, 10, 9), "?");
        assert_eq!(fg(&terminal, 12, 9), MUTED, "keys");
        assert_eq!(fg(&terminal, 17, 9), FAINT, "the separator");
        assert!(bold(&terminal, 19, 9), "j/k");
        // The agent's pane stays highlighted, but keys do not reach it.
        assert!(row(&terminal, 0).contains("╭─ claude · term ─"));
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn zoomed_view_hides_the_sides_and_offers_unzoom() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.side_panes = false;
        v.zoomed = true;
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("╭─ claude · term ─"), "{top}");
        assert!(top.ends_with('╮') && !top.contains("Projects"), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   Ctrl+Q termis mode"),
            "{status}"
        );
        v.mode = Mode::Term(Pane::Agent);
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.contains("z unzoom · q quit"), "{status}");
        assert!(!status.contains("1/2/3"), "{status}");
    }

    #[test]
    fn narrow_zoomed_view_offers_no_zoom_key() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.side_panes = false;
        v.zoomed = true;
        let status = row(&draw(80, 10, &v), 9);
        assert!(!status.contains("zoom"), "{status}");
        assert!(status.contains("q quit"), "{status}");
    }

    #[test]
    fn highlighted_pane_follows_the_mode() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let rows = panes.center.height;
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Changes)));
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ Changes ─"), "{top}");
        assert!(top.contains(" │ claude · term "), "{top}");
        assert_eq!(corners(&terminal, panes.right), "╭╮╰╯");
        assert_eq!(fg(&terminal, panes.right.x, 1), ACCENT, "the frame");
        assert_eq!(corners(&terminal, panes.center), "    ", "no frame");
        assert_eq!(corners(&terminal, panes.left), " │ │", "no frame");
        // A faint rule in the projects pane's margin sets apart the two
        // panes without a frame.
        let rule = panes.left.right() - 1;
        assert_eq!(column(&terminal, rule, rows), "│".repeat(9));
        assert!((0..rows).all(|y| fg(&terminal, rule, y) == FAINT));
        assert_eq!(fg(&terminal, panes.center.x + 1, 0), MUTED, "claude");
        assert!(row(&terminal, 9).starts_with(" TERMIS "));

        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Projects)));
        assert!(row(&terminal, 0).starts_with("╭─ Projects ─"));
        assert_eq!(corners(&terminal, panes.left), "╭╮╰╯");
        assert_eq!(fg(&terminal, panes.left.x, 1), ACCENT);
        assert_eq!(corners(&terminal, panes.center), "    ");
        // Now the rule is in the changes pane's margin.
        assert_eq!(column(&terminal, panes.right.x, rows), "│".repeat(9));
        assert_eq!(fg(&terminal, panes.right.x, 1), FAINT);
        assert_eq!(column(&terminal, panes.center.x, rows).trim(), "");
    }

    #[test]
    fn moving_the_highlight_moves_nothing() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"hello");
        let snapshot = changes::tests::sample();
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        // What each pane shows inside its margins, whichever pane is
        // highlighted.
        let interiors = |mode| {
            let mut v = view(&emulator, mode);
            v.changes = Some(&snapshot);
            let terminal = draw(100, 10, &v);
            [panes.left, panes.center, panes.right].map(|pane| {
                let inner = pane_inner(pane);
                (inner.top()..inner.bottom())
                    .map(|y| {
                        (inner.left()..inner.right())
                            .map(|x| symbol(&terminal, x, y))
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
            })
        };
        let agent = interiors(Mode::Agent);
        assert!(agent[0][0].starts_with(" ~/dev/term"), "{:?}", agent[0]);
        assert!(agent[1][0].starts_with("hello"), "{:?}", agent[1]);
        assert!(agent[2][0].starts_with(" 4 files"), "{:?}", agent[2]);
        assert_eq!(interiors(Mode::Term(Pane::Projects)), agent);
        assert_eq!(interiors(Mode::Term(Pane::Changes)), agent);
    }

    #[test]
    fn status_bar_keeps_a_gap_before_the_project() {
        const PROJECT: &str = "a-project-name-long-enough-to-push-the-hints-off-the-bar";
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Projects));
        v.project = PROJECT;
        let status = row(&draw(100, 10, &v), 9);
        // The hints shrink to the left area, which ends one blank column
        // before the right-aligned project; what is left is never cut.
        let project = format!("termis · {PROJECT} ");
        let left_width = 100 - project.chars().count() - 1;
        let keys: String = status.chars().take(left_width).collect();
        assert_eq!(keys.trim_end(), " TERMIS   ? keys · q quit", "{status}");
        assert!(status.ends_with(&format!(" {project}")), "{status}");
    }

    #[test]
    fn cursor_follows_the_agent_only_in_agent_mode() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut terminal = draw(100, 10, &view(&emulator, Mode::Agent));
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 23, y: 1 }
        );
        for mode in [Mode::Term(Pane::Agent), Mode::Term(Pane::Changes)] {
            let terminal = draw(100, 10, &view(&emulator, mode));
            assert!(
                !terminal.backend().cursor_visible(),
                "cursor hidden in termis mode ({mode:?})"
            );
        }
    }

    #[test]
    fn changes_pane_says_when_there_is_no_repository() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Agent);
        v.repo = false;
        let terminal = draw(100, 10, &v);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let inner = padded(pane_inner(panes.right));
        let y = inner.y + inner.height / 2;
        let middle = row(&terminal, y);
        assert!(middle.contains("not a git repository"), "{middle}");
        let x = middle.find("not a git").expect("text") as u16;
        assert!(x > inner.x && x < inner.right(), "inside the right pane");
        assert_eq!(fg(&terminal, x, y), MUTED);

        v.repo = true;
        let terminal = draw(100, 10, &v);
        for y in 0..10 {
            assert!(!row(&terminal, y).contains("not a git repository"));
        }
    }

    #[test]
    fn changes_pane_shows_the_branch_and_the_lists() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = changes::tests::sample();
        let first = changes::Selected {
            section: changes::Section::Staged,
            path: "src/app.rs".into(),
        };
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.changes = Some(&snapshot);
        v.selected = Some(&first);
        let terminal = draw(100, 10, &v);
        assert!(
            row(&terminal, 0).contains("╭─ Changes · main ─"),
            "{}",
            row(&terminal, 0)
        );
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let inner = padded(pane_inner(panes.right));
        let right = |y: u16| -> String {
            row(&terminal, y)
                .chars()
                .skip(usize::from(inner.x))
                .take(usize::from(inner.width))
                .collect()
        };
        assert!(right(1).starts_with("4 files"), "{}", right(1));
        assert_eq!(right(2).trim(), "");
        assert_eq!(right(3), format!("staged{}2", " ".repeat(14)));
        assert_eq!(right(4), "▸ M src/app.rs +31 −4");
        assert!(right(6).starts_with("today ─"), "{}", right(6));
        assert_eq!(right(7), "3 commits    +312 −58");
        assert!(
            row(&terminal, 9).contains("j/k move"),
            "{}",
            row(&terminal, 9)
        );

        v.mode = Mode::Agent;
        let terminal = draw(100, 10, &v);
        assert!(row(&terminal, 0).ends_with("╮  Changes · main         "));
        assert!(!row(&terminal, 9).contains("j/k move"));
        // Nothing selected, nothing to move.
        v.mode = Mode::Term(Pane::Changes);
        v.selected = None;
        assert!(!row(&draw(100, 10, &v), 9).contains("j/k move"));

        // Detached: the title says so; no snapshot yet: just the name.
        let mut detached = changes::tests::sample();
        detached.status.branch.head = None;
        detached.status.branch.oid = Some("abc".into());
        v.changes = Some(&detached);
        assert!(row(&draw(100, 10, &v), 0).contains("Changes · detached"));
        v.changes = None;
        assert!(row(&draw(100, 10, &v), 0).contains("╭─ Changes ─"));
    }

    #[test]
    fn hints_shrink_to_the_bar() {
        let all = strings::term_hints(
            true,
            strings::Zoom::In,
            false,
            strings::PaneKeys::Changes,
            true,
            false,
            false,
        );
        let joined = |hints: &[strings::Hint]| {
            let texts: Vec<String> = hints.iter().map(|&hint| strings::hint_text(hint)).collect();
            texts.join(strings::HINT_SEPARATOR)
        };
        let full = joined(&all);
        assert_eq!(fit_hints(all.clone(), full.width()), all);
        let narrower = fit_hints(all.clone(), full.width() - 1);
        assert_eq!(
            joined(&narrower),
            "? keys · j/k move · s stage · u unstage · a all · c commit · o open · 1/2/3 pane · z zoom · q quit · Esc back",
            "t terminal goes first here"
        );
        let narrow = fit_hints(all.clone(), 30);
        assert_eq!(joined(&narrow), "? keys · s stage · u unstage · q quit");
        assert_eq!(narrow, fit_hints(all, 0), "the rest always stays");
        // As drawn, the same text: each hint's keys and action, separated.
        let spans = hint_spans(&narrow);
        let drawn: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(drawn, joined(&narrow));
        assert_eq!(spans[0].content, "?");
        assert_eq!(spans[1].content, " keys");
        assert_eq!(spans[2].content, strings::HINT_SEPARATOR);
        assert_eq!(
            hint_spans(&strings::diff_hints())[6].content,
            "d / Esc",
            "keys of several words stay together"
        );

        // At 100 columns with a long project name the changes keys still
        // end with q quit.
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = changes::tests::sample();
        let first = changes::Selected {
            section: changes::Section::Staged,
            path: "src/app.rs".into(),
        };
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.project = "comunidad";
        v.changes = Some(&snapshot);
        v.selected = Some(&first);
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.contains("q quit"), "{status}");
        assert!(status.ends_with("termis · comunidad "), "{status}");
    }

    #[test]
    fn a_toast_takes_the_place_of_the_hints() {
        use std::time::{Duration, Instant};
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let now = Instant::now();
        let done = toast::Toast::new(
            "committed abc1234".into(),
            toast::Kind::Info,
            now,
            Duration::from_secs(5),
        );
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.toast = Some(&done);
        let terminal = draw(100, 10, &v);
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" TERMIS   committed abc1234"),
            "{status}"
        );
        assert!(!status.contains("q quit"), "{status}");
        assert_eq!(fg(&terminal, 10, 9), ACCENT);
        assert!(!bold(&terminal, 10, 9));

        let failed = toast::Toast::new(
            "git add: fatal: pathspec 'x' did not match any files".into(),
            toast::Kind::Error,
            now,
            Duration::from_secs(5),
        );
        v.mode = Mode::Agent;
        v.toast = Some(&failed);
        let terminal = draw(100, 10, &v);
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   git add: fatal: pathspec"),
            "{status}"
        );
        assert_eq!(fg(&terminal, 10, 9), ALERT);
        assert!(!bold(&terminal, 10, 9));
        // An agent not shown that asks is in the asking colour, in bold.
        let asking = toast::Toast::new(
            "! api is asking".into(),
            toast::Kind::Attention,
            now,
            Duration::from_secs(5),
        );
        v.toast = Some(&asking);
        let terminal = draw(100, 10, &v);
        let status = row(&terminal, 9);
        assert!(status.starts_with(" claude   ! api is asking"), "{status}");
        assert_eq!(fg(&terminal, 10, 9), ASKING);
        assert!(bold(&terminal, 10, 9));
        v.toast = Some(&failed);
        // A long notice is cut before the project, never over it.
        v.project = "a-project-name-long-enough-to-push-the-notice-off-the-bar";
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.contains('…'), "{status}");
        assert!(status.ends_with("off-the-bar "), "{status}");
    }

    #[test]
    fn the_key_list_opens_over_the_panes() {
        let mut emulator = Emulator::new(Size { rows: 38, cols: 53 }, 0);
        emulator.process(&b"hello world ".repeat(200));
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.help = true;
        let terminal = draw(100, 40, &v);
        let all: Vec<String> = (0..40).map(|y| row(&terminal, y)).collect();
        let top = all
            .iter()
            .position(|line| line.contains("╭─ keys ─"))
            .expect("box");
        assert!(
            all.iter().any(|line| line.contains("commit box")),
            "sections"
        );
        let projects = all
            .iter()
            .position(|line| line.contains("projects pane"))
            .expect("the projects pane's keys");
        let keys = &all[projects + 1];
        assert!(keys.contains("Enter"), "{keys}");
        // `x` closes the session under the list's cursor.
        assert!(keys.contains("x closes the cursor's session"), "{keys}");
        // The agent's text is gone under the box.
        let inside = &all[top + 2];
        let box_start = inside.find('│').expect("border");
        let box_end = inside.rfind('│').expect("border");
        assert!(
            !inside[box_start..box_end].contains("hello world"),
            "{inside}"
        );
        assert!(all[39].contains("? or Esc close"), "{}", all[39]);
        assert!(!terminal.backend().cursor_visible());
        v.help = false;
        let terminal = draw(100, 40, &v);
        assert!(!(0..40).any(|y| row(&terminal, y).contains("╭─ keys ─")));
        assert!(row(&terminal, 39).contains("? keys"));
    }

    #[test]
    fn the_picker_opens_over_the_panes_with_its_keys_in_the_bar() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let home = std::path::Path::new("/Users/x");
        let open = picker::Item::open("term".into(), home.join("dev/term"), Some(home));
        let picker = picker::Picker::new(vec![open], vec![]);
        let mut v = view(&emulator, Mode::Picker);
        v.picker = Some(&picker);
        let mut terminal = draw(100, 10, &v);
        let all: Vec<String> = (0..10).map(|y| row(&terminal, y)).collect();
        let top = (0..10u16)
            .find(|&y| all[usize::from(y)].contains("╭─ open a project ─"))
            .expect("the box");
        let query = &all[usize::from(top) + 1];
        assert!(query.contains("│ › a project's name"), "{query}");
        let first = &all[usize::from(top) + 3];
        assert!(first.contains("│ ▸ term  ~/dev/term "), "{first}");
        assert!(first.contains(" open │"), "{first}");
        let status = &all[9];
        assert!(
            status
                .starts_with(" OPEN   Enter open · Tab new conversation · Up/Down move · Esc back"),
            "{status}"
        );
        assert!(status.ends_with("termis · term "), "{status}");
        assert_eq!(bg(&terminal, 1, 9), ACCENT);
        assert_eq!(fg(&terminal, 1, 9), INK);
        // The agent's pane stays highlighted under it; the cursor is in
        // the query, after the prompt.
        assert!(all[0].contains("╭─ claude · term ─"), "{}", all[0]);
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 18, y: top + 1 }
        );
        // Termis mode offers it, over the three panes only.
        let projects = view(&emulator, Mode::Term(Pane::Projects));
        let status = row(&draw(100, 10, &projects), 9);
        assert!(
            status.contains("? keys · o open · t terminal · 1/2/3 pane · z zoom"),
            "{status}"
        );
        let mut full = Terminal::new(TestBackend::new(100, 4)).expect("terminal");
        full.draw(|frame| {
            render_full(frame, &emulator, None, Mode::Term(Pane::Agent), false, None)
        })
        .expect("draw");
        let last = row(&full, 3);
        assert!(last.starts_with(" TERMIS   ? keys · j/k scroll"), "{last}");
        assert!(!last.contains("o open"), "{last}");
    }

    #[test]
    fn the_bar_says_which_of_several_sessions_is_shown() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Projects));
        v.position = Some((2, 3));
        let status = row(&draw(140, 10, &v), 9);
        assert!(status.ends_with("termis · term 2/3 "), "{status}");
        // The projects pane's keys come first; x closes the session shown.
        assert!(
            status.contains(
                "? keys · j/k move · Enter show · o open · n/N session · 1/2/3 pane · z zoom · x close · q quit"
            ),
            "{status}"
        );
        // Ten columns fewer, o open goes first.
        let status = row(&draw(130, 10, &v), 9);
        assert!(
            status.contains("Enter show · n/N session · 1/2/3 pane · z zoom · x close · q quit"),
            "{status}"
        );
        // In agent mode too; with one session, neither.
        v.mode = Mode::Agent;
        assert!(row(&draw(130, 10, &v), 9).ends_with("termis · term 2/3 "));
        v.mode = Mode::Term(Pane::Projects);
        v.position = None;
        let status = row(&draw(130, 10, &v), 9);
        assert!(status.ends_with("termis · term "), "{status}");
        assert!(!status.contains("n/N"), "{status}");
        assert!(
            status.contains("? keys · o open · t terminal · 1/2/3 pane"),
            "{status}"
        );
    }

    #[test]
    fn the_list_shows_beside_the_panes_else_the_bar_counts_who_asks() {
        use crate::agent::{State, Status};
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let asking = Status {
            state: Some(State::Attention),
            ..Status::default()
        };
        let session = |project, status| projects::Row {
            project,
            status,
            ended: false,
            changed: false,
        };
        let rows = [
            session("term", Some(&asking)),
            session("api", Some(&asking)),
            session("web", Some(&asking)),
            session("notes", None),
        ];
        let mut v = view(&emulator, Mode::Agent);
        v.position = Some((1, 4));
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 0,
        });
        // With the side panes the list is in the projects pane, and the
        // bar counts nothing.
        let terminal = draw(100, 10, &v);
        assert!(
            row(&terminal, 1).starts_with("  ▸ ! term "),
            "{}",
            row(&terminal, 1)
        );
        assert!(
            row(&terminal, 2).starts_with("    ! api "),
            "{}",
            row(&terminal, 2)
        );
        let status = row(&terminal, 9);
        assert!(status.ends_with(" termis · term 1/4 "), "{status}");
        assert!(!status.contains("asking"), "{status}");
        // Without them the bar counts the others that ask, in a chip in
        // the asking colour.
        v.side_panes = false;
        v.zoomed = true;
        let terminal = draw(100, 10, &v);
        let status = row(&terminal, 9);
        assert!(
            status.ends_with(" ! 2 asking   termis · term 1/4 "),
            "{status}"
        );
        let x = status.chars().position(|c| c == '!').expect("the count");
        let x = u16::try_from(x).expect("on screen");
        for x in x - 1..x + 11 {
            assert_eq!(bg(&terminal, x, 9), ASKING, "the chip, column {x}");
            assert_eq!(fg(&terminal, x, 9), INK);
            assert!(bold(&terminal, x, 9));
        }
        assert_eq!(bg(&terminal, x + 11, 9), Color::Reset, "past the chip");
        assert_eq!(fg(&terminal, x + 13, 9), MUTED, "termis");
        // When only the one shown asks, nothing.
        let alone = [rows[0], session("api", None), rows[3]];
        v.position = Some((1, 3));
        v.list = Some(projects::List {
            rows: &alone,
            shown: 0,
            cursor: 0,
        });
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.ends_with("  termis · term 1/3 "), "{status}");
        assert!(!status.contains("asking"), "{status}");
    }

    #[test]
    fn the_diff_takes_the_changes_pane() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = changes::tests::sample();
        let request = crate::git::diff::Request {
            paths: vec!["src/app.rs".into()],
            staged: false,
            untracked: false,
        };
        let mut d = diff::Diff::new(request.clone(), "src/app.rs".into());
        assert!(d.accept(&request, "@@ -1 +1 @@\n-a\n+b\n"));
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.changes = Some(&snapshot);
        v.diff = Some(&d);
        let terminal = draw(100, 10, &v);
        let panes = layout::panes(Rect::new(0, 0, 100, 10));
        let inner = padded(pane_inner(panes.right));
        let right = |y: u16| -> String {
            row(&terminal, y)
                .chars()
                .skip(usize::from(inner.x))
                .take(usize::from(inner.width))
                .collect()
        };
        assert!(right(1).starts_with("diff src/app.rs"), "{}", right(1));
        assert!(right(2).starts_with("@@ -1 +1 @@"), "{}", right(2));
        assert!(
            !(1..9).any(|y| right(y).starts_with("staged ")),
            "the lists are gone"
        );
        let status = row(&terminal, 9);
        assert!(status.contains("j/k scroll · d / Esc back"), "{status}");
    }

    #[test]
    fn agent_area_is_the_center_interior() {
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(agent_area(area, true, true), area);
        assert_eq!(agent_area(area, true, false), area);
        assert_eq!(agent_area(area, false, true), Rect::new(21, 1, 53, 37));
        // Without the sides, zoomed or collapsed: the whole width inside one
        // border.
        assert_eq!(agent_area(area, false, false), Rect::new(1, 1, 98, 37));
        let narrow = Rect::new(0, 0, 80, 40);
        assert_eq!(agent_area(narrow, false, false), Rect::new(1, 1, 78, 37));
    }

    #[test]
    fn the_title_says_how_far_back_the_agent_view_is() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, true).into(), 10);
        emulator.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n10");
        emulator.scroll_back(2);
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ claude · term ↑ 2 ─"), "{top}");
        assert!(row(&terminal, 1).contains("│2 "), "{}", row(&terminal, 1));
        let status = row(&terminal, 9);
        assert!(status.contains("j/k scroll"), "{status}");
        emulator.scroll_to_live();
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ claude · term ─"), "{top}");
        assert!(row(&terminal, 1).contains("│4 "), "{}", row(&terminal, 1));
    }

    #[test]
    fn the_title_names_the_tabs_and_the_chip_the_terminal_shown() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let terminals = ["zsh", "zsh 2"];
        let mut v = view(&emulator, Mode::Agent);
        v.terminals = &terminals;
        let center = layout::panes(Rect::new(0, 0, 100, 10)).center;
        // On the agent's tab the project is the subject and the terminals
        // details; the chip names the agent.
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ claude · term   zsh   zsh 2 ─"), "{top}");
        let name = center.x + 3;
        assert!(bold(&terminal, name + 9, 0), "term");
        assert_eq!(fg(&terminal, name + 16, 0), MUTED, "zsh");
        assert!(!bold(&terminal, name + 16, 0));
        assert!(row(&terminal, 9).starts_with(" claude   "));
        // On a terminal's tab that terminal is, and the chip names it.
        v.terminal = Some(1);
        let terminal = draw(100, 10, &v);
        assert_eq!(fg(&terminal, name + 9, 0), MUTED, "term");
        assert!(!bold(&terminal, name + 9, 0));
        assert_eq!(fg(&terminal, name + 22, 0), Color::Reset, "zsh 2");
        assert!(bold(&terminal, name + 22, 0));
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" zsh 2   Ctrl+Q termis mode"),
            "{status}"
        );
        assert_eq!(bg(&terminal, 1, 9), CHIP);
        // Termis mode offers the tabs beside.
        v.mode = Mode::Term(Pane::Projects);
        let status = row(&draw(140, 10, &v), 9);
        assert!(
            status.contains("t terminal · ,/. tab · 1/2/3 pane"),
            "{status}"
        );
    }

    #[test]
    fn the_bar_offers_no_scrolling_on_the_alternate_screen() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 100, 10), false, true).into(), 10);
        emulator.process(b"\x1b[?1049h");
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        let status = row(&terminal, 9);
        assert!(!status.contains("j/k scroll"), "{status}");
        assert!(
            status.contains("? keys · o open · t terminal · 1/2/3 pane"),
            "{status}"
        );
        emulator.process(b"\x1b[?1049l");
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        assert!(row(&terminal, 9).contains("j/k scroll"));
    }

    #[test]
    fn branch_label_names_head() {
        let branch = Branch {
            head: Some("main".into()),
            ..Branch::default()
        };
        assert_eq!(branch_label(&branch), Some("main"));
        let branch = Branch {
            oid: Some("abc".into()),
            ..Branch::default()
        };
        assert_eq!(branch_label(&branch), Some("detached"));
        assert_eq!(branch_label(&Branch::default()), None);
    }

    #[test]
    fn git_is_in_the_changes_pane_not_the_card() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut snapshot = changes::tests::sample();
        snapshot.status.branch.upstream = Some("origin/main".into());
        snapshot.status.branch.ahead_behind = Some((2, 1));
        let mut v = view(&emulator, Mode::Agent);
        v.changes = Some(&snapshot);
        let terminal = draw(100, 10, &v);
        assert!(
            row(&terminal, 1).starts_with("  ~/dev/term"),
            "{}",
            row(&terminal, 1)
        );
        for y in 2..8 {
            let inner: String = row(&terminal, y).chars().skip(2).take(16).collect();
            assert_eq!(inner.trim(), "", "no git in the card, row {y}");
        }
        // The narrow pane keeps the upstream's state over the remote, and
        // over its own name.
        let top = row(&terminal, 0);
        assert!(top.ends_with("╮  Changes · main ↑2 ↓1   "), "{top}");
        let top = row(&draw(160, 10, &v), 0);
        assert!(top.contains("  Changes · main · origin ↑2 ↓1 "), "{top}");
        let mut feature = changes::tests::sample();
        feature.status.branch = Branch {
            head: Some("feature".into()),
            ..snapshot.status.branch.clone()
        };
        v.changes = Some(&feature);
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.ends_with("╮  feature ↑2 ↓1          "), "{top}");
        // Without the pane's name the branch is still the subject, in bold,
        // and the state a detail.
        let title = layout::panes(Rect::new(0, 0, 100, 10)).right.x + 2;
        assert_eq!(fg(&terminal, title, 0), Color::Reset);
        assert!(bold(&terminal, title, 0));
        assert_eq!(fg(&terminal, title + 8, 0), MUTED);
        assert!(!bold(&terminal, title + 8, 0));
        v.changes = None;
        let terminal = draw(100, 10, &v);
        assert!(row(&terminal, 1).starts_with("  ~/dev/term"));
    }

    #[test]
    fn collapsed_view_shows_only_the_agent_pane() {
        let mut emulator =
            Emulator::new(agent_area(Rect::new(0, 0, 80, 10), false, false).into(), 0);
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.side_panes = false;
        let terminal = draw(80, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("╭─ claude · term ─"), "{top}");
        assert!(
            !top.contains("Projects") && !top.contains("Changes"),
            "{top}"
        );
        assert!(top.ends_with('╮'), "{top}");
        assert!(row(&terminal, 1).contains("│hello"));
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   Ctrl+Q termis mode"),
            "{status}"
        );
        assert!(status.ends_with("termis · term "));
    }

    #[test]
    fn collapsed_bar_never_offers_the_pane_digits() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
            let mut v = view(&emulator, mode);
            v.side_panes = false;
            let status = row(&draw(80, 10, &v), 9);
            assert!(!status.contains("1/2/3"), "{status}");
        }
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.side_panes = false;
        let status = row(&draw(80, 10, &v), 9);
        assert!(
            status.starts_with(" TERMIS   ? keys · j/k scroll · o open · q quit · Esc back"),
            "{status}"
        );
    }

    #[test]
    fn full_screen_puts_the_cursor_where_the_agent_has_it() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 20 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, None, Mode::Agent, false, None))
            .expect("draw");
        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.get_cursor_position().expect("cursor"),
            Position { x: 2, y: 0 }
        );
    }

    #[test]
    fn full_screen_shows_term_mode_on_the_last_row_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 4, cols: 40 }, 0);
        emulator.process(b"ab");
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, None, Mode::Term(Pane::Agent), false, None))
            .expect("draw");
        assert!(!terminal.backend().cursor_visible());
        let last = row(&terminal, 3);
        assert!(
            last.starts_with(" TERMIS   ? keys · j/k scroll · q quit"),
            "{last}"
        );
        assert_eq!(bg(&terminal, 1, 3), ACCENT);
        assert_eq!(fg(&terminal, 1, 3), INK);
        assert!(bold(&terminal, 10, 3), "?");
        // Once the agent ended, termis mode keeps its row and offers the
        // relaunch; agent mode shows the ended line instead.
        terminal
            .draw(|frame| {
                render_full(
                    frame,
                    &emulator,
                    Some(" ended "),
                    Mode::Term(Pane::Agent),
                    false,
                    None,
                )
            })
            .expect("draw");
        let last = row(&terminal, 3);
        assert!(
            last.starts_with(" TERMIS   ? keys · r relaunch · q quit"),
            "{last}"
        );
        // A notice takes the place of the keys, as in the status bar.
        let asking = toast::Toast::new(
            "term is working · press q again to quit".into(),
            toast::Kind::Error,
            std::time::Instant::now(),
            std::time::Duration::from_secs(3),
        );
        terminal
            .draw(|frame| {
                render_full(
                    frame,
                    &emulator,
                    None,
                    Mode::Term(Pane::Agent),
                    false,
                    Some(&asking),
                )
            })
            .expect("draw");
        let last = row(&terminal, 3);
        assert!(
            last.starts_with(" TERMIS   term is working · press q"),
            "{last}"
        );
        assert_eq!(fg(&terminal, 10, 3), ALERT);
        // The key list opens here too.
        terminal
            .draw(|frame| render_full(frame, &emulator, None, Mode::Term(Pane::Agent), true, None))
            .expect("draw");
        assert!(
            row(&terminal, 0).contains("╭─ keys ─"),
            "{}",
            row(&terminal, 0)
        );
        // Four rows cannot hold the list: the bottom of the frame says so.
        assert!(row(&terminal, 3).contains('…'), "{}", row(&terminal, 3));
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended "), Mode::Agent, false, None))
            .expect("draw");
        assert!(row(&terminal, 3).starts_with(" × ended "));
        assert!(!terminal.backend().cursor_visible());
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
            let mut emulator = Emulator::new(crate::emu::MIN_SIZE, 0);
            emulator.process(b"x");
            let mut v = view(&emulator, Mode::Term(Pane::Agent));
            v.side_panes = false;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
            terminal
                .draw(|frame| {
                    render(frame, &v);
                })
                .expect("draw");
            for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
                terminal
                    .draw(|frame| render_full(frame, &emulator, Some("x"), mode, false, None))
                    .expect("draw");
            }
        }
    }

    #[test]
    fn ended_line_covers_the_agent_bottom_row_and_hides_the_cursor() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"ab");
        let mut v = view(&emulator, Mode::Agent);
        v.ended = Some(" claude exited (code 0) · r relaunch · q quit ");
        let terminal = draw(100, 10, &v);
        // The agent's last row is the one above the bottom of its frame.
        let bottom = row(&terminal, 7);
        assert_eq!(symbol(&terminal, 20, 8), "╰");
        assert!(
            bottom.contains("claude exited (code 0) · r relaunch · q quit"),
            "{bottom}"
        );
        assert!(!terminal.backend().cursor_visible());
        // The whole row is one grey chip, past the text too, that starts
        // with the muted glyph of a session that is over.
        assert!(bottom.contains("│ × claude exited"), "{bottom}");
        assert_eq!(fg(&terminal, 22, 7), MUTED, "×");
        assert_eq!(fg(&terminal, 24, 7), CHIP_TEXT, "the text");
        for x in 21..74 {
            assert_eq!(bg(&terminal, x, 7), CHIP, "column {x}");
        }
        assert_eq!(bg(&terminal, 74, 7), Color::Reset, "the frame");
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   r relaunch · q quit · Ctrl+Q termis mode"),
            "{status}"
        );
        // While other sessions remain, x and q close this one.
        let mut several = view(&emulator, Mode::Agent);
        several.ended = v.ended;
        several.position = Some((1, 2));
        let status = row(&draw(100, 10, &several), 9);
        assert!(
            status.starts_with(" claude   r relaunch · x close · q close · Ctrl+Q termis mode"),
            "{status}"
        );
        // A line longer than the row is cut, with an ellipsis before the
        // frame.
        several.ended = Some(" claude exited (code 3) · r relaunch · x close · q close ");
        let bottom = row(&draw(100, 10, &several), 7);
        assert!(
            bottom.contains("│ × claude exited (code 3) · r relaunch · x close · q…│"),
            "{bottom}"
        );

        // In termis mode the line stays, the cursor stays hidden and the bar
        // offers the relaunch among the commands.
        let mut side = view(&emulator, Mode::Term(Pane::Changes));
        side.ended = v.ended;
        let terminal = draw(100, 10, &side);
        assert!(row(&terminal, 7).contains("r relaunch"));
        let status = row(&terminal, 9);
        assert!(status.starts_with(" TERMIS "), "{status}");
        assert!(
            status.contains("z zoom · r relaunch · q quit · Esc back"),
            "{status}"
        );
        assert!(!terminal.backend().cursor_visible());

        let mut terminal = Terminal::new(TestBackend::new(20, 3)).expect("terminal");
        terminal
            .draw(|frame| render_full(frame, &emulator, Some(" ended "), Mode::Agent, false, None))
            .expect("draw");
        assert!(row(&terminal, 2).starts_with(" × ended "));
        assert!(!terminal.backend().cursor_visible());
    }
}
