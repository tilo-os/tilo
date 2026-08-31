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
use ui::gesture::{DragSelect, Granularity};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::{
    clipboard_copy, clipboard_paste, next_word_boundary_chars, prev_word_boundary_chars,
    word_boundaries_chars,
};
use ui::theme::Theme;
use ui::widget::{EmptyState, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::bottom_panel::Severity;
use crate::doc::EditorDoc;
use crate::highlight::{Highlighter, Span};
use crate::lsp::{CompletionItem, LspDiagnostic, severity_from_lsp};
use crate::model::Language;

/// A language-server request the editor wants the app to make on its behalf,
/// carrying the primary caret's position implicitly (the app reads it). Set by
/// a keybind, drained by the app after routing the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspAsk {
    Completion,
    Hover,
    Definition,
}

/// A compact glyph hinting at a completion item's `CompletionItemKind`
/// (LSP-numbered). Falls back to a neutral dot for kinds we don't specialise.
fn completion_kind_glyph(kind: Option<u8>) -> &'static str {
    match kind {
        Some(2 | 3) => "\u{0192}",  // Method / Function — ƒ
        Some(5 | 10) => "\u{25C8}", // Field / Property — ◈
        Some(6) => "\u{25CB}",      // Variable — ○
        Some(7 | 8) => "\u{25A0}",  // Class / Interface — ■
        Some(9) => "\u{25A3}",      // Module — ▣
        Some(14) => "\u{2726}",     // Keyword — ✦
        Some(21) => "\u{25C6}",     // Constant — ◆
        _ => "\u{2022}",            // • default
    }
}

