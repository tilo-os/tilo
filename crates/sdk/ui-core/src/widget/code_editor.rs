//! The shared multi-line code-editing widget: a retained [`Widget`] that
//! puts real code editing — caret, selection (shift/drag, word-wise),
//! clipboard, undo/redo, IME, and a multiline a11y node — on the *same*
//! [`TextInput`] core every other text field
//! uses. On top of that it adds a **gutter** with line numbers and a
//! current-line highlight, **syntax highlighting** through a pluggable
//! [`Highlighter`] (the widget owns rendering/caret/selection, the app
//! owns the language tokenizer, and every colour comes from the OS
//! [`Theme`]), and **soft-tab / indent** handling.
//!
//! ```no_run
//! use tilo_ui::widget::code_editor::{CodeEditor, Language};
//! let mut editor = CodeEditor::local()
//!     .with_language(Language::Rust)
//!     .with_tab_width(4);
//! editor.set_text("fn main() {\n    println!(\"hi\");\n}\n");
//! ```

use crate::widget::{AppModel, ModelPath};
use std::ops::Range;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::{PathSegment, PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, TextInputKind, Widget};

pub mod syntax;

pub use syntax::{SyntaxHighlighter, SyntaxState};

/// The syntactic category a run of source falls into. The editor never
/// hard-codes a colour for these — the theme's
/// [`SyntaxPalette`](crate::tokens::SyntaxPalette) resolves each to a
/// concrete colour, so syntax tracks the active theme in light, dark and high
/// contrast.
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
    /// `#[derive(..)]`, an HTML attribute name.
    Attribute,
    /// A markup tag name (`div` in `<div>`).
    Tag,
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

impl LineSpans {}

/// A pluggable, per-line source tokenizer. The [`CodeEditor`] owns rendering,
/// caret, and selection; a `Highlighter` owns *only* the language lexing —
/// mapping a line of source to coloured [`Span`]s. [`SyntaxHighlighter`]
/// covers every language with a grammar; hand it (or a wrapper that adds
/// app-specific spans) to [`CodeEditor::with_highlighter`].
pub trait Highlighter {
    /// `line` never contains `\n`. `carry` is the [`HlState`] the previous
    /// line left (`0` for the first line); the returned [`LineSpans::carry`]
    /// is what the following line begins with. Byte ranges in the returned
    /// spans are relative to `line`.
    fn spans(&self, line: &str, carry: HlState) -> LineSpans;
}

/// A built-in language the editor ships a [`Highlighter`] for. Any other
/// language with a grammar is a [`SyntaxHighlighter::for_token`] away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    PlainText,
    Rust,
    Typst,
    Sql,
    Json,
}

impl Language {
    /// `None` for [`Language::PlainText`].
    pub fn highlighter(self) -> Option<Box<dyn Highlighter>> {
        match self {
            Language::PlainText => None,
            Language::Rust => Some(Box::new(SyntaxHighlighter::for_token_or_plain("rs"))),
            // No Typst grammar ships in the syntax set; Typst keeps its lexer.
            Language::Typst => Some(Box::new(TypstHighlighter)),
            Language::Sql => Some(Box::new(SyntaxHighlighter::for_token_or_plain("sql"))),
            Language::Json => Some(Box::new(SyntaxHighlighter::for_token_or_plain("json"))),
        }
    }
}

/// How much a [`Diagnostic`] matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// A note or a hint.
    Info,
    Warning,
    Error,
}

/// A problem a checker found in the source: a byte `range`, how much it
/// matters, and what it says. The editor draws it — a squiggle under the
/// range, a mark in the gutter, the message on hover — in the theme's
/// status colours; the app only says what is wrong where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn new(range: Range<usize>, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            range,
            severity,
            message: message.into(),
        }
    }

    /// The status colour of `severity` in `theme`.
    fn color(&self, theme: &Theme) -> vello::peniko::Color {
        let semantic = &theme.tokens.colors.semantic;
        match self.severity {
            Severity::Error => semantic.danger,
            Severity::Warning => semantic.warning,
            Severity::Info => semantic.info,
        }
    }
}

