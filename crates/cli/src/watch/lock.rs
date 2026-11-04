// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one-instance lock. The OS holds it for the life of the process and
//! drops it on any exit, so a held lock always means a live holder and there
//! is no stale lock to break (the same kind of lock ADR 0048 uses).

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// A held lock. Dropping it releases it.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// The result of asking for the lock.
#[derive(Debug)]
pub enum Acquire {
    Held(InstanceLock),
    /// Another process (or another handle) holds it.
    Busy,
}

/// Takes the lock at `path` without waiting.
pub fn acquire(path: &Path) -> io::Result<Acquire> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Acquire::Held(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(Acquire::Busy),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_path(tag: &str) -> std::path::PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("watch.lock")
    }

    #[test]
    fn a_second_acquire_while_held_is_busy_and_does_not_wait() {
        // Arrange
        let path = lock_path("watch-lock-busy");
        let first = acquire(&path).unwrap();

        // Act
        let second = acquire(&path).unwrap();

        // Assert
        assert!(matches!(first, Acquire::Held(_)));
        assert!(matches!(second, Acquire::Busy));
    }

    #[test]
    fn a_lock_file_left_by_a_killed_process_does_not_block() {
        // Arrange: the file exists and no handle holds it.
        let path = lock_path("watch-lock-stale");
        std::fs::write(&path, "4242").unwrap();

        // Act
        let result = acquire(&path).unwrap();

        // Assert
        assert!(matches!(result, Acquire::Held(_)));
    }

    #[test]
    fn dropping_the_lock_releases_it() {
        let path = lock_path("watch-lock-release");
        drop(acquire(&path).unwrap());
        assert!(matches!(acquire(&path).unwrap(), Acquire::Held(_)));
    }
}
