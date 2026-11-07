// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pull engine: copies the rows other machines pushed into the local
//! `usage_events_remote` table, page by page, through any [`Transport`]. The
//! local `usage_events` table is only read, never written.

use std::fmt;

use playbook_store::types::Value as Sql;
use playbook_store::{params, params_from_iter, Connection, OptionalExtension};

use crate::privacy::{salted_hash, Domain};
use crate::push::{
    record_failure, write_state, KEY_FAILURES, KEY_LAST_SUCCESS, SYNC_BATCH_ROWS,
    SYNC_RESEND_WINDOW_SECONDS,
};
use crate::remote::{ensure_schema, RemoteError};
use crate::tables::{ColumnDef, TableDef, Transform};
use crate::transport::{Statement, Transport, TransportError, Value};

const LOCAL_REMOTE_TABLE: &str = "usage_events_remote";
const REPO_HASH_COLUMN: &str = "repo_hash";
const READY_TABLES: [&str; 4] = [
    "sync_state",
    LOCAL_REMOTE_TABLE,
    "sync_cursors",
    "account_aliases",
];
const NOT_READY_HINT: &str = "run any playbook command to create it";

const TABLE_EXISTS_SQL: &str = "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1";
const READ_CURSOR_SQL: &str =
    "SELECT pushed_at, key FROM sync_cursors WHERE machine = ?1 AND table_name = ?2";
// Only moves forward, so a re-read of the resend window never lowers the cursor.
const WRITE_CURSOR_SQL: &str = "INSERT INTO sync_cursors (machine, table_name, pushed_at, key)
     VALUES (?1, ?2, ?3, ?4)
     ON CONFLICT(machine, table_name) DO UPDATE SET pushed_at = excluded.pushed_at, key = excluded.key
     WHERE (excluded.pushed_at, excluded.key) > (sync_cursors.pushed_at, sync_cursors.key)";
const UPSERT_ALIAS_SQL: &str = "INSERT INTO account_aliases (hash, key) VALUES (?1, ?2)
     ON CONFLICT(hash) DO UPDATE SET key = excluded.key";
const READ_NAMES_SQL: &str = "SELECT key FROM account_names";
const NAMES_TABLE: &str = "account_names";

/// What the caller decides: who this machine is and the shared salt.
#[derive(Debug, Clone)]
pub struct PullSettings {
    /// Rows of this machine are skipped as self-echo.
    pub machine: String,
    pub salt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PullStats {
    /// Rows inserted or grown locally.
    pub rows_written: usize,
    /// Rows seen whose event id also exists in the local `usage_events`.
    pub overlaps: usize,
}

#[derive(Debug)]
pub enum PullError {
    Transport(TransportError),
    Remote(RemoteError),
    Local(playbook_store::Error),
    /// A table playbook creates is missing from the local database.
    LocalNotReady {
        table: &'static str,
    },
    /// The remote sent a value of another type than the table definition says.
    UnexpectedValue {
        column: &'static str,
    },
}

impl fmt::Display for PullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Remote(error) => write!(f, "{error}"),
            Self::Local(error) => write!(f, "the local database failed: {error}"),
            Self::LocalNotReady { table } => {
                write!(
                    f,
                    "the local database has no {table} table: {NOT_READY_HINT}"
                )
            }
            Self::UnexpectedValue { column } => {
                write!(
                    f,
                    "the remote sent an unexpected value type for column {column}"
                )
            }
        }
    }
}

impl std::error::Error for PullError {}

impl From<TransportError> for PullError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<RemoteError> for PullError {
    fn from(error: RemoteError) -> Self {
        Self::Remote(error)
    }
}

impl From<playbook_store::Error> for PullError {
    fn from(error: playbook_store::Error) -> Self {
        Self::Local(error)
    }
}

/// Pulls one table from every other machine. Safe to repeat: each page and its
/// cursor commit together, and the upsert never lowers a stored value.
pub fn pull(
    local: &Connection,
    transport: &dyn Transport,
    table: &TableDef,
    settings: &PullSettings,
    clock: &dyn Fn() -> i64,
) -> Result<PullStats, PullError> {
    pull_with(local, transport, table, settings, clock, &mut || {})
}

