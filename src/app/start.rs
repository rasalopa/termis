//! The start screen, before the first session: what each key and click
//! on it opens, as a start from the command line would, and the loop that
//! reads the host's input until one opens a session or the user quits
//! (ADR-0015).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use super::account::{Account, Accounts};
use super::host::Host;
use super::open::{self, Target};
use super::remembered::Remembered;
use super::session::{self, Sessions};
use super::{
    Action, Options, account_for, chosen_account, first_line, focus_settled, focused_after,
    gained_after, picker_key, trouble,
};
use crate::git;
use crate::keys::ModeKey;
use crate::strings;
use crate::ui::palette::Palette;
use crate::ui::picker::{Choice, Picker};
use crate::ui::projects::display_path;
use crate::ui::start::{self as screen, Row};
use crate::ui::toast::Toast;

/// The session the start asks for: the one the command line names, or
/// the one chosen on the start screen.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Pick {
    /// The directory the agent runs in.
    pub(super) dir: PathBuf,
    /// The root of the repository around `dir`; none outside one.
    pub(super) repo: Option<PathBuf>,
    /// The account the agent runs as.
    pub(super) account: Account,
    /// Whether it starts a new conversation rather than continuing the
    /// directory's last one.
    pub(super) new_conversation: bool,
}

impl Pick {
    /// `options` with this session as the one asked for.
    pub(super) fn options<'a>(&'a self, options: Options<'a>) -> Options<'a> {
        Options {
            cwd: &self.dir,
            new_conversation: self.new_conversation,
            ..options
        }
    }
}

/// What the start screen offers, and what opening it needs.
pub(super) struct Offer<'a> {
    /// The current directory, which `.` opens.
    pub(super) cwd: &'a Path,
    /// The root of the repository around `cwd`; none outside one.
    pub(super) repo: Option<PathBuf>,
    /// The directories of the last run that are still there, in order;
    /// the screen keeps those with an account to run as, which are the
    /// ones the start reopens.
    pub(super) run: Vec<PathBuf>,
    /// The project picker with no session open: the screen numbers its
    /// first nine rows, and `o` opens it.
    pub(super) list: Picker,
    /// The home, shown as `~`, which a typed path counts from.
    pub(super) home: Option<&'a Path>,
    /// The accounts the agent runs as.
    pub(super) accounts: &'a Accounts,
    /// The account each directory last opened as.
    pub(super) remembered: &'a Remembered,
    /// The account the agent last ran each directory as.
    pub(super) last_ran: &'a HashMap<PathBuf, String>,
    /// The colours the screen is drawn with.
    pub(super) palette: Palette,
    /// The termis-mode key, which closes the picker.
    pub(super) mode_key: ModeKey,
}

/// Shows the start screen on `host`, offering what `offer` holds, until a
/// key or a click opens a session, which it returns, or a key quits, which
/// returns none.
/// No input thread runs yet: the screen reads the host's input itself.
/// Fails when the screen cannot be drawn or the input cannot be read.
pub(super) fn choose(host: &mut Host, offer: Offer<'_>) -> Result<Option<Pick>> {
    let mut screen = Screen::new(offer);
    loop {
        let view = screen.view();
        let mut rows = Vec::new();
        host.terminal
            .draw(|frame| rows = screen::render(frame, &view))
            .context("draw")?;
        screen.rows = rows;
        let event = event::read().context("read host input")?;
        match screen.on_event(event, Instant::now()) {
            Step::Stay => {}
            Step::Quit => return Ok(None),
            Step::Open(pick) => return Ok(Some(pick)),
        }
    }
}

/// The directory of the last `run` the start reopens showing it: `cwd`,
/// given canonical as the run's directories are, when the run has it,
/// else the run's first; none without a run.
fn run_target(run: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
    run.iter().find(|dir| *dir == cwd).or(run.first()).cloned()
}

/// The index among the picker's rows of the recent project `digit` opens,
/// `1` the first; none for another character.
fn recent_index(digit: char) -> Option<usize> {
    let number = usize::try_from(digit.to_digit(10)?).ok()?;
    number.checked_sub(1)
}

/// What an event on the start screen leads to.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing opens: the screen stays.
    Stay,
    /// Nothing starts: `termis` ends.
    Quit,
    /// A session to start, the choice made.
    Open(Pick),
}

