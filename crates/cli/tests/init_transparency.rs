// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook setup --dry-run`, `--explain`, `playbook hooks list` and the
//! summary after a run, all against a scratch `$HOME`.

use playbook::init::wire::wired_hooks;
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
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "playbook-init-transparency-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude")).expect("scratch home");
        fs::write(home.join(".claude/settings.json"), "{\"theme\":\"dark\"}\n").unwrap();
        fs::write(home.join(".zshrc"), "alias a=b\n").unwrap();
        Self { home }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    /// Like `run`, with extra environment. Telemetry variables from the
    /// machine running the tests are always removed first.
    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.env_remove("CLAUDE_CODE_ENABLE_TELEMETRY")
            .env_remove("OTEL_LOGS_EXPORTER")
            .env_remove("OTEL_METRICS_EXPORTER")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
            .envs(env.iter().copied());
        cmd.args(args)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn")
    }

    /// Every file under the scratch home with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.home, &mut out);
        out
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_dry_run_changes_nothing_and_exits_zero() {
    let sb = Sandbox::new("dry");
    let before = sb.snapshot();

    let out = sb.run(&["setup", "--dry-run", "--short-name", "--system-prompt"]);

    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(sb.snapshot(), before, "a dry run wrote a file");
}

#[test]
fn a_dry_run_with_the_policy_guard_names_the_key_and_stores_nothing() {
    let sb = Sandbox::new("dry-policy-guard");
    let before = sb.snapshot();

    let out = sb.run(&["setup", "--dry-run", "--policy-guard"]);

    let text = stdout(&out);
    assert!(out.status.success(), "{text}");
    for needle in ["policy-guard (on)", "hooks.policyGuard.enabled"] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
    assert_eq!(sb.snapshot(), before, "a dry run wrote a file");
}

