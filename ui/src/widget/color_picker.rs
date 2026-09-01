//! Event-driven: each press latches a [`DragTarget`] so a drag continues
//! to track the cursor even after it leaves the widget rect (see
//! `wants_pointer_capture`). All visuals pull colours from theme tokens —
//! no literal palette values live in this file.

use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{TextInput, TextInputKind};
use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// Geometry constants for the inner layout. Pulled out so the test fixture
/// and the live layout agree on rect positions without poking taffy.
const PICKER_WIDTH: f64 = 260.0;
const PICKER_HEIGHT: f64 = 220.0;
const SV_SIZE: f64 = 120.0;
const HUE_STRIP_WIDTH: f64 = 16.0;
const PREVIEW_SIZE: f64 = 40.0;
const SLIDER_HEIGHT: f64 = 14.0;
const SLIDER_GAP: f64 = 6.0;
const COMPONENT_GAP: f64 = 8.0;
const TRACK_HEIGHT: f64 = 6.0;
const KNOB_RADIUS: f64 = 6.0;

/// Which sub-control owns the current pointer drag, set on press and
/// routed `CursorMoved` to the matching `set_*` helper until release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragTarget {
    Sv,
    Hue,
    R,
    G,
    B,
}

pub struct ColorPicker {
    color: Color,
    on_change: Option<Box<dyn FnMut(Color)>>,
    rect: Rect,
    sv_rect: Rect,
    hue_strip_rect: Rect,
    r_slider_rect: Rect,
    g_slider_rect: Rect,
    b_slider_rect: Rect,
    preview_rect: Rect,
    drag_target: Option<DragTarget>,
    last_cursor: Point,
    is_focused: bool,
    /// Local-mode `TextInput` sitting under the preview swatch, holding the
    /// canonical hex string. A parse failure leaves `self.color` untouched.
    hex_input: TextInput,
    /// Cached rect for the hex input. Computed in `apply_layout` so
    /// `handle_event` can hit-test mouse events against it.
    hex_input_rect: Rect,
    /// Set by `set_hex_text_for_test` so tests can drive a parse pass
    /// without faking IME keystrokes; drained inside `handle_event`.
    /// Gated to test/debug builds so the test surface doesn't leak into prod.
    #[cfg(any(test, debug_assertions))]
    hex_input_force_dirty_for_test: bool,
    /// Stashed mid-`handle_event` when the embedded hex input asked for
    /// focus; consumed in `set_focus(true)` to redirect the App's "focus
    /// this node" call onto the embedded input instead — routing through
    /// the App is what blurs whatever sibling held focus before.
    hex_input_focus_pending: bool,
    /// One-shot record of the last user-driven colour change, drained by
    /// [`take_changed`] — a *programmatic* [`set_color`] does not set it.
    changed: Option<Color>,
}

impl ColorPicker {
    pub fn new(initial: Color) -> Self {
        let mut s = Self {
            color: initial,
            on_change: None,
            rect: Rect::ZERO,
            sv_rect: Rect::ZERO,
            hue_strip_rect: Rect::ZERO,
            r_slider_rect: Rect::ZERO,
            g_slider_rect: Rect::ZERO,
            b_slider_rect: Rect::ZERO,
            preview_rect: Rect::ZERO,
            drag_target: None,
            last_cursor: Point::ZERO,
            is_focused: false,
            hex_input: TextInput::local().with_kind(TextInputKind::Code),
            hex_input_rect: Rect::ZERO,
            #[cfg(any(test, debug_assertions))]
            hex_input_force_dirty_for_test: false,
            hex_input_focus_pending: false,
            changed: None,
        };
        let hex = s.hex_string();
        s.hex_input.set_text(&hex);
        s
    }

    /// Poll for a user-driven colour change since the last call, draining
    /// the record — lets a retained-mode app read it each frame instead of
    /// wiring an `FnMut` that would have to borrow its own state. A
    /// *programmatic* [`set_color`] is not reported here.
    pub fn take_changed(&mut self) -> Option<Color> {
        self.changed.take()
    }

    /// Push a new "current colour" into the picker from the outside.
    /// Updates the swatch/SV/hue/slider positions and the embedded hex
    /// field, but does NOT fire `on_change` or the `take_changed` record
    /// — see [`take_changed`]. No-op when the colour is unchanged.
    pub fn set_color(&mut self, color: Color) {
        if color.r() == self.color.r()
            && color.g() == self.color.g()
            && color.b() == self.color.b()
            && color.a() == self.color.a()
        {
            return;
        }
        self.color = color;
        self.sync_input();
    }

    pub fn hex_input_rect(&self) -> Rect {
        self.hex_input_rect
    }

