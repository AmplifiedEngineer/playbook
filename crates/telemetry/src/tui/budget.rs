// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The budget lines of the header: each known limit as one visual row, then the
//! burn of the period on screen against the previous one.
//!
//! A limit row reads left to right: the name, a one letter source glyph, a
//! wide bar with a pace marker, the percent used, the amount, a verdict and
//! the time left. The bar is colored by how full it is. The marker sits where
//! the elapsed share of the period puts you by now, so a bar that runs past
//! the marker is ahead of pace and one that stops short is behind.
//!
//! A second dim line carries the figures that need a table: what is left,
//! what is allowed and averaged per hour and per day, the projected total,
//! the lead over the pace, the full reset time and where the numbers come
//! from. Only the worst limit shows that line by default. The `i` key shows it
//! for every limit and puts the full text of the notes where the spend line
//! was.
//!
//! The source glyph: `m` is measured, read from Claude Code's own statusline
//! input. `c` is cached, read from the usage numbers Claude Code keeps in its
//! config file (past six hours this machine's messages since the read are
//! added). `u` is a manual `usage.budget.*` config value. `h` is local
//! history, estimated from the session transcripts.

use super::data::{Data, LimitsView};
use super::fmt::{fit, money, WEEKDAYS};
use super::icon::{Cue, Icon};
use super::projection::Reach;
use super::theme::{Ink, Palette};
use crate::aggregate::date_key;
use crate::limits::{Limit, LimitPeriod, Origin, State, Unit};
use crate::limits_source::STALE_SECONDS;
use crate::period::{elapsed_text, Change};
use crate::query::{clock, day_of, weekday};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// The most lines drawn: a limit, its figures, a second limit and the spend
/// line. Two limits with two lines each would need five before the spend line,
/// which a 24 row screen cannot spare, so the figures of the second limit sit
/// behind `i`. Only the lines that exist are drawn, so most views are shorter.
const ROWS: usize = 4;
/// The most lines under the metrics of a header that stacks on a short screen.
/// With two limits the figures of the first sit behind `i`, which always draws
/// up to `ROWS`.
pub const STACKED_ROWS: usize = 3;
/// The detail view draws a limit row and a figures row for each limit.
const _: () = assert!(ROWS >= 2 * MAX_LIMITS);
/// The most limits drawn. The rest are counted in the header's bottom border.
const MAX_LIMITS: usize = 2;
/// The width of the name column, so the bars of all rows start together.
const NAME_WIDTH: usize = 8;
/// The name column, the glyph and the two spaces around the glyph.
const HEAD_WIDTH: usize = NAME_WIDTH + 3;
/// The bar never shrinks below this while any other part can be dropped.
const BAR_FLOOR: usize = 10;
/// The bar stops growing here, so a wide screen is not one long bar.
const BAR_MAX: usize = 60;
/// The width of each bar of the spend comparison.
const MINI_BAR: usize = 10;
const SECONDS_PER_DAY: i64 = 86_400;
/// From this many days left the countdown drops the hours.
const DAYS_ONLY_FROM: i64 = 2;
/// The share of a full bar, in percent, that the marker position is scaled to.
const PERCENT: f64 = 100.0;

/// The separator between the pieces of a line.
const GAP: usize = 2;
/// The separator between a piece and the one it belongs to.
const TIGHT: usize = 1;

// How the pieces of a row drop out on a narrow screen. The highest rank goes
// first, `0` never goes.
const KEEP: u8 = 0;
const STALE_RANK: u8 = 1;
const AMOUNT_RANK: u8 = 5;
const COUNTDOWN_RANK: u8 = 4;
const CLAUSE_RANK: u8 = 3;
const DATE_RANK: u8 = 6;

// The pieces of the figures line, in the order they drop. The source stays
// until the least useful figures are gone, because a stale or partial source
// changes what the numbers mean.
const LEFT_RANK: u8 = KEEP;
const SOURCE_RANK: u8 = 1;
const PACE_RANK: u8 = 2;
const ALLOW_RANK: u8 = 3;
const PROJECTED_RANK: u8 = 4;
const RESETS_RANK: u8 = 5;
const AVERAGE_RANK: u8 = 6;

// The width of each figure, so two figure lines line up.
const LEFT_COLUMN: usize = 12;
const PACE_COLUMN: usize = 11;
const ALLOW_COLUMN: usize = 22;
const PROJECTED_COLUMN: usize = 12;
const AVERAGE_COLUMN: usize = 21;
const RESETS_COLUMN: usize = 26;

// The spend line.
const CHANGE_RANK: u8 = 1;
const PREVIOUS_RANK: u8 = 2;
const BARS_RANK: u8 = 3;
const SPEND_AVERAGE_RANK: u8 = 4;

/// A lead over the pace past this many points is drawn in the warning color.
const AHEAD_WARN_POINTS: f64 = 10.0;

const GLYPH_MEASURED: &str = "m";
const GLYPH_CACHED: &str = "c";
const GLYPH_MANUAL: &str = "u";
const GLYPH_HISTORY: &str = "h";

const BAR_FILLED: &str = "\u{2588}";
const BAR_EMPTY: &str = "\u{2591}";
const BAR_MARKER: &str = "\u{2502}";
const ASCII_FILLED: &str = "#";
const ASCII_EMPTY: &str = "-";
const ASCII_MARKER: &str = "|";

/// The verdict words. The mark before each comes from `Icon`.
const WORD_REACHED: &str = "reached";
const WORD_HITS: &str = "hits it";
const WORD_CLOSE: &str = "close";
const WORD_OK: &str = "ok";
const WORD_EARLY: &str = "too early";

