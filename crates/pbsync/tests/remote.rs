// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#[path = "support/sqlite_fake.rs"]
mod sqlite_fake;

use pbsync::remote::{ensure_salt, ensure_schema, RemoteError, REMOTE_SCHEMA_VERSION};
use pbsync::tables::usage_events;
use pbsync::transport::Value;
use sqlite_fake::SqliteFake;

const NOW: i64 = 1_800_000_000;
const NEVER_REMOTE: [&str; 5] = ["session_id", "agent", "branch", "cwd", "email"];

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn schema_dump(fake: &SqliteFake) -> Vec<Vec<Value>> {
    fake.query("SELECT name, sql FROM sqlite_master ORDER BY name")
}

fn is_write(sql: &str) -> bool {
    !sql.trim_start().to_uppercase().starts_with("SELECT")
}

#[test]
fn ensure_schema_twice_changes_nothing() {
    // Arrange
    let fake = SqliteFake::new();
    ensure_schema(&fake, &[usage_events()], NOW).unwrap();
    let first = schema_dump(&fake);
    // Act
    let second = ensure_schema(&fake, &[usage_events()], NOW + 1);
    // Assert
    assert_eq!(second, Ok(()));
    assert_eq!(schema_dump(&fake), first);
    let version = fake.query("SELECT value FROM shared_meta WHERE key = 'schema_version'");
    assert_eq!(version, [[text(&REMOTE_SCHEMA_VERSION.to_string())]]);
}

#[test]
fn the_remote_table_is_keyed_by_machine_and_event_and_has_no_identifying_column() {
    // Arrange
    let fake = SqliteFake::new();
    // Act
    ensure_schema(&fake, &[usage_events()], NOW).unwrap();
    // Assert
    let info = fake.query("SELECT name, pk FROM pragma_table_info('shared_usage_events')");
    let mut keyed: Vec<(i64, Value)> = info
        .iter()
        .filter_map(|row| match &row[1] {
            Value::Integer(pk) if *pk > 0 => Some((*pk, row[0].clone())),
            _ => None,
        })
        .collect();
    keyed.sort_by_key(|(pk, _)| *pk);
    assert_eq!(keyed, [(1, text("machine")), (2, text("event_id"))]);
    for banned in NEVER_REMOTE {
        assert!(!info.iter().any(|row| row[0] == text(banned)), "{banned}");
    }
    for needed in ["pushed_at", "schema_version", "account", "repo", "cost_usd"] {
        assert!(info.iter().any(|row| row[0] == text(needed)), "{needed}");
    }
}

#[test]
fn no_table_is_created_for_an_unlisted_definition() {
    // Arrange
    let fake = SqliteFake::new();
    // Act
    ensure_schema(&fake, &[], NOW).unwrap();
    // Assert
    let names =
        fake.query("SELECT name FROM sqlite_master WHERE name LIKE 'shared_%' ORDER BY name");
    assert_eq!(
        names,
        [[text("shared_config_snapshots")], [text("shared_meta")]]
    );
}

#[test]
fn a_newer_remote_is_refused_with_no_writes() {
    // Arrange
    let fake = SqliteFake::new();
    fake.seed(
        "CREATE TABLE shared_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO shared_meta VALUES ('schema_version', '2');",
    );
    // Act
    let result = ensure_schema(&fake, &[usage_events()], NOW);
    // Assert
    let error = result.unwrap_err();
    assert_eq!(
        error,
        RemoteError::RemoteNewer {
            remote: 2,
            local: 1
        }
    );
    assert!(error.to_string().contains("version 2"));
    assert!(error.to_string().contains("version 1"));
    assert!(error.to_string().contains("playbook upgrade"));
    assert!(
        !fake.log().iter().any(|sql| is_write(sql)),
        "{:?}",
        fake.log()
    );
}

#[test]
fn two_clients_converge_on_the_first_salt() {
    // Arrange
    let fake = SqliteFake::new();
    ensure_schema(&fake, &[], NOW).unwrap();
    // Act
    let first = ensure_salt(&fake, &|| "salt-a".to_string()).unwrap();
    let second = ensure_salt(&fake, &|| "salt-b".to_string()).unwrap();
    // Assert
    assert_eq!(first, "salt-a");
    assert_eq!(second, "salt-a");
}
