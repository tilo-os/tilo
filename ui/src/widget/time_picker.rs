//! TimePicker — three side-by-side vertical spinners for `HH : MM : SS`.
//!
//! Each spinner is a small column of three regions: an up-arrow button, the
//! two-digit value cell (background `surfaces.raised`, radius `radii.sm`), and
//! a down-arrow button. Clicking an arrow steps the value by 1 with wrap-
//! around (hours 0..=23, minutes/seconds 0..=59); scrolling the wheel while
//! the cursor is inside a value cell does the same.
//!
//! The seconds spinner is hidden when the picker is constructed without
//! `with_seconds`; its `second_rect` stays `Rect::ZERO` and the colon between
//! minutes and seconds is suppressed.
//!
//! State lives entirely on the widget (this is a "leaf control" — there is no
//! Automerge path bound to the value yet; the `on_change` callback is the
//! escape hatch for plumbing changes back into the document if a host needs
//! to). The five-pass model is honoured so the widget composes cleanly inside
//! a `Row` / `Column`.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};

use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// Maximum hour value (inclusive). 24-hour clock — wraps 23 → 0.
const HOUR_MAX: u8 = 23;
/// Maximum minute/second value (inclusive). Wraps 59 → 0.
const MINSEC_MAX: u8 = 59;

/// Fixed width of one spinner cell. Picked so the two-digit value reads
/// comfortably at the `lg` type-scale (16 px) without crowding the arrows.
const SPINNER_WIDTH: f32 = 36.0;
/// Fixed height of the picker. Three vertical regions of equal height
/// (arrow, value, arrow) of 24 px each — the value cell ends up matching the
/// `lg` text height with some breathing room.
const PICKER_HEIGHT: f32 = 72.0;
/// Width of the colon separator between spinners.
const COLON_WIDTH: f32 = 8.0;

/// Which cell currently has keyboard focus (digit-input target).
/// `None` means no cell is focused and digit keys are ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeCell { Hours, Minutes, Seconds }

/// Three-spinner time picker. See module docs.
pub struct TimePicker {
    hours: u8,           // 0..=23
    minutes: u8,         // 0..=59
    seconds: Option<u8>, // Some(0..=59) only when explicitly enabled
    on_change: Option<Box<dyn FnMut(u8, u8, Option<u8>)>>,
    rect: Rect,
    hour_rect: Rect,     // populated in apply_layout
    minute_rect: Rect,
    second_rect: Rect,   // ZERO when seconds == None
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
            hours: hours.min(HOUR_MAX),
            minutes: minutes.min(MINSEC_MAX),
            seconds: None,
            on_change: None,
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

    /// Which cell currently consumes digit keys, if any. Mostly for
    /// tests; the chrome reads this for visual focus highlighting.
    pub fn focused_cell(&self) -> Option<TimeCell> { self.focused_cell }

    /// Current per-cell digit accumulator (visible while typing).
    pub fn digit_buffer(&self) -> &str { &self.digit_buffer }

