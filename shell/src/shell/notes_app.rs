//! Notes — a minimal CRDT-backed plaintext editor hosted as an `AppWidget`.
//!
//! The body lives in a single Automerge `Text` object under the tile's root
//! at [`BODY_KEY`], edited through [`CrdtTextBody`] so edits merge with
//! peers via the existing `CollabRequest::Apply` path for free.
//!
//! Selection + clipboard model mirrors `ui::widget::TextInput`: click drops
//! the caret, drag extends a selection, double/triple-click selects word or
//! line, Shift+arrow/Home/End/Ctrl+A extend or set it, and Ctrl+C/X/V route
//! through the shared [`ui::text_selection`] clipboard helpers. Caret and
//! selection highlight are positioned by splitting at `\n` and measuring
//! prefix widths — approximate across wrap boundaries but close enough.

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
/// ~8px average advance for 15px proportional text; 7.2 avoids
/// overshooting on the common case.
const APPROX_ADVANCE_PX: f64 = 7.2;

/// Byte offset where the line containing `pos` starts (one past the
/// previous `\n`, or 0 for the first line).
fn line_start_at(text: &str, pos: usize) -> usize {
    text.get(..pos)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i.saturating_add(1))
}

/// Byte offset where the line containing `pos` ends (the `\n` itself,
/// or the text's length on the last line).
fn line_end_at(text: &str, pos: usize) -> usize {
    text.get(pos..)
        .and_then(|s| s.find('\n'))
        .map_or(text.len(), |i| pos.saturating_add(i))
}

/// Walks forward from `pos` to the next valid char boundary (or the
/// text's end). Used after column arithmetic lands mid-character.
fn snap_forward_to_char_boundary(text: &str, pos: usize) -> usize {
    let mut p = pos.min(text.len());
    while p < text.len() && !text.is_char_boundary(p) {
        p = p.saturating_add(1);
    }
    p
}

/// Widens a small row/column count to `f64` for draw-time geometry.
fn count_to_f64(n: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded well under 2^53 by every caller (a document row/column count)"
    )]
    {
        n as f64
    }
}

pub struct NotesApp {
    rect: Rect,
    hovered: bool,
    focused: bool,
    /// CRDT-backed body plus the caret arithmetic over it.
    body: CrdtTextBody,
    /// `anchor == cursor` means no selection is active.
    selection: Selection,
    last_mouse: Point,
    is_dragging: bool,
    /// For double/triple-click detection.
    last_click_time: Option<Instant>,
    last_click_pos: Point,
    click_count: u32,
    /// For mapping mouse positions to byte offsets in the draw pass's
    /// coordinate system.
    body_origin: Point,
    wrap_width: f32,
    /// `Rect::ZERO` when not showing. Only drives the pointer cursor —
    /// clicking anywhere already focuses the note.
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

