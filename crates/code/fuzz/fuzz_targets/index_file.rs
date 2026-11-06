// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#![no_main]

use libfuzzer_sys::fuzz_target;
use playbook_code::index;
use playbook_code_fuzz::split_input;
use std::fs;
use std::process::Command;

const WORKTREE: &str = "fuzz-worktree";

fuzz_target!(|data: &[u8]| {
    let Some((language, bytes)) = split_input(data) else {
        return;
    };
    let scratch = std::env::temp_dir().join(format!("playbook-fuzz-{}", std::process::id()));
    let root = scratch.join("repo");
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&root).expect("scratch repo");
    let name = format!("input.{}", extension(language));
    fs::write(root.join(&name), bytes).expect("write input");
    let init = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["init", "--quiet"])
        .status()
        .expect("git runs");
    assert!(init.success());
    let store = scratch.join("store").join("index.sqlite3");
    index::run(&root, &store, WORKTREE, false, &index::Options::default())
        .expect("a file of any content indexes without error");
    let _ = fs::remove_dir_all(&scratch);
});

fn extension(language: playbook_code::lang::Lang) -> &'static str {
    use playbook_code::lang::Lang;
    match language {
        Lang::Rust => "rs",
        Lang::TypeScript => "ts",
        Lang::Tsx => "tsx",
        Lang::JavaScript => "js",
        Lang::Python => "py",
        Lang::Go => "go",
    }
}
