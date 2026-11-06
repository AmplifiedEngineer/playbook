// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The line that names the release notes on the first run after an upgrade
//! (ADR 0037 decision 5.3). The `state` table keeps `notices/binary`, the
//! last version whose line was shown, so each step that runs the ADR 0034
//! migrations shows the line once.
//!
//! A step reads what to show with `prepare`, runs the heal, then calls
//! `Pending::commit`, which moves the key in one transaction and returns the
//! line only when this process moved it. Two processes that start together
//! show the line once between them.

use crate::update::resolve::compare;
use std::cmp::Ordering;
use std::path::Path;

const BINARY_KEY: &str = "notices/binary";

/// The line the detached plugin refresh leaves for the next session.
pub const PLUGIN_UPDATE_KEY: &str = "notices/plugin-update";

/// The summary the detached skills refresh leaves for the next session.
pub const SKILLS_SYNC_KEY: &str = "notices/skills-sync";

/// The key of ADR 0035 plugin notices, which nothing reads now. The first
/// move clears it.
const OLD_PLUGIN_KEY: &str = "notices/plugin";

/// What a step shows: the line, or nothing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Notices {
    pub line: Option<String>,
    /// A second line for the same step, set by the caller after `commit`
    /// when a separate `pbtop` is older than this binary. It shows only with
    /// `line`, so it is shown once.
    pub pbtop: Option<String>,
}

impl Notices {
    /// The lines with their newlines, or an empty string.
    pub fn render(&self) -> String {
        self.line
            .iter()
            .chain(self.pbtop.iter().filter(|_| self.line.is_some()))
            .map(|l| format!("{l}\n"))
            .collect()
    }
}

/// The key move one step will make, read before its heal moves the record.
#[derive(Debug, Default)]
pub struct Pending {
    /// The key before the step, and the version to move it to.
    change: Option<(Option<String>, String)>,
    /// The line to show when this process moves the key.
    line: Option<String>,
}

/// The value of `key`. `Err` when the store cannot be read, so the caller
/// shows and records nothing and the next step tries again.
fn shown(root: &Path, key: &str) -> Result<Option<String>, ()> {
    Ok(crate::state::list_read_only(root, key)
        .map_err(|_| ())?
        .into_iter()
        .find_map(|(k, v)| (k == key && !v.is_empty()).then_some(v)))
}

/// Whether an older binary ran here: it left a migration record or old
/// state files, though 0.26 and earlier write no version record. Call it
/// before anything opens the store for writing, since that imports and
/// renames the old files.
pub fn older_binary_ran(root: &Path) -> bool {
    crate::state::pending_legacy(root) > 0
        || crate::state::list_read_only(root, "migrations/").is_ok_and(|rows| !rows.is_empty())
}

/// What the step will show, read without writing.
///
/// The start is `notices/binary`, else `binary` (`versions/binary`). With
/// neither, a home where `older` says an older binary ran shows the line
/// without a start, and a fresh install records this binary and shows
/// nothing. A start at or above this binary shows nothing.
pub fn prepare(root: &Path, binary: Option<&str>, older: bool) -> Pending {
    let running = env!("CARGO_PKG_VERSION");
    let Ok(before) = shown(root, BINARY_KEY) else {
        return Pending::default();
    };
    if before.as_deref() == Some(running) {
        return Pending::default();
    }
    let line = match before.as_deref().or(binary) {
        Some(from) => match compare(from, running) {
            Ordering::Less => Some(crate::release_notes::updated_line(Some(from), running)),
            Ordering::Equal => None,
            Ordering::Greater => return Pending::default(),
        },
        None if older => Some(crate::release_notes::updated_line(None, running)),
        None => None,
    };
    Pending {
        change: Some((before, running.to_string())),
        line,
    }
}

/// Records `version` as shown, for a caller that printed the notes of that
/// release itself (ADR 0037): `playbook update` and the install script.
/// Best effort.
pub fn record(root: &Path, version: &str) {
    let _ = crate::state::set(root, BINARY_KEY, version);
}

/// Keeps the outcome line of a detached refresh under `key` until a session
/// shows it. Best effort.
pub fn store_notice(root: &Path, key: &str, line: &str) {
    let _ = crate::state::set(root, key, line);
}

/// Reads and deletes the line stored under `key` in one transaction, so two
/// sessions show it once between them.
pub fn take_notice(root: &Path, key: &str) -> Option<String> {
    // Most sessions have none, so look first without taking the write lock.
    let stored = crate::state::list_read_only(root, key).ok()?;
    if !stored.iter().any(|(k, v)| k == key && !v.is_empty()) {
        return None;
    }
    crate::state::transaction(root, |tx| {
        let line = tx.get(key)?.filter(|v| !v.is_empty());
        if line.is_some() {
            tx.delete(key)?;
        }
        Ok(line)
    })
    .unwrap_or(None)
}

