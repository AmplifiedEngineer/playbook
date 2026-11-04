// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The six panels: spend and token graphs, the model and project breakdowns,
//! the live sessions and the sessions of the range by day. Each is a rounded
//! box with its key hint and title in the top border.

use super::app::{App, ProjectsView, SpendView};
use super::chart::{self, Metric, Plot};
use super::data::{Data, Slots};
use super::fmt::{
    compact, duration_ms, fit, money, percent, signed_money, signed_percent, WEEKDAYS,
};
use super::layout::PanelId;
use super::sort::{
    self, cost_change, group_rows, group_tokens, pair_rows, session_day_rows, session_rows, Panel,
    Sort,
};
use super::theme::{Fill, Ink, Palette};
use super::view::{visible_rows, window_start};
use crate::aggregate::date_key;
use crate::period::Change;
use crate::query::{short_id, weekday, ActiveSession, Range, ACTIVE_SECONDS};
use crate::telemetry_db::{Health, ToolStat};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

/// A session whose last message is newer than this is working right now.
const LIVE_SECONDS: i64 = 120;

/// Meter cell: filled, and the empty rest.
const METER: &str = "\u{25a0}";
const METER_EMPTY: &str = "\u{b7}";

fn fg(pal: &Palette, ink: Ink) -> Style {
    Style::default().fg(pal.color(ink))
}

/// The selected row: the theme's selection colors, or reverse video in mono.
fn selected_style(pal: &Palette) -> Style {
    if pal.mono {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(pal.color(pal.sel_fg))
            .bg(pal.color(pal.sel_bg))
            .add_modifier(Modifier::BOLD)
    }
}

/// `┤¹Title├` the way a classic system monitor writes a box title, with the panel's key hint in the
/// accent color. The focused panel's hint is a badge.
fn title_line(
    pal: &Palette,
    panel: Panel,
    text: &str,
    box_ink: Ink,
    focused: bool,
) -> Line<'static> {
    let edge = fg(pal, box_ink);
    let hint = if focused {
        let badge = Style::default().add_modifier(Modifier::BOLD);
        if pal.mono {
            badge.add_modifier(Modifier::REVERSED)
        } else {
            badge.fg(pal.color(pal.title)).bg(pal.color(pal.hi))
        }
    } else {
        fg(pal, pal.hi)
    };
    let name = fg(pal, pal.title).add_modifier(if focused {
        Modifier::BOLD
    } else {
        Modifier::empty()
    });
    Line::from(vec![
        Span::styled("\u{2500}\u{2524}", edge),
        Span::styled(panel.hint(), hint),
        Span::styled(text.to_string(), name),
        Span::styled("\u{251c}", edge),
    ])
}

/// A note in the border, `┤note├`, for the right of the top border or the
/// bottom border.
fn note_line(pal: &Palette, text: &str, box_ink: Ink) -> Line<'static> {
    let edge = fg(pal, box_ink);
    Line::from(vec![
        Span::styled("\u{2524}", edge),
        Span::styled(text.to_string(), fg(pal, pal.fg)),
        Span::styled("\u{251c}\u{2500}", edge),
    ])
}

fn panel_block(
    pal: &Palette,
    panel: Panel,
    title: &str,
    box_ink: Ink,
    focused: bool,
) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(pal, box_ink))
        .title(title_line(pal, panel, title, box_ink, focused))
}

/// A panel box. The Spend and Projects panels also name the view `v` has
/// them showing; no other panel has a view, so the others show nothing.
fn view_block(
    pal: &Palette,
    app: &App,
    area: Rect,
    panel: Panel,
    slot: Option<PanelId>,
    title: &str,
    box_ink: Ink,
) -> Block<'static> {
    let focused = slot.map_or(app.focus == panel, |id| app.focus_slot() == Some(id));
    let block = panel_block(pal, panel, title, box_ink, focused);
    let name = match panel {
        // A pinned panel always shows one view, so it has no tag to switch.
        Panel::Spend if slot.is_some_and(|id| id.spend_view().is_some()) => return block,
        Panel::Spend => app.spend_view.name(),
        Panel::Projects if slot.is_some_and(|id| id.projects_view().is_some()) => return block,
        Panel::Projects => app.projects_view.name(),
        _ => return block,
    };
    // The title, then the tag in its long form, else its short form, else no
    // tag when the box is too narrow for either.
    let room = usize::from(area.width).saturating_sub(2 + 1 + panel.hint().chars().count());
    let used = title.chars().count() + 2;
    let long = format!(" view: {name}");
    let short = format!(": {name}");
    let text = [long, short]
        .into_iter()
        .find(|t| used + t.chars().count() + 3 <= room);
    let Some(text) = text else { return block };
    let edge = fg(pal, box_ink);
    block.title(Line::from(vec![
        Span::styled("\u{2524}", edge),
        Span::styled("v", fg(pal, pal.hi).add_modifier(Modifier::BOLD)),
        Span::styled(text, fg(pal, pal.fg)),
        Span::styled("\u{251c}", edge),
    ]))
}

fn num(text: impl Into<String>) -> Cell<'static> {
    Cell::from(Text::from(text.into()).alignment(Alignment::Right))
}

