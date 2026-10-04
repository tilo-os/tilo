//! Side-by-side two-way diff view.
//!
//! Renders [`DiffModel`] as two aligned columns with line-number gutters,
//! per-row add/remove/modify washes, syntax highlighting, a change minimap on
//! the right edge, and prev/next-change navigation. Draws everything directly
//! into the Vello scene (no nested taffy children) and owns its own vertical
//! scroll offset. The content ground, the headers' rule and every size come
//! from the theme; nothing here fills the pane.

use std::collections::HashMap;

use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::code_editor::Span;
use ui::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use ui::widget::toggle_group::SegmentedStyle;
use ui::widget::{ControlSize, EmptyState, EmptyStateContent, SegmentedFace, ToggleGroup, Widget};

use crate::highlight;
use crate::i18n::t;
use crate::model::{AlignedRow, DiffModel, RowKind};
use crate::pane::{self, CodeMetrics};
use crate::scroll::{HScroll, wheel_pixels};

/// Row metrics measured from the renderer the panel paints with. Refreshed at
/// the top of every draw and read by scrolling, block rects (accessibility
/// bounds), the compact source switcher and the paint, so all of them follow
/// the same line height.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    code: CodeMetrics,
    /// Line-number gutter width.
    gutter_w: f64,
    /// The change-marker slot between the gutter and the code: the marker's
    /// glyph with a gap on each side.
    marker_w: f64,
    /// Width of the overview strip, the width of a scroll bar's gutter.
    strip_w: f64,
    /// Below this width two code columns cease to be readable and the pane
    /// shows one source at a time.
    breakpoint: f64,
}

impl Metrics {
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let code = CodeMetrics::measure(tr, theme);
        Self {
            gutter_w: code.gutter_w(tr, theme, 4),
            ..Self::with_code(code, theme)
        }
    }

    /// What is known before the first frame measures anything.
    fn unmeasured(theme: &Theme) -> Self {
        let code = CodeMetrics::unmeasured(theme);
        Self {
            gutter_w: 2.0 * code.pad,
            ..Self::with_code(code, theme)
        }
    }

    fn with_code(code: CodeMetrics, theme: &Theme) -> Self {
        Self {
            gutter_w: 0.0,
            marker_w: code.cell_w + 2.0 * code.gap,
            strip_w: f64::from(theme.tokens.metrics.scrollbar_gutter),
            breakpoint: f64::from(theme.tokens.metrics.compact_breakpoint),
            code,
        }
    }

    /// Where code starts in a column that begins at `x0`.
    fn code_x(&self, x0: f64) -> f64 {
        x0 + self.gutter_w + self.marker_w
    }

    /// Where a change marker starts in a column that begins at `x0`.
    fn marker_x(&self, x0: f64) -> f64 {
        x0 + self.gutter_w + self.code.gap
    }
}

