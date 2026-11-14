// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The data the terminal view's sessions panel shows: a record per agent
//! session and the small formatters for it. The host program builds the
//! records (see `host::load_sessions`); this module only defines and prints
//! them.

const DAY: i64 = 86_400;

/// A pull request linked to a session by its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrKind {
    Open,
    Merged,
    Closed,
}

impl PrKind {
    pub fn label(self) -> &'static str {
        match self {
            PrKind::Open => "open",
            PrKind::Merged => "merged",
            PrKind::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub kind: PrKind,
}

/// Where the session ran, as far as git can tell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    /// The working directory the transcript recorded.
    pub cwd: Option<String>,
    /// Root of a linked worktree, when the directory is one.
    pub worktree: Option<String>,
    /// Which playbook convention made the worktree, empty when none did.
    pub convention: &'static str,
    /// The directory no longer exists.
    pub gone: bool,
    /// Uncommitted or untracked changes. `None` when it could not be read.
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Waiting,
    Idle,
    Stale,
    Done,
    Crashed,
    Closed,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Idle => "idle",
            State::Stale => "stale",
            State::Done => "done",
            State::Crashed => "crashed",
            State::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: String,
    pub agent: String,
    pub repo: String,
    pub branch: String,
    pub place: Place,
    /// Issue number read from the branch name. A guess, shown as inferred.
    pub issue: Option<u64>,
    pub pr: Option<Pr>,
    pub state: State,
    pub last_activity: i64,
    /// Prompt size of the newest request: input plus cache read plus cache write.
    pub context_tokens: u64,
    pub cost_usd: f64,
    pub unpriced: usize,
    pub messages: usize,
    /// A person has to look at this one: it is finished or stale and holds
    /// changes that are not committed.
    pub needs_attention: bool,
    /// What a person could do. Never run by playbook.
    pub hint: String,
}

/// `3m`, `2h`, `4d` since `then`, or `now` under a minute.
pub fn age(then: i64, now: i64) -> String {
    let s = (now - then).max(0);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / DAY),
    }
}

/// `182k` or `1.2M` tokens, `-` for none.
pub fn tokens(n: u64) -> String {
    match n {
        0 => "-".into(),
        1..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

pub fn link_label(r: &Record) -> String {
    match (r.pr, r.issue) {
        (Some(pr), _) => format!("PR #{} {}", pr.number, pr.kind.label()),
        (None, Some(n)) => format!("issue #{n}?"),
        (None, None) => "-".into(),
    }
}

pub fn worktree_label(r: &Record) -> String {
    if r.place.gone {
        return "gone".into();
    }
    match &r.place.worktree {
        Some(path) => {
            let name = path.rsplit('/').next().unwrap_or(path);
            if r.place.convention.is_empty() {
                name.to_string()
            } else {
                format!("{name} ({})", r.place.convention)
            }
        }
        None if r.place.cwd.is_some() => "main checkout".into(),
        None => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    #[test]
    fn ages_and_token_counts_read_short() {
        assert_eq!(age(NOW - 5, NOW), "now");
        assert_eq!(age(NOW - 90, NOW), "1m");
        assert_eq!(age(NOW - 7300, NOW), "2h");
        assert_eq!(age(NOW - 3 * DAY, NOW), "3d");
        assert_eq!(tokens(0), "-");
        assert_eq!(tokens(182_000), "182k");
        assert_eq!(tokens(1_250_000), "1.2M");
    }

    #[test]
    fn every_state_has_its_label() {
        let labels = [
            (State::Working, "working"),
            (State::Waiting, "waiting"),
            (State::Idle, "idle"),
            (State::Stale, "stale"),
            (State::Done, "done"),
            (State::Crashed, "crashed"),
            (State::Closed, "closed"),
        ];
        for (state, want) in labels {
            assert_eq!(state.label(), want);
        }
    }

    fn record() -> Record {
        Record {
            id: "s".into(),
            agent: "claude".into(),
            repo: "r".into(),
            branch: "b".into(),
            place: Place::default(),
            issue: None,
            pr: None,
            state: State::Idle,
            last_activity: 0,
            context_tokens: 0,
            cost_usd: 0.0,
            unpriced: 0,
            messages: 0,
            needs_attention: false,
            hint: String::new(),
        }
    }

    #[test]
    fn the_link_label_prefers_a_pr_then_an_inferred_issue() {
        let mut r = record();
        assert_eq!(link_label(&r), "-");
        r.issue = Some(7);
        assert_eq!(link_label(&r), "issue #7?");
        r.pr = Some(Pr {
            number: 9,
            kind: PrKind::Merged,
        });
        assert_eq!(link_label(&r), "PR #9 merged");
    }

    #[test]
    fn the_worktree_label_names_gone_linked_and_main_checkouts() {
        let mut r = record();
        assert_eq!(worktree_label(&r), "-");
        r.place.cwd = Some("/a/b".into());
        assert_eq!(worktree_label(&r), "main checkout");
        r.place.worktree = Some("/a/wt/feat".into());
        assert_eq!(worktree_label(&r), "feat");
        r.place.convention = "playbook";
        assert_eq!(worktree_label(&r), "feat (playbook)");
        r.place.gone = true;
        assert_eq!(worktree_label(&r), "gone");
    }
}
