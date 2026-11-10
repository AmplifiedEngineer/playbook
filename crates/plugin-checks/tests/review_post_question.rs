// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins the rule that both review commands ask what to post. Only an
//! explicit `--self` or `--no-post` skips the question. Auto mode does not
//! change posting, and naming no PR or reviewing your own PR must not skip it.

use std::fs;
use std::path::PathBuf;

fn read(name: &str) -> String {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
        .join("skills/grounding-review/references")
        .join(format!("{name}.md"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const COMMANDS: [&str; 2] = ["quick-pass", "deep-pass"];

#[test]
fn both_commands_name_no_post_and_ask_what_to_post() {
    for name in COMMANDS {
        let text = read(name);
        assert!(text.contains("--no-post"), "{name}: no --no-post flag");
        assert!(
            text.contains("Post which findings as a pending review?"),
            "{name}: lost the post question"
        );
    }
}

#[test]
fn auto_mode_does_not_skip_the_question() {
    for name in COMMANDS {
        let text = read(name);
        assert!(
            text.contains("Auto mode does not change posting."),
            "{name}: auto rule missing"
        );
        assert!(
            text.contains("Pipelines and unattended callers must pass `--self`."),
            "{name}: pipelines rule missing"
        );
        assert!(
            !text.contains("To post: /playbook"),
            "{name}: stale auto line"
        );
        assert!(
            !text.contains("or the run mode is auto"),
            "{name}: auto still skips"
        );
    }
}

#[test]
fn no_pr_number_and_own_pr_do_not_skip_the_question() {
    for name in COMMANDS {
        let text = read(name);
        for stale in [
            "nothing was named to post to",
            "no PR number/branch was given",
            "or the resolved PR is authored by",
            "never posts either, even with a PR number",
        ] {
            assert!(!text.contains(stale), "{name}: still says `{stale}`");
        }
        assert!(
            text.contains("offer only `comment` and `skip`"),
            "{name}: own-PR verbs not narrowed"
        );
    }
}
