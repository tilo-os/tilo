//! Event-driven: each press latches a [`DragTarget`] so a drag continues to track the cursor even after
//! it leaves the widget rect (see `wants_pointer_capture`).

use crate::color_compat::ColorCompat;
use tilo_ui_base::i18n::widget_text;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextWeight};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use crate::widget::ButtonRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::TaffyTreeExt;
#[path = "color_picker_component.rs"]
mod component;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use crate::widget::{Button, ButtonFace, ColorSwatch, ColorSwatchFace};
use crate::widget::{TextInput, TextInputKind};
pub use component::ColorPickerProps;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Geometry constants for the inner layout. Pulled out so the test fixture
/// and the live layout agree on rect positions without poking taffy.
const PICKER_WIDTH: f64 = 260.0;
const SV_SIZE: f64 = 120.0;
/// The smallest SV square that still keeps the preview column beside it;
/// narrower, the preview and the hex field move under it.
const SV_MIN: f64 = 80.0;
const HUE_STRIP_WIDTH: f64 = 16.0;
const SV_CURSOR_RADIUS: f64 = 6.0;
const PREVIEW_SIZE: f64 = 40.0;
const SLIDER_HEIGHT: f64 = 24.0;
/// A channel slider's name and value stand beside its track: the step between a column and
/// the track (the knob overhangs into it), and the narrowest either column gets before the
/// words are measured.
const SLIDER_SIDE_GAP: f64 = token(SPACE.s3);
const SLIDER_COLUMN_MIN: f64 = token(SPACE.s8);
const SLIDER_GAP: f64 = 4.0;
const COMPONENT_GAP: f64 = 8.0;
/// A spacing or metric token as a layout length. Layout runs without a
/// theme (hosts may place the picker externally), so the optional rows use
/// the unscaled token values, as the base block's geometry does.
const fn token(value: f32) -> f64 {
    value as f64
}
const SPACE: tilo_ui_base::tokens::Spacing = tilo_ui_base::tokens::Spacing::BASE;
const SV_GROWTH_MAX: f64 = token(SPACE.s16) + token(SPACE.s4);
const TITLE_HEIGHT: f64 = token(SPACE.s6);
/// Gap between the base block and each optional row.
const SECTION_GAP: f64 = token(SPACE.s3);
const SWATCH_SIZE: f64 = token(SPACE.s6);
const SWATCH_GAP: f64 = token(SPACE.s2);
const BUTTON_HEIGHT: f64 = token(tilo_ui_base::tokens::Metrics::BASE.min_pointer_target);
const BUTTON_WIDTH: f64 = token(SPACE.s16) + token(SPACE.s6);
const BUTTON_GAP: f64 = token(SPACE.s2);
const COLUMN_WIDTH: f64 = token(SPACE.s16) + token(SPACE.s8);
const FIELD_HEIGHT: f64 = token(tilo_ui_base::tokens::Metrics::BASE.bar_control_height);

/// Where every part of a picker sits in one rect: shared by layout and by
/// the size the picker reports, so the reported size is the drawn one.
#[derive(Clone, Debug, Default, PartialEq)]
struct PickerLayout {
    title: Rect,
    sv: Rect,
    hue: Rect,
    preview: Rect,
    hex: Rect,
    r: Rect,
    g: Rect,
    b: Rect,
    swatches: Vec<Rect>,
    cancel: Rect,
    done: Rect,
    bottom: f64,
}
/// Hue stops the strip's gradient passes through (every 60°).
const HUE_STOPS: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorPickerOutcome {
    Confirmed(Color),
    Cancelled,
}

const TARGET_VALUE: &str = "color_picker";
const TARGET_CANCEL: &str = "color_picker.cancel";
const TARGET_DONE: &str = "color_picker.done";
const TARGET_SWATCH: &str = "color_picker.swatch:";
const TARGET_HUE: &str = "color_picker.hue";
const TARGET_R: &str = "color_picker.r";
const TARGET_G: &str = "color_picker.g";
const TARGET_B: &str = "color_picker.b";

