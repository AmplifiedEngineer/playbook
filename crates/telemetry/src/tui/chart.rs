// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A bar chart with axes, for a spend or token series: eighth blocks for the
//! bars, a y axis with round values, an x axis with dates and weekdays, a
//! cursor that reads one bar out, and a line of totals.
//!
//! From the top, inside the box: a totals line, the cursor readout, the plot,
//! a row of weekday letters, a row of dates. Rows are dropped from the least
//! useful when the panel is short, and the y axis when it is narrow, so the
//! chart still draws at any size. `Plot` is the geometry and the data; drawing
//! uses it, so the cursor lands on the bar it points at.

use super::data::{DayPoint, Slots};
use super::fmt::{axis_money, axis_tokens, compact, fit, money, WEEKDAYS};
use super::scale::{Axis, ROW_EIGHTHS};
use super::theme::{Fill, Ink, Palette, SERIES};
use crate::aggregate::date_key;
use crate::query::weekday;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use std::collections::BTreeMap;

/// Vertical eighth blocks, one to eight eighths of a cell.
const EIGHTHS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// The shortest plot worth drawing, in rows.
const MIN_PLOT: u16 = 3;
/// A bar is never drawn wider than this, so a few days do not become slabs.
/// A week or more of bars always fills the plot instead.
const MAX_CELL: usize = 16;
/// The narrowest inner width that keeps a y axis.
const MIN_AXIS_WIDTH: u16 = 16;
/// Seconds in the hour one bar of an hour chart spans.
const HOUR_SECONDS: i64 = 3600;

/// What a chart plots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cost,
    Tokens,
}

impl Metric {
    /// A value as it reads in text: `$1.25` or `23.8K`.
    pub fn show(self, v: f64) -> String {
        match self {
            Metric::Cost => money(v),
            Metric::Tokens => compact(v.round() as u64),
        }
    }

    fn axis_label(self, v: f64) -> String {
        match self {
            Metric::Cost => axis_money(v),
            Metric::Tokens => axis_tokens(v),
        }
    }
}

/// One bar: a day, or several days added up when the range is wider than the
/// plot.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub first: i64,
    pub last: i64,
    pub cost: f64,
    pub tokens: u64,
    pub messages: usize,
    /// Cost per model, by model name.
    pub models: Vec<(String, f64)>,
}

impl Bar {
    pub fn value(&self, metric: Metric) -> f64 {
        match metric {
            Metric::Cost => self.cost,
            Metric::Tokens => self.tokens as f64,
        }
    }

    pub fn days(&self) -> i64 {
        self.last - self.first + 1
    }

    pub fn contains(&self, day: i64) -> bool {
        (self.first..=self.last).contains(&day)
    }

    /// The model that cost the most, with its share of the bar's cost.
    pub fn top_model(&self) -> Option<(&str, f64)> {
        let (name, cost) = self
            .models
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))?;
        (self.cost > 0.0).then(|| (name.as_str(), cost / self.cost))
    }
}

/// The days of `series` in groups of `per`, each added up into a bar.
pub fn bars(series: &[DayPoint], per: usize) -> Vec<Bar> {
    series
        .chunks(per.max(1))
        .map(|days| {
            let mut models: BTreeMap<&str, f64> = BTreeMap::new();
            for d in days {
                for (name, cost) in &d.models {
                    *models.entry(name.as_str()).or_default() += cost;
                }
            }
            Bar {
                first: days[0].day,
                last: days[days.len() - 1].day,
                cost: days.iter().map(|d| d.cost_usd).sum(),
                tokens: days.iter().map(|d| d.tokens).sum(),
                messages: days.iter().map(|d| d.messages).sum(),
                models: models
                    .into_iter()
                    .map(|(name, cost)| (name.to_string(), cost))
                    .collect(),
            }
        })
        .collect()
}

/// Which rows the chart shows, for an inner height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rows {
    pub stats: bool,
    pub readout: bool,
    pub weekdays: bool,
    pub dates: bool,
    pub plot: u16,
}

impl Rows {
    /// Keeps as many rows as fit while the plot stays at least `MIN_PLOT`
    /// tall. The weekday row needs room for a plot of five. Rows go in this
    /// order: weekdays, totals, readout, dates.
    pub fn for_height(height: u16, weekdays: bool) -> Rows {
        let (mut stats, mut readout, mut dates, mut weekdays) = (true, true, true, weekdays);
        let used = |s: bool, r: bool, d: bool, w: bool| {
            u16::from(s) + u16::from(r) + u16::from(d) + u16::from(w)
        };
        if weekdays && height < used(stats, readout, dates, true) + 5 {
            weekdays = false;
        }
        if height < used(stats, readout, dates, weekdays) + MIN_PLOT {
            stats = false;
        }
        if height < used(stats, readout, dates, weekdays) + MIN_PLOT {
            readout = false;
        }
        if height < used(stats, readout, dates, weekdays) + MIN_PLOT {
            dates = false;
        }
        Rows {
            stats,
            readout,
            weekdays,
            dates,
            plot: height.saturating_sub(used(stats, readout, dates, weekdays)),
        }
    }
}

/// Where everything goes inside a chart panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub stats_y: Option<u16>,
    pub readout_y: Option<u16>,
    pub plot: Rect,
    pub weekdays_y: Option<u16>,
    pub dates_y: Option<u16>,
    /// Columns of the y axis: its labels and the axis line.
    pub gutter: u16,
    pub inner: Rect,
    /// Days added up into one bar.
    pub per: usize,
    /// For each bar, its first column in the plot and the columns it takes,
    /// with its gap.
    pub cells: Vec<(u16, u16)>,
    /// The columns every bar fills. The columns left over from the cells go
    /// into the gaps, so only the gaps differ.
    pub bar: u16,
}

impl Geometry {
    /// The layout for `days` days in a panel, with y labels `label_width`
    /// columns wide.
    pub fn new(panel: Rect, days: usize, label_width: u16) -> Geometry {
        Geometry::build(panel, days, label_width, true)
    }

    /// The layout for `days` bars. A chart of hours has no weekday row, so it
    /// passes `weekdays` false.
    fn build(panel: Rect, days: usize, label_width: u16, weekdays: bool) -> Geometry {
        let inner = Rect::new(
            panel.x + 1,
            panel.y + 1,
            panel.width.saturating_sub(2),
            panel.height.saturating_sub(2),
        );
        let wide = inner.width >= MIN_AXIS_WIDTH;
        // The labels, a blank column before them so they do not touch the box,
        // and the axis line.
        let gutter = if wide { label_width + 2 } else { 0 };
        let plot_width = usize::from(inner.width.saturating_sub(gutter));
        let (per, bars) = if days == 0 || plot_width == 0 {
            (1, 0)
        } else if days > plot_width {
            let per = days.div_ceil(plot_width);
            (per, days.div_ceil(per))
        } else {
            (1, days)
        };
        // A few bars get a fixed width. Many share the plot, a column or two
        // each, so the chart always fills its width.
        let cells: Vec<(u16, u16)> = if bars == 0 {
            Vec::new()
        } else if plot_width / bars >= MAX_CELL {
            (0..bars)
                .map(|i| ((i * MAX_CELL) as u16, MAX_CELL as u16))
                .collect()
        } else {
            (0..bars)
                .map(|i| {
                    let (from, to) = (i * plot_width / bars, (i + 1) * plot_width / bars);
                    (from as u16, (to - from) as u16)
                })
                .collect()
        };
        // Every bar is as wide as the narrowest cell less its gap of one;
        // crowded bars of one column keep it.
        let narrowest = cells.iter().map(|c| c.1).min().unwrap_or(1);
        let bar = narrowest.saturating_sub(1).max(1);
        // A weekday label needs its own column and a gap, so crowded bars
        // (a month in a narrow panel) go without the row.
        let roomy = cells.iter().all(|c| c.1 >= 2);
        let rows = Rows::for_height(inner.height, weekdays && per == 1 && bars > 0 && roomy);
        let mut y = inner.y;
        let mut take = |rows: u16| {
            y += rows;
            y - rows
        };
        let stats_y = rows.stats.then(|| take(1));
        let readout_y = rows.readout.then(|| take(1));
        let plot_y = take(rows.plot);
        let weekdays_y = rows.weekdays.then(|| take(1));
        let dates_y = rows.dates.then(|| take(1));
        Geometry {
            stats_y,
            readout_y,
            plot: Rect::new(inner.x + gutter, plot_y, plot_width as u16, rows.plot),
            weekdays_y,
            dates_y,
            gutter,
            inner,
            per,
            cells,
            bar,
        }
    }

