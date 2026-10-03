//! `TimePicker` — three side-by-side vertical spinners for `HH : MM : SS`.
//!
//! Each spinner is a column of three regions: an up-arrow, the two-digit
//! value cell, and a down-arrow. Clicking an arrow steps the value by 1
//! with wrap-around (hours 0..=23, minutes/seconds 0..=59); scrolling the
//! wheel over a value cell does the same.
//!
//! The seconds spinner is hidden unless the value contains seconds. The
//! selected time can stay widget-local, bind to typed process-local state with
//! [`TimePicker::from_state`], or be observed with `on_change` for an
//! app-owned durable mutation.

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Affine composes two 2D transforms via f64 \
              matrix math; it never panics or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Maximum hour value (inclusive). 24-hour clock — wraps 23 → 0.
const HOUR_MAX: u8 = 23;
/// Maximum minute/second value (inclusive). Wraps 59 → 0.
const MINSEC_MAX: u8 = 59;

/// Fixed width of one spinner cell. Picked so the two-digit value reads
/// comfortably at the `lg` type-scale (16 px) without crowding the arrows.
const SPINNER_WIDTH: f32 = 44.0;
/// Fixed height of the picker. Three vertical regions of equal height
/// (arrow, value, arrow) of 24 px each — the value cell ends up matching the
/// `lg` text height with some breathing room.
const PICKER_HEIGHT: f32 = 132.0;
/// Width of the colon separator between spinners.
const COLON_WIDTH: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeCell {
    Hours,
    Minutes,
    Seconds,
}

/// Typed value shared by a [`TimePicker`] and application state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeValue {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: Option<u8>,
}

impl TimeValue {
    #[must_use]
    pub const fn new(hours: u8, minutes: u8) -> Self {
        Self {
            hours,
            minutes,
            seconds: None,
        }
    }

    #[must_use]
    pub const fn with_seconds(mut self, seconds: u8) -> Self {
        self.seconds = Some(seconds);
        self
    }

    fn normalized(self) -> Self {
        Self {
            hours: self.hours.min(HOUR_MAX),
            minutes: self.minutes.min(MINSEC_MAX),
            seconds: self.seconds.map(|seconds| seconds.min(MINSEC_MAX)),
        }
    }
}

/// `(hours, minutes, seconds)`.
type TimeChange = Box<dyn FnMut(u8, u8, Option<u8>)>;

/// Three-spinner time picker. See module docs.
pub struct TimePicker {
    /// Stable semantic/action id. Callers rendering more than one picker in a
    /// retained tree set this explicitly with [`Self::with_id`].
    id: String,
    /// Fluent message id spoken for the complete time control.
    accessible_name: Option<String>,
    hours: u8,           // 0..=23
    minutes: u8,         // 0..=59
    seconds: Option<u8>, // Some(0..=59) only when explicitly enabled
    on_change: Option<TimeChange>,
    local_state: Option<crate::local_state::LocalBinding<TimeValue>>,
    rect: Rect,
    hour_rect: Rect, // populated in apply_layout
    minute_rect: Rect,
    second_rect: Rect, // ZERO when seconds == None
    last_cursor: Point,
    is_focused: bool,
    /// Which cell consumes the next digit key. Set by clicking a
    /// value cell or by Tab cycling. `None` while no cell is in
    /// edit mode — digit keys then fall through to the rest of the
    /// tree as usual.
    focused_cell: Option<TimeCell>,
    /// Per-cell digit accumulator. Two characters max — the third
    /// digit closes the cell and starts a fresh sequence on the next
    /// cell. Cleared when a cell is entered or when Esc is pressed.
    digit_buffer: String,
}

