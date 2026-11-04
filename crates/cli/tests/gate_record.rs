// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-spawn tests for `playbook gate record`. These prove the CLI-level
//! half of the plan's claims that unit tests on `record::run` alone cannot:
//! a bad input must exit non-zero AND leave the database untouched, and
//! re-recording the same `(plan_slug, phase)` must overwrite, not
//! duplicate. Both need a real DB file and exit code to check against.

use playbook::gate::{db, hash, record};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// `record::run` shells out through `manifest::check::toplevel` and
/// `paths::home_dir`, both of which read the process's own cwd/`$HOME`
/// rather than an injectable parameter, so a direct-call test must serialise
/// on those process-wide values the way `tests/worktree_sweep.rs`'s
/// `CWD_LOCK` does.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `f` with the process cwd set to `repo` and `$HOME` set to `home`,
/// restoring both afterward, for the tests below that call `record::run`
/// in-process rather than spawning the compiled binary.
fn with_repo_and_home<T>(repo: &Path, home: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = lock_env();
    let prev_cwd = std::env::current_dir().expect("read current dir");
    let prev_home = std::env::var_os("HOME");
    std::env::set_current_dir(repo).expect("cd into repo");
    std::env::set_var("HOME", home);
    let out = f();
    std::env::set_current_dir(&prev_cwd).expect("restore cwd");
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    out
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    /// A scratch git repo with an `origin` remote, plus its own scratch
    /// `$HOME`, so every fixture is isolated from the real machine and every other fixture.
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-gate-record-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(&dir).expect("scratch repo should be creatable");
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&dir)
            .status()
            .expect("git init should run");
        assert!(init.success(), "git init should succeed");
        let remote = Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/test-owner/test-repo.git",
            ])
            .current_dir(&dir)
            .status()
            .expect("git remote add should run");
        assert!(remote.success(), "git remote add should succeed");
        let repo = dir.canonicalize().expect("scratch repo should resolve");

        let home = std::env::temp_dir().join(format!(
            "playbook-gate-record-home-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(&home).expect("scratch home should be creatable");

        Self { repo, home }
    }

    /// A repo fixture with no `origin` remote, so worktree scoping cannot resolve: covers the silent-fallback error path.
    fn new_without_origin(tag: &str) -> Self {
        let f = Self::new(tag);
        let remove = Command::new("git")
            .args(["remote", "remove", "origin"])
            .current_dir(&f.repo)
            .status()
            .expect("git remote remove should run");
        assert!(remove.success(), "git remote remove should succeed");
        f
    }

    fn write_input(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.repo.join(name);
        fs::write(&path, contents).expect("input fixture should write");
        path
    }

    /// `--source` reuses `input`: both flags accept arbitrary content and
    /// these tests have no reason for the two to differ.
    fn run(
        &self,
        plan_slug: &str,
        command: &str,
        phase: &str,
        input: &PathBuf,
    ) -> std::process::Output {
        self.command(plan_slug, command, phase, input)
            .output()
            .expect("playbook binary should spawn")
    }

    fn command(&self, plan_slug: &str, command: &str, phase: &str, input: &PathBuf) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.args(["gate", "record", plan_slug, command, phase])
            .arg(input)
            .arg("--source")
            .arg(input)
            .current_dir(&self.repo)
            .env("HOME", &self.home);
        cmd
    }

    /// Where earlier versions kept the gate database.
    fn old_scoped_dir(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("playbook")
            .join("repos")
            .join("test-owner")
            .join("test-repo")
            .join(worktree_id(&self.repo))
    }

    fn db_path(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("playbook")
            .join("data")
            .join("gate")
            .join("test-owner")
            .join("test-repo")
            .join(format!("{}.db", worktree_id(&self.repo)))
    }
}

