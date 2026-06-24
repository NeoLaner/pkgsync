//! Application state and the logic that mutates it.
//!
//! This module knows nothing about rendering or threads — it's pure state +
//! behavior, so the whole keymap and screen flow is unit-testable with no
//! terminal (see the tests at the bottom). `main.rs` reads this state to draw,
//! forwards key presses into `handle_key`, performs the actual fetching on a
//! background thread, and hands results back via `on_fetch_result`.

use crate::diff::{Category, DiffEntry};
use crate::source::SourceSpec;
use ratatui::{crossterm::event::KeyCode, widgets::ListState};
use std::collections::HashSet;
use std::path::PathBuf;

/// Spinner animation frames for the loading screen.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Which category of diff entries is currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Install,
    Upgrade,
    Remove,
}

impl Filter {
    fn matches(self, cat: Category) -> bool {
        match self {
            Filter::All => true,
            Filter::Install => cat == Category::Install,
            Filter::Upgrade => cat == Category::Upgrade,
            Filter::Remove => cat == Category::Remove,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Install => "install",
            Filter::Upgrade => "upgrade",
            Filter::Remove => "remove",
        }
    }
}

/// Sub-mode of the diff screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Confirm,
}

/// Which kind of source the user is currently typing the details for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Ssh,
    File,
}

/// An entry in the source menu: a remembered source to pick directly, or a
/// prompt to enter a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItem {
    Source(SourceSpec),
    NewSsh,
    NewFile,
}

/// The top-level screen the app is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Entry menu: choose a source *type* (SSH or local file).
    Menu,
    /// Typing the details (host/IP or file path) for the chosen type.
    Input,
    /// Choosing from a list of pre-supplied sources (CLI-args flow).
    Picker,
    /// A fetch is in flight (data is being gathered on a worker thread).
    Loading,
    /// Showing the diff (interaction governed by `Mode`).
    Diff,
    /// A fetch failed; the string is the message to show.
    Error(String),
}

/// All the mutable state of the running app.
pub struct App {
    // --- entry menu / input ---
    /// The menu rows: remembered sources, then "enter new" prompts.
    pub menu_items: Vec<MenuItem>,
    /// Cursor in the source menu.
    pub menu_state: ListState,
    /// The text the user is typing (host/IP or file path).
    pub input: String,
    /// What the current input is for.
    pub input_kind: InputKind,

    // --- picker ---
    /// Candidate sources to choose from.
    pub specs: Vec<SourceSpec>,
    /// Cursor in the picker list.
    pub picker_state: ListState,

    // --- diff ---
    /// The current diff, replaced on each successful fetch.
    pub entries: Vec<DiffEntry>,
    pub filter: Filter,
    /// Ticked package names (stable across filtering/refetch within a view).
    pub selected: HashSet<String>,
    pub list_state: ListState,
    pub mode: Mode,

    // --- control ---
    pub screen: Screen,
    /// The source currently being shown, so `refresh` knows what to re-fetch.
    current_spec: Option<SourceSpec>,
    /// Spinner animation counter.
    spinner: usize,
    pub should_quit: bool,
    /// One-shot: the user confirmed an apply.
    apply_requested: bool,
    /// One-shot: a fetch the main loop should start on a worker thread.
    pending_fetch: Option<SourceSpec>,
    /// Incremented on every fetch request/cancel. A worker's result is only
    /// applied if it still matches — so superseded or cancelled fetches are
    /// ignored when they finally land.
    fetch_epoch: u64,
}

impl App {
    /// Start at the picker. If there's exactly one candidate, fetch it
    /// immediately so `pkgsync office` feels like a one-shot command.
    pub fn new(specs: Vec<SourceSpec>) -> Self {
        let mut picker_state = ListState::default();
        if !specs.is_empty() {
            picker_state.select(Some(0));
        }
        let mut app = Self {
            menu_items: Vec::new(),
            menu_state: ListState::default(),
            input: String::new(),
            input_kind: InputKind::Ssh,
            specs,
            picker_state,
            entries: Vec::new(),
            filter: Filter::All,
            selected: HashSet::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            screen: Screen::Picker,
            current_spec: None,
            spinner: 0,
            should_quit: false,
            apply_requested: false,
            pending_fetch: None,
            fetch_epoch: 0,
        };
        if app.specs.len() == 1 {
            app.request_fetch(app.specs[0].clone());
        }
        app
    }

