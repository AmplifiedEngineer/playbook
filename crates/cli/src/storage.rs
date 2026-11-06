// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Where a repo's plans, implementation notes, worktrees and handoffs live:
//! `~/.config/playbook/repos/<owner>/<repo>/<kind>/`, the same from every
//! checkout and every worktree. `<owner>/<repo>` comes from the `origin`
//! remote, so a person or an agent with no binary can work the path out with
//! `git remote get-url origin` alone.
//!
//! Older versions scoped these folders to one worktree
//! (`repos/<owner>/<repo>/<worktree-id>/<kind>`). The first call that needs a
//! folder copies what the old folders hold into the new one. Nothing is
//! deleted or overwritten: an old file stays where it was, and a `MOVED.txt`
//! note beside it says where the copy went and doubles as the "done" marker.

use crate::common::paths::{repo_dir_for_slug, repo_scoped_dir, RepoScope};
use crate::common::repo::repo_slug_in;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The note left in an old folder once its files were copied.
const POINTER: &str = "MOVED.txt";

/// A storage folder under a repo's directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plans,
    Implement,
    Worktrees,
    Handoff,
}

impl Kind {
    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Plans => "plans",
            Kind::Implement => "implement",
            Kind::Worktrees => "worktrees",
            Kind::Handoff => "handoff",
        }
    }

    /// Whether files in an old worktree-scoped folder are copied across. Not
    /// `worktrees`: it holds live git checkouts that point back at their
    /// repo, and a copy would be a second, broken checkout. Not `handoff`
    /// either: handoffs came from the runtime folder, see [`migrate_handoffs`].
    fn copies_from_worktree_dirs(self) -> bool {
        matches!(self, Kind::Plans | Kind::Implement)
    }
}

/// `repos/<owner>/<repo>/`, or `None` without a usable `origin` remote.
pub fn repo_dir() -> Option<PathBuf> {
    repo_scoped_dir(RepoScope::Repo)
}

/// `repos/<owner>/<repo>/` for the repo that holds `dir`, or for the current
/// repo when `dir` is `None`.
pub fn repo_dir_in(dir: Option<&Path>) -> Option<PathBuf> {
    match dir {
        None => repo_dir(),
        Some(dir) => repo_dir_for_slug(&repo_slug_in(dir)),
    }
}

/// The folder for `kind`, after copying anything older versions left in a
/// worktree-scoped folder. `None` without a usable `origin` remote.
pub fn kind_dir(kind: Kind) -> Option<PathBuf> {
    let base = repo_dir()?;
    if kind.copies_from_worktree_dirs() {
        if let Err(err) = migrate_worktree_dirs(&base, kind) {
            eprintln!(
                "playbook: could not copy the old {} folders: {err}",
                kind.dir_name()
            );
        }
    }
    Some(base.join(kind.dir_name()))
}

/// Names under a repo folder that are not old worktree folders.
const RESERVED: &[&str] = &["plans", "implement", "worktrees", "handoff", ".config"];

