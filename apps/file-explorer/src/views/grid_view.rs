//! Grid-mode content. Flow layout of 120x100 cards, 6 per row, each with a
//! 48-pt icon glyph above the entry name.

use crate::model::Entry;
use ui::widget::{Column, Container, Row, Text, Widget};

/// Number of grid cards per row (AUDIT.md M2).
pub const CARDS_PER_ROW: usize = 6;
/// Grid card width in logical pixels (AUDIT.md M2).
pub const CARD_WIDTH_PX: f32 = 120.0;
/// Grid card height in logical pixels (AUDIT.md M2).
pub const CARD_HEIGHT_PX: f32 = 100.0;

pub fn build_grid(entries: &[Entry], _selected: Option<usize>) -> Box<dyn Widget> {
    let mut grid = Column::new();
    let mut current_row = Row::new();
    let mut in_row = 0usize;

    for e in entries.iter() {
        let icon = Text::new(crate::icons::icon_for(e)).with_size(48.0);
        let name = Text::new(&e.name).with_size(14.0);
        let card_inner = Column::new()
            .push(Box::new(icon))
            .push(Box::new(name));
        let card = Container::new().with_child(Box::new(card_inner));
        current_row = current_row.push(Box::new(card));
        in_row += 1;
        if in_row == CARDS_PER_ROW {
            grid = grid.push(Box::new(current_row));
            current_row = Row::new();
            in_row = 0;
        }
    }
    if in_row > 0 {
        grid = grid.push(Box::new(current_row));
    }
    Box::new(grid)
}
