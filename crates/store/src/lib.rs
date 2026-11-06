// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one place playbook reaches SQLite. The crate keeps the names of the
//! engine behind it, so a caller changes its `use` line and nothing else.

#![allow(clippy::disallowed_types)]

mod retry;

pub use retry::{retry_busy_only, retry_busy_or_locked, retry_with_pause, BUSY_PAUSE, OPEN_BUDGET};

#[cfg(feature = "engine-rusqlite")]
pub use rusqlite::{
    named_params, params, params_from_iter, types, Connection, Error, ErrorCode, OpenFlags,
    OptionalExtension, Result, Row, ToSql, Transaction, TransactionBehavior,
};

/// The engine's raw result codes, such as `SQLITE_BUSY`.
#[cfg(feature = "engine-rusqlite")]
pub use rusqlite::ffi;
