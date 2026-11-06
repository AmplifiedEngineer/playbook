// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `config.db`: all configuration for all apps, in `<data dir>/config.db`.
//!
//! Rows are `(tier, scope, key) -> value_json`. The tiers and their scopes:
//!
//! | tier     | scope                                              |
//! |----------|----------------------------------------------------|
//! | `global` | empty                                              |
//! | `org`    | the owner of the repo slug                         |
//! | `repo`   | the whole `owner/repo` slug                        |
//! | `app`    | the product name, for example `playbook-telemetry` |
//!
//! [`get_in`] resolves a key most specific first: repo, org, app, global. The
//! caller applies its own default when it returns `None`. The schema is the
//! one the former `playbook.db` had, so a table copy needs no conversion.

use playbook_store::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The schema every writer applies.
pub const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS config (
    tier TEXT NOT NULL,
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (tier, scope, key)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;";

/// The `app` scope of each tool. Product names, never an alias such as
/// `pbtop`, so an alias cannot split one tool's configuration in two.
pub const APP_PLAYBOOK: &str = "playbook";
pub const APP_TELEMETRY: &str = "playbook-telemetry";
pub const APP_CODE: &str = "playbook-code";

/// `<data>/config.db`.
pub fn db_in(data: &Path) -> PathBuf {
    data.join("config.db")
}

/// `<root>/playbook.db`: where earlier versions kept the same tables, next to
/// the data folder.
pub fn legacy_db_in(root: &Path) -> PathBuf {
    root.join("playbook.db")
}

/// The raw JSON text of `key` in the most specific tier that sets it, read
/// from the database at `db`, in one query so the answer comes from one
/// snapshot. `repo_slug` is `owner/repo` (a slug without a `/` matches no
/// repo and no org); `app` is a product name (empty means none). A stored
/// JSON `null` is a value and shadows the lower tiers. A missing database or
/// table is `None`, never an error.
pub fn get_in(
    db: &Path,
    key: &str,
    repo_slug: Option<&str>,
    app: Option<&str>,
) -> playbook_store::Result<Option<String>> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(2000))?;
    let split = repo_slug.and_then(|s| s.split_once('/'));
    let repo = split.map(|_| repo_slug.unwrap_or_default());
    let org = split.map(|(o, _)| o);
    let app = app.filter(|a| !a.is_empty());
    let found = conn
        .query_row(
            "SELECT value_json FROM config WHERE key = ?1 AND (\
                (tier = 'repo' AND scope = ?2) OR (tier = 'org' AND scope = ?3) \
                OR (tier = 'app' AND scope = ?4) OR (tier = 'global' AND scope = '')) \
             ORDER BY CASE tier WHEN 'repo' THEN 0 WHEN 'org' THEN 1 WHEN 'app' THEN 2 ELSE 3 END \
             LIMIT 1",
            (key, repo, org, app),
            |r| r.get::<_, String>(0),
        )
        .optional();
    match found {
        Ok(v) => Ok(v),
        // The file exists but was never initialised.
        Err(playbook_store::Error::SqliteFailure(_, Some(m))) if m.contains("no such table") => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Writes `value_json` for `key` in the `app` tier of `app`, in the existing
/// database at `db`. Returns `false` and writes nothing when the database or
/// its `config` table does not exist, so a tool never creates the store and
/// hides an older one. One statement, so the write is atomic.
pub fn set_app_in(
    db: &Path,
    key: &str,
    app: &str,
    value_json: &str,
    now: i64,
) -> playbook_store::Result<bool> {
    if !db.is_file() {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Best effort: a short wait, so a busy store never stalls a screen.
    conn.busy_timeout(Duration::from_millis(250))?;
    let result = conn.execute(
        "INSERT OR REPLACE INTO config (tier, scope, key, value_json, updated_at) \
         VALUES ('app', ?1, ?2, ?3, ?4)",
        (app, key, value_json, now),
    );
    match result {
        Ok(_) => Ok(true),
        Err(playbook_store::Error::SqliteFailure(_, Some(m))) if m.contains("no such table") => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("pbdata-cfg-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).expect("dir");
        db_in(&d)
    }

    fn put(db: &Path, tier: &str, scope: &str, key: &str, v: &str) {
        let c = Connection::open(db).expect("open");
        c.execute_batch(SCHEMA_SQL).expect("schema");
        c.execute(
            "INSERT OR REPLACE INTO config VALUES (?1, ?2, ?3, ?4, 1)",
            (tier, scope, key, v),
        )
        .expect("insert");
    }

    fn get(db: &Path, repo: Option<&str>, app: Option<&str>) -> Option<String> {
        get_in(db, "k", repo, app).expect("read")
    }

    #[test]
    fn missing_or_uninitialised_database_is_none() {
        let db = temp("missing");
        assert_eq!(get(&db, None, None), None);
        drop(Connection::open(&db).expect("create"));
        assert_eq!(get(&db, Some("o/r"), Some("a")), None);
    }

    #[test]
    fn precedence_is_repo_org_app_global() {
        let db = temp("prec");
        put(&db, "global", "", "k", "\"g\"");
        assert_eq!(get(&db, Some("o/r"), Some("a")).as_deref(), Some("\"g\""));
        put(&db, "app", "a", "k", "\"app\"");
        assert_eq!(get(&db, Some("o/r"), Some("a")).as_deref(), Some("\"app\""));
        put(&db, "org", "o", "k", "\"org\"");
        assert_eq!(get(&db, Some("o/r"), Some("a")).as_deref(), Some("\"org\""));
        put(&db, "repo", "o/r", "k", "\"repo\"");
        assert_eq!(
            get(&db, Some("o/r"), Some("a")).as_deref(),
            Some("\"repo\"")
        );
        // Another app and another repo do not see them.
        assert_eq!(get(&db, None, Some("b")).as_deref(), Some("\"g\""));
        assert_eq!(get(&db, Some("x/y"), None).as_deref(), Some("\"g\""));
    }

    #[test]
    fn a_bare_slug_and_an_empty_app_match_nothing() {
        let db = temp("bare");
        put(&db, "org", "o", "k", "\"org\"");
        put(&db, "repo", "o", "k", "\"badrepo\"");
        put(&db, "app", "", "k", "\"emptyapp\"");
        assert_eq!(get(&db, Some("o"), Some("")), None);
    }

    #[test]
    fn json_null_is_a_value_that_shadows_lower_tiers() {
        let db = temp("null");
        put(&db, "global", "", "k", "\"g\"");
        put(&db, "repo", "o/r", "k", "null");
        assert_eq!(get(&db, Some("o/r"), None).as_deref(), Some("null"));
    }

    #[test]
    fn a_reader_does_not_block_on_an_open_write_transaction() {
        let db = temp("contend");
        let c = Connection::open(&db).expect("open");
        c.pragma_update(None, "journal_mode", "WAL").expect("wal");
        drop(c);
        put(&db, "global", "", "k", "\"committed\"");
        let w = Connection::open(&db).expect("writer");
        w.execute_batch(
            "BEGIN IMMEDIATE; INSERT OR REPLACE INTO config VALUES ('global','','k','\"pending\"',2);",
        )
        .expect("begin");
        assert_eq!(get(&db, None, None).as_deref(), Some("\"committed\""));
        w.execute_batch("ROLLBACK").expect("rollback");
    }

    #[test]
    fn the_legacy_schema_reads_unchanged() {
        let db = temp("golden");
        let c = Connection::open(&db).expect("open");
        c.execute_batch(
            "CREATE TABLE config (tier TEXT NOT NULL, scope TEXT NOT NULL, key TEXT NOT NULL,
               value_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
               PRIMARY KEY (tier, scope, key)) WITHOUT ROWID;
             INSERT INTO config VALUES ('global','','k','true',1);",
        )
        .expect("legacy");
        drop(c);
        assert_eq!(get(&db, None, Some(APP_TELEMETRY)).as_deref(), Some("true"));
    }

    #[test]
    fn set_app_in_writes_the_app_tier_and_get_in_reads_it_back() {
        let db = temp("setapp");
        put(&db, "global", "", "k", "\"g\"");

        let wrote = set_app_in(&db, "k", "a", "\"mine\"", 5).expect("write");

        assert!(wrote);
        assert_eq!(get(&db, None, Some("a")), Some("\"mine\"".to_string()));
        assert_eq!(get(&db, None, Some("other")), Some("\"g\"".to_string()));
    }

    #[test]
    fn set_app_in_does_not_create_a_missing_database() {
        let db = temp("setmissing");

        let wrote = set_app_in(&db, "k", "a", "1", 5).expect("write");

        assert!(!wrote);
        assert!(!db.exists());
    }

    #[test]
    fn set_app_in_reports_false_when_the_config_table_does_not_exist() {
        let db = temp("setnotable");
        drop(Connection::open(&db).expect("create"));

        let wrote = set_app_in(&db, "k", "a", "1", 5).expect("write");

        assert!(!wrote);
    }
}