/// Wraps the multiline [`TextInput`] core and enables its gutter +
/// syntax-highlight path, so all caret/selection/clipboard/undo/IME/a11y
/// behaviour is the one shared implementation.
pub struct CodeEditor {
    inner: TextInput,
    /// Selectable and copyable, never edited.
    read_only: bool,
    /// The fewest and most lines the editor is tall for; past the most it
    /// scrolls.
    min_lines: Option<usize>,
    max_lines: Option<usize>,
    diagnostics: Vec<Diagnostic>,
    /// The pointer, for the diagnostic under it.
    hover: Option<Point>,
}

impl CodeEditor {
    /// CRDT-bound at `path` (mirrors [`TextInput::new`]). Bind the backing
    /// Automerge `Text` object with [`CodeEditor::bind_text`].
    pub fn new(path: Vec<PathSegment>) -> Self {
        Self::wrap(Self::configure(TextInput::new(path)))
    }

    fn wrap(inner: TextInput) -> Self {
        Self {
            inner,
            read_only: false,
            min_lines: None,
            max_lines: None,
            diagnostics: Vec::new(),
            hover: None,
        }
    }

    /// Non-CRDT, owns its own string — read with [`CodeEditor::value`],
    /// observe edits with [`CodeEditor::take_dirty`].
    pub fn local() -> Self {
        Self::wrap(Self::configure(TextInput::local()))
    }

    /// Source a person reads, selects and copies but does not edit (a
    /// diff, a generated file, a log). Keys that would change the text do
    /// nothing; assistive technology hears it as read-only.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// [`Self::read_only`] after construction.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// What the editor is called to assistive technology ("Query",
    /// "Response body").
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.inner = self.inner.with_accessible_name(name);
        self
    }

    /// At least `lines` lines tall, however short the source.
    #[must_use]
    pub fn with_min_lines(mut self, lines: usize) -> Self {
        self.min_lines = Some(lines.max(1));
        self
    }

    /// At most `lines` lines tall; longer source scrolls inside.
    #[must_use]
    pub fn with_max_lines(mut self, lines: usize) -> Self {
        self.max_lines = Some(lines.max(1));
        self
    }

    /// Show `diagnostics` (replacing any shown before).
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.set_diagnostics(diagnostics);
        self
    }

    /// Replace the diagnostics shown (a checker ran again).
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics = diagnostics;
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The diagnostic under `point`, if any: the most severe one whose
    /// range covers the byte there.
    pub fn diagnostic_at(&self, point: Point) -> Option<&Diagnostic> {
        if !self.inner.get_rect().contains(point) {
            return None;
        }
        let byte = self.inner.byte_at_point(point);
        self.diagnostics
            .iter()
            .filter(|d| d.range.start <= byte && byte < d.range.end.max(d.range.start + 1))
            .max_by_key(|d| d.severity)
    }

    /// Multiline body, monospace face, plain chrome, a line-number gutter,
    /// current-line highlight, and 4-space soft tabs by default.
    fn configure(input: TextInput) -> TextInput {
        let mut input = input;
        input.keep_document_inset();
        input
            .multiline(true)
            .with_kind(TextInputKind::Code)
            .with_mono()
            .with_text_role(crate::tokens::TextRole::Code)
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

    /// Show the text object `obj`, read on every sync. Edits stay local
    /// for the owner to take ([`Self::take_dirty`]) and submit as actions.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    /// Detach the bound text, keeping what is shown.
    pub fn clear_text_binding(&mut self) {
        self.inner.clear_text_binding();
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
        self.draw_diagnostics(scene, transform, theme);
    }

    /// For an embedded host that routes the pointer itself: where it is
    /// (window coordinates), or `None` when it left. The diagnostic under it
    /// shows its message in [`Self::draw_diagnostic_card`].
    pub fn set_pointer(&mut self, point: Option<Point>) {
        self.hover = point;
    }

    /// For an embedded host: paint the message of the diagnostic under the
    /// pointer from its own top-layer pass, kept inside `window`.
    pub fn draw_diagnostic_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let presence = PresenceCtx::default();
        let previous = presence.push_viewport(window);
        self.draw_diagnostic_hover(scene, transform, text_renderer, theme, &presence);
        presence.pop_viewport(previous);
    }

    pub fn inner(&self) -> &TextInput {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }
}