fn fg(pal: &Palette, ink: Ink) -> Style {
    Style::default().fg(pal.color(ink))
}

/// `10-12 09:30` for a second, in the chosen time zone.
pub(super) fn stamp(at: i64) -> String {
    let date = date_key(day_of(at));
    format!("{} {}", &date[5..], &clock(at)[..5])
}

/// `Fri 10-12 09:30` for a second, in the chosen time zone.
fn weekday_stamp(at: i64) -> String {
    format!("{} {}", WEEKDAYS[weekday(day_of(at))], stamp(at))
}

/// A dollar figure without cents from one hundred up, where cents are noise.
fn dollars(n: f64) -> String {
    if n.abs() >= 100.0 {
        let whole = money(n.round());
        whole.trim_end_matches(".00").to_string()
    } else {
        money(n)
    }
}

fn amount(unit: Unit, n: f64) -> String {
    match unit {
        Unit::Dollars => dollars(n),
        Unit::Percent => format!("{n:.0}%"),
    }
}

fn rate(unit: Unit, n: Option<f64>) -> String {
    match (unit, n) {
        (_, None) => "n/a".to_string(),
        (Unit::Dollars, Some(n)) => dollars(n),
        (Unit::Percent, Some(n)) => format!("{n:.1}%"),
    }
}

pub(super) fn name(l: &Limit) -> &'static str {
    match (l.period, l.unit) {
        (LimitPeriod::FiveHour, _) => "5 hour window",
        (LimitPeriod::SevenDay, _) => "weekly window",
        (LimitPeriod::Daily, _) => "daily budget",
        (LimitPeriod::Weekly, Unit::Percent) => "weekly spend limit",
        (LimitPeriod::Monthly, Unit::Percent) => "monthly spend limit",
        (LimitPeriod::Weekly, _) => "weekly budget",
        (LimitPeriod::Monthly, _) => "monthly budget",
    }
}

/// The name that fits the name column. The unit tells a weekly window from a
/// weekly budget: a window is a percent, a budget is dollars.
fn short_name(l: &Limit) -> &'static str {
    match l.period {
        LimitPeriod::FiveHour => "5 hour",
        LimitPeriod::SevenDay => "7 day",
        LimitPeriod::Daily => "Daily",
        LimitPeriod::Weekly => "Weekly",
        LimitPeriod::Monthly => "Monthly",
    }
}

fn origin_glyph(origin: Origin) -> &'static str {
    match origin {
        Origin::Measured => GLYPH_MEASURED,
        Origin::Cached => GLYPH_CACHED,
        Origin::Manual => GLYPH_MANUAL,
    }
}

/// The time left as a number the eye can scan: days from two days out, then
/// hours and minutes.
fn countdown(seconds: i64) -> String {
    let s = seconds.max(0);
    if s / SECONDS_PER_DAY >= DAYS_ONLY_FROM {
        format!("{}d", s / SECONDS_PER_DAY)
    } else {
        elapsed_text(s)
    }
}

/// Where the reset falls, short: the clock inside a day, else weekday and date.
fn reset_hint(end: i64, now: i64) -> String {
    if end - now < SECONDS_PER_DAY {
        clock(end)[..5].to_string()
    } else {
        let date = date_key(day_of(end));
        format!("{} {}", WEEKDAYS[weekday(day_of(end))], &date[5..])
    }
}

/// The share of the period that has passed, from 0 to 1.
fn elapsed_share(l: &Limit, now: i64) -> f64 {
    let span = l.end - l.start;
    if span <= 0 {
        return 0.0;
    }
    ((now - l.start) as f64 / span as f64).clamp(0.0, 1.0)
}

/// The share of the limit used, in percent, from 0 to 100.
fn used_percent(l: &Limit) -> f64 {
    if l.limit > 0.0 {
        (l.used / l.limit * PERCENT).clamp(0.0, PERCENT)
    } else {
        0.0
    }
}

/// The cell of a `width` cell bar the pace marker sits in.
fn marker_cell(share: f64, width: usize) -> usize {
    let cell = (share.clamp(0.0, 1.0) * width as f64) as usize;
    cell.min(width.saturating_sub(1))
}

/// The cells of the bar. A filled cell wears the color of its place on the
/// scale, so the bar reddens as it fills. The marker replaces the cell at the
/// elapsed share, and is a different character, so it reads without color.
fn bar_spans(pal: &Palette, icon: Icon, pct: f64, share: f64, width: usize) -> Vec<Span<'static>> {
    let ascii = icon == Icon::Off;
    let (filled_glyph, empty_glyph, marker_glyph) = if ascii {
        (ASCII_FILLED, ASCII_EMPTY, ASCII_MARKER)
    } else {
        (BAR_FILLED, BAR_EMPTY, BAR_MARKER)
    };
    let filled = ((pct / PERCENT * width as f64).round() as usize).min(width);
    let marker = marker_cell(share, width);
    let mut cells = Vec::with_capacity(width);
    for i in 0..width {
        cells.push(if i == marker {
            Span::styled(
                marker_glyph,
                fg(pal, pal.title).add_modifier(Modifier::BOLD),
            )
        } else if i < filled {
            Span::styled(
                filled_glyph,
                Style::default()
                    .fg(pal.graded(pal.process, (i + 1) as f64 / width as f64 * PERCENT)),
            )
        } else {
            Span::styled(empty_glyph, fg(pal, pal.meter_bg))
        });
    }
    cells
}

