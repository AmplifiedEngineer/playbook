// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A local `usage.db` stand-in. The two sync tables are a documented copy of the
//! contract playbook creates; pbsync only reads and writes them.

use playbook_store::{params, Connection};

const LOCAL_SQL: &str = "
-- sync_state: keys `salt`, `last_success`, `last_error`, plus `failures`.
CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sync_watermarks (table_name TEXT PRIMARY KEY, rowid_mark INTEGER NOT NULL);
CREATE TABLE usage_events (
    event_id TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    account TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_creation_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    agent TEXT NOT NULL DEFAULT 'claude-code',
    machine TEXT NOT NULL DEFAULT ''
);";

pub(crate) const ACCOUNT: &str = "dev@example.com";
pub(crate) const MACHINE: &str = "machine-1";
pub(crate) const SESSION: &str = "session-secret-42";
pub(crate) const BRANCH: &str = "feature/private-work";
pub(crate) const REPO: &str = "/Users/dev/code/secret-project";

pub(crate) fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(LOCAL_SQL).unwrap();
    conn
}

/// Inserts an event for `ACCOUNT` on `MACHINE`.
pub(crate) fn insert(conn: &Connection, event_id: &str, timestamp: i64, output_tokens: i64) {
    insert_for(conn, event_id, timestamp, output_tokens, ACCOUNT, MACHINE);
}

pub(crate) fn insert_for(
    conn: &Connection,
    event_id: &str,
    timestamp: i64,
    output_tokens: i64,
    account: &str,
    machine: &str,
) {
    conn.execute(
        "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort, repo,
            branch, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
            cost_usd, machine)
         VALUES (?1, ?2, ?3, ?4, 'opus', 'high', ?5, ?6, 1, ?7, 2, 3, 0.5, ?8)",
        params![
            event_id,
            timestamp,
            SESSION,
            account,
            REPO,
            BRANCH,
            output_tokens,
            machine
        ],
    )
    .unwrap();
}

pub(crate) fn state(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

pub(crate) fn watermark(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT rowid_mark FROM sync_watermarks WHERE table_name = 'usage_events'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}
