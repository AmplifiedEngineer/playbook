// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Turns events into proposals and carries out the ones you approve. The
//! first action is `label`: it adds labels from the rule and never reads or
//! writes text. Everything it needs from GitHub sits behind [`GithubWrite`].

use serde_json::json;

use super::audit::AuditLog;
use super::clock::Clock;
use super::event::Event;
use super::rule::{Autonomy, Rule};
use super::settings::effective;
use super::store::{Cursor, Insert, NewWork, Store, StoreError, Tx, WorkRow};

/// The marker an automated comment carries so watch mode ignores its own text.
pub const HIDDEN_MARKER: &str = "<!-- playbook-watch -->";

/// The GitHub calls the label action makes.
pub trait GithubWrite {
    fn is_open(&self, repo: &str, number: u64) -> Result<bool, String>;
    fn labels(&self, repo: &str, number: u64) -> Result<Vec<String>, String>;
    fn add_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String>;
}

/// A rule with the id it is stored under.
pub type NamedRule = (String, Rule);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// Another process holds the lock, so it will run the item.
    Handed,
    /// The label was added.
    Done,
    /// The labels were already there.
    AlreadyDone,
    /// The issue was closed, or the rule is gone.
    Skipped(&'static str),
}

#[derive(Debug)]
pub enum EngineError {
    Store(StoreError),
    /// The id is unknown or the item is not waiting for approval.
    NotProposed(i64),
    /// GitHub refused or failed. The item stays proposed, so approve can retry.
    Github(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => e.fmt(f),
            Self::NotProposed(id) => write!(f, "item {id} is not waiting for approval"),
            Self::Github(why) => write!(f, "GitHub call failed: {why}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<StoreError> for EngineError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// Whether `rule` matches `event`. Text is never read: only labels, the
/// author, and an exact comment prefix. A comment you wrote yourself, or one
/// that carries the marker, matches nothing unless it starts with the prefix a
/// rule asks for.
pub fn matches(rule: &Rule, event: &Event, me: &str) -> bool {
    if rule.on != event.kind {
        return false;
    }
    let f = &rule.filter;
    if !f.labels_any.is_empty() && !f.labels_any.iter().any(|l| event.labels.contains(l)) {
        return false;
    }
    if !f.authors.is_empty() && !f.authors.iter().any(|a| a == &event.author) {
        return false;
    }
    if event.comment_head.contains(HIDDEN_MARKER) {
        return false;
    }
    match &f.comment_prefix {
        Some(prefix) => {
            let trusted = event.author == me
                || matches!(
                    event.association.as_str(),
                    "OWNER" | "MEMBER" | "COLLABORATOR"
                );
            trusted && event.comment_head.starts_with(prefix.as_str())
        }
        None => event.author != me || !is_comment(event),
    }
}

fn is_comment(event: &Event) -> bool {
    event.key.starts_with("comment:") || event.key.starts_with("reviewcomment:")
}

pub struct Engine<'a, W, C> {
    pub store: &'a mut Store,
    pub write: &'a W,
    pub clock: &'a C,
    pub audit: &'a AuditLog,
    /// The login of the authenticated user.
    pub me: &'a str,
}

impl<W: GithubWrite, C: Clock> Engine<'_, W, C> {
    fn record(&self, rule: &str, key: &str, outcome: &str) {
        // A failed audit write must not stop the work, but it is not silent.
        if let Err(e) = self.audit.append(&json!({
            "time": self.clock.now(),
            "rule": rule,
            "key": key,
            "outcome": outcome,
        })) {
            eprintln!("watch: could not write the audit log: {e}");
        }
    }

    /// Saves the cursor and queues a proposal for every rule an event matches,
    /// in one transaction. Events older than `max_catchup` seconds are audited
    /// as stale and skipped. Returns the ids of the new proposals.
    pub fn ingest(
        &mut self,
        source: &str,
        cursor: &Cursor,
        events: &[Event],
        rules: &[NamedRule],
        ceiling: Autonomy,
        max_catchup: i64,
    ) -> Result<Vec<i64>, EngineError> {
        let now = self.clock.now();
        let mut decisions: Vec<(String, String, &'static str)> = Vec::new();
        let ids = self.store.transaction(|tx| {
            tx.set_cursor(source, cursor)?;
            let mut ids = Vec::new();
            for event in events {
                for (id, rule) in rules {
                    if effective(rule.autonomy, ceiling) == Autonomy::Off
                        || !matches(rule, event, self.me)
                    {
                        continue;
                    }
                    let key = format!("{}:{id}", event.key);
                    if now - event.updated_at > max_catchup {
                        decisions.push((id.clone(), key, "stale"));
                        continue;
                    }
                    ids.extend(queue(tx, event, id, &key, now, &mut decisions)?);
                }
            }
            Ok(ids)
        })?;
        for (rule, key, outcome) in &decisions {
            self.record(rule, key, outcome);
        }
        Ok(ids)
    }

    /// Carries out a proposal. With the lock held by another process it only
    /// marks the item approved and that process runs it.
    pub fn approve(
        &mut self,
        id: i64,
        rules: &[NamedRule],
        lock_free: bool,
    ) -> Result<ApproveOutcome, EngineError> {
        let row = self.store.work(id)?.filter(|r| r.state == "proposed");
        let Some(row) = row else {
            return Err(EngineError::NotProposed(id));
        };
        if !lock_free {
            self.set(id, "approved", None)?;
            self.record(&row.rule, &row.key, "approved");
            return Ok(ApproveOutcome::Handed);
        }
        self.run(id, &row, rules)
    }

    /// Runs every approved item. The lock holder calls this on each cycle.
    pub fn run_approved(&mut self, rules: &[NamedRule]) -> Result<usize, EngineError> {
        let ids = self.store.ids_in_state("approved")?;
        for id in &ids {
            if let Some(row) = self.store.work(*id)? {
                self.run(*id, &row, rules)?;
            }
        }
        Ok(ids.len())
    }

    pub fn reject(&mut self, id: i64) -> Result<(), EngineError> {
        let row = self.store.work(id)?.filter(|r| r.state == "proposed");
        let Some(row) = row else {
            return Err(EngineError::NotProposed(id));
        };
        self.set(id, "done", Some("rejected"))?;
        self.record(&row.rule, &row.key, "rejected");
        Ok(())
    }

    fn set(&mut self, id: i64, state: &str, reason: Option<&str>) -> Result<(), EngineError> {
        self.store
            .transaction(|tx| tx.set_state(id, state, reason))
            .map_err(Into::into)
    }

    fn run(
        &mut self,
        id: i64,
        row: &WorkRow,
        rules: &[NamedRule],
    ) -> Result<ApproveOutcome, EngineError> {
        let Some((_, rule)) = rules.iter().find(|(name, _)| name == &row.rule) else {
            return self.finish(id, row, ApproveOutcome::Skipped("rule_missing"));
        };
        let github = |e: String| EngineError::Github(e);
        // The world may have changed since the proposal: check before acting.
        if !self.write.is_open(&row.repo, row.object).map_err(github)? {
            return self.finish(id, row, ApproveOutcome::Skipped("closed"));
        }
        let have = self.write.labels(&row.repo, row.object).map_err(github)?;
        let missing: Vec<String> = rule
            .with
            .labels
            .iter()
            .filter(|l| !have.contains(l))
            .cloned()
            .collect();
        if missing.is_empty() {
            return self.finish(id, row, ApproveOutcome::AlreadyDone);
        }
        self.write
            .add_labels(&row.repo, row.object, &missing)
            .map_err(github)?;
        self.finish(id, row, ApproveOutcome::Done)
    }

    fn finish(
        &mut self,
        id: i64,
        row: &WorkRow,
        outcome: ApproveOutcome,
    ) -> Result<ApproveOutcome, EngineError> {
        let label = match &outcome {
            ApproveOutcome::Done => "done",
            ApproveOutcome::AlreadyDone => "already_done",
            ApproveOutcome::Skipped(why) => why,
            ApproveOutcome::Handed => "approved",
        };
        let reason = match &outcome {
            ApproveOutcome::Skipped(_) => Some("skipped"),
            _ => Some(label),
        };
        self.set(id, "done", reason)?;
        self.record(&row.rule, &row.key, label);
        Ok(outcome)
    }
}

/// Queues one proposal inside the transaction, or notes that it was a repeat.
fn queue(
    tx: &Tx<'_>,
    event: &Event,
    rule: &str,
    key: &str,
    now: i64,
    decisions: &mut Vec<(String, String, &'static str)>,
) -> Result<Option<i64>, StoreError> {
    let item = NewWork {
        key,
        rule,
        repo: &event.repo,
        object: event.number,
        body_hash: event.body_hash,
    };
    match tx.insert_work(&item, now)? {
        Insert::Queued(id) => {
            tx.set_state(id, "proposed", None)?;
            decisions.push((rule.to_string(), key.to_string(), "proposed"));
            Ok(Some(id))
        }
        Insert::Dead(_) => {
            decisions.push((rule.to_string(), key.to_string(), "queue_full"));
            Ok(None)
        }
        Insert::Duplicate => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::clock::ManualClock;
    use crate::watch::rule::Stored;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Spy {
        calls: RefCell<Vec<(String, u64, Vec<String>)>>,
        closed: bool,
        have: Vec<String>,
    }

    impl GithubWrite for Spy {
        fn is_open(&self, _: &str, _: u64) -> Result<bool, String> {
            Ok(!self.closed)
        }
        fn labels(&self, _: &str, _: u64) -> Result<Vec<String>, String> {
            Ok(self.have.clone())
        }
        fn add_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push((repo.into(), number, labels.to_vec()));
            Ok(())
        }
    }

    const LABEL: &str = r#"{"on":"issue.opened","do":"label","with":{"labels":["triage"]}}"#;
    const PREFIXED: &str = r#"{"on":"issue.commented","where":{"comment_prefix":"/playbook merge"},"do":"label","with":{"labels":["ready"]}}"#;

    fn rule(text: &str) -> Rule {
        match Stored::parse(text).unwrap() {
            Stored::Rule(r) => *r,
            Stored::Tombstone => panic!("not a rule"),
        }
    }

    fn rules(text: &str) -> Vec<NamedRule> {
        vec![("r".to_string(), rule(text))]
    }

    fn issue(number: u64, author: &str) -> Event {
        Event {
            key: format!("issue:{number}:opened"),
            repo: "o/r".into(),
            kind: crate::watch::rule::On::IssueOpened,
            author: author.into(),
            association: "NONE".into(),
            object: number,
            number,
            updated_at: 1_000,
            body_hash: [1; 32],
            labels: vec![],
            comment_head: String::new(),
        }
    }

    fn comment(author: &str, association: &str, head: &str) -> Event {
        Event {
            key: "comment:9:created".into(),
            kind: crate::watch::rule::On::IssueCommented,
            association: association.into(),
            object: 9,
            number: 4,
            comment_head: head.into(),
            ..issue(4, author)
        }
    }

    struct Fixture {
        store: Store,
        clock: ManualClock,
        audit: AuditLog,
        dir: std::path::PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let dir = crate::common::test_support::scratch_dir(tag);
        Fixture {
            store: Store::open(&dir.join("data/watch.db")).unwrap(),
            clock: ManualClock::new(1_000),
            audit: AuditLog::new(&dir.join("runtime")),
            dir,
        }
    }

    fn ingest(f: &mut Fixture, spy: &Spy, events: &[Event], rules: &[NamedRule]) -> Vec<i64> {
        let mut engine = Engine {
            store: &mut f.store,
            write: spy,
            clock: &f.clock,
            audit: &f.audit,
            me: "me",
        };
        engine
            .ingest(
                "issues",
                &Cursor::default(),
                events,
                rules,
                Autonomy::Propose,
                72 * 3600,
            )
            .unwrap()
    }

    fn engine<'a>(f: &'a mut Fixture, spy: &'a Spy) -> Engine<'a, Spy, ManualClock> {
        Engine {
            store: &mut f.store,
            write: spy,
            clock: &f.clock,
            audit: &f.audit,
            me: "me",
        }
    }

    fn audit_lines(f: &Fixture) -> usize {
        std::fs::read_to_string(f.dir.join("runtime/audit.jsonl")).map_or(0, |t| t.lines().count())
    }

    #[test]
    fn a_new_issue_becomes_a_proposal_and_nothing_is_written() {
        let mut f = fixture("watch-engine-propose");
        let spy = Spy::default();
        let ids = ingest(&mut f, &spy, &[issue(7, "stranger")], &rules(LABEL));
        assert_eq!(ids.len(), 1);
        assert_eq!(f.store.work(ids[0]).unwrap().unwrap().state, "proposed");
        assert!(spy.calls.borrow().is_empty());
        assert_eq!(audit_lines(&f), 1);
    }

    #[test]
    fn approve_with_a_free_lock_adds_the_label_once() {
        let mut f = fixture("watch-engine-approve");
        let spy = Spy::default();
        let id = ingest(&mut f, &spy, &[issue(7, "stranger")], &rules(LABEL))[0];
        let out = engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .unwrap();
        assert_eq!(out, ApproveOutcome::Done);
        assert_eq!(
            spy.calls.borrow().as_slice(),
            &[("o/r".to_string(), 7, vec!["triage".to_string()])]
        );
        let row = f.store.work(id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.reason.as_deref()),
            ("done", Some("done"))
        );
        // A second approve finds nothing waiting and adds no call.
        assert!(engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .is_err());
        assert_eq!(spy.calls.borrow().len(), 1);
    }

    #[test]
    fn a_label_that_is_already_there_adds_no_call() {
        let mut f = fixture("watch-engine-present");
        let spy = Spy {
            have: vec!["triage".into()],
            ..Spy::default()
        };
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];
        let out = engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .unwrap();
        assert_eq!(out, ApproveOutcome::AlreadyDone);
        assert!(spy.calls.borrow().is_empty());
    }

    #[test]
    fn approve_while_another_process_holds_the_lock_only_marks_it_and_the_holder_runs_it() {
        let mut f = fixture("watch-engine-handoff");
        let spy = Spy::default();
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];
        let out = engine(&mut f, &spy)
            .approve(id, &rules(LABEL), false)
            .unwrap();
        assert_eq!(out, ApproveOutcome::Handed);
        assert_eq!(f.store.work(id).unwrap().unwrap().state, "approved");
        assert!(spy.calls.borrow().is_empty());

        let ran = engine(&mut f, &spy).run_approved(&rules(LABEL)).unwrap();
        assert_eq!(ran, 1);
        assert_eq!(spy.calls.borrow().len(), 1);
        assert_eq!(f.store.work(id).unwrap().unwrap().state, "done");
    }

