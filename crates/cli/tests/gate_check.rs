// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-spawn tests for `playbook gate check`. `gate check` needs
//! pre-existing rows to query, so each fixture seeds them directly through
//! `playbook::gate::db` (the same in-process seeding precedent
//! `tests/gate_db.rs` uses) before spawning the binary for the invocation
//! under test.

use playbook::gate::{check, db, hash};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Content `seed`'s default hash and `Fixture::run`'s spawned `--source`
/// both derive from, so a seeded row resolves fresh rather than STALE.
const SEED_SOURCE_CONTENT: &str = "seed source content";

/// `check_in_process` mutates the process cwd and `$HOME`, both process-wide
/// state, so every call serialises through this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
            "playbook-gate-check-{tag}-{}-{n}",
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
            "playbook-gate-check-home-{tag}-{}-{n}",
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

    /// Seed a phase row directly through the library, bypassing `gate
    /// record`'s CLI: `gate check` needs pre-existing rows to query.
    fn seed(&self, plan_slug: &str, phase: &str, verdict: &str) {
        let source_hash = hash::hash_hex(SEED_SOURCE_CONTENT.as_bytes());
        self.seed_with_hash(plan_slug, phase, verdict, Some(&source_hash));
    }

    /// Seed a phase row with an explicit `source_hash`, or `None` to
    /// simulate a pre-migration row that predates the column.
    fn seed_with_hash(
        &self,
        plan_slug: &str,
        phase: &str,
        verdict: &str,
        source_hash: Option<&str>,
    ) {
        let conn = db::open_db(&self.db_path()).expect("open db for seed");
        match source_hash {
            Some(hash) => db::upsert_phase(
                &conn,
                plan_slug,
                phase,
                verdict,
                "evidence",
                "seed-cmd",
                "2026-01-01T00:00:00Z",
                hash,
            )
            .expect("seed upsert with hash"),
            None => {
                conn.execute(
                    "INSERT OR REPLACE INTO gate_phases \
                     (plan_slug, phase, verdict, evidence, command, recorded_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    playbook_store::params![
                        plan_slug,
                        phase,
                        verdict,
                        "evidence",
                        "seed-cmd",
                        "2026-01-01T00:00:00Z"
                    ],
                )
                .map(|_| ())
                .expect("seed raw insert without source_hash");
            }
        }
    }

    /// Calls `check::run` directly in this process, avoiding a binary spawn
    /// for scenarios that don't need to exercise the CLI layer itself.
    /// Serialises through `ENV_LOCK` since it mutates cwd and `$HOME`.
    fn check_in_process(
        &self,
        plan_slug: &str,
        command: &str,
        phases: &[String],
        source: &str,
    ) -> Result<String, String> {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_dir = std::env::current_dir().expect("read current dir");
        let previous_home = std::env::var_os("HOME");
        std::env::set_current_dir(&self.repo).expect("cd into fixture repo");
        std::env::set_var("HOME", &self.home);

        let result = check::run(plan_slug, command, phases, source);

        std::env::set_current_dir(&previous_dir).expect("restore cwd");
        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    /// Writes `source_content` to a file and passes it as `--source`. `run`
    /// delegates here with `SEED_SOURCE_CONTENT` so a seeded row resolves
    /// its intended verdict; a caller passing different content exercises a
    /// STALE mismatch through the real CLI.
    fn run_with_source(
        &self,
        plan_slug: &str,
        command: &str,
        phases: &[&str],
        source_content: &str,
    ) -> std::process::Output {
        let source = self.repo.join("seed-source.txt");
        fs::write(&source, source_content).expect("source fixture should write");
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["gate", "check", plan_slug, command])
            .args(phases)
            .arg("--source")
            .arg(&source)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .output()
            .expect("playbook binary should spawn")
    }

    /// Writes a source file matching `seed`'s stored hash and passes it as
    /// `--source`, so a seeded row resolves its intended verdict rather than
    /// STALE.
    fn run(&self, plan_slug: &str, command: &str, phases: &[&str]) -> std::process::Output {
        self.run_with_source(plan_slug, command, phases, SEED_SOURCE_CONTENT)
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

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn all_named_phases_pass_exits_zero() {
    // Arrange
    let f = Fixture::new("all-pass");
    f.seed("plan-a", "spec", "PASS");
    f.seed("plan-a", "impl", "PASS");

    // Act
    let out = f.run("plan-a", "gate-run", &["spec", "impl"]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0, got {:?}: {}",
        out.status.code(),
        stderr_of(&out)
    );
}

#[test]
fn warn_and_pass_mix_exits_zero_and_warn_line_is_distinguishable_from_pass() {
    // Arrange
    let f = Fixture::new("warn-pass");
    f.seed("plan-a", "spec", "PASS");
    f.seed("plan-a", "impl", "WARN");

    // Act
    let out = f.run("plan-a", "gate-run", &["spec", "impl"]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0: {}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    let warn_line = stdout
        .lines()
        .find(|line| line.starts_with("impl:"))
        .unwrap_or_else(|| panic!("no line for impl phase in: {stdout}"));
    assert!(
        warn_line.contains("WARN"),
        "warn phase's line must contain WARN: {warn_line}"
    );
    assert!(
        !warn_line.contains("PASS"),
        "warn phase's line must NOT contain PASS: {warn_line}"
    );
}

/// Table-driven, one case per state, matching the plan's exit-code table:
/// `[(Missing, 1), (Fail, 1), (Warn, 0), (Inconclusive, 1)]`. A single named
/// phase is checked per case, seeded to that state (or left unseeded for
/// Missing).
#[test]
fn each_single_phase_state_maps_to_its_own_exit_code() {
    // Arrange
    let cases: [(&str, Option<&str>, Option<i32>); 5] = [
        ("missing", None, Some(1)),
        ("fail", Some("FAIL"), Some(1)),
        ("stuck", Some("STUCK"), Some(1)),
        ("warn", Some("WARN"), Some(0)),
        ("inconclusive", Some("INCONCLUSIVE"), Some(1)),
    ];

    for (label, verdict, expected_code) in cases {
        let f = Fixture::new(&format!("state-{label}"));
        if let Some(v) = verdict {
            f.seed("plan-a", "phase-x", v);
        }

        // Act
        let out = f.run("plan-a", "gate-run", &["phase-x"]);

        // Assert
        assert_eq!(
            out.status.code(),
            expected_code,
            "state {label}: expected exit {expected_code:?}, got {:?}: stdout={} stderr={}",
            out.status.code(),
            stdout_of(&out),
            stderr_of(&out)
        );
    }
}

#[test]
fn fail_and_missing_phases_in_one_invocation_are_both_named_distinctly() {
    // Arrange
    let f = Fixture::new("fail-and-missing");
    f.seed("plan-a", "broken", "FAIL");
    // "unrecorded" is deliberately never seeded.

    // Act
    let out = f.run("plan-a", "gate-run", &["broken", "unrecorded"]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1: {}",
        stdout_of(&out)
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("broken: FAIL"),
        "the FAIL phase must be individually named: {stderr}"
    );
    assert!(
        stderr.contains("unrecorded: MISSING"),
        "the MISSING phase must be individually named: {stderr}"
    );
}

#[test]
fn mismatched_source_through_the_real_binary_exits_nonzero_and_reports_stale() {
    // Arrange
    let f = Fixture::new("cli-stale");
    f.seed("plan-a", "spec", "PASS");

    // Act
    let out = f.run_with_source(
        "plan-a",
        "gate-run",
        &["spec"],
        "different content entirely",
    );

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1: {}",
        stdout_of(&out)
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("spec: STALE"),
        "a source mismatch through the real CLI must report STALE: {stderr}"
    );
}

#[test]
fn zero_phase_arguments_exits_one_with_a_pinned_message() {
    // Arrange
    let f = Fixture::new("zero-phases");

    // Act
    let out = f.run("plan-a", "gate-run", &[]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}: {}",
        out.status.code(),
        stdout_of(&out)
    );
    assert!(
        stderr_of(&out).contains("no phases specified"),
        "got: {}",
        stderr_of(&out)
    );
}

