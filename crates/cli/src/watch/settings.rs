// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! How watch settings combine across the config tiers.
//!
//! Rules follow the usual rule: the first tier that sets one decides, whole.
//! The autonomy ceiling is the opposite: the lowest value set in any tier
//! wins, so a repo cannot raise what the global tier lowered.

use super::rule::{Autonomy, Rule, RuleError, Stored};

/// The ceiling in force, given the `watch.autonomy` value of each tier that
/// sets one. With none set it is `propose`.
pub fn ceiling(tiers: &[Autonomy]) -> Autonomy {
    tiers.iter().copied().min().unwrap_or(Autonomy::Propose)
}

/// The level a rule runs at: the lower of its own level and the ceiling.
pub fn effective(rule: Autonomy, ceiling: Autonomy) -> Autonomy {
    rule.min(ceiling)
}

/// The rule in force from the stored texts of its tiers, nearest tier first
/// (repo, org, global). The first tier that holds a value decides: a rule is
/// returned whole, and a tombstone means no rule. A value that fails to parse
/// is an error for the caller to audit, and the other rules keep running.
pub fn resolve_rule(tiers: &[Option<&str>]) -> Result<Option<Rule>, RuleError> {
    let Some(text) = tiers.iter().flatten().next() else {
        return Ok(None);
    };
    Ok(match Stored::parse(text)? {
        Stored::Tombstone => None,
        Stored::Rule(rule) => Some(*rule),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOBAL: &str = r#"{"on":"issue.opened","do":"label","with":{"labels":["global"]}}"#;
    const REPO: &str = r#"{"on":"pr.opened","do":"label","with":{"labels":["repo"]}}"#;

    #[test]
    fn the_ceiling_is_the_lowest_set_value() {
        assert_eq!(
            ceiling(&[Autonomy::Propose, Autonomy::Auto]),
            Autonomy::Propose
        );
        assert_eq!(
            ceiling(&[Autonomy::Auto, Autonomy::Propose]),
            Autonomy::Propose
        );
        assert_eq!(ceiling(&[Autonomy::Off, Autonomy::Auto]), Autonomy::Off);
    }

    #[test]
    fn the_ceiling_defaults_to_propose() {
        assert_eq!(ceiling(&[]), Autonomy::Propose);
    }

    #[test]
    fn a_repo_may_opt_itself_up_when_no_other_tier_sets_one() {
        assert_eq!(ceiling(&[Autonomy::Auto]), Autonomy::Auto);
    }

    #[test]
    fn a_rule_never_runs_above_the_ceiling() {
        assert_eq!(
            effective(Autonomy::Auto, Autonomy::Propose),
            Autonomy::Propose
        );
        assert_eq!(effective(Autonomy::Off, Autonomy::Auto), Autonomy::Off);
    }

    #[test]
    fn the_nearest_tier_wins_whole() {
        let rule = resolve_rule(&[Some(REPO), None, Some(GLOBAL)])
            .unwrap()
            .unwrap();
        assert_eq!(rule.with.labels, vec!["repo".to_string()]);
    }

    #[test]
    fn a_tombstone_at_the_repo_switches_the_global_rule_off() {
        let off = r#"{"autonomy":"off"}"#;
        assert_eq!(
            resolve_rule(&[Some(off), None, Some(GLOBAL)]).unwrap(),
            None
        );
        // The same global rule is present where no tombstone sits.
        assert!(resolve_rule(&[None, None, Some(GLOBAL)]).unwrap().is_some());
    }

    #[test]
    fn no_tier_means_no_rule() {
        assert_eq!(resolve_rule(&[None, None, None]).unwrap(), None);
    }

    #[test]
    fn an_invalid_value_is_an_error() {
        assert!(resolve_rule(&[Some("{")]).is_err());
    }
}
