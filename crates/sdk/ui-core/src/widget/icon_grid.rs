//! [`IconGrid`] — a responsive, selectable grid of icon + label cells.
//!
//! A single measured leaf widget that **reflows** to the available width
//! (column count derived from cell size at layout time), **selects** via
//! the shared [`SelectionModel`], and
//! **activates** items on double-click or Enter via `on_activate`.
//!
//! Designed to be rebuilt each frame by a stateless host: seed the
//! selection with [`set_selection`](IconGrid::set_selection) and react to
//! `on_item_click(index, ctrl, shift)`.

use crate::num::{f32_of_f64, usize_of_f64};
use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::presence::PresenceCtx;
use crate::selection::SelectionModel;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{text_band, type_line_height};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconGridItem {
    pub icon: Icon,
    pub label: String,
}

impl IconGridItem {
    pub fn new(icon: Icon, label: impl Into<String>) -> Self {
        Self {
            icon,
            label: label.into(),
        }
    }
}

/// `(index, ctrl, shift)`.
type ItemClick = Box<dyn FnMut(usize, bool, bool)>;

pub struct IconGrid {
    items: Vec<IconGridItem>,
    /// Per-cell box (icon + label) in logical px.
    cell_w: f64,
    cell_h: f64,
    /// Both axes.
    gap: f64,
    icon_px: f64,
    /// Set by [`Self::with_cell_size`]: an authored cell stays exact; the
    /// default cell follows the theme's label type in `build_layout`.
    cell_authored: bool,

    rect: Rect,
    /// Resolved at the last layout, cached for hit-testing + keyboard
    /// navigation. Always `>= 1`.
    columns: usize,

    selection: SelectionModel,
    is_focused: bool,
    hover: Option<usize>,
    last_cursor: Point,
    last_click: Option<(usize, crate::time::Instant)>,

    on_item_click: Option<ItemClick>,
    on_activate: Option<Box<dyn FnMut(usize)>>,
}

/// Default cell at the default text scale, tuned for a file-manager icon
/// view; `build_layout` grows it with the label type.
const DEFAULT_CELL_W: f64 = 96.0;
const DEFAULT_CELL_H: f64 = 88.0;
/// The default cell is this many ems of `sm` label wide (8 × 12 = 96).
const DEFAULT_CELL_LABEL_EMS: f64 = 8.0;
/// Gap between cells before a theme is seen: the sibling step, unscaled.
/// `build_layout` reads the live `spacing.s3` on every pass.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const DEFAULT_CELL_GAP: f64 = crate::tokens::Spacing::BASE.s3 as f64;

impl IconGrid {
    /// Default cell metrics (96×88 px cells, `s3` gap, 40 px glyphs) are
    /// tuned for a file-manager icon view; the cell follows the theme's
    /// label type unless [`Self::with_cell_size`] authors it.
    pub fn new(items: Vec<IconGridItem>) -> Self {
        Self {
            items,
            cell_w: DEFAULT_CELL_W,
            cell_h: DEFAULT_CELL_H,
            gap: DEFAULT_CELL_GAP,
            icon_px: 40.0,
            cell_authored: false,
            rect: Rect::ZERO,
            columns: 1,
            selection: SelectionModel::new(),
            is_focused: false,
            hover: None,
            last_cursor: Point::ZERO,
            last_click: None,
            on_item_click: None,
            on_activate: None,
        }
    }

    #[must_use]
    pub fn with_cell_size(mut self, w: f64, h: f64) -> Self {
        self.cell_w = w.max(8.0);
        self.cell_h = h.max(8.0);
        self.cell_authored = true;
        self
    }

    /// Host owns the selection across frames.
    pub fn set_selection(&mut self, sel: SelectionModel) {
        self.selection = sel;
    }

    #[must_use]
    pub fn with_selection(mut self, sel: SelectionModel) -> Self {
        self.selection = sel;
        self
    }

    pub fn selection(&self) -> &SelectionModel {
        &self.selection
    }

    pub fn column_count(&self) -> usize {
        self.columns
    }

