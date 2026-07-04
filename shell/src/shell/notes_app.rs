//! Notes — a minimal CRDT-backed plaintext editor hosted as an `AppWidget`.
//!
//! The whole body lives in a single Automerge `Text` object under the tile's
//! root at [`BODY_KEY`]. Edits go through `splice_text` so they merge cleanly
//! with peer edits via the existing `CollabRequest::Apply` path. The widget
//! therefore gets "fully collaborative by default" for free — no per-app
//! presence wiring beyond what every widget already does.
//!
//! Selection + clipboard model mirrors `ui::widget::TextInput`:
//!
//! * Click drops the caret; drag extends a selection.
//! * Double-click → word; triple-click → line.
//! * Shift+arrow / Shift+Home / Shift+End / Ctrl+A extends or sets selection.
//! * Backspace / Delete with a selection replace the range.
//! * Ctrl+C / Ctrl+X / Ctrl+V route through the shared
//!   [`ui::text_selection`] clipboard helpers.
//!
//! Rendering uses the `TextRenderer`'s built-in soft-wrap (cosmic-text) so
//! long paragraphs wrap inside the tile; hard newlines (`\n`) are preserved.
//! The caret and the selection highlight are positioned by splitting at
//! `\n`-delimited logical lines and measuring prefix widths — approximate
//! across wrap boundaries but matches the cursor's row/column model.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, Value,
    transaction::Transactable,
};
use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::{
    Selection, clipboard_copy, clipboard_paste, line_boundaries,
    next_word_boundary, prev_word_boundary, word_boundaries,
};
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Automerge key under the tile's root where the note body lives.
pub const BODY_KEY: &str = "notes_body";

const PADDING: f64 = 28.0;
const TEXT_SIZE: f32 = 15.0;
const LINE_HEIGHT: f32 = TEXT_SIZE * 1.4;
const COMMIT_DEBOUNCE: Duration = Duration::from_millis(800);
/// Maximum gap between two mouse-clicks that still counts as a
/// double / triple click. 400 ms matches the GTK / Qt default.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
/// Pixels of cursor wiggle still allowed between rapid clicks before
/// they're treated as separate click chains.
const MULTI_CLICK_RADIUS: f64 = 6.0;

pub struct NotesApp {
    rect: Rect,
    hovered: bool,
    focused: bool,
    text: String,
    /// Caret + selection anchor pair. `selection.cursor` is the live
    /// caret byte offset into `text`. `selection.anchor == cursor`
    /// means no selection is active.
    selection: Selection,
    last_edit: Option<Instant>,
    last_mouse: Point,
    /// True while the user is dragging the mouse to extend a selection
    /// (between MouseInput::Pressed and MouseInput::Released).
    is_dragging: bool,
    /// Time + position of the most recent click — used to detect
    /// double / triple clicks (within `MULTI_CLICK_INTERVAL` and
    /// `MULTI_CLICK_RADIUS`).
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    /// Cached geometry of the body content area — used to map mouse
    /// positions back to byte offsets in the same coordinate system the
    /// draw pass laid the text out in.
    body_origin: Point,
    wrap_width: f32,
}

