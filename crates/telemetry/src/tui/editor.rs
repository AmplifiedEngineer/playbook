// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The layout editor: a small window to add, remove, rename and reorder tabs
//! and to arrange the panels each tab shows. It works on a copy of the
//! layout; nothing changes until the user saves. The cursor sits on a panel
//! slot, and the last changes can be undone.

use super::arrange;
use super::layout::{Layout, PanelId, Tab, ALL, MAX_NAME, MAX_TABS};
use ratatui::crossterm::event::KeyCode;

/// How many changes `u` can take back.
const UNDO_DEPTH: usize = 20;

/// What the app does after a key in the editor.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Close without saving.
    Cancel,
    /// Apply and store the edited layout.
    Save,
    /// Go back to the default layout and clear the stored one.
    Reset,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub layout: Layout,
    /// The tab being edited.
    pub tab: usize,
    /// The highlighted line of the panel list.
    pub row: usize,
    /// The panel slot the arranging keys act on, in screen order.
    pub slot: usize,
    /// The layouts before the latest changes, oldest first.
    undo: Vec<Layout>,
    /// The new name while a tab is being renamed.
    pub rename: Option<String>,
    /// The last thing the user should know, such as why a key did nothing.
    pub message: Option<String>,
}

impl Editor {
    pub fn new(layout: &Layout, tab: usize) -> Editor {
        Editor {
            layout: layout.clone(),
            tab: tab.min(layout.tabs.len().saturating_sub(1)),
            row: 0,
            slot: 0,
            undo: Vec::new(),
            rename: None,
            message: None,
        }
    }

    fn current(&mut self) -> Option<&mut Tab> {
        self.layout.tabs.get_mut(self.tab)
    }

