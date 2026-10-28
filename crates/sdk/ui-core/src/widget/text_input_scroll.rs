//! How a multi-line field scrolls: by a wheel's notch, 1:1 under fingers on
//! a touchpad or a screen, and gliding on after they lift.

use super::TextInput;

impl TextInput {
    pub fn scroll_y(&self) -> f64 {
        self.scroll_y
    }

    /// The scroll the text is drawn at: where it is scrolled, and past an
    /// end by the give fingers pull it there with
    /// ([`crate::motion::RubberBand`]).
    pub(crate) fn shown_scroll_y(&self) -> f64 {
        self.scroll_y + self.wheel_glide.give(crate::motion::frame_now())
    }

    /// No-op for a single-line field.
    pub fn scroll_by(&mut self, dy: f64) {
        if !self.multiline {
            return;
        }
        let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
        let max = (self.content_height() - self.multiline_padding_y() * 2.0 - view_h).max(0.0);
        self.scroll_y = (self.scroll_y + dy).clamp(0.0, max);
    }

    /// A wheel's notch scrolls a line; fingers on a touchpad or a screen
    /// move the text 1:1 in logical pixels and, lifted while moving, leave
    /// it gliding to a stop (the shared [`ScrollMomentum`]).
    ///
    /// [`ScrollMomentum`]: crate::virtual_viewport::ScrollMomentum
    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: &winit::event::MouseScrollDelta,
        phase: winit::event::TouchPhase,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        use crate::virtual_viewport::WheelStep;
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => {
                self.wheel_glide.stop();
                -f64::from(*y) * self.line_height()
            }
            winit::event::MouseScrollDelta::PixelDelta(_) => {
                self.wheel_glide.set_device_scale(scale_factor);
                let view_h = self.rect.height() - self.multiline_padding_y() * 2.0;
                let room = self.content_height() - self.multiline_padding_y() * 2.0 - view_h;
                self.wheel_glide.set_view(
                    view_h,
                    self.multiline && room > 0.0,
                    crate::tokens::EffectiveMotion::new(
                        crate::tokens::Motion::default(),
                        self.reduce_motion,
                    ),
                );
                match self.wheel_glide.wheel(delta, phase, self.reduce_motion) {
                    WheelStep::Scroll(dy) => dy,
                    WheelStep::Fling => {
                        *handled = true;
                        return;
                    }
                    WheelStep::Ignored => return,
                }
            }
        };
        let before = self.scroll_y;
        self.scroll_by(dy);
        let moved = self.wheel_glide.wheel_applied(dy, self.scroll_y - before);
        if moved {
            self.hand_scrolled = true;
            *handled = true;
        }
    }

    /// A glide after the fingers lifted moves the text on, frame by frame.
    pub(super) fn advance_wheel_glide(&mut self) {
        if !self.wheel_glide.is_active() {
            return;
        }
        let delta = self.wheel_glide.advance(crate::motion::frame_now());
        let before = self.scroll_y;
        self.scroll_by(delta);
        if self.scroll_y.to_bits() == before.to_bits() {
            if delta != 0.0 {
                self.wheel_glide.stop();
            }
        } else {
            self.hand_scrolled = true;
        }
    }
}
