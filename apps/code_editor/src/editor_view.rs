//! The code editor pane: a custom [`Widget`] that renders line numbers, a
//! gutter, syntax-highlighted source text, the active-line highlight, and the
//! caret(s) + selection — all drawn directly into the Vello scene so the
//! editor has the dense, monospace IDE look the toolkit's prose widgets don't
//! provide.
//!
//! Text is the source of truth in the CRDT: the widget is bound to a single
//! Automerge `Text` object (the active buffer). Every edit splices into that
//! Text, so edits are collaborative and undoable. The widget keeps only render
//! caches: the split-into-lines view of the buffer, the per-line highlight
//! spans, and the caret/selection positions (which are ephemeral per-peer
//! state, restored across undo via `get_state`/`set_state`).
//!
//! Caret model: positions are *(line, column)* where column is a count of
//! characters (not bytes), matching Automerge Text's char addressing. Multiple
//! carets are supported; the primary caret drives the status bar and scrolling.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::doc::EditorDoc;
use crate::highlight::{Highlighter, Span};
use crate::model::Language;

/// Monospace cell metrics. The editor advances a fixed cell width per column;
/// for the bundled monospace font at this size that tracks the glyph advance
/// closely enough for crisp caret placement.
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f64 = 20.0;
/// Approximate monospace advance for `FONT_SIZE`. Measured lazily against the
/// real font on first layout and cached, so it stays correct if the system
/// monospace face changes.
const FALLBACK_CELL_W: f64 = 8.4;
/// Left padding inside the gutter and right padding before code starts.
const GUTTER_PAD: f64 = 12.0;
const CODE_PAD_LEFT: f64 = 12.0;

/// A single caret + optional selection anchor, in (line, col) char coords.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub col: usize,
    /// Selection anchor; the selected range is [anchor, caret] (either order).
    pub anchor: Option<(usize, usize)>,
}

impl Caret {
    fn at(line: usize, col: usize) -> Self {
        Caret { line, col, anchor: None }
    }
    /// Ordered ((line,col) start, (line,col) end) of the selection, or None.
    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let c = (self.line, self.col);
        if a <= c {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }
}

pub struct EditorView {
    rect: Rect,
    /// The buffer's lines (cache of the CRDT Text split on '\n').
    lines: Vec<String>,
    /// Highlight spans per line, recomputed when the buffer changes.
    spans: Vec<Vec<Span>>,
    /// The Automerge Text object this view edits. `None` until bound.
    text_obj: Option<ObjId>,
    /// Active language → grammar for highlighting.
    language: Language,
    /// Carets. Always at least one. Index 0 is primary.
    carets: Vec<Caret>,
    /// Vertical scroll offset in lines (fractional for smooth wheel).
    scroll_y: f64,
    /// Horizontal scroll offset in pixels.
    scroll_x: f64,
    /// Measured monospace cell width, resolved on first draw.
    cell_w: f64,
    is_focused: bool,
    dirty: bool,
    /// Snapshot of the buffer text the line cache was built from — lets `sync`
    /// detect remote edits cheaply (string compare) and rebuild.
    cached_text: String,
    /// Soft-tab width for the active language.
    indent_width: usize,
    /// Last known cursor position in logical pixels, updated on CursorMoved so
    /// click handlers can hit-test without threading the position through.
    last_cursor: Point,
}

impl EditorView {
    pub fn new() -> Self {
        EditorView {
            rect: Rect::ZERO,
            lines: vec![String::new()],
            spans: vec![Vec::new()],
            text_obj: None,
            language: Language::PlainText,
            carets: vec![Caret::at(0, 0)],
            scroll_y: 0.0,
            scroll_x: 0.0,
            cell_w: FALLBACK_CELL_W,
            is_focused: true,
            dirty: true,
            cached_text: String::new(),
            indent_width: 4,
            last_cursor: Point::ZERO,
        }
    }

    /// Bind this view to a buffer's Text object and language. Resets carets to
    /// the top. The line/highlight caches refresh on the next `sync`.
    pub fn bind(&mut self, text_obj: ObjId, language: Language) {
        self.text_obj = Some(text_obj);
        self.language = language;
        self.indent_width = language.indent_width();
        self.carets = vec![Caret::at(0, 0)];
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
        self.cached_text.clear(); // force rebuild on next sync
        self.dirty = true;
    }

