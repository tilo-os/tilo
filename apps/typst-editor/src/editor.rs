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
//! The source text lives in the Automerge `Text` object the editor is bound to;
//! every edit splices into it through the shared widget.

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{Modifiers, WindowEvent};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use ui::widget::code_editor::{CodeEditor, Language};

use crate::compile::{DiagKind, Diagnostic};

/// Tab expands to this many spaces (soft tabs).
const TAB_WIDTH: usize = 2;

pub struct SourceEditor {
    /// The shared code-editing widget — caret, selection, clipboard, undo,
    /// gutter, and Typst syntax highlighting all live here.
    code: CodeEditor,
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

impl Default for SourceEditor {
    fn default() -> Self {
        Self {
            code: CodeEditor::local()
                .with_language(Language::Typst)
                .with_tab_width(TAB_WIDTH),
            focused: true,
            rect: Rect::ZERO,
            drag_anchor: None,
            hover: None,
        }
    }
}

impl SourceEditor {
    #[cfg(test)]
    pub(crate) fn history_test_caret_rect(&self, theme: &Theme, byte: usize) -> Rect {
        self.code.caret_rect(theme, byte)
    }

    /// Unbound source presentation with no caret and persistent local scrolling.
    pub(crate) fn snapshot(source: &str) -> Self {
        let mut editor = Self::new();
        editor.focused = false;
        editor.code.inner_mut().set_preserve_unfocused_scroll(true);
        editor.set_text(source);
        editor
    }