impl TimePicker {
    /// Construct a new picker with the given hour/minute values. Both
    /// arguments are clamped to their valid ranges (hours 0..=23, minutes
    /// 0..=59) so callers can't smuggle out-of-band values past the
    /// constructor.
    pub fn new(hours: u8, minutes: u8) -> Self {
        Self {
            id: "time_picker".into(),
            accessible_name: None,
            hours: hours.min(HOUR_MAX),
            minutes: minutes.min(MINSEC_MAX),
            seconds: None,
            on_change: None,
            local_state: None,
            rect: Rect::ZERO,
            hour_rect: Rect::ZERO,
            minute_rect: Rect::ZERO,
            second_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            is_focused: false,
            focused_cell: None,
            digit_buffer: String::new(),
        }
    }

    /// Create a time picker bound to typed process-local state.
    ///
    /// Pointer, keyboard, wheel, and semantic changes update `state`;
    /// mutations through any clone update this picker on the next
    /// synchronization pass.
    pub fn from_state(state: &crate::LocalState<TimeValue>) -> Self {
        let (value, binding) = crate::local_state::LocalBinding::new(state);
        let value = value.normalized();
        let mut picker = Self::new(value.hours, value.minutes);
        picker.seconds = value.seconds;
        picker.local_state = Some(binding);
        picker.publish_local_state();
        picker
    }

    /// Which cell currently consumes digit keys, if any. Mostly for
    /// tests; the chrome reads this for visual focus highlighting.
    pub fn focused_cell(&self) -> Option<TimeCell> {
        self.focused_cell
    }

    /// Name the time picker for assistive technology.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Set the stable semantic/action id used by accessibility, search and
    /// direct agent invocation.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Current per-cell digit accumulator (visible while typing).
    pub fn digit_buffer(&self) -> &str {
        &self.digit_buffer
    }

    /// Append a single digit to the focused cell's accumulator and commit
    /// it. First digit replaces the cell value (typing "9" into "23" gives
    /// "09"). Second digit appends if still in range, else replaces the
    /// buffer with just the new digit ("2" then "5" gives "5", not "25").
    /// After two valid digits, focus auto-advances to the next cell.
    fn accept_digit(&mut self, d: u8) {
        let Some(cell) = self.focused_cell else {
            return;
        };
        let max = match cell {
            TimeCell::Hours => HOUR_MAX,
            TimeCell::Minutes | TimeCell::Seconds => MINSEC_MAX,
        };
        if self.digit_buffer.is_empty() || self.digit_buffer.len() >= 2 {
            self.digit_buffer.clear();
        }
        let mut candidate = self.digit_buffer.clone();
        candidate.push(char::from_digit(u32::from(d), 10).unwrap_or('0'));
        let parsed: u8 = candidate.parse().unwrap_or(0);
        if parsed > max {
            self.digit_buffer = char::from_digit(u32::from(d), 10)
                .unwrap_or('0')
                .to_string();
        } else {
            self.digit_buffer = candidate;
        }
        let v: u8 = self.digit_buffer.parse().unwrap_or(0);
        match cell {
            TimeCell::Hours => self.hours = v,
            TimeCell::Minutes => self.minutes = v,
            TimeCell::Seconds => self.seconds = Some(v),
        }
        self.fire_on_change();
        if self.digit_buffer.len() == 2 {
            self.advance_focus();
        }
    }

    /// Move focus to the next cell (Tab semantic). Wraps off the
    /// trailing edge to `None` so the user can step out of the
    /// picker by tabbing past its last cell.
    fn advance_focus(&mut self) {
        self.focused_cell = match self.focused_cell {
            Some(TimeCell::Hours) => Some(TimeCell::Minutes),
            Some(TimeCell::Minutes) => {
                if self.seconds.is_some() {
                    Some(TimeCell::Seconds)
                } else {
                    None
                }
            }
            Some(TimeCell::Seconds) | None => None,
        };
        self.digit_buffer.clear();
    }

    /// Move focus to the previous cell (Shift+Tab semantic).
    fn retreat_focus(&mut self) {
        self.focused_cell = match self.focused_cell {
            Some(TimeCell::Seconds) => Some(TimeCell::Minutes),
            Some(TimeCell::Minutes) => Some(TimeCell::Hours),
            _ => None,
        };
        self.digit_buffer.clear();
    }

