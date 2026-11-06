// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Checks the index against hand-labeled fixture repos. Every label in a
//! fixture's `labels` file must hold. Every entry in its `xfail.toml` is a
//! known gap and is asserted as absent, so the test fails once the extractor
//! starts to produce the symbol and the entry has to go.

mod common;

use common::{fixture_dir, index_fixture, Indexed};
use playbook_code::refs::{find_refs, RefsOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

/// Each label kind needs this many entries per fixture, for the later gate.
const MIN_LABELS_PER_KIND: usize = 3;
const REF_LIMIT: usize = 1000;

fn ref_lines(indexed: &Indexed, name: &str) -> BTreeSet<(String, usize)> {
    let opts = RefsOptions {
        limit: REF_LIMIT,
        ..RefsOptions::new(name)
    };
    let found = find_refs(&indexed.conn, &indexed.root, &opts).expect("refs");
    found.refs.into_iter().map(|r| (r.path, r.line)).collect()
}

fn check_labels(lang: &str) {
    let text = fs::read_to_string(fixture_dir(lang).join("labels")).expect("labels file");
    let indexed = index_fixture(lang);
    let rows = indexed.symbols();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        *counts.entry(parts[0].to_string()).or_default() += 1;
        match parts.as_slice() {
            ["symbol", path, qualified, kind, start] => {
                let start: i64 = start.parse().expect("start line");
                let found = rows.iter().any(|r| {
                    (&r.path[..], &r.qualified[..], &r.kind[..], r.start_line)
                        == (*path, *qualified, *kind, start)
                });
                assert!(found, "{lang}: missing symbol {line}");
            }
            [label @ ("ref" | "negative"), name, path, row] => {
                let at = (path.to_string(), row.parse().expect("line"));
                let present = ref_lines(&indexed, name).contains(&at);
                assert_eq!(present, *label == "ref", "{lang}: wrong {line}");
            }
            _ => panic!("bad label line in {lang}: {line}"),
        }
    }
    for kind in ["symbol", "ref", "negative"] {
        let count = counts.get(kind).copied().unwrap_or(0);
        assert!(
            count >= MIN_LABELS_PER_KIND,
            "{lang}: too few {kind} labels"
        );
    }
}

/// Each `["path"]` table of `xfail.toml` maps a symbol name to the reason it
/// is missing.
fn check_gaps(lang: &str) {
    let text = fs::read_to_string(fixture_dir(lang).join("xfail.toml")).expect("xfail file");
    let rows = index_fixture(lang).symbols();
    let mut path = "";
    let mut seen = 0;
    for line in text.lines().filter(|l| !l.is_empty()) {
        if let Some(header) = line.strip_prefix("[\"").and_then(|h| h.strip_suffix("\"]")) {
            path = header;
        } else if let Some((name, _reason)) = line.split_once(" = ") {
            seen += 1;
            let present = rows.iter().any(|r| r.path == path && r.name == name);
            assert!(
                !present,
                "{lang}: {path} {name} is extracted now, remove its xfail entry"
            );
        }
    }
    assert!(seen > 0, "{lang} lists no known gaps");
}

macro_rules! per_language {
    ($($lang:ident),*) => {$(
        mod $lang {
            #[test]
            fn labels_hold() {
                super::check_labels(stringify!($lang));
            }

            #[test]
            fn known_gaps_stay_absent() {
                super::check_gaps(stringify!($lang));
            }
        }
    )*};
}

per_language!(rust, typescript, javascript, python, go);