/// The start screen between two events.
struct Screen<'a> {
    offer: Offer<'a>,
    /// The names of the projects the start reopens, in the run's order.
    names: Vec<String>,
    /// The current directory with the home as `~`.
    here: String,
    /// The current directory, canonical, whose repository is known.
    cwd: PathBuf,
    /// The picker `o` opened over the screen; every way out of it drops
    /// it, and what was typed with it.
    picker: Option<Picker>,
    /// Why the last choice did not open, until the next key or click.
    notice: Option<Toast>,
    /// Where the last frame drew each row, and what it opens.
    rows: Vec<(Rect, Row)>,
    /// Whether the host window has the focus, as its focus events last
    /// said; it has until one says otherwise.
    focused: bool,
    /// When the host window last gained the focus, so that the click that
    /// brings it forward opens nothing.
    gained: Option<Instant>,
}

impl<'a> Screen<'a> {
    fn new(mut offer: Offer<'a>) -> Self {
        // A directory with no account to run as does not reopen, so the
        // screen neither counts nor names it.
        let (accounts, remembered, last_ran) = (offer.accounts, offer.remembered, offer.last_ran);
        offer
            .run
            .retain(|dir| account_for(dir, accounts, remembered, last_ran).is_ok());
        let names = offer
            .run
            .iter()
            .map(|dir| session::project_name(dir))
            .collect();
        let here = display_path(offer.cwd, offer.home);
        let cwd = offer
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| offer.cwd.to_path_buf());
        Self {
            offer,
            names,
            here,
            cwd,
            picker: None,
            notice: None,
            rows: Vec::new(),
            focused: true,
            gained: None,
        }
    }

    /// What the next frame shows.
    fn view(&self) -> screen::View<'_> {
        screen::View {
            version: crate::VERSION,
            run: &self.names,
            here: &self.here,
            list: &self.offer.list,
            picker: self.picker.as_ref(),
            notice: self.notice.as_ref(),
            palette: self.offer.palette,
        }
    }

    /// What `event` does at `now`: a key press as [`Screen::key`] says, a
    /// click as [`Screen::mouse`] says, and the first line of a paste goes
    /// into the picker's query while it is open; each takes the notice
    /// away. Anything else does nothing.
    fn on_event(&mut self, event: Event, now: Instant) -> Step {
        self.focused = focused_after(self.focused, &event);
        self.gained = gained_after(self.gained, &event, now);
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.notice = None;
                self.key(&key)
            }
            Event::Paste(text) => {
                if let Some(picker) = &mut self.picker {
                    self.notice = None;
                    if let Some(line) = first_line(&text) {
                        picker.insert(line);
                    }
                }
                Step::Stay
            }
            Event::Mouse(mouse) => self.mouse(&mouse, now),
            _ => Step::Stay,
        }
    }

    /// What `mouse` does at `now`: a press of the left button takes the
    /// notice away, and on a row the last frame drew does what the row's
    /// key does. With the picker open it does nothing, and so does the
    /// press that brings the window forward, as [`focus_settled`] tells.
    /// Any other mouse event does nothing.
    fn mouse(&mut self, mouse: &MouseEvent, now: Instant) -> Step {
        let click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
        if !click || self.picker.is_some() || !focus_settled(self.focused, self.gained, now) {
            return Step::Stay;
        }
        self.notice = None;
        let at = Position::new(mouse.column, mouse.row);
        let row = self.rows.iter().find(|(rect, _)| rect.contains(at));
        match row.map(|&(_, row)| row) {
            Some(Row::Run) => self.reopen(),
            Some(Row::Here) => self.open_here(),
            Some(Row::Recent(index)) => self.open_recent(index),
            None => Step::Stay,
        }
    }

    /// What `key` does: with the picker open, what [`Screen::picker_key`]
    /// says; else `Enter` reopens the last run, `.` opens the current
    /// directory, `1` to `9` the recent project on the row of that digit,
    /// `o` opens the picker, and `q` or `Ctrl+C` quit. `.`, the digits and
    /// `o` take Shift too, as the picker takes typed characters, for the
    /// layouts that need it for them. Any other key does nothing.
    fn key(&mut self, key: &KeyEvent) -> Step {
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        let plain = key.modifiers.is_empty();
        let typed = plain || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Enter if plain => self.reopen(),
            KeyCode::Char('.') if typed => self.open_here(),
            KeyCode::Char('o') if typed => {
                self.picker = Some(self.offer.list.clone());
                Step::Stay
            }
            KeyCode::Char('q') if plain => Step::Quit,
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => Step::Quit,
            KeyCode::Char(digit) if typed => {
                recent_index(digit).map_or(Step::Stay, |index| self.open_recent(index))
            }
            _ => Step::Stay,
        }
    }

    /// What `key` does with the picker open: the termis-mode key, or
    /// `Esc`, closes it, back to the screen; `Enter` opens its choice,
    /// continuing its last conversation, and `Tab` on a new one; the other
    /// keys edit the query, choose a row or pick an account, as
    /// [`picker_key`] says.
    fn picker_key(&mut self, key: &KeyEvent) -> Step {
        if self.offer.mode_key.matches(key) {
            self.picker = None;
            return Step::Stay;
        }
        let action = picker_key(key);
        let Some(picker) = &mut self.picker else {
            return Step::Stay;
        };
        match action {
            Action::Mode(_) => self.picker = None,
            Action::Type(text) => picker.insert(&text),
            Action::Erase => picker.backspace(),
            Action::Choose(step) => picker.step(step),
            Action::StepAccount(step) => picker.step_account(step),
            Action::Open { new_conversation } => return self.open_picked(new_conversation),
            _ => {}
        }
        Step::Stay
    }

    /// Reopens the last run, showing the current directory when the run
    /// has it, else the run's first directory; without a run, as `.`
    /// does.
    fn reopen(&mut self) -> Step {
        match run_target(&self.offer.run, &self.cwd) {
            Some(dir) => self.open(dir, None, false),
            None => self.open_here(),
        }
    }

    /// Opens the current directory, continuing its last conversation.
    fn open_here(&mut self) -> Step {
        self.open(self.offer.cwd.to_path_buf(), None, false)
    }

    /// Opens the recent project at `index` among the picker's rows, as
    /// the account its row names, continuing its last conversation; none
    /// unless the last frame drew its row.
    fn open_recent(&mut self, index: usize) -> Step {
        let drawn = self.rows.iter().any(|&(_, row)| row == Row::Recent(index));
        let Some(item) = self.offer.list.rows().nth(index).filter(|_| drawn) else {
            return Step::Stay;
        };
        let account = item.account.clone();
        let target = open::target(Choice::Item(item), &Sessions::new(), self.offer.home);
        self.open_target(target, account.as_deref(), false)
    }

    /// Opens the picker's choice as the account it gives, on a new
    /// conversation when `new_conversation`; the picker stays while it
    /// cannot.
    fn open_picked(&mut self, new_conversation: bool) -> Step {
        let Some(picker) = &self.picker else {
            return Step::Stay;
        };
        let Some(choice) = picker.choice() else {
            return Step::Stay;
        };
        let account = picker.account().map(str::to_owned);
        let target = open::target(choice, &Sessions::new(), self.offer.home);
        self.open_target(target, account.as_deref(), new_conversation)
    }

    /// Opens the directory of `target` as [`Screen::open`] does, or stays
    /// with why it cannot.
    fn open_target(
        &mut self,
        target: Result<Target>,
        account: Option<&str>,
        new_conversation: bool,
    ) -> Step {
        match target {
            Ok(Target::Dir(dir)) => self.open(dir, account, new_conversation),
            // No session is open before the screen's choice to be shown.
            Ok(Target::Session(_)) => Step::Stay,
            Err(error) => self.refuse(format!("{error:#}")),
        }
    }

    /// Opens a session in `dir`, as the account named `picked`, else the
    /// one the directory gets, on a new conversation when
    /// `new_conversation`, in the repository around it; or stays, saying
    /// for its project why it cannot: the account is gone, or git failed.
    fn open(&mut self, dir: PathBuf, picked: Option<&str>, new_conversation: bool) -> Step {
        let offer = &self.offer;
        let project = session::project_name(&dir);
        let account = chosen_account(
            picked,
            &dir,
            offer.accounts,
            offer.remembered,
            offer.last_ran,
        );
        let account = match account {
            Ok(account) => account,
            Err(why) => return self.refuse(strings::for_project(&project, &why)),
        };
        // The current directory's repository was found before the screen.
        let repo = if dir == self.offer.cwd || dir == self.cwd {
            Ok(self.offer.repo.clone())
        } else {
            git::repo_root(&dir)
        };
        match repo {
            Ok(repo) => Step::Open(Pick {
                dir,
                repo,
                account,
                new_conversation,
            }),
            Err(error) => self.refuse(strings::for_project(&project, &format!("{error:#}"))),
        }
    }

    /// Stays, with `text` in the bar until the next key or click.
    fn refuse(&mut self, text: String) -> Step {
        self.notice = Some(trouble(text));
        Step::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agents;
    use crate::app::session::tests::Fake;
    use crate::app::{FOCUS_SETTLES, config};
    use crate::git::tests::init;
    use crate::ui::picker::Item;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use tempfile::TempDir;

    /// The projects of the home, by name.
    const PROJECTS: [&str; 5] = ["termis", "dictamen", "api", "notes", "web"];

    /// A home with the [`PROJECTS`], `termis` a repository; two accounts,
    /// personal, the agent's own, and work; `notes` remembered as an
    /// account gone, and `api` last run as work.
    struct Home {
        _temporary: TempDir,
        home: PathBuf,
        dirs: Vec<PathBuf>,
        accounts: Accounts,
        remembered: Remembered,
        last_ran: HashMap<PathBuf, String>,
    }

    impl Home {
        fn new() -> Self {
            let temporary = TempDir::new().expect("temp dir");
            let home = temporary.path().canonicalize().expect("exists");
            let dirs: Vec<PathBuf> = PROJECTS.iter().map(|name| home.join(name)).collect();
            for dir in &dirs {
                fs::create_dir(dir).expect("mkdir");
            }
            init(&home.join("termis"));
            let config = config::parse(
                "[[accounts]]\nname = \"personal\"\n[[accounts]]\nname = \"work\"\ndir = \"/w\"\n",
            )
            .expect("parses");
            let accounts = Accounts::new(&config.accounts, None, Agents::new(&Fake(""), &Fake("")))
                .expect("two");
            let file = home.join("accounts.json");
            let mut remembered = Remembered::open(Some(file), None, true).expect("none yet");
            assert_eq!(remembered.remember(&home.join("notes"), "old"), None);
            let last_ran = HashMap::from([(home.join("api"), "work".to_owned())]);
            Self {
                _temporary: temporary,
                home,
                dirs,
                accounts,
                remembered,
                last_ran,
            }
        }

        /// The directory of the project `name`.
        fn dir(&self, name: &str) -> &Path {
            let at = PROJECTS.iter().position(|project| *project == name);
            &self.dirs[at.expect("a project")]
        }

        /// The picker's rows: `termis` and `api` as the accounts they
        /// get, `dictamen` as work, which its row names, and `notes` as
        /// the account gone.
        fn list(&self) -> Picker {
            let item = |name: &str, account: &str| {
                Item::recent(name.into(), self.dir(name).to_path_buf(), Some(&self.home))
                    .with_account(Some(account.into()))
            };
            let rows = vec![
                item("termis", "personal"),
                item("api", "work"),
                item("dictamen", "work"),
                item("notes", "old"),
            ];
            Picker::new(vec![], rows).with_accounts(vec!["personal".into(), "work".into()])
        }

        /// The start screen in `cwd` after the last `run`, drawn once at
        /// 100 by 30.
        fn screen(&self, cwd: &str, run: &[&str]) -> Screen<'_> {
            let cwd = self.dir(cwd);
            let offer = Offer {
                cwd,
                repo: git::repo_root(cwd).expect("git"),
                run: run
                    .iter()
                    .map(|name| self.dir(name).to_path_buf())
                    .collect(),
                list: self.list(),
                home: Some(&self.home),
                accounts: &self.accounts,
                remembered: &self.remembered,
                last_ran: &self.last_ran,
                palette: Palette::DEFAULT,
                mode_key: ModeKey::DEFAULT,
            };
            let mut screen = Screen::new(offer);
            draw(&mut screen, 100, 30);
            screen
        }
    }

    /// Draws `screen` on a terminal of `width` by `height`, as the loop
    /// does before each event, and returns the terminal.
    fn draw(screen: &mut Screen<'_>, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let view = screen.view();
        let mut rows = Vec::new();
        terminal
            .draw(|frame| rows = screen::render(frame, &view))
            .expect("draw");
        screen.rows = rows;
        terminal
    }

    impl Screen<'_> {
        /// What `event` does now.
        fn send(&mut self, event: Event) -> Step {
            self.on_event(event, Instant::now())
        }
    }

    /// A press of `code` with `modifiers`.
    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    /// A press of `code` alone.
    fn press(code: KeyCode) -> Event {
        key(code, KeyModifiers::NONE)
    }

    /// A press of the key that types `ch`.
    fn typed(ch: char) -> Event {
        press(KeyCode::Char(ch))
    }

    /// Where `step` opens: the directory, its repository, the account and
    /// whether on a new conversation.
    fn opened(step: Step) -> (PathBuf, Option<PathBuf>, String, bool) {
        match step {
            Step::Open(pick) => (
                pick.dir,
                pick.repo,
                pick.account.name,
                pick.new_conversation,
            ),
            other => panic!("opens nothing: {other:?}"),
        }
    }

    /// The text of the notice `screen` shows, if any.
    fn notice<'a>(screen: &'a Screen<'_>) -> Option<&'a str> {
        screen.notice.as_ref().map(|toast| toast.text.as_str())
    }

    #[test]
    fn enter_reopens_the_run_showing_the_current_directory_when_it_is_in_it() {
        let home = Home::new();
        let run = ["termis", "dictamen", "api"];
        let mut screen = home.screen("dictamen", &run);
        let dictamen = home.dir("dictamen").to_path_buf();
        let step = screen.send(press(KeyCode::Enter));
        assert_eq!(opened(step), (dictamen, None, "personal".into(), false));
        // The current directory's repository, found before the screen.
        let mut screen = home.screen("termis", &run);
        let termis = home.dir("termis").to_path_buf();
        let step = screen.send(press(KeyCode::Enter));
        assert_eq!(
            opened(step),
            (termis.clone(), Some(termis), "personal".into(), false)
        );
    }

    #[test]
    fn enter_shows_the_first_run_directory_whose_account_resolves_otherwise() {
        let home = Home::new();
        let api = home.dir("api").to_path_buf();
        let expected = (api, None, "work".into(), false);
        // Outside the run, and in it as an account gone.
        for cwd in ["web", "notes"] {
            let mut screen = home.screen(cwd, &["notes", "api", "termis"]);
            let step = screen.send(press(KeyCode::Enter));
            assert_eq!(opened(step), expected, "{cwd}");
        }
    }

    #[test]
    fn the_enter_row_counts_and_names_only_the_directories_that_reopen() {
        let home = Home::new();
        // `notes` has no account to run as, so it does not reopen.
        let mut screen = home.screen("web", &["notes", "termis"]);
        assert_eq!(screen.names, ["termis"]);
        let terminal = draw(&mut screen, 100, 30);
        let found = screen.rows.iter().find(|&&(_, row)| row == Row::Run);
        let (rect, _) = found.expect("drawn");
        let buffer = terminal.backend().buffer();
        let text: String = (rect.left()..rect.right())
            .map(|x| buffer[(x, rect.y)].symbol())
            .collect();
        assert_eq!(text.trim_end(), "Enter  reopen 1 session  termis");
        // With none that reopens, the row of `Enter` opens the current
        // directory and the row of `.` goes.
        let screen = home.screen("web", &["notes"]);
        assert!(screen.names.is_empty());
        let rows: Vec<Row> = screen.rows.iter().map(|&(_, row)| row).collect();
        assert_eq!(rows[..2], [Row::Here, Row::Recent(0)]);
    }

    #[test]
    fn enter_without_a_run_opens_the_current_directory() {
        let home = Home::new();
        let web = home.dir("web").to_path_buf();
        // No run, or none whose account resolves.
        for run in [&[][..], &["notes"]] {
            let mut screen = home.screen("web", run);
            let step = screen.send(press(KeyCode::Enter));
            assert_eq!(
                opened(step),
                (web.clone(), None, "personal".into(), false),
                "{run:?}"
            );
        }
    }

    #[test]
    fn dot_opens_the_current_directory_continuing_its_conversation() {
        let home = Home::new();
        let mut screen = home.screen("termis", &["dictamen"]);
        let termis = home.dir("termis").to_path_buf();
        let step = screen.send(typed('.'));
        assert_eq!(
            opened(step),
            (termis.clone(), Some(termis), "personal".into(), false)
        );
    }

    #[test]
    fn a_digit_opens_its_row_as_the_account_the_row_names() {
        let home = Home::new();
        for (digit, name, account) in [
            ('1', "termis", "personal"),
            ('2', "api", "work"),
            ('3', "dictamen", "work"),
        ] {
            let mut screen = home.screen("web", &["termis"]);
            let (dir, _, opened_as, new) = opened(screen.send(typed(digit)));
            assert_eq!(
                (dir.as_path(), opened_as.as_str(), new),
                (home.dir(name), account, false)
            );
        }
        let mut screen = home.screen("web", &[]);
        let (_, repo, _, _) = opened(screen.send(typed('1')));
        assert_eq!(repo.as_deref(), Some(home.dir("termis")), "git ran for it");
    }

    #[test]
    fn a_digit_past_the_rows_drawn_does_nothing() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(typed('5')), Step::Stay);
        assert_eq!(notice(&screen), None);
        // A short terminal draws two of the four rows.
        draw(&mut screen, 100, 9);
        let drawn = |row| screen.rows.iter().any(|&(_, drawn)| drawn == row);
        assert!(drawn(Row::Recent(1)) && !drawn(Row::Recent(2)));
        assert_eq!(screen.send(typed('3')), Step::Stay);
        assert!(matches!(screen.send(typed('2')), Step::Open(_)));
    }

    #[test]
    fn a_row_whose_account_is_gone_stays_with_a_notice() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(typed('4')), Step::Stay);
        assert_eq!(
            notice(&screen),
            Some("notes: opened last as account old, which the config file does not declare")
        );
    }

    #[test]
    fn the_current_directory_whose_account_is_gone_stays_with_a_notice() {
        let home = Home::new();
        let gone = "notes: opened last as account old, which the config file does not declare";
        for (run, event) in [(&["termis"][..], typed('.')), (&[], press(KeyCode::Enter))] {
            let mut screen = home.screen("notes", run);
            assert_eq!(screen.send(event), Step::Stay);
            assert_eq!(notice(&screen), Some(gone), "{run:?}");
        }
    }

    #[test]
    fn a_notice_goes_at_the_next_key() {
        let home = Home::new();
        let mut screen = home.screen("notes", &[]);
        assert_eq!(screen.send(typed('.')), Step::Stay);
        assert!(notice(&screen).is_some());
        // A resize, the release of a key or a paste keep it.
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('.'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        for event in [
            Event::Resize(80, 24),
            Event::Key(release),
            Event::Paste("x".into()),
        ] {
            assert_eq!(screen.send(event), Step::Stay);
            assert!(notice(&screen).is_some());
        }
        assert_eq!(screen.send(typed('x')), Step::Stay);
        assert_eq!(notice(&screen), None);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(typed('q')), Step::Quit);
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(screen.send(ctrl_c), Step::Quit);
        let shifted = key(KeyCode::Char('Q'), KeyModifiers::SHIFT);
        assert_eq!(screen.send(shifted), Step::Stay);
    }

    #[test]
    fn esc_the_mode_key_alt_digits_paste_and_releases_do_nothing() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        let release =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('1'), KeyModifiers::NONE, KeyEventKind::Repeat);
        for event in [
            press(KeyCode::Esc),
            key(KeyCode::Char('?'), KeyModifiers::SHIFT),
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
            key(KeyCode::Char('1'), KeyModifiers::ALT),
            typed('0'),
            typed('x'),
            press(KeyCode::Tab),
            Event::FocusGained,
            Event::FocusLost,
            Event::Paste("termis".into()),
            Event::Key(release),
            Event::Key(repeat),
        ] {
            assert_eq!(screen.send(event.clone()), Step::Stay, "{event:?}");
            assert!(
                screen.picker.is_none() && screen.notice.is_none(),
                "{event:?}"
            );
        }
    }

    #[test]
    fn digits_and_the_dot_with_shift_count() {
        let home = Home::new();
        let shift = |ch| key(KeyCode::Char(ch), KeyModifiers::SHIFT);
        let mut screen = home.screen("web", &["termis"]);
        let (dir, ..) = opened(screen.send(shift('1')));
        assert_eq!(dir, home.dir("termis"));
        let (dir, ..) = opened(screen.send(shift('.')));
        assert_eq!(dir, home.dir("web"));
        assert_eq!(screen.send(shift('o')), Step::Stay);
        assert!(screen.picker.is_some());
    }

    #[test]
    fn o_opens_the_picker_with_the_screens_rows() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(typed('o')), Step::Stay);
        let picker = screen.picker.as_ref().expect("open");
        assert_eq!(picker, &screen.offer.list);
        let names: Vec<&str> = picker.rows().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["termis", "api", "dictamen", "notes"]);
        // Its keys are the picker's: `q` is typed.
        assert_eq!(screen.send(typed('q')), Step::Stay);
        assert_eq!(screen.picker.as_ref().map(Picker::query), Some("q"));
    }

    #[test]
    fn esc_or_the_mode_key_closes_the_picker_and_drops_the_query() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        let mode_key = key(KeyCode::Char('q'), KeyModifiers::CONTROL);
        for close in [press(KeyCode::Esc), mode_key] {
            screen.send(typed('o'));
            screen.send(typed('a'));
            screen.send(typed('p'));
            assert_eq!(screen.picker.as_ref().map(Picker::query), Some("ap"));
            assert_eq!(screen.send(close), Step::Stay);
            assert!(screen.picker.is_none());
            screen.send(typed('o'));
            assert_eq!(screen.picker.as_ref().map(Picker::query), Some(""));
            screen.send(press(KeyCode::Esc));
        }
    }

    #[test]
    fn enter_in_the_picker_continues_and_tab_starts_a_new_conversation() {
        let home = Home::new();
        let api = home.dir("api").to_path_buf();
        for (open, new) in [(KeyCode::Enter, false), (KeyCode::Tab, true)] {
            let mut screen = home.screen("web", &["termis"]);
            screen.send(typed('o'));
            screen.send(press(KeyCode::Down));
            let step = screen.send(press(open));
            assert_eq!(opened(step), (api.clone(), None, "work".into(), new));
        }
    }

    #[test]
    fn the_account_picked_in_the_picker_goes_with_the_choice() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        screen.send(typed('o'));
        screen.send(press(KeyCode::Right));
        let (dir, _, account, _) = opened(screen.send(press(KeyCode::Enter)));
        assert_eq!(
            (dir.as_path(), account.as_str()),
            (home.dir("termis"), "work")
        );
        // A row whose account is gone opens once another is picked.
        let mut screen = home.screen("web", &["termis"]);
        screen.send(typed('o'));
        for ch in "notes".chars() {
            screen.send(typed(ch));
        }
        assert_eq!(screen.send(press(KeyCode::Enter)), Step::Stay);
        assert!(notice(&screen).is_some_and(|text| text.starts_with("notes: opened last")));
        screen.send(press(KeyCode::Left));
        let (dir, _, account, _) = opened(screen.send(press(KeyCode::Enter)));
        assert_eq!(
            (dir.as_path(), account.as_str()),
            (home.dir("notes"), "personal")
        );
    }

    #[test]
    fn a_typed_path_that_is_not_a_directory_keeps_the_picker_with_a_notice() {
        let home = Home::new();
        fs::write(home.home.join("file"), "").expect("write");
        let mut screen = home.screen("web", &["termis"]);
        screen.send(typed('o'));
        for ch in "~/file".chars() {
            screen.send(typed(ch));
        }
        assert_eq!(screen.send(press(KeyCode::Enter)), Step::Stay);
        assert_eq!(notice(&screen), Some("~/file is not a directory"));
        assert_eq!(screen.picker.as_ref().map(Picker::query), Some("~/file"));
        // A typed directory counts from the home.
        for _ in "file".chars() {
            screen.send(press(KeyCode::Backspace));
        }
        for ch in "web".chars() {
            screen.send(typed(ch));
        }
        let (dir, ..) = opened(screen.send(press(KeyCode::Enter)));
        assert_eq!(dir, home.dir("web"));
    }

    #[test]
    fn a_paste_in_the_picker_types_its_first_line() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(Event::Paste("api".into())), Step::Stay);
        assert!(screen.picker.is_none(), "not on the screen");
        screen.send(typed('o'));
        screen.send(Event::Paste("dict\namen".into()));
        screen.send(Event::Paste("\nx".into()));
        assert_eq!(screen.picker.as_ref().map(Picker::query), Some("dict"));
        let (dir, ..) = opened(screen.send(press(KeyCode::Enter)));
        assert_eq!(dir, home.dir("dictamen"));
    }

    /// A mouse event of `kind` at `at`.
    fn mouse(kind: MouseEventKind, at: Position) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: at.x,
            row: at.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A press of the left button at `at`.
    fn click(at: Position) -> Event {
        mouse(MouseEventKind::Down(MouseButton::Left), at)
    }

    /// A cell of the row the last frame drew for `row`, past its key.
    fn on(screen: &Screen<'_>, row: Row) -> Position {
        let found = screen.rows.iter().find(|&&(_, drawn)| drawn == row);
        let (rect, _) = found.expect("drawn");
        Position::new(rect.x + 10, rect.y)
    }

    #[test]
    fn a_click_on_a_row_does_what_its_key_does() {
        let home = Home::new();
        for (row, name, account) in [
            (Row::Run, "termis", "personal"),
            (Row::Here, "web", "personal"),
            (Row::Recent(1), "api", "work"),
        ] {
            let mut screen = home.screen("web", &["termis"]);
            let at = on(&screen, row);
            let (dir, _, opened_as, _) = opened(screen.send(click(at)));
            assert_eq!(
                (dir.as_path(), opened_as.as_str()),
                (home.dir(name), account),
                "{row:?}"
            );
        }
        // A click that opens nothing shows why, until the next click.
        let mut screen = home.screen("web", &["termis"]);
        assert_eq!(screen.send(click(on(&screen, Row::Recent(3)))), Step::Stay);
        assert!(notice(&screen).is_some_and(|text| text.starts_with("notes: ")));
        assert_eq!(screen.send(click(Position::new(0, 0))), Step::Stay);
        assert_eq!(notice(&screen), None);
    }

    #[test]
    fn the_click_that_brings_the_window_forward_does_nothing() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        let at = on(&screen, Row::Here);
        let t0 = Instant::now();
        assert_eq!(screen.on_event(Event::FocusGained, t0), Step::Stay);
        let soon = t0 + FOCUS_SETTLES / 2;
        assert_eq!(screen.on_event(click(at), soon), Step::Stay);
        // Without the focus, a click does nothing either.
        assert_eq!(screen.on_event(Event::FocusLost, soon), Step::Stay);
        let settled = t0 + FOCUS_SETTLES;
        assert_eq!(screen.on_event(click(at), settled), Step::Stay);
        assert_eq!(screen.on_event(Event::FocusGained, t0), Step::Stay);
        let (dir, ..) = opened(screen.on_event(click(at), settled));
        assert_eq!(dir, home.dir("web"));
    }

    #[test]
    fn the_wheel_drags_other_buttons_and_clicks_off_the_rows_do_nothing() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        let at = on(&screen, Row::Run);
        for kind in [
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
        ] {
            assert_eq!(screen.send(mouse(kind, at)), Step::Stay, "{kind:?}");
        }
        // Left of the column, on the logo and on the bar.
        for off in [
            Position::new(5, at.y),
            Position::new(40, 8),
            Position::new(5, 29),
        ] {
            assert_eq!(screen.send(click(off)), Step::Stay, "{off:?}");
        }
    }

    #[test]
    fn clicks_with_the_picker_open_do_nothing() {
        let home = Home::new();
        let mut screen = home.screen("web", &["termis"]);
        let at = on(&screen, Row::Run);
        screen.send(typed('o'));
        assert_eq!(screen.send(click(at)), Step::Stay);
        assert!(screen.picker.is_some());
    }
}
