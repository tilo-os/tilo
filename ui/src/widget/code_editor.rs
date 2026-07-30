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
/// Colours are drawn from the theme's terminal ANSI ramp (which every theme
/// defines for light and dark) plus its semantic / text tokens, so syntax
/// re-colours automatically with the active theme and never hard-codes an RGB.
pub fn token_color(kind: TokenKind, theme: &Theme) -> Color {
    let p = &theme.tokens.terminal_palette;
    // ANSI indices: 0..7 normal, 8..15 bright. Named for legibility.
    const MAGENTA: usize = 5;
    const BLUE: usize = 4;
    const GREEN: usize = 2;
    const YELLOW: usize = 3;
    const CYAN: usize = 6;
    const RED: usize = 1;
    match kind {
        TokenKind::Keyword => p.ansi[MAGENTA],
        TokenKind::Type => p.ansi[YELLOW],
        TokenKind::Function => p.ansi[BLUE],
        TokenKind::Number => p.ansi[CYAN],
        TokenKind::StringLit => p.ansi[GREEN],
        TokenKind::CharLit => p.ansi[GREEN],
        TokenKind::Comment => theme.text_dim,
        TokenKind::Operator => theme.text_primary,
        TokenKind::Punctuation => theme.text_dim,
        TokenKind::Attribute => p.ansi[YELLOW],
        TokenKind::Preprocessor => p.ansi[RED],
        TokenKind::Constant => p.ansi[CYAN],
        TokenKind::Variable => theme.text_primary,
        TokenKind::Heading => theme.accent_primary,
        TokenKind::Emphasis => theme.text_primary,
        TokenKind::Strong => theme.text_primary,
        TokenKind::Raw => p.ansi[CYAN],
        TokenKind::Math => p.ansi[MAGENTA],
        TokenKind::Text => theme.text_primary,
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
}