    /// Collapsed-selection path; selection-aware paths route through
    /// `delete_selection`.
    fn delete_before(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let cur = self.selection.cursor;
        if cur == 0 {
            return;
        }
        let prev = self.body.prev_boundary(cur);
        let caret = self.body.splice(state, root, prev..cur, "");
        self.selection.move_to(caret);
    }

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
            self.body.text.get(self.selection.range())
        }
    }

    fn move_or_extend(&mut self, pos: usize, extend: bool) {
        if extend {
            self.selection.extend_to(pos);
        } else {
            self.selection.move_to(pos);
        }
    }

    /// Vertical-nav helper for `ArrowUp`: same column, previous line.
    fn move_line_up(&mut self, shift: bool) {
        let cur = self.selection.cursor;
        let line_start = line_start_at(&self.body.text, cur);
        let col = cur.saturating_sub(line_start);
        let new_pos = if line_start == 0 {
            0
        } else {
            let prev_nl = line_start_at(&self.body.text, line_start.saturating_sub(1));
            let prev_line_end = line_start.saturating_sub(1);
            let prev_line_len = prev_line_end.saturating_sub(prev_nl);
            let want_col = col.min(prev_line_len);
            // `prev_nl` is always itself a valid char boundary (0, or one
            // past a `\n`), so clamping downward from `want_col` bytes
            // past it can never walk below `prev_nl`.
            self.body.clamp(prev_nl.saturating_add(want_col))
        };
        self.move_or_extend(new_pos, shift);
    }

    /// Vertical-nav helper for `ArrowDown`: same column, next line.
    fn move_line_down(&mut self, shift: bool) {
        let cur = self.selection.cursor;
        let line_start = line_start_at(&self.body.text, cur);
        let col = cur.saturating_sub(line_start);
        let line_end = line_end_at(&self.body.text, cur);
        let new_pos = if line_end == self.body.text.len() {
            self.body.text.len()
        } else {
            let next_line_start = line_end.saturating_add(1);
            let next_line_end = line_end_at(&self.body.text, next_line_start);
            let next_line_len = next_line_end.saturating_sub(next_line_start);
            let want_col = col.min(next_line_len);
            snap_forward_to_char_boundary(&self.body.text, next_line_start.saturating_add(want_col))
        };
        self.move_or_extend(new_pos, shift);
    }

    fn move_home(&mut self, shift: bool) {
        let line_start = line_start_at(&self.body.text, self.selection.cursor);
        self.move_or_extend(line_start, shift);
    }

    fn move_end(&mut self, shift: bool) {
        let line_end = line_end_at(&self.body.text, self.selection.cursor);
        self.move_or_extend(line_end, shift);
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
        full_bleed_leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
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
        // Notes paints no background: its ink comes from the window card
        // the frame already draws, via `Theme::on_card`.
        let card_theme = theme.on_window_card();
        let text_color = theme.on_card().ink;
        // Fill-state accent (focused-but-not-hovered), not the hover ramp slot.
        let accent = theme.accent_primary;
        // ~30% alpha (matches `TextInput`'s 0.35 for a thinner body weight).
        let sel_color = accent.with_alpha_factor(0.30);

        let content_x = self.rect.x0 + PADDING;
        let content_y = self.rect.y0 + PADDING;
        let wrap_w_f64 = (self.rect.width() - PADDING * 2.0).max(40.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "screen-space width in pixels, always far below f32's exact-integer range"
        )]
        let wrap_w = wrap_w_f64 as f32;
        self.wrap_width = wrap_w;

        // No header row: the app name lives on the shell's tab pill, and a
        // corner hint here would be chrome this app doesn't own.
        let body_top = content_y;
        self.body_origin = Point::new(content_x, body_top);

        if self.body.text.is_empty() && !self.focused {
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
        self.empty_action_rect = Rect::ZERO;

        if !self.selection.is_collapsed() {
            self.draw_selection_highlight(
                scene, transform, tr, sel_color, content_x, body_top, wrap_w,
            );
        }

        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine::mul is pure matrix composition (translate here), not primitive integer arithmetic that can silently wrap"
        )]
        let body_transform = transform * Affine::translate((content_x, body_top));
        tr.draw_text(
            scene,
            &self.body.text,
            TEXT_SIZE,
            text_color,
            body_transform,
            Some(wrap_w),
        );

        if self.focused {
            self.draw_caret(scene, transform, tr, accent, content_x, body_top, wrap_w);
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
                self.handle_cursor_moved(*position, scale_factor, handled);
            }
            WindowEvent::MouseInput {
                state: es,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_mouse_input(*es, modifiers, handled, focus_target, node_id);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                self.handle_keyboard_input(key_event, modifiers, state, root, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "NotesApp"
    }

    /// Folded into the body's presence key so a peer's caret for one tab
    /// isn't drawn in another tab of the same app.
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

    /// The zero-state CTA points like a button; elsewhere the tile keeps
    /// the default cursor.
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
        if let Some((a, c)) = s.split_once(':')
            && let (Ok(anchor), Ok(cursor)) = (a.parse::<usize>(), c.parse::<usize>())
        {
            self.selection.anchor = anchor;
            self.selection.cursor = cursor;
            self.clamp_cursor();
            return;
        }
        // Backwards-compat: an older state blob was just the cursor.
        if let Ok(pos) = s.parse::<usize>() {
            self.selection = Selection::collapsed(pos);
            self.clamp_cursor();
        }
    }
}

