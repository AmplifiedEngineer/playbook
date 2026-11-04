// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared storage for the playbook crates. Every database any playbook tool
//! creates lives in one folder, `~/.config/playbook/data`:
//!
//! * [`data_dir`] and the per-repo file helpers name that folder and the files
//!   in it.
//! * [`migrate`] moves a database from its old path into the folder without
//!   ever deleting it.

pub mod config;
pub mod memory;
pub mod memory_search;
pub mod migrate;

use playbook_common::atomic::ensure_private_dir;
use playbook_common::paths::{playbook_root_from, worktree_id};
use playbook_common::repo::repo_slug;
use playbook_common::session::home_dir;
use std::path::{Path, PathBuf};

/// `$HOME/.config/playbook/data`.
pub fn data_dir() -> PathBuf {
    data_dir_from(&home_dir())
}

/// Core of [`data_dir`], taking `home` explicitly for tests.
pub fn data_dir_from(home: &Path) -> PathBuf {
    playbook_root_from(home).join("data")
}

/// [`data_dir`], created owner only when missing.
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// `<data>/<kind>/<owner>/<repo>/<worktree-id>.db`: the file of one database
/// kind (`gate`, `code`) for one worktree. Per-worktree files keep two
/// worktrees of the same repo apart, as the old `repos/...` folders did.
pub fn scoped_db_in(data: &Path, kind: &str, owner: &str, repo: &str, worktree: &str) -> PathBuf {
    data.join(kind)
        .join(owner)
        .join(repo)
        .join(format!("{worktree}.db"))
}

/// The per-worktree database kinds.
pub const WORKTREE_KINDS: [&str; 2] = ["gate", "code"];

/// Removes the databases of one removed worktree (each kind, with its `-wal`
/// and `-shm`). Returns how many files were removed. Missing files are fine.
pub fn remove_worktree_dbs(data: &Path, slug: &str, worktree: &str) -> usize {
    let Some((owner, repo)) = slug.split_once('/') else {
        return 0;
    };
    if !slug.split('/').all(safe_segment) || !safe_segment(worktree) {
        return 0;
    }
    let mut removed = 0;
    for kind in WORKTREE_KINDS {
        let main = scoped_db_in(data, kind, owner, repo, worktree);
        for side in ["", "-wal", "-shm"] {
            let mut name = main.as_os_str().to_owned();
            name.push(side);
            if std::fs::remove_file(PathBuf::from(name)).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// The per-worktree databases of `slug` whose worktree id is not in `live`,
/// as `(kind, path)`. They belong to worktrees that no longer exist.
pub fn orphan_dbs(data: &Path, slug: &str, live: &[String]) -> Vec<(&'static str, PathBuf)> {
    let Some((owner, repo)) = slug.split_once('/') else {
        return Vec::new();
    };
    if !slug.split('/').all(safe_segment) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for kind in WORKTREE_KINDS {
        let dir = data.join(kind).join(owner).join(repo);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let is_db = path.extension().is_some_and(|x| x == "db");
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if is_db && path.is_file() && !live.contains(&stem) {
                out.push((kind, path));
            }
        }
    }
    out.sort();
    out
}

/// A path part that cannot climb out of its folder or merge two kinds.
fn safe_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\'])
}

/// [`scoped_db_in`] for the current repo and worktree under [`data_dir`].
/// `None` when the repo has no `origin` remote or no resolvable worktree.
pub fn scoped_db(kind: &str) -> Option<PathBuf> {
    let slug = repo_slug();
    let (owner, repo) = slug.split_once('/')?;
    let id = worktree_id();
    if id.is_empty() || !slug.split('/').all(safe_segment) {
        return None;
    }
    Some(scoped_db_in(&data_dir(), kind, owner, repo, &id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_sits_under_the_playbook_root() {
        assert_eq!(
            data_dir_from(Path::new("/h")),
            PathBuf::from("/h/.config/playbook/data")
        );
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("dirs");
        std::fs::write(p, b"x").expect("write");
    }

    #[test]
    fn removing_a_worktree_takes_its_databases_and_sidecars_only() {
        let d = std::env::temp_dir().join(format!("pbdata-rm-{}", std::process::id()));
        let gone = scoped_db_in(&d, "gate", "o", "r", "wt-gone");
        let kept = scoped_db_in(&d, "gate", "o", "r", "wt-kept");
        let code = scoped_db_in(&d, "code", "o", "r", "wt-gone");
        for p in [&gone, &kept, &code] {
            touch(p);
        }
        touch(&PathBuf::from(format!("{}-wal", gone.display())));
        assert_eq!(remove_worktree_dbs(&d, "o/r", "wt-gone"), 3);
        assert!(!gone.exists() && !code.exists());
        assert!(kept.exists());
        assert_eq!(remove_worktree_dbs(&d, "o/r", "../x"), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn orphans_are_the_databases_of_worktrees_that_are_gone() {
        let d = std::env::temp_dir().join(format!("pbdata-orphan-{}", std::process::id()));
        touch(&scoped_db_in(&d, "gate", "o", "r", "live"));
        touch(&scoped_db_in(&d, "gate", "o", "r", "dead"));
        touch(&scoped_db_in(&d, "code", "o", "r", "dead"));
        touch(&scoped_db_in(&d, "code", "o", "other", "dead"));
        let got = orphan_dbs(&d, "o/r", &["live".to_string()]);
        let names: Vec<_> = got
            .iter()
            .map(|(k, p)| format!("{k}:{}", p.file_name().expect("name").to_string_lossy()))
            .collect();
        assert_eq!(names, vec!["code:dead.db", "gate:dead.db"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dot_segments_are_not_safe_path_parts() {
        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert!(!safe_segment(bad), "{bad:?}");
        }
        assert!(safe_segment("pragmatic-engineer"));
        // A subgroup slug is several safe segments.
        assert!("group/sub/repo".split('/').all(safe_segment));
        assert!(!"group/../repo".split('/').all(safe_segment));
    }

    #[test]
    fn scoped_files_keep_repos_and_worktrees_apart() {
        let d = Path::new("/d");
        assert_eq!(
            scoped_db_in(d, "gate", "o", "r", "wt-1"),
            PathBuf::from("/d/gate/o/r/wt-1.db")
        );
        assert_ne!(
            scoped_db_in(d, "gate", "o", "r", "a"),
            scoped_db_in(d, "gate", "o", "r", "b")
        );
        assert_ne!(
            scoped_db_in(d, "gate", "o", "r", "a"),
            scoped_db_in(d, "code", "o", "r", "a")
        );
    }
}
