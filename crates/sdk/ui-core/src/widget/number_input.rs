//! `NumberInput` — a compact, click-to-type numeric field with steppers.
//!
//! A number you can nudge with `−`/`+` buttons or click and type directly;
//! values are parsed, validated, and clamped to a range.
//!
//! Two stepper buttons flank an embedded local-mode [`TextInput`].
//! Clicking the field focuses and pre-selects it; **Enter**, a stepper
//! click, a click outside, or blur all **commit** — parsing the leading
//! number (tolerating a trailing unit like `px`), clamping to
//! `[min, max]`, and reverting invalid text to the last good value.
//!
//! [`take_changed`](NumberInput::take_changed) polls a committed user
//! change for the owner to submit; [`set_value`](NumberInput::set_value)
//! pushes one in silently; [`on_action`](NumberInput::on_action) routes each
//! commit through an action, and [`from_state`](NumberInput::from_state)
//! supplies typed process-local two-way binding. The field never writes the
//! document.
//!
//! Automatic dimensions use a text-scaled compact minimum and grow horizontally
//! to fit the formatted value, field padding and steppers. [`NumberInput::with_size`]
//! keeps an explicit logical allocation; long values then scroll in the field.
//!
//! [`NumberStepperFace`] paints the same steppers (and, inline, the value
//! between them) for a painted host that lays its own rows, so a stepper
//! drawn by hand is the one the widget draws.

use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};
use crate::widget::{AppModel, ModelPath};
use crate::widget::{TextInput, TextInputKind};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Narrowest framed control at 1×. The stepper buttons are `height`-sized
/// squares; the field takes the remainder.
const DEFAULT_WIDTH: f64 = 140.0;
/// Gap between the field and each stepper button.
const GAP: f64 = 4.0;

/// What goes between a number and its `unit` in `locale`: nothing before a
/// percent, per-mille or degree sign in the locales that write them so
/// (`50%`, `20°`), a no-break space where the locale sets them apart
/// (`50 %` in German and French), and a space before a word unit (`16 px`).
fn unit_joiner(locale: &str, unit: &str) -> &'static str {
    let sign = matches!(unit, "%" | "‰" | "°" | "′" | "″");
    if !sign {
        return " ";
    }
    let language = locale
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let spaced = unit != "°"
        && matches!(
            language.as_str(),
            "de" | "fr"
                | "es"
                | "nb"
                | "nn"
                | "no"
                | "sv"
                | "fi"
                | "da"
                | "cs"
                | "sk"
                | "ru"
                | "et"
        );
    if spaced { "\u{a0}" } else { "" }
}

/// Parse the leading numeric portion of `s` into an `f64`. Tolerates a
/// leading sign, one decimal point, and any trailing non-numeric suffix
/// (so `"16 px"`, `"16px"`, `"-3.5%"` all parse to their number). Returns
/// `None` when no digits lead the string.
fn parse_leading_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut end = 0usize;
    let mut seen_dot = false;
    let mut seen_digit = false;
    for (i, c) in s.char_indices() {
        if (c == '-' || c == '+') && i == 0 {
            end = i.saturating_add(c.len_utf8());
            continue;
        }
        if c == '.' {
            if seen_dot {
                break;
            }
            seen_dot = true;
            end = i.saturating_add(c.len_utf8());
            continue;
        }
        if c.is_ascii_digit() {
            seen_digit = true;
            end = i.saturating_add(c.len_utf8());
            continue;
        }
        break;
    }
    if !seen_digit {
        return None;
    }
    // `end` is always 0 or the byte offset just past a char boundary
    // visited above, so this can never actually miss — `.get` keeps the
    // lint satisfied without asserting that at runtime.
    s.get(..end).and_then(|leading| leading.parse::<f64>().ok())
}

/// `i64::MIN`/`MAX` as `f64` *literals* (not casts), so clamping into
/// `i64`'s range below doesn't itself need a lossy-cast allow. `f64`
/// can't represent `i64::MAX` exactly; the literal rounds up to the
/// nearest representable value (`2^63`), which only makes the clamp
/// very slightly more permissive at the top edge — irrelevant since the
/// cast below saturates there regardless.
const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;
const I64_MAX_AS_F64: f64 = 9_223_372_036_854_775_807.0;

/// Rounds `v` to the nearest `i64`, clamping first so the saturating
/// float-to-int cast is an explicit, visible step rather than an
/// implicit one the lint can't see through.
#[allow(
    clippy::cast_possible_truncation,
    reason = "value is clamped into i64's representable range immediately above"
)]
fn round_to_i64(v: f64) -> i64 {
    v.round().clamp(I64_MIN_AS_F64, I64_MAX_AS_F64) as i64
}

/// Narrows a screen-space length (`f64`, this widget's own unit) to the
/// `f32` taffy consumes. Control widths/heights never approach `f32`'s
/// range, so this never truncates in practice.
#[allow(
    clippy::cast_possible_truncation,
    reason = "widget widths/heights are always far below f32's range"
)]
fn px_f32(v: f64) -> f32 {
    v as f32
}

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// One of a stepper's two buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepperPart {
    /// The `−` button, at the leading end.
    Decrease,
    /// The `+` button, at the trailing end.
    Increase,
}

/// Immediate-mode number stepper: `−  value  +`, the face of a
/// [`NumberInput`]. Inline (the default) it is the toolbar stepper of
/// [`NumberInput::compact`], bare steppers either side of the value;
/// [`Self::framed`] gives the secondary steppers of the framed control,
/// whose value the host's field draws.
///
/// The steppers are squares of the rect's height at either end; read their
/// geometry from [`Self::steppers`] and their pointer targets from
/// [`Self::hit_rects`], so a painted host hit-tests what it painted.
#[derive(Clone, Copy, Debug)]
pub struct NumberStepperFace<'a> {
    rect: Rect,
    value: &'a str,
    names: (&'a str, &'a str),
    control_size: crate::widget::ControlSize,
    framed: bool,
    hovered: Option<StepperPart>,
    disabled: bool,
}

impl<'a> NumberStepperFace<'a> {
    /// An inline stepper in `rect` showing `value` (already formatted: a
    /// number and its unit, or the host's own terms such as a clock time).
    #[must_use]
    pub fn new(rect: Rect, value: &'a str) -> Self {
        Self {
            rect,
            value,
            names: ("Decrease", "Increase"),
            control_size: crate::widget::ControlSize::Regular,
            framed: false,
            hovered: None,
            disabled: false,
        }
    }