impl Widget for CodeEditor {
    crate::provides!(
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.capture_layout_text_scale(theme.text_scale);
        // Registers *this* widget (not the inner input) so the host
        // drives CodeEditor's own passes.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let mut size = self.inner.measure(known, available, text_renderer);
        if known.height.is_none() && (self.min_lines.is_some() || self.max_lines.is_some()) {
            let line = self.inner.body_line_height();
            if line > 0.0 {
                // The field's own padding around its lines stays as measured:
                // what is left of its content height after whole rows.
                let total = self.inner.content_height();
                let rows = (total / line).floor().max(1.0);
                let chrome = (total - rows * line).max(0.0);
                let lines = |n: usize| {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "a line count is far below f64's integer range"
                    )]
                    let n = n as f64;
                    n * line + chrome
                };
                let mut height = f64::from(size.height);
                if let Some(min) = self.min_lines {
                    height = height.max(lines(min));
                }
                if let Some(max) = self.max_lines {
                    height = height.min(lines(max));
                }
                size.height = crate::num::f32_of_f64(height.ceil());
            }
        }
        size
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
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
        self.draw_diagnostics(scene, transform, theme);
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
        self.draw_diagnostic_hover(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.hover = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if let WindowEvent::CursorLeft { .. } = event {
            self.hover = None;
        }
        if self.read_only && edits_text(event, *ctx.modifiers) {
            // Read-only: the key is answered (nothing else acts on it) and
            // the text stays as it is.
            if self.inner.is_focused() {
                *ctx.handled = true;
            }
            return;
        }
        self.inner.handle_event(event, ctx);
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

    fn wants_tab_input(&self) -> bool {
        // A read-only editor indents nothing: Tab moves focus on.
        !self.read_only && self.inner.wants_tab_input()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.inner)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.read_only || !self.inner.is_focused() {
            return None;
        }
        // Source text uses the shared input contract; no bytes go to a process.
        self.inner.ime_purpose()
    }

    fn ime_multiline(&self) -> bool {
        self.ime_purpose().is_some() && self.inner.ime_multiline()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_purpose()?;
        self.inner.ime_edit_fingerprint(key)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = self.inner.accessibility()?;
        node.state.read_only |= self.read_only;
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.read_only && matches!(action, crate::a11y::Action::SetValue(_)) {
            return false;
        }
        self.inner.perform_action(action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.inner.text_review()
    }
}

impl CodeEditor {
    /// The squiggles under every diagnostic's range and the marks in the
    /// gutter beside its first line, in the theme's status colours.
    fn draw_diagnostics(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.diagnostics.is_empty() {
            return;
        }
        let rect = self.inner.get_rect();
        let gutter = self.inner.gutter_width();
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &rect,
        );
        // Least severe first, so an error's squiggle and mark win.
        let mut order: Vec<&Diagnostic> = self.diagnostics.iter().collect();
        order.sort_by_key(|d| d.severity);
        for d in order {
            let color = d.color(theme);
            let end = d
                .range
                .end
                .max(d.range.start + 1)
                .min(self.inner.value().len());
            let start = d.range.start.min(end);
            for row in self.inner.selection_rects(theme, start, end) {
                let x0 = row.x0.max(rect.x0 + gutter);
                let x1 = row.x1.max(x0 + 4.0).min(rect.x1);
                paint_diagnostic_underline(scene, transform, x0, x1, row.y1 - 1.0, color);
            }
            if gutter > 0.0 {
                let caret = self.inner.caret_rect(theme, start);
                let d_px = f64::from(theme.tokens.spacing.s2) * 0.75;
                let centre = Point::new(
                    rect.x0 + f64::from(theme.tokens.spacing.s1) + d_px / 2.0,
                    caret.center().y,
                );
                if rect.contains(centre) {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        color,
                        None,
                        &vello::kurbo::Circle::new(centre, d_px / 2.0),
                    );
                }
            }
        }
        scene.pop_layer();
    }

    /// The message of the diagnostic under the pointer, on a floating
    /// surface just below its line, in the window's top layer.
    fn draw_diagnostic_hover(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(point) = self.hover else { return };
        let Some(d) = self.diagnostic_at(point) else {
            return;
        };
        let style = theme.text(crate::tokens::TextRole::Caption);
        let pad = f64::from(theme.tokens.spacing.s3);
        let max_w = f64::from(theme.tokens.metrics.panel_width_utility);
        let (w, h) = text_renderer.measure_style(
            &d.message,
            Some(crate::num::f32_of_f64(max_w - pad * 2.0)),
            &style,
        );
        let size = (f64::from(w) + pad * 2.0, f64::from(h) + pad * 2.0);
        let line = self.inner.caret_rect(theme, d.range.start);
        let anchor = Rect::new(point.x, line.y0, point.x, line.y1);
        let window = presence
            .viewport
            .get()
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        let (panel, _) = crate::widget::floating_surface::place_anchored(
            anchor,
            size,
            crate::widget::PopoverSide::Below,
            f64::from(theme.tokens.spacing.s1),
            crate::widget::floating_surface::panel_room(window, theme),
        );
        crate::widget::floating_surface::paint(scene, transform, panel, theme);
        let ink = theme.on_ground(crate::chrome::panel_ground(theme));
        text_renderer.draw_style(
            scene,
            &d.message,
            ink.text_primary,
            transform.pre_translate(vello::kurbo::Vec2::new(panel.x0 + pad, panel.y0 + pad)),
            Some(crate::num::f32_of_f64(panel.width() - pad * 2.0)),
            &style,
        );
    }
}

