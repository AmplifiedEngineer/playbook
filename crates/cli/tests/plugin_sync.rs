// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `plugin_update::sync` installs the playbook plugin when the caller allows
//! it. A fake `claude` logs its calls and, like the real one, writes the
//! marketplace and plugin files, so no test starts the real `claude`: the
//! probe names the fake directly.
//!
//! Every install test passes `force`, so a CI variable or the skip variable
//! in the caller's environment cannot change the result; the guard itself is
//! covered by the unit tests in `plugin_update.rs`.

#[path = "support/isolated.rs"]
mod isolated;

use playbook::plugin_update::{
    auto_update_after, behind, mark_auto_update, run, sync, AutoUpdate, Outcome, Probe, SyncOpts,
};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PLUGIN_ID: &str = "playbook@amplifiedengineer";
const MARKETPLACE_ADD: &str = "plugin marketplace add amplifiedengineer/marketplace --json\n";
const INSTALL: &str = "plugin install playbook@amplifiedengineer --json\n";
const MARKETPLACE_UPDATE: &str = "plugin marketplace update amplifiedengineer\n";
const UPDATE: &str = "plugin update playbook@amplifiedengineer\n";
const README_ADD: &str = "claude plugin marketplace add amplifiedengineer/marketplace";
const README_INSTALL: &str = "claude plugin install playbook@amplifiedengineer";

struct Fixture {
    home: PathBuf,
    claude: PathBuf,
}

impl Fixture {
    /// An empty home and a `claude` whose `install` exits `install_exit`
    /// and, on success, installs the plugin at the binary's version.
    fn new(install_exit: i32) -> Fixture {
        Fixture::listing(install_exit, VERSION)
    }

