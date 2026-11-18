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
use super::dots::DotGrid;
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
        grid[plot.row(value)].fill(role);
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

/// What a dot draws. The order is the priority on a shared cell: a limit
/// outranks the projection, which outranks the used area.
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

/// The dot row of the projection in each dot column from `now` on, so the
/// dotted line starts at the last used value.
fn projection_edge(b: &Burnup, fine: &Plot) -> Vec<Option<usize>> {
    (0..fine.width)
        .map(|x| {
            let at = fine.time(b, x);
            b.projected_at(at)
                .filter(|_| at >= b.now)
                .map(|v| fine.row(v))
        })
        .collect()
}

/// The dots of a continuous line through one row per column. Two columns
/// that differ in height meet halfway, so a rise reads as one stroke and not
/// as two separate dots. A column with `None` breaks the line.
fn connect(edge: &[Option<usize>]) -> Vec<(usize, usize)> {
    // The rows each column covers, as `(low, high)`.
    let mut span: Vec<Option<(usize, usize)>> = edge.iter().map(|r| r.map(|y| (y, y))).collect();
    for x in 1..edge.len() {
        let (Some(from), Some(to)) = (edge[x - 1], edge[x]) else {
            continue;
        };
        // The left column runs to the middle, the right one from just past it.
        let mid = (from + to) / 2;
        let next = if to > from {
            mid + 1
        } else {
            mid.saturating_sub(1)
        };
        let widen = |s: &mut Option<(usize, usize)>, row: usize| {
            if let Some((lo, hi)) = s {
                *lo = (*lo).min(row);
                *hi = (*hi).max(row);
            }
        };
        widen(&mut span[x - 1], mid);
        widen(&mut span[x], if to == from { to } else { next });
    }
    span.into_iter()
        .enumerate()
        .filter_map(|(x, s)| s.map(|(lo, hi)| (lo..=hi).map(move |y| (x, y))))
        .flatten()
        .collect()
}

/// Fills each dot column up to `now` solid from the bottom to its value, each
/// dot row inked by its height against the limit. The top dot row of a column
/// is the value itself, so the edge of the fill is the used line.
fn fill_used(b: &Burnup, fine: &Plot, grid: &mut DotGrid<Dot>) {
    let last = fine.height - 1;
    for x in 0..fine.width {
        let at = fine.time(b, x);
        let Some(value) = value_at(b, at).filter(|_| at <= b.now) else {
            continue;
        };
        for y in fine.row(value)..=last {
            let percent = b.limit.map_or(0.0, |l| fine.value(y) / l * PERCENT);
            grid.set(x, y, Dot::used(percent));
        }
    }
}

/// The plot as spans of Braille cells, top row first. The used area is a solid
/// fill, grown from the bottom of each dot column to its value. The reference
/// lines and the projection are dots too, but only in cells the fill leaves
/// free, so they never cut it. Markers and the other point glyphs stay
/// whole-cell glyphs over the dots.
fn dot_rows(b: &Burnup, pal: &Palette, plot: &Plot) -> Vec<Vec<Span<'static>>> {
    let fine = Plot {
        width: plot.width * 2,
        height: plot.height * 4,
        top: plot.top,
    };
    let mut fill = DotGrid::new(plot.width, plot.height);
    fill_used(b, &fine, &mut fill);
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
    for (x, y) in connect(&projection_edge(b, &fine)) {
        if x % 3 != 2 {
            dots.set(x, y, Dot::Projection);
        }
    }
    (0..plot.height)
        .map(|row| {
            (0..plot.width)
                .map(|col| {
                    let drawn = fill.cell(col, row).or_else(|| dots.cell(col, row));
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
        assert!(Dot::used(255.0) < Dot::Projection);
        assert!(Dot::Projection < Dot::Limit);
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

    /// Whether each column's dots touch those of the next column.
    fn unbroken(dots: &[(usize, usize)]) -> bool {
        let range = |x: usize| {
            let ys: Vec<usize> = dots.iter().filter(|d| d.0 == x).map(|d| d.1).collect();
            ys.iter().min().copied().zip(ys.iter().max().copied())
        };
        let last = dots.iter().map(|d| d.0).max().unwrap_or(0);
        (0..last).all(|x| match (range(x), range(x + 1)) {
            (Some((lo, hi)), Some((lo2, hi2))) => lo2 <= hi + 1 && lo <= hi2 + 1,
            _ => false,
        })
    }

    #[test]
    fn no_samples_draw_no_line() {
        assert!(connect(&[]).is_empty());
        assert!(connect(&[None, None]).is_empty());
    }

    #[test]
    fn one_sample_draws_one_dot() {
        assert_eq!(connect(&[Some(3)]), vec![(0, 3)]);
    }

    #[test]
    fn two_samples_far_apart_in_height_join_without_a_gap() {
        // Arrange
        let edge = [Some(2), Some(14)];

        // Act
        let dots = connect(&edge);

        // Assert
        assert!(dots.contains(&(0, 2)) && dots.contains(&(1, 14)));
        assert!(unbroken(&dots));
        assert_eq!(dots.len(), 13);
    }

    #[test]
    fn a_fall_joins_as_well_as_a_rise() {
        let dots = connect(&[Some(14), Some(2)]);
        assert!(unbroken(&dots));
        assert_eq!(dots.len(), 13);
    }

    #[test]
    fn a_missing_column_breaks_the_line() {
        let dots = connect(&[Some(1), None, Some(9)]);
        assert_eq!(dots, vec![(0, 1), (2, 9)]);
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
            let top = f.row(value_at(&b, at).unwrap());
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

    #[test]
    fn the_projection_is_dotted_and_starts_at_the_last_used_value() {
        // Arrange
        let mut b = line(vec![(0, 0.0), (400, 40.0)], 400);
        b.total = 40.0;
        b.projected_end = Some(100.0);
        let f = fine(&b);

        // Act
        let projected = projection_edge(&b, &f);

        // Assert
        let first = projected.iter().position(Option::is_some).unwrap();
        assert!(f.time(&b, first) >= b.now);
        // The first column lies a fraction past `now`, so one dot row of rise at most.
        assert!(projected[first].unwrap().abs_diff(f.row(40.0)) <= 1);
        assert!(projected[f.width - 1].is_some());
    }
}
