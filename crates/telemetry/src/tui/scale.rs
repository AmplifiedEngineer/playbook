// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The scale math of a chart axis: a round top value for the data, and the
//! grid lines from zero to the top at a round step. Pure, so every rule is a
//! unit test.

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

/// The fewest rows between two grid lines, to within the half row the axis
/// loses at the top.
const MIN_LINE_ROWS: usize = 2;
/// The mantissas of a grid step, times a power of ten.
const GRID_MANTISSAS: [f64; 3] = [1.0, 2.0, 5.0];
/// How far a ratio may be above a whole number and still count as it.
const WHOLE_TOLERANCE: f64 = 1e-9;
/// The least gap between two lines in eighths that the rule above allows.
#[cfg(test)]
const MIN_LINE_EIGHTHS: usize = 14;
/// The peak of an empty chart, so it still has a scale.
const EMPTY_PEAK: f64 = 1.0;

/// The vertical axis: zero at the bottom, `max` at the top, a grid line at
/// every multiple of `step` between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub max: f64,
    pub step: f64,
}

impl Axis {
    /// An axis for a plot `rows` high: the smallest step of 1, 2 or 5 times a
    /// power of ten that keeps the lines at least two rows apart, and a top
    /// that is the next multiple of it above `peak`. A plot too short for a
    /// middle line has only zero and the round top.
    pub fn of(peak: f64, rows: u16) -> Axis {
        let peak = if peak.is_finite() && peak > 0.0 {
            peak
        } else {
            EMPTY_PEAK
        };
        // A plot of 14 rows holds seven intervals. The axis is half a row
        // short of the full height, so lines can be a little under two rows
        // apart, never in the same row.
        let intervals = usize::from(rows) / MIN_LINE_ROWS;
        if intervals < 2 {
            let max = nice_ceiling(peak);
            return Axis { max, step: max };
        }
        let start = (peak / intervals as f64).log10().floor() as i32;
        for exp in start.. {
            for mantissa in GRID_MANTISSAS {
                let step = mantissa * 10f64.powi(exp);
                let count = (peak / step * (1.0 - WHOLE_TOLERANCE)).ceil().max(1.0);
                if count as usize <= intervals {
                    return Axis {
                        max: count * step,
                        step,
                    };
                }
            }
        }
        unreachable!("a large enough step always fits")
    }

