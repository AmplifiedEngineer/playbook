// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Burn-up panel: how much of one limit is used over its period as filled
//! columns, a dotted projection to the period end, horizontal lines at 50, 80
//! and 100 percent of the limit, and markers where those are crossed. A dollar
//! limit follows the local cost per day. A percent window follows the
//! readings, or a straight line when there are none. Every line has its own
//! glyph, so the chart reads the same without color. Where the terminal can
//! show Braille the plot is drawn as dots, four times as fine vertically; the
//! solid glyphs stay as the fallback. Under `MIN_CHART_WIDTH` columns, or
//! without room for a few rows, the panel shows the plain table instead.

use super::budget;
use super::data::Data;
use super::dots::{line_dots, DotGrid, DOT_COLS};
use super::fmt::{axis_money, fit};
use super::icon::Icon;
use super::theme::{Ink, Palette};
use crate::burnup::Basis;
use crate::burnup::Burnup;
use crate::limits::{CrossState, Limit, LimitPeriod, Unit};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;

/// Panel columns under which the table replaces the chart.
pub const MIN_CHART_WIDTH: u16 = 40;
/// The fewest plot rows worth drawing.
const MIN_PLOT_ROWS: usize = 4;
/// Columns left of the plot for the dollar labels.
const GUTTER: usize = 8;
/// The plot reaches this far above the highest value.
const HEADROOM: f64 = 1.05;
/// The thresholds drawn, in percent of the limit.
const NEAR: f64 = 80.0;
const PERCENT: f64 = 100.0;
/// The projection keeps one dot in this many along its path.
const PROJECTION_STEP: usize = 2;
/// The glyph in the legend for the used area drawn as dots.
const DOT_ACTUAL: char = '\u{28ff}';

const ACTUAL: char = '\u{2588}';
const PROJECTION: char = '\u{00b7}';
const HALF_LINE: char = '\u{2504}';
const NEAR_LINE: char = '\u{2508}';
const LIMIT_LINE: char = '\u{2501}';
const BILLED: char = '\u{25c6}';
const MARKER: char = '\u{25b2}';
const RATE_LIMITED: char = '\u{00d7}';

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Empty,
    Actual,
    Projection,
    Half,
    Near,
    Limit,
    Billed,
    Marker,
    RateLimited,
}

impl Role {
    fn glyph(self) -> char {
        match self {
            Role::Empty => ' ',
            Role::Actual => ACTUAL,
            Role::Projection => PROJECTION,
            Role::Half => HALF_LINE,
            Role::Near => NEAR_LINE,
            Role::Limit => LIMIT_LINE,
            Role::Billed => BILLED,
            Role::Marker => MARKER,
            Role::RateLimited => RATE_LIMITED,
        }
    }

    fn ink(self, pal: &Palette) -> Ink {
        match self {
            Role::Empty | Role::Half | Role::Near => pal.inactive,
            Role::Actual => pal.live,
            Role::Projection => pal.warn,
            Role::Limit | Role::RateLimited => pal.error,
            Role::Billed => pal.hi,
            Role::Marker => pal.title,
        }
    }
}

/// An axis label in the unit of the chart.
fn axis(b: &Burnup, value: f64) -> String {
    match b.unit {
        Unit::Dollars => axis_money(value),
        Unit::Percent => format!("{value:.0}%"),
    }
}

/// What the chart is of, for the title.
fn subject(b: &Burnup, zone_label: &str) -> String {
    match b.unit {
        Unit::Dollars => format!("spend this period, {zone_label}"),
        Unit::Percent => format!(
            "{} used, {zone_label}",
            match b.period {
                LimitPeriod::FiveHour => "5 hour window",
                LimitPeriod::SevenDay => "weekly window",
                _ => "window",
            }
        ),
    }
}

/// Table lines that are not days: the header and the three crossings.
const TABLE_CHROME: usize = 4;

/// The scale and size of one plot.
struct Plot {
    width: usize,
    height: usize,
    top: f64,
}

