// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook setup` as `/playbook:setup` runs it, against a scratch `$HOME`:
//! the golden settings a fresh and a contested install must produce, what a
//! plain run leaves alone, and what `--system-prompt` adds.

use serde_json::Value;
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
            "playbook-init-cli-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude")).expect("scratch home");
        Self { home }
    }

    /// Runs `init --yes`: without a terminal, a run with no --yes leaves the
    /// Claude Code settings alone.
    fn init(&self, shell: &str, flags: &[&str]) -> Output {
        let mut all = vec!["--yes", "--no-telemetry", "--no-completions"];
        all.extend_from_slice(flags);
        self.init_raw(shell, &all)
    }

    fn init_raw(&self, shell: &str, flags: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .arg("setup")
            .args(flags)
            .env("HOME", &self.home)
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .env("SHELL", shell)
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn");
        assert!(out.status.success(), "init failed: {}", text(&out));
        out
    }

    fn config_set(&self, key: &str, value: &str) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["config", "set", "--global", key, value])
            .env("HOME", &self.home)
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn");
        assert!(out.status.success(), "config set failed: {}", text(&out));
    }

    fn config_get(&self, key: &str) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["config", "get", key])
            .env("HOME", &self.home)
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
        fs::write(path, content).expect("scratch file");
    }

    fn count(&self, dir: &str, prefix: &str) -> usize {
        fs::read_dir(self.path(dir))
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                    .count()
            })
            .unwrap_or(0)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `(settings_json, settings_base_json)` from a golden fixture, as raw strings.
fn golden(name: &str) -> (String, String) {
    let path = Path::new(env!("PLAYBOOK_REPO_ROOT"))
        .join("crates/cli/tests/fixtures/golden")
        .join(name);
    let doc: Value = serde_json::from_str(&fs::read_to_string(path).expect("fixture")).unwrap();
    let field = |key: &str| doc[key].as_str().expect("fixture string field").to_string();
    (field("settings_json"), field("settings_base_json"))
}

fn json(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid JSON")
}

#[test]
fn a_clean_install_matches_the_golden_settings() {
    let sb = Sandbox::new("golden-clean");
    let (settings, base) = golden("init.clean-install.json");
    // Attribution ON writes nothing to a file with no attribution key.
    sb.init("/bin/bash", &["--attribution"]);
    assert_eq!(json(&sb.read(".claude/settings.json")), json(&settings));
    assert_eq!(json(&sb.read(".claude/.settings.base.json")), json(&base));
}

#[test]
fn a_contested_key_matches_the_golden_bytes_and_leaves_a_skip_report() {
    let sb = Sandbox::new("golden-skip");
    let (settings, base) = golden("init.skip-triggering.json");
    sb.write(
        ".claude/settings.json",
        "{\"statusLine\":{\"type\":\"command\",\"command\":\"my-status\"}}\n",
    );
    // Attribution ON writes nothing to a file with no attribution key.
    sb.init("/bin/bash", &["--attribution"]);
    assert_eq!(sb.read(".claude/settings.json"), settings);
    assert_eq!(sb.read(".claude/.settings.base.json"), base);
    assert!(sb.count(".claude", "settings-merge-skipped.") >= 1);
}

#[test]
fn a_yes_run_places_settings_guards_and_the_prompt_and_touches_no_rc_file() {
    let sb = Sandbox::new("plain");
    sb.init("/bin/zsh", &[]);
    let settings = json(&sb.read(".claude/settings.json"));
    let hooks = settings["hooks"].to_string();
    for guard in ["rm-workspace-guard", "bg-await-guard"] {
        assert!(
            hooks.contains(&format!("playbook hook {guard}")),
            "{guard} unwired"
        );
        assert!(!sb.path(&format!(".claude/hooks/{guard}.sh")).exists());
    }
    assert!(
        !hooks.contains("no-slop-guard"),
        "the retired guard is wired"
    );
    assert!(sb.path(".claude/.settings.base.json").is_file());
    assert!(!sb.path(".config/playbook/statusline.sh").exists());
    assert!(!sb.path(".zshrc").exists());
    assert!(!sb.path(".bashrc").exists());
    assert!(!sb.path(".claude/shell").exists());
    assert!(!sb.path(".config/playbook/shell").exists());
    assert!(sb
        .path(".config/playbook/prompts/SYSTEM_PROMPT.md")
        .is_file());
}

