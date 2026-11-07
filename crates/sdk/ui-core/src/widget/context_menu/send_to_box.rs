//! The send-to box beside a menu: where it stands, the keyboard moving
//! between it and the menu's rows, and the box a window opens with no menu
//! around it.

use vello::kurbo::Rect;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::ContextMenu;
use crate::widget::EventCtx;

impl ContextMenu {
    /// The box beside the menu, as it stands.
    #[must_use]
    pub fn send_to(&self) -> &crate::widget::SendToBox {
        &self.send_to
    }

    /// Open the box with no menu around it: a window's own send key over a
    /// selection, and a shell asking an app to send what it holds, or to
    /// report a problem, when it names whom the box goes to. `anchor` is
    /// where what is sent stands; the box stands beside it.
    pub fn open_send_to_alone(
        &mut self,
        about: Option<crate::shared_selection::SharedSelection>,
        bounds: Option<[f64; 4]>,
        draft: Option<&str>,
        to: &[crate::floating_surface::Recipient],
    ) {
        self.set_about(about);
        let cursor = self.last_cursor;
        let anchor = bounds
            .filter(|bounds| bounds.iter().all(|value| value.is_finite()))
            .map(|[x0, y0, x1, y1]| Rect::new(x0, y0, x1, y1))
            .unwrap_or_else(|| Rect::new(cursor.x, cursor.y, cursor.x, cursor.y));
        self.alone = Some(anchor);
        self.send_to.open(draft, to);
        self.send_to.focus_first();
        self.send_to.place(anchor, self.viewport);
        self.dirty = true;
    }

    /// Whether the box stands open with no menu around it.
    #[must_use]
    pub fn send_to_alone_open(&self) -> bool {
        self.alone.is_some() && self.send_to.is_open()
    }

    /// Whether the box with no menu around it is on screen: open, arriving,
    /// or leaving.
    #[must_use]
    pub fn send_to_alone_drawn(&self) -> bool {
        self.alone.is_some() && (self.send_to.is_open() || self.send_to.is_drawn())
    }

    /// Hand what the person does to the send-to box beside this menu: a
    /// press in it, a key while the keyboard is in it, and Tab between the
    /// box's fields and the menu's rows. Whether it took it.
    pub(super) fn route_to_send_to(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.send_to.is_open() {
            return false;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.send_to.contains(self.last_cursor)
        {
            self.send_to.press(self.last_cursor);
            *ctx.handled = true;
            if self.send_to.take_sent() {
                self.close();
            }
            return true;
        }
        let tab = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Tab),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        );
        if tab {
            if self.send_to.is_focused() {
                let left = !ctx.modifiers.state().shift_key();
                let _ = self.send_to.handle_event(event, ctx);
                if left && !self.send_to.is_focused() {
                    // With no menu to move to, the box is the whole of what
                    // Tab can reach: it wraps back to its first stop.
                    if self.open {
                        self.focus_first_action();
                    } else {
                        self.send_to.focus_first();
                    }
                    self.dirty = true;
                }
            } else {
                self.send_to.focus_first();
                self.dirty = true;
            }
            *ctx.handled = true;
            return true;
        }
        if self.send_to.is_focused() && self.send_to.handle_event(event, ctx) {
            if self.send_to.take_sent() {
                self.close();
            }
            *ctx.handled = true;
            return true;
        }
        false
    }

    /// The box with no menu around it: it hears the pointer and the keys
    /// until it sends, is dismissed, or the pointer presses elsewhere.
    pub(super) fn handle_alone(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
        }
        if self.route_to_send_to(event, ctx) {
            if self.send_to.take_sent() {
                self.close();
            }
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => {
                if !self.send_to_alone_open() {
                    return;
                }
                self.close();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.close();
                *ctx.handled = true;
            }
            _ => {}
        }
    }
}
