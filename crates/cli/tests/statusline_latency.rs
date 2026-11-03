// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The budget line must stay cheap and must not change with a large Claude
//! Code config file next to it. The output check is deterministic and always
//! runs. The timing check is a release-only median in its own test binary, so
//! no sibling test shares the CPU: run it with
//! `cargo test --release -p playbook --test statusline_latency`.

mod common;

use common::{strip_ansi, Scratch};
use playbook::statusline::{budget, render, Env};
use std::time::{Duration, Instant};

const NOW: i64 = 1_000_000;
const DAY_SECS: i64 = 86_400;
/// Larger than the 512 KB the monthly reader will accept.
const LARGE_FILE_BYTES: usize = 600 * 1024;
const WARM_UPS: usize = 20;
const RUNS: usize = 200;
/// The budget for the feature's own share of one statusline run.
const MEDIAN_LIMIT: Duration = Duration::from_millis(5);

/// A 7 day window that is reached, with a day left before it resets.
fn alarm_input(cwd: &str) -> String {
    format!(
        r#"{{"cwd":"{cwd}","rate_limits":{{"seven_day":{{"used_percentage":100,"resets_at":{}}}}}}}"#,
        NOW + DAY_SECS
    )
}

#[test]
fn a_large_claude_json_does_not_change_the_seven_day_alarm() {
    let s = Scratch::new();
    let home = s.0.to_str().unwrap();
    let padding = "x".repeat(LARGE_FILE_BYTES);
    let file = format!(
        r#"{{"cachedUsageUtilization":{{"fetchedAt":{},"utilization":{{}}}},"padding":"{padding}"}}"#,
        NOW * 1000
    );
    std::fs::write(s.0.join(".claude.json"), file).unwrap();
    let env = Env::from_pairs([
        ("HOME", home),
        ("CLAUDE_CONFIG_DIR", home),
        ("PWD", home),
        ("PATH", ""),
    ]);

    let out = String::from_utf8(render(&alarm_input(home), &env, NOW)).unwrap();

    let plain = strip_ansi(&out);
    assert_eq!(plain.lines().last(), Some("! 7d CAP HIT"), "{plain:?}");
}

#[test]
#[cfg_attr(debug_assertions, ignore = "timing needs a release build")]
fn building_the_seven_day_alarm_takes_under_five_milliseconds_at_the_median() {
    let input = alarm_input("/tmp");
    for _ in 0..WARM_UPS {
        assert_eq!(budget::build(&input, NOW).len(), 1);
    }
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            let built = budget::build(&input, NOW);
            let took = start.elapsed();
            assert_eq!(built.len(), 1);
            took
        })
        .collect();
    times.sort();
    let median = times[RUNS / 2];
    println!("median budget build: {median:?}");
    assert!(median < MEDIAN_LIMIT, "median {median:?}");
}
