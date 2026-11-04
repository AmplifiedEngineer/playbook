// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A grid of Braille dots: each terminal cell holds 2 columns by 4 rows of
//! dots, so a chart gets twice the horizontal and four times the vertical
//! resolution of whole-cell glyphs. Every set dot carries a role, and a cell
//! takes the role that ranks highest among its dots.

const DOT_COLS: usize = 2;
const DOT_ROWS: usize = 4;
const BRAILLE_BASE: u32 = 0x2800;

/// The Braille bit of the dot at `[column][row]` inside a cell.
const BITS: [[u8; DOT_ROWS]; DOT_COLS] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// The character for a cell whose dots are the set bits of `mask`.
pub fn braille(mask: u8) -> char {
    char::from_u32(BRAILLE_BASE + u32::from(mask)).unwrap_or(' ')
}

/// Dots over `width` by `height` cells. `R` ranks the roles: on a shared cell
/// the greater role wins.
#[derive(Debug, Clone)]
pub struct DotGrid<R> {
    width: usize,
    height: usize,
    cells: Vec<(u8, Option<R>)>,
}

impl<R: Copy + Ord> DotGrid<R> {
    pub fn new(width: usize, height: usize) -> Self {
        DotGrid {
            width,
            height,
            cells: vec![(0, None); width * height],
        }
    }

    /// The number of dot columns.
    pub fn dot_width(&self) -> usize {
        self.width * DOT_COLS
    }

    /// The number of dot rows.
    pub fn dot_height(&self) -> usize {
        self.height * DOT_ROWS
    }

    /// Sets the dot at `(x, y)`, counted from the top left. A dot outside the
    /// grid is ignored.
    pub fn set(&mut self, x: usize, y: usize, role: R) {
        if x >= self.dot_width() || y >= self.dot_height() {
            return;
        }
        let cell = &mut self.cells[(y / DOT_ROWS) * self.width + x / DOT_COLS];
        cell.0 |= BITS[x % DOT_COLS][y % DOT_ROWS];
        cell.1 = cell.1.max(Some(role));
    }

    /// Sets the dots on the straight line between two dots, ends included.
    pub fn line(&mut self, from: (usize, usize), to: (usize, usize), role: R) {
        let (mut x, mut y) = (from.0 as isize, from.1 as isize);
        let (x1, y1) = (to.0 as isize, to.1 as isize);
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = ((x1 - x).signum(), (y1 - y).signum());
        let mut err = dx + dy;
        loop {
            self.set(x as usize, y as usize, role);
            if (x, y) == (x1, y1) {
                return;
            }
            let doubled = 2 * err;
            if doubled >= dy {
                err += dy;
                x += sx;
            }
            if doubled <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Sets column `x` from dot row `top` down to and including `bottom`.
    pub fn fill_column(&mut self, x: usize, top: usize, bottom: usize, role: R) {
        for y in top..=bottom {
            self.set(x, y, role);
        }
    }

    /// The character and role of cell `(col, row)`, `None` when it has no dot.
    pub fn cell(&self, col: usize, row: usize) -> Option<(char, R)> {
        if col >= self.width || row >= self.height {
            return None;
        }
        let (mask, role) = self.cells[row * self.width + col];
        role.map(|r| (braille(mask), r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(c: char) -> u32 {
        c as u32 - BRAILLE_BASE
    }

    #[test]
    fn dots_in_one_cell_encode_to_the_exact_braille_mask() {
        // Arrange
        let mut grid = DotGrid::new(1, 1);

        // Act
        grid.set(0, 0, 1u8);
        grid.set(1, 3, 1);

        // Assert
        assert_eq!(grid.cell(0, 0), Some(('\u{2881}', 1)));
    }

    #[test]
    fn a_full_cell_is_all_eight_dots_and_an_empty_one_has_none() {
        // Arrange
        let mut grid = DotGrid::new(2, 1);

        // Act
        for x in 0..2 {
            grid.fill_column(x, 0, 3, 0u8);
        }

        // Assert
        assert_eq!(grid.cell(0, 0), Some(('\u{28ff}', 0)));
        assert_eq!(grid.cell(1, 0), None);
    }

    #[test]
    fn each_dot_of_a_cell_maps_to_its_own_bit() {
        let expected = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        for (x, column) in expected.iter().enumerate() {
            for (y, bit) in column.iter().enumerate() {
                let mut grid = DotGrid::new(1, 1);

                grid.set(x, y, 0u8);

                assert_eq!(grid.cell(0, 0), Some((braille(*bit), 0)), "({x},{y})");
            }
        }
    }

    #[test]
    fn the_higher_role_wins_a_shared_cell_in_either_order() {
        let mut low_first = DotGrid::new(1, 1);
        low_first.set(0, 0, 1u8);
        low_first.set(1, 1, 3);
        let mut high_first = DotGrid::new(1, 1);
        high_first.set(1, 1, 3u8);
        high_first.set(0, 0, 1);

        assert_eq!(low_first.cell(0, 0).map(|c| c.1), Some(3));
        assert_eq!(high_first.cell(0, 0).map(|c| c.1), Some(3));
    }

    #[test]
    fn a_vertical_line_uses_four_dot_rows_per_cell_row() {
        // Arrange
        let mut grid = DotGrid::new(1, 2);

        // Act
        grid.line((0, 0), (0, 7), 0u8);

        // Assert
        assert_eq!(grid.dot_height(), 8);
        for row in 0..2 {
            assert_eq!(
                mask(grid.cell(0, row).unwrap().0),
                0x01 | 0x02 | 0x04 | 0x40
            );
        }
    }

    #[test]
    fn a_line_over_two_cells_sets_both_end_points() {
        // Arrange
        let mut grid = DotGrid::new(2, 2);

        // Act
        grid.line((0, 0), (3, 7), 0u8);

        // Assert
        assert_ne!(mask(grid.cell(0, 0).unwrap().0) & 0x01, 0, "first dot");
        assert_ne!(mask(grid.cell(1, 1).unwrap().0) & 0x80, 0, "last dot");
    }

    #[test]
    fn a_flat_line_and_a_single_point_are_drawn() {
        let mut grid = DotGrid::new(2, 1);

        grid.line((0, 2), (3, 2), 0u8);
        let mut point = DotGrid::new(1, 1);
        point.line((1, 1), (1, 1), 0u8);

        assert_eq!(grid.cell(0, 0), Some((braille(0x04 | 0x20), 0)));
        assert_eq!(grid.cell(1, 0), Some((braille(0x04 | 0x20), 0)));
        assert_eq!(point.cell(0, 0), Some((braille(0x10), 0)));
    }

    #[test]
    fn dots_outside_the_grid_and_an_empty_grid_are_ignored() {
        let mut grid = DotGrid::new(1, 1);
        grid.set(2, 0, 0u8);
        grid.set(0, 4, 0);
        let empty: DotGrid<u8> = DotGrid::new(0, 0);

        assert_eq!(grid.cell(0, 0), None);
        assert_eq!(empty.cell(0, 0), None);
    }
}
