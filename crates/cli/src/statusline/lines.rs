// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Lines 2 and 3 of the status line: model and context on one, session
//! economics on the other. Each segment is `None` when it has nothing to say.

use super::budget::Segment;
use super::fmt::*;
use crate::json::statusline::value_text;

/// The session fields the segments read, as the script's shell variables.
#[derive(Debug, Default, Clone)]
pub(super) struct Session {
    pub cwd: String,
    pub session_id: String,
    pub model: String,
    pub used: String,
    pub ctx_total_tokens: String,
    pub ctx_window_size: String,
    pub cache_create: String,
    pub cache_read: String,
    pub rl_5h: String,
    pub rl_5h_reset: String,
    pub rl_7d: String,
    pub effort: String,
    pub thinking: String,
    pub cost_usd: String,
    pub wall_ms: String,
}

impl Session {
    pub(super) fn parse(input: &str, pwd: &str) -> Self {
        let mut s = Session::default();
        for (key, value) in crate::json::statusline::session_values(input, pwd) {
            let text = value_text(&value);
            match key {
                "cwd" => s.cwd = text,
                "session_id" => s.session_id = text,
                "model" => s.model = text,
                "used" => s.used = text,
                "ctx_total_tokens" => s.ctx_total_tokens = text,
                "ctx_window_size" => s.ctx_window_size = text,
                "cache_create" => s.cache_create = text,
                "cache_read" => s.cache_read = text,
                "rl_5h" => s.rl_5h = text,
                "rl_5h_reset" => s.rl_5h_reset = text,
                "rl_7d" => s.rl_7d = text,
                "json_effort" => s.effort = text,
                "json_thinking" => s.thinking = text,
                "cost_usd" => s.cost_usd = text,
                "wall_ms" => s.wall_ms = text,
                _ => {}
            }
        }
        s
    }
}

/// Everything the segments need beyond the session itself.
pub(super) struct Opts {
    pub show_model: bool,
    pub show_context: bool,
    pub show_session_age: bool,
    pub show_cache_ratio: bool,
    pub show_rate_limits: bool,
    pub bar_width: i64,
    pub compact_trigger: i64,
    pub home: String,
    pub now: i64,
}

fn render_model(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_model || s.model.is_empty() {
        return None;
    }
    let mut extras = s.effort.clone();
    if s.thinking == "true" {
        if !extras.is_empty() {
            extras.push_str(", ");
        }
        extras.push_str("thinking");
    }
    let mut label = format!("{ORANGE}{}{RESET}", s.model);
    if !extras.is_empty() {
        label.push_str(&format!(" {DIM}({extras}){RESET}"));
    }
    Some(label)
}

fn render_context(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_context || s.used.is_empty() {
        return None;
    }
    let pct = round0(&s.used);
    let color = ctx_color(&s.used);
    let bar = ctx_bar(&s.used, o.bar_width);
    let mut out = format!("{DIM}Ctx{RESET} {color}[{bar}] {pct}%{RESET}");
    if !s.ctx_total_tokens.is_empty() {
        let tok = fmt_tokens(int_part(&s.ctx_total_tokens));
        if s.ctx_window_size.is_empty() {
            out.push_str(&format!(" {DIM}({tok}){RESET}"));
        } else {
            let win = fmt_tokens(int_part(&s.ctx_window_size));
            out.push_str(&format!(" {DIM}({tok}/{win}){RESET}"));
        }
    }
    let gap =
        compact_gap(&s.used, o.compact_trigger).filter(|_| pct.parse::<i64>().unwrap_or(0) > 65);
    match gap {
        Some(0) => out.push_str(&format!(" {DIM}→{RESET} {RED}COMPACTING NEXT TURN{RESET}")),
        Some(g) => out.push_str(&format!(
            " {DIM}→{RESET} {color}{g}%{RESET} {DIM}to compact{RESET}"
        )),
        None => {}
    }
    if !s.ctx_total_tokens.is_empty() && int_part(&s.ctx_total_tokens) >= CONTEXT_ROT_THRESHOLD {
        out.push_str(&format!(" {DIM}·{RESET} {RED}⚠ context rot risk{RESET}"));
    }
    Some(out)
}

