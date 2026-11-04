// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The stores that clean up small leftovers under the playbook root: session
//! folders and the markers of old storage migrations.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Action, Due, Item, Limits, Store};
use crate::common::paths::playbook_root_from;
use crate::config::keys;

/// A folder with more entries than this counts as active, so a huge tree never
/// makes one scan slow.
pub const AGE_WALK_MAX_ENTRIES: usize = 10_000;
/// How deep the leftover search goes below its root.
pub const SCAN_MAX_DEPTH: usize = 6;
const SECS_PER_DAY: i64 = 86_400;
/// Markers that a storage migration leaves behind.
const LEFTOVER_NAMES: [&str; 2] = [".copied", "MOVED.txt"];

/// Size and newest modification time of a file or folder tree.
struct Walked {
    bytes: u64,
    newest_mtime: i64,
    /// The tree held more than [`AGE_WALK_MAX_ENTRIES`] entries.
    capped: bool,
}

fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Walks `path` without following symlinks, so a link planted in a store never
/// pulls the walk (or an age) from outside it.
fn walk(path: &Path) -> Option<Walked> {
    let top = fs::symlink_metadata(path).ok()?;
    let mut out = Walked {
        bytes: top.len(),
        newest_mtime: mtime_secs(&top),
        capped: false,
    };
    let mut pending = vec![path.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > AGE_WALK_MAX_ENTRIES {
                out.capped = true;
                return Some(out);
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            out.bytes += meta.len();
            out.newest_mtime = out.newest_mtime.max(mtime_secs(&meta));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Some(out)
}

/// An item for `path` that is due once it has been idle for the store's
/// `idle_days`. A capped walk counts as active.
fn aged_item(store: &Store, path: PathBuf, now: i64) -> Option<Item> {
    let walked = walk(&path)?;
    let newest_mtime = if walked.capped {
        now
    } else {
        walked.newest_mtime
    };
    let idle_secs = i64::from(store.limits.idle_days) * SECS_PER_DAY;
    let due = (now - newest_mtime >= idle_secs).then(|| Due {
        reason: format!("idle for {} days or more", store.limits.idle_days),
        retain_days: store.limits.retain_days,
    });
    Some(Item {
        path,
        bytes: walked.bytes,
        newest_mtime,
        due,
    })
}

/// Each entry directly under a root of `store`, except symlinks.
fn scan_children(store: &Store, now: i64) -> Vec<Item> {
    let mut items = Vec::new();
    for root in &store.roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| !kind.is_symlink()) {
                items.extend(aged_item(store, entry.path(), now));
            }
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    items
}

fn find_leftovers(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        if LEFTOVER_NAMES.iter().any(|leftover| name == *leftover) {
            found.push(entry.path());
        } else if kind.is_dir() && depth < SCAN_MAX_DEPTH {
            find_leftovers(&entry.path(), depth + 1, found);
        }
    }
}

/// The `.copied` markers and `MOVED.txt` notes below each root.
fn scan_leftovers(store: &Store, now: i64) -> Vec<Item> {
    let mut paths = Vec::new();
    for root in &store.roots {
        find_leftovers(root, 0, &mut paths);
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| aged_item(store, path, now))
        .collect()
}

/// The configured number for `key`, or its default when it cannot be read.
fn configured_days(home: &Path, key: &'static str) -> u32 {
    let from_config = crate::config::resolve_valid(key, home, None)
        .ok()
        .and_then(|(value, _, _)| keys::gc_number_or_default(key, &value));
    let days = from_config.or_else(|| keys::gc_number(key).map(|entry| entry.default));
    days.and_then(|n| u32::try_from(n).ok()).unwrap_or(1)
}

fn store(
    name: &'static str,
    root: PathBuf,
    scan: super::ScanFn,
    idle_key: &'static str,
    home: &Path,
) -> Store {
    Store {
        name,
        roots: vec![root],
        scan,
        action: Action::Tombstone,
        retention_key: Some(idle_key),
        limits: Limits {
            idle_days: configured_days(home, idle_key),
            max_bytes: 0,
            retain_days: configured_days(home, "gc.graceDays"),
        },
    }
}

