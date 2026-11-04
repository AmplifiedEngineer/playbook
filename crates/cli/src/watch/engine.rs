// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Turns events into proposals and carries out the ones you approve or that a
//! rule runs on its own. The actions are `label` and `comment` with fixed
//! text: neither reads text from GitHub. Everything the actions need from
//! GitHub sits behind [`GithubWrite`].

use serde_json::json;

use super::audit::AuditLog;
use super::backoff::{delay, Jitter, MAX_ATTEMPTS};
use super::clock::Clock;
use super::event::Event;
use super::limits::{check, day_of, ladder_allows, Caps, GlobalCaps, Usage};
use super::rule::{Autonomy, Do, On, Rule};
use super::settings::effective;
use super::store::{Cursor, Insert, NewWork, Store, StoreError, Tx, WorkRow};

/// The marker an automated comment carries so watch mode ignores its own text.
pub const HIDDEN_MARKER: &str = "<!-- playbook-watch -->";

/// The reason on an approved item that a rule queued itself. An item a person
/// approved has none, and the day limits and the ladder apply to the first
/// only.
const AUTO: &str = "auto";

/// What an action reads about an issue or pull request before it acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueView {
    pub open: bool,
    pub labels: Vec<String>,
    /// SHA-256 of the body, the same hash an event carries.
    pub body_hash: [u8; 32],
}

/// The GitHub calls the actions make.
pub trait GithubWrite {
    fn issue(&self, repo: &str, number: u64) -> Result<IssueView, String>;
    fn add_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String>;
    /// Whether a comment on the issue or pull request holds `marker`.
    fn has_comment(&self, repo: &str, number: u64, marker: &str) -> Result<bool, String>;
    fn add_comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String>;
}

