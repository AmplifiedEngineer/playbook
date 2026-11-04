// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The synthetic memory search fixture, the loader that checks it, and the
//! harness that measures a retrieval path against a committed baseline. The
//! fixture holds invented facts and queries only, never content from a real
//! memory store. These tests build their index in memory and never read the
//! caller's home.
//!
//! To regenerate `baseline-scan.json`, run
//! `UPDATE_MEMORY_SEARCH_BASELINE=1 cargo test -p playbook --test memory_search_quality`.
//! Do that only in the commit that moves hits, and list each changed query in
//! the PR.

#[path = "support/memory_search_fixture.rs"]
mod memory_search_fixture;
#[path = "support/recall_corpus.rs"]
mod recall_corpus;
#[path = "support/recall_harness.rs"]
mod recall_harness;

use memory_search_fixture::{Fixture, FixtureError, Split, MIN_SCORE_QUERIES};
use playbook::hooks::memory_anchors::recall_harness::{CandidateSource, IndexSource, ScanSource};
use playbook::hooks::memory_anchors::PromptQuery;
use recall_corpus::Corpus;
use recall_harness::{ReversedRanker, TOP_K};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Facts in the corpus CI measures: the fixture plus distractors.
const CORPUS_FACTS: usize = 2_000;
const CORPUS_SEED: u64 = 0x5EED_0001;
const GOLDEN_CORPUS_HASH: u64 = 0x6B93_2369_3B2A_FD0B;
/// Queries the scan answers in its first 3 at 50k facts, as measured on this
/// fixture. A drop below it is a regression; the fixture is harder than a real
/// store, so this is not the 15 of 25 a real store reaches.
const LARGE_SCAN_MIN_RECALL_AT_3: usize = 13;
/// Related-fact edges per fact, near the 15 per fact of a real store.
const EDGES_PER_FACT: usize = 15;

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

fn baseline_path() -> PathBuf {
    fixture_dir().join("baseline-scan.json")
}

fn default_corpus(fixture: &Fixture) -> Corpus {
    Corpus::build(fixture, CORPUS_FACTS, CORPUS_SEED)
}

#[test]
fn the_corpus_hash_is_stable() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();

    // Act
    let corpus = default_corpus(&fixture);

    // Assert
    assert_eq!(corpus.hash(), GOLDEN_CORPUS_HASH, "corpus bytes changed");
}

#[test]
fn the_corpus_keeps_every_fixture_fact_and_reaches_its_size() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();

    // Act
    let corpus = default_corpus(&fixture);

    // Assert
    assert_eq!(corpus.facts.len(), CORPUS_FACTS);
    for (id, text) in &fixture.facts {
        assert_eq!(corpus.facts.get(id), Some(text), "{id} was changed");
    }
}

#[test]
fn another_seed_gives_another_corpus() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();

    // Act
    let other = Corpus::build(&fixture, CORPUS_FACTS, CORPUS_SEED + 1);

    // Assert
    assert_ne!(other.hash(), default_corpus(&fixture).hash());
}

#[test]
fn the_scan_matches_its_committed_baseline() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = default_corpus(&fixture);
    let source = recall_harness::scan_source(&corpus);

    // Act
    let run = recall_harness::run(&source, &fixture);
    let (at1, total) = recall_harness::recall_at(&run, &fixture, 1);
    let (at3, _) = recall_harness::recall_at(&run, &fixture, TOP_K);
    println!("scan at {CORPUS_FACTS} facts: recall@1 {at1}/{total}, recall@3 {at3}/{total}");
    if std::env::var_os("UPDATE_MEMORY_SEARCH_BASELINE").is_some() {
        std::fs::write(
            baseline_path(),
            recall_harness::baseline_text(&corpus, &run),
        )
        .unwrap();
    }

    // Assert
    let baseline = std::fs::read_to_string(baseline_path()).unwrap();
    if let Err(diff) = recall_harness::check_against(&baseline, &corpus, &run) {
        panic!("scan results differ from baseline-scan.json:\n{diff}");
    }
}

#[test]
fn a_reversed_ranker_fails_the_harness_with_a_per_query_diff() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = default_corpus(&fixture);
    let baseline = std::fs::read_to_string(baseline_path()).unwrap();
    let broken = ReversedRanker(recall_harness::scan_source(&corpus));

    // Act
    let run = recall_harness::run(&broken, &fixture);
    let verdict = recall_harness::check_against(&baseline, &corpus, &run);

    // Assert
    let diff = verdict.expect_err("a reversed ranking must not pass");
    assert!(diff.contains("baseline"), "diff names no baseline: {diff}");
    assert!(diff.lines().any(|l| l.starts_with('q')), "no query named");
}

