// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs the fixture queries through a retrieval path and compares the ordered
//! top results with a committed baseline.

use crate::memory_search_fixture::Fixture;
use crate::recall_corpus::{name_and_description, Corpus};
use playbook::hooks::memory_anchors::recall_harness::{index_row, CandidateSource, ScanSource};
use playbook::hooks::memory_anchors::{PromptQuery, RecallMatch};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// How many facts one prompt may recall, as in the hook.
pub(crate) const TOP_K: usize = 3;

/// The session index text for `corpus`: one unanchored row per fact.
pub(crate) fn index_text(corpus: &Corpus) -> String {
    corpus
        .facts
        .iter()
        .map(|(id, text)| {
            let (name, description) = name_and_description(text);
            index_row(id, &name, &description, &format!("{id}.md"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn scan_source(corpus: &Corpus) -> ScanSource {
    ScanSource::new(index_text(corpus))
}

/// A broken path that ranks worst first. The harness must reject it.
pub(crate) struct ReversedRanker<S>(pub(crate) S);

impl<S: CandidateSource> CandidateSource for ReversedRanker<S> {
    fn candidates(&self, query: &PromptQuery, limit: usize) -> Vec<RecallMatch> {
        let mut all = self.0.candidates(query, usize::MAX);
        all.reverse();
        all.truncate(limit);
        all
    }
}

/// Query id to the ordered ids a path returned.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Run(pub(crate) BTreeMap<String, Vec<String>>);

pub(crate) fn run(source: &impl CandidateSource, fixture: &Fixture) -> Run {
    Run(fixture
        .queries
        .iter()
        .map(|q| {
            let hits = source.candidates(&PromptQuery::new(&q.text), TOP_K);
            let ids = hits.iter().map(|m| m.fact_id().to_string()).collect();
            (q.id.clone(), ids)
        })
        .collect())
}

/// How many queries have an expected id in their first `k` results, and how
/// many queries there are.
pub(crate) fn recall_at(run: &Run, fixture: &Fixture, k: usize) -> (usize, usize) {
    let hit = |q: &&crate::memory_search_fixture::Query| {
        run.0
            .get(&q.id)
            .is_some_and(|ids| ids.iter().take(k).any(|id| q.expected.contains(id)))
    };
    (
        fixture.queries.iter().filter(hit).count(),
        fixture.queries.len(),
    )
}

/// The baseline file text for `run` over `corpus`.
pub(crate) fn baseline_text(corpus: &Corpus, run: &Run) -> String {
    let doc = json!({
        "corpus_facts": corpus.facts.len(),
        "corpus_hash": format!("{:016x}", corpus.hash()),
        "top": run.0,
    });
    let mut text = serde_json::to_string_pretty(&doc).expect("baseline serialises");
    text.push('\n');
    text
}

/// `Ok` when `run` equals the baseline in `baseline_json`, else the per-query
/// difference.
pub(crate) fn check_against(baseline_json: &str, corpus: &Corpus, run: &Run) -> Result<(), String> {
    let doc: Value = serde_json::from_str(baseline_json).map_err(|e| format!("baseline: {e}"))?;
    let expected_hash = format!("{:016x}", corpus.hash());
    if doc["corpus_hash"] != expected_hash {
        return Err(format!(
            "corpus hash is {expected_hash}, baseline was made for {}",
            doc["corpus_hash"]
        ));
    }
    let mut report = String::new();
    let empty = serde_json::Map::new();
    let baseline = doc["top"].as_object().unwrap_or(&empty);
    for (query, got) in &run.0 {
        let want: Vec<String> = baseline
            .get(query)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if &want != got {
            let _ = writeln!(report, "{query}: baseline {want:?}, got {got:?}");
        }
    }
    if report.is_empty() {
        Ok(())
    } else {
        Err(report)
    }
}