impl Plot {
    fn new(b: &Burnup, width: usize, height: usize) -> Plot {
        let top = [
            b.limit,
            b.projected_end,
            Some(b.total),
            b.billed.map(|(_, usd)| usd),
        ]
        .into_iter()
        .flatten()
        .fold(0.0, f64::max)
            * HEADROOM;
        Plot {
            width,
            height,
            top: if top > 0.0 { top } else { 1.0 },
        }
    }

    /// The row of `usd`, the top row being 0.
    fn row(&self, usd: f64) -> usize {
        let from_bottom = (usd / self.top * (self.height - 1) as f64).round().max(0.0) as usize;
        (self.height - 1).saturating_sub(from_bottom.min(self.height - 1))
    }

    /// The value at `row`, the inverse of `row`.
    fn value(&self, row: usize) -> f64 {
        self.top * (self.height - 1 - row.min(self.height - 1)) as f64
            / (self.height - 1).max(1) as f64
    }

    fn col(&self, b: &Burnup, at: i64) -> usize {
        let share = (at - b.start) as f64 / (b.end - b.start) as f64;
        ((share * (self.width - 1) as f64).round().max(0.0) as usize).min(self.width - 1)
    }

    fn time(&self, b: &Burnup, col: usize) -> i64 {
        b.start + ((b.end - b.start) as f64 * col as f64 / (self.width - 1) as f64) as i64
    }
}

/// The plot as rows of roles, top row first.
fn cells(b: &Burnup, plot: &Plot) -> Vec<Vec<Role>> {
    let mut grid = vec![vec![Role::Empty; plot.width]; plot.height];
    let times: Vec<i64> = (0..plot.width).map(|col| plot.time(b, col)).collect();
    for (col, at) in times.iter().enumerate().filter(|(_, at)| **at <= b.now) {
        if let Some(v) = value_at(b, *at) {
            for row in grid.iter_mut().skip(plot.row(v)) {
                row[col] = Role::Actual;
            }
        }
    }
    for (pct, value) in b.cutoffs() {
        let role = if pct >= PERCENT {
            Role::Limit
        } else if pct >= NEAR {
            Role::Near
        } else {
            Role::Half
        };
        // A line crosses the used area without erasing it.
        for cell in grid[plot.row(value)]
            .iter_mut()
            .filter(|cell| **cell == Role::Empty)
        {
            *cell = role;
        }
    }
    for (col, at) in times.iter().enumerate().filter(|(_, at)| **at > b.now) {
        if let Some(v) = b.projected_at(*at) {
            grid[plot.row(v)][col] = Role::Projection;
        }
    }
    if let Some(limit) = b.limit {
        for c in &b.crossings {
            if let CrossState::Crossed { at } | CrossState::Projected { at } = c.state {
                grid[plot.row(limit * c.pct / PERCENT)][plot.col(b, at)] = Role::Marker;
            }
        }
    }
    if let Some((at, usd)) = b.billed {
        grid[plot.row(usd)][plot.col(b, at)] = Role::Billed;
    }
    for at in &b.rate_limited {
        let used = value_at(b, *at).unwrap_or(0.0);
        grid[plot.row(used)][plot.col(b, *at)] = Role::RateLimited;
    }
    grid
}

/// What is used at `at`: the day's total for a dollar limit, the line
/// between the points for a percent window. `None` before the first point.
fn value_at(b: &Burnup, at: i64) -> Option<f64> {
    if b.basis == Basis::Daily {
        let day = (b.zone.day_of(at) - b.zone.day_of(b.start)).max(0) as usize;
        return b.actual.get(day).copied();
    }
    let next = b.points.iter().position(|(t, _)| *t > at);
    match next {
        None => b.points.last().map(|(_, v)| *v),
        Some(0) => None,
        Some(i) => {
            let ((t0, v0), (t1, v1)) = (b.points[i - 1], b.points[i]);
            let share = (at - t0) as f64 / (t1 - t0) as f64;
            Some(v0 + (v1 - v0) * share)
        }
    }
}

/// `MM-DD` of a second for a period of days or a window longer than a day,
/// `HH:MM` for a window of a day or less, in the zone of the chart.
fn short(b: &Burnup, at: i64) -> String {
    let iso = b.zone.iso(at);
    match b.unit {
        Unit::Dollars => iso[5..10].to_string(),
        Unit::Percent if b.end - b.start > 86_400 => iso[5..10].to_string(),
        Unit::Percent => iso[11..16].to_string(),
    }
}

