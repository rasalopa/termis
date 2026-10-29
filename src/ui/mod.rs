//! Drawing: the three panes, the status bar, and the full-screen view.

use std::path::PathBuf;

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
use crate::keys::ModeKey;
use crate::strings::{self, TitlePart};

pub mod changes;
pub mod commit;
pub mod diff;
pub mod help;
pub mod layout;
pub mod palette;
pub mod picker;
pub mod projects;
pub mod select;
pub mod toast;
pub mod tree;
pub mod viewer;

/// Columns of the center pane's title room that the title the program in
/// the tab shown set takes besides its own: a blank on each side of it,
/// the stretch of the frame after it and two cells of the frame before
/// it. Without the frame, eight blanks or more part it from the tabs.
const TITLE_GAP: usize = 5;
/// The least columns the title the program in the tab shown set is
/// drawn in; with fewer it goes.
const TITLE_MIN: usize = 8;

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
    /// The names of the session's tabs after the agent's, its terminals
    /// and its viewer in the order they were opened, after the agent's
    /// tab in the center pane title.
    pub terminals: &'a [&'a str],
    /// The index of the tab shown among them; none on the agent's tab.
    /// The status bar names a terminal shown while keys go to it.
    pub terminal: Option<usize>,
    /// Where the session shown is among several, from 1, and how many
    /// there are, for the status bar; none while there is one.
    pub position: Option<(usize, usize)>,
    /// What the tab shown shows.
    pub center: Center<'a>,
    /// The text selected there, drawn reversed.
    pub marked: Option<select::Marked>,
    /// Where keys go, with the highlighted pane. The app keeps the
    /// highlight on the agent's pane while `sides` has no side pane, and
    /// shows the changes pane while it is highlighted.
    pub mode: Mode,
    /// The side panes on screen.
    pub sides: layout::Sides,
    /// The widths of the side panes.
    pub widths: layout::Widths,
    /// Whether the side panes are hidden by the zoom rather than by the
    /// width.
    pub zoomed: bool,
    /// The line to draw over the agent's screen once it ended.
    pub ended: Option<&'a str>,
    /// Whether the changes pane has a repository to show: the session's
    /// own, or one its agent wrote in.
    pub repo: bool,
    /// The name of the repository the changes pane shows when it is not
    /// the session's own but one its agent wrote in, which its title
    /// names.
    pub written_repo: Option<&'a str>,
    /// The repository as last read, once the worker read it.
    pub changes: Option<&'a Snapshot>,
    /// The row of the changes pane that the list keys act on.
    pub selected: Option<&'a changes::Selected>,
    /// How far the changes pane was scrolled on the previous frame; the
    /// renderer returns the offset to pass back on the next one.
    pub scroll: usize,
    /// The commit draft while its box is open.
    pub draft: Option<&'a commit::Draft>,
    /// Whether the agent is writing a subject for the draft, which the
    /// box says in a chip.
    pub suggesting: bool,
    /// A notice shown in place of the key hints while it lasts.
    pub toast: Option<&'a toast::Toast>,
    /// Whether the key list is open over the screen.
    pub help: bool,
    /// The diff shown in the changes pane in place of the lists.
    pub diff: Option<&'a diff::Diff>,
    /// The card of the left pane.
    pub card: projects::Card<'a>,
    /// The list of sessions, from two sessions on or while the session
    /// shown has repositories its agent wrote in: in the left pane while
    /// the side panes are shown and the session's file tree is not, else
    /// as the count of those asking in the status bar.
    pub list: Option<projects::List<'a>>,
    /// The session's file tree, while the left pane shows it in place of
    /// the list of sessions and the card.
    pub tree: Option<tree::Pane<'a>>,
    /// The project picker while it is open over the screen.
    pub picker: Option<&'a picker::Picker>,
    /// Whether `e` has a file to open in the editor, which the termis-mode
    /// hints then offer.
    pub edit: bool,
    /// Whether `p` has something to push, which the termis-mode hints
    /// then offer with the changes pane highlighted.
    pub push: bool,
    /// Whether a push from the repository shown runs, which the status
    /// bar says in a chip.
    pub pushing: bool,
    /// The termis-mode key, which the status bar and the key list name.
    pub mode_key: ModeKey,
    /// The colours the frame is drawn with.
    pub palette: palette::Palette,
}

/// What the center pane shows of the tab shown.
#[derive(Clone, Copy)]
pub enum Center<'a> {
    /// A child's screen: the agent's or a terminal's.
    Screen(&'a Emulator),
    /// A file, in the viewer.
    File(viewer::Pane<'a>),
}

impl<'a> Center<'a> {
    /// How many lines the view is scrolled back through a screen's
    /// history; none in the viewer.
    fn scrolled(self) -> usize {
        match self {
            Self::Screen(emulator) => emulator.scrolled(),
            Self::File(_) => 0,
        }
    }

    /// The title the child of a screen set; none in the viewer.
    fn title(self) -> Option<&'a str> {
        match self {
            Self::Screen(emulator) => emulator.title(),
            Self::File(_) => None,
        }
    }

    /// Whether the scroll keys have something to move: not on a screen's
    /// alternate screen, which keeps no history.
    fn scrollable(self) -> bool {
        match self {
            Self::Screen(emulator) => !emulator.alternate_screen(),
            Self::File(_) => true,
        }
    }

    /// Draws it in `area`: a screen with the host cursor where its child
    /// has it while `cursor`, or the viewer; the text `marked` reversed.
    fn render(self, frame: &mut Frame, area: Rect, cursor: bool, marked: Option<select::Marked>) {
        match self {
            Self::Screen(emulator) => render_agent(frame, area, emulator, cursor),
            Self::File(pane) => viewer::render(frame, area, &pane),
        }
        if let Some(marked) = marked
            && let Some(grid) = select::Grid::new(self, area)
        {
            select::draw(frame.buffer_mut(), grid.area(), marked);
        }
    }
}

impl View<'_> {
    /// Where the scrolled parts of the side panes start as given, for
    /// a frame that does not draw them, and no rows shown.
    pub fn scrolls(&self) -> Scrolls {
        Scrolls {
            changes: self.scroll,
            tree: self.tree.as_ref().map_or(0, |tree| tree.folds.scroll),
            list: self.list.as_ref().map_or(0, |list| list.scroll),
            pages: Pages::default(),
        }
    }
}

/// Where the scrolled parts of the side panes start, for the next frame,
/// and how many rows of them showed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scrolls {
    /// The changes pane's lists, or its diff.
    pub changes: usize,
    /// The file tree.
    pub tree: usize,
    /// The list of sessions.
    pub list: usize,
    /// The rows shown, which half and whole pages move by.
    pub pages: Pages,
}

/// How many rows of the side panes' lists and views a frame showed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pages {
    /// The lines the changes pane's lists had room for, or its diff
    /// under the header.
    pub changes: usize,
    /// The rows the file tree had room for, or the list of sessions
    /// showed.
    pub projects: usize,
}

/// What a click can land on, as a frame drew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The row at this index of the list of sessions.
    Listed(usize),
    /// The entry of the file tree at this path, relative to the tree's
    /// directory, and whether it is a directory.
    Entry(PathBuf, bool),
    /// A file of the changes pane's lists.
    Change(changes::Selected),
    /// A tab named in the center pane's title: the agent's, none, else
    /// the terminal or the viewer at this index of [`View::terminals`].
    Tab(Option<usize>),
}

/// Where a frame drew what a click can land on: each target with the
/// cells it took, in the order drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets(Vec<(Rect, Target)>);

impl Targets {
    /// Notes that `target` was drawn on the cells of `area`.
    fn add(&mut self, area: Rect, target: Target) {
        self.0.push((area, target));
    }

    /// The target drawn on the cell at `position`; none where none was.
    #[cfg(test)]
    pub fn at(&self, position: Position) -> Option<&Target> {
        self.0
            .iter()
            .find(|(area, _)| area.contains(position))
            .map(|(_, target)| target)
    }

    /// Each target with the cells it took, in the order drawn.
    pub fn iter(&self) -> impl Iterator<Item = &(Rect, Target)> {
        self.0.iter()
    }
}

#[cfg(test)]
impl FromIterator<(Rect, Target)> for Targets {
    fn from_iter<I: IntoIterator<Item = (Rect, Target)>>(targets: I) -> Self {
        Self(targets.into_iter().collect())
    }
}

/// Draws each of `rows` on a row of `area`, from its top, as many as
/// fit, and notes in `targets` the target of each row that has one, on
/// the cells of that row: what the lists draw a click can land on.
fn draw_rows(
    frame: &mut Frame,
    area: Rect,
    rows: Vec<(Line<'_>, Option<Target>)>,
    targets: &mut Targets,
) {
    for (row, (line, target)) in area.rows().zip(rows) {
        frame.render_widget(Paragraph::new(line), row);
        if let Some(target) = target {
            targets.add(row, target);
        }
    }
}

/// Where the agent's screen goes for a terminal of `area`: the whole area
/// when `full_screen`, else the center pane's interior beside the side
/// panes `sides` has, of `widths`, which takes the width of those not
/// shown.
pub fn agent_area(
    area: Rect,
    full_screen: bool,
    sides: layout::Sides,
    widths: layout::Widths,
) -> Rect {
    if full_screen {
        area
    } else {
        pane_inner(layout::panes_with(area, sides, widths).center)
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

/// Draws a pane's frame and `title` in `area` and returns its interior,
/// with the cells each part of the title took: while `highlighted`, a
/// rounded frame in the accent with the title on its top edge; else the
/// title alone, in the top margin, `indent` columns into the interior.
/// `end`, in the center pane the title the program in the tab shown set,
/// goes muted at the right end of the same row, before a stretch of the
/// frame while highlighted.
fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    title: strings::Title,
    end: Option<String>,
    highlighted: bool,
    indent: u16,
) -> (Rect, Vec<Rect>) {
    let palette = palette::current();
    let inner = pane_inner(area);
    let title = title_line(title, highlighted);
    let end = end.map(|end| Span::styled(end, Style::new().fg(palette.muted)));
    // The title's row: the top edge between the frame's corners, else
    // the top margin from `indent` columns into the interior.
    let indent = if highlighted {
        0
    } else {
        indent.min(inner.width)
    };
    let row = Rect {
        x: inner.x + indent,
        y: area.y,
        width: inner.width - indent,
        height: area.height.min(1),
    };
    let (spans, base) = if highlighted {
        let mut block = frame_block();
        if let Some(end) = end {
            let accent = Style::new().fg(palette.accent);
            let spans = vec![
                Span::styled(" ", accent),
                end,
                Span::styled(format!(" {}", symbols::line::HORIZONTAL), accent),
            ];
            block = block.title(Line::from(spans).right_aligned());
        }
        frame.render_widget(block, area);
        (framed_title(title.spans), Style::new().fg(palette.text))
    } else {
        if let Some(end) = end {
            frame.render_widget(Paragraph::new(Line::from(end).right_aligned()), row);
        }
        (title.spans, Style::new())
    };
    // Each span on its place; while highlighted, over the text colour of
    // the frame's titles, as a block draws its title.
    let places = places(row, &spans);
    for (place, span) in places.iter().zip(spans) {
        frame.buffer_mut().set_style(*place, base);
        frame.render_widget(span, *place);
    }
    // The stretch of the frame before the title and the blank after it
    // are not its parts.
    let parts = if highlighted {
        places[1..places.len() - 1].to_vec()
    } else {
        places
    };
    (inner, parts)
}

/// The cells each of `spans` takes on `row`, laid one after the other
/// from its left end and cut where it ends: none past it.
fn places(row: Rect, spans: &[Span<'_>]) -> Vec<Rect> {
    let mut x = row.x;
    spans
        .iter()
        .map(|span| {
            let start = x.min(row.right());
            x = x.saturating_add(u16::try_from(span.width()).unwrap_or(u16::MAX));
            Rect {
                x: start,
                width: x.min(row.right()) - start,
                ..row
            }
        })
        .collect()
}

/// The rounded frame in the accent of the highlighted pane, the picker
/// and the key list, its titles in the text colour.
fn frame_block() -> Block<'static> {
    let palette = palette::current();
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.accent))
        .title_style(Style::new().fg(palette.text))
}

