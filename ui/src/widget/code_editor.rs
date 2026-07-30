//! `CodeEditor` — the shared multi-line code-editing widget.
//!
//! A retained [`Widget`] that puts real code editing — caret, selection
//! (shift/drag, word-wise), clipboard (Ctrl+C/V/X), undo/redo, IME, and a
//! multiline a11y node — on the *same* [`TextInput`](crate::widget::TextInput)
//! caret / `text_selection` / clipboard core every other text field uses. On
//! top of that shared core it adds the three things a code surface needs and a
//! plain field does not:
//!
//! * a **gutter** with line numbers and a current-line highlight,
//! * **syntax highlighting** through a pluggable [`Highlighter`] — the widget
//!   owns rendering/caret/selection, the app owns the language tokenizer — with
//!   token→theme-color mapping so every colour comes from the OS [`Theme`], and
//! * **soft-tab / indent** handling (Tab inserts spaces, Shift+Tab outdents).
//!
//! Apps stop each re-deriving a code editor: `code_editor`, `typst_editor`, and
//! `db_client`'s SQL editor all previously forked caret/selection/gutter/
//! highlight code. They supply only a language [`Highlighter`]; the editor is
//! shared.
//!
//! ```no_run
//! use ui::widget::code_editor::{CodeEditor, Language};
//! let mut editor = CodeEditor::local()
//!     .with_language(Language::Rust)
//!     .with_tab_width(4);
//! editor.set_text("fn main() {\n    println!(\"hi\");\n}\n");
//! ```

use std::collections::HashSet;
use std::ops::Range;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::presence::{PathSegment, PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

/// The syntactic category a run of source falls into. The editor never hard
/// -codes a colour for these — [`token_color`] resolves each to a concrete
/// [`Color`] from the OS [`Theme`], so syntax tracks the active theme in light
/// and dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Language keyword (`fn`, `let`, `SELECT`, …).
    Keyword,
    /// Type name (`String`, `u32`, a capitalised identifier).
    Type,
    /// A called / defined function or macro name.
    Function,
    /// Numeric literal.
    Number,
    /// String literal.
    StringLit,
    /// Character / byte literal.
    CharLit,
    /// Comment (line or block).
    Comment,
    /// Operator glyphs (`+`, `=>`, `::`).
    Operator,
    /// Punctuation / delimiters.
    Punctuation,
    /// Attribute / annotation (`#[derive(..)]`).
    Attribute,
    /// Preprocessor / directive (`#include`, `#!`).
    Preprocessor,
    /// Named constant / literal keyword (`true`, `null`, `None`).
    Constant,
    /// Plain identifier / variable.
    Variable,
    /// Markup heading (Typst `=`, Markdown `#`).
    Heading,
    /// Emphasised (italic) markup run.
    Emphasis,
    /// Strong (bold) markup run.
    Strong,
    /// Raw / inline-code / fenced markup run.
    Raw,
    /// Inline / block maths.
    Math,
    /// Uncategorised body text (renders in the default text colour).
    Text,
}

/// A coloured run within one logical line: a byte `range` (into that line) and
/// its [`TokenKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte range within the *line* the span was produced for.
    pub range: Range<usize>,
    /// The category to colour this run as.
    pub kind: TokenKind,
}

impl Span {
    /// Construct a span over `range` with `kind`.
    pub fn new(range: Range<usize>, kind: TokenKind) -> Self {
        Self { range, kind }
    }
}

/// Opaque cross-line carry so multi-line constructs — block comments, fenced
/// code — survive from one line to the next. `0` is the document start; a
/// [`Highlighter`] owns the meaning of every other value. Line-local
/// highlighters ignore it and always return `0`.
pub type HlState = u64;

/// One logical line's highlight result: ordered, non-overlapping [`Span`]s
/// (gaps render in the default text colour) plus the [`HlState`] the *next*
/// line begins with.
#[derive(Clone, Debug, Default)]
pub struct LineSpans {
    /// Coloured runs on this line, ordered by start, non-overlapping.
    pub spans: Vec<Span>,
    /// Carry the following line should start from.
    pub carry: HlState,
}

impl LineSpans {
    /// A line with the given spans and no carry (line-local highlighter).
    pub fn line_local(spans: Vec<Span>) -> Self {
        Self { spans, carry: 0 }
    }
}

/// A pluggable, per-line source tokenizer. The [`CodeEditor`] owns rendering,
/// caret, and selection; a `Highlighter` owns *only* the language lexing —
/// mapping a line of source to coloured [`Span`]s. Apps implement this for
/// their language (Rust for `code_editor`, Typst for `typst_editor`, SQL for
/// `db_client`) and hand it to [`CodeEditor::with_highlighter`].
pub trait Highlighter {
    /// Tokenize a single logical `line` (never contains `\n`). `carry` is the
    /// [`HlState`] the previous line left (`0` for the first line); the returned
    /// [`LineSpans::carry`] is what the following line begins with. Byte ranges
    /// in the returned spans are relative to `line`.
    fn spans(&self, line: &str, carry: HlState) -> LineSpans;
}

