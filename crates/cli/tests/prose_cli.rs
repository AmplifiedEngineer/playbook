// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook prose check` as the real binary runs it.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;

fn run(tag: &str, text: &str, strict: bool) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("prose-cli-{}-{tag}", playbook::testing::run_id()));
    fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("a.md");
    fs::write(&file, text).expect("scratch file");
    let mut cmd = isolated::playbook();
    cmd.args(["prose", "check"]).env_remove("GITHUB_ACTIONS");
    if strict {
        cmd.arg("--strict");
    }
    let out = cmd.arg(&file).output().expect("playbook runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_hard_finding_exits_zero_without_strict() {
    let (code, out) = run("a", "Let that sink in.\n", false);

    assert_eq!(code, 0);
    assert!(out.contains("a.md:1: emphasis crutch"), "{out}");
    assert!(out.contains("1 hard"), "{out}");
}

#[test]
fn a_hard_finding_exits_one_with_strict() {
    let (code, _) = run("b", "Let that sink in.\n", true);

    assert_eq!(code, 1);
}

#[test]
fn a_soft_finding_exits_zero_even_with_strict() {
    let (code, out) = run("c", "It always fails.\n", true);

    assert_eq!(code, 0);
    assert!(out.contains("absolute word"), "{out}");
}

#[test]
fn passive_voice_warns_and_never_fails_strict() {
    let (code, out) = run("d", "The retry was added.\n", true);

    assert_eq!(code, 0);
    assert!(out.contains("passive voice"), "{out}");
}

#[test]
fn a_facts_file_makes_an_invented_number_a_hard_finding() {
    let dir = std::env::temp_dir().join(format!("prose-cli-{}-e", playbook::testing::run_id()));
    fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("a.md");
    fs::write(&file, "Fixed in 9 places.\n").expect("text");
    fs::write(dir.join("a.md.facts"), "kind: reply\nFixed in 3 places.\n").expect("facts");

    let out = isolated::playbook()
        .args(["prose", "check", "--strict"])
        .env_remove("GITHUB_ACTIONS")
        .arg(&file)
        .output()
        .expect("playbook runs");

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("token not in the facts \"9\""));
}