    /// Unbind (no buffer open). Shows an empty state.
    pub fn unbind(&mut self) {
        self.text_obj = None;
        self.lines = vec![String::new()];
        self.spans = vec![Vec::new()];
        self.carets = vec![Caret::at(0, 0)];
        self.cached_text.clear();
        self.dirty = true;
    }

    pub fn is_bound(&self) -> bool {
        self.text_obj.is_some()
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn indent_width(&self) -> usize {
        self.indent_width
    }

    /// Primary caret position (line, col), 0-based.
    pub fn primary_caret(&self) -> (usize, usize) {
        let c = self.carets[0];
        (c.line, c.col)
    }

    /// Number of carets (for the status bar's "N cursors" indicator).
    pub fn caret_count(&self) -> usize {
        self.carets.len()
    }

    /// Total selected character count across all carets.
    pub fn selection_len(&self) -> usize {
        self.carets
            .iter()
            .filter_map(|c| c.selection())
            .map(|(s, e)| self.range_char_len(s, e))
            .sum()
    }

    fn range_char_len(&self, s: (usize, usize), e: (usize, usize)) -> usize {
        if s.0 == e.0 {
            return e.1.saturating_sub(s.1);
        }
        let mut total = self.lines[s.0].chars().count().saturating_sub(s.1) + 1;
        for l in (s.0 + 1)..e.0 {
            total += self.lines[l].chars().count() + 1;
        }
        total += e.1;
        total
    }

    /// Rebuild the line + highlight caches from the buffer text.
    fn rebuild(&mut self, text: &str) {
        self.cached_text = text.to_string();
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(|s| s.to_string()).collect()
        };
        // split('\n') on "a\n" yields ["a", ""]; that trailing empty line is
        // the editable line after the final newline, which is what we want.
        self.recompute_highlight();
        self.clamp_carets();
        self.dirty = true;
    }

