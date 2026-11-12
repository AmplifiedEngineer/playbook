// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pbtop pictures under `docs/images`, drawn from the real view with the
//! real theme, so their colors cannot drift. Each `Shot` names a picture,
//! its theme, its range and how to set up the app. The test renders every
//! shot with sample data and a fixed clock, and compares it with the
//! committed file. After an intended change, rewrite the file with
//! `UPDATE_SNAPSHOTS=1 cargo test -p playbook-telemetry --lib tui::screenshot`
//! and review the diff.

use super::app::App;
use super::data::{Data, LimitsView};
use super::sample;
use super::theme::{ColorDepth, Theme};
use super::view::render;
use crate::query::{live_view, Range, WeekStart};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use std::fmt::Write;
use std::path::PathBuf;

/// 2026-10-21 00:46 UTC, late enough in the month for a full chart.
const NOW: i64 = 1_792_543_560;
const COLS: u16 = 150;
const ROWS: u16 = 44;
const CELL_W: f64 = 8.4;
const CELL_H: f64 = 18.0;
const MARGIN: f64 = 14.0;
const BASELINE: f64 = 13.7;
const FONT: &str = "SFMono-Regular,Menlo,Consolas,'DejaVu Sans Mono',monospace";

/// Page and default text colors, and which table gives the named terminal
/// colors their value.
///
/// The Standard theme paints its own page, so its ground comes from the
/// palette. The Dark and Light themes keep the terminal's own colors: a
/// default ground and the sixteen named ANSI colors. A picture has no
/// terminal, so it uses one fixed page and one fixed table per theme.
struct Ground {
    bg: String,
    fg: String,
    light: bool,
}

/// The sixteen named ANSI colors, as they look on a dark page.
const ANSI_DARK: [(Color, &str); 16] = [
    (Color::Black, "#000000"),
    (Color::Red, "#e5534b"),
    (Color::Green, "#57ab5a"),
    (Color::Yellow, "#daaa3f"),
    (Color::Blue, "#539bf5"),
    (Color::Magenta, "#b083f0"),
    (Color::Cyan, "#39c5cf"),
    (Color::Gray, "#adbac7"),
    (Color::DarkGray, "#768390"),
    (Color::LightRed, "#ff938a"),
    (Color::LightGreen, "#6bc46d"),
    (Color::LightYellow, "#eac55f"),
    (Color::LightBlue, "#6cb6ff"),
    (Color::LightMagenta, "#dcbdfb"),
    (Color::LightCyan, "#56d4dd"),
    (Color::White, "#ffffff"),
];

/// The same names as they look on a white page: darker, so they read.
const ANSI_LIGHT: [(Color, &str); 16] = [
    (Color::Black, "#1f2328"),
    (Color::Red, "#cf222e"),
    (Color::Green, "#116329"),
    (Color::Yellow, "#7d4e00"),
    (Color::Blue, "#0969da"),
    (Color::Magenta, "#8250df"),
    (Color::Cyan, "#1b7c83"),
    (Color::Gray, "#6e7781"),
    (Color::DarkGray, "#57606a"),
    (Color::LightRed, "#a40e26"),
    (Color::LightGreen, "#1a7f37"),
    (Color::LightYellow, "#9a6700"),
    (Color::LightBlue, "#0550ae"),
    (Color::LightMagenta, "#6639ba"),
    (Color::LightCyan, "#3192aa"),
    (Color::White, "#ffffff"),
];

impl Ground {
    fn new(theme: Theme) -> Ground {
        let pal = theme.palette(ColorDepth::True);
        let light = theme == Theme::Light;
        let (bg, fg) = if light {
            ("#ffffff", "#1f2328")
        } else {
            ("#1e1e1e", "#d4d4d4")
        };
        let mut ground = Ground {
            bg: bg.to_string(),
            fg: fg.to_string(),
            light,
        };
        ground.bg = ground.hex(pal.color(pal.bg), bg);
        ground.fg = ground.hex(pal.color(pal.fg), fg);
        ground
    }

