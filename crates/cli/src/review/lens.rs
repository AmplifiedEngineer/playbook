// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Lens helpers shared by `/playbook:review --deep` and `/playbook:implement`
//! Step 9. They hold what those commands used to spell out in prose: how a
//! review-triage tier map is completed so a missing or malformed answer
//! always fails open to `full-lens`.

use serde_json::{json, Map, Value};

const TIERS: [&str; 3] = ["skip", "cheap-check", "full-lens"];

/// A tier chosen by the caller regardless of triage, such as `tests=skip`
/// under `--no-tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forced {
    pub lens: String,
    pub tier: String,
    pub reason: String,
}

/// Parse `lens=tier:reason`.
pub fn parse_forced(spec: &str) -> Result<Forced, String> {
    let bad = || format!("bad --force '{spec}': use lens=tier:reason");
    let (lens, rest) = spec.split_once('=').ok_or_else(bad)?;
    let (tier, reason) = rest.split_once(':').ok_or_else(bad)?;
    if lens.is_empty() || !TIERS.contains(&tier) {
        return Err(bad());
    }
    Ok(Forced {
        lens: lens.to_string(),
        tier: tier.to_string(),
        reason: reason.trim().to_string(),
    })
}

/// The JSON object inside `raw`: the whole text, or what sits between the
/// first `{` and the last `}` (a reply wrapped in prose or a code fence).
fn tier_map(raw: &str) -> Option<Map<String, Value>> {
    let text = raw.trim();
    let parse = |t: &str| match serde_json::from_str::<Value>(t) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    };
    parse(text).or_else(|| {
        let (start, end) = (text.find('{')?, text.rfind('}')?);
        (start < end).then(|| parse(&text[start..=end])).flatten()
    })
}

/// One lens's tier and reason, after fail-open defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub lens: String,
    pub tier: String,
    pub reason: String,
}

/// Complete a triage reply into one entry per lens, in `lenses` order.
///
/// Three fail-open rules: a reply that is missing or not a JSON object
/// defaults every lens to `full-lens`; a lens absent from the map defaults to
/// `full-lens`; a lens whose `tier` is not `skip`, `cheap-check` or
/// `full-lens` defaults to `full-lens`. Lenses that triage did classify keep
/// their tier. A `Forced` entry overrides triage for that lens only.
pub fn merge(lenses: &[String], raw: &str, forced: &[Forced]) -> Vec<Resolved> {
    let map = tier_map(raw);
    lenses
        .iter()
        .map(|lens| {
            if let Some(f) = forced.iter().find(|f| &f.lens == lens) {
                return Resolved {
                    lens: lens.clone(),
                    tier: f.tier.clone(),
                    reason: f.reason.clone(),
                };
            }
            let entry = map.as_ref().map(|m| m.get(lens));
            let (tier, reason) = match entry {
                None => (None, "triage returned nothing usable: ran in full"),
                Some(None) => (None, "triage did not classify this lens: ran in full"),
                Some(Some(v)) => match v["tier"].as_str().filter(|t| TIERS.contains(t)) {
                    Some(t) => (Some(t), v["reason"].as_str().unwrap_or("")),
                    None => (None, "triage gave an unrecognised tier: ran in full"),
                },
            };
            Resolved {
                lens: lens.clone(),
                tier: tier.unwrap_or("full-lens").to_string(),
                reason: reason.to_string(),
            }
        })
        .collect()
}

/// The merged map as JSON, then the one-line summary the commands print.
pub fn render(resolved: &[Resolved]) -> String {
    let mut obj = Map::new();
    for r in resolved {
        obj.insert(r.lens.clone(), json!({"tier": r.tier, "reason": r.reason}));
    }
    let summary = resolved
        .iter()
        .map(|r| format!("{}={}", r.lens, r.tier))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}\nTriage: {summary}", Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lenses(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_clean_reply_is_kept_as_is() {
        let raw =
            r#"{"a":{"tier":"skip","reason":"docs only"},"b":{"tier":"cheap-check","reason":"r"}}"#;
        let out = merge(&lenses(&["a", "b"]), raw, &[]);
        assert_eq!(out[0].tier, "skip");
        assert_eq!(out[0].reason, "docs only");
        assert_eq!(out[1].tier, "cheap-check");
    }

    #[test]
    fn no_reply_runs_every_lens_in_full() {
        for raw in ["", "   ", "the classifier timed out", "[1,2]", "{not json"] {
            let out = merge(&lenses(&["a", "b"]), raw, &[]);
            assert!(out.iter().all(|r| r.tier == "full-lens"), "{raw:?}");
        }
    }

    #[test]
    fn a_missing_lens_defaults_while_the_others_keep_their_tier() {
        let raw = r#"{"a":{"tier":"skip","reason":"x"}}"#;
        let out = merge(&lenses(&["a", "b"]), raw, &[]);
        assert_eq!(out[0].tier, "skip");
        assert_eq!(out[1].tier, "full-lens");
        assert!(out[1].reason.contains("did not classify"));
    }

    #[test]
    fn an_unrecognised_tier_defaults_to_full_lens() {
        for bad in [
            r#"{"a":{"tier":"maybe","reason":"x"}}"#,
            r#"{"a":{"tier":"SKIP","reason":"x"}}"#,
            r#"{"a":{"tier":null}}"#,
            r#"{"a":{"reason":"no tier"}}"#,
            r#"{"a":"skip"}"#,
        ] {
            let out = merge(&lenses(&["a"]), bad, &[]);
            assert_eq!(out[0].tier, "full-lens", "{bad}");
        }
    }

    #[test]
    fn a_reply_wrapped_in_prose_or_a_fence_still_parses() {
        let raw = "Here you go:\n```json\n{\"a\":{\"tier\":\"skip\",\"reason\":\"x\"}}\n```\n";
        assert_eq!(merge(&lenses(&["a"]), raw, &[])[0].tier, "skip");
    }

    #[test]
    fn extra_lenses_in_the_reply_are_ignored() {
        let raw = r#"{"a":{"tier":"skip","reason":"x"},"zzz":{"tier":"skip","reason":"y"}}"#;
        assert_eq!(merge(&lenses(&["a"]), raw, &[]).len(), 1);
    }

    #[test]
    fn a_forced_tier_overrides_triage_for_that_lens_only() {
        let raw =
            r#"{"tests":{"tier":"full-lens","reason":"x"},"scope":{"tier":"skip","reason":"y"}}"#;
        let forced = [parse_forced("tests=skip:no new tests by explicit user choice").unwrap()];
        let out = merge(&lenses(&["tests", "scope"]), raw, &forced);
        assert_eq!(out[0].tier, "skip");
        assert_eq!(out[0].reason, "no new tests by explicit user choice");
        assert_eq!(out[1].tier, "skip");
    }

    #[test]
    fn a_bad_force_spec_is_rejected() {
        for bad in ["tests", "tests=skip", "tests=maybe:x", "=skip:x"] {
            assert!(parse_forced(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn render_prints_json_then_the_summary_line() {
        let out = render(&merge(
            &lenses(&["a", "b"]),
            r#"{"a":{"tier":"skip","reason":"r"}}"#,
            &[],
        ));
        let (json_line, summary) = out.split_once('\n').unwrap();
        let v: Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(v["a"]["tier"], "skip");
        assert_eq!(v["b"]["tier"], "full-lens");
        assert_eq!(summary, "Triage: a=skip, b=full-lens");
    }
}
