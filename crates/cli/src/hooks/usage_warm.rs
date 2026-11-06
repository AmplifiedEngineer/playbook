// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! SessionStart and SessionEnd hook that warms the usage store. It starts a
//! detached, low priority, time bounded ingest and returns at once, with no
//! output, so it never slows or blocks a session. The plugin-disabled pause is
//! handled before any hook runs. See `usage::warm`.

use crate::common::payload::Payload;
use crate::usage::run::Paths;

pub fn run(payload: &Payload) {
    // A real hook call always names its session; a malformed one does nothing.
    if payload.field(".session_id").is_empty() {
        return;
    }
    let dir = crate::common::paths::usage_db_dir();
    // Only a session start sees the credential the session begins with. The
    // token itself is hashed here and never leaves this call.
    if payload.field(".hook_event_name") == "SessionStart" {
        let _ = std::fs::create_dir_all(&dir);
        crate::usage::session_account::record(
            &dir,
            &payload.field(".session_id"),
            crate::usage::session_account::env_fingerprint().as_deref(),
        );
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let home = crate::common::home_dir();
    let mut paths = Paths::from_home(&home);
    paths.db = dir.join("usage.db");
    crate::usage::warm::maybe_spawn(&home, &paths, &exe);
}