    /// A color as `#rrggbb`. A color the table does not know, such as the
    /// terminal default, takes `fallback`.
    fn hex(&self, color: Color, fallback: &str) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            named => {
                let table = if self.light { &ANSI_LIGHT } else { &ANSI_DARK };
                table
                    .iter()
                    .find(|(c, _)| *c == named)
                    .map_or(fallback, |(_, hex)| hex)
                    .to_string()
            }
        }
    }
}

/// One run of same-styled text on a row: first cell, cell count, fill, text, bold.
type Run = Option<(u16, u16, String, String, bool)>;

/// One picture: the file path under `docs/images`, the theme and range to
/// draw, the text for screen readers, and a step that sets the public `App`
/// fields to reach the view.
struct Shot {
    file: String,
    label: String,
    theme: Theme,
    range: Range,
    setup: fn(&mut App),
}

fn no_setup(_: &mut App) {}

/// The views of the pbtop page, each drawn in both themes. The stem is the
/// start of the file name under `docs/images/pbtop/`.
struct View {
    stem: &'static str,
    label: &'static str,
    setup: fn(&mut App),
}

/// The default layout has three tabs; `App::new` starts with the one tab
/// layout, so a tab shot sets the layout first.
fn tabs(app: &mut App, tab: usize) {
    app.layout = super::layout::Layout::default();
    app.tab = tab;
}

fn charts(app: &mut App) {
    tabs(app, 0);
}

fn numbers(app: &mut App) {
    tabs(app, 1);
}

fn sessions(app: &mut App) {
    tabs(app, 2);
}

fn help(app: &mut App) {
    charts(app);
    app.help = Some(0);
}

fn layout_editor(app: &mut App) {
    charts(app);
    app.editor = Some(super::editor::Editor::new(&app.layout, 0));
}

fn budget(app: &mut App) {
    charts(app);
    app.budget_popup = Some(super::budget_popup::BudgetPopup::default());
}

const VIEWS: &[View] = &[
    View {
        stem: "charts",
        label: "pbtop Charts tab: spend per day, spend per day by model, tokens per day, spend per hour and a burn-up panel",
        setup: charts,
    },
    View {
        stem: "numbers",
        label: "pbtop Numbers tab: budget, models and projects",
        setup: numbers,
    },
    View {
        stem: "sessions",
        label: "pbtop Sessions tab: live sessions and sessions per day",
        setup: sessions,
    },
    View {
        stem: "help",
        label: "pbtop help window listing every key",
        setup: help,
    },
    View {
        stem: "layout-editor",
        label: "pbtop layout editor over the Charts tab, for arranging tabs and panels",
        setup: layout_editor,
    },
    View {
        stem: "budget",
        label: "pbtop budget window over the Charts tab, asking for a monthly budget",
        setup: budget,
    },
];

/// The shots in `docs/images/pbtop/`, in two themes, plus the older monthly
/// picture on the default theme that the old guide and outside links use.
fn shots() -> Vec<Shot> {
    let mut all = vec![Shot {
        file: "pbtop-month.svg".to_string(),
        label: "pbtop monthly charts view with sample data".to_string(),
        theme: Theme::Standard,
        range: Range::CurrentMonth,
        setup: no_setup,
    }];
    for view in VIEWS {
        for theme in [Theme::Dark, Theme::Light] {
            all.push(Shot {
                file: format!("pbtop/{}-{}.svg", view.stem, theme.name()),
                label: format!("{}, sample data, {} theme", view.label, theme.name()),
                theme,
                range: Range::CurrentMonth,
                setup: view.setup,
            });
        }
    }
    all
}