/// Mirrors `paths::worktree_id`'s slugify rule independently (not by
/// calling the production function), so this stays a real check, not a tautology.
fn worktree_id(repo: &std::path::Path) -> String {
    repo.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn no_verdict_line_exits_nonzero_and_writes_nothing() {
    // Arrange
    let f = Fixture::new("no-verdict");
    let input = f.write_input("report.md", "Nothing conclusive to report here.");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}: {}",
        out.status.code(),
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("gate record:"),
        "got: {}",
        stderr_of(&out)
    );
    let row = db::open_db(&f.db_path())
        .and_then(|conn| db::query_phase(&conn, "plan-a", "spec"))
        .expect("open/query should not error even though nothing was ever written");
    assert!(
        row.is_none(),
        "no row should have been written for an input with no VERDICT line"
    );
}

#[test]
fn invalid_verdict_value_exits_nonzero_and_writes_nothing() {
    // Arrange
    let f = Fixture::new("invalid-verdict");
    let input = f.write_input("report.md", "VERDICT: PASSING");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}: {}",
        out.status.code(),
        stderr_of(&out)
    );
    let row = db::open_db(&f.db_path())
        .and_then(|conn| db::query_phase(&conn, "plan-a", "spec"))
        .expect("open/query should not error even though nothing was ever written");
    assert!(
        row.is_none(),
        "a value outside the 4 keywords must not be recorded"
    );
}

#[test]
fn re_recording_same_plan_and_phase_overwrites_not_duplicates() {
    // Arrange
    let f = Fixture::new("re-record");
    let first_input = f.write_input("first.md", "VERDICT: WARN");
    let second_input = f.write_input("second.md", "VERDICT: PASS");

    // Act
    let first = f.run("plan-a", "spec-review", "spec", &first_input);
    assert_eq!(
        first.status.code(),
        Some(0),
        "first record should succeed: {}",
        stderr_of(&first)
    );
    let second = f.run("plan-a", "spec-review-2", "spec", &second_input);

    // Assert
    assert_eq!(
        second.status.code(),
        Some(0),
        "second record should succeed: {}",
        stderr_of(&second)
    );
    let conn = db::open_db(&f.db_path()).expect("open db after two records");
    let row = db::query_phase(&conn, "plan-a", "spec")
        .expect("query should not error")
        .expect("row should exist after recording");
    assert_eq!(
        row.verdict, "PASS",
        "only the latest verdict should be present"
    );
}

#[test]
fn state_db_resolves_under_the_new_repo_and_worktree_scoped_path() {
    // Arrange
    let f = Fixture::new("resolves-scoped-path");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0: {}",
        stderr_of(&out)
    );
    assert!(
        f.db_path().is_file(),
        "expected the gate database in the data folder at {:?}",
        f.db_path()
    );
}

#[test]
fn gate_record_errors_when_worktree_scoping_cannot_resolve() {
    // Arrange: no `origin` remote configured.
    let f = Fixture::new_without_origin("no-origin");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected a hard error, not a silent repo-local fallback: {}",
        stderr_of(&out)
    );
    assert!(
        !f.repo.join(".claude").join("state.db").exists(),
        "must never fall back to writing a repo-local state.db"
    );
}

#[test]
fn legacy_claude_state_db_migrates_on_first_gate_record_call() {
    // Arrange: a legacy `.claude/state.db` and `.claude/plans/` from a
    // pre-ADR-0012 install, seeded directly on disk.
    let f = Fixture::new("legacy-migrates");
    let legacy = f.repo.join(".claude");
    let legacy_db_path = legacy.join("state.db");
    let legacy_conn = db::open_db(&legacy_db_path).expect("seed a real legacy state.db");
    db::upsert_phase(
        &legacy_conn,
        "plan-old",
        "spec",
        "PASS",
        "pre-migration evidence",
        "old-cmd",
        "2026-01-01T00:00:00Z",
        "test-hash",
    )
    .expect("seed pre-migration row");
    drop(legacy_conn);
    fs::create_dir_all(legacy.join("plans")).expect("mkdir legacy plans");
    fs::write(legacy.join("plans").join("x.md"), "plan content").expect("seed legacy plan");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0: {}",
        stderr_of(&out)
    );
    assert!(
        !legacy_db_path.exists(),
        "the legacy state.db should have been moved, not left behind"
    );
    assert_eq!(
        fs::read_to_string(f.old_scoped_dir().join("plans").join("x.md")).unwrap(),
        "plan content",
        "plans/ content must survive the migration"
    );
    let conn = db::open_db(&f.db_path()).expect("open migrated+recorded db");
    let old_row = db::query_phase(&conn, "plan-old", "spec")
        .expect("query")
        .expect("the pre-migration row must survive the move");
    assert_eq!(old_row.verdict, "PASS");
    let new_row = db::query_phase(&conn, "plan-a", "spec")
        .expect("query")
        .expect("this call's own recording should still have landed");
    assert_eq!(new_row.verdict, "PASS");
}

