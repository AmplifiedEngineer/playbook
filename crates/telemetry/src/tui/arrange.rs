// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Rearranging the panels of one tab as they sit on screen: swapping with a
//! neighbour, joining or splitting a row, adding and removing. Each operation
//! takes the cursor slot (an index into the tab's panels, in screen order),
//! checks the limits before it changes anything, and returns where the
//! cursor goes so it follows the panel it moved. A plain swap leaves the
//! rows alone, so an automatic tab stays automatic; only a width change
//! turns it into an arranged one. Adding takes no cursor: a new panel goes to
//! the next free place.

use super::layout::{auto_rows, PanelId, Tab, MAX_ROWS, MAX_ROW_PANELS, MAX_TAB_PANELS};

/// Why the budget strip cannot be moved: it always takes the top line.
const STRIP_PINNED: &str = "The budget strip stays at the top";

/// The cells of the tab as it is drawn: the size of each row and, for each
/// cell in screen order, the index of its panel. The budget strip has no cell
/// wherever the list holds it, so it is left out and the rows match the grid.
fn cells(tab: &Tab) -> (Vec<u8>, Vec<usize>) {
    let cells: Vec<usize> = tab
        .panels
        .iter()
        .enumerate()
        .filter(|(_, p)| **p != PanelId::Budget)
        .map(|(i, _)| i)
        .collect();
    let Some(rows) = &tab.rows else {
        return (auto_rows(cells.len()), cells);
    };
    let mut next = tab.panels.iter();
    let rows = rows
        .iter()
        .map(|n| {
            next.by_ref()
                .take(usize::from(*n))
                .filter(|p| **p != PanelId::Budget)
                .count() as u8
        })
        .filter(|n| *n > 0)
        .collect();
    (rows, cells)
}

/// The panel indices of each row as the tab is drawn. A panel with no cell,
/// the budget strip of an automatic tab, is in no row.
pub fn grid(tab: &Tab) -> Vec<Vec<usize>> {
    let (rows, cells) = cells(tab);
    let mut next = cells.into_iter();
    rows.iter()
        .map(|n| next.by_ref().take(usize::from(*n)).collect())
        .collect()
}

/// The row and the column of cell `at`.
fn locate(rows: &[u8], at: usize) -> (usize, usize) {
    let mut start = 0;
    for (row, n) in rows.iter().enumerate() {
        let n = usize::from(*n);
        if at < start + n {
            return (row, at - start);
        }
        start += n;
    }
    (rows.len().saturating_sub(1), 0)
}

fn row_start(rows: &[u8], row: usize) -> usize {
    rows[..row].iter().map(|n| usize::from(*n)).sum()
}

/// The cell of panel `slot`, or why it has none.
fn cell_of(cells: &[usize], slot: usize) -> Result<usize, String> {
    cells
        .iter()
        .position(|i| *i == slot)
        .ok_or_else(|| STRIP_PINNED.to_string())
}

/// Swaps the panel with its neighbour in the same row.
pub fn move_sideways(tab: &mut Tab, slot: usize, by: isize) -> Result<usize, String> {
    let (rows, cells) = cells(tab);
    let (row, col) = locate(&rows, cell_of(&cells, slot)?);
    let to = col
        .checked_add_signed(by)
        .filter(|c| *c < usize::from(rows[row]));
    let Some(to) = to else {
        return Err("The panel is at the edge of its row".to_string());
    };
    let target = cells[row_start(&rows, row) + to];
    tab.panels.swap(slot, target);
    Ok(target)
}

/// The panel above or below `slot`, in the same column or the last one of a
/// shorter row.
pub fn vertical_neighbour(tab: &Tab, slot: usize, by: isize) -> Result<usize, String> {
    let (rows, cells) = cells(tab);
    let (row, col) = locate(&rows, cell_of(&cells, slot)?);
    let Some(next) = row.checked_add_signed(by).filter(|r| *r < rows.len()) else {
        let edge = if by < 0 { "top" } else { "bottom" };
        return Err(format!("The panel is at the {edge} of the tab"));
    };
    Ok(cells[row_start(&rows, next) + col.min(usize::from(rows[next]) - 1)])
}

/// Swaps the panel with the one above or below it.
pub fn move_vertically(tab: &mut Tab, slot: usize, by: isize) -> Result<usize, String> {
    let target = vertical_neighbour(tab, slot, by)?;
    tab.panels.swap(slot, target);
    Ok(target)
}

