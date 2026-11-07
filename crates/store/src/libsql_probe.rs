//! Throwaway spike: open a file database through libsql with a hand-rolled blocking wrapper.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Minimal executor: local libsql futures finish without a reactor.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

/// Open `path`, make a table, insert one row, read it back.
pub fn open_and_query(path: &Path) -> Result<i64, libsql::Error> {
    block_on(async {
        let db = libsql::Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        conn.execute("CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, v TEXT)", ())
            .await?;
        conn.execute("INSERT INTO t (v) VALUES (?1)", libsql::params!["a"]).await?;
        let mut rows = conn.query("SELECT count(*) FROM t", ()).await?;
        let row = rows.next().await?.expect("one row");
        row.get::<i64>(0)
    })
}

/// Open an existing database and run one query (the statusline shape).
pub fn open_one_query(path: &Path) -> Result<i64, libsql::Error> {
    block_on(async {
        let db = libsql::Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        let mut rows = conn.query("SELECT count(*) FROM t", ()).await?;
        let row = rows.next().await?.expect("one row");
        row.get::<i64>(0)
    })
}

/// The SQLite version string and source id this libsql sees.
pub fn version() -> Result<String, libsql::Error> {
    block_on(async {
        let db = libsql::Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        let mut rows = conn.query("SELECT sqlite_version() || ' ' || sqlite_source_id()", ()).await?;
        rows.next().await?.expect("one row").get::<String>(0)
    })
}