    /// Append a single digit to the focused cell's accumulator and
    /// commit it. Behaviour:
    /// - First digit: replaces the cell value (so typing "9" into a
    ///   field currently showing "23" gives "09").
    /// - Second digit: appends if the resulting two-digit number is
    ///   still in range; otherwise replaces the buffer with just the
    ///   new digit (so typing "2" then "5" into hours gives "5", not
    ///   "25").
    /// - After two valid digits the focus auto-advances to the next
    ///   cell, matching every native time-input.
    fn accept_digit(&mut self, d: u8) {
        let Some(cell) = self.focused_cell else { return; };
        let max = match cell {
            TimeCell::Hours => HOUR_MAX,
            TimeCell::Minutes | TimeCell::Seconds => MINSEC_MAX,
        };
        if self.digit_buffer.is_empty() || self.digit_buffer.len() >= 2 {
            self.digit_buffer.clear();
        }
        let mut candidate = self.digit_buffer.clone();
        candidate.push(char::from_digit(d as u32, 10).unwrap());
        let parsed: u8 = candidate.parse().unwrap_or(0);
        if parsed > max {
            // The two-digit candidate overflows: drop the prior digit
            // and treat the new one as a fresh first character.
            self.digit_buffer = char::from_digit(d as u32, 10).unwrap().to_string();
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
            Some(TimeCell::Minutes) => if self.seconds.is_some() {
                Some(TimeCell::Seconds)
            } else {
                None
            },
            Some(TimeCell::Seconds) => None,
            None => None,
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
        if self.hour_rect.contains(p) { return Some(TimeCell::Hours); }
        if self.minute_rect.contains(p) { return Some(TimeCell::Minutes); }
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
    pub fn with_seconds(mut self, seconds: u8) -> Self {
        self.seconds = Some(seconds.min(MINSEC_MAX));
        self
    }

    pub fn hours(&self) -> u8 { self.hours }
    pub fn minutes(&self) -> u8 { self.minutes }
    pub fn seconds(&self) -> Option<u8> { self.seconds }

    /// Install a callback invoked with `(hours, minutes, seconds)` every
    /// time a user-driven change commits a new value.
    pub fn on_change<F: FnMut(u8, u8, Option<u8>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Set the hour value, clamping into 0..=23.
    pub fn set_hours(&mut self, h: u8) {
        self.hours = h.min(HOUR_MAX);
    }

    /// Set the minute value, clamping into 0..=59.
    pub fn set_minutes(&mut self, m: u8) {
        self.minutes = m.min(MINSEC_MAX);
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

    /// Wrap `v + 1` within `0..=max`.
    fn wrap_inc(v: u8, max: u8) -> u8 {
        if v >= max { 0 } else { v + 1 }
    }

    /// Wrap `v - 1` within `0..=max`.
    fn wrap_dec(v: u8, max: u8) -> u8 {
        if v == 0 { max } else { v - 1 }
    }

    /// Fire the `on_change` callback with the current values.
    fn fire_on_change(&mut self) {
        if let Some(cb) = self.on_change.as_mut() {
            cb(self.hours, self.minutes, self.seconds);
        }
    }

    /// Bump the hour spinner by `delta` (±1).
    fn step_hours(&mut self, delta: i8) {
        if delta > 0 {
            self.hours = Self::wrap_inc(self.hours, HOUR_MAX);
        } else if delta < 0 {
            self.hours = Self::wrap_dec(self.hours, HOUR_MAX);
        }
        self.fire_on_change();
    }

    /// Bump the minute spinner by `delta` (±1).
    fn step_minutes(&mut self, delta: i8) {
        if delta > 0 {
            self.minutes = Self::wrap_inc(self.minutes, MINSEC_MAX);
        } else if delta < 0 {
            self.minutes = Self::wrap_dec(self.minutes, MINSEC_MAX);
        }
        self.fire_on_change();
    }

    /// Bump the seconds spinner by `delta` (±1). No-op if seconds disabled.
    fn step_seconds(&mut self, delta: i8) {
        let Some(cur) = self.seconds else { return };
        let new = if delta > 0 {
            Self::wrap_inc(cur, MINSEC_MAX)
        } else if delta < 0 {
            Self::wrap_dec(cur, MINSEC_MAX)
        } else {
            cur
        };
        self.seconds = Some(new);
        self.fire_on_change();
    }

    /// Draw a single chevron (up or down) centred in `cell`.
    fn draw_chevron(scene: &mut Scene, transform: Affine, cell: Rect, color: vello::peniko::Color, up: bool) {
        // Triangle inscribed in the central ~50% of the cell — keeps the
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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Total width = spinner + colon + spinner [+ colon + spinner]
        let n_spinners: f32 = if self.seconds.is_some() { 3.0 } else { 2.0 };
        let total_w = n_spinners * SPINNER_WIDTH + (n_spinners - 1.0) * COLON_WIDTH;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(total_w),
                    height: Dimension::length(PICKER_HEIGHT),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );

        let cell_w = SPINNER_WIDTH as f64;
        let colon_w = COLON_WIDTH as f64;
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius = theme.tokens.radii.sm as f64;
        let text_size = theme.tokens.typography.scale.lg;

        // Collect the visible spinner cells along with their current value.
        let mut cells: Vec<(Rect, u8)> = vec![
            (self.hour_rect, self.hours),
            (self.minute_rect, self.minutes),
        ];
        if let Some(s) = self.seconds {
            cells.push((self.second_rect, s));
        }

        for (cell_idx, (cell, value)) in cells.iter().enumerate() {
            let (up_rect, value_rect, down_rect) = Self::spinner_subrects(*cell);

            // Value cell background.
            let bg = RoundedRect::from_rect(value_rect, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &bg,
            );
            // Focus ring on the cell currently consuming digit keys.
            let cell_kind = match cell_idx {
                0 => TimeCell::Hours,
                1 => TimeCell::Minutes,
                _ => TimeCell::Seconds,
            };
            if self.is_focused && self.focused_cell == Some(cell_kind) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    colors.border_focus,
                    None,
                    &bg,
                );
            }

            // Two-digit value label centred in the value cell.
            let label = format!("{:02}", value);
            let (w, h) = text_renderer.measure(&label, text_size, None);
            let tx = value_rect.x0 + (value_rect.width() - w as f64) / 2.0;
            let ty = value_rect.y0 + (value_rect.height() - h as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &label,
                text_size,
                colors.text_primary,
                transform * Affine::translate((tx, ty)),
                None,
            );

            // Up and down chevrons.
            Self::draw_chevron(scene, transform, up_rect, colors.text_dim, true);
            Self::draw_chevron(scene, transform, down_rect, colors.text_dim, false);
        }

        // Colons between visible spinners. Drawn at the same point size as
        // the value text so the baselines line up.
        let between_h_m_x = (self.hour_rect.x1 + self.minute_rect.x0) / 2.0;
        let centre_y = self.rect.center().y;
        let colon = ":";
        let (cw, ch) = text_renderer.measure(colon, text_size, None);
        text_renderer.draw_text(
            scene,
            colon,
            text_size,
            colors.text_dim,
            transform * Affine::translate((between_h_m_x - cw as f64 / 2.0, centre_y - ch as f64 / 2.0)),
            None,
        );
        if self.seconds.is_some() {
            let between_m_s_x = (self.minute_rect.x1 + self.second_rect.x0) / 2.0;
            text_renderer.draw_text(
                scene,
                colon,
                text_size,
                colors.text_dim,
                transform * Affine::translate((between_m_s_x - cw as f64 / 2.0, centre_y - ch as f64 / 2.0)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                // First: did the click land on a value cell (the
                // middle third)? If so, latch keyboard focus on that
                // cell so digit keys flow through `accept_digit`.
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
                // Otherwise: same up/down chevron handling as before.
                let mut cells: Vec<(Rect, u8)> = vec![
                    (self.hour_rect, 0),
                    (self.minute_rect, 1),
                ];
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
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: element_state, text, .. }, .. } => {
                if !self.is_focused || *element_state != ElementState::Pressed {
                    return;
                }
                let shift = modifiers.state().shift_key();
                match logical_key {
                    Key::Named(NamedKey::Tab) => {
                        if shift { self.retreat_focus(); } else { self.advance_focus(); }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.focused_cell = None;
                        self.digit_buffer.clear();
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        // Drop the most recent digit from the buffer.
                        // Empty buffer → reset the cell value to zero
                        // (clearing the field).
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
                        // Direct digit input. We deliberately use the
                        // logical-key character path so numpad digits
                        // also work (winit reports them as
                        // `Key::Character("0".."9")` after IME).
                        if let Some(t) = text {
                            for c in t.chars() {
                                if let Some(d) = c.to_digit(10) {
                                    self.accept_digit(d as u8);
                                    *handled = true;
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let p = self.last_cursor;
                // Wheel up → positive y delta → increment; wheel down →
                // negative → decrement. Match the convention from native
                // OS time pickers and the spec.
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(pp) => pp.y,
                };
                let step: i8 = if dy > 0.0 { 1 } else if dy < 0.0 { -1 } else { 0 };
                if step == 0 {
                    return;
                }

                let mut cells: Vec<(Rect, u8)> = vec![
                    (self.hour_rect, 0),
                    (self.minute_rect, 1),
                ];
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
            _ => {}
        }
    }

    fn name(&self) -> &str { "TimePicker" }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
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
    fn is_focused(&self) -> bool { self.is_focused }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        tp.handle_event(
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

    /// Stage `hour_rect`, `minute_rect`, and (optionally) `second_rect` by
    /// running `build_layout` + `apply_layout` through a real taffy tree.
    /// Returns the picker with all sub-rects populated.
    fn picker_with_layout(tp: &mut TimePicker) {
        use taffy::prelude::TaffyMaxContent;
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = tp.build_layout(&mut taffy, &crate::theme::Theme::dark(), &mut node_map);
        let root = taffy.new_with_children(Style::default(), &[node]).unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        tp.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn time_picker_clamps_hours_to_23() {
        let tp = TimePicker::new(25, 30);
        assert_eq!(tp.hours(), 23);
        let mut tp = tp;
        tp.set_hours(99);
        assert_eq!(tp.hours(), 23);
    }

    #[test]
    fn time_picker_clamps_minutes_to_59() {
        let tp = TimePicker::new(12, 99);
        assert_eq!(tp.minutes(), 59);
    }

    #[test]
    fn time_picker_scroll_up_increments_minutes() {
        let mut tp = TimePicker::new(10, 30);
        picker_with_layout(&mut tp);
        assert!(tp.minute_rect.width() > 0.0, "minute_rect must be populated by apply_layout");

        // Move cursor into the centre of the minute spinner's value cell.
        // The value cell is the middle third of `minute_rect`.
        let centre = tp.minute_rect.center();
        dispatch(&mut tp, &cursor_moved(centre.x, centre.y));

        // Positive wheel delta → increment.
        dispatch(&mut tp, &wheel(1.0));

        assert_eq!(tp.minutes(), 31, "wheel up over minute cell must increment minutes");
    }

    /// Clicking on the hour value cell must latch keyboard focus on
    /// hours so subsequent digit keys edit the hour value (winit's
    /// `KeyEvent` can't be constructed in tests, so we exercise the
    /// click path here and the digit-acceptance path below directly).
    #[test]
    fn time_picker_click_on_hour_value_cell_focuses_hours() {
        let mut tp = TimePicker::new(10, 30);
        picker_with_layout(&mut tp);
        Widget::set_focus(&mut tp, true);

        let centre = tp.hour_rect.center();
        dispatch(&mut tp, &cursor_moved(centre.x, centre.y));
        dispatch(&mut tp, &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));
    }

    /// Typing two digits into the focused hour cell sets the hour
    /// directly (replaces, not appends). Auto-advances to minutes
    /// after the second digit lands.
    #[test]
    fn time_picker_two_digit_input_sets_hours_then_advances() {
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

    /// Typing a second digit that would overflow the max drops the
    /// prior digit and accepts the new one as a fresh first
    /// character — "2" → "5" leaves hours = 5, not 25.
    #[test]
    fn time_picker_two_digit_overflow_resets_to_new_digit() {
        let mut tp = TimePicker::new(0, 0);
        Widget::set_focus(&mut tp, true);
        tp.focused_cell = Some(TimeCell::Hours);

        tp.accept_digit(2);
        assert_eq!(tp.hours(), 2);
        tp.accept_digit(5);
        assert_eq!(tp.hours(), 5);
        assert_eq!(tp.digit_buffer(), "5");
    }

    #[test]
    fn time_picker_tab_cycles_cells() {
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

    /// Shift+Tab equivalent: walking back through the cells.
    #[test]
    fn time_picker_retreat_focus_walks_back() {
        let mut tp = TimePicker::new(12, 30).with_seconds(45);
        Widget::set_focus(&mut tp, true);
        tp.focused_cell = Some(TimeCell::Seconds);

        tp.retreat_focus();
        assert_eq!(tp.focused_cell(), Some(TimeCell::Minutes));
        tp.retreat_focus();
        assert_eq!(tp.focused_cell(), Some(TimeCell::Hours));
    }
}
