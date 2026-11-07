// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Whether the user switched the playbook skills plugin off in Claude Code,
//! and whether Claude Code lists it as installed.
//!
//! `playbook setup` writes user-level hook entries and a status line into
//! `~/.claude/settings.json`. Disabling the plugin in Claude Code sets
//! `enabledPlugins["playbook@pragmatic-engineer"]` to `false` and leaves those
//! entries in place, so every hook entry point and the status line ask here
//! first. Only an explicit `false` pauses playbook. A missing key, a missing
//! file and a file that cannot be read or parsed all count as enabled, so
//! Homebrew-only users and broken settings never lose their guards.

use std::path::Path;

/// The plugin's key in `enabledPlugins`.
pub const PLUGIN_KEY: &str = "playbook@pragmatic-engineer";

/// Marketplace names Claude Code may list the plugin under besides the one in
/// [`PLUGIN_KEY`]. The marketplace is named `pragmatic-engineer` today. If its
/// owner renames it to a new organization name, installs under that name must
/// work too, and so must installs that kept the old one.
pub const ALT_MARKETPLACES: [&str; 2] = ["amplifiedengineer", "amplified-engineer"];

/// Every key the plugin may have in `enabledPlugins` and `installed_plugins.json`,
/// the preferred one first.
pub fn plugin_keys() -> Vec<String> {
    std::iter::once(PLUGIN_KEY.to_string())
        .chain(ALT_MARKETPLACES.iter().map(|m| format!("playbook@{m}")))
        .collect()
}

/// Every key the retired `playbook-hooks` plugin may have.
pub fn hooks_plugin_keys() -> Vec<String> {
    std::iter::once(HOOKS_PLUGIN_KEY.to_string())
        .chain(
            ALT_MARKETPLACES
                .iter()
                .map(|m| format!("playbook-hooks@{m}")),
        )
        .collect()
}

/// Set to `1` to run hooks and the status line even when the plugin is disabled.
pub const IGNORE_ENV: &str = "PLAYBOOK_IGNORE_PLUGIN_DISABLED";

/// Whether this process should do nothing because the plugin is disabled.
/// Reads `~/.claude/settings.json` once. The escape hatch wins over the file.
pub fn paused() -> bool {
    if std::env::var(IGNORE_ENV).is_ok_and(|v| v == "1") {
        return false;
    }
    is_disabled_in(&crate::common::home_dir())
}

/// Whether `home`'s `.claude/settings.json` sets the plugin to `false`.
pub fn is_disabled_in(home: &Path) -> bool {
    disabled_in_file(&home.join(".claude").join("settings.json"))
}

/// Whether the settings file at `path` sets the plugin to `false`.
pub fn disabled_in_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    // Most users have no entry, so skip the JSON parse unless the key is there.
    if !plugin_keys().iter().any(|k| text.contains(k.as_str())) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .is_some_and(|v| disables_plugin(&v))
}

/// Whether a parsed settings value sets the plugin to `false`, which pauses
/// every hook.
pub fn disables_plugin(settings: &serde_json::Value) -> bool {
    plugin_keys().iter().any(|key| {
        settings.pointer("/enabledPlugins").and_then(|e| e.get(key))
            == Some(&serde_json::Value::Bool(false))
    })
}

/// The key of the `playbook-hooks` plugin. `playbook uninstall` and the doctor
/// hooks row read it, to tell a machine that installed the plugin how to remove it.
pub const HOOKS_PLUGIN_KEY: &str = "playbook-hooks@pragmatic-engineer";

/// The marketplace the plugin comes from.
pub const MARKETPLACE: &str = "pragmatic-engineer";

fn installed_entry(home: &Path, key: &str) -> Option<Option<serde_json::Value>> {
    installed_value(&home.join(".claude"), key)
}

/// `key`'s entry in `<claude_home>/plugins/installed_plugins.json`: a list of
/// scoped entries (version 2) or one object (version 1). `None` when the file
/// is missing, malformed or does not list `key`.
pub fn installed_entry_at(claude_home: &Path, key: &str) -> Option<serde_json::Value> {
    installed_value(claude_home, key).flatten()
}

fn installed_value(claude_home: &Path, key: &str) -> Option<Option<serde_json::Value>> {
    let value = read_installed(claude_home)?;
    let find = |v: &serde_json::Value| v.get(key).filter(|e| !e.is_null()).cloned();
    Some(value.get("plugins").and_then(find).or_else(|| find(&value)))
}