    pub fn bars(&self) -> usize {
        self.cells.len()
    }

    /// The first column of bar `i` on screen.
    pub fn x_of(&self, i: usize) -> u16 {
        self.plot.x + self.cells[i].0
    }

    /// The columns the bar itself fills, the same for every bar.
    pub fn bar_width(&self) -> u16 {
        self.bar
    }

    /// The columns from the plot start to the end of the last bar.
    pub fn span(&self) -> usize {
        self.cells
            .len()
            .checked_sub(1)
            .map_or(0, |i| usize::from(self.cells[i].0 + self.bar_width()))
    }
}

/// A series laid out in a panel: geometry, bars and the scale.
#[derive(Debug, Clone)]
pub struct Plot {
    pub geo: Geometry,
    pub bars: Vec<Bar>,
    pub axis: Axis,
    pub metric: Metric,
    pub scale: Scale,
}

/// What one point of a series is: a day, or an hour whose `day` holds the
/// second it starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Days,
    Hours,
}

impl Plot {
    pub fn new(panel: Rect, series: &[DayPoint], metric: Metric) -> Plot {
        Plot::laid_out(panel, series, metric, Scale::Days)
    }

    /// The cost per hour of a series of hours.
    pub fn hourly(panel: Rect, series: &[DayPoint]) -> Plot {
        Plot::laid_out(panel, series, Metric::Cost, Scale::Hours)
    }

    fn laid_out(panel: Rect, series: &[DayPoint], metric: Metric, scale: Scale) -> Plot {
        // The label width depends on the scale, which depends on how many
        // days a bar holds, which depends on the label width. It settles in a
        // pass or two; a third pass is the last.
        let mut label_width = 2;
        let mut plot = None;
        for _ in 0..3 {
            let geo = Geometry::build(panel, series.len(), label_width, scale == Scale::Days);
            let bars = bars(series, geo.per);
            let peak = bars.iter().map(|b| b.value(metric)).fold(0.0, f64::max);
            let axis = Axis::of(peak);
            let width = axis
                .ticks()
                .iter()
                .map(|t| metric.axis_label(*t).chars().count() as u16)
                .max()
                .unwrap_or(2);
            let settled = width == label_width;
            label_width = width;
            plot = Some(Plot {
                geo,
                bars,
                axis,
                metric,
                scale,
            });
            if settled {
                break;
            }
        }
        plot.unwrap_or_else(|| unreachable_plot(panel, metric, scale))
    }

    /// The index of the bar holding `day`.
    pub fn index_of(&self, day: i64) -> Option<usize> {
        self.bars.iter().position(|b| b.contains(day))
    }

    /// The bar the readout shows before the cursor is moved: the one holding
    /// today, else the newest.
    pub fn default_index(&self, today: i64) -> Option<usize> {
        self.index_of(today)
            .or_else(|| self.bars.len().checked_sub(1))
    }

    /// The index of the tallest bar, if any bar has a value.
    pub fn peak_index(&self) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, b) in self.bars.iter().enumerate() {
            let v = b.value(self.metric);
            if v > 0.0 && best.is_none_or(|(_, top)| v > top) {
                best = Some((i, v));
            }
        }
        best.map(|(i, _)| i)
    }
}

/// `Plot::new` always runs its loop at least once; this is the same plot for
/// an empty series, so no code path needs an `unwrap`.
fn unreachable_plot(panel: Rect, metric: Metric, scale: Scale) -> Plot {
    Plot {
        geo: Geometry::new(panel, 0, 2),
        bars: Vec::new(),
        axis: Axis::of(0.0),
        metric,
        scale,
    }
}

/// What the chart shows besides the bars.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub fill: Fill,
    /// Stack each bar by model, in the colors of these slots, instead of one
    /// color along the fill.
    pub stack: Option<&'a Slots>,
    /// The bar the cursor is on, if it was moved or hovered.
    pub cursor: Option<usize>,
    /// The day number of today, to mark its bar.
    pub today: i64,
}

fn dim(pal: &Palette) -> Style {
    Style::default().fg(pal.color(pal.graph_text))
}

fn text(pal: &Palette) -> Style {
    Style::default().fg(pal.color(pal.fg))
}

/// The weekday label style: the cursor, today and the peak stand out, with
/// modifiers as well as colors so the mono theme shows them too.
fn mark_style(pal: &Palette, cursor: bool, today: bool, peak: bool) -> Style {
    if cursor {
        return if pal.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
                .fg(pal.color(pal.sel_fg))
                .bg(pal.color(pal.sel_bg))
                .add_modifier(Modifier::BOLD)
        };
    }
    if today {
        return Style::default()
            .fg(pal.color(pal.hi))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    }
    if peak {
        return Style::default()
            .fg(pal.color(pal.warn))
            .add_modifier(Modifier::BOLD);
    }
    dim(pal)
}

/// `Thu 2026-09-03`.
pub fn day_label(day: i64) -> String {
    format!("{} {}", WEEKDAYS[weekday(day)], date_key(day))
}

/// `09-03`, the date without its year.
fn short_date(day: i64) -> String {
    date_key(day)[5..].to_string()
}

/// The readout line for a bar: its date and tags, then its numbers, each part
/// added only while it fits.
pub fn readout(plot: &Plot, bar: &Bar, today: bool, peak: bool, width: usize) -> String {
    let mut head = if bar.days() == 1 {
        day_label(bar.first)
    } else {
        format!(
            "{} to {} ({} days)",
            short_date(bar.first),
            short_date(bar.last),
            bar.days()
        )
    };
    if today {
        head.push_str(" today");
    }
    if peak {
        head.push_str(" peak");
    }
    let cost = money(bar.cost);
    let tokens = format!("{} tok", compact(bar.tokens));
    let msgs = format!("{} msgs", bar.messages);
    let mut parts = vec![head];
    match plot.metric {
        Metric::Cost => parts.extend([cost, msgs, tokens]),
        Metric::Tokens => parts.extend([tokens, cost, msgs]),
    }
    if let (Metric::Cost, Some((model, share))) = (plot.metric, bar.top_model()) {
        let name = model.strip_prefix("claude-").unwrap_or(model);
        parts.push(format!("{name} {:.0}%", share * 100.0));
    }
    let mut out = String::new();
    for part in parts {
        let next = if out.is_empty() {
            part
        } else {
            format!("{out}  {part}")
        };
        if next.chars().count() > width {
            if out.is_empty() {
                return fit(&next, width);
            }
            break;
        }
        out = next;
    }
    out
}

/// The totals line, the longest form that fits.
pub fn stats(plot: &Plot, series: &[DayPoint], today: i64, width: usize) -> String {
    let m = plot.metric;
    let value = |d: &DayPoint| match m {
        Metric::Cost => d.cost_usd,
        Metric::Tokens => d.tokens as f64,
    };
    let total: f64 = series.iter().map(value).sum();
    let peak = series.iter().map(value).fold(0.0, f64::max);
    let peak_day = series.iter().find(|d| value(d) == peak && peak > 0.0);
    // The average is per day that has happened, so a week still in progress
    // is not pulled down by the days to come.
    let elapsed = series.iter().filter(|d| d.day <= today).count();
    let avg = if elapsed == 0 {
        0.0
    } else {
        total / elapsed as f64
    };
    let on = peak_day.map_or(String::new(), |d| format!(" on {}", short_date(d.day)));
    let forms = [
        format!(
            "total {}  peak {}{on}  avg {}/day",
            m.show(total),
            m.show(peak),
            m.show(avg)
        ),
        format!(
            "total {}  peak {}  avg {}/day",
            m.show(total),
            m.show(peak),
            m.show(avg)
        ),
        format!(
            "total {}  peak {}  avg {}",
            m.show(total),
            m.show(peak),
            m.show(avg)
        ),
        format!("total {}  peak {}", m.show(total), m.show(peak)),
        format!("total {}", m.show(total)),
    ];
    for form in &forms {
        if form.chars().count() <= width {
            return form.clone();
        }
    }
    fit(&forms[4], width)
}