/// One of the two sides of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Side {
    /// The line this side holds on `row`, if it holds one.
    fn text(self, row: &AlignedRow) -> Option<&str> {
        match self {
            Self::Left => row.left.as_deref(),
            Self::Right => row.right.as_deref(),
        }
    }

    /// Its 1-based line number on `row`.
    fn number(self, row: &AlignedRow) -> Option<usize> {
        match self {
            Self::Left => row.left_no,
            Self::Right => row.right_no,
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
    compact_show_right: bool,
    compact_source_hits: [Rect; 2],
    cursor: Point,
    modifiers: ModifiersState,
    /// The overview strip's scroll bar: the thumb is the visible window.
    strip_bar: Scrollbar,
    /// Where the strip's track was drawn last.
    strip_track: Rect,
    /// Horizontal scroll shared by the code columns.
    across: HScroll,
    /// The widest source line in characters, which sets how far code scrolls.
    widest: usize,
    /// Last measured row metrics (see [`Metrics`]); an estimate until the
    /// first draw.
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
            compact_show_right: false,
            compact_source_hits: [Rect::ZERO; 2],
            cursor: Point::ZERO,
            modifiers: ModifiersState::empty(),
            strip_bar: Scrollbar::vertical(),
            strip_track: Rect::ZERO,
            across: HScroll::default(),
            widest: 0,
            metrics: Metrics::unmeasured(&Theme::dark()),
        }
    }

    /// Place the panel at `rect` directly (the parent app reserves the status
    /// bar below it and re-places the view when that band's measured height
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
            self.metrics.code.row_h,
            self.metrics.code.header_h,
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
        self.widest = left
            .lines()
            .chain(right.lines())
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        self.scroll_y = 0.0;
        self.across.reset();
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
        let row_h = self.metrics.code.row_h;
        let block_top = b.first_row as f64 * row_h;
        let viewport_h = self.body_height();
        // Centre the block in the viewport where possible.
        let target = block_top - viewport_h / 2.0 + row_h;
        self.scroll_y = target.max(0.0).min(self.max_scroll());
    }

    fn body_height(&self) -> f64 {
        (self.rect.height() - self.metrics.code.header_h).max(0.0)
    }

    fn content_height(&self) -> f64 {
        self.model.rows.len() as f64 * self.metrics.code.row_h
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
        self.rect.width() < self.metrics.breakpoint
    }

    /// The fill of a row on one side, `None` for an unchanged row. `present`
    /// says the side holds a line; an absent one is the filler that keeps the
    /// columns aligned. The change under review (`active`) takes a stronger
    /// wash of its own hue; where it has no hue it takes the selection lift.
    fn row_fill(
        kind: RowKind,
        present: bool,
        side: Side,
        active: bool,
        theme: &Theme,
    ) -> Option<Color> {
        let c = &theme.tokens.colors;
        let ground = if active {
            c.intent.selection
        } else {
            c.surfaces.content
        };
        let filler = c.lift_on(ground, 0.5);
        let washed = |tone| pane::washed(c.surfaces.content, tone, active);
        match (kind, side) {
            (RowKind::Equal, _) => active.then_some(ground),
            (RowKind::Insert, Side::Left) | (RowKind::Delete, Side::Right) => Some(filler),
            (RowKind::Insert, Side::Right) => Some(if present {
                washed(c.semantic.success)
            } else {
                filler
            }),
            (RowKind::Delete, Side::Left) => Some(if present {
                washed(c.semantic.danger)
            } else {
                filler
            }),
            (RowKind::Modify, _) => Some(washed(c.semantic.warning)),
        }
    }

    /// The marker a row shows beside its code on one side.
    fn row_marker(
        kind: RowKind,
        present: bool,
        side: Side,
        theme: &Theme,
    ) -> Option<(&'static str, Color)> {
        let s = &theme.tokens.colors.semantic;
        match (kind, side, present) {
            (RowKind::Insert, Side::Right, true) => Some(("+", s.success)),
            (RowKind::Delete, Side::Left, true) => Some(("-", s.danger)),
            (RowKind::Modify, _, true) => Some(("~", s.warning)),
            _ => None,
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
        let measured = Metrics::measure(tr, theme);
        if measured != self.metrics {
            self.metrics = measured;
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
        self.fit_across();
        let r = self.rect;
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        if self.model.rows.is_empty() {
            Self::draw_empty_state(&mut ctx, r);
            self.dirty = false;
            return;
        }
        if self.is_compact() {
            self.draw_compact(&mut ctx);
            self.dirty = false;
            return;
        }

        let m = self.metrics;
        let (left_col, right_col) = self.column_rects();

        // --- Column headers ---
        for (col, label) in [(left_col, &self.left_label), (right_col, &self.right_label)] {
            let band = Rect::new(col.x0, col.y0, col.x1, col.y0 + m.code.header_h);
            pane::paint_header(&mut ctx, band, label, None);
        }

        // Divider between columns.
        let edge = ui::surface::edge_on(theme.tokens.colors.surfaces.content);
        ctx.fill_rect(
            Rect::new(
                right_col.x0 - 0.5,
                r.y0 + m.code.header_h,
                right_col.x0 + 0.5,
                r.y1,
            ),
            edge,
        );

        // --- Body (clipped + scrolled) ---
        self.draw_column(&mut ctx, left_col, Side::Left);
        self.draw_column(&mut ctx, right_col, Side::Right);
        let tracks = [left_col, right_col].map(|col| HScroll::track_in(self.code_view(col), theme));
        self.across.draw(&mut ctx, &tracks);

        // --- Overview strip on the right edge ---
        self.draw_strip(&mut ctx);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
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
                let (dx, dy) =
                    wheel_pixels(*delta, self.metrics.code.row_h, self.modifiers.shift_key());
                // The host routes the wheel to the focused subtree, so the
                // pane scrolls whenever it sees one.
                self.scroll_by(dy);
                if self.across.scroll_by(dx) {
                    self.dirty = true;
                }
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) {
                    self.dirty |= moved;
                    *ctx.handled = true;
                } else if let Some(offset) =
                    self.strip_bar
                        .drag_to(self.strip_track, self.strip_metrics(), self.cursor)
                {
                    self.scroll_to(offset);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) {
                    self.dirty |= moved;
                    *ctx.handled = true;
                    return;
                }
                match self
                    .strip_bar
                    .press(self.strip_track, self.strip_metrics(), self.cursor)
                {
                    PressOutcome::Miss => {}
                    PressOutcome::Thumb => {
                        self.dirty = true;
                        *ctx.handled = true;
                        return;
                    }
                    PressOutcome::Page(offset) => {
                        self.scroll_to(offset);
                        *ctx.handled = true;
                        return;
                    }
                }
                if self.is_compact()
                    && let Some(index) = self
                        .compact_source_hits
                        .iter()
                        .position(|rect| rect.contains(self.cursor))
                {
                    self.select_compact_side(index == 1);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) | self.strip_bar.release() {
                    self.dirty = true;
                }
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
                .with_bounds(self.rect),
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
    /// The controls of the last draw as accessibility nodes: the source switch
    /// of the narrow layout and every change block. The root publishes them,
    /// so they are exposed once.
    pub(crate) fn access_controls(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut controls = Vec::new();
        if self.is_compact() {
            controls.extend(
                [(false, t("source-left")), (true, t("source-right"))]
                    .into_iter()
                    .map(|(show_right, label)| {
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
                    }),
            );
        }
        controls.extend(self.model.blocks.iter().enumerate().map(|(index, block)| {
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
        controls
    }

    /// Column rects for the two source panes, excluding the overview strip.
    fn column_rects(&self) -> (Rect, Rect) {
        let r = self.rect;
        let content_right = r.x1 - self.metrics.strip_w;
        let mid_x = r.x0.midpoint(content_right);
        (
            Rect::new(r.x0, r.y0, mid_x, r.y1),
            Rect::new(mid_x, r.y0, content_right, r.y1),
        )
    }

    /// Where the code of the column `col` shows: right of its gutter and
    /// markers, below its header.
    fn code_view(&self, col: Rect) -> Rect {
        Rect::new(
            self.metrics.code_x(col.x0),
            self.rect.y0 + self.metrics.code.header_h,
            col.x1,
            self.rect.y1,
        )
    }

    /// Tell the horizontal scroll how wide the code is and how much of it
    /// shows.
    fn fit_across(&mut self) {
        let m = self.metrics;
        let col_w = if self.is_compact() {
            self.rect.width() - m.strip_w
        } else {
            (self.rect.width() - m.strip_w) / 2.0
        };
        let viewport = (col_w - m.gutter_w - m.marker_w).max(0.0);
        let content = self.widest as f64 * m.code.cell_w + m.code.pad;
        self.across.set_extent(content, viewport);
    }

    fn block_rect(&self, block: &crate::model::ChangeBlock) -> Rect {
        let m = self.metrics.code;
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

    /// The narrow layout: one source at a time, chosen by a segmented switch
    /// in the header band.
    fn draw_compact(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let r = self.rect;
        let m = self.metrics;
        let content_right = r.x1 - m.strip_w;
        let inset = f64::from(theme.tokens.spacing.s3);
        let strip_h = SegmentedFace::height(ControlSize::Small, theme);
        let strip_y = r.y0 + (m.code.header_h - strip_h) / 2.0;
        let (x0, x1) = (r.x0 + inset, content_right - inset);
        let mid = x0.midpoint(x1);
        self.compact_source_hits = [
            Rect::new(x0, strip_y, mid, strip_y + strip_h),
            Rect::new(mid, strip_y, x1, strip_y + strip_h),
        ];
        let labels = [t("source-left"), t("source-right")];
        let labels = [labels[0].as_str(), labels[1].as_str()];
        ToggleGroup::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &SegmentedFace::new(
                &self.compact_source_hits,
                &labels,
                usize::from(self.compact_show_right),
            )
            .style(SegmentedStyle::Segmented)
            .control_size(ControlSize::Small)
            .hovered(
                self.compact_source_hits
                    .iter()
                    .position(|rect| rect.contains(self.cursor)),
            ),
        );
        let body_top = r.y0 + m.code.header_h;
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            r.x0,
            content_right,
            body_top,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let col = Rect::new(r.x0, r.y0, content_right, r.y1);
        let side = if self.compact_show_right {
            Side::Right
        } else {
            Side::Left
        };
        self.draw_column(ctx, col, side);
        let track = HScroll::track_in(self.code_view(col), theme);
        self.across.draw(ctx, &[track]);
        self.draw_strip(ctx);
    }

    /// One side of every visible row in the column `col`: the wash, the line
    /// number and change marker, then the code, scrolled across.
    ///
    /// `ri` is bounds-checked via `get` rather than indexed directly: the last
    /// row is derived from `self.model.rows.len()` so it is always in range
    /// today, but a future change to that arithmetic must degrade (skip the
    /// row) rather than panic on a third-party diff's row count.
    fn draw_column(&self, ctx: &mut PaintCtx<'_>, col: Rect, side: Side) {
        let theme = ctx.theme;
        let m = self.metrics;
        let body_top = self.rect.y0 + m.code.header_h;
        let first_row = (self.scroll_y / m.code.row_h).floor() as usize;
        let visible_rows = (self.body_height() / m.code.row_h).ceil() as usize + 2;
        let last_row = (first_row + visible_rows).min(self.model.rows.len());
        let spans = match side {
            Side::Left => &self.left_spans,
            Side::Right => &self.right_spans,
        };
        let body = Rect::new(col.x0, body_top, col.x1, self.rect.y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let row_y = |ri: usize| body_top + ri as f64 * m.code.row_h - self.scroll_y;
        for ri in first_row..last_row {
            let Some(row) = self.model.rows.get(ri) else {
                continue;
            };
            let y = row_y(ri);
            let present = side.text(row).is_some();
            let active = row.block.is_some() && row.block == self.active_block;
            if let Some(fill) = Self::row_fill(row.kind, present, side, active, theme) {
                ctx.fill_rect(Rect::new(col.x0, y, col.x1, y + m.code.row_h), fill);
            }
            Self::draw_gutter(
                ctx,
                &m,
                col.x0,
                y,
                side.number(row),
                Self::row_marker(row.kind, present, side, theme),
            );
        }
        // Code scrolls across under the gutter's edge, so it clips to its own
        // view.
        let view = self.code_view(col);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &view,
        );
        for ri in first_row..last_row {
            let Some(row) = self.model.rows.get(ri) else {
                continue;
            };
            let Some(text) = side.text(row) else {
                continue;
            };
            highlight::draw_line(
                ctx,
                pane::line_origin(view.x0 - self.across.offset(), row_y(ri), &m.code),
                text,
                line_spans(spans, side.number(row)),
            );
        }
        ctx.scene.pop_layer();
        ctx.scene.pop_layer();
    }

    /// A row's line number and change marker, in the column that starts at
    /// `x0`.
    fn draw_gutter(
        ctx: &mut PaintCtx<'_>,
        m: &Metrics,
        x0: f64,
        y: f64,
        number: Option<usize>,
        marker: Option<(&'static str, Color)>,
    ) {
        let colors = &ctx.theme.tokens.colors;
        if let Some(n) = number {
            let style = pane::number_style(ctx.theme);
            let digits = n.to_string();
            let (w, h) = ctx.text.measure_style(&digits, None, &style);
            ctx.text(
                &digits,
                &style,
                colors.text_dim,
                Point::new(
                    x0 + m.gutter_w - m.code.pad - f64::from(w),
                    y + (m.code.row_h - f64::from(h)) / 2.0,
                ),
                None,
            );
        }
        if let Some((glyph, tone)) = marker {
            let style = ctx.theme.text(TextRole::Code);
            ctx.text(
                glyph,
                &style,
                tone,
                pane::line_origin(m.marker_x(x0), y, &m.code),
                None,
            );
        }
    }

    /// The "open two files" state: shown instead of the columns until a
    /// comparison is loaded.
    fn draw_empty_state(ctx: &mut PaintCtx<'_>, rect: Rect) {
        let title = t("empty-title");
        let content = EmptyStateContent {
            icon: Some(Icon::FileText),
            ..EmptyStateContent::new(&title)
        };
        EmptyState::paint(ctx, rect, &content, Point::new(-1.0, -1.0));
    }

    fn strip_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content_height(), self.body_height(), self.scroll_y)
    }

    fn scroll_to(&mut self, offset: f64) {
        self.scroll_by(offset - self.scroll_y);
    }

    /// The strip along the trailing edge: the scroll bar of the rows, over a
    /// map of where the changes are. The thumb is the window the columns show.
    fn draw_strip(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let m = self.metrics;
        let r = self.rect;
        let margin = f64::from(theme.tokens.spacing.s1) / 2.0;
        let map_top = r.y0 + m.code.header_h;
        let track = Rect::new(
            r.x1 - m.strip_w + margin,
            map_top,
            r.x1 - margin,
            r.y1 - margin,
        );
        self.strip_track = track;
        if self.content_height() > self.body_height() {
            self.strip_bar.draw(
                ctx.scene,
                ctx.transform,
                track,
                self.strip_metrics(),
                theme,
                false,
            );
        }

        // Where each change block sits in the whole comparison.
        let total = self.model.rows.len().max(1) as f64;
        let tick = f64::from(theme.tokens.spacing.s1) / 2.0;
        for b in &self.model.blocks {
            let y0 = track.y0 + (b.first_row as f64 / total) * track.height();
            let y1 = track.y0 + ((b.last_row + 1) as f64 / total) * track.height();
            let color = match b.kind {
                crate::model::BlockKind::Added => c.semantic.success,
                crate::model::BlockKind::Removed => c.semantic.danger,
                crate::model::BlockKind::Modified => c.semantic.warning,
            };
            let x = track.center().x;
            let marker = Rect::new(x - tick, y0, x + tick, y1.max(y0 + 2.0 * tick));
            ctx.fill_rect(marker, color);
        }
    }
}

/// The spans of 1-based source line `number` on one side.
fn line_spans(spans: &[Vec<Span>], number: Option<usize>) -> &[Span] {
    number
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| spans.get(index))
        .map_or(&[], Vec::as_slice)
}
