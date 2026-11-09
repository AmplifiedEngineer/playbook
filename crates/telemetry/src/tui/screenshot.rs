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
use super::data::Data;
use super::snapshots::fixture_at;
use super::theme::{ColorDepth, Theme};
use super::view::render;
use crate::query::{live_view, Range};
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

/// Page and default text colors, read from the theme.
struct Ground {
    bg: String,
    fg: String,
}

/// One run of same-styled text on a row: first cell, cell count, fill, text, bold.
type Run = Option<(u16, u16, String, String, bool)>;

/// One picture: the file path under `docs/images`, the theme and range to
/// draw, the text for screen readers, and a step that sets the public `App`
/// fields to reach the view.
struct Shot {
    file: &'static str,
    label: &'static str,
    theme: Theme,
    range: Range,
    setup: fn(&mut App),
}

fn no_setup(_: &mut App) {}

const SHOTS: &[Shot] = &[Shot {
    file: "pbtop-month.svg",
    label: "pbtop monthly charts view with sample data",
    theme: Theme::Standard,
    range: Range::CurrentMonth,
    setup: no_setup,
}];

/// The sample data comes only from `fixture_at` and the fixed clock `NOW`, in
/// UTC. It reads no environment, user name or store.
fn buffer(shot: &Shot) -> (Buffer, Ground) {
    crate::tz::set(crate::tz::Zone::Utc);
    let events = fixture_at(NOW);
    let live = live_view(&events, &events, &events, NOW);
    let mut app = App::new(shot.range);
    app.theme = shot.theme;
    app.data = Some(Data::build(&events, live, NOW, shot.range, ""));
    (shot.setup)(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    terminal.draw(|f| render(f, &app)).unwrap();
    let pal = shot.theme.palette(ColorDepth::True);
    let ground = Ground {
        bg: hex(pal.color(pal.bg), "#000000"),
        fg: hex(pal.color(pal.fg), "#cccccc"),
    };
    (terminal.backend().buffer().clone(), ground)
}

fn hex(color: Color, fallback: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => fallback.to_string(),
    }
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
            (hex(cell.bg, &ground.bg), hex(cell.fg, &ground.fg))
        } else {
            (hex(cell.fg, &ground.fg), hex(cell.bg, &ground.bg))
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
        .join(shot.file)
}

#[test]
fn every_picture_matches_its_committed_svg() {
    for shot in SHOTS {
        // Arrange
        let path = image_path(shot);
        let (buf, ground) = buffer(shot);

        // Act
        let svg = render_svg(&buf, &ground, shot.label);

        // Assert
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
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
    let shot = &SHOTS[0];
    let (buf, ground) = buffer(shot);

    // Act
    let svg = render_svg(&buf, &ground, shot.label);

    // Assert
    let lines = svg.lines().count();
    assert!(
        lines <= usize::from(ROWS) + 10,
        "{lines} lines, the limit is {}",
        ROWS + 10
    );
}
