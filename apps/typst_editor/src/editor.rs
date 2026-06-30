//! The source editor pane: a monospace, line-numbered, syntax-highlighted
//! text view that edits an Automerge `Text` object directly.
//!
//! State that belongs in the CRDT (the source text) lives there; this struct
//! holds only ephemeral view state — caret, selection anchor, scroll offset,
//! and a tiny internal clipboard. Caret/anchor are **char indices** into the
//! text, which is exactly the unit Automerge's `splice_text` uses, so edits
//! and cursor maths stay in one coordinate system.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::highlight::{self, Token};

/// Monospace point size of the editor text.
pub const FONT_SIZE: f32 = 13.0;
/// Tab expands to this many spaces.
const TAB_WIDTH: usize = 2;

pub struct SourceEditor {
    pub caret: usize,
    pub anchor: usize,
    pub scroll_y: f32,
    pub focused: bool,
    rect: Rect,
    /// Cached glyph cell metrics, refreshed each `draw`.
    char_w: f64,
    line_h: f64,
    /// Preferred column for vertical caret motion (sticky across up/down).
    desired_col: Option<usize>,
    /// Internal clipboard for Ctrl+C/X/V (no system clipboard dependency).
    clipboard: String,
    blink_start: std::time::Instant,
}

impl Default for SourceEditor {
    fn default() -> Self {
        Self {
            caret: 0,
            anchor: 0,
            scroll_y: 0.0,
            focused: true,
            rect: Rect::ZERO,
            char_w: FONT_SIZE as f64 * 0.6,
            line_h: (FONT_SIZE as f64 * 1.5).round(),
            desired_col: None,
            clipboard: String::new(),
            blink_start: std::time::Instant::now(),
        }
    }
}

