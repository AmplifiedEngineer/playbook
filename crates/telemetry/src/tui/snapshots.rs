// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Whole-screen snapshots of the view at fixed sizes, from a fixed set of
//! events and a fixed clock. A snapshot is plain text in
//! `tests/fixtures/usage_tui/`. After an intended layout change, regenerate
//! them with `UPDATE_SNAPSHOTS=1 cargo test --lib usage::tui::snapshots` and
//! review the diff.

use super::app::App;
use super::data::Data;
use super::data::LimitsView;
use super::icon::Icon;
use super::sort::Panel;
use super::theme::{ColorDepth, Theme};
use super::view::render;
use crate::limits::LimitPeriod;
use crate::limits_source::{Snapshot, Sources, Window};
use crate::query::{live_view, Range, WeekStart};
use crate::UsageEvent;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use std::path::PathBuf;

const NOW: i64 = 1_788_400_000;
const DAY: i64 = 86_400;

fn event(age_secs: i64, session: &str, model: &str, repo: &str, cost: f64) -> UsageEvent {
    UsageEvent {
        event_id: format!("{age_secs}{session}{model}"),
        timestamp: NOW - age_secs,
        session_id: session.into(),
        model: model.into(),
        repo: repo.into(),
        branch: "main".into(),
        input_tokens: 3_000,
        output_tokens: 800,
        cache_read_tokens: 20_000,
        cost_usd: cost,
        ..UsageEvent::default()
    }
}

fn fixture() -> Vec<UsageEvent> {
    let mut events = vec![
        event(20, "s-live", "claude-opus-5", "alpha", 0.75),
        event(300, "s-live", "claude-opus-5", "alpha", 0.5),
        event(2 * DAY, "s-old", "claude-sonnet-5", "beta", 1.25),
        event(9 * DAY, "s-old", "claude-opus-5", "beta", 6.0),
        event(20 * DAY, "s-older", "claude-haiku-5", "gamma", 0.1),
    ];
    // A few more days of synthetic spend so the charts have a shape. Every
    // fifth day is idle. Haiku stays the smallest model by a wide margin.
    for d in 1..30_i64 {
        if d % 5 == 0 || [2, 9, 20].contains(&d) {
            continue;
        }
        let base = [0.4, 1.1, 2.6, 0.8, 1.9, 0.3, 1.4][(d % 7) as usize];
        let (model, repo, cost) = match d % 3 {
            0 => ("claude-opus-5", "alpha", base),
            1 => ("claude-sonnet-5", "beta", base / 2.0),
            _ => ("claude-haiku-5", "gamma", base / 100.0),
        };
        events.push(event(
            d * DAY + 3 * 3_600,
            &format!("s-{d}"),
            model,
            repo,
            cost,
        ));
    }
    events
}

fn app(theme: Theme) -> App {
    let events = fixture();
    let live = live_view(&events, &events, &events, NOW);
    let mut app = App::new(Range::LastDays(30));
    app.theme = theme;
    app.data = Some(Data::build(&events, live, NOW, Range::LastDays(30), ""));
    app
}

fn draw(app: &App, width: u16, height: u16) -> String {
    draw_with(width, height, |f| render(f, app)).0
}

/// Draws with `paint` and returns the text and the buffer.
fn draw_with(width: u16, height: u16, paint: impl FnOnce(&mut ratatui::Frame)) -> (String, Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(paint).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..height)
        .map(|y| {
            let row: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
            row.trim_end().to_string()
        })
        .collect();
    (rows.join("\n") + "\n", buffer)
}

fn check(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/usage_tui")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing snapshot {}: {e}", path.display()));
    assert_eq!(
        actual, want,
        "snapshot {name} changed; rerun with UPDATE_SNAPSHOTS=1 if intended"
    );
}

#[test]
fn full_screen_100x30() {
    check("full_100x30", &draw(&app(Theme::Standard), 100, 30));
}

#[test]
fn minimum_screen_80x24() {
    check("min_80x24", &draw(&app(Theme::Standard), 80, 24));
}

#[test]
fn wide_screen_160x50() {
    check("wide_160x50", &draw(&app(Theme::Standard), 160, 50));
}

#[test]
fn the_help_window_at_three_sizes() {
    for (w, h) in [(80, 24), (100, 30), (160, 50)] {
        let mut a = app(Theme::Standard);
        a.area = ratatui::layout::Rect::new(0, 0, w, h);
        a.help = Some(0);
        check(&format!("help_{w}x{h}"), &draw(&a, w, h));
    }
}

#[test]
fn the_help_window_scrolled_to_the_end_at_80x24() {
    let mut a = app(Theme::Standard);
    a.area = ratatui::layout::Rect::new(0, 0, 80, 24);
    a.help = Some(usize::MAX);
    check("help_end_80x24", &draw(&a, 80, 24));
}

#[test]
fn the_week_range_has_wide_bars_and_weekday_names() {
    let mut a = app(Theme::Standard);
    let events = fixture();
    let live = live_view(&events, &events, &events, NOW);
    let range = Range::ThisWeek(crate::query::WeekStart::Monday);
    a.range = range;
    a.data = Some(Data::build(&events, live, NOW, range, ""));
    check("week_160x50", &draw(&a, 160, 50));
}

#[test]
fn today_compares_with_yesterday_and_charts_the_last_seven_days() {
    let mut a = app(Theme::Standard);
    let events = fixture();
    let live = live_view(&events, &events, &events, NOW);
    a.range = Range::Today;
    a.data = Some(Data::build(&events, live, NOW, Range::Today, ""));
    check("today_100x30", &draw(&a, 100, 30));
}

#[test]
fn a_moved_cursor_reads_out_its_day() {
    let mut a = app(Theme::Standard);
    a.cursor = Some(NOW.div_euclid(DAY) - 9);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Tue 2026-08-25"), "{text}");
}

fn with_spend_view(theme: Theme, view: crate::tui::app::SpendView) -> App {
    let mut a = app(theme);
    a.spend_view = view;
    a
}

#[test]
fn spend_stacked_by_model_100x30() {
    use crate::tui::app::SpendView;
    check(
        "stacked_100x30",
        &draw(
            &with_spend_view(Theme::Standard, SpendView::ByModel),
            100,
            30,
        ),
    );
}