/// Like [`pull`], and calls `after_page` once each page is committed.
pub fn pull_with(
    local: &Connection,
    transport: &dyn Transport,
    table: &TableDef,
    settings: &PullSettings,
    clock: &dyn Fn() -> i64,
    after_page: &mut dyn FnMut(),
) -> Result<PullStats, PullError> {
    ensure_ready(local)?;
    let now = clock();
    let result = pull_all(local, transport, table, settings, now, after_page);
    match &result {
        Ok(_) => {
            write_state(local, KEY_LAST_SUCCESS, &now.to_string())?;
            write_state(local, KEY_FAILURES, "0")?;
        }
        Err(error) => {
            // The pull error is the one to report: a failed bookkeeping write is dropped.
            let _ = record_failure(local, error);
        }
    }
    result
}

fn pull_all(
    local: &Connection,
    transport: &dyn Transport,
    table: &TableDef,
    settings: &PullSettings,
    now: i64,
    after_page: &mut dyn FnMut(),
) -> Result<PullStats, PullError> {
    ensure_schema(transport, &[*table], now)?;
    let plan = Plan::new(table);
    let mut stats = PullStats::default();
    for machine in other_machines(transport, table, &settings.machine)? {
        pull_machine(local, transport, &plan, &machine, &mut stats, after_page)?;
    }
    rebuild_aliases(local, table, &settings.salt)?;
    Ok(stats)
}

fn ensure_ready(local: &Connection) -> Result<(), PullError> {
    for table in READY_TABLES {
        local
            .query_row(TABLE_EXISTS_SQL, [table], |_| Ok(()))
            .optional()?
            .ok_or(PullError::LocalNotReady { table })?;
    }
    Ok(())
}

/// The SQL for one table, built once per pull.
struct Plan<'a> {
    table: &'a TableDef,
    key_index: usize,
    page_sql: String,
    upsert_sql: String,
}

impl<'a> Plan<'a> {
    fn new(table: &'a TableDef) -> Self {
        let names: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
        let key = table.key_columns[0];
        let key_index = names.iter().position(|n| *n == key).unwrap_or_default();
        let page_sql = format!(
            "SELECT {names}, pushed_at FROM {remote} WHERE machine = ?
             AND (pushed_at > ? OR (pushed_at = ? AND {key} > ?))
             ORDER BY pushed_at, {key} LIMIT {SYNC_BATCH_ROWS}",
            names = names.join(", "),
            remote = table.remote_table,
        );
        Self {
            table,
            key_index,
            page_sql,
            upsert_sql: upsert_sql(table),
        }
    }
}

fn local_name(column: &ColumnDef) -> &'static str {
    match column.transform {
        Transform::HashRepo => REPO_HASH_COLUMN,
        _ => column.name,
    }
}

/// Repo and branch are not pulled, so they keep their empty defaults.
fn upsert_sql(table: &TableDef) -> String {
    let names: Vec<&str> = table.columns.iter().map(local_name).collect();
    let marks = vec!["?"; names.len() + 2].join(", ");
    let updates: Vec<String> = table
        .columns
        .iter()
        .filter(|c| c.grows)
        .map(|c| format!("{0} = excluded.{0}", c.name))
        .collect();
    let growth = table.growth_column;
    format!(
        "INSERT INTO {LOCAL_REMOTE_TABLE} (machine, {names}, pushed_at) VALUES ({marks})
         ON CONFLICT(machine, {key}) DO UPDATE SET {updates}
         WHERE excluded.{growth} > {LOCAL_REMOTE_TABLE}.{growth}",
        names = names.join(", "),
        key = table.key_columns[0],
        updates = updates.join(", "),
    )
}

fn other_machines(
    transport: &dyn Transport,
    table: &TableDef,
    own: &str,
) -> Result<Vec<String>, PullError> {
    let sql = format!(
        "SELECT DISTINCT machine FROM {} WHERE machine <> ? ORDER BY machine",
        table.remote_table
    );
    let results = transport.execute(&[statement(sql, vec![Value::Text(own.to_string())])])?;
    results[0]
        .rows
        .iter()
        .map(|row| match row.first() {
            Some(Value::Text(machine)) => Ok(machine.clone()),
            _ => Err(PullError::UnexpectedValue { column: "machine" }),
        })
        .collect()
}

fn statement(sql: String, args: Vec<Value>) -> Statement {
    Statement { sql, args }
}

fn pull_machine(
    local: &Connection,
    transport: &dyn Transport,
    plan: &Plan<'_>,
    machine: &str,
    stats: &mut PullStats,
    after_page: &mut dyn FnMut(),
) -> Result<(), PullError> {
    let (stored_at, stored_key) = read_cursor(local, plan.table, machine)?;
    let mut position = (
        stored_at.saturating_sub(SYNC_RESEND_WINDOW_SECONDS),
        stored_key.clone(),
    );
    loop {
        let page = read_page(transport, plan, machine, &position)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        position = (last.pushed_at, last.key.clone());
        commit_page(local, plan, machine, &page, &position, stats)?;
        after_page();
        if page.len() < SYNC_BATCH_ROWS {
            return Ok(());
        }
    }
}

