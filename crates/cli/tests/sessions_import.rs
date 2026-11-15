// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `sessions-runtime-files-to-db` migration, run against a temporary
//! `HOME`. The entry is unreleased (`since` is `next`), so `playbook migrate`
//! does not run it yet: the tests run it through the registry in process and
//! fire the hooks as the binary. Rows are compared by value, never by the bytes
//! of `sessions.db`, because WAL checkpointing can change the file.

use playbook::init::migrate::{registry, run_with, Ctx};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/isolated.rs"]
mod isolated;

fn scratch(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "pb-sessions-import-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    // `/var` is a symlink on macOS, and the child reports resolved paths.
    fs::canonicalize(&home).unwrap()
}

/// The hook child: an isolated home, no host git config, and a `gh` that
/// fails first on `PATH`, so a stray lookup reads as a failed one and never
/// reaches the host or the network.
fn base(home: &Path) -> Command {
    let bin = home.join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut cmd = isolated::playbook_in(home);
    cmd.current_dir(home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("HOOK_INPUT")
        .env_remove("CI");
    cmd
}

/// Printed only when the import hit a database error.
const RETRY_NOTE: &str = "will retry on the next run";

/// What a migrate run printed and whether it succeeded.
struct Run {
    ok: bool,
    text: String,
}

fn migrate(home: &Path) -> Run {
    let entry = registry()
        .into_iter()
        .find(|m| m.id == "sessions-runtime-files-to-db")
        .expect("the registry holds the sessions import");
    let report = run_with(&[entry], &Ctx::for_home(home));
    Run {
        ok: !report.failed,
        text: report
            .steps
            .iter()
            .map(|s| s.render())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn prompt_hook(home: &Path, id: &str) {
    let mut child = base(home)
        .args(["hook", "session-activity"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let payload = format!(r#"{{"session_id":"{id}","hook_event_name":"UserPromptSubmit"}}"#);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

fn runtime(home: &Path, id: &str) -> PathBuf {
    let dir = playbook::common::paths::playbook_root_from(home)
        .join("runtime")
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn db(home: &Path) -> PathBuf {
    playbook_data::data_dir_from(home).join("sessions.db")
}

/// Every column of the row of `id`, as text, in column order.
fn row(home: &Path, id: &str) -> Vec<Option<String>> {
    let conn = playbook_store::Connection::open(db(home)).unwrap();
    conn.query_row(
        "SELECT agent, cwd, CAST(started_at AS TEXT), CAST(last_prompt_at AS TEXT),
                CAST(last_stop_at AS TEXT), CAST(waiting_since AS TEXT),
                CAST(waiting_after_prompt AS TEXT), CAST(ended_at AS TEXT), end_reason
         FROM sessions WHERE session_id = ?1",
        [id],
        |r| (0..9).map(|i| r.get::<_, Option<String>>(i)).collect(),
    )
    .unwrap()
}

fn meta_has(home: &Path, key: &str) -> bool {
    let conn = playbook_store::Connection::open(db(home)).unwrap();
    conn.query_row("SELECT COUNT(*) FROM meta WHERE key = ?1", [key], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap()
        == 1
}

fn ended_at(home: &Path, id: &str) -> Option<i64> {
    let conn = playbook_store::Connection::open(db(home)).unwrap();
    conn.query_row(
        "SELECT ended_at FROM sessions WHERE session_id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn migrate_imports_an_old_session_folder_once() {
    // Arrange
    let home = scratch("once");
    let dir = runtime(&home, "s1");
    fs::write(dir.join("start-ts"), "500").unwrap();
    fs::write(dir.join("clean-exit"), "logout\n").unwrap();

    // Act
    assert!(migrate(&home).ok);
    let first = row(&home, "s1");
    assert!(migrate(&home).ok);

    // Assert
    assert_eq!(row(&home, "s1"), first);
    assert_eq!(first[2].as_deref(), Some("500"));
    assert_eq!(first[8].as_deref(), Some("logout"));
    assert!(meta_has(&home, "runtime_import"));
    assert!(dir.join("start-ts").is_file());
    assert!(dir.join("clean-exit").is_file());
}

#[test]
fn a_home_with_no_session_folders_gets_no_database() {
    // Arrange
    let home = scratch("none");

    // Act
    let out = migrate(&home);

    // Assert
    assert!(out.ok);
    assert!(!db(&home).exists());
}

#[test]
fn a_corrupt_database_leaves_the_counter_files_as_a_healthy_run_has_them() {
    // Arrange
    let mut counters = Vec::new();
    for corrupt in [false, true] {
        let home = scratch(if corrupt {
            "corrupt-counters"
        } else {
            "healthy-counters"
        });
        let dir = runtime(&home, "s1");
        fs::write(dir.join("start-ts"), "500").unwrap();
        fs::write(dir.join("search-count"), "3").unwrap();
        fs::write(dir.join("tool-count"), "7").unwrap();
        if corrupt {
            fs::create_dir_all(db(&home).parent().unwrap()).unwrap();
            fs::write(db(&home), "not a database").unwrap();
        }

        // Act
        let out = migrate(&home);

        // Assert
        assert_eq!(out.text.contains(RETRY_NOTE), corrupt, "{}", out.text);
        counters.push((
            fs::read_to_string(dir.join("search-count")).unwrap(),
            fs::read_to_string(dir.join("tool-count")).unwrap(),
        ));
    }
    assert_eq!(counters[0], counters[1]);
}

#[test]
fn a_corrupt_database_exits_zero_and_the_report_names_the_error() {
    // Arrange
    let home = scratch("corrupt-report");
    fs::write(runtime(&home, "s1").join("start-ts"), "500").unwrap();
    fs::create_dir_all(db(&home).parent().unwrap()).unwrap();
    fs::write(db(&home), "not a database").unwrap();

    // Act
    let out = migrate(&home);

    // Assert
    assert!(out.ok);
    let text = &out.text;
    assert!(text.contains(RETRY_NOTE), "{text}");
    assert!(!text.contains("imported"), "{text}");
}

#[test]
fn a_corrupt_activity_file_still_imports_the_start_time() {
    // Arrange
    let home = scratch("bad-activity");
    let dir = runtime(&home, "s1");
    fs::write(dir.join("start-ts"), "500").unwrap();
    fs::write(dir.join("activity.json"), "not json").unwrap();

    // Act
    let out = migrate(&home);

    // Assert
    assert!(out.ok);
    let r = row(&home, "s1");
    assert_eq!(r[2].as_deref(), Some("500"));
    assert_eq!(r[5], None);
}

#[test]
fn an_unreadable_folder_is_named_and_the_others_are_imported() {
    // Arrange
    let home = scratch("skip");
    fs::write(runtime(&home, "a1").join("start-ts"), "500").unwrap();
    fs::create_dir(runtime(&home, "b2").join("start-ts")).unwrap();

    // Act
    let out = migrate(&home);

    // Assert
    assert!(out.ok);
    let text = &out.text;
    assert!(text.contains("b2"), "{text}");
    assert_eq!(row(&home, "a1")[2].as_deref(), Some("500"));
    assert!(meta_has(&home, "runtime_import"));
}

#[test]
fn a_resumed_session_keeps_its_ended_at_cleared_after_a_later_migrate() {
    // Arrange
    let home = scratch("resume");
    fs::write(runtime(&home, "s1").join("clean-exit"), "logout").unwrap();
    assert!(migrate(&home).ok);
    assert!(ended_at(&home, "s1").is_some());

    // Act
    prompt_hook(&home, "s1");
    assert!(migrate(&home).ok);

    // Assert
    assert_eq!(ended_at(&home, "s1"), None);
}
