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
use tilo_sort::CompactSort as _;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

#[cfg(test)]
use crate::presence::PresenceCtx;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, TextInputKind, Widget};

pub mod syntax;
pub mod tilolang;

pub use syntax::{SyntaxHighlighter, SyntaxState};
pub use tilolang::TilolangHighlighter;

/// The syntactic category of a run of source, and a coloured run within one line: shared with the
/// grammar set (`tilo-highlight`), which the shared runtime opens only when code is first
/// highlighted.
pub use tilo_highlight::{Span, TokenKind};

/// Opaque cross-line carry so multi-line constructs — block comments, fenced code — survive from
/// one line to the next.
pub type HlState = u64;

/// One logical line's highlight result: ordered, non-overlapping [`Span`]s (gaps render in the
/// default text colour) plus the [`HlState`] the *next* line begins with.
#[derive(Clone, Debug, Default)]
pub struct LineSpans {
    pub spans: Vec<Span>,
    pub carry: HlState,
}

impl LineSpans {}

/// A pluggable, per-line source tokenizer.
pub trait Highlighter {
    /// `line` never contains `\n`.
    fn spans(&self, line: &str, carry: HlState) -> LineSpans;
}

/// A built-in language the editor ships a [`Highlighter`] for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    PlainText,
    Rust,
    Typst,
    Sql,
    Json,
    Shell,
    /// Git commands, which are shell commands of their own.
    Git,
    Python,
    JavaScript,
    TypeScript,
    Yaml,
    Markdown,
    Html,
    Css,
    Toml,
    Dockerfile,
    C,
    Cpp,
    Go,
    Verilog,
    SystemVerilog,
    /// tilolang, the app language, coloured by its own lexer.
    Tilolang,
}

crate::component::choice!(Language, "Language", {
    PlainText => "plain_text",
    Rust => "rust",
    Typst => "typst",
    Sql => "sql",
    Json => "json",
    Shell => "shell",
    Git => "git",
    Python => "python",
    JavaScript => "javascript",
    TypeScript => "typescript",
    Yaml => "yaml",
    Markdown => "markdown",
    Html => "html",
    Css => "css",
    Toml => "toml",
    Dockerfile => "dockerfile",
    C => "c",
    Cpp => "cpp",
    Go => "go",
    Verilog => "verilog",
    SystemVerilog => "system_verilog",
    Tilolang => "tilolang",
});

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
            // Git commands are shell commands; the set has no grammar of their own.
            Language::Shell | Language::Git => {
                Some(Box::new(SyntaxHighlighter::for_token_or_plain("sh")))
            }
            Language::Python => Some(Box::new(SyntaxHighlighter::for_token_or_plain("py"))),
            Language::JavaScript => Some(Box::new(SyntaxHighlighter::for_token_or_plain("js"))),
            Language::TypeScript => Some(Box::new(SyntaxHighlighter::for_token_or_plain("ts"))),
            Language::Yaml => Some(Box::new(SyntaxHighlighter::for_token_or_plain("yaml"))),
            Language::Markdown => Some(Box::new(SyntaxHighlighter::for_token_or_plain("md"))),
            Language::Html => Some(Box::new(SyntaxHighlighter::for_token_or_plain("html"))),
            Language::Css => Some(Box::new(SyntaxHighlighter::for_token_or_plain("css"))),
            Language::Toml => Some(Box::new(SyntaxHighlighter::for_token_or_plain("toml"))),
            Language::Dockerfile => Some(Box::new(SyntaxHighlighter::for_token_or_plain(
                "dockerfile",
            ))),
            Language::C => Some(Box::new(SyntaxHighlighter::for_token_or_plain("c"))),
            Language::Cpp => Some(Box::new(SyntaxHighlighter::for_token_or_plain("cpp"))),
            Language::Go => Some(Box::new(SyntaxHighlighter::for_token_or_plain("go"))),
            Language::Verilog => Some(Box::new(SyntaxHighlighter::for_token_or_plain("v"))),
            Language::SystemVerilog => Some(Box::new(SyntaxHighlighter::for_token_or_plain("sv"))),
            Language::Tilolang => Some(Box::new(TilolangHighlighter)),
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

/// A problem a checker found in the source: a byte `range`, how much it matters, and what it says.
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