/// The three date labels under the plot: first, middle and last, as many as
/// fit without touching. Years are shown when there is room for them.
fn date_labels(plot: &Plot, width: usize) -> Vec<(usize, String)> {
    let bars = &plot.bars;
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Vec::new();
    };
    let span = plot.geo.span();
    let long = |d: i64| date_key(d);
    let short = |d: i64| short_date(d);
    let pick = |fmt: &dyn Fn(i64) -> String| {
        let mut out = vec![(0, fmt(first.first))];
        if bars.len() > 1 {
            out.push((
                span.saturating_sub(fmt(last.last).chars().count()),
                fmt(last.last),
            ));
        }
        if bars.len() > 2 {
            let mid = &bars[bars.len() / 2];
            let label = fmt(mid.first);
            let at = usize::from(plot.geo.cells[bars.len() / 2].0);
            let at = at.min(span.saturating_sub(label.chars().count()));
            out.push((at, label));
        }
        out.sort_by_key(|(at, _)| *at);
        out
    };
    let fits = |labels: &[(usize, String)]| {
        labels
            .windows(2)
            .all(|w| w[0].0 + w[0].1.chars().count() < w[1].0)
            && labels
                .last()
                .is_some_and(|(at, l)| at + l.chars().count() <= width)
    };
    let drop_middle = |mut labels: Vec<(usize, String)>| {
        if labels.len() == 3 {
            labels.remove(1);
        }
        labels
    };
    for labels in [pick(&long), pick(&short), drop_middle(pick(&short))] {
        if fits(&labels) {
            return labels;
        }
    }
    vec![(0, short(first.first))]
}

/// The day of the month, `1` to `31`.
fn day_number(day: i64) -> String {
    date_key(day)[8..].trim_start_matches('0').to_string()
}

/// The hour of the clock a second falls in, `0` to `23`.
fn hour_number(at: i64) -> String {
    crate::tz::clock(at)[..2]
        .trim_start_matches('0')
        .to_string()
}

/// `14:00`, the whole hour a second starts.
fn hour_label(at: i64) -> String {
    format!("{}:00", &crate::tz::clock(at)[..2])
}

/// The label under bar `i`: its day of the month, or its hour of the clock.
fn tick_label(plot: &Plot, i: usize) -> String {
    let first = plot.bars[i].first;
    match plot.scale {
        Scale::Days => day_number(first),
        Scale::Hours => match hour_number(first).as_str() {
            "" => "0".to_string(),
            h => h.to_string(),
        },
    }
}

/// The readout line for an hour bar: the hour, then its numbers, each added
/// only while it fits.
fn hour_readout(bar: &Bar, now_hour: bool, peak: bool, width: usize) -> String {
    let mut head = format!(
        "{} to {}",
        hour_label(bar.first),
        hour_label(bar.last + HOUR_SECONDS)
    );
    if now_hour {
        head.push_str(" now");
    }
    if peak {
        head.push_str(" peak");
    }
    let mut parts = vec![
        head,
        money(bar.cost),
        format!("{} msgs", bar.messages),
        format!("{} tok", compact(bar.tokens)),
    ];
    if let Some((model, share)) = bar.top_model() {
        let name = model.strip_prefix("claude-").unwrap_or(model);
        parts.push(format!("{name} {:.0}%", share * 100.0));
    }
    let mut out = String::new();
    for part in parts {
        let next = if out.is_empty() {
            part
        } else {
            format!("{out}  {part}")
        };
        if next.chars().count() > width {
            if out.is_empty() {
                return fit(&next, width);
            }
            break;
        }
        out = next;
    }
    out
}

/// The totals line of an hour chart, the longest form that fits.
fn hour_stats(series: &[DayPoint], width: usize) -> String {
    let total: f64 = series.iter().map(|d| d.cost_usd).sum();
    let peak = series.iter().map(|d| d.cost_usd).fold(0.0, f64::max);
    let at = series
        .iter()
        .find(|d| d.cost_usd == peak && peak > 0.0)
        .map_or(String::new(), |d| format!(" at {}", hour_label(d.day)));
    let forms = [
        format!("total {}  peak {}{at}", money(total), money(peak)),
        format!("total {}  peak {}", money(total), money(peak)),
        format!("total {}", money(total)),
    ];
    for form in &forms {
        if form.chars().count() <= width {
            return form.clone();
        }
    }
    fit(&forms[2], width)
}

/// Whether bar `i` carries a day number when every `step` bars do. The first
/// and the last bar always do.
fn shows_number(i: usize, last: usize, step: usize) -> bool {
    i.is_multiple_of(step) || i == last
}

/// Whether the number of bar `i` leaves a column free before the next number
/// that is shown, so two numbers never touch.
fn number_fits(plot: &Plot, i: usize, last: usize, step: usize) -> bool {
    let end = usize::from(plot.geo.cells[i].0) + tick_label(plot, i).len();
    match (i + 1..=last).find(|j| shows_number(*j, last, step)) {
        Some(j) => end < usize::from(plot.geo.cells[j].0) || i == last,
        None => true,
    }
}

/// How many bars apart the day numbers are: every day when each cell holds a
/// number and a gap, else every second, third or seventh day, the smallest
/// step that keeps the regular numbers clear of each other. The first and the
/// last bar always show one, and when the last is too close to the one before
/// it the one before is left out.
fn number_step(plot: &Plot) -> usize {
    let last = plot.bars.len().saturating_sub(1);
    for step in [1, 2, 3, 7] {
        let shown: Vec<usize> = (0..=last).filter(|i| i.is_multiple_of(step)).collect();
        let clear = shown.windows(2).all(|w| {
            let end = usize::from(plot.geo.cells[w[0]].0) + tick_label(plot, w[0]).len();
            end < usize::from(plot.geo.cells[w[1]].0)
        });
        if clear {
            return step;
        }
    }
    usize::MAX
}

/// The weekday label: three letters, two or one, so the labels of one chart
/// all look alike.
fn weekday_label(letters: usize, day: i64) -> String {
    WEEKDAYS[weekday(day)][..letters].to_string()
}

/// How many letters of a weekday fit every cell of the chart, leaving a
/// column between labels.
fn weekday_letters(geo: &Geometry) -> usize {
    let narrowest = geo.cells.iter().map(|c| c.1).min().unwrap_or(1);
    usize::from(narrowest.saturating_sub(1)).clamp(1, 3)
}