    /// `(index, ctrl, shift)`.
    #[must_use]
    pub fn on_item_click<F: FnMut(usize, bool, bool) + 'static>(mut self, f: F) -> Self {
        self.on_item_click = Some(Box::new(f));
        self
    }

    /// Double-click or Enter.
    #[must_use]
    pub fn on_activate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    /// Always at least one, so a too-narrow grid still shows a single
    /// column.
    pub fn columns_for_width(&self, width: f64) -> usize {
        let stride = self.cell_w + self.gap;
        if stride <= 0.0 {
            return 1;
        }
        // width + gap, because the last cell needs no trailing gap.
        usize_of_f64(((width + self.gap) / stride).floor()).max(1)
    }

    fn content_height(&self, columns: usize) -> f64 {
        let rows = if self.items.is_empty() {
            0
        } else {
            self.items.len().div_ceil(columns.max(1))
        };
        if rows == 0 {
            0.0
        } else {
            count_to_f64(rows) * self.cell_h + count_to_f64(rows.saturating_sub(1)) * self.gap
        }
    }

    fn cell_origin(&self, i: usize) -> Point {
        let columns = self.columns.max(1);
        let col = i.checked_rem(columns).unwrap_or(0);
        let row = i.checked_div(columns).unwrap_or(0);
        Point::new(
            self.rect.x0 + count_to_f64(col) * (self.cell_w + self.gap),
            self.rect.y0 + count_to_f64(row) * (self.cell_h + self.gap),
        )
    }

    fn cell_rect(&self, i: usize) -> Rect {
        let o = self.cell_origin(i);
        Rect::new(o.x, o.y, o.x + self.cell_w, o.y + self.cell_h)
    }

    pub fn item_at(&self, pt: Point) -> Option<usize> {
        if self.columns == 0 {
            return None;
        }
        let stride_x = self.cell_w + self.gap;
        let stride_y = self.cell_h + self.gap;
        let rel_x = pt.x - self.rect.x0;
        let rel_y = pt.y - self.rect.y0;
        if rel_x < 0.0 || rel_y < 0.0 {
            return None;
        }
        let col = usize_of_f64(rel_x / stride_x);
        let row = usize_of_f64(rel_y / stride_y);
        if col >= self.columns {
            return None;
        }
        // Reject points that land in the inter-cell gap.
        if rel_x - count_to_f64(col) * stride_x > self.cell_w {
            return None;
        }
        if rel_y - count_to_f64(row) * stride_y > self.cell_h {
            return None;
        }
        let idx = row.saturating_mul(self.columns).saturating_add(col);
        if idx < self.items.len() {
            Some(idx)
        } else {
            None
        }
    }
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` impl is
/// pure `f64` matrix multiply-add with no overflow trap, so it cannot
/// panic regardless of operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Widens a column/row count to `f64` for layout math. Goes through `u32`
/// (exactly representable in `f64`) with a saturating fallback rather than
/// an `as` cast, so it's lossless for any realistic grid size.
fn count_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Widget for IconGrid {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let tokens = &theme.tokens;
        // The gap between cells is the sibling step (12 px at 1×).
        self.gap = f64::from(tokens.spacing.s3);
        if !self.cell_authored {
            // The default cell follows the label type: eight ems of `sm`
            // label across (96 px at 1×) and the glyph, its label gap and one
            // label line plus a section inset above and below (88 px at 1×).
            // An authored `with_cell_size` is left exactly as written.
            let label_size = f64::from(tokens.typography.size(TextRole::Caption));
            let label_line = type_line_height(&tokens.typography, TextRole::Caption);
            self.cell_w = text_band(DEFAULT_CELL_W, label_size * DEFAULT_CELL_LABEL_EMS, 0.0);
            self.cell_h = text_band(
                DEFAULT_CELL_H,
                self.icon_px + f64::from(tokens.spacing.s2) + label_line,
                2.0 * f64::from(tokens.spacing.s3),
            );
        }
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // known wins, else the definite available width, else enough
        // for one cell.
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(w) => f64::from(w),
                _ => self.cell_w,
            },
            f64::from,
        );
        let columns = self.columns_for_width(width);
        self.columns = columns;
        let height = self.content_height(columns);
        taffy::Size {
            width: f32_of_f64(width),
            height: f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        // Actual allocated width — measure may have run against a
        // different available width.
        self.columns = self.columns_for_width(self.rect.width());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let label_style = tokens.typography.style(TextRole::Caption);
        let label_gap = f64::from(tokens.spacing.s2);
        let label_inset = f64::from(tokens.spacing.s2);
        for (i, item) in self.items.iter().enumerate() {
            let cell = self.cell_rect(i);
            if presence.should_cull(cell) {
                continue;
            }
            let selected = self.selection.is_selected(i);
            let hovered = self.hover == Some(i) && !selected;
            if selected || hovered {
                let bg = if selected {
                    tokens.colors.lift_on(tokens.colors.surfaces.content, 1.0)
                } else {
                    tokens.colors.intent.selection_hover
                };
                let shape = RoundedRect::from_rect(cell, f64::from(tokens.radii.control));
                scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
            }

            let label = text_renderer.elide_to_width_style(
                &item.label,
                crate::num::f32_of_f64(self.cell_w - label_inset),
                &label_style,
            );
            let (tw, th) = text_renderer.measure_style(&label, None, &label_style);
            // The glyph + label block is centred in the cell so the margins
            // above and below match; the label is centred under the glyph.
            let block_h = self.icon_px + label_gap + f64::from(th);
            let icon_x = cell.x0 + (self.cell_w - self.icon_px) / 2.0;
            let icon_y = cell.y0 + ((self.cell_h - block_h) / 2.0).max(0.0);
            let icon_rect = Rect::new(icon_x, icon_y, icon_x + self.icon_px, icon_y + self.icon_px);
            let glyph_color = tokens.colors.text_primary;
            draw_icon(
                scene,
                transform,
                icon_rect,
                item.icon,
                glyph_color,
                LUCIDE_DEFAULT_STROKE_PX,
            );

            let tx = cell.x0 + (self.cell_w - f64::from(tw)) / 2.0;
            let ty = icon_y + self.icon_px + label_gap;
            let label_color = tokens.colors.text_primary;
            text_renderer.draw_style(
                scene,
                &label,
                label_color,
                translated(transform, tx, ty),
                None,
                &label_style,
            );
        }

        // So arrow-key navigation is visible without relying on the
        // selection fill alone.
        if self.is_focused
            && let Some(i) = self.selection.primary()
        {
            let cell = self.cell_rect(i);
            if !presence.should_cull(cell) {
                let ring = RoundedRect::from_rect(cell, f64::from(tokens.radii.control));
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &ring,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = pt;
                let new_hover = self.item_at(pt);
                if new_hover != self.hover {
                    self.hover = new_hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self.item_at(self.last_cursor) {
                    let ctrl =
                        ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
                    let shift = ctx.modifiers.state().shift_key();
                    self.selection.apply_click(i, ctrl, shift);
                    *ctx.focus_target = Some(ctx.node_id);
                    if let Some(cb) = &mut self.on_item_click {
                        cb(i, ctrl, shift);
                    }
                    let now = crate::time::now();
                    let is_double = matches!(
                        self.last_click,
                        Some((pi, t)) if pi == i && now.duration_since(t) < crate::gesture::multi_click_interval()
                    );
                    if is_double {
                        self.last_click = None;
                        if let Some(cb) = &mut self.on_activate {
                            cb(i);
                        }
                    } else {
                        self.last_click = Some((i, now));
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.is_focused && ke.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                let shift = ctx.modifiers.state().shift_key();
                let len = self.items.len();
                if len == 0 {
                    return;
                }
                let cols = self.columns.max(1);
                let cur = self.selection.primary();
                let target = match &ke.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        Some(cur.map_or(0, |c| c.saturating_add(1).min(len.saturating_sub(1))))
                    }
                    Key::Named(NamedKey::ArrowLeft) => Some(cur.map_or(0, |c| c.saturating_sub(1))),
                    Key::Named(NamedKey::ArrowDown) => {
                        Some(cur.map_or(0, |c| c.saturating_add(cols).min(len.saturating_sub(1))))
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        Some(cur.map_or(0, |c| c.saturating_sub(cols)))
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let (Some(c), Some(cb)) = (cur, self.on_activate.as_mut()) {
                            cb(c);
                        }
                        *ctx.handled = true;
                        None
                    }
                    Key::Character(s)
                        if (s == "a" || s == "A") && ctx.modifiers.state().control_key() =>
                    {
                        self.selection.select_all(len);
                        *ctx.handled = true;
                        None
                    }
                    _ => None,
                };
                if let Some(t) = target {
                    self.selection.move_to(t, shift);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "IconGrid"
    }

    fn test_id(&self) -> Option<&str> {
        Some("icon_grid")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, f: bool) {
        self.is_focused = f;
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Items are drawn, not child widgets, so the container carries
        // the semantics.
        let primary = self.selection.primary();
        let value = primary
            .and_then(|i| self.items.get(i))
            .map(|item| item.label.clone());
        let mut node = AccessNode::new(Role::List)
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                ..Default::default()
            });
        // So the reader can say "2 of 9".
        if let Some(i) = primary
            && i < self.items.len()
        {
            node = node.with_position(i.saturating_add(1), self.items.len());
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyMaxContent;

    fn items(n: usize) -> Vec<IconGridItem> {
        (0..n)
            .map(|i| IconGridItem::new(Icon::File, format!("f{i}")))
            .collect()
    }

    #[test]
    fn icon_grid_columns_rows_and_hit_mapping() {
        fn columns_scale_with_width() {
            let g = IconGrid::new(items(10)); // 96px cell + 12px gap = 108 stride
            // (width + gap) / stride, floored.
            assert_eq!(g.columns_for_width(108.0), 1);
            assert_eq!(g.columns_for_width(216.0), 2);
            assert_eq!(g.columns_for_width(700.0), 6);
            // Never zero, even for an absurdly narrow box.
            assert_eq!(g.columns_for_width(10.0), 1);
        }

        fn content_height_rounds_up_rows() {
            let g = IconGrid::new(items(7));
            // 3 columns → 3 rows (3,3,1). height = 3*88 + 2*12 = 288.
            assert!((g.content_height(3) - 288.0).abs() < 0.01);
            // Empty grid is zero-height.
            assert_eq!(IconGrid::new(items(0)).content_height(4), 0.0);
        }

        fn item_at_maps_points_and_rejects_gaps() {
            let mut g = IconGrid::new(items(6));
            g.rect = Rect::new(0.0, 0.0, 324.0, 200.0); // 3 columns (324 = 3*108)
            g.columns = 3;
            // Cell 0 centre (~48,44).
            assert_eq!(g.item_at(Point::new(48.0, 44.0)), Some(0));
            // Cell 4 = row 1, col 1 → origin x=108,y=100; centre ~156,144.
            assert_eq!(g.item_at(Point::new(156.0, 144.0)), Some(4));
            // A point in the horizontal gap between col 0 and col 1 (x≈100).
            assert_eq!(g.item_at(Point::new(100.0, 44.0)), None);
            // Row 2 is empty (6 items fill rows 0–1), so it maps to nothing.
            assert_eq!(g.item_at(Point::new(48.0, 244.0)), None);
        }

        columns_scale_with_width();
        content_height_rounds_up_rows();
        item_at_maps_points_and_rejects_gaps();
    }

    #[test]
    fn icon_grid_selection_semantics_and_focus() {
        fn click_selects_and_reports() {
            use std::cell::RefCell;
            use std::rc::Rc;
            let log = Rc::new(RefCell::new(Vec::<(usize, bool, bool)>::new()));
            let mut g = {
                let log = log.clone();
                IconGrid::new(items(6))
                    .on_item_click(move |i, c, s| log.borrow_mut().push((i, c, s)))
            };
            g.rect = Rect::new(0.0, 0.0, 324.0, 200.0);
            g.columns = 3;

            let device = winit::event::DeviceId::dummy();
            let feed = |g: &mut IconGrid, ev: WindowEvent| {
                crate::testing::dispatch(g, &ev);
            };
            feed(
                &mut g,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (156.0, 144.0).into(),
                },
            );
            feed(
                &mut g,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert!(g.selection().is_selected(4));
            assert_eq!(*log.borrow(), vec![(4, false, false)]);
        }

        fn accessibility_reports_list_role_and_selected_label() {
            use crate::a11y::Role;
            let mut g = IconGrid::new(items(6));
            let node = Widget::accessibility(&g).expect("grid is not presentational");
            assert_eq!(node.role, Role::List);
            assert!(node.state.focusable);
            assert_eq!(node.value, None, "no selection → no value");

            g.selection.select_only(4);
            let node = Widget::accessibility(&g).unwrap();
            assert_eq!(node.value.as_deref(), Some("f4"));
        }

        /// Asserts on the production `draw` path via the scene encoding.
        fn focused_grid_paints_a_focus_ring() {
            let theme = Theme::dark();

            let mut unfocused = IconGrid::new(items(6));
            unfocused.selection.select_only(2);
            unfocused.set_focus(false);
            let without = color_fill_count(&mut unfocused, &theme);

            let mut focused = IconGrid::new(items(6));
            focused.selection.select_only(2);
            focused.set_focus(true);
            let with = color_fill_count(&mut focused, &theme);

            assert_eq!(
                with,
                without + 1,
                "a focused grid must add exactly one focus-ring stroke ({with} vs {without})",
            );
        }

        click_selects_and_reports();
        accessibility_reports_list_role_and_selected_label();
        focused_grid_paints_a_focus_ring();
    }

    /// Lay the grid out, render against `theme`, and count COLOR draw-tags.
    fn color_fill_count(g: &mut IconGrid, theme: &Theme) -> usize {
        use vello::Scene;
        use vello_encoding::DrawTag;
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);

        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut tr,
            theme,
            &presence,
        );
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }
}
