// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-level tests for `playbook sessions`, run against an isolated scratch home.
//! The fixture history is older than the default window, so it lists nothing.

#[path = "support/isolated.rs"]
mod isolated;

use playbook::sessions::{RECENT_DAYS, STALE_AFTER_DAYS, WORKING_SECONDS};
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn run(args: &[&str]) -> (bool, String) {
    let home = Home::new();
    let project = home.0.join(".claude/projects/proj-one");
    fs::create_dir_all(&project).unwrap();
    let fixtures = std::path::Path::new(env!("PLAYBOOK_REPO_ROOT"))
        .join("crates/telemetry/tests/fixtures/usage");
    fs::copy(
        fixtures.join("usage/proj-one/s1.jsonl"),
        project.join("s1.jsonl"),
    )
    .unwrap();
    fs::copy(
        fixtures.join("account/claude.json"),
        home.0.join(".claude.json"),
    )
    .unwrap();
    let out = home.command(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn history_older_than_the_window_lists_no_sessions() {
    let (ok, text) = run(&["sessions"]);
    assert!(ok);
    assert_eq!(text, "No recent sessions.\n");
}

#[test]
fn json_has_a_generation_time_and_an_empty_session_list() {
    let (ok, text) = run(&["sessions", "--json"]);
    assert!(ok);
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc["generated_at"].as_i64().unwrap() > 0);
    assert_eq!(doc["sessions"].as_array().unwrap().len(), 0);
}

#[test]
fn the_list_subcommand_is_the_same_as_the_bare_command() {
    assert_eq!(run(&["sessions", "list"]), run(&["sessions"]));
}

#[test]
fn the_sweep_dry_run_reports_and_names_nothing_to_close() {
    let (ok, text) = run(&["sessions", "sweep", "--dry-run"]);
    assert!(ok);
    assert_eq!(
        text,
        "Dry run: nothing is removed, stopped or closed.\nNo finished or stale sessions.\n"
    );
}

#[test]
fn the_sweep_refuses_to_run_without_the_dry_run_flag() {
    let (ok, text) = run(&["sessions", "sweep"]);
    assert!(!ok);
    assert!(text.is_empty());
}

/// Past the working boundary and inside the stale limit.
const FAR_AGO_SECS: i64 = WORKING_SECONDS * 40;
/// Far inside the working boundary, so the clock cannot flip a state.
const RECENT_SECS: i64 = WORKING_SECONDS / 20;
/// Past the stale limit and inside the recent window.
const STALE_SECS: i64 = (STALE_AFTER_DAYS + RECENT_DAYS) / 2 * SECS_PER_DAY;
const SECS_PER_DAY: i64 = 86_400;

/// A scratch `$HOME` with a Claude project folder, removed on drop.
struct Home(std::path::PathBuf);

impl Home {
    fn new() -> Self {
        let n = CALLS.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "playbook-sessions-db-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(home.join(".claude/projects/p")).unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        Home(home.canonicalize().unwrap())
    }

    /// One assistant message of session `id`, `ago` seconds old. Returns its
    /// Unix time.
    fn event(&self, id: &str, ago: i64) -> i64 {
        let secs = now() - ago;
        let at = playbook::common::time::iso_utc(secs);
        let line = format!(
            r#"{{"type":"assistant","uuid":"u-{id}","sessionId":"{id}","timestamp":"{at}","cwd":"/nonexistent/{id}","gitBranch":"feat/x","message":{{"id":"m-{id}","model":"claude-sonnet-5","content":[],"usage":{{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}}}}}}"#
        );
        fs::write(self.0.join(format!(".claude/projects/p/{id}.jsonl")), line).unwrap();
        secs
    }

    fn db(&self) -> std::path::PathBuf {
        playbook_data::data_dir_from(&self.0).join("sessions.db")
    }

    /// The binary with `args`, isolated to this home.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        isolated::isolate(&mut cmd, &self.0);
        isolated::go_offline(&mut cmd, &self.0);
        cmd.args(args);
        cmd
    }

    fn sessions(&self) -> std::process::Output {
        let out = self.command(&["sessions", "--json"]).output().unwrap();
        assert!(out.status.success());
        out
    }

    fn sessions_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.sessions().stdout).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn now() -> i64 {
    playbook::common::time::now_secs()
}