    /// What the two buttons are called for assistive technology and
    /// tooltips, when "Decrease" and "Increase" say less than the host's
    /// own words ("Earlier", "Later").
    #[must_use]
    pub fn names(mut self, decrease: &'a str, increase: &'a str) -> Self {
        self.names = (decrease, increase);
        self
    }

    /// The control size the steppers and the value are set in.
    #[must_use]
    pub fn control_size(mut self, size: crate::widget::ControlSize) -> Self {
        self.control_size = size;
        self
    }

    /// The framed control's secondary steppers; the value is the host's
    /// field, not painted here.
    #[must_use]
    pub fn framed(mut self) -> Self {
        self.framed = true;
        self
    }

    /// The button under the pointer.
    #[must_use]
    pub fn hovered(mut self, part: Option<StepperPart>) -> Self {
        self.hovered = part;
        self
    }

    /// Unavailable: painted under the disabled alpha.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The control's height in `theme`.
    #[must_use]
    pub fn height(size: crate::widget::ControlSize, theme: &Theme) -> f64 {
        size.height(theme)
    }

    /// The width an inline stepper of `size` needs so each of `values`
    /// fits between its steppers without the control changing width as it
    /// steps. Pass the widest values it can show (`"00:00"` and `"Now"`).
    #[must_use]
    pub fn width(
        size: crate::widget::ControlSize,
        values: &[&str],
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let style = size.label_style(theme).numeric();
        let padding = f64::from(theme.padding_standard);
        let value = values
            .iter()
            .map(|value| f64::from(text.measure_style(value, None, &style).0))
            .fold(0.0, f64::max);
        (value + 2.0 * padding + 2.0 * size.height(theme) + 2.0 * GAP).ceil()
    }

    /// The `−` and `+` button faces: squares of the rect's height.
    #[must_use]
    pub fn steppers(&self) -> (Rect, Rect) {
        let r = self.rect;
        let h = r.height();
        (
            Rect::new(r.x0, r.y0, r.x0 + h, r.y1),
            Rect::new(r.x1 - h, r.y0, r.x1, r.y1),
        )
    }

    /// The run between the steppers where the value sits.
    #[must_use]
    pub fn value_rect(&self) -> Rect {
        let (minus, plus) = self.steppers();
        Rect::new(minus.x1 + GAP, self.rect.y0, plus.x0 - GAP, self.rect.y1)
    }

    /// Where a pointer presses each button: its face grown to the minimum
    /// target about its centre.
    #[must_use]
    pub fn hit_rects(&self, theme: &Theme) -> (Rect, Rect) {
        let (minus, plus) = self.steppers();
        (
            self.button(minus, StepperPart::Decrease).hit_rect(theme),
            self.button(plus, StepperPart::Increase).hit_rect(theme),
        )
    }

    /// The button under `point`, by its pointer target.
    #[must_use]
    pub fn part_at(&self, theme: &Theme, point: Point) -> Option<StepperPart> {
        let (minus, plus) = self.hit_rects(theme);
        if minus.contains(point) {
            Some(StepperPart::Decrease)
        } else if plus.contains(point) {
            Some(StepperPart::Increase)
        } else {
            None
        }
    }

