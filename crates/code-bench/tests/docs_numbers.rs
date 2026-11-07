// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every number on the evaluation page is either a named constant of the
//! harness or a figure of the committed fixture report, and every figure of
//! the report appears on the page. The scheduled run repeats the check
//! against its real report by pointing `CODE_E2E_REPORT` at it.

use code_bench::e2e::report::Report;
use code_bench::e2e::{
    BRIEF_FACTS_TOKENS_OFF, BRIEF_FACTS_TOKENS_ON, COST_CAP_CENTS, LINE_TOLERANCE, MARGIN_BP,
    MIN_TASKS_PER_KIND, REPEATS, TASK_COUNT,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const PAGE: &str = "docs/internals/20-code-index-evaluation.md";
const FIXTURE: &str = "crates/code-bench/data/report-fixture.json";
const REAL_REPORT_ENV: &str = "CODE_E2E_REPORT";
const LINK_OPEN: &str = "](";
const CENTS_PER_DOLLAR: u64 = 100;
const BASIS_POINTS_PER_POINT: i64 = 100;
const INTERVAL_PERCENT: u64 = 95;
const FIXTURE_LABEL: &str = "fixture";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}

/// The numbers in the text, commas removed, link targets skipped.
fn page_numbers(text: &str) -> BTreeSet<String> {
    let mut cleaned = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(LINK_OPEN) {
        cleaned.push_str(&rest[..at]);
        let after = &rest[at + LINK_OPEN.len()..];
        rest = after.split_once(')').map_or("", |(_, tail)| tail);
    }
    cleaned.push_str(rest);
    let chars: Vec<char> = cleaned.chars().collect();
    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut number = String::new();
        while i < chars.len() {
            let joins = matches!(chars[i], ',' | '.')
                && number.chars().last().is_some_and(|c| c.is_ascii_digit())
                && chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if chars[i].is_ascii_digit() || joins {
                if chars[i] != ',' {
                    number.push(chars[i]);
                }
                i += 1;
            } else {
                break;
            }
        }
        found.insert(number);
    }
    found
}

fn figures(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Number(n) => {
            out.insert(n.to_string().trim_start_matches('-').to_string());
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| figures(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| figures(v, out)),
        _ => {}
    }
}

fn constants() -> BTreeSet<String> {
    [
        MARGIN_BP.to_string(),
        format!("{}.0", MARGIN_BP / BASIS_POINTS_PER_POINT),
        MARGIN_BP.abs().to_string(),
        REPEATS.to_string(),
        COST_CAP_CENTS.to_string(),
        (COST_CAP_CENTS / CENTS_PER_DOLLAR).to_string(),
        LINE_TOLERANCE.to_string(),
        MIN_TASKS_PER_KIND.to_string(),
        TASK_COUNT.to_string(),
        BRIEF_FACTS_TOKENS_ON.to_string(),
        BRIEF_FACTS_TOKENS_OFF.to_string(),
        INTERVAL_PERCENT.to_string(),
    ]
    .into()
}

/// Every difference between the page and the report.
fn differences(page: &str, report: &Report) -> Vec<String> {
    let on_page = page_numbers(page);
    let mut report_figures = BTreeSet::new();
    figures(&serde_json::to_value(report).unwrap(), &mut report_figures);
    let allowed: BTreeSet<String> = report_figures.union(&constants()).cloned().collect();
    let mut out: Vec<String> = on_page
        .difference(&allowed)
        .map(|n| format!("the page has {n}, which is not in the report"))
        .collect();
    out.extend(
        report_figures
            .difference(&on_page)
            .map(|n| format!("the report has {n}, which is not on the page")),
    );
    out
}

fn fixture() -> Report {
    serde_json::from_str(&read(FIXTURE)).unwrap()
}

#[test]
fn the_fixture_is_labelled_as_a_fixture() {
    assert!(fixture().source.starts_with(FIXTURE_LABEL));
}

#[test]
fn every_number_on_the_page_matches_the_fixture_report() {
    assert_eq!(differences(&read(PAGE), &fixture()), Vec::<String>::new());
}

#[test]
fn a_stale_number_on_the_page_fails() {
    let stale = read(PAGE).replacen("8000", "8100", 1);
    assert_eq!(differences(&stale, &fixture()).len(), 2);
}

#[test]
fn a_figure_missing_from_the_page_fails() {
    let missing = read(PAGE).replace("6100", "many");
    assert_eq!(differences(&missing, &fixture()).len(), 1);
}

#[test]
fn the_real_report_matches_the_page_when_a_run_supplies_one() {
    let Ok(path) = std::env::var(REAL_REPORT_ENV) else {
        return;
    };
    let real: Report = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(differences(&read(PAGE), &real), Vec::<String>::new());
}
