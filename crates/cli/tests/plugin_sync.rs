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

use playbook::plugin_update::{behind, sync, Outcome, Probe, SyncOpts};
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

    fn no_calls(&self) {
        assert!(
            !self.home.join("claude-calls.log").exists(),
            "claude must not run"
        );
    }
}

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
