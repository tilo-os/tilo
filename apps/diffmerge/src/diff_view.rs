//! Side-by-side two-way diff view.
//!
//! Two **editable** panels sit side by side: the left holds the original text,
//! the right the changed version. Each is the shared monospace code editor
//! ([`ui::widget::code_editor::CodeEditor`]) — so a user can type or **paste**
//! (Ctrl+V) two texts to compare, with a caret, selection, clipboard, and undo
//! for free — and each carries the shared [`Scrollable`] overlay scrollbar
//! (wheel + a draggable semi-transparent thumb). The diff itself is recomputed
//! live from the two buffers and painted as a translucent per-line tint behind
//! the code, so changed lines light up green / red / amber as you edit.
//!
//! Nothing here hand-rolls scroll, a scrollbar, or an editor: the editors are
//! the shared `CodeEditor`, the bars are the shared `Scrollable`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{Style as TaffyStyle, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::code_editor::CodeEditor;
use ui::widget::{Scrollable, Widget};

use crate::model::{DiffModel, RowKind};

const HEADER_H: f64 = 34.0;
const FONT_SIZE: f32 = 13.0;
const PAD_X: f64 = 8.0;

/// Which panel currently owns the caret / keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Left,
    Right,
}

/// Side-by-side editable diff panel.
pub struct DiffView {
    /// The original text (left panel) — a shared, editable, monospace editor.
    left: CodeEditor,
    /// The changed text (right panel).
    right: CodeEditor,
    /// Shared overlay scrollbar for the left panel (wheel + draggable thumb).
    left_scroll: Scrollable,
    /// Shared overlay scrollbar for the right panel.
    right_scroll: Scrollable,
    left_label: String,
    right_label: String,
    /// The diff recomputed from the two buffers.
    model: DiffModel,
    /// The `(left, right)` strings the model was last computed from, so `sync`
    /// only recomputes when a buffer actually changed.
    last_src: (String, String),
    rect: Rect,
    /// The two editor body rects (below the headers), cached from `draw` for
    /// navigation scroll targets.
    left_body: Rect,
    right_body: Rect,
    /// Index into `model.blocks` of the most recently navigated-to change.
    active_block: Option<usize>,
    focus: Focus,
    /// Which panel a left-button press is dragging a selection in, if any.
    pressed: Option<bool>,
    /// Last pointer position (local space), for column hit-testing.
    cursor: Point,
    dirty: bool,
}

impl DiffView {
    pub fn new() -> Self {
        let left = CodeEditor::local()
            .with_font_size(FONT_SIZE)
            .with_placeholder("Paste or type the original text here…");
        let right = CodeEditor::local()
            .with_font_size(FONT_SIZE)
            .with_placeholder("…and the changed version here to compare.");
        Self {
            left,
            right,
            // A persistent (but semi-transparent) overlay bar: discoverable at
            // rest and draggable, since a child-less Scrollable driven from
            // `draw` never runs the sync-path flash that would otherwise reveal
            // an auto-hiding bar.
            left_scroll: Scrollable::new_vertical().always_visible(),
            right_scroll: Scrollable::new_vertical().always_visible(),
            left_label: String::new(),
            right_label: String::new(),
            model: DiffModel::default(),
            last_src: (String::new(), String::new()),
            rect: Rect::ZERO,
            left_body: Rect::ZERO,
            right_body: Rect::ZERO,
            active_block: None,
            focus: Focus::None,
            pressed: None,
            cursor: Point::ZERO,
            dirty: true,
        }
    }

    /// Seed both panels with external text (a two-file open, or the demo) and
    /// recompute the diff. Not counted as a user edit.
    pub fn set_inputs(&mut self, left_label: &str, left: &str, right_label: &str, right: &str) {
        self.left.set_text(left);
        self.right.set_text(right);
        self.left_label = left_label.to_string();
        self.right_label = right_label.to_string();
        self.recompute();
        self.active_block = self.model.blocks.first().map(|_| 0);
        self.dirty = true;
    }

    pub fn model(&self) -> &DiffModel {
        &self.model
    }

