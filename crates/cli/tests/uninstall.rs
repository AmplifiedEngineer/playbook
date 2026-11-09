// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook uninstall` against a scratch `$HOME`: it undoes what `init` and
//! the installer placed, keeps everything the user owns, and changes nothing
//! without `--yes`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    home: PathBuf,
    bin_dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "playbook-uninstall-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&home);
        let bin_dir = home.join("bin");
        fs::create_dir_all(home.join(".claude")).expect("scratch home");
        fs::create_dir_all(&bin_dir).expect("scratch bin dir");
        Self { home, bin_dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            .env("PLAYBOOK_BIN_DIR", &self.bin_dir)
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            // No real `claude plugin update` from a test.
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .current_dir(&self.home)
            .output()
            .expect("playbook should spawn")
    }

    fn init(&self) {
        let out = self.run(&["setup", "--yes", "--no-telemetry", "--system-prompt"]);
        assert!(out.status.success(), "init failed: {}", text(&out));
        // Init no longer places statusline.sh. Seed the copy an older install
        // left, recorded the way that install recorded it.
        self.write(
            ".config/playbook/statusline.sh",
            "#!/bin/bash\necho shipped\n",
        );
        playbook::init::migrate::record_shipped(
            &self.home,
            "statusline",
            &self.path(".config/playbook/statusline.sh"),
        );
        // Setup no longer writes a shell line (ADR 0044). Seed the block an
        // older setup left, so uninstall still has one to remove.
        let rc = self.read(".zshrc");
        self.write(".zshrc", &format!("{rc}{OLD_RC_BLOCK}"));
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
        fs::write(path, content).expect("scratch file");
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    fn settings(&self) -> serde_json::Value {
        serde_json::from_str(&self.read(".claude/settings.json")).expect("settings.json is JSON")
    }

    fn backups(&self, dir: &Path, prefix: &str) -> usize {
        fs::read_dir(dir)
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

fn hook_commands(settings: &serde_json::Value) -> Vec<String> {
    let mut found = Vec::new();
    for groups in settings["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|h| h.values())
    {
        for group in groups.as_array().into_iter().flatten() {
            for entry in group["hooks"].as_array().into_iter().flatten() {
                if let Some(cmd) = entry["command"].as_str() {
                    found.push(cmd.to_string());
                }
            }
        }
    }
    found
}

const USER_RC: &str = "export A=1\n";

const OLD_RC_BLOCK: &str = "\n# playbook launchers (ccc/ccd)\ncommand -v playbook >/dev/null 2>&1 && eval \"$(playbook shell-init)\"\n";

#[test]
fn removes_everything_init_placed_and_keeps_the_users_own_content() {
    let sb = Sandbox::new("roundtrip");
    sb.write(".zshrc", USER_RC);
    sb.write(
        ".claude/settings.json",
        r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-own"}]}]}}"#,
    );
    sb.write(".config/playbook/memory/fact.md", "keep me\n");
    sb.write(".claude/backups/old/settings.json", "{}\n");
    sb.write(".config/playbook/shell/zsh/cc.zsh", "old launcher\n");
    sb.init();
    let settings_after_init = sb.read(".claude/settings.json");
    let backups_after_init = sb.backups(&sb.path(".claude"), "settings.json.bak.");
    assert!(sb.read(".zshrc").contains("playbook shell-init"));
    assert!(sb.path(".config/playbook/statusline.sh").is_file());

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));

    assert_eq!(sb.read(".zshrc"), USER_RC);
    let settings = sb.settings();
    assert_eq!(settings["theme"], "dark");
    assert_eq!(hook_commands(&settings), vec!["my-own".to_string()]);
    assert!(settings.get("statusLine").is_none());
    assert_eq!(settings["attribution"]["sessionUrl"], false);
    assert!(
        text(&out).contains("kept the attribution key"),
        "{}",
        text(&out)
    );
    assert!(
        text(&out)
            .contains("run `playbook setup --only attribution --attribution` while playbook is still installed"),
        "{}",
        text(&out)
    );
    for gone in [
        ".config/playbook/statusline.sh",
        ".config/playbook/prompts/SYSTEM_PROMPT.md",
        ".config/playbook/hooks/lib/config-hash.sh",
    ] {
        assert!(!sb.path(gone).exists(), "{gone} should be removed");
    }
    assert!(sb.path(".config/playbook/memory/fact.md").is_file());
    assert!(sb.path(".claude/backups/old/settings.json").is_file());
    assert_eq!(sb.backups(&sb.home, ".zshrc.bak-"), 1);
    assert!(!sb.path(".config/playbook/shell").exists());
    let backups = sb.backups(&sb.path(".claude"), "settings.json.bak.");
    assert!(
        backups >= backups_after_init,
        "uninstall must not drop an earlier backup"
    );
    let newest = newest_backup(&sb.path(".claude"));
    assert_eq!(fs::read_to_string(newest).unwrap(), settings_after_init);
    let rc_backup = fs::read_dir(&sb.home)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().starts_with(".zshrc.bak-"))
        .expect("rc backup");
    assert!(fs::read_to_string(rc_backup.path())
        .unwrap()
        .contains("playbook shell-init"));
}

