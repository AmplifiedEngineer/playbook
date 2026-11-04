// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Build output in worktrees: the ecosystem table and the detection of folders
//! that are safe to call build output. Detection only reads; nothing here
//! removes anything.

use std::fs;
use std::path::{Path, PathBuf};

use super::stores::mtime_secs;
use super::{Action, Item, Limits, Store};
use crate::common::git;

/// How deep below a worktree root the scan looks for a build folder.
pub const SCAN_MAX_DEPTH: usize = 4;

/// One language ecosystem: files that show a project uses it, and the folders
/// it generates. A marker starting with `*` matches a file name suffix.
pub struct Ecosystem {
    pub name: &'static str,
    pub markers: &'static [&'static str],
    pub folders: &'static [&'static str],
}

/// The v1 table. `dist/`, `build/` and `bin/` are left out because they often
/// hold release artifacts or local tools that cannot be regenerated. Adding a
/// row and a fixture test is all a new ecosystem takes.
pub const TABLE: &[Ecosystem] = &[
    Ecosystem {
        name: "Rust",
        markers: &["Cargo.toml"],
        folders: &["target"],
    },
    Ecosystem {
        name: "Node",
        markers: &["package.json"],
        folders: &["node_modules", ".next", ".turbo"],
    },
    Ecosystem {
        name: "Python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        folders: &[
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            ".ruff_cache",
            ".tox",
        ],
    },
    Ecosystem {
        name: "Java and Kotlin",
        markers: &["pom.xml", "build.gradle", "build.gradle.kts"],
        folders: &["target", ".gradle"],
    },
    Ecosystem {
        name: ".NET",
        markers: &["*.csproj", "*.sln"],
        folders: &["obj"],
    },
    Ecosystem {
        name: "Swift and Zig",
        markers: &["Package.swift", "build.zig"],
        folders: &[".build", ".zig-cache"],
    },
    Ecosystem {
        name: "Go",
        markers: &["go.mod"],
        folders: &[],
    },
];

/// Virtual environment folders. They hold installed packages and interpreters
/// that are costly to rebuild, so the scan never lists one and never looks
/// inside one, even when it sits in an ignored path next to a Python marker.
const VENV_NAMES: &[&str] = &[".venv", "venv"];

/// A build folder that passed every detection check.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    pub folder: PathBuf,
    pub ecosystem: &'static str,
}

/// Whether `name` is `entry` byte for byte, so a `Target` folder on a case
/// insensitive volume is never mistaken for `target`.
pub fn matcher_is_exact(entry: &str, name: &str) -> bool {
    entry == name
}

fn marker_present(dir: &Path, marker: &str) -> bool {
    match marker.strip_prefix('*') {
        Some(suffix) => fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry.file_name().to_string_lossy().ends_with(suffix)
                    && entry.file_type().is_ok_and(|kind| kind.is_file())
            }),
        None => fs::symlink_metadata(dir.join(marker)).is_ok_and(|meta| meta.is_file()),
    }
}

/// The ecosystem whose marker sits in `folder`'s parent or an ancestor inside
/// `worktree` and which lists the folder's name.
fn ecosystem_of(worktree: &Path, folder: &Path) -> Option<&'static Ecosystem> {
    let name = folder.file_name()?.to_str()?;
    TABLE
        .iter()
        .filter(|eco| {
            eco.folders
                .iter()
                .any(|entry| matcher_is_exact(entry, name))
        })
        .find(|eco| {
            folder
                .ancestors()
                .skip(1)
                .take_while(|dir| dir.starts_with(worktree))
                .any(|dir| eco.markers.iter().any(|marker| marker_present(dir, marker)))
        })
}

fn is_ignored(worktree: &Path, folder: &Path) -> bool {
    let rel = folder.strip_prefix(worktree).unwrap_or(folder);
    let Some(rel) = rel.to_str() else {
        return false;
    };
    git::run(
        Some(worktree),
        &["check-ignore", "-q", "--", rel],
        git::SLOW_TIMEOUT,
    )
    .is_some_and(|out| out.status.success())
}