/// A rule with the id it is stored under.
pub type NamedRule = (String, Rule);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// Another process holds the lock, so it will run the item.
    Handed,
    /// The action was taken.
    Done,
    /// The effect was already there.
    AlreadyDone,
    /// The issue was closed, its body changed, or the rule is gone.
    Skipped(&'static str),
    /// A cap or the trust ladder sent the item back to a proposal.
    Demoted(&'static str),
    /// GitHub failed. The item will be tried again later.
    Retry,
    /// GitHub failed too many times. The item is on the dead list.
    Dead,
}

#[derive(Debug)]
pub enum EngineError {
    Store(StoreError),
    /// The id is unknown or the item is not waiting for approval.
    NotProposed(i64),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => e.fmt(f),
            Self::NotProposed(id) => write!(f, "item {id} is not waiting for approval"),
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

/// What one try at an action found.
enum Step {
    Done,
    AlreadyDone,
    Skipped(&'static str),
}

pub struct Engine<'a, W, C> {
    pub store: &'a mut Store,
    pub write: &'a W,
    pub clock: &'a C,
    pub audit: &'a AuditLog,
    pub jitter: &'a dyn Jitter,
    /// The caps from `watch.limits.*`.
    pub caps: GlobalCaps,
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

    /// Saves the cursor and queues work for every rule an event matches, in
    /// one transaction: a proposal, or for a rule running on its own an
    /// approved item. Events older than `max_catchup` seconds are audited as
    /// stale and skipped. Returns the ids of the new items.
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
                    let level = effective(rule.autonomy, ceiling);
                    if level == Autonomy::Off || !matches(rule, event, self.me) {
                        continue;
                    }
                    let key = format!("{}:{id}", event.key);
                    if now - event.updated_at > max_catchup {
                        decisions.push((id.clone(), key, "stale"));
                        continue;
                    }
                    let item = Queued {
                        event,
                        rule: id,
                        class: rule.action.as_str(),
                        key: &key,
                        auto: level == Autonomy::Auto,
                    };
                    ids.extend(queue(tx, &item, now, &mut decisions)?);
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
        let now = self.clock.now();
        self.store.transaction(|tx| {
            tx.set_state(id, "approved", None)?;
            tx.stamp_approved(id, now)
        })?;
        if !lock_free {
            self.record(&row.rule, &row.key, "approved");
            return Ok(ApproveOutcome::Handed);
        }
        let row = WorkRow {
            state: "approved".to_string(),
            reason: None,
            ..row
        };
        self.run(id, &row, rules)
    }

    /// Runs every approved item of `repo` that is due, with that repo's rules.
    /// The lock holder calls this on each cycle.
    pub fn run_approved(&mut self, repo: &str, rules: &[NamedRule]) -> Result<usize, EngineError> {
        let mut ran = 0;
        for id in self.store.ids_due("approved", self.clock.now())? {
            if let Some(row) = self.store.work(id)?.filter(|r| r.repo == repo) {
                self.run(id, &row, rules)?;
                ran += 1;
            }
        }
        Ok(ran)
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
        if row.reason.as_deref() == Some(AUTO) {
            if let Some((reason, detail)) = self.demotion(row, rule)? {
                self.set(id, "proposed", Some(reason))?;
                self.record(&row.rule, &row.key, &format!("{reason}:{detail}"));
                return Ok(ApproveOutcome::Demoted(reason));
            }
        }
        match self.perform(row, rule) {
            Ok(Step::Done) => self.finish(id, row, ApproveOutcome::Done),
            Ok(Step::AlreadyDone) => self.finish(id, row, ApproveOutcome::AlreadyDone),
            Ok(Step::Skipped(why)) => self.finish(id, row, ApproveOutcome::Skipped(why)),
            Err(why) => self.fail(id, row, &why),
        }
    }

    /// Why an automatic item may not run now: a cap, or an action class that
    /// has not earned the right yet.
    fn demotion(
        &self,
        row: &WorkRow,
        rule: &Rule,
    ) -> Result<Option<(&'static str, String)>, EngineError> {
        let now = self.clock.now();
        let day = day_of(now);
        let (rule_actions, rule_usd) = self.store.counter(day, &row.rule)?;
        let (global_actions, global_usd) = self.store.counter(day, "*")?;
        let usage = Usage {
            rule_actions,
            rule_usd,
            global_actions,
            global_usd,
        };
        let caps = Caps {
            rule_actions: rule.limits.actions_per_day,
            rule_usd: rule.limits.usd_per_day,
            global_actions: self.caps.actions_per_day,
            global_usd: self.caps.usd_per_day,
        };
        let last = self.store.last_acted(&row.rule, &row.repo, row.object)?;
        if let Err(limit) = check(&usage, &caps, 0.0, last, now) {
            return Ok(Some(("limit", limit.as_str().to_string())));
        }
        if !rule.action.runs_free() {
            let history = self.store.approvals(rule.action.as_str())?;
            if let Err(why) = ladder_allows(&history, now) {
                return Ok(Some(("ladder", why)));
            }
        }
        Ok(None)
    }

    /// The world may have changed since the proposal: check before acting,
    /// and check that the effect is not already there.
    fn perform(&self, row: &WorkRow, rule: &Rule) -> Result<Step, String> {
        let view = self.write.issue(&row.repo, row.object)?;
        if !view.open {
            return Ok(Step::Skipped("closed"));
        }
        // Only these events carry the hash of the body that `issue` returns.
        let opened = matches!(rule.on, On::IssueOpened | On::PrOpened);
        if opened && view.body_hash[..] != row.body_hash[..] {
            return Ok(Step::Skipped("body_changed"));
        }
        match rule.action {
            Do::Label => {
                let missing: Vec<String> = rule
                    .with
                    .labels
                    .iter()
                    .filter(|l| !view.labels.contains(l))
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    return Ok(Step::AlreadyDone);
                }
                self.write.add_labels(&row.repo, row.object, &missing)?;
            }
            Do::Comment => {
                let marker = format!("<!-- playbook-watch:{} -->", row.key);
                if self.write.has_comment(&row.repo, row.object, &marker)? {
                    return Ok(Step::AlreadyDone);
                }
                let text = rule.with.text.as_deref().unwrap_or_default();
                let body = format!("{text}\n\n{HIDDEN_MARKER}\n{marker}");
                self.write.add_comment(&row.repo, row.object, &body)?;
            }
        }
        Ok(Step::Done)
    }

    /// A failed try goes back to `approved` with a later `not_before`, so an
    /// approval is not lost. After too many tries the item is dead.
    fn fail(&mut self, id: i64, row: &WorkRow, why: &str) -> Result<ApproveOutcome, EngineError> {
        let attempts = row.attempts + 1;
        if attempts >= MAX_ATTEMPTS {
            self.store
                .transaction(|tx| tx.set_retry(id, "dead", Some(why), attempts, 0))?;
            self.record(&row.rule, &row.key, "dead");
            return Ok(ApproveOutcome::Dead);
        }
        let not_before = self.clock.now() + delay(row.attempts, self.jitter);
        self.store.transaction(|tx| {
            tx.set_retry(id, "approved", row.reason.as_deref(), attempts, not_before)
        })?;
        self.record(&row.rule, &row.key, "retry");
        Ok(ApproveOutcome::Retry)
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
            _ => "approved",
        };
        let reason = match &outcome {
            ApproveOutcome::Skipped(_) => "skipped",
            _ => label,
        };
        let now = self.clock.now();
        self.store.transaction(|tx| {
            tx.set_state(id, "done", Some(reason))?;
            if outcome == ApproveOutcome::Done {
                tx.count_action(day_of(now), row, 0.0, now)?;
            }
            Ok(())
        })?;
        self.record(&row.rule, &row.key, label);
        Ok(outcome)
    }
}

