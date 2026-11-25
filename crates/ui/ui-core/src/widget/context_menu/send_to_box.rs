//! The send-to box: where it stands, the keyboard moving round its own fields,
//! the box the send row opens in the menu's place, and the box a window opens
//! with no menu around it.

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
    /// the keys while it stands ([`Self::is_open`]), draws it over
    /// everything and describes it. It stands beside `anchor`, in `viewport`.
    /// Only the native window stands it ([`crate::app`]).
    pub(crate) fn open_window_box(
        &mut self,
        about: Option<crate::shared_selection::SharedSelection>,
        anchor: Option<Rect>,
        draft: Option<&str>,
        to: &[crate::floating_surface::Recipient],
        carried: &[tilo_reference::Attached],
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
        self.send_to.open(draft, to);
        self.send_to.carry(carried);
        self.send_to.stand_at_rest();
        self.send_to.place(anchor, self.viewport);
        self.dirty = true;
    }

    /// Whether the box is on screen: open, arriving, or leaving.
    pub(super) fn send_to_alone_drawn(&self) -> bool {
        self.alone.is_some() && (self.send_to.is_open() || self.send_to.is_drawn())
    }

    /// The box hears the pointer and the keys until it sends, is put away
    /// (Escape), or the pointer presses outside it. Tab goes round its own
    /// fields.
    pub(super) fn handle_alone(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } if !self.send_to.contains(self.last_cursor) => self.close(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.send_to.press(self.last_cursor);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.close(),
            _ => {
                if !self.send_to.handle_event(event, ctx) {
                    return;
                }
            }
        }
        if self.send_to.take_sent() {
            self.close();
        }
        self.dirty = true;
        *ctx.handled = true;
    }
}
