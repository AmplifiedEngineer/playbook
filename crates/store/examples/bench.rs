//! Throwaway spike: one engine per binary. `once` runs a single open and query, for process timing.
use std::path::Path;
use std::time::Instant;

#[cfg(feature = "engine-libsql")]
fn open_one_query(p: &Path) -> i64 {
    playbook_store::libsql_probe::open_one_query(p).unwrap()
}

#[cfg(not(feature = "engine-libsql"))]
fn open_one_query(p: &Path) -> i64 {
    let c = rusqlite::Connection::open(p).unwrap();
    c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap()
}

fn main() {
    let p = std::path::PathBuf::from(std::env::args().nth(1).expect("db path"));
    if std::env::args().nth(2).as_deref() == Some("once") {
        std::process::exit(i32::from(open_one_query(&p) != 1));
    }
    let t = Instant::now();
    open_one_query(&p);
    println!("first open+query {:.3} ms", t.elapsed().as_secs_f64() * 1e3);
    open_one_query(&p);
    let mut runs: Vec<f64> = (0..5)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..200 {
                open_one_query(&p);
            }
            t.elapsed().as_secs_f64() * 1e3 / 200.0
        })
        .collect();
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("median of 5 (200 opens each) {:.3} ms/open+query", runs[2]);
}
