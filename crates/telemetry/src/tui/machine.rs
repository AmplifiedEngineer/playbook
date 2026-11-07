// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which machine the machine panels show. Every row carries the id of the
//! computer that wrote it. Live, agents, skills, tools, health, sessions and
//! projects follow the pick, while the account panels count every machine.

use crate::query::short_id;
use crate::UsageEvent;
use playbook_store::Connection;

/// What the user picked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MachineSel {
    /// The computer running pbtop.
    #[default]
    This,
    /// One other machine, by id.
    Key(String),
    All,
}

/// What the pick resolved to, for the header and the cycle key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachinesView {
    /// The id whose rows are shown, `None` for all machines.
    pub key: Option<String>,
    /// The id of this machine.
    pub local: String,
    /// The name for the header, empty when the store holds no events.
    pub shown: String,
    /// This machine first, then every other machine with events, newest first.
    pub choices: Vec<String>,
}

const ALL_MACHINES: &str = "all machines";
const THIS_MACHINE: &str = "this machine";
const KEY_PREFIX: &str = "key:";
const THIS_TEXT: &str = "this";
const ALL_TEXT: &str = "all";

impl MachineSel {
    /// The id to show among `choices`, `None` for all machines. A picked id
    /// that has no rows any more falls back to this machine.
    pub fn resolve(&self, local: &str, choices: &[String]) -> Option<String> {
        match self {
            MachineSel::All => None,
            MachineSel::Key(k) if choices.contains(k) => Some(k.clone()),
            _ => Some(local.to_string()),
        }
    }

    /// The next pick of the cycle key: this machine, each other machine in
    /// turn, then all of them. With one machine there is nothing to cycle.
    pub fn next(&self, shown: Option<&str>, choices: &[String]) -> MachineSel {
        if choices.len() < 2 {
            return self.clone();
        }
        let Some(current) = shown else {
            return MachineSel::This;
        };
        match choices.iter().position(|c| c == current) {
            Some(i) if i + 1 < choices.len() => MachineSel::Key(choices[i + 1].clone()),
            _ => MachineSel::All,
        }
    }

    pub fn to_text(&self) -> String {
        match self {
            MachineSel::This => THIS_TEXT.to_string(),
            MachineSel::All => ALL_TEXT.to_string(),
            MachineSel::Key(id) => format!("{KEY_PREFIX}{id}"),
        }
    }

    /// Reads a stored pick. Anything odd gives `This`.
    pub fn parse(text: &str) -> MachineSel {
        match text.strip_prefix(KEY_PREFIX) {
            Some(id) if !id.is_empty() => MachineSel::Key(id.to_string()),
            _ if text == ALL_TEXT => MachineSel::All,
            _ => MachineSel::This,
        }
    }
}

impl MachinesView {
    /// Names this machine in the header once its name is known.
    pub fn name_local(&mut self, name: &str) {
        if self.key.as_deref() == Some(self.local.as_str()) {
            self.shown = if name.is_empty() {
                THIS_MACHINE.to_string()
            } else {
                name.to_string()
            };
        }
    }
}

/// Resolves `sel` against the machines in the store.
pub fn view(conn: &Connection, sel: &MachineSel) -> Result<MachinesView, String> {
    let local = crate::machine_rows::local_machine_id(conn)?;
    let mut choices = vec![local.clone()];
    choices.extend(others(conn, &local)?);
    let key = sel.resolve(&local, &choices);
    let shown = match &key {
        None => ALL_MACHINES.to_string(),
        Some(k) if *k == local => THIS_MACHINE.to_string(),
        Some(k) => short_id(k),
    };
    Ok(MachinesView {
        key,
        local,
        shown,
        choices,
    })
}

/// The other machines that wrote events, newest activity first.
fn others(conn: &Connection, local: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT machine FROM usage_events WHERE machine <> '' AND machine <> ?1
             GROUP BY machine ORDER BY MAX(timestamp) DESC, machine",
        )
        .map_err(|e| format!("failed to read the machines: {e}"))?;
    let rows = stmt
        .query_map([local], |r| r.get(0))
        .map_err(|e| format!("failed to read the machines: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a machine: {e}"))
}

/// Drops the events of other machines. `None` keeps everything.
pub fn keep(events: &mut Vec<UsageEvent>, key: Option<&str>) {
    if let Some(key) = key {
        events.retain(|e| e.machine == key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<String> {
        ["me", "b", "c"].map(String::from).to_vec()
    }

    #[test]
    fn this_machine_is_the_default_and_a_vanished_pick_falls_back_to_it() {
        assert_eq!(
            MachineSel::This.resolve("me", &choices()).as_deref(),
            Some("me")
        );
        assert_eq!(
            MachineSel::Key("b".into())
                .resolve("me", &choices())
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            MachineSel::Key("gone".into())
                .resolve("me", &choices())
                .as_deref(),
            Some("me")
        );
        assert_eq!(MachineSel::All.resolve("me", &choices()), None);
    }

    #[test]
    fn the_cycle_visits_this_machine_each_other_machine_then_all() {
        let c = choices();
        let mut sel = MachineSel::This;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let shown = sel.resolve("me", &c);
            seen.push(shown.clone());
            sel = sel.next(shown.as_deref(), &c);
        }
        assert_eq!(
            seen,
            [
                Some("me".to_string()),
                Some("b".into()),
                Some("c".into()),
                None
            ]
        );
        assert_eq!(sel, MachineSel::This);
    }

    #[test]
    fn with_one_machine_the_cycle_keeps_the_pick() {
        let one = ["me".to_string()];
        assert_eq!(MachineSel::This.next(Some("me"), &one), MachineSel::This);
    }

    #[test]
    fn a_pick_round_trips_through_text_and_odd_text_means_this_machine() {
        for sel in [
            MachineSel::This,
            MachineSel::All,
            MachineSel::Key("abc".into()),
        ] {
            assert_eq!(MachineSel::parse(&sel.to_text()), sel);
        }
        for odd in ["", "key:", "ALL", "key", "7"] {
            assert_eq!(MachineSel::parse(odd), MachineSel::This);
        }
    }

    #[test]
    fn keep_filters_to_one_machine_or_none() {
        let event = |m: &str| UsageEvent {
            machine: m.to_string(),
            ..UsageEvent::default()
        };
        let mut events = vec![event("a"), event("b"), event("a")];
        keep(&mut events, None);
        assert_eq!(events.len(), 3);
        keep(&mut events, Some("a"));
        assert_eq!(events.len(), 2);
    }
}