/// The stores of this unit, with their limits read from the config under
/// `home`.
pub fn small_stores(home: &Path) -> Vec<Store> {
    vec![
        store(
            "session folders",
            playbook_root_from(home).join("runtime"),
            scan_children,
            "gc.sessionDirIdleDays",
            home,
        ),
        store(
            "migration leftovers",
            playbook_root_from(home).join("repos"),
            scan_leftovers,
            "gc.leftoverDays",
            home,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = SECS_PER_DAY;

    fn scratch(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn backdate(path: &Path, age_secs: i64) {
        let when =
            SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(NOW - age_secs).unwrap());
        fs::File::options()
            .write(true)
            .open(path)
            .or_else(|_| fs::File::open(path))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn test_store(root: &Path, scan: super::super::ScanFn, idle_days: u32) -> Store {
        Store {
            name: "t",
            roots: vec![root.to_path_buf()],
            scan,
            action: Action::Tombstone,
            retention_key: None,
            limits: Limits {
                idle_days,
                max_bytes: 0,
                retain_days: 7,
            },
        }
    }

    fn due_paths(items: &[Item]) -> Vec<&Path> {
        items
            .iter()
            .filter(|item| item.due.is_some())
            .map(|item| item.path.as_path())
            .collect()
    }

    #[test]
    fn session_idle_boundary_is_exact_to_the_second() {
        // Arrange
        let root = scratch("stores-session-boundary");
        for (name, age) in [("just-under", 14 * DAY - 1), ("exact", 14 * DAY)] {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state"), "x").unwrap();
            backdate(&dir.join("state"), age);
            backdate(&dir, age);
        }
        let store = test_store(&root, scan_children, 14);

        // Act
        let items = store.items(NOW);

        // Assert
        assert_eq!(due_paths(&items), vec![root.join("exact").as_path()]);
    }

    #[test]
    fn session_idle_uses_newest_nested_mtime() {
        // Arrange
        let root = scratch("stores-session-nested");
        let dir = root.join("sess");
        fs::create_dir_all(dir.join("deep")).unwrap();
        fs::write(dir.join("old"), "x").unwrap();
        fs::write(dir.join("deep/fresh"), "x").unwrap();
        backdate(&dir.join("old"), 30 * DAY);
        backdate(&dir.join("deep/fresh"), DAY);
        backdate(&dir.join("deep"), 30 * DAY);
        backdate(&dir, 30 * DAY);
        let store = test_store(&root, scan_children, 14);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items[0].due.is_none());
        assert_eq!(items[0].newest_mtime, NOW - DAY);
    }

    #[test]
    fn age_walk_cap_counts_as_active() {
        // Arrange
        let root = scratch("stores-walk-cap");
        let dir = root.join("big");
        fs::create_dir_all(&dir).unwrap();
        for n in 0..=AGE_WALK_MAX_ENTRIES {
            fs::write(dir.join(n.to_string()), "").unwrap();
        }
        let store = test_store(&root, scan_children, 1);
        backdate(&dir, 30 * DAY);

        // Act
        let items = store.items(NOW);

        // Assert
        assert!(items[0].due.is_none());
        assert_eq!(items[0].newest_mtime, NOW);
    }

    #[test]
    fn leftovers_follow_the_90_day_boundary() {
        // Arrange
        let root = scratch("stores-leftovers");
        let repo = root.join("acme/app");
        fs::create_dir_all(repo.join("handoff/.copied")).unwrap();
        fs::write(repo.join("handoff/.copied/slug"), "").unwrap();
        fs::create_dir_all(repo.join("old")).unwrap();
        fs::write(repo.join("old/MOVED.txt"), "").unwrap();
        fs::write(repo.join("notes.txt"), "").unwrap();
        backdate(&repo.join("handoff/.copied/slug"), 90 * DAY - 1);
        backdate(&repo.join("handoff/.copied"), 90 * DAY - 1);
        backdate(&repo.join("old/MOVED.txt"), 90 * DAY);
        let store = test_store(&root, scan_leftovers, 90);

        // Act
        let items = store.items(NOW);

        // Assert
        assert_eq!(items.len(), 2);
        assert_eq!(
            due_paths(&items),
            vec![repo.join("old/MOVED.txt").as_path()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_never_follows_a_symlink_out_of_the_root() {
        // Arrange
        let base = scratch("stores-symlink");
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("MOVED.txt")).unwrap();
        let sessions = test_store(&root, scan_children, 1);
        let leftovers = test_store(&root, scan_leftovers, 1);

        // Act
        let session_items = sessions.items(NOW + 365 * DAY);
        let leftover_items = leftovers.items(NOW + 365 * DAY);

        // Assert
        assert!(session_items.is_empty());
        assert_eq!(
            due_paths(&leftover_items),
            vec![root.join("MOVED.txt").as_path()]
        );
        assert!(outside.join("keep").exists());
    }

    #[test]
    fn production_stores_use_the_configured_days() {
        // Arrange
        let home = scratch("stores-config");
        let config = home.join(".config/playbook");
        fs::create_dir_all(&config).unwrap();

        // Act
        let stores = small_stores(&home);

        // Assert
        assert_eq!(stores[0].limits.idle_days, 14);
        assert_eq!(stores[0].limits.retain_days, 7);
        assert_eq!(stores[1].limits.idle_days, 90);
    }
}
