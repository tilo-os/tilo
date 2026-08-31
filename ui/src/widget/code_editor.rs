//! The shared multi-line code-editing widget: a retained [`Widget`] that
//! puts real code editing — caret, selection (shift/drag, word-wise),
//! clipboard, undo/redo, IME, and a multiline a11y node — on the *same*
//! [`TextInput`](crate::widget::TextInput) core every other text field
//! uses. On top of that it adds a **gutter** with line numbers and a
//! current-line highlight, **syntax highlighting** through a pluggable
//! [`Highlighter`] (the widget owns rendering/caret/selection, the app
//! owns the language tokenizer, and every colour comes from the OS
//! [`Theme`]), and **soft-tab / indent** handling.
//!
//! `code_editor`, `typst_editor`, and `db_client`'s SQL editor previously
//! each forked their own caret/selection/gutter/highlight code; now they
//! supply only a language [`Highlighter`] and share this editor.
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
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, Widget};

/// The syntactic category a run of source falls into. The editor never
/// hard-codes a colour for these — [`token_color`] resolves each to a
/// concrete [`Color`] from the OS [`Theme`], so syntax tracks the active
/// theme in light and dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    /// Type name (`String`, `u32`, a capitalised identifier).
    Type,
    Function,
    Number,
    StringLit,
    CharLit,
    Comment,
    Operator,
    Punctuation,
    /// `#[derive(..)]`.
    Attribute,
    /// `#include`, `#!`.
    Preprocessor,
    /// Named constant / literal keyword (`true`, `null`, `None`).
    Constant,
    Variable,
    /// Markup heading (Typst `=`, Markdown `#`).
    Heading,
    Emphasis,
    Strong,
    /// Raw / inline-code / fenced markup run.
    Raw,
    Math,
    /// Uncategorised body text (renders in the default text colour).
    Text,
}

/// A coloured run within one logical line: a byte `range` (into that line)
/// and its [`TokenKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

impl Span {
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
    pub spans: Vec<Span>,
    pub carry: HlState,
}

impl LineSpans {
    /// No carry — for a line-local highlighter.
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
    /// `line` never contains `\n`. `carry` is the [`HlState`] the previous
    /// line left (`0` for the first line); the returned [`LineSpans::carry`]
    /// is what the following line begins with. Byte ranges in the returned
    /// spans are relative to `line`.
    fn spans(&self, line: &str, carry: HlState) -> LineSpans;
}

/// Colours are drawn from the theme's semantic/accent-ramp/text tokens —
/// all redefined per theme — so syntax re-colours with the active theme
/// and never hard-codes an RGB. The terminal ANSI ramp is deliberately
/// *not* used: it is shared across light and dark.
pub fn token_color(kind: TokenKind, theme: &Theme) -> Color {
    // Accent ramp indices, named for legibility (the ramp is a monochromatic
    // indigo scale, so these are lightness steps of the theme accent).
    const ACCENT_MID: usize = 5; // the dominant accent (indigo-500 in dark)
    const ACCENT_LIGHT: usize = 7;
    const ACCENT_LIGHTER: usize = 6;
    let c = &theme.tokens.colors;
    match kind {
        TokenKind::Keyword | TokenKind::Heading => c.accent.stops[ACCENT_MID],
        TokenKind::Type | TokenKind::Attribute => c.semantic.warning,
        TokenKind::Function | TokenKind::Constant => c.semantic.info,
        TokenKind::Number | TokenKind::Raw => c.accent.stops[ACCENT_LIGHT],
        TokenKind::StringLit | TokenKind::CharLit => c.semantic.success,
        TokenKind::Comment | TokenKind::Punctuation => c.text_dim,
        TokenKind::Operator
        | TokenKind::Variable
        | TokenKind::Emphasis
        | TokenKind::Strong
        | TokenKind::Text => c.text_primary,
        TokenKind::Preprocessor => c.semantic.danger,
        TokenKind::Math => c.accent.stops[ACCENT_LIGHTER],
    }
}

/// A built-in language the editor ships a [`Highlighter`] for. Apps that need a
/// language not listed here implement [`Highlighter`] themselves and pass it to
/// [`CodeEditor::with_highlighter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    PlainText,
    Rust,
    Typst,
    Sql,
}

impl Language {
    /// `None` for [`Language::PlainText`].
    pub fn highlighter(self) -> Option<Box<dyn Highlighter>> {
        match self {
            Language::PlainText => None,
            Language::Rust => Some(Box::new(RustHighlighter::new())),
            Language::Typst => Some(Box::new(TypstHighlighter)),
            Language::Sql => Some(Box::new(SqlHighlighter::new())),
        }
    }
}

