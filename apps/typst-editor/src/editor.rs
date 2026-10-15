//! The source editor pane.
//!
//! The text-editing surface itself is the shared `ui::CodeEditor` — one caret
//! / selection / clipboard / undo / IME / a11y implementation, a line-number
//! gutter, and Typst syntax highlighting supplied by the toolkit's built-in
//! `Language::Typst` highlighter. This module is now a thin shell that owns that
//! widget and adds the Typst-editor-specific pieces: the inline
//! compiler-diagnostic overlay (gutter severity markers, wavy squiggles under
//! error spans, end-of-line messages and the hover tooltip), plus an anchored
//! shared completion menu, using the editor's public geometry.
//!
//! The editor shows the source text of the file the app has open and submits
//! every edit as a splice of the file's `source` field, so a keystroke is an
//! attributed, undoable, permission-checked action.

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{Modifiers, WindowEvent};

use ui::ColorCompat as _;
use ui::framework::{Dispatcher, TextControl};
use ui::presence::{PathSegment, PresenceCtx};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Path;
use ui::tokens::TextRole;
use ui::widget::code_editor::{CodeEditor, Diagnostic as CodeDiagnostic, Language, Severity};
use ui::widget::{ContextMenu, MenuItem, Widget};

use crate::compile::{DiagKind, Diagnostic};
use crate::completions::{self, CompletionSession};
use crate::resource::Workspace;

/// Tab expands to this many spaces (soft tabs).
const TAB_WIDTH: usize = 2;

/// Dense source type; retain both the theme's UI and accessibility text scales.
const SOURCE_TYPE_SCALE: f32 = 0.64;

/// Source syntax inserted around the shared editor's selection, or at its caret.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceFormat {
    Bold,
    Italic,
    Math,
    Citation,
    Section,
}

/// Completion choices are non-modal: typing and IME stay with the source.
/// Reuse the shared menu's painting, navigation and accessible actions without
/// the focus trap appropriate to an ordinary context menu.
struct CompletionMenu(ContextMenu);

impl std::ops::Deref for CompletionMenu {
    type Target = ContextMenu;
    fn deref(&self) -> &ContextMenu {
        &self.0
    }
}

impl std::ops::DerefMut for CompletionMenu {
    fn deref_mut(&mut self) -> &mut ContextMenu {
        &mut self.0
    }
}

impl ui::widget::Decorator for CompletionMenu {
    type Inner = ContextMenu;
    fn inner(&self) -> &ContextMenu {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut ContextMenu {
        &mut self.0
    }
    fn traps_focus(&self) -> bool {
        false
    }
    fn modal_capture(&self) -> Option<Rect> {
        None
    }
}

pub struct SourceEditor {
    /// The shared code-editing widget — caret, selection, clipboard, undo,
    /// gutter, and Typst syntax highlighting all live here — over the open
    /// file's source.
    code: TextControl<CodeEditor, Workspace>,
    /// Where the edits go, once the window is attached.
    dispatcher: Option<Dispatcher<Workspace>>,
    /// Whether the source pane holds keyboard focus. Mirrored onto `code` each
    /// frame so its caret paints and its key routing engages.
    pub focused: bool,
    rect: Rect,
    /// Anchor byte for an in-flight click+drag selection.
    drag_anchor: Option<usize>,
    /// Last known hover position (window space), or `None` when the pointer is
    /// off the pane. Drives the inline-diagnostic tooltip.
    hover: Option<Point>,
    completion_menu: CompletionMenu,
    completion: Option<CompletionSession>,
    /// Exact buffer/caret for which the visible choices were computed.
    completion_snapshot: Option<(String, usize)>,
    dismissed_completion: Option<(String, usize)>,
    /// Only a local edit may trigger suggestions, never shaping, sync or navigation.
    completion_edit: Option<(String, usize)>,
    completion_pointer: Option<Point>,
}

impl SourceEditor {
    /// An editor over no source, read only until a dispatcher is
    /// [attached](Self::attach).
    pub fn new() -> Self {
        let nowhere = Workspace::paths()
            .files
            .item(&ui::tilo_resource::Id::from_raw(""))
            .source;
        Self {
            code: TextControl::view_only_at(Self::control_new(&nowhere), nowhere),
            dispatcher: None,
            focused: true,
            rect: Rect::ZERO,
            drag_anchor: None,
            hover: None,
            completion_menu: CompletionMenu(ContextMenu::new()),
            completion: None,
            completion_snapshot: None,
            dismissed_completion: None,
            completion_edit: None,
            completion_pointer: None,
        }
    }
}

impl Default for SourceEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceEditor {
    fn control_new(path: &Path<Workspace, String>) -> CodeEditor {
        // Collection ids, not their current order, identify the source to peers.
        let presence_path = path
            .segments()
            .iter()
            .map(|segment| PathSegment::Field(segment.to_string()))
            .collect();
        let mut control = CodeEditor::new(presence_path)
            .with_accessible_name("Typst source")
            .with_language(Language::Typst)
            .with_tab_width(TAB_WIDTH);
        control.set_focused(true);
        control
    }

    /// Include this control in the host's widget children so the framework
    /// publishes the real caret/selection and exposes the editor to accessibility.
    pub fn widget(&self) -> &dyn Widget {
        self
    }

    pub fn set_focused(&mut self, focused: bool) {
        if !focused {
            self.dismiss_completions();
        }
        self.focused = focused;
        self.control_mut().set_focused(focused);
    }

    pub fn widget_mut(&mut self) -> &mut dyn Widget {
        self
    }

    fn control(&self) -> &CodeEditor {
        self.code.control()
    }

    fn control_mut(&mut self) -> &mut CodeEditor {
        self.code.control_mut()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.control_mut().set_rect(rect);
    }

    /// Show the source at `path`, each edit a splice there once a
    /// dispatcher is attached.
    pub fn follow(&mut self, path: Path<Workspace, String>) {
        self.dismiss_completions();
        self.dismissed_completion = None;
        let mut control = Self::control_new(&path);
        control.set_focused(self.focused);
        control.set_rect(self.rect);
        self.code = TextControl::view_only_at(control, path);
        if let Some(dispatcher) = &self.dispatcher {
            self.code.attach(dispatcher.clone());
        }
    }

    /// Submit the edits of the source shown through `dispatcher`.
    pub fn attach(&mut self, dispatcher: Dispatcher<Workspace>) {
        self.code.attach(dispatcher.clone());
        self.dispatcher = Some(dispatcher);
    }