/// Wraps the multiline [`TextInput`] core and enables its gutter + syntax-highlight path, so all
/// caret/selection/clipboard/undo/IME/a11y behaviour is the one shared implementation.
pub struct CodeEditor {
    inner: TextInput,
    /// Selectable and copyable, never edited.
    read_only: bool,
    /// The fewest and most lines the editor is tall for; past the most it scrolls.
    min_lines: Option<usize>,
    max_lines: Option<usize>,
    diagnostics: Vec<Diagnostic>,
    /// The pointer, for the diagnostic under it.
    hover: Option<Point>,
    /// The place the view last asked to show, as it asked for it: the same place asked for again
    /// is not gone to again, so a person's own scrolling is left alone.
    pub(crate) revealed: Option<String>,
    /// How large the code reads, as a factor of its size: a person's own text zoom for this
    /// editor, never the window's.
    zoom: f32,
}

impl CodeEditor {
    /// CRDT-bound at `path` (mirrors [`TextInput::new`]).
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
            revealed: None,
            zoom: 1.0,
        }
    }

    /// Non-CRDT, owns its own string — read with [`CodeEditor::value`], observe edits with
    /// [`CodeEditor::take_dirty`].
    pub fn local() -> Self {
        Self::wrap(Self::configure(TextInput::local()))
    }

    /// Non-CRDT, following `state`: the editor shows the code it holds and writes each edit back to it,
    /// so code set elsewhere shows in place.
    pub fn from_state(state: &crate::LocalState<String>) -> Self {
        Self::wrap(Self::configure(TextInput::from_state(state)))
    }

    /// Source a person reads, selects and copies but does not edit (a diff, a generated file, a log).
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.set_read_only(true);
        self
    }

    /// [`Self::read_only`] after construction.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
        self.inner.set_read_only(read_only);
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// What the editor is called to assistive technology ("Query", "Response body").
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

    /// The diagnostic under `point`, if any: the most severe one whose range covers the byte there.
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

    /// Multiline body, monospace face, plain chrome, a line-number gutter, current-line highlight, and
    /// 4-space soft tabs by default.
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

    /// The editor owns rendering/caret/selection; this owns only the per-line lexing.
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

    /// How large the code reads, as a factor of its size (`1.25`): the editor's own text zoom,
    /// beside the window's scale rather than instead of it. A factor that is not finite or not
    /// above zero leaves the size as it was.
    #[must_use]
    pub fn with_zoom(mut self, factor: f32) -> Self {
        self.set_zoom(factor);
        self
    }

    /// [`Self::with_zoom`] after construction.
    pub fn set_zoom(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom = factor;
            self.inner.set_own_text_scale(factor);
        }
    }

    /// The factor the code reads at ([`Self::with_zoom`]).
    #[must_use]
    pub fn zoom(&self) -> f32 {
        self.zoom
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

    /// Show the text object `obj`, read on every sync.
    pub fn bind_text(&mut self, obj: ObjId) {
        self.inner.bind_text(obj);
    }

    /// Detach the bound text, keeping what is shown.
    pub fn clear_text_binding(&mut self) {
        self.inner.clear_text_binding();
    }

    /// For a host outside the taffy tree calling each frame instead of the `Widget::sync` pass.
    pub fn pull(&mut self, doc: &mut AutoCommit) {
        self.inner.sync(doc, &automerge::ROOT);
    }

    /// Stamps inserted runs with this author id (peer-attribution highlight).
    pub fn set_local_author(&mut self, author: Option<tilo_action_policy::MemberId>) {
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

    /// `[anchor, cursor)` in bytes; `anchor == cursor` clears it.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.inner.set_selection(anchor, cursor);
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        self.inner.selection_range_public()
    }

    /// Show the place `place` names, written `line` or `line:column`, both counted from 1, and
    /// put the caret there. The same place asked for twice is not gone to twice: a view that
    /// wants the same place again (a heading chosen again) adds a count after it (`3:1:2`).
    pub fn goto_place(&mut self, place: &str) {
        let mut parts = place.split(':');
        let line = parts
            .next()
            .and_then(|line| line.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let column = parts
            .next()
            .and_then(|column| column.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        self.goto_line(line);
        let start = self.cursor_pos();
        let rest = self.inner.value().get(start..).unwrap_or_default();
        let offset = rest
            .char_indices()
            .nth(column.saturating_sub(1))
            .map_or(rest.len(), |(at, _)| at);
        self.set_cursor_pos(start.saturating_add(offset));
        self.revealed = Some(place.to_owned());
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

    /// For embedding outside the taffy tree (a monolithic immediate-mode host).
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

    /// A host uses this to align a gutter marker with a source line (`caret_rect(theme, line_start)`).
    pub fn caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.inner.caret_rect(theme, byte)
    }

    /// One rect per visual row; a host draws a diagnostic squiggle along each rect's bottom edge.
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

    /// A monolithic host that owns its own layout calls [`set_rect`] then this each frame,
    /// with the presence it draws for: `presence` names the person, so their own writes are not
    /// tinted as someone else's.
    pub fn draw_embedded(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.inner
            .draw_child(scene, transform, text_renderer, theme, &env);
        self.draw_diagnostics(scene, transform, theme);
    }

    /// For an embedded host that routes the pointer itself: where it is (window coordinates), or `None`
    /// when it left.
    pub fn set_pointer(&mut self, point: Option<Point>) {
        self.hover = point;
    }

    /// For an embedded host: this editor's damage since the frame on screen, as part of `owner`, the
    /// widget in the tree it is drawn in ([`TextInput::collect_damage_for`]).
    pub fn collect_damage_for<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) {
        self.inner.collect_damage_for(owner, regions);
    }

    /// For an embedded host's `mark_drawn`: the frame just drawn is on screen.
    pub fn mark_shown(&mut self) {
        self.inner.mark_shown();
    }

    /// Whether the diagnostic under the pointer shows its message ([`Self::draw_diagnostic_card`]).
    pub fn shows_diagnostic_card(&self) -> bool {
        self.hover
            .is_some_and(|point| self.diagnostic_at(point).is_some())
    }

    /// For an embedded host: paint the message of the diagnostic under the pointer from its own
    /// top-layer pass, kept inside `window`.
    pub fn draw_diagnostic_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let env = crate::draw::DrawEnv {
            viewport: Some(window),
            ..crate::draw::DrawEnv::default()
        };
        self.draw_diagnostic_hover(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env,
        ));
    }

    pub fn inner(&self) -> &TextInput {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }
}

