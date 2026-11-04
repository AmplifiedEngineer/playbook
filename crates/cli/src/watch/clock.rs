// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one place watch mode reads the wall clock, so tests can move time.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of the current time in epoch seconds.
pub trait Clock {
    fn now(&self) -> i64;
}

/// The real clock.
impl<T: Clock + ?Sized> Clock for &T {
    fn now(&self) -> i64 {
        (**self).now()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    pub fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn a_manual_clock_moves_only_when_advanced() {
        let clock = ManualClock::new(100);
        assert_eq!(clock.now(), 100);
        clock.advance(30);
        assert_eq!(clock.now(), 130);
    }

    #[test]
    fn the_system_clock_is_after_2024() {
        assert!(SystemClock.now() > 1_700_000_000);
    }

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_other_watch_source_reads_the_wall_clock() {
        // Arrange
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/watch");
        let mut files = Vec::new();
        rust_files(&root, &mut files);

        // Act
        let offenders: Vec<_> = files
            .iter()
            .filter(|f| f.file_name().is_some_and(|n| n != "clock.rs"))
            .filter(|f| {
                let text = fs::read_to_string(f).unwrap();
                text.contains("SystemTime::now") || text.contains("Instant::now")
            })
            .collect();

        // Assert
        assert!(offenders.is_empty(), "{offenders:?}");
    }
}