#[test]
fn the_scan_recalls_some_but_not_all_fixture_queries() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let source = recall_harness::scan_source(&default_corpus(&fixture));

    // Act
    let run = recall_harness::run(&source, &fixture);

    // Assert
    let (at3, total) = recall_harness::recall_at(&run, &fixture, TOP_K);
    assert!(
        at3 > 0 && at3 < total,
        "recall@3 {at3}/{total} is not plausible"
    );
}

/// Removes a scratch folder when dropped, so a failed run leaves nothing big.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("pb-memory-search-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// Writes `corpus` as fact files under `<root>/memory` and its graph into
/// `<root>/data/memory.db`, with `EDGES_PER_FACT` related-fact edges per fact.
fn write_store(root: &Path, repo: &str, corpus: &Corpus) -> PathBuf {
    let mem_dir = root.join("memory");
    std::fs::create_dir_all(&mem_dir).unwrap();
    let ids: Vec<&String> = corpus.facts.keys().collect();
    let mut nodes = Vec::new();
    for (id, text) in &corpus.facts {
        std::fs::write(mem_dir.join(format!("{id}.md")), text).unwrap();
        let (name, description) = recall_corpus::name_and_description(text);
        nodes.push(serde_json::json!({
            "id": id, "file": format!("{id}.md"), "scope": "project",
            "project": repo, "type": "project", "name": name, "description": description,
        }));
    }
    let mut rng = recall_corpus::SplitMix64::new(CORPUS_SEED);
    let mut edges = Vec::new();
    for from in &ids {
        for _ in 0..EDGES_PER_FACT {
            let to = ids[(rng.next_u64() % ids.len() as u64) as usize];
            edges.push(serde_json::json!({
                "from": from, "to": to, "relation": "possible_relates_to",
                "signals": ["type_scope", "body_overlap"],
            }));
        }
    }
    let graph = serde_json::json!({"nodes": nodes, "edges": edges, "version": 1});
    playbook::hooks::rebuild_memory_graph::import_json(&mem_dir, &graph.to_string()).unwrap();
    mem_dir
}

#[test]
#[ignore = "builds 10k and 50k fact stores; run by hand to measure"]
fn session_start_index_build_at_scale() {
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let repo = "synthetic/store";
    for facts in [10_000, 50_000] {
        // Arrange
        let corpus = Corpus::build(&fixture, facts, CORPUS_SEED);
        let dir = scratch(&format!("session-{facts}"));
        let mem_dir = write_store(&dir.0, repo, &corpus);

        // Act
        let started = Instant::now();
        let index = playbook::hooks::memory_anchors::recall_harness::index_text(&mem_dir, repo);
        let build = started.elapsed();
        let source = ScanSource::new(index.clone());
        let started = Instant::now();
        for q in &fixture.queries {
            let _ = source.candidates(&PromptQuery::new(&q.text), TOP_K);
        }
        let per_query = started.elapsed() / u32::try_from(fixture.queries.len()).unwrap();

        // Assert
        assert_eq!(index.lines().count(), facts, "one row per fact");
        println!("{facts} facts: session index build {build:?}, scan per query {per_query:?}");
    }
}

#[test]
#[ignore = "builds a 50k fact corpus; run by hand to measure"]
fn scan_recall_holds_at_50k_facts() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = Corpus::build(&fixture, 50_000, CORPUS_SEED);
    let source = recall_harness::scan_source(&corpus);

    // Act
    let started = Instant::now();
    let run = recall_harness::run(&source, &fixture);
    let per_query = started.elapsed() / u32::try_from(fixture.queries.len()).unwrap();

    // Assert
    let (at3, total) = recall_harness::recall_at(&run, &fixture, TOP_K);
    println!("scan at 50000 facts: recall@3 {at3}/{total}, {per_query:?} per query");
    assert!(at3 >= LARGE_SCAN_MIN_RECALL_AT_3, "recall@3 {at3}/{total}");
}

