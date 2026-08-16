//! List-mode content: a clean, modern file list built on [`FileView`].
//!
//! No zebra striping — transparent rows, a subtle hover veneer, and a solid
//! accent highlight for the selection. Columns are Name (flex) / Size /
//! Modified with an inline type glyph; the header is click-to-sort.

use std::cell::Cell;
use std::rc::Rc;

use crate::command::CommandSink;
use crate::model::{Entry, SortBy, SortOrder, ViewMode};
use crate::views::file_view::{DropTargets, FileView};
use ui::widget::Widget;
use ui::SelectionModel;
use vello::kurbo::{Point, Rect};

/// Column keys in left-to-right display order (the inline type glyph lives in
/// the Name column, so it is no longer a column of its own).
pub const COLUMN_KEYS: &[&str] = &["name", "size", "modified"];

/// Build the list view bound to the live entries, selection, sort and zoom.
/// `renaming` marks the row edited in place, publishing its name-field rect
/// into `rename_rect` for the host to draw the shared editor over. `cursor`
/// seeds the view's pointer position so a press hit-tests correctly on the
/// frame it is rebuilt (see [`FileView::with_cursor`]).
#[allow(clippy::too_many_arguments)]
pub fn build_list(
    entries: &[Entry],
    selection: &SelectionModel,
    sink: &CommandSink,
    scale: f64,
    sort_by: SortBy,
    sort_order: SortOrder,
    renaming: Option<usize>,
    rename_rect: Rc<Cell<Option<Rect>>>,
    cursor: Point,
    drop_targets: DropTargets,
) -> Box<dyn Widget> {
    Box::new(
        FileView::new(entries, selection, sink)
            .mode(ViewMode::List)
            .with_scale(scale)
            .with_sort(sort_by, sort_order)
            .with_rename(renaming, rename_rect)
            .with_cursor(cursor)
            .with_drop_targets(drop_targets),
    )
}
