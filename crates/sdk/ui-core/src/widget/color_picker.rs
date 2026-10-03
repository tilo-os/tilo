//! Event-driven: each press latches a [`DragTarget`] so a drag continues
//! to track the cursor even after it leaves the widget rect (see
//! `wants_pointer_capture`). All visuals pull colours from theme tokens —
//! no literal palette values live in this file.
//!
//! The selected colour can stay widget-local, bind to typed process-local
//! state with [`ColorPicker::from_state`], or be observed with `on_change`
//! for an app-owned durable mutation.
//!
//! Two optional parts extend the inline picker:
//!
//! - **Swatches** ([`ColorPicker::with_swatches`]): a palette of quick picks
//!   below the sliders, each a radio button for assistive technology.
//! - **Confirm mode** ([`ColorPicker::confirming`]): a titled picker whose
//!   edits stay pending until *Done*; *Cancel* (or `Escape`) restores the
//!   colour it opened with. The host reads the result from
//!   [`ColorPicker::take_outcome`] or [`ColorPicker::on_outcome`].
//!
//! The saturation/value field and hue strip paint the real HSV gradients the
//! pointer maps into; every chrome colour (edges, rings, labels, buttons)
//! comes from theme tokens.

use crate::color_compat::ColorCompat;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, ButtonFace, ColorSwatchFace};
use crate::widget::{TextInput, TextInputKind};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

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
/// Height of the RGB sliders and the gaps around them, below the SV square.
const SLIDERS_HEIGHT: f64 = COMPONENT_GAP + 3.0 * SLIDER_HEIGHT + 2.0 * SLIDER_GAP;
/// A spacing or metric token as a layout length. Layout runs without a
/// theme (hosts may place the picker externally), so the optional rows use
/// the unscaled token values, as the base block's geometry does.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const fn token(value: f32) -> f64 {
    value as f64
}
const SPACE: crate::tokens::Spacing = crate::tokens::Spacing::BASE;
/// How much the SV square grows when the picker is laid out wider than
/// [`PICKER_WIDTH`] (a picker filling a sheet), one pixel per pixel.
const SV_GROWTH_MAX: f64 = token(SPACE.s16) + token(SPACE.s4);
/// Confirm-mode title band.
const TITLE_HEIGHT: f64 = token(SPACE.s6);
/// Gap between the base block and each optional row.
const SECTION_GAP: f64 = token(SPACE.s3);
const SWATCH_SIZE: f64 = token(SPACE.s6);
const SWATCH_GAP: f64 = token(SPACE.s2);
const BUTTON_HEIGHT: f64 = token(crate::tokens::Metrics::BASE.min_pointer_target);
const BUTTON_WIDTH: f64 = token(SPACE.s16) + token(SPACE.s6);
const BUTTON_GAP: f64 = token(SPACE.s2);
/// Hue stops the strip's gradient passes through (every 60°).
const HUE_STOPS: usize = 7;

/// One quick pick in a picker's swatch palette.
#[derive(Clone, Debug)]
pub struct ColorSwatch {
    pub color: Color,
    /// Spoken name; `None` speaks the `#RRGGBB` hex.
    pub name: Option<String>,
}

impl ColorSwatch {
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self { color, name: None }
    }

    #[must_use]
    pub fn named(color: Color, name: impl Into<String>) -> Self {
        Self {
            color,
            name: Some(name.into()),
        }
    }

    fn spoken_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| crate::color::to_hex(self.color))
    }
}

/// How a confirming picker closed (see [`ColorPicker::confirming`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorPickerOutcome {
    /// *Done*: the pending colour, now also the committed one.
    Confirmed(Color),
    /// *Cancel*: the picker restored the colour it opened with.
    Cancelled,
}

/// Semantic target of the picker's colour value.
const TARGET_VALUE: &str = "color_picker";
const TARGET_CANCEL: &str = "color_picker.cancel";
const TARGET_DONE: &str = "color_picker.done";
const TARGET_SWATCH: &str = "color_picker.swatch:";

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
    /// Fluent message id spoken for the complete colour control.
    accessible_name: Option<String>,
    color: Color,
    on_change: Option<Box<dyn FnMut(Color)>>,
    local_state: Option<crate::local_state::LocalBinding<Color>>,
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
    swatches: Vec<ColorSwatch>,
    swatch_rects: Vec<Rect>,
    /// `Some` in confirm mode: the title painted above the picker.
    confirm_title: Option<String>,
    /// The colour *Cancel* restores: the opening colour, or the last one
    /// confirmed.
    committed: Color,
    outcome: Option<ColorPickerOutcome>,
    on_outcome: Option<Box<dyn FnMut(ColorPickerOutcome)>>,
    title_rect: Rect,
    cancel_rect: Rect,
    done_rect: Rect,
}

impl ColorPicker {
    pub fn new(initial: Color) -> Self {
        let mut s = Self {
            accessible_name: Some("Colour".to_owned()),
            color: initial,
            on_change: None,
            local_state: None,
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
            hex_input: TextInput::local()
                .with_kind(TextInputKind::Code)
                .with_id("color_picker.hex")
                .with_accessible_name("Hex colour"),
            hex_input_rect: Rect::ZERO,
            #[cfg(any(test, debug_assertions))]
            hex_input_force_dirty_for_test: false,
            hex_input_focus_pending: false,
            changed: None,
            swatches: Vec::new(),
            swatch_rects: Vec::new(),
            confirm_title: None,
            committed: initial,
            outcome: None,
            on_outcome: None,
            title_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            done_rect: Rect::ZERO,
        };
        let hex = s.hex_string();
        s.hex_input.set_text(&hex);
        s
    }

