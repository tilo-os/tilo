//! Grid-mode content built on the toolkit's responsive `IconGrid`.
//!
//! Replaces the old fixed six-cards-per-row layout: the grid now reflows to
//! the available width, draws crisp file-type vector glyphs, supports
//! multi-selection, and reports clicks through the command sink.

use crate::command::{push, CommandSink, FileCommand};
use crate::model::Entry;
use ui::icons::icon_for_path;
use ui::widget::{IconGrid, IconGridItem, Widget};
use ui::SelectionModel;

/// Default icon-grid cell size (logical px). The grid derives its column
/// count from this and the available width.
pub const GRID_CELL_WIDTH: f64 = 104.0;
pub const GRID_CELL_HEIGHT: f64 = 92.0;

/// Build the responsive icon grid bound to the live selection + sink.
pub fn build_grid(entries: &[Entry], selection: &SelectionModel, sink: &CommandSink) -> Box<dyn Widget> {
    let items: Vec<IconGridItem> = entries
        .iter()
        .map(|e| IconGridItem::new(icon_for_path(&e.path, e.is_dir), e.name.clone()))
        .collect();

    let mut grid = IconGrid::new(items)
        .with_cell_size(GRID_CELL_WIDTH, GRID_CELL_HEIGHT)
        .with_selection(selection.clone());

    let click_sink = sink.clone();
    grid = grid.on_item_click(move |i, ctrl, shift| {
        push(&click_sink, FileCommand::Click { index: i, ctrl, shift });
    });
    let activate_sink = sink.clone();
    grid = grid.on_activate(move |i| {
        push(&activate_sink, FileCommand::Activate(i));
    });
    Box::new(grid)
}