/// One of the picker's sliders a reader steps or sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    Hue,
    Red,
    Green,
    Blue,
}

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
    accessible_name: Option<String>,
    color: Color,
    on_change: Option<Box<dyn FnMut(Color)>>,
    local_state: Option<tilo_ui_base::widget::binding::BoxedBinding<Color>>,
    rect: Rect,
    sv_rect: Rect,
    hue_strip_rect: Rect,
    r_slider_rect: Rect,
    g_slider_rect: Rect,
    b_slider_rect: Rect,
    preview_rect: Rect,
    drag_target: Option<DragTarget>,
    touch: crate::widget::SliderTouch,
    last_cursor: Point,
    is_focused: bool,
    hex_input: TextInput,
    /// Cached rect for the hex input. Computed in `apply_layout` so
    /// `handle_event` can hit-test mouse events against it.
    hex_input_rect: Rect,
    /// Set by `set_hex_text_for_test` so tests can drive a parse pass
    /// without faking IME keystrokes; drained inside `handle_event`.
    /// Guarded to test/debug builds so the test surface doesn't leak into prod.
    #[cfg(any(test, debug_assertions))]
    hex_input_force_dirty_for_test: bool,
    /// Stashed mid-`handle_event` when the embedded hex input asked for
    /// focus; consumed in `set_focus(true)` to redirect the App's "focus
    /// this node" call onto the embedded input instead — routing through
    /// the App is what blurs whatever sibling held focus before.
    hex_input_focus_pending: bool,
    changed: Option<Color>,
    swatches: Vec<ColorSwatch>,
    swatch_rects: Vec<Rect>,
    confirm_title: Option<String>,
    committed: Color,
    outcome: Option<ColorPickerOutcome>,
    on_outcome: Option<Box<dyn FnMut(ColorPickerOutcome)>>,
    title_rect: Rect,
    cancel_rect: Rect,
    done_rect: Rect,
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    /// The columns before and after each channel's track: as wide as the widest channel
    /// name and value it shows, measured in the theme and language of its last layout so
    /// no word is shortened while the picker has room.
    label_column: f64,
    value_column: f64,
    /// The style the channel names and values are drawn and measured in.
    caption_style: Option<tilo_ui_base::text_render::TextStyle>,

    dirty: bool,
}

impl ColorPicker {
    pub fn new(initial: Color) -> Self {
        let mut s = Self {
            dirty: true,
            accessible_name: Some(tilo_ui_base::i18n::widget_format("widget-color-name", &[])),
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
            touch: crate::widget::SliderTouch::default(),
            last_cursor: Point::ZERO,
            is_focused: false,
            hex_input: TextInput::local()
                .control_size(crate::widget::ControlSize::Small)
                .with_kind(TextInputKind::Code)
                .with_id("color_picker.hex")
                .with_accessible_name(tilo_ui_base::i18n::widget_format("widget-color-hex", &[])),
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
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            label_column: SLIDER_COLUMN_MIN,
            value_column: SLIDER_COLUMN_MIN,
            caption_style: None,
        };
        let hex = s.hex_string();
        s.hex_input.set_text(&hex);
        s
    }

    pub fn from_state(state: &tilo_ui_base::LocalState<Color>) -> Self {
        let (color, binding) = tilo_ui_base::widget::binding::local(state);
        let mut picker = Self::new(color);
        picker.local_state = Some(binding);
        picker
    }

    /// Create a colour picker following a colour held as its hex text: `fallback` until the
    /// text names one.
    pub fn from_text_state(state: &tilo_ui_base::LocalState<String>, fallback: Color) -> Self {
        let (color, binding) =
            tilo_ui_base::widget::binding::derived(state, move |text| parse_hex(text).unwrap_or(fallback));
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

    #[must_use]
    pub fn confirming(mut self, title: impl Into<String>) -> Self {
        self.confirm_title = Some(title.into());
        self
    }

    #[must_use]
    pub fn committed_color(&self) -> Color {
        self.committed
    }

    pub fn take_outcome(&mut self) -> Option<ColorPickerOutcome> {
        self.outcome.take()
    }

    #[must_use]
    pub fn on_outcome<F: FnMut(ColorPickerOutcome) + 'static>(mut self, f: F) -> Self {
        self.on_outcome = Some(Box::new(f));
        self
    }

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

    fn side_column(width: f64) -> bool {
        width >= SV_MIN + 2.0 * COMPONENT_GAP + HUE_STRIP_WIDTH + COLUMN_WIDTH
    }

    fn sv_size(width: f64) -> f64 {
        if Self::side_column(width) {
            (width - 2.0 * COMPONENT_GAP - HUE_STRIP_WIDTH - COLUMN_WIDTH)
                .clamp(SV_MIN, SV_SIZE + SV_GROWTH_MAX)
        } else {
            (width - COMPONENT_GAP - HUE_STRIP_WIDTH).max(token(SPACE.s8))
        }
    }

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

    fn content_height(&self, width: f64) -> f64 {
        self.layout_in(Rect::new(0.0, 0.0, width.max(1.0), f64::MAX / 4.0))
            .bottom
            .ceil()
    }

    fn layout_in(&self, rect: Rect) -> PickerLayout {
        let side = Self::sv_size(rect.width());
        let natural = self.layout_with_sv(rect, side);
        if natural.bottom <= rect.height() {
            return natural;
        }
        let minimum = token(tilo_ui_base::tokens::Metrics::BASE.min_pointer_target);
        let side = (side - (natural.bottom - rect.height())).max(minimum.min(side));
        self.layout_with_sv(rect, side)
    }

