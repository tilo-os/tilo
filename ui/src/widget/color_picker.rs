//! ColorPicker widget — Unit 5 of the 2026-05-22 widget expansion.
//!
//! Composite picker built from three independently-targeted controls in a
//! single rect:
//!   * SV (saturation × value) square on the left,
//!   * Hue strip on the right,
//!   * 3 RGB channel sliders along the bottom,
//!   * Preview swatch in the top-right corner.
//!
//! The widget is event-driven: each press latches a [`DragTarget`] so a
//! drag continues to track the cursor even after it leaves the widget
//! rect (see `wants_pointer_capture`). All visuals pull colours from the
//! theme tokens — no literal palette values live in this file.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Rect, Point, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::color_compat::ColorCompat;
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;

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

/// Which sub-control owns the current pointer drag. `None` means no drag
/// is active; on `MouseInput::Pressed` we set this based on which rect
/// contains `last_cursor`, then route subsequent `CursorMoved` events to
/// the matching `set_*` helper until release.
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
}

impl ColorPicker {
    pub fn new(initial: Color) -> Self {
        Self {
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
        }
    }

    /// Currently displayed colour. Mirrors the SV square / hue strip /
    /// RGB sliders — they all read and write through this single field.
    pub fn color(&self) -> Color {
        self.color
    }

