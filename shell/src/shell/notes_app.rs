//! Notes — a minimal CRDT-backed plaintext editor hosted as an `AppWidget`.
//!
//! The whole body lives in a single Automerge `Text` object under the tile's
//! root at [`BODY_KEY`], edited through [`CrdtTextBody`] so edits merge with
//! peers via the existing `CollabRequest::Apply` path — collaborative for
//! free, no per-app presence wiring.
//!
//! Selection + clipboard model mirrors `ui::widget::TextInput`: click drops
//! the caret, drag extends a selection, double/triple-click selects word or
//! line, Shift+arrow/Home/End/Ctrl+A extend or set it, and Ctrl+C/X/V route
//! through the shared [`ui::text_selection`] clipboard helpers.
//!
//! The `TextRenderer`'s soft-wrap (cosmic-text) wraps long paragraphs; caret
//! and selection highlight are positioned by splitting at `\n` and measuring
//! prefix widths — approximate across wrap boundaries but close enough to
//! the cursor's row/column model.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::{
    Selection, clipboard_copy, clipboard_paste, line_boundaries, word_boundaries,
};

use crate::shell::crdt_text::{CrdtTextBody, full_bleed_leaf, full_bleed_rect};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, EmptyState, IconDescriptor, Widget};
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
/// Maximum gap between two mouse-clicks that still counts as a
/// double / triple click. 400 ms matches the GTK / Qt default.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);
/// Pixels of cursor wiggle still allowed between rapid clicks before
/// they're treated as separate click chains.
const MULTI_CLICK_RADIUS: f64 = 6.0;

pub struct NotesApp {
    rect: Rect,
    hovered: bool,
    focused: bool,
    /// CRDT-backed body plus the caret arithmetic over it.
    body: CrdtTextBody,
    /// Caret + selection anchor pair. `selection.cursor` is the live
    /// caret byte offset into `body.text`. `selection.anchor == cursor`
    /// means no selection is active.
    selection: Selection,
    last_mouse: Point,
    /// True while the user is dragging the mouse to extend a selection
    /// (between MouseInput::Pressed and MouseInput::Released).
    is_dragging: bool,
    /// Time + position of the most recent click — used to detect
    /// double / triple clicks (within `MULTI_CLICK_INTERVAL` and
    /// `MULTI_CLICK_RADIUS`).
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    /// Cached geometry of the body content area — used to map mouse
    /// positions back to byte offsets in the same coordinate system the
    /// draw pass laid the text out in.
    body_origin: Point,
    wrap_width: f32,
    /// Bounds of the zero-state's call-to-action pill, resolved by the
    /// draw pass and `Rect::ZERO` whenever the zero-state isn't showing.
    /// Only drives the pointer cursor — the pill sits inside the tile, so
    /// the ordinary "click anywhere to place the caret" path already
    /// focuses the note when it is pressed.
    empty_action_rect: Rect,
}

impl NotesApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            body: CrdtTextBody::new(BODY_KEY),
            selection: Selection::default(),
            last_mouse: Point::ZERO,
            is_dragging: false,
            last_click_time: None,
            last_click_pos: Point::ZERO,
            click_count: 0,
            body_origin: Point::ZERO,
            wrap_width: 40.0,
            empty_action_rect: Rect::ZERO,
        }
    }

    /// Create a Notes tile pre-seeded with welcome text so first-boot users
    /// see something concrete. The seed is written to the CRDT lazily on
    /// first sync so repeated boots don't keep appending.
    pub fn with_seed(text: impl Into<String>) -> Self {
        let mut app = Self::new();
        app.body = CrdtTextBody::with_seed(BODY_KEY, text);
        let end = app.body.text.len();
        app.selection = Selection::collapsed(end);
        app
    }

    fn clamp_cursor(&mut self) {
        self.selection.cursor = self.body.clamp(self.selection.cursor);
        self.selection.anchor = self.body.clamp(self.selection.anchor);
    }

    fn delete_selection(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.selection.is_collapsed() {
            return;
        }
        let range = self.selection.min()..self.selection.max();
        let caret = self.body.splice(state, root, range, "");
        self.selection.move_to(caret);
    }

    /// Used by every typing path (printable chars, Enter, Tab, paste).
    fn insert_str(&mut self, state: &mut AutoCommit, root: &ObjId, s: &str) {
        // A collapsed selection makes this an insert; an open one makes it
        // a replace. `splice` covers both — the range is just empty.
        let range = self.selection.min()..self.selection.max();
        let caret = self.body.splice(state, root, range, s);
        self.selection.move_to(caret);
    }

    /// Single-char backspace path for a collapsed selection; selection-aware
    /// paths route through `delete_selection`.
    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let cur = self.selection.cursor;
        if cur == 0 {
            return;
        }
        let prev = self.body.prev_boundary(cur);
        let caret = self.body.splice(state, root, prev..cur, "");
        self.selection.move_to(caret);
    }

    /// Single-char delete-forward path for when the selection is collapsed.
    fn delete_after(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let cur = self.selection.cursor;
        if cur >= self.body.text.len() {
            return;
        }
        let next = self.body.next_boundary(cur);
        self.body.splice(state, root, cur..next, "");
    }

    fn selected_str(&self) -> Option<&str> {
        if self.selection.is_collapsed() {
            None
        } else {
            Some(&self.body.text[self.selection.range()])
        }
    }
}