    /// Show the open file's source as `doc` holds it. Called from the app's
    /// `sync`.
    pub fn pull(&mut self, doc: &AutoCommit) {
        self.code.pull(doc);
    }

    /// Submit what the control edited since the last call: a splice of the
    /// source, or taken back while no dispatcher is attached.
    fn submit(&mut self, edits: &mut ui::widget::Edits<'_>) {
        self.code.submit(edits);
    }

    /// The caret's byte offset in the source.
    pub fn caret(&self) -> usize {
        self.control().cursor_pos()
    }

    /// Move the caret to a byte offset (used by the screenshot harness).
    pub fn set_caret(&mut self, byte: usize) {
        self.control_mut().set_cursor_pos(byte);
    }

    /// The current buffer text (mirrors the bound Automerge object).
    pub fn text(&self) -> &str {
        self.control().value()
    }

    /// Programmatically set the buffer (local/unbound use — tests, seeding).
    pub fn set_text(&mut self, s: &str) {
        self.control_mut().set_text(s);
    }

    /// Reset the caret to the document start (used when switching documents).
    pub fn reset_cursor(&mut self) {
        self.control_mut().set_cursor_pos(0);
        self.control_mut().scroll_by(-1.0e9);
    }

    /// Screenshot harness only: select a couple of source lines so a capture
    /// shows a real selection band over highlighted code.
    pub fn demo_select(&mut self) {
        let text = self.control_mut().value().to_string();
        let start = byte_of_line_col(&text, 1, 0);
        let end = byte_of_line_col(&text, 3, 0).min(text.len());
        let (a, b) = if end > start {
            (start, end)
        } else {
            (0, text.len().min(24))
        };
        self.control_mut().set_selection(a, b);
        self.focused = true;
    }

    /// Place the caret on a specific 1-based line (used when a diagnostic is
    /// clicked). `_text` is unused — the widget holds the source.
    pub fn goto_line(&mut self, _text: &str, line_1based: usize) {
        self.control_mut().goto_line(line_1based);
    }

    /// Insert Typst syntax through the same control and permission-checked
    /// splice action as typing. Selected text stays selected inside its new
    /// delimiters; without a selection the caret sits between them. Returns
    /// whether the edit was accepted. A refused edit restores its selection.
    pub fn insert_format(
        &mut self,
        format: SourceFormat,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let caret = self.caret();
        let selection = self.control().inner().selection_anchor_focus_public();
        let (start, end) = self.control().selection().unwrap_or((caret, caret));
        let text = self.text();
        let selected = text.get(start..end).unwrap_or_default();
        let (prefix, suffix) = match format {
            SourceFormat::Bold => ("*", "*"),
            SourceFormat::Italic => ("_", "_"),
            SourceFormat::Math => ("$", "$"),
            SourceFormat::Citation => ("@", ""),
            SourceFormat::Section => {
                // A heading must start on its own line, and must not absorb
                // the unselected remainder of the paragraph.
                let prefix = if start == 0 || text.get(..start).is_some_and(|s| s.ends_with('\n')) {
                    "= "
                } else {
                    "\n= "
                };
                let suffix =
                    if end == text.len() || text.get(end..).is_some_and(|s| s.starts_with('\n')) {
                        ""
                    } else {
                        "\n"
                    };
                (prefix, suffix)
            }
        };
        let replacement = format!("{prefix}{selected}{suffix}");
        let expected = format!(
            "{}{replacement}{}",
            text.get(..start).unwrap_or_default(),
            text.get(end..).unwrap_or_default(),
        );
        let inner_start = start + prefix.len();
        let inner_end = inner_start + selected.len();
        self.control_mut()
            .inner_mut()
            .insert_at_cursor(&replacement);
        self.submit(edits);
        if self.text() != expected {
            if let Some((anchor, focus)) = selection {
                self.control_mut().set_selection(anchor, focus);
            } else {
                self.set_caret(caret);
            }
            return false;
        }
        if selection.is_some() {
            let (anchor, focus) = if caret == start {
                (inner_end, inner_start)
            } else {
                (inner_start, inner_end)
            };
            self.control_mut().set_selection(anchor, focus);
        } else {
            self.set_caret(inner_start);
        }
        self.focused = true;
        self.control_mut().set_focused(true);
        self.completion_edit = Some((self.text().to_owned(), self.caret()));
        true
    }

    // ----- completions -----------------------------------------------------

    /// The shared menu owns its overlay, hit testing and semantic choices.
    pub fn completion_widget(&self) -> &dyn Widget {
        &self.completion_menu
    }

    pub fn completion_widget_mut(&mut self) -> &mut dyn Widget {
        &mut self.completion_menu
    }

    /// Recompute after the current source has been pulled and after local edits.
    /// Merely opening a file, moving its caret or receiving remote edits cannot
    /// summon a popup. Existing choices are invalidated as soon as they go stale.
    pub fn update_completions(&mut self, workspace: &Workspace) {
        let snapshot = (self.text().to_owned(), self.caret());
        let edited = self.completion_edit.take().as_ref() == Some(&snapshot);
        if self.control().selection().is_some() || self.control().inner().is_composing() {
            self.dismiss_completions();
            return;
        }
        if !edited {
            if self
                .completion_snapshot
                .as_ref()
                .is_some_and(|old| *old != snapshot)
            {
                self.dismiss_completions();
            }
            return;
        }
        if self.dismissed_completion.as_ref() == Some(&snapshot) {
            return;
        }
        let Some(session) = completions::complete(&snapshot.0, snapshot.1, workspace) else {
            self.dismiss_completions();
            return;
        };
        let items = session
            .items
            .iter()
            .map(|item| {
                let glyph = item.glyph.as_deref().unwrap_or("");
                let label = if matches!(item.badge.as_str(), "fn" | "kw") {
                    format!("{} [{}] — {}", item.label, item.badge, item.detail)
                } else {
                    format!("{} {glyph} — {} · {}", item.label, item.detail, item.badge)
                };
                MenuItem::selection(label)
            })
            .collect();
        self.completion_menu.set_items(items);
        self.completion_menu.open_anchored(Rect::ZERO, self.rect);
        self.completion_menu.focus_first_action();
        self.completion = Some(session);
        self.completion_snapshot = Some(snapshot);
    }

    pub fn dismiss_completions(&mut self) {
        self.dismissed_completion = Some((self.text().to_owned(), self.caret()));
        self.completion_menu.close();
        self.completion = None;
        self.completion_snapshot = None;
        self.completion_edit = None;
    }