/// Raw token count past which model quality is known to degrade.
const CONTEXT_ROT_THRESHOLD: i64 = 200_000;

fn render_session_age(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_session_age || s.session_id.is_empty() {
        return None;
    }
    let file = super::telemetry::runtime_dir(&o.home, &s.session_id).join("start-ts");
    let raw = std::fs::read_to_string(file).ok()?;
    let start = strict_int(raw.trim_end_matches('\n'))?;
    if start <= 0 {
        return None;
    }
    let age = o.now - start;
    (age >= 600).then(|| format!("{DIM}Up{RESET} {GREEN}{}{RESET}", fmt_age(age)))
}

fn render_cache_ratio(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_cache_ratio || (s.cache_create.is_empty() && s.cache_read.is_empty()) {
        return None;
    }
    let pct = cache_hit_pct(int_part(&s.cache_create), int_part(&s.cache_read))?;
    Some(format!(
        "{DIM}Cache{RESET} {}{pct}%{RESET}",
        cache_color(pct)
    ))
}

fn render_cost(s: &Session) -> Option<String> {
    if s.cost_usd.is_empty() {
        return None;
    }
    let cost: f64 = s.cost_usd.trim().parse().unwrap_or(0.0);
    if cost <= 0.0 || cost.is_nan() {
        return None;
    }
    let mut out = format!("{ORANGE}${cost:.4}{RESET}");
    if !s.wall_ms.is_empty() {
        if let Some(rate) = cost_per_min(cost, int_part(&s.wall_ms)) {
            out.push_str(&format!(" {DIM}(${rate}/min){RESET}"));
        }
    }
    Some(out)
}

fn render_rate_5h(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_rate_limits || s.rl_5h.is_empty() {
        return None;
    }
    let mut out = format!(
        "{DIM}5h{RESET} {}{}%{RESET}",
        rl_color(&s.rl_5h),
        round0(&s.rl_5h)
    );
    if !s.rl_5h_reset.is_empty() {
        // A non-integer reset is an arithmetic error in the script, which
        // drops the whole segment rather than only the countdown.
        let reset = strict_int(&s.rl_5h_reset)?;
        let left = reset - o.now;
        if left > 0 {
            out.push_str(&format!(" {DIM}({} left){RESET}", fmt_age(left)));
        }
    }
    Some(out)
}

fn render_rate_7d(s: &Session, o: &Opts) -> Option<String> {
    if !o.show_rate_limits || s.rl_7d.is_empty() {
        return None;
    }
    let v: f64 = s.rl_7d.trim().parse().unwrap_or(0.0);
    (v > 50.0).then(|| {
        format!(
            "{DIM}Rate 7d:{RESET} {}{}%{RESET}",
            rl_color(&s.rl_7d),
            round0(&s.rl_7d)
        )
    })
}

fn join(segments: Vec<Option<String>>) -> Option<String> {
    let parts: Vec<String> = segments.into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(&sep()))
}

/// Line 2: model and context.
pub(super) fn line_2(s: &Session, o: &Opts) -> Option<String> {
    join(vec![render_model(s, o), render_context(s, o)])
}

/// Line 3: cost, cache, 5h quota, session age, 7d rate.
pub(super) fn line_3(s: &Session, o: &Opts) -> Option<String> {
    join(vec![
        render_cost(s),
        render_cache_ratio(s, o),
        render_rate_5h(s, o),
        render_session_age(s, o),
        render_rate_7d(s, o),
    ])
}

/// Columns taken by the ` | ` between two segments, once colour is stripped.
const SEPARATOR_WIDTH: usize = 3;