    pub fn active_block(&self) -> Option<usize> {
        self.active_block
    }

    /// Screenshot-only: focus the right panel and park a caret in its text so a
    /// still frame reads as a live, editable panel.
    pub fn focus_right_for_shots(&mut self) {
        self.focus = Focus::Right;
        self.right.set_focused(true);
        self.left.set_focused(false);
        // A caret a little into the buffer, clamped to a char boundary by core.
        let target = self.right.value().len().min(40);
        self.right.set_cursor_pos(target);
    }

    /// Recompute the diff from the two live buffers, caching the source so the
    /// per-frame `sync` can skip the work when nothing changed.
    fn recompute(&mut self) {
        let l = self.left.value().to_string();
        let r = self.right.value().to_string();
        self.model = DiffModel::compute(&l, &r);
        self.last_src = (l, r);
    }

    /// Recompute only if a buffer changed since the last computation.
    fn recompute_if_changed(&mut self) -> bool {
        if self.left.value() != self.last_src.0 || self.right.value() != self.last_src.1 {
            self.recompute();
            // A structural change can invalidate the active block index; clamp it.
            if let Some(b) = self.active_block {
                if b >= self.model.blocks.len() {
                    self.active_block = self.model.blocks.first().map(|_| 0);
                }
            } else {
                self.active_block = self.model.blocks.first().map(|_| 0);
            }
            true
        } else {
            false
        }
    }

    /// Jump the active change to the next/previous block and scroll both panels
    /// so it is in view. Returns true if navigation occurred.
    pub fn navigate(&mut self, forward: bool) -> bool {
        if self.model.blocks.is_empty() {
            return false;
        }
        let n = self.model.blocks.len();
        let next = match self.active_block {
            None => 0,
            Some(cur) => {
                if forward {
                    (cur + 1).min(n - 1)
                } else {
                    cur.saturating_sub(1)
                }
            }
        };
        self.active_block = Some(next);
        self.scroll_block_into_view(next);
        self.dirty = true;
        true
    }

    /// Scroll both editors so change `block`'s first row is roughly centred.
    fn scroll_block_into_view(&mut self, block: usize) {
        let Some(b) = self.model.blocks.get(block).copied() else {
            return;
        };
        // Map the block's first aligned row to a per-side line number.
        let (left_line, right_line) = self
            .model
            .rows
            .get(b.first_row)
            .map(|row| (row.left_no, row.right_no))
            .unwrap_or((None, None));
        if let Some(n) = left_line {
            scroll_editor_to_line(&mut self.left, self.left_body, n.saturating_sub(1));
        }
        if let Some(n) = right_line {
            scroll_editor_to_line(&mut self.right, self.right_body, n.saturating_sub(1));
        }
    }

    /// Column split: `(left_col, right_col)` full-height rects.
    fn columns(&self) -> (Rect, Rect) {
        let r = self.rect;
        let mid = (r.x0 + r.x1) / 2.0;
        (
            Rect::new(r.x0, r.y0, mid, r.y1),
            Rect::new(mid, r.y0, r.x1, r.y1),
        )
    }

    /// The editor body rect within a column (below its header).
    fn body_of(col: Rect) -> Rect {
        Rect::new(col.x0, col.y0 + HEADER_H, col.x1, col.y1)
    }

    /// Per-side translucent line tints from the current diff: `(line0, colour)`.
    fn side_tints(&self, left: bool, theme: &Theme) -> Vec<(usize, Color)> {
        let c = &theme.tokens.colors;
        let mut out = Vec::new();
        for row in &self.model.rows {
            let picked = if left {
                match row.kind {
                    RowKind::Delete => row.left_no.map(|n| (n, c.semantic.danger)),
                    RowKind::Modify => row.left_no.map(|n| (n, c.semantic.warning)),
                    _ => None,
                }
            } else {
                match row.kind {
                    RowKind::Insert => row.right_no.map(|n| (n, c.semantic.success)),
                    RowKind::Modify => row.right_no.map(|n| (n, c.semantic.warning)),
                    _ => None,
                }
            };
            if let Some((n, col)) = picked {
                out.push((n.saturating_sub(1), col));
            }
        }
        out
    }