/// Resolve a [`TokenKind`] to a concrete [`Color`] from the OS [`Theme`].
///
/// Colours are drawn from the theme's semantic (success/warning/info/danger),
/// accent-ramp, and text tokens — all of which the theme redefines for light,
/// dark, and each preset — so syntax re-colours automatically with the active
/// theme and never hard-codes an RGB. (The terminal ANSI ramp is deliberately
/// *not* used: it is shared across light and dark.) This mirrors the palette
/// `typst_editor` shipped, generalised with distinct keyword / type slots.
pub fn token_color(kind: TokenKind, theme: &Theme) -> Color {
    let c = &theme.tokens.colors;
    // Accent ramp indices, named for legibility (the ramp is a monochromatic
    // indigo scale, so these are lightness steps of the theme accent).
    const ACCENT_MID: usize = 5; // the dominant accent (indigo-500 in dark)
    const ACCENT_LIGHT: usize = 7;
    const ACCENT_LIGHTER: usize = 6;
    match kind {
        TokenKind::Keyword => c.accent.stops[ACCENT_MID],
        TokenKind::Type => c.semantic.warning,
        TokenKind::Function => c.semantic.info,
        TokenKind::Number => c.accent.stops[ACCENT_LIGHT],
        TokenKind::StringLit => c.semantic.success,
        TokenKind::CharLit => c.semantic.success,
        TokenKind::Comment => c.text_dim,
        TokenKind::Operator => c.text_primary,
        TokenKind::Punctuation => c.text_dim,
        TokenKind::Attribute => c.semantic.warning,
        TokenKind::Preprocessor => c.semantic.danger,
        TokenKind::Constant => c.semantic.info,
        TokenKind::Variable => c.text_primary,
        TokenKind::Heading => c.accent.stops[ACCENT_MID],
        TokenKind::Emphasis => c.text_primary,
        TokenKind::Strong => c.text_primary,
        TokenKind::Raw => c.accent.stops[ACCENT_LIGHT],
        TokenKind::Math => c.accent.stops[ACCENT_LIGHTER],
        TokenKind::Text => c.text_primary,
    }
}

/// A built-in language the editor ships a [`Highlighter`] for. Apps that need a
/// language not listed here implement [`Highlighter`] themselves and pass it to
/// [`CodeEditor::with_highlighter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// No highlighting (plain text).
    PlainText,
    /// Rust.
    Rust,
    /// Typst markup.
    Typst,
    /// SQL.
    Sql,
}

impl Language {
    /// The built-in [`Highlighter`] for this language, or `None` for
    /// [`Language::PlainText`].
    pub fn highlighter(self) -> Option<Box<dyn Highlighter>> {
        match self {
            Language::PlainText => None,
            Language::Rust => Some(Box::new(RustHighlighter::new())),
            Language::Typst => Some(Box::new(TypstHighlighter)),
            Language::Sql => Some(Box::new(SqlHighlighter::new())),
        }
    }
}

// ---------------------------------------------------------------------------
// The widget.
// ---------------------------------------------------------------------------

/// A shared multi-line code editor. Wraps the multiline [`TextInput`] core and
/// enables its gutter + syntax-highlight path, so all caret / selection /
/// clipboard / undo / IME / a11y behaviour is the one shared implementation.
pub struct CodeEditor {
    inner: TextInput,
}

