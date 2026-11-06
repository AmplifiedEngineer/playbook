// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which credential each session used. The transcripts do not say, and
//! `~/.claude.json` names only the account Claude Code is signed in to, which
//! a `CLAUDE_CODE_OAUTH_TOKEN` in the environment overrides. A session start
//! hook sees that environment, so it appends one line per session to a small
//! log. Ingest reads the log into the `session_accounts` table and labels the
//! events of each session with its account.
//!
//! The token is never stored, logged or printed. Only a fingerprint is: the
//! first 8 hex digits of a SHA-256 over a fixed prefix and the token, with
//! the prefix `token:`. It cannot be turned back into the token.

use crate::account_store::err;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS session_accounts (
    session_id TEXT PRIMARY KEY,
    account TEXT NOT NULL
);";

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

/// Appends the account of `session_id` to the log in `dir`: the fingerprint
/// when one is given, else the signed in marker. Never fails the caller.
pub fn record(dir: &Path, session_id: &str, fingerprint: Option<&str>) {
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
        let _ = file.write_all(format!("{session_id}\t{key}\n").as_bytes());
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
        let Some((session, key)) = line.split_once('\t') else {
            continue;
        };
        if key == SIGNED_IN {
            conn.execute(
                "DELETE FROM session_accounts WHERE session_id = ?1",
                [session],
            )
        } else {
            conn.execute(
                "INSERT INTO session_accounts (session_id, account) VALUES (?1, ?2)
                 ON CONFLICT(session_id) DO UPDATE SET account = excluded.account",
                [session, key],
            )
        }
        .map_err(|e| err("save a session account", e))?;
        imported += 1;
    }
    crate::db::raise_watermark(conn, OFFSET, i64::try_from(end).unwrap_or(i64::MAX))?;
    Ok(imported)
}

/// The account recorded for `session_id`, if its credential was a token.
pub fn of_session(conn: &Connection, session_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT account FROM session_accounts WHERE session_id = ?1",
        [session_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| err("read a session account", e))
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

    #[test]
    fn a_session_is_labeled_by_its_recorded_credential() {
        let dir = scratch_dir("session-account");
        let conn = open_db(&dir.join("u.db")).unwrap();
        record(&dir, "s1", Some("token:aaaa1111"));
        record(&dir, "s2", None);
        assert_eq!(import(&conn, &dir).unwrap(), 2);
        assert_eq!(
            of_session(&conn, "s1").unwrap().as_deref(),
            Some("token:aaaa1111")
        );
        assert_eq!(of_session(&conn, "s2").unwrap(), None);

        // A later start of the same session on the signed in account clears it.
        record(&dir, "s1", None);
        record(&dir, "s3", Some("token:bbbb2222"));
        assert_eq!(import(&conn, &dir).unwrap(), 2);
        assert_eq!(import(&conn, &dir).unwrap(), 0);
        assert_eq!(of_session(&conn, "s1").unwrap(), None);
        assert_eq!(
            of_session(&conn, "s3").unwrap().as_deref(),
            Some("token:bbbb2222")
        );
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let dir = scratch_dir("session-account-partial");
        let conn = open_db(&dir.join("u.db")).unwrap();
        std::fs::write(dir.join(LOG), "s1\ttoken:aaaa1111\ns2\ttoke").unwrap();
        assert_eq!(import(&conn, &dir).unwrap(), 1);
        std::fs::write(dir.join(LOG), "s1\ttoken:aaaa1111\ns2\ttoken:cccc3333\n").unwrap();
        assert_eq!(import(&conn, &dir).unwrap(), 1);
        assert_eq!(
            of_session(&conn, "s2").unwrap().as_deref(),
            Some("token:cccc3333")
        );
    }

    #[test]
    fn odd_ids_and_keys_are_not_recorded() {
        let dir = scratch_dir("session-account-odd");
        record(&dir, "a b", Some("token:aaaa1111"));
        record(&dir, "s", Some("x\ny"));
        record(&dir, "", None);
        assert!(!dir.join(LOG).exists());
    }

    #[test]
    fn the_token_text_never_reaches_the_log_the_database_or_the_output() {
        let dir = scratch_dir("session-account-leak");
        let conn = open_db(&dir.join("u.db")).unwrap();
        let key = fingerprint(SENTINEL);
        assert!(!key.contains(SENTINEL));
        record(&dir, "s1", Some(&key));
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
}
