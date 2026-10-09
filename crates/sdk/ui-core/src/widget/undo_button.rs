//! The toolkit's Undo / Redo control.
//!
//! `UndoButton` reads the host-published [`UndoStatus`],
//! exposes the same availability to assistive technology, and asks the host to
//! perform the verb through the same path as Ctrl+Z. It provides a discoverable
//! pointer and screen-reader control for Undo and Redo.
//!
//! ```ignore
//! // In a toolbar row:
//! Row::new()
//!     .with_child(Box::new(UndoButton::undo()))
//!     .with_child(Box::new(UndoButton::redo()))
//! ```
//!
//! No further wiring: the host publishes the status and drains the request by
//! sweeping its `node_map`, so the control works at any depth.

use crate::widget::ModelPath;
use crate::widget::TaffyTreeExt;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::undo::{UndoAccess, UndoRequest, UndoStatus};
use crate::widget::Widget;
use crate::widget::button::FaceSizer;
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize};

/// An Undo or Redo button wired to the host's history.
///
/// Disabled — dimmed, unfocusable, and reported `disabled` to assistive
/// technology — exactly when the corresponding shortcut would do nothing.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent pointer, focus, availability, and repaint states"
)]
pub struct UndoButton {
    kind: UndoRequest,
    /// Visible label. Empty by default: the control is icon-only, and its
    /// meaning reaches a screen reader through the accessible name instead.
    label: String,
    /// Hover tooltip, which also names the shortcut so the control teaches it
    /// rather than replacing it.
    tooltip: String,
    role: ButtonRole,
    control_size: ControlSize,
    /// The face geometry seen in `build_layout`, for `measure`.
    sizer: FaceSizer,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    /// The host-published status plus this control's pending request.
    access: UndoAccess,
    /// Visual state the last paint was emitted from, so a status change (an
    /// action arriving on an empty stack) repaints instead of leaving a dimmed
    /// button over a live history.
    last_paint_state: (bool, bool, bool, bool),
    dirty: bool,
}

impl UndoButton {
    fn new(kind: UndoRequest) -> Self {
        Self {
            kind,
            label: String::new(),
            tooltip: format!("{} ({})", kind.title(), kind.shortcut()),
            role: ButtonRole::Plain,
            control_size: ControlSize::Regular,
            sizer: FaceSizer::stock(ControlSize::Regular),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            access: UndoAccess::new(),
            last_paint_state: (false, false, false, false),
            dirty: true,
        }
    }

    /// An icon-only Undo button.
    pub fn undo() -> Self {
        Self::new(UndoRequest::Undo)
    }

    /// An icon-only Redo button.
    pub fn redo() -> Self {
        Self::new(UndoRequest::Redo)
    }

    /// Which verb this control performs.
    pub fn kind(&self) -> UndoRequest {
        self.kind
    }

    /// Show a visible label beside the icon (`"Undo"`). Pass a fluent key to
    /// localise it; the accessible name follows the same string.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Override the tooltip. The default already names the shortcut.
    #[must_use]
    pub fn with_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    /// What the control does. Defaults to [`ButtonRole::Plain`]: a history
    /// control belongs in a toolbar, not competing with the primary action.
    #[must_use]
    pub fn role(mut self, role: ButtonRole) -> Self {
        self.role = role;
        self
    }

    /// Its height and label role.
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self
    }

    /// Whether the verb is available right now — what `is_enabled` and the
    /// accessible `disabled` state are both derived from, so they cannot
    /// disagree.
    pub fn is_available(&self) -> bool {
        self.access.can(self.kind)
    }

    /// The status this control last received from the host.
    pub fn status(&self) -> &UndoStatus {
        self.access.status()
    }

    fn icon(&self) -> crate::icons::Icon {
        match self.kind {
            UndoRequest::Undo => crate::icons::Icon::Undo2,
            UndoRequest::Redo => crate::icons::Icon::Redo2,
        }
    }

    /// The name an assistive technology announces. The visible label when
    /// there is one, otherwise the verb — an icon alone says nothing.
    fn accessible_name(&self) -> String {
        if self.label.is_empty() {
            self.kind.title().to_string()
        } else {
            crate::i18n!(&self.label)
        }
    }

    /// Ask the host for this control's verb. Returns whether the request was
    /// accepted — it is refused when the verb is unavailable, so an activation
    /// that raced a status change cannot ask for an undo that isn't there.
    fn activate(&mut self) -> bool {
        let accepted = self.access.request(self.kind);
        if accepted {
            self.dirty = true;
        }
        accepted
    }

    fn paint_state(&self) -> (bool, bool, bool, bool) {
        (
            self.is_hovered,
            self.is_pressed,
            self.is_focused,
            self.is_available(),
        )
    }
}