    /// Configure a panel's overlay scrollbar from its editor's live geometry and
    /// composite it over the just-drawn code.
    fn draw_bar(
        scroll: &mut Scrollable,
        editor: &CodeEditor,
        body: Rect,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        scroll.set_viewport_rect(body);
        scroll.set_content_size(body.width() as f32, editor.inner().content_height() as f32);
        scroll.set_offset_y(editor.inner().scroll_y() as f32);
        scroll.draw_bar_overlay(scene, transform, theme);
    }

    /// After the shared bar processed a press/drag, mirror its offset back into
    /// the editor so the code scrolls with the thumb.
    fn sync_editor_to_bar(editor: &mut CodeEditor, scroll: &Scrollable) {
        let delta = scroll.offset_y() as f64 - editor.inner().scroll_y();
        if delta.abs() > f64::EPSILON {
            editor.scroll_by(delta);
        }
    }
}

/// Forward one raw event into a child-less [`Scrollable`] (for thumb-drag /
/// track interaction) using a throwaway taffy node — the bar needs no real
/// layout tree since it neither lays out nor draws a child.
fn feed_scroll(
    scroll: &mut Scrollable,
    event: &WindowEvent,
    scale_factor: f64,
    modifiers: &winit::event::Modifiers,
    state: &mut AutoCommit,
    path: &ObjId,
) -> bool {
    let mut handled = false;
    let mut ft = None;
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let node = taffy.new_leaf(TaffyStyle::default()).unwrap();
    scroll.handle_event(
        event, scale_factor, modifiers, state, path, &mut handled, &mut ft, node, &taffy,
    );
    handled
}

/// Forward one raw event into an editor using a throwaway taffy node. The editor
/// is already bound to its own (local) buffer, so `path` is only a fallback.
fn feed_editor(
    editor: &mut CodeEditor,
    event: &WindowEvent,
    scale_factor: f64,
    modifiers: &winit::event::Modifiers,
    state: &mut AutoCommit,
    path: &ObjId,
) -> bool {
    let mut handled = false;
    let mut ft = None;
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let node = taffy.new_leaf(TaffyStyle::default()).unwrap();
    editor.handle_event(
        event, scale_factor, modifiers, state, path, &mut handled, &mut ft, node, &taffy,
    );
    handled
}

/// Scroll `editor` so 0-based `line0` sits ~40% down its viewport.
fn scroll_editor_to_line(editor: &mut CodeEditor, body: Rect, line0: usize) {
    let lh = editor.line_height().max(1.0);
    let target = line0 as f64 * lh - body.height() * 0.4;
    let cur = editor.inner().scroll_y();
    editor.scroll_by(target - cur);
}

/// Byte offset of 0-based `line0` in `text`.
fn byte_of_line(text: &str, line0: usize) -> usize {
    if line0 == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line0 {
                return i + 1;
            }
        }
    }
    text.len()
}