/// One table column. `min_inner` is the inner panel width below which the
/// column is left out, so a narrow terminal keeps the names readable. A label
/// starting with `>` is right aligned, like its numbers.
struct Col {
    head: &'static str,
    width: Constraint,
    min_inner: u16,
}

const fn col(head: &'static str, width: Constraint, min_inner: u16) -> Col {
    Col {
        head,
        width,
        min_inner,
    }
}

/// The header row: sorted column marked with an arrow, in the title color.
fn head(pal: &Palette, cols: &[Col], keep: &[bool], sort: Sort, sortable: bool) -> Row<'static> {
    let cells = cols
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(i, c)| {
            let (label, right) = match c.head.strip_prefix('>') {
                Some(l) => (l, true),
                None => (c.head, false),
            };
            let mark = match (sortable && sort.col == i, sort.desc) {
                (true, true) => "\u{25bc}",
                (true, false) => "\u{25b2}",
                _ => "",
            };
            let text = format!("{label}{mark}");
            if right {
                num(text)
            } else {
                Cell::from(text)
            }
        });
    Row::new(cells).style(fg(pal, pal.title).add_modifier(Modifier::BOLD))
}

/// Draws a table inside its panel box. `rows` gets the width in cells of every
/// column (0 for a column that is left out) so it can cut text to fit, and
/// returns one cell per column for every row. Only the rows that fit are drawn,
/// scrolled so the selected one is in view.
#[allow(clippy::too_many_arguments)]
fn table<'a>(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pal: &Palette,
    panel: Panel,
    focused: bool,
    block: Block<'static>,
    cols: &[Col],
    sortable: bool,
    rows: impl FnOnce(&[usize]) -> Vec<Vec<Cell<'a>>>,
) {
    let inner = area.width.saturating_sub(2);
    let keep: Vec<bool> = cols.iter().map(|c| inner >= c.min_inner).collect();
    let constraints: Vec<Constraint> = cols
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(c, _)| c.width)
        .collect();
    // The table spaces columns by one cell inside its border.
    let shown = Layout::horizontal(&constraints)
        .spacing(1)
        .split(Rect::new(0, 0, inner, 1));
    let mut widths = vec![0; cols.len()];
    let mut next = shown.iter();
    for (i, k) in keep.iter().enumerate() {
        if *k {
            widths[i] = next.next().map_or(0, |r| usize::from(r.width));
        }
    }
    let all = rows(&widths);
    let visible = visible_rows(area);
    let selected = app.selection(panel);
    let start = window_start(selected, visible);
    let rows: Vec<Row> = all
        .into_iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, cells)| {
            let row = Row::new(
                cells
                    .into_iter()
                    .zip(&keep)
                    .filter(|(_, k)| **k)
                    .map(|(c, _)| c),
            );
            if focused && i == selected {
                row.style(selected_style(pal))
            } else {
                row
            }
        })
        .collect();
    let header = head(pal, cols, &keep, app.sort(panel), sortable);
    frame.render_widget(
        Table::new(rows, constraints).header(header).block(block),
        area,
    );
}

fn empty(frame: &mut Frame, area: Rect, pal: &Palette, block: Block<'static>, message: &str) {
    frame.render_widget(
        Paragraph::new(message.to_string())
            .style(fg(pal, pal.inactive))
            .wrap(Wrap { trim: true })
            .block(block),
        area,
    );
}

/// A meter of `■` cells colored along the gradient up to `fraction`,
/// the rest dim dots.
fn meter(pal: &Palette, fill: Fill, fraction: f64, width: usize) -> Cell<'static> {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let mut spans = Vec::with_capacity(width);
    for i in 0..width {
        spans.push(if i < filled {
            let pct = (i + 1) as f64 / width as f64 * 100.0;
            Span::styled(METER, Style::default().fg(pal.graded(fill, pct)))
        } else {
            Span::styled(METER_EMPTY, fg(pal, pal.meter_bg))
        });
    }
    Cell::from(Line::from(spans))
}

/// A cost cell tinted by how it compares with the largest one on screen.
fn cost_cell(pal: &Palette, cost: f64, peak: f64) -> Cell<'static> {
    let pct = if peak > 0.0 { cost / peak * 100.0 } else { 0.0 };
    num(money(cost)).style(Style::default().fg(pal.graded(pal.process, pct)))
}

/// The legend of the bar colors, in the bottom border: the gradient from a low
/// value to a high one. The mono theme has no colors, so it has no legend.
fn gradient_legend(pal: &Palette, fill: Fill, what: &str, box_ink: Ink) -> Option<Line<'static>> {
    if pal.mono {
        return None;
    }
    let edge = fg(pal, box_ink);
    let mut spans = vec![
        Span::styled("\u{2524}", edge),
        Span::styled(format!("{what} low "), fg(pal, pal.fg)),
    ];
    for pct in [10.0, 30.0, 50.0, 70.0, 90.0] {
        spans.push(Span::styled(
            METER,
            Style::default().fg(pal.graded(fill, pct)),
        ));
    }
    spans.push(Span::styled(" high", fg(pal, pal.fg)));
    spans.push(Span::styled("\u{251c}\u{2500}", edge));
    Some(Line::from(spans))
}

