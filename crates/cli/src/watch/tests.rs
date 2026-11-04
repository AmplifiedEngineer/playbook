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
use super::engine::IssueView;
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
        })
    }
    fn has_comment(&self, _: &str, _: u64, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn add_comment(&self, _: &str, _: u64, _: &str) -> Result<(), String> {
        Ok(())
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
