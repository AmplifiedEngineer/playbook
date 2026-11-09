// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The remote schema and the shared salt, over any [`Transport`].

use std::fmt;

use crate::tables::TableDef;
use crate::transport::{Statement, StatementResult, Transport, TransportError, Value};

/// The schema version this binary writes and understands.
pub const REMOTE_SCHEMA_VERSION: u32 = 1;

const KEY_SCHEMA_VERSION: &str = "schema_version";
const KEY_CREATED_AT: &str = "created_at";
pub(crate) const KEY_SALT: &str = "salt";
const COLUMN_MACHINE: &str = "machine";
const COLUMN_PUSHED_AT: &str = "pushed_at";
const UPDATE_HINT: &str = "run `playbook upgrade`";

const META_EXISTS_SQL: &str =
    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'shared_meta'";
const READ_VERSION_SQL: &str = "SELECT value FROM shared_meta WHERE key = ?";
const CREATE_META_SQL: &str =
    "CREATE TABLE IF NOT EXISTS shared_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)";
const INSERT_META_SQL: &str = "INSERT OR IGNORE INTO shared_meta (key, value) VALUES (?, ?)";
const READ_SALT_SQL: &str = "SELECT value FROM shared_meta WHERE key = ?";

/// Tables that are not row-synced from `usage.db`, created beside the shared ones.
const EXTRA_DDL: [&str; 1] = [
    "CREATE TABLE IF NOT EXISTS shared_config_snapshots (machine TEXT NOT NULL, key TEXT NOT NULL, \
     value TEXT NOT NULL, pushed_at INTEGER NOT NULL, schema_version INTEGER NOT NULL, \
     PRIMARY KEY (machine, key))",
];

/// Why a remote setup step failed.
#[derive(Debug, PartialEq, Eq)]
pub enum RemoteError {
    Transport(TransportError),
    /// The remote was written by a newer binary than this one.
    RemoteNewer {
        remote: u32,
        local: u32,
    },
    Malformed {
        what: &'static str,
    },
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::RemoteNewer { remote, local } => write!(
                f,
                "the remote schema is version {remote} but this binary understands version {local}: {UPDATE_HINT} and try again"
            ),
            Self::Malformed { what } => write!(f, "the remote answered with an unexpected {what}"),
        }
    }
}

impl std::error::Error for RemoteError {}

impl From<TransportError> for RemoteError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

fn statement(sql: impl Into<String>, args: Vec<Value>) -> Statement {
    Statement {
        sql: sql.into(),
        args,
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_string())
}

fn first_text(result: &StatementResult) -> Result<Option<&str>, RemoteError> {
    match result.rows.first().and_then(|row| row.first()) {
        None => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value)),
        Some(_) => Err(RemoteError::Malformed { what: "value type" }),
    }
}

/// The version stored on the remote, or none when the remote is fresh. Reads only.
fn remote_version(transport: &dyn Transport) -> Result<Option<u32>, RemoteError> {
    let exists = transport.execute(&[statement(META_EXISTS_SQL, vec![])])?;
    if exists[0].rows.is_empty() {
        return Ok(None);
    }
    let read = transport.execute(&[statement(READ_VERSION_SQL, vec![text(KEY_SCHEMA_VERSION)])])?;
    first_text(&read[0])?
        .map(|stored| {
            stored.parse().map_err(|_| RemoteError::Malformed {
                what: "schema version",
            })
        })
        .transpose()
}

fn create_table_sql(table: &TableDef) -> String {
    let mut columns = vec![format!("{COLUMN_MACHINE} TEXT NOT NULL")];
    columns.extend(
        table
            .columns
            .iter()
            .map(|c| format!("{} {} NOT NULL", c.name, c.kind.sql())),
    );
    columns.push(format!("{COLUMN_PUSHED_AT} INTEGER NOT NULL"));
    columns.push(format!("{KEY_SCHEMA_VERSION} INTEGER NOT NULL"));
    let key = table.key_columns.join(", ");
    format!(
        "CREATE TABLE IF NOT EXISTS {} ({}, PRIMARY KEY ({COLUMN_MACHINE}, {key}))",
        table.remote_table,
        columns.join(", ")
    )
}

fn create_index_sql(table: &TableDef) -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS {name}_by_pushed ON {name} ({COLUMN_PUSHED_AT}, {COLUMN_MACHINE}, {key})",
        name = table.remote_table,
        key = table.key_columns.join(", ")
    )
}

/// Creates the shared tables when absent and records the schema version. Safe to
/// repeat. A remote on a newer schema is refused before anything is written.
pub fn ensure_schema(
    transport: &dyn Transport,
    tables: &[TableDef],
    now: i64,
) -> Result<(), RemoteError> {
    if let Some(remote) = remote_version(transport)? {
        if remote > REMOTE_SCHEMA_VERSION {
            return Err(RemoteError::RemoteNewer {
                remote,
                local: REMOTE_SCHEMA_VERSION,
            });
        }
    }
    let mut statements = vec![
        statement(CREATE_META_SQL, vec![]),
        statement(
            INSERT_META_SQL,
            vec![
                text(KEY_SCHEMA_VERSION),
                text(&REMOTE_SCHEMA_VERSION.to_string()),
            ],
        ),
        statement(
            INSERT_META_SQL,
            vec![text(KEY_CREATED_AT), text(&now.to_string())],
        ),
    ];
    for table in tables {
        statements.push(statement(create_table_sql(table), vec![]));
        statements.push(statement(create_index_sql(table), vec![]));
    }
    statements.extend(EXTRA_DDL.iter().map(|sql| statement(*sql, vec![])));
    transport.execute(&statements)?;
    Ok(())
}

/// Stores a salt when none exists yet, then returns the stored one, so clients
/// that race all end up with the winner's salt.
pub fn ensure_salt(
    transport: &dyn Transport,
    generate: &dyn Fn() -> String,
) -> Result<String, RemoteError> {
    let results = transport.execute(&[
        statement(
            INSERT_META_SQL,
            vec![text(KEY_SALT), Value::Text(generate())],
        ),
        statement(READ_SALT_SQL, vec![text(KEY_SALT)]),
    ])?;
    first_text(&results[1])?
        .map(str::to_string)
        .ok_or(RemoteError::Malformed { what: "salt" })
}
