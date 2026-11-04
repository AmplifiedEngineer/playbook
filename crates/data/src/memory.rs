// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `memory.db`: the memory graph, a derived index of the Markdown fact files.
//! The Markdown files stay the source of truth; any writer can rebuild this
//! database from them at any time, so it is safe to delete.
//!
//! `nodes` mirrors the node objects of `memory.graph.json` and `edges` its
//! edges. `project`, `name` and `description` are optional there (a `code:`
//! anchor node has none), `pinned` is 0 or 1, and `signals` is the list of
//! similarity signals of a `possible_relates_to` edge, comma separated.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The schema every writer applies.
pub const SCHEMA_SQL: &str = "\
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY,
    file TEXT NOT NULL,
    scope TEXT NOT NULL,
    project TEXT,
    type TEXT NOT NULL,
    name TEXT,
    description TEXT,
    pinned INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS edges (
    src TEXT NOT NULL,
    dst TEXT NOT NULL,
    relation TEXT NOT NULL,
    signals TEXT,
    PRIMARY KEY (src, dst, relation)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS edges_by_dst ON edges (dst);
CREATE INDEX IF NOT EXISTS nodes_by_scope ON nodes (scope, project);
CREATE TABLE IF NOT EXISTS facts_fts_map (
    rowid INTEGER PRIMARY KEY,
    fact_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    project TEXT
);
CREATE INDEX IF NOT EXISTS facts_fts_map_by_scope ON facts_fts_map (scope, project);";

/// The full text index over fact text. It is contentless, so the text itself
/// is not stored and `facts_fts_map` maps a row back to its fact. It is not
/// part of [`SCHEMA_SQL`] because a build without FTS5 must still be able to
/// write the graph. The tokenizer matches the recall matcher, which splits
/// words on every non-alphanumeric character.
pub const FACTS_FTS_SQL: &str = "\
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    name,
    description,
    body,
    content='',
    contentless_delete=1,
    tokenize='unicode61'
);";

/// Whether the SQLite build can create FTS5 tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fts5Support {
    Available,
    Unavailable,
}

impl Fts5Support {
    /// What the SQLite behind `conn` was compiled with.
    pub fn probe(conn: &Connection) -> Self {
        let Ok(mut stmt) = conn.prepare("PRAGMA compile_options") else {
            return Self::Unavailable;
        };
        let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
            return Self::Unavailable;
        };
        if rows.flatten().any(|opt| opt == "ENABLE_FTS5") {
            Self::Available
        } else {
            Self::Unavailable
        }
    }
}

/// The `scope` of a node that stands for a code location (an `anchors:`
/// target). Every other node is a fact.
pub const CODE_SCOPE: &str = "code";

/// The `scope` values of fact nodes, widest first.
pub const FACT_SCOPES: [&str; 3] = ["global", "org", "project"];

/// The schema version stored in `meta` under `schema_version`.
pub const SCHEMA_VERSION: &str = "2";

const BUSY: Duration = Duration::from_millis(5000);

/// `<data>/memory.db`.
pub fn db_in(data: &Path) -> PathBuf {
    data.join("memory.db")
}

/// The nodes and edges of a graph.
pub type Graph = (Vec<NodeRow>, Vec<EdgeRow>);

/// One node of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub id: String,
    pub file: String,
    pub scope: String,
    pub project: Option<String>,
    pub kind: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub pinned: bool,
}

/// One edge of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeRow {
    pub from: String,
    pub to: String,
    pub relation: String,
    pub signals: Option<Vec<String>>,
}

/// Applies [`SCHEMA_SQL`], creates the fact index when the build has FTS5, and
/// records the schema version. Fails when the file was written by a newer
/// schema (rebuild it with the newer playbook).
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    init_with(conn, Fts5Support::probe(conn))
}

/// [`init`] with the FTS5 support given, so a test can make a build that has
/// FTS5 behave as one that has not.
pub fn init_with(conn: &Connection, fts5: Fts5Support) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY)?;
    conn.execute_batch(SCHEMA_SQL)?;
    if fts5 == Fts5Support::Available {
        // The index is derived and readers fall back without it, so a table
        // that cannot be made must not stop the graph from being written.
        let _ = conn.execute_batch(FACTS_FTS_SQL);
    }
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION],
    )?;
    let found: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )?;
    match (found.parse::<u32>(), SCHEMA_VERSION.parse::<u32>()) {
        (Ok(f), Ok(ours)) if f > ours => Err(rusqlite::Error::InvalidParameterName(format!(
            "memory.db has schema version {f}, this build understands {ours}; upgrade"
        ))),
        (Ok(f), Ok(ours)) if f < ours => conn
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                [SCHEMA_VERSION],
            )
            .map(|_| ()),
        _ => Ok(()),
    }
}