#[test]
fn the_retired_launcher_flags_are_accepted_and_write_no_rc_file() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        let sb = Sandbox::new("retired-aliases");
        let out = sb.init(
            shell,
            &["--aliases", "--claude-override", "--only", "hooks,aliases"],
        );
        assert!(out.status.success(), "{}", text(&out));
        assert!(!sb.path(".bashrc").exists());
        assert!(!sb.path(".zshrc").exists());
        assert!(sb.read(".claude/settings.json").contains("playbook hook"));
    }
}

#[test]
fn migrate_lists_the_old_rc_block_and_setup_removes_it_after_a_backup() {
    // Arrange
    let sb = Sandbox::new("rc-block");
    let before = "export EDITOR=vim\n\n# playbook launchers (ccc/ccd)\ncommand -v playbook >/dev/null 2>&1 && eval \"$(playbook shell-init)\"\n";
    sb.write(".zshrc", before);

    // Act
    let migrate = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("migrate")
        .env("HOME", &sb.home)
        .current_dir(&sb.home)
        .output()
        .unwrap();
    let listed = text(&migrate);
    let untouched = sb.read(".zshrc");
    sb.init("/bin/zsh", &[]);

    // Assert
    assert!(
        listed.contains(".zshrc still loads the retired"),
        "{listed}"
    );
    assert!(listed.contains("    Fix: playbook setup\n"), "{listed}");
    assert_eq!(untouched, before);
    assert_eq!(sb.read(".zshrc"), "export EDITOR=vim\n");
    assert_eq!(sb.count("", ".zshrc.bak-"), 1);
}

#[test]
fn an_only_run_leaves_the_old_rc_block_alone() {
    let sb = Sandbox::new("rc-block-only");
    let before = "# playbook launchers (ccc/ccd)\ncommand -v playbook >/dev/null 2>&1 && eval \"$(playbook shell-init)\"\n";
    sb.write(".zshrc", before);

    sb.init("/bin/zsh", &["--only", "hooks"]);

    assert_eq!(sb.read(".zshrc"), before);
    assert_eq!(sb.count("", ".zshrc.bak-"), 0);
}