/// Wraps the multiline [`TextInput`] core and enables its gutter +
/// syntax-highlight path, so all caret/selection/clipboard/undo/IME/a11y
/// behaviour is the one shared implementation.
pub struct CodeEditor {
    inner: TextInput,
}

impl CodeEditor {
    /// CRDT-bound at `path` (mirrors [`TextInput::new`]). Bind the backing
    /// Automerge `Text` object with [`CodeEditor::bind_text`].
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self {
            inner: Self::configure(TextInput::new(path)),
        }
    }

    /// Non-CRDT, owns its own string — read with [`CodeEditor::value`],
    /// observe edits with [`CodeEditor::take_dirty`].
    pub fn local() -> Self {
        Self {
            inner: Self::configure(TextInput::local()),
        }
    }

    /// Multiline body, monospace face, plain chrome, a line-number gutter,
    /// current-line highlight, and 4-space soft tabs by default.
    fn configure(input: TextInput) -> TextInput {
        input
            .multiline(true)
            .with_mono()
            .with_plain_chrome()
            .with_gutter(true)
            .with_current_line_highlight(true)
            .with_tab_width(4)
    }

    /// The editor owns rendering/caret/selection; this owns only the
    /// per-line lexing.
    #[must_use]
    pub fn with_highlighter(mut self, h: Box<dyn Highlighter>) -> Self {
        self.inner.set_highlighter(Some(h));
        self
    }

    pub fn set_highlighter(&mut self, h: Option<Box<dyn Highlighter>>) {
        self.inner.set_highlighter(h);
    }

    /// A no-op highlighter for [`Language::PlainText`].
    #[must_use]
    pub fn with_language(mut self, lang: Language) -> Self {
        self.inner.set_highlighter(lang.highlighter());
        self
    }

    /// `0` inserts a literal `\t`.
    #[must_use]
    pub fn with_tab_width(mut self, n: usize) -> Self {
        self.inner.set_tab_width(n);
        self
    }

    #[must_use]
    pub fn with_gutter(mut self, on: bool) -> Self {
        self.inner.set_gutter(on);
        self
    }

    #[must_use]
    pub fn with_current_line_highlight(mut self, on: bool) -> Self {
        self.inner.set_current_line_highlight(on);
        self
    }

    #[must_use]
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.inner.set_font_size(size);
        self
    }

    /// Caret parked at the end; not counted as an edit.
    #[must_use]
    pub fn with_initial_value(mut self, s: &str) -> Self {
        self.inner.set_text(s);
        self
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.inner.set_placeholder(hint);
        self
    }

    /// A stable id for the e2e harness.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.inner = self.inner.with_id(id);
        self
    }

    /// Every edit splices into `obj` and [`sync`](Widget::sync) reads
    /// from it.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    /// For a host outside the taffy tree calling each frame instead of
    /// the `Widget::sync` pass. No-op until [`bind_text`](CodeEditor::bind_text).
    pub fn pull(&mut self, doc: &mut AutoCommit) {
        self.inner.sync(doc, &automerge::ROOT);
    }

    /// Stamps inserted runs with this author id (peer-attribution highlight).
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.inner.set_local_author(author);
    }

    /// Not counted as a user edit.
    pub fn set_text(&mut self, s: &str) {
        self.inner.set_text(s);
    }

    pub fn value(&self) -> &str {
        self.inner.value()
    }

    pub fn cursor_pos(&self) -> usize {
        self.inner.cursor_pos()
    }

    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.inner.set_cursor_pos(pos);
    }

    /// `[anchor, cursor)` in bytes; `anchor == cursor` clears it. Used
    /// by a host driving click+drag selection.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.inner.set_selection(anchor, cursor);
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        self.inner.selection_range_public()
    }

    pub fn goto_line(&mut self, line_1based: usize) {
        let target = line_1based.saturating_sub(1);
        let mut byte = 0usize;
        let mut line = 0usize;
        let text = self.inner.value();
        if line < target {
            for (i, b) in text.bytes().enumerate() {
                if b == b'\n' {
                    line = line.saturating_add(1);
                    if line == target {
                        byte = i.saturating_add(1);
                        break;
                    }
                }
            }
        }
        self.inner.set_cursor_pos(byte.min(text.len()));
    }

    pub fn select_all(&mut self) {
        self.inner.select_all();
    }

    pub fn take_dirty(&mut self) -> Option<String> {
        self.inner.take_dirty()
    }

    pub fn line_count(&self) -> usize {
        self.inner
            .value()
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            .saturating_add(1)
    }

    pub fn set_focused(&mut self, on: bool) {
        self.inner.set_focus(on);
    }

    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// For embedding outside the taffy tree (a monolithic immediate-mode
    /// host). Pairs with [`draw_embedded`].
    ///
    /// [`draw_embedded`]: CodeEditor::draw_embedded
    pub fn set_rect(&mut self, rect: Rect) {
        self.inner.apply_layout_external(rect);
    }

    pub fn rect(&self) -> Rect {
        self.inner.get_rect()
    }

    /// `dy` logical px (wheel), clamped to the content.
    pub fn scroll_by(&mut self, dy: f64) {
        self.inner.scroll_by(dy);
    }

    // ---- geometry for host overlays (diagnostics, markers) -----------------

    /// A host uses this to align a gutter marker with a source line
    /// (`caret_rect(theme, line_start)`).
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.inner.caret_rect(theme, byte)
    }

    /// One rect per visual row; a host draws a diagnostic squiggle along
    /// each rect's bottom edge.
    pub fn selection_rects(&self, theme: &Theme, start: usize, end: usize) -> Vec<Rect> {
        self.inner.selection_rects(theme, start, end)
    }

    pub fn line_height(&self) -> f64 {
        self.inner.body_line_height()
    }

    pub fn gutter_width(&self) -> f64 {
        self.inner.gutter_width()
    }

    /// Hover hit-test.
    pub fn byte_at_point(&self, point: Point) -> usize {
        self.inner.byte_at_point(point)
    }

    /// A monolithic host that owns its own layout calls [`set_rect`]
    /// then this each frame.
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
        self.inner
            .draw(scene, transform, text_renderer, theme, &presence);
    }

    pub fn inner(&self) -> &TextInput {
        &self.inner
    }

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
        // Registers *this* widget (not the inner input) so the host
        // drives CodeEditor's own passes.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        });
        let _ = theme;
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
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
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
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
        self.inner.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner
            .draw_overlay(scene, transform, text_renderer, theme, presence);
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
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }

    fn name(&self) -> &'static str {
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

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.inner)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Terminal)
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

