//! Grid-mode content: a responsive grid of files built on [`FileView`].
//!
//! Items flow left-to-right and wrap to fill the available width (the view
//! fills its parent's width, so the column count reflects the real pane width
//! instead of collapsing to a single column). Each is its icon or preview with
//! the name under it, wrapped onto a second line rather than cut; selection is
//! the shared lift; double-click opens.

use crate::model::ViewMode;
use crate::views::file_view::FileView;
use ui::widget::Widget;

/// Present `view` — already bound to the live entries, selection, zoom and
/// in-place rename — as the responsive grid.
pub fn build_grid(view: FileView) -> Box<dyn Widget> {
    Box::new(view.mode(ViewMode::Grid))
}
