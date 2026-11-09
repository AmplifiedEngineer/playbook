// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one way to tell the user something on stderr. While the terminal view
//! owns the screen, anything written to stderr smears across its panels, so
//! the view calls [`hold`] first and every notice is kept until [`release`]
//! prints them after the screen is restored.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static HELD: AtomicBool = AtomicBool::new(false);
static BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static OLD_WRITER: Mutex<Option<String>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether notices are being kept back.
pub fn is_held() -> bool {
    HELD.load(Ordering::Relaxed)
}

/// Prints `message` on stderr, or keeps it while the screen is held.
pub fn notice(message: &str) {
    if is_held() {
        lock(&BUFFER).push(message.to_string());
    } else {
        eprintln!("{message}");
    }
}

/// [`notice`], once per process for each distinct message.
pub fn notice_once(message: &str) {
    let first = lock(&SEEN)
        .get_or_insert_with(HashSet::new)
        .insert(message.to_string());
    if first {
        notice(message);
    }
}

/// Records that an older playbook still writes to the old data folder at `dir`.
pub fn old_writer_seen(dir: &str) {
    *lock(&OLD_WRITER) = Some(dir.to_string());
}

/// Starts keeping notices back, for a program that is about to draw a screen.
pub fn hold() {
    HELD.store(true, Ordering::Relaxed);
}

/// Stops keeping notices back and prints what was kept, then the one line
/// about an older playbook when one was seen.
pub fn release() {
    HELD.store(false, Ordering::Relaxed);
    let kept: Vec<String> = std::mem::take(&mut *lock(&BUFFER));
    for line in kept {
        eprintln!("{line}");
    }
    if let Some(dir) = lock(&OLD_WRITER).take() {
        eprintln!(
            "an older playbook is still writing to {dir}; upgrade it (`brew upgrade playbook` or `playbook upgrade`)"
        );
    }
}

/// What a held screen has kept, for tests.
#[cfg(test)]
pub fn kept() -> Vec<String> {
    lock(&BUFFER).clone()
}

#[cfg(test)]
pub fn reset() {
    HELD.store(false, Ordering::Relaxed);
    lock(&BUFFER).clear();
    *lock(&SEEN) = None;
    *lock(&OLD_WRITER) = None;
}

#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_reaches_stderr_while_the_screen_is_held() {
        let _g = lock(&TEST_LOCK);
        reset();
        hold();
        notice("a");
        notice_once("b");
        notice_once("b");
        assert_eq!(kept(), ["a", "b"]);
        assert!(is_held());
        release();
        assert!(kept().is_empty());
        assert!(!is_held());
    }
}
