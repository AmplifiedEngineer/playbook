// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The layout editor window: the edited tab drawn as a map of its rows, or the
//! picker for adding a panel, over the screen.

use super::arrange;
use super::editor::Editor;
use super::icon::{Cue, Icon};
use super::theme::Palette;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

/// Lines for the map or the picker, so the window keeps its size.
const BODY_LINES: usize = 9;
/// Picker lines shown at once, under its heading.
const PICKER_LINES: usize = BODY_LINES - 1;
/// Tab line, rename line, body, description, message, three key lines and the two border lines.
const WINDOW_LINES: u16 = (BODY_LINES + 9) as u16;

pub fn render(frame: &mut Frame, area: Rect, ed: &Editor, pal: &Palette, icon: Icon) {
    let width = area.width.saturating_sub(2).min(78);
    let height = WINDOW_LINES.min(area.height.saturating_sub(2));
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
    let inner = usize::from(width).saturating_sub(2);
    let mut lines = vec![tab_line(ed, inner, accent, plain)];
    lines.push(match &ed.rename {
        Some(name) => Line::styled(
            format!(" New name: {name}_   enter apply   esc cancel"),
            accent,
        ),
        None => Line::raw(""),
    });
    let mut body = match ed.picker {
        Some(at) => picker_lines(ed, at, accent, plain),
        None => map_lines(ed, inner, accent, plain),
    };
    body.resize(BODY_LINES, Line::raw(""));
    lines.extend(body);
    lines.push(Line::styled(
        format!(" {}", describe_cursor(ed)),
        Style::default().fg(pal.color(pal.graph_text)),
    ));
    lines.push(match &ed.message {
        _ if ed.confirm_reset => Line::styled(
            " Reset layout and view settings to the defaults? [y/N]",
            accent,
        ),
        Some(m) => Line::styled(
            format!(" {}{m}", icon.mark(Cue::Alert)),
            Style::default()
                .fg(pal.color(pal.error))
                .add_modifier(Modifier::BOLD),
        ),
        None => Line::raw(""),
    });
    for keys in [
        " Left/Right tab   h/l j/k move cursor   H/J/K/L move panel   f width",
        " a add   d remove   u undo   < > move tab   n new   x delete   r rename",
        " R reset   esc or q close   changes are saved as you make them",
    ] {
        lines.push(Line::styled(keys, plain));
    }
    let edge = Style::default().fg(pal.color(pal.hi));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::styled(
            "\u{2524} Layout: tabs and panels \u{251c}",
            edge,
        ));
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn tab_line(ed: &Editor, inner: usize, accent: Style, plain: Style) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let full: usize = ed
        .layout
        .tabs
        .iter()
        .map(|t| t.name.chars().count() + 2)
        .sum();
    let names = full < inner.saturating_sub(2);
    for (i, tab) in ed.layout.tabs.iter().enumerate() {
        let shown = if names || i == ed.tab {
            format!(" {} ", tab.name)
        } else {
            format!(" {} ", i + 1)
        };
        spans.push(if i == ed.tab {
            Span::styled(shown, accent.add_modifier(Modifier::UNDERLINED))
        } else {
            Span::styled(shown, plain)
        });
    }
    Line::from(spans)
}

/// What the panel under the cursor shows.
fn describe_cursor(ed: &Editor) -> String {
    ed.layout
        .tabs
        .get(ed.tab)
        .and_then(|t| t.panels.get(ed.slot))
        .map_or(String::new(), |p| format!("{}: {}", p.word(), p.describe()))
}

/// One line per row of the tab, panels side by side as the screen draws them.
/// The budget strip, which no row holds, comes first because the screen draws
/// it on top. The cursor cell is bold and starts with `>`, so it shows without color.
fn map_lines(ed: &Editor, inner: usize, accent: Style, plain: Style) -> Vec<Line<'static>> {
    let Some(tab) = ed.layout.tabs.get(ed.tab) else {
        return Vec::new();
    };
    let total = inner.saturating_sub(2);
    let rows = arrange::grid(tab);
    let drawn: Vec<usize> = rows.iter().flatten().copied().collect();
    let strip = (0..tab.panels.len()).filter(|i| !drawn.contains(i));
    let line = |slots: &[usize]| {
        let count = slots.len();
        let mut spans = vec![Span::raw(" ")];
        for (col, slot) in slots.iter().enumerate() {
            let width = if col + 1 < count {
                total / count
            } else {
                total - total / count * (count - 1)
            };
            let selected = *slot == ed.slot;
            let word = tab.panels.get(*slot).map_or("", |p| p.word());
            spans.push(Span::styled(
                cell(word, selected, width),
                if selected { accent } else { plain },
            ));
        }
        Line::from(spans)
    };
    strip
        .map(|i| line(&[i]))
        .chain(rows.iter().map(|slots| line(slots)))
        .collect()
}

/// `[> word    ]` padded or cut to `width` columns.
fn cell(word: &str, selected: bool, width: usize) -> String {
    let marker = if selected { '>' } else { ' ' };
    let room = width.saturating_sub(2);
    let body: String = format!("{marker} {word}").chars().take(room).collect();
    format!("[{body:<room$}]")
}

/// The panels the tab does not show, the chosen one marked, scrolled to keep it in view.
fn picker_lines(ed: &Editor, at: usize, accent: Style, plain: Style) -> Vec<Line<'static>> {
    let choices = ed.available();
    let at = at.min(choices.len().saturating_sub(1));
    let start = (at + 1).saturating_sub(PICKER_LINES);
    let after = ed
        .layout
        .tabs
        .get(ed.tab)
        .and_then(|t| t.panels.get(arrange::insert_point(t).checked_sub(1)?))
        .map_or("", |p| p.word());
    let mut lines = vec![Line::styled(
        format!(" Add after {after}   enter add   esc back   j/k choose"),
        accent,
    )];
    lines.extend(
        choices
            .iter()
            .enumerate()
            .skip(start)
            .take(PICKER_LINES)
            .map(|(i, id)| {
                let pointer = if i == at { ">" } else { " " };
                let text = format!(" {pointer} {:<18} {}", id.word(), id.describe());
                Line::styled(text, if i == at { accent } else { plain })
            }),
    );
    lines
}
