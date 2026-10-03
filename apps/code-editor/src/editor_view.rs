//! The code editor pane: a custom [`Widget`] that renders line numbers, a
//! gutter, syntax-highlighted source text, the active-line highlight, and the
//! caret(s) + selection — all drawn directly into the Vello scene so the
//! editor has the dense, monospace IDE look the toolkit's prose widgets don't
//! provide.
//!
//! Text is the source of truth in the CRDT: the widget is bound to a single
//! Automerge `Text` object (the active buffer). Every edit is first emitted as
//! an action request; the app dispatcher is the only code that splices that
//! Text. The widget keeps only render
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
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::text_selection::{
    clipboard_copy, clipboard_paste, next_word_boundary_chars, prev_word_boundary_chars,
    word_boundaries_chars,
};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    AppModel, Autocomplete, Button, ButtonFace, ControlSize, EmptyState, EmptyStateContent,
    ModelPath, Suggestion, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::bottom_panel::Severity;
use crate::highlight::{IncrementalHighlights, Span};
use crate::lsp::{CompletionItem, LspDiagnostic, severity_from_lsp};
use crate::model::Language;

/// A pre-mutation request emitted by the editor surface.  Keeping the complete
/// before/after snapshots here lets the app derive the compact action payload
/// at the dispatch boundary, while the view itself remains a CRDT reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTextEdit {
    pub base: String,
    pub next: String,
}

/// A language-server request the editor wants the app to make on its behalf,
/// carrying the primary caret's position implicitly (the app reads it). Set by
/// a keybind, drained by the app after routing the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspAsk {
    Completion,
    Hover,
    Definition,
}

