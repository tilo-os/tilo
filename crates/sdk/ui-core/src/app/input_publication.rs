//! When an input event can have changed the published resource tree.
//!
//! The resource tree is the app's public surface: agents, the palette and
//! assistive technology read it, so it must be current after every real
//! change. Rebuilding it is the expensive part of a refresh: a framework app
//! publishes its whole resource, every child and every surface declaration.
//! Pointer motion arrives at the pointer's report rate and almost never
//! changes it, so bare motion keeps the cached tree unless the dispatch
//! edited a document (the app's or a bound component's record), took focus,
//! was handled, or a widget reported a presentation change. Every other event
//! drops the tree as before.

use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::bound_dispatch::BoundOwnerMark;

/// What the published tree depends on that an event's dispatch can move,
/// read before the dispatch.
pub(super) struct ResourceInputMark {
    document: (Vec<automerge::ChangeHash>, usize),
    presentation: u64,
    focused: Option<taffy::NodeId>,
    owners: Vec<BoundOwnerMark>,
}

/// The bit for `button` in [`super::runtime::InputRuntime::pointer_buttons`].
fn button_bit(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 1 << 1,
        MouseButton::Middle => 1 << 2,
        MouseButton::Back => 1 << 3,
        MouseButton::Forward => 1 << 4,
        MouseButton::Other(n) => 1 << (5 + u32::from(n) % 27),
    }
}

impl super::App {
    /// Follow which pointer buttons are down. Motion with a button held is a
    /// drag, which may move what a widget declares (a selection, a split)
    /// without marking each move handled, so it never keeps the tree.
    pub(super) fn track_pointer_buttons(&mut self, event: &WindowEvent) {
        if let WindowEvent::MouseInput { state, button, .. } = event {
            let bit = button_bit(*button);
            match state {
                ElementState::Pressed => self.input.pointer_buttons |= bit,
                ElementState::Released => self.input.pointer_buttons &= !bit,
            }
        }
    }

    /// Bring every control's action availability up to date before `event`
    /// reaches the widgets, so a press or key activates only what the
    /// current tree allows. Hover activates nothing, and every frame
    /// refreshes availability again before it paints, so bare pointer motion
    /// skips the projection, which walks every control and every published
    /// resource.
    pub(super) fn refresh_controls_before_input(&mut self, event: &WindowEvent) {
        if !self.is_bare_pointer_motion(event) {
            self.refresh_control_action_availability();
        }
    }

    /// Read what `event`'s dispatch could move. Only bare pointer motion can
    /// keep the tree, so any other event skips the bound owners' walk.
    pub(super) fn resource_input_mark(&self, event: &WindowEvent) -> ResourceInputMark {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::InputCarry);
        let mark = ResourceInputMark {
            document: crate::widget::settle_mark(&self.state.doc),
            presentation: crate::widget::resource_presentation_epoch(),
            focused: self.focused_node,
            owners: if self.is_bare_pointer_motion(event) {
                crate::bound_dispatch::bound_owner_marks(self.root_widget.as_ref(), &self.state.doc)
            } else {
                Vec::new()
            },
        };
        trace.finish(CacheResult::NotApplicable);
        mark
    }

    /// Whether `event` is pointer motion with no button held: hover.
    fn is_bare_pointer_motion(&self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CursorMoved { .. } => self.input.pointer_buttons == 0,
            WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => true,
            _ => false,
        }
    }

    /// After the retained tree took `event`: drop the published resource
    /// tree unless the event was bare pointer motion that nothing consumed
    /// (`consumed`: handled, a History event or a focus target) and that
    /// changed neither the app's document, any bound component's record, the
    /// focused node nor any widget's declared presentation since `before`.
    pub(super) fn invalidate_resources_after_input(
        &mut self,
        event: &WindowEvent,
        consumed: bool,
        before: &ResourceInputMark,
    ) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::InputCarry);
        let unchanged = !consumed
            && self.is_bare_pointer_motion(event)
            && crate::widget::resource_presentation_epoch() == before.presentation
            && self.focused_node == before.focused
            && crate::widget::settle_mark(&self.state.doc) == before.document
            && self.actions.bound_resource_cache.carry_across_input(
                &before.owners,
                &crate::bound_dispatch::bound_owner_marks(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                ),
            );
        trace.finish(if unchanged {
            CacheResult::Hit
        } else {
            CacheResult::Miss
        });
        if !unchanged {
            self.invalidate_widget_resource_cache();
        }
    }
}