impl Widget for CodeEditor {
    crate::described!();

    crate::provides!(
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::widget::undo_points::UndoPoints
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
        self.inner
            .capture_layout_text_scale(theme.text_scale * self.zoom);
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
                // The padding around the lines stays: what is left after whole rows.
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

    /// Its size is its field's: the lines it holds.
    fn is_layout_dirty(&self) -> bool {
        self.inner.is_layout_dirty()
    }

    fn layout_measured(&mut self) {
        self.inner.layout_measured();
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.inner
            .draw_child(scene, transform, text_renderer, theme, &env);
        self.draw_diagnostics(scene, transform, theme);
    }

    fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.inner.draw_overlay(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env.clone(),
        ));
        self.draw_diagnostic_hover(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env.clone(),
        ));
    }

    /// The field's: the editor is its field and the diagnostics it marks.
    fn needs_redraw(&self) -> bool {
        Widget::needs_redraw(&self.inner)
    }

    /// The field's damage, as this widget's: a scroll is offered as a copy, the lines and marks (the
    /// diagnostics among them) that changed repaint where they were and are.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.inner.collect_damage_for(self, regions);
    }

    fn mark_drawn(&mut self) {
        Widget::mark_drawn(&mut self.inner);
    }

    /// The field's glide after the fingers lifted.
    fn needs_animation_frame(&self) -> bool {
        Widget::needs_animation_frame(&self.inner)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.hover = Some(ctx.logical(position));
        }
        if let WindowEvent::CursorLeft { .. } = event {
            self.hover = None;
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

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        if self.read_only || !self.inner.is_focused() {
            return None;
        }
        // Source text uses the shared input contract; no bytes go to a process.
        self.inner.ime_content()
    }

    fn ime_multiline(&self) -> bool {
        self.ime_content().is_some() && self.inner.ime_multiline()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_content()?;
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
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.read_only && matches!(action, crate::a11y::Action::SetValue(_)) {
            return false;
        }
        self.inner.perform_action(action, &[], state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.inner.text_review()
    }
}

