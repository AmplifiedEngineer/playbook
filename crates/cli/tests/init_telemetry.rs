// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook setup` and the telemetry part, against a scratch `$HOME`: the
//! default yes, the off switches, a user's own endpoint, `--only`, and
//! uninstall putting every value back.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str, settings: Option<&str>) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "playbook-init-tel-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude")).expect("scratch home");
        if let Some(text) = settings {
            fs::write(home.join(".claude/settings.json"), text).expect("settings");
        }
        Self { home }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        // Nothing from the machine running the tests may look like a setup.
        for (name, _) in std::env::vars() {
            if name == "CLAUDE_CODE_ENABLE_TELEMETRY" || name.starts_with("OTEL_") {
                cmd.env_remove(name);
            }
        }
        cmd.args(args)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .envs(extra_env.iter().copied())
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn")
    }

    fn env(&self) -> serde_json::Map<String, Value> {
        let text = fs::read_to_string(self.home.join(".claude/settings.json")).unwrap_or_default();
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("env").and_then(Value::as_object).cloned())
            .unwrap_or_default()
    }

    /// Every file under the scratch home with its contents, except the
    /// telemetry keys of settings.json, so a change elsewhere shows.
    fn snapshot_without_telemetry(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        walk(&self.home, &self.home, &mut out);
        out
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let mut text = fs::read_to_string(&path).unwrap_or_default();
            if rel == ".claude/settings.json" {
                if let Ok(mut v) = serde_json::from_str::<Value>(&text) {
                    let mut emptied = false;
                    if let Some(env) = v.get_mut("env").and_then(Value::as_object_mut) {
                        env.retain(|k, _| {
                            k != "CLAUDE_CODE_ENABLE_TELEMETRY" && !k.starts_with("OTEL_")
                        });
                        emptied = env.is_empty();
                    }
                    if emptied {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("env");
                        }
                    }
                    text = v.to_string();
                }
            }
            if rel.contains(".bak.")
                || rel.contains("telemetry-env.json")
                || rel.contains("usage/")
                || rel.ends_with("collector.json")
            {
                continue;
            }
            out.insert(rel, text);
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const OFF_TRIO: &str = r#"{"model":"opus","env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","DISABLE_TELEMETRY":"1","DO_NOT_TRACK":"1"}}"#;

