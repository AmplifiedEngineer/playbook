// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Budget panel: each known limit with what is used, left, the average
//! and allowed spend per hour and per day, the projection and the state, then
//! the burn rate of the period on screen against the previous one. The tag on
//! every line says where the number comes from: `measured` is read from Claude
//! Code's own statusline input, `manual` is a `usage.budget.*` config value,
//! `cached` is read from the usage numbers Claude Code keeps in its config
//! file (with how old they are; past six hours this machine's messages since
//! the read are added), `local history` is estimated from the session transcripts.

use super::data::{Data, LimitsView};
use super::fmt::{fit, money};
use super::icon::{Cue, Icon};
use super::theme::{Ink, Palette};
use crate::aggregate::date_key;
use crate::limits::{Burn, Limit, LimitPeriod, Origin, State, Unit};
use crate::limits_source::STALE_SECONDS;
use crate::period::elapsed_text;
use crate::query::{clock, day_of};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;

/// Border plus three lines: two limits and the burn rate.
pub const HEIGHT: u16 = 5;
/// The most limits drawn. The rest are counted in the title.
const MAX_LIMITS: usize = 2;
const BAR: usize = 10;

fn fg(pal: &Palette, ink: Ink) -> Style {
    Style::default().fg(pal.color(ink))
}

/// `10-12 09:30` for a second, in the chosen time zone.
pub(super) fn stamp(at: i64) -> String {
    let date = date_key(day_of(at));
    format!("{} {}", &date[5..], &clock(at)[..5])
}

fn amount(unit: Unit, n: f64) -> String {
    match unit {
        Unit::Dollars => money(n),
        Unit::Percent => format!("{n:.0}%"),
    }
}

fn rate(unit: Unit, n: Option<f64>) -> String {
    match (unit, n) {
        (_, None) => "n/a".to_string(),
        (Unit::Dollars, Some(n)) => money(n),
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

/// A piece of a line. When the line is too wide, the pieces with the highest
/// `drop` go first. `0` is never dropped.
struct Seg {
    drop: u8,
    spans: Vec<Span<'static>>,
}

impl Seg {
    fn text(drop: u8, text: String, style: Style) -> Seg {
        Seg {
            drop,
            spans: vec![Span::styled(text, style)],
        }
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|s| s.content.chars().count()).sum()
    }
}

/// The segments joined by two spaces, the widest-dropping ones removed until
/// the line fits `width`.
fn join(mut segs: Vec<Seg>, width: usize) -> Line<'static> {
    let total = |s: &[Seg]| s.iter().map(Seg::width).sum::<usize>() + 2 * s.len().saturating_sub(1);
    while total(&segs) > width {
        let Some(worst) = segs.iter().map(|s| s.drop).max().filter(|d| *d > 0) else {
            break;
        };
        if let Some(i) = segs.iter().rposition(|s| s.drop == worst) {
            segs.remove(i);
        }
    }
    let mut spans = Vec::new();
    for (i, seg) in segs.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.extend(seg.spans);
    }
    Line::from(spans)
}