    /// Like [`Fixture::new`], for a marketplace whose newest plugin is
    /// `listed`.
    fn listing(install_exit: i32, listed: &str) -> Fixture {
        let home = isolated::fresh_home();
        let bin_dir = home.join("fake-bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let claude = bin_dir.join("claude");
        let plugin_dir = home
            .join(".claude/plugins/cache/amplifiedengineer/playbook")
            .join(listed);
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> "$HOME/claude-calls.log"
case "$*" in
"plugin marketplace add"*)
  mkdir -p "$HOME/.claude/plugins"
  printf '{{"extraKnownMarketplaces":{{"amplifiedengineer":{{"source":{{"source":"github","repo":"amplifiedengineer/marketplace"}}}}}}}}' > "$HOME/.claude/settings.json"
  printf '{{"amplifiedengineer":{{}}}}' > "$HOME/.claude/plugins/known_marketplaces.json"
  ;;
"plugin install"*)
  if [ {install_exit} -ne 0 ]; then echo 'install failed: network' >&2; exit {install_exit}; fi
  mkdir -p "{dir}/.claude-plugin" "{dir}/skills"
  printf '{{"name":"playbook","version":"{listed}"}}' > "{dir}/.claude-plugin/plugin.json"
  printf '{{"version":2,"plugins":{{"{PLUGIN_ID}":[{{"scope":"user","installPath":"{dir}"}}]}}}}' > "$HOME/.claude/plugins/installed_plugins.json"
  ;;
esac
"#,
            dir = plugin_dir.display()
        );
        // Written under another name and renamed, so no writer still holds the
        // script when it first runs.
        let staged = bin_dir.join("claude.tmp");
        fs::write(&staged, script).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&staged, &claude).unwrap();
        Fixture { home, claude }
    }

    fn probe(&self) -> Probe {
        Probe {
            home: self.home.clone(),
            claude: Some(self.claude.clone()),
        }
    }

    fn without_claude(&self) -> Probe {
        Probe {
            home: self.home.clone(),
            claude: None,
        }
    }

    /// Claude Code already lists the plugin at `version`, as an earlier
    /// install left it.
    fn installed_at(&self, version: &str) {
        let dir = self
            .home
            .join(".claude/plugins/cache/amplifiedengineer/playbook")
            .join(version);
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"playbook","version":"{version}"}}"#),
        )
        .unwrap();
        fs::write(
            self.home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"{PLUGIN_ID}":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
    }

    fn known_marketplace(&self) {
        let dir = self.home.join(".claude/plugins");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("known_marketplaces.json"),
            r#"{"amplifiedengineer":{}}"#,
        )
        .unwrap();
    }

    fn known_marketplace_named(&self, name: &str) {
        let dir = self.home.join(".claude/plugins");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("known_marketplaces.json"),
            format!(r#"{{"{name}":{{}}}}"#),
        )
        .unwrap();
    }

    /// What the fake logged. Asserting the file exists proves the fake ran.
    fn calls(&self) -> String {
        let log = self.home.join("claude-calls.log");
        assert!(log.exists(), "the fake claude never ran");
        fs::read_to_string(log).unwrap()
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    /// Writes `settings.json` as the exact `text`.
    fn write_settings(&self, text: &str) {
        fs::create_dir_all(self.home.join(".claude")).unwrap();
        fs::write(self.settings_path(), text).unwrap();
    }

    /// Writes `settings.json` with a `amplifiedengineer` entry holding
    /// `source` and, when given, `auto_update`.
    fn write_entry(&self, marketplace: &str, auto_update: Option<Value>) {
        let mut entry = json!({ "source": source() });
        if let Some(v) = auto_update {
            entry["autoUpdate"] = v;
        }
        let root = json!({ "theme": "dark", "extraKnownMarketplaces": { marketplace: entry } });
        self.write_settings(&format!("{root:#}\n"));
    }

    fn settings_bytes(&self) -> Vec<u8> {
        fs::read(self.settings_path()).unwrap()
    }

    fn settings(&self) -> Value {
        serde_json::from_slice(&self.settings_bytes()).unwrap()
    }

    /// Any `settings.json.bak.*` left beside the settings file.
    fn backups(&self) -> Vec<String> {
        fs::read_dir(self.home.join(".claude"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.json.bak"))
            .collect()
    }

    fn no_calls(&self) {
        assert!(
            !self.home.join("claude-calls.log").exists(),
            "claude must not run"
        );
    }
}

fn source() -> Value {
    json!({ "source": "github", "repo": "amplifiedengineer/marketplace" })
}

const MARKETPLACE: &str = "amplifiedengineer";
const ON_LINE: &str = "plugin: auto-update on for amplifiedengineer";
const ALREADY_ON_LINE: &str = "plugin: auto-update already on for amplifiedengineer";
const USER_OFF_LINE: &str = "plugin: auto-update is off for amplifiedengineer by your choice";
const NOT_BOOLEAN_LINE: &str =
    "plugin: the auto-update setting for amplifiedengineer is not true or false, left as it is";

fn install_allowed() -> SyncOpts {
    SyncOpts {
        allow_install: true,
        force: true,
    }
}

#[test]
fn an_empty_home_adds_the_marketplace_and_installs_when_allowed() {
    // Arrange
    let f = Fixture::new(0);

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(f.calls(), format!("{MARKETPLACE_ADD}{INSTALL}"));
    assert_eq!(
        outcome,
        Outcome::Installed {
            id: PLUGIN_ID.to_string(),
            version: Some(VERSION.to_string()),
        }
    );
}

#[test]
fn a_known_marketplace_is_not_added_again() {
    // Arrange
    let f = Fixture::new(0);
    f.known_marketplace();

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(f.calls(), INSTALL);
    assert!(matches!(outcome, Outcome::Installed { .. }));
}

#[test]
fn an_old_marketplace_name_is_installed_from_as_it_is() {
    // Arrange
    let f = Fixture::new(0);
    f.known_marketplace_named("pragmatic-engineer");

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(
        f.calls(),
        "plugin install playbook@pragmatic-engineer --json\n"
    );
    assert!(
        matches!(&outcome, Outcome::Installed { id, .. } if id == "playbook@pragmatic-engineer"),
        "{outcome:?}"
    );
}

#[test]
fn an_empty_home_is_left_alone_when_install_is_not_allowed() {
    // Arrange
    let f = Fixture::new(0);

    // Act
    let outcome = sync(&f.probe(), SyncOpts::default());

    // Assert
    assert_eq!(outcome, Outcome::NotInstalled);
    f.no_calls();
}

#[test]
fn no_claude_on_path_names_both_install_commands() {
    // Arrange
    let f = Fixture::new(0);

    // Act
    let outcome = sync(&f.without_claude(), install_allowed());

    // Assert
    let line = outcome.line().expect("a line for the user");
    assert!(matches!(outcome, Outcome::NoCli { .. }));
    assert!(line.contains(README_ADD), "{line}");
    assert!(line.contains(README_INSTALL), "{line}");
    f.no_calls();
}

#[test]
fn a_failed_install_names_the_install_command() {
    // Arrange
    let f = Fixture::new(1);
    f.known_marketplace();

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(f.calls(), INSTALL);
    let line = outcome.line().expect("a line for the user");
    assert!(matches!(outcome, Outcome::Failed { .. }));
    assert!(line.contains("install failed: network"), "{line}");
    assert!(line.contains(README_INSTALL), "{line}");
}

#[test]
fn a_second_run_updates_instead_of_installing() {
    // Arrange
    let f = Fixture::new(0);
    sync(&f.probe(), install_allowed());

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(
        f.calls(),
        format!("{MARKETPLACE_ADD}{INSTALL}{MARKETPLACE_UPDATE}{UPDATE}")
    );
    assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
}

/// The binary's line one minor behind, so it is on an older MAJOR.MINOR.
fn older_minor() -> String {
    let mut parts = VERSION.split('.').map(|p| p.parse::<u64>().unwrap());
    let (major, minor) = (parts.next().unwrap(), parts.next().unwrap());
    match minor.checked_sub(1) {
        Some(previous) => format!("{major}.{previous}.0"),
        None => format!("{}.999.0", major - 1),
    }
}

#[test]
fn a_plugin_on_an_older_minor_is_reported_behind() {
    // Arrange
    let older = older_minor();
    let f = Fixture::listing(0, &older);
    f.installed_at(&older);

    // Act
    let outcome = sync(&f.probe(), SyncOpts::default());

    // Assert
    assert_eq!(f.calls(), format!("{MARKETPLACE_UPDATE}{UPDATE}"));
    assert_eq!(
        outcome,
        Outcome::Behind {
            id: PLUGIN_ID.to_string(),
            version: Some(older),
            running: VERSION.to_string(),
            marketplace: None,
        }
    );
}

#[test]
fn an_install_that_lands_on_an_older_minor_is_reported_behind() {
    // Arrange
    let older = older_minor();
    let f = Fixture::listing(0, &older);

    // Act
    let outcome = sync(&f.probe(), install_allowed());

    // Assert
    assert_eq!(f.calls(), format!("{MARKETPLACE_ADD}{INSTALL}"));
    assert_eq!(
        outcome,
        Outcome::Behind {
            id: PLUGIN_ID.to_string(),
            version: Some(older),
            running: VERSION.to_string(),
            marketplace: None,
        }
    );
}

#[test]
fn a_plugin_on_the_binary_line_is_not_behind() {
    // Arrange
    let f = Fixture::listing(0, VERSION);
    f.installed_at(VERSION);

    // Act
    let outcome = sync(&f.probe(), SyncOpts::default());

    // Assert
    assert_eq!(f.calls(), format!("{MARKETPLACE_UPDATE}{UPDATE}"));
    assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
}

#[test]
fn behind_compares_the_major_and_minor_line_only() {
    let cases = [
        ("0.40.2", "0.41.0", true),
        ("0.9.0", "0.10.0", true),
        ("0.41.0", "0.41.3", false),
        ("0.42.0", "0.41.0", false),
        ("1.0.0", "0.41.0", false),
        ("0.41.0", "0.41.0", false),
        ("not a version", "0.41.0", false),
        ("0.41.0", "not a version", false),
        ("", "0.41.0", false),
    ];
    for (installed, running, expected) in cases {
        // Act
        let got = behind(installed, running);

        // Assert
        assert_eq!(got, expected, "{installed} vs {running}");
    }
}

#[test]
fn a_fresh_install_turns_marketplace_auto_update_on() {
    // Arrange
    let f = Fixture::new(0);
    let outcome = sync(&f.probe(), install_allowed());

    // Act
    let marked = auto_update_after(&outcome, &f.home);

    // Assert
    assert_eq!(marked, Some(AutoUpdate::Set));
    assert_eq!(marked.unwrap().line().as_deref(), Some(ON_LINE));
    assert_eq!(
        f.settings(),
        json!({ "extraKnownMarketplaces": { MARKETPLACE: { "source": source(), "autoUpdate": true } } })
    );
}

#[test]
fn auto_update_is_written_beside_source() {
    // Arrange
    let f = Fixture::new(0);
    f.write_entry(MARKETPLACE, None);
    let before = f.settings();
    let before_bytes = f.settings_bytes();

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::Set);
    let backups = f.backups();
    assert_eq!(backups.len(), 1, "a write keeps one backup: {backups:?}");
    let backup = fs::read(f.home.join(".claude").join(&backups[0])).unwrap();
    assert_eq!(backup, before_bytes, "the backup holds the old bytes");
    let after = f.settings();
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&after), keys(&before), "top level key order is kept");
    assert_eq!(
        keys(&after["extraKnownMarketplaces"][MARKETPLACE]),
        ["source", "autoUpdate"],
        "autoUpdate follows source"
    );
    let entry = &after["extraKnownMarketplaces"][MARKETPLACE];
    assert_eq!(entry["source"], source());
    assert_eq!(entry["autoUpdate"], json!(true));
    assert_eq!(entry.as_object().unwrap().len(), 2);
    let mut expected = before;
    expected["extraKnownMarketplaces"][MARKETPLACE]["autoUpdate"] = json!(true);
    assert_eq!(after, expected, "every other key is kept");
}

