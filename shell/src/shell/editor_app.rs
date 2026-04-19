//! Editor — a code-leaning sibling to [`super::notes_app::NotesApp`].
//!
//! Same CRDT-backed plaintext store (single Automerge `Text` under
//! [`BODY_KEY`]), same `splice_text` edit path, same caret / typing
//! controls — but rendered as discrete lines with a left-edge line-number
//! gutter, mirroring the concept's `EditorApp.tsx` mock at
//! `nirios-concept/src/components/apps/EditorApp.tsx`.
//!
//! The concept uses the Catppuccin Mocha palette hard-coded into the mock.
//! Per `CLAUDE.md`, shell widgets must read from the OS-controlled [`Theme`]
//! rather than bake in a palette, so we map "gutter / body / keyword /
//! string / accent" onto the existing theme tokens (neutral ramp + accent
//! ramp + semantic colors). The visual cue that separates "Editor" from
//! "Notes" is the gutter + per-line rendering, not a bespoke palette.
//!
//! Syntax coloring is intentionally minimal: a token-level sweep that tints
//! string literals, numbers, and a fixed keyword set. Anything more
//! ambitious (TS/TSX AST, multi-line strings, templating) is out of scope
//! for the parity milestone — the mock itself only shows a frozen snippet.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, Value,
    transaction::Transactable,
};
use quick_ui::presence::{PathSegment, PresenceCtx, PresenceKey};
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Automerge key under the tile's root where the editor body lives.
/// Deliberately distinct from `NotesApp::BODY_KEY` so Notes and Editor
/// tiles hosted in the same shell don't stomp on each other's text.
pub const BODY_KEY: &str = "editor_body";

const PADDING_X: f64 = 20.0;
const PADDING_Y: f64 = 48.0; // leave room for the "Editor" header
const TEXT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = TEXT_SIZE * 1.5;
const GUTTER_WIDTH: f64 = 44.0;
const COMMIT_DEBOUNCE: Duration = Duration::from_millis(800);

/// Default seed used when the CRDT is empty on first boot. A short TSX
/// snippet that exercises every highlight class: keywords, strings,
/// numbers, punctuation.
const DEFAULT_SEED: &str = "\
// EditorApp — CRDT-backed code surface\n\
import { useState } from 'react';\n\
\n\
export function Greeting() {\n\
  const [count, setCount] = useState(0);\n\
  return <p>Clicked {count} times</p>;\n\
}\n";

pub struct EditorApp {
    rect: Rect,
    hovered: bool,
    focused: bool,
    text: String,
    /// Byte offset into `text` for the caret.
    cursor_pos: usize,
    last_edit: Option<Instant>,
    last_mouse: Point,
}

impl EditorApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            text: String::new(),
            cursor_pos: 0,
            last_edit: None,
            last_mouse: Point::ZERO,
        }
    }

    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.text = text.into();
        app.cursor_pos = app.text.len();
        app
    }

    /// Convenience: pre-seed with the default TSX snippet.
    pub fn with_default_snippet() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }

    fn resolve_body(&self, state: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, BODY_KEY) {
            return Some(id);
        }
        state.put_object(root, BODY_KEY, ObjType::Text).ok()
    }

    fn clamp_cursor(&mut self) {
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }
    }

    fn insert_text(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        let Some(body) = self.resolve_body(state, root) else { return };
        let char_idx = self.text[..self.cursor_pos].chars().count();
        self.text.insert_str(self.cursor_pos, s);
        let _ = state.splice_text(&body, char_idx, 0, s);
        self.cursor_pos += s.len();
        self.last_edit = Some(Instant::now());
    }

    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.cursor_pos == 0 {
            return;
        }
        let Some(body) = self.resolve_body(state, root) else { return };
        let mut prev = self.cursor_pos - 1;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        let removed = &self.text[prev..self.cursor_pos];
        let char_idx = self.text[..prev].chars().count();
        let n = removed.chars().count();
        self.text.drain(prev..self.cursor_pos);
        self.cursor_pos = prev;
        let _ = state.splice_text(&body, char_idx, n as isize, "");
        self.last_edit = Some(Instant::now());
    }

    /// Move the caret to the visual start of the current line (the offset
    /// right after the preceding `\n`, or 0 for the first line).
    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    /// Move the caret to the visual end of the current line (the offset of
    /// the next `\n`, or the end of the buffer for the last line).
    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(self.text.len())
    }

    /// Convert a **char-index** (as stored in `PresenceCtx::carets`) to a
    /// byte offset into `self.text`. Returns `self.text.len()` if the char
    /// index walks past the end — peer state that's a bit ahead shouldn't
    /// panic us.
    fn char_index_to_byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Presence key for the editor body. Widget-level path so `text_input`-
    /// style helpers (peer caret / selection) pick it up.
    pub(crate) fn body_presence_key() -> PresenceKey {
        PresenceKey::Widget {
            path: vec![PathSegment::Field(BODY_KEY.into())],
        }
    }
}

