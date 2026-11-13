// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The time zone times are shown in, and the day a time belongs to.
//!
//! Everything that prints a clock time, labels a day, or groups events into
//! daily totals goes through this module, so the whole tool agrees on where a
//! day starts. A "day number" is the count of calendar days since 1970-01-01
//! in the chosen zone, so `aggregate::date_key` turns it into a date as before.
//!
//! The zone is `local` (the default) or `utc`. It is chosen at start-up
//! from, in order: the `--utc` / `--local` flag, the `PLAYBOOK_TELEMETRY_TZ`
//! environment variable, the `usage.timeZone` setting, then `local`. A value
//! that is not one of those two is ignored with a notice, never a panic.
//! pbtop's `z` key switches between the two during a session.

use chrono::{Local, Offset, TimeZone};

use crate::aggregate::SECONDS_PER_DAY;

/// The environment variable that overrides the stored setting.
pub const ENV_VAR: &str = "PLAYBOOK_TELEMETRY_TZ";
/// The setting that holds the default zone.
pub const SETTING: &str = "usage.timeZone";

/// A way to turn an instant into a wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// Coordinated Universal Time.
    Utc,
    /// The time zone of this machine, daylight saving included.
    Local,
    /// A fixed offset east of UTC in seconds, with no daylight saving.
    Fixed(i32),
}

impl Zone {
    /// `utc` or `local`, case and surrounding space ignored.
    pub fn parse(text: &str) -> Option<Zone> {
        match text.trim().to_ascii_lowercase().as_str() {
            "utc" => Some(Zone::Utc),
            "local" => Some(Zone::Local),
            _ => None,
        }
    }

    /// Seconds east of UTC at the instant `ts`. A machine zone that cannot be
    /// read, or an instant chrono cannot place, counts as UTC.
    pub fn offset_at(self, ts: i64) -> i64 {
        match self {
            Zone::Utc => 0,
            Zone::Fixed(secs) => i64::from(secs),
            Zone::Local => Local
                .timestamp_opt(ts, 0)
                .single()
                .map_or(0, |t| i64::from(t.offset().fix().local_minus_utc())),
        }
    }

    /// The calendar day number holding `ts` in this zone.
    pub fn day_of(self, ts: i64) -> i64 {
        day_of_with(&|t| self.offset_at(t), ts)
    }

    /// The first instant of calendar day `day` in this zone. On a day whose
    /// midnight does not exist (a daylight saving jump) this is the first
    /// instant after the jump.
    pub fn day_start(self, day: i64) -> i64 {
        day_start_with(&|t| self.offset_at(t), day)
    }

    /// Seconds into the local day at `ts`.
    fn second_of_day(self, ts: i64) -> i64 {
        ts.saturating_add(self.offset_at(ts))
            .rem_euclid(SECONDS_PER_DAY)
    }

    /// `HH:MM:SS` of `ts` on this zone's wall clock.
    pub fn clock(self, ts: i64) -> String {
        let s = self.second_of_day(ts);
        format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    }

    /// `HH:MM` of `ts` on this zone's wall clock, for narrow places.
    pub fn hour_minute(self, ts: i64) -> String {
        let s = self.second_of_day(ts);
        format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
    }

    /// `YYYY-MM-DDTHH:MM:SS` plus `Z` or a `+HH:MM` offset. A negative `ts`
    /// reads as the epoch.
    pub fn iso(self, ts: i64) -> String {
        let ts = ts.max(0);
        let offset = self.offset_at(ts);
        let local = ts.saturating_add(offset);
        let (y, m, d) = crate::aggregate::civil_from_days(local.div_euclid(SECONDS_PER_DAY));
        let s = local.rem_euclid(SECONDS_PER_DAY);
        let suffix = if offset == 0 {
            "Z".to_string()
        } else {
            let sign = if offset < 0 { '-' } else { '+' };
            let a = offset.abs();
            format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
        };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{suffix}",
            s / 3600,
            s % 3600 / 60,
            s % 60
        )
    }

    /// The offset at `ts` as people write it: `UTC`, `UTC+10`, `UTC+5:30`,
    /// `UTC-3`. Daylight saving moves it through the year.
    pub fn offset_label(self, ts: i64) -> String {
        offset_text(self.offset_at(ts))
    }

    /// What to tell the user about the zone at `ts`: `UTC`, or
    /// `local time (UTC+10)`.
    pub fn describe(self, ts: i64) -> String {
        match self {
            Zone::Utc => "UTC".to_string(),
            Zone::Local | Zone::Fixed(_) => format!("local time ({})", self.offset_label(ts)),
        }
    }

    /// The short name shown beside times: `UTC` or `local`.
    pub fn label(self) -> &'static str {
        match self {
            Zone::Utc => "UTC",
            Zone::Local | Zone::Fixed(_) => "local",
        }
    }
}