/// The chart a panel shows, laid out for its box. The cursor keys use the same
/// layout, so the cursor lands on the bar drawn.
pub fn chart_plot(area: Rect, data: &Data, panel: Panel) -> Plot {
    let metric = match panel {
        Panel::Tokens => Metric::Tokens,
        _ => Metric::Cost,
    };
    Plot::new(area, &data.series, metric)
}

/// The model legend of a stacked chart, in the bottom border: a swatch and a
/// short name for each model, the biggest first, as many as fit, then `+N`.
fn model_legend(pal: &Palette, slots: &Slots, width: usize, box_ink: Ink) -> Line<'static> {
    let edge = fg(pal, box_ink);
    let room = width.saturating_sub(4);
    let mut spans = vec![Span::styled("\u{2524}", edge)];
    let mut used = 0;
    for (i, name) in slots.order.iter().enumerate() {
        let short = short_model(name);
        let left = slots.order.len() - i;
        let more = format!(" +{left}");
        let need = 2 + short.chars().count() + usize::from(i > 0);
        // Keep room for the `+N` unless this is the last one.
        let reserve = if left > 1 { more.chars().count() } else { 0 };
        if used + need + reserve > room {
            let space = if i > 0 { " " } else { "" };
            spans.push(Span::styled(format!("{space}+{left}"), fg(pal, pal.fg)));
            break;
        }
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(swatch(pal, slots.slot(name)));
        spans.push(Span::styled(format!(" {short}"), fg(pal, pal.fg)));
        used += need;
    }
    spans.push(Span::styled("\u{251c}\u{2500}", edge));
    Line::from(spans)
}

/// A model's mark: a block in its color, or its pattern without color.
pub fn swatch(pal: &Palette, slot: usize) -> Span<'static> {
    if pal.mono {
        Span::raw(chart::PATTERNS[slot % chart::PATTERNS.len()].to_string())
    } else {
        Span::styled(
            METER,
            Style::default().fg(pal.color(chart::series_ink(pal, slot))),
        )
    }
}

/// Cost per day or tokens per day: a bar chart with axes, a cursor and a legend.
fn chart_panel(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    data: &Data,
    pal: &Palette,
    panel: Panel,
    slot: Option<PanelId>,
) {
    let (name, fill, box_ink, what) = match panel {
        Panel::Tokens => ("Tokens", pal.down, pal.box_net, "tokens"),
        _ => ("Spend", pal.cpu, pal.box_cpu, "cost"),
    };
    if data.series.is_empty() {
        let block = view_block(pal, app, area, panel, slot, name, box_ink);
        return empty(frame, area, pal, block, "No usage in this range.");
    }
    let plot = chart_plot(area, data, panel);
    let per = match plot.geo.per {
        1 => "day".to_string(),
        n => format!("{n} days"),
    };
    let view = slot.and_then(PanelId::spend_view).unwrap_or(app.spend_view);
    let stacked = panel == Panel::Spend && view == SpendView::ByModel;
    // Today charts the last seven days, which the title says.
    let span = if data.range == Range::Today {
        ", 7 days"
    } else {
        ""
    };
    let title = match panel {
        Panel::Tokens => format!("Tokens per {per}{span}"),
        _ if stacked => format!("Spend per {per} by model, USD{span}"),
        _ => format!("Spend per {per}, USD{span}"),
    };
    let mut block = view_block(pal, app, area, panel, slot, &title, box_ink);
    if plot.geo.stats_y.is_none() {
        // No room for the totals line, so the peak goes in the bottom border.
        let peak = plot
            .bars
            .iter()
            .map(|b| b.value(plot.metric))
            .fold(0.0, f64::max);
        let note = format!("peak {}", plot.metric.show(peak));
        block = block.title_bottom(Line::from(vec![
            Span::styled("\u{2524}", fg(pal, box_ink)),
            Span::styled(note, fg(pal, pal.fg)),
            Span::styled("\u{251c}", fg(pal, box_ink)),
        ]));
    }
    if stacked {
        let legend = model_legend(pal, &data.slots, usize::from(area.width), box_ink);
        block = block.title_bottom(legend.right_aligned());
    } else if let Some(legend) = gradient_legend(pal, fill, what, box_ink) {
        block = block.title_bottom(legend.right_aligned());
    }
    frame.render_widget(block, area);
    let view = chart::View {
        fill,
        stack: stacked.then_some(&data.slots),
        cursor: app.cursor.and_then(|day| plot.index_of(day)),
        today: crate::tz::day_of(data.now),
    };
    chart::render(frame.buffer_mut(), &plot, &data.series, &view, pal);
}

/// Cost per day: a chart, stacked by model, or a table of the days.
pub fn spend(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette, slot: PanelId) {
    match slot.spend_view().unwrap_or(app.spend_view) {
        SpendView::Daily => daily_table(frame, area, app, data, pal, slot),
        _ => chart_panel(frame, area, app, data, pal, Panel::Spend, Some(slot)),
    }
}