impl Widget for DiffView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Reconcile the diff with whatever the user typed / pasted since the
        // last frame.
        if self.recompute_if_changed() {
            self.dirty = true;
        }
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
                flex_grow: 1.0,
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
        let (lc, rc) = self.columns();
        self.left_body = Self::body_of(lc);
        self.right_body = Self::body_of(rc);
        self.left.set_rect(self.left_body);
        self.right.set_rect(self.right_body);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        let (left_col, right_col) = self.columns();
        let left_body = Self::body_of(left_col);
        let right_body = Self::body_of(right_col);
        self.left_body = left_body;
        self.right_body = right_body;

        // Column headers.
        draw_header(scene, transform, tr, theme, left_col, &self.left_label);
        draw_header(scene, transform, tr, theme, right_col, &self.right_label);

        // Divider between columns.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(right_col.x0, r.y0), Point::new(right_col.x0, r.y1)),
        );

        // Diff tints — painted first, behind the (transparent-chrome) editors,
        // so changed code reads on top of a coloured band. Computed before the
        // mutable editor borrows.
        let left_tints = self.side_tints(true, theme);
        let right_tints = self.side_tints(false, theme);
        draw_tints(scene, transform, theme, &self.left, left_body, &left_tints);
        draw_tints(scene, transform, theme, &self.right, right_body, &right_tints);

        // Editors (each clips + scrolls itself, and paints its own gutter,
        // caret, selection, and monospace text).
        self.left.set_rect(left_body);
        self.right.set_rect(right_body);
        self.left.set_focused(self.focus == Focus::Left);
        self.right.set_focused(self.focus == Focus::Right);
        self.left.draw_embedded(scene, transform, tr, theme);
        self.right.draw_embedded(scene, transform, tr, theme);

        // Overlay scrollbars (wheel + draggable thumb), configured from the
        // editors' live content/scroll.
        Self::draw_bar(&mut self.left_scroll, &self.left, left_body, scene, transform, theme);
        Self::draw_bar(&mut self.right_scroll, &self.right, right_body, scene, transform, theme);

        // "Identical" affirmation when both sides carry content but nothing
        // differs. (Empty panels show the editors' own placeholders.)
        if !self.left.value().is_empty()
            && !self.right.value().is_empty()
            && self.model.is_identical()
        {
            tr.draw_text(
                scene,
                "Files are identical",
                13.0,
                c.semantic.success,
                transform * Affine::translate((r.x0 + 24.0, r.y0 + HEADER_H + 10.0)),
                None,
            );
        }

        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        m: &winit::event::Modifiers,
        s: &mut AutoCommit,
        p: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let mid_x = (self.rect.x0 + self.rect.x1) / 2.0;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p_local = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.cursor = p_local;
                // A live thumb drag owns the pointer until release.
                if self.left_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.left_scroll, event, scale_factor, m, s, p);
                    Self::sync_editor_to_bar(&mut self.left, &self.left_scroll);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                if self.right_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.right_scroll, event, scale_factor, m, s, p);
                    Self::sync_editor_to_bar(&mut self.right, &self.right_scroll);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // An in-progress text selection drag extends in its panel.
                if let Some(on_left) = self.pressed {
                    let ed = if on_left { &mut self.left } else { &mut self.right };
                    feed_editor(ed, event, scale_factor, m, s, p);
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Otherwise let the bar under the pointer track hover (reveals
                // the overlay bar near the edge); never consumes.
                let scroll = if p_local.x < mid_x { &mut self.left_scroll } else { &mut self.right_scroll };
                feed_scroll(scroll, event, scale_factor, m, s, p);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-*y as f64) * 60.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                // Two aligned panels scroll together on the wheel.
                self.left.scroll_by(dy);
                self.right.scroll_by(dy);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let on_left = self.cursor_side(mid_x);
                let scroll = if on_left { &mut self.left_scroll } else { &mut self.right_scroll };
                // Offer the press to the panel's scrollbar first (thumb/track).
                feed_scroll(scroll, event, scale_factor, m, s, p);
                if scroll.wants_pointer_capture() {
                    if on_left {
                        Self::sync_editor_to_bar(&mut self.left, &self.left_scroll);
                    } else {
                        Self::sync_editor_to_bar(&mut self.right, &self.right_scroll);
                    }
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Otherwise place the caret / start a selection in that panel.
                self.focus = if on_left { Focus::Left } else { Focus::Right };
                self.left.set_focused(on_left);
                self.right.set_focused(!on_left);
                let ed = if on_left { &mut self.left } else { &mut self.right };
                feed_editor(ed, event, scale_factor, m, s, p);
                self.pressed = Some(on_left);
                *focus = Some(node);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.left_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.left_scroll, event, scale_factor, m, s, p);
                    *handled = true;
                } else if self.right_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.right_scroll, event, scale_factor, m, s, p);
                    *handled = true;
                }
                if let Some(on_left) = self.pressed.take() {
                    let ed = if on_left { &mut self.left } else { &mut self.right };
                    feed_editor(ed, event, scale_factor, m, s, p);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { .. } => {
                // Route typing / paste / caret motion to the focused panel.
                let handled_here = match self.focus {
                    Focus::Left => feed_editor(&mut self.left, event, scale_factor, m, s, p),
                    Focus::Right => feed_editor(&mut self.right, event, scale_factor, m, s, p),
                    Focus::None => false,
                };
                if handled_here {
                    if self.recompute_if_changed() {
                        // model refreshed; status/tint update next draw
                    }
                    self.dirty = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "DiffView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        // The editors are childless leaves (default `needs_redraw == true`), so
        // we do NOT OR them in — that would pin a constant repaint. Instead each
        // consumed edit / scroll flips `self.dirty`, and the overlay bars report
        // their own scroll/fade animation.
        self.dirty || self.left_scroll.needs_redraw() || self.right_scroll.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.left_scroll.needs_animation_frame() || self.right_scroll.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.left_scroll.mark_drawn();
        self.right_scroll.mark_drawn();
    }
}

impl DiffView {
    /// Which panel the last cursor position is over (`true` = left).
    fn cursor_side(&self, mid_x: f64) -> bool {
        self.cursor.x < mid_x
    }
}

/// Draw a column header bar with its label.
fn draw_header(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    col: Rect,
    label: &str,
) {
    let c = &theme.tokens.colors;
    let bar = Rect::new(col.x0, col.y0, col.x1, col.y0 + HEADER_H);
    scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        c.border_standard,
        None,
        &Line::new(
            Point::new(col.x0, col.y0 + HEADER_H),
            Point::new(col.x1, col.y0 + HEADER_H),
        ),
    );
    if !label.is_empty() {
        tr.draw_text(
            scene,
            label,
            13.0,
            c.text_primary,
            transform * Affine::translate((col.x0 + PAD_X + 4.0, col.y0 + 9.0)),
            Some((col.width() - 2.0 * PAD_X) as f32),
        );
    }
}

