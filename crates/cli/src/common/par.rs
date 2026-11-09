// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Bounded fan-out over `std::thread::scope`. No runtime, no extra crates: the
//! callers wait on child processes (`gh`, `git`), so a few plain threads are
//! enough. Results come back in input order, so output stays deterministic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// Most calls in flight at once: enough to overlap the waits, low enough to
/// stay clear of `gh` rate limits and a loaded machine.
pub const MAX_CONCURRENT: usize = 4;

/// `f` over every item with at most `cap` threads, results in input order.
/// A panic in `f` propagates to the caller, as it would in a plain loop.
pub fn map<I, T, F>(items: &[I], cap: usize, f: F) -> Vec<T>
where
    I: Sync,
    T: Send,
    F: Fn(&I) -> T + Sync,
{
    let workers = cap.max(1).min(items.len());
    if workers <= 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<T>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let value = f(item);
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
            });
        }
    });
    #[allow(
        clippy::expect_used,
        reason = "every index below `items.len()` is claimed once and filled before the scope ends"
    )]
    let filled = |slot: Mutex<Option<T>>| {
        slot.into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .expect("every slot is filled before the scope ends")
    };
    slots.into_iter().map(filled).collect()
}

/// [`map`] for an `f` that can fail. A panic in `f` becomes that item's `Err`
/// instead of reaching the caller, so one broken call never hides the others.
/// The default panic hook still prints the panic message to standard error.
pub fn try_map<I, T, F>(items: &[I], cap: usize, f: F) -> Vec<Result<T, String>>
where
    I: Sync,
    T: Send,
    F: Fn(&I) -> Result<T, String> + Sync,
{
    map(items, cap, |item| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item)))
            .unwrap_or_else(|payload| Err(panic_text(payload.as_ref())))
    })
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    let what = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    format!("a parallel call panicked: {what}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn results_keep_input_order_whatever_the_finish_order() {
        let items: Vec<u64> = (0..12).collect();
        let out = map(&items, 4, |n| {
            std::thread::sleep(Duration::from_millis((12 - n) * 2));
            n * 10
        });
        assert_eq!(out, (0..12).map(|n| n * 10).collect::<Vec<_>>());
    }

    #[test]
    fn no_more_than_the_cap_run_at_once() {
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let items = [0u8; 16];
        map(&items, 3, |_| {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            live.fetch_sub(1, Ordering::SeqCst);
        });
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert!(peak.load(Ordering::SeqCst) >= 2, "work never overlapped");
    }

    #[test]
    fn empty_and_single_inputs_work_without_threads() {
        assert!(map::<u8, u8, _>(&[], 4, |n| *n).is_empty());
        assert_eq!(map(&[5u8], 4, |n| n + 1), vec![6]);
    }

    #[test]
    fn a_cap_of_zero_still_runs_everything() {
        assert_eq!(map(&[1u8, 2, 3], 0, |n| *n), vec![1, 2, 3]);
    }

    /// Holds each item until the item after it has finished, so the finish
    /// order is the reverse of the input order without any sleep. It also
    /// proves the items overlap: one at a time would never get past item 0.
    struct ReverseGate {
        done: Mutex<Vec<bool>>,
        changed: std::sync::Condvar,
    }

    /// Longest the gate waits before it gives up and fails the test.
    const GATE_LIMIT: Duration = Duration::from_secs(10);

    impl ReverseGate {
        fn new(len: usize) -> Self {
            Self {
                done: Mutex::new(vec![false; len]),
                changed: std::sync::Condvar::new(),
            }
        }

        fn pass(&self, i: usize) {
            let mut done = self.done.lock().unwrap();
            if i + 1 < done.len() {
                let (guard, timeout) = self
                    .changed
                    .wait_timeout_while(done, GATE_LIMIT, |d| !d[i + 1])
                    .unwrap();
                assert!(!timeout.timed_out(), "item {i} never saw item {}", i + 1);
                done = guard;
            }
            done[i] = true;
            self.changed.notify_all();
        }
    }

    #[test]
    fn try_map_keeps_input_order_when_the_items_finish_in_reverse() {
        let items: Vec<usize> = (0..4).collect();
        let gate = ReverseGate::new(items.len());

        let out = try_map(&items, items.len(), |&i| {
            gate.pass(i);
            Ok(i * 10)
        });

        assert_eq!(out, vec![Ok(0), Ok(10), Ok(20), Ok(30)]);
    }

    #[test]
    fn try_map_gives_the_same_results_as_a_plain_loop() {
        let items: Vec<u32> = (0..9).collect();
        let call = |n: &u32| {
            if n % 4 == 3 {
                Err(format!("odd {n}"))
            } else {
                Ok(n * 2)
            }
        };

        let parallel = try_map(&items, 4, call);
        let sequential: Vec<_> = items.iter().map(call).collect();

        assert_eq!(parallel, sequential);
        assert_eq!(parallel[3], Err("odd 3".to_string()));
        assert_eq!(parallel[4], Ok(8));
    }

    #[test]
    fn a_failure_in_one_item_leaves_the_others_in_place() {
        let out = try_map(&[1u8, 2, 3], 3, |n| {
            if *n == 2 {
                Err("two failed".to_string())
            } else {
                Ok(*n)
            }
        });

        assert_eq!(out, vec![Ok(1), Err("two failed".to_string()), Ok(3)]);
    }

    #[test]
    fn a_panic_in_one_item_becomes_that_items_error() {
        let out = try_map(&[1u8, 2, 3], 3, |n| {
            assert_ne!(*n, 2, "boom at two");
            Ok(*n)
        });

        assert_eq!(out[0], Ok(1));
        assert_eq!(out[2], Ok(3));
        let Err(text) = &out[1] else {
            panic!("item 2 should fail: {out:?}");
        };
        assert!(text.contains("boom at two"), "{text}");
    }

    #[test]
    fn try_map_never_runs_more_than_the_cap_at_once() {
        const CAP: usize = 3;
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let items = [0u8; 12];

        try_map(&items, CAP, |_| {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            let waited = std::time::Instant::now();
            while peak.load(Ordering::SeqCst) < CAP && waited.elapsed() < GATE_LIMIT {
                std::thread::yield_now();
            }
            live.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        });

        assert_eq!(peak.load(Ordering::SeqCst), CAP);
    }
}
