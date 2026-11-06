// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins what the memory.db write path leaves on disk: the SQL of every schema
//! object and the journal mode. A change of engine must keep this file as is.
//! Set `UPDATE_GOLDEN=1` to rewrite it after an intended schema change.

use std::path::PathBuf;

use playbook_data::memory;
use playbook_store::Connection;

const GOLDEN: &str = "tests/golden/memory-open.txt";

fn capture() -> String {
    let dir = std::env::temp_dir().join(format!("playbook-data-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = dir.join("memory.db");
    memory::write_graph(&db, &[], &[]).expect("write empty graph");

    let conn = Connection::open(&db).expect("open");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .expect("prepare");
    let objects = stmt
        .query_map([], |r| {
            Ok(format!(
                "-- {} {}\n{}\n",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    drop(stmt);
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
    format!("PRAGMA journal_mode = {mode}\n{}", objects.join(""))
}

#[test]
fn memory_db_schema_and_pragmas_match_the_golden_file() {
    // Arrange
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);

    // Act
    let got = capture();

    // Assert
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write golden");
    }
    let want = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(got, want);
}
