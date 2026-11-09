// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one `--json` shape of the subcommands a plugin command runs inline.
//!
//! Claude Code aborts a whole inline invocation when a command exits non
//! zero, and the model never runs. So `--json` always exits 0 and carries the
//! outcome here: `ok`, `code` (the exit code plain mode would use), `meaning`
//! (what that code means) and `error` (text, empty when `ok`). The report
//! fields of the subcommand sit beside them. Plain mode keeps its exit codes.

use serde_json::{json, Map, Value};

/// What a plain-mode exit code means for the commit and PR subcommands.
pub fn meaning(code: i32) -> &'static str {
    match code {
        0 => "ok",
        1 => "failed or refused",
        2 => "usage error",
        3 => "refused by a rule",
        4 => "rebase conflict",
        5 => "a git or gh call failed",
        6 => "the PR closed or a check failed",
        7 => "timed out while waiting",
        8 => "auto-merge did not queue",
        9 => "nothing to do",
        _ => "unknown",
    }
}

/// Wraps `fields` (an object, or anything else as `report`) in the envelope.
/// `error` is the text for a failure, empty for success.
pub fn wrap(code: i32, error: &str, fields: Value) -> Value {
    let mut out = Map::new();
    out.insert("ok".into(), json!(code == 0));
    out.insert("code".into(), json!(code));
    out.insert("meaning".into(), json!(meaning(code)));
    out.insert("error".into(), json!(error));
    match fields {
        Value::Object(map) => {
            for (k, v) in map {
                out.entry(k).or_insert(v);
            }
        }
        Value::Null => {}
        other => {
            out.insert("report".into(), other);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_success_is_ok_with_code_zero_and_its_fields() {
        let v = wrap(0, "", json!({"verdict": "READY"}));
        assert_eq!(v["ok"], true);
        assert_eq!(v["code"], 0);
        assert_eq!(v["meaning"], "ok");
        assert_eq!(v["error"], "");
        assert_eq!(v["verdict"], "READY");
    }

    #[test]
    fn a_failure_keeps_the_plain_mode_code_and_the_error_text() {
        let v = wrap(3, "refused: x", json!({"verdict": "REFUSED"}));
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], 3);
        assert_eq!(v["meaning"], "refused by a rule");
        assert_eq!(v["error"], "refused: x");
    }

    #[test]
    fn a_report_field_never_overrides_an_envelope_field() {
        let v = wrap(0, "", json!({"ok": false, "code": 9}));
        assert_eq!(v["ok"], true);
        assert_eq!(v["code"], 0);
    }

    #[test]
    fn a_non_object_report_sits_under_report() {
        assert_eq!(wrap(0, "", json!([1]))["report"], json!([1]));
    }
}