impl CodeEditor {
    /// A CRDT-bound editor at `path` (mirrors [`TextInput::new`]). Bind the
    /// backing Automerge `Text` object with [`CodeEditor::bind_text`].
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self { inner: Self::configure(TextInput::new(path)) }
    }

    /// A local (non-CRDT) editor that owns its own string — read it with
    /// [`CodeEditor::value`], observe edits with [`CodeEditor::take_dirty`].
    pub fn local() -> Self {
        Self { inner: Self::configure(TextInput::local()) }
    }

    /// Shared setup applied to whichever base `TextInput` is used: multiline
    /// body, monospace face, plain chrome (no form-field box), a line-number
    /// gutter, current-line highlight, and 4-space soft tabs by default.
    fn configure(input: TextInput) -> TextInput {
        input
            .multiline(true)
            .with_mono()
            .with_plain_chrome()
            .with_gutter(true)
            .with_current_line_highlight(true)
            .with_tab_width(4)
    }

    /// Supply the language tokenizer. The editor owns rendering/caret/selection;
    /// this owns only the per-line lexing.
    pub fn with_highlighter(mut self, h: Box<dyn Highlighter>) -> Self {
        self.inner.set_highlighter(Some(h));
        self
    }

    /// Replace (or clear with `None`) the language tokenizer after construction.
    pub fn set_highlighter(&mut self, h: Option<Box<dyn Highlighter>>) {
        self.inner.set_highlighter(h);
    }

    /// Use the built-in highlighter for `lang` (a no-op highlighter for
    /// [`Language::PlainText`]).
    pub fn with_language(mut self, lang: Language) -> Self {
        self.inner.set_highlighter(lang.highlighter());
        self
    }

    /// Number of spaces a Tab inserts (and the outdent step for Shift+Tab).
    /// `0` inserts a literal `\t`.
    pub fn with_tab_width(mut self, n: usize) -> Self {
        self.inner.set_tab_width(n);
        self
    }

    /// Toggle the line-number gutter (on by default).
    pub fn with_gutter(mut self, on: bool) -> Self {
        self.inner.set_gutter(on);
        self
    }

    /// Toggle the current-line highlight band (on by default).
    pub fn with_current_line_highlight(mut self, on: bool) -> Self {
        self.inner.set_current_line_highlight(on);
        self
    }

    /// Override the body font size.
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.inner.set_font_size(size);
        self
    }

    /// Seed the initial text (caret parked at the end, not counted as an edit).
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.inner.set_text(s);
        self
    }

    /// Placeholder shown when empty + unfocused.
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.inner.set_placeholder(hint);
        self
    }

    /// A stable id for the e2e harness.
    pub fn with_id(mut self, id: &str) -> Self {
        self.inner = self.inner.with_id(id);
        self
    }

    /// Bind directly to an Automerge `Text` object. Every edit splices into it
    /// and [`sync`](Widget::sync) reads from it.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    /// Pull the latest text from the bound Automerge object — the convenience a
    /// host outside the taffy tree calls each frame instead of the `Widget::sync`
    /// pass. No-op until [`bind_text`](CodeEditor::bind_text) is set.
    pub fn pull(&mut self, doc: &mut AutoCommit) {
        self.inner.sync(doc, &automerge::ROOT);
    }

    /// Stamp inserted runs with this author id (peer-attribution highlight).
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    /// Programmatically set the whole buffer (not counted as a user edit).
    pub fn set_text(&mut self, s: &str) {
        self.inner.set_text(s);
    }

    /// The live buffer text.
    pub fn value(&self) -> &str {
        self.inner.value()
    }

    /// The caret byte offset.
    pub fn cursor_pos(&self) -> usize {
        self.inner.cursor_pos()
    }

    /// Move the caret to a byte offset (clamped to a char boundary by the core).
    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.inner.set_cursor_pos(pos);
    }

    /// Set the selection to span `[anchor, cursor)` (bytes); `anchor == cursor`
    /// clears it. Used by a host driving click+drag selection.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.inner.set_selection(anchor, cursor);
    }

    /// The active selection's byte range, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.inner.selection_range_public()
    }

    /// Move the caret to the start of a 1-based line, clearing the selection.
    pub fn goto_line(&mut self, line_1based: usize) {
        let target = line_1based.saturating_sub(1);
        let mut byte = 0usize;
        let mut line = 0usize;
        let text = self.inner.value();
        if line < target {
            for (i, b) in text.bytes().enumerate() {
                if b == b'\n' {
                    line += 1;
                    if line == target {
                        byte = i + 1;
                        break;
                    }
                }
            }
        }
        self.inner.set_cursor_pos(byte.min(text.len()));
    }

    /// Select the whole buffer.
    pub fn select_all(&mut self) {
        self.inner.select_all();
    }

    /// One-shot read of user edits since the last call (see
    /// [`TextInput::take_dirty`]).
    pub fn take_dirty(&mut self) -> Option<String> {
        self.inner.take_dirty()
    }

    /// Number of logical lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.inner.value().bytes().filter(|&b| b == b'\n').count() + 1
    }

    /// Focus / blur the editor.
    pub fn set_focused(&mut self, on: bool) {
        self.inner.set_focus(on);
    }

    /// Whether the editor holds focus.
    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// Assign the editor's rect when embedded outside the taffy tree (a
    /// monolithic immediate-mode host). Pairs with [`draw_embedded`].
    ///
    /// [`draw_embedded`]: CodeEditor::draw_embedded
    pub fn set_rect(&mut self, rect: Rect) {
        self.inner.apply_layout_external(rect);
    }

    /// The editor's laid-out rect.
    pub fn rect(&self) -> Rect {
        self.inner.get_rect()
    }

    /// Scroll the body by `dy` logical px (wheel), clamped to the content.
    pub fn scroll_by(&mut self, dy: f64) {
        self.inner.scroll_by(dy);
    }

    // ---- geometry for host overlays (diagnostics, markers) -----------------

    /// The on-screen caret rect for a byte offset — a host uses this to align a
    /// gutter marker with a source line (`caret_rect(theme, line_start)`).
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.inner.caret_rect(theme, byte)
    }

    /// On-screen rects covering byte range `[start, end)` — one per visual row.
    /// A host draws a diagnostic squiggle along each rect's bottom edge.
    pub fn selection_rects(&self, theme: &Theme, start: usize, end: usize) -> Vec<Rect> {
        self.inner.selection_rects(theme, start, end)
    }

    /// The shared text/caret line-height in logical px.
    pub fn line_height(&self) -> f64 {
        self.inner.body_line_height()
    }

    /// The line-number gutter width in logical px.
    pub fn gutter_width(&self) -> f64 {
        self.inner.gutter_width()
    }

    /// Resolve a logical-space point to a byte offset (hover hit-test).
    pub fn byte_at_point(&self, point: Point) -> usize {
        self.inner.byte_at_point(point)
    }

    /// Draw when embedded outside the taffy tree — a monolithic host that owns
    /// its own layout calls [`set_rect`] then this each frame.
    ///
    /// [`set_rect`]: CodeEditor::set_rect
    pub fn draw_embedded(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let presence = PresenceCtx::default();
        self.inner.draw(scene, transform, text_renderer, theme, &presence);
    }

    /// Borrow the underlying [`TextInput`] for advanced hosts.
    pub fn inner(&self) -> &TextInput {
        &self.inner
    }

    /// Mutably borrow the underlying [`TextInput`] for advanced hosts.
    pub fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }
}

