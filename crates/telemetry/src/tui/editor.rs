// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The layout editor: a small window to add, remove, rename and reorder tabs
//! and to arrange the panels each tab shows. It works on a copy of the
//! layout, and the app applies and stores it after every change. The cursor
//! sits on a panel slot, a picker adds the panels the tab does not show, and
//! the last changes can be undone.

use super::arrange;
use super::layout::{Layout, PanelId, Tab, ALL, MAX_NAME, MAX_TABS};
use ratatui::crossterm::event::KeyCode;

/// How many changes `u` can take back.
const UNDO_DEPTH: usize = 20;

/// What the app does after a key in the editor.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Close the editor; every change was already applied.
    Close,
    /// The user confirmed putting the layout and the screen setup back to
    /// the defaults.
    ResetAll,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub layout: Layout,
    /// The tab being edited.
    pub tab: usize,
    /// The line of the picker while it is open: the panels the tab does not show.
    pub picker: Option<usize>,
    /// The panel slot the arranging keys act on, in screen order.
    pub slot: usize,
    /// The layouts before the latest changes, oldest first.
    undo: Vec<Layout>,
    /// The new name while a tab is being renamed.
    pub rename: Option<String>,
    /// True while the reset waits for a `y`.
    pub confirm_reset: bool,
    /// The last thing the user should know, such as why a key did nothing.
    pub message: Option<String>,
}

impl Editor {
    pub fn new(layout: &Layout, tab: usize) -> Editor {
        Editor {
            layout: layout.clone(),
            tab: tab.min(layout.tabs.len().saturating_sub(1)),
            picker: None,
            slot: 0,
            undo: Vec::new(),
            rename: None,
            confirm_reset: false,
            message: None,
        }
    }

    fn current(&mut self) -> Option<&mut Tab> {
        self.layout.tabs.get_mut(self.tab)
    }

