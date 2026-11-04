// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The layout editor: a small window to add, remove, rename and reorder tabs
//! and to choose which panels each tab shows. It works on a copy of the
//! layout; nothing changes until the user saves.

use super::layout::{Layout, PanelId, Tab, ALL, MAX_NAME, MAX_TABS};
use ratatui::crossterm::event::KeyCode;

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
            rename: None,
            message: None,
        }
    }

    fn current(&mut self) -> Option<&mut Tab> {
        self.layout.tabs.get_mut(self.tab)
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
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
            KeyCode::Char('K') => self.move_panel(-1),
            KeyCode::Char('J') => self.move_panel(1),
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

    /// Moves the highlighted panel earlier or later within the tab.
    fn move_panel(&mut self, by: isize) {
        let panel = ALL[self.row];
        let Some(tab) = self.current() else { return };
        let Some(at) = tab.panels.iter().position(|p| *p == panel) else {
            self.message = Some("Add the panel to the tab first".to_string());
            return;
        };
        let to = at as isize + by;
        if to >= 0 && (to as usize) < tab.panels.len() {
            tab.panels.swap(at, to as usize);
        }
    }

    fn move_tab(&mut self, by: isize) {
        let to = self.tab as isize + by;
        if to >= 0 && (to as usize) < self.layout.tabs.len() {
            self.layout.tabs.swap(self.tab, to as usize);
            self.tab = to as usize;
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
    }

    fn delete_tab(&mut self) {
        if self.layout.tabs.len() == 1 {
            self.message = Some("Keep at least one tab".to_string());
            return;
        }
        self.layout.tabs.remove(self.tab);
        self.tab = self.tab.min(self.layout.tabs.len() - 1);
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
                    && !matches!(c, ';' | '=' | ',')
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

    #[test]
    fn capital_j_and_k_reorder_the_panels_of_a_tab() {
        let mut e = editor();
        e.row = 0;

        e.key(KeyCode::Char('J'));

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Tokens, PanelId::Spend]);
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

        for c in "a;b=c,d".chars() {
            e.key(KeyCode::Char(c));
        }

        assert_eq!(e.rename.as_deref(), Some("abcd"));
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
    fn moving_a_panel_not_in_the_tab_changes_nothing_and_says_why() {
        let mut e = editor();
        e.row = 4;

        e.key(KeyCode::Char('K'));

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend, PanelId::Tokens]);
        assert!(e.message.is_some());
    }

    #[test]
    fn moving_a_panel_past_either_end_changes_nothing() {
        let mut e = editor();
        e.row = 0;
        e.key(KeyCode::Char('K'));
        e.row = 1;
        e.key(KeyCode::Char('J'));

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Spend, PanelId::Tokens]);
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
        e.row = 1;
        e.key(KeyCode::Char('K'));
        e.key(KeyCode::Char('r'));
        e.message = Some("old".into());
        e.rename = Some("Fresh".into());
        e.key(KeyCode::Enter);

        assert_eq!(e.layout.tabs[0].panels, [PanelId::Tokens, PanelId::Spend]);
        assert!(e.message.is_none());
    }
}