    /// Drain after semantic activation as well as ordinary events. Acceptance
    /// goes through the same permission-checked, undoable splice as typing.
    pub fn accept_pending_completion(&mut self, edits: &mut ui::widget::Edits<'_>) -> bool {
        let Some(index) = self.completion_menu.take_selection() else {
            return false;
        };
        let snapshot = (self.text().to_owned(), self.caret());
        if !self.focused
            || self.control().inner().is_composing()
            || self.control().selection().is_some()
            || self.completion_snapshot.as_ref() != Some(&snapshot)
        {
            self.dismiss_completions();
            return false;
        }
        let Some(session) = self.completion.take() else {
            return false;
        };
        let Some(item) = session.items.get(index) else {
            return false;
        };
        let caret = self.caret();
        let selection = self.control().inner().selection_anchor_focus_public();
        let (Some(before), Some(after)) = (
            self.text().get(..session.start),
            self.text().get(session.end..),
        ) else {
            self.dismiss_completions();
            return false;
        };
        let expected = format!("{before}{}{after}", item.insert);
        self.control_mut().set_selection(session.start, session.end);
        self.control_mut()
            .inner_mut()
            .insert_at_cursor(&item.insert);
        self.submit(edits);
        let accepted = self.text() == expected;
        if !accepted {
            if let Some((anchor, focus)) = selection {
                self.control_mut().set_selection(anchor, focus);
            } else {
                self.set_caret(caret);
            }
        }
        self.dismiss_completions();
        accepted
    }