/// The keys in `installed_plugins.json` for the retired `playbook-hooks`
/// plugin, or for a `playbook` plugin from another marketplace when this
/// marketplace's plugin is installed too. Both run next to the one playbook
/// supports. An install scoped to another project does not count, because
/// Claude Code does not load it from `cwd`. Empty when the file is missing
/// or malformed.
pub fn other_playbook_plugins(claude_home: &Path, cwd: &Path) -> Vec<String> {
    let Some(value) = read_installed(claude_home) else {
        return Vec::new();
    };
    let map = value.get("plugins").unwrap_or(&value);
    let active = |key: &str| map.get(key).is_some_and(|v| loaded_here(v, cwd));
    let own_keys = plugin_keys();
    let hooks_keys = hooks_plugin_keys();
    let ours = own_keys.iter().any(|k| active(k));
    let mut keys: Vec<String> = map
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|k| {
            hooks_keys.iter().any(|h| h == k.as_str())
                || (ours && k.starts_with("playbook@") && !own_keys.iter().any(|o| o == k.as_str()))
        })
        .filter(|k| active(k))
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// Whether an `installed_plugins.json` entry (a version 2 list or a version
/// 1 object) has an install Claude Code loads from `cwd`: one without a
/// `projectPath`, or one whose `projectPath` holds `cwd`.
fn loaded_here(entry: &serde_json::Value, cwd: &Path) -> bool {
    let loads = |e: &serde_json::Value| match e.get("projectPath").and_then(|p| p.as_str()) {
        Some(project) => cwd.starts_with(project),
        None => true,
    };
    match entry {
        serde_json::Value::Array(list) => list.iter().any(loads),
        serde_json::Value::Object(_) => loads(entry),
        _ => false,
    }
}

