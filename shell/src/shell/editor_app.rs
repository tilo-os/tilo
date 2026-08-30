//! Editor — a code-leaning sibling to [`super::notes_app::NotesApp`].
//!
//! Both sit on the same [`CrdtTextBody`]; what makes this one the *editor*
//! is entirely in `draw` — discrete lines behind a line-number gutter and a
//! token-level syntax sweep.
//!
//! Colours read from the OS-controlled [`Theme`] rather than a bespoke
//! palette, so "gutter / body / keyword / string / accent" map onto
//! existing neutral/accent/semantic tokens.
//!
//! Syntax coloring is intentionally minimal: string literals, numbers, and
//! a fixed keyword set. A real AST, multi-line strings, or templating are
//! out of scope.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::line_boundaries;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

use crate::shell::crdt_text::{CrdtTextBody, full_bleed_leaf, full_bleed_rect};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Automerge key under the tile's root where the editor body lives.
/// Distinct from `NotesApp::BODY_KEY` so Notes and Editor tiles hosted in
/// the same shell don't stomp on each other's text.
pub const BODY_KEY: &str = "editor_body";

const PADDING_X: f64 = 20.0;
const PADDING_Y: f64 = 48.0; // leave room for the "Editor" header
const TEXT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = TEXT_SIZE * 1.5;
const GUTTER_WIDTH: f64 = 44.0;

/// `transform * Affine::translate(xy)`, used at every glyph draw site
/// below. Vello's `Affine::mul` is plain matrix composition with no
/// side effects, but it is a custom `Mul` impl rather than primitive
/// integer arithmetic, which is what trips the lint at each call site.
fn at(transform: Affine, xy: (f64, f64)) -> Affine {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine::mul is pure matrix composition (translate here), not primitive integer arithmetic that can silently wrap"
    )]
    {
        transform * Affine::translate(xy)
    }
}

/// Widens a small row/column count to `f64` for draw-time geometry.
fn count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded well under 2^53 by every caller (a document row/column count)"
    )]
    {
        n as f64
    }
}

/// First-boot seed: a short TSX snippet exercising every highlight class.
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
    /// CRDT-backed body plus the caret arithmetic over it.
    body: CrdtTextBody,
    /// Byte offset into `body.text` for the caret.
    cursor_pos: usize,
    last_mouse: Point,
}