/// Draws the chart inside `plot.geo.inner`. The caller draws the box.
pub fn render(buf: &mut Buffer, plot: &Plot, series: &[DayPoint], view: &View, pal: &Palette) {
    let geo = &plot.geo;
    if geo.inner.width == 0 || geo.inner.height == 0 {
        return;
    }
    let width = usize::from(geo.inner.width);
    let view = &View {
        cursor: view.cursor.filter(|i| *i < plot.bars.len()),
        ..*view
    };
    let peak = plot.peak_index();
    if let Some(y) = geo.stats_y {
        let line = match plot.scale {
            Scale::Days => stats(plot, series, view.today, width),
            Scale::Hours => hour_stats(series, width),
        };
        buf.set_stringn(geo.inner.x, y, line, width, text(pal));
    }
    if let Some(y) = geo.readout_y {
        // With no cursor yet the readout shows the newest bar.
        let shown = view.cursor.or_else(|| plot.default_index(view.today));
        if let Some(i) = shown {
            let bar = &plot.bars[i];
            let line = match plot.scale {
                Scale::Days => readout(plot, bar, bar.contains(view.today), peak == Some(i), width),
                Scale::Hours => hour_readout(bar, i + 1 == plot.bars.len(), peak == Some(i), width),
            };
            let style = if view.cursor.is_some() {
                text(pal).add_modifier(Modifier::BOLD)
            } else {
                text(pal)
            };
            buf.set_stringn(geo.inner.x, y, line, width, style);
        }
    }
    draw_axis(buf, plot, pal);
    match view.stack {
        Some(slots) if plot.metric == Metric::Cost => draw_stacked(buf, plot, slots, pal),
        _ => draw_bars(buf, plot, view, pal),
    }
    if let Some(y) = geo.weekdays_y {
        let letters = weekday_letters(geo);
        for (i, bar) in plot.bars.iter().enumerate() {
            let label = weekday_label(letters, bar.first);
            let style = mark_style(
                pal,
                view.cursor == Some(i),
                bar.contains(view.today),
                peak == Some(i),
            );
            let x = geo.x_of(i);
            buf.set_stringn(x, y, label, usize::from(geo.cells[i].1), style);
        }
    }
    if let (Some(y), 1) = (geo.dates_y, geo.per) {
        let step = number_step(plot);
        let last = plot.bars.len().saturating_sub(1);
        for (i, bar) in plot.bars.iter().enumerate() {
            if !shows_number(i, last, step) || !number_fits(plot, i, last, step) {
                continue;
            }
            let style = mark_style(
                pal,
                view.cursor == Some(i),
                bar.contains(view.today),
                peak == Some(i),
            );
            let x = geo.x_of(i);
            let room = usize::from(geo.plot.right().saturating_sub(x));
            buf.set_stringn(x, y, tick_label(plot, i), room, style);
        }
    } else if let (Some(y), Scale::Days) = (geo.dates_y, plot.scale) {
        for (at, label) in date_labels(plot, usize::from(geo.plot.width)) {
            buf.set_stringn(
                geo.plot.x + at as u16,
                y,
                &label,
                usize::from(geo.plot.width).saturating_sub(at),
                dim(pal),
            );
        }
    }
}

/// The y axis: labels at the bottom, middle and top, an axis line, and faint
/// guide lines across the plot at the top and the middle.
fn draw_axis(buf: &mut Buffer, plot: &Plot, pal: &Palette) {
    let geo = &plot.geo;
    let p = geo.plot;
    if p.height == 0 || p.width == 0 {
        return;
    }
    let guide = Style::default().fg(pal.color(pal.div_line));
    // Each label sits on the row where a bar of its value ends, so a bar
    // that equals a label reaches that label.
    let rows: Vec<(u16, f64)> = match p.height {
        1 => vec![(0, plot.axis.max)],
        2 => vec![(0, plot.axis.max), (1, 0.0)],
        _ => plot
            .axis
            .ticks()
            .into_iter()
            .rev()
            .map(|v| (plot.axis.row_of(v, p.height), v))
            .collect(),
    };
    if p.height >= MIN_PLOT {
        for (row, _) in rows.iter().take(2) {
            for x in p.x..p.x + p.width {
                buf[(x, p.y + row)].set_char('\u{2504}').set_style(guide);
            }
        }
    }
    if geo.gutter == 0 {
        return;
    }
    let axis_x = p.x - 1;
    for row in 0..p.height {
        buf[(axis_x, p.y + row)]
            .set_char('\u{2502}')
            .set_style(dim(pal));
    }
    for (row, value) in rows {
        buf[(axis_x, p.y + row)]
            .set_char('\u{2524}')
            .set_style(dim(pal));
        let label = fit(&plot.metric.axis_label(value), usize::from(geo.gutter - 2));
        let width = label.chars().count() as u16;
        buf.set_string(axis_x - width, p.y + row, label, dim(pal));
    }
}

/// The bars, colored along the fill by their height on the axis, with the
/// cursor bar in the bright title color.
fn draw_bars(buf: &mut Buffer, plot: &Plot, view: &View, pal: &Palette) {
    let geo = &plot.geo;
    let p = geo.plot;
    for (i, bar) in plot.bars.iter().enumerate() {
        let value = bar.value(plot.metric);
        let levels = plot.axis.levels(value, p.height);
        let pct = value / plot.axis.max * 100.0;
        let color = if view.cursor == Some(i) && !pal.mono {
            pal.color(pal.title)
        } else {
            pal.graded(view.fill, pct)
        };
        let x0 = geo.x_of(i);
        for col in 0..geo.bar_width() {
            let x = x0 + col;
            if x >= p.x + p.width {
                break;
            }
            for r in 0..p.height {
                let eighths = levels
                    .saturating_sub(usize::from(r) * ROW_EIGHTHS)
                    .min(ROW_EIGHTHS);
                if eighths == 0 {
                    break;
                }
                let y = p.y + p.height - 1 - r;
                buf[(x, y)]
                    .set_char(EIGHTHS[eighths - 1])
                    .set_style(Style::default().fg(color));
            }
        }
    }
}

/// The glyphs that tell stacked models apart without color, by slot.
pub const PATTERNS: [char; 8] = [
    '\u{2588}', '\u{2593}', '\u{2592}', '\u{2591}', '\u{259a}', '\u{259e}', '\u{259b}', '\u{259f}',
];

/// A bar's models as runs of eighths, bottom up: the slot and where the run
/// ends. The biggest model comes first and the last run ends at `levels`.
fn runs(bar: &Bar, slots: &Slots, levels: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut sum = 0.0;
    let mut prev = 0;
    for name in &slots.order {
        let cost = bar
            .models
            .iter()
            .find(|(m, _)| m == name)
            .map_or(0.0, |(_, c)| *c);
        if cost <= 0.0 {
            continue;
        }
        sum += cost;
        let end = ((levels as f64 * sum / bar.cost).round() as usize).min(levels);
        if end > prev {
            out.push((slots.slot(name), end));
            prev = end;
        }
    }
    // Rounding can stop short of the top: the last model takes the rest.
    match out.last_mut() {
        Some(last) => last.1 = levels,
        None if levels > 0 => out.push((0, levels)),
        None => {}
    }
    out
}

/// Bars stacked by model: each run of eighths in its model's color. A cell
/// that holds the edge between two models is one block in the lower color on
/// the upper one, so the edge is exact to an eighth of a cell. Without color
/// the models are told apart by pattern.
fn draw_stacked(buf: &mut Buffer, plot: &Plot, slots: &Slots, pal: &Palette) {
    let geo = &plot.geo;
    let p = geo.plot;
    for (i, bar) in plot.bars.iter().enumerate() {
        let levels = plot.axis.levels(bar.cost, p.height);
        let runs = runs(bar, slots, levels);
        let start_of = |k: usize| if k == 0 { 0 } else { runs[k - 1].1 };
        for r in 0..p.height {
            let lo = usize::from(r) * 8;
            if lo >= levels {
                break;
            }
            let hi = (lo + 8).min(levels);
            // How much of this cell each run fills.
            let overlap = |k: usize| hi.min(runs[k].1).saturating_sub(lo.max(start_of(k)));
            let inside: Vec<usize> = (0..runs.len()).filter(|k| overlap(*k) > 0).collect();
            let main = inside
                .iter()
                .copied()
                .max_by_key(|k| (overlap(*k), std::cmp::Reverse(*k)))
                .unwrap_or(0);
            let eighths = hi - lo;
            let (glyph, style) = match (pal.mono, inside.as_slice()) {
                (true, _) if eighths == 8 => {
                    (PATTERNS[runs[main].0 % PATTERNS.len()], Style::default())
                }
                (true, _) => (EIGHTHS[eighths - 1], Style::default()),
                // An edge inside a full cell: lower run as the block, upper as the ground.
                (false, [low, high]) if eighths == 8 => {
                    let edge = runs[*low].1 - lo;
                    let style = Style::default()
                        .fg(pal.color(pal.series[runs[*low].0 % SERIES]))
                        .bg(pal.color(pal.series[runs[*high].0 % SERIES]));
                    (EIGHTHS[edge - 1], style)
                }
                (false, _) => (
                    EIGHTHS[eighths - 1],
                    Style::default().fg(pal.color(pal.series[runs[main].0 % SERIES])),
                ),
            };
            let y = p.y + p.height - 1 - r;
            for col in 0..geo.bar_width() {
                let x = geo.x_of(i) + col;
                if x < p.x + p.width {
                    buf[(x, y)].set_char(glyph).set_style(style);
                }
            }
        }
    }
}