fn find_from(hay: &str, from: usize, needle: char) -> Option<usize> {
    let rest = hay.get(from..)?;
    rest.find(needle).map(|i| from.saturating_add(i))
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
    pub fn new(
        keywords: &[&str],
        constants: &[&str],
        line_comment: Option<&str>,
        case_insensitive: bool,
    ) -> Self {
        let norm = |s: &str| {
            if case_insensitive {
                s.to_ascii_uppercase()
            } else {
                s.to_string()
            }
        };
        Self {
            keywords: keywords.iter().map(|s| norm(s)).collect(),
            constants: constants.iter().map(|s| norm(s)).collect(),
            line_comment: line_comment.map(std::string::ToString::to_string),
            case_insensitive,
        }
    }

    fn classify_word(&self, w: &str) -> Option<TokenKind> {
        let key = if self.case_insensitive {
            w.to_ascii_uppercase()
        } else {
            w.to_string()
        };
        if self.keywords.contains(&key) {
            Some(TokenKind::Keyword)
        } else if self.constants.contains(&key) {
            Some(TokenKind::Constant)
        } else if w.chars().next().is_some_and(char::is_uppercase) {
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
            if let Some(lc) = &self.line_comment
                && line.get(i..).is_some_and(|s| s.starts_with(lc.as_str()))
            {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            let Some(&b) = bytes.get(i) else { break };
            let c = b as char;
            if c == '"' || c == '\'' {
                let end = string_end(line, i, c);
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
                let Some(word) = line.get(i..end) else {
                    i = end;
                    continue;
                };
                let followed_by_paren = line
                    .get(end..)
                    .is_some_and(|s| s.trim_start().starts_with('('));
                if let Some(kind) = self.classify_word(word) {
                    spans.push(Span::new(i..end, kind));
                } else if followed_by_paren {
                    spans.push(Span::new(i..end, TokenKind::Function));
                }
                i = end;
                continue;
            }
            i = i.saturating_add(next_char_len(line, i));
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
    pub fn new() -> Self {
        const KW: &[&str] = &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
            "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
            "type", "unsafe", "use", "where", "while", "union",
        ];
        const CONST: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];
        Self {
            generic: GenericHighlighter::new(KW, CONST, Some("//"), false),
        }
    }
}

impl RustHighlighter {
    /// `spans` when the carried state says we're still inside a `/* … */`
    /// block comment: look for the close marker and recurse on the
    /// remainder. Split out of `spans` to keep that under the line cap.
    fn spans_in_block_comment(&self, line: &str) -> LineSpans {
        let n = line.len();
        if let Some(close) = line.find("*/") {
            let end = close.saturating_add(2);
            let rest = line.get(end..).unwrap_or_default();
            let mut ls = self.spans(rest, 0);
            shift_spans(&mut ls.spans, end);
            let mut spans = vec![Span::new(0..end, TokenKind::Comment)];
            spans.append(&mut ls.spans);
            return LineSpans {
                spans,
                carry: ls.carry,
            };
        }
        LineSpans {
            spans: vec![Span::new(0..n, TokenKind::Comment)],
            carry: IN_BLOCK_COMMENT,
        }
    }

    /// `spans` when not already inside a block comment: the main
    /// character-by-character tokenizer. Split out of `spans` to keep that
    /// under the line cap.
    fn tokenize_rust_line(&self, line: &str) -> LineSpans {
        let n = line.len();
        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut carry_out = 0;
        while i < n {
            if line.get(i..).is_some_and(|s| s.starts_with("//")) {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            if line.get(i..).is_some_and(|s| s.starts_with("/*")) {
                let after_open = i.saturating_add(2);
                if let Some(rel) = line.get(after_open..).and_then(|s| s.find("*/")) {
                    let end = after_open.saturating_add(rel).saturating_add(2);
                    spans.push(Span::new(i..end, TokenKind::Comment));
                    i = end;
                    continue;
                }
                spans.push(Span::new(i..n, TokenKind::Comment));
                carry_out = IN_BLOCK_COMMENT;
                break;
            }
            let Some(&b) = bytes.get(i) else { break };
            let c = b as char;
            if c == '#'
                && line
                    .get(i..)
                    .is_some_and(|s| s.starts_with("#[") || s.starts_with("#!["))
            {
                let end = find_from(line, i, ']').map_or(n, |e| e.saturating_add(1));
                spans.push(Span::new(i..end, TokenKind::Attribute));
                i = end;
                continue;
            }
            if c == '"' {
                let end = string_end(line, i, '"');
                spans.push(Span::new(i..end, TokenKind::StringLit));
                i = end;
                continue;
            }
            // `'a'` / `'\n'` is a char literal; `'a` alone is a lifetime,
            // coloured as an attribute-ish type marker → Keyword.
            if c == '\'' {
                if let Some(end) = char_literal_end(line, i) {
                    spans.push(Span::new(i..end, TokenKind::CharLit));
                    i = end;
                    continue;
                }
                let end = ident_end(line, i.saturating_add(1));
                if end > i.saturating_add(1) {
                    spans.push(Span::new(i..end, TokenKind::Keyword));
                    i = end;
                    continue;
                }
            }
            if c.is_ascii_digit() {
                let end = number_end(line, i);
                spans.push(Span::new(i..end, TokenKind::Number));
                i = end;
                continue;
            }
            if is_ident_start(c) {
                let end = ident_end(line, i);
                let Some(word) = line.get(i..end) else {
                    i = end;
                    continue;
                };
                let after = line.get(end..).unwrap_or_default();
                let is_macro = after.starts_with('!');
                let followed_by_paren = after.trim_start().starts_with('(');
                if let Some(kind) = self.generic.classify_word(word) {
                    spans.push(Span::new(i..end, kind));
                } else if is_macro {
                    // Include the `!` in the function span.
                    let end_incl = end.saturating_add(1);
                    spans.push(Span::new(i..end_incl, TokenKind::Function));
                    i = end_incl;
                    continue;
                } else if followed_by_paren {
                    spans.push(Span::new(i..end, TokenKind::Function));
                }
                i = end;
                continue;
            }
            i = i.saturating_add(next_char_len(line, i));
        }
        LineSpans {
            spans,
            carry: carry_out,
        }
    }
}

impl Default for RustHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for RustHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        if carry & IN_BLOCK_COMMENT != 0 {
            return self.spans_in_block_comment(line);
        }
        self.tokenize_rust_line(line)
    }
}