impl CodeEditor {
    /// The squiggles under every diagnostic's range and the marks in the gutter beside its first line,
    /// in the theme's status colours.
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
        order.compact_sort_by_key(|d| d.severity);
        let mut marks = Vec::new();
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
                marks.push(Rect::new(x0, row.y0, x1, row.y1 + 2.0));
            }
            if gutter > 0.0 {
                let caret = self.inner.caret_rect(theme, start);
                let d_px = f64::from(theme.tokens.spacing.s2) * 0.75;
                let centre = Point::new(
                    rect.x0 + f64::from(theme.tokens.spacing.s1) + d_px / 2.0,
                    caret.center().y,
                );
                if rect.contains(centre) {
                    let dot = vello::kurbo::Circle::new(centre, d_px / 2.0);
                    scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &dot);
                    marks.push(vello::kurbo::Shape::bounding_box(&dot).inflate(1.0, 1.0));
                }
            }
        }
        scene.pop_layer();
        self.inner.note_marks(marks);
    }

    /// The message of the diagnostic under the pointer, on a floating surface just below its line, in
    /// the window's top layer.
    fn draw_diagnostic_hover(&self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
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
        let window = env
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        let (panel, _) = crate::widget::floating_surface::place_anchored(
            anchor,
            size,
            crate::widget::PopoverSide::Below,
            f64::from(theme.tokens.spacing.s1),
            crate::widget::floating_surface::panel_area(window, theme),
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

/// A wavy underline from `x0` to `x1` along `y`: the mark of a problem in text, as spelling marks
/// one.
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

impl crate::widget::undo_points::UndoPoints for CodeEditor {
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

    /// The code's own caret and band geometry, as the field it wraps draws them.
    fn presence_range_rects(
        &self,
        theme: &crate::theme::Theme,
        key: &PresenceKey,
        range: crate::presence::SelectionRange,
    ) -> Vec<vello::kurbo::Rect> {
        crate::presence::capability::PublishesPresence::presence_range_rects(
            &self.inner,
            theme,
            key,
            range,
        )
    }
}

fn find_from(hay: &str, from: usize, needle: char) -> Option<usize> {
    let rest = hay.get(from..)?;
    rest.find(needle).map(|i| from.saturating_add(i))
}

/// Bit in [`HlState`] marking "the next line is inside a ```` ``` ```` fenced
/// code block" (Typst).
const IN_FENCE: HlState = 1;

/// A pragmatic Typst highlighter — a per-line lexer recognising headings, comments, strings,
/// `#`-functions, numbers, raw code (inline and fenced), maths, `*strong*` and `_emph_` runs.
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

/// [`TypstHighlighter::spans`]'s fast paths: fence delimiter, inside a fence, heading.
fn typst_fence_or_heading_spans(line: &str, len: usize, carry: HlState) -> Option<LineSpans> {
    let trimmed = line.trim_start();
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

/// [`TypstHighlighter::spans`]'s character-by-character tokenizer, once the fast paths in
/// [`typst_fence_or_heading_spans`] don't apply.
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

    tilo_test_env::scenario! {
        /// The code editor's highlighting, typing, selection and geometry.
        /// Each phase was a test of its own and builds its own fixture.
        fn the_code_editor_highlights_edits_and_lays_out;

        phase code_editor_typing_newlines_and_indentation_use_the_real_key_path;
        {
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

            enter_inserts_a_real_newline_and_gutter_counts_lines();
            tab_inserts_soft_spaces_and_shift_tab_outdents();
        }

        phase code_editor_selection_cut_and_goto_line;
        {
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

            shift_arrow_extends_selection_word_wise();
            goto_line_moves_the_caret_to_the_line_start();
            accessibility_reports_a_multiline_text_input();
        }


        phase an_embedded_editor_does_not_tint_who_wrote_its_text;
        // Who wrote a run is history's to tell: the live page marks only what
        // someone is writing now, so a run the document records as theirs is
        // drawn plain.
        {
            use tilo_action_policy::MemberId;
            use automerge::ObjType;
            use automerge::marks::{ExpandMark, Mark};
            use automerge::transaction::Transactable as _;

            let theme = Theme::dark();
            let writer = MemberId::person("ada");
            let mut doc = AutoCommit::new();
            let text = doc
                .put_object(&automerge::ROOT, "text", ObjType::Text)
                .expect("a text object");
            doc.splice_text(&text, 0, 0, "words ada wrote")
                .expect("its words");
            doc.mark(
                &text,
                Mark::new("author".to_owned(), automerge::ScalarValue::Str("ada".into()), 0, 15),
                ExpandMark::None,
            )
            .expect("the run is hers");
            let mut editor = CodeEditor::local();
            editor.bind_text(text);
            editor.pull(&mut doc);
            editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            let presence = PresenceCtx::empty();
            let mut renderer = TextRenderer::new();
            let mut scene = Scene::new();
            editor.draw_embedded(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut renderer, &theme, presence.draw_env()));
            let colour = theme.tokens.colors.presence.of_rgb(presence.color_for(&writer));
            let drawn = &scene.encoding().draw_data;
            assert!(
                [0.20, 0.35].iter().all(|alpha| {
                    !drawn.contains(&colour.multiply_alpha(*alpha).premultiply().to_rgba8().to_u32())
                }),
                "a run someone wrote earlier is not tinted in their colour"
            );
        }
    }

    use crate::testing::Dispatch;
    use automerge::AutoCommit;
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::event::{Modifiers, WindowEvent};
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

    use crate::text_render::TextRenderer;
}

#[cfg(test)]
mod document_tests {
    use super::*;
    use crate::testing::dispatch;
    tilo_test_env::scenario! {
        /// Read-only and dictated input, the line bounds and the diagnostics of a code editor.
        /// Each phase was a test of its own and builds its own fixture.
        fn the_code_editor_guards_read_only_text_and_bounds_and_marks_it;

        phase dictation_target_is_editable_source_and_refuses_read_only_or_blurred_input;
        {
            let mut editor = CodeEditor::local();
            editor.set_text("replace me");
            editor.set_focused(true);
            editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            // Source is text over many lines; the input method is told so.
            let mut source = crate::ime::InputContent::TEXT;
            source.hints.multiline = true;
            assert_eq!(Widget::ime_content(&editor), Some(source));
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
            assert!(Widget::ime_content(&editor).is_none());
            assert!(Widget::ime_edit_fingerprint(&editor, &key).is_none());
            dispatch(
                &mut editor,
                &WindowEvent::Ime(winit::event::Ime::Commit("refused".into())),
            );
            assert_eq!(editor.value(), "日本語 café");
            editor.set_read_only(false);
            assert!(Widget::ime_edit_fingerprint(&editor, &key).is_some());
            editor.set_focused(false);
            assert!(Widget::ime_content(&editor).is_none());
            assert!(Widget::ime_edit_fingerprint(&editor, &key).is_none());
        }

        phase read_only_editor_refuses_middle_click_paste_and_can_be_made_editable;
        {
            let services = crate::process::ProcessServices::null();
            let _host = crate::process::enter(&services);
            crate::text_selection::clipboard_copy_primary("pasted");
            let mut editor = CodeEditor::local().with_initial_value("source").read_only();
            editor.set_focused(true);
            editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
            dispatch(
                &mut editor,
                &crate::synthetic_input::cursor_moved(50.0, 15.0),
            );
            let paste = crate::synthetic_input::mouse_input(
                ElementState::Pressed,
                winit::event::MouseButton::Middle,
            );
            dispatch(&mut editor, &paste);
            assert_eq!(editor.value(), "source");
            editor.set_read_only(false);
            dispatch(&mut editor, &paste);
            assert!(editor.value().contains("pasted"));
            let before = editor.value().to_owned();
            editor.set_read_only(true);
            dispatch(&mut editor, &paste);
            assert_eq!(editor.value(), before);
        }

        phase a_read_only_editor_selects_but_never_edits;
        {
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
            let spans = Language::Json
                .highlighter()
                .expect("JSON has a grammar")
                .spans("  \"ok\": true", 0)
                .spans;
            assert!(spans.iter().any(|s| s.kind == TokenKind::StringLit));
        }

        phase min_and_max_lines_bound_the_height;
        {
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

        phase diagnostics_find_the_most_severe_under_the_pointer_and_paint;
        {
            let theme = Theme::dark();
            let mut editor = CodeEditor::local().with_diagnostics(vec![
                Diagnostic::new(0..3, Severity::Warning, "unused variable"),
                Diagnostic::new(1..2, Severity::Error, "unknown function `ab`"),
            ]);
            editor.set_text("abc def");
            editor.set_rect(Rect::new(0.0, 0.0, 400.0, 100.0));
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            Widget::draw(&mut editor, &mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, PresenceCtx::default().draw_env()));
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

    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            key,
            text.map(Into::into),
            ElementState::Pressed,
        ))
    }
}

#[path = "code_editor_component.rs"]
mod component;
pub use component::CodeEditorProps;