    /// Create a colour picker bound to typed process-local state.
    ///
    /// Pointer, text, and semantic changes update `state`; mutations through
    /// any clone update this picker on the next synchronization pass.
    pub fn from_state(state: &crate::LocalState<Color>) -> Self {
        let (color, binding) = crate::local_state::LocalBinding::new(state);
        let mut picker = Self::new(color);
        picker.local_state = Some(binding);
        picker
    }

    /// Poll for a user-driven colour change since the last call, draining
    /// the record — lets a retained-mode app read it each frame instead of
    /// wiring an `FnMut` that would have to borrow its own state. A
    /// *programmatic* `set_color` is not reported here.
    pub fn take_changed(&mut self) -> Option<Color> {
        self.changed.take()
    }

    /// Offer a palette of quick picks below the sliders. Choosing one sets
    /// the colour (pending, in confirm mode) exactly as a drag would.
    #[must_use]
    pub fn with_swatches(mut self, swatches: impl IntoIterator<Item = ColorSwatch>) -> Self {
        self.swatches = swatches.into_iter().collect();
        self
    }

    /// Replace the swatch palette (see [`Self::with_swatches`]).
    pub fn set_swatches(&mut self, swatches: impl IntoIterator<Item = ColorSwatch>) {
        self.swatches = swatches.into_iter().collect();
        if self.rect != Rect::ZERO {
            self.apply_layout_external(self.rect);
        }
    }

    /// Confirm mode: paint `title` above the picker and *Cancel* / *Done*
    /// below it. Edits stay pending — [`Self::color`] follows them and
    /// [`Self::take_changed`] still reports them for a live preview — while
    /// [`Self::committed_color`] keeps the colour the picker opened with.
    /// *Done* (or `Enter` while the picker has focus) confirms; *Cancel* (or
    /// `Escape`) restores the committed colour.
    #[must_use]
    pub fn confirming(mut self, title: impl Into<String>) -> Self {
        self.confirm_title = Some(title.into());
        self
    }

    /// The colour *Cancel* restores: the opening colour, or the last one
    /// confirmed.
    #[must_use]
    pub fn committed_color(&self) -> Color {
        self.committed
    }

    /// Poll for how a confirming picker closed since the last call.
    pub fn take_outcome(&mut self) -> Option<ColorPickerOutcome> {
        self.outcome.take()
    }

