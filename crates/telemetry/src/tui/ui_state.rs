// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What pbtop remembers between runs: the selected tab, the range, the chart
//! and breakdown views, the sort of each table and the filter. Only the
//! screen setup is stored, never data. The value is one line of JSON in the
//! `usage.ui.state` config key (app tier), written by pbtop. A missing,
//! unreadable or odd value gives the defaults; a bad part is dropped alone.

use super::app::{ProjectsView, SpendView};
use super::machine::MachineSel;
use super::sort::{Panel, Sort, PANELS};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// The key that holds the state.
pub const KEY: &str = "usage.ui.state";

/// The longest filter that is stored, in characters.
const MAX_FILTER: usize = 64;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiState {
    /// The selected tab, by name so a reordered layout keeps the choice.
    pub tab: Option<String>,
    /// A range name such as `week` or `30d`.
    pub range: Option<String>,
    pub spend_view: Option<SpendView>,
    pub projects_view: Option<ProjectsView>,
    /// Column and direction of each table, in `PANELS` order.
    pub sorts: Vec<Option<Sort>>,
    pub filter: Option<String>,
    pub machine: Option<MachineSel>,
}

impl UiState {
    pub fn to_text(&self) -> String {
        let sorts: Vec<Value> = PANELS
            .iter()
            .enumerate()
            .map(|(i, _)| match self.sorts.get(i).copied().flatten() {
                Some(s) => json!([s.col, s.desc]),
                None => Value::Null,
            })
            .collect();
        json!({
            "tab": self.tab,
            "range": self.range,
            "spend": self.spend_view.map(|v| v.name()),
            "projects": self.projects_view.map(|v| v.name()),
            "sorts": sorts,
            "filter": self.filter,
            "machine": self.machine.as_ref().map(MachineSel::to_text),
        })
        .to_string()
    }

