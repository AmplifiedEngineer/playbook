// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `~/.config/playbook/memory/memory.signals.json` data layer: hit counters, staleness
//! stamps, and a consolidation cursor. Every reader/writer of this file goes through this module's lock, so no caller's update is silently dropped by another's.

use crate::common::atomic::with_dir_lock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// How many hits within `PROMOTION_WINDOW_SECS` a node needs before it is
/// marked `promoted`. Deliberately lower than "rediscovered five-plus
/// times", the pain this exists to catch: three hits should flag a pattern
/// before it repeats five times, not after.
const PROMOTION_HIT_THRESHOLD: u32 = 3;

/// The rolling window `PROMOTION_HIT_THRESHOLD` hits must land inside to
/// count together. Three matches inside a week is a real, active pattern;
/// three matches spread across six months is not the same signal.
const PROMOTION_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;

/// The whole signals store: one node entry per memory node plus a shared
/// consolidation cursor. Round-trips through `serde_json` as-is; a missing
/// or unparsable file falls back to `SignalsStore::default()`, an empty
/// store with no nodes.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SignalsStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    cursor: Cursor,
    #[serde(default)]
    nodes: HashMap<String, NodeSignals>,
}

/// Where the consolidation pass last left off, read and advanced by
/// `read_cursor`/`advance_cursor` below.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    #[serde(default)]
    last_run_at: Option<String>,
}

/// Per-node signals: hit count and promotion state (`bump_hit`/`ranking_signals`),
/// plus `stale`/`verified_hash`/`verified_at` (`cached_stale`/`set_staleness`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct NodeSignals {
    #[serde(default)]
    hits: u32,
    #[serde(default)]
    window_start: Option<String>,
    #[serde(default)]
    promoted: bool,
    #[serde(default)]
    stale: Option<bool>,
    #[serde(default)]
    verified_hash: Option<String>,
    #[serde(default)]
    verified_at: Option<String>,
}

/// Run `f` against the current store loaded from `mem_dir`'s
/// `memory.signals.json` (an empty default if the file is missing, the
/// normal first-run case), then write the mutated store back atomically. A
/// file that exists but fails to read or parse is left untouched instead:
/// unlike the memory graph in `data/memory.db`, this store accumulates state nothing else can
/// rebuild, so silently replacing a corrupt file with an empty one would be
/// permanent, unrecoverable data loss. Serializes concurrent callers with an
/// mkdir-based advisory lock at `memory.signals.json.lock`, mirroring
/// `rebuild_memory_graph.rs`'s `rebuild()`: without it, two callers racing to
/// bump different nodes can each read the same starting snapshot, and
/// whichever write finishes second silently discards the other's change.
pub fn modify_locked(mem_dir: &Path, f: impl FnOnce(&mut SignalsStore)) {
    let lock_path = mem_dir.join("memory.signals.json.lock");
    let (acquired, ()) = with_dir_lock(&lock_path, 50, Duration::from_millis(10), || {
        if let Some(mut store) = read_store(mem_dir) {
            store.version = 1;
            f(&mut store);
            write_store_atomically(mem_dir, &store);
        }
    });
    if acquired {
        let _ = fs::remove_dir(&lock_path);
    }
}

/// Increment `node_id`'s hit count by one, creating its entry if absent, and
/// mark it promoted once its hits cross `PROMOTION_HIT_THRESHOLD` within a
/// `PROMOTION_WINDOW_SECS` rolling window. A hit landing outside that window
/// (or a node with no window recorded yet) starts a fresh window at a count
/// of one, rather than accumulating with hits from long before. Promotion is
/// a one-way ratchet: once `promoted` is `true`, this never resets it back
/// to `false`.
pub fn bump_hit(mem_dir: &Path, node_id: &str) {
    bump_hits(mem_dir, &[node_id.to_string()]);
}

/// Applies `bump_hit` to every id in `ids` (repeats count each time) inside a
/// single lock cycle and a single store rewrite.
pub fn bump_hits(mem_dir: &Path, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let now = crate::common::time::now_epoch_secs();
    modify_locked(mem_dir, |store| {
        for node_id in ids {
            let entry = store.nodes.entry(node_id.clone()).or_default();
            let window_expired = entry
                .window_start
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none_or(|start| now.saturating_sub(start) > PROMOTION_WINDOW_SECS);
            if window_expired {
                entry.window_start = Some(now.to_string());
                entry.hits = 1;
            } else {
                entry.hits += 1;
            }
            if entry.hits >= PROMOTION_HIT_THRESHOLD {
                entry.promoted = true;
            }
        }
    });
}

/// `node_id`'s cached staleness verdict. `None` on a genuine miss (no entry
/// yet) or an unparsable file; a parse failure never reads as "not stale".
pub fn cached_stale(mem_dir: &Path, node_id: &str) -> Option<bool> {
    read_store(mem_dir)?.nodes.get(node_id)?.stale
}

/// Caches `node_id`'s fresh staleness verdict and signature, leaving its hit
/// count and promotion state untouched.
pub fn set_staleness(mem_dir: &Path, node_id: &str, stale: bool, verified_hash: Option<String>) {
    let now = crate::common::time::now_epoch_secs().to_string();
    modify_locked(mem_dir, |store| {
        let entry = store.nodes.entry(node_id.to_string()).or_default();
        entry.stale = Some(stale);
        entry.verified_at = Some(now);
        if let Some(hash) = verified_hash {
            entry.verified_hash = Some(hash);
        }
    });
}

/// The consolidation cursor's last recorded pass time, in epoch seconds.
/// `None` before the first pass ever ran, or on a missing/unparsable file.
pub fn read_cursor(mem_dir: &Path) -> Option<u64> {
    read_store(mem_dir)?
        .cursor
        .last_run_at
        .and_then(|s| s.parse::<u64>().ok())
}

/// Stamps the consolidation cursor with the current time, so the next pass
/// only considers facts touched since.
pub fn advance_cursor(mem_dir: &Path) {
    let now = crate::common::time::now_epoch_secs().to_string();
    modify_locked(mem_dir, |store| {
        store.cursor.last_run_at = Some(now.clone());
    });
}

/// Per-node `(hits, promoted)` in one read-only pass, for ranking. A missing
/// or unparsable file returns an empty map, so a ranking caller falls back to
/// pins and links alone. Never writes.
pub fn ranking_signals(mem_dir: &Path) -> HashMap<String, (u32, bool)> {
    read_store(mem_dir)
        .map(|store| {
            store
                .nodes
                .into_iter()
                .map(|(id, signals)| (id, (signals.hits, signals.promoted)))
                .collect()
        })
        .unwrap_or_default()
}

/// A missing file returns an empty default store. A file that exists but
/// fails to read or parse returns `None`, so the caller can leave it on disk
/// untouched rather than overwriting possibly-recoverable data.
fn read_store(mem_dir: &Path) -> Option<SignalsStore> {
    match fs::read_to_string(mem_dir.join("memory.signals.json")) {
        Ok(content) => serde_json::from_str(&content).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(SignalsStore::default()),
        Err(_) => None,
    }
}

/// Write `store` to `memory.signals.json` inside `mem_dir` via a temp file in
/// the same directory plus a rename, so a reader never observes a partially
/// written file. Mirrors `rebuild_memory_graph.rs`'s `write_graph_atomically`.
fn write_store_atomically(mem_dir: &Path, store: &SignalsStore) {
    let Ok(rendered) = serde_json::to_string_pretty(store) else {
        return;
    };
    let _ = crate::common::atomic::write_atomic(&mem_dir.join("memory.signals.json"), rendered);
}
