// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Draws the app into a ratatui frame. Pure over `App`, so a `TestBackend`
//! renders it to a buffer for snapshot tests. This file is the layout, the
//! header and the footer; `panels` draws each panel.

use super::app::App;
use super::budget;
use super::burnup;
use super::data::Data;
use super::fmt::{compact, fit, money, signed_compact, signed_count, signed_money, signed_percent};
use super::keys::{self, HelpLine};
use super::layout::{auto_rows, PanelId};
use super::notice::{self, Notice};
use super::panels;
use super::theme::Palette;
use crate::aggregate::date_key;
use crate::period::{elapsed_text, Comparison, Span as TimeSpan};
use crate::query::{clock, day_of, Range};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;

/// Smallest screen the layout fits.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

/// Where each panel goes.
#[derive(Debug, PartialEq, Eq)]
pub struct Panels {
    pub header: Rect,
    /// Empty on a short screen.
    pub budget: Rect,
    pub spend: Rect,
    pub tokens: Rect,
    pub models: Rect,
    pub projects: Rect,
    pub live: Rect,
    pub sessions: Rect,
    pub footer: Rect,
}

/// The header holds the comparison: a context line and one row each for cost,
/// tokens and messages, between two borders.
const HEADER_HEIGHT: u16 = 6;

/// The screen height from which the Budget panel is drawn.
const BUDGET_MIN_ROWS: u16 = 36;

/// Header and footer are fixed. The rest is three rows: graphs, breakdowns,
/// live and sessions, each split in two columns.
pub fn layout(area: Rect) -> Panels {
    // The budget panel needs room: it is left out below 36 rows, which keeps
    // the common 24 and 30 row screens as they were.
    let budget_height = if area.height >= BUDGET_MIN_ROWS {
        budget::HEIGHT
    } else {
        0
    };
    let [header, budget, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(budget_height),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [graphs, tables, live] = Layout::vertical([
        Constraint::Fill(40),
        Constraint::Fill(26),
        Constraint::Fill(34),
    ])
    .areas(body);
    let halves = |row: Rect| {
        let [a, b] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(row);
        (a, b)
    };
    let (spend, tokens) = halves(graphs);
    let (models, projects) = halves(tables);
    let (live, sessions) = halves(live);
    Panels {
        header,
        budget,
        spend,
        tokens,
        models,
        projects,
        live,
        sessions,
        footer,
    }
}

/// Where the parts of one tab go.
#[derive(Debug, PartialEq, Eq)]
pub struct Placed {
    pub header: Rect,
    /// Empty when the layout has one tab.
    pub tabbar: Rect,
    /// Empty when the tab has no budget panel or the screen is short.
    pub budget: Rect,
    /// Each panel of the tab with its place, budget excluded.
    pub panels: Vec<(PanelId, Rect)>,
    /// What a short body left out, `None` when every panel has its place.
    pub hidden: Option<Hidden>,
    pub footer: Rect,
}

/// The panels of a tab a body too short for its rows does not draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hidden {
    pub count: usize,
    /// The screen height at which every panel of the grid fits.
    pub needs_height: u16,
}

/// The screen height of a tabbed layout from which the budget strip is drawn.
/// A tab can be all about the budget, so this is lower than the classic one.
const TAB_BUDGET_MIN_ROWS: u16 = 24;

/// Splits `body` into bands, each a `(columns, weight)` pair: one wide panel
/// or two side by side.
fn tile(body: Rect, rows: &[(usize, u16)]) -> Vec<Rect> {
    let bands = Layout::vertical(rows.iter().map(|(_, w)| Constraint::Fill(*w))).split(body);
    let mut out = Vec::new();
    for ((cols, _), band) in rows.iter().zip(bands.iter()) {
        if *cols == 1 {
            out.push(*band);
        } else {
            let halves =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(*band);
            out.extend(halves.iter().copied());
        }
    }
    out
}

/// The row heights of five and six panels, which land where the classic screen
/// puts them.
const WEIGHTS_FIVE_SIX: [u16; 3] = [40, 26, 34];

/// Splits `body` for `n` panels in the automatic grid of `auto_rows`: one panel
/// fills the body, two stack, three make a pair over a wide panel, four make
/// two pairs, and five to eight make rows of two with the last row wide when
/// the count is odd. Two stack at every width because the panels that default
/// to two (Live and Sessions) are tables that want the whole width, and a
/// 150x44 screen still gives each of them 150x15 cells. Rows are never shorter
/// than `MIN_ROW_HEIGHT`, so a short body keeps only the rows that fit and the
/// panels of the others stay out; the count of those is the second value.
fn grid(body: Rect, n: usize) -> (Vec<Rect>, usize) {
    let full = auto_rows(n);
    let fit = usize::from(body.height / MIN_ROW_HEIGHT).max(1);
    let rows = &full[..full.len().min(fit)];
    let weighted = rows.len() == full.len() && matches!(n, 5 | 6);
    let bands: Vec<(usize, u16)> = rows
        .iter()
        .enumerate()
        .map(|(i, cols)| {
            let weight = if weighted { WEIGHTS_FIVE_SIX[i] } else { 1 };
            (usize::from(*cols), weight)
        })
        .collect();
    let shown: usize = rows.iter().map(|c| usize::from(*c)).sum();
    (tile(body, &bands), n - shown)
}

/// The least body height per row for an arranged tab to be drawn as arranged.
const MIN_ROW_HEIGHT: u16 = 5;

/// The panel count of each row once the budget strip and the retired
/// projection word, which have no cell in the body, are taken out. A row that held only the strip disappears.
fn body_rows(ids: &[PanelId], rows: &[u8]) -> Vec<usize> {
    let mut ids = ids.iter();
    rows.iter()
        .map(|n| {
            ids.by_ref()
                .take(usize::from(*n))
                .filter(|id| id.has_cell())
                .count()
        })
        .filter(|n| *n > 0)
        .collect()
}

/// Places the header, the optional tab bar, the budget strip, the panels of
/// a tab and the footer on `area`. A layout with one tab keeps the classic
/// screen: no tab bar, and the budget strip from 36 rows as before. `rows` is
/// the size of each row of an arranged tab; `None`, or a body too short for
/// the rows, gives the automatic grid, which leaves out the rows that do not
/// fit and reports them in `hidden`.
pub fn place(area: Rect, ids: &[PanelId], rows: Option<&[u8]>, tabbed: bool) -> Placed {
    let wants_budget = ids.contains(&PanelId::Budget);
    // A tab with few other panels can afford the strip on a short screen.
    let few = ids.iter().filter(|id| id.has_cell()).count() <= 3;
    let min_rows = if tabbed && few {
        TAB_BUDGET_MIN_ROWS
    } else {
        BUDGET_MIN_ROWS
    };
    let budget_height = if wants_budget && area.height >= min_rows {
        budget::HEIGHT
    } else {
        0
    };
    let [header, tabbar, budget, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(u16::from(tabbed)),
        Constraint::Length(budget_height),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let shown: Vec<PanelId> = ids.iter().copied().filter(|id| id.has_cell()).collect();
    let arranged = rows.map(|r| body_rows(ids, r)).filter(|r| {
        !r.is_empty()
            && r.iter().sum::<usize>() == shown.len()
            && body.height >= MIN_ROW_HEIGHT * r.len() as u16
    });
    let (rects, left_out) = match arranged {
        Some(r) => (
            tile(body, &r.iter().map(|n| (*n, 1)).collect::<Vec<_>>()),
            0,
        ),
        None => grid(body, shown.len()),
    };
    let rows_needed = u16::try_from(auto_rows(shown.len()).len()).unwrap_or(u16::MAX);
    let hidden = (left_out > 0).then(|| Hidden {
        count: left_out,
        needs_height: area
            .height
            .saturating_sub(body.height)
            .saturating_add(rows_needed.saturating_mul(MIN_ROW_HEIGHT)),
    });
    let panels = shown.iter().copied().zip(rects).collect();
    Placed {
        header,
        tabbar,
        budget,
        panels,
        hidden,
        footer,
    }
}

/// The place of one panel of a tab on `area`, `None` when the tab lacks it.
pub fn panel_rect(
    area: Rect,
    ids: &[PanelId],
    rows: Option<&[u8]>,
    tabbed: bool,
    id: PanelId,
) -> Option<Rect> {
    place(area, ids, rows, tabbed)
        .panels
        .into_iter()
        .find(|(p, _)| *p == id)
        .map(|(_, r)| r)
}

/// Table rows that fit in a panel: its height less two borders and the header.
pub fn visible_rows(area: Rect) -> usize {
    usize::from(area.height.saturating_sub(3))
}

/// The first row drawn so the selected one is the last one in view at most.
pub fn window_start(selected: usize, visible: usize) -> usize {
    (selected + 1).saturating_sub(visible)
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let pal = app.theme.palette(app.depth);
    // The ground first: widgets below only set what they color.
    let base = Style::default().fg(pal.color(pal.fg)).bg(pal.color(pal.bg));
    frame.buffer_mut().set_style(area, base);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let msg = format!(
            "Terminal too small: need {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}",
            area.width, area.height
        );
        frame.render_widget(Paragraph::new(msg), area);
        return;
    }
    let tabbed = app.layout.tabs.len() > 1;
    let p = place(area, app.tab_ids(), app.tab_rows(), tabbed);
    render_header(frame, p.header, app, &pal);
    if tabbed {
        render_tabbar(frame, p.tabbar, app, &pal);
    }
    let top = p.tabbar.bottom();
    let body = Rect::new(0, top, area.width, p.footer.y.saturating_sub(top));
    let dots = burnup::braille_ok(app.icon, pal.mono, app.dots_off);
    match (&app.data, notice::body(app)) {
        (Some(data), None) => {
            if p.budget.height > 0 {
                budget::render(frame, p.budget, data, &pal, app.icon);
            }
            for (id, rect) in &p.panels {
                match id {
                    PanelId::Spend
                    | PanelId::SpendTotal
                    | PanelId::SpendModel
                    | PanelId::SpendDaily => panels::spend(frame, *rect, app, data, &pal, *id),
                    PanelId::Tokens => panels::tokens(frame, *rect, app, data, &pal),
                    PanelId::Models => panels::models(frame, *rect, app, data, &pal),
                    PanelId::Breakdown
                    | PanelId::BreakdownProjects
                    | PanelId::BreakdownAgents
                    | PanelId::BreakdownSkills
                    | PanelId::BreakdownTools
                    | PanelId::BreakdownHealth => {
                        panels::projects(frame, *rect, app, data, &pal, *id)
                    }
                    PanelId::Live => panels::live(frame, *rect, app, data, &pal),
                    PanelId::Sessions => panels::sessions(frame, *rect, app, data, &pal),
                    PanelId::Hourly => panels::hourly(frame, *rect, data, &pal),
                    PanelId::Burnup => burnup::render(frame, *rect, data, &pal, dots),
                    PanelId::Budget | PanelId::Projection => {}
                }
            }
        }
        (data, Some(n)) => {
            if let Some(data) = data {
                if p.budget.height > 0 {
                    budget::render(frame, p.budget, data, &pal, app.icon);
                }
            }
            render_notice(frame, body, &n, &pal);
        }
        (None, None) => {}
    }
    if let (Some(_), Some(n)) = (&app.data, notice::banner(app)) {
        let height = (n.lines.len() as u16 + 2).min(body.height);
        render_notice(
            frame,
            Rect::new(body.x, body.y, body.width, height),
            &n,
            &pal,
        );
    }
    frame.render_widget(
        Paragraph::new(footer(app, &pal, area.width, p.hidden)),
        p.footer,
    );
    if let Some(scroll) = app.help {
        render_help(frame, area, scroll, &pal);
    }
    if let Some(popup) = &app.budget_popup {
        super::budget_popup::render(frame, area, popup, &pal);
    }
    if app.reset_popup {
        super::reset_popup::render(frame, area, &pal);
    }
    if let Some(editor) = &app.editor {
        super::editor_view::render(frame, area, editor, &pal, app.icon);
    }
}

