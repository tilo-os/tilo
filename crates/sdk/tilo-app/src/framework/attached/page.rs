//! An attached view shown as a web page: what a person's browser renders of it
//! ([`ui_core::composition::page`]), and what they do on that page, acted on
//! the view as their click or their typing would be in a window.

use super::{AttachedView, lay_out};

impl AttachedView {
    /// The view as a page shows it: its composition, laid out with everything
    /// that reached it since its last frame.
    #[must_use]
    pub fn composition(&self) -> ui_core::composition::Composition {
        self.frame().composition()
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
