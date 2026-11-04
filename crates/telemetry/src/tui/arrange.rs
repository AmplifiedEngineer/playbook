// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Rearranging the panels of one tab as they sit on screen: swapping with a
//! neighbour, joining or splitting a row, adding and removing. Each operation
//! takes the cursor slot (an index into the tab's panels, in screen order),
//! checks the limits before it changes anything, and returns where the
//! cursor goes so it follows the panel it moved. A plain swap leaves the
//! rows alone, so an automatic tab stays automatic; only a width change
//! turns it into an arranged one.

use super::layout::{auto_rows, PanelId, Tab, MAX_ARRANGED_PANELS, MAX_ROWS, MAX_ROW_PANELS};

/// The size of each row as the tab is drawn.
pub fn rows(tab: &Tab) -> Vec<u8> {
    tab.rows
        .clone()
        .unwrap_or_else(|| auto_rows(tab.panels.len()))
}

/// The row and the column of `slot`.
fn locate(rows: &[u8], slot: usize) -> (usize, usize) {
    let mut start = 0;
    for (row, n) in rows.iter().enumerate() {
        let n = usize::from(*n);
        if slot < start + n {
            return (row, slot - start);
        }
        start += n;
    }
    (rows.len().saturating_sub(1), 0)
}

fn row_start(rows: &[u8], row: usize) -> usize {
    rows[..row].iter().map(|n| usize::from(*n)).sum()
}

/// Swaps the panel with its neighbour in the same row.
pub fn move_sideways(tab: &mut Tab, slot: usize, by: isize) -> Result<usize, String> {
    let rows = rows(tab);
    let (row, col) = locate(&rows, slot);
    let to = col
        .checked_add_signed(by)
        .filter(|c| *c < usize::from(rows[row]));
    let Some(to) = to else {
        return Err("The panel is at the edge of its row".to_string());
    };
    let target = row_start(&rows, row) + to;
    tab.panels.swap(slot, target);
    Ok(target)
}

/// Swaps the panel with the one above or below it, in the same column or the
/// last one of a shorter row.
pub fn move_vertically(tab: &mut Tab, slot: usize, by: isize) -> Result<usize, String> {
    let rows = rows(tab);
    let (row, col) = locate(&rows, slot);
    let Some(next) = row.checked_add_signed(by).filter(|r| *r < rows.len()) else {
        let edge = if by < 0 { "top" } else { "bottom" };
        return Err(format!("The panel is at the {edge} of the tab"));
    };
    let target = row_start(&rows, next) + col.min(usize::from(rows[next]) - 1);
    tab.panels.swap(slot, target);
    Ok(target)
}

/// Splits a row of two into two rows, or joins a row of one with a row of one
/// above or below it.
pub fn toggle_width(tab: &mut Tab, slot: usize) -> Result<usize, String> {
    if tab.panels.len() > MAX_ARRANGED_PANELS {
        return Err(format!(
            "An arranged tab holds at most {MAX_ARRANGED_PANELS} panels"
        ));
    }
    let mut rows = rows(tab);
    let (row, _) = locate(&rows, slot);
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
    tab.rows = Some(rows);
    Ok(slot)
}

/// Adds `panel` after the cursor slot: on its own full width row in an
/// arranged tab, in reading order in an automatic one.
pub fn insert_after(tab: &mut Tab, slot: usize, panel: PanelId) -> Result<usize, String> {
    if tab.panels.contains(&panel) {
        return Err(format!("{} is already on the tab", panel.word()));
    }
    let Some(mut rows) = tab.rows.clone() else {
        let at = (slot + 1).min(tab.panels.len());
        tab.panels.insert(at, panel);
        return Ok(at);
    };
    if tab.panels.len() >= MAX_ARRANGED_PANELS || rows.len() >= MAX_ROWS {
        return Err(format!(
            "An arranged tab holds at most {MAX_ARRANGED_PANELS} panels in {MAX_ROWS} rows"
        ));
    }
    let (row, _) = locate(&rows, slot);
    let at = row_start(&rows, row) + usize::from(rows[row]);
    rows.insert(row + 1, 1);
    tab.panels.insert(at, panel);
    tab.rows = Some(rows);
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
        let mut nine = tab("A=spend,tokens,models,live,sessions,projection,burnup,budget");
        nine.panels.push(PanelId::Breakdown);

        // Act
        let split = toggle_width(&mut four_rows, 0);
        let big = toggle_width(&mut nine, 0);

        // Assert
        assert!(split.is_err() && big.is_err());
        assert_eq!(four_rows.rows, Some(vec![2, 1, 1, 1]));
        assert_eq!(nine.rows, None);
    }

    #[test]
    fn adding_to_an_arranged_tab_makes_a_row_after_the_cursor_row() {
        // Arrange
        let mut t = tab("A=spend,tokens|models");

        // Act
        let at = insert_after(&mut t, 0, PanelId::Live);

        // Assert
        assert_eq!(at, Ok(2));
        assert_eq!(words(&t), ["spend", "tokens", "live", "models"]);
        assert_eq!(t.rows, Some(vec![2, 1, 1]));
    }

    #[test]
    fn adding_to_an_automatic_tab_inserts_after_the_cursor_and_stays_automatic() {
        // Arrange
        let mut t = tab("A=spend,tokens");

        // Act
        let at = insert_after(&mut t, 0, PanelId::Live);

        // Assert
        assert_eq!(at, Ok(1));
        assert_eq!(words(&t), ["spend", "live", "tokens"]);
        assert_eq!(t.rows, None);
    }

    #[test]
    fn adding_is_refused_at_eight_panels_or_four_rows_or_for_a_duplicate() {
        // Arrange
        let mut eight = tab("A=spend,tokens|models,live|sessions,projection|burnup,budget");
        let mut four = tab("A=spend|tokens|models|live");
        let mut one = tab("A=spend,tokens");

        // Act
        let full = insert_after(&mut eight, 0, PanelId::Breakdown);
        let rows = insert_after(&mut four, 0, PanelId::Sessions);
        let again = insert_after(&mut one, 0, PanelId::Spend);

        // Assert
        assert!(full.is_err() && rows.is_err() && again.is_err());
        assert_eq!((eight.panels.len(), four.panels.len()), (8, 4));
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
}
