// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `rate_limits.jsonl`: the readings of the 5 hour and weekly windows over the
//! last days, one JSON line each, so the burn-up of a window can draw the
//! curve it really followed instead of a straight line. The statusline writes
//! it next to `rate_limits.json` (see `limits_source::remember`); only the usage
//! views read it. A line holds a time and two percentages: no account, no
//! token, no session id.
//!
//! The file is small by construction: a line is added at most once in 5
//! minutes and only when a percentage changed, lines older than 8 days go, and
//! the file never passes 256 KiB. The statusline runs this on every render, so
//! the common case reads the first and last line only and appends one line.
//! Everything is best effort: a failure here never reaches the statusline.

use crate::limits::LimitPeriod;
use crate::util::atomic::{write_atomic_private, write_atomic_private_unsynced};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// The largest the file may be. A bigger file is ignored by readers and
/// replaced by the next write.
pub const MAX_BYTES: u64 = 256 * 1024;
/// Lines older than this (8 days, the weekly window and a day) are dropped.
pub const KEEP_SECONDS: i64 = 8 * 24 * 3600;
/// The fewest seconds between two lines.
pub const MIN_GAP_SECONDS: i64 = 300;
/// The oldest line may be this much past [`KEEP_SECONDS`] before a write
/// rewrites the file, so the rewrite happens about once a day and not on every
/// write once the file holds 8 days.
const TRIM_SLACK_SECONDS: i64 = 24 * 3600;
/// A trim leaves the file at this size, so the next writes append again.
const TRIM_TO_BYTES: u64 = MAX_BYTES / 4 * 3;
/// How much of the start and the end of the file one write looks at: more
/// than the longest line.
const EDGE_BYTES: u64 = 512;

/// The format version written on every line as `v`. A line without `v` is an
/// older line and is read as this version; a line with a newer `v` is skipped.
pub const LINE_VERSION: i64 = 1;

/// One line of the file.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Reading {
    t: i64,
    five_hour: Option<f64>,
    seven_day: Option<f64>,
}

/// What a write needs to know of the file without reading all of it.
struct Edges {
    first: Option<Reading>,
    last: Option<Reading>,
    ends_clean: bool,
}

/// `rate_limits.jsonl` in the usage folder.
pub fn history_path(usage_dir: &Path) -> PathBuf {
    usage_dir.join("rate_limits.jsonl")
}

fn percent(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64().filter(|p| p.is_finite() && *p >= 0.0)
}

fn parse_line(line: &str) -> Option<Reading> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    // No `v` is a line from before the field existed: same shape as version 1.
    if v.get("v").is_some_and(|n| n.as_i64() != Some(LINE_VERSION)) {
        return None;
    }
    Some(Reading {
        t: v.get("t")?.as_i64().filter(|t| *t > 0)?,
        five_hour: percent(&v, "five_hour"),
        seven_day: percent(&v, "seven_day"),
    })
}

fn line_of(r: &Reading) -> String {
    format!(
        "{}\n",
        json!({"v": LINE_VERSION, "t": r.t, "five_hour": r.five_hour, "seven_day": r.seven_day})
    )
}