    /// Lay every part out with the allocated saturation/value square.
    fn layout_with_sv(&self, rect: Rect, sv: f64) -> PickerLayout {
        let (x0, x1) = (rect.x0, rect.x1);
        let width = x1 - x0;
        let mut layout = PickerLayout::default();
        let mut y0 = rect.y0;
        if self.confirm_title.is_some() {
            layout.title = Rect::new(x0, y0, x1, y0 + TITLE_HEIGHT);
            y0 = layout.title.y1 + COMPONENT_GAP;
        }
        layout.sv = Rect::new(x0, y0, x0 + sv, y0 + sv);
        let hue_x0 = layout.sv.x1 + COMPONENT_GAP;
        layout.hue = Rect::new(hue_x0, y0, hue_x0 + HUE_STRIP_WIDTH, y0 + sv);
        let mut block_bottom = layout.sv.y1;
        if Self::side_column(width) {
            let col_x0 = x1 - COLUMN_WIDTH;
            layout.preview = Rect::new(col_x0, y0, x1, y0 + PREVIEW_SIZE);
            let hex_y0 = layout.preview.y1 + COMPONENT_GAP;
            layout.hex = Rect::new(col_x0, hex_y0, x1, hex_y0 + FIELD_HEIGHT);
            block_bottom = block_bottom.max(layout.hex.y1);
        } else {
            let top = block_bottom + COMPONENT_GAP;
            layout.preview = Rect::new(x0, top, x0 + FIELD_HEIGHT, top + FIELD_HEIGHT);
            let hex_x0 = layout.preview.x1 + COMPONENT_GAP;
            layout.hex = Rect::new(hex_x0, top, x1.max(hex_x0), top + FIELD_HEIGHT);
            block_bottom = layout.hex.y1;
        }
        // The channel sliders: the shared `Slider` face, between the column that names
        // the channel and the column that reads it, the knob overhanging into the steps
        // on either side and never past the picker's edge.
        let track_x0 = x0 + self.label_column + SLIDER_SIDE_GAP;
        let track_x1 = (x1 - self.value_column - SLIDER_SIDE_GAP).max(track_x0);
        let mut top = block_bottom + COMPONENT_GAP;
        for slot in [&mut layout.r, &mut layout.g, &mut layout.b] {
            *slot = Rect::new(track_x0, top, track_x1, top + SLIDER_HEIGHT);
            top = slot.y1 + SLIDER_GAP;
        }
        let mut bottom = layout.b.y1;
        if !self.swatches.is_empty() {
            let per_row = Self::swatches_per_row(width);
            let top = bottom + SECTION_GAP;
            for index in 0..self.swatches.len() {
                let row = f64::from(u32::try_from(index / per_row).unwrap_or(u32::MAX));
                let col = f64::from(u32::try_from(index % per_row).unwrap_or(u32::MAX));
                let sx = x0 + col * (SWATCH_SIZE + SWATCH_GAP);
                let sy = top + row * (SWATCH_SIZE + SWATCH_GAP);
                let swatch = Rect::new(sx, sy, sx + SWATCH_SIZE, sy + SWATCH_SIZE);
                bottom = bottom.max(swatch.y1);
                layout.swatches.push(swatch);
            }
        }
        if self.confirm_title.is_some() {
            let top = bottom + SECTION_GAP;
            layout.done = Rect::new(x1 - BUTTON_WIDTH, top, x1, top + BUTTON_HEIGHT);
            let cancel_x1 = layout.done.x0 - BUTTON_GAP;
            layout.cancel = Rect::new(
                cancel_x1 - BUTTON_WIDTH,
                top,
                cancel_x1,
                top + BUTTON_HEIGHT,
            );
            bottom = layout.done.y1;
        }
        layout.bottom = bottom - rect.y0;
        layout
    }