impl EditorApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            body: CrdtTextBody::new(BODY_KEY),
            cursor_pos: 0,
            last_mouse: Point::ZERO,
        }
    }

    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.body = CrdtTextBody::with_seed(BODY_KEY, text);
        app.cursor_pos = app.body.text.len();
        app
    }

    pub fn with_default_snippet() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }

    fn clamp_cursor(&mut self) {
        self.cursor_pos = self.body.clamp(self.cursor_pos);
    }

    fn insert_text(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        let at = self.cursor_pos;
        self.cursor_pos = self.body.splice(state, root, at..at, s);
    }

    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let cur = self.cursor_pos;
        if cur == 0 {
            return;
        }
        let prev = self.body.prev_boundary(cur);
        self.cursor_pos = self.body.splice(state, root, prev..cur, "");
    }

    /// Byte offset of the start of the line `pos` sits on.
    fn line_start(&self, pos: usize) -> usize {
        line_boundaries(&self.body.text, pos).0
    }

    /// Byte offset of the end of the line `pos` sits on (next `\n`, or EOF).
    fn line_end(&self, pos: usize) -> usize {
        line_boundaries(&self.body.text, pos).1
    }

    /// Steps the caret one grapheme left. Returns whether it moved.
    fn move_left(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        self.cursor_pos = self.body.prev_boundary(self.cursor_pos);
        true
    }

    /// Steps the caret one grapheme right. Returns whether it moved.
    fn move_right(&mut self) -> bool {
        if self.cursor_pos >= self.body.text.len() {
            return false;
        }
        self.cursor_pos = self.body.next_boundary(self.cursor_pos);
        true
    }

    /// Same column, previous line. Returns whether it moved. Columns are
    /// byte offsets within the line — fine for ASCII, "close enough" for
    /// mixed UTF-8 given the clamp.
    fn move_line_up(&mut self) -> bool {
        let line_start = self.line_start(self.cursor_pos);
        if line_start == 0 {
            return false;
        }
        let col = self.cursor_pos.saturating_sub(line_start);
        let prev_line_end = line_start.saturating_sub(1); // the `\n`
        let prev_line_start = self.line_start(prev_line_end);
        let prev_len = prev_line_end.saturating_sub(prev_line_start);
        self.cursor_pos = prev_line_start.saturating_add(col.min(prev_len));
        self.clamp_cursor();
        true
    }

    /// Same column, next line. Returns whether it moved.
    fn move_line_down(&mut self) -> bool {
        let line_start = self.line_start(self.cursor_pos);
        let col = self.cursor_pos.saturating_sub(line_start);
        let line_end = self.line_end(self.cursor_pos);
        if line_end >= self.body.text.len() {
            return false;
        }
        let next_line_start = line_end.saturating_add(1);
        let next_line_end = self.line_end(next_line_start);
        let next_len = next_line_end.saturating_sub(next_line_start);
        self.cursor_pos = next_line_start.saturating_add(col.min(next_len));
        self.clamp_cursor();
        true
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

/// Recognised keyword set for [`tokenize`]. Module-level so the (large,
/// static) list doesn't count against the tokenizer function's own
/// line budget.
const KEYWORDS: &[&str] = &[
    "import",
    "export",
    "from",
    "default",
    "const",
    "let",
    "var",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "break",
    "continue",
    "switch",
    "case",
    "class",
    "extends",
    "implements",
    "interface",
    "type",
    "enum",
    "try",
    "catch",
    "finally",
    "throw",
    "new",
    "this",
    "super",
    "async",
    "await",
    "yield",
    "true",
    "false",
    "null",
    "undefined",
    "pub",
    "fn",
    "struct",
    "impl",
    "mod",
    "use",
    "crate",
    "self",
    "Self",
    "mut",
    "match",
    "where",
    "trait",
];

/// A tiny JS/TSX-leaning tokenizer: line comments, string literals (no
/// interpolation), numeric literals, keyword/identifier, punctuation.
/// Output is a flat list of (`start_byte`, `end_byte`, class) triples;
/// whitespace is `Plain` so the caller advances the pen in one pass.
fn tokenize(line: &str) -> Vec<(usize, usize, TokClass)> {
    let bytes = line.as_bytes();
    let mut out: Vec<(usize, usize, TokClass)> = Vec::new();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'/' && bytes.get(i.saturating_add(1)) == Some(&b'/') {
            out.push((i, bytes.len(), TokClass::Comment));
            return out;
        }
        // Whitespace: coalesce as Plain so the pen advances the right width.
        if b == b' ' || b == b'\t' {
            let end = scan_whitespace(bytes, i);
            out.push((i, end, TokClass::Plain));
            i = end;
            continue;
        }
        if b == b'\'' || b == b'"' || b == b'`' {
            let end = scan_string_lit(bytes, i, b);
            out.push((i, end, TokClass::StringLit));
            i = end;
            continue;
        }
        if b.is_ascii_digit() {
            let end = scan_number(bytes, i);
            out.push((i, end, TokClass::Number));
            i = end;
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            let end = scan_ident(bytes, i);
            let ident = line.get(i..end).unwrap_or("");
            let class = if KEYWORDS.contains(&ident) {
                TokClass::Keyword
            } else {
                TokClass::Ident
            };
            out.push((i, end, class));
            i = end;
            continue;
        }
        out.push((i, i.saturating_add(1), TokClass::Punct));
        i = i.saturating_add(1);
    }
    out
}

