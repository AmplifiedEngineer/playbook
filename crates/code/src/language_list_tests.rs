// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use super::db;
use super::index::{self, Options};
use super::status::status;
use super::testkit::{fx, pinned, Fx};
use std::collections::BTreeMap;

fn languages(f: &Fx) -> BTreeMap<String, usize> {
    let conn = f.conn();
    status(&conn, &f.index, &f.root, &f.worktree)
        .expect("status")
        .by_lang
}

fn stored(f: &Fx) -> Option<String> {
    db::meta_get(&f.conn(), &db::languages_key(&f.worktree))
}

fn counts(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn rust_and_go(tag: &str) -> Fx {
    let f = fx(tag);
    f.write("a.rs", "fn a() {}\n");
    f.write("b.go", "package b\n");
    f
}

#[test]
fn a_repo_with_rust_and_go_files_stores_both_languages() {
    let _g = pinned();
    let f = rust_and_go("ll-both");
    f.run();
    assert_eq!(languages(&f), counts(&[("go", 1), ("rust", 1)]));
    assert_eq!(stored(&f).as_deref(), Some(r#"{"go":1,"rust":1}"#));
}

#[test]
fn a_repo_without_source_files_stores_an_empty_list() {
    let _g = pinned();
    let f = fx("ll-empty");
    f.run();
    assert_eq!(stored(&f).as_deref(), Some("{}"));
    assert!(languages(&f).is_empty());
}

#[test]
fn an_unchanged_index_writes_no_language_row_on_the_second_run() {
    let _g = pinned();
    let f = rust_and_go("ll-second");
    assert!(f.run().languages_written);
    assert!(!f.run().languages_written);
}

#[test]
fn status_reads_the_stored_list_instead_of_recounting() {
    let _g = pinned();
    let f = rust_and_go("ll-stored");
    f.run();
    std::fs::remove_file(f.root.join("b.go")).expect("rm");
    assert_eq!(languages(&f), counts(&[("go", 1), ("rust", 1)]));
}

#[test]
fn deleting_the_last_file_of_a_language_and_reindexing_drops_it() {
    let _g = pinned();
    let f = rust_and_go("ll-drop");
    f.run();
    std::fs::remove_file(f.root.join("b.go")).expect("rm");
    assert!(f.run().languages_written);
    assert_eq!(languages(&f), counts(&[("rust", 1)]));
}

#[test]
fn a_store_from_an_older_build_gains_the_list_on_the_first_run() {
    let _g = pinned();
    let f = rust_and_go("ll-migrate");
    f.run();
    {
        let conn = db::open_index(&f.index, db::OpenMode::Index).expect("open");
        conn.execute("DELETE FROM meta", []).expect("old format");
    }
    assert_eq!(stored(&f), None);
    assert_eq!(languages(&f), counts(&[("go", 1), ("rust", 1)]));
    let report =
        index::run(&f.root, &f.index, &f.worktree, false, &Options::default()).expect("run");
    assert!(report.languages_written);
    assert_eq!(stored(&f).as_deref(), Some(r#"{"go":1,"rust":1}"#));
}