/// An inline diagnostic projected into the editor's line/column space (0-based),
/// drawn as a coloured squiggle under the offending span.
#[derive(Clone, Debug)]
struct InlineDiag {
    line: usize,
    start_col: usize,
    /// Exclusive end column on `line`; `usize::MAX` means "to end of line" (the
    /// diagnostic spanned multiple lines).
    end_col: usize,
    severity: Severity,
    message: String,
}

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
        Caret {
            line,
            col,
            anchor: None,
        }
    }
    /// Ordered ((line,col) start, (line,col) end) of the selection, or None.
    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let c = (self.line, self.col);
        if a <= c { Some((a, c)) } else { Some((c, a)) }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (focus, dirty, popup state, zero-state click) rather than a set of options that combine — a bitflag/enum would not read better"
)]
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
    /// Last known cursor position in logical pixels, updated on `CursorMoved` so
    /// click handlers can hit-test without threading the position through.
    last_cursor: Point,
    /// Shared press→move→release selection gesture (the toolkit's click ladder:
    /// single→char, double→word, triple→line, shift→extend). Owns the
    /// multi-click timing so the editor doesn't re-derive it.
    drag: DragSelect,
    /// The flat char-index span the current drag was anchored on. For a
    /// word/line drag this is the whole word/line clicked; the drag extends
    /// from whichever end is opposite the pointer so the original span stays
    /// covered — same rule as `TextInput`'s `drag_anchor_span`.
    drag_anchor: (usize, usize),

    // ---- LSP surface ----
    /// Inline diagnostics for the active buffer (from the language server),
    /// drawn as squiggles and used for the Problems panel.
    diagnostics: Vec<InlineDiag>,
    /// The completion popup's candidates. Non-empty + `completion_active` draws
    /// the popup and captures navigation keys.
    completions: Vec<CompletionItem>,
    completion_index: usize,
    completion_active: bool,
    /// The word start (line, col) the completion should replace when accepted —
    /// the identifier prefix under the caret at request time.
    completion_anchor: (usize, usize),
    /// A hover tooltip's text, drawn near the caret until dismissed.
    hover_text: Option<String>,
    /// A pending language-server request the app should perform (set by a
    /// keybind, drained by the app).
    lsp_ask: Option<LspAsk>,

    // ---- find ----
    /// Every match of the find bar's query, so all of them are tinted at once
    /// and the user can see where the hits are without stepping through them.
    find_matches: Vec<crate::find::Match>,
    /// Which of them is current, drawn more strongly than the rest.
    find_current: Option<usize>,

    // ---- empty state ----
    /// The "Open File" action pill's rect in the no-buffer zero-state, recorded
    /// each draw for hit-testing. Empty when a buffer is open.
    empty_action_rect: Rect,
    /// Rising-edge flag set when the zero-state's action pill is clicked; the
    /// app drains it and opens the file picker.
    empty_action: bool,
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
            drag: DragSelect::new(),
            drag_anchor: (0, 0),
            diagnostics: Vec::new(),
            completions: Vec::new(),
            completion_index: 0,
            completion_active: false,
            completion_anchor: (0, 0),
            hover_text: None,
            lsp_ask: None,
            find_matches: Vec::new(),
            find_current: None,
            empty_action_rect: Rect::ZERO,
            empty_action: false,
        }
    }

    /// Drain the rising-edge "Open File" click from the zero-state, if any.
    pub fn take_empty_action(&mut self) -> bool {
        std::mem::take(&mut self.empty_action)
    }

    /// The primary caret. `carets` is always non-empty by construction (`new`
    /// seeds one and nothing ever empties it), but this goes through
    /// `first()` so that invariant is provable instead of asserted via
    /// indexing.
    fn primary(&self) -> Caret {
        self.carets
            .first()
            .copied()
            .unwrap_or_else(|| Caret::at(0, 0))
    }

    /// Set the find bar's matches so they are tinted in the buffer.
    ///
    /// Showing *all* matches rather than only the one you are standing on is
    /// what makes a find useful for reading — you can see at a glance whether a
    /// symbol appears twice on screen or thirty times.
    pub fn set_find_matches(&mut self, matches: &[crate::find::Match], current: Option<usize>) {
        if self.find_matches != matches || self.find_current != current {
            self.find_matches = matches.to_vec();
            self.find_current = current;
            self.dirty = true;
        }
        if let Some(m) = current.and_then(|i| self.find_matches.get(i)).copied() {
            // Keep the current match on screen as the user steps through.
            self.carets = vec![Caret::at(m.line, m.col)];
            self.scroll_to_caret();
        }
    }

    /// The editor's current selection as text, if any. The find bar seeds its
    /// query with this, so Ctrl+F on a selected word searches for that word.
    pub fn selected_text_public(&self) -> Option<String> {
        self.selected_text()
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
        let c = self.primary();
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
            .filter_map(Caret::selection)
            .map(|(s, e)| self.range_char_len(s, e))
            .sum()
    }

    fn range_char_len(&self, s: (usize, usize), e: (usize, usize)) -> usize {
        if s.0 == e.0 {
            return e.1.saturating_sub(s.1);
        }
        let start_len = self.lines.get(s.0).map_or(0, |l| l.chars().count());
        let mut total = start_len.saturating_sub(s.1) + 1;
        for l in (s.0 + 1)..e.0 {
            total += self.lines.get(l).map_or(0, |l| l.chars().count()) + 1;
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
            text.split('\n')
                .map(std::string::ToString::to_string)
                .collect()
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
        let line_len = |lines: &[String], l: usize| lines.get(l).map_or(0, |s| s.chars().count());
        for c in &mut self.carets {
            c.line = c.line.min(last_line);
            let cols = line_len(&self.lines, c.line);
            c.col = c.col.min(cols);
            if let Some((al, ac)) = c.anchor {
                let al = al.min(last_line);
                let ac = ac.min(line_len(&self.lines, al));
                c.anchor = Some((al, ac));
            }
        }
    }

    /// Convert a (line, col) char position to an absolute char index into the
    /// flat buffer (for CRDT splicing).
    fn char_index(&self, line: usize, col: usize) -> usize {
        let mut idx = 0;
        for l in 0..line {
            idx += self.lines.get(l).map_or(0, |s| s.chars().count()) + 1; // +1 for the '\n'
        }
        idx + col
    }

    /// Inverse of [`char_index`]: map a flat char index into the buffer back to
    /// a `(line, col)` position. Used to convert the byte/char offsets the
    /// shared `ui::text_selection` word-boundary walkers return into the
    /// editor's line/column caret model.
    fn pos_from_flat(&self, mut idx: usize) -> (usize, usize) {
        for (l, line) in self.lines.iter().enumerate() {
            let len = line.chars().count();
            if idx <= len {
                return (l, idx);
            }
            idx -= len + 1; // consume the line's chars plus its trailing '\n'
        }
        let last = self.lines.len().saturating_sub(1);
        (last, self.lines.get(last).map_or(0, |s| s.chars().count()))
    }

    /// The primary caret's ordered selection as flat char indices, or `None`.
    fn primary_flat_selection(&self) -> Option<(usize, usize)> {
        let (s, e) = self.primary().selection()?;
        Some((self.char_index(s.0, s.1), self.char_index(e.0, e.1)))
    }

    /// The text covered by the primary caret's selection, sliced out of the
    /// flat buffer (`cached_text`). `None` when nothing is selected.
    fn selected_text(&self) -> Option<String> {
        let (s, e) = self.primary_flat_selection()?;
        if s >= e {
            return None;
        }
        Some(self.cached_text.chars().skip(s).take(e - s).collect())
    }

    /// Move the primary caret to a flat char index, collapsing multi-carets.
    /// `extend` keeps (or promotes) the selection anchor; otherwise the
    /// selection collapses.
    fn set_primary_flat(&mut self, target: usize, extend: bool) {
        self.carets.truncate(1);
        if self.carets.is_empty() {
            self.carets.push(Caret::at(0, 0));
        }
        let (cl, cc) = (self.primary().line, self.primary().col);
        let (nl, nc) = self.pos_from_flat(target);
        let Some(c) = self.carets.first_mut() else {
            return;
        };
        if extend {
            if c.anchor.is_none() {
                c.anchor = Some((cl, cc));
            }
        } else {
            c.anchor = None;
        }
        c.line = nl;
        c.col = nc;
        self.scroll_to_caret();
    }

    /// Word-wise caret motion (Ctrl+Left / Ctrl+Right, and their shift-extend
    /// variants), delegating the boundary walk to the shared
    /// `ui::text_selection` char-indexed helpers so the editor uses exactly the
    /// same word semantics as `TextInput`.
    fn word_move(&mut self, forward: bool, extend: bool) {
        let primary = self.primary();
        let flat = self.char_index(primary.line, primary.col);
        let target = if forward {
            next_word_boundary_chars(&self.cached_text, flat)
        } else {
            prev_word_boundary_chars(&self.cached_text, flat)
        };
        self.set_primary_flat(target, extend);
    }

    /// Select the whole word under `(line, col)` via the shared
    /// `word_boundaries_chars` helper (double-click). Returns the flat span so
    /// a following drag can extend from it.
    fn select_word_at(&mut self, line: usize, col: usize) -> (usize, usize) {
        self.carets.truncate(1);
        let flat = self.char_index(line, col);
        let (ws, we) = word_boundaries_chars(&self.cached_text, flat);
        let (al, ac) = self.pos_from_flat(ws);
        let (cl, cc) = self.pos_from_flat(we);
        self.carets = vec![Caret {
            line: cl,
            col: cc,
            anchor: if ws == we { None } else { Some((al, ac)) },
        }];
        self.dirty = true;
        (ws, we)
    }

    /// Select the whole line `line` (triple-click). Returns the flat span.
    fn select_line_at(&mut self, line: usize) -> (usize, usize) {
        let len = self.lines.get(line).map_or(0, |l| l.chars().count());
        self.carets = vec![Caret {
            line,
            col: len,
            anchor: Some((line, 0)),
        }];
        self.dirty = true;
        (self.char_index(line, 0), self.char_index(line, len))
    }

    /// Set the primary selection from an anchor+cursor pair of flat indices,
    /// collapsing to a bare caret when the two coincide.
    fn set_primary_range(&mut self, anchor: usize, cursor: usize) {
        let (al, ac) = self.pos_from_flat(anchor);
        let (cl, cc) = self.pos_from_flat(cursor);
        self.carets = vec![Caret {
            line: cl,
            col: cc,
            anchor: if anchor == cursor {
                None
            } else {
                Some((al, ac))
            },
        }];
        self.dirty = true;
    }

    /// Copy the primary selection to the system clipboard (no-op when empty).
    fn copy_selection(&self) {
        if let Some(t) = self.selected_text() {
            clipboard_copy(&t);
        }
    }

    /// Cut: copy the primary selection, then delete it through the CRDT.
    fn cut_selection(&mut self, doc: &mut AutoCommit) {
        if let Some(t) = self.selected_text() {
            clipboard_copy(&t);
            // `backspace` deletes an active selection wholesale.
            self.backspace(doc);
        }
    }

    /// Paste clipboard text at the caret, replacing any selection.
    fn paste_clipboard(&mut self, doc: &mut AutoCommit) {
        if let Some(t) = clipboard_paste()
            && !t.is_empty()
        {
            self.insert(doc, &t);
        }
    }

    // ---- language-server surface -----------------------------------------

    /// Replace the editor's inline diagnostics from a fresh LSP set.
    pub fn set_diagnostics(&mut self, diags: &[LspDiagnostic]) {
        self.diagnostics = diags
            .iter()
            .map(|d| InlineDiag {
                line: d.range.start.line as usize,
                start_col: d.range.start.character as usize,
                end_col: if d.range.end.line == d.range.start.line {
                    d.range.end.character as usize
                } else {
                    usize::MAX // multi-line: squiggle to end of the start line
                },
                severity: severity_from_lsp(d.severity),
                message: d.message.clone(),
            })
            .collect();
        self.dirty = true;
    }

    /// Show a completion popup for `items` (hidden when empty). The anchor is
    /// the identifier prefix under the caret, which accepting will replace.
    pub fn show_completions(&mut self, items: Vec<CompletionItem>) {
        if items.is_empty() {
            self.hide_completions();
            return;
        }
        let (line, col) = self.primary_caret();
        let flat = self.char_index(line, col);
        let start = prev_word_boundary_chars(&self.cached_text, flat).min(flat);
        // Only treat it as a word-prefix if the run start..caret is a word.
        self.completion_anchor = self.pos_from_flat(start);
        self.completions = items;
        self.completion_index = 0;
        self.completion_active = true;
        self.dirty = true;
    }

    /// Dismiss the completion popup.
    pub fn hide_completions(&mut self) {
        if self.completion_active {
            self.dirty = true;
        }
        self.completion_active = false;
        self.completions.clear();
    }

    /// Whether the completion popup is currently capturing navigation keys.
    pub fn completion_active(&self) -> bool {
        self.completion_active && !self.completions.is_empty()
    }

    /// Accept the highlighted completion: replace the identifier prefix under
    /// the caret with the item's insert text, splicing through the CRDT.
    fn accept_completion(&mut self, doc: &mut AutoCommit) {
        if !self.completion_active() {
            return;
        }
        let Some(item) = self.completions.get(self.completion_index).cloned() else {
            return;
        };
        let (line, col) = self.primary_caret();
        // Replace [completion_anchor, caret) with the insert text.
        self.carets = vec![Caret {
            line,
            col,
            anchor: Some(self.completion_anchor),
        }];
        self.insert(doc, &item.insert_text);
        self.hide_completions();
    }

    /// Set (or clear) the hover tooltip text.
    pub fn set_hover(&mut self, text: Option<String>) {
        self.hover_text = text;
        self.dirty = true;
    }

    /// The message of the diagnostic whose span contains `(line, col)`, if any.
    /// Powers an instant, server-free hover when the caret sits on a squiggle.
    fn diagnostic_message_at(&self, line: usize, col: usize) -> Option<String> {
        self.diagnostics.iter().find_map(|d| {
            if d.line != line {
                return None;
            }
            let end = if d.end_col == usize::MAX {
                usize::MAX
            } else {
                d.end_col
            };
            if col >= d.start_col && col <= end {
                Some(d.message.clone())
            } else {
                None
            }
        })
    }

    /// Move the primary caret to an absolute (line, col), clamped to the buffer.
    pub fn move_caret_to(&mut self, line: usize, col: usize) {
        self.carets = vec![Caret::at(line, col)];
        self.clamp_carets();
        self.scroll_to_caret();
        self.dirty = true;
    }

    /// Drain a pending language-server request the user triggered by a keybind.
    pub fn take_lsp_ask(&mut self) -> Option<LspAsk> {
        self.lsp_ask.take()
    }

    /// Set the primary selection to span `anchor..cursor` in (line, col) space.
    /// Used by the headless `--shots` scenarios to stage a visible selection.
    pub fn set_selection_lc(&mut self, anchor: (usize, usize), cursor: (usize, usize)) {
        self.carets = vec![Caret {
            line: cursor.0,
            col: cursor.1,
            anchor: Some(anchor),
        }];
        self.clamp_carets();
        self.scroll_to_caret();
        self.dirty = true;
    }

    /// The buffer's line count — used by shots scenarios to pick a valid line.
    pub fn line_count(&self) -> usize {
        self.lines.len()
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
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        // Process carets bottom-to-top so a splice doesn't shift the indices
        // of carets we haven't handled yet.
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, _) in order {
            self.apply_insert_at(doc, &text, i, s);
        }
        self.sync_from_doc(doc);
        self.scroll_to_caret();
    }

    fn apply_insert_at(&mut self, doc: &mut AutoCommit, text: &ObjId, i: usize, s: &str) {
        let Some(caret) = self.carets.get(i).copied() else {
            return;
        };
        // Delete selection first if present.
        let start = if let Some((sel_s, sel_e)) = caret.selection() {
            let s_idx = self.char_index(sel_s.0, sel_s.1);
            let e_idx = self.char_index(sel_e.0, sel_e.1);
            EditorDoc::splice(doc, text, s_idx, e_idx - s_idx, "");
            if let Some(c) = self.carets.get_mut(i) {
                c.anchor = None;
            }
            sel_s
        } else {
            (caret.line, caret.col)
        };
        let idx = self.char_index(start.0, start.1);
        EditorDoc::splice(doc, text, idx, 0, s);
        // Recompute caret position after the inserted text.
        let inserted_lines = s.split('\n').count();
        let Some(c) = self.carets.get_mut(i) else {
            return;
        };
        if inserted_lines == 1 {
            *c = Caret::at(start.0, start.1 + s.chars().count());
        } else {
            let last = s.rsplit('\n').next().unwrap_or("");
            *c = Caret::at(start.0 + inserted_lines - 1, last.chars().count());
        }
    }

    /// Backspace at every caret. With a selection, deletes it; otherwise
    /// removes the char before the caret (or joins with the previous line).
    fn backspace(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, caret) in order {
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                EditorDoc::splice(doc, &text, s_idx, e_idx - s_idx, "");
                if let Some(c) = self.carets.get_mut(i) {
                    *c = Caret::at(s.0, s.1);
                }
            } else {
                let idx = self.char_index(caret.line, caret.col);
                if idx == 0 {
                    continue;
                }
                EditorDoc::splice(doc, &text, idx - 1, 1, "");
                // Position recomputed after sync from the new flat text; we
                // estimate here and clamp later.
                if caret.col > 0 {
                    if let Some(c) = self.carets.get_mut(i) {
                        *c = Caret::at(caret.line, caret.col - 1);
                    }
                } else if caret.line > 0 {
                    let prev_len = self
                        .lines
                        .get(caret.line - 1)
                        .map_or(0, |l| l.chars().count());
                    if let Some(c) = self.carets.get_mut(i) {
                        *c = Caret::at(caret.line - 1, prev_len);
                    }
                }
            }
        }
        self.sync_from_doc(doc);
        self.scroll_to_caret();
    }

    /// Forward-delete at every caret.
    fn delete_forward(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, caret) in order {
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                EditorDoc::splice(doc, &text, s_idx, e_idx - s_idx, "");
                if let Some(c) = self.carets.get_mut(i) {
                    *c = Caret::at(s.0, s.1);
                }
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
    fn sync_from_doc(&mut self, doc: &AutoCommit) {
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
        // A plain horizontal arrow with a live selection collapses to the
        // selection edge instead of moving one character past it — every
        // native editor does this, and so does `TextInput`.
        if dx != 0
            && dy == 0
            && !extend
            && let Some((s, e)) = self.primary().selection()
        {
            let target = if dx < 0 { s } else { e };
            self.carets = vec![Caret::at(target.0, target.1)];
            self.scroll_to_caret();
            return;
        }
        if self.carets.is_empty() {
            self.carets.push(Caret::at(0, 0));
        }
        let Some(c) = self.carets.first_mut() else {
            return;
        };
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        if dy != 0 {
            let new_line = (c.line as isize + dy).clamp(0, self.lines.len() as isize - 1) as usize;
            c.line = new_line;
            let cols = self.lines.get(new_line).map_or(0, |l| l.chars().count());
            c.col = c.col.min(cols);
        }
        if dx != 0 {
            let cols = self.lines.get(c.line).map_or(0, |l| l.chars().count());
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
                c.col = self.lines.get(c.line).map_or(0, |l| l.chars().count());
            }
        }
        self.scroll_to_caret();
    }

    fn move_home(&mut self, extend: bool) {
        self.carets.truncate(1);
        if self.carets.is_empty() {
            self.carets.push(Caret::at(0, 0));
        }
        let Some(c) = self.carets.first_mut() else {
            return;
        };
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        // Smart home: first non-whitespace, then column 0.
        let first_non_ws = self.lines.get(c.line).map_or(0, |line| {
            line.chars().take_while(|ch| ch.is_whitespace()).count()
        });
        c.col = if c.col == first_non_ws {
            0
        } else {
            first_non_ws
        };
        self.scroll_to_caret();
    }

    fn move_end(&mut self, extend: bool) {
        self.carets.truncate(1);
        if self.carets.is_empty() {
            self.carets.push(Caret::at(0, 0));
        }
        let Some(c) = self.carets.first_mut() else {
            return;
        };
        if extend && c.anchor.is_none() {
            c.anchor = Some((c.line, c.col));
        }
        if !extend {
            c.anchor = None;
        }
        c.col = self.lines.get(c.line).map_or(0, |l| l.chars().count());
        self.scroll_to_caret();
    }

    fn select_all(&mut self) {
        let last = self.lines.len().saturating_sub(1);
        let last_col = self.lines.get(last).map_or(0, |l| l.chars().count());
        self.carets = vec![Caret {
            line: last,
            col: last_col,
            anchor: Some((0, 0)),
        }];
        self.dirty = true;
    }

    /// Add a caret on the line below the lowest current caret (multi-cursor).
    fn add_caret_below(&mut self) {
        if let Some(last) = self.carets.iter().max_by_key(|c| c.line).copied()
            && let Some(cols) = self.lines.get(last.line + 1).map(|l| l.chars().count())
        {
            self.carets
                .push(Caret::at(last.line + 1, last.col.min(cols)));
            self.dirty = true;
        }
    }

    /// Insert a newline plus auto-indent matching the current line's leading
    /// whitespace — the single most-felt editor nicety.
    fn newline_with_indent(&mut self, doc: &mut AutoCommit) {
        let (line, _) = self.primary_caret();
        let indent: String = self
            .lines
            .get(line)
            .map(|l| {
                l.chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .collect()
            })
            .unwrap_or_default();
        let mut payload = String::from("\n");
        payload.push_str(&indent);
        self.insert(doc, &payload);
    }

    /// Indent: insert `indent_width` spaces at the caret.
    fn insert_indent(&mut self, doc: &mut AutoCommit) {
        let spaces = " ".repeat(self.indent_width);
        self.insert(doc, &spaces);
    }

    // ---- line operations (all route through the CRDT) ----

    /// The inclusive `[first, last]` line range the primary caret covers. A
    /// selection that ends exactly at column 0 of a line does not pull that
    /// trailing line in — matching every editor's line-op behaviour, so that
    /// selecting down to the start of a line and running "delete line" does not
    /// eat the line the caret only touches.
    fn primary_line_range(&self) -> (usize, usize) {
        let c = self.primary();
        match c.selection() {
            Some((s, e)) => {
                let last = if e.1 == 0 && e.0 > s.0 { e.0 - 1 } else { e.0 };
                (s.0, last)
            }
            None => (c.line, c.line),
        }
    }

    /// The flat char index of the first character of `line`.
    fn line_start_index(&self, line: usize) -> usize {
        self.char_index(line, 0)
    }

    /// Reposition the primary caret (and its selection anchor) by `delta` lines,
    /// keeping the column. Used after a line move so the caret rides with the
    /// text it moved.
    fn shift_primary_lines(&mut self, delta: isize) {
        let cur = self.primary();
        let shift = |l: usize| (l as isize + delta).max(0) as usize;
        self.carets = vec![Caret {
            line: shift(cur.line),
            col: cur.col,
            anchor: cur.anchor.map(|(al, ac)| (shift(al), ac)),
        }];
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Duplicate the caret's line(s), inserting the copy directly below. With
    /// `focus_copy` the caret rides down onto the new copy (copy-down); without
    /// it the caret stays on the original (copy-up). Bound to Shift+Alt+Down /
    /// Shift+Alt+Up.
    pub fn duplicate_lines(&mut self, doc: &mut AutoCommit, focus_copy: bool) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        let block: String = self
            .lines
            .get(a..=b)
            .map(|s| s.join("\n"))
            .unwrap_or_default();
        let end_of_b = self.char_index(b, self.lines.get(b).map_or(0, |l| l.chars().count()));
        EditorDoc::splice(doc, &text, end_of_b, 0, &format!("\n{block}"));
        self.sync_from_doc(doc);
        if focus_copy {
            self.shift_primary_lines((b - a + 1) as isize);
        } else {
            self.scroll_to_caret();
        }
    }

    /// Move the caret's line(s) up or down by one, swapping with the neighbour.
    /// A single CRDT splice, so it undoes as one step. Bound to Alt+Up/Alt+Down.
    pub fn move_lines(&mut self, doc: &mut AutoCommit, up: bool) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        let block: String = self
            .lines
            .get(a..=b)
            .map(|s| s.join("\n"))
            .unwrap_or_default();
        if up {
            if a == 0 {
                return;
            }
            let Some(prev) = self.lines.get(a - 1).cloned() else {
                return;
            };
            let from = self.line_start_index(a - 1);
            let to = self.char_index(b, self.lines.get(b).map_or(0, |l| l.chars().count()));
            // Region is "{prev}\n{block}" → swap to "{block}\n{prev}".
            EditorDoc::splice(doc, &text, from, to - from, &format!("{block}\n{prev}"));
            self.sync_from_doc(doc);
            self.shift_primary_lines(-1);
        } else {
            if b + 1 >= self.lines.len() {
                return;
            }
            let Some(next) = self.lines.get(b + 1).cloned() else {
                return;
            };
            let from = self.line_start_index(a);
            let to = self.char_index(
                b + 1,
                self.lines.get(b + 1).map_or(0, |l| l.chars().count()),
            );
            // Region is "{block}\n{next}" → swap to "{next}\n{block}".
            EditorDoc::splice(doc, &text, from, to - from, &format!("{next}\n{block}"));
            self.sync_from_doc(doc);
            self.shift_primary_lines(1);
        }
    }

    /// Delete the caret's line(s) entirely, landing the caret at the start of
    /// what follows. Bound to Ctrl+Shift+K.
    pub fn delete_lines(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        let (from, to) = if b + 1 < self.lines.len() {
            // Take the block and its trailing newline so the line vanishes.
            (self.line_start_index(a), self.line_start_index(b + 1))
        } else if a > 0 {
            // Last line(s): take the leading newline instead.
            (
                self.char_index(
                    a - 1,
                    self.lines.get(a - 1).map_or(0, |l| l.chars().count()),
                ),
                self.char_index(b, self.lines.get(b).map_or(0, |l| l.chars().count())),
            )
        } else {
            // The whole buffer is selected — leave a single empty line.
            (
                0,
                self.char_index(b, self.lines.get(b).map_or(0, |l| l.chars().count())),
            )
        };
        EditorDoc::splice(doc, &text, from, to - from, "");
        self.sync_from_doc(doc);
        let target = a.min(self.lines.len().saturating_sub(1));
        self.carets = vec![Caret::at(target, 0)];
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Toggle a line comment on the caret's line(s). If every non-blank line in
    /// the range is already commented, uncomment; otherwise comment them all
    /// (at the range's shallowest indent, so the comment markers line up).
    /// No-op for languages without a line comment. Bound to Ctrl+/.
    pub fn toggle_line_comment(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let Some(token) = self.language.line_comment() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        let non_blank: Vec<usize> = (a..=b)
            .filter(|&l| !self.lines.get(l).is_none_or(|s| s.trim().is_empty()))
            .collect();
        if non_blank.is_empty() {
            return;
        }
        let all_commented = non_blank.iter().all(|&l| {
            self.lines
                .get(l)
                .is_some_and(|s| s.trim_start().starts_with(token))
        });
        // Shallowest indent across the range, so added markers align.
        let indent = non_blank
            .iter()
            .map(|&l| {
                self.lines.get(l).map_or(0, |s| {
                    s.chars().take_while(|c| *c == ' ' || *c == '\t').count()
                })
            })
            .min()
            .unwrap_or(0);
        // Splice bottom-up so earlier line indices stay valid.
        for &l in non_blank.iter().rev() {
            let Some(line) = self.lines.get(l) else {
                continue;
            };
            if all_commented {
                // Remove the token (and one following space, if present) at the
                // first non-whitespace column.
                let lead = line.chars().take_while(|c| c.is_whitespace()).count();
                let after_start = line.char_indices().nth(lead).map_or(line.len(), |(i, _)| i);
                let after = line.get(after_start..).unwrap_or("");
                if let Some(rest) = after.strip_prefix(token) {
                    let mut remove = token.chars().count();
                    if rest.starts_with(' ') {
                        remove += 1;
                    }
                    let at = self.char_index(l, lead);
                    EditorDoc::splice(doc, &text, at, remove, "");
                }
            } else {
                let at = self.char_index(l, indent);
                EditorDoc::splice(doc, &text, at, 0, &format!("{token} "));
            }
        }
        self.sync_from_doc(doc);
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Indent every line the selection spans by one indent unit. Falls back to
    /// a plain tab-at-caret when the selection is empty or single-line (handled
    /// by the caller). Bound to Tab with a multi-line selection.
    pub fn indent_selection(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        let pad = " ".repeat(self.indent_width);
        for l in (a..=b).rev() {
            if self.lines.get(l).is_none_or(String::is_empty) {
                continue;
            }
            let at = self.line_start_index(l);
            EditorDoc::splice(doc, &text, at, 0, &pad);
        }
        self.sync_from_doc(doc);
        self.clamp_carets();
    }

    /// Outdent every line the selection spans by up to one indent unit. Bound
    /// to Shift+Tab.
    pub fn outdent_selection(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.text_obj.clone() else {
            return;
        };
        let (a, b) = self.primary_line_range();
        for l in (a..=b).rev() {
            let Some(line) = self.lines.get(l) else {
                continue;
            };
            // Remove up to `indent_width` leading spaces, or a single leading tab.
            let remove = if line.starts_with('\t') {
                1
            } else {
                line.chars()
                    .take_while(|c| *c == ' ')
                    .count()
                    .min(self.indent_width)
            };
            if remove > 0 {
                let at = self.line_start_index(l);
                EditorDoc::splice(doc, &text, at, remove, "");
            }
        }
        self.sync_from_doc(doc);
        self.clamp_carets();
    }

    /// Whether the primary selection spans more than one line — the caller uses
    /// this to decide between block-indent and a plain tab insertion.
    pub fn selection_is_multiline(&self) -> bool {
        let (a, b) = self.primary_line_range();
        b > a
    }

    // ---- hit testing ----

    /// Map a pixel point to a (line, col) caret position.
    fn point_to_caret(&self, p: Point) -> (usize, usize) {
        let gutter = self.gutter_width();
        let rel_y = p.y - self.rect.y0 + self.scroll_y * LINE_HEIGHT;
        let line = ((rel_y / LINE_HEIGHT).floor() as isize).clamp(0, self.lines.len() as isize - 1)
            as usize;
        let rel_x = p.x - self.rect.x0 - gutter - CODE_PAD_LEFT + self.scroll_x;
        let max_col = self.lines.get(line).map_or(0, |l| l.chars().count());
        let col = ((rel_x / self.cell_w).round().max(0.0) as usize).min(max_col);
        (line, col)
    }

    // ---- draw helpers (split out of `Widget::draw`, which orchestrates them) ----

    /// Measure the real monospace cell width once a renderer is available,
    /// caching it on `self` (the fallback stays in place until the first
    /// successful measurement).
    fn measure_cell_width(&mut self, text_renderer: &mut TextRenderer) {
        let (mw, _) =
            text_renderer.measure_styled("M", FONT_SIZE, None, TextWeight::Regular, false, true);
        if mw > 0.0 {
            self.cell_w = f64::from(mw);
        }
    }

    /// The geometry a draw pass threads through: gutter width, the code
    /// area's left edge, the first (possibly partial) visible line, its
    /// fractional scroll offset, and how many rows fit in the viewport.
    fn layout_metrics(&self) -> (f64, f64, usize, f64, usize) {
        let gutter_w = self.gutter_width();
        let code_x0 = self.rect.x0 + gutter_w + CODE_PAD_LEFT;
        let first_line = self.scroll_y.floor() as usize;
        let frac = self.scroll_y - first_line as f64;
        let n_visible = self.visible_lines();
        (gutter_w, code_x0, first_line, frac, n_visible)
    }

    /// Fill the editor background and the (subtly raised) gutter background.
    fn draw_backgrounds(&self, scene: &mut Scene, transform: Affine, theme: &Theme, gutter_w: f64) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let gutter_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + gutter_w,
            self.rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &gutter_rect,
        );
    }

    /// Draw the per-line background bands for every visible row: the
    /// active-line highlight, find-bar match bands, and selection bands.
    /// Runs as its own pass over the visible rows (before [`Self::draw_line_text`]
    /// paints line numbers and code on top) — safe because rows never overlap,
    /// so the pass order only has to hold within a single row, not across rows.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass over one screenful of lines threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_line_backgrounds(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        gutter_w: f64,
        code_x0: f64,
        first_line: usize,
        frac: f64,
        n_visible: usize,
        primary_line: usize,
    ) {
        let c = &theme.tokens.colors;
        for vi in 0..n_visible {
            let line_idx = first_line + vi;
            let Some(line) = self.lines.get(line_idx) else {
                break;
            };
            let y = self.rect.y0 + (vi as f64 - frac) * LINE_HEIGHT;
            if y > self.rect.y1 {
                break;
            }

            // Active-line highlight across the code area.
            if line_idx == primary_line && self.carets.len() == 1 {
                let hl = Rect::new(self.rect.x0 + gutter_w, y, self.rect.x1, y + LINE_HEIGHT);
                scene.fill(Fill::NonZero, transform, c.surface_white_5, None, &hl);
            }

            // Find-bar matches on this line, under the selection so a selected
            // current match still reads as selected.
            for (mi, m) in self.find_matches.iter().enumerate() {
                if m.line != line_idx {
                    continue;
                }
                let mx0 = code_x0 + m.col as f64 * self.cell_w - self.scroll_x;
                let mx1 = code_x0 + (m.col + m.len) as f64 * self.cell_w - self.scroll_x;
                let band = Rect::new(
                    mx0.max(self.rect.x0 + gutter_w),
                    y + 1.0,
                    mx1,
                    y + LINE_HEIGHT - 1.0,
                );
                if band.width() <= 0.0 {
                    continue;
                }
                let is_current = self.find_current == Some(mi);
                // The current match is filled; the rest are outlined, so the
                // one you are on is unmistakable among thirty others.
                if is_current {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        c.semantic.warning.multiply_alpha(0.45),
                        None,
                        &band,
                    );
                } else {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        c.semantic.warning.multiply_alpha(0.16),
                        None,
                        &band,
                    );
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        c.semantic.warning.multiply_alpha(0.5),
                        None,
                        &band,
                    );
                }
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
                        line.chars().count() + 1 // include EOL
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
        }
    }

    /// Draw the gutter line number and syntax-highlighted code text for every
    /// visible row (the pass on top of [`Self::draw_line_backgrounds`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass over one screenful of lines threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_line_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        gutter_w: f64,
        code_x0: f64,
        first_line: usize,
        frac: f64,
        n_visible: usize,
        primary_line: usize,
    ) {
        let c = &theme.tokens.colors;
        for vi in 0..n_visible {
            let line_idx = first_line + vi;
            let Some(line) = self.lines.get(line_idx) else {
                break;
            };
            let y = self.rect.y0 + (vi as f64 - frac) * LINE_HEIGHT;
            if y > self.rect.y1 {
                break;
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
                    let prefix = line
                        .as_bytes()
                        .get(..span.start.min(line.len()))
                        .unwrap_or(&[]);
                    let char_col = std::str::from_utf8(prefix).map_or(0, |s| s.chars().count());
                    let seg = line.get(span.start..span.end.min(line.len())).unwrap_or("");
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
    }

    /// Draw a coloured wavy underline under each inline diagnostic's span
    /// (from the language server) that falls within the visible line range.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_diagnostics(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        first_line: usize,
        n_visible: usize,
        code_x0: f64,
        gutter_w: f64,
    ) {
        let c = &theme.tokens.colors;
        for d in &self.diagnostics {
            if d.line < first_line || d.line >= first_line + n_visible {
                continue;
            }
            let y = self.rect.y0 + (d.line as f64 - self.scroll_y) * LINE_HEIGHT;
            if y > self.rect.y1 {
                continue;
            }
            let line_len = self.lines.get(d.line).map_or(0, |l| l.chars().count());
            let start_col = d.start_col.min(line_len);
            let end_col = if d.end_col == usize::MAX {
                line_len
            } else {
                d.end_col.min(line_len)
            }
            .max(start_col + 1);
            let x_left = (code_x0 + start_col as f64 * self.cell_w - self.scroll_x)
                .max(self.rect.x0 + gutter_w);
            let x_right = code_x0 + end_col as f64 * self.cell_w - self.scroll_x;
            if x_right <= x_left {
                continue;
            }
            let sy = y + LINE_HEIGHT - 3.0;
            let color = match d.severity {
                Severity::Error => c.semantic.danger,
                Severity::Warning => c.semantic.warning,
                Severity::Info => c.semantic.info,
                Severity::Hint => c.text_dim,
            };
            // Zigzag underline — a couple of px amplitude reads as a squiggle.
            // Stepped by an integer segment count (rather than a `while x <
            // x_right` float loop) so the number of iterations is exact.
            let mut path = BezPath::new();
            path.move_to((x_left, sy));
            let span = (x_right - x_left).max(0.0);
            let segments = (span / 3.0).ceil() as usize;
            let mut up = true;
            for seg in 1..=segments {
                let nx = (x_left + seg as f64 * 3.0).min(x_right);
                path.line_to((nx, if up { sy - 1.6 } else { sy + 1.6 }));
                up = !up;
            }
            scene.stroke(&Stroke::new(1.2), transform, color, None, &path);
        }
    }

    /// Draw the caret(s) as a thin vertical stroke, blink-free for screenshot
    /// determinism, for whichever carets fall in the visible line range.
    /// No-op while the view isn't focused.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_carets(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        first_line: usize,
        n_visible: usize,
        code_x0: f64,
        gutter_w: f64,
    ) {
        if !self.is_focused {
            return;
        }
        let c = &theme.tokens.colors;
        for caret in &self.carets {
            if caret.line < first_line || caret.line >= first_line + n_visible {
                continue;
            }
            let y = self.rect.y0 + (caret.line as f64 - self.scroll_y) * LINE_HEIGHT;
            let x = code_x0 + caret.col as f64 * self.cell_w - self.scroll_x;
            if x < self.rect.x0 + gutter_w {
                continue;
            }
            let caret_line =
                Line::new(Point::new(x, y + 2.0), Point::new(x, y + LINE_HEIGHT - 2.0));
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                c.border_focus,
                None,
                &caret_line,
            );
        }
    }

    /// Draw the completion popup: a floating, scrollable list anchored under
    /// the caret, with the highlighted row filled and each item's kind glyph,
    /// label and (space permitting) detail text. No-op when the popup isn't
    /// showing.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_completion_popup(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        gutter_w: f64,
        caret_x: f64,
        caret_top: f64,
    ) {
        const ROW_H: f64 = 20.0;
        if !self.completion_active() {
            return;
        }
        let c = &theme.tokens.colors;
        let max_rows = 8usize;
        let total = self.completions.len();
        let shown = total.min(max_rows);
        // Scroll the window so the selected row stays visible.
        let offset = if self.completion_index >= max_rows {
            self.completion_index - max_rows + 1
        } else {
            0
        };
        let width = 340.0_f64.min(self.rect.width() - (gutter_w + 20.0));
        let height = shown as f64 * ROW_H + 8.0;
        let px = caret_x
            .min(self.rect.x1 - width - 8.0)
            .max(self.rect.x0 + gutter_w + 2.0);
        let py = (caret_top + LINE_HEIGHT + 2.0)
            .min(self.rect.y1 - height - 4.0)
            .max(self.rect.y0);
        let popup = Rect::new(px, py, px + width, py + height);
        let shape = RoundedRect::from_rect(popup, 6.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.overlay, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &shape,
        );

        for row in 0..shown {
            let idx = offset + row;
            let Some(item) = self.completions.get(idx) else {
                break;
            };
            let ry = py + 4.0 + row as f64 * ROW_H;
            if idx == self.completion_index {
                let hl = Rect::new(px + 2.0, ry, px + width - 2.0, ry + ROW_H);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.accent[5].multiply_alpha(0.30),
                    None,
                    &hl,
                );
            }
            // Kind glyph (compact hint at the identifier's type).
            let glyph = completion_kind_glyph(item.kind);
            text_renderer.draw_text_styled(
                scene,
                glyph,
                FONT_SIZE,
                c.accent[6],
                transform * Affine::translate((px + 8.0, ry + 3.0)),
                None,
                TextWeight::Regular,
                false,
                true,
            );
            text_renderer.draw_text_styled(
                scene,
                &item.label,
                FONT_SIZE,
                c.text_primary,
                transform * Affine::translate((px + 26.0, ry + 3.0)),
                None,
                TextWeight::Regular,
                false,
                true,
            );
            if let Some(detail) = &item.detail {
                let (lw, _) = text_renderer.measure_styled(
                    &item.label,
                    FONT_SIZE,
                    None,
                    TextWeight::Regular,
                    false,
                    true,
                );
                text_renderer.draw_text_styled(
                    scene,
                    detail,
                    FONT_SIZE - 1.0,
                    c.text_dim,
                    transform * Affine::translate((px + 34.0 + f64::from(lw), ry + 3.5)),
                    Some((width - 40.0 - f64::from(lw)) as f32),
                    TextWeight::Regular,
                    false,
                    true,
                );
            }
        }
    }

    /// Draw the hover tooltip: a small box near the caret with the server's
    /// text, preferring above the caret and dropping below when there's no
    /// room. No-op when there is no hover text to show.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw pass threading through the same handful of already-computed geometry values; grouping them into a struct would not make the call clearer"
    )]
    fn draw_hover_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        gutter_w: f64,
        caret_x: f64,
        caret_top: f64,
    ) {
        let Some(hover) = self.hover_text.clone() else {
            return;
        };
        let c = &theme.tokens.colors;
        let first: String = hover.lines().take(4).collect::<Vec<_>>().join("\n");
        let (tw, _) = text_renderer.measure(&first, FONT_SIZE, None);
        let width = (f64::from(tw) + 20.0).min(self.rect.width() - (gutter_w + 20.0));
        let lines = first.lines().count().max(1);
        let height = lines as f64 * (f64::from(FONT_SIZE) + 4.0) + 12.0;
        let px = caret_x
            .min(self.rect.x1 - width - 8.0)
            .max(self.rect.x0 + gutter_w + 2.0);
        // Prefer above the caret; drop below if there's no room.
        let py = if caret_top - height - 4.0 >= self.rect.y0 {
            caret_top - height - 4.0
        } else {
            caret_top + LINE_HEIGHT + 2.0
        };
        let box_rect = Rect::new(px, py, px + width, py + height);
        let shape = RoundedRect::from_rect(box_rect, 6.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.overlay, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &shape,
        );
        for (i, line) in first.lines().enumerate() {
            text_renderer.draw_text(
                scene,
                line,
                FONT_SIZE,
                c.text_primary,
                transform
                    * Affine::translate((
                        px + 10.0,
                        py + 6.0 + i as f64 * (f64::from(FONT_SIZE) + 4.0),
                    )),
                Some((width - 20.0) as f32),
            );
        }
    }
}