    fn cell_for_point(&self, p: Point) -> Option<TimeCell> {
        if self.hour_rect.contains(p) {
            return Some(TimeCell::Hours);
        }
        if self.minute_rect.contains(p) {
            return Some(TimeCell::Minutes);
        }
        if self.seconds.is_some() && self.second_rect.contains(p) {
            return Some(TimeCell::Seconds);
        }
        None
    }

    fn cell_value_rect(&self, cell: TimeCell) -> Rect {
        let outer = match cell {
            TimeCell::Hours => self.hour_rect,
            TimeCell::Minutes => self.minute_rect,
            TimeCell::Seconds => self.second_rect,
        };
        let (_, value, _) = Self::spinner_subrects(outer);
        value
    }

    /// Enable the seconds spinner with the given initial value (clamped to
    /// 0..=59). Without this builder call the seconds field stays `None` and
    /// the third spinner is hidden.
    #[must_use]
    pub fn with_seconds(mut self, seconds: u8) -> Self {
        self.seconds = Some(seconds.min(MINSEC_MAX));
        self.publish_local_state();
        self
    }

    pub fn hours(&self) -> u8 {
        self.hours
    }
    pub fn minutes(&self) -> u8 {
        self.minutes
    }
    pub fn seconds(&self) -> Option<u8> {
        self.seconds
    }

    pub fn value(&self) -> TimeValue {
        TimeValue {
            hours: self.hours,
            minutes: self.minutes,
            seconds: self.seconds,
        }
    }

    /// Install a callback invoked with `(hours, minutes, seconds)` every
    /// time a user-driven change commits a new value.
    #[must_use]
    pub fn on_change<F: FnMut(u8, u8, Option<u8>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    pub fn set_hours(&mut self, h: u8) {
        self.hours = h.min(HOUR_MAX);
        self.publish_local_state();
    }

    pub fn set_minutes(&mut self, m: u8) {
        self.minutes = m.min(MINSEC_MAX);
        self.publish_local_state();
    }

    pub fn set_seconds(&mut self, seconds: Option<u8>) {
        self.seconds = seconds.map(|seconds| seconds.min(MINSEC_MAX));
        self.publish_local_state();
    }

    pub fn set_value(&mut self, value: TimeValue) {
        self.apply_value(value.normalized());
        self.publish_local_state();
    }

    /// Place the picker at an explicit rect without going through taffy — the
    /// embed path a host uses when it hand-positions the spinner row inside a
    /// form. Rebuilds the per-spinner hit-rects from `rect` exactly as
    /// [`Widget::apply_layout`] does, so click/scroll/keyboard all work.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        let cell_w = f64::from(SPINNER_WIDTH);
        let colon_w = f64::from(COLON_WIDTH);
        let y0 = self.rect.y0;
        let y1 = self.rect.y1;
        let mut x = self.rect.x0;
        self.hour_rect = Rect::new(x, y0, x + cell_w, y1);
        x += cell_w + colon_w;
        self.minute_rect = Rect::new(x, y0, x + cell_w, y1);
        if self.seconds.is_some() {
            x += cell_w + colon_w;
            self.second_rect = Rect::new(x, y0, x + cell_w, y1);
        } else {
            self.second_rect = Rect::ZERO;
        }
    }

    /// Intrinsic size of the spinner row at the current seconds setting —
    /// the width/height an embedding host should allocate for
    /// [`Self::apply_layout_external`].
    pub fn intrinsic_size(&self) -> (f64, f64) {
        let n = if self.seconds.is_some() { 3.0 } else { 2.0 };
        let w = n * f64::from(SPINNER_WIDTH) + (n - 1.0) * f64::from(COLON_WIDTH);
        (w, f64::from(PICKER_HEIGHT))
    }

