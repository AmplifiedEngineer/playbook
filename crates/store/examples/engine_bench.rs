//! Throwaway spike: both engines in one binary, plus an open-and-query timing loop.
use std::path::Path;
use std::time::Instant;

fn rusqlite_open_one_query(p: &Path) -> i64 {
    let c = rusqlite::Connection::open(p).unwrap();
    c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let order = std::env::args().nth(1).unwrap_or_default();
    if order == "libsql-first" {
        println!("libsql first: {}", playbook_store::libsql_probe::version().unwrap());
    }
    let c0 = rusqlite::Connection::open_in_memory().unwrap();
    let v: String = c0.query_row("SELECT sqlite_version() || ' ' || sqlite_source_id()", [], |r| r.get(0)).unwrap();
    println!("rusqlite: {v}");
    println!("libsql: {}", playbook_store::libsql_probe::version().unwrap());
    let dir = std::env::temp_dir().join(format!("pb-engine-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rs = dir.join("rs.db");
    let ls = dir.join("ls.db");
    {
        let c = rusqlite::Connection::open(&rs).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO t (v) VALUES ('a');").unwrap();
    }
    println!("libsql create/insert/query count: {}", playbook_store::libsql_probe::open_and_query(&ls).unwrap());
    // Cross-engine: libsql reads the rusqlite file, rusqlite reads the libsql file.
    println!("libsql reads rusqlite file: {}", playbook_store::libsql_probe::open_one_query(&rs).unwrap());
    println!("rusqlite reads libsql file: {}", rusqlite_open_one_query(&ls));
    println!("sqlite versions: rusqlite {}", rusqlite::version());

    const N: usize = 200;
    for (name, f) in [
        ("rusqlite", Box::new(|| { rusqlite_open_one_query(&rs); }) as Box<dyn Fn()>),
        ("libsql", Box::new(|| { playbook_store::libsql_probe::open_one_query(&rs).unwrap(); })),
    ] {
        let t = Instant::now();
        f(); // first open in this process
        let first = t.elapsed().as_secs_f64() * 1e3;
        f(); // warm-up
        let runs: Vec<f64> = (0..5)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..N {
                    f();
                }
                t.elapsed().as_secs_f64() * 1e3 / N as f64
            })
            .collect();
        println!("{name}: first {first:.3} ms, median of 5 per open+query {:.3} ms (runs {runs:.3?})", median(runs.clone()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
