// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `completions` part of `playbook setup`, run as the binary against a
//! scratch home. Every child starts from a cleared environment with only the
//! variables a scenario needs, so a developer's own `pb` or shell rc never
//! changes a result.

#![cfg(unix)]

use playbook::init::completion::{
    loader_path, loader_text, stub_paths, stub_text, Shell, LEGACY_BLOCK_REST, LEGACY_STUB,
    LOADER_HEADER, MARKER, RC_LINE, SKIP_UNKNOWN_SHELL, STUB_MARKER, ZSHRC,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const OFFLINE_PROXY: &str = "127.0.0.1:9";
const NOT_OURS_NOTE: &str = "(not ours)";
const EDITED_RC_LINE: &str =
    "[ -r \"$HOME/mine.zsh\" ] && . \"$HOME/mine.zsh\"  # playbook completions\n";
const READ_ONLY_DIR_MODE: u32 = 0o555;
const RESTORED_DIR_MODE: u32 = 0o755;
const ZSH_SHELL: &str = "/bin/zsh";
const BASH_SHELL: &str = "/bin/bash";
const FISH_SHELL: &str = "/usr/bin/fish";
const OWNER_ONLY_UMASK: &str = "077";
const POSIX_SH: &str = "/bin/sh";
const NEW_RC_MODE: u32 = 0o644;
const MODE_MASK: u32 = 0o777;

struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-init-completions-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let bin = root.join("bin");
        fs::create_dir_all(home.join(".claude")).expect("scratch home");
        fs::create_dir_all(&bin).expect("scratch bin");
        symlink(env!("CARGO_BIN_EXE_playbook"), bin.join("playbook")).expect("link playbook");
        Self { home, bin }
    }

    fn command(&self, shell: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::new(self.bin.join("playbook"));
        cmd.arg("setup")
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env("HTTPS_PROXY", OFFLINE_PROXY)
            .env("HTTP_PROXY", OFFLINE_PROXY)
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
            .current_dir(&self.home)
            .stdin(Stdio::null());
        if let Some(shell) = shell {
            cmd.env("SHELL", shell);
        }
        if let Some(ci) = std::env::var_os("CI") {
            cmd.env("CI", ci);
        }
        cmd
    }

    fn setup(&self, shell: Option<&str>, args: &[&str]) -> Output {
        let out = self.command(shell, args).output().expect("playbook spawns");
        assert!(out.status.success(), "setup failed: {}", text(&out));
        out
    }

    fn only(&self, shell: &str) -> Output {
        self.setup(Some(shell), &["--yes", "--only", "completions"])
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut seen = BTreeMap::new();
        walk(&self.home, &mut seen);
        seen
    }

    fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }
}

fn walk(dir: &Path, seen: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).expect("stat");
        if meta.is_dir() {
            seen.insert(path.clone(), Vec::new());
            walk(&path, seen);
        } else {
            seen.insert(path.clone(), fs::read(&path).unwrap_or_default());
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

fn rc_line() -> String {
    format!("{RC_LINE}\n")
}

/// The first path setup writes for a shell: the loader for zsh, else a stub.
fn first_file(home: &Path, shell: Shell) -> PathBuf {
    match shell {
        Shell::Zsh => loader_path(home),
        _ => stub_paths(home, shell).remove(0),
    }
}

fn assert_stubs_written(shell: Shell, shell_env: &str, tag: &str) {
    let sb = Sandbox::new(tag);

    sb.only(shell_env);

    let paths = stub_paths(&sb.home, shell);
    assert_eq!(paths.len(), 2, "playbook and pb stubs");
    for path in paths {
        assert_eq!(sb.read(&path), stub_text(shell), "{}", path.display());
    }
}

#[test]
fn setup_writes_the_bash_stubs() {
    assert_stubs_written(Shell::Bash, BASH_SHELL, "bash");
}

#[test]
fn setup_writes_the_fish_stubs() {
    assert_stubs_written(Shell::Fish, FISH_SHELL, "fish");
}

#[test]
fn setup_creates_a_missing_zshrc_with_one_line_and_the_loader() {
    let sb = Sandbox::new("zsh-fresh");

    sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&loader_path(&sb.home)), loader_text());
    assert_eq!(sb.read(&sb.home.join(ZSHRC)), rc_line());
    assert!(stub_paths(&sb.home, Shell::Zsh).is_empty());
}

#[test]
fn a_created_zshrc_is_world_readable_under_a_strict_umask() {
    let sb = Sandbox::new("zsh-umask");
    let mut cmd = Command::new(POSIX_SH);
    cmd.arg("-c")
        .arg(format!(
            "umask {OWNER_ONLY_UMASK}; exec \"$0\" setup --yes --only completions"
        ))
        .arg(sb.bin.join("playbook"))
        .env_clear()
        .env("HOME", &sb.home)
        .env("PATH", &sb.bin)
        .env("SHELL", ZSH_SHELL)
        .env("HTTPS_PROXY", OFFLINE_PROXY)
        .env("HTTP_PROXY", OFFLINE_PROXY)
        .current_dir(&sb.home)
        .stdin(Stdio::null());

    let out = cmd.output().expect("sh spawns");

    assert!(out.status.success(), "{}", text(&out));
    let mode = fs::metadata(sb.home.join(ZSHRC))
        .expect("zshrc exists")
        .permissions()
        .mode();
    assert_eq!(mode & MODE_MASK, NEW_RC_MODE);
}

