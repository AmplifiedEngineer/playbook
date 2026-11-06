// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `schema_version` row in a database's `meta` table. A build refuses a
//! database stamped newer than it understands, and stamps one that is absent
//! or older. A database with no row counts as version 0, so every database
//! made before the row existed opens and is stamped.

use crate::{Connection, Error, OptionalExtension};
use std::fmt;

/// The meta key holding the version.
const VERSION_KEY: &str = "schema_version";

/// The command that brings an older build up to date.
const UPDATE_COMMAND: &str = "playbook update";

#[derive(Debug)]
pub enum SchemaError {
    /// The database was stamped by a newer build.
    Newer {
        database: String,
        found: i64,
        ours: i64,
    },
    /// The stored `schema_version` is not a whole number.
    NotAnInteger {
        database: String,
        value: String,
    },
    Store(Error),
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::Newer {
                database,
                found,
                ours,
            } => write!(
                f,
                "{database} has schema version {found}, this build understands {ours}; \
                 upgrade with `{UPDATE_COMMAND}`"
            ),
            SchemaError::NotAnInteger { database, value } => write!(
                f,
                "{database} has a schema_version that is not a whole number ({value:?}); \
                 nothing was changed"
            ),
            SchemaError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SchemaError {}

impl From<Error> for SchemaError {
    fn from(e: Error) -> Self {
        SchemaError::Store(e)
    }
}

/// The version stored in `conn`, `None` when no row exists. Writes nothing.
pub fn stored_version(conn: &Connection, database: &str) -> Result<Option<i64>, SchemaError> {
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
        [],
        |r| r.get(0),
    )?;
    if !has_meta {
        return Ok(None);
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [VERSION_KEY],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|value| {
        value.parse().map_err(|_| SchemaError::NotAnInteger {
            database: database.to_string(),
            value,
        })
    })
    .transpose()
}

/// Refuses a database stamped newer than `ours`, and otherwise stamps `ours`
/// when the row is absent or older. A refusal writes nothing.
pub fn check_and_stamp(conn: &Connection, database: &str, ours: i64) -> Result<(), SchemaError> {
    let found = stored_version(conn, database)?;
    match found {
        Some(found) if found > ours => {
            return Err(SchemaError::Newer {
                database: database.to_string(),
                found,
                ours,
            })
        }
        Some(found) if found == ours => return Ok(()),
        _ => {}
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        (VERSION_KEY, ours.to_string()),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DB: &str = "usage.db";

    fn memory_with_version(value: Option<&str>) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        if let Some(v) = value {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                [v],
            )
            .unwrap();
        }
        conn
    }

    fn stored(conn: &Connection) -> Option<String> {
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .ok()
    }

    #[test]
    fn a_newer_version_is_refused_with_both_versions_and_nothing_is_written() {
        // Arrange
        let conn = memory_with_version(Some("3"));
        let before = conn.total_changes();

        // Act
        let err = check_and_stamp(&conn, DB, 2).unwrap_err();

        // Assert
        let text = err.to_string();
        assert!(text.contains("usage.db"), "{text}");
        assert!(
            text.contains("version 3") && text.contains("understands 2"),
            "{text}"
        );
        assert!(text.contains("playbook update"), "{text}");
        assert_eq!(conn.total_changes(), before);
        assert_eq!(stored(&conn).as_deref(), Some("3"));
    }

    #[test]
    fn an_older_version_opens_and_is_stamped_with_ours() {
        // Arrange
        let conn = memory_with_version(Some("1"));

        // Act
        let result = check_and_stamp(&conn, DB, 2);

        // Assert
        assert!(result.is_ok());
        assert_eq!(stored(&conn).as_deref(), Some("2"));
    }

    #[test]
    fn an_equal_version_opens_without_a_write() {
        // Arrange
        let conn = memory_with_version(Some("2"));
        let before = conn.total_changes();

        // Act
        let result = check_and_stamp(&conn, DB, 2);

        // Assert
        assert!(result.is_ok());
        assert_eq!(conn.total_changes(), before);
    }

    #[test]
    fn a_database_without_the_meta_table_is_stamped() {
        // Arrange
        let conn = Connection::open_in_memory().unwrap();

        // Act
        let result = check_and_stamp(&conn, DB, 1);

        // Assert
        assert!(result.is_ok());
        assert_eq!(stored(&conn).as_deref(), Some("1"));
    }

    #[test]
    fn a_meta_table_without_the_row_is_stamped() {
        // Arrange
        let conn = memory_with_version(None);

        // Act
        let result = check_and_stamp(&conn, DB, 1);

        // Assert
        assert!(result.is_ok());
        assert_eq!(stored(&conn).as_deref(), Some("1"));
    }

    #[test]
    fn a_non_integer_value_is_a_named_error_and_nothing_is_written() {
        // Arrange
        let conn = memory_with_version(Some("two"));
        let before = conn.total_changes();

        // Act
        let err = check_and_stamp(&conn, DB, 2).unwrap_err();

        // Assert
        assert!(matches!(err, SchemaError::NotAnInteger { .. }), "{err}");
        assert!(err.to_string().contains("usage.db"));
        assert_eq!(conn.total_changes(), before);
        assert_eq!(stored(&conn).as_deref(), Some("two"));
    }

    #[test]
    fn reading_the_version_never_creates_the_meta_table() {
        // Arrange
        let conn = Connection::open_in_memory().unwrap();

        // Act
        let version = stored_version(&conn, DB).unwrap();

        // Assert
        assert_eq!(version, None);
        let tables: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 0);
    }
}