    /// Route BEFORE the host's normal pointer, key and shortcut handling. Text
    /// and IME still go to `CodeEditor`; only menu navigation/acceptance is captured.
    pub fn completion_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.completion_pointer = Some(ctx.logical(position));
        } else if let WindowEvent::Touch(touch) = event {
            self.completion_pointer = Some(ctx.logical(touch.location));
        }
        if self.control().inner().is_composing()
            || matches!(event, WindowEvent::Ime(winit::event::Ime::Preedit(..)))
        {
            self.dismiss_completions();
            return false;
        }
        let pending = self.accept_pending_completion(&mut ctx.edits());
        if !self.completion_menu.is_open() {
            return pending;
        }
        let over_menu = self
            .completion_pointer
            .is_some_and(|point| self.completion_menu.get_rect().contains(point));
        if !over_menu
            && matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::Touch(_)
            )
        {
            self.dismiss_completions();
            return false;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event {
            let modifiers = ctx.modifiers.state();
            if modifiers.control_key() || modifiers.super_key() || modifiers.alt_key() {
                return false;
            }
            if event.state != ElementState::Pressed {
                return false;
            }
            match event.logical_key {
                Key::Named(NamedKey::Tab | NamedKey::Enter) => {
                    let selected = self.completion_menu.hovered().unwrap_or(0);
                    self.completion_menu.click_item(selected);
                    self.accept_pending_completion(&mut ctx.edits());
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Escape) => {}
                Key::Named(
                    NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::PageUp
                    | NamedKey::PageDown,
                ) => {
                    self.dismiss_completions();
                    return false;
                }
                _ => return false,
            }
        }
        self.completion_menu.handle_event(event, ctx);
        self.accept_pending_completion(&mut ctx.edits());
        if !self.completion_menu.is_open() {
            self.dismiss_completions();
        }
        *ctx.handled
    }

    // ----- scrolling -------------------------------------------------------

    /// Wheel scroll. `dx`/`_text` are unused — the code view soft-wraps, so it
    /// only scrolls vertically.
    pub fn scroll_by(&mut self, _dx: f32, dy: f32, _text: &str) {
        self.control_mut().scroll_by(f64::from(dy));
    }

    /// Scroll the caret into view. The shared widget follows the caret after
    /// every edit automatically, so this is a no-op kept for call-site parity.
    pub fn ensure_visible(&mut self, _text: &str) {}

    // ----- mouse -----------------------------------------------------------

    pub fn on_mouse_down(&mut self, p: Point, shift: bool, _text: &str) {
        self.focused = true;
        self.control_mut().set_focused(true);
        let byte = self.control_mut().byte_at_point(p);
        if shift {
            let anchor = self
                .drag_anchor
                .unwrap_or_else(|| self.control_mut().cursor_pos());
            self.control_mut().set_selection(anchor, byte);
            self.drag_anchor = Some(anchor);
        } else {
            self.control_mut().set_cursor_pos(byte);
            self.drag_anchor = Some(byte);
        }
    }

    pub fn on_mouse_drag(&mut self, p: Point, _text: &str) {
        if let Some(anchor) = self.drag_anchor {
            let byte = self.control_mut().byte_at_point(p);
            self.control_mut().set_selection(anchor, byte);
        }
    }

    /// End of a click+drag. The anchor is kept so a following Shift+click still
    /// extends from the original point.
    pub fn on_mouse_up(&mut self) {}

    /// Update the hover position (window space); `None` when the pointer leaves
    /// the pane. Returns whether the tooltip-relevant state changed, so the app
    /// can schedule a redraw only when something moved into/out of a squiggle.
    pub fn on_mouse_move(&mut self, p: Option<Point>) -> bool {
        if self.hover == p {
            return false;
        }
        self.hover = p;
        true
    }

    // ----- keyboard --------------------------------------------------------

    /// Handle a keyboard event through the shared widget, submitting what it
    /// edited as one splice of the open file's source. Returns true if
    /// consumed.
    ///
    /// # Panics
    /// Never panics: `taffy::TaffyTree::new_leaf` cannot return `Err` for a
    /// freshly created, single-node tree on this taffy version (see
    /// `ui::widget::TaffyTreeExt`'s doc comment for the same guarantee).
    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        use taffy::{Style, TaffyTree};
        let focused = self.focused;
        let before = self.text().to_owned();
        self.control_mut().set_focused(focused);
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy.new_leaf(Style::default()).unwrap();
        self.control_mut().handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers,
                state: doc.reborrow(),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        self.submit(doc);
        if before != self.text() {
            self.completion_edit = Some((self.text().to_owned(), self.caret()));
        }
        handled
    }

    // ----- rendering -------------------------------------------------------

    /// Paint the pane. The editor stands on the window ground: it fills
    /// nothing of its own. The shared widget paints the gutter, line numbers,
    /// current-line band, selection, highlighted text and caret, and the
    /// compiler's diagnostics: a severity mark in the gutter and a squiggle
    /// under each span, in the theme's status colours.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        diags: &[Diagnostic],
        presence: &PresenceCtx,
    ) {
        let focused = self.focused;
        self.control_mut().set_focused(focused);
        let rect = self.rect;
        self.control_mut().set_rect(rect);
        // This embedded editor bypasses widget measurement. Give it the
        // theme's resolved code size instead of its stock 1× role size;
        // content sizing prevents applying the accessibility scale twice.
        self.control_mut()
            .inner_mut()
            .set_content_size(Some(theme.text(TextRole::Code).size * SOURCE_TYPE_SCALE));
        let text = self.control().value().to_string();
        let spans = diags
            .iter()
            .filter_map(|d| {
                let (b0, b1) = diag_byte_span(d, &text)?;
                let severity = match d.kind {
                    DiagKind::Error => Severity::Error,
                    DiagKind::Warning => Severity::Warning,
                };
                let message = match d.hints.first() {
                    Some(hint) => format!("{}\n{hint}", d.message),
                    None => d.message.clone(),
                };
                Some(CodeDiagnostic::new(b0..b1, severity, message))
            })
            .collect();
        self.control_mut().set_diagnostics(spans);
        let pointer = self.hover.filter(|p| self.rect.contains(*p));
        self.control_mut().set_pointer(pointer);
        // This is a document on the window ground, not a raised input well.
        // Keep the shared gutter/current-line lifts subtle on that ground.
        let mut source_theme = theme.clone();
        source_theme.tokens.colors.surfaces.raised = theme.tokens.colors.surfaces.content;
        let mut content = Scene::new();
        self.control_mut()
            .draw(&mut content, transform, tr, &source_theme, presence);
        // Shape first: caret geometry now includes wrapping, theme scales and
        // this frame's caret-follow scroll. Clip on a complete visual-row edge
        // rather than cutting the last line in half at the pane boundary.
        let clip = self.content_clip(theme);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        scene.append(&content, None);
        self.draw_inline_diagnostics(scene, transform, tr, theme, diags);
        scene.pop_layer();
        if self.completion_menu.is_open() {
            let anchor = self.control().caret_rect(theme, self.caret());
            if clip.contains(anchor.center()) {
                let window = presence.viewport.get().unwrap_or(rect);
                let row = f64::from(theme.tokens.metrics.menu_row_height).max(f64::from(
                    theme.text(TextRole::Body).line_height_px() + theme.tokens.spacing.s2,
                ));
                // Keep suggestions below the caret whenever two rows fit;
                // near the window bottom the shared menu flips above instead.
                let popup = if window.y1 - anchor.y1 >= row * 2.0 {
                    Rect::new(
                        window.x0,
                        anchor.y0,
                        window.x1,
                        window.y1.min(anchor.y1 + row * 6.0),
                    )
                } else {
                    Rect::new(
                        window.x0,
                        window.y0.max(anchor.y0 - row * 6.0),
                        window.x1,
                        anchor.y1,
                    )
                };
                self.completion_menu.reanchor(anchor, popup);
                self.completion_menu.layout_within(tr, theme);
            } else {
                self.dismiss_completions();
            }
        }
    }

    fn draw_inline_diagnostics(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        diags: &[Diagnostic],
    ) {
        let mut style = theme.text(TextRole::Caption);
        style.size = style
            .size
            .min(theme.text(TextRole::Code).size * SOURCE_TYPE_SCALE);
        let pad = f64::from(theme.tokens.spacing.s1);
        let gap = f64::from(theme.tokens.spacing.s3);
        for entry in inline_diagnostics(diags, self.text()) {
            let end = self.control().caret_rect(theme, entry.line_end);
            let x = end.x1 + gap;
            let available = self.rect.x1 - pad - x;
            if available <= pad * 3.0 || !self.rect.contains(end.center()) {
                continue;
            }
            let message = entry
                .diagnostic
                .message
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let label = match entry.diagnostic.kind {
                DiagKind::Error => format!("⊗ {message}"),
                DiagKind::Warning => format!("⚠ {message}"),
            };
            let label = tr.elide_to_width_style(&label, (available - 2.0 * pad) as f32, &style);
            let (w, h) = tr.measure_style(&label, None, &style);
            let h = f64::from(h).min(end.height());
            let y = end.center().y - h / 2.0;
            let chip = Rect::new(
                x,
                y,
                (x + f64::from(w) + 2.0 * pad).min(self.rect.x1),
                y + h,
            );
            let color = match entry.diagnostic.kind {
                DiagKind::Error => theme.tokens.colors.semantic.danger,
                DiagKind::Warning => theme.tokens.colors.semantic.warning,
            };
            scene.fill(
                Fill::NonZero,
                transform,
                color.with_alpha_factor(0.12),
                None,
                &chip.to_rounded_rect(f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_primary,
                transform.pre_translate(Vec2::new(x + pad, y)),
                None,
                &style,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn soft_wrapped_lines(&self, theme: &Theme) -> Vec<usize> {
        let mut byte = 0;
        self.text()
            .split('\n')
            .enumerate()
            .filter_map(|(line, text)| {
                let start = self.control().caret_rect(theme, byte);
                let end = self.control().caret_rect(theme, byte + text.len());
                byte += text.len() + 1;
                ((start.y0 - end.y0).abs() > 0.5).then_some(line + 1)
            })
            .collect()
    }

    fn content_clip(&self, theme: &Theme) -> Rect {
        let first = self.control().caret_rect(theme, 0);
        let line = first.height();
        if line <= 0.0 {
            return Rect::ZERO;
        }
        let top = (first.y0 + ((self.rect.y0 - first.y0) / line).ceil() * line)
            .clamp(self.rect.y0, self.rect.y1);
        let bottom = first.y0 + ((self.rect.y1 - first.y0) / line).floor() * line;
        Rect::new(
            self.rect.x0,
            top,
            self.rect.x1,
            bottom.clamp(self.rect.y0, self.rect.y1).max(top),
        )
    }

    /// The hover card for the diagnostic under the pointer, drawn in the
    /// window's top layer so it floats above the preview and can overflow
    /// the pane.
    pub fn draw_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        self.control()
            .draw_diagnostic_card(scene, transform, tr, theme, window);
    }

    /// The caret is a steady bar, so a focused-but-idle editor needs no
    /// animation frames.
    pub fn wants_animation(&self) -> bool {
        false
    }
}

/// One message per logical line, with errors taking precedence over warnings.
/// The end is a UTF-8 boundary before the newline, not the diagnostic span's end.
struct InlineDiagnostic<'a> {
    line_end: usize,
    diagnostic: &'a Diagnostic,
}

fn inline_diagnostics<'a>(diags: &'a [Diagnostic], text: &str) -> Vec<InlineDiagnostic<'a>> {
    let mut lines = std::collections::BTreeMap::<usize, &Diagnostic>::new();
    for diagnostic in diags {
        let Some(line) = diagnostic.line.and_then(|line| line.checked_sub(1)) else {
            continue;
        };
        if text.split('\n').nth(line).is_none() || diag_byte_span(diagnostic, text).is_none() {
            continue;
        }
        let end = byte_of_line_col(text, line, usize::MAX);
        lines
            .entry(end)
            .and_modify(|old| {
                if diagnostic.kind == DiagKind::Error {
                    *old = diagnostic;
                }
            })
            .or_insert(diagnostic);
    }
    lines
        .into_iter()
        .map(|(line_end, diagnostic)| InlineDiagnostic {
            line_end,
            diagnostic,
        })
        .collect()
}

