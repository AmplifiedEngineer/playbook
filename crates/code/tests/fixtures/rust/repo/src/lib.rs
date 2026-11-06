// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
//! Geometry helpers.
pub mod shape;

pub const SCALE: i32 = 2;

pub struct Point {
    pub x: i32,
    pub y: i32,
}

pub enum Axis {
    Horizontal,
    Vertical,
}

pub fn area(p: &Point) -> i32 {
    // area is computed from the point
    let note = "area";
    p.x * p.y * SCALE
}

pub trait Describe {
    fn describe(&self) -> String;
}

impl Describe for Point {
    fn describe(&self) -> String {
        format!("{}", area(self))
    }
}