#[test]
fn gate_check_errors_when_worktree_scoping_cannot_resolve() {
    // Arrange: no `origin` remote configured, `gate check`'s own
    // independent path-construction call site.
    let f = Fixture::new_without_origin("no-origin");

    // Act
    let out = f.run("plan-a", "gate-run", &["spec"]);

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected a hard error, not a silent repo-local fallback: {}",
        stderr_of(&out)
    );
    assert!(
        !f.repo.join(".claude").join("state.db").exists(),
        "must never fall back to reading/writing a repo-local state.db"
    );
}

#[test]
fn recorded_pass_checked_against_same_source_resolves_pass_and_is_satisfied() {
    // Arrange
    let f = Fixture::new("stale-pass-same-source");
    let source = f.repo.join("source-a.txt");
    fs::write(&source, "content A").expect("write source file");
    let recorded_hash = hash::hash_hex(b"content A");
    f.seed_with_hash("plan-a", "spec", "PASS", Some(&recorded_hash));
    let phases = vec!["spec".to_string()];

    // Act
    let result = f.check_in_process("plan-a", "gate-run", &phases, source.to_str().unwrap());

    // Assert
    let output = result.expect("matching source content should pass, not error");
    assert!(output.contains("PASS"), "got: {output}");
}

#[test]
fn recorded_pass_checked_against_different_source_resolves_stale_and_is_not_satisfied() {
    // Arrange
    let f = Fixture::new("stale-pass-diff-source");
    let recorded_hash = hash::hash_hex(b"content A");
    f.seed_with_hash("plan-a", "spec", "PASS", Some(&recorded_hash));
    let source_b = f.repo.join("source-b.txt");
    fs::write(&source_b, "content B").expect("write source file");
    let phases = vec!["spec".to_string()];

    // Act
    let result = f.check_in_process("plan-a", "gate-run", &phases, source_b.to_str().unwrap());

    // Assert
    let err = result.expect_err("mismatched source content must not silently pass");
    assert!(err.contains("STALE"), "got: {err}");
}

