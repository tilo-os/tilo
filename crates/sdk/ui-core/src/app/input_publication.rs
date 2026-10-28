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

use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::bound_dispatch::BoundOwnerMark;
use crate::widget::FloatingSurfaceWidgetExt;

/// What the published tree depends on that an event's dispatch can move,
/// read before the dispatch.
pub(super) struct ResourceInputMark {
    document: (Vec<automerge::ChangeHash>, usize),
    presentation: u64,
    focused: Option<taffy::NodeId>,
    owners: Vec<BoundOwnerMark>,
    view_only_scroll: bool,
    /// The wheel may still prove view-only by a container's pan.
    may_pan: bool,
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
    /// Follow where the person acts: the pointer's place, and where the
    /// last press landed until a key is pressed. `scale` turns the event's
    /// physical pixels into the surface's logical ones.
    pub(super) fn track_acting_place(&mut self, event: &WindowEvent, scale: f64) {
        let logical = |x: f64, y: f64| vello::kurbo::Point::new(x / scale, y / scale);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.input.pointer_at = Some(logical(position.x, position.y));
            }
            WindowEvent::CursorLeft { .. } => self.input.pointer_at = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => self.input.pressed_at = self.input.pointer_at,
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => {
                let at = logical(touch.location.x, touch.location.y);
                self.input.pointer_at = Some(at);
                self.input.pressed_at = Some(at);
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                self.input.pressed_at = None;
            }
            _ => {}
        }
    }

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
    /// current tree allows. Hover and declared view-only scrolling activate
    /// nothing. Every frame refreshes availability again before it paints,
    /// so these events skip the projection, which walks every control and
    /// every published resource.
    pub(super) fn refresh_controls_before_input(&mut self, event: &WindowEvent) {
        if !self.is_bare_pointer_motion(event) && !self.is_view_only_scroll(event) {
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
        crate::widget::take_view_only_pan();
        let mark = ResourceInputMark {
            document: crate::doc::settle_mark(&self.state.doc),
            presentation: crate::widget::resource_presentation_epoch(),
            focused: self.focused_node,
            view_only_scroll,
            may_pan,
            owners: if self.is_inert_input(event) || may_pan {
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

    /// Whether a wheel `event` arrives where it can be view-only: nothing
    /// held, nothing pending, no modal or floating surface. The root may
    /// declare it so up front ([`Self::is_view_only_scroll`]); else a scroll
    /// container reports during dispatch that it only panned
    /// ([`crate::widget::note_view_only_pan`]).
    fn may_pan_view_only(&self, event: &WindowEvent) -> bool {
        matches!(event, WindowEvent::MouseWheel { .. })
            && self.input.pointer_buttons == 0
            && self.input.modifiers.state().is_empty()
            && !self.needs_sync
            && !self.layout.structure_dirty
            && self.root_widget.modal_capture().is_none()
            && self.root_widget.floating_surface_presentations().is_empty()
    }

    /// Input that activates nothing by itself: hover, a key's release, a
    /// modifier key's press, a modifier change and a wheel. Unless something
    /// consumed it, it keeps the published tree when nothing the tree depends
    /// on moved. A key's release followed every keystroke and dropped the
    /// tree its press had just republished; a wheel at the end of a list,
    /// which no scroller takes, dropped it at every notch.
    fn is_inert_input(&self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                event.state == ElementState::Released
                    || matches!(
                        event.logical_key,
                        Key::Named(
                            NamedKey::Shift
                                | NamedKey::Control
                                | NamedKey::Alt
                                | NamedKey::AltGraph
                                | NamedKey::Super
                                | NamedKey::Meta
                                | NamedKey::Hyper
                        )
                    )
            }
            WindowEvent::ModifiersChanged(_) | WindowEvent::MouseWheel { .. } => true,
            _ => self.is_bare_pointer_motion(event),
        }
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
    /// tree unless an explicitly view-only scroll left committed documents
    /// unchanged, or the event was inert input ([`Self::is_inert_input`])
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
        let panned = crate::widget::take_view_only_pan();
        let declared = before.view_only_scroll && self.is_view_only_scroll(event);
        let view_only = allow_scroll
            && before.may_pan
            && (declared || panned)
            && before.document.1 == 0
            && before.owners.iter().all(BoundOwnerMark::is_committed);
        let unchanged = ((!consumed && self.is_inert_input(event)) || view_only)
            && crate::widget::resource_presentation_epoch() == before.presentation
            && self.focused_node == before.focused
            && crate::doc::settle_mark(&self.state.doc) == before.document
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
        unchanged && view_only
    }
}
