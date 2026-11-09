// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook upgrade` wiring through the real binary. Nothing here touches the
//! network.

#![cfg(unix)]

#[path = "support/isolated.rs"]
mod isolated;

use std::process::Command;

fn playbook() -> Command {
    isolated::playbook()
}

#[test]
fn upgrade_and_its_hidden_update_alias_share_one_help() {
    for name in ["upgrade", "update"] {
        let out = playbook().args([name, "--help"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{name} --help failed");
        for flag in [
            "--check",
            "--list",
            "--pre",
            "--yes",
            "--json",
            "--no-plugin",
        ] {
            assert!(text.contains(flag), "{name} help lacks {flag}: {text}");
        }
    }
}

#[test]
fn the_root_help_lists_upgrade_and_hides_update() {
    let out = playbook().arg("--help").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines().any(|l| l.trim_start().starts_with("upgrade ")),
        "{text}"
    );
    assert!(
        !text.lines().any(|l| l.trim_start().starts_with("update ")),
        "{text}"
    );
}

#[test]
fn json_reports_a_refusal_in_the_envelope_for_both_names_without_a_warning_line() {
    for name in ["upgrade", "update"] {
        // Arrange
        let home = std::env::temp_dir().join(format!(
            "playbook-upgrade-json-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(home.join(".config/playbook/update.lock")).unwrap();

        // Act
        let out = playbook()
            .args([name, "--yes", "--json"])
            .env("HOME", &home)
            .output()
            .unwrap();

        // Assert
        assert!(out.status.success(), "{name}: json exits 0");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["code"], 1, "{v}");
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("another playbook process is updating"),
            "{v}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("alias") && !stderr.contains("deprecated"),
            "{stderr}"
        );
    }
}

/// A home whose cached release check is stale and names a newer release.
fn home_with_a_stale_newer_release(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "playbook-update-check-{tag}-{}",
        std::process::id()
    ));
    let root = home.join(".config/playbook");
    std::fs::create_dir_all(&root).unwrap();
    playbook::state::set(
        &root,
        playbook::update::check::CACHE_KEY,
        r#"{"checked_at":0,"latest":"999.0.0"}"#,
    )
    .unwrap();
    home
}

fn cache(home: &std::path::Path) -> String {
    playbook::state::get(
        &home.join(".config/playbook"),
        playbook::update::check::CACHE_KEY,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn a_piped_json_run_keeps_its_stdout_and_leaves_the_check_alone() {
    // Arrange
    let home = home_with_a_stale_newer_release("json");
    let before = cache(&home);

    // Act
    let out = playbook()
        .args(["mode", "status", "--json"])
        .env("HOME", &home)
        .output()
        .unwrap();

    // Assert
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "{stdout}"
    );
    assert!(!stderr.contains("is available"), "{stderr}");
    assert_eq!(cache(&home), before, "a piped run must not claim a refresh");
}

#[test]
fn a_held_update_lock_refuses_a_manual_update() {
    // Arrange
    let home = std::env::temp_dir().join(format!("playbook-update-lock-{}", std::process::id()));
    std::fs::create_dir_all(home.join(".config/playbook/update.lock")).unwrap();

    // Act
    let out = playbook()
        .args(["update", "--yes"])
        .env("HOME", &home)
        .output()
        .unwrap();

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("another playbook process is updating"),
        "{stderr}"
    );
}
