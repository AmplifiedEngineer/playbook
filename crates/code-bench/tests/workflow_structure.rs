// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structure of the scheduled workflow, read line by line. The file has a
//! fixed layout (two-space indent per level, one step per `- ` at six spaces),
//! and zizmor lints the same file in CI, so no YAML parser is needed here.

use std::path::{Path, PathBuf};

const ALWAYS: &str = "if: always()";
const UPLOAD_ACTION: &str = "actions/upload-artifact@";
const GATE_COMMAND: &str = "code-bench gate";
const STEP_START: &str = "      - ";
const SHA_LEN: usize = 40;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(file: &str) -> Vec<String> {
    let path = root().join(".github/workflows").join(file);
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn bench() -> Vec<String> {
    load("code-bench.yml")
}

/// The lines of each step of the bench job, one block per step.
fn steps(lines: &[String]) -> Vec<Vec<&str>> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut in_steps = false;
    for line in lines {
        if line == "    steps:" {
            in_steps = true;
            continue;
        }
        if !in_steps {
            continue;
        }
        if line.starts_with(STEP_START) {
            blocks.push(Vec::new());
        }
        if let Some(block) = blocks.last_mut() {
            block.push(line.as_str());
        }
    }
    blocks
}

fn mentions(block: &[&str], text: &str) -> bool {
    block.iter().any(|l| l.contains(text))
}

/// The lines directly under a top-level or job-level `header:` line, down to
/// the first line indented no deeper than the header.
fn children<'a>(lines: &'a [String], header: &str) -> Vec<&'a str> {
    let depth = header.len() - header.trim_start().len();
    let start = lines.iter().position(|l| l == header).unwrap();
    lines[start + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || l.len() - l.trim_start().len() > depth)
        .map(String::as_str)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn events(lines: &[String]) -> Vec<&str> {
    let mut events: Vec<&str> = children(lines, "on:")
        .into_iter()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .map(|l| l.trim().trim_end_matches(':'))
        .collect();
    events.sort_unstable();
    events
}

fn assert_timeout_and_least_privilege(lines: &[String], file: &str) {
    let timeout = lines
        .iter()
        .find_map(|l| l.strip_prefix("    timeout-minutes: "))
        .unwrap_or_else(|| panic!("{file} has no timeout"));
    assert!(timeout.trim().parse::<u32>().is_ok(), "{file}");
    assert!(lines.iter().any(|l| l == "permissions: {}"), "{file}");
    let granted: Vec<&str> = children(lines, "    permissions:")
        .into_iter()
        .map(str::trim)
        .collect();
    assert_eq!(granted, ["contents: read"], "{file}");
}

fn assert_pinned(lines: &[String], file: &str) {
    let uses: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_once("uses: ").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();
    assert!(!uses.is_empty(), "{file}");
    for step in uses {
        let (_, pin) = step.split_once('@').unwrap();
        assert!(
            pin.len() == SHA_LEN && pin.bytes().all(|b| b.is_ascii_hexdigit()),
            "{file}: {step}"
        );
    }
}

#[test]
fn bench_uploads_its_report_after_the_gate_step_and_always() {
    let lines = bench();
    let steps = steps(&lines);
    let gate = steps
        .iter()
        .position(|s| mentions(s, GATE_COMMAND))
        .expect("a gate step");
    let upload = steps
        .iter()
        .position(|s| mentions(s, UPLOAD_ACTION))
        .expect("an upload step");
    assert!(upload > gate);
    assert!(steps[upload].iter().any(|l| l.trim() == ALWAYS));
}

#[test]
fn bench_gate_step_has_no_if_that_could_skip_it() {
    let lines = bench();
    let steps = steps(&lines);
    let gate = steps
        .iter()
        .find(|s| mentions(s, GATE_COMMAND))
        .expect("a gate step");
    assert!(!gate.iter().any(|l| l.trim_start().starts_with("if:")));
}

#[test]
fn bench_runs_on_schedule_and_dispatch_only() {
    assert_eq!(events(&bench()), ["schedule", "workflow_dispatch"]);
}

#[test]
fn bench_matrix_covers_every_pilot_language() {
    let lines = bench();
    let list = lines
        .iter()
        .find_map(|l| l.trim().strip_prefix("language: ["))
        .expect("a language list")
        .trim_end_matches(']');
    let mut languages: Vec<&str> = list.split(',').map(str::trim).collect();
    languages.sort_unstable();
    let manifest = code_bench::corpus::Manifest::bundled().unwrap();
    let mut pilot: Vec<&str> = manifest
        .tier(code_bench::corpus::Tier::Pilot)
        .map(|r| r.language.as_str())
        .collect();
    pilot.sort_unstable();
    pilot.dedup();
    assert_eq!(languages, pilot);
}

#[test]
fn bench_job_has_a_timeout_and_least_privilege() {
    assert_timeout_and_least_privilege(&bench(), "code-bench.yml");
}

#[test]
fn bench_actions_are_pinned_by_sha() {
    assert_pinned(&bench(), "code-bench.yml");
}

const GATE_VARIABLE: &str = "vars.CODE_CI_PILOT_GATE_RAN == 'true'";

/// (workflow file, command of its own pass or fail step)
const GATED: [(&str, &str); 1] = [("code-mutants.yml", "code-bench mutants")];

#[test]
fn each_gated_workflow_uploads_after_its_own_pass_or_fail_step() {
    for (file, command) in GATED {
        let lines = load(file);
        let steps = steps(&lines);
        let check = steps
            .iter()
            .position(|s| mentions(s, command))
            .unwrap_or_else(|| panic!("{file} has no step running {command}"));
        let upload = steps
            .iter()
            .position(|s| mentions(s, UPLOAD_ACTION))
            .unwrap_or_else(|| panic!("{file} has no upload step"));
        assert!(upload > check, "{file}");
        assert!(steps[upload].iter().any(|l| l.trim() == ALWAYS), "{file}");
        assert!(
            !steps[check]
                .iter()
                .any(|l| l.trim_start().starts_with("if:")),
            "{file}"
        );
    }
}

#[test]
fn gated_workflows_run_on_schedule_and_dispatch_only_after_the_pilot_gate() {
    for (file, _) in GATED {
        let lines = load(file);
        assert_eq!(events(&lines), ["schedule", "workflow_dispatch"], "{file}");
        let gate = format!("    if: {GATE_VARIABLE}");
        assert!(lines.contains(&gate), "{file}");
    }
}

#[test]
fn gated_jobs_have_a_timeout_and_least_privilege() {
    for (file, _) in GATED {
        assert_timeout_and_least_privilege(&load(file), file);
    }
}

#[test]
fn gated_actions_are_pinned_by_sha() {
    for (file, _) in GATED {
        assert_pinned(&load(file), file);
    }
}
