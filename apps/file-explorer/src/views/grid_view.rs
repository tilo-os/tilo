//! Grid mode: a responsive grid of files.

use crate::model::ViewMode;
use crate::views::file_view::FileView;
use ui::widget::Widget;

pub fn build_grid(view: FileView) -> Box<dyn Widget> {
    Box::new(view.mode(ViewMode::Grid))
}
