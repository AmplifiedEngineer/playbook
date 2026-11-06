// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Bounded retry of a busy statement. `busy_timeout` alone is not enough on a
//! fresh database: SQLite skips the busy handler when waiting could deadlock,
//! for example while another opener switches the file to WAL, so the statement
//! returns BUSY at once.
//!
//! Two policies exist because two callers grew apart. They are kept as they
//! were and named for the errors they retry.

use std::time::{Duration, Instant};

use crate::{Error, ErrorCode, Result};

/// Pause between retries of a busy statement.
pub const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// How long an opener retries a busy first statement.
pub const OPEN_BUDGET: Duration = Duration::from_millis(5000);

/// Retries `op` while SQLite reports BUSY or LOCKED, until `budget` has
/// elapsed. Any other result, including success, is returned at once.
pub fn retry_busy_or_locked<T>(op: impl FnMut() -> Result<T>, budget: Duration) -> Result<T> {
    retry_with_pause(
        op,
        budget,
        &[ErrorCode::DatabaseBusy, ErrorCode::DatabaseLocked],
        || std::thread::sleep(BUSY_PAUSE),
    )
}

/// Retries `op` while SQLite reports BUSY only, until `budget` has elapsed.
pub fn retry_busy_only<T>(op: impl FnMut() -> Result<T>, budget: Duration) -> Result<T> {
    retry_with_pause(op, budget, &[ErrorCode::DatabaseBusy], || {
        std::thread::sleep(BUSY_PAUSE);
    })
}

/// The shared loop. `pause` runs between attempts, so a test can inject its own.
pub fn retry_with_pause<T>(
    mut op: impl FnMut() -> Result<T>,
    budget: Duration,
    retry_on: &[ErrorCode],
    mut pause: impl FnMut(),
) -> Result<T> {
    let start = Instant::now();
    loop {
        match op() {
            Err(Error::SqliteFailure(err, _))
                if retry_on.contains(&err.code) && start.elapsed() < budget =>
            {
                pause();
            }
            other => return other,
        }
    }
}
