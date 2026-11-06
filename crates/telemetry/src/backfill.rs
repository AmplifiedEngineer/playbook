// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One-time repair of rows stored by the first ingest, which took the repo
//! from the last folder of the working directory and had no cache lifetime
//! split. The transcripts hold the truth, so they are rescanned and the two
//! fields updated by message id. A row whose transcript is gone keeps what it
//! has (five minute cache assumed). The marker lives in `usage_watermarks`.

use super::db;
use super::UsageSource;
use playbook_store::{Connection, Transaction, TransactionBehavior};

const MARKER: &str = "backfill:repo-and-cache-tier-v1";

/// Runs the repair once per database. Safe to call on every ingest: it is a
/// single cheap read after the first run, and two racing callers apply it once.
pub fn run_once(source: &dyn UsageSource, conn: &Connection) -> Result<usize, String> {
    if db::get_watermark(conn, MARKER)? > 0 {
        return Ok(0);
    }
    // Nothing stored yet means nothing to repair, and the first ingest reads
    // every transcript anyway, so skip the second scan.
    if db::count_usage_events(conn)? == 0 {
        db::raise_watermark(conn, MARKER, 1)?;
        return Ok(0);
    }
    // Scan before taking the write lock so other writers are not held up.
    let events = source.events_since(0)?;

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("failed to begin backfill transaction: {e}"))?;
    if db::get_watermark(&tx, MARKER)? > 0 {
        return Ok(0);
    }
    let mut changed = 0;
    for event in &events.usage {
        changed += db::set_repo_and_cache_tier(
            &tx,
            &event.event_id,
            &event.repo,
            event.cache_creation_1h_tokens,
        )?;
    }
    db::raise_watermark(&tx, MARKER, 1)?;
    tx.commit()
        .map_err(|e| format!("failed to commit backfill transaction: {e}"))?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::scratch_dir;
    use crate::{Events, UsageEvent};
    use std::cell::Cell;
    use std::fs;

    struct Fake {
        events: Events,
        scans: Cell<u32>,
    }
    impl UsageSource for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn events_since(&self, _watermark: i64) -> Result<Events, String> {
            self.scans.set(self.scans.get() + 1);
            Ok(self.events.clone())
        }
    }

    fn row(id: &str, repo: &str, one_hour: u64) -> UsageEvent {
        UsageEvent {
            event_id: id.into(),
            timestamp: 10,
            session_id: "s".into(),
            account: "a".into(),
            model: "claude-sonnet-5".into(),
            repo: repo.into(),
            cache_creation_tokens: 100,
            cache_creation_1h_tokens: one_hour,
            ..UsageEvent::default()
        }
    }

    fn fake(events: Vec<UsageEvent>) -> Fake {
        Fake {
            events: Events {
                usage: events,
                tools: vec![],
            },
            scans: Cell::new(0),
        }
    }

    #[test]
    fn it_corrects_matching_rows_once_and_leaves_missing_transcripts_alone() {
        let dir = scratch_dir("usage-backfill");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        db::upsert_usage_event(&conn, &row("kept", "agent-folder", 0)).unwrap();
        db::upsert_usage_event(&conn, &row("orphan", "old-name", 0)).unwrap();
        let source = fake(vec![row("kept", "real-repo", 60)]);

        let first = run_once(&source, &conn).unwrap();
        let second = run_once(&source, &conn).unwrap();

        assert_eq!((first, second), (1, 0));
        assert_eq!(source.scans.get(), 1, "the marker stops a second scan");
        let rows = db::load_usage_events(&conn).unwrap();
        let kept = rows.iter().find(|r| r.event_id == "kept").unwrap();
        let orphan = rows.iter().find(|r| r.event_id == "orphan").unwrap();
        assert_eq!(
            (kept.repo.as_str(), kept.cache_creation_1h_tokens),
            ("real-repo", 60)
        );
        assert_eq!(
            (orphan.repo.as_str(), orphan.cache_creation_1h_tokens),
            ("old-name", 0)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_store_is_marked_without_scanning_transcripts() {
        let dir = scratch_dir("usage-backfill-empty");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        let source = fake(vec![row("x", "r", 0)]);

        let changed = run_once(&source, &conn).unwrap();

        assert_eq!((changed, source.scans.get()), (0, 0));
        assert_eq!(db::get_watermark(&conn, MARKER).unwrap(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