impl Widget for CodeEditor {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Build the leaf the way the inner multiline input would, but register
        // *this* widget so the host drives CodeEditor's own passes.
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                ..Default::default()
            })
            .unwrap();
        let _ = theme;
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.inner.measure(known, available, text_renderer)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        self.inner.apply_layout_external(rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.inner
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.inner.handle_event(
            event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy,
        );
    }

    fn name(&self) -> &str {
        "CodeEditor"
    }

    fn test_id(&self) -> Option<&str> {
        self.inner.test_id()
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn layout_needs_measure(&self) -> bool {
        self.inner.layout_needs_measure()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.inner.cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        self.inner.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inner.accessibility()
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.inner.perform_action(action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }

    fn needs_commit(&self) -> bool {
        self.inner.needs_commit()
    }

    fn on_commit(&mut self) {
        self.inner.on_commit();
    }

    fn get_state(&self) -> Option<String> {
        self.inner.get_state()
    }

    fn set_state(&mut self, state: &str) {
        self.inner.set_state(state);
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.inner.text_review()
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        self.inner.presence_key()
    }
}

// ---------------------------------------------------------------------------
// Built-in highlighters.
// ---------------------------------------------------------------------------

/// Helper: byte offset of the next occurrence of `needle` at/after `from`.
fn find_from(hay: &str, from: usize, needle: char) -> Option<usize> {
    hay[from..].find(needle).map(|i| from + i)
}

/// A generic keyword-based highlighter: a set of keywords, an optional
/// line-comment prefix, and a string quote character. Enough for many
/// C-family / scripting languages, and the base other built-ins refine.
pub struct GenericHighlighter {
    keywords: HashSet<String>,
    constants: HashSet<String>,
    line_comment: Option<String>,
    /// Case-insensitive keyword matching (SQL).
    case_insensitive: bool,
}

impl GenericHighlighter {
    /// Build from keyword and constant lists, a line-comment prefix (e.g.
    /// `"//"`), and whether keyword matching is case-insensitive.
    pub fn new(
        keywords: &[&str],
        constants: &[&str],
        line_comment: Option<&str>,
        case_insensitive: bool,
    ) -> Self {
        let norm = |s: &str| if case_insensitive { s.to_ascii_uppercase() } else { s.to_string() };
        Self {
            keywords: keywords.iter().map(|s| norm(s)).collect(),
            constants: constants.iter().map(|s| norm(s)).collect(),
            line_comment: line_comment.map(|s| s.to_string()),
            case_insensitive,
        }
    }

    fn classify_word(&self, w: &str) -> Option<TokenKind> {
        let key = if self.case_insensitive { w.to_ascii_uppercase() } else { w.to_string() };
        if self.keywords.contains(&key) {
            Some(TokenKind::Keyword)
        } else if self.constants.contains(&key) {
            Some(TokenKind::Constant)
        } else if w.chars().next().is_some_and(|c| c.is_uppercase()) {
            Some(TokenKind::Type)
        } else {
            None
        }
    }
}

impl Highlighter for GenericHighlighter {
    fn spans(&self, line: &str, _carry: HlState) -> LineSpans {
        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let n = line.len();
        let mut i = 0usize;
        while i < n {
            // Line comment.
            if let Some(lc) = &self.line_comment {
                if line[i..].starts_with(lc.as_str()) {
                    spans.push(Span::new(i..n, TokenKind::Comment));
                    break;
                }
            }
            let c = bytes[i] as char;
            // String literal (double or single quote).
            if c == '"' || c == '\'' {
                let end = string_end(line, i, c);
                spans.push(Span::new(i..end, TokenKind::StringLit));
                i = end;
                continue;
            }
            // Number.
            if c.is_ascii_digit() {
                let end = number_end(line, i);
                spans.push(Span::new(i..end, TokenKind::Number));
                i = end;
                continue;
            }
            // Identifier / keyword.
            if is_ident_start(c) {
                let end = ident_end(line, i);
                let word = &line[i..end];
                let followed_by_paren = line[end..].trim_start().starts_with('(');
                if let Some(kind) = self.classify_word(word) {
                    spans.push(Span::new(i..end, kind));
                } else if followed_by_paren {
                    spans.push(Span::new(i..end, TokenKind::Function));
                }
                i = end;
                continue;
            }
            i += next_char_len(line, i);
        }
        LineSpans::line_local(spans)
    }
}

/// Bit in [`HlState`] marking "the next line begins inside a `/* … */` block
/// comment" (used by [`RustHighlighter`] and [`SqlHighlighter`]).
const IN_BLOCK_COMMENT: HlState = 1;

/// A pragmatic Rust highlighter — a per-line lexer (not a full parser) that
/// recognises the constructs that matter visually: keywords, types, functions,
/// numbers, strings, char literals, attributes, line comments, and `/* … */`
/// block comments (carried across lines).
pub struct RustHighlighter {
    generic: GenericHighlighter,
}

impl RustHighlighter {
    /// A Rust highlighter with the standard keyword / constant sets.
    pub fn new() -> Self {
        const KW: &[&str] = &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
            "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
            "type", "unsafe", "use", "where", "while", "union",
        ];
        const CONST: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];
        Self { generic: GenericHighlighter::new(KW, CONST, Some("//"), false) }
    }
}