    /// Construct directly in the diff screen with given data (used by `demo`).
    pub fn demo(entries: Vec<DiffEntry>) -> Self {
        let mut app = Self::new(Vec::new());
        app.set_diff(entries);
        app
    }

    /// Start at the interactive entry menu, pre-populated with `known` sources
    /// (recents + ssh_config hosts) followed by the "enter new" prompts.
    pub fn interactive(known: Vec<SourceSpec>) -> Self {
        let mut app = Self::new(Vec::new());
        let mut items: Vec<MenuItem> = known.into_iter().map(MenuItem::Source).collect();
        items.push(MenuItem::NewSsh);
        items.push(MenuItem::NewFile);
        app.menu_items = items;
        app.menu_state.select(Some(0));
        app.screen = Screen::Menu;
        app
    }

    // --- fetch plumbing -----------------------------------------------------

    /// Mark a fetch to be started by the main loop, and show the spinner.
    fn request_fetch(&mut self, spec: SourceSpec) {
        self.fetch_epoch = self.fetch_epoch.wrapping_add(1); // invalidate older fetches
        self.current_spec = Some(spec.clone());
        self.pending_fetch = Some(spec);
        self.spinner = 0;
        self.screen = Screen::Loading;
    }

    /// Re-fetch the currently displayed source (e.g. after applying changes).
    /// No-op if there's no current source (like demo mode).
    pub fn refresh(&mut self) {
        if let Some(spec) = self.current_spec.clone() {
            self.request_fetch(spec);
        }
    }

    /// Consume a pending fetch, if any (the main loop spawns the worker).
    pub fn take_fetch_request(&mut self) -> Option<SourceSpec> {
        self.pending_fetch.take()
    }

    /// The epoch a freshly-spawned worker should be tagged with.
    pub fn current_epoch(&self) -> u64 {
        self.fetch_epoch
    }

    /// Receive a worker thread's result, ignoring it if its fetch was
    /// superseded by a newer request or cancelled.
    pub fn on_fetch_result(&mut self, epoch: u64, result: Result<Vec<DiffEntry>, String>) {
        if epoch != self.fetch_epoch {
            return;
        }
        match result {
            Ok(entries) => self.set_diff(entries),
            Err(message) => self.screen = Screen::Error(message),
        }
    }

    /// Abandon the in-flight fetch (bumping the epoch so its result is ignored)
    /// and return to the entry screen.
    fn cancel_fetch(&mut self) {
        self.fetch_epoch = self.fetch_epoch.wrapping_add(1);
        self.screen = self.entry_screen();
    }

    /// Where "back" leads: the typed menu (interactive) or the picker (CLI args).
    fn entry_screen(&self) -> Screen {
        if self.specs.is_empty() {
            Screen::Menu
        } else {
            Screen::Picker
        }
    }

    fn set_diff(&mut self, entries: Vec<DiffEntry>) {
        self.entries = entries;
        self.selected.clear();
        self.filter = Filter::All;
        self.mode = Mode::Browse;
        self.list_state = ListState::default();
        if !self.entries.is_empty() {
            self.list_state.select(Some(0));
        }
        self.screen = Screen::Diff;
    }

    /// Advance the spinner (called by the loop while idle/loading).
    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    pub fn spinner_frame(&self) -> char {
        SPINNER[self.spinner % SPINNER.len()]
    }

    pub fn loading_label(&self) -> String {
        self.current_spec
            .as_ref()
            .map_or_else(|| "…".to_string(), SourceSpec::label)
    }

    // --- diff view helpers --------------------------------------------------

    pub fn visible(&self) -> Vec<&DiffEntry> {
        self.entries
            .iter()
            .filter(|e| self.filter.matches(e.kind.category()))
            .collect()
    }

