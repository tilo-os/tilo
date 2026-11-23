//! An attached view shown as a web page: what a person's browser renders of it
//! ([`ui_core::composition::page`]).

use super::AttachedView;

/// The most frames a read of the page lays out to take up what its view owes: a wake
/// that keeps asking for frames (an animation) is not waited out.
const PAGE_FRAMES: usize = 8;

impl AttachedView {
    /// The view as a page shows it: its composition, laid out with everything
    /// that reached it since its last frame, and with the frames that take up
    /// (a store that opened once the workspace answered, a note an action
    /// opened) laid out as a window lays them out while it is looked at, and
    /// a picture of each part a page shows as one (a chart's marks).
    #[must_use]
    pub fn composition(&self) -> ui_core::composition::Composition {
        drop(self.frame());
        let _still = ui_core::arriving::still_frames();
        let mut frame = self.frame.borrow_mut();
        for _ in 0..PAGE_FRAMES {
            if !frame.poll_frame().unwrap_or(false) {
                break;
            }
        }
        let mut composition = frame.composition();
        frame.paint_pictures(&mut composition);
        composition
    }

    /// The theme the view is laid out in: what a page's colours are.
    #[must_use]
    pub fn theme(&self) -> ui_core::Theme {
        self.frame_as_is().theme().clone()
    }
}
