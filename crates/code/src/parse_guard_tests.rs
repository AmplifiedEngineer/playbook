// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use super::index::Options;
use super::lang::{self, Driver, Lang, MAX_NESTING};
use super::status::full_check;
use super::testkit::{fx, pinned, Fx};
use super::walker::Limits;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SMALL_CAP: u64 = 100;
const TEST_BUDGET: u64 = 5;

fn full_row(f: &Fx) -> (String, String, i64, i64) {
    f.conn()
        .query_row("SELECT status, error, size, mtime_ns FROM files", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .expect("row")
}

fn symbols(f: &Fx, cond: &str) -> i64 {
    let sql =
        format!("SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id WHERE {cond}");
    f.conn().query_row(&sql, [], |r| r.get(0)).expect("count")
}

fn row(f: &Fx, path: &str) -> Option<(String, Option<String>)> {
    f.conn()
        .query_row(
            "SELECT status, error FROM files WHERE path = ?1",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

fn counting(counter: &Arc<AtomicUsize>, spend: u64) -> Driver {
    let counter = Arc::clone(counter);
    Box::new(move |language, bytes, tick| {
        counter.fetch_add(1, Ordering::SeqCst);
        for _ in 0..spend {
            if tick().is_break() {
                return Err("stopped".to_string());
            }
        }
        lang::drive(language, bytes, tick)
    })
}

fn nested(levels: usize) -> String {
    format!(
        "fn f() {{ let x = {}1{}; }}\n",
        "(".repeat(levels),
        ")".repeat(levels)
    )
}

fn depth_of(text: &str) -> usize {
    let parsed = lang::parse(Lang::Rust, text.as_bytes()).expect("parse");
    lang::tree_depth(&parsed.tree)
}

fn nested_with_depth(depth: usize) -> String {
    let base = depth_of(&nested(0));
    nested(depth - base)
}

fn small_cap() -> Options {
    Options {
        limits: Limits {
            max_file_bytes: SMALL_CAP,
            ..Limits::default()
        },
        ..Options::default()
    }
}

fn padded(size: u64) -> String {
    format!("//{}", "x".repeat(usize::try_from(size).expect("size") - 2))
}

fn cap_row(tag: &str, size: u64) -> (String, Option<String>) {
    let f = fx(tag);
    f.write("a.rs", &padded(size));
    f.run_with(&small_cap());
    row(&f, "a.rs").expect("row")
}

#[test]
fn the_size_cap_indexes_minus_one_and_exact_and_skips_plus_one() {
    let _g = pinned();
    for (tag, size, want) in [
        ("pg-cap-under", SMALL_CAP - 1, ("ok", None)),
        ("pg-cap-at", SMALL_CAP, ("ok", None)),
        ("pg-cap-over", SMALL_CAP + 1, ("skipped", Some("too_large"))),
    ] {
        let got = cap_row(tag, size);
        assert_eq!((got.0.as_str(), got.1.as_deref()), want, "{tag}");
    }
}

#[test]
fn a_binary_file_and_a_dangling_symlink_keep_their_existing_reasons() {
    let _g = pinned();
    let f = fx("pg-existing-reasons");
    super::walker::test_repo::write(&f.root, "a.dat", b"a\0b");
    #[cfg(unix)]
    std::os::unix::fs::symlink("missing-target", f.root.join("link.rs")).expect("symlink");
    f.run();
    let reason = |path| row(&f, path).map(|r| r.1);
    assert_eq!(reason("a.dat"), Some(Some("binary".to_string())));
    #[cfg(unix)]
    assert_eq!(reason("link.rs"), Some(Some("symlink".to_string())));
}

#[test]
fn a_zero_byte_file_and_a_non_utf8_file_stay_indexed() {
    let _g = pinned();
    let f = fx("pg-edge-content");
    f.write("empty.rs", "");
    super::walker::test_repo::write(
        &f.root,
        "lossy.rs",
        b"fn caf\xe9() {}\nfn known_symbol() {}\n",
    );
    f.run();
    assert_eq!(row(&f, "empty.rs"), Some(("ok".to_string(), None)));
    assert_eq!(symbols(&f, "f.path = 'empty.rs'"), 0);
    assert_eq!(row(&f, "lossy.rs"), Some(("partial".to_string(), None)));
    assert_eq!(symbols(&f, "s.name = 'known_symbol'"), 1);
}

#[test]
fn nesting_below_and_at_the_cap_is_indexed_and_one_more_is_too_deep() {
    let _g = pinned();
    let f = fx("pg-depth");
    for (name, depth) in [
        ("under.rs", MAX_NESTING - 1),
        ("at.rs", MAX_NESTING),
        ("over.rs", MAX_NESTING + 1),
    ] {
        f.write(name, &nested_with_depth(depth));
    }
    assert_eq!(depth_of(&nested_with_depth(MAX_NESTING)), MAX_NESTING);
    f.run();
    for name in ["under.rs", "at.rs"] {
        assert_eq!(row(&f, name).expect("row").0, "ok");
    }
    let (status, error) = row(&f, "over.rs").expect("row");
    assert_eq!(status, "skipped");
    assert!(error.expect("error").starts_with("too_deep@"));
}

#[test]
fn a_too_deep_file_is_not_parsed_again_on_the_second_run() {
    let _g = pinned();
    let f = fx("pg-twice");
    f.write("deep.rs", &nested_with_depth(MAX_NESTING + 1));
    let counter = Arc::new(AtomicUsize::new(0));
    let opts = || Options {
        driver: counting(&counter, 0),
        ..Options::default()
    };
    f.run_with(&opts());
    let report = f.run_with(&opts());
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(report.skipped, 1);
}

#[test]
fn the_rows_of_two_runs_over_a_too_deep_file_are_equal() {
    let _g = pinned();
    let f = fx("pg-rows");
    f.write("deep.rs", &nested_with_depth(MAX_NESTING + 1));
    f.run();
    let first = full_row(&f);
    f.run();
    assert_eq!(first, full_row(&f));
}

#[test]
fn a_changed_parser_string_or_tick_budget_parses_a_guard_skipped_file_again() {
    let _g = pinned();
    for (tag, changed) in [
        (
            "pg-parser-change",
            Options {
                parser: "another-parser".to_string(),
                ..Options::default()
            },
        ),
        (
            "pg-budget-change",
            Options {
                tick_budget: TEST_BUDGET,
                ..Options::default()
            },
        ),
    ] {
        let f = fx(tag);
        f.write("deep.rs", &nested_with_depth(MAX_NESTING + 1));
        let counter = Arc::new(AtomicUsize::new(0));
        let driver = || counting(&counter, 0);
        f.run_with(&Options {
            driver: driver(),
            ..Options::default()
        });
        f.run_with(&Options {
            driver: driver(),
            ..changed
        });
        assert_eq!(counter.load(Ordering::SeqCst), 2, "{tag}");
    }
}

#[test]
fn status_reports_no_drift_for_a_too_deep_file_and_a_change_after_an_edit() {
    let _g = pinned();
    let f = fx("pg-status");
    f.write("deep.rs", &nested_with_depth(MAX_NESTING + 1));
    f.run();
    let quiet = full_check(&f.conn(), &f.root).expect("full");
    assert!(quiet.is_empty(), "{quiet:?}");

    f.write("deep.rs", &nested_with_depth(MAX_NESTING + 2));
    let edited = full_check(&f.conn(), &f.root).expect("full");
    assert_eq!(edited.changed, vec!["deep.rs"]);
}

#[test]
fn the_tick_budget_passes_minus_one_and_exact_and_skips_plus_one() {
    let _g = pinned();
    for (tag, spend, want) in [
        ("pg-ticks-under", TEST_BUDGET - 1, "ok"),
        ("pg-ticks-at", TEST_BUDGET, "ok"),
        ("pg-ticks-over", TEST_BUDGET + 1, "skipped"),
    ] {
        let f = fx(tag);
        f.write("a.rs", "fn spender() {}\n");
        let counter = Arc::new(AtomicUsize::new(0));
        f.run_with(&Options {
            driver: counting(&counter, spend),
            tick_budget: TEST_BUDGET,
            ..Options::default()
        });
        let (status, error) = row(&f, "a.rs").expect("row");
        assert_eq!(status, want, "{tag}");
        if want == "skipped" {
            assert!(error.expect("error").starts_with("parse_timeout@"));
        }
    }
}