#[test]
fn spend_stacked_by_model_without_color_uses_patterns() {
    use crate::tui::app::SpendView;
    let text = draw(&with_spend_view(Theme::Mono, SpendView::ByModel), 100, 30);
    let shown = crate::tui::chart::PATTERNS
        .iter()
        .filter(|c| text.contains(**c))
        .count();
    assert!(shown >= 3, "three models, three patterns, saw {shown}");
}

#[test]
fn the_daily_table_at_100x30() {
    use crate::tui::app::SpendView;
    check(
        "daily_100x30",
        &draw(&with_spend_view(Theme::Standard, SpendView::Daily), 100, 30),
    );
}

#[test]
fn a_model_wears_the_same_color_in_the_table_and_the_stacked_bars() {
    use crate::tui::app::SpendView;
    let a = with_spend_view(Theme::Standard, SpendView::ByModel);
    let buf = buffer(&a, 100, 30);
    let p = panels();
    let data = a.data.as_ref().unwrap();
    let pal = Theme::Standard.palette(ColorDepth::True);
    // The Models panel is sorted by cost: opus, sonnet, haiku.
    let rows: Vec<Color> = (0..3)
        .map(|i| buf[(p.models.x + 1, p.models.y + 2 + i)].fg)
        .collect();
    for (i, model) in ["claude-opus-5", "claude-sonnet-5", "claude-haiku-5"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            buf[(p.models.x + 1, p.models.y + 2 + i as u16)].symbol(),
            "\u{25a0}"
        );
        assert_eq!(
            rows[i],
            pal.color(pal.series[data.slots.slot(model)]),
            "{model}"
        );
    }
    let bars: std::collections::BTreeSet<String> = (p.spend.y + 1..p.spend.y + p.spend.height - 1)
        .flat_map(|y| (p.spend.x + 1..p.spend.x + p.spend.width - 1).map(move |x| (x, y)))
        .filter(|(x, y)| {
            ('\u{2581}'..='\u{2588}')
                .contains(&buf[(*x, *y)].symbol().chars().next().unwrap_or(' '))
        })
        .flat_map(|(x, y)| {
            [
                format!("{:?}", buf[(x, y)].fg),
                format!("{:?}", buf[(x, y)].bg),
            ]
        })
        .collect();
    // The two big models show up in the bars in the colors of their rows.
    for color in &rows[..2] {
        assert!(
            bars.contains(&format!("{color:?}")),
            "{color:?} not in {bars:?}"
        );
    }
    // The legend in the bottom border names them with the same swatches.
    let legend_row = p.spend.y + p.spend.height - 1;
    let legend: String = (p.spend.x..p.spend.x + p.spend.width)
        .map(|x| buf[(x, legend_row)].symbol())
        .collect();
    assert!(
        legend.contains("opus-5") && legend.contains("sonnet-5"),
        "{legend}"
    );
}

#[test]
fn the_daily_table_marks_today_and_the_peak_day() {
    use crate::tui::app::SpendView;
    let mut a = with_spend_view(Theme::Standard, SpendView::Daily);
    let buf = buffer(&a, 100, 30);
    let p = panels();
    // Newest first, so today is the first row.
    let today = &buf[(p.spend.x + 2, p.spend.y + 2)];
    let hi = rgb(0xb5, 0x40, 0x40);
    assert_eq!(today.fg, hi);
    assert!(today
        .modifier
        .contains(ratatui::style::Modifier::UNDERLINED));
    // By cost, the peak day ($6.01) is the first row.
    a.sorts[Panel::Spend.index()] = crate::tui::sort::Sort::new(3, true);
    let buf = buffer(&a, 100, 30);
    let peak_row = (p.spend.y + 2..p.spend.y + p.spend.height - 1)
        .find(|y| {
            let row: String = (p.spend.x..p.spend.x + p.spend.width)
                .map(|x| buf[(x, *y)].symbol())
                .collect();
            row.contains("$6.01")
        })
        .expect("the peak day is in the table");
    assert_eq!(buf[(p.spend.x + 2, peak_row)].fg, rgb(0xff, 0xb8, 0x14));
}

fn with_limits(
    a: App,
    snapshot: Option<&Snapshot>,
    budgets: &[(LimitPeriod, f64)],
    range: Range,
) -> App {
    let sources = Sources {
        snapshot: snapshot.cloned(),
        budgets: budgets.to_vec(),
        ..Sources::default()
    };
    with_sources(a, &sources, range)
}

fn with_sources(mut a: App, sources: &Sources, range: Range) -> App {
    let events = fixture();
    let live = live_view(&events, &events, &events, NOW);
    let mut data = Data::build(&events, live, NOW, range, "");
    data.limits = LimitsView::build(sources, &events, NOW, WeekStart::Monday, range);
    a.range = range;
    a.data = Some(data);
    a
}

fn plan_snapshot() -> Snapshot {
    Snapshot {
        captured_at: NOW - 60,
        five_hour: Some(Window {
            used_percentage: 50.0,
            resets_at: NOW + 3 * 3600,
        }),
        seven_day: Some(Window {
            used_percentage: 30.0,
            resets_at: NOW + 4 * DAY,
        }),
        spend_limit: None,
        ..Snapshot::default()
    }
}

#[test]
fn the_budget_panel_shows_the_plan_windows_as_measured() {
    let a = with_limits(
        app(Theme::Standard),
        Some(&plan_snapshot()),
        &[],
        Range::ThisWeek(WeekStart::Monday),
    );
    let text = draw(&a, 100, 36);
    assert!(text.contains("5 hour window [measured]"));
    assert!(text.contains("will exceed"));
    assert!(text.contains("weekly window [measured]"));
    check("budget_measured_100x36", &text);
}

#[test]
fn the_budget_panel_shows_a_manual_limit_and_says_so() {
    let a = with_limits(
        app(Theme::Standard),
        None,
        &[(LimitPeriod::Monthly, 40.0), (LimitPeriod::Weekly, 5.0)],
        Range::CurrentMonth,
    );
    let text = draw(&a, 160, 50);
    assert!(text.contains("[manual]"));
    assert!(text.contains("[local history]"));
    check("budget_manual_160x50", &text);
}

#[test]
fn the_budget_panel_says_how_to_set_a_limit_when_none_is_known() {
    let text = draw(&app(Theme::Standard), 100, 36);
    check("budget_unknown_100x36", &text);
    assert!(text.contains("Limit unknown: playbook config set --global usage.budget.monthlyUsd"));
}