    fn button(&self, rect: Rect, part: StepperPart) -> crate::widget::ButtonFace<'a> {
        let (icon, name) = match part {
            StepperPart::Decrease => (crate::icons::Icon::Minus, self.names.0),
            StepperPart::Increase => (crate::icons::Icon::Plus, self.names.1),
        };
        let role = if self.framed {
            crate::widget::ButtonRole::Secondary
        } else {
            crate::widget::ButtonRole::Plain
        };
        crate::widget::ButtonFace::icon_only(rect, icon, name)
            .role(role)
            .control_size(self.control_size)
            .hovered(self.hovered == Some(part) && !self.disabled)
    }

    /// Paint the two steppers.
    pub fn paint_steppers(&self, ctx: &mut PaintCtx<'_>) {
        let (minus, plus) = self.steppers();
        for (rect, part) in [
            (minus, StepperPart::Decrease),
            (plus, StepperPart::Increase),
        ] {
            crate::widget::Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &self.button(rect, part),
            );
        }
    }

    /// Paint the value centred between the steppers, in the size's label
    /// type with tabular figures.
    pub fn paint_value(&self, ctx: &mut PaintCtx<'_>) {
        let style = self.control_size.label_style(ctx.theme).numeric();
        let (w, h) = ctx.text.measure_style(self.value, None, &style);
        let centre = self.value_rect().center();
        let x = centre.x - f64::from(w) / 2.0;
        let y = centre.y - f64::from(h) / 2.0;
        ctx.text.draw_style(
            ctx.scene,
            self.value,
            ctx.theme.tokens.colors.text_primary,
            translated(ctx.transform, x, y),
            None,
            &style,
        );
    }

    /// Paint the whole stepper: the steppers and, inline, the value.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>) {
        if self.disabled {
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                ctx.transform,
                &self.rect,
            );
        }
        self.paint_steppers(ctx);
        if !self.framed {
            self.paint_value(ctx);
        }
        if self.disabled {
            ctx.scene.pop_layer();
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (focus/hover/pending/bound/enabled), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct NumberInput {
    id: String,
    /// Fluent message id spoken for the composite control.
    accessible_name: Option<String>,
    /// Canonical value — the single source of truth. The embedded field's
    /// text is a *view* of this (re-synced on every commit); `value` is
    /// what [`value`](NumberInput::value) reports.
    value: f64,
    min: f64,
    max: f64,
    /// Amount a single stepper press (or Arrow Up/Down) adds / subtracts.
    step: f64,
    /// Decimal places used to format the value for display. `0` renders an
    /// integer (font size, margins); `>0` renders a fixed-precision float
    /// (line spacing `1.15`, opacity `0.50`).
    decimals: usize,
    /// Unit suffix shown after the number (`"px"`, `"%"`, `"pt"`). Purely
    /// cosmetic — it's baked into the display text and stripped on parse.
    suffix: String,
    /// Shows a value in the app's own terms ("Now", "10:57") instead of
    /// the number and its unit.
    formatter: Option<Box<dyn Fn(f64) -> String>>,
    /// Reads typed text back into a value, paired with `formatter`.
    parser: Option<Box<dyn Fn(&str) -> Option<f64>>>,
    rect: Rect,
    field: TextInput,
    field_rect: Rect,
    minus_rect: Rect,
    plus_rect: Rect,
    last_cursor: Point,
    is_focused: bool,
    /// See [`ColorPicker`](crate::widget::ColorPicker)'s hex-input focus
    /// dance: the embedded field isn't in the parent's `node_map`, so when
    /// it asks for focus it sets *our* node id. We stash the request here
    /// and redirect it onto the field inside `set_focus(true)`.
    field_focus_pending: bool,
    minus_hover: bool,
    plus_hover: bool,
    /// One-shot record of the last *committed user change*, drained by
    /// [`take_changed`](NumberInput::take_changed).
    changed: Option<f64>,
    /// Process-local state the committed value is kept in.
    binding: Option<BoxedBinding<f64>>,
    action: Option<ActionEmitter<f64>>,
    enabled: bool,
    /// The control's height and the role of its value.
    control_size: crate::widget::ControlSize,
    /// An inline stepper (`−  value  +`) with bare steppers and no field
    /// frame, for toolbars (zoom, page, point size, copies).
    compact: bool,
    /// The footprint and type seen in `build_layout`, for the measured width.
    minimum: Size<f32>,
    field_padding: f32,
    value_style: crate::text_render::TextStyle,
    interaction: crate::interaction::InteractionEnvironment,
}

impl NumberInput {
    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    /// Construct a number field showing `value`, with a default range of
    /// the full `f64` line, `step = 1`, integer display, and no suffix.
    pub fn new(value: f64) -> Self {
        let mut s = Self {
            id: String::new(),
            accessible_name: None,
            value,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            step: 1.0,
            decimals: 0,
            suffix: String::new(),
            formatter: None,
            parser: None,
            rect: Rect::ZERO,
            field: TextInput::local().with_kind(TextInputKind::Number),
            field_rect: Rect::ZERO,
            minus_rect: Rect::ZERO,
            plus_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            is_focused: false,
            field_focus_pending: false,
            minus_hover: false,
            plus_hover: false,
            changed: None,
            binding: None,
            action: None,
            enabled: true,
            control_size: crate::widget::ControlSize::Regular,
            compact: false,
            minimum: Size::ZERO,
            field_padding: 0.0,
            value_style: crate::tokens::typography::stock()
                .style(crate::widget::ControlSize::Regular.text_role())
                .numeric(),
            interaction: crate::interaction::InteractionEnvironment::default(),
        };
        s.value = s.value.clamp(s.min, s.max);
        s.sync_field();
        s
    }

    /// Create a true two-way binding to typed process-local numeric state.
    ///
    /// Committed typing, steppers, keyboard actions, and accessibility
    /// actions update `state`; mutations through any clone update this field
    /// on the next synchronization pass. [`Self::take_changed`] remains a
    /// user-intent signal and does not report model synchronization.
    pub fn from_state(state: &crate::LocalState<f64>) -> Self {
        let (value, binding) = super::binding::local(state);
        Self {
            binding: Some(binding),
            ..Self::new(value)
        }
    }

    /// Clamp committed + typed values to `[min, max]`. The current value is
    /// re-clamped immediately.
    #[must_use]
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self.value = self.value.clamp(min, max);
        self.sync_field();
        self.publish_local_state();
        self
    }

    /// Amount a single stepper press (or Arrow Up / Down) moves the value.
    #[must_use]
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// Decimal places for display (`0` = integer). Also re-formats the
    /// current value.
    #[must_use]
    pub fn with_precision(mut self, decimals: usize) -> Self {
        self.decimals = decimals;
        self.sync_field();
        self
    }

    /// Show each value in the app's own words (a time stepper's "Now" or
    /// "10:57"); `parse` reads what the person types back into a value
    /// (`None` keeps the current one). The control is measured to hold every
    /// formatted value in range.
    #[must_use]
    pub fn with_value_format(
        mut self,
        format: impl Fn(f64) -> String + 'static,
        parse: impl Fn(&str) -> Option<f64> + 'static,
    ) -> Self {
        self.formatter = Some(Box::new(format));
        self.parser = Some(Box::new(parse));
        self.sync_field();
        self
    }

    /// Read typed text as a value: the app's parser, or the leading number.
    fn parse_text(&self, text: &str) -> Option<f64> {
        match &self.parser {
            Some(parse) => parse(text),
            None => parse_leading_number(text),
        }
    }

    /// Unit suffix appended to the displayed number (`"px"`, `"%"`, …),
    /// placed as the locale writes it (`50%` in English, `50 %` in German).
    #[must_use]
    pub fn with_suffix(mut self, suffix: &str) -> Self {
        self.suffix = suffix.to_string();
        self.sync_field();
        self
    }

    /// Override the [`Widget::test_id`]; also used to disambiguate multiple
    /// number fields in one inspector.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    /// Name the complete number input for assistive technology.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// The control's height and the role of its value (see
    /// [`ControlSize`](crate::widget::ControlSize)). The width is always
    /// measured to hold every value the control can show.
    #[must_use]
    pub fn control_size(mut self, size: crate::widget::ControlSize) -> Self {
        self.control_size = size;
        self
    }

    /// An inline stepper: bare `−` and `+` icon buttons either side of the
    /// value, no field frame, as wide as the widest value. Clicking the value
    /// edits it in place. For toolbars: zoom, page, point size, copies.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Route every committed numeric value through an exported action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<f64>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.changed = None;
        self
    }

    /// Build-time enabled flag. A disabled field is inert and dimmed.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self.field.set_disabled(!enabled);
        self
    }

    /// The current numeric value (always within `[min, max]`).
    pub fn value(&self) -> f64 {
        self.value
    }

    /// The current value as an `i64` (rounded) — convenience for the common
    /// integer-inspector case (font size, margins).
    pub fn value_i64(&self) -> i64 {
        round_to_i64(self.value)
    }

    /// Push a value in programmatically. Clamps and re-formats the
    /// display, but does NOT report a change through
    /// [`take_changed`](NumberInput::take_changed). A bound
    /// [`crate::LocalState`] is updated immediately.
    /// Mirrors [`TextInput::set_text`](crate::widget::TextInput::set_text).
    pub fn set_value(&mut self, v: f64) {
        self.value = v.clamp(self.min, self.max);
        self.sync_field();
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    /// Poll for a committed user change since the last call, draining it.
    /// Returns `Some(value)` after the user commits via Enter / a stepper /
    /// blur; `None` otherwise.
    pub fn take_changed(&mut self) -> Option<f64> {
        self.changed.take()
    }

    /// Whether the field currently owns keyboard focus (the embedded text
    /// input is being edited).
    pub fn is_editing(&self) -> bool {
        self.field.is_focused()
    }

    /// Format `value` for display: `decimals`-precision number plus the
    /// unit suffix (space-separated when a suffix is present).
    fn format_value(&self) -> String {
        self.format_display(self.value)
    }

    /// `value` formatted exactly as [`Self::format_value`] would show it.
    fn format_display(&self, value: f64) -> String {
        if let Some(format) = &self.formatter {
            return format(value);
        }
        let n = if self.decimals == 0 {
            format!("{}", round_to_i64(value))
        } else {
            format!("{:.*}", self.decimals, value)
        };
        if self.suffix.is_empty() {
            n
        } else {
            let locale = crate::i18n::I18nManager::current_locale();
            format!("{n}{}{}", unit_joiner(&locale, &self.suffix), self.suffix)
        }
    }

    /// Control width that holds `display` in the field beside two
    /// `height`-square steppers: `field_padding` either side of the text,
    /// the two buttons and the gaps between, never below `minimum.width`.
    fn required_width(
        minimum: Size<f32>,
        field_padding: f32,
        display: &str,
        style: &crate::text_render::TextStyle,
        text: &mut TextRenderer,
    ) -> f32 {
        let (value_width, _) = text.measure_style(display, None, style);
        minimum
            .width
            .max(value_width + field_padding * 2.0 + minimum.height * 2.0 + px_f32(GAP * 2.0))
            .ceil()
    }

    /// The footprint `build_layout` starts from: the size's height (a
    /// coarse pointer grows it to the minimum target; a fine one reaches the
    /// target through the steppers' hit rects) and, for the framed control,
    /// the reference width at the text scale.
    fn base_footprint(&self, theme: &Theme) -> Size<f32> {
        let height = self.control_size.height(theme);
        let height = if self.interaction.supports_hover() {
            height
        } else {
            height.max(f64::from(self.interaction.minimum_target()))
        };
        let width = if self.compact {
            0.0
        } else {
            DEFAULT_WIDTH * f64::from(theme.text_scale)
        };
        Size {
            width: px_f32(width),
            height: px_f32(height),
        }
    }

    /// Width this control needs so **every** value it can show fits — the
    /// current value and the finite ends of its range, formatted with the
    /// configured precision and suffix — beside its steppers. A host laying
    /// its own rows (a font-size stepper in an inspector) reserves this
    /// instead of guessing the field's type size; it is the same rule the
    /// widget applies to itself in `adjust_layout_with_measure`, so a slot
    /// sized from it never clips a value the widget would have held.
    pub fn preferred_width(&self, theme: &Theme, text: &mut TextRenderer) -> f64 {
        let minimum = self.base_footprint(theme);
        let padding = px_f32(self.field.text_padding_x(theme));
        let style = self.value_style_in(theme);
        let widest = [self.value, self.min, self.max]
            .into_iter()
            .filter(|value| value.is_finite())
            .map(|value| {
                Self::required_width(minimum, padding, &self.format_display(value), &style, text)
            })
            .fold(minimum.width, f32::max);
        f64::from(widest)
    }

    /// Overwrite the embedded field's text with the canonical value's
    /// formatted string.
    fn sync_field(&mut self) {
        let t = self.format_value();
        self.field.set_text(&t);
    }

    /// Commit whatever is typed: parse the leading number, clamp, adopt
    /// it. Records a change for the owner
    /// ([`take_changed`](NumberInput::take_changed)) only when the value
    /// actually moved; always re-syncs the display so invalid/out-of-range
    /// text snaps back.
    fn commit_local(&mut self) {
        if let Some(parsed) = self.parse_text(self.field.value()) {
            let clamped = parsed.clamp(self.min, self.max);
            if (clamped - self.value).abs() > f64::EPSILON {
                self.value = clamped;
                self.record_user_value(clamped);
            }
        }
        // Whether or not the value moved, re-render the canonical text so a
        // trailing unit is restored and bad input is discarded.
        self.sync_field();
    }

    /// Move the value by `delta` (one stepper press / arrow), clamping.
    /// Records a change + pending write when it actually moves.
    fn step_by(&mut self, delta: f64) {
        let next = (self.value + delta).clamp(self.min, self.max);
        if (next - self.value).abs() > f64::EPSILON {
            self.value = next;
            self.record_user_value(next);
        }
        self.sync_field();
    }

    /// Position the control at an externally-computed `rect`, without a
    /// taffy node — the mirror of [`TextInput::apply_layout_external`].
    /// Computes the same sub-rects `apply_layout` does.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
        let h = y1 - y0;
        self.minus_rect = Rect::new(x0, y0, x0 + h, y1);
        self.plus_rect = Rect::new(x1 - h, y0, x1, y1);
        self.field_rect = Rect::new(self.minus_rect.x1 + GAP, y0, self.plus_rect.x0 - GAP, y1);
        self.field.apply_layout_external(self.field_rect);
    }

    /// The value's type: the field's body text, or the size's label role
    /// with tabular figures when compact.
    fn value_style_in(&self, theme: &Theme) -> crate::text_render::TextStyle {
        if self.compact {
            self.control_size.label_style(theme).numeric()
        } else {
            theme.text(crate::tokens::TextRole::Body)
        }
    }

    /// Where a pointer presses a stepper: its face grown to the minimum
    /// target about its centre.
    fn stepper_hit(&self, stepper: Rect) -> Rect {
        self.interaction.minimum_target_rect(stepper)
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.binding {
            binding.publish(&self.value);
        }
    }

    fn record_user_value(&mut self, value: f64) {
        if let Some(action) = &mut self.action {
            let _ = action.emit(value);
        } else {
            self.changed = Some(value);
            self.publish_local_state();
        }
    }
}

