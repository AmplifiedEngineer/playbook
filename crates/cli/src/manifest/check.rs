// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Port of the retired shell original, guarding the tracked-file allowlist so
//! runtime state or a re-tracked personal settings.json cannot leak into the
//! public repo. Every tracked path must be an allowlisted top-level file or
//! live under an allowlisted top-level directory.
//!
//! One deliberate divergence from the shell: it required `jq` on PATH before
//! doing anything (`check-manifest.sh:17`) but never called it, and this port
//! does no JSON work, so the dead requirement is dropped rather than ported.
//!
//! The shell's optional REPO_ROOT argument IS preserved: omitting it falls
//! back to `git rev-parse --show-toplevel` (see [`toplevel`]), so running the
//! subcommand bare from anywhere inside the repo behaves as the script did.

use std::path::{Path, PathBuf};

/// Top-level files the manifest allows verbatim.
const ALLOW_FILES: [&str; 16] = [
    ".gitignore",
    "CHANGELOG.md",
    "README.md",
    "LICENSE",
    "NOTICE",
    "NOTICE-pbdoc",
    "justfile",
    "install.sh",
    "install-pbtop.sh",
    "CODE_OF_CONDUCT.md",
    "CONTRIBUTING.md",
    "SECURITY.md",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "osv-scanner.toml",
];

/// Top-level directories any tracked file may live under.
const ALLOW_DIRS: [&str; 8] = [
    "seed",
    "plugins",
    "scripts",
    "docs",
    ".github",
    "crates",
    ".cargo",
    "packaging",
];

/// A personal settings.json must never track, at the root or inside an
/// allowed folder such as `seed/`, where the folder rule would admit it.
/// Returns the offender's report line, or `None` if `path` is allowlisted.
fn offender(path: &str) -> Option<String> {
    if path == "settings.json" || path.ends_with("/settings.json") {
        return Some(format!(
            "{path}  (personal settings.json must not be tracked)"
        ));
    }
    let allowed = match path.split_once('/') {
        Some((top, _rest)) => ALLOW_DIRS.contains(&top),
        None => ALLOW_FILES.contains(&path),
    };
    (!allowed).then(|| path.to_string())
}

/// Pure decision: given the tracked paths `git ls-files` reports, return the
/// offenders in encounter order. No filesystem or git access, so every
/// allowlist case is exercised directly against a plain list, no repo needed.
fn violations(tracked: &[String]) -> Vec<String> {
    tracked
        .iter()
        .filter(|path| !path.is_empty())
        .filter_map(|path| offender(path))
        .collect()
}

/// Runs `git ls-files` against `repo_root` and checks every tracked path
/// against the allowlist. Mirrors the shell original: success lists the
/// tracked-file total, failure lists every offender.
pub fn check(repo_root: &Path) -> Result<String, String> {
    if !repo_root.is_dir() {
        return Err(format!(
            "repo root is not a directory: {}",
            repo_root.display()
        ));
    }

    let output = crate::common::git::run(
        Some(repo_root),
        &["ls-files"],
        crate::common::git::SLOW_TIMEOUT,
    )
    .ok_or("failed to run git ls-files (git missing or timed out)")?;
    if !output.status.success() {
        return Err(format!("not a git repository: {}", repo_root.display()));
    }

    let tracked: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    let total = tracked.len();
    let offenders = violations(&tracked);

    if offenders.is_empty() {
        return Ok(format!(
            "check-manifest: OK ({total} tracked files, all allowlisted)"
        ));
    }

    let mut message = format!("{} tracked file(s) outside the allowlist:", offenders.len());
    for item in &offenders {
        message.push_str(&format!("\n  {item}"));
    }
    Err(message)
}