/// Height of the help window on a screen `rows` tall: all its lines, or what
/// the screen leaves.
pub fn help_height(rows: u16) -> u16 {
    (keys::help_lines().len() as u16 + 2).min(rows.saturating_sub(2))
}

/// The help window, centered, over the panels. It scrolls when the screen is
/// shorter than the text.
fn render_help(frame: &mut Frame, area: Rect, scroll: usize, pal: &Palette) {
    let width = area.width.saturating_sub(2).min(78);
    let height = help_height(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let accent = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD);
    let plain = Style::default().fg(pal.color(pal.fg));
    let lines: Vec<Line> = keys::help_lines()
        .into_iter()
        .map(|l| match l {
            HelpLine::Blank => Line::raw(""),
            HelpLine::Heading(h) => Line::styled(h, accent),
            HelpLine::Key {
                shown,
                text,
                panels,
            } => Line::from(vec![
                Span::styled(format!("{shown:<10} "), accent),
                Span::raw(format!("{text:<41}")),
                Span::styled(panels, plain),
            ]),
        })
        .collect();
    let page = usize::from(height.saturating_sub(2));
    let top = scroll.min(lines.len().saturating_sub(page));
    let edge = Style::default().fg(pal.color(pal.hi));
    let more = if lines.len() > page {
        "\u{2191}/\u{2193} j/k scroll, any other key closes"
    } else {
        "any key closes"
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::styled("\u{2524} Help: keys by group \u{251c}", edge))
        .title_bottom(Line::styled(format!("\u{2524} {more} \u{251c}"), edge));
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).scroll((top as u16, 0)).block(block),
        rect,
    );
}

/// The tab bar: each tab with its number, the selected one in the accent.
fn render_tabbar(frame: &mut Frame, area: Rect, app: &App, pal: &Palette) {
    let plain = Style::default().fg(pal.color(pal.fg));
    let chosen = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let full: usize = app
        .layout
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| format!(" F{} {} ", i + 1, t.name).chars().count())
        .sum();
    // When the names do not fit, only the selected tab keeps its name, cut to
    // the width the numbered tabs leave over.
    let names = full <= usize::from(area.width);
    let numbered: usize = (0..app.layout.tabs.len())
        .filter(|&i| i != app.tab)
        .map(|i| format!(" F{} ", i + 1).chars().count())
        .sum();
    let mut spans = Vec::new();
    for (i, tab) in app.layout.tabs.iter().enumerate() {
        let style = if i == app.tab { chosen } else { plain };
        let text = if names {
            format!(" F{} {} ", i + 1, tab.name)
        } else if i == app.tab {
            let room = usize::from(area.width).saturating_sub(numbered);
            let whole = format!(" F{} {} ", i + 1, tab.name);
            whole.chars().take(room).collect()
        } else {
            format!(" F{} ", i + 1)
        };
        spans.push(Span::styled(text, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draws a message in a bordered box that clears what is under it.
fn render_notice(frame: &mut Frame, area: Rect, n: &Notice, pal: &Palette) {
    let ink = if n.error { pal.error } else { pal.hi };
    let edge = Style::default().fg(pal.color(ink));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::styled(format!("\u{2524} {} \u{251c}", n.title), edge));
    let lines: Vec<Line> = n.lines.iter().map(|l| Line::raw(l.clone())).collect();
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(block),
        area,
    );
}

/// The key hints: each key in the accent color, its meaning in the text color,
/// and at the right when the view was last refreshed.
fn footer(app: &App, pal: &Palette, area_width: u16, hidden: Option<Hidden>) -> Line<'static> {
    let plain = Style::default().fg(pal.color(pal.fg));
    if let Some(note) = &app.note {
        return Line::styled(format!(" {note}"), Style::default().fg(pal.color(pal.hi)));
    }
    if app.editing {
        return Line::styled(
            format!(" filter: {}_   enter apply   esc cancel", app.draft),
            plain,
        );
    }
    let keys = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD);
    let (status, status_style) = refresh_status(app, pal, area_width);
    // The status keeps its place at the right, so the hints get what is left.
    let alert = hidden.map(|h| {
        let noun = if h.count == 1 { "panel" } else { "panels" };
        format!(
            "{} {noun} hidden, {} rows show all ",
            h.count, h.needs_height
        )
    });
    let alert_width = alert.as_ref().map_or(0, |a| a.chars().count());
    let room = usize::from(area_width).saturating_sub(status.chars().count() + alert_width);
    let theme = format!("theme ({})", app.theme.name());
    // Shown in the order of the key table. When the line is too wide, hints
    // drop in the table's `drop` order, so the ones that matter most stay.
    let mut hints: Vec<(&str, String, u8)> = keys::footer_hints()
        .into_iter()
        .filter(|h| app.layout.tabs.len() > 1 || h.key != "Tab")
        .map(|h| {
            let verb = if h.verb == "theme" {
                theme.clone()
            } else {
                h.verb.to_string()
            };
            (h.key, verb, h.drop)
        })
        .collect();
    let width = |hints: &[(&str, String, u8)]| {
        1 + hints
            .iter()
            .map(|(k, l, _)| k.chars().count() + l.chars().count() + 2)
            .sum::<usize>()
    };
    for rank in 1..=u8::MAX {
        if width(&hints) <= room {
            break;
        }
        hints.retain(|(_, _, d)| *d != rank);
    }
    let used = width(&hints) + alert_width;
    let mut spans = vec![Span::raw(" ")];
    if let Some(alert) = alert {
        spans.push(Span::styled(alert, Style::default().fg(pal.color(pal.hi))));
    }
    for (key, label, _) in hints {
        spans.push(Span::styled(key.to_string(), keys));
        spans.push(Span::styled(format!(" {label} "), plain));
    }
    let gap = usize::from(area_width).saturating_sub(used + status.chars().count());
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(status, status_style));
    Line::from(spans)
}

/// What the right end of the footer says: when the data was last read, or why
/// the last read failed. The time is the snapshot's own clock, so it moves only
/// when a refresh really finished.
fn refresh_status(app: &App, pal: &Palette, area_width: u16) -> (String, Style) {
    if let Some(e) = &app.error {
        let room = usize::from(area_width / 2);
        let text = fit(&format!("refresh failed: {e} "), room);
        return (text, Style::default().fg(pal.color(pal.error)));
    }
    let plain = Style::default().fg(pal.color(pal.fg));
    if let Some((done, total)) = app.indexing {
        return (format!("indexing {done}/{total} files "), plain);
    }
    match &app.data {
        // The telemetry state and the interval go first when the line is short.
        Some(d) if area_width >= 130 => (
            format!(
                "{}  updated {} {}, every {}s ",
                app.telemetry.map_or("", |m| m.footer()),
                clock(d.now),
                crate::tz::describe(d.now),
                app.refresh_every.as_secs()
            ),
            plain,
        ),
        Some(d) if area_width >= 100 => (
            format!(
                "updated {} {}, every {}s ",
                clock(d.now),
                crate::tz::describe(d.now),
                app.refresh_every.as_secs()
            ),
            plain,
        ),
        Some(d) => (
            format!("updated {} {} ", clock(d.now), crate::tz::short(d.now)),
            plain,
        ),
        None => (String::new(), plain),
    }
}

/// `Aug 31`, the date of a day number without its year.
fn month_name_day(day: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let key = date_key(day);
    let month: usize = key[5..7].parse().unwrap_or(1);
    let dom: u32 = key[8..].parse().unwrap_or(1);
    format!("{} {dom}", MONTHS[month.clamp(1, 12) - 1])
}