struct PulledRow {
    pushed_at: i64,
    key: String,
    values: Vec<Sql>,
}

fn read_page(
    transport: &dyn Transport,
    plan: &Plan<'_>,
    machine: &str,
    (pushed_at, key): &(i64, String),
) -> Result<Vec<PulledRow>, PullError> {
    let args = vec![
        Value::Text(machine.to_string()),
        Value::Integer(*pushed_at),
        Value::Integer(*pushed_at),
        Value::Text(key.clone()),
    ];
    let results = transport.execute(&[statement(plan.page_sql.clone(), args)])?;
    results[0]
        .rows
        .iter()
        .map(|row| decode_row(plan, row))
        .collect()
}

fn decode_row(plan: &Plan<'_>, row: &[Value]) -> Result<PulledRow, PullError> {
    let width = plan.table.columns.len();
    let (Some(Value::Integer(pushed_at)), Some(Value::Text(key))) =
        (row.get(width), row.get(plan.key_index))
    else {
        return Err(PullError::UnexpectedValue {
            column: "pushed_at",
        });
    };
    Ok(PulledRow {
        pushed_at: *pushed_at,
        key: key.clone(),
        values: row[..width].iter().map(to_sql).collect(),
    })
}

fn to_sql(value: &Value) -> Sql {
    match value {
        Value::Null => Sql::Null,
        Value::Integer(n) => Sql::Integer(*n),
        Value::Real(x) => Sql::Real(*x),
        Value::Text(s) => Sql::Text(s.clone()),
    }
}

/// One short local transaction: the page's upserts and the cursor move together.
fn commit_page(
    local: &Connection,
    plan: &Plan<'_>,
    machine: &str,
    page: &[PulledRow],
    position: &(i64, String),
    stats: &mut PullStats,
) -> Result<(), PullError> {
    let tx = local.unchecked_transaction()?;
    let overlap_sql = format!(
        "SELECT 1 FROM {} WHERE {} = ?1",
        plan.table.local_table, plan.table.key_columns[0]
    );
    let (mut written, mut overlaps) = (0, 0);
    for row in page {
        let mut binds = vec![Sql::Text(machine.to_string())];
        binds.extend(row.values.iter().cloned());
        binds.push(Sql::Integer(row.pushed_at));
        written += tx.execute(&plan.upsert_sql, params_from_iter(binds))?;
        let seen = tx
            .query_row(&overlap_sql, [&row.key], |_| Ok(()))
            .optional()?;
        overlaps += usize::from(seen.is_some());
    }
    tx.execute(
        WRITE_CURSOR_SQL,
        params![machine, plan.table.local_table, position.0, position.1],
    )?;
    tx.commit()?;
    stats.rows_written += written;
    stats.overlaps += overlaps;
    Ok(())
}

fn read_cursor(
    local: &Connection,
    table: &TableDef,
    machine: &str,
) -> Result<(i64, String), PullError> {
    let found = local
        .query_row(READ_CURSOR_SQL, params![machine, table.local_table], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    Ok(found.unwrap_or_default())
}

/// Maps the hash of every known local account to its key, so a pulled row of
/// the same account resolves. A hash with no local account stays unresolved.
fn rebuild_aliases(local: &Connection, table: &TableDef, salt: &str) -> Result<(), PullError> {
    let account = table
        .columns
        .iter()
        .find(|c| c.transform == Transform::HashAccount)
        .map_or("account", |c| c.name);
    let mut keys = column_values(
        local,
        &format!("SELECT DISTINCT {account} FROM {}", table.local_table),
    )?;
    let has_names = local
        .query_row(TABLE_EXISTS_SQL, [NAMES_TABLE], |_| Ok(()))
        .optional()?
        .is_some();
    if has_names {
        keys.extend(column_values(local, READ_NAMES_SQL)?);
    }
    let tx = local.unchecked_transaction()?;
    for key in keys {
        let hash = salted_hash(salt, Domain::Account, &key);
        tx.execute(UPSERT_ALIAS_SQL, params![hash, key])?;
    }
    tx.commit()?;
    Ok(())
}

fn column_values(local: &Connection, sql: &str) -> Result<Vec<String>, PullError> {
    let mut prepared = local.prepare(sql)?;
    let rows = prepared.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}