    /// Prepare immutable source geometry using the widget's normal cached
    /// measurement, including its renderer font/locale epoch invalidation.
    pub(crate) fn prepare_snapshot_layout(&mut self, tr: &mut TextRenderer) {
        Widget::measure(
            &mut self.code,
            taffy::Size {
                width: Some(self.rect.width() as f32),
                height: Some(self.rect.height() as f32),
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(self.rect.width() as f32),
                height: taffy::AvailableSpace::Definite(self.rect.height() as f32),
            },
            tr,
        );
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.code.set_rect(rect);
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn line_height(&self) -> f64 {
        self.code.line_height()
    }

    /// Bind the editor to the Automerge `Text` object holding the source and
    /// pull its current content. Called from the app's `sync`.
    pub fn bind(&mut self, obj: ObjId, doc: &mut AutoCommit) {
        self.code.bind_text(obj);
        self.code.pull(doc);
    }

    /// The caret's byte offset in the source.
    pub fn caret(&self) -> usize {
        self.code.cursor_pos()
    }

    /// Move the caret to a byte offset (used by the screenshot harness).
    pub fn set_caret(&mut self, byte: usize) {
        self.code.set_cursor_pos(byte);
    }

    /// The current buffer text (mirrors the bound Automerge object).
    pub fn text(&self) -> &str {
        self.code.value()
    }

    /// Programmatically set the buffer (local/unbound use — tests, seeding).
    pub fn set_text(&mut self, s: &str) {
        self.code.set_text(s);
    }

    /// Reset the caret to the document start (used when switching documents).
    pub fn reset_cursor(&mut self) {
        self.code.set_cursor_pos(0);
        self.code.scroll_by(-1.0e9);
    }

    /// Screenshot harness only: select a couple of source lines so a capture
    /// shows a real selection band over highlighted code.
    pub fn demo_select(&mut self) {
        let text = self.code.value().to_string();
        let start = byte_of_line_col(&text, 1, 0);
        let end = byte_of_line_col(&text, 3, 0).min(text.len());
        let (a, b) = if end > start {
            (start, end)
        } else {
            (0, text.len().min(24))
        };
        self.code.set_selection(a, b);
        self.focused = true;
    }

    /// Place the caret on a specific 1-based line (used when a diagnostic is
    /// clicked). `_text` is unused — the widget holds the source.
    pub fn goto_line(&mut self, _text: &str, line_1based: usize) {
        self.code.goto_line(line_1based);
    }

    // ----- scrolling -------------------------------------------------------

    /// Effective bounded offset for immutable History view persistence.
    pub(crate) fn snapshot_scroll_y(&self) -> f64 {
        self.code.inner().scroll_y()
    }

    /// Wheel scroll. `dx`/`_text` are unused — the code view soft-wraps, so it
    /// only scrolls vertically.
    pub fn scroll_by(&mut self, _dx: f32, dy: f32, _text: &str) {
        self.code.scroll_by(f64::from(dy));
    }

    /// Scroll the caret into view. The shared widget follows the caret after
    /// every edit automatically, so this is a no-op kept for call-site parity.
    pub fn ensure_visible(&mut self, _text: &str) {}

    // ----- mouse -----------------------------------------------------------

    pub fn on_mouse_down(&mut self, p: Point, shift: bool, _text: &str) {
        self.focused = true;
        self.code.set_focused(true);
        let byte = self.code.byte_at_point(p);
        if shift {
            let anchor = self.drag_anchor.unwrap_or_else(|| self.code.cursor_pos());
            self.code.set_selection(anchor, byte);
            self.drag_anchor = Some(anchor);
        } else {
            self.code.set_cursor_pos(byte);
            self.drag_anchor = Some(byte);
        }
    }

    pub fn on_mouse_drag(&mut self, p: Point, _text: &str) {
        if let Some(anchor) = self.drag_anchor {
            let byte = self.code.byte_at_point(p);
            self.code.set_selection(anchor, byte);
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

    /// Handle a keyboard event through the shared widget. Returns true if
    /// consumed. `text_id` is the bound source object; the widget is already
    /// bound to it, so edits splice there.
    ///
    /// # Panics
    /// Never panics: `taffy::TaffyTree::new_leaf` cannot return `Err` for a
    /// freshly created, single-node tree on this taffy version (see
    /// `ui::widget::TaffyTreeExt`'s doc comment for the same guarantee).
    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        text_id: &ObjId,
    ) -> bool {
        use taffy::{Style, TaffyTree};
        self.code.set_focused(self.focused);
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy.new_leaf(Style::default()).unwrap();
        // `text_id` is the object the widget was bound to; pass it as the root so
        // the resolve falls back to it even if the binding were ever cleared.
        self.code.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers,
                state: doc,
                path: text_id,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    // ----- rendering -------------------------------------------------------

    /// Paint the pane. The editor stands on the window ground: it fills
    /// nothing of its own.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        diags: &[Diagnostic],
    ) {
        // The shared widget paints the gutter, line numbers, current-line band,
        // selection, highlighted text, and caret.
        self.code.set_focused(self.focused);
        self.code.set_rect(self.rect);
        self.code.draw_embedded(scene, transform, tr, theme);

        // Diagnostics overlay, drawn on top using the widget's geometry, clipped
        // to the pane so squiggles/markers never spill past it.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        self.draw_diagnostics(scene, transform, theme, diags);
        scene.pop_layer();
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
        diags: &[Diagnostic],
    ) {
        let Some(hp) = self.hover.filter(|p| self.rect.contains(*p)) else {
            return;
        };
        let byte = self.code.byte_at_point(hp);
        let text = self.code.value().to_string();
        if let Some(d) = diags.iter().find(|d| {
            diag_byte_span(d, &text).is_some_and(|(b0, b1)| byte >= b0 && byte < b1.max(b0 + 1))
        }) {
            self.paint_tooltip(scene, transform, tr, theme, hp, d);
        }
    }

    /// Gutter severity markers + inline squiggles, positioned from the shared
    /// widget's caret / selection geometry so they track scroll, wrap, and the
    /// gutter width exactly.
    fn draw_diagnostics(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        diags: &[Diagnostic],
    ) {
        if diags.is_empty() {
            return;
        }
        let text = self.code.value().to_string();
        let lh = self.code.line_height();
        let unit = f64::from(theme.tokens.spacing.s1);

        // Gutter severity dot on each line a diagnostic starts on.
        for (line0, kind) in worst_severity_by_line(diags) {
            let byte = byte_of_line_col(&text, line0, 0);
            let cr = self.code.caret_rect(theme, byte);
            if cr.y1 < self.rect.y0 || cr.y0 > self.rect.y1 {
                continue;
            }
            let cx = self.rect.x0 + 2.0 * unit;
            let cy = cr.y0 + lh * 0.5;
            scene.fill(
                Fill::NonZero,
                transform,
                diag_color(kind, theme),
                None,
                &Circle::new(Point::new(cx, cy), unit * 0.75),
            );
        }

        // Inline squiggle under every diagnostic span, one segment per visual row.
        for d in diags {
            if let Some((b0, b1)) = diag_byte_span(d, &text) {
                for r in self.code.selection_rects(theme, b0, b1) {
                    let yb = r.y1 - unit * 0.9;
                    draw_squiggle(
                        scene,
                        transform,
                        (r.x0, r.x1, yb),
                        unit,
                        diag_color(d.kind, theme),
                    );
                }
            }
        }
    }

    /// Draw a floating card near `at` carrying a diagnostic's message (and
    /// its first hint, dimmed). Sizing is measured from the text so the card
    /// hugs the content.
    fn paint_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        at: Point,
        d: &Diagnostic,
    ) {
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s3);
        let gap = f64::from(spacing.s1);
        let max_text_w = f64::from(theme.tokens.metrics.panel_width_list) - 2.0 * pad;
        let message_style = theme.text(TextRole::Label);
        let hint_style = theme.text(TextRole::Caption);
        let mut lines = vec![(d.message.clone(), message_style, false)];
        if let Some(h) = d.hints.first() {
            lines.push((h.clone(), hint_style, true));
        }
        let mut w = 0.0f64;
        let mut total_h = 0.0f64;
        let mut heights = Vec::with_capacity(lines.len());
        for (s, style, _) in &lines {
            let (tw, th) = tr.measure_style(s, Some(ui::num::f32_of_f64(max_text_w)), style);
            w = w.max(f64::from(tw));
            heights.push(f64::from(th));
            total_h += f64::from(th) + gap;
        }
        total_h -= gap;
        let card_w = w + pad * 2.0;
        let card_h = total_h + pad * 2.0;
        let (below, side) = (f64::from(spacing.s4), f64::from(spacing.s3));
        let mut x0 = at.x + side;
        let mut y0 = at.y + below + f64::from(spacing.s1);
        if x0 + card_w > self.rect.x1 - gap {
            x0 = (self.rect.x1 - gap - card_w).max(self.rect.x0 + gap);
        }
        if y0 + card_h > self.rect.y1 - gap {
            y0 = (at.y - card_h - f64::from(spacing.s2)).max(self.rect.y0 + gap);
        }
        let card = Rect::new(x0, y0, x0 + card_w, y0 + card_h);
        ui::widget::floating_surface::paint(scene, transform, card, theme);
        let on_card = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut ty = y0 + pad;
        for ((s, style, dim), h) in lines.iter().zip(heights.iter()) {
            let color = if *dim {
                on_card.text_dim
            } else {
                on_card.text_primary
            };
            tr.draw_style(
                scene,
                s,
                color,
                transform * Affine::translate((x0 + pad, ty)),
                Some(ui::num::f32_of_f64(card_w - pad * 2.0)),
                style,
            );
            ty += h + gap;
        }
    }

