//! What the pointer and the keyboard do to a scroll view: dragging a thumb,
//! paging a track, the wheel and its momentum, the keys that move the view, a
//! pan, and the view those gestures are measured against.

use super::*;
use crate::widget::scrollbar::PressOutcome;
use vello::kurbo::Point;
use winit::event::{MouseScrollDelta, TouchPhase};
use winit::keyboard::{Key, NamedKey};

impl Scrollable {
    /// Cursor motion: continues an active drag, or refreshes bar
    /// hover/fade state. Distinguishes a drag from a duplicate no-op so the
    /// latter never creates an undo boundary or synchronization request.
    pub(super) fn on_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
    ) -> CursorMoveOutcome {
        let cur = Point::new(position.x / scale_factor, position.y / scale_factor);
        // A duplicate position carries no hover information. Coalesce it at
        // the viewport instead of routing it through every retained child.
        // Active drags are excluded because their geometry may have changed
        // independently between ticks while the pointer stayed still.
        if self.cursor_initialized
            && cur == self.last_cursor
            && !self.vbar.is_dragging()
            && !self.hbar.is_dragging()
        {
            return CursorMoveOutcome::Coalesced;
        }
        self.cursor_initialized = true;
        self.last_cursor = cur;
        if self.vbar.is_dragging() {
            if let Some(track) = self.vertical_track()
                && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), cur)
            {
                self.scroll_by(0.0, f32_of_f64(target) - self.offset_y);
            }
            return CursorMoveOutcome::DragConsumed;
        }
        if self.hbar.is_dragging() {
            if let Some(track) = self.horizontal_track()
                && let Some(target) = self.hbar.drag_to(track, self.h_metrics(), cur)
            {
                self.scroll_by(f32_of_f64(target) - self.offset_x, 0.0);
            }
            return CursorMoveOutcome::DragConsumed;
        }
        // Falls through so the child updates its own hover too.
        let mut changed = false;
        if let Some(track) = self.vertical_track() {
            changed |= self.vbar.update_hover(track, self.v_metrics(), cur);
        } else {
            changed |= self.vbar.set_hover(false);
        }
        if let Some(track) = self.horizontal_track() {
            changed |= self.hbar.update_hover(track, self.h_metrics(), cur);
        } else {
            changed |= self.hbar.set_hover(false);
        }
        // Only edge transitions touch the fade timer.
        let near = self.cursor_near_bar(cur);
        if near != self.pointer_near_bar {
            self.pointer_near_bar = near;
            self.wake();
            changed = true;
        }
        if changed {
            self.bar_dirty = true;
        }
        CursorMoveOutcome::Forward
    }

    /// Thumb press begins a drag; track press pages toward the click and,
    /// if it actually moved, arms a hold for auto-repeat. Returns `true`
    /// when either bar consumed the press; `focus_target`/`node_id` are
    /// handled by the caller since they don't depend on this outcome.
    pub(super) fn on_track_or_thumb_press(&mut self, cur: Point) -> bool {
        if let Some(track) = self.vertical_track() {
            match self.vbar.press(track, self.v_metrics(), cur) {
                PressOutcome::Thumb => {
                    self.scroll_dirty = true;
                    self.wake();
                    return true;
                }
                PressOutcome::Page(target) => {
                    let dir = (target - f64::from(self.offset_y)).signum();
                    self.scroll_by(0.0, f32_of_f64(target) - self.offset_y);
                    if dir != 0.0 {
                        self.hold = Some(TrackHold {
                            vertical: true,
                            dir,
                            anchor: cur,
                            next_at: deadline_after(crate::time::now(), HOLD_INITIAL_MS),
                        });
                    }
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        if let Some(track) = self.horizontal_track() {
            match self.hbar.press(track, self.h_metrics(), cur) {
                PressOutcome::Thumb => {
                    self.scroll_dirty = true;
                    self.wake();
                    return true;
                }
                PressOutcome::Page(target) => {
                    let dir = (target - f64::from(self.offset_x)).signum();
                    self.scroll_by(f32_of_f64(target) - self.offset_x, 0.0);
                    if dir != 0.0 {
                        self.hold = Some(TrackHold {
                            vertical: false,
                            dir,
                            anchor: cur,
                            next_at: deadline_after(crate::time::now(), HOLD_INITIAL_MS),
                        });
                    }
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Page/Home/End/Arrow navigation while focused. Returns `true` when
    /// the key moved the view; a non-nav key falls through to the child.
    pub(super) fn on_navigation_key(&mut self, key: &Key) -> bool {
        let page_v = self.effective_viewport_h() * PAGE_FRACTION;
        let page_h = self.viewport_w * PAGE_FRACTION;
        // A step big enough that `clamp` caps it at the content edge.
        let full = self.content_h.max(self.content_w);
        let has_v = self.axis_has_vertical();
        match key {
            Key::Named(NamedKey::PageDown) if has_v => self.scroll_by(0.0, page_v),
            Key::Named(NamedKey::PageUp) if has_v => self.scroll_by(0.0, -page_v),
            Key::Named(NamedKey::PageDown) => self.scroll_by(page_h, 0.0),
            Key::Named(NamedKey::PageUp) => self.scroll_by(-page_h, 0.0),
            Key::Named(NamedKey::Home) if has_v => self.scroll_by(0.0, -full),
            Key::Named(NamedKey::End) if has_v => self.scroll_by(0.0, full),
            Key::Named(NamedKey::Home) => self.scroll_by(-full, 0.0),
            Key::Named(NamedKey::End) => self.scroll_by(full, 0.0),
            Key::Named(NamedKey::ArrowDown) if has_v => self.scroll_by(0.0, LINE_STEP),
            Key::Named(NamedKey::ArrowUp) if has_v => self.scroll_by(0.0, -LINE_STEP),
            Key::Named(NamedKey::ArrowRight) if self.axis_has_horizontal() => {
                self.scroll_by(LINE_STEP, 0.0);
            }
            Key::Named(NamedKey::ArrowLeft) if self.axis_has_horizontal() => {
                self.scroll_by(-LINE_STEP, 0.0);
            }
            _ => return false,
        }
        true
    }

    /// Wheel delta → scroll offset; shift remaps vertical onto X for a
    /// plain wheel (a tilt wheel's own X delta passes through untouched).
    /// Returns `true` when the view actually moved, so an at-the-edge
    /// wheel can bubble to an outer/sibling scroll region.
    ///
    /// Pixel deltas (touchpads, touchscreens) follow the fingers 1:1; when
    /// the gesture's `Ended` phase arrives while they were still moving, the
    /// content keeps gliding and decelerates, unless motion is reduced.
    pub(super) fn on_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        phase: TouchPhase,
        shift: bool,
        scale_factor: f64,
    ) -> bool {
        // winit y-positive means "scroll up", but content is drawn at
        // (… − offset), so wheel-up must DECREASE offset_y — negate.
        const LINE_PX: f32 = 60.0;
        self.device_scale = scale_factor;
        // The hand takes over a jump in progress from where it is drawn.
        self.scroll_to.take_over(crate::motion::frame_now());
        let pixel = matches!(delta, MouseScrollDelta::PixelDelta(_));
        if pixel {
            match phase {
                TouchPhase::Ended => {
                    let release = self.kinetic.release(crate::time::now());
                    let now = crate::motion::frame_now();
                    // Let go past an end, the give springs back instead.
                    if self.band.is_stretched(now) {
                        let (vx, vy) = release.unwrap_or_default();
                        self.band.release(
                            vello::kurbo::Vec2::new(vx, vy),
                            self.band_viewport(),
                            self.motion,
                            now,
                        );
                        self.scroll_dirty = true;
                        return true;
                    }
                    return match release {
                        Some((vx, vy)) if !self.reduce_motion_active => {
                            self.fling(f32_of_f64(vx), f32_of_f64(vy));
                            true
                        }
                        _ => false,
                    };
                }
                TouchPhase::Cancelled => {
                    self.kinetic.reset();
                    if self.band.is_stretched(crate::motion::frame_now()) {
                        self.band.release(
                            vello::kurbo::Vec2::ZERO,
                            self.band_viewport(),
                            self.motion,
                            crate::motion::frame_now(),
                        );
                        self.scroll_dirty = true;
                    }
                    return false;
                }
                TouchPhase::Started => self.kinetic.reset(),
                TouchPhase::Moved => {}
            }
        } else {
            self.kinetic.reset();
        }
        // Fingers (or a wheel) back on the input catch a glide in progress.
        self.fling(0.0, 0.0);
        let (mut dx, mut dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (*x * -LINE_PX, *y * -LINE_PX),
            MouseScrollDelta::PixelDelta(pp) => (
                -f32_of_f64(pp.x / scale_factor),
                -f32_of_f64(pp.y / scale_factor),
            ),
        };
        if shift && self.axis_has_horizontal() && dx == 0.0 {
            dx = dy;
            dy = 0.0;
        }
        if pixel {
            return self.pan(dx, dy);
        }
        let before = (self.offset_x, self.offset_y);
        self.scroll_by(dx, dy);
        self.offset_x.to_bits() != before.0.to_bits()
            || self.offset_y.to_bits() != before.1.to_bits()
    }

    /// Fingers moved the content by `dx`, `dy`: one to one inside, and past
    /// an end through the rubber band; moving back takes the give in first.
    /// Returns whether anything moved.
    pub(super) fn pan(&mut self, dx: f32, dy: f32) -> bool {
        let now = crate::motion::frame_now();
        let hand = vello::kurbo::Vec2::new(f64::from(dx), f64::from(dy));
        let viewport = self.band_viewport();
        let rest = self.band.take_in(hand, viewport, self.motion, now);
        let took_in = rest != hand;
        let before = (self.offset_x, self.offset_y);
        self.scroll_by(f32_of_f64(rest.x), f32_of_f64(rest.y));
        let moved = vello::kurbo::Vec2::new(
            f64::from(self.offset_x - before.0),
            f64::from(self.offset_y - before.1),
        );
        // What the content could not take, on an axis it scrolls along.
        let excess = vello::kurbo::Vec2::new(
            if self.axis_has_horizontal() && self.content_w > self.viewport_w + FIT_EPS {
                rest.x - moved.x
            } else {
                0.0
            },
            if self.axis_has_vertical() && self.max_offset_y() > 0.0 {
                rest.y - moved.y
            } else {
                0.0
            },
        );
        if excess != vello::kurbo::Vec2::ZERO {
            self.band.pull(excess, viewport, self.motion, now);
        }
        let gave = took_in || self.band.is_stretched(now);
        if gave {
            self.scroll_dirty = true;
            self.cursor_initialized = false;
            self.wake();
        }
        if moved == vello::kurbo::Vec2::ZERO && !gave {
            self.kinetic.reset();
            return false;
        }
        self.kinetic.record(hand.x, hand.y, crate::time::now());
        true
    }

    /// The view the give is measured against.
    pub(super) fn band_viewport(&self) -> vello::kurbo::Vec2 {
        vello::kurbo::Vec2::new(
            f64::from(self.viewport_w),
            f64::from(self.effective_viewport_h()),
        )
    }
}