/// The sample data comes only from `sample` and the fixed clock `NOW`, in
/// UTC. It reads no environment, user name or store.
fn buffer(shot: &Shot) -> (Buffer, Ground) {
    crate::tz::set(crate::tz::Zone::Utc);
    let events = sample::events(NOW);
    let live = live_view(&events, &events, &events, NOW);
    let mut app = App::new(shot.range);
    app.theme = shot.theme;
    let mut data = Data::build(&events, live, NOW, shot.range, "");
    data.limits = LimitsView::build(
        &sample::sources(NOW),
        &events,
        NOW,
        WeekStart::Monday,
        shot.range,
    );
    app.data = Some(data);
    (shot.setup)(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    terminal.draw(|f| render(f, &app)).unwrap();
    let ground = Ground::new(shot.theme);
    (terminal.backend().buffer().clone(), ground)
}

/// A block glyph as the part of its cell it fills: left, top, width, height
/// in eighths of the cell.
fn block(symbol: &str) -> Option<(u8, u8, u8, u8)> {
    let c = symbol.chars().next()?;
    match c {
        '█' => Some((0, 0, 8, 8)),
        '▀' => Some((0, 0, 8, 4)),
        '▌' => Some((0, 0, 4, 8)),
        '▐' => Some((4, 0, 4, 8)),
        // U+2581 is one eighth tall, U+2587 seven.
        '▁'..='▇' => {
            let eighths = (c as u32 - 0x2580) as u8;
            Some((0, 8 - eighths, 8, eighths))
        }
        _ => None,
    }
}

/// A wide glyph such as an emoji takes two cells.
fn symbol_cells(symbol: &str) -> u16 {
    if symbol.chars().any(|c| c as u32 >= 0x1F000) {
        2
    } else {
        1
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn flush(run: &mut Run, top: f64, out: &mut String) {
    if let Some((x, cells, fill, body, bold)) = run.take() {
        let weight = if bold { " font-weight=\"700\"" } else { "" };
        let _ = write!(
            out,
            "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{fill}\" textLength=\"{:.1}\" \
             lengthAdjust=\"spacingAndGlyphs\"{weight}>{}</text>",
            MARGIN + f64::from(x) * CELL_W,
            top + BASELINE,
            f64::from(cells) * CELL_W,
            escape(&body)
        );
    }
}

/// A box inside the cell at column `x`, in eighths of a cell: left, top,
/// width, height.
fn rect(out: &mut String, x: u16, top: f64, cells: u16, part: (u8, u8, u8, u8), fill: &str) {
    let (l, t, w, h) = part;
    let unit_w = CELL_W / 8.0;
    let unit_h = CELL_H / 8.0;
    let _ = write!(
        out,
        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{fill}\"/>",
        MARGIN + f64::from(x) * CELL_W + f64::from(l) * unit_w,
        top + f64::from(t) * unit_h,
        f64::from(w) * unit_w * f64::from(cells),
        f64::from(h) * unit_h,
    );
}

fn render_row(buf: &Buffer, y: u16, ground: &Ground, out: &mut String) {
    let top = MARGIN + f64::from(y) * CELL_H;
    let mut run: Run = None;
    let mut x = 0;
    while x < COLS {
        let cell = &buf[(x, y)];
        let (fill, paper) = if cell.modifier.contains(Modifier::REVERSED) {
            (
                ground.hex(cell.bg, &ground.bg),
                ground.hex(cell.fg, &ground.fg),
            )
        } else {
            (
                ground.hex(cell.fg, &ground.fg),
                ground.hex(cell.bg, &ground.bg),
            )
        };
        let symbol = cell.symbol();
        let cells = symbol_cells(symbol);
        if paper != ground.bg {
            flush(&mut run, top, out);
            rect(out, x, top, cells, (0, 0, 8, 8), &paper);
        }
        if let Some(part) = block(symbol) {
            flush(&mut run, top, out);
            rect(out, x, top, cells, part, &fill);
        } else if symbol.trim().is_empty() {
            flush(&mut run, top, out);
        } else {
            let bold = cell.modifier.contains(Modifier::BOLD);
            match &mut run {
                Some((rx, rc, rf, body, rb)) if *rf == fill && *rb == bold && *rx + *rc == x => {
                    body.push_str(symbol);
                    *rc += cells;
                }
                _ => {
                    flush(&mut run, top, out);
                    run = Some((x, cells, fill, symbol.to_string(), bold));
                }
            }
        }
        x += cells;
    }
    flush(&mut run, top, out);
}

/// A row's elements share one line; an empty row writes nothing.
fn render_line(buf: &Buffer, y: u16, ground: &Ground, out: &mut String) {
    let start = out.len();
    render_row(buf, y, ground, out);
    if out.len() > start {
        out.push('\n');
    }
}

fn render_svg(buf: &Buffer, ground: &Ground, label: &str) -> String {
    let width = f64::from(COLS) * CELL_W + 2.0 * MARGIN;
    let height = f64::from(ROWS) * CELL_H + 2.0 * MARGIN;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\" role=\"img\" \
         aria-label=\"{label}\">"
    );
    let _ = writeln!(
        out,
        "<rect width=\"100%\" height=\"100%\" rx=\"10\" fill=\"{}\"/>",
        ground.bg
    );
    let _ = writeln!(
        out,
        "<g shape-rendering=\"crispEdges\" font-family=\"{FONT}\" font-size=\"13.5\" \
         xml:space=\"preserve\">"
    );
    for y in 0..ROWS {
        render_line(buf, y, ground, &mut out);
    }
    out.push_str("</g>\n</svg>\n");
    out
}

fn image_path(shot: &Shot) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/images")
        .join(&shot.file)
}

#[test]
fn every_picture_matches_its_committed_svg() {
    for shot in &shots() {
        // Arrange
        let path = image_path(shot);
        let (buf, ground) = buffer(shot);

        // Act
        let svg = render_svg(&buf, &ground, &shot.label);

        // Assert
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &svg).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
        assert_eq!(
            svg, want,
            "docs/images/{} is out of date; rerun with UPDATE_SNAPSHOTS=1 if intended",
            shot.file
        );
    }
}