/// The crossings as one line: `50% 01-11  80% 01-17  100% beyond`.
fn crossing_line(b: &Burnup) -> String {
    if b.crossings.is_empty() {
        return "no limit known".to_string();
    }
    b.crossings
        .iter()
        .map(|c| match c.state {
            CrossState::Crossed { at } => format!("{}% {} {MARKER}", c.pct, short(b, at)),
            CrossState::Projected { at } => format!("{}% ~{} {MARKER}", c.pct, short(b, at)),
            CrossState::Beyond { .. } => format!("{}% beyond", c.pct),
            CrossState::Unknown => format!("{}% too early", c.pct),
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// What each glyph draws.
fn legend(b: &Burnup, dots: bool) -> String {
    let used = match b.unit {
        Unit::Dollars => "spent",
        Unit::Percent => "used",
    };
    let mut parts = vec![
        format!("{} {used}", if dots { DOT_ACTUAL } else { ACTUAL }),
        format!("{PROJECTION} projected"),
    ];
    if b.limit.is_some() {
        parts.push(format!(
            "{LIMIT_LINE} limit  {NEAR_LINE} 80%  {HALF_LINE} 50%"
        ));
        parts.push(format!("{MARKER} crossing"));
    }
    if b.billed.is_some() {
        parts.push(format!("{BILLED} reported by Claude"));
    }
    if !b.rate_limited.is_empty() {
        parts.push(format!("{RATE_LIMITED} rate limited"));
    }
    parts.join("  ")
}

/// The window percentages, when any window is known.
fn windows_line(limits: &[Limit]) -> Option<String> {
    let parts: Vec<String> = limits
        .iter()
        .filter(|l| {
            l.unit == Unit::Percent
                && matches!(l.period, LimitPeriod::FiveHour | LimitPeriod::SevenDay)
        })
        .map(|l| format!("{} {:.0}%", budget::name(l), l.used))
        .collect();
    (!parts.is_empty()).then(|| parts.join("  "))
}

fn style(pal: &Palette, ink: Ink) -> Style {
    Style::default().fg(pal.color(ink))
}

/// The plot as spans of whole-cell glyphs, top row first.
fn solid_rows(b: &Burnup, pal: &Palette, plot: &Plot) -> Vec<Vec<Span<'static>>> {
    cells(b, plot)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|r| Span::styled(r.glyph().to_string(), style(pal, r.ink(pal))))
                .collect()
        })
        .collect()
}

/// What a dot draws. A cell shows the fill, else the projection, else the
/// reference lines. The order of the variants only picks the role among dots
/// of one of those layers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dot {
    /// The used area at this whole percent of the limit.
    Used(u8),
    Half,
    Near,
    Projection,
    Limit,
}

impl Dot {
    fn ink(self, pal: &Palette) -> Ink {
        match self {
            Dot::Used(pct) => pal.usage_ink(f64::from(pct)),
            Dot::Limit => pal.error,
            Dot::Half | Dot::Near => pal.inactive,
            Dot::Projection => pal.hi,
        }
    }

    /// The used area at `percent` of the limit, to the nearest whole percent.
    fn used(percent: f64) -> Dot {
        Dot::Used(percent.clamp(0.0, 255.0).round() as u8)
    }
}

/// The last dot column of the used area, the last one at or before `now`.
fn last_filled(b: &Burnup, fine: &Plot) -> Option<usize> {
    (0..fine.width).rev().find(|x| fine.time(b, *x) <= b.now)
}

/// What the used area shows in dot column `x`. The last column shows the
/// value at `now`, so a day boundary between it and `now` does not leave the
/// fill a step short of the projection.
fn fill_value(b: &Burnup, fine: &Plot, x: usize) -> Option<f64> {
    let at = if last_filled(b, fine) == Some(x) {
        b.now
    } else {
        fine.time(b, x)
    };
    value_at(b, at).filter(|_| at <= b.now)
}

