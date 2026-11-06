// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr stack map`: give each review finding to the PR whose diff
//! holds the changed line, so it can go to that PR's pending review.
//!
//! Usage: `map --stack <stack.json> --findings <findings.json> [--out <file>]`.
//! `stack.json` is written by `pr stack <pr> --context`. A finding is
//! `{file, line, body}` plus an optional `pr` that names the PR the reviewer
//! was reading. With `pr`, that PR must hold the line. Without it, the topmost
//! open PR whose diff covers the line wins, since that is the line number a
//! reader of the finished stack sees.

use super::shared::gh;
use crate::common::par;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// The new-side line ranges one diff covers, per file.
pub(super) type Ranges = BTreeMap<String, Vec<(u64, u64)>>;

/// Which side of a hunk header to read.
#[derive(Clone, Copy)]
pub(super) enum Side {
    /// The `+start,len` part: lines as the PR leaves them.
    New,
    /// The `-start,len` part: lines as the PR's base has them.
    Old,
}

/// Read the hunk headers of a unified diff. A range spans the whole hunk, so
/// a comment may sit on an added or an unchanged line inside it. On the old
/// side the file name comes from the `---` line, so a deleted file is kept.
pub(super) fn hunks(diff: &str, side: Side) -> Ranges {
    let (file_mark, range_mark) = match side {
        Side::New => ("+++ ", '+'),
        Side::Old => ("--- ", '-'),
    };
    let mut out: Ranges = BTreeMap::new();
    let mut file: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix(file_mark) {
            let prefix = if matches!(side, Side::New) {
                "b/"
            } else {
                "a/"
            };
            file = rest.strip_prefix(prefix).map(str::to_string);
        } else if line.starts_with("@@") {
            let Some(f) = &file else { continue };
            let Some(range) = line.split_whitespace().find(|t| t.starts_with(range_mark)) else {
                continue;
            };
            let spec = range.trim_start_matches(range_mark);
            let (start, len) = match spec.split_once(',') {
                Some((s, l)) => (s.parse::<u64>(), l.parse::<u64>()),
                None => (spec.parse::<u64>(), Ok(1)),
            };
            if let (Ok(start), Ok(len)) = (start, len) {
                if len > 0 {
                    out.entry(f.clone())
                        .or_default()
                        .push((start, start + len - 1));
                }
            }
        }
    }
    out
}

pub(super) fn holds(ranges: &Ranges, file: &str, line: u64) -> bool {
    ranges
        .get(file)
        .is_some_and(|rs| rs.iter().any(|(a, b)| (*a..=*b).contains(&line)))
}

/// What GitHub says about a PR right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub head_sha: String,
    pub base: String,
    pub open: bool,
}

fn unmapped(finding: &Value, reason: &str) -> Value {
    json!({ "finding": finding, "reason": reason })
}