#[test]
fn the_budget_panel_is_left_out_of_a_short_screen() {
    for (w, h) in [(80, 24), (100, 30), (100, 35)] {
        assert!(
            !draw(&app(Theme::Standard), w, h).contains("\u{2524}Budget\u{251c}"),
            "{w}x{h}"
        );
    }
    assert!(draw(&app(Theme::Standard), 100, 36).contains("\u{2524}Budget\u{251c}"));
}

#[test]
fn a_stale_plan_reading_is_marked() {
    let mut snap = plan_snapshot();
    snap.captured_at = NOW - 7 * 3600;
    snap.five_hour = Some(Window {
        used_percentage: 5.0,
        resets_at: NOW + 3600,
    });
    let a = with_limits(app(Theme::Standard), Some(&snap), &[], Range::Today);
    assert!(draw(&a, 160, 50).contains("[measured, stale]"));
}

#[test]
fn the_state_wears_the_error_color_when_the_limit_will_be_exceeded() {
    let a = with_limits(
        app(Theme::Standard),
        Some(&plan_snapshot()),
        &[],
        Range::ThisWeek(WeekStart::Monday),
    );
    let buf = buffer(&a, 100, 36);
    let row = crate::tui::view::layout(ratatui::layout::Rect::new(0, 0, 100, 36))
        .budget
        .y
        + 1;
    let cells: Vec<&str> = (0..100).map(|x| buf[(x, row)].symbol()).collect();
    let at = cells
        .windows(4)
        .position(|w| w.concat() == "will")
        .expect("the state is on the first limit row");
    let pal = Theme::Standard.palette(ColorDepth::True);
    assert_eq!(buf[(at as u16, row)].fg, pal.color(pal.error));
}

#[test]
fn too_small_screen() {
    check("too_small_60x20", &draw(&app(Theme::Standard), 60, 20));
}

#[test]
fn before_the_first_read() {
    check("loading_100x30", &draw(&App::new(Range::All), 100, 30));
}

#[test]
fn the_theme_changes_color_not_text() {
    let standard = draw(&app(Theme::Standard), 100, 30);
    let dark = draw(&app(Theme::Dark), 100, 30);
    let light = draw(&app(Theme::Light), 100, 30);
    let mono = draw(&app(Theme::Mono), 100, 30);
    // Only the footer differs: it names the theme.
    // Only the footer, which names the theme, and the legend, which the mono
    // theme leaves out, differ.
    let legend_row = usize::from(panels().spend.bottom() - 1);
    let body = |s: &str| {
        s.lines()
            .enumerate()
            .take(29)
            .filter(|(i, _)| *i != legend_row)
            // The models wear patterns, not colors, in mono.
            .map(|(_, l)| l.replace(crate::tui::chart::PATTERNS, "\u{25a0}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(body(&standard), body(&dark));
    assert_eq!(body(&dark), body(&light));
    assert_eq!(body(&dark), body(&mono));
}

fn buffer(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// The first cell on `row` whose symbol is `symbol`.
fn find(buf: &Buffer, symbol: &str, row: u16) -> (u16, u16) {
    let x = (0..buf.area.width)
        .find(|x| buf[(*x, row)].symbol() == symbol)
        .unwrap_or_else(|| panic!("no {symbol} on row {row}"));
    (x, row)
}

#[test]
fn the_default_theme_paints_a_black_ground_and_gray_text() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    // Every cell has the black ground.
    assert!(buf.content().iter().all(|c| c.bg == rgb(0, 0, 0)
        || c.bg == rgb(0x6a, 0x2f, 0x2f)
        || c.bg == rgb(0xb5, 0x40, 0x40)));
    // The header's totals use the main text color.
    let (x, y) = find(&buf, "$", 2);
    assert_eq!(buf[(x, y)].fg, rgb(0xcc, 0xcc, 0xcc));
}

#[test]
fn footer_and_header_text_use_the_main_text_color_not_the_dim_graph_color() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    let area = buf.area;
    for y in [area.bottom() - 1, 1] {
        for x in area.left()..area.right() {
            let cell = &buf[(x, y)];
            if cell.symbol().trim().is_empty() {
                continue;
            }
            assert_ne!(cell.fg, rgb(0x60, 0x60, 0x60), "dim text at {x},{y}");
        }
    }
}

fn panels() -> crate::tui::view::Panels {
    crate::tui::view::layout(ratatui::layout::Rect::new(0, 0, 100, 30))
}

#[test]
fn each_box_has_its_own_outline_color() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    let p = panels();
    let corner = |r: ratatui::layout::Rect| {
        assert_eq!(
            buf[(r.x, r.y)].symbol(),
            "\u{256d}",
            "rounded corner at {r:?}"
        );
        buf[(r.x, r.y)].fg
    };
    assert_eq!(corner(p.header), rgb(0x55, 0x6d, 0x59)); // header: cpu
    assert_eq!(corner(p.spend), rgb(0x55, 0x6d, 0x59)); // spend: cpu
    assert_eq!(corner(p.tokens), rgb(0x5c, 0x58, 0x8d)); // tokens: net
    assert_eq!(corner(p.models), rgb(0x6c, 0x6c, 0x4b)); // models: mem
    assert_eq!(corner(p.projects), rgb(0x6c, 0x6c, 0x4b)); // projects: mem
    assert_eq!(corner(p.live), rgb(0x80, 0x52, 0x52)); // live: proc
    assert_eq!(corner(p.sessions), rgb(0x80, 0x52, 0x52)); // sessions: proc
}

#[test]
fn titles_carry_a_hint_in_the_accent_color() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    let p = panels();
    // The hint follows the corner, a dash and the `┤`.
    let (x, y) = find(&buf, "\u{b9}", p.spend.y);
    assert_eq!(buf[(x, y)].fg, rgb(0xb5, 0x40, 0x40));
    assert_eq!(buf[(x - 1, y)].symbol(), "\u{2524}");
    assert_eq!(buf[(x + 1, y)].fg, rgb(0xee, 0xee, 0xee));
    // The focused panel (models, hint 3) wears a badge: title on accent.
    let (x, y) = find(&buf, "\u{b3}", p.models.y);
    assert_eq!(buf[(x, y)].bg, rgb(0xb5, 0x40, 0x40));
}