    /// Position the picker at an externally-computed `rect`, without a
    /// taffy node. Computes the same sub-rects `apply_layout` does.
    /// Mirrors [`TextInput::apply_layout_external`].
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);

        self.sv_rect = Rect::new(x0, y0, x0 + SV_SIZE, y0 + SV_SIZE);

        let hue_x0 = self.sv_rect.x1 + COMPONENT_GAP;
        self.hue_strip_rect = Rect::new(hue_x0, y0, hue_x0 + HUE_STRIP_WIDTH, y0 + SV_SIZE);

        self.preview_rect = Rect::new(x1 - PREVIEW_SIZE, y0, x1, y0 + PREVIEW_SIZE);

        let sliders_top = self.sv_rect.y1 + COMPONENT_GAP;
        self.r_slider_rect = Rect::new(x0, sliders_top, x1, sliders_top + SLIDER_HEIGHT);
        self.g_slider_rect = Rect::new(
            x0,
            self.r_slider_rect.y1 + SLIDER_GAP,
            x1,
            self.r_slider_rect.y1 + SLIDER_GAP + SLIDER_HEIGHT,
        );
        self.b_slider_rect = Rect::new(
            x0,
            self.g_slider_rect.y1 + SLIDER_GAP,
            x1,
            self.g_slider_rect.y1 + SLIDER_GAP + SLIDER_HEIGHT,
        );
        if self.b_slider_rect.y1 > y1 {
            self.b_slider_rect = Rect::new(
                self.b_slider_rect.x0,
                self.b_slider_rect.y0,
                self.b_slider_rect.x1,
                y1,
            );
        }

        let text_h = 22.0;
        self.hex_input_rect = Rect::new(
            self.preview_rect.x0,
            self.preview_rect.y1 + COMPONENT_GAP,
            self.preview_rect.x1.max(self.preview_rect.x0 + 96.0),
            self.preview_rect.y1 + COMPONENT_GAP + text_h,
        );
        self.hex_input.apply_layout_external(self.hex_input_rect);
    }

    /// Sets the embedded hex input's text directly, bypassing
    /// character-by-character IME driving. The next `handle_event` parses
    /// it and applies it to `self.color`. Gated to test/debug builds —
    /// release binaries don't carry the matching dirty-sentinel branch.
    #[cfg(any(test, debug_assertions))]
    pub fn set_hex_text_for_test(&mut self, s: &str) {
        self.hex_input.set_text(s);
        self.hex_input_force_dirty_for_test = true;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_hex_input_focused_for_test(&self) -> bool {
        self.hex_input.is_focused()
    }

    /// Re-emits the canonical hex string into the embedded input after
    /// the SV square/hue strip/sliders mutate `self.color`.
    fn sync_input(&mut self) {
        let hex = self.hex_string();
        self.hex_input.set_text(&hex);
    }

    pub fn color(&self) -> Color {
        self.color
    }

    /// Six-digit `#RRGGBB` hex string for the current colour. Alpha is
    /// intentionally not encoded — the picker doesn't expose that channel.
    pub fn hex_string(&self) -> String {
        format!(
            "#{:02X}{:02X}{:02X}",
            self.color.r(),
            self.color.g(),
            self.color.b()
        )
    }

    pub fn rgb_string(&self) -> String {
        format!(
            "rgb({}, {}, {})",
            self.color.r(),
            self.color.g(),
            self.color.b()
        )
    }

    /// Register a callback that fires whenever `color` mutates, after the
    /// new value is stored so observers can just call `picker.color()`.
    #[must_use]
    pub fn on_change<F: FnMut(Color) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Narrows a layout/measurement value to `f32` for vello/taffy. Every
    /// call site here is a screen-space coordinate or a fraction already
    /// clamped to `0.0..=1.0`, always far below `f32::MAX`, so the
    /// truncation the lint warns about can't actually occur.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "screen-space pixel values and 0.0..=1.0 fractions in this widget never approach f32::MAX"
    )]
    fn px(v: f64) -> f32 {
        v as f32
    }

    /// Round a float already known to sit in `0.0..=255.0` down to its
    /// `u8` channel value. Callers clamp immediately before calling this,
    /// so the cast can never truncate or lose a sign.
    fn channel_u8(x: f32) -> u8 {
        let x = x.clamp(0.0, 255.0).round();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "x is clamped to 0.0..=255.0 immediately above, so this can never truncate or wrap"
        )]
        let channel = x as u8;
        channel
    }

    /// Standard HSV → RGB (HSV all in 0..1; output 0..255 per channel).
    /// Branch table form to avoid floating-point modulus surprises around
    /// h ≈ 1.0.
    #[allow(
        clippy::many_single_char_names,
        reason = "h, s, v, i, f, p, q, t, r, g, b are the standard HSV/RGB channel and \
                  interpolation names; inventing longer ones would only hurt readability here"
    )]
    fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
        let h = h.rem_euclid(1.0);
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "h*6.0 is in [0.0, 6.0] after rem_euclid(1.0) above, so floor() never \
                      approaches i32::MAX"
        )]
        let i = (h * 6.0).floor() as i32;
        #[allow(
            clippy::cast_precision_loss,
            reason = "i is bounded to roughly 0..=6 (see above), far below f32's exact-integer range"
        )]
        let f = h * 6.0 - i as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - f * s);
        let t = v * (1.0 - (1.0 - f) * s);
        let (r, g, b) = match i.rem_euclid(6) {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        (
            Self::channel_u8(r * 255.0),
            Self::channel_u8(g * 255.0),
            Self::channel_u8(b * 255.0),
        )
    }

    /// Inverse of `hsv_to_rgb`. Returns (h, s, v) all in 0..1. When the
    /// colour is on the achromatic axis (max == min) we keep `h = 0` —
    /// callers writing back into HSV should preserve whatever hue they
    /// last had rather than relying on this fallback.
    #[allow(
        clippy::many_single_char_names,
        reason = "r, g, b, h, s, v are the standard RGB/HSV channel names; inventing longer \
                  ones would only hurt readability here"
    )]
    fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
        let rf = f32::from(r) / 255.0;
        let gf = f32::from(g) / 255.0;
        let bf = f32::from(b) / 255.0;
        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        let delta = max - min;
        let v = max;
        let s = if max <= f32::EPSILON {
            0.0
        } else {
            delta / max
        };
        #[allow(
            clippy::float_cmp,
            reason = "max is exactly one of rf/gf/bf via f32::max (a selection, not an \
                      independent computation), so exact equality is the correct test"
        )]
        let h = if delta <= f32::EPSILON {
            0.0
        } else if max == rf {
            ((gf - bf) / delta).rem_euclid(6.0) / 6.0
        } else if max == gf {
            (((bf - rf) / delta) + 2.0) / 6.0
        } else {
            (((rf - gf) / delta) + 4.0) / 6.0
        };
        (h, s, v)
    }

    fn current_hsv(&self) -> (f32, f32, f32) {
        Self::rgb_to_hsv(self.color.r(), self.color.g(), self.color.b())
    }

    /// Rebuild `self.color` from a new hue, preserving the current
    /// saturation and value. Fires the `on_change` callback.
    #[allow(
        clippy::many_single_char_names,
        reason = "h, s, v, r, g, b are the standard HSV/RGB channel names; inventing longer \
                  ones would only hurt readability here"
    )]
    fn set_hue(&mut self, h: f32) {
        let (_, s, v) = self.current_hsv();
        // On the achromatic axis s=0; clamp to a minimum so picking a
        // hue still shows visible saturation.
        let s = if s < 0.05 { 1.0 } else { s };
        let v = if v < 0.05 { 1.0 } else { v };
        let (r, g, b) = Self::hsv_to_rgb(h, s, v);
        self.set_rgb(r, g, b);
    }

    /// Internal setter — central choke point so every mutation fires the
    /// `on_change` callback (when one is registered) and refreshes the
    /// colour atomically.
    fn set_rgb(&mut self, r: u8, g: u8, b: u8) {
        let new_color = Color::from_rgba8(r, g, b, self.color.a());
        if new_color.r() == self.color.r()
            && new_color.g() == self.color.g()
            && new_color.b() == self.color.b()
        {
            return;
        }
        self.color = new_color;
        // Record for the poll-based `take_changed()` seam (retained-mode
        // apps) and fire the optional callback (imperative callers).
        self.changed = Some(self.color);
        if let Some(cb) = self.on_change.as_mut() {
            cb(self.color);
        }
    }

    /// Map a press inside `sv_rect` to saturation (x) and value (y): top
    /// left is sat=0/value=1, matching the standard SV-square convention.
    fn sv_from_point(&self, p: Point) -> (f32, f32) {
        let sx = Self::px(((p.x - self.sv_rect.x0) / self.sv_rect.width()).clamp(0.0, 1.0));
        let sy = Self::px(((p.y - self.sv_rect.y0) / self.sv_rect.height()).clamp(0.0, 1.0));
        let s = sx;
        let v = 1.0 - sy;
        (s, v)
    }

    /// Hue from cursor y in the hue strip rect, 0..1 top to bottom.
    fn hue_from_point(&self, p: Point) -> f32 {
        Self::px(((p.y - self.hue_strip_rect.y0) / self.hue_strip_rect.height()).clamp(0.0, 1.0))
    }

    fn channel_from_point(slider_rect: Rect, p: Point) -> f32 {
        Self::px(((p.x - slider_rect.x0) / slider_rect.width()).clamp(0.0, 1.0))
    }

    fn target_at(&self, p: Point) -> Option<DragTarget> {
        if self.sv_rect.contains(p) {
            return Some(DragTarget::Sv);
        }
        if self.hue_strip_rect.contains(p) {
            return Some(DragTarget::Hue);
        }
        if self.r_slider_rect.contains(p) {
            return Some(DragTarget::R);
        }
        if self.g_slider_rect.contains(p) {
            return Some(DragTarget::G);
        }
        if self.b_slider_rect.contains(p) {
            return Some(DragTarget::B);
        }
        None
    }

    /// Drive whichever control is currently dragging from a cursor
    /// position. No-op when `drag_target` is `None`.
    #[allow(
        clippy::many_single_char_names,
        reason = "h, s, v, r, g, b, p, f are the standard HSV/RGB channel and cursor-point \
                  names; inventing longer ones would only hurt readability here"
    )]
    fn apply_drag(&mut self, p: Point) {
        match self.drag_target {
            Some(DragTarget::Sv) => {
                let (s, v) = self.sv_from_point(p);
                let (h, _, _) = self.current_hsv();
                let (r, g, b) = Self::hsv_to_rgb(h, s, v);
                self.set_rgb(r, g, b);
            }
            Some(DragTarget::Hue) => {
                let h = self.hue_from_point(p);
                self.set_hue(h);
            }
            Some(DragTarget::R) => {
                let f = Self::channel_from_point(self.r_slider_rect, p);
                let new_r = Self::channel_u8(f * 255.0);
                self.set_rgb(new_r, self.color.g(), self.color.b());
            }
            Some(DragTarget::G) => {
                let f = Self::channel_from_point(self.g_slider_rect, p);
                let new_g = Self::channel_u8(f * 255.0);
                self.set_rgb(self.color.r(), new_g, self.color.b());
            }
            Some(DragTarget::B) => {
                let f = Self::channel_from_point(self.b_slider_rect, p);
                let new_b = Self::channel_u8(f * 255.0);
                self.set_rgb(self.color.r(), self.color.g(), new_b);
            }
            None => {}
        }
    }

    /// Draws the SV square (with focus ring and cursor) and the hue strip
    /// (with tick). Split out of `draw` to keep that function readable.
    fn draw_sv_and_hue(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        colors: &crate::tokens::colors::Colors,
        radius_sm: f64,
    ) {
        // --- SV square ---
        let sv_shape = RoundedRect::from_rect(self.sv_rect, radius_sm);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &sv_shape,
        );
        // Faint overlay hinting the square responds to clicks.
        let sv_fill = Color::from_rgba8(self.color.r(), self.color.g(), self.color.b(), 96);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            sv_fill,
            None,
            &sv_shape,
        );
        // The SV square carries the keyboard-focus ring for the whole
        // picker (see the no-bounding-box note in `draw`).
        let (sv_border_w, sv_border_c) = if self.is_focused {
            (2.0, colors.border_focus)
        } else {
            (1.0, colors.border_standard)
        };
        scene.stroke(
            &Stroke::new(sv_border_w),
            transform,
            sv_border_c,
            None,
            &sv_shape,
        );

        // SV cursor — a small ring at the (saturation, 1 - value) coordinate.
        let (_, sat, val) = self.current_hsv();
        let sv_cursor = Point::new(
            self.sv_rect.x0 + f64::from(sat) * self.sv_rect.width(),
            self.sv_rect.y0 + (1.0 - f64::from(val)) * self.sv_rect.height(),
        );
        let sv_ring = vello::kurbo::Circle::new(sv_cursor, KNOB_RADIUS);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            colors.text_primary,
            None,
            &sv_ring,
        );

        // --- Hue strip (8 horizontal bands from `hue_stops`) ---
        let stops = Self::hue_stops(theme);
        #[allow(
            clippy::cast_precision_loss,
            reason = "stops has a fixed length of 8; i and stops.len() are always far below \
                      f64's exact-integer range"
        )]
        {
            let band_h = self.hue_strip_rect.height() / stops.len() as f64;
            for (i, stop) in stops.iter().enumerate() {
                let band = Rect::new(
                    self.hue_strip_rect.x0,
                    self.hue_strip_rect.y0 + i as f64 * band_h,
                    self.hue_strip_rect.x1,
                    self.hue_strip_rect.y0 + (i as f64 + 1.0) * band_h,
                );
                scene.fill(vello::peniko::Fill::NonZero, transform, *stop, None, &band);
            }
        }
        let hue_shape = RoundedRect::from_rect(self.hue_strip_rect, radius_sm);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &hue_shape,
        );

        let (h, _, _) = self.current_hsv();
        let tick_y = self.hue_strip_rect.y0 + f64::from(h) * self.hue_strip_rect.height();
        let tick = Rect::new(
            self.hue_strip_rect.x0 - 2.0,
            tick_y - 1.0,
            self.hue_strip_rect.x1 + 2.0,
            tick_y + 1.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &tick,
        );
    }

    /// Draws the preview swatch and the three RGB sliders (track, active
    /// fill and knob). Split out of `draw` to keep that function readable.
    fn draw_preview_and_sliders(
        &self,
        scene: &mut Scene,
        transform: Affine,
        colors: &crate::tokens::colors::Colors,
        radius_sm: f64,
    ) {
        // --- Preview swatch ---
        let preview_shape = RoundedRect::from_rect(self.preview_rect, radius_sm);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.color,
            None,
            &preview_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &preview_shape,
        );

        // --- RGB sliders --- computed from RGB directly rather than
        // cached, since the slider has no separate 0..1 state.
        let r_fill = Color::from_rgba8(self.color.r(), 0, 0, 255);
        let g_fill = Color::from_rgba8(0, self.color.g(), 0, 255);
        let b_fill = Color::from_rgba8(0, 0, self.color.b(), 255);
        for (slider, fill, frac) in [
            (
                self.r_slider_rect,
                r_fill,
                f64::from(self.color.r()) / 255.0,
            ),
            (
                self.g_slider_rect,
                g_fill,
                f64::from(self.color.g()) / 255.0,
            ),
            (
                self.b_slider_rect,
                b_fill,
                f64::from(self.color.b()) / 255.0,
            ),
        ] {
            let track_y0 = slider.y0 + (slider.height() - TRACK_HEIGHT) / 2.0;
            let track_y1 = track_y0 + TRACK_HEIGHT;
            let track = Rect::new(slider.x0, track_y0, slider.x1, track_y1);
            let track_shape = RoundedRect::from_rect(track, radius_sm);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &track_shape,
            );

            let active = Rect::new(
                track.x0,
                track.y0,
                track.x0 + frac * track.width(),
                track.y1,
            );
            let active_shape = RoundedRect::from_rect(active, radius_sm);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &active_shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &track_shape,
            );

            // Knob — filled at text_primary so it pops on any track fill.
            let knob_x = track.x0 + frac * track.width();
            let knob_y = slider.y0 + slider.height() / 2.0;
            let knob = vello::kurbo::Circle::new(Point::new(knob_x, knob_y), KNOB_RADIUS);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.text_primary,
                None,
                &knob,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &knob,
            );
        }
    }

    /// Hue-strip stop palette pulled from theme tokens. Falls back to
    /// `Color::WHITE`/`Color::BLACK` for slots not represented in the
    /// theme — tests don't pin the exact colours, only that hue clicks
    /// change the RGB output.
    fn hue_stops(theme: &Theme) -> [Color; 8] {
        let c = &theme.tokens.colors;
        [
            c.semantic.danger,            // red-ish (top)
            c.semantic.warning,           // amber/orange
            c.semantic.success,           // green
            c.semantic.info,              // cyan/blue
            c.accent[ACCENT_PRIMARY_IDX], // indigo
            Color::WHITE,
            Color::BLACK,
            c.semantic.danger, // wrap back to red
        ]
    }
}

