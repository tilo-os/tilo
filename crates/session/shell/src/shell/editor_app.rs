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
use std::path::PathBuf;
use ui::widget::{AppModel, ModelPath};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use taffy::{NodeId, TaffyTree};
use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_edit::{Edit, EditCore, Motion};
use ui::text_render::TextRenderer;
use ui::text_selection::{clipboard_copy, clipboard_paste, line_boundaries};
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
const DISK_BASE_KEY: &str = "editor_disk_base";

const PADDING_X: f64 = 20.0;
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
    /// Shared caret/selection model. Byte offsets are applied to the CRDT
    /// through [`CrdtTextBody::splice`].
    edit: EditCore,
    last_mouse: Point,
    source: Option<SourceDocument>,
    disk_conflict: Option<String>,
    disk_observation_pending: bool,
}

struct SourceDocument {
    path: PathBuf,
    provider_id: &'static str,
    disk_text: String,
}

impl EditorApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            body: CrdtTextBody::new(BODY_KEY),
            edit: EditCore::new(),
            last_mouse: Point::ZERO,
            source: None,
            disk_conflict: None,
            disk_observation_pending: false,
        }
    }

    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.body = CrdtTextBody::with_seed(BODY_KEY, text);
        app.edit.set_caret(app.body.text.len());
        app
    }

    pub fn with_default_snippet() -> Self {
        Self::with_seed(DEFAULT_SEED)
    }

    pub fn for_file(path: PathBuf, provider_id: &'static str, text: String) -> Self {
        let mut app = Self::with_seed(text.clone());
        app.source = Some(SourceDocument {
            path,
            provider_id,
            disk_text: text,
        });
        app.disk_observation_pending = true;
        app
    }

    fn clamp_cursor(&mut self) {
        self.edit.clamp_to(&self.body.text);
    }

    fn reconcile_disk(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.disk_observation_pending {
            return;
        }
        self.disk_observation_pending = false;
        let Some(source) = &self.source else {
            return;
        };
        let disk_text = source.disk_text.clone();
        let base =
            state
                .get(root, DISK_BASE_KEY)
                .ok()
                .flatten()
                .and_then(|(value, _)| match value {
                    Value::Scalar(value) => match value.as_ref() {
                        ScalarValue::Str(text) => Some(text.to_string()),
                        _ => None,
                    },
                    Value::Object(_) => None,
                });
        match base {
            None => {
                let _ = state.put(root, DISK_BASE_KEY, disk_text);
                self.disk_conflict = None;
            }
            Some(base) if disk_text == base => {
                self.disk_conflict = None;
            }
            Some(base) if self.body.text == base => {
                let end = self.body.text.len();
                let caret = self.body.splice(state, root, 0..end, &disk_text);
                self.edit.set_caret(caret);
                let _ = state.put(root, DISK_BASE_KEY, disk_text);
                self.disk_conflict = None;
            }
            Some(_) => {
                self.disk_conflict = Some(crate::shell::i18n::t("editor-disk-conflict"));
            }
        }
    }

    fn insert_text(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        let edit = self.edit.insert(s);
        self.apply_edit(state, root, &edit);
    }

    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let edit = self.edit.delete_backward(&self.body.text, false);
        self.apply_edit(state, root, &edit);
    }

    fn delete_word_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let edit = self.edit.delete_backward(&self.body.text, true);
        self.apply_edit(state, root, &edit);
    }

    fn delete_word_after(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let edit = self.edit.delete_forward(&self.body.text, true);
        self.apply_edit(state, root, &edit);
    }

    fn apply_edit(&mut self, state: &mut AutoCommit, root: &ObjId, edit: &Edit) {
        if edit.is_noop() {
            return;
        }
        let caret = self.body.splice(
            state,
            root,
            edit.at..edit.at.saturating_add(edit.removed),
            &edit.inserted,
        );
        self.edit.set_caret(caret);
    }

    /// Byte offset of the start of the line `pos` sits on.
    fn line_start(&self, pos: usize) -> usize {
        line_boundaries(&self.body.text, pos).0
    }

    /// Byte offset of the end of the line `pos` sits on (next `\n`, or EOF).
    fn line_end(&self, pos: usize) -> usize {
        line_boundaries(&self.body.text, pos).1
    }

    /// Same column, previous line. Returns whether it moved. Columns are
    /// byte offsets within the line — fine for ASCII, "close enough" for
    /// mixed UTF-8 given the clamp.
    fn move_line_up(&mut self, extend: bool) -> bool {
        let line_start = self.line_start(self.edit.caret());
        if line_start == 0 {
            return false;
        }
        let col = self.edit.caret().saturating_sub(line_start);
        let prev_line_end = line_start.saturating_sub(1); // the `\n`
        let prev_line_start = self.line_start(prev_line_end);
        let prev_len = prev_line_end.saturating_sub(prev_line_start);
        let target = prev_line_start.saturating_add(col.min(prev_len));
        if extend {
            let anchor = self.edit.selection().anchor;
            self.edit.set_selection(anchor, target);
        } else {
            self.edit.set_caret(target);
        }
        self.clamp_cursor();
        true
    }

    /// Same column, next line. Returns whether it moved.
    fn move_line_down(&mut self, extend: bool) -> bool {
        let line_start = self.line_start(self.edit.caret());
        let col = self.edit.caret().saturating_sub(line_start);
        let line_end = self.line_end(self.edit.caret());
        if line_end >= self.body.text.len() {
            return false;
        }
        let next_line_start = line_end.saturating_add(1);
        let next_line_end = self.line_end(next_line_start);
        let next_len = next_line_end.saturating_sub(next_line_start);
        let target = next_line_start.saturating_add(col.min(next_len));
        if extend {
            let anchor = self.edit.selection().anchor;
            self.edit.set_selection(anchor, target);
        } else {
            self.edit.set_caret(target);
        }
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
    selection: Color,
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
            selection: tokens.colors.accent[5].with_alpha_factor(0.22),
        }
    }
}

