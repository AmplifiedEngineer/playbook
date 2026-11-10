// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The highest effort worth paying for on a model tier.
//!
//! The Haiku 5.5 evaluation (#576) measured the cost of effort on Haiku: at
//! `xhigh` it costs 2x `medium` for the same pass rate, and at `max` it costs
//! 13x (about 7,100 thinking tokens and 32 s per call) and scores worst
//! (91% against 97% at `medium`). So:
//!
//! - `max` is never used on Haiku, whatever a role asks for.
//! - Haiku roles stop at `medium` by default.
//! - A role may opt in to a higher level, up to `xhigh`, only with a recorded
//!   reason in `OPT_INS`. No role has one today.
//!
//! The cap is a ceiling like the others: the lowest one wins, and it never
//! raises anything.

use super::{lower, rank};

/// The level no Haiku role may exceed, opt-in or not.
pub const HAIKU_HARD_CEILING: &str = "xhigh";

/// The level a Haiku role gets when it has no opt-in.
pub const HAIKU_DEFAULT_CAP: &str = "medium";

/// Roles that may run Haiku above the default cap: (component name, level,
/// reason). The level is itself held to `HAIKU_HARD_CEILING`.
pub const OPT_INS: [(&str, &str, &str); 0] = [];

/// Whether `model` (an alias such as `haiku` or an id such as
/// `claude-haiku-5-5`) is a Haiku model.
fn is_haiku(model: &str) -> bool {
    model
        .trim_matches(['"', '\''])
        .to_ascii_lowercase()
        .contains("haiku")
}

/// The level `component` opted in to in `table`, if it has an entry.
fn opt_in_level<'a>(table: &[(&'a str, &'a str, &'a str)], component: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(name, _, _)| *name == component)
        .map(|(_, level, _)| *level)
}

/// The ceiling `model` puts on `component` given the opt-in `table`, `None`
/// when the model has none.
fn cap_in<'a>(
    table: &[(&'a str, &'a str, &'a str)],
    model: Option<&str>,
    component: &str,
) -> Option<&'a str> {
    if !model.is_some_and(is_haiku) {
        return None;
    }
    let wanted = opt_in_level(table, component).unwrap_or(HAIKU_DEFAULT_CAP);
    lower(Some(wanted), Some(HAIKU_HARD_CEILING))
}

/// The ceiling `model` puts on `component`, `None` when the model has none.
pub fn cap_for(model: Option<&str>, component: &str) -> Option<&'static str> {
    cap_in(&OPT_INS, model, component)
}

/// `level` held to what `model` allows for `component`.
pub fn clamp<'a>(model: Option<&str>, component: &str, level: &'a str) -> &'a str {
    match (cap_for(model, component), rank(level)) {
        (Some(cap), Some(r)) if rank(cap).is_some_and(|c| r > c) => cap,
        _ => level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_roles_stop_at_medium_by_default() {
        assert_eq!(cap_for(Some("haiku"), "cheap-checker"), Some("medium"));
        assert_eq!(
            cap_for(Some("claude-haiku-5-5"), "collector"),
            Some("medium")
        );
    }

    #[test]
    fn max_is_never_allowed_on_haiku() {
        assert_eq!(clamp(Some("haiku"), "cheap-checker", "max"), "medium");
    }

    #[test]
    fn an_opted_in_role_may_use_xhigh() {
        let table = [("drafter", "xhigh", "a reason that is long enough to count")];
        assert_eq!(cap_in(&table, Some("haiku"), "drafter"), Some("xhigh"));
        assert_eq!(cap_in(&table, Some("haiku"), "critic"), Some("medium"));
    }

    #[test]
    fn other_models_have_no_cap() {
        assert_eq!(cap_for(Some("sonnet"), "implementer"), None);
        assert_eq!(cap_for(Some("opus"), "reviewer"), None);
        assert_eq!(cap_for(None, "reviewer"), None);
    }

    #[test]
    fn every_opt_in_has_a_reason_and_a_valid_level() {
        for (name, level, reason) in OPT_INS {
            assert!(rank(level).is_some(), "{name}: {level}");
            assert!(reason.len() > 20, "{name} needs a real reason");
        }
    }
}
