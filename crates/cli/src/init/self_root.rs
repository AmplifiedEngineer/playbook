// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Resolves `InitPaths::self_root`, the files `init` seeds from:
//! `CLAUDE_PLUGIN_ROOT` when it holds them, or its `seed/` folder when it is
//! a dev checkout (ADR 0047), else the copies compiled into this binary. A plugin cache directory is never a source,
//! because an old plugin there ships an old settings template (ADR 0034).
//! The plugin's skills, commands and agents are found by `crate::plugin_root`.

use std::path::{Path, PathBuf};

const EMBEDDED_SETTINGS: &str = include_str!("../../../../seed/settings.shared.json");
const EMBEDDED_PROMPT: &str = include_str!("../../../../seed/prompts/SYSTEM_PROMPT.md");
const EMBEDDED_BREWFILE: &str = include_str!("../../../../seed/Brewfile");

/// True when `root` holds the two files `init` seeds from. The skills plugin
/// does not ship them (the hooks plugin and this binary do), so a plugin root
/// that lacks them is not a source for `init`.
pub fn has_seed_files(root: &Path) -> bool {
    root.join("settings.shared.json").is_file() && root.join("prompts/SYSTEM_PROMPT.md").is_file()
}

/// Where a checkout keeps the seed files.
pub(crate) const CHECKOUT_SEED_DIR: &str = "seed";

/// The seed folder inside the checkout at `root`: `seed/` when it holds the
/// seed files, else `root` itself.
pub(crate) fn seed_in_checkout(root: &Path) -> PathBuf {
    let nested = root.join(CHECKOUT_SEED_DIR);
    if has_seed_files(&nested) {
        nested
    } else {
        root.to_path_buf()
    }
}

/// The env root, or its `seed/` folder, when it holds the seed files and is
/// not another plugin, such as the retired `playbook-hooks`, else `None`.
/// Writes nothing, for a dry run.
pub fn seed_root(claude_plugin_root_env: Option<&str>) -> Option<PathBuf> {
    claude_plugin_root_env
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|root| crate::plugin_root::is_ours(root))
        .map(|root| seed_in_checkout(&root))
        .filter(|root| has_seed_files(root))
}

/// `seed_root`, then the copies of the settings template and system prompt
/// compiled into this binary. A Homebrew install ships only the binary, and
/// the plugin ships neither file.
/// The embedded copies are written under `$HOME/.config/playbook/embedded/`
/// (playbook's own state, never Claude Code's) and that directory is returned.
pub fn resolve_or_embedded(claude_plugin_root_env: Option<&str>, home: &Path) -> Option<PathBuf> {
    seed_root(claude_plugin_root_env).or_else(|| write_embedded(home).ok())
}

/// The embedded Brewfile, written under the same embedded directory.
pub fn embedded_brewfile(home: &Path) -> std::io::Result<PathBuf> {
    Ok(write_embedded(home)?.join("Brewfile"))
}

