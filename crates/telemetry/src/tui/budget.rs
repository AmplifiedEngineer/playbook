// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Budget panel: each known limit with what is used, left, the average
//! and allowed spend per hour and per day, the projection and the state, then
//! the burn rate of the period on screen against the previous one. The tag on
//! every line says where the number comes from: `measured` is read from Claude
//! Code's own statusline input, `manual` is a `usage.budget.*` config value,
//! `cached` is read from the usage numbers Claude Code keeps in its config
//! file (with how old they are), `local history` is estimated from the session transcripts.

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
fn tag(origin: Origin, stale: bool, cache_age: Option<i64>) -> String {
    match (origin, stale) {
        (Origin::Measured, true) => "measured, stale".to_string(),
        (Origin::Measured, _) => "measured".to_string(),
        (Origin::Manual, _) => "manual".to_string(),
        (Origin::Cached, _) => match cache_age {
            // Past the freshness limit the cache's used amount is dropped and
            // the bar shows this machine's estimate, which misses other
            // machines and the web apps.
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
    stale: bool,
    cache_age: Option<i64>,
    width: usize,
) -> Line<'static> {
    let tag = tag(l.origin, stale, cache_age);
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
        lines.push(limit_line(
            pal,
            icon,
            l,
            data.now,
            v.stale,
            v.cache_age,
            width,
        ));
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