/// The settings backup written last, by file name (an epoch suffix).
fn newest_backup(dir: &Path) -> PathBuf {
    let mut backups: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.bak.")
        })
        .collect();
    backups.sort();
    backups.pop().expect("a settings backup")
}

#[test]
fn without_yes_nothing_changes_and_it_exits_nonzero() {
    let sb = Sandbox::new("no-yes");
    sb.init();
    let rc_before = sb.read(".zshrc");
    let settings_before = sb.read(".claude/settings.json");

    let out = sb.run(&["uninstall"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("--yes"), "{}", text(&out));
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert_eq!(sb.read(".claude/settings.json"), settings_before);
    assert!(sb.path(".config/playbook/statusline.sh").is_file());
}

#[test]
fn dry_run_lists_the_plan_and_changes_nothing() {
    let sb = Sandbox::new("dry-run");
    sb.init();
    sb.write("bin/playbook", "#!/bin/sh\n");
    sb.write(".config/playbook/shell/cc.zsh", "old\n");
    let rc_before = sb.read(".zshrc");
    let settings_before = sb.read(".claude/settings.json");
    let settings_backups = sb.backups(&sb.path(".claude"), "settings.json.bak.");

    let out = sb.run(&["uninstall", "--yes", "--dry-run", "--remove-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    let listed = text(&out);
    assert!(listed.contains("would remove"), "{listed}");
    assert!(listed.contains("bin/playbook"), "{listed}");
    assert_eq!(sb.read(".zshrc"), rc_before);
    assert!(sb.path("bin/playbook").is_file());
    assert!(sb.path(".config/playbook/statusline.sh").is_file());
    assert_eq!(sb.backups(&sb.home, ".zshrc.bak-"), 0);
    assert_eq!(sb.read(".claude/settings.json"), settings_before);
    assert_eq!(
        sb.backups(&sb.path(".claude"), "settings.json.bak."),
        settings_backups
    );
    assert!(sb.path(".config/playbook/shell/cc.zsh").is_file());
}

#[test]
fn the_binary_stays_unless_asked_for() {
    let sb = Sandbox::new("binary");
    sb.write(
        ".bashrc",
        "export X=1\n\n# playbook binary\nexport PATH=\"/x/bin:$PATH\"\nexport PATH=\"/opt/mine:$PATH\"\n",
    );
    sb.write("bin/playbook", "#!/bin/sh\n");
    sb.write("bin/playbook.0.1.0.bak", "old\n");
    sb.write("bin/other", "keep\n");
    std::os::unix::fs::symlink("playbook", sb.path("bin/pb")).unwrap();

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.path("bin/playbook").is_file());
    assert!(sb.read(".bashrc").contains("# playbook binary"));

    let out = sb.run(&["uninstall", "--yes", "--remove-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!sb.path("bin/playbook").exists());
    assert!(!sb.path("bin/playbook.0.1.0.bak").exists());
    assert!(sb.path("bin/pb").symlink_metadata().is_err());
    assert!(sb.path("bin/other").is_file());
    assert_eq!(
        sb.read(".bashrc"),
        "export X=1\n\nexport PATH=\"/opt/mine:$PATH\"\n"
    );
}

#[test]
fn an_edited_statusline_script_is_kept() {
    let sb = Sandbox::new("edited");
    sb.init();
    sb.write(".config/playbook/statusline.sh", "#!/bin/bash\necho mine\n");

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("kept"), "{}", text(&out));
    assert!(sb
        .read(".config/playbook/statusline.sh")
        .contains("echo mine"));
}

#[test]
fn an_edited_system_prompt_is_kept() {
    let sb = Sandbox::new("edited-prompt");
    sb.init();
    sb.write(".config/playbook/prompts/SYSTEM_PROMPT.md", "my prompt\n");

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        sb.read(".config/playbook/prompts/SYSTEM_PROMPT.md"),
        "my prompt\n"
    );
}

#[test]
fn files_with_no_placement_record_are_kept() {
    let sb = Sandbox::new("unrecorded");
    sb.write(
        ".config/playbook/statusline.sh",
        "#!/bin/bash\necho theirs\n",
    );
    sb.write(".config/playbook/prompts/SYSTEM_PROMPT.md", "theirs\n");

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.path(".config/playbook/statusline.sh").is_file());
    assert!(sb
        .path(".config/playbook/prompts/SYSTEM_PROMPT.md")
        .is_file());
}

#[test]
fn a_custom_status_line_that_runs_the_placed_script_keeps_it() {
    let sb = Sandbox::new("status-in-use");
    sb.init();
    sb.write(
        ".claude/settings.json",
        r#"{"statusLine":{"type":"command","command":"bash ~/.config/playbook/statusline.sh --compact"}}"#,
    );
    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.path(".config/playbook/statusline.sh").is_file());
    assert!(sb.read(".claude/settings.json").contains("--compact"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_stays_a_symlink_and_keeps_its_mode() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let sb = Sandbox::new("symlink");
    sb.write(
        "dotfiles/settings.json",
        r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"playbook hook session-clean-exit"}]}]}}"#,
    );
    fs::set_permissions(
        sb.path("dotfiles/settings.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    symlink(
        sb.path("dotfiles/settings.json"),
        sb.path(".claude/settings.json"),
    )
    .unwrap();

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(fs::symlink_metadata(sb.path(".claude/settings.json"))
        .unwrap()
        .file_type()
        .is_symlink());
    let target = sb.read("dotfiles/settings.json");
    assert!(!target.contains("playbook hook"), "{target}");
    assert!(target.contains("dark"));
    let mode = fs::metadata(sb.path("dotfiles/settings.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_custom_status_line_and_custom_hooks_survive() {
    let sb = Sandbox::new("custom");
    sb.write(
        ".claude/settings.json",
        r#"{"statusLine":{"type":"command","command":"my-status"},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"playbook hook rm-workspace-guard"},{"type":"command","command":"my-guard"}]}]}}"#,
    );

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    let settings = sb.settings();
    assert_eq!(settings["statusLine"]["command"], "my-status");
    assert_eq!(hook_commands(&settings), vec!["my-guard".to_string()]);
}

#[test]
fn the_legacy_statusline_command_goes_with_the_placed_script() {
    let sb = Sandbox::new("legacy-status");
    sb.write(
        ".claude/settings.json",
        r#"{"statusLine":{"type":"command","command":"bash $HOME/.config/playbook/statusline.sh"}}"#,
    );
    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(sb.settings().get("statusLine").is_none());
}

#[test]
fn an_invalid_settings_file_is_reported_and_left_alone() {
    let sb = Sandbox::new("bad-json");
    sb.write(".claude/settings.json", "{ not json");
    sb.write(
        ".zshrc",
        "# playbook launchers (cc/ccd)\neval \"$(playbook shell-init)\"\n",
    );

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(!out.status.success());
    assert_eq!(sb.read(".claude/settings.json"), "{ not json");
    assert_eq!(sb.read(".zshrc"), "");
}

#[test]
fn nothing_installed_is_a_clean_no_op() {
    let sb = Sandbox::new("empty");
    let out = sb.run(&["uninstall", "--yes", "--remove-binary"]);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn uninstall_removes_the_codex_guards_and_keeps_the_users_own_hook() {
    // Arrange: a hooks.json with the guards setup wrote and one of the user's
    let sb = Sandbox::new("codex-hooks");
    let file = playbook::init::codex_hooks::hooks_path(&sb.home);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mine = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-own-hook"}]}]}}"#;
    fs::write(&file, mine).unwrap();
    playbook::init::codex_hooks::wire(&file, 100).expect("wire");
    assert!(fs::read_to_string(&file).unwrap().contains("playbook hook"));

    // Act
    let out = sb.run(&["uninstall", "--yes", "--remove-binary"]);

    // Assert
    assert!(out.status.success(), "{}", text(&out));
    let after = fs::read_to_string(&file).unwrap();
    assert!(!after.contains("playbook hook"), "{after}");
    assert!(after.contains("my-own-hook"), "{after}");
    assert!(text(&out).contains("hook entries"), "{}", text(&out));
}

#[test]
fn the_installer_scripts_in_the_claude_dir_are_removed_and_nothing_else() {
    let sb = Sandbox::new("claude-dir");
    sb.write(".claude/install.sh", "#!/bin/bash\n");
    sb.write(".claude/uninstall.sh", "#!/bin/bash\n");
    sb.write(".claude/CLAUDE.md", "mine\n");
    sb.write(".claude/projects/p/s.jsonl", "{}\n");

    let out = sb.run(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!sb.path(".claude/install.sh").exists());
    assert!(!sb.path(".claude/uninstall.sh").exists());
    assert!(sb.path(".claude/CLAUDE.md").is_file());
    assert!(sb.path(".claude/projects/p/s.jsonl").is_file());
}

const LISTED: &str = r#"{"version":2,"plugins":{"playbook@amplifiedengineer":[{"scope":"user"}]}}"#;

#[test]
fn uninstall_lists_the_plugin_and_marketplace_steps_that_still_apply() {
    // Arrange
    let sb = Sandbox::new("manual-both");
    sb.write(".claude/plugins/installed_plugins.json", LISTED);
    sb.write(
        ".claude/plugins/known_marketplaces.json",
        r#"{"amplifiedengineer":{"source":{}}}"#,
    );

    // Act
    let out = sb.run(&["uninstall", "--yes"]);

    // Assert
    let shown = text(&out);
    assert!(
        shown.contains("claude plugin uninstall playbook@amplifiedengineer"),
        "{shown}"
    );
    assert!(
        shown.contains("claude plugin marketplace remove amplifiedengineer"),
        "{shown}"
    );
    assert!(!shown.contains("pragmatic-engineer"), "{shown}");
}

#[test]
fn uninstall_names_the_old_plugin_and_marketplace_for_an_install_that_kept_them() {
    let sb = Sandbox::new("manual-old-name");
    sb.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{"playbook@pragmatic-engineer":[{"scope":"user"}]}}"#,
    );
    sb.write(
        ".claude/plugins/known_marketplaces.json",
        r#"{"pragmatic-engineer":{"source":{}}}"#,
    );

    let shown = text(&sb.run(&["uninstall", "--yes"]));

    assert!(
        shown.contains("claude plugin uninstall playbook@pragmatic-engineer"),
        "{shown}"
    );
    assert!(
        shown.contains("claude plugin marketplace remove pragmatic-engineer"),
        "{shown}"
    );
    assert!(!shown.contains("amplifiedengineer"), "{shown}");
}

#[test]
fn uninstall_lists_the_hooks_plugin_under_the_new_marketplace_name() {
    let sb = Sandbox::new("manual-hooks-new-name");
    sb.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{"playbook-hooks@amplifiedengineer":[{}]}}"#,
    );

    let shown = text(&sb.run(&["uninstall", "--yes"]));

    assert!(
        shown.contains("claude plugin uninstall playbook-hooks@amplifiedengineer"),
        "{shown}"
    );
}