    /// Compute the three sub-rects (up arrow / value cell / down arrow) for
    /// a spinner cell. Returned in `(up, value, down)` order.
    fn spinner_subrects(cell: Rect) -> (Rect, Rect, Rect) {
        let third = cell.height() / 3.0;
        let up = Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + third);
        let value = Rect::new(cell.x0, cell.y0 + third, cell.x1, cell.y1 - third);
        let down = Rect::new(cell.x0, cell.y1 - third, cell.x1, cell.y1);
        (up, value, down)
    }

    fn wrap_inc(v: u8, max: u8) -> u8 {
        if v >= max { 0 } else { v.saturating_add(1) }
    }

    fn wrap_dec(v: u8, max: u8) -> u8 {
        if v == 0 { max } else { v.saturating_sub(1) }
    }

    fn fire_on_change(&mut self) {
        self.publish_local_state();
        if let Some(cb) = self.on_change.as_mut() {
            cb(self.hours, self.minutes, self.seconds);
        }
    }

    fn apply_value(&mut self, value: TimeValue) {
        self.hours = value.hours;
        self.minutes = value.minutes;
        self.seconds = value.seconds;
    }

    fn publish_local_state(&mut self) {
        let value = self.value();
        if let Some(binding) = &mut self.local_state {
            binding.publish(&value);
        }
    }

    fn step_hours(&mut self, delta: i8) {
        if delta > 0 {
            self.hours = Self::wrap_inc(self.hours, HOUR_MAX);
        } else if delta < 0 {
            self.hours = Self::wrap_dec(self.hours, HOUR_MAX);
        }
        self.fire_on_change();
    }

    fn step_minutes(&mut self, delta: i8) {
        if delta > 0 {
            self.minutes = Self::wrap_inc(self.minutes, MINSEC_MAX);
        } else if delta < 0 {
            self.minutes = Self::wrap_dec(self.minutes, MINSEC_MAX);
        }
        self.fire_on_change();
    }

    /// No-op if seconds disabled.
    fn step_seconds(&mut self, delta: i8) {
        let Some(cur) = self.seconds else { return };
        let new = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => Self::wrap_inc(cur, MINSEC_MAX),
            std::cmp::Ordering::Less => Self::wrap_dec(cur, MINSEC_MAX),
            std::cmp::Ordering::Equal => cur,
        };
        self.seconds = Some(new);
        self.fire_on_change();
    }

    fn draw_chevron(
        scene: &mut Scene,
        transform: Affine,
        cell: Rect,
        color: vello::peniko::Color,
        up: bool,
    ) {
        // Triangle inscribed in the central ~50% of the cell, keeping the
        // hit area larger than the visible arrow.
        let cx = cell.center().x;
        let cy = cell.center().y;
        let w = cell.width() * 0.35;
        let h = cell.height() * 0.30;
        let mut path = BezPath::new();
        if up {
            path.move_to((cx - w, cy + h / 2.0));
            path.line_to((cx + w, cy + h / 2.0));
            path.line_to((cx, cy - h / 2.0));
        } else {
            path.move_to((cx - w, cy - h / 2.0));
            path.line_to((cx + w, cy - h / 2.0));
            path.line_to((cx, cy + h / 2.0));
        }
        path.close_path();
        scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &path);
    }
}