/// A bar with no marker, `filled` of `width` cells, for the spend comparison.
fn plain_bar(pal: &Palette, icon: Icon, fraction: f64, width: usize) -> Vec<Span<'static>> {
    let (filled_glyph, empty_glyph) = if icon == Icon::Off {
        (ASCII_FILLED, ASCII_EMPTY)
    } else {
        (BAR_FILLED, BAR_EMPTY)
    };
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    vec![
        Span::styled(filled_glyph.repeat(filled), fg(pal, pal.fg)),
        Span::styled(empty_glyph.repeat(width - filled), fg(pal, pal.meter_bg)),
    ]
}

/// A piece of a line. When the line is too wide, the pieces with the highest
/// `drop` go first. `0` is never dropped.
struct Seg {
    drop: u8,
    /// Joined to the piece before it by one space, not two.
    tight: bool,
    spans: Vec<Span<'static>>,
}

impl Seg {
    fn new(drop: u8, spans: Vec<Span<'static>>) -> Seg {
        Seg {
            drop,
            tight: false,
            spans,
        }
    }

    fn text(drop: u8, text: String, style: Style) -> Seg {
        Seg::new(drop, vec![Span::styled(text, style)])
    }

    fn tight(mut self) -> Seg {
        self.tight = true;
        self
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|s| s.content.chars().count()).sum()
    }

    fn gap(&self) -> usize {
        if self.tight {
            TIGHT
        } else {
            GAP
        }
    }
}

fn total(segs: &[Seg]) -> usize {
    segs.iter()
        .enumerate()
        .map(|(i, s)| s.width() + if i > 0 { s.gap() } else { 0 })
        .sum()
}

/// The pieces that fit `width`: the highest-ranked are removed until the
/// rest fit, the last of a rank first.
fn keep(mut segs: Vec<Seg>, width: usize) -> Vec<Seg> {
    while total(&segs) > width {
        let Some(worst) = segs.iter().map(|s| s.drop).max().filter(|d| *d > 0) else {
            break;
        };
        if let Some(i) = segs.iter().rposition(|s| s.drop == worst) {
            segs.remove(i);
        }
    }
    segs
}

/// The spans of the pieces, with their separators.
fn spans_of(segs: Vec<Seg>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, seg) in segs.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(seg.gap())));
        }
        spans.extend(seg.spans);
    }
    spans
}

/// The spans of the pieces that fit `width`.
fn keep_spans(segs: Vec<Seg>, width: usize) -> Vec<Span<'static>> {
    spans_of(keep(segs, width))
}

