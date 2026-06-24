//! Application state and the logic that mutates it.
//!
//! Crucially, this module knows *nothing* about rendering — it's pure state +
//! behavior. That means we can unit-test navigation, selection and filtering
//! with no terminal at all (see the tests at the bottom). `main.rs` reads this
//! state to draw, and forwards key presses into `handle_key`.

use crate::diff::{Category, DiffEntry};
use ratatui::{crossterm::event::KeyCode, widgets::ListState};
use std::collections::HashSet;

/// Which category of diff entries is currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Install,
    Upgrade,
    Remove,
}

impl Filter {
    /// Does an entry of category `cat` pass this filter?
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

/// All the mutable state of the running app.
pub struct App {
    /// The full diff, never mutated after construction.
    pub entries: Vec<DiffEntry>,
    /// Active category filter.
    pub filter: Filter,
    /// Names of entries the user has ticked for action. Keyed by NAME (not
    /// list index) so a selection survives filtering and re-ordering.
    pub selected: HashSet<String>,
    /// ratatui's list cursor/scroll state. Its index refers to the currently
    /// *visible* (filtered) list.
    pub list_state: ListState,
    /// Set when the user asks to quit; the main loop checks this.
    pub should_quit: bool,
}

impl App {
    pub fn new(entries: Vec<DiffEntry>) -> Self {
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0)); // start with the first row highlighted
        }
        Self {
            entries,
            filter: Filter::All,
            selected: HashSet::new(),
            list_state,
            should_quit: false,
        }
    }

    /// The entries currently visible under the active filter, in order.
    /// Allocates a small Vec of references — fine for our list sizes.
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

    /// The entry under the cursor, if any.
    pub fn selected_entry(&self) -> Option<&DiffEntry> {
        let index = self.list_state.selected()?;
        self.visible().into_iter().nth(index)
    }

    /// Is this package ticked for action?
    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.contains(name)
    }

    /// Move the cursor down one row, wrapping to the top at the end.
    pub fn move_down(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            _ => 0, // off the bottom (or nothing selected) -> wrap to top
        };
        self.list_state.select(Some(next));
    }

    /// Move the cursor up one row, wrapping to the bottom at the top.
    pub fn move_up(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let prev = match self.list_state.selected() {
            Some(0) | None => len - 1, // at top (or nothing) -> wrap to bottom
            Some(i) => i - 1,
        };
        self.list_state.select(Some(prev));
    }

    /// Tick / untick the highlighted entry.
    pub fn toggle_current(&mut self) {
        // Grab the name first so we end the immutable borrow of `self` before
        // we mutate `self.selected`. `let ... else` bails if nothing's selected.
        let Some(name) = self.selected_entry().map(|e| e.name.clone()) else {
            return;
        };
        // `HashSet::remove` returns false if it wasn't present -> then insert.
        if !self.selected.remove(&name) {
            self.selected.insert(name);
        }
    }

    /// Switch filter and keep the cursor valid for the new visible length.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        let len = self.visible_len();
        if len == 0 {
            self.list_state.select(None);
        } else {
            // Clamp the old index into the new range so the cursor doesn't
            // point past the end of a now-shorter list.
            let clamped = self.list_state.selected().unwrap_or(0).min(len - 1);
            self.list_state.select(Some(clamped));
        }
    }

    /// Map a key press to a state change. Returning nothing keeps `main.rs`'s
    /// event loop trivial — and lets us test the whole keymap without a TUI.
    pub fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Tab | KeyCode::Char(' ') => self.toggle_current(),
            KeyCode::Char('a') => self.set_filter(Filter::All),
            KeyCode::Char('i') => self.set_filter(Filter::Install),
            KeyCode::Char('u') => self.set_filter(Filter::Upgrade),
            KeyCode::Char('r') => self.set_filter(Filter::Remove),
            _ => {}
        }
    }
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

    #[test]
    fn starts_on_first_entry() {
        let app = App::new(sample());
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn empty_app_has_no_selection() {
        let app = App::new(vec![]);
        assert_eq!(app.list_state.selected(), None);
        assert!(app.selected_entry().is_none());
    }

    #[test]
    fn movement_wraps_both_ways() {
        let mut app = App::new(sample());
        app.move_up(); // from 0 wraps to last
        assert_eq!(app.selected_entry().unwrap().name, "hyprland");
        app.move_down(); // wraps back to first
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn toggle_adds_then_removes() {
        let mut app = App::new(sample());
        app.toggle_current();
        assert!(app.is_selected("btop"));
        app.toggle_current();
        assert!(!app.is_selected("btop"));
    }

    #[test]
    fn filter_limits_visible_and_clamps_cursor() {
        let mut app = App::new(sample());
        app.move_down();
        app.move_down(); // now on "hyprland" (index 2)
        app.set_filter(Filter::Install); // only "btop" remains
        assert_eq!(app.visible().len(), 1);
        // cursor was at 2, clamped into the 1-item list -> index 0 -> btop
        assert_eq!(app.selected_entry().unwrap().name, "btop");
    }

    #[test]
    fn selection_survives_filtering() {
        let mut app = App::new(sample());
        app.toggle_current(); // tick "btop"
        app.set_filter(Filter::Remove); // hides btop, shows discord
        assert!(app.is_selected("btop")); // still ticked even though hidden
    }

    #[test]
    fn q_sets_quit() {
        let mut app = App::new(sample());
        assert!(!app.should_quit);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }
}
