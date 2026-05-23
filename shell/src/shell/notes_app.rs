//! Notes — a minimal CRDT-backed plaintext editor hosted as an `AppWidget`.
//!
//! The whole body lives in a single Automerge `Text` object under the tile's
//! root at [`BODY_KEY`]. Edits go through `splice_text` so they merge cleanly
//! with peer edits via the existing `CollabRequest::Apply` path. The widget
//! therefore gets "fully collaborative by default" for free — no per-app
//! presence wiring beyond what every widget already does.
//!
//! Rendering uses the `TextRenderer`'s built-in soft-wrap (cosmic-text) so
//! long paragraphs wrap inside the tile; hard newlines (`\n`) are preserved.
//! The caret is positioned by splitting the text at `cursor_pos`, measuring
//! the prefix with the same wrap width the body uses, and offsetting into
//! the final run — approximate but good enough for the parity milestone.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, Value,
    transaction::Transactable,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// Automerge key under the tile's root where the note body lives.
pub const BODY_KEY: &str = "notes_body";

const PADDING: f64 = 28.0;
const TEXT_SIZE: f32 = 15.0;
const LINE_HEIGHT: f32 = TEXT_SIZE * 1.4;
const COMMIT_DEBOUNCE: Duration = Duration::from_millis(800);

pub struct NotesApp {
    rect: Rect,
    hovered: bool,
    focused: bool,
    text: String,
    /// Byte offset into `text` for the caret.
    cursor_pos: usize,
    last_edit: Option<Instant>,
    last_mouse: Point,
}

impl NotesApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            text: String::new(),
            cursor_pos: 0,
            last_edit: None,
            last_mouse: Point::ZERO,
        }
    }

    /// Create a Notes tile pre-seeded with welcome text so first-boot users
    /// see something concrete. The seed is written to the CRDT lazily on
    /// first sync so repeated boots don't keep appending.
    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.text = text.into();
        app.cursor_pos = app.text.len();
        app
    }

    /// Resolve (or create) the Text object that backs the note body. Returns
    /// `None` only if the Automerge store rejects both the read and the
    /// create — which would indicate a corrupt doc, so callers treat it as
    /// a skip.
    fn resolve_body(&self, state: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, BODY_KEY) {
            return Some(id);
        }
        state.put_object(root, BODY_KEY, ObjType::Text).ok()
    }

    fn clamp_cursor(&mut self) {
        if self.cursor_pos > self.text.len() {
            self.cursor_pos = self.text.len();
        }
        while self.cursor_pos > 0 && !self.text.is_char_boundary(self.cursor_pos) {
            self.cursor_pos -= 1;
        }
    }

    fn insert_text(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        let Some(body) = self.resolve_body(state, root) else { return };
        let char_idx = self.text[..self.cursor_pos].chars().count();
        self.text.insert_str(self.cursor_pos, s);
        let _ = state.splice_text(&body, char_idx, 0, s);
        self.cursor_pos += s.len();
        self.last_edit = Some(Instant::now());
    }

    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.cursor_pos == 0 {
            return;
        }
        let Some(body) = self.resolve_body(state, root) else { return };
        let mut prev = self.cursor_pos - 1;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        let removed = &self.text[prev..self.cursor_pos];
        let char_idx = self.text[..prev].chars().count();
        let n = removed.chars().count();
        self.text.drain(prev..self.cursor_pos);
        self.cursor_pos = prev;
        let _ = state.splice_text(&body, char_idx, n as isize, "");
        self.last_edit = Some(Instant::now());
    }
}