// Focused input is delivered directly to the retained node, not necessarily
// through the painted app. Stand in for the text control so popup navigation
// precedes editing on that route as well as on the app's shortcut route.
impl ui::widget::Decorator for SourceEditor {
    type Inner = TextControl<CodeEditor, Workspace>;

    fn inner(&self) -> &Self::Inner {
        &self.code
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.code
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.completion_menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.completion_menu]
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let source = Widget::build_layout(&mut self.code, taffy, theme, nodes);
        let menu = Widget::build_layout(&mut self.completion_menu, taffy, theme, nodes);
        // Both nodes were just allocated in this tree. The popup is positioned
        // from shaped caret geometry, outside the code's measured content.
        let _ = taffy.set_children(source, &[menu]);
        ui::widget::decorator::register_node(nodes, source, self);
        source
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.completion_event(event, ctx) {
            return;
        }
        let before = self.text().to_owned();
        Widget::handle_event(&mut self.code, event, ctx);
        if self.text() != before {
            self.completion_edit = Some((self.text().to_owned(), self.caret()));
        }
    }
}

/// The byte offset in `text` of 0-based `(line, col)` (col in chars). Clamps to
/// the line end / text end for out-of-range inputs.
fn byte_of_line_col(text: &str, line0: usize, col0: usize) -> usize {
    let mut byte = 0usize;
    for (i, l) in text.split('\n').enumerate() {
        if i == line0 {
            let add = l.char_indices().nth(col0).map_or(l.len(), |(o, _)| o);
            return byte + add;
        }
        byte += l.len() + 1; // + '\n'
    }
    text.len()
}