/// The dots of the projection, one connected path that starts in the dot
/// column right of the used area at the height of its top, and ends at the
/// period end. None when there is no projection. Each dot touches the next, so
/// a steep stretch has no gap.
fn projection_path(b: &Burnup, fine: &Plot) -> Vec<(usize, usize)> {
    let Some(end) = b.projected_end else {
        return Vec::new();
    };
    let Some(top) = last_filled(b, fine)
        .and_then(|x| fill_value(b, fine, x))
        .map(|v| fine.row(v))
    else {
        return Vec::new();
    };
    // The fill owns its last cell, so the projection starts in the next one.
    let start = last_filled(b, fine).map_or(0, |x| (x / DOT_COLS + 1) * DOT_COLS);
    if start >= fine.width {
        return Vec::new();
    }
    line_dots((start, top), (fine.width - 1, fine.row(end)))
}

/// Every second dot of `path`, so the projection reads as dotted.
fn dotted(path: &[(usize, usize)]) -> impl Iterator<Item = (usize, usize)> + '_ {
    path.iter().copied().step_by(PROJECTION_STEP)
}

/// Fills each dot column up to `now` solid from the bottom to its value, each
/// dot row inked by its height against the limit. The top dot row of a column
/// is the value itself, so the edge of the fill is the used line.
fn fill_used(b: &Burnup, fine: &Plot, grid: &mut DotGrid<Dot>) {
    let last = fine.height - 1;
    for x in 0..fine.width {
        let Some(value) = fill_value(b, fine, x) else {
            continue;
        };
        for y in fine.row(value)..=last {
            let percent = b.limit.map_or(0.0, |l| fine.value(y) / l * PERCENT);
            grid.set(x, y, Dot::used(percent));
        }
    }
}

/// The plot as spans of Braille cells, top row first. The used area is a solid
/// fill, grown from the bottom of each dot column to its value. The dotted
/// projection takes the cells the fill leaves free, and the reference lines
/// the cells neither touches, so nothing cuts the fill or the projection. Markers and the other point glyphs stay
/// whole-cell glyphs over the dots.
fn dot_rows(b: &Burnup, pal: &Palette, plot: &Plot) -> Vec<Vec<Span<'static>>> {
    let fine = Plot {
        width: plot.width * 2,
        height: plot.height * 4,
        top: plot.top,
    };
    let mut fill = DotGrid::new(plot.width, plot.height);
    fill_used(b, &fine, &mut fill);
    let mut projection = DotGrid::new(plot.width, plot.height);
    for (x, y) in dotted(&projection_path(b, &fine)) {
        projection.set(x, y, Dot::Projection);
    }
    let mut dots = DotGrid::new(plot.width, plot.height);
    let mut glyphs = vec![vec![Role::Empty; plot.width]; plot.height];
    for (y, row) in cells(b, &fine).into_iter().enumerate() {
        for (x, role) in row.into_iter().enumerate() {
            match role {
                Role::Near if x % 2 == 0 => dots.set(x, y, Dot::Near),
                Role::Half if x % 4 == 0 => dots.set(x, y, Dot::Half),
                Role::Limit => dots.set(x, y, Dot::Limit),
                Role::Billed | Role::Marker | Role::RateLimited => {
                    glyphs[y / 4][x / 2] = role;
                }
                Role::Empty | Role::Actual | Role::Projection | Role::Near | Role::Half => {}
            }
        }
    }
    (0..plot.height)
        .map(|row| {
            (0..plot.width)
                .map(|col| {
                    let drawn = fill
                        .cell(col, row)
                        .or_else(|| projection.cell(col, row))
                        .or_else(|| dots.cell(col, row));
                    match (glyphs[row][col], drawn) {
                        (Role::Empty, Some((ch, dot))) => {
                            Span::styled(ch.to_string(), style(pal, dot.ink(pal)))
                        }
                        (Role::Empty, None) => Span::raw(" "),
                        (role, _) => {
                            Span::styled(role.glyph().to_string(), style(pal, role.ink(pal)))
                        }
                    }
                })
                .collect()
        })
        .collect()
}