impl Default for EditorApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-token color class. Token-level rather than per-char so one
/// `tr.draw_text` call can paint a run rather than glyph-by-glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokClass {
    Plain,
    Keyword,
    Ident,
    StringLit,
    Number,
    Comment,
    Punct,
}

/// A tiny JS/TSX-leaning tokenizer. Recognizes:
///
///   - `//`-to-EOL line comments
///   - `'…'` / `"…"` / `` `…` `` string literals (no interpolation)
///   - Numeric literals (digits, optionally with a dot)
///   - Identifier-keyword hits from a fixed set
///   - Punctuation (everything else non-whitespace)
///
/// Output is a flat list of (start_byte, end_byte, class) triples against
/// the input slice. Whitespace is emitted as `Plain` so the caller can
/// advance the pen without a second pass.
fn tokenize(line: &str) -> Vec<(usize, usize, TokClass)> {
    const KEYWORDS: &[&str] = &[
        "import", "export", "from", "default", "const", "let", "var", "function",
        "return", "if", "else", "for", "while", "do", "break", "continue",
        "switch", "case", "class", "extends", "implements", "interface", "type",
        "enum", "try", "catch", "finally", "throw", "new", "this", "super",
        "async", "await", "yield", "true", "false", "null", "undefined",
        "pub", "fn", "struct", "impl", "mod", "use", "crate", "self", "Self",
        "mut", "match", "where", "trait",
    ];
    let bytes = line.as_bytes();
    let mut out: Vec<(usize, usize, TokClass)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Line comment: '//' … EOL
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            out.push((i, bytes.len(), TokClass::Comment));
            return out;
        }
        // Whitespace: coalesce as Plain so the pen advances the right width.
        if b == b' ' || b == b'\t' {
            let start = i;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            out.push((start, i, TokClass::Plain));
            continue;
        }
        // String literal: ' " ` — consume until matching quote (or EOL).
        if b == b'\'' || b == b'"' || b == b'`' {
            let quote = b;
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                // Skip a single escaped char so `\"` doesn't terminate.
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1; // consume closing quote
            }
            out.push((start, i, TokClass::StringLit));
            continue;
        }
        // Number: optional leading digit, then digits and at most one dot.
        if b.is_ascii_digit() {
            let start = i;
            let mut saw_dot = false;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == b'.' && !saw_dot && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                    saw_dot = true;
                    i += 1;
                } else {
                    break;
                }
            }
            out.push((start, i, TokClass::Number));
            continue;
        }
        // Identifier / keyword.
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                    i += 1;
                } else {
                    break;
                }
            }
            let ident = &line[start..i];
            let class = if KEYWORDS.contains(&ident) {
                TokClass::Keyword
            } else {
                TokClass::Ident
            };
            out.push((start, i, class));
            continue;
        }
        // Fall-through: a single punctuation byte.
        out.push((i, i + 1, TokClass::Punct));
        i += 1;
    }
    out
}

