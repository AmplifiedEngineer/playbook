// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structure of the scheduled workflows, read from the parsed YAML and never
//! from substrings.

use serde_norway::Value;
use std::path::{Path, PathBuf};

const ALWAYS: &str = "always()";
const UPLOAD_ACTION: &str = "actions/upload-artifact@";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load(file: &str) -> Value {
    let path = root().join(".github/workflows").join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    serde_norway::from_str(&text).unwrap()
}

fn key<'a>(value: &'a Value, name: &str) -> &'a Value {
    value
        .get(name)
        .unwrap_or_else(|| panic!("{name} is missing"))
}

fn steps(workflow: &Value, job: &str) -> Vec<Value> {
    key(key(key(workflow, "jobs"), job), "steps")
        .as_sequence()
        .unwrap()
        .clone()
}

fn is_upload(step: &Value) -> bool {
    step.get("uses")
        .and_then(Value::as_str)
        .is_some_and(|u| u.starts_with(UPLOAD_ACTION))
}

fn runs(step: &Value, command: &str) -> bool {
    step.get("run")
        .and_then(Value::as_str)
        .is_some_and(|r| r.contains(command))
}

#[test]
fn bench_uploads_its_report_after_the_gate_step_and_always() {
    let steps = steps(&load("code-bench.yml"), "bench");
    let gate = steps
        .iter()
        .position(|s| runs(s, "code-bench gate"))
        .expect("a gate step");
    let upload = steps.iter().position(is_upload).expect("an upload step");
    assert!(upload > gate);
    assert_eq!(key(&steps[upload], "if").as_str(), Some(ALWAYS));
}

#[test]
fn bench_gate_step_has_no_if_that_could_skip_it() {
    let steps = steps(&load("code-bench.yml"), "bench");
    let gate = steps.iter().find(|s| runs(s, "code-bench gate")).unwrap();
    assert!(gate.get("if").is_none());
}

#[test]
fn bench_runs_on_schedule_and_dispatch_only() {
    let workflow = load("code-bench.yml");
    let on = key(&workflow, "on").as_mapping().unwrap();
    let mut events: Vec<&str> = on.keys().filter_map(Value::as_str).collect();
    events.sort_unstable();
    assert_eq!(events, ["schedule", "workflow_dispatch"]);
}

#[test]
fn bench_matrix_covers_every_pilot_language() {
    let workflow = load("code-bench.yml");
    let matrix = key(
        key(key(key(&workflow, "jobs"), "bench"), "strategy"),
        "matrix",
    );
    let mut languages: Vec<&str> = key(matrix, "language")
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
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
    let workflow = load("code-bench.yml");
    let job = key(key(&workflow, "jobs"), "bench");
    assert!(key(job, "timeout-minutes").as_u64().is_some());
    assert!(key(&workflow, "permissions")
        .as_mapping()
        .unwrap()
        .is_empty());
    let granted = key(job, "permissions").as_mapping().unwrap();
    assert_eq!(granted.len(), 1);
    assert_eq!(
        key(key(job, "permissions"), "contents").as_str(),
        Some("read")
    );
}

#[test]
fn bench_actions_are_pinned_by_sha() {
    let steps = steps(&load("code-bench.yml"), "bench");
    for step in steps.iter().filter_map(|s| s.get("uses")?.as_str()) {
        let (_, pin) = step.split_once('@').unwrap();
        assert!(
            pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit()),
            "{step}"
        );
    }
}

const GATE_VARIABLE: &str = "vars.CODE_CI_PILOT_GATE_RAN == 'true'";

/// (workflow file, job id, command of its own pass or fail step)
const GATED: [(&str, &str, &str); 1] = [("code-mutants.yml", "mutants", "code-bench mutants")];

#[test]
fn each_gated_workflow_uploads_after_its_own_pass_or_fail_step() {
    for (file, job, command) in GATED {
        let steps = steps(&load(file), job);
        let check = steps
            .iter()
            .position(|s| runs(s, command))
            .unwrap_or_else(|| panic!("{file} has no step running {command}"));
        let upload = steps
            .iter()
            .position(is_upload)
            .unwrap_or_else(|| panic!("{file} has no upload step"));
        assert!(upload > check, "{file}");
        assert_eq!(key(&steps[upload], "if").as_str(), Some(ALWAYS), "{file}");
        assert!(steps[check].get("if").is_none(), "{file}");
    }
}

#[test]
fn gated_workflows_run_on_schedule_and_dispatch_only_after_the_pilot_gate() {
    for (file, job, _) in GATED {
        let workflow = load(file);
        let on = key(&workflow, "on").as_mapping().unwrap();
        let mut events: Vec<&str> = on.keys().filter_map(Value::as_str).collect();
        events.sort_unstable();
        assert_eq!(events, ["schedule", "workflow_dispatch"], "{file}");
        let gate = key(key(key(&workflow, "jobs"), job), "if");
        assert_eq!(gate.as_str(), Some(GATE_VARIABLE), "{file}");
    }
}

#[test]
fn gated_jobs_have_a_timeout_and_least_privilege() {
    for (file, job, _) in GATED {
        let workflow = load(file);
        let job = key(key(&workflow, "jobs"), job);
        assert!(key(job, "timeout-minutes").as_u64().is_some(), "{file}");
        assert!(key(&workflow, "permissions")
            .as_mapping()
            .unwrap()
            .is_empty());
        let granted = key(job, "permissions");
        assert_eq!(granted.as_mapping().unwrap().len(), 1, "{file}");
        assert_eq!(key(granted, "contents").as_str(), Some("read"), "{file}");
    }
}

#[test]
fn gated_actions_are_pinned_by_sha() {
    for (file, job, _) in GATED {
        for step in steps(&load(file), job)
            .iter()
            .filter_map(|s| s.get("uses")?.as_str().map(String::from))
        {
            let (_, pin) = step.split_once('@').unwrap();
            let sha = pin.len() == 40 && pin.bytes().all(|b| b.is_ascii_hexdigit());
            assert!(sha, "{file}: {step}");
        }
    }
}
