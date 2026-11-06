// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The shipped binary must never depend on the benchmark crate.

use std::process::Command;

const SHIPPED_PACKAGE: &str = "playbook";
const BENCH_PACKAGE: &str = "code-bench";

fn tree_names(tree: &str) -> Vec<&str> {
    tree.lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect()
}

fn mentions_bench(tree: &str) -> bool {
    tree_names(tree).contains(&BENCH_PACKAGE)
}

fn normal_tree(package: &str) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn shipped_binary_tree_has_no_code_bench() {
    let tree = normal_tree(SHIPPED_PACKAGE);
    assert!(tree.starts_with(SHIPPED_PACKAGE), "unexpected tree: {tree}");
    assert!(!mentions_bench(&tree));
}

#[test]
fn bench_crate_tree_is_detected_when_present() {
    assert!(mentions_bench(&normal_tree(BENCH_PACKAGE)));
}

#[test]
fn a_dependency_on_code_bench_would_fail_the_check() {
    let tree = "playbook v1.0.0\ncode-bench v1.0.0\nserde v1.0.0\n";
    assert!(mentions_bench(tree));
}
