// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which `usage.db` layouts this pbsync can sync from. Checked before anything
//! is written to the remote.

use std::fmt;

use playbook_store::schema::stored_version;
use playbook_store::Connection;

/// The newest `usage.db` layout this pbsync understands. A database stamped
/// above it may have changed a table that sync reads or writes.
pub const MAX_USAGE_DB_SCHEMA_VERSION: i64 = 5;
/// The first `usage.db` layout that has the sync tables.
pub const MIN_USAGE_DB_SCHEMA_VERSION: i64 = 5;

const USAGE_DB_NAME: &str = "usage.db";

#[derive(Debug, PartialEq, Eq)]
pub enum LocalSchemaError {
    /// The database was written by a newer playbook.
    Newer {
        found: i64,
    },
    /// The database has no sync tables yet: it is unstamped or older.
    NotReady {
        found: Option<i64>,
    },
    Unreadable {
        reason: String,
    },
}

impl fmt::Display for LocalSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newer { found } => write!(
                f,
                "usage.db is on schema version {found} but this pbsync understands up to {MAX_USAGE_DB_SCHEMA_VERSION}: update pbsync; nothing was sent"
            ),
            Self::NotReady { found } => write!(
                f,
                "usage.db is on schema version {} but sync needs {MIN_USAGE_DB_SCHEMA_VERSION}: run any playbook command so the database migrates",
                found.map_or_else(|| "none".to_string(), |v| v.to_string())
            ),
            Self::Unreadable { reason } => write!(f, "cannot read the usage.db version: {reason}"),
        }
    }
}

impl std::error::Error for LocalSchemaError {}

pub fn check(conn: &Connection) -> Result<(), LocalSchemaError> {
    let found = stored_version(conn, USAGE_DB_NAME).map_err(|e| LocalSchemaError::Unreadable {
        reason: e.to_string(),
    })?;
    match found {
        Some(version) if version > MAX_USAGE_DB_SCHEMA_VERSION => {
            Err(LocalSchemaError::Newer { found: version })
        }
        Some(version) if version >= MIN_USAGE_DB_SCHEMA_VERSION => Ok(()),
        other => Err(LocalSchemaError::NotReady { found: other }),
    }
}