/// A pragmatic SQL highlighter: case-insensitive keywords, `'…'` strings,
/// `--` line comments, `/* … */` block comments (carried), and numbers.
pub struct SqlHighlighter {
    generic: GenericHighlighter,
}

impl SqlHighlighter {
    pub fn new() -> Self {
        const KW: &[&str] = &[
            "SELECT",
            "FROM",
            "WHERE",
            "INSERT",
            "INTO",
            "VALUES",
            "UPDATE",
            "SET",
            "DELETE",
            "CREATE",
            "TABLE",
            "DROP",
            "ALTER",
            "ADD",
            "COLUMN",
            "INDEX",
            "VIEW",
            "JOIN",
            "INNER",
            "LEFT",
            "RIGHT",
            "OUTER",
            "FULL",
            "ON",
            "GROUP",
            "BY",
            "ORDER",
            "HAVING",
            "LIMIT",
            "OFFSET",
            "AS",
            "AND",
            "OR",
            "NOT",
            "IN",
            "LIKE",
            "BETWEEN",
            "IS",
            "DISTINCT",
            "UNION",
            "ALL",
            "PRIMARY",
            "KEY",
            "FOREIGN",
            "REFERENCES",
            "DEFAULT",
            "CONSTRAINT",
            "UNIQUE",
            "CASE",
            "WHEN",
            "THEN",
            "ELSE",
            "END",
            "COUNT",
            "SUM",
            "AVG",
            "MIN",
            "MAX",
            "WITH",
        ];
        const CONST: &[&str] = &["NULL", "TRUE", "FALSE"];
        Self {
            generic: GenericHighlighter::new(KW, CONST, None, true),
        }
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
                let end = close.saturating_add(2);
                let rest = line.get(end..).unwrap_or_default();
                let mut ls = self.spans(rest, 0);
                shift_spans(&mut ls.spans, end);
                let mut spans = vec![Span::new(0..end, TokenKind::Comment)];
                spans.append(&mut ls.spans);
                return LineSpans {
                    spans,
                    carry: ls.carry,
                };
            }
            return LineSpans {
                spans: vec![Span::new(0..n, TokenKind::Comment)],
                carry: IN_BLOCK_COMMENT,
            };
        }
        let mut spans = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut carry_out = 0;
        while i < n {
            if line.get(i..).is_some_and(|s| s.starts_with("--")) {
                spans.push(Span::new(i..n, TokenKind::Comment));
                break;
            }
            if line.get(i..).is_some_and(|s| s.starts_with("/*")) {
                let after_open = i.saturating_add(2);
                if let Some(rel) = line.get(after_open..).and_then(|s| s.find("*/")) {
                    let end = after_open.saturating_add(rel).saturating_add(2);
                    spans.push(Span::new(i..end, TokenKind::Comment));
                    i = end;
                    continue;
                }
                spans.push(Span::new(i..n, TokenKind::Comment));
                carry_out = IN_BLOCK_COMMENT;
                break;
            }
            let Some(&b) = bytes.get(i) else { break };
            let c = b as char;
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
                let Some(word) = line.get(i..end) else {
                    i = end;
                    continue;
                };
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
            i = i.saturating_add(next_char_len(line, i));
        }
        LineSpans {
            spans,
            carry: carry_out,
        }
    }
}

