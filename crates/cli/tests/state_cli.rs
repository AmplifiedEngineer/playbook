// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `state` table end to end: `playbook state list` and the init migration
//! that imports the old `migrations.state` and sweep marker files.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-state-cli-{tag}-{}-{n}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".config/playbook")).unwrap();
    fs::create_dir_all(dir.join(".claude")).unwrap();
    dir
}

fn run(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .env("HOME", home)
        .env("SHELL", "/bin/zsh")
        .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
        .current_dir(home)
        .output()
        .expect("playbook should spawn")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn list_is_empty_on_a_fresh_home_and_filters_by_prefix() {
    let h = home("list");
    let out = run(&h, &["state", "list"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");

    let root = h.join(".config/playbook");
    playbook::state::set(&root, "a/1", "x").unwrap();
    playbook::state::set(&root, "b/1", "y").unwrap();
    let out = run(&h, &["state", "list", "a/"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "a/1\tx");
    let out = run(&h, &["state", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["b/1"], "y");
}

#[test]
fn init_imports_the_old_files_once_and_keeps_them_renamed() {
    let h = home("import");
    let root = h.join(".config/playbook");
    fs::write(
        root.join("migrations.state"),
        "applied 0001-old\nshipped system-prompt abc123\n",
    )
    .unwrap();
    fs::write(root.join("worktree-sweep-marker-_tmp_r"), "1700000000").unwrap();

    let out = run(
        &h,
        &[
            "setup",
            "--yes",
            "--no-plugin",
            "--no-hooks",
            "--no-statusline",
            "--no-path",
            "--no-system-prompt",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("imported 2 old state file(s)"),
        "{}",
        text(&out)
    );
    assert!(root.join("migrations.state.migrated").is_file());
    assert!(root.join("worktree-sweep-marker-_tmp_r.migrated").is_file());
    assert!(!root.join("migrations.state").exists());

    let rows = text(&run(&h, &["state", "list"]));
    assert!(rows.contains("migrations/applied/0001-old\t1"), "{rows}");
    assert!(
        rows.contains("migrations/shipped/system-prompt\tabc123"),
        "{rows}"
    );
    assert!(rows.contains("worktree-sweep/_tmp_r\t1700000000"), "{rows}");

    let again = run(
        &h,
        &[
            "setup",
            "--yes",
            "--no-hooks",
            "--no-statusline",
            "--no-path",
            "--no-system-prompt",
        ],
    );
    assert!(!text(&again).contains("imported"), "{}", text(&again));
}

#[test]
fn the_auto_guard_still_protects_the_database_that_holds_the_state() {
    // The state table lives in config.db, which the auto mode guard already
    // blocks the model from touching (src/hooks/auto_cost.rs).
    let src = include_str!("../src/hooks/auto_cost.rs");
    assert!(
        src.contains("config.db-wal"),
        "the guard must name the db files"
    );
}