fn range_label(data: &Data) -> String {
    match data.range.days(data.now) {
        Some((first, past)) if past - first == 1 => {
            format!(
                "{} {}, {}",
                data.range.label(),
                month_name_day(first),
                &date_key(first)[..4]
            )
        }
        Some((first, past)) => format!(
            "{} {} to {}, {}",
            data.range.label(),
            month_name_day(first),
            month_name_day(past - 1),
            &date_key(past - 1)[..4]
        ),
        None => "all time".to_string(),
    }
}

/// `Aug 31`, the date without its year.
fn month_day(timestamp: i64) -> String {
    month_name_day(day_of(timestamp))
}

/// `Aug 31 to Sep 3`, or just the date when the span is one day.
fn date_span(span: TimeSpan) -> String {
    let (first, last) = (month_day(span.start), month_day(span.end - 1));
    if first == last {
        first
    } else {
        format!("{first} to {last}")
    }
}

/// `12:00`, the time of day the span reaches.
fn reaches(span: TimeSpan) -> String {
    clock(span.end - 1)[..5].to_string()
}

/// The context line: the period so far and the one it is compared with, and
/// the elapsed span they share. Returns the current part and the rest, in the
/// longest form that fits `room`.
fn context(c: &Comparison, room: usize) -> (String, String) {
    let (w, p) = (c.windows, c.period);
    let span = elapsed_text(w.current.seconds());
    let forms = [
        (
            format!(
                "{} {} until {}",
                p.label(),
                date_span(w.current),
                reaches(w.current)
            ),
            format!(
                "  vs  {} {} until {}  (same span {span})",
                p.previous_label(),
                date_span(w.previous),
                reaches(w.previous)
            ),
        ),
        (
            format!("{} {}", p.label(), date_span(w.current)),
            format!(
                "  vs  {} {}  (same span {span})",
                p.previous_label(),
                date_span(w.previous)
            ),
        ),
        (
            p.label().to_string(),
            format!("  vs  {}  (same {span})", p.previous_label()),
        ),
    ];
    let fits = |(a, b): &(String, String)| a.chars().count() + b.chars().count() <= room;
    let last = forms[2].clone();
    forms.into_iter().find(fits).unwrap_or(last)
}

/// The context line of the header: the comparison when the range is a period,
/// else the range and its dates alone.
fn context_line(d: &Data, room: usize) -> (String, String) {
    match &d.comparison {
        Some(c) => context(c, room),
        None => (
            range_label(d),
            "  (no previous period to compare)".to_string(),
        ),
    }
}

/// One metric of the header: its name, its value, and when the range is a
/// period the value before and the change.
struct Metric {
    name: &'static str,
    now: String,
    before: Option<String>,
    amount: String,
    percent: String,
    /// The change itself, whose sign picks the color.
    change: f64,
}

impl Metric {
    fn alone(name: &'static str, now: String) -> Metric {
        Metric {
            name,
            now,
            before: None,
            amount: String::new(),
            percent: String::new(),
            change: 0.0,
        }
    }
}

fn metrics(d: &Data) -> [Metric; 3] {
    let Some(c) = &d.comparison else {
        return [
            Metric::alone("cost", money(d.totals.cost_usd)),
            Metric::alone("tokens", compact(d.totals.tokens())),
            Metric::alone("messages", d.totals.messages.to_string()),
        ];
    };
    let (cost, tokens, messages) = (c.cost(), c.tokens(), c.messages());
    [
        Metric {
            name: "cost",
            now: money(c.current.cost_usd),
            before: Some(money(c.previous.cost_usd)),
            amount: signed_money(cost.amount),
            percent: signed_percent(cost.percent, c.current.cost_usd > 0.0),
            change: cost.amount,
        },
        Metric {
            name: "tokens",
            now: compact(c.current.tokens()),
            before: Some(compact(c.previous.tokens())),
            amount: signed_compact(tokens.amount),
            percent: signed_percent(tokens.percent, c.current.tokens() > 0),
            change: tokens.amount,
        },
        Metric {
            name: "messages",
            now: c.current.messages.to_string(),
            before: Some(c.previous.messages.to_string()),
            amount: signed_count(messages.amount),
            percent: signed_percent(messages.percent, c.current.messages > 0),
            change: messages.amount,
        },
    ]
}