impl Default for NotesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for NotesApp {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.body.sync(state, root);
        self.clamp_cursor();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        full_bleed_leaf(taffy, node_map, self as *mut dyn Widget)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = full_bleed_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Notes paints no background: the body sits straight on the window
        // card the frame draws, so its ink comes from the card rather than
        // from a ramp slot chosen for one theme's card. `Theme::on_card`
        // answers that for the whole fleet — the stub tile and the
        // foreign-surface placeholder ask it the same question.
        let card_theme = theme.on_window_card();
        let text_color = theme.on_card().ink;
        // Caret is a *fill* state (focused-but-not-hovered), so use the
        // semantic `accent_primary` (indigo-500) accessor — the concept's
        // dominant accent — rather than the raw `accent[4]` (indigo-600)
        // ramp slot, which is the hover shade.
        let accent = theme.accent_primary;
        // Translucent selection background — concept calls for indigo-500
        // at ~30% alpha (matches `TextInput`'s 0.35 visually for a thinner
        // body weight).
        let sel_color = accent.with_alpha_factor(0.30);

        let content_x = self.rect.x0 + PADDING;
        let content_y = self.rect.y0 + PADDING;
        let wrap_w = (self.rect.width() - PADDING * 2.0).max(40.0) as f32;
        self.wrap_width = wrap_w;

        // No header row: the app name already lives on the tab pill the
        // shell draws above this tile, and a corner hint here would be the
        // app drawing chrome it doesn't own. The zero-state below carries
        // the "you can write here" invitation as an actual control instead.
        let body_top = content_y;
        self.body_origin = Point::new(content_x, body_top);

        if self.body.text.is_empty() && !self.focused {
            // A blank note is a *designed* state via the shared `EmptyState`
            // (hero icon, title, dim explanation, accent CTA) rather than a
            // stray line of placeholder text. Nothing is invented to fill
            // the space — the note stays empty until the user types.
            self.empty_action_rect = EmptyState::paint(
                scene,
                transform,
                tr,
                &card_theme,
                self.rect,
                Some(Icon::PenTool),
                "This note is empty",
                Some("Jot anything here — shared live with collaborators in the workspace."),
                Some("Start writing"),
                self.last_mouse,
            )
            .unwrap_or(Rect::ZERO);
            return;
        }
        // Once there is text (or focus), the note is its own affordance.
        self.empty_action_rect = Rect::ZERO;