impl Widget for TimePicker {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        if let Some(value) = self
            .local_state
            .as_mut()
            .and_then(crate::local_state::LocalBinding::poll)
        {
            self.apply_value(value.normalized());
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Total width = spinner + colon + spinner [+ colon + spinner]
        let n_spinners: f32 = if self.seconds.is_some() { 3.0 } else { 2.0 };
        let total_w = n_spinners * SPINNER_WIDTH + (n_spinners - 1.0) * COLON_WIDTH;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(total_w),
                height: Dimension::length(PICKER_HEIGHT),
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
        self.apply_layout_external(Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        ));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.detail);
        let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.lg);

        let mut cells: Vec<(Rect, u8)> = vec![
            (self.hour_rect, self.hours),
            (self.minute_rect, self.minutes),
        ];
        if let Some(s) = self.seconds {
            cells.push((self.second_rect, s));
        }

        for (cell_idx, (cell, value)) in cells.iter().enumerate() {
            let (up_rect, value_rect, down_rect) = Self::spinner_subrects(*cell);

            let bg = RoundedRect::from_rect(value_rect, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &bg,
            );
            let cell_kind = match cell_idx {
                0 => TimeCell::Hours,
                1 => TimeCell::Minutes,
                _ => TimeCell::Seconds,
            };
            // Each value cell is a Control: a rest edge derived from its
            // surface (HIG §9.5); the focused cell keeps `border_focus`.
            let outline = crate::surface::edge_outline(value_rect, radius);
            let (width, color) = if self.is_focused && self.focused_cell == Some(cell_kind) {
                (2.0, colors.border_focus)
            } else {
                (1.0, crate::surface::edge_on(colors.surfaces.raised))
            };
            scene.stroke(&Stroke::new(width), transform, color, None, &outline);

            let label = format!("{value:02}");
            let (w, h) = text_renderer.measure(&label, text_size, None);
            let tx = value_rect.x0 + (value_rect.width() - f64::from(w)) / 2.0;
            let ty = value_rect.y0 + (value_rect.height() - f64::from(h)) / 2.0;
            text_renderer.draw_text(
                scene,
                &label,
                text_size,
                colors.text_primary,
                translated(transform, tx, ty),
                None,
            );

            Self::draw_chevron(scene, transform, up_rect, colors.text_dim, true);
            Self::draw_chevron(scene, transform, down_rect, colors.text_dim, false);
        }

        // Colons at the same point size as the value text so baselines line up.
        let between_h_m_x = f64::midpoint(self.hour_rect.x1, self.minute_rect.x0);
        let centre_y = self.rect.center().y;
        let colon = ":";
        let (cw, ch) = text_renderer.measure(colon, text_size, None);
        text_renderer.draw_text(
            scene,
            colon,
            text_size,
            colors.text_dim,
            translated(
                transform,
                between_h_m_x - f64::from(cw) / 2.0,
                centre_y - f64::from(ch) / 2.0,
            ),
            None,
        );
        if self.seconds.is_some() {
            let between_m_s_x = f64::midpoint(self.minute_rect.x1, self.second_rect.x0);
            text_renderer.draw_text(
                scene,
                colon,
                text_size,
                colors.text_dim,
                translated(
                    transform,
                    between_m_s_x - f64::from(cw) / 2.0,
                    centre_y - f64::from(ch) / 2.0,
                ),
                None,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_mouse_press(ctx.handled, ctx.focus_target, ctx.node_id);
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_keyboard_input(event, *ctx.modifiers, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(*delta, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "TimePicker"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        if !focused {
            self.focused_cell = None;
            self.digit_buffer.clear();
        } else if self.focused_cell.is_none() {
            // Keyboard-focus entry without a click — default to hours.
            self.focused_cell = Some(TimeCell::Hours);
        }
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// The time picker reports `Role::ComboBox` — a selector whose value
    /// is the chosen time as `HH:MM` (or `HH:MM:SS` when the seconds spinner
    /// is enabled). The spinners render inline, so `expanded` is left unset.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let value = match self.seconds {
            Some(s) => format!("{:02}:{:02}:{:02}", self.hours, self.minutes, s),
            None => format!("{:02}:{:02}", self.hours, self.minutes),
        };
        Some(
            AccessNode::new(Role::ComboBox)
                .with_semantic_key(&self.id)
                .with_action_target(&self.id)
                .with_optional_name(self.accessible_name.as_ref().map(|name| crate::i18n!(name)))
                .with_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    ..Default::default()
                }),
        )
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
        let mut fields = value.trim().split(':');
        let (Some(hours), Some(minutes)) = (fields.next(), fields.next()) else {
            return false;
        };
        let Ok(hours) = hours.parse::<u8>() else {
            return false;
        };
        let Ok(minutes) = minutes.parse::<u8>() else {
            return false;
        };
        let seconds = match fields.next() {
            Some(seconds) => match seconds.parse::<u8>() {
                Ok(seconds) => Some(seconds),
                Err(_) => return false,
            },
            None => None,
        };
        if fields.next().is_some()
            || hours > HOUR_MAX
            || minutes > MINSEC_MAX
            || seconds.is_some_and(|seconds| seconds > MINSEC_MAX)
        {
            return false;
        }
        self.apply_value(TimeValue {
            hours,
            minutes,
            seconds,
        });
        self.fire_on_change();
        true
    }
}