/// How today and the peak day are marked: in color and in type, so the mono
/// theme shows them too.
pub fn today_style(pal: &Palette) -> Style {
    fg(pal, pal.hi).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

pub fn peak_style(pal: &Palette) -> Style {
    fg(pal, pal.warn).add_modifier(Modifier::BOLD)
}

/// One row for each day up to today: date, messages, tokens, cost and the model
/// that cost the most. Today and the peak day are marked.
fn daily_table(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    data: &Data,
    pal: &Palette,
    slot: PanelId,
) {
    let today = crate::tz::day_of(data.now);
    let focused = app.focus_slot() == Some(slot);
    let mut block = view_block(
        pal,
        app,
        area,
        Panel::Spend,
        Some(slot),
        "Daily cost",
        pal.box_cpu,
    );
    let edge = fg(pal, pal.box_cpu);
    block = block.title_bottom(
        Line::from(vec![
            Span::styled("\u{2524}", edge),
            Span::styled("today", today_style(pal)),
            Span::raw(" "),
            Span::styled("peak", peak_style(pal)),
            Span::styled("\u{251c}\u{2500}", edge),
        ])
        .right_aligned(),
    );
    let rows = sort::day_rows(&data.series, today, app.sort(Panel::Spend));
    if rows.is_empty() {
        return empty(frame, area, pal, block, "No usage in this range.");
    }
    let peak = rows.iter().map(|d| d.cost_usd).fold(0.0, f64::max);
    let peak_day = rows
        .iter()
        .find(|d| d.cost_usd == peak && peak > 0.0)
        .map(|d| d.day);
    let long = area.width.saturating_sub(2) >= 52;
    let cols = [
        col("date", Constraint::Length(if long { 14 } else { 9 }), 0),
        col(">msgs", Constraint::Length(5), 0),
        col(">tokens", Constraint::Length(7), 0),
        col(">cost", Constraint::Length(9), 0),
        col("top model", Constraint::Fill(1), 40),
    ];
    table(
        frame,
        area,
        app,
        pal,
        Panel::Spend,
        focused,
        block,
        &cols,
        true,
        |w| {
            rows.iter()
                .map(|d| {
                    let date = date_key(d.day);
                    let text = format!(
                        "{} {}",
                        WEEKDAYS[weekday(d.day)],
                        if long { date.as_str() } else { &date[5..] }
                    );
                    let style = if d.day == today {
                        today_style(pal)
                    } else if Some(d.day) == peak_day {
                        peak_style(pal)
                    } else {
                        Style::default()
                    };
                    let top = d
                        .top_model()
                        .map_or(String::new(), |m| short_model(m).to_string());
                    let model = match d.top_model() {
                        Some(m) => Span::styled(
                            fit(&top, w[4]),
                            Style::default()
                                .fg(pal.color(chart::series_ink(pal, data.slots.slot(m)))),
                        ),
                        None => Span::raw(""),
                    };
                    vec![
                        Cell::from(text).style(style),
                        num(d.messages.to_string()),
                        num(compact(d.tokens)),
                        cost_cell(pal, d.cost_usd, peak),
                        Cell::from(Line::from(model)),
                    ]
                })
                .collect()
        },
    );
}

/// Tokens per day.
pub fn tokens(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette) {
    chart_panel(frame, area, app, data, pal, Panel::Tokens, None);
}

/// How a change reads in color: more spend is the warning color, less is the
/// live green, and no change is plain. The sign in the text carries it without
/// color.
fn change_style(pal: &Palette, amount: f64) -> Style {
    if amount > 0.0 {
        fg(pal, pal.warn)
    } else if amount < 0.0 {
        fg(pal, pal.live)
    } else {
        Style::default()
    }
}

/// A change cell: its text in the color of its direction.
fn change_cell(pal: &Palette, text: String, amount: f64) -> Cell<'static> {
    num(text).style(change_style(pal, amount))
}

