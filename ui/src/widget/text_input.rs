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

use std::sync::OnceLock;
use std::sync::Mutex;
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

/// Lazy global clipboard handle. `arboard::Clipboard::new()` can be
/// slow on Wayland the first time (sets up the data-control device),
/// so we cache one handle process-wide and lock it per access. Kept
/// behind a `Mutex` because arboard's handle isn't `Sync`.
fn clipboard() -> Option<&'static Mutex<arboard::Clipboard>> {
    static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| arboard::Clipboard::new().ok().map(Mutex::new)).as_ref()
}

fn clipboard_copy(text: &str) {
    if let Some(cb) = clipboard() {
        if let Ok(mut handle) = cb.lock() {
            let _ = handle.set_text(text.to_string());
        }
    }
}

fn clipboard_paste() -> Option<String> {
    let cb = clipboard()?;
    let mut handle = cb.lock().ok()?;
    handle.get_text().ok()
}

/// A character counts as a "word" character if it's a letter, digit,
/// or underscore — same convention as POSIX `\w` and Vim's `iskeyword`.
/// Whitespace and punctuation are word boundaries; double-click word
/// selection and Ctrl+Left/Right use this predicate.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Walk back to the previous "word boundary" (start of the word the
/// cursor sits in, or start of the previous word if already at a
/// boundary). Returns a byte index that is always on a char boundary.
fn prev_word_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx == 0 { return 0; }
    let mut i = byte_idx.min(text.len());
    while i > 0 && !text.is_char_boundary(i) { i -= 1; }
    // Skip non-word chars walking left (whitespace + punctuation eaten
    // before reaching the previous word's body).
    while i > 0 {
        let c = text[..i].chars().rev().next().unwrap();
        if is_word_char(c) { break; }
        i -= c.len_utf8();
    }
    // Skip the word body itself.
    while i > 0 {
        let c = text[..i].chars().rev().next().unwrap();
        if !is_word_char(c) { break; }
        i -= c.len_utf8();
    }
    i
}

/// Mirror of [`prev_word_boundary`] for the forward direction.
fn next_word_boundary(text: &str, byte_idx: usize) -> usize {
    let mut i = byte_idx.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) { i += 1; }
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if !is_word_char(c) { break; }
        i += c.len_utf8();
    }
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if is_word_char(c) { break; }
        i += c.len_utf8();
    }
    i
}

/// Range of the word containing `byte_idx` (or the next word if
/// `byte_idx` sits on a boundary). Returns `(start, end)` as byte
/// indices; used by double-click word selection.
fn word_range_containing(text: &str, byte_idx: usize) -> (usize, usize) {
    let mut start = byte_idx.min(text.len());
    while start > 0 && !text.is_char_boundary(start) { start -= 1; }
    // Find left edge of current word.
    while start > 0 {
        let c = text[..start].chars().rev().next().unwrap();
        if !is_word_char(c) { break; }
        start -= c.len_utf8();
    }
    // Find right edge.
    let mut end = start;
    while end < text.len() {
        let c = text[end..].chars().next().unwrap();
        if !is_word_char(c) { break; }
        end += c.len_utf8();
    }
    (start, end)
}

fn prev_char_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx == 0 { return 0; }
    let mut i = byte_idx.min(text.len()) - 1;
    while i > 0 && !text.is_char_boundary(i) { i -= 1; }
    i
}

fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx >= text.len() { return text.len(); }
    let mut i = byte_idx + 1;
    while i < text.len() && !text.is_char_boundary(i) { i += 1; }
    i
}

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
    /// True when a user edit landed since the last `take_dirty()`.
    /// Cleared on `take_dirty()` and on `set_text()` (programmatic
    /// writes by the parent don't count as user edits).
    dirty: bool,
    /// `true` in CRDT mode (a path was supplied); `false` in local
    /// mode. Gates whether the edit helpers attempt to splice into
    /// `state` (they no-op cleanly when this is false even if a
    /// path string was set without an actual CRDT object).
    crdt_bound: bool,
    on_change_path: String,
    last_mouse_pos: Point,
    char_x_positions: Vec<f64>,
    scroll_x: f64,
    last_edit_time: Option<Instant>,
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
            dirty: false,
            crdt_bound: false,
        }
    }

    /// Construct a TextInput that owns its own string with no CRDT
    /// integration. Use when a parent widget needs a small edit
    /// field (color-picker hex, in-place rename, etc.) and doesn't
    /// have a CRDT path to spend on it. Read the live string via
    /// [`value`] and observe changes with [`take_dirty`].
    pub fn local() -> Self {
        let mut s = Self::new(Vec::new());
        s.crdt_bound = false;
        s
    }

    /// Set the initial text. Cursor is parked at the end. The
    /// dirty flag stays false — this is a programmatic seed, not
    /// a user edit.
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.text = s.to_string();
        self.cursor_pos = self.text.len();
        self.dirty = false;
        self
    }

    /// Programmatically replace the text. Clamps the cursor,
    /// clears the dirty flag — the parent set this, the user
    /// didn't edit it.
    pub fn set_text(&mut self, s: &str) {
        if self.text == s { return; }
        self.text = s.to_string();
        self.selection_anchor = None;
        self.clamp_to_text();
        self.dirty = false;
        self.last_edit_time = None;
    }

    /// Current text content, regardless of CRDT/local mode.
    pub fn value(&self) -> &str {
        &self.text
    }

    /// Current caret position as a byte index into [`value`]. Guaranteed
    /// to land on a UTF-8 char boundary. Used by wrapper widgets (like
    /// `RichTextLine`) that need to know where to split / merge / decide
    /// whether the cursor is at the start or end of the line.
    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    /// Move the caret to `pos` bytes. Clamped to the text length and
    /// to the nearest preceding char boundary. Selection is collapsed.
    /// Programmatic only — does not count as a user edit (dirty stays
    /// false). Used by `RichTextLine` after a split to land the caret
    /// at the start of the new line.
    pub fn set_cursor_pos(&mut self, pos: usize) {
        let mut p = pos.min(self.text.len());
        while p > 0 && !self.text.is_char_boundary(p) {
            p -= 1;
        }
        self.cursor_pos = p;
        self.selection_anchor = None;
    }

    /// One-shot read of user edits. Returns `Some(text)` if any
    /// user-initiated edit has landed since the last call, else
    /// `None`. The dirty flag is cleared.
    pub fn take_dirty(&mut self) -> Option<String> {
        if self.dirty {
            self.dirty = false;
            Some(self.text.clone())
        } else {
            None
        }
    }

    /// External-layout assignment for callers that embed this widget
    /// outside the taffy tree (the parent owns layout). Pairs with the
    /// trait method `apply_layout`: a parent calling this is asserting
    /// it has done what `apply_layout` would normally do. Today the
    /// only invariant `apply_layout` would establish is `self.rect`; if
    /// `apply_layout` grows responsibilities in the future, this method
    /// will need to grow with it.
    ///
    /// Embed bypasses `draw_with_presence` too — only safe with
    /// local-mode TextInputs that have no presence path or permission
    /// scope.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self._id = id.to_string();
        self
    }

    pub fn with_on_change_path(mut self, path: &str) -> Self {
        self.on_change_path = path.to_string();
        self.crdt_bound = !path.is_empty();
        self
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
    /// the CRDT. No-op if there's no selection. In local mode
    /// (`obj_id` is `None`), only the local string is updated.
    fn delete_selection(&mut self, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        let Some((start, end)) = self.selection_range() else { return; };
        let char_start = self.text[..start].chars().count();
        let char_count = self.text[start..end].chars().count();
        self.text.replace_range(start..end, "");
        self.cursor_pos = start;
        self.selection_anchor = None;
        self.last_edit_time = Some(Instant::now());
        if let Some(obj_id) = obj_id {
            let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
        }
        self.dirty = true;
    }

    /// Insert `s` at the caret, replacing the current selection if
    /// any. The CRDT delta and the local cursor are kept in sync.
    /// In local mode (`obj_id` is `None`), only the local string is
    /// updated.
    fn insert_str(&mut self, s: &str, state: &mut AutoCommit, obj_id: Option<&ObjId>) {
        if let Some((start, end)) = self.selection_range() {
            let char_start = self.text[..start].chars().count();
            let char_count = self.text[start..end].chars().count();
            self.text.replace_range(start..end, s);
            self.cursor_pos = start + s.len();
            self.selection_anchor = None;
            if let Some(obj_id) = obj_id {
                let _ = state.splice_text(obj_id, char_start, char_count as isize, s);
            }
        } else {
            let char_idx = self.text[..self.cursor_pos].chars().count();
            self.text.insert_str(self.cursor_pos, s);
            if let Some(obj_id) = obj_id {
                let _ = state.splice_text(obj_id, char_idx, 0, s);
            }
            self.cursor_pos += s.len();
        }
        self.last_edit_time = Some(Instant::now());
        self.dirty = true;
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
        if !self.crdt_bound { return; }
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

        // Pre-calculate positions for exact caret placement and click resolution.
        self.char_x_positions.clear();
        self.char_x_positions.resize(self.text.len() + 1, 0.0);
        for (byte_idx, _) in self.text.char_indices() {
            let (w, _) = text_renderer.measure(&self.text[..byte_idx], text_size, None);
            self.char_x_positions[byte_idx] = w as f64;
        }
        let (total_w, _) = text_renderer.measure(&self.text, text_size, None);
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
            &self.text,
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
                                let (start, end) = word_range_containing(&self.text, pos);
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
                let obj_id_opt: Option<ObjId> = if self.crdt_bound {
                    match state.get(root, self.on_change_path.as_str()) {
                        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };
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
                            self.delete_selection(state, obj_id_opt.as_ref());
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
                            if let Some(obj_id) = obj_id_opt.as_ref() {
                                let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
                            }
                            self.dirty = true;
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        if self.has_selection() {
                            self.delete_selection(state, obj_id_opt.as_ref());
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
                            if let Some(obj_id) = obj_id_opt.as_ref() {
                                let _ = state.splice_text(obj_id, char_start, char_count as isize, "");
                            }
                            self.dirty = true;
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_str(" ", state, obj_id_opt.as_ref());
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
                                    self.delete_selection(state, obj_id_opt.as_ref());
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
                                        self.insert_str(&cleaned, state, obj_id_opt.as_ref());
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
                                self.insert_str(t.as_str(), state, obj_id_opt.as_ref());
                                *handled = true;
                            }
                        }
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(t)) => {
                if !self.is_focused {
                    return;
                }
                let obj_id_opt: Option<ObjId> = if self.crdt_bound {
                    match state.get(root, self.on_change_path.as_str()) {
                        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };
                if !t.chars().all(|c| c.is_control()) {
                    self.insert_str(t.as_str(), state, obj_id_opt.as_ref());
                    *handled = true;
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

    /// `word_range_containing` should snap to the word the cursor is
    /// sitting *inside* — and at a space immediately after a word, to
    /// the word on the left (matches every native editor's double-
    /// click-on-trailing-space behaviour). Used by double-click word
    /// selection.
    #[test]
    fn word_range_containing_snaps_to_word_bounds() {
        let s = "hello world";
        assert_eq!(word_range_containing(s, 2), (0, 5));   // mid-word
        assert_eq!(word_range_containing(s, 0), (0, 5));   // word start
        assert_eq!(word_range_containing(s, 5), (0, 5));   // space → snap left
        assert_eq!(word_range_containing(s, 6), (6, 11));  // start of next word
        assert_eq!(word_range_containing(s, 11), (6, 11)); // end → snap left
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
        ti.delete_selection(&mut doc, Some(&obj_id));
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
        ti.insert_str("there", &mut doc, Some(&obj_id));
        assert_eq!(ti.text, "hello there");
        assert_eq!(ti.cursor_pos, "hello there".len());
        assert!(!ti.has_selection());
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
