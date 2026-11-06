// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! File discovery through git. `git ls-files` already honors `.gitignore`,
//! nested ignores and global excludes, so no walker crate is needed. Safe
//! defaults then drop vendored and generated trees, links, submodules, large
//! files and binary files, and every skip is reported with its reason.

use playbook_common::git;
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Directory names that are never indexed, wherever they appear in a path.
const EXCLUDED_DIRS: [&str; 8] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
];

/// Injectable caps, so tests never build huge inputs.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// How many leading bytes are probed for a NUL byte.
    pub binary_probe_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1024 * 1024,
            binary_probe_bytes: 8 * 1024,
        }
    }
}

/// Why a listed file is not indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    DefaultExclude,
    Symlink,
    Gitlink,
    TooLarge,
    Binary,
    NonUtf8Path,
    TooDeep,
    ParseTimeout,
}

impl SkipReason {
    /// The stable name stored in the index and printed in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::DefaultExclude => "default_exclude",
            SkipReason::Symlink => "symlink",
            SkipReason::Gitlink => "gitlink",
            SkipReason::TooLarge => "too_large",
            SkipReason::Binary => "binary",
            SkipReason::NonUtf8Path => "non_utf8_path",
            SkipReason::TooDeep => "too_deep",
            SkipReason::ParseTimeout => "parse_timeout",
        }
    }
}

pub const GUARD_SEPARATOR: char = '@';

/// The reason part of a stored `error` value, without any guard fingerprint.
pub fn reason_of(stored: &str) -> &str {
    stored
        .split_once(GUARD_SEPARATOR)
        .map_or(stored, |(r, _)| r)
}

/// Whether a stored `error` value names a parse guard reason.
pub fn is_guard_reason(stored: &str) -> bool {
    [SkipReason::TooDeep, SkipReason::ParseTimeout]
        .iter()
        .any(|r| r.as_str() == reason_of(stored))
}

/// A listed file that is not indexed, with the stat facts the freshness check
/// compares later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub reason: SkipReason,
    pub size: u64,
    pub mtime_ns: i64,
}

/// The result of one discovery pass, in sorted path order.
#[derive(Debug, Default)]
pub struct Discovered {
    /// Files to index.
    pub files: Vec<String>,
    /// Files listed by git but not indexed.
    pub skipped: Vec<Skipped>,
}

/// Modified time as nanoseconds since the epoch, 0 when unavailable.
pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Whether any directory component of `path` is a default exclude, or the
/// file is a minified script.
pub fn is_default_excluded(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or("");
    file.ends_with(".min.js") || parts.iter().any(|dir| EXCLUDED_DIRS.contains(dir))
}

/// The paths `git ls-files -co --exclude-standard -z` prints, sorted and
/// deduplicated. A git failure is an error, never an empty list.
pub fn list_paths(root: &Path) -> Result<(Vec<String>, usize), String> {
    let out = git::run(
        Some(root),
        &["ls-files", "-co", "--exclude-standard", "-z"],
        git::SLOW_TIMEOUT,
    )
    .ok_or_else(|| "git ls-files did not finish".to_string())?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git ls-files failed: {detail}"));
    }
    let mut paths = Vec::new();
    let mut non_utf8 = 0;
    for raw in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        match std::str::from_utf8(raw) {
            Ok(path) => paths.push(path.to_string()),
            Err(_) => non_utf8 += 1,
        }
    }
    paths.sort();
    paths.dedup();
    Ok((paths, non_utf8))
}

/// A path git lists that the disk no longer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gone;