/// Old worktree folders sit next to the new ones, named by an id built from
/// the worktree's absolute path. The id starts with a dash on Unix and with a
/// drive letter on Windows, so a folder counts by shape: it is not one of the
/// new folders and it holds a `<kind>/` folder.
fn worktree_dirs(base: &Path, kind: Kind) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(base) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !RESERVED.contains(&e.file_name().to_string_lossy().as_ref()))
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(kind.dir_name()).is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Copy `repos/<owner>/<repo>/<worktree-id>/<kind>/` from every old worktree
/// folder into `repos/<owner>/<repo>/<kind>/`, once per old folder.
pub fn migrate_worktree_dirs(base: &Path, kind: Kind) -> io::Result<()> {
    if !kind.copies_from_worktree_dirs() {
        return Ok(());
    }
    let new = base.join(kind.dir_name());
    for old in worktree_dirs(base, kind)
        .into_iter()
        .map(|d| d.join(kind.dir_name()))
        .filter(|d| d.is_dir() && !d.join(POINTER).exists())
    {
        fs::create_dir_all(&new)?;
        let stats = copy_missing(&old, &new)?;
        let mut summary = format!(
            "{} files copied, {} kept apart because the new folder already had a different file with the same name",
            stats.copied,
            stats.clashed.len()
        );
        for name in &stats.clashed {
            summary.push_str(&format!("\n  kept apart: {}", name.display()));
        }
        if stats.skipped > 0 {
            summary.push_str(&format!(
                "\n  {} link(s) or special file(s) were not copied",
                stats.skipped
            ));
        }
        write_pointer(&old, &new, &summary)?;
        if stats.copied > 0 || !stats.clashed.is_empty() {
            eprintln!(
                "playbook: copied {} file(s) from {} to {}; the originals stay where they are",
                stats.copied,
                old.display(),
                new.display()
            );
            for name in stats.clashed.iter().take(CLASH_NAMES_SHOWN) {
                eprintln!(
                    "playbook: kept apart, the new folder has a different {}",
                    name.display()
                );
            }
            if stats.clashed.len() > CLASH_NAMES_SHOWN {
                eprintln!(
                    "playbook: and {} more; see {}",
                    stats.clashed.len() - CLASH_NAMES_SHOWN,
                    old.join(POINTER).display()
                );
            }
        }
    }
    Ok(())
}

/// Most clashing file names printed on stderr; the pointer note lists them all.
const CLASH_NAMES_SHOWN: usize = 5;

#[derive(Default)]
struct Copied {
    copied: usize,
    /// Files whose name the new folder already had with different bytes.
    clashed: Vec<PathBuf>,
    /// Symbolic links and special files, which are not copied.
    skipped: usize,
}

/// Copy regular files from `from` into `to`, recursively. A name that already
/// exists in `to` is left alone, so a copy never overwrites newer work.
fn copy_missing(from: &Path, to: &Path) -> io::Result<Copied> {
    let mut stats = Copied::default();
    for entry in fs::read_dir(from)?.flatten() {
        let name = entry.file_name();
        if name == POINTER {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            let inner = copy_missing(&entry.path(), &target)?;
            stats.copied += inner.copied;
            stats.clashed.extend(inner.clashed);
            stats.skipped += inner.skipped;
        } else if kind.is_file() {
            if copy_new(&entry.path(), &target)? {
                stats.copied += 1;
            } else if !same_bytes(&entry.path(), &target) {
                stats.clashed.push(target);
            }
        } else {
            stats.skipped += 1;
        }
    }
    Ok(stats)
}

/// Copy `from` to `to` unless `to` exists, keeping the file's modified time
/// (a handoff's age decides whether it is still fresh). The bytes land in a
/// temporary file first and a hard link puts them in place, so a reader never
/// sees half a file and two processes cannot both win.
fn copy_new(from: &Path, to: &Path) -> io::Result<bool> {
    if to.exists() {
        return Ok(false);
    }
    let tmp = to.with_extension(format!("copying-{}", std::process::id()));
    let staged = stage(from, &tmp);
    let linked = staged.and_then(|()| fs::hard_link(&tmp, to));
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err),
    }
}

