//! Side-by-side two-way diff view.
//!
//! Renders [`DiffModel`] as two aligned columns with line-number gutters,
//! per-row add/remove/modify tinting, syntax highlighting, a change minimap on
//! the right edge, and prev/next-change navigation. Draws everything directly
//! into the Vello scene (no nested taffy children) and owns its own vertical
//! scroll offset.

use std::collections::HashMap;

use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{SegmentedFace, ToggleGroup, Widget};

use crate::highlight;
use crate::model::{DiffModel, RowKind};
use ui::paint::{PaintCtx, with_alpha};
use ui::widget::code_editor::Span;

/// Minimum row pitch; the measured line height of `FONT_SIZE` wins when the
/// text scale makes it taller (see [`Metrics`]).
const ROW_H: f64 = 20.0;
/// Minimum line-number gutter; grows with the measured digit width.
const GUTTER_W: f64 = 52.0;
const MINIMAP_W: f64 = 14.0;
/// Minimum column header band.
const HEADER_H: f64 = 34.0;
const FONT_SIZE: f32 = 13.0;
const HEADER_FONT_SIZE: f32 = 13.0;
const GUTTER_FONT_SIZE: f32 = 12.0;
const PAD_X: f64 = 8.0;
const COMPACT_BREAKPOINT: f64 = 640.0;

/// Row metrics measured from the renderer the panel paints with. Refreshed at
/// the top of every draw and read by scrolling, block rects (accessibility
/// bounds), the compact source switcher and the paint, so all of them follow
/// the same line height.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    row_h: f64,
    header_h: f64,
    /// Top offset that centres a `FONT_SIZE` line inside `row_h`.
    text_dy: f64,
    gutter_w: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            header_h: HEADER_H,
            text_dy: 2.0,
            gutter_w: GUTTER_W,
        }
    }
}

impl Metrics {
    fn measure(tr: &mut TextRenderer) -> Self {
        let line_h = f64::from(tr.measure("Ag", FONT_SIZE, None).1);
        let row_h = (line_h + 3.0).max(ROW_H);
        let header_label_h = f64::from(tr.measure("Ag", HEADER_FONT_SIZE, None).1);
        let header_h = (header_label_h + 16.0).max(HEADER_H);
        let gutter_w =
            (f64::from(tr.measure("0000", GUTTER_FONT_SIZE, None).0) + 16.0).max(GUTTER_W);
        Self {
            row_h,
            header_h,
            text_dy: (row_h - line_h) / 2.0,
            gutter_w,
        }
    }
}

/// Build a single-leaf taffy node. `new_leaf` only fails when the tree itself
/// is corrupted, which cannot happen for a tree we just built in this same
/// function — mirrors the established `TaffyTreeExt` convention used by `ui`.
#[allow(
    clippy::unwrap_used,
    reason = "new_leaf cannot fail for a freshly-built tree; see doc comment"
)]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}