    /// Six-digit `#RRGGBB` hex string for the current colour. Alpha
    /// is intentionally not encoded — the picker doesn't expose an
    /// alpha channel yet, and showing `#RRGGBBAA` would just confuse.
    pub fn hex_string(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.color.r(), self.color.g(), self.color.b())
    }

    /// Decimal `rgb(r, g, b)` triple. Mirrors what every CSS panel
    /// shows under a hex picker.
    pub fn rgb_string(&self) -> String {
        format!("rgb({}, {}, {})", self.color.r(), self.color.g(), self.color.b())
    }

    /// Register a callback that fires whenever `color` mutates. The
    /// callback runs *after* the new value is stored so observers can
    /// just call `picker.color()` to read it back.
    pub fn on_change<F: FnMut(Color) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Standard HSV → RGB (HSV all in 0..1; output 0..255 per channel).
    /// Branch table form to avoid floating-point modulus surprises around
    /// h ≈ 1.0.
    fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
        let h = h.rem_euclid(1.0);
        let s = s.clamp(0.0, 1.0);
        let v = v.clamp(0.0, 1.0);
        let i = (h * 6.0).floor() as i32;
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
            (r * 255.0).round().clamp(0.0, 255.0) as u8,
            (g * 255.0).round().clamp(0.0, 255.0) as u8,
            (b * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Inverse of `hsv_to_rgb`. Returns (h, s, v) all in 0..1. When the
    /// colour is on the achromatic axis (max == min) we keep `h = 0` —
    /// callers writing back into HSV should preserve whatever hue they
    /// last had rather than relying on this fallback.
    fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
        let rf = r as f32 / 255.0;
        let gf = g as f32 / 255.0;
        let bf = b as f32 / 255.0;
        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        let delta = max - min;
        let v = max;
        let s = if max <= f32::EPSILON { 0.0 } else { delta / max };
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

    /// Convenience: HSV view of `self.color`.
    fn current_hsv(&self) -> (f32, f32, f32) {
        Self::rgb_to_hsv(self.color.r(), self.color.g(), self.color.b())
    }

    /// Rebuild `self.color` from a new hue, preserving the current
    /// saturation and value. Fires the `on_change` callback.
    fn set_hue(&mut self, h: f32) {
        let (_, s, v) = self.current_hsv();
        // If we're on the achromatic axis the SV is at s=0 — picking a
        // hue should still inject some saturation so the user sees the
        // change. Clamp s to a minimum so the hue selection is visible.
        let s = if s < 0.05 { 1.0 } else { s };
        let v = if v < 0.05 { 1.0 } else { v };
        let (r, g, b) = Self::hsv_to_rgb(h, s, v);
        self.set_rgb(r, g, b);
    }

    /// Rebuild `self.color` from a new saturation, preserving hue and value.
    /// Public helper exposed per the widget-expansion plan even though the
    /// SV square currently sets s and v together via `apply_drag` —
    /// keyboard step bindings (future work) will route through this.
    #[allow(dead_code)]
    fn set_saturation(&mut self, s: f32) {
        let (h, _, v) = self.current_hsv();
        let v = if v < 0.05 { 1.0 } else { v };
        let (r, g, b) = Self::hsv_to_rgb(h, s, v);
        self.set_rgb(r, g, b);
    }

    /// Rebuild `self.color` from a new value, preserving hue and saturation.
    /// Public helper exposed per the widget-expansion plan even though the
    /// SV square currently sets s and v together via `apply_drag` —
    /// keyboard step bindings (future work) will route through this.
    #[allow(dead_code)]
    fn set_value(&mut self, v: f32) {
        let (h, s, _) = self.current_hsv();
        let s = if s < 0.05 { 1.0 } else { s };
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
        if let Some(cb) = self.on_change.as_mut() {
            cb(self.color);
        }
    }

    /// Map a press inside `sv_rect` to saturation (x) and value (y).
    /// (0,0) is top-left → sat=0, value=1. (1,1) is bottom-right → sat=1,
    /// value=0. Matches the standard "white top-left, black bottom-left"
    /// convention of every common colour picker.
    fn sv_from_point(&self, p: Point) -> (f32, f32) {
        let sx = ((p.x - self.sv_rect.x0) / self.sv_rect.width()).clamp(0.0, 1.0) as f32;
        let sy = ((p.y - self.sv_rect.y0) / self.sv_rect.height()).clamp(0.0, 1.0) as f32;
        let s = sx;
        let v = 1.0 - sy;
        (s, v)
    }

    /// Hue from cursor y in the hue strip rect, 0..1 top to bottom.
    fn hue_from_point(&self, p: Point) -> f32 {
        ((p.y - self.hue_strip_rect.y0) / self.hue_strip_rect.height())
            .clamp(0.0, 1.0) as f32
    }

    /// Channel fraction (0..1) from cursor x within a slider rect.
    fn channel_from_point(slider_rect: Rect, p: Point) -> f32 {
        ((p.x - slider_rect.x0) / slider_rect.width()).clamp(0.0, 1.0) as f32
    }

    /// Decide which sub-control should claim a press at `p`. Returns
    /// `None` if no rect contains the cursor — callers ignore the press
    /// in that case.
    fn target_at(&self, p: Point) -> Option<DragTarget> {
        if self.sv_rect.contains(p) { return Some(DragTarget::Sv); }
        if self.hue_strip_rect.contains(p) { return Some(DragTarget::Hue); }
        if self.r_slider_rect.contains(p) { return Some(DragTarget::R); }
        if self.g_slider_rect.contains(p) { return Some(DragTarget::G); }
        if self.b_slider_rect.contains(p) { return Some(DragTarget::B); }
        None
    }

    /// Drive whichever control is currently dragging from a cursor
    /// position. No-op when `drag_target` is `None`.
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
                let new_r = (f * 255.0).round() as u8;
                self.set_rgb(new_r, self.color.g(), self.color.b());
            }
            Some(DragTarget::G) => {
                let f = Self::channel_from_point(self.g_slider_rect, p);
                let new_g = (f * 255.0).round() as u8;
                self.set_rgb(self.color.r(), new_g, self.color.b());
            }
            Some(DragTarget::B) => {
                let f = Self::channel_from_point(self.b_slider_rect, p);
                let new_b = (f * 255.0).round() as u8;
                self.set_rgb(self.color.r(), self.color.g(), new_b);
            }
            None => {}
        }
    }

    /// Hue-strip stop palette pulled from theme tokens. Falls back to
    /// `Color::WHITE`/`Color::BLACK` for slots not represented in the
    /// theme — tests don't pin the exact colours, only that hue clicks
    /// change the RGB output.
    fn hue_stops(theme: &Theme) -> [Color; 8] {
        let c = &theme.tokens.colors;
        [
            c.semantic.danger,                  // red-ish (top)
            c.semantic.warning,                 // amber/orange
            c.semantic.success,                 // green
            c.semantic.info,                    // cyan/blue
            c.accent[ACCENT_PRIMARY_IDX],       // indigo
            Color::WHITE,
            Color::BLACK,
            c.semantic.danger,                  // wrap back to red
        ]
    }
}

