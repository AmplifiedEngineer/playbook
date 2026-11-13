// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Number and text formatting for the terminal view.

/// `$1,234.56`.
pub fn money(usd: f64) -> String {
    let cents = (usd.abs() * 100.0).round() as u64;
    let sign = if usd < 0.0 && cents > 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", grouped(cents / 100), cents % 100)
}

/// `$1,235`: rounded to the whole dollar.
pub fn money_whole(usd: f64) -> String {
    let dollars = usd.abs().round() as u64;
    let sign = if usd < 0.0 && dollars > 0 { "-" } else { "" };
    format!("{sign}${}", grouped(dollars))
}

/// `1,234,567`: digits in groups of three.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `950`, `1.2K`, `3.4M`, `5.6B`.
pub fn compact(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];
    for (size, suffix) in UNITS {
        if n >= size {
            return format!("{:.1}{suffix}", n as f64 / size as f64);
        }
    }
    n.to_string()
}

/// `+$1.20`, `-$0.50`, or `$0.00` when the change rounds to no cents.
pub fn signed_money(usd: f64) -> String {
    match money(usd.abs()) {
        zero if zero == "$0.00" => zero,
        text if usd < 0.0 => format!("-{text}"),
        text => format!("+{text}"),
    }
}

/// `+1.5K`, `-300`, or `0`.
pub fn signed_compact(n: f64) -> String {
    let abs = n.abs().round() as u64;
    match abs {
        0 => "0".to_string(),
        _ if n < 0.0 => format!("-{}", compact(abs)),
        _ => format!("+{}", compact(abs)),
    }
}

/// `+12`, `-3`, or `0`.
pub fn signed_count(n: f64) -> String {
    match n.round() as i64 {
        0 => "0".to_string(),
        c => format!("{c:+}"),
    }
}