    /// Reads a stored value. Never fails: whatever cannot be read is left out.
    pub fn parse(text: &str) -> UiState {
        let Ok(doc) = serde_json::from_str::<Value>(text) else {
            return UiState::default();
        };
        let word = |key: &str| doc.get(key).and_then(Value::as_str).map(str::to_string);
        let sorts = doc
            .get("sorts")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .take(PANELS.len())
                    .map(|item| {
                        let pair = item.as_array()?;
                        let col = usize::try_from(pair.first()?.as_u64()?).ok()?;
                        Some(Sort::new(col, pair.get(1)?.as_bool()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        UiState {
            tab: word("tab").filter(|t| !t.is_empty()),
            range: word("range"),
            spend_view: word("spend").and_then(|n| find_spend(&n)),
            projects_view: word("projects").and_then(|n| find_projects(&n)),
            sorts,
            filter: word("filter").map(|f| f.chars().take(MAX_FILTER).collect()),
            machine: word("machine").map(|m| MachineSel::parse(&m)),
        }
    }

    /// The sort stored for `panel`, when it is one the panel accepts.
    pub fn sort_for(&self, panel: Panel, compared: bool) -> Option<Sort> {
        let sort = self.sorts.get(panel.index()).copied().flatten()?;
        panel.sortable(compared).contains(&sort.col).then_some(sort)
    }
}

/// The view named `name`, found by stepping `next` once around the cycle.
fn find_view<T: Copy + PartialEq>(
    start: T,
    next: impl Fn(T) -> T,
    name: impl Fn(T) -> &'static str,
    wanted: &str,
) -> Option<T> {
    let mut view = start;
    loop {
        if name(view) == wanted {
            return Some(view);
        }
        view = next(view);
        if view == start {
            return None;
        }
    }
}

fn find_spend(wanted: &str) -> Option<SpendView> {
    find_view(
        SpendView::default(),
        SpendView::next,
        SpendView::name,
        wanted,
    )
}

fn find_projects(wanted: &str) -> Option<ProjectsView> {
    find_view(
        ProjectsView::default(),
        ProjectsView::next,
        ProjectsView::name,
        wanted,
    )
}

/// The stored state for the user at `home`.
pub fn load(home: &Path) -> UiState {
    crate::config::resolve(home, KEY)
        .and_then(|v| v.as_str().map(UiState::parse))
        .unwrap_or_default()
}

/// Stores `state`. Quiet on failure: losing the screen setup must never
/// disturb the view.
pub fn save(home: &Path, state: &UiState) {
    let _ = crate::config::save_app(home, KEY, &Value::from(state.to_text()));
}

/// Stores the layout text; empty text goes back to the default tabs.
/// `Ok(false)` means there is no config store to write to; the layout then
/// lasts only for this run.
pub fn save_layout(home: &Path, text: &str) -> Result<bool, String> {
    crate::config::save_app(home, "usage.ui.layout", &Value::from(text))
}

/// Puts the layout and the screen setup back to the defaults by storing the
/// empty value, which already means the default. The screen setup goes first,
/// so a failure never leaves the screen showing a layout that is no longer
/// stored. Only these two keys are written. `Ok(false)` means there is no config store.
pub fn reset(home: &Path) -> Result<bool, String> {
    reset_with(
        || save_layout(home, ""),
        || crate::config::save_app(home, KEY, &Value::from("")),
    )
}

fn reset_with(
    clear_layout: impl FnOnce() -> Result<bool, String>,
    clear_state: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    let state = clear_state().map_err(|e| format!("{e}, nothing changed"))?;
    let layout =
        clear_layout().map_err(|e| format!("{e}; the screen setting was already cleared"))?;
    Ok(layout && state)
}

/// `text` as one single-quoted shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What to tell the user after a layout save, `None` when nothing needs saying.
pub fn save_notice(result: Result<bool, String>, text: &str) -> Option<String> {
    match result {
        Ok(true) => None,
        // The default layout needs no stored value.
        Ok(false) if text.is_empty() => None,
        Ok(false) => Some(format!(
            "Layout kept for this run only. To keep it, run: playbook config set --global usage.ui.layout {}",
            shell_quote(text)
        )),
        Err(e) => Some(e),
    }
}

/// How soon after a write the next one may follow.
const WRITE_EVERY: Duration = Duration::from_millis(200);

/// What a flush tells the user.
#[derive(Debug, PartialEq, Eq)]
pub enum Flushed {
    /// Nothing was written.
    Idle,
    /// The layout was stored, or there is no store and the user was told.
    Saved,
    /// There is no config store: the command to run, said once while the
    /// editor is open and again for the layout it closes with.
    Notice(String),
    /// A write failed. Said once for a run of failures; the layout stays
    /// pending and a due flush retries it.
    Failed(String),
}

/// The layout text waiting to be stored. Changes arrive faster than they need
/// writing, so only the newest text is kept and it is written at most every
/// 200 ms, and at once when the editor closes or the program quits.
#[derive(Debug, Default)]
pub struct Autosave {
    dirty: Option<String>,
    last_write: Option<Instant>,
    told_no_store: bool,
    failing: bool,
}

/// The message for a layout that could not be written.
fn not_saved(why: &str) -> String {
    format!("Layout not saved: {why}")
}

impl Autosave {
    /// Drops the pending text, for when the layout is reset to the default.
    pub fn discard(&mut self) {
        self.dirty = None;
    }

    /// Remembers `text` as the layout to store, replacing an older one.
    pub fn push(&mut self, text: String) {
        self.dirty = Some(text);
    }

    /// Writes the pending text through `write` when it is due: 200 ms after
    /// the last write, or at once with `force`. A write that just failed is
    /// retried only when the 200 ms are up, so a store that stays broken is
    /// not hammered on every pass of the loop.
    pub fn flush(
        &mut self,
        now: Instant,
        force: bool,
        write: impl FnOnce(&str) -> Result<bool, String>,
    ) -> Flushed {
        if self.dirty.is_none() {
            return Flushed::Idle;
        }
        let waiting = self
            .last_write
            .is_some_and(|at| now.duration_since(at) < WRITE_EVERY);
        if waiting && !(force && !self.failing) {
            return Flushed::Idle;
        }
        let Some(text) = self.dirty.clone() else {
            return Flushed::Idle;
        };
        self.last_write = Some(now);
        match write(&text) {
            Err(e) => {
                let first = !self.failing;
                self.failing = true;
                if first {
                    Flushed::Failed(not_saved(&e))
                } else {
                    Flushed::Idle
                }
            }
            Ok(stored) => {
                self.dirty = None;
                self.failing = false;
                // Without a store the user hears it once while editing, and
                // again with the layout the editor closes with.
                let notice = if stored || (self.told_no_store && !force) {
                    None
                } else {
                    save_notice(Ok(false), &text)
                };
                notice.map_or(Flushed::Saved, |n| {
                    self.told_no_store = true;
                    Flushed::Notice(n)
                })
            }
        }
    }

    /// Writes what is still pending, once, at the end of the run. Gives the
    /// message to show when that write fails.
    pub fn finish(&mut self, write: impl FnOnce(&str) -> Result<bool, String>) -> Option<String> {
        let text = self.dirty.take()?;
        write(&text).err().map(|e| not_saved(&e))
    }
}

/// The stored layout text and the layout it gives, with a message when the
/// stored text is invalid.
pub fn load_layout(home: &Path) -> (super::layout::Layout, Option<String>) {
    let stored = crate::config::stored_raw(home, "usage.ui.layout");
    super::layout::Layout::from_stored(stored.as_ref().and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UiState {
        UiState {
            tab: Some("Numbers".into()),
            range: Some("30d".into()),
            spend_view: Some(SpendView::Daily),
            projects_view: Some(ProjectsView::Agents),
            sorts: vec![None, None, Some(Sort::new(1, false)), None, None, None],
            filter: Some("alpha".into()),
            machine: Some(MachineSel::Key("m-2".into())),
        }
    }

    #[test]
    fn a_corrupt_machine_pick_falls_back_to_this_machine() {
        for text in [
            r#"{"machine":"???"}"#,
            r#"{"machine":"key:"}"#,
            r#"{"machine":7}"#,
        ] {
            let state = UiState::parse(text);
            assert_ne!(
                state.machine,
                Some(MachineSel::All),
                "{text} must not widen the scope"
            );
            assert!(
                matches!(state.machine, None | Some(MachineSel::This)),
                "{text}"
            );
        }
        assert_eq!(UiState::parse("not json").machine, None);
    }

    #[test]
    fn the_state_round_trips_through_its_text() {
        let state = sample();

        let back = UiState::parse(&state.to_text());

        assert_eq!(back, state);
    }

    #[test]
    fn text_that_is_not_json_gives_the_defaults() {
        assert_eq!(UiState::parse("not json"), UiState::default());
        assert_eq!(UiState::parse(""), UiState::default());
        assert_eq!(UiState::parse("[1,2]"), UiState::default());
    }

    #[test]
    fn a_bad_part_is_dropped_and_the_rest_is_kept() {
        let state = UiState::parse(r#"{"tab":"Charts","spend":"nope","sorts":"x","range":5}"#);

        assert_eq!(state.tab.as_deref(), Some("Charts"));
        assert_eq!(state.spend_view, None);
        assert_eq!(state.range, None);
        assert!(state.sorts.is_empty());
    }

    #[test]
    fn a_sort_column_the_panel_does_not_have_is_ignored() {
        let mut state = sample();
        state.sorts[Panel::Models.index()] = Some(Sort::new(99, true));

        assert_eq!(state.sort_for(Panel::Models, false), None);
    }

    #[test]
    fn a_long_filter_is_cut() {
        let long = "x".repeat(500);
        let text = json!({ "filter": long }).to_string();

        let state = UiState::parse(&text);

        assert_eq!(state.filter.unwrap().chars().count(), MAX_FILTER);
    }

    #[test]
    fn a_fresh_home_loads_the_defaults() {
        let home = crate::util::scratch_dir("ui-fresh");

        assert_eq!(load(&home), UiState::default());
    }

    #[test]
    fn saving_without_a_config_store_does_nothing_and_does_not_fail() {
        let home = crate::util::scratch_dir("ui-nostore");

        save(&home, &sample());

        assert_eq!(load(&home), UiState::default());
    }

    #[test]
    fn a_saved_state_is_restored_on_the_next_load() {
        let home = crate::util::scratch_dir("ui-roundtrip");
        let data = crate::config::root(&home).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let db = playbook_data::config::db_in(&data);
        let conn = playbook_store::Connection::open(&db).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        drop(conn);

        save(&home, &sample());

        assert_eq!(load(&home), sample());
    }

    #[test]
    fn an_invalid_stored_layout_falls_back_to_the_default_layout_and_says_why() {
        let home = crate::util::scratch_dir("ui-layout");
        let data = crate::config::root(&home).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let db = playbook_data::config::db_in(&data);
        let conn = playbook_store::Connection::open(&db).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        conn.execute(
            "INSERT INTO config VALUES ('global', '', 'usage.ui.layout', '\"A=bogus\"', 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let (layout, warning) = load_layout(&home);

        assert_eq!(layout, super::super::layout::Layout::default());
        assert!(warning.is_some_and(|w| w.contains("bogus")));
    }

    fn store(tag: &str) -> std::path::PathBuf {
        let home = crate::util::scratch_dir(tag);
        let data = crate::config::root(&home).join("data");
        std::fs::create_dir_all(&data).unwrap();
        let conn = playbook_store::Connection::open(playbook_data::config::db_in(&data)).unwrap();
        conn.execute_batch(playbook_data::config::SCHEMA_SQL)
            .unwrap();
        home
    }

    #[test]
    fn a_saved_layout_is_read_back_on_the_next_load() {
        let home = store("ui-save-layout");

        let wrote = save_layout(&home, "Money=spend;Live=live").unwrap();
        let (layout, warning) = load_layout(&home);

        assert!(wrote);
        assert!(warning.is_none());
        assert_eq!(layout.to_text(), "Money=spend;Live=live");
    }

    fn after(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn ten_changes_inside_the_window_are_written_as_the_first_and_then_the_last() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        let mut written: Vec<String> = Vec::new();

        // Act
        for i in 0..10 {
            auto.push(format!("text {i}"));
            auto.flush(after(start, i * 10), false, |t| {
                written.push(t.to_string());
                Ok(true)
            });
        }
        auto.flush(after(start, 250), false, |t| {
            written.push(t.to_string());
            Ok(true)
        });

        // Assert
        assert_eq!(written, ["text 0", "text 9"]);
    }

    #[test]
    fn a_forced_flush_writes_at_once_and_an_empty_one_writes_nothing() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        auto.push("a".into());
        auto.flush(start, false, |_| Ok(true));
        auto.push("b".into());
        let mut written = Vec::new();

        // Act
        let forced = auto.flush(after(start, 1), true, |t| {
            written.push(t.to_string());
            Ok(true)
        });
        let nothing = auto.flush(after(start, 2), true, |_| Err("unused".into()));

        // Assert
        assert_eq!((forced, nothing), (Flushed::Saved, Flushed::Idle));
        assert_eq!(written, ["b"]);
    }

    #[test]
    fn a_failed_write_keeps_the_text_and_the_next_success_clears_it() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        auto.push("a".into());

        // Act
        let failed = auto.flush(start, false, |_| Err("disk full".into()));
        let waiting = auto.flush(after(start, 50), false, |_| Ok(true));
        let retried = auto.flush(after(start, 300), false, |_| Ok(true));
        let after_success = auto.flush(after(start, 600), false, |_| Err("unused".into()));

        // Assert
        assert_eq!(
            failed,
            Flushed::Failed("Layout not saved: disk full".into())
        );
        assert_eq!(waiting, Flushed::Idle);
        assert_eq!(retried, Flushed::Saved);
        assert_eq!(after_success, Flushed::Idle);
    }

    #[test]
    fn without_a_store_the_notice_comes_once_for_many_changes() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        let mut results = Vec::new();

        // Act
        for i in 0..10 {
            auto.push(format!("Charts=spend{i}"));
            results.push(auto.flush(after(start, i * 300), false, |_| Ok(false)));
        }

        // Assert
        let notices = results
            .iter()
            .filter(|r| matches!(r, Flushed::Notice(_)))
            .count();
        assert_eq!(notices, 1);
        assert!(matches!(&results[0], Flushed::Notice(n) if n.contains("this run only")));
    }

    #[test]
    fn a_forced_flush_does_not_hammer_a_store_that_keeps_failing() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        auto.push("a".into());
        let mut calls = 0;

        // Act
        let first = auto.flush(start, true, |_| {
            calls += 1;
            Err("locked".into())
        });
        let second = auto.flush(after(start, 1), true, |_| {
            calls += 1;
            Err("locked".into())
        });
        let later = auto.flush(after(start, 300), true, |_| {
            calls += 1;
            Err("locked".into())
        });

        // Assert
        assert_eq!(first, Flushed::Failed("Layout not saved: locked".into()));
        assert_eq!((second, later), (Flushed::Idle, Flushed::Idle));
        assert_eq!(calls, 2);
    }

    #[test]
    fn without_a_store_the_closing_flush_names_the_layout_the_editor_ends_with() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        auto.push("Charts=spend".into());
        let opened = auto.flush(start, false, |_| Ok(false));
        auto.push("Charts=spend,burnup".into());

        // Act
        let closed = auto.flush(after(start, 300), true, |_| Ok(false));

        // Assert
        assert!(matches!(opened, Flushed::Notice(n) if n.contains("'Charts=spend'")));
        assert!(matches!(closed, Flushed::Notice(n) if n.contains("Charts=spend,burnup")));
    }

    #[test]
    fn a_reset_without_a_store_does_not_use_up_the_notice() {
        // Arrange
        let start = Instant::now();
        let mut auto = Autosave::default();
        auto.push(String::new());
        let reset = auto.flush(start, false, |_| Ok(false));
        auto.push("Charts=spend".into());

        // Act
        let edit = auto.flush(after(start, 300), false, |_| Ok(false));

        // Assert
        assert_eq!(reset, Flushed::Saved);
        assert!(matches!(edit, Flushed::Notice(_)));
    }

    #[test]
    fn finish_writes_what_is_pending_once_and_reports_a_failure() {
        // Arrange
        let mut auto = Autosave::default();
        auto.push("a".into());

        // Act
        let failed = auto.finish(|_| Err("disk full".into()));
        let nothing_left = auto.finish(|_| Err("unused".into()));

        // Assert
        assert_eq!(failed, Some("Layout not saved: disk full".into()));
        assert_eq!(nothing_left, None);
    }

    #[test]
    fn resetting_the_layout_clears_the_saved_layout() {
        let home = store("ui-reset-layout");
        save_layout(&home, "Money=spend").unwrap();

        save_layout(&home, "").unwrap();

        assert_eq!(
            load_layout(&home).0,
            super::super::layout::Layout::default()
        );
    }

    #[test]
    fn a_reset_empties_the_layout_and_the_state_and_leaves_the_theme() {
        // Arrange
        let home = store("ui-reset-both");
        save_layout(&home, "Money=spend").unwrap();
        save(&home, &UiState::parse(r#"{"range":"30d"}"#));
        crate::config::save_app(&home, "usage.theme", &Value::from("dark")).unwrap();

        // Act
        let wrote = reset(&home);

        // Assert
        assert_eq!(wrote, Ok(true));
        assert_eq!(load(&home), UiState::default());
        assert_eq!(
            load_layout(&home).0,
            super::super::layout::Layout::default()
        );
        assert_eq!(
            crate::config::resolve(&home, "usage.theme").and_then(|v| v.as_str().map(String::from)),
            Some("dark".to_string())
        );
    }

    #[test]
    fn a_failed_state_write_stops_the_reset_before_the_layout_is_touched() {
        // Arrange
        let mut layout_calls = 0;

        // Act
        let result = reset_with(
            || {
                layout_calls += 1;
                Ok(true)
            },
            || Err("locked".to_string()),
        );

        // Assert
        assert_eq!(result, Err("locked, nothing changed".to_string()));
        assert_eq!(layout_calls, 0);
    }

    #[test]
    fn a_failed_layout_write_names_the_setting_that_was_already_cleared() {
        // Arrange
        let state = || Ok(true);

        // Act
        let result = reset_with(|| Err("locked".to_string()), state);

        // Assert
        let why = result.unwrap_err();
        assert!(why.contains("locked") && why.contains("screen setting was already cleared"));
    }

    #[test]
    fn a_reset_without_a_store_says_so() {
        assert_eq!(reset_with(|| Ok(false), || Ok(false)), Ok(false));
    }

    #[test]
    fn a_reset_drops_the_pending_layout_text() {
        // Arrange
        let mut auto = Autosave::default();
        auto.push("Old=spend".into());

        // Act
        auto.discard();
        let flushed = auto.flush(Instant::now(), true, |_| Err("unused".into()));

        // Assert
        assert_eq!(flushed, Flushed::Idle);
    }

    #[test]
    fn the_no_store_notice_quotes_a_tab_name_with_a_single_quote() {
        let notice = save_notice(Ok(false), "Bob's=spend").unwrap();

        assert!(notice.ends_with("'Bob'\\''s=spend'"), "{notice}");
    }

    #[test]
    fn the_save_notice_is_silent_on_success_and_for_a_reset_without_a_store() {
        assert_eq!(save_notice(Ok(true), "A=spend"), None);
        assert_eq!(save_notice(Ok(false), ""), None);
        assert_eq!(
            save_notice(Err("boom".into()), "A=spend"),
            Some("boom".into())
        );
    }

    #[test]
    fn saving_a_layout_without_a_config_store_reports_that_nothing_was_written() {
        let home = crate::util::scratch_dir("ui-save-nostore");

        let wrote = save_layout(&home, "Money=spend").unwrap();

        assert!(!wrote);
    }
}