/// Whether `conn` holds a usable `facts_fts` virtual table.
pub fn facts_index_present(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'facts_fts' \
         AND sql LIKE 'CREATE VIRTUAL TABLE%'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// Replaces the whole graph in the database at `db` in one transaction, so a
/// reader sees the old graph or the new one, never a mix. Creates the file
/// and its folder (owner only) when missing.
pub fn write_graph(db: &Path, nodes: &[NodeRow], edges: &[EdgeRow]) -> Result<(), String> {
    if let Some(dir) = db.parent() {
        playbook_common::atomic::ensure_private_dir(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    // Owner-only before SQLite can create it with the default umask.
    playbook_common::atomic::make_db_file_private(db)?;
    let mut conn = Connection::open(db).map_err(|e| e.to_string())?;
    conn.busy_timeout(BUSY).map_err(|e| e.to_string())?;
    playbook_common::sqlite::retry_busy(
        || conn.pragma_update(None, "journal_mode", "WAL"),
        playbook_common::sqlite::OPEN_BUDGET,
    )
    .map_err(|e| e.to_string())?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    // The schema is made inside the same transaction as the rows, so a reader
    // sees no tables at all or the whole first graph, never empty tables.
    init(&tx).map_err(|e| e.to_string())?;
    tx.execute_batch("DELETE FROM edges; DELETE FROM nodes;")
        .map_err(|e| e.to_string())?;
    {
        let mut n = tx
            .prepare(
                "INSERT OR REPLACE INTO nodes (id, file, scope, project, type, name, description, pinned) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        for r in nodes {
            n.execute(rusqlite::params![
                r.id,
                r.file,
                r.scope,
                r.project,
                r.kind,
                r.name,
                r.description,
                r.pinned
            ])
            .map_err(|e| e.to_string())?;
        }
        let mut e = tx
            .prepare(
                "INSERT OR REPLACE INTO edges (src, dst, relation, signals) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|e| e.to_string())?;
        for r in edges {
            let signals = r.signals.as_ref().map(|s| s.join(","));
            e.execute(rusqlite::params![r.from, r.to, r.relation, signals])
                .map_err(|e| e.to_string())?;
        }
    }
    let (secs, millis) = now();
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('rebuilt_at', ?1), ('rebuilt_at_ms', ?2)",
        [secs.to_string(), millis.to_string()],
    )
    .map_err(|e| e.to_string())?;
    write_facts_index(&tx, nodes).map_err(|e| e.to_string())?;
    write_graph_fault_point()?;
    tx.commit().map_err(|e| e.to_string())
}

/// Replaces the fact index with one row per fact of `nodes`, in fact id order
/// so row ids repeat from one write to the next, and counts the write in
/// `graph_generation`. The index is whole-graph like `nodes` (a write replaces
/// both), so a fact that left `nodes` leaves the index too. An index that
/// cannot be written is rolled back and left behind the graph, which readers
/// see as stale.
fn write_facts_index(conn: &Connection, nodes: &[NodeRow]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('graph_generation', '1') \
         ON CONFLICT (key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    )?;
    if !facts_index_present(conn) {
        return Ok(());
    }
    conn.execute_batch("SAVEPOINT facts_index")?;
    match fill_facts_index(conn, nodes) {
        Ok(()) => conn.execute_batch("RELEASE facts_index"),
        Err(_) => conn.execute_batch("ROLLBACK TO facts_index; RELEASE facts_index"),
    }
}

fn fill_facts_index(conn: &Connection, nodes: &[NodeRow]) -> rusqlite::Result<()> {
    conn.execute_batch("DELETE FROM facts_fts; DELETE FROM facts_fts_map;")?;
    // A later row of the same id wins, as it does in `nodes`.
    let facts: std::collections::BTreeMap<&str, &NodeRow> = nodes
        .iter()
        .filter(|n| n.scope != CODE_SCOPE)
        .map(|n| (n.id.as_str(), n))
        .collect();
    let mut index = conn.prepare(
        "INSERT INTO facts_fts (rowid, name, description, body) VALUES (?1, ?2, ?3, '')",
    )?;
    let mut map = conn.prepare(
        "INSERT INTO facts_fts_map (rowid, fact_id, scope, project) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (row, node) in (1_i64..).zip(facts.values()) {
        index.execute(rusqlite::params![
            row,
            node.name.as_deref().unwrap_or(""),
            node.description.as_deref().unwrap_or("")
        ])?;
        map.execute(rusqlite::params![row, node.id, node.scope, node.project])?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) \
         SELECT 'facts_index_generation', value FROM meta WHERE key = 'graph_generation'",
        [],
    )?;
    Ok(())
}

/// Whether the fact index was written by the latest graph write, so a reader
/// can trust it. False when it is missing, was not filled, or trails the
/// graph.
pub fn facts_index_is_fresh(conn: &Connection) -> bool {
    let generation = |key: &str| {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    };
    facts_index_present(conn)
        && generation("graph_generation")
            .is_some_and(|g| generation("facts_index_generation") == Some(g))
}

/// A seam inside [`write_graph`]'s transaction, after every row is written
/// and before the commit. A test makes it fail to prove the write rolls back
/// whole.
#[cfg(not(test))]
fn write_graph_fault_point() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_BEFORE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn write_graph_fault_point() -> Result<(), String> {
    if FAIL_BEFORE_COMMIT.with(std::cell::Cell::get) {
        return Err("injected failure before commit".to_string());
    }
    Ok(())
}

/// When the graph in `db` was last written: the time the writer recorded in
/// the database, or the file time when that cannot be read. A reader never
/// changes either, so reading is not mistaken for a write. `Ok(None)` when
/// the database does not exist, and an error when it cannot be inspected.
pub fn touched_at(db: &Path) -> std::io::Result<Option<std::time::SystemTime>> {
    let meta = match std::fs::metadata(db) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let file_time = meta.modified()?;
    Ok(Some(recorded_at(db).unwrap_or(file_time)))
}

fn recorded_at(db: &Path) -> Option<std::time::SystemTime> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(BUSY).ok()?;
    let ms: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'rebuilt_at_ms'",
            [],
            |r| r.get(0),
        )
        .ok()?;
    let ms: u64 = ms.parse().ok()?;
    Some(std::time::UNIX_EPOCH + Duration::from_millis(ms))
}