    fn choose_swatch(&mut self, index: usize) -> bool {
        let Some(color) = self.swatches.get(index).map(|swatch| swatch.color) else {
            return false;
        };
        self.set_rgb(color.r(), color.g(), color.b());
        self.sync_input();
        true
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    pub fn set_color(&mut self, color: Color) {
        self.dirty = true;
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

    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        let layout = self.layout_in(rect);
        self.title_rect = layout.title;
        self.sv_rect = layout.sv;
        self.hue_strip_rect = layout.hue;
        self.preview_rect = layout.preview;
        self.hex_input_rect = layout.hex;
        self.r_slider_rect = layout.r;
        self.g_slider_rect = layout.g;
        self.b_slider_rect = layout.b;
        self.swatch_rects = layout.swatches;
        self.cancel_rect = layout.cancel;
        self.done_rect = layout.done;
        self.hex_input.apply_layout_external(self.hex_input_rect);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_hex_text_for_test(&mut self, s: &str) {
        self.dirty = true;
        self.hex_input.set_text(s);
        self.hex_input_force_dirty_for_test = true;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_hex_input_focused_for_test(&self) -> bool {
        self.hex_input.is_focused()
    }

    fn sync_input(&mut self) {
        self.dirty = true;
        let hex = self.hex_string();
        self.hex_input.set_text(&hex);
    }

    pub fn color(&self) -> Color {
        self.color
    }

    pub fn hex_string(&self) -> String {
        crate::color::to_hex(self.color)
    }

    /// Register a callback that fires whenever `color` mutates, after the
    /// new value is stored so observers can just call `picker.color()`.
    #[must_use]
    pub fn on_change<F: FnMut(Color) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
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
        self.dirty = true;
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
        self.dirty = true;
        let new_color = Color::from_rgba8(r, g, b, self.color.a());
        if new_color.r() == self.color.r()
            && new_color.g() == self.color.g()
            && new_color.b() == self.color.b()
        {
            return;
        }
        self.color = new_color;
        self.publish_local_state();
        // Record for the poll-based `take_changed()` path (retained-mode
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
        let sx = tilo_ui_base::num::f32_of_f64(
            ((p.x - self.sv_rect.x0) / self.sv_rect.width()).clamp(0.0, 1.0),
        );
        let sy = tilo_ui_base::num::f32_of_f64(
            ((p.y - self.sv_rect.y0) / self.sv_rect.height()).clamp(0.0, 1.0),
        );
        let s = sx;
        let v = 1.0 - sy;
        (s, v)
    }

    /// Hue from cursor y in the hue strip rect, 0..1 top to bottom.
    fn hue_from_point(&self, p: Point) -> f32 {
        tilo_ui_base::num::f32_of_f64(
            ((p.y - self.hue_strip_rect.y0) / self.hue_strip_rect.height()).clamp(0.0, 1.0),
        )
    }

    fn channel_from_point(slider_rect: Rect, p: Point) -> f32 {
        tilo_ui_base::num::f32_of_f64(crate::widget::SliderFace::new(slider_rect, 0.0).value_at(p))
    }

    fn target_at(&self, p: Point) -> Option<DragTarget> {
        if self.sv_rect.contains(p) {
            return Some(DragTarget::Sv);
        }
        if self.hue_strip_rect.contains(p) {
            return Some(DragTarget::Hue);
        }
        let reach = crate::widget::SliderFace::overhang();
        if self.r_slider_rect.inflate(reach, 0.0).contains(p) {
            return Some(DragTarget::R);
        }
        if self.g_slider_rect.inflate(reach, 0.0).contains(p) {
            return Some(DragTarget::G);
        }
        if self.b_slider_rect.inflate(reach, 0.0).contains(p) {
            return Some(DragTarget::B);
        }
        None
    }

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
        colors: &tilo_ui_base::tokens::colors::Colors,
        radius_sm: f64,
    ) {
        use vello::peniko::{ColorStop, Extend, Gradient};

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
        let (sv_border_w, sv_border_c) = if self.is_focused {
            (2.0, colors.border_focus)
        } else {
            // The field holds every colour; its edge is measured against the
            // surface the picker sits on (HIG §9.5).
            (1.0, tilo_ui_base::surface::edge_on(colors.surfaces.raised))
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
        let sv_ring = vello::kurbo::Circle::new(sv_cursor, SV_CURSOR_RADIUS);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            colors.ink_on_any(&[self.color], colors.text_primary, 3.0),
            None,
            &sv_ring,
        );

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
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tilo_ui_base::surface::edge_on(colors.surfaces.raised),
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
            Some(tilo_ui_base::num::f32_of_f64(self.title_rect.width())),
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
                .role(ButtonRole::Primary)
                .hovered(self.done_rect.contains(self.last_cursor)),
        );
    }

    fn same_rgb(a: Color, b: Color) -> bool {
        a.r() == b.r() && a.g() == b.g() && a.b() == b.b()
    }

    /// The column before a channel's track, where the channel is named.
    fn slider_name_column(&self, slider: Rect) -> Rect {
        Rect::new(
            self.rect.x0,
            slider.y0,
            self.rect.x0 + self.label_column,
            slider.y1,
        )
    }

    /// The column after a channel's track, where the channel's value stands.
    fn slider_value_column(&self, slider: Rect) -> Rect {
        Rect::new(
            self.rect.x1 - self.value_column,
            slider.y0,
            self.rect.x1,
            slider.y1,
        )
    }

    /// Size the columns beside the channel tracks from the words they hold: the widest of
    /// the three channel names and the widest reading, as the theme and the window's
    /// language write them. A name is never shortened while the picker has room for it.
    fn measure_columns(&mut self, text: &mut TextRenderer) {
        let Some(style) = self.caption_style.clone() else {
            return;
        };
        let names = [
            "widget-colour-red",
            "widget-colour-green",
            "widget-colour-blue",
        ];
        let label = names
            .into_iter()
            .map(|name| text.measure_style(&widget_text(name), None, &style).0)
            .fold(0.0_f32, f32::max);
        let value = text.measure_style("255", None, &style.numeric()).0;
        self.label_column = f64::from(label).max(SLIDER_COLUMN_MIN).ceil();
        self.value_column = f64::from(value).max(SLIDER_COLUMN_MIN).ceil();
    }

    fn draw_preview_and_sliders(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        radius_sm: f64,
    ) {
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
            tilo_ui_base::surface::edge_on(self.color),
            None,
            &preview_shape,
        );