/// The enclosing repo root, mirroring the shell's default when no `REPO_ROOT`
/// argument was given (check-manifest.sh:21-22). `None` when the cwd is not
/// inside a git repository.
pub fn toplevel() -> Option<PathBuf> {
    let path = crate::common::git::output(Path::new("."), &["rev-parse", "--show-toplevel"])?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlisted_top_level_file_passes() {
        let tracked = vec!["README.md".to_string()];
        assert!(violations(&tracked).is_empty());
    }

    /// The plugin contract files and the scripts folder are allowlisted.
    #[test]
    fn the_plugin_contract_files_are_allowlisted() {
        let tracked: Vec<String> = [
            "plugins/intents.json",
            "CHANGELOG.md",
            "scripts/check-intents.sh",
        ]
        .iter()
        .map(|p| (*p).to_string())
        .collect();
        assert!(violations(&tracked).is_empty());
    }

    /// Pins an ALLOW_FILES entry, so a future edit that drops or typos it
    /// fails here directly instead of only being caught by
    /// `playbook manifest check .` against the real repo, which `cargo test`
    /// never runs.
    #[test]
    fn rust_toolchain_toml_is_allowlisted() {
        let tracked = vec!["rust-toolchain.toml".to_string()];
        assert!(violations(&tracked).is_empty());
    }

    /// The Nix flake lives in pragmatic-engineer/nix, so a root flake is an offender.
    #[test]
    fn the_nix_flake_files_are_offenders() {
        let tracked = vec!["flake.nix".to_string(), "flake.lock".to_string()];
        assert_eq!(violations(&tracked), tracked);
    }

    #[test]
    fn the_retired_breaking_changes_json_is_an_offender() {
        let tracked = vec!["breaking-changes.json".to_string()];
        assert_eq!(violations(&tracked), tracked);
    }

    #[test]
    fn the_retired_top_level_plugin_and_shell_directories_are_offenders() {
        let tracked: Vec<String> = [
            "skills/a/SKILL.md",
            "commands/a.md",
            "agents/a.md",
            "shell/worktree.zsh",
            "output-styles/a.md",
            ".claude-plugin/plugin.json",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(violations(&tracked), tracked);
    }

    #[test]
    fn the_retired_bin_directory_is_an_offender() {
        let tracked = vec!["bin/playbook".to_string()];
        assert_eq!(violations(&tracked).len(), 1);
    }

    /// Package manifest templates (scoop) live under `packaging/`.
    #[test]
    fn packaging_templates_are_allowlisted() {
        let tracked = vec!["packaging/scoop/playbook.json.tmpl".to_string()];
        assert!(violations(&tracked).is_empty());
    }

    #[test]
    fn allowlisted_top_level_directory_passes() {
        let tracked = vec!["scripts/check-links.sh".to_string()];
        assert!(violations(&tracked).is_empty());
    }

    #[test]
    fn the_packaging_directory_passes() {
        let tracked = vec!["packaging/aur/PKGBUILD.tmpl".to_string()];
        assert!(violations(&tracked).is_empty());
    }

    #[test]
    fn disallowed_top_level_file_is_an_offender() {
        let tracked = vec!["stray.txt".to_string()];
        assert_eq!(violations(&tracked), vec!["stray.txt".to_string()]);
    }

    #[test]
    fn new_disallowed_top_level_directory_is_an_offender() {
        // This is the exact failure the validator exists to catch: a brand
        // new top-level directory that was never allowlisted.
        let tracked = vec!["sessions/leaked.json".to_string()];
        assert_eq!(
            violations(&tracked),
            vec!["sessions/leaked.json".to_string()]
        );
    }

    #[test]
    fn tracked_personal_settings_json_is_an_offender() {
        let tracked = vec!["settings.json".to_string()];
        let result = violations(&tracked);
        assert_eq!(result.len(), 1);
        assert!(result[0].contains("settings.json"));
        assert!(result[0].contains("personal settings.json must not be tracked"));
    }

    #[test]
    fn a_personal_settings_json_inside_an_allowed_folder_is_an_offender() {
        let tracked = vec!["seed/settings.json".to_string()];
        let result = violations(&tracked);
        assert_eq!(result.len(), 1);
        assert!(result[0].contains("personal settings.json must not be tracked"));
    }

    #[test]
    fn clean_skeleton_has_no_offenders() {
        let tracked = vec![
            ".gitignore".to_string(),
            "README.md".to_string(),
            "LICENSE".to_string(),
            "seed/settings.shared.json".to_string(),
            "scripts/check-links.sh".to_string(),
            "seed/prompts/system.md".to_string(),
            "docs/README.md".to_string(),
            ".github/workflows/ci.yml".to_string(),
        ];
        assert!(violations(&tracked).is_empty());
    }

    #[test]
    fn empty_lines_are_skipped_not_counted_as_offenders() {
        let tracked = vec![String::new(), "README.md".to_string(), String::new()];
        assert!(violations(&tracked).is_empty());
    }
}
