// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins the numbered pick list that replaces the tier-only question. The user
//! marks findings or comments with a reply such as `1,3,5-7` or `all except 4`.
//! `AskUserQuestion` offers at most four options, so it cannot carry a long
//! list. The rules live in one reference that the review commands and
//! `address-pr-comments` all point at.

use std::fs;
use std::path::PathBuf;

const REFERENCE: &str = "skills/grounding-review/references/pick-list.md";
const REFERENCE_POINTER: &str = "skills/grounding-review/references/pick-list.md";

const POSTING_COMMANDS: [&str; 2] = ["quick-review", "deep-review"];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_reference_defines_the_reply_forms() {
    let text = read(REFERENCE);
    for form in ["1,3,5-7", "all except 4", "`all`", "`none`"] {
        assert!(text.contains(form), "pick-list.md: lost the form {form}");
    }
    assert!(
        text.contains("[x]") && text.contains("[ ]"),
        "pick-list.md: lost the checkbox marks"
    );
    assert!(
        text.contains("empty reply"),
        "pick-list.md: lost the default-selection rule"
    );
}

#[test]
fn the_reference_keeps_the_tier_names_working() {
    let text = read(REFERENCE);
    for tier in [
        "only blockers",
        "all must-fix",
        "all must-fix + questions",
        "all except optional",
        "all findings",
    ] {
        assert!(text.contains(tier), "pick-list.md: tier `{tier}` dropped");
    }
}

#[test]
fn both_review_commands_use_the_pick_list_and_keep_the_post_question() {
    for name in POSTING_COMMANDS {
        let text = read(&format!("commands/{name}.md"));
        assert!(
            text.contains(REFERENCE_POINTER),
            "{name}: does not read the pick list"
        );
        assert!(
            text.contains("Post which findings as a pending review?"),
            "{name}: lost the post question"
        );
        assert!(
            text.contains("--self") && text.contains("--no-post"),
            "{name}: lost the flags that skip the question"
        );
    }
}

#[test]
fn address_pr_comments_picks_the_items_from_a_list() {
    let text = read("commands/address-pr-comments.md");
    assert!(
        text.contains(REFERENCE_POINTER),
        "address-pr-comments: does not read the pick list"
    );
    assert!(
        text.contains("Which comments to work on?"),
        "address-pr-comments: lost the pick question"
    );
    assert!(
        text.contains("--dry-run"),
        "address-pr-comments: lost the flag that skips the question"
    );
}