/// The first and the last line and whether the file ends in a newline, from
/// two short reads. A line is `None` when it is not whole and valid.
fn edges(path: &Path, len: u64) -> Option<Edges> {
    let mut f = fs::File::open(path).ok()?;
    let mut head = vec![0; usize::try_from(len.min(EDGE_BYTES)).ok()?];
    f.read_exact(&mut head).ok()?;
    let first = std::str::from_utf8(&head)
        .ok()
        .and_then(|s| s.split_once('\n'))
        .and_then(|(line, _)| parse_line(line));
    let from = len.saturating_sub(EDGE_BYTES);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = vec![0; usize::try_from(len - from).ok()?];
    f.read_exact(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let body = text.trim_end_matches('\n');
    let last = match body.rsplit_once('\n') {
        Some((_, line)) => parse_line(line),
        // Only a whole file in the window is a whole line.
        None if from == 0 => parse_line(body),
        None => None,
    };
    Some(Edges {
        first,
        last,
        ends_clean: tail.last() == Some(&b'\n'),
    })
}

/// Adds a line for the windows the merge accepted from a statusline reading,
/// read at `now`, when the last line is at least [`MIN_GAP_SECONDS`] older and
/// one of those percentages changed. A window the reading lacked, or whose
/// figure was dropped as stale, is `None` and written as `null`. Drops old
/// lines and keeps the file under [`MAX_BYTES`]. Does nothing when both are
/// `None`.
pub fn record(usage_dir: &Path, five_hour: Option<f64>, seven_day: Option<f64>, now: i64) {
    if five_hour.is_none() && seven_day.is_none() {
        return;
    }
    let new = Reading {
        t: now,
        five_hour,
        seven_day,
    };
    let _ = append(&history_path(usage_dir), &new);
}

/// Whether every window `new` carries has the value the line `last` has.
fn same_as(last: &Reading, new: &Reading) -> bool {
    [
        (new.five_hour, last.five_hour),
        (new.seven_day, last.seven_day),
    ]
    .iter()
    .all(|(n, l)| n.is_none() || n == l)
}

fn append(path: &Path, new: &Reading) -> std::io::Result<()> {
    let line = line_of(new);
    let len = match fs::metadata(path) {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return Ok(()),
        Err(_) => 0,
    };
    if len == 0 || len > MAX_BYTES {
        // New, or too big to trust: start again from this line.
        return write_atomic_private(path, line);
    }
    let Some(edges) = edges(path, len) else {
        return Ok(());
    };
    // A last line in the future means the clock went back: write anyway, and
    // drop the lines from the future so the file stays in time order.
    let future = edges.last.is_some_and(|l| l.t > new.t);
    if let Some(last) = edges.last.filter(|_| !future) {
        if new.t - last.t < MIN_GAP_SECONDS || same_as(&last, new) {
            return Ok(());
        }
    }
    let cutoff = new.t - KEEP_SECONDS;
    let old = edges
        .first
        .is_none_or(|f| f.t < cutoff - TRIM_SLACK_SECONDS);
    if edges.ends_clean && !old && !future && len + line.len() as u64 <= MAX_BYTES {
        let mut f = OpenOptions::new().append(true).open(path)?;
        return f.write_all(line.as_bytes());
    }
    trim_and_append(path, cutoff, future.then_some(new.t), &line)
}

/// Rewrites the file without the lines older than `cutoff`, without those
/// after `until` when it is given, and without the oldest ones until it fits
/// [`TRIM_TO_BYTES`], then adds `line`.
fn trim_and_append(
    path: &Path,
    cutoff: i64,
    until: Option<i64>,
    line: &str,
) -> std::io::Result<()> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    let mut kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // The lines are in time order, so the old ones are at the front.
    let old = kept
        .iter()
        .take_while(|l| parse_line(l).is_none_or(|r| r.t < cutoff))
        .count();
    kept.drain(..old);
    if let Some(until) = until {
        kept.retain(|l| parse_line(l).is_some_and(|r| r.t <= until));
    }
    let mut size: u64 = kept.iter().map(|l| l.len() as u64 + 1).sum::<u64>() + line.len() as u64;
    let mut skip = 0;
    while size > TRIM_TO_BYTES && skip < kept.len() {
        size -= kept[skip].len() as u64 + 1;
        skip += 1;
    }
    let mut out = String::with_capacity(usize::try_from(size).unwrap_or(0));
    for l in &kept[skip..] {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(line);
    write_atomic_private_unsynced(path, out)
}

type Readings = Vec<(LimitPeriod, i64, f64)>;

/// The last file `load` parsed and what it made of it, keyed by path, modified
/// time and size, so a refresh that finds the file unchanged parses nothing.
struct Cached {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    readings: Readings,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// How many times `load` parsed the file on this thread.
    static PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn parse_all(bytes: &[u8]) -> Readings {
    #[cfg(test)]
    PARSES.with(|p| p.set(p.get() + 1));
    let mut out: Readings = String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(parse_line)
        .flat_map(|r| {
            [
                r.five_hour.map(|p| (LimitPeriod::FiveHour, r.t, p)),
                r.seven_day.map(|p| (LimitPeriod::SevenDay, r.t, p)),
            ]
        })
        .flatten()
        .collect();
    out.sort_by_key(|(_, t, _)| *t);
    out
}

/// The readings in the file as `(window, time, percent)`, oldest first. An
/// unreadable or oversized file gives none, and a line that is not valid is
/// skipped. The parsed result is kept until the file's modified time or size
/// changes, because the TUI asks on every refresh.
pub fn load(usage_dir: &Path) -> Readings {
    let path = history_path(usage_dir);
    let Ok(meta) = fs::metadata(&path) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Vec::new();
    }
    let (modified, len) = (meta.modified().ok(), meta.len());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache
        .as_ref()
        .filter(|c| c.path == path && c.modified == modified && c.len == len)
    {
        return c.readings.clone();
    }
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    let readings = parse_all(&bytes);
    // A file without a modified time cannot tell a change apart: do not keep it.
    *cache = modified.map(|modified| Cached {
        path,
        modified: Some(modified),
        len,
        readings: readings.clone(),
    });
    readings
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pb-history-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn lines(d: &Path) -> Vec<String> {
        fs::read_to_string(history_path(d))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn times(d: &Path) -> Vec<i64> {
        lines(d)
            .iter()
            .filter_map(|l| parse_line(l))
            .map(|r| r.t)
            .collect()
    }

    #[test]
    fn a_line_holds_only_the_version_the_time_and_the_two_percentages() {
        let d = dir("shape");

        record(&d, Some(12.5), None, NOW);

        assert_eq!(
            lines(&d),
            vec![format!(
                r#"{{"v":1,"t":{NOW},"five_hour":12.5,"seven_day":null}}"#
            )]
        );
    }

    #[test]
    fn a_line_from_before_the_version_field_is_still_read() {
        let d = dir("legacy");
        let old = format!(
            "{{\"t\":{},\"five_hour\":5.0,\"seven_day\":null}}\n",
            NOW - 1000
        );
        fs::write(history_path(&d), old).unwrap();

        record(&d, Some(6.0), None, NOW);

        assert_eq!(
            load(&d),
            vec![
                (LimitPeriod::FiveHour, NOW - 1000, 5.0),
                (LimitPeriod::FiveHour, NOW, 6.0)
            ]
        );
    }

    #[test]
    fn a_line_with_a_newer_version_is_skipped_by_readers() {
        let d = dir("newer");
        let newer = format!(
            "{{\"v\":2,\"t\":{},\"five_hour\":9.0,\"seven_day\":null}}\n",
            NOW - 1000
        );
        fs::write(history_path(&d), newer).unwrap();

        assert!(load(&d).is_empty());
    }

    #[test]
    fn an_unchanged_file_is_parsed_once_across_loads() {
        let d = dir("cache-hit");
        record(&d, Some(10.0), None, NOW);
        let before = PARSES.with(|p| p.get());

        let first = load(&d);
        let second = load(&d);

        assert_eq!(first, second);
        assert_eq!(first, vec![(LimitPeriod::FiveHour, NOW, 10.0)]);
        assert_eq!(PARSES.with(|p| p.get()) - before, 1);
    }

    #[test]
    fn a_new_line_makes_the_next_load_parse_again() {
        let d = dir("cache-miss");
        record(&d, Some(10.0), None, NOW);
        let _ = load(&d);
        let before = PARSES.with(|p| p.get());

        record(&d, Some(11.0), None, NOW + 300);
        let got = load(&d);

        assert_eq!(got.len(), 2);
        assert_eq!(PARSES.with(|p| p.get()) - before, 1);
    }

    #[test]
    fn a_deleted_file_gives_no_readings_even_after_a_cached_load() {
        let d = dir("cache-gone");
        record(&d, Some(10.0), None, NOW);
        assert_eq!(load(&d).len(), 1);

        fs::remove_file(history_path(&d)).unwrap();

        assert!(load(&d).is_empty());
    }

    #[test]
    fn two_readings_a_minute_apart_leave_one_line() {
        let d = dir("minute");

        record(&d, Some(10.0), Some(20.0), NOW);
        record(&d, Some(11.0), Some(20.0), NOW + 60);

        assert_eq!(times(&d), vec![NOW]);
    }

    #[test]
    fn the_gap_rule_is_299_no_300_yes_and_an_equal_value_never() {
        let d = dir("gap");
        record(&d, Some(10.0), None, NOW);

        record(&d, Some(11.0), None, NOW + 299);
        assert_eq!(times(&d), vec![NOW]);
        record(&d, Some(11.0), None, NOW + 300);
        assert_eq!(times(&d), vec![NOW, NOW + 300]);
        record(&d, Some(11.0), None, NOW + 300 + 301);
        assert_eq!(times(&d), vec![NOW, NOW + 300]);
        record(&d, Some(12.0), Some(1.0), NOW + 300 + 301);
        assert_eq!(times(&d), vec![NOW, NOW + 300, NOW + 601]);
    }

    #[test]
    fn a_change_in_the_other_window_alone_is_a_change() {
        let d = dir("other");
        record(&d, Some(10.0), Some(20.0), NOW);

        record(&d, Some(10.0), Some(21.0), NOW + 300);

        assert_eq!(times(&d), vec![NOW, NOW + 300]);
    }

    #[test]
    fn nothing_is_written_without_a_window() {
        let d = dir("none");

        record(&d, None, None, NOW);

        assert!(!history_path(&d).exists());
    }

    fn reading_at(t: i64, five: f64) -> String {
        line_of(&Reading {
            t,
            five_hour: Some(five),
            seven_day: None,
        })
    }

    #[test]
    fn lines_older_than_8_days_are_pruned_once_the_oldest_is_a_day_past_that() {
        let d = dir("prune");
        let past = NOW - KEEP_SECONDS - TRIM_SLACK_SECONDS - 1;
        let edge = NOW - KEEP_SECONDS;
        let body = [
            reading_at(past, 1.0),
            reading_at(NOW - KEEP_SECONDS - 100, 2.0),
            reading_at(edge, 3.0),
            reading_at(NOW - 600, 4.0),
        ]
        .concat();
        fs::write(history_path(&d), body).unwrap();

        record(&d, Some(5.0), None, NOW);

        assert_eq!(times(&d), vec![edge, NOW - 600, NOW]);
    }

    #[test]
    fn the_file_is_not_rewritten_for_a_line_inside_the_day_of_slack() {
        let d = dir("slack");
        let first = NOW - KEEP_SECONDS - TRIM_SLACK_SECONDS;
        fs::write(
            history_path(&d),
            [reading_at(first, 1.0), reading_at(NOW - 600, 4.0)].concat(),
        )
        .unwrap();

        record(&d, Some(5.0), None, NOW);

        assert_eq!(times(&d), vec![first, NOW - 600, NOW]);
    }

    #[test]
    fn a_window_the_reading_did_not_carry_is_written_as_null_and_does_not_count() {
        let d = dir("carried");
        record(&d, Some(10.0), Some(20.0), NOW);

        record(&d, Some(11.0), None, NOW + 300);
        // the weekly figure of the first line is not a new value of this one
        record(&d, None, Some(20.0), NOW + 600);
        // same as the last line for the window it carries
        record(&d, None, Some(20.0), NOW + 900);

        let got = lines(&d);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[1].contains(r#""seven_day":null"#), "{got:?}");
        assert!(got[2].contains(r#""five_hour":null"#), "{got:?}");
    }

    #[test]
    fn a_clock_that_went_back_does_not_freeze_the_history() {
        let d = dir("clock");
        record(&d, Some(10.0), None, NOW + 86_400);
        record(&d, Some(11.0), None, NOW - 1000);
        assert_eq!(times(&d), vec![NOW - 1000]);

        record(&d, Some(12.0), None, NOW - 1000 + 300);

        assert_eq!(times(&d), vec![NOW - 1000, NOW - 700]);
    }

    #[test]
    fn a_first_line_that_is_not_valid_is_cleaned_out_by_the_next_write() {
        let d = dir("badfirst");
        fs::write(
            history_path(&d),
            format!("garbage\n{}", reading_at(NOW - 600, 4.0)),
        )
        .unwrap();

        record(&d, Some(5.0), None, NOW);

        assert_eq!(lines(&d).len(), 2);
        assert_eq!(times(&d), vec![NOW - 600, NOW]);
    }

    #[test]
    fn a_corrupt_line_is_skipped_by_readers_and_does_not_block_a_write() {
        let d = dir("corrupt");
        let good = line_of(&Reading {
            t: NOW - 1000,
            five_hour: Some(5.0),
            seven_day: None,
        });
        fs::write(history_path(&d), format!("not json\n{good}{{\"t\":\n")).unwrap();

        record(&d, Some(6.0), None, NOW);

        let got = load(&d);
        assert_eq!(
            got,
            vec![
                (LimitPeriod::FiveHour, NOW - 1000, 5.0),
                (LimitPeriod::FiveHour, NOW, 6.0)
            ]
        );
    }

    #[test]
    fn a_file_over_the_cap_is_ignored_by_readers_and_replaced_by_the_next_write() {
        let d = dir("oversize");
        let big = line_of(&Reading {
            t: NOW - 1000,
            five_hour: Some(5.0),
            seven_day: None,
        });
        fs::write(
            history_path(&d),
            big.repeat(MAX_BYTES as usize / big.len() + 2),
        )
        .unwrap();
        assert!(fs::metadata(history_path(&d)).unwrap().len() > MAX_BYTES);
        assert!(load(&d).is_empty());

        record(&d, Some(6.0), None, NOW);

        assert_eq!(times(&d), vec![NOW]);
        assert_eq!(load(&d), vec![(LimitPeriod::FiveHour, NOW, 6.0)]);
    }

    #[test]
    fn a_full_file_is_trimmed_from_the_front_and_never_passes_the_cap() {
        let d = dir("full");
        let one = |t: i64| {
            line_of(&Reading {
                t,
                five_hour: Some(5.0),
                seven_day: Some(7.0),
            })
        };
        let n = MAX_BYTES as usize / one(NOW - 1000).len();
        let body: String = (0..n as i64)
            .map(|i| one(NOW - 1000 - (n as i64 - i)))
            .collect();
        fs::write(history_path(&d), &body).unwrap();
        let before = fs::metadata(history_path(&d)).unwrap().len();
        assert!(before <= MAX_BYTES && before + 80 > MAX_BYTES, "{before}");

        record(&d, Some(6.0), Some(7.0), NOW);

        let after = fs::metadata(history_path(&d)).unwrap().len();
        assert!(after <= TRIM_TO_BYTES, "{after}");
        let t = times(&d);
        assert_eq!(t.last(), Some(&NOW));
        assert!(t.windows(2).all(|w| w[0] < w[1]));
        assert!(t.len() > 1000);
        // the next writes append again, without a rewrite
        record(&d, Some(7.0), Some(7.0), NOW + 300);
        assert_eq!(times(&d).last(), Some(&(NOW + 300)));
    }

    #[test]
    fn a_torn_last_line_is_cleaned_by_the_next_write() {
        let d = dir("torn");
        let good = line_of(&Reading {
            t: NOW - 1000,
            five_hour: Some(5.0),
            seven_day: None,
        });
        fs::write(history_path(&d), format!("{good}{{\"t\":17")).unwrap();

        record(&d, Some(6.0), None, NOW);

        let all = lines(&d);
        assert_eq!(all.len(), 3);
        assert_eq!(times(&d), vec![NOW - 1000, NOW]);
    }

    #[test]
    fn the_cap_and_the_age_are_the_documented_bounds() {
        assert_eq!(MAX_BYTES, 262_144);
        assert_eq!(KEEP_SECONDS, 691_200);
        assert_eq!(MIN_GAP_SECONDS, 300);
    }

    #[test]
    fn an_unwritable_folder_is_ignored() {
        let d = dir("ro");
        let missing = d.join("nope/deeper");

        record(&missing, Some(1.0), None, NOW);

        assert!(!history_path(&missing).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&d, fs::Permissions::from_mode(0o500)).unwrap();
            // a user that can write anyway (root) is not the case under test
            let can_write = fs::write(d.join("probe"), "x").is_ok();
            record(&d, Some(1.0), None, NOW);
            let wrote = history_path(&d).exists();
            fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(can_write || !wrote);
        }
    }

    #[test]
    fn a_new_file_is_private() {
        let d = dir("private");

        record(&d, Some(1.0), None, NOW);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(history_path(&d)).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
    }

    /// How many times the work of copying the file one write may cost.
    const FULL_WRITE_BUDGET: u32 = 4;

    #[test]
    fn a_full_history_costs_a_write_no_more_than_a_few_copies_of_the_file() {
        use std::time::{Duration, Instant};
        let one = |t: i64| reading_line(t, 1.0, 2.0);
        let n = MAX_BYTES as usize / one(NOW - 100_000).len();
        let body: String = (0..n as i64).map(|i| one(NOW - 100_000 + i)).collect();
        // The work a trim cannot avoid: read the whole file, look at every
        // line and write it out again. It is measured next to each timed
        // write, so a loaded machine slows both and the ratio holds.
        let copy = || {
            let d = dir("timing-copy");
            fs::write(history_path(&d), &body).unwrap();
            let start = Instant::now();
            let text = fs::read_to_string(history_path(&d)).unwrap();
            let lines = text.lines().filter_map(parse_line).count();
            fs::write(d.join("copy"), &text).unwrap();
            std::hint::black_box(lines);
            start.elapsed()
        };
        let write = |with_history: bool| {
            let d = dir(&format!("timing-{with_history}"));
            if with_history {
                fs::write(history_path(&d), &body).unwrap();
            } else {
                // a folder in the file's place: the history step does nothing
                fs::create_dir(history_path(&d)).unwrap();
            }
            let start = Instant::now();
            record(&d, Some(55.0), None, NOW);
            start.elapsed()
        };

        let rounds: Vec<_> = (0..7)
            .map(|_| (copy(), write(false), write(true)))
            .collect();

        // The best round of each: an interruption lands in a single round.
        let min = |f: fn(&(Duration, Duration, Duration)) -> Duration| {
            rounds.iter().map(f).min().unwrap()
        };
        let (copy, without, with) = (min(|r| r.0), min(|r| r.1), min(|r| r.2));
        assert!(
            with <= without + copy * FULL_WRITE_BUDGET,
            "with {with:?}, without {without:?}, one copy {copy:?}"
        );
    }

    fn reading_line(t: i64, five: f64, seven: f64) -> String {
        line_of(&Reading {
            t,
            five_hour: Some(five),
            seven_day: Some(seven),
        })
    }
}