#[test]
fn run_records_source_hash_matching_hash_hex_of_source_content() {
    // Arrange
    let f = Fixture::new("source-hash-direct");
    let input = f.write_input("report.md", "VERDICT: PASS");
    let source_content = "content A";
    let source = f.write_input("source-a.txt", source_content);

    // Act
    let result = with_repo_and_home(&f.repo, &f.home, || {
        record::run(
            "plan-a",
            "spec-review",
            "spec",
            input.to_str().expect("utf8 path"),
            source.to_str().expect("utf8 path"),
        )
    });

    // Assert
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let conn = db::open_db(&f.db_path()).expect("open db after run");
    let row = db::query_phase(&conn, "plan-a", "spec")
        .expect("query should not error")
        .expect("row should exist after run");
    assert_eq!(
        row.source_hash,
        Some(hash::hash_hex(source_content.as_bytes()))
    );
}

#[test]
fn re_running_with_different_source_content_overwrites_the_stored_hash() {
    // Arrange
    let f = Fixture::new("source-hash-overwrite");
    let first_input = f.write_input("first.md", "VERDICT: WARN");
    let first_source = f.write_input("source-first.txt", "content A");
    let second_input = f.write_input("second.md", "VERDICT: PASS");
    let second_source_content = "content B";
    let second_source = f.write_input("source-second.txt", second_source_content);

    // Act
    let (first_result, second_result) = with_repo_and_home(&f.repo, &f.home, || {
        let first = record::run(
            "plan-a",
            "spec-review",
            "spec",
            first_input.to_str().expect("utf8 path"),
            first_source.to_str().expect("utf8 path"),
        );
        let second = record::run(
            "plan-a",
            "spec-review-2",
            "spec",
            second_input.to_str().expect("utf8 path"),
            second_source.to_str().expect("utf8 path"),
        );
        (first, second)
    });

    // Assert
    assert!(
        first_result.is_ok(),
        "first run should succeed: {first_result:?}"
    );
    assert!(
        second_result.is_ok(),
        "second run should succeed: {second_result:?}"
    );
    let conn = db::open_db(&f.db_path()).expect("open db after two runs");
    let row = db::query_phase(&conn, "plan-a", "spec")
        .expect("query should not error")
        .expect("row should exist after two runs");
    assert_eq!(
        row.source_hash,
        Some(hash::hash_hex(second_source_content.as_bytes())),
        "the second run's source hash should have overwritten the first"
    );
}

#[test]
fn run_with_a_nonexistent_source_path_errors_and_leaves_the_database_untouched() {
    // Arrange
    let f = Fixture::new("source-missing");
    let input = f.write_input("report.md", "VERDICT: PASS");
    let missing_source = f.repo.join("does-not-exist.txt");

    // Act
    let result = with_repo_and_home(&f.repo, &f.home, || {
        record::run(
            "plan-a",
            "spec-review",
            "spec",
            input.to_str().expect("utf8 path"),
            missing_source.to_str().expect("utf8 path"),
        )
    });

    // Assert
    assert!(result.is_err(), "expected Err for a missing source path");
    let row = db::open_db(&f.db_path())
        .and_then(|conn| db::query_phase(&conn, "plan-a", "spec"))
        .expect("open/query should not error even though nothing was ever written");
    assert!(
        row.is_none(),
        "no row should have been written when the source path does not exist"
    );
}