/// A wavy underline from `x0` to `x1` along `y`: the mark of a problem in
/// text, as spelling marks one.
pub fn paint_diagnostic_underline(
    scene: &mut Scene,
    transform: Affine,
    x0: f64,
    x1: f64,
    y: f64,
    color: vello::peniko::Color,
) {
    let mut path = vello::kurbo::BezPath::new();
    path.move_to((x0, y));
    // One zig per two pixels, counted in whole steps so the run ends
    // exactly at `x1`.
    let mut step = 1_u32;
    loop {
        let x = (x0 + 2.0 * f64::from(step)).min(x1);
        let up = step.is_multiple_of(2);
        path.line_to((x, y + if up { -1.0 } else { 1.0 }));
        // Not "less than": a NaN end stops the run too.
        if x.partial_cmp(&x1) != Some(std::cmp::Ordering::Less) {
            break;
        }
        step = step.saturating_add(1);
    }
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        color,
        None,
        &path,
    );
}

/// Whether `event` would change the text of a focused editor: typed text,
/// an editing key, the input method, or a cut, paste, undo or redo
/// shortcut. Moving the caret, selecting and copying do not.
fn edits_text(event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, NamedKey};
    match event {
        WindowEvent::Ime(winit::event::Ime::Commit(_) | winit::event::Ime::Preedit(..)) => true,
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
            let state = modifiers.state();
            let command = state.control_key() || state.super_key();
            match logical_key {
                Key::Named(
                    NamedKey::Enter
                    | NamedKey::Backspace
                    | NamedKey::Delete
                    | NamedKey::Tab
                    | NamedKey::Space
                    | NamedKey::Cut
                    | NamedKey::Paste
                    | NamedKey::Undo
                    | NamedKey::Redo,
                ) => true,
                Key::Character(c) if command => {
                    matches!(c.to_ascii_lowercase().as_str(), "x" | "v" | "z" | "y" | "d")
                }
                Key::Character(_) => text.is_some(),
                _ => false,
            }
        }
        _ => false,
    }
}

impl crate::undo::UndoPoints for CodeEditor {
    fn get_state(&self) -> Option<String> {
        self.inner.get_state()
    }
    fn set_state(&mut self, state: &str) {
        self.inner.set_state(state);
    }
}

impl crate::presence::capability::PublishesPresence for CodeEditor {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use vello::peniko::Color;