    /// Observe confirm-mode outcomes as they happen (see
    /// [`Self::take_outcome`] for the polling form).
    #[must_use]
    pub fn on_outcome<F: FnMut(ColorPickerOutcome) + 'static>(mut self, f: F) -> Self {
        self.on_outcome = Some(Box::new(f));
        self
    }

    /// *Done*: commit the pending colour and report
    /// [`ColorPickerOutcome::Confirmed`].
    pub fn confirm(&mut self) {
        self.committed = self.color;
        self.finish(ColorPickerOutcome::Confirmed(self.color));
    }

    /// *Cancel*: restore the committed colour (reported through
    /// [`Self::take_changed`] when it differs, so a live preview reverts too)
    /// and report [`ColorPickerOutcome::Cancelled`].
    pub fn cancel(&mut self) {
        let committed = self.committed;
        self.set_rgb(committed.r(), committed.g(), committed.b());
        self.sync_input();
        self.finish(ColorPickerOutcome::Cancelled);
    }

    fn finish(&mut self, outcome: ColorPickerOutcome) {
        self.drag_target = None;
        self.outcome = Some(outcome);
        if let Some(callback) = self.on_outcome.as_mut() {
            callback(outcome);
        }
    }

    /// The size the picker lays out at, including any swatch and confirm
    /// rows — for hosts that place it with [`Self::apply_layout_external`].
    #[must_use]
    pub fn preferred_size(&self) -> (f64, f64) {
        (PICKER_WIDTH, self.content_height(PICKER_WIDTH))
    }

    /// The height the picker needs when laid out `width` wide. Wider than
    /// [`Self::preferred_size`] the SV square grows and swatch rows hold
    /// more swatches, so a sheet can give the picker its full width.
    #[must_use]
    pub fn preferred_height(&self, width: f64) -> f64 {
        self.content_height(width)
    }

    /// Side of the SV square (and height of the hue strip) at `width`.
    fn sv_size(width: f64) -> f64 {
        SV_SIZE + (width - PICKER_WIDTH).clamp(0.0, SV_GROWTH_MAX)
    }

    fn has_extras(&self) -> bool {
        self.confirm_title.is_some() || !self.swatches.is_empty()
    }

    /// Swatches per row at `width`.
    fn swatches_per_row(width: f64) -> usize {
        let per = ((width + SWATCH_GAP) / (SWATCH_SIZE + SWATCH_GAP)).floor();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "floor of a positive on-screen width over a fixed pitch; clamped to at least one"
        )]
        let per = per.max(1.0) as usize;
        per
    }

    fn swatch_rows(&self, width: f64) -> usize {
        self.swatches.len().div_ceil(Self::swatches_per_row(width))
    }

    fn content_height(&self, width: f64) -> f64 {
        let sv = Self::sv_size(width);
        if !self.has_extras() {
            return PICKER_HEIGHT + (sv - SV_SIZE);
        }
        let mut height = sv + SLIDERS_HEIGHT;
        if self.confirm_title.is_some() {
            height += TITLE_HEIGHT + COMPONENT_GAP + SECTION_GAP + BUTTON_HEIGHT;
        }
        let rows = self.swatch_rows(width);
        if rows > 0 {
            let rows = f64::from(u32::try_from(rows).unwrap_or(u32::MAX));
            height += SECTION_GAP + rows * SWATCH_SIZE + (rows - 1.0) * SWATCH_GAP;
        }
        height
    }

    /// Choose the swatch at `index`, as a click on it does.
    fn choose_swatch(&mut self, index: usize) -> bool {
        let Some(color) = self.swatches.get(index).map(|swatch| swatch.color) else {
            return false;
        };
        self.set_rgb(color.r(), color.g(), color.b());
        self.sync_input();
        true
    }

    /// Name the colour picker for assistive technology.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Push a new "current colour" into the picker from the outside.
    /// Updates the swatch/SV/hue/slider positions and the embedded hex
    /// field, but does NOT fire `on_change` or the `take_changed` record
    /// — see `take_changed`. No-op when the colour is unchanged.
    pub fn set_color(&mut self, color: Color) {
        if color.r() == self.color.r()
            && color.g() == self.color.g()
            && color.b() == self.color.b()
            && color.a() == self.color.a()
        {
            return;
        }
        self.color = color;
        self.committed = color;
        self.sync_input();
        self.publish_local_state();
    }

    pub fn hex_input_rect(&self) -> Rect {
        self.hex_input_rect
    }

    /// Position the picker at an externally-computed `rect`, without a
    /// taffy node. Computes the same sub-rects `apply_layout` does.
    /// Mirrors [`TextInput::apply_layout_external`].
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        let (x0, mut y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
        if self.confirm_title.is_some() {
            self.title_rect = Rect::new(x0, y0, x1, y0 + TITLE_HEIGHT);
            y0 = self.title_rect.y1 + COMPONENT_GAP;
        } else {
            self.title_rect = Rect::ZERO;
        }

        let sv = Self::sv_size(x1 - x0);
        self.sv_rect = Rect::new(x0, y0, x0 + sv, y0 + sv);

        let hue_x0 = self.sv_rect.x1 + COMPONENT_GAP;
        self.hue_strip_rect = Rect::new(hue_x0, y0, hue_x0 + HUE_STRIP_WIDTH, y0 + sv);

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

        // Optional rows below the base block.
        let mut bottom = self.b_slider_rect.y1;
        self.swatch_rects.clear();
        if !self.swatches.is_empty() {
            let per_row = Self::swatches_per_row(x1 - x0);
            let top = bottom + SECTION_GAP;
            for index in 0..self.swatches.len() {
                let row = f64::from(u32::try_from(index / per_row).unwrap_or(u32::MAX));
                let col = f64::from(u32::try_from(index % per_row).unwrap_or(u32::MAX));
                let sx = x0 + col * (SWATCH_SIZE + SWATCH_GAP);
                let sy = top + row * (SWATCH_SIZE + SWATCH_GAP);
                let swatch = Rect::new(sx, sy, sx + SWATCH_SIZE, sy + SWATCH_SIZE);
                bottom = bottom.max(swatch.y1);
                self.swatch_rects.push(swatch);
            }
        }
        if self.confirm_title.is_some() {
            let top = bottom + SECTION_GAP;
            self.done_rect = Rect::new(x1 - BUTTON_WIDTH, top, x1, top + BUTTON_HEIGHT);
            let cancel_x1 = self.done_rect.x0 - BUTTON_GAP;
            self.cancel_rect = Rect::new(
                cancel_x1 - BUTTON_WIDTH,
                top,
                cancel_x1,
                top + BUTTON_HEIGHT,
            );
        } else {
            self.cancel_rect = Rect::ZERO;
            self.done_rect = Rect::ZERO;
        }
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
        crate::color::to_hex(self.color)
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

    /// HSV → RGB with the hue as a fraction of a turn (`0..1`), the unit the
    /// hue strip works in; see [`crate::color::hsv_to_rgb`].
    fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
        let c = crate::color::hsv_to_rgb(h * 360.0, s, v);
        (c.r(), c.g(), c.b())
    }

    /// Inverse of `hsv_to_rgb`, hue as a fraction of a turn. On the
    /// achromatic axis `h = 0` — callers writing back into HSV should
    /// preserve whatever hue they last had rather than rely on it.
    fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
        let (h, s, v) = crate::color::rgb_to_hsv(Color::from_rgb8(r, g, b));
        (h / 360.0, s, v)
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
        self.publish_local_state();
        // Record for the poll-based `take_changed()` seam (retained-mode
        // apps) and fire the optional callback (imperative callers).
        self.changed = Some(self.color);
        if let Some(cb) = self.on_change.as_mut() {
            cb(self.color);
        }
    }

    fn colors_equal(left: &Color, right: &Color) -> bool {
        left.r() == right.r()
            && left.g() == right.g()
            && left.b() == right.b()
            && left.a() == right.a()
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish_by(&self.color, Self::colors_equal);
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
    ///
    /// Both paint the colours the pointer maps into: the square is the
    /// current hue at full saturation and value, washed toward white along x
    /// and toward black along y; the strip runs the hue circle top to bottom.
    /// They are the subject being picked, not chrome, so they are computed
    /// from HSV rather than read from the theme.
    fn draw_sv_and_hue(
        &self,
        scene: &mut Scene,
        transform: Affine,
        colors: &crate::tokens::colors::Colors,
        radius_sm: f64,
    ) {
        use vello::peniko::{ColorStop, Extend, Gradient};

        // --- SV square ---
        let sv_shape = RoundedRect::from_rect(self.sv_rect, radius_sm);
        let (hue, sat, val) = self.current_hsv();
        let (hr, hg, hb) = Self::hsv_to_rgb(hue, 1.0, 1.0);
        let pure = Color::from_rgb8(hr, hg, hb);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            pure,
            None,
            &sv_shape,
        );
        let ramp = |from: Point, to: Point, ink: Color| {
            Gradient::new_linear(from, to)
                .with_extend(Extend::Pad)
                .with_stops(
                    &[
                        ColorStop {
                            offset: 0.0,
                            color: ink.into(),
                        },
                        ColorStop {
                            offset: 1.0,
                            color: ink.with_alpha_factor(0.0).into(),
                        },
                    ][..],
                )
        };
        let r = self.sv_rect;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            &ramp(Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Color::WHITE),
            None,
            &sv_shape,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            &ramp(Point::new(r.x0, r.y1), Point::new(r.x0, r.y0), Color::BLACK),
            None,
            &sv_shape,
        );
        // The SV square carries the keyboard-focus ring for the whole
        // picker (see the no-bounding-box note in `draw`).
        let (sv_border_w, sv_border_c) = if self.is_focused {
            (2.0, colors.border_focus)
        } else {
            // The field holds every colour; its edge is measured against the
            // surface the picker sits on (HIG §9.5).
            (1.0, crate::surface::edge_on(colors.surfaces.raised))
        };
        scene.stroke(
            &Stroke::new(sv_border_w),
            transform,
            sv_border_c,
            None,
            &sv_shape,
        );

        // SV cursor — a ring at the (saturation, 1 - value) coordinate, its
        // ink contrasting with the colour under it.
        let sv_cursor = Point::new(
            self.sv_rect.x0 + f64::from(sat) * self.sv_rect.width(),
            self.sv_rect.y0 + (1.0 - f64::from(val)) * self.sv_rect.height(),
        );
        let sv_ring = vello::kurbo::Circle::new(sv_cursor, KNOB_RADIUS);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            colors.ink_on_any(&[self.color], colors.text_primary, 3.0),
            None,
            &sv_ring,
        );

        // --- Hue strip ---
        let strip = self.hue_strip_rect;
        let stops: Vec<ColorStop> = (0..HUE_STOPS)
            .map(|index| {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "HUE_STOPS is a small compile-time constant"
                )]
                let t = index as f32 / (HUE_STOPS - 1) as f32;
                let (sr, sg, sb) = Self::hsv_to_rgb(t.min(0.999_9), 1.0, 1.0);
                ColorStop {
                    offset: t,
                    color: Color::from_rgb8(sr, sg, sb).into(),
                }
            })
            .collect();
        let spectrum = Gradient::new_linear(
            Point::new(strip.x0, strip.y0),
            Point::new(strip.x0, strip.y1),
        )
        .with_extend(Extend::Pad)
        .with_stops(&stops[..]);
        let hue_shape = RoundedRect::from_rect(strip, radius_sm);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            &spectrum,
            None,
            &hue_shape,
        );
        // The strip is a rainbow with no one colour to derive from; its edge
        // is measured against the raised picker surface around it.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(colors.surfaces.raised),
            None,
            &hue_shape,
        );

        let tick_y = strip.y0 + f64::from(hue) * strip.height();
        let tick = Rect::new(strip.x0 - 2.0, tick_y - 1.0, strip.x1 + 2.0, tick_y + 1.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_primary,
            None,
            &tick,
        );
    }

    fn draw_title(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(title) = &self.confirm_title else {
            return;
        };
        let style = theme.text(TextRole::Body);
        let (_, line) = text_renderer.measure_style(title, None, &style);
        let y = self.title_rect.y0 + (self.title_rect.height() - f64::from(line)) / 2.0;
        text_renderer.draw_style(
            scene,
            title,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(self.title_rect.x0, y)),
            Some(Self::px(self.title_rect.width())),
            &style.clone().with_weight(TextWeight::Semibold),
        );
    }

    fn draw_swatches(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        for (swatch, rect) in self.swatches.iter().zip(&self.swatch_rects) {
            ColorSwatchFace::new(*rect, swatch.color, swatch.spoken_name())
                .selected(Self::same_rgb(swatch.color, self.color))
                .hovered(rect.contains(self.last_cursor))
                .paint(scene, transform, text_renderer, theme);
        }
    }

    fn draw_buttons(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.confirm_title.is_none() {
            return;
        }
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.cancel_rect, "Cancel")
                .hovered(self.cancel_rect.contains(self.last_cursor)),
        );
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.done_rect, "Done")
                .primary()
                .hovered(self.done_rect.contains(self.last_cursor)),
        );
    }

    fn same_rgb(a: Color, b: Color) -> bool {
        a.r() == b.r() && a.g() == b.g() && a.b() == b.b()
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
        // Edge derived from the swatch itself, so it registers on any colour.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(self.color),
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
            // Unfilled half is a lift off the ground (HIG §9.5 Slider row).
            let unfilled = colors.lift(1.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                unfilled,
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
                crate::surface::edge_on(unfilled),
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
                crate::surface::edge_on(colors.text_primary),
                None,
                &knob,
            );
        }
    }
}