/// The ink of a model's series color, for the legend and the Models table.
pub fn series_ink(pal: &Palette, slot: usize) -> Ink {
    pal.series[slot % SERIES]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::scale::HALF_ROW;
    use crate::tui::theme::{ColorDepth, Theme};

    fn pal() -> Palette {
        Theme::Standard.palette(ColorDepth::True)
    }

    /// 2026-08-05, a Wednesday.
    const FIRST: i64 = 20_670;

    fn series(costs: &[f64]) -> Vec<DayPoint> {
        costs
            .iter()
            .enumerate()
            .map(|(i, c)| DayPoint {
                day: FIRST + i as i64,
                cost_usd: *c,
                tokens: (*c * 1000.0) as u64,
                messages: usize::from(*c > 0.0),
                models: if *c > 0.0 {
                    vec![("claude-opus-5".to_string(), *c)]
                } else {
                    Vec::new()
                },
            })
            .collect()
    }

    fn draw(panel: Rect, series: &[DayPoint], cursor: Option<usize>) -> (Plot, Buffer) {
        draw_with(panel, series, cursor, None, &pal())
    }

    fn draw_with(
        panel: Rect,
        series: &[DayPoint],
        cursor: Option<usize>,
        stack: Option<&Slots>,
        pal: &Palette,
    ) -> (Plot, Buffer) {
        let plot = Plot::new(panel, series, Metric::Cost);
        let mut buf = Buffer::empty(panel);
        let view = View {
            fill: pal.cpu,
            stack,
            cursor,
            today: series.last().map_or(0, |d| d.day),
        };
        render(&mut buf, &plot, series, &view, pal);
        (plot, buf)
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (buf.area.x..buf.area.x + buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    fn screen(buf: &Buffer) -> String {
        (buf.area.y..buf.area.y + buf.area.height)
            .map(|y| row(buf, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rows_keep_the_plot_at_least_three_tall_and_drop_rows_in_order() {
        let r = |h, w| Rows::for_height(h, w);
        assert_eq!(
            r(9, true),
            Rows {
                stats: true,
                readout: true,
                weekdays: true,
                dates: true,
                plot: 5
            }
        );
        assert_eq!(
            r(8, true),
            Rows {
                stats: true,
                readout: true,
                weekdays: false,
                dates: true,
                plot: 5
            }
        );
        assert_eq!(r(7, true).plot, 4);
        assert_eq!(
            r(6, false),
            Rows {
                stats: true,
                readout: true,
                weekdays: false,
                dates: true,
                plot: 3
            }
        );
        assert_eq!(
            r(5, false),
            Rows {
                stats: false,
                readout: true,
                weekdays: false,
                dates: true,
                plot: 3
            }
        );
        assert_eq!(
            r(4, false),
            Rows {
                stats: false,
                readout: false,
                weekdays: false,
                dates: true,
                plot: 3
            }
        );
        assert_eq!(
            r(3, false),
            Rows {
                stats: false,
                readout: false,
                weekdays: false,
                dates: false,
                plot: 3
            }
        );
        assert_eq!(r(1, false).plot, 1);
        assert_eq!(r(0, true).plot, 0);
    }

    #[test]
    fn every_bar_has_the_same_width_whatever_the_panel_width() {
        for panel_width in 30..140 {
            for days in [7, 14, 30] {
                let g = Geometry::new(Rect::new(0, 0, panel_width, 10), days, 4);
                let plot_width = g.plot.width;
                let first = g.bar_width();
                let mut end = 0;
                for i in 0..g.bars() {
                    assert_eq!(g.bar_width(), first, "{panel_width} wide, {days} days");
                    let (from, cell) = g.cells[i];
                    assert!(from >= end, "cells overlap at {panel_width}/{days}");
                    assert!(from + g.bar_width() <= from + cell);
                    end = from + cell;
                }
                assert!(
                    g.span() <= usize::from(plot_width),
                    "{panel_width} wide, {days} days"
                );
                // Gaps differ by at most one column.
                let gaps: Vec<u16> = g.cells.iter().map(|c| c.1 - g.bar_width()).collect();
                let (lo, hi) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
                assert!(hi - lo <= 1, "{panel_width} wide, {days} days: {gaps:?}");
            }
        }
    }

    #[test]
    fn a_month_in_a_panel_of_three_point_something_columns_per_day_keeps_equal_bars() {
        // 38 inner columns less a 6 column gutter is 32 columns for 30 days.
        // Make it 3.x columns per day: 100 columns of plot for 30 days.
        let g = Geometry::new(Rect::new(0, 0, 108, 10), 30, 4);
        assert_eq!(g.plot.width, 100);
        assert!(g.cells.iter().any(|c| c.1 == 3) && g.cells.iter().any(|c| c.1 == 4));
        assert_eq!(g.bar_width(), 2);
    }

    #[test]
    fn a_bar_equal_to_an_axis_label_ends_on_that_labels_row() {
        for height in [3u16, 4, 5, 6, 7, 8, 9, 12] {
            let panel = Rect::new(0, 0, 40, height + 6);
            // Peak 400 gives the axis 0, 200, 400.
            for (value, label) in [(400.0, "$400"), (200.0, "$200")] {
                let series = series(&[value, 400.0]);
                let (plot, buf) = draw(panel, &series, None);
                let p = plot.geo.plot;
                let label_row = (p.y..p.y + p.height)
                    .find(|y| row(&buf, *y).contains(label))
                    .unwrap_or_else(|| panic!("no {label} at plot height {}", p.height));
                let x = plot.geo.x_of(0);
                let top = (p.y..p.y + p.height)
                    .find(|y| buf[(x, *y)].symbol() != " " && buf[(x, *y)].symbol() != "\u{2504}")
                    .expect("a bar");
                assert_eq!(top, label_row, "{label} at plot height {}", p.height);
                if value == 400.0 {
                    // The top label sits in the middle of its row, so the
                    // bar that equals it ends in the middle of that row.
                    assert_eq!(
                        buf[(x, top)].symbol(),
                        EIGHTHS[HALF_ROW - 1].to_string(),
                        "plot height {}",
                        p.height
                    );
                }
            }
        }
    }

    #[test]
    fn bars_share_the_plot_so_the_chart_fills_its_width() {
        // 38 inner columns less a 6 column axis gutter leaves 32.
        let g = |days| Geometry::new(Rect::new(0, 0, 40, 10), days, 4);
        let widths = |g: &Geometry| g.cells.iter().map(|c| c.1).collect::<Vec<_>>();
        let thirty = g(30);
        assert_eq!((thirty.per, thirty.bars()), (1, 30));
        assert_eq!(widths(&thirty).iter().sum::<u16>(), 32);
        assert!(widths(&thirty).iter().all(|w| (1..=2).contains(w)));
        let seven = g(7);
        assert_eq!(widths(&seven).iter().sum::<u16>(), 32);
        assert!(widths(&seven).iter().all(|w| (4..=5).contains(w)));
        assert_eq!(seven.bar_width(), seven.cells[0].1 - 1, "a gap of one");
        assert_eq!(thirty.bar_width(), 1, "no gap at one column");
        // A few days get a fixed width and do not stretch into slabs.
        let one = g(1);
        assert_eq!(one.cells, vec![(0, MAX_CELL as u16)]);
        assert_eq!(one.bar_width(), MAX_CELL as u16 - 1);
        // Bars never overlap and never leave the plot.
        for days in 1..200 {
            let g = g(days);
            let mut end = 0;
            for (from, w) in &g.cells {
                assert!(*from >= end && from + w <= 32, "{days}");
                end = from + w;
            }
        }
        // More days than columns are added up, so nothing is cut off.
        let ninety = g(90);
        assert_eq!((ninety.per, ninety.bars()), (3, 30));
        let year = g(400);
        assert_eq!((year.per, year.bars()), (13, 31));
        assert_eq!(g(0).bars(), 0);
    }

    #[test]
    fn buckets_add_up_days_cost_tokens_messages_and_models() {
        let mut s = series(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        s[1].models.push(("claude-haiku-5".to_string(), 0.5));
        let b = bars(&s, 2);
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].first, b[0].last, b[0].cost), (FIRST, FIRST + 1, 3.0));
        assert_eq!(b[0].messages, 2);
        assert_eq!(b[0].tokens, 3_000);
        assert_eq!(b[0].models.len(), 2);
        assert_eq!(
            (b[2].first, b[2].last, b[2].days()),
            (FIRST + 4, FIRST + 4, 1)
        );
        assert_eq!(b[0].top_model().map(|(n, _)| n), Some("claude-opus-5"));
        let total: f64 = b.iter().map(|x| x.cost).sum();
        assert!((total - 15.0).abs() < 1e-9);
    }

    #[test]
    fn the_y_axis_labels_the_bottom_middle_and_top_with_round_values() {
        let s = series(&[1.0, 4.1, 0.5]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 12), &s, None);
        let p = plot.geo.plot;
        assert_eq!(plot.axis.max, 5.0);
        let label = |y: u16| {
            row(&buf, y)
                .chars()
                .skip(1)
                .take(usize::from(p.x - 2))
                .collect::<String>()
        };
        assert_eq!(label(p.y).trim(), "$5");
        assert_eq!(label(p.y + p.height / 2).trim(), "$2.5");
        assert_eq!(label(p.y + p.height - 1).trim(), "$0");
        // The axis line is a tick at each label and a bar between them.
        let axis_x = p.x - 1;
        assert_eq!(buf[(axis_x, p.y)].symbol(), "\u{2524}");
        assert_eq!(buf[(axis_x, p.y + 1)].symbol(), "\u{2502}");
    }

    #[test]
    fn the_peak_bar_reaches_the_scale_proportionally() {
        // The top of the axis is $5, so $2.5 fills half of a four row plot.
        let s = series(&[2.5, 5.0]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 10), &s, None);
        let p = plot.geo.plot;
        let h = p.height;
        let col = |i: usize| plot.geo.x_of(i);
        let filled = |x: u16| {
            (0..h)
                .filter(|r| buf[(x, p.y + h - 1 - r)].symbol() == "\u{2588}")
                .count()
        };
        // The top is the middle of the top row, where the $5 label is: the
        // peak bar has a half block there, and $2.5 ends at half of that scale.
        let scale = usize::from(h) * ROW_EIGHTHS - HALF_ROW;
        assert_eq!(filled(col(1)), usize::from(h) - 1);
        assert_eq!(
            buf[(col(1), p.y)].symbol(),
            EIGHTHS[HALF_ROW - 1].to_string()
        );
        assert_eq!(filled(col(0)), scale / 2 / ROW_EIGHTHS);
    }

    #[test]
    fn a_small_day_still_shows_a_sliver() {
        let s = series(&[0.001, 100.0]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 10), &s, None);
        let p = plot.geo.plot;
        assert_eq!(buf[(p.x, p.y + p.height - 1)].symbol(), "\u{2581}");
    }

    /// The numbers on the row under the weekdays, as (column, text) pairs.
    fn numbers(buf: &Buffer, plot: &Plot) -> Vec<(usize, String)> {
        let line = row(buf, plot.geo.dates_y.unwrap());
        let mut out: Vec<(usize, String)> = Vec::new();
        for (at, ch) in line.chars().enumerate() {
            if ch.is_ascii_digit() {
                match out.last_mut() {
                    Some((from, text)) if *from + text.len() == at => text.push(ch),
                    _ => out.push((at, ch.to_string())),
                }
            }
        }
        out
    }

    #[test]
    fn a_week_shows_the_weekday_and_the_day_number_under_it_for_every_bar() {
        // 2026-08-05 is a Wednesday, so the week runs Wed 5 to Tue 11.
        let s = series(&[1.0; 7]);
        let (plot, buf) = draw(Rect::new(0, 0, 60, 14), &s, None);
        let (w, d) = (plot.geo.weekdays_y.unwrap(), plot.geo.dates_y.unwrap());
        assert_eq!(d, w + 1, "the number is directly under the weekday");
        let days = row(&buf, w);
        let nums = row(&buf, d);
        for (name, n) in ["Wed", "Thu", "Fri", "Sat", "Sun", "Mon", "Tue"]
            .into_iter()
            .zip(5..)
        {
            let at = days.find(name).unwrap();
            assert_eq!(nums[at..].split_whitespace().next(), Some(&*n.to_string()));
        }
        assert!(!nums.contains("2026") && !nums.contains("08-"), "{nums}");
    }

    #[test]
    fn a_month_in_a_narrow_panel_shows_fewer_numbers_and_never_a_date() {
        for width in [40, 50, 60, 100] {
            let s = series(&vec![1.0; 31]);
            let (plot, buf) = draw(Rect::new(0, 0, width, 12), &s, None);
            assert_eq!(plot.geo.per, 1, "{width}");
            let nums = numbers(&buf, &plot);
            let text: Vec<&str> = nums.iter().map(|(_, t)| t.as_str()).collect();
            // 31 days from Aug 5: the first is 5 and the last is Sep 4.
            assert_eq!(text.first(), Some(&"5"), "{width}: {text:?}");
            assert_eq!(text.last(), Some(&"4"), "{width}: {text:?}");
            assert!(text.iter().all(|t| t.len() <= 2), "{width}: {text:?}");
            // A column stays free between two numbers.
            assert!(
                nums.windows(2).all(|w| w[0].0 + w[0].1.len() < w[1].0),
                "{width}: {nums:?}"
            );
            let line = row(&buf, plot.geo.dates_y.unwrap());
            assert!(!line.contains("2026") && !line.contains("08-"), "{line}");
        }
    }

    #[test]
    fn a_wide_panel_shows_every_day_number_of_a_month() {
        let s = series(&vec![1.0; 31]);
        let (plot, buf) = draw(Rect::new(0, 0, 120, 12), &s, None);
        assert_eq!(numbers(&buf, &plot).len(), 31);
    }

    #[test]
    fn bars_that_hold_several_days_keep_short_date_labels() {
        let s = series(&vec![1.0; 90]);
        let (plot, buf) = draw(Rect::new(0, 0, 60, 12), &s, None);
        assert!(plot.geo.per > 1);
        let dates = row(&buf, plot.geo.dates_y.unwrap());
        assert!(dates.contains("08-05"), "{dates}");
    }

    #[test]
    fn weekday_ticks_follow_the_day_and_widen_with_the_room() {
        // 2026-08-05 is a Wednesday.
        let s = series(&[1.0; 16]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 14), &s, None);
        let y = plot.geo.weekdays_y.expect("a tall panel has a weekday row");
        let letters: String = row(&buf, y)
            .chars()
            .skip(usize::from(plot.geo.plot.x))
            .filter(|c| !c.is_whitespace())
            .take(7)
            .collect();
        assert_eq!(letters, "WTFSSMT");
        // A month in the same panel is too crowded for any weekday row.
        let month = series(&[1.0; 30]);
        let (plot, _) = draw(Rect::new(0, 0, 40, 14), &month, None);
        assert_eq!(plot.geo.weekdays_y, None);
        let week = series(&[1.0; 7]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 14), &week, None);
        let y = plot.geo.weekdays_y.unwrap();
        let names: Vec<String> = row(&buf, y)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert_eq!(names, ["Wed", "Thu", "Fri", "Sat", "Sun", "Mon", "Tue"]);
        // A short panel has no weekday row.
        let (plot, _) = draw(Rect::new(0, 0, 40, 9), &week, None);
        assert_eq!(plot.geo.weekdays_y, None);
    }

    #[test]
    fn a_week_of_bars_fills_the_plot_of_a_wide_panel() {
        // 118 inner columns less a 6 column gutter leaves 112 for seven bars.
        let g = Geometry::new(Rect::new(0, 0, 120, 12), 7, 4);
        assert_eq!(g.cells.iter().map(|c| c.1).sum::<u16>(), 112);
        assert!(g.span() >= 112 - 1, "the last bar reaches the plot edge");
    }

    #[test]
    fn y_axis_labels_keep_a_blank_column_from_the_box() {
        let s = series(&[1.0, 2.0, 3.0]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 12), &s, None);
        let p = plot.geo.plot;
        for y in [p.y, p.y + p.height - 1] {
            let line = row(&buf, y);
            let label_start = line.chars().position(|c| c == '$').expect("a label");
            assert!(label_start >= 2, "{line}");
            assert_eq!(buf[(plot.geo.inner.x, y)].symbol(), " ", "{line}");
        }
    }

    #[test]
    fn today_and_the_peak_day_are_marked_in_the_weekday_row() {
        let mut costs = vec![1.0; 7];
        costs[2] = 9.0;
        let s = series(&costs);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 14), &s, None);
        let y = plot.geo.weekdays_y.unwrap();
        let at = |i: usize| &buf[(plot.geo.x_of(i), y)];
        let pal = pal();
        assert_eq!(at(6).fg, pal.color(pal.hi), "today");
        assert!(at(6).modifier.contains(Modifier::UNDERLINED));
        assert_eq!(at(2).fg, pal.color(pal.warn), "peak");
        assert_ne!(at(3).fg, pal.color(pal.warn));
    }

    #[test]
    fn the_cursor_bar_is_bright_and_its_readout_names_the_day() {
        let s = series(&[1.0, 2.0, 3.0]);
        let (plot, buf) = draw(Rect::new(0, 0, 40, 12), &s, Some(1));
        let p = plot.geo.plot;
        let pal = pal();
        let bottom = p.y + p.height - 1;
        assert_eq!(buf[(plot.geo.x_of(1), bottom)].fg, pal.color(pal.title));
        assert_ne!(buf[(p.x, bottom)].fg, pal.color(pal.title));
        let readout = row(&buf, plot.geo.readout_y.unwrap());
        assert!(readout.contains("Thu 2026-08-06"), "{readout}");
        assert!(readout.contains("$2.00"), "{readout}");
        assert!(readout.contains("1 msgs"), "{readout}");
        // Without a cursor the readout shows the newest day, today.
        let (plot, buf) = draw(Rect::new(0, 0, 40, 12), &s, None);
        let readout = row(&buf, plot.geo.readout_y.unwrap());
        assert!(readout.contains("Fri 2026-08-07 today"), "{readout}");
    }

    #[test]
    fn the_readout_drops_parts_that_do_not_fit() {
        let s = series(&[1.0, 2.0]);
        let plot = Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Cost);
        let bar = &plot.bars[1];
        let full = readout(&plot, bar, true, true, 120);
        assert!(
            full.contains("today peak") && full.contains("opus-5 100%") && full.contains("tok"),
            "{full}"
        );
        let short = readout(&plot, bar, false, false, 24);
        assert_eq!(short, "Thu 2026-08-06  $2.00");
        assert_eq!(readout(&plot, bar, false, false, 8), "Thu 202~");
    }

    #[test]
    fn a_token_chart_leads_with_tokens() {
        let s = series(&[2.0]);
        let plot = Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Tokens);
        let line = readout(&plot, &plot.bars[0], false, false, 80);
        assert!(line.contains("2.0K tok  $2.00"), "{line}");
        assert!(!line.contains("opus"), "{line}");
    }

    #[test]
    fn the_totals_line_has_total_peak_and_average_and_shrinks_to_fit() {
        let s = series(&[1.0, 6.0, 2.0, 3.0]);
        let plot = Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Cost);
        assert_eq!(
            stats(&plot, &s, FIRST + 3, 80),
            "total $12.00  peak $6.00 on 08-06  avg $3.00/day"
        );
        assert_eq!(
            stats(&plot, &s, FIRST + 3, 39),
            "total $12.00  peak $6.00  avg $3.00/day"
        );
        assert_eq!(
            stats(&plot, &s, FIRST + 3, 35),
            "total $12.00  peak $6.00  avg $3.00"
        );
        assert_eq!(stats(&plot, &s, FIRST + 3, 24), "total $12.00  peak $6.00");
        assert_eq!(stats(&plot, &s, FIRST + 3, 12), "total $12.00");
        assert_eq!(stats(&plot, &s, FIRST + 3, 6), "total~");
    }

    #[test]
    fn the_average_counts_only_the_days_that_have_happened() {
        let s = series(&[2.0, 4.0, 0.0, 0.0]);
        let plot = Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Cost);
        // Today is the second day, so the average is $6 over two days.
        let line = stats(&plot, &s, FIRST + 1, 80);
        assert!(line.contains("avg $3.00/day"), "{line}");
        // Before the readout is moved it shows today, not the last day.
        assert_eq!(plot.default_index(FIRST + 1), Some(1));
        assert_eq!(plot.default_index(FIRST + 99), Some(3));
    }

    #[test]
    fn the_chart_draws_at_every_size_without_panicking() {
        let s = series(&vec![1.0; 45]);
        for w in 0..70 {
            for h in 0..22 {
                let panel = Rect::new(0, 0, w, h);
                let (plot, buf) = draw(panel, &s, Some(3));
                if w >= 3 && h >= 3 {
                    assert!(plot.geo.plot.height + 2 <= h, "{w}x{h}");
                }
                assert_eq!(buf.area, panel);
                let _ = Plot::new(panel, &[], Metric::Tokens);
                let _ = draw(panel, &[], None);
            }
        }
    }

    #[test]
    fn a_narrow_panel_drops_the_y_axis_but_keeps_the_bars() {
        let s = series(&[1.0, 2.0, 3.0]);
        let (plot, buf) = draw(Rect::new(0, 0, 14, 10), &s, None);
        assert_eq!(plot.geo.gutter, 0);
        assert!(screen(&buf).contains('\u{2588}'));
        assert!(!screen(&buf).contains('\u{2524}'));
    }

    #[test]
    fn a_range_wider_than_the_plot_is_added_up_and_the_axis_follows() {
        let s = series(&vec![1.0; 90]);
        let plot = Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Cost);
        assert_eq!(plot.geo.per, 3);
        assert_eq!(plot.bars.len(), 30);
        // Each bar holds three $1 days, so the scale tops out at $3.
        assert_eq!(plot.axis.max, 3.0);
        assert_eq!(plot.index_of(FIRST + 7), Some(2));
        assert_eq!(plot.index_of(FIRST + 900), None);
    }

    #[test]
    fn the_peak_index_is_the_tallest_bar_or_none() {
        let s = series(&[1.0, 9.0, 2.0]);
        assert_eq!(
            Plot::new(Rect::new(0, 0, 40, 12), &s, Metric::Cost).peak_index(),
            Some(1)
        );
        let z = series(&[0.0, 0.0]);
        assert_eq!(
            Plot::new(Rect::new(0, 0, 40, 12), &z, Metric::Cost).peak_index(),
            None
        );
    }

    /// Two models, sorted by cost, in slots 2 and 5.
    fn slots() -> Slots {
        let g = |key: &str, cost: f64| crate::aggregate::Group {
            key: key.into(),
            cost_usd: cost,
            ..Default::default()
        };
        Slots::of(&[g("opus", 9.0), g("sonnet", 1.0)], &[])
    }

    fn two_models(opus: f64, sonnet: f64) -> Vec<DayPoint> {
        vec![DayPoint {
            day: FIRST,
            cost_usd: opus + sonnet,
            tokens: 1000,
            messages: 2,
            models: vec![("opus".to_string(), opus), ("sonnet".to_string(), sonnet)],
        }]
    }

    #[test]
    fn runs_split_the_height_by_cost_biggest_model_first_and_end_at_the_top() {
        let slots = slots();
        let bar = &bars(&two_models(2.5, 1.5), 1)[0];
        let r = runs(bar, &slots, 32);
        assert_eq!(
            r,
            vec![(slots.slot("opus"), 20), (slots.slot("sonnet"), 32)]
        );
        // A model with no cost has no run, and the last run reaches the top
        // even when rounding falls short.
        let one = &bars(&two_models(0.0, 3.0), 1)[0];
        assert_eq!(runs(one, &slots, 9), vec![(slots.slot("sonnet"), 9)]);
        let thirds = &bars(&two_models(1.0, 1.0), 1)[0];
        assert_eq!(runs(thirds, &slots, 7).last().map(|r| r.1), Some(7));
        assert!(runs(bar, &slots, 0).is_empty());
    }

    #[test]
    fn a_stacked_bar_has_each_model_in_its_color_with_an_exact_edge() {
        let slots = slots();
        let pal = pal();
        let series = two_models(2.5, 1.5);
        let (plot, buf) = draw_with(Rect::new(0, 0, 40, 12), &series, None, Some(&slots), &pal);
        let (p, h) = (plot.geo.plot, plot.geo.plot.height);
        assert_eq!(plot.axis.max, 4.0, "the bar fills the plot");
        let x = plot.geo.x_of(0);
        let cell = |r: u16| &buf[(x, p.y + h - 1 - r)];
        let opus = pal.color(pal.series[slots.slot("opus")]);
        let sonnet = pal.color(pal.series[slots.slot("sonnet")]);
        assert_ne!(opus, sonnet);
        // 2.5 of 4 is 62.5%: the cell at that height holds the edge, with the
        // lower model as the block and the upper one behind it.
        let edge_row = (h * 5 / 8).min(h - 1);
        let edge = cell(edge_row);
        assert_eq!((edge.fg, edge.bg), (opus, sonnet), "edge cell");
        assert!(('\u{2581}'..='\u{2587}').contains(&edge.symbol().chars().next().unwrap()));
        assert_eq!(cell(0).fg, opus);
        assert_eq!(cell(h - 1).fg, sonnet);
        assert_eq!(cell(h - 1).symbol(), EIGHTHS[HALF_ROW - 1].to_string());
        assert_eq!(cell(h - 2).symbol(), "\u{2588}");
    }

    #[test]
    fn a_stacked_bar_is_as_tall_as_the_plain_one() {
        let series = two_models(2.5, 1.0);
        let slots = slots();
        let height = |buf: &Buffer, plot: &Plot| {
            let x = plot.geo.x_of(0);
            let p = plot.geo.plot;
            (p.y..p.y + p.height)
                .filter(|y| buf[(x, *y)].symbol() != " " && buf[(x, *y)].symbol() != "\u{2504}")
                .count()
        };
        let (plain, a) = draw_with(Rect::new(0, 0, 40, 12), &series, None, None, &pal());
        let (stacked, b) = draw_with(Rect::new(0, 0, 40, 12), &series, None, Some(&slots), &pal());
        assert_eq!(height(&a, &plain), height(&b, &stacked));
    }

    #[test]
    fn without_color_the_models_are_told_apart_by_pattern() {
        let slots = slots();
        let mono = Theme::Mono.palette(ColorDepth::True);
        let series = two_models(2.0, 2.0);
        let (plot, buf) = draw_with(Rect::new(0, 0, 40, 12), &series, None, Some(&slots), &mono);
        let (p, h) = (plot.geo.plot, plot.geo.plot.height);
        let x = plot.geo.x_of(0);
        let low = buf[(x, p.y + h - 1)].symbol().to_string();
        // The top row is a half block, so the pattern shows one row below.
        let high = buf[(x, p.y + 1)].symbol().to_string();
        assert_eq!(low, PATTERNS[slots.slot("opus")].to_string());
        assert_eq!(high, PATTERNS[slots.slot("sonnet")].to_string());
        assert_ne!(low, high);
        assert!(buf
            .content()
            .iter()
            .all(|c| c.fg == ratatui::style::Color::Reset));
    }

    #[test]
    fn a_stacked_chart_draws_at_every_size_without_panicking() {
        let slots = slots();
        let series: Vec<DayPoint> = (0..40)
            .map(|d| DayPoint {
                day: FIRST + d,
                ..two_models(1.0 + (d % 5) as f64, 0.5)[0].clone()
            })
            .collect();
        for w in 0..60 {
            for h in 0..16 {
                let _ = draw_with(
                    Rect::new(0, 0, w, h),
                    &series,
                    Some(2),
                    Some(&slots),
                    &pal(),
                );
            }
        }
    }

    fn hours(costs: &[f64]) -> Vec<DayPoint> {
        const START: i64 = 1_788_393_600;
        costs
            .iter()
            .enumerate()
            .map(|(i, c)| DayPoint {
                day: START + i as i64 * HOUR_SECONDS,
                cost_usd: *c,
                tokens: 10,
                messages: usize::from(*c > 0.0),
                models: vec![("opus".to_string(), *c)],
            })
            .collect()
    }

    #[test]
    fn an_hour_chart_has_a_bar_per_hour_and_no_weekday_row() {
        // Arrange
        crate::tz::set(crate::tz::Zone::Utc);
        let series = hours(&[0.0; 24]);

        // Act
        let plot = Plot::hourly(Rect::new(0, 0, 80, 14), &series);

        // Assert
        assert_eq!(plot.bars.len(), 24);
        assert_eq!(plot.geo.per, 1);
        assert_eq!(plot.scale, Scale::Hours);
        assert!(plot.geo.weekdays_y.is_none());
        assert!(plot.geo.dates_y.is_some());
    }

    #[test]
    fn the_hour_labels_read_the_clock_of_the_zone() {
        crate::tz::set(crate::tz::Zone::Utc);

        assert_eq!(hour_label(1_788_393_600), "00:00");
        assert_eq!(hour_label(1_788_393_600 + 14 * 3_600), "14:00");
        assert_eq!(hour_number(1_788_393_600), "");
        assert_eq!(hour_number(1_788_393_600 + 9 * 3_600), "9");
    }

    #[test]
    fn the_hour_readout_and_totals_name_the_hour_and_the_peak() {
        // Arrange
        crate::tz::set(crate::tz::Zone::Utc);
        let mut costs = [0.0; 24];
        costs[14] = 3.0;
        costs[23] = 1.0;
        let series = hours(&costs);
        let plot = Plot::hourly(Rect::new(0, 0, 100, 14), &series);

        // Act
        let stats = hour_stats(&series, 80);
        let readout = hour_readout(&plot.bars[14], false, true, 80);
        let last = hour_readout(&plot.bars[23], true, false, 80);
        let narrow = hour_stats(&series, 10);

        // Assert
        assert_eq!(stats, "total $4.00  peak $3.00 at 14:00");
        assert!(
            readout.starts_with("14:00 to 15:00 peak  $3.00"),
            "{readout}"
        );
        assert!(last.starts_with("23:00 to 00:00 now  $1.00"), "{last}");
        assert_eq!(narrow.chars().count(), 10);
    }

    #[test]
    fn an_hour_chart_draws_the_hour_numbers_under_the_bars() {
        // Arrange
        crate::tz::set(crate::tz::Zone::Utc);
        let mut costs = [0.0; 24];
        costs[5] = 2.0;
        let series = hours(&costs);
        let area = Rect::new(0, 0, 100, 14);
        let plot = Plot::hourly(area, &series);
        let mut buf = Buffer::empty(area);
        let pal = Theme::Standard.palette(ColorDepth::True);
        let view = View {
            fill: pal.cpu,
            stack: None,
            cursor: None,
            today: -1,
        };

        // Act
        render(&mut buf, &plot, &series, &view, &pal);

        // Assert
        let y = plot.geo.dates_y.unwrap();
        let row: String = (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(row.contains('0') && row.contains("23"), "{row}");
    }
}
