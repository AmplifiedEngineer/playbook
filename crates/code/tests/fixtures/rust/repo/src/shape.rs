use crate::{area, Point};

pub struct Square {
    pub side: i32,
}

pub fn corner() -> Point {
    Point { x: 0, y: 0 }
}

pub fn double_area() -> i32 {
    // area doubled
    area(&corner()) * 2
}