impl Widget for ColorPicker {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {
        // The picker's state lives on `self.color`; CRDT persistence is
        // an opt-in extension a caller can wire through `on_change`
        // (e.g. writing to an Automerge field on every change).
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(Self::px(PICKER_WIDTH)),
                height: Dimension::length(Self::px(PICKER_HEIGHT)),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        let x1 = x0 + f64::from(layout.size.width);
        let y1 = y0 + f64::from(layout.size.height);
        self.apply_layout_external(Rect::new(x0, y0, x1, y1));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius_sm = f64::from(theme.tokens.radii.sm);

        // No bounding-box focus border: the picker's rect spans empty
        // space between its controls, so a stroke around all of it would
        // frame dead space. Focus is already carried by the SV cursor
        // ring, hue tick, and slider knobs.
        self.draw_sv_and_hue(scene, transform, theme, colors, radius_sm);
        self.draw_preview_and_sliders(scene, transform, colors, radius_sm);

        // --- Hex input + RGB readout --- drawn here directly since
        // `hex_input` isn't in the taffy tree.
        self.hex_input
            .draw(scene, transform, text_renderer, theme, presence);

        let text_size = theme.tokens.typography.scale.sm;
        let rgb_label = self.rgb_string();
        let label_x = self.preview_rect.x0;
        let row_gap = f64::from(theme.tokens.spacing.s2);
        let rgb_y = self.hex_input_rect.y1 + row_gap;
        text_renderer.draw_text(
            scene,
            &rgb_label,
            text_size,
            colors.text_dim,
            transform.pre_translate(Vec2::new(label_x, rgb_y)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Embedded hex input gets the event first. It isn't registered in
        // the parent's taffy/node_map, though, so its own
        // `*focus_target = Some(id)` would set *our* node id — the App
        // would call `picker.set_focus(true)`, never the input's, and
        // keystrokes would silently no-op behind its focus guard. Snapshot
        // the proposal and redirect locally if it fired while the cursor
        // was inside the input's rect.
        let prev_focus = *focus_target;
        self.hex_input.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            root,
            handled,
            focus_target,
            node_id,
            taffy,
        );
        if *focus_target != prev_focus && self.hex_input_rect.contains(self.last_cursor) {
            // Keep focus_target = Some(node_id) so the App's blur-then-
            // focus path runs, then redirect in set_focus via this flag.
            self.hex_input_focus_pending = true;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                if self.drag_target.is_some() {
                    self.apply_drag(p);
                    self.sync_input();
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: el_state,
                button: MouseButton::Left,
                ..
            } => {
                if *el_state == ElementState::Pressed {
                    // If the press already landed inside the hex input
                    // the input has claimed focus + handling — don't
                    // start an SV/hue/slider drag on top of that.
                    let in_input = self.hex_input_rect.contains(self.last_cursor);
                    if !in_input {
                        // Click outside: release any focus the input
                        // held so keystrokes stop landing in it.
                        if self.hex_input.is_focused() {
                            self.hex_input.set_focus(false);
                        }
                        if let Some(target) = self.target_at(self.last_cursor) {
                            self.drag_target = Some(target);
                            *focus_target = Some(node_id);
                            *handled = true;
                            // Jump-on-press: apply the current cursor position
                            // immediately so a single click commits the value
                            // without requiring a follow-up `CursorMoved`.
                            self.apply_drag(self.last_cursor);
                            self.sync_input();
                        }
                    }
                } else if *el_state == ElementState::Released && self.drag_target.is_some() {
                    self.drag_target = None;
                    *handled = true;
                }
            }
            _ => {}
        }

        // Drain the embedded input's dirty flag (or test sentinel) and
        // parse it as a colour. Invalid text is silently ignored;
        // `sync_input` overwrites the bad string on the next mutation.
        let dirty = self.hex_input.take_dirty();
        #[cfg(any(test, debug_assertions))]
        let dirty = dirty.or_else(|| {
            if self.hex_input_force_dirty_for_test {
                self.hex_input_force_dirty_for_test = false;
                Some(self.hex_input.value().to_string())
            } else {
                None
            }
        });
        if let Some(new_text) = dirty
            && let Some(parsed) = parse_hex(&new_text)
        {
            let r = parsed.r();
            let g = parsed.g();
            let b = parsed.b();
            self.set_rgb(r, g, b);
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_target.is_some()
    }

    fn name(&self) -> &'static str {
        "ColorPicker"
    }

