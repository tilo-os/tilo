//! An attached view shown as a web page: what a person's browser renders of it
//! ([`ui_core::composition::page`]), and what they do on that page, acted on
//! the view as their click or their typing would be in a window.

use super::{AttachedView, lay_out};

/// The most frames a read of the page lays out to take up what its view owes: a wake
/// that keeps asking for frames (an animation) is not waited out.
const PAGE_FRAMES: usize = 8;

impl AttachedView {
    /// Show the item in a workspace page link, as a native window opened on that item does.
    ///
    /// # Errors
    /// The mounted app has no item opener or its view refuses the item.
    pub fn show_page_item(&mut self, item: &str) -> Result<(), String> {
        let root = self
            .frame
            .get_mut()
            .widget_mut::<super::super::root::ResourceRoot>()
            .ok_or("the page holds no resource view")?;
        root.show_item(item)?;
        self.settle_acted();
        Ok(())
    }

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

    /// Act on the view as a person does on its page: `action` on the widget at
    /// `widget` (the path its element stands for), at the semantic `target` it
    /// carries. The person acts on what they see, so what reached the view is
    /// laid out first; what the action did is taken up as an operation's is,
    /// and laid out when the page is next read.
    ///
    /// # Errors
    /// The view has no such widget, or it refuses the action.
    pub fn act_on_page(
        &mut self,
        widget: &str,
        target: Option<&str>,
        action: &ui_core::a11y::Action,
    ) -> Result<(), String> {
        if self.unsettled.replace(false) {
            self.merged.set(false);
            lay_out(self.frame.get_mut());
        }
        self.moved();
        let acted = self
            .frame
            .get_mut()
            .perform_semantic_named(widget, target, action);
        self.settle_acted();
        acted
    }
}
