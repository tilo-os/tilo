//! The send-to box a window stands with no menu around it ([`App::open_send_box`]).

use super::App;
use crate::widget::Widget as _;

impl App {
    /// Open the send-to box with no menu around it, about `about`: what a window's own key does
    /// over a selection, and what the shell's send keys or its report ask of the window. It stands
    /// beside `bounds`, else beside what holds the keyboard, and takes the keys at once.
    pub fn open_send_box(
        &mut self,
        about: Option<crate::shared_subject::SharedSelection>,
        bounds: Option<[f64; 4]>,
        draft: Option<&str>,
        to: &[crate::floating_surface::Recipient],
        #[cfg(feature = "model")] carried: &[tilo_reference::Attached],
    ) {
        let anchor = bounds
            .filter(|bounds| bounds.iter().all(|value| value.is_finite()))
            .map(|[x0, y0, x1, y1]| vello::kurbo::Rect::new(x0, y0, x1, y1))
            .or_else(|| self.keyboard_place());
        let viewport = self
            .presence_ctx
            .viewport
            .get()
            .or_else(|| Some(self.root_widget.get_rect()));
        self.window_box
            .open_window_box(about, anchor, draft, to, viewport);
        #[cfg(feature = "model")]
        self.window_box.carry_in_window_box(carried);
        self.layout.layout_dirty = true;
    }

    /// Where the keyboard is: the item with it, deepest first, so a list answers for its focused
    /// row rather than for itself.
    pub(super) fn keyboard_place(&self) -> Option<vello::kurbo::Rect> {
        let keyboard = crate::widget::context_menu_area::keyboard_anchor;
        self.focused_node
            .and_then(|node| {
                let tree = self.tree();
                tree.inspect_node(node, keyboard).ok().flatten()
            })
            .or_else(|| keyboard(self.root_widget.as_ref()))
    }

    /// Hand `event` to the box the window stands with no menu around it; whether it took it.
    pub(super) fn window_box_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
    ) -> bool {
        let mut handled = false;
        let mut focus = None;
        self.window_box.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: scale,
                modifiers: &self.input.modifiers,
                state: crate::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self
                    .layout
                    .root_node
                    .unwrap_or_else(|| taffy::NodeId::from(0_u64)),
                taffy: &self.layout.taffy,
            },
        );
        handled
    }
}
