//! List-mode content: a clean, modern file list built on [`FileView`].
//!
//! No zebra striping — transparent rows, a subtle hover veneer, and a solid
//! accent highlight for the selection. Columns are Name (flex) / Size /
//! Modified with an inline type glyph; the header is click-to-sort.

use crate::model::{SortBy, SortOrder, ViewMode};
use crate::views::file_view::FileView;
use ui::widget::Widget;

/// Column keys in left-to-right display order (the inline type glyph lives in
/// the Name column, so it is no longer a column of its own).
pub const COLUMN_KEYS: &[&str] = &["name", "size", "modified"];

/// Present `view` — already bound to the live entries, selection, zoom and
/// in-place rename — as the sortable list, ordered by `sort_by`/`sort_order`.
pub fn build_list(view: FileView, sort_by: SortBy, sort_order: SortOrder) -> Box<dyn Widget> {
    Box::new(view.mode(ViewMode::List).with_sort(sort_by, sort_order))
}