/// The header body: the context line, then cost, tokens and messages, with the
/// previous period's value and the change for a period.
fn header_lines(d: &Data, pal: &Palette, room: usize) -> Vec<Line<'static>> {
    let accent = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD);
    let plain = Style::default().fg(pal.color(pal.fg));
    let (now, rest) = context_line(d, room);
    let mut lines = vec![Line::from(vec![
        Span::styled(now, accent),
        Span::styled(rest, plain),
    ])];
    for m in metrics(d) {
        let tint = if m.change > 0.0 {
            pal.warn
        } else if m.change < 0.0 {
            pal.live
        } else {
            pal.fg
        };
        let mut spans = vec![
            Span::styled(format!("{:<9}", m.name), plain),
            Span::styled(
                format!("{:>11}", m.now),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(before) = &m.before {
            spans.push(Span::styled(format!("   vs{before:>11}   "), plain));
            spans.push(Span::styled(
                format!("{:>9} {:>8}", m.amount, m.percent),
                Style::default().fg(pal.color(tint)),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The time zone note in the header border. Weeks say which day they start on.
fn zone_note(app: &App) -> String {
    let zone = match app.range {
        Range::ThisWeek(w) | Range::LastWeek(w) => {
            format!("{}, weeks start {}", crate::tz::label(), w.short())
        }
        _ => crate::tz::label().to_string(),
    };
    let Some(data) = app.data.as_ref() else {
        return zone;
    };
    [&data.accounts.shown, &data.machines.shown]
        .into_iter()
        .filter(|name| !name.is_empty())
        .fold(zone, |note, name| format!("{note}, {name}"))
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, pal: &Palette) {
    let edge = Style::default().fg(pal.color(pal.box_cpu));
    let note = |text: String, ink: Style| {
        Line::from(vec![
            Span::styled("\u{2524}", edge),
            Span::styled(text, ink),
            Span::styled("\u{251c}\u{2500}", edge),
        ])
        .right_aligned()
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::from(vec![
            Span::styled("\u{2500}\u{2524}", edge),
            Span::styled(app.icon.prefix(), edge),
            Span::styled(
                "Agent Budget Dashboard",
                Style::default()
                    .fg(pal.color(pal.title))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("\u{251c}", edge),
        ]))
        .title(Line::from(vec![
            Span::styled("\u{2500}\u{2524}", edge),
            Span::styled(zone_note(app), Style::default().fg(pal.color(pal.fg))),
            Span::styled("\u{251c}", edge),
        ]));
    if !app.filter.is_empty() {
        let ink = Style::default().fg(pal.color(pal.hi));
        block = block.title(note(format!("filter {}", app.filter), ink));
    }
    if let Some(d) = app.data.as_ref().filter(|d| d.unpriced > 0) {
        let ink = Style::default().fg(pal.color(pal.warn));
        block = block.title(note(format!("{} unpriced", d.unpriced), ink));
    }
    let room = usize::from(area.width.saturating_sub(2));
    let lines = match (&app.data, &app.error) {
        (Some(d), _) => header_lines(d, pal, room),
        (None, Some(e)) => vec![Line::styled(
            e.clone(),
            Style::default().fg(pal.color(pal.error)),
        )],
        (None, None) => vec![Line::raw("Reading usage...")],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{LiveView, Range};
    use crate::UsageEvent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const NOW: i64 = 1_788_400_000;

    fn screen(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with(events: &[UsageEvent]) -> App {
        let mut app = App::new(Range::All);
        app.data = Some(Data::build(
            events,
            LiveView::default(),
            NOW,
            Range::All,
            "",
        ));
        app
    }

    fn event(model: &str, repo: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{model}{repo}"),
            timestamp: NOW - 60,
            session_id: "s".into(),
            model: model.into(),
            repo: repo.into(),
            input_tokens: 1_500,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    fn yesterday(model: &str, repo: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("y{model}{repo}{cost}"),
            timestamp: NOW - 86_400 - 60,
            ..event(model, repo, cost)
        }
    }

    fn app_for(range: Range, events: &[UsageEvent]) -> App {
        let mut app = App::new(range);
        app.data = Some(Data::build(events, LiveView::default(), NOW, range, ""));
        app
    }

    #[test]
    fn the_header_and_both_tables_show_the_totals_and_rows() {
        let app = app_with(&[event("opus", "alpha", 12.5), event("sonnet", "beta", 1.25)]);
        let text = screen(&app, 100, 30);
        assert!(text.contains("$13.75"), "{text}");
        assert!(text.contains("messages") && text.contains("1.5K"), "{text}");
        assert!(text.contains("opus") && text.contains("alpha"), "{text}");
        assert!(
            text.contains("Models") && text.contains("Projects"),
            "{text}"
        );
    }

    #[test]
    fn a_range_that_is_not_a_period_has_no_comparison() {
        let text = screen(&app_with(&[event("opus", "a", 1.0)]), 100, 30);
        assert!(text.contains("(no previous period to compare)"), "{text}");
        assert!(!text.contains(" vs "), "{text}");
    }

    #[test]
    fn the_header_shows_both_periods_and_the_change_in_cost_tokens_and_messages() {
        let app = app_for(
            Range::Today,
            &[
                event("opus", "alpha", 12.5),
                event("sonnet", "beta", 1.25),
                yesterday("opus", "alpha", 10.0),
            ],
        );
        let text = screen(&app, 100, 30);
        for want in [
            "today",
            "yesterday",
            "same span",
            "$13.75",
            "$10.00",
            "+$3.75",
            "+37.5%",
            "+1",
            "UTC",
        ] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
        // Messages: 2 now against 1 before, tokens 3.0K against 1.5K.
        assert!(text.contains("+100%") && text.contains("+1.5K"), "{text}");
    }

    #[test]
    fn a_period_with_no_previous_spend_says_new_not_a_percent() {
        let text = screen(
            &app_for(Range::Today, &[event("opus", "alpha", 2.0)]),
            100,
            30,
        );
        assert!(text.contains("new"), "{text}");
        assert!(!text.contains("inf") && !text.contains("NaN"), "{text}");
    }

    #[test]
    fn a_fall_in_spend_reads_with_a_minus_sign() {
        let app = app_for(
            Range::Today,
            &[event("opus", "a", 1.0), yesterday("opus", "a", 4.0)],
        );
        let text = screen(&app, 100, 30);
        assert!(text.contains("-$3.00") && text.contains("-75.0%"), "{text}");
    }

    #[test]
    fn every_panel_draws_with_live_data() {
        let now_event = UsageEvent {
            timestamp: NOW - 20,
            ..event("claude-opus-5", "alpha", 0.5)
        };
        let live = crate::query::live_view(
            std::slice::from_ref(&now_event),
            std::slice::from_ref(&now_event),
            std::slice::from_ref(&now_event),
            NOW,
        );
        let mut app = App::new(Range::LastDays(30));
        app.data = Some(Data::build(
            std::slice::from_ref(&now_event),
            live,
            NOW,
            Range::LastDays(30),
            "",
        ));
        let text = screen(&app, 100, 30);
        for want in [
            "Spend per day",
            "Tokens per day",
            "Models",
            "Projects",
            "Live sessions",
            "Sessions per day",
            "live",
            "opus-5",
        ] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
    }

    #[test]
    fn the_models_table_compares_each_model_with_the_previous_period() {
        let app = app_for(
            Range::Today,
            &[
                event("claude-opus-5", "alpha", 12.5),
                yesterday("claude-opus-5", "alpha", 10.0),
                yesterday("claude-haiku-5", "alpha", 0.5),
            ],
        );
        let text = screen(&app, 100, 30);
        let rows: Vec<&str> = text
            .lines()
            .filter_map(|l| l.split('\u{2502}').nth(1))
            .filter(|r| r.contains("opus-5") || r.contains("haiku-5"))
            .collect();
        assert_eq!(rows.len(), 2, "{text}");
        // Opus: $12.50 now, $10.00 before, +$2.50 and +25.0%.
        let opus = rows.iter().find(|r| r.contains("opus-5")).unwrap();
        for want in ["$12.50", "$10.00", "+$2.50", "+25.0%"] {
            assert!(opus.contains(want), "{want}: {opus}");
        }
        // Haiku was used only yesterday: nothing now, down 100%.
        let haiku = rows.iter().find(|r| r.contains("haiku-5")).unwrap();
        for want in ["$0.00", "$0.50", "-$0.50", "-100%"] {
            assert!(haiku.contains(want), "{want}: {haiku}");
        }
        assert!(text.contains("vs yesterday"), "{text}");
    }

    #[test]
    fn a_narrow_screen_drops_the_share_bar_and_keeps_the_names() {
        let app = app_with(&[event("claude-opus-5", "alpha", 12.5)]);
        let wide = screen(&app, 120, 30);
        let narrow = screen(&app, 80, 24);
        assert!(
            wide.contains("share") && wide.contains('\u{25a0}'),
            "{wide}"
        );
        assert!(!narrow.contains("share"), "{narrow}");
        assert!(
            narrow.contains("opus-5") && narrow.contains("alpha"),
            "{narrow}"
        );
    }

    #[test]
    fn the_layout_tiles_the_screen_without_overlap() {
        let area = Rect::new(0, 0, 100, 30);
        let p = layout(area);
        let rects = [
            p.header, p.budget, p.spend, p.tokens, p.models, p.projects, p.live, p.sessions,
            p.footer,
        ];
        let cells: u32 = rects
            .iter()
            .map(|r| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(cells, 100 * 30);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_screen_below_the_minimum_says_so_instead_of_drawing() {
        let app = app_with(&[]);
        let text = screen(&app, 40, 8);
        assert!(text.contains("Terminal too small"), "{text}");
    }

    #[test]
    fn before_the_first_read_the_view_says_it_is_reading() {
        let app = App::new(Range::All);
        assert!(screen(&app, 80, 24).contains("Reading usage"));
    }

    #[test]
    fn an_empty_range_fills_the_body_with_the_dates_that_have_data() {
        let mut app = app_with(&[]);
        let d = app.data.as_mut().unwrap();
        d.store = crate::tui::data::StoreSpan {
            events: 3,
            first: Some(NOW - 90_000),
            last: Some(NOW - 80_000),
        };
        for (w, h) in [(80, 24), (100, 30), (160, 50)] {
            let text = screen(&app, w, h);
            assert!(text.contains("The store has 3 messages from"), "{text}");
            assert!(text.contains("Press a to show all time"), "{text}");
            assert!(!text.contains("No usage in this range.\n"), "{text}");
        }
    }

    #[test]
    fn an_empty_store_names_the_transcript_folder() {
        let mut app = app_with(&[]);
        app.sources = "/fake/home/.claude/projects".into();
        let text = screen(&app, 80, 24);
        assert!(text.contains("No usage recorded yet"), "{text}");
        assert!(text.contains("/fake/home/.claude/projects"), "{text}");
    }

    #[test]
    fn a_read_failure_shows_in_the_body_with_and_without_old_data() {
        let mut app = App::new(Range::Today);
        app.error = Some("database is locked".into());
        let text = screen(&app, 80, 24);
        assert!(text.contains("Could not read usage"), "{text}");
        assert!(text.matches("database is locked").count() >= 2, "{text}");
        let mut app = app_with(&[event("claude-opus-5", "alpha", 1.0)]);
        app.error = Some("disk I/O error".into());
        let text = screen(&app, 100, 30);
        assert!(text.contains("Refresh failed"), "{text}");
        assert!(text.contains("alpha"), "{text}");
    }

    #[test]
    fn the_spend_and_projects_titles_name_their_view_and_the_others_do_not() {
        let mut app = app_with(&[event("claude-opus-5", "alpha", 12.5)]);
        let text = screen(&app, 120, 30);
        assert!(text.contains("v view: total"), "{text}");
        assert!(text.contains("v view: projects"), "{text}");
        assert_eq!(text.matches("v view:").count(), 2, "{text}");
        app.key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('4'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        app.key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('v'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert!(screen(&app, 120, 30).contains("v view: agents"));
    }

    #[test]
    fn the_help_window_fits_every_supported_size_and_names_the_panels() {
        for (w, h) in [(80, 24), (100, 30), (160, 60)] {
            let mut app = app_with(&[event("claude-opus-5", "alpha", 1.0)]);
            app.help = Some(0);
            let text = screen(&app, w, h);
            assert!(text.contains("Navigation"), "{w}x{h}: {text}");
            if h >= 60 {
                // The whole table fits on a tall screen.
                for group in [
                    "Periods",
                    "Views",
                    "Sorting",
                    "Filtering",
                    "Refresh",
                    "Quit",
                ] {
                    assert!(text.contains(group), "{w}x{h}: {text}");
                }
                assert!(text.contains("Spend, Projects"), "{w}x{h}");
            }
            assert!(text.contains("closes"), "{w}x{h}");
            // Every line of the window is inside the screen.
            assert_eq!(text.lines().count(), usize::from(h));
        }
    }

    #[test]
    fn every_help_line_fits_the_narrowest_window() {
        for line in super::keys::help_lines() {
            if let super::keys::HelpLine::Key {
                shown,
                text,
                panels,
            } = line
            {
                let width = 11 + 41 + panels.chars().count();
                assert!(shown.chars().count() <= 10, "{shown}");
                assert!(text.chars().count() <= 40, "{text}");
                assert!(width <= 76, "{shown}: {width}");
            }
        }
    }

    #[test]
    fn the_footer_names_what_each_key_does_and_always_keeps_help_and_quit() {
        let app = App::new(Range::All);
        let wide = screen(&app, 140, 30);
        for hint in ["m month", "w week", "d day", "a all", "? help", "q quit"] {
            assert!(wide.contains(hint), "{hint}");
        }
        for width in [80, 90, 100] {
            let text = screen(&app, width, 24);
            assert!(text.contains("? help") && text.contains("q quit"), "{text}");
        }
    }

    #[test]
    fn the_footer_lists_the_keys_and_the_active_theme() {
        let app = App::new(Range::All);
        let text = screen(&app, 160, 30);
        for hint in [
            "/ filter",
            "s/S sort",
            "v view",
            "\u{2190}/\u{2192} day",
            "t theme (default)",
        ] {
            assert!(text.contains(hint), "{hint}");
        }
    }

    #[test]
    fn the_footer_shows_when_the_data_was_read_and_how_often() {
        let app = app_with(&[]);
        assert!(screen(&app, 150, 30).contains("updated 01:46:40 UTC, every 5s"));
        // A short line keeps the time and drops the interval.
        let short = screen(&app, 80, 24);
        assert!(short.contains("updated 01:46:40 UTC"), "{short}");
        assert!(!short.contains("every"), "{short}");
        // A configured interval is shown.
        let mut app = app;
        app.refresh_every = std::time::Duration::from_secs(30);
        assert!(screen(&app, 150, 30).contains("every 30s"));
    }

    #[test]
    fn the_footer_names_the_telemetry_state_on_a_wide_screen_only() {
        use crate::telemetry_status::Mode;
        let mut app = app_with(&[]);
        app.telemetry = Some(Mode::OffNotConfigured);
        assert!(screen(&app, 150, 30).contains("telemetry: off (not configured)"));
        assert!(!screen(&app, 80, 24).contains("telemetry:"));
        app.telemetry = Some(Mode::OnRunning);
        assert!(screen(&app, 150, 30).contains("telemetry: on, collector running"));
    }

    #[test]
    fn a_failed_refresh_is_named_in_the_footer_and_the_data_stays() {
        let mut app = app_with(&[event("opus", "alpha", 2.0)]);
        app.error = Some("database is busy".into());
        let text = screen(&app, 120, 30);
        assert!(text.contains("refresh failed: database is busy"), "{text}");
        assert!(text.contains("$2.00"), "the last good data stays:\n{text}");
        assert!(!text.contains("updated"), "{text}");
    }

    #[test]
    fn the_sessions_table_names_each_day_once_and_lists_its_sessions() {
        let session = |id: &str, ts: i64, cost: f64| UsageEvent {
            event_id: format!("{id}{ts}"),
            timestamp: ts,
            session_id: id.into(),
            model: "claude-opus-5".into(),
            repo: "alpha".into(),
            cost_usd: cost,
            ..UsageEvent::default()
        };
        // Today is Thursday 09-03, one day back is Wednesday 09-02.
        let events = [
            session("aaaaaaaa-1", NOW - 60, 1.0),
            session("bbbbbbbb-2", NOW - 50, 2.0),
            session("cccccccc-3", NOW - 86_400, 3.0),
        ];
        let live = LiveView::default();
        let mut app = App::new(Range::All);
        app.data = Some(Data::build(&events, live, NOW, Range::All, ""));
        let text = screen(&app, 160, 50);
        let ids = ["aaaaaaaa", "bbbbbbbb", "cccccccc"];
        let rows: Vec<&str> = text
            .lines()
            .filter_map(|l| l.split('\u{2502}').nth(3))
            .filter(|r| ids.iter().any(|id| r.contains(id)))
            .collect();
        assert_eq!(rows.len(), 3, "{text}");
        assert!(
            rows[0].contains("Thu 09-03") && rows[0].contains("bbbbbbbb"),
            "{rows:?}"
        );
        assert!(
            !rows[1].contains("09-03") && rows[1].contains("aaaaaaaa"),
            "{rows:?}"
        );
        assert!(
            rows[2].contains("Wed 09-02") && rows[2].contains("cccccccc"),
            "{rows:?}"
        );
    }

    #[test]
    fn the_footer_shows_the_draft_while_editing_and_the_filter_once_set() {
        let mut app = App::new(Range::All);
        app.editing = true;
        app.draft = "opus".into();
        assert!(screen(&app, 100, 30).contains("filter: opus_"));
        app.editing = false;
        app.filter = "opus".into();
        assert!(screen(&app, 100, 30).contains("filter opus"));
    }

    #[test]
    fn the_mono_theme_draws_no_foreground_color() {
        let mut app = app_with(&[event("claude-opus-4-1", "a/b", 1.0)]);
        app.theme = super::super::theme::Theme::Mono;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let colored = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|c| c.fg != ratatui::style::Color::Reset)
            .count();
        assert_eq!(colored, 0);
    }

    #[test]
    fn the_header_names_each_period_its_previous_one_and_their_exact_utc_dates() {
        use crate::query::WeekStart;
        // Thursday 2026-09-03 01:46:40 UTC, so 1h 46m into the day.
        for (range, want) in [
            (
                Range::Today,
                "today Sep 3 until 01:46  vs  yesterday Sep 2 until 01:46  (same span 1h 46m)",
            ),
            (
                Range::ThisWeek(WeekStart::Monday),
                "this week Aug 31 to Sep 3 until 01:46  vs  last week Aug 24 to Aug 27 until 01:46  (same span 3d 1h)",
            ),
            (
                Range::CurrentMonth,
                "this month Sep 1 to Sep 3 until 01:46  vs  last month Aug 1 to Aug 3 until 01:46  (same span 2d 1h)",
            ),
            // The other ranges have no previous period.
            (Range::LastWeek(WeekStart::Monday), "last week Aug 24 to Aug 30, 2026"),
            (Range::LastMonth, "last month Aug 1 to Aug 31, 2026"),
            (Range::LastDays(30), "last 30 days Aug 5 to Sep 3, 2026"),
            (Range::All, "all time"),
        ] {
            let text = screen(&app_for(range, &[]), 120, 30);
            assert!(text.contains(want), "{want}:\n{text}");
            assert!(text.contains("UTC"), "{text}");
        }
        // A week says which day the weeks start on, and moves with it.
        let sunday = Range::ThisWeek(WeekStart::Sunday);
        let text = screen(&app_for(sunday, &[]), 120, 30);
        assert!(text.contains("UTC, weeks start Sun"), "{text}");
        assert!(
            text.contains("this week Aug 30 to Sep 3")
                && text.contains("last week Aug 23 to Aug 27"),
            "{text}"
        );
    }

    #[test]
    fn the_context_line_gives_up_detail_before_the_same_span_note() {
        let c = Data::build(&[], LiveView::default(), NOW, Range::Today, "")
            .comparison
            .expect("today compares");
        let mut lengths = Vec::new();
        for room in [110, 80, 50, 40] {
            let (a, b) = context(&c, room);
            let len = a.chars().count() + b.chars().count();
            assert!(len <= room || room < 45, "{room}: {a}{b}");
            assert!(b.contains("same"), "{room}: {b}");
            lengths.push(len);
        }
        assert!(lengths.windows(2).all(|w| w[0] >= w[1]), "{lengths:?}");
    }

    #[test]
    fn the_footer_drops_the_least_important_hints_first_on_a_narrow_screen() {
        let app = App::new(Range::All);
        let wide = screen(&app, 160, 30);
        let narrow = screen(&app, 80, 24);
        assert!(wide.contains("t theme"), "{wide}");
        assert!(!narrow.contains("t theme"), "{narrow}");
        for kept in ["m month", "v view", "/ filter", "? help", "q quit"] {
            assert!(narrow.contains(kept), "{kept}:\n{narrow}");
        }
    }

    #[test]
    fn the_unpriced_count_shows_in_the_header_border() {
        let mut e = event("mystery", "alpha", 0.0);
        e.unpriced = true;
        let app = app_with(&[e]);
        assert!(screen(&app, 80, 24).contains("1 unpriced"));
        assert!(screen(&app, 120, 30).contains("1 unpriced"));
    }

    #[test]
    fn a_resize_redraws_to_the_new_size() {
        let app = App::new(Range::All);
        assert!(screen(&app, 60, 20).contains("Terminal too small"));
        assert!(!screen(&app, 100, 30).contains("Terminal too small"));
    }

    use super::super::layout::{Layout, ALL};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn tabbed_app() -> App {
        let mut app = app_with(&[event("opus", "alpha", 1.0)]);
        app.layout = Layout::default();
        app
    }

    #[test]
    fn the_classic_placement_matches_the_old_fixed_layout() {
        for (w, h) in [(80, 24), (100, 30), (120, 36), (160, 50)] {
            let area = Rect::new(0, 0, w, h);
            let old = layout(area);

            let classic = Layout::classic().tabs[0].panels.clone();
            let placed = place(area, &classic, None, false);

            let rect = |id| panel_rect(area, &classic, None, false, id).unwrap();
            assert_eq!(rect(PanelId::Spend), old.spend, "{w}x{h}");
            assert_eq!(rect(PanelId::Tokens), old.tokens, "{w}x{h}");
            assert_eq!(rect(PanelId::Models), old.models, "{w}x{h}");
            assert_eq!(rect(PanelId::Breakdown), old.projects, "{w}x{h}");
            assert_eq!(rect(PanelId::Live), old.live, "{w}x{h}");
            assert_eq!(rect(PanelId::Sessions), old.sessions, "{w}x{h}");
            assert_eq!(placed.budget, old.budget, "{w}x{h}");
            assert_eq!(placed.footer, old.footer, "{w}x{h}");
        }
    }

    #[test]
    fn a_tab_with_two_panels_stacks_them_and_fills_the_body() {
        let area = Rect::new(0, 0, 100, 30);

        let placed = place(area, &[PanelId::Spend, PanelId::Tokens], None, true);

        let (_, top) = placed.panels[0];
        let (_, bottom) = placed.panels[1];
        assert_eq!(top.width, 100);
        assert_eq!(top.bottom(), bottom.y);
        assert_eq!(bottom.bottom(), placed.footer.y);
        assert_eq!(placed.tabbar.height, 1);
    }

    #[test]
    fn the_editor_knows_the_rows_of_the_automatic_grid() {
        for n in 1..=9 {
            // Arrange
            let area = Rect::new(0, 0, 100, 60);

            // Act
            let (rects, _) = grid(area, n);
            let mut by_row: Vec<u8> = Vec::new();
            let mut last_y = None;
            for r in &rects {
                if last_y == Some(r.y) {
                    *by_row.last_mut().unwrap() += 1;
                } else {
                    by_row.push(1);
                    last_y = Some(r.y);
                }
            }

            // Assert
            assert_eq!(by_row, auto_rows(n), "{n} panels");
        }
    }

    fn assert_tiles_body(area: Rect, ids: &[PanelId], rows: Option<&[u8]>) -> Placed {
        let placed = place(area, ids, rows, true);
        let body_cells: u32 = placed
            .panels
            .iter()
            .map(|(_, r)| u32::from(r.width) * u32::from(r.height))
            .sum();
        let body_height = placed.footer.y - placed.budget.bottom();
        assert_eq!(body_cells, u32::from(body_height) * u32::from(area.width));
        for (i, (_, a)) in placed.panels.iter().enumerate() {
            for (_, b) in &placed.panels[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
            }
        }
        placed
    }

    #[test]
    fn explicit_rows_tile_the_body_with_a_wide_panel_over_a_pair() {
        // Arrange: the auto grid for 3 panels puts the pair on top instead.
        let area = Rect::new(0, 0, 100, 30);
        let ids = [PanelId::Spend, PanelId::Tokens, PanelId::Models];

        // Act
        let placed = assert_tiles_body(area, &ids, Some(&[1, 2]));

        // Assert
        let rect = |i: usize| placed.panels[i].1;
        assert_eq!(rect(0).width, 100);
        assert_eq!(rect(1).y, rect(0).bottom());
        assert_eq!(rect(1).y, rect(2).y);
        assert_eq!(rect(1).width + rect(2).width, 100);
        assert_ne!(placed.panels, place(area, &ids, None, true).panels);
    }

    #[test]
    fn explicit_rows_that_do_not_add_up_to_the_panels_use_the_auto_grid() {
        let area = Rect::new(0, 0, 100, 30);
        let ids = [PanelId::Spend, PanelId::Tokens, PanelId::Models];

        let short = place(area, &ids, Some(&[1, 1]), true);
        let auto = place(area, &ids, None, true);

        assert_eq!(short.panels, auto.panels);
    }

    #[test]
    fn explicit_rows_give_way_to_the_auto_grid_on_a_short_body() {
        // Arrange
        let area = Rect::new(0, 0, 100, 14);
        let ids = [PanelId::Spend, PanelId::Tokens, PanelId::Models];

        // Act
        let rows = place(area, &ids, Some(&[1, 1, 1]), true);
        let auto = place(area, &ids, None, true);
        let tall = place(Rect::new(0, 0, 100, 40), &ids, Some(&[1, 1, 1]), true);

        // Assert
        assert_eq!(rows.panels, auto.panels);
        assert!(tall.panels.iter().all(|(_, r)| r.width == 100));
    }

    #[test]
    fn a_budget_strip_in_a_row_leaves_the_rest_of_the_row_wide() {
        // Arrange
        let area = Rect::new(0, 0, 100, 40);
        let ids = [PanelId::Budget, PanelId::Spend, PanelId::Tokens];

        // Act
        let placed = assert_tiles_body(area, &ids, Some(&[2, 1]));

        // Assert
        assert_eq!(placed.panels[0].1.width, 100);
        assert_eq!(placed.panels[1].1.width, 100);
    }

    #[test]
    fn seven_and_eight_panels_all_get_a_place() {
        // Arrange
        let area = Rect::new(0, 0, 120, 40);
        let panels = eight_panels();

        for n in 7..=8 {
            // Act
            let ids = &panels[..n];
            let placed = assert_tiles_body(area, ids, None);

            // Assert
            assert_eq!(placed.panels.len(), n, "{n} panels");
        }
    }

    #[test]
    fn an_editor_refusal_starts_with_an_alert_mark() {
        // Arrange
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        for _ in 0..13 {
            app.key(press(KeyCode::Char('n')));
        }
        let text = screen(&app, 100, 30);

        // Assert
        assert!(text.contains("! At most 12 tabs"), "{text}");
    }

    #[test]
    fn the_tab_bar_names_every_tab_and_the_footer_hints_the_tab_key() {
        let app = tabbed_app();

        let text = screen(&app, 140, 30);

        assert!(text.contains("F1 Charts"), "{text}");
        assert!(text.contains("F3 Sessions"), "{text}");
        assert!(text.contains("Tab tab"), "{text}");
    }

    #[test]
    fn a_single_tab_layout_shows_no_tab_bar() {
        let app = app_with(&[event("opus", "alpha", 1.0)]);

        let text = screen(&app, 140, 30);

        assert!(!text.contains("F1 All"), "{text}");
    }

    #[test]
    fn tab_switches_between_tabs_and_wraps_and_function_keys_jump() {
        let mut app = tabbed_app();

        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let second = app.tab;
        app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        let wrapped = app.tab;
        app.key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));

        assert_eq!(second, 1);
        assert_eq!(wrapped, 2);
        assert_eq!(app.tab, 1);
    }

    #[test]
    fn f10_to_f12_select_the_last_tabs_and_a_missing_tab_changes_nothing() {
        let mut app = tabbed_app();
        app.layout = Layout::parse(&twelve_tabs()).unwrap();

        app.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        let twelfth = app.tab;
        app.key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
        let tenth = app.tab;
        app.layout = Layout::parse("A=spend;B=tokens;C=live;D=models;E=sessions").unwrap();
        app.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));

        assert_eq!((twelfth, tenth, app.tab), (11, 9, 9));
    }

    #[test]
    fn a_long_selected_tab_name_is_cut_so_every_tab_number_stays_visible() {
        let mut app = tabbed_app();
        let text = (1..=12)
            .map(|i| format!("{}{i:02}=spend", "N".repeat(14)))
            .collect::<Vec<_>>()
            .join(";");
        app.layout = Layout::parse(&text).unwrap();
        app.go_tab(5);

        // The other eleven tabs need 8 * 4 + 3 * 5 = 47 columns, so 60 leaves
        // 13 for the selected tab, which is shorter than its 21 column label.
        // The 80 column minimum never reaches this cut, so draw the bar alone.
        let pal = app.theme.palette(app.depth);
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|f| render_tabbar(f, f.area(), &app, &pal))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let bar: String = (0..60).map(|x| buffer[(x, 0)].symbol()).collect();

        assert!(bar.contains(" F12"), "{bar}");
        assert!(bar.contains(" F6 NNNNNNNNN"), "{bar}");
        assert!(!bar.contains("NNNNNNNNNNNNNN06"), "{bar}");
    }

    fn twelve_tabs() -> String {
        (1..=12)
            .map(|i| format!("T{i}=spend"))
            .collect::<Vec<_>>()
            .join(";")
    }

    #[test]
    fn the_focus_moves_to_a_panel_the_new_tab_shows() {
        let mut app = tabbed_app();
        app.focus = crate::tui::sort::Panel::Spend;

        app.key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));

        assert_eq!(app.focus, crate::tui::sort::Panel::Live);
    }

    #[test]
    fn a_number_key_ignores_a_panel_the_tab_does_not_show() {
        let mut app = tabbed_app();
        app.go_tab(0);

        app.key(KeyEvent::new(KeyCode::Char('6'), KeyModifiers::NONE));

        assert_eq!(app.focus, crate::tui::sort::Panel::Spend);
    }

    #[test]
    fn the_numbers_tab_shows_the_budget_strip_on_a_24_row_screen() {
        let mut app = tabbed_app();
        app.tab = 1;

        let text = screen(&app, 100, 24);

        assert!(text.contains("Budget"), "{text}");
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn after_a_layout_is_loaded_the_focus_sits_on_a_panel_of_the_shown_tab() {
        let mut app = App::new(Range::All);
        app.layout = Layout::default();

        app.apply_ui_state(&crate::tui::ui_state::UiState::default());

        assert_eq!(app.focus, crate::tui::sort::Panel::Spend);
    }

    #[test]
    fn a_function_key_past_the_last_tab_does_nothing() {
        let mut app = tabbed_app();
        app.go_tab(1);

        app.key(press(KeyCode::F(4)));

        assert_eq!(app.tab, 1);
    }

    #[test]
    fn with_one_tab_the_tab_key_moves_the_panel_focus_as_before() {
        let mut app = App::new(Range::All);
        let before = app.focus;

        app.key(press(KeyCode::Tab));

        assert_ne!(app.focus, before);
        assert_eq!(app.tab, 0);
    }

    #[test]
    fn the_bracket_key_wraps_inside_the_panels_of_a_tab() {
        let mut app = tabbed_app();
        app.go_tab(1);
        app.focus = crate::tui::sort::Panel::Projects;

        app.key(press(KeyCode::Char(']')));

        assert_eq!(app.focus, crate::tui::sort::Panel::Models);
    }

    #[test]
    fn from_a_hidden_focus_the_bracket_key_lands_on_the_second_slot() {
        let mut app = tabbed_app();
        app.focus = crate::tui::sort::Panel::Sessions;

        app.key(press(KeyCode::Char(']')));

        assert_eq!(app.focus_slot(), Some(PanelId::SpendModel));
    }

    #[test]
    fn a_tab_with_only_the_budget_leaves_the_row_keys_inert() {
        let mut app = app_with(&[event("opus", "alpha", 1.0)]);
        app.layout = Layout::parse("Money=budget;Other=models").unwrap();
        let before = app.sorts;

        let effect = app.key(press(KeyCode::Char('s')));

        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.sorts, before);
    }

    #[test]
    fn every_panel_count_fills_the_tab_body_without_overlap() {
        let area = Rect::new(0, 0, 100, 30);
        for n in 1..=6 {
            let ids: Vec<PanelId> = ALL.iter().copied().take(n).collect();

            let placed = place(area, &ids, None, true);

            assert_eq!(placed.panels.len(), n, "{n} panels");
            for (i, (_, a)) in placed.panels.iter().enumerate() {
                assert!(
                    a.y >= placed.tabbar.bottom() && a.bottom() <= placed.footer.y,
                    "{n}"
                );
                for (_, b) in placed.panels.iter().skip(i + 1) {
                    assert!(!a.intersects(*b), "{n} panels overlap");
                }
            }
        }
    }

    #[test]
    fn a_wide_tab_bar_on_a_narrow_screen_keeps_the_selected_name() {
        let names: Vec<String> = (0..9).map(|i| format!("Tab-number-{i:04}")).collect();
        let text = names
            .iter()
            .map(|n| format!("{n}=spend"))
            .collect::<Vec<_>>()
            .join(";");
        let mut app = app_with(&[event("opus", "alpha", 1.0)]);
        app.layout = Layout::parse(&text).unwrap();
        app.go_tab(8);

        let shown = screen(&app, 80, 24);

        assert!(shown.contains("Tab-number-0008"), "{shown}");
    }

    #[test]
    fn the_cursor_keys_do_nothing_on_a_tab_without_charts() {
        let mut app = tabbed_app();
        app.go_tab(1);

        app.key(press(KeyCode::Left));
        app.key(press(KeyCode::Right));

        assert_eq!(app.cursor, None);
    }

    #[test]
    fn e_opens_the_layout_editor_and_escape_closes_it_with_the_layout_as_it_was() {
        // Arrange
        let mut app = tabbed_app();

        // Act
        app.key(press(KeyCode::Char('e')));
        let open = app.editor.is_some();
        let effect = app.key(press(KeyCode::Esc));

        // Assert
        assert!(open);
        assert!(app.editor.is_none());
        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.layout, Layout::default());
    }

    #[test]
    fn a_change_in_the_editor_shows_behind_it_at_once_and_asks_to_store_it() {
        // Arrange
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        let effect = app.key(press(KeyCode::Char('x')));

        // Assert
        assert_eq!(
            effect,
            crate::tui::app::Effect::SaveLayout(
                "Numbers=models,breakdown,budget;Sessions=live,sessions".into()
            )
        );
        assert_eq!(app.layout.tabs.len(), 2);
        assert_eq!(app.tab, 0);
        assert!(app.editor.is_some());
    }

    #[test]
    fn escape_after_a_change_keeps_it_and_stores_nothing_more() {
        // Arrange
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));
        app.key(press(KeyCode::Char('x')));

        // Act
        let effect = app.key(press(KeyCode::Esc));

        // Assert
        assert!(app.editor.is_none());
        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.layout.tabs.len(), 2);
    }

    #[test]
    fn a_layout_the_text_form_cannot_hold_is_refused_with_a_message() {
        // Arrange
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));
        if let Some(editor) = app.editor.as_mut() {
            editor.layout.tabs.clear();
        }

        // Act
        let effect = app.key(press(KeyCode::Char('l')));

        // Assert
        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.layout, Layout::default());
        assert!(app.editor.is_some_and(|e| e.message.is_some()));
    }

    #[test]
    fn a_key_that_changes_nothing_in_the_editor_stores_nothing() {
        // Arrange
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        let moved = app.key(press(KeyCode::Char('l')));
        let refused = app.key(press(KeyCode::Char('L')));

        // Assert
        assert_eq!(moved, crate::tui::app::Effect::None);
        assert_eq!(refused, crate::tui::app::Effect::None);
    }

    #[test]
    fn the_old_save_keys_do_nothing_in_the_editor() {
        for key in ['w', 's'] {
            // Arrange
            let mut app = tabbed_app();
            app.key(press(KeyCode::Char('e')));

            // Act
            let effect = app.key(press(KeyCode::Char(key)));

            // Assert
            assert_eq!(effect, crate::tui::app::Effect::None, "{key}");
            assert!(app.editor.is_some(), "{key}");
        }
    }

    fn customised_app() -> App {
        let mut app = app_with(&[event("opus", "alpha", 1.0)]);
        app.layout = Layout::parse("One=spend;Two=tokens").unwrap();
        app.go_tab(1);
        app.range = Range::All;
        app.filter = "alpha".into();
        app.spend_view = crate::tui::app::SpendView::Daily;
        app.projects_view = crate::tui::app::ProjectsView::Agents;
        app.sorts[0] = crate::tui::sort::Sort {
            col: 2,
            desc: false,
        };
        app.selected[2] = 3;
        app
    }

    #[test]
    fn r_asks_before_resetting_and_n_changes_nothing() {
        // Arrange
        let mut app = customised_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        let asked = app.key(press(KeyCode::Char('R')));
        let prompt = screen(&app, 100, 30);
        let answer = app.key(press(KeyCode::Char('n')));

        // Assert
        assert_eq!(asked, crate::tui::app::Effect::None);
        assert!(prompt.contains("Reset layout and view settings to the defaults? [y/N]"));
        assert_eq!(answer, crate::tui::app::Effect::None);
        assert!(app.editor.is_some());
        assert_eq!(app.filter, "alpha");
    }

    #[test]
    fn y_asks_the_loop_to_clear_the_store_and_changes_nothing_in_memory_yet() {
        // Arrange
        let mut app = customised_app();
        app.key(press(KeyCode::Char('e')));
        app.key(press(KeyCode::Char('R')));

        // Act
        let effect = app.key(press(KeyCode::Char('y')));

        // Assert
        assert_eq!(effect, crate::tui::app::Effect::ResetPrefs);
        assert_eq!(app.filter, "alpha");
        assert_eq!(app.layout.tabs.len(), 2);
    }

    #[test]
    fn a_cleared_store_puts_layout_tab_range_views_sorts_and_filter_back() {
        // Arrange
        let mut app = customised_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        let effect = app.reset_done(Ok(()));

        // Assert
        assert_eq!(effect, crate::tui::app::Effect::Requery);
        assert_eq!(app.layout, Layout::default());
        assert_eq!(app.tab, 0);
        assert_eq!(app.range, Range::ThisWeek(app.week_start));
        assert!(app.filter.is_empty());
        assert_eq!(app.spend_view, crate::tui::app::SpendView::default());
        assert_eq!(app.projects_view, crate::tui::app::ProjectsView::default());
        assert_eq!(
            app.sorts,
            crate::tui::sort::PANELS.map(crate::tui::sort::Panel::default_sort)
        );
        assert_eq!(app.selected, [0; 6]);
        assert!(app.editor.is_none());
    }

    #[test]
    fn a_failed_clear_changes_nothing_and_says_so_in_the_footer() {
        // Arrange
        let mut app = customised_app();
        app.key(press(KeyCode::Char('e')));

        // Act
        let effect = app.reset_done(Err("database is locked, nothing changed".into()));

        // Assert
        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.layout.tabs.len(), 2);
        assert_eq!(app.tab, 1);
        assert_eq!(app.filter, "alpha");
        assert!(app.editor.is_some());
        let footer = screen(&app, 140, 30);
        assert!(
            footer.contains("Reset failed: database is locked, nothing changed"),
            "{footer}"
        );
    }

    #[test]
    fn a_reset_asks_for_a_requery_when_only_the_filter_or_only_the_range_changed() {
        for (range, filter) in [
            (Range::ThisWeek(crate::query::WeekStart::Monday), "alpha"),
            (Range::All, ""),
        ] {
            // Arrange
            let mut app = tabbed_app();
            app.week_start = crate::query::WeekStart::Monday;
            app.range = range;
            app.filter = filter.into();

            // Act
            let effect = app.reset_done(Ok(()));

            // Assert
            assert_eq!(
                effect,
                crate::tui::app::Effect::Requery,
                "{range:?} {filter}"
            );
        }
    }

    #[test]
    fn a_reset_needs_no_requery_when_the_query_is_already_the_default() {
        // Arrange
        let mut app = tabbed_app();
        app.range = Range::ThisWeek(app.week_start);
        app.go_tab(2);
        app.key(press(KeyCode::Char('e')));

        // Act
        let effect = app.reset_done(Ok(()));

        // Assert
        assert_eq!(effect, crate::tui::app::Effect::None);
        assert_eq!(app.tab, 0);
        assert!(app.editor.is_none());
    }

    #[test]
    fn the_editor_window_draws_the_tab_as_rows_and_marks_the_cursor_cell() {
        // Arrange
        let mut app = tabbed_app();
        app.layout = Layout::parse("Charts=spend,tokens|burnup").unwrap();
        app.go_tab(0);
        app.key(press(KeyCode::Char('e')));

        // Act
        let text = screen(&app, 100, 30);

        // Assert
        let rows: Vec<&str> = text.lines().collect();
        let first = rows
            .iter()
            .position(|l| l.contains("[> spend"))
            .unwrap_or(0);
        assert!(rows[first].contains("[  tokens"), "{text}");
        assert!(rows[first + 1].contains("[  burnup"), "{text}");
        assert_eq!(rows[first + 1].matches("[  burnup").count(), 1, "{text}");
        assert!(text.contains("Layout: tabs and panels"), "{text}");
        assert!(text.contains("spend: cost per day"), "{text}");
        assert!(
            text.contains("changes are saved as you make them"),
            "{text}"
        );
    }

    #[test]
    fn the_picker_lists_the_panels_the_tab_lacks_and_marks_the_chosen_one() {
        // Arrange
        let mut app = tabbed_app();
        app.layout = Layout::parse("Charts=spend,tokens").unwrap();
        app.go_tab(0);
        app.key(press(KeyCode::Char('e')));

        // Act
        app.key(press(KeyCode::Char('a')));
        let text = screen(&app, 100, 30);

        // Assert
        assert!(text.contains("Add after tokens"), "{text}");
        assert!(text.contains("> models"), "{text}");
        assert!(text.contains("live"), "{text}");
        assert!(!text.contains("  tokens "), "{text}");
    }

    #[test]
    fn the_picker_names_the_last_panel_of_the_row_the_new_one_joins_in_an_arranged_tab() {
        // Arrange
        let mut app = tabbed_app();
        app.layout = Layout::parse("Charts=spend,tokens|models").unwrap();
        app.go_tab(0);
        app.key(press(KeyCode::Char('e')));

        // Act
        app.key(press(KeyCode::Char('a')));
        let text = screen(&app, 100, 30);

        // Assert
        assert!(text.contains("Add after models"), "{text}");
        assert!(!text.contains("Add after spend"), "{text}");
    }

    #[test]
    fn the_picker_scrolls_to_keep_the_chosen_line_in_view() {
        // Arrange
        let mut app = tabbed_app();
        app.layout = Layout::parse("Charts=spend,tokens").unwrap();
        app.go_tab(0);
        app.key(press(KeyCode::Char('e')));
        app.key(press(KeyCode::Char('a')));

        // Act
        for _ in 0..10 {
            app.key(press(KeyCode::Char('j')));
        }
        let text = screen(&app, 100, 30);

        // Assert
        assert!(text.contains("> breakdown.projects"), "{text}");
        assert!(!text.contains("  models "), "{text}");
    }

    #[test]
    fn ctrl_c_quits_from_the_editor() {
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));

        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(app.quit);
    }

    #[test]
    fn the_rename_prompt_and_a_refusal_are_drawn_in_the_editor() {
        let mut app = tabbed_app();
        app.key(press(KeyCode::Char('e')));
        app.key(press(KeyCode::Char('r')));
        let prompt = screen(&app, 100, 30);
        for _ in 0..6 {
            app.key(press(KeyCode::Backspace));
        }
        app.key(press(KeyCode::Enter));
        let refusal = screen(&app, 100, 30);

        assert!(prompt.contains("New name:"), "{prompt}");
        assert!(refusal.contains("A tab needs a name"), "{refusal}");
    }

    #[test]
    fn a_note_replaces_the_footer_hints_until_the_next_key() {
        let mut app = tabbed_app();
        app.note = Some("Layout kept for this run only".into());

        let shown = screen(&app, 140, 30);
        app.key(press(KeyCode::Char('t')));

        assert!(shown.contains("Layout kept for this run only"), "{shown}");
        assert!(app.note.is_none());
    }

    fn eight_panels() -> Vec<PanelId> {
        ALL.iter()
            .copied()
            .filter(|p| !p.is_pinned() && p.has_cell())
            .take(8)
            .collect()
    }

    #[test]
    fn on_a_wide_screen_every_count_from_one_to_eight_shows_every_panel_with_its_rows() {
        // Arrange
        let area = Rect::new(0, 0, 150, 44);
        let ids = eight_panels();

        for n in 1..=8 {
            // Act
            let placed = assert_tiles_body(area, &ids[..n], None);

            // Assert
            assert_eq!(placed.panels.len(), n, "{n} panels");
            assert_eq!(placed.hidden, None, "{n} panels");
            let mut by_row: Vec<u8> = Vec::new();
            let mut last_y = None;
            for (_, r) in &placed.panels {
                if last_y == Some(r.y) {
                    *by_row.last_mut().unwrap() += 1;
                } else {
                    by_row.push(1);
                    last_y = Some(r.y);
                }
            }
            assert_eq!(by_row, auto_rows(n), "{n} panels");
            assert!(placed
                .panels
                .iter()
                .all(|(_, r)| r.height >= MIN_ROW_HEIGHT));
        }
    }

    #[test]
    fn on_the_smallest_screen_a_tab_keeps_the_rows_that_fit_and_names_the_rest() {
        // Arrange
        let area = Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT);
        let ids = eight_panels();

        // Act
        let shown: Vec<(usize, usize, Option<Hidden>)> = (1..=8)
            .map(|n| {
                let placed = place(area, &ids[..n], None, true);
                (n, placed.panels.len(), placed.hidden)
            })
            .collect();

        // Assert
        let body_rows = usize::from((MIN_HEIGHT - HEADER_HEIGHT - 2) / MIN_ROW_HEIGHT);
        assert_eq!(body_rows, 3);
        for (n, drawn, hidden) in shown {
            let expected: usize = auto_rows(n)
                .iter()
                .take(body_rows)
                .map(|c| usize::from(*c))
                .sum();
            assert_eq!(drawn, expected, "{n} panels");
            assert_eq!(hidden.map_or(0, |h| h.count), n - expected, "{n} panels");
            if let Some(h) = hidden {
                assert_eq!(
                    h.needs_height,
                    MIN_HEIGHT + MIN_ROW_HEIGHT * auto_rows(n).len() as u16 - 16,
                    "{n} panels"
                );
            }
        }
    }

    #[test]
    fn the_screen_height_a_hidden_note_asks_for_shows_every_panel() {
        // Arrange
        let ids = eight_panels();
        let short = place(Rect::new(0, 0, 100, 24), &ids, None, true);

        // Act
        let needs = short.hidden.unwrap().needs_height;
        let tall = place(Rect::new(0, 0, 100, needs), &ids, None, true);

        // Assert
        assert_eq!(tall.hidden, None);
        assert_eq!(tall.panels.len(), 8);
    }

    #[test]
    fn rows_chosen_by_hand_give_way_to_fewer_rows_and_a_note_on_a_short_screen() {
        // Arrange
        let area = Rect::new(0, 0, 100, 24);
        let ids = eight_panels();
        let rows = [1, 1, 1, 1, 1, 1, 1, 1];

        // Act
        let placed = place(area, &ids, Some(&rows), true);

        // Assert
        assert_eq!(placed.panels.len(), 6);
        assert_eq!(placed.hidden.map(|h| h.count), Some(2));
    }

    #[test]
    fn the_budget_strip_draws_on_top_wherever_it_is_listed_and_beside_eight_panels() {
        // Arrange
        let area = Rect::new(0, 0, 150, 44);
        let mut ids = eight_panels();
        ids.push(PanelId::Budget);
        let mut first = vec![PanelId::Budget];
        first.extend(eight_panels());

        // Act
        let last = place(area, &ids, None, true);
        let top = place(area, &first, None, true);

        // Assert
        assert_eq!(last.budget.height, budget::HEIGHT);
        assert_eq!(last.budget.y, last.tabbar.bottom());
        assert_eq!(last.budget.width, area.width);
        assert_eq!(last.panels, top.panels);
        assert_eq!(last.panels.len(), 8);
        assert_eq!(last.hidden, None);
    }

    #[test]
    fn a_screen_too_short_for_the_strip_leaves_it_out_and_keeps_the_panels() {
        // Arrange
        let ids = [PanelId::Budget, PanelId::Models, PanelId::Breakdown];

        // Act
        let short = place(Rect::new(0, 0, 100, 23), &ids, None, true);
        let enough = place(Rect::new(0, 0, 100, 24), &ids, None, true);

        // Assert
        assert_eq!(short.budget.height, 0);
        assert_eq!(enough.budget.height, budget::HEIGHT);
        assert_eq!(short.panels.len(), 2);
    }

    #[test]
    fn the_footer_names_the_panels_a_short_screen_hides_and_the_height_that_shows_them() {
        // Arrange
        let mut app = tabbed_app();
        let words: Vec<&str> = eight_panels().iter().map(|p| p.word()).collect();
        app.layout = Layout::parse(&format!("A={};B=live", words.join(","))).unwrap();
        app.go_tab(0);

        // Act
        let short = screen(&app, 100, 24);
        let tall = screen(&app, 100, 44);

        // Assert
        assert!(
            short.contains("2 panels hidden, 28 rows show all"),
            "{short}"
        );
        assert!(!tall.contains("hidden"), "{tall}");
    }

    #[test]
    fn the_retired_projection_word_takes_no_cell_and_draws_nothing() {
        // Arrange
        let area = Rect::new(0, 0, 100, 30);
        let with = [PanelId::Models, PanelId::Breakdown, PanelId::Projection];
        let without = [PanelId::Models, PanelId::Breakdown];

        // Act
        let old = place(area, &with, None, true);
        let new = place(area, &without, None, true);

        // Assert
        assert_eq!(old.panels, new.panels);
        assert_eq!(old.hidden, None);
    }

    #[test]
    fn the_numbers_tab_stacks_models_over_breakdown_under_the_strip() {
        // Arrange
        let area = Rect::new(0, 0, 100, 30);
        let ids = Layout::default().tabs[1].panels.clone();

        // Act
        let placed = place(area, &ids, None, true);

        // Assert
        let rect = |id| placed.panels.iter().find(|(p, _)| *p == id).unwrap().1;
        let (models, breakdown) = (rect(PanelId::Models), rect(PanelId::Breakdown));
        assert_eq!(placed.budget.height, budget::HEIGHT);
        assert_eq!(models.y, placed.budget.bottom());
        assert_eq!(breakdown.y, models.bottom());
        assert_eq!((models.width, breakdown.width), (100, 100));
        assert_eq!(placed.panels.len(), 2);
    }

    #[test]
    fn the_hourly_panel_charts_the_last_day_with_its_total_in_the_title() {
        // Arrange
        let mut app = app_with(&[
            UsageEvent {
                timestamp: NOW - 60,
                ..event("opus", "alpha", 1.25)
            },
            UsageEvent {
                timestamp: NOW - 3 * 3600,
                ..event("sonnet", "alpha", 2.0)
            },
        ]);
        app.layout = Layout::parse("A=hourly,models").unwrap();

        // Act
        let text = screen(&app, 120, 30);

        // Assert
        assert!(text.contains("Spend per hour, 24h total $3.25"), "{text}");
        assert!(text.contains("total $3.25  peak $2.00 at"), "{text}");
        assert!(text.contains("now"), "{text}");
    }

    #[test]
    fn the_hourly_panel_says_so_when_the_last_day_is_idle() {
        // Arrange
        let mut app = app_with(&[UsageEvent {
            timestamp: NOW - 3 * 86_400,
            ..event("opus", "alpha", 1.0)
        }]);
        app.layout = Layout::parse("A=hourly,models").unwrap();

        // Act
        let text = screen(&app, 120, 30);

        // Assert
        assert!(text.contains("Spend per hour, 24h total $0.00"), "{text}");
        assert!(text.contains("No usage in the last 24 hours."), "{text}");
    }
}
