// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The synthetic memory search fixture and the loader that checks it. The
//! fixture holds invented facts and queries only, never content from a real
//! memory store.

#[path = "support/memory_search_fixture.rs"]
mod memory_search_fixture;

use memory_search_fixture::{Fixture, FixtureError, Split, MIN_SCORE_QUERIES};
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/memory_search")
}

fn fact(id: &str) -> (String, String) {
    (
        id.to_string(),
        format!("---\nname: {id}\ndescription: d\n---\n\nbody\n"),
    )
}

/// A query list with `score` score queries, all expecting `alpha`.
fn queries_with_score(score: usize) -> String {
    let mut text = String::from("version\t1\n");
    for n in 0..score {
        text.push_str(&format!("s{n}\tscore\talpha\tsome words\n"));
    }
    text
}

fn parse(facts: &[(String, String)], queries: &str) -> Result<Fixture, FixtureError> {
    Fixture::parse(facts, queries)
}

#[test]
fn an_expected_id_with_no_fact_is_refused_by_name() {
    let mut queries = queries_with_score(MIN_SCORE_QUERIES);
    queries.push_str("q1\ttune\tghost\ttext here\n");
    let err = parse(&[fact("alpha")], &queries).unwrap_err();
    assert_eq!(
        err,
        FixtureError::UnknownExpectedId {
            query: "q1".to_string(),
            id: "ghost".to_string()
        }
    );
    assert!(err.to_string().contains("ghost"));
}

#[test]
fn any_one_of_several_expected_ids_is_accepted_when_all_exist() {
    let mut queries = queries_with_score(MIN_SCORE_QUERIES);
    queries.push_str("q1\ttune\talpha,beta\ttext here\n");
    let fixture = parse(&[fact("alpha"), fact("beta")], &queries).unwrap();
    let last = fixture.queries.last().unwrap();
    assert_eq!(last.expected, ["alpha", "beta"]);
    assert_eq!(last.split, Split::Tune);
}

#[test]
fn a_carriage_return_in_a_fact_is_refused() {
    let crlf = (
        "alpha".to_string(),
        "---\r\nname: alpha\r\n---\r\n".to_string(),
    );
    let err = parse(&[crlf], &queries_with_score(MIN_SCORE_QUERIES)).unwrap_err();
    assert_eq!(
        err,
        FixtureError::Crlf {
            file: "alpha.md".to_string()
        }
    );
}

#[test]
fn a_carriage_return_in_the_query_list_is_refused() {
    let queries = queries_with_score(MIN_SCORE_QUERIES).replace('\n', "\r\n");
    let err = parse(&[fact("alpha")], &queries).unwrap_err();
    assert!(matches!(err, FixtureError::Crlf { .. }));
}

#[test]
fn text_outside_ascii_is_refused_so_normal_form_cannot_vary() {
    // "e" followed by a combining acute accent is not in NFC.
    let decomposed = ("alpha".to_string(), "caf\u{65}\u{301}".to_string());
    let err = parse(&[decomposed], &queries_with_score(MIN_SCORE_QUERIES)).unwrap_err();
    assert!(matches!(err, FixtureError::NonAscii { .. }));
}

#[test]
fn a_query_in_neither_partition_is_refused() {
    let mut queries = queries_with_score(MIN_SCORE_QUERIES);
    queries.push_str("q1\tholdout\talpha\ttext here\n");
    let err = parse(&[fact("alpha")], &queries).unwrap_err();
    assert_eq!(
        err,
        FixtureError::UnknownSplit {
            query: "q1".to_string(),
            split: "holdout".to_string()
        }
    );
}

#[test]
fn a_missing_or_unsupported_version_is_refused() {
    let facts = [fact("alpha")];
    let body = queries_with_score(MIN_SCORE_QUERIES);
    let no_version = body.replacen("version\t1\n", "", 1);
    assert!(matches!(
        parse(&facts, &no_version).unwrap_err(),
        FixtureError::BadVersion(_)
    ));
    let future = body.replacen("version\t1", "version\t2", 1);
    assert!(matches!(
        parse(&facts, &future).unwrap_err(),
        FixtureError::BadVersion(_)
    ));
}

#[test]
fn a_malformed_or_duplicate_query_is_refused() {
    let facts = [fact("alpha")];
    let mut short = queries_with_score(MIN_SCORE_QUERIES);
    short.push_str("q1\ttune\talpha\n");
    assert!(matches!(
        parse(&facts, &short).unwrap_err(),
        FixtureError::BadQueryLine { .. }
    ));
    let mut dup = queries_with_score(MIN_SCORE_QUERIES);
    dup.push_str("s0\tscore\talpha\tother words\n");
    assert_eq!(
        parse(&facts, &dup).unwrap_err(),
        FixtureError::DuplicateQuery("s0".to_string())
    );
}

#[test]
fn a_score_partition_below_the_minimum_is_refused() {
    let err = parse(&[fact("alpha")], &queries_with_score(MIN_SCORE_QUERIES - 1)).unwrap_err();
    assert_eq!(
        err,
        FixtureError::ScoreTooSmall {
            found: MIN_SCORE_QUERIES - 1
        }
    );
}

#[test]
fn the_committed_fixture_loads_with_both_partitions() {
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let count = |s: Split| fixture.queries.iter().filter(|q| q.split == s).count();
    assert!(count(Split::Score) >= MIN_SCORE_QUERIES);
    assert!(count(Split::Tune) > 0);
    assert!(fixture.facts.len() >= fixture.queries.len());
}

#[test]
fn the_committed_fixture_names_no_private_content() {
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    for (id, text) in &fixture.facts {
        assert!(
            home.is_empty() || !text.contains(&home),
            "{id} contains the home folder path"
        );
        assert!(!text.contains("/Users/"), "{id} contains a user path");
    }
}

#[test]
fn the_committed_fixture_files_are_marked_binary_for_git() {
    let attrs = std::fs::read_to_string(fixture_dir().join(".gitattributes")).unwrap();
    assert!(attrs.lines().any(|l| l.trim() == "* binary"));
}
