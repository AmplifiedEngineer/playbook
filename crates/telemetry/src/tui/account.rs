// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which account the view shows. Usage events carry an account key, and the
//! totals, the chart and the budget panel all read the events of one account,
//! so a second login does not add to the first one's numbers.

use crate::account::UNKNOWN_ACCOUNT;
use crate::{ToolInvocationEvent, UsageEvent};
use playbook_store::Connection;

/// What the user picked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AccountSel {
    /// The account of the newest event.
    #[default]
    Latest,
    Key(String),
    All,
}

/// What the picked account resolved to, for the header and the cycle key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountsView {
    /// The key whose events are shown, `None` for all accounts.
    pub key: Option<String>,
    /// The name for the header, empty when the store holds no events.
    pub shown: String,
    /// Every account with events as `(key, name)`, newest activity first.
    pub choices: Vec<(String, String)>,
}

impl AccountSel {
    /// The key to show among `choices`. `Latest` skips the `unknown` label
    /// that Codex events carry, when another account exists.
    pub fn resolve(&self, choices: &[(String, String)]) -> Option<String> {
        let has = |k: &str| choices.iter().any(|(c, _)| c == k);
        match self {
            AccountSel::All => None,
            AccountSel::Key(k) if has(k) => Some(k.clone()),
            _ => choices
                .iter()
                .find(|(k, _)| k != UNKNOWN_ACCOUNT)
                .or(choices.first())
                .map(|(k, _)| k.clone()),
        }
    }

    /// The next pick of the cycle key: each account in turn, then all of them,
    /// then back to the newest. With one account there is nothing to cycle.
    pub fn next(&self, shown: Option<&str>, choices: &[(String, String)]) -> AccountSel {
        if choices.len() < 2 {
            return self.clone();
        }
        let Some(current) = shown else {
            return AccountSel::Latest;
        };
        match choices.iter().position(|(k, _)| k == current) {
            Some(i) if i + 1 < choices.len() => AccountSel::Key(choices[i + 1].0.clone()),
            _ => AccountSel::All,
        }
    }

    /// The all accounts key: all, or back to the newest.
    pub fn toggle_all(&self) -> AccountSel {
        match self {
            AccountSel::All => AccountSel::Latest,
            _ => AccountSel::All,
        }
    }
}

/// Resolves `sel` against the accounts in the store.
pub fn view(conn: &Connection, sel: &AccountSel) -> Result<AccountsView, String> {
    let choices: Vec<(String, String)> = crate::account_store::accounts(conn)?
        .into_iter()
        .map(|a| (a.key, a.name))
        .collect();
    let key = sel.resolve(&choices);
    let shown = match (&key, choices.is_empty()) {
        (_, true) => String::new(),
        (None, _) => "all accounts".to_string(),
        (Some(k), _) => crate::account_store::display(&choices, k),
    };
    Ok(AccountsView {
        key,
        shown,
        choices,
    })
}

/// Drops the events of other accounts. `None` keeps everything.
pub fn keep(events: &mut Vec<UsageEvent>, key: Option<&str>) {
    if let Some(key) = key {
        events.retain(|e| e.account == key);
    }
}

/// Drops the tool events of other accounts. `None` keeps everything.
pub fn keep_tools(events: &mut Vec<ToolInvocationEvent>, key: Option<&str>) {
    if let Some(key) = key {
        events.retain(|e| e.account == key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<(String, String)> {
        [("work", "Work"), ("home", "home"), ("unknown", "unknown")]
            .map(|(k, n)| (k.to_string(), n.to_string()))
            .to_vec()
    }

    #[test]
    fn the_newest_account_is_the_default_and_unknown_never_wins_it() {
        assert_eq!(
            AccountSel::Latest.resolve(&choices()).as_deref(),
            Some("work")
        );
        let only_unknown = &choices()[2..];
        assert_eq!(
            AccountSel::Latest.resolve(only_unknown).as_deref(),
            Some("unknown")
        );
        let lead_unknown = [choices()[2].clone(), choices()[1].clone()];
        assert_eq!(
            AccountSel::Latest.resolve(&lead_unknown).as_deref(),
            Some("home")
        );
    }

    #[test]
    fn a_picked_key_that_vanished_falls_back_to_the_newest() {
        assert_eq!(
            AccountSel::Key("home".into())
                .resolve(&choices())
                .as_deref(),
            Some("home")
        );
        assert_eq!(
            AccountSel::Key("gone".into())
                .resolve(&choices())
                .as_deref(),
            Some("work")
        );
        assert_eq!(AccountSel::All.resolve(&choices()), None);
    }

    #[test]
    fn the_cycle_visits_each_account_then_all_then_the_newest() {
        let c = choices();
        let mut sel = AccountSel::Latest;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let shown = sel.resolve(&c);
            seen.push(shown.clone());
            sel = sel.next(shown.as_deref(), &c);
        }
        assert_eq!(
            seen,
            [
                Some("work".to_string()),
                Some("home".to_string()),
                Some("unknown".to_string()),
                None
            ]
        );
        assert_eq!(sel, AccountSel::Latest);
        assert_eq!(
            AccountSel::Latest.next(Some("work"), &c[..1]),
            AccountSel::Latest
        );
    }

    #[test]
    fn the_all_key_toggles_between_all_and_the_newest() {
        assert_eq!(AccountSel::Latest.toggle_all(), AccountSel::All);
        assert_eq!(AccountSel::Key("home".into()).toggle_all(), AccountSel::All);
        assert_eq!(AccountSel::All.toggle_all(), AccountSel::Latest);
    }

    #[test]
    fn keep_filters_to_one_account_or_none() {
        let event = |a: &str| UsageEvent {
            account: a.to_string(),
            ..UsageEvent::default()
        };
        let mut events = vec![event("a"), event("b"), event("a")];
        keep(&mut events, None);
        assert_eq!(events.len(), 3);
        keep(&mut events, Some("a"));
        assert_eq!(events.len(), 2);
    }
}
