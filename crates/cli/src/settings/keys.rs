// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Single-sourced allowlist for `settings.shared.json`: the only keys `gen`
//! may keep and `check` may accept, so both agree on what ships.

/// The `settings.shared.json` top-level keys that may ship: the three things
/// `playbook setup` applies for a part the user agrees to. `hooks` is the guard
/// entries, `statusLine` is the status line and `permissions` is the security
/// defaults. Display options, notification toggles and `env` entries are
/// personal preferences that no hook or launcher depends on, so they never ship.
pub const SHIPPABLE_KEYS: &[&str] = &["permissions", "hooks", "statusLine"];

/// The security section of the template: the whole `permissions` key. A fresh
/// `playbook setup` leaves it out unless the user opts in, see
/// `init::run::neutralise_security`. Update control (the autoupdater) is left
/// to the user, `playbook upgrade` and Homebrew.
pub const SECURITY_KEYS: &[&str] = &["permissions"];

/// The `permissions` block of a seed when it is an object with a non-empty
/// `allow` array, the shape `gen` keeps and `check` accepts.
pub fn shipped_permissions(seed: &serde_json::Value) -> Option<&serde_json::Value> {
    seed.get("permissions").filter(|block| {
        block
            .get("allow")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|allow| !allow.is_empty())
    })
}
