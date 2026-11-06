// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Clean up what earlier releases left behind. They gave each session a
//! throwaway plugin of effort variants, `playbook-variants-<pid>` in the temp
//! dir (ADR-0017), and removed it when the session ended. A session that was
//! killed left its folder. Variants are gone now (the `Agent` call takes an
//! `effort`), so the launcher removes any such folder whose process is no
//! longer running. A folder of a session that is still running is kept.

use std::fs;
use std::path::Path;

/// The folder name an earlier release gave the throwaway plugin.
const PREFIX: &str = "playbook-variants-";

/// Remove the `playbook-variants-<pid>` folders under `temp` that no running
/// process owns. Any failure is ignored: the launcher must start regardless.
pub fn remove_leftovers(temp: &Path) {
    remove_with(temp, &crate::worktree::pid_is_alive);
}

fn remove_with(temp: &Path, alive: &dyn Fn(u32) -> bool) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(PREFIX))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        // A symlink or a plain file is not ours to remove.
        if !path.is_dir() || path.is_symlink() || alive(pid) {
            continue;
        }
        let _ = fs::remove_dir_all(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    #[test]
    fn a_folder_of_a_dead_session_is_removed_and_a_live_one_is_kept() {
        let tmp = scratch_dir("stale-variants");
        fs::create_dir_all(tmp.join("playbook-variants-111/agents")).unwrap();
        fs::create_dir_all(tmp.join("playbook-variants-222/agents")).unwrap();
        remove_with(&tmp, &|pid| pid == 222);
        assert!(!tmp.join("playbook-variants-111").exists());
        assert!(tmp.join("playbook-variants-222").exists());
    }

    #[test]
    fn other_names_and_files_are_left_alone() {
        let tmp = scratch_dir("stale-variants-other");
        fs::create_dir_all(tmp.join("playbook-variants-abc")).unwrap();
        fs::create_dir_all(tmp.join("playbook-other-1")).unwrap();
        fs::write(tmp.join("playbook-variants-333"), "x").unwrap();
        remove_with(&tmp, &|_| false);
        assert!(tmp.join("playbook-variants-abc").exists());
        assert!(tmp.join("playbook-other-1").exists());
        assert!(tmp.join("playbook-variants-333").exists());
    }

    #[test]
    fn a_missing_temp_dir_is_not_an_error() {
        remove_leftovers(&scratch_dir("stale-variants-none").join("nope"));
    }
}
