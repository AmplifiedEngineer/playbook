// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pre-clap fast path must agree with clap on every form it handles, and
//! hand everything else to clap unchanged.

#[path = "support/isolated.rs"]
mod isolated;

use clap::{CommandFactory, Parser, ValueEnum};
use playbook::fastpath::{classify, version_text, Fast};
use playbook::{Cli, Command, HookName};
use std::ffi::OsString;

fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

fn name_of(v: HookName) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

#[test]
fn every_hook_name_resolves_to_the_same_hook_as_clap() {
    for v in HookName::value_variants() {
        let name = name_of(*v);
        let fast = classify(&argv(&["playbook", "hook", &name]));
        let Some(Fast::Hook(fast)) = fast else {
            panic!("{name} was not recognised");
        };
        let cli = Cli::try_parse_from(["playbook", "hook", &name]).unwrap();
        let Command::Hook { name: clap_name } = cli.command else {
            panic!("clap parsed {name} as another command");
        };
        assert_eq!(name_of(fast), name_of(clap_name), "{name}");
    }
}

#[test]
fn a_form_the_fast_path_skips_is_still_parsed_or_rejected_by_clap() {
    for parts in [
        &["playbook", "hook", "nope"][..],
        &["playbook", "hook", "session-init", "extra"],
        &["playbook", "statusline", "--bogus"],
    ] {
        assert!(classify(&argv(parts)).is_none(), "{parts:?}");
        assert!(Cli::try_parse_from(parts).is_err(), "{parts:?}");
    }
}

#[test]
fn the_code_semantic_commands_are_left_to_clap() {
    for parts in [
        &["playbook", "code", "definition", "src/a.ts:1:4"][..],
        &["playbook", "code", "hover", "src/a.ts:1:4"],
        &["playbook", "code", "diagnostics"],
        &["playbook", "code", "lsp", "status"],
    ] {
        assert!(classify(&argv(parts)).is_none(), "{parts:?}");
    }
}

#[test]
fn version_text_equals_what_clap_renders() {
    assert_eq!(version_text(), Cli::command().render_version());
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = isolated::playbook().args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_binary_prints_the_same_for_the_flag_and_the_clap_subcommand() {
    let fast = run(&["--version"]);
    let short = run(&["-V"]);
    let clap = run(&["version"]);
    assert_eq!(fast, clap);
    assert_eq!(short, clap);
}

#[test]
fn help_and_error_output_still_come_from_clap() {
    let (code, out, _) = run(&["hook", "--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage"), "{out}");
    let (code, _, err) = run(&["hook", "nope"]);
    assert_ne!(code, 0);
    assert!(err.contains("invalid value"), "{err}");
}

#[test]
fn a_code_argv_is_never_taken_by_the_fast_path() {
    for parts in [
        &["playbook", "code", "index"][..],
        &["playbook", "code", "index", "--json"],
        &["playbook", "code"],
    ] {
        assert!(classify(&argv(parts)).is_none(), "{parts:?}");
    }
}

#[test]
fn shell_init_is_gone_from_clap_the_help_and_the_cli_reference() {
    assert!(Cli::command().find_subcommand("shell-init").is_none());
    assert!(Cli::try_parse_from(["playbook", "shell-init"]).is_err());
    let (code, help, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(!help.contains("shell-init"), "{help}");
    let reference = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference/cli-and-config.md"
    ))
    .unwrap();
    assert!(!reference.contains("shell-init"));
}

#[test]
fn disk_is_gone_from_clap_the_help_and_the_cli_reference() {
    assert!(Cli::command().find_subcommand("disk").is_none());
    assert!(Cli::try_parse_from(["playbook", "disk", "status"]).is_err());
    let (code, help, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(!help.contains("playbook disk"), "{help}");
    let reference = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference/cli-and-config.md"
    ))
    .unwrap();
    assert!(!reference.contains("playbook disk"));
}