/// Whether the plot may use Braille dots: the terminal shows symbols (the
/// icon is not off), the theme has color, and `PBTOP_DOTS=0` was not set for
/// a font that draws Braille badly.
pub fn braille_ok(icon: Icon, mono: bool, dots_off: bool) -> bool {
    icon != Icon::Off && !mono && !dots_off
}

/// The chart lines for a plot area `width` columns wide and `rows` tall,
/// including the date axis and the legend.
fn chart_lines(
    b: &Burnup,
    pal: &Palette,
    width: usize,
    rows: usize,
    windows: Option<String>,
    dots: bool,
) -> Vec<Line<'static>> {
    let legend_rows = 2 + usize::from(windows.is_some());
    let plot = Plot::new(b, width - GUTTER, rows - 1 - legend_rows);
    let plot_rows = if dots {
        dot_rows(b, pal, &plot)
    } else {
        solid_rows(b, pal, &plot)
    };
    let mut out = Vec::new();
    for (i, row) in plot_rows.into_iter().enumerate() {
        let label = if i == 0 {
            axis(b, plot.top)
        } else if i == plot.height - 1 {
            axis(b, 0.0)
        } else {
            String::new()
        };
        let mut spans = vec![Span::styled(
            format!("{label:>w$} ", w = GUTTER - 1),
            style(pal, pal.inactive),
        )];
        spans.extend(row);
        out.push(Line::from(spans));
    }
    let (first, last) = (short(b, b.start), short(b, b.end - 1));
    let gap = plot.width.saturating_sub(first.len() + last.len());
    out.push(Line::styled(
        format!("{:>w$}{first}{}{last}", "", " ".repeat(gap), w = GUTTER),
        style(pal, pal.inactive),
    ));
    out.push(Line::styled(
        fit(&crossing_line(b), width),
        style(pal, pal.fg),
    ));
    out.push(Line::styled(
        fit(&legend(b, dots), width),
        style(pal, pal.inactive),
    ));
    if let Some(w) = windows {
        out.push(Line::styled(fit(&w, width), style(pal, pal.inactive)));
    }
    out
}

