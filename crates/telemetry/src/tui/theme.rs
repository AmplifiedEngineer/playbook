// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Colors for the terminal view. `t` cycles the themes. `Mono` uses no color
//! at all, for terminals and pipes that cannot show it.
//!
//! The `default` theme is the default. It uses the Okabe and Ito (2008)
//! color universal design palette on black, so it stays readable with common
//! color vision differences. Severity is not a green to red gradient: value
//! ramps run one hue from dark to light, and state is also shown by glyphs.
//! The opt-in `spectrum` theme is the exception: its usage color runs the
//! full HSV hue wheel from green to red, on the same black ground.
//! A two digit hex is a gray (`#cc` is 204, 204, 204), and a graph or meter
//! colors a value from a start, mid and end color. The palette is a public
//! scientific one, cited here and not copied from other software.

use ratatui::style::Color;

/// A color as hue (0 to 360 degrees), saturation and value (0 to 100 percent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hsv {
    pub h: u16,
    pub s: u8,
    pub v: u8,
}

impl Hsv {
    pub const fn new(h: u16, s: u8, v: u8) -> Hsv {
        Hsv { h, s, v }
    }

    pub fn rgb(self) -> Rgb {
        hsv_to_rgb(
            f64::from(self.h),
            f64::from(self.s) / 100.0,
            f64::from(self.v) / 100.0,
        )
    }

    /// The color `t` of the way (0 to 1) from `self` to `to`. The hue takes
    /// the shorter way round the wheel.
    fn mix(self, to: Hsv, t: f64) -> Rgb {
        let (a, b) = (f64::from(self.h), f64::from(to.h));
        let turn = (b - a + 540.0).rem_euclid(360.0) - 180.0;
        let by = |x: u8, y: u8| f64::from(x) + (f64::from(y) - f64::from(x)) * t;
        hsv_to_rgb(
            a + turn * t,
            by(self.s, to.s) / 100.0,
            by(self.v, to.v) / 100.0,
        )
    }
}

/// HSV to RGB. `h` is in degrees and wraps (360 is 0), `s` and `v` run 0 to 1
/// and are clamped.
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Rgb {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |n: f64| ((n + m) * 255.0).round() as u8;
    Rgb(byte(r), byte(g), byte(b))
}

/// Usage thresholds, in percent of the limit. The usage color holds its calm
/// stop below `USAGE_RISING`, moves to the next stop at each threshold, and
/// stays at the alarm stop from `USAGE_LIMIT` up.
pub const USAGE_RISING: f64 = 50.0;
pub const USAGE_HIGH: f64 = 70.0;
pub const USAGE_CRITICAL: f64 = 90.0;
pub const USAGE_LIMIT: f64 = 100.0;

/// How the burn-up line and fill color a usage percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// Four stops (calm, high, critical, alarm) that blend between the
    /// thresholds.
    Smooth([Hsv; 4]),
    /// Four inks that change at each threshold, for the 16 color themes.
    Steps([Ink; 4]),
}

