//! Grid-mode content: a responsive icon grid built on [`FileView`].
//!
//! Items flow left-to-right and wrap to fill the available width (the view
//! fills its parent's width, so the column count reflects the real pane width
//! instead of collapsing to a single column). Icons + labels, hover veneer,
//! accent selection, double-click to open.

use std::cell::Cell;
use std::rc::Rc;

use crate::command::CommandSink;
use crate::model::{Entry, ViewMode};
use crate::views::file_view::FileView;
use ui::widget::Widget;
use ui::SelectionModel;
use vello::kurbo::{Point, Rect};

/// Default icon-grid cell size (logical px, at zoom level 0).
pub const GRID_CELL_WIDTH: f64 = 112.0;
pub const GRID_CELL_HEIGHT: f64 = 104.0;

/// Build the grid view bound to the live entries, selection and zoom.
/// `renaming` marks the cell edited in place, publishing its label rect into
/// `rename_rect` for the host to draw the shared editor over. `cursor` seeds the
/// view's pointer position so a press hit-tests correctly on the frame it is
/// rebuilt (see [`FileView::with_cursor`]).
#[allow(clippy::too_many_arguments)]
pub fn build_grid(
    entries: &[Entry],
    selection: &SelectionModel,
    sink: &CommandSink,
    scale: f64,
    renaming: Option<usize>,
    rename_rect: Rc<Cell<Option<Rect>>>,
    cursor: Point,
) -> Box<dyn Widget> {
    Box::new(
        FileView::new(entries, selection, sink)
            .mode(ViewMode::Grid)
            .with_scale(scale)
            .with_rename(renaming, rename_rect)
            .with_cursor(cursor),
    )
}
