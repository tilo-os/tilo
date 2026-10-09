//! The source editor pane.
//!
//! The text-editing surface itself is the shared `ui::CodeEditor` — one caret
//! / selection / clipboard / undo / IME / a11y implementation, a line-number
//! gutter, and Typst syntax highlighting supplied by the toolkit's built-in
//! `Language::Typst` highlighter. This module is now a thin shell that owns that
//! widget and adds the Typst-editor-specific pieces: the inline
//! compiler-diagnostic overlay (gutter severity markers, wavy squiggles under
//! error spans, and the hover tooltip), rendered on top of the shared editor
//! using its public geometry.
//!
//! The editor shows the source text of the file the app has open and submits
//! every edit as a splice of it through the file's plain-text child, so a
//! keystroke is an attributed, undoable, permission-checked action.

use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::framework::{Dispatcher, TextControl};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::PlainText;
use ui::widget::Widget;
use ui::widget::code_editor::{CodeEditor, Diagnostic as CodeDiagnostic, Language, Severity};

use crate::compile::{DiagKind, Diagnostic};

/// Tab expands to this many spaces (soft tabs).
const TAB_WIDTH: usize = 2;

pub struct SourceEditor {
    /// The shared code-editing widget — caret, selection, clipboard, undo,
    /// gutter, and Typst syntax highlighting all live here — over the open
    /// file's source.
    code: TextControl<CodeEditor>,
    /// Whether the source pane holds keyboard focus. Mirrored onto `code` each
    /// frame so its caret paints and its key routing engages.
    pub focused: bool,
    rect: Rect,
    /// Anchor byte for an in-flight click+drag selection.
    drag_anchor: Option<usize>,
    /// Last known hover position (window space), or `None` when the pointer is
    /// off the pane. Drives the inline-diagnostic tooltip.
    hover: Option<Point>,
}

impl SourceEditor {
    /// An editor showing the source text `locate` finds, read only until a
    /// dispatcher is [attached](Self::attach).
    pub fn new(locate: impl Fn(&AutoCommit) -> Option<automerge::ObjId> + 'static) -> Self {
        Self {
            code: TextControl::view_only(
                CodeEditor::local()
                    .with_language(Language::Typst)
                    .with_tab_width(TAB_WIDTH),
                locate,
            ),
            focused: true,
            rect: Rect::ZERO,
            drag_anchor: None,
            hover: None,
        }
    }
}

impl SourceEditor {
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.code.control_mut().set_rect(rect);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn line_height(&self) -> f64 {
        self.code.control().line_height()
    }

    /// Submit the edits of the open file's source through `dispatcher`.
    pub fn attach(&mut self, dispatcher: Dispatcher<PlainText>) {
        self.code.attach(dispatcher);
    }

    /// Show the open file's source as `doc` holds it. Called from the app's
    /// `sync`.
    pub fn pull(&mut self, doc: &AutoCommit) {
        self.code.pull(doc);
    }

    /// The caret's byte offset in the source.
    pub fn caret(&self) -> usize {
        self.code.control().cursor_pos()
    }

    /// Move the caret to a byte offset (used by the screenshot harness).
    pub fn set_caret(&mut self, byte: usize) {
        self.code.control_mut().set_cursor_pos(byte);
    }

    /// The current buffer text (mirrors the bound Automerge object).
    pub fn text(&self) -> &str {
        self.code.control().value()
    }

    /// Programmatically set the buffer (local/unbound use — tests, seeding).
    pub fn set_text(&mut self, s: &str) {
        self.code.control_mut().set_text(s);
    }

    /// Reset the caret to the document start (used when switching documents).
    pub fn reset_cursor(&mut self) {
        self.code.control_mut().set_cursor_pos(0);
        self.code.control_mut().scroll_by(-1.0e9);
    }

    /// Screenshot harness only: select a couple of source lines so a capture
    /// shows a real selection band over highlighted code.
    pub fn demo_select(&mut self) {
        let text = self.code.control_mut().value().to_string();
        let start = byte_of_line_col(&text, 1, 0);
        let end = byte_of_line_col(&text, 3, 0).min(text.len());
        let (a, b) = if end > start {
            (start, end)
        } else {
            (0, text.len().min(24))
        };
        self.code.control_mut().set_selection(a, b);
        self.focused = true;
    }

    /// Place the caret on a specific 1-based line (used when a diagnostic is
    /// clicked). `_text` is unused — the widget holds the source.
    pub fn goto_line(&mut self, _text: &str, line_1based: usize) {
        self.code.control_mut().goto_line(line_1based);
    }

    // ----- scrolling -------------------------------------------------------