/// Models: messages, tokens and cost. For a period, also the cost in the
/// previous one and the change in amount and percent, for every model of either
/// period. Without a comparison, the share of the total cost instead.
pub fn models(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette) {
    let focused = app.focus == Panel::Models;
    let mut block = panel_block(pal, Panel::Models, "Models", pal.box_mem, focused);
    if let Some(c) = &data.comparison {
        let note = format!("vs {}", c.period.previous_label());
        block = block.title(note_line(pal, &note, pal.box_mem).right_aligned());
    }
    let sorted = pair_rows(&data.models, &data.models_before, app.sort(Panel::Models));
    if sorted.is_empty() {
        return empty(frame, area, pal, block, "No usage in this range.");
    }
    let compared = data.comparison.is_some();
    let mut cols = vec![
        col("name", Constraint::Fill(1), 0),
        col(
            ">msgs",
            Constraint::Length(5),
            if compared { 64 } else { 0 },
        ),
        col(
            ">tokens",
            Constraint::Length(7),
            if compared { 64 } else { 0 },
        ),
        col(">cost", Constraint::Length(if compared { 8 } else { 9 }), 0),
    ];
    if compared {
        cols.extend([
            col(">prev", Constraint::Length(8), 0),
            col(">change", Constraint::Length(8), 0),
            col(">%", Constraint::Length(7), 46),
        ]);
    } else {
        cols.push(col("share", Constraint::Length(8), 48));
    }
    let total = data.totals.cost_usd;
    table(
        frame,
        area,
        app,
        pal,
        Panel::Models,
        app.focus == Panel::Models,
        block,
        &cols,
        true,
        |w| {
            sorted
                .iter()
                .map(|p| {
                    let name = short_model(&p.key);
                    let mut cells = vec![
                        // A swatch in the model's color, as in the stacked chart.
                        Cell::from(Line::from(vec![
                            swatch(pal, data.slots.slot(&p.key)),
                            // The space after the swatch goes first when the name is tight.
                            Span::raw(match w[0] {
                                0..=15 => fit(name, w[0].saturating_sub(1)),
                                _ => format!(" {}", fit(name, w[0] - 2)),
                            }),
                        ])),
                        num(p.current.messages.to_string()),
                        num(compact(group_tokens(&p.current))),
                        num(money(p.current.cost_usd)),
                    ];
                    if compared {
                        let delta = cost_change(p);
                        let percent = Change::of(p.current.cost_usd, p.previous.cost_usd).percent;
                        cells.extend([
                            num(money(p.previous.cost_usd)).style(Style::default()),
                            change_cell(pal, signed_money(delta), delta),
                            change_cell(
                                pal,
                                signed_percent(percent, p.current.cost_usd > 0.0),
                                delta,
                            ),
                        ]);
                    } else {
                        let share = if total > 0.0 {
                            p.current.cost_usd / total
                        } else {
                            0.0
                        };
                        cells.push(meter(pal, pal.cached, share, 8));
                    }
                    cells
                })
                .collect()
        },
    );
}

/// Projects of the period: messages, tokens, cost and share of the total cost.
pub fn projects(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    data: &Data,
    pal: &Palette,
    slot: PanelId,
) {
    match slot.projects_view().unwrap_or(app.projects_view) {
        ProjectsView::Agents => return agents(frame, area, app, data, pal, slot),
        ProjectsView::Skills => return skills(frame, area, app, data, pal, slot),
        ProjectsView::Tools => return tools(frame, area, app, data, pal, slot),
        ProjectsView::Health => return health(frame, area, app, data, pal, slot),
        ProjectsView::Projects => {}
    }
    let block = view_block(
        pal,
        app,
        area,
        Panel::Projects,
        Some(slot),
        "Projects",
        pal.box_mem,
    );
    if data.repos.is_empty() {
        return empty(frame, area, pal, block, "No usage in this range.");
    }
    let total_cost = data.totals.cost_usd;
    let sorted = group_rows(&data.repos, app.sort(Panel::Projects));
    let cols = [
        col("name", Constraint::Fill(1), 0),
        col(">msgs", Constraint::Length(5), 0),
        col(">tokens", Constraint::Length(7), 0),
        col(">cost", Constraint::Length(9), 0),
        col("share", Constraint::Length(8), 48),
    ];
    table(
        frame,
        area,
        app,
        pal,
        Panel::Projects,
        app.focus_slot() == Some(slot),
        block,
        &cols,
        true,
        |w| {
            sorted
                .iter()
                .map(|g| {
                    let share = if total_cost > 0.0 {
                        g.cost_usd / total_cost
                    } else {
                        0.0
                    };
                    vec![
                        Cell::from(fit(&g.key, w[0])),
                        num(g.messages.to_string()),
                        num(compact(group_tokens(g))),
                        num(money(g.cost_usd)),
                        meter(pal, pal.avail, share, 8),
                    ]
                })
                .collect()
        },
    );
}

/// Cost per agent, from telemetry. The transcript totals stay the totals; the
/// note says how much of the spend telemetry covers.
fn agents(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette, slot: PanelId) {
    telemetry_split(
        frame,
        area,
        app,
        data,
        pal,
        slot,
        ("Agents", "agent"),
        &data.agents,
    );
}

/// Cost per skill, from telemetry. Only requests made while a skill was
/// active carry its name, so the rows are a share of the telemetry cost.
fn skills(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette, slot: PanelId) {
    telemetry_split(
        frame,
        area,
        app,
        data,
        pal,
        slot,
        ("Skills", "skill"),
        &data.skills,
    );
}

/// The empty state of a telemetry panel: short lines that name the cause
/// (telemetry off, collector down, or no rows yet) and each command on a line
/// of its own.
fn telemetry_off(app: &App) -> String {
    crate::telemetry_status::empty_panel_lines(app.telemetry).join("\n")
}

