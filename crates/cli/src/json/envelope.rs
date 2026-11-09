// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The shared `--json` envelope: `ok`, `code`, `meaning` and `error` around a
//! command's own report fields. With `--json` the process exits 0 and the
//! caller reads `ok` and `code`; plain mode keeps its exit codes.

use serde_json::{json, Map, Value};

/// The exit code of a `--json` run, whatever the outcome.
pub const JSON_EXIT: i32 = 0;

/// The code plain mode uses for success.
pub const CODE_OK: i32 = 0;

/// The envelope around `report`. A non object `report` adds no fields. The
/// envelope keys win over a report field of the same name.
pub fn build(code: i32, meaning: &str, error: &str, report: Value) -> Value {
    let mut fields = match report {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    fields.insert("ok".to_string(), json!(code == CODE_OK));
    fields.insert("code".to_string(), json!(code));
    fields.insert("meaning".to_string(), json!(meaning));
    fields.insert("error".to_string(), json!(error));
    Value::Object(fields)
}

/// Prints the envelope as one line and returns the exit code, which is 0.
pub fn emit(code: i32, meaning: &str, error: &str, report: Value) -> i32 {
    println!("{}", build(code, meaning, error, report));
    JSON_EXIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_carries_ok_true_and_the_report_fields() {
        let value = build(0, "done", "", json!({"pr": 7}));
        assert_eq!(value["ok"], true);
        assert_eq!(value["code"], 0);
        assert_eq!(value["pr"], 7);
        assert_eq!(value["error"], "");
    }

    #[test]
    fn failure_carries_ok_false_and_the_error() {
        let value = build(5, "gh failed", "no such pr", Value::Null);
        assert_eq!(value["ok"], false);
        assert_eq!(value["code"], 5);
        assert_eq!(value["meaning"], "gh failed");
        assert_eq!(value["error"], "no such pr");
    }

    #[test]
    fn envelope_keys_win_over_report_fields() {
        let value = build(0, "m", "", json!({"ok": false, "code": 9}));
        assert_eq!(value["ok"], true);
        assert_eq!(value["code"], 0);
    }

    #[test]
    fn emit_exits_zero_on_failure() {
        assert_eq!(emit(5, "m", "e", Value::Null), JSON_EXIT);
    }
}
