// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structural parity checks for the command files: the run mode is read in a
//! Step 0 section before any question, and the per-kind auto-mode rules.
//! The shared parser and the file table are in `support/auto_parity.rs`.

#[path = "support/auto_parity.rs"]
mod ap;
#[path = "support/auto_parity_auto.rs"]
mod ap_auto;

use ap::*;
use ap_auto::*;
use regex::Regex;
use std::fs;

#[test]
fn every_command_file_has_a_row_in_the_table_and_the_reverse() {
    // Arrange
    let mut on_disk: Vec<String> = fs::read_dir(commands_dir())
        .expect("commands/ is readable")
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    let mut in_table: Vec<String> = COMMANDS.iter().map(|c| c.name.to_owned()).collect();

    // Act
    on_disk.sort();
    in_table.sort();

    // Assert
    assert_eq!(
        on_disk, in_table,
        "commands/*.md and the COMMANDS table must list the same commands"
    );
}

#[test]
fn step0_section_reads_mode_status() {
    // Arrange
    let mut failures = Vec::new();

    for spec in COMMANDS.iter().filter(|c| c.auto != Auto::Unchanged) {
        let file = load(spec.name);

        // Act
        let step0 = file.step0();

        // Assert
        match step0 {
            None => failures.push(format!("{}: no `Step 0` section", spec.name)),
            Some(s) if !reads_mode(&file.text_of(&s)) => failures.push(format!(
                "{}: `{}` never reads the run mode (Intent: run-mode)",
                spec.name, s.title
            )),
            Some(_) => {}
        }
    }

    assert!(
        failures.is_empty(),
        "Step 0 problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn step0_comes_before_the_first_ask_outside_it() {
    // Arrange
    let mut failures = Vec::new();

    for spec in COMMANDS.iter().filter(|c| c.asks) {
        let file = load(spec.name);
        let Some(step0) = file.step0() else {
            failures.push(format!("{}: no `Step 0` section to order", spec.name));
            continue;
        };

        // Act
        let first_ask = file.first_ask_outside(Some(&step0));

        // Assert
        if let Some((line, text)) = first_ask {
            if line < step0.start {
                failures.push(format!(
                    "{}: ask on line {} precedes Step 0 (line {}): {}",
                    spec.name,
                    line + 1,
                    step0.start + 1,
                    text.trim()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "ordering problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn positive_control_every_asking_command_has_an_ask_outside_step0() {
    // Arrange
    let asking: Vec<&CommandSpec> = COMMANDS.iter().filter(|c| c.asks).collect();
    for required in ["plan", "implement", "deep-review"] {
        assert!(
            asking.iter().any(|c| c.name == required),
            "the control table must include {required}"
        );
    }

    for spec in asking {
        let file = load(spec.name);
        let step0 = file.step0();

        // Act
        let first_ask = file.first_ask_outside(step0.as_ref());

        // Assert
        assert!(
            first_ask.is_some(),
            "commands/{}.md is expected to ask something outside Step 0, but the ask \
             detector found nothing, so the ordering checks would pass vacuously",
            spec.name
        );
    }
}

#[test]
fn ask_detector_matches_question_forms_and_not_mentions() {
    // Arrange
    let asks = [
        r#"- **Q1:** "Post which findings as a pending review?" Offer six tiers."#,
        r#"**Q1**: "Post which findings as a pending review?" Offer six tiers."#,
        r#"- **Q2:** "Submit verb? approve / comment / request-changes / skip." Reaching it."#,
        r#"Plan the rest. Ask once: "Write these to memory?""#,
        "call the AskUserQuestion tool ONCE",
    ];
    let not_asks = [
        "8. **Never ask whether to run.** Invoking the command IS the instruction to run.",
        "Q2 stays pending until the author submits it.",
        "Run every task in order and report each result.",
        "The reviewer asked for changes on three files.",
    ];

    // Act
    let missed: Vec<&&str> = asks.iter().filter(|l| !is_ask(l)).collect();
    let false_hits: Vec<&&str> = not_asks.iter().filter(|l| is_ask(l)).collect();

    // Assert
    assert!(missed.is_empty(), "detector missed real asks: {missed:?}");
    assert!(
        false_hits.is_empty(),
        "detector matched prose that is not an ask: {false_hits:?}"
    );
}

#[test]
fn implement_allowed_tools_include_ask_user_question() {
    // Arrange
    let file = load("implement");

    // Act
    let tools = file.allowed_tools();

    // Assert
    assert!(
        tools.iter().any(|t| t == "AskUserQuestion"),
        "implement allowed-tools {tools:?} lacks AskUserQuestion"
    );
}

#[test]
fn implement_never_hard_resets() {
    // Arrange
    let file = load("implement");

    // Act
    let offenders: Vec<usize> = file
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("reset --hard"))
        .map(|(i, _)| i + 1)
        .collect();

    // Assert
    assert!(
        offenders.is_empty(),
        "commands/implement.md mentions `reset --hard` on lines {offenders:?}"
    );
}

#[test]
fn implement_auto_path_never_forces_and_parks_on_non_fast_forward() {
    // Arrange
    let file = load("implement");
    let auto_path = require_section(&file, file.auto_path(), "`Auto path`");

    // Act
    let text = file.text_of(&auto_path);

    // Assert
    assert!(
        !text.contains("--force"),
        "the auto path section must not mention --force or --force-with-lease"
    );
    let parks = Regex::new(r"(?is)non-fast-forward[^.]*\bparks?\b[^.]*\bSegment\b")
        .expect("park pattern is valid");
    assert!(
        parks.is_match(&text),
        "the auto path section must state that a non-fast-forward push parks the Segment"
    );
}

#[test]
fn plan_step6_says_plain_auto_stops_with_a_message() {
    // Arrange
    let file = load("plan");
    let step6 = require_section(&file, file.step("6"), "`Step 6`");

    // Act
    let text = file.text_of(&step6);

    // Assert
    let stops = Regex::new(r"(?i)plain `--auto`[^.\n]*\bstops?\b").expect("stop pattern is valid");
    assert!(
        stops.is_match(&text),
        "Step 6 must say that plain `--auto` stops"
    );
    assert!(
        text.to_lowercase().contains("message"),
        "Step 6 must say plain `--auto` stops with a clear message"
    );
}

#[test]
fn auto_design_appears_in_plan_and_in_the_fix_hand_off_only() {
    // Arrange
    let mut with_flag: Vec<String> = fs::read_dir(commands_dir())
        .expect("commands directory should be readable")
        .map(|e| e.expect("directory entry should be readable").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter(|p| {
            fs::read_to_string(p)
                .expect("command file should be readable")
                .contains("--auto-design")
        })
        .map(|p| {
            p.file_stem()
                .expect("command file has a stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    // Act
    with_flag.sort();

    // Assert
    assert_eq!(with_flag, vec!["fix".to_owned(), "plan".to_owned()]);
}

#[test]
fn refuse_list_step0_stops_in_auto_naming_the_command_and_why() {
    // Arrange
    let mut failures = Vec::new();

    for spec in rows(Auto::Refuses) {
        let file = load(spec.name);
        let Some(step0) = file.step0() else {
            failures.push(format!("{}: no `Step 0` section", spec.name));
            continue;
        };
        let command = format!("/playbook:{}", spec.name);
        let blocks = file.paragraphs(&step0);

        // Act
        let read = blocks.iter().find(|(_, b)| reads_mode(b));
        let refusal = blocks.iter().find(|(_, b)| {
            let b = b.to_lowercase();
            b.contains("auto")
                && b.contains("stop")
                && b.contains(&command)
                && b.contains("because")
        });

        // Assert
        let Some((refusal_line, _)) = refusal else {
            failures.push(format!(
                "{}: Step 0 has no refusal that mentions auto, stop, `{command}` and because",
                spec.name
            ));
            continue;
        };
        match read {
            None => failures.push(format!("{}: Step 0 never reads the mode", spec.name)),
            Some((read_line, _)) if read_line > refusal_line => failures.push(format!(
                "{}: the refusal (line {}) precedes the mode read (line {})",
                spec.name,
                refusal_line + 1,
                read_line + 1
            )),
            Some(_) => {}
        }
        if let Some((line, text)) = file.first_ask_outside(None) {
            if line < *refusal_line {
                failures.push(format!(
                    "{}: ask on line {} precedes the refusal (line {}): {}",
                    spec.name,
                    line + 1,
                    refusal_line + 1,
                    text.trim()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "refusal problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn review_step0_says_auto_does_not_skip_the_post_question() {
    // Arrange
    let rule = Regex::new(r"(?is)\bauto\b[^.]*\bdoes not change posting\b[^.]*\.[^.]*--self")
        .expect("rule pattern");
    let old = Regex::new(r"(?i)auto[^.]*implies[^.]*--self").expect("old pattern");
    let mut failures = Vec::new();

    for spec in rows(Auto::ReportsOnly) {
        let file = load(spec.name);

        // Act
        let text = file.text_of(&file.step0().expect("Step 0"));

        // Assert
        if !rule.is_match(&text) {
            failures.push(format!("{}: Step 0 lacks the auto posting rule", spec.name));
        }
        if old.is_match(&text) {
            failures.push(format!(
                "{}: Step 0 still says auto implies --self",
                spec.name
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn review_posting_section_is_gated_on_not_self_and_holds_every_post_call() {
    // Arrange
    let mut failures = Vec::new();

    for spec in rows(Auto::ReportsOnly) {
        let file = load(spec.name);
        let posting = posting_section(&file);

        // Act
        let gate = file
            .paragraphs(&posting)
            .into_iter()
            .find(|(_, b)| b.contains("SELF_MODE") && b.to_lowercase().contains("stop"));
        let outside: Vec<usize> = (file.body_start..file.lines.len())
            .filter(|i| !(posting.start..posting.end).contains(i))
            .filter(|&i| is_post_call(&file.lines[i]))
            .collect();

        // Assert
        match gate {
            None => failures.push(format!(
                "{}: `{}` has no `SELF_MODE` ... stop gate",
                spec.name, posting.title
            )),
            Some((gate_line, _)) => {
                let reached_first = (posting.start..gate_line)
                    .find(|&i| is_post_call(&file.lines[i]) || is_ask(&file.lines[i]));
                if let Some(i) = reached_first {
                    failures.push(format!(
                        "{}: line {} posts or asks before the `SELF_MODE` gate (line {})",
                        spec.name,
                        i + 1,
                        gate_line + 1
                    ));
                }
            }
        }
        if !outside.is_empty() {
            failures.push(format!(
                "{}: review post calls outside `{}` on lines {:?}",
                spec.name,
                posting.title,
                outside.iter().map(|i| i + 1).collect::<Vec<_>>()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "posting gate problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn review_auto_path_text_never_posts_a_review() {
    // Arrange
    let mut failures = Vec::new();

    for spec in rows(Auto::ReportsOnly) {
        let file = load(spec.name);
        let posting = posting_section(&file);
        let Some(step0) = file.step0() else {
            failures.push(format!(
                "{}: no `Step 0` section, so no auto path to inspect",
                spec.name
            ));
            continue;
        };
        let auto_text: Vec<Section> = [Some(step0), file.auto_path()]
            .into_iter()
            .flatten()
            .collect();

        // Act
        for section in &auto_text {
            let overlaps = section.start < posting.end && posting.start < section.end;
            let posts = (section.start..section.end).any(|i| is_post_call(&file.lines[i]));

            // Assert
            if overlaps {
                failures.push(format!(
                    "{}: `{}` overlaps the posting section",
                    spec.name, section.title
                ));
            }
            if posts {
                failures.push(format!(
                    "{}: `{}` contains a call that posts a review",
                    spec.name, section.title
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "auto path problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn learn_project_step0_stages_in_auto_and_skips_the_write_question() {
    // Arrange
    let file = load("learn-project");
    let step0 = require_section(&file, file.step0(), "`Step 0`");
    let stages = Regex::new(r"(?is)\bauto\b[^.]*--stage").expect("stage pattern");
    let skips = Regex::new(
        r"(?is)\bskips?\b[^.]*(write question|Write these to memory)|(write question|Write these to memory)[^.]*\bskipped\b",
    )
    .expect("skip pattern");

    // Act
    let text = file.text_of(&step0);

    // Assert
    assert!(
        stages.is_match(&text),
        "Step 0 must say auto behaves as `--stage`"
    );
    assert!(
        skips.is_match(&text),
        "Step 0 must say the write question is skipped in that path"
    );
}

#[test]
fn commands_that_never_ask_carry_no_mode_block() {
    // Arrange
    let mut failures = Vec::new();

    for spec in rows(Auto::Unchanged) {
        let file = load(spec.name);

        // Act
        if let Some((line, text)) = file.first_ask_outside(None) {
            failures.push(format!(
                "{}: expected no asks, but line {} matches: {}",
                spec.name,
                line + 1,
                text.trim()
            ));
        }
        if file.step0().is_some() {
            failures.push(format!(
                "{}: has a `Step 0`, but behaves the same in either mode",
                spec.name
            ));
        }
    }

    // Assert
    assert!(
        failures.is_empty(),
        "no-mode-block problems:\n{}",
        failures.join("\n")
    );
}

#[test]
fn review_picks_an_engine_and_takes_its_own_recommendation_in_auto() {
    // Arrange
    let post_words = Regex::new(r"gh pr (review|comment)|--method POST").expect("post words");
    let skip_rule = Regex::new(r"(?is)does not skip[^.]*post question[^.]*--self").expect("rule");
    let implies = Regex::new(r"(?i)auto[^.]*implies[^.]*--self").expect("old pattern");
    let mut failures = Vec::new();

    for spec in rows(Auto::PicksAnEngine) {
        let file = load(spec.name);
        let hint = file
            .frontmatter_value("argument-hint")
            .expect("argument-hint");

        // Act
        let auto_text = file.text_of(&file.auto_path().expect("an `Auto path` section"));
        let step = |n: &str| file.step(n).map(|s| file.text_of(&s)).unwrap_or_default();
        let (step1, step4, step5) = (step("1"), step("4"), step("5"));

        // Assert
        for flag in ["--quick", "--deep", "--auto", "--self"] {
            if !hint.contains(flag) {
                failures.push(format!("{}: argument-hint lacks {flag}", spec.name));
            }
        }
        for stop in [
            "--quick and --deep conflict",
            "--all and --preset belong to the deep review",
        ] {
            if !step1.contains(stop) {
                failures.push(format!("{}: Step 1 lacks the stop `{stop}`", spec.name));
            }
        }
        if !step4.contains("Ask in ask mode only") {
            failures.push(format!(
                "{}: Step 4 must limit the question to ask mode",
                spec.name
            ));
        }
        if !auto_text.contains("take the recommendation without asking")
            || !auto_text.contains("--auto")
        {
            failures.push(format!(
                "{}: the auto path must take the recommendation and pass --auto on",
                spec.name
            ));
        }
        if !skip_rule.is_match(&auto_text) || implies.is_match(&auto_text) {
            failures.push(format!(
                "{}: the auto path must say auto does not skip the post question",
                spec.name
            ));
        }
        let engine = |label: &str| {
            step5
                .lines()
                .find(|l| l.trim_start().starts_with(label))
                .unwrap_or_default()
                .to_owned()
        };
        if !engine("- Quick:").contains("/playbook:quick-review")
            || !engine("- Deep:").contains("/playbook:deep-review")
        {
            failures.push(format!(
                "{}: Step 5 must map quick to quick-review and deep to deep-review",
                spec.name
            ));
        }
        if let Some(line) = file
            .lines
            .iter()
            .find(|l| is_post_call(l) || post_words.is_match(l))
        {
            failures.push(format!("{}: posts to GitHub itself: {line}", spec.name));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