impl SourceEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn line_height(&self) -> f64 {
        self.line_h
    }

    fn selection(&self) -> Option<(usize, usize)> {
        if self.caret == self.anchor {
            None
        } else {
            Some((self.caret.min(self.anchor), self.caret.max(self.anchor)))
        }
    }

    fn bump_blink(&mut self) {
        self.blink_start = std::time::Instant::now();
    }

    /// Place the caret on a specific 1-based line (used when a diagnostic is
    /// clicked). Scrolls the line into view on the next draw.
    pub fn goto_line(&mut self, text: &str, line_1based: usize) {
        let idx = LineIndex::new(text);
        let line = line_1based.saturating_sub(1).min(idx.line_count().saturating_sub(1));
        let caret = idx.line_start(line);
        self.caret = caret;
        self.anchor = caret;
        self.desired_col = None;
        // Centre the target line in the viewport.
        let target = line as f64 * self.line_h - self.rect.height() * 0.4;
        self.scroll_y = target.max(0.0) as f32;
        self.bump_blink();
    }

    // ----- scrolling -------------------------------------------------------

    pub fn scroll_by(&mut self, dy: f32, text: &str) {
        self.scroll_y += dy;
        self.clamp_scroll(text);
    }

    fn clamp_scroll(&mut self, text: &str) {
        let idx = LineIndex::new(text);
        let content_h = idx.line_count() as f64 * self.line_h + self.line_h;
        let max = (content_h - self.rect.height()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max as f32);
    }

    fn ensure_caret_visible(&mut self, text: &str) {
        let idx = LineIndex::new(text);
        let (line, _) = idx.line_col(self.caret);
        let caret_top = line as f64 * self.line_h;
        let caret_bot = caret_top + self.line_h;
        let view_top = self.scroll_y as f64;
        let view_bot = view_top + self.rect.height();
        if caret_top < view_top {
            self.scroll_y = caret_top as f32;
        } else if caret_bot > view_bot {
            self.scroll_y = (caret_bot - self.rect.height()) as f32;
        }
        self.clamp_scroll(text);
    }

    // ----- mouse -----------------------------------------------------------

    /// Map a window-space point to a caret char index.
    fn caret_at_point(&self, p: Point, text: &str) -> usize {
        let idx = LineIndex::new(text);
        let gutter = self.gutter_width(idx.line_count());
        let rel_y = p.y - self.rect.y0 + self.scroll_y as f64;
        let line = (rel_y / self.line_h).floor().max(0.0) as usize;
        let line = line.min(idx.line_count().saturating_sub(1));
        let content_x = self.rect.x0 + gutter;
        let col = (((p.x - content_x) / self.char_w).round()).max(0.0) as usize;
        let col = col.min(idx.line_len(line));
        idx.line_start(line) + col
    }

    pub fn on_mouse_down(&mut self, p: Point, shift: bool, text: &str) {
        self.focused = true;
        let c = self.caret_at_point(p, text);
        self.caret = c;
        if !shift {
            self.anchor = c;
        }
        self.desired_col = None;
        self.bump_blink();
    }

    pub fn on_mouse_drag(&mut self, p: Point, text: &str) {
        let c = self.caret_at_point(p, text);
        self.caret = c;
        self.desired_col = None;
        self.bump_blink();
    }

    // ----- keyboard --------------------------------------------------------

    /// Handle a keyboard event. Returns true if consumed. `text_id` is the
    /// Automerge `Text` object holding the source; edits splice into it
    /// directly. Caller is responsible for re-reading the text afterward.
    pub fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        text_id: &ObjId,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();
        let shift = mods.shift_key();
        let current = doc.text(text_id).unwrap_or_default();
        let len = current.chars().count();

        // Editing/navigation shortcuts with Ctrl/Cmd.
        if ctrl {
            if let Key::Character(c) = logical_key {
                match c.to_lowercase().as_str() {
                    "a" => {
                        self.anchor = 0;
                        self.caret = len;
                        self.bump_blink();
                        return true;
                    }
                    "c" => {
                        if let Some((lo, hi)) = self.selection() {
                            self.clipboard = substring(&current, lo, hi);
                        }
                        return true;
                    }
                    "x" => {
                        if let Some((lo, hi)) = self.selection() {
                            self.clipboard = substring(&current, lo, hi);
                            self.delete_range(doc, text_id, lo, hi);
                        }
                        return true;
                    }
                    "v" => {
                        let paste = self.clipboard.clone();
                        if !paste.is_empty() {
                            self.insert_str(doc, text_id, &current, &paste);
                        }
                        return true;
                    }
                    _ => return false, // let the app handle other Ctrl combos
                }
            }
            return false;
        }

        match logical_key {
            Key::Named(NamedKey::Backspace) => {
                if let Some((lo, hi)) = self.selection() {
                    self.delete_range(doc, text_id, lo, hi);
                } else if self.caret > 0 {
                    self.delete_range(doc, text_id, self.caret - 1, self.caret);
                }
                self.desired_col = None;
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::Delete) => {
                if let Some((lo, hi)) = self.selection() {
                    self.delete_range(doc, text_id, lo, hi);
                } else if self.caret < len {
                    self.delete_range(doc, text_id, self.caret, self.caret + 1);
                }
                self.desired_col = None;
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.insert_str(doc, text_id, &current, "\n");
                true
            }
            Key::Named(NamedKey::Tab) => {
                if shift {
                    self.outdent(doc, text_id, &current);
                } else {
                    let spaces = " ".repeat(TAB_WIDTH);
                    self.insert_str(doc, text_id, &current, &spaces);
                }
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_horizontal(-1, len, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_horizontal(1, len, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_vertical(-1, &current, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_vertical(1, &current, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::Home) => {
                let idx = LineIndex::new(&current);
                let (line, _) = idx.line_col(self.caret);
                self.caret = idx.line_start(line);
                if !shift {
                    self.anchor = self.caret;
                }
                self.desired_col = None;
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::End) => {
                let idx = LineIndex::new(&current);
                let (line, _) = idx.line_col(self.caret);
                self.caret = idx.line_start(line) + idx.line_len(line);
                if !shift {
                    self.anchor = self.caret;
                }
                self.desired_col = None;
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::PageUp) => {
                let page = (self.rect.height() / self.line_h).floor().max(1.0) as i64;
                self.move_vertical(-page, &current, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::PageDown) => {
                let page = (self.rect.height() / self.line_h).floor().max(1.0) as i64;
                self.move_vertical(page, &current, shift);
                self.after_edit(doc, text_id);
                true
            }
            Key::Named(NamedKey::Space) => {
                self.insert_str(doc, text_id, &current, " ");
                true
            }
            _ => {
                // Printable text (handles letters, punctuation, IME strings).
                if let Some(t) = text {
                    let s: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !s.is_empty() {
                        self.insert_str(doc, text_id, &current, &s);
                        return true;
                    }
                }
                if let Key::Character(c) = logical_key {
                    let s: String = c.chars().filter(|c| !c.is_control()).collect();
                    if !s.is_empty() {
                        self.insert_str(doc, text_id, &current, &s);
                        return true;
                    }
                }
                false
            }
        }
    }

    fn move_horizontal(&mut self, dir: i64, len: usize, shift: bool) {
        let new = (self.caret as i64 + dir).clamp(0, len as i64) as usize;
        self.caret = new;
        if !shift {
            self.anchor = new;
        }
        self.desired_col = None;
        self.bump_blink();
    }

    fn move_vertical(&mut self, dir: i64, text: &str, shift: bool) {
        let idx = LineIndex::new(text);
        let (line, col) = idx.line_col(self.caret);
        let want = self.desired_col.unwrap_or(col);
        self.desired_col = Some(want);
        let target = (line as i64 + dir).clamp(0, idx.line_count() as i64 - 1) as usize;
        let new_col = want.min(idx.line_len(target));
        self.caret = idx.line_start(target) + new_col;
        if !shift {
            self.anchor = self.caret;
        }
        self.bump_blink();
    }

    fn after_edit(&mut self, doc: &mut AutoCommit, text_id: &ObjId) {
        let text = doc.text(text_id).unwrap_or_default();
        self.ensure_caret_visible(&text);
        self.bump_blink();
    }

    /// Replace any active selection with `s`, then advance the caret.
    fn insert_str(&mut self, doc: &mut AutoCommit, text_id: &ObjId, current: &str, s: &str) {
        if let Some((lo, hi)) = self.selection() {
            self.delete_range(doc, text_id, lo, hi);
        }
        let _ = doc.splice_text(text_id, self.caret, 0, s);
        self.caret += s.chars().count();
        self.anchor = self.caret;
        self.desired_col = None;
        let _ = current; // current captured by caller for context only
        self.after_edit(doc, text_id);
    }

    fn delete_range(&mut self, doc: &mut AutoCommit, text_id: &ObjId, lo: usize, hi: usize) {
        if hi <= lo {
            return;
        }
        let _ = doc.splice_text(text_id, lo, (hi - lo) as isize, "");
        self.caret = lo;
        self.anchor = lo;
    }

    /// Shift+Tab: remove up to `TAB_WIDTH` leading spaces from the caret line.
    fn outdent(&mut self, doc: &mut AutoCommit, text_id: &ObjId, current: &str) {
        let idx = LineIndex::new(current);
        let (line, _) = idx.line_col(self.caret);
        let start = idx.line_start(line);
        let chars: Vec<char> = current.chars().collect();
        let mut remove = 0;
        while remove < TAB_WIDTH && start + remove < chars.len() && chars[start + remove] == ' ' {
            remove += 1;
        }
        if remove > 0 {
            let _ = doc.splice_text(text_id, start, remove as isize, "");
            self.caret = self.caret.saturating_sub(remove);
            self.anchor = self.caret;
        }
        self.after_edit(doc, text_id);
    }

    // ----- rendering -------------------------------------------------------

    fn gutter_width(&self, line_count: usize) -> f64 {
        let digits = line_count.max(1).to_string().len().max(2);
        digits as f64 * self.char_w + 18.0
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        text: &str,
        tokens: &[Vec<Token>],
    ) {
        // Refresh cell metrics from the actual monospace face.
        let (cw, _ch) = tr.measure_styled("M", FONT_SIZE, None, TextWeight::Regular, false, true);
        if cw > 0.0 {
            self.char_w = cw as f64;
        }
        self.line_h = (FONT_SIZE as f64 * 1.55).round();

        let c = &theme.tokens.colors;
        // Pane background (slightly darker than chrome for an "editor" feel).
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &self.rect);

        let idx = LineIndex::new(text);
        let line_count = idx.line_count();
        let gutter = self.gutter_width(line_count);
        let content_x = self.rect.x0 + gutter;

        // Gutter background + divider.
        let gutter_rect = Rect::new(self.rect.x0, self.rect.y0, content_x, self.rect.y1);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &gutter_rect);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(content_x, self.rect.y0), Point::new(content_x, self.rect.y1)),
        );

        let top_pad = 6.0;
        let first = ((self.scroll_y as f64 - top_pad) / self.line_h).floor().max(0.0) as usize;
        let visible = (self.rect.height() / self.line_h).ceil() as usize + 2;
        let last = (first + visible).min(line_count);

        let sel = self.selection();
        let (caret_line, caret_col) = idx.line_col(self.caret);

        for line in first..last {
            let y = self.rect.y0 + top_pad + line as f64 * self.line_h - self.scroll_y as f64;
            let line_len = idx.line_len(line);

            // Current-line highlight (when focused and no selection).
            if self.focused && sel.is_none() && line == caret_line {
                let hl = Rect::new(content_x, y, self.rect.x1, y + self.line_h);
                scene.fill(Fill::NonZero, transform, c.surface_white_5, None, &hl);
            }

            // Selection highlight on this line.
            if let Some((lo, hi)) = sel {
                let ls = idx.line_start(line);
                let le = ls + line_len;
                let sel_lo = lo.max(ls);
                let sel_hi = hi.min(le + 1); // +1 to include trailing newline visual
                if sel_hi > sel_lo && lo <= le && hi >= ls {
                    let c0 = sel_lo.saturating_sub(ls);
                    let c1 = (sel_hi.saturating_sub(ls)).min(line_len + 1);
                    let x0 = content_x + c0 as f64 * self.char_w;
                    let x1 = content_x + c1 as f64 * self.char_w;
                    let r = Rect::new(x0, y, x1.max(x0 + 2.0), y + self.line_h);
                    scene.fill(Fill::NonZero, transform, selection_color(theme), None, &r);
                }
            }

            // Line number (right-aligned in the gutter).
            let num = (line + 1).to_string();
            let num_w = num.chars().count() as f64 * self.char_w;
            let num_x = content_x - 8.0 - num_w;
            let num_color = if line == caret_line { c.text_primary } else { c.text_dim };
            tr.draw_text_styled(
                scene,
                &num,
                FONT_SIZE,
                num_color,
                transform * Affine::translate((num_x, y)),
                None,
                TextWeight::Regular,
                false,
                true,
            );

            // Tokens for this line.
            if let Some(line_tokens) = tokens.get(line) {
                for tok in line_tokens {
                    if tok.text.trim().is_empty() {
                        continue;
                    }
                    let (color, weight, italic) = highlight::style(tok.cat, theme);
                    let x = content_x + tok.start as f64 * self.char_w;
                    tr.draw_text_styled(
                        scene,
                        &tok.text,
                        FONT_SIZE,
                        color,
                        transform * Affine::translate((x, y)),
                        None,
                        weight,
                        italic,
                        true,
                    );
                }
            }
        }

        // Caret.
        if self.focused && self.caret_visible() {
            let y = self.rect.y0 + top_pad + caret_line as f64 * self.line_h - self.scroll_y as f64;
            let x = content_x + caret_col as f64 * self.char_w;
            if y + self.line_h >= self.rect.y0 && y <= self.rect.y1 {
                let caret = Rect::new(x, y + 1.0, x + 1.5, y + self.line_h - 1.0);
                scene.fill(Fill::NonZero, transform, theme.accent_primary, None, &caret);
            }
        }

        scene.pop_layer();
    }

    fn caret_visible(&self) -> bool {
        // ~1.06 Hz blink.
        let ms = self.blink_start.elapsed().as_millis();
        (ms / 530) % 2 == 0
    }

    /// Whether the editor wants continuous repaints (caret blink) — only while
    /// focused.
    pub fn wants_animation(&self) -> bool {
        self.focused
    }
}