#[test]
fn yes_turns_every_part_on_in_a_scratch_home() {
    // Arrange: a copy of the binary in the install folder, so the short-name
    // part can link `pb` next to it.
    let sb = Sandbox::new("yes-all");
    let bin = sb.path(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("playbook");
    fs::copy(env!("CARGO_BIN_EXE_playbook"), &exe).unwrap();

    // Act
    let out = Command::new(&exe)
        .args(["setup", "--yes"])
        .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
        .env("HOME", &sb.home)
        .env("SHELL", "/bin/zsh")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("PLAYBOOK_BIN_DIR", &bin)
        .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
        .env_remove("CLAUDE_CODE_ENABLE_TELEMETRY")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_LOGS_EXPORTER")
        .env_remove("OTEL_METRICS_EXPORTER")
        .current_dir(&sb.home)
        .output()
        .unwrap();
    let config = Command::new(&exe)
        .args(["config", "get", "systemPrompt.enabled"])
        .env("HOME", &sb.home)
        .output()
        .unwrap();

    // Assert
    assert!(out.status.success(), "{}", text(&out));
    let settings = sb.read(".claude/settings.json");
    assert!(settings.contains("playbook hook"), "hooks");
    assert!(settings.contains("playbook statusline"), "status line");
    assert!(
        settings.contains("OTEL_EXPORTER_OTLP_ENDPOINT"),
        "telemetry"
    );
    assert!(settings.contains("\"attribution\""), "attribution OFF");
    assert!(sb.path(".zshenv").is_file(), "path: {}", text(&out));
    assert!(sb
        .path(".config/playbook/prompts/SYSTEM_PROMPT.md")
        .is_file());
    assert!(String::from_utf8_lossy(&config.stdout).contains("true (source: global)"));
    assert_eq!(
        fs::read_link(bin.join("pb")).unwrap(),
        Path::new("playbook")
    );
    assert!(!text(&out).contains("collector-service"), "{}", text(&out));
}

#[test]
fn yes_keeps_the_prompt_off_on_an_earlier_install_and_after_a_no() {
    let earlier = Sandbox::new("yes-earlier");
    earlier.write(
        ".claude/settings.json",
        r#"{"statusLine":{"type":"command","command":"playbook statusline"}}"#,
    );
    let said_no = Sandbox::new("yes-said-no");
    said_no.config_set("systemPrompt.enabled", "false");

    let first = earlier.init("/bin/zsh", &[]);
    let second = said_no.init("/bin/zsh", &[]);

    for (sb, out, reason) in [
        (&earlier, &first, "this install had no system prompt before"),
        (&said_no, &second, "systemPrompt.enabled is false"),
    ] {
        assert!(!sb
            .path(".config/playbook/prompts/SYSTEM_PROMPT.md")
            .exists());
        let said = text(out);
        assert!(said.contains(reason), "{said}");
        assert!(
            said.contains("playbook setup --only system-prompt"),
            "{said}"
        );
    }
}

#[test]
fn no_system_prompt_stores_false_and_only_system_prompt_turns_it_back_on() {
    let sb = Sandbox::new("prompt-no-then-yes");
    let prompt = ".config/playbook/prompts/SYSTEM_PROMPT.md";

    let off = sb.init("/bin/bash", &["--no-system-prompt"]);
    let stored_off = sb.config_get("systemPrompt.enabled");
    let file_after_no = sb.path(prompt).exists();
    let on = sb.init("/bin/bash", &["--only", "system-prompt"]);
    let stored_on = sb.config_get("systemPrompt.enabled");

    assert!(off.status.success(), "{}", text(&off));
    assert_eq!(stored_off, "systemPrompt.enabled: false (source: global)");
    assert!(!file_after_no, "a no must not place the prompt");
    assert!(on.status.success(), "{}", text(&on));
    assert_eq!(stored_on, "systemPrompt.enabled: true (source: global)");
    assert!(sb.path(prompt).exists(), "{}", text(&on));
}

#[test]
fn only_a_retired_part_runs_nothing() {
    let sb = Sandbox::new("retired-only");

    let out = sb.init("/bin/bash", &["--only", "aliases"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!sb.path(".claude/settings.json").exists());
    assert!(!sb.path(".bashrc").exists());
    assert!(!sb.path(".zshrc").exists());
}

/// The places the retired collector-service part wrote: the launchd agent,
/// the systemd unit and the stored no.
fn no_collector_service_trace(sb: &Sandbox) {
    assert!(!sb.path("Library/LaunchAgents").exists());
    assert!(!sb.path(".config/systemd").exists());
    assert!(!playbook::init::collector_service::declined(&sb.home));
}

#[test]
fn only_the_retired_collector_service_part_runs_nothing() {
    let sb = Sandbox::new("retired-collector-only");

    let out = sb.init("/bin/bash", &["--only", "collector-service"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!sb.path(".claude/settings.json").exists());
    no_collector_service_trace(&sb);
}

#[test]
fn the_retired_collector_service_flags_are_accepted_and_change_nothing() {
    for flag in ["--collector-service", "--no-collector-service"] {
        let sb = Sandbox::new("retired-collector-flag");
        let plain = Sandbox::new("retired-collector-plain");

        let out = sb.init("/bin/bash", &[flag, "--only", "hooks"]);
        plain.init("/bin/bash", &["--only", "hooks"]);

        assert!(out.status.success(), "{flag}: {}", text(&out));
        assert!(!text(&out).contains("collector-service"), "{}", text(&out));
        assert_eq!(
            sb.read(".claude/settings.json"),
            plain.read(".claude/settings.json"),
            "{flag}"
        );
        no_collector_service_trace(&sb);
    }
}

#[test]
fn the_system_prompt_flag_places_the_prompt_where_the_launcher_reads_it() {
    let sb = Sandbox::new("prompt");
    sb.init("/bin/bash", &["--system-prompt"]);
    let shipped = fs::read_to_string(
        Path::new(env!("PLAYBOOK_REPO_ROOT")).join("seed/prompts/SYSTEM_PROMPT.md"),
    )
    .unwrap();
    assert_eq!(
        sb.read(".config/playbook/prompts/SYSTEM_PROMPT.md"),
        shipped
    );
    assert!(!sb.path(".claude/prompts").exists());
}

#[test]
fn a_run_refreshes_an_installed_prompt_and_an_unattended_one_adds_none() {
    let sb = Sandbox::new("prompt-refresh");
    sb.write(
        ".config/playbook/prompts/SYSTEM_PROMPT.md",
        "a stale copy\n",
    );
    sb.init("/bin/bash", &[]);
    let shipped = fs::read_to_string(
        Path::new(env!("PLAYBOOK_REPO_ROOT")).join("seed/prompts/SYSTEM_PROMPT.md"),
    )
    .unwrap();
    assert_eq!(
        sb.read(".config/playbook/prompts/SYSTEM_PROMPT.md"),
        shipped
    );

    let fresh = Sandbox::new("prompt-absent");
    fresh.init_raw("/bin/bash", &["--no-telemetry"]);
    assert!(!fresh
        .path(".config/playbook/prompts/SYSTEM_PROMPT.md")
        .exists());
}

#[test]
fn a_custom_settings_key_survives() {
    let sb = Sandbox::new("merge");
    sb.write(
        ".claude/settings.json",
        "{\"my_custom_key\":\"sentinel_value\"}\n",
    );
    sb.init("/bin/bash", &[]);
    assert_eq!(
        json(&sb.read(".claude/settings.json"))["my_custom_key"],
        "sentinel_value"
    );
}

#[test]
fn a_plain_run_twice_is_byte_identical_and_makes_no_new_backups() {
    let sb = Sandbox::new("idempotent");
    sb.init("/bin/bash", &[]);
    let settings = sb.read(".claude/settings.json");
    let base = sb.read(".claude/.settings.base.json");
    let backups = sb.count(".claude", "settings.json.bak.");
    let skips = sb.count(".claude", "settings-merge-skipped.");
    let again = text(&sb.init("/bin/bash", &[]));
    for step in ["settings", "hooks"] {
        assert!(
            again.contains(&format!("[=] {step}  ")),
            "{step} rewrote: {again}"
        );
    }
    assert_eq!(sb.read(".claude/settings.json"), settings);
    assert_eq!(sb.read(".claude/.settings.base.json"), base);
    assert_eq!(sb.count(".claude", "settings.json.bak."), backups);
    assert_eq!(sb.count(".claude", "settings-merge-skipped."), skips);
}

#[test]
fn no_hooks_leaves_every_hook_entry_out_and_says_so() {
    let h = Sandbox::new("no-hooks");
    let out = h.init("/bin/zsh", &["--no-hooks"]);
    let t = text(&out);
    assert!(t.contains("[skip] hooks  "), "{t}");
    let settings = h.read(".claude/settings.json");
    assert!(!settings.contains("playbook hook"), "{settings}");
    assert!(
        settings.contains("playbook statusline"),
        "the status line part still applies"
    );
}

#[test]
fn no_hooks_keeps_the_hooks_a_user_already_has() {
    let h = Sandbox::new("keep-hooks");
    h.write(
        ".claude/settings.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#,
    );
    h.init("/bin/zsh", &["--no-hooks"]);
    let settings = h.read(".claude/settings.json");
    assert!(settings.contains("echo mine"), "{settings}");
    assert!(!settings.contains("playbook hook"), "{settings}");
}

#[test]
fn a_yes_run_wires_hooks_and_a_rerun_with_no_hooks_is_stable() {
    let h = Sandbox::new("hooks-default");
    h.init("/bin/zsh", &[]);
    assert!(h.read(".claude/settings.json").contains("playbook hook"));
    let before = h.read(".claude/settings.json");
    h.init("/bin/zsh", &["--no-hooks"]);
    assert_eq!(
        h.read(".claude/settings.json"),
        before,
        "existing hooks stay"
    );
}

#[test]
fn a_yes_run_without_a_terminal_uses_defaults_and_never_blocks() {
    let h = Sandbox::new("defaults");
    let out = h.init("/bin/zsh", &[]);
    let t = text(&out);
    assert!(t.contains("hooks"), "{t}");
    assert!(!t.contains("shim"), "{t}");
}

#[test]
fn a_no_flag_overrides_its_yes_twin() {
    let h = Sandbox::new("override");
    let t = text(&h.init("/bin/zsh", &["--short-name", "--no-short-name"]));
    assert!(t.contains("you said no"), "{t}");
}

fn has_security(settings: &Value) -> bool {
    settings.get("permissions").is_some()
}

#[test]
fn a_plain_init_applies_no_security_defaults() {
    let sb = Sandbox::new("sec-off");
    let out = sb.init("/bin/bash", &[]);
    let settings = json(&sb.read(".claude/settings.json"));
    assert!(!has_security(&settings), "{settings}");
    assert!(settings.get("env").is_none(), "{settings}");
    assert!(text(&out).contains("--security"), "{}", text(&out));
}

#[test]
fn the_security_flag_applies_the_defaults_and_a_rerun_is_stable() {
    let sb = Sandbox::new("sec-flag");
    sb.init("/bin/bash", &["--security"]);
    let first = sb.read(".claude/settings.json");
    let settings = json(&first);
    assert!(
        settings.get("env").is_none(),
        "no autoupdater switch: {settings}"
    );
    assert_eq!(settings["permissions"]["deny"][0], "Read(**/.env)");
    assert!(settings["permissions"]["ask"]
        .to_string()
        .contains("Bash(python3:*)"));
    let backups = sb.count(".claude", "settings.json.bak.");

    sb.init("/bin/bash", &["--security"]);
    assert_eq!(sb.read(".claude/settings.json"), first);
    assert_eq!(sb.count(".claude", "settings.json.bak."), backups);
}

#[test]
fn a_security_rerun_adds_the_code_allow_entry_to_a_block_the_user_never_changed() {
    // Arrange: an install made before the entry shipped, block equal to its base.
    const CODE_ENTRY: &str = "Bash(playbook code:*)";
    let shipped =
        fs::read_to_string(Path::new(env!("PLAYBOOK_REPO_ROOT")).join("seed/settings.shared.json"))
            .expect("shipped template is readable");
    let mut permissions = json(&shipped)["permissions"].clone();
    let allow = permissions["allow"].as_array_mut().expect("allow list");
    allow.retain(|entry| entry != CODE_ENTRY);
    let older_allow = allow.clone();
    let older = serde_json::json!({ "permissions": permissions }).to_string();
    let sb = Sandbox::new("sec-code-entry");
    sb.write(".claude/settings.json", &older);
    sb.write(".claude/.settings.base.json", &older);

    // Act
    sb.init("/bin/bash", &["--security"]);

    // Assert
    let merged = json(&sb.read(".claude/settings.json"));
    let merged_allow = merged["permissions"]["allow"]
        .as_array()
        .expect("allow list");
    assert_eq!(
        merged_allow
            .iter()
            .filter(|entry| *entry == CODE_ENTRY)
            .count(),
        1,
        "{merged}"
    );
    for entry in &older_allow {
        assert!(
            merged_allow.contains(entry),
            "{entry} was dropped: {merged}"
        );
    }
}

#[test]
fn the_config_key_applies_the_defaults_and_no_security_overrides_it() {
    let sb = Sandbox::new("sec-config");
    sb.config_set("security.defaults", "true");
    sb.init("/bin/bash", &[]);
    assert!(has_security(&json(&sb.read(".claude/settings.json"))));

    let other = Sandbox::new("sec-config-override");
    other.config_set("security.defaults", "true");
    other.init("/bin/bash", &["--no-security"]);
    assert!(!has_security(&json(&other.read(".claude/settings.json"))));
}

#[test]
fn a_plain_init_never_strips_security_entries_the_user_already_has() {
    let sb = Sandbox::new("sec-keep");
    sb.init("/bin/bash", &["--security"]);
    let applied = sb.read(".claude/settings.json");
    let base = sb.read(".claude/.settings.base.json");

    sb.init("/bin/bash", &[]);
    assert_eq!(sb.read(".claude/settings.json"), applied);
    assert_eq!(sb.read(".claude/.settings.base.json"), base);

    // A hand-written block survives too, and a later opt-in does not
    // overwrite it.
    let mine = Sandbox::new("sec-mine");
    mine.write(
        ".claude/settings.json",
        r#"{"permissions":{"allow":["Read"]},"env":{"DISABLE_AUTOUPDATER":"0","MINE":"1"}}"#,
    );
    mine.init("/bin/bash", &[]);
    let settings = json(&mine.read(".claude/settings.json"));
    assert_eq!(settings["permissions"], json(r#"{"allow":["Read"]}"#));
    assert_eq!(settings["env"]["DISABLE_AUTOUPDATER"], "0");
    assert_eq!(settings["env"]["MINE"], "1");
    mine.init("/bin/bash", &["--security"]);
    let settings = json(&mine.read(".claude/settings.json"));
    assert_eq!(settings["permissions"], json(r#"{"allow":["Read"]}"#));
}

#[test]
fn an_existing_install_keeps_every_key_an_older_template_shipped() {
    let sb = Sandbox::new("retired-keys");
    let old = r#"{"cleanupPeriodDays":14,"outputStyle":"Concise & Direct","tui":"fullscreen","env":{"DO_NOT_TRACK":"1","DISABLE_AUTOUPDATER":"1"},"worktree":{"baseRef":"head"}}"#;
    sb.write(".claude/settings.json", old);
    sb.write(".claude/.settings.base.json", old);

    sb.init("/bin/bash", &[]);

    let settings = json(&sb.read(".claude/settings.json"));
    assert_eq!(settings["cleanupPeriodDays"], 14);
    assert_eq!(settings["outputStyle"], "Concise & Direct");
    assert_eq!(settings["tui"], "fullscreen");
    assert_eq!(settings["env"]["DO_NOT_TRACK"], "1");
    assert_eq!(settings["env"]["DISABLE_AUTOUPDATER"], "1");
    assert_eq!(settings["worktree"]["baseRef"], "head");
    assert!(settings.get("hooks").is_some());
}

#[test]
fn the_retired_settings_flags_are_refused() {
    let sb = Sandbox::new("no-settings-flag");

    for flag in ["--settings", "--no-settings"] {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["setup", flag])
            .env("HOME", &sb.home)
            .current_dir(&sb.home)
            .output()
            .expect("playbook should spawn");
        assert!(!out.status.success(), "{flag} should be refused");
    }
}

#[test]
fn a_fresh_install_adds_no_personal_preference_key() {
    let sb = Sandbox::new("no-prefs");
    sb.init("/bin/bash", &[]);

    let settings = json(&sb.read(".claude/settings.json"));

    let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["attribution", "hooks", "statusLine"], "{keys:?}");
}

#[test]
fn turning_every_settings_part_off_leaves_settings_json_alone() {
    let sb = Sandbox::new("all-off");
    sb.write(".claude/settings.json", "{\"theme\":\"dark\"}\n");

    let t = text(&sb.init(
        "/bin/bash",
        &[
            "--no-hooks",
            "--no-statusline",
            "--no-security",
            // Attribution ON writes nothing to a file with no attribution key.
            "--attribution",
        ],
    ));

    assert_eq!(sb.read(".claude/settings.json"), "{\"theme\":\"dark\"}\n");
    assert!(t.contains("settings.json is not merged"), "{t}");
}

#[test]
fn a_run_with_no_terminal_and_no_yes_leaves_the_claude_settings_alone() {
    let h = Sandbox::new("unattended");

    let out = h.init_raw("/bin/zsh", &[]);

    let t = text(&out);
    assert!(t.contains("playbook setup --yes"), "{t}");
    assert!(t.contains("hooks, status line and attribution off"), "{t}");
    let settings = fs::read_to_string(h.path(".claude/settings.json")).unwrap_or_default();
    assert!(!settings.contains("playbook hook"), "{settings}");
    assert!(!settings.contains("statusLine"), "{settings}");
    assert!(!settings.contains("attribution"), "{settings}");
}

#[test]
fn an_explicit_hooks_flag_with_no_terminal_adds_only_the_hooks() {
    let h = Sandbox::new("unattended-hooks");

    let out = h.init_raw("/bin/zsh", &["--hooks"]);
    let t = text(&out);
    assert!(
        t.contains("settings: status line and attribution off."),
        "{t}"
    );

    let settings = fs::read_to_string(h.path(".claude/settings.json")).unwrap_or_default();
    assert!(settings.contains("playbook hook"), "{settings}");
    assert!(!settings.contains("statusLine"), "{settings}");
}

fn attribution_of(sb: &Sandbox) -> Value {
    let settings: Value = serde_json::from_str(&sb.read(".claude/settings.json")).unwrap();
    settings["attribution"].clone()
}

#[test]
fn yes_turns_attribution_off_and_keeps_the_other_settings() {
    // Arrange
    let sb = Sandbox::new("attribution-yes");
    sb.write(".claude/settings.json", "{\"theme\":\"dark\"}\n");

    // Act
    sb.init("/bin/bash", &["--no-hooks", "--no-statusline"]);

    // Assert
    assert_eq!(
        attribution_of(&sb),
        serde_json::json!({"commit": "", "pr": "", "sessionUrl": false})
    );
    let settings: Value = serde_json::from_str(&sb.read(".claude/settings.json")).unwrap();
    assert_eq!(settings["theme"], "dark");
}

#[test]
fn yes_and_only_keep_a_user_attribution_value_and_no_attribution_replaces_it() {
    // Arrange
    let sb = Sandbox::new("attribution-user");
    sb.write(
        ".claude/settings.json",
        "{\"attribution\":{\"commit\":\"Made by me\"}}\n",
    );

    // Act
    sb.init("/bin/bash", &["--no-hooks", "--no-statusline"]);
    let after_yes = attribution_of(&sb);
    let kept = text(&sb.init_raw("/bin/bash", &["--only", "attribution"]));
    let after_only = attribution_of(&sb);
    sb.init_raw("/bin/bash", &["--only", "attribution", "--no-attribution"]);

    // Assert
    assert_eq!(
        after_yes,
        serde_json::json!({"commit": "Made by me", "pr": "", "sessionUrl": false})
    );
    assert!(
        kept.contains("--only attribution --no-attribution"),
        "{kept}"
    );
    assert_eq!(after_only["commit"], "Made by me");
    assert_eq!(attribution_of(&sb)["commit"], "");
}

#[test]
fn attribution_on_removes_only_the_off_values_and_yes_keeps_it_on() {
    // Arrange
    let sb = Sandbox::new("attribution-on");
    sb.write(
        ".claude/settings.json",
        "{\"theme\":\"dark\",\"attribution\":{\"commit\":\"custom\",\"pr\":\"\",\"sessionUrl\":false},\"model\":\"opus\"}\n",
    );

    // Act
    let on = text(&sb.init_raw("/bin/bash", &["--only", "attribution", "--attribution"]));
    let after_on = sb.read(".claude/settings.json");
    sb.init("/bin/bash", &["--no-hooks", "--no-statusline"]);
    let after_yes = sb.read(".claude/settings.json");

    // Assert
    assert!(
        on.contains("attribution on: removed attribution.pr, attribution.sessionUrl"),
        "{on}"
    );
    let settings: Value = serde_json::from_str(&after_on).unwrap();
    let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["theme", "attribution", "model"], "{after_on}");
    assert_eq!(
        settings["attribution"],
        serde_json::json!({"commit": "custom"})
    );
    assert_eq!(after_yes, after_on, "--yes keeps the stored ON");
}

#[test]
fn the_old_attribution_flags_still_work() {
    // Arrange
    let sb = Sandbox::new("attribution-old");
    sb.write(
        ".claude/settings.json",
        "{\"attribution\":{\"commit\":\"mine\"}}\n",
    );

    // Act
    sb.init_raw(
        "/bin/bash",
        &["--only", "attribution-off", "--attribution-off"],
    );
    let off = attribution_of(&sb);
    sb.init_raw(
        "/bin/bash",
        &["--only", "attribution-off", "--no-attribution-off"],
    );
    let on = attribution_of(&sb);

    // Assert
    assert_eq!(
        off,
        serde_json::json!({"commit": "", "pr": "", "sessionUrl": false}),
        "--attribution-off is --no-attribution, which replaces your own value"
    );
    assert_eq!(on, Value::Null, "--no-attribution-off is --attribution");
}