impl Default for RustHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for RustHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let n = line.len();
        // Continuing a block comment from a previous line.
        if carry & IN_BLOCK_COMMENT != 0 {
            if let Some(close) = line.find("*/") {
                let end = close + 2;
                let mut ls = self.spans(&line[end..], 0);
                shift_spans(&mut ls.spans, end);
                let mut spans = vec![Span::new(0..end, TokenKind::Comment)];
                spans.append(&mut ls.spans);
                return LineSpans { spans, carry: ls.carry };
            }
            return LineSpans { spans: vec![Span::new(0..n, TokenKind::Comment)], carry: IN_BLOCK_COMMENT };
        }

        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut carry_out = 0;
        while i < n {
            // Line comment.
            if line[i..].starts_with("//") {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            // Block comment open.
            if line[i..].starts_with("/*") {
                if let Some(rel) = line[i + 2..].find("*/") {
                    let end = i + 2 + rel + 2;
                    spans.push(Span::new(i..end, TokenKind::Comment));
                    i = end;
                    continue;
                }
                spans.push(Span::new(i..n, TokenKind::Comment));
                carry_out = IN_BLOCK_COMMENT;
                break;
            }
            let c = bytes[i] as char;
            // Attribute: `#[ … ]` or `#![ … ]`.
            if c == '#' && (line[i..].starts_with("#[") || line[i..].starts_with("#![")) {
                let end = find_from(line, i, ']').map(|e| e + 1).unwrap_or(n);
                spans.push(Span::new(i..end, TokenKind::Attribute));
                i = end;
                continue;
            }
            // String literal.
            if c == '"' {
                let end = string_end(line, i, '"');
                spans.push(Span::new(i..end, TokenKind::StringLit));
                i = end;
                continue;
            }
            // Char literal vs lifetime: `'a'` / `'\n'` is a char; `'a` alone is a
            // lifetime (colour it as an attribute-ish type marker → Keyword).
            if c == '\'' {
                if let Some(end) = char_literal_end(line, i) {
                    spans.push(Span::new(i..end, TokenKind::CharLit));
                    i = end;
                    continue;
                }
                // Lifetime: 'ident
                let end = ident_end(line, i + 1);
                if end > i + 1 {
                    spans.push(Span::new(i..end, TokenKind::Keyword));
                    i = end;
                    continue;
                }
            }
            // Number.
            if c.is_ascii_digit() {
                let end = number_end(line, i);
                spans.push(Span::new(i..end, TokenKind::Number));
                i = end;
                continue;
            }
            // Identifier / keyword / macro / function call.
            if is_ident_start(c) {
                let end = ident_end(line, i);
                let word = &line[i..end];
                let after = &line[end..];
                let is_macro = after.starts_with('!');
                let followed_by_paren = after.trim_start().starts_with('(');
                if let Some(kind) = self.generic.classify_word(word) {
                    spans.push(Span::new(i..end, kind));
                } else if is_macro {
                    // include the `!` in the function span.
                    spans.push(Span::new(i..end + 1, TokenKind::Function));
                    i = end + 1;
                    continue;
                } else if followed_by_paren {
                    spans.push(Span::new(i..end, TokenKind::Function));
                }
                i = end;
                continue;
            }
            i += next_char_len(line, i);
        }
        LineSpans { spans, carry: carry_out }
    }
}

/// A pragmatic SQL highlighter: case-insensitive keywords, `'…'` strings,
/// `--` line comments, `/* … */` block comments (carried), and numbers.
pub struct SqlHighlighter {
    generic: GenericHighlighter,
}

impl SqlHighlighter {
    /// A SQL highlighter with a broad ANSI keyword set.
    pub fn new() -> Self {
        const KW: &[&str] = &[
            "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE",
            "CREATE", "TABLE", "DROP", "ALTER", "ADD", "COLUMN", "INDEX", "VIEW", "JOIN", "INNER",
            "LEFT", "RIGHT", "OUTER", "FULL", "ON", "GROUP", "BY", "ORDER", "HAVING", "LIMIT",
            "OFFSET", "AS", "AND", "OR", "NOT", "IN", "LIKE", "BETWEEN", "IS", "DISTINCT", "UNION",
            "ALL", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "DEFAULT", "CONSTRAINT", "UNIQUE",
            "CASE", "WHEN", "THEN", "ELSE", "END", "COUNT", "SUM", "AVG", "MIN", "MAX", "WITH",
        ];
        const CONST: &[&str] = &["NULL", "TRUE", "FALSE"];
        Self { generic: GenericHighlighter::new(KW, CONST, None, true) }
    }
}

impl Default for SqlHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for SqlHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let n = line.len();
        if carry & IN_BLOCK_COMMENT != 0 {
            if let Some(close) = line.find("*/") {
                let end = close + 2;
                let mut ls = self.spans(&line[end..], 0);
                shift_spans(&mut ls.spans, end);
                let mut spans = vec![Span::new(0..end, TokenKind::Comment)];
                spans.append(&mut ls.spans);
                return LineSpans { spans, carry: ls.carry };
            }
            return LineSpans { spans: vec![Span::new(0..n, TokenKind::Comment)], carry: IN_BLOCK_COMMENT };
        }
        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut carry_out = 0;
        while i < n {
            if line[i..].starts_with("--") {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            if line[i..].starts_with("/*") {
                if let Some(rel) = line[i + 2..].find("*/") {
                    let end = i + 2 + rel + 2;
                    spans.push(Span::new(i..end, TokenKind::Comment));
                    i = end;
                    continue;
                }
                spans.push(Span::new(i..n, TokenKind::Comment));
                carry_out = IN_BLOCK_COMMENT;
                break;
            }
            let c = bytes[i] as char;
            if c == '\'' {
                let end = string_end(line, i, '\'');
                spans.push(Span::new(i..end, TokenKind::StringLit));
                i = end;
                continue;
            }
            if c.is_ascii_digit() {
                let end = number_end(line, i);
                spans.push(Span::new(i..end, TokenKind::Number));
                i = end;
                continue;
            }
            if is_ident_start(c) {
                let end = ident_end(line, i);
                let word = &line[i..end];
                if let Some(kind) = self.generic.classify_word(word) {
                    // Don't up-case-classify identifiers as Type in SQL; only
                    // keyword/constant matter.
                    if matches!(kind, TokenKind::Keyword | TokenKind::Constant) {
                        spans.push(Span::new(i..end, kind));
                    }
                }
                i = end;
                continue;
            }
            i += next_char_len(line, i);
        }
        LineSpans { spans, carry: carry_out }
    }
}

/// Bit in [`HlState`] marking "the next line is inside a ```` ``` ```` fenced
/// code block" (Typst).
const IN_FENCE: HlState = 1;

/// A pragmatic Typst highlighter — a per-line lexer recognising headings,
/// comments, strings, `#`-functions, numbers, raw code (inline and fenced),
/// maths, `*strong*` and `_emph_` runs. Fenced code blocks are carried across
/// lines. Ported from `typst_editor`'s in-app highlighter so the migration
/// preserves its look.
pub struct TypstHighlighter;

