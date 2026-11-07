// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Turning the text of a config value into the JSON type its key expects.

use super::{keys, ConfigError};

/// Parse `config set`'s raw `value` string into the JSON type `key` expects,
/// using `keys::default_value` to learn that type generically so an
/// unknown key or a wrong-type value is rejected before `write::set` ever
/// runs and touches a file. A boolean key accepts `true`/`false`
/// case-insensitively; a numeric key accepts any finite number, left to `write::set` to range check; any
/// other known key is passed through as a string, leaving an out-of-enum
/// value for `write::set`'s own check to reject.
pub fn parse_config_value(key: &str, value: &str) -> Result<serde_json::Value, ConfigError> {
    let default =
        keys::default_value(key).ok_or_else(|| ConfigError::UnknownKey(key.to_string()))?;
    match default {
        serde_json::Value::Bool(_) => match value.to_ascii_lowercase().as_str() {
            "true" => Ok(serde_json::Value::Bool(true)),
            "false" => Ok(serde_json::Value::Bool(false)),
            _ => Err(ConfigError::WrongType {
                key: key.to_string(),
                expected: "boolean",
            }),
        },
        serde_json::Value::Number(_) => {
            parse_number(value).ok_or_else(|| ConfigError::InvalidNumber {
                key: key.to_string(),
                value: value.to_string(),
            })
        }
        serde_json::Value::Array(_) => Ok(parse_path_list(value)),
        _ => Ok(serde_json::Value::String(value.to_string())),
    }
}

/// A list value: a JSON array, or a colon-separated list like `$PATH`. Empty
/// clears the list. Each item is validated by `write::set`.
fn parse_path_list(value: &str) -> serde_json::Value {
    if let Ok(parsed @ serde_json::Value::Array(_)) = serde_json::from_str(value) {
        return parsed;
    }
    serde_json::Value::Array(
        value
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| serde_json::Value::String(s.to_string()))
            .collect(),
    )
}

/// A whole number stays an integer so the stored JSON has no `.0`; anything
/// else finite parses as a decimal. Range checks are left to `write::set`.
fn parse_number(value: &str) -> Option<serde_json::Value> {
    if let Ok(whole) = value.parse::<i64>() {
        return Some(whole.into());
    }
    if let Ok(whole) = value.parse::<u64>() {
        return Some(whole.into());
    }
    value
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
}