        let style = self
            .caption_style
            .clone()
            .unwrap_or_else(|| theme.text(TextRole::Caption));
        let (_, line) = text_renderer.measure_style("Ag", None, &style);
        for (slider, channel, target, name) in [
            (
                self.r_slider_rect,
                self.color.r(),
                DragTarget::R,
                "widget-colour-red",
            ),
            (
                self.g_slider_rect,
                self.color.g(),
                DragTarget::G,
                "widget-colour-green",
            ),
            (
                self.b_slider_rect,
                self.color.b(),
                DragTarget::B,
                "widget-colour-blue",
            ),
        ] {
            // What the slider controls before it, what it reads after it: a track with
            // neither is a bar a person cannot read.
            let y = slider.y0 + f64::from(slider.height() - f64::from(line)).max(0.0) / 2.0;
            let name_column = self.slider_name_column(slider);
            let words = text_renderer.elide_to_width_style(
                &widget_text(name),
                tilo_ui_base::num::f32_of_f64(name_column.width()),
                &style,
            );
            text_renderer.draw_style(
                scene,
                &words,
                theme.text_dim,
                transform.pre_translate(Vec2::new(name_column.x0, y)),
                None,
                &style,
            );
            let value = channel.to_string();
            let numeric = style.clone().numeric();
            let value_column = self.slider_value_column(slider);
            let (value_width, _) = text_renderer.measure_style(&value, None, &numeric);
            let value_x = (value_column.x1 - f64::from(value_width)).max(value_column.x0);
            text_renderer.draw_style(
                scene,
                &value,
                theme.text_primary,
                transform.pre_translate(Vec2::new(value_x, y)),
                None,
                &numeric,
            );
            crate::widget::Slider::paint(
                scene,
                transform,
                theme,
                &crate::widget::SliderFace::new(slider, f64::from(channel) / 255.0)
                    .dragging(self.drag_target == Some(target)),
            );
        }
    }
}

impl Widget for ColorPicker {
    /// Its size follows from what it was made with and the theme, which builds its node anew:
    /// a laid-out one asks for no solve of its own.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    tilo_ui_base::described!();

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // A sync is a read of what the document holds, not a paint: only the
        // pull landing a colour this picker was not already showing is one.
        if let Some(binding) = &mut self.local_state
            && let tilo_ui_base::widget::binding::Pulled::Value(color) = binding.pull(state, root)
            && self.color != color
        {
            self.color = color;
            self.sync_input();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.caption_style = Some(theme.text(TextRole::Caption));
        let style = Style {
            size: Size::from_lengths(
                tilo_ui_base::num::f32_of_f64(PICKER_WIDTH),
                tilo_ui_base::num::f32_of_f64(self.content_height(PICKER_WIDTH)),
            ),
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut tilo_ui_base::text_render::TextRenderer,
    ) -> bool {
        // The columns beside the channel tracks take the words they hold, as the theme
        // and the window's language set them and this pass can measure them.
        self.measure_columns(text);
        let Ok(layout) = taffy.layout(node) else {
            return false;
        };
        let width = f64::from(layout.size.width);
        if width <= 0.0 {
            return false;
        }
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.size.height = Dimension::length(tilo_ui_base::num::f32_of_f64(self.content_height(width)));
        if style == *current {
            return false;
        }
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.apply_layout_external(tilo_ui_base::widget::layout_rect(taffy, node, origin));
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        let colors = &theme.tokens.colors;
        let radius_sm = f64::from(theme.tokens.radii.detail);

        // No bounding-box focus border: the picker's rect spans empty
        // space between its controls, so a stroke around all of it would
        // frame dead space. Focus is already carried by the SV cursor
        // ring, hue tick, and slider knobs.
        self.draw_title(scene, transform, text_renderer, theme);
        self.draw_sv_and_hue(scene, transform, colors, radius_sm);
        self.draw_preview_and_sliders(scene, transform, text_renderer, theme, radius_sm);
        self.draw_swatches(scene, transform, text_renderer, theme);
        self.draw_buttons(scene, transform, text_renderer, theme);

        self.hex_input
            .draw_child(scene, transform, text_renderer, theme, &env);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
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
                let p = ctx.logical(position);
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
            WindowEvent::Touch(touch) => {
                use crate::widget::SliderTouchStep;
                let mut finger = self.touch;
                let step = finger.step(touch, ctx.scale_factor, |point| {
                    self.target_at(point).is_some()
                });
                self.touch = finger;
                match step {
                    SliderTouchStep::Press(point) => {
                        self.last_cursor = point;
                        if self.hex_input.is_focused() {
                            self.hex_input.set_focus(false);
                        }
                        self.drag_target = self.target_at(point);
                        *ctx.focus_target = Some(ctx.node_id);
                        *ctx.handled = true;
                        self.apply_drag(point);
                        self.sync_input();
                    }
                    SliderTouchStep::Move(point) => {
                        if self.drag_target.is_some() {
                            self.apply_drag(point);
                            self.sync_input();
                        }
                        *ctx.handled = true;
                    }
                    SliderTouchStep::Release => {
                        self.drag_target = None;
                        *ctx.handled = true;
                    }
                    SliderTouchStep::Ignored => {}
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
        self.drag_target.is_some() || self.touch.is_active()
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.target_at(point).is_some()
    }

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
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

    /// Each swatch names itself on hover, as it does for a reader.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.swatches
            .iter()
            .zip(&self.swatch_rects)
            .find_map(|(swatch, rect)| ColorSwatchFace::of(*rect, swatch).tooltip_target(point))
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
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

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.hex_input.ime_edit_fingerprint(key)
    }

    fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
        Widget::ime_content(&self.hex_input)
    }

    /// The colour picker reports `Role::ComboBox` — a selector whose
    /// value is the chosen colour as its canonical `#RRGGBB` hex string.
    /// The SV square, hue strip and channel sliders render inline (there is
    /// no separate open/closed state), so `expanded` is left unset.
    ///
    /// What inputs the host has, from the frame: a swatch is a finger's target on touch.
    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
    }