#[test]
fn uninstall_lists_the_hooks_plugin_too() {
    let sb = Sandbox::new("manual-hooks-plugin");
    sb.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{"playbook@amplifiedengineer":[{}],"playbook-hooks@pragmatic-engineer":[{}]}}"#,
    );

    let shown = text(&sb.run(&["uninstall", "--yes"]));

    assert!(
        shown.contains("claude plugin uninstall playbook-hooks@pragmatic-engineer"),
        "{shown}"
    );
}

#[test]
fn uninstall_lists_only_the_steps_that_apply() {
    // Arrange: the plugin is gone but the marketplace entry remains.
    let sb = Sandbox::new("manual-marketplace-only");
    sb.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"version":2,"plugins":{}}"#,
    );
    sb.write(
        ".claude/plugins/known_marketplaces.json",
        r#"{"amplifiedengineer":{"source":{}}}"#,
    );

    // Act
    let shown = text(&sb.run(&["uninstall", "--yes"]));

    // Assert
    assert!(!shown.contains("claude plugin uninstall"), "{shown}");
    assert!(
        shown.contains("claude plugin marketplace remove amplifiedengineer"),
        "{shown}"
    );
}

#[test]
fn uninstall_prints_no_manual_steps_when_nothing_remains() {
    let sb = Sandbox::new("manual-none");
    let shown = text(&sb.run(&["uninstall", "--yes"]));
    assert!(!shown.contains("claude plugin"), "{shown}");
    assert!(!shown.contains("remaining manual steps"), "{shown}");
}