/// The byte range `[start, end)` a diagnostic covers, from its 1-based
/// line/col span, widened to at least one byte so a zero-width span still
/// underlines. `None` when the diagnostic carries no location.
fn diag_byte_span(d: &Diagnostic, text: &str) -> Option<(usize, usize)> {
    let (sl, sc) = (d.line?.saturating_sub(1), d.col?.saturating_sub(1));
    let (el, ec) = (
        d.end_line.unwrap_or(d.line?).saturating_sub(1),
        d.end_col
            .unwrap_or(d.col?.saturating_add(1))
            .saturating_sub(1),
    );
    let b0 = byte_of_line_col(text, sl, sc);
    let next = text
        .get(b0..)
        .unwrap_or_default()
        .chars()
        .next()
        .map_or(b0, |c| b0 + c.len_utf8());
    let b1 = byte_of_line_col(text, el, ec).max(next);
    Some((b0, b1.min(text.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::{Id, Invoke, Replica, Resource as _};

    /// A workspace whose one file holds `text`, and an editor showing it
    /// that nothing edits.
    fn showing(text: &str) -> (AutoCommit, SourceEditor) {
        let mut workspace = Replica::<Workspace>::new();
        let id = Id::from_raw("file");
        let file = crate::resource::TypstFile {
            source: text.to_owned(),
            ..crate::resource::TypstFile::default()
        };
        workspace
            .apply(Invoke::edit(&Workspace::paths().files.insert_as(&id, 0, &file)).unwrap())
            .unwrap();
        let doc = AutoCommit::load(&workspace.save()).unwrap();
        let mut editor = SourceEditor::new();
        editor.follow(Workspace::paths().files.item(&id).source);
        editor.pull(&doc);
        (doc, editor)
    }

    fn source(doc: &AutoCommit) -> String {
        Workspace::read(doc)
            .unwrap()
            .files
            .values()
            .next()
            .unwrap()
            .source
            .clone()
    }

    fn key(key: winit::keyboard::Key, text: Option<&str>) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            key,
            text.map(winit::keyboard::SmolStr::new),
            winit::event::ElementState::Pressed,
        ))
    }

    fn type_source(editor: &mut SourceEditor, doc: &mut AutoCommit, text: &str) {
        for ch in text.chars() {
            let text = ch.to_string();
            assert!(editor.handle_key(
                &key(
                    winit::keyboard::Key::Character(text.clone().into()),
                    Some(&text)
                ),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(doc),
            ));
            editor.update_completions(&Workspace::read(doc).unwrap());
        }
    }

    fn completion_event(
        editor: &mut SourceEditor,
        doc: &mut AutoCommit,
        event: &WindowEvent,
    ) -> bool {
        let mut taffy = taffy::TaffyTree::<()>::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut handled = false;
        editor.completion_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: ui::widget::Edits::lend(doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        )
    }

    #[test]
    fn completion_typing_acceptance_and_navigation_use_shared_controls() {
        use winit::keyboard::{Key, NamedKey};
        for accept in [NamedKey::Tab, NamedKey::Enter] {
            let (mut doc, mut editor) = showing("");
            editor.attach(ui::framework::testing::detached_dispatcher("author"));
            type_source(&mut editor, &mut doc, "#ta");
            assert!(editor.completion_menu.is_open());
            assert!(completion_event(
                &mut editor,
                &mut doc,
                &key(Key::Named(accept), None)
            ));
            assert_eq!(source(&doc), "#table(columns: 2, [], [])");
            assert!(!editor.completion_menu.is_open());
            editor.update_completions(&Workspace::read(&doc).unwrap());
            assert!(
                !editor.completion_menu.is_open(),
                "acceptance cannot reopen suggestions"
            );
        }
        let (mut doc, mut editor) = showing("");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        type_source(&mut editor, &mut doc, "#");
        let caret = editor.caret();
        assert!(completion_event(
            &mut editor,
            &mut doc,
            &key(Key::Named(NamedKey::ArrowDown), None)
        ));
        assert_eq!(
            editor.caret(),
            caret,
            "menu navigation does not move the source caret"
        );
        assert_eq!(editor.completion_menu.hovered(), Some(1));
        assert!(completion_event(
            &mut editor,
            &mut doc,
            &key(Key::Named(NamedKey::ArrowUp), None)
        ));
        assert_eq!(editor.completion_menu.hovered(), Some(0));
        assert!(completion_event(
            &mut editor,
            &mut doc,
            &key(Key::Named(NamedKey::Escape), None)
        ));
        editor.update_completions(&Workspace::read(&doc).unwrap());
        assert!(!editor.completion_menu.is_open());
        editor.set_caret(0);
        editor.update_completions(&Workspace::read(&doc).unwrap());
        editor.set_caret(1);
        editor.update_completions(&Workspace::read(&doc).unwrap());
        assert!(
            !editor.completion_menu.is_open(),
            "navigation never triggers suggestions"
        );
        type_source(&mut editor, &mut doc, "t");
        assert!(
            editor.completion_menu.is_open(),
            "editing resumes suggestions after Escape"
        );
    }

    #[test]
    fn completion_does_not_open_after_navigation_in_the_same_event_batch() {
        use winit::keyboard::Key;
        let (mut doc, mut editor) = showing("");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        assert!(editor.handle_key(
            &key(Key::Character("#".into()), Some("#")),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
        ));
        editor.set_caret(0);
        editor.update_completions(&Workspace::read(&doc).unwrap());
        editor.set_caret(1);
        editor.update_completions(&Workspace::read(&doc).unwrap());
        assert!(!editor.completion_menu.is_open());
    }

    #[test]
    fn completion_semantic_activation_splices_the_same_crdt_text() {
        use automerge::ReadDoc as _;
        let (mut doc, mut editor) = showing("");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        type_source(&mut editor, &mut doc, "#ta");
        let obj = editor.control().inner().bound_obj().unwrap().clone();
        let access = editor.completion_widget().accessibility().unwrap();
        let target = access.children[0].action_target.clone().unwrap();
        assert!(editor.completion_widget_mut().perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(editor.accept_pending_completion(&mut ui::widget::Edits::lend(&mut doc)));
        assert_eq!(doc.text(&obj).unwrap(), "#table(columns: 2, [], [])");
        assert_eq!(source(&doc), editor.text());
    }

    #[test]
    fn completion_pointer_acceptance_uses_the_shaped_caret_anchor() {
        let (mut doc, mut editor) = showing("");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        editor.set_rect(Rect::new(30.0, 40.0, 530.0, 440.0));
        type_source(&mut editor, &mut doc, "#ta");
        let theme = Theme::dark().with_ui_scale(1.5);
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &[],
            &PresenceCtx::empty(),
        );
        let caret = editor.control().caret_rect(&theme, editor.caret());
        let row = editor.completion_menu.item_rect(0).unwrap();
        assert!(row.y0 >= caret.y1, "choices open below the shaped caret");
        let point = row.center();
        completion_event(
            &mut editor,
            &mut doc,
            &ui::synthetic_input::cursor_moved(point.x, point.y),
        );
        assert!(completion_event(
            &mut editor,
            &mut doc,
            &ui::synthetic_input::mouse_press()
        ));
        assert_eq!(source(&doc), "#table(columns: 2, [], [])");
        assert!(!editor.completion_menu.is_open());
    }

    #[test]
    fn completion_does_not_capture_outside_clicks_or_wheel_scroll() {
        use winit::event::{DeviceId, MouseScrollDelta, TouchPhase};
        for event in [
            ui::synthetic_input::mouse_press(),
            WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            },
        ] {
            let (mut doc, mut editor) = showing("");
            editor.attach(ui::framework::testing::detached_dispatcher("author"));
            type_source(&mut editor, &mut doc, "#ta");
            completion_event(
                &mut editor,
                &mut doc,
                &ui::synthetic_input::cursor_moved(1000.0, 1000.0),
            );
            assert!(
                !completion_event(&mut editor, &mut doc, &event),
                "the underlying pane must receive outside input"
            );
            assert!(!editor.completion_menu.is_open());
            assert_eq!(source(&doc), "#ta");
        }
    }

    #[test]
    fn ime_preedit_dismisses_completions_without_accepting_a_candidate() {
        let (mut doc, mut editor) = showing("");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        type_source(&mut editor, &mut doc, "#ta");
        assert!(editor.completion_menu.is_open());
        let preedit = WindowEvent::Ime(winit::event::Ime::Preedit("日本".into(), Some((0, 6))));
        assert!(!completion_event(&mut editor, &mut doc, &preedit));
        assert!(!editor.completion_menu.is_open());
        editor.handle_key(
            &preedit,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(editor.control().inner().is_composing());
        editor.completion_menu.click_item(0);
        assert!(!editor.accept_pending_completion(&mut ui::widget::Edits::lend(&mut doc)));
        assert_eq!(source(&doc), "#ta");
    }

    #[test]
    fn completion_is_not_seeded_or_allowed_to_bypass_permissions() {
        let (mut doc, mut editor) = showing("#ta");
        editor.set_caret(3);
        editor.update_completions(&Workspace::read(&doc).unwrap());
        assert!(!editor.completion_menu.is_open());
        // Exercise acceptance against a view-only binding, independent of the
        // local-edit gate that normally prevents this popup from opening.
        editor.completion_edit = Some((editor.text().to_owned(), editor.caret()));
        editor.update_completions(&Workspace::read(&doc).unwrap());
        editor.completion_menu.click_item(0);
        assert!(!editor.accept_pending_completion(&mut ui::widget::Edits::lend(&mut doc)));
        assert_eq!(source(&doc), "#ta");
        assert_eq!(editor.text(), "#ta");
        assert_eq!(editor.caret(), 3);
    }

    #[test]
    fn inline_messages_anchor_at_utf8_line_ends_and_prefer_errors() {
        let diagnostic = |kind, line, col, message: &str| Diagnostic {
            kind,
            line: Some(line),
            col: Some(col),
            end_line: Some(line),
            end_col: Some(col),
            message: message.into(),
            hints: vec![],
        };
        let text = "λ café\n日本\n";
        let diagnostics = [
            diagnostic(DiagKind::Warning, 1, 2, "warning"),
            diagnostic(DiagKind::Error, 1, 3, "error"),
            diagnostic(DiagKind::Warning, 2, 1, "unicode"),
            diagnostic(DiagKind::Error, 3, 1, "empty final line"),
            diagnostic(DiagKind::Error, 99, 1, "outside source"),
        ];
        let entries = inline_diagnostics(&diagnostics, text);
        assert_eq!(
            entries.iter().map(|e| e.line_end).collect::<Vec<_>>(),
            [8, 15, 16]
        );
        assert_eq!(entries[0].diagnostic.message, "error");
        for entry in entries {
            assert!(text.is_char_boundary(entry.line_end));
        }
        assert_eq!(diag_byte_span(&diagnostics[2], text), Some((9, 12)));
        assert_eq!(diag_byte_span(&diagnostics[3], text), Some((16, 16)));
    }

    #[test]
    fn editor_maps_positions_and_diagnostic_spans() {
        // Case `byte_of_line_col_maps_positions`.
        {
            let t = "ab\ncde\nf";
            assert_eq!(byte_of_line_col(t, 0, 0), 0);
            assert_eq!(byte_of_line_col(t, 1, 0), 3);
            assert_eq!(byte_of_line_col(t, 1, 2), 5);
            assert_eq!(byte_of_line_col(t, 2, 0), 7);
        }
        // Case `diag_byte_span_covers_the_reported_span`.
        {
            // 1-based ln 2 cols 3..6 → 0-based line 1 cols 2..5 → bytes 5..8 in
            // "ab\ncdefg".
            let t = "ab\ncdefg";
            let d = Diagnostic {
                kind: DiagKind::Error,
                message: "boom".into(),
                line: Some(2),
                col: Some(3),
                end_line: Some(2),
                end_col: Some(6),
                hints: vec![],
            };
            assert_eq!(diag_byte_span(&d, t), Some((5, 8)));
        }
        // Case `diag_zero_width_span_is_widened`.
        {
            let t = "abc";
            let d = Diagnostic {
                kind: DiagKind::Warning,
                message: "x".into(),
                line: Some(1),
                col: Some(2),
                end_line: Some(1),
                end_col: Some(2),
                hints: vec![],
            };
            let (b0, b1) = diag_byte_span(&d, t).unwrap();
            assert!(
                b1 > b0,
                "a zero-width span still underlines at least one byte"
            );
        }
        // Case `gutter_severity_prefers_error`.
        {
            let warn = Diagnostic {
                kind: DiagKind::Warning,
                message: "w".into(),
                line: Some(2),
                col: Some(1),
                end_line: Some(2),
                end_col: Some(2),
                hints: vec![],
            };
            let err = Diagnostic {
                kind: DiagKind::Error,
                message: "e".into(),
                line: Some(2),
                col: Some(4),
                end_line: Some(2),
                end_col: Some(6),
                hints: vec![],
            };
            let mut ed = SourceEditor::new();
            ed.set_text("a\nbcdefg\n");
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            ed.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &[warn, err],
                &PresenceCtx::empty(),
            );
            let severities: Vec<Severity> = ed
                .control()
                .diagnostics()
                .iter()
                .map(|d| d.severity)
                .collect();
            assert_eq!(
                severities,
                [Severity::Warning, Severity::Error],
                "the widget marks each span at its severity"
            );
        }
    }

    #[test]
    fn embedded_source_scales_type_gutter_and_hit_testing_together() {
        let mut editor = SourceEditor::new();
        editor.set_text("= Motion\n\nA short note.\n");
        editor.set_rect(Rect::new(0.0, 0.0, 660.0, 700.0));
        let mut tr = TextRenderer::new();
        let mut base_line = 0.0;
        let mut base_gutter = 0.0;
        for (index, theme) in [
            Theme::dark(),
            Theme::dark().with_ui_scale(1.5),
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.2),
            Theme::dark(),
        ]
        .iter()
        .enumerate()
        {
            editor.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                theme,
                &[],
                &PresenceCtx::empty(),
            );
            let input = editor.control().inner();
            let line = input.body_line_height();
            let gutter = editor.control().gutter_width();
            if index == 0 {
                base_line = line;
                base_gutter = gutter;
            }
            let factor = f64::from(theme.ui_scale * theme.text_scale);
            assert!((line / base_line - factor).abs() < 0.01);
            if index == 1 || index == 2 {
                assert!(gutter > base_gutter, "line numbers grow with source text");
            }
            let caret = editor.control().caret_rect(theme, 4);
            assert_eq!(editor.control().byte_at_point(caret.center()), 4);
        }
    }

    #[test]
    fn formatting_splices_selected_unicode_and_preserves_selection_direction() {
        use automerge::ReadDoc as _;
        for (format, expected, selected) in [
            (SourceFormat::Bold, "before *café λ* after", (8, 16)),
            (SourceFormat::Italic, "before _café λ_ after", (8, 16)),
            (SourceFormat::Math, "before $café λ$ after", (8, 16)),
            (SourceFormat::Citation, "before @café λ after", (8, 16)),
            (SourceFormat::Section, "before \n= café λ\n after", (10, 18)),
        ] {
            for reversed in [false, true] {
                let (mut doc, mut editor) = showing("before café λ after");
                editor.attach(ui::framework::testing::detached_dispatcher("author"));
                let obj = editor.control().inner().bound_obj().unwrap().clone();
                let (anchor, focus) = if reversed { (15, 7) } else { (7, 15) };
                editor.control_mut().set_selection(anchor, focus);
                assert!(editor.insert_format(format, &mut ui::widget::Edits::lend(&mut doc)));
                assert_eq!(source(&doc), expected);
                assert_eq!(
                    doc.text(&obj).unwrap(),
                    expected,
                    "the same CRDT Text is spliced"
                );
                assert_eq!(editor.text(), expected);
                assert_eq!(editor.control().selection(), Some(selected));
                assert_eq!(
                    editor.caret(),
                    if reversed { selected.0 } else { selected.1 }
                );
                editor.pull(&doc);
                assert_eq!(editor.text(), expected);
            }
        }
    }

    #[test]
    fn formatting_at_a_caret_inserts_editable_syntax_without_placeholder_copy() {
        for (format, expected, caret) in [
            (SourceFormat::Bold, "**", 1),
            (SourceFormat::Italic, "__", 1),
            (SourceFormat::Math, "$$", 1),
            (SourceFormat::Citation, "@", 1),
            (SourceFormat::Section, "= ", 2),
        ] {
            let (mut doc, mut editor) = showing("");
            editor.attach(ui::framework::testing::detached_dispatcher("author"));
            assert!(editor.insert_format(format, &mut ui::widget::Edits::lend(&mut doc)));
            assert_eq!(source(&doc), expected);
            assert_eq!(editor.caret(), caret);
            assert_eq!(editor.control().selection(), None);
        }
        let (mut doc, mut editor) = showing("paragraph");
        editor.attach(ui::framework::testing::detached_dispatcher("author"));
        editor.set_caret(4);
        assert!(editor.insert_format(
            SourceFormat::Section,
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert_eq!(source(&doc), "para\n= \ngraph");
        assert_eq!(editor.caret(), 7);
    }

    #[test]
    fn view_only_formatting_restores_the_original_text_caret_and_selection() {
        let (mut doc, mut editor) = showing("café λ");
        editor.control_mut().set_selection(8, 0);
        assert!(!editor.insert_format(SourceFormat::Bold, &mut ui::widget::Edits::lend(&mut doc)));
        assert_eq!(source(&doc), "café λ");
        assert_eq!(editor.text(), "café λ");
        assert_eq!(editor.caret(), 0);
        assert_eq!(editor.control().selection(), Some((0, 8)));
        editor.set_caret(5);
        assert!(!editor.insert_format(SourceFormat::Math, &mut ui::widget::Edits::lend(&mut doc)));
        assert_eq!(editor.caret(), 5);
        assert_eq!(editor.control().selection(), None);
    }

    #[test]
    fn source_clips_on_complete_visual_rows_after_scaling_wrapping_and_scrolling() {
        let mut editor = SourceEditor::new();
        editor.set_text(&"a long source line that wraps many times at narrow widths\n".repeat(30));
        let mut tr = TextRenderer::new();
        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.2),
        ] {
            for height in [5.0, 103.0, 231.0] {
                editor.set_rect(Rect::new(19.0, 27.0, 233.0, 27.0 + height));
                for scroll in [0.0, 13.5, 51.25] {
                    editor.scroll_by(0.0, scroll, "");
                    let mut rendered = Scene::new();
                    editor.draw(
                        &mut rendered,
                        Affine::IDENTITY,
                        &mut tr,
                        &theme,
                        &[],
                        &PresenceCtx::empty(),
                    );
                    let clip = editor.content_clip(&theme);
                    let mut clipping = Scene::new();
                    clipping.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        Affine::IDENTITY,
                        &clip,
                    );
                    assert!(
                        rendered
                            .encoding()
                            .path_data
                            .starts_with(&clipping.encoding().path_data),
                        "the complete-row clip is actually encoded before content"
                    );
                    assert_eq!(rendered.encoding().n_open_clips, 0);
                    assert!((clip.x0 - editor.rect.x0).abs() < f64::EPSILON);
                    assert!((clip.x1 - editor.rect.x1).abs() < f64::EPSILON);
                    let first = editor.control().caret_rect(&theme, 0);
                    assert!(clip.y0 >= editor.rect.y0);
                    assert!(clip.y0 - editor.rect.y0 < first.height());
                    assert!(clip.y1 <= editor.rect.y1);
                    if clip.height() > 0.0 {
                        let rows = (clip.y1 - first.y0) / first.height();
                        assert!(
                            (rows - rows.round()).abs() < 0.001,
                            "no partial final row: {rows}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn active_line_is_a_subtle_lift_of_the_document_ground_only_when_focused() {
        let mut editor = SourceEditor::new();
        editor.set_text("a note\n");
        editor.set_rect(Rect::new(0.0, 0.0, 400.0, 200.0));
        let mut tr = TextRenderer::new();
        for theme in [Theme::dark(), Theme::light()] {
            let band = theme
                .tokens
                .colors
                .lift_on(theme.tokens.colors.surfaces.content, 0.5)
                .with_alpha_factor(0.6)
                .premultiply()
                .to_rgba8()
                .to_u32();
            for focused in [true, false] {
                editor.focused = focused;
                let mut rendered = Scene::new();
                editor.draw(
                    &mut rendered,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &[],
                    &PresenceCtx::empty(),
                );
                assert_eq!(rendered.encoding().draw_data.contains(&band), focused);
            }
        }
    }

    #[test]
    fn source_uses_real_file_scoped_presence_and_publishes_its_selection() {
        use ui::collaboration::PresencePayload;
        use ui::presence::capability::{presence_key, subtree_presence};
        let (_doc, mut editor) = showing("= Heading\nA note\n");
        editor.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        let key = presence_key(editor.widget()).unwrap();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut plain = Scene::new();
        editor.draw(
            &mut plain,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &[],
            &PresenceCtx::empty(),
        );
        let mut presence = PresenceCtx::empty();
        presence.set_caret(&key, ui::UserId("collaborator".into()), 3);
        presence.set_display_name(ui::UserId("collaborator".into()), "Ada");
        let mut with_peer = Scene::new();
        editor.draw(
            &mut with_peer,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &[],
            &presence,
        );
        assert!(
            with_peer.encoding().draw_data.len() > plain.encoding().draw_data.len(),
            "the actual peer caret and label are painted"
        );
        editor.control_mut().set_selection(2, 5);
        assert!(
            matches!(subtree_presence(editor.widget()).as_slice(), [(scope, PresencePayload::Selection { start: 2, end: 5 })] if scope == &key)
        );
        editor.follow(
            Workspace::paths()
                .files
                .item(&Id::from_raw("other-file"))
                .source,
        );
        assert_ne!(presence_key(editor.widget()).unwrap(), key);
        editor.focused = false;
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &[],
            &presence,
        );
        assert!(subtree_presence(editor.widget()).is_empty());
    }

    #[test]
    fn editor_input_without_a_dispatcher_and_caret_moves() {
        // Case `typing_without_a_dispatcher_changes_nothing`:
        // An editor nobody attached a dispatcher to only shows the text; the
        // keys it takes are taken back.
        {
            use winit::event::ElementState;
            use winit::keyboard::{Key, SmolStr};
            let (mut doc, mut ed) = showing("");
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            ed.focused = true;
            for ch in ["h", "i"] {
                let ev = ui::synthetic_input::key_event(
                    Key::Character(SmolStr::new(ch)),
                    Some(SmolStr::new(ch)),
                    ElementState::Pressed,
                );
                let event = ui::synthetic_input::keyboard_input(ev);
                assert!(ed.handle_key(
                    &event,
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc)
                ));
            }
            assert_eq!(source(&doc), "");
            assert_eq!(ed.text(), "");
        }
        // Case `goto_line_moves_the_caret_to_the_line_start`.
        {
            let (_doc, mut ed) = showing("one\ntwo\nthree");
            ed.goto_line("", 2);
            assert_eq!(ed.caret(), 4, "line 2 starts just after the first newline");
        }
    }
}
