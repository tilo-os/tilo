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

use crate::app::replace_chars;
use automerge::{AutoCommit, ObjId, ReadDoc};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::gesture::{DragSelect, Granularity};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_edit::{Edit, EditCore, Motion};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::text_selection::{
    TextPosition, clipboard_copy, clipboard_paste, grapheme_boundary_at_or_before,
    next_word_boundary_chars, prev_word_boundary_chars, word_boundaries_chars,
};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::TaffyTreeExt;
use ui::widget::{
    AppModel, Button, ButtonFace, ControlSize, EmptyState, ModelPath, Suggestion, SuggestionList,
    Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::bottom_panel::Severity;
use crate::highlight::{BackgroundHighlights, IncrementalHighlights, Span};
use crate::lsp::{CompletionItem, LspDiagnostic, severity_from_lsp};
use crate::model::Language;
use crate::row_viewport::RowViewportIndex;
use std::sync::Arc;

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

    /// This caret as a shared-core selection over the flat buffer `text`.
    fn core(&self, text: &str) -> EditCore {
        let cursor = byte_of(text, self.line, self.col);
        let anchor = self.anchor.map_or(cursor, |(l, c)| byte_of(text, l, c));
        let mut core = EditCore::new();
        core.set_selection(anchor, cursor);
        core
    }

    /// The caret a shared-core selection over `text` describes.
    fn from_core(text: &str, core: &EditCore) -> Self {
        let (line, col) = line_col_of(text, core.caret());
        Caret {
            line,
            col,
            anchor: core
                .has_selection()
                .then(|| line_col_of(text, core.anchor())),
        }
    }
}

/// The byte offset of `(line, col)` — `col` in characters — in `text`,
/// clamped to the end of the line (and of the buffer).
fn byte_of(text: &str, line: usize, col: usize) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text.get(start..).and_then(|rest| rest.find('\n')) {
            Some(nl) => start += nl + 1,
            None => return text.len(),
        }
    }
    let row = text
        .get(start..)
        .and_then(|rest| rest.split('\n').next())
        .unwrap_or_default();
    start + row.char_indices().nth(col).map_or(row.len(), |(b, _)| b)
}

/// The `(line, col)` — `col` in characters — of byte `byte` in `text`.
fn line_col_of(text: &str, byte: usize) -> (usize, usize) {
    let head = text.get(..byte.min(text.len())).unwrap_or(text);
    let line_start = head.rfind('\n').map_or(0, |nl| nl + 1);
    (
        head.matches('\n').count(),
        head.get(line_start..).map_or(0, |row| row.chars().count()),
    )
}

/// `col` (characters) pulled back onto the start of the grapheme cluster it
/// falls in, so a caret never sits between a base and its combining mark.
fn snap_col(row: &str, col: usize) -> usize {
    let byte = byte_of(row, 0, col);
    let snapped = grapheme_boundary_at_or_before(row, byte);
    row.get(..snapped).map_or(0, |head| head.chars().count())
}

/// Recently shown render projections, never the authoritative buffer storage.
/// Three active/recent panes at most, with at most 48 MiB of cached source in
/// the two inactive panes; each pane still follows the CRDT on admission.
struct BufferProjection {
    object: ObjId,
    language: Language,
    heads: Option<Vec<automerge::ChangeHash>>,
    text: String,
    lines: Arc<Vec<String>>,
    row_windows: HashMap<usize, RowViewportIndex>,
    spans: Vec<Vec<Span>>,
    highlights: IncrementalHighlights,
    background: Option<BackgroundHighlights>,
    carets: Vec<Caret>,
    scroll: (f64, f64),
}