#[test]
fn init_telemetry_then_uninstall_removes_exactly_the_variables_init_wrote() {
    let sb = Sandbox::new("telemetry");
    sb.write(".claude/settings.json", "{\"env\":{\"MY_OWN\":\"1\"}}\n");
    let run = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.args(args)
            .env("HOME", &sb.home)
            .env("SHELL", "/bin/zsh")
            .env("PLAYBOOK_BIN_DIR", &sb.bin_dir)
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .current_dir(&sb.home);
        for (name, _) in std::env::vars() {
            if name.starts_with("OTEL_") || name == "CLAUDE_CODE_ENABLE_TELEMETRY" {
                cmd.env_remove(name);
            }
        }
        cmd.output().expect("playbook should spawn")
    };
    let init = run(&[
        "setup",
        "--telemetry",
        "--no-hooks",
        "--no-statusline",
        "--no-path",
        "--yes",
    ]);
    assert!(init.status.success(), "{}", text(&init));
    let after_init = sb.read(".claude/settings.json");
    assert!(
        after_init.contains("CLAUDE_CODE_ENABLE_TELEMETRY"),
        "{after_init}"
    );
    assert!(after_init.contains("MY_OWN"), "{after_init}");
    assert!(
        !after_init.contains("OTEL_LOG_USER_PROMPTS"),
        "{after_init}"
    );

    let gone = run(&["uninstall", "--yes"]);
    assert!(gone.status.success(), "{}", text(&gone));
    let after = sb.read(".claude/settings.json");
    assert!(!after.contains("OTEL_"), "{after}");
    assert!(!after.contains("CLAUDE_CODE_ENABLE_TELEMETRY"), "{after}");
    assert!(after.contains("MY_OWN"), "{after}");
}

