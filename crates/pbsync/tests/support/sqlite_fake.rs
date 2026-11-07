// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! An in-memory SQLite database standing in for the remote, so tests run real SQL.

// Each test binary uses a different part of these helpers.
#![allow(dead_code)]

use std::cell::RefCell;

use pbsync::transport::{Statement, StatementResult, Transport, TransportError, Value};
use playbook_store::types::{Value as Sql, ValueRef};
use playbook_store::{params_from_iter, Connection};

pub(crate) struct SqliteFake {
    conn: Connection,
    log: RefCell<Vec<String>>,
}

impl SqliteFake {
    pub(crate) fn new() -> Self {
        Self {
            conn: Connection::open_in_memory().unwrap(),
            log: RefCell::new(Vec::new()),
        }
    }

    /// Runs SQL directly, without logging it, to set up a scenario.
    pub(crate) fn seed(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    /// Every statement the transport ran, in order.
    pub(crate) fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    pub(crate) fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        self.run(sql, &[]).unwrap().rows
    }

    fn run(&self, sql: &str, args: &[Value]) -> Result<StatementResult, String> {
        let mut prepared = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let columns = prepared
            .column_names()
            .iter()
            .map(|c| c.to_string())
            .collect();
        let binds = args.iter().map(to_sql);
        let mut rows = prepared
            .query(params_from_iter(binds))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let width = row.as_ref().column_count();
            out.push(
                (0..width)
                    .map(|i| from_sql(row.get_ref(i).unwrap()))
                    .collect(),
            );
        }
        Ok(StatementResult {
            columns,
            rows: out,
            affected_rows: 0,
        })
    }
}

fn to_sql(value: &Value) -> Sql {
    match value {
        Value::Null => Sql::Null,
        Value::Integer(n) => Sql::Integer(*n),
        Value::Real(x) => Sql::Real(*x),
        Value::Text(s) => Sql::Text(s.clone()),
    }
}

fn from_sql(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Integer(n) => Value::Integer(n),
        ValueRef::Real(x) => Value::Real(x),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
    }
}

impl Transport for SqliteFake {
    fn execute(&self, statements: &[Statement]) -> Result<Vec<StatementResult>, TransportError> {
        statements
            .iter()
            .enumerate()
            .map(|(index, s)| {
                self.log.borrow_mut().push(s.sql.clone());
                self.run(&s.sql, &s.args)
                    .map_err(|message| TransportError::Statement { index, message })
            })
            .collect()
    }
}