/// Whether git tracks anything at or below `folder`. A submodule entry counts.
/// Unknown counts as tracked, the safe answer.
fn has_tracked_files(worktree: &Path, folder: &Path) -> bool {
    let rel = folder.strip_prefix(worktree).unwrap_or(folder);
    let Some(rel) = rel.to_str() else {
        return true;
    };
    match git::run(Some(worktree), &["ls-files", "--", rel], git::SLOW_TIMEOUT) {
        Some(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

/// Every build folder under `worktree`, down to [`SCAN_MAX_DEPTH`] levels. It
/// never enters `.git`, a virtual environment or a folder it matched, never
/// follows a symlink, and skips a folder that holds its own `.git`.
pub fn detect(worktree: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    scan_dir(worktree, worktree, 1, &mut found);
    found.sort_by(|a, b| a.folder.cmp(&b.folder));
    found
}

/// A folder named like a virtual environment, or one that holds the
/// `pyvenv.cfg` file every Python virtual environment carries.
fn is_virtualenv(dir: &Path) -> bool {
    let named = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| VENV_NAMES.contains(&name));
    named || fs::symlink_metadata(dir.join("pyvenv.cfg")).is_ok()
}

fn scan_dir(worktree: &Path, dir: &Path, depth: usize, found: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if is_virtualenv(&path) {
            continue;
        }
        if let Some(eco) = ecosystem_of(worktree, &path) {
            let holds_repo = fs::symlink_metadata(path.join(".git")).is_ok();
            if !holds_repo && is_ignored(worktree, &path) && !has_tracked_files(worktree, &path) {
                found.push(Found {
                    folder: path,
                    ecosystem: eco.name,
                });
            }
        } else if depth < SCAN_MAX_DEPTH {
            scan_dir(worktree, &path, depth + 1, found);
        }
    }
}

/// Total size of the files below `path`, without following symlinks.
fn tree_bytes(path: &Path) -> (u64, i64) {
    let mut bytes = 0;
    let mut newest = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            bytes += meta.len();
            newest = newest.max(mtime_secs(&meta));
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    (bytes, newest)
}

/// The build folders of the secondary worktrees of the repo that holds the
/// current directory. Report only: a primary checkout is never listed.
pub fn scan(_store: &Store, _now: i64) -> Vec<Item> {
    let Ok(repo) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(worktrees) = crate::worktree::list_worktrees(&repo) else {
        return Vec::new();
    };
    worktrees
        .iter()
        .skip(1)
        .flat_map(|entry| detect(&entry.path))
        .map(|found| {
            let (bytes, newest_mtime) = tree_bytes(&found.folder);
            Item {
                path: found.folder,
                bytes,
                newest_mtime,
                also: Vec::new(),
                due: None,
            }
        })
        .collect()
}

/// The build output store. It has no root: a removal names the worktree it
/// works in, so no run wide root reaches into one.
pub fn store() -> Store {
    Store {
        name: "build output",
        roots: Vec::new(),
        scan,
        action: Action::ReportOnly,
        retention_key: Some("gc.buildOutputIdleDays"),
        limits: Limits::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A git repo whose ignore file lists `ignore`, with the given files.
    fn repo(tag: &str, ignore: &str, files: &[&str]) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        git_in(&dir, &["init", "-q", "-b", "main"]);
        fs::write(dir.join(".gitignore"), ignore).unwrap();
        for file in files {
            put(&dir, file);
        }
        dir
    }

    fn put(dir: &Path, rel: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn folders(dir: &Path) -> Vec<String> {
        detect(dir)
            .iter()
            .map(|f| {
                f.folder
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn matcher_is_case_exact() {
        assert!(!matcher_is_exact("target", "Target"));
        assert!(matcher_is_exact("target", "target"));
    }

    #[test]
    fn fixture_rust() {
        let dir = repo("bo-rust", "target/\n", &["Cargo.toml", "target/debug/x"]);
        assert_eq!(folders(&dir), ["target"]);
    }

    #[test]
    fn fixture_node() {
        let dir = repo(
            "bo-node",
            "node_modules/\n.next/\n",
            &["package.json", "node_modules/a/x", ".next/y"],
        );
        assert_eq!(folders(&dir), [".next", "node_modules"]);
    }

    #[test]
    fn fixture_python() {
        let dir = repo(
            "bo-python",
            "__pycache__/\n",
            &["pyproject.toml", "src/pkg/__pycache__/x.pyc"],
        );
        assert_eq!(folders(&dir), ["src/pkg/__pycache__"]);
    }

    #[test]
    fn fixture_java() {
        let dir = repo(
            "bo-java",
            "target/\n.gradle/\n",
            &["pom.xml", "target/c", ".gradle/g"],
        );
        assert_eq!(folders(&dir), [".gradle", "target"]);
    }

    #[test]
    fn fixture_dotnet() {
        let dir = repo(
            "bo-dotnet",
            "obj/\nbin/\n",
            &["App.csproj", "obj/x", "bin/y"],
        );
        assert_eq!(folders(&dir), ["obj"]);
    }

    #[test]
    fn fixture_swift_zig() {
        let dir = repo(
            "bo-swift",
            ".build/\n.zig-cache/\n",
            &["Package.swift", ".build/x", ".zig-cache/y"],
        );
        assert_eq!(folders(&dir), [".build", ".zig-cache"]);
    }

    #[test]
    fn fixture_go_matches_nothing() {
        let dir = repo("bo-go", "vendor/\nbin/\n", &["go.mod", "vendor/x", "bin/y"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn tracked_folder_is_protected() {
        let dir = repo("bo-tracked", "target/\n", &["Cargo.toml", "target/kept"]);
        git_in(&dir, &["add", "-f", "target/kept"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn folder_without_marker_is_ignored() {
        let dir = repo("bo-nomarker", "target/\n", &["target/x"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn non_ignored_folder_is_ignored() {
        let dir = repo("bo-notignored", "", &["Cargo.toml", "target/x"]);
        assert!(folders(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_is_skipped() {
        let dir = repo("bo-symlink", "target\n", &["Cargo.toml"]);
        let elsewhere = repo("bo-symlink-elsewhere", "", &["x"]);
        std::os::unix::fs::symlink(&elsewhere, dir.join("target")).unwrap();
        assert!(folders(&dir).is_empty());
        assert!(elsewhere.join("x").exists());
    }

    #[test]
    fn nested_git_is_skipped() {
        let dir = repo("bo-nested", "target/\n", &["Cargo.toml", "target/inner/x"]);
        fs::create_dir_all(dir.join("target/.git")).unwrap();
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn submodule_is_skipped() {
        let dir = repo("bo-submodule", "target/\n", &["Cargo.toml", "target/x"]);
        // A tracked gitlink at the folder path makes `ls-files` list it.
        let sha = "1".repeat(40);
        git_in(
            &dir,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},target"),
            ],
        );
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn git_file_worktree_is_handled() {
        let main = repo("bo-wt-main", "target/\n", &["Cargo.toml"]);
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-q", "-m", "init"]);
        let linked = main.with_file_name(format!(
            "{}-linked",
            main.file_name().unwrap().to_string_lossy()
        ));
        git_in(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                linked.to_str().unwrap(),
            ],
        );
        put(&linked, "target/debug/x");
        assert!(linked.join(".git").is_file());
        assert_eq!(folders(&fs::canonicalize(&linked).unwrap()), ["target"]);
    }

    #[test]
    fn external_cargo_target_untouched() {
        let dir = repo("bo-external", "target/\n", &["Cargo.toml"]);
        let external = repo("bo-external-target", "", &["debug/x"]);
        assert!(folders(&dir).is_empty());
        assert!(external.join("debug/x").exists());
    }

    #[test]
    fn dist_is_not_in_the_table() {
        let dir = repo("bo-dist", "dist/\n", &["pyproject.toml", "dist/pkg.whl"]);
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn venv_is_out_of_v1() {
        let dir = repo(
            "bo-venv",
            ".venv/\nvenv/\nenv/\n",
            &[
                "pyproject.toml",
                ".venv/pyvenv.cfg",
                ".venv/lib/python3.12/__pycache__/x.pyc",
                "venv/lib/python3.12/__pycache__/x.pyc",
                "env/pyvenv.cfg",
                "env/lib/python3.12/__pycache__/x.pyc",
            ],
        );
        assert!(!TABLE
            .iter()
            .flat_map(|eco| eco.folders)
            .any(|name| VENV_NAMES.contains(name)));
        assert!(folders(&dir).is_empty());
    }

    #[test]
    fn scan_depth_4_found_5_not() {
        let dir = repo(
            "bo-depth",
            "target/\n",
            &["Cargo.toml", "a/b/c/target/x", "a/b/c/d/target/x"],
        );
        assert_eq!(folders(&dir), ["a/b/c/target"]);
    }
}