impl Widget for EditorApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        // Foreground on the white window-card: slate-900 for body, slate-500
        // for gutter, slate-400 for comments.
        let body_color = tokens.colors.neutral[1];
        let gutter_color = tokens.colors.neutral[5];
        let gutter_bg = tokens.colors.neutral[9]; // slate-100: a whisper tint
        let keyword_color = tokens.colors.accent[5]; // indigo-500
        let string_color = tokens.colors.semantic.success; // emerald
        let number_color = tokens.colors.semantic.warning; // amber
        let comment_color = tokens.colors.neutral[6];
        let punct_color = tokens.colors.neutral[4];
        let ident_color = tokens.colors.neutral[2];
        let caret_color = tokens.colors.accent[5];

        // Header row: "Editor" title + hint on the right.
        let title_size = tokens.typography.scale.lg;
        let hint_size = tokens.typography.scale.sm;
        tr.draw_text(
            scene,
            "Editor",
            title_size,
            body_color,
            transform * Affine::translate((self.rect.x0 + PADDING_X, self.rect.y0 + 18.0)),
            None,
        );
        let hint = if self.focused {
            "Live-shared code  ·  Ctrl+Z to undo"
        } else {
            "Click to edit"
        };
        let (hw, _) = tr.measure(hint, hint_size, None);
        tr.draw_text(
            scene,
            hint,
            hint_size,
            gutter_color,
            transform
                * Affine::translate((
                    self.rect.x1 - PADDING_X - hw as f64,
                    self.rect.y0 + 18.0 + (title_size - hint_size) as f64 / 2.0,
                )),
            None,
        );

        // Gutter strip (a narrow tinted column behind the line numbers).
        let body_top = self.rect.y0 + PADDING_Y;
        let gutter_rect = Rect::new(
            self.rect.x0 + PADDING_X,
            body_top,
            self.rect.x0 + PADDING_X + GUTTER_WIDTH,
            self.rect.y1,
        );
        scene.fill(Fill::NonZero, transform, gutter_bg, None, &gutter_rect);

        // Lines: iterate `split('\n')` and draw each at its vertical slot.
        // Clip to the tile so a very long body doesn't overflow below the
        // window card's rounded corner.
        let clip_rect = Rect::new(self.rect.x0, body_top, self.rect.x1, self.rect.y1);
        scene.push_layer(
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_rect,
        );

        let content_x = self.rect.x0 + PADDING_X + GUTTER_WIDTH + 10.0;

        if self.text.is_empty() && !self.focused {
            let placeholder =
                "// Empty buffer. Type to begin — edits sync live with collaborators.";
            tr.draw_text(
                scene,
                placeholder,
                TEXT_SIZE,
                comment_color,
                transform * Affine::translate((content_x, body_top + 2.0)),
                None,
            );
            scene.pop_layer();
            return;
        }

        // Running caret position: set if the cursor falls on this line.
        let cursor_line = self.text[..self.cursor_pos].matches('\n').count();
        let cursor_col_prefix: &str = {
            let line_start = self.line_start(self.cursor_pos);
            &self.text[line_start..self.cursor_pos]
        };

        for (row, line) in self.text.split('\n').enumerate() {
            let y = body_top + (row as f64) * LINE_HEIGHT as f64 + 2.0;
            if y > self.rect.y1 {
                break;
            }

            // Line number (right-aligned within the gutter with a 6 px
            // right-pad so digits don't kiss the divider).
            let num = format!("{}", row + 1);
            let (num_w, _) = tr.measure(&num, TEXT_SIZE, None);
            tr.draw_text(
                scene,
                &num,
                TEXT_SIZE,
                gutter_color,
                transform
                    * Affine::translate((
                        self.rect.x0 + PADDING_X + GUTTER_WIDTH - num_w as f64 - 6.0,
                        y,
                    )),
                None,
            );

            // Token spans.
            let tokens_on_line = tokenize(line);
            let mut pen_x = content_x;
            for (start, end, class) in tokens_on_line {
                let slice = &line[start..end];
                let color = match class {
                    TokClass::Plain => body_color,
                    TokClass::Keyword => keyword_color,
                    TokClass::Ident => ident_color,
                    TokClass::StringLit => string_color,
                    TokClass::Number => number_color,
                    TokClass::Comment => comment_color,
                    TokClass::Punct => punct_color,
                };
                if !slice.is_empty() {
                    tr.draw_text(
                        scene,
                        slice,
                        TEXT_SIZE,
                        color,
                        transform * Affine::translate((pen_x, y)),
                        None,
                    );
                    let (w, _) = tr.measure(slice, TEXT_SIZE, None);
                    pen_x += w as f64;
                }
            }

            // Caret on the line that owns the cursor.
            if self.focused && row == cursor_line {
                let (prefix_w, _) = tr.measure(cursor_col_prefix, TEXT_SIZE, None);
                let caret_x = content_x + prefix_w as f64;
                let caret = Rect::new(
                    caret_x,
                    y - 2.0,
                    caret_x + 1.5,
                    y + LINE_HEIGHT as f64 - 2.0,
                );
                scene.fill(Fill::NonZero, transform, caret_color, None, &caret);
            }
        }

        // Thin gutter / body divider rule.
        let divider = Rect::new(
            gutter_rect.x1,
            gutter_rect.y0,
            gutter_rect.x1 + 1.0,
            gutter_rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::rgba8(0, 0, 0, 24),
            None,
            &divider,
        );

        // Peer carets + selections. Read `PresenceCtx` keyed by the editor
        // body and paint a per-peer caret bar + translucent selection fill
        // on top of the locally-rendered text. Char-index is mapped to
        // byte index via `char_indices` so a peer whose caret walks past
        // the text boundary simply lands at end-of-buffer.
        let key = Self::body_presence_key();
        if let Some(carets) = presence.carets.get(&key) {
            for (user, caret_char) in carets.iter() {
                let byte_idx = self.char_index_to_byte(*caret_char);
                let row = self.text[..byte_idx].matches('\n').count();
                let line_start_byte = self.line_start(byte_idx);
                let prefix = &self.text[line_start_byte..byte_idx];
                let (prefix_w, _) = tr.measure(prefix, TEXT_SIZE, None);
                let x = content_x + prefix_w as f64;
                let y = body_top + (row as f64) * LINE_HEIGHT as f64 + 2.0;
                presence.draw_peer_caret(scene, theme, user, (x as f32, y as f32));
            }
        }

        if let Some(selections) = presence.selections.get(&key) {
            for (user, range) in selections.iter() {
                if range.is_empty() {
                    continue;
                }
                // Walk the selection line-by-line: for each line that
                // intersects [range.start, range.end], build a rect from
                // the selection's first column to its last column on that
                // line. Multi-line selections therefore produce one rect
                // per visual row, matching how editors conventionally
                // render per-line selection bands.
                let sel_start_byte = self.char_index_to_byte(range.start);
                let sel_end_byte = self.char_index_to_byte(range.end);
                let mut byte_cursor = sel_start_byte;
                let mut row = self.text[..sel_start_byte].matches('\n').count();
                let mut line_rects: Vec<Rect> = Vec::new();
                while byte_cursor < sel_end_byte {
                    let line_end_byte = self.line_end(byte_cursor).min(sel_end_byte);
                    let line_start_byte = self.line_start(byte_cursor);
                    let pre = &self.text[line_start_byte..byte_cursor];
                    let span = &self.text[byte_cursor..line_end_byte];
                    let (pre_w, _) = tr.measure(pre, TEXT_SIZE, None);
                    let (span_w, _) = tr.measure(span, TEXT_SIZE, None);
                    let x0 = content_x + pre_w as f64;
                    let x1 = x0 + span_w as f64;
                    let y0 = body_top + (row as f64) * LINE_HEIGHT as f64 + 2.0;
                    let y1 = y0 + LINE_HEIGHT as f64;
                    line_rects.push(Rect::new(x0, y0, x1, y1));
                    // Advance to the start of the next line (skip the `\n`).
                    byte_cursor = line_end_byte;
                    if byte_cursor < self.text.len() && self.text.as_bytes()[byte_cursor] == b'\n' {
                        byte_cursor += 1;
                        row += 1;
                    } else {
                        break;
                    }
                }
                if !line_rects.is_empty() {
                    presence.draw_peer_selection(scene, theme, user, range, &line_rects);
                }
            }
        }

        scene.pop_layer();
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
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => {
                if self.hovered && *es == ElementState::Pressed {
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
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
                let has_ctrl = mods.control_key() || mods.super_key();
                if has_ctrl {
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        self.delete_before(state, root);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.insert_text(state, root, "\n");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_text(state, root, " ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.insert_text(state, root, "  ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if self.cursor_pos > 0 {
                            let mut p = self.cursor_pos - 1;
                            while p > 0 && !self.text.is_char_boundary(p) {
                                p -= 1;
                            }
                            self.cursor_pos = p;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if self.cursor_pos < self.text.len() {
                            let mut p = self.cursor_pos + 1;
                            while p < self.text.len() && !self.text.is_char_boundary(p) {
                                p += 1;
                            }
                            self.cursor_pos = p;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        // Move caret up one line, preserving the column
                        // offset where possible. Columns are measured in
                        // bytes within the line — fine for ASCII code and
                        // "close enough" for mixed utf-8 given the clamp.
                        let line_start = self.line_start(self.cursor_pos);
                        let col = self.cursor_pos - line_start;
                        if line_start > 0 {
                            let prev_line_end = line_start - 1; // the `\n`
                            let prev_line_start = self.line_start(prev_line_end);
                            let prev_len = prev_line_end - prev_line_start;
                            let new_pos = prev_line_start + col.min(prev_len);
                            self.cursor_pos = new_pos;
                            self.clamp_cursor();
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        let line_start = self.line_start(self.cursor_pos);
                        let col = self.cursor_pos - line_start;
                        let line_end = self.line_end(self.cursor_pos);
                        if line_end < self.text.len() {
                            let next_line_start = line_end + 1;
                            let next_line_end = self.line_end(next_line_start);
                            let next_len = next_line_end - next_line_start;
                            let new_pos = next_line_start + col.min(next_len);
                            self.cursor_pos = new_pos;
                            self.clamp_cursor();
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::Home) => {
                        self.cursor_pos = self.line_start(self.cursor_pos);
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.cursor_pos = self.line_end(self.cursor_pos);
                        *handled = true;
                    }
                    _ => {
                        if let Some(t) = text {
                            let s = t.as_str();
                            if !s.is_empty() && !s.chars().all(|c| c.is_control()) {
                                self.insert_text(state, root, s);
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
        "EditorApp"
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

    fn needs_commit(&self) -> bool {
        self.last_edit
            .map(|t| t.elapsed() >= COMMIT_DEBOUNCE)
            .unwrap_or(false)
    }

    fn on_commit(&mut self) {
        self.last_edit = None;
    }

    fn get_state(&self) -> Option<String> {
        Some(self.cursor_pos.to_string())
    }

    fn set_state(&mut self, s: &str) {
        if let Ok(pos) = s.parse::<usize>() {
            self.cursor_pos = pos;
            self.clamp_cursor();
        }
    }
}

impl AppWidget for EditorApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Editor"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰈙")
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
    fn tokenize_separates_keyword_string_number() {
        let toks = tokenize("const count = 0; return 'hi';");
        // "const" at offset 0 is a Keyword
        let has_keyword = toks
            .iter()
            .any(|(s, e, c)| *c == TokClass::Keyword && "const" == &"const count = 0; return 'hi';"[*s..*e]);
        assert!(has_keyword, "expected `const` to tokenize as Keyword");
        let has_string = toks.iter().any(|(_, _, c)| *c == TokClass::StringLit);
        assert!(has_string, "expected `'hi'` to tokenize as StringLit");
        let has_number = toks.iter().any(|(_, _, c)| *c == TokClass::Number);
        assert!(has_number, "expected `0` to tokenize as Number");
    }

    #[test]
    fn tokenize_line_comment_swallows_rest_of_line() {
        let toks = tokenize("let x = 1; // trailing");
        assert!(toks.iter().any(|(_, _, c)| *c == TokClass::Comment));
        let last = toks.last().unwrap();
        assert_eq!(last.2, TokClass::Comment);
    }

    #[test]
    fn sync_creates_body_and_seed_roundtrips() {
        let (mut doc, tile) = bind();
        let mut app = EditorApp::with_seed("fn main() {}\n");
        app.sync(&mut doc, &tile);
        assert_eq!(app.text, "fn main() {}\n");
        assert!(matches!(
            doc.get(&tile, BODY_KEY),
            Ok(Some((Value::Object(ObjType::Text), _)))
        ));
    }

    #[test]
    fn insert_and_delete_mutate_crdt() {
        let (mut doc, tile) = bind();
        let mut app = EditorApp::new();
        app.sync(&mut doc, &tile);
        app.insert_text(&mut doc, &tile, "abc");
        assert_eq!(app.text, "abc");
        assert_eq!(app.cursor_pos, 3);
        app.delete_before(&mut doc, &tile);
        assert_eq!(app.text, "ab");
    }

    #[test]
    fn body_presence_key_is_widget_with_editor_body_field() {
        // Stable contract: the shell seeds presence state under this
        // exact key, so a peer's caret/selection for the editor body
        // must materialise as `Widget { [Field("editor_body")] }`.
        let key = EditorApp::body_presence_key();
        match key {
            PresenceKey::Widget { path } => {
                assert_eq!(path.len(), 1);
                match &path[0] {
                    PathSegment::Field(f) => assert_eq!(f, BODY_KEY),
                    _ => panic!("expected Field segment"),
                }
            }
            _ => panic!("expected PresenceKey::Widget"),
        }
    }

    #[test]
    fn char_index_to_byte_handles_multibyte_and_oob() {
        // The body is "aβc" where `β` is a 2-byte UTF-8 scalar.
        let app = EditorApp::with_seed("aβc");
        assert_eq!(app.char_index_to_byte(0), 0);
        assert_eq!(app.char_index_to_byte(1), 1);
        assert_eq!(app.char_index_to_byte(2), 3); // past `β`
        // Walk past the end clamps to text.len() rather than panicking.
        assert_eq!(app.char_index_to_byte(99), app.text.len());
    }

    #[test]
    fn arrow_down_preserves_column_across_lines() {
        // Manual smoke: verify that line_start / line_end work as expected
        // on a two-line buffer. We drive the caret arithmetic that the
        // handle_event ArrowDown branch would perform.
        let app = EditorApp::with_seed("abcd\nefgh");
        // Cursor at col 2 of line 0 → byte offset 2.
        let pos = 2;
        let line_start = app.line_start(pos);
        assert_eq!(line_start, 0);
        let col = pos - line_start;
        let line_end = app.line_end(pos);
        assert_eq!(line_end, 4);
        let next_line_start = line_end + 1; // 5
        let next_line_end = app.line_end(next_line_start);
        assert_eq!(next_line_end, 9);
        let next_len = next_line_end - next_line_start;
        let new_pos = next_line_start + col.min(next_len);
        assert_eq!(new_pos, 7); // line "efgh", column 2 → 'g'
    }
}