/// The verdict of a limit: a mark and a word, colored by state.
fn verdict(pal: &Palette, icon: Icon, l: &Limit, now: i64) -> (Seg, Option<State>) {
    let Some(h) = l.headroom(now) else {
        return (
            Seg::text(KEEP, "no limit".into(), fg(pal, pal.inactive)),
            None,
        );
    };
    let (cue, word, ink) = match h.state {
        State::Exceeded => (Cue::Alert, WORD_REACHED, pal.error),
        State::WillExceed { .. } => (Cue::Alert, WORD_HITS, pal.error),
        State::OnTrack => (Cue::Watch, WORD_CLOSE, pal.warn),
        State::Under => (Cue::Fine, WORD_OK, pal.live),
        State::Unknown => (Cue::Unknown, WORD_EARLY, pal.inactive),
    };
    let weight = if cue == Cue::Alert {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let text = format!("{}{word}", icon.mark(cue));
    (
        Seg::text(KEEP, text, fg(pal, ink).add_modifier(weight)),
        Some(h.state),
    )
}

/// The clause after the verdict: when the limit is reached, or that it is
/// safe. A limit that will be hit already says so in the verdict word, so
/// its clause is only the time.
fn clause(pal: &Palette, l: &Limit, now: i64, v: &LimitsView, state: Option<State>) -> Option<Seg> {
    let reach = super::projection::reach(l, v.recent_cost_24h, now)?;
    let will_hit = matches!(state, Some(State::WillExceed { .. }));
    let dim = fg(pal, pal.fg);
    match reach {
        Reach::In { seconds, recent } => {
            let pace = if recent { " (24h)" } else { "" };
            let text = if will_hit {
                format!("in {}{pace}", elapsed_text(seconds))
            } else {
                format!("hits it in {}{pace}", elapsed_text(seconds))
            };
            let seg = Seg::text(CLAUSE_RANK, text, fg(pal, pal.error));
            Some(if will_hit { seg.tight() } else { seg })
        }
        Reach::Safe => Some(Seg::text(CLAUSE_RANK, "safe".to_string(), dim)),
    }
}

/// How stale the reading behind a limit is, when it is. Shown on the row,
/// because a stale number can mislead.
fn stale_text(l: &Limit, v: &LimitsView) -> Option<String> {
    match (l.origin, v.cache_age) {
        (Origin::Measured, _) if v.stale => Some("stale".to_string()),
        (Origin::Cached, Some(a)) if a > STALE_SECONDS => {
            Some(format!("stale {}", elapsed_text(a)))
        }
        _ => None,
    }
}

/// The source label of a limit: where it came from, and how old when that
/// matters.
fn tag(
    origin: Origin,
    stale: bool,
    cache_age: Option<i64>,
    plus_local: bool,
    all_machines: bool,
) -> String {
    match (origin, stale) {
        (Origin::Measured, true) => "measured, stale".to_string(),
        (Origin::Measured, _) => "measured".to_string(),
        // A manual budget is counted from this machine's transcripts, plus the
        // synced events of other machines when there are any.
        (Origin::Manual, _) if all_machines => "manual, all machines".to_string(),
        (Origin::Manual, _) => "manual, local".to_string(),
        (Origin::Cached, _) => match cache_age {
            // Past the freshness limit the cache's used amount is the floor
            // and this machine's messages since the read are added; other
            // machines and the web apps since then are missed.
            Some(a) if a > STALE_SECONDS && plus_local => {
                format!("cached {} ago, plus this machine since", elapsed_text(a))
            }
            // No usable amount in the cache: the bar is this machine alone,
            // which misses other machines and the web apps.
            Some(a) if a > STALE_SECONDS => {
                format!(
                    "limit cached {} ago, used is this machine only",
                    elapsed_text(a)
                )
            }
            Some(a) => format!("cached {} ago", elapsed_text(a)),
            None => "cached".to_string(),
        },
    }
}

/// The right part of a limit's row, the pieces that fit beside a bar of at
/// least `BAR_FLOOR` cells.
fn limit_tail(
    pal: &Palette,
    icon: Icon,
    l: &Limit,
    now: i64,
    v: &LimitsView,
    width: usize,
) -> Vec<Seg> {
    let pct = used_percent(l);
    let plain = fg(pal, pal.fg);
    let (verdict_seg, state) = verdict(pal, icon, l, now);
    let mut tail = vec![Seg::text(
        KEEP,
        format!("{pct:>3.0}%"),
        fg(pal, pal.usage_ink(pct)).add_modifier(Modifier::BOLD),
    )];
    // A percent limit says its amount in the percent already.
    if l.unit == Unit::Dollars {
        tail.push(Seg::text(
            AMOUNT_RANK,
            format!("{} of {}", amount(l.unit, l.used), amount(l.unit, l.limit)),
            plain,
        ));
    }
    tail.push(verdict_seg);
    tail.extend(clause(pal, l, now, v, state));
    tail.extend(stale_text(l, v).map(|t| Seg::text(STALE_RANK, t, fg(pal, pal.warn))));
    tail.push(Seg::text(
        COUNTDOWN_RANK,
        format!("resets in {}", countdown(l.end - now)),
        plain,
    ));
    tail.push(
        Seg::text(
            DATE_RANK,
            format!("({})", reset_hint(l.end, now)),
            fg(pal, pal.inactive),
        )
        .tight(),
    );
    let room = width.saturating_sub(HEAD_WIDTH + GAP);
    keep(tail, room.saturating_sub(BAR_FLOOR))
}

/// The one row of a limit: name, source glyph, bar with the pace marker,
/// percent, amount, verdict and time left. `tail_width` is the widest tail of
/// the rows drawn together, so their bars start and end in the same cells.
fn limit_line(
    pal: &Palette,
    icon: Icon,
    l: &Limit,
    now: i64,
    v: &LimitsView,
    width: usize,
    tail_width: usize,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:<NAME_WIDTH$}", short_name(l)),
            fg(pal, pal.title).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(origin_glyph(l.origin), fg(pal, pal.inactive)),
        Span::raw(" "),
    ];
    let tail = limit_tail(pal, icon, l, now, v, width);
    let own = total(&tail);
    let room = width.saturating_sub(HEAD_WIDTH + GAP);
    let bar_width = room.saturating_sub(tail_width.max(own)).clamp(1, BAR_MAX);
    spans.extend(bar_spans(
        pal,
        icon,
        used_percent(l),
        elapsed_share(l, now),
        bar_width,
    ));
    spans.push(Span::raw(" ".repeat(GAP)));
    spans.extend(spans_of(tail));
    Line::from(spans)
}

/// How far the used share is ahead of (positive) or behind the elapsed
/// share, in points of the limit.
fn pace_points(l: &Limit, now: i64) -> f64 {
    used_percent(l) - elapsed_share(l, now) * PERCENT
}

fn column(text: String, width: usize) -> String {
    format!("{text:<width$}")
}

/// The dim line under a limit: the figures that need a table, in columns, and
/// where the numbers come from.
fn figures_line(pal: &Palette, l: &Limit, now: i64, v: &LimitsView, width: usize) -> Line<'static> {
    let dim = fg(pal, pal.inactive);
    let plain = fg(pal, pal.fg);
    let source = tag(
        l.origin,
        v.stale,
        v.cache_age,
        v.cached_plus_local,
        v.all_machines,
    );
    let mut segs = Vec::new();
    if let Some(h) = l.headroom(now) {
        segs.push(Seg::text(
            LEFT_RANK,
            column(format!("left {}", amount(l.unit, h.remaining)), LEFT_COLUMN),
            plain,
        ));
        let points = pace_points(l, now);
        let pace_ink = if points > AHEAD_WARN_POINTS {
            pal.warn
        } else {
            pal.live
        };
        segs.push(Seg::text(
            PACE_RANK,
            column(format!("pace {points:+.0}pt"), PACE_COLUMN),
            fg(pal, pace_ink),
        ));
        segs.push(Seg::text(
            ALLOW_RANK,
            column(
                format!(
                    "allow {}/h {}/d",
                    rate(l.unit, h.allowed_per_hour),
                    rate(l.unit, h.allowed_per_day)
                ),
                ALLOW_COLUMN,
            ),
            plain,
        ));
        segs.push(Seg::text(
            PROJECTED_RANK,
            column(
                format!("proj {}", rate(l.unit, h.projected)),
                PROJECTED_COLUMN,
            ),
            plain,
        ));
        segs.push(Seg::text(
            AVERAGE_RANK,
            column(
                format!(
                    "avg {}/h {}/d",
                    rate(l.unit, h.avg_per_hour),
                    rate(l.unit, h.avg_per_day)
                ),
                AVERAGE_COLUMN,
            ),
            plain,
        ));
    }
    segs.push(Seg::text(
        RESETS_RANK,
        column(
            format!("resets {} {}", weekday_stamp(l.end), crate::tz::label()),
            RESETS_COLUMN,
        ),
        plain,
    ));
    segs.push(Seg::text(SOURCE_RANK, source, dim));
    let head = format!("{:<HEAD_WIDTH$}", short_name(l));
    let rest = width.saturating_sub(HEAD_WIDTH);
    let mut spans = vec![Span::styled(head, dim)];
    spans.extend(spans_of(keep(segs, rest)));
    Line::from(spans)
}

