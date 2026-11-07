// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The push engine: sends the new and recently changed rows of one table, in
//! batches, through any [`Transport`]. Only the columns a [`TableDef`] lists
//! leave the machine, and only for accounts the user allowed.

use std::fmt;

use playbook_store::types::{Value as Sql, ValueRef};
use playbook_store::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::local_schema::{self, LocalSchemaError};
use crate::privacy::{salted_hash, Domain};
use crate::remote::{ensure_schema, RemoteError, REMOTE_SCHEMA_VERSION};
use crate::tables::{ColumnDef, TableDef, Transform};
use crate::transport::{Statement, Transport, TransportError, Value};

pub const SYNC_BATCH_ROWS: usize = 500;
/// Rows younger than this are sent again, because a local row can grow after its first write.
pub const SYNC_RESEND_WINDOW_SECONDS: i64 = 3600;
pub const SYNC_BACKOFF_BASE_SECONDS: u64 = 30;
pub const SYNC_BACKOFF_MAX_SECONDS: u64 = 900;

const TIMESTAMP_COLUMN: &str = "timestamp";
const ACCOUNT_COLUMN: &str = "account";
const MACHINE_COLUMN: &str = "machine";
const PUSHED_AT_COLUMN: &str = "pushed_at";
const SCHEMA_VERSION_COLUMN: &str = "schema_version";
pub(crate) const KEY_LAST_SUCCESS: &str = "last_success";
pub(crate) const KEY_LAST_ERROR: &str = "last_error";
pub(crate) const KEY_FAILURES: &str = "failures";

const READ_WATERMARK_SQL: &str = "SELECT rowid_mark FROM sync_watermarks WHERE table_name = ?1";
const RAISE_WATERMARK_SQL: &str =
    "INSERT INTO sync_watermarks (table_name, rowid_mark) VALUES (?1, ?2)
     ON CONFLICT(table_name) DO UPDATE SET rowid_mark = MAX(rowid_mark, excluded.rowid_mark)";
const WRITE_STATE_SQL: &str = "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value";
const READ_STATE_SQL: &str = "SELECT value FROM sync_state WHERE key = ?1";

/// What the caller decides: who this machine is and what may leave it.
#[derive(Debug, Clone)]
pub struct PushSettings {
    pub machine: String,
    /// Account keys as stored locally. Empty means send nothing.
    pub allowed_accounts: Vec<String>,
    pub salt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PushStats {
    pub rows_sent: usize,
}

#[derive(Debug)]
pub enum PushError {
    Transport(TransportError),
    Remote(RemoteError),
    Local(playbook_store::Error),
    LocalSchema(LocalSchemaError),
    /// A hashed column held something other than text.
    UnexpectedValue {
        column: &'static str,
    },
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Remote(error) => write!(f, "{error}"),
            Self::Local(error) => write!(f, "the local database failed: {error}"),
            Self::LocalSchema(error) => write!(f, "{error}"),
            Self::UnexpectedValue { column } => {
                write!(f, "column {column} holds a value that is not text")
            }
        }
    }
}

impl std::error::Error for PushError {}

impl From<TransportError> for PushError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<RemoteError> for PushError {
    fn from(error: RemoteError) -> Self {
        Self::Remote(error)
    }
}

impl From<LocalSchemaError> for PushError {
    fn from(error: LocalSchemaError) -> Self {
        Self::LocalSchema(error)
    }
}

impl From<playbook_store::Error> for PushError {
    fn from(error: playbook_store::Error) -> Self {
        Self::Local(error)
    }
}

/// Seconds to wait before the next attempt: 30, doubling, capped at 900.
pub fn backoff_seconds(consecutive_failures: u32) -> u64 {
    let Some(doublings) = consecutive_failures.checked_sub(1) else {
        return 0;
    };
    SYNC_BACKOFF_BASE_SECONDS
        .saturating_mul(1u64.checked_shl(doublings).unwrap_or(u64::MAX))
        .min(SYNC_BACKOFF_MAX_SECONDS)
}