/// Scans a whitespace run starting at `start`. Returns the end offset.
fn scan_whitespace(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i = i.saturating_add(1);
    }
    i
}

/// Scans a quoted string literal starting at `start` (the opening
/// `quote` byte). Returns the end offset, past the closing quote when
/// found.
fn scan_string_lit(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start.saturating_add(1);
    while let Some(&b) = bytes.get(i) {
        if b == quote {
            break;
        }
        // Skip a single escaped char so `\"` doesn't terminate.
        if b == b'\\' && bytes.get(i.saturating_add(1)).is_some() {
            i = i.saturating_add(2);
        } else {
            i = i.saturating_add(1);
        }
    }
    if bytes.get(i).is_some() {
        i = i.saturating_add(1); // consume closing quote
    }
    i
}

/// Scans a numeric literal (digits with at most one `.`) starting at
/// `start`. Returns the end offset.
fn scan_number(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    let mut saw_dot = false;
    while let Some(&c) = bytes.get(i) {
        if c.is_ascii_digit() {
            i = i.saturating_add(1);
        } else if c == b'.'
            && !saw_dot
            && matches!(bytes.get(i.saturating_add(1)), Some(d) if d.is_ascii_digit())
        {
            saw_dot = true;
            i = i.saturating_add(1);
        } else {
            break;
        }
    }
    i
}

/// Scans an identifier/keyword run starting at `start`. Returns the end
/// offset.
fn scan_ident(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while matches!(bytes.get(i), Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$') {
        i = i.saturating_add(1);
    }
    i
}

/// Colours resolved from the theme once per frame and threaded through
/// `draw`'s helpers, so the palette itself doesn't blow out their
/// argument counts.
struct EditorPalette {
    body: Color,
    gutter: Color,
    gutter_bg: Color,
    keyword: Color,
    string: Color,
    number: Color,
    comment: Color,
    punct: Color,
    ident: Color,
    caret: Color,
}

impl EditorPalette {
    fn from_theme(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        // No background of its own, so every neutral here is ink *on the
        // window card* — hence `Theme::on_card` rather than fixed ramp slots.
        let ink = theme.on_card();
        Self {
            body: ink.ink,
            gutter: ink.ink_dim,
            gutter_bg: ink.tint, // a whisper tint of the card, either way round
            keyword: tokens.colors.accent[5], // indigo-500
            string: tokens.colors.semantic.success, // emerald
            number: tokens.colors.semantic.warning, // amber
            comment: ink.ink_tertiary,
            punct: ink.ink_dim,
            ident: ink.ink,
            caret: tokens.colors.accent[5],
        }
    }
}

