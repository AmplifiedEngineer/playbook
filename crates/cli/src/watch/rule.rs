// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A watch rule: one JSON object that says which event to match, how to
//! filter it by metadata, and which built in action to take. There is no field
//! for a shell command or an API path, and unknown fields are an error.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

/// How much freedom a rule has. The order is the order of freedom, so the
/// lowest of two levels is `min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Autonomy {
    Off,
    Propose,
    Auto,
}

/// The events a rule can match. The probe kinds (`pr.updated`, `pr.reviewed`,
/// `pr.checks_green`) join with the first rule that needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum On {
    #[serde(rename = "issue.opened")]
    IssueOpened,
    #[serde(rename = "issue.commented")]
    IssueCommented,
    #[serde(rename = "pr.opened")]
    PrOpened,
    #[serde(rename = "pr.commented")]
    PrCommented,
}

/// The built in actions. More join one per rule delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Do {
    Label,
}

/// Who decides: only fixed rules for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decide {
    #[default]
    Rules,
}

/// Filters on event metadata. Text is never matched, only an exact comment
/// prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Where {
    #[serde(default)]
    pub labels_any: Vec<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    pub base: Option<String>,
    pub comment_prefix: Option<String>,
}

/// The arguments of the action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct With {
    #[serde(default)]
    pub labels: Vec<String>,
}

/// A complete rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub on: On,
    #[serde(default, rename = "where")]
    pub filter: Where,
    #[serde(rename = "do")]
    pub action: Do,
    #[serde(default)]
    pub with: With,
    #[serde(default)]
    pub decide: Decide,
    #[serde(default = "propose")]
    pub autonomy: Autonomy,
}

fn propose() -> Autonomy {
    Autonomy::Propose
}

/// What a stored rule value means at one tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    /// `{"autonomy":"off"}` alone: switches an inherited rule off at this tier.
    Tombstone,
    Rule(Box<Rule>),
}

/// Why a rule value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError(String);

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuleError {}

impl Stored {
    /// Parses the JSON text of one rule. `auto` is refused until the runner
    /// that enforces limits exists.
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| RuleError(format!("the rule is not valid JSON: {e}")))?;
        if is_tombstone(&value) {
            return Ok(Self::Tombstone);
        }
        let rule: Rule =
            serde_json::from_value(value).map_err(|e| RuleError(format!("invalid rule: {e}")))?;
        if rule.autonomy == Autonomy::Auto {
            return Err(RuleError("auto is not available yet".to_string()));
        }
        if rule.action == Do::Label && rule.with.labels.is_empty() {
            return Err(RuleError(
                "the label action needs at least one entry in with.labels".to_string(),
            ));
        }
        Ok(Self::Rule(Box::new(rule)))
    }
}

fn is_tombstone(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.get("autonomy").and_then(Value::as_str) == Some("off"))
}

/// Whether `id` can name a rule: lowercase letters, digits and hyphens, not
/// empty and not starting with a hyphen.
pub fn is_rule_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = r#"{"on":"issue.opened","do":"label","with":{"labels":["triage"]}}"#;

    #[test]
    fn a_valid_rule_parses_with_the_defaults() {
        // Arrange / Act
        let stored = Stored::parse(LABEL).unwrap();
        // Assert
        let Stored::Rule(rule) = stored else {
            panic!("expected a rule");
        };
        assert_eq!(rule.on, On::IssueOpened);
        assert_eq!(rule.autonomy, Autonomy::Propose);
        assert_eq!(rule.decide, Decide::Rules);
        assert_eq!(rule.with.labels, vec!["triage".to_string()]);
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let text = r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"run":"rm -rf"}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn an_unknown_field_inside_where_is_refused() {
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"where":{"title":"x"}}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn a_bad_event_or_action_is_refused() {
        assert!(Stored::parse(r#"{"on":"push","do":"label"}"#).is_err());
        assert!(Stored::parse(r#"{"on":"issue.opened","do":"shell"}"#).is_err());
    }

    #[test]
    fn an_autonomy_outside_the_three_values_is_refused() {
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"autonomy":"yolo"}"#;
        assert!(Stored::parse(text).is_err());
    }

    #[test]
    fn auto_is_refused_for_now() {
        let text =
            r#"{"on":"issue.opened","do":"label","with":{"labels":["a"]},"autonomy":"auto"}"#;
        let err = Stored::parse(text).unwrap_err();
        assert_eq!(err.to_string(), "auto is not available yet");
    }

    #[test]
    fn the_short_off_form_is_a_tombstone() {
        assert_eq!(
            Stored::parse(r#"{"autonomy":"off"}"#).unwrap(),
            Stored::Tombstone
        );
    }

    #[test]
    fn the_short_form_must_be_alone() {
        assert!(Stored::parse(r#"{"autonomy":"off","do":"label"}"#).is_err());
    }

    #[test]
    fn a_label_rule_without_labels_is_refused() {
        assert!(Stored::parse(r#"{"on":"issue.opened","do":"label"}"#).is_err());
    }

    #[test]
    fn text_that_is_not_json_is_refused() {
        assert!(Stored::parse("not json").is_err());
    }

    #[test]
    fn rule_ids_are_lowercase_words() {
        assert!(is_rule_id("triage-issue"));
        assert!(is_rule_id("a1"));
        assert!(!is_rule_id("Triage"));
        assert!(!is_rule_id(""));
        assert!(!is_rule_id("-x"));
        assert!(!is_rule_id("a.b"));
    }
}
