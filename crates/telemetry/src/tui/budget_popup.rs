// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The window `b` opens to set the manual monthly budget. The amount goes to
//! `usage.budget.monthlyUsd` through `config::save_app`, the same path as the
//! other settings pbtop writes. A detected limit still wins over it.

use super::theme::Palette;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

/// The setting the popup writes.
pub const KEY: &str = "usage.budget.monthlyUsd";

/// What a key did to the popup.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    /// Store this many dollars; 0 clears the budget.
    Save(f64),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BudgetPopup {
    pub text: String,
    pub error: Option<String>,
}

/// Dollars from what was typed. A leading `$` and commas are ignored.
pub fn parse(text: &str) -> Result<f64, String> {
    let cleaned: String = text
        .trim()
        .trim_start_matches('$')
        .chars()
        .filter(|c| *c != ',')
        .collect();
    match cleaned.parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 0.0 => Ok(n),
        _ => Err("Type a number of dollars, 0 or more".to_string()),
    }
}

impl BudgetPopup {
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        self.error = None;
        match code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Enter => match parse(&self.text) {
                Ok(usd) => Outcome::Save(usd),
                Err(why) => {
                    self.error = Some(why);
                    Outcome::Stay
                }
            },
            KeyCode::Backspace => {
                self.text.pop();
                Outcome::Stay
            }
            KeyCode::Char(c) if self.text.chars().count() < 16 => {
                self.text.push(c);
                Outcome::Stay
            }
            _ => Outcome::Stay,
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, popup: &BudgetPopup, pal: &Palette) {
    let width = area.width.saturating_sub(2).min(52);
    let height = 6.min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let accent = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD);
    let plain = Style::default().fg(pal.color(pal.fg));
    let mut lines = vec![
        Line::styled("Manual monthly budget in USD (0 clears it)", plain),
        Line::from(vec![
            Span::styled("$ ", accent),
            Span::styled(format!("{}_", popup.text), plain),
        ]),
    ];
    if let Some(why) = &popup.error {
        lines.push(Line::styled(
            why.clone(),
            Style::default().fg(pal.color(pal.error)),
        ));
    }
    let edge = Style::default().fg(pal.color(pal.hi));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(Line::styled("\u{2524} Budget \u{251c}", edge))
        .title_bottom(Line::styled(
            "\u{2524} Enter saves, Esc cancels \u{251c}",
            edge,
        ));
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dollar_sign_and_commas_are_ignored() {
        assert_eq!(parse("$1,000"), Ok(1000.0));
        assert_eq!(parse(" 250.5 "), Ok(250.5));
        assert_eq!(parse("0"), Ok(0.0));
    }

    #[test]
    fn text_that_is_not_a_non_negative_number_is_refused() {
        for bad in ["", "abc", "-5", "NaN", "inf"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn enter_saves_a_good_amount_and_keeps_the_popup_on_a_bad_one() {
        let mut p = BudgetPopup::default();
        p.key(KeyCode::Char('x'));
        assert_eq!(p.key(KeyCode::Enter), Outcome::Stay);
        assert!(p.error.is_some());
        p.key(KeyCode::Backspace);
        for c in "1000".chars() {
            p.key(KeyCode::Char(c));
        }
        assert_eq!(p.key(KeyCode::Enter), Outcome::Save(1000.0));
        assert_eq!(BudgetPopup::default().key(KeyCode::Esc), Outcome::Close);
    }
}