fn telemetry_split(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    data: &Data,
    pal: &Palette,
    slot: PanelId,
    (title, label): (&str, &'static str),
    view: &crate::query::AgentView,
) {
    let mut block = view_block(
        pal,
        app,
        area,
        Panel::Projects,
        Some(slot),
        title,
        pal.box_mem,
    );
    if !data.telemetry_rows && view.is_empty() {
        return empty(frame, area, pal, block, &telemetry_off(app));
    }
    if view.is_empty() {
        let text = format!("No {label} activity in this range. v switches the view.");
        return empty(frame, area, pal, block, &text);
    }
    let covered = view.coverage(data.totals.cost_usd) * 100.0;
    let note = if view.from_transcripts {
        "from transcripts, counts only".to_string()
    } else {
        format!("source telemetry {covered:.0}% of spend")
    };
    block = block.title(note_line(pal, &note, pal.box_mem).right_aligned());
    let from_transcripts = view.from_transcripts;
    let total_cost = if view.from_transcripts {
        view.rows.iter().map(|g| g.messages as f64).sum()
    } else {
        view.cost_usd
    };
    // Transcript rows have no cost, so the cost column sorts by count instead.
    let mut sort = app.sort(Panel::Projects);
    if from_transcripts && sort.col == 3 {
        sort.col = 1;
    }
    let sorted = group_rows(&view.rows, sort);
    let cols = [
        col(label, Constraint::Fill(1), 0),
        col(">reqs", Constraint::Length(5), 0),
        col(">tokens", Constraint::Length(7), 0),
        col(">cost", Constraint::Length(9), 0),
        col("share", Constraint::Length(8), 48),
    ];
    table(
        frame,
        area,
        app,
        pal,
        Panel::Projects,
        app.focus_slot() == Some(slot),
        block,
        &cols,
        true,
        |w| {
            sorted
                .iter()
                .map(|g| {
                    let weight = if from_transcripts {
                        g.messages as f64
                    } else {
                        g.cost_usd
                    };
                    let share = if total_cost > 0.0 {
                        weight / total_cost
                    } else {
                        0.0
                    };
                    vec![
                        Cell::from(fit(&g.key, w[0])),
                        num(g.messages.to_string()),
                        num(compact(group_tokens(g))),
                        num(if from_transcripts {
                            "n/a".to_string()
                        } else {
                            money(g.cost_usd)
                        }),
                        meter(pal, pal.avail, share, 8),
                    ]
                })
                .collect()
        },
    );
}

/// Rows in the health view: the figures listed in `health`.
pub const HEALTH_ROWS: usize = 9;

/// The title note of a telemetry view: its source and, for a period, the
/// span it is compared with.
fn telemetry_note(pal: &Palette, data: &Data) -> Line<'static> {
    let text = match &data.comparison {
        Some(c) => format!("source telemetry  vs {}", c.period.previous_label()),
        None => "source telemetry".to_string(),
    };
    note_line(pal, &text, pal.box_mem).right_aligned()
}

/// Latency and error rate per tool, from telemetry. For a period each tool
/// also shows its average latency in the previous span and the change.
fn tools(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette, slot: PanelId) {
    let mut block = view_block(
        pal,
        app,
        area,
        Panel::Projects,
        Some(slot),
        "Tools",
        pal.box_mem,
    );
    if !data.telemetry_rows {
        return empty(frame, area, pal, block, &telemetry_off(app));
    }
    if data.health.tools.is_empty() {
        return empty(
            frame,
            area,
            pal,
            block,
            "No tool calls in this range. v switches the view.",
        );
    }
    block = block.title(telemetry_note(pal, data));
    let compared = data.comparison.is_some();
    // The comparison columns only exist for a period, and only when wide.
    let min = if compared { 46 } else { u16::MAX };
    let cols = [
        col("tool", Constraint::Fill(1), 0),
        col(">calls", Constraint::Length(6), 0),
        col(">err", Constraint::Length(5), 0),
        col(">avg", Constraint::Length(7), 0),
        col(">max", Constraint::Length(7), 70),
        col(">prev", Constraint::Length(7), min),
        col(">change", Constraint::Length(8), min),
    ];
    let before: std::collections::BTreeMap<&str, &ToolStat> = data
        .health
        .tools_before
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();
    table(
        frame,
        area,
        app,
        pal,
        Panel::Projects,
        app.focus_slot() == Some(slot),
        block,
        &cols,
        false,
        |w| {
            data.health
                .tools
                .iter()
                .map(|t| {
                    let rate_style = if t.failures > 0 {
                        fg(pal, pal.warn)
                    } else {
                        Style::default()
                    };
                    let mut cells = vec![
                        Cell::from(fit(&t.name, w[0])),
                        num(t.calls.to_string()),
                        num(percent(t.error_rate())).style(rate_style),
                        num(duration_ms(t.avg_ms())),
                        num(duration_ms(t.max_ms as f64)),
                    ];
                    match before.get(t.name.as_str()) {
                        Some(p) => {
                            let delta = t.avg_ms() - p.avg_ms();
                            let pct = Change::of(t.avg_ms(), p.avg_ms()).percent;
                            cells.push(num(duration_ms(p.avg_ms())).style(Style::default()));
                            cells.push(change_cell(pal, signed_percent(pct, t.calls > 0), delta));
                        }
                        None => {
                            cells.push(num("-").style(Style::default()));
                            cells.push(change_cell(pal, "new".to_string(), 0.0));
                        }
                    }
                    cells
                })
                .collect()
        },
    );
}