impl Highlighter for TypstHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let n = line.len();
        let trimmed = line.trim_start();
        // Fence delimiter line toggles the fenced state; it renders as raw.
        if trimmed.starts_with("```") {
            let next = if carry & IN_FENCE != 0 { 0 } else { IN_FENCE };
            return LineSpans { spans: vec![Span::new(0..n, TokenKind::Raw)], carry: next };
        }
        if carry & IN_FENCE != 0 {
            return LineSpans { spans: vec![Span::new(0..n, TokenKind::Raw)], carry: IN_FENCE };
        }
        // Heading line.
        if trimmed.starts_with('=') {
            return LineSpans { spans: vec![Span::new(0..n, TokenKind::Heading)], carry: 0 };
        }
        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0usize;
        while i < n {
            let c = bytes[i] as char;
            // Line comment.
            if line[i..].starts_with("//") {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            // String literal.
            if c == '"' {
                let end = find_from(line, i + 1, '"').map(|e| e + 1).unwrap_or(n);
                spans.push(Span::new(i..end, TokenKind::StringLit));
                i = end;
                continue;
            }
            // Inline raw code.
            if c == '`' {
                let end = find_from(line, i + 1, '`').map(|e| e + 1).unwrap_or(n);
                spans.push(Span::new(i..end, TokenKind::Raw));
                i = end;
                continue;
            }
            // Maths span.
            if c == '$' {
                let end = find_from(line, i + 1, '$').map(|e| e + 1).unwrap_or(n);
                spans.push(Span::new(i..end, TokenKind::Math));
                i = end;
                continue;
            }
            // `#` function / keyword.
            if c == '#' {
                let mut j = i + 1;
                while j < n {
                    let cj = bytes[j] as char;
                    if cj.is_alphanumeric() || cj == '_' || cj == '.' {
                        j += 1;
                    } else {
                        break;
                    }
                }
                let end = j.max(i + 1);
                spans.push(Span::new(i..end, TokenKind::Function));
                i = end;
                continue;
            }
            // *strong* run.
            if c == '*' {
                if let Some(close) = find_from(line, i + 1, '*') {
                    if close > i + 1 {
                        spans.push(Span::new(i..close + 1, TokenKind::Strong));
                        i = close + 1;
                        continue;
                    }
                }
            }
            // _emph_ run.
            if c == '_' {
                if let Some(close) = find_from(line, i + 1, '_') {
                    if close > i + 1 {
                        spans.push(Span::new(i..close + 1, TokenKind::Emphasis));
                        i = close + 1;
                        continue;
                    }
                }
            }
            // Number with optional unit suffix (12pt, 1.5cm, 80%).
            if c.is_ascii_digit() {
                let mut j = i;
                while j < n && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                    j += 1;
                }
                while j < n && ((bytes[j] as char).is_ascii_alphabetic() || bytes[j] == b'%') {
                    j += 1;
                }
                spans.push(Span::new(i..j, TokenKind::Number));
                i = j;
                continue;
            }
            i += next_char_len(line, i);
        }
        LineSpans { spans, carry: 0 }
    }
}

// ---------------------------------------------------------------------------
// Lexing helpers (byte-offset based, char-boundary safe).
// ---------------------------------------------------------------------------

fn next_char_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// End (exclusive byte offset) of an identifier starting at `i`.
fn ident_end(s: &str, i: usize) -> usize {
    let mut j = i;
    for c in s[i..].chars() {
        if is_ident_continue(c) {
            j += c.len_utf8();
        } else {
            break;
        }
    }
    j
}

/// End (exclusive) of a numeric literal starting at `i` — digits, a decimal
/// point, hex/underscore digits, and a trailing type/unit suffix.
fn number_end(s: &str, i: usize) -> usize {
    let mut j = i;
    for c in s[i..].chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            j += c.len_utf8();
        } else {
            break;
        }
    }
    j
}

/// End (exclusive) of a string literal that opened with `quote` at `i`,
/// honouring `\`-escapes. Runs to end-of-line if unterminated.
fn string_end(s: &str, i: usize, quote: char) -> usize {
    let mut j = i + quote.len_utf8();
    let mut escaped = false;
    for c in s[j..].chars() {
        j += c.len_utf8();
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            break;
        }
    }
    j.min(s.len())
}

/// If `i` opens a Rust char literal (`'x'`, `'\n'`, `'\u{1F}'`), return its end
/// (exclusive); otherwise `None` (so the caller can treat `'` as a lifetime).
fn char_literal_end(s: &str, i: usize) -> Option<usize> {
    let rest = &s[i + 1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        // Escape: consume until the closing quote.
        for (off, c) in chars {
            if c == '\'' {
                return Some(i + 1 + off + 1);
            }
        }
        None
    } else {
        // A single char then a closing quote.
        let after = i + 1 + first.len_utf8();
        if s[after..].starts_with('\'') {
            Some(after + 1)
        } else {
            None
        }
    }
}

