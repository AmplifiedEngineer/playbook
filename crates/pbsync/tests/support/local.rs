// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A local `usage.db` stand-in. The two sync tables are a documented copy of the
//! contract playbook creates; pbsync only reads and writes them.

// Each test binary uses a different part of these helpers.
#![allow(dead_code)]

use playbook_store::{params, Connection};

const LOCAL_SQL: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta VALUES ('schema_version', '5');
CREATE TABLE machine (
    id INTEGER PRIMARY KEY CHECK (id = 1), machine_id TEXT NOT NULL, name TEXT
);
-- sync_state: keys `salt`, `last_success`, `last_error`, plus `failures`.
CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sync_cursors (
    machine TEXT NOT NULL, table_name TEXT NOT NULL, pushed_at INTEGER NOT NULL,
    key TEXT NOT NULL, PRIMARY KEY (machine, table_name)
);
CREATE TABLE usage_events_remote (
    machine TEXT NOT NULL, event_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
    account TEXT NOT NULL, model TEXT NOT NULL, effort TEXT NOT NULL,
    repo TEXT NOT NULL DEFAULT '', repo_hash TEXT NOT NULL DEFAULT '',
    branch TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL, cache_creation_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL, pushed_at INTEGER NOT NULL,
    PRIMARY KEY (machine, event_id)
);
CREATE INDEX usage_events_remote_event ON usage_events_remote (event_id);
CREATE TABLE account_aliases (hash TEXT PRIMARY KEY, key TEXT NOT NULL);
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

/// The same schema in a file, so a second connection can write alongside.
pub(crate) fn open_at(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(LOCAL_SQL).unwrap();
    conn
}

/// Sets the `usage.db` version the database claims.
pub(crate) fn stamp(conn: &Connection, version: i64) {
    conn.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
        [version.to_string()],
    )
    .unwrap();
}

/// The optional `account_names(key, name)` table.
pub(crate) fn add_account_names(conn: &Connection, key: &str, name: &str) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS account_names (key TEXT PRIMARY KEY, name TEXT NOT NULL)",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_names (key, name) VALUES (?1, ?2)",
        [key, name],
    )
    .unwrap();
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