    fn test_id(&self) -> Option<&str> {
        Some("color_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        if focused && self.hex_input_focus_pending {
            // Redirect the App's delivered focus onto the input; keep
            // our own `is_focused` false so we don't double-claim.
            self.hex_input_focus_pending = false;
            self.is_focused = false;
            self.hex_input.set_focus(true);
        } else {
            self.is_focused = focused;
            if !focused {
                self.hex_input.set_focus(false);
            }
        }
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.hex_input)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.hex_input)
    }

    /// The colour picker reports [`Role::ComboBox`] — a selector whose
    /// value is the chosen colour as its canonical `#RRGGBB` hex string.
    /// The SV square, hue strip and channel sliders render inline (there is
    /// no separate open/closed state), so `expanded` is left unset.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::ComboBox)
                .with_value(self.hex_string())
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    ..Default::default()
                }),
        )
    }
}

/// Parse a `#rrggbb` (case-insensitive, leading `#` optional) hex string
/// into a fully-opaque `Color`. `None` for anything that isn't exactly
/// six hex digits after an optional `#`.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if !s.is_ascii() {
        return None;
    }
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(s.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(s.get(4..6)?, 16).ok()?;
    Some(Color::from_rgba8(r, g, b, 255))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    /// Force a deterministic layout without spinning up a real taffy
    /// pass. Mirrors the SV/hue/slider arithmetic in `apply_layout`.
    fn picker_at(initial: Color, origin: Point) -> ColorPicker {
        let mut p = ColorPicker::new(initial);
        let x0 = origin.x;
        let y0 = origin.y;
        let x1 = x0 + PICKER_WIDTH;
        let y1 = y0 + PICKER_HEIGHT;
        p.rect = Rect::new(x0, y0, x1, y1);
        p.sv_rect = Rect::new(x0, y0, x0 + SV_SIZE, y0 + SV_SIZE);
        let hue_x0 = p.sv_rect.x1 + COMPONENT_GAP;
        p.hue_strip_rect = Rect::new(hue_x0, y0, hue_x0 + HUE_STRIP_WIDTH, y0 + SV_SIZE);
        p.preview_rect = Rect::new(x1 - PREVIEW_SIZE, y0, x1, y0 + PREVIEW_SIZE);
        let sliders_top = p.sv_rect.y1 + COMPONENT_GAP;
        p.r_slider_rect = Rect::new(x0, sliders_top, x1, sliders_top + SLIDER_HEIGHT);
        p.g_slider_rect = Rect::new(
            x0,
            p.r_slider_rect.y1 + SLIDER_GAP,
            x1,
            p.r_slider_rect.y1 + SLIDER_GAP + SLIDER_HEIGHT,
        );
        p.b_slider_rect = Rect::new(
            x0,
            p.g_slider_rect.y1 + SLIDER_GAP,
            x1,
            p.g_slider_rect.y1 + SLIDER_GAP + SLIDER_HEIGHT,
        );
        p
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn dispatch(p: &mut ColorPicker, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        p.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    /// Drag the hue strip vertically. The starting colour is a red-ish
    /// fill — after clicking near the strip's vertical centre the colour
    /// must change *and* the value channel of HSV must stay roughly
    /// where it was (the hue strip changes hue, not brightness).
    #[test]
    fn color_picker_dragging_hue_strip_changes_hue_only() {
        let initial = Color::from_rgba8(180, 50, 50, 255);
        let _ = Theme::dark();
        let mut p = picker_at(initial, Point::ZERO);

        let (_, _, v_before) = ColorPicker::rgb_to_hsv(initial.r(), initial.g(), initial.b());

        let click_x = f64::midpoint(p.hue_strip_rect.x0, p.hue_strip_rect.x1);
        let click_y = p.hue_strip_rect.y0 + 0.66 * p.hue_strip_rect.height();
        dispatch(&mut p, &cursor_moved(click_x, click_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let after = p.color();
        assert!(
            after.r() != initial.r() || after.g() != initial.g() || after.b() != initial.b(),
            "hue click must change RGB — got same colour {:?}",
            (after.r(), after.g(), after.b())
        );

        let (_, _, v_after) = ColorPicker::rgb_to_hsv(after.r(), after.g(), after.b());
        assert!(
            (v_after - v_before).abs() < 0.1 + 0.001,
            "hue strip must preserve V within ±0.1 — before={v_before}, after={v_after}",
        );
    }

    /// Clicking the SV square's top-right should yield high saturation
    /// and high value; clicking the bottom-left should drop one of them.
    #[test]
    fn color_picker_clicking_sv_square_changes_saturation_and_value() {
        let mut p = picker_at(Color::from_rgba8(120, 60, 200, 255), Point::ZERO);

        let tr_x = p.sv_rect.x1 - 1.0;
        let tr_y = p.sv_rect.y0 + 1.0;
        dispatch(&mut p, &cursor_moved(tr_x, tr_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let after_tr = p.color();
        let (_, s_tr, v_tr) = ColorPicker::rgb_to_hsv(after_tr.r(), after_tr.g(), after_tr.b());
        assert!(
            s_tr > 0.8,
            "top-right SV click must yield high saturation — got s={s_tr}",
        );
        assert!(
            v_tr > 0.8,
            "top-right SV click must yield high value — got v={v_tr}",
        );

        // Either channel dropping below the post-TR reading proves the click landed.
        let bl_x = p.sv_rect.x0 + 1.0;
        let bl_y = p.sv_rect.y1 - 1.0;
        dispatch(&mut p, &cursor_moved(bl_x, bl_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let after_bl = p.color();
        let (_, s_bl, v_bl) = ColorPicker::rgb_to_hsv(after_bl.r(), after_bl.g(), after_bl.b());
        assert!(
            s_bl < s_tr || v_bl < v_tr,
            "bottom-left SV click must lower saturation or value — \
             s_tr={s_tr}, s_bl={s_bl}, v_tr={v_tr}, v_bl={v_bl}",
        );
    }

    /// Slamming the R slider to full-right and G/B to full-left should
    /// produce pure red within rounding tolerance.
    #[test]
    fn color_picker_rgb_slider_to_red_yields_pure_red() {
        let mut p = picker_at(Color::from_rgba8(80, 80, 80, 255), Point::ZERO);

        let r_x = p.r_slider_rect.x1 - 1.0;
        let r_y = f64::midpoint(p.r_slider_rect.y0, p.r_slider_rect.y1);
        dispatch(&mut p, &cursor_moved(r_x, r_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let g_x = p.g_slider_rect.x0 + 1.0;
        let g_y = f64::midpoint(p.g_slider_rect.y0, p.g_slider_rect.y1);
        dispatch(&mut p, &cursor_moved(g_x, g_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let b_x = p.b_slider_rect.x0 + 1.0;
        let b_y = f64::midpoint(p.b_slider_rect.y0, p.b_slider_rect.y1);
        dispatch(&mut p, &cursor_moved(b_x, b_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let c = p.color();
        assert!(
            (i32::from(c.r()) - 255).abs() <= 5,
            "expected r ≈ 255, got {}",
            c.r(),
        );
        assert!(c.g() <= 5, "expected g ≈ 0, got {}", c.g());
        assert!(c.b() <= 5, "expected b ≈ 0, got {}", c.b());
    }

    /// `hex_string` must always produce `#RRGGBB` in upper-case with
    /// zero-padded bytes. Tested across two colors that exercise
    /// both single-digit (< 16) and two-digit channels.
    #[test]
    fn hex_string_formats_six_uppercase_digits() {
        let p = ColorPicker::new(Color::from_rgba8(0x12, 0x34, 0x56, 0xff));
        assert_eq!(p.hex_string(), "#123456");
        let p = ColorPicker::new(Color::from_rgba8(0x0a, 0x00, 0xff, 0xff));
        assert_eq!(p.hex_string(), "#0A00FF");
    }

    #[test]
    fn rgb_string_formats_decimal_triple() {
        let p = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
        assert_eq!(p.rgb_string(), "rgb(99, 102, 241)");
    }

    #[test]
    fn accessibility_reports_combobox_role_and_hex_value() {
        use crate::a11y::Role;
        let p = ColorPicker::new(Color::from_rgba8(0x12, 0x34, 0x56, 0xff));
        let node = Widget::accessibility(&p).expect("color picker is a selector");
        assert_eq!(node.role, Role::ComboBox);
        assert_eq!(node.value.as_deref(), Some("#123456"));
        assert!(node.state.focusable);
        assert!(!node.state.focused);
        assert_eq!(node.state.expanded, None, "the picker renders inline");
    }

    #[test]
    fn accessibility_value_tracks_color_change() {
        let mut p = ColorPicker::new(Color::from_rgba8(0, 0, 0, 0xff));
        p.set_rgb(0x0a, 0x00, 0xff);
        let node = Widget::accessibility(&p).unwrap();
        assert_eq!(node.value.as_deref(), Some("#0A00FF"));
    }

    /// Retained-mode embed round-trip: an app drops the picker into its
    /// widget tree, the user drags a channel, and the app reads the new
    /// colour back by polling `take_changed()` (no callback wired). The
    /// record drains after one read, and a *programmatic* `set_color`
    /// updates the swatch/hex without reporting a spurious user change.
    #[test]
    fn embed_round_trip_take_changed_and_set_color() {
        let mut p = picker_at(Color::from_rgba8(80, 80, 80, 255), Point::ZERO);
        assert_eq!(p.take_changed(), None, "no edit → take_changed is None");

        // A real drag through the production handle_event path.
        let r_x = p.r_slider_rect.x1 - 1.0;
        let r_y = f64::midpoint(p.r_slider_rect.y0, p.r_slider_rect.y1);
        dispatch(&mut p, &cursor_moved(r_x, r_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let reported = p.take_changed().expect("a drag reports a colour change");
        assert_eq!(
            reported,
            p.color(),
            "reported colour matches the live colour"
        );
        assert!(
            (i32::from(reported.r()) - 255).abs() <= 5,
            "R went to ~255, got {}",
            reported.r()
        );
        assert_eq!(p.take_changed(), None, "take_changed drains after one read");

        p.set_color(Color::from_rgba8(0x0a, 0x00, 0xff, 0xff));
        assert_eq!(
            p.hex_string(),
            "#0A00FF",
            "set_color updates the hex readout"
        );
        assert_eq!(p.color().r(), 0x0a);
        assert_eq!(
            p.take_changed(),
            None,
            "programmatic set_color must not surface as a user change",
        );
    }

    /// The `on_change` callback and the `take_changed` poll seam observe
    /// the SAME choke point, so both fire for one user edit — an app can
    /// pick whichever integration style fits.
    #[test]
    fn on_change_callback_and_take_changed_agree() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Option<Color>>> = Rc::new(RefCell::new(None));
        let seen2 = seen.clone();
        let mut p = ColorPicker::new(Color::from_rgba8(0, 0, 0, 255))
            .on_change(move |c| *seen2.borrow_mut() = Some(c));
        // Re-run layout arithmetic (on_change consumed `self`, so build fresh
        // rects the same way picker_at does).
        let base = picker_at(Color::from_rgba8(0, 0, 0, 255), Point::ZERO);
        p.rect = base.rect;
        p.r_slider_rect = base.r_slider_rect;
        p.g_slider_rect = base.g_slider_rect;
        p.b_slider_rect = base.b_slider_rect;
        p.sv_rect = base.sv_rect;
        p.hue_strip_rect = base.hue_strip_rect;

        let r_x = p.r_slider_rect.x1 - 1.0;
        let r_y = f64::midpoint(p.r_slider_rect.y0, p.r_slider_rect.y1);
        dispatch(&mut p, &cursor_moved(r_x, r_y));
        dispatch(&mut p, &mouse_press());

        let poll = p.take_changed().expect("poll seam saw the change");
        let cb = seen.borrow().expect("callback saw the change");
        assert_eq!(poll, cb, "both seams observe the identical colour");
    }

    #[test]
    fn parse_hex_rejects_non_ascii_six_byte_input() {
        // 6 bytes via UTF-8 multi-byte chars, not 6 chars.
        assert!(parse_hex("ä♥X").is_none());
        assert!(parse_hex("##abc12").is_none()); // not 6 hex digits
        assert!(parse_hex("#ABCDEF").is_some()); // sanity
        assert!(parse_hex("#abcdef").is_some()); // lowercase
    }
}