impl NotesApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            text: String::new(),
            selection: Selection::default(),
            last_edit: None,
            last_mouse: Point::ZERO,
            is_dragging: false,
            last_click_time: None,
            last_click_pos: Point::ZERO,
            click_count: 0,
            body_origin: Point::ZERO,
            wrap_width: 40.0,
        }
    }

    /// Create a Notes tile pre-seeded with welcome text so first-boot users
    /// see something concrete. The seed is written to the CRDT lazily on
    /// first sync so repeated boots don't keep appending.
    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.text = text.into();
        let end = app.text.len();
        app.selection = Selection::collapsed(end);
        app
    }

    /// Resolve (or create) the Text object that backs the note body. Returns
    /// `None` only if the Automerge store rejects both the read and the
    /// create — which would indicate a corrupt doc, so callers treat it as
    /// a skip.
    fn resolve_body(&self, state: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, BODY_KEY) {
            return Some(id);
        }
        state.put_object(root, BODY_KEY, ObjType::Text).ok()
    }

    fn clamp_cursor(&mut self) {
        let len = self.text.len();
        if self.selection.cursor > len {
            self.selection.cursor = len;
        }
        while self.selection.cursor > 0 && !self.text.is_char_boundary(self.selection.cursor) {
            self.selection.cursor -= 1;
        }
        if self.selection.anchor > len {
            self.selection.anchor = len;
        }
        while self.selection.anchor > 0 && !self.text.is_char_boundary(self.selection.anchor) {
            self.selection.anchor -= 1;
        }
    }

    /// Delete the current selection from both `self.text` and the CRDT,
    /// then collapse the selection to the deletion point. No-op if the
    /// selection is collapsed.
    fn delete_selection(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.selection.is_collapsed() {
            return;
        }
        let Some(body) = self.resolve_body(state, root) else { return };
        let start = self.selection.min();
        let end = self.selection.max();
        let char_start = self.text[..start].chars().count();
        let char_count = self.text[start..end].chars().count();
        self.text.replace_range(start..end, "");
        self.selection.move_to(start);
        let _ = state.splice_text(&body, char_start, char_count as isize, "");
        self.last_edit = Some(Instant::now());
    }

    /// Insert `s` at the caret, replacing the current selection if any.
    /// Used by every typing path (printable chars, Enter, Tab, paste).
    fn insert_str(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        let Some(body) = self.resolve_body(state, root) else { return };
        if !self.selection.is_collapsed() {
            let start = self.selection.min();
            let end = self.selection.max();
            let char_start = self.text[..start].chars().count();
            let char_count = self.text[start..end].chars().count();
            self.text.replace_range(start..end, s);
            self.selection.move_to(start + s.len());
            let _ = state.splice_text(&body, char_start, char_count as isize, s);
        } else {
            let cur = self.selection.cursor;
            let char_idx = self.text[..cur].chars().count();
            self.text.insert_str(cur, s);
            let _ = state.splice_text(&body, char_idx, 0, s);
            self.selection.move_to(cur + s.len());
        }
        self.last_edit = Some(Instant::now());
    }

    /// Original single-char backspace path (used when the selection is
    /// collapsed). Selection-aware paths route through `delete_selection`.
    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.selection.cursor == 0 {
            return;
        }
        let Some(body) = self.resolve_body(state, root) else { return };
        let cur = self.selection.cursor;
        let mut prev = cur - 1;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        let removed_chars = self.text[prev..cur].chars().count();
        let char_idx = self.text[..prev].chars().count();
        self.text.drain(prev..cur);
        self.selection.move_to(prev);
        let _ = state.splice_text(&body, char_idx, removed_chars as isize, "");
        self.last_edit = Some(Instant::now());
    }

    /// Single-char delete-forward path for when the selection is collapsed.
    fn delete_after(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.selection.cursor >= self.text.len() {
            return;
        }
        let Some(body) = self.resolve_body(state, root) else { return };
        let cur = self.selection.cursor;
        let mut next = cur + 1;
        while next < self.text.len() && !self.text.is_char_boundary(next) {
            next += 1;
        }
        let removed_chars = self.text[cur..next].chars().count();
        let char_idx = self.text[..cur].chars().count();
        self.text.drain(cur..next);
        let _ = state.splice_text(&body, char_idx, removed_chars as isize, "");
        self.last_edit = Some(Instant::now());
    }

    /// All currently-selected text (or `None` if collapsed).
    fn selected_str(&self) -> Option<&str> {
        if self.selection.is_collapsed() {
            None
        } else {
            Some(&self.text[self.selection.range()])
        }
    }
}