impl Usage {
    pub fn at(self, pct: f64) -> Ink {
        match self {
            Usage::Smooth(stops) => {
                let (from, to, t) = if pct < USAGE_RISING {
                    return Ink::Rgb(stops[0].rgb());
                } else if pct < USAGE_HIGH {
                    (0, 1, (pct - USAGE_RISING) / (USAGE_HIGH - USAGE_RISING))
                } else if pct < USAGE_CRITICAL {
                    (1, 2, (pct - USAGE_HIGH) / (USAGE_CRITICAL - USAGE_HIGH))
                } else if pct < USAGE_LIMIT {
                    (
                        2,
                        3,
                        (pct - USAGE_CRITICAL) / (USAGE_LIMIT - USAGE_CRITICAL),
                    )
                } else {
                    return Ink::Rgb(stops[3].rgb());
                };
                Ink::Rgb(stops[from].mix(stops[to], t))
            }
            Usage::Steps(inks) => {
                let step = if pct < USAGE_RISING {
                    0
                } else if pct < USAGE_HIGH {
                    1
                } else if pct < USAGE_CRITICAL {
                    2
                } else {
                    3
                };
                inks[step]
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Standard,
    /// The default palette with a green to red usage gradient. Opt-in.
    Spectrum,
    Dark,
    Light,
    Mono,
}

/// How many colors the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    #[default]
    True,
    Ansi256,
    Ansi16,
}

impl ColorDepth {
    /// From `COLORTERM` and `TERM`: true color when `COLORTERM` says so, 256
    /// colors when `TERM` names them, else the 16 basic ones.
    pub fn detect(colorterm: Option<&str>, term: Option<&str>) -> ColorDepth {
        if matches!(colorterm, Some("truecolor" | "24bit")) {
            ColorDepth::True
        } else if term.is_some_and(|t| t.contains("256color")) {
            ColorDepth::Ansi256
        } else {
            ColorDepth::Ansi16
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A color role value: the terminal's own color, a named 16 color, or an RGB
/// value that is reduced to the terminal's depth when drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Default,
    Named(Color),
    Rgb(Rgb),
}

/// What a graph or meter paints with: one color, or a three point
/// gradient (start at 0%, mid at 50%, end at 100%).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Flat(Ink),
    Gradient(Rgb, Rgb, Rgb),
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u8
}

impl Fill {
    /// The color at `pct` percent (clamped to 0 to 100).
    pub fn at(self, pct: f64) -> Ink {
        match self {
            Fill::Flat(ink) => ink,
            Fill::Gradient(start, mid, end) => {
                let pct = pct.clamp(0.0, 100.0);
                let (a, b, t) = if pct <= 50.0 {
                    (start, mid, pct / 50.0)
                } else {
                    (mid, end, (pct - 50.0) / 50.0)
                };
                Ink::Rgb(Rgb(lerp(a.0, b.0, t), lerp(a.1, b.1, t), lerp(a.2, b.2, t)))
            }
        }
    }
}

/// Every color the view uses, by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub depth: ColorDepth,
    pub mono: bool,
    pub bg: Ink,
    pub fg: Ink,
    pub title: Ink,
    /// Panel number hints and the accent.
    pub hi: Ink,
    pub sel_bg: Ink,
    pub sel_fg: Ink,
    pub inactive: Ink,
    pub graph_text: Ink,
    pub meter_bg: Ink,
    pub div_line: Ink,
    pub box_cpu: Ink,
    pub box_mem: Ink,
    pub box_net: Ink,
    pub box_proc: Ink,
    pub cpu: Fill,
    pub down: Fill,
    pub cached: Fill,
    pub avail: Fill,
    pub process: Fill,
    pub live: Ink,
    /// The middle stop of a green, yellow, red ramp; `warn` is orange here.
    pub caution: Ink,
    pub warn: Ink,
    pub error: Ink,
    /// The burn-up line and fill by percent of the limit.
    pub usage: Usage,
    /// One color per series, for what is told apart by name (a model).
    pub series: [Ink; SERIES],
}

/// How many series colors a palette has. More series share them.
pub const SERIES: usize = 8;

const fn gray(v: u8) -> Rgb {
    Rgb(v, v, v)
}

const fn hex(rgb: u32) -> Rgb {
    Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Okabe and Ito (2008) sky blue and orange, the two hues of the value ramps.
const SKY: Rgb = hex(0x56b4e9);
const ORANGE: Rgb = hex(0xe69f00);

const fn ink(rgb: Rgb) -> Ink {
    Ink::Rgb(rgb)
}

impl Theme {
    /// Every theme, in the order `next` visits them.
    pub const ALL: [Theme; 5] = [
        Theme::Standard,
        Theme::Spectrum,
        Theme::Dark,
        Theme::Light,
        Theme::Mono,
    ];

    pub fn next(self) -> Theme {
        match self {
            Theme::Standard => Theme::Spectrum,
            Theme::Spectrum => Theme::Dark,
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Mono,
            Theme::Mono => Theme::Standard,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Standard => "default",
            Theme::Spectrum => "spectrum",
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::Mono => "mono",
        }
    }

    pub fn parse(name: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.name() == name)
    }

    pub fn palette(self, depth: ColorDepth) -> Palette {
        match self {
            Theme::Standard => Palette {
                depth,
                mono: false,
                bg: ink(gray(0x00)),
                fg: ink(gray(0xcc)),
                title: ink(gray(0xee)),
                hi: ink(SKY),
                sel_bg: ink(hex(0x1f4e79)),
                sel_fg: ink(gray(0xff)),
                inactive: ink(gray(0x76)),
                graph_text: ink(gray(0x90)),
                meter_bg: ink(gray(0x6b)),
                div_line: ink(gray(0x6b)),
                box_cpu: ink(hex(0x3f89b8)),
                box_mem: ink(hex(0xb07d00)),
                box_net: ink(hex(0xa0678a)),
                box_proc: ink(gray(0x8a)),
                cpu: Fill::Gradient(hex(0x1b6fb5), SKY, hex(0xb8e2f8)),
                down: Fill::Gradient(hex(0x1b6fb5), hex(0x3f89b8), hex(0xb8e2f8)),
                cached: Fill::Gradient(hex(0x1b6fb5), SKY, hex(0xb8e2f8)),
                avail: Fill::Gradient(hex(0xb07d00), ORANGE, hex(0xffd27a)),
                process: Fill::Gradient(hex(0xb07d00), ORANGE, hex(0xffd27a)),
                live: ink(hex(0x009e73)),
                caution: ink(hex(0xf0e442)),
                warn: ink(ORANGE),
                error: ink(hex(0xd55e00)),
                // Okabe and Ito bluish green, yellow, orange and vermillion
                // as HSV stops, so the blend stays on safe hues.
                usage: Usage::Smooth([
                    Hsv::new(164, 100, 62),
                    Hsv::new(56, 72, 94),
                    Hsv::new(41, 100, 90),
                    Hsv::new(26, 100, 84),
                ]),
                series: [
                    ink(SKY),
                    ink(ORANGE),
                    ink(hex(0xcc79a7)),
                    ink(hex(0x009e73)),
                    ink(hex(0xf0e442)),
                    ink(hex(0x1b6fb5)),
                    ink(hex(0xd55e00)),
                    ink(gray(0xbb)),
                ],
            },
            // The default palette with the full hue wheel for severity: green,
            // yellow, orange, then red. Each stop is bright enough for black.
            Theme::Spectrum => Palette {
                live: ink(Hsv::new(130, 85, 75).rgb()),
                caution: ink(Hsv::new(55, 85, 95).rgb()),
                warn: ink(Hsv::new(30, 90, 95).rgb()),
                error: ink(Hsv::new(0, 80, 95).rgb()),
                usage: Usage::Smooth([
                    Hsv::new(130, 85, 75),
                    Hsv::new(55, 85, 95),
                    Hsv::new(30, 90, 95),
                    Hsv::new(0, 80, 95),
                ]),
                ..Theme::Standard.palette(depth)
            },
            Theme::Dark => Palette {
                depth,
                mono: false,
                bg: Ink::Default,
                fg: Ink::Default,
                title: Ink::Default,
                hi: Ink::Named(Color::Cyan),
                sel_bg: Ink::Named(Color::Blue),
                sel_fg: Ink::Named(Color::White),
                inactive: Ink::Named(Color::DarkGray),
                graph_text: Ink::Named(Color::DarkGray),
                meter_bg: Ink::Named(Color::DarkGray),
                div_line: Ink::Named(Color::DarkGray),
                box_cpu: Ink::Default,
                box_mem: Ink::Default,
                box_net: Ink::Default,
                box_proc: Ink::Default,
                cpu: Fill::Flat(Ink::Named(Color::Cyan)),
                down: Fill::Flat(Ink::Named(Color::Magenta)),
                cached: Fill::Flat(Ink::Named(Color::Cyan)),
                avail: Fill::Flat(Ink::Named(Color::Cyan)),
                process: Fill::Flat(Ink::Default),
                live: Ink::Named(Color::Green),
                caution: Ink::Named(Color::Yellow),
                warn: Ink::Named(Color::Yellow),
                error: Ink::Named(Color::Red),
                usage: Usage::Steps([
                    Ink::Named(Color::Green),
                    Ink::Named(Color::Yellow),
                    Ink::Named(Color::LightRed),
                    Ink::Named(Color::Red),
                ]),
                series: [
                    Ink::Named(Color::Cyan),
                    Ink::Named(Color::Yellow),
                    Ink::Named(Color::Magenta),
                    Ink::Named(Color::Green),
                    Ink::Named(Color::LightRed),
                    Ink::Named(Color::LightBlue),
                    Ink::Named(Color::LightMagenta),
                    Ink::Named(Color::LightGreen),
                ],
            },
            // Bright colors vanish on a white ground, so use the dark ones.
            Theme::Light => Palette {
                hi: Ink::Named(Color::Blue),
                inactive: Ink::Named(Color::Gray),
                graph_text: Ink::Named(Color::Gray),
                meter_bg: Ink::Named(Color::Gray),
                div_line: Ink::Named(Color::Gray),
                cpu: Fill::Flat(Ink::Named(Color::Blue)),
                cached: Fill::Flat(Ink::Named(Color::Blue)),
                avail: Fill::Flat(Ink::Named(Color::Blue)),
                caution: ink(Rgb(160, 100, 0)),
                warn: ink(Rgb(160, 100, 0)),
                usage: Usage::Steps([
                    Ink::Named(Color::Green),
                    ink(Rgb(160, 100, 0)),
                    Ink::Named(Color::Magenta),
                    Ink::Named(Color::Red),
                ]),
                series: [
                    Ink::Named(Color::Blue),
                    ink(Rgb(160, 100, 0)),
                    Ink::Named(Color::Magenta),
                    Ink::Named(Color::Green),
                    Ink::Named(Color::Red),
                    Ink::Named(Color::Cyan),
                    Ink::Named(Color::DarkGray),
                    ink(Rgb(120, 120, 0)),
                ],
                ..Theme::Dark.palette(depth)
            },
            Theme::Mono => Palette {
                mono: true,
                hi: Ink::Default,
                sel_bg: Ink::Default,
                sel_fg: Ink::Default,
                inactive: Ink::Default,
                graph_text: Ink::Default,
                meter_bg: Ink::Default,
                div_line: Ink::Default,
                cpu: Fill::Flat(Ink::Default),
                down: Fill::Flat(Ink::Default),
                cached: Fill::Flat(Ink::Default),
                avail: Fill::Flat(Ink::Default),
                live: Ink::Default,
                caution: Ink::Default,
                warn: Ink::Default,
                error: Ink::Default,
                usage: Usage::Steps([Ink::Default; 4]),
                series: [Ink::Default; SERIES],
                ..Theme::Dark.palette(depth)
            },
        }
    }
}

/// The 16 basic colors with the values xterm shows, for the nearest match.
const BASIC: [(Color, Rgb); 16] = [
    (Color::Black, Rgb(0, 0, 0)),
    (Color::Red, Rgb(205, 0, 0)),
    (Color::Green, Rgb(0, 205, 0)),
    (Color::Yellow, Rgb(205, 205, 0)),
    (Color::Blue, Rgb(0, 0, 238)),
    (Color::Magenta, Rgb(205, 0, 205)),
    (Color::Cyan, Rgb(0, 205, 205)),
    (Color::Gray, Rgb(229, 229, 229)),
    (Color::DarkGray, Rgb(127, 127, 127)),
    (Color::LightRed, Rgb(255, 0, 0)),
    (Color::LightGreen, Rgb(0, 255, 0)),
    (Color::LightYellow, Rgb(255, 255, 0)),
    (Color::LightBlue, Rgb(92, 92, 255)),
    (Color::LightMagenta, Rgb(255, 0, 255)),
    (Color::LightCyan, Rgb(0, 255, 255)),
    (Color::White, Rgb(255, 255, 255)),
];

fn distance(a: Rgb, b: Rgb) -> i32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The nearest of the 6 levels of the 256 color cube.
fn cube_level(v: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    (0..6u8)
        .min_by_key(|i| (i32::from(LEVELS[usize::from(*i)]) - i32::from(v)).abs())
        .unwrap_or(0)
}

impl Rgb {
    /// This color at the given depth. 256 colors use the cube or, for a gray,
    /// the 24 step gray ramp, whichever is nearer.
    pub fn at(self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::True => Color::Rgb(self.0, self.1, self.2),
            ColorDepth::Ansi256 => {
                let levels = [0, 95, 135, 175, 215, 255];
                let (r, g, b) = (cube_level(self.0), cube_level(self.1), cube_level(self.2));
                let cube = Rgb(
                    levels[usize::from(r)],
                    levels[usize::from(g)],
                    levels[usize::from(b)],
                );
                let avg = ((u16::from(self.0) + u16::from(self.1) + u16::from(self.2)) / 3) as u8;
                let step = (i32::from(avg) - 8 + 5) / 10;
                let step = step.clamp(0, 23) as u8;
                let ramp_v = 8 + 10 * step;
                let ramp = Rgb(ramp_v, ramp_v, ramp_v);
                if distance(self, ramp) < distance(self, cube) {
                    Color::Indexed(232 + step)
                } else {
                    Color::Indexed(16 + 36 * r + 6 * g + b)
                }
            }
            ColorDepth::Ansi16 => BASIC
                .iter()
                .min_by_key(|(_, c)| distance(self, *c))
                .map_or(Color::Reset, |(color, _)| *color),
        }
    }
}

impl Palette {
    pub fn color(&self, ink: Ink) -> Color {
        match ink {
            Ink::Default => Color::Reset,
            Ink::Named(c) => c,
            Ink::Rgb(rgb) => rgb.at(self.depth),
        }
    }

