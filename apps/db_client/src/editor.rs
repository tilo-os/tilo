//! Multi-line SQL editor backed by the shared CRDT buffer.
//!
//! The text itself lives in the Automerge document (see [`crate::doc`]); this
//! widget holds only the caret and render caches. Each keystroke splices into
//! the CRDT `Text` object so undo/redo and live collaboration come for free.
//! Syntax highlighting is computed per-frame from the buffer via
//! [`crate::syntax`].
//!
//! `Ctrl/Cmd+Enter` is surfaced to the host through [`SqlEditor::take_run`] —
//! the app polls it after dispatching events and runs the query.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use automerge::AutoCommit;

use crate::doc::EditorDoc;
use crate::syntax::{self, Tok};
use crate::theme_ext::ThemeExt;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Vertical advance per line, in logical px.
const LINE_HEIGHT: f64 = 20.0;
/// Editor font size.
const FONT_SIZE: f32 = 13.5;
/// Left padding before the gutter.
const GUTTER_W: f64 = 44.0;
/// Padding inside the text body.
const PAD: f64 = 10.0;

/// The SQL editor. Drawn by the app inside a caller-provided rect.
pub struct SqlEditor {
    /// Caret as a char index into the buffer.
    caret: usize,
    /// Cached buffer string, refreshed from the CRDT each `sync`.
    buffer: String,
    /// The rect the editor occupies (set by the app before draw/event).
    rect: Rect,
    /// Whether the editor has keyboard focus.
    focused: bool,
    /// One-shot flag: set when Ctrl/Cmd+Enter is pressed; consumed by the app.
    run_requested: bool,
    /// Vertical scroll offset in lines.
    scroll_line: usize,
    /// Whether a buffer mutation happened this frame (so the app re-syncs).
    mutated: bool,
    /// Last observed cursor position (for click-to-place-caret).
    last_cursor: Point,
}