/// One event a rule matched, on its way into the queue.
struct Queued<'a> {
    event: &'a Event,
    rule: &'a str,
    class: &'a str,
    key: &'a str,
    /// The rule runs on its own, so the item starts approved.
    auto: bool,
}

/// Queues one item inside the transaction, or notes that it was a repeat.
fn queue(
    tx: &Tx<'_>,
    queued: &Queued<'_>,
    now: i64,
    decisions: &mut Vec<(String, String, &'static str)>,
) -> Result<Option<i64>, StoreError> {
    let item = NewWork {
        key: queued.key,
        rule: queued.rule,
        class: queued.class,
        repo: &queued.event.repo,
        object: queued.event.number,
        body_hash: queued.event.body_hash,
    };
    let record = |outcome| (queued.rule.to_string(), queued.key.to_string(), outcome);
    match tx.insert_work(&item, now)? {
        Insert::Queued(id) if queued.auto => {
            tx.set_state(id, "approved", Some(AUTO))?;
            decisions.push(record("queued_auto"));
            Ok(Some(id))
        }
        Insert::Queued(id) => {
            tx.set_state(id, "proposed", None)?;
            decisions.push(record("proposed"));
            Ok(Some(id))
        }
        Insert::Dead(_) => {
            decisions.push(record("queue_full"));
            Ok(None)
        }
        Insert::Duplicate => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::backoff::Jitter;
    use crate::watch::clock::ManualClock;
    use crate::watch::rule::Stored;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Spy {
        calls: RefCell<Vec<(String, u64, Vec<String>)>>,
        closed: bool,
        have: Vec<String>,
        /// The hash the issue body has now. Events in these tests carry [1; 32].
        hash: Option<[u8; 32]>,
        /// Comments already on the issue, as markers.
        markers: Vec<String>,
        /// Fail every write with this error.
        fail: Option<String>,
        comments: RefCell<Vec<(u64, String)>>,
    }

    impl GithubWrite for Spy {
        fn issue(&self, _: &str, _: u64) -> Result<IssueView, String> {
            Ok(IssueView {
                open: !self.closed,
                labels: self.have.clone(),
                body_hash: self.hash.unwrap_or([1; 32]),
            })
        }
        fn has_comment(&self, _: &str, _: u64, marker: &str) -> Result<bool, String> {
            Ok(self.markers.iter().any(|m| m == marker))
        }
        fn add_comment(&self, _: &str, number: u64, body: &str) -> Result<(), String> {
            self.comments.borrow_mut().push((number, body.to_string()));
            Ok(())
        }
        fn add_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
            if let Some(why) = &self.fail {
                return Err(why.clone());
            }
            self.calls
                .borrow_mut()
                .push((repo.into(), number, labels.to_vec()));
            Ok(())
        }
    }

    /// Always picks the longest wait.
    struct Longest;
    impl Jitter for Longest {
        fn pick(&self, ceiling: i64) -> i64 {
            ceiling
        }
    }

    const AUTO_LABEL: &str =
        r#"{"on":"issue.opened","do":"label","autonomy":"auto","with":{"labels":["triage"]}}"#;
    const COMMENT: &str = r#"{"on":"issue.opened","do":"comment","autonomy":"auto","with":{"text":"Thanks, we will look."}}"#;
    const CAPS: GlobalCaps = GlobalCaps {
        actions_per_day: 50,
        usd_per_day: 5.0,
    };
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
        ingest_at(f, spy, events, rules, Autonomy::Propose)
    }

    fn ingest_at(
        f: &mut Fixture,
        spy: &Spy,
        events: &[Event],
        rules: &[NamedRule],
        ceiling: Autonomy,
    ) -> Vec<i64> {
        engine(f, spy)
            .ingest(
                "issues",
                &Cursor::default(),
                events,
                rules,
                ceiling,
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
            jitter: &Longest,
            caps: CAPS,
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

        let ran = engine(&mut f, &spy)
            .run_approved("o/r", &rules(LABEL))
            .unwrap();
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
    fn a_body_edited_after_the_proposal_is_skipped_and_nothing_is_written() {
        // Arrange
        let mut f = fixture("watch-engine-edited");
        let spy = Spy {
            hash: Some([2; 32]),
            ..Spy::default()
        };
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];

        // Act
        let out = engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .unwrap();

        // Assert
        assert_eq!(out, ApproveOutcome::Skipped("body_changed"));
        assert!(spy.calls.borrow().is_empty());
        let row = f.store.work(id).unwrap().unwrap();
        assert_eq!(row.reason.as_deref(), Some("skipped"));
    }

    fn state(f: &Fixture, id: i64) -> (String, Option<String>) {
        let row = f.store.work(id).unwrap().unwrap();
        (row.state, row.reason)
    }

    fn actions_today(f: &Fixture) -> i64 {
        f.store.counter(day_of(f.clock.now()), "*").unwrap().0
    }

    #[test]
    fn a_rule_at_auto_labels_once_and_a_repeat_adds_no_call() {
        // Arrange
        let mut f = fixture("watch-engine-auto");
        let spy = Spy::default();
        let events = [issue(7, "a")];
        let rules = rules(AUTO_LABEL);

        // Act
        ingest_at(&mut f, &spy, &events, &rules, Autonomy::Auto);
        engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();
        ingest_at(&mut f, &spy, &events, &rules, Autonomy::Auto);
        engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();

        // Assert
        assert_eq!(spy.calls.borrow().len(), 1);
        assert_eq!(actions_today(&f), 1);
    }

    #[test]
    fn a_rule_at_auto_under_a_propose_ceiling_only_proposes() {
        let mut f = fixture("watch-engine-ceiling");
        let spy = Spy::default();
        let id = ingest_at(
            &mut f,
            &spy,
            &[issue(7, "a")],
            &rules(AUTO_LABEL),
            Autonomy::Propose,
        )[0];
        assert_eq!(state(&f, id).0, "proposed");
        engine(&mut f, &spy)
            .run_approved("o/r", &rules(AUTO_LABEL))
            .unwrap();
        assert!(spy.calls.borrow().is_empty());
        assert_eq!(actions_today(&f), 0);
    }

    #[test]
    fn an_auto_item_over_the_rule_limit_is_demoted_to_a_proposal() {
        // Arrange: the rule may act once a day.
        let mut f = fixture("watch-engine-limit");
        let spy = Spy::default();
        let one = AUTO_LABEL.replace(
            r#""autonomy":"auto""#,
            r#""autonomy":"auto","limits":{"actions_per_day":1}"#,
        );
        let rules = rules(&one);

        // Act
        let ids = ingest_at(
            &mut f,
            &spy,
            &[issue(7, "a"), issue(8, "a")],
            &rules,
            Autonomy::Auto,
        );
        engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();

        // Assert
        assert_eq!(state(&f, ids[0]).0, "done");
        assert_eq!(state(&f, ids[1]), ("proposed".into(), Some("limit".into())));
        assert_eq!(spy.calls.borrow().len(), 1);
    }

    #[test]
    fn the_global_daily_cap_demotes_and_a_person_can_still_approve() {
        let mut f = fixture("watch-engine-global");
        let spy = Spy::default();
        let rules = rules(AUTO_LABEL);
        let ids = ingest_at(&mut f, &spy, &[issue(7, "a")], &rules, Autonomy::Auto);
        let mut e = engine(&mut f, &spy);
        e.caps.actions_per_day = 0;
        e.run_approved("o/r", &rules).unwrap();
        assert_eq!(state(&f, ids[0]), ("proposed".into(), Some("limit".into())));

        // A person's approval is not held to the automatic caps.
        let out = engine(&mut f, &spy).approve(ids[0], &rules, true).unwrap();
        assert_eq!(out, ApproveOutcome::Done);
    }

    #[test]
    fn a_proposal_does_not_count_and_an_approved_item_that_runs_counts_once() {
        let mut f = fixture("watch-engine-counter");
        let spy = Spy::default();
        let id = ingest(&mut f, &spy, &[issue(7, "a")], &rules(LABEL))[0];
        assert_eq!(actions_today(&f), 0);
        engine(&mut f, &spy)
            .approve(id, &rules(LABEL), true)
            .unwrap();
        assert_eq!(actions_today(&f), 1);
    }

    #[test]
    fn the_comment_action_posts_the_rule_text_with_a_marker_and_not_twice() {
        // Arrange
        let mut f = fixture("watch-engine-comment");
        let spy = Spy::default();
        let rules = rules(COMMENT);
        let ids = ingest_at(&mut f, &spy, &[issue(7, "a")], &rules, Autonomy::Auto);

        // Act
        engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();

        // Assert
        let posted = spy.comments.borrow().clone();
        assert_eq!(posted.len(), 1);
        assert!(posted[0].1.starts_with("Thanks, we will look.\n\n"));
        assert!(posted[0].1.contains(HIDDEN_MARKER));
        assert!(posted[0]
            .1
            .contains("<!-- playbook-watch:issue:7:opened:r -->"));
        assert_eq!(state(&f, ids[0]).0, "done");
    }

    #[test]
    fn the_comment_action_adds_nothing_when_its_marker_is_there() {
        let mut f = fixture("watch-engine-comment-marker");
        let spy = Spy {
            markers: vec!["<!-- playbook-watch:issue:7:opened:r -->".into()],
            ..Spy::default()
        };
        let rules = rules(COMMENT);
        ingest_at(&mut f, &spy, &[issue(7, "a")], &rules, Autonomy::Auto);
        engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();
        assert!(spy.comments.borrow().is_empty());
    }

    #[test]
    fn a_failed_write_is_retried_later_and_keeps_its_approval() {
        // Arrange
        let mut f = fixture("watch-engine-retry");
        let spy = Spy {
            fail: Some("HTTP 502".into()),
            ..Spy::default()
        };
        let rules = rules(AUTO_LABEL);
        let id = ingest_at(&mut f, &spy, &[issue(7, "a")], &rules, Autonomy::Auto)[0];

        // Act
        let out = engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();

        // Assert: back to approved, one try counted, due 30 seconds on.
        assert_eq!(out, 1);
        let row = f.store.work(id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.attempts, row.not_before),
            ("approved", 1, 1_030)
        );
        // Not due yet, so a second cycle leaves it alone.
        assert_eq!(engine(&mut f, &spy).run_approved("o/r", &rules).unwrap(), 0);
        f.clock.advance(30);
        assert_eq!(engine(&mut f, &spy).run_approved("o/r", &rules).unwrap(), 1);
    }

    #[test]
    fn the_fifth_failed_try_moves_the_item_to_the_dead_list_with_the_reason() {
        let mut f = fixture("watch-engine-dead");
        let spy = Spy {
            fail: Some("HTTP 502".into()),
            ..Spy::default()
        };
        let rules = rules(AUTO_LABEL);
        let id = ingest_at(&mut f, &spy, &[issue(7, "a")], &rules, Autonomy::Auto)[0];
        for _ in 0..5 {
            f.clock.advance(4_000);
            engine(&mut f, &spy).run_approved("o/r", &rules).unwrap();
        }
        assert_eq!(state(&f, id), ("dead".into(), Some("HTTP 502".into())));
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