const EAGER_HIGHLIGHT_BYTES: usize = 256 * 1024;
const INACTIVE_SOURCE_BYTES: usize = 48 * 1024 * 1024;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (focus, dirty, popup state, zero-state click) rather than a set of options that combine — a bitflag/enum would not read better"
)]
pub struct EditorView {
    rect: Rect,
    /// The buffer's lines (cache of the CRDT Text split on '\n').
    lines: Arc<Vec<String>>,
    row_windows: HashMap<usize, RowViewportIndex>,
    projections: std::collections::VecDeque<BufferProjection>,
    background_highlights: Option<BackgroundHighlights>,
    highlight_wake: ui::WakeHandle,
    /// Highlight spans per line, recomputed when the buffer changes.
    spans: Vec<Vec<Span>>,
    highlight_cache: IncrementalHighlights,
    /// The Automerge Text object this view edits. `None` until bound.
    text_obj: Option<ObjId>,
    /// Stable committed content witness; pending edits always bypass it.
    text_heads: Option<Vec<automerge::ChangeHash>>,
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
    completion_list: SuggestionList,
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
    /// The no-buffer zero-state and its one "Open file" action; the app
    /// drains the action and opens the file picker.
    empty: EmptyState,
    /// Compact recovery affordance shown only after horizontal scrolling. It
    /// makes the current offset explicit and returns to column zero on click;
    /// Home provides the matching keyboard path.
    horizontal_scroll_reset_rect: Rect,
    /// One optimistic UI edit awaiting the app's authoritative action dispatch.
    pending_text_edit: Option<PendingTextEdit>,
    /// The input method's composition in progress. It is shown at the
    /// primary caret (see [`Self::composition_on_line`]) and becomes text
    /// only when committed, at every caret.
    composition: EditCore,
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
            lines: Arc::new(vec![String::new()]),
            row_windows: HashMap::new(),
            projections: std::collections::VecDeque::new(),
            background_highlights: None,
            highlight_wake: ui::WakeHandle::detached(),
            spans: vec![Vec::new()],
            highlight_cache: IncrementalHighlights::default(),
            text_obj: None,
            text_heads: None,
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
            completion_list: SuggestionList::new()
                .with_text_role(TextRole::Code)
                .with_max_visible(COMPLETION_ROWS),
            hover_text: None,
            lsp_ask: None,
            find_matches: Vec::new(),
            find_current: None,
            empty: EmptyState::new("No file open")
                .with_icon(Icon::FileCode)
                .with_action("Open file"),
            horizontal_scroll_reset_rect: Rect::ZERO,
            pending_text_edit: None,
            composition: EditCore::new(),
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
        self.empty.take_action()
    }

    /// Portable native view over the currently bound shared text resource.
    pub(crate) fn continuation_position(
        &self,
        doc: &AutoCommit,
        resource_id: &str,
    ) -> Option<ui::text_position::TextPosition> {
        let object = self.text_obj.as_ref()?;
        if doc.text(object).ok()? != self.cached_text || self.cell_w <= 0.0 {
            return None;
        }
        let top_line = self.scroll_y.max(0.0).floor() as usize;
        let view = ui::text_position::ResolvedTextPosition {
            carets: self
                .carets
                .iter()
                .map(|caret| {
                    (
                        self.char_index(caret.line, caret.col),
                        caret
                            .anchor
                            .map(|(line, column)| self.char_index(line, column)),
                    )
                })
                .collect(),
            top_character: self.char_index(top_line.min(self.lines.len().saturating_sub(1)), 0),
            vertical_fraction: self.scroll_y.max(0.0).fract(),
            horizontal_columns: self.scroll_x.max(0.0) / self.cell_w,
        };
        ui::text_position::TextPosition::capture(doc, object, resource_id, &view)
    }

    /// Restore presentation only: focus and canonical text are untouched.
    pub(crate) fn restore_continuation_position(
        &mut self,
        doc: &AutoCommit,
        position: &ui::text_position::TextPosition,
    ) -> bool {
        let Some(object) = self.text_obj.as_ref() else {
            return false;
        };
        let Some(view) = position.resolve(doc, object) else {
            return false;
        };
        self.sync_from_doc(doc);
        self.carets = view
            .carets
            .iter()
            .map(|(cursor, anchor)| {
                let (line, col) = self.pos_from_flat(*cursor);
                Caret {
                    line,
                    col,
                    anchor: anchor.map(|position| self.pos_from_flat(position)),
                }
            })
            .collect();
        self.clamp_carets();
        let (line, _) = self.pos_from_flat(view.top_character);
        self.scroll_y = line as f64 + view.vertical_fraction;
        self.scroll_x = view.horizontal_columns * self.cell_w;
        self.dirty = true;
        true
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

    /// Bind the authoritative Text while restoring this peer's recent render
    /// projection and viewport. `sync` validates its committed witness before
    /// reusing content; pending transactions always bypass that witness.
    pub fn bind(&mut self, text_obj: ObjId, language: Language) {
        self.save_projection();
        self.language = language;
        self.indent_width = language.indent_width();
        self.composition.clear_preedit();
        if let Some(index) = self
            .projections
            .iter()
            .position(|entry| entry.object == text_obj && entry.language == language)
            && let Some(entry) = self.projections.remove(index)
        {
            self.text_heads = entry.heads;
            self.cached_text = entry.text;
            self.lines = entry.lines;
            self.row_windows = entry.row_windows;
            self.spans = entry.spans;
            self.highlight_cache = entry.highlights;
            self.background_highlights = entry.background;
            self.carets = entry.carets;
            (self.scroll_x, self.scroll_y) = entry.scroll;
        } else {
            self.clear_projection();
        }
        self.text_obj = Some(text_obj);
        self.dirty = true;
    }

    fn save_projection(&mut self) {
        let Some(object) = self.text_obj.take() else {
            return;
        };
        self.projections.retain(|entry| entry.object != object);
        self.projections.push_back(BufferProjection {
            object,
            language: self.language,
            heads: self.text_heads.take(),
            text: std::mem::take(&mut self.cached_text),
            lines: std::mem::replace(&mut self.lines, Arc::new(vec![String::new()])),
            row_windows: std::mem::take(&mut self.row_windows),
            spans: std::mem::take(&mut self.spans),
            highlights: std::mem::take(&mut self.highlight_cache),
            background: self.background_highlights.take(),
            carets: self.carets.clone(),
            scroll: (self.scroll_x, self.scroll_y),
        });
        while self.projections.len() > 2
            || self
                .projections
                .iter()
                .map(|entry| entry.text.len())
                .sum::<usize>()
                > INACTIVE_SOURCE_BYTES
        {
            self.projections.pop_front();
        }
    }

    fn clear_projection(&mut self) {
        self.text_heads = None;
        self.lines = Arc::new(vec![String::new()]);
        self.row_windows.clear();
        self.spans.clear();
        self.highlight_cache = IncrementalHighlights::default();
        self.background_highlights = None;
        self.cached_text.clear();
        self.carets = vec![Caret::at(0, 0)];
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
    }

    pub(crate) fn set_highlight_waker(&mut self, wake: ui::WakeHandle) {
        self.highlight_wake = wake;
    }

    fn collect_highlights(&mut self) -> bool {
        let before = self.spans.len();
        if let Some(job) = self.background_highlights.as_mut()
            && !job.collect(&mut self.spans)
        {
            self.background_highlights = None;
        }
        let changed = self.spans.len() != before;
        self.dirty |= changed;
        changed
    }

    /// Unbind while retaining bounded recent projections for reopening.
    pub fn unbind(&mut self) {
        self.save_projection();
        self.clear_projection();
        self.composition.clear_preedit();
        self.dirty = true;
    }

    /// Read-only native diagnostic offsets: horizontal pixels, vertical lines.
    pub(crate) fn native_scroll_offsets(&self) -> (f64, f64) {
        (self.scroll_x, self.scroll_y)
    }

    /// Opt-in native performance observation, outside normal frame work.
    pub(crate) fn native_projection_stats(&self) -> (usize, usize, usize) {
        (
            self.spans.len(),
            self.projections.len(),
            self.projections.iter().map(|entry| entry.text.len()).sum(),
        )
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
        let lines = Arc::new(lines);
        if text.len() > EAGER_HIGHLIGHT_BYTES {
            self.spans.clear();
            self.highlight_cache = IncrementalHighlights::default();
            self.background_highlights = BackgroundHighlights::start(
                self.language,
                Arc::clone(&lines),
                self.highlight_wake.clone(),
            );
        } else {
            self.background_highlights = None;
            self.highlight_cache
                .update(self.language, &self.lines, &lines, &mut self.spans);
        }
        self.row_windows = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.len() > 4096)
            .map(|(index, line)| (index, RowViewportIndex::new(line)))
            .collect();
        self.lines = lines;
        self.clamp_carets();
        self.dirty = true;
    }

    /// Clamp every caret into the buffer and onto a grapheme boundary.
    fn clamp_carets(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        let clamp = |lines: &[String], (l, c): (usize, usize)| {
            let l = l.min(last_line);
            (l, lines.get(l).map_or(0, |row| snap_col(row, c)))
        };
        for c in &mut self.carets {
            (c.line, c.col) = clamp(&self.lines, (c.line, c.col));
            c.anchor = c.anchor.map(|a| clamp(&self.lines, a));
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
    fn cut_selection(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(t) = self.selected_text() {
            clipboard_copy(&t);
            // `backspace` deletes an active selection wholesale.
            self.backspace(doc);
        }
    }

    /// Paste clipboard text at the caret, replacing any selection.
    fn paste_clipboard(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    fn accept_completion(&mut self, doc: &mut ui::widget::Edits<'_>) {
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

    /// The viewport trimmed to a whole number of lines.
    fn whole_line_rect(&self) -> Rect {
        let lines = (self.rect.height() / self.line_h).floor();
        if lines < 1.0 {
            return self.rect;
        }
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + lines * self.line_h,
        )
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

    /// Run one shared-core edit at every caret over a local copy of the
    /// buffer and request the result as one action-owned replacement. Carets
    /// are edited from the end of the buffer backwards so an edit never moves
    /// a caret still waiting for its own; the ones already edited shift by
    /// the edit's length change. A caret whose span an edit already consumed
    /// merges into it.
    fn edit_at_carets(&mut self, mut edit_one: impl FnMut(&mut EditCore, &str) -> Edit) {
        if self.text_obj.is_none() {
            return;
        }
        let mut next = self.cached_text.clone();
        let mut cores: Vec<EditCore> = self.carets.iter().map(|c| c.core(&next)).collect();
        let mut order: Vec<usize> = (0..cores.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(cores.get(i).map_or(0, |c| c.range().start)));
        // Start of the lowest edit made so far.
        let mut floor = next.len();
        let mut done: Vec<usize> = Vec::with_capacity(cores.len());
        for i in order {
            let Some(core) = cores.get_mut(i) else {
                continue;
            };
            let edit = if core.range().end > floor {
                core.set_caret(floor);
                Edit::default()
            } else {
                edit_one(core, &next)
            };
            if !edit.is_noop() && edit.apply(&mut next) {
                floor = edit.at;
                let shift = |byte: usize| {
                    if byte >= edit.range().end {
                        byte - edit.removed + edit.inserted.len()
                    } else {
                        byte.min(edit.caret_after())
                    }
                };
                for &j in &done {
                    if let Some(core) = cores.get_mut(j) {
                        let (anchor, caret) = (shift(core.anchor()), shift(core.caret()));
                        core.set_selection(anchor, caret);
                    }
                }
            }
            done.push(i);
        }
        let mut carets: Vec<Caret> = Vec::with_capacity(cores.len());
        for core in &cores {
            let caret = Caret::from_core(&next, core);
            if !carets.contains(&caret) {
                carets.push(caret);
            }
        }
        self.carets = carets;
        self.request_text_edit(&next);
        self.scroll_to_caret();
    }

    /// Insert a string at every caret, replacing its selection.
    fn insert(&mut self, _doc: &mut ui::widget::Edits<'_>, s: &str) {
        self.edit_at_carets(|core, _| core.insert(s));
    }

    /// Backspace at every caret: the selection, else the character (grapheme
    /// cluster) before the caret — joining lines at a line start.
    fn backspace(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        self.edit_at_carets(|core, text| core.delete_backward(text, false));
    }

    /// Forward-delete at every caret: the selection, else the character
    /// (grapheme cluster) after the caret.
    fn delete_forward(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        self.edit_at_carets(|core, text| core.delete_forward(text, false));
    }

    /// The input method committed `text`: the composition ends and the text
    /// is typed at every caret.
    fn commit_composition(&mut self, text: &str) {
        self.composition.clear_preedit();
        self.edit_at_carets(|core, _| core.commit(text));
    }

    /// The primary line's composition as a core whose caret sits at the
    /// primary caret within that line, so the core's display mapping places
    /// the preedit there. `None` while nothing is being composed.
    fn composition_on_line(&self) -> Option<(usize, EditCore)> {
        if !self.composition.has_preedit() {
            return None;
        }
        let primary = self.primary();
        let row = self.lines.get(primary.line)?;
        let mut core = self.composition.clone();
        let byte = self.row_windows.get(&primary.line).map_or_else(
            || byte_of(row, 0, primary.col),
            |index| index.window(row, primary.col..primary.col).0.start,
        );
        core.set_caret(byte);
        Some((primary.line, core))
    }

    /// The column the primary caret is drawn at: inside the composition at
    /// the input method's cursor while composing.
    fn primary_display_col(&self) -> usize {
        match self.composition_on_line() {
            Some((line, core)) => {
                let row = self.lines.get(line).map_or("", String::as_str);
                let start = core
                    .preedit_display_range(row)
                    .map_or(0, |range| range.start);
                let column = self.row_windows.get(&line).map_or_else(
                    || row.get(..start).map_or(0, |prefix| prefix.chars().count()),
                    |index| index.column(row, start),
                );
                let within = core.display_caret(row).saturating_sub(start);
                column.saturating_add(
                    core.preedit()
                        .get(..within)
                        .map_or(0, |prefix| prefix.chars().count()),
                )
            }
            None => self.primary().col,
        }
    }

    fn handle_ime(&mut self, ime: &Ime, handled: &mut bool) {
        if !self.is_focused && !matches!(ime, Ime::Disabled) {
            return;
        }
        match ime {
            Ime::Preedit(text, _) if text.is_empty() => self.composition.clear_preedit(),
            Ime::Preedit(text, cursor) => self.composition.set_preedit(text, *cursor),
            Ime::Commit(text) => self.commit_composition(text),
            Ime::Disabled => self.composition.clear_preedit(),
            Ime::Enabled => return,
        }
        self.dirty = true;
        *handled = true;
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

    /// Arrow motion of the primary caret: `dx` characters (grapheme
    /// clusters, crossing line ends) and `dy` lines at the same column.
    /// `extend` grows the selection; a plain arrow collapses multi-carets to
    /// the primary and a live selection to its near edge.
    fn move_caret(&mut self, dx: isize, dy: isize, extend: bool) {
        if !extend {
            self.carets.truncate(1);
        }
        let primary = self.primary();
        let text = self.cached_text.as_str();
        let mut core = primary.core(text);
        let horizontal = if dx < 0 { Motion::Left } else { Motion::Right };
        for _ in 0..dx.unsigned_abs() {
            core.motion(text, horizontal, extend);
        }
        if dy != 0 {
            let last = self.lines.len().saturating_sub(1) as isize;
            let line = (primary.line as isize + dy).clamp(0, last) as usize;
            let col = self
                .lines
                .get(line)
                .map_or(0, |row| snap_col(row, primary.col));
            let motion = if dy < 0 {
                Motion::LineUp
            } else {
                Motion::LineDown
            };
            core.motion_to(motion, TextPosition::new(byte_of(text, line, col)), extend);
        }
        let moved = Caret::from_core(text, &core);
        match self.carets.first_mut() {
            Some(c) => *c = moved,
            None => self.carets.push(moved),
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
            && let Some(col) = self
                .lines
                .get(last.line + 1)
                .map(|row| snap_col(row, last.col))
        {
            self.carets.push(Caret::at(last.line + 1, col));
            self.dirty = true;
        }
    }

    /// Insert a newline plus auto-indent matching the current line's leading
    /// whitespace — the single most-felt editor nicety.
    fn newline_with_indent(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    fn insert_indent(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    pub fn duplicate_lines(&mut self, _doc: &mut ui::widget::Edits<'_>, focus_copy: bool) {
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
        replace_chars(&mut next, end_of_b, 0, &format!("\n{block}"));
        self.request_text_edit(&next);
        if focus_copy {
            self.shift_primary_lines((b - a + 1) as isize);
        } else {
            self.scroll_to_caret();
        }
    }

    /// Move the caret's line(s) up or down by one, swapping with the neighbour.
    /// One action-owned replacement, so it undoes as one step. Bound to Alt+Up/Alt+Down.
    pub fn move_lines(&mut self, _doc: &mut ui::widget::Edits<'_>, up: bool) {
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
            replace_chars(&mut next, from, to - from, &format!("{block}\n{prev}"));
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
            replace_chars(&mut updated, from, to - from, &format!("{next}\n{block}"));
            self.request_text_edit(&updated);
            self.shift_primary_lines(1);
        }
    }

    /// Delete the caret's line(s) entirely, landing the caret at the start of
    /// what follows. Bound to Ctrl+Shift+K.
    pub fn delete_lines(&mut self, _doc: &mut ui::widget::Edits<'_>) {
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
        replace_chars(&mut next, from, to - from, "");
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
    pub fn toggle_line_comment(&mut self, _doc: &mut ui::widget::Edits<'_>) {
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
                    replace_chars(&mut next, at, remove, "");
                }
            } else {
                let at = self.char_index(l, indent);
                replace_chars(&mut next, at, 0, &format!("{token} "));
            }
        }
        self.request_text_edit(&next);
        self.clamp_carets();
        self.scroll_to_caret();
    }

    /// Indent every line the selection spans by one indent unit. Falls back to
    /// a plain tab-at-caret when the selection is empty or single-line (handled
    /// by the caller). Bound to Tab with a multi-line selection.
    pub fn indent_selection(&mut self, _doc: &mut ui::widget::Edits<'_>) {
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
            replace_chars(&mut next, at, 0, &pad);
        }
        self.request_text_edit(&next);
        self.clamp_carets();
    }

    /// Outdent every line the selection spans by up to one indent unit. Bound
    /// to Shift+Tab.
    pub fn outdent_selection(&mut self, _doc: &mut ui::widget::Edits<'_>) {
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
                replace_chars(&mut next, at, remove, "");
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
        let col = (rel_x / self.cell_w).round().max(0.0) as usize;
        (
            line,
            self.lines.get(line).map_or(0, |row| snap_col(row, col)),
        )
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
        let number_style = code_style.numeric();
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
        let composition = self.composition_on_line();
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
                &number_style,
            );

            // The highlighted code text, span by span.
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &code_clip,
            );
            let composing = composition
                .as_ref()
                .filter(|(l, _)| *l == line_idx)
                .map(|(_, core)| core);
            self.draw_code_row(
                &mut PaintCtx::new(&mut *scene, transform, &mut *text_renderer, theme),
                rows,
                (line_idx, line.as_str()),
                y,
                composing,
            );
            scene.pop_layer();
        }
    }

    /// Map the visible display columns back to committed text around IME.
    fn visible_row_window(
        &self,
        index: &RowViewportIndex,
        line: &str,
        (base_x, gutter_w): (f64, f64),
        (caret_column, preedit_columns): (Option<usize>, usize),
    ) -> std::ops::Range<usize> {
        let first_column = ((self.rect.x0 + gutter_w - base_x) / self.cell_w)
            .floor()
            .max(0.0) as usize;
        let last_column = ((self.rect.x1 - base_x) / self.cell_w).ceil().max(0.0) as usize;
        let to_source_column = |column: usize| match caret_column {
            Some(caret) if column > caret => column.saturating_sub(preedit_columns).max(caret),
            _ => column,
        };
        let (window, _) = index.window(
            line,
            to_source_column(first_column.saturating_sub(4))
                ..to_source_column(last_column.saturating_add(4)),
        );
        window
    }

    /// Draw one row's highlighted code, span by span. On the composing row
    /// the preedit is spliced in at the caret through the shared core's
    /// display mapping: text after the caret is drawn shifted past it.
    fn draw_code_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        rows: &VisibleRows,
        (line_idx, line): (usize, &str),
        y: f64,
        composing: Option<&EditCore>,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let c = &theme.tokens.colors;
        let empty: Vec<Span> = Vec::new();
        let spans = self.spans.get(line_idx).unwrap_or(&empty);
        let base_x = rows.code_x0 - self.scroll_x;
        let plain_index = RowViewportIndex::default();
        let index = self.row_windows.get(&line_idx).unwrap_or(&plain_index);
        let preedit = composing.and_then(|core| core.preedit_display_range(line));
        let preedit_columns = composing.map_or(0, |core| core.preedit().chars().count());
        let caret_column = preedit
            .as_ref()
            .map(|range| index.column(line, range.start));
        let column_at = |byte| index.column(line, byte);
        let window = self.visible_row_window(
            index,
            line,
            (base_x, rows.gutter_w),
            (caret_column, preedit_columns),
        );
        // The display column committed byte `byte` starts at; a run
        // starting at the caret begins after the composition.
        let col_at = |byte: usize| {
            column_at(byte).saturating_add(
                if preedit.as_ref().is_some_and(|range| byte >= range.start) {
                    preedit_columns
                } else {
                    0
                },
            )
        };
        // A line the highlighter produced no spans for draws plain, so no
        // characters are ever dropped; a span the composition falls
        // inside is drawn as two runs.
        let plain = [(0..line.len(), None)];
        let highlighted = spans
            .iter()
            .skip(spans.partition_point(|span| span.range.end <= window.start))
            .map(|span| (span.range.clone(), Some(span.kind)));
        let caret = preedit.as_ref().map(|range| range.start);
        let runs = if spans.is_empty() {
            Box::new(plain.into_iter()) as Box<dyn Iterator<Item = _>>
        } else {
            Box::new(highlighted)
        }
        .flat_map(|(range, kind)| {
            let end = range.end.min(line.len());
            let start = range.start.min(end);
            match caret {
                Some(at) if start < at && at < end => [(start..at, kind), (at..end, kind)],
                _ => [(start..end, kind), (end..end, kind)],
            }
        });
        for (range, kind) in runs {
            if range.end <= window.start {
                continue;
            }
            if range.start >= window.end {
                break;
            }
            let range = range.start.max(window.start)..range.end.min(window.end);
            let seg = line.get(range.clone()).unwrap_or("");
            if seg.is_empty() {
                continue;
            }
            let x = base_x + col_at(range.start) as f64 * self.cell_w;
            // Skip fully off-screen-left segments.
            if x + seg.chars().count() as f64 * self.cell_w < self.rect.x0 + rows.gutter_w {
                continue;
            }
            let color = kind.map_or(c.text_primary, |kind| theme.tokens.syntax.color(kind));
            let weight = if kind == Some(ui::widget::code_editor::TokenKind::Keyword) {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            text_renderer.draw_style(
                scene,
                seg,
                color,
                transform * Affine::translate((x, y + self.text_dy)),
                None,
                &Self::code_style(theme).with_weight(weight),
            );
        }
        if let (Some(core), Some(range)) = (composing, &preedit) {
            // The composition, underlined until the input method commits it.
            let x0 = base_x + column_at(range.start) as f64 * self.cell_w;
            let x1 = x0 + preedit_columns as f64 * self.cell_w;
            text_renderer.draw_style(
                scene,
                core.preedit(),
                c.text_primary,
                transform * Affine::translate((x0, y + self.text_dy)),
                None,
                &Self::code_style(theme),
            );
            let uy = y + self.line_h - 2.0;
            let underline = Line::new(Point::new(x0, uy), Point::new(x1, uy));
            ui::widget::paint_preedit_underline(scene, transform, underline, theme);
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
            ui::widget::code_editor::paint_diagnostic_underline(
                scene, transform, x_left, x_right, sy, color,
            );
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
        for (i, caret) in self.carets.iter().enumerate() {
            if caret.line < first_line || caret.line >= first_line + n_visible {
                continue;
            }
            let col = if i == 0 {
                self.primary_display_col()
            } else {
                caret.col
            };
            let y = self.rect.y0 + (caret.line as f64 - self.scroll_y) * self.line_h;
            let x = code_x0 + col as f64 * self.cell_w - self.scroll_x;
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
        caret_x: f64,
        caret_top: f64,
    ) {
        if !self.completion_active() {
            return;
        }
        let suggestions: Vec<Suggestion> = self
            .completions
            .iter()
            .map(|item| {
                let suggestion = Suggestion::labelled(&item.insert_text, &item.label)
                    .with_icon(completion_kind_icon(item));
                match &item.detail {
                    Some(detail) => suggestion.with_detail(detail),
                    None => suggestion,
                }
            })
            .collect();
        // The shared suggestion list opens from the caret's line: below it,
        // or above it when the editor ends first; it scrolls to keep the
        // chosen row in view and sizes itself to its labels and details.
        let x = caret_x.max(self.rect.x0 + self.gutter_width());
        self.completion_list.set_suggestions(suggestions);
        self.completion_list
            .set_anchor(Rect::new(x, caret_top, x, caret_top + self.line_h));
        self.completion_list.set_viewport(self.rect);
        self.completion_list
            .set_highlight(Some(self.completion_index));
        self.completion_list
            .paint(scene, transform, text_renderer, theme);
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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

    /// The pointer over the open completion popup: a row lifts under it, a
    /// click accepts that row, the wheel scrolls the list. A press anywhere
    /// else dismisses the popup and goes on to the editor. Returns `true`
    /// when the event was the popup's.
    fn handle_completion_pointer(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if !self.completion_active() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x / scale_factor, position.y / scale_factor);
                if self.completion_list.hover_at(point) {
                    self.dirty = true;
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(index) = self.completion_list.row_at(self.last_cursor) else {
                    self.hide_completions();
                    return false;
                };
                self.completion_index = index;
                self.accept_completion(state);
                self.dirty = true;
                *handled = true;
                true
            }
            WindowEvent::MouseWheel { delta, .. }
                if self
                    .completion_list
                    .rect()
                    .is_some_and(|r| r.contains(self.last_cursor)) =>
            {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => -p.y / self.line_h,
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a wheel step is a handful of rows"
                )]
                let rows = rows.round() as i32;
                self.completion_list.scroll_by(rows);
                self.dirty = true;
                *handled = true;
                true
            }
            _ => false,
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        use automerge::transaction::Transactable;
        self.collect_highlights();
        let (state, _) = ui::doc::document_ref(state, path);
        // Scroll and caret frames must not materialize the entire CRDT Text.
        // A pending transaction has no stable content witness: always read it,
        // including two different edits with the same pending-operation count.
        let heads = (state.pending_ops() == 0).then(|| state.committed_heads());
        if heads.is_some() && self.text_heads == heads {
            return;
        }
        if let Some(t) = self.text_obj.clone() {
            use automerge::ReadDoc;
            let text = state.text(&t).unwrap_or_default();
            if text != self.cached_text {
                self.rebuild(&text);
            }
        }
        self.text_heads = heads;
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.collect_highlights()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        #[allow(
            clippy::unwrap_used,
            reason = "node was just created by this widget's own build_layout in the same tree; a lookup failure means the taffy tree is already corrupted and there is no meaningful layout to fall back to"
        )]
        let layout = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(layout, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.collect_highlights();
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
            self.empty.apply_layout_external(self.rect);
            self.empty
                .draw(scene, transform, text_renderer, theme, presence);
            return;
        }

        // The viewport shows whole lines: a line the bottom edge would cut in
        // half is not drawn at all, so no half-glyphs sit against the panel
        // or the status bar.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.whole_line_rect(),
        );
        let primary_line = self.primary().line;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_line_backgrounds(&mut ctx, &rows, primary_line);
        self.draw_line_text(&mut ctx, &rows, primary_line);
        self.draw_diagnostics(scene, transform, theme, &rows);
        self.draw_carets(scene, transform, theme, &rows);
        self.draw_horizontal_scroll_recovery(scene, transform, text_renderer, theme, code_x0);
        scene.pop_layer();
        // Scrolled, the code continues past the top edge: it fades out
        // there, so a line the edge cuts mid-scroll reads as going on.
        if self.scroll_y > 0.0 {
            ui::surface::content_edge_fade(
                scene,
                transform,
                self.whole_line_rect(),
                ui::surface::ContentEdge::Top,
                theme.tokens.colors.surfaces.content,
            );
        }

        // Caret pixel, reused by the completion popup + hover tooltip.
        let (caret_line, _) = self.primary_caret();
        let caret_x = code_x0 + self.primary_display_col() as f64 * self.cell_w - self.scroll_x;
        let caret_top = self.rect.y0 + (caret_line as f64 - self.scroll_y) * self.line_h;

        self.draw_completion_popup(scene, transform, text_renderer, theme, caret_x, caret_top);
        self.draw_hover_tooltip(scene, transform, text_renderer, theme, caret_x, caret_top);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.text_obj.is_none() {
            // Unbound: the only interactive thing is the zero-state's
            // "Open file" action.
            self.empty
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            if matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            ) {
                self.dirty = true;
            }
            return;
        }
        let mods = ctx.modifiers.state();
        if self.handle_completion_pointer(event, ctx.scale_factor, &mut ctx.state, ctx.handled) {
            return;
        }
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
                self.handle_key_press(key_event, mods, &mut ctx.state, ctx.handled);
            }
            WindowEvent::Ime(ime) => self.handle_ime(ime, ctx.handled),
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

    fn children(&self) -> Vec<&dyn Widget> {
        if self.text_obj.is_none() {
            vec![&self.empty]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.text_obj.is_none() {
            vec![&mut self.empty]
        } else {
            Vec::new()
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn wants_tab_input(&self) -> bool {
        self.is_bound()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        // This is an action-owned source buffer, not input to a live process.
        (self.is_focused && self.is_bound()).then_some(winit::window::ImePurpose::Normal)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_purpose()?;
        if self.composition.has_preedit() || self.pending_text_edit.is_some() {
            return None;
        }
        let object = self.text_obj.as_ref()?;
        let mut stamp = ui::ime::edit_fingerprint(&self.cached_text, 0, 0, key);
        stamp = ui::ime::scoped_fingerprint(object.to_string().as_bytes(), stamp, key);
        // Include every selection, even when the primary caret stays put.
        for caret in &self.carets {
            let scope = format!("{}:{}:{:?}", caret.line, caret.col, caret.anchor);
            stamp = ui::ime::scoped_fingerprint(scope.as_bytes(), stamp, key);
        }
        Some(stamp)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.is_focused || !self.is_bound() {
            return None;
        }
        let rows = self.visible_rows();
        let (line, _) = self.primary_caret();
        let x = rows.code_x0 + self.primary_display_col() as f64 * self.cell_w - self.scroll_x;
        let y = self.rect.y0 + (line as f64 - self.scroll_y) * self.line_h;
        Some(Rect::new(x, y, x + 2.0, y + self.line_h))
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
    pub fn insert_for_test(&mut self, doc: &mut ui::widget::Edits<'_>, s: &str) {
        self.insert(doc, s);
    }

    /// Test hook: backspace at the primary caret.
    #[cfg(test)]
    pub fn backspace_for_test(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    pub fn newline_for_test(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    pub fn cut_for_test(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
        ev.sync(&doc, &root);
        (ev, doc)
    }

    #[test]
    fn committed_text_cache_tracks_pending_remote_and_rebound_content() {
        use automerge::transaction::Transactable;
        let (mut view, mut doc) = bound_editor("base");
        doc.commit();
        view.sync(&doc, &automerge::ROOT);
        let mut left = doc.clone();
        let mut right = doc.clone();
        let text = view.text_obj.clone().unwrap();
        left.splice_text(&text, 0, 4, "left").unwrap();
        right.splice_text(&text, 0, 4, "else").unwrap();
        assert_eq!(left.pending_ops(), right.pending_ops());
        view.sync(&left, &automerge::ROOT);
        assert_eq!(view.text_for_test(), "left");
        view.sync(&right, &automerge::ROOT);
        assert_eq!(view.text_for_test(), "else");
        right.commit();
        view.sync(&right, &automerge::ROOT);
        assert_eq!(view.text_for_test(), "else");
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(
            view.text_for_test(),
            "base",
            "restoring earlier heads refreshes text"
        );
        let other = doc
            .put_object(automerge::ROOT, "other", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&other, 0, 0, "another buffer").unwrap();
        doc.commit();
        view.bind(other, Language::PlainText);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(view.text_for_test(), "another buffer");
    }

    #[test]
    fn exceptionally_long_rows_shape_only_the_visible_source_and_preserve_ime() {
        use automerge::ReadDoc;
        let source = "a".repeat(300_000);
        let (mut view, mut doc) = bound_editor(&source);
        view.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut renderer = TextRenderer::new();
        let theme = Theme::dark();
        view.measure_cell_width(&mut renderer, &theme);
        for column in [0, 200_000] {
            view.scroll_x = f64::from(column) * view.cell_w;
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(
                scene.encoding().resources.glyphs.len() < 100,
                "offscreen characters must not be shaped"
            );
            assert!(!scene.encoding().resources.glyphs.is_empty());
        }
        view.carets = vec![Caret::at(0, 200_000)];
        ime(&mut view, &mut doc, Ime::Preedit("e\u{301}👩‍👩‍👧‍👦".into(), None));
        assert_eq!(
            view.primary_display_col(),
            200_009,
            "composition uses scalar columns without copying the full row"
        );
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            &PresenceCtx::empty(),
        );
        assert!(scene.encoding().resources.glyphs.len() < 100);
        assert_eq!(
            doc.text(view.text_obj.as_ref().unwrap()).unwrap(),
            source,
            "viewport and preedit never truncate authoritative text"
        );
    }

    #[test]
    fn native_text_continuation_scales_horizontal_columns_for_the_receiving_view() {
        let (mut source, doc) = bound_editor(&"x".repeat(1000));
        source.force_geometry(Rect::new(0.0, 0.0, 320.0, 240.0), 8.0);
        source.set_selection_lc((0, 450), (0, 500));
        let position = source
            .continuation_position(&doc, "11111111-2222-4333-8444-555555555555")
            .unwrap();
        let (source_x, source_y) = source.native_scroll_offsets();
        assert!(source_x > 0.0);
        let mut target = EditorView::new();
        target.bind(source.text_obj.clone().unwrap(), Language::Rust);
        target.force_geometry(Rect::new(0.0, 0.0, 320.0, 240.0), 16.0);
        target.sync(&doc, &automerge::ROOT);
        assert!(target.restore_continuation_position(&doc, &position));
        assert_eq!(target.native_scroll_offsets(), (source_x * 2.0, source_y));
        assert_eq!(target.selected_text(), source.selected_text());
        assert_eq!(
            target.continuation_position(&doc, &position.resource_id),
            Some(position),
            "portable view coordinates survive different measured cell widths"
        );
    }

    #[test]
    fn recent_buffer_projections_restore_viewports_and_follow_changed_source_heads() {
        use automerge::ReadDoc;
        use automerge::transaction::Transactable;
        use std::fmt::Write as _;
        let mut source = String::new();
        for line in 0..120 {
            writeln!(source, "let value_{line} = {line};").unwrap();
        }
        let (mut view, mut doc) = bound_editor(&source);
        let first = view.text_obj.clone().unwrap();
        let second = doc
            .put_object(automerge::ROOT, "second", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&second, 0, 0, &source).unwrap();
        doc.commit();
        view.sync(&doc, &automerge::ROOT);
        view.carets = vec![Caret::at(80, 3)];
        view.scroll_y = 60.0;
        view.scroll_x = 10.0;
        let cached_lines = Arc::clone(&view.lines);
        let cached_spans = view.spans.as_ptr();
        view.bind(second.clone(), Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        view.carets = vec![Caret::at(90, 4)];
        view.scroll_y = 70.0;
        view.bind(first.clone(), Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        assert!(
            Arc::ptr_eq(&view.lines, &cached_lines),
            "returning retains the line projection"
        );
        assert_eq!(
            view.spans.as_ptr(),
            cached_spans,
            "returning does not rerun tokenization"
        );
        assert_eq!(view.primary_caret(), (80, 3));
        assert_eq!(view.native_scroll_offsets(), (10.0, 60.0));
        view.bind(second, Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(view.primary_caret(), (90, 4));
        assert_eq!(view.native_scroll_offsets().1.to_bits(), 70.0_f64.to_bits());
        doc.splice_text(&first, 0, 0, "remote\n").unwrap();
        doc.commit();
        view.bind(first.clone(), Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(
            view.cached_text,
            doc.text(&first).unwrap(),
            "cached inactive buffers follow remote edits"
        );
        assert!(!Arc::ptr_eq(&view.lines, &cached_lines));
        for number in 0..3 {
            let object = doc
                .put_object(
                    automerge::ROOT,
                    format!("other{number}"),
                    automerge::ObjType::Text,
                )
                .unwrap();
            doc.splice_text(&object, 0, 0, "new buffer").unwrap();
            doc.commit();
            view.bind(object, Language::Rust);
            view.sync(&doc, &automerge::ROOT);
        }
        view.bind(first, Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(
            view.primary_caret(),
            (0, 0),
            "oldest projection was evicted"
        );
        assert_eq!(view.native_scroll_offsets(), (0.0, 0.0));
    }

    #[test]
    fn a_completion_row_is_chosen_with_the_pointer() {
        let (mut ev, mut doc) = bound_editor("pri");
        ev.set_caret_for_test(0, 3);
        let item = |label: &str| CompletionItem {
            label: label.into(),
            kind: Some(3),
            detail: None,
            insert_text: label.into(),
        };
        ev.show_completions(vec![item("primes_below"), item("println")]);
        ev.completion_list.set_suggestions(
            ev.completions
                .iter()
                .map(|item| Suggestion::new(&item.insert_text))
                .collect(),
        );
        ev.completion_list
            .set_anchor(Rect::new(10.0, 10.0, 210.0, 30.0));
        let row = ev.completion_list.row_rect(1).expect("second row in view");
        ev.last_cursor = row.center();
        let mut handled = false;
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        assert!(ev.handle_completion_pointer(
            &press,
            1.0,
            &mut ui::widget::Edits::lend(&mut doc),
            &mut handled
        ));
        assert!(handled);
        assert!(!ev.completion_active(), "accepting closes the popup");
        assert_eq!(ev.text_for_test(), "println");

        // A press outside the popup dismisses it and reaches the editor.
        ev.show_completions(vec![item("primes_below")]);
        ev.last_cursor = Point::new(500.0, 500.0);
        let mut handled = false;
        assert!(!ev.handle_completion_pointer(
            &press,
            1.0,
            &mut ui::widget::Edits::lend(&mut doc),
            &mut handled
        ));
        assert!(!ev.completion_active());
    }

    #[test]
    fn the_viewport_is_clipped_to_whole_lines() {
        let (mut ev, _doc) = bound_editor("a\nb\nc\n");
        ev.line_h = 20.0;
        ev.rect = Rect::new(0.0, 10.0, 400.0, 10.0 + 5.0 * 20.0 + 13.0);
        let view = ev.whole_line_rect();
        assert!((view.height() - 100.0).abs() < 1e-9, "{view:?}");
        assert!((view.y0 - ev.rect.y0).abs() < 1e-9);
        // A viewport shorter than one line is left as it is.
        ev.rect = Rect::new(0.0, 0.0, 400.0, 12.0);
        assert_eq!(ev.whole_line_rect(), ev.rect);
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
    fn completion_candidates_show_their_kind_and_macros_have_their_own_icon() {
        let item = |label: &str, kind: Option<u8>, detail: Option<&str>| CompletionItem {
            label: label.to_string(),
            kind,
            detail: detail.map(str::to_string),
            insert_text: label.to_string(),
        };
        assert_eq!(
            completion_kind_icon(&item("primes_below", Some(3), Some("fn(u64) -> Vec<u64>"))),
            Icon::Code
        );
        // A macro is a function by LSP kind; its detail (or the `!` of its
        // name) says what it is.
        assert_eq!(
            completion_kind_icon(&item("println", Some(3), Some("macro"))),
            Icon::Sparkles
        );
        assert_eq!(
            completion_kind_icon(&item("vec!", Some(3), None)),
            Icon::Sparkles
        );
        assert_eq!(
            completion_kind_icon(&item("primary", Some(6), Some("let binding"))),
            Icon::Square
        );
        assert_eq!(completion_kind_icon(&item("x", None, None)), Icon::FileText);
    }

    #[test]
    fn dictation_target_tracks_code_selection_content_and_pending_edits() {
        let (mut view, mut doc) = bound_editor("same source");
        view.set_focus(true);
        assert_eq!(view.ime_purpose(), Some(winit::window::ImePurpose::Normal));
        let key = [17; 32];
        let stamp = view.ime_edit_fingerprint(&key).expect("editable code");
        let caret = view.ime_cursor_rect();
        view.carets[0].anchor = Some((0, 4));
        assert_eq!(view.ime_cursor_rect(), caret);
        assert_ne!(view.ime_edit_fingerprint(&key), Some(stamp));
        let selected = view.ime_edit_fingerprint(&key).unwrap();
        let object = view.text_obj.clone().unwrap();
        EditorDoc::splice(&mut doc, &object, 0, 11, "same change");
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(view.ime_cursor_rect(), caret);
        assert_ne!(view.ime_edit_fingerprint(&key), Some(selected));
        let original = view.ime_edit_fingerprint(&key).unwrap();
        let carets = view.carets.clone();
        let other = EditorDoc::ensure_buffer(&mut doc, 1, "same change");
        view.bind(other, Language::Rust);
        view.sync(&doc, &automerge::ROOT);
        view.carets = carets;
        assert_eq!(view.ime_cursor_rect(), caret);
        assert_ne!(view.ime_edit_fingerprint(&key), Some(original));
        ime(&mut view, &mut doc, Ime::Preedit("draft".into(), None));
        assert!(view.ime_edit_fingerprint(&key).is_none());
        ime(&mut view, &mut doc, Ime::Disabled);
        assert!(view.ime_edit_fingerprint(&key).is_some());
        ime(&mut view, &mut doc, Ime::Commit("日本語 café".into()));
        assert!(view.ime_edit_fingerprint(&key).is_none());
        assert!(view.take_text_edit().is_some());
        assert!(view.ime_edit_fingerprint(&key).is_some());
        view.set_focus(false);
        assert!(view.ime_purpose().is_none());
        assert!(view.ime_edit_fingerprint(&key).is_none());
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
        ev.insert_for_test(&mut ui::widget::Edits::lend(&mut doc), "X");
        assert_eq!(ev.text_for_test(), "aXb");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    fn newline_preserves_leading_indent() {
        let (mut ev, mut doc) = bound_editor("    foo");
        ev.set_caret_for_test(0, 7); // end of "    foo"
        ev.newline_for_test(&mut ui::widget::Edits::lend(&mut doc));
        // New line carries the 4-space indent.
        assert_eq!(ev.text_for_test(), "    foo\n    ");
        assert_eq!(ev.primary_caret(), (1, 4));
    }

    fn backspace_at_line_start_joins_previous_line() {
        let (mut ev, mut doc) = bound_editor("ab\ncd");
        ev.set_caret_for_test(1, 0); // start of "cd"
        ev.backspace_for_test(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(ev.text_for_test(), "abcd");
        assert_eq!(ev.primary_caret(), (0, 2));
    }

    fn backspace_removes_char_before_caret() {
        let (mut ev, mut doc) = bound_editor("abc");
        ev.set_caret_for_test(0, 2);
        ev.backspace_for_test(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(ev.text_for_test(), "ac");
        assert_eq!(ev.primary_caret(), (0, 1));
    }

    fn multi_char_unicode_inserts_keep_caret_in_chars() {
        let (mut ev, mut doc) = bound_editor("");
        ev.set_caret_for_test(0, 0);
        ev.insert_for_test(&mut ui::widget::Edits::lend(&mut doc), "héllo");
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

    use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

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
        let win_ev = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        ui::testing::Dispatch::new().doc(doc).send(ev, &win_ev);
    }

    fn left(ev: &mut EditorView, doc: &mut AutoCommit, pressed: bool, shift: bool) {
        let win_ev = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
            button: MouseButton::Left,
        };
        ui::testing::Dispatch::new()
            .modifiers(mods(false, shift))
            .doc(doc)
            .send(ev, &win_ev);
    }

    fn key(ev: &mut EditorView, doc: &mut AutoCommit, k: Key, ctrl: bool, shift: bool) {
        let ke = ui::synthetic_input::key_event(k, None, ElementState::Pressed);
        let win_ev = WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ke,
            is_synthetic: false,
        };
        ui::testing::Dispatch::new()
            .modifiers(mods(ctrl, shift))
            .doc(doc)
            .send(ev, &win_ev);
    }

    fn ime(ev: &mut EditorView, doc: &mut AutoCommit, ime: Ime) {
        // Positive composition cases model delivery to the active field.
        // The app-level journey separately verifies refusal after focus loss.
        ev.set_focus(true);
        let handled = ui::testing::Dispatch::new()
            .doc(doc)
            .send(ev, &WindowEvent::Ime(ime))
            .handled;
        assert!(handled, "the editor consumes input-method events");
    }

    fn named(ev: &mut EditorView, doc: &mut AutoCommit, k: NamedKey, shift: bool) {
        key(ev, doc, Key::Named(k), false, shift);
    }

    #[test]
    fn arrows_and_deletes_step_over_whole_grapheme_clusters() {
        let line = "ae\u{301}👍🏽🇩🇪z";
        let (mut ev, mut doc) = bound_editor(line);
        ev.set_caret_for_test(0, 1);
        // Right steps over "e\u{301}" (2 chars), then the thumb (2), the flag (2).
        named(&mut ev, &mut doc, NamedKey::ArrowRight, false);
        assert_eq!(ev.primary_caret(), (0, 3));
        named(&mut ev, &mut doc, NamedKey::ArrowRight, false);
        assert_eq!(ev.primary_caret(), (0, 5));
        named(&mut ev, &mut doc, NamedKey::ArrowLeft, false);
        assert_eq!(ev.primary_caret(), (0, 3));
        // A caret placed inside a cluster is pulled back onto its start.
        ev.set_caret_for_test(0, 2);
        assert_eq!(ev.primary_caret(), (0, 1));

        // Shift+Right selects the whole thumb; typing replaces it.
        ev.set_caret_for_test(0, 3);
        named(&mut ev, &mut doc, NamedKey::ArrowRight, true);
        assert_eq!(ev.selected_text_for_test().as_deref(), Some("👍🏽"));
        ime(&mut ev, &mut doc, Ime::Commit("ok".into()));
        assert_eq!(ev.text_for_test(), "ae\u{301}ok🇩🇪z");
        assert_eq!(ev.primary_caret(), (0, 5));

        // Delete removes the flag whole; Backspace takes "k", "o", then the
        // whole combined "e\u{301}".
        named(&mut ev, &mut doc, NamedKey::Delete, false);
        assert_eq!(ev.text_for_test(), "ae\u{301}okz");
        named(&mut ev, &mut doc, NamedKey::Backspace, false);
        named(&mut ev, &mut doc, NamedKey::Backspace, false);
        named(&mut ev, &mut doc, NamedKey::Backspace, false);
        assert_eq!(ev.text_for_test(), "az");
        assert_eq!(ev.primary_caret(), (0, 1));

        // Up/Down keep the column but never land inside a cluster.
        let (mut ev, mut doc) = bound_editor("abcd\ne\u{301}e\u{301}");
        ev.set_caret_for_test(0, 3);
        named(&mut ev, &mut doc, NamedKey::ArrowDown, false);
        assert_eq!(ev.primary_caret(), (1, 2));
        named(&mut ev, &mut doc, NamedKey::ArrowUp, true);
        assert_eq!(ev.selection_for_test(), Some(((0, 2), (1, 2))));
    }

    #[test]
    fn every_caret_edits_whole_clusters_and_takes_the_ime_commit() {
        let (mut ev, mut doc) = bound_editor("x👍🏽\ny🇩🇪\nz");
        ev.set_caret_for_test(0, 3);
        key(&mut ev, &mut doc, Key::Character("d".into()), true, false);
        key(&mut ev, &mut doc, Key::Character("d".into()), true, false);
        assert_eq!(ev.caret_count(), 3);
        // The third caret is clamped to the end of "z".
        named(&mut ev, &mut doc, NamedKey::Backspace, false);
        assert_eq!(ev.text_for_test(), "x\ny\n");
        assert_eq!(ev.caret_count(), 3);

        // The composition shows at the primary caret only; nothing is typed
        // until the input method commits, then it lands at every caret.
        ime(&mut ev, &mut doc, Ime::Preedit("漢字".into(), Some((3, 3))));
        assert_eq!(ev.text_for_test(), "x\ny\n");
        assert_eq!(ev.primary_display_col(), 2, "caret inside the preedit");
        ime(&mut ev, &mut doc, Ime::Preedit("e\u{301}".into(), None));
        ime(&mut ev, &mut doc, Ime::Commit("e\u{301}".into()));
        assert_eq!(ev.text_for_test(), "xe\u{301}\nye\u{301}\ne\u{301}");
        assert_eq!(ev.primary_display_col(), 3);
        assert!(!ev.composition.has_preedit());

        // One Backspace removes the whole combined letter at each caret.
        named(&mut ev, &mut doc, NamedKey::Backspace, false);
        assert_eq!(ev.text_for_test(), "x\ny\n");
        // Delete at the end of each line joins it with the next.
        named(&mut ev, &mut doc, NamedKey::Delete, false);
        assert_eq!(ev.text_for_test(), "xy");
        assert_eq!(ev.caret_count(), 2, "carets that meet merge");

        // A multi-line commit at two carets keeps both carets after it.
        let (mut ev, mut doc) = bound_editor("a\nb");
        ev.set_caret_for_test(0, 1);
        key(&mut ev, &mut doc, Key::Character("d".into()), true, false);
        ime(&mut ev, &mut doc, Ime::Commit("1\n2".into()));
        assert_eq!(ev.text_for_test(), "a1\n2\nb1\n2");
        assert_eq!(ev.carets[0], Caret::at(1, 1));
        assert_eq!(ev.carets[1], Caret::at(3, 1));
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
        ev.cut_for_test(&mut ui::widget::Edits::lend(&mut doc));
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
