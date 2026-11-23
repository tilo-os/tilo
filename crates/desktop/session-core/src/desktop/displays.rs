//! Where the displays stand in the shell's coordinates.

use tilo_desktop_model::{Display, Layout};
use ui::kurbo::Rect;

/// A display's rectangle, in the shell's coordinates.
#[must_use]
pub fn display_rect(display: &Display) -> Rect {
    Rect::new(
        display.x,
        display.y,
        display.x + display.width,
        display.y + display.height,
    )
}

/// The display the person works on, in the shell's coordinates, while the shell spans several: the
/// one every shell surface without a place of its own stands on. `None` on one display.
#[must_use]
pub fn working_display(tree: &Layout) -> Option<Rect> {
    if !tree.is_partitioned() {
        return None;
    }
    let focused = tree.outputs.focused_display()?;
    tree.outputs
        .displays
        .iter()
        .find(|display| display.id == focused)
        .map(display_rect)
}
