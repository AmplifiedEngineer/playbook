// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Doctor rows for the cargo settings that keep `target` folders small, and
//! for the warm `target` a new worktree is cloned from. Only a Rust repo gets
//! rows. Nothing here writes: the hints say what to change.

use super::check::{Env, Level, Row};
use crate::cc::worktree::main_worktree;
use crate::hooks::worktree_create::git;
use std::path::Path;

/// The section of a cargo config that sets the dev profile.
const DEV_PROFILE_SECTION: &str = "[profile.dev]";
/// What to add to `~/.cargo/config.toml`.
const DEV_PROFILE_FIX: &str =
    "add to ~/.cargo/config.toml: [profile.dev] with debug = 0 and incremental = false";
/// Values of `debug` that turn debug info off.
const DEBUG_OFF: [&str; 3] = ["0", "false", "\"none\""];

/// What a cargo config sets in its `[profile.dev]` section.
#[derive(Debug, Default, PartialEq, Eq)]
struct DevProfile {
    debug: Option<String>,
    incremental: Option<String>,
}

impl DevProfile {
    fn is_lean(&self) -> bool {
        self.debug
            .as_deref()
            .is_some_and(|v| DEBUG_OFF.contains(&v))
            && self.incremental.as_deref() == Some("false")
    }
}

fn parse_dev_profile(text: &str) -> DevProfile {
    let mut profile = DevProfile::default();
    let mut in_dev = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_dev = line == DEV_PROFILE_SECTION;
        } else if let (true, Some((key, value))) = (in_dev, line.split_once('=')) {
            let value = Some(value.trim().to_string());
            match key.trim() {
                "debug" => profile.debug = value,
                "incremental" => profile.incremental = value,
                _ => {}
            }
        }
    }
    profile
}

/// Rows for the repo in `env`, or none when it is not a Rust repo.
pub(crate) fn rows(env: &Env) -> Vec<Row> {
    let Some(root) = env
        .repo_root
        .as_deref()
        .filter(|r| r.join("Cargo.toml").is_file())
    else {
        return Vec::new();
    };
    let user_config =
        std::fs::read_to_string(env.home.join(".cargo/config.toml")).unwrap_or_default();
    let mut rows = vec![profile_row(&parse_dev_profile(&user_config))];
    if crate::hooks::worktree_target::CLONES_TARGET {
        rows.extend(clone_row(root));
    }
    rows
}

fn profile_row(profile: &DevProfile) -> Row {
    if profile.is_lean() {
        Row::new(
            "-",
            Level::Info,
            "~/.cargo/config.toml sets the dev profile to debug = 0 and incremental = false, so target folders stay small",
        )
    } else {
        Row::new(
            "-",
            Level::Info,
            "~/.cargo/config.toml does not turn off dev debug info and incremental data, so each target folder grows large",
        )
        .hint(DEV_PROFILE_FIX)
    }
}

/// Whether new worktrees will have a warm `target` to clone.
fn clone_row(repo: &Path) -> Option<Row> {
    let porcelain = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let main = Path::new(&main_worktree(&porcelain)?).to_path_buf();
    if main.join("target").is_dir() {
        if !crate::hooks::worktree_target::is_ignored(&main) {
            return Some(
                Row::new(
                    "-",
                    Level::Info,
                    "git does not ignore the target folder, so new worktrees do not clone it",
                )
                .hint("add target/ to .gitignore"),
            );
        }
        return Some(
            Row::new(
                "-",
                Level::Info,
                "new worktrees clone the main checkout's target folder",
            )
            .hint(
                "the clone saves most when main was last built with cargo build and the dev profile above",
            ),
        );
    }
    Some(
        Row::new(
            "-",
            Level::Info,
            "the main checkout has no target folder, so new worktrees start without a warm build",
        )
        .hint(format!(
            "run cargo build in {} so new worktrees can clone it",
            main.display()
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lean_dev_profile_is_recognised() {
        // Arrange
        let text = "[build]\njobs = 4\n\n[profile.dev]\ndebug = 0 # none\nincremental = false\n";

        // Act
        let profile = parse_dev_profile(text);

        // Assert
        assert!(profile.is_lean(), "{profile:?}");
    }

    #[test]
    fn settings_outside_the_dev_profile_do_not_count() {
        // Arrange
        let text = "[profile.release]\ndebug = 0\nincremental = false\n";

        // Act
        let profile = parse_dev_profile(text);

        // Assert
        assert_eq!(profile, DevProfile::default());
        assert!(!profile.is_lean());
    }

    #[test]
    fn debug_info_or_incremental_builds_are_not_lean() {
        for text in [
            "[profile.dev]\ndebug = 2\nincremental = false\n",
            "[profile.dev]\ndebug = 0\nincremental = true\n",
            "[profile.dev]\ndebug = 0\n",
            "",
        ] {
            assert!(!parse_dev_profile(text).is_lean(), "{text:?}");
        }
    }

    fn rust_repo(name: &str, gitignore: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pb-doctor-build-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.join(".gitignore"), gitignore).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(init.success());
        dir
    }

    #[test]
    fn the_clone_row_says_so_when_git_ignores_target() {
        // Arrange
        let repo = rust_repo("ignored", "target\n");

        // Act
        let row = clone_row(&repo).expect("row");

        // Assert
        assert!(
            row.message.contains("clone the main checkout"),
            "{}",
            row.message
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn the_clone_row_warns_when_git_does_not_ignore_target() {
        // Arrange
        let repo = rust_repo("unignored", "");

        // Act
        let row = clone_row(&repo).expect("row");

        // Assert
        assert!(row.message.contains("does not ignore"), "{}", row.message);
        assert_eq!(row.hint.as_deref(), Some("add target/ to .gitignore"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_repo_without_cargo_toml_gets_no_rows() {
        // Arrange
        let dir = std::env::temp_dir().join(format!("pb-doctor-build-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env = Env {
            home: dir.clone(),
            claude_home: dir.join(".claude"),
            plugin_root: None,
            shell: "/bin/zsh".to_string(),
            path_var: std::ffi::OsString::new(),
            repo_root: Some(dir.clone()),
            repo_slug: None,
        };

        // Act
        let found = rows(&env);

        // Assert
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hint_appears_only_when_the_profile_is_not_lean() {
        // Arrange
        let lean = parse_dev_profile("[profile.dev]\ndebug = 0\nincremental = false\n");

        // Act
        let (ok, bad) = (profile_row(&lean), profile_row(&DevProfile::default()));

        // Assert
        assert!(ok.hint.is_none());
        assert_eq!(bad.hint.as_deref(), Some(DEV_PROFILE_FIX));
    }
}
