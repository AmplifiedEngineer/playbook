// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reads the newest main-session assistant turn from the tail of a Claude
//! Code transcript: when it was written, which model answered and which cache
//! tier the session last wrote. The status line uses it to tell whether the
//! prompt cache is still warm. Only the end of the file is read, in at most
//! two windows. Any I/O or parse problem is `None`, never an error.

use crate::usage::claude_code::{one_hour_cache_tokens, token};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// First window read from the end of the file. It holds the newest turn in
/// almost every session, so most renders read only this much.
const TAIL_FIRST_BYTES: u64 = 256 * 1024;

/// Second, larger window, read only when the first has no counted record or
/// none that wrote cache. Same size as the tail in `hooks::context_hygiene`.
const TAIL_CAP_BYTES: u64 = 2 * 1024 * 1024;

/// How long a cache write stays readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTier {
    FiveMinutes,
    OneHour,
}

/// The newest counted main-session assistant record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTurn {
    /// Unix seconds of the record.
    pub at: i64,
    /// The record's `message.model`.
    pub model: String,
    /// Tier of the newest record in the window that wrote cache.
    pub tier: Option<CacheTier>,
}

/// The newest counted turn in the transcript at `path`, or `None` when the
/// file cannot be read or holds none in the tail windows.
pub fn last_main_turn(path: &Path) -> Option<LastTurn> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let first = scan(&read_window(&mut file, len, TAIL_FIRST_BYTES)?);
    if matches!(&first, Some(turn) if turn.tier.is_some()) || len <= TAIL_FIRST_BYTES {
        return first;
    }
    scan(&read_window(&mut file, len, TAIL_CAP_BYTES)?)
}

/// The last `cap` bytes of the file as text, without a partial first line
/// when the window starts mid file.
fn read_window(file: &mut File, len: u64, cap: u64) -> Option<String> {
    let start = len.saturating_sub(cap);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        Some(
            text.split_once('\n')
                .map_or(String::new(), |(_, rest)| rest.to_string()),
        )
    } else {
        Some(text)
    }
}

/// Walks the lines newest first. `at` and `model` come from the newest
/// counted record, `tier` from the newest one that wrote cache.
fn scan(body: &str) -> Option<LastTurn> {
    let mut turn: Option<LastTurn> = None;
    for line in body.lines().rev() {
        let Some((at, model, tier)) = counted(line) else {
            continue;
        };
        match &mut turn {
            None => turn = Some(LastTurn { at, model, tier }),
            Some(found) if found.tier.is_none() => found.tier = tier,
            Some(_) => {}
        }
        if matches!(&turn, Some(found) if found.tier.is_some()) {
            break;
        }
    }
    turn
}