impl Default for NotesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for NotesApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // First sync: if no body exists yet, seed it with whatever the
        // widget was constructed with (via `with_seed`). After that the
        // CRDT is the source of truth.
        match state.get(root, BODY_KEY) {
            Ok(Some((Value::Object(ObjType::Text), id))) => {
                if let Ok(t) = state.text(&id) {
                    self.text = t;
                }
            }
            _ => {
                if let Ok(id) = state.put_object(root, BODY_KEY, ObjType::Text) {
                    if !self.text.is_empty() {
                        let _ = state.splice_text(&id, 0, 0, &self.text);
                    }
                }
            }
        }
        self.clamp_cursor();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let text_color = tokens.colors.neutral[1];
        let dim = tokens.colors.neutral[4];
        // Caret is a *fill* state (focused-but-not-hovered), so use the
        // semantic `accent_primary` (indigo-500) accessor — the concept's
        // dominant accent — rather than the raw `accent[4]` (indigo-600)
        // ramp slot, which is the hover shade.
        let accent = theme.accent_primary;

        let content_x = self.rect.x0 + PADDING;
        let content_y = self.rect.y0 + PADDING;
        let wrap_w = (self.rect.width() - PADDING * 2.0).max(40.0) as f32;

        // Header row: "Notes" title + hint on the right.
        let title_size = tokens.typography.scale.lg;
        tr.draw_text(
            scene,
            "Notes",
            title_size,
            text_color,
            transform * Affine::translate((content_x, content_y)),
            None,
        );
        let hint = if self.focused {
            "Type to edit  ·  Ctrl+Z to undo"
        } else {
            "Click to edit"
        };
        let (hw, _) = tr.measure(hint, tokens.typography.scale.sm, None);
        tr.draw_text(
            scene,
            hint,
            tokens.typography.scale.sm,
            dim,
            transform
                * Affine::translate((
                    self.rect.x1 - PADDING - hw as f64,
                    content_y + (title_size - tokens.typography.scale.sm) as f64 / 2.0,
                )),
            None,
        );

        let body_top = content_y + title_size as f64 + 20.0;

        if self.text.is_empty() && !self.focused {
            tr.draw_text(
                scene,
                "Jot anything here — shared live with collaborators in the workspace.",
                TEXT_SIZE,
                dim,
                transform * Affine::translate((content_x, body_top)),
                Some(wrap_w),
            );
            return;
        }

        // Body text with soft wrap.
        tr.draw_text(
            scene,
            &self.text,
            TEXT_SIZE,
            text_color,
            transform * Affine::translate((content_x, body_top)),
            Some(wrap_w),
        );

        if self.focused {
            // Caret: count newlines before cursor to pick a row, then measure
            // the width of the last visual line's prefix for the column. This
            // deliberately treats each `\n`-separated logical line as a
            // single wrap-unit so the caret stays close-enough to the glyph
            // position without a glyph-by-glyph layout round-trip.
            let prefix = &self.text[..self.cursor_pos];
            let nl_count = prefix.matches('\n').count();
            let last_line_prefix = prefix.rsplit('\n').next().unwrap_or("");
            let (prefix_w, _) = tr.measure(last_line_prefix, TEXT_SIZE, Some(wrap_w));
            let caret_x = content_x + prefix_w as f64;
            let caret_y = body_top + (nl_count as f64) * LINE_HEIGHT as f64;
            let caret = Rect::new(
                caret_x,
                caret_y,
                caret_x + 1.5,
                caret_y + LINE_HEIGHT as f64,
            );
            scene.fill(Fill::NonZero, transform, accent, None, &caret);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.clamp_cursor();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                let was_hover = self.hovered;
                self.hovered = self.rect.contains(p);
                if self.hovered != was_hover {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => {
                if self.hovered && *es == ElementState::Pressed {
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: es,
                        ..
                    },
                ..
            } => {
                if !self.focused || *es != ElementState::Pressed {
                    return;
                }
                // Let the shell handle global hotkeys (Ctrl/Cmd combos) —
                // NotesApp only grabs plain typing.
                let mods = modifiers.state();
                let has_ctrl = mods.control_key() || mods.super_key();
                if has_ctrl {
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        self.delete_before(state, root);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.insert_text(state, root, "\n");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_text(state, root, " ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.insert_text(state, root, "    ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if self.cursor_pos > 0 {
                            let mut p = self.cursor_pos - 1;
                            while p > 0 && !self.text.is_char_boundary(p) {
                                p -= 1;
                            }
                            self.cursor_pos = p;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if self.cursor_pos < self.text.len() {
                            let mut p = self.cursor_pos + 1;
                            while p < self.text.len() && !self.text.is_char_boundary(p) {
                                p += 1;
                            }
                            self.cursor_pos = p;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::Home) => {
                        let line_start = self.text[..self.cursor_pos]
                            .rfind('\n')
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        self.cursor_pos = line_start;
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        let line_end = self.text[self.cursor_pos..]
                            .find('\n')
                            .map(|i| self.cursor_pos + i)
                            .unwrap_or(self.text.len());
                        self.cursor_pos = line_end;
                        *handled = true;
                    }
                    _ => {
                        if let Some(t) = text {
                            let s = t.as_str();
                            if !s.is_empty() && !s.chars().all(|c| c.is_control()) {
                                self.insert_text(state, root, s);
                                *handled = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn needs_commit(&self) -> bool {
        self.last_edit
            .map(|t| t.elapsed() >= COMMIT_DEBOUNCE)
            .unwrap_or(false)
    }

    fn on_commit(&mut self) {
        self.last_edit = None;
    }

    fn get_state(&self) -> Option<String> {
        Some(self.cursor_pos.to_string())
    }

    fn set_state(&mut self, s: &str) {
        if let Ok(pos) = s.parse::<usize>() {
            self.cursor_pos = pos;
            self.clamp_cursor();
        }
    }
}

impl AppWidget for NotesApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Notes"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰎚")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    fn bind() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let tile_root = doc
            .put_object(&root, "tile", ObjType::Map)
            .expect("tile root");
        (doc, tile_root)
    }

    #[test]
    fn sync_creates_body_and_seed_roundtrips() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hi");
        app.sync(&mut doc, &tile);
        assert_eq!(app.text, "hi");
        assert!(matches!(
            doc.get(&tile, BODY_KEY),
            Ok(Some((Value::Object(ObjType::Text), _)))
        ));
    }

    #[test]
    fn insert_and_delete_mutate_crdt() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::new();
        app.sync(&mut doc, &tile);
        app.insert_text(&mut doc, &tile, "abc");
        assert_eq!(app.text, "abc");
        assert_eq!(app.cursor_pos, 3);
        app.delete_before(&mut doc, &tile);
        assert_eq!(app.text, "ab");
    }

    #[test]
    fn newline_insertion_shifts_cursor_to_next_row() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::new();
        app.sync(&mut doc, &tile);
        app.insert_text(&mut doc, &tile, "ab\n");
        assert_eq!(app.text, "ab\n");
        app.insert_text(&mut doc, &tile, "c");
        assert_eq!(app.text, "ab\nc");
        assert_eq!(app.cursor_pos, 4);
    }
}