    #[test]
    fn highlighting_maps_languages_and_theme_colours() {
        fn typst_heading_and_function() {
            let hl = TypstHighlighter;
            assert_eq!(hl.spans("= Title", 0).spans[0].kind, TokenKind::Heading);
            let ls = hl.spans(r#"#set text("hi")"#, 0);
            let kinds: Vec<_> = ls.spans.iter().map(|s| s.kind).collect();
            assert!(kinds.contains(&TokenKind::Function));
            assert!(kinds.contains(&TokenKind::StringLit));
        }

        fn typst_fenced_block_carries() {
            let hl = TypstHighlighter;
            let open = hl.spans("```rust", 0);
            assert_eq!(open.carry & IN_FENCE, IN_FENCE);
            let mid = hl.spans("let x = 1;", open.carry);
            assert!(mid.spans.iter().all(|s| s.kind == TokenKind::Raw));
            let close = hl.spans("```", mid.carry);
            assert_eq!(close.carry & IN_FENCE, 0);
        }

        fn token_color_changes_with_theme() {
            use crate::color_compat::ColorCompat;
            let dark = Theme::dark().tokens.syntax.color(TokenKind::Keyword);
            let light = Theme::light().tokens.syntax.color(TokenKind::Keyword);
            let rgb = |c: Color| [c.r(), c.g(), c.b()];
            assert_ne!(rgb(dark), rgb(light), "keyword colour must track the theme");
            assert_eq!(
                rgb(Theme::dark().tokens.syntax.color(TokenKind::Comment)),
                rgb(Theme::dark().text_dim),
            );
        }

        fn language_maps_to_a_highlighter() {
            assert!(Language::Rust.highlighter().is_some());
            assert!(Language::Sql.highlighter().is_some());
            assert!(Language::Typst.highlighter().is_some());
            assert!(Language::PlainText.highlighter().is_none());
        }

        fn highlighter_colours_segments_from_the_theme() {
            use crate::color_compat::ColorCompat;
            let theme = Theme::dark();
            let mut ed = CodeEditor::local().with_language(Language::Rust);
            ed.set_text("let x = 1");
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            // Draw builds the visual lines + highlight cache and runs the
            // real colouring path (`row_segments` → the theme's syntax palette).
            draw_once(&mut ed, &theme);
            let segs = ed.inner().code_row_colors(&theme);
            let rgb = |c: Color| [c.r(), c.g(), c.b()];
            let kw = segs
                .iter()
                .find(|(t, _)| t == "let")
                .expect("`let` is a drawn segment");
            assert_eq!(
                rgb(kw.1),
                rgb(theme.tokens.syntax.color(TokenKind::Keyword)),
                "the `let` keyword is drawn in the themed keyword colour",
            );
            assert!(
                segs.iter().any(|(_, c)| rgb(*c) == rgb(theme.text_primary)),
                "uncoloured runs fall back to the theme text colour",
            );
        }

        typst_heading_and_function();
        typst_fenced_block_carries();
        token_color_changes_with_theme();
        language_maps_to_a_highlighter();
        highlighter_colours_segments_from_the_theme();
    }

    use crate::testing::Dispatch;
    use automerge::AutoCommit;
    use taffy::TaffyTree;
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::event::{ElementState, Modifiers, WindowEvent};
    use winit::keyboard::NamedKey;

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

    use crate::synthetic_input::char_key;

    use crate::synthetic_input::named_key;

    fn drive(ed: &mut CodeEditor, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) -> bool {
        Dispatch::new().doc(doc).modifiers(m).send(ed, ev).handled
    }

    fn draw_once(ed: &mut CodeEditor, theme: &Theme) {
        let presence = PresenceCtx::default();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(ed, &mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    fn lay_out_and_draw(
        ed: &mut CodeEditor,
        theme: &Theme,
        renderer: &mut TextRenderer,
        width: f32,
    ) -> f32 {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let node = Widget::build_layout(ed, &mut taffy, theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                |known, available, id, _, _| {
                    node_map
                        .get(&id)
                        .map_or(taffy::Size::ZERO, |&widget| unsafe {
                            (*widget).measure(known, available, renderer)
                        })
                },
            )
            .expect("code editor layout computes");
        let height = taffy.layout(node).expect("code editor layout").size.height;
        Widget::apply_layout(ed, &taffy, node, Point::ZERO);
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        Widget::draw(ed, &mut scene, Affine::IDENTITY, renderer, theme, &presence);
        height
    }

    use crate::text_render::TextRenderer;

    #[test]
    fn code_editor_typing_newlines_and_indentation_use_the_real_key_path() {
        fn typing_inserts_on_the_real_key_path() {
            let mut ed = CodeEditor::local();
            ed.set_focused(true);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let mut doc = AutoCommit::new();
            for ch in ["f", "n", " ", "x"] {
                assert!(drive(&mut ed, &mut doc, &char_key(ch), mods(false, false)));
            }
            assert_eq!(ed.value(), "fn x");
            assert_eq!(ed.cursor_pos(), 4);
        }

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

            let wrapper_rect =
                Widget::ime_cursor_rect(&ed).expect("wrapper publishes IME geometry");
            let inner_rect = Widget::ime_cursor_rect(ed.inner()).expect("inner input has a caret");
            assert_eq!(wrapper_rect, inner_rect);
            assert!(wrapper_rect.x0 > ed.gutter_width());
        }

        fn enter_inserts_a_real_newline_and_gutter_counts_lines() {
            let mut ed = CodeEditor::local();
            ed.set_focused(true);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let mut doc = AutoCommit::new();
            {
                let ch = "a";
                drive(&mut ed, &mut doc, &char_key(ch), mods(false, false));
            }
            drive(
                &mut ed,
                &mut doc,
                &named_key(NamedKey::Enter),
                mods(false, false),
            );
            drive(&mut ed, &mut doc, &char_key("b"), mods(false, false));
            assert_eq!(ed.value(), "a\nb");
            assert_eq!(
                ed.line_count(),
                2,
                "gutter line count reflects the two logical lines"
            );
        }

        fn tab_inserts_soft_spaces_and_shift_tab_outdents() {
            let mut ed = CodeEditor::local().with_tab_width(4);
            ed.set_focused(true);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let mut doc = AutoCommit::new();
            assert!(drive(
                &mut ed,
                &mut doc,
                &named_key(NamedKey::Tab),
                mods(false, false)
            ));
            drive(&mut ed, &mut doc, &char_key("x"), mods(false, false));
            assert_eq!(ed.value(), "    x", "Tab inserts four spaces");
            // Shift+Tab removes one indent level.
            assert!(drive(
                &mut ed,
                &mut doc,
                &named_key(NamedKey::Tab),
                mods(false, true)
            ));
            assert_eq!(
                ed.value(),
                "x",
                "Shift+Tab outdents the leading four spaces"
            );
        }

        typing_inserts_on_the_real_key_path();
        wrapper_forwards_the_shaped_ime_candidate_rect();
        enter_inserts_a_real_newline_and_gutter_counts_lines();
        tab_inserts_soft_spaces_and_shift_tab_outdents();
    }

    #[test]
    fn code_editor_selection_cut_and_goto_line() {
        fn selection_and_cut_run_through_the_real_path() {
            let mut ed = CodeEditor::local().with_initial_value("abc");
            ed.set_focused(true);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let mut doc = AutoCommit::new();
            assert!(drive(&mut ed, &mut doc, &char_key("a"), mods(true, false)));
            assert_eq!(
                ed.inner().selection_range_public(),
                Some((0, 3)),
                "Ctrl+A selects the whole buffer on the real path",
            );
            assert!(drive(&mut ed, &mut doc, &char_key("x"), mods(true, false)));
            assert_eq!(
                ed.value(),
                "",
                "Ctrl+X cuts the selection through the shared core"
            );
        }

        fn shift_arrow_extends_selection_word_wise() {
            let mut ed = CodeEditor::local().with_initial_value("hello world");
            ed.set_focused(true);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let mut doc = AutoCommit::new();
            drive(
                &mut ed,
                &mut doc,
                &named_key(NamedKey::Home),
                mods(false, false),
            );
            drive(
                &mut ed,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
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

        selection_and_cut_run_through_the_real_path();
        shift_arrow_extends_selection_word_wise();
        goto_line_moves_the_caret_to_the_line_start();
        accessibility_reports_a_multiline_text_input();
    }

    #[test]
    fn code_editor_gutter_and_wrapper_geometry_follow_the_theme_scale() {
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

        fn scaled_gutter_and_wrapped_rows_share_the_shaped_editor_geometry() {
            let text = "first logical line wraps across several visual rows because the editor is narrow\nsecond";
            let mut normal = CodeEditor::local().with_initial_value(text);
            normal.set_rect(Rect::new(0.0, 0.0, 190.0, 420.0));
            draw_once(&mut normal, &Theme::dark());
            let normal_gutter = normal.gutter_width();
            let normal_line_height = normal.line_height();

            let theme = Theme::dark().with_text_scale(2.0);
            let mut large = CodeEditor::local().with_initial_value(text);
            large.set_rect(Rect::new(0.0, 0.0, 190.0, 900.0));
            draw_once(&mut large, &theme);
            assert!(
                large.gutter_width() >= normal_gutter * 1.8,
                "measured gutter grows with its line-number glyphs ({} vs {})",
                large.gutter_width(),
                normal_gutter,
            );
            assert!(
                large.line_height() >= normal_line_height * 1.9,
                "wrapper reports the shaped 2× row height"
            );

            let first_end = text.find('\n').expect("hard line break");
            let wrapped = large.selection_rects(&theme, 0, first_end);
            assert!(wrapped.len() >= 2, "first logical line wraps visually");
            assert!(wrapped.iter().all(|rect| {
                (rect.height() - large.line_height()).abs() < 0.1
                    && rect.x0 >= large.rect().x0 + large.gutter_width()
            }));
            let second_start = first_end + 1;
            let caret = large.caret_rect(&theme, second_start);
            assert!(caret.x0 >= large.rect().x0 + large.gutter_width());
            let point = Point::new(caret.x0 + 1.0, caret.y0 + caret.height() / 2.0);
            assert_eq!(large.byte_at_point(point), second_start);
            let mut doc = AutoCommit::new();
            drive(
                &mut large,
                &mut doc,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(point.x, point.y),
                },
                mods(false, false),
            );
            assert!(drive(
                &mut large,
                &mut doc,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                },
                mods(false, false),
            ));
            assert_eq!(
                large.cursor_pos(),
                second_start,
                "pointer event places the caret on the logical second line through wrapped rows"
            );
        }

        fn wrapper_layout_uses_theme_scale_with_a_stale_renderer_and_reverses_to_normal() {
            let text = "a long source line wraps over several visual rows while the wrapper owns the themed layout lifecycle";
            let mut editor = CodeEditor::local().with_initial_value(text);
            let mut renderer = TextRenderer::new();
            assert_eq!(renderer.text_scale(), 1.0, "stale renderer precondition");

            let large_theme = Theme::dark().with_text_scale(2.0);
            let large_height = lay_out_and_draw(&mut editor, &large_theme, &mut renderer, 190.0);
            let large_gutter = editor.gutter_width();
            let large_font = editor
                .inner()
                .shaped_font_size_for_test()
                .expect("large shaped font");
            assert!(
                (f64::from(large_height) - editor.inner().content_height().ceil()).abs() < 0.1,
                "wrapper measure and draw must share the 2x paragraph height"
            );

            let normal_theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = std::collections::HashMap::new();
            let _ = Widget::build_layout(&mut editor, &mut taffy, &normal_theme, &mut node_map);
            assert!(
                Widget::layout_needs_measure(&editor),
                "the same wrapper becomes measure-dirty when theme scale returns to 1x"
            );
            let normal_height = lay_out_and_draw(&mut editor, &normal_theme, &mut renderer, 190.0);
            let normal_gutter = editor.gutter_width();
            let normal_font = editor
                .inner()
                .shaped_font_size_for_test()
                .expect("normal shaped font");
            assert!(
                (f64::from(normal_height) - editor.inner().content_height().ceil()).abs() < 0.1,
                "wrapper measure and draw must share the restored 1x paragraph height"
            );
            assert!(large_height > normal_height * 1.8);
            assert!(large_gutter >= normal_gutter * 1.8);
            assert!(large_font >= normal_font * 1.9);
            assert_eq!(
                renderer.text_scale(),
                1.0,
                "renderer stayed stale throughout"
            );
        }

        no_gutter_offset_when_gutter_disabled();
        scaled_gutter_and_wrapped_rows_share_the_shaped_editor_geometry();
        wrapper_layout_uses_theme_scale_with_a_stale_renderer_and_reverses_to_normal();
    }
}

#[cfg(test)]
mod document_tests {
    use super::*;
    use crate::testing::dispatch;
    #[test]
    fn dictation_target_is_editable_source_and_refuses_read_only_or_blurred_input() {
        let mut editor = CodeEditor::local();
        editor.set_text("replace me");
        editor.set_focused(true);
        editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        assert_eq!(
            Widget::ime_purpose(&editor),
            Some(winit::window::ImePurpose::Normal)
        );
        let key = [19; 32];
        let stamp = Widget::ime_edit_fingerprint(&editor, &key).unwrap();
        editor.select_all();
        assert_ne!(Widget::ime_edit_fingerprint(&editor, &key), Some(stamp));
        dispatch(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Commit("日本語 café".into())),
        );
        assert_eq!(editor.value(), "日本語 café");
        editor.set_read_only(true);
        assert!(Widget::ime_purpose(&editor).is_none());
        assert!(Widget::ime_edit_fingerprint(&editor, &key).is_none());
        dispatch(
            &mut editor,
            &WindowEvent::Ime(winit::event::Ime::Commit("refused".into())),
        );
        assert_eq!(editor.value(), "日本語 café");
        editor.set_read_only(false);
        assert!(Widget::ime_edit_fingerprint(&editor, &key).is_some());
        editor.set_focused(false);
        assert!(Widget::ime_purpose(&editor).is_none());
        assert!(Widget::ime_edit_fingerprint(&editor, &key).is_none());
    }

    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            key,
            text.map(Into::into),
            ElementState::Pressed,
        ))
    }

    #[test]
    fn a_read_only_editor_selects_but_never_edits() {
        let mut editor = CodeEditor::local()
            .with_language(Language::Json)
            .with_accessible_name("Response body")
            .read_only();
        editor.set_text("{\n  \"ok\": true\n}\n");
        editor.set_focused(true);
        editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        dispatch(
            &mut editor,
            &key_event(Key::Character("x".into()), Some("x")),
        );
        dispatch(
            &mut editor,
            &key_event(Key::Named(NamedKey::Backspace), None),
        );
        dispatch(&mut editor, &key_event(Key::Named(NamedKey::Enter), None));
        assert_eq!(editor.value(), "{\n  \"ok\": true\n}\n", "nothing edits it");
        editor.select_all();
        assert!(editor.selection().is_some(), "it still selects");
        assert!(!Widget::wants_tab_input(&editor), "Tab moves focus on");
        let node = Widget::accessibility(&editor).unwrap();
        assert!(node.state.read_only);
        assert_eq!(node.name.as_deref(), Some("Response body"));
        // JSON is highlighted.
        let spans = Language::Json
            .highlighter()
            .expect("JSON has a grammar")
            .spans("  \"ok\": true", 0)
            .spans;
        assert!(spans.iter().any(|s| s.kind == TokenKind::StringLit));
    }

    #[test]
    fn min_and_max_lines_bound_the_height() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let measure = |editor: &mut CodeEditor, tr: &mut TextRenderer| {
            editor
                .inner_mut()
                .capture_layout_text_scale(theme.text_scale);
            Widget::measure(
                editor,
                taffy::Size {
                    width: Some(400.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                tr,
            )
            .height
        };
        let mut short = CodeEditor::local().with_min_lines(5);
        short.set_text("one line");
        let mut plain = CodeEditor::local();
        plain.set_text("one line");
        let min = measure(&mut short, &mut tr);
        let natural = measure(&mut plain, &mut tr);
        assert!(min > natural * 2.0, "five lines tall: {min} vs {natural}");
        let mut long = CodeEditor::local().with_max_lines(3);
        long.set_text(&"line\n".repeat(40));
        let mut free = CodeEditor::local();
        free.set_text(&"line\n".repeat(40));
        assert!(measure(&mut long, &mut tr) < measure(&mut free, &mut tr) / 4.0);
    }

    #[test]
    fn diagnostics_find_the_most_severe_under_the_pointer_and_paint() {
        let theme = Theme::dark();
        let mut editor = CodeEditor::local().with_diagnostics(vec![
            Diagnostic::new(0..3, Severity::Warning, "unused variable"),
            Diagnostic::new(1..2, Severity::Error, "unknown function `ab`"),
        ]);
        editor.set_text("abc def");
        editor.set_rect(Rect::new(0.0, 0.0, 400.0, 100.0));
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        Widget::draw(
            &mut editor,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        let caret = editor.caret_rect(&theme, 1);
        let at = Point::new(caret.x0 + 1.0, caret.center().y);
        let hit = editor.diagnostic_at(at).map(|d| d.severity);
        assert_eq!(
            hit,
            Some(Severity::Error),
            "the error wins where both cover"
        );
        let danger = theme
            .tokens
            .colors
            .semantic
            .danger
            .premultiply()
            .to_rgba8()
            .to_u32();
        assert!(
            scene.encoding().draw_data.contains(&danger),
            "the error is marked in the danger colour"
        );
    }
}