/// Bit in [`HlState`] marking "the next line is inside a ```` ``` ```` fenced
/// code block" (Typst).
const IN_FENCE: HlState = 1;

/// A pragmatic Typst highlighter — a per-line lexer recognising headings,
/// comments, strings, `#`-functions, numbers, raw code (inline and fenced),
/// maths, `*strong*` and `_emph_` runs. Fenced code blocks are carried
/// across lines.
pub struct TypstHighlighter;

impl Highlighter for TypstHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let len = line.len();
        if let Some(early) = typst_fence_or_heading_spans(line, len, carry) {
            return early;
        }
        tokenize_typst_line(line, len)
    }
}

/// [`TypstHighlighter::spans`]'s fast paths: fence delimiter, inside a
/// fence, heading. `None` means "none of these apply, fall through to the
/// character tokenizer." Split out to keep that under the line cap.
fn typst_fence_or_heading_spans(line: &str, len: usize, carry: HlState) -> Option<LineSpans> {
    let trimmed = line.trim_start();
    // Fence delimiter line toggles the fenced state; it renders as raw.
    if trimmed.starts_with("```") {
        let next = if carry & IN_FENCE != 0 { 0 } else { IN_FENCE };
        return Some(LineSpans {
            spans: vec![Span::new(0..len, TokenKind::Raw)],
            carry: next,
        });
    }
    if carry & IN_FENCE != 0 {
        return Some(LineSpans {
            spans: vec![Span::new(0..len, TokenKind::Raw)],
            carry: IN_FENCE,
        });
    }
    if trimmed.starts_with('=') {
        return Some(LineSpans {
            spans: vec![Span::new(0..len, TokenKind::Heading)],
            carry: 0,
        });
    }
    None
}

