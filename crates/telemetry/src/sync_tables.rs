// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The local tables sync reads and writes, and the view that merges synced
//! rows from other machines into the local events. Playbook creates them on
//! first open. Until a pull stores rows in `usage_events_remote` the view
//! returns exactly the local events.

use playbook_store::Connection;

/// The account of a synced row whose hash has no alias is this prefix plus the
/// first `UNRESOLVED_HASH_DIGITS` digits of the hash.
const UNRESOLVED_PREFIX: &str = "synced:";
const UNRESOLVED_HASH_DIGITS: usize = 8;
/// Shown for the fields a synced row does not carry.
pub const NOT_SYNCED: &str = "(not synced)";

/// The table the account scope panels read: the merged view once other
/// machines have synced rows, else the local table, which skips the merge.
pub fn account_source(conn: &Connection) -> &'static str {
    if has_remote_rows(conn) {
        "usage_events_all"
    } else {
        "usage_events"
    }
}

/// Whether any other machine has synced events.
pub fn has_remote_rows(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM usage_events_remote)",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// One whole row per event: the larger `output_tokens` wins and the local row
/// wins a tie, then the later push, so columns are never mixed across rows.
pub fn schema_sql() -> String {
    format!(
        "
CREATE TABLE IF NOT EXISTS sync_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_watermarks (
    table_name TEXT PRIMARY KEY,
    rowid_mark INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_cursors (
    machine TEXT NOT NULL,
    table_name TEXT NOT NULL,
    pushed_at INTEGER NOT NULL,
    key TEXT NOT NULL,
    PRIMARY KEY (machine, table_name)
);
CREATE TABLE IF NOT EXISTS usage_events_remote (
    machine TEXT NOT NULL,
    event_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    account TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    repo TEXT NOT NULL DEFAULT '',
    repo_hash TEXT NOT NULL DEFAULT '',
    branch TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_creation_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL,
    pushed_at INTEGER NOT NULL,
    PRIMARY KEY (machine, event_id)
);
CREATE INDEX IF NOT EXISTS usage_events_remote_by_event ON usage_events_remote (event_id);
CREATE TABLE IF NOT EXISTS account_aliases (
    hash TEXT PRIMARY KEY,
    key TEXT NOT NULL
);
CREATE VIEW IF NOT EXISTS usage_events_all AS
SELECT event_id, timestamp, session_id, account, model, effort, repo, branch,
       input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
       cost_usd, cache_creation_1h_tokens, agent, machine
FROM (
    SELECT *, ROW_NUMBER() OVER (
        PARTITION BY event_id
        ORDER BY output_tokens DESC, is_local DESC, pushed_at DESC, machine
    ) AS rank
    FROM (
        SELECT event_id, timestamp, session_id, account, model, effort, repo, branch,
               input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
               cost_usd, cache_creation_1h_tokens, agent, machine, 1 AS is_local,
               0 AS pushed_at
        FROM usage_events
        UNION ALL
        SELECT r.event_id, r.timestamp, '', COALESCE(
                   (SELECT a.key FROM account_aliases a WHERE a.hash = r.account),
                   '{UNRESOLVED_PREFIX}' || lower(substr(r.account, 1, {UNRESOLVED_HASH_DIGITS}))),
               r.model, r.effort, '{NOT_SYNCED}', '{NOT_SYNCED}',
               r.input_tokens, r.output_tokens, r.cache_creation_tokens,
               r.cache_read_tokens, r.cost_usd, r.cache_creation_1h_tokens, '',
               r.machine, 0, r.pushed_at
        FROM usage_events_remote r
    )
) WHERE rank = 1;"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{load_account_events, open_db, upsert_usage_event};
    use crate::UsageEvent;

    fn conn(name: &str) -> Connection {
        open_db(&crate::util::scratch_dir(name).join("usage.db")).unwrap()
    }

    fn remote(c: &Connection, machine: &str, id: &str, account: &str, output: i64, cost: f64) {
        c.execute(
            "INSERT INTO usage_events_remote (machine, event_id, timestamp, account, model,
                 effort, repo_hash, input_tokens, output_tokens, cache_creation_tokens,
                 cache_read_tokens, cost_usd, pushed_at)
             VALUES (?1, ?2, 100, ?3, 'm', '', 'abc', 1, ?4, 0, 0, ?5, 7)",
            (machine, id, account, output, cost),
        )
        .unwrap();
    }

    fn local(c: &Connection, id: &str, output: u64, cost: f64) {
        let e = UsageEvent {
            event_id: id.into(),
            timestamp: 100,
            session_id: "s".into(),
            account: "mine".into(),
            model: "m".into(),
            repo: "r".into(),
            branch: "b".into(),
            output_tokens: output,
            cost_usd: cost,
            ..UsageEvent::default()
        };
        upsert_usage_event(c, &e).unwrap();
    }

    /// The `(output_tokens, cost, machine, account)` of every merged row.
    fn merged(c: &Connection) -> Vec<(i64, f64, String, String)> {
        c.prepare(
            "SELECT output_tokens, cost_usd, machine, account FROM usage_events_all
             ORDER BY event_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn the_view_is_inert_until_rows_are_pulled() {
        // Arrange
        let c = conn("sync-inert");
        local(&c, "e", 5, 1.0);

        // Act
        let (before, source_before) = (merged(&c), account_source(&c));
        remote(&c, "m2", "x", "ab", 1, 1.0);

        // Assert
        assert_eq!(before.len(), 1);
        assert_eq!(source_before, "usage_events");
        assert_eq!(account_source(&c), "usage_events_all");
    }

    #[test]
    fn a_remote_only_row_is_visible_with_its_machine() {
        // Arrange
        let c = conn("sync-remote-only");
        remote(&c, "m2", "x", "ab", 4, 2.0);

        // Act
        let rows = merged(&c);

        // Assert
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].0, rows[0].2.as_str()), (4, "m2"));
        assert_eq!(load_account_events(&c).unwrap().len(), 1);
    }

    #[test]
    fn one_event_on_both_sides_gives_one_whole_row() {
        // (local output, remote output, winning output, remote wins)
        let cases = [(5, 9, 9, true), (9, 5, 9, false), (7, 7, 7, false)];
        for (local_out, remote_out, want_out, remote_wins) in cases {
            // Arrange
            let c = conn(&format!("sync-dup-{local_out}-{remote_out}"));
            let this_machine = crate::machine_rows::local_machine_id(&c).unwrap();
            local(&c, "e", local_out, 1.0);
            remote(&c, "m2", "e", "ab", remote_out as i64, 3.0);

            // Act
            let rows = merged(&c);

            // Assert
            let case = format!("{local_out} vs {remote_out}");
            let (want_machine, want_cost) = if remote_wins {
                ("m2", 3.0)
            } else {
                (this_machine.as_str(), 1.0)
            };
            assert_eq!(rows.len(), 1, "{case}");
            assert_eq!(rows[0].0, want_out, "{case}");
            assert_eq!(rows[0].2, want_machine, "{case}");
            assert_eq!(
                rows[0].1, want_cost,
                "the winner keeps its own cost, {case}"
            );
        }
    }

    #[test]
    fn two_remote_copies_with_different_costs_return_one_whole_row() {
        // Arrange
        let c = conn("sync-two-remote");
        remote(&c, "m2", "e", "ab", 5, 1.0);
        remote(&c, "m3", "e", "ab", 5, 2.0);

        // Act
        let rows = merged(&c);

        // Assert
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1 == 1.0 || rows[0].1 == 2.0);
    }

    #[test]
    fn an_unresolved_hash_shows_eight_lowercase_digits_and_an_alias_resolves_it() {
        // Arrange
        let c = conn("sync-alias");
        remote(&c, "m2", "e", "ABCDEF0123456789", 1, 1.0);

        // Act
        let unresolved = merged(&c)[0].3.clone();
        c.execute(
            "INSERT INTO account_aliases (hash, key) VALUES ('ABCDEF0123456789', 'token:1111aaaa')",
            [],
        )
        .unwrap();
        let resolved = merged(&c)[0].3.clone();

        // Assert
        assert_eq!(unresolved, "synced:abcdef01");
        assert_eq!(resolved, "token:1111aaaa");
    }

    #[test]
    fn a_remote_row_shows_the_placeholders_for_what_it_does_not_carry() {
        // Arrange
        let c = conn("sync-placeholders");
        remote(&c, "m2", "e", "ab", 1, 1.0);

        // Act
        let e = &load_account_events(&c).unwrap()[0];

        // Assert
        assert_eq!(
            (e.repo.as_str(), e.branch.as_str()),
            (NOT_SYNCED, NOT_SYNCED)
        );
        assert_eq!((e.session_id.as_str(), e.agent.as_str()), ("", ""));
    }
}