#[test]
fn an_auto_update_already_on_is_not_rewritten() {
    // Arrange
    let f = Fixture::new(0);
    f.write_entry(MARKETPLACE, Some(json!(true)));
    let bytes = f.settings_bytes();
    // Whole seconds, so a file system that rounds stored times still matches.
    let backdated = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 3600,
        );
    fs::File::options()
        .write(true)
        .open(f.settings_path())
        .unwrap()
        .set_modified(backdated)
        .unwrap();

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::AlreadyOn);
    assert_eq!(marked.line().as_deref(), Some(ALREADY_ON_LINE));
    assert_eq!(f.settings_bytes(), bytes);
    let mtime = fs::metadata(f.settings_path()).unwrap().modified().unwrap();
    assert_eq!(mtime, backdated);
    assert_eq!(f.backups(), Vec::<String>::new());
}

#[test]
fn an_explicit_auto_update_false_is_kept() {
    // Arrange
    let f = Fixture::new(0);
    f.write_entry(MARKETPLACE, Some(json!(false)));
    let bytes = f.settings_bytes();

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::UserOff);
    assert_eq!(marked.line().as_deref(), Some(USER_OFF_LINE));
    assert_eq!(f.settings_bytes(), bytes);
    assert_eq!(f.backups(), Vec::<String>::new());
}