    /// The picker is a group of the controls a reader works it with: the colour's hex value,
    /// the hue strip and the red, green and blue sliders — each named and reading its own
    /// value — then one radio button per swatch (checked while it is the current colour) and,
    /// in confirm mode, *Cancel* and *Done*.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let value = AccessNode::new(Role::ComboBox)
            .with_action_target(TARGET_VALUE)
            .with_optional_name(self.accessible_name.as_ref().map(|name| widget_text(name)))
            .with_value(self.hex_string())
            .with_bounds(self.hex_input_rect)
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused()),
            );
        let mut children = vec![value];
        let (hue, _, _) = self.current_hsv();
        children.push(Self::slider_node(
            "widget-colour-hue",
            format!("{:.0}°", hue * 360.0),
            TARGET_HUE,
            self.hue_strip_rect,
        ));
        for (name, channel, target, rect) in [
            (
                "widget-colour-red",
                self.color.r(),
                TARGET_R,
                self.r_slider_rect,
            ),
            (
                "widget-colour-green",
                self.color.g(),
                TARGET_G,
                self.g_slider_rect,
            ),
            (
                "widget-colour-blue",
                self.color.b(),
                TARGET_B,
                self.b_slider_rect,
            ),
        ] {
            children.push(Self::slider_node(name, channel.to_string(), target, rect));
        }
        children.extend(
            self.swatches
                .iter()
                .zip(&self.swatch_rects)
                .enumerate()
                .map(|(index, (swatch, rect))| {
                    ColorSwatchFace::new(*rect, swatch.color, swatch.spoken_name())
                        .selected(Self::same_rgb(swatch.color, self.color))
                        .target_rect(self.interaction.minimum_target_rect(*rect))
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
            .or_else(|| self.accessible_name.as_ref().map(|name| widget_text(name)));
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
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        use tilo_ui_base::a11y::Action;
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
            (TARGET_VALUE, _) => self.perform_action(action, &[], state, root),
            (TARGET_HUE, action) => self.move_channel(action, Channel::Hue),
            (TARGET_R, action) => self.move_channel(action, Channel::Red),
            (TARGET_G, action) => self.move_channel(action, Channel::Green),
            (TARGET_B, action) => self.move_channel(action, Channel::Blue),
            _ => false,
        }
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        let tilo_ui_base::a11y::Action::SetValue(value) = action else {
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

impl ColorPicker {
    /// One of the picker's sliders as a reader meets it: its name, and what it reads now.
    fn slider_node(
        name: &str,
        value: String,
        target: &'static str,
        bounds: Rect,
    ) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        AccessNode::new(Role::Slider)
            .with_name(widget_text(name))
            .with_value(value)
            .with_action_target(target)
            .with_bounds(bounds)
            .with_state(AccessState::new().focusable(true))
    }

    /// A slider's step from a reader: five steps a press, or the value it names.
    fn move_channel(&mut self, action: &tilo_ui_base::a11y::Action, channel: Channel) -> bool {
        use tilo_ui_base::a11y::Action;
        let span = if channel == Channel::Hue {
            360.0
        } else {
            255.0
        };
        let current = match channel {
            Channel::Hue => f64::from(self.current_hsv().0) * 360.0,
            Channel::Red => f64::from(self.color.r()),
            Channel::Green => f64::from(self.color.g()),
            Channel::Blue => f64::from(self.color.b()),
        };
        let target = match action {
            Action::Increment => current + span / 20.0,
            Action::Decrement => current - span / 20.0,
            Action::SetValue(value) => {
                let text = value.trim().trim_end_matches(['%', '°']).trim();
                let Ok(parsed) = text.parse::<f64>() else {
                    return false;
                };
                if !parsed.is_finite() {
                    return false;
                }
                parsed
            }
            _ => return false,
        }
        .clamp(0.0, span);
        match channel {
            Channel::Hue => {
                let hue = tilo_ui_base::num::f32_of_f64(target / 360.0).min(0.999_9);
                self.set_hue(hue);
            }
            Channel::Red => {
                let red = Self::channel_u8(tilo_ui_base::num::f32_of_f64(target));
                self.set_rgb(red, self.color.g(), self.color.b());
            }
            Channel::Green => {
                let green = Self::channel_u8(tilo_ui_base::num::f32_of_f64(target));
                self.set_rgb(self.color.r(), green, self.color.b());
            }
            Channel::Blue => {
                let blue = Self::channel_u8(tilo_ui_base::num::f32_of_f64(target));
                self.set_rgb(self.color.r(), self.color.g(), blue);
            }
        }
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
    use crate::testing::dispatch;
    use tilo_ui_base::theme::Theme;
    use automerge::AutoCommit;

    fn picker_at(initial: Color, origin: Point) -> ColorPicker {
        let mut p = ColorPicker::new(initial);
        let (w, h) = p.preferred_size();
        p.apply_layout_external(Rect::new(origin.x, origin.y, origin.x + w, origin.y + h));
        p
    }

    use tilo_ui_base::synthetic_input::cursor_moved;

    use tilo_ui_base::synthetic_input::mouse_press;

    use tilo_ui_base::synthetic_input::mouse_release;

    fn finger(phase: winit::event::TouchPhase, x: f64, y: f64) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id: 5,
        })
    }

    tilo_test_env::scenario! {
        /// The colour picker's surfaces, strings, reporting, swatches and confirm mode.
        /// Each phase was a test of its own and builds its own fixture.
        fn the_colour_picker_edits_reports_and_confirms_a_colour;

        phase a_finger_drags_the_colour_pickers_channel_sliders;
        {
            use winit::event::TouchPhase;
            let mut p = picker_at(Color::from_rgba8(80, 80, 80, 255), Point::ZERO);
            let r = p.r_slider_rect;
            let y = f64::midpoint(r.y0, r.y1);
            assert!(Widget::claims_raw_touch(&p, Point::new(r.center().x, y)));

            assert!(dispatch(
                &mut p,
                &finger(TouchPhase::Started, r.x0 + 1.0, y)
            ));
            assert!(p.color().r() <= 5, "the finger lands at the left end");
            assert!(Widget::wants_pointer_capture(&p));
            assert!(dispatch(
                &mut p,
                &finger(TouchPhase::Moved, r.x1 - 1.0, y + 80.0)
            ));
            assert!(p.color().r() >= 250, "got {}", p.color().r());
            assert!(dispatch(
                &mut p,
                &finger(TouchPhase::Ended, r.x1 - 1.0, y + 80.0)
            ));
            assert!(!Widget::wants_pointer_capture(&p));
            assert_eq!(p.color().g(), 80, "only the dragged channel moved");
        }

        phase picker_surfaces_edit_hue_saturation_value_and_rgb;
        {
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

        phase compact_confirm_picker_reserves_the_actual_footer_in_portrait_and_landscape;
        {
            for area in [
                Rect::new(24.0, 24.0, 336.0, 776.0),
                Rect::new(24.0, 24.0, 776.0, 336.0),
            ] {
                let mut picker = ColorPicker::new(Color::from_rgb8(50, 100, 150))
                    .with_swatches((0..10).map(|_| ColorSwatch::new(Color::from_rgb8(150, 100, 50))))
                    .confirming("Choose colour");
                let natural = picker.preferred_height(area.width());
                picker.apply_layout_external(area);
                for (target, rect) in picker.test_rects() {
                    assert!(
                        area.contains_rect(rect),
                        "{target} escapes the real compact picker: {rect:?}"
                    );
                }
                assert!(
                    picker.sv_rect.width() >= token(tilo_ui_base::tokens::Metrics::BASE.min_pointer_target)
                );
                assert_eq!(
                    picker.preferred_height(area.width()),
                    natural,
                    "height allocation never changes natural measurement"
                );
                let done = picker.done_rect.center();
                click(&mut picker, done);
                assert!(matches!(
                    picker.take_outcome(),
                    Some(ColorPickerOutcome::Confirmed(_))
                ));
            }
        }

        phase swatches_lay_out_select_and_expose_radio_actions;
        {
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
                use tilo_ui_base::{a11y::Action, a11y::Role};
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
                assert_eq!(radios[1].name.as_deref(), Some("Blue"));
                assert_eq!(radios[1].state.checked, Some(false));

                let mut doc = AutoCommit::new();
                let target = format!("{TARGET_SWATCH}2");
                assert!(Widget::perform_semantic_action(
                    &mut p,
                    &target,
                    &Action::Activate,
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ));
                assert_eq!(p.hex_string(), "#10B981");
                assert!(!Widget::perform_semantic_action(
                    &mut p,
                    &format!("{TARGET_SWATCH}9"),
                    &Action::Activate,
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ));
            }

            swatches_lay_out_below_the_sliders_and_inside_the_picker();
            clicking_a_swatch_sets_the_colour_and_reports_the_change();
            every_swatch_is_a_radio_with_its_own_action();
        }

        phase every_slider_names_its_channel_and_reads_its_value;
        {
            fn each_name_and_value_stand_beside_its_slider_inside_the_picker() {
                let p = laid_out(ColorPicker::new(Color::from_rgb8(0x12, 0x34, 0x56)));
                for slider in [p.r_slider_rect, p.g_slider_rect, p.b_slider_rect] {
                    let name = p.slider_name_column(slider);
                    let value = p.slider_value_column(slider);
                    assert!(
                        name.x1 < slider.x0,
                        "the name stands before the track: {name:?} against {slider:?}"
                    );
                    assert!(
                        value.x0 > slider.x1,
                        "the value stands after it: {value:?} against {slider:?}"
                    );
                    for column in [name, value] {
                        assert!(
                            p.rect.contains_rect(column),
                            "the words are inside the picker: {column:?}"
                        );
                    }
                }
            }

            fn every_caption_name_and_value_is_painted() {
                use vello::kurbo::Affine;
                use vello::Scene;
                let mut p = laid_out(ColorPicker::new(Color::from_rgb8(0x12, 0x34, 0x56)));
                let theme = Theme::dark();
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                Widget::draw(
                    &mut p,
                    &mut tilo_ui_base::draw::DrawCtx::new(
                        &mut scene,
                        Affine::IDENTITY,
                        &mut text,
                        &theme,
                        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
                    ),
                );
                assert!(
                    scene.encoding().resources.glyph_runs.len() >= 6,
                    "each channel paints its name and its value"
                );
            }

            fn every_channel_name_is_drawn_whole_in_each_language() {
                use tilo_ui_base::tokens::TextRole;

                /// Puts the thread back in the language the other cases write in.
                struct Speaking;
                impl Drop for Speaking {
                    fn drop(&mut self) {
                        tilo_ui_base::i18n::set_person_locale(None);
                        tilo_ui_base::i18n::I18nManager::set_locale("en-US");
                    }
                }

                let _restore = Speaking;
                for language in ["en-US", "de-DE"] {
                    tilo_ui_base::i18n::set_person_locale(Some(language));
                    tilo_ui_base::i18n::I18nManager::register_widget_messages();
                    if language == "de-DE" {
                        tilo_ui_base::i18n::I18nManager::add_resource(
                            language,
                            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui-base/locales/de-DE.ftl"))
                                .to_owned(),
                        );
                    }
                    tilo_ui_base::i18n::I18nManager::set_locale(language);
                    if language == "de-DE" {
                        assert_eq!(
                            widget_text("widget-colour-green"),
                            "Grün",
                            "the window speaks German before its columns are measured"
                        );
                    }

                    let mut picker = ColorPicker::new(Color::from_rgb8(0x12, 0x34, 0x56));
                    let (width, height) = picker.preferred_size();
                    let theme = Theme::dark();
                    let style = theme.text(TextRole::Caption);
                    let mut text = TextRenderer::new();
                    tilo_ui_base::widget::layout::lay_out_detached(
                        &mut picker,
                        Rect::new(0.0, 0.0, width, height),
                        &theme,
                        &mut text,
                    )
                    .expect("the picker lays out");

                    for (slider, key) in [
                        (picker.r_slider_rect, "widget-colour-red"),
                        (picker.g_slider_rect, "widget-colour-green"),
                        (picker.b_slider_rect, "widget-colour-blue"),
                    ] {
                        let words = widget_text(key);
                        let column = picker.slider_name_column(slider).width();
                        let measured = f64::from(text.measure_style(&words, None, &style).0);
                        assert!(
                            column + 0.5 >= measured,
                            "{words} ({measured} wide) keeps its own column ({column}) in {language}"
                        );
                        assert_eq!(
                            text.elide_to_width_style(
                                &words,
                                tilo_ui_base::num::f32_of_f64(column),
                                &style
                            ),
                            words,
                            "{words} is drawn whole in {language}"
                        );
                    }
                }
            }

            fn a_reader_steps_a_channel_without_a_pointer() {
                use tilo_ui_base::a11y::Action;
                let mut p = laid_out(ColorPicker::new(Color::from_rgb8(0x12, 0x34, 0x56)));
                let mut doc = AutoCommit::new();
                assert!(Widget::perform_semantic_action(
                    &mut p,
                    TARGET_B,
                    &Action::SetValue("255".into()),
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ));
                assert_eq!(p.color().b(), 255);
                assert_eq!(p.hex_string(), "#1234FF");
            }

            each_name_and_value_stand_beside_its_slider_inside_the_picker();
            every_caption_name_and_value_is_painted();
            every_channel_name_is_drawn_whole_in_each_language();
            a_reader_steps_a_channel_without_a_pointer();
        }

        phase confirm_mode_holds_edits_until_done_or_cancel;
        {
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
                use tilo_ui_base::{a11y::Action, a11y::Role};
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
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
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
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                ));
                assert_eq!(p.hex_string(), "#ABCDEF");
            }

            cancel_restores_the_opening_colour_and_reports_the_revert();
            enter_confirms_and_escape_cancels_while_focused();
            confirm_mode_exposes_titled_group_with_cancel_and_done_actions();
        }

        phase a_bound_text_colour_is_shown_and_followed;
        {
            let state = tilo_ui_base::LocalState::new("#ff0000".to_owned());
            let mut picker = ColorPicker::from_text_state(&state, Color::TRANSPARENT);
            assert_eq!(
                Some(picker.color()),
                parse_hex("#ff0000"),
                "the colour is shown"
            );
            state.set("#00ff00".to_owned());
            picker.sync(&(), &());
            assert_eq!(
                Some(picker.color()),
                parse_hex("#00ff00"),
                "a colour set elsewhere shows in place"
            );
        }
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
        let ev = tilo_ui_base::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
        dispatch(p, &tilo_ui_base::synthetic_input::keyboard_input(ev));
    }
}