    /// The caret is a steady bar, so a focused-but-idle editor needs no
    /// animation frames.
    pub fn wants_animation(&self) -> bool {
        false
    }
}

/// The theme colour a diagnostic severity draws in.
fn diag_color(kind: DiagKind, theme: &Theme) -> vello::peniko::Color {
    match kind {
        DiagKind::Error => theme.tokens.colors.semantic.danger,
        DiagKind::Warning => theme.tokens.colors.semantic.warning,
    }
}

/// Map each 0-based source line that a diagnostic *starts* on to the worst
/// severity present there (Error beats Warning), for the gutter markers.
fn worst_severity_by_line(diags: &[Diagnostic]) -> std::collections::HashMap<usize, DiagKind> {
    let mut map: std::collections::HashMap<usize, DiagKind> = std::collections::HashMap::new();
    for d in diags {
        let Some(line1) = d.line else { continue };
        let line = line1.saturating_sub(1);
        map.entry(line)
            .and_modify(|k| {
                if d.kind == DiagKind::Error {
                    *k = DiagKind::Error;
                }
            })
            .or_insert(d.kind);
    }
    map
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

/// Stroke a wavy underline (a squiggle) from `x0` to `x1` at baseline `y`
/// (`span` is `(x0, x1, y)`), its wave `unit` wide, in `color`.
fn draw_squiggle(
    scene: &mut Scene,
    transform: Affine,
    span: (f64, f64, f64),
    unit: f64,
    color: vello::peniko::Color,
) {
    let (x0, x1, y) = span;
    let period = unit;
    let amplitude = unit * 0.4;
    if x1 <= x0 {
        return;
    }
    let mut path = BezPath::new();
    path.move_to(Point::new(x0, y));
    let mut x = x0;
    let mut up = true;
    loop {
        if x >= x1 {
            break;
        }
        x = (x + period * 0.5).min(x1);
        let dy = if up { -amplitude } else { amplitude };
        path.line_to(Point::new(x, y + dy));
        up = !up;
    }
    scene.stroke(&Stroke::new(unit * 0.32), transform, color, None, &path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};

    fn doc_with(text: &str) -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let id = doc.put_object(&ROOT, "source", ObjType::Text).unwrap();
        doc.splice_text(&id, 0, 0, text).unwrap();
        (doc, id)
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
            let map = worst_severity_by_line(&[warn, err]);
            assert_eq!(map.get(&1), Some(&DiagKind::Error));
        }
    }

    #[test]
    fn editor_input_edits_document_and_moves_caret() {
        // Case `typing_edits_the_bound_document`:
        // Typing through the shell edits the bound Automerge object via the shared
        // widget's real key path.
        {
            use winit::event::{ElementState, KeyEvent};
            use winit::keyboard::{Key, SmolStr};
            let (mut doc, id) = doc_with("");
            let mut ed = SourceEditor::new();
            ed.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            ed.bind(id.clone(), &mut doc);
            ed.focused = true;
            for ch in ["h", "i"] {
                let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
                ev.logical_key = Key::Character(SmolStr::new(ch));
                ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                );
                ev.location = winit::keyboard::KeyLocation::Standard;
                ev.state = ElementState::Pressed;
                ev.repeat = false;
                ev.text = Some(SmolStr::new(ch));
                let event = WindowEvent::KeyboardInput {
                    device_id: unsafe { std::mem::zeroed() },
                    event: ev,
                    is_synthetic: false,
                };
                assert!(ed.handle_key(&event, &Modifiers::default(), &mut doc, &id));
            }
            assert_eq!(doc.text(&id).unwrap(), "hi");
        }
        // Case `goto_line_moves_the_caret_to_the_line_start`.
        {
            let (mut doc, id) = doc_with("one\ntwo\nthree");
            let mut ed = SourceEditor::new();
            ed.bind(id, &mut doc);
            ed.goto_line("", 2);
            assert_eq!(ed.caret(), 4, "line 2 starts just after the first newline");
        }
    }
}
