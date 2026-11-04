// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Contrast and color-blind checks. The method is WCAG 2.x contrast from
//! sRGB relative luminance, and the Machado et al. (2009) simulation at
//! full severity in linear RGB, compared with CIE76 delta E in CIELAB.

use super::theme::{Ink, Rgb};

pub(crate) const TEXT_CONTRAST: f64 = 4.5;
pub(crate) const NON_TEXT_CONTRAST: f64 = 3.0;
pub(crate) const MIN_CVD_DISTANCE: f64 = 10.0;

const PROTAN: [[f64; 3]; 3] = [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
];
const DEUTAN: [[f64; 3]; 3] = [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.011820, 0.042940, 0.968881],
];
const TRITAN: [[f64; 3]; 3] = [
    [1.255528, -0.076749, -0.178779],
    [-0.078411, 0.930809, 0.147602],
    [0.004733, 0.691367, 0.303900],
];

fn linear(c: u8) -> f64 {
    let v = f64::from(c) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
}

pub(crate) fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn simulate(c: Rgb, m: &[[f64; 3]; 3]) -> [f64; 3] {
    let v = [linear(c.0), linear(c.1), linear(c.2)];
    m.map(|row| (row[0] * v[0] + row[1] * v[1] + row[2] * v[2]).clamp(0.0, 1.0))
}

fn lab(linear_rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = linear_rgb;
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn distance(a: Rgb, b: Rgb, m: &[[f64; 3]; 3]) -> f64 {
    let (la, lb) = (lab(simulate(a, m)), lab(simulate(b, m)));
    ((la[0] - lb[0]).powi(2) + (la[1] - lb[1]).powi(2) + (la[2] - lb[2]).powi(2)).sqrt()
}

/// The first pair in `colors` closer than the threshold under any of
/// the three simulations, if any.
pub(crate) fn confusable(colors: &[Rgb]) -> Option<(Rgb, Rgb)> {
    for (i, a) in colors.iter().enumerate() {
        for b in &colors[i + 1..] {
            if [&PROTAN, &DEUTAN, &TRITAN]
                .iter()
                .any(|m| distance(*a, *b, m) < MIN_CVD_DISTANCE)
            {
                return Some((*a, *b));
            }
        }
    }
    None
}

/// The RGB of an ink that must be a true-color one.
pub(crate) fn rgb_of(ink: Ink) -> Rgb {
    match ink {
        Ink::Rgb(rgb) => rgb,
        other => panic!("the Standard theme uses RGB only, got {other:?}"),
    }
}
