// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Behavioural tests for `memory_signals`, the `memory.signals.json` data
//! layer. Unlike every other hook test file, there is no `HookName`/CLI
//! entry point to invoke as a subprocess yet, so these call the module's
//! `pub fn`s directly, in-process, through `playbook::hooks::memory_signals`.
//!
//! Each test gets its own scratch directory, unique per call, so tests stay
//! parallel-safe under `cargo test`'s default concurrent execution.

use playbook::hooks::memory_signals::{
    bump_hit, bump_hits, cached_stale, modify_locked, ranking_signals, set_staleness,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// --- Test infrastructure ---------------------------------------------------

fn is_promoted(mem_dir: &Path, node_id: &str) -> bool {
    ranking_signals(mem_dir)
        .get(node_id)
        .is_some_and(|(_, promoted)| *promoted)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory standing in for `~/.claude/memory`, unique per
/// call so parallel test threads never collide.
fn scratch_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-wu1-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn signals_path(mem_dir: &Path) -> PathBuf {
    mem_dir.join("memory.signals.json")
}

fn read_store(mem_dir: &Path) -> Value {
    let content =
        fs::read_to_string(signals_path(mem_dir)).expect("memory.signals.json should exist");
    serde_json::from_str(&content).expect("memory.signals.json should be valid JSON")
}

fn hits_for(store: &Value, node_id: &str) -> u64 {
    store["nodes"][node_id]["hits"]
        .as_u64()
        .expect("node should have a numeric hits field")
}

// --- Scenario 1: round-trip plus missing-file default ----------------------

#[test]
fn bump_hit_round_trips_through_the_signals_file() {
    // Arrange
    let mem_dir = scratch_home("round-trip");

    // Act
    bump_hit(&mem_dir, "global/example-fact");

    // Assert
    let store = read_store(&mem_dir);
    assert_eq!(hits_for(&store, "global/example-fact"), 1);
    assert!(
        !mem_dir.join("memory.signals.json.lock").exists(),
        "a call that acquired the lock must remove its own lock directory"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// A file that exists but fails to parse must be left untouched, not
/// silently replaced with an empty store: this store accumulates state
/// nothing else can rebuild, unlike the fully-regenerable memory graph.
#[test]
fn a_corrupt_signals_file_is_left_untouched_rather_than_wiped() {
    // Arrange
    let mem_dir = scratch_home("corrupt-file");
    fs::write(signals_path(&mem_dir), "not valid json").unwrap();

    // Act
    bump_hit(&mem_dir, "global/example-fact");

    // Assert
    let content = fs::read_to_string(signals_path(&mem_dir)).unwrap();
    assert_eq!(
        content, "not valid json",
        "a corrupt file must be left exactly as-is, not overwritten with an empty store"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// A scratch directory with no `memory.signals.json` present at all must
/// still produce a valid, empty store rather than panicking: `modify_locked`
/// hands its closure a default `SignalsStore` in that case, and the closure
/// below records what it saw without mutating anything.
#[test]
fn modify_locked_defaults_to_an_empty_store_when_the_file_is_missing() {
    // Arrange
    let mem_dir = scratch_home("missing-file-default");

    // Act
    modify_locked(&mem_dir, |_store| {});

    // Assert
    let store = read_store(&mem_dir);
    assert_eq!(
        store["nodes"].as_object().map(serde_json::Map::len),
        Some(0),
        "a missing file should default to an empty store, not panic or leave nodes populated"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

// --- Scenario 2: two concurrent writers under normal contention ------------

/// Two sessions bumping different nodes near the same moment must both land
/// in the final store. Without a lock around the read-modify-write cycle,
/// the write that finishes second overwrites whatever the other just wrote,
/// silently dropping its bump.
#[test]
fn two_concurrent_bumps_for_different_nodes_both_survive() {
    // Arrange
    let mem_dir = scratch_home("concurrent-bumps");
    let mem_dir_a = mem_dir.clone();
    let mem_dir_b = mem_dir.clone();

    // Act: two real threads, each bumping a different node, so their
    // read-modify-write cycles actually overlap.
    let a = std::thread::spawn(move || bump_hit(&mem_dir_a, "global/fact-a"));
    let b = std::thread::spawn(move || bump_hit(&mem_dir_b, "global/fact-b"));
    a.join().expect("thread a should not panic");
    b.join().expect("thread b should not panic");

    // Assert
    let store = read_store(&mem_dir);
    assert_eq!(
        hits_for(&store, "global/fact-a"),
        1,
        "fact-a's bump must survive a concurrent write, not be silently dropped"
    );
    assert_eq!(
        hits_for(&store, "global/fact-b"),
        1,
        "fact-b's bump must survive a concurrent write, not be silently dropped"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// Two concurrent bumps to the SAME node, the actual hit-counter use case,
/// must both land as a count of 2, not 1: the disjoint-node scenario above
/// proves the lock serializes access, but can't catch a read-modify-write bug
/// that only shows up when two writers touch the same map entry.
#[test]
fn two_concurrent_bumps_for_the_same_node_both_count() {
    // Arrange
    let mem_dir = scratch_home("concurrent-same-node");
    let mem_dir_a = mem_dir.clone();
    let mem_dir_b = mem_dir.clone();

    // Act
    let a = std::thread::spawn(move || bump_hit(&mem_dir_a, "global/shared-fact"));
    let b = std::thread::spawn(move || bump_hit(&mem_dir_b, "global/shared-fact"));
    a.join().expect("thread a should not panic");
    b.join().expect("thread b should not panic");

    // Assert
    let store = read_store(&mem_dir);
    assert_eq!(
        hits_for(&store, "global/shared-fact"),
        2,
        "two concurrent bumps to the same node must both count, not overwrite each other"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

// --- Scenario 3: lock-exhausted path ----------------------------------------

/// `with_dir_lock` is deliberately fail-open: it always runs its closure,
/// whether or not it acquired the lock. Pre-creating the lock directory
/// simulates another process mid-write; the call under test must still
/// complete without panicking, must leave a valid JSON file behind, and must
/// not remove a lock directory it never created (removing it would destroy
/// the other process's in-progress lock). The increment itself is not
/// asserted: a lost update under exhaustion is the accepted, documented
/// behavior, not a bug this test should hide by retrying until it passes.
#[test]
fn bump_hit_completes_without_panicking_when_the_lock_is_already_held() {
    // Arrange
    let mem_dir = scratch_home("lock-held");
    let lock_dir = mem_dir.join("memory.signals.json.lock");
    fs::create_dir(&lock_dir).expect("pre-creating the lock dir should succeed");

    // Act
    bump_hit(&mem_dir, "global/fact-under-lock");

    // Assert
    let content = fs::read_to_string(signals_path(&mem_dir))
        .expect("a file should still be written under lock exhaustion");
    let _: Value = serde_json::from_str(&content).expect("the written file must be valid JSON");
    assert!(
        lock_dir.exists(),
        "a call that did not acquire the lock must not remove it"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

// --- Scenario 4: usage-based promotion --------------------------------------

/// Fewer hits than the promotion threshold must never promote a node.
#[test]
fn hits_below_the_threshold_do_not_promote() {
    // Arrange
    let mem_dir = scratch_home("below-threshold");

    // Act
    bump_hit(&mem_dir, "global/rarely-hit-fact");
    bump_hit(&mem_dir, "global/rarely-hit-fact");

    // Assert
    assert!(
        !is_promoted(&mem_dir, "global/rarely-hit-fact"),
        "two hits should not cross the promotion threshold"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// A hit landing exactly on the threshold promotes: the boundary is `>=`,
/// not `>`.
#[test]
fn a_hit_landing_exactly_on_the_threshold_promotes() {
    // Arrange
    let mem_dir = scratch_home("exact-threshold");

    // Act
    bump_hit(&mem_dir, "global/threshold-fact");
    bump_hit(&mem_dir, "global/threshold-fact");
    assert!(
        !is_promoted(&mem_dir, "global/threshold-fact"),
        "two hits, one short of the threshold, must not promote yet"
    );
    bump_hit(&mem_dir, "global/threshold-fact");

    // Assert
    assert!(
        is_promoted(&mem_dir, "global/threshold-fact"),
        "the third hit, landing exactly on the threshold, should promote"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// Hits recorded outside the promotion window reset rather than accumulate:
/// three matches inside a week is a real, active pattern, three matches
/// spread across six months is not the same signal. `NodeSignals` has no
/// public constructor, so the fixture is written directly as JSON, matching
/// what `bump_hit` itself would have produced two hits and a week-plus ago.
#[test]
fn hits_outside_an_expired_window_reset_rather_than_accumulate() {
    // Arrange: a window_start well past the 7-day promotion window, with two
    // hits already recorded under it.
    let mem_dir = scratch_home("expired-window");
    let seven_days_and_a_bit_ago = now_epoch_secs() - (7 * 24 * 60 * 60) - 60;
    fs::write(
        signals_path(&mem_dir),
        format!(
            r#"{{"nodes":{{"global/stale-window-fact":{{"hits":2,"window_start":"{seven_days_and_a_bit_ago}","promoted":false}}}}}}"#
        ),
    )
    .unwrap();

    // Act
    bump_hit(&mem_dir, "global/stale-window-fact");

    // Assert
    let store = read_store(&mem_dir);
    assert_eq!(
        hits_for(&store, "global/stale-window-fact"),
        1,
        "a hit outside the expired window should reset the count, not accumulate to 3"
    );
    assert!(
        !is_promoted(&mem_dir, "global/stale-window-fact"),
        "a reset count of 1 must not be promoted"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

// --- Scenario 5: staleness caching shares the typed store, not a second one -

/// A cached verdict round-trips through `cached_stale` unchanged.
#[test]
fn set_staleness_then_cached_stale_round_trips() {
    // Arrange
    let mem_dir = scratch_home("staleness-round-trip");

    // Act
    set_staleness(
        &mem_dir,
        "global/anchor-fact",
        true,
        Some("abc123".to_string()),
    );

    // Assert
    assert_eq!(cached_stale(&mem_dir, "global/anchor-fact"), Some(true));

    let _ = fs::remove_dir_all(&mem_dir);
}

/// Regression pin: a hit bump for the same node must not erase its cached
/// staleness verdict, since both now round-trip through one typed struct.
#[test]
fn a_bump_hit_after_set_staleness_does_not_erase_the_cached_verdict() {
    // Arrange
    let mem_dir = scratch_home("staleness-survives-bump");
    set_staleness(&mem_dir, "global/anchor-fact", true, None);

    // Act
    bump_hit(&mem_dir, "global/anchor-fact");

    // Assert
    assert_eq!(
        cached_stale(&mem_dir, "global/anchor-fact"),
        Some(true),
        "a hit bump for the same node must not drop its cached staleness verdict"
    );
    let store = read_store(&mem_dir);
    assert_eq!(
        hits_for(&store, "global/anchor-fact"),
        1,
        "the bump itself must still have counted"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

/// A corrupt file must read as a genuine miss (`None`), never a false
/// "not stale" (`Some(false)`).
#[test]
fn cached_stale_on_a_corrupt_file_reads_as_a_miss_not_false() {
    // Arrange
    let mem_dir = scratch_home("staleness-corrupt-file");
    fs::write(signals_path(&mem_dir), "not valid json").unwrap();

    // Act
    let result = cached_stale(&mem_dir, "global/anchor-fact");

    // Assert
    assert_eq!(
        result, None,
        "a corrupt file must read as a cache miss, not a false verdict"
    );

    let _ = fs::remove_dir_all(&mem_dir);
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// --- Batched bumps and stale lock recovery ---------------------------------

#[test]
fn bump_hits_matches_sequential_bump_hit_calls() {
    // Arrange
    let batched = scratch_home("batch-eq-a");
    let sequential = scratch_home("batch-eq-b");
    let ids: Vec<String> = ["a", "b", "a", "c", "a"].map(String::from).to_vec();

    // Act
    bump_hits(&batched, &ids);
    for id in &ids {
        bump_hit(&sequential, id);
    }

    // Assert
    let (b, s) = (read_store(&batched), read_store(&sequential));
    for id in ["a", "b", "c"] {
        assert_eq!(hits_for(&b, id), hits_for(&s, id), "hits differ for {id}");
        assert_eq!(is_promoted(&batched, id), is_promoted(&sequential, id));
    }
    assert_eq!(hits_for(&b, "a"), 3);

    let _ = fs::remove_dir_all(&batched);
    let _ = fs::remove_dir_all(&sequential);
}

#[cfg(unix)]
#[test]
fn a_stale_lock_dir_does_not_block_bump_hits() {
    // Arrange
    let mem_dir = scratch_home("stale-lock");
    let lock = mem_dir.join("memory.signals.json.lock");
    fs::create_dir(&lock).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::open(&lock)
        .and_then(|h| h.set_modified(past))
        .unwrap();
    let ids: Vec<String> = (0..50).map(|i| format!("fact-{i}")).collect();

    // Act
    let started = std::time::Instant::now();
    bump_hits(&mem_dir, &ids);
    let elapsed = started.elapsed();

    // Assert
    assert!(elapsed.as_secs() < 2, "stale lock blocked: {elapsed:?}");
    let store = read_store(&mem_dir);
    assert_eq!(hits_for(&store, "fact-0"), 1);
    assert_eq!(hits_for(&store, "fact-49"), 1);
    assert!(!lock.exists(), "stale lock should be cleared and released");

    let _ = fs::remove_dir_all(&mem_dir);
}