#[test]
fn an_entry_that_is_not_shaped_like_a_marketplace_is_left_as_it_is() {
    let not_registered = |t: &str| (t.to_string(), AutoUpdate::NotRegistered);
    let not_boolean = |v: &str| {
        (
            format!(
                r#"{{"extraKnownMarketplaces":{{"amplifiedengineer":{{"source":{{"source":"github"}},"autoUpdate":{v}}}}}}}"#
            ),
            AutoUpdate::NotBoolean,
        )
    };
    let cases = [
        not_registered(r#"{"extraKnownMarketplaces":"x"}"#),
        not_registered(r#"{"extraKnownMarketplaces":[]}"#),
        not_registered(r#"{"extraKnownMarketplaces":{"amplifiedengineer":"x"}}"#),
        not_registered(r#"{"extraKnownMarketplaces":{"amplifiedengineer":{"autoUpdate":true}}}"#),
        not_boolean(r#""yes""#),
        not_boolean("1"),
        not_boolean("null"),
    ];
    for (text, expected) in cases {
        // Arrange
        let f = Fixture::new(0);
        f.write_settings(&text);

        // Act
        let marked = mark_auto_update(&f.home, MARKETPLACE);

        // Assert
        assert_eq!(marked, expected, "{text}");
        assert_eq!(f.settings_bytes(), text.as_bytes(), "{text}");
        assert_eq!(f.backups(), Vec::<String>::new(), "{text}");
        let line = marked.line();
        match expected {
            AutoUpdate::NotBoolean => assert_eq!(line.as_deref(), Some(NOT_BOOLEAN_LINE)),
            _ => assert_eq!(line, None, "{text}"),
        }
    }
}

#[test]
fn an_old_id_marketplace_is_left_alone() {
    for auto_update in [None, Some(json!(true)), Some(json!(false))] {
        // Arrange
        let f = Fixture::new(0);
        f.write_entry("pragmatic-engineer", auto_update.clone());
        let bytes = f.settings_bytes();
        let installed = Outcome::Installed {
            id: "playbook@pragmatic-engineer".to_string(),
            version: None,
        };

        // Act
        let via_outcome = auto_update_after(&installed, &f.home);
        let direct = mark_auto_update(&f.home, "pragmatic-engineer");

        // Assert
        assert_eq!(via_outcome, None, "{auto_update:?}");
        assert_eq!(direct, AutoUpdate::NotRegistered, "{auto_update:?}");
        assert_eq!(f.settings_bytes(), bytes, "{auto_update:?}");
        assert_eq!(f.backups(), Vec::<String>::new());
    }
}

#[test]
fn an_outcome_that_installed_nothing_does_not_touch_settings() {
    let id = || PLUGIN_ID.to_string();
    let outcomes = [
        Outcome::NoCli {
            id: id(),
            install: true,
        },
        Outcome::Failed {
            id: id(),
            why: "x".into(),
            install: true,
        },
        Outcome::Skipped("x".into()),
        Outcome::NotInstalled,
    ];
    for outcome in outcomes {
        // Arrange
        let f = Fixture::new(0);
        f.write_entry(MARKETPLACE, None);
        let bytes = f.settings_bytes();

        // Act
        let marked = auto_update_after(&outcome, &f.home);

        // Assert
        assert_eq!(marked, None, "{outcome:?}");
        assert_eq!(f.settings_bytes(), bytes, "{outcome:?}");
    }
}

#[test]
fn a_marketplace_without_a_settings_entry_is_not_added() {
    // Arrange
    let f = Fixture::new(0);
    let text = r#"{"theme":"dark"}"#;
    f.write_settings(text);

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::NotRegistered);
    assert_eq!(marked.line(), None);
    assert_eq!(f.settings_bytes(), text.as_bytes());
    assert_eq!(f.backups(), Vec::<String>::new());
}

#[test]
fn a_missing_settings_file_is_not_created() {
    // Arrange
    let f = Fixture::new(0);

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::NotRegistered);
    assert!(!f.settings_path().exists());
}

#[test]
fn an_unreadable_settings_file_is_not_touched() {
    // Arrange
    let f = Fixture::new(0);
    let text = r#"{"extraKnownMarketplaces": {"amplifiedengineer": "#;
    f.write_settings(text);

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert!(matches!(marked, AutoUpdate::Failed(_)), "{marked:?}");
    let line = marked.line().expect("a line for the user");
    assert!(
        line.starts_with("plugin: could not turn on auto-update for amplifiedengineer ("),
        "{line}"
    );
    assert_eq!(f.settings_bytes(), text.as_bytes());
    assert_eq!(f.backups(), Vec::<String>::new());
}

#[test]
fn auto_update_keeps_a_symlinked_settings_file_a_symlink() {
    // Arrange
    let f = Fixture::new(0);
    f.write_entry(MARKETPLACE, None);
    let target = f.home.join("dotfiles-settings.json");
    fs::rename(f.settings_path(), &target).unwrap();
    std::os::unix::fs::symlink(&target, f.settings_path()).unwrap();

    // Act
    let marked = mark_auto_update(&f.home, MARKETPLACE);

    // Assert
    assert_eq!(marked, AutoUpdate::Set);
    assert!(fs::symlink_metadata(f.settings_path())
        .unwrap()
        .file_type()
        .is_symlink());
    let written: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(
        written["extraKnownMarketplaces"][MARKETPLACE]["autoUpdate"],
        json!(true)
    );
}

#[test]
fn an_updated_or_current_install_turns_auto_update_on() {
    // Arrange
    let f = Fixture::listing(0, VERSION);
    f.installed_at(VERSION);
    f.known_marketplace();
    f.write_entry(MARKETPLACE, None);
    let current = sync(&f.probe(), SyncOpts::default());
    assert!(matches!(current, Outcome::Current { .. }), "{current:?}");
    let updated = Outcome::Updated {
        id: PLUGIN_ID.to_string(),
        from: Some(older_minor()),
        to: Some(VERSION.to_string()),
    };
    let behind = Outcome::Behind {
        id: PLUGIN_ID.to_string(),
        version: Some(older_minor()),
        running: VERSION.to_string(),
        marketplace: None,
    };

    // Act
    let after_current = auto_update_after(&current, &f.home);
    let first_state = f.settings();
    f.write_entry(MARKETPLACE, None);
    let after_updated = auto_update_after(&updated, &f.home);
    f.write_entry(MARKETPLACE, None);
    let after_behind = auto_update_after(&behind, &f.home);

    // Assert
    assert_eq!(after_current, Some(AutoUpdate::Set));
    assert_eq!(after_updated, Some(AutoUpdate::Set));
    assert_eq!(after_behind, Some(AutoUpdate::Set));
    assert_eq!(
        first_state["extraKnownMarketplaces"][MARKETPLACE]["autoUpdate"],
        json!(true)
    );
}

#[test]
fn setup_twice_is_idempotent() {
    // Arrange
    let f = Fixture::new(0);
    let first = sync(&f.probe(), install_allowed());
    auto_update_after(&first, &f.home);
    let bytes = f.settings_bytes();

    // Act
    let second = sync(&f.probe(), install_allowed());
    let marked = auto_update_after(&second, &f.home);

    // Assert
    assert_eq!(
        f.calls(),
        format!("{MARKETPLACE_ADD}{INSTALL}{MARKETPLACE_UPDATE}{UPDATE}")
    );
    assert_eq!(marked, Some(AutoUpdate::AlreadyOn));
    assert_eq!(f.settings_bytes(), bytes);
}

#[test]
fn the_upgrade_path_never_writes_auto_update() {
    // Arrange
    let f = Fixture::listing(0, VERSION);
    f.installed_at(VERSION);
    f.known_marketplace();
    f.write_entry(MARKETPLACE, None);
    let bytes = f.settings_bytes();

    // Act
    let outcome = run(&f.probe());

    // Assert
    assert!(matches!(outcome, Outcome::Current { .. }), "{outcome:?}");
    assert_eq!(f.settings_bytes(), bytes);
    assert_eq!(f.backups(), Vec::<String>::new());
}
