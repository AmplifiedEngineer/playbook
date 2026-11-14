// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Stop / SessionEnd hook (ports the retired shell original): records in
//! `sessions.db` that a turn stopped or that the session ended, so a later
//! session start can tell an abandoned session from a finished one.
//!
//! Wired to both `Stop` (fires after every assistant turn) and `SessionEnd`
//! (fires once, when the session truly ends) from the same script. Only
//! `SessionEnd` carries a `.reason`, and only when that reason is present and
//! not `"other"` does this hook record the end and consider queuing the
//! auto-learn nudge; that presence-and-not-other check is how the two events
//! are told apart from inside one script.

use crate::common::atomic::with_dir_lock;
use crate::common::slug::slugify;
use crate::common::{session_dir, session_id, Payload};
use crate::hooks::git_toplevel;
use crate::sessions::store;
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::time::Duration;

const DEFAULT_AUTO_LEARN_MIN_EDITS: i64 = 5;

/// Run the session-clean-exit hook. Never panics; every failure along the
/// way is swallowed, matching the retired shell original's fail-soft
/// contract.
pub fn run(payload: &Payload) {
    let dir = session_dir(payload);
    if dir.is_empty() {
        return;
    }

    let reason = payload.field(".reason");
    let ended = !reason.is_empty() && reason != "other";
    record(&session_id(payload), ended.then_some(reason.as_str()));
    if !ended {
        return;
    }

    if reason == "clear" {
        crate::handoff::mark_cleared(&session_id(payload));
    }
    queue_auto_learn(payload, &dir);
}

/// Record the end with `reason`, or a stop when there is none. A database
/// error is dropped: a hook that blocks a session is worse than a missing label.
fn record(id: &str, reason: Option<&str>) {
    let now = crate::common::time::now_secs();
    let Ok(conn) = store::open() else {
        return;
    };
    let _ = match reason {
        Some(reason) => store::end(&conn, id, reason, now),
        None => store::stop(&conn, id, now),
    };
}

/// Queue an auto-learn flag for this repo if the session made enough edits.
/// Matches the retired shell original.
fn queue_auto_learn(payload: &Payload, dir: &str) {
    if std::env::var("AUTO_LEARN_NUDGE").unwrap_or_else(|_| "1".to_string()) == "0" {
        return;
    }
    let root = git_toplevel();
    if root.is_empty() {
        return;
    }

    let edits = read_int(&Path::new(dir).join("edit-count"));
    // Trim before parsing: python's `int(...)` strips surrounding whitespace,
    // so a padded value like " 3" must still mean 3 here rather than falling
    // back to the default. Matches the retired shell original.
    let threshold = std::env::var("AUTO_LEARN_MIN_EDITS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_AUTO_LEARN_MIN_EDITS);
    if edits < threshold {
        return;
    }

    let qdir = crate::common::paths::runtime_root().join("to-learn");
    if fs::create_dir_all(&qdir).is_err() {
        return;
    }

    let now = crate::common::time::now_epoch_secs();
    let flag = AutoLearnFlag {
        repo_root: &root,
        edits,
        session_id: &session_id(payload),
        ts: now,
    };
    let Ok(rendered) = serde_json::to_string(&flag) else {
        return;
    };

    let slug = slugify(&root);
    let dest = qdir.join(format!("{slug}.json"));
    // Repo-shared, not session-scoped: two sessions in the same repo can
    // queue this near the same moment. `write_atomic` gives each writer its
    // own temp file, and the lock serializes the two so the rename that lands
    // last is a complete, independent write.
    let lock_path = qdir.join(format!("{slug}.json.lock"));
    let (acquired, ()) = with_dir_lock(&lock_path, 50, Duration::from_millis(10), || {
        let _ = crate::common::atomic::write_atomic(&dest, rendered);
    });
    if acquired {
        let _ = fs::remove_dir(&lock_path);
    }
}

#[derive(Serialize)]
struct AutoLearnFlag<'a> {
    repo_root: &'a str,
    edits: i64,
    session_id: &'a str,
    ts: u64,
}

/// Parse the digits in `path`'s contents as an integer, ignoring any other
/// characters. `0` on any read or parse failure. Matches
/// the retired shell original.
fn read_int(path: &Path) -> i64 {
    let Ok(contents) = fs::read_to_string(path) else {
        return 0;
    };
    let digits: String = contents.chars().filter(char::is_ascii_digit).collect();
    digits.parse::<i64>().unwrap_or(0)
}