impl EditorApp {
    /// "Editor" title + the focus hint on the right.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        palette: &EditorPalette,
    ) {
        let tokens = &theme.tokens;
        let title_size = tokens.typography.scale.lg;
        let hint_size = tokens.typography.scale.sm;
        tr.draw_text(
            scene,
            "Editor",
            title_size,
            palette.body,
            at(transform, (self.rect.x0 + PADDING_X, self.rect.y0 + 18.0)),
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
            palette.gutter,
            at(
                transform,
                (
                    self.rect.x1 - PADDING_X - f64::from(hw),
                    self.rect.y0 + 18.0 + f64::from(title_size - hint_size) / 2.0,
                ),
            ),
            None,
        );
    }

    /// Gutter line numbers, tokenized body text, and the local caret —
    /// one visual row per source line.
    fn draw_lines(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        palette: &EditorPalette,
        body_top: f64,
        content_x: f64,
    ) {
        // Running caret position: set if the cursor falls on this line.
        let cursor_line = self
            .body
            .text
            .get(..self.cursor_pos)
            .map_or(0, |s| s.matches('\n').count());
        let cursor_col_prefix: &str = {
            let line_start = self.line_start(self.cursor_pos);
            self.body
                .text
                .get(line_start..self.cursor_pos)
                .unwrap_or("")
        };

        for (row, line) in self.body.text.split('\n').enumerate() {
            let y = body_top + count_to_f64(row) * f64::from(LINE_HEIGHT) + 2.0;
            if y > self.rect.y1 {
                break;
            }

            // 6px right-pad so digits don't kiss the divider.
            let num = format!("{}", row.saturating_add(1));
            let (num_w, _) = tr.measure(&num, TEXT_SIZE, None);
            tr.draw_text(
                scene,
                &num,
                TEXT_SIZE,
                palette.gutter,
                at(
                    transform,
                    (
                        self.rect.x0 + PADDING_X + GUTTER_WIDTH - f64::from(num_w) - 6.0,
                        y,
                    ),
                ),
                None,
            );

            let tokens_on_line = tokenize(line);
            let mut pen_x = content_x;
            for (start, end, class) in tokens_on_line {
                let slice = line.get(start..end).unwrap_or("");
                let color = match class {
                    TokClass::Plain => palette.body,
                    TokClass::Keyword => palette.keyword,
                    TokClass::Ident => palette.ident,
                    TokClass::StringLit => palette.string,
                    TokClass::Number => palette.number,
                    TokClass::Comment => palette.comment,
                    TokClass::Punct => palette.punct,
                };
                if !slice.is_empty() {
                    tr.draw_text(
                        scene,
                        slice,
                        TEXT_SIZE,
                        color,
                        at(transform, (pen_x, y)),
                        None,
                    );
                    let (w, _) = tr.measure(slice, TEXT_SIZE, None);
                    pen_x += f64::from(w);
                }
            }

            if self.focused && row == cursor_line {
                let (prefix_w, _) = tr.measure(cursor_col_prefix, TEXT_SIZE, None);
                let caret_x = content_x + f64::from(prefix_w);
                let caret = Rect::new(
                    caret_x,
                    y - 2.0,
                    caret_x + 1.5,
                    y + f64::from(LINE_HEIGHT) - 2.0,
                );
                scene.fill(Fill::NonZero, transform, palette.caret, None, &caret);
            }
        }
    }

    /// Char-index maps to byte index so a peer caret past the text
    /// boundary simply lands at end-of-buffer. `origin` is `(body_top,
    /// content_x)`.
    fn draw_peer_carets(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        origin: (f64, f64),
    ) {
        let (body_top, content_x) = origin;
        let key = self.body.presence_key();
        let Some(carets) = presence.carets.get(&key) else {
            return;
        };
        for (user, caret_char) in carets {
            let byte_idx = self.body.char_index_to_byte(*caret_char);
            let row = self
                .body
                .text
                .get(..byte_idx)
                .map_or(0, |s| s.matches('\n').count());
            let line_start_byte = self.line_start(byte_idx);
            let prefix = self.body.text.get(line_start_byte..byte_idx).unwrap_or("");
            let (prefix_w, _) = tr.measure(prefix, TEXT_SIZE, None);
            let x = content_x + f64::from(prefix_w);
            let y = body_top + count_to_f64(row) * f64::from(LINE_HEIGHT) + 2.0;
            // 2px wide, full line height — matches the local caret in
            // `TextInput` / `RichTextLine`.
            let caret_rect = Rect::new(x, y, x + 2.0, y + f64::from(LINE_HEIGHT));
            presence.draw_peer_caret(scene, theme, transform, user, caret_rect);
        }
    }

    /// `origin` is `(body_top, content_x)`.
    fn draw_peer_selections(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        origin: (f64, f64),
    ) {
        let key = self.body.presence_key();
        let Some(selections) = presence.selections.get(&key) else {
            return;
        };
        for (user, range) in selections {
            if range.is_empty() {
                continue;
            }
            let line_rects = self.peer_selection_line_rects(range, origin, tr);
            // One call per visual line — `draw_peer_selection` paints a
            // single `Rect`.
            for line_rect in &line_rects {
                presence.draw_peer_selection(scene, theme, transform, user, *line_rect);
            }
        }
    }

    /// Walk line-by-line, one rect per visual row (the conventional
    /// per-line selection band), for a peer's char-index selection
    /// range. `origin` is `(body_top, content_x)`.
    fn peer_selection_line_rects(
        &self,
        range: &ui::presence::SelectionRange,
        origin: (f64, f64),
        tr: &mut TextRenderer,
    ) -> Vec<Rect> {
        let (body_top, content_x) = origin;
        let sel_start_byte = self.body.char_index_to_byte(range.start);
        let sel_end_byte = self.body.char_index_to_byte(range.end);
        let mut byte_cursor = sel_start_byte;
        let mut row = self
            .body
            .text
            .get(..sel_start_byte)
            .map_or(0, |s| s.matches('\n').count());
        let mut line_rects: Vec<Rect> = Vec::new();
        while byte_cursor < sel_end_byte {
            let line_end_byte = self.line_end(byte_cursor).min(sel_end_byte);
            let line_start_byte = self.line_start(byte_cursor);
            let pre = self
                .body
                .text
                .get(line_start_byte..byte_cursor)
                .unwrap_or("");
            let span = self.body.text.get(byte_cursor..line_end_byte).unwrap_or("");
            let (pre_w, _) = tr.measure(pre, TEXT_SIZE, None);
            let (span_w, _) = tr.measure(span, TEXT_SIZE, None);
            let x0 = content_x + f64::from(pre_w);
            let x1 = x0 + f64::from(span_w);
            let y0 = body_top + count_to_f64(row) * f64::from(LINE_HEIGHT) + 2.0;
            let y1 = y0 + f64::from(LINE_HEIGHT);
            line_rects.push(Rect::new(x0, y0, x1, y1));
            byte_cursor = line_end_byte;
            if self.body.text.as_bytes().get(byte_cursor) == Some(&b'\n') {
                byte_cursor = byte_cursor.saturating_add(1);
                row = row.saturating_add(1);
            } else {
                break;
            }
        }
        line_rects
    }
}