impl Widget for ColorPicker {
    fn sync(&mut self, _state: &mut AppModel, _root: &ModelPath) {
        if let Some(color) = self
            .local_state
            .as_mut()
            .and_then(crate::local_state::LocalBinding::poll)
        {
            self.color = color;
            self.sync_input();
        }
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
                height: Dimension::length(Self::px(self.content_height(PICKER_WIDTH))),
            },
            // Fixed target: never the item that gives in an overflowing row.
            flex_shrink: 0.0,
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
        let radius_sm = f64::from(theme.tokens.radii.detail);

        // No bounding-box focus border: the picker's rect spans empty
        // space between its controls, so a stroke around all of it would
        // frame dead space. Focus is already carried by the SV cursor
        // ring, hue tick, and slider knobs.
        self.draw_title(scene, transform, text_renderer, theme);
        self.draw_sv_and_hue(scene, transform, colors, radius_sm);
        self.draw_preview_and_sliders(scene, transform, colors, radius_sm);
        self.draw_swatches(scene, transform, text_renderer, theme);
        self.draw_buttons(scene, transform, text_renderer, theme);

        // --- Hex input + RGB readout --- drawn here directly since
        // `hex_input` isn't in the taffy tree.
        self.hex_input
            .draw(scene, transform, text_renderer, theme, presence);