#[test]
fn the_bars_run_the_gradient_with_their_height_on_the_scale() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    let p = panels();
    // Collect the colors of bar cells inside the spend panel.
    let mut colors: Vec<Color> = Vec::new();
    for y in p.spend.y + 1..p.spend.y + p.spend.height - 1 {
        for x in p.spend.x + 1..p.spend.x + p.spend.width - 1 {
            let c = &buf[(x, y)];
            if c.symbol()
                .chars()
                .next()
                .is_some_and(|ch| ('\u{2581}'..='\u{2588}').contains(&ch))
            {
                colors.push(c.fg);
            }
        }
    }
    assert!(!colors.is_empty());
    // Taller days sit further along the gradient than small ones.
    let mut distinct = colors.clone();
    distinct.sort_by_key(|c| format!("{c:?}"));
    distinct.dedup();
    assert!(distinct.len() >= 3, "{distinct:?}");
}

#[test]
fn the_selected_row_of_the_focused_panel_is_highlighted() {
    let a = app(Theme::Standard);
    let buf = buffer(&a, 100, 30);
    let p = panels();
    let first = p.models.y + 2;
    // Models is focused: its first data row has the selection ground.
    assert_eq!(buf[(2, first)].bg, rgb(0x6a, 0x2f, 0x2f));
    assert_eq!(buf[(2, first)].fg, rgb(0xee, 0xee, 0xee));
    assert_eq!(buf[(2, first + 1)].bg, rgb(0, 0, 0));
    // The projects panel is not focused, so it shows no selection.
    assert_eq!(buf[(p.projects.x + 2, first)].bg, rgb(0, 0, 0));
}

#[test]
fn meters_use_the_gradient_and_the_dim_color_for_the_rest() {
    let buf = buffer(&app(Theme::Standard), 100, 30);
    let p = panels();
    let first = p.models.y + 2;
    // The haiku share is tiny: a dim dot at the end of its row.
    let (x, y) = find(&buf, "\u{b7}", first + 2);
    assert_eq!(buf[(x, y)].fg, rgb(0x40, 0x40, 0x40));
    // The opus share fills most of its row with colored squares.
    let squares: Vec<Color> = (0..50)
        .filter(|x| buf[(*x, first)].symbol() == "\u{25a0}")
        .map(|x| buf[(x, first)].fg)
        .collect();
    assert!(squares.len() >= 6, "{squares:?}");
    assert_ne!(squares[0], squares[squares.len() - 1]);
}

#[test]
fn a_256_color_terminal_gets_indexed_colors_only() {
    let mut a = app(Theme::Standard);
    a.depth = ColorDepth::Ansi256;
    let buf = buffer(&a, 100, 30);
    for c in buf.content() {
        for color in [c.fg, c.bg] {
            assert!(!matches!(color, Color::Rgb(..)), "{color:?}");
        }
    }
    assert!(buf
        .content()
        .iter()
        .any(|c| matches!(c.fg, Color::Indexed(_))));
}

#[test]
fn a_16_color_terminal_gets_named_colors_only() {
    let mut a = app(Theme::Standard);
    a.depth = ColorDepth::Ansi16;
    let buf = buffer(&a, 100, 30);
    for c in buf.content() {
        for color in [c.fg, c.bg] {
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                "{color:?}"
            );
        }
    }
}