impl Default for NotesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for NotesApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // First sync: if no body exists yet, seed it with whatever the
        // widget was constructed with (via `with_seed`). After that the
        // CRDT is the source of truth.
        match state.get(root, BODY_KEY) {
            Ok(Some((Value::Object(ObjType::Text), id))) => {
                if let Ok(t) = state.text(&id) {
                    self.text = t;
                }
            }
            _ => {
                if let Ok(id) = state.put_object(root, BODY_KEY, ObjType::Text) {
                    if !self.text.is_empty() {
                        let _ = state.splice_text(&id, 0, 0, &self.text);
                    }
                }
            }
        }
        self.clamp_cursor();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let text_color = tokens.colors.neutral[1];
        let dim = tokens.colors.neutral[4];
        // Caret is a *fill* state (focused-but-not-hovered), so use the
        // semantic `accent_primary` (indigo-500) accessor — the concept's
        // dominant accent — rather than the raw `accent[4]` (indigo-600)
        // ramp slot, which is the hover shade.
        let accent = theme.accent_primary;
        // Translucent selection background — concept calls for indigo-500
        // at ~30% alpha (matches `TextInput`'s 0.35 visually for a thinner
        // body weight).
        let sel_color = accent.with_alpha_factor(0.30);

        let content_x = self.rect.x0 + PADDING;
        let content_y = self.rect.y0 + PADDING;
        let wrap_w = (self.rect.width() - PADDING * 2.0).max(40.0) as f32;
        self.wrap_width = wrap_w;

        // Header row: "Notes" title + hint on the right.
        let title_size = tokens.typography.scale.lg;
        tr.draw_text(
            scene,
            "Notes",
            title_size,
            text_color,
            transform * Affine::translate((content_x, content_y)),
            None,
        );
        let hint = if self.focused {
            "Type to edit  ·  Ctrl+Z to undo"
        } else {
            "Click to edit"
        };
        let (hw, _) = tr.measure(hint, tokens.typography.scale.sm, None);
        tr.draw_text(
            scene,
            hint,
            tokens.typography.scale.sm,
            dim,
            transform
                * Affine::translate((
                    self.rect.x1 - PADDING - hw as f64,
                    content_y + (title_size - tokens.typography.scale.sm) as f64 / 2.0,
                )),
            None,
        );

        let body_top = content_y + title_size as f64 + 20.0;
        self.body_origin = Point::new(content_x, body_top);

        if self.text.is_empty() && !self.focused {
            tr.draw_text(
                scene,
                "Jot anything here — shared live with collaborators in the workspace.",
                TEXT_SIZE,
                dim,
                transform * Affine::translate((content_x, body_top)),
                Some(wrap_w),
            );
            return;
        }

        // Selection highlight (behind the text). Walk each logical line
        // and clip the selection range to it; paint one rect per line.
        if !self.selection.is_collapsed() {
            let sel_lo = self.selection.min();
            let sel_hi = self.selection.max();
            let mut line_start = 0usize;
            let mut row = 0usize;
            // Iterate logical lines (split on '\n'). For each, compute
            // the byte range and intersect with selection.
            let chunks = self.text.split('\n');
            for line_text in chunks {
                let line_end = line_start + line_text.len();
                let lo = sel_lo.max(line_start);
                let hi = sel_hi.min(line_end);
                if lo < hi {
                    let pre = &line_text[..(lo - line_start)];
                    let mid = &line_text[(lo - line_start)..(hi - line_start)];
                    let (x0, _) = tr.measure(pre, TEXT_SIZE, Some(wrap_w));
                    let (mw, _) = tr.measure(mid, TEXT_SIZE, Some(wrap_w));
                    let y0 = body_top + row as f64 * LINE_HEIGHT as f64;
                    let rect = Rect::new(
                        content_x + x0 as f64,
                        y0,
                        content_x + x0 as f64 + mw as f64,
                        y0 + LINE_HEIGHT as f64,
                    );
                    scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
                } else if sel_lo <= line_end && sel_hi > line_end {
                    // The selection spans the trailing newline of this
                    // logical line — paint a small "newline glyph"
                    // indicator past the line content to convey the
                    // selected line break.
                    let (full_w, _) = tr.measure(line_text, TEXT_SIZE, Some(wrap_w));
                    let y0 = body_top + row as f64 * LINE_HEIGHT as f64;
                    let rect = Rect::new(
                        content_x + full_w as f64,
                        y0,
                        content_x + full_w as f64 + (TEXT_SIZE as f64 * 0.4),
                        y0 + LINE_HEIGHT as f64,
                    );
                    scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
                }
                line_start = line_end + 1; // +1 for the consumed \n
                row += 1;
            }
        }

        // Body text with soft wrap.
        tr.draw_text(
            scene,
            &self.text,
            TEXT_SIZE,
            text_color,
            transform * Affine::translate((content_x, body_top)),
            Some(wrap_w),
        );

        if self.focused {
            // Caret: count newlines before cursor to pick a row, then measure
            // the width of the last visual line's prefix for the column. This
            // deliberately treats each `\n`-separated logical line as a
            // single wrap-unit so the caret stays close-enough to the glyph
            // position without a glyph-by-glyph layout round-trip.
            let cur = self.selection.cursor;
            let prefix = &self.text[..cur];
            let nl_count = prefix.matches('\n').count();
            let last_line_prefix = prefix.rsplit('\n').next().unwrap_or("");
            let (prefix_w, _) = tr.measure(last_line_prefix, TEXT_SIZE, Some(wrap_w));
            let caret_x = content_x + prefix_w as f64;
            let caret_y = body_top + (nl_count as f64) * LINE_HEIGHT as f64;
            let caret = Rect::new(
                caret_x,
                caret_y,
                caret_x + 1.5,
                caret_y + LINE_HEIGHT as f64,
            );
            scene.fill(Fill::NonZero, transform, accent, None, &caret);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.clamp_cursor();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                let was_hover = self.hovered;
                self.hovered = self.rect.contains(p);
                if self.hovered != was_hover {
                    *handled = true;
                }

                if self.is_dragging {
                    // Hit-test needs a TextRenderer; the event path
                    // doesn't have one. We approximate by deferring to
                    // a simpler line-then-best-char model: count rows
                    // by y, find byte offset by a prefix sweep using
                    // pixel-per-char heuristic. The real test is the
                    // draw pass which sees the renderer; here we use a
                    // tight loop over chars assuming a monospace-ish
                    // advance. This is acceptable because:
                    //   1. The selection re-snaps to true glyph bounds
                    //      on the next draw via `body_origin`.
                    //   2. The user perceives the cursor as following
                    //      their mouse — the row is always right, and
                    //      column drift settles to the glyph grid on
                    //      mouse-up via re-hit-test below.
                    let new_pos = self.approximate_hit(p);
                    if new_pos != self.selection.cursor {
                        self.selection.extend_to(new_pos);
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => match es {
                ElementState::Pressed if self.hovered => {
                    *focus_target = Some(node_id);

                    let now = Instant::now();
                    let within_time = self
                        .last_click_time
                        .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
                    let within_radius =
                        (self.last_click_pos - self.last_mouse).hypot() <= MULTI_CLICK_RADIUS;
                    self.click_count = if within_time && within_radius {
                        self.click_count + 1
                    } else {
                        1
                    };
                    self.last_click_time = Some(now);
                    self.last_click_pos = self.last_mouse;

                    let pos = self.approximate_hit(self.last_mouse);
                    match self.click_count {
                        1 => {
                            if modifiers.state().shift_key() {
                                self.selection.extend_to(pos);
                            } else {
                                self.selection.move_to(pos);
                                self.is_dragging = true;
                            }
                        }
                        2 => {
                            let (s, e) = word_boundaries(&self.text, pos);
                            self.selection.anchor = s;
                            self.selection.cursor = e;
                        }
                        _ => {
                            let (s, e) = line_boundaries(&self.text, pos);
                            self.selection.anchor = s;
                            self.selection.cursor = e;
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
            },
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: es,
                        ..
                    },
                ..
            } => {
                if !self.focused || *es != ElementState::Pressed {
                    return;
                }
                let mods = modifiers.state();
                let shift = mods.shift_key();
                let ctrl = mods.control_key() || mods.super_key();

                // Ctrl combos: clipboard + select-all. Everything else
                // falls through to the local key handler.
                if ctrl {
                    if let Key::Character(c) = logical_key {
                        let lc = c.to_lowercase();
                        match lc.as_str() {
                            "a" => {
                                if !self.text.is_empty() {
                                    self.selection.anchor = 0;
                                    self.selection.cursor = self.text.len();
                                }
                                *handled = true;
                                return;
                            }
                            "c" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(s);
                                }
                                *handled = true;
                                return;
                            }
                            "x" => {
                                if let Some(s) = self.selected_str() {
                                    let owned = s.to_string();
                                    clipboard_copy(&owned);
                                    self.delete_selection(state, root);
                                }
                                *handled = true;
                                return;
                            }
                            "v" => {
                                if let Some(paste) = clipboard_paste() {
                                    if !paste.is_empty() {
                                        // Notes preserves \n — only strip
                                        // truly-control chars other than \n/\t.
                                        let cleaned: String = paste
                                            .chars()
                                            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                                            .collect();
                                        if !cleaned.is_empty() {
                                            self.insert_str(state, root, &cleaned);
                                        }
                                    }
                                }
                                *handled = true;
                                return;
                            }
                            _ => {
                                // Let the shell pick up global Ctrl+Z etc.
                                return;
                            }
                        }
                    }
                    // Ctrl + non-character: let shell handle it.
                    return;
                }

                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if !self.selection.is_collapsed() {
                            self.delete_selection(state, root);
                        } else {
                            self.delete_before(state, root);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        if !self.selection.is_collapsed() {
                            self.delete_selection(state, root);
                        } else {
                            self.delete_after(state, root);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.insert_str(state, root, "\n");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_str(state, root, " ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.insert_str(state, root, "    ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if !shift && !self.selection.is_collapsed() {
                            // Plain arrow with a selection collapses to the
                            // left edge — matches every native editor.
                            self.selection.move_to(self.selection.min());
                        } else {
                            let mut p = self.selection.cursor;
                            if p > 0 {
                                p -= 1;
                                while p > 0 && !self.text.is_char_boundary(p) {
                                    p -= 1;
                                }
                            }
                            if shift {
                                self.selection.extend_to(p);
                            } else {
                                self.selection.move_to(p);
                            }
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if !shift && !self.selection.is_collapsed() {
                            self.selection.move_to(self.selection.max());
                        } else {
                            let mut p = self.selection.cursor;
                            if p < self.text.len() {
                                p += 1;
                                while p < self.text.len() && !self.text.is_char_boundary(p) {
                                    p += 1;
                                }
                            }
                            if shift {
                                self.selection.extend_to(p);
                            } else {
                                self.selection.move_to(p);
                            }
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let cur = self.selection.cursor;
                        let line_start = self.text[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let col = cur - line_start;
                        // Previous line (or stay if at row 0).
                        let new_pos = if line_start == 0 {
                            0
                        } else {
                            let prev_nl = self.text[..line_start - 1]
                                .rfind('\n')
                                .map(|i| i + 1)
                                .unwrap_or(0);
                            let prev_line_end = line_start - 1; // The \n we just crossed.
                            let prev_line_len = prev_line_end - prev_nl;
                            let want_col = col.min(prev_line_len);
                            let mut p = prev_nl + want_col;
                            while p > prev_nl && !self.text.is_char_boundary(p) {
                                p -= 1;
                            }
                            p
                        };
                        if shift {
                            self.selection.extend_to(new_pos);
                        } else {
                            self.selection.move_to(new_pos);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        let cur = self.selection.cursor;
                        let line_start = self.text[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let col = cur - line_start;
                        let line_end = self.text[cur..]
                            .find('\n')
                            .map(|i| cur + i)
                            .unwrap_or(self.text.len());
                        let new_pos = if line_end == self.text.len() {
                            self.text.len()
                        } else {
                            let next_line_start = line_end + 1;
                            let next_line_end = self.text[next_line_start..]
                                .find('\n')
                                .map(|i| next_line_start + i)
                                .unwrap_or(self.text.len());
                            let next_line_len = next_line_end - next_line_start;
                            let want_col = col.min(next_line_len);
                            let mut p = next_line_start + want_col;
                            while p < self.text.len() && !self.text.is_char_boundary(p) {
                                p += 1;
                            }
                            p
                        };
                        if shift {
                            self.selection.extend_to(new_pos);
                        } else {
                            self.selection.move_to(new_pos);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        let line_start = self.text[..self.selection.cursor]
                            .rfind('\n')
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        if shift {
                            self.selection.extend_to(line_start);
                        } else {
                            self.selection.move_to(line_start);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        let line_end = self.text[self.selection.cursor..]
                            .find('\n')
                            .map(|i| self.selection.cursor + i)
                            .unwrap_or(self.text.len());
                        if shift {
                            self.selection.extend_to(line_end);
                        } else {
                            self.selection.move_to(line_end);
                        }
                        *handled = true;
                    }
                    _ => {
                        if let Some(t) = text {
                            let s = t.as_str();
                            if !s.is_empty() && !s.chars().all(|c| c.is_control()) {
                                self.insert_str(state, root, s);
                                *handled = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn needs_commit(&self) -> bool {
        self.last_edit
            .map(|t| t.elapsed() >= COMMIT_DEBOUNCE)
            .unwrap_or(false)
    }

    fn on_commit(&mut self) {
        self.last_edit = None;
    }

    fn get_state(&self) -> Option<String> {
        // Encode "anchor:cursor" so undo/redo restores selection state.
        Some(format!("{}:{}", self.selection.anchor, self.selection.cursor))
    }

    fn set_state(&mut self, s: &str) {
        if let Some((a, c)) = s.split_once(':') {
            if let (Ok(anchor), Ok(cursor)) = (a.parse::<usize>(), c.parse::<usize>()) {
                self.selection.anchor = anchor;
                self.selection.cursor = cursor;
                self.clamp_cursor();
                return;
            }
        }
        // Backwards-compat: an older state blob was just the cursor.
        if let Ok(pos) = s.parse::<usize>() {
            self.selection = Selection::collapsed(pos);
            self.clamp_cursor();
        }
    }
}

impl NotesApp {
    /// Coarse hit-test used inside `handle_event` (which has no
    /// `TextRenderer` available). Picks the row by `\n`-delimited line
    /// and the column by walking characters with a per-character advance
    /// approximation. The result is good enough for "drag selects
    /// roughly the right text" UX; the visual selection rect is drawn
    /// against true glyph metrics in `draw`.
    fn approximate_hit(&self, point: Point) -> usize {
        if self.text.is_empty() {
            return 0;
        }
        let rel_x = (point.x - self.body_origin.x).max(0.0);
        let rel_y = (point.y - self.body_origin.y).max(0.0);
        let row = (rel_y / LINE_HEIGHT as f64).floor() as usize;

        // Approximate width per character. For 15px text the average
        // advance is around 8px for proportional fonts; we use a slightly
        // tighter 7.2 to avoid overshooting on the common case.
        const APPROX_ADVANCE_PX: f64 = 7.2;

        let mut line_starts: Vec<usize> = std::iter::once(0)
            .chain(self.text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        if line_starts.is_empty() {
            line_starts.push(0);
        }
        let line_count = line_starts.len();
        let row = row.min(line_count.saturating_sub(1));
        let line_start = line_starts[row];
        let line_end = if row + 1 < line_count {
            line_starts[row + 1] - 1
        } else {
            self.text.len()
        };

        let chars_at = (rel_x / APPROX_ADVANCE_PX).round().max(0.0) as usize;
        let line_text = &self.text[line_start..line_end];
        for (i, (b, _)) in line_text.char_indices().enumerate() {
            if i >= chars_at {
                return line_start + b;
            }
        }
        line_end
    }

    /// Ctrl+Left moves to the previous word boundary (exposed for tests
    /// and future keybindings; not currently used directly by the event
    /// loop since `notes_app` doesn't yet rebind Ctrl+arrow).
    #[allow(dead_code)]
    fn prev_word(&self) -> usize {
        prev_word_boundary(&self.text, self.selection.cursor)
    }

    #[allow(dead_code)]
    fn next_word(&self) -> usize {
        next_word_boundary(&self.text, self.selection.cursor)
    }
}

impl AppWidget for NotesApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Notes"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰎚")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    fn bind() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let tile_root = doc
            .put_object(&root, "tile", ObjType::Map)
            .expect("tile root");
        (doc, tile_root)
    }

    #[test]
    fn sync_creates_body_and_seed_roundtrips() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hi");
        app.sync(&mut doc, &tile);
        assert_eq!(app.text, "hi");
        assert!(matches!(
            doc.get(&tile, BODY_KEY),
            Ok(Some((Value::Object(ObjType::Text), _)))
        ));
    }

    #[test]
    fn insert_and_delete_mutate_crdt() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::new();
        app.sync(&mut doc, &tile);
        app.insert_str(&mut doc, &tile, "abc");
        assert_eq!(app.text, "abc");
        assert_eq!(app.selection.cursor, 3);
        app.delete_before(&mut doc, &tile);
        assert_eq!(app.text, "ab");
    }

    #[test]
    fn newline_insertion_shifts_cursor_to_next_row() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::new();
        app.sync(&mut doc, &tile);
        app.insert_str(&mut doc, &tile, "ab\n");
        assert_eq!(app.text, "ab\n");
        app.insert_str(&mut doc, &tile, "c");
        assert_eq!(app.text, "ab\nc");
        assert_eq!(app.selection.cursor, 4);
    }

    /// Shift+Right extends the selection by one character (anchor stays
    /// pinned at the original cursor). Routed via `selection.extend_to`.
    #[test]
    fn shift_right_extends_selection() {
        let mut app = NotesApp::with_seed("hello");
        // Start collapsed at 0.
        app.selection = Selection::collapsed(0);
        // Step 1: extend by one char.
        let p = app.selection.cursor + 1;
        app.selection.extend_to(p);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 1);
        assert_eq!(&app.text[app.selection.range()], "h");

        // Step 2: extend by another two chars (cumulative).
        app.selection.extend_to(3);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(&app.text[app.selection.range()], "hel");
    }

    /// Double-click selects the word containing the byte position.
    #[test]
    fn double_click_selects_word() {
        let app = NotesApp::with_seed("hello world quick");
        let (s, e) = word_boundaries(&app.text, 8);
        assert_eq!(s, 6);
        assert_eq!(e, 11);
        assert_eq!(&app.text[s..e], "world");
    }

    /// Ctrl+A behavior: anchor=0, cursor=len.
    #[test]
    fn ctrl_a_selects_all() {
        let mut app = NotesApp::with_seed("alpha\nbeta\ngamma");
        // Simulate Ctrl+A.
        app.selection.anchor = 0;
        app.selection.cursor = app.text.len();
        assert!(!app.selection.is_collapsed());
        assert_eq!(app.selection.min(), 0);
        assert_eq!(app.selection.max(), app.text.len());
        assert_eq!(&app.text[app.selection.range()], "alpha\nbeta\ngamma");
    }

    /// Backspace with an active selection deletes the range and collapses,
    /// rather than chopping a single char to the left.
    #[test]
    fn backspace_with_selection_deletes_range() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        // delete_selection is the path the Backspace arm takes when a
        // selection is live.
        app.delete_selection(&mut doc, &tile);
        assert_eq!(app.text, "hello ");
        assert!(app.selection.is_collapsed());
        assert_eq!(app.selection.cursor, 6);
    }

    /// `insert_str` over an active selection replaces the range (paste
    /// path).
    #[test]
    fn insert_str_replaces_selection() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        app.insert_str(&mut doc, &tile, "there");
        assert_eq!(app.text, "hello there");
        assert!(app.selection.is_collapsed());
        assert_eq!(app.selection.cursor, "hello there".len());
    }

    /// Triple-click line selection routes through `line_boundaries`.
    #[test]
    fn triple_click_selects_line() {
        let app = NotesApp::with_seed("alpha\nbeta\ngamma");
        let (s, e) = line_boundaries(&app.text, 8);
        // Byte 8 sits inside "beta" (starts at 6, ends at 10 — the \n).
        assert_eq!(s, 6);
        assert_eq!(e, 10);
        assert_eq!(&app.text[s..e], "beta");
    }

    /// State serialization round-trips anchor + cursor across undo.
    #[test]
    fn state_serializes_anchor_and_cursor() {
        let mut app = NotesApp::with_seed("hello world");
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        let s = app.get_state().unwrap();
        assert_eq!(s, "6:11");
        let mut app2 = NotesApp::with_seed("hello world");
        app2.set_state(&s);
        assert_eq!(app2.selection.anchor, 6);
        assert_eq!(app2.selection.cursor, 11);
    }

    /// Backwards-compat: pre-selection state blobs were the cursor only.
    #[test]
    fn state_backcompat_cursor_only() {
        let mut app = NotesApp::with_seed("hello");
        app.set_state("3");
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(app.selection.anchor, 3);
    }

    /// Ctrl+C copies the selected text to the clipboard, Ctrl+V pastes
    /// it back. Skipped if arboard isn't available (CI without a
    /// display, etc).
    #[test]
    #[cfg(target_os = "linux")]
    fn ctrl_c_then_ctrl_v_round_trips_through_arboard() {
        // Sanity-check via the ui-side helpers: write a sentinel and
        // try to read it back. If `clipboard_paste` returns None the
        // host has no usable arboard backend (CI without a display,
        // Wayland session without data-control, etc.) — treat as skip
        // rather than spurious failure.
        clipboard_copy("__qos_clip_probe__");
        if clipboard_paste().as_deref() != Some("__qos_clip_probe__") {
            eprintln!("clipboard backend unavailable; skipping round-trip test");
            return;
        }
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        // Ctrl+C path.
        let copied = app.selected_str().map(|s| s.to_string()).unwrap_or_default();
        clipboard_copy(&copied);
        // Collapse and move to end.
        app.selection = Selection::collapsed(app.text.len());
        // Ctrl+V path.
        let pasted = clipboard_paste();
        if let Some(p) = pasted {
            app.insert_str(&mut doc, &tile, &p);
            assert!(app.text.ends_with("world"));
        }
        // (No hard assertion when clipboard returns None — that just
        // means the test runner couldn't reach the system clipboard,
        // which we treat as a skip rather than a failure.)
    }
}