/// Pushes one table. Safe to repeat: the remote upsert is idempotent and the
/// watermark moves only after a batch is committed remotely. A failure is
/// recorded in `sync_state` and returned.
pub fn push(
    local: &Connection,
    transport: &dyn Transport,
    table: &TableDef,
    settings: &PushSettings,
    clock: &dyn Fn() -> i64,
) -> Result<PushStats, PushError> {
    local_schema::check(local)?;
    let now = clock();
    match send_all(local, transport, table, settings, now) {
        Ok(stats) => {
            write_state(local, KEY_LAST_SUCCESS, &now.to_string())?;
            write_state(local, KEY_FAILURES, "0")?;
            Ok(stats)
        }
        Err(error) => {
            // The push error is the one to report: a failed bookkeeping write is dropped.
            let _ = record_failure(local, &error);
            Err(error)
        }
    }
}

fn send_all(
    local: &Connection,
    transport: &dyn Transport,
    table: &TableDef,
    settings: &PushSettings,
    now: i64,
) -> Result<PushStats, PushError> {
    if settings.allowed_accounts.is_empty() {
        return Ok(PushStats::default());
    }
    ensure_schema(transport, &[*table], now)?;
    let watermark = read_watermark(local, table)?;
    let select = select_sql(table, settings.allowed_accounts.len());
    let upsert = upsert_sql(table);
    let mut cursor = i64::MIN;
    let mut stats = PushStats::default();
    loop {
        let batch = read_batch(local, table, settings, &select, watermark, cursor, now)?;
        let Some(&(last_rowid, _)) = batch.last() else {
            return Ok(stats);
        };
        let statements = batch_statements(&upsert, &batch, settings, now);
        send_batch(transport, &statements)?;
        raise_watermark(local, table, last_rowid)?;
        stats.rows_sent += batch.len();
        cursor = last_rowid;
    }
}

fn column_names(table: &TableDef) -> String {
    let names: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
    names.join(", ")
}

/// New rows (past the watermark) plus the resend window, for allowed accounts only.
fn select_sql(table: &TableDef, accounts: usize) -> String {
    let marks = vec!["?"; accounts].join(", ");
    format!(
        "SELECT rowid, {columns} FROM {local} WHERE rowid > ?1
         AND (rowid > ?2 OR {TIMESTAMP_COLUMN} >= ?3)
         AND {MACHINE_COLUMN} <> '' AND {ACCOUNT_COLUMN} IN ({marks})
         ORDER BY rowid LIMIT {SYNC_BATCH_ROWS}",
        columns = column_names(table),
        local = table.local_table,
    )
}

fn upsert_sql(table: &TableDef) -> String {
    let columns = format!(
        "{MACHINE_COLUMN}, {}, {PUSHED_AT_COLUMN}, {SCHEMA_VERSION_COLUMN}",
        column_names(table)
    );
    let marks = vec!["?"; table.columns.len() + 3].join(", ");
    let mut updates: Vec<String> = table
        .columns
        .iter()
        .filter(|c| c.grows)
        .map(|c| format!("{0} = excluded.{0}", c.name))
        .collect();
    updates.push(format!("{PUSHED_AT_COLUMN} = excluded.{PUSHED_AT_COLUMN}"));
    let remote = table.remote_table;
    let growth = table.growth_column;
    format!(
        "INSERT INTO {remote} ({columns}) VALUES ({marks})
         ON CONFLICT({MACHINE_COLUMN}, {key}) DO UPDATE SET {updates}
         WHERE excluded.{growth} > {remote}.{growth}",
        key = table.key_columns.join(", "),
        updates = updates.join(", "),
    )
}

type Batch = Vec<(i64, Vec<Value>)>;