#[test]
fn only_telemetry_changes_the_telemetry_keys_and_nothing_else() {
    let sb = Sandbox::new("only", Some(r#"{"model":"opus","env":{"FOO":"1"}}"#));
    fs::write(sb.home.join(".zshrc"), "# mine\n").unwrap();
    let before = sb.snapshot_without_telemetry();

    let out = sb.run(&["setup", "--only", "telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    let env = sb.env();
    assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    assert!(env["OTEL_EXPORTER_OTLP_ENDPOINT"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));
    assert!(env["OTEL_EXPORTER_OTLP_HEADERS"]
        .as_str()
        .unwrap()
        .starts_with("Authorization=Bearer "));
    assert_eq!(sb.snapshot_without_telemetry(), before);
    assert!(text(&out).contains("Restart Claude Code"), "{}", text(&out));
}

#[test]
fn a_bare_telemetry_flag_means_only_telemetry() {
    let sb = Sandbox::new("bare", Some("{}"));
    fs::write(sb.home.join(".zshrc"), "# mine\n").unwrap();
    let before = sb.snapshot_without_telemetry();

    let out = sb.run(&["setup", "--telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    assert_eq!(sb.snapshot_without_telemetry(), before);
}

#[test]
fn an_unknown_part_name_lists_the_valid_names() {
    let sb = Sandbox::new("unknown", None);

    let out = sb.run(&["setup", "--only", "bogus"]);

    assert!(!out.status.success());
    let msg = text(&out);
    assert!(msg.contains("unknown part `bogus`"), "{msg}");
    assert!(msg.contains("telemetry") && msg.contains("hooks"), "{msg}");
}

#[test]
fn only_with_the_no_flag_of_the_same_part_is_a_clear_conflict() {
    let sb = Sandbox::new("conflict", None);

    let out = sb.run(&["setup", "--only", "telemetry", "--no-telemetry"]);

    assert!(!out.status.success());
    assert!(
        text(&out).contains("conflicts with --no-telemetry"),
        "{}",
        text(&out)
    );
}

#[test]
fn only_with_dry_run_shows_just_that_part_and_writes_nothing() {
    let sb = Sandbox::new("dry", Some("{}"));

    let out = sb.run(&["setup", "--only", "telemetry", "--dry-run"]);

    assert!(out.status.success(), "{}", text(&out));
    let msg = text(&out);
    assert!(msg.contains("telemetry (on)"), "{msg}");
    assert!(!msg.contains("hooks ("), "{msg}");
    assert!(sb.env().is_empty());
}

#[test]
fn only_with_explain_describes_the_named_part() {
    let sb = Sandbox::new("explain", None);

    let out = sb.run(&["setup", "--only", "telemetry", "--explain"]);

    assert!(out.status.success(), "{}", text(&out));
    let msg = text(&out);
    assert!(msg.starts_with("telemetry"), "{msg}");
    assert!(!msg.contains("\nhooks\n"), "{msg}");
}

#[test]
fn the_three_off_switches_get_one_switch_flipped_and_two_left_alone() {
    let sb = Sandbox::new("trio", Some(OFF_TRIO));

    let out = sb.run(&["setup", "--only", "telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    let env = sb.env();
    assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    assert_eq!(env["DISABLE_TELEMETRY"], "1");
    assert_eq!(env["DO_NOT_TRACK"], "1");
    let msg = text(&out);
    assert!(msg.contains("CLAUDE_CODE_ENABLE_TELEMETRY 0 -> 1"), "{msg}");
    assert!(msg.contains("DISABLE_TELEMETRY=1"), "{msg}");
}

#[test]
fn yes_alone_does_not_replace_an_off_switch_the_user_set() {
    let sb = Sandbox::new("yes-off", Some(OFF_TRIO));

    let out = sb.run(&["setup", "--yes", "--no-hooks", "--no-statusline"]);

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    assert!(
        text(&out).contains("playbook setup --only telemetry"),
        "{}",
        text(&out)
    );
}

#[test]
fn yes_counts_as_consent_when_nothing_is_set() {
    let sb = Sandbox::new("yes", Some("{}"));

    let out = sb.run(&["setup", "--yes"]);

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
}

#[test]
fn no_terminal_and_no_yes_writes_nothing_and_prints_the_command() {
    let sb = Sandbox::new("unattended", Some("{}"));

    let out = sb.run(&["setup"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.env().is_empty());
    assert!(
        text(&out).contains("playbook setup --only telemetry"),
        "{}",
        text(&out)
    );
}

#[test]
fn declining_with_the_no_flag_writes_nothing() {
    let sb = Sandbox::new("decline", Some("{}"));

    let out = sb.run(&["setup", "--yes", "--no-telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.env().is_empty());
}

#[test]
fn an_enabled_setup_with_the_users_own_endpoint_is_left_alone() {
    let own = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_EXPORTER_OTLP_ENDPOINT":"https://mine.example"}}"#;
    let sb = Sandbox::new("own", Some(own));

    let out = sb.run(&["setup", "--only", "telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        sb.env()["OTEL_EXPORTER_OTLP_ENDPOINT"],
        "https://mine.example"
    );
    assert_eq!(
        fs::read_to_string(sb.home.join(".claude/settings.json")).unwrap(),
        own
    );
}

#[test]
fn uninstall_puts_back_the_three_off_values() {
    let sb = Sandbox::new("restore", Some(OFF_TRIO));
    sb.run(&["setup", "--only", "telemetry"]);

    let out = sb.run(&["uninstall", "--yes"]);

    assert!(out.status.success(), "{}", text(&out));
    let env = sb.env();
    assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    assert_eq!(env["DISABLE_TELEMETRY"], "1");
    assert_eq!(env["DO_NOT_TRACK"], "1");
    assert!(!env.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"));
}

#[test]
fn a_copy_of_the_settings_env_in_the_process_environment_is_not_a_second_setup() {
    let sb = Sandbox::new("mirrored", Some(OFF_TRIO));

    let out = sb.run_with(
        &["setup", "--only", "telemetry"],
        &[
            ("CLAUDE_CODE_ENABLE_TELEMETRY", "0"),
            ("DISABLE_TELEMETRY", "1"),
        ],
    );

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
}

#[test]
fn an_endpoint_in_the_process_environment_that_settings_do_not_name_is_left_alone() {
    let sb = Sandbox::new("process-own", Some(OFF_TRIO));

    let out = sb.run_with(
        &["setup", "--only", "telemetry"],
        &[("OTEL_EXPORTER_OTLP_ENDPOINT", "https://mine.example")],
    );

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
}

#[test]
fn yes_with_an_explicit_telemetry_flag_replaces_the_off_switch() {
    let sb = Sandbox::new("yes-flag", Some(OFF_TRIO));

    let out = sb.run(&[
        "setup",
        "--yes",
        "--telemetry",
        "--no-hooks",
        "--no-statusline",
    ]);

    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sb.env()["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
}

#[test]
fn a_dry_run_over_an_off_switch_changes_no_file() {
    let sb = Sandbox::new("dry-off", Some(OFF_TRIO));
    let before = fs::read_to_string(sb.home.join(".claude/settings.json")).unwrap();

    let out = sb.run(&["setup", "--only", "telemetry", "--dry-run"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("Runs every time"), "{}", text(&out));
    let after = fs::read_to_string(sb.home.join(".claude/settings.json")).unwrap();
    assert_eq!(after, before);
    assert!(!sb.home.join(".config/playbook/telemetry-env.json").exists());
}

#[test]
fn a_part_that_is_not_a_choice_is_refused_by_only() {
    let sb = Sandbox::new("only-trust", None);

    let out = sb.run(&["setup", "--only", "trust"]);

    assert!(!out.status.success());
    assert!(
        text(&out).contains("unknown part `trust`"),
        "{}",
        text(&out)
    );
}

#[cfg(unix)]
#[test]
fn a_settings_file_created_by_init_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new("mode", None);
    fs::remove_dir_all(sb.home.join(".claude")).unwrap();

    let out = sb.run(&["setup", "--only", "telemetry"]);

    assert!(out.status.success(), "{}", text(&out));
    let mode = fs::metadata(sb.home.join(".claude/settings.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
