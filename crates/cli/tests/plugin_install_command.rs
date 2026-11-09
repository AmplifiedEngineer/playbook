// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plugin install` runs against a fake `claude` on PATH that logs
//! its calls and, like the real one, registers the marketplace and lists the
//! plugin. The isolated home sets `PLAYBOOK_SKIP_PLUGIN_UPDATE`, so every test
//! here also proves the command ignores that opt-out.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ID: &str = "playbook@amplifiedengineer";
const MARKETPLACE_ADD: &str = "plugin marketplace add amplifiedengineer/marketplace --json";
const INSTALL: &str = "plugin install playbook@amplifiedengineer --json";
const NEXT_STEP: &str = "claude plugin marketplace add amplifiedengineer/marketplace && claude plugin install playbook@amplifiedengineer";

struct Fixture {
    home: PathBuf,
}

impl Fixture {
    /// A home whose fake `claude` registers the marketplace in
    /// `settings.json` and `known_marketplaces.json`, and lists the plugin
    /// when `install_exit` is 0.
    fn new(install_exit: i32) -> Fixture {
        let home = isolated::fresh_home();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let dir = home
            .join(".claude/plugins/cache/amplifiedengineer/playbook")
            .join(VERSION);
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
  printf '{{"name":"playbook","version":"{VERSION}"}}' > "{dir}/.claude-plugin/plugin.json"
  printf '{{"version":2,"plugins":{{"{ID}":[{{"scope":"user","installPath":"{dir}"}}]}}}}' > "$HOME/.claude/plugins/installed_plugins.json"
  ;;
esac
"#,
            dir = dir.display()
        );
        let staged = bin.join("claude.tmp");
        fs::write(&staged, script).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&staged, bin.join("claude")).unwrap();
        Fixture { home }
    }

    fn run(&self, args: &[&str], with_claude: bool) -> Output {
        let mut cmd: Command = isolated::playbook_in(&self.home);
        let path = if with_claude {
            format!("{}:/usr/bin:/bin", self.home.join("bin").display())
        } else {
            "/usr/bin:/bin".to_string()
        };
        cmd.args(["plugin", "install"])
            .args(args)
            .current_dir(&self.home)
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.home.join("claude-calls.log")).unwrap_or_default()
    }

    fn settings(&self) -> Value {
        let text = fs::read_to_string(self.home.join(".claude/settings.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_fresh_machine_gets_the_marketplace_the_auto_update_flag_and_the_plugin() {
    let f = Fixture::new(0);
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert_eq!(f.calls(), format!("{MARKETPLACE_ADD}\n{INSTALL}\n"));
    for line in [
        "plugin: added the amplifiedengineer marketplace",
        &format!("plugin: installed {ID} {VERSION}"),
        "plugin: auto-update on for amplifiedengineer",
        "auto-update for the marketplace covers the plugin",
        "Restart Claude Code or run /reload-plugins",
    ] {
        assert!(text.contains(line), "missing {line:?} in {text}");
    }
    assert_eq!(
        f.settings()["extraKnownMarketplaces"]["amplifiedengineer"]["autoUpdate"],
        true
    );
}

#[test]
fn a_second_run_reports_every_step_as_done_and_calls_nothing() {
    let f = Fixture::new(0);
    assert!(f.run(&[], true).status.success());
    let before = f.calls();
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert_eq!(f.calls(), before, "the second run must not call claude");
    for line in [
        "plugin: the amplifiedengineer marketplace is already registered",
        &format!("plugin: {ID} is already installed ({VERSION})"),
        "plugin: auto-update already on for amplifiedengineer",
    ] {
        assert!(text.contains(line), "missing {line:?} in {text}");
    }
    assert!(!text.contains("/reload-plugins"), "{text}");
}

#[test]
fn a_marketplace_under_the_old_name_is_left_alone_and_named() {
    let f = Fixture::new(0);
    fs::create_dir_all(f.home.join(".claude/plugins")).unwrap();
    fs::write(
        f.home.join(".claude/plugins/known_marketplaces.json"),
        r#"{"pragmatic-engineer":{}}"#,
    )
    .unwrap();
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert!(!f.calls().contains("marketplace add"), "{}", f.calls());
    assert!(
        text.contains("registered under its old name pragmatic-engineer; left alone"),
        "{text}"
    );
}

#[test]
fn an_explicit_auto_update_false_stays() {
    let f = Fixture::new(0);
    fs::create_dir_all(f.home.join(".claude")).unwrap();
    // The fake rewrites settings.json on add, so the choice lands after the
    // marketplace is registered by an earlier run.
    assert!(f.run(&[], true).status.success());
    let mut settings = f.settings();
    settings["extraKnownMarketplaces"]["amplifiedengineer"]["autoUpdate"] = Value::Bool(false);
    fs::write(f.home.join(".claude/settings.json"), settings.to_string()).unwrap();
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert!(
        text.contains("plugin: auto-update is off for amplifiedengineer by your choice"),
        "{text}"
    );
    assert_eq!(
        f.settings()["extraKnownMarketplaces"]["amplifiedengineer"]["autoUpdate"],
        false
    );
}

#[test]
fn no_claude_on_path_exits_one_and_names_the_next_step() {
    let f = Fixture::new(0);
    let o = f.run(&[], false);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(1), "{text}");
    assert!(text.contains("the claude command is not on PATH"), "{text}");
    assert!(text.contains("install Claude Code"), "{text}");
    assert!(text.contains(NEXT_STEP), "{text}");
    assert_eq!(f.calls(), "");
}

#[test]
fn a_failed_install_exits_one_with_the_command() {
    let f = Fixture::new(1);
    let o = f.run(&[], true);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(1), "{text}");
    assert!(text.contains("plugin: the install failed"), "{text}");
    assert!(text.contains(NEXT_STEP), "{text}");
}

