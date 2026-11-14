// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which account the view shows. Usage events carry an account key, and the
//! totals, the chart and the budget panel all read the events of one account,
//! so a second login does not add to the first one's numbers.

use crate::account::UNKNOWN_ACCOUNT;
use crate::{ToolInvocationEvent, UsageEvent};
use playbook_store::Connection;

/// The header text when every account is shown.
const ALL_ACCOUNTS: &str = "all accounts";

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
    /// The name for the header, empty when the store holds no events. An
    /// unnamed email account shows its full address.
    pub shown: String,
    /// The header name with an unnamed email account cut to its domain, for
    /// screenshots. Equal to `shown` for every other account.
    pub masked: String,
    /// Every account with events as `(key, name)`, newest activity first.
    pub choices: Vec<(String, String)>,
}

impl AccountsView {
    /// The header name, masked or not.
    pub fn label(&self, mask: bool) -> &str {
        if mask {
            &self.masked
        } else {
            &self.shown
        }
    }
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
    let (shown, masked) = match (&key, choices.is_empty()) {
        (_, true) => (String::new(), String::new()),
        (None, _) => (ALL_ACCOUNTS.to_string(), ALL_ACCOUNTS.to_string()),
        (Some(k), _) => {
            let name = crate::account_store::display(&choices, k);
            let masked = header_label(k, &name, &choices);
            (name, masked)
        }
    };
    Ok(AccountsView {
        key,
        shown,
        masked,
        choices,
    })
}

/// The masked header text for `key`. An unnamed email key shows only `@` and its first
/// domain label, which keeps the address out of screenshots. When two unnamed
/// keys cut to the same text, each gets its place among them, so cycling
/// accounts still changes the header.
pub(crate) fn header_label(key: &str, name: &str, choices: &[(String, String)]) -> String {
    let Some(label) = email_cut(key, name) else {
        return name.to_string();
    };
    let clashing: Vec<&str> = choices
        .iter()
        .filter(|(k, n)| email_cut(k, n).as_deref() == Some(label.as_str()))
        .map(|(k, _)| k.as_str())
        .collect();
    match clashing.iter().position(|k| *k == key) {
        Some(at) if clashing.len() > 1 => format!("{label} ({})", at + 1),
        _ => label,
    }
}

/// `None` for a named account or a key with no local part to hide.
fn email_cut(key: &str, name: &str) -> Option<String> {
    if name != key {
        return None;
    }
    let (local, domain) = key.split_once('@')?;
    if local.is_empty() {
        return None;
    }
    let host = domain
        .split('.')
        .find(|part| !part.is_empty())
        .unwrap_or("");
    Some(format!("@{host}"))
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

    fn label(key: &str) -> String {
        header_label(key, key, &[(key.to_string(), key.to_string())])
    }

    #[test]
    fn an_unnamed_email_account_shows_only_its_domain_name() {
        assert_eq!(label("dev@example.com.br"), "@example");
        assert_eq!(label("a@example.com"), "@example");
        assert_eq!(label("dev@.com"), "@com");
        assert_eq!(label("dev@"), "@");
    }

    #[test]
    fn named_plain_and_unknown_accounts_show_unchanged() {
        assert_eq!(header_label("dev@example.com.br", "Work", &[]), "Work");
        assert_eq!(label("home"), "home");
        assert_eq!(label(UNKNOWN_ACCOUNT), UNKNOWN_ACCOUNT);
        assert_eq!(label("@x.com"), "@x.com");
    }

    #[test]
    fn the_view_label_is_the_full_email_unless_the_mask_is_on() {
        let view = AccountsView {
            shown: "dev@example.com".into(),
            masked: "@example".into(),
            ..AccountsView::default()
        };

        assert_eq!(view.label(false), "dev@example.com");
        assert_eq!(view.label(true), "@example");
    }

    #[test]
    fn unnamed_emails_on_one_domain_get_their_place() {
        let choices: Vec<(String, String)> =
            ["dev@example.com", "ana@example.org", "bo@work.invalid"]
                .map(|k| (k.to_string(), k.to_string()))
                .to_vec();
        let shown = |key: &str| header_label(key, key, &choices);
        assert_eq!(shown("dev@example.com"), "@example (1)");
        assert_eq!(shown("ana@example.org"), "@example (2)");
        assert_eq!(shown("bo@work.invalid"), "@work");
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