#[test]
fn uninstall_removes_a_plugin_seen_marker_an_earlier_release_left() {
    let sb = Sandbox::new("plugin-seen");
    sb.write(".config/playbook/plugin-seen", "1\n");

    sb.run(&["uninstall", "--yes"]);

    assert!(!sb.path(".config/playbook/plugin-seen").exists());
}

#[test]
fn a_pb_of_the_users_own_is_kept() {
    let sb = Sandbox::new("own-pb");
    sb.write("bin/playbook", "#!/bin/sh\n");
    sb.write("bin/other", "#!/bin/sh\n");
    std::os::unix::fs::symlink("other", sb.path("bin/pb")).unwrap();

    let out = sb.run(&["uninstall", "--yes", "--remove-binary"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!sb.path("bin/playbook").exists());
    assert!(sb.path("bin/pb").symlink_metadata().is_ok());
}

#[test]
fn a_dry_run_keeps_the_pb_link() {
    let sb = Sandbox::new("dry-pb");
    sb.write("bin/playbook", "#!/bin/sh\n");
    std::os::unix::fs::symlink("playbook", sb.path("bin/pb")).unwrap();

    sb.run(&["uninstall", "--yes", "--dry-run", "--remove-binary"]);

    assert!(sb.path("bin/pb").symlink_metadata().is_ok());
}

#[test]
fn uninstall_removes_a_collector_service_left_from_an_older_setup() {
    use playbook::usage::service::{log_path, render, Kind};
    let Some(kind) = Kind::current() else {
        return;
    };
    // Arrange: the service file an older `setup --only collector-service`
    // wrote; setup no longer installs one.
    let sb = Sandbox::new("collector-service");
    let file = kind.file(&sb.home);
    let program = PathBuf::from(env!("CARGO_BIN_EXE_playbook"));
    sb.write(
        file.strip_prefix(&sb.home).unwrap().to_str().unwrap(),
        &render(kind, &program, &log_path(&sb.home)),
    );

    // Act
    let dry = sb.run(&["uninstall", "--yes", "--dry-run"]);
    let kept_by_dry_run = file.is_file();
    let removed = sb.run(&["uninstall", "--yes"]);

    // Assert
    assert!(
        text(&dry).contains("would stop and remove the"),
        "{}",
        text(&dry)
    );
    assert!(kept_by_dry_run);
    assert!(
        text(&removed).contains("stopped and removed the"),
        "{}",
        text(&removed)
    );
    assert!(!file.exists());
}

#[test]
fn an_agent_under_the_old_label_is_left_alone_in_a_scratch_home_and_uninstall_removes_it() {
    use playbook::usage::service::{log_path, old_file, render_old, Kind};
    if Kind::current() != Some(Kind::Launchd) {
        return;
    }
    // Arrange: the agent playbook 0.37.0 wrote, under the old label.
    let sb = Sandbox::new("old-label");
    let old = old_file(&sb.home);
    let program = std::path::Path::new(env!("CARGO_BIN_EXE_playbook"));
    sb.write(
        old.strip_prefix(&sb.home).unwrap().to_str().unwrap(),
        &render_old(program, &log_path(&sb.home)),
    );

    // Act
    let migrate = sb.run(&["migrate"]);
    let doctor = sb.run(&["doctor", "check"]);
    let kept = old.is_file();
    let dry = sb.run(&["uninstall", "--yes", "--dry-run"]);
    let removed = sb.run(&["uninstall", "--yes"]);

    // Assert
    // A scratch HOME never reaches the real launchd, so nothing is removed.
    // The removal migration says so once its release ships it (ADR 0046);
    // until then it does not run at all.
    let removal = playbook::init::migrate::COLLECTOR_REMOVAL_MIGRATION;
    let shipped = playbook::init::migrate::active()
        .iter()
        .any(|m| m.id == removal);
    let said = text(&migrate).contains(removal) && text(&migrate).contains("did not remove it");
    assert_eq!(said, shipped, "{}", text(&migrate));
    assert!(kept);
    assert!(!Kind::Launchd.file(&sb.home).exists());
    assert!(
        text(&doctor)
            .contains("a collector service file is still installed: launchd agent io.github")
            && text(&doctor).contains("playbook migrate"),
        "{}",
        text(&doctor)
    );
    assert!(
        text(&dry).contains("would stop and remove the launchd agent io.github"),
        "{}",
        text(&dry)
    );
    assert!(removed.status.success(), "{}", text(&removed));
    assert!(!old.exists(), "{}", text(&removed));
}