    /// Wheel scroll. `dx`/`_text` are unused — the code view soft-wraps, so it
    /// only scrolls vertically.
    pub fn scroll_by(&mut self, _dx: f32, dy: f32, _text: &str) {
        self.code.control_mut().scroll_by(f64::from(dy));
    }

    /// Scroll the caret into view. The shared widget follows the caret after
    /// every edit automatically, so this is a no-op kept for call-site parity.
    pub fn ensure_visible(&mut self, _text: &str) {}

    // ----- mouse -----------------------------------------------------------

    pub fn on_mouse_down(&mut self, p: Point, shift: bool, _text: &str) {
        self.focused = true;
        self.code.control_mut().set_focused(true);
        let byte = self.code.control_mut().byte_at_point(p);
        if shift {
            let anchor = self
                .drag_anchor
                .unwrap_or_else(|| self.code.control_mut().cursor_pos());
            self.code.control_mut().set_selection(anchor, byte);
            self.drag_anchor = Some(anchor);
        } else {
            self.code.control_mut().set_cursor_pos(byte);
            self.drag_anchor = Some(byte);
        }
    }

    pub fn on_mouse_drag(&mut self, p: Point, _text: &str) {
        if let Some(anchor) = self.drag_anchor {
            let byte = self.code.control_mut().byte_at_point(p);
            self.code.control_mut().set_selection(anchor, byte);
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
        self.code.control_mut().set_focused(self.focused);
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy.new_leaf(Style::default()).unwrap();
        self.code.control_mut().handle_event(
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
        self.code.submit(doc);
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
    ) {
        self.code.control_mut().set_focused(self.focused);
        self.code.control_mut().set_rect(self.rect);
        let text = self.code.control().value().to_string();
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
        self.code.control_mut().set_diagnostics(spans);
        self.code
            .control_mut()
            .set_pointer(self.hover.filter(|p| self.rect.contains(*p)));
        self.code
            .control_mut()
            .draw_embedded(scene, transform, tr, theme);
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
        self.code
            .control()
            .draw_diagnostic_card(scene, transform, tr, theme, window);
    }

    /// The caret is a steady bar, so a focused-but-idle editor needs no
    /// animation frames.
    pub fn wants_animation(&self) -> bool {
        false
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
        d.end_col.unwrap_or(d.col? + 1).saturating_sub(1),
    );
    let b0 = byte_of_line_col(text, sl, sc);
    let b1 = byte_of_line_col(text, el, ec)
        .max(b0 + 1)
        .min(text.len().max(b0 + 1));
    Some((b0, b1.min(text.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};

    fn doc_with(text: &str) -> (AutoCommit, automerge::ObjId) {
        let mut doc = AutoCommit::new();
        let id = doc.put_object(&ROOT, "source", ObjType::Text).unwrap();
        doc.splice_text(&id, 0, 0, text).unwrap();
        (doc, id)
    }

    /// An editor over the text `id`.
    fn editor_on(id: &automerge::ObjId) -> SourceEditor {
        let id = id.clone();
        SourceEditor::new(move |_| Some(id.clone()))
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
            let mut ed = SourceEditor::new(|_| None);
            ed.set_text("a\nbcdefg\n");
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            ed.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &[warn, err],
            );
            let severities: Vec<Severity> = ed
                .code
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
    fn editor_input_without_a_dispatcher_and_caret_moves() {
        // Case `typing_without_a_dispatcher_changes_nothing`:
        // An editor nobody attached a dispatcher to only shows the text; the
        // keys it takes are taken back.
        {
            use winit::event::ElementState;
            use winit::keyboard::{Key, SmolStr};
            let (mut doc, id) = doc_with("");
            let mut ed = editor_on(&id);
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            ed.pull(&doc);
            ed.focused = true;
            for ch in ["h", "i"] {
                let ev = ui::synthetic_input::key_event(
                    Key::Character(SmolStr::new(ch)),
                    Some(SmolStr::new(ch)),
                    ElementState::Pressed,
                );
                let event = WindowEvent::KeyboardInput {
                    device_id: winit::event::DeviceId::dummy(),
                    event: ev,
                    is_synthetic: false,
                };
                assert!(ed.handle_key(
                    &event,
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc)
                ));
            }
            assert_eq!(doc.text(&id).unwrap(), "");
            assert_eq!(ed.text(), "");
        }
        // Case `goto_line_moves_the_caret_to_the_line_start`.
        {
            let (doc, id) = doc_with("one\ntwo\nthree");
            let mut ed = editor_on(&id);
            ed.pull(&doc);
            ed.goto_line("", 2);
            assert_eq!(ed.caret(), 4, "line 2 starts just after the first newline");
        }
    }
}