impl Pending {
    /// Moves the key in one transaction, if no other process moved it
    /// first, and returns the line when this process moved it.
    pub fn commit(self, root: &Path) -> Notices {
        let Some((before, to)) = self.change else {
            return Notices::default();
        };
        let moved = crate::state::transaction(root, |tx| {
            if tx.get(BINARY_KEY)?.filter(|v| !v.is_empty()) == before {
                tx.set(BINARY_KEY, &to)?;
                tx.delete(OLD_PLUGIN_KEY)?;
                return Ok(true);
            }
            Ok(false)
        })
        .unwrap_or(false);
        Notices {
            line: self.line.filter(|_| moved),
            pbtop: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_playbook_root as root;

    const RUNNING: &str = env!("CARGO_PKG_VERSION");

    fn line(root: &Path, recorded: Option<&str>, older: bool) -> Option<String> {
        prepare(root, recorded, older).commit(root).line
    }

    #[test]
    fn the_pbtop_line_follows_the_release_line_and_never_shows_alone() {
        let both = Notices {
            line: Some("playbook updated from 0.1.0 to 0.2.0".into()),
            pbtop: Some("pbtop 0.1.0 is older".into()),
        };
        let alone = Notices {
            line: None,
            pbtop: Some("pbtop 0.1.0 is older".into()),
        };

        assert_eq!(
            both.render(),
            "playbook updated from 0.1.0 to 0.2.0\npbtop 0.1.0 is older\n"
        );
        assert_eq!(alone.render(), "");
    }

    #[test]
    fn a_stored_notice_is_taken_once() {
        // Arrange
        let root = root("notices-take");
        store_notice(&root, PLUGIN_UPDATE_KEY, "plugin: updated x");

        // Act
        let first = take_notice(&root, PLUGIN_UPDATE_KEY);
        let second = take_notice(&root, PLUGIN_UPDATE_KEY);

        // Assert
        assert_eq!(first.as_deref(), Some("plugin: updated x"));
        assert_eq!(second, None);
    }

    #[test]
    fn a_fresh_install_shows_nothing_and_records_this_binary() {
        // Arrange
        let root = root("notices-fresh");

        // Act
        let first = line(&root, None, false);

        // Assert
        assert_eq!(first, None);
        assert_eq!(shown(&root, BINARY_KEY), Ok(Some(RUNNING.into())));
    }

    #[test]
    fn an_upgrade_shows_the_line_once() {
        // Arrange
        let root = root("notices-upgrade");

        // Act
        let first = line(&root, Some("0.25.0"), false);
        let again = line(&root, Some("0.25.0"), false);

        // Assert
        assert_eq!(
            first,
            Some(crate::release_notes::updated_line(Some("0.25.0"), RUNNING))
        );
        assert_eq!(again, None);
        assert_eq!(shown(&root, BINARY_KEY), Ok(Some(RUNNING.into())));
    }

    #[test]
    fn the_first_move_clears_the_old_plugin_key() {
        // Arrange
        let root = root("notices-plugin-key");
        crate::state::set(&root, OLD_PLUGIN_KEY, "0.30.0").unwrap();

        // Act
        line(&root, Some("0.25.0"), false);

        // Assert
        assert_eq!(shown(&root, OLD_PLUGIN_KEY), Ok(None));
    }

    #[test]
    fn a_home_of_an_older_binary_without_a_record_shows_the_line_without_a_start() {
        // Arrange
        let root = root("notices-old");

        // Act
        let first = line(&root, None, true);

        // Assert
        assert_eq!(
            first,
            Some(crate::release_notes::updated_line(None, RUNNING))
        );
    }

    #[test]
    fn the_shown_version_wins_and_a_downgrade_shows_and_writes_nothing() {
        // Arrange
        let root = root("notices-order");
        crate::state::set(&root, BINARY_KEY, "999.0.0").unwrap();

        // Act
        let shown_line = line(&root, Some("0.0.1"), true);

        // Assert
        assert_eq!(shown_line, None);
        assert_eq!(shown(&root, BINARY_KEY), Ok(Some("999.0.0".into())));
    }

    #[test]
    fn a_caller_that_recorded_the_version_leaves_nothing_to_show() {
        // Arrange
        let root = root("notices-recorded");

        // Act
        record(&root, RUNNING);
        let after = line(&root, Some("0.25.0"), true);

        // Assert
        assert_eq!(after, None);
    }

    #[test]
    fn only_the_process_that_moves_the_key_shows_the_line() {
        // Arrange: two steps read the same start before either commits.
        let root = root("notices-race");
        let first = prepare(&root, Some("0.25.0"), false);
        let second = prepare(&root, Some("0.25.0"), false);

        // Act
        let shown_first = first.commit(&root);
        let shown_second = second.commit(&root);

        // Assert
        assert!(shown_first.line.is_some());
        assert_eq!(shown_second.line, None);
    }
}