/// Puts the budget strip first in the list and, in an arranged tab, on a row of
/// its own ahead of the grid rows. The strip draws on top either way, so the
/// tab looks as before. Returns where the panel at `slot` is afterwards.
fn settle(tab: &mut Tab, slot: usize) -> usize {
    let Some(at) = tab.panels.iter().position(|p| *p == PanelId::Budget) else {
        return slot;
    };
    let lone_first = tab.rows.as_ref().is_none_or(|r| r.first() == Some(&1));
    if at == 0 && lone_first {
        return slot;
    }
    if let Some(rows) = &mut tab.rows {
        let (row, _) = locate(rows, at);
        rows[row] -= 1;
        if rows[row] == 0 {
            rows.remove(row);
        }
        rows.insert(0, 1);
    }
    tab.panels.remove(at);
    tab.panels.insert(0, PanelId::Budget);
    match slot.cmp(&at) {
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Less => slot + 1,
        std::cmp::Ordering::Greater => slot,
    }
}

/// The leading panels and rows that belong to the strip of a settled tab.
fn strip_lead(tab: &Tab) -> (usize, usize) {
    let strip = usize::from(tab.panels.first() == Some(&PanelId::Budget));
    (strip, if tab.rows.is_some() { strip } else { 0 })
}

/// The panels of the tab that take a cell, which is every panel but the strip.
pub fn grid_count(tab: &Tab) -> usize {
    tab.panels.iter().filter(|p| **p != PanelId::Budget).count()
}

/// Splits a row of two into two rows, or joins a row of one with a row of one
/// above or below it.
pub fn toggle_width(tab: &mut Tab, slot: usize) -> Result<usize, String> {
    if grid_count(tab) > MAX_TAB_PANELS {
        return Err(format!(
            "Only a tab of at most {MAX_TAB_PANELS} panels can be arranged"
        ));
    }
    let mut work = tab.clone();
    let slot = settle(&mut work, slot);
    let (strip, lead) = strip_lead(&work);
    if slot < strip {
        return Err(STRIP_PINNED.to_string());
    }
    let mut rows = match &work.rows {
        Some(rows) => rows[lead..].to_vec(),
        None => auto_rows(work.panels.len() - strip),
    };
    let (row, _) = locate(&rows, slot - strip);
    if usize::from(rows[row]) == MAX_ROW_PANELS {
        if rows.len() >= MAX_ROWS {
            return Err(format!("At most {MAX_ROWS} rows"));
        }
        rows[row] = 1;
        rows.insert(row + 1, 1);
    } else if rows.get(row + 1) == Some(&1) {
        rows[row] = 2;
        rows.remove(row + 1);
    } else if row > 0 && rows[row - 1] == 1 {
        rows[row - 1] = 2;
        rows.remove(row);
    } else {
        return Err("No row of one panel beside it to share with".to_string());
    }
    let mut all = vec![1; strip];
    all.extend(rows);
    work.rows = Some(all);
    *tab = work;
    Ok(slot)
}

/// Where the next panel lands in the rows of an arranged tab: the end of the
/// last row when it has room, a new row when the tab has a free one, otherwise
/// the end of the first row with room. Returns the index among the grid
/// panels and the row it joins, `None` when it opens a new row.
fn free_place(rows: &[u8]) -> (usize, Option<usize>) {
    let total: usize = rows.iter().map(|n| usize::from(*n)).sum();
    let roomy = |n: &u8| usize::from(*n) < MAX_ROW_PANELS;
    match rows.last() {
        Some(last) if roomy(last) => (total, Some(rows.len() - 1)),
        _ if rows.len() < MAX_ROWS => (total, None),
        _ => {
            let row = rows.iter().position(roomy).unwrap_or(rows.len() - 1);
            (row_start(rows, row) + usize::from(rows[row]), Some(row))
        }
    }
}

/// The index `add` puts a new grid panel at: the end of an automatic tab, the
/// next free place of an arranged one.
pub fn insert_point(tab: &Tab) -> usize {
    let mut work = tab.clone();
    settle(&mut work, 0);
    let (strip, lead) = strip_lead(&work);
    match &work.rows {
        None => work.panels.len(),
        Some(rows) => strip + free_place(&rows[lead..]).0,
    }
}

