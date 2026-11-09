// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The hooks live in `~/.claude/settings.json`, not in a plugin. These tests
//! run the real binary against a scratch `$HOME` and pin the whole life of
//! those entries: `init` writes them, entries a user added survive both `init`
//! and `uninstall`, and `uninstall` removes only the entries `init` owns.

use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Home {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-hooks-in-settings-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".claude")).expect("scratch home");
        Home { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", &self.dir)
            .env("SHELL", "/bin/zsh")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .current_dir(&self.dir)
            .output()
            .expect("playbook should spawn");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join(".claude/settings.json")
    }

    fn write_settings(&self, value: &Value) {
        fs::write(self.settings_path(), value.to_string()).unwrap();
    }

    fn settings(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.settings_path()).unwrap()).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Every command string under `.hooks`, with its event.
fn commands(settings: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (event, groups) in settings["hooks"].as_object().into_iter().flatten() {
        for group in groups.as_array().into_iter().flatten() {
            for hook in group["hooks"].as_array().into_iter().flatten() {
                if let Some(cmd) = hook["command"].as_str() {
                    out.push((event.clone(), cmd.to_string()));
                }
            }
        }
    }
    out
}

fn has(settings: &Value, event: &str, cmd: &str) -> bool {
    commands(settings)
        .iter()
        .any(|(e, c)| e == event && c == cmd)
}

fn user_hooks() -> Value {
    json!({
        "theme": "dark",
        "hooks": {
            "WorktreeCreate": [{"hooks": [{"type": "command", "command": "echo my-worktree-hook"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "echo my-stop-hook"}]}],
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": "echo my-bash-hook"}]
            }]
        }
    })
}

#[test]
fn init_writes_every_hook_including_the_worktree_and_session_ones() {
    let h = Home::new("writes");

    h.run(&["setup", "--hooks"]);

    let s = h.settings();
    for hook in playbook::init::wire::wired_hooks() {
        assert!(
            has(&s, hook.event, &format!("playbook hook {}", hook.name)),
            "{} missing under {}",
            hook.name,
            hook.event
        );
    }
    assert!(has(&s, "WorktreeCreate", "playbook hook worktree-create"));
    assert!(has(&s, "WorktreeRemove", "playbook hook worktree-remove"));
    assert!(has(&s, "SessionStart", "playbook hook session-init"));
}

#[test]
fn the_hook_commands_are_the_bare_name_never_an_absolute_path() {
    let h = Home::new("bare");

    h.run(&["setup", "--hooks"]);

    let ours: Vec<String> = commands(&h.settings())
        .into_iter()
        .map(|(_, cmd)| cmd)
        .filter(|cmd| cmd.contains("playbook hook "))
        .collect();
    assert!(!ours.is_empty(), "init wrote no playbook hook entries");
    for cmd in ours {
        assert!(cmd.starts_with("playbook hook "), "{cmd}");
        assert!(!cmd.contains('/'), "{cmd}");
    }
}

#[test]
fn init_keeps_the_hooks_and_keys_a_user_already_has() {
    let h = Home::new("preserve");
    h.write_settings(&user_hooks());

    h.run(&["setup", "--hooks"]);

    let s = h.settings();
    assert_eq!(s["theme"], "dark");
    assert!(has(&s, "WorktreeCreate", "echo my-worktree-hook"));
    assert!(has(&s, "Stop", "echo my-stop-hook"));
    assert!(has(&s, "PreToolUse", "echo my-bash-hook"));
    assert!(has(&s, "WorktreeCreate", "playbook hook worktree-create"));
}

#[test]
fn uninstall_removes_only_the_entries_init_owns() {
    let h = Home::new("uninstall");
    h.write_settings(&user_hooks());
    h.run(&["setup", "--hooks"]);

    h.run(&["uninstall", "--yes"]);

    let s = h.settings();
    assert!(
        !commands(&s)
            .iter()
            .any(|(_, c)| c.starts_with("playbook hook ")),
        "{s}"
    );
    assert!(has(&s, "WorktreeCreate", "echo my-worktree-hook"));
    assert!(has(&s, "Stop", "echo my-stop-hook"));
    assert!(has(&s, "PreToolUse", "echo my-bash-hook"));
    assert_eq!(s["theme"], "dark");
}

#[test]
fn the_doctor_reports_the_wired_hooks_as_present() {
    let h = Home::new("doctor");
    h.run(&["setup", "--hooks"]);

    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["doctor", "check"])
        .env("HOME", &h.dir)
        .env("SHELL", "/bin/sh")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .current_dir(&h.dir)
        .output()
        .expect("playbook should spawn");

    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!(
            "PASS  playbook hook entries in settings.json ({n} of {n})",
            n = playbook::init::wire::wired_hooks().count()
        )),
        "{text}"
    );
}