impl Widget for NumberInput {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.field.set_interaction_environment(environment);
        self.minus_hover &= environment.supports_hover();
        self.plus_hover &= environment.supports_hover();
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // Don't clobber the field while the user is mid-edit; the model is
        // adopted as soon as editing ends.
        if self.field.is_focused() {
            return;
        }
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(v) = binding.pull(state, root)
        {
            let clamped = v.clamp(self.min, self.max);
            if (clamped - self.value).abs() > f64::EPSILON {
                self.value = clamped;
                self.sync_field();
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let size = self.base_footprint(theme);
        self.minimum = size;
        self.field_padding = px_f32(self.field.text_padding_x(theme));
        self.value_style = self.value_style_in(theme);
        let style = Style {
            size: Size {
                width: Dimension::length(size.width),
                height: Dimension::length(size.height),
            },
            // A fixed target: in a row that overflows, the fluid field beside
            // it gives, not the stepper (taffy would otherwise squeeze this
            // leaf below its own size — see `Toggle::build_layout`).
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn adjust_layout_with_measure(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        // Every value the control can show fits, so stepping never resizes it.
        let width = [self.value, self.min, self.max]
            .into_iter()
            .filter(|value| value.is_finite())
            .map(|value| {
                Self::required_width(
                    self.minimum,
                    self.field_padding,
                    &self.format_display(value),
                    &self.value_style,
                    text,
                )
            })
            .fold(self.minimum.width, f32::max);
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.size.width = Dimension::length(width);
        // Keep authored min/max constraints, including Manual's exact allocation.
        if style == *current {
            return false;
        }
        tree.set_style(node, style).is_ok()
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
        let disabled = !self.effectively_enabled();
        if disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &self.rect,
            );
        }

        // --- Steppers: the shared stepper face, bare when compact. The value
        // is the embedded field (its own chrome), or, compact and at rest,
        // the bare value centred between the steppers ---
        let shown = self.format_value();
        let hovered = if self.minus_hover {
            Some(StepperPart::Decrease)
        } else if self.plus_hover {
            Some(StepperPart::Increase)
        } else {
            None
        };
        let face = NumberStepperFace::new(self.rect, &shown)
            .control_size(self.control_size)
            .hovered(hovered)
            .disabled(disabled);
        let face = if self.compact { face } else { face.framed() };
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        face.paint_steppers(&mut ctx);
        if self.compact && !self.field.is_focused() {
            face.paint_value(&mut ctx);
        } else {
            self.field
                .draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
        }

        if disabled {
            scene.pop_layer();
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.effectively_enabled() {
            return;
        }

        // Forward to the embedded field first (click-to-focus, typing, IME).
        // The field isn't in the parent's node_map, so a focus request it
        // makes sets *our* node id — redirect it onto the field in set_focus.
        let prev_focus = *ctx.focus_target;
        self.field.handle_event(event, ctx);
        if *ctx.focus_target != prev_focus && self.field_rect.contains(self.last_cursor) {
            self.field_focus_pending = true;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let mh = self.stepper_hit(self.minus_rect).contains(p);
                let ph = !mh && self.stepper_hit(self.plus_rect).contains(p);
                if mh != self.minus_hover || ph != self.plus_hover {
                    self.minus_hover = mh;
                    self.plus_hover = ph;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: el,
                button: MouseButton::Left,
                ..
            } if *el == ElementState::Pressed => {
                if self.minus_hover {
                    // Commit any half-typed edit before stepping so the step
                    // is relative to what the user last saw, then blur the field.
                    if self.field.is_focused() {
                        self.commit_local();
                        self.field.set_focus(false);
                    }
                    self.step_by(-self.step);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if self.plus_hover {
                    if self.field.is_focused() {
                        self.commit_local();
                        self.field.set_focus(false);
                    }
                    self.step_by(self.step);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if !self.field_rect.contains(self.last_cursor) && self.field.is_focused() {
                    // Press landed outside the field while it was focused —
                    // commit the typed value and release focus.
                    self.commit_local();
                    self.field.set_focus(false);
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed && self.field.is_focused() => {
                match &key_event.logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.commit_local();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.commit_local();
                        self.step_by(self.step);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.commit_local();
                        self.step_by(-self.step);
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        // The embedded field drives its own selection drag capture.
        self.field.is_dragging()
    }

    fn name(&self) -> &'static str {
        "NumberInput"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("number_input")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.effectively_enabled() {
            return None;
        }
        if self.field_rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else if self.minus_rect.contains(point) || self.plus_rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        self.effectively_enabled()
    }
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }
    fn set_focus(&mut self, focused: bool) {
        if focused {
            // Route focus into the embedded field so it's immediately
            // editable, however it arrived. A click in the field also
            // pre-selects the value so the first keystroke replaces it;
            // focus arriving any other way leaves the caret where it is.
            let via_field_click = self.field_focus_pending;
            self.field_focus_pending = false;
            self.is_focused = true;
            self.field.set_focus(true);
            if via_field_click {
                self.field.select_all();
            }
        } else {
            self.is_focused = false;
            if self.field.is_focused() {
                // Blur commits the typed value for the owner to take.
                self.commit_local();
                self.field.set_focus(false);
            }
        }
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.field)
    }
    fn is_focused(&self) -> bool {
        self.is_focused || self.field.is_focused()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.field.ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        // Winit has no numeric purpose yet. Normal still activates the
        // standard text-input-v3 lifecycle; the field continues to validate
        // and clamp committed text itself.
        Some(winit::window::ImePurpose::Normal)
    }

    /// Reports `Role::SpinButton` with the formatted value; an assistive
    /// technology can nudge it with Increment / Decrement or jump it with
    /// `SetValue`, all through the same commit path as pointer/keyboard input.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut node = AccessNode::new(Role::SpinButton)
            .with_semantic_key(self.test_id().unwrap_or("number_input"))
            .with_optional_name(self.accessible_name.as_ref().map(|name| crate::i18n!(name)))
            .with_value(self.format_value())
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.effectively_enabled(),
                ..Default::default()
            });
        if self.action.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("number_input"));
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::Increment => {
                self.step_by(self.step);
                true
            }
            Action::Decrement => {
                self.step_by(-self.step);
                true
            }
            Action::SetValue(v) => {
                let Some(parsed) = self.parse_text(v) else {
                    return false;
                };
                let clamped = parsed.clamp(self.min, self.max);
                if (clamped - self.value).abs() > f64::EPSILON {
                    self.value = clamped;
                    self.record_user_value(clamped);
                }
                self.sync_field();
                true
            }
            _ => false,
        }
    }
}