/// Side-by-side diff panel.
pub struct DiffView {
    model: DiffModel,
    left_label: String,
    right_label: String,
    /// Syntax spans per source line of each side, tokenized as a whole file.
    left_spans: Vec<Vec<Span>>,
    right_spans: Vec<Vec<Span>>,
    rect: Rect,
    scroll_y: f64,
    /// Index into `model.blocks` of the most recently navigated-to change.
    active_block: Option<usize>,
    dirty: bool,
    hovered_minimap: bool,
    compact_show_right: bool,
    compact_source_hits: [Rect; 2],
    cursor: Point,
    /// Last measured row metrics (see [`Metrics`]); defaults until the first draw.
    metrics: Metrics,
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
            left_spans: Vec::new(),
            right_spans: Vec::new(),
            rect: Rect::ZERO,
            scroll_y: 0.0,
            active_block: None,
            dirty: true,
            hovered_minimap: false,
            compact_show_right: false,
            compact_source_hits: [Rect::ZERO; 2],
            cursor: Point::ZERO,
            metrics: Metrics::default(),
        }
    }

    /// Place the panel at `rect` directly (the parent app owns the toolbar
    /// band above it and re-places the view when that band's measured height
    /// changes, so paint and layout agree within one frame).
    pub(crate) fn place(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
    }

    /// `(row_h, header_h, gutter_w)` from the last draw.
    #[cfg(test)]
    pub(crate) fn metrics_for_test(&self) -> (f64, f64, f64) {
        (
            self.metrics.row_h,
            self.metrics.header_h,
            self.metrics.gutter_w,
        )
    }

    pub fn set_inputs(&mut self, left_label: &str, left: &str, right_label: &str, right: &str) {
        self.model = DiffModel::compute(left, right);
        self.left_label = left_label.to_string();
        self.right_label = right_label.to_string();
        self.left_spans =
            highlight::document(&highlight::grammar_for_label(left_label), left.lines());
        self.right_spans =
            highlight::document(&highlight::grammar_for_label(right_label), right.lines());
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

    pub(crate) fn resource_ctx(&self) -> Option<crate::actions::DiffResourceCtx> {
        (!self.model.blocks.is_empty()).then(|| crate::actions::DiffResourceCtx {
            left_label: self.left_label.clone(),
            right_label: self.right_label.clone(),
            model: self.model.clone(),
        })
    }

    /// Navigate to one canonical change-block address. Invalid or stale
    /// addresses are ignored instead of jumping to a different block.
    pub fn block_fingerprint(&self, block: usize) -> Option<String> {
        crate::actions::diff_block_fingerprint(&self.model, block)
    }

    pub fn show_block(&mut self, block: usize, fingerprint: &str) -> bool {
        if self.block_fingerprint(block).as_deref() != Some(fingerprint) {
            return false;
        }
        self.active_block = Some(block);
        self.scroll_block_into_view(block);
        self.dirty = true;
        true
    }

    pub fn select_compact_side(&mut self, show_right: bool) {
        if self.compact_show_right != show_right {
            self.compact_show_right = show_right;
            self.dirty = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn compact_showing_right(&self) -> bool {
        self.compact_show_right
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
        let row_h = self.metrics.row_h;
        let block_top = b.first_row as f64 * row_h;
        let viewport_h = self.body_height();
        // Centre the block in the viewport where possible.
        let target = block_top - viewport_h / 2.0 + row_h;
        self.scroll_y = target.max(0.0).min(self.max_scroll());
    }

    fn body_height(&self) -> f64 {
        (self.rect.height() - self.metrics.header_h).max(0.0)
    }

    fn content_height(&self) -> f64 {
        self.model.rows.len() as f64 * self.metrics.row_h
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

    fn is_compact(&self) -> bool {
        self.rect.width() < COMPACT_BREAKPOINT
    }

    /// Background tint for a row kind on a given side. `is_left` selects the
    /// add vs remove emphasis. Returns `None` for equal/phantom rows.
    fn row_tint(kind: RowKind, side_present: bool, is_left: bool, theme: &Theme) -> Option<Color> {
        let c = &theme.tokens.colors;
        let add = with_alpha(c.semantic.success, 38);
        let del = with_alpha(c.semantic.danger, 38);
        let modi = with_alpha(c.semantic.warning, 30);
        let phantom = with_alpha(c.lift(1.0), 90);
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
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.place(Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        ));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // One metric snapshot per frame: rows, gutter, header and scroll clamp
        // all follow the renderer's actual line height.
        let measured = Metrics::measure(tr);
        if measured != self.metrics {
            self.metrics = measured;
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
        let c = &theme.tokens.colors;
        let r = self.rect;
        // Panel background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);
        if self.is_compact() {
            self.draw_compact(scene, transform, tr, theme);
            self.dirty = false;
            return;
        }

        let mid_x = (r.x0 + r.x1 - MINIMAP_W) / 2.0;
        let left_col = Rect::new(r.x0, r.y0, mid_x, r.y1);
        let right_col = Rect::new(mid_x, r.y0, r.x1 - MINIMAP_W, r.y1);

        // --- Column headers ---
        let header_h = self.metrics.header_h;
        draw_header(
            scene,
            transform,
            tr,
            theme,
            left_col,
            &self.left_label,
            header_h,
        );
        draw_header(
            scene,
            transform,
            tr,
            theme,
            right_col,
            &self.right_label,
            header_h,
        );

        // Divider between columns.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(mid_x, r.y0), Point::new(mid_x, r.y1)),
        );

        // --- Body (clipped + scrolled) ---
        let body_top = r.y0 + header_h;
        let body_clip = Rect::new(r.x0, body_top, r.x1 - MINIMAP_W, r.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body_clip,
        );
        self.draw_visible_rows(scene, transform, tr, theme);
        scene.pop_layer();

        // --- Minimap on the right edge ---
        self.draw_minimap(scene, transform, theme);

        // --- Empty state ---
        self.draw_empty_state(scene, transform, tr, theme, r, body_top);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.is_compact() =>
            {
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.select_compact_side(false),
                    Key::Named(NamedKey::ArrowRight) => self.select_compact_side(true),
                    _ => return,
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-*y) * 60.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                // Only consume when the pointer is plausibly over us — but the
                // host routes wheel to the focused subtree, so just scroll.
                self.scroll_by(dy);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_compact()
                    && let Some(index) = self
                        .compact_source_hits
                        .iter()
                        .position(|rect| rect.contains(self.cursor))
                {
                    self.select_compact_side(index == 1);
                    *ctx.handled = true;
                    return;
                }
                // Click on the minimap jumps to that proportion of content.
                if self.hovered_minimap {
                    // hovered_minimap is set on cursor move; recompute target.
                    // Handled via last cursor position below.
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.cursor = p;
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let changes = self.model.blocks.len();
        let mut children = Vec::new();
        if self.is_compact() {
            children.extend([(false, "Left"), (true, "Right")].into_iter().map(
                |(show_right, label)| {
                    AccessNode::new(Role::ToggleButton)
                        .with_name(format!("Show {label} file"))
                        .with_description("Switch the visible local diff source")
                        .with_action_target(format!(
                            "diffmerge.diff-source:{}",
                            if show_right { "right" } else { "left" }
                        ))
                        .with_bounds(
                            self.compact_source_hits
                                .get(usize::from(show_right))
                                .copied()
                                .unwrap_or(Rect::ZERO),
                        )
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(self.compact_show_right == show_right),
                        )
                },
            ));
        }
        children.extend(self.model.blocks.iter().enumerate().map(|(index, block)| {
            AccessNode::new(Role::Button)
                .with_name(format!(
                    "Change {}: lines {} through {}",
                    index.saturating_add(1),
                    block.first_row.saturating_add(1),
                    block.last_row.saturating_add(1)
                ))
                .with_description("Show this local change block")
                .with_action_target(format!(
                    "diffmerge.diff-block:{index}:{}",
                    self.block_fingerprint(index).unwrap_or_default()
                ))
                .with_bounds(self.block_rect(block))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .selected(self.active_block == Some(index)),
                )
        }));
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(format!(
                    "Diff between {} and {}",
                    self.left_label, self.right_label
                ))
                .with_description(format!(
                    "{changes} change{}. Scroll to review the aligned source lines.",
                    if changes == 1 { "" } else { "s" }
                ))
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl DiffView {
    /// Column rects for the two source panes, excluding the minimap strip.
    fn column_rects(&self) -> (Rect, Rect) {
        let r = self.rect;
        let mid_x = (r.x0 + r.x1 - MINIMAP_W) / 2.0;
        (
            Rect::new(r.x0, r.y0, mid_x, r.y1),
            Rect::new(mid_x, r.y0, r.x1 - MINIMAP_W, r.y1),
        )
    }

    fn block_rect(&self, block: &crate::model::ChangeBlock) -> Rect {
        let m = self.metrics;
        let top = self.rect.y0 + m.header_h + block.first_row as f64 * m.row_h - self.scroll_y;
        let bottom = self.rect.y0 + m.header_h + block.last_row.saturating_add(1) as f64 * m.row_h
            - self.scroll_y;
        Rect::new(
            self.rect.x0,
            top.max(self.rect.y0 + m.header_h),
            self.rect.x1,
            bottom.min(self.rect.y1).max(top),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "compact painting owns the same source-selector geometry used by pointer and assistive input"
    )]
    fn draw_compact(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let m = self.metrics;
        let content_right = r.x1 - MINIMAP_W;
        let mid = r.x0.midpoint(content_right);
        self.compact_source_hits = [
            Rect::new(r.x0 + 6.0, r.y0 + 4.0, mid - 3.0, r.y0 + m.header_h - 4.0),
            Rect::new(
                mid + 3.0,
                r.y0 + 4.0,
                content_right - 6.0,
                r.y0 + m.header_h - 4.0,
            ),
        ];
        let labels = ["Left", "Right"];
        ToggleGroup::paint(
            scene,
            transform,
            tr,
            theme,
            &SegmentedFace::new(
                &self.compact_source_hits,
                &labels,
                usize::from(self.compact_show_right),
            )
            .pill()
            .hovered(
                self.compact_source_hits
                    .iter()
                    .position(|rect| rect.contains(self.cursor)),
            ),
        );
        let body_top = r.y0 + m.header_h;
        let clip = Rect::new(r.x0, body_top, content_right, r.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        let first = (self.scroll_y / m.row_h).floor() as usize;
        let last =
            (first + (self.body_height() / m.row_h).ceil() as usize + 2).min(self.model.rows.len());
        for index in first..last {
            let Some(row) = self.model.rows.get(index) else {
                continue;
            };
            let y = body_top + index as f64 * m.row_h - self.scroll_y;
            let (present, number, text, left, spans) = if self.compact_show_right {
                (
                    row.right.is_some(),
                    row.right_no,
                    row.right.as_ref(),
                    false,
                    &self.right_spans,
                )
            } else {
                (
                    row.left.is_some(),
                    row.left_no,
                    row.left.as_ref(),
                    true,
                    &self.left_spans,
                )
            };
            let rect = Rect::new(r.x0, y, content_right, y + m.row_h);
            if let Some(tint) = Self::row_tint(row.kind, present, left, theme) {
                scene.fill(Fill::NonZero, transform, tint, None, &rect);
            }
            if row.block == self.active_block && row.block.is_some() {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.accent[5],
                    None,
                    &Rect::new(r.x0, y, r.x0 + 3.0, y + m.row_h),
                );
            }
            draw_gutter_number(scene, transform, tr, theme, r.x0, y, number, m);
            if let Some(text) = text {
                highlight::draw_line(
                    &mut PaintCtx::new(scene, transform, tr, theme),
                    Point::new(r.x0 + m.gutter_w + PAD_X, y + m.text_dy),
                    text,
                    line_spans(spans, number),
                    FONT_SIZE,
                );
            }
        }
        scene.pop_layer();
        self.draw_minimap(scene, transform, theme);
        self.draw_empty_state(scene, transform, tr, theme, r, body_top);
    }

    /// Draw the scrolled window of visible rows into the already-clipped body
    /// layer. `ri` is bounds-checked via `get` rather than indexed directly:
    /// `last_row` is derived from `self.model.rows.len()` so it is always in
    /// range today, but a future change to that arithmetic must degrade
    /// (skip the row) rather than panic on a third-party diff's row count.
    fn draw_visible_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let m = self.metrics;
        let (left_col, right_col) = self.column_rects();
        let body_top = r.y0 + m.header_h;

        let first_row = (self.scroll_y / m.row_h).floor() as usize;
        let visible_rows = (self.body_height() / m.row_h).ceil() as usize + 2;
        let last_row = (first_row + visible_rows).min(self.model.rows.len());

        for ri in first_row..last_row {
            let Some(row) = self.model.rows.get(ri) else {
                continue;
            };
            let y = body_top + ri as f64 * m.row_h - self.scroll_y;
            let row_rect_l = Rect::new(left_col.x0, y, left_col.x1, y + m.row_h);
            let row_rect_r = Rect::new(right_col.x0, y, right_col.x1, y + m.row_h);

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
                let bar = Rect::new(r.x0, y, r.x0 + 3.0, y + m.row_h);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &bar);
            }

            // Gutter numbers.
            draw_gutter_number(scene, transform, tr, theme, left_col.x0, y, row.left_no, m);
            draw_gutter_number(
                scene,
                transform,
                tr,
                theme,
                right_col.x0,
                y,
                row.right_no,
                m,
            );

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
                    transform * Affine::translate((right_col.x0 + 4.0, y + m.text_dy)),
                    None,
                );
            }

            // Line text, syntax-highlighted.
            if let Some(text) = &row.left {
                highlight::draw_line(
                    &mut PaintCtx::new(scene, transform, tr, theme),
                    Point::new(left_col.x0 + m.gutter_w + PAD_X, y + m.text_dy),
                    text,
                    line_spans(&self.left_spans, row.left_no),
                    FONT_SIZE,
                );
            }
            if let Some(text) = &row.right {
                highlight::draw_line(
                    &mut PaintCtx::new(scene, transform, tr, theme),
                    Point::new(right_col.x0 + m.gutter_w + PAD_X, y + m.text_dy),
                    text,
                    line_spans(&self.right_spans, row.right_no),
                    FONT_SIZE,
                );
            }
        }
    }

    /// The "no file loaded" / "files are identical" placeholder, shown only
    /// when there is nothing else in the body to look at.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        body_top: f64,
    ) {
        let c = &theme.tokens.colors;
        if self.model.rows.is_empty() {
            // The only primary way to begin is the shared, keyboard-focusable
            // Open files toolbar action above. Centre its guidance in the
            // otherwise deliberately empty canvas so it reads as a starting
            // state rather than a missing left column.
            let title = "Start a comparison";
            let detail = tr.elide_to_width(
                "Open two local text files, or load the included sample to explore a real diff.",
                13.0,
                (r.width() - 48.0).max(0.0) as f32,
            );
            let (title_w, _) = tr.measure(title, 18.0, None);
            let (detail_w, _) = tr.measure(&detail, 13.0, None);
            let title_x = r.x0 + (r.width() - f64::from(title_w)).max(24.0) / 2.0;
            let detail_x = r.x0 + (r.width() - f64::from(detail_w)).max(24.0) / 2.0;
            let y = body_top + (r.y1 - body_top) * 0.34;
            tr.draw_text(
                scene,
                title,
                18.0,
                c.text_primary,
                transform * Affine::translate((title_x, y)),
                None,
            );
            tr.draw_text(
                scene,
                &detail,
                13.0,
                c.text_dim,
                transform * Affine::translate((detail_x, y + 30.0)),
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
    }

    fn draw_minimap(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let map_x0 = r.x1 - MINIMAP_W;
        let map_top = r.y0 + self.metrics.header_h;
        let map_h = (r.y1 - map_top).max(1.0);
        let track = Rect::new(map_x0, map_top, r.x1, r.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            with_alpha(c.surfaces.raised, 200),
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
    header_h: f64,
) {
    let c = &theme.tokens.colors;
    let bar = Rect::new(col.x0, col.y0, col.x1, col.y0 + header_h);
    scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
    // The header band closes with its own derived edge against the body.
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::surface::edge_on(c.surfaces.raised),
        None,
        &Line::new(
            Point::new(col.x0, col.y0 + header_h),
            Point::new(col.x1, col.y0 + header_h),
        ),
    );
    if !label.is_empty() {
        let (_, lh) = tr.measure(label, HEADER_FONT_SIZE, None);
        tr.draw_text(
            scene,
            label,
            HEADER_FONT_SIZE,
            c.text_primary,
            transform
                * Affine::translate((
                    col.x0 + PAD_X + 4.0,
                    col.y0 + (header_h - f64::from(lh)) / 2.0,
                )),
            Some((col.width() - 2.0 * PAD_X) as f32),
        );
    }
}

#[expect(
    clippy::many_single_char_names,
    reason = "c/m/y/r/h follow this file's graphics convention (colour tokens, metrics, position, rect, hunk)"
)]
fn draw_gutter_number(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y: f64,
    no: Option<usize>,
    m: Metrics,
) {
    let c = &theme.tokens.colors;
    // Gutter background.
    let g = Rect::new(x0, y, x0 + m.gutter_w, y + m.row_h);
    scene.fill(
        Fill::NonZero,
        transform,
        with_alpha(c.surfaces.raised, 120),
        None,
        &g,
    );
    if let Some(n) = no {
        let s = n.to_string();
        // Right-align on the measured width (a `len * 7` estimate drifted
        // with digit widths and text scale) and centre on the row.
        let (nw, nh) = tr.measure(&s, GUTTER_FONT_SIZE, None);
        tr.draw_text(
            scene,
            &s,
            GUTTER_FONT_SIZE,
            c.text_dim,
            transform
                * Affine::translate((
                    x0 + m.gutter_w - f64::from(nw) - 6.0,
                    y + (m.row_h - f64::from(nh)) / 2.0,
                )),
            None,
        );
    }
}

/// The spans of 1-based source line `number` on one side.
fn line_spans(spans: &[Vec<Span>], number: Option<usize>) -> &[Span] {
    number
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| spans.get(index))
        .map_or(&[], Vec::as_slice)
}