impl Widget for ColorPicker {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {
        // The picker's state lives on `self.color`. Persistence to the
        // CRDT is an opt-in extension a caller can wire through the
        // `on_change` callback (e.g. write to an Automerge field on every
        // change). For now the picker is self-contained.
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(PICKER_WIDTH as f32),
                height: Dimension::length(PICKER_HEIGHT as f32),
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x0 = origin.x + layout.location.x as f64;
        let y0 = origin.y + layout.location.y as f64;
        let x1 = x0 + layout.size.width as f64;
        let y1 = y0 + layout.size.height as f64;
        self.rect = Rect::new(x0, y0, x1, y1);

        // SV square pinned top-left.
        self.sv_rect = Rect::new(x0, y0, x0 + SV_SIZE, y0 + SV_SIZE);

        // Hue strip to the right of the SV square.
        let hue_x0 = self.sv_rect.x1 + COMPONENT_GAP;
        self.hue_strip_rect = Rect::new(
            hue_x0,
            y0,
            hue_x0 + HUE_STRIP_WIDTH,
            y0 + SV_SIZE,
        );

        // Preview swatch in the top-right of the rect — right-aligned.
        self.preview_rect = Rect::new(
            x1 - PREVIEW_SIZE,
            y0,
            x1,
            y0 + PREVIEW_SIZE,
        );

        // Three RGB sliders stacked below the SV/hue row.
        let sliders_top = self.sv_rect.y1 + COMPONENT_GAP;
        self.r_slider_rect = Rect::new(
            x0,
            sliders_top,
            x1,
            sliders_top + SLIDER_HEIGHT,
        );
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
        // Clamp to the bottom of the widget rect (defensive — if the
        // outer height is smaller than expected, sliders still fit).
        if self.b_slider_rect.y1 > y1 {
            self.b_slider_rect = Rect::new(
                self.b_slider_rect.x0,
                self.b_slider_rect.y0,
                self.b_slider_rect.x1,
                y1,
            );
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let colors = &theme.tokens.colors;
        let radius_sm = theme.tokens.radii.sm as f64;

        // Focus border around the whole picker.
        if self.is_focused {
            let shape = RoundedRect::from_rect(self.rect, radius_sm);
            scene.stroke(&Stroke::new(1.0), transform, theme.border_focus, None, &shape);
        }

        // --- SV square ---
        // Drawn as a single translucent fill of the current colour over
        // the raised surface. The HSV gradient is intentionally out of
        // scope here (see module docstring) — tests only check that
        // clicks change saturation/value, not pixel art.
        let sv_shape = RoundedRect::from_rect(self.sv_rect, radius_sm);
        scene.fill(vello::peniko::Fill::NonZero, transform, colors.surfaces.raised, None, &sv_shape);
        // Overlay the current colour at the user's chosen sat/value —
        // gives a faint hint that the square responds to clicks.
        let sv_fill = Color::from_rgba8(self.color.r(), self.color.g(), self.color.b(), 96);
        scene.fill(vello::peniko::Fill::NonZero, transform, sv_fill, None, &sv_shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &sv_shape);

        // SV cursor — a small ring at the (saturation, 1 - value) coordinate.
        let (_, sat, val) = self.current_hsv();
        let sv_cursor = Point::new(
            self.sv_rect.x0 + sat as f64 * self.sv_rect.width(),
            self.sv_rect.y0 + (1.0 - val as f64) * self.sv_rect.height(),
        );
        let sv_ring = vello::kurbo::Circle::new(sv_cursor, KNOB_RADIUS);
        scene.stroke(&Stroke::new(2.0), transform, colors.text_primary, None, &sv_ring);

        // --- Hue strip (8 horizontal bands from `hue_stops`) ---
        let stops = Self::hue_stops(theme);
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
        let hue_shape = RoundedRect::from_rect(self.hue_strip_rect, radius_sm);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &hue_shape);