/// Mirrors `each_single_phase_state_maps_to_its_own_exit_code`'s table-driven
/// style: a pre-migration row (no stored `source_hash`) must resolve STALE
/// regardless of its stored verdict, confirming no verdict-specific branch
/// short-circuits the NULL check first.
#[test]
fn pre_migration_rows_with_no_source_hash_resolve_stale_regardless_of_verdict() {
    // Arrange
    let verdicts = ["PASS", "FAIL"];

    for verdict in verdicts {
        let f = Fixture::new(&format!("stale-legacy-{}", verdict.to_lowercase()));
        f.seed_with_hash("plan-a", "spec", verdict, None);
        let source = f.repo.join("source.txt");
        fs::write(&source, "any content").expect("write source file");
        let phases = vec!["spec".to_string()];

        // Act
        let result = f.check_in_process("plan-a", "gate-run", &phases, source.to_str().unwrap());

        // Assert
        let err = result.expect_err(&format!(
            "a pre-migration {verdict} row must resolve STALE, not pass silently"
        ));
        assert!(err.contains("STALE"), "verdict {verdict}: got {err}");
    }
}

#[test]
fn three_phases_in_one_run_carry_distinct_fail_missing_stale_labels() {
    // Arrange
    let f = Fixture::new("stale-three-phases");
    let source_b = f.repo.join("source-b.txt");
    fs::write(&source_b, "content B").expect("write source file");
    let current_hash = hash::hash_hex(b"content B");
    f.seed_with_hash("plan-a", "broken", "FAIL", Some(&current_hash));
    let stale_hash = hash::hash_hex(b"content A");
    f.seed_with_hash("plan-a", "spec", "PASS", Some(&stale_hash));
    // "unrecorded" is deliberately never seeded.
    let phases = vec![
        "broken".to_string(),
        "unrecorded".to_string(),
        "spec".to_string(),
    ];

    // Act
    let result = f.check_in_process("plan-a", "gate-run", &phases, source_b.to_str().unwrap());

    // Assert
    let err = result.expect_err("a run with any unsatisfied phase must error");
    assert!(err.contains("broken: FAIL"), "got: {err}");
    assert!(err.contains("unrecorded: MISSING"), "got: {err}");
    assert!(err.contains("spec: STALE"), "got: {err}");
}

#[test]
fn nonexistent_source_path_returns_err() {
    // Arrange
    let f = Fixture::new("stale-nonexistent-source");
    f.seed_with_hash("plan-a", "spec", "PASS", Some("any-hash"));
    let phases = vec!["spec".to_string()];
    let missing_source = f.repo.join("does-not-exist.txt");

    // Act
    let result = f.check_in_process(
        "plan-a",
        "gate-run",
        &phases,
        missing_source.to_str().unwrap(),
    );

    // Assert
    let err = result.expect_err("a nonexistent source path must be a clear error");
    assert!(err.contains("failed to read"), "got: {err}");
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&stdout_of(out))
        .unwrap_or_else(|e| panic!("bad json ({e}): {}", stdout_of(out)))
}