    fn visible_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| self.filter.matches(e.kind.category()))
            .count()
    }

    pub fn selected_entry(&self) -> Option<&DiffEntry> {
        let index = self.list_state.selected()?;
        self.visible().into_iter().nth(index)
    }

    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.contains(name)
    }

    pub fn move_down(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            _ => 0,
        };
        self.list_state.select(Some(next));
    }

    pub fn move_up(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let prev = match self.list_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.list_state.select(Some(prev));
    }

    pub fn toggle_current(&mut self) {
        let Some(name) = self.selected_entry().map(|e| e.name.clone()) else {
            return;
        };
        if !self.selected.remove(&name) {
            self.selected.insert(name);
        }
    }

    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
        } else {
            let clamped = self.list_state.selected().unwrap_or(0).min(len - 1);
            self.list_state.select(Some(clamped));
        }
    }

    // --- picker helpers -----------------------------------------------------

    /// Move the picker cursor by `delta` (+1 down, -1 up), wrapping.
    fn picker_move(&mut self, delta: isize) {
        let len = self.specs.len();
        if len == 0 {
            return;
        }
        let current = self.picker_state.selected().unwrap_or(0) as isize;
        // rem_euclid keeps the result in 0..len even for negative values.
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.picker_state.select(Some(next));
    }

    fn picker_select(&mut self) {
        if let Some(index) = self.picker_state.selected()
            && let Some(spec) = self.specs.get(index).cloned()
        {
            self.request_fetch(spec);
        }
    }

    // --- input --------------------------------------------------------------

    /// Map a key press to a state change, dispatched by the current screen.
    pub fn handle_key(&mut self, code: KeyCode) {
        match self.screen {
            Screen::Menu => self.handle_menu_key(code),
            Screen::Input => self.handle_input_key(code),
            Screen::Picker => self.handle_picker_key(code),
            Screen::Loading => match code {
                KeyCode::Char('q') => self.should_quit = true,
                // The worker thread keeps running, but its result is ignored.
                KeyCode::Esc => self.cancel_fetch(),
                _ => {}
            },
            Screen::Diff => match self.mode {
                Mode::Browse => self.handle_browse_key(code),
                Mode::Confirm => self.handle_confirm_key(code),
            },
            Screen::Error(_) => match code {
                KeyCode::Char('q') => self.should_quit = true,
                _ => self.screen = self.entry_screen(), // any other key -> back
            },
        }
    }

    fn handle_menu_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.menu_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.menu_move(-1),
            KeyCode::Enter => {
                let index = self.menu_state.selected().unwrap_or(0);
                match self.menu_items.get(index).cloned() {
                    Some(MenuItem::Source(spec)) => self.request_fetch(spec),
                    Some(MenuItem::NewSsh) => self.start_input(InputKind::Ssh),
                    Some(MenuItem::NewFile) => self.start_input(InputKind::File),
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// Move the menu cursor, wrapping.
    fn menu_move(&mut self, delta: isize) {
        let len = self.menu_items.len();
        if len == 0 {
            return;
        }
        let current = self.menu_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.menu_state.select(Some(next));
    }

    fn start_input(&mut self, kind: InputKind) {
        self.input_kind = kind;
        self.input.clear();
        self.screen = Screen::Input;
    }

    fn handle_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.screen = Screen::Menu, // back to the type menu
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => self.submit_input(),
            // Any printable character is appended to the buffer.
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    /// Turn the typed text into a `SourceSpec` and kick off a fetch. Empty input
    /// is ignored so Enter on a blank field does nothing.
    fn submit_input(&mut self) {
        let text = self.input.trim();
        if text.is_empty() {
            return;
        }
        let spec = match self.input_kind {
            InputKind::Ssh => SourceSpec::Ssh(text.to_string()),
            InputKind::File => SourceSpec::File(expand_tilde(text)),
        };
        self.request_fetch(spec);
    }

    fn handle_picker_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.picker_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.picker_move(-1),
            KeyCode::Enter => self.picker_select(),
            _ => {}
        }
    }

    fn handle_browse_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = self.entry_screen(), // back to source choice
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Tab | KeyCode::Char(' ') => self.toggle_current(),
            KeyCode::Char('a') => self.set_filter(Filter::All),
            KeyCode::Char('i') => self.set_filter(Filter::Install),
            KeyCode::Char('u') => self.set_filter(Filter::Upgrade),
            KeyCode::Char('r') => self.set_filter(Filter::Remove),
            KeyCode::Char('R') | KeyCode::F(5) => self.refresh(),
            KeyCode::Enter if !self.selected.is_empty() => self.mode = Mode::Confirm,
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.apply_requested = true;
                self.mode = Mode::Browse;
            }
            KeyCode::Char('n') | KeyCode::Esc => self.mode = Mode::Browse,
            _ => {}
        }
    }

    /// Consume the one-shot "apply" signal (fires true exactly once).
    pub fn take_apply_request(&mut self) -> bool {
        std::mem::take(&mut self.apply_requested)
    }
}

