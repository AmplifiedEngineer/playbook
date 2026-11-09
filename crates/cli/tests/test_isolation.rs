// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A test that starts the built `playbook` with the real `HOME` runs the
//! first-run heal against the real home: it once wrote the real migration
//! record and moved the real collector agent. This check fails for any test
//! file that starts the built `playbook` without `support/isolated.rs` or a
//! `HOME` of its own.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::path::{Path, PathBuf};

const SPAWNS: [&str; 2] = [
    "Command::new(env!(\"CARGO_BIN_EXE_playbook\"))",
    "Proc::new(env!(\"CARGO_BIN_EXE_playbook\"))",
];

/// Files the check skips, each with its reason. The binary refuses to heal
/// the real home from a cargo test anyway.
const EXEMPT: [&str; 2] = [
    // Copies the binary for shell scripts; each caller sets HOME.
    "cli/tests/support/shell_bin_dir.rs",
    // Links the binary into a sandbox bin folder. Every child it runs has a
    // cleared environment with HOME inside the sandbox.
    "cli/tests/docs_recipes.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// How far after a spawn its own `.env("HOME", ...)` may sit.
const WINDOW: usize = 20;

/// The spawns in `text` that set no `HOME` within [`WINDOW`] lines, by line
/// number. A file that names the binary in some other way, as a `bin()`
/// helper does, must set `HOME` or use the helper somewhere.
fn bare_spawns(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let homed = |i: usize| {
        lines[i..lines.len().min(i + WINDOW)]
            .iter()
            .any(|l| l.contains(".env(\"HOME\"") || l.contains("isolate("))
    };
    let mut bare: Vec<usize> = (0..lines.len())
        .filter(|&i| SPAWNS.iter().any(|s| lines[i].contains(s)) && !homed(i))
        .map(|i| i + 1)
        .collect();
    let isolated = text.contains("isolated::") || text.contains(".env(\"HOME\"");
    if bare.is_empty() && text.contains("env!(\"CARGO_BIN_EXE_playbook\")") && !isolated {
        bare.push(0);
    }
    bare
}

/// The files under `crates/*/tests` that start the built `playbook` without
/// the helper or their own `HOME`, with the line of each bare spawn.
fn unisolated(crates: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for krate in fs::read_dir(crates).into_iter().flatten().flatten() {
        rust_files(&krate.path().join("tests"), &mut files);
    }
    files
        .iter()
        .filter(|f| !f.ends_with("support/isolated.rs") && !f.ends_with("test_isolation.rs"))
        .filter(|f| !EXEMPT.iter().any(|e| f.ends_with(e)))
        .filter_map(|f| {
            let bare = bare_spawns(&fs::read_to_string(f).ok()?);
            (!bare.is_empty()).then(|| format!("{}:{bare:?}", f.display()))
        })
        .collect()
}

#[test]
fn every_test_that_starts_playbook_gives_it_a_scratch_home() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    let bad = unisolated(&crates);

    assert!(
        bad.is_empty(),
        "start the binary with isolated::playbook() from support/isolated.rs, or set HOME: {bad:?}"
    );
}

#[test]
fn the_check_catches_each_spawn_without_a_home() {
    let spawn = "Command::new(env!(\"CARGO_BIN_EXE_playbook\"))";
    let homed = format!("{spawn}\n    .env(\"HOME\", &h)\n");
    let bare = format!("{spawn}\n    .arg(\"x\")\n");
    let filler = "\n".repeat(WINDOW);

    assert_eq!(bare_spawns(&homed), Vec::<usize>::new());
    assert_eq!(bare_spawns(&bare), vec![1]);
    assert_eq!(
        bare_spawns(&format!("{homed}{filler}{bare}")),
        vec![WINDOW + 3],
        "one HOME does not cover another spawn in the same file"
    );
    assert_eq!(
        bare_spawns("fn f() { isolated::playbook(); }"),
        Vec::<usize>::new()
    );
    assert_eq!(
        bare_spawns("fn bin() -> &'static str { env!(\"CARGO_BIN_EXE_playbook\") }"),
        vec![0],
        "a helper that names the binary needs a HOME somewhere"
    );
}