    pub fn graded(&self, fill: Fill, pct: f64) -> Color {
        self.color(fill.at(pct))
    }

    /// The usage ink at `pct` percent of the limit.
    pub fn usage_ink(&self, pct: f64) -> Ink {
        self.usage.at(pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_visits_every_theme_and_wraps() {
        let mut t = Theme::Standard;
        let mut seen = vec![t];
        for _ in 0..5 {
            t = t.next();
            seen.push(t);
        }
        assert_eq!(
            seen,
            [
                Theme::Standard,
                Theme::Spectrum,
                Theme::Dark,
                Theme::Light,
                Theme::Mono,
                Theme::Standard
            ]
        );
    }

    #[test]
    fn the_default_theme_is_the_default_and_names_round_trip() {
        assert_eq!(Theme::default(), Theme::Standard);
        for t in Theme::ALL {
            assert_eq!(Theme::parse(t.name()), Some(t));
        }
        assert_eq!(Theme::parse("solarized"), None);
    }

    #[test]
    fn the_default_theme_uses_its_documented_values() {
        let p = Theme::Standard.palette(ColorDepth::True);
        let rgb = |ink| p.color(ink);
        assert_eq!(rgb(p.bg), Color::Rgb(0, 0, 0));
        assert_eq!(rgb(p.fg), Color::Rgb(0xcc, 0xcc, 0xcc));
        assert_eq!(rgb(p.title), Color::Rgb(0xee, 0xee, 0xee));
        assert_eq!(rgb(p.hi), Color::Rgb(0x56, 0xb4, 0xe9));
        assert_eq!(rgb(p.sel_bg), Color::Rgb(0x1f, 0x4e, 0x79));
        assert_eq!(rgb(p.sel_fg), Color::Rgb(0xff, 0xff, 0xff));
        assert_eq!(rgb(p.inactive), Color::Rgb(0x76, 0x76, 0x76));
        assert_eq!(rgb(p.graph_text), Color::Rgb(0x90, 0x90, 0x90));
        assert_eq!(rgb(p.warn), Color::Rgb(0xe6, 0x9f, 0x00));
        assert_eq!(rgb(p.error), Color::Rgb(0xd5, 0x5e, 0x00));
        assert_eq!(rgb(p.live), Color::Rgb(0x00, 0x9e, 0x73));
        assert_eq!(rgb(p.div_line), Color::Rgb(0x6b, 0x6b, 0x6b));
        assert_eq!(rgb(p.meter_bg), Color::Rgb(0x6b, 0x6b, 0x6b));
        let g = |a, b, c| Fill::Gradient(hex(a), hex(b), hex(c));
        assert_eq!(p.cpu, g(0x1b6fb5, 0x56b4e9, 0xb8e2f8));
        assert_eq!(p.down, g(0x1b6fb5, 0x3f89b8, 0xb8e2f8));
        assert_eq!(p.cached, g(0x1b6fb5, 0x56b4e9, 0xb8e2f8));
        assert_eq!(p.avail, g(0xb07d00, 0xe69f00, 0xffd27a));
        assert_eq!(p.process, g(0xb07d00, 0xe69f00, 0xffd27a));
    }

    #[test]
    fn a_gradient_runs_start_to_mid_to_end() {
        let g = Fill::Gradient(Rgb(0, 0, 0), Rgb(100, 100, 100), Rgb(200, 0, 0));
        assert_eq!(g.at(0.0), Ink::Rgb(Rgb(0, 0, 0)));
        assert_eq!(g.at(25.0), Ink::Rgb(Rgb(50, 50, 50)));
        assert_eq!(g.at(50.0), Ink::Rgb(Rgb(100, 100, 100)));
        assert_eq!(g.at(100.0), Ink::Rgb(Rgb(200, 0, 0)));
        assert_eq!(g.at(250.0), g.at(100.0));
    }

    #[test]
    fn depth_comes_from_colorterm_then_term() {
        use ColorDepth::*;
        assert_eq!(ColorDepth::detect(Some("truecolor"), Some("xterm")), True);
        assert_eq!(ColorDepth::detect(Some("24bit"), None), True);
        assert_eq!(ColorDepth::detect(None, Some("xterm-256color")), Ansi256);
        assert_eq!(ColorDepth::detect(None, Some("linux")), Ansi16);
        assert_eq!(ColorDepth::detect(None, None), Ansi16);
    }

    #[test]
    fn colors_reduce_to_the_256_and_16_color_sets() {
        // A gray lands on the gray ramp, a hue on the cube, red on basic red.
        assert_eq!(gray(0xcc).at(ColorDepth::Ansi256), Color::Indexed(252));
        assert_eq!(Rgb(255, 0, 0).at(ColorDepth::Ansi256), Color::Indexed(196));
        assert_eq!(Rgb(0, 0, 0).at(ColorDepth::Ansi256), Color::Indexed(16));
        assert_eq!(Rgb(0xdc, 0x4c, 0x4c).at(ColorDepth::Ansi16), Color::Red);
        assert_eq!(gray(0x30).at(ColorDepth::Ansi16), Color::Black);
        assert_eq!(gray(0xee).at(ColorDepth::Ansi16), Color::Gray);
    }

    #[test]
    fn hsv_converts_at_the_edges() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), Rgb(255, 0, 0));
        assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), Rgb(255, 0, 0));
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), Rgb(0, 255, 0));
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), Rgb(0, 0, 255));
        assert_eq!(hsv_to_rgb(200.0, 0.0, 0.5), Rgb(128, 128, 128));
        assert_eq!(hsv_to_rgb(200.0, 1.0, 0.0), Rgb(0, 0, 0));
        assert_eq!(hsv_to_rgb(60.0, 1.0, 1.0), Rgb(255, 255, 0));
        assert_eq!(hsv_to_rgb(-60.0, 1.0, 1.0), hsv_to_rgb(300.0, 1.0, 1.0));
    }

    fn usage_rgb(p: &Palette, pct: f64) -> Rgb {
        match p.usage_ink(pct) {
            Ink::Rgb(c) => c,
            other => panic!("expected RGB, got {other:?}"),
        }
    }

    #[test]
    fn the_default_usage_gradient_holds_then_moves_through_each_threshold() {
        let p = Theme::Standard.palette(ColorDepth::True);
        let Usage::Smooth(stops) = p.usage else {
            panic!("the default theme blends");
        };
        let at = |pct| usage_rgb(&p, pct);
        assert_eq!(at(0.0), stops[0].rgb());
        assert_eq!(at(49.0), stops[0].rgb());
        assert_eq!(at(50.0), stops[0].rgb());
        assert_ne!(at(51.0), at(50.0));
        assert_ne!(at(69.0), stops[1].rgb());
        assert_eq!(at(70.0), stops[1].rgb());
        assert_ne!(at(89.0), stops[2].rgb());
        assert_eq!(at(90.0), stops[2].rgb());
        assert_ne!(at(99.0), stops[3].rgb());
        assert_eq!(at(100.0), stops[3].rgb());
        assert_eq!(at(150.0), stops[3].rgb());
    }

    #[test]
    fn the_usage_gradient_has_no_jump_at_a_threshold() {
        let p = Theme::Standard.palette(ColorDepth::True);
        for edge in [USAGE_HIGH, USAGE_CRITICAL, USAGE_LIMIT] {
            let (a, b) = (usage_rgb(&p, edge - 0.1), usage_rgb(&p, edge));
            let d = |x: u8, y: u8| i32::from(x).abs_diff(i32::from(y));
            assert!(d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2) < 12, "{edge}");
        }
    }

    #[test]
    fn the_stepped_usage_inks_change_at_each_threshold() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = theme.palette(ColorDepth::True);
            let at = |pct| p.usage_ink(pct);
            assert_eq!(at(49.0), at(0.0));
            assert_ne!(at(50.0), at(49.0));
            assert_eq!(at(69.0), at(50.0));
            assert_ne!(at(70.0), at(69.0));
            assert_eq!(at(89.0), at(70.0));
            assert_ne!(at(90.0), at(89.0));
            assert_eq!(at(100.0), at(90.0));
            assert_eq!(at(250.0), at(100.0));
        }
        let mono = Theme::Mono.palette(ColorDepth::True);
        assert_eq!(mono.color(mono.usage_ink(95.0)), Color::Reset);
    }

    #[test]
    fn usage_colors_reduce_to_the_256_and_16_color_sets() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            let p = Theme::Standard.palette(depth);
            let calm = p.color(p.usage_ink(10.0));
            let alarm = p.color(p.usage_ink(110.0));
            assert_ne!(calm, alarm);
            assert!(!matches!(calm, Color::Rgb(..)));
        }
    }

    #[test]
    fn the_spectrum_theme_is_opt_in_and_a_full_cycle_returns_to_the_default() {
        assert_ne!(Theme::default(), Theme::Spectrum);
        assert_eq!(Theme::parse("spectrum"), Some(Theme::Spectrum));
        let mut t = Theme::Spectrum;
        for _ in 0..Theme::ALL.len() {
            t = t.next();
        }
        assert_eq!(t, Theme::Spectrum);
        assert_eq!(Theme::Mono.next(), Theme::Standard);
    }

    #[test]
    fn the_spectrum_gradient_holds_then_moves_through_each_threshold() {
        let p = Theme::Spectrum.palette(ColorDepth::True);
        let Usage::Smooth(stops) = p.usage else {
            panic!("the spectrum theme blends");
        };
        let at = |pct| usage_rgb(&p, pct);
        assert_eq!(at(49.0), stops[0].rgb());
        assert_eq!(at(50.0), stops[0].rgb());
        assert_ne!(at(69.0), stops[1].rgb());
        assert_eq!(at(70.0), stops[1].rgb());
        assert_ne!(at(89.0), stops[2].rgb());
        assert_eq!(at(90.0), stops[2].rgb());
        assert_eq!(at(100.0), stops[3].rgb());
        assert_eq!(at(101.0), stops[3].rgb());
        assert_eq!(at(400.0), stops[3].rgb());
    }

    #[test]
    fn the_spectrum_runs_from_green_to_red() {
        let p = Theme::Spectrum.palette(ColorDepth::True);
        let calm = usage_rgb(&p, 0.0);
        let alarm = usage_rgb(&p, 100.0);
        assert!(calm.1 > calm.0, "calm is green: {calm:?}");
        assert!(alarm.0 > alarm.1, "alarm is red: {alarm:?}");
    }

    #[test]
    fn the_spectrum_stays_readable_on_its_black_ground_and_in_reduced_depths() {
        use crate::tui::a11y::{contrast, rgb_of, NON_TEXT_CONTRAST};
        let p = Theme::Spectrum.palette(ColorDepth::True);
        for pct in [0.0, 50.0, 60.0, 70.0, 80.0, 90.0, 95.0, 100.0] {
            let ratio = contrast(usage_rgb(&p, pct), rgb_of(p.bg));
            assert!(ratio >= NON_TEXT_CONTRAST, "{pct}% is {ratio:.2}");
        }
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            let p = Theme::Spectrum.palette(depth);
            let calm = p.color(p.usage_ink(0.0));
            let alarm = p.color(p.usage_ink(100.0));
            assert_ne!(calm, alarm);
            assert!(!matches!(calm, Color::Rgb(..)));
        }
    }

    #[test]
    fn the_default_theme_keeps_its_safe_usage_hues() {
        let p = Theme::Standard.palette(ColorDepth::True);
        let alarm = usage_rgb(&p, 100.0);
        // The HSV stop (26, 100, 84) rounds to one step off the vermillion hex.
        assert_eq!(alarm, Rgb(214, 93, 0));
        assert_eq!(p.error, ink(hex(0xd55e00)));
    }

    #[test]
    fn mono_sets_no_color() {
        let p = Theme::Mono.palette(ColorDepth::True);
        assert!(p.mono);
        for i in [p.fg, p.hi, p.box_cpu, p.live, p.warn, p.error, p.bg] {
            assert_eq!(p.color(i), Color::Reset);
        }
        assert_eq!(p.graded(p.cpu, 90.0), Color::Reset);
        assert_eq!(p.graded(p.process, 90.0), Color::Reset);
    }

    /// Contrast and color-blind checks. The method is WCAG 2.x contrast from
    /// sRGB relative luminance, and the Machado et al. (2009) simulation at
    /// full severity in linear RGB, compared with CIE76 delta E in CIELAB.
    mod accessibility {
        use super::*;
        use crate::tui::a11y::{confusable, contrast, rgb_of, NON_TEXT_CONTRAST, TEXT_CONTRAST};

        fn standard() -> Palette {
            Theme::Standard.palette(ColorDepth::True)
        }

        fn series() -> Vec<Rgb> {
            standard().series.iter().map(|i| rgb_of(*i)).collect()
        }

        #[test]
        fn text_roles_meet_the_text_contrast_on_the_ground() {
            let p = standard();
            let bg = rgb_of(p.bg);
            for (name, ink) in [
                ("fg", p.fg),
                ("title", p.title),
                ("graph_text", p.graph_text),
                ("inactive", p.inactive),
                ("hi", p.hi),
                ("warn", p.warn),
                ("error", p.error),
                ("live", p.live),
            ] {
                let ratio = contrast(rgb_of(ink), bg);
                assert!(ratio >= TEXT_CONTRAST, "{name} is {ratio:.2}");
            }
        }

        #[test]
        fn the_selected_row_meets_the_text_contrast() {
            let p = standard();
            let ratio = contrast(rgb_of(p.sel_fg), rgb_of(p.sel_bg));
            assert!(ratio >= TEXT_CONTRAST, "selection is {ratio:.2}");
        }

        #[test]
        fn lines_boxes_and_series_meet_the_non_text_contrast() {
            let p = standard();
            let bg = rgb_of(p.bg);
            let mut shapes = vec![p.div_line, p.meter_bg, p.box_cpu, p.box_mem, p.box_net];
            shapes.push(p.box_proc);
            shapes.extend(p.series);
            for ink in shapes {
                let ratio = contrast(rgb_of(ink), bg);
                assert!(ratio >= NON_TEXT_CONTRAST, "{ink:?} is {ratio:.2}");
            }
            for fill in [p.cpu, p.down, p.cached, p.avail, p.process] {
                let Fill::Gradient(a, b, c) = fill else {
                    panic!("the Standard theme uses gradients, got {fill:?}");
                };
                for stop in [a, b, c] {
                    let ratio = contrast(stop, bg);
                    assert!(ratio >= NON_TEXT_CONTRAST, "{stop:?} is {ratio:.2}");
                }
            }
        }

        /// Costs are drawn as text in the `process` color, so its start must
        /// read as text.
        #[test]
        fn the_cost_text_ramp_starts_readable() {
            let Fill::Gradient(start, _, _) = standard().process else {
                panic!("the Standard theme uses gradients");
            };
            assert!(contrast(start, Rgb(0, 0, 0)) >= TEXT_CONTRAST);
        }

        #[test]
        fn the_state_colors_stay_apart_under_each_color_vision_type() {
            let p = standard();
            let states = [rgb_of(p.warn), rgb_of(p.error), rgb_of(p.live)];
            assert_eq!(confusable(&states), None);
        }

        #[test]
        fn text_and_shape_roles_hold_up_after_reduction_to_256_colors() {
            let p = Theme::Standard.palette(ColorDepth::Ansi256);
            let bg = Rgb(0, 0, 0);
            let back = |ink: Ink| match p.color(ink) {
                Color::Indexed(i) => indexed_rgb(i),
                other => panic!("expected an indexed color, got {other:?}"),
            };
            for ink in [
                p.fg,
                p.graph_text,
                p.inactive,
                p.hi,
                p.warn,
                p.error,
                p.live,
            ] {
                assert!(contrast(back(ink), bg) >= TEXT_CONTRAST, "{ink:?}");
            }
            for ink in [p.div_line, p.box_cpu, p.box_mem, p.box_net, p.box_proc] {
                assert!(contrast(back(ink), bg) >= NON_TEXT_CONTRAST, "{ink:?}");
            }
        }

        /// The xterm value of a 256 color index of 16 or more.
        fn indexed_rgb(i: u8) -> Rgb {
            if i >= 232 {
                return gray(8 + 10 * (i - 232));
            }
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            let n = i - 16;
            Rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }

        #[test]
        fn every_series_pair_stays_apart_under_each_color_vision_type() {
            assert_eq!(confusable(&series()), None);
        }

        #[test]
        fn the_usage_stops_meet_the_contrast_and_stay_apart_under_color_vision_types() {
            let p = standard();
            let Usage::Smooth(stops) = p.usage else {
                panic!("the default theme blends");
            };
            let rgbs: Vec<Rgb> = stops.iter().map(|s| s.rgb()).collect();
            for c in &rgbs {
                assert!(contrast(*c, rgb_of(p.bg)) >= NON_TEXT_CONTRAST, "{c:?}");
            }
            assert_eq!(confusable(&rgbs), None);
        }

        #[test]
        fn the_checker_flags_two_near_grays() {
            let grays = [gray(0x77), gray(0x7a)];
            assert!(confusable(&grays).is_some());
        }

        #[test]
        fn the_checker_rejects_a_low_contrast_gray() {
            assert!(contrast(gray(0x60), Rgb(0, 0, 0)) < TEXT_CONTRAST);
        }
    }
}