#[test]
#[ignore = "reads the developer's own memory store"]
fn the_harness_runs_end_to_end_on_the_own_store() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let index = playbook::hooks::memory_anchors::recall_harness::own_store_index_text();
    let source = ScanSource::new(index.clone());

    // Act
    let run = recall_harness::run(&source, &fixture);

    // Assert: content is not fixed, so this prints and only checks it ran.
    let answered = run.0.values().filter(|ids| !ids.is_empty()).count();
    println!(
        "own store: {} rows, {answered}/{} fixture queries matched something",
        index.lines().count(),
        run.0.len()
    );
}

const STORE_REPO: &str = "synthetic/store";

fn index_source(corpus: &Corpus, root: &Path) -> IndexSource {
    let mem_dir = write_store(root, STORE_REPO, corpus);
    let db = playbook_data::memory::db_in(&mem_dir.parent().unwrap().join("data"));
    IndexSource::new(recall_harness::index_text(corpus), db, STORE_REPO)
}

/// Ids that `source` returned for each query, split into queries it answers
/// (an expected id in the top 3) and the rest.
fn answered(run: &recall_harness::Run, fixture: &Fixture) -> std::collections::BTreeSet<String> {
    fixture
        .queries
        .iter()
        .filter(|q| run.0[&q.id].iter().any(|id| q.expected.contains(id)))
        .map(|q| q.id.clone())
        .collect()
}

#[test]
fn the_index_path_returns_the_scans_ordered_results_when_few_facts_match() {
    // Arrange: only the fixture facts, so no query has more matches than the
    // index returns candidates for.
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = Corpus::build(&fixture, fixture.facts.len(), CORPUS_SEED);
    let dir = scratch("parity");
    let by_index = index_source(&corpus, &dir.0);
    let by_scan = recall_harness::scan_source(&corpus);

    // Act
    let from_index = recall_harness::run(&by_index, &fixture);
    let from_scan = recall_harness::run(&by_scan, &fixture);

    // Assert
    assert_eq!(
        by_index.fallbacks(),
        0,
        "the index path fell back to the scan"
    );
    let differing: Vec<_> = from_scan
        .0
        .iter()
        .filter(|(q, ids)| from_index.0.get(*q) != Some(*ids))
        .map(|(q, ids)| format!("{q}: scan {ids:?}, index {:?}", from_index.0[q]))
        .collect();
    assert!(differing.is_empty(), "{}", differing.join("\n"));
}

#[test]
fn the_index_path_loses_no_query_the_scan_answers_among_many_distractors() {
    // Arrange: with thousands of equal-scoring distractors the two paths may
    // fill the remaining slots differently, so compare which queries each
    // answers rather than the filler.
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = default_corpus(&fixture);
    let dir = scratch("parity-large");
    let by_index = index_source(&corpus, &dir.0);

    // Act
    let from_index = answered(&recall_harness::run(&by_index, &fixture), &fixture);
    let from_scan = answered(
        &recall_harness::run(&recall_harness::scan_source(&corpus), &fixture),
        &fixture,
    );

    // Assert
    let lost: Vec<_> = from_scan.difference(&from_index).collect();
    let won: Vec<_> = from_index.difference(&from_scan).collect();
    println!(
        "index path: {} won {won:?}, {} lost {lost:?}",
        won.len(),
        lost.len()
    );
    assert!(
        lost.is_empty(),
        "queries the scan answers and the index path does not: {lost:?}"
    );
    assert_eq!(
        by_index.fallbacks(),
        0,
        "the index path fell back to the scan"
    );
}

#[test]
fn the_index_path_matches_its_committed_baseline() {
    // Arrange
    let fixture = Fixture::load(&fixture_dir()).unwrap();
    let corpus = default_corpus(&fixture);
    let dir = scratch("baseline-index");
    let source = index_source(&corpus, &dir.0);

    // Act
    let run = recall_harness::run(&source, &fixture);
    if std::env::var_os("UPDATE_MEMORY_SEARCH_BASELINE_INDEX").is_some() {
        let text = recall_harness::baseline_text(&corpus, &run);
        std::fs::write(fixture_dir().join("baseline-index.json"), text).unwrap();
    }

    // Assert
    let baseline = std::fs::read_to_string(fixture_dir().join("baseline-index.json")).unwrap();
    if let Err(diff) = recall_harness::check_against(&baseline, &corpus, &run) {
        panic!("index results differ from baseline-index.json:\n{diff}");
    }
    assert_eq!(
        source.fallbacks(),
        0,
        "the index path fell back to the scan"
    );
}