#[test]
fn the_helper_points_every_home_at_the_scratch_home_and_drops_playbook_variables() {
    let home = isolated::fresh_home();
    let mut cmd = std::process::Command::new("/usr/bin/env");
    isolated::isolate(&mut cmd, &home);
    cmd.env("KEEP", "1");

    let out = cmd.env("PLAYBOOK_ALLOW_REAL_HOME", "1").output().unwrap();
    let mut probe = std::process::Command::new("/usr/bin/env");
    std::env::set_var("PLAYBOOK_LEAK_PROBE", "1");
    isolated::isolate(&mut probe, &home);
    let dropped = probe.output().unwrap();
    std::env::remove_var("PLAYBOOK_LEAK_PROBE");

    let env = String::from_utf8_lossy(&out.stdout);
    for want in [
        "HOME=",
        "XDG_CONFIG_HOME=",
        "XDG_CACHE_HOME=",
        "XDG_DATA_HOME=",
        "CLAUDE_CONFIG_DIR=",
    ] {
        let line = env
            .lines()
            .find(|l| l.starts_with(want))
            .unwrap_or_default();
        assert!(line.contains(&home.display().to_string()), "{want}: {line}");
    }
    assert!(
        env.contains("PLAYBOOK_ALLOW_REAL_HOME=1"),
        "a variable set after isolate reaches the child"
    );
    assert!(!String::from_utf8_lossy(&dropped.stdout).contains("PLAYBOOK_LEAK_PROBE"));
}

#[test]
fn a_child_of_a_cargo_test_inherits_the_signal_the_net_reads() {
    // The binary's net reads CARGO_MANIFEST_DIR or NEXTEST_RUN_ID at run
    // time, so both cargo test and nextest must pass one to a child.
    let out = std::process::Command::new("/usr/bin/env").output().unwrap();
    let env = String::from_utf8_lossy(&out.stdout);

    assert!(
        env.lines()
            .any(|l| l.starts_with("CARGO_MANIFEST_DIR=") || l.starts_with("NEXTEST_RUN_ID=")),
        "{env}"
    );
}

/// What reads the real home of the account from a test process.
const HOME_READS: [&str; 3] = ["env::var(\"HOME\")", "env::home_dir()", "home_dir()"];

/// The lines of `text` that read the process home, comments left out. For a
/// unit test file only the part after `#[cfg(test)]` counts, and a file that
/// runs its tests through `in_scratch_home` is allowed to read it.
fn home_reads(text: &str, unit: bool) -> Vec<usize> {
    let start = if unit {
        text.find("#[cfg(test)]").unwrap_or(text.len())
    } else {
        0
    };
    if text[start..].contains("in_scratch_home(") {
        return Vec::new();
    }
    let before = text[..start].lines().count();
    text[start..]
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .filter(|(_, l)| HOME_READS.iter().any(|r| l.contains(r)))
        .map(|(i, _)| before + i)
        .collect()
}

/// The hook test files that read the process home, with the lines.
fn hook_tests_on_real_home(cli: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&cli.join("src/hooks"), &mut files);
    let mut integration = Vec::new();
    rust_files(&cli.join("tests"), &mut integration);
    let hook_integration = integration.into_iter().filter(|f| {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.starts_with("hooks_") || name == "headless_hooks.rs"
    });
    let units = files.into_iter().map(|f| (f, true));
    let all = units.chain(hook_integration.map(|f| (f, false)));
    all.filter_map(|(f, unit)| {
        let reads = home_reads(&fs::read_to_string(&f).ok()?, unit);
        (!reads.is_empty()).then(|| format!("{}:{reads:?}", f.display()))
    })
    .collect()
}

#[test]
fn no_hook_test_reads_the_real_home() {
    let cli = Path::new(env!("CARGO_MANIFEST_DIR"));

    let bad = hook_tests_on_real_home(cli);

    assert!(
        bad.is_empty(),
        "run a hook test in a scratch home (testing::in_scratch_home) or start the binary with \
         isolated::playbook(), and do not read the process HOME: {bad:?}"
    );
}

#[test]
fn the_real_home_check_catches_a_read_and_allows_the_helper() {
    let unit = "fn f() { home_dir(); }\n#[cfg(test)]\nmod tests {\n    fn t() { let h = home_dir(); }\n}\n";
    let scratch =
        "#[cfg(test)]\nmod tests {\n    fn t() { if !in_scratch_home(\"a::t\") { return; } home_dir(); }\n}\n";
    let integration =
        "// std::env::var(\"HOME\") in a comment\nfn t() { std::env::var(\"HOME\"); }\n";

    assert_eq!(home_reads(unit, true), vec![3], "only the test part counts");
    assert_eq!(home_reads(scratch, true), Vec::<usize>::new());
    assert_eq!(home_reads(integration, false), vec![1]);
}