impl EditorView {
    // ---- event helpers (split out of `Widget::handle_event`, which dispatches to them) ----

    /// Left-click: add a caret (Ctrl/Cmd-click) or resolve the shared click
    /// ladder (single/double/triple, shift-extend) into a char/word/line
    /// selection anchored for the drag that may follow.
    fn handle_mouse_press(&mut self, mods: winit::keyboard::ModifiersState, handled: &mut bool) {
        if !self.rect.contains(self.last_cursor) {
            return;
        }
        let (line, col) = self.point_to_caret(self.last_cursor);
        let shift = mods.shift_key();
        if (mods.control_key() || mods.super_key()) && !shift {
            self.carets.push(Caret::at(line, col));
        } else {
            let intent = self.drag.press(self.last_cursor, shift);
            self.drag_anchor = match intent.granularity {
                Granularity::Word => self.select_word_at(line, col),
                Granularity::Line => self.select_line_at(line),
                Granularity::Char => {
                    let flat = self.char_index(line, col);
                    if intent.extend {
                        // Shift-click: keep the existing anchor (or promote
                        // the current caret) and extend.
                        let primary = self.primary();
                        let (al, ac) = primary.anchor.unwrap_or((primary.line, primary.col));
                        let anchor = self.char_index(al, ac);
                        self.set_primary_range(anchor, flat);
                        (anchor, anchor)
                    } else {
                        self.carets = vec![Caret::at(line, col)];
                        (flat, flat)
                    }
                }
            };
        }
        self.is_focused = true;
        self.dirty = true;
        *handled = true;
    }

