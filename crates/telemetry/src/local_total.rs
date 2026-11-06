// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What this machine spent this month, read from the usage database for the
//! statusline. The statusline renders on every prompt, so the total is kept in
//! a small file for [`LOCAL_TOTAL_CACHE_SECONDS`], the database is opened read
//! only and never created, and any failure (missing, locked, unreadable) is
//! `None` so the caller shows what it shows without a local figure.

use crate::db;
use crate::limits::{Limit, LimitPeriod};
use crate::query::WeekStart;
use playbook_store::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a stored total is trusted before the database is read again.
pub const LOCAL_TOTAL_CACHE_SECONDS: i64 = 10;

/// The month is the same calendar month pbtop sums for a manual budget.
const PERIOD: LimitPeriod = LimitPeriod::Monthly;

/// Where the month's cost is read from and where the short lived copy is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTotal {
    /// The usage database. It is opened read only and never created.
    pub db: PathBuf,
    /// The file holding the last total, or `None` to always read the database.
    pub cache: Option<PathBuf>,
}

impl LocalTotal {
    /// Dollars spent on this machine from the start of the local month
    /// through `now`, or `None` when the database cannot be read right now.
    pub fn month_used(&self, now: i64) -> Option<f64> {
        let (month_start, _) = PERIOD.containing(now, WeekStart::default());
        if let Some(total) = self.cached(now, month_start) {
            return Some(total);
        }
        let total = read_total(&self.db, now, month_start)?;
        self.store(now, total);
        Some(total)
    }

    fn cached(&self, now: i64, month_start: i64) -> Option<f64> {
        let text = std::fs::read_to_string(self.cache.as_ref()?).ok()?;
        let (stamp, total) = text.trim().split_once(' ')?;
        let stamp: i64 = stamp.parse().ok()?;
        let total: f64 = total.parse().ok()?;
        let fresh = (0..LOCAL_TOTAL_CACHE_SECONDS).contains(&(now - stamp));
        (fresh && stamp >= month_start && total.is_finite()).then_some(total)
    }

    fn store(&self, now: i64, total: f64) {
        if let Some(path) = &self.cache {
            let _ = std::fs::write(path, format!("{now} {total}"));
        }
    }
}

fn read_total(path: &Path, now: i64, month_start: i64) -> Option<f64> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).ok()?;
    conn.busy_timeout(Duration::ZERO).ok()?;
    let events = db::load_usage_events_since(&conn, month_start).ok()?;
    Some(Limit::spent(PERIOD, &events, now, WeekStart::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UsageEvent;

    const NOW: i64 = 1_788_400_000;
    const OPUS: &str = "claude-opus-4-1";

    fn month_start() -> i64 {
        PERIOD.containing(NOW, WeekStart::default()).0
    }

    fn event(id: &str, timestamp: i64, account: &str) -> UsageEvent {
        UsageEvent {
            event_id: id.to_string(),
            timestamp,
            account: account.to_string(),
            model: OPUS.to_string(),
            agent: "claude-code".to_string(),
            input_tokens: 1_000_000,
            ..UsageEvent::default()
        }
    }

    fn priced(timestamp: i64) -> f64 {
        let mut e = event("p", timestamp, "a");
        e.apply_pricing();
        e.cost_usd
    }

    fn store(tag: &str, events: &[UsageEvent]) -> (PathBuf, LocalTotal) {
        let dir = crate::util::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.db");
        let conn = db::open_db(&path).unwrap();
        for e in events {
            db::upsert_usage_event(&conn, e).unwrap();
        }
        let local = LocalTotal {
            db: path,
            cache: None,
        };
        (dir, local)
    }

    #[test]
    fn it_sums_the_month_so_far_across_accounts() {
        let (_, local) = store(
            "lt-sum",
            &[
                event("a", NOW - 100, "work"),
                event("b", NOW - 50, "personal"),
            ],
        );
        let each = priced(NOW);
        assert!(each > 0.0);
        assert_eq!(local.month_used(NOW), Some(each * 2.0));
    }

    #[test]
    fn the_month_starts_at_local_midnight_of_the_first_day() {
        let start = month_start();
        let (_, local) = store(
            "lt-boundary",
            &[
                event("last-month", start - 1, "a"),
                event("first-second", start, "a"),
                event("future", NOW + 1, "a"),
            ],
        );
        assert_eq!(local.month_used(NOW), Some(priced(NOW)));
    }

    #[test]
    fn a_missing_database_gives_nothing_and_is_not_created() {
        let dir = crate::util::scratch_dir("lt-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let local = LocalTotal {
            db: dir.join("usage.db"),
            cache: None,
        };
        assert_eq!(local.month_used(NOW), None);
        assert!(!dir.join("usage.db").exists());
    }

    #[test]
    fn a_file_that_is_not_a_database_gives_nothing() {
        let dir = crate::util::scratch_dir("lt-garbage");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("usage.db"), "not a database at all").unwrap();
        let local = LocalTotal {
            db: dir.join("usage.db"),
            cache: None,
        };
        assert_eq!(local.month_used(NOW), None);
    }

    #[test]
    fn a_locked_database_gives_nothing_without_waiting() {
        let (_, local) = store("lt-locked", &[event("a", NOW - 10, "a")]);
        let writer = Connection::open(&local.db).unwrap();
        writer
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let began = std::time::Instant::now();
        assert_eq!(local.month_used(NOW), None);
        assert!(began.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_fresh_stored_total_is_used_without_opening_the_database() {
        let dir = crate::util::scratch_dir("lt-hit");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("local-total");
        std::fs::write(&cache, format!("{} 412.5", NOW - 9)).unwrap();
        let local = LocalTotal {
            db: dir.join("no-such.db"),
            cache: Some(cache),
        };
        assert_eq!(local.month_used(NOW), Some(412.5));
    }

    #[test]
    fn a_read_stores_the_total_and_an_old_one_is_read_again() {
        let (dir, mut local) = store("lt-store", &[event("a", NOW - 10, "a")]);
        let cache = dir.join("local-total");
        local.cache = Some(cache.clone());
        let spent = priced(NOW);
        assert_eq!(local.month_used(NOW), Some(spent));
        assert_eq!(
            std::fs::read_to_string(&cache).unwrap(),
            format!("{NOW} {spent}")
        );
        std::fs::write(&cache, format!("{} 999", NOW - LOCAL_TOTAL_CACHE_SECONDS)).unwrap();
        assert_eq!(local.month_used(NOW), Some(spent));
    }

    #[test]
    fn a_stored_total_from_last_month_or_the_future_is_ignored() {
        let (dir, mut local) = store("lt-bad-cache", &[event("a", NOW - 10, "a")]);
        let cache = dir.join("local-total");
        local.cache = Some(cache.clone());
        for stamp in [month_start() - 1, NOW + 1] {
            std::fs::write(&cache, format!("{stamp} 999")).unwrap();
            assert_eq!(local.month_used(NOW), Some(priced(NOW)), "{stamp}");
        }
    }

    #[test]
    fn a_garbled_stored_total_is_read_from_the_database() {
        let (dir, mut local) = store("lt-garbled", &[event("a", NOW - 10, "a")]);
        let cache = dir.join("local-total");
        local.cache = Some(cache.clone());
        std::fs::write(&cache, "soon").unwrap();
        assert_eq!(local.month_used(NOW), Some(priced(NOW)));
    }
}