#[test]
fn sessions_lists_a_waiting_session_from_the_database() {
    // Arrange
    let home = Home::new();
    home.event("w1", FAR_AGO_SECS);
    playbook::testing::seed_session_row(
        &home.0,
        "w1",
        &[
            ("started_at", now() - FAR_AGO_SECS),
            ("waiting_after_prompt", 1),
        ],
    );

    // Act
    let doc = home.sessions_json();

    // Assert
    assert_eq!(doc["sessions"][0]["state"], "waiting");
}

#[test]
fn sessions_lists_a_crashed_session_that_needs_attention() {
    // Arrange
    let home = Home::new();
    home.event("c1", RECENT_SECS);
    playbook::testing::seed_session_row(
        &home.0,
        "c1",
        &[
            ("started_at", now() - FAR_AGO_SECS),
            ("crashed_at", now() - FAR_AGO_SECS),
        ],
    );

    // Act
    let doc = home.sessions_json();

    // Assert
    assert_eq!(doc["sessions"][0]["state"], "crashed");
    assert_eq!(doc["sessions"][0]["needs_attention"], true);
}

#[test]
fn sessions_lists_a_closed_session_as_closed() {
    // Arrange
    let home = Home::new();
    home.event("x1", FAR_AGO_SECS);
    playbook::testing::seed_session_row(
        &home.0,
        "x1",
        &[
            ("started_at", now() - FAR_AGO_SECS),
            ("closed_at", now() - RECENT_SECS),
        ],
    );

    // Act
    let doc = home.sessions_json();

    // Assert
    assert_eq!(doc["sessions"][0]["state"], "closed");
}

#[test]
fn sessions_without_a_database_list_as_before_and_create_none() {
    // Arrange
    let home = Home::new();
    let at = home.event("n1", STALE_SECS);

    // Act
    let doc = home.sessions_json();

    // Assert
    let expected = serde_json::json!([{
        "id": "n1", "agent": "claude-code", "repo": "n1", "branch": "feat/x",
        "cwd": "/nonexistent/n1", "worktree": null, "worktree_convention": "",
        "worktree_gone": true, "uncommitted": null, "issue_inferred": null,
        "pr": null, "state": "stale", "last_activity": at, "context_tokens": 1,
        "cost_usd": 0.000012, "unpriced_messages": 0, "messages": 1,
        "needs_attention": false, "hint": "stale: archive or close this session"
    }]);
    assert_eq!(doc["sessions"], expected);
    assert!(!home.db().exists());
}

#[test]
fn a_session_with_events_and_no_row_is_listed_beside_one_with_a_row() {
    // Arrange
    let home = Home::new();
    home.event("r1", FAR_AGO_SECS);
    home.event("r2", FAR_AGO_SECS);
    playbook::testing::seed_session_row(
        &home.0,
        "r1",
        &[
            ("waiting_after_prompt", 1),
            ("started_at", now() - FAR_AGO_SECS),
        ],
    );

    // Act
    let doc = home.sessions_json();

    // Assert
    let state_of = |id: &str| {
        doc["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["state"].clone())
    };
    assert_eq!(state_of("r1"), Some("waiting".into()));
    assert_eq!(state_of("r2"), Some("idle".into()));
}

#[test]
fn an_unreadable_database_still_lists_with_one_stderr_line() {
    // Arrange
    let home = Home::new();
    home.event("b1", FAR_AGO_SECS);
    let db = home.db();
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, "not a database").unwrap();

    // Act
    let out = home.sessions();

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr,
        "playbook: failed to read sessions.db: file is not a database; listing sessions without it\n"
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["sessions"][0]["id"], "b1");
    assert!(!db.with_extension("db-wal").exists());
    assert!(!db.with_extension("db-shm").exists());
}