#[test]
fn json_wraps_one_entry_per_step_in_the_envelope_and_exits_zero_on_failure() {
    let f = Fixture::new(0);
    let o = f.run(&["--json"], true);
    assert!(o.status.success());
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["code"], 0);
    assert_eq!(v["error"], "");
    assert_eq!(v["reload"], true);
    let steps: Vec<(&str, &str)> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["step"].as_str().unwrap(), s["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        steps,
        [
            ("marketplace", "added"),
            ("plugin", "installed"),
            ("auto-update", "set")
        ]
    );

    let v: Value = serde_json::from_str(&out(&f.run(&["--json"], true))).unwrap();
    let statuses: Vec<&str> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["present", "present", "already-on"]);

    let failing = Fixture::new(0);
    let o = failing.run(&["--json"], false);
    assert!(o.status.success(), "json exits 0 whatever the result");
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 1);
    assert_eq!(v["steps"][1]["status"], "no-claude");
}

#[test]
fn a_plugin_installed_under_the_old_name_keeps_its_marketplace_and_auto_update_choice() {
    let f = Fixture::new(0);
    let plugins = f.home.join(".claude/plugins");
    fs::create_dir_all(&plugins).unwrap();
    fs::write(
        plugins.join("known_marketplaces.json"),
        r#"{"pragmatic-engineer":{}}"#,
    )
    .unwrap();
    fs::write(
        plugins.join("installed_plugins.json"),
        r#"{"version":2,"plugins":{"playbook@pragmatic-engineer":[{"scope":"user"}]}}"#,
    )
    .unwrap();
    let o = f.run(&[], true);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    assert_eq!(f.calls(), "", "nothing to add or install");
    assert!(
        text.contains("playbook@pragmatic-engineer is already installed"),
        "{text}"
    );
    assert!(
        text.contains("the marketplace is registered as pragmatic-engineer, not amplifiedengineer"),
        "{text}"
    );
}