/// `secs` east of UTC as `UTC`, `UTC+10`, `UTC+5:30` or `UTC-3`.
fn offset_text(secs: i64) -> String {
    if secs == 0 {
        return "UTC".to_string();
    }
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    match a % 3600 / 60 {
        0 => format!("UTC{sign}{}", a / 3600),
        m => format!("UTC{sign}{}:{m:02}", a / 3600),
    }
}

/// The day number of `ts` for a zone whose offset at an instant is `off`.
fn day_of_with(off: &dyn Fn(i64) -> i64, ts: i64) -> i64 {
    ts.saturating_add(off(ts)).div_euclid(SECONDS_PER_DAY)
}

/// The first instant of `day` for a zone whose offset at an instant is `off`.
fn day_start_with(off: &dyn Fn(i64) -> i64, day: i64) -> i64 {
    let wall = day.saturating_mul(SECONDS_PER_DAY);
    // The offset in force at the guessed midnight, then once more at the
    // instant that gives, which settles days that contain a transition.
    let first = wall.saturating_sub(off(wall));
    let second = wall.saturating_sub(off(first));
    [first, second]
        .into_iter()
        .filter(|t| day_of_with(off, *t) == day)
        .min()
        .unwrap_or(first)
}

/// The process-wide zone state. 0 is UTC, 1 is local, 2 is fixed (offset in
/// `fixed`). Unset reads as UTC so code that never calls `set` (tests,
/// embedders) keeps the old behaviour.
///
/// In the library's own unit tests the state lives per thread, not per
/// process. The test harness runs each test on its own thread, so a test that
/// installs a zone (directly, or through `entry::run` calling `ensure_init`)
/// cannot change the zone another parallel test reads. The flip side: a zone
/// set in a unit test applies to that thread only, and a thread the code under
/// test starts reads UTC.
#[cfg(not(test))]
mod state {
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};

    static MODE: AtomicU8 = AtomicU8::new(0);
    static FIXED: AtomicI64 = AtomicI64::new(0);
    static INITIALISED: AtomicBool = AtomicBool::new(false);

    pub(super) fn mode() -> u8 {
        MODE.load(Ordering::Relaxed)
    }
    pub(super) fn set_mode(v: u8) {
        MODE.store(v, Ordering::Relaxed);
    }
    pub(super) fn fixed() -> i64 {
        FIXED.load(Ordering::Relaxed)
    }
    pub(super) fn set_fixed(v: i64) {
        FIXED.store(v, Ordering::Relaxed);
    }
    pub(super) fn initialised() -> bool {
        INITIALISED.load(Ordering::Relaxed)
    }
    pub(super) fn set_initialised() {
        INITIALISED.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod state {
    use std::cell::Cell;

    thread_local! {
        static MODE: Cell<u8> = const { Cell::new(0) };
        static FIXED: Cell<i64> = const { Cell::new(0) };
        static INITIALISED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn mode() -> u8 {
        MODE.with(Cell::get)
    }
    pub(super) fn set_mode(v: u8) {
        MODE.with(|c| c.set(v));
    }
    pub(super) fn fixed() -> i64 {
        FIXED.with(Cell::get)
    }
    pub(super) fn set_fixed(v: i64) {
        FIXED.with(|c| c.set(v));
    }
    pub(super) fn initialised() -> bool {
        INITIALISED.with(Cell::get)
    }
    pub(super) fn set_initialised() {
        INITIALISED.with(|c| c.set(true));
    }
}

/// Makes `zone` the zone of the whole process.
pub fn set(zone: Zone) {
    match zone {
        Zone::Utc => state::set_mode(0),
        Zone::Local => state::set_mode(1),
        Zone::Fixed(secs) => {
            state::set_fixed(i64::from(secs));
            state::set_mode(2);
        }
    }
}

/// The zone of the process.
pub fn current() -> Zone {
    match state::mode() {
        1 => Zone::Local,
        2 => Zone::Fixed(i32::try_from(state::fixed()).unwrap_or(0)),
        _ => Zone::Utc,
    }
}

/// The calendar day number holding `ts` in the process zone.
pub fn day_of(ts: i64) -> i64 {
    current().day_of(ts)
}

/// The first instant of day `day` in the process zone.
pub fn day_start(day: i64) -> i64 {
    current().day_start(day)
}

/// `HH:MM:SS` of `ts` in the process zone.
pub fn clock(ts: i64) -> String {
    current().clock(ts)
}

/// The ISO 8601 form of `ts` in the process zone.
pub fn iso(ts: i64) -> String {
    current().iso(ts)
}

/// `UTC` or `local time (UTC+10)` for the process zone at `ts`.
pub fn describe(ts: i64) -> String {
    current().describe(ts)
}

/// The zone as a JSON object: the `mode` (`utc` or `local`), the `offset_seconds` at
/// `ts` in seconds east of UTC, and the human `label`.
pub fn json(ts: i64) -> serde_json::Value {
    json_for(current(), ts)
}

fn json_for(zone: Zone, ts: i64) -> serde_json::Value {
    serde_json::json!({
        "mode": if zone == Zone::Utc { "utc" } else { "local" },
        "offset_seconds": zone.offset_at(ts),
        "label": zone.describe(ts),
    })
}

/// `UTC` or `UTC+10`, for narrow places.
pub fn short(ts: i64) -> String {
    current().offset_label(ts)
}

/// `UTC` or `local`, for the process zone.
pub fn label() -> &'static str {
    current().label()
}

/// Picks the zone from the flag, the environment, the setting, then `local`.
/// An unreadable environment value or setting is skipped.
pub fn choose(
    flag: Option<Zone>,
    env: Option<&str>,
    setting: Option<&str>,
    warn: &mut dyn FnMut(&str),
) -> Zone {
    if let Some(zone) = flag {
        return zone;
    }
    if let Some(text) = env.filter(|t| !t.trim().is_empty()) {
        match Zone::parse(text) {
            Some(zone) => return zone,
            None => warn(&format!(
                "ignoring {ENV_VAR}={}: use `local` or `utc`",
                shown(text)
            )),
        }
    }
    match setting {
        Some(text) => Zone::parse(text).unwrap_or_else(|| {
            warn(&format!(
                "ignoring {SETTING}={}: use `local` or `utc`",
                shown(text)
            ));
            Zone::Local
        }),
        None => Zone::Local,
    }
}

/// `text` quoted with control characters escaped and cut to 32 characters, so
/// a hostile value cannot write escape codes to the terminal.
fn shown(text: &str) -> String {
    let short: String = text.chars().take(32).collect();
    format!("{short:?}")
}

/// Runs `init(None)` unless `init` already ran, for programs that embed the
/// library and call `entry::run` without going through `cli::main`.
pub fn ensure_init() {
    if !state::initialised() {
        init(None);
    }
}

/// Chooses the zone from the real environment and the stored setting and
/// installs it.
pub fn init(flag: Option<Zone>) {
    state::set_initialised();
    let env = std::env::var(ENV_VAR).ok();
    let setting = crate::host::config_value_now(SETTING);
    let zone = choose(
        flag,
        env.as_deref(),
        setting.as_ref().and_then(|v| v.as_str()),
        &mut |m| crate::say::notice(m),
    );
    set(zone);
}

#[cfg(test)]
mod tests {
    use super::*;

    const EAST_5H: Zone = Zone::Fixed(5 * 3600);
    const WEST_3H: Zone = Zone::Fixed(-3 * 3600);

    #[test]
    fn a_fixed_offset_shifts_the_clock_and_the_iso_text() {
        let ts = 1_700_000_000; // 2023-11-14T22:13:20Z
        assert_eq!(EAST_5H.clock(ts), "03:13:20");
        assert_eq!(EAST_5H.iso(ts), "2023-11-15T03:13:20+05:00");
        assert_eq!(WEST_3H.iso(ts), "2023-11-14T19:13:20-03:00");
        assert_eq!(Zone::Utc.iso(ts), "2023-11-14T22:13:20Z");
        assert_eq!(EAST_5H.hour_minute(ts), "03:13");
        assert_eq!(WEST_3H.hour_minute(ts), "19:13");
    }

    #[test]
    fn an_event_just_after_utc_midnight_is_still_yesterday_west_of_utc() {
        let utc_midnight = 19_000 * SECONDS_PER_DAY;
        let ts = utc_midnight + 60;
        assert_eq!(Zone::Utc.day_of(ts), 19_000);
        assert_eq!(WEST_3H.day_of(ts), 18_999);
    }

    #[test]
    fn an_event_just_before_utc_midnight_is_already_tomorrow_east_of_utc() {
        let utc_midnight = 19_000 * SECONDS_PER_DAY;
        let ts = utc_midnight - 60;
        assert_eq!(Zone::Utc.day_of(ts), 18_999);
        assert_eq!(EAST_5H.day_of(ts), 19_000);
    }

    #[test]
    fn day_start_is_local_midnight_and_inverts_day_of() {
        let day = 19_000;
        let start = EAST_5H.day_start(day);
        assert_eq!(start, day * SECONDS_PER_DAY - 5 * 3600);
        assert_eq!(EAST_5H.day_of(start), day);
        assert_eq!(EAST_5H.day_of(start - 1), day - 1);
        assert_eq!(EAST_5H.clock(start), "00:00:00");
    }

    #[test]
    fn utc_day_start_is_the_plain_multiple_of_a_day() {
        assert_eq!(Zone::Utc.day_start(19_000), 19_000 * SECONDS_PER_DAY);
    }

    #[test]
    fn the_local_zone_never_panics_and_day_start_stays_inside_its_day() {
        for day in [-1, 0, 10_000, 19_000, 20_000, 60_000] {
            let start = Zone::Local.day_start(day);
            assert_eq!(Zone::Local.day_of(start), day, "day {day}");
            assert_eq!(Zone::Local.day_of(start - 1), day - 1, "day {day}");
        }
    }

    #[test]
    fn extreme_instants_do_not_panic() {
        for ts in [i64::MIN, i64::MAX, 0, -1] {
            let _ = Zone::Local.day_of(ts);
            let _ = Zone::Local.clock(ts);
            let _ = Zone::Local.iso(ts);
        }
        let _ = Zone::Local.day_start(i64::MAX);
        let _ = Zone::Local.day_start(i64::MIN);
    }

    #[test]
    fn the_label_names_the_zone_and_its_offset() {
        let ts = 1_700_000_000;
        assert_eq!(Zone::Utc.describe(ts), "UTC");
        assert_eq!(Zone::Fixed(10 * 3600).describe(ts), "local time (UTC+10)");
        assert_eq!(Zone::Fixed(19_800).describe(ts), "local time (UTC+5:30)");
        assert_eq!(Zone::Fixed(-3 * 3600).describe(ts), "local time (UTC-3)");
        assert_eq!(Zone::Fixed(0).describe(ts), "local time (UTC)");
    }

    /// Offset of a Sydney-like zone: +10 until 2023-10-01 02:00 local, which
    /// is 2023-09-30T16:00:00Z, then +11 (the clocks jump forward one hour).
    const JUMP: i64 = 1_696_089_600;
    fn sydney_like(ts: i64) -> i64 {
        if ts < JUMP {
            10 * 3600
        } else {
            11 * 3600
        }
    }

    #[test]
    fn the_offset_label_follows_the_instant_so_daylight_saving_shows() {
        assert_eq!(offset_text(sydney_like(JUMP - 1)), "UTC+10");
        assert_eq!(offset_text(sydney_like(JUMP)), "UTC+11");
    }

    #[test]
    fn the_day_of_a_daylight_saving_jump_is_twenty_three_hours_long() {
        let jump_day = day_of_with(&sydney_like, JUMP);
        let start = day_start_with(&sydney_like, jump_day);
        let next = day_start_with(&sydney_like, jump_day + 1);
        assert_eq!(next - start, 23 * 3600);
        assert_eq!(day_of_with(&sydney_like, next - 1), jump_day);
        assert_eq!(day_of_with(&sydney_like, next), jump_day + 1);
    }

    #[test]
    fn the_json_carries_the_mode_offset_and_label() {
        let v = json_for(Zone::Fixed(3600), 0);
        assert_eq!(v["mode"], "local");
        assert_eq!(v["offset_seconds"], 3600);
        assert_eq!(v["label"], "local time (UTC+1)");
        assert_eq!(json_for(Zone::Utc, 0)["label"], "UTC");
    }

    #[test]
    fn a_zone_name_parses_regardless_of_case_and_rejects_others() {
        assert_eq!(Zone::parse(" UTC "), Some(Zone::Utc));
        assert_eq!(Zone::parse("Local"), Some(Zone::Local));
        assert_eq!(Zone::parse("Europe/Berlin"), None);
        assert_eq!(Zone::parse(""), None);
    }

    #[test]
    fn the_flag_beats_the_environment_which_beats_the_setting() {
        let mut none = |_: &str| {};
        let mut z = |f, e, s| choose(f, e, s, &mut none);
        assert_eq!(z(Some(Zone::Utc), Some("local"), Some("local")), Zone::Utc);
        assert_eq!(z(None, Some("utc"), Some("local")), Zone::Utc);
        assert_eq!(z(None, None, Some("utc")), Zone::Utc);
        assert_eq!(z(None, None, None), Zone::Local);
    }

    #[test]
    fn an_invalid_environment_value_warns_and_falls_through() {
        let mut seen = Vec::new();
        let zone = choose(None, Some("mars"), Some("utc"), &mut |m| {
            seen.push(m.to_string());
        });
        assert_eq!(zone, Zone::Utc);
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("mars"));
    }

    #[test]
    fn an_invalid_setting_warns_and_falls_back_to_local() {
        let mut seen = Vec::new();
        let zone = choose(None, None, Some("mars"), &mut |m| seen.push(m.to_string()));
        assert_eq!(zone, Zone::Local);
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("usage.timeZone"), "{seen:?}");
    }

    #[test]
    fn a_hostile_value_is_escaped_and_cut_in_the_warning() {
        let mut seen = Vec::new();
        let long = format!("\u{1b}[2J{}", "x".repeat(100));
        choose(None, Some(&long), None, &mut |m| seen.push(m.to_string()));
        assert!(!seen[0].contains('\u{1b}'), "{:?}", seen[0]);
        assert!(seen[0].len() < 120, "{}", seen[0].len());
    }

    /// Offset -3h until local midnight of day 100 (a clock that jumps at
    /// 00:00), then -2h: local midnight does not exist on that day.
    fn midnight_spring(ts: i64) -> i64 {
        if ts < 100 * SECONDS_PER_DAY + 3 * 3600 {
            -3 * 3600
        } else {
            -2 * 3600
        }
    }

    #[test]
    fn a_day_whose_midnight_does_not_exist_starts_at_the_first_instant_after_the_jump() {
        let start = day_start_with(&midnight_spring, 100);
        assert_eq!(day_of_with(&midnight_spring, start), 100);
        assert_eq!(day_of_with(&midnight_spring, start - 1), 99);
    }

    /// The fall back at midnight: -2h until local 00:00 of day 100, then -3h,
    /// so local 23:00 to 00:00 happens twice.
    fn midnight_fall(ts: i64) -> i64 {
        if ts < 100 * SECONDS_PER_DAY + 2 * 3600 {
            -2 * 3600
        } else {
            -3 * 3600
        }
    }

    #[test]
    fn a_repeated_hour_at_midnight_still_gives_each_day_one_start() {
        for day in [99, 100, 101] {
            let start = day_start_with(&midnight_fall, day);
            assert_eq!(day_of_with(&midnight_fall, start), day, "day {day}");
            assert_eq!(day_of_with(&midnight_fall, start - 1), day - 1, "day {day}");
        }
    }
}