/// A record counts when it is a main-session assistant message with input
/// tokens and a parseable timestamp (the filter of `context_hygiene`).
fn counted(line: &str) -> Option<(i64, String, Option<CacheTier>)> {
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    if record.get("type")?.as_str()? != "assistant"
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let message = record.get("message")?;
    let usage = message.get("usage")?;
    let created = token(usage, "cache_creation_input_tokens");
    let input = token(usage, "input_tokens") + created + token(usage, "cache_read_input_tokens");
    if input == 0 {
        return None;
    }
    let at = crate::common::time::parse_iso8601_utc_epoch(record.get("timestamp")?.as_str()?)?;
    let model = message.get("model")?.as_str()?.to_string();
    let tier = if one_hour_cache_tokens(usage) > 0 {
        Some(CacheTier::OneHour)
    } else if created > 0 {
        Some(CacheTier::FiveMinutes)
    } else {
        None
    };
    Some((at, model, tier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A per-test scratch directory, removed on drop so a failing test
    /// cleans up too.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "transcript-tail-{name}-{}",
                crate::testing::run_id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, lines: &[String]) -> PathBuf {
            let path = self.0.join("t.jsonl");
            let mut text = lines.join("\n");
            text.push('\n');
            std::fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const T0: &str = "2026-11-10T22:56:49.046Z";
    const T0_EPOCH: i64 = 1_794_351_409;
    const T1: &str = "2026-11-10T22:56:59.000Z";
    const T1_EPOCH: i64 = T0_EPOCH + 10;

    fn assistant(
        ts: &str,
        model: &str,
        write_5m: u64,
        write_1h: u64,
        read: u64,
        side: bool,
    ) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":{side},"timestamp":"{ts}","message":{{"model":"{model}","usage":{{"input_tokens":3,"cache_creation_input_tokens":{},"cache_read_input_tokens":{read},"cache_creation":{{"ephemeral_5m_input_tokens":{write_5m},"ephemeral_1h_input_tokens":{write_1h}}}}}}}}}"#,
            write_5m + write_1h
        )
    }

    fn rec(ts: &str, write_5m: u64, write_1h: u64) -> String {
        assistant(ts, "claude-opus-4-8", write_5m, write_1h, 100, false)
    }

    fn user_lines(bytes: usize) -> Vec<String> {
        let line = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "x".repeat(990)
        );
        vec![line; bytes.div_ceil(1024)]
    }

    fn turn(at: i64, tier: Option<CacheTier>) -> Option<LastTurn> {
        Some(LastTurn {
            at,
            model: "claude-opus-4-8".to_string(),
            tier,
        })
    }

    #[test]
    fn one_hour_write_gives_the_exact_time_model_and_tier() {
        let s = Scratch::new("one-hour");
        let p = s.write(&[rec(T0, 0, 500)]);
        assert_eq!(last_main_turn(&p), turn(T0_EPOCH, Some(CacheTier::OneHour)));
    }

    #[test]
    fn five_minute_write_gives_the_five_minute_tier() {
        let s = Scratch::new("five-min");
        let p = s.write(&[rec(T0, 500, 0)]);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn the_tier_comes_from_an_older_record_but_the_time_from_the_newest() {
        let s = Scratch::new("older-tier");
        let p = s.write(&[rec(T0, 0, 500), rec(T1, 0, 0)]);
        assert_eq!(last_main_turn(&p), turn(T1_EPOCH, Some(CacheTier::OneHour)));
    }

    #[test]
    fn no_cache_write_anywhere_gives_no_tier() {
        let s = Scratch::new("no-tier");
        let p = s.write(&[rec(T0, 0, 0), rec(T1, 0, 0)]);
        assert_eq!(last_main_turn(&p), turn(T1_EPOCH, None));
    }

    #[test]
    fn a_tier_behind_300_kib_is_found_in_the_second_window() {
        let s = Scratch::new("tier-second-window");
        let mut lines = vec![rec(T0, 0, 500)];
        lines.extend(user_lines(300 * 1024));
        lines.push(rec(T1, 0, 0));
        let p = s.write(&lines);
        assert_eq!(last_main_turn(&p), turn(T1_EPOCH, Some(CacheTier::OneHour)));
    }

    #[test]
    fn a_sidechain_record_is_ignored() {
        let s = Scratch::new("sidechain");
        let side = assistant(T1, "claude-opus-4-8", 0, 500, 100, true);
        let p = s.write(&[rec(T0, 500, 0), side]);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn an_all_zero_synthetic_record_is_ignored() {
        let s = Scratch::new("synthetic");
        let zero = r#"{"type":"assistant","timestamp":"2026-11-10T22:56:59.000Z","message":{"model":"<synthetic>","usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let p = s.write(&[rec(T0, 500, 0), zero.to_string()]);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn the_newest_record_behind_300_kib_is_found_in_the_second_window() {
        let s = Scratch::new("behind-300k");
        let mut lines = vec![rec(T0, 500, 0)];
        lines.extend(user_lines(300 * 1024));
        let p = s.write(&lines);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn the_newest_record_behind_1_5_mib_is_found_inside_the_cap() {
        let s = Scratch::new("behind-1-5m");
        let mut lines = vec![rec(T0, 500, 0)];
        lines.extend(user_lines(1536 * 1024));
        let p = s.write(&lines);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn a_record_past_the_cap_is_not_found() {
        let s = Scratch::new("past-cap");
        let mut lines = vec![rec(T0, 500, 0)];
        lines.extend(user_lines(3 * 1024 * 1024));
        let p = s.write(&lines);
        assert_eq!(last_main_turn(&p), None);
    }

    #[test]
    fn a_partial_first_line_is_dropped_and_the_record_is_found_whole_later() {
        let s = Scratch::new("partial-line");
        let big = assistant(T0, &"m".repeat(45 * 1024), 500, 0, 100, false);
        let mut lines = vec![big.clone()];
        lines.extend(user_lines(250 * 1024));
        let p = s.write(&lines);
        let len = std::fs::metadata(&p).unwrap().len();
        // The first window starts inside the big record, so it sees no record.
        assert!(big.len() as u64 > len - TAIL_FIRST_BYTES);
        let found = last_main_turn(&p).unwrap();
        assert_eq!(found.at, T0_EPOCH);
        assert_eq!(found.tier, Some(CacheTier::FiveMinutes));
    }

    #[test]
    fn a_missing_file_and_an_empty_file_give_none() {
        let s = Scratch::new("missing-empty");
        assert_eq!(last_main_turn(&s.0.join("nope.jsonl")), None);
        let p = s.0.join("empty.jsonl");
        std::fs::write(&p, "").unwrap();
        assert_eq!(last_main_turn(&p), None);
    }

    #[test]
    fn a_bad_json_line_is_skipped() {
        let s = Scratch::new("bad-json");
        let p = s.write(&[rec(T0, 500, 0), "{not json".to_string()]);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }

    #[test]
    fn a_record_with_an_unparseable_timestamp_is_skipped() {
        let s = Scratch::new("bad-ts");
        let p = s.write(&[rec(T0, 500, 0), rec("yesterday", 500, 0)]);
        assert_eq!(
            last_main_turn(&p),
            turn(T0_EPOCH, Some(CacheTier::FiveMinutes))
        );
    }
}
