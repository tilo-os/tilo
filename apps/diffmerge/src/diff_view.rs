//! Side-by-side two-way diff view.
//!
//! Renders [`DiffModel`] as two aligned columns with line-number gutters,
//! per-row add/remove/modify tinting, syntax highlighting, a change minimap on
//! the right edge, and prev/next-change navigation. Draws everything directly
//! into the Vello scene (no nested taffy children) and owns its own vertical
//! scroll offset.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{MouseScrollDelta, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::highlight;
use crate::model::{DiffModel, RowKind};

const ROW_H: f64 = 20.0;
const GUTTER_W: f64 = 52.0;
const MINIMAP_W: f64 = 14.0;
const HEADER_H: f64 = 34.0;
const FONT_SIZE: f32 = 13.0;
const PAD_X: f64 = 8.0;

/// Side-by-side diff panel.
pub struct DiffView {
    model: DiffModel,
    left_label: String,
    right_label: String,
    rect: Rect,
    scroll_y: f64,
    /// Index into `model.blocks` of the most recently navigated-to change.
    active_block: Option<usize>,
    dirty: bool,
    hovered_minimap: bool,
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            model: DiffModel::default(),
            left_label: String::new(),
            right_label: String::new(),
            rect: Rect::ZERO,
            scroll_y: 0.0,
            active_block: None,
            dirty: true,
            hovered_minimap: false,
        }
    }

    pub fn set_inputs(&mut self, left_label: &str, left: &str, right_label: &str, right: &str) {
        self.model = DiffModel::compute(left, right);
        self.left_label = left_label.to_string();
        self.right_label = right_label.to_string();
        self.scroll_y = 0.0;
        self.active_block = self.model.blocks.first().map(|_| 0);
        self.dirty = true;
    }

    pub fn model(&self) -> &DiffModel {
        &self.model
    }

    pub fn active_block(&self) -> Option<usize> {
        self.active_block
    }

    /// Jump the active change to the next/previous block and scroll it into
    /// view. Returns true if navigation occurred.
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

    fn scroll_block_into_view(&mut self, block: usize) {
        let Some(b) = self.model.blocks.get(block) else {
            return;
        };
        let block_top = b.first_row as f64 * ROW_H;
        let viewport_h = self.body_height();
        // Centre the block in the viewport where possible.
        let target = block_top - viewport_h / 2.0 + ROW_H;
        self.scroll_y = target.max(0.0).min(self.max_scroll());
    }

    fn body_height(&self) -> f64 {
        (self.rect.height() - HEADER_H).max(0.0)
    }

    fn content_height(&self) -> f64 {
        self.model.rows.len() as f64 * ROW_H
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.body_height()).max(0.0)
    }

    fn scroll_by(&mut self, dy: f64) {
        let new = (self.scroll_y + dy).max(0.0).min(self.max_scroll());
        if (new - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = new;
            self.dirty = true;
        }
    }

    /// Background tint for a row kind on a given side. `is_left` selects the
    /// add vs remove emphasis. Returns `None` for equal/phantom rows.
    fn row_tint(kind: RowKind, side_present: bool, is_left: bool, theme: &Theme) -> Option<Color> {
        let c = &theme.tokens.colors;
        let add = with_alpha(c.semantic.success, 38);
        let del = with_alpha(c.semantic.danger, 38);
        let modi = with_alpha(c.semantic.warning, 30);
        let phantom = with_alpha(c.neutral[2], 90);
        match kind {
            RowKind::Equal => None,
            RowKind::Insert => {
                if is_left {
                    Some(phantom)
                } else if side_present {
                    Some(add)
                } else {
                    Some(phantom)
                }
            }
            RowKind::Delete => {
                if is_left {
                    if side_present {
                        Some(del)
                    } else {
                        Some(phantom)
                    }
                } else {
                    Some(phantom)
                }
            }
            RowKind::Modify => Some(modi),
        }
    }
}