fn run_json(f: &Fixture, phases: &[&str], content: &str) -> std::process::Output {
    let source = f.repo.join("seed-source.txt");
    fs::write(&source, content).expect("source fixture should write");
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["gate", "check", "plan-a", "gate-run"])
        .args(phases)
        .arg("--source")
        .arg(&source)
        .arg("--json")
        .current_dir(&f.repo)
        .env("HOME", &f.home)
        .output()
        .expect("playbook binary should spawn")
}

#[test]
fn json_all_pass_reports_ok_true_and_exits_zero() {
    let f = Fixture::new("json-pass");
    f.seed("plan-a", "spec", "PASS");
    f.seed("plan-a", "impl", "WARN");

    let out = run_json(&f, &["spec", "impl"], SEED_SOURCE_CONTENT);

    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let v = json_of(&out);
    assert_eq!(v["version"], 1);
    assert_eq!(v["slug"], "plan-a");
    assert_eq!(v["ok"], true);
    assert_eq!(
        v["phases"],
        serde_json::json!([
            {"phase": "spec", "status": "PASS"},
            {"phase": "impl", "status": "WARN"}
        ])
    );
}

#[test]
fn json_fail_and_missing_report_ok_false_and_exit_one() {
    let f = Fixture::new("json-fail");
    f.seed("plan-a", "broken", "FAIL");

    let out = run_json(&f, &["broken", "unrecorded"], SEED_SOURCE_CONTENT);

    assert_eq!(out.status.code(), Some(1));
    let v = json_of(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["slug"], "plan-a");
    assert_eq!(
        v["phases"],
        serde_json::json!([
            {"phase": "broken", "status": "FAIL"},
            {"phase": "unrecorded", "status": "MISSING"}
        ])
    );
    assert!(stderr_of(&out).is_empty());
}

#[test]
fn json_stale_source_reports_stale_and_exit_one() {
    let f = Fixture::new("json-stale");
    f.seed("plan-a", "spec", "PASS");

    let out = run_json(&f, &["spec"], "different content entirely");

    assert_eq!(out.status.code(), Some(1));
    let v = json_of(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(
        v["phases"],
        serde_json::json!([{"phase": "spec", "status": "STALE"}])
    );
    assert!(stderr_of(&out).is_empty());
}

#[test]
fn json_empty_phase_list_is_ok_false_with_empty_array_and_exit_one() {
    let f = Fixture::new("json-empty");

    let out = run_json(&f, &[], SEED_SOURCE_CONTENT);

    assert_eq!(out.status.code(), Some(1));
    let v = json_of(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["phases"], serde_json::json!([]));
    assert!(stderr_of(&out).contains("no phases specified"));
}

#[test]
fn text_output_without_json_flag_is_byte_identical() {
    let f = Fixture::new("text-identical");
    f.seed("plan-a", "spec", "PASS");
    f.seed("plan-a", "impl", "WARN");

    let out = f.run("plan-a", "gate-run", &["spec", "impl"]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_of(&out), "spec: PASS\nimpl: WARN\n");
    assert!(stderr_of(&out).is_empty());
}

#[test]
fn json_with_an_unreadable_source_prints_nothing_on_stdout_and_exits_one() {
    let f = Fixture::new("json-bad-source");
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args([
            "gate", "check", "plan-a", "gate-run", "spec", "--json", "--source",
        ])
        .arg(f.repo.join("missing.txt"))
        .current_dir(&f.repo)
        .env("HOME", &f.home)
        .output()
        .expect("playbook binary should spawn");

    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty());
    assert!(stderr_of(&out).starts_with("gate check: "));
}

#[test]
fn text_failure_output_is_exact_on_stderr_with_empty_stdout() {
    let f = Fixture::new("text-fail-exact");
    f.seed("plan-a", "broken", "FAIL");

    let out = f.run("plan-a", "gate-run", &["broken", "unrecorded"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_of(&out).is_empty());
    assert_eq!(
        stderr_of(&out),
        "gate check: broken: FAIL\nunrecorded: MISSING\n"
    );
}