/// One figure of the health view. `better_up` says a rise is good news (cache
/// hit rate), else a rise is a warning.
struct Figure {
    name: &'static str,
    now: f64,
    before: Option<f64>,
    now_text: String,
    before_text: Option<String>,
    better_up: bool,
}

/// A count with its rate beside it: `6 (2.9%)`.
fn counted(count: u64, rate: f64) -> String {
    format!("{count} ({})", percent(rate))
}

/// The figures of the health view, the ones that tell something is wrong
/// first. Only a few rows fit in a small terminal and the table scrolls.
fn figures(data: &Data) -> Vec<Figure> {
    let h = &data.health;
    let active = h.now.active_ms > 0 || h.before.as_ref().is_some_and(|b| b.active_ms > 0);
    let hit = |t: &crate::query::Totals| {
        let all = t.input_tokens + t.cache_creation_tokens + t.cache_read_tokens;
        if all == 0 {
            0.0
        } else {
            t.cache_read_tokens as f64 / all as f64
        }
    };
    let figure = |name, better_up, f: &dyn Fn(&Health) -> (f64, String)| {
        let (now, now_text) = f(&h.now);
        let before = h.before.as_ref().map(f);
        Figure {
            name,
            now,
            before: before.as_ref().map(|b| b.0),
            now_text,
            before_text: before.map(|b| b.1),
            better_up,
        }
    };
    let cache = data.comparison.as_ref().map(|c| hit(&c.previous));
    vec![
        figure("API errors", false, &|h| {
            (h.errors as f64, counted(h.errors, h.api_error_rate()))
        }),
        figure("Retries", false, &|h| {
            (h.retried as f64, h.retried.to_string())
        }),
        figure("Tool errors", false, &|h| {
            (
                h.tool_failures as f64,
                counted(h.tool_failures, h.tool_error_rate()),
            )
        }),
        // Claude Code's own active time when it was collected, else the
        // floor made of API and tool time.
        if active {
            figure("Active time", true, &|h| {
                (h.active_ms as f64, duration_ms(h.active_ms as f64))
            })
        } else {
            figure("Busy time", true, &|h| {
                (h.busy_ms() as f64, duration_ms(h.busy_ms() as f64))
            })
        },
        Figure {
            name: "Cache hit (transcripts)",
            now: hit(&data.totals),
            before: cache,
            now_text: percent(hit(&data.totals)),
            before_text: cache.map(percent),
            better_up: true,
        },
        figure("API requests", true, &|h| {
            (h.requests as f64, compact(h.requests))
        }),
        figure("Lines changed", true, &|h| {
            (h.lines_changed as f64, compact(h.lines_changed))
        }),
        figure("Commits", true, &|h| (h.commits as f64, compact(h.commits))),
        figure("Pull requests", true, &|h| {
            (h.pull_requests as f64, compact(h.pull_requests))
        }),
    ]
}

/// API errors, retries, tool errors, busy time and cache hit rate for the
/// range against the previous span, from telemetry. The cache hit rate comes
/// from transcripts, so it stays available without telemetry.
fn health(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette, slot: PanelId) {
    let mut block = view_block(
        pal,
        app,
        area,
        Panel::Projects,
        Some(slot),
        "Health",
        pal.box_mem,
    );
    if !data.telemetry_rows {
        return empty(frame, area, pal, block, &telemetry_off(app));
    }
    block = block.title(telemetry_note(pal, data));
    let min = if data.health.before.is_some() {
        0
    } else {
        u16::MAX
    };
    let cols = [
        col("figure", Constraint::Fill(1), 0),
        col(">now", Constraint::Length(12), 0),
        col(">before", Constraint::Length(12), min),
        col(">change", Constraint::Length(8), min),
    ];
    let rows = figures(data);
    debug_assert_eq!(rows.len(), HEALTH_ROWS);
    table(
        frame,
        area,
        app,
        pal,
        Panel::Projects,
        app.focus_slot() == Some(slot),
        block,
        &cols,
        false,
        |w| {
            rows.iter()
                .map(|f| {
                    let mut cells = vec![Cell::from(fit(f.name, w[0])), num(f.now_text.clone())];
                    if let (Some(b), Some(text)) = (f.before, &f.before_text) {
                        let delta = f.now - b;
                        let pct = Change::of(f.now, b).percent;
                        // A rise reads as a warning unless a rise is the good news.
                        let tone = if f.better_up { -delta } else { delta };
                        cells.push(num(text.clone()).style(Style::default()));
                        cells.push(change_cell(pal, signed_percent(pct, f.now > 0.0), tone));
                    }
                    cells
                })
                .collect()
        },
    );
}