impl Widget for DiffView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        let c = &theme.tokens.colors;
        let r = self.rect;
        // Panel background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        let mid_x = (r.x0 + r.x1 - MINIMAP_W) / 2.0;
        let left_col = Rect::new(r.x0, r.y0, mid_x, r.y1);
        let right_col = Rect::new(mid_x, r.y0, r.x1 - MINIMAP_W, r.y1);

        // --- Column headers ---
        draw_header(scene, transform, tr, theme, left_col, &self.left_label);
        draw_header(scene, transform, tr, theme, right_col, &self.right_label);

        // Divider between columns.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(mid_x, r.y0), Point::new(mid_x, r.y1)),
        );

        // --- Body (clipped + scrolled) ---
        let body_top = r.y0 + HEADER_H;
        let body_clip = Rect::new(r.x0, body_top, r.x1 - MINIMAP_W, r.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body_clip,
        );

        let first_row = (self.scroll_y / ROW_H).floor() as usize;
        let visible_rows = (self.body_height() / ROW_H).ceil() as usize + 2;
        let last_row = (first_row + visible_rows).min(self.model.rows.len());

        for ri in first_row..last_row {
            let row = &self.model.rows[ri];
            let y = body_top + ri as f64 * ROW_H - self.scroll_y;
            let row_rect_l = Rect::new(left_col.x0, y, left_col.x1, y + ROW_H);
            let row_rect_r = Rect::new(right_col.x0, y, right_col.x1, y + ROW_H);

            // Left side tint.
            if let Some(tint) = Self::row_tint(row.kind, row.left.is_some(), true, theme) {
                scene.fill(Fill::NonZero, transform, tint, None, &row_rect_l);
            }
            // Right side tint.
            if let Some(tint) = Self::row_tint(row.kind, row.right.is_some(), false, theme) {
                scene.fill(Fill::NonZero, transform, tint, None, &row_rect_r);
            }

            // Active block highlight ring (left edge accent bar).
            if row.block.is_some() && row.block == self.active_block {
                let bar = Rect::new(r.x0, y, r.x0 + 3.0, y + ROW_H);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &bar);
            }

            // Gutter numbers.
            draw_gutter_number(scene, transform, tr, theme, left_col.x0, y, row.left_no);
            draw_gutter_number(scene, transform, tr, theme, right_col.x0, y, row.right_no);

            // Change glyph in the left gutter margin between number and text.
            let glyph = match row.kind {
                RowKind::Insert => Some(("+", c.semantic.success)),
                RowKind::Delete => Some(("-", c.semantic.danger)),
                RowKind::Modify => Some(("~", c.semantic.warning)),
                RowKind::Equal => None,
            };
            if let Some((g, gc)) = glyph {
                tr.draw_text(
                    scene,
                    g,
                    FONT_SIZE,
                    gc,
                    transform * Affine::translate((right_col.x0 + 4.0, y + 2.0)),
                    None,
                );
            }

            // Line text, syntax-highlighted.
            if let Some(text) = &row.left {
                draw_code_line(
                    scene,
                    transform,
                    tr,
                    theme,
                    left_col.x0 + GUTTER_W + PAD_X,
                    y,
                    text,
                );
            }
            if let Some(text) = &row.right {
                draw_code_line(
                    scene,
                    transform,
                    tr,
                    theme,
                    right_col.x0 + GUTTER_W + PAD_X,
                    y,
                    text,
                );
            }
        }

        scene.pop_layer();

        // --- Minimap on the right edge ---
        self.draw_minimap(scene, transform, theme);

        // --- Empty state ---
        if self.model.rows.is_empty() {
            tr.draw_text(
                scene,
                "No file loaded",
                15.0,
                c.text_dim,
                transform * Affine::translate((r.x0 + 24.0, body_top + 24.0)),
                None,
            );
        } else if self.model.is_identical() {
            tr.draw_text(
                scene,
                "Files are identical",
                14.0,
                c.semantic.success,
                transform * Affine::translate((r.x0 + 24.0, body_top + 8.0)),
                None,
            );
        }
        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _m: &winit::event::Modifiers,
        _s: &mut AutoCommit,
        _p: &ObjId,
        handled: &mut bool,
        _focus: &mut Option<NodeId>,
        _node: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-*y) * 60.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                // Only consume when the pointer is plausibly over us — but the
                // host routes wheel to the focused subtree, so just scroll.
                self.scroll_by(dy);
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                // Click on the minimap jumps to that proportion of content.
                if self.hovered_minimap {
                    // hovered_minimap is set on cursor move; recompute target.
                    // Handled via last cursor position below.
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                let minimap_x0 = self.rect.x1 - MINIMAP_W;
                self.hovered_minimap = p.x >= minimap_x0 && p.x <= self.rect.x1;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "DiffView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl DiffView {
    fn draw_minimap(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let map_x0 = r.x1 - MINIMAP_W;
        let map_top = r.y0 + HEADER_H;
        let map_h = (r.y1 - map_top).max(1.0);
        let track = Rect::new(map_x0, map_top, r.x1, r.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            with_alpha(c.neutral[1], 200),
            None,
            &track,
        );

        let total = self.model.rows.len().max(1) as f64;
        // Markers for each change block.
        for b in &self.model.blocks {
            let y0 = map_top + (b.first_row as f64 / total) * map_h;
            let y1 = map_top + ((b.last_row + 1) as f64 / total) * map_h;
            let color = match b.kind {
                crate::model::BlockKind::Added => c.semantic.success,
                crate::model::BlockKind::Removed => c.semantic.danger,
                crate::model::BlockKind::Modified => c.semantic.warning,
            };
            let marker = Rect::new(map_x0 + 2.0, y0, r.x1 - 2.0, (y1).max(y0 + 2.0));
            scene.fill(Fill::NonZero, transform, color, None, &marker);
        }

        // Viewport indicator.
        if self.content_height() > self.body_height() {
            let vt = map_top + (self.scroll_y / self.content_height()) * map_h;
            let vb =
                map_top + ((self.scroll_y + self.body_height()) / self.content_height()) * map_h;
            let vp = RoundedRect::new(map_x0, vt, r.x1, vb.max(vt + 4.0), 2.0);
            scene.stroke(&Stroke::new(1.5), transform, c.text_primary, None, &vp);
        }
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
        &Stroke::new(1.0),
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

fn draw_gutter_number(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y: f64,
    no: Option<usize>,
) {
    let c = &theme.tokens.colors;
    // Gutter background.
    let g = Rect::new(x0, y, x0 + GUTTER_W, y + ROW_H);
    scene.fill(
        Fill::NonZero,
        transform,
        with_alpha(c.neutral[1], 120),
        None,
        &g,
    );
    if let Some(n) = no {
        let s = n.to_string();
        // Right-align within the gutter.
        let approx_w = s.len() as f64 * 7.0;
        tr.draw_text(
            scene,
            &s,
            12.0,
            c.text_dim,
            transform * Affine::translate((x0 + GUTTER_W - approx_w - 6.0, y + 3.0)),
            None,
        );
    }
}

/// Draw a single line of code with syntax highlighting at `(x0, y)`.
fn draw_code_line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y: f64,
    text: &str,
) {
    let mut x = x0;
    for (seg, color) in highlight::segments(text, theme) {
        if seg.is_empty() {
            continue;
        }
        // Tabs/leading spaces are part of the segment text; cosmic-text
        // renders them and advances the cursor. We measure to advance x.
        tr.draw_text(
            scene,
            &seg,
            FONT_SIZE,
            color,
            transform * Affine::translate((x, y + 2.0)),
            None,
        );
        let (w, _) = tr.measure(&seg, FONT_SIZE, None);
        x += f64::from(w);
    }
}

/// Apply an explicit 8-bit alpha to a colour.
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}