fn read_installed(claude_home: &Path) -> Option<serde_json::Value> {
    let path = claude_home.join("plugins/installed_plugins.json");
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Whether Claude Code lists `key` as installed, or `None` when
/// `installed_plugins.json` is missing or cannot be parsed.
pub fn listed_in(home: &Path, key: &str) -> Option<bool> {
    installed_entry(home, key).map(|entry| entry.is_some())
}

/// Whether Claude Code lists the plugin as installed, or `None` when
/// `installed_plugins.json` is missing or cannot be parsed.
pub fn installed_in(home: &Path) -> Option<bool> {
    let seen: Vec<bool> = plugin_keys()
        .iter()
        .filter_map(|k| listed_in(home, k))
        .collect();
    (!seen.is_empty()).then(|| seen.contains(&true))
}

/// Whether the marketplace is listed in `known_marketplaces.json`.
pub fn marketplace_known_in(home: &Path) -> bool {
    std::fs::read_to_string(home.join(".claude/plugins/known_marketplaces.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| {
            std::iter::once(MARKETPLACE)
                .chain(ALT_MARKETPLACES)
                .any(|name| v.get(name).is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    #[test]
    fn a_plugin_key_under_a_renamed_marketplace_pauses_and_counts_as_installed() {
        for marketplace in ALT_MARKETPLACES {
            let home = home_with(Some(&format!(
                r#"{{"enabledPlugins":{{"playbook@{marketplace}":false}}}}"#
            )));
            assert!(is_disabled_in(&home), "{marketplace}");
            std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
            std::fs::write(
                home.join(".claude/plugins/installed_plugins.json"),
                format!(
                    r#"{{"version":2,"plugins":{{"playbook@{marketplace}":[{{"scope":"user"}}]}}}}"#
                ),
            )
            .unwrap();
            assert_eq!(installed_in(&home), Some(true), "{marketplace}");
            std::fs::write(
                home.join(".claude/plugins/known_marketplaces.json"),
                format!(r#"{{"{marketplace}":{{}}}}"#),
            )
            .unwrap();
            assert!(marketplace_known_in(&home), "{marketplace}");
        }
    }

    #[test]
    fn the_old_key_and_the_new_key_listed_together_are_not_reported_as_extra_installs() {
        let home = home_with(None);
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"playbook@pragmatic-engineer":[{}],"playbook@amplifiedengineer":[{}],"playbook-hooks@amplifiedengineer":[{}]}}"#,
        )
        .unwrap();
        assert_eq!(
            other_playbook_plugins(&home.join(".claude"), Path::new("/p")),
            vec!["playbook-hooks@amplifiedengineer"]
        );
    }

    fn home_with(settings: Option<&str>) -> std::path::PathBuf {
        let home = scratch_dir("plugin-state");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        if let Some(text) = settings {
            std::fs::write(home.join(".claude/settings.json"), text).unwrap();
        }
        home
    }

    #[test]
    fn an_explicit_false_means_disabled() {
        let home = home_with(Some(
            r#"{"enabledPlugins":{"playbook@pragmatic-engineer":false}}"#,
        ));
        assert!(is_disabled_in(&home));
    }

    #[test]
    fn true_means_enabled() {
        let home = home_with(Some(
            r#"{"enabledPlugins":{"playbook@pragmatic-engineer":true}}"#,
        ));
        assert!(!is_disabled_in(&home));
    }

    #[test]
    fn an_absent_key_or_absent_block_means_enabled() {
        assert!(!is_disabled_in(&home_with(Some(
            r#"{"enabledPlugins":{"other@x":false}}"#
        ))));
        assert!(!is_disabled_in(&home_with(Some(r#"{"hooks":{}}"#))));
    }

    #[test]
    fn a_missing_file_means_enabled() {
        assert!(!is_disabled_in(&home_with(None)));
    }

    #[test]
    fn a_malformed_file_means_enabled() {
        let home = home_with(Some(
            r#"{"enabledPlugins":{"playbook@pragmatic-engineer":false"#,
        ));
        assert!(!is_disabled_in(&home));
    }

    #[test]
    fn a_non_boolean_value_means_enabled() {
        let home = home_with(Some(
            r#"{"enabledPlugins":{"playbook@pragmatic-engineer":"false"}}"#,
        ));
        assert!(!is_disabled_in(&home));
    }

    fn write_installed(home: &std::path::Path, body: &str) {
        let dir = home.join(".claude/plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("installed_plugins.json"), body).unwrap();
    }

    #[test]
    fn installed_in_reads_both_shapes_of_the_plugin_list() {
        let v2 = home_with(None);
        write_installed(
            &v2,
            r#"{"version":2,"plugins":{"playbook@pragmatic-engineer":[{"scope":"user"}]}}"#,
        );
        assert_eq!(installed_in(&v2), Some(true));
        let v1 = home_with(None);
        write_installed(
            &v1,
            r#"{"version":1,"plugins":{"playbook@pragmatic-engineer":{"version":"0.25.0"}}}"#,
        );
        assert_eq!(installed_in(&v1), Some(true));
        let empty = home_with(None);
        write_installed(&empty, r#"{"version":2,"plugins":{}}"#);
        assert_eq!(installed_in(&empty), Some(false));
    }

    #[test]
    fn installed_in_is_unknown_when_the_list_is_missing_or_malformed() {
        assert_eq!(installed_in(&home_with(None)), None);
        let bad = home_with(None);
        write_installed(&bad, "{not json");
        assert_eq!(installed_in(&bad), None);
    }

    #[test]
    fn other_playbook_plugins_lists_the_hooks_plugin_and_other_marketplaces() {
        let home = home_with(None);
        let claude_home = home.join(".claude");
        let cwd = home.join("repo");
        assert!(other_playbook_plugins(&claude_home, &cwd).is_empty());
        std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
        let write = |text: &str| {
            std::fs::write(claude_home.join("plugins/installed_plugins.json"), text).unwrap()
        };
        write(
            r#"{"version":2,"plugins":{"playbook@pragmatic-engineer":[{}],"playbook-hooks@pragmatic-engineer":[{}],"playbook@fork":[{"projectPath":"/elsewhere"},{}],"playbook@team":[{"projectPath":"/elsewhere"}],"other@x":[{}]}}"#,
        );
        assert_eq!(
            other_playbook_plugins(&claude_home, &cwd),
            vec!["playbook-hooks@pragmatic-engineer", "playbook@fork"]
        );
        // A fork used alone is the user's playbook, not a second one.
        write(r#"{"version":2,"plugins":{"playbook@fork":[{}]}}"#);
        assert!(other_playbook_plugins(&claude_home, &cwd).is_empty());
    }
}