    /// Recompute highlight spans for every line. Multi-line constructs (block
    /// comments, multi-line strings) carry state across lines via the
    /// highlighter's `LineState`.
    fn recompute_highlight(&mut self) {
        let hl = Highlighter::for_language(self.language);
        let mut state = hl.fresh_state();
        self.spans = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            // The highlighter wants the trailing newline so anchored rules
            // (line comments, etc.) terminate correctly.
            let mut with_nl = line.clone();
            with_nl.push('\n');
            let spans = hl.highlight_line(&with_nl, &mut state);
            self.spans.push(spans);
        }
    }

    fn clamp_carets(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        for c in &mut self.carets {
            c.line = c.line.min(last_line);
            let cols = self.lines[c.line].chars().count();
            c.col = c.col.min(cols);
            if let Some((al, ac)) = c.anchor {
                let al = al.min(last_line);
                let ac = ac.min(self.lines[al].chars().count());
                c.anchor = Some((al, ac));
            }
        }
    }

    /// Convert a (line, col) char position to an absolute char index into the
    /// flat buffer (for CRDT splicing).
    fn char_index(&self, line: usize, col: usize) -> usize {
        let mut idx = 0;
        for l in 0..line {
            idx += self.lines[l].chars().count() + 1; // +1 for the '\n'
        }
        idx + col
    }

    /// The width of the gutter (line-number column) for the current line count.
    fn gutter_width(&self) -> f64 {
        let digits = self.lines.len().max(1).to_string().len().max(2);
        GUTTER_PAD * 2.0 + digits as f64 * self.cell_w
    }

    /// Visible-line capacity of the viewport.
    fn visible_lines(&self) -> usize {
        (self.rect.height() / LINE_HEIGHT).ceil() as usize + 1
    }

    /// Scroll so the primary caret is visible.
    fn scroll_to_caret(&mut self) {
        let (line, col) = self.primary_caret();
        let top = self.scroll_y;
        let rows = (self.rect.height() / LINE_HEIGHT).floor().max(1.0);
        let bottom = top + rows - 1.0;
        if (line as f64) < top {
            self.scroll_y = line as f64;
        } else if (line as f64) > bottom {
            self.scroll_y = line as f64 - rows + 1.0;
        }
        // Horizontal: keep the caret column in view.
        let code_w = (self.rect.width() - self.gutter_width() - CODE_PAD_LEFT).max(0.0);
        let caret_px = col as f64 * self.cell_w;
        if caret_px < self.scroll_x {
            self.scroll_x = caret_px;
        } else if caret_px > self.scroll_x + code_w - self.cell_w {
            self.scroll_x = caret_px - code_w + self.cell_w;
        }
        self.scroll_x = self.scroll_x.max(0.0);
        self.dirty = true;
    }

    // ---- editing primitives (all route through the CRDT) ----

    /// Insert a string at the primary caret (replacing any selection),
    /// splicing into the CRDT. Updates the caret. For simplicity multi-caret
    /// inserts apply the same text at each caret, processed bottom-up so
    /// earlier indices stay valid.
    fn insert(&mut self, doc: &mut AutoCommit, s: &str) {
        let Some(text) = self.text_obj.clone() else { return };
        // Process carets bottom-to-top so a splice doesn't shift the indices
        // of carets we haven't handled yet.
        let mut order: Vec<usize> = (0..self.carets.len()).collect();
        order.sort_by(|&a, &b| {
            let ca = (self.carets[a].line, self.carets[a].col);
            let cb = (self.carets[b].line, self.carets[b].col);
            cb.cmp(&ca)
        });
        for &i in &order {
            self.apply_insert_at(doc, &text, i, s);
        }
        self.sync_from_doc(doc);
        self.scroll_to_caret();
    }

    fn apply_insert_at(&mut self, doc: &mut AutoCommit, text: &ObjId, i: usize, s: &str) {
        let caret = self.carets[i];
        // Delete selection first if present.
        let start = if let Some((sel_s, sel_e)) = caret.selection() {
            let s_idx = self.char_index(sel_s.0, sel_s.1);
            let e_idx = self.char_index(sel_e.0, sel_e.1);
            EditorDoc::splice(doc, text, s_idx, e_idx - s_idx, "");
            self.carets[i].anchor = None;
            sel_s
        } else {
            (caret.line, caret.col)
        };
        let idx = self.char_index(start.0, start.1);
        EditorDoc::splice(doc, text, idx, 0, s);
        // Recompute caret position after the inserted text.
        let inserted_lines = s.split('\n').count();
        if inserted_lines == 1 {
            self.carets[i] = Caret::at(start.0, start.1 + s.chars().count());
        } else {
            let last = s.rsplit('\n').next().unwrap_or("");
            self.carets[i] =
                Caret::at(start.0 + inserted_lines - 1, last.chars().count());
        }
    }

    /// Backspace at every caret. With a selection, deletes it; otherwise
    /// removes the char before the caret (or joins with the previous line).
    fn backspace(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else { return };
        let mut order: Vec<usize> = (0..self.carets.len()).collect();
        order.sort_by(|&a, &b| {
            let ca = (self.carets[a].line, self.carets[a].col);
            let cb = (self.carets[b].line, self.carets[b].col);
            cb.cmp(&ca)
        });
        for &i in &order {
            let caret = self.carets[i];
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                EditorDoc::splice(doc, &text, s_idx, e_idx - s_idx, "");
                self.carets[i] = Caret::at(s.0, s.1);
            } else {
                let idx = self.char_index(caret.line, caret.col);
                if idx == 0 {
                    continue;
                }
                EditorDoc::splice(doc, &text, idx - 1, 1, "");
                // Position recomputed after sync from the new flat text; we
                // estimate here and clamp later.
                if caret.col > 0 {
                    self.carets[i] = Caret::at(caret.line, caret.col - 1);
                } else if caret.line > 0 {
                    let prev_len = self.lines[caret.line - 1].chars().count();
                    self.carets[i] = Caret::at(caret.line - 1, prev_len);
                }
            }
        }
        self.sync_from_doc(doc);
        self.scroll_to_caret();
    }

    /// Forward-delete at every caret.
    fn delete_forward(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else { return };
        let mut order: Vec<usize> = (0..self.carets.len()).collect();
        order.sort_by(|&a, &b| {
            let ca = (self.carets[a].line, self.carets[a].col);
            let cb = (self.carets[b].line, self.carets[b].col);
            cb.cmp(&ca)
        });
        for &i in &order {
            let caret = self.carets[i];
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                EditorDoc::splice(doc, &text, s_idx, e_idx - s_idx, "");
                self.carets[i] = Caret::at(s.0, s.1);
            } else {
                let idx = self.char_index(caret.line, caret.col);
                let total: usize = self.cached_text.chars().count();
                if idx >= total {
                    continue;
                }
                EditorDoc::splice(doc, &text, idx, 1, "");
            }
        }
        self.sync_from_doc(doc);
    }

    /// Re-read the buffer from the CRDT and rebuild caches. Called after every
    /// local edit and from `sync` for remote edits.
    fn sync_from_doc(&mut self, doc: &mut AutoCommit) {
        if let Some(t) = self.text_obj.clone() {
            use automerge::ReadDoc;
            let text = doc.text(&t).unwrap_or_default();
            self.rebuild(&text);
        }
    }

    // ---- caret movement ----

    fn move_caret(&mut self, dx: isize, dy: isize, extend: bool) {
        // Collapse multi-carets to primary on plain arrow (matches editor UX).
        if !extend {
            self.carets.truncate(1);
        }
        let c = &mut self.carets[0];
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        if dy != 0 {
            let new_line = (c.line as isize + dy)
                .clamp(0, self.lines.len() as isize - 1) as usize;
            c.line = new_line;
            let cols = self.lines[new_line].chars().count();
            c.col = c.col.min(cols);
        }
        if dx != 0 {
            let cols = self.lines[c.line].chars().count();
            if dx > 0 {
                if c.col < cols {
                    c.col += 1;
                } else if c.line + 1 < self.lines.len() {
                    c.line += 1;
                    c.col = 0;
                }
            } else if c.col > 0 {
                c.col -= 1;
            } else if c.line > 0 {
                c.line -= 1;
                c.col = self.lines[c.line].chars().count();
            }
        }
        self.scroll_to_caret();
    }

    fn move_home(&mut self, extend: bool) {
        self.carets.truncate(1);
        let c = &mut self.carets[0];
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        // Smart home: first non-whitespace, then column 0.
        let line = &self.lines[c.line];
        let first_non_ws = line.chars().take_while(|ch| ch.is_whitespace()).count();
        c.col = if c.col == first_non_ws { 0 } else { first_non_ws };
        self.scroll_to_caret();
    }

    fn move_end(&mut self, extend: bool) {
        self.carets.truncate(1);
        let c = &mut self.carets[0];
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        c.col = self.lines[c.line].chars().count();
        self.scroll_to_caret();
    }

    fn select_all(&mut self) {
        self.carets.truncate(1);
        let last = self.lines.len() - 1;
        let last_col = self.lines[last].chars().count();
        self.carets[0] = Caret {
            line: last,
            col: last_col,
            anchor: Some((0, 0)),
        };
        self.dirty = true;
    }

    /// Add a caret on the line below the lowest current caret (multi-cursor).
    fn add_caret_below(&mut self) {
        if let Some(last) = self.carets.iter().max_by_key(|c| c.line).copied() {
            if last.line + 1 < self.lines.len() {
                let cols = self.lines[last.line + 1].chars().count();
                self.carets.push(Caret::at(last.line + 1, last.col.min(cols)));
                self.dirty = true;
            }
        }
    }

    /// Insert a newline plus auto-indent matching the current line's leading
    /// whitespace — the single most-felt editor nicety.
    fn newline_with_indent(&mut self, doc: &mut AutoCommit) {
        let (line, _) = self.primary_caret();
        let indent: String = self.lines[line]
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .collect();
        let mut payload = String::from("\n");
        payload.push_str(&indent);
        self.insert(doc, &payload);
    }

    /// Indent: insert `indent_width` spaces at the caret (or, with a multi-line
    /// selection, this could indent each line — kept simple as cursor-level
    /// for now).
    fn insert_indent(&mut self, doc: &mut AutoCommit) {
        let spaces = " ".repeat(self.indent_width);
        self.insert(doc, &spaces);
    }

    // ---- hit testing ----

    /// Map a pixel point to a (line, col) caret position.
    fn point_to_caret(&self, p: Point) -> (usize, usize) {
        let gutter = self.gutter_width();
        let rel_y = p.y - self.rect.y0 + self.scroll_y * LINE_HEIGHT;
        let line = ((rel_y / LINE_HEIGHT).floor() as isize)
            .clamp(0, self.lines.len() as isize - 1) as usize;
        let rel_x = p.x - self.rect.x0 - gutter - CODE_PAD_LEFT + self.scroll_x;
        let col = ((rel_x / self.cell_w).round().max(0.0) as usize)
            .min(self.lines[line].chars().count());
        (line, col)
    }
}