#[test]
fn a_dry_run_names_the_files_and_the_changes_for_each_part() {
    let sb = Sandbox::new("dry-text");

    let text = stdout(&sb.run(&["setup", "--dry-run", "--yes", "--system-prompt"]));

    let hooks = format!("add {} hook entries", wired_hooks().count());
    for needle in [
        "hooks (on)",
        hooks.as_str(),
        "~/.claude/settings.json: add statusLine",
        "~/.zshenv: add `# playbook binary`",
        "~/.config/playbook/prompts/SYSTEM_PROMPT.md: add the file",
        "security (off)",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
}

#[test]
fn a_dry_run_reports_a_part_you_turned_off_as_skipped() {
    let sb = Sandbox::new("dry-off");

    let text = stdout(&sb.run(&["setup", "--dry-run", "--no-hooks"]));

    assert!(text.contains("hooks (off)\n  skipped"), "{text}");
}

#[test]
fn a_dry_run_describes_what_a_real_run_then_does() {
    let sb = Sandbox::new("dry-vs-real");
    let plan = stdout(&sb.run(&["setup", "--dry-run", "--yes"]));

    let real = sb.run(&["setup", "--yes"]);

    assert!(real.status.success());
    let settings = fs::read_to_string(sb.home.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("\"statusLine\"") && plan.contains("add statusLine"));
    assert!(!fs::read_to_string(sb.home.join(".zshrc"))
        .unwrap_or_default()
        .contains("playbook shell-init"));
    let again = stdout(&sb.run(&["setup", "--dry-run", "--yes"]));
    assert!(!again.contains("add statusLine"), "{again}");
    assert!(
        again.contains("no change") || again.contains("already"),
        "{again}"
    );
}

#[test]
fn explain_with_a_part_describes_it_and_changes_nothing() {
    let sb = Sandbox::new("explain-one");
    let before = sb.snapshot();

    let out = sb.run(&["setup", "--explain", "path"]);

    assert!(out.status.success());
    let text = stdout(&out);
    assert!(
        text.contains("What it edits:") && text.contains("~/.zshenv"),
        "{text}"
    );
    assert!(text.contains("How to undo:"), "{text}");
    assert_eq!(sb.snapshot(), before);
}

#[test]
fn explain_without_a_part_describes_every_part() {
    let sb = Sandbox::new("explain-all");

    let text = stdout(&sb.run(&["setup", "--explain"]));

    for part in [
        "hooks",
        "statusline",
        "security",
        "path",
        "system-prompt",
        "short-name",
        "completions",
        "trust",
    ] {
        assert!(
            text.contains(&format!("{part}\n  What it does")),
            "{part} missing:\n{text}"
        );
    }
}

#[test]
fn explain_hooks_lists_every_hook_init_wires() {
    let sb = Sandbox::new("explain-hooks");

    let text = stdout(&sb.run(&["setup", "--explain", "hooks"]));

    for h in wired_hooks() {
        assert!(
            text.contains(h.name) && text.contains(h.purpose),
            "{}",
            h.name
        );
    }
}

#[test]
fn explain_of_an_unknown_part_fails_and_lists_the_parts() {
    let sb = Sandbox::new("explain-bad");

    let out = sb.run(&["setup", "--explain", "nope"]);

    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Choose one of"));
}

#[test]
fn dry_run_and_explain_cannot_be_combined() {
    let sb = Sandbox::new("conflict");

    let out = sb.run(&["setup", "--dry-run", "--explain"]);

    assert!(!out.status.success());
}

#[test]
fn hooks_list_prints_every_hook_with_event_matcher_and_purpose() {
    let sb = Sandbox::new("list");

    let out = sb.run(&["hooks", "list"]);

    assert!(out.status.success());
    let text = stdout(&out);
    for h in wired_hooks() {
        let matcher = h.matcher.unwrap_or("any");
        let line = format!("{} [{}] {}", h.event, matcher, h.name);
        assert!(
            text.lines()
                .any(|l| l.contains(&line) && l.contains(h.purpose)),
            "no line for {line}"
        );
    }
}

#[test]
fn a_real_run_ends_with_how_to_undo_each_changed_part() {
    let sb = Sandbox::new("summary");

    let text = stdout(&sb.run(&["setup", "--yes"]));

    assert!(text.contains("What changed and how to undo it:"), "{text}");
    assert!(
        text.contains("hooks: edited ~/.claude/settings.json"),
        "{text}"
    );
    assert!(text.contains("playbook uninstall"), "{text}");
    assert!(text.contains("settings.json.bak"), "{text}");
}

#[test]
fn a_second_run_prints_no_undo_summary() {
    let sb = Sandbox::new("summary-twice");
    sb.run(&["setup", "--yes"]);

    let text = stdout(&sb.run(&["setup", "--yes"]));

    assert!(!text.contains("What changed and how to undo it:"), "{text}");
}

#[test]
fn init_says_telemetry_is_already_on_and_does_not_ask() {
    let sandbox = Sandbox::new("telemetry-on");

    let out = sandbox.run_with_env(
        &["setup", "--dry-run"],
        &[("CLAUDE_CODE_ENABLE_TELEMETRY", "1")],
    );

    assert!(
        stdout(&out).contains("telemetry: already enabled"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn init_stays_quiet_about_telemetry_when_it_is_not_set() {
    let sandbox = Sandbox::new("telemetry-unset");

    let out = sandbox.run(&["setup", "--dry-run"]);

    assert!(
        !stdout(&out).contains("telemetry: already"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn an_explicit_telemetry_flag_still_wins_over_the_already_on_line() {
    let sandbox = Sandbox::new("telemetry-flag");

    let out = sandbox.run_with_env(
        &["setup", "--dry-run", "--no-telemetry"],
        &[("CLAUDE_CODE_ENABLE_TELEMETRY", "1")],
    );

    assert!(
        !stdout(&out).contains("telemetry: already"),
        "{}",
        stdout(&out)
    );
}