/// Paint the per-line diff tints for one panel behind its code, clipped to the
/// editor body. Line positions come from the editor's own caret geometry, so
/// they track its scroll and wrapping exactly.
fn draw_tints(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    editor: &CodeEditor,
    body: Rect,
    tints: &[(usize, Color)],
) {
    if tints.is_empty() {
        return;
    }
    let text = editor.value();
    let gutter = editor.gutter_width();
    let lh = editor.line_height();
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
    for &(line0, col) in tints {
        let byte = byte_of_line(text, line0);
        let cr = editor.caret_rect(theme, byte);
        let y0 = cr.y0;
        if y0 + lh < body.y0 || y0 > body.y1 {
            continue;
        }
        let rect = Rect::new(body.x0 + gutter, y0, body.x1, y0 + lh);
        scene.fill(Fill::NonZero, transform, with_alpha(col, 46), None, &rect);
    }
    scene.pop_layer();
}

/// Apply an explicit 8-bit alpha to a colour.
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use ui::widget::ScrollAxis;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, KeyEvent, Modifiers};
    use winit::keyboard::{Key, ModifiersState, SmolStr};

    /// Run one real five-pass frame so the editors lay out (their content
    /// height becomes known) and the overlay scrollbars get configured.
    fn run_frame(view: &mut DiffView, w: f32, h: f32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = view.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(w), height: Dimension::length(h) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let mm = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = mm.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        view.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        view.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn char_event(s: &str) -> WindowEvent {
        // Mirrors the toolkit's own headless key-event helper: KeyEvent has
        // private fields, so we zero it and set the observable ones.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: PhysicalPosition::new(x, y) }
    }

    fn mouse(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left }
    }

    fn send(view: &mut DiffView, ev: &WindowEvent, mods: &Modifiers) -> bool {
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        view.handle_event(ev, 1.0, mods, &mut doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }

    /// Both panels are real editors: clicking to focus and typing edits the
    /// left buffer, and a Ctrl+V paste lands in the right buffer — proving the
    /// diff inputs are live, editable, and pasteable (not read-only).
    #[test]
    fn both_panels_accept_typed_and_pasted_edits() {
        let mut view = DiffView::new();
        run_frame(&mut view, 900.0, 400.0);
        let mid = (view.rect.x0 + view.rect.x1) / 2.0;

        // Focus the LEFT panel and type.
        let lx = view.rect.x0 + 40.0;
        let by = view.rect.y0 + HEADER_H + 24.0;
        send(&mut view, &moved(lx, by), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Pressed), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Released), &Modifiers::default());
        for ch in ["h", "e", "l", "l", "o"] {
            send(&mut view, &char_event(ch), &Modifiers::default());
        }
        assert_eq!(view.left.value(), "hello", "the left panel accepts typed edits");

        // Focus the RIGHT panel and paste.
        ui::clipboard_copy("pasted-into-right");
        let rx = view.rect.x0 + view.rect.width() * 0.75;
        send(&mut view, &moved(rx, by), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Pressed), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Released), &Modifiers::default());
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        send(&mut view, &char_event("v"), &ctrl);
        assert!(
            view.right.value().contains("pasted-into-right"),
            "the right panel accepts a paste, got {:?}",
            view.right.value()
        );
        // The two panels are independent buffers.
        assert_eq!(view.left.value(), "hello", "left kept its own text");
        assert!(rx > mid, "sanity: the right click landed in the right column");
    }

    /// The panels scroll through the SHARED overlay scrollbar, and its thumb is
    /// draggable: with content taller than the viewport a thumb exists, pressing
    /// it captures the pointer, and dragging it down scrolls the editor.
    #[test]
    fn panel_scrollbar_thumb_drag_scrolls() {
        let mut view = DiffView::new();
        let long: String = (0..80).map(|i| format!("line {i}\n")).collect();
        view.set_inputs("orig", &long, "changed", &format!("{long}extra\n"));
        run_frame(&mut view, 900.0, 300.0);

        let thumb = view
            .left_scroll
            .thumb_rect(ScrollAxis::Vertical)
            .expect("the overflowing left panel draws a draggable thumb");
        let center = thumb.center();
        let before = view.left.inner().scroll_y();
        assert_eq!(before, 0.0, "starts scrolled to the top");

        // Hover the thumb, press it (captures), drag down, release.
        send(&mut view, &moved(center.x, center.y), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Pressed), &Modifiers::default());
        assert!(
            view.left_scroll.wants_pointer_capture(),
            "pressing the thumb starts a drag (pointer capture)"
        );
        send(&mut view, &moved(center.x, center.y + 120.0), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Released), &Modifiers::default());

        let after = view.left.inner().scroll_y();
        assert!(
            after > before + 1.0,
            "dragging the thumb scrolled the panel: {before} -> {after}"
        );
    }

    /// Editing a panel re-runs the diff live: after seeding identical text (no
    /// changes), appending a line to the right makes the model report the
    /// addition — the diff tracks the editable buffers, not a frozen snapshot.
    #[test]
    fn editing_recomputes_the_diff_live() {
        let mut view = DiffView::new();
        view.set_inputs("a", "one\ntwo\n", "b", "one\ntwo\n");
        assert!(view.model().is_identical(), "identical seed has no changes");

        // Focus + type an extra line into the right panel.
        run_frame(&mut view, 900.0, 400.0);
        let rx = view.rect.x0 + view.rect.width() * 0.75;
        let by = view.rect.y0 + HEADER_H + 24.0;
        send(&mut view, &moved(rx, by), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Pressed), &Modifiers::default());
        send(&mut view, &mouse(ElementState::Released), &Modifiers::default());
        // Jump to the very end so the append lands after the existing text.
        view.right.select_all();
        view.right.set_cursor_pos(view.right.value().len());
        for ch in ["x", "\n"] {
            send(&mut view, &char_event(ch), &Modifiers::default());
        }
        // Reconcile as the frame loop's sync would.
        view.sync(&mut automerge::AutoCommit::new(), &automerge::ROOT);
        assert!(
            !view.model().is_identical(),
            "a live edit produced a diff, model added={} modified={}",
            view.model().added,
            view.model().modified
        );
    }
}