impl crate::widget::controls::ActionControls for TimePicker {
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        vec![crate::widget::ControlBindingMetadata::semantic(
            self.id.clone(),
        )]
    }
}

impl TimePicker {
    /// A click on a value cell latches keyboard focus there so digit keys
    /// flow through `accept_digit`; otherwise a click on an up/down
    /// chevron steps that spinner.
    fn handle_mouse_press(
        &mut self,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let p = self.last_cursor;
        if let Some(cell) = self.cell_for_point(p) {
            let value_rect = self.cell_value_rect(cell);
            if value_rect.contains(p) {
                if !self.is_focused {
                    *focus_target = Some(node_id);
                }
                if self.focused_cell != Some(cell) {
                    self.focused_cell = Some(cell);
                    self.digit_buffer.clear();
                }
                *handled = true;
                return;
            }
        }
        // Otherwise: up/down chevron handling.
        let mut cells: Vec<(Rect, u8)> = vec![(self.hour_rect, 0), (self.minute_rect, 1)];
        if self.seconds.is_some() {
            cells.push((self.second_rect, 2));
        }
        for (cell, which) in &cells {
            if !cell.contains(p) {
                continue;
            }
            let (up_rect, _value_rect, down_rect) = Self::spinner_subrects(*cell);
            let delta: i8 = if up_rect.contains(p) {
                1
            } else if down_rect.contains(p) {
                -1
            } else {
                0
            };
            if delta != 0 {
                match which {
                    0 => self.step_hours(delta),
                    1 => self.step_minutes(delta),
                    2 => self.step_seconds(delta),
                    _ => {}
                }
                *handled = true;
            }
            break;
        }
    }