/// `0%`, `12%`, `3.5%`: a share from 0 to 1.
pub fn percent(fraction: f64) -> String {
    let p = fraction * 100.0;
    if p == 0.0 {
        "0%".to_string()
    } else if p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// `507ms`, `1.2s`, `2m05s`, `1h03m`.
pub fn duration_ms(ms: f64) -> String {
    let ms = ms.max(0.0);
    if ms < 1_000.0 {
        return format!("{ms:.0}ms");
    }
    let secs = ms / 1_000.0;
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    let total = secs.round() as u64;
    if total < 3_600 {
        format!("{}m{:02}s", total / 60, total % 60)
    } else {
        format!("{}h{:02}m", total / 3_600, total % 3_600 / 60)
    }
}

/// `+21.6%`, `-4.0%`, `+250%`. With no previous value there is no percent: a
/// current value that grew from nothing reads `new`, nothing against nothing `-`.
pub fn signed_percent(percent: Option<f64>, grew: bool) -> String {
    match percent {
        None if grew => "new".to_string(),
        None => "-".to_string(),
        Some(p) if p.abs() < 0.05 => "0%".to_string(),
        Some(p) if p.abs() >= 100.0 => format!("{p:+.0}%"),
        Some(p) => format!("{p:+.1}%"),
    }
}

/// A number with at most three decimals and no trailing zeros: `2.5`, `0.05`.
fn trimmed(n: f64) -> String {
    let text = format!("{n:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// An axis label in dollars: `$0`, `$0.25`, `$12.5`, `$250`, `$2.5K`.
pub fn axis_money(usd: f64) -> String {
    format!("${}", axis_number(usd))
}

/// An axis label in tokens: `0`, `250`, `500K`, `2.5M`.
pub fn axis_tokens(n: f64) -> String {
    axis_number(n)
}

fn axis_number(n: f64) -> String {
    const UNITS: [(f64, &str); 3] = [(1e9, "B"), (1e6, "M"), (1e3, "K")];
    for (size, suffix) in UNITS {
        if n >= size * 0.9995 {
            return format!("{}{suffix}", trimmed(n / size));
        }
    }
    if n >= 100.0 {
        format!("{n:.0}")
    } else {
        trimmed(n)
    }
}

/// `Mon` to `Sun` for a weekday index with Monday as 0.
pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A horizontal bar `width` cells wide, filled to `fraction` (0 to 1).
pub fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!(
        "{}{}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(width - filled)
    )
}

/// Cuts `text` to `width` characters, ending with `~` when it was cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    if width > 0 {
        out.push('~');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_groups_thousands_and_rounds_cents() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(1234.567), "$1,234.57");
        assert_eq!(money(1_000_000.0), "$1,000,000.00");
        assert_eq!(money(-0.001), "$0.00");
        assert_eq!(money(-5.5), "-$5.50");
    }

    #[test]
    fn whole_money_groups_thousands_and_rounds_to_the_dollar() {
        assert_eq!(money_whole(0.0), "$0");
        assert_eq!(money_whole(986.4), "$986");
        assert_eq!(money_whole(1500.0), "$1,500");
        assert_eq!(money_whole(1_234_567.5), "$1,234,568");
        assert_eq!(money_whole(-0.4), "$0");
        assert_eq!(money_whole(-1234.0), "-$1,234");
    }

    #[test]
    fn changes_carry_their_sign_and_no_sign_for_nothing() {
        assert_eq!(signed_money(1.2), "+$1.20");
        assert_eq!(signed_money(-0.5), "-$0.50");
        assert_eq!(signed_money(0.001), "$0.00");
        assert_eq!(signed_money(-0.001), "$0.00");
        assert_eq!(signed_compact(1_500.0), "+1.5K");
        assert_eq!(signed_compact(-300.0), "-300");
        assert_eq!(signed_compact(0.2), "0");
        assert_eq!(signed_count(12.0), "+12");
        assert_eq!(signed_count(-3.0), "-3");
        assert_eq!(signed_count(0.0), "0");
    }

    #[test]
    fn a_percent_change_says_new_when_there_was_nothing_before() {
        assert_eq!(signed_percent(Some(21.64), true), "+21.6%");
        assert_eq!(signed_percent(Some(-4.0), true), "-4.0%");
        assert_eq!(signed_percent(Some(250.0), true), "+250%");
        assert_eq!(signed_percent(Some(0.01), true), "0%");
        assert_eq!(signed_percent(None, true), "new");
        assert_eq!(signed_percent(None, false), "-");
    }

    #[test]
    fn compact_uses_one_decimal_units() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(1_500), "1.5K");
        assert_eq!(compact(3_400_000), "3.4M");
        assert_eq!(compact(5_600_000_000), "5.6B");
    }

    #[test]
    fn axis_labels_are_short_and_round() {
        for (n, money, tokens) in [
            (0.0, "$0", "0"),
            (0.05, "$0.05", "0.05"),
            (0.25, "$0.25", "0.25"),
            (1.25, "$1.25", "1.25"),
            (12.5, "$12.5", "12.5"),
            (250.0, "$250", "250"),
            (999.9, "$1K", "1K"),
            (2_500.0, "$2.5K", "2.5K"),
            (500_000.0, "$500K", "500K"),
            (2_500_000.0, "$2.5M", "2.5M"),
            (1_000_000_000.0, "$1B", "1B"),
        ] {
            assert_eq!(axis_money(n), money, "{n}");
            assert_eq!(axis_tokens(n), tokens, "{n}");
        }
    }

    #[test]
    fn bar_fills_in_proportion_and_clamps() {
        assert_eq!(bar(0.5, 4), "\u{2588}\u{2588}\u{2591}\u{2591}");
        assert_eq!(bar(2.0, 2), "\u{2588}\u{2588}");
        assert_eq!(bar(-1.0, 2), "\u{2591}\u{2591}");
    }

    #[test]
    fn fit_marks_a_cut() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("abcdefgh", 5), "abcd~");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn percent_and_durations_read_at_a_glance() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.035), "3.5%");
        assert_eq!(percent(0.12), "12%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(duration_ms(507.0), "507ms");
        assert_eq!(duration_ms(1_234.0), "1.2s");
        assert_eq!(duration_ms(125_000.0), "2m05s");
        assert_eq!(duration_ms(3_780_000.0), "1h03m");
        assert_eq!(duration_ms(-4.0), "0ms");
    }
}