/// `45s`, `7m`, `2h`.
fn age(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// A model name without the vendor prefix, which every row shares.
fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

fn session_state(now: i64, s: &ActiveSession, pal: &Palette) -> (&'static str, Color) {
    if now - s.last < LIVE_SECONDS {
        ("live", pal.color(pal.live))
    } else {
        ("idle", pal.color(pal.warn))
    }
}

/// Sessions with a message in the last 15 minutes, with state and cost so far.
pub fn live(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette) {
    let live = &data.live;
    let focused = app.focus == Panel::Live;
    let block = panel_block(pal, Panel::Live, "Live sessions", pal.box_proc, focused).title(
        note_line(
            pal,
            &format!(
                "hour {}  today {}",
                money(live.hour.cost_usd),
                money(live.today.cost_usd)
            ),
            pal.box_proc,
        )
        .right_aligned(),
    );
    if live.active.is_empty() {
        let minutes = ACTIVE_SECONDS / 60;
        let text = format!("No session has sent a message in the last {minutes} minutes.");
        return empty(frame, area, pal, block, &text);
    }
    let cols = [
        col("state", Constraint::Length(5), 0),
        col("project", Constraint::Fill(1), 0),
        col("model", Constraint::Fill(1), 0),
        col(">msgs", Constraint::Length(5), 46),
        col(">cost", Constraint::Length(8), 0),
        col(">ago", Constraint::Length(4), 0),
    ];
    let rows = session_rows(&live.active, app.sort(Panel::Live));
    let peak = rows.iter().map(|s| s.cost_usd).fold(0.0, f64::max);
    table(
        frame,
        area,
        app,
        pal,
        Panel::Live,
        app.focus == Panel::Live,
        block,
        &cols,
        true,
        |w| {
            rows.iter()
                .map(|s| {
                    let (state, color) = session_state(data.now, s, pal);
                    vec![
                        Cell::from(state).style(Style::default().fg(color)),
                        Cell::from(fit(&s.repo, w[1])),
                        Cell::from(fit(short_model(&s.model), w[2])),
                        num(s.messages.to_string()),
                        cost_cell(pal, s.cost_usd, peak),
                        num(age(data.now - s.last)),
                    ]
                })
                .collect()
        },
    );
}

/// Every session of the range, grouped by day (newest first): the day
/// on the first row of its group, then the short session id, project, main
/// model, messages, tokens and cost of that day.
pub fn sessions(frame: &mut Frame, area: Rect, app: &App, data: &Data, pal: &Palette) {
    let focused = app.focus == Panel::Sessions;
    let block = panel_block(
        pal,
        Panel::Sessions,
        "Sessions per day",
        pal.box_proc,
        focused,
    )
    .title(note_line(pal, &crate::tz::short(data.now), pal.box_proc).right_aligned());
    if data.sessions.is_empty() {
        return empty(frame, area, pal, block, "No sessions in this range.");
    }
    let long = area.width.saturating_sub(2) >= 56;
    let cols = [
        col("day", Constraint::Length(if long { 9 } else { 5 }), 0),
        col("session", Constraint::Length(8), 0),
        col("project", Constraint::Fill(1), 0),
        col("model", Constraint::Fill(1), 0),
        col(">msgs", Constraint::Length(5), 64),
        col(">tokens", Constraint::Length(7), 64),
        col(">cost", Constraint::Length(8), 0),
    ];
    let rows = session_day_rows(&data.sessions, app.sort(Panel::Sessions));
    let peak = rows.iter().map(|s| s.cost_usd).fold(0.0, f64::max);
    let today = crate::tz::day_of(data.now);
    table(
        frame,
        area,
        app,
        pal,
        Panel::Sessions,
        app.focus == Panel::Sessions,
        block,
        &cols,
        true,
        |w| {
            rows.iter()
                .enumerate()
                .map(|(i, s)| {
                    let first_of_day = i == 0 || rows[i - 1].day != s.day;
                    let day = if !first_of_day {
                        Cell::from("")
                    } else {
                        let date = date_key(s.day);
                        let text = if long {
                            format!("{} {}", WEEKDAYS[weekday(s.day)], &date[5..])
                        } else {
                            date[5..].to_string()
                        };
                        let style = if s.day == today {
                            today_style(pal)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        };
                        Cell::from(text).style(style)
                    };
                    vec![
                        day,
                        Cell::from(short_id(&s.id)),
                        Cell::from(fit(&s.repo, w[2])),
                        Cell::from(fit(short_model(&s.model), w[3])),
                        num(s.messages.to_string()),
                        num(compact(s.tokens)),
                        cost_cell(pal, s.cost_usd, peak),
                    ]
                })
                .collect()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_fills_in_proportion_and_clamps() {
        let pal = super::super::theme::Theme::Mono.palette(Default::default());
        let text = |f: f64, w: usize| {
            let cell = meter(&pal, pal.cpu, f, w);
            format!("{cell:?}")
        };
        assert_eq!(text(0.5, 4).matches(METER).count(), 2);
        assert_eq!(text(2.0, 2).matches(METER).count(), 2);
        assert_eq!(text(-1.0, 2).matches(METER_EMPTY).count(), 2);
    }

    #[test]
    fn short_model_drops_the_vendor_prefix_only() {
        assert_eq!(short_model("claude-opus-5"), "opus-5");
        assert_eq!(short_model("gpt-5-codex"), "gpt-5-codex");
    }

    #[test]
    fn age_picks_the_largest_unit() {
        assert_eq!(age(45), "45s");
        assert_eq!(age(7 * 60), "7m");
        assert_eq!(age(2 * 3600 + 5), "2h");
        assert_eq!(age(-3), "0s");
    }
}