#[test]
fn mono_draws_no_color_at_all_but_still_marks_the_selection() {
    let buf = buffer(&app(Theme::Mono), 100, 30);
    assert!(buf
        .content()
        .iter()
        .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
    let first = panels().models.y + 2;
    assert!(buf[(2, first)]
        .modifier
        .contains(ratatui::style::Modifier::REVERSED));
}

#[test]
fn the_dark_theme_keeps_the_terminal_ground() {
    let buf = buffer(&app(Theme::Dark), 100, 30);
    assert!(buf
        .content()
        .iter()
        .all(|c| matches!(c.bg, Color::Reset | Color::Blue | Color::Cyan)));
}

fn agents_app(with_telemetry: bool) -> App {
    use crate::aggregate::Group;
    use crate::tui::app::ProjectsView;
    let mut a = app(Theme::Standard);
    a.projects_view = ProjectsView::Agents;
    if with_telemetry {
        let group = |key: &str, messages: u64, cost: f64| Group {
            key: key.into(),
            messages,
            input_tokens: 1_000 * messages,
            output_tokens: 500 * messages,
            cost_usd: cost,
            ..Group::default()
        };
        let data = a.data.as_mut().unwrap();
        data.agents.rows = vec![
            group("main", 40, 9.0),
            group("general-purpose", 12, 2.5),
            group("reviewer", 5, 0.5),
        ];
        data.agents.cost_usd = 12.0;
        data.telemetry_rows = true;
        data.skills.rows = vec![group("review", 9, 3.0), group("plan", 4, 1.0)];
        data.skills.cost_usd = 4.0;
    }
    a
}

#[test]
fn the_agents_view_lists_cost_per_agent_and_the_telemetry_share() {
    let text = draw(&agents_app(true), 100, 30);
    assert!(
        text.contains("Agents") && text.contains("reviewer"),
        "{text}"
    );
    assert!(text.contains("source telemetry"), "{text}");
    check("agents_100x30", &text);
}

#[test]
fn the_agents_view_falls_back_to_transcript_counts_with_a_label() {
    let mut a = agents_app(false);
    let data = a.data.as_mut().unwrap();
    data.agents.from_transcripts = true;
    data.agents.rows = vec![crate::aggregate::Group {
        key: "Explore".into(),
        messages: 7,
        ..Default::default()
    }];

    let text = draw(&a, 100, 30);

    assert!(text.contains("from transcripts"), "{text}");
    assert!(text.contains("Explore"), "{text}");
    assert!(!text.contains("telemetry is off"), "{text}");
}

#[test]
fn the_agents_panel_names_the_fix_for_the_current_telemetry_state() {
    let mut a = agents_app(false);
    a.telemetry = Some(crate::telemetry_status::Mode::OffNoEndpoint);

    let text = draw(&a, 140, 30);

    assert!(text.contains("OTEL_EXPORTER_OTLP_ENDPOINT"), "{text}");
}

#[test]
fn transcript_agents_sort_by_count_when_the_cost_column_is_the_sort() {
    let mut a = agents_app(false);
    let data = a.data.as_mut().unwrap();
    data.agents.from_transcripts = true;
    let g = |key: &str, n: u64| crate::aggregate::Group {
        key: key.into(),
        messages: n,
        ..Default::default()
    };
    data.agents.rows = vec![g("alpha", 1), g("zulu", 9)];

    let text = draw(&a, 100, 30);

    let zulu = text.find("zulu").unwrap();
    let alpha = text.find("alpha").unwrap();
    assert!(zulu < alpha, "{text}");
    assert!(text.contains("n/a"), "{text}");
}

#[test]
fn the_agents_view_says_telemetry_is_off_when_there_are_no_rows() {
    let text = draw(&agents_app(false), 100, 30);
    assert!(text.contains("telemetry is off"), "{text}");
    check("agents_off_100x30", &text);
}

#[test]
fn the_skills_view_lists_cost_per_skill_and_says_when_none_ran() {
    use crate::tui::app::ProjectsView;
    let mut a = agents_app(true);
    a.projects_view = ProjectsView::Skills;
    let text = draw(&a, 100, 30);
    assert!(text.contains("Skills") && text.contains("review"), "{text}");
    check("skills_100x30", &text);

    a.data.as_mut().unwrap().skills = Default::default();
    let text = draw(&a, 100, 30);
    assert!(text.contains("No skill activity"), "{text}");
    assert!(!text.contains("telemetry is off"), "{text}");
}

#[test]
fn the_skills_view_says_telemetry_is_off_when_there_are_no_rows() {
    use crate::tui::app::ProjectsView;
    let mut a = agents_app(false);
    a.projects_view = ProjectsView::Skills;
    assert!(draw(&a, 100, 30).contains("telemetry is off"));
}

#[test]
fn v_cycles_projects_agents_skills_and_back() {
    use crate::tui::app::ProjectsView::*;
    assert_eq!(Projects.next().next().next().next().next(), Projects);
    assert_eq!(Agents.next(), Skills);
    assert_eq!(Skills.next(), Tools);
    assert_eq!(Tools.next(), Health);
}

fn health_app(with_telemetry: bool, view: crate::tui::app::ProjectsView) -> App {
    use crate::telemetry_db::{Health, ToolStat};
    let mut a = app(Theme::Standard);
    let events = fixture();
    let live = live_view(&events, &events, &events, NOW);
    a.range = Range::Today;
    let mut data = Data::build(&events, live, NOW, Range::Today, "");
    if with_telemetry {
        let t = |name: &str, calls: u64, failures: u64, total_ms: u64, max_ms: u64| ToolStat {
            name: name.into(),
            calls,
            failures,
            total_ms,
            max_ms,
        };
        data.telemetry_rows = true;
        data.health.tools = vec![
            t("Bash", 40, 4, 40 * 900, 12_000),
            t("Read", 120, 0, 120 * 8, 90),
            t("Edit", 30, 3, 30 * 15, 80),
        ];
        data.health.tools_before = vec![
            t("Bash", 30, 1, 30 * 600, 9_000),
            t("Read", 90, 0, 90 * 10, 70),
        ];
        data.health.now = Health {
            requests: 200,
            api_ms: 900_000,
            errors: 6,
            retried: 2,
            tool_calls: 190,
            tool_failures: 7,
            tool_ms: 37_000,
            active_ms: 5_400_000,
            lines_changed: 1_200,
            commits: 4,
            pull_requests: 1,
        };
        data.health.before = Some(Health {
            requests: 150,
            api_ms: 600_000,
            errors: 3,
            retried: 1,
            tool_calls: 120,
            tool_failures: 1,
            tool_ms: 20_000,
            active_ms: 3_600_000,
            lines_changed: 800,
            commits: 2,
            pull_requests: 1,
        });
    }
    a.data = Some(data);
    a.projects_view = view;
    a
}

#[test]
fn the_tools_view_shows_latency_and_error_rate_against_the_previous_period() {
    use crate::tui::app::ProjectsView;
    let text = draw(&health_app(true, ProjectsView::Tools), 100, 30);
    assert!(text.contains("Tools") && text.contains("Bash"), "{text}");
    assert!(
        text.contains("vs yesterday") && text.contains("source telemetry"),
        "{text}"
    );
    check("tools_100x30", &text);
}

#[test]
fn the_health_view_shows_errors_retries_busy_time_and_cache_hit_against_the_previous_period() {
    use crate::tui::app::ProjectsView;
    let text = draw(&health_app(true, ProjectsView::Health), 100, 30);
    assert!(
        text.contains("Health") && text.contains("Retries"),
        "{text}"
    );
    // A tall terminal shows every figure, the cache hit rate from transcripts too.
    let tall = draw(&health_app(true, ProjectsView::Health), 160, 50);
    assert!(
        tall.contains("Cache hit")
            && tall.contains("Active time")
            && tall.contains("Lines changed"),
        "{tall}"
    );
    check("health_100x30", &text);
}

#[test]
fn the_tools_and_health_views_say_telemetry_is_off_when_there_are_no_rows() {
    use crate::tui::app::ProjectsView;
    for view in [ProjectsView::Tools, ProjectsView::Health] {
        let text = draw(&health_app(false, view), 100, 30);
        assert!(text.contains("telemetry is off"), "{view:?}\n{text}");
    }
}

#[test]
fn the_tools_view_tells_a_stopped_collector_from_telemetry_off_and_from_no_rows() {
    use crate::telemetry_status::Mode;
    use crate::tui::app::ProjectsView;
    let mut a = health_app(false, ProjectsView::Tools);
    a.telemetry = Some(Mode::OnCollectorDown);
    let down = draw(&a, 100, 40);
    a.telemetry = Some(Mode::OnRunning);
    let quiet = draw(&a, 100, 30);

    assert!(down.contains("collector is not running"), "{down}");
    assert!(!down.contains("telemetry is off"), "{down}");
    // The command sits alone on its line, inside the panel border.
    let alone = |text: &str, command: &str| {
        text.lines()
            .any(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()) == command)
    };
    assert!(alone(&down, crate::daemon::START_HINT), "{down}");
    assert!(quiet.contains("collector runs"), "{quiet}");
    check("tools_collector_down_100x40", &down);
}

