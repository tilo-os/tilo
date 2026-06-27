//! Single-line text input with full editing affordances:
//!
//! * Cursor placement via click; click + drag selects.
//! * Double-click selects a word; triple-click selects all.
//! * Shift + arrow / Home / End extends the selection.
//! * Ctrl + arrow jumps to the previous / next word boundary.
//! * Ctrl + A / C / V / X for select-all / copy / paste / cut.
//! * Backspace / Delete respect the selection (replace then collapse);
//!   Ctrl-modified delete consumes the next word.
//! * CRDT-backed: every mutation is replayed through
//!   `AutoCommit::splice_text` so collaborators see the same edits.
//!
//! Text vertical alignment matches the caret rendering — both are
//! centered against the same `line_height = font_size * 1.25` band so
//! the glyphs don't drift below the caret (visual-gap regression
//! pinned by `text_baseline_centred_with_caret`).

use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable, ObjType};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point, Stroke};
use vello::peniko::BlendMode;
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, NamedKey};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::color_compat::ColorCompat;
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::text_selection::{
    clipboard_copy, clipboard_paste, next_char_boundary, next_word_boundary,
    prev_char_boundary, prev_word_boundary, word_boundaries,
};
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};

/// Render-time font size for the input's body text. Kept as a single
/// constant so the measure-and-draw pair always agrees.
const FONT_SIZE: f32 = 16.0;
/// `line_height` follows the same `× 1.25` ratio cosmic-text uses
/// when shaping (see `text_render.rs::measure_weighted`). The caret
/// is centered against this band, not against `FONT_SIZE` directly,
/// so the glyph row that draws *below* the cosmic-text baseline ends
/// up vertically aligned with the caret.
const LINE_HEIGHT: f32 = FONT_SIZE * 1.25;
/// Caret thickness in CSS-like pixels. 2 px reads as a clean ibeam
/// without becoming an obvious block at smaller font sizes.
const CARET_WIDTH: f64 = 2.0;
/// Maximum gap between two mouse-clicks that still counts as a
/// double / triple click. 400 ms matches the GTK / Qt default.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
/// Pixels of cursor wiggle still allowed between rapid clicks before
/// they're treated as separate click chains.
const MULTI_CLICK_RADIUS: f64 = 4.0;

pub struct TextInput {
    _id: String,
    path: Vec<PathSegment>,
    rect: Rect,
    is_hovered: bool,
    is_focused: bool,
    text: String,
    /// Current caret position as a *byte* index into `text`. Always on
    /// a char boundary (clamped by `sync` and after every mutation).
    cursor_pos: usize,
    /// When `Some`, an active selection is anchored here. The visible
    /// range is `(min(anchor, cursor), max(anchor, cursor))`. Cleared
    /// to `None` whenever the cursor moves without `shift` held, or
    /// when text is inserted / a single click lands.
    selection_anchor: Option<usize>,
    /// True while the user is dragging the mouse to extend a selection
    /// (between MouseInput::Pressed and MouseInput::Released). Forces
    /// `wants_pointer_capture` so a drag that leaves the rect keeps
    /// updating the selection.
    is_dragging: bool,
    /// Time + position of the most recent click — used to detect
    /// double / triple clicks (within `MULTI_CLICK_INTERVAL` and
    /// `MULTI_CLICK_RADIUS`).
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    scroll_x: f64,
    last_edit_time: Option<Instant>,
    /// When true the widget renders / measures a row of bullet glyphs in
    /// place of the real text (password masking). The CRDT-backed
    /// `self.text` stays the *true* value so callers that read the
    /// document (e.g. the greeter's auth path) get the real secret.
    secret: bool,
}