    /// The values of the grid lines, bottom to top, the last being `max`.
    pub fn ticks(self) -> Vec<f64> {
        let intervals = (self.max / self.step).round() as usize;
        (0..=intervals)
            .map(|k| {
                if k == intervals {
                    self.max
                } else {
                    k as f64 * self.step
                }
            })
            .collect()
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
    fn the_steps_are_one_two_or_five_times_a_power_of_ten() {
        let want = |peak: f64, rows: u16, ticks: &[f64]| {
            let got = Axis::of(peak, rows).ticks();
            assert_eq!(got.len(), ticks.len(), "{peak} in {rows} rows: {got:?}");
            for (g, w) in got.iter().zip(ticks) {
                assert!(
                    (g - w).abs() < w.abs() * 1e-9 + 1e-12,
                    "{got:?} not {ticks:?}"
                );
            }
        };
        want(47.6, 12, &[0.0, 10.0, 20.0, 30.0, 40.0, 50.0]);
        want(
            47.6,
            20,
            &[
                0.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0,
            ],
        );
        want(
            400.0,
            20,
            &[0.0, 50.0, 100.0, 150.0, 200.0, 250.0, 300.0, 350.0, 400.0],
        );
        want(400.0, 12, &[0.0, 100.0, 200.0, 300.0, 400.0]);
        want(100.0, 12, &[0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        want(0.0, 12, &[0.0, 0.2, 0.4, 0.6, 0.8, 1.0]);
    }

    #[test]
    fn a_plot_under_four_rows_keeps_only_zero_and_the_top() {
        assert_eq!(Axis::of(100.0, 1).ticks(), vec![0.0, 100.0]);
        assert_eq!(Axis::of(100.0, 3).ticks(), vec![0.0, 100.0]);
        assert_eq!(Axis::of(2.2, 3).ticks(), vec![0.0, 2.5]);
        assert_eq!(Axis::of(100.0, 4).ticks(), vec![0.0, 50.0, 100.0]);
        assert_eq!(Axis::of(100.0, 5).ticks(), vec![0.0, 50.0, 100.0]);
    }

    #[test]
    fn real_charts_get_dense_round_lines() {
        let close = |got: Vec<f64>, want: &[f64]| {
            assert_eq!(got.len(), want.len(), "{got:?}");
            for (g, w) in got.iter().zip(want) {
                assert!(
                    (g - w).abs() < w.abs() * 1e-9 + 1e-12,
                    "{got:?} not {want:?}"
                );
            }
        };
        // About $350 on 14 rows: a line every $50.
        let spend = Axis::of(347.0, 14);
        assert_eq!(spend.step, 50.0);
        assert_eq!(spend.max, 350.0);
        // About 800M tokens on 11 rows: a line every 200M.
        let tokens = Axis::of(790e6, 11);
        assert_eq!(tokens.step, 200e6);
        close(tokens.ticks(), &[0.0, 200e6, 400e6, 600e6, 800e6]);
        // 1.4B tokens on 11 rows needs 500M: 250M and 800M are not round.
        assert_eq!(Axis::of(1.4e9, 11).step, 500e6);
    }

    #[test]
    fn every_step_is_one_two_or_five_and_lines_keep_two_rows_apart() {
        let mut peak = 0.01;
        while peak < 1e10 {
            for rows in 1..=60u16 {
                let axis = Axis::of(peak, rows);
                let ticks = axis.ticks();
                assert_eq!(ticks[0], 0.0);
                assert_eq!(*ticks.last().unwrap(), axis.max);
                assert!(axis.max >= peak * (1.0 - 1e-9), "{peak} {rows}");
                if ticks.len() == 2 {
                    continue;
                }
                let decade = 10f64.powf(axis.step.log10().floor());
                let mantissa = (axis.step / decade).round();
                assert!(
                    [1.0, 2.0, 5.0].contains(&mantissa),
                    "{peak} {rows}: {axis:?}"
                );
                assert!((axis.step - mantissa * decade).abs() < axis.step * 1e-9);
                for pair in ticks.windows(2) {
                    let (a, b) = (axis.levels(pair[0], rows), axis.levels(pair[1], rows));
                    assert!(b - a + 1 >= MIN_LINE_EIGHTHS, "{peak} {rows}: {ticks:?}");
                }
                // The next smaller step would crowd the lines.
                if rows >= 30 {
                    assert!(ticks.len() >= 6, "{peak} {rows}: {ticks:?}");
                }
            }
            peak *= 1.37;
        }
    }

    #[test]
    fn levels_scale_to_eighths_and_a_small_value_keeps_one() {
        let axis = Axis {
            max: 100.0,
            step: 50.0,
        };
        assert_eq!(axis.levels(100.0, 2), 12);
        assert_eq!(axis.levels(50.0, 2), 6);
        assert_eq!(axis.levels(0.001, 2), 1);
        assert_eq!(axis.levels(0.0, 2), 0);
        assert_eq!(axis.levels(500.0, 2), 12);
        assert_eq!(axis.levels(5.0, 0), 0);
    }

    #[test]
    fn a_label_row_is_the_row_where_a_bar_of_its_value_ends() {
        let axis = Axis {
            max: 100.0,
            step: 50.0,
        };
        // Five rows: 36 eighths at the top, 18 in the middle.
        assert_eq!(axis.levels(100.0, 5), 36);
        assert_eq!(axis.row_of(100.0, 5), 0);
        assert_eq!(axis.row_of(50.0, 5), 2);
        assert_eq!(axis.row_of(0.0, 5), 4);
        assert_eq!(axis.row_of(100.0, 2), 0);
        assert_eq!(axis.row_of(50.0, 2), 1);
        assert_eq!(axis.row_of(0.0, 2), 1);
    }
}
