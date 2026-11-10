// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Atomic counter increment. Ports `incr_counter` (the retired shell original)
//! and `_incr_counter` (the retired shell original), including their mkdir-lock
//! plus temp-file-swap semantics and the up-to-50-tries retry behaviour.

use crate::common::atomic::with_dir_lock;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Atomically increment the integer stored in `file`, returning the new
/// value. A missing or unparsable file starts from 0. Never panics; any
/// failure along the way leaves the counter unresolved and best-effort, the
/// same fail-soft contract common.py's `incr_counter` has.
pub fn incr_counter(file: &str) -> i64 {
    incr_counter_waiting(file, LOCK_TRIES)
}

/// How many times a caller tries the lock before it runs unguarded, and the
/// pause between tries. Together they give a contended caller about half a
/// second.
const LOCK_TRIES: u32 = 50;
const LOCK_PAUSE: Duration = Duration::from_millis(10);

/// [`incr_counter`] with the number of lock tries chosen by the caller, so a
/// test of the lock can wait out a busy machine instead of hitting the
/// fail-open path.
fn incr_counter_waiting(file: &str, tries: u32) -> i64 {
    let path = Path::new(file);
    let lock_path = Path::new(&format!("{file}.lock")).to_path_buf();
    let (acquired, next) = with_dir_lock(&lock_path, tries, LOCK_PAUSE, || {
        let current = fs::read_to_string(path)
            .ok()
            .and_then(|contents| contents.trim().parse::<i64>().ok())
            .unwrap_or(0);
        // Python's int is arbitrary precision; i64 is not. Saturate rather
        // than silently wrapping to a negative counter when a corrupted or
        // hand-edited counter file holds a value at or near i64::MAX.
        let next = current.saturating_add(1);
        write_atomically(path, &next.to_string());
        next
    });
    // Only release a lock this call took: removing a live holder's lock would
    // let two writers in at once. A stale lock is cleared on acquire.
    if acquired {
        let _ = fs::remove_dir(&lock_path);
    }
    next
}

/// Write `contents` to `path` via a temp file in the same directory plus a
/// rename, so a reader never observes a partially written file. Never
/// panics; a failed write leaves the previous file content in place.
fn write_atomically(path: &Path, contents: &str) {
    let _ = crate::common::atomic::write_atomic(path, contents);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    /// Tries a contended test caller makes: 60 seconds at [`LOCK_PAUSE`].
    const PATIENT_LOCK_TRIES: u32 = 6_000;

    #[test]
    fn missing_file_starts_at_one() {
        // Arrange
        let root = scratch_dir("counter-missing-file");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");

        // Act
        let got = incr_counter(file.to_str().unwrap());

        // Assert
        assert_eq!(got, 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "1");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_call_returns_two() {
        // Arrange
        let root = scratch_dir("counter-second-call");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");
        incr_counter(file.to_str().unwrap());

        // Act
        let got = incr_counter(file.to_str().unwrap());

        // Assert
        assert_eq!(got, 2);
        assert_eq!(fs::read_to_string(&file).unwrap(), "2");

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_foreign_lock_survives_and_a_stale_one_is_cleared() {
        // Arrange
        let root = scratch_dir("counter-foreign-lock");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");
        let lock = root.join("cnt.lock");
        fs::create_dir_all(&lock).unwrap();

        // Act
        let first = incr_counter(file.to_str().unwrap());
        let live = lock.exists();
        let past = std::time::SystemTime::now() - Duration::from_secs(60);
        fs::File::open(&lock).unwrap().set_modified(past).unwrap();
        let second = incr_counter(file.to_str().unwrap());

        // Assert
        assert_eq!((first, second), (1, 2));
        assert!(live, "a live holder's lock must not be removed");
        assert!(!lock.exists(), "a stale lock is cleared and released");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lock_directory_removed_after_call() {
        // Arrange
        let root = scratch_dir("counter-lock-dir");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");

        // Act
        incr_counter(file.to_str().unwrap());

        // Assert
        let lock_dir = root.join("cnt.lock");
        assert!(!lock_dir.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pre_seeded_file_increments_from_existing_value() {
        // Arrange
        let root = scratch_dir("counter-pre-seeded");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");
        fs::write(&file, "41").unwrap();

        // Act
        let got = incr_counter(file.to_str().unwrap());

        // Assert
        assert_eq!(got, 42);
        assert_eq!(fs::read_to_string(&file).unwrap(), "42");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn counter_at_i64_max_saturates_instead_of_wrapping() {
        // Arrange
        let root = scratch_dir("counter-overflow");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");
        fs::write(&file, i64::MAX.to_string()).unwrap();

        // Act
        let got = incr_counter(file.to_str().unwrap());

        // Assert: a wrapping add would go negative here.
        assert_eq!(got, i64::MAX);
        assert_eq!(fs::read_to_string(&file).unwrap(), i64::MAX.to_string());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_increments_do_not_lose_a_count() {
        // Arrange
        let root = scratch_dir("counter-concurrent");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("cnt");
        let file_str = file.to_str().unwrap().to_string();
        let thread_count = 20i64;

        // Act: the lock waits far longer than a loaded machine needs, so no
        // thread falls through to the unguarded path the real wait allows.
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let f = file_str.clone();
                std::thread::spawn(move || incr_counter_waiting(&f, PATIENT_LOCK_TRIES))
            })
            .collect();
        let mut results: Vec<i64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        results.sort_unstable();

        // Assert
        let expected: Vec<i64> = (1..=thread_count).collect();
        assert_eq!(results, expected);
        let final_value: i64 = fs::read_to_string(&file).unwrap().trim().parse().unwrap();
        assert_eq!(final_value, thread_count);

        let _ = fs::remove_dir_all(&root);
    }
}
