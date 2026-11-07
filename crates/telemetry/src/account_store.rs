// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Display names for account keys. An account key is the label stored on each
//! event: an email from `~/.claude.json` or a `token:` fingerprint. A name is
//! only shown beside it.

use playbook_store::Connection;

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS account_names (
    key TEXT PRIMARY KEY,
    name TEXT NOT NULL
);";

/// The longest account key accepted.
const MAX_KEY: usize = 120;

pub(crate) fn err(what: &str, e: playbook_store::Error) -> String {
    format!("failed to {what}: {e}")
}

/// The longest display name accepted.
const MAX_NAME: usize = 40;

/// Invisible formatting characters (zero width marks, bidi overrides).
fn is_format_char(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// A key is a single word of at most `MAX_KEY` characters.
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key != key.trim()
        || key
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || is_format_char(c))
    {
        return Err("an account is a single word without spaces".to_string());
    }
    if key.len() > MAX_KEY {
        return Err(format!("an account key is at most {MAX_KEY} characters"));
    }
    Ok(())
}

/// True when `text` is already the key of an event or of a named account.
fn key_in_use(conn: &Connection, text: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM usage_events WHERE account = ?1)
             OR EXISTS(SELECT 1 FROM account_names WHERE key = ?1)",
        [text],
        |r| r.get(0),
    )
    .map_err(|e| err("check the account keys", e))
}

/// True when some event carries `key`.
pub fn has_events(conn: &Connection, key: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM usage_events WHERE account = ?1)",
        [key],
        |r| r.get(0),
    )
    .map_err(|e| err("check the account keys", e))
}

/// Sets the display name of `key`. An empty name removes it.
pub fn set_name(conn: &Connection, key: &str, name: &str) -> Result<(), String> {
    check_key(key)?;
    let name = name.trim();
    if name.chars().any(|c| c.is_control() || is_format_char(c)) {
        return Err("a name cannot hold control or invisible characters".to_string());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("a name is at most {MAX_NAME} characters"));
    }
    if !name.is_empty() && key_in_use(conn, name)? && name != key {
        return Err(format!("{name} is already an account key"));
    }
    if let Some((other, _)) = names(conn)?
        .into_iter()
        .find(|(k, n)| k != key && n.eq_ignore_ascii_case(name))
    {
        return Err(format!("{other} already uses the name {name}"));
    }
    if name.is_empty() {
        conn.execute("DELETE FROM account_names WHERE key = ?1", [key])
    } else {
        conn.execute(
            "INSERT INTO account_names (key, name) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET name = excluded.name",
            [key, name],
        )
    }
    .map(drop)
    .map_err(|e| err("save the account name", e))
}

/// Every named key with its name.
pub fn names(conn: &Connection) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare("SELECT key, name FROM account_names ORDER BY key")
        .map_err(|e| err("read the account names", e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| err("read the account names", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("read the account names", e))
}

/// The name of `key`, or the key itself when it has none.
pub fn display(names: &[(String, String)], key: &str) -> String {
    names
        .iter()
        .find(|(k, _)| k == key)
        .map_or_else(|| key.to_string(), |(_, n)| n.clone())
}

/// A key as the user typed it: an existing key, else the key of a name
/// (case-insensitive), else the text itself as a new key.
pub fn resolve(conn: &Connection, input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("an account is a single word without spaces".to_string());
    }
    if input.len() > MAX_KEY {
        return Err(format!("an account key is at most {MAX_KEY} characters"));
    }
    let named = names(conn)?;
    if named.iter().any(|(k, _)| k == input) {
        return Ok(input.to_string());
    }
    if let Some((k, _)) = named.iter().find(|(_, n)| n.eq_ignore_ascii_case(input)) {
        return Ok(k.clone());
    }
    Ok(input.to_string())
}

/// One account in the store.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub key: String,
    pub name: String,
    pub events: i64,
    pub cost_usd: f64,
    pub first: i64,
    pub last: i64,
}

