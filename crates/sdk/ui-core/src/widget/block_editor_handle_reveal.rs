//! When the block drag handles show. They are for the pointer: moving it over
//! the document brings them up, and they fade once it has rested a few
//! seconds or the person starts typing, so they never sit beside the text
//! being written. Resting on a handle keeps it, ready to be grabbed. The fade
//! is the toolkit's [`IdleFade`]: the editor sleeps through the hold on a wake
//! deadline and asks for frames only while the handles are fading, so resting
//! handles, or gone ones, cost no frames.

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::BlockEditor;
use crate::animation::{CHROME_FADE, IdleFade};
use crate::time::{Duration, Instant};

/// How long the handles stay after the pointer last moved.
pub(super) const HANDLE_HOLD: Duration = Duration::from_secs(3);

/// The handles' fade and what the last draw painted of it.
#[derive(Clone, Copy, Debug)]
pub(super) struct HandleReveal {
    fade: IdleFade,
    /// The handles' opacity as the last draw painted them.
    drawn: f32,
    /// The theme asks for no motion: the handles go at once instead of
    /// fading.
    reduce_motion: bool,
}

impl Default for HandleReveal {
    fn default() -> Self {
        Self {
            fade: IdleFade::new(HANDLE_HOLD, CHROME_FADE),
            drawn: 0.0,
            reduce_motion: false,
        }
    }
}

impl HandleReveal {
    pub(super) fn set_reduce_motion(&mut self, reduce_motion: bool) {
        self.reduce_motion = reduce_motion;
    }

    /// The draw painted the handles at `opacity`.
    pub(super) fn drawn(&mut self, opacity: f32) {
        self.drawn = opacity;
    }

    /// The handles' opacity as the last draw painted them.
    pub(super) fn drawn_opacity(&self) -> f32 {
        self.drawn
    }
}

impl BlockEditor {
    /// Show the handles when the pointer moves over the document; hide them
    /// when the person types into it.
    pub(super) fn reveal_handles_for(
        &mut self,
        event: &WindowEvent,
        ctx: &crate::widget::EventCtx<'_>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if self.rect.contains(ctx.logical(position)) {
                    self.handle_reveal.fade.wake(crate::time::now());
                    if self.handle_reveal.drawn < 1.0 {
                        self.dirty = true;
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.is_focused
                    && event.state == ElementState::Pressed
                    && is_typing(&event.logical_key, ctx.modifiers.state()) =>
            {
                self.hide_handles();
            }
            WindowEvent::Ime(winit::event::Ime::Commit(_)) if self.is_focused => {
                self.hide_handles();
            }
            _ => {}
        }
    }

    fn hide_handles(&mut self) {
        self.handle_reveal.fade.hide();
        if self.handle_reveal.drawn > 0.0 {
            self.dirty = true;
        }
    }

    /// How visible the handles are at `now`.
    pub(super) fn handle_opacity(&self, now: Instant) -> f32 {
        let reveal = &self.handle_reveal;
        if reveal.fade.is_idle() {
            return 0.0;
        }
        let mut opacity = reveal.fade.opacity_at(now, false);
        // Resting on a handle keeps it, ready to grab.
        if opacity < 1.0 && self.hovered_drag_handle().is_some() {
            opacity = 1.0;
        }
        if reveal.reduce_motion && opacity < 1.0 {
            0.0
        } else {
            opacity
        }
    }

    /// Whether the handles need frames: only while they fade, which is
    /// decided by the clock alone. Showing and hiding them is a one-off
    /// repaint (`dirty`), and the frame after the fade's last paints them
    /// gone. Nothing here waits on a draw, so an editor its host does not
    /// draw (scrolled away, behind another tab) cannot keep asking.
    pub(super) fn handles_need_frame(&self) -> bool {
        let reveal = &self.handle_reveal;
        let held = self.hovered_drag_handle().is_some();
        reveal.fade.fading_at(crate::time::now(), held)
            // Without motion they go at once when the hold ends: one frame
            // removes them, if they were painted.
            && (!reveal.reduce_motion || reveal.drawn > 0.0)
    }

    /// Whether the drag handles are fading and need frames: the part of
    /// [`crate::widget::Widget::needs_animation_frame`] a host that answers
    /// that itself forwards.
    #[must_use]
    pub fn drag_handles_fading(&self) -> bool {
        self.handles_need_frame()
    }

    /// When the resting handles start to fade, for the host's wake deadline.
    pub(super) fn handle_fade_deadline(&self) -> Option<Instant> {
        self.handle_reveal
            .fade
            .fade_deadline_at(crate::time::now(), false)
    }

    /// When the fade is over, for the host's wake deadline: the sync it
    /// brings retires the fade and repaints the handles gone.
    pub(super) fn handle_fade_end(&self) -> Option<Instant> {
        let fade = &self.handle_reveal.fade;
        fade.fade_end().filter(|end| *end > crate::time::now())
    }

    /// Retire a fade that has run its course: the handles are gone, and if
    /// the last draw still painted some of them, one repaint removes them.
    /// Runs on every sync, so it needs no frame of its own.
    pub(super) fn retire_finished_handle_fade(&mut self) {
        let now = crate::time::now();
        let held = self.hovered_drag_handle().is_some();
        if held || !self.handle_reveal.fade.finished_at(now) {
            return;
        }
        self.handle_reveal.fade.hide();
        if self.handle_reveal.drawn > 0.0 {
            self.dirty = true;
        }
    }

    /// Test hook: the handles' opacity as the last draw painted them.
    #[cfg(test)]
    pub(super) fn handle_opacity_drawn_for_test(&self) -> f32 {
        self.handle_reveal.drawn
    }
}

/// Whether `key` writes into the document: text, or the keys that edit it.
fn is_typing(key: &Key, modifiers: winit::keyboard::ModifiersState) -> bool {
    !modifiers.control_key()
        && !modifiers.super_key()
        && !modifiers.alt_key()
        && matches!(
            key,
            Key::Character(_)
                | Key::Named(
                    NamedKey::Space
                        | NamedKey::Enter
                        | NamedKey::Tab
                        | NamedKey::Backspace
                        | NamedKey::Delete
                )
        )
}