#[test]
fn the_tools_view_is_readable_at_the_minimum_size_and_without_a_comparison() {
    use crate::tui::app::ProjectsView;
    let a = health_app(true, ProjectsView::Tools);
    assert!(draw(&a, 80, 24).contains("Bash"));
    let mut b = app(Theme::Standard);
    b.projects_view = ProjectsView::Health;
    let data = b.data.as_mut().unwrap();
    data.telemetry_rows = true;
    data.health.now.requests = 5;
    let text = draw(&b, 100, 30);
    assert!(
        text.contains("API errors") && !text.contains("before"),
        "{text}"
    );
}

fn top_row(icon: Icon) -> String {
    let mut a = app(Theme::Dark);
    a.icon = icon;
    draw(&a, 100, 30)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn the_header_title_starts_with_the_tv_in_both_width_modes() {
    let wide = top_row(Icon::Wide);
    // The buffer keeps a blank cell behind a two-cell emoji, so the wide row
    // reads emoji, blank, one pad. The narrow row has two pads after the blank.
    assert!(
        wide.contains("\u{2524}\u{1F4FA}  Agent Budget Dashboard"),
        "{wide}"
    );
    let narrow = top_row(Icon::Narrow);
    assert!(
        narrow.contains("\u{2524}\u{1F4FA}   Agent Budget Dashboard"),
        "{narrow}"
    );
}

#[test]
fn the_header_has_no_icon_in_plain_text_mode() {
    let plain = top_row(Icon::Off);
    assert!(
        plain.contains("\u{2500}\u{2524}Agent Budget Dashboard"),
        "{plain}"
    );
    assert!(!plain.contains('\u{1F4FA}'));
}

fn tabbed(tab: usize) -> App {
    let mut a = app(Theme::Standard);
    a.layout = crate::tui::layout::Layout::default();
    a.go_tab(tab as isize);
    a
}

#[test]
fn the_charts_tab_stacks_the_three_charts_at_100x30() {
    let a = with_limits(
        tabbed(0),
        None,
        &[(LimitPeriod::Monthly, 100.0)],
        Range::LastDays(30),
    );

    check("tab_charts_100x30", &draw(&a, 100, 30));
}

#[test]
fn two_pinned_spend_views_draw_side_by_side_and_only_the_focused_one_is_bold() {
    // Arrange
    let mut a = app(Theme::Standard);
    a.layout = crate::tui::layout::Layout::parse("A=spend.total,spend.model|tokens").unwrap();
    a.go_tab(0);
    a.focus = crate::tui::sort::Panel::Spend;
    a.key(ratatui::crossterm::event::KeyEvent::new(
        ratatui::crossterm::event::KeyCode::Tab,
        ratatui::crossterm::event::KeyModifiers::NONE,
    ));

    // Act
    let (text, buf) = draw_with(100, 30, |f| render(f, &a));

    // Assert
    let title = text.lines().find(|l| l.contains("Spend per")).unwrap_or("");
    assert!(title.contains("Spend per day, USD"), "{text}");
    assert!(title.contains("Spend per day by model, USD"), "{text}");
    assert!(!text.contains("view:"), "{text}");
    let at = |needle: &str| {
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(needle))
            .unwrap_or((0, ""));
        let x = line[..line.find(needle).unwrap_or(0)].chars().count();
        buf[(x as u16, y as u16)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)
    };
    assert!(!at("Spend per day, USD"));
    assert!(at("Spend per day by model"));
    check("split_spend_100x30", &text);
}

#[test]
fn two_pinned_breakdown_views_each_draw_their_own_table() {
    // Arrange
    let mut a = app(Theme::Standard);
    a.layout =
        crate::tui::layout::Layout::parse("A=breakdown.agents,breakdown.skills|tokens").unwrap();
    a.go_tab(0);

    // Act
    let text = draw(&a, 100, 30);

    // Assert
    let title = text
        .lines()
        .find(|l| l.contains("Agents"))
        .unwrap_or_default();
    assert!(title.contains("Skills"), "{text}");
    assert!(!text.contains("view:"), "{text}");
    check("split_breakdown_100x30", &text);
}

#[test]
fn the_numbers_tab_shows_the_tables_and_the_budget_at_100x30() {
    check("tab_numbers_100x30", &draw(&tabbed(1), 100, 30));
}

#[test]
fn the_sessions_tab_stacks_live_and_sessions_at_80x24() {
    check("tab_sessions_80x24", &draw(&tabbed(2), 80, 24));
}

#[test]
fn the_layout_editor_window_at_100x30() {
    let mut a = tabbed(0);
    a.editor = Some(crate::tui::editor::Editor::new(&a.layout, 0));

    check("layout_editor_100x30", &draw(&a, 100, 30));
}

#[test]
fn the_layout_editor_picker_at_100x30() {
    let mut a = tabbed(0);
    let mut editor = crate::tui::editor::Editor::new(&a.layout, 0);
    editor.key(ratatui::crossterm::event::KeyCode::Char('a'));
    a.editor = Some(editor);

    check("layout_editor_picker_100x30", &draw(&a, 100, 30));
}

#[test]
fn the_projection_panel_shows_both_paces_for_a_budget_and_the_window_average() {
    let mut a = with_limits(
        tabbed(1),
        Some(&plan_snapshot()),
        &[(LimitPeriod::Monthly, 100.0)],
        Range::LastDays(30),
    );
    a.layout = crate::tui::layout::Layout::default();
    a.go_tab(1);

    let text = draw(&a, 140, 40);

    assert!(text.contains("Projection"), "{text}");
    assert!(text.contains("Monthly budget"), "{text}");
    assert!(text.contains("last 24h"), "{text}");
    assert!(text.contains("window average"), "{text}");
    check("projection_140x40", &text);
}

fn enterprise_sources(cache_age: i64) -> Sources {
    use crate::plan::{Account, Plan, SpendLimit, UsageCache};
    Sources {
        account: Account {
            plan: Plan::EnterpriseOrUnknown,
            cache: Some(UsageCache {
                fetched_at: NOW - cache_age,
                five_hour: None,
                seven_day: None,
                spend: Some(SpendLimit {
                    limit_usd: 500.0,
                    used_usd: Some(120.0),
                }),
                billed: None,
                extra_windows: Vec::new(),
            }),
        },
        ..Sources::default()
    }
}