    #[test]
    fn reject_drops_the_proposal_without_a_write() {
        let mut f = fixture("watch-engine-reject");
        let spy = Spy::default();
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];
        engine(&mut f, &spy).reject(id).unwrap();
        let row = f.store.work(id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.reason.as_deref()),
            ("done", Some("rejected"))
        );
        assert!(spy.calls.borrow().is_empty());
    }

    #[test]
    fn an_issue_closed_before_approval_is_skipped() {
        let mut f = fixture("watch-engine-closed");
        let spy = Spy {
            closed: true,
            ..Spy::default()
        };
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];
        let out = engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .unwrap();
        assert_eq!(out, ApproveOutcome::Skipped("closed"));
        assert!(spy.calls.borrow().is_empty());
    }

    #[test]
    fn the_same_event_twice_is_one_proposal() {
        let mut f = fixture("watch-engine-dedupe");
        let spy = Spy::default();
        let first = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL));
        let second = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL));
        assert_eq!((first.len(), second.len()), (1, 0));
    }

    #[test]
    fn an_event_older_than_the_catch_up_window_is_audited_as_stale() {
        let mut f = fixture("watch-engine-stale");
        let spy = Spy::default();
        let limit = 72 * 3600;
        f.clock.advance(limit);
        // The boundary event is processed, one second older is not.
        let mut edge = issue(1, "a");
        edge.updated_at = 1_000;
        let mut old = issue(2, "a");
        old.updated_at = 999;
        let ids = ingest(&mut f, &spy, &[edge, old], &rules(LABEL));
        assert_eq!(ids.len(), 1);
        let text = std::fs::read_to_string(f.dir.join("runtime/audit.jsonl")).unwrap();
        assert!(text.contains("stale"));
    }

    #[test]
    fn an_autonomy_ceiling_of_off_runs_nothing() {
        let mut f = fixture("watch-engine-off");
        let spy = Spy::default();
        let mut e = engine(&mut f, &spy);
        let ids = e
            .ingest(
                "issues",
                &Cursor::default(),
                &[issue(7, "a")],
                &rules(LABEL),
                Autonomy::Off,
                100_000,
            )
            .unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn a_prefix_rule_ignores_a_stranger_and_runs_for_a_trusted_author() {
        let mut f = fixture("watch-engine-prefix");
        let spy = Spy::default();
        let stranger = comment("stranger", "NONE", "/playbook merge now");
        assert!(ingest(&mut f, &spy, &[stranger], &rules(PREFIXED)).is_empty());
        let trusted = comment("owner", "OWNER", "/playbook merge now");
        assert_eq!(ingest(&mut f, &spy, &[trusted], &rules(PREFIXED)).len(), 1);
    }

    #[test]
    fn your_own_comment_without_the_prefix_and_a_marked_comment_run_nothing() {
        let r = rule(PREFIXED);
        assert!(!matches(&r, &comment("me", "OWNER", "just a note"), "me"));
        let marked = format!("{HIDDEN_MARKER} /playbook merge");
        assert!(!matches(&r, &comment("me", "OWNER", &marked), "me"));
    }

    #[test]
    fn filters_on_labels_and_authors_are_applied() {
        let r = rule(
            r#"{"on":"issue.opened","where":{"labels_any":["bug"],"authors":["alice"]},"do":"label","with":{"labels":["x"]}}"#,
        );
        let mut e = issue(1, "alice");
        assert!(!matches(&r, &e, "me"));
        e.labels = vec!["bug".into()];
        assert!(matches(&r, &e, "me"));
        e.author = "bob".into();
        assert!(!matches(&r, &e, "me"));
    }
}
