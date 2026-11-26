//! When an input event can have changed the published resource tree.
//!
//! The resource tree is the app's public surface: agents, the palette and
//! assistive technology read it, so it must be current after every real
//! change. Rebuilding it is the expensive part of a refresh: a framework app
//! publishes its whole resource, every child and every surface declaration.
//! Pointer motion arrives at the pointer's report rate and almost never
//! changes it, and every keystroke ends in a key release that never does, so
//! inert input (bare motion, a key's release, a modifier) keeps the cached
//! tree unless the dispatch edited a document (the app's or a bound
//! component's record), took focus, was handled, or a widget reported a
//! presentation change; so does a wheel nothing took (at the end of a
//! list). A wheel that only panned a scroll container (the
//! root declared it view-only, or the container reported the pan) keeps it
//! too when all documents are committed and unchanged; the next frame still lays out and refreshes
//! accessibility geometry. Every other event drops the tree as before.

use winit::event::WindowEvent;

#[cfg(feature = "model")]
use crate::bound_dispatch::BoundOwnerMark;
use crate::floating_surface::FloatingSurfaceWidgetExt;

/// What the published tree depends on that an event's dispatch can move,
/// read before the dispatch.
pub(super) struct ResourceInputMark {
    document: (Vec<automerge::ChangeHash>, usize),
    presentation: u64,
    focused: Option<taffy::NodeId>,
    /// Where each hosted component's record stood: only Tilo's model hosts one.
    #[cfg(feature = "model")]
    owners: Vec<BoundOwnerMark>,
    view_only_scroll: bool,
    /// The wheel may still prove view-only by a container's pan.
    may_pan: bool,
}

impl ResourceInputMark {
    /// Whether every hosted component's record was committed.
    fn owners_committed(&self) -> bool {
        #[cfg(feature = "model")]
        let committed = self.owners.iter().all(BoundOwnerMark::is_committed);
        #[cfg(not(feature = "model"))]
        let committed = true;
        committed
    }
}

impl super::App {
    /// Whether the hosted components' records stand where they stood at `before`, so what they
    /// publish carries across the input.
    fn owners_unmoved_since(&self, before: &ResourceInputMark) -> bool {
        #[cfg(feature = "model")]
        let unmoved = self.actions.bound_resource_cache.carry_across_input(
            &before.owners,
            &crate::bound_dispatch::bound_owner_marks(self.root_widget.as_ref(), &self.state.doc),
        );
        #[cfg(not(feature = "model"))]
        let unmoved = {
            let _ = before;
            true
        };
        unmoved
    }

    /// Bring every control's action availability up to date before `event`
    /// reaches the widgets, so a press or key activates only what the
    /// current tree allows. Hover and declared view-only scrolling activate
    /// nothing. Every frame refreshes availability again before it paints,
    /// so these events skip the projection, which walks every control and
    /// every published resource.
    pub(super) fn refresh_controls_before_input(&mut self, event: &WindowEvent) {
        if !self.input.pointer.is_bare_motion(event) && !self.is_view_only_scroll(event) {
            self.refresh_control_action_availability();
        }
    }

    /// Read the document owners only for inputs eligible to keep the tree.
    pub(super) fn resource_input_mark(&self, event: &WindowEvent) -> ResourceInputMark {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::InputCarry);
        let may_pan = self.may_pan_view_only(event);
        let view_only_scroll = may_pan && self.root_widget.view_only_scroll(event);
        // A pan reported by an earlier dispatch is not this event's.
        tilo_ui_base::widget::take_view_only_pan();
        let mark = ResourceInputMark {
            document: tilo_ui_base::doc::settle_mark(&self.state.doc),
            presentation: tilo_ui_base::widget::resource_presentation_epoch(),
            focused: self.focused_node,
            view_only_scroll,
            may_pan,
            #[cfg(feature = "model")]
            owners: if self.input.pointer.is_inert(event) || may_pan {
                crate::bound_dispatch::bound_owner_marks(self.root_widget.as_ref(), &self.state.doc)
            } else {
                Vec::new()
            },
        };
        trace.finish(CacheResult::NotApplicable);
        mark
    }

    fn is_view_only_scroll(&self, event: &WindowEvent) -> bool {
        self.may_pan_view_only(event) && self.root_widget.view_only_scroll(event)
    }

    /// Whether `event` arrives where it can be view-only: a wheel with no
    /// button held, or a move of the pointer with one held (a drag step);
    /// no modifier, nothing pending, no modal or floating surface. The root
    /// may declare a wheel so up front ([`Self::is_view_only_scroll`]); else
    /// a scroll container or a pane's edge reports during dispatch that it
    /// only moved what is laid out ([`tilo_ui_base::widget::note_view_only_pan`]).
    fn may_pan_view_only(&self, event: &WindowEvent) -> bool {
        let held = self.input.pointer.buttons != 0;
        let movable = match event {
            WindowEvent::MouseWheel { .. } => !held,
            WindowEvent::CursorMoved { .. } => held,
            _ => false,
        };
        movable
            && self.input.modifiers.state().is_empty()
            && !self.needs_sync
            && !self.layout.structure_dirty
            && self.root_widget.modal_capture().is_none()
            && self.root_widget.floating_surface_presentations().is_empty()
    }

    /// After the retained tree took `event`: drop the published resource
    /// tree unless an explicitly view-only scroll left committed documents
    /// unchanged, or the event was inert input ([`crate::app_core::PointerPlace::is_inert`])
    /// that nothing consumed
    /// (`consumed`: handled, a History event or a focus target) and that
    /// changed neither the app's document, any bound component's record, the
    /// focused node nor any widget's declared presentation since `before`.
    pub(super) fn invalidate_resources_after_input(
        &mut self,
        event: &WindowEvent,
        consumed: bool,
        before: &ResourceInputMark,
        allow_scroll: bool,
    ) -> bool {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::InputCarry);
        let panned = tilo_ui_base::widget::take_view_only_pan();
        let declared = before.view_only_scroll && self.is_view_only_scroll(event);
        let view_only = allow_scroll
            && before.may_pan
            && (declared || panned)
            && before.document.1 == 0
            && before.owners_committed();
        let unchanged = ((!consumed && self.input.pointer.is_inert(event)) || view_only)
            && tilo_ui_base::widget::resource_presentation_epoch() == before.presentation
            && self.focused_node == before.focused
            && tilo_ui_base::doc::settle_mark(&self.state.doc) == before.document
            && self.owners_unmoved_since(before);
        trace.finish(if unchanged {
            CacheResult::Hit
        } else {
            CacheResult::Miss
        });
        if !unchanged {
            self.invalidate_widget_resource_cache();
        }
        unchanged && view_only
    }
}