#[test]
fn an_enterprise_account_shows_the_cached_spend_limit_and_says_the_windows_are_not_available() {
    let mut a = with_sources(tabbed(1), &enterprise_sources(600), Range::LastDays(30));
    a.layout = crate::tui::layout::Layout::default();
    a.go_tab(1);

    let text = draw(&a, 140, 40);

    assert!(text.contains("[cached 10m ago]"), "{text}");
    assert!(text.contains("$120.00/$500.00"), "{text}");
    assert!(
        text.contains("5 hour and weekly windows: not available for this plan"),
        "{text}"
    );
    check("projection_enterprise_140x40", &text);
}

#[test]
fn a_stale_cached_limit_is_labeled_with_its_age_and_says_used_is_this_machine_only() {
    let a = with_sources(
        tabbed(1),
        &enterprise_sources(46 * 3600),
        Range::LastDays(30),
    );

    let text = draw(&a, 140, 40);

    assert!(
        text.contains("[limit cached 1d 22h ago, used is this machine only]"),
        "{text}"
    );
}

#[test]
fn with_no_limit_and_no_windows_both_panels_say_what_is_missing() {
    let a = with_sources(tabbed(1), &Sources::default(), Range::LastDays(30));

    let text = draw(&a, 140, 40);

    assert!(text.contains("Limit unknown:"), "{text}");
    assert_eq!(
        text.matches("5 hour and weekly windows: not available for this plan")
            .count(),
        2,
        "{text}"
    );
}

const BURN_START: i64 = 1_788_220_800;
const BURN_NOW: i64 = BURN_START + 14 * DAY + 3 * 3_600;
const BURN_DAILY: [f64; 7] = [4.0, 11.0, 26.0, 8.0, 19.0, 3.0, 14.0];
/// What the server billed, a little under the local estimate.
const BURN_BILLED: f64 = 100.0;

fn burn_events() -> Vec<UsageEvent> {
    (0..15_i64)
        .map(|d| UsageEvent {
            timestamp: BURN_START + d * DAY + 3_600,
            cost_usd: BURN_DAILY[(d % 7) as usize],
            ..UsageEvent::default()
        })
        .collect()
}

/// The burn-up of the fixed fortnight, for one kind of data.
fn burn_case(kind: &str) -> (crate::burnup::Burnup, Vec<crate::limits::Limit>) {
    use crate::burnup::{burnup, spend_only, Inputs};
    use crate::limits::{Limit, Origin, Unit};
    let end = BURN_START + 30 * DAY;
    let events = burn_events();
    let total: f64 = events.iter().map(|e| e.cost_usd).sum();
    let limit = match kind {
        "nolimit" => None,
        "exceeded" => Some(120.0),
        _ => Some(300.0),
    };
    let mut limits: Vec<Limit> = limit
        .map(|l| Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Manual,
            used: total,
            limit: l,
            start: BURN_START,
            end,
        })
        .into_iter()
        .collect();
    if kind == "windows" {
        let window = |period, used| Limit {
            unit: Unit::Percent,
            period,
            origin: Origin::Measured,
            used,
            limit: 100.0,
            start: BURN_NOW - 3_600,
            end: BURN_NOW + 3_600,
        };
        limits.push(window(LimitPeriod::FiveHour, 50.0));
        limits.push(window(LimitPeriod::SevenDay, 30.0));
    }
    let billed = (kind == "limit").then_some((BURN_NOW - 600, BURN_BILLED));
    let b = match limits.first() {
        Some(l) => burnup(
            l,
            crate::tz::Zone::Utc,
            Inputs {
                events: &events,
                readings: &[],
                billed,
                now: BURN_NOW,
            },
        )
        .unwrap(),
        None => spend_only(&events, (BURN_START, end), crate::tz::Zone::Utc, BURN_NOW).unwrap(),
    };
    (b, limits)
}

/// A five hour window at 50 percent, read twice, for the percent chart.
fn window_case() -> (crate::burnup::Burnup, Vec<crate::limits::Limit>) {
    use crate::burnup::{burnup, Inputs};
    use crate::limits::{Limit, Origin, Unit};
    let l = Limit {
        unit: Unit::Percent,
        period: LimitPeriod::FiveHour,
        origin: Origin::Measured,
        used: 50.0,
        limit: 100.0,
        start: BURN_NOW - 3 * 3_600,
        end: BURN_NOW + 2 * 3_600,
    };
    let readings = [(BURN_NOW - 2 * 3_600, 10.0), (BURN_NOW - 3_600, 30.0)];
    let b = burnup(
        &l,
        crate::tz::Zone::Utc,
        Inputs {
            events: &[],
            readings: &readings,
            billed: None,
            now: BURN_NOW,
        },
    )
    .unwrap();
    (b, vec![l])
}

fn draw_burnup(kind: &str, theme: Theme, w: u16, h: u16) -> (String, Buffer) {
    crate::tz::set(crate::tz::Zone::Utc);
    let (b, limits) = burn_case(kind);
    let pal = theme.palette(ColorDepth::True);
    draw_with(w, h, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &limits, &pal);
    })
}

#[test]
fn the_burnup_panel_at_three_sizes_in_both_themes_for_each_kind_of_data() {
    for kind in ["limit", "nolimit"] {
        for (theme, name) in [(Theme::Standard, "standard"), (Theme::Mono, "mono")] {
            if kind != "limit" && name == "mono" {
                continue;
            }
            for (w, h) in [(80, 24), (100, 30), (140, 40)] {
                let (text, _) = draw_burnup(kind, theme, w, h);

                check(&format!("burnup_{kind}_{name}_{w}x{h}"), &text);
            }
        }
    }
}

#[test]
fn the_burnup_panel_shows_the_windows_and_the_exceeded_limit() {
    let (windows, _) = draw_burnup("windows", Theme::Standard, 140, 40);
    let (exceeded, _) = draw_burnup("exceeded", Theme::Standard, 100, 30);

    assert!(windows.contains("5 hour window 50%"), "{windows}");
    assert!(windows.contains("weekly window 30%"), "{windows}");
    assert!(exceeded.contains("50% 09-"), "{exceeded}");
    check("burnup_windows_standard_140x40", &windows);
    check("burnup_exceeded_standard_100x30", &exceeded);
}