#[test]
fn a_picture_has_one_line_per_terminal_row() {
    // Arrange
    let all = shots();
    let shot = &all[0];
    let (buf, ground) = buffer(shot);

    // Act
    let svg = render_svg(&buf, &ground, &shot.label);

    // Assert
    let lines = svg.lines().count();
    assert!(
        lines <= usize::from(ROWS) + 10,
        "{lines} lines, the limit is {}",
        ROWS + 10
    );
}

#[test]
fn every_view_has_both_themes() {
    // Arrange
    let all = shots();

    // Act
    let files: Vec<&str> = all.iter().map(|s| s.file.as_str()).collect();

    // Assert
    for view in VIEWS {
        for theme in ["dark", "light"] {
            let want = format!("pbtop/{}-{theme}.svg", view.stem);
            assert!(files.contains(&want.as_str()), "missing {want}");
        }
    }
    assert_eq!(files.len(), VIEWS.len() * 2 + 1);
}

#[test]
fn no_two_views_draw_the_same_screen() {
    // Arrange
    let all = shots();

    // Act
    let drawn: Vec<(String, String)> = all
        .iter()
        .filter(|s| s.file.starts_with("pbtop/"))
        .map(|s| {
            let (buf, ground) = buffer(s);
            (s.file.clone(), render_svg(&buf, &ground, ""))
        })
        .collect();

    // Assert
    for (i, (a, svg_a)) in drawn.iter().enumerate() {
        for (b, svg_b) in &drawn[i + 1..] {
            assert_ne!(svg_a, svg_b, "{a} and {b} draw the same screen");
        }
    }
}

#[test]
fn the_two_themes_of_a_view_differ() {
    // Arrange
    let all = shots();
    let draw = |file: &str| {
        let shot = all.iter().find(|s| s.file == file).unwrap();
        let (buf, ground) = buffer(shot);
        render_svg(&buf, &ground, "")
    };

    // Act
    let dark = draw("pbtop/charts-dark.svg");
    let light = draw("pbtop/charts-light.svg");

    // Assert
    assert_ne!(dark, light);
}
