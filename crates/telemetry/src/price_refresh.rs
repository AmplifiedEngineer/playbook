// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The daily refresh of the price table. The terminal view starts one
//! background download at most once a day, keeps a trimmed copy in the cache
//! folder and the next run prices from whichever copy is newer, that one or
//! the snapshot built into the program. A failed download changes nothing, so
//! pbtop works offline. `usage.priceRefresh` set to `off` stops the download.
//!
//! The download shells out to `curl`, like this crate does for `git`: the
//! crate carries no HTTP client (a test pins that), and one request a day does
//! not justify a TLS stack.

use crate::price_table::{self, PriceTable};
use crate::util::atomic::{ensure_private_dir, write_atomic_private};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The setting that turns the download off.
pub const SETTING: &str = "usage.priceRefresh";
const SETTING_OFF: &str = "off";

const SECONDS_PER_DAY: i64 = 86_400;
const FETCH_TIMEOUT_SECONDS: &str = "20";
/// The full list is about 4 MB; refuse anything far past that.
const MAX_DOWNLOAD_BYTES: &str = "20000000";
const STAMP_EXTENSION: &str = "stamp";

/// Whether the setting allows the download.
pub fn enabled(home: &Path) -> bool {
    !matches!(
        crate::host::config_value(SETTING, home),
        Some(Value::String(v)) if v == SETTING_OFF
    )
}

fn stamp_of(cache: &Path) -> PathBuf {
    cache.with_extension(STAMP_EXTENSION)
}

/// True when no attempt is recorded in the last day. The stamp holds the
/// epoch seconds of the last attempt, so a failing download is not retried
/// until the next day either.
pub fn due(cache: &Path, now: i64) -> bool {
    std::fs::read_to_string(stamp_of(cache))
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok())
        .is_none_or(|last| now - last >= SECONDS_PER_DAY)
}

/// The `curl` arguments. Both `--proto` and `--proto-redir` allow only https,
/// so neither the first request nor a redirect can fall back to plain HTTP.
fn curl_args() -> [&'static str; 13] {
    [
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-time",
        FETCH_TIMEOUT_SECONDS,
        "--max-filesize",
        MAX_DOWNLOAD_BYTES,
        price_table::SOURCE_URL,
    ]
}

/// Downloads the full list with `curl`.
pub fn curl_fetch() -> Result<String, String> {
    let out = Command::new("curl")
        .args(curl_args())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl failed with {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|_| "the price list is not text".to_string())
}

/// Runs one refresh when it is due: records the attempt, fetches, and writes a
/// trimmed copy to `cache` when the answer is a believable price list.
/// Returns whether the cache was replaced.
pub fn refresh(
    cache: &Path,
    now: i64,
    fetch: impl FnOnce() -> Result<String, String>,
) -> Result<bool, String> {
    if !due(cache, now) {
        return Ok(false);
    }
    let dir = cache.parent().ok_or("the price cache has no folder")?;
    ensure_private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    write_atomic_private(&stamp_of(cache), now.to_string())
        .map_err(|e| format!("cannot write the price stamp: {e}"))?;
    let trimmed = price_table::trim(&fetch()?, now)?;
    if !PriceTable::parse(&trimmed)?.is_plausible() {
        return Err("the downloaded price list has too few models".to_string());
    }
    write_atomic_private(cache, trimmed)
        .map_err(|e| format!("cannot write the price cache: {e}"))?;
    Ok(true)
}

/// Starts the refresh on a background thread when the setting allows it. The
/// thread never blocks the screen and its failure is not reported: the table
/// that is already in use stays.
pub fn spawn(home: &Path, cache: &Path) {
    if !enabled(home) || !due(cache, crate::util::time::now_secs()) {
        return;
    }
    let cache = cache.to_path_buf();
    std::thread::spawn(move || {
        let _ = refresh(&cache, crate::util::time::now_secs(), curl_fetch);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::scratch_dir;

    const NOW: i64 = 1_800_000_000;

    fn list(models: usize) -> String {
        let entries: Vec<String> = (0..models)
            .map(|i| {
                format!(
                    r#""claude-m{i}":{{"litellm_provider":"anthropic","input_cost_per_token":1e-6,"output_cost_per_token":2e-6}}"#
                )
            })
            .collect();
        format!("{{{}}}", entries.join(","))
    }

    fn turn_off(home: &Path) {
        let config = home.join(".config").join("playbook");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.json"),
            r#"{"usage":{"priceRefresh":"off"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_refresh_is_due_with_no_stamp_and_again_a_day_after_the_last_attempt() {
        let dir = scratch_dir("price-due");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("prices.json");

        assert!(due(&cache, NOW));
        std::fs::write(stamp_of(&cache), NOW.to_string()).unwrap();
        assert!(!due(&cache, NOW + SECONDS_PER_DAY - 1));
        assert!(due(&cache, NOW + SECONDS_PER_DAY));
        std::fs::write(stamp_of(&cache), "garbage").unwrap();
        assert!(due(&cache, NOW));
    }

    #[test]
    fn a_good_download_is_trimmed_into_the_cache_and_stamped() {
        let dir = scratch_dir("price-ok");
        let cache = dir.join("cache").join("prices.json");

        let wrote = refresh(&cache, NOW, || Ok(list(12))).unwrap();

        assert!(wrote);
        let table = PriceTable::parse(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!((table.len(), table.snapshot_epoch), (12, NOW));
        assert!(!due(&cache, NOW));
    }

    #[test]
    fn a_second_refresh_inside_the_day_does_not_fetch() {
        let dir = scratch_dir("price-twice");
        let cache = dir.join("prices.json");
        refresh(&cache, NOW, || Ok(list(12))).unwrap();

        let again = refresh(&cache, NOW + 60, || panic!("fetched twice in a day"));

        assert_eq!(again, Ok(false));
    }

    #[test]
    fn a_failed_or_implausible_download_keeps_the_old_cache_and_waits_a_day() {
        let dir = scratch_dir("price-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("prices.json");
        std::fs::write(&cache, "old").unwrap();

        let failed = refresh(&cache, NOW, || Err("offline".into()));
        let tiny = refresh(&cache, NOW + SECONDS_PER_DAY, || Ok(list(2)));
        let junk = refresh(&cache, NOW + 2 * SECONDS_PER_DAY, || Ok("nope".into()));

        assert_eq!(failed, Err("offline".to_string()));
        assert!(tiny.is_err());
        assert!(junk.is_err());
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), "old");
        assert!(!due(&cache, NOW + 2 * SECONDS_PER_DAY));
    }

    #[test]
    fn curl_allows_only_https_for_the_request_and_for_redirects() {
        let args = curl_args();

        for flag in ["--proto", "--proto-redir"] {
            let at = args.iter().position(|a| *a == flag).unwrap();
            assert_eq!(args[at + 1], "=https", "{flag}");
        }
        assert_eq!(args.last().copied(), Some(price_table::SOURCE_URL));
        assert!(price_table::SOURCE_URL.starts_with("https://"));
    }

    #[test]
    fn the_setting_turns_the_download_off() {
        let home = scratch_dir("price-setting");
        crate::host::set_test_home(&home);

        assert!(enabled(&home));
        turn_off(&home);
        assert!(!enabled(&home));
    }

    #[test]
    fn spawn_with_the_setting_off_writes_nothing() {
        let home = scratch_dir("price-spawn-off");
        crate::host::set_test_home(&home);
        turn_off(&home);
        let cache = home.join("cache").join("prices.json");

        spawn(&home, &cache);

        assert!(!home.join("cache").exists());
    }
}