    /// Runs one key and remembers the layout before it when the key changed it.
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        if self.rename.is_none() && code == KeyCode::Char('u') {
            self.message = None;
            self.undo();
            self.clamp_slot();
            return Outcome::Stay;
        }
        let before = self.layout.clone();
        let outcome = self.dispatch(code);
        if self.layout != before {
            if self.undo.len() == UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.undo.push(before);
        }
        self.clamp_slot();
        outcome
    }

    /// Keeps the cursor on a panel of the edited tab.
    fn clamp_slot(&mut self) {
        let panels = self.layout.tabs.get(self.tab).map_or(0, |t| t.panels.len());
        self.slot = self.slot.min(panels.saturating_sub(1));
    }

    fn undo(&mut self) {
        match self.undo.pop() {
            Some(layout) => {
                self.tab = self.tab.min(layout.tabs.len().saturating_sub(1));
                self.layout = layout;
            }
            None => self.message = Some("Nothing to undo".to_string()),
        }
    }

    fn dispatch(&mut self, code: KeyCode) -> Outcome {
        if self.rename.is_some() {
            self.rename_key(code);
            return Outcome::Stay;
        }
        self.message = None;
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Cancel,
            KeyCode::Down | KeyCode::Char('j') => self.row = (self.row + 1).min(ALL.len() - 1),
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Right => self.go(1),
            KeyCode::Left => self.go(-1),
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle(),
            KeyCode::Char('h') => self.slot = self.slot.saturating_sub(1),
            KeyCode::Char('l') => self.slot += 1,
            KeyCode::Char('H') => self.arrange(|t, s| arrange::move_sideways(t, s, -1)),
            KeyCode::Char('L') => self.arrange(|t, s| arrange::move_sideways(t, s, 1)),
            KeyCode::Char('K') => self.arrange(|t, s| arrange::move_vertically(t, s, -1)),
            KeyCode::Char('J') => self.arrange(|t, s| arrange::move_vertically(t, s, 1)),
            KeyCode::Char('f') => self.arrange(arrange::toggle_width),
            KeyCode::Char('d') => self.arrange(arrange::remove),
            KeyCode::Char('a') => {
                let panel = ALL[self.row];
                self.arrange(|t, s| arrange::insert_after(t, s, panel));
            }
            KeyCode::Char('<') => self.move_tab(-1),
            KeyCode::Char('>') => self.move_tab(1),
            KeyCode::Char('n') => self.new_tab(),
            KeyCode::Char('x') => self.delete_tab(),
            KeyCode::Char('r') => {
                self.rename = self.layout.tabs.get(self.tab).map(|t| t.name.clone());
            }
            KeyCode::Char('w') | KeyCode::Char('s') => {
                return match Layout::parse(&self.layout.to_text()) {
                    Ok(_) => Outcome::Save,
                    Err(why) => {
                        self.message = Some(why);
                        Outcome::Stay
                    }
                };
            }
            KeyCode::Char('R') => return Outcome::Reset,
            _ => {}
        }
        Outcome::Stay
    }

    /// Edits the next or previous tab, wrapping around.
    fn go(&mut self, by: isize) {
        let n = self.layout.tabs.len().max(1) as isize;
        self.tab = (self.tab as isize + by).rem_euclid(n) as usize;
        self.slot = 0;
    }

    /// Applies one arranging operation to the edited tab and moves the cursor
    /// to where it says; a refusal leaves the tab as it was and says why.
    fn arrange(&mut self, op: impl FnOnce(&mut Tab, usize) -> Result<usize, String>) {
        let slot = self.slot;
        let Some(tab) = self.current() else { return };
        match op(tab, slot.min(tab.panels.len().saturating_sub(1))) {
            Ok(to) => self.slot = to,
            Err(why) => self.message = Some(why),
        }
    }

    /// Adds the highlighted panel to the tab, or takes it off. A tab keeps at
    /// least one panel.
    fn toggle(&mut self) {
        let panel = ALL[self.row];
        let Some(tab) = self.current() else { return };
        if let Some(at) = tab.panels.iter().position(|p| *p == panel) {
            if tab.panels.len() == 1 {
                self.message = Some("A tab needs at least one panel".to_string());
            } else {
                tab.panels.remove(at);
                tab.rows = None;
            }
        } else {
            tab.panels.push(panel);
            tab.rows = None;
        }
    }

    fn move_tab(&mut self, by: isize) {
        let to = self.tab as isize + by;
        if to >= 0 && (to as usize) < self.layout.tabs.len() {
            self.layout.tabs.swap(self.tab, to as usize);
            self.tab = to as usize;
            self.slot = 0;
        }
    }

    fn new_tab(&mut self) {
        if self.layout.tabs.len() >= MAX_TABS {
            self.message = Some(format!("At most {MAX_TABS} tabs"));
            return;
        }
        let name = (1..)
            .map(|i| format!("Tab {i}"))
            .find(|n| {
                !self
                    .layout
                    .tabs
                    .iter()
                    .any(|t| t.name.eq_ignore_ascii_case(n))
            })
            .unwrap_or_default();
        self.layout.tabs.push(Tab {
            name,
            panels: vec![ALL[self.row]],
            rows: None,
        });
        self.tab = self.layout.tabs.len() - 1;
        self.slot = 0;
    }

    fn delete_tab(&mut self) {
        if self.layout.tabs.len() == 1 {
            self.message = Some("Keep at least one tab".to_string());
            return;
        }
        self.layout.tabs.remove(self.tab);
        self.tab = self.tab.min(self.layout.tabs.len() - 1);
        self.slot = 0;
    }

    fn rename_key(&mut self, code: KeyCode) {
        let Some(name) = self.rename.as_mut() else {
            return;
        };
        match code {
            KeyCode::Esc => self.rename = None,
            KeyCode::Backspace => {
                name.pop();
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !matches!(c, ';' | '=' | ',' | '|')
                    && name.chars().count() < MAX_NAME =>
            {
                name.push(c);
            }
            KeyCode::Enter => {
                let new = name.trim().to_string();
                let taken = self
                    .layout
                    .tabs
                    .iter()
                    .enumerate()
                    .any(|(i, t)| i != self.tab && t.name.eq_ignore_ascii_case(&new));
                if new.is_empty() {
                    self.message = Some("A tab needs a name".to_string());
                } else if taken {
                    self.message = Some(format!("The name {new:?} is taken"));
                } else {
                    if let Some(tab) = self.layout.tabs.get_mut(self.tab) {
                        tab.name = new;
                    }
                    self.rename = None;
                    self.message = None;
                }
            }
            _ => {}
        }
    }

    /// Whether the highlighted panel is on the tab being edited, and where.
    pub fn position(&self, panel: PanelId) -> Option<usize> {
        self.layout
            .tabs
            .get(self.tab)?
            .panels
            .iter()
            .position(|p| *p == panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three tabs and eight panels of the layout before the burn-up
    /// panel, so these tests do not move with the default.
    const THREE_TABS: &str =
        "Charts=spend,tokens;Numbers=models,breakdown,budget,projection;Sessions=live,sessions";

    fn editor() -> Editor {
        Editor::new(&Layout::parse(THREE_TABS).unwrap(), 0)
    }

    fn names(e: &Editor) -> Vec<&str> {
        e.layout.tabs.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn space_adds_and_removes_the_highlighted_panel() {
        let mut e = editor();
        e.row = 2;

        e.key(KeyCode::Char(' '));
        let added = e.position(PanelId::Models);
        e.key(KeyCode::Char(' '));

        assert_eq!(added, Some(2));
        assert_eq!(e.position(PanelId::Models), None);
    }

    #[test]
    fn the_last_panel_of_a_tab_cannot_be_removed() {
        let mut e = Editor::new(&Layout::parse("Only=spend").unwrap(), 0);

        e.key(KeyCode::Char(' '));

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend]);
        assert!(e.message.is_some());
    }

    fn press_all(e: &mut Editor, keys: &str) {
        for c in keys.chars() {
            e.key(KeyCode::Char(c));
        }
    }

    #[test]
    fn capital_j_and_k_move_the_slot_across_rows_and_the_cursor_follows() {
        // Arrange
        let mut e = editor();

        // Act
        press_all(&mut e, "J");

        // Assert
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Tokens, PanelId::Spend]);
        assert_eq!(e.slot, 1);
        assert_eq!(e.layout.tabs[0].rows, None);
    }

    #[test]
    fn h_and_l_move_the_cursor_through_the_slots_and_stop_at_the_ends() {
        // Arrange
        let mut e = editor();

        // Act
        press_all(&mut e, "lll");
        let end = e.slot;
        press_all(&mut e, "hhh");

        // Assert
        assert_eq!(end, 1);
        assert_eq!(e.slot, 0);
    }

    #[test]
    fn f_splits_a_row_and_u_takes_the_change_back() {
        // Arrange
        let mut e = Editor::new(&Layout::parse("A=spend,tokens").unwrap(), 0);
        e.layout.tabs[0].rows = Some(vec![2]);

        // Act
        press_all(&mut e, "f");
        let split = e.layout.tabs[0].rows.clone();
        press_all(&mut e, "u");

        // Assert
        assert_eq!(split, Some(vec![1, 1]));
        assert_eq!(e.layout.tabs[0].rows, Some(vec![2]));
    }

    #[test]
    fn u_with_nothing_to_undo_says_so_and_keeps_only_twenty_steps() {
        // Arrange
        let mut e = editor();
        let empty = {
            press_all(&mut e, "u");
            e.message.clone()
        };

        // Act
        for _ in 0..UNDO_DEPTH + 5 {
            press_all(&mut e, "J");
        }
        for _ in 0..UNDO_DEPTH + 5 {
            press_all(&mut e, "u");
        }

        // Assert
        assert!(empty.is_some_and(|m| m.contains("Nothing")));
        assert_eq!(e.undo.len(), 0);
        assert_eq!(e.message.as_deref(), Some("Nothing to undo"));
    }

    #[test]
    fn undoing_an_added_panel_pulls_the_cursor_back_onto_the_tab() {
        // Arrange
        let mut e = editor();
        e.row = ALL.iter().position(|p| *p == PanelId::Live).unwrap();
        press_all(&mut e, "a");
        press_all(&mut e, "ll");

        // Act
        press_all(&mut e, "u");

        // Assert
        assert_eq!(e.layout.tabs[0].panels.len(), 2);
        assert_eq!(e.slot, 1);
    }

    #[test]
    fn d_removes_the_slot_and_a_adds_the_listed_panel_after_it() {
        // Arrange
        let mut e = editor();
        e.row = ALL.iter().position(|p| *p == PanelId::Live).unwrap();

        // Act
        press_all(&mut e, "a");
        let added = e.layout.tabs[0].panels.clone();
        press_all(&mut e, "d");

        // Assert
        assert_eq!(added, [PanelId::Spend, PanelId::Live, PanelId::Tokens]);
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend, PanelId::Tokens]);
    }

    #[test]
    fn a_refused_arranging_key_changes_nothing_and_leaves_no_undo_step() {
        // Arrange
        let mut e = Editor::new(&Layout::parse("Only=spend").unwrap(), 0);

        // Act
        press_all(&mut e, "dKH");

        // Assert
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend]);
        assert!(e.undo.is_empty());
        assert!(e.message.is_some());
    }

    #[test]
    fn angle_keys_reorder_the_tabs_and_follow_the_moved_tab() {
        let mut e = editor();

        e.key(KeyCode::Char('>'));

        assert_eq!(names(&e), ["Numbers", "Charts", "Sessions"]);
        assert_eq!(e.tab, 1);
    }

    #[test]
    fn n_adds_a_uniquely_named_tab_and_x_deletes_it() {
        let mut e = editor();

        e.key(KeyCode::Char('n'));
        let added = names(&e).contains(&"Tab 1");
        e.key(KeyCode::Char('x'));

        assert!(added);
        assert_eq!(names(&e), ["Charts", "Numbers", "Sessions"]);
    }

    #[test]
    fn the_last_tab_cannot_be_deleted() {
        let mut e = Editor::new(&Layout::classic(), 0);

        e.key(KeyCode::Char('x'));

        assert_eq!(e.layout.tabs.len(), 1);
        assert!(e.message.is_some());
    }

    #[test]
    fn adding_a_panel_to_an_arranged_tab_makes_it_automatic() {
        let mut e = editor();
        e.layout = Layout::parse("A=spend,tokens|models").unwrap();
        e.tab = 0;
        e.row = ALL.iter().position(|p| *p == PanelId::Live).unwrap();

        e.key(KeyCode::Enter);

        assert_eq!(e.layout.tabs[0].rows, None);
        assert_eq!(e.layout.tabs[0].panels.len(), 4);
    }

    #[test]
    fn at_most_twelve_tabs_can_be_added() {
        let mut e = editor();
        for _ in 0..MAX_TABS + 1 {
            e.key(KeyCode::Char('n'));
        }

        assert_eq!(e.layout.tabs.len(), MAX_TABS);
    }

    #[test]
    fn rename_edits_the_name_and_refuses_a_taken_or_empty_one() {
        let mut e = editor();
        e.key(KeyCode::Char('r'));
        for _ in 0..6 {
            e.key(KeyCode::Backspace);
        }
        e.key(KeyCode::Enter);
        let empty_refused = e.rename.is_some();
        for c in "numbers".chars() {
            e.key(KeyCode::Char(c));
        }
        e.key(KeyCode::Enter);
        let taken_refused = e.rename.is_some();
        for _ in 0..7 {
            e.key(KeyCode::Backspace);
        }
        for c in "Money".chars() {
            e.key(KeyCode::Char(c));
        }
        e.key(KeyCode::Enter);

        assert!(empty_refused && taken_refused);
        assert_eq!(names(&e)[0], "Money");
        assert!(e.rename.is_none());
    }

    #[test]
    fn rename_drops_characters_the_layout_text_cannot_hold() {
        let mut e = editor();
        e.key(KeyCode::Char('r'));
        e.rename = Some(String::new());

        for c in "a;b=c,d|e".chars() {
            e.key(KeyCode::Char(c));
        }

        assert_eq!(e.rename.as_deref(), Some("abcde"));
    }

    #[test]
    fn save_returns_a_layout_that_parses_and_escape_cancels() {
        let mut e = editor();

        let save = e.key(KeyCode::Char('w'));
        let cancel = e.key(KeyCode::Esc);

        assert_eq!(save, Outcome::Save);
        assert_eq!(cancel, Outcome::Cancel);
        assert!(Layout::parse(&e.layout.to_text()).is_ok());
    }

    #[test]
    fn capital_r_asks_for_the_default_layout() {
        let mut e = editor();

        assert_eq!(e.key(KeyCode::Char('R')), Outcome::Reset);
    }

    #[test]
    fn the_line_keys_stop_at_the_first_and_last_panel() {
        let mut e = editor();

        for _ in 0..=ALL.len() {
            e.key(KeyCode::Char('j'));
        }
        let bottom = e.row;
        for _ in 0..=ALL.len() {
            e.key(KeyCode::Char('k'));
        }

        assert_eq!(bottom, ALL.len() - 1);
        assert_eq!(e.row, 0);
    }

    #[test]
    fn left_and_right_wrap_around_the_tabs() {
        let mut e = editor();

        e.key(KeyCode::Left);
        let last = e.tab;
        e.key(KeyCode::Right);

        assert_eq!(last, 2);
        assert_eq!(e.tab, 0);
    }

    #[test]
    fn escape_while_renaming_cancels_the_rename_and_keeps_the_editor_open() {
        let mut e = editor();
        e.key(KeyCode::Char('r'));
        e.key(KeyCode::Char('x'));

        let outcome = e.key(KeyCode::Esc);

        assert_eq!(outcome, Outcome::Stay);
        assert!(e.rename.is_none());
        assert_eq!(names(&e)[0], "Charts");
    }

    #[test]
    fn a_name_longer_than_the_limit_is_cut_while_typing() {
        let mut e = editor();
        e.key(KeyCode::Char('r'));
        e.rename = Some(String::new());

        for _ in 0..20 {
            e.key(KeyCode::Char('a'));
        }

        assert_eq!(
            e.rename.as_deref().map(|n| n.chars().count()),
            Some(MAX_NAME)
        );
    }

    #[test]
    fn moving_a_panel_past_either_end_changes_nothing_and_says_why() {
        // Arrange
        let mut e = editor();

        // Act
        e.key(KeyCode::Char('K'));
        let top = e.message.clone();
        e.slot = 1;
        e.key(KeyCode::Char('J'));

        // Assert
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend, PanelId::Tokens]);
        assert!(top.is_some() && e.message.is_some());
    }

    #[test]
    fn moving_a_tab_past_either_end_changes_nothing() {
        let mut e = editor();

        e.key(KeyCode::Char('<'));
        e.tab = 2;
        e.key(KeyCode::Char('>'));

        assert_eq!(names(&e), ["Charts", "Numbers", "Sessions"]);
        assert_eq!(e.tab, 2);
    }

    #[test]
    fn deleting_the_last_tab_selects_the_new_last_tab() {
        let mut e = editor();
        e.tab = 2;

        e.key(KeyCode::Char('x'));

        assert_eq!(e.tab, 1);
    }

    #[test]
    fn k_moves_a_panel_earlier_and_a_successful_rename_clears_a_stale_message() {
        let mut e = editor();
        e.slot = 1;
        e.key(KeyCode::Char('K'));
        e.key(KeyCode::Char('r'));
        e.message = Some("old".into());
        e.rename = Some("Fresh".into());
        e.key(KeyCode::Enter);

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Tokens, PanelId::Spend]);
        assert!(e.message.is_none());
    }
}
