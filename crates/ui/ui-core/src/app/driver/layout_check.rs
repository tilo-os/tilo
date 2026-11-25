//! What a layout check reads off a driven app beside its frame: the window laid out for the
//! person and the size it is read at in one frame, what its frames built, and what a capture
//! still waits for.

use super::Driver;
use crate::widget::Widget;

impl Driver {
    /// Lay the app out at `viewport` under `theme` for a person using `input`, in one frame: the
    /// window is built for the person and the size it is read at, where
    /// [`Self::set_interaction_environment`] would first lay it out at the size it had.
    ///
    /// # Errors
    /// As [`Self::relayout_at`].
    pub fn relayout_for(
        &mut self,
        viewport: (u32, u32),
        theme: crate::theme::Theme,
        input: crate::interaction::InteractionEnvironment,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.app.set_interaction_environment(input);
        // A widget takes up the way it is reached when it is built.
        self.app.layout.request_rebuild();
        self.relayout_themed(viewport, theme)
    }

    /// Layout nodes the app's frames built since it was made: every node of each tree a frame
    /// built again, and each node a change carried out in place mounted. A frame that kept its
    /// tree builds none.
    #[must_use]
    pub fn layout_nodes_built(&self) -> u64 {
        self.app.layout.frames.nodes_built
    }

    /// The widgets waiting on work whose completion will replace a placeholder
    /// ([`Widget::has_pending_visual_work`]) that none of their children explains, each as its path
    /// of names from the app's root: what a capture waits for that asks for no frame of its own.
    #[must_use]
    pub fn pending_visual_work(&self) -> Vec<String> {
        fn visit(widget: &dyn Widget, path: &mut Vec<String>, out: &mut Vec<String>) {
            let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
                return;
            };
            if !widget.has_pending_visual_work() {
                return;
            }
            path.push(widget.name().to_owned());
            let before = out.len();
            widget.for_each_child(&mut |child| visit(child, path, out));
            if out.len() == before {
                out.push(path.join("/"));
            }
            path.pop();
        }
        let mut out = Vec::new();
        visit(self.app.root_widget.as_ref(), &mut Vec::new(), &mut out);
        out
    }
}
