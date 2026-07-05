//! List-mode content: a clean, modern file list built on [`FileView`].
//!
//! No zebra striping — transparent rows, a subtle hover veneer, and a solid
//! accent highlight for the selection. Columns are Name (flex) / Size /
//! Modified with an inline type glyph; the header is click-to-sort.

use crate::command::CommandSink;
use crate::model::{Entry, SortBy, SortOrder, ViewMode};
use crate::views::file_view::FileView;
use ui::widget::Widget;
use ui::SelectionModel;

/// Column keys in left-to-right display order (the inline type glyph lives in
/// the Name column, so it is no longer a column of its own).
pub const COLUMN_KEYS: &[&str] = &["name", "size", "modified"];

/// Build the list view bound to the live entries, selection, sort and zoom.
pub fn build_list(
    entries: &[Entry],
    selection: &SelectionModel,
    sink: &CommandSink,
    scale: f64,
    sort_by: SortBy,
    sort_order: SortOrder,
) -> Box<dyn Widget> {
    Box::new(
        FileView::new(entries, selection, sink)
            .mode(ViewMode::List)
            .with_scale(scale)
            .with_sort(sort_by, sort_order),
    )
}