/// A note about where the numbers come from: a short form for the status
/// line and the sentence for the detail view.
struct Note {
    short: String,
    full: String,
    ink: Ink,
}

/// What Claude Code reported as used, with its age, when no bar above shows
/// that figure: a bar from a manual budget or a stale cache counts only this
/// machine. A fresh cache replaces the estimate, so it needs no note.
fn reported_note(v: &LimitsView, now: i64) -> Option<(String, String)> {
    let (read_at, used) = v.reported?;
    let monthly = v
        .limits
        .iter()
        .find(|l| l.period == LimitPeriod::Monthly && l.unit == Unit::Dollars)?;
    let age = (now - read_at).max(0);
    let stale = age > STALE_SECONDS;
    // The bar of a fresh cache already shows the newest figure, whichever
    // source the reported one came from.
    let fresh_cache = v.cache_age.is_some_and(|a| a <= STALE_SECONDS);
    if monthly.origin == Origin::Measured || (monthly.origin == Origin::Cached && fresh_cache) {
        return None;
    }
    let suffix = if stale { " (stale)" } else { "" };
    let short = format!(
        "Claude read {} used {} ago{suffix}",
        dollars(used),
        elapsed_text(age)
    );
    let full = format!(
        "Claude reported {} used this month, read {} ago{suffix}; run /usage in Claude Code to refresh it",
        crate::limits_source::amount(Unit::Dollars, used),
        elapsed_text(age),
    );
    Some((short, full))
}

/// Says that a manual monthly budget is set and the detected one applies.
fn both_budgets_line(v: &LimitsView) -> Option<String> {
    let (manual, detected) = v.manual_beside_detected?;
    Some(format!(
        "Manual monthly budget {} and detected {}: the detected one applies",
        money(manual),
        money(detected)
    ))
}

/// The short form of the note on the missing 5 hour and weekly windows.
fn windows_short(note: &str) -> &'static str {
    if note.contains("Enterprise") {
        "5 hour and weekly windows: not used on Enterprise"
    } else if note.contains("no recent reading") {
        "5 hour and weekly windows: no recent reading"
    } else {
        "5 hour and weekly windows: not on this plan"
    }
}

fn notes(pal: &Palette, v: &LimitsView, now: i64) -> Vec<Note> {
    let mut out = Vec::new();
    if let Some(full) = both_budgets_line(v) {
        let (manual, detected) = v.manual_beside_detected.unwrap_or_default();
        out.push(Note {
            short: format!(
                "detected {} applies, manual {}",
                dollars(detected),
                dollars(manual)
            ),
            full,
            ink: pal.warn,
        });
    }
    if let Some((short, full)) = reported_note(v, now) {
        out.push(Note {
            short,
            full,
            ink: pal.fg,
        });
    }
    if let Some(note) = v.windows_note {
        out.push(Note {
            short: windows_short(note).to_string(),
            full: note.to_string(),
            ink: pal.inactive,
        });
    }
    out
}

/// The one dim line for every note, with where the rest is explained.
fn status_line(pal: &Palette, notes: &[Note], width: usize) -> Line<'static> {
    let shorts: Vec<&str> = notes.iter().map(|n| n.short.as_str()).collect();
    let text = format!("{}  (i: details)", shorts.join("; "));
    Line::styled(fit(&text, width), fg(pal, pal.inactive))
}

/// The change of the spend against the previous period, as a signed percent.
fn change_text(now: f64, before: f64) -> String {
    match Change::of(now, before).percent {
        Some(p) => format!("{p:+.0}%"),
        None => "new".to_string(),
    }
}

