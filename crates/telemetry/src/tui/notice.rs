// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The messages that replace the panels when there is nothing to chart: the
//! first read has not finished or failed, the store is empty, or the range
//! and filter match nothing. Each says what is going on and what to press.

use super::app::App;
use super::data::Data;
use crate::aggregate::date_key;
use crate::query::day_of;

/// A message for the body of the screen.
#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
    /// Drawn in the error color.
    pub error: bool,
}

fn day(ts: i64) -> String {
    date_key(day_of(ts))
}

/// Whether the snapshot has nothing to draw for the range and filter.
fn is_empty(d: &Data) -> bool {
    d.totals.messages == 0 && d.live.active.is_empty()
}

/// The message that takes the whole body, if one applies.
pub fn body(app: &App) -> Option<Notice> {
    let Some(d) = &app.data else {
        return Some(match &app.error {
            Some(e) => read_failure(app, e),
            None => Notice {
                title: "Reading usage".into(),
                lines: vec![if app.sources.is_empty() {
                    "Reading usage...".to_string()
                } else {
                    format!("Reading usage from {} ...", app.sources)
                }],
                error: false,
            },
        });
    };
    if !is_empty(d) {
        return None;
    }
    if let Some((done, total)) = app.indexing {
        return Some(Notice {
            title: "Indexing".into(),
            lines: vec![
                format!("Reading transcripts from {}", app.sources),
                format!("{done} of {total} files read. Data will appear as it is read."),
            ],
            error: false,
        });
    }
    if d.store.events == 0 {
        return Some(empty_store(app));
    }
    Some(empty_range(app, d))
}

fn read_failure(app: &App, error: &str) -> Notice {
    Notice {
        title: "Could not read usage".into(),
        lines: vec![
            error.to_string(),
            String::new(),
            format!("Transcripts are read from {}", app.sources),
            format!(
                "Run `{}` to see the same error in plain text.",
                crate::host::command("usage --summary")
            ),
            "The view retries on its own; press q to quit.".into(),
        ],
        error: true,
    }
}

fn empty_store(app: &App) -> Notice {
    Notice {
        title: "No usage recorded yet".into(),
        lines: vec![
            "The usage store has no messages.".into(),
            String::new(),
            format!("Transcripts are read from {}", app.sources),
            "To check: list that folder, it should hold one folder per project with .jsonl files."
                .into(),
            "If Claude Code keeps its history elsewhere, set CLAUDE_CONFIG_DIR before starting."
                .into(),
            format!(
                "`{}` reads the same places and prints what it finds.",
                crate::host::command("usage --summary")
            ),
        ],
        error: false,
    }
}

fn empty_range(app: &App, d: &Data) -> Notice {
    let (Some(first), Some(last)) = (d.store.first, d.store.last) else {
        return empty_store(app);
    };
    let mut lines = Vec::new();
    if d.filter.is_empty() {
        lines.push(format!("No messages in {}.", d.range.label()));
    } else {
        lines.push(format!(
            "No messages match the filter `{}` in {}.",
            d.filter,
            d.range.label()
        ));
    }
    lines.push(format!(
        "The store has {} messages from {} to {}.",
        d.store.events,
        day(first),
        day(last)
    ));
    lines.push(String::new());
    if !d.filter.is_empty() {
        lines.push("Press c to clear the filter.".into());
    }
    lines.push("Press r to step to a longer range.".into());
    Notice {
        title: "No usage in this range".into(),
        lines,
        error: false,
    }
}

/// A read failure while old data is still on screen: shown over the body so it
/// is seen, while the numbers below stay as the last good read.
pub fn banner(app: &App) -> Option<Notice> {
    let e = app.error.as_ref()?;
    app.data.as_ref()?;
    Some(Notice {
        title: "Refresh failed".into(),
        lines: vec![
            e.clone(),
            "The numbers below are from the last good read.".into(),
        ],
        error: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{LiveView, Range};
    use crate::tui::data::StoreSpan;
    use crate::UsageEvent;

    const NOW: i64 = 1_788_400_000;

    fn app(events: &[UsageEvent], store: StoreSpan, range: Range) -> App {
        let mut app = App::new(range);
        app.sources = "/fake/home/.claude/projects".into();
        let mut d = Data::build(events, LiveView::default(), NOW, range, "");
        d.store = store;
        app.data = Some(d);
        app
    }

    fn text(n: &Notice) -> String {
        format!("{}\n{}", n.title, n.lines.join("\n"))
    }

    #[test]
    fn an_empty_store_says_where_transcripts_are_read_from() {
        let n = body(&app(&[], StoreSpan::default(), Range::Today)).unwrap();
        let t = text(&n);
        assert!(t.contains("No usage recorded yet"), "{t}");
        assert!(t.contains("/fake/home/.claude/projects"), "{t}");
        assert!(t.contains("CLAUDE_CONFIG_DIR"), "{t}");
        assert!(t.contains("--summary"), "{t}");
    }

    #[test]
    fn while_indexing_an_empty_view_says_data_will_appear() {
        let mut a = app(&[], StoreSpan::default(), Range::Today);
        a.indexing = Some((120, 800));
        let t = text(&body(&a).unwrap());
        assert!(t.contains("120 of 800 files"), "{t}");
        assert!(t.contains("Data will appear"), "{t}");
        assert!(!t.contains("No usage"), "{t}");
    }

    #[test]
    fn an_empty_range_with_data_elsewhere_names_the_dates_and_the_key() {
        let store = StoreSpan {
            events: 2,
            first: Some(NOW - 40 * 86_400),
            last: Some(NOW - 30 * 86_400),
        };
        let n = body(&app(&[], store, Range::Today)).unwrap();
        let t = text(&n);
        assert!(t.contains("No messages in today"), "{t}");
        assert!(t.contains("2 messages from 2026-07-"), "{t}");
        assert!(t.contains("Press r to step to a longer range"), "{t}");
    }

    #[test]
    fn a_filter_with_no_match_says_so_and_how_to_clear_it() {
        let store = StoreSpan {
            events: 5,
            first: Some(NOW - 86_400),
            last: Some(NOW),
        };
        let mut a = app(&[], store, Range::All);
        let d = a.data.as_mut().unwrap();
        d.filter = "nomatch".into();
        let t = text(&body(&a).unwrap());
        assert!(t.contains("filter `nomatch`"), "{t}");
        assert!(t.contains("Press c to clear"), "{t}");
    }

    #[test]
    fn a_range_with_rows_has_no_notice() {
        let e = UsageEvent {
            event_id: "e".into(),
            timestamp: NOW - 60,
            session_id: "s".into(),
            model: "m".into(),
            ..UsageEvent::default()
        };
        let store = StoreSpan {
            events: 1,
            first: Some(NOW - 60),
            last: Some(NOW - 60),
        };
        assert_eq!(body(&app(&[e], store, Range::All)), None);
    }

    #[test]
    fn a_failed_first_read_fills_the_body_with_the_error() {
        let mut a = App::new(Range::Today);
        a.sources = "/fake/projects".into();
        a.error = Some("database is locked".into());
        let n = body(&a).unwrap();
        assert!(n.error);
        assert!(text(&n).contains("database is locked"));
        assert!(text(&n).contains("/fake/projects"));
        assert_eq!(banner(&a), None);
    }

    #[test]
    fn a_failed_refresh_over_old_data_shows_a_banner() {
        let mut a = app(&[], StoreSpan::default(), Range::All);
        a.error = Some("disk I/O error".into());
        assert!(text(&banner(&a).unwrap()).contains("disk I/O error"));
    }
}
