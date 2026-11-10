// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reads the flat JSON array `gh pr checks --json` produces: one object per
//! check, each carrying a `bucket` field (`"pass"`, `"fail"`, `"pending"`,
//! and so on). Used to shell out to `jq` for a per-bucket tally and for the
//! array's own length, so a missing `jq` produced the same empty result as
//! every check having passed, silently misreporting a red run as clean.

use serde_json::Value;

/// Tallies each element's `"bucket"` string field in a top-level JSON array
/// against `buckets`, preserving the order `buckets` names them in. A bucket
/// with no matching elements comes back 0, matching `jq`'s own `?`
/// operators: malformed JSON, a non-array top-level value, an array element
/// that is not an object, or one missing the `bucket` field are all
/// swallowed rather than erroring, the same tolerance
/// `src/doctor/field.rs`'s field readers document.
pub fn bucket_counts(json: &str, buckets: &[&str]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = buckets.iter().map(|&b| (b.to_string(), 0)).collect();

    let Ok(Value::Array(elements)) = serde_json::from_str::<Value>(json) else {
        return counts;
    };

    for element in &elements {
        let Some(bucket) = element.get("bucket").and_then(Value::as_str) else {
            continue;
        };
        if let Some(entry) = counts.iter_mut().find(|(name, _)| name == bucket) {
            entry.1 += 1;
        }
    }

    counts
}

/// Length of a top-level JSON array, or 0 for anything else: malformed JSON,
/// an empty array, or a non-array top-level value, matching `jq 'length'`'s
/// permissive behavior.
pub fn array_length(json: &str) -> usize {
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Array(elements)) => elements.len(),
        _ => 0,
    }
}

/// Reads one field from a top-level JSON object as display text: a string
/// value as-is, a number in its literal form (`42`, not `"42"`), matching
/// the two real call sites in `skills/grounding-review/references/quick-pass.md` (`.number`, a
/// number, and `.headRefOid`, a string). Empty on any failure: malformed
/// JSON, a non-object top-level value, a missing key, or a value of any
/// other type, the same empty-on-mismatch convention [`bucket_counts`] and
/// `src/doctor/field.rs` already use.
pub fn field(json: &str, key: &str) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(json) else {
        return String::new();
    };
    match map.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_counts_tallies_each_named_bucket_from_a_mixed_fixture() {
        // Arrange
        let json = r#"[
            {"name": "build", "bucket": "pass"},
            {"name": "lint", "bucket": "pass"},
            {"name": "test", "bucket": "fail"},
            {"name": "deploy", "bucket": "pending"}
        ]"#;
        let buckets = ["pass", "fail", "pending"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(
            got,
            vec![
                ("pass".to_string(), 2),
                ("fail".to_string(), 1),
                ("pending".to_string(), 1),
            ]
        );
    }

    #[test]
    fn bucket_counts_is_all_zero_for_an_empty_array() {
        // Arrange
        let json = "[]";
        let buckets = ["pass", "fail"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(got, vec![("pass".to_string(), 0), ("fail".to_string(), 0)]);
    }

    #[test]
    fn bucket_counts_excludes_an_object_missing_the_bucket_key_without_panicking() {
        // Arrange
        let json = r#"[
            {"name": "build", "bucket": "pass"},
            {"name": "lint"}
        ]"#;
        let buckets = ["pass"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(got, vec![("pass".to_string(), 1)]);
    }

    #[test]
    fn bucket_counts_excludes_a_non_object_array_element_without_panicking() {
        // Arrange
        let json = r#"[
            {"name": "build", "bucket": "pass"},
            "not an object"
        ]"#;
        let buckets = ["pass"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(got, vec![("pass".to_string(), 1)]);
    }

    #[test]
    fn bucket_counts_is_all_zero_for_malformed_json() {
        // Arrange
        let json = "not json";
        let buckets = ["pass", "fail"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(got, vec![("pass".to_string(), 0), ("fail".to_string(), 0)]);
    }

    #[test]
    fn bucket_counts_is_all_zero_for_a_non_array_top_level_value() {
        // Arrange
        let json = r#"{"bucket": "pass"}"#;
        let buckets = ["pass", "fail"];

        // Act
        let got = bucket_counts(json, &buckets);

        // Assert
        assert_eq!(got, vec![("pass".to_string(), 0), ("fail".to_string(), 0)]);
    }

    #[test]
    fn array_length_matches_the_element_count_directly() {
        // Arrange
        let json = r#"[
            {"name": "build", "bucket": "pass"},
            {"name": "lint", "bucket": "pass"},
            {"name": "test", "bucket": "fail"}
        ]"#;

        // Act
        let got = array_length(json);

        // Assert
        assert_eq!(got, 3);
    }

    #[test]
    fn array_length_is_zero_for_an_empty_array() {
        // Arrange
        let json = "[]";

        // Act
        let got = array_length(json);

        // Assert
        assert_eq!(got, 0);
    }

    #[test]
    fn array_length_is_zero_for_malformed_json() {
        // Arrange
        let json = "not json";

        // Act
        let got = array_length(json);

        // Assert
        assert_eq!(got, 0);
    }

    #[test]
    fn field_reads_a_string_value() {
        // Arrange
        let json = r#"{"headRefOid": "abc123"}"#;

        // Act
        let got = field(json, "headRefOid");

        // Assert
        assert_eq!(got, "abc123");
    }

    #[test]
    fn field_reads_a_number_value_as_its_literal_text() {
        // Arrange
        let json = r#"{"number": 42}"#;

        // Act
        let got = field(json, "number");

        // Assert
        assert_eq!(got, "42");
    }

    #[test]
    fn field_is_empty_when_the_key_is_missing() {
        // Arrange
        let json = r#"{"other": "value"}"#;

        // Act
        let got = field(json, "number");

        // Assert
        assert_eq!(got, "");
    }

    #[test]
    fn field_is_empty_for_a_non_object_top_level_value() {
        // Arrange
        let json = "[1, 2, 3]";

        // Act
        let got = field(json, "number");

        // Assert
        assert_eq!(got, "");
    }

    #[test]
    fn field_is_empty_for_malformed_json() {
        // Arrange
        let json = "not json";

        // Act
        let got = field(json, "number");

        // Assert
        assert_eq!(got, "");
    }
}
