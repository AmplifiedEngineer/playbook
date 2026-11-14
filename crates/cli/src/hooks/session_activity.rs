// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! UserPromptSubmit, Stop and Notification hook (ADR-0024, stage 3). It records
//! in `sessions.db` whether a turn is running or the session waits for a
//! person. It reads no prompt text, prints nothing and never blocks.

use crate::common::{session_id, Payload};
use crate::sessions::store;

/// The Notification types that mean the session waits for a person.
const NOTICES: [&str; 3] = ["permission_prompt", "idle_prompt", "agent_needs_input"];

/// One store writer: the connection, the session id and the time.
type Writer = fn(&playbook_store::Connection, &str, i64) -> Result<usize, String>;

pub fn run(payload: &Payload) {
    let id = session_id(payload);
    if id.is_empty() {
        return;
    }
    let write: Writer = match payload.field(".hook_event_name").as_str() {
        "UserPromptSubmit" => store::prompt,
        "Stop" => store::stop,
        "Notification" if NOTICES.contains(&payload.field(".notification_type").as_str()) => {
            store::waiting
        }
        _ => return,
    };
    // A database error is dropped: a hook that blocks a session is worse than
    // a missing label.
    if let Ok(conn) = store::open() {
        let _ = write(&conn, &id, crate::common::time::now_secs());
    }
}