#[test]
fn the_burnup_chart_names_every_line_and_marks_the_crossings() {
    let (text, _) = draw_burnup("limit", Theme::Standard, 100, 30);

    let legend = text
        .lines()
        .find(|l| l.contains("spent"))
        .unwrap_or_default();
    for part in [
        "\u{2588} spent",
        "\u{00b7} projected",
        "\u{2501} limit",
        "\u{2508} 80%",
        "\u{2504} 50%",
        "\u{25b2} crossing",
        "\u{25c6} reported by Claude",
    ] {
        assert!(legend.contains(part), "{part}\n{text}");
    }
    for glyph in [
        '\u{2588}', '\u{00b7}', '\u{2501}', '\u{25c6}', '\u{25b2}', '\u{2504}', '\u{2508}',
    ] {
        let rows: Vec<&str> = text.lines().collect();
        let legend_at = rows.iter().position(|l| l.contains("spent")).unwrap_or(0);
        assert!(
            rows[..legend_at.saturating_sub(2)]
                .iter()
                .any(|l| l.contains(glyph)),
            "{glyph} is not drawn in the plot\n{text}"
        );
    }
    assert!(
        text.contains("100% beyond") || text.contains("100% ~"),
        "{text}"
    );
}

#[test]
fn mono_carries_every_burnup_line_without_color() {
    let (mono, buf) = draw_burnup("limit", Theme::Mono, 100, 30);
    let (standard, _) = draw_burnup("limit", Theme::Standard, 100, 30);

    assert_eq!(mono, standard);
    assert!(buf.content().iter().all(|c| c.fg == Color::Reset));
}

#[test]
fn a_panel_under_forty_columns_shows_the_table() {
    let (text, _) = draw_burnup("limit", Theme::Mono, 30, 24);

    assert!(text.contains("date"), "{text}");
    assert!(text.contains("cumulative"), "{text}");
    assert!(!text.contains('\u{2588}'), "{text}");
    check("burnup_limit_mono_30x24", &text);
}

#[test]
fn before_the_period_starts_the_panel_says_so() {
    crate::tz::set(crate::tz::Zone::Utc);
    let b = crate::burnup::spend_only(
        &[],
        (BURN_NOW + DAY, BURN_NOW + 31 * DAY),
        crate::tz::Zone::Utc,
        BURN_NOW,
    )
    .unwrap();
    let pal = Theme::Mono.palette(ColorDepth::True);

    let (text, _) = draw_with(80, 12, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &[], &pal);
    });

    assert!(text.contains("the period has not started"), "{text}");
}

#[test]
fn the_charts_tab_draws_the_burnup_beside_the_other_charts_at_140x40() {
    let a = with_limits(
        tabbed(0),
        Some(&plan_snapshot()),
        &[(LimitPeriod::Monthly, 100.0)],
        Range::LastDays(30),
    );

    let text = draw(&a, 140, 40);

    assert!(text.contains("Burn-up"), "{text}");
    assert!(text.contains("5 hour window"), "{text}");
}

#[test]
fn the_charts_tab_at_the_smallest_screen_shows_the_burnup_table() {
    let a = with_limits(
        tabbed(0),
        None,
        &[(LimitPeriod::Monthly, 100.0)],
        Range::LastDays(30),
    );

    let text = draw(&a, 80, 24);

    assert!(text.contains("Burn-up"), "{text}");
    check("tab_charts_80x24", &text);
}

#[test]
fn the_burnup_dates_follow_the_zone_of_its_days_not_the_global_zone() {
    crate::tz::set(crate::tz::Zone::Fixed(-3 * 3_600));
    let (b, limits) = burn_case("limit");
    let b = crate::burnup::Burnup {
        zone: crate::tz::Zone::Utc,
        ..b
    };
    let pal = Theme::Mono.palette(ColorDepth::True);

    let (text, _) = draw_with(100, 30, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &limits, &pal);
    });

    assert!(text.contains("09-01"), "{text}");
    assert!(!text.contains("08-31"), "{text}");
    assert!(text.contains("spend this period, UTC"), "{text}");
}

#[test]
fn a_percent_window_is_charted_from_its_readings_with_percent_labels() {
    crate::tz::set(crate::tz::Zone::Utc);
    let (b, limits) = window_case();
    let pal = Theme::Standard.palette(ColorDepth::True);

    let (text, _) = draw_with(100, 30, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &limits, &pal);
    });

    assert!(text.contains("5 hour window used, UTC"), "{text}");
    assert!(text.contains("\u{2588} used"), "{text}");
    assert!(text.contains("50% 03:00 \u{25b2}  80% ~04:48"), "{text}");
    assert!(
        text.lines().nth(1).is_some_and(|l| l.contains("105% ")),
        "{text}"
    );
    assert!(
        text.contains('\u{2588}') && text.contains('\u{2501}'),
        "{text}"
    );
    check("burnup_window_standard_100x30", &text);
}

#[test]
fn a_rate_limit_is_marked_on_the_chart_with_its_line_and_legend_entry() {
    crate::tz::set(crate::tz::Zone::Utc);
    let (b, limits) = window_case();
    let pal = Theme::Mono.palette(ColorDepth::True);
    let plain = draw_with(100, 30, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &limits, &pal);
    })
    .0;
    let end = b.end;
    let b = b.with_rate_limits(&[
        BURN_NOW - 3_600,
        BURN_NOW - 4 * 3_600,
        BURN_NOW + 3_600,
        end,
    ]);

    let (text, _) = draw_with(100, 30, |f| {
        crate::tui::burnup::render_burnup(f, f.area(), Some(&b), &limits, &pal);
    });

    assert!(!plain.contains('\u{00d7}'), "{plain}");
    assert_eq!(
        text.matches('\u{00d7}').count(),
        3,
        "two marks, one legend: {text}"
    );
    assert!(text.contains("\u{00d7} rate limited"), "{text}");
    assert!(
        text.contains("Claude Code was rate limited at 2026-"),
        "{text}"
    );
}

#[test]
fn the_charted_burnup_is_the_most_used_limit_else_the_spend_alone() {
    let (low, _) = burn_case("limit");
    let (high, _) = burn_case("exceeded");
    let (spend, _) = burn_case("nolimit");
    let view = crate::tui::data::LimitsView {
        burnups: vec![Some(low), Some(high.clone())],
        spend_only: Some(spend.clone()),
        ..Default::default()
    };
    let none = crate::tui::data::LimitsView {
        spend_only: Some(spend.clone()),
        ..Default::default()
    };

    assert_eq!(view.charted(), Some(&high));
    assert_eq!(none.charted(), Some(&spend));
}