impl NotesApp {
    /// The selection-highlight rects painted per visual line, including
    /// the trailing-newline sliver when the selection spans a line break.
    fn draw_selection_highlight(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        sel_color: vello::peniko::Color,
        content_x: f64,
        body_top: f64,
        wrap_w: f32,
    ) {
        let sel_lo = self.selection.min();
        let sel_hi = self.selection.max();
        let mut line_start = 0usize;
        for (row, line_text) in self.body.text.split('\n').enumerate() {
            let line_end = line_start.saturating_add(line_text.len());
            let lo = sel_lo.max(line_start);
            let hi = sel_hi.min(line_end);
            let y0 = body_top + count_to_f64(row) * f64::from(LINE_HEIGHT);
            if lo < hi {
                let pre_len = lo.saturating_sub(line_start);
                let mid_end = hi.saturating_sub(line_start);
                let pre = line_text.get(..pre_len).unwrap_or("");
                let mid = line_text.get(pre_len..mid_end).unwrap_or("");
                let (x0, _) = tr.measure(pre, TEXT_SIZE, Some(wrap_w));
                let (mw, _) = tr.measure(mid, TEXT_SIZE, Some(wrap_w));
                let rect = Rect::new(
                    content_x + f64::from(x0),
                    y0,
                    content_x + f64::from(x0) + f64::from(mw),
                    y0 + f64::from(LINE_HEIGHT),
                );
                scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
            } else if sel_lo <= line_end && sel_hi > line_end {
                // Selection spans this line's trailing newline.
                let (full_w, _) = tr.measure(line_text, TEXT_SIZE, Some(wrap_w));
                let rect = Rect::new(
                    content_x + f64::from(full_w),
                    y0,
                    content_x + f64::from(full_w) + (f64::from(TEXT_SIZE) * 0.4),
                    y0 + f64::from(LINE_HEIGHT),
                );
                scene.fill(Fill::NonZero, transform, sel_color, None, &rect);
            }
            line_start = line_end.saturating_add(1);
        }
    }

    /// Count newlines before the cursor for the row, measure the last
    /// line's prefix for the column.
    fn draw_caret(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        accent: vello::peniko::Color,
        content_x: f64,
        body_top: f64,
        wrap_w: f32,
    ) {
        let cur = self.selection.cursor;
        let prefix = self.body.text.get(..cur).unwrap_or("");
        let nl_count = prefix.matches('\n').count();
        let last_line_prefix = prefix.rsplit('\n').next().unwrap_or("");
        let (prefix_w, _) = tr.measure(last_line_prefix, TEXT_SIZE, Some(wrap_w));
        let caret_x = content_x + f64::from(prefix_w);
        let caret_y = body_top + count_to_f64(nl_count) * f64::from(LINE_HEIGHT);
        let caret = Rect::new(
            caret_x,
            caret_y,
            caret_x + 1.5,
            caret_y + f64::from(LINE_HEIGHT),
        );
        scene.fill(Fill::NonZero, transform, accent, None, &caret);
    }