/// The pure mapping step. `stack` is the `pr stack --json` object, `diffs`
/// holds each open PR's diff, `live` the current head, base and state.
fn map_findings(
    stack: &Value,
    diffs: &BTreeMap<u64, String>,
    findings: &[Value],
    live: &BTreeMap<u64, Live>,
) -> Value {
    let prs = stack["prs"].as_array().cloned().unwrap_or_default();
    let mut stale: BTreeMap<u64, String> = BTreeMap::new();
    let mut open: Vec<(u64, String, Ranges)> = Vec::new();
    let mut state_of: BTreeMap<u64, String> = BTreeMap::new();
    let mut old_of: Vec<(u64, Ranges)> = Vec::new();
    for p in &prs {
        let (Some(n), Some(state)) = (p["number"].as_u64(), p["state"].as_str()) else {
            continue;
        };
        state_of.insert(n, state.to_string());
        if state != "open" {
            continue;
        }
        if let Some(l) = live.get(&n) {
            let sha = p["head_sha"].as_str().unwrap_or("");
            if !l.open {
                stale.insert(n, "the PR is no longer open".into());
            } else if l.head_sha != sha {
                stale.insert(
                    n,
                    "the branch was pushed to since the review started".into(),
                );
            } else if l.base != p["base"].as_str().unwrap_or("") {
                stale.insert(n, "the PR was retargeted since the review started".into());
            }
        }
        old_of.push((
            n,
            hunks(diffs.get(&n).map_or("", String::as_str), Side::Old),
        ));
        open.push((
            n,
            p["head_sha"].as_str().unwrap_or("").to_string(),
            hunks(diffs.get(&n).map_or("", String::as_str), Side::New),
        ));
    }

    let mut by_pr: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    let mut loose = Vec::new();
    let mut fixed_above = Vec::new();
    for f in findings {
        let file = f["file"]
            .as_str()
            .or_else(|| f["path"].as_str())
            .unwrap_or("");
        let body = f["body"].as_str().unwrap_or("");
        let Some(line) = f["line"].as_u64() else {
            loose.push(unmapped(f, "the finding has no line number"));
            continue;
        };
        if file.is_empty() {
            loose.push(unmapped(f, "the finding has no file"));
            continue;
        }
        let target = match f["pr"].as_u64() {
            Some(n) => match state_of.get(&n).map(String::as_str) {
                Some("open") if open.iter().any(|(m, _, r)| *m == n && holds(r, file, line)) => {
                    Some(n)
                }
                Some("open") => {
                    loose.push(unmapped(
                        f,
                        &format!("line {line} is not in the diff of #{n}"),
                    ));
                    continue;
                }
                Some("merged") => {
                    loose.push(unmapped(
                        f,
                        &format!("#{n} is merged and is never commented on"),
                    ));
                    continue;
                }
                Some(_) => {
                    loose.push(unmapped(
                        f,
                        &format!("#{n} is closed and is never commented on"),
                    ));
                    continue;
                }
                None => {
                    loose.push(unmapped(f, &format!("#{n} is not in this stack")));
                    continue;
                }
            },
            None => open
                .iter()
                .rev()
                .find(|(_, _, r)| holds(r, file, line))
                .map(|(n, _, _)| *n),
        };
        let Some(n) = target else {
            loose.push(unmapped(f, "no open PR in the stack changes this line"));
            continue;
        };
        if let Some(why) = stale.get(&n) {
            loose.push(unmapped(f, &format!("held for #{n}: {why}")));
            continue;
        }
        if let Some(above) = changed_above(&old_of, n, file, line) {
            fixed_above.push(json!({ "finding": f, "pr": n, "fixed_by": above }));
            continue;
        }
        by_pr.entry(n).or_default().push(json!({
            "path": file, "line": line, "side": "RIGHT", "body": body,
        }));
    }

    let by_pr_json: serde_json::Map<String, Value> = by_pr
        .into_iter()
        .map(|(n, comments)| {
            let sha = open
                .iter()
                .find(|(m, _, _)| *m == n)
                .map_or("", |(_, s, _)| s.as_str());
            (
                n.to_string(),
                json!({ "head_sha": sha, "comments": comments }),
            )
        })
        .collect();
    let stale_json: Vec<Value> = stale
        .iter()
        .map(|(n, why)| json!({ "pr": n, "reason": why }))
        .collect();
    json!({
        "by_pr": by_pr_json,
        "unmapped": loose,
        "stale": stale_json,
        "fixed_above": fixed_above,
    })
}

/// The first open PR above `pr` whose diff changes or removes `file:line` of
/// the code `pr` leaves. A PR's old side is its parent's new side, so the
/// numbers match for the PR right above and drift only by lines the PRs in
/// between added or removed.
fn changed_above(old_of: &[(u64, Ranges)], pr: u64, file: &str, line: u64) -> Option<u64> {
    let at = old_of.iter().position(|(n, _)| *n == pr)?;
    old_of[at + 1..]
        .iter()
        .find(|(_, old)| holds(old, file, line))
        .map(|(n, _)| *n)
}

fn read_json(path: &str) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

