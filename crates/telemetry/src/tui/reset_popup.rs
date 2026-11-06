// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The window `X` opens to ask before the usage history is deleted. Only `y`
//! confirms; every other key cancels, so one stray key never deletes it.

use super::theme::Palette;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// The keys that confirm: `Y` too, so Caps Lock does not make the answer miss.
const CONFIRM: [KeyCode; 2] = [KeyCode::Char('y'), KeyCode::Char('Y')];

const WIDTH: u16 = 60;
const HEIGHT: u16 = 8;

/// What the window says, one line each.
const BODY: [&str; 4] = [
    "Delete the stored usage history and read it again from the",
    "session transcripts, as on a first run?",
    "The budget, the limit readings, the account names and the",
    "layout stay.",
];

/// True when the key is a yes; any other key cancels.
pub fn confirms(code: KeyCode) -> bool {
    CONFIRM.contains(&code)
}

pub fn render(frame: &mut Frame, area: Rect, pal: &Palette) {
    let width = area.width.saturating_sub(2).min(WIDTH);
    let height = HEIGHT.min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let plain = Style::default().fg(pal.color(pal.fg));
    let edge = Style::default()
        .fg(pal.color(pal.error))
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = BODY.iter().map(|l| Line::styled(*l, plain)).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::styled("\u{2524} Reset usage \u{251c}", edge))
        .title_bottom(Line::styled(
            "\u{2524} y deletes, any other key cancels \u{251c}",
            edge,
        ));
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_confirms_in_either_case_and_nothing_else() {
        assert!(confirms(KeyCode::Char('y')));
        assert!(confirms(KeyCode::Char('Y')));
        for other in [
            KeyCode::Char('n'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('X'),
        ] {
            assert!(!confirms(other), "{other:?}");
        }
    }
}