impl EditorApp {
    fn line_height(tr: &TextRenderer) -> f64 {
        f64::from(LINE_HEIGHT * tr.text_scale())
    }

    fn body_offset(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let (_, title_height) = tr.measure(
            "Editor",
            theme.tokens.typography.scale.lg / theme.text_scale,
            None,
        );
        let (_, hint_height) = tr.measure(
            "Ag",
            theme.tokens.typography.scale.sm / theme.text_scale,
            None,
        );
        18.0 + f64::from(title_height + hint_height) + 12.0
    }

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
        let title_size = tokens.typography.scale.lg / theme.text_scale;
        let hint_size = tokens.typography.scale.sm / theme.text_scale;
        tr.draw_text(
            scene,
            "Editor",
            title_size,
            palette.body,
            at(transform, (self.rect.x0 + PADDING_X, self.rect.y0 + 18.0)),
            None,
        );
        let hint = crate::shell::i18n::t(if self.disk_conflict.is_some() {
            "editor-hint-disk-conflict"
        } else if self.focused {
            "editor-hint-focused"
        } else {
            "editor-hint-click"
        });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a non-negative on-screen width in logical pixels; f32 holds it with sub-pixel precision"
        )]
        let hint = tr.elide_to_width(
            &hint,
            hint_size,
            (self.rect.width() - 2.0 * PADDING_X).max(0.0) as f32,
        );
        let (_, title_height) = tr.measure("Editor", title_size, None);
        tr.draw_text(
            scene,
            &hint,
            hint_size,
            if self.disk_conflict.is_some() {
                theme.tokens.colors.semantic.warning
            } else {
                palette.gutter
            },
            at(
                transform,
                (
                    self.rect.x0 + PADDING_X,
                    self.rect.y0 + 22.0 + f64::from(title_height),
                ),
            ),
            None,
        );
    }

    /// Gutter line numbers, tokenized body text, and the local caret —
    /// one visual row per source line.
    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine for gutter, text and caret in one row loop"
    )]
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
            .get(..self.edit.caret())
            .map_or(0, |s| s.matches('\n').count());
        let cursor_col_prefix: &str = {
            let line_start = self.line_start(self.edit.caret());
            self.body
                .text
                .get(line_start..self.edit.caret())
                .unwrap_or("")
        };

        for (row, line) in self.body.text.split('\n').enumerate() {
            let y = body_top + count_to_f64(row) * Self::line_height(tr) + 2.0;
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

            let line_start = self
                .body
                .text
                .split_inclusive('\n')
                .take(row)
                .map(str::len)
                .sum::<usize>();
            let (sel_start, sel_end) = {
                let range = self.edit.range();
                (
                    range.start.max(line_start),
                    // Byte offsets inside one string: the sum cannot exceed its length.
                    range.end.min(line_start.saturating_add(line.len())),
                )
            };
            if sel_start < sel_end {
                // line_start <= sel_start < sel_end, so neither subtraction saturates.
                let prefix = line
                    .get(..sel_start.saturating_sub(line_start))
                    .unwrap_or("");
                let selected = line
                    .get(sel_start.saturating_sub(line_start)..sel_end.saturating_sub(line_start))
                    .unwrap_or("");
                let (prefix_w, _) = tr.measure(prefix, TEXT_SIZE, None);
                let (selected_w, _) = tr.measure(selected, TEXT_SIZE, None);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    palette.selection,
                    None,
                    &Rect::new(
                        content_x + f64::from(prefix_w),
                        y - 2.0,
                        content_x + f64::from(prefix_w + selected_w),
                        y + f64::from(LINE_HEIGHT) - 2.0,
                    ),
                );
            }

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
                    y + Self::line_height(tr) - 2.0,
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
            let y = body_top + count_to_f64(row) * Self::line_height(tr) + 2.0;
            // 2px wide, full line height — matches the local caret in
            // `TextInput` / `RichTextLine`.
            let caret_rect = Rect::new(x, y, x + 2.0, y + Self::line_height(tr));
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
            let y0 = body_top + count_to_f64(row) * Self::line_height(tr) + 2.0;
            let y1 = y0 + Self::line_height(tr);
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
    ui::provides!(
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::record::capability::RecordScoped,
        dyn ui::undo::UndoPoints
    );

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.body.sync(state, root);
        self.reconcile_disk(state, root);
        self.clamp_cursor();
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let source = self.source.as_ref()?;
        let mut node = ui::a11y::AccessNode::new(ui::a11y::Role::Document)
            .with_name(source.path.file_name().map_or_else(
                || source.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ))
            .with_value(self.body.text.clone())
            .with_description(
                self.disk_conflict
                    .clone()
                    .unwrap_or_else(|| source.provider_id.to_string()),
            )
            .with_bounds(self.rect);
        if self.disk_conflict.is_some() {
            node.state.invalid = true;
        }
        Some(node)
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

        let body_top = self.rect.y0 + Self::body_offset(tr, theme);
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
        // A hairline derived against the gutter it separates (HIG §9.5),
        // not a literal black that vanishes on a dark editor ground.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::tokens::colors::Colors::hairline_on(palette.gutter_bg, 26),
            None,
            &divider,
        );

        let origin = (body_top, content_x);
        self.draw_peer_carets(scene, transform, tr, theme, presence, origin);
        self.draw_peer_selections(scene, transform, tr, theme, presence, origin);

        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.clamp_cursor();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_mouse = p;
                let was_hover = self.hovered;
                self.hovered = self.rect.contains(p);
                if self.hovered != was_hover {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => {
                if self.hovered && *es == ElementState::Pressed {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                self.handle_keyboard_input(
                    key_event,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
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
}

impl ui::undo::UndoPoints for EditorApp {
    fn needs_commit(&self) -> bool {
        self.body.needs_commit()
    }
    fn on_commit(&mut self) {
        self.body.on_commit();
    }
    fn get_state(&self) -> Option<String> {
        Some(self.edit.caret().to_string())
    }
    fn set_state(&mut self, s: &str) {
        if let Ok(pos) = s.parse::<usize>() {
            self.edit.set_caret(pos);
            self.clamp_cursor();
        }
    }
}

impl ui::record::capability::RecordScoped for EditorApp {
    fn record_intent(&self) -> Option<ui::record::RecordCheckout> {
        self.source.as_ref().map(|source| {
            ui::record::RecordCheckout::new(
                ui::record::RecordId::for_file(&source.path),
                ui::record::RecordKind::new("text-document"),
            )
        })
    }
    /// Folded into the body's presence key so a peer's caret for one tab
    /// isn't drawn in another tab of the same app.
    fn set_record_scope(&mut self, record: &ui::record::RecordId) {
        self.body.set_record(record);
    }
}

impl ui::presence::capability::PublishesPresence for EditorApp {
    fn presence_key(&self) -> Option<ui::presence::PresenceKey> {
        Some(self.body.presence_key())
    }
}

impl EditorApp {
    #[expect(
        clippy::too_many_lines,
        reason = "one key dispatcher maps every editing shortcut"
    )]
    fn handle_keyboard_input(
        &mut self,
        key_event: &KeyEvent,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        if !self.focused || key_event.state != ElementState::Pressed {
            return;
        }
        let mods = modifiers.state();
        if mods.super_key() {
            return;
        }
        if mods.control_key() {
            match &key_event.logical_key {
                Key::Named(NamedKey::Backspace) => {
                    self.delete_word_before(state, root);
                    *handled = true;
                }
                Key::Named(NamedKey::Delete) => {
                    self.delete_word_after(state, root);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.edit
                        .motion(&self.body.text, Motion::WordLeft, mods.shift_key());
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.edit
                        .motion(&self.body.text, Motion::WordRight, mods.shift_key());
                    *handled = true;
                }
                Key::Named(NamedKey::Home) => {
                    self.edit
                        .motion(&self.body.text, Motion::DocStart, mods.shift_key());
                    *handled = true;
                }
                Key::Named(NamedKey::End) => {
                    self.edit
                        .motion(&self.body.text, Motion::DocEnd, mods.shift_key());
                    *handled = true;
                }
                Key::Character(value) => match value.to_lowercase().as_str() {
                    "a" => {
                        self.edit.select_all(&self.body.text);
                        *handled = true;
                    }
                    "c" => {
                        if let Some(selected) = self.body.text.get(self.edit.range()) {
                            clipboard_copy(selected);
                        }
                        *handled = true;
                    }
                    "x" => {
                        if self.edit.has_selection() {
                            if let Some(selected) = self.body.text.get(self.edit.range()) {
                                clipboard_copy(selected);
                            }
                            let edit = self.edit.delete_backward(&self.body.text, false);
                            self.apply_edit(state, root, &edit);
                        }
                        *handled = true;
                    }
                    "v" => {
                        if let Some(paste) = clipboard_paste() {
                            let cleaned: String = paste
                                .chars()
                                .filter(|character| {
                                    !character.is_control()
                                        || *character == '\n'
                                        || *character == '\t'
                                })
                                .collect();
                            if !cleaned.is_empty() {
                                let edit = self.edit.insert(&cleaned);
                                self.apply_edit(state, root, &edit);
                            }
                        }
                        *handled = true;
                    }
                    _ => {}
                },
                _ => {}
            }
            return;
        }
        match &key_event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                self.delete_before(state, root);
                *handled = true;
            }
            Key::Named(NamedKey::Delete) => {
                let edit = self.edit.delete_forward(&self.body.text, false);
                self.apply_edit(state, root, &edit);
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
                self.edit
                    .motion(&self.body.text, Motion::Left, mods.shift_key());
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.edit
                    .motion(&self.body.text, Motion::Right, mods.shift_key());
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.move_line_up(mods.shift_key()) {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if self.move_line_down(mods.shift_key()) {
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Home) => {
                self.edit
                    .motion(&self.body.text, Motion::LineStart, mods.shift_key());
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.edit
                    .motion(&self.body.text, Motion::LineEnd, mods.shift_key());
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
    fn editor_paints_rows_and_tokenizes_syntax() {
        fn enlarged_editor_rows_and_header_reserve_painted_text_height() {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let (_, body_height) = tr.measure("Ag", TEXT_SIZE, None);
            assert!(EditorApp::line_height(&tr) >= f64::from(body_height));
            let (_, title_height) = tr.measure(
                "Editor",
                theme.tokens.typography.scale.lg / theme.text_scale,
                None,
            );
            let (_, hint_height) = tr.measure(
                "Click to edit",
                theme.tokens.typography.scale.sm / theme.text_scale,
                None,
            );
            assert!(
                EditorApp::body_offset(&mut tr, &theme)
                    >= 18.0 + f64::from(title_height + hint_height) + 12.0
            );
        }

        fn tokenize_separates_keyword_string_number() {
            let toks = tokenize("const count = 0; return 'hi';");
            let has_keyword = toks.iter().any(|(s, e, c)| {
                *c == TokClass::Keyword
                    && "const count = 0; return 'hi';".get(*s..*e) == Some("const")
            });
            assert!(has_keyword, "expected `const` to tokenize as Keyword");
            let has_string = toks.iter().any(|(_, _, c)| *c == TokClass::StringLit);
            assert!(has_string, "expected `'hi'` to tokenize as StringLit");
            let has_number = toks.iter().any(|(_, _, c)| *c == TokClass::Number);
            assert!(has_number, "expected `0` to tokenize as Number");
        }

        fn tokenize_line_comment_swallows_rest_of_line() {
            let toks = tokenize("let x = 1; // trailing");
            assert!(toks.iter().any(|(_, _, c)| *c == TokClass::Comment));
            let last = toks.last().unwrap();
            assert_eq!(last.2, TokClass::Comment);
        }

        enlarged_editor_rows_and_header_reserve_painted_text_height();
        tokenize_separates_keyword_string_number();
        tokenize_line_comment_swallows_rest_of_line();
    }

    #[test]
    fn editor_text_edits_mutate_the_crdt_with_code_editor_word_motion() {
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

        fn insert_and_delete_mutate_crdt() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::new();
            app.sync(&mut doc, &tile);
            app.insert_text(&mut doc, &tile, "abc");
            assert_eq!(app.body.text, "abc");
            assert_eq!(app.edit.caret(), 3);
            app.delete_before(&mut doc, &tile);
            assert_eq!(app.body.text, "ab");
        }

        fn control_word_editing_matches_code_editor_expectations() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::with_seed("hello world");
            app.sync(&mut doc, &tile);

            app.delete_word_before(&mut doc, &tile);
            assert_eq!(app.body.text, "hello ");
            assert_eq!(app.edit.caret(), "hello ".len());

            app.edit.set_caret(0);
            app.delete_word_after(&mut doc, &tile);
            assert_eq!(app.body.text, "");
            assert_eq!(app.edit.caret(), 0);
        }

        fn control_arrows_jump_between_words_and_document_extremes() {
            let mut app = EditorApp::with_seed("hello world");
            app.edit.set_caret(0);
            app.edit.motion(&app.body.text, Motion::WordRight, false);
            assert_eq!(app.edit.caret(), "hello ".len());
            app.edit.motion(&app.body.text, Motion::WordLeft, false);
            assert_eq!(app.edit.caret(), 0);
            app.edit.set_caret("hello".len());
            app.edit.motion(&app.body.text, Motion::WordLeft, false);
            assert_eq!(app.edit.caret(), 0);
            app.edit.set_caret(0);
            app.edit.motion(&app.body.text, Motion::WordRight, false);
            assert_eq!(app.edit.caret(), "hello ".len());
        }

        fn char_index_to_byte_handles_multibyte_and_oob() {
            // The body is "aβc" where `β` is a 2-byte UTF-8 scalar.
            let app = EditorApp::with_seed("aβc");
            assert_eq!(app.body.char_index_to_byte(0), 0);
            assert_eq!(app.body.char_index_to_byte(1), 1);
            assert_eq!(app.body.char_index_to_byte(2), 3); // past `β`
            // Walk past the end clamps to text.len() rather than panicking.
            assert_eq!(app.body.char_index_to_byte(99), app.body.text.len());
        }

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

        sync_creates_body_and_seed_roundtrips();
        insert_and_delete_mutate_crdt();
        control_word_editing_matches_code_editor_expectations();
        control_arrows_jump_between_words_and_document_extremes();
        char_index_to_byte_handles_multibyte_and_oob();
        arrow_down_preserves_column_across_lines();
    }

    #[test]
    fn focused_editor_dispatch_consumes_edits_and_extends_selections() {
        fn focused_editor_dispatch_consumes_ctrl_backspace() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::with_seed("hello world");
            app.sync(&mut doc, &tile);
            app.focused = true;
            let mut key: KeyEvent = unsafe { std::mem::zeroed() };
            key.logical_key = Key::Named(NamedKey::Backspace);
            key.state = ElementState::Pressed;
            let modifiers = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            let mut handled = false;

            app.handle_keyboard_input(&key, modifiers, &mut doc, &tile, &mut handled);

            assert!(handled);
            assert_eq!(app.body.text, "hello ");
        }

        fn focused_editor_dispatch_selects_and_replaces_text() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::with_seed("hello");
            app.sync(&mut doc, &tile);
            app.focused = true;

            let mut select_all: KeyEvent = unsafe { std::mem::zeroed() };
            select_all.logical_key = Key::Character("a".into());
            select_all.state = ElementState::Pressed;
            let control = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            let mut handled = false;
            app.handle_keyboard_input(&select_all, control, &mut doc, &tile, &mut handled);
            assert!(handled);
            assert_eq!(app.edit.range(), 0.."hello".len());

            let mut replacement: KeyEvent = unsafe { std::mem::zeroed() };
            replacement.logical_key = Key::Character("x".into());
            replacement.text = Some("x".into());
            replacement.state = ElementState::Pressed;
            handled = false;
            app.handle_keyboard_input(
                &replacement,
                Modifiers::default(),
                &mut doc,
                &tile,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(app.body.text, "x");
            assert_eq!(app.edit.caret(), 1);
        }

        fn focused_editor_dispatch_extends_selection_with_shift_and_control() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::with_seed("hello world");
            app.sync(&mut doc, &tile);
            app.focused = true;

            let mut left: KeyEvent = unsafe { std::mem::zeroed() };
            left.logical_key = Key::Named(NamedKey::ArrowLeft);
            left.state = ElementState::Pressed;
            let shift = Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
            let mut handled = false;
            app.handle_keyboard_input(&left, shift, &mut doc, &tile, &mut handled);
            assert!(handled);
            assert_eq!(
                app.edit.range(),
                "hello world".len() - 1.."hello world".len()
            );

            let control_shift = Modifiers::from(
                winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
            );
            handled = false;
            app.handle_keyboard_input(&left, control_shift, &mut doc, &tile, &mut handled);
            assert!(handled);
            assert_eq!(app.edit.range(), "hello ".len().."hello world".len());
        }

        fn focused_editor_dispatch_extends_vertical_selection() {
            let (mut doc, tile) = bind();
            let mut app = EditorApp::with_seed("one\ntwo\nthree");
            app.sync(&mut doc, &tile);
            app.focused = true;

            let mut up: KeyEvent = unsafe { std::mem::zeroed() };
            up.logical_key = Key::Named(NamedKey::ArrowUp);
            up.state = ElementState::Pressed;
            let shift = Modifiers::from(winit::keyboard::ModifiersState::SHIFT);
            let mut handled = false;
            app.handle_keyboard_input(&up, shift, &mut doc, &tile, &mut handled);

            assert!(handled);
            assert_eq!(app.edit.selection().anchor, "one\ntwo\nthree".len());
            assert_eq!(app.edit.caret(), "one\ntwo".len());
            assert_eq!(app.edit.range(), "one\ntwo".len().."one\ntwo\nthree".len());
        }

        focused_editor_dispatch_consumes_ctrl_backspace();
        focused_editor_dispatch_selects_and_replaces_text();
        focused_editor_dispatch_extends_selection_with_shift_and_control();
        focused_editor_dispatch_extends_vertical_selection();
    }

    #[test]
    fn disk_changes_replace_clean_bodies_and_surface_conflicts_for_edited_ones() {
        fn external_disk_change_replaces_an_unedited_record_body() {
            let (mut doc, tile) = bind();
            let path = PathBuf::from("/work/Document-000.txt");
            let mut first = EditorApp::for_file(path.clone(), "org.tilo.Editor", "base".into());
            first.sync(&mut doc, &tile);

            let mut reopened =
                EditorApp::for_file(path, "org.tilo.Editor", "changed on disk".into());
            reopened.sync(&mut doc, &tile);

            assert_eq!(reopened.body.text, "changed on disk");
            assert!(reopened.disk_conflict.is_none());
        }

        fn an_older_editor_never_replays_its_stale_disk_observation() {
            let (mut doc, tile) = bind();
            let path = PathBuf::from("/work/Document-000.txt");
            let mut older = EditorApp::for_file(path.clone(), "org.tilo.Editor", "old".into());
            older.sync(&mut doc, &tile);
            let mut newer = EditorApp::for_file(path, "org.tilo.Editor", "new".into());
            newer.sync(&mut doc, &tile);
            assert_eq!(newer.body.text, "new");

            older.sync(&mut doc, &tile);

            assert_eq!(older.body.text, "new");
            assert_eq!(
                doc.get(&tile, DISK_BASE_KEY)
                    .unwrap()
                    .and_then(|(value, _)| value.into_scalar().ok())
                    .and_then(|value| match value {
                        ScalarValue::Str(text) => Some(text.to_string()),
                        _ => None,
                    })
                    .as_deref(),
                Some("new")
            );
        }

        fn concurrent_disk_change_preserves_unsaved_record_edits_and_surfaces_conflict() {
            let (mut doc, tile) = bind();
            let path = PathBuf::from("/work/Document-000.txt");
            let mut first = EditorApp::for_file(path.clone(), "org.tilo.Editor", "base".into());
            first.sync(&mut doc, &tile);
            first.body.splice(&mut doc, &tile, 0..4, "local edit");

            let mut reopened =
                EditorApp::for_file(path, "org.tilo.Editor", "changed on disk".into());
            reopened.sync(&mut doc, &tile);

            assert_eq!(reopened.body.text, "local edit");
            assert!(reopened.disk_conflict.is_some());
            let semantics = reopened.accessibility().expect("file semantics");
            assert!(semantics.state.invalid);
            assert!(
                semantics
                    .description
                    .as_deref()
                    .is_some_and(|message| message.contains("unsaved edits"))
            );
        }

        fn body_presence_key_names_the_record_and_the_body_field() {
            use ui::presence::{PathSegment, PresenceKey};
            // Stable contract: the shell seeds presence under this exact key,
            // so a peer's caret/selection materialises where `draw` looks.
            // Unhosted — no record named yet — it is the bare field.
            let mut app = EditorApp::new();
            let key = ui::presence::capability::presence_key(&app)
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
            ui::record::capability::set_record_scope(&mut app, &record);
            match ui::presence::capability::presence_key(&app).expect("still publishes") {
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

        external_disk_change_replaces_an_unedited_record_body();
        an_older_editor_never_replays_its_stale_disk_observation();
        concurrent_disk_change_preserves_unsaved_record_edits_and_surfaces_conflict();
        body_presence_key_names_the_record_and_the_body_field();
    }
}
