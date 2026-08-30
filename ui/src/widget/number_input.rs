//! `NumberInput` — a compact, click-to-type numeric field with steppers.
//!
//! A number you can nudge with `−`/`+` buttons or click and type
//! directly; always parsed, validated and clamped to a range. Before this
//! widget every inspector (font size, line spacing, opacity, margins, …)
//! hand-rolled it.
//!
//! Two stepper buttons flank an embedded local-mode [`TextInput`].
//! Clicking the field focuses and pre-selects it; **Enter**, a stepper
//! click, a click outside, or blur all **commit** — parsing the leading
//! number (tolerating a trailing unit like `px`), clamping to
//! `[min, max]`, and reverting invalid text to the last good value.
//!
//! [`take_changed`](NumberInput::take_changed) polls a committed user
//! change; [`set_value`](NumberInput::set_value) pushes one in silently;
//! [`with_on_change_path`](NumberInput::with_on_change_path) optionally
//! binds the value to a CRDT `F64`, like [`Slider`](crate::widget::Slider).

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::TextInput;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Default control height in logical pixels — matches [`TextInput`]'s
/// 40px form-field band closely enough while staying compact for an
/// inspector row.
const DEFAULT_HEIGHT: f64 = 32.0;
/// Default overall width. The stepper buttons are `height`-sized squares;
/// the field takes the remainder.
const DEFAULT_WIDTH: f64 = 140.0;
/// Gap between the field and each stepper button.
const GAP: f64 = 4.0;
/// Point size of the `−` / `+` stepper glyphs.
const STEP_GLYPH_SIZE: f32 = 18.0;

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
            end = i + c.len_utf8();
            continue;
        }
        if c == '.' {
            if seen_dot {
                break;
            }
            seen_dot = true;
            end = i + c.len_utf8();
            continue;
        }
        if c.is_ascii_digit() {
            seen_digit = true;
            end = i + c.len_utf8();
            continue;
        }
        break;
    }
    if !seen_digit {
        return None;
    }
    s[..end].parse::<f64>().ok()
}

pub struct NumberInput {
    id: String,
    /// Canonical value — the single source of truth. The embedded field's
    /// text is a *view* of this (re-synced on every commit); `value` is
    /// what [`value`](NumberInput::value) and the CRDT reflect.
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
    /// A committed value awaiting a CRDT write. Set when a commit happens
    /// where we lack the `state` handle (blur via `set_focus(false)`);
    /// flushed on the next `sync`.
    pending_write: Option<f64>,
    on_change_path: String,
    crdt_bound: bool,
    enabled: bool,
    width: f64,
    height: f64,
}

impl NumberInput {
    /// Construct a number field showing `value`, with a default range of
    /// the full `f64` line, `step = 1`, integer display, and no suffix.
    pub fn new(value: f64) -> Self {
        let mut s = Self {
            id: String::new(),
            value,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            step: 1.0,
            decimals: 0,
            suffix: String::new(),
            rect: Rect::ZERO,
            field: TextInput::local(),
            field_rect: Rect::ZERO,
            minus_rect: Rect::ZERO,
            plus_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            is_focused: false,
            field_focus_pending: false,
            minus_hover: false,
            plus_hover: false,
            changed: None,
            pending_write: None,
            on_change_path: String::new(),
            crdt_bound: false,
            enabled: true,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        };
        s.value = s.value.clamp(s.min, s.max);
        s.sync_field();
        s
    }