/// Returns the active selection-highlight colour (a translucent accent).
fn selection_color(theme: &Theme) -> vello::peniko::Color {
    let a = theme.accent_primary;
    vello::peniko::Color::from_rgba8(a.r(), a.g(), a.b(), 70)
}

/// Char-index helper over a text split into `\n` lines. Cheap to build, used
/// for every caret-coordinate conversion.
struct LineIndex {
    /// (char_start, char_len) per line, excluding the newline.
    lines: Vec<(usize, usize)>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut len = 0usize;
        for ch in text.chars() {
            if ch == '\n' {
                lines.push((start, len));
                start += len + 1;
                len = 0;
            } else {
                len += 1;
            }
        }
        lines.push((start, len));
        Self { lines }
    }

    fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn line_start(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.0).unwrap_or(0)
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.1).unwrap_or(0)
    }

    /// Map a char index to (line, col).
    fn line_col(&self, caret: usize) -> (usize, usize) {
        for (i, &(start, len)) in self.lines.iter().enumerate() {
            if caret <= start + len {
                return (i, caret.saturating_sub(start));
            }
        }
        let last = self.lines.len().saturating_sub(1);
        (last, self.line_len(last))
    }
}

/// Extract chars `[lo, hi)` from `text` as an owned String.
fn substring(text: &str, lo: usize, hi: usize) -> String {
    text.chars().skip(lo).take(hi.saturating_sub(lo)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ObjType;
    use automerge::ROOT;

    fn doc_with(text: &str) -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let id = doc.put_object(&ROOT, "source", ObjType::Text).unwrap();
        doc.splice_text(&id, 0, 0, text).unwrap();
        (doc, id)
    }

    #[test]
    fn line_index_maps_caret() {
        let idx = LineIndex::new("ab\ncde\nf");
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line_col(0), (0, 0));
        assert_eq!(idx.line_col(3), (1, 0));
        assert_eq!(idx.line_col(6), (1, 3));
        assert_eq!(idx.line_col(7), (2, 0));
    }

    #[test]
    fn insert_advances_caret() {
        let (mut doc, id) = doc_with("");
        let mut ed = SourceEditor::new();
        ed.insert_str(&mut doc, &id, "", "hi");
        assert_eq!(doc.text(&id).unwrap(), "hi");
        assert_eq!(ed.caret, 2);
    }

    #[test]
    fn backspace_deletes_selection() {
        let (mut doc, id) = doc_with("hello");
        let mut ed = SourceEditor::new();
        ed.anchor = 1;
        ed.caret = 4; // select "ell"
        ed.delete_range(&mut doc, &id, 1, 4);
        assert_eq!(doc.text(&id).unwrap(), "ho");
        assert_eq!(ed.caret, 1);
    }
}