/// Shift every span's range right by `by` bytes (used when a leading block
/// -comment prefix is prepended to a line's remaining spans).
fn shift_spans(spans: &mut [Span], by: usize) {
    for sp in spans.iter_mut() {
        sp.range.start += by;
        sp.range.end += by;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn rust_keyword_and_function_classified() {
        let hl = RustHighlighter::new();
        let ls = hl.spans("fn main() {", 0);
        let kinds: Vec<_> = ls.spans.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&TokenKind::Keyword), "`fn` is a keyword: {kinds:?}");
        assert!(kinds.contains(&TokenKind::Function), "`main` before `(` is a function: {kinds:?}");
    }

    #[test]
    fn rust_span_ranges_map_to_the_right_text() {
        let hl = RustHighlighter::new();
        let line = "let x = 42;";
        let ls = hl.spans(line, 0);
        // `let` keyword, `42` number.
        let kw = ls.spans.iter().find(|s| s.kind == TokenKind::Keyword).unwrap();
        assert_eq!(&line[kw.range.clone()], "let");
        let num = ls.spans.iter().find(|s| s.kind == TokenKind::Number).unwrap();
        assert_eq!(&line[num.range.clone()], "42");
    }

    #[test]
    fn rust_string_literal_span() {
        let hl = RustHighlighter::new();
        let line = r#"let s = "hi there";"#;
        let ls = hl.spans(line, 0);
        let str_span = ls.spans.iter().find(|s| s.kind == TokenKind::StringLit).unwrap();
        assert_eq!(&line[str_span.range.clone()], r#""hi there""#);
    }

    #[test]
    fn rust_block_comment_carries_across_lines() {
        let hl = RustHighlighter::new();
        let l0 = hl.spans("code(); /* open", 0);
        assert_eq!(l0.carry & IN_BLOCK_COMMENT, IN_BLOCK_COMMENT, "unterminated /* carries");
        let l1 = hl.spans("still comment", l0.carry);
        assert!(l1.spans.iter().all(|s| s.kind == TokenKind::Comment));
        assert_eq!(l1.carry & IN_BLOCK_COMMENT, IN_BLOCK_COMMENT);
        let l2 = hl.spans("*/ done();", l1.carry);
        assert_eq!(l2.carry & IN_BLOCK_COMMENT, 0, "closing */ clears the carry");
        // The `*/` prefix is a comment, and `done` after it is a function.
        assert_eq!(l2.spans[0].kind, TokenKind::Comment);
        assert!(l2.spans.iter().any(|s| s.kind == TokenKind::Function));
    }

    #[test]
    fn sql_keywords_are_case_insensitive() {
        let hl = SqlHighlighter::new();
        let ls = hl.spans("select * from users where id = 1", 0);
        let kws: Vec<_> = ls
            .spans
            .iter()
            .filter(|s| s.kind == TokenKind::Keyword)
            .map(|s| "select * from users where id = 1"[s.range.clone()].to_string())
            .collect();
        assert!(kws.contains(&"select".to_string()));
        assert!(kws.contains(&"from".to_string()));
        assert!(kws.contains(&"where".to_string()));
    }

    #[test]
    fn typst_heading_and_function() {
        let hl = TypstHighlighter;
        assert_eq!(hl.spans("= Title", 0).spans[0].kind, TokenKind::Heading);
        let ls = hl.spans(r#"#set text("hi")"#, 0);
        let kinds: Vec<_> = ls.spans.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&TokenKind::Function));
        assert!(kinds.contains(&TokenKind::StringLit));
    }

    #[test]
    fn typst_fenced_block_carries() {
        let hl = TypstHighlighter;
        let open = hl.spans("```rust", 0);
        assert_eq!(open.carry & IN_FENCE, IN_FENCE);
        let mid = hl.spans("let x = 1;", open.carry);
        assert!(mid.spans.iter().all(|s| s.kind == TokenKind::Raw));
        let close = hl.spans("```", mid.carry);
        assert_eq!(close.carry & IN_FENCE, 0);
    }

    #[test]
    fn token_color_changes_with_theme() {
        use crate::color_compat::ColorCompat;
        // A keyword is coloured from the theme's ANSI ramp, so light and dark
        // resolve to different concrete colours.
        let dark = token_color(TokenKind::Keyword, &Theme::dark());
        let light = token_color(TokenKind::Keyword, &Theme::light());
        let rgb = |c: Color| [c.r(), c.g(), c.b()];
        assert_ne!(rgb(dark), rgb(light), "keyword colour must track the theme");
        // Comment maps to the theme's dim text token.
        assert_eq!(
            rgb(token_color(TokenKind::Comment, &Theme::dark())),
            rgb(Theme::dark().text_dim),
        );
    }

    #[test]
    fn language_maps_to_a_highlighter() {
        assert!(Language::Rust.highlighter().is_some());
        assert!(Language::Sql.highlighter().is_some());
        assert!(Language::Typst.highlighter().is_some());
        assert!(Language::PlainText.highlighter().is_none());
    }

    // ---- widget-level: the real edit/caret/selection/clipboard/a11y path ----

    use automerge::AutoCommit;
    use taffy::{Style, TaffyTree};
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, NamedKey};

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        use winit::keyboard::ModifiersState;
        let mut s = ModifiersState::empty();
        if ctrl {
            s |= ModifiersState::CONTROL;
        }
        if shift {
            s |= ModifiersState::SHIFT;
        }
        Modifiers::from(s)
    }

    fn char_event(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(winit::keyboard::SmolStr::new(s));
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(winit::keyboard::SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn named_event(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn drive(ed: &mut CodeEditor, doc: &mut AutoCommit, ev: &WindowEvent, m: &Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        ed.handle_event(
            ev, 1.0, m, doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy,
        );
        handled
    }

    fn draw_once(ed: &mut CodeEditor, theme: &Theme) {
        let presence = PresenceCtx::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(ed, &mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    use crate::text_render::TextRenderer;

    #[test]
    fn typing_inserts_on_the_real_key_path() {
        let mut ed = CodeEditor::local();
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        for ch in ["f", "n", " ", "x"] {
            assert!(drive(&mut ed, &mut doc, &char_event(ch), &mods(false, false)));
        }
        assert_eq!(ed.value(), "fn x");
        assert_eq!(ed.cursor_pos(), 4);
    }

    #[test]
    fn enter_inserts_a_real_newline_and_gutter_counts_lines() {
        let mut ed = CodeEditor::local();
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        for ch in ["a"] {
            drive(&mut ed, &mut doc, &char_event(ch), &mods(false, false));
        }
        drive(&mut ed, &mut doc, &named_event(NamedKey::Enter), &mods(false, false));
        drive(&mut ed, &mut doc, &char_event("b"), &mods(false, false));
        assert_eq!(ed.value(), "a\nb");
        assert_eq!(ed.line_count(), 2, "gutter line count reflects the two logical lines");
    }

    #[test]
    fn tab_inserts_soft_spaces_and_shift_tab_outdents() {
        let mut ed = CodeEditor::local().with_tab_width(4);
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        assert!(drive(&mut ed, &mut doc, &named_event(NamedKey::Tab), &mods(false, false)));
        drive(&mut ed, &mut doc, &char_event("x"), &mods(false, false));
        assert_eq!(ed.value(), "    x", "Tab inserts four spaces");
        // Shift+Tab removes one indent level.
        assert!(drive(&mut ed, &mut doc, &named_event(NamedKey::Tab), &mods(false, true)));
        assert_eq!(ed.value(), "x", "Shift+Tab outdents the leading four spaces");
    }

    #[test]
    fn selection_and_cut_run_through_the_real_path() {
        let mut ed = CodeEditor::local().with_initial_value("abc");
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        // Select all through the real Ctrl+A path, then confirm a live selection.
        assert!(drive(&mut ed, &mut doc, &char_event("a"), &mods(true, false)));
        assert_eq!(
            ed.inner().selection_range_public(),
            Some((0, 3)),
            "Ctrl+A selects the whole buffer on the real path",
        );
        // Ctrl+X removes the selection regardless of the clipboard backend.
        assert!(drive(&mut ed, &mut doc, &char_event("x"), &mods(true, false)));
        assert_eq!(ed.value(), "", "Ctrl+X cuts the selection through the shared core");
    }

    #[test]
    fn shift_arrow_extends_selection_word_wise() {
        let mut ed = CodeEditor::local().with_initial_value("hello world");
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        // Caret home, then Ctrl+Shift+Right selects the first word.
        drive(&mut ed, &mut doc, &named_event(NamedKey::Home), &mods(false, false));
        drive(&mut ed, &mut doc, &named_event(NamedKey::ArrowRight), &mods(true, true));
        let sel = ed.inner().selection_range_public().expect("a selection exists");
        assert_eq!(sel.0, 0);
        assert!(sel.1 >= 5, "word-wise extend covers at least `hello`: {sel:?}");
    }

    #[test]
    fn goto_line_moves_the_caret_to_the_line_start() {
        let mut ed = CodeEditor::local().with_initial_value("one\ntwo\nthree");
        ed.goto_line(2);
        assert_eq!(ed.cursor_pos(), 4, "line 2 starts just after the first newline");
        assert_eq!(&ed.value()[ed.cursor_pos()..ed.cursor_pos() + 3], "two");
    }

    #[test]
    fn accessibility_reports_a_multiline_text_input() {
        use crate::a11y::Role;
        let ed = CodeEditor::local().with_initial_value("fn main() {}");
        let node = ed.accessibility().expect("code editor exposes an a11y node");
        assert_eq!(node.role, Role::TextInput);
        assert!(node.state.multiline, "code editor reports a multiline text field");
        assert_eq!(node.value.as_deref(), Some("fn main() {}"));
    }

    #[test]
    fn highlighter_colours_segments_from_the_theme() {
        use crate::color_compat::ColorCompat;
        let theme = Theme::dark();
        let mut ed = CodeEditor::local().with_language(Language::Rust);
        ed.set_text("let x = 1");
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        // A draw builds the visual lines + highlight cache and runs the real
        // colouring path (`row_segments` → `token_color`).
        draw_once(&mut ed, &theme);
        let segs = ed.inner().code_row_colors(&theme);
        let rgb = |c: Color| [c.r(), c.g(), c.b()];
        let kw = segs.iter().find(|(t, _)| t == "let").expect("`let` is a drawn segment");
        assert_eq!(
            rgb(kw.1),
            rgb(token_color(TokenKind::Keyword, &theme)),
            "the `let` keyword is drawn in the themed keyword colour",
        );
        // A default-coloured gap (whitespace / identifier) uses text_primary.
        assert!(
            segs.iter().any(|(_, c)| rgb(*c) == rgb(theme.text_primary)),
            "uncoloured runs fall back to the theme text colour",
        );
    }

    #[test]
    fn no_gutter_offset_when_gutter_disabled() {
        // With the gutter on, the caret x is pushed right of the plain inset;
        // turning it off collapses that offset — proves the gutter participates
        // in the shared caret geometry.
        let theme = Theme::dark();
        let mut with_g = CodeEditor::local().with_initial_value("x");
        with_g.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        draw_once(&mut with_g, &theme);
        let caret_with = with_g.inner().caret_rect(&theme, 0).x0;

        let mut without_g = CodeEditor::local().with_gutter(false).with_initial_value("x");
        without_g.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        draw_once(&mut without_g, &theme);
        let caret_without = without_g.inner().caret_rect(&theme, 0).x0;

        assert!(
            caret_with > caret_without + 10.0,
            "the gutter shifts the caret right ({caret_with} vs {caret_without})",
        );
    }
}