    /// Pointer motion: while dragging, extend the selection to the pointer at
    /// the gesture's granularity — char follows the caret; word/line snap so
    /// the original span the press selected stays covered.
    fn handle_cursor_moved(&mut self, next: Point, handled: &mut bool) {
        self.last_cursor = next;
        let Some(gran) = self.drag.drag() else {
            return;
        };
        let (line, col) = self.point_to_caret(self.last_cursor);
        let q = self.char_index(line, col);
        let (anchor, cursor) = match gran {
            Granularity::Char => (self.drag_anchor.0, q),
            Granularity::Word => {
                let (ws, we) = word_boundaries_chars(&self.cached_text, q);
                if q < self.drag_anchor.0 {
                    (self.drag_anchor.1, ws)
                } else {
                    (self.drag_anchor.0, we)
                }
            }
            Granularity::Line => {
                let len = self.lines.get(line).map_or(0, |l| l.chars().count());
                let (ls, le) = (self.char_index(line, 0), self.char_index(line, len));
                if q < self.drag_anchor.0 {
                    (self.drag_anchor.1, ls)
                } else {
                    (self.drag_anchor.0, le)
                }
            }
        };
        self.set_primary_range(anchor, cursor);
        *handled = true;
    }

    /// Mouse wheel: scroll vertically, clamped to the buffer's line count.
    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta, handled: &mut bool) {
        if !self.rect.contains(self.last_cursor) {
            return;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y / LINE_HEIGHT,
        };
        let max = (self.lines.len() as f64 - 1.0).max(0.0);
        self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
        self.dirty = true;
        *handled = true;
    }

    /// A pressed key: completion-popup navigation, LSP request triggers, then
    /// caret motion / editing / clipboard commands, then plain text input.
    /// A pressed key: completion-popup navigation, LSP request triggers, then
    /// caret motion / editing / clipboard commands, then plain text input.
    fn handle_key_press(
        &mut self,
        key_event: &KeyEvent,
        mods: winit::keyboard::ModifiersState,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let logical_key = &key_event.logical_key;
        let text = key_event.text.as_deref();
        let ctrl = mods.control_key() || mods.super_key();
        let shift = mods.shift_key();
        let alt = mods.alt_key();

        // Any keypress dismisses a stale hover tooltip.
        if self.hover_text.is_some() {
            self.hover_text = None;
            self.dirty = true;
        }

        if self.handle_completion_popup_keys(logical_key, state, handled) {
            return;
        }
        if self.handle_lsp_trigger_keys(logical_key, handled) {
            return;
        }
        if self.handle_navigation_keys(logical_key, ctrl, shift, alt, state, handled) {
            return;
        }
        if self.handle_ctrl_shortcut_keys(logical_key, ctrl, shift, state, handled) {
            return;
        }
        self.handle_editing_keys(logical_key, text, ctrl, shift, state, handled);
    }

    /// While the completion popup is open, handle its navigation keys
    /// (Up/Down/Enter/Tab/Escape) and consume the event. Any other key
    /// dismisses the popup and falls through to normal handling. Returns
    /// `true` when the event was fully handled here.
    fn handle_completion_popup_keys(
        &mut self,
        logical_key: &Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if !self.completion_active() {
            return false;
        }
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                let n = self.completions.len();
                self.completion_index = (self.completion_index + 1) % n;
                self.dirty = true;
                *handled = true;
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = self.completions.len();
                self.completion_index = (self.completion_index + n - 1) % n;
                self.dirty = true;
                *handled = true;
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                self.accept_completion(state);
                *handled = true;
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.hide_completions();
                *handled = true;
                true
            }
            _ => {
                self.hide_completions();
                false
            }
        }
    }

    /// F12 (go to definition) and F1 (hover — instant for an on-caret
    /// diagnostic, plus an LSP request). Returns `true` when consumed.
    fn handle_lsp_trigger_keys(&mut self, logical_key: &Key, handled: &mut bool) -> bool {
        if let Key::Named(NamedKey::F12) = logical_key {
            self.lsp_ask = Some(LspAsk::Definition);
            *handled = true;
            return true;
        }
        if let Key::Named(NamedKey::F1) = logical_key {
            // Instant, server-free hover: if the caret sits on a diagnostic,
            // show its message immediately. Also ask the LSP for a richer
            // hover (which supersedes this when it arrives).
            let (cl, cc) = self.primary_caret();
            if let Some(msg) = self.diagnostic_message_at(cl, cc) {
                self.set_hover(Some(msg));
            }
            self.lsp_ask = Some(LspAsk::Hover);
            *handled = true;
            return true;
        }
        false
    }

    /// Caret motion: arrows (plain, Ctrl-by-word, Alt for line move/duplicate,
    /// Ctrl+Alt to add a caret below), Home/End, Page Up/Down. Returns `true`
    /// when the key matched.
    #[allow(
        clippy::too_many_arguments,
        reason = "one key event's already-decoded fields threaded to the caret-motion match below; bundling them into a struct would not make the call clearer"
    )]
    fn handle_navigation_keys(
        &mut self,
        logical_key: &Key,
        ctrl: bool,
        shift: bool,
        alt: bool,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        match logical_key {
            Key::Named(NamedKey::ArrowLeft) => {
                // Ctrl+Left jumps a word; +Shift extends by a word.
                if ctrl {
                    self.word_move(false, shift);
                } else {
                    self.move_caret(-1, 0, shift);
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if ctrl {
                    self.word_move(true, shift);
                } else {
                    self.move_caret(1, 0, shift);
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if ctrl && alt {
                    self.add_caret_below();
                } else if alt && shift {
                    self.duplicate_lines(state, false);
                } else if alt {
                    self.move_lines(state, true);
                } else {
                    self.move_caret(0, -1, shift);
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if ctrl && alt {
                    self.add_caret_below();
                } else if alt && shift {
                    self.duplicate_lines(state, true);
                } else if alt {
                    self.move_lines(state, false);
                } else {
                    self.move_caret(0, 1, shift);
                }
            }
            Key::Named(NamedKey::Home) => self.move_home(shift),
            Key::Named(NamedKey::End) => self.move_end(shift),
            Key::Named(NamedKey::PageUp) => {
                let rows = (self.rect.height() / LINE_HEIGHT).floor() as isize;
                self.move_caret(0, -rows, shift);
            }
            Key::Named(NamedKey::PageDown) => {
                let rows = (self.rect.height() / LINE_HEIGHT).floor() as isize;
                self.move_caret(0, rows, shift);
            }
            _ => return false,
        }
        *handled = true;
        true
    }

    /// Ctrl+letter shortcuts: select-all, copy/cut/paste, add-caret-below,
    /// delete-line, toggle-comment. Returns `true` when the key matched.
    fn handle_ctrl_shortcut_keys(
        &mut self,
        logical_key: &Key,
        ctrl: bool,
        shift: bool,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let Key::Character(s) = logical_key else {
            return false;
        };
        if !ctrl {
            return false;
        }
        match s.to_lowercase().as_str() {
            "a" => self.select_all(),
            "c" => self.copy_selection(),
            "x" => self.cut_selection(state),
            "v" => self.paste_clipboard(state),
            "d" => self.add_caret_below(),
            "k" if shift => self.delete_lines(state),
            "/" => self.toggle_line_comment(state),
            _ => return false,
        }
        *handled = true;
        true
    }

    /// Backspace/Delete/Enter/Tab/Space, and — for anything else — plain
    /// printable text insertion.
    fn handle_editing_keys(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        ctrl: bool,
        shift: bool,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match logical_key {
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
                // Shift+Tab outdents; Tab over a multi-line selection indents
                // the whole block; otherwise it inserts one indent unit at
                // the caret.
                if shift {
                    self.outdent_selection(state);
                } else if self.selection_is_multiline() {
                    self.indent_selection(state);
                } else {
                    self.insert_indent(state);
                }
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                // Ctrl+Space asks the app to fetch completions here.
                if ctrl {
                    self.lsp_ask = Some(LspAsk::Completion);
                } else {
                    self.insert(state, " ");
                }
                *handled = true;
            }
            _ => {
                // Printable text insertion. `text` carries the resolved
                // character(s) for the key (handles shift, layout, dead
                // keys).
                if !ctrl
                    && let Some(s) = text
                    && !s.is_empty()
                    && !s.chars().any(char::is_control)
                {
                    self.insert(state, s);
                    *handled = true;
                }
            }
        }
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
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a single leaf with a fixed Style cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        #[allow(
            clippy::unwrap_used,
            reason = "node was just created by this widget's own build_layout in the same tree; a lookup failure means the taffy tree is already corrupted and there is no meaningful layout to fall back to"
        )]
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
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
        self.measure_cell_width(text_renderer);
        let (gutter_w, code_x0, first_line, frac, n_visible) = self.layout_metrics();
        self.draw_backgrounds(scene, transform, theme, gutter_w);

        if self.text_obj.is_none() {
            // A composed zero-state through the shared primitive: a hero icon,
            // a title, a one-line invitation, and an actionable "Open File"
            // pill that summons the portal picker.
            self.empty_action_rect = EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.rect,
                Some(Icon::FileCode),
                "No file open",
                Some("Open a file from the tree, or pick one to start editing."),
                Some("Open File"),
                self.last_cursor,
            )
            .unwrap_or(Rect::ZERO);
            return;
        }

        let primary_line = self.primary().line;
        self.draw_line_backgrounds(
            scene,
            transform,
            theme,
            gutter_w,
            code_x0,
            first_line,
            frac,
            n_visible,
            primary_line,
        );
        self.draw_line_text(
            scene,
            transform,
            text_renderer,
            theme,
            gutter_w,
            code_x0,
            first_line,
            frac,
            n_visible,
            primary_line,
        );
        self.draw_diagnostics(
            scene, transform, theme, first_line, n_visible, code_x0, gutter_w,
        );
        self.draw_carets(
            scene, transform, theme, first_line, n_visible, code_x0, gutter_w,
        );

        // Right border separating from any side panel.
        let border = Line::new(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x0, self.rect.y1),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &border,
        );

        // Caret pixel, reused by the completion popup + hover tooltip.
        let (caret_line, caret_col) = self.primary_caret();
        let caret_x = code_x0 + caret_col as f64 * self.cell_w - self.scroll_x;
        let caret_top = self.rect.y0 + (caret_line as f64 - self.scroll_y) * LINE_HEIGHT;

        self.draw_completion_popup(
            scene,
            transform,
            text_renderer,
            theme,
            gutter_w,
            caret_x,
            caret_top,
        );
        self.draw_hover_tooltip(
            scene,
            transform,
            text_renderer,
            theme,
            gutter_w,
            caret_x,
            caret_top,
        );
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
            // Unbound: the only interactive thing is the zero-state's "Open
            // File" pill. Track the cursor for its hover (redrawing only when
            // the pointer crosses the pill so an idle screen stays still), and
            // fire the action on a click inside it.
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let next = Point::new(position.x / scale_factor, position.y / scale_factor);
                    let was = self.empty_action_rect.contains(self.last_cursor);
                    let now = self.empty_action_rect.contains(next);
                    self.last_cursor = next;
                    if was != now {
                        self.dirty = true;
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if self.empty_action_rect.contains(self.last_cursor) => {
                    self.empty_action = true;
                    *handled = true;
                }
                _ => {}
            }
            return;
        }
        let mods = modifiers.state();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_press(mods, handled),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.release() {
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let next = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.handle_cursor_moved(next, handled);
            }
            WindowEvent::MouseWheel { delta, .. } => self.handle_mouse_wheel(*delta, handled),
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.handle_key_press(key_event, mods, state, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
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

    /// Test hook: force geometry (rect + measured cell width) so pointer
    /// hit-testing (`point_to_caret`) resolves without a real draw pass.
    #[cfg(test)]
    pub fn force_geometry(&mut self, rect: Rect, cell_w: f64) {
        self.rect = rect;
        self.cell_w = cell_w;
    }

    /// Test hook: the logical pixel that `point_to_caret` maps back to
    /// `(line, col)` — the inverse of hit-testing, so mouse tests can aim at a
    /// known caret cell. Assumes `scroll == 0` (the test default).
    #[cfg(test)]
    pub fn pixel_for_test(&self, line: usize, col: usize) -> Point {
        let x = self.rect.x0 + self.gutter_width() + CODE_PAD_LEFT + col as f64 * self.cell_w;
        let y = self.rect.y0 + (line as f64 - self.scroll_y) * LINE_HEIGHT + 1.0;
        Point::new(x, y)
    }

    /// Test hook: the primary caret's ordered selection endpoints, or `None`.
    #[cfg(test)]
    pub fn selection_for_test(&self) -> Option<((usize, usize), (usize, usize))> {
        self.carets[0].selection()
    }

    /// Test hook: how many find matches the editor is currently highlighting.
    #[cfg(test)]
    pub fn find_match_count_for_test(&self) -> usize {
        self.find_matches.len()
    }

    /// Test hook: the primary selection's text (production `selected_text`).
    #[cfg(test)]
    pub fn selected_text_for_test(&self) -> Option<String> {
        self.selected_text()
    }

    /// Test hook: word-wise caret motion on the production path.
    #[cfg(test)]
    pub fn word_move_for_test(&mut self, forward: bool, extend: bool) {
        self.word_move(forward, extend);
    }

    /// Test hook: cut the primary selection through the CRDT.
    #[cfg(test)]
    pub fn cut_for_test(&mut self, doc: &mut AutoCommit) {
        self.cut_selection(doc);
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

    // ---- selection via the real winit event path -------------------------
    //
    // These drive `Widget::handle_event` with synthetic winit events so the
    // assertions exercise the production selection code (the shared
    // `ui::gesture::DragSelect` ladder + `ui::text_selection` boundary walkers),
    // not a test-only shortcut.

    use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};

    fn fake_device_id() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn geom_editor(seed: &str) -> (EditorView, AutoCommit) {
        let (mut ev, doc) = bound_editor(seed);
        // 800x600 viewport, 8px monospace cells — matches the draw-time metric
        // closely enough for deterministic hit-testing.
        ev.force_geometry(Rect::new(0.0, 0.0, 800.0, 600.0), 8.0);
        (ev, doc)
    }

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        let mut s = ModifiersState::empty();
        if ctrl {
            s |= ModifiersState::CONTROL;
        }
        if shift {
            s |= ModifiersState::SHIFT;
        }
        s.into()
    }

    fn move_to(ev: &mut EditorView, doc: &mut AutoCommit, p: Point) {
        let m = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::new(0);
        let win_ev = WindowEvent::CursorMoved {
            device_id: fake_device_id(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        ev.handle_event(
            &win_ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    fn left(ev: &mut EditorView, doc: &mut AutoCommit, pressed: bool, shift: bool) {
        let m = mods(false, shift);
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::new(0);
        let win_ev = WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
            button: MouseButton::Left,
        };
        ev.handle_event(
            &win_ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    fn key(ev: &mut EditorView, doc: &mut AutoCommit, k: Key, ctrl: bool, shift: bool) {
        let m = mods(ctrl, shift);
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::new(0);
        let mut ke: KeyEvent = unsafe { std::mem::zeroed() };
        ke.logical_key = k;
        ke.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ke.location = winit::keyboard::KeyLocation::Standard;
        ke.state = ElementState::Pressed;
        ke.repeat = false;
        ke.text = None;
        let win_ev = WindowEvent::KeyboardInput {
            device_id: fake_device_id(),
            event: ke,
            is_synthetic: false,
        };
        ev.handle_event(
            &win_ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    #[test]
    fn double_click_selects_word() {
        let (mut ev, mut doc) = geom_editor("hello world");
        // Two presses at the same point within the multi-click window → word.
        let p = ev.pixel_for_test(0, 8); // inside "world"
        move_to(&mut ev, &mut doc, p);
        left(&mut ev, &mut doc, true, false);
        left(&mut ev, &mut doc, true, false);
        assert_eq!(ev.selection_for_test(), Some(((0, 6), (0, 11))));
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("world"));
    }

    #[test]
    fn triple_click_selects_line() {
        let (mut ev, mut doc) = geom_editor("hello world\nsecond line");
        let p = ev.pixel_for_test(0, 3);
        move_to(&mut ev, &mut doc, p);
        left(&mut ev, &mut doc, true, false);
        left(&mut ev, &mut doc, true, false);
        left(&mut ev, &mut doc, true, false);
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 11))));
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("hello world"));
    }

    #[test]
    fn drag_selects_char_range() {
        let (mut ev, mut doc) = geom_editor("hello world");
        let (p0, p5) = (ev.pixel_for_test(0, 0), ev.pixel_for_test(0, 5));
        move_to(&mut ev, &mut doc, p0);
        left(&mut ev, &mut doc, true, false); // press at col 0
        // Drag to col 5 (end of "hello").
        move_to(&mut ev, &mut doc, p5);
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 5))));
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("hello"));
        left(&mut ev, &mut doc, false, false); // release keeps selection
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 5))));
    }

    #[test]
    fn drag_across_lines_selects_multiline() {
        let (mut ev, mut doc) = geom_editor("abc\ndef");
        let (p01, p12) = (ev.pixel_for_test(0, 1), ev.pixel_for_test(1, 2));
        move_to(&mut ev, &mut doc, p01);
        left(&mut ev, &mut doc, true, false);
        move_to(&mut ev, &mut doc, p12);
        assert_eq!(ev.selection_for_test(), Some(((0, 1), (1, 2))));
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("bc\nde"));
    }

    #[test]
    fn shift_arrow_extends_by_char() {
        let (mut ev, mut doc) = geom_editor("hello");
        ev.set_caret_for_test(0, 0);
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            false,
            true,
        );
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 1))));
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            false,
            true,
        );
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 2))));
    }

    #[test]
    fn ctrl_arrow_moves_by_word() {
        let (mut ev, mut doc) = geom_editor("foo bar baz");
        ev.set_caret_for_test(0, 0);
        // Word-forward lands at the start of the next word.
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            true,
            false,
        );
        assert_eq!(ev.primary_caret(), (0, 4));
        assert_eq!(
            ev.selection_for_test(),
            None,
            "ctrl (no shift) does not select"
        );
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            true,
            false,
        );
        assert_eq!(ev.primary_caret(), (0, 8));
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowLeft),
            true,
            false,
        );
        assert_eq!(ev.primary_caret(), (0, 4));
    }

    #[test]
    fn ctrl_shift_arrow_extends_by_word() {
        let (mut ev, mut doc) = geom_editor("foo bar baz");
        ev.set_caret_for_test(0, 0);
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            true,
            true,
        );
        assert_eq!(ev.selection_for_test(), Some(((0, 0), (0, 4))));
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("foo "));
    }

    #[test]
    fn word_move_uses_shared_boundaries_across_lines() {
        // Word-forward at end of a line steps onto the next line's first word —
        // proving the flat-index conversion round-trips through the shared
        // char-indexed boundary helper.
        let (mut ev, _doc) = geom_editor("foo\nbar");
        ev.set_caret_for_test(0, 3); // end of "foo"
        ev.word_move_for_test(true, false);
        assert_eq!(ev.primary_caret(), (1, 0));
    }

    #[test]
    fn plain_arrow_collapses_selection_to_edge() {
        let (mut ev, mut doc) = geom_editor("hello");
        ev.set_caret_for_test(0, 1);
        // Build a selection (0,1)..(0,4).
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            false,
            true,
        );
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            false,
            true,
        );
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowRight),
            false,
            true,
        );
        assert_eq!(ev.selection_for_test(), Some(((0, 1), (0, 4))));
        // Plain Left collapses to the left edge, not one char before it.
        key(
            &mut ev,
            &mut doc,
            Key::Named(NamedKey::ArrowLeft),
            false,
            false,
        );
        assert_eq!(ev.selection_for_test(), None);
        assert_eq!(ev.primary_caret(), (0, 1));
    }

    #[test]
    fn ctrl_x_cuts_selection_through_crdt() {
        // Cut deletes the selected range via the CRDT (clipboard write is a
        // silent no-op in the headless test env, so we assert on the buffer).
        let (mut ev, mut doc) = geom_editor("hello world");
        ev.set_caret_for_test(0, 0);
        // Select "hello " (0,0)..(0,6) with word+char extends.
        for _ in 0..6 {
            key(
                &mut ev,
                &mut doc,
                Key::Named(NamedKey::ArrowRight),
                false,
                true,
            );
        }
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("hello "));
        ev.cut_for_test(&mut doc);
        assert_eq!(ev.text_for_test(), "world");
        assert_eq!(ev.selection_for_test(), None);
    }

    #[test]
    fn clipboard_copy_paste_round_trips_when_available() {
        // Only meaningful where arboard has a backend (a Wayland/X11 session).
        // Probe with a sentinel; if it doesn't round-trip, skip cleanly — the
        // clipboard helper is silent on absence (headless CI).
        ui::text_selection::clipboard_copy("__qos_probe__");
        if ui::text_selection::clipboard_paste().as_deref() != Some("__qos_probe__") {
            eprintln!("clipboard_copy_paste_round_trips: no clipboard backend, skipping");
            return;
        }
        let (mut ev, mut doc) = geom_editor("hello world");
        ev.set_caret_for_test(0, 0);
        for _ in 0..5 {
            key(
                &mut ev,
                &mut doc,
                Key::Named(NamedKey::ArrowRight),
                false,
                true,
            );
        }
        // Ctrl+C copies "hello".
        key(&mut ev, &mut doc, Key::Character("c".into()), true, false);
        // Move to end and Ctrl+V pastes it.
        ev.set_caret_for_test(0, 11);
        key(&mut ev, &mut doc, Key::Character("v".into()), true, false);
        assert_eq!(ev.text_for_test(), "hello worldhello");
    }
}