/// Expand a leading `~/` to the user's home directory; otherwise pass through.
/// `std::fs` does not understand `~`, so we do it ourselves for typed paths.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffKind;

    fn sample() -> Vec<DiffEntry> {
        vec![
            DiffEntry {
                name: "btop".into(),
                kind: DiffKind::Missing { remote_version: "1.4".into() },
            },
            DiffEntry {
                name: "discord".into(),
                kind: DiffKind::Extra { local_version: "0.1".into() },
            },
            DiffEntry {
                name: "hyprland".into(),
                kind: DiffKind::VersionSkew {
                    local_version: "0.45".into(),
                    remote_version: "0.46".into(),
                },
            },
        ]
    }

    fn specs() -> Vec<SourceSpec> {
        vec![
            SourceSpec::Ssh("office".into()),
            SourceSpec::Ssh("laptop".into()),
        ]
    }

    // --- diff-view behavior (now constructed via `demo`) ---

    #[test]
    fn starts_on_first_entry() {
        let app = App::demo(sample());
        assert_eq!(app.screen, Screen::Diff);
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn empty_diff_has_no_selection() {
        let app = App::demo(vec![]);
        assert!(app.selected_entry().is_none());
    }

    #[test]
    fn movement_wraps_both_ways() {
        let mut app = App::demo(sample());
        app.move_up();
        assert_eq!(app.selected_entry().unwrap().name, "hyprland");
        app.move_down();
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn toggle_adds_then_removes() {
        let mut app = App::demo(sample());
        app.toggle_current();
        assert!(app.is_selected("btop"));
        app.toggle_current();
        assert!(!app.is_selected("btop"));
    }

    #[test]
    fn filter_limits_visible_and_clamps_cursor() {
        let mut app = App::demo(sample());
        app.move_down();
        app.move_down();
        app.set_filter(Filter::Install);
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn confirm_flow_raises_apply_request_once() {
        let mut app = App::demo(sample());
        app.toggle_current();
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.mode, Mode::Confirm);
        app.handle_key(KeyCode::Char('y'));
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.take_apply_request());
        assert!(!app.take_apply_request());
    }

    #[test]
    fn esc_in_browse_returns_to_entry_screen() {
        // demo has no CLI specs, so "back" is the interactive menu.
        let mut app = App::demo(sample());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.screen, Screen::Menu);

        // with CLI specs, "back" is the picker instead.
        let mut app = App::new(specs());
        app.on_fetch_result(app.current_epoch(), Ok(sample()));
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.screen, Screen::Picker);
    }

    // --- picker / async flow ---

    #[test]
    fn multiple_specs_start_at_picker() {
        let app = App::new(specs());
        assert_eq!(app.screen, Screen::Picker);
        assert_eq!(app.picker_state.selected(), Some(0));
    }

    #[test]
    fn single_spec_auto_fetches() {
        let mut app = App::new(vec![SourceSpec::Ssh("office".into())]);
        assert_eq!(app.screen, Screen::Loading);
        assert!(app.take_fetch_request().is_some());
    }

    #[test]
    fn picker_navigation_wraps() {
        let mut app = App::new(specs());
        app.handle_key(KeyCode::Up); // from 0 wraps to last (1)
        assert_eq!(app.picker_state.selected(), Some(1));
    }

    #[test]
    fn selecting_in_picker_requests_fetch() {
        let mut app = App::new(specs());
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::Loading);
        assert_eq!(app.take_fetch_request(), Some(SourceSpec::Ssh("office".into())));
    }

    #[test]
    fn fetch_ok_shows_diff_and_err_shows_error() {
        let mut app = App::new(specs());
        let epoch = app.current_epoch();
        app.on_fetch_result(epoch, Ok(sample()));
        assert_eq!(app.screen, Screen::Diff);
        app.on_fetch_result(epoch, Err("host down".into()));
        assert_eq!(app.screen, Screen::Error("host down".into()));
    }

    #[test]
    fn refresh_requeues_current_source() {
        let mut app = App::new(vec![SourceSpec::Ssh("office".into())]);
        let _ = app.take_fetch_request(); // drain the auto-fetch
        let epoch = app.current_epoch();
        app.on_fetch_result(epoch, Ok(sample())); // now in Diff
        app.refresh();
        assert_eq!(app.screen, Screen::Loading);
        assert_eq!(app.take_fetch_request(), Some(SourceSpec::Ssh("office".into())));
    }

    #[test]
    fn cancelling_loading_ignores_the_stale_result() {
        let mut app = App::new(vec![SourceSpec::Ssh("office".into())]); // auto-fetch -> Loading
        let stale_epoch = app.current_epoch();
        let _ = app.take_fetch_request();

        app.handle_key(KeyCode::Esc); // cancel
        assert_eq!(app.screen, Screen::Picker); // specs non-empty -> picker

        // The abandoned fetch finishes late; its result must be ignored.
        app.on_fetch_result(stale_epoch, Ok(sample()));
        assert_eq!(app.screen, Screen::Picker);
    }

    // --- interactive menu / input flow ---

    #[test]
    fn interactive_starts_at_menu() {
        let app = App::interactive(Vec::new());
        assert_eq!(app.screen, Screen::Menu);
        assert_eq!(app.menu_state.selected(), Some(0));
        // empty known list -> just the two "enter new" prompts
        assert_eq!(app.menu_items, vec![MenuItem::NewSsh, MenuItem::NewFile]);
    }

    #[test]
    fn known_sources_come_first_and_fetch_on_select() {
        let mut app = App::interactive(vec![SourceSpec::Ssh("office".into())]);
        // row 0 = office, row 1 = NewSsh, row 2 = NewFile
        assert_eq!(app.menu_items.len(), 3);
        app.handle_key(KeyCode::Enter); // pick "office"
        assert_eq!(app.screen, Screen::Loading);
        assert_eq!(app.take_fetch_request(), Some(SourceSpec::Ssh("office".into())));
    }

    #[test]
    fn new_ssh_prompt_is_after_known_sources() {
        let mut app = App::interactive(vec![SourceSpec::Ssh("office".into())]);
        app.handle_key(KeyCode::Down); // -> row 1 = NewSsh
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::Input);
        assert_eq!(app.input_kind, InputKind::Ssh);
    }

    #[test]
    fn menu_enter_opens_ssh_input() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Enter); // item 0 = SSH
        assert_eq!(app.screen, Screen::Input);
        assert_eq!(app.input_kind, InputKind::Ssh);
    }

    #[test]
    fn menu_down_then_enter_opens_file_input() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter); // item 1 = File
        assert_eq!(app.input_kind, InputKind::File);
    }

    #[test]
    fn typing_editing_then_submit_requests_ssh_fetch() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Enter); // SSH input
        for c in "office".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.input, "offic");
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.screen, Screen::Loading);
        assert_eq!(app.take_fetch_request(), Some(SourceSpec::Ssh("offic".into())));
    }

    #[test]
    fn empty_submit_stays_in_input() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Enter); // input
        app.handle_key(KeyCode::Enter); // submit blank -> no-op
        assert_eq!(app.screen, Screen::Input);
    }

    #[test]
    fn esc_from_input_returns_to_menu() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Enter);
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.screen, Screen::Menu);
    }

    #[test]
    fn file_input_expands_tilde() {
        let mut app = App::interactive(Vec::new());
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Enter); // file input
        for c in "~/x.pkgs".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        app.handle_key(KeyCode::Enter);
        match app.take_fetch_request() {
            Some(SourceSpec::File(p)) => {
                let home = std::env::var("HOME").expect("HOME set in test env");
                assert_eq!(p, PathBuf::from(home).join("x.pkgs"));
            }
            other => panic!("expected file spec, got {other:?}"),
        }
    }
}