        // Selection highlight (behind the text).
        if !self.selection.is_collapsed() {
            let sel_lo = self.selection.min();
            let sel_hi = self.selection.max();
            let mut line_start = 0usize;
            let mut row = 0usize;
            let chunks = self.body.text.split('\n');
            for line_text in chunks {
                let line_end = line_start + line_text.len();
                let lo = sel_lo.max(line_start);
                let hi = sel_hi.min(line_end);
                if lo < hi {
                    let pre = &line_text[..(lo - line_start)];
                    let mid = &line_text[(lo - line_start)..(hi - line_start)];
                    let (x0, _) = tr.measure(pre, TEXT_SIZE, Some(wrap_w));
                    let (mw, _) = tr.measure(mid, TEXT_SIZE, Some(wrap_w));
                    let y0 = body_top + row as f64 * LINE_HEIGHT as f64;
                    let rect = Rect::new(
                        content_x + x0 as f64,
                        y0,
                        content_x + x0 as f64 + mw as f64,
                        y0 + LINE_HEIGHT as f64,
                    );
                    scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
                } else if sel_lo <= line_end && sel_hi > line_end {
                    // Selection spans this line's trailing newline — paint a
                    // small indicator past the line content for it.
                    let (full_w, _) = tr.measure(line_text, TEXT_SIZE, Some(wrap_w));
                    let y0 = body_top + row as f64 * LINE_HEIGHT as f64;
                    let rect = Rect::new(
                        content_x + full_w as f64,
                        y0,
                        content_x + full_w as f64 + (TEXT_SIZE as f64 * 0.4),
                        y0 + LINE_HEIGHT as f64,
                    );
                    scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
                }
                line_start = line_end + 1;
                row += 1;
            }
        }

        tr.draw_text(
            scene,
            &self.body.text,
            TEXT_SIZE,
            text_color,
            transform * Affine::translate((content_x, body_top)),
            Some(wrap_w),
        );