    /// Runs one key and remembers the layout before it when the key changed it.
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        if self.rename.is_none()
            && self.picker.is_none()
            && !self.confirm_reset
            && code == KeyCode::Char('u')
        {
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
        if self.confirm_reset {
            self.confirm_reset = false;
            return if code == KeyCode::Char('y') {
                Outcome::ResetAll
            } else {
                Outcome::Stay
            };
        }
        if self.picker.is_some() {
            self.picker_key(code);
            return Outcome::Stay;
        }
        self.message = None;
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Down | KeyCode::Char('j') => self.look(1),
            KeyCode::Up | KeyCode::Char('k') => self.look(-1),
            KeyCode::Right => self.go(1),
            KeyCode::Left => self.go(-1),
            KeyCode::Char('h') => self.slot = self.slot.saturating_sub(1),
            KeyCode::Char('l') => self.slot += 1,
            KeyCode::Char('H') => self.arrange(|t, s| arrange::move_sideways(t, s, -1)),
            KeyCode::Char('L') => self.arrange(|t, s| arrange::move_sideways(t, s, 1)),
            KeyCode::Char('K') => self.arrange(|t, s| arrange::move_vertically(t, s, -1)),
            KeyCode::Char('J') => self.arrange(|t, s| arrange::move_vertically(t, s, 1)),
            KeyCode::Char('f') => self.arrange(arrange::toggle_width),
            KeyCode::Char('d') => self.arrange(arrange::remove),
            KeyCode::Char('a') => self.open_picker(),
            KeyCode::Char('<') => self.move_tab(-1),
            KeyCode::Char('>') => self.move_tab(1),
            KeyCode::Char('n') => self.new_tab(),
            KeyCode::Char('x') => self.delete_tab(),
            KeyCode::Char('r') => {
                self.rename = self.layout.tabs.get(self.tab).map(|t| t.name.clone());
            }
            KeyCode::Char('R') => self.confirm_reset = true,
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

    /// Moves the cursor to the cell above or below it.
    fn look(&mut self, by: isize) {
        let Some(tab) = self.layout.tabs.get(self.tab) else {
            return;
        };
        if let Ok(to) = arrange::vertical_neighbour(tab, self.slot, by) {
            self.slot = to;
        }
    }

    /// The panels the edited tab does not show, in the order of the list.
    pub fn available(&self) -> Vec<PanelId> {
        let shown = self
            .layout
            .tabs
            .get(self.tab)
            .map_or(&[][..], |t| &t.panels);
        ALL.iter().copied().filter(|p| !shown.contains(p)).collect()
    }

    fn open_picker(&mut self) {
        if self.available().is_empty() {
            self.message = Some("Every panel is already on the tab".to_string());
        } else {
            self.picker = Some(0);
        }
    }

    fn picker_key(&mut self, code: KeyCode) {
        let choices = self.available();
        let at = self
            .picker
            .unwrap_or(0)
            .min(choices.len().saturating_sub(1));
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.picker = None,
            KeyCode::Down | KeyCode::Char('j') => {
                self.picker = Some((at + 1).min(choices.len().saturating_sub(1)));
            }
            KeyCode::Up | KeyCode::Char('k') => self.picker = Some(at.saturating_sub(1)),
            KeyCode::Enter => {
                self.picker = None;
                if let Some(panel) = choices.get(at).copied() {
                    self.arrange(|t, s| arrange::insert_after(t, s, panel));
                }
            }
            _ => {}
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
        let first = self
            .layout
            .tabs
            .get(self.tab)
            .and_then(|t| t.panels.get(self.slot))
            .copied()
            .unwrap_or(ALL[0]);
        self.layout.tabs.push(Tab {
            name,
            panels: vec![first],
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
    fn the_picker_lists_only_the_panels_the_tab_does_not_show() {
        // Arrange
        let mut e = editor();

        // Act
        e.key(KeyCode::Char('a'));

        // Assert
        let choices = e.available();
        assert_eq!(e.picker, Some(0));
        assert!(!choices.contains(&PanelId::Spend) && !choices.contains(&PanelId::Tokens));
        assert_eq!(choices.len(), ALL.len() - 2);
    }

    #[test]
    fn enter_in_the_picker_adds_the_chosen_panel_after_the_cursor_and_closes_it() {
        // Arrange
        let mut e = editor();
        let second = e.available()[1];

        // Act
        e.key(KeyCode::Char('a'));
        e.key(KeyCode::Char('j'));
        e.key(KeyCode::Enter);

        // Assert
        assert_eq!(
            e.layout.tabs[0].panels,
            [PanelId::Spend, second, PanelId::Tokens]
        );
        assert_eq!(e.picker, None);
        assert_eq!(e.slot, 1);
    }

    #[test]
    fn escape_in_the_picker_goes_back_to_the_editor_without_a_change() {
        // Arrange
        let mut e = editor();
        e.key(KeyCode::Char('a'));

        // Act
        let outcome = e.key(KeyCode::Esc);

        // Assert
        assert_eq!(outcome, Outcome::Stay);
        assert_eq!(e.picker, None);
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend, PanelId::Tokens]);
        assert!(e.undo.is_empty());
    }

    #[test]
    fn the_picker_stops_at_its_first_and_last_line() {
        // Arrange
        let mut e = editor();
        e.key(KeyCode::Char('a'));
        let last = e.available().len() - 1;

        // Act
        for _ in 0..=ALL.len() {
            e.key(KeyCode::Char('j'));
        }
        let bottom = e.picker;
        for _ in 0..=ALL.len() {
            e.key(KeyCode::Char('k'));
        }

        // Assert
        assert_eq!(bottom, Some(last));
        assert_eq!(e.picker, Some(0));
    }

    #[test]
    fn a_panel_added_to_an_arranged_tab_gets_a_row_of_its_own() {
        // Arrange
        let mut e = editor();
        e.layout = Layout::parse("A=spend,tokens|models").unwrap();
        e.tab = 0;

        // Act
        e.key(KeyCode::Char('a'));
        e.key(KeyCode::Enter);

        // Assert
        assert_eq!(e.layout.tabs[0].rows, Some(vec![2, 1, 1]));
        assert_eq!(e.layout.tabs[0].panels.len(), 4);
    }

    #[test]
    fn j_and_k_move_the_cursor_to_the_cell_below_and_above() {
        // Arrange
        let mut e = editor();
        e.layout = Layout::parse("A=spend,tokens|models").unwrap();
        e.tab = 0;
        e.slot = 1;

        // Act
        e.key(KeyCode::Char('j'));
        let below = e.slot;
        e.key(KeyCode::Char('k'));

        // Assert
        assert_eq!(below, 2);
        assert_eq!(e.slot, 0);
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
        press_all(&mut e, "a");
        e.key(KeyCode::Enter);
        press_all(&mut e, "ll");

        // Act
        press_all(&mut e, "u");

        // Assert
        assert_eq!(e.layout.tabs[0].panels.len(), 2);
        assert_eq!(e.slot, 1);
    }

    #[test]
    fn d_removes_the_cursor_slot() {
        // Arrange
        let mut e = editor();
        e.slot = 1;

        // Act
        press_all(&mut e, "d");

        // Assert
        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend]);
        assert_eq!(e.slot, 0);
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
    fn escape_and_q_close_the_editor_and_w_and_s_do_nothing() {
        // Arrange
        let mut e = editor();

        // Act
        let saves = [KeyCode::Char('w'), KeyCode::Char('s')].map(|k| e.key(k));
        let esc = e.key(KeyCode::Esc);
        let q = e.key(KeyCode::Char('q'));

        // Assert
        assert_eq!(saves, [Outcome::Stay, Outcome::Stay]);
        assert_eq!((esc, q), (Outcome::Close, Outcome::Close));
        assert_eq!(e.layout, Layout::parse(THREE_TABS).unwrap());
    }

    #[test]
    fn capital_r_asks_first_and_only_y_confirms_the_reset() {
        for (answer, expected) in [
            ('y', Outcome::ResetAll),
            ('n', Outcome::Stay),
            ('Y', Outcome::Stay),
            ('x', Outcome::Stay),
        ] {
            // Arrange
            let mut e = editor();
            let asked = e.key(KeyCode::Char('R'));

            // Act
            let outcome = e.key(KeyCode::Char(answer));

            // Assert
            assert_eq!(asked, Outcome::Stay);
            assert_eq!(outcome, expected, "{answer}");
            assert!(!e.confirm_reset, "{answer}");
        }
    }

    #[test]
    fn escape_answers_the_reset_question_with_no_and_does_not_close_the_editor() {
        // Arrange
        let mut e = editor();
        e.key(KeyCode::Char('R'));

        // Act
        let outcome = e.key(KeyCode::Esc);

        // Assert
        assert_eq!(outcome, Outcome::Stay);
        assert!(!e.confirm_reset);
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
