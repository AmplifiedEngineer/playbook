// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Moves a legacy `statusLine.command` that runs the old `statusline.sh` over
//! to `playbook statusline`. Init no longer places the script.

use crate::common::paths::playbook_root_from;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `$HOME/.config/playbook/statusline.sh`, where older installs placed the script.
pub fn playbook_statusline_path(home: &Path) -> PathBuf {
    playbook_root_from(home).join("statusline.sh")
}

/// The command that renders the status line with the Rust binary.
pub const RUST_COMMAND: &str = "playbook statusline";

/// Seconds between status line runs while Claude Code is idle. Short enough
/// that the prompt cache countdown visibly ticks down.
pub const REFRESH_INTERVAL_SECS: u64 = 5;

/// The key under `statusLine` that sets how often it runs while idle.
pub const REFRESH_KEY: &str = "refreshInterval";

/// Whether a `statusLine` object runs playbook's renderer.
pub(crate) fn runs_playbook(status_line: &serde_json::Value) -> bool {
    status_line
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.trim() == RUST_COMMAND)
}

/// Whether a `statusLine` object that runs playbook has the interval playbook
/// sets. Always true for a status line that is not playbook's.
pub(crate) fn refresh_is_current(status_line: &serde_json::Value) -> bool {
    !runs_playbook(status_line)
        || status_line.get(REFRESH_KEY).and_then(|v| v.as_u64()) == Some(REFRESH_INTERVAL_SECS)
}

/// Sets `refreshInterval` to [`REFRESH_INTERVAL_SECS`] on a `statusLine`
/// object that runs playbook, whatever it was. Playbook owns that interval.
/// Any other status line is left alone. `true` when the object changed.
pub(crate) fn set_refresh(status_line: &mut serde_json::Value) -> bool {
    if refresh_is_current(status_line) {
        return false;
    }
    let Some(map) = status_line.as_object_mut() else {
        return false;
    };
    map.insert(REFRESH_KEY.to_string(), REFRESH_INTERVAL_SECS.into());
    true
}

/// Whether `command` runs the playbook-installed `statusline.sh` through an
/// interpreter. A custom script elsewhere, or any extra argument, is not it.
pub(crate) fn is_legacy_command(command: &str, home: &Path) -> bool {
    let mut tokens = command.split_whitespace();
    let (Some(interp), Some(path), None) = (tokens.next(), tokens.next(), tokens.next()) else {
        return false;
    };
    let home_str = home.to_string_lossy();
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => format!("{home_str}/{rest}"),
        None => path.replace("$HOME", &home_str),
    };
    interp == "bash" && Path::new(&expanded) == playbook_statusline_path(home)
}

/// The parsed settings when their `statusLine.command` runs the playbook
/// `statusline.sh`, else `None` (also for a missing or invalid file).
fn settings_with_legacy_command(settings_path: &Path, home: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(settings_path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    value
        .pointer("/statusLine/command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| is_legacy_command(c, home))
        .then_some(value)
}

/// Whether `migrate_legacy_command` would rewrite `statusLine.command`. Reads
/// only. An edited script is left alone by the rewrite, so it is not pending.
pub fn legacy_command_pending(settings_path: &Path, home: &Path, script_edited: bool) -> bool {
    !script_edited && settings_with_legacy_command(settings_path, home).is_some()
}

/// Point a `statusLine.command` that runs the playbook `statusline.sh` at the
/// Rust renderer, leaving every other value alone. `Ok(true)` when it changed.
/// With `script_edited` the user owns the script, so the command stays and the
/// merge baseline drops its `statusLine` so the template cannot repoint it.
pub fn migrate_legacy_command(
    settings_path: &Path,
    home: &Path,
    script_edited: bool,
) -> io::Result<bool> {
    let Some(mut value) = settings_with_legacy_command(settings_path, home) else {
        return Ok(false);
    };
    let Some(command) = value.pointer_mut("/statusLine/command") else {
        return Ok(false);
    };
    if script_edited {
        unpin_base_status_line(&settings_path.with_file_name(".settings.base.json"))?;
        return Ok(false);
    }
    *command = serde_json::Value::String(RUST_COMMAND.to_string());
    if let Some(line) = value.get_mut("statusLine") {
        set_refresh(line);
    }
    let body = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
    // A symlinked settings.json (stow, chezmoi) stays a symlink.
    let target = fs::canonicalize(settings_path)?;
    crate::common::atomic::write_atomic(&target, format!("{body}\n"))?;
    Ok(true)
}

/// Drop `statusLine` from the merge baseline so the user's value counts as a customisation.
fn unpin_base_status_line(base_path: &Path) -> io::Result<()> {
    let Ok(text) = fs::read_to_string(base_path) else {
        return Ok(());
    };
    let Ok(serde_json::Value::Object(mut base)) = serde_json::from_str(&text) else {
        return Ok(());
    };
    if base.remove("statusLine").is_none() {
        return Ok(());
    }
    let body = serde_json::to_string_pretty(&base).map_err(io::Error::other)?;
    crate::common::atomic::write_atomic(base_path, format!("{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(refresh: Option<serde_json::Value>) -> serde_json::Value {
        let mut v = json!({"type": "command", "command": RUST_COMMAND});
        if let Some(r) = refresh {
            v[REFRESH_KEY] = r;
        }
        v
    }

    #[test]
    fn any_interval_on_playbooks_status_line_becomes_the_playbook_one() {
        for refresh in [None, Some(json!(30)), Some(json!(1)), Some(json!("x"))] {
            let mut v = line(refresh.clone());
            assert!(set_refresh(&mut v), "{refresh:?}");
            assert_eq!(v[REFRESH_KEY], json!(REFRESH_INTERVAL_SECS), "{refresh:?}");
            assert!(refresh_is_current(&v));
        }
    }

    #[test]
    fn the_playbook_interval_is_left_as_it_is() {
        let mut v = line(Some(json!(REFRESH_INTERVAL_SECS)));
        assert!(!set_refresh(&mut v));
    }

    #[test]
    fn a_status_line_that_does_not_run_playbook_is_left_alone() {
        for refresh in [None, Some(30)] {
            let mut v = json!({"type": "command", "command": "bash ~/mine.sh"});
            if let Some(r) = refresh {
                v[REFRESH_KEY] = json!(r);
            }
            let before = v.clone();
            assert!(!set_refresh(&mut v));
            assert_eq!(v, before);
            assert!(refresh_is_current(&v));
        }
    }

    #[test]
    fn the_seed_settings_use_the_same_interval() {
        let seed: serde_json::Value =
            serde_json::from_str(include_str!("../../../../seed/settings.shared.json")).unwrap();
        assert_eq!(
            seed["statusLine"][REFRESH_KEY],
            json!(REFRESH_INTERVAL_SECS)
        );
    }
}
