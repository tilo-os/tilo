//! A finger on a text field. Nothing happens where it comes down: a lift
//! within the tap's slop is a tap, a drag across the text selects, and a
//! drag up or down is let go, so what holds the field scrolls and the field
//! neither takes focus nor brings up the keyboard.

use super::TextInput;
use crate::widget::EventCtx;
use taffy::NodeId;
use vello::kurbo::Point;
use winit::event::{Modifiers, MouseScrollDelta, TouchPhase, WindowEvent};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum TouchIntent {
    Undecided,
    Selecting,
    /// Another finger joined, or an open menu took it: the field keeps the
    /// contact but neither taps nor selects with it.
    Withdrawn,
}

impl TextInput {
    pub(super) fn touch_takes_wheel(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        if self.active_touch.is_none() {
            return false;
        }
        if self.touch_intent == TouchIntent::Undecided
            && let MouseScrollDelta::PixelDelta(moved) = delta
            && self.decide_touch_drag(moved.x, moved.y, ctx.scale_factor)
        {
            self.handle_left_press(
                Modifiers::default(),
                ctx.focus_target,
                ctx.node_id,
                ctx.handled,
            );
        }
        let takes = self.active_touch.is_some() && self.touch_intent == TouchIntent::Selecting;
        if takes {
            *ctx.handled = true;
        }
        takes
    }

    pub(super) fn handle_touch_event(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            TouchPhase::Started if self.active_touch.is_none() && self.rect.contains(point) => {
                self.active_touch = Some(touch.id);
                self.touch_origin = touch.location;
                self.touch_intent = if self.context_menu.is_open() {
                    TouchIntent::Withdrawn
                } else {
                    TouchIntent::Undecided
                };
                self.last_mouse_pos = point;
                self.is_hovered = true;
                *handled = true;
            }
            TouchPhase::Started if self.active_touch.is_some() => {
                if self.touch_intent == TouchIntent::Undecided {
                    self.touch_intent = TouchIntent::Withdrawn;
                }
                *handled = true;
            }
            TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                let dx = touch.location.x - self.touch_origin.x;
                let dy = touch.location.y - self.touch_origin.y;
                if self.touch_intent == TouchIntent::Undecided
                    && dx.hypot(dy) > crate::interaction::TouchPointerEmulator::TAP_SLOP
                    && self.decide_touch_drag(dx, dy, scale_factor)
                {
                    self.handle_left_press(Modifiers::default(), focus_target, node_id, handled);
                }
                if self.touch_intent == TouchIntent::Selecting {
                    self.handle_cursor_moved(touch.location, scale_factor, handled);
                }
                if self.active_touch.is_some() {
                    *handled = true;
                }
            }
            TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                let intent = self.touch_intent;
                self.active_touch = None;
                self.is_hovered = self.rect.contains(point);
                if intent == TouchIntent::Undecided {
                    self.last_mouse_pos = point;
                    self.touch_tap(focus_target, node_id, handled);
                }
                if self.is_dragging {
                    self.last_mouse_pos = point;
                    self.end_drag();
                }
                *handled = true;
            }
            TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                self.active_touch = None;
                self.is_dragging = false;
                *handled = true;
            }
            _ if self.active_touch.is_some() => {
                // A secondary finger cannot leak to a control below while
                // this field owns the selection gesture.
                *handled = true;
            }
            _ => {}
        }
    }

    /// The owned finger travelled `dx`, `dy` window pixels past the tap's
    /// slop. Across the text, it selects from where it came down: true, and
    /// the caller presses there. Up or down, the field lets it go, and the
    /// toolkit's scroll of it moves what holds the field (or the lines of a
    /// field that scrolls itself).
    fn decide_touch_drag(&mut self, dx: f64, dy: f64, scale_factor: f64) -> bool {
        if dx.abs() > dy.abs() {
            self.touch_intent = TouchIntent::Selecting;
            self.last_mouse_pos = Point::new(
                self.touch_origin.x / scale_factor,
                self.touch_origin.y / scale_factor,
            );
            true
        } else {
            self.active_touch = None;
            false
        }
    }

    fn touch_tap(
        &mut self,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) {
        if self.context_menu.is_open() {
            return;
        }
        if self.is_focused
            && self
                .suggestion_chip
                .is_some_and(|chip| chip.contains(self.last_mouse_pos))
            && self.accept_suggestion()
        {
            *handled = true;
            return;
        }
        if self.press_accessory() {
            *handled = true;
            return;
        }
        self.handle_left_press(Modifiers::default(), focus_target, node_id, handled);
    }

    pub(super) fn end_drag(&mut self) {
        self.is_dragging = false;
        self.drag_select.release();
        self.publish_primary_selection();
        if self.read_only && !self.has_selection() {
            let byte = self.resolve_position(self.last_mouse_pos).byte;
            if let Some(target) = self.link_at(byte) {
                crate::widget::open_external_url(&target);
            }
        }
        self.report_selection();
    }
}