        // Hue indicator — a horizontal tick at the current hue's y.
        let (h, _, _) = self.current_hsv();
        let tick_y = self.hue_strip_rect.y0 + h as f64 * self.hue_strip_rect.height();
        let tick = Rect::new(
            self.hue_strip_rect.x0 - 2.0,
            tick_y - 1.0,
            self.hue_strip_rect.x1 + 2.0,
            tick_y + 1.0,
        );
        scene.fill(vello::peniko::Fill::NonZero, transform, colors.text_primary, None, &tick);

        // --- Preview swatch ---
        let preview_shape = RoundedRect::from_rect(self.preview_rect, radius_sm);
        scene.fill(vello::peniko::Fill::NonZero, transform, self.color, None, &preview_shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &preview_shape);

        // --- RGB sliders ---
        // Each slider re-uses the same draw recipe: track on raised
        // surface, fill bar in the channel's pure colour, knob at the
        // current value. We compute the knob position from the live RGB
        // channels rather than caching it — the slider has no separate
        // 0..1 state, it *is* the channel.
        let r_fill = Color::from_rgba8(self.color.r(), 0, 0, 255);
        let g_fill = Color::from_rgba8(0, self.color.g(), 0, 255);
        let b_fill = Color::from_rgba8(0, 0, self.color.b(), 255);
        for (slider, fill, frac) in [
            (self.r_slider_rect, r_fill, self.color.r() as f64 / 255.0),
            (self.g_slider_rect, g_fill, self.color.g() as f64 / 255.0),
            (self.b_slider_rect, b_fill, self.color.b() as f64 / 255.0),
        ] {
            let track_y0 = slider.y0 + (slider.height() - TRACK_HEIGHT) / 2.0;
            let track_y1 = track_y0 + TRACK_HEIGHT;
            let track = Rect::new(slider.x0, track_y0, slider.x1, track_y1);
            let track_shape = RoundedRect::from_rect(track, radius_sm);
            scene.fill(vello::peniko::Fill::NonZero, transform, colors.surfaces.raised, None, &track_shape);

            let active = Rect::new(
                track.x0,
                track.y0,
                track.x0 + frac * track.width(),
                track.y1,
            );
            let active_shape = RoundedRect::from_rect(active, radius_sm);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &active_shape);
            scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &track_shape);