fn state_seg(pal: &Palette, icon: Icon, l: &Limit, now: i64) -> (Seg, Option<State>) {
    let Some(h) = l.headroom(now) else {
        return (Seg::text(0, "no limit".into(), fg(pal, pal.inactive)), None);
    };
    let (cue, text, ink) = match h.state {
        State::Exceeded => (Cue::Alert, "limit reached".to_string(), pal.error),
        State::WillExceed { at } => (
            Cue::Alert,
            format!("will exceed ~{} {}", stamp(at), crate::tz::label()),
            pal.error,
        ),
        State::OnTrack => (Cue::Watch, "on track".to_string(), pal.warn),
        State::Under => (Cue::Fine, "under".to_string(), pal.live),
        State::Unknown => (
            Cue::Unknown,
            "too early to project".to_string(),
            pal.inactive,
        ),
    };
    let weight = if cue == Cue::Alert {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let text = format!("{}{text}", icon.mark(cue));
    (
        Seg::text(2, text, fg(pal, ink).add_modifier(weight)),
        Some(h.state),
    )
}

/// The source label of a limit: where it came from, and how old when that
/// matters.
fn tag(origin: Origin, stale: bool, cache_age: Option<i64>, plus_local: bool) -> String {
    match (origin, stale) {
        (Origin::Measured, true) => "measured, stale".to_string(),
        (Origin::Measured, _) => "measured".to_string(),
        // A manual budget is counted against the transcripts of this machine.
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

fn limit_line(
    pal: &Palette,
    icon: Icon,
    l: &Limit,
    now: i64,
    v: &LimitsView,
    width: usize,
) -> Line<'static> {
    let tag = tag(l.origin, v.stale, v.cache_age, v.cached_plus_local);
    let mut segs = vec![Seg {
        drop: 0,
        spans: vec![
            Span::styled(
                name(l).to_string(),
                fg(pal, pal.title).add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(format!(" [{tag}]"), fg(pal, pal.fg)),
        ],
    }];
    let pct = if l.limit > 0.0 {
        (l.used / l.limit * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let filled = ((pct / 100.0 * BAR as f64).round() as usize).min(BAR);
    let mut bar = Vec::new();
    for i in 0..BAR {
        bar.push(if i < filled {
            Span::styled(
                "\u{25a0}",
                Style::default().fg(pal.graded(pal.process, (i + 1) as f64 / BAR as f64 * 100.0)),
            )
        } else {
            Span::styled("\u{b7}", fg(pal, pal.meter_bg))
        });
    }
    bar.push(Span::raw(format!(
        " {}/{}",
        amount(l.unit, l.used),
        amount(l.unit, l.limit)
    )));
    segs.push(Seg {
        drop: 1,
        spans: bar,
    });
    let (state, _) = state_seg(pal, icon, l, now);
    segs.push(state);
    if let Some(h) = l.headroom(now) {
        let plain = fg(pal, pal.fg);
        segs.push(Seg::text(
            3,
            format!("left {}", amount(l.unit, h.remaining)),
            plain,
        ));
        segs.push(Seg::text(
            4,
            format!(
                "allow {}/h {}/d",
                rate(l.unit, h.allowed_per_hour),
                rate(l.unit, h.allowed_per_day)
            ),
            plain,
        ));
        segs.push(Seg::text(
            5,
            format!(
                "avg {}/h {}/d",
                rate(l.unit, h.avg_per_hour),
                rate(l.unit, h.avg_per_day)
            ),
            plain,
        ));
        segs.push(Seg::text(
            6,
            format!("proj {}", rate(l.unit, h.projected)),
            plain,
        ));
    }
    segs.push(Seg::text(
        7,
        format!("resets {} {}", stamp(l.end), crate::tz::label()),
        fg(pal, pal.fg),
    ));
    join(segs, width)
}

/// What Claude Code reported as used, with its age, when no bar above shows
/// that figure: a bar from a manual budget or a stale cache counts only this
/// machine. A fresh cache replaces the estimate, so it needs no line.
fn reported_line(v: &LimitsView, now: i64) -> Option<String> {
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
    Some(format!(
        "Claude reported {} used this month, read {} ago{}; run /usage in Claude Code to refresh it",
        crate::limits_source::amount(Unit::Dollars, used),
        elapsed_text(age),
        if stale { " (stale)" } else { "" }
    ))
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

fn burn_text(b: &Burn) -> String {
    match (b.per_hour, b.per_day) {
        (Some(h), Some(d)) => format!("{}/h {}/day", money(h), money(d)),
        _ => "n/a".to_string(),
    }
}

fn burn_line(pal: &Palette, v: &LimitsView, width: usize) -> Line<'static> {
    let segs = vec![
        Seg {
            drop: 0,
            spans: vec![
                Span::styled(
                    format!("{} spend", v.period.label()),
                    fg(pal, pal.title).add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(" [local history]", fg(pal, pal.fg)),
            ],
        },
        Seg::text(
            1,
            format!("{} so far", money(v.burn.cost_usd)),
            fg(pal, pal.fg),
        ),
        Seg::text(2, format!("avg {}", burn_text(&v.burn)), fg(pal, pal.fg)),
        Seg::text(
            3,
            format!("{} {}", v.period.previous_label(), burn_text(&v.previous)),
            fg(pal, pal.fg),
        ),
    ];
    join(segs, width)
}

/// Draws the panel. Nothing is drawn when `area` is empty.
pub fn render(frame: &mut Frame, area: Rect, data: &Data, pal: &Palette, icon: Icon) {
    if area.height == 0 {
        return;
    }
    let v = &data.limits;
    let edge = fg(pal, pal.box_cpu);
    let more = v.limits.len().saturating_sub(MAX_LIMITS);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::from(vec![
            Span::styled("\u{2500}\u{2524}", edge),
            Span::styled("Budget", fg(pal, pal.title)),
            Span::styled("\u{251c}", edge),
        ]));
    let note = if more > 0 {
        format!("{}  +{more} more in --json", crate::tz::label())
    } else {
        format!("{}  all projects", crate::tz::label())
    };
    block = block.title(
        Line::from(vec![
            Span::styled("\u{2524}", edge),
            Span::styled(note, fg(pal, pal.fg)),
            Span::styled("\u{251c}\u{2500}", edge),
        ])
        .right_aligned(),
    );
    let width = usize::from(area.width.saturating_sub(2));
    let mut lines: Vec<Line<'static>> = Vec::new();
    if v.limits.is_empty() {
        let dim = fg(pal, pal.inactive);
        lines.push(Line::styled(
            fit(
                "Limit unknown: playbook config set --global usage.budget.monthlyUsd <usd> (or weeklyUsd),",
                width,
            ),
            dim,
        ));
        lines.push(Line::styled(
            fit(
                v.windows_note.unwrap_or(
                    "or use Claude Code on Pro or Max with the playbook statusline for the 5 hour and weekly limits.",
                ),
                width,
            ),
            dim,
        ));
    }
    for l in v.limits.iter().take(MAX_LIMITS) {
        lines.push(limit_line(pal, icon, l, data.now, v, width));
    }
    if let Some(line) = both_budgets_line(v) {
        lines.push(Line::styled(fit(&line, width), fg(pal, pal.warn)));
    }
    if let Some(line) = reported_line(v, data.now) {
        lines.push(Line::styled(fit(&line, width), fg(pal, pal.fg)));
    }
    if let Some(note) = v.windows_note.filter(|_| !v.limits.is_empty()) {
        lines.push(Line::styled(fit(note, width), fg(pal, pal.inactive)));
    }
    lines.push(burn_line(pal, v, width));
    // The burn rate is always the last line, so two limits never push it out.
    let room = usize::from(area.height.saturating_sub(2));
    if lines.len() > room {
        let burn = lines.pop();
        lines.truncate(room.saturating_sub(1));
        lines.extend(burn);
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{ColorDepth, Theme};

    const HOUR: i64 = 3600;

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

    fn shown(icon: Icon, used: f64, now: i64) -> (String, Modifier) {
        let pal = Theme::Standard.palette(ColorDepth::True);
        let (seg, _) = state_seg(&pal, icon, &limit(used), now);
        let span = &seg.spans[0];
        (span.content.to_string(), span.style.add_modifier)
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
            (100.0, ten_hours, "! limit reached"),
            (20.0, ten_hours, "! will exceed"),
            (9.0, ten_hours, "~ on track"),
            (5.0, ten_hours, "+ under"),
            (5.0, HOUR / 2, "? too early to project"),
        ];

        for (used, now, want) in cases {
            // Act
            let (text, _) = shown(Icon::Off, used, now);

            // Assert
            assert!(
                text.starts_with(want),
                "{text:?} should start with {want:?}"
            );
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
        assert!(reached.0.starts_with("\u{25b2} limit reached"));
        assert!(reached.1.contains(Modifier::BOLD));
        assert!(watch.0.starts_with("\u{25c6} on track"));
        assert!(!watch.1.contains(Modifier::BOLD));
    }
}