/// Classifies one listed path against the disk. `Ok(None)` means index it,
/// `Ok(Some(..))` means skip it, and `Err(Gone)` means git lists it but the disk
/// does not have it (deleted before commit), which counts as removed.
pub fn classify(root: &Path, path: &str, limits: &Limits) -> Result<Option<Skipped>, Gone> {
    let full = root.join(path);
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Gone),
        // Any other stat failure is left to the indexer, which reports the
        // file as failed and keeps its old rows.
        Err(_) => return Ok(None),
    };
    let skip = |reason| {
        Ok(Some(Skipped {
            path: path.to_string(),
            reason,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        }))
    };
    if is_default_excluded(path) {
        return skip(SkipReason::DefaultExclude);
    }
    if meta.file_type().is_symlink() {
        return skip(SkipReason::Symlink);
    }
    if meta.is_dir() {
        return skip(SkipReason::Gitlink);
    }
    if meta.len() > limits.max_file_bytes {
        return skip(SkipReason::TooLarge);
    }
    let mut probe = vec![0u8; limits.binary_probe_bytes];
    let read = std::fs::File::open(&full).and_then(|mut f| read_up_to(&mut f, &mut probe));
    match read {
        Ok(n) if probe[..n].contains(&0) => skip(SkipReason::Binary),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Lists the repo's files and applies the safe defaults.
pub fn discover(root: &Path, limits: &Limits) -> Result<Discovered, String> {
    let (paths, non_utf8) = list_paths(root)?;
    let mut found = Discovered::default();
    for _ in 0..non_utf8 {
        found.skipped.push(Skipped {
            path: String::from("(non UTF-8 path)"),
            reason: SkipReason::NonUtf8Path,
            size: 0,
            mtime_ns: 0,
        });
    }
    for path in paths {
        match classify(root, &path, limits) {
            Ok(None) => found.files.push(path),
            Ok(Some(skipped)) => found.skipped.push(skipped),
            Err(Gone) => {}
        }
    }
    Ok(found)
}

#[cfg(test)]
pub(crate) mod test_repo {
    //! A scratch git repo for unit tests that need a real listing.
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(crate) fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git should run");
        assert!(status.success(), "git {args:?} failed");
    }

    /// An initialised repo with an origin, under a unique scratch dir.
    pub(crate) fn init(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).expect("repo dir");
        let dir = dir.canonicalize().expect("canonical");
        git_in(&dir, &["init", "--quiet"]);
        git_in(
            &dir,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/test-owner/test-repo.git",
            ],
        );
        dir
    }

    pub(crate) fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, bytes).expect("write");
    }
}

#[cfg(test)]
mod tests {
    use super::test_repo::{git_in, init, write};
    use super::*;
    use crate::testkit::pinned;

    fn names(found: &Discovered) -> Vec<&str> {
        found.files.iter().map(String::as_str).collect()
    }