/// The icon of a completion item's kind: what the candidate is, at a glance.
/// A macro is a function by LSP kind, so it is told apart by its detail (or
/// the `!` its name ends in) and given its own icon.
fn completion_kind_icon(item: &CompletionItem) -> Icon {
    let is_macro = item.label.ends_with('!')
        || item
            .detail
            .as_deref()
            .is_some_and(|detail| detail.starts_with("macro"));
    if is_macro {
        return Icon::Sparkles;
    }
    match item.kind {
        Some(2..=4) => Icon::Code,
        Some(5 | 10) => Icon::List,
        Some(6) => Icon::Square,
        Some(7 | 8 | 13 | 22) => Icon::Layers,
        Some(9) => Icon::Folder,
        Some(14) => Icon::Star,
        Some(21) => Icon::Pin,
        _ => Icon::FileText,
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

/// The rows one paint pass covers and where their columns start.
#[derive(Clone, Copy, Debug)]
struct VisibleRows {
    gutter_w: f64,
    /// Left edge of the code area, after the gutter and its padding.
    code_x0: f64,
    /// The first (possibly partial) visible line.
    first_line: usize,
    /// How far `first_line` is scrolled above the viewport top, in lines.
    frac: f64,
    n_visible: usize,
}

/// How many completion candidates the popup lists before it scrolls.
const COMPLETION_ROWS: usize = 8;

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
    highlight_cache: IncrementalHighlights,
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
    /// Measured monospace cell width of the `Code` role, resolved on every
    /// draw so it follows the theme and the text scale.
    cell_w: f64,
    /// Line pitch: the measured `Code` line. Every row, hit test, caret,
    /// selection, diagnostic underline and scroll step reads this one value,
    /// so large text never overprints its neighbouring row.
    line_h: f64,
    /// Top offset that centres a measured `Code` line inside `line_h`: rows
    /// and gutter numbers share it instead of per-site nudges.
    text_dy: f64,
    /// Inset of the line numbers from the gutter's edges and of the code from
    /// the gutter (`spacing.s3` of the theme last drawn with).
    pad: f64,
    /// The `Code` role's resolved size last drawn with, so a change of theme
    /// or text scale re-measures the cell and keeps the caret in view.
    code_size: f32,
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
    /// The toolkit's suggestion list, used for its dropdown only: the editor
    /// owns the text and the keys, this paints the rows beside the caret.
    completion_popup: Autocomplete,
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
    /// Compact recovery affordance shown only after horizontal scrolling. It
    /// makes the current offset explicit and returns to column zero on click;
    /// Home provides the matching keyboard path.
    horizontal_scroll_reset_rect: Rect,
    /// One optimistic UI edit awaiting the app's authoritative action dispatch.
    pending_text_edit: Option<PendingTextEdit>,
}

impl EditorView {
    pub fn new() -> Self {
        // Before the first draw measures the real face, geometry comes from
        // the default theme's `Code` role: a monospace advance of about 0.6 em
        // and the role's leading.
        let nominal = {
            let theme = Theme::dark();
            let style = theme.text(TextRole::Code);
            let size = f64::from(style.size);
            (
                (size * 0.6).ceil(),
                (size * f64::from(style.line_height)).ceil(),
                f64::from(theme.tokens.spacing.s3),
            )
        };
        EditorView {
            rect: Rect::ZERO,
            lines: vec![String::new()],
            spans: vec![Vec::new()],
            highlight_cache: IncrementalHighlights::default(),
            text_obj: None,
            language: Language::PlainText,
            carets: vec![Caret::at(0, 0)],
            scroll_y: 0.0,
            scroll_x: 0.0,
            cell_w: nominal.0,
            line_h: nominal.1,
            text_dy: 0.0,
            pad: nominal.2,
            code_size: 0.0,
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
            completion_popup: Autocomplete::new().with_text_role(TextRole::Code),
            hover_text: None,
            lsp_ask: None,
            find_matches: Vec::new(),
            find_current: None,
            empty_action_rect: Rect::ZERO,
            empty_action: false,
            horizontal_scroll_reset_rect: Rect::ZERO,
            pending_text_edit: None,
        }
    }

    /// Drain the one coalesced edit requested by UI primitives this event.
    pub fn take_text_edit(&mut self) -> Option<PendingTextEdit> {
        self.pending_text_edit.take()
    }

    /// Reconcile an optimistic view after an action refusal.
    pub fn reconcile_from_doc(&mut self, doc: &AutoCommit) {
        self.pending_text_edit = None;
        self.sync_from_doc(doc);
        self.clamp_carets();
        self.scroll_to_caret();
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
        self.highlight_cache = IncrementalHighlights::default();
        self.carets = vec![Caret::at(0, 0)];
        self.cached_text.clear();
        self.dirty = true;
    }

    /// Read-only native diagnostic offsets: horizontal pixels, vertical lines.
    pub(crate) fn native_scroll_offsets(&self) -> (f64, f64) {
        (self.scroll_x, self.scroll_y)
    }

    pub fn is_bound(&self) -> bool {
        self.text_obj.is_some()
    }

    pub fn language(&self) -> Language {
        self.language
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
        let lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n')
                .map(std::string::ToString::to_string)
                .collect()
        };
        // split('\n') on "a\n" yields ["a", ""]; that trailing empty line is
        // the editable line after the final newline, which is what we want.
        self.highlight_cache
            .update(self.language, &self.lines, &lines, &mut self.spans);
        self.lines = lines;
        self.clamp_carets();
        self.dirty = true;
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

    /// Cut: copy the primary selection, then request its action-owned delete.
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
    /// the caret with the item's insert text through the action path.
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

    fn line_height(&self) -> f64 {
        self.line_h
    }

    fn gutter_pad(&self) -> f64 {
        self.pad
    }

    fn code_pad_left(&self) -> f64 {
        self.pad
    }

    /// The width of the gutter (line-number column) for the current line count.
    fn gutter_width(&self) -> f64 {
        let digits = self.lines.len().max(1).to_string().len().max(2);
        self.gutter_pad() * 2.0 + digits as f64 * self.cell_w
    }

    /// Visible-line capacity of the viewport.
    fn visible_lines(&self) -> usize {
        (self.rect.height() / self.line_h).ceil() as usize + 1
    }

    /// Scroll so the primary caret is visible.
    fn scroll_to_caret(&mut self) {
        let (line, col) = self.primary_caret();
        let top = self.scroll_y;
        let rows = (self.rect.height() / self.line_h).floor().max(1.0);
        let bottom = top + rows - 1.0;
        if (line as f64) < top {
            self.scroll_y = line as f64;
        } else if (line as f64) > bottom {
            self.scroll_y = line as f64 - rows + 1.0;
        }
        // Horizontal: keep the caret column in view.
        let code_w = (self.rect.width() - self.gutter_width() - self.code_pad_left()).max(0.0);
        let caret_px = col as f64 * self.cell_w;
        if caret_px < self.scroll_x {
            self.scroll_x = caret_px;
        } else if caret_px > self.scroll_x + code_w - self.cell_w {
            self.scroll_x = caret_px - code_w + self.cell_w;
        }
        self.scroll_x = self.scroll_x.max(0.0);
        self.dirty = true;
    }

    // ---- editing primitives (all request an action before CRDT mutation) ----

    /// Replace a character range in a local, ephemeral string.  This is only
    /// used to prepare an action request; it never touches Automerge.
    fn splice_local(text: &mut String, start: usize, delete: usize, insert: &str) {
        let chars = text.chars().count();
        let start = start.min(chars);
        let end = start.saturating_add(delete).min(chars);
        let byte_at = |n: usize| {
            text.char_indices()
                .nth(n)
                .map_or_else(|| text.len(), |(byte, _)| byte)
        };
        let start_byte = byte_at(start);
        let end_byte = byte_at(end);
        text.replace_range(start_byte..end_byte, insert);
    }

    /// Optimistically rebuild the render cache and coalesce the requested
    /// change. The caller is responsible for dispatching it; a refusal calls
    /// `reconcile_from_doc` and puts this cache back on the real document.
    fn request_text_edit(&mut self, next: &str) {
        if next == self.cached_text {
            return;
        }
        let base = self
            .pending_text_edit
            .as_ref()
            .map_or_else(|| self.cached_text.clone(), |edit| edit.base.clone());
        self.pending_text_edit = Some(PendingTextEdit {
            base,
            next: next.to_string(),
        });
        self.rebuild(next);
    }

    /// Insert a string at the primary caret (replacing any selection),
    /// requesting one compact action-owned replacement. Updates the caret. For simplicity multi-caret
    /// inserts apply the same text at each caret, processed bottom-up so
    /// earlier indices stay valid.
    fn insert(&mut self, _doc: &mut AutoCommit, s: &str) {
        if self.text_obj.is_none() {
            return;
        }
        let mut next = self.cached_text.clone();
        // Process carets bottom-to-top so a splice doesn't shift the indices
        // of carets we haven't handled yet.
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, _) in order {
            self.apply_insert_at(&mut next, i, s);
        }
        self.request_text_edit(&next);
        self.scroll_to_caret();
    }

    fn apply_insert_at(&mut self, next: &mut String, i: usize, s: &str) {
        let Some(caret) = self.carets.get(i).copied() else {
            return;
        };
        // Delete selection first if present.
        let start = if let Some((sel_s, sel_e)) = caret.selection() {
            let s_idx = self.char_index(sel_s.0, sel_s.1);
            let e_idx = self.char_index(sel_e.0, sel_e.1);
            Self::splice_local(next, s_idx, e_idx - s_idx, "");
            if let Some(c) = self.carets.get_mut(i) {
                c.anchor = None;
            }
            sel_s
        } else {
            (caret.line, caret.col)
        };
        let idx = self.char_index(start.0, start.1);
        Self::splice_local(next, idx, 0, s);
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
    fn backspace(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
        let mut next = self.cached_text.clone();
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, caret) in order {
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                Self::splice_local(&mut next, s_idx, e_idx - s_idx, "");
                if let Some(c) = self.carets.get_mut(i) {
                    *c = Caret::at(s.0, s.1);
                }
            } else {
                let idx = self.char_index(caret.line, caret.col);
                if idx == 0 {
                    continue;
                }
                Self::splice_local(&mut next, idx - 1, 1, "");
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
        self.request_text_edit(&next);
        self.scroll_to_caret();
    }

    /// Forward-delete at every caret.
    fn delete_forward(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
        let mut next = self.cached_text.clone();
        let mut order: Vec<(usize, Caret)> = self.carets.iter().copied().enumerate().collect();
        order.sort_by_key(|(_, c)| std::cmp::Reverse((c.line, c.col)));
        for (i, caret) in order {
            if let Some((s, e)) = caret.selection() {
                let s_idx = self.char_index(s.0, s.1);
                let e_idx = self.char_index(e.0, e.1);
                Self::splice_local(&mut next, s_idx, e_idx - s_idx, "");
                if let Some(c) = self.carets.get_mut(i) {
                    *c = Caret::at(s.0, s.1);
                }
            } else {
                let idx = self.char_index(caret.line, caret.col);
                let total: usize = self.cached_text.chars().count();
                if idx >= total {
                    continue;
                }
                Self::splice_local(&mut next, idx, 1, "");
            }
        }
        self.request_text_edit(&next);
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

    // ---- line operations (all request action-owned CRDT edits) ----

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
    pub fn duplicate_lines(&mut self, _doc: &mut AutoCommit, focus_copy: bool) {
        if self.text_obj.is_none() {
            return;
        }
        let (a, b) = self.primary_line_range();
        let block: String = self
            .lines
            .get(a..=b)
            .map(|s| s.join("\n"))
            .unwrap_or_default();
        let end_of_b = self.char_index(b, self.lines.get(b).map_or(0, |l| l.chars().count()));
        let mut next = self.cached_text.clone();
        Self::splice_local(&mut next, end_of_b, 0, &format!("\n{block}"));
        self.request_text_edit(&next);
        if focus_copy {
            self.shift_primary_lines((b - a + 1) as isize);
        } else {
            self.scroll_to_caret();
        }
    }

    /// Move the caret's line(s) up or down by one, swapping with the neighbour.
    /// One action-owned replacement, so it undoes as one step. Bound to Alt+Up/Alt+Down.
    pub fn move_lines(&mut self, _doc: &mut AutoCommit, up: bool) {
        if self.text_obj.is_none() {
            return;
        }
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
            let mut next = self.cached_text.clone();
            Self::splice_local(&mut next, from, to - from, &format!("{block}\n{prev}"));
            self.request_text_edit(&next);
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
            let mut updated = self.cached_text.clone();
            Self::splice_local(&mut updated, from, to - from, &format!("{next}\n{block}"));
            self.request_text_edit(&updated);
            self.shift_primary_lines(1);
        }
    }

    /// Delete the caret's line(s) entirely, landing the caret at the start of
    /// what follows. Bound to Ctrl+Shift+K.
    pub fn delete_lines(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
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
        let mut next = self.cached_text.clone();
        Self::splice_local(&mut next, from, to - from, "");
        self.request_text_edit(&next);
        let target = a.min(self.lines.len().saturating_sub(1));
        self.carets = vec![Caret::at(target, 0)];
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Toggle a line comment on the caret's line(s). If every non-blank line in
    /// the range is already commented, uncomment; otherwise comment them all
    /// (at the range's shallowest indent, so the comment markers line up).
    /// No-op for languages without a line comment. Bound to Ctrl+/.
    pub fn toggle_line_comment(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
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
        let mut next = self.cached_text.clone();
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
                    Self::splice_local(&mut next, at, remove, "");
                }
            } else {
                let at = self.char_index(l, indent);
                Self::splice_local(&mut next, at, 0, &format!("{token} "));
            }
        }
        self.request_text_edit(&next);
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Indent every line the selection spans by one indent unit. Falls back to
    /// a plain tab-at-caret when the selection is empty or single-line (handled
    /// by the caller). Bound to Tab with a multi-line selection.
    pub fn indent_selection(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
        let (a, b) = self.primary_line_range();
        let pad = " ".repeat(self.indent_width);
        let mut next = self.cached_text.clone();
        for l in (a..=b).rev() {
            if self.lines.get(l).is_none_or(String::is_empty) {
                continue;
            }
            let at = self.line_start_index(l);
            Self::splice_local(&mut next, at, 0, &pad);
        }
        self.request_text_edit(&next);
        self.clamp_carets();
    }

    /// Outdent every line the selection spans by up to one indent unit. Bound
    /// to Shift+Tab.
    pub fn outdent_selection(&mut self, _doc: &mut AutoCommit) {
        if self.text_obj.is_none() {
            return;
        }
        let (a, b) = self.primary_line_range();
        let mut next = self.cached_text.clone();
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
                Self::splice_local(&mut next, at, remove, "");
            }
        }
        self.request_text_edit(&next);
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
        let rel_y = p.y - self.rect.y0 + self.scroll_y * self.line_height();
        let line = ((rel_y / self.line_height()).floor() as isize)
            .clamp(0, self.lines.len() as isize - 1) as usize;
        let rel_x = p.x - self.rect.x0 - gutter - self.code_pad_left() + self.scroll_x;
        let max_col = self.lines.get(line).map_or(0, |l| l.chars().count());
        let col = ((rel_x / self.cell_w).round().max(0.0) as usize).min(max_col);
        (line, col)
    }

    // ---- draw helpers (split out of `Widget::draw`, which orchestrates them) ----

    /// Measure the real monospace cell width once a renderer is available,
    /// caching it on `self` (the fallback stays in place until the first
    /// successful measurement).
    fn measure_cell_width(&mut self, text_renderer: &mut TextRenderer, theme: &Theme) {
        let style = Self::code_style(theme);
        let (mw, mh) = text_renderer.measure_style("M", None, &style);
        if mw > 0.0 {
            self.cell_w = f64::from(mw);
        }
        if mh > 0.0 {
            self.line_h = Self::line_pitch(f64::from(mh));
            self.text_dy = (self.line_h - f64::from(mh)) / 2.0;
        }
        self.pad = f64::from(theme.tokens.spacing.s3);
    }

    /// The face the buffer and its line numbers are set in: the `Code` role.
    fn code_style(theme: &Theme) -> TextStyle {
        theme.text(TextRole::Code)
    }

    /// Row pitch for a measured `Code` line: the line itself, on a whole
    /// pixel so rows tile without seams.
    fn line_pitch(line_h: f64) -> f64 {
        line_h.ceil()
    }

    /// The geometry a draw pass threads through.
    fn visible_rows(&self) -> VisibleRows {
        let gutter_w = self.gutter_width();
        let first_line = self.scroll_y.floor() as usize;
        VisibleRows {
            gutter_w,
            code_x0: self.rect.x0 + gutter_w + self.code_pad_left(),
            first_line,
            frac: self.scroll_y - first_line as f64,
            n_visible: self.visible_lines(),
        }
    }

    /// Draw the per-line background bands for every visible row: the
    /// active-line highlight, find matches, and selection bands. The buffer
    /// and its gutter stand on the content ground the framework painted, so
    /// nothing is filled under them.
    /// Runs as its own pass over the visible rows (before [`Self::draw_line_text`]
    /// paints line numbers and code on top) — safe because rows never overlap,
    /// so the pass order only has to hold within a single row, not across rows.
    fn draw_line_backgrounds(
        &self,
        ctx: &mut PaintCtx<'_>,
        rows: &VisibleRows,
        primary_line: usize,
    ) {
        let VisibleRows {
            gutter_w,
            code_x0,
            first_line,
            frac,
            n_visible,
        } = *rows;
        let (scene, transform, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let c = &theme.tokens.colors;
        for vi in 0..n_visible {
            let line_idx = first_line + vi;
            let Some(line) = self.lines.get(line_idx) else {
                break;
            };
            let y = self.rect.y0 + (vi as f64 - frac) * self.line_h;
            if y > self.rect.y1 {
                break;
            }

            // Active-line highlight across the code area.
            if line_idx == primary_line && self.carets.len() == 1 {
                let hl = Rect::new(self.rect.x0 + gutter_w, y, self.rect.x1, y + self.line_h);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.lift_on(c.surfaces.content, 0.5),
                    None,
                    &hl,
                );
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
                    let sel = Rect::new(sx0.max(self.rect.x0 + gutter_w), y, sx1, y + self.line_h);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        c.intent.selection_tint,
                        None,
                        &sel,
                    );
                }
            }

            // Find matches on this line, over the selection so a match stays
            // visible while it is selected. Each is a wash over its glyphs
            // only; the current one is outlined.
            for (mi, m) in self.find_matches.iter().enumerate() {
                if m.line != line_idx {
                    continue;
                }
                let mx0 = code_x0 + m.col as f64 * self.cell_w - self.scroll_x;
                let mx1 = code_x0 + (m.col + m.len) as f64 * self.cell_w - self.scroll_x;
                let band = Rect::new(mx0.max(self.rect.x0 + gutter_w), y, mx1, y + self.line_h);
                ui::widget::paint_find_match(
                    scene,
                    transform,
                    band,
                    self.find_current == Some(mi),
                    theme,
                );
            }
        }
    }

    /// Draw the gutter line number and syntax-highlighted code text for every
    /// visible row (the pass on top of [`Self::draw_line_backgrounds`]).
    fn draw_line_text(&self, ctx: &mut PaintCtx<'_>, rows: &VisibleRows, primary_line: usize) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let c = &theme.tokens.colors;
        let code_style = Self::code_style(theme);
        let keyword_style = code_style.clone().strong();
        // Horizontal editor scrolling moves source text, never the gutter.
        // Vello text does not implicitly inherit a clipping rect, so without
        // this explicit layer negative-x text painted back through line
        // numbers on a narrow window. Keep the line-number pass outside it
        // and clip every source span to the content pane.
        let code_clip = Rect::new(
            self.rect.x0 + rows.gutter_w,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        for vi in 0..rows.n_visible {
            let line_idx = rows.first_line + vi;
            let Some(line) = self.lines.get(line_idx) else {
                break;
            };
            let y = self.rect.y0 + (vi as f64 - rows.frac) * self.line_h;
            if y > self.rect.y1 {
                break;
            }

            // Line number, right-aligned in the gutter.
            let num = (line_idx + 1).to_string();
            let num_w = num.len() as f64 * self.cell_w;
            let num_x = self.rect.x0 + rows.gutter_w - self.gutter_pad() - num_w;
            let num_color = if line_idx == primary_line {
                c.text_primary
            } else {
                c.text_dim
            };
            text_renderer.draw_style(
                scene,
                &num,
                num_color,
                transform * Affine::translate((num_x, y + self.text_dy)),
                None,
                &code_style,
            );

            // The highlighted code text, span by span.
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &code_clip,
            );
            let empty: Vec<Span> = Vec::new();
            let spans = self.spans.get(line_idx).unwrap_or(&empty);
            let base_x = rows.code_x0 - self.scroll_x;
            if spans.is_empty() && !line.is_empty() {
                // Fallback for a line the highlighter produced no spans for —
                // draw it plain so no characters are ever dropped.
                text_renderer.draw_style(
                    scene,
                    line,
                    c.text_primary,
                    transform * Affine::translate((base_x, y + self.text_dy)),
                    None,
                    &code_style,
                );
            } else {
                for span in spans {
                    // Byte offsets → char offset for x positioning.
                    let prefix = line
                        .as_bytes()
                        .get(..span.range.start.min(line.len()))
                        .unwrap_or(&[]);
                    let char_col = std::str::from_utf8(prefix).map_or(0, |s| s.chars().count());
                    let seg = line
                        .get(span.range.start..span.range.end.min(line.len()))
                        .unwrap_or("");
                    if seg.is_empty() {
                        continue;
                    }
                    let x = base_x + char_col as f64 * self.cell_w;
                    // Skip fully off-screen-left segments.
                    if x + seg.chars().count() as f64 * self.cell_w < self.rect.x0 + rows.gutter_w {
                        continue;
                    }
                    let color = theme.tokens.syntax.color(span.kind);
                    let style = if span.kind == ui::widget::code_editor::TokenKind::Keyword {
                        &keyword_style
                    } else {
                        &code_style
                    };
                    text_renderer.draw_style(
                        scene,
                        seg,
                        color,
                        transform * Affine::translate((x, y + self.text_dy)),
                        None,
                        style,
                    );
                }
            }
            scene.pop_layer();
        }
    }

    /// Draw a coloured wavy underline under each inline diagnostic's span
    /// (from the language server) that falls within the visible line range.
    fn draw_diagnostics(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rows: &VisibleRows,
    ) {
        let VisibleRows {
            gutter_w,
            code_x0,
            first_line,
            n_visible,
            ..
        } = *rows;
        let c = &theme.tokens.colors;
        for d in &self.diagnostics {
            if d.line < first_line || d.line >= first_line + n_visible {
                continue;
            }
            let y = self.rect.y0 + (d.line as f64 - self.scroll_y) * self.line_h;
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
            let sy = y + self.line_h - 3.0;
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
    fn draw_carets(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rows: &VisibleRows) {
        let VisibleRows {
            gutter_w,
            code_x0,
            first_line,
            n_visible,
            ..
        } = *rows;
        if !self.is_focused {
            return;
        }
        let c = &theme.tokens.colors;
        for caret in &self.carets {
            if caret.line < first_line || caret.line >= first_line + n_visible {
                continue;
            }
            let y = self.rect.y0 + (caret.line as f64 - self.scroll_y) * self.line_h;
            let x = code_x0 + caret.col as f64 * self.cell_w - self.scroll_x;
            if x < self.rect.x0 + gutter_w {
                continue;
            }
            let inset = f64::from(theme.tokens.spacing.s1) / 2.0;
            let caret_line = vello::kurbo::Line::new(
                Point::new(x, y + inset),
                Point::new(x, y + self.line_h - inset),
            );
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                c.intent.you,
                None,
                &caret_line,
            );
        }
    }

    /// Draw the completion popup: the toolkit's suggestion list, anchored
    /// under the caret (above it when there is no room below). Each row
    /// carries its kind icon, its label and the candidate's detail in a
    /// trailing column. No-op when the popup isn't showing.
    fn draw_completion_popup(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        caret_x: f64,
        caret_top: f64,
    ) {
        if !self.completion_active() {
            return;
        }
        let total = self.completions.len();
        let shown = total.min(COMPLETION_ROWS);
        // Scroll the window so the selected row stays visible.
        let offset = if self.completion_index >= COMPLETION_ROWS {
            self.completion_index - COMPLETION_ROWS + 1
        } else {
            0
        };
        let suggestions: Vec<Suggestion> = self
            .completions
            .iter()
            .skip(offset)
            .take(shown)
            .map(|item| {
                let suggestion = Suggestion::labelled(&item.insert_text, &item.label)
                    .with_icon(completion_kind_icon(item));
                match &item.detail {
                    Some(detail) => suggestion.with_detail(detail),
                    None => suggestion,
                }
            })
            .collect();
        // The list is as wide as its widest label and detail need, within
        // the editor.
        let spacing = &theme.tokens.spacing;
        let label_style = theme.text(TextRole::Code);
        let detail_style = theme.text(TextRole::Caption);
        let icon = f64::from(theme.tokens.metrics.icon_md);
        let wanted = self
            .completions
            .iter()
            .skip(offset)
            .take(shown)
            .map(|item| {
                let label = f64::from(
                    text_renderer
                        .measure_style(&item.label, None, &label_style)
                        .0,
                );
                let detail = item.detail.as_deref().map_or(0.0, |detail| {
                    f64::from(text_renderer.measure_style(detail, None, &detail_style).0)
                });
                label + detail
            })
            .fold(0.0, f64::max);
        let chrome = icon + 6.0 * f64::from(spacing.s4);
        let width = (wanted + chrome)
            .max(f64::from(theme.tokens.metrics.panel_width_utility))
            .min((self.rect.width() - 2.0 * f64::from(spacing.s4)).max(0.0));
        // The list opens a little under its (zero-height) field, so the field
        // sits at the caret's bottom edge, or above the list when the editor
        // ends first.
        let row_h = f64::from(theme.tokens.metrics.menu_row_height);
        let list_h = shown as f64 * row_h + f64::from(spacing.s2);
        let drop_gap = f64::from(spacing.s1) * 1.5;
        let below = caret_top + self.line_h;
        let anchor_y = if below + drop_gap + list_h <= self.rect.y1 {
            below
        } else {
            caret_top - list_h - 2.0 * drop_gap
        };
        let x = caret_x
            .min(self.rect.x1 - width - f64::from(spacing.s2))
            .max(self.rect.x0 + self.gutter_width());
        self.completion_popup
            .apply_layout_external(Rect::new(x, anchor_y, x + width, anchor_y));
        self.completion_popup.set_suggestions(suggestions);
        self.completion_popup.open();
        self.completion_popup
            .set_highlight(Some(self.completion_index - offset));
        self.completion_popup
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    /// Draw the hover tooltip: a small panel near the caret with the server's
    /// text, preferring above the caret and dropping below when there's no
    /// room. No-op when there is no hover text to show.
    fn draw_hover_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        caret_x: f64,
        caret_top: f64,
    ) {
        let Some(hover) = self.hover_text.as_deref() else {
            return;
        };
        let spacing = &theme.tokens.spacing;
        let style = theme.text(TextRole::Body);
        let first: String = hover.lines().take(4).collect::<Vec<_>>().join("\n");
        let pad = f64::from(spacing.s3);
        let max_text = (self.rect.width() - 2.0 * (pad + f64::from(spacing.s4))).max(0.0);
        let (tw, th) = text_renderer.measure_style(&first, Some(max_text as f32), &style);
        let width = f64::from(tw) + 2.0 * pad;
        let height = f64::from(th) + 2.0 * pad;
        let gap = f64::from(spacing.s1);
        let px = caret_x
            .min(self.rect.x1 - width - f64::from(spacing.s2))
            .max(self.rect.x0 + self.gutter_width());
        // Prefer above the caret; drop below if there's no room.
        let py = if caret_top - height - gap >= self.rect.y0 {
            caret_top - height - gap
        } else {
            caret_top + self.line_h + gap
        };
        let panel = Rect::new(px, py, px + width, py + height);
        ui::widget::floating_surface::paint(scene, transform, panel, theme);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        text_renderer.draw_style(
            scene,
            &first,
            ink.text_primary,
            transform * Affine::translate((px + pad, py + pad)),
            Some(max_text as f32),
            &style,
        );
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
            MouseScrollDelta::PixelDelta(p) => p.y / self.line_h,
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
                let rows = (self.rect.height() / self.line_h).floor() as isize;
                self.move_caret(0, -rows, shift);
            }
            Key::Named(NamedKey::PageDown) => {
                let rows = (self.rect.height() / self.line_h).floor() as isize;
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

    /// The "Line start" button shown only after horizontal scrolling. It
    /// makes the offset explicit and returns to column zero on click; Home
    /// provides the matching keyboard path.
    fn draw_horizontal_scroll_recovery(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        code_x0: f64,
    ) {
        self.horizontal_scroll_reset_rect = Rect::ZERO;
        if self.scroll_x <= 1.0 {
            return;
        }
        let inset = f64::from(theme.tokens.spacing.s2);
        let size = ControlSize::Small;
        let face = ButtonFace::new(Rect::ZERO, "Line start")
            .icon(Icon::ArrowLeft)
            .control_size(size);
        let width = face.intrinsic_width(text_renderer, theme);
        let height = ButtonFace::height(size, theme);
        let rect = Rect::new(
            code_x0 + inset,
            self.rect.y1 - inset - height,
            (code_x0 + inset + width).min(self.rect.x1 - inset),
            self.rect.y1 - inset,
        );
        let face = ButtonFace::new(rect, "Line start")
            .icon(Icon::ArrowLeft)
            .control_size(size)
            .hovered(rect.contains(self.last_cursor));
        Button::paint(scene, transform, text_renderer, theme, &face);
        self.horizontal_scroll_reset_rect = rect;
    }
}

impl Widget for EditorView {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
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
        presence: &PresenceCtx,
    ) {
        let code_size = theme.text(TextRole::Code).size;
        if (self.code_size - code_size).abs() > f32::EPSILON {
            self.code_size = code_size;
            self.measure_cell_width(text_renderer, theme);
            self.scroll_to_caret();
        }
        self.measure_cell_width(text_renderer, theme);
        let rows = self.visible_rows();
        let VisibleRows { code_x0, .. } = rows;

        if self.text_obj.is_none() {
            // A composed zero-state through the shared primitive: a hero icon,
            // one sentence, and an actionable "Open file" button that summons
            // the portal picker.
            self.empty_action_rect = EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.rect,
                &EmptyStateContent {
                    icon: Some(Icon::FileCode),
                    action_label: Some("Open file"),
                    ..EmptyStateContent::new("No file open")
                },
                self.last_cursor,
            )
            .unwrap_or(Rect::ZERO);
            return;
        }

        // The last visible row is usually cut by the editor's bottom edge; keep
        // what is cut off from painting into the bar below.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        let primary_line = self.primary().line;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_line_backgrounds(&mut ctx, &rows, primary_line);
        self.draw_line_text(&mut ctx, &rows, primary_line);
        self.draw_diagnostics(scene, transform, theme, &rows);
        self.draw_carets(scene, transform, theme, &rows);
        self.draw_horizontal_scroll_recovery(scene, transform, text_renderer, theme, code_x0);
        scene.pop_layer();

        // Caret pixel, reused by the completion popup + hover tooltip.
        let (caret_line, caret_col) = self.primary_caret();
        let caret_x = code_x0 + caret_col as f64 * self.cell_w - self.scroll_x;
        let caret_top = self.rect.y0 + (caret_line as f64 - self.scroll_y) * self.line_h;

        self.draw_completion_popup(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            caret_x,
            caret_top,
        );
        self.draw_hover_tooltip(scene, transform, text_renderer, theme, caret_x, caret_top);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.text_obj.is_none() {
            // Unbound: the only interactive thing is the zero-state's "Open
            // File" pill. Track the cursor for its hover (redrawing only when
            // the pointer crosses the pill so an idle screen stays still), and
            // fire the action on a click inside it.
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let next =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    *ctx.handled = true;
                }
                _ => {}
            }
            return;
        }
        let mods = ctx.modifiers.state();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.horizontal_scroll_reset_rect.contains(self.last_cursor) => {
                self.scroll_x = 0.0;
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_press(mods, ctx.handled),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.release() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let next = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.handle_cursor_moved(next, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => self.handle_mouse_wheel(*delta, ctx.handled),
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.handle_key_press(key_event, mods, ctx.state, ctx.handled);
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Document)
                .with_name("Code editor")
                .with_state(ui::a11y::AccessState {
                    focusable: true,
                    focused: self.is_focused,
                    multiline: true,
                    ..Default::default()
                }),
        )
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
        let x =
            self.rect.x0 + self.gutter_width() + self.code_pad_left() + col as f64 * self.cell_w;
        let y = self.rect.y0 + (line as f64 - self.scroll_y) * self.line_height() + 1.0;
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

    /// Test hook: cut the primary selection through the action request path.
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
    fn pointer_hit_testing_follows_the_line_pitch_and_selects_words_lines_and_ranges() {
        // Each phase builds a fresh bound editor so click counts and selections cannot leak.
        line_pitch_matches_the_code_line_at_1x_and_follows_the_text_scale();
        double_click_selects_word();
        triple_click_selects_line();
        drag_selects_char_range();
        drag_across_lines_selects_multiline();
    }

    /// The row pitch is the `Code` role's measured line at 1× and grows with
    /// the glyphs at 2×; pointer hit-testing maps a click on the third row
    /// back to line 2 at both scales, so paint and hits share one pitch.
    fn line_pitch_matches_the_code_line_at_1x_and_follows_the_text_scale() {
        let (mut ev, _doc) = bound_editor("a\nb\nc\nd");
        ev.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        ev.measure_cell_width(&mut tr, &theme);
        let (mw, mh) = tr.measure_style("M", None, &theme.text(TextRole::Code));
        assert!(
            (ev.line_h - f64::from(mh).ceil()).abs() < 1e-9,
            "a row is the measured line, on a whole pixel"
        );
        assert!(
            (ev.text_dy - (ev.line_h - f64::from(mh)) / 2.0).abs() < 1e-9,
            "glyphs centre on the measured line inside the row"
        );
        assert!((ev.cell_w - f64::from(mw)).abs() < 1e-9);
        let third_row_1x = Point::new(
            ev.rect.x0 + ev.gutter_width() + ev.code_pad_left(),
            2.5 * ev.line_h,
        );
        assert_eq!(ev.point_to_caret(third_row_1x).0, 2);
        let (cell_1x, pitch_1x) = (ev.cell_w, ev.line_h);

        let large = Theme::dark().with_text_scale(2.0);
        tr.set_text_scale(2.0);
        ev.measure_cell_width(&mut tr, &large);
        assert!(ev.line_h > pitch_1x, "rows grew with the glyphs");
        assert!(ev.cell_w > cell_1x, "the cell width scaled with the glyphs");
        let third_row_2x = Point::new(
            ev.rect.x0 + ev.gutter_width() + ev.code_pad_left(),
            2.5 * ev.line_h,
        );
        assert_eq!(ev.point_to_caret(third_row_2x).0, 2);
        // The 1× third-row point now lands on the taller second row.
        assert_eq!(ev.point_to_caret(third_row_1x).0, 1);
    }

    #[test]
    fn typing_requests_action_owned_edits_and_keeps_the_caret_in_chars() {
        // Each phase builds a fresh bound editor.
        insert_text_requests_an_action_owned_edit();
        newline_preserves_leading_indent();
        backspace_at_line_start_joins_previous_line();
        backspace_removes_char_before_caret();
        multi_char_unicode_inserts_keep_caret_in_chars();
    }

    fn insert_text_requests_an_action_owned_edit() {
        let (mut ev, mut doc) = bound_editor("ab");
        ev.set_caret_for_test(0, 1);
        ev.insert_for_test(&mut doc, "X");
        assert_eq!(ev.text_for_test(), "aXb");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    fn newline_preserves_leading_indent() {
        let (mut ev, mut doc) = bound_editor("    foo");
        ev.set_caret_for_test(0, 7); // end of "    foo"
        ev.newline_for_test(&mut doc);
        // New line carries the 4-space indent.
        assert_eq!(ev.text_for_test(), "    foo\n    ");
        assert_eq!(ev.primary_caret(), (1, 4));
    }

    fn backspace_at_line_start_joins_previous_line() {
        let (mut ev, mut doc) = bound_editor("ab\ncd");
        ev.set_caret_for_test(1, 0); // start of "cd"
        ev.backspace_for_test(&mut doc);
        assert_eq!(ev.text_for_test(), "abcd");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    fn backspace_removes_char_before_caret() {
        let (mut ev, mut doc) = bound_editor("abc");
        ev.set_caret_for_test(0, 2);
        ev.backspace_for_test(&mut doc);
        assert_eq!(ev.text_for_test(), "ac");
        assert_eq!(ev.primary_caret(), (0, 1));
    }

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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

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
    fn arrow_keys_move_and_extend_by_char_and_word() {
        // Each phase builds a fresh bound editor.
        shift_arrow_extends_by_char();
        ctrl_arrow_moves_by_word();
        ctrl_shift_arrow_extends_by_word();
        word_move_uses_shared_boundaries_across_lines();
        plain_arrow_collapses_selection_to_edge();
    }

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

    fn word_move_uses_shared_boundaries_across_lines() {
        // Word-forward at end of a line steps onto the next line's first word —
        // proving the flat-index conversion round-trips through the shared
        // char-indexed boundary helper.
        let (mut ev, _doc) = geom_editor("foo\nbar");
        ev.set_caret_for_test(0, 3); // end of "foo"
        ev.word_move_for_test(true, false);
        assert_eq!(ev.primary_caret(), (1, 0));
    }

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
    fn clipboard_shortcuts_cut_copy_and_paste_the_selection() {
        // Each phase builds a fresh bound editor. The copy/paste phase keeps its
        // original no-backend early return, which only ends that phase.
        ctrl_x_cuts_selection_through_crdt();
        clipboard_copy_paste_round_trips_when_available();
    }

    fn ctrl_x_cuts_selection_through_crdt() {
        // Cut requests deletion of the selected range (clipboard write is a
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

    fn clipboard_copy_paste_round_trips_when_available() {
        // Only meaningful where arboard has a backend (a Wayland/X11 session).
        // Probe with a sentinel; if it doesn't round-trip, skip cleanly — the
        // clipboard helper is silent on absence (headless CI).
        ui::text_selection::clipboard_copy("__tilo_probe__");
        if ui::text_selection::clipboard_paste().as_deref() != Some("__tilo_probe__") {
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

#[cfg(test)]
mod incremental_regression {
    use super::*;

    #[test]
    fn rebuild_reuses_unchanged_highlight_suffix() {
        let source = (0..2000)
            .map(|i| format!("let value_{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = EditorView::new();
        editor.language = Language::Rust;
        editor.rebuild(&source);
        crate::highlight::take_parsed_line_count();
        editor.rebuild(&source.replacen("value_0", "changed_0", 1));
        let parsed = crate::highlight::take_parsed_line_count();
        assert_eq!(
            parsed, 1,
            "a local edit with identical parser state must not reparse the other1999 lines"
        );
    }
}
