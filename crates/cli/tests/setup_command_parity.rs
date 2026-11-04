// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `/playbook:setup` slash command takes the same flags as
//! `playbook setup` and asks the same questions, in the same order and with
//! the same defaults. `plugins/playbook/commands/setup.md` holds both as
//! tables; these tests compare them with the help the binary prints and with
//! the part catalog its wizard is built from.

use clap::CommandFactory;
use playbook::init::{catalog, choices, telemetry, wizard};
use playbook::Cli;
use std::fs;
use std::path::PathBuf;

fn setup_md() -> String {
    let path = PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("plugins/playbook/commands/setup.md");
    fs::read_to_string(path).expect("setup.md")
}

/// The cells of each row of the table under `## <heading>`, header and
/// divider rows left out.
fn table(md: &str, heading: &str) -> Vec<Vec<String>> {
    let start = md
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("setup.md has no `## {heading}` section"));
    md[start..]
        .lines()
        // The empty line before the heading, then the heading itself.
        .skip(2)
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        // The header row and the divider row.
        .skip(2)
        .map(|l| {
            l.trim()
                .trim_matches('|')
                .split(" | ")
                .map(|c| c.trim().to_string())
                .collect()
        })
        .collect()
}

/// `(names, text)` for each option in `playbook setup --help`, in order.
/// The names cell is written the way the table writes it: each name in
/// backticks, then any `[alias: ...]` names.
fn help_rows() -> Vec<(String, String)> {
    let mut cmd = Cli::command();
    let help = cmd
        .find_subcommand_mut("setup")
        .expect("setup subcommand")
        .render_long_help()
        .to_string();
    let options = help
        .split("\nOptions:\n")
        .nth(1)
        .expect("an Options section");
    let mut rows: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for line in options.lines() {
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if indent < 8 && text.starts_with('-') {
            let names = text.split(", ").map(|n| format!("`{n}`")).collect();
            rows.push((names, Vec::new()));
        } else if let Some(alias) = text
            .strip_prefix("[alias: ")
            .and_then(|a| a.strip_suffix(']'))
        {
            let row = rows.last_mut().expect("an alias follows a flag");
            row.0.extend(alias.split(", ").map(|n| format!("`{n}`")));
        } else {
            rows.last_mut()
                .expect("text follows a flag")
                .1
                .push(text.to_string());
        }
    }
    rows.into_iter()
        .map(|(names, text)| (names.join(", "), text.join(" ")))
        .collect()
}

#[test]
fn the_flag_table_matches_playbook_setup_help() {
    // Arrange
    let expected = help_rows();
    assert!(expected.len() > 20, "too few options parsed: {expected:?}");

    // Act
    let actual: Vec<(String, String)> = table(&setup_md(), "Flags")
        .into_iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect();

    // Assert
    assert_eq!(
        actual, expected,
        "the Flags table in setup.md must list `playbook setup --help` row by row"
    );
}

#[test]
fn the_argument_hint_names_every_flag() {
    let md = setup_md();
    let hint: Vec<&str> = md
        .lines()
        .find_map(|l| l.strip_prefix("argument-hint: "))
        .expect("argument-hint")
        .split(|c: char| "[]|\" ".contains(c))
        .filter(|t| !t.is_empty())
        .collect();
    for (names, _) in help_rows() {
        let long = names
            .split(", ")
            .map(|n| n.trim_matches('`'))
            .find(|n| n.starts_with("--"))
            .expect("a long name");
        let flag = long.split(' ').next().unwrap_or(long);
        if flag != "--defaults" {
            assert!(hint.contains(&flag), "argument-hint lacks {flag}");
        }
    }
}

#[test]
fn the_question_table_matches_the_wizard() {
    // Arrange
    let asked: Vec<&catalog::Part> = catalog::PARTS.iter().filter(|p| p.asked).collect();

    // Act
    let rows = table(&setup_md(), "Questions");

    // Assert
    assert_eq!(rows.len(), asked.len(), "one row per asked part");
    for (n, (row, part)) in rows.iter().zip(&asked).enumerate() {
        let id = part.id;
        assert_eq!(row[0], (n + 1).to_string(), "{id}: order");
        assert_eq!(row[1], format!("`{id}`"), "row {}: part", n + 1);
        assert!(
            row[2].chars().count() <= 12,
            "{id}: header too long for AskUserQuestion"
        );
        assert_eq!(row[3], part.question, "{id}: question");
        let default = if part.default_on { "Yes" } else { "No" };
        assert_eq!(row[4], default, "{id}: default");
        assert_eq!(row[5], wizard::decline_line(id), "{id}: the line for a no");
    }
}

/// The starts of the lines that skip the telemetry question. Older binaries
/// word the rest of the line differently, so the command matches on these.
const SKIP_PREFIXES: [&str; 2] = ["telemetry: already", "telemetry: switched off"];

#[test]
fn the_command_spots_every_telemetry_skip_line() {
    let md = setup_md();
    for prefix in SKIP_PREFIXES {
        assert!(
            md.contains(&format!("`{prefix}`")),
            "setup.md must name `{prefix}`"
        );
    }
    for line in [
        telemetry::ENABLED_LINE,
        telemetry::SET_UP_LINE,
        telemetry::OFF_ELSEWHERE_LINE,
    ] {
        assert!(
            SKIP_PREFIXES.iter().any(|p| line.starts_with(p)),
            "no prefix in setup.md matches: {line}"
        );
    }
}

#[test]
fn the_command_quotes_the_welcome() {
    assert!(
        setup_md().contains(wizard::WELCOME),
        "setup.md must quote the welcome text"
    );
}

#[test]
fn the_only_help_lists_every_part_id() {
    // Arrange
    let mut cmd = Cli::command();
    let only = cmd
        .find_subcommand_mut("setup")
        .expect("setup subcommand")
        .get_arguments()
        .find(|a| a.get_id() == "only")
        .expect("the --only argument")
        .get_help()
        .expect("help on --only")
        .to_string();

    // Act
    // A part with no flag runs on every setup, so `--only` never names it.
    let missing: Vec<&str> = catalog::PARTS
        .iter()
        .filter(|p| !p.flags.is_empty() && !only.contains(p.id))
        .map(|p| p.id)
        .collect();

    // Assert
    assert!(missing.is_empty(), "--only help lacks {missing:?}");
}

/// The start of each Step 2 rule that drops retired words.
const RETIRED_RULE: &str = "- Drop the retired words ";

#[test]
fn the_command_drops_every_retired_part() {
    let rules: String = setup_md()
        .lines()
        .filter(|l| l.starts_with(RETIRED_RULE))
        .collect();
    for part in choices::RETIRED_PARTS {
        for word in [
            format!("`--{part}`"),
            format!("`--no-{part}`"),
            format!("`{part}`"),
        ] {
            assert!(
                rules.contains(&word),
                "setup.md must drop the retired word {word}"
            );
        }
    }
}