/// Columns the plain texts take side by side.
fn joined_width(texts: &[&str]) -> usize {
    let gaps = texts.len().saturating_sub(1) * SEPARATOR_WIDTH;
    texts.iter().map(|t| t.chars().count()).sum::<usize>() + gaps
}

/// Line 4: budget projections, fitted to `width` columns. Fitting works on the
/// plain texts, before any colour is added. When the line is too wide the
/// lowest priority segments first take their short form, then the lowest
/// priority segments are dropped. The most important segment always stays.
/// `None` when there is nothing to say.
pub(super) fn line_4(segments: &[Segment], width: usize) -> Option<String> {
    let mut ordered: Vec<&Segment> = segments.iter().collect();
    ordered.sort_by_key(|s| s.priority);
    let mut texts: Vec<&str> = ordered.iter().map(|s| s.text.as_str()).collect();
    let fits = |texts: &[&str]| joined_width(texts) <= width;
    for (i, segment) in ordered.iter().enumerate().rev() {
        if fits(&texts) {
            break;
        }
        if let Some(short) = &segment.short {
            texts[i] = short;
        }
    }
    while texts.len() > 1 && !fits(&texts) {
        texts.pop();
    }
    let parts: Vec<String> = ordered
        .iter()
        .zip(&texts)
        .map(|(segment, text)| {
            let color = if segment.alarm { RED } else { DIM };
            format!("{color}{text}{RESET}")
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(&sep()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::budget::{Segment, PRIORITY_SEVEN_DAY};

    const STAND_IN_PRIORITY: u8 = PRIORITY_SEVEN_DAY + 1;

    fn seven_day() -> Segment {
        Segment {
            text: "! 7d CAP HIT".into(),
            short: None,
            priority: PRIORITY_SEVEN_DAY,
            alarm: true,
        }
    }

    fn stand_in() -> Segment {
        Segment {
            text: "stand-in 12345".into(),
            short: Some("stand-in".into()),
            priority: STAND_IN_PRIORITY,
            alarm: false,
        }
    }

    /// The fitted line without colour, ` | ` between segments.
    fn plain(segments: &[Segment], width: usize) -> Option<String> {
        let line = line_4(segments, width)?;
        let (bytes, _) = percent_b(&line);
        let text = String::from_utf8(bytes).unwrap();
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(start) = rest.find('\x1b') {
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            let end = rest.find('m').unwrap();
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Some(out)
    }

    #[test]
    fn no_segments_make_no_line() {
        assert_eq!(line_4(&[], 120), None);
    }

    #[test]
    fn the_line_shrinks_from_the_lowest_priority_up() {
        // The input order does not matter, priority does.
        let segments = [stand_in(), seven_day()];
        let rows = [
            (120, "! 7d CAP HIT | stand-in 12345"),
            (29, "! 7d CAP HIT | stand-in 12345"),
            (28, "! 7d CAP HIT | stand-in"),
            (23, "! 7d CAP HIT | stand-in"),
            (22, "! 7d CAP HIT"),
            (5, "! 7d CAP HIT"),
            (0, "! 7d CAP HIT"),
        ];
        for (width, want) in rows {
            assert_eq!(plain(&segments, width).as_deref(), Some(want), "{width}");
        }
    }

    #[test]
    fn a_lone_segment_is_never_dropped() {
        assert_eq!(
            plain(&[stand_in()], 3).as_deref(),
            Some("stand-in"),
            "the short form is kept when even it does not fit"
        );
    }

    #[test]
    fn alarms_are_red_and_the_rest_dim() {
        let line = line_4(&[seven_day(), stand_in()], 120).unwrap();
        assert!(
            line.contains(&format!("{RED}! 7d CAP HIT{RESET}")),
            "{line}"
        );
        assert!(
            line.contains(&format!("{DIM}stand-in 12345{RESET}")),
            "{line}"
        );
    }
}
