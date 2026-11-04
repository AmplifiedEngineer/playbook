// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins the gate loop rules to ONE file and the three commands to that file.
//! The cap and the STUCK rule used to be copied into `plan`, `adr` and
//! `implement` as "max 3"; this fails if a number or a copy drifts back.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const RULES: &str = "skills/delegating-subagents/references/gate-loop.md";
const COMMANDS: [&str; 3] = [
    "commands/plan.md",
    "commands/adr.md",
    "commands/implement.md",
];

#[test]
fn the_rules_file_pins_the_cap_and_the_stuck_definition() {
    let rules = read(RULES);
    for needle in [
        "hard cap is `gate.maxRounds` rounds",
        "Intent: config-get",
        "PASS or WARN stops the phase at once",
        "**STUCK**",
        "--stuck",
        "Two consecutive rounds with no progress",
        "as many findings as B(n-1) or more",
        "still contains the top finding",
        "COMPLETE current artifact",
        "INCONCLUSIVE",
    ] {
        assert!(rules.contains(needle), "{RULES} lost: {needle}");
    }
    assert!(!rules.contains("max 3"), "the old cap must not return");
    assert!(
        !rules.contains("hard cap is 5"),
        "the cap is read from gate.maxRounds, not fixed"
    );
}

#[test]
fn each_command_points_at_the_rules_and_keeps_no_copy_of_the_old_cap() {
    for path in COMMANDS {
        let text = read(path);
        assert!(
            text.contains("references/gate-loop.md"),
            "{path} must reference the gate loop file"
        );
        for stale in [
            "max 3 iterations",
            "(max 3)",
            "Max 3 iterations",
            "after its 3 iterations",
        ] {
            assert!(!text.contains(stale), "{path} still says: {stale}");
        }
        assert!(
            text.matches("hard cap `gate.maxRounds` rounds").count() <= 1,
            "{path} repeats the loop summary instead of pointing at it"
        );
    }
}

#[test]
fn the_skill_links_the_rules_from_its_redispatch_section() {
    assert!(read("skills/delegating-subagents/SKILL.md").contains("references/gate-loop.md"));
}