fn read_batch(
    local: &Connection,
    table: &TableDef,
    settings: &PushSettings,
    select: &str,
    watermark: i64,
    cursor: i64,
    now: i64,
) -> Result<Batch, PushError> {
    let mut binds = vec![
        Sql::Integer(cursor),
        Sql::Integer(watermark),
        Sql::Integer(now - SYNC_RESEND_WINDOW_SECONDS),
    ];
    binds.extend(settings.allowed_accounts.iter().cloned().map(Sql::Text));
    let mut prepared = local.prepare(select)?;
    let mut rows = prepared.query(params_from_iter(binds))?;
    let mut batch = Vec::new();
    while let Some(row) = rows.next()? {
        batch.push((row.get(0)?, remote_values(row, table, &settings.salt)?));
    }
    Ok(batch)
}

fn remote_values(row: &Row<'_>, table: &TableDef, salt: &str) -> Result<Vec<Value>, PushError> {
    table
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| transform(row.get_ref(index + 1)?, column, salt))
        .collect()
}

fn transform(raw: ValueRef<'_>, column: &ColumnDef, salt: &str) -> Result<Value, PushError> {
    let domain = match column.transform {
        Transform::Copy => return Ok(copy_value(raw)),
        Transform::HashAccount => Domain::Account,
        Transform::HashRepo => Domain::Repo,
    };
    let text = raw.as_str().map_err(|_| PushError::UnexpectedValue {
        column: column.name,
    })?;
    Ok(Value::Text(salted_hash(salt, domain, text)))
}

fn copy_value(raw: ValueRef<'_>) -> Value {
    match raw {
        ValueRef::Integer(n) => Value::Integer(n),
        ValueRef::Real(x) => Value::Real(x),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
    }
}

fn batch_statements(
    upsert: &str,
    batch: &Batch,
    settings: &PushSettings,
    now: i64,
) -> Vec<Statement> {
    let plain = |sql: &str| Statement {
        sql: sql.to_string(),
        args: vec![],
    };
    let mut statements = vec![plain("BEGIN")];
    for (_, values) in batch {
        let mut args = vec![Value::Text(settings.machine.clone())];
        args.extend(values.iter().cloned());
        args.push(Value::Integer(now));
        args.push(Value::Integer(i64::from(REMOTE_SCHEMA_VERSION)));
        statements.push(Statement {
            sql: upsert.to_string(),
            args,
        });
    }
    statements.push(plain("COMMIT"));
    statements
}

/// One pipeline per batch. The remote may run on after a failed statement, so
/// any error is followed by a ROLLBACK, whose own result is of no use.
fn send_batch(transport: &dyn Transport, statements: &[Statement]) -> Result<(), PushError> {
    if let Err(error) = transport.execute(statements) {
        let _ = transport.execute(&[Statement {
            sql: "ROLLBACK".to_string(),
            args: vec![],
        }]);
        return Err(error.into());
    }
    Ok(())
}

fn read_watermark(local: &Connection, table: &TableDef) -> Result<i64, PushError> {
    let mark = local
        .query_row(READ_WATERMARK_SQL, [table.local_table], |r| r.get(0))
        .optional()?;
    Ok(mark.unwrap_or(0))
}

fn raise_watermark(local: &Connection, table: &TableDef, rowid: i64) -> Result<(), PushError> {
    local.execute(RAISE_WATERMARK_SQL, params![table.local_table, rowid])?;
    Ok(())
}

pub(crate) fn write_state(
    local: &Connection,
    key: &str,
    value: &str,
) -> Result<(), playbook_store::Error> {
    local.execute(WRITE_STATE_SQL, params![key, value])?;
    Ok(())
}

pub(crate) fn read_state(
    local: &Connection,
    key: &str,
) -> Result<Option<String>, playbook_store::Error> {
    local
        .query_row(READ_STATE_SQL, [key], |r| r.get(0))
        .optional()
}

/// Stores the error text and counts one more consecutive failure.
pub(crate) fn record_failure(
    local: &Connection,
    error: &dyn fmt::Display,
) -> Result<(), playbook_store::Error> {
    let previous = read_state(local, KEY_FAILURES)?;
    let failures = previous.and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    write_state(local, KEY_LAST_ERROR, &error.to_string())?;
    write_state(local, KEY_FAILURES, &failures.saturating_add(1).to_string())
}