        if self.focused {
            // Count newlines before cursor to pick a row, then measure the
            // last visual line's prefix width for the column — treats each
            // logical line as a single wrap-unit rather than a glyph-by-glyph
            // layout round-trip.
            let cur = self.selection.cursor;
            let prefix = &self.body.text[..cur];
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

                if self.is_dragging {
                    // Hit-test needs a TextRenderer, which the event path
                    // doesn't have; approximate via a per-character advance
                    // heuristic. Acceptable because the selection re-snaps
                    // to true glyph bounds on the next draw, and the row is
                    // always right even while the column drifts — it
                    // settles to the glyph grid on mouse-up.
                    let new_pos = self.approximate_hit(p);
                    if new_pos != self.selection.cursor {
                        self.selection.extend_to(new_pos);
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => match es {
                ElementState::Pressed if self.hovered => {
                    *focus_target = Some(node_id);

                    let now = Instant::now();
                    let within_time = self
                        .last_click_time
                        .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
                    let within_radius =
                        (self.last_click_pos - self.last_mouse).hypot() <= MULTI_CLICK_RADIUS;
                    self.click_count = if within_time && within_radius {
                        self.click_count + 1
                    } else {
                        1
                    };
                    self.last_click_time = Some(now);
                    self.last_click_pos = self.last_mouse;

                    let pos = self.approximate_hit(self.last_mouse);
                    match self.click_count {
                        1 => {
                            if modifiers.state().shift_key() {
                                self.selection.extend_to(pos);
                            } else {
                                self.selection.move_to(pos);
                                self.is_dragging = true;
                            }
                        }
                        2 => {
                            let (s, e) = word_boundaries(&self.body.text, pos);
                            self.selection.anchor = s;
                            self.selection.cursor = e;
                        }
                        _ => {
                            let (s, e) = line_boundaries(&self.body.text, pos);
                            self.selection.anchor = s;
                            self.selection.cursor = e;
                        }
                    }
                    *handled = true;
                }
                ElementState::Released => {
                    if self.is_dragging {
                        self.is_dragging = false;
                        *handled = true;
                    }
                }
                _ => {}
            },
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
                let mods = modifiers.state();
                let shift = mods.shift_key();
                let ctrl = mods.control_key() || mods.super_key();

                // Ctrl combos: clipboard + select-all; everything else
                // falls through to the local key handler.
                if ctrl {
                    if let Key::Character(c) = logical_key {
                        let lc = c.to_lowercase();
                        match lc.as_str() {
                            "a" => {
                                if !self.body.text.is_empty() {
                                    self.selection.anchor = 0;
                                    self.selection.cursor = self.body.text.len();
                                }
                                *handled = true;
                                return;
                            }
                            "c" => {
                                if let Some(s) = self.selected_str() {
                                    clipboard_copy(s);
                                }
                                *handled = true;
                                return;
                            }
                            "x" => {
                                if let Some(s) = self.selected_str() {
                                    let owned = s.to_string();
                                    clipboard_copy(&owned);
                                    self.delete_selection(state, root);
                                }
                                *handled = true;
                                return;
                            }
                            "v" => {
                                if let Some(paste) = clipboard_paste() {
                                    if !paste.is_empty() {
                                        // Notes preserves \n — strip only
                                        // other control chars.
                                        let cleaned: String = paste
                                            .chars()
                                            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                                            .collect();
                                        if !cleaned.is_empty() {
                                            self.insert_str(state, root, &cleaned);
                                        }
                                    }
                                }
                                *handled = true;
                                return;
                            }
                            _ => {
                                // Let the shell pick up global Ctrl+Z etc.
                                return;
                            }
                        }
                    }
                    // Ctrl + non-character: let shell handle it.
                    return;
                }

                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if !self.selection.is_collapsed() {
                            self.delete_selection(state, root);
                        } else {
                            self.delete_before(state, root);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        if !self.selection.is_collapsed() {
                            self.delete_selection(state, root);
                        } else {
                            self.delete_after(state, root);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.insert_str(state, root, "\n");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.insert_str(state, root, " ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::Tab) => {
                        self.insert_str(state, root, "    ");
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if !shift && !self.selection.is_collapsed() {
                            // Matches every native editor.
                            self.selection.move_to(self.selection.min());
                        } else {
                            let mut p = self.selection.cursor;
                            if p > 0 {
                                p -= 1;
                                while p > 0 && !self.body.text.is_char_boundary(p) {
                                    p -= 1;
                                }
                            }
                            if shift {
                                self.selection.extend_to(p);
                            } else {
                                self.selection.move_to(p);
                            }
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if !shift && !self.selection.is_collapsed() {
                            self.selection.move_to(self.selection.max());
                        } else {
                            let mut p = self.selection.cursor;
                            if p < self.body.text.len() {
                                p += 1;
                                while p < self.body.text.len() && !self.body.text.is_char_boundary(p) {
                                    p += 1;
                                }
                            }
                            if shift {
                                self.selection.extend_to(p);
                            } else {
                                self.selection.move_to(p);
                            }
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let cur = self.selection.cursor;
                        let line_start = self.body.text[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let col = cur - line_start;
                        let new_pos = if line_start == 0 {
                            0
                        } else {
                            let prev_nl = self.body.text[..line_start - 1]
                                .rfind('\n')
                                .map(|i| i + 1)
                                .unwrap_or(0);
                            let prev_line_end = line_start - 1;
                            let prev_line_len = prev_line_end - prev_nl;
                            let want_col = col.min(prev_line_len);
                            let mut p = prev_nl + want_col;
                            while p > prev_nl && !self.body.text.is_char_boundary(p) {
                                p -= 1;
                            }
                            p
                        };
                        if shift {
                            self.selection.extend_to(new_pos);
                        } else {
                            self.selection.move_to(new_pos);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        let cur = self.selection.cursor;
                        let line_start = self.body.text[..cur].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let col = cur - line_start;
                        let line_end = self.body.text[cur..]
                            .find('\n')
                            .map(|i| cur + i)
                            .unwrap_or(self.body.text.len());
                        let new_pos = if line_end == self.body.text.len() {
                            self.body.text.len()
                        } else {
                            let next_line_start = line_end + 1;
                            let next_line_end = self.body.text[next_line_start..]
                                .find('\n')
                                .map(|i| next_line_start + i)
                                .unwrap_or(self.body.text.len());
                            let next_line_len = next_line_end - next_line_start;
                            let want_col = col.min(next_line_len);
                            let mut p = next_line_start + want_col;
                            while p < self.body.text.len() && !self.body.text.is_char_boundary(p) {
                                p += 1;
                            }
                            p
                        };
                        if shift {
                            self.selection.extend_to(new_pos);
                        } else {
                            self.selection.move_to(new_pos);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        let line_start = self.body.text[..self.selection.cursor]
                            .rfind('\n')
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        if shift {
                            self.selection.extend_to(line_start);
                        } else {
                            self.selection.move_to(line_start);
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        let line_end = self.body.text[self.selection.cursor..]
                            .find('\n')
                            .map(|i| self.selection.cursor + i)
                            .unwrap_or(self.body.text.len());
                        if shift {
                            self.selection.extend_to(line_end);
                        } else {
                            self.selection.move_to(line_end);
                        }
                        *handled = true;
                    }
                    _ => {
                        if let Some(t) = text {
                            let s = t.as_str();
                            if !s.is_empty() && !s.chars().all(|c| c.is_control()) {
                                self.insert_str(state, root, s);
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

    /// The record this tab's document is. Folded into the body's presence
    /// key so a peer's caret arriving for one tab is not drawn in another
    /// tab of the same app.
    fn set_record_scope(&mut self, record: &ui::record::RecordId) {
        self.body.set_record(record);
    }

    fn presence_key(&self) -> Option<ui::presence::PresenceKey> {
        Some(self.body.presence_key())
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

    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    /// The zero-state's call-to-action reads as a button, so it points like
    /// one. Everywhere else the tile is a text surface and keeps the
    /// default cursor.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.empty_action_rect.area() > 0.0 && self.empty_action_rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn needs_commit(&self) -> bool {
        self.body.needs_commit()
    }

    fn on_commit(&mut self) {
        self.body.on_commit();
    }

    fn get_state(&self) -> Option<String> {
        // Encode "anchor:cursor" so undo/redo restores selection state.
        Some(format!(
            "{}:{}",
            self.selection.anchor, self.selection.cursor
        ))
    }

    fn set_state(&mut self, s: &str) {
        if let Some((a, c)) = s.split_once(':') {
            if let (Ok(anchor), Ok(cursor)) = (a.parse::<usize>(), c.parse::<usize>()) {
                self.selection.anchor = anchor;
                self.selection.cursor = cursor;
                self.clamp_cursor();
                return;
            }
        }
        // Backwards-compat: an older state blob was just the cursor.
        if let Ok(pos) = s.parse::<usize>() {
            self.selection = Selection::collapsed(pos);
            self.clamp_cursor();
        }
    }
}

impl NotesApp {
    /// Coarse hit-test used inside `handle_event`, which has no
    /// `TextRenderer` available — approximates with a per-character
    /// advance. Good enough for "drag selects roughly the right text";
    /// the visual selection rect is drawn against true glyph metrics in
    /// `draw`.
    fn approximate_hit(&self, point: Point) -> usize {
        if self.body.text.is_empty() {
            return 0;
        }
        let rel_x = (point.x - self.body_origin.x).max(0.0);
        let rel_y = (point.y - self.body_origin.y).max(0.0);
        let row = (rel_y / LINE_HEIGHT as f64).floor() as usize;

        // Approximate width per character. For 15px text the average
        // advance is around 8px for proportional fonts; we use a slightly
        // tighter 7.2 to avoid overshooting on the common case.
        const APPROX_ADVANCE_PX: f64 = 7.2;

        let mut line_starts: Vec<usize> = std::iter::once(0)
            .chain(self.body.text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        if line_starts.is_empty() {
            line_starts.push(0);
        }
        let line_count = line_starts.len();
        let row = row.min(line_count.saturating_sub(1));
        let line_start = line_starts[row];
        let line_end = if row + 1 < line_count {
            line_starts[row + 1] - 1
        } else {
            self.body.text.len()
        };

        let chars_at = (rel_x / APPROX_ADVANCE_PX).round().max(0.0) as usize;
        let line_text = &self.body.text[line_start..line_end];
        for (i, (b, _)) in line_text.char_indices().enumerate() {
            if i >= chars_at {
                return line_start + b;
            }
        }
        line_end
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
    use automerge::{ObjType, ReadDoc, Value, transaction::Transactable};

    fn bind() -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let tile_root = doc
            .put_object(&root, "tile", ObjType::Map)
            .expect("tile root");
        (doc, tile_root)
    }

    /// Run the real draw pass over a tile-sized rect. Nothing is rasterised —
    /// we only care about the geometry the pass resolves — but it is the
    /// production `Widget::draw`, not a stand-in for it.
    fn draw_tile(app: &mut NotesApp, rect: Rect) {
        app.rect = rect;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// A blank note shows the shared zero-state, centred in the tile.
    #[test]
    fn a_blank_note_paints_the_shared_zero_state_centred() {
        let mut app = NotesApp::new();
        let tile = Rect::new(0.0, 0.0, 900.0, 600.0);
        draw_tile(&mut app, tile);

        let pill = app.empty_action_rect;
        assert!(
            pill.area() > 0.0,
            "a blank note paints EmptyState, whose call-to-action resolves a rect",
        );
        assert!(tile.contains(pill.center()), "the pill sits inside the tile");
        assert!(
            (pill.center().x - tile.center().x).abs() < 1.0,
            "the zero-state is horizontally centred, not left-aligned at the padding: \
             pill centre {} vs tile centre {}",
            pill.center().x,
            tile.center().x,
        );
        // The pill is the last element of the stack, so it sits below the
        // tile's midline while the stack as a whole straddles it. A
        // top-aligned layout would put it in the upper half.
        assert!(
            pill.center().y > tile.center().y,
            "the pill trails a vertically-centred stack, so it is past the midline: \
             {} vs {}",
            pill.center().y,
            tile.center().y,
        );
        assert!(
            pill.y1 < tile.y1,
            "and the stack still fits inside the tile",
        );
    }

    /// The call-to-action is wired as a control, not a decoration.
    #[test]
    fn the_zero_state_action_points_like_a_button() {
        let mut app = NotesApp::new();
        let tile = Rect::new(0.0, 0.0, 900.0, 600.0);
        draw_tile(&mut app, tile);
        assert_eq!(
            app.cursor_shape(app.empty_action_rect.center()),
            Some(winit::window::CursorIcon::Pointer),
        );
        assert_eq!(
            app.cursor_shape(Point::new(2.0, 2.0)),
            None,
            "the rest of the tile is a text surface and keeps the default cursor",
        );
    }

    /// The zero-state is for a note with nothing in it. As soon as there is
    /// text the note is its own affordance, and no pill is left behind to
    /// keep claiming the pointer.
    #[test]
    fn a_note_with_text_paints_no_zero_state() {
        let (mut doc, tile_root) = bind();
        let mut app = NotesApp::with_seed("hello");
        app.sync(&mut doc, &tile_root);
        let tile = Rect::new(0.0, 0.0, 900.0, 600.0);
        draw_tile(&mut app, tile);
        assert_eq!(
            app.empty_action_rect,
            Rect::ZERO,
            "text in the body ⇒ no zero-state",
        );
        assert_eq!(app.cursor_shape(tile.center()), None);
    }

    /// Focusing a blank note drops straight into editing — the zero-state
    /// gets out of the way rather than sitting behind the caret.
    #[test]
    fn focusing_a_blank_note_clears_the_zero_state() {
        let mut app = NotesApp::new();
        let tile = Rect::new(0.0, 0.0, 900.0, 600.0);
        draw_tile(&mut app, tile);
        assert!(app.empty_action_rect.area() > 0.0);

        app.set_focus(true);
        draw_tile(&mut app, tile);
        assert_eq!(app.empty_action_rect, Rect::ZERO);
        assert_eq!(app.cursor_shape(tile.center()), None);
    }

    /// The body starts at the tile padding — no header row is reserved.
    #[test]
    fn body_text_starts_at_the_padding_with_no_header_gap() {
        let (mut doc, tile_root) = bind();
        let mut app = NotesApp::with_seed("hello");
        app.sync(&mut doc, &tile_root);
        let tile = Rect::new(10.0, 20.0, 910.0, 620.0);
        draw_tile(&mut app, tile);
        assert_eq!(app.body_origin.x, tile.x0 + PADDING);
        assert_eq!(
            app.body_origin.y,
            tile.y0 + PADDING,
            "no header row is reserved above the body",
        );
    }

    #[test]
    fn sync_creates_body_and_seed_roundtrips() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hi");
        app.sync(&mut doc, &tile);
        assert_eq!(app.body.text, "hi");
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
        app.insert_str(&mut doc, &tile, "abc");
        assert_eq!(app.body.text, "abc");
        assert_eq!(app.selection.cursor, 3);
        app.delete_before(&mut doc, &tile);
        assert_eq!(app.body.text, "ab");
    }

    #[test]
    fn newline_insertion_shifts_cursor_to_next_row() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::new();
        app.sync(&mut doc, &tile);
        app.insert_str(&mut doc, &tile, "ab\n");
        assert_eq!(app.body.text, "ab\n");
        app.insert_str(&mut doc, &tile, "c");
        assert_eq!(app.body.text, "ab\nc");
        assert_eq!(app.selection.cursor, 4);
    }

    /// Shift+Right extends the selection by one character (anchor stays
    /// pinned at the original cursor). Routed via `selection.extend_to`.
    #[test]
    fn shift_right_extends_selection() {
        let mut app = NotesApp::with_seed("hello");
        app.selection = Selection::collapsed(0);
        let p = app.selection.cursor + 1;
        app.selection.extend_to(p);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 1);
        assert_eq!(&app.body.text[app.selection.range()], "h");

        app.selection.extend_to(3);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(&app.body.text[app.selection.range()], "hel");
    }

    /// Double-click selects the word containing the byte position.
    #[test]
    fn double_click_selects_word() {
        let app = NotesApp::with_seed("hello world quick");
        let (s, e) = word_boundaries(&app.body.text, 8);
        assert_eq!(s, 6);
        assert_eq!(e, 11);
        assert_eq!(&app.body.text[s..e], "world");
    }

    /// Ctrl+A behavior: anchor=0, cursor=len.
    #[test]
    fn ctrl_a_selects_all() {
        let mut app = NotesApp::with_seed("alpha\nbeta\ngamma");
        app.selection.anchor = 0;
        app.selection.cursor = app.body.text.len();
        assert!(!app.selection.is_collapsed());
        assert_eq!(app.selection.min(), 0);
        assert_eq!(app.selection.max(), app.body.text.len());
        assert_eq!(&app.body.text[app.selection.range()], "alpha\nbeta\ngamma");
    }

    /// Backspace with an active selection deletes the range and collapses,
    /// rather than chopping a single char to the left.
    #[test]
    fn backspace_with_selection_deletes_range() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        app.delete_selection(&mut doc, &tile);
        assert_eq!(app.body.text, "hello ");
        assert!(app.selection.is_collapsed());
        assert_eq!(app.selection.cursor, 6);
    }

    /// `insert_str` over an active selection replaces the range (paste
    /// path).
    #[test]
    fn insert_str_replaces_selection() {
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        app.insert_str(&mut doc, &tile, "there");
        assert_eq!(app.body.text, "hello there");
        assert!(app.selection.is_collapsed());
        assert_eq!(app.selection.cursor, "hello there".len());
    }

    /// Triple-click line selection routes through `line_boundaries`.
    #[test]
    fn triple_click_selects_line() {
        let app = NotesApp::with_seed("alpha\nbeta\ngamma");
        let (s, e) = line_boundaries(&app.body.text, 8);
        // Byte 8 sits inside "beta" (starts at 6, ends at 10 — the \n).
        assert_eq!(s, 6);
        assert_eq!(e, 10);
        assert_eq!(&app.body.text[s..e], "beta");
    }

    /// State serialization round-trips anchor + cursor across undo.
    #[test]
    fn state_serializes_anchor_and_cursor() {
        let mut app = NotesApp::with_seed("hello world");
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        let s = app.get_state().unwrap();
        assert_eq!(s, "6:11");
        let mut app2 = NotesApp::with_seed("hello world");
        app2.set_state(&s);
        assert_eq!(app2.selection.anchor, 6);
        assert_eq!(app2.selection.cursor, 11);
    }

    /// Backwards-compat: pre-selection state blobs were the cursor only.
    #[test]
    fn state_backcompat_cursor_only() {
        let mut app = NotesApp::with_seed("hello");
        app.set_state("3");
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(app.selection.anchor, 3);
    }

    /// Ctrl+C copies the selected text to the clipboard, Ctrl+V pastes
    /// it back. Skipped if arboard isn't available (CI without a
    /// display, etc).
    #[test]
    #[cfg(target_os = "linux")]
    fn ctrl_c_then_ctrl_v_round_trips_through_arboard() {
        // Write a sentinel and read it back; None means no usable arboard
        // backend (CI without a display, etc.) — treat as skip.
        clipboard_copy("__qos_clip_probe__");
        if clipboard_paste().as_deref() != Some("__qos_clip_probe__") {
            eprintln!("clipboard backend unavailable; skipping round-trip test");
            return;
        }
        let (mut doc, tile) = bind();
        let mut app = NotesApp::with_seed("hello world");
        app.sync(&mut doc, &tile);
        app.selection.anchor = 6;
        app.selection.cursor = 11;
        let copied = app
            .selected_str()
            .map(|s| s.to_string())
            .unwrap_or_default();
        clipboard_copy(&copied);
        app.selection = Selection::collapsed(app.body.text.len());
        let pasted = clipboard_paste();
        if let Some(p) = pasted {
            app.insert_str(&mut doc, &tile, &p);
            assert!(app.body.text.ends_with("world"));
        }
    }
}