impl crate::widget::controls::ActionControls for NumberInput {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("number_input");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::SemanticAction(control_id.to_string()),
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::Modifiers;

    fn layout_with_theme(n: &mut NumberInput, theme: &Theme) {
        let mut tree = TaffyTree::new();
        let node = n.build_layout(&mut tree, theme, &mut std::collections::HashMap::new());
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(n));
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        unsafe {
            bindings.rebuild(n, &tree, node, &nodes);
            crate::widget::compute_widget_layout(
                &mut tree,
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(292.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &nodes,
                &bindings,
                &mut text,
            )
            .unwrap();
        }
        n.apply_layout(&tree, node, Point::new(24.0, 12.0));
    }

    #[test]
    fn number_footprint_fits_large_text_and_widest_value_unless_manual() {
        // Case `automatic_defaults_fit_large_text_and_keep_explicit_sizes`.
        {
            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut text = TextRenderer::new();
                text.set_text_scale(scale);
                let mut number = NumberInput::new(15.0).with_suffix("min");
                layout_with_theme(&mut number, &theme);
                assert!(number.rect.width() >= DEFAULT_WIDTH * f64::from(scale));
                assert_eq!(
                    number.rect.height(),
                    crate::widget::ControlSize::Regular.height(&theme)
                );
                assert!(number.rect.width() <= 292.0, "starter narrow control width");
                let (value_width, value_height) = text.measure_style(
                    number.field.value(),
                    None,
                    &theme.text(crate::tokens::TextRole::Body),
                );
                assert!(
                    f64::from(value_width) + 2.0 * number.field.text_padding_x(&theme)
                        <= number.field_rect.width(),
                    "value width {value_width}, field {:?}",
                    number.field_rect
                );
                assert!(f64::from(value_height) <= number.field_rect.height());
                number.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &crate::PresenceCtx::default(),
                );
                assert_eq!(number.field.get_rect(), number.field_rect);
                number.field.set_text("25 min");
                number.commit_local();
                assert_eq!(number.value(), 25.0);
                assert_eq!(number.field.value(), "25 min");
                let mut compact = NumberInput::new(100.0)
                    .with_range(10.0, 400.0)
                    .with_suffix("%")
                    .compact()
                    .control_size(crate::widget::ControlSize::Small);
                layout_with_theme(&mut compact, &theme);
                assert!(
                    compact.rect.width() < number.rect.width(),
                    "a compact stepper hugs its value: {:?}",
                    compact.rect
                );
                assert_eq!(
                    compact.rect.height(),
                    crate::widget::ControlSize::Small.height(&theme)
                );
                let (widest, _) = text.measure_style("400%", None, &compact.value_style);
                assert!(f64::from(widest) <= compact.field_rect.width());
            }
        }

        // Case `manual_allocation_overrides_automatic_number_footprint`.
        {
            let manual = crate::widget::Manual::new(|ctx| {
                ctx.place(0, Rect::new(10.25, 20.75, 130.75, 61.0))?;
                Ok(Size {
                    width: ctx.available().width,
                    height: 100.0,
                })
            })
            .child(Box::new(NumberInput::new(15.0).with_suffix("min")));
            let harness = crate::headless::FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(manual),
                (340, 300),
                Theme::dark().with_text_scale(2.0),
            );
            assert_eq!(
                harness.inspection().root.children[0].rect,
                [10.25, 20.75, 120.5, 40.25]
            );
        }

        // Case `steppers_are_square_icon_buttons_with_pointer_target_hits`.
        {
            let theme = Theme::dark();
            for compact in [false, true] {
                let mut number =
                    NumberInput::new(15.0).control_size(crate::widget::ControlSize::Small);
                if compact {
                    number = number.compact();
                }
                layout_with_theme(&mut number, &theme);
                for stepper in [number.minus_rect, number.plus_rect] {
                    assert_eq!(stepper.width(), stepper.height(), "{stepper:?}");
                    let hit = number.stepper_hit(stepper);
                    assert!(hit.width() >= 32.0 && hit.height() >= 32.0, "{hit:?}");
                }
                let mut scene = Scene::new();
                number.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                    &crate::PresenceCtx::default(),
                );
                assert_eq!(
                    scene.encoding().resources.glyph_runs.len(),
                    1,
                    "the steppers are icons; only the value is text"
                );
            }
        }

        // Case `preferred_width_fits_the_widest_value_in_range`.
        // `preferred_width` reserves room for the widest value the control can
        // show — the ends of its range, formatted with precision and suffix —
        // beside its two steppers, never less than the configured width. A host
        // slot sized from it holds every value the widget itself would hold.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let input = NumberInput::new(1.0)
                .with_range(0.0, 1000.0)
                .with_precision(2)
                .with_suffix("px");
            let preferred = input.preferred_width(&theme, &mut text);

            let (widest, _) = text.measure_style(
                "1000.00 px",
                None,
                &theme.text(crate::tokens::TextRole::Body),
            );
            let padding = px_f32(input.field.text_padding_x(&theme));
            let height = px_f32(crate::widget::ControlSize::Regular.height(&theme));
            let required = f64::from(widest + padding * 2.0 + height * 2.0 + px_f32(GAP * 2.0));
            assert!(
                preferred + 1e-6 >= required,
                "preferred {preferred} must hold the widest value ({required})"
            );
            assert!(
                preferred >= DEFAULT_WIDTH,
                "never below the configured width"
            );
            assert!(
                preferred < required + 1.0,
                "preferred {preferred} should be the widest value's need rounded up, not more (required {required})"
            );

            // A narrow range needs no more than the configured width.
            let compact = NumberInput::new(3.0).with_range(0.0, 9.0);
            assert_eq!(compact.preferred_width(&theme, &mut text), DEFAULT_WIDTH);
        }
    }

    fn layout_at(n: &mut NumberInput, x0: f64, y0: f64) {
        // The Regular footprint at 1×, placed without a taffy pass.
        let height = crate::widget::ControlSize::Regular.height(&Theme::dark());
        n.apply_layout_external(Rect::new(x0, y0, x0 + DEFAULT_WIDTH, y0 + height));
    }

    fn dispatch(n: &mut NumberInput, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        n.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    use crate::synthetic_input::cursor_moved;
    use crate::synthetic_input::mouse_press;
    fn named_key(k: NamedKey) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Named(k),
            None,
            ElementState::Pressed,
        ))
    }

    #[test]
    fn number_text_parses_and_formats_with_units_signs_and_precision() {
        // Case `parse_leading_number_tolerates_units_and_signs`.
        {
            assert_eq!(parse_leading_number("16"), Some(16.0));
            assert_eq!(parse_leading_number("16 px"), Some(16.0));
            assert_eq!(parse_leading_number("16px"), Some(16.0));
            assert_eq!(parse_leading_number("  -3.5% "), Some(-3.5));
            assert_eq!(parse_leading_number("1.15"), Some(1.15));
            assert_eq!(parse_leading_number("px"), None);
            assert_eq!(parse_leading_number(""), None);
            assert_eq!(parse_leading_number("."), None);
        }

        // Case `format_value_respects_precision_and_suffix`.
        {
            let n = NumberInput::new(16.0).with_suffix("px");
            assert_eq!(n.format_value(), "16 px");
            let n = NumberInput::new(1.153).with_precision(2);
            assert_eq!(n.format_value(), "1.15");
            let n = NumberInput::new(42.0);
            assert_eq!(n.format_value(), "42");
        }

        // Case `new_and_with_range_clamp_initial_value`.
        // New clamps the initial value into range.
        {
            let n = NumberInput::new(500.0).with_range(8.0, 72.0);
            assert_eq!(n.value(), 72.0, "over-max initial value clamps to max");
            let n = NumberInput::new(1.0).with_range(8.0, 72.0);
            assert_eq!(n.value(), 8.0, "under-min initial value clamps to min");
        }
    }

    #[test]
    fn typed_values_commit_clamped_or_revert() {
        // Case `type_then_enter_parses_clamps_and_commits`.
        // Typing a value and committing with Enter parses, clamps and reports
        // the change through the production `handle_event` path.
        {
            let mut n = NumberInput::new(16.0)
                .with_range(8.0, 72.0)
                .with_suffix("px");
            layout_at(&mut n, 0.0, 0.0);

            // Focus the field (as a click would) and replace its text with an
            // out-of-range value.
            n.field.set_focus(true);
            n.field.set_text("999");
            dispatch(&mut n, &named_key(NamedKey::Enter));

            assert_eq!(n.value(), 72.0, "typed 999 clamps to max 72");
            assert_eq!(
                n.take_changed(),
                Some(72.0),
                "commit reports the clamped change"
            );
            assert_eq!(n.take_changed(), None, "take_changed drains");
            // Display re-rendered to the canonical value + suffix.
            assert_eq!(n.field.value(), "72 px");
        }

        // Case `type_with_unit_suffix_commits_number`.
        // A valid in-range value commits verbatim; a unit suffix in the typed
        // text is tolerated by the parser.
        {
            let mut n = NumberInput::new(16.0)
                .with_range(1.0, 100.0)
                .with_suffix("px");
            layout_at(&mut n, 0.0, 0.0);
            n.field.set_focus(true);
            n.field.set_text("24 px");
            dispatch(&mut n, &named_key(NamedKey::Enter));
            assert_eq!(n.value(), 24.0);
            assert_eq!(n.take_changed(), Some(24.0));
        }

        // Case `invalid_text_reverts_and_reports_nothing`.
        // Invalid text reverts to the last good value with no reported change.
        {
            let mut n = NumberInput::new(16.0).with_range(8.0, 72.0);
            layout_at(&mut n, 0.0, 0.0);
            n.field.set_focus(true);
            n.field.set_text("abc");
            dispatch(&mut n, &named_key(NamedKey::Enter));
            assert_eq!(n.value(), 16.0, "unparseable text keeps the old value");
            assert_eq!(n.field.value(), "16", "display snapped back to the value");
            assert_eq!(n.take_changed(), None, "no change to report");
        }

        // Case `blur_commits_the_typed_value_for_the_owner`.
        {
            let mut n = NumberInput::new(16.0).with_range(8.0, 72.0);
            layout_at(&mut n, 0.0, 0.0);
            n.field.set_focus(true);
            n.field.set_text("40");

            // App moves focus elsewhere → set_focus(false) commits locally.
            Widget::set_focus(&mut n, false);
            assert_eq!(n.value(), 40.0, "blur committed the typed value");
            assert_eq!(n.take_changed(), Some(40.0));
        }
    }

    #[test]
    fn steppers_and_arrow_keys_step_and_clamp() {
        // Case `steppers_increment_and_clamp`.
        // Clicking the + / − steppers moves by `step`, clamped, and reports it.
        {
            let mut n = NumberInput::new(70.0).with_range(8.0, 72.0).with_step(2.0);
            layout_at(&mut n, 0.0, 0.0);
            let plus_c = (
                n.plus_rect.x0.midpoint(n.plus_rect.x1),
                n.plus_rect.y0.midpoint(n.plus_rect.y1),
            );
            let minus_c = (
                n.minus_rect.x0.midpoint(n.minus_rect.x1),
                n.minus_rect.y0.midpoint(n.minus_rect.y1),
            );

            // Click + once: 70 → 72.
            dispatch(&mut n, &cursor_moved(plus_c.0, plus_c.1));
            dispatch(&mut n, &mouse_press());
            assert_eq!(n.value(), 72.0);
            assert_eq!(n.take_changed(), Some(72.0));

            // Click + again: already at max → no move, no report.
            dispatch(&mut n, &mouse_press());
            assert_eq!(n.value(), 72.0, "clamped at max");
            assert_eq!(n.take_changed(), None, "no change when already clamped");

            // Click − : 72 → 70.
            dispatch(&mut n, &cursor_moved(minus_c.0, minus_c.1));
            dispatch(&mut n, &mouse_press());
            assert_eq!(n.value(), 70.0);
            assert_eq!(n.take_changed(), Some(70.0));
        }

        // Case `arrow_keys_step_while_editing`.
        // Arrow Up / Down while editing step the value.
        {
            let mut n = NumberInput::new(16.0).with_range(1.0, 100.0).with_step(1.0);
            layout_at(&mut n, 0.0, 0.0);
            n.field.set_focus(true);
            dispatch(&mut n, &named_key(NamedKey::ArrowUp));
            assert_eq!(n.value(), 17.0);
            dispatch(&mut n, &named_key(NamedKey::ArrowDown));
            dispatch(&mut n, &named_key(NamedKey::ArrowDown));
            assert_eq!(n.value(), 15.0);
        }

        // Case `keyboard_focus_makes_field_editable`.
        // Keyboard (Tab) focus routes into the editable field so it's typeable
        // without a mouse — arrows then step it.
        {
            let mut n = NumberInput::new(16.0).with_range(1.0, 100.0).with_step(1.0);
            layout_at(&mut n, 0.0, 0.0);
            Widget::set_focus(&mut n, true);
            assert!(
                n.is_editing(),
                "keyboard focus routes into the editable field"
            );
            dispatch(&mut n, &named_key(NamedKey::ArrowUp));
            assert_eq!(n.value(), 17.0, "arrows step a keyboard-focused field");
        }

        // Case `disabled_field_rejects_input`.
        {
            let mut n = NumberInput::new(16.0)
                .with_range(8.0, 72.0)
                .with_enabled(false);
            layout_at(&mut n, 0.0, 0.0);
            let before = n.value();
            let plus_c = (
                n.plus_rect.x0.midpoint(n.plus_rect.x1),
                n.plus_rect.y0.midpoint(n.plus_rect.y1),
            );
            dispatch(&mut n, &cursor_moved(plus_c.0, plus_c.1));
            dispatch(&mut n, &mouse_press());
            assert_eq!(n.value(), before, "disabled field ignores stepper clicks");
            assert!(!Widget::is_focusable(&n));
        }
    }

    #[test]
    fn value_bindings_sync_silently_from_models() {
        // Case `set_value_is_silent`.
        // `set_value` is programmatic: it clamps + updates the display but does
        // NOT report a user change.
        {
            let mut n = NumberInput::new(16.0).with_range(8.0, 72.0);
            n.set_value(200.0);
            assert_eq!(n.value(), 72.0, "clamped");
            assert_eq!(
                n.take_changed(),
                None,
                "programmatic set is not a user change"
            );
            assert_eq!(n.field.value(), "72");
        }

        // Case `local_state_binding_is_two_way_across_number_inputs`.
        {
            use crate::a11y::Action;

            let state = crate::LocalState::new(16.0);
            let mut first = NumberInput::from_state(&state)
                .with_range(8.0, 72.0)
                .with_step(1.0);
            let mut second = NumberInput::from_state(&state).with_range(8.0, 72.0);
            let mut document = AutoCommit::new();

            state.set(24.0);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(first.value(), 24.0);
            assert_eq!(second.value(), 24.0);
            assert_eq!(first.take_changed(), None, "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &Action::Increment,
                &mut crate::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), 25.0);
            assert_eq!(first.take_changed(), Some(25.0));
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(second.value(), 25.0);

            second.set_value(30.0);
            assert_eq!(state.get(), 30.0);
            assert_eq!(second.take_changed(), None);
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert_eq!(first.value(), 30.0);
        }
    }

    #[test]
    fn accessibility_reports_spinbutton_and_routes_value_actions() {
        // Case `accessibility_reports_spinbutton_and_value`.
        {
            use crate::a11y::Role;
            let n = NumberInput::new(16.0).with_suffix("px");
            let node = Widget::accessibility(&n).unwrap();
            assert_eq!(node.role, Role::SpinButton);
            assert_eq!(node.value.as_deref(), Some("16 px"));
            assert!(node.state.focusable);
        }

        // Case `perform_action_increment_and_setvalue`.
        {
            use crate::a11y::Action;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut n = NumberInput::new(16.0).with_range(8.0, 72.0).with_step(4.0);
            assert!(Widget::perform_action(
                &mut n,
                &Action::Increment,
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert_eq!(n.value(), 20.0);
            assert!(Widget::perform_action(
                &mut n,
                &Action::SetValue("100".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &root
            ));
            assert_eq!(n.value(), 72.0, "SetValue clamps");
        }

        // Case `action_binding_owns_accessibility_value_changes`.
        {
            let mut number =
                NumberInput::new(10.0).on_action(crate::widget::test_number_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut number,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&number));
            crate::widget::controls::refresh_control_action_availability(
                &mut number,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&number));
            assert!(Widget::perform_action(
                &mut number,
                &crate::a11y::Action::SetValue("42".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get_heads().is_empty(), "the field writes nothing");
            assert_eq!(number.take_changed(), None);
            assert_eq!(
                crate::widget::controls::take_control_invocation(&mut number)
                    .unwrap()
                    .arguments["value"],
                42.0
            );
        }
    }

    #[test]
    fn values_read_in_the_apps_words_and_units_sit_where_the_locale_puts_them() {
        assert_eq!(unit_joiner("en-US", "%"), "");
        assert_eq!(unit_joiner("de-DE", "%"), "\u{a0}");
        assert_eq!(unit_joiner("fr", "%"), "\u{a0}");
        assert_eq!(unit_joiner("de-DE", "°"), "");
        assert_eq!(unit_joiner("en-US", "px"), " ");
        let percent = NumberInput::new(50.0).with_suffix("%");
        assert_eq!(percent.field.value(), "50%");

        let mut time = NumberInput::new(0.0)
            .with_range(0.0, 1440.0)
            .with_step(1.0)
            .with_value_format(
                |minutes| {
                    if minutes == 0.0 {
                        "Now".to_owned()
                    } else {
                        let minutes = round_to_i64(minutes);
                        format!("{}:{:02}", minutes / 60, minutes % 60)
                    }
                },
                |text| {
                    if text.trim().eq_ignore_ascii_case("now") {
                        return Some(0.0);
                    }
                    let (hours, minutes) = text.trim().split_once(':')?;
                    Some(hours.parse::<f64>().ok()? * 60.0 + minutes.parse::<f64>().ok()?)
                },
            );
        assert_eq!(time.field.value(), "Now");
        time.set_value(657.0);
        assert_eq!(time.field.value(), "10:57");
        time.field.set_text("11:05");
        time.commit_local();
        assert_eq!(time.value(), 665.0);
        assert_eq!(time.field.value(), "11:05");
    }
}
