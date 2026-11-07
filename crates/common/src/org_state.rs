// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! First-run copy of per-user state that older playbooks keyed by the old
//! GitHub organization name.
//!
//! Memory facts, per-repo folders and the org-tier settings files live under
//! `<root>/<kind>/<owner>/...`, where `<owner>` comes from the `origin`
//! remote. After the organization was renamed, `<owner>` is the new name, so
//! the old folders no longer match any repo. [`migrate`] copies each old
//! folder into the new one once. It never moves, overwrites or deletes: an
//! older binary keeps working on the old folders, and a file that already
//! exists under the new name wins.

use crate::org::{LEGACY_ORGS, ORG};
use std::fs;
use std::io;
use std::path::Path;

/// The folders under the playbook root that hold `<owner>/...` state.
pub const OWNER_KEYED_DIRS: [&str; 3] = ["repos", "memory", "orgs"];

/// The file under the root that records a finished copy, with the org name in it.
pub const MARKER: &str = ".org-state-migrated";

/// What one [`migrate`] call did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Files copied.
    pub copied: usize,
    /// Whether the work was already done on an earlier run.
    pub already_done: bool,
}

/// Copy the state of every old organization name into the current one, then
/// write [`MARKER`]. One `stat` and one small read when the work is done. A
/// failure leaves the marker unwritten, so the next run tries again. A root
/// with no old-name folder gets no marker: a read-only command must not write
/// to a machine that has nothing to migrate.
pub fn migrate(root: &Path) -> io::Result<Report> {
    if !root.is_dir() {
        return Ok(Report::default());
    }
    if fs::read_to_string(root.join(MARKER)).is_ok_and(|s| s.trim() == ORG) {
        return Ok(Report {
            copied: 0,
            already_done: true,
        });
    }
    let mut copied = 0;
    let mut found_old_state = false;
    for kind in OWNER_KEYED_DIRS {
        let base = root.join(kind);
        for legacy in LEGACY_ORGS {
            let from = base.join(legacy);
            if from.is_dir() && !fs::symlink_metadata(&from)?.file_type().is_symlink() {
                found_old_state = true;
                copied += copy_missing(&from, &base.join(ORG))?;
            }
        }
    }
    if found_old_state {
        fs::write(root.join(MARKER), format!("{ORG}\n"))?;
    }
    Ok(Report {
        copied,
        already_done: false,
    })
}

/// Copy the files under `from` that do not exist under `to`. Symlinks are
/// skipped. Nothing is overwritten or removed. Returns the number of files copied.
pub fn copy_missing(from: &Path, to: &Path) -> io::Result<usize> {
    fs::create_dir_all(to)?;
    let mut copied = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            copied += copy_missing(&entry.path(), &dest)?;
        } else if kind.is_file() && !dest.exists() {
            fs::copy(entry.path(), &dest)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_dir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn every_old_org_folder_is_copied_to_the_current_one_and_kept() {
        let root = scratch_dir("org-state-copy");
        write(
            &root.join("memory/pragmatic-engineer/playbook/fact.md"),
            "old",
        );
        write(
            &root.join("repos/amplified-engineer/playbook/plans/p.md"),
            "mid",
        );
        write(&root.join("orgs/pragmatic-engineer/config.json"), "{}");

        let report = migrate(&root).unwrap();

        assert_eq!(report.copied, 3);
        assert_eq!(
            fs::read_to_string(root.join(format!("memory/{ORG}/playbook/fact.md"))).unwrap(),
            "old"
        );
        assert!(root
            .join(format!("repos/{ORG}/playbook/plans/p.md"))
            .is_file());
        assert!(root.join(format!("orgs/{ORG}/config.json")).is_file());
        // The old folders stay, so an older binary keeps working.
        assert!(root
            .join("memory/pragmatic-engineer/playbook/fact.md")
            .is_file());
        assert!(root
            .join("repos/amplified-engineer/playbook/plans/p.md")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_under_the_new_name_is_never_overwritten() {
        let root = scratch_dir("org-state-keep");
        write(&root.join("memory/pragmatic-engineer/r/a.md"), "old a");
        write(&root.join("memory/pragmatic-engineer/r/b.md"), "old b");
        write(&root.join(format!("memory/{ORG}/r/a.md")), "new a");

        let report = migrate(&root).unwrap();

        assert_eq!(report.copied, 1);
        assert_eq!(
            fs::read_to_string(root.join(format!("memory/{ORG}/r/a.md"))).unwrap(),
            "new a"
        );
        assert_eq!(
            fs::read_to_string(root.join(format!("memory/{ORG}/r/b.md"))).unwrap(),
            "old b"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_second_run_does_nothing() {
        let root = scratch_dir("org-state-twice");
        write(&root.join("memory/pragmatic-engineer/r/a.md"), "old");
        assert_eq!(migrate(&root).unwrap().copied, 1);
        // A file added to the old folder later is not copied: the work is marked done.
        write(&root.join("memory/pragmatic-engineer/r/late.md"), "late");

        let again = migrate(&root).unwrap();

        assert_eq!(
            again,
            Report {
                copied: 0,
                already_done: true
            }
        );
        assert!(!root.join(format!("memory/{ORG}/r/late.md")).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_machine_with_no_old_state_is_left_untouched() {
        let root = scratch_dir("org-state-none");
        fs::create_dir_all(root.join("memory/acme/repo")).unwrap();

        let report = migrate(&root).unwrap();

        assert_eq!(report.copied, 0);
        assert!(!root.join(MARKER).exists());
        assert!(!root.join(format!("memory/{ORG}")).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_root_is_left_alone() {
        let root = scratch_dir("org-state-missing");
        assert_eq!(migrate(&root).unwrap(), Report::default());
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let root = scratch_dir("org-state-link");
        write(&root.join("memory/pragmatic-engineer/r/a.md"), "a");
        std::os::unix::fs::symlink("/etc/hosts", root.join("memory/pragmatic-engineer/r/link"))
            .unwrap();

        migrate(&root).unwrap();

        assert!(root.join(format!("memory/{ORG}/r/a.md")).is_file());
        assert!(!root.join(format!("memory/{ORG}/r/link")).exists());
        let _ = fs::remove_dir_all(&root);
    }
}