fn write_embedded(home: &Path) -> std::io::Result<PathBuf> {
    let root = crate::common::paths::playbook_root_from(home)
        .join("embedded")
        .join(env!("CARGO_PKG_VERSION"));
    std::fs::create_dir_all(root.join("prompts"))?;
    std::fs::write(root.join("settings.shared.json"), EMBEDDED_SETTINGS)?;
    std::fs::write(root.join("prompts/SYSTEM_PROMPT.md"), EMBEDDED_PROMPT)?;
    std::fs::write(root.join("Brewfile"), EMBEDDED_BREWFILE)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "playbook-self-root-{tag}-{}-{:?}",
            crate::testing::run_id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn seeded(root: &Path) {
        fs::create_dir_all(root.join("prompts")).expect("prompts dir");
        fs::write(root.join("settings.shared.json"), "{}").expect("settings");
        fs::write(root.join("prompts/SYSTEM_PROMPT.md"), "p").expect("prompt");
    }

    #[test]
    fn with_no_env_root_the_embedded_files_are_written_and_returned() {
        // Arrange
        let home = scratch("embedded");

        // Act
        let got = resolve_or_embedded(None, &home).expect("embedded root");

        // Assert
        assert!(got.starts_with(home.join(".config/playbook/embedded")));
        assert_eq!(
            fs::read_to_string(got.join("settings.shared.json")).unwrap(),
            EMBEDDED_SETTINGS
        );
        assert!(got.join("prompts/SYSTEM_PROMPT.md").is_file());
        assert!(got.join("Brewfile").is_file());
    }

    #[test]
    fn an_env_root_with_the_seed_files_is_preferred_over_the_embedded_files() {
        // Arrange
        let home = scratch("embedded-not-needed");
        let root = home.join("plugin-root");
        seeded(&root);

        // Act
        let got = resolve_or_embedded(root.to_str(), &home);

        // Assert
        assert_eq!(got, Some(root));
        assert!(!home.join(".config/playbook/embedded").exists());
    }

    #[test]
    fn a_checkout_root_resolves_to_its_seed_folder() {
        // Arrange: a checkout keeps the seed files under seed/.
        let home = scratch("checkout-seed");
        let root = home.join("checkout");
        seeded(&root.join(CHECKOUT_SEED_DIR));

        // Act
        let got = resolve_or_embedded(root.to_str(), &home);

        // Assert
        assert_eq!(got, Some(root.join(CHECKOUT_SEED_DIR)));
        assert!(!home.join(".config/playbook/embedded").exists());
    }

    #[test]
    fn an_empty_seed_folder_leaves_the_root_as_the_seed() {
        // Arrange
        let home = scratch("root-and-seed");
        let root = home.join("checkout");
        seeded(&root);
        fs::create_dir_all(root.join(CHECKOUT_SEED_DIR)).expect("empty seed dir");

        // Act
        let got = seed_root(root.to_str());

        // Assert
        assert_eq!(got, Some(root));
    }

    #[test]
    fn an_env_root_without_the_seed_files_falls_back_to_the_embedded_files() {
        // Arrange: the plugin ships neither file.
        let home = scratch("skills-only-root");
        let root = home.join("skills-plugin");
        fs::create_dir_all(root.join("skills")).expect("skills dir");

        // Act
        let got = resolve_or_embedded(root.to_str(), &home).expect("embedded");

        // Assert
        assert!(got.starts_with(home.join(".config/playbook/embedded")));
        assert!(has_seed_files(&got));
    }

    #[test]
    fn an_old_plugin_cache_with_seed_files_is_never_a_seed_source() {
        // Arrange: a 0.25 era plugin cache dir shipped the seed files.
        let home = scratch("old-cache-seed");
        seeded(&home.join(".claude/plugins/cache/amplifiedengineer/playbook/0.25.0"));

        // Act
        let dry = seed_root(None);
        let got = resolve_or_embedded(None, &home).expect("embedded");

        // Assert
        assert_eq!(dry, None);
        assert!(got.starts_with(home.join(".config/playbook/embedded")));
    }

    #[test]
    fn an_empty_env_var_counts_as_unset() {
        assert_eq!(seed_root(Some("")), None);
    }

    #[test]
    fn the_retired_hooks_plugin_is_never_a_seed_source() {
        // Arrange: the old playbook-hooks plugin shipped the seed files.
        let home = scratch("hooks-plugin-seed");
        let root = home.join("playbook-hooks");
        seeded(&root);
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"playbook-hooks","version":"0.25.0"}"#,
        )
        .unwrap();

        // Act and assert
        assert_eq!(seed_root(root.to_str()), None);
    }

    #[test]
    fn the_retired_hooks_plugin_with_a_seed_folder_is_never_a_seed_source() {
        // Arrange: the manifest at the root names another plugin.
        let home = scratch("hooks-plugin-seed-dir");
        let root = home.join("playbook-hooks");
        seeded(&root.join(CHECKOUT_SEED_DIR));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"playbook-hooks","version":"0.25.0"}"#,
        )
        .unwrap();

        // Act and assert
        assert_eq!(seed_root(root.to_str()), None);
    }

    #[test]
    fn a_seed_folder_with_the_files_wins_over_the_root() {
        // Arrange: a checkout partway through the move holds both copies.
        let home = scratch("both-seeded");
        let root = home.join("checkout");
        seeded(&root);
        seeded(&root.join(CHECKOUT_SEED_DIR));

        // Act and assert
        assert_eq!(seed_root(root.to_str()), Some(root.join(CHECKOUT_SEED_DIR)));
    }
}
