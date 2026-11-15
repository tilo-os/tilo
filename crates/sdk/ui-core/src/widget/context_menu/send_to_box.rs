//! The send-to box: where it stands, the keyboard moving round its own fields,
//! the box the send row opens beside the menu, and the box a window opens with
//! no menu around it.

use vello::kurbo::Rect;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::ContextMenu;
use crate::widget::EventCtx;

impl ContextMenu {
    /// The box, as it stands.
    #[must_use]
    pub fn send_to(&self) -> &crate::widget::SendToBox {
        &self.send_to
    }

    /// Stand the box with no menu around it — a window's own key over a
    /// selection, the shell's send keys, a report — on the window's own menu,
    /// which never opens rows of its own: the window hands it the pointer and
    /// the keys while it stands ([`Self::send_to_alone_open`]), draws it over
    /// everything and describes it. It stands beside `anchor`, in `viewport`.
    /// Only the native window stands it ([`crate::app`]).
    #[cfg(feature = "native")]
    pub(crate) fn open_window_box(
        &mut self,
        about: Option<crate::shared_selection::SharedSelection>,
        anchor: Option<Rect>,
        draft: Option<&str>,
        to: &[crate::floating_surface::Recipient],
        viewport: Option<Rect>,
    ) {
        self.set_about(about);
        self.viewport = viewport.or(self.viewport);
        let cursor = self.last_cursor;
        let anchor = anchor.unwrap_or_else(|| Rect::new(cursor.x, cursor.y, cursor.x, cursor.y));
        self.alone = Some(anchor);
        // Whom it goes to is named as it opens — the people or the place the
        // asker named, else a new agent — so the person's next keystroke is
        // the message's, after the sentence it may carry.
        let draft: String = draft
            .unwrap_or_default()
            .chars()
            .take(crate::shared_selection::MAX_SEND_CHARS)
            .collect();
        self.send_to.open(Some(&draft), to);
        self.send_to.stand_at_rest();
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

    /// Hand what the person does to the open box before the menu hears it: a
    /// press in it, a key while the keyboard is in it, Tab round the box's own
    /// fields and back to its first, and Escape, which puts away the box (and
    /// a box beside the menu only, as a nested menu's Escape does). Whether it
    /// took it.
    pub(super) fn route_to_send_to(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        if !self.send_to.is_open() {
            return false;
        }
        let key = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => Some(*key),
            _ => None,
        };
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.send_to.contains(self.last_cursor)
        {
            self.send_to.press(self.last_cursor);
        } else if key == Some(NamedKey::Escape) {
            if self.alone.is_some() {
                self.close();
            } else {
                self.send_to.close();
                self.sync_rows();
            }
        } else if key == Some(NamedKey::Tab) {
            // The box is the whole of what Tab reaches: past its last stop it
            // wraps back to its first.
            if !self.send_to.is_focused()
                || !self.send_to.handle_event(event, ctx)
                || !self.send_to.is_focused()
            {
                self.send_to.focus_first();
            }
        } else if !(self.send_to.is_focused() && self.send_to.handle_event(event, ctx)) {
            return false;
        }
        if self.send_to.take_sent() {
            self.close();
        }
        self.dirty = true;
        *ctx.handled = true;
        true
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
        if !self.route_to_send_to(event, ctx)
            && self.send_to_alone_open()
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
        {
            self.close();
            *ctx.handled = true;
        }
    }
}
