// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared SQLite helpers. `busy_timeout` alone is not enough on a fresh
//! database: SQLite skips the busy handler when waiting could deadlock, for
//! example while another opener switches the file to WAL, so the statement
//! returns BUSY at once. [`retry_busy`] retries that one error for a bounded
//! time, and every opener that can race another process uses it around the
//! journal mode switch and the schema batch.

// `usage` keeps its own copy of this helper: it may not import `crate::common`
// (see `tests/usage_boundary.rs`).
use std::time::{Duration, Instant};

/// Pause between retries of a busy statement.
const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// How long an opener retries a busy first statement.
pub const OPEN_BUDGET: Duration = Duration::from_millis(5000);

/// Retries `op` while SQLite reports BUSY or LOCKED, until `budget` has
/// elapsed. Any other result, including success, is returned at once.
pub fn retry_busy<T>(
    mut op: impl FnMut() -> playbook_store::Result<T>,
    budget: Duration,
) -> playbook_store::Result<T> {
    let start = Instant::now();
    loop {
        match op() {
            Err(playbook_store::Error::SqliteFailure(err, _))
                if matches!(
                    err.code,
                    playbook_store::ErrorCode::DatabaseBusy
                        | playbook_store::ErrorCode::DatabaseLocked
                ) && start.elapsed() < budget =>
            {
                std::thread::sleep(BUSY_PAUSE);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> playbook_store::Error {
        playbook_store::Error::SqliteFailure(
            playbook_store::ffi::Error::new(playbook_store::ffi::SQLITE_BUSY),
            None,
        )
    }

    #[test]
    fn a_busy_statement_is_retried_until_it_succeeds() {
        let mut calls = 0;

        let got = retry_busy(
            || {
                calls += 1;
                if calls < 3 {
                    Err(busy())
                } else {
                    Ok(calls)
                }
            },
            Duration::from_secs(5),
        );

        assert_eq!(got.ok(), Some(3));
    }

    #[test]
    fn another_error_is_returned_without_a_retry() {
        let mut calls = 0;

        let got: playbook_store::Result<()> = retry_busy(
            || {
                calls += 1;
                Err(playbook_store::Error::QueryReturnedNoRows)
            },
            Duration::from_secs(5),
        );

        assert!(got.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_statement_that_stays_busy_gives_up_after_the_budget() {
        let started = Instant::now();

        let got: playbook_store::Result<()> =
            retry_busy(|| Err(busy()), Duration::from_millis(100));

        assert!(got.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