impl Widget for UndoButton {
    crate::provides!(dyn crate::undo::UndoControl);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // An icon-only control is a square of its size's height; a labelled
        // one is measured (`measure`) to hold its label.
        self.sizer = FaceSizer::of(self.control_size, theme);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "logical pixel counts far below f32 precision"
        )]
        let h = self.sizer.height() as f32;
        let w = if self.label.is_empty() {
            Dimension::length(h)
        } else {
            Dimension::auto()
        };
        #[allow(
            clippy::expect_used,
            reason = "a fixed-size leaf style is valid and Widget cannot return a layout error"
        )]
        let style = Style {
            flex_shrink: 0.0,
            size: Size {
                width: w,
                height: Dimension::length(h),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut crate::text_render::TextRenderer,
    ) -> Size<f32> {
        let label = crate::i18n!(&self.label);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "logical pixel counts far below f32 precision"
        )]
        let (width, height) = (
            self.sizer.intrinsic_width(text, Some(self.icon()), &label) as f32,
            self.sizer.height() as f32,
        );
        Size {
            width: known.width.unwrap_or(width),
            height: known.height.unwrap_or(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        #[allow(
            clippy::expect_used,
            reason = "Widget apply_layout receives the node created by build_layout"
        )]
        let layout = taffy
            .layout(node)
            .expect("build_layout registered this node with the same tree");
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        let next = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        if next != self.rect {
            self.rect = next;
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.last_paint_state = self.paint_state();
        let label = crate::i18n!(&self.label);
        let name = self.accessible_name();
        // The shared button painter, not a private one: an Undo button must be
        // indistinguishable from the toolbar buttons beside it.
        let face = ButtonFace::new(self.rect, &label)
            .role(self.role)
            .control_size(self.control_size)
            .icon(self.icon())
            .accessible_name(&name)
            .hovered(self.is_hovered && self.is_available())
            .pressed(self.is_pressed && self.is_available())
            .focused(self.is_focused && self.is_available())
            .disabled(!self.is_available());
        Button::paint(scene, transform, text_renderer, theme, &face);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Unavailable: reject input outright, matching every other disabled
        // widget. No hover feedback, no request, not consumed.
        if !self.is_available() {
            if self.is_hovered || self.is_pressed {
                self.is_hovered = false;
                self.is_pressed = false;
                self.dirty = true;
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        self.dirty = true;
                        *ctx.handled = true;
                        if pressed {
                            *ctx.focus_target = Some(ctx.node_id);
                        } else {
                            // Fires on release-while-hovered, like `Button`.
                            self.activate();
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "UndoButton"
    }

    fn test_id(&self) -> Option<&str> {
        Some(match self.kind {
            UndoRequest::Undo => "undo_button",
            UndoRequest::Redo => "redo_button",
        })
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip(&self) -> Option<&str> {
        Some(self.tooltip.as_str())
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.is_available() && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if focused != self.is_focused {
            self.is_focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// Disabled precisely when the verb is unavailable. This is what keeps
    /// keyboard focus off a dead control and, through
    /// [`accessibility`](Widget::accessibility), what an AT is told.
    fn is_enabled(&self) -> bool {
        self.is_available()
    }

    /// Keyboard (Enter / Space) and assistive-technology activation.
    ///
    /// [`perform_action_at`](Widget::perform_action_at) defaults to forwarding
    /// here with the empty key path, so an AT operating the node lands on the
    /// same request a mouse release makes.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        match action {
            crate::a11y::Action::Activate => self.activate(),
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let available = self.is_available();
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.accessible_name())
                // What this verb would actually do — `"Undo: +12 -3 chars"` —
                // or, when there is nothing, why it is unavailable. Never a
                // description of an action that is not on the stack.
                .with_description(self.access.status().description(self.kind))
                .with_state(AccessState {
                    focused: self.is_focused && available,
                    focusable: available,
                    disabled: !available,
                    ..Default::default()
                }),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.paint_state() != self.last_paint_state
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl crate::undo::UndoControl for UndoButton {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> UndoStatus) {
        self.access.publish(&status());
        // Surrender focus when the verb goes away. Undoing the last action
        // with the keyboard empties the stack under the control the user is
        // standing on: it drops out of `focusable_order` the same instant, so
        // keeping the ring would leave a focus indicator on something Tab can
        // no longer reach and an AT told it was focused.
        if !self.is_available() && self.is_focused {
            self.is_focused = false;
            self.dirty = true;
        }
        if self.paint_state() != self.last_paint_state {
            self.dirty = true;
        }
    }
    fn take_undo_request(&mut self) -> Option<UndoRequest> {
        self.access.take_request()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Action;
    use automerge::AutoCommit;
    use automerge::ROOT;

    fn button_at(kind: UndoRequest, rect: Rect) -> UndoButton {
        let mut b = match kind {
            UndoRequest::Undo => UndoButton::undo(),
            UndoRequest::Redo => UndoButton::redo(),
        };
        b.rect = rect;
        b
    }

    fn status(can_undo: bool, can_redo: bool) -> UndoStatus {
        UndoStatus {
            can_undo,
            can_redo,
            undo_summary: can_undo.then(|| "+4 -0 chars".to_string()),
            redo_summary: can_redo.then(|| "+4 -0 chars".to_string()),
        }
    }

    /// Hover, press, release over the control's centre — the real pointer
    /// path, not a shortcut into `activate`.
    fn click(b: &mut UndoButton) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let device: winit::event::DeviceId = unsafe { std::mem::zeroed() };
        let centre = b.rect.center();
        for ev in [
            WindowEvent::CursorMoved {
                device_id: device,
                position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: device,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            b.handle_event(
                &ev,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: crate::widget::Edits::lend(&mut doc),
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }
    }

    #[test]
    fn undo_controls_read_their_own_verb_and_report_availability() {
        // Case `accessibility_reports_disabled_exactly_when_the_verb_is_unavailable`.
        // The disabled state an AT is told must be the state that is real: no
        // undo on the stack means `disabled`, not focusable, and a description
        // that says so.
        {
            use crate::a11y::Role;
            let mut b = button_at(UndoRequest::Undo, Rect::new(0.0, 0.0, 32.0, 32.0));

            let node = Widget::accessibility(&b).expect("an undo button is not presentational");
            assert_eq!(node.role, Role::Button);
            assert_eq!(node.name.as_deref(), Some("Undo"));
            assert!(node.state.disabled, "an empty history disables the control");
            assert!(!node.state.focusable, "a dead control must not take focus");
            assert_eq!(node.description.as_deref(), Some("Nothing to undo"));
            assert!(!b.is_enabled(), "is_enabled must agree with the a11y state");

            crate::undo::set_undo_status(&mut b, &mut || status(true, false));
            let node = Widget::accessibility(&b).unwrap();
            assert!(!node.state.disabled, "an action on the stack enables it");
            assert!(node.state.focusable);
            assert_eq!(node.description.as_deref(), Some("Undo: +4 -0 chars"));
            assert!(b.is_enabled());
        }

        // Case `each_control_reads_its_own_verb`.
        // A Redo control must read the redo half of the status, not the undo
        // half — otherwise Redo lights up whenever anything is undoable.
        {
            let mut undo = button_at(UndoRequest::Undo, Rect::new(0.0, 0.0, 32.0, 32.0));
            let mut redo = button_at(UndoRequest::Redo, Rect::new(0.0, 0.0, 32.0, 32.0));
            crate::undo::set_undo_status(&mut undo, &mut || status(true, false));
            crate::undo::set_undo_status(&mut redo, &mut || status(true, false));
            assert!(undo.is_enabled(), "undo is available");
            assert!(!redo.is_enabled(), "redo is not, despite undo being");
            assert_eq!(
                Widget::accessibility(&redo).unwrap().description.as_deref(),
                Some("Nothing to redo")
            );
        }

        // Case `a_status_change_marks_the_control_for_repaint`.
        // A status arriving on a control that painted dimmed has to repaint it,
        // or the first undoable action leaves a live history behind a dead-looking
        // button until something else happens to invalidate the frame.
        {
            let mut b = button_at(UndoRequest::Undo, Rect::new(0.0, 0.0, 32.0, 32.0));
            b.last_paint_state = b.paint_state();
            b.dirty = false;
            assert!(!b.needs_redraw(), "nothing has changed yet");

            crate::undo::set_undo_status(&mut b, &mut || status(true, false));
            assert!(
                b.needs_redraw(),
                "gaining an undoable action must repaint the control"
            );
        }

        // Case `a_control_that_goes_unavailable_surrenders_focus`.
        // Undoing the last action with the keyboard empties the stack under the
        // control the user is standing on. It must not keep the focus ring, and
        // must not keep telling an assistive technology it is focused — it has
        // just left the focus order.
        {
            let mut b = button_at(UndoRequest::Undo, Rect::new(0.0, 0.0, 32.0, 32.0));
            crate::undo::set_undo_status(&mut b, &mut || status(true, false));
            b.set_focus(true);
            assert!(b.is_focused());
            assert!(
                Widget::accessibility(&b).unwrap().state.focused,
                "a live, focused control reports focus"
            );

            // The action is undone; the stack is empty.
            crate::undo::set_undo_status(&mut b, &mut || status(false, true));
            assert!(!b.is_focused(), "focus is surrendered with the verb");
            let node = Widget::accessibility(&b).unwrap();
            assert!(!node.state.focused, "and is not reported to an AT");
            assert!(node.state.disabled);
        }
    }

    #[test]
    fn undo_requests_share_one_route_for_pointer_and_assistive_activation() {
        // Case `clicking_requests_the_verb_only_when_it_is_available`.
        // The pointer path produces a request the host can drain — and only when
        // the verb is available.
        {
            let mut b = button_at(UndoRequest::Undo, Rect::new(0.0, 0.0, 32.0, 32.0));
            click(&mut b);
            assert_eq!(
                crate::undo::take_undo_request(&mut b),
                None,
                "a disabled control must not ask the host to undo"
            );

            crate::undo::set_undo_status(&mut b, &mut || status(true, false));
            click(&mut b);
            assert_eq!(
                crate::undo::take_undo_request(&mut b),
                Some(UndoRequest::Undo)
            );
            assert_eq!(
                crate::undo::take_undo_request(&mut b),
                None,
                "one click, one undo"
            );
        }

        // Case `assistive_activation_takes_the_same_route_as_a_click`.
        // Assistive technology reaches the same request as a mouse release —
        // `perform_action_at` forwards the empty key path here.
        {
            let mut doc = AutoCommit::new();
            let mut b = button_at(UndoRequest::Redo, Rect::new(0.0, 0.0, 32.0, 32.0));

            assert!(
                !b.perform_action_at(
                    &Action::Activate,
                    &[],
                    &mut crate::widget::Edits::lend(&mut doc),
                    &ROOT
                ),
                "an unavailable verb declines the action"
            );
            assert_eq!(crate::undo::take_undo_request(&mut b), None);

            crate::undo::set_undo_status(&mut b, &mut || status(false, true));
            assert!(b.perform_action_at(
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert_eq!(
                crate::undo::take_undo_request(&mut b),
                Some(UndoRequest::Redo)
            );
        }
    }
}