    /// Tab/Shift+Tab cycle cells, Escape clears the edit buffer, arrows
    /// step or move focus, Enter commits and advances, and any other key
    /// falls through to digit entry on the focused cell.
    fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: element_state,
                    text,
                    ..
                },
            ..
        } = event
        else {
            return;
        };
        if !self.is_focused || *element_state != ElementState::Pressed {
            return;
        }
        let shift = modifiers.state().shift_key();
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                if shift {
                    self.retreat_focus();
                } else {
                    self.advance_focus();
                }
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.focused_cell = None;
                self.digit_buffer.clear();
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                // An empty buffer resets the cell value to zero.
                if let Some(cell) = self.focused_cell {
                    self.digit_buffer.pop();
                    let v: u8 = self.digit_buffer.parse().unwrap_or(0);
                    match cell {
                        TimeCell::Hours => self.hours = v,
                        TimeCell::Minutes => self.minutes = v,
                        TimeCell::Seconds => self.seconds = Some(v),
                    }
                    self.fire_on_change();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(cell) = self.focused_cell {
                    self.digit_buffer.clear();
                    match cell {
                        TimeCell::Hours => self.step_hours(1),
                        TimeCell::Minutes => self.step_minutes(1),
                        TimeCell::Seconds => self.step_seconds(1),
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(cell) = self.focused_cell {
                    self.digit_buffer.clear();
                    match cell {
                        TimeCell::Hours => self.step_hours(-1),
                        TimeCell::Minutes => self.step_minutes(-1),
                        TimeCell::Seconds => self.step_seconds(-1),
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.retreat_focus();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.advance_focus();
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.digit_buffer.clear();
                self.advance_focus();
                *handled = true;
            }
            _ => {
                // Deliberately the logical-key character path, not
                // named keys, so numpad digits also work.
                if let Some(t) = text {
                    for c in t.chars() {
                        if let Some(d) = c.to_digit(10) {
                            self.accept_digit(u8::try_from(d).unwrap_or(0));
                            *handled = true;
                        }
                    }
                }
            }
        }
    }

    /// Wheel up increments, matching native OS time pickers.
    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta, handled: &mut bool) {
        let p = self.last_cursor;
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y),
            MouseScrollDelta::PixelDelta(pp) => pp.y,
        };
        let step: i8 = if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        };
        if step == 0 {
            return;
        }

        let mut cells: Vec<(Rect, u8)> = vec![(self.hour_rect, 0), (self.minute_rect, 1)];
        if self.seconds.is_some() {
            cells.push((self.second_rect, 2));
        }
        for (cell, which) in &cells {
            let (_up_rect, value_rect, _down_rect) = Self::spinner_subrects(*cell);
            if value_rect.contains(p) {
                match which {
                    0 => self.step_hours(step),
                    1 => self.step_minutes(step),
                    2 => self.step_seconds(step),
                    _ => {}
                }
                *handled = true;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn wheel(dy: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: MouseScrollDelta::LineDelta(0.0, dy),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    /// Drive an event through the picker with default scaffolding.
    fn dispatch(tp: &mut TimePicker, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        tp.handle_event(
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

    /// Populates `hour_rect`/`minute_rect`/`second_rect` by running
    /// `build_layout` + `apply_layout` through a real taffy tree.
    fn picker_with_layout(tp: &mut TimePicker) {
        use taffy::prelude::TaffyMaxContent;
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = tp.build_layout(&mut taffy, &crate::theme::Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        tp.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn time_values_clamp_and_are_announced() {
        // Case `accessibility_reports_combobox_role_and_hhmm_value`.
        {
            use crate::a11y::Role;
            let tp = TimePicker::new(9, 5);
            let node = Widget::accessibility(&tp).expect("time picker is a selector");
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(node.value.as_deref(), Some("09:05"));
            assert!(node.state.focusable);
            assert!(!node.state.focused);
            assert_eq!(node.state.expanded, None, "spinners render inline");
        }

        // Case `accessibility_value_includes_seconds_when_enabled`.
        {
            let mut tp = TimePicker::new(9, 5).with_seconds(7);
            Widget::set_focus(&mut tp, true);
            let node = Widget::accessibility(&tp).unwrap();
            assert_eq!(node.value.as_deref(), Some("09:05:07"));
            assert!(node.state.focused);
        }

        // Case `time_picker_clamps_hours_to_23`.
        {
            let tp = TimePicker::new(25, 30);
            assert_eq!(tp.hours(), 23);
            let mut tp = tp;
            tp.set_hours(99);
            assert_eq!(tp.hours(), 23);
        }

        // Case `time_picker_clamps_minutes_to_59`.
        {
            let tp = TimePicker::new(12, 99);
            assert_eq!(tp.minutes(), 59);
        }
    }

    #[test]
    fn time_cells_take_focus_digits_and_scroll() {
        // Case `time_picker_scroll_up_increments_minutes`.
        {
            let mut tp = TimePicker::new(10, 30);
            picker_with_layout(&mut tp);
            assert!(
                tp.minute_rect.width() > 0.0,
                "minute_rect must be populated by apply_layout"
            );

            let centre = tp.minute_rect.center();
            dispatch(&mut tp, &cursor_moved(centre.x, centre.y));
            dispatch(&mut tp, &wheel(1.0));

            assert_eq!(
                tp.minutes(),
                31,
                "wheel up over minute cell must increment minutes"
            );
        }

        // Case `time_picker_click_on_hour_value_cell_focuses_hours`.
        // Clicking on the hour value cell must latch keyboard focus on
        // hours so subsequent digit keys edit the hour value (winit's
        // `KeyEvent` can't be constructed in tests, so we exercise the
        // click path here and the digit-acceptance path below directly).
        {
            let mut tp = TimePicker::new(10, 30);
            picker_with_layout(&mut tp);
            Widget::set_focus(&mut tp, true);

            let centre = tp.hour_rect.center();
            dispatch(&mut tp, &cursor_moved(centre.x, centre.y));
            dispatch(
                &mut tp,
                &WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));
        }

        // Case `time_picker_two_digit_input_sets_hours_then_advances`.
        // Typing two digits into the focused hour cell sets the hour
        // directly (replaces, not appends). Auto-advances to minutes
        // after the second digit lands.
        {
            let mut tp = TimePicker::new(0, 0);
            Widget::set_focus(&mut tp, true);
            tp.focused_cell = Some(TimeCell::Hours);

            tp.accept_digit(1);
            assert_eq!(tp.hours(), 1);
            assert_eq!(tp.digit_buffer(), "1");
            assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));

            tp.accept_digit(4);
            assert_eq!(tp.hours(), 14);
            assert_eq!(tp.focused_cell(), Some(TimeCell::Minutes));
        }

        // Case `time_picker_two_digit_overflow_resets_to_new_digit`.
        // Typing a second digit that would overflow the max drops the
        // prior digit and accepts the new one as a fresh first
        // character — "2" → "5" leaves hours = 5, not 25.
        {
            let mut tp = TimePicker::new(0, 0);
            Widget::set_focus(&mut tp, true);
            tp.focused_cell = Some(TimeCell::Hours);

            tp.accept_digit(2);
            assert_eq!(tp.hours(), 2);
            tp.accept_digit(5);
            assert_eq!(tp.hours(), 5);
            assert_eq!(tp.digit_buffer(), "5");
        }

        // Case `time_picker_tab_cycles_cells`.
        {
            let mut tp = TimePicker::new(12, 30).with_seconds(45);
            Widget::set_focus(&mut tp, true);
            assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));

            tp.advance_focus();
            assert_eq!(tp.focused_cell(), Some(TimeCell::Minutes));
            tp.advance_focus();
            assert_eq!(tp.focused_cell(), Some(TimeCell::Seconds));
            tp.advance_focus();
            assert_eq!(tp.focused_cell(), None);
        }

        // Case `time_picker_retreat_focus_walks_back`.
        // Shift+Tab equivalent: walking back through the cells.
        {
            let mut tp = TimePicker::new(12, 30).with_seconds(45);
            Widget::set_focus(&mut tp, true);
            tp.focused_cell = Some(TimeCell::Seconds);

            tp.retreat_focus();
            assert_eq!(tp.focused_cell(), Some(TimeCell::Minutes));
            tp.retreat_focus();
            assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));
        }
    }

    #[test]
    fn time_changes_sync_bound_state_and_callbacks() {
        // Case `local_state_keeps_time_pickers_in_sync`.
        {
            let state = crate::LocalState::new(TimeValue::new(8, 30));
            let mut left = TimePicker::from_state(&state);
            let mut right = TimePicker::from_state(&state);
            let expected = TimeValue::new(16, 45).with_seconds(12);
            let mut doc = AutoCommit::new();

            left.set_value(expected);
            assert_eq!(state.with(|value| *value), expected);
            Widget::sync(&mut right, &mut doc, &automerge::ROOT);
            assert_eq!(right.value(), expected);
        }

        // Case `semantic_time_change_updates_state_and_callback`.
        {
            use std::cell::RefCell;
            use std::rc::Rc;

            let state = crate::LocalState::new(TimeValue::new(8, 30));
            let captured = Rc::new(RefCell::new(None));
            let callback = Rc::clone(&captured);
            let mut picker = TimePicker::from_state(&state).on_change(move |h, m, s| {
                *callback.borrow_mut() = Some(TimeValue {
                    hours: h,
                    minutes: m,
                    seconds: s,
                });
            });
            let mut doc = AutoCommit::new();
            let expected = TimeValue::new(21, 7).with_seconds(9);

            assert!(Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("21:07:09".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|value| *value), expected);
            assert_eq!(*captured.borrow(), Some(expected));

            assert!(!Widget::perform_action(
                &mut picker,
                &crate::a11y::Action::SetValue("24:00".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(state.with(|value| *value), expected);
        }
    }
}