        let text_style = theme.text(TextRole::Caption);
        let rgb_label = self.rgb_string();
        let label_x = self.preview_rect.x0;
        let row_gap = f64::from(theme.tokens.spacing.s2);
        let rgb_y = self.hex_input_rect.y1 + row_gap;
        text_renderer.draw_style(
            scene,
            &rgb_label,
            colors.text_dim,
            transform.pre_translate(Vec2::new(label_x, rgb_y)),
            None,
            &text_style,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Embedded hex input gets the event first. It isn't registered in
        // the parent's taffy/node_map, though, so its own
        // `*focus_target = Some(id)` would set *our* node id — the App
        // would call `picker.set_focus(true)`, never the input's, and
        // keystrokes would silently no-op behind its focus guard. Snapshot
        // the proposal and redirect locally if it fired while the cursor
        // was inside the input's rect.
        let prev_focus = *ctx.focus_target;
        self.hex_input
            .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
        if *ctx.focus_target != prev_focus && self.hex_input_rect.contains(self.last_cursor) {
            // Keep focus_target = Some(node_id) so the App's blur-then-
            // focus path runs, then redirect in set_focus via this flag.
            self.hex_input_focus_pending = true;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                if self.drag_target.is_some() {
                    self.apply_drag(p);
                    self.sync_input();
                    *ctx.handled = true;
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
                    let swatch = self
                        .swatch_rects
                        .iter()
                        .position(|rect| rect.contains(self.last_cursor));
                    if let Some(index) = swatch {
                        if self.hex_input.is_focused() {
                            self.hex_input.set_focus(false);
                        }
                        self.choose_swatch(index);
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                    } else if self.confirm_title.is_some()
                        && self.done_rect.contains(self.last_cursor)
                    {
                        self.confirm();
                        *ctx.handled = true;
                    } else if self.confirm_title.is_some()
                        && self.cancel_rect.contains(self.last_cursor)
                    {
                        self.cancel();
                        *ctx.handled = true;
                    } else if !in_input {
                        // Click outside: release any focus the input
                        // held so keystrokes stop landing in it.
                        if self.hex_input.is_focused() {
                            self.hex_input.set_focus(false);
                        }
                        if let Some(target) = self.target_at(self.last_cursor) {
                            self.drag_target = Some(target);
                            *ctx.focus_target = Some(ctx.node_id);
                            *ctx.handled = true;
                            // Jump-on-press: apply the current cursor position
                            // immediately so a single click commits the value
                            // without requiring a follow-up `CursorMoved`.
                            self.apply_drag(self.last_cursor);
                            self.sync_input();
                        }
                    }
                } else if *el_state == ElementState::Released && self.drag_target.is_some() {
                    self.drag_target = None;
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key @ (NamedKey::Enter | NamedKey::Escape)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.confirm_title.is_some()
                && !*ctx.handled
                && (self.is_focused || self.hex_input.is_focused()) =>
            {
                if *key == NamedKey::Enter {
                    self.confirm();
                } else {
                    self.cancel();
                }
                *ctx.handled = true;
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

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = vec![
            ("color_picker.sv".to_owned(), self.sv_rect),
            ("color_picker.hue".to_owned(), self.hue_strip_rect),
            ("color_picker.hex".to_owned(), self.hex_input_rect),
            ("color_picker.cancel".to_owned(), self.cancel_rect),
            ("color_picker.done".to_owned(), self.done_rect),
        ];
        rects.extend(
            self.swatch_rects
                .iter()
                .enumerate()
                .map(|(index, rect)| (format!("color_picker.swatch[{index}]"), *rect)),
        );
        rects.retain(|(_, rect)| rect.area() > 0.0);
        rects
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

    /// The colour picker reports `Role::ComboBox` — a selector whose
    /// value is the chosen colour as its canonical `#RRGGBB` hex string.
    /// The SV square, hue strip and channel sliders render inline (there is
    /// no separate open/closed state), so `expanded` is left unset.
    ///
    /// With swatches or in confirm mode the picker is a group: the colour
    /// value, one radio button per swatch (checked while it is the current
    /// colour) and, in confirm mode, *Cancel* and *Done* buttons — each with
    /// its own semantic action.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let value = AccessNode::new(Role::ComboBox)
            .with_action_target(TARGET_VALUE)
            .with_optional_name(self.accessible_name.as_ref().map(|name| crate::i18n!(name)))
            .with_value(self.hex_string())
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                ..Default::default()
            });
        if !self.has_extras() {
            return Some(value);
        }
        let mut children = vec![value];
        children.extend(
            self.swatches
                .iter()
                .zip(&self.swatch_rects)
                .enumerate()
                .map(|(index, (swatch, rect))| {
                    ColorSwatchFace::new(*rect, swatch.color, swatch.spoken_name())
                        .selected(Self::same_rgb(swatch.color, self.color))
                        .accessibility(format!("{TARGET_SWATCH}{index}"))
                }),
        );
        if self.confirm_title.is_some() {
            for (label, target, rect) in [
                ("Cancel", TARGET_CANCEL, self.cancel_rect),
                ("Done", TARGET_DONE, self.done_rect),
            ] {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(label)
                        .with_action_target(target)
                        .with_bounds(rect)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
        }
        let name = self
            .confirm_title
            .clone()
            .or_else(|| self.accessible_name.as_ref().map(|name| crate::i18n!(name)));
        Some(
            AccessNode::new(Role::Group)
                .with_optional_name(name)
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if let Some(index) = target.strip_prefix(TARGET_SWATCH) {
            return matches!(action, Action::Activate)
                && index
                    .parse::<usize>()
                    .is_ok_and(|index| self.choose_swatch(index));
        }
        match (target, action) {
            (TARGET_CANCEL, Action::Activate) if self.confirm_title.is_some() => {
                self.cancel();
                true
            }
            (TARGET_DONE, Action::Activate) if self.confirm_title.is_some() => {
                self.confirm();
                true
            }
            (TARGET_VALUE, _) => self.perform_action(action, state, root),
            _ => false,
        }
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _root: &ModelPath,
    ) -> bool {
        let crate::a11y::Action::SetValue(value) = action else {
            return false;
        };
        let Some(color) = parse_hex(value) else {
            return false;
        };
        self.set_rgb(color.r(), color.g(), color.b());
        self.sync_input();
        true
    }
}

/// Parse a typed hex colour (see [`crate::color::parse_hex`]) into a
/// fully-opaque `Color`: the picker has no alpha channel, so an `#rrggbbaa`
/// entry keeps only its colour.
fn parse_hex(s: &str) -> Option<Color> {
    crate::color::parse_hex(s).map(|c| Color::from_rgb8(c.r(), c.g(), c.b()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use automerge::AutoCommit;

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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    #[test]
    fn picker_surfaces_edit_hue_saturation_value_and_rgb() {
        /// Drag the hue strip vertically. The starting colour is a red-ish
        /// fill — after clicking near the strip's vertical centre the colour
        /// must change *and* the value channel of HSV must stay roughly
        /// where it was (the hue strip changes hue, not brightness).
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

        fn hue_strip_paints_the_hue_it_picks() {
            // The strip's stops are the HSV hue circle, so the colour a press
            // yields at a height matches the colour painted there.
            let top = ColorPicker::hsv_to_rgb(0.0, 1.0, 1.0);
            let third = ColorPicker::hsv_to_rgb(1.0 / 3.0, 1.0, 1.0);
            assert_eq!(top, (255, 0, 0));
            assert_eq!(third, (0, 255, 0));
        }

        color_picker_dragging_hue_strip_changes_hue_only();
        color_picker_clicking_sv_square_changes_saturation_and_value();
        color_picker_rgb_slider_to_red_yields_pure_red();
        hue_strip_paints_the_hue_it_picks();
    }

    #[test]
    fn colour_strings_format_and_parse_strictly() {
        /// `hex_string` must always produce `#RRGGBB` in upper-case with
        /// zero-padded bytes. Tested across two colors that exercise
        /// both single-digit (< 16) and two-digit channels.
        fn hex_string_formats_six_uppercase_digits() {
            let p = ColorPicker::new(Color::from_rgba8(0x12, 0x34, 0x56, 0xff));
            assert_eq!(p.hex_string(), "#123456");
            let p = ColorPicker::new(Color::from_rgba8(0x0a, 0x00, 0xff, 0xff));
            assert_eq!(p.hex_string(), "#0A00FF");
        }

        fn rgb_string_formats_decimal_triple() {
            let p = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
            assert_eq!(p.rgb_string(), "rgb(99, 102, 241)");
        }

        fn parse_hex_rejects_non_ascii_six_byte_input() {
            // 6 bytes via UTF-8 multi-byte chars, not 6 chars.
            assert!(parse_hex("ä♥X").is_none());
            assert!(parse_hex("##abc12").is_none()); // not 6 hex digits
            assert!(parse_hex("#ABCDEF").is_some()); // sanity
            assert!(parse_hex("#abcdef").is_some()); // lowercase
        }

        hex_string_formats_six_uppercase_digits();
        rgb_string_formats_decimal_triple();
        parse_hex_rejects_non_ascii_six_byte_input();
    }

    #[test]
    fn change_reporting_embeds_callbacks_and_shared_state_agree() {
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

        fn local_state_keeps_color_pickers_in_sync() {
            let initial = Color::from_rgba8(0x12, 0x34, 0x56, 0xff);
            let expected = Color::from_rgba8(0xab, 0xcd, 0xef, 0xff);
            let state = crate::LocalState::new(initial);
            let mut left = ColorPicker::from_state(&state);
            let mut right = ColorPicker::from_state(&state);
            let mut doc = AutoCommit::new();

            left.set_color(expected);
            assert!(state.with(|color| ColorPicker::colors_equal(color, &expected)));
            assert!(left.take_changed().is_none());
            Widget::sync(&mut right, &mut doc, &automerge::ROOT);
            assert_eq!(right.hex_string(), "#ABCDEF");
            assert!(right.take_changed().is_none());
        }

        fn semantic_color_change_updates_state_callback_and_changed_record() {
            use std::cell::RefCell;
            use std::rc::Rc;

            let state = crate::LocalState::new(Color::BLACK);
            let captured = Rc::new(RefCell::new(None));
            let callback = Rc::clone(&captured);
            let mut picker = ColorPicker::from_state(&state)
                .on_change(move |color| *callback.borrow_mut() = Some(color));
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("#36A2EB".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(picker.hex_string(), "#36A2EB");
            assert!(
                state.with(|color| color.r() == 0x36 && color.g() == 0xa2 && color.b() == 0xeb)
            );
            assert_eq!(captured.borrow().as_ref().map(Color::r), Some(0x36));
            assert_eq!(picker.take_changed().map(|color| color.b()), Some(0xeb));

            assert!(!Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("blue".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(picker.hex_string(), "#36A2EB");
        }

        accessibility_reports_combobox_role_and_hex_value();
        accessibility_value_tracks_color_change();
        embed_round_trip_take_changed_and_set_color();
        on_change_callback_and_take_changed_agree();
        local_state_keeps_color_pickers_in_sync();
        semantic_color_change_updates_state_callback_and_changed_record();
    }

    fn swatches() -> Vec<ColorSwatch> {
        vec![
            ColorSwatch::named(Color::from_rgb8(0xEF, 0x44, 0x44), "Red"),
            ColorSwatch::new(Color::from_rgb8(0x3B, 0x82, 0xF6)),
            ColorSwatch::new(Color::from_rgb8(0x10, 0xB9, 0x81)),
        ]
    }

    fn laid_out(picker: ColorPicker) -> ColorPicker {
        let mut picker = picker;
        let (w, h) = picker.preferred_size();
        picker.apply_layout_external(Rect::new(0.0, 0.0, w, h));
        picker
    }

    fn click(p: &mut ColorPicker, at: Point) {
        dispatch(p, &cursor_moved(at.x, at.y));
        dispatch(p, &mouse_press());
        dispatch(p, &mouse_release());
    }

    fn key(p: &mut ColorPicker, named: NamedKey) {
        // SAFETY: KeyEvent has private fields in winit 0.30; the public ones
        // tolerate the zero bit pattern and `handle_event` only inspects
        // `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        dispatch(
            p,
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            },
        );
    }

    #[test]
    fn swatches_lay_out_select_and_expose_radio_actions() {
        fn plain_picker_keeps_its_size_and_rows() {
            let p = ColorPicker::new(Color::from_rgb8(1, 2, 3));
            assert_eq!(p.preferred_size(), (PICKER_WIDTH, PICKER_HEIGHT));
            let p = laid_out(p);
            assert!(p.swatch_rects.is_empty());
            assert_eq!(p.done_rect, Rect::ZERO);
        }

        fn swatches_lay_out_below_the_sliders_and_inside_the_picker() {
            let p = laid_out(ColorPicker::new(Color::from_rgb8(1, 2, 3)).with_swatches(swatches()));
            assert_eq!(p.swatch_rects.len(), 3);
            for rect in &p.swatch_rects {
                assert!(rect.y0 >= p.b_slider_rect.y1 + SECTION_GAP - 0.01);
                assert!(rect.y1 <= p.rect.y1 + 0.01);
                assert!(p.rect.contains(Point::new(rect.x1 - 0.5, rect.y1 - 0.5)));
            }
        }

        fn clicking_a_swatch_sets_the_colour_and_reports_the_change() {
            let mut p =
                laid_out(ColorPicker::new(Color::from_rgb8(1, 2, 3)).with_swatches(swatches()));
            let target = p.swatch_rects[1].center();
            click(&mut p, target);
            assert_eq!(p.hex_string(), "#3B82F6");
            assert_eq!(p.take_changed().map(|c| c.b()), Some(0xF6));
        }

        fn every_swatch_is_a_radio_with_its_own_action() {
            use crate::a11y::{Action, Role};
            let mut p = laid_out(
                ColorPicker::new(Color::from_rgb8(0xEF, 0x44, 0x44)).with_swatches(swatches()),
            );
            let node = Widget::accessibility(&p).expect("picker semantics");
            assert_eq!(node.role, Role::Group);
            let radios: Vec<_> = node
                .children
                .iter()
                .filter(|child| child.role == Role::RadioButton)
                .collect();
            assert_eq!(radios.len(), 3);
            assert_eq!(radios[0].name.as_deref(), Some("Red"));
            assert_eq!(
                radios[0].state.checked,
                Some(true),
                "the current colour is checked"
            );
            assert_eq!(radios[1].name.as_deref(), Some("#3B82F6"));
            assert_eq!(radios[1].state.checked, Some(false));

            let mut doc = AutoCommit::new();
            let target = format!("{TARGET_SWATCH}2");
            assert!(Widget::perform_semantic_action(
                &mut p,
                &target,
                &Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(p.hex_string(), "#10B981");
            assert!(!Widget::perform_semantic_action(
                &mut p,
                &format!("{TARGET_SWATCH}9"),
                &Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
        }

        plain_picker_keeps_its_size_and_rows();
        swatches_lay_out_below_the_sliders_and_inside_the_picker();
        clicking_a_swatch_sets_the_colour_and_reports_the_change();
        every_swatch_is_a_radio_with_its_own_action();
    }

    #[test]
    fn confirm_mode_holds_edits_until_done_or_cancel() {
        fn confirm_mode_keeps_edits_pending_until_done() {
            let opening = Color::from_rgb8(0x1A, 0x22, 0x33);
            let mut p = laid_out(
                ColorPicker::new(opening)
                    .with_swatches(swatches())
                    .confirming("Page colour"),
            );
            assert!(p.title_rect.height() > 0.0);
            assert!(
                p.done_rect.y0 > p.swatch_rects[0].y1,
                "buttons sit below the swatches"
            );
            assert!(p.cancel_rect.x1 < p.done_rect.x0, "Cancel precedes Done");

            let swatch = p.swatch_rects[0].center();
            click(&mut p, swatch);
            assert_eq!(
                p.hex_string(),
                "#EF4444",
                "the pending colour follows the edit"
            );
            assert_eq!(p.committed_color().r(), 0x1A, "nothing is committed yet");
            assert_eq!(p.take_outcome(), None);

            let done = p.done_rect.center();
            click(&mut p, done);
            assert_eq!(
                p.take_outcome(),
                Some(ColorPickerOutcome::Confirmed(Color::from_rgb8(
                    0xEF, 0x44, 0x44
                )))
            );
            assert_eq!(p.committed_color().r(), 0xEF);
        }

        fn cancel_restores_the_opening_colour_and_reports_the_revert() {
            let opening = Color::from_rgb8(0x1A, 0x22, 0x33);
            let mut p = laid_out(
                ColorPicker::new(opening)
                    .with_swatches(swatches())
                    .confirming("Text colour"),
            );
            let swatch = p.swatch_rects[1].center();
            click(&mut p, swatch);
            let _ = p.take_changed();
            let cancel = p.cancel_rect.center();
            click(&mut p, cancel);
            assert_eq!(p.take_outcome(), Some(ColorPickerOutcome::Cancelled));
            assert_eq!(p.hex_string(), "#1A2233");
            assert_eq!(
                p.take_changed().map(|c| c.r()),
                Some(0x1A),
                "a live preview is told to revert"
            );
        }

        fn enter_confirms_and_escape_cancels_while_focused() {
            let mut p = laid_out(ColorPicker::new(Color::from_rgb8(9, 9, 9)).confirming("Accent"));
            key(&mut p, NamedKey::Enter);
            assert_eq!(p.take_outcome(), None, "keys only act on a focused picker");
            p.set_focus(true);
            p.set_rgb(0x20, 0x40, 0x60);
            key(&mut p, NamedKey::Escape);
            assert_eq!(p.take_outcome(), Some(ColorPickerOutcome::Cancelled));
            assert_eq!(p.hex_string(), "#090909");
            p.set_rgb(0x20, 0x40, 0x60);
            key(&mut p, NamedKey::Enter);
            assert_eq!(
                p.take_outcome(),
                Some(ColorPickerOutcome::Confirmed(Color::from_rgb8(
                    0x20, 0x40, 0x60
                )))
            );
        }

        fn confirm_mode_exposes_titled_group_with_cancel_and_done_actions() {
            use crate::a11y::{Action, Role};
            let mut p = laid_out(ColorPicker::new(Color::from_rgb8(9, 9, 9)).confirming("Accent"));
            let node = Widget::accessibility(&p).expect("picker semantics");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Accent"));
            let buttons: Vec<_> = node
                .children
                .iter()
                .filter(|child| child.role == Role::Button)
                .filter_map(|child| child.name.clone())
                .collect();
            assert_eq!(buttons, ["Cancel", "Done"]);
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut p,
                TARGET_DONE,
                &Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(matches!(
                p.take_outcome(),
                Some(ColorPickerOutcome::Confirmed(_))
            ));
            assert!(Widget::perform_semantic_action(
                &mut p,
                TARGET_VALUE,
                &Action::SetValue("#ABCDEF".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(p.hex_string(), "#ABCDEF");
        }

        confirm_mode_keeps_edits_pending_until_done();
        cancel_restores_the_opening_colour_and_reports_the_revert();
        enter_confirms_and_escape_cancels_while_focused();
        confirm_mode_exposes_titled_group_with_cancel_and_done_actions();
    }
}