#[test]
fn setup_adds_the_line_below_the_users_own_lines_and_keeps_them() {
    let sb = Sandbox::new("zsh-append");
    let own = "export EDITOR=vim\nautoload -Uz compinit && compinit\n";
    fs::write(sb.home.join(ZSHRC), own).expect("seed rc");

    sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&sb.home.join(ZSHRC)), format!("{own}{}", rc_line()));
}

#[test]
fn an_edited_marker_line_is_kept_and_no_second_line_is_added() {
    let sb = Sandbox::new("zsh-edited");
    fs::write(sb.home.join(ZSHRC), EDITED_RC_LINE).expect("seed rc");

    let out = sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&sb.home.join(ZSHRC)), EDITED_RC_LINE);
    assert!(text(&out).contains("(edited)"), "{}", text(&out));
}

#[test]
fn a_loader_without_our_header_is_kept_and_the_rc_is_not_touched() {
    let sb = Sandbox::new("zsh-foreign-loader");
    let theirs = loader_path(&sb.home);
    fs::create_dir_all(theirs.parent().expect("parent")).expect("mkdir");
    fs::write(&theirs, "# my own loader\n").expect("seed");

    let out = sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&theirs), "# my own loader\n");
    assert!(!sb.home.join(ZSHRC).exists());
    assert!(text(&out).contains(NOT_OURS_NOTE), "{}", text(&out));
}

#[test]
fn a_stale_loader_is_rewritten_and_the_rc_line_stays() {
    let sb = Sandbox::new("zsh-stale-loader");
    sb.only(ZSH_SHELL);
    let loader = loader_path(&sb.home);
    fs::write(&loader, format!("{LOADER_HEADER}\n# old logic\n")).expect("age the loader");
    let rc = sb.read(&sb.home.join(ZSHRC));

    sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&loader), loader_text());
    assert_eq!(sb.read(&sb.home.join(ZSHRC)), rc);
}

/// The loader the previous release wrote, before the compinit call went silent.
const PREVIOUS_LOADER_BODY: &str = "(( $+commands[playbook] )) || return 0\n\
(( $+functions[compdef] )) || { autoload -Uz compinit && compinit -C; }\n\
_playbook() { unfunction _playbook; source <(playbook completions zsh 2>/dev/null); _playbook \"$@\"; }\n\
compdef _playbook playbook\n\
[[ ${commands[pb]} -ef ${commands[playbook]} ]] && compdef _playbook pb\n";

#[test]
fn a_loader_written_by_the_previous_release_is_migrated() {
    let sb = Sandbox::new("zsh-previous-loader");
    sb.only(ZSH_SHELL);
    let loader = loader_path(&sb.home);
    fs::write(&loader, format!("{LOADER_HEADER}\n{PREVIOUS_LOADER_BODY}")).expect("age the loader");
    let rc = sb.read(&sb.home.join(ZSHRC));

    sb.only(ZSH_SHELL);

    assert_eq!(sb.read(&loader), loader_text());
    assert!(!sb.read(&loader).contains("compinit -C; }\n"));
    assert_eq!(sb.read(&sb.home.join(ZSHRC)), rc);
}

#[test]
fn setup_migrates_the_pre_loader_stub_and_block() {
    let sb = Sandbox::new("zsh-migrate");
    let stub = sb.home.join(LEGACY_STUB);
    fs::create_dir_all(stub.parent().expect("parent")).expect("mkdir");
    fs::write(&stub, format!("#compdef playbook pb\n{STUB_MARKER}\n")).expect("seed stub");
    let old = format!(
        "export A=1\n{MARKER}\n{}\n{}\n",
        LEGACY_BLOCK_REST[0], LEGACY_BLOCK_REST[1]
    );
    fs::write(sb.home.join(ZSHRC), &old).expect("seed rc");

    let out = sb.only(ZSH_SHELL);

    assert!(!stub.exists());
    assert_eq!(
        sb.read(&sb.home.join(ZSHRC)),
        format!("export A=1\n{}", rc_line())
    );
    assert_eq!(sb.read(&loader_path(&sb.home)), loader_text());
    assert!(text(&out).contains("removed"), "{}", text(&out));
    let backups: Vec<_> = fs::read_dir(&sb.home)
        .expect("home")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".zshrc.bak-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(sb.read(&backups[0].path()), old);
}

#[test]
fn a_second_run_leaves_the_home_tree_identical() {
    let sb = Sandbox::new("rerun");
    sb.only(ZSH_SHELL);
    let first = sb.snapshot();

    sb.only(ZSH_SHELL);

    assert_eq!(sb.snapshot(), first);
    assert_eq!(sb.read(&sb.home.join(ZSHRC)), rc_line());
}

