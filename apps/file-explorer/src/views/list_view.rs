//! List mode: the file list.

use crate::model::{SortBy, SortOrder, ViewMode};
use crate::views::file_view::FileView;
use ui::widget::Widget;

pub fn build_list(view: FileView, sort_by: SortBy, sort_order: SortOrder) -> Box<dyn Widget> {
    Box::new(view.mode(ViewMode::List).with_sort(sort_by, sort_order))
}