#[test]
fn stuck_flag_stores_stuck_and_rounds_count_up_to_it() {
    // Arrange
    let f = Fixture::new("stuck-rounds");
    let fail = f.write_input("fail.md", "VERDICT: FAIL");
    let source = f.write_input("source.txt", "content");
    let (fail_path, source_path) = (fail.to_str().unwrap(), source.to_str().unwrap());

    // Act
    let rounds = with_repo_and_home(&f.repo, &f.home, || {
        let first = record::run_with(
            "plan-a",
            "plan",
            "adversarial",
            fail_path,
            source_path,
            false,
        );
        let second = record::run_with(
            "plan-a",
            "plan",
            "adversarial",
            fail_path,
            source_path,
            true,
        );
        (first, second)
    });

    // Assert
    assert_eq!(rounds, (Ok(1), Ok(2)));
    let conn = db::open_db(&f.db_path()).expect("open db");
    let row = db::query_phase(&conn, "plan-a", "adversarial")
        .unwrap()
        .unwrap();
    assert_eq!(row.verdict, "STUCK");
}

fn seed_row(path: &Path, plan: &str, verdict: &str) {
    let conn = db::open_db(path).expect("seed db");
    db::upsert_phase(
        &conn,
        plan,
        "spec",
        verdict,
        "e",
        "c",
        "2026-01-01T00:00:00Z",
        "h",
    )
    .expect("seed row");
}

#[test]
fn an_old_worktree_state_db_moves_into_the_data_folder() {
    // Arrange
    let f = Fixture::new("data-move");
    let old = f.old_scoped_dir().join("state.db");
    seed_row(&old, "plan-old", "PASS");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(!old.exists(), "the old file is set aside");
    assert!(old.with_file_name("state.db.migrated").exists());
    let conn = db::open_db(&f.db_path()).expect("open new");
    assert!(db::query_phase(&conn, "plan-old", "spec")
        .unwrap()
        .is_some());
    assert!(db::query_phase(&conn, "plan-a", "spec").unwrap().is_some());
}

#[test]
fn old_and_new_gate_databases_merge_without_double_counting() {
    // Arrange: a database in both places, one shared key.
    let f = Fixture::new("data-merge");
    seed_row(&f.db_path(), "shared", "PASS");
    seed_row(&f.db_path(), "only-new", "PASS");
    let old = f.old_scoped_dir().join("state.db");
    seed_row(&old, "shared", "FAIL");
    seed_row(&old, "only-old", "WARN");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act: twice, the second must change nothing.
    let first = f.run("plan-a", "spec-review", "spec", &input);
    let second = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(first.status.code(), Some(0), "{}", stderr_of(&first));
    assert_eq!(second.status.code(), Some(0), "{}", stderr_of(&second));
    let conn = db::open_db(&f.db_path()).expect("open");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM gate_phases", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "shared, only-new, only-old and plan-a");
    assert_eq!(
        db::query_phase(&conn, "shared", "spec")
            .unwrap()
            .unwrap()
            .verdict,
        "PASS",
        "the row already in the data folder wins"
    );
}

#[test]
fn a_corrupt_old_state_db_is_left_in_place_and_recording_still_works() {
    // Arrange
    let f = Fixture::new("data-corrupt");
    let old = f.old_scoped_dir().join("state.db");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, vec![b'x'; 4096]).unwrap();
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(old.exists(), "a corrupt file is never touched");
    assert!(f.db_path().is_file());
}

#[test]
fn no_old_database_means_no_leftovers() {
    // Arrange
    let f = Fixture::new("data-none");
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(!f.old_scoped_dir().join("state.db").exists());
    assert!(!f.old_scoped_dir().join("state.db.migrated").exists());
}

#[test]
fn an_old_schema_state_db_moves_and_keeps_its_rows() {
    // Arrange: the schema before source_hash and STUCK existed.
    let f = Fixture::new("data-old-schema");
    let old = f.old_scoped_dir().join("state.db");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    let raw = rusqlite::Connection::open(&old).unwrap();
    raw.execute_batch(
        "CREATE TABLE gate_phases (plan_slug TEXT NOT NULL, phase TEXT NOT NULL,
           verdict TEXT NOT NULL CHECK(verdict IN ('PASS','FAIL','WARN','INCONCLUSIVE')),
           evidence TEXT NOT NULL, command TEXT NOT NULL, recorded_at TEXT NOT NULL,
           PRIMARY KEY (plan_slug, phase)) WITHOUT ROWID;
         INSERT INTO gate_phases VALUES ('plan-old','spec','PASS','e','c','2026-01-01T00:00:00Z');",
    )
    .unwrap();
    drop(raw);
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let conn = db::open_db(&f.db_path()).expect("open");
    let row = db::query_phase(&conn, "plan-old", "spec")
        .unwrap()
        .expect("old row");
    assert_eq!(row.verdict, "PASS");
    assert!(db::query_phase(&conn, "plan-a", "spec").unwrap().is_some());
}