/// Draws the panel for one burn-up and the limits shown beside it.
pub(super) fn render_burnup(
    frame: &mut Frame,
    area: Rect,
    b: Option<&Burnup>,
    limits: &[Limit],
    pal: &Palette,
    dots: bool,
) {
    if area.height == 0 {
        return;
    }
    let zone_label = b.map_or_else(crate::tz::label, |b| b.zone.label());
    let edge = style(pal, pal.box_cpu);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::from(vec![
            Span::styled("\u{2500}\u{2524}", edge),
            Span::styled("Burn-up", style(pal, pal.title)),
            Span::styled("\u{251c}", edge),
        ]))
        .title(
            Line::from(vec![
                Span::styled("\u{2524}", edge),
                Span::styled(
                    b.map_or_else(
                        || format!("spend this period, {zone_label}"),
                        |b| subject(b, zone_label),
                    ),
                    style(pal, pal.fg),
                ),
                Span::styled("\u{251c}\u{2500}", edge),
            ])
            .right_aligned(),
        );
    let inner = block.inner(area);
    let (width, rows) = (usize::from(inner.width), usize::from(inner.height));
    let windows = [windows_line(limits), b.and_then(Burnup::rate_limited_line)]
        .into_iter()
        .flatten()
        .reduce(|a, line| format!("{a}  {line}"));
    let lines = match b {
        None => vec![Line::styled("no burn-up yet", style(pal, pal.inactive))],
        Some(b) if b.points.is_empty() => {
            vec![Line::styled(
                "the period has not started",
                style(pal, pal.inactive),
            )]
        }
        Some(b)
            if inner.width >= MIN_CHART_WIDTH
                && rows >= MIN_PLOT_ROWS + 4 + usize::from(windows.is_some()) =>
        {
            chart_lines(b, pal, width, rows, windows, dots)
        }
        Some(b) => b
            .table(rows.saturating_sub(TABLE_CHROME).max(1))
            .into_iter()
            .chain(b.crossing_lines())
            .map(|t| Line::styled(fit(&t, width), style(pal, pal.fg)))
            .collect(),
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub fn render(frame: &mut Frame, area: Rect, data: &Data, pal: &Palette, dots: bool) {
    render_burnup(
        frame,
        area,
        data.limits.charted(),
        &data.limits.limits,
        pal,
        dots,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::a11y::{contrast, rgb_of, NON_TEXT_CONTRAST};
    use crate::tui::theme::{ColorDepth, Theme};

    #[test]
    fn the_used_area_takes_its_ink_from_the_usage_gradient() {
        let pal = Theme::Standard.palette(ColorDepth::True);
        assert_eq!(Dot::used(0.0).ink(&pal), pal.usage_ink(0.0));
        assert_eq!(Dot::used(69.6).ink(&pal), pal.usage_ink(70.0));
        assert_eq!(Dot::used(400.0).ink(&pal), pal.usage_ink(255.0));
    }

    #[test]
    fn the_used_area_stays_readable_on_a_dark_ground() {
        let pal = Theme::Standard.palette(ColorDepth::True);
        let bg = rgb_of(pal.bg);
        for pct in [0.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0] {
            let ratio = contrast(rgb_of(Dot::used(pct).ink(&pal)), bg);
            assert!(ratio >= NON_TEXT_CONTRAST, "{pct}% is {ratio:.2}");
        }
    }

    fn line(points: Vec<(i64, f64)>, now: i64) -> Burnup {
        Burnup {
            unit: Unit::Percent,
            period: LimitPeriod::FiveHour,
            start: 0,
            end: 1_000,
            now,
            zone: crate::tz::Zone::Utc,
            basis: Basis::Line,
            points,
            actual: Vec::new(),
            total: 0.0,
            projected_end: None,
            limit: Some(100.0),
            billed: None,
            crossings: Vec::new(),
            rate_limited: Vec::new(),
        }
    }

    fn fine(b: &Burnup) -> Plot {
        let p = Plot::new(b, 10, 5);
        Plot {
            width: p.width * 2,
            height: p.height * 4,
            top: p.top,
        }
    }

    /// Whether dot `(x, y)` is set in `grid`.
    fn lit(grid: &DotGrid<Dot>, x: usize, y: usize) -> bool {
        let Some((ch, _)) = grid.cell(x / 2, y / 4) else {
            return false;
        };
        let mut probe = DotGrid::new(1, 1);
        probe.set(x % 2, y % 4, 0u8);
        let bit = probe.cell(0, 0).map_or(0, |(c, _)| c as u32 - 0x2800);
        (ch as u32 - 0x2800) & bit != 0
    }

    fn filled(b: &Burnup) -> (DotGrid<Dot>, Plot) {
        let f = fine(b);
        let mut grid = DotGrid::new(f.width / 2, f.height / 4);
        fill_used(b, &f, &mut grid);
        (grid, f)
    }

    /// A monthly dollar limit with a steady daily spend, so the used area
    /// climbs through the 50 and 80 percent lines.
    fn climbing_month() -> Burnup {
        use crate::limits::Origin;
        const DAY: i64 = 86_400;
        const DAYS: i64 = 20;
        const DAILY_USD: f64 = 22.0;
        const LIMIT_USD: f64 = 600.0;
        const PERIOD_DAYS: i64 = 30;
        let events: Vec<crate::UsageEvent> = (0..DAYS)
            .map(|d| crate::UsageEvent {
                timestamp: d * DAY + 1,
                cost_usd: DAILY_USD,
                ..crate::UsageEvent::default()
            })
            .collect();
        let limit = Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Manual,
            used: DAILY_USD * DAYS as f64,
            limit: LIMIT_USD,
            start: 0,
            end: PERIOD_DAYS * DAY,
        };
        let inputs = crate::burnup::Inputs {
            events: &events,
            readings: &[],
            billed: None,
            now: DAYS * DAY,
        };
        crate::burnup::burnup(&limit, crate::tz::Zone::Utc, inputs).unwrap()
    }

    #[test]
    fn a_cutoff_line_does_not_erase_the_used_area_under_it() {
        // Arrange
        let b = climbing_month();
        let plot = Plot::new(&b, 40, 6);

        // Act
        let grid = cells(&b, &plot);

        // Assert
        let last_used = (0..plot.width)
            .rev()
            .find(|col| grid.iter().any(|row| row[*col] == Role::Actual))
            .unwrap();
        let top = plot.row(b.total);
        for (row, cells) in grid.iter().enumerate().skip(top) {
            assert!(cells[last_used] == Role::Actual, "row {row}");
        }
    }

    #[test]
    fn a_column_is_filled_from_the_bottom_to_its_value_with_no_dot_missing() {
        // Arrange
        let b = line(vec![(0, 10.0), (400, 60.0)], 400);

        // Act
        let (grid, f) = filled(&b);

        // Assert
        for x in 0..f.width {
            let at = f.time(&b, x);
            if at > b.now {
                continue;
            }
            let top = f.row(fill_value(&b, &f, x).unwrap());
            for y in 0..f.height {
                assert_eq!(lit(&grid, x, y), y >= top, "dot ({x}, {y})");
            }
        }
    }

    #[test]
    fn the_fill_top_is_the_value_so_a_steep_rise_has_no_gap_under_it() {
        // Arrange
        let b = line(vec![(0, 0.0), (100, 5.0), (200, 90.0)], 300);

        // Act
        let (grid, f) = filled(&b);

        // Assert
        for x in 0..f.width {
            let at = f.time(&b, x);
            if at <= b.now {
                let top = f.row(value_at(&b, at).unwrap());
                assert!(lit(&grid, x, top), "column {x} lacks its top dot");
                assert!(top == 0 || !lit(&grid, x, top - 1), "column {x} overshoots");
            }
        }
    }

    #[test]
    fn the_fill_draws_nothing_after_now() {
        let b = line(vec![(0, 0.0), (400, 40.0)], 400);
        let (grid, f) = filled(&b);
        assert!((0..f.height).any(|y| lit(&grid, 0, y)));
        assert!((0..f.height).all(|y| !lit(&grid, f.width - 1, y)));
    }

    #[test]
    fn a_reference_line_never_removes_a_fill_dot() {
        // Arrange
        let pal = Theme::Standard.palette(ColorDepth::True);
        let b = line(vec![(0, 10.0), (500, 90.0)], 500);
        let plot = Plot::new(&b, 10, 12);
        let (grid, _) = {
            let f = Plot {
                width: plot.width * 2,
                height: plot.height * 4,
                top: plot.top,
            };
            let mut grid = DotGrid::new(plot.width, plot.height);
            fill_used(&b, &f, &mut grid);
            (grid, f)
        };

        // Act
        let rows = dot_rows(&b, &pal, &plot);

        // Assert
        for (row, spans) in rows.iter().enumerate() {
            for (col, span) in spans.iter().enumerate() {
                if let Some((ch, _)) = grid.cell(col, row) {
                    assert_eq!(span.content, ch.to_string(), "cell ({col}, {row})");
                }
            }
        }
    }

    fn projecting(total: f64, end: f64) -> Burnup {
        let mut b = line(vec![(0, 0.0), (400, total)], 400);
        b.total = total;
        b.projected_end = Some(end);
        b
    }

    #[test]
    fn the_projection_is_one_connected_path_from_the_last_used_value_to_the_end() {
        for end in [45.0, 100.0, 400.0] {
            // Arrange
            let b = projecting(40.0, end);
            let f = fine(&b);

            // Act
            let path = projection_path(&b, &f);

            // Assert
            assert_eq!(path.first().map(|d| d.1), Some(f.row(40.0)), "end {end}");
            assert_eq!(path.last(), Some(&(f.width - 1, f.row(end))), "end {end}");
            assert!(
                path.windows(2)
                    .all(|w| w[0].0.abs_diff(w[1].0) <= 1 && w[0].1.abs_diff(w[1].1) <= 1),
                "gap in {path:?}"
            );
        }
    }

    #[test]
    fn the_projection_starts_right_of_the_fill_at_its_top_even_across_a_day_boundary() {
        // Arrange: a daily series whose last day starts between the last
        // filled dot column and now.
        let mut b = line(Vec::new(), 0);
        b.basis = Basis::Daily;
        b.unit = Unit::Dollars;
        b.start = 0;
        b.end = 10 * 86_400;
        b.now = 3 * 86_400 + 600;
        b.actual = vec![5.0, 10.0, 20.0, 60.0];
        b.total = 60.0;
        b.limit = Some(100.0);
        b.projected_end = Some(90.0);
        let f = Plot {
            width: 200,
            height: 20,
            top: 100.0,
        };
        let (grid, f) = (
            {
                let mut g = DotGrid::new(f.width / 2, f.height / 4);
                fill_used(&b, &f, &mut g);
                g
            },
            f,
        );

        // Act
        let path = projection_path(&b, &f);

        // Assert
        let last = (0..f.width)
            .rev()
            .find(|x| lit(&grid, *x, f.height - 1))
            .unwrap();
        let top = (0..f.height).find(|y| lit(&grid, last, *y)).unwrap();
        assert_eq!(path.first(), Some(&((last / DOT_COLS + 1) * DOT_COLS, top)));
        assert!((0..f.height).all(|y| !lit(&grid, path[0].0, y)));
    }

    #[test]
    fn a_projection_dot_shows_in_the_cell_after_the_fill_for_an_even_and_an_odd_last_column() {
        let pal = Theme::Standard.palette(ColorDepth::True);
        for now in [500, 540] {
            // Arrange
            let mut b = projecting(40.0, 100.0);
            b.now = now;
            let plot = Plot::new(&b, 10, 8);
            let f = Plot {
                width: plot.width * 2,
                height: plot.height * 4,
                top: plot.top,
            };
            let last = last_filled(&b, &f).unwrap();

            // Act
            let rows = dot_rows(&b, &pal, &plot);

            // Assert
            let (x, y) = projection_path(&b, &f)[0];
            let span = &rows[y / 4][x / 2];
            assert_eq!(x / 2, last / 2 + 1, "now {now}, last column {last}");
            assert_eq!(
                span.style.fg,
                Some(pal.color(Dot::Projection.ink(&pal))),
                "now {now}, last column {last}: {:?}",
                span.content
            );
            assert!(span
                .content
                .chars()
                .all(|c| ('\u{2800}'..='\u{28ff}').contains(&c)));
        }
    }

    #[test]
    fn the_projection_is_dotted_and_there_is_none_without_a_projected_end() {
        let b = projecting(40.0, 100.0);
        let path = projection_path(&b, &fine(&b));
        assert_eq!(dotted(&path).count(), path.len().div_ceil(PROJECTION_STEP));
        let mut none = projecting(40.0, 100.0);
        none.projected_end = None;
        assert!(projection_path(&none, &fine(&none)).is_empty());
    }

    #[test]
    fn no_reference_dot_shares_a_cell_with_a_projection_dot() {
        // Arrange
        let pal = Theme::Standard.palette(ColorDepth::True);
        let b = projecting(47.0, 170.0);
        let plot = Plot::new(&b, 40, 14);
        let f = Plot {
            width: plot.width * 2,
            height: plot.height * 4,
            top: plot.top,
        };
        let mut fill = DotGrid::new(plot.width, plot.height);
        fill_used(&b, &f, &mut fill);
        let projected: std::collections::HashSet<(usize, usize)> = dotted(&projection_path(&b, &f))
            .map(|(x, y)| (x / 2, y / 4))
            .filter(|(col, row)| fill.cell(*col, *row).is_none())
            .collect();
        assert!(projected.len() > 10);

        // Act
        let rows = dot_rows(&b, &pal, &plot);

        // Assert
        let reference = [pal.error, pal.inactive].map(|ink| pal.color(ink));
        for (row, spans) in rows.iter().enumerate() {
            for (col, span) in spans.iter().enumerate() {
                // A whole-cell marker replaces the dots under it.
                let dotted_cell = span
                    .content
                    .chars()
                    .all(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
                if dotted_cell && projected.contains(&(col, row)) {
                    assert_eq!(
                        span.style.fg,
                        Some(pal.color(Dot::Projection.ink(&pal))),
                        "cell ({col}, {row}) {:?}",
                        span.content
                    );
                    assert!(!reference.contains(&span.style.fg.unwrap()));
                }
            }
        }
    }
}
