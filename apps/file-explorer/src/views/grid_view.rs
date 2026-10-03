//! Grid-mode content: a responsive icon grid built on [`FileView`].
//!
//! Items flow left-to-right and wrap to fill the available width (the view
//! fills its parent's width, so the column count reflects the real pane width
//! instead of collapsing to a single column). Icons + labels, hover veneer,
//! accent selection, double-click to open.

use crate::model::ViewMode;
use crate::views::file_view::FileView;
use ui::widget::Widget;

/// Present `view` — already bound to the live entries, selection, zoom and
/// in-place rename — as the responsive icon grid.
pub fn build_grid(view: FileView) -> Box<dyn Widget> {
    Box::new(view.mode(ViewMode::Grid))
}
