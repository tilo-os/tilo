//! Grid-mode content: a responsive icon grid built on [`FileView`].
//!
//! Items flow left-to-right and wrap to fill the available width (the view
//! fills its parent's width, so the column count reflects the real pane width
//! instead of collapsing to a single column). Icons + labels, hover veneer,
//! accent selection, double-click to open.

use crate::command::CommandSink;
use crate::model::{Entry, ViewMode};
use crate::views::file_view::FileView;
use ui::widget::Widget;
use ui::SelectionModel;

/// Default icon-grid cell size (logical px, at zoom level 0).
pub const GRID_CELL_WIDTH: f64 = 112.0;
pub const GRID_CELL_HEIGHT: f64 = 104.0;

/// Build the grid view bound to the live entries, selection and zoom.
pub fn build_grid(
    entries: &[Entry],
    selection: &SelectionModel,
    sink: &CommandSink,
    scale: f64,
) -> Box<dyn Widget> {
    Box::new(
        FileView::new(entries, selection, sink)
            .mode(ViewMode::Grid)
            .with_scale(scale),
    )
}