    /// Coarse hit-test for `handle_event` (no `TextRenderer` there); the
    /// visual selection is drawn against true glyph metrics in `draw`.
    fn approximate_hit(&self, point: Point) -> usize {
        if self.body.text.is_empty() {
            return 0;
        }
        let rel_x = (point.x - self.body_origin.x).max(0.0);
        let rel_y = (point.y - self.body_origin.y).max(0.0);
        let row_f = (rel_y / f64::from(LINE_HEIGHT)).floor().max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to >= 0.0 above and immediately re-clamped to the document's line count below"
        )]
        let row = row_f as usize;

        let mut line_starts: Vec<usize> = std::iter::once(0)
            .chain(
                self.body
                    .text
                    .match_indices('\n')
                    .map(|(i, _)| i.saturating_add(1)),
            )
            .collect();
        if line_starts.is_empty() {
            line_starts.push(0);
        }
        let line_count = line_starts.len();
        let row = row.min(line_count.saturating_sub(1));
        let line_start = line_starts.get(row).copied().unwrap_or(0);
        let line_end = if row.saturating_add(1) < line_count {
            line_starts
                .get(row.saturating_add(1))
                .copied()
                .unwrap_or(self.body.text.len())
                .saturating_sub(1)
        } else {
            self.body.text.len()
        };

        let chars_at_f = (rel_x / APPROX_ADVANCE_PX).round().max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to >= 0.0 immediately above; an overshoot just means \"past end of line\", which the loop's fallback return below already handles"
        )]
        let chars_at = chars_at_f as usize;
        let line_text = self.body.text.get(line_start..line_end).unwrap_or("");
        for (i, (b, _)) in line_text.char_indices().enumerate() {
            if i >= chars_at {
                return line_start.saturating_add(b);
            }
        }
        line_end
    }

    fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_mouse = p;
        let was_hover = self.hovered;
        self.hovered = self.rect.contains(p);
        if self.hovered != was_hover {
            *handled = true;
        }

        if self.is_dragging {
            // No TextRenderer on the event path; approximate hit via
            // per-character advance. The selection re-snaps to true
            // glyph bounds on the next draw.
            let new_pos = self.approximate_hit(p);
            if new_pos != self.selection.cursor {
                self.selection.extend_to(new_pos);
                *handled = true;
            }
        }
    }

    fn handle_mouse_input(
        &mut self,
        es: ElementState,
        modifiers: &Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        match es {
            ElementState::Pressed if self.hovered => {
                *focus_target = Some(node_id);

                let now = Instant::now();
                let within_time = self
                    .last_click_time
                    .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_INTERVAL);
                let within_radius =
                    self.last_click_pos.distance(self.last_mouse) <= MULTI_CLICK_RADIUS;
                self.click_count = if within_time && within_radius {
                    self.click_count.saturating_add(1)
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
            ElementState::Released if self.is_dragging => {
                self.is_dragging = false;
                *handled = true;
            }
            _ => {}
        }
    }

    fn handle_keyboard_input(
        &mut self,
        key_event: &KeyEvent,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        if !self.focused || key_event.state != ElementState::Pressed {
            return;
        }
        let mods = modifiers.state();
        let shift = mods.shift_key();
        let ctrl = mods.control_key() || mods.super_key();

        if ctrl {
            self.handle_ctrl_key(&key_event.logical_key, state, root, handled);
            return;
        }
        self.handle_plain_key(
            &key_event.logical_key,
            key_event.text.as_deref(),
            shift,
            state,
            root,
            handled,
        );
    }

    /// Ctrl+A/C/X/V. Any other Ctrl+character (or Ctrl+non-character) is
    /// left unhandled so the shell can pick up global shortcuts.
    fn handle_ctrl_key(
        &mut self,
        logical_key: &Key,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        let Key::Character(c) = logical_key else {
            return;
        };
        let lc = c.to_lowercase();
        match lc.as_str() {
            "a" => {
                if !self.body.text.is_empty() {
                    self.selection.anchor = 0;
                    self.selection.cursor = self.body.text.len();
                }
                *handled = true;
            }
            "c" => {
                if let Some(s) = self.selected_str() {
                    clipboard_copy(s);
                }
                *handled = true;
            }
            "x" => {
                if let Some(s) = self.selected_str() {
                    let owned = s.to_string();
                    clipboard_copy(&owned);
                    self.delete_selection(state, root);
                }
                *handled = true;
            }
            "v" => {
                if let Some(paste) = clipboard_paste()
                    && !paste.is_empty()
                {
                    // Preserve \n, strip other control chars.
                    let cleaned: String = paste
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                        .collect();
                    if !cleaned.is_empty() {
                        self.insert_str(state, root, &cleaned);
                    }
                }
                *handled = true;
            }
            _ => {}
        }
    }

    /// Everything but Ctrl+combos: editing keys, navigation, and plain
    /// text insertion.
    fn handle_plain_key(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        shift: bool,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        match logical_key {
            Key::Named(NamedKey::Backspace) => {
                if self.selection.is_collapsed() {
                    self.delete_before(state, root);
                } else {
                    self.delete_selection(state, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::Delete) => {
                if self.selection.is_collapsed() {
                    self.delete_after(state, root);
                } else {
                    self.delete_selection(state, root);
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
                    self.selection.move_to(self.selection.min());
                } else {
                    let p = self.body.prev_boundary(self.selection.cursor);
                    self.move_or_extend(p, shift);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if !shift && !self.selection.is_collapsed() {
                    self.selection.move_to(self.selection.max());
                } else {
                    let p = self.body.next_boundary(self.selection.cursor);
                    self.move_or_extend(p, shift);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_line_up(shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_line_down(shift);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.move_home(shift);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.move_end(shift);
                *handled = true;
            }
            _ => {
                if let Some(s) = text
                    && !s.is_empty()
                    && !s.chars().all(char::is_control)
                {
                    self.insert_str(state, root, s);
                    *handled = true;
                }
            }
        }
    }
}

impl AppWidget for NotesApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
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

    /// Runs the real `Widget::draw`, not a stand-in — nothing is rasterised,
    /// only the geometry it resolves matters here.
    fn draw_tile(app: &mut NotesApp, rect: Rect) {
        app.rect = rect;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

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
        assert!(
            tile.contains(pill.center()),
            "the pill sits inside the tile"
        );
        assert!(
            (pill.center().x - tile.center().x).abs() < 1.0,
            "the zero-state is horizontally centred, not left-aligned at the padding: \
             pill centre {} vs tile centre {}",
            pill.center().x,
            tile.center().x,
        );
        // Trails a vertically-centred stack, so a top-aligned layout would
        // instead put it in the upper half.
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

    /// Once there is text, no pill is left behind claiming the pointer.
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

    #[test]
    fn shift_right_extends_selection() {
        let mut app = NotesApp::with_seed("hello");
        app.selection = Selection::collapsed(0);
        let p = app.selection.cursor + 1;
        app.selection.extend_to(p);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 1);
        assert_eq!(
            app.body.text.get(app.selection.range()).unwrap_or_default(),
            "h"
        );

        app.selection.extend_to(3);
        assert_eq!(app.selection.anchor, 0);
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(
            app.body.text.get(app.selection.range()).unwrap_or_default(),
            "hel"
        );
    }

    #[test]
    fn double_click_selects_word() {
        let app = NotesApp::with_seed("hello world quick");
        let (s, e) = word_boundaries(&app.body.text, 8);
        assert_eq!(s, 6);
        assert_eq!(e, 11);
        assert_eq!(app.body.text.get(s..e).unwrap_or_default(), "world");
    }

    #[test]
    fn ctrl_a_selects_all() {
        let mut app = NotesApp::with_seed("alpha\nbeta\ngamma");
        app.selection.anchor = 0;
        app.selection.cursor = app.body.text.len();
        assert!(!app.selection.is_collapsed());
        assert_eq!(app.selection.min(), 0);
        assert_eq!(app.selection.max(), app.body.text.len());
        assert_eq!(
            app.body.text.get(app.selection.range()).unwrap_or_default(),
            "alpha\nbeta\ngamma"
        );
    }

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

    #[test]
    fn triple_click_selects_line() {
        let app = NotesApp::with_seed("alpha\nbeta\ngamma");
        let (s, e) = line_boundaries(&app.body.text, 8);
        // Byte 8 sits inside "beta" (starts at 6, ends at 10 — the \n).
        assert_eq!(s, 6);
        assert_eq!(e, 10);
        assert_eq!(app.body.text.get(s..e).unwrap_or_default(), "beta");
    }

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

    #[test]
    fn state_backcompat_cursor_only() {
        let mut app = NotesApp::with_seed("hello");
        app.set_state("3");
        assert_eq!(app.selection.cursor, 3);
        assert_eq!(app.selection.anchor, 3);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn ctrl_c_then_ctrl_v_round_trips_through_arboard() {
        // Skip without a usable arboard backend (CI, no display).
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
            .map(std::string::ToString::to_string)
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
