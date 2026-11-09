// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook path plugin-root` run as a subprocess against a scratch `HOME`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-path-plugin-root-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_installed_plugin_is_the_only_stdout_line_and_a_stale_override_is_noted() {
    // Arrange
    let home = scratch("found");
    let plugin = home.join(".claude/plugins/cache/amplifiedengineer/playbook/0.31.0");
    fs::create_dir_all(plugin.join("skills")).unwrap();
    fs::write(
        home.join(".claude/plugins/installed_plugins.json"),
        format!(
            r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
            plugin.display()
        ),
    )
    .unwrap();
    let stale = home.join("gone");

    // Act
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["path", "plugin-root"])
        .current_dir(&home)
        .env("HOME", &home)
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env("PLAYBOOK_PLUGIN_DIR", &stale)
        .output()
        .unwrap();

    // Assert
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", plugin.display())
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("skipped PLAYBOOK_PLUGIN_DIR"), "{stderr}");
}