impl Default for SqlEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl SqlEditor {
    pub fn new() -> Self {
        Self {
            caret: 0,
            buffer: String::new(),
            rect: Rect::ZERO,
            focused: true,
            run_requested: false,
            scroll_line: 0,
            mutated: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Pull the buffer from the CRDT. Called by the app each frame before
    /// layout/draw so a collaborator's edits surface.
    pub fn sync(&mut self, doc: &AutoCommit) {
        let s = EditorDoc::read_sql(doc);
        if s != self.buffer {
            self.buffer = s;
            self.clamp_caret();
        }
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn set_focused(&mut self, f: bool) {
        self.focused = f;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Returns true (once) if the user asked to run the query.
    pub fn take_run(&mut self) -> bool {
        std::mem::take(&mut self.run_requested)
    }

    /// Returns true (once) if the buffer changed this frame.
    pub fn take_mutated(&mut self) -> bool {
        std::mem::take(&mut self.mutated)
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    fn clamp_caret(&mut self) {
        let len = self.buffer.chars().count();
        if self.caret > len {
            self.caret = len;
        }
    }

    /// (line, column) of the caret in char units.
    fn caret_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for (i, ch) in self.buffer.chars().enumerate() {
            if i == self.caret {
                return (line, col);
            }
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn line_count(&self) -> usize {
        self.buffer.chars().filter(|c| *c == '\n').count() + 1
    }

    /// Char index at the start of `line`.
    fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut seen = 0;
        for (i, ch) in self.buffer.chars().enumerate() {
            if ch == '\n' {
                seen += 1;
                if seen == line {
                    return i + 1;
                }
            }
        }
        self.buffer.chars().count()
    }

    /// Char length of `line` (excluding the newline).
    fn line_len(&self, line: usize) -> usize {
        let start = self.line_start(line);
        let mut len = 0;
        for ch in self.buffer.chars().skip(start) {
            if ch == '\n' {
                break;
            }
            len += 1;
        }
        len
    }

    fn move_caret_to(&mut self, line: usize, col: usize) {
        let line = line.min(self.line_count().saturating_sub(1));
        let col = col.min(self.line_len(line));
        self.caret = self.line_start(line) + col;
        self.ensure_caret_visible();
    }

    fn visible_lines(&self) -> usize {
        ((self.rect.height() - 2.0 * PAD) / LINE_HEIGHT)
            .floor()
            .max(1.0) as usize
    }

    fn ensure_caret_visible(&mut self) {
        let (line, _) = self.caret_line_col();
        let vis = self.visible_lines();
        if line < self.scroll_line {
            self.scroll_line = line;
        } else if line >= self.scroll_line + vis {
            self.scroll_line = line + 1 - vis;
        }
    }

    /// Handle a key/mouse event, mutating the CRDT directly.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Focus on click inside the editor; the app sets the rect.
                // Caret placement on click is handled in CursorMoved cache;
                // here we just take focus if the last cursor was inside.
                if self.rect.contains(self.last_cursor) {
                    self.focused = true;
                    self.place_caret_at(self.last_cursor);
                    *handled = true;
                } else {
                    self.focused = false;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::KeyboardInput {
                event:
                    key_event @ KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key_event(key_event, modifiers, doc, handled);
            }
            _ => {}
        }
    }

    fn handle_key_event(
        &mut self,
        key_event: &KeyEvent,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if !self.focused {
            return;
        }
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let text = &key_event.text;
        let logical_key = &key_event.logical_key;
        if self.handle_navigation_key(logical_key) {
            *handled = true;
            return;
        }
        match logical_key {
            Key::Named(NamedKey::Enter) => {
                if ctrl {
                    self.run_requested = true;
                } else {
                    self.caret = EditorDoc::insert(doc, self.caret, "\n");
                    self.buffer = EditorDoc::read_sql(doc);
                    self.mutated = true;
                    self.ensure_caret_visible();
                }
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                if self.caret > 0 {
                    self.caret = EditorDoc::delete(doc, self.caret - 1, 1);
                    self.buffer = EditorDoc::read_sql(doc);
                    self.mutated = true;
                    self.ensure_caret_visible();
                }
                *handled = true;
            }
            Key::Named(NamedKey::Delete) => {
                let len = self.buffer.chars().count();
                if self.caret < len {
                    self.caret = EditorDoc::delete(doc, self.caret, 1);
                    self.buffer = EditorDoc::read_sql(doc);
                    self.mutated = true;
                }
                *handled = true;
            }
            Key::Named(NamedKey::Tab) => {
                self.caret = EditorDoc::insert(doc, self.caret, "  ");
                self.buffer = EditorDoc::read_sql(doc);
                self.mutated = true;
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                self.caret = EditorDoc::insert(doc, self.caret, " ");
                self.buffer = EditorDoc::read_sql(doc);
                self.mutated = true;
                *handled = true;
            }
            Key::Character(_) => {
                if let Some(t) = text
                    && !ctrl
                {
                    self.caret = EditorDoc::insert(doc, self.caret, t.as_str());
                    self.buffer = EditorDoc::read_sql(doc);
                    self.mutated = true;
                    self.ensure_caret_visible();
                    *handled = true;
                }
            }
            _ => {
                // Some layouts deliver printable text without a Character
                // key; honour `text` as a fallback.
                if let Some(t) = text
                    && !ctrl
                    && !t.is_empty()
                {
                    self.caret = EditorDoc::insert(doc, self.caret, t.as_str());
                    self.buffer = EditorDoc::read_sql(doc);
                    self.mutated = true;
                    self.ensure_caret_visible();
                    *handled = true;
                }
            }
        }
    }

    /// Caret-movement keys, which never touch the document. Returns whether
    /// `logical_key` was one of these (and so has already been handled).
    fn handle_navigation_key(&mut self, logical_key: &Key) -> bool {
        match logical_key {
            Key::Named(NamedKey::ArrowLeft) => {
                if self.caret > 0 {
                    self.caret -= 1;
                }
                self.ensure_caret_visible();
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.caret < self.buffer.chars().count() {
                    self.caret += 1;
                }
                self.ensure_caret_visible();
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                let (l, c) = self.caret_line_col();
                if l > 0 {
                    self.move_caret_to(l - 1, c);
                }
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                let (l, c) = self.caret_line_col();
                self.move_caret_to(l + 1, c);
                true
            }
            Key::Named(NamedKey::Home) => {
                let (l, _) = self.caret_line_col();
                self.move_caret_to(l, 0);
                true
            }
            Key::Named(NamedKey::End) => {
                let (l, _) = self.caret_line_col();
                self.move_caret_to(l, self.line_len(l));
                true
            }
            _ => false,
        }
    }

    fn place_caret_at(&mut self, p: Point) {
        let body_x = self.rect.x0 + GUTTER_W + PAD;
        let body_y = self.rect.y0 + PAD;
        let line =
            (((p.y - body_y) / LINE_HEIGHT).floor() as isize).max(0) as usize + self.scroll_line;
        // Approximate column from a monospace advance.
        let char_w = f64::from(FONT_SIZE) * 0.6;
        let col = (((p.x - body_x) / char_w).round() as isize).max(0) as usize;
        self.move_caret_to(line, col);
    }

    /// Paint the editor into `scene`.
    pub fn draw(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let p = theme.db();

        // Body background + border.
        scene.fill(Fill::NonZero, transform, p.editor_bg, None, &self.rect);

        // Gutter background.
        let gutter = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + GUTTER_W,
            self.rect.y1,
        );
        scene.fill(Fill::NonZero, transform, p.gutter_bg, None, &gutter);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &Line::new(
                Point::new(gutter.x1, self.rect.y0),
                Point::new(gutter.x1, self.rect.y1),
            ),
        );

        let lines: Vec<&str> = self.buffer.split('\n').collect();
        let total = lines.len();
        let vis = self.visible_lines();
        let char_w = f64::from(FONT_SIZE) * 0.6;
        let body_x = self.rect.x0 + GUTTER_W + PAD;

        let (caret_line, caret_col) = self.caret_line_col();

        for vi in 0..vis {
            let li = self.scroll_line + vi;
            if li >= total {
                break;
            }
            let y = self.rect.y0 + PAD + vi as f64 * LINE_HEIGHT;

            // Line number, right-aligned in the gutter.
            let num = (li + 1).to_string();
            let num_w = num.chars().count() as f64 * (f64::from(FONT_SIZE) * 0.55);
            let num_color = if li == caret_line {
                theme.text_primary
            } else {
                p.gutter_fg
            };
            tr.draw_text(
                scene,
                &num,
                FONT_SIZE - 1.0,
                num_color,
                transform * Affine::translate((gutter.x1 - PAD * 0.6 - num_w, y + 2.0)),
                None,
            );

            // Current-line highlight.
            if li == caret_line && self.focused {
                let hl = Rect::new(body_x - PAD, y, self.rect.x1, y + LINE_HEIGHT);
                scene.fill(Fill::NonZero, transform, p.current_line, None, &hl);
            }

            // Tokenize this line and paint each span in its colour.
            let mut x = body_x;
            let Some(&text) = lines.get(li) else {
                continue;
            };
            for span in syntax::tokenize(text) {
                let color = token_color(span.kind, theme, &p);
                let w = span.text.chars().count() as f64 * char_w;
                if !span.text.trim().is_empty() {
                    tr.draw_text(
                        scene,
                        &span.text,
                        FONT_SIZE,
                        color,
                        transform * Affine::translate((x, y + 2.0)),
                        None,
                    );
                }
                x += w;
            }

            // Caret.
            if li == caret_line && self.focused {
                let cx = body_x + caret_col as f64 * char_w;
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    theme.accent_primary,
                    None,
                    &Line::new(
                        Point::new(cx, y + 1.0),
                        Point::new(cx, y + LINE_HEIGHT - 1.0),
                    ),
                );
            }
        }

        // Scroll indicator if the buffer overflows.
        if total > vis {
            let track_h = self.rect.height() - 2.0 * PAD;
            let thumb_h = (vis as f64 / total as f64 * track_h).max(20.0);
            let thumb_y = self.rect.y0 + PAD + (self.scroll_line as f64 / total as f64) * track_h;
            let thumb = Rect::new(
                self.rect.x1 - 5.0,
                thumb_y,
                self.rect.x1 - 2.0,
                (thumb_y + thumb_h).min(self.rect.y1 - PAD),
            )
            .to_rounded_rect(2.0);
            scene.fill(Fill::NonZero, transform, p.gutter_fg, None, &thumb);
        }
    }
}

fn token_color(kind: Tok, theme: &Theme, p: &crate::theme_ext::DbPalette) -> Color {
    match kind {
        Tok::Keyword => p.kw,
        Tok::Function => p.func,
        Tok::Type => p.ty,
        Tok::Number => p.num,
        Tok::String => p.str_lit,
        Tok::Comment => p.comment,
        Tok::Operator => p.op,
        Tok::Punct => theme.text_dim,
        Tok::Identifier | Tok::Whitespace => theme.text_primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> (SqlEditor, AutoCommit) {
        let mut doc = AutoCommit::new();
        EditorDoc::set_sql(&mut doc, "SELECT 1\nFROM t");
        let mut ed = SqlEditor::new();
        ed.sync(&doc);
        (ed, doc)
    }

    #[test]
    fn caret_line_col_tracks_newlines() {
        let (mut ed, _doc) = seeded();
        ed.caret = 9; // start of second line ("FROM t")
        assert_eq!(ed.caret_line_col(), (1, 0));
    }

    #[test]
    fn line_helpers_match_buffer() {
        let (ed, _doc) = seeded();
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.line_len(0), 8); // "SELECT 1"
        assert_eq!(ed.line_start(1), 9);
    }
}