fn stage(from: &Path, tmp: &Path) -> io::Result<()> {
    let modified = fs::metadata(from)?.modified()?;
    fs::copy(from, tmp)?;
    fs::OpenOptions::new()
        .write(true)
        .open(tmp)?
        .set_modified(modified)
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    matches!((fs::read(a), fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

fn write_pointer(old: &Path, new: &Path, summary: &str) -> io::Result<()> {
    let mut file = fs::File::create(old.join(POINTER))?;
    writeln!(
        file,
        "playbook keeps this folder at {} now, the same from every worktree of the repo.\n\
         Files here were copied there, not moved or deleted: {summary}.\n\
         This note marks the copy as done. It is safe to delete the folder when you no longer need it.",
        new.display()
    )
}

/// The old, global handoff folder: one folder for every directory, keyed by
/// the directory's slug in each file name.
pub fn legacy_handoff_root() -> PathBuf {
    crate::common::paths::runtime_root().join("handoff")
}

/// The handoff folder for the repo that holds `dir` (the current repo when
/// `dir` is `None`), or the old global one when that directory has no usable
/// `origin` remote. Copies the handoffs of exactly `slug` across the first
/// time.
pub fn handoff_root(slug: &str, dir: Option<&Path>) -> PathBuf {
    let legacy = legacy_handoff_root();
    let Some(base) = repo_dir_in(dir) else {
        return legacy;
    };
    let new = base.join(Kind::Handoff.dir_name());
    if let Err(err) = migrate_handoffs(&legacy, &new, slug) {
        eprintln!("playbook: could not copy the old handoffs: {err}");
    }
    new
}

/// Copy the handoffs for the directory `slug` from the old global folder into
/// `new`, once per `slug`. Only that directory's own files move: a handoff
/// belongs to the directory its session ran in, so a child or sibling
/// directory's handoffs wait for its own first call. The old files stay; a
/// marker in `new` stops a handoff that a session already consumed from
/// coming back.
pub fn migrate_handoffs(legacy: &Path, new: &Path, slug: &str) -> io::Result<()> {
    if slug.is_empty() || !legacy.is_dir() {
        return Ok(());
    }
    // Named by the legacy slug, as before, so a handoff that was consumed is
    // not copied again after an upgrade changed the key.
    let marker = new.join(".copied").join(crate::handoff::legacy_slug(slug));
    if marker.exists() {
        return Ok(());
    }
    fs::create_dir_all(new)?;
    let mut copied = 0;
    for entry in fs::read_dir(legacy)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if crate::handoff::is_handoff_for(&name, slug)
            && entry.file_type()?.is_file()
            && copy_new(&entry.path(), &new.join(&name))?
        {
            copied += 1;
        }
    }
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&marker, "")?;
    if copied > 0 {
        let pointer = legacy.join(POINTER);
        if !pointer.exists() {
            write_pointer(
                legacy,
                new,
                "each directory's handoffs are copied there on first use",
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn plans_and_implement_files_are_copied_from_every_old_worktree_folder() {
        let base = scratch_dir("storage-copy");
        write(&base.join("-a-wt1/plans/one.md"), "one");
        write(&base.join("-a-wt2/plans/two.md"), "two");
        write(&base.join("-a-wt2/plans/sub/three.md"), "three");

        migrate_worktree_dirs(&base, Kind::Plans).unwrap();

        assert_eq!(
            fs::read_to_string(base.join("plans/one.md")).unwrap(),
            "one"
        );
        assert_eq!(
            fs::read_to_string(base.join("plans/two.md")).unwrap(),
            "two"
        );
        assert_eq!(
            fs::read_to_string(base.join("plans/sub/three.md")).unwrap(),
            "three"
        );
    }

    #[test]
    fn the_old_files_stay_and_a_pointer_marks_the_copy_done() {
        let base = scratch_dir("storage-pointer");
        write(&base.join("-a-wt1/plans/one.md"), "one");

        migrate_worktree_dirs(&base, Kind::Plans).unwrap();

        assert!(base.join("-a-wt1/plans/one.md").is_file());
        let note = fs::read_to_string(base.join("-a-wt1/plans/MOVED.txt")).unwrap();
        assert!(
            note.contains(&base.join("plans").display().to_string()),
            "{note}"
        );
        assert!(!base.join("plans/MOVED.txt").exists());
    }

    #[test]
    fn a_second_run_does_not_bring_back_a_file_that_was_removed_from_the_new_folder() {
        let base = scratch_dir("storage-once");
        write(&base.join("-a-wt1/plans/one.md"), "one");
        migrate_worktree_dirs(&base, Kind::Plans).unwrap();
        fs::remove_file(base.join("plans/one.md")).unwrap();

        migrate_worktree_dirs(&base, Kind::Plans).unwrap();

        assert!(!base.join("plans/one.md").exists());
    }

    #[test]
    fn an_existing_file_is_never_overwritten_and_a_clash_is_counted() {
        let base = scratch_dir("storage-clash");
        write(&base.join("plans/one.md"), "new work");
        write(&base.join("-a-wt1/plans/one.md"), "old work");

        migrate_worktree_dirs(&base, Kind::Plans).unwrap();

        assert_eq!(
            fs::read_to_string(base.join("plans/one.md")).unwrap(),
            "new work"
        );
        assert_eq!(
            fs::read_to_string(base.join("-a-wt1/plans/one.md")).unwrap(),
            "old work"
        );
        let note = fs::read_to_string(base.join("-a-wt1/plans/MOVED.txt")).unwrap();
        assert!(note.contains("1 kept apart"), "{note}");
        assert!(note.contains("plans/one.md"), "{note}");
    }

    #[test]
    fn worktree_checkouts_are_never_copied() {
        let base = scratch_dir("storage-worktrees");
        write(&base.join("-a-wt1/worktrees/branch/file.rs"), "code");

        migrate_worktree_dirs(&base, Kind::Worktrees).unwrap();

        assert!(!base.join("worktrees/branch").exists());
        assert!(!base.join("-a-wt1/worktrees/MOVED.txt").exists());
    }

    #[test]
    fn old_folders_are_found_by_shape_not_by_a_leading_dash() {
        let base = scratch_dir("storage-shape");
        write(&base.join("C--Users-me-repo/plans/win.md"), "win");
        write(&base.join("handoff/plans/not-a-worktree.md"), "x");

        migrate_worktree_dirs(&base, Kind::Plans).unwrap();

        assert!(base.join("plans/win.md").is_file());
        assert!(!base.join("plans/not-a-worktree.md").exists());
    }

    #[test]
    fn handoffs_for_this_directory_are_copied_once() {
        let root = scratch_dir("storage-handoff");
        let legacy = root.join("legacy");
        let new = root.join("new");
        write(&legacy.join("-w-repo-1-2.md"), "here");
        write(&legacy.join("-w-repo-sub-1-2.md"), "child");
        write(&legacy.join("-w-other-1-2.md"), "elsewhere");

        migrate_handoffs(&legacy, &new, "-w-repo").unwrap();
        fs::remove_file(new.join("-w-repo-1-2.md")).unwrap();
        migrate_handoffs(&legacy, &new, "-w-repo").unwrap();

        assert!(
            !new.join("-w-repo-1-2.md").exists(),
            "a consumed handoff came back"
        );
        assert!(!new.join("-w-repo-sub-1-2.md").exists());
        assert!(!new.join("-w-other-1-2.md").exists());
        assert!(legacy.join("-w-repo-1-2.md").is_file());
        assert!(legacy.join(POINTER).is_file());
    }

    #[test]
    fn a_child_that_consumed_its_handoff_does_not_get_it_back_from_the_parent() {
        let root = scratch_dir("storage-handoff-child");
        let legacy = root.join("legacy");
        let new = root.join("new");
        write(&legacy.join("-w-repo-1-2.md"), "parent");
        write(&legacy.join("-w-repo-sub-1-2.md"), "child");

        migrate_handoffs(&legacy, &new, "-w-repo-sub").unwrap();
        fs::remove_file(new.join("-w-repo-sub-1-2.md")).unwrap();
        migrate_handoffs(&legacy, &new, "-w-repo").unwrap();
        migrate_handoffs(&legacy, &new, "-w-repo-sub").unwrap();

        assert!(new.join("-w-repo-1-2.md").is_file());
        assert!(
            !new.join("-w-repo-sub-1-2.md").exists(),
            "a consumed child handoff came back"
        );
    }

    #[test]
    fn a_copy_keeps_the_modified_time_of_the_original() {
        let root = scratch_dir("storage-mtime");
        let legacy = root.join("legacy");
        let new = root.join("new");
        write(&legacy.join("-w-repo-1-2.md"), "old");
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 86_400);
        fs::File::options()
            .write(true)
            .open(legacy.join("-w-repo-1-2.md"))
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        migrate_handoffs(&legacy, &new, "-w-repo").unwrap();

        let got = fs::metadata(new.join("-w-repo-1-2.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(got, old_time);
    }

    #[test]
    fn a_failed_copy_leaves_no_temporary_file() {
        let root = scratch_dir("storage-tmpclean");
        let missing = root.join("missing.md");
        let to = root.join("out.md");

        assert!(copy_new(&missing, &to).is_err());

        let leftovers = fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0);
    }
}