    fn reason_of(found: &Discovered, path: &str) -> Option<SkipReason> {
        found
            .skipped
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.reason)
    }

    #[cfg(unix)]
    #[test]
    fn tracked_and_untracked_sources_are_listed_and_skips_carry_reasons() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-mixed");
        write(&root, ".gitignore", b"ignored.txt\n");
        write(&root, "ignored.txt", b"x");
        write(&root, "a.rs", b"fn a() {}\n");
        write(&root, "node_modules/pkg/index.js", b"x");
        write(&root, "bin.dat", b"ab\0cd");
        std::os::unix::fs::symlink(root.join("a.rs"), root.join("link.rs")).expect("link");
        git_in(&root, &["add", "a.rs"]);
        write(&root, "b.ts", b"export const b = 1;\n");

        // Act
        let found = discover(&root, &Limits::default()).expect("discover");

        // Assert
        assert_eq!(names(&found), vec![".gitignore", "a.rs", "b.ts"]);
        assert_eq!(
            reason_of(&found, "node_modules/pkg/index.js"),
            Some(SkipReason::DefaultExclude)
        );
        assert_eq!(reason_of(&found, "bin.dat"), Some(SkipReason::Binary));
        assert_eq!(reason_of(&found, "link.rs"), Some(SkipReason::Symlink));
        assert_eq!(reason_of(&found, "ignored.txt"), None);
    }

    #[test]
    fn a_file_of_exactly_the_cap_is_listed_and_one_byte_more_is_skipped() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-size");
        let limits = Limits {
            max_file_bytes: 1024,
            ..Limits::default()
        };
        write(&root, "n.txt", &vec![b'a'; 1023]);
        write(&root, "at.txt", &vec![b'a'; 1024]);
        write(&root, "over.txt", &vec![b'a'; 1025]);

        // Act
        let found = discover(&root, &limits).expect("discover");

        // Assert
        assert_eq!(names(&found), vec!["at.txt", "n.txt"]);
        assert_eq!(reason_of(&found, "over.txt"), Some(SkipReason::TooLarge));
    }

    #[test]
    fn a_nul_at_the_last_probed_byte_is_binary_and_one_past_it_is_not() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-nul");
        let limits = Limits {
            binary_probe_bytes: 8192,
            ..Limits::default()
        };
        let mut inside = vec![b'a'; 9000];
        inside[8191] = 0;
        let mut outside = vec![b'a'; 9000];
        outside[8192] = 0;
        write(&root, "inside.txt", &inside);
        write(&root, "outside.txt", &outside);

        // Act
        let found = discover(&root, &limits).expect("discover");

        // Assert
        assert_eq!(names(&found), vec!["outside.txt"]);
        assert_eq!(reason_of(&found, "inside.txt"), Some(SkipReason::Binary));
    }

    #[test]
    fn awkward_file_names_round_trip_through_nul_separated_output() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-names");
        let awkward = ["with space.rs", "caf\u{e9}.py", "line\nbreak.ts"];
        for name in awkward {
            write(&root, name, b"x = 1\n");
        }

        // Act
        let found = discover(&root, &Limits::default()).expect("discover");

        // Assert
        let mut want: Vec<&str> = awkward.to_vec();
        want.sort_unstable();
        assert_eq!(names(&found), want);
    }

    #[test]
    fn every_default_exclude_is_skipped_with_its_reason() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-excludes");
        let excluded = [
            "node_modules/a.js",
            "target/a.rs",
            "dist/a.js",
            "build/a.js",
            "vendor/a.go",
            ".venv/a.py",
            "venv/a.py",
            "__pycache__/a.py",
            "x.min.js",
        ];
        for path in excluded {
            write(&root, path, b"x\n");
        }
        write(&root, "keep.rs", b"x\n");

        // Act
        let found = discover(&root, &Limits::default()).expect("discover");

        // Assert
        assert_eq!(names(&found), vec!["keep.rs"]);
        for path in excluded {
            assert_eq!(
                reason_of(&found, path),
                Some(SkipReason::DefaultExclude),
                "{path}"
            );
        }
    }

    #[test]
    fn a_submodule_gitlink_is_skipped() {
        let _guard = pinned();
        // Arrange: a nested repo registered as a gitlink in the index.
        let root = init("walker-gitlink");
        let inner = root.join("inner");
        std::fs::create_dir_all(&inner).expect("inner");
        git_in(&inner, &["init", "--quiet"]);
        write(&inner, "f.txt", b"x");
        git_in(&inner, &["add", "f.txt"]);
        git_in(
            &inner,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "i",
            ],
        );
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(&inner)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
        git_in(
            &root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},inner"),
            ],
        );

        // Act
        let found = discover(&root, &Limits::default()).expect("discover");

        // Assert
        assert_eq!(reason_of(&found, "inner"), Some(SkipReason::Gitlink));
        assert!(!found.files.iter().any(|f| f.starts_with("inner")));
    }

    #[test]
    fn an_empty_repo_lists_nothing_and_a_non_repo_is_an_error() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-empty");
        let plain = playbook_common::test_support::scratch_dir("walker-plain");
        std::fs::create_dir_all(&plain).expect("plain dir");

        // Act
        let empty = discover(&root, &Limits::default()).expect("empty repo");
        let outside = discover(&plain, &Limits::default());

        // Assert
        assert!(empty.files.is_empty() && empty.skipped.is_empty());
        assert!(outside.is_err());
    }

    #[test]
    fn a_tracked_file_deleted_from_disk_is_not_listed() {
        let _guard = pinned();
        // Arrange
        let root = init("walker-deleted");
        write(&root, "gone.rs", b"x\n");
        write(&root, "stay.rs", b"x\n");
        git_in(&root, &["add", "."]);
        std::fs::remove_file(root.join("gone.rs")).expect("remove");

        // Act
        let found = discover(&root, &Limits::default()).expect("discover");

        // Assert
        assert_eq!(names(&found), vec!["stay.rs"]);
    }
}
