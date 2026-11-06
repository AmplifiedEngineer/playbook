// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Keeps the view fresh without blocking the screen. A background thread owns
//! the database connection: it ingests new transcript lines and re-reads the
//! store on a timer, and at once when the range or filter changes, with the
//! same ingest `usage --summary` runs. A first read of a large history goes
//! in small batches and the screen shows the partial store as it grows.

use super::account::AccountSel;
use super::data::{self, Data};
use crate::query::Range;
use crate::run::{ingest_into, Paths};
use crate::util::time::now_secs;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the store is re-read when nothing else asks, unless
/// `usage.refreshSeconds` says otherwise.
pub const DEFAULT_REFRESH_SECONDS: u64 = 5;

pub enum Request {
    /// Show this range and filter now, then keep refreshing them.
    Query {
        range: Range,
        filter: String,
        account: AccountSel,
    },
}

pub enum Update {
    Data(Box<Data>),
    Error(String),
    /// Files read so far and files to read, while a first read is running;
    /// `None` once it is done.
    Indexing(Option<(usize, usize)>),
}

pub struct Worker {
    requests: Sender<Request>,
    pub updates: Receiver<Update>,
    handle: Option<JoinHandle<()>>,
}

/// How often a partial snapshot is sent while a first read runs.
const PARTIAL_EVERY: Duration = Duration::from_secs(1);

/// Reads the store into a snapshot.
fn snapshot(
    conn: &rusqlite::Connection,
    paths: &Paths,
    query: &(Range, String, AccountSel),
) -> Update {
    match data::load(conn, query.0, &query.1, &query.2, now_secs()) {
        Ok(mut d) => {
            // A limits failure never hides the rest of the view.
            if let Ok(l) = data::load_limits(conn, paths, query.0, d.accounts.key.as_deref(), d.now)
            {
                d.limits = l;
            }
            Update::Data(Box::new(d))
        }
        Err(e) => Update::Error(e),
    }
}

impl Worker {
    pub fn spawn(
        paths: Paths,
        range: Range,
        filter: String,
        account: AccountSel,
        every: Duration,
    ) -> Worker {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (update_tx, updates) = mpsc::channel::<Update>();
        let handle = std::thread::spawn(move || {
            let mut query = (range, filter, account);
            let mut first = true;
            loop {
                let conn = match crate::db::open_db(&paths.db) {
                    Ok(c) => c,
                    Err(e) => {
                        if update_tx.send(Update::Error(e)).is_err() {
                            return;
                        }
                        match request_rx.recv_timeout(every) {
                            Ok(Request::Query {
                                range,
                                filter,
                                account,
                            }) => query = (range, filter, account),
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                        continue;
                    }
                };
                // Show what the store holds before any ingest, so the screen
                // never waits on a first read of a large history.
                if first {
                    first = false;
                    if update_tx.send(snapshot(&conn, &paths, &query)).is_err() {
                        return;
                    }
                }
                let mut gone = false;
                let mut last_sent = std::time::Instant::now();
                let ingested = ingest_into(&paths, &conn, &mut |done, total| {
                    if total == 0 || done == total {
                        return true;
                    }
                    while let Ok(Request::Query {
                        range,
                        filter,
                        account,
                    }) = request_rx.try_recv()
                    {
                        query = (range, filter, account);
                        last_sent = std::time::Instant::now() - PARTIAL_EVERY;
                    }
                    if update_tx
                        .send(Update::Indexing(Some((done, total))))
                        .is_err()
                    {
                        gone = true;
                    }
                    if last_sent.elapsed() >= PARTIAL_EVERY {
                        last_sent = std::time::Instant::now();
                        if update_tx.send(snapshot(&conn, &paths, &query)).is_err() {
                            gone = true;
                        }
                    }
                    !gone
                });
                if gone || update_tx.send(Update::Indexing(None)).is_err() {
                    return;
                }
                let update = match ingested {
                    Ok(_) => snapshot(&conn, &paths, &query),
                    Err(e) => Update::Error(e),
                };
                if update_tx.send(update).is_err() {
                    return;
                }
                match request_rx.recv_timeout(every) {
                    Ok(Request::Query {
                        range,
                        filter,
                        account,
                    }) => query = (range, filter, account),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Worker {
            requests,
            updates,
            handle: Some(handle),
        }
    }

    pub fn ask(&self, range: Range, filter: &str, account: &AccountSel) {
        let _ = self.requests.send(Request::Query {
            range,
            filter: filter.to_string(),
            account: account.clone(),
        });
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the request channel ends the loop after its current pass; the
        // handle is dropped without a join so quitting never waits on an ingest.
        drop(self.handle.take());
    }
}