/// [`TypstHighlighter::spans`]'s character-by-character tokenizer, once the
/// fast paths in [`typst_fence_or_heading_spans`] don't apply. Split out to
/// keep that under the line cap.
fn tokenize_typst_line(line: &str, len: usize) -> LineSpans {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < len {
        let Some(&byte) = bytes.get(i) else { break };
        let c = byte as char;
        if line.get(i..).is_some_and(|s| s.starts_with("//")) {
            spans.push(Span::new(i..len, TokenKind::Comment));
            break;
        }
        if c == '"' {
            let end =
                find_from(line, i.saturating_add(1), '"').map_or(len, |e| e.saturating_add(1));
            spans.push(Span::new(i..end, TokenKind::StringLit));
            i = end;
            continue;
        }
        if c == '`' {
            let end =
                find_from(line, i.saturating_add(1), '`').map_or(len, |e| e.saturating_add(1));
            spans.push(Span::new(i..end, TokenKind::Raw));
            i = end;
            continue;
        }
        if c == '$' {
            let end =
                find_from(line, i.saturating_add(1), '$').map_or(len, |e| e.saturating_add(1));
            spans.push(Span::new(i..end, TokenKind::Math));
            i = end;
            continue;
        }
        if c == '#' {
            let mut pos = i.saturating_add(1);
            while let Some(&next_byte) = bytes.get(pos) {
                let next_char = next_byte as char;
                if next_char.is_alphanumeric() || next_char == '_' || next_char == '.' {
                    pos = pos.saturating_add(1);
                } else {
                    break;
                }
            }
            let end = pos.max(i.saturating_add(1));
            spans.push(Span::new(i..end, TokenKind::Function));
            i = end;
            continue;
        }
        if c == '*'
            && let Some(close) = find_from(line, i.saturating_add(1), '*')
            && close > i.saturating_add(1)
        {
            spans.push(Span::new(i..close.saturating_add(1), TokenKind::Strong));
            i = close.saturating_add(1);
            continue;
        }
        if c == '_'
            && let Some(close) = find_from(line, i.saturating_add(1), '_')
            && close > i.saturating_add(1)
        {
            spans.push(Span::new(i..close.saturating_add(1), TokenKind::Emphasis));
            i = close.saturating_add(1);
            continue;
        }
        // Number with optional unit suffix (12pt, 1.5cm, 80%).
        if c.is_ascii_digit() {
            let mut pos = i;
            while bytes
                .get(pos)
                .is_some_and(|&next_byte| next_byte.is_ascii_digit() || next_byte == b'.')
            {
                pos = pos.saturating_add(1);
            }
            while bytes.get(pos).is_some_and(|&next_byte| {
                (next_byte as char).is_ascii_alphabetic() || next_byte == b'%'
            }) {
                pos = pos.saturating_add(1);
            }
            spans.push(Span::new(i..pos, TokenKind::Number));
            i = pos;
            continue;
        }
        i = i.saturating_add(next_char_len(line, i));
    }
    LineSpans { spans, carry: 0 }
}

// ---------------------------------------------------------------------------
// Lexing helpers (byte-offset based, char-boundary safe).
// ---------------------------------------------------------------------------

fn next_char_len(s: &str, i: usize) -> usize {
    s.get(i..)
        .and_then(|rest| rest.chars().next())
        .map_or(1, char::len_utf8)
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn ident_end(s: &str, i: usize) -> usize {
    let mut j = i;
    if let Some(rest) = s.get(i..) {
        for c in rest.chars() {
            if is_ident_continue(c) {
                j = j.saturating_add(c.len_utf8());
            } else {
                break;
            }
        }
    }
    j
}

/// End (exclusive) of a numeric literal starting at `i` — digits, a decimal
/// point, hex/underscore digits, and a trailing type/unit suffix.
fn number_end(s: &str, i: usize) -> usize {
    let mut j = i;
    if let Some(rest) = s.get(i..) {
        for c in rest.chars() {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                j = j.saturating_add(c.len_utf8());
            } else {
                break;
            }
        }
    }
    j
}

/// End (exclusive) of a string literal that opened with `quote` at `i`,
/// honouring `\`-escapes. Runs to end-of-line if unterminated.
fn string_end(s: &str, i: usize, quote: char) -> usize {
    let mut j = i.saturating_add(quote.len_utf8());
    let mut escaped = false;
    if let Some(rest) = s.get(j..) {
        for c in rest.chars() {
            j = j.saturating_add(c.len_utf8());
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                break;
            }
        }
    }
    j.min(s.len())
}

/// If `i` opens a Rust char literal (`'x'`, `'\n'`, `'\u{1F}'`), return its end
/// (exclusive); otherwise `None` (so the caller can treat `'` as a lifetime).
fn char_literal_end(s: &str, i: usize) -> Option<usize> {
    let rest = s.get(i.saturating_add(1)..)?;
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        // Escape: consume until the closing quote.
        for (off, c) in chars {
            if c == '\'' {
                return Some(i.saturating_add(1).saturating_add(off).saturating_add(1));
            }
        }
        None
    } else {
        // A single char then a closing quote.
        let after = i.saturating_add(1).saturating_add(first.len_utf8());
        if s.get(after..).is_some_and(|s| s.starts_with('\'')) {
            Some(after.saturating_add(1))
        } else {
            None
        }
    }
}