impl Widget for EditorView {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Pull remote/undo edits: if the buffer text differs from our cache,
        // rebuild. Cheap string compare on the hot path.
        if let Some(t) = self.text_obj.clone() {
            use automerge::ReadDoc;
            let text = state.text(&t).unwrap_or_default();
            if text != self.cached_text {
                self.rebuild(&text);
            }
        }
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
                flex_grow: 1.0,
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        // Measure the real monospace cell width once we have a renderer.
        let (mw, _) =
            text_renderer.measure_styled("M", FONT_SIZE, None, TextWeight::Regular, false, true);
        if mw > 0.0 {
            self.cell_w = mw as f64;
        }

        // Editor background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        let gutter_w = self.gutter_width();
        let code_x0 = self.rect.x0 + gutter_w + CODE_PAD_LEFT;
        let first_line = self.scroll_y.floor() as usize;
        let frac = self.scroll_y - first_line as f64;
        let n_visible = self.visible_lines();

        // Gutter background (very subtly raised).
        let gutter_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + gutter_w,
            self.rect.y1,
        );
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &gutter_rect);

        if self.text_obj.is_none() {
            // Empty state.
            text_renderer.draw_text(
                scene,
                "Open a file from the explorer to start editing",
                FONT_SIZE,
                c.text_dim,
                transform * Affine::translate((self.rect.x0 + 24.0, self.rect.y0 + 24.0)),
                Some(self.rect.width() as f32),
            );
            return;
        }

        let primary_line = self.carets[0].line;

        // Clip code area so long lines don't bleed into the gutter.
        for vi in 0..n_visible {
            let line_idx = first_line + vi;
            if line_idx >= self.lines.len() {
                break;
            }
            let y = self.rect.y0 + (vi as f64 - frac) * LINE_HEIGHT;
            if y > self.rect.y1 {
                break;
            }

            // Active-line highlight across the code area.
            if line_idx == primary_line && self.carets.len() == 1 {
                let hl = Rect::new(
                    self.rect.x0 + gutter_w,
                    y,
                    self.rect.x1,
                    y + LINE_HEIGHT,
                );
                scene.fill(Fill::NonZero, transform, c.surface_white_5, None, &hl);
            }

            // Selection rectangles for any caret touching this line.
            for caret in &self.carets {
                if let Some((s, e)) = caret.selection() {
                    if line_idx < s.0 || line_idx > e.0 {
                        continue;
                    }
                    let start_col = if line_idx == s.0 { s.1 } else { 0 };
                    let end_col = if line_idx == e.0 {
                        e.1
                    } else {
                        self.lines[line_idx].chars().count() + 1 // include EOL
                    };
                    let sx0 = code_x0 + start_col as f64 * self.cell_w - self.scroll_x;
                    let sx1 = code_x0 + end_col as f64 * self.cell_w - self.scroll_x;
                    let sel = Rect::new(sx0.max(self.rect.x0 + gutter_w), y, sx1, y + LINE_HEIGHT);
                    // Translucent accent for the selection band; alpha is read
                    // from the theme accent so a theme swap recolours it.
                    let sel_color = c.accent[5].multiply_alpha(0.35);
                    scene.fill(Fill::NonZero, transform, sel_color, None, &sel);
                }
            }

            // Line number, right-aligned in the gutter.
            let num = (line_idx + 1).to_string();
            let num_w = num.len() as f64 * self.cell_w;
            let num_x = self.rect.x0 + gutter_w - GUTTER_PAD - num_w;
            let num_color = if line_idx == primary_line {
                c.text_primary
            } else {
                c.text_dim
            };
            text_renderer.draw_text_styled(
                scene,
                &num,
                FONT_SIZE,
                num_color,
                transform * Affine::translate((num_x, y + 2.0)),
                None,
                TextWeight::Regular,
                false,
                true,
            );

            // The highlighted code text, span by span.
            let line = &self.lines[line_idx];
            let empty: Vec<Span> = Vec::new();
            let spans = self.spans.get(line_idx).unwrap_or(&empty);
            let base_x = code_x0 - self.scroll_x;
            if spans.is_empty() && !line.is_empty() {
                // Fallback for a line the highlighter produced no spans for —
                // draw it plain so no characters are ever dropped.
                text_renderer.draw_text_styled(
                    scene,
                    line,
                    FONT_SIZE,
                    c.text_primary,
                    transform * Affine::translate((base_x, y + 2.0)),
                    None,
                    TextWeight::Regular,
                    false,
                    true,
                );
            } else {
                for span in spans {
                    // Byte offsets → char offset for x positioning.
                    let prefix = &line.as_bytes()[..span.start.min(line.len())];
                    let char_col = std::str::from_utf8(prefix)
                        .map(|s| s.chars().count())
                        .unwrap_or(0);
                    let seg = line
                        .get(span.start..span.end.min(line.len()))
                        .unwrap_or("");
                    if seg.is_empty() {
                        continue;
                    }
                    let x = base_x + char_col as f64 * self.cell_w;
                    // Skip fully off-screen-left segments.
                    if x + seg.chars().count() as f64 * self.cell_w < self.rect.x0 + gutter_w {
                        continue;
                    }
                    let color = span.role.color(theme);
                    let weight = if matches!(span.role, crate::highlight::TokenRole::Keyword) {
                        TextWeight::Semibold
                    } else {
                        TextWeight::Regular
                    };
                    text_renderer.draw_text_styled(
                        scene,
                        seg,
                        FONT_SIZE,
                        color,
                        transform * Affine::translate((x, y + 2.0)),
                        None,
                        weight,
                        false,
                        true,
                    );
                }
            }
        }

        // Carets (drawn last, on top). Blink-free for screenshot determinism.
        if self.is_focused {
            for caret in &self.carets {
                if caret.line < first_line || caret.line >= first_line + n_visible {
                    continue;
                }
                let y = self.rect.y0 + (caret.line as f64 - self.scroll_y) * LINE_HEIGHT;
                let x = code_x0 + caret.col as f64 * self.cell_w - self.scroll_x;
                if x < self.rect.x0 + gutter_w {
                    continue;
                }
                let caret_line = Line::new(
                    Point::new(x, y + 2.0),
                    Point::new(x, y + LINE_HEIGHT - 2.0),
                );
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    c.border_focus,
                    None,
                    &caret_line,
                );
            }
        }

        // Right border separating from any side panel.
        let border = Line::new(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x0, self.rect.y1),
        );
        scene.stroke(&Stroke::new(1.0), transform, c.border_standard, None, &border);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if self.text_obj.is_none() {
            return;
        }
        let mods = modifiers.state();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.rect.contains(self.last_cursor) {
                    let (line, col) = self.point_to_caret(self.last_cursor);
                    if mods.control_key() || mods.super_key() {
                        // Ctrl/Cmd-click adds a caret.
                        self.carets.push(Caret::at(line, col));
                    } else {
                        self.carets = vec![Caret::at(line, col)];
                    }
                    self.is_focused = true;
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.rect.contains(self.last_cursor) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => *y as f64 * 3.0,
                        MouseScrollDelta::PixelDelta(p) => p.y / LINE_HEIGHT,
                    };
                    let max = (self.lines.len() as f64 - 1.0).max(0.0);
                    self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                let ctrl = mods.control_key() || mods.super_key();
                let shift = mods.shift_key();
                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.move_caret(-1, 0, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        self.move_caret(1, 0, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        if ctrl && mods.alt_key() {
                            self.add_caret_below();
                        } else {
                            self.move_caret(0, -1, shift);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        if ctrl && mods.alt_key() {
                            self.add_caret_below();
                        } else {
                            self.move_caret(0, 1, shift);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        self.move_home(shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.move_end(shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageUp) => {
                        let rows = (self.rect.height() / LINE_HEIGHT).floor() as isize;
                        self.move_caret(0, -rows, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageDown) => {
                        let rows = (self.rect.height() / LINE_HEIGHT).floor() as isize;
                        self.move_caret(0, rows, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.backspace(state);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        self.delete_forward(state);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.newline_with_indent(state);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.insert_indent(state);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert(state, " ");
                        *handled = true;
                    }
                    Key::Character(s) if ctrl => {
                        match s.to_lowercase().as_str() {
                            "a" => {
                                self.select_all();
                                *handled = true;
                            }
                            "d" => {
                                self.add_caret_below();
                                *handled = true;
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        // Printable text insertion. `text` carries the
                        // resolved character(s) for the key (handles shift,
                        // layout, dead keys).
                        if !ctrl {
                            if let Some(t) = text {
                                let s: &str = t.as_str();
                                if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                                    self.insert(state, s);
                                    *handled = true;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "EditorView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("editor_view")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl EditorView {
    /// Test hook: directly drive a text insertion (bypasses winit events).
    #[cfg(test)]
    pub fn insert_for_test(&mut self, doc: &mut AutoCommit, s: &str) {
        self.insert(doc, s);
    }

    /// Test hook: backspace at the primary caret.
    #[cfg(test)]
    pub fn backspace_for_test(&mut self, doc: &mut AutoCommit) {
        self.backspace(doc);
    }

    /// Test hook: place the primary caret at (line, col).
    #[cfg(test)]
    pub fn set_caret_for_test(&mut self, line: usize, col: usize) {
        self.carets = vec![Caret::at(line, col)];
        self.clamp_carets();
    }

    /// Test hook: newline-with-indent at the primary caret.
    #[cfg(test)]
    pub fn newline_for_test(&mut self, doc: &mut AutoCommit) {
        self.newline_with_indent(doc);
    }

    /// Test hook: current buffer text.
    #[cfg(test)]
    pub fn text_for_test(&self) -> String {
        self.cached_text.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::EditorDoc;
    use crate::model::Language;

    fn bound_editor(seed: &str) -> (EditorView, AutoCommit) {
        let mut doc = AutoCommit::new();
        let t = EditorDoc::ensure_buffer(&mut doc, 0, seed);
        let mut ev = EditorView::new();
        ev.bind(t, Language::Rust);
        // Mimic the App's first sync so the line cache is populated.
        let root = automerge::ROOT;
        ev.sync(&mut doc, &root);
        (ev, doc)
    }

    #[test]
    fn insert_text_splices_into_crdt() {
        let (mut ev, mut doc) = bound_editor("ab");
        ev.set_caret_for_test(0, 1);
        ev.insert_for_test(&mut doc, "X");
        assert_eq!(ev.text_for_test(), "aXb");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    #[test]
    fn newline_preserves_leading_indent() {
        let (mut ev, mut doc) = bound_editor("    foo");
        ev.set_caret_for_test(0, 7); // end of "    foo"
        ev.newline_for_test(&mut doc);
        // New line carries the 4-space indent.
        assert_eq!(ev.text_for_test(), "    foo\n    ");
        assert_eq!(ev.primary_caret(), (1, 4));
    }

    #[test]
    fn backspace_at_line_start_joins_previous_line() {
        let (mut ev, mut doc) = bound_editor("ab\ncd");
        ev.set_caret_for_test(1, 0); // start of "cd"
        ev.backspace_for_test(&mut doc);
        assert_eq!(ev.text_for_test(), "abcd");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    #[test]
    fn backspace_removes_char_before_caret() {
        let (mut ev, mut doc) = bound_editor("abc");
        ev.set_caret_for_test(0, 2);
        ev.backspace_for_test(&mut doc);
        assert_eq!(ev.text_for_test(), "ac");
        assert_eq!(ev.primary_caret(), (0, 1));
    }

    #[test]
    fn multi_char_unicode_inserts_keep_caret_in_chars() {
        let (mut ev, mut doc) = bound_editor("");
        ev.set_caret_for_test(0, 0);
        ev.insert_for_test(&mut doc, "héllo");
        // Caret column counts characters, not bytes (é is 2 bytes).
        assert_eq!(ev.primary_caret(), (0, 5));
        assert_eq!(ev.text_for_test(), "héllo");
    }
}