#[test]
fn a_pending_move_defers_to_the_old_file_and_a_later_call_merges_it() {
    // Arrange: another process holds the migrate lock.
    let f = Fixture::new("data-deferred");
    let old = f.old_scoped_dir().join("state.db");
    seed_row(&old, "plan-old", "PASS");
    let lock = f.db_path().with_extension("db.migrate.lock");
    fs::create_dir_all(&lock).unwrap();
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act: a short lock budget, so the busy call gives up in milliseconds.
    let busy = f
        .command("plan-a", "spec-review", "spec", &input)
        .env(playbook_data::migrate::TEST_LOCK_RETRIES_ENV, "2")
        .output()
        .expect("playbook binary should spawn");
    fs::remove_dir(&lock).unwrap();
    let later = f.run("plan-b", "spec-review", "spec", &input);

    // Assert: the busy call used the old file, nothing was hidden.
    assert_eq!(busy.status.code(), Some(0), "{}", stderr_of(&busy));
    assert_eq!(later.status.code(), Some(0), "{}", stderr_of(&later));
    assert!(!old.exists(), "moved by the later call");
    let conn = db::open_db(&f.db_path()).expect("open");
    for plan in ["plan-old", "plan-a", "plan-b"] {
        assert!(
            db::query_phase(&conn, plan, "spec").unwrap().is_some(),
            "{plan}"
        );
    }
}

#[test]
fn a_later_phase_row_in_the_old_file_wins_the_merge() {
    // Arrange: an older playbook recorded a newer verdict at the old path.
    let f = Fixture::new("data-newer");
    let new_conn = db::open_db(&f.db_path()).expect("new");
    db::upsert_phase(
        &new_conn,
        "shared",
        "spec",
        "PASS",
        "e",
        "c",
        "2026-01-01T00:00:00Z",
        "h",
    )
    .unwrap();
    drop(new_conn);
    let old = f.old_scoped_dir().join("state.db");
    let old_conn = db::open_db(&old).expect("old");
    db::upsert_phase(
        &old_conn,
        "shared",
        "spec",
        "FAIL",
        "e",
        "c",
        "2026-02-01T00:00:00Z",
        "h",
    )
    .unwrap();
    drop(old_conn);
    let input = f.write_input("report.md", "VERDICT: PASS");

    // Act
    let out = f.run("plan-a", "spec-review", "spec", &input);

    // Assert
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let conn = db::open_db(&f.db_path()).expect("open");
    let row = db::query_phase(&conn, "shared", "spec").unwrap().unwrap();
    assert_eq!(row.verdict, "FAIL");
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn the_round_shows_the_default_cap_of_five() {
    // Arrange
    let f = Fixture::new("round-default-cap");
    let input = f.write_input("report.md", "VERDICT: FAIL");

    // Act
    let out = f.run("plan-a", "plan", "adversarial", &input);

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("(round 1 of 5)"),
        "{}",
        stdout_of(&out)
    );
}

#[test]
fn the_round_shows_a_configured_cap() {
    // Arrange
    let f = Fixture::new("round-set-cap");
    let input = f.write_input("report.md", "VERDICT: FAIL");
    let set = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["config", "set", "--global", "gate.maxRounds", "8"])
        .current_dir(&f.repo)
        .env("HOME", &f.home)
        .output()
        .expect("playbook binary should spawn");
    assert!(set.status.success(), "{}", stderr_of(&set));

    // Act
    let out = f.run("plan-a", "plan", "adversarial", &input);

    // Assert
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("(round 1 of 8)"),
        "{}",
        stdout_of(&out)
    );
}