impl TextInput {
    /// Construct a text input bound to a presence `path` (used as the
    /// widget's `PresenceKey::Widget { path }`). Empty path opts the widget
    /// out of presence rendering.
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self {
            _id: String::new(),
            path,
            rect: Rect::ZERO,
            is_hovered: false,
            is_focused: false,
            text: String::new(),
            cursor_pos: 0,
            selection_anchor: None,
            is_dragging: false,
            last_click_time: None,
            last_click_pos: Point::ZERO,
            click_count: 0,
            on_change_path: String::new(),
            last_mouse_pos: Point::ZERO,
            char_x_positions: Vec::new(),
            scroll_x: 0.0,
            last_edit_time: None,
            secret: false,
        }
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self._id = id.to_string();
        self
    }

    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self
    }

    /// Enable password masking: glyphs render as `•` bullets while the
    /// stored CRDT value remains the real text. No-op visual change for
    /// the default (`false`) so existing callers are unaffected.
    pub fn with_secret(mut self, on: bool) -> Self {
        self.secret = on;
        self
    }

    /// The string actually painted / measured. For secret inputs this is
    /// one bullet per *character* of the real text; otherwise it's the
    /// real text itself.
    fn display_text(&self) -> String {
        if self.secret {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    /// Display string for the prefix `self.text[..byte_idx]` — used to
    /// keep `char_x_positions` (which is keyed by real-text byte offsets)
    /// aligned with the masked glyph run, so caret placement and click
    /// resolution stay correct under masking.
    fn display_prefix(&self, byte_idx: usize) -> String {
        if self.secret {
            "•".repeat(self.text[..byte_idx].chars().count())
        } else {
            self.text[..byte_idx].to_string()
        }
    }

    /// Sorted `(start, end)` byte indices of the live selection, or
    /// `None` when nothing is selected (anchor missing or collapsed
    /// against the caret).
    fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor_pos { return None; }
        Some((anchor.min(self.cursor_pos), anchor.max(self.cursor_pos)))
    }

    fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// Substring of `text` covered by the live selection.
    fn selected_str(&self) -> Option<&str> {
        self.selection_range().map(|(s, e)| &self.text[s..e])
    }

    /// Move the caret to `pos`. When `extend` is true the existing
    /// selection anchor is preserved (or the current cursor is
    /// promoted to anchor if there's no active selection); when
    /// false, the selection is collapsed.
    fn set_cursor(&mut self, pos: usize, extend: bool) {
        let pos = pos.min(self.text.len());
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor_pos = pos;
    }

    /// Delete the current selection from both the local string and
    /// the CRDT. No-op if there's no selection.
    fn delete_selection(&mut self, state: &mut AutoCommit, obj_id: &ObjId) {
        let Some((start, end)) = self.selection_range() else { return; };
        let char_start = self.text[..start].chars().count();
        let char_count = self.text[start..end].chars().count();
        self.text.replace_range(start..end, "");
        self.cursor_pos = start;
        self.selection_anchor = None;
        self.last_edit_time = Some(Instant::now());
        let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
    }

    /// Insert `s` at the caret, replacing the current selection if
    /// any. The CRDT delta and the local cursor are kept in sync.
    fn insert_str(&mut self, s: &str, state: &mut AutoCommit, obj_id: &ObjId) {
        if let Some((start, end)) = self.selection_range() {
            let char_start = self.text[..start].chars().count();
            let char_count = self.text[start..end].chars().count();
            self.text.replace_range(start..end, s);
            self.cursor_pos = start + s.len();
            self.selection_anchor = None;
            let _ = state.splice_text(obj_id, char_start, char_count as isize, s);
        } else {
            let char_idx = self.text[..self.cursor_pos].chars().count();
            self.text.insert_str(self.cursor_pos, s);
            let _ = state.splice_text(obj_id, char_idx, 0, s);
            self.cursor_pos += s.len();
        }
        self.last_edit_time = Some(Instant::now());
    }

    /// Resolve the cursor byte-position from a logical-space x point
    /// inside the input's rect.
    fn cursor_from_x(&self, logical_x: f64) -> usize {
        let padding_x = 10.0;
        let relative_x = logical_x - (self.rect.x0 + padding_x) + self.scroll_x;
        let mut best_dist = f64::MAX;
        let mut best_idx = 0;
        for (i, &x_pos) in self.char_x_positions.iter().enumerate() {
            if i == self.text.len() || self.text.is_char_boundary(i) {
                let dist = (relative_x - x_pos).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = i;
                }
            }
        }
        best_idx
    }

    fn clamp_to_text(&mut self) {
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }
        if let Some(a) = self.selection_anchor {
            let mut a = a.min(self.text.len());
            while a > 0 && !self.text.is_char_boundary(a) { a -= 1; }
            self.selection_anchor = Some(a);
        }
    }
}