/// Adds `panel` in the next free place, so one action fills the tab. The budget
/// strip goes first and takes no cell. Any other panel goes at the end of an
/// automatic tab, whose grid then reflows, and in an arranged tab at the end of
/// the last row with room, or on a row of its own. A tab holds at most
/// `MAX_TAB_PANELS` panels besides the strip.
pub fn add(tab: &mut Tab, panel: PanelId) -> Result<usize, String> {
    if tab.panels.contains(&panel) {
        return Err(format!("{} is already on the tab", panel.word()));
    }
    if panel == PanelId::Budget {
        tab.panels.insert(0, panel);
        if let Some(rows) = &mut tab.rows {
            rows.insert(0, 1);
        }
        return Ok(0);
    }
    if grid_count(tab) >= MAX_TAB_PANELS {
        return Err(format!(
            "A tab holds at most {MAX_TAB_PANELS} panels besides the budget strip"
        ));
    }
    settle(tab, 0);
    let at = insert_point(tab);
    let (_, lead) = strip_lead(tab);
    if let Some(rows) = &mut tab.rows {
        match free_place(&rows[lead..]).1 {
            Some(row) => rows[lead + row] += 1,
            None => rows.push(1),
        }
    }
    tab.panels.insert(at, panel);
    Ok(at)
}