/// The spend line: this period against the previous one as two bars on the
/// same scale, the change, and the average rate.
fn burn_line(pal: &Palette, icon: Icon, v: &LimitsView, width: usize) -> Line<'static> {
    let plain = fg(pal, pal.fg);
    let scale = v.burn.cost_usd.max(v.previous.cost_usd);
    let fraction = |n: f64| if scale > 0.0 { n / scale } else { 0.0 };
    let (now_cost, before) = (v.burn.cost_usd, v.previous.cost_usd);
    let change_ink = if now_cost > before {
        pal.warn
    } else {
        pal.live
    };
    let average = match (v.burn.per_hour, v.burn.per_day) {
        (Some(h), Some(d)) => format!("avg {}/h {}/d", money(h), money(d)),
        _ => "avg n/a".to_string(),
    };
    let head = vec![
        Span::styled(
            format!("{:<NAME_WIDTH$}", "Spend"),
            fg(pal, pal.title).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(GLYPH_HISTORY, fg(pal, pal.inactive)),
        Span::raw(" "),
    ];
    let segs = vec![
        Seg::text(
            KEEP,
            format!("{} {}", v.period.label(), dollars(now_cost)),
            plain,
        ),
        Seg::new(
            BARS_RANK,
            plain_bar(pal, icon, fraction(now_cost), MINI_BAR),
        )
        .tight(),
        Seg::text(
            PREVIOUS_RANK,
            format!("{} {}", v.period.previous_label(), dollars(before)),
            plain,
        ),
        Seg::new(BARS_RANK, plain_bar(pal, icon, fraction(before), MINI_BAR)).tight(),
        Seg::text(
            CHANGE_RANK,
            change_text(now_cost, before),
            fg(pal, change_ink),
        ),
        Seg::text(SPEND_AVERAGE_RANK, average, fg(pal, pal.inactive)),
    ];
    let mut spans = head;
    spans.extend(keep_spans(segs, width.saturating_sub(HEAD_WIDTH)));
    Line::from(spans)
}

/// The row where no limit is known: an empty bar and how to set one.
fn no_limit_line(pal: &Palette, icon: Icon, width: usize) -> Line<'static> {
    let dim = fg(pal, pal.inactive);
    let hint = "press b to set a monthly budget";
    let room = width.saturating_sub(HEAD_WIDTH + GAP + hint.chars().count());
    let bar = room.clamp(BAR_FLOOR.min(width), BAR_MAX);
    let empty = if icon == Icon::Off {
        ASCII_EMPTY
    } else {
        BAR_EMPTY
    };
    Line::from(vec![
        Span::styled(
            format!("{:<NAME_WIDTH$}", "No limit"),
            dim.add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(empty.repeat(bar), fg(pal, pal.meter_bg)),
        Span::raw(" ".repeat(GAP)),
        Span::styled(fit(hint, width.saturating_sub(HEAD_WIDTH + bar + GAP)), dim),
    ])
}

/// The budget lines, at most `ROWS`. `detail` shows the figures of
/// every limit and the full notes in place of the spend line.
fn body(
    data: &Data,
    pal: &Palette,
    icon: Icon,
    width: usize,
    detail: bool,
    cap: usize,
) -> (Vec<Line<'static>>, usize) {
    let cap = if detail { ROWS } else { cap.min(ROWS) };
    let v = &data.limits;
    let all_notes = notes(pal, v, data.now);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if v.limits.is_empty() {
        lines.push(no_limit_line(pal, icon, width));
    }
    let shown = &v.limits[..v.limits.len().min(MAX_LIMITS)];
    let tail_width = shown
        .iter()
        .map(|l| total(&limit_tail(pal, icon, l, data.now, v, width)))
        .max()
        .unwrap_or(0);
    // The figures of the first limit and the spend line share what is left of
    // `cap`; a short cap gives the spend line the row, and `i` shows the
    // figures of every limit.
    let first_figures = shown.len() + 2 <= cap;
    for (i, l) in shown.iter().enumerate() {
        lines.push(limit_line(pal, icon, l, data.now, v, width, tail_width));
        if detail || (i == 0 && first_figures) {
            lines.push(figures_line(pal, l, data.now, v, width));
        }
    }
    let mut hidden_notes = all_notes.len();
    if detail {
        for note in &all_notes {
            if lines.len() < cap {
                lines.push(Line::styled(fit(&note.full, width), fg(pal, note.ink)));
                hidden_notes -= 1;
            }
        }
        if lines.len() < cap {
            lines.push(burn_line(pal, icon, v, width));
        }
    } else {
        if v.limits.is_empty() && !all_notes.is_empty() {
            lines.push(status_line(pal, &all_notes, width));
            hidden_notes = 0;
        }
        lines.push(burn_line(pal, icon, v, width));
        if hidden_notes > 0 && lines.len() < cap {
            lines.push(status_line(pal, &all_notes, width));
            hidden_notes = 0;
        }
    }
    lines.truncate(cap);
    (lines, hidden_notes)
}

/// The budget lines of the header and what they could not show.
pub struct Lines {
    pub lines: Vec<Line<'static>>,
    /// Limits past `MAX_LIMITS`, left to `--json`.
    pub more: usize,
    /// Notes that did not fit and wait behind `i`.
    pub hidden_notes: usize,
}