/// Whether `error` (from [`write_graph`]) says the file is not a usable
/// database, as opposed to being busy or read only.
pub fn is_corrupt(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("not a database") || e.contains("malformed") || e.contains("corrupt")
}

fn now() -> (u64, u128) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs(), d.as_millis())
}

/// The graph stored in the database at `db`, nodes by id and edges by
/// `(from, to, relation)`. `Ok(None)` when the file does not exist.
pub fn read_graph(db: &Path) -> Result<Option<Graph>, String> {
    Ok(read_graph_and_time(db)?.map(|(graph, _)| graph))
}

/// [`read_graph`] plus the time the writer recorded for that graph, both
/// from one read transaction, so the time always belongs to the graph.
pub fn read_graph_and_time(
    db: &Path,
) -> Result<Option<(Graph, Option<std::time::SystemTime>)>, String> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    conn.busy_timeout(BUSY).map_err(|e| e.to_string())?;
    // One read transaction for both tables, so a rebuild between the two
    // queries cannot hand back nodes of one graph with edges of the next.
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut ns = match tx.prepare(
        "SELECT id, file, scope, project, type, name, description, pinned FROM nodes ORDER BY id",
    ) {
        Ok(stmt) => stmt,
        // A file made but not yet filled by its first writer holds no graph.
        Err(e) if e.to_string().contains("no such table") => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let nodes = ns
        .query_map([], |r| {
            Ok(NodeRow {
                id: r.get(0)?,
                file: r.get(1)?,
                scope: r.get(2)?,
                project: r.get(3)?,
                kind: r.get(4)?,
                name: r.get(5)?,
                description: r.get(6)?,
                pinned: r.get::<_, i64>(7)? != 0,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut es = tx
        .prepare("SELECT src, dst, relation, signals FROM edges ORDER BY src, dst, relation")
        .map_err(|e| e.to_string())?;
    let edges = es
        .query_map([], |r| {
            let signals: Option<String> = r.get(3)?;
            Ok(EdgeRow {
                from: r.get(0)?,
                to: r.get(1)?,
                relation: r.get(2)?,
                signals: signals.map(|s| {
                    s.split(',')
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                }),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let rebuilt_at = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'rebuilt_at_ms'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .map(|ms| std::time::UNIX_EPOCH + Duration::from_millis(ms));
    Ok(Some(((nodes, edges), rebuilt_at)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use playbook_common::test_support::scratch_dir;

    #[cfg(unix)]
    #[test]
    fn a_new_graph_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("memory-private-db");
        let db = db_in(&dir.join("data"));

        write_graph(&db, &[], &[]).unwrap();

        let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    fn node(id: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            file: format!("{id}.md"),
            scope: "global".into(),
            project: None,
            kind: "feedback".into(),
            name: Some(id.into()),
            description: Some("d".into()),
            pinned: false,
        }
    }

    fn anchor(id: &str) -> NodeRow {
        NodeRow {
            file: String::new(),
            kind: "anchor".into(),
            name: None,
            description: None,
            ..node(id)
        }
    }

    #[test]
    fn a_graph_round_trips_including_anchor_nodes_and_signals() {
        let d = scratch_dir("mem-rt");
        let db = db_in(&d);
        let nodes = vec![node("a"), anchor("code:~/x"), {
            let mut p = node("b");
            p.project = Some("o/r".into());
            p.pinned = true;
            p
        }];
        let edges = vec![
            EdgeRow {
                from: "a".into(),
                to: "code:~/x".into(),
                relation: "anchors".into(),
                signals: None,
            },
            EdgeRow {
                from: "a".into(),
                to: "b".into(),
                relation: "possible_relates_to".into(),
                signals: Some(vec!["anchor_dir".into(), "type_scope".into()]),
            },
        ];
        write_graph(&db, &nodes, &edges).unwrap();
        let (n, e) = read_graph(&db).unwrap().unwrap();
        assert_eq!(n.len(), 3);
        assert!(n.contains(&anchor("code:~/x")));
        assert!(n
            .iter()
            .any(|x| x.id == "b" && x.pinned && x.project.is_some()));
        assert_eq!(e.len(), 2);
        assert!(e
            .iter()
            .any(|x| x.signals == Some(vec!["anchor_dir".into(), "type_scope".into()])));
    }

    #[test]
    fn a_rewrite_replaces_the_graph_and_never_adds_to_it() {
        let d = scratch_dir("mem-rewrite");
        let db = db_in(&d);
        write_graph(&db, &[node("a"), node("b")], &[]).unwrap();
        write_graph(&db, &[node("b")], &[]).unwrap();
        let (n, _) = read_graph(&db).unwrap().unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].id, "b");
    }

    #[test]
    fn a_missing_database_reads_as_none() {
        let d = scratch_dir("mem-none");
        assert_eq!(read_graph(&db_in(&d)).unwrap(), None);
    }

    #[test]
    fn a_newer_schema_is_refused_and_the_data_is_kept() {
        let d = scratch_dir("mem-newer");
        let db = db_in(&d);
        write_graph(&db, &[node("a")], &[]).unwrap();
        Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE meta SET value = '3' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        assert!(write_graph(&db, &[node("z")], &[]).is_err());
        let (n, _) = read_graph(&db).unwrap().unwrap();
        assert_eq!(n[0].id, "a");
    }

    #[test]
    fn two_nodes_with_one_id_leave_one_row() {
        let d = scratch_dir("mem-dup");
        let db = db_in(&d);
        write_graph(&db, &[node("b"), node("b")], &[]).unwrap();
        let (n, _) = read_graph(&db).unwrap().unwrap();
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn a_failed_rewrite_leaves_the_old_graph() {
        let d = scratch_dir("mem-atomic");
        let db = db_in(&d);
        write_graph(&db, &[node("a")], &[]).unwrap();
        // A trigger makes the insert of one node fail halfway through.
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER boom BEFORE INSERT ON nodes WHEN NEW.id = 'boom' \
                 BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .unwrap();

        let result = write_graph(&db, &[node("c"), node("boom")], &[]);

        assert!(result.is_err());
        let (n, _) = read_graph(&db).unwrap().unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].id, "a");
    }

    #[test]
    fn a_file_that_is_not_a_database_is_reported_as_corrupt() {
        let d = scratch_dir("mem-corrupt");
        let db = db_in(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&db, b"this is not a sqlite file at all, just text padding").unwrap();

        let error = write_graph(&db, &[node("a")], &[]).unwrap_err();

        assert!(is_corrupt(&error), "{error}");
    }

    #[test]
    fn a_database_without_tables_reads_as_no_graph() {
        let d = scratch_dir("mem-no-tables");
        let db = db_in(&d);
        std::fs::create_dir_all(&d).unwrap();
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE x (a);")
            .unwrap();

        assert!(read_graph(&db).unwrap().is_none());
    }

    #[test]
    fn touched_at_follows_the_database() {
        let d = scratch_dir("mem-touched");
        let db = db_in(&d);
        assert!(touched_at(&db).unwrap().is_none());

        write_graph(&db, &[node("a")], &[]).unwrap();

        let stored: String = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key = 'rebuilt_at_ms'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let recorded = std::time::UNIX_EPOCH + Duration::from_millis(stored.parse().unwrap());
        assert_eq!(touched_at(&db).unwrap(), Some(recorded));
    }

    fn open_memory() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn init_without_fts5_succeeds_and_makes_no_fact_index() {
        // Arrange
        let conn = open_memory();

        // Act
        let result = init_with(&conn, Fts5Support::Unavailable);

        // Assert
        assert!(result.is_ok());
        assert!(!facts_index_present(&conn));
        assert!(conn.prepare("SELECT 1 FROM facts_fts_map").is_ok());
    }

    #[test]
    fn init_succeeds_when_the_fact_index_cannot_be_created() {
        // Arrange: a plain table already holds the name.
        let conn = open_memory();
        conn.execute_batch("CREATE TABLE facts_fts (x TEXT);")
            .unwrap();

        // Act
        let result = init_with(&conn, Fts5Support::Available);

        // Assert
        assert!(result.is_ok());
        assert!(!facts_index_present(&conn));
    }

    #[test]
    fn init_creates_the_fact_index_once_and_is_then_a_no_op() {
        // Arrange
        let conn = open_memory();

        // Act
        init_with(&conn, Fts5Support::Available).unwrap();
        conn.execute(
            "INSERT INTO facts_fts (rowid, name) VALUES (1, 'alpha')",
            [],
        )
        .unwrap();
        init_with(&conn, Fts5Support::Available).unwrap();

        // Assert
        assert!(facts_index_present(&conn));
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH 'alpha'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn the_bundled_sqlite_has_fts5() {
        // Arrange, Act
        let support = Fts5Support::probe(&open_memory());

        // Assert
        assert_eq!(support, Fts5Support::Available, "bundled SQLite lost FTS5");
    }

    #[test]
    fn a_version_one_file_is_moved_to_the_current_version() {
        // Arrange
        let d = scratch_dir("mem-v1");
        let db = db_in(&d);
        write_graph(&db, &[node("a")], &[]).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();

        // Act
        write_graph(&db, &[node("b")], &[]).unwrap();

        // Assert
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(facts_index_present(&conn));
    }

    fn fact(id: &str, scope: &str, project: Option<&str>, name: &str, desc: &str) -> NodeRow {
        NodeRow {
            scope: scope.into(),
            project: project.map(str::to_string),
            name: Some(name.into()),
            description: Some(desc.into()),
            ..node(id)
        }
    }

    fn open(db: &Path) -> Connection {
        Connection::open(db).unwrap()
    }

    fn indexed_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT fact_id FROM facts_fts_map ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn ids_matching(conn: &Connection, term: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT m.fact_id FROM facts_fts f JOIN facts_fts_map m ON m.rowid = f.rowid \
                 WHERE facts_fts MATCH ?1 ORDER BY m.fact_id",
            )
            .unwrap();
        stmt.query_map([term], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn generation(conn: &Connection) -> String {
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'graph_generation'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_rebuild_indexes_every_fact_and_no_code_anchor() {
        // Arrange
        let db = db_in(&scratch_dir("mem-idx"));
        let nodes = [
            fact("b", "global", None, "ferry-timetable", "cache expiry"),
            fact("a", "project", Some("o/r"), "crane-queue", "backpressure"),
            NodeRow {
                scope: CODE_SCOPE.into(),
                ..anchor("code:~/x")
            },
        ];

        // Act
        write_graph(&db, &nodes, &[]).unwrap();

        // Assert
        let conn = open(&db);
        assert_eq!(indexed_ids(&conn), ["a", "b"]);
        assert_eq!(ids_matching(&conn, "ferry"), ["b"]);
        assert_eq!(ids_matching(&conn, "backpressure"), ["a"]);
        assert!(facts_index_is_fresh(&conn));
    }

    #[test]
    fn a_fact_that_left_the_graph_leaves_the_index() {
        // Arrange
        let db = db_in(&scratch_dir("mem-idx-del"));
        let keep = fact("keep", "global", None, "alpha", "d");
        let gone = fact("gone", "global", None, "beta", "d");
        write_graph(&db, &[keep.clone(), gone], &[]).unwrap();

        // Act
        write_graph(&db, &[keep], &[]).unwrap();

        // Assert
        let conn = open(&db);
        assert_eq!(indexed_ids(&conn), ["keep"]);
        assert!(ids_matching(&conn, "beta").is_empty());
    }

    #[test]
    fn a_changed_fact_of_the_same_count_is_reindexed_under_a_new_generation() {
        // Arrange
        let db = db_in(&scratch_dir("mem-idx-chg"));
        write_graph(&db, &[fact("a", "global", None, "old-name", "d")], &[]).unwrap();
        let before = generation(&open(&db));

        // Act
        write_graph(&db, &[fact("a", "global", None, "new-name", "d")], &[]).unwrap();

        // Assert
        let conn = open(&db);
        assert_ne!(generation(&conn), before);
        assert!(ids_matching(&conn, "old").is_empty());
        assert_eq!(ids_matching(&conn, "new"), ["a"]);
        assert!(facts_index_is_fresh(&conn));
    }

    #[test]
    fn rewriting_the_whole_graph_keeps_the_facts_of_every_repo() {
        // Arrange: a rebuild that changes repo A writes repo B's facts too.
        let db = db_in(&scratch_dir("mem-idx-two"));
        let b = fact("b", "project", Some("o/b"), "repo-b-fact", "d");
        let a_old = fact("a", "project", Some("o/a"), "repo-a-old", "d");
        write_graph(&db, &[a_old, b.clone()], &[]).unwrap();
        let a_new = fact("a", "project", Some("o/a"), "repo-a-new", "d");

        // Act
        write_graph(&db, &[a_new, b], &[]).unwrap();

        // Assert
        let conn = open(&db);
        assert_eq!(indexed_ids(&conn), ["a", "b"]);
        assert_eq!(ids_matching(&conn, "repo"), ["a", "b"]);
        assert_eq!(ids_matching(&conn, "new"), ["a"]);
    }

    #[test]
    fn two_rows_with_one_id_leave_one_index_row() {
        // Arrange
        let db = db_in(&scratch_dir("mem-idx-dup"));
        let first = fact("a", "global", None, "first", "d");
        let second = fact("a", "global", None, "second", "d");

        // Act
        write_graph(&db, &[first, second], &[]).unwrap();

        // Assert
        let conn = open(&db);
        assert_eq!(indexed_ids(&conn), ["a"]);
        assert_eq!(ids_matching(&conn, "second"), ["a"]);
    }

    #[test]
    fn a_failure_before_the_commit_rolls_back_nodes_and_index_together() {
        // Arrange
        let db = db_in(&scratch_dir("mem-idx-fault"));
        write_graph(&db, &[fact("a", "global", None, "kept", "d")], &[]).unwrap();
        FAIL_BEFORE_COMMIT.with(|f| f.set(true));

        // Act
        let result = write_graph(&db, &[fact("z", "global", None, "lost", "d")], &[]);
        FAIL_BEFORE_COMMIT.with(|f| f.set(false));

        // Assert
        assert!(result.is_err());
        let conn = open(&db);
        assert_eq!(indexed_ids(&conn), ["a"]);
        assert_eq!(ids_matching(&conn, "kept"), ["a"]);
        let nodes: i64 = conn
            .query_row("SELECT count(*) FROM nodes WHERE id = 'z'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(nodes, 0);
        assert!(facts_index_is_fresh(&conn));
    }

    #[test]
    fn a_write_succeeds_and_the_index_is_not_fresh_when_the_table_is_unusable() {
        // Arrange: a plain table holds the name, so the index cannot exist.
        let db = db_in(&scratch_dir("mem-idx-plain"));
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        open(&db)
            .execute_batch("CREATE TABLE facts_fts (x TEXT);")
            .unwrap();

        // Act
        let result = write_graph(&db, &[fact("a", "global", None, "n", "d")], &[]);

        // Assert
        assert!(result.is_ok());
        assert!(!facts_index_is_fresh(&open(&db)));
    }
}
