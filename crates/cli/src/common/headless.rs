// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Whether playbook runs with no person at the keyboard (CI, `claude -p`).
//! One switch for every hook: `PLAYBOOK_HEADLESS` wins when set, and
//! `CI=true` is its alias when `PLAYBOOK_HEADLESS` is unset.

const TRUE_WORDS: [&str; 4] = ["1", "true", "yes", "on"];
const FALSE_WORDS: [&str; 4] = ["0", "false", "no", "off"];

fn is_word(value: &str, words: &[&str]) -> bool {
    let v = value.trim().to_lowercase();
    words.contains(&v.as_str())
}

/// Pure form of `is_headless`, so the truth table is testable without
/// touching the process environment.
pub(crate) fn headless_from(playbook_headless: Option<&str>, ci: Option<&str>) -> bool {
    match playbook_headless {
        Some(v) if is_word(v, &TRUE_WORDS) => true,
        Some(v) if is_word(v, &FALSE_WORDS) => false,
        _ => ci.is_some_and(|v| is_word(v, &["1", "true"])),
    }
}

pub fn is_headless() -> bool {
    headless_from(
        std::env::var("PLAYBOOK_HEADLESS").ok().as_deref(),
        std::env::var("CI").ok().as_deref(),
    )
}

/// Headless runs inject no memory unless `PLAYBOOK_HEADLESS_MEMORY` is truthy.
pub fn headless_memory_enabled() -> bool {
    std::env::var("PLAYBOOK_HEADLESS_MEMORY")
        .ok()
        .is_some_and(|v| is_word(&v, &TRUE_WORDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truth_table() {
        let cases: [(Option<&str>, Option<&str>, bool); 16] = [
            (None, None, false),
            (None, Some("true"), true),
            (None, Some("1"), true),
            (None, Some("TRUE"), true),
            (None, Some("false"), false),
            (None, Some("yes"), false),
            (Some("1"), None, true),
            (Some("true"), None, true),
            (Some("YES"), None, true),
            (Some("On"), None, true),
            (Some("0"), Some("true"), false),
            (Some("false"), Some("true"), false),
            (Some("no"), Some("1"), false),
            (Some("off"), Some("true"), false),
            (Some(""), Some("true"), true),
            (Some("maybe"), Some("true"), true),
        ];
        for (switch, ci, want) in cases {
            assert_eq!(
                headless_from(switch, ci),
                want,
                "switch={switch:?} ci={ci:?}"
            );
        }
    }
}
