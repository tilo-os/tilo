//! List-mode content: the file list, a framework `DataTable` hosted by
//! [`FileView`].
//!
//! The table paints the header, the rows, the selection lift and the sort
//! indicator. Columns are Name (flex) / Size / Modified with the file's icon
//! or preview in the Name column; the header is click-to-sort.

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