/// The budget lines for a column `width` wide, at most `cap` and `ROWS`. A line exists
/// only for data that exists, so the header never keeps rows for the rest.
pub fn lines(
    data: &Data,
    pal: &Palette,
    icon: Icon,
    width: usize,
    detail: bool,
    cap: usize,
) -> Lines {
    let (lines, hidden_notes) = body(data, pal, icon, width, detail, cap);
    Lines {
        lines,
        more: data.limits.limits.len().saturating_sub(MAX_LIMITS),
        hidden_notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{ColorDepth, Theme};

    const HOUR: i64 = 3600;

    #[test]
    fn a_manual_budget_names_the_machines_it_counts() {
        // (other machines synced, label)
        for (all_machines, want) in [(false, "manual, local"), (true, "manual, all machines")] {
            // Act
            let label = tag(Origin::Manual, false, None, false, all_machines);

            // Assert
            assert_eq!(label, want);
        }
    }

    fn limit(used: f64) -> Limit {
        Limit {
            unit: Unit::Percent,
            period: LimitPeriod::Monthly,
            origin: Origin::Manual,
            used,
            limit: 100.0,
            start: 0,
            end: 100 * HOUR,
        }
    }

    fn pal() -> Palette {
        Theme::Standard.palette(ColorDepth::True)
    }

    fn shown(icon: Icon, used: f64, now: i64) -> (String, Modifier) {
        let (seg, _) = verdict(&pal(), icon, &limit(used), now);
        let span = &seg.spans[0];
        (span.content.to_string(), span.style.add_modifier)
    }

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn both_budgets_are_named_with_the_one_that_applies() {
        let mut v = LimitsView::default();
        assert_eq!(both_budgets_line(&v), None);
        v.manual_beside_detected = Some((1000.0, 2000.0));

        assert_eq!(
            both_budgets_line(&v).as_deref(),
            Some(
                "Manual monthly budget $1,000.00 and detected $2,000.00: the detected one applies"
            )
        );
    }

    #[test]
    fn each_state_starts_with_its_own_ascii_mark_when_the_icon_is_off() {
        // Arrange
        let ten_hours = 10 * HOUR;
        let cases = [
            (100.0, ten_hours, "! reached"),
            (20.0, ten_hours, "! hits it"),
            (9.0, ten_hours, "~ close"),
            (5.0, ten_hours, "+ ok"),
            (5.0, HOUR / 2, "? too early"),
        ];

        for (used, now, want) in cases {
            // Act
            let (text, _) = shown(Icon::Off, used, now);

            // Assert
            assert_eq!(text, want);
        }
    }

    #[test]
    fn a_utf8_terminal_gets_symbols_and_only_alerts_are_bold() {
        // Arrange
        let ten_hours = 10 * HOUR;

        // Act
        let reached = shown(Icon::Wide, 100.0, ten_hours);
        let watch = shown(Icon::Wide, 9.0, ten_hours);

        // Assert
        assert_eq!(reached.0, "\u{25b2} reached");
        assert!(reached.1.contains(Modifier::BOLD));
        assert_eq!(watch.0, "\u{25c6} close");
        assert!(!watch.1.contains(Modifier::BOLD));
    }

    #[test]
    fn the_pace_marker_sits_at_the_elapsed_share_of_the_bar() {
        // (elapsed share, bar width, cell)
        for (share, width, cell) in [
            (0.0, 20, 0),
            (0.25, 20, 5),
            (0.5, 20, 10),
            (0.99, 20, 19),
            (1.0, 20, 19),
            (1.5, 20, 19),
            (-1.0, 20, 0),
            (0.5, 1, 0),
        ] {
            assert_eq!(marker_cell(share, width), cell, "{share} of {width}");
        }
    }

    #[test]
    fn the_bar_draws_one_marker_and_fills_to_the_used_share() {
        // Arrange
        let (pct, share, width) = (50.0, 0.8, 10);

        for (icon, filled, empty, marker) in [
            (Icon::Wide, "\u{2588}", "\u{2591}", "\u{2502}"),
            (Icon::Off, "#", "-", "|"),
        ] {
            // Act
            let cells = bar_spans(&pal(), icon, pct, share, width);

            // Assert
            let drawn: Vec<&str> = cells.iter().map(|c| c.content.as_ref()).collect();
            assert_eq!(drawn.len(), width);
            assert_eq!(drawn.iter().filter(|c| **c == marker).count(), 1);
            assert_eq!(drawn[marker_cell(share, width)], marker);
            assert_eq!(drawn.iter().filter(|c| **c == filled).count(), 5);
            assert_eq!(drawn.iter().filter(|c| **c == empty).count(), 4);
        }
    }

    #[test]
    fn a_bar_past_the_marker_is_ahead_of_pace_and_one_short_of_it_is_behind() {
        // Arrange
        let mut l = limit(80.0);
        l.start = 0;
        l.end = 100 * HOUR;
        let now = 50 * HOUR;

        // Act
        let ahead = pace_points(&l, now);
        l.used = 20.0;
        let behind = pace_points(&l, now);

        // Assert
        assert_eq!(ahead.round(), 30.0);
        assert_eq!(behind.round(), -30.0);
    }

    fn view_with(limits: Vec<Limit>) -> LimitsView {
        LimitsView {
            limits,
            ..LimitsView::default()
        }
    }

    fn row(l: &Limit, now: i64, v: &LimitsView, width: usize, tail: usize) -> String {
        text(&limit_line(&pal(), Icon::Off, l, now, v, width, tail).spans)
    }

    fn budget(used: f64) -> Limit {
        Limit {
            unit: Unit::Dollars,
            limit: 800.0,
            ..limit(used)
        }
    }

    #[test]
    fn a_row_drops_its_pieces_in_rank_order_as_the_screen_narrows() {
        // Arrange: halfway used a hundred hours in, so it will be hit.
        let l = budget(400.0);
        let now = 40 * HOUR;
        let v = view_with(vec![l.clone()]);
        // The pieces in the order they drop: the date, the amount, the
        // countdown, the time to the limit.
        let pieces = ["(", "$400 of $800", "resets in", "hits it in"];

        let mut fewest = pieces.len();
        for width in (HEAD_WIDTH + 12..=160).rev() {
            // Act
            let line = row(&l, now, &v, width, 0);

            // Assert: a piece that survives means every later one survives.
            let present: Vec<bool> = pieces.iter().map(|p| line.contains(p)).collect();
            for (i, here) in present.iter().enumerate() {
                if *here {
                    assert!(
                        present[i + 1..].iter().all(|p| *p),
                        "{width}: {present:?} {line}"
                    );
                }
            }
            fewest = fewest.min(present.iter().filter(|p| **p).count());
            assert!(line.contains("50%") && line.contains("! hits it"), "{line}");
        }
        assert_eq!(fewest, 0, "the narrowest rows drop every optional piece");
        assert!(row(&l, now, &v, 150, 0).contains("(Mon 01-05)"));
        assert!(row(&l, now, &v, 150, 0).contains("$400 of $800"));
    }

    #[test]
    fn a_row_never_overflows_and_keeps_the_percent_and_verdict() {
        let l = limit(50.0);
        let v = view_with(vec![l.clone()]);

        for width in 40..=160 {
            // Act
            let line = row(&l, 40 * HOUR, &v, width, 0);

            // Assert
            assert!(line.chars().count() <= width, "{width}: {line}");
            assert!(line.contains("50%"), "{width}: {line}");
            assert!(line.contains("hits it"), "{width}: {line}");
        }
    }

    #[test]
    fn the_bar_grows_with_the_header_width_until_its_cap() {
        let l = limit(50.0);
        let v = view_with(vec![l.clone()]);
        let bar = |w: usize| {
            row(&l, 40 * HOUR, &v, w, 0)
                .chars()
                .skip(HEAD_WIDTH)
                .take_while(|c| matches!(c, '#' | '-' | '|'))
                .count()
        };

        assert!(bar(80) < bar(120));
        assert!(bar(120) <= bar(150));
        assert_eq!(bar(400), BAR_MAX);
    }

    #[test]
    fn rows_drawn_together_share_one_bar_width() {
        let (a, b) = (budget(400.0), limit(30.0));
        let v = view_with(vec![a.clone(), b.clone()]);
        let width = 100;
        let widest = [&a, &b]
            .iter()
            .map(|l| total(&limit_tail(&pal(), Icon::Off, l, 40 * HOUR, &v, width)))
            .max()
            .unwrap_or(0);
        let bar = |l: &Limit| {
            row(l, 40 * HOUR, &v, width, widest)
                .chars()
                .skip(HEAD_WIDTH)
                .take_while(|c| matches!(c, '#' | '-' | '|'))
                .count()
        };

        assert_eq!(bar(&a), bar(&b));
    }

    #[test]
    fn the_countdown_counts_days_from_two_days_out_and_then_hours() {
        assert_eq!(countdown(9 * SECONDS_PER_DAY + 3 * HOUR), "9d");
        assert_eq!(countdown(2 * SECONDS_PER_DAY), "2d");
        assert_eq!(countdown(SECONDS_PER_DAY + 5 * HOUR), "1d 5h");
        assert_eq!(countdown(2 * HOUR + 600), "2h 10m");
        assert_eq!(countdown(-5), "0m");
    }

    #[test]
    fn dollars_drop_the_cents_from_one_hundred_up() {
        assert_eq!(dollars(412.4), "$412");
        assert_eq!(dollars(1234.5), "$1,235");
        assert_eq!(dollars(41.2), "$41.20");
        assert_eq!(dollars(0.0), "$0.00");
    }

    #[test]
    fn a_stale_reading_is_named_on_the_row() {
        let mut v = view_with(vec![limit(50.0)]);
        let mut l = limit(50.0);
        l.origin = Origin::Measured;
        v.stale = true;
        assert_eq!(stale_text(&l, &v).as_deref(), Some("stale"));
        l.origin = Origin::Cached;
        v.cache_age = Some(7 * HOUR);
        assert_eq!(stale_text(&l, &v).as_deref(), Some("stale 7h 0m"));
        v.cache_age = Some(HOUR);
        assert_eq!(stale_text(&l, &v), None);
    }

    #[test]
    fn the_figures_line_keeps_what_is_left_and_drops_the_average_first() {
        let l = limit(50.0);
        let v = view_with(vec![l.clone()]);
        let now = 40 * HOUR;
        let at = |w: usize| text(&figures_line(&pal(), &l, now, &v, w).spans);

        let wide = at(150);
        let narrow = at(30);
        let no_average = at(100);

        assert!(!no_average.contains("avg"), "{no_average}");
        assert!(no_average.contains("manual, local"), "{no_average}");
        assert!(wide.contains("left 50%"), "{wide}");
        assert!(wide.contains("allow"), "{wide}");
        assert!(wide.contains("avg"), "{wide}");
        assert!(wide.contains("proj"), "{wide}");
        assert!(wide.contains("pace +10pt"), "{wide}");
        assert!(wide.contains("manual, local"), "{wide}");
        assert!(narrow.contains("left 50%"), "{narrow}");
        assert!(!narrow.contains("manual"), "{narrow}");
        assert!(!narrow.contains("allow"), "{narrow}");
    }

    #[test]
    fn the_windows_note_has_a_short_form_for_each_reason() {
        use crate::limits_source::{windows_note, Sources};
        let sources = Sources::default();
        let note = windows_note(&sources, &[]).expect("a plan without windows has a note");

        assert_eq!(
            windows_short(note),
            "5 hour and weekly windows: not on this plan"
        );
        assert!(windows_short("x, no recent reading, x").ends_with("no recent reading"));
        assert!(windows_short("Enterprise").ends_with("not used on Enterprise"));
    }

    #[test]
    fn the_spend_change_is_a_signed_percent_or_new() {
        assert_eq!(change_text(150.0, 100.0), "+50%");
        assert_eq!(change_text(50.0, 100.0), "-50%");
        assert_eq!(change_text(5.0, 0.0), "new");
    }
}