/// `title` as it goes on the top edge of a frame: after a stretch of the
/// frame and a blank, and before a blank, those in the accent.
fn framed_title(title: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let palette = palette::current();
    let accent = Style::new().fg(palette.accent);
    let mut spans = vec![Span::styled(
        format!("{} ", symbols::line::HORIZONTAL),
        accent,
    )];
    spans.extend(title);
    spans.push(Span::styled(" ", accent));
    spans
}

/// A rounded frame in the accent with `title` on its top edge, after a
/// stretch of the frame: the picker's and the key list's.
fn framed(title: Vec<Span<'static>>) -> Block<'static> {
    frame_block().title(Line::from(framed_title(title)))
}

/// `title` as a pane draws it: the name in the accent and bold while the
/// pane is `highlighted`, else muted; the subject in bold; the details
/// muted; the separators faint.
fn title_line(title: strings::Title, highlighted: bool) -> Line<'static> {
    let palette = palette::current();
    let spans: Vec<Span> = title
        .into_iter()
        .map(|(part, text)| {
            let style = match part {
                TitlePart::Name if highlighted => {
                    Style::new().fg(palette.accent).add_modifier(Modifier::BOLD)
                }
                TitlePart::Name | TitlePart::Detail => Style::new().fg(palette.muted),
                TitlePart::Subject => Style::new().add_modifier(Modifier::BOLD),
                TitlePart::Separator => Style::new().fg(palette.faint),
            };
            Span::styled(text, style)
        })
        .collect();
    Line::from(spans)
}

/// Draws a faint rule down column `x` of `area`: the line between two
/// panes when neither has a frame.
fn draw_rule(frame: &mut Frame, area: Rect, x: u16) {
    let palette = palette::current();
    let style = Style::new().fg(palette.faint);
    for y in area.top()..area.bottom() {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_symbol(symbols::line::VERTICAL).set_style(style);
        }
    }
}

/// A count of lines added, in their colour; muted when it is zero.
fn lines_added(count: u64) -> Span<'static> {
    let palette = palette::current();
    let colour = if count == 0 {
        palette.muted
    } else {
        palette.added
    };
    Span::styled(strings::added(count), Style::new().fg(colour))
}

/// A count of lines removed, in their colour; muted when it is zero.
fn lines_removed(count: u64) -> Span<'static> {
    let palette = palette::current();
    let colour = if count == 0 {
        palette.muted
    } else {
        palette.removed
    };
    Span::styled(strings::removed(count), Style::new().fg(colour))
}

/// A section's heading in `width` columns: `name` muted, then a faint
/// rule to the end of the row.
fn heading(name: &str, width: usize) -> Line<'static> {
    let palette = palette::current();
    let name = changes::cut(&changes::printable(name), width);
    let rule = width.saturating_sub(name.width() + 1);
    Line::from(vec![
        Span::styled(name, Style::new().fg(palette.muted)),
        Span::raw(" "),
        Span::styled(
            symbols::line::HORIZONTAL.repeat(rule),
            Style::new().fg(palette.faint),
        ),
    ])
}

/// Draws the agent of `view`, its center, over the whole frame, with the
/// cursor where the agent has it while keys go to it and the text marked
/// reversed. The last row shows the termis-mode tag and commands, or the
/// toast in their place while it lasts, while that mode is on, else the
/// line saying the agent ended once it did; the key list goes over it
/// while open. In the view's palette.
pub fn render_full(frame: &mut Frame, view: &View<'_>) {
    palette::drawing(view.palette, || render_full_frame(frame, view));
}