/// Parse the flags, fetch the diffs and the live state, and map.
pub fn run(args: &str) -> Result<String, String> {
    let (mut stack_path, mut findings_path, mut out_path) = (None, None, None);
    let mut toks = args.split_whitespace();
    while let Some(t) = toks.next() {
        match t {
            "--stack" => stack_path = toks.next(),
            "--findings" => findings_path = toks.next(),
            "--out" => out_path = toks.next(),
            other => return Err(format!("error: unknown argument '{other}'")),
        }
    }
    let stack_path = stack_path.ok_or("error: pass --stack <stack.json>")?;
    let findings_path = findings_path.ok_or("error: pass --findings <findings.json>")?;
    let stack = read_json(stack_path)?;
    let raw = read_json(findings_path)?;
    let findings: Vec<Value> = match raw.get("findings").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => raw
            .as_array()
            .cloned()
            .ok_or("error: findings must be a JSON array")?,
    };

    let dir = Path::new(stack_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let open: Vec<u64> = stack["prs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| p["state"] == "open")
                .filter_map(|p| p["number"].as_u64())
                .collect()
        })
        .unwrap_or_default();
    let fetched = par::map(&open, par::MAX_CONCURRENT, |n| {
        let diff = match fs::read_to_string(dir.join(format!("pr-{n}.diff"))) {
            Ok(d) => d,
            Err(_) => gh(&["pr", "diff", &n.to_string()])?,
        };
        let live = gh(&[
            "pr",
            "view",
            &n.to_string(),
            "--json",
            "headRefOid,baseRefName,state",
        ])?;
        let v: Value = serde_json::from_str(&live).map_err(|e| e.to_string())?;
        Ok::<_, String>((
            *n,
            diff,
            Live {
                head_sha: v["headRefOid"].as_str().unwrap_or("").to_string(),
                base: v["baseRefName"].as_str().unwrap_or("").to_string(),
                open: v["state"] == "OPEN",
            },
        ))
    });
    let mut diffs = BTreeMap::new();
    let mut live = BTreeMap::new();
    for r in fetched {
        let (n, d, l) = r.map_err(|e| format!("error: could not read a PR of the stack: {e}"))?;
        diffs.insert(n, d);
        live.insert(n, l);
    }
    let mapped = map_findings(&stack, &diffs, &findings, &live);
    let text = serde_json::to_string_pretty(&mapped).map_err(|e| e.to_string())?;
    match out_path {
        Some(p) => {
            fs::write(p, &text).map_err(|e| format!("could not write {p}: {e}"))?;
            Ok(format!("mapped_file={p}"))
        }
        None => Ok(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF_A: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,4 @@\n x\n+y\n+z\n w\n@@ -20 +22,3 @@\n+q\n";
    const DIFF_B: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,1 +3,2 @@\n+n\n--- a/src/b.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-gone\n";

    fn stack() -> Value {
        json!({"prs":[
            {"number":1,"state":"merged","head_sha":"s1","base":"main"},
            {"number":2,"state":"open","head_sha":"s2","base":"b1"},
            {"number":3,"state":"open","head_sha":"s3","base":"b2"},
            {"number":4,"state":"closed","head_sha":"s4","base":"b3"}]})
    }

    fn diffs() -> BTreeMap<u64, String> {
        BTreeMap::from([(2, DIFF_A.to_string()), (3, DIFF_B.to_string())])
    }

    fn live_ok() -> BTreeMap<u64, Live> {
        BTreeMap::from([
            (
                2,
                Live {
                    head_sha: "s2".into(),
                    base: "b1".into(),
                    open: true,
                },
            ),
            (
                3,
                Live {
                    head_sha: "s3".into(),
                    base: "b2".into(),
                    open: true,
                },
            ),
        ])
    }

    fn finding(file: &str, line: u64, pr: Option<u64>) -> Value {
        let mut f = json!({"file": file, "line": line, "body": "msg"});
        if let Some(n) = pr {
            f["pr"] = json!(n);
        }
        f
    }

    #[test]
    fn hunk_ranges_cover_the_new_side_and_skip_deleted_files() {
        let r = hunks(DIFF_A, Side::New);
        assert_eq!(r["src/a.rs"], [(1, 4), (22, 24)]);
        assert!(!hunks(DIFF_B, Side::New).contains_key("src/b.rs"));
        assert!(holds(&r, "src/a.rs", 3) && !holds(&r, "src/a.rs", 10));
    }

    #[test]
    fn a_line_goes_to_the_pr_whose_diff_holds_it() {
        let m = map_findings(
            &stack(),
            &diffs(),
            &[finding("src/a.rs", 23, None)],
            &live_ok(),
        );
        assert_eq!(m["by_pr"]["2"]["comments"][0]["line"], 23);
        assert_eq!(m["by_pr"]["2"]["head_sha"], "s2");
        assert_eq!(m["by_pr"]["2"]["comments"][0]["side"], "RIGHT");
    }

    #[test]
    fn with_no_pr_hint_the_topmost_open_pr_wins_a_shared_line() {
        let m = map_findings(
            &stack(),
            &diffs(),
            &[finding("src/a.rs", 3, None)],
            &live_ok(),
        );
        assert!(
            m["by_pr"].get("3").is_some() && m["by_pr"].get("2").is_none(),
            "{m}"
        );
    }

    #[test]
    fn a_pr_hint_is_honoured_and_checked() {
        let m = map_findings(
            &stack(),
            &diffs(),
            &[finding("src/a.rs", 2, Some(2))],
            &live_ok(),
        );
        assert!(m["by_pr"].get("2").is_some(), "{m}");
        let m = map_findings(
            &stack(),
            &diffs(),
            &[finding("src/a.rs", 99, Some(2))],
            &live_ok(),
        );
        assert!(m["unmapped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not in the diff of #2"));
    }

    #[test]
    fn merged_and_closed_prs_are_never_commented_on() {
        let f = [
            finding("src/a.rs", 3, Some(1)),
            finding("src/a.rs", 3, Some(4)),
        ];
        let m = map_findings(&stack(), &diffs(), &f, &live_ok());
        assert!(m["by_pr"].as_object().unwrap().is_empty());
        assert!(m["unmapped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("merged"));
        assert!(m["unmapped"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("closed"));
    }

    #[test]
    fn a_force_pushed_or_retargeted_pr_holds_its_findings() {
        let mut live = live_ok();
        live.insert(
            2,
            Live {
                head_sha: "new".into(),
                base: "b1".into(),
                open: true,
            },
        );
        live.insert(
            3,
            Live {
                head_sha: "s3".into(),
                base: "main".into(),
                open: true,
            },
        );
        let f = [
            finding("src/a.rs", 23, None),
            finding("src/a.rs", 3, Some(3)),
        ];
        let m = map_findings(&stack(), &diffs(), &f, &live);
        assert!(m["by_pr"].as_object().unwrap().is_empty());
        assert_eq!(m["stale"].as_array().unwrap().len(), 2);
        assert!(m["unmapped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("pushed to"));
        assert!(m["unmapped"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("retargeted"));
    }

    #[test]
    fn a_pr_closed_mid_review_is_stale() {
        let mut live = live_ok();
        live.insert(
            3,
            Live {
                head_sha: "s3".into(),
                base: "b2".into(),
                open: false,
            },
        );
        let m = map_findings(
            &stack(),
            &diffs(),
            &[finding("src/a.rs", 3, Some(3))],
            &live,
        );
        assert!(m["stale"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("no longer open"));
    }

    #[test]
    fn a_finding_on_code_a_pr_above_changes_is_listed_as_fixed_above() {
        let three = json!({"prs":[
            {"number":2,"state":"open","head_sha":"s2","base":"b1"},
            {"number":3,"state":"open","head_sha":"s3","base":"b2"},
            {"number":4,"state":"open","head_sha":"s4","base":"b3"}]});
        let mut d = diffs();
        d.insert(
            4,
            "--- a/src/q.rs\n+++ b/src/q.rs\n@@ -7,2 +7,2 @@\n-a\n+b\n".into(),
        );
        let mut live = live_ok();
        live.insert(
            4,
            Live {
                head_sha: "s4".into(),
                base: "b3".into(),
                open: true,
            },
        );
        let f = [
            finding("src/a.rs", 3, Some(2)),
            finding("src/a.rs", 2, Some(2)),
            finding("src/a.rs", 3, Some(3)),
        ];
        let m = map_findings(&three, &d, &f, &live);
        assert_eq!(m["fixed_above"].as_array().unwrap().len(), 1, "{m}");
        assert_eq!(m["fixed_above"][0]["pr"], 2);
        assert_eq!(m["fixed_above"][0]["fixed_by"], 3);
        assert_eq!(m["by_pr"]["2"]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(m["by_pr"]["3"]["comments"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_line_no_pr_changes_and_bad_findings_are_listed_not_dropped() {
        let f = [
            finding("src/z.rs", 1, None),
            json!({"file":"src/a.rs","body":"x"}),
            json!({"line":1}),
        ];
        let m = map_findings(&stack(), &diffs(), &f, &live_ok());
        assert_eq!(m["unmapped"].as_array().unwrap().len(), 3);
    }
}