impl Widget for TextInput {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Ok(Some((val, obj_id))) = state.get(root, self.on_change_path.as_str()) {
            match val {
                Value::Scalar(s) => {
                    if let automerge::ScalarValue::Str(text) = s.as_ref() {
                        self.text = text.to_string();
                    }
                }
                Value::Object(ObjType::Text) => {
                    if let Ok(text) = state.text(&obj_id) {
                        self.text = text;
                    }
                }
                _ => {}
            }
        }
        self.clamp_to_text();
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(40.0),
            },
            ..Default::default()
        }).unwrap();
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
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let bg_color = if self.is_focused {
            theme.bg_active
        } else {
            theme.tokens.colors.surfaces.raised
        };

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, bg_color, None, &shape);

        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(&Stroke::new(if self.is_focused { 2.0 } else { 1.0 }), transform, border_color, None, &shape);

        let text_size = FONT_SIZE;
        let line_height = LINE_HEIGHT as f64;
        let padding_x = theme.padding_standard as f64;

        // The painted/measured string — bullets when masked, real text
        // otherwise. `char_x_positions` stays keyed by *real-text* byte
        // offsets (so the caret/click logic below is unchanged) but the
        // measured widths come from the masked glyph run.
        let display = self.display_text();

        // Pre-calculate positions for exact caret placement and click resolution.
        self.char_x_positions.clear();
        self.char_x_positions.resize(self.text.len() + 1, 0.0);
        for (byte_idx, _) in self.text.char_indices() {
            let (w, _) = text_renderer.measure(&self.display_prefix(byte_idx), text_size, None);
            self.char_x_positions[byte_idx] = w as f64;
        }
        let (total_w, _) = text_renderer.measure(&display, text_size, None);
        self.char_x_positions[self.text.len()] = total_w as f64;

        // Caret + scroll math.
        let cursor_pos = self.cursor_pos.min(self.text.len());
        let cursor_x_offset = if cursor_pos < self.char_x_positions.len() {
            self.char_x_positions[cursor_pos]
        } else {
            total_w as f64
        };

        let visible_width = self.rect.width() - padding_x * 2.0;
        if cursor_x_offset - self.scroll_x < 0.0 {
            self.scroll_x = cursor_x_offset;
        } else if cursor_x_offset - self.scroll_x > visible_width {
            self.scroll_x = cursor_x_offset - visible_width;
        }

        scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);

        // Vertical centering: position the *line_height* band in the
        // middle of the rect so glyph descenders + caret share the
        // same visual axis. `text_y` is the top of the line.
        let text_y = self.rect.y0 + (self.rect.height() - line_height) / 2.0;

        // Selection background — paint before the text so glyphs sit on top.
        if let Some((sel_start, sel_end)) = self.selection_range() {
            let x_start = self.char_x_positions[sel_start];
            let x_end = self.char_x_positions[sel_end];
            let sel_rect = Rect::new(
                self.rect.x0 + padding_x + x_start - self.scroll_x,
                text_y,
                self.rect.x0 + padding_x + x_end - self.scroll_x,
                text_y + line_height,
            );
            // Translucent accent so the underlying text reads through.
            let sel_color = theme.accent_primary.with_alpha_factor(0.35);
            scene.fill(vello::peniko::Fill::NonZero, transform, sel_color, None, &sel_rect);
        }

        text_renderer.draw_text(
            scene,
            &display,
            text_size,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0 + padding_x - self.scroll_x, text_y)),
            None,
        );

        if self.is_focused {
            let cursor_x = self.rect.x0 + padding_x + cursor_x_offset - self.scroll_x;
            let cursor_rect = Rect::new(
                cursor_x,
                text_y,
                cursor_x + CARET_WIDTH,
                text_y + line_height,
            );
            scene.fill(vello::peniko::Fill::NonZero, transform, theme.accent_primary, None, &cursor_rect);
        }

        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        self.clamp_to_text();

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
                self.last_mouse_pos = logical_pos;

                if self.is_dragging {
                    let pos = self.cursor_from_x(logical_pos.x);
                    if pos != self.cursor_pos {
                        if self.selection_anchor.is_none() {
                            self.selection_anchor = Some(self.cursor_pos);
                        }
                        self.cursor_pos = pos;
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                match element_state {
                    ElementState::Pressed if self.is_hovered => {
                        if !self.is_focused {
                            *focus_target = Some(node_id);
                        }

                        // Multi-click ladder: single → double (word) → triple (all).
                        let now = Instant::now();
                        let within_time = self.last_click_time
                            .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
                        let within_radius =
                            (self.last_click_pos - self.last_mouse_pos).hypot() <= MULTI_CLICK_RADIUS;
                        self.click_count = if within_time && within_radius {
                            self.click_count + 1
                        } else {
                            1
                        };
                        self.last_click_time = Some(now);
                        self.last_click_pos = self.last_mouse_pos;

                        let pos = self.cursor_from_x(self.last_mouse_pos.x);
                        match self.click_count {
                            1 => {
                                if modifiers.state().shift_key() {
                                    self.set_cursor(pos, true);
                                } else {
                                    self.selection_anchor = None;
                                    self.cursor_pos = pos;
                                    self.is_dragging = true;
                                }
                            }
                            2 => {
                                let (start, end) = word_boundaries(&self.text, pos);
                                self.selection_anchor = Some(start);
                                self.cursor_pos = end;
                            }
                            _ => {
                                self.selection_anchor = Some(0);
                                self.cursor_pos = self.text.len();
                            }
                        }
                        *handled = true;
                    }
                    ElementState::Released => {
                        if self.is_dragging {
                            self.is_dragging = false;
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: element_state, .. }, .. } => {
                if !self.is_focused || *element_state != ElementState::Pressed {
                    return;
                }
                let Ok(Some((Value::Object(ObjType::Text), obj_id))) =
                    state.get(root, self.on_change_path.as_str()) else { return; };
                let shift = modifiers.state().shift_key();
                let ctrl = modifiers.state().control_key();

                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        let new_pos = if ctrl {
                            prev_word_boundary(&self.text, self.cursor_pos)
                        } else {
                            prev_char_boundary(&self.text, self.cursor_pos)
                        };
                        // Plain arrow with active selection collapses
                        // to the selection edge instead of moving one
                        // character past it — matches every native
                        // editor's behaviour.
                        if !shift && !ctrl {
                            if let Some((s, _)) = self.selection_range() {
                                self.cursor_pos = s;
                                self.selection_anchor = None;
                                *handled = true;
                                return;
                            }
                        }
                        self.set_cursor(new_pos, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let new_pos = if ctrl {
                            next_word_boundary(&self.text, self.cursor_pos)
                        } else {
                            next_char_boundary(&self.text, self.cursor_pos)
                        };
                        if !shift && !ctrl {
                            if let Some((_, e)) = self.selection_range() {
                                self.cursor_pos = e;
                                self.selection_anchor = None;
                                *handled = true;
                                return;
                            }
                        }
                        self.set_cursor(new_pos, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        self.set_cursor(0, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.set_cursor(self.text.len(), shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        if self.has_selection() {
                            self.delete_selection(state, &obj_id);
                        } else if self.cursor_pos > 0 {
                            let start = if ctrl {
                                prev_word_boundary(&self.text, self.cursor_pos)
                            } else {
                                prev_char_boundary(&self.text, self.cursor_pos)
                            };
                            let end = self.cursor_pos;
                            let char_start = self.text[..start].chars().count();
                            let char_count = self.text[start..end].chars().count();
                            self.text.replace_range(start..end, "");
                            self.cursor_pos = start;
                            self.last_edit_time = Some(Instant::now());
                            let _ = state.splice_text(&obj_id, char_start, char_count as isize, "");
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        if self.has_selection() {
                            self.delete_selection(state, &obj_id);
                        } else if self.cursor_pos < self.text.len() {
                            let start = self.cursor_pos;
                            let end = if ctrl {
                                next_word_boundary(&self.text, self.cursor_pos)
                            } else {
                                next_char_boundary(&self.text, self.cursor_pos)
                            };
                            let char_start = self.text[..start].chars().count();
                            let char_count = self.text[start..end].chars().count();
                            self.text.replace_range(start..end, "");
                            self.last_edit_time = Some(Instant::now());
                            let _ = state.splice_text(&obj_id, char_start, char_count as isize, "");
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_str(" ", state, &obj_id);
                        *handled = true;
                    }
                    Key::Character(c) if ctrl => {
                        let lc = c.to_lowercase();
                        match lc.as_str() {
                            "a" => {
                                if !self.text.is_empty() {
                                    self.selection_anchor = Some(0);
                                    self.cursor_pos = self.text.len();
                                }
                                *handled = true;
                            }
                            "c" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(s);
                                }
                                *handled = true;
                            }
                            "x" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(&s.to_string());
                                    self.delete_selection(state, &obj_id);
                                }
                                *handled = true;
                            }
                            "v" => {
                                if let Some(paste) = clipboard_paste() {
                                    // Strip any newlines / nulls — single-line input.
                                    let cleaned: String = paste
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .collect();
                                    if !cleaned.is_empty() {
                                        self.insert_str(&cleaned, state, &obj_id);
                                    }
                                }
                                *handled = true;
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        if let Some(t) = text {
                            if !t.as_str().chars().all(|c| c.is_control()) {
                                self.insert_str(t.as_str(), state, &obj_id);
                                *handled = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "TextInput" }

    fn test_id(&self) -> Option<&str> {
        if self._id.is_empty() {
            Some("text_input")
        } else {
            Some(self._id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    /// While dragging the selection or while a multi-click chain is
    /// still alive, the input wants to keep receiving cursor events
    /// even when the cursor wanders into a sibling widget. Matches
    /// the same pattern as `Slider::wants_pointer_capture`.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn needs_commit(&self) -> bool {
        if let Some(t) = self.last_edit_time {
            t.elapsed() >= Duration::from_secs(1)
        } else {
            false
        }
    }

    fn on_commit(&mut self) {
        self.last_edit_time = None;
    }

    fn get_state(&self) -> Option<String> {
        Some(self.cursor_pos.to_string())
    }

    fn set_state(&mut self, state: &str) {
        if let Ok(pos) = state.parse::<usize>() {
            self.cursor_pos = pos;
        }
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.path.clone() })
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(key) = self.presence_key() {
            let origin = self.rect.origin();
            let advance = 8.0_f32;

            if let Some(list) = presence.carets.get(&key) {
                for (user, caret) in list.iter() {
                    let x = origin.x as f32 + 4.0 + (*caret as f32) * advance;
                    let y = origin.y as f32 + 4.0;
                    presence.draw_peer_caret(scene, theme, user, (x, y));
                }
            }

            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list.iter() {
                    let x_start = origin.x + 4.0 + (range.start as f64) * advance as f64;
                    let x_end = origin.x + 4.0 + (range.end as f64) * advance as f64;
                    let rect = Rect::new(
                        x_start,
                        origin.y + 2.0,
                        x_end,
                        origin.y + 22.0,
                    );
                    presence.draw_peer_selection(scene, theme, user, range, &[rect]);
                }
            }
        }

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let r = self.rect;
                let overlay = Self::PERMISSION_DENIED_OVERLAY;
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    Affine::IDENTITY,
                    overlay,
                    None,
                    &r,
                );
            }
        }
    }
}

impl TextInput {
    /// Overlay color used to dim the control when the permission scope
    /// resolves to denied. Concept's every-widget convention is
    /// `bg-black/30` ≈ 0.30 × 255 ≈ 77.
    pub(crate) const PERMISSION_DENIED_OVERLAY: vello::peniko::Color =
        vello::peniko::Color::from_rgba8(0, 0, 0, 77);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Polish-sweep #127 pin: the permission-denied dim must paint at
    /// the same alpha across every widget.
    #[test]
    fn permission_denied_overlay_alpha_matches_other_widgets() {
        let expected = vello::peniko::Color::from_rgba8(0, 0, 0, 77);
        assert_eq!(TextInput::PERMISSION_DENIED_OVERLAY, expected);
    }

    #[test]
    fn text_input_test_id_default_is_text_input() {
        let ti = TextInput::new(Vec::new());
        assert_eq!(Widget::test_id(&ti), Some("text_input"));
    }

    #[test]
    fn text_input_test_id_can_be_overridden() {
        let ti = TextInput::new(Vec::new()).with_id("search_box");
        assert_eq!(Widget::test_id(&ti), Some("search_box"));
    }

    /// Walking back through a sentence with `prev_word_boundary` must
    /// land on the start of each word. Word membership uses
    /// `is_word_char`, so the spaces between words count as
    /// boundaries and the cursor doesn't get stuck mid-word.
    #[test]
    fn prev_word_boundary_walks_word_starts() {
        let s = "hello world quick ui";
        //       ^0    ^6    ^12   ^18
        assert_eq!(prev_word_boundary(s, s.len()), 18);
        assert_eq!(prev_word_boundary(s, 18), 12);
        assert_eq!(prev_word_boundary(s, 12), 6);
        assert_eq!(prev_word_boundary(s, 6), 0);
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    /// Mirror of the above for the forward direction. Note that
    /// `next_word_boundary` lands *past* the trailing space — that's
    /// how every native editor handles "Ctrl + Right".
    #[test]
    fn next_word_boundary_walks_word_ends() {
        let s = "hello world quick ui";
        // After "hello" the cursor lands at index 6 (start of "world").
        assert_eq!(next_word_boundary(s, 0), 6);
        assert_eq!(next_word_boundary(s, 6), 12);
        assert_eq!(next_word_boundary(s, 12), 18);
        assert_eq!(next_word_boundary(s, 18), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
    }

    /// `word_boundaries` should snap to the word the cursor is sitting
    /// *inside* — and at a space immediately after a word, to the word
    /// on the left (matches every native editor's double-click-on-
    /// trailing-space behaviour). Used by double-click word selection.
    #[test]
    fn word_boundaries_snaps_to_word_bounds() {
        let s = "hello world";
        assert_eq!(word_boundaries(s, 2), (0, 5));   // mid-word
        assert_eq!(word_boundaries(s, 0), (0, 5));   // word start
        assert_eq!(word_boundaries(s, 5), (0, 5));   // space → snap left
        assert_eq!(word_boundaries(s, 6), (6, 11));  // start of next word
        assert_eq!(word_boundaries(s, 11), (6, 11)); // end → snap left
    }

    /// Mock the bare minimum to drive a selection delete through
    /// `delete_selection`: stub doc, no CRDT roundtrip required for
    /// the local-text contract.
    #[test]
    fn delete_selection_removes_range_and_collapses() {
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("text_1");
        ti.text = "hello world".to_string();
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;
        // Use a synthetic AutoCommit + Text object so `splice_text`
        // succeeds; the test's contract is local-text only.
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.delete_selection(&mut doc, &obj_id);
        assert_eq!(ti.text, "hello ");
        assert_eq!(ti.cursor_pos, 6);
        assert!(!ti.has_selection());
    }

    /// `insert_str` over an active selection should *replace* it,
    /// not append. This is the path Ctrl+V uses when pasting over
    /// selected text.
    #[test]
    fn insert_str_replaces_selection() {
        let mut ti = TextInput::new(Vec::new()).with_on_change_path("text_1");
        ti.text = "hello world".to_string();
        ti.selection_anchor = Some(6);
        ti.cursor_pos = 11;
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.insert_str("there", &mut doc, &obj_id);
        assert_eq!(ti.text, "hello there");
        assert_eq!(ti.cursor_pos, "hello there".len());
        assert!(!ti.has_selection());
    }

    /// Password masking: a `secret` input must render bullets (one per
    /// character) while the CRDT-backed `text` keeps the *real* value —
    /// auth code reads `self.text`, the screen shows `•`. Drives a real
    /// Automerge `Text` object through `sync` to prove the stored secret
    /// is intact end-to-end.
    #[test]
    fn secret_input_masks_glyphs_but_keeps_value() {
        let mut ti = TextInput::new(Vec::new())
            .with_on_change_path("password")
            .with_secret(true);

        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "password", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hunter2");

        // sync pulls the real value out of the CRDT.
        ti.sync(&mut doc, &automerge::ROOT);
        assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

        // What gets painted/measured is a bullet per character.
        assert_eq!(ti.display_text(), "•••••••");
        // The masked prefix tracks character count, not bytes.
        assert_eq!(ti.display_prefix(3), "•••");

        // A non-secret input is unaffected (no behaviour change).
        let mut plain = TextInput::new(Vec::new());
        plain.text = "hunter2".to_string();
        assert_eq!(plain.display_text(), "hunter2");
        assert_eq!(plain.display_prefix(3), "hun");
    }

    /// The fix for visual-gap "text below center" pins the text top
    /// against `line_height` (= `FONT_SIZE * 1.25`), *not* against
    /// `FONT_SIZE` itself. Without this, the cosmic-text baseline
    /// shifts the glyphs ~3 px below the caret in a 40 px input.
    #[test]
    fn text_baseline_centred_with_caret() {
        let rect_h = 40.0_f32;
        let text_y_correct = (rect_h - LINE_HEIGHT) / 2.0;
        let text_y_naive = (rect_h - FONT_SIZE) / 2.0;
        // The naive computation lands ~2 px below the correct one for
        // 16 px text → that's the "below center" the user reported.
        assert!(text_y_naive > text_y_correct);
        assert!((text_y_naive - text_y_correct - (LINE_HEIGHT - FONT_SIZE) / 2.0).abs() < 0.01);
    }
}