/// Used when a leading block-comment prefix is prepended to a line's
/// remaining spans.
fn shift_spans(spans: &mut [Span], by: usize) {
    for sp in spans.iter_mut() {
        sp.range.start = sp.range.start.saturating_add(by);
        sp.range.end = sp.range.end.saturating_add(by);
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
        assert!(
            kinds.contains(&TokenKind::Keyword),
            "`fn` is a keyword: {kinds:?}"
        );
        assert!(
            kinds.contains(&TokenKind::Function),
            "`main` before `(` is a function: {kinds:?}"
        );
    }

    #[test]
    fn rust_span_ranges_map_to_the_right_text() {
        let hl = RustHighlighter::new();
        let line = "let x = 42;";
        let ls = hl.spans(line, 0);
        let kw = ls
            .spans
            .iter()
            .find(|s| s.kind == TokenKind::Keyword)
            .unwrap();
        assert_eq!(line.get(kw.range.clone()).expect("span in bounds"), "let");
        let num = ls
            .spans
            .iter()
            .find(|s| s.kind == TokenKind::Number)
            .unwrap();
        assert_eq!(line.get(num.range.clone()).expect("span in bounds"), "42");
    }

    #[test]
    fn rust_string_literal_span() {
        let hl = RustHighlighter::new();
        let line = r#"let s = "hi there";"#;
        let ls = hl.spans(line, 0);
        let str_span = ls
            .spans
            .iter()
            .find(|s| s.kind == TokenKind::StringLit)
            .unwrap();
        assert_eq!(
            line.get(str_span.range.clone()).expect("span in bounds"),
            r#""hi there""#
        );
    }

    #[test]
    fn rust_block_comment_carries_across_lines() {
        let hl = RustHighlighter::new();
        let l0 = hl.spans("code(); /* open", 0);
        assert_eq!(
            l0.carry & IN_BLOCK_COMMENT,
            IN_BLOCK_COMMENT,
            "unterminated /* carries"
        );
        let l1 = hl.spans("still comment", l0.carry);
        assert!(l1.spans.iter().all(|s| s.kind == TokenKind::Comment));
        assert_eq!(l1.carry & IN_BLOCK_COMMENT, IN_BLOCK_COMMENT);
        let l2 = hl.spans("*/ done();", l1.carry);
        assert_eq!(
            l2.carry & IN_BLOCK_COMMENT,
            0,
            "closing */ clears the carry"
        );
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
            .map(|s| {
                "select * from users where id = 1"
                    .get(s.range.clone())
                    .expect("span in bounds")
                    .to_string()
            })
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
        let dark = token_color(TokenKind::Keyword, &Theme::dark());
        let light = token_color(TokenKind::Keyword, &Theme::light());
        let rgb = |c: Color| [c.r(), c.g(), c.b()];
        assert_ne!(rgb(dark), rgb(light), "keyword colour must track the theme");
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
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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

    fn drive(ed: &mut CodeEditor, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        ed.handle_event(
            ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
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
            assert!(drive(
                &mut ed,
                &mut doc,
                &char_event(ch),
                mods(false, false)
            ));
        }
        assert_eq!(ed.value(), "fn x");
        assert_eq!(ed.cursor_pos(), 4);
    }

    #[test]
    fn wrapper_forwards_the_shaped_ime_candidate_rect() {
        let mut ed = CodeEditor::local()
            .with_language(Language::Rust)
            .with_initial_value("let value = 1;");
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ed,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "漢".into(),
                Some(("漢".len(), "漢".len())),
            )),
            mods(false, false),
        ));
        draw_once(&mut ed, &Theme::dark());

        let wrapper_rect = Widget::ime_cursor_rect(&ed).expect("wrapper publishes IME geometry");
        let inner_rect = Widget::ime_cursor_rect(ed.inner()).expect("inner input has a caret");
        assert_eq!(wrapper_rect, inner_rect);
        assert!(wrapper_rect.x0 > ed.gutter_width());
    }

    #[test]
    fn enter_inserts_a_real_newline_and_gutter_counts_lines() {
        let mut ed = CodeEditor::local();
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        {
            let ch = "a";
            drive(&mut ed, &mut doc, &char_event(ch), mods(false, false));
        }
        drive(
            &mut ed,
            &mut doc,
            &named_event(NamedKey::Enter),
            mods(false, false),
        );
        drive(&mut ed, &mut doc, &char_event("b"), mods(false, false));
        assert_eq!(ed.value(), "a\nb");
        assert_eq!(
            ed.line_count(),
            2,
            "gutter line count reflects the two logical lines"
        );
    }

    #[test]
    fn tab_inserts_soft_spaces_and_shift_tab_outdents() {
        let mut ed = CodeEditor::local().with_tab_width(4);
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ed,
            &mut doc,
            &named_event(NamedKey::Tab),
            mods(false, false)
        ));
        drive(&mut ed, &mut doc, &char_event("x"), mods(false, false));
        assert_eq!(ed.value(), "    x", "Tab inserts four spaces");
        // Shift+Tab removes one indent level.
        assert!(drive(
            &mut ed,
            &mut doc,
            &named_event(NamedKey::Tab),
            mods(false, true)
        ));
        assert_eq!(
            ed.value(),
            "x",
            "Shift+Tab outdents the leading four spaces"
        );
    }

    #[test]
    fn selection_and_cut_run_through_the_real_path() {
        let mut ed = CodeEditor::local().with_initial_value("abc");
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ed,
            &mut doc,
            &char_event("a"),
            mods(true, false)
        ));
        assert_eq!(
            ed.inner().selection_range_public(),
            Some((0, 3)),
            "Ctrl+A selects the whole buffer on the real path",
        );
        assert!(drive(
            &mut ed,
            &mut doc,
            &char_event("x"),
            mods(true, false)
        ));
        assert_eq!(
            ed.value(),
            "",
            "Ctrl+X cuts the selection through the shared core"
        );
    }

    #[test]
    fn shift_arrow_extends_selection_word_wise() {
        let mut ed = CodeEditor::local().with_initial_value("hello world");
        ed.set_focused(true);
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut doc = AutoCommit::new();
        drive(
            &mut ed,
            &mut doc,
            &named_event(NamedKey::Home),
            mods(false, false),
        );
        drive(
            &mut ed,
            &mut doc,
            &named_event(NamedKey::ArrowRight),
            mods(true, true),
        );
        let sel = ed
            .inner()
            .selection_range_public()
            .expect("a selection exists");
        assert_eq!(sel.0, 0);
        assert!(
            sel.1 >= 5,
            "word-wise extend covers at least `hello`: {sel:?}"
        );
    }

    #[test]
    fn goto_line_moves_the_caret_to_the_line_start() {
        let mut ed = CodeEditor::local().with_initial_value("one\ntwo\nthree");
        ed.goto_line(2);
        assert_eq!(
            ed.cursor_pos(),
            4,
            "line 2 starts just after the first newline"
        );
        let start = ed.cursor_pos();
        let end = start.saturating_add(3);
        assert_eq!(
            ed.value().get(start..end).expect("byte range is in bounds"),
            "two"
        );
    }

    #[test]
    fn accessibility_reports_a_multiline_text_input() {
        use crate::a11y::Role;
        let ed = CodeEditor::local().with_initial_value("fn main() {}");
        let node = ed
            .accessibility()
            .expect("code editor exposes an a11y node");
        assert_eq!(node.role, Role::MultilineTextInput);
        assert!(
            node.state.multiline,
            "code editor reports a multiline text field"
        );
        assert_eq!(node.value.as_deref(), Some("fn main() {}"));
    }

    #[test]
    fn highlighter_colours_segments_from_the_theme() {
        use crate::color_compat::ColorCompat;
        let theme = Theme::dark();
        let mut ed = CodeEditor::local().with_language(Language::Rust);
        ed.set_text("let x = 1");
        ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        // Draw builds the visual lines + highlight cache and runs the
        // real colouring path (`row_segments` → `token_color`).
        draw_once(&mut ed, &theme);
        let segs = ed.inner().code_row_colors(&theme);
        let rgb = |c: Color| [c.r(), c.g(), c.b()];
        let kw = segs
            .iter()
            .find(|(t, _)| t == "let")
            .expect("`let` is a drawn segment");
        assert_eq!(
            rgb(kw.1),
            rgb(token_color(TokenKind::Keyword, &theme)),
            "the `let` keyword is drawn in the themed keyword colour",
        );
        assert!(
            segs.iter().any(|(_, c)| rgb(*c) == rgb(theme.text_primary)),
            "uncoloured runs fall back to the theme text colour",
        );
    }

    #[test]
    fn no_gutter_offset_when_gutter_disabled() {
        // Proves the gutter participates in the shared caret geometry:
        // turning it off collapses the offset it pushes the caret by.
        let theme = Theme::dark();
        let mut with_g = CodeEditor::local().with_initial_value("x");
        with_g.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        draw_once(&mut with_g, &theme);
        let caret_with = with_g.inner().caret_rect(&theme, 0).x0;

        let mut without_g = CodeEditor::local()
            .with_gutter(false)
            .with_initial_value("x");
        without_g.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        draw_once(&mut without_g, &theme);
        let caret_without = without_g.inner().caret_rect(&theme, 0).x0;

        assert!(
            caret_with > caret_without + 10.0,
            "the gutter shifts the caret right ({caret_with} vs {caret_without})",
        );
    }
}