/// The accounts that have events, newest activity first. Cost is the stored
/// column, a summary figure; the views price from token counts.
pub fn accounts(conn: &Connection) -> Result<Vec<AccountRow>, String> {
    let named = names(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT account, COUNT(*), COALESCE(SUM(cost_usd), 0), MIN(timestamp), MAX(timestamp)
             FROM usage_events GROUP BY account ORDER BY MAX(timestamp) DESC, account",
        )
        .map_err(|e| err("list the accounts", e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AccountRow {
                key: r.get(0)?,
                name: String::new(),
                events: r.get(1)?,
                cost_usd: r.get(2)?,
                first: r.get(3)?,
                last: r.get(4)?,
            })
        })
        .map_err(|e| err("list the accounts", e))?;
    let mut out = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| err("list the accounts", e))?;
    for row in &mut out {
        row.name = display(&named, &row.key);
    }
    Ok(out)
}

/// The text of `playbook usage account list`.
pub fn list_text(conn: &Connection) -> Result<String, String> {
    let rows = accounts(conn)?;
    if rows.is_empty() {
        return Ok("No usage recorded yet.".to_string());
    }
    let mut out = String::new();
    for r in rows {
        let shown = if r.name == r.key {
            r.key.clone()
        } else {
            format!("{} ({})", r.name, r.key)
        };
        out.push_str(&format!(
            "{shown}\n  {} events, ${:.2}, {} to {}\n",
            r.events,
            r.cost_usd,
            crate::tz::iso(r.first),
            crate::tz::iso(r.last)
        ));
    }
    Ok(out.trim_end().to_string())
}

/// A store with four events on two accounts and one skill record, for tests.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::open_db;
    use crate::util::scratch_dir;

    pub(crate) fn store() -> Connection {
        let conn = open_db(&scratch_dir("account-store").join("u.db")).unwrap();
        for (id, ts, session, account, cost) in [
            ("a", 100, "s1", "work@x.com", 1.0),
            ("b", 200, "s1", "work@x.com", 2.0),
            ("c", 300, "s2", "work@x.com", 4.0),
            ("d", 400, "s3", "other@x.com", 8.0),
        ] {
            conn.execute(
                "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                    repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                    cache_read_tokens, cost_usd) VALUES (?1, ?2, ?3, ?4, 'm', '', 'r', 'b', 1, 1, 0, 0, ?5)",
                playbook_store::params![id, ts, session, account, cost],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO tool_invocation_events VALUES ('t', 150, 's1', 'work@x.com', 'skill', 'k', '')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn names_show_in_place_of_keys_and_resolve_back() {
        let conn = store();
        set_name(&conn, "work@x.com", "Company").unwrap();
        let rows = accounts(&conn).unwrap();
        assert_eq!(rows[1].name, "Company");
        assert_eq!(rows[0].name, "other@x.com");
        assert_eq!(resolve(&conn, "company").unwrap(), "work@x.com");
        assert_eq!(resolve(&conn, "token:ab12cd34").unwrap(), "token:ab12cd34");
        set_name(&conn, "work@x.com", " ").unwrap();
        assert!(names(&conn).unwrap().is_empty());
        assert!(resolve(&conn, "two words").is_err());
    }

    #[test]
    fn a_name_cannot_hold_control_characters_or_repeat_another_key_name() {
        let conn = store();
        set_name(&conn, "work@x.com", "Company").unwrap();
        assert!(set_name(&conn, "other@x.com", "bad\x1b[2J").is_err());
        let err = set_name(&conn, "other@x.com", "company").unwrap_err();
        assert!(err.contains("work@x.com already uses"), "{err}");
        set_name(&conn, "work@x.com", "COMPANY").unwrap();
    }

    #[test]
    fn a_bad_key_or_a_name_that_is_another_key_is_refused() {
        let conn = store();
        assert!(set_name(&conn, "two words", "X").is_err());
        assert!(set_name(&conn, "", "X").is_err());
        assert!(set_name(&conn, "work@x.com", "other@x.com").is_err());
        assert!(set_name(&conn, "work@x.com", "a\u{202E}b").is_err());
        assert!(set_name(&conn, "work@x.com", &"n".repeat(41)).is_err());
        assert!(names(&conn).unwrap().is_empty());
        assert!(has_events(&conn, "work@x.com").unwrap());
        assert!(!has_events(&conn, "work@x.cm").unwrap());
    }

    #[test]
    fn the_list_shows_the_name_beside_the_key() {
        let conn = store();
        set_name(&conn, "other@x.com", "Home").unwrap();
        let text = list_text(&conn).unwrap();
        assert!(
            text.starts_with("Home (other@x.com)\n  1 events, $8.00"),
            "{text}"
        );
        assert!(text.contains("work@x.com\n  3 events, $7.00"), "{text}");
    }
}
