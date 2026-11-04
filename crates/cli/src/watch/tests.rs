// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The commands end to end over a fake GitHub and a scratch home.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::json;
use sha2::Digest;

use super::clock::ManualClock;
use super::code::FakeCode;
use super::engine::IssueView;
use super::journal::RecoverOps;
use super::*;
use crate::config::write::Tier;

const TOKEN: &str = "ghp_secret_token_value";
const RULE: &str = r#"{"on":"issue.opened","do":"label","with":{"labels":["triage"]}}"#;
const ISSUE: &str = r#"[{"number":7,"created_at":"2026-11-05T10:00:00Z","updated_at":"2026-11-05T10:00:00Z","user":{"login":"stranger"},"author_association":"NONE","labels":[],"body":"hello"}]"#;
const NOW: &str = "2026-11-05T12:00:00Z";
const EARLIER: &str = "2026-11-05T09:00:00Z";

#[derive(Default)]
struct Spy {
    calls: RefCell<Vec<(u64, Vec<String>)>>,
}

impl GithubWrite for Spy {
    fn issue(&self, _: &str, _: u64) -> Result<IssueView, String> {
        Ok(IssueView {
            open: true,
            labels: vec![],
            body_hash: sha2::Sha256::digest(b"hello").into(),
            title: "t".into(),
            body: "hello".into(),
            fork: false,
        })
    }
    fn has_comment(&self, _: &str, _: u64, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn add_comment(&self, _: &str, _: u64, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn merge_check(&self, _: &str, _: u64) -> Result<crate::watch::engine::MergeCheck, String> {
        Err("no merge in this test".to_string())
    }
    fn merge(&self, _: &str, _: u64, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn diff(&self, _: &str, _: u64) -> Result<String, String> {
        Ok(String::new())
    }
    fn has_review(&self, _: &str, _: u64, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn create_pending_review(&self, _: &str, _: u64, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn update_branch(&self, _: &str, _: u64) -> Result<crate::watch::engine::BranchUpdate, String> {
        Ok(crate::watch::engine::BranchUpdate::UpToDate)
    }
    fn thread(&self, _: &str, _: u64) -> Result<Vec<crate::watch::engine::Post>, String> {
        Ok(vec![])
    }
    fn add_labels(&self, _: &str, number: u64, labels: &[String]) -> Result<(), String> {
        self.calls.borrow_mut().push((number, labels.to_vec()));
        Ok(())
    }
}

/// Answers the issues endpoint with one issue and the others with nothing.
struct Canned(Rc<Cell<usize>>);

impl Http for Canned {
    fn get(
        &mut self,
        url: &str,
        _: Option<&str>,
        body: &mut Vec<u8>,
    ) -> Result<github::Reply, SourceError> {
        self.0.set(self.0.get() + 1);
        let json = if url.contains("/issues?") {
            ISSUE
        } else {
            "[]"
        };
        body.extend_from_slice(json.as_bytes());
        Ok(github::Reply {
            status: 200,
            ..github::Reply::default()
        })
    }
}

struct Env {
    home: PathBuf,
    data: PathBuf,
    clock: ManualClock,
    spy: Spy,
    code: FakeCode,
    requests: Rc<Cell<usize>>,
}

fn env(tag: &str, enabled: bool) -> Env {
    let home = crate::testing::scratch_root(tag);
    let root = crate::common::paths::playbook_root_from(&home);
    let put = |key: &str, value: serde_json::Value| {
        config_store::put(&root, Tier::Global, None, key, &value).unwrap();
    };
    put("watch.enabled", json!(enabled));
    put("watch.repos", json!(["o/r"]));
    put("watch.rule.triage", json!(RULE));
    Env {
        data: playbook_data::data_dir_from(&home),
        home,
        clock: ManualClock::new(parse_iso8601_utc_epoch(NOW).unwrap()),
        spy: Spy::default(),
        code: FakeCode::default(),
        requests: Rc::default(),
    }
}

impl Env {
    fn ctx(&self) -> Ctx<'_, Spy, ManualClock> {
        Ctx {
            home: &self.home,
            data: &self.data,
            write: &self.spy,
            clock: &self.clock,
            me: "me",
            model: &NoModel,
            code: &self.code,
        }
    }

    fn once(&self, since: Option<&str>) -> Result<String, String> {
        once(&self.ctx(), since, || Canned(self.requests.clone()))
    }

    fn rows(&self, state: &str) -> Vec<i64> {
        Store::open(&self.data.join("watch.db"))
            .unwrap()
            .ids_in_state(state)
            .unwrap()
    }
}

fn hold(data: &Path) -> lock::InstanceLock {
    fs::create_dir_all(data).unwrap();
    match acquire(&data.join("watch.lock")).unwrap() {
        Acquire::Held(lock) => lock,
        Acquire::Busy => panic!("the lock was taken"),
    }
}

#[test]
fn the_first_run_starts_now_and_queues_no_backlog() {
    // Arrange
    let e = env("watch-cmd-first", true);

    // Act
    let out = e.once(None).unwrap();

    // Assert
    assert!(out.contains("watching from now"), "{out}");
    assert_eq!(e.requests.get(), 0);
    assert!(e.rows("proposed").is_empty());
}

#[test]
fn since_replays_the_backlog_as_proposals_and_writes_nothing() {
    // Arrange
    let e = env("watch-cmd-since", true);

    // Act
    let out = e.once(Some(EARLIER)).unwrap();

    // Assert
    assert!(out.contains("proposal 1: rule triage on o/r#7"), "{out}");
    assert!(e.spy.calls.borrow().is_empty());
    assert!(!out.contains(TOKEN));
}

#[test]
fn a_rule_at_auto_under_an_auto_ceiling_labels_in_the_same_run() {
    // Arrange
    let e = env("watch-cmd-auto", true);
    let root = crate::common::paths::playbook_root_from(&e.home);
    let auto = RULE.replace(r#""do":"label""#, r#""do":"label","autonomy":"auto""#);
    config_store::put(&root, Tier::Global, None, "watch.rule.triage", &json!(auto)).unwrap();
    config_store::put(&root, Tier::Global, None, "watch.autonomy", &json!("auto")).unwrap();

    // Act
    let out = e.once(Some(EARLIER)).unwrap();

    // Assert
    assert!(out.contains("o/r: ran 1 approved"), "{out}");
    assert_eq!(e.spy.calls.borrow().len(), 1);
    // The next run sees the same event again and adds no call.
    e.once(None).unwrap();
    assert_eq!(e.spy.calls.borrow().len(), 1);
}

#[test]
fn since_must_be_a_utc_time() {
    let e = env("watch-cmd-bad-since", true);
    let err = e.once(Some("yesterday")).unwrap_err();
    assert!(err.contains("--since"), "{err}");
}

#[test]
fn a_disabled_watch_does_nothing_and_says_so() {
    let e = env("watch-cmd-off", false);
    let out = e.once(Some(EARLIER)).unwrap();
    assert!(out.contains("watch is off"), "{out}");
    assert_eq!(e.requests.get(), 0);
    assert!(!e.data.join("watch.db").exists());
}

#[test]
fn a_second_watch_process_prints_already_running_and_polls_nothing() {
    // Arrange
    let e = env("watch-cmd-busy", true);
    let _held = hold(&e.data);

    // Act
    let out = e.once(Some(EARLIER)).unwrap();

    // Assert
    assert_eq!(out, "watch already running\n");
    assert_eq!(e.requests.get(), 0);
}

#[test]
fn approve_with_a_free_lock_labels_the_issue_once() {
    // Arrange
    let e = env("watch-cmd-approve", true);
    e.once(Some(EARLIER)).unwrap();

    // Act
    let out = approve(&e.ctx(), 1).unwrap();

    // Assert
    assert_eq!(out, "item 1 done\n");
    assert_eq!(
        e.spy.calls.borrow().as_slice(),
        &[(7, vec!["triage".to_string()])]
    );
    assert!(e.rows("done").contains(&1));
}

#[test]
fn approve_while_another_process_runs_watch_marks_it_and_writes_nothing() {
    // Arrange
    let e = env("watch-cmd-handoff", true);
    e.once(Some(EARLIER)).unwrap();
    let _held = hold(&e.data);

    // Act
    let out = approve(&e.ctx(), 1).unwrap();

    // Assert
    assert!(out.contains("the running watch will carry it out"), "{out}");
    assert_eq!(e.rows("approved"), vec![1]);
    assert!(e.spy.calls.borrow().is_empty());
}

#[test]
fn the_running_watch_carries_out_an_approved_item_on_its_next_cycle() {
    // Arrange
    let e = env("watch-cmd-cycle", true);
    e.once(Some(EARLIER)).unwrap();
    let held = hold(&e.data);
    approve(&e.ctx(), 1).unwrap();
    drop(held);

    // Act
    let out = e.once(None).unwrap();

    // Assert
    assert!(out.contains("o/r: ran 1 approved"), "{out}");
    assert_eq!(e.spy.calls.borrow().len(), 1);
}

#[test]
fn reject_closes_the_proposal_without_a_write() {
    // Arrange
    let e = env("watch-cmd-reject", true);
    e.once(Some(EARLIER)).unwrap();

    // Act
    let out = reject(&e.ctx(), 1).unwrap();

    // Assert
    assert_eq!(out, "item 1 rejected\n");
    assert!(e.spy.calls.borrow().is_empty());
    assert!(e.rows("proposed").is_empty());
}

#[test]
fn approving_an_unknown_item_is_an_error() {
    let e = env("watch-cmd-unknown", true);
    e.once(None).unwrap();
    assert!(approve(&e.ctx(), 99).is_err());
}

#[test]
fn dry_run_lists_the_rule_and_the_match_and_changes_nothing() {
    // Arrange
    let e = env("watch-cmd-dry", true);

    // Act
    let out = dry_run(&e.ctx(), Some(EARLIER), || Canned(e.requests.clone())).unwrap();

    // Assert
    assert!(
        out.contains("rule triage: IssueOpened -> Label at Propose"),
        "{out}"
    );
    assert!(
        out.contains("would propose: rule triage for issue:7:opened"),
        "{out}"
    );
    assert!(!e.data.join("watch.db").exists());
    assert!(e.spy.calls.borrow().is_empty());
}

#[test]
fn a_repo_tombstone_switches_the_global_rule_off_for_that_repo() {
    // Arrange
    let e = env("watch-cmd-tombstone", true);
    let root = crate::common::paths::playbook_root_from(&e.home);
    config_store::put(
        &root,
        Tier::Repo,
        Some("o/r"),
        "watch.rule.triage",
        &json!(r#"{"autonomy":"off"}"#),
    )
    .unwrap();

    // Act
    let out = dry_run(&e.ctx(), Some(EARLIER), || Canned(e.requests.clone())).unwrap();

    // Assert
    assert!(!out.contains("rule triage"), "{out}");
}

#[test]
fn status_reports_the_queue_the_lock_and_the_limits() {
    // Arrange
    let e = env("watch-cmd-status", true);
    e.once(Some(EARLIER)).unwrap();
    let _held = hold(&e.data);

    // Act
    let json = control::status_json(&e.ctx()).unwrap();
    let text = control::status(&e.ctx(), false).unwrap();

    // Assert
    assert_eq!(json["queue"]["proposed"], 1);
    assert_eq!(json["running"], true);
    assert_eq!(json["paused"], false);
    assert_eq!(json["today"]["actions_limit"], 50);
    assert!(
        text.contains("proposed: 1") && text.contains("running: true"),
        "{text}"
    );
}

#[test]
fn pause_stops_a_rule_that_runs_on_its_own_and_resume_lets_it_through() {
    // Arrange
    let e = env("watch-cmd-pause", true);
    let root = crate::common::paths::playbook_root_from(&e.home);
    let auto = RULE.replace(r#""do":"label""#, r#""do":"label","autonomy":"auto""#);
    config_store::put(&root, Tier::Global, None, "watch.rule.triage", &json!(auto)).unwrap();
    config_store::put(&root, Tier::Global, None, "watch.autonomy", &json!("auto")).unwrap();
    control::set_paused(&e.ctx(), true).unwrap();

    // Act
    e.once(Some(EARLIER)).unwrap();
    let paused_calls = e.spy.calls.borrow().len();
    control::set_paused(&e.ctx(), false).unwrap();
    e.once(None).unwrap();

    // Assert
    assert_eq!(paused_calls, 0);
    assert_eq!(e.spy.calls.borrow().len(), 1);
}

#[test]
fn log_filters_by_rule_and_outcome_and_prints_no_control_characters() {
    // Arrange
    let e = env("watch-cmd-log", true);
    let audit = AuditLog::new(&e.data.join("watch"));
    for (rule, outcome) in [("a", "proposed"), ("b", "done"), ("a", "done")] {
        audit
            .append(&json!({"time": 1, "rule": rule, "key": "k\u{1b}[31m", "outcome": outcome}))
            .unwrap();
    }

    // Act
    let all = control::log(&e.ctx(), None, None, false).unwrap();
    let one = control::log(&e.ctx(), Some("a"), Some("done"), false).unwrap();
    let as_json = control::log(&e.ctx(), None, None, true).unwrap();

    // Assert
    assert_eq!(all.lines().count(), 3);
    assert_eq!(one.lines().count(), 1);
    assert!(!all.chars().any(|c| c.is_control() && c != '\n'), "{all:?}");
    assert!(as_json
        .lines()
        .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()));
}

/// Serves the issue only after `ready` is set, and counts requests.
struct Gated {
    ready: Rc<Cell<bool>>,
}

impl Http for Gated {
    fn get(
        &mut self,
        url: &str,
        _: Option<&str>,
        body: &mut Vec<u8>,
    ) -> Result<github::Reply, SourceError> {
        let issue = ISSUE.replace("2026-11-05T10:00:00Z", "2026-11-05T13:00:00Z");
        let json = if self.ready.get() && url.contains("/issues?") {
            issue.as_str()
        } else {
            "[]"
        };
        body.extend_from_slice(json.as_bytes());
        Ok(github::Reply {
            status: 200,
            ..github::Reply::default()
        })
    }
}

/// Fails every request.
struct Broken(SourceError);

impl Http for Broken {
    fn get(
        &mut self,
        _: &str,
        _: Option<&str>,
        _: &mut Vec<u8>,
    ) -> Result<github::Reply, SourceError> {
        Err(self.0.clone())
    }
}

struct NoModel;

impl claude::ModelRunner for NoModel {
    fn run(&self, _: &claude::Call) -> Result<String, String> {
        Err("no model in this test".to_string())
    }
}

impl RecoverOps for Spy {
    fn remove_worktree(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn branch_pushed(&self, _: &str, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn ensure_marker(&self, _: &store::WorkRow) -> Result<(), String> {
        Ok(())
    }
}

struct Longest;

impl backoff::Jitter for Longest {
    fn pick(&self, ceiling: i64) -> i64 {
        ceiling
    }
}

/// Runs the loop for `passes` passes. `on_sleep` sees each wait.
fn run_passes<H: Http>(
    e: &Env,
    passes: usize,
    make_http: impl FnMut() -> H,
    mut on_sleep: impl FnMut(i64),
) -> (Result<(), String>, String, Vec<i64>) {
    let printed = RefCell::new(String::new());
    let waits = RefCell::new(Vec::new());
    let result = {
        let mut env = cycle::LoopEnv {
            sleep: &mut |seconds| {
                waits.borrow_mut().push(seconds);
                on_sleep(seconds);
            },
            stop: &|| waits.borrow().len() >= passes,
            print: &mut |text| printed.borrow_mut().push_str(text),
            jitter: &Longest,
        };
        cycle::run_loop(&e.ctx(), make_http, &mut env)
    };
    (result, printed.into_inner(), waits.into_inner())
}

#[test]
fn a_source_is_not_polled_again_before_its_wait_has_passed() {
    // Arrange
    let e = env("watch-cmd-next-poll", true);
    e.once(Some(EARLIER)).unwrap();
    let after_first = e.requests.get();

    // Act
    e.once(None).unwrap();
    let too_soon = e.requests.get();
    e.clock.advance(61);
    e.once(None).unwrap();

    // Assert
    assert_eq!(too_soon, after_first);
    assert!(e.requests.get() > after_first);
}

#[test]
fn a_pass_recovers_an_item_left_running_by_a_killed_run() {
    // Arrange: a proposal that was running with a worktree, then the run died.
    let e = env("watch-cmd-recover", true);
    e.once(Some(EARLIER)).unwrap();
    let mut store = Store::open(&e.data.join("watch.db")).unwrap();
    store
        .transaction(|tx| {
            tx.set_state(1, "running", None)?;
            tx.set_journal(1, "worktree", "/tmp/wt", "watch/b")
        })
        .unwrap();
    drop(store);

    // Act
    let out = e.once(None).unwrap();

    // Assert: queued again, then decided again as a proposal.
    assert!(out.contains("item 1 recovered: Requeued"), "{out}");
    assert_eq!(e.rows("proposed"), vec![1]);
}

#[test]
fn the_loop_polls_each_pass_and_resumes_from_the_cursor_after_an_eight_hour_gap() {
    // Arrange: the first pass starts the sources from now.
    let e = env("watch-loop-gap", true);
    let ready = Rc::new(Cell::new(false));

    // Act: the machine sleeps 8 hours, and an issue arrives meanwhile.
    let (result, printed, waits) = run_passes(
        &e,
        2,
        || Gated {
            ready: ready.clone(),
        },
        |_| {
            ready.set(true);
            e.clock.advance(8 * 3600);
        },
    );

    // Assert
    result.unwrap();
    assert!(printed.contains("watching from now"), "{printed}");
    assert!(
        printed.contains("proposal 1: rule triage on o/r#7"),
        "{printed}"
    );
    assert_eq!(waits, vec![60, 60]);
}

#[test]
fn a_transient_error_backs_off_with_growing_waits_and_the_loop_goes_on() {
    // Arrange: cursors exist, so every pass makes requests.
    let e = env("watch-loop-transient", true);
    e.once(Some(EARLIER)).unwrap();
    // The sources are not due again until their wait has passed.
    e.clock.advance(120);

    // Act
    let (result, printed, waits) = run_passes(
        &e,
        4,
        || Broken(SourceError::Transient("HTTP 502".into())),
        |_| {},
    );

    // Assert: the poll setting is 60, so the backoff shows once it passes 60.
    result.unwrap();
    assert!(printed.contains("HTTP 502"), "{printed}");
    assert_eq!(waits, vec![60, 60, 120, 240]);
}

#[test]
fn a_refused_token_stops_the_loop_with_a_clear_message() {
    let e = env("watch-loop-unauthorized", true);
    e.once(Some(EARLIER)).unwrap();
    e.clock.advance(120);
    let (result, _, waits) = run_passes(&e, 5, || Broken(SourceError::Unauthorized), |_| {});
    assert!(result.unwrap_err().contains("refused the token"));
    assert!(waits.is_empty());
}

#[test]
fn a_second_foreground_watch_says_so_and_fails() {
    let e = env("watch-loop-busy", true);
    let _held = hold(&e.data);
    let (result, _, waits) = run_passes(&e, 1, || Canned(e.requests.clone()), |_| {});
    assert_eq!(result.unwrap_err(), "watch already running");
    assert!(waits.is_empty());
}

#[test]
fn a_watch_switched_off_while_the_loop_runs_says_so_once_and_polls_nothing() {
    let e = env("watch-loop-off", false);
    let (result, printed, waits) = run_passes(&e, 3, || Canned(e.requests.clone()), |_| {});
    result.unwrap();
    assert_eq!(printed.matches("watch is off").count(), 1, "{printed}");
    assert_eq!(e.requests.get(), 0);
    assert_eq!(waits.len(), 3);
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn watch_sources() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/watch"),
        &mut files,
    );
    files
        .into_iter()
        .map(|f| {
            let text = fs::read_to_string(&f).unwrap();
            (f, text)
        })
        .collect()
}

#[test]
fn the_real_http_client_is_built_only_in_mod_rs() {
    // Arrange
    let needle = ["UreqHttp", "new"].join("::");

    // Act
    let offenders: Vec<_> = watch_sources()
        .into_iter()
        .filter(|(f, text)| f.file_name().is_some_and(|n| n != "mod.rs") && text.contains(&needle))
        .map(|(f, _)| f)
        .collect();

    // Assert
    assert!(offenders.is_empty(), "{offenders:?}");
}

#[test]
fn the_kill_switch_is_watch_enabled_read_fresh_each_time() {
    use super::code::{CodeWork, GitCode};
    // (the setting, whether the switch is on)
    for (enabled, killed) in [(true, false), (false, true)] {
        // Arrange
        let env = env(&format!("watch-kill-{enabled}"), enabled);
        let clone = env.home.clone();
        let code = GitCode {
            clone: &clone,
            worktrees: env.data.join("trees"),
            home: &env.home,
        };

        // Act / Assert
        assert_eq!(code.killed(), killed);
    }
}

#[test]
fn nothing_runs_a_shell() {
    // Arrange
    let needles = [
        ["sh", "-c"].join(" "),
        ["Command::new(\"", "sh\")"].concat(),
    ];

    // Act
    let offenders: Vec<_> = watch_sources()
        .into_iter()
        .filter(|(_, text)| needles.iter().any(|n| text.contains(n)))
        .map(|(f, _)| f)
        .collect();

    // Assert
    assert!(offenders.is_empty(), "{offenders:?}");
}