impl Widget for EditorApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.body.sync(state, root);
        self.clamp_cursor();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        full_bleed_leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = full_bleed_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let palette = EditorPalette::from_theme(theme);
        self.draw_header(scene, transform, tr, theme, &palette);

        let body_top = self.rect.y0 + PADDING_Y;
        let gutter_rect = Rect::new(
            self.rect.x0 + PADDING_X,
            body_top,
            self.rect.x0 + PADDING_X + GUTTER_WIDTH,
            self.rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            palette.gutter_bg,
            None,
            &gutter_rect,
        );

        // Clip so a very long body doesn't overflow the card's rounded corner.
        let clip_rect = Rect::new(self.rect.x0, body_top, self.rect.x1, self.rect.y1);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip_rect,
        );

        let content_x = self.rect.x0 + PADDING_X + GUTTER_WIDTH + 10.0;

        if self.body.text.is_empty() && !self.focused {
            let placeholder =
                "// Empty buffer. Type to begin — edits sync live with collaborators.";
            tr.draw_text(
                scene,
                placeholder,
                TEXT_SIZE,
                palette.comment,
                at(transform, (content_x, body_top + 2.0)),
                None,
            );
            scene.pop_layer();
            return;
        }

        self.draw_lines(scene, transform, tr, &palette, body_top, content_x);

        let divider = Rect::new(
            gutter_rect.x1,
            gutter_rect.y0,
            gutter_rect.x1 + 1.0,
            gutter_rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 24),
            None,
            &divider,
        );

        let origin = (body_top, content_x);
        self.draw_peer_carets(scene, transform, tr, theme, presence, origin);
        self.draw_peer_selections(scene, transform, tr, theme, presence, origin);

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
                event: key_event, ..
            } => {
                self.handle_keyboard_input(key_event, modifiers, state, root, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "EditorApp"
    }

    /// Folded into the body's presence key so a peer's caret for one tab
    /// isn't drawn in another tab of the same app.
    fn set_record_scope(&mut self, record: &ui::record::RecordId) {
        self.body.set_record(record);
    }

    fn presence_key(&self) -> Option<ui::presence::PresenceKey> {
        Some(self.body.presence_key())
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
        self.body.needs_commit()
    }

    fn on_commit(&mut self) {
        self.body.on_commit();
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

impl EditorApp {
    fn handle_keyboard_input(
        &mut self,
        key_event: &KeyEvent,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        if !self.focused || key_event.state != ElementState::Pressed {
            return;
        }
        let mods = modifiers.state();
        if mods.control_key() || mods.super_key() {
            return;
        }
        match &key_event.logical_key {
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
                if self.move_left() {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.move_right() {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.move_line_up() {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if self.move_line_down() {
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
                if let Some(s) = key_event.text.as_deref()
                    && !s.is_empty()
                    && !s.chars().all(char::is_control)
                {
                    self.insert_text(state, root, s);
                    *handled = true;
                }
            }
        }
    }
}

impl AppWidget for EditorApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
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
    use automerge::{ObjType, ReadDoc, Value, transaction::Transactable};

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
        let has_keyword = toks.iter().any(|(s, e, c)| {
            *c == TokClass::Keyword && "const count = 0; return 'hi';".get(*s..*e) == Some("const")
        });
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
        assert_eq!(app.body.text, "fn main() {}\n");
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
        assert_eq!(app.body.text, "abc");
        assert_eq!(app.cursor_pos, 3);
        app.delete_before(&mut doc, &tile);
        assert_eq!(app.body.text, "ab");
    }

    #[test]
    fn body_presence_key_names_the_record_and_the_body_field() {
        use ui::presence::{PathSegment, PresenceKey};
        // Stable contract: the shell seeds presence under this exact key,
        // so a peer's caret/selection materialises where `draw` looks.
        // Unhosted — no record named yet — it is the bare field.
        let mut app = EditorApp::new();
        let key = app
            .presence_key()
            .expect("the editor body publishes presence");
        match key {
            PresenceKey::Widget { path } => {
                assert_eq!(path, vec![PathSegment::Field(BODY_KEY.into())]);
            }
            other => panic!("expected PresenceKey::Widget, got {other:?}"),
        }

        // Hosted in a tab, the record comes first — otherwise two Editor
        // tabs would share a key and a peer's caret would draw in whichever
        // tab painted last.
        let record = ui::record::RecordId::for_tab(0x2b);
        app.set_record_scope(&record);
        match app.presence_key().expect("still publishes") {
            PresenceKey::Widget { path } => {
                assert_eq!(
                    path,
                    vec![
                        PathSegment::Field(record.as_str().into()),
                        PathSegment::Field(BODY_KEY.into()),
                    ],
                );
            }
            other => panic!("expected PresenceKey::Widget, got {other:?}"),
        }
    }

    #[test]
    fn char_index_to_byte_handles_multibyte_and_oob() {
        // The body is "aβc" where `β` is a 2-byte UTF-8 scalar.
        let app = EditorApp::with_seed("aβc");
        assert_eq!(app.body.char_index_to_byte(0), 0);
        assert_eq!(app.body.char_index_to_byte(1), 1);
        assert_eq!(app.body.char_index_to_byte(2), 3); // past `β`
        // Walk past the end clamps to text.len() rather than panicking.
        assert_eq!(app.body.char_index_to_byte(99), app.body.text.len());
    }

    #[test]
    fn arrow_down_preserves_column_across_lines() {
        // Drives the caret arithmetic the handle_event ArrowDown branch performs.
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