/// Takes the panel off the tab. A tab keeps at least one panel.
pub fn remove(tab: &mut Tab, slot: usize) -> Result<usize, String> {
    if tab.panels.len() == 1 {
        return Err("A tab needs at least one panel".to_string());
    }
    if let Some(mut rows) = tab.rows.take() {
        let (row, _) = locate(&rows, slot);
        rows[row] -= 1;
        if rows[row] == 0 {
            rows.remove(row);
        }
        tab.rows = Some(rows);
    }
    tab.panels.remove(slot);
    Ok(slot.min(tab.panels.len() - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::layout::Layout;

    fn tab(text: &str) -> Tab {
        Layout::parse(text).unwrap().tabs.remove(0)
    }

    fn words(tab: &Tab) -> Vec<&'static str> {
        tab.panels.iter().map(|p| p.word()).collect()
    }

    #[test]
    fn a_sideways_move_swaps_inside_the_row_and_the_cursor_follows() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let to = move_sideways(&mut t, 1, -1);

        // Assert
        assert_eq!(to, Ok(0));
        assert_eq!(words(&t), ["tokens", "spend", "models"]);
        assert_eq!(t.rows, Some(vec![2, 1]));
    }

    #[test]
    fn a_sideways_move_at_the_edge_of_a_row_changes_nothing_and_says_why() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let right = move_sideways(&mut t, 1, 1);
        let alone = move_sideways(&mut t, 2, 1);

        // Assert
        assert!(right.is_err() && alone.is_err());
        assert_eq!(words(&t), ["spend", "tokens", "models"]);
    }

    #[test]
    fn a_vertical_move_swaps_with_the_cell_above_and_keeps_the_rows() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let to = move_vertically(&mut t, 2, -1);

        // Assert
        assert_eq!(to, Ok(0));
        assert_eq!(words(&t), ["models", "tokens", "spend"]);
        assert_eq!(t.rows, Some(vec![2, 1]));
    }

    #[test]
    fn a_vertical_move_from_a_wide_row_lands_in_the_last_cell_of_a_shorter_one() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let down = move_vertically(&mut t, 1, 1);

        // Assert
        assert_eq!(down, Ok(2));
        assert_eq!(words(&t), ["spend", "models", "tokens"]);
    }

    #[test]
    fn a_plain_reorder_keeps_an_automatic_tab_automatic() {
        // Arrange
        let mut t = tab("A=spend,tokens");

        // Act
        let to = move_vertically(&mut t, 1, -1);

        // Assert
        assert_eq!(to, Ok(0));
        assert_eq!(words(&t), ["tokens", "spend"]);
        assert_eq!(t.rows, None);
    }

    #[test]
    fn a_vertical_move_past_the_top_or_bottom_is_refused() {
        // Arrange
        let mut t = tab("A=spend,tokens");

        // Act
        let up = move_vertically(&mut t, 0, -1);
        let down = move_vertically(&mut t, 1, 1);

        // Assert
        assert!(up.unwrap_err().contains("top"));
        assert!(down.unwrap_err().contains("bottom"));
    }

    #[test]
    fn the_width_toggle_splits_a_row_of_two_into_two_rows() {
        // Arrange
        let mut t = tab("A=spend,tokens");
        t.rows = Some(vec![2]);

        // Act
        let to = toggle_width(&mut t, 0);

        // Assert
        assert_eq!(to, Ok(0));
        assert_eq!(t.rows, Some(vec![1, 1]));
    }

    #[test]
    fn the_width_toggle_joins_a_row_of_one_with_the_next_or_the_previous() {
        // Arrange
        let mut next = tab("A=spend|tokens|models");
        let mut previous = tab("A=spend|tokens|models");

        // Act
        toggle_width(&mut next, 0).unwrap();
        toggle_width(&mut previous, 2).unwrap();

        // Assert
        assert_eq!(next.rows, Some(vec![2, 1]));
        assert_eq!(previous.rows, Some(vec![1, 2]));
    }

    #[test]
    fn the_width_toggle_turns_an_automatic_tab_into_an_arranged_one() {
        // Arrange
        let mut t = tab("A=spend,tokens,models");

        // Act
        toggle_width(&mut t, 0).unwrap();

        // Assert
        assert_eq!(t.rows, Some(vec![1, 1, 1]));
    }

    #[test]
    fn the_width_toggle_refuses_a_fifth_row_and_a_tab_over_eight_panels() {
        // Arrange
        let mut four_rows = tab("A=spend,tokens|models|live|sessions");
        let mut nine = tab("A=spend,tokens,models,live,sessions,projection,burnup,breakdown");
        nine.panels.push(PanelId::Live);
        nine.panels.push(PanelId::Sessions);

        // Act
        let split = toggle_width(&mut four_rows, 0);
        let big = toggle_width(&mut nine, 0);

        // Assert
        assert!(split.is_err() && big.is_err());
        assert_eq!(four_rows.rows, Some(vec![2, 1, 1, 1]));
        assert_eq!(nine.rows, None);
    }

    #[test]
    fn adding_to_an_arranged_tab_fills_the_last_row_then_opens_a_new_one() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let first = add(&mut t, PanelId::Live);
        let second = add(&mut t, PanelId::Sessions);

        // Assert
        assert_eq!(first, Ok(3));
        assert_eq!(second, Ok(4));
        assert_eq!(words(&t), ["spend", "tokens", "models", "live", "sessions"]);
        assert_eq!(t.rows, Some(vec![2, 2, 1]));
    }

    #[test]
    fn adding_to_a_full_arranged_row_set_fills_the_first_row_with_room() {
        // Arrange
        let mut t = tab("A=spend|tokens|models|live,sessions");

        // Act
        let at = add(&mut t, PanelId::Burnup);

        // Assert
        assert_eq!(at, Ok(1));
        assert_eq!(
            words(&t),
            ["spend", "burnup", "tokens", "models", "live", "sessions"]
        );
        assert_eq!(t.rows, Some(vec![2, 1, 1, 2]));
    }

    #[test]
    fn the_insert_point_is_the_end_of_an_automatic_tab_and_the_free_place_of_an_arranged_one() {
        // Arrange
        let arranged = tab("A=spend|tokens|models|live,sessions");
        let automatic = tab("A=spend,tokens");

        // Act
        let free = insert_point(&arranged);
        let end = insert_point(&automatic);

        // Assert
        assert_eq!(free, 1);
        assert_eq!(end, 2);
    }

    #[test]
    fn adding_to_an_automatic_tab_appends_and_stays_automatic() {
        // Arrange
        let mut t = tab("A=spend,tokens");

        // Act
        let at = add(&mut t, PanelId::Live);

        // Assert
        assert_eq!(at, Ok(2));
        assert_eq!(words(&t), ["spend", "tokens", "live"]);
        assert_eq!(t.rows, None);
    }

    #[test]
    fn a_tab_takes_eight_panels_automatic_or_arranged_and_refuses_the_ninth() {
        // Arrange
        let order = [
            PanelId::Spend,
            PanelId::Tokens,
            PanelId::Models,
            PanelId::Breakdown,
            PanelId::Live,
            PanelId::Sessions,
            PanelId::Burnup,
            PanelId::Projection,
        ];
        for arranged in [false, true] {
            let mut t = tab(if arranged {
                "A=spend|tokens"
            } else {
                "A=spend,tokens"
            });

            // Act
            for panel in &order[2..] {
                assert!(add(&mut t, *panel).is_ok(), "{panel:?}");
            }
            let ninth = add(&mut t, PanelId::SpendTotal);

            // Assert
            assert_eq!(t.panels.len(), MAX_TAB_PANELS);
            assert_eq!(
                ninth,
                Err("A tab holds at most 8 panels besides the budget strip".to_string())
            );
            assert_eq!(t.panels.len(), 8);
        }
    }

    #[test]
    fn adding_is_refused_for_a_duplicate() {
        // Arrange
        let mut one = tab("A=spend,tokens");

        // Act
        let again = add(&mut one, PanelId::Spend);

        // Assert
        assert!(again.is_err());
        assert_eq!(one.panels.len(), 2);
    }

    #[test]
    fn the_budget_strip_goes_first_beside_eight_panels_and_takes_no_cell() {
        // Arrange
        let mut automatic = tab("A=spend,tokens,models,breakdown,live,sessions,burnup,projection");
        let mut arranged = tab("A=spend,tokens|models,breakdown|live,sessions|burnup,projection");

        // Act
        let auto_at = add(&mut automatic, PanelId::Budget);
        let arranged_at = add(&mut arranged, PanelId::Budget);

        // Assert
        assert_eq!((auto_at, arranged_at), (Ok(0), Ok(0)));
        assert_eq!(automatic.panels[0], PanelId::Budget);
        assert_eq!(automatic.panels.len(), 9);
        assert_eq!(arranged.rows, Some(vec![1, 2, 2, 2, 2]));
        assert_eq!(grid(&arranged).len(), 4);
        assert_eq!(grid(&automatic).len(), 4);
    }

    #[test]
    fn a_panel_added_beside_the_strip_keeps_the_strip_first_and_alone() {
        // Arrange
        let mut t = tab("A=models,budget|breakdown,live");

        // Act
        let at = add(&mut t, PanelId::Sessions);

        // Assert
        assert_eq!(
            words(&t),
            ["budget", "models", "breakdown", "live", "sessions"]
        );
        assert_eq!(at, Ok(4));
        assert_eq!(t.rows, Some(vec![1, 1, 2, 1]));
    }

    #[test]
    fn the_strip_cannot_move_and_moving_other_panels_leaves_it_first() {
        // Arrange
        let mut t = tab("A=budget,models,breakdown");

        // Act
        let side = move_sideways(&mut t, 0, 1);
        let down = move_vertically(&mut t, 0, 1);
        let toggle = toggle_width(&mut t, 0);
        let moved = move_vertically(&mut t, 1, 1);

        // Assert
        assert!(side.is_err() && down.is_err() && toggle.is_err());
        assert_eq!(moved, Ok(2));
        assert_eq!(words(&t), ["budget", "breakdown", "models"]);
        assert_eq!(t.rows, None);
    }

    #[test]
    fn removing_from_an_arranged_tab_shrinks_its_row_and_drops_an_empty_one() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let first = remove(&mut t, 1);
        let second = remove(&mut t, 1);

        // Assert
        assert_eq!(first, Ok(1));
        assert_eq!(second, Ok(0));
        assert_eq!(t.rows, Some(vec![1]));
        assert_eq!(words(&t), ["spend"]);
    }

    #[test]
    fn the_last_panel_of_a_tab_cannot_be_removed() {
        // Arrange
        let mut t = tab("A=spend");

        // Act
        let result = remove(&mut t, 0);

        // Assert
        assert!(result.is_err());
        assert_eq!(words(&t), ["spend"]);
    }

    #[test]
    fn moves_in_an_automatic_tab_skip_the_budget_strip_wherever_it_is_listed() {
        // Arrange
        let mut t = tab("A=models,breakdown,budget,projection");

        // Act
        let down = move_vertically(&mut t, 0, 1);

        // Assert
        assert_eq!(down, Ok(3));
        assert_eq!(words(&t), ["projection", "breakdown", "budget", "models"]);
        assert!(move_sideways(&mut t, 2, 1).is_err());
    }

    #[test]
    fn the_width_toggle_moves_the_budget_strip_first_and_keeps_it_out_of_the_grid() {
        // Arrange
        let mut t = tab("A=models,budget,breakdown,projection");

        // Act
        let to = toggle_width(&mut t, 0);

        // Assert
        assert_eq!(to, Ok(1));
        assert_eq!(t.rows, Some(vec![1, 1, 1, 1]));
        assert_eq!(words(&t), ["budget", "models", "breakdown", "projection"]);
    }

    #[test]
    fn the_width_toggle_does_not_join_a_row_with_the_budget_strip() {
        // Arrange
        let mut t = tab("A=models,budget");

        // Act
        let on_panel = toggle_width(&mut t, 0);
        let on_strip = toggle_width(&mut t, 1);

        // Assert
        assert!(on_panel.is_err() && on_strip.is_err());
        assert_eq!(t.rows, None);
        assert_eq!(words(&t), ["models", "budget"]);
    }

    #[test]
    fn the_width_toggle_refuses_a_single_panel_row_with_only_full_rows_beside_it() {
        // Arrange
        let mut t = tab("A=spend,tokens|models|live,sessions");

        // Act
        let result = toggle_width(&mut t, 2);

        // Assert
        assert!(result.is_err());
        assert_eq!(t.rows, Some(vec![2, 1, 2]));
    }
}
