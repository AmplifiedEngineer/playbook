// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The scale math of a chart axis: a round top value for the data, and the
//! labels at the bottom, middle and top. Pure, so every rule is a unit test.

/// The mantissas an axis top can take, times a power of ten. Half of each is
/// still a round number, so the middle label reads cleanly.
const STEPS: [f64; 11] = [1.0, 1.2, 1.6, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0];

/// The smallest round value that is at least `value`: 1, 1.2, 1.6, 2, 2.5, 3, 4, 5, 6
/// or 8 times a power of ten. Zero, a negative or a value that is not a number
/// gives 1, so an empty chart still has a scale.
pub fn nice_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let exp = value.log10().floor();
    let scale = 10f64.powf(exp);
    let mantissa = value / scale;
    // The small allowance keeps a value that is already round (0.3 / 0.1 is
    // 2.9999999999999996) from jumping to the next step.
    let step = STEPS
        .iter()
        .copied()
        .find(|s| *s >= mantissa * (1.0 - 1e-9))
        .unwrap_or(10.0);
    step * scale
}

/// Eighths of a cell in one row of the plot.
pub const ROW_EIGHTHS: usize = 8;
/// Half a row in eighths. An axis label sits on the middle of its text row,
/// so the top of the scale is the middle of the top row, not its edge.
pub const HALF_ROW: usize = ROW_EIGHTHS / 2;

/// The vertical axis: zero at the bottom, `max` at the top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub max: f64,
}

impl Axis {
    /// An axis whose top is the round value above `peak`.
    pub fn of(peak: f64) -> Axis {
        Axis {
            max: nice_ceiling(peak),
        }
    }

    /// The values at the bottom, middle and top.
    pub fn ticks(self) -> [f64; 3] {
        [0.0, self.max / 2.0, self.max]
    }

    /// Eighths of a cell a value fills in a plot `rows` cells high. Zero is
    /// the bottom edge of the bottom row and `max` is the middle of the top
    /// row, where its label is. Anything above zero shows at least one eighth,
    /// so a small day is never blank.
    pub fn levels(self, value: f64, rows: u16) -> usize {
        let total = (usize::from(rows) * ROW_EIGHTHS).saturating_sub(HALF_ROW);
        if value <= 0.0 || !value.is_finite() || total == 0 {
            return 0;
        }
        (((value / self.max) * total as f64).round() as usize).clamp(1, total)
    }

    /// The row, counted from the top of a plot `rows` high, where a bar of
    /// `value` ends. A label for that value goes on this row.
    pub fn row_of(self, value: f64, rows: u16) -> u16 {
        let top_of_bar = self.levels(value, rows).saturating_sub(1) / ROW_EIGHTHS;
        rows.saturating_sub(1)
            .saturating_sub(u16::try_from(top_of_bar).unwrap_or(u16::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_is_the_next_round_value() {
        for (value, want) in [
            (0.0, 1.0),
            (-4.0, 1.0),
            (f64::NAN, 1.0),
            (0.7, 0.8),
            (1.0, 1.0),
            (1.01, 1.2),
            (1.3, 1.6),
            (1.7, 2.0),
            (2.2, 2.5),
            (2.6, 3.0),
            (4.1, 5.0),
            (6.0, 6.0),
            (6.01, 8.0),
            (8.5, 10.0),
            (10.0, 10.0),
            (47.6, 50.0),
            (123.0, 160.0),
            (6_000.0, 6_000.0),
            (47_600.0, 50_000.0),
            (1_234_567.0, 1_600_000.0),
        ] {
            let got = nice_ceiling(value);
            assert!(
                (got - want).abs() < want * 1e-9,
                "{value}: {got} not {want}"
            );
        }
    }

    #[test]
    fn a_value_that_is_already_round_stays_put_despite_float_noise() {
        // 0.3 / 0.1 is 2.9999999999999996 in floating point.
        assert!((nice_ceiling(0.3) - 0.3).abs() < 1e-12);
        assert!((nice_ceiling(0.06) - 0.06).abs() < 1e-12);
    }

    #[test]
    fn the_ceiling_never_undershoots_and_never_overshoots_by_more_than_a_third() {
        let mut v = 0.001;
        while v < 1e9 {
            let c = nice_ceiling(v);
            assert!(c >= v * (1.0 - 1e-9), "{v} -> {c}");
            assert!(c <= v * 1.34, "{v} -> {c}");
            v *= 1.07;
        }
    }

    #[test]
    fn the_middle_tick_is_half_the_top() {
        let axis = Axis::of(47.6);
        assert_eq!(axis.ticks(), [0.0, 25.0, 50.0]);
        assert_eq!(Axis::of(0.0).ticks(), [0.0, 0.5, 1.0]);
    }

    #[test]
    fn levels_scale_to_eighths_and_a_small_value_keeps_one() {
        let axis = Axis { max: 100.0 };
        assert_eq!(axis.levels(100.0, 2), 12);
        assert_eq!(axis.levels(50.0, 2), 6);
        assert_eq!(axis.levels(0.001, 2), 1);
        assert_eq!(axis.levels(0.0, 2), 0);
        assert_eq!(axis.levels(500.0, 2), 12);
        assert_eq!(axis.levels(5.0, 0), 0);
    }

    #[test]
    fn a_label_row_is_the_row_where_a_bar_of_its_value_ends() {
        let axis = Axis { max: 100.0 };
        // Five rows: 36 eighths at the top, 18 in the middle.
        assert_eq!(axis.levels(100.0, 5), 36);
        assert_eq!(axis.row_of(100.0, 5), 0);
        assert_eq!(axis.row_of(50.0, 5), 2);
        assert_eq!(axis.row_of(0.0, 5), 4);
        assert_eq!(axis.row_of(100.0, 1), 0);
    }
}
