// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which credential each session used. The transcripts do not say, and
//! `~/.claude.json` names only the account Claude Code is signed in to, which
//! a `CLAUDE_CODE_OAUTH_TOKEN` in the environment overrides. A session start
//! hook sees that environment, so it appends one line per session to a small
//! log, with the time of the line. Ingest reads the log into the
//! `session_account_spans` table and labels each event with the account of the
//! latest span that began at or before the event, so a session that moves from
//! one account to another is split at the move.
//!
//! The token is never stored, logged or printed. Only a fingerprint is: the
//! first 8 hex digits of a SHA-256 over a fixed prefix and the token, with
//! the prefix `token:`. It cannot be turned back into the token.

use crate::account_store::err;
use playbook_store::Connection;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS session_accounts (
    session_id TEXT PRIMARY KEY,
    account TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_account_spans (
    session_id TEXT NOT NULL,
    since INTEGER NOT NULL,
    account TEXT NOT NULL,
    PRIMARY KEY (session_id, since)
);";

/// The watermark that marks the one-time copy of `session_accounts` rows.
const SPANS_COPIED: &str = "session-account-spans-copied";
/// The start of a span with no recorded time: the whole session.
const LEGACY_SINCE: i64 = 0;
const LOG_FIELD_SEP: char = '\t';

/// The variable that holds a long lived login token.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

const LOG: &str = "session-accounts.log";
const OFFSET: &str = "session-accounts-offset";
/// The log marker for a session that used the signed in account.
const SIGNED_IN: &str = "-";
const DOMAIN: &str = "playbook-account-v1:";

/// `token:` and 8 hex digits that identify `token` without revealing it.
pub fn fingerprint(token: &str) -> String {
    let digest = Sha256::digest(format!("{DOMAIN}{token}").as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("token:{hex}")
}

/// The fingerprint of the token in this environment, if one is set.
pub fn env_fingerprint() -> Option<String> {
    std::env::var(TOKEN_ENV)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|t| fingerprint(&t))
}

fn plain(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Appends the account of `session_id` to the log in `dir`, stamped with the
/// unix time `at`: the fingerprint when one is given, else the signed in
/// marker. Never fails the caller.
pub fn record(dir: &Path, session_id: &str, fingerprint: Option<&str>, at: i64) {
    let key = fingerprint.unwrap_or(SIGNED_IN);
    if !plain(session_id) || !plain(key) {
        return;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(dir.join(LOG)) {
        let _ = file.write_all(format!("{session_id}\t{key}\t{at}\n").as_bytes());
    }
}

/// Reads the log lines added since the last import into `session_accounts`.
pub fn import(conn: &Connection, dir: &Path) -> Result<usize, String> {
    let Ok(bytes) = std::fs::read(dir.join(LOG)) else {
        return Ok(0);
    };
    let from = usize::try_from(crate::db::get_watermark(conn, OFFSET)?).unwrap_or(0);
    // Only whole lines: a writer may be mid-append.
    let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if from >= end {
        return Ok(0);
    }
    let mut imported = 0;
    for line in String::from_utf8_lossy(&bytes[from..end]).lines() {
        let mut fields = line.split(LOG_FIELD_SEP);
        let (Some(session), Some(key)) = (fields.next(), fields.next()) else {
            continue;
        };
        let saved = match fields.next().map(str::parse::<i64>) {
            None => save_legacy(conn, session, key),
            Some(Ok(since)) => save_span(conn, session, key, since),
            Some(Err(_)) => continue,
        };
        saved.map_err(|e| err("save a session account", e))?;
        imported += 1;
    }
    crate::db::raise_watermark(conn, OFFSET, i64::try_from(end).unwrap_or(i64::MAX))?;
    Ok(imported)
}

/// A line from before lines were timed: it replaces the whole-session span.
fn save_legacy(conn: &Connection, session: &str, key: &str) -> playbook_store::Result<usize> {
    if key == SIGNED_IN {
        conn.execute(
            "DELETE FROM session_account_spans WHERE session_id = ?1 AND since = ?2",
            (session, LEGACY_SINCE),
        )
    } else {
        save_span(conn, session, key, LEGACY_SINCE)
    }
}

/// A timed line starts a span. The signed in marker starts none: it names no
/// label, and events outside every span take the signed in account anyway.
fn save_span(
    conn: &Connection,
    session: &str,
    key: &str,
    since: i64,
) -> playbook_store::Result<usize> {
    if key == SIGNED_IN {
        return Ok(0);
    }
    conn.execute(
        "INSERT INTO session_account_spans (session_id, since, account) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id, since) DO UPDATE SET account = excluded.account",
        (session, since, key),
    )
}

/// The columns the joined-key repair rewrites.
const ACCOUNT_COLUMNS: [(&str, &str); 5] = [
    ("usage_events", "account"),
    ("tool_invocation_events", "account"),
    ("session_accounts", "account"),
    ("session_account_spans", "account"),
    ("account_assignment_rows", "previous"),
];

/// SQL condition for a key shaped `head<TAB>digits`: a non-empty head and a
/// non-empty, all-digit tail after the first separator.
fn repairable(column: &str) -> String {
    format!(
        "instr({column}, char(?1)) > 1
         AND substr({column}, instr({column}, char(?1)) + 1) != ''
         AND substr({column}, instr({column}, char(?1)) + 1) NOT GLOB '*[^0-9]*'"
    )
}

/// Strips the `<TAB><digits>` tail that an old two-field read of a timed log
/// line left on stored keys. Returns how many rows it rewrote.
pub fn repair_joined_keys(conn: &Connection) -> Result<usize, String> {
    let fail = |e| err("repair the joined account keys", e);
    let sep = u32::from(LOG_FIELD_SEP);
    let broken: bool = conn
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM session_accounts WHERE {})
                     OR EXISTS (SELECT 1 FROM session_account_spans WHERE {})",
                repairable("account"),
                repairable("account")
            ),
            [sep],
            |r| r.get(0),
        )
        .map_err(fail)?;
    if !broken {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction().map_err(fail)?;
    let mut repaired = 0;
    for (table, column) in ACCOUNT_COLUMNS {
        repaired += tx
            .execute(
                &format!(
                    "UPDATE {table} SET {column} = substr({column}, 1, instr({column}, char(?1)) - 1)
                     WHERE {}",
                    repairable(column)
                ),
                [sep],
            )
            .map_err(fail)?;
    }
    tx.commit().map_err(fail)?;
    Ok(repaired)
}

/// Copies the rows of the old one-label-per-session table into spans that
/// cover the whole session. It runs once, so a span later removed stays gone.
pub fn copy_legacy_rows(conn: &Connection) -> Result<(), String> {
    if crate::db::get_watermark(conn, SPANS_COPIED)? > 0 {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO session_account_spans (session_id, since, account)
         SELECT session_id, ?1, account FROM session_accounts",
        [LEGACY_SINCE],
    )
    .map_err(|e| err("copy the session accounts into spans", e))?;
    crate::db::raise_watermark(conn, SPANS_COPIED, 1)
}

/// The spans of one session, oldest first.
pub struct Spans(Vec<(i64, String)>);

impl Spans {
    /// The account of the latest span that began at or before `ts`, else of
    /// the earliest span, else none.
    pub fn account_at(&self, ts: i64) -> Option<&str> {
        let started = self.0.partition_point(|(since, _)| *since <= ts);
        self.0
            .get(started.saturating_sub(1))
            .map(|(_, account)| account.as_str())
    }
}

/// The spans recorded for `session_id`.
pub fn spans_of(conn: &Connection, session_id: &str) -> Result<Spans, String> {
    let read = |e| err("read a session account", e);
    let mut stmt = conn
        .prepare(
            "SELECT since, account FROM session_account_spans
             WHERE session_id = ?1 ORDER BY since",
        )
        .map_err(read)?;
    let rows = stmt
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(read)?;
    Ok(Spans(rows.collect::<Result<_, _>>().map_err(read)?))
}

/// The account of `session_id` at time `ts`, if its credential was recorded.
pub fn of_session_at(
    conn: &Connection,
    session_id: &str,
    ts: i64,
) -> Result<Option<String>, String> {
    Ok(spans_of(conn, session_id)?
        .account_at(ts)
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_db;
    use crate::util::scratch_dir;

    const SENTINEL: &str = "sk-ant-oat01-SENTINEL-0123456789-secret";

    #[test]
    fn a_fingerprint_is_stable_short_and_differs_per_token() {
        let a = fingerprint("one");
        assert_eq!(a, fingerprint("one"));
        assert_ne!(a, fingerprint("two"));
        assert!(
            a.starts_with("token:") && a.len() == "token:".len() + 8,
            "{a}"
        );
        // A fixed vector: a change here would split every stored account.
        assert_eq!(fingerprint("abc"), "token:4243ddb1");
    }

    fn account(conn: &Connection, session: &str, ts: i64) -> Option<String> {
        of_session_at(conn, session, ts).unwrap()
    }

    #[test]
    fn a_session_is_labeled_by_its_recorded_credential() {
        let dir = scratch_dir("session-account");
        let conn = open_db(&dir.join("u.db")).unwrap();
        record(&dir, "s1", Some("token:aaaa1111"), 10);
        record(&dir, "s2", None, 10);
        assert_eq!(import(&conn, &dir).unwrap(), 2);
        assert_eq!(account(&conn, "s1", 99).as_deref(), Some("token:aaaa1111"));
        assert_eq!(account(&conn, "s2", 99), None);
        assert_eq!(import(&conn, &dir).unwrap(), 0);
    }

    #[test]
    fn a_session_moves_to_the_account_in_effect_at_each_time() {
        let dir = scratch_dir("session-account-move");
        let conn = open_db(&dir.join("u.db")).unwrap();
        record(&dir, "s1", Some("token:aaaa1111"), 0);
        record(&dir, "s1", Some("token:bbbb2222"), 250);
        import(&conn, &dir).unwrap();
        let at = |ts| account(&conn, "s1", ts).unwrap();
        assert_eq!(at(200), "token:aaaa1111");
        assert_eq!(at(250), "token:bbbb2222");
        assert_eq!(at(400), "token:bbbb2222");
    }

    #[test]
    fn an_event_before_the_first_span_takes_the_earliest_span() {
        let dir = scratch_dir("session-account-early");
        let conn = open_db(&dir.join("u.db")).unwrap();
        record(&dir, "s1", Some("token:aaaa1111"), 500);
        record(&dir, "s1", Some("token:bbbb2222"), 900);
        import(&conn, &dir).unwrap();
        assert_eq!(account(&conn, "s1", 100).as_deref(), Some("token:aaaa1111"));
    }

    #[test]
    fn a_legacy_two_field_line_labels_the_whole_session_and_a_later_one_replaces_it() {
        let dir = scratch_dir("session-account-legacy-line");
        let conn = open_db(&dir.join("u.db")).unwrap();
        std::fs::write(dir.join(LOG), "s1\ttoken:aaaa1111\ns2\ttoken:cccc3333\n").unwrap();
        import(&conn, &dir).unwrap();
        assert_eq!(account(&conn, "s1", 1).as_deref(), Some("token:aaaa1111"));
        assert_eq!(
            account(&conn, "s1", 9_999).as_deref(),
            Some("token:aaaa1111")
        );

        std::fs::write(
            dir.join(LOG),
            "s1\ttoken:aaaa1111\ns2\ttoken:cccc3333\ns1\ttoken:bbbb2222\ns2\t-\n",
        )
        .unwrap();
        import(&conn, &dir).unwrap();
        assert_eq!(
            account(&conn, "s1", 9_999).as_deref(),
            Some("token:bbbb2222")
        );
        assert_eq!(account(&conn, "s2", 9_999), None);
    }

    #[test]
    fn a_legacy_session_accounts_row_is_copied_once_and_labels_the_whole_session() {
        let dir = scratch_dir("session-account-legacy-row");
        let conn = open_db(&dir.join("u.db")).unwrap();
        conn.execute(
            "INSERT INTO session_accounts VALUES ('s1', 'token:aaaa1111')",
            [],
        )
        .unwrap();
        // Reopening is what runs the copy on a database that has the old row.
        conn.execute(
            "DELETE FROM usage_watermarks WHERE source = ?1",
            [SPANS_COPIED],
        )
        .unwrap();
        copy_legacy_rows(&conn).unwrap();
        assert_eq!(account(&conn, "s1", 1).as_deref(), Some("token:aaaa1111"));

        conn.execute("DELETE FROM session_account_spans", [])
            .unwrap();
        copy_legacy_rows(&conn).unwrap();
        assert_eq!(account(&conn, "s1", 1), None);
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let dir = scratch_dir("session-account-partial");
        let conn = open_db(&dir.join("u.db")).unwrap();
        std::fs::write(dir.join(LOG), "s1\ttoken:aaaa1111\t5\ns2\ttoke").unwrap();
        assert_eq!(import(&conn, &dir).unwrap(), 1);
        std::fs::write(
            dir.join(LOG),
            "s1\ttoken:aaaa1111\t5\ns2\ttoken:cccc3333\t6\n",
        )
        .unwrap();
        assert_eq!(import(&conn, &dir).unwrap(), 1);
        assert_eq!(account(&conn, "s2", 9).as_deref(), Some("token:cccc3333"));
    }

    #[test]
    fn odd_ids_and_keys_are_not_recorded() {
        let dir = scratch_dir("session-account-odd");
        record(&dir, "a b", Some("token:aaaa1111"), 1);
        record(&dir, "s", Some("x\ny"), 1);
        record(&dir, "", None, 1);
        assert!(!dir.join(LOG).exists());
    }

    #[test]
    fn the_token_text_never_reaches_the_log_the_database_or_the_output() {
        let dir = scratch_dir("session-account-leak");
        let conn = open_db(&dir.join("u.db")).unwrap();
        let key = fingerprint(SENTINEL);
        assert!(!key.contains(SENTINEL));
        record(&dir, "s1", Some(&key), 1);
        import(&conn, &dir).unwrap();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 1, 's1', ?1, 'm', '', 'r', 'b', 1, 1, 0, 0, 0)",
            [&key],
        )
        .unwrap();
        let listing = crate::account_store::list_text(&conn).unwrap();
        assert!(listing.contains(&key) && !listing.contains(SENTINEL));
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let found = bytes
                .windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes());
            assert!(!found, "token text found in {:?}", entry.path());
        }
    }

    const BROKEN: &str = "a@b.c\t1794204756";

    fn seed_broken(conn: &Connection, key: &str) {
        for sql in [
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 1, 's1', ?1, 'm', '', 'r', 'b', 1, 1, 0, 0, 0)",
            "INSERT INTO tool_invocation_events (event_id, timestamp, session_id, account, kind, name)
                VALUES ('t', 1, 's1', ?1, 'skill', 'x')",
            "INSERT INTO session_accounts VALUES ('s1', ?1)",
            "INSERT INTO session_account_spans (session_id, since, account) VALUES ('s1', 5, ?1)",
            "INSERT INTO account_assignment_rows VALUES (1, 'usage', 'e', ?1)",
        ] {
            conn.execute(sql, [key]).unwrap();
        }
    }

    fn stored(conn: &Connection) -> Vec<String> {
        [
            "SELECT account FROM usage_events",
            "SELECT account FROM tool_invocation_events",
            "SELECT account FROM session_accounts",
            "SELECT account FROM session_account_spans",
            "SELECT previous FROM account_assignment_rows",
        ]
        .iter()
        .map(|sql| conn.query_row(sql, [], |r| r.get(0)).unwrap())
        .collect()
    }

    #[test]
    fn a_key_joined_to_a_timestamp_is_repaired_in_every_table() {
        // Arrange
        let dir = scratch_dir("session-account-repair");
        let conn = open_db(&dir.join("u.db")).unwrap();
        seed_broken(&conn, BROKEN);
        // Act
        let repaired = repair_joined_keys(&conn).unwrap();
        // Assert
        assert_eq!(repaired, 5);
        assert_eq!(stored(&conn), vec!["a@b.c"; 5]);
    }

    #[test]
    fn a_three_field_line_read_as_two_fields_is_repaired() {
        // Arrange: the old two-field split kept "key<TAB>at" as the key.
        let dir = scratch_dir("session-account-repair-log");
        let conn = open_db(&dir.join("u.db")).unwrap();
        let old_split = "s1\ta@b.c\t1794204756".split_once('\t').unwrap().1;
        conn.execute(
            "INSERT INTO session_accounts VALUES ('s1', ?1)",
            [old_split],
        )
        .unwrap();
        // Act
        repair_joined_keys(&conn).unwrap();
        // Assert
        let account: String = conn
            .query_row("SELECT account FROM session_accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(account, "a@b.c");
    }

    #[test]
    fn a_store_without_a_joined_key_is_not_written() {
        // Arrange
        let dir = scratch_dir("session-account-repair-clean");
        let conn = open_db(&dir.join("u.db")).unwrap();
        seed_broken(&conn, "a@b.c");
        let before = conn.total_changes();
        // Act
        let repaired = repair_joined_keys(&conn).unwrap();
        // Assert
        assert_eq!(repaired, 0);
        assert_eq!(conn.total_changes(), before);
    }

    #[test]
    fn an_empty_or_multi_tab_tail_is_left_alone_and_a_second_run_repairs_nothing() {
        // Arrange
        let dir = scratch_dir("session-account-repair-odd");
        let conn = open_db(&dir.join("u.db")).unwrap();
        conn.execute("INSERT INTO session_accounts VALUES ('s1', 'a@b.c\t')", [])
            .unwrap();
        conn.execute("INSERT INTO session_accounts VALUES ('s2', 'a\t1\t2')", [])
            .unwrap();
        conn.execute("INSERT INTO session_accounts VALUES ('s3', ?1)", [BROKEN])
            .unwrap();
        // Act
        let first = repair_joined_keys(&conn).unwrap();
        let second = repair_joined_keys(&conn).unwrap();
        // Assert
        assert_eq!(first, 1);
        assert_eq!(second, 0);
        let keys: Vec<String> = conn
            .prepare("SELECT account FROM session_accounts ORDER BY session_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(keys, vec!["a@b.c\t", "a\t1\t2", "a@b.c"]);
    }

    #[test]
    fn a_store_with_only_unrepairable_keys_is_not_written() {
        // Arrange
        let dir = scratch_dir("session-account-repair-unrepairable");
        let conn = open_db(&dir.join("u.db")).unwrap();
        for (session, key) in [("s1", "a@b.c\t"), ("s2", "a\t1\t2"), ("s3", "\t5")] {
            conn.execute(
                "INSERT INTO session_accounts VALUES (?1, ?2)",
                [session, key],
            )
            .unwrap();
        }
        let before = conn.total_changes();
        // Act
        let repaired = repair_joined_keys(&conn).unwrap();
        // Assert
        assert_eq!(repaired, 0);
        assert_eq!(conn.total_changes(), before);
    }

    #[test]
    fn an_ingest_repairs_broken_keys_once_and_then_leaves_them_alone() {
        // Arrange
        let paths = crate::util::scratch_paths("session-account-repair-ingest");
        let conn = open_db(&paths.db).unwrap();
        seed_broken(&conn, BROKEN);
        // Act
        crate::run::ingest_into(&paths, &conn, &mut |_, _| true).unwrap();
        let first = stored(&conn);
        crate::run::ingest_into(&paths, &conn, &mut |_, _| true).unwrap();
        // Assert
        assert_eq!(first, vec!["a@b.c"; 5]);
        assert_eq!(stored(&conn), first);
    }
}