/// [`render_full`] with the view's palette current.
fn render_full_frame(frame: &mut Frame, view: &View<'_>) {
    let area = frame.area();
    let cursor = view.mode == Mode::Agent && view.ended.is_none();
    view.center.render(frame, area, cursor, view.marked);
    match view.mode {
        Mode::Term(_) | Mode::Commit | Mode::Picker => {
            let bar = Bar {
                agent: "",
                side_panes: false,
                zoom: strings::Zoom::None,
                open: false,
                sessions: false,
                list: false,
                repo_row: false,
                tabs: false,
                terminal: false,
                tree: false,
                edit: false,
                push: false,
                ended: view.ended.is_some(),
                selection: false,
                toast: view.toast,
                help: view.help,
                diff: false,
                scrollable: view.center.scrollable(),
                mode_key: view.mode_key,
            };
            let keys = mode_keys(view.mode, bar, usize::from(area.width));
            render_last_row(frame, area, keys, Style::new());
        }
        Mode::Agent => render_ended(frame, area, view.ended),
    }
    if view.help {
        self::help::render(frame, area, view.mode_key.label());
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

/// Draws the three panes and the status bar, in the view's palette.
/// Returns where the changes pane, the file tree and the list of
/// sessions start on the next frame and how many rows of them showed,
/// and where it drew what a click can land on.
pub fn render(frame: &mut Frame, view: &View<'_>) -> (Scrolls, Targets) {
    palette::drawing(view.palette, || render_panes(frame, view))
}

/// [`render`] with the view's palette current.
fn render_panes(frame: &mut Frame, view: &View<'_>) -> (Scrolls, Targets) {
    let palette = view.palette;
    let panes = layout::panes_with(frame.area(), view.sides, view.widths);
    let side_panes = view.sides != layout::Sides::None;
    let changes_pane = view.sides == layout::Sides::Both;
    let focus = match view.mode.pane() {
        _ if !side_panes => Pane::Agent,
        Pane::Changes if !changes_pane => Pane::Agent,
        pane => pane,
    };
    let mut scrolls = view.scrolls();
    let mut targets = Targets::default();

    if side_panes {
        let projects_title = vec![(TitlePart::Name, strings::PANE_PROJECTS.to_owned())];
        let (left_inner, _) = draw_pane(
            frame,
            panes.left,
            projects_title,
            None,
            focus == Pane::Projects,
            1,
        );
        let left_inner = padded(left_inner);
        // One line between neighbours: the highlighted pane's frame, else
        // a faint rule in the side pane's margin.
        match focus {
            Pane::Changes => draw_rule(frame, panes.left, panes.left.right().saturating_sub(1)),
            Pane::Projects => draw_rule(frame, panes.right, panes.right.x),
            Pane::Agent => {}
        }
        let drawn = projects::render(
            frame,
            left_inner,
            &view.card,
            view.list.as_ref(),
            view.tree.as_ref(),
            focus == Pane::Projects,
            &mut targets,
        );
        if let Some(tree) = drawn.tree {
            scrolls.tree = tree;
        }
        if let Some(list) = drawn.list {
            scrolls.list = list;
        }
        scrolls.pages.projects = drawn.shown;
    }
    if changes_pane {
        (scrolls.changes, scrolls.pages.changes) = render_changes(
            frame,
            panes.right,
            view,
            focus == Pane::Changes,
            &mut targets,
        );
    }

    // Room inside the frame, as for the changes pane.
    let room = usize::from(panes.center.width).saturating_sub(5);
    let title = strings::agent_pane_title(
        view.agent,
        view.project,
        view.terminals,
        view.terminal,
        view.center.scrolled(),
        room,
    );
    // The title the program in the tab shown set, in what the tabs leave,
    // so that they stay put when it changes; none over an agent that
    // ended, whose title would be stale.
    let used: usize = title.parts.iter().map(|(_, text)| text.width()).sum();
    let free = room.saturating_sub(used + TITLE_GAP);
    let end = view
        .center
        .title()
        .filter(|_| view.ended.is_none() && free >= TITLE_MIN)
        .map(|end| changes::cut(end, free));
    let (inner, places) = draw_pane(
        frame,
        panes.center,
        title.parts,
        end,
        focus == Pane::Agent,
        0,
    );
    // A tab's name takes the cells of the parts naming it that show.
    for (parts, tab) in title.tabs {
        let Some((first, last)) = places
            .get(parts)
            .and_then(|cells| cells.first().zip(cells.last()))
        else {
            continue;
        };
        let cells = Rect {
            width: last.right() - first.x,
            ..*first
        };
        if !cells.is_empty() {
            targets.add(cells, Target::Tab(tab));
        }
    }
    view.center.render(
        frame,
        inner,
        view.mode == Mode::Agent && view.ended.is_none(),
        view.marked,
    );
    render_ended(frame, inner, view.ended);

    // The mode tag and its keys on the left, the project flush right,
    // after a grey chip while a push runs and the count of the other
    // sessions asking, in a chip, while the list is hidden or the file
    // tree is in its place. Unzoom is offered only when the width would
    // show the sides again.
    let zoom = if view.zoomed && layout::sides_visible(frame.area()) {
        strings::Zoom::Out
    } else if side_panes {
        strings::Zoom::In
    } else {
        strings::Zoom::None
    };
    let asking = view
        .list
        .filter(|_| !side_panes || view.tree.is_some())
        .map_or(0, |list| list.asking());
    let mut spans = Vec::new();
    if view.pushing {
        let chip = Style::new().fg(palette.chip_text).bg(palette.chip);
        spans.push(Span::styled(format!(" {} ", strings::PUSHING), chip));
        spans.push(Span::raw("  "));
    }
    if asking > 0 {
        let chip = Style::new()
            .fg(palette.ink)
            .bg(palette.asking)
            .add_modifier(Modifier::BOLD);
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
    let chip = match view.center {
        Center::File(_) => strings::MODE_VIEW,
        Center::Screen(_) => tab.unwrap_or(view.agent),
    };
    let bar = Bar {
        agent: chip,
        side_panes,
        zoom,
        open: true,
        sessions: view.position.is_some(),
        list: view.list.is_some(),
        repo_row: view
            .list
            .is_some_and(|list| matches!(list.rows.get(list.cursor), Some(projects::Row::Repo(_)))),
        tabs: !view.terminals.is_empty(),
        terminal: tab.is_some(),
        tree: view.tree.is_some(),
        edit: view.edit,
        push: view.push,
        ended: view.ended.is_some(),
        selection: view.selected.is_some(),
        toast: view.toast,
        help: view.help,
        diff: view.diff.is_some(),
        scrollable: view.center.scrollable(),
        mode_key: view.mode_key,
    };
    let keys = mode_keys(view.mode, bar, usize::from(left.width));
    frame.render_widget(Paragraph::new(keys), left);
    frame.render_widget(Paragraph::new(project), right);
    if view.help {
        help::render(frame, frame.area(), view.mode_key.label());
    }
    if let Some(picker) = view.picker
        && let Some(position) = picker::render(frame, frame.area(), picker)
    {
        frame.set_cursor_position(position);
    }
    (scrolls, targets)
}

/// Draws the changes pane in `area`, `highlighted` or not, as `view`
/// says: the title with the branch, then the lists, the diff, or why
/// there is no repository, noting in `targets` where it drew the files
/// of its lists. Returns where its lists or its diff start on the next
/// frame, and how many lines of them showed.
fn render_changes(
    frame: &mut Frame,
    area: Rect,
    view: &View<'_>,
    highlighted: bool,
    targets: &mut Targets,
) -> (usize, usize) {
    let branch = view.changes.map(|snapshot| &snapshot.status.branch);
    // Room inside the frame: its corners, the stretch before the title
    // and the space after it.
    let room = usize::from(area.width).saturating_sub(5);
    let title = strings::changes_pane_title(
        view.written_repo,
        branch.and_then(branch_label),
        branch.and_then(|branch| branch.upstream.as_deref()),
        branch.and_then(|branch| branch.ahead_behind),
        room,
    );
    let (inner, _) = draw_pane(frame, area, title, None, highlighted, 1);
    let inner = padded(inner);
    match (view.changes, view.diff) {
        _ if !view.repo => {
            render_empty(frame, inner, strings::NOT_A_REPOSITORY);
            (view.scroll, 0)
        }
        (Some(_), Some(diff)) => diff::render(frame, inner, diff, view.scroll),
        (Some(snapshot), None) => {
            let (scroll, page, cursor) = changes::render(
                frame,
                inner,
                snapshot,
                changes::Lists {
                    selected: view.selected,
                    focused: highlighted,
                    scroll: view.scroll,
                },
                view.draft.map(|draft| commit::Open {
                    draft,
                    suggesting: view.suggesting,
                }),
                view.card.now,
                targets,
            );
            if let Some(position) = cursor
                && view.mode == Mode::Commit
            {
                frame.set_cursor_position(position);
            }
            (scroll, page)
        }
        (None, _) => (view.scroll, 0),
    }
}

/// What the status bar shows besides the mode.
struct Bar<'a> {
    /// The name of the agent or of the terminal shown, or `view` on the
    /// viewer, for the tag while keys go to it.
    agent: &'a str,
    side_panes: bool,
    zoom: strings::Zoom,
    /// Whether `o` opens the project picker and `t` a terminal.
    open: bool,
    /// Whether there are other sessions for `n` and `N` or `Alt+1` to
    /// `Alt+9` to show, which `x` leaves on screen.
    sessions: bool,
    /// Whether the projects pane has the list of sessions, whose keys it
    /// offers while it shows it.
    list: bool,
    /// Whether the list's cursor is on a repository an agent wrote in,
    /// which `x` takes off the list.
    repo_row: bool,
    /// Whether the session shown has other tabs for `,` and `.` to show.
    tabs: bool,
    /// Whether the tab shown is a terminal or the viewer, which `x`
    /// closes.
    terminal: bool,
    /// Whether the projects pane shows the file tree, whose keys it
    /// offers, and `f` the list of sessions.
    tree: bool,
    /// Whether `e` has a file to open in the editor.
    edit: bool,
    /// Whether `p` has something to push.
    push: bool,
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
    /// The termis-mode key, which the hints of agent mode end with.
    mode_key: ModeKey,
}

/// The mode tag followed by the keys that apply, within `width` columns:
/// the name of the agent or of the terminal shown, or `view` on the
/// viewer, in a grey chip, how to show another session from two sessions
/// on and how to enter termis mode, `TERMIS` in a chip in the accent and
/// the commands (how to close the key list while it is open), `COMMIT`
/// and the box's keys, or `OPEN` and the picker's.
/// A toast takes the place of the keys while it lasts: a notice that
/// something worked in the accent, a failure in the alert colour, an
/// agent that asks in the asking colour and bold.
fn mode_keys(mode: Mode, bar: Bar<'_>, width: usize) -> Line<'static> {
    let palette = palette::current();
    let Bar {
        agent,
        side_panes,
        zoom,
        open,
        sessions,
        list,
        repo_row,
        tabs,
        terminal,
        tree,
        edit,
        push,
        ended,
        selection,
        toast,
        help,
        diff,
        scrollable,
        mode_key,
    } = bar;
    let accent_chip = Style::new()
        .fg(palette.ink)
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let (tag, tag_style, hints) = match mode {
        Mode::Agent => (
            agent,
            Style::new().fg(palette.chip_text).bg(palette.chip),
            strings::agent_hints(ended, sessions, mode_key.label()),
        ),
        Mode::Term(_) if help => (strings::MODE_TERM, accent_chip, strings::help_hints()),
        Mode::Term(Pane::Changes) if diff => {
            (strings::MODE_TERM, accent_chip, strings::diff_hints())
        }
        Mode::Term(pane) => (
            strings::MODE_TERM,
            accent_chip,
            strings::term_hints(
                match pane {
                    Pane::Projects if side_panes && tree => strings::PaneKeys::Tree,
                    Pane::Projects if side_panes && list => strings::PaneKeys::Projects,
                    Pane::Agent if scrollable => strings::PaneKeys::Agent,
                    Pane::Changes if side_panes && selection => strings::PaneKeys::Changes,
                    _ => strings::PaneKeys::None,
                },
                strings::Offers {
                    side_panes,
                    zoom,
                    ended,
                    open,
                    sessions,
                    tabs,
                    // `x` closes the row under the list's cursor, which
                    // starts on the tab shown; it quits on the last
                    // session's agent's tab, and takes a repository off
                    // the list.
                    close: sessions || terminal || repo_row,
                    // `f` changes what the projects pane shows, while it
                    // is on screen.
                    files: side_panes,
                    tree,
                    edit,
                    // `p` pushes from the changes pane only.
                    push: push && pane == Pane::Changes,
                },
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
                toast::Kind::Info => Style::new().fg(palette.accent),
                toast::Kind::Error => Style::new().fg(palette.alert),
                toast::Kind::Attention => {
                    Style::new().fg(palette.asking).add_modifier(Modifier::BOLD)
                }
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
    let palette = palette::current();
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, &(keys, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                strings::HINT_SEPARATOR,
                Style::new().fg(palette.faint),
            ));
        }
        spans.push(Span::styled(
            keys,
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action}"),
            Style::new().fg(palette.muted),
        ));
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
    let palette = palette::current();
    let line = Line::styled(text.to_owned(), Style::new().fg(palette.muted));
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
    let palette = palette::current();
    if let Some(text) = ended {
        let bar = Style::new().fg(palette.chip_text).bg(palette.chip);
        let glyph = format!(" {}", strings::ENDED_GLYPH);
        let room = usize::from(area.width).saturating_sub(glyph.width());
        let line = Line::from(vec![
            Span::styled(glyph, Style::new().fg(palette.muted)),
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
    use crate::ui::palette::{ACCENT, ALERT, ASKING, CHIP, CHIP_TEXT, FAINT, INK, MUTED};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;
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
            center: Center::Screen(emulator),
            marked: None,
            mode,
            sides: layout::Sides::Both,
            widths: layout::Widths::DEFAULT,
            zoomed: false,
            ended: None,
            repo: true,
            written_repo: None,
            changes: None,
            selected: None,
            scroll: 0,
            draft: None,
            suggesting: false,
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
            tree: None,
            picker: None,
            edit: false,
            push: false,
            pushing: false,
            mode_key: ModeKey::DEFAULT,
            palette: palette::Palette::DEFAULT,
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
        let mut emulator = Emulator::new(
            agent_area(
                Rect::new(0, 0, 100, 10),
                false,
                layout::Sides::None,
                layout::Widths::DEFAULT,
            )
            .into(),
            0,
        );
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
        let mut emulator = Emulator::new(
            agent_area(
                Rect::new(0, 0, 100, 10),
                false,
                layout::Sides::None,
                layout::Widths::DEFAULT,
            )
            .into(),
            0,
        );
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.sides = layout::Sides::None;
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
    fn the_bar_and_the_key_list_name_the_termis_mode_key_of_the_view() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let ctrl_o: ModeKey = "ctrl+o".parse().expect("allowed");
        let v = View {
            mode_key: ctrl_o,
            ..view(&emulator, Mode::Agent)
        };
        let status = row(&draw(100, 10, &v), 9);
        assert!(
            status.starts_with(" claude   Ctrl+O termis mode "),
            "{status}"
        );
        assert!(!status.contains("Ctrl+Q"), "{status}");
        // The key list, beside the panes and over the whole terminal.
        let listing = View {
            mode: Mode::Term(Pane::Agent),
            help: true,
            ..v
        };
        let rows = |terminal: &Terminal<TestBackend>| -> String {
            (0..40)
                .map(|y| row(terminal, y))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut full = Terminal::new(TestBackend::new(100, 40)).expect("terminal");
        full.draw(|frame| render_full(frame, &listing))
            .expect("draw");
        for screen in [rows(&draw(100, 40, &listing)), rows(&full)] {
            for keys in ["Ctrl+O / Alt+1..9", "Esc, i or Ctrl+O / ?", "Esc / Ctrl+O"] {
                assert!(screen.contains(keys), "{keys}\n{screen}");
            }
            assert!(!screen.contains("Ctrl+Q"), "{screen}");
        }
    }

    #[test]
    fn a_frame_takes_the_colours_of_its_view_and_leaves_them_behind() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let red = palette::Palette {
            accent: Color::Red,
            ..palette::Palette::DEFAULT
        };
        let v = View {
            palette: red,
            ..view(&emulator, Mode::Term(Pane::Agent))
        };
        let center = layout::panes(Rect::new(0, 0, 100, 10)).center;
        let terminal = draw(100, 10, &v);
        assert_eq!(bg(&terminal, 1, 9), Color::Red, "the TERMIS chip");
        assert_eq!(fg(&terminal, center.x, 0), Color::Red, "the frame");
        assert_eq!(palette::current(), palette::Palette::DEFAULT);
        let mut full = Terminal::new(TestBackend::new(100, 10)).expect("terminal");
        full.draw(|frame| render_full(frame, &v)).expect("draw");
        assert_eq!(bg(&full, 1, 9), Color::Red, "over the whole terminal");
        assert_eq!(palette::current(), palette::Palette::DEFAULT);
        // The next frame, in the default palette, and a pane drawn on its
        // own, outside a frame.
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Agent)));
        assert_eq!(bg(&terminal, 1, 9), ACCENT);
        assert_eq!(fg(&terminal, center.x, 0), ACCENT);
        let mut alone = Terminal::new(TestBackend::new(100, 40)).expect("terminal");
        alone
            .draw(|frame| help::render(frame, frame.area(), "Ctrl+Q"))
            .expect("draw");
        let corner = (0..40)
            .find_map(|y| {
                let x = row(&alone, y).chars().position(|c| c == '╭')?;
                Some((u16::try_from(x).ok()?, y))
            })
            .expect("the key list");
        assert_eq!(fg(&alone, corner.0, corner.1), ACCENT);
    }

    #[test]
    fn a_busy_frame_draws_no_colour_its_palette_does_not_have() {
        use crate::agent::{Limit, Limits, State, Status};
        use crate::git::numstat::Lines;
        use crate::git::status::{Change, Entry};
        use crate::git::today::Today;
        use crate::ui::palette::{ADDED, MODIFIED, REMOVED};
        use std::time::{Duration, Instant};
        // A colour for each entry that no default is, so that one drawn
        // from a constant shows as a default. `TEXT` is the terminal's
        // own, which a cell has anyway.
        let sentinels = palette::Palette {
            text: Color::Indexed(100),
            accent: Color::Indexed(101),
            asking: Color::Indexed(102),
            alert: Color::Indexed(103),
            added: Color::Indexed(104),
            removed: Color::Indexed(105),
            modified: Color::Indexed(106),
            muted: Color::Indexed(107),
            faint: Color::Indexed(108),
            chip: Color::Indexed(109),
            chip_text: Color::Indexed(110),
            ink: Color::Indexed(111),
        };
        let defaults = [
            ACCENT, ASKING, ALERT, ADDED, REMOVED, MODIFIED, MUTED, FAINT, CHIP, CHIP_TEXT, INK,
        ];
        let mut seen = std::collections::HashSet::new();
        let mut check = |terminal: &Terminal<TestBackend>, case: &str| {
            let buffer = terminal.backend().buffer();
            for (at, cell) in buffer.content().iter().enumerate() {
                let (x, y) = buffer.pos_of(at);
                assert!(
                    !defaults.contains(&cell.fg) && !defaults.contains(&cell.bg),
                    "{case}: ({x}, {y}) {cell:?}"
                );
                seen.extend([cell.fg, cell.bg]);
            }
        };
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process(b"\x1b]2;a title\x07x");
        // With a file in conflict whose lines git did not count.
        let mut snapshot = changes::tests::sample();
        snapshot.status.entries.push(Entry {
            path: "lib.bin".into(),
            original: None,
            index: None,
            worktree: Some(Change::Unmerged),
            submodule: false,
        });
        snapshot.worktree.insert("lib.bin".into(), Lines::Binary);
        let toasts = [
            toast::Kind::Info,
            toast::Kind::Error,
            toast::Kind::Attention,
        ]
        .map(|kind| {
            toast::Toast::new(
                "a notice".into(),
                kind,
                Instant::now(),
                Duration::from_secs(5),
            )
        });
        let now = 1_000_000;
        let status = Status {
            context: Some(50),
            limits: Limits {
                five_hours: Some(Limit {
                    used: 85,
                    resets_at: now + 3600,
                }),
                week: Some(Limit {
                    used: 96,
                    resets_at: now + 86400,
                }),
            },
            state: Some(State::Attention),
            ..Status::default()
        };
        let working = Status {
            state: Some(State::Working),
            ..Status::default()
        };
        let session = |project, status, ended, changed| {
            projects::Row::Session(projects::SessionRow {
                project,
                status,
                ended,
                changed,
            })
        };
        let rows = [
            session("term", Some(&working), false, true),
            projects::Row::Terminal(projects::TerminalRow {
                name: "zsh",
                running: true,
            }),
            repo_row("lib", true, false),
            projects::Row::More(2),
            session("api", Some(&status), false, false),
            session("old", None, true, false),
        ];
        let paths = ["src/app.rs", "new.txt", "Cargo.toml"].map(std::path::PathBuf::from);
        // A list cut short, with a directory and a file that hold changes.
        let files = tree::Tree::new(&paths, Some(3));
        let folds = tree::Folds::default();
        let tree_changes = tree::Changes::new(&snapshot.status, std::path::Path::new(""));
        let first = changes::Selected {
            section: changes::Section::Staged,
            path: "src/app.rs".into(),
        };
        let mut draft = commit::Draft::default();
        draft.apply(&commit::Edit::Insert("feat: x".into()));
        let request = crate::git::diff::Request {
            paths: vec!["src/app.rs".into()],
            staged: false,
            untracked: false,
        };
        let mut d = diff::Diff::new(request.clone(), "src/app.rs".into());
        assert!(d.accept(&request, "@@ -1 +1 @@\n-a\n+b\n"));
        let home = std::path::Path::new("/Users/x");
        let open = picker::Item::open("term".into(), home.join("dev/term"), Some(home));
        let recent = picker::Item::recent("api".into(), home.join("dev/api"), Some(home));
        let picker = picker::Picker::new(vec![open], vec![recent]);
        let content = viewer::Content::Text(viewer::Text::new("fn main() {}\n"));
        let modes = [
            Mode::Agent,
            Mode::Term(Pane::Agent),
            Mode::Term(Pane::Changes),
            Mode::Term(Pane::Projects),
            Mode::Commit,
            Mode::Picker,
        ];
        for (mode, toast) in modes.into_iter().zip(toasts.iter().cycle()) {
            let mut v = View {
                palette: sentinels,
                ..view(&emulator, mode)
            };
            v.changes = Some(&snapshot);
            v.selected = Some(&first);
            v.toast = Some(toast);
            v.position = Some((1, 3));
            v.pushing = true;
            v.card.now = now;
            v.card.agent = Some(projects::AgentCard {
                name: "claude",
                status: &status,
                time: None,
                ended: false,
            });
            v.draft = (mode == Mode::Commit).then_some(&draft);
            v.suggesting = true;
            v.picker = (mode == Mode::Picker).then_some(&picker);
            for cursor in 0..rows.len() {
                v.list = Some(projects::List {
                    rows: &rows,
                    shown: 0,
                    cursor,
                    scroll: 0,
                });
                check(&draw(140, 40, &v), &format!("{mode:?}, row {cursor}"));
            }
            v.tree = Some(tree::Pane {
                heading: "term",
                content: Some(Ok(&files)),
                folds: &folds,
                changes: &tree_changes,
            });
            v.diff = Some(&d);
            check(
                &draw(140, 40, &v),
                &format!("{mode:?}, the tree and a diff"),
            );
            v.help = true;
            check(&draw(140, 40, &v), &format!("{mode:?}, the key list"));
            let mut full = Terminal::new(TestBackend::new(140, 40)).expect("terminal");
            full.draw(|frame| render_full(frame, &v)).expect("draw");
            check(&full, &format!("{mode:?}, over the whole terminal"));
            v.help = false;
            v.center = Center::File(viewer::Pane {
                content: &content,
                scroll: 0,
            });
            check(&draw(140, 40, &v), &format!("{mode:?}, the viewer"));
        }
        // What could not be read, is being read or is empty.
        let troubled = Snapshot {
            today: Err("git log: fatal: bad".into()),
            last: Err("git log: fatal: bad".into()),
            ..Snapshot::default()
        };
        let quiet = Snapshot {
            today: Ok(Today::default()),
            ..Snapshot::default()
        };
        let failed = viewer::Content::Failed("bad".into());
        let none = picker::Picker::new(vec![], vec![]);
        for (mode, repo, content) in [
            (
                Mode::Term(Pane::Changes),
                &troubled,
                &viewer::Content::Reading,
            ),
            (Mode::Picker, &quiet, &failed),
        ] {
            let mut v = View {
                palette: sentinels,
                ..view(&emulator, mode)
            };
            v.changes = Some(repo);
            v.tree = Some(tree::Pane {
                heading: "term",
                content: Some(Err("git ls-files: fatal: bad")),
                folds: &folds,
                changes: &tree_changes,
            });
            v.center = Center::File(viewer::Pane { content, scroll: 0 });
            v.picker = (mode == Mode::Picker).then_some(&none);
            check(&draw(140, 40, &v), &format!("{mode:?}, what failed"));
        }
        let mut v = View {
            palette: sentinels,
            ..view(&emulator, Mode::Agent)
        };
        v.ended = Some(" claude exited (code 0) · r relaunch · q quit ");
        v.card.agent = Some(projects::AgentCard {
            name: "claude",
            status: &status,
            time: None,
            ended: true,
        });
        check(&draw(140, 40, &v), "an agent that ended");
        let mut full = Terminal::new(TestBackend::new(140, 40)).expect("terminal");
        full.draw(|frame| render_full(frame, &v)).expect("draw");
        check(&full, "an agent that ended, over the whole terminal");
        // And each of the palette's was drawn somewhere.
        for n in 100..=111 {
            assert!(seen.contains(&Color::Indexed(n)), "{n}");
        }
    }

    #[test]
    fn the_panes_take_the_widths_of_the_view() {
        let widths = layout::Widths::new(40, 15).expect("allowed");
        let area = Rect::new(0, 0, 100, 10);
        let emulator = Emulator::new(
            agent_area(area, false, layout::Sides::Both, widths).into(),
            0,
        );
        let mut v = view(&emulator, Mode::Agent);
        v.widths = widths;
        let terminal = draw(100, 10, &v);
        assert_eq!(symbol(&terminal, 40, 0), "╭", "the center's left edge");
        assert_eq!(symbol(&terminal, 84, 0), "╮", "and its right one");
        let top = row(&terminal, 0);
        let changes: String = top.chars().skip(85 + 2).take(7).collect();
        assert_eq!(changes, "Changes", "{top}");
        assert_eq!(fg(&terminal, 87, 0), MUTED, "{top}");
    }

    #[test]
    fn narrow_zoomed_view_offers_no_zoom_key() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.sides = layout::Sides::None;
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
        // A repository the agent wrote in is named before the branch.
        v.written_repo = Some("lib");
        assert!(row(&draw(100, 10, &v), 0).contains("╭─ Changes · lib ─"));
        let snapshot = changes::tests::sample();
        v.changes = Some(&snapshot);
        assert!(
            row(&draw(140, 10, &v), 0).contains("Changes · lib · main"),
            "{}",
            row(&draw(140, 10, &v), 0)
        );
    }

    #[test]
    fn hints_shrink_to_the_bar() {
        let all = strings::term_hints(
            strings::PaneKeys::Changes,
            strings::Offers {
                side_panes: true,
                zoom: strings::Zoom::In,
                ended: false,
                open: true,
                sessions: false,
                tabs: false,
                close: false,
                files: true,
                tree: false,
                edit: false,
                push: true,
            },
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
            "? keys · j/k move · s stage · u unstage · a all · c commit · p push · o open · t terminal · 1/2/3 pane · z zoom · q quit · Esc back",
            "f files goes first here"
        );
        let narrower = fit_hints(all.clone(), joined(&narrower).width() - 1);
        assert_eq!(
            joined(&narrower),
            "? keys · j/k move · s stage · u unstage · a all · c commit · p push · o open · 1/2/3 pane · z zoom · q quit · Esc back",
            "then t terminal"
        );
        let push = "? keys · j/k move · s stage · u unstage · a all · p push · q quit";
        assert_eq!(
            joined(&fit_hints(all.clone(), push.width())),
            push,
            "c commit goes before p push"
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
    fn a_push_running_shows_in_a_grey_chip_and_p_on_changes_while_there_is_something_to_push() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        let status = row(&draw(120, 10, &v), 9);
        assert!(!status.contains("p push"), "{status}");
        v.push = true;
        let status = row(&draw(120, 10, &v), 9);
        assert!(status.contains(" · p push · "), "{status}");
        assert!(!status.contains("pushing"), "{status}");
        // Not on the other panes, where `p` does nothing.
        for pane in [Pane::Projects, Pane::Agent] {
            v.mode = Mode::Term(pane);
            let status = row(&draw(120, 10, &v), 9);
            assert!(!status.contains("p push"), "{pane:?}: {status}");
        }
        v.push = false;
        v.pushing = true;
        let terminal = draw(120, 10, &v);
        let status = row(&terminal, 9);
        assert!(status.ends_with("  pushing   termis · term "), "{status}");
        let at = status.find(" pushing ").expect("the chip");
        let x = u16::try_from(status[..at].chars().count()).expect("on screen");
        for x in x..x + 9 {
            assert_eq!(bg(&terminal, x, 9), CHIP, "the chip, column {x}");
            assert_eq!(fg(&terminal, x, 9), CHIP_TEXT);
        }
        assert_eq!(bg(&terminal, x + 9, 9), Color::Reset, "past the chip");
    }

    #[test]
    fn the_commit_box_offers_a_suggestion_and_says_while_one_is_written() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = Snapshot::default();
        let mut draft = commit::Draft::default();
        draft.apply(&commit::Edit::Insert("feat: x".into()));
        let mut v = view(&emulator, Mode::Commit);
        v.changes = Some(&snapshot);
        v.draft = Some(&draft);
        let all = |terminal: &Terminal<TestBackend>| -> Vec<String> {
            (0..20).map(|y| row(terminal, y)).collect()
        };
        let terminal = draw(140, 20, &v);
        let status = row(&terminal, 19);
        assert!(
            status.starts_with(
                " COMMIT   Enter commit · Ctrl+J body line · Ctrl+G suggest · Esc cancel"
            ),
            "{status}"
        );
        assert!(
            !all(&terminal)
                .iter()
                .any(|line| line.contains("suggesting"))
        );
        v.suggesting = true;
        let terminal = draw(140, 20, &v);
        let title = all(&terminal)
            .into_iter()
            .position(|line| line.contains("commit   suggesting "))
            .expect("the chip after the box's title");
        assert!(row(&terminal, u16::try_from(title + 2).expect("a row")).contains("feat: x"));
        // Without the box, nothing says so.
        v.draft = None;
        let terminal = draw(140, 20, &v);
        assert!(
            !all(&terminal)
                .iter()
                .any(|line| line.contains("suggesting"))
        );
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
        // `x` closes the session, terminal or viewer under the list's
        // cursor, and takes a repository off it.
        assert!(keys.contains("session, tab, repo; x closes it"), "{keys}");
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
        full.draw(|frame| render_full(frame, &view(&emulator, Mode::Term(Pane::Agent))))
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
        let rows = ["api", "term", "web"].map(|project| {
            projects::Row::Session(projects::SessionRow {
                project,
                status: None,
                ended: false,
                changed: false,
            })
        });
        v.list = Some(projects::List {
            rows: &rows,
            shown: 1,
            cursor: 1,
            scroll: 0,
        });
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
        v.list = None;
        let status = row(&draw(130, 10, &v), 9);
        assert!(status.ends_with("termis · term "), "{status}");
        assert!(!status.contains("n/N"), "{status}");
        assert!(
            status.contains("? keys · o open · t terminal · f files · 1/2/3 pane"),
            "{status}"
        );
        // One session whose agent wrote in another repository has the
        // list, and its keys.
        let one = [rows[1], repo_row("lib", false, false)];
        v.list = Some(projects::List {
            rows: &one,
            shown: 0,
            cursor: 0,
            scroll: 0,
        });
        let status = row(&draw(130, 10, &v), 9);
        assert!(
            status.contains("? keys · j/k move · Enter show · o open · t terminal"),
            "{status}"
        );
        assert!(!status.contains("n/N"), "{status}");
        // `x` would quit on the session's row; on the repository's it
        // takes it off the list.
        assert!(!status.contains("x close"), "{status}");
        v.list = Some(projects::List {
            rows: &one,
            shown: 0,
            cursor: 1,
            scroll: 0,
        });
        let status = row(&draw(130, 10, &v), 9);
        assert!(status.contains("x close · q quit"), "{status}");
    }

    #[test]
    fn agent_mode_offers_the_alt_digits_from_two_sessions_on() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let mut v = view(&emulator, Mode::Agent);
        let status = row(&draw(100, 10, &v), 9);
        assert!(
            status.starts_with(" claude   Ctrl+Q termis mode "),
            "{status}"
        );
        assert!(!status.contains('⌥'), "one session: {status}");
        // From two sessions on, before the way to termis mode; the keys in
        // bold, a column each, what they do muted.
        v.position = Some((1, 2));
        let terminal = draw(100, 10, &v);
        let status = row(&terminal, 9);
        assert!(
            status.starts_with(" claude   ⌥1-9 session · Ctrl+Q termis mode "),
            "{status}"
        );
        assert!(status.ends_with(" termis · term 1/2 "), "{status}");
        assert_eq!(symbol(&terminal, 10, 9), "⌥");
        assert_eq!(symbol(&terminal, 11, 9), "1");
        assert!(bold(&terminal, 10, 9));
        assert_eq!(fg(&terminal, 15, 9), MUTED, "session");
        assert_eq!(fg(&terminal, 23, 9), FAINT, "the separator");
        // On a terminal and on the viewer too, which do not get them.
        let content = viewer::Content::Text(viewer::Text::new("fn main() {}\n"));
        let terminals = ["zsh", "main.rs"];
        v.terminals = &terminals;
        v.terminal = Some(0);
        let status = row(&draw(100, 10, &v), 9);
        assert!(
            status.starts_with(" zsh   ⌥1-9 session · Ctrl+Q termis mode "),
            "{status}"
        );
        v.terminal = Some(1);
        v.center = Center::File(viewer::Pane {
            content: &content,
            scroll: 0,
        });
        let status = row(&draw(100, 10, &v), 9);
        assert!(
            status.starts_with(" view   ⌥1-9 session · Ctrl+Q termis mode "),
            "{status}"
        );
        // Termis mode offers n/N instead.
        v.mode = Mode::Term(Pane::Agent);
        let status = row(&draw(170, 10, &v), 9);
        assert!(status.contains("n/N session"), "{status}");
        assert!(!status.contains('⌥'), "{status}");
        // A narrow bar drops it; the way to termis mode and the project at
        // the right end stay whole.
        const PROJECT: &str = "a-project-named-to-fill-thirty";
        let mut v = view(&emulator, Mode::Agent);
        v.sides = layout::Sides::None;
        v.position = Some((1, 2));
        v.project = PROJECT;
        let status = row(&draw(80, 10, &v), 9);
        let project = format!("termis · {PROJECT} 1/2 ");
        let left_width = 80 - project.chars().count() - 1;
        let keys: String = status.chars().take(left_width).collect();
        assert_eq!(keys.trim_end(), " claude   Ctrl+Q termis mode", "{status}");
        assert!(status.ends_with(&format!(" {project}")), "{status}");
        // Once the agent ended, `x close` goes before it: `q` closes the
        // session too.
        let mut v = view(&emulator, Mode::Agent);
        v.ended = Some(" claude exited (code 0) · r relaunch · x close · q close ");
        v.position = Some((1, 2));
        let status = row(&draw(90, 10, &v), 9);
        assert!(
            status
                .starts_with(" claude   r relaunch · q close · ⌥1-9 session · Ctrl+Q termis mode "),
            "{status}"
        );
        assert!(status.ends_with(" termis · term 1/2 "), "{status}");
    }

    /// The list's row of a repository an agent wrote in.
    fn repo_row(name: &str, changed: bool, shown: bool) -> projects::Row<'_> {
        projects::Row::Repo(projects::RepoRow {
            name,
            changed,
            shown,
        })
    }

    #[test]
    fn the_list_shows_beside_the_panes_else_the_bar_counts_who_asks() {
        use crate::agent::{State, Status};
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let asking = Status {
            state: Some(State::Attention),
            ..Status::default()
        };
        let session = |project, status| {
            projects::Row::Session(projects::SessionRow {
                project,
                status,
                ended: false,
                changed: false,
            })
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
            scroll: 0,
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
        v.sides = layout::Sides::None;
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
            scroll: 0,
        });
        let status = row(&draw(100, 10, &v), 9);
        assert!(status.ends_with("  termis · term 1/3 "), "{status}");
        assert!(!status.contains("asking"), "{status}");
    }

    #[test]
    fn the_tree_takes_the_left_pane_and_the_bar_counts_who_asks() {
        use crate::agent::{State, Status};
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let asking = Status {
            state: Some(State::Attention),
            ..Status::default()
        };
        let session = |project, status| {
            projects::Row::Session(projects::SessionRow {
                project,
                status,
                ended: false,
                changed: false,
            })
        };
        let rows = [session("term", None), session("api", Some(&asking))];
        let paths = ["src/main.rs", "Cargo.toml"].map(std::path::PathBuf::from);
        let files = tree::Tree::new(&paths, None);
        let folds = tree::Folds::default();
        let changes = tree::Changes::default();
        let mut v = view(&emulator, Mode::Term(Pane::Projects));
        v.position = Some((1, 2));
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 0,
            scroll: 0,
        });
        v.tree = Some(tree::Pane {
            heading: "term",
            content: Some(Ok(&files)),
            folds: &folds,
            changes: &changes,
        });
        let terminal = draw(140, 10, &v);
        // In the frame of the highlighted pane, past its padding: the
        // heading and the tree, the cursor's row in the accent; no list.
        assert!(
            row(&terminal, 1).starts_with("│ term "),
            "{}",
            row(&terminal, 1)
        );
        assert!(
            row(&terminal, 2).starts_with("│ ▸ src "),
            "{}",
            row(&terminal, 2)
        );
        assert!(row(&terminal, 3).starts_with("│   Cargo.toml "));
        assert_eq!(fg(&terminal, 4, 2), ACCENT);
        assert!((1..9).all(|y| !row(&terminal, y).contains("api")));
        // The bar offers the tree's keys and the list, and counts the
        // other sessions that ask, as when the list is hidden.
        let status = row(&terminal, 9);
        assert!(
            status.contains("? keys · j/k move · l/Enter open · h close · f sessions"),
            "{status}"
        );
        assert!(
            status.ends_with(" ! 1 asking   termis · term 1/2 "),
            "{status}"
        );
        // With the list back, neither.
        v.tree = None;
        let status = row(&draw(170, 10, &v), 9);
        assert!(status.contains("Enter show"), "{status}");
        assert!(status.contains("f files"), "{status}");
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
    fn a_frame_says_how_many_rows_of_the_side_panes_it_showed() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = changes::tests::sample();
        let pages = |v: &View<'_>| {
            let mut terminal = Terminal::new(TestBackend::new(100, 10)).expect("terminal");
            let mut scrolls = Scrolls::default();
            terminal
                .draw(|frame| (scrolls, _) = render(frame, v))
                .expect("draw");
            scrolls.pages
        };
        // Seven rows inside the panes: the lists have five over today's
        // two; the card has no rows to move through.
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.changes = Some(&snapshot);
        assert_eq!(
            pages(&v),
            Pages {
                changes: 5,
                projects: 0
            }
        );
        // The diff, six under its header.
        let request = crate::git::diff::Request {
            paths: vec!["src/app.rs".into()],
            staged: false,
            untracked: false,
        };
        let d = diff::Diff::new(request, "src/app.rs".into());
        v.diff = Some(&d);
        assert_eq!(pages(&v).changes, 6);
        // The list of three sessions, all of it; the tree, six rows under
        // its heading.
        let session = |project| {
            projects::Row::Session(projects::SessionRow {
                project,
                status: None,
                ended: false,
                changed: false,
            })
        };
        let rows = [session("term"), session("api"), session("web")];
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 0,
            scroll: 0,
        });
        assert_eq!(pages(&v).projects, 3);
        let paths = ["src/main.rs", "Cargo.toml"].map(std::path::PathBuf::from);
        let files = tree::Tree::new(&paths, None);
        let folds = tree::Folds::default();
        let changes = tree::Changes::default();
        v.tree = Some(tree::Pane {
            heading: "term",
            content: Some(Ok(&files)),
            folds: &folds,
            changes: &changes,
        });
        assert_eq!(pages(&v).projects, 6);
        // Without the side panes, none.
        v.sides = layout::Sides::None;
        v.zoomed = true;
        assert_eq!(pages(&v), Pages::default());
    }

    #[test]
    fn a_frame_says_where_the_list_of_sessions_starts_on_the_next() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let list_scroll = |v: &View<'_>| {
            let mut terminal = Terminal::new(TestBackend::new(100, 9)).expect("terminal");
            let mut scrolls = Scrolls::default();
            terminal
                .draw(|frame| (scrolls, _) = render(frame, v))
                .expect("draw");
            scrolls.list
        };
        let session = |project| {
            projects::Row::Session(projects::SessionRow {
                project,
                status: None,
                ended: false,
                changed: false,
            })
        };
        let rows = ["one", "two", "three", "four", "five", "six"].map(session);
        // Five rows of the list over the path: the cursor on the sixth
        // moves it down one.
        let mut v = view(&emulator, Mode::Term(Pane::Projects));
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 5,
            scroll: 0,
        });
        assert_eq!(list_scroll(&v), 1);
        // While the tree takes its place, where it was.
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 0,
            scroll: 3,
        });
        assert_eq!(list_scroll(&v), 0, "the list drawn");
        let paths = [std::path::PathBuf::from("Cargo.toml")];
        let files = tree::Tree::new(&paths, None);
        let folds = tree::Folds::default();
        let changes = tree::Changes::default();
        v.tree = Some(tree::Pane {
            heading: "term",
            content: Some(Ok(&files)),
            folds: &folds,
            changes: &changes,
        });
        assert_eq!(list_scroll(&v), 3, "the tree drawn");
    }

    /// Draws `view` in `width` by `height` cells. Returns the terminal,
    /// and where the frame drew what a click can land on.
    fn targeted(width: u16, height: u16, view: &View<'_>) -> (Terminal<TestBackend>, Targets) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut targets = Targets::default();
        terminal
            .draw(|frame| (_, targets) = render(frame, view))
            .expect("draw");
        (terminal, targets)
    }

    /// The text of the cells of `area`, row after row.
    fn text(terminal: &Terminal<TestBackend>, area: Rect) -> String {
        let buffer = terminal.backend().buffer();
        area.positions()
            .map(|at| buffer.cell(at).map_or(" ", Cell::symbol))
            .collect()
    }

    /// The rows of the lists that `targets` has, with their cells.
    fn row_targets(targets: &Targets) -> Vec<(Rect, Target)> {
        targets
            .iter()
            .filter(|(_, target)| {
                matches!(
                    target,
                    Target::Listed(_) | Target::Entry(..) | Target::Change(_)
                )
            })
            .cloned()
            .collect()
    }

    #[test]
    fn a_frame_says_where_it_drew_each_row_of_the_side_panes() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let snapshot = changes::tests::sample();
        let session = |project| {
            projects::Row::Session(projects::SessionRow {
                project,
                status: None,
                ended: false,
                changed: false,
            })
        };
        let names = ["term", "api", "web", "docs", "site", "blog"];
        let rows = names.map(session);
        let mut v = view(&emulator, Mode::Term(Pane::Changes));
        v.changes = Some(&snapshot);
        v.list = Some(projects::List {
            rows: &rows,
            shown: 0,
            cursor: 5,
            scroll: 0,
        });
        // At 100 by 9 the projects pane has six rows inside, 16 columns
        // between its padding: five rows of the list from the second,
        // which keeps the cursor's in view, then the path.
        let (terminal, targets) = targeted(100, 9, &v);
        let drawn: Vec<(Rect, Target)> = row_targets(&targets)
            .into_iter()
            .filter(|(_, target)| matches!(target, Target::Listed(_)))
            .collect();
        let listed: Vec<Target> = (1..6).map(Target::Listed).collect();
        assert_eq!(
            drawn
                .iter()
                .map(|(_, target)| target.clone())
                .collect::<Vec<_>>(),
            listed
        );
        for (y, (area, target)) in (1..).zip(&drawn) {
            assert_eq!(*area, Rect::new(2, y, 16, 1), "{target:?}");
            let Target::Listed(index) = target else {
                unreachable!("{target:?}")
            };
            assert!(text(&terminal, *area).contains(names[*index]), "{index}");
        }
        assert_eq!(targets.at(Position::new(5, 6)), None, "the path");
        assert_eq!(targets.at(Position::new(1, 1)), None, "the padding");
        assert_eq!(targets.at(Position::new(18, 1)), None, "the padding");
        // At 100 by 10 the changes pane has seven rows inside, the lists
        // five over today's two: the totals, a gap, the staged title and
        // its two files.
        let (terminal, targets) = targeted(100, 10, &v);
        let files: Vec<(Rect, Target)> = row_targets(&targets)
            .into_iter()
            .filter(|(_, target)| matches!(target, Target::Change(_)))
            .collect();
        let staged = ["src/app.rs", "src/ui/layout.rs"];
        assert_eq!(files.len(), staged.len(), "{files:?}");
        for ((y, (area, target)), path) in (4..).zip(&files).zip(staged) {
            assert_eq!(*area, Rect::new(77, y, 21, 1), "{target:?}");
            let selected = changes::Selected {
                section: changes::Section::Staged,
                path: path.into(),
            };
            assert_eq!(*target, Target::Change(selected));
            // The path as far as it shows, cut before its line counts.
            assert!(text(&terminal, *area).contains(&path[..6]), "{path}");
        }
        assert_eq!(targets.at(Position::new(80, 1)), None, "the totals");
        assert_eq!(targets.at(Position::new(80, 3)), None, "a title");
        assert_eq!(targets.at(Position::new(80, 6)), None, "today");
        // No row in the center pane or the status bar.
        let center = layout::panes(Rect::new(0, 0, 100, 10)).center;
        assert!(
            row_targets(&targets)
                .iter()
                .all(|(area, _)| area.right() <= center.x || area.x >= center.right())
        );
        assert!(row_targets(&targets).iter().all(|(area, _)| area.y < 9));
        // The tree in place of the list: its entries, not its heading.
        let paths = ["src/main.rs", "Cargo.toml"].map(PathBuf::from);
        let files = tree::Tree::new(&paths, None);
        let folds = tree::Folds::default();
        let tree_changes = tree::Changes::default();
        v.tree = Some(tree::Pane {
            heading: "term",
            content: Some(Ok(&files)),
            folds: &folds,
            changes: &tree_changes,
        });
        let (terminal, targets) = targeted(100, 10, &v);
        let entries: Vec<(Rect, Target)> = row_targets(&targets)
            .into_iter()
            .filter(|(_, target)| !matches!(target, Target::Change(_)))
            .collect();
        assert_eq!(
            entries,
            [
                (Rect::new(2, 2, 16, 1), Target::Entry("src".into(), true)),
                (
                    Rect::new(2, 3, 16, 1),
                    Target::Entry("Cargo.toml".into(), false)
                ),
            ],
            "under the heading, and no row of the list"
        );
        assert!(text(&terminal, entries[1].0).contains("Cargo.toml"));
        // The diff in place of the lists: no file.
        let request = crate::git::diff::Request {
            paths: vec!["src/app.rs".into()],
            staged: false,
            untracked: false,
        };
        let d = diff::Diff::new(request, "src/app.rs".into());
        v.diff = Some(&d);
        let (_, targets) = targeted(100, 10, &v);
        assert!(
            !row_targets(&targets)
                .iter()
                .any(|(_, target)| matches!(target, Target::Change(_)))
        );
        // Without the side panes, no row.
        v.sides = layout::Sides::None;
        v.zoomed = true;
        let (_, targets) = targeted(100, 10, &v);
        assert!(row_targets(&targets).is_empty());
    }

    /// The tabs of the center title that `targets` has, with their cells.
    fn tab_targets(targets: &Targets) -> Vec<(Rect, Option<usize>)> {
        targets
            .iter()
            .filter_map(|(area, target)| match target {
                Target::Tab(tab) => Some((*area, *tab)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_frame_says_where_it_drew_each_tab_of_the_center_title() {
        let plain = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let emulator = titled();
        let center = layout::panes(Rect::new(0, 0, 100, 10)).center;
        let terminals = ["zsh", "zsh 2"];
        // Highlighted, after the corner and a stretch of the frame; else
        // from the corner's column on. With a terminal title at the right
        // end or not, and whichever tab is shown.
        for screen in [&plain, &emulator] {
            for (mode, start) in [
                (Mode::Agent, center.x + 3),
                (Mode::Term(Pane::Changes), center.x + 1),
                (Mode::Term(Pane::Projects), center.x + 1),
            ] {
                for shown in [None, Some(1)] {
                    let mut v = view(screen, mode);
                    v.terminals = &terminals;
                    v.terminal = shown;
                    let (terminal, targets) = targeted(100, 10, &v);
                    let tabs = tab_targets(&targets);
                    let names: Vec<String> = tabs
                        .iter()
                        .map(|(area, _)| text(&terminal, *area))
                        .collect();
                    assert_eq!(names, ["claude · term", "zsh", "zsh 2"], "{mode:?}");
                    let which: Vec<Option<usize>> = tabs.iter().map(|(_, tab)| *tab).collect();
                    assert_eq!(which, [None, Some(0), Some(1)]);
                    assert!(tabs.iter().all(|(area, _)| area.y == 0 && area.height == 1));
                    assert_eq!(tabs[0].0.x, start, "{mode:?}");
                    // Nothing from the last name to the pane's edge, the
                    // terminal title included.
                    for x in tabs[2].0.right()..center.right() {
                        assert_eq!(targets.at(Position::new(x, 0)), None, "{x}");
                    }
                }
            }
        }
        // Short of room: the count of the terminals left out is no tab,
        // and the one shown keeps its index.
        let four = ["zsh", "zsh 2", "zsh 3", "nvim main.rs"];
        let mut v = view(&plain, Mode::Agent);
        v.terminals = &four;
        v.terminal = Some(3);
        let (terminal, targets) = targeted(80, 10, &v);
        let top = row(&terminal, 0);
        assert!(
            top.contains("claude · term   zsh   nvim main.rs   +2"),
            "{top}"
        );
        let tabs = tab_targets(&targets);
        let names: Vec<String> = tabs
            .iter()
            .map(|(area, _)| text(&terminal, *area))
            .collect();
        assert_eq!(names, ["claude · term", "zsh", "nvim main.rs"]);
        assert_eq!(tabs[2].1, Some(3));
        // A name the pane cuts is a tab as far as it shows, up to the
        // corner.
        let mut v = view(&plain, Mode::Agent);
        v.project = "a-project-name-far-too-long-for-the-center-pane-to-show";
        let (terminal, targets) = targeted(100, 10, &v);
        let tabs = tab_targets(&targets);
        assert_eq!(tabs.len(), 1, "{tabs:?}");
        assert_eq!(tabs[0].0.x, center.x + 3);
        assert_eq!(tabs[0].0.right(), center.right() - 1);
        assert_eq!(symbol(&terminal, center.right() - 1, 0), "╮");
    }

    #[test]
    fn agent_area_is_the_center_interior() {
        use layout::Sides;
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(
            agent_area(area, true, Sides::Both, layout::Widths::DEFAULT),
            area
        );
        assert_eq!(
            agent_area(area, true, Sides::None, layout::Widths::DEFAULT),
            area
        );
        assert_eq!(
            agent_area(area, false, Sides::Both, layout::Widths::DEFAULT),
            Rect::new(21, 1, 53, 37)
        );
        // With the changes pane hidden, its width too.
        assert_eq!(
            agent_area(area, false, Sides::Projects, layout::Widths::DEFAULT),
            Rect::new(21, 1, 78, 37)
        );
        // Without the sides, zoomed or collapsed: the whole width inside one
        // border.
        assert_eq!(
            agent_area(area, false, Sides::None, layout::Widths::DEFAULT),
            Rect::new(1, 1, 98, 37)
        );
        let narrow = Rect::new(0, 0, 80, 40);
        assert_eq!(
            agent_area(narrow, false, Sides::None, layout::Widths::DEFAULT),
            Rect::new(1, 1, 78, 37)
        );
    }

    #[test]
    fn the_title_says_how_far_back_the_agent_view_is() {
        let mut emulator = Emulator::new(
            agent_area(
                Rect::new(0, 0, 100, 10),
                false,
                layout::Sides::Both,
                layout::Widths::DEFAULT,
            )
            .into(),
            10,
        );
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
        // Termis mode offers the tabs beside, and `x` closes the terminal
        // shown, on the projects pane too, where the list's cursor starts
        // on its row.
        v.mode = Mode::Term(Pane::Projects);
        let status = row(&draw(160, 10, &v), 9);
        assert!(
            status.contains(
                "t terminal · ,/. tab · f files · 1/2/3 pane · z zoom · x close · q quit"
            ),
            "{status}"
        );
        v.mode = Mode::Term(Pane::Changes);
        let status = row(&draw(160, 10, &v), 9);
        assert!(status.contains("z zoom · x close · q quit"), "{status}");
        v.terminal = None;
        for pane in [Pane::Changes, Pane::Projects] {
            v.mode = Mode::Term(pane);
            let status = row(&draw(160, 10, &v), 9);
            assert!(!status.contains("x close"), "the agent's tab quits");
        }
    }

    /// An emulator whose child set a title with OSC 0.
    fn titled() -> Emulator {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process("\x1b]0;✳ Orders pagination\x07".as_bytes());
        emulator
    }

    /// The cells of row `y` from column 0 up to `end`.
    fn cells(terminal: &Terminal<TestBackend>, y: u16, end: u16) -> Vec<Cell> {
        let buffer = terminal.backend().buffer();
        (0..end).map(|x| buffer[(x, y)].clone()).collect()
    }

    #[test]
    fn the_title_ends_with_the_one_the_tab_shown_set() {
        let plain = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let emulator = titled();
        let corner = layout::panes(Rect::new(0, 0, 100, 10)).center.right() - 1;
        // Highlighted: muted at the right end, before a stretch of the
        // frame, and the tabs where they are without it.
        let terminal = draw(100, 10, &view(&emulator, Mode::Agent));
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ claude · term ─"), "{top}");
        assert!(top.contains("─ ✳ Orders pagination ─╮  Changes"), "{top}");
        assert_eq!(symbol(&terminal, corner, 0), "╮");
        let first = corner - 21;
        assert_eq!(symbol(&terminal, first, 0), "✳");
        assert_eq!(fg(&terminal, first, 0), MUTED);
        assert!(!bold(&terminal, first, 0));
        assert_eq!(fg(&terminal, corner - 1, 0), ACCENT, "the frame");
        let without = draw(100, 10, &view(&plain, Mode::Agent));
        assert_eq!(
            cells(&terminal, 0, first - 1),
            cells(&without, 0, first - 1)
        );
        // Not highlighted: muted, ending where the interior ends.
        let terminal = draw(100, 10, &view(&emulator, Mode::Term(Pane::Changes)));
        let top = row(&terminal, 0);
        assert!(top.contains("│ claude · term "), "{top}");
        assert!(top.contains("✳ Orders pagination ╭─ Changes"), "{top}");
        assert_eq!(symbol(&terminal, corner - 1, 0), "n");
        let first = corner - 19;
        assert_eq!(fg(&terminal, first, 0), MUTED);
        assert!(!bold(&terminal, first, 0));
        let without = draw(100, 10, &view(&plain, Mode::Term(Pane::Changes)));
        assert_eq!(cells(&terminal, 0, first), cells(&without, 0, first));
    }

    #[test]
    fn the_title_goes_before_the_tabs_when_room_is_short() {
        let plain = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let emulator = titled();
        let three = ["zsh", "zsh 2", "zsh 3"];
        let mut v = view(&emulator, Mode::Agent);
        let mut without = view(&plain, Mode::Agent);
        v.terminals = &three;
        let top = row(&draw(100, 10, &v), 0);
        assert!(
            top.contains("╭─ claude · term   zsh   zsh 2   zsh 3 ── ✳ Orders … ─╮"),
            "{top}"
        );
        // Eight columns are the least it is drawn in.
        let eight = ["zsh", "zsh 2", "zsh 3 x"];
        v.terminals = &eight;
        let top = row(&draw(100, 10, &v), 0);
        assert!(top.contains("zsh 3 x ── ✳ Order… ─╮"), "{top}");
        let seven = ["zsh", "zsh 2", "zsh 3 xy"];
        v.terminals = &seven;
        without.terminals = &seven;
        assert_eq!(row(&draw(100, 10, &v), 0), row(&draw(100, 10, &without), 0));
        let four = ["zsh", "zsh 2", "zsh 3", "nvim main.rs"];
        v.terminals = &four;
        without.terminals = &four;
        // These fill the room: the title goes, and every tab is named.
        let top = row(&draw(100, 10, &v), 0);
        assert!(top.contains("zsh 3   nvim main.rs ╮"), "{top}");
        assert_eq!(top, row(&draw(100, 10, &without), 0));
    }

    #[test]
    fn a_wide_title_is_cut_between_characters() {
        let mut emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        emulator.process("\x1b]2;日本語のタイトルがとても長い場合の表示\x07".as_bytes());
        let v = view(&emulator, Mode::Agent);
        // The second cell of a wide character reads as a blank.
        for (width, shown) in [
            (100, "日 本 語 の タ イ ト ル が と て も 長 い 場 …"),
            (101, "日 本 語 の タ イ ト ル が と て も 長 い 場 合 …"),
        ] {
            let terminal = draw(width, 10, &v);
            let corner = layout::panes(Rect::new(0, 0, width, 10)).center.right() - 1;
            assert_eq!(symbol(&terminal, corner, 0), "╮", "{width}");
            let top = row(&terminal, 0);
            assert!(top.contains(&format!("── {shown} ─╮")), "{top}");
        }
    }

    #[test]
    fn no_title_shows_in_the_viewer_or_over_an_agent_that_ended() {
        let emulator = titled();
        let content = viewer::Content::Text(viewer::Text::new("fn main() {}\n"));
        let terminals = ["main.rs"];
        let mut v = view(&emulator, Mode::Agent);
        v.terminals = &terminals;
        v.terminal = Some(0);
        v.center = Center::File(viewer::Pane {
            content: &content,
            scroll: 0,
        });
        // Nothing but the frame follows the tabs.
        let top = row(&draw(100, 10, &v), 0);
        let rule = "─".repeat(27);
        assert!(
            top.contains(&format!("╭─ claude · term   main.rs {rule}╮")),
            "{top}"
        );
        let mut v = view(&emulator, Mode::Agent);
        v.ended = Some(" claude exited (code 0) · r relaunch · q quit ");
        let top = row(&draw(100, 10, &v), 0);
        assert!(!top.contains("Orders"), "{top}");
    }

    #[test]
    fn the_viewer_takes_the_center_pane_and_the_chip_says_view() {
        let emulator = Emulator::new(Size { rows: 8, cols: 53 }, 0);
        let content = viewer::Content::Text(viewer::Text::new("fn main() {}\n"));
        let terminals = ["zsh", "main.rs"];
        let mut v = view(&emulator, Mode::Agent);
        v.terminals = &terminals;
        v.terminal = Some(1);
        v.center = Center::File(viewer::Pane {
            content: &content,
            scroll: 0,
        });
        let terminal = draw(100, 10, &v);
        // The file's name is the tab shown; its lines are in the pane, and
        // no child's cursor.
        let top = row(&terminal, 0);
        assert!(top.contains("╭─ claude · term   zsh   main.rs ─"), "{top}");
        let name = layout::panes(Rect::new(0, 0, 100, 10)).center.x + 3;
        assert!(bold(&terminal, name + 22, 0), "main.rs");
        assert!(!bold(&terminal, name + 9, 0), "term");
        let first = row(&terminal, 1);
        assert!(first.contains("│1 fn main() {}"), "{first}");
        assert!(!terminal.backend().cursor_visible());
        let status = row(&terminal, 9);
        assert!(status.starts_with(" view   Ctrl+Q termis mode"), "{status}");
        assert_eq!(bg(&terminal, 1, 9), CHIP);
        // In termis mode it has the agent pane's scroll keys, and `x`
        // closes it.
        v.mode = Mode::Term(Pane::Agent);
        let status = row(&draw(170, 10, &v), 9);
        assert!(
            status.contains("? keys · j/k scroll · PgUp/PgDn page · G end · o open"),
            "{status}"
        );
        assert!(status.contains("x close"), "{status}");
        // `e` while it has the file to open.
        v.edit = true;
        let status = row(&draw(180, 10, &v), 9);
        assert!(status.contains("G end · e edit · o open"), "{status}");
    }

    #[test]
    fn the_text_selected_is_drawn_reversed_on_the_screen_and_in_the_viewer() {
        let area = Rect::new(0, 0, 100, 10);
        let screen = agent_area(area, false, layout::Sides::Both, layout::Widths::DEFAULT);
        let mut emulator = Emulator::new(screen.into(), 0);
        emulator.process("one\r\ntwo 日本\r\nthree".as_bytes());
        let mut v = view(&emulator, Mode::Agent);
        let grid = select::Grid::new(v.center, screen).expect("cells");
        // From `n` of one to 日, whose style covers both its cells.
        v.marked = Some(grid.marked((0, 1), (1, 4), select::Unit::Cell));
        let terminal = draw(100, 10, &v);
        let reversed_in = |terminal: &Terminal<TestBackend>, y: u16| -> Vec<u16> {
            (0..100).filter(|&x| reversed(terminal, x, y)).collect()
        };
        let (x, y) = (screen.x, screen.y);
        assert_eq!(
            reversed_in(&terminal, y),
            (x + 1..screen.right()).collect::<Vec<_>>(),
            "the rest of the first row"
        );
        assert_eq!(
            reversed_in(&terminal, y + 1),
            (x..x + 5).collect::<Vec<_>>(),
            "the start of the last, up to 日"
        );
        assert!(reversed_in(&terminal, y + 2).is_empty());
        assert_eq!(symbol(&terminal, x + 4, y + 1), "日");
        // From the second cell of 日, which takes it whole.
        v.marked = Some(grid.marked((1, 5), (1, 6), select::Unit::Cell));
        let terminal = draw(100, 10, &v);
        assert_eq!(reversed_in(&terminal, y + 1), [x + 4, x + 6]);
        assert_eq!(symbol(&terminal, x + 6, y + 1), "本");
        // Over the whole terminal too.
        let mut full = Terminal::new(TestBackend::new(20, 4)).expect("terminal");
        let small = Emulator::new(Size { rows: 4, cols: 20 }, 0);
        let marked = select::Marked {
            start: (1, 2),
            end: (1, 3),
        };
        full.draw(|frame| {
            render_full(
                frame,
                &View {
                    marked: Some(marked),
                    ..view(&small, Mode::Agent)
                },
            );
        })
        .expect("draw");
        assert_eq!(reversed_in(&full, 1), [2, 3]);
        // In the viewer, past the line numbers.
        let content = viewer::Content::Text(viewer::Text::new("fn main() {}\n"));
        v.center = Center::File(viewer::Pane {
            content: &content,
            scroll: 0,
        });
        v.marked = Some(select::Marked {
            start: (0, 0),
            end: (0, 1),
        });
        let terminal = draw(100, 10, &v);
        assert_eq!(symbol(&terminal, x, y), "1");
        assert_eq!(reversed_in(&terminal, y), [x + 2, x + 3], "f and n");
    }

    #[test]
    fn the_bar_offers_no_scrolling_on_the_alternate_screen() {
        let mut emulator = Emulator::new(
            agent_area(
                Rect::new(0, 0, 100, 10),
                false,
                layout::Sides::Both,
                layout::Widths::DEFAULT,
            )
            .into(),
            10,
        );
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
        let mut emulator = Emulator::new(
            agent_area(
                Rect::new(0, 0, 80, 10),
                false,
                layout::Sides::None,
                layout::Widths::DEFAULT,
            )
            .into(),
            0,
        );
        emulator.process(b"hello");
        let mut v = view(&emulator, Mode::Agent);
        v.sides = layout::Sides::None;
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
    fn the_center_takes_the_width_of_the_changes_pane_hidden() {
        let area = Rect::new(0, 0, 100, 10);
        let mut emulator = Emulator::new(
            agent_area(
                area,
                false,
                layout::Sides::Projects,
                layout::Widths::DEFAULT,
            )
            .into(),
            0,
        );
        emulator.process(b"hello");
        let snapshot = changes::tests::sample();
        let mut v = view(&emulator, Mode::Agent);
        v.sides = layout::Sides::Projects;
        v.changes = Some(&snapshot);
        let panes = layout::panes_with(area, layout::Sides::Projects, layout::Widths::DEFAULT);
        let terminal = draw(100, 10, &v);
        let top = row(&terminal, 0);
        assert!(top.starts_with("  Projects "), "{top}");
        assert!(!top.contains("Changes"), "{top}");
        assert!(top.ends_with('╮'), "the agent's frame to the edge: {top}");
        assert_eq!(corners(&terminal, panes.center), "╭╮╰╯");
        assert!(row(&terminal, 1).contains("│hello"));
        assert!((0..10).all(|y| !row(&terminal, y).contains("4 files")));
        // With the projects pane highlighted, the rule in the hidden
        // pane's margin falls past the edge of the screen.
        v.mode = Mode::Term(Pane::Projects);
        let terminal = draw(100, 10, &v);
        assert_eq!(corners(&terminal, panes.left), "╭╮╰╯");
        let edge = panes.center.right() - 1;
        assert_eq!(column(&terminal, edge, panes.center.height).trim(), "");
        // Highlighted while hidden, which the app does not do, the agent's
        // pane keeps the frame.
        v.mode = Mode::Term(Pane::Changes);
        let terminal = draw(100, 10, &v);
        assert_eq!(corners(&terminal, panes.center), "╭╮╰╯");
        assert!(row(&terminal, 9).starts_with(" TERMIS "));
    }

    #[test]
    fn collapsed_bar_never_offers_the_pane_digits() {
        let emulator = Emulator::new(Size { rows: 8, cols: 78 }, 0);
        for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
            let mut v = view(&emulator, mode);
            v.sides = layout::Sides::None;
            let status = row(&draw(80, 10, &v), 9);
            assert!(!status.contains("1/2/3"), "{status}");
        }
        let mut v = view(&emulator, Mode::Term(Pane::Agent));
        v.sides = layout::Sides::None;
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
            .draw(|frame| render_full(frame, &view(&emulator, Mode::Agent)))
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
            .draw(|frame| render_full(frame, &view(&emulator, Mode::Term(Pane::Agent))))
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
                    &View {
                        ended: Some(" ended "),
                        ..view(&emulator, Mode::Term(Pane::Agent))
                    },
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
                    &View {
                        toast: Some(&asking),
                        ..view(&emulator, Mode::Term(Pane::Agent))
                    },
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
            .draw(|frame| {
                render_full(
                    frame,
                    &View {
                        help: true,
                        ..view(&emulator, Mode::Term(Pane::Agent))
                    },
                )
            })
            .expect("draw");
        assert!(
            row(&terminal, 0).contains("╭─ keys ─"),
            "{}",
            row(&terminal, 0)
        );
        // Four rows cannot hold the list: the bottom of the frame says so.
        assert!(row(&terminal, 3).contains('…'), "{}", row(&terminal, 3));
        terminal
            .draw(|frame| {
                render_full(
                    frame,
                    &View {
                        ended: Some(" ended "),
                        ..view(&emulator, Mode::Agent)
                    },
                )
            })
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
            emulator.process(b"\x1b]2;a title\x07x");
            let mut v = view(&emulator, Mode::Term(Pane::Agent));
            v.sides = layout::Sides::None;
            let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
            terminal
                .draw(|frame| {
                    render(frame, &v);
                })
                .expect("draw");
            for mode in [Mode::Agent, Mode::Term(Pane::Agent)] {
                terminal
                    .draw(|frame| {
                        render_full(
                            frame,
                            &View {
                                ended: Some("x"),
                                ..view(&emulator, mode)
                            },
                        )
                    })
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
        // While other sessions remain, x and q close this one, and
        // Alt+1 to Alt+9 still show another.
        let mut several = view(&emulator, Mode::Agent);
        several.ended = v.ended;
        several.position = Some((1, 2));
        let status = row(&draw(100, 10, &several), 9);
        assert!(
            status.starts_with(
                " claude   r relaunch · x close · q close · ⌥1-9 session · Ctrl+Q termis mode"
            ),
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
            .draw(|frame| {
                render_full(
                    frame,
                    &View {
                        ended: Some(" ended "),
                        ..view(&emulator, Mode::Agent)
                    },
                )
            })
            .expect("draw");
        assert!(row(&terminal, 2).starts_with(" × ended "));
        assert!(!terminal.backend().cursor_visible());
    }
}