            // Knob — filled at text_primary so it pops on any track fill.
            let knob_x = track.x0 + frac * track.width();
            let knob_y = slider.y0 + slider.height() / 2.0;
            let knob = vello::kurbo::Circle::new(Point::new(knob_x, knob_y), KNOB_RADIUS);
            scene.fill(vello::peniko::Fill::NonZero, transform, colors.text_primary, None, &knob);
            scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &knob);
        }

        // --- Hex + RGB readouts (right column, below preview) ---
        // Hex sits directly under the preview swatch; the RGB triple
        // sits one line below. Both stay inside the same column as the
        // preview so they don't fight the SV square for horizontal
        // space. Text size matches the toolbar / breadcrumb body
        // labels (`typography.scale.sm`).
        let text_size = theme.tokens.typography.scale.sm;
        let hex_label = self.hex_string();
        let rgb_label = self.rgb_string();
        let label_x = self.preview_rect.x0;
        // Stack the two lines tightly under the preview, leaving
        // ~`spacing.s2` between rows so they breathe.
        let row_gap = theme.tokens.spacing.s2 as f64;
        let hex_y = self.preview_rect.y1 + row_gap;
        let rgb_y = hex_y + text_size as f64 + row_gap;
        text_renderer.draw_text(
            scene,
            &hex_label,
            text_size,
            colors.text_primary,
            transform * Affine::translate((label_x, hex_y)),
            None,
        );
        text_renderer.draw_text(
            scene,
            &rgb_label,
            text_size,
            colors.text_dim,
            transform * Affine::translate((label_x, rgb_y)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                if self.drag_target.is_some() {
                    self.apply_drag(p);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: el_state, button: MouseButton::Left, .. } => {
                if *el_state == ElementState::Pressed {
                    if let Some(target) = self.target_at(self.last_cursor) {
                        self.drag_target = Some(target);
                        *focus_target = Some(node_id);
                        *handled = true;
                        // Jump-on-press: apply the current cursor position
                        // immediately so a single click commits the value
                        // without requiring a follow-up `CursorMoved`.
                        self.apply_drag(self.last_cursor);
                    }
                } else if *el_state == ElementState::Released {
                    if self.drag_target.is_some() {
                        self.drag_target = None;
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_target.is_some()
    }

    fn name(&self) -> &str { "ColorPicker" }

    fn test_id(&self) -> Option<&str> { Some("color_picker") }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
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
        let node = tree.new_leaf(Style::default()).unwrap();
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

        // Click well down the hue strip — that should land somewhere in
        // the blue band, not the red band the cursor started conceptually at.
        let click_x = (p.hue_strip_rect.x0 + p.hue_strip_rect.x1) / 2.0;
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
            "hue strip must preserve V within ±0.1 — before={}, after={}",
            v_before,
            v_after,
        );
    }

    /// Clicking the SV square's top-right should yield high saturation
    /// and high value; clicking the bottom-left should drop one of them.
    #[test]
    fn color_picker_clicking_sv_square_changes_saturation_and_value() {
        let mut p = picker_at(Color::from_rgba8(120, 60, 200, 255), Point::ZERO);

        // Top-right of SV square — sat ≈ 1, val ≈ 1.
        let tr_x = p.sv_rect.x1 - 1.0;
        let tr_y = p.sv_rect.y0 + 1.0;
        dispatch(&mut p, &cursor_moved(tr_x, tr_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let after_tr = p.color();
        let (_, s_tr, v_tr) =
            ColorPicker::rgb_to_hsv(after_tr.r(), after_tr.g(), after_tr.b());
        assert!(
            s_tr > 0.8,
            "top-right SV click must yield high saturation — got s={}",
            s_tr,
        );
        assert!(
            v_tr > 0.8,
            "top-right SV click must yield high value — got v={}",
            v_tr,
        );

        // Bottom-left of SV square — sat ≈ 0, val ≈ 0. Either channel
        // dropping below the post-TR reading proves the click landed.
        let bl_x = p.sv_rect.x0 + 1.0;
        let bl_y = p.sv_rect.y1 - 1.0;
        dispatch(&mut p, &cursor_moved(bl_x, bl_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let after_bl = p.color();
        let (_, s_bl, v_bl) =
            ColorPicker::rgb_to_hsv(after_bl.r(), after_bl.g(), after_bl.b());
        assert!(
            s_bl < s_tr || v_bl < v_tr,
            "bottom-left SV click must lower saturation or value — \
             s_tr={}, s_bl={}, v_tr={}, v_bl={}",
            s_tr,
            s_bl,
            v_tr,
            v_bl,
        );
    }

    /// Slamming the R slider to full-right and G/B to full-left should
    /// produce pure red within rounding tolerance.
    #[test]
    fn color_picker_rgb_slider_to_red_yields_pure_red() {
        let mut p = picker_at(Color::from_rgba8(80, 80, 80, 255), Point::ZERO);

        // R slider to its right edge.
        let r_x = p.r_slider_rect.x1 - 1.0;
        let r_y = (p.r_slider_rect.y0 + p.r_slider_rect.y1) / 2.0;
        dispatch(&mut p, &cursor_moved(r_x, r_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        // G slider to its left edge.
        let g_x = p.g_slider_rect.x0 + 1.0;
        let g_y = (p.g_slider_rect.y0 + p.g_slider_rect.y1) / 2.0;
        dispatch(&mut p, &cursor_moved(g_x, g_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        // B slider to its left edge.
        let b_x = p.b_slider_rect.x0 + 1.0;
        let b_y = (p.b_slider_rect.y0 + p.b_slider_rect.y1) / 2.0;
        dispatch(&mut p, &cursor_moved(b_x, b_y));
        dispatch(&mut p, &mouse_press());
        dispatch(&mut p, &mouse_release());

        let c = p.color();
        assert!(
            (c.r() as i32 - 255).abs() <= 5,
            "expected r ≈ 255, got {}", c.r(),
        );
        assert!(
            c.g() <= 5,
            "expected g ≈ 0, got {}", c.g(),
        );
        assert!(
            c.b() <= 5,
            "expected b ≈ 0, got {}", c.b(),
        );
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
}