    /// Clamp committed + typed values to `[min, max]`. The current value is
    /// re-clamped immediately.
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self.value = self.value.clamp(min, max);
        self.sync_field();
        self
    }

    /// Amount a single stepper press (or Arrow Up / Down) moves the value.
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// Decimal places for display (`0` = integer). Also re-formats the
    /// current value.
    pub fn with_precision(mut self, decimals: usize) -> Self {
        self.decimals = decimals;
        self.sync_field();
        self
    }

    /// Unit suffix appended to the displayed number (`"px"`, `"%"`, …).
    pub fn with_suffix(mut self, suffix: &str) -> Self {
        self.suffix = suffix.to_string();
        self.sync_field();
        self
    }

    /// Override the [`Widget::test_id`]; also used to disambiguate multiple
    /// number fields in one inspector.
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    /// Fixed control width / height in logical pixels.
    pub fn with_size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Bind the committed value to a CRDT `F64` at `path` under the widget's
    /// root — read on `sync`, written on every commit. Empty path leaves the
    /// widget self-contained (poll [`take_changed`](NumberInput::take_changed)).
    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self.crdt_bound = !path.is_empty();
        self
    }

    /// Build-time enabled flag. A disabled field is inert and dimmed.
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
        self.value.round() as i64
    }

    /// Push a value in programmatically. Clamps and re-formats the
    /// display, but does NOT report a change through
    /// [`take_changed`](NumberInput::take_changed) or write the CRDT.
    /// Mirrors [`TextInput::set_text`](crate::widget::TextInput::set_text).
    pub fn set_value(&mut self, v: f64) {
        self.value = v.clamp(self.min, self.max);
        self.sync_field();
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
        let n = if self.decimals == 0 {
            format!("{}", self.value.round() as i64)
        } else {
            format!("{:.*}", self.decimals, self.value)
        };
        if self.suffix.is_empty() {
            n
        } else {
            format!("{} {}", n, self.suffix)
        }
    }

    /// Overwrite the embedded field's text with the canonical value's
    /// formatted string.
    fn sync_field(&mut self) {
        let t = self.format_value();
        self.field.set_text(&t);
    }

    /// Commit whatever is typed: parse the leading number, clamp, adopt
    /// it. Records a change (and pending CRDT write) only when the value
    /// actually moved; always re-syncs the display so invalid/out-of-range
    /// text snaps back. Does not touch the CRDT itself (no `state` here)
    /// — the caller flushes via [`flush_crdt`](NumberInput::flush_crdt) or
    /// the next `sync`.
    fn commit_local(&mut self) {
        if let Some(parsed) = parse_leading_number(self.field.value()) {
            let clamped = parsed.clamp(self.min, self.max);
            if (clamped - self.value).abs() > f64::EPSILON {
                self.value = clamped;
                self.changed = Some(clamped);
                self.pending_write = Some(clamped);
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
            self.changed = Some(next);
            self.pending_write = Some(next);
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

    /// Flush a pending committed value to the bound CRDT field, if any.
    fn flush_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(v) = self.pending_write.take()
            && self.crdt_bound
        {
            let _ = state.put(
                root,
                self.on_change_path.as_str(),
                automerge::ScalarValue::F64(v),
            );
        }
    }
}

impl Widget for NumberInput {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Flush a value committed while we lacked the state handle (blur).
        self.flush_crdt(state, root);
        if !self.crdt_bound {
            return;
        }
        // Don't clobber the field while the user is mid-edit.
        if self.field.is_focused() {
            return;
        }
        if let Ok(Some((val, _))) = state.get(root, self.on_change_path.as_str())
            && let Value::Scalar(s) = val
        {
            let v = match s.as_ref() {
                automerge::ScalarValue::F64(f) => Some(*f),
                automerge::ScalarValue::Int(i) => Some(*i as f64),
                automerge::ScalarValue::Uint(u) => Some(*u as f64),
                _ => None,
            };
            if let Some(v) = v {
                let clamped = v.clamp(self.min, self.max);
                if (clamped - self.value).abs() > f64::EPSILON {
                    self.value = clamped;
                    self.sync_field();
                }
            }
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
                width: Dimension::length(self.width as f32),
                height: Dimension::length(self.height as f32),
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
        let radius = f64::from(theme.tokens.radii.md);
        let disabled = !self.enabled;
        if disabled {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &self.rect,
            );
        }

        // --- Stepper buttons ---
        for (btn_rect, glyph, hovered) in [
            (self.minus_rect, "\u{2212}", self.minus_hover), // − (minus sign)
            (self.plus_rect, "+", self.plus_hover),
        ] {
            let shape = RoundedRect::from_rect(btn_rect, radius);
            let bg = if hovered && !disabled {
                theme.bg_active
            } else {
                colors.surfaces.raised
            };
            scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.border_standard,
                None,
                &shape,
            );

            let (gw, _) = text_renderer.measure(glyph, STEP_GLYPH_SIZE, None);
            let line_height = f64::from(STEP_GLYPH_SIZE * 1.25);
            let gx = btn_rect.x0 + (btn_rect.width() - f64::from(gw)) / 2.0;
            let gy = btn_rect.y0 + (btn_rect.height() - line_height) / 2.0;
            let glyph_color = if disabled {
                theme.text_dim
            } else {
                theme.text_primary
            };
            text_renderer.draw_text(
                scene,
                glyph,
                STEP_GLYPH_SIZE,
                glyph_color,
                transform * Affine::translate((gx, gy)),
                None,
            );
        }

        // --- Editable field (embedded TextInput draws its own chrome) ---
        self.field
            .draw(scene, transform, text_renderer, theme, presence);

        if disabled {
            scene.pop_layer();
        }
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
        if !self.enabled {
            return;
        }

        // Forward to the embedded field first (click-to-focus, typing, IME).
        // The field isn't in the parent's node_map, so a focus request it
        // makes sets *our* node id — redirect it onto the field in set_focus.
        let prev_focus = *focus_target;
        self.field.handle_event(
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
        if *focus_target != prev_focus && self.field_rect.contains(self.last_cursor) {
            self.field_focus_pending = true;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let mh = self.minus_rect.contains(p);
                let ph = self.plus_rect.contains(p);
                if mh != self.minus_hover || ph != self.plus_hover {
                    self.minus_hover = mh;
                    self.plus_hover = ph;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: el,
                button: MouseButton::Left,
                ..
            } if *el == ElementState::Pressed => {
                if self.minus_rect.contains(self.last_cursor) {
                    // Commit any half-typed edit before stepping so the step
                    // is relative to what the user last saw, then blur the field.
                    if self.field.is_focused() {
                        self.commit_local();
                        self.field.set_focus(false);
                    }
                    self.step_by(-self.step);
                    self.flush_crdt(state, root);
                    *focus_target = Some(node_id);
                    *handled = true;
                } else if self.plus_rect.contains(self.last_cursor) {
                    if self.field.is_focused() {
                        self.commit_local();
                        self.field.set_focus(false);
                    }
                    self.step_by(self.step);
                    self.flush_crdt(state, root);
                    *focus_target = Some(node_id);
                    *handled = true;
                } else if !self.field_rect.contains(self.last_cursor) && self.field.is_focused() {
                    // Press landed outside the field while it was focused —
                    // commit the typed value and release focus.
                    self.commit_local();
                    self.flush_crdt(state, root);
                    self.field.set_focus(false);
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed && self.field.is_focused() => {
                match &key_event.logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.commit_local();
                        self.flush_crdt(state, root);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.commit_local();
                        self.step_by(self.step);
                        self.flush_crdt(state, root);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.commit_local();
                        self.step_by(-self.step);
                        self.flush_crdt(state, root);
                        *handled = true;
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
        if !self.enabled {
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
        self.enabled
    }
    fn is_enabled(&self) -> bool {
        self.enabled
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
                // Blur commits the typed value; the CRDT write is deferred to
                // the next `sync` (we have no `state` handle here).
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

    /// Reports [`Role::SpinButton`] with the formatted value; an assistive
    /// technology can nudge it with Increment / Decrement or jump it with
    /// `SetValue`, all through the same commit path as pointer/keyboard input.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::SpinButton)
                .with_value(self.format_value())
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.enabled,
                    ..Default::default()
                }),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        use crate::a11y::Action;
        if !self.enabled {
            return false;
        }
        match action {
            Action::Increment => {
                self.step_by(self.step);
                self.flush_crdt(state, root);
                true
            }
            Action::Decrement => {
                self.step_by(-self.step);
                self.flush_crdt(state, root);
                true
            }
            Action::SetValue(v) => {
                let Some(parsed) = parse_leading_number(v) else {
                    return false;
                };
                let clamped = parsed.clamp(self.min, self.max);
                if (clamped - self.value).abs() > f64::EPSILON {
                    self.value = clamped;
                    self.changed = Some(clamped);
                    self.pending_write = Some(clamped);
                    self.flush_crdt(state, root);
                }
                self.sync_field();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{KeyEvent, Modifiers};
    use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

    fn layout_at(n: &mut NumberInput, x0: f64, y0: f64) {
        // Mirror `apply_layout`'s arithmetic without a taffy pass.
        let x1 = x0 + n.width;
        let y1 = y0 + n.height;
        n.rect = Rect::new(x0, y0, x1, y1);
        let h = n.height;
        n.minus_rect = Rect::new(x0, y0, x0 + h, y1);
        n.plus_rect = Rect::new(x1 - h, y0, x1, y1);
        n.field_rect = Rect::new(n.minus_rect.x1 + GAP, y0, n.plus_rect.x0 - GAP, y1);
        n.field.apply_layout_external(n.field_rect);
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
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    fn cursor(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn named_key(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    fn parse_leading_number_tolerates_units_and_signs() {
        assert_eq!(parse_leading_number("16"), Some(16.0));
        assert_eq!(parse_leading_number("16 px"), Some(16.0));
        assert_eq!(parse_leading_number("16px"), Some(16.0));
        assert_eq!(parse_leading_number("  -3.5% "), Some(-3.5));
        assert_eq!(parse_leading_number("1.15"), Some(1.15));
        assert_eq!(parse_leading_number("px"), None);
        assert_eq!(parse_leading_number(""), None);
        assert_eq!(parse_leading_number("."), None);
    }

    #[test]
    fn format_value_respects_precision_and_suffix() {
        let n = NumberInput::new(16.0).with_suffix("px");
        assert_eq!(n.format_value(), "16 px");
        let n = NumberInput::new(1.153).with_precision(2);
        assert_eq!(n.format_value(), "1.15");
        let n = NumberInput::new(42.0);
        assert_eq!(n.format_value(), "42");
    }

    /// New clamps the initial value into range.
    #[test]
    fn new_and_with_range_clamp_initial_value() {
        let n = NumberInput::new(500.0).with_range(8.0, 72.0);
        assert_eq!(n.value(), 72.0, "over-max initial value clamps to max");
        let n = NumberInput::new(1.0).with_range(8.0, 72.0);
        assert_eq!(n.value(), 8.0, "under-min initial value clamps to min");
    }

    /// Typing a value and committing with Enter parses, clamps and reports
    /// the change through the production handle_event path.
    #[test]
    fn type_then_enter_parses_clamps_and_commits() {
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

    /// A valid in-range value commits verbatim; a unit suffix in the typed
    /// text is tolerated by the parser.
    #[test]
    fn type_with_unit_suffix_commits_number() {
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

    /// Invalid text reverts to the last good value with no reported change.
    #[test]
    fn invalid_text_reverts_and_reports_nothing() {
        let mut n = NumberInput::new(16.0).with_range(8.0, 72.0);
        layout_at(&mut n, 0.0, 0.0);
        n.field.set_focus(true);
        n.field.set_text("abc");
        dispatch(&mut n, &named_key(NamedKey::Enter));
        assert_eq!(n.value(), 16.0, "unparseable text keeps the old value");
        assert_eq!(n.field.value(), "16", "display snapped back to the value");
        assert_eq!(n.take_changed(), None, "no change to report");
    }

    /// Clicking the + / − steppers moves by `step`, clamped, and reports it.
    #[test]
    fn steppers_increment_and_clamp() {
        let mut n = NumberInput::new(70.0).with_range(8.0, 72.0).with_step(2.0);
        layout_at(&mut n, 0.0, 0.0);
        let plus_c = (
            (n.plus_rect.x0 + n.plus_rect.x1) / 2.0,
            (n.plus_rect.y0 + n.plus_rect.y1) / 2.0,
        );
        let minus_c = (
            (n.minus_rect.x0 + n.minus_rect.x1) / 2.0,
            (n.minus_rect.y0 + n.minus_rect.y1) / 2.0,
        );

        // Click + once: 70 → 72.
        dispatch(&mut n, &cursor(plus_c.0, plus_c.1));
        dispatch(&mut n, &press());
        assert_eq!(n.value(), 72.0);
        assert_eq!(n.take_changed(), Some(72.0));

        // Click + again: already at max → no move, no report.
        dispatch(&mut n, &press());
        assert_eq!(n.value(), 72.0, "clamped at max");
        assert_eq!(n.take_changed(), None, "no change when already clamped");

        // Click − : 72 → 70.
        dispatch(&mut n, &cursor(minus_c.0, minus_c.1));
        dispatch(&mut n, &press());
        assert_eq!(n.value(), 70.0);
        assert_eq!(n.take_changed(), Some(70.0));
    }

    /// Arrow Up / Down while editing step the value.
    #[test]
    fn arrow_keys_step_while_editing() {
        let mut n = NumberInput::new(16.0).with_range(1.0, 100.0).with_step(1.0);
        layout_at(&mut n, 0.0, 0.0);
        n.field.set_focus(true);
        dispatch(&mut n, &named_key(NamedKey::ArrowUp));
        assert_eq!(n.value(), 17.0);
        dispatch(&mut n, &named_key(NamedKey::ArrowDown));
        dispatch(&mut n, &named_key(NamedKey::ArrowDown));
        assert_eq!(n.value(), 15.0);
    }

    /// Blur (focus lost) commits the typed value; the CRDT write is deferred
    /// and flushed on the next `sync`.
    #[test]
    fn blur_commits_and_flushes_to_crdt_on_sync() {
        let mut n = NumberInput::new(16.0)
            .with_range(8.0, 72.0)
            .with_on_change_path("font_size");
        layout_at(&mut n, 0.0, 0.0);
        n.field.set_focus(true);
        n.field.set_text("40");

        // App moves focus elsewhere → set_focus(false) commits locally.
        Widget::set_focus(&mut n, false);
        assert_eq!(n.value(), 40.0, "blur committed the typed value");
        assert_eq!(n.take_changed(), Some(40.0));

        // The CRDT write was deferred; sync flushes it.
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        n.sync(&mut doc, &root);
        let stored = doc.get(&root, "font_size").unwrap().unwrap().0;
        match stored {
            Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::F64(f) => assert_eq!(*f, 40.0),
                other => panic!("expected F64, got {other:?}"),
            },
            other => panic!("expected scalar, got {other:?}"),
        }
    }

    /// A CRDT-bound field reads its value back on `sync` (the Slider pattern).
    #[test]
    fn sync_reads_value_from_crdt() {
        let mut n = NumberInput::new(16.0)
            .with_range(0.0, 100.0)
            .with_on_change_path("opacity_pct");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, "opacity_pct", automerge::ScalarValue::F64(80.0));
        n.sync(&mut doc, &root);
        assert_eq!(n.value(), 80.0);
        assert_eq!(n.field.value(), "80");
    }

    /// `set_value` is programmatic: it clamps + updates the display but does
    /// NOT report a user change.
    #[test]
    fn set_value_is_silent() {
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

    /// Keyboard (Tab) focus routes into the editable field so it's typeable
    /// without a mouse — arrows then step it.
    #[test]
    fn keyboard_focus_makes_field_editable() {
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

    #[test]
    fn accessibility_reports_spinbutton_and_value() {
        use crate::a11y::Role;
        let n = NumberInput::new(16.0).with_suffix("px");
        let node = Widget::accessibility(&n).unwrap();
        assert_eq!(node.role, Role::SpinButton);
        assert_eq!(node.value.as_deref(), Some("16 px"));
        assert!(node.state.focusable);
    }

    #[test]
    fn perform_action_increment_and_setvalue() {
        use crate::a11y::Action;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut n = NumberInput::new(16.0).with_range(8.0, 72.0).with_step(4.0);
        assert!(Widget::perform_action(
            &mut n,
            &Action::Increment,
            &mut doc,
            &root
        ));
        assert_eq!(n.value(), 20.0);
        assert!(Widget::perform_action(
            &mut n,
            &Action::SetValue("100".into()),
            &mut doc,
            &root
        ));
        assert_eq!(n.value(), 72.0, "SetValue clamps");
    }

    #[test]
    fn disabled_field_rejects_input() {
        let mut n = NumberInput::new(16.0)
            .with_range(8.0, 72.0)
            .with_enabled(false);
        layout_at(&mut n, 0.0, 0.0);
        let before = n.value();
        let plus_c = (
            (n.plus_rect.x0 + n.plus_rect.x1) / 2.0,
            (n.plus_rect.y0 + n.plus_rect.y1) / 2.0,
        );
        dispatch(&mut n, &cursor(plus_c.0, plus_c.1));
        dispatch(&mut n, &press());
        assert_eq!(n.value(), before, "disabled field ignores stepper clicks");
        assert!(!Widget::is_focusable(&n));
    }
}