#[test]
fn dry_run_leaves_the_home_tree_identical() {
    let sb = Sandbox::new("dry-run");
    let before = sb.snapshot();

    let out = sb.setup(
        Some(ZSH_SHELL),
        &["--yes", "--only", "completions", "--dry-run"],
    );

    assert_eq!(sb.snapshot(), before);
    assert!(!sb.home.join(ZSHRC).exists());
    assert!(text(&out).contains("completions"), "{}", text(&out));
}

#[test]
fn a_stub_path_without_the_marker_is_left_alone() {
    let sb = Sandbox::new("not-ours");
    let mine = stub_paths(&sb.home, Shell::Bash).remove(0);
    fs::create_dir_all(mine.parent().expect("parent")).expect("mkdir");
    fs::write(&mine, "# my own completion\n").expect("seed");

    let out = sb.only(BASH_SHELL);

    assert_eq!(sb.read(&mine), "# my own completion\n");
    assert!(text(&out).contains(NOT_OURS_NOTE), "{}", text(&out));
}

#[test]
fn an_unknown_shell_writes_nothing_and_says_why() {
    for shell in [Some("/bin/tcsh"), None] {
        let sb = Sandbox::new("unknown-shell");
        let before = sb.snapshot();

        let out = sb.setup(shell, &["--yes", "--only", "completions"]);

        assert_eq!(sb.snapshot(), before, "{shell:?}");
        assert!(text(&out).contains(SKIP_UNKNOWN_SHELL), "{}", text(&out));
    }
}

#[test]
fn a_read_only_loader_dir_is_skipped_with_a_reason() {
    let sb = Sandbox::new("read-only");
    let dir = loader_path(&sb.home)
        .parent()
        .expect("parent")
        .to_path_buf();
    fs::create_dir_all(&dir).expect("mkdir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(READ_ONLY_DIR_MODE)).expect("chmod");
    let probe = dir.join("probe");
    if fs::write(&probe, "").is_ok() {
        // The mode does not bind this user (root), so there is nothing to test.
        let _ = fs::remove_file(&probe);
        return;
    }

    let out = sb.only(ZSH_SHELL);

    let entries = fs::read_dir(&dir).expect("read dir").count();
    fs::set_permissions(&dir, fs::Permissions::from_mode(RESTORED_DIR_MODE)).expect("chmod back");
    assert_eq!(entries, 0, "the dir stays empty");
    assert!(text(&out).contains("skipped"), "{}", text(&out));
}

#[test]
fn setup_does_not_need_zsh_or_fish_on_path() {
    for (shell, which) in [(ZSH_SHELL, Shell::Zsh), (FISH_SHELL, Shell::Fish)] {
        let sb = Sandbox::new("no-shell-binary");
        assert_eq!(fs::read_dir(&sb.bin).expect("bin").count(), 1);

        sb.only(shell);

        assert!(first_file(&sb.home, which).is_file());
    }
}

#[test]
fn dry_run_and_explain_describe_the_part_and_write_nothing() {
    let sb = Sandbox::new("explain");
    let before = sb.snapshot();

    let explain = sb.setup(Some(ZSH_SHELL), &["--explain", "completions"]);
    let dry = sb.setup(Some(ZSH_SHELL), &["--yes", "--dry-run"]);

    assert_eq!(sb.snapshot(), before);
    let explained = text(&explain);
    assert!(explained.contains("completions"), "{explained}");
    assert!(explained.contains("--no-completions"), "{explained}");
    assert!(text(&dry).contains("completions"), "{}", text(&dry));
}

#[test]
fn completions_part_is_off_without_a_terminal_unless_yes_or_only() {
    let plain = Sandbox::new("no-terminal");
    plain.setup(Some(ZSH_SHELL), &[]);
    assert!(!first_file(&plain.home, Shell::Zsh).exists());
    assert!(!plain.home.join(ZSHRC).exists());

    let yes = Sandbox::new("yes");
    yes.setup(Some(ZSH_SHELL), &["--yes"]);
    assert!(first_file(&yes.home, Shell::Zsh).is_file());

    let only = Sandbox::new("only");
    only.setup(Some(ZSH_SHELL), &["--only", "completions"]);
    assert!(first_file(&only.home, Shell::Zsh).is_file());

    let declined = Sandbox::new("declined");
    declined.setup(Some(ZSH_SHELL), &["--yes", "--no-completions"]);
    assert!(!first_file(&declined.home, Shell::Zsh).exists());
}

#[test]
fn only_completions_conflicts_with_its_off_switch() {
    let sb = Sandbox::new("conflict");

    let out = sb
        .command(
            Some(ZSH_SHELL),
            &["--only", "completions", "--no-completions"],
        )
        .output()
        .expect("spawns");

    assert!(!out.status.success());
    assert!(text(&out).contains("--no-completions"), "{}", text(&out));
}
