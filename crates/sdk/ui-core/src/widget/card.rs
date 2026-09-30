//! `Card` + `CardGrid` — the canonical grouped-content primitives.
//!
//! Promotes two shapes every app was hand-rolling, badly and inconsistently:
//! a grouped "card" surface (per-app radii, often literal numbers not theme
//! tokens, so no two cards matched), and a responsive grid of equal cards
//! reflowing by width (every app re-copied the column formula
//! `cols = floor((w + gap) / (card + gap))` with different magic numbers, so
//! cards didn't line up). [`card_columns`]/[`card_column_width`] expose the
//! pure reflow math so even immediate-mode callers share one correct formula.
//!
//! Every colour, radius, and gap is read from the [`Theme`].

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::style_helpers::{flex, fr, length, minmax, percent, repeat};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Container, Element, Row, Section, Spacer, Text, Widget};

/// Lossless `usize -> f64` widening for grid geometry. Column/row counts
/// are bounded far below `u32::MAX`, so routing through `u32` (which `f64`
/// represents exactly) avoids the precision loss a direct cast risks.
fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Default minimum card width (logical px) a [`CardGrid`] column will hold
/// before it drops to fewer columns. Sized for a typical content card.
pub const DEFAULT_CARD_MIN_WIDTH: f64 = 260.0;
/// Default fixed row height (logical px) of a [`CardGrid`] cell.
pub const DEFAULT_CARD_HEIGHT: f64 = 160.0;
const CARD_TITLE_GAP: f32 = 12.0;

// ---------------------------------------------------------------------------
// Pure reflow geometry — shared by CardGrid and immediate-mode callers.
// ---------------------------------------------------------------------------

/// Always returns **at least 1** so a too-narrow area still shows a single
/// column. `avail_w + gap` because the last column needs no trailing gap.
pub fn card_columns(avail_w: f64, min_card_w: f64, gap: f64) -> usize {
    let stride = min_card_w + gap;
    if stride <= 0.0 || avail_w <= 0.0 {
        return 1;
    }
    let n = ((avail_w + gap) / stride).floor();
    // `n` is finite (both operands are finite by construction) and the
    // early return above guarantees `avail_w + gap > 0`, so `n >= 0`; clamp
    // into a range `usize` always holds before the cast, so nothing here
    // can truncate or misread a sign.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "n is clamped into `[1.0, u32::MAX]` immediately before the cast, so the \
                  conversion is provably exact"
    )]
    let cols = n.clamp(1.0, f64::from(u32::MAX)) as usize;
    cols
}

/// Never negative. Combined with [`card_columns`] this snaps every card to
/// the same aligned column so a grid of cards lines up exactly.
pub fn card_column_width(avail_w: f64, cols: usize, gap: f64) -> f64 {
    let cols = cols.max(1);
    let total_gap = usize_to_f64(cols.saturating_sub(1)) * gap;
    ((avail_w - total_gap) / usize_to_f64(cols)).max(0.0)
}

// ---------------------------------------------------------------------------
// Card — a themed grouped surface.
// ---------------------------------------------------------------------------

/// A rounded, themed surface holding a body, optionally under a title row.
/// It stretches to fill its column's width, so a stack of cards shares
/// left/right edges and lines up.
pub struct Card {
    inner: Element,
}

impl Card {
    /// Compose a measured card with independently retained header/body/footer regions.
    pub fn composed(body: Element) -> super::ContentCard {
        super::ContentCard::new(body)
    }

    pub fn new(body: Element) -> Self {
        Self {
            inner: surface(None, None, body),
        }
    }

    pub fn titled(title: impl Into<String>, body: Element) -> Self {
        Self {
            inner: surface(Some(title.into()), None, body),
        }
    }

    /// `action` (e.g. an "Add" button or a count badge) sits on the same
    /// line as the title.
    pub fn titled_with_action(title: impl Into<String>, action: Element, body: Element) -> Self {
        Self {
            inner: surface(Some(title.into()), Some(action), body),
        }
    }
}

/// The title sits *inside* the surface, above the body, so the group reads
/// as one cohesive card. It is a `Title`-role heading, so it sits on the theme
/// type scale and follows the accessibility text scale like the body; the
/// title/body gap comes from the shared Section composition.
fn surface(title: Option<String>, action: Option<Element>, body: Element) -> Element {
    let inner: Element = if let Some(title) = title {
        let mut title_row = Row::new().push(Box::new(
            Text::new(&title).with_role(crate::tokens::TextRole::Title),
        ));
        if let Some(action) = action {
            title_row = title_row.push(Box::new(Spacer::new())).push(action);
        }
        Box::new(Section::with_gap(
            vec![Box::new(title_row), body],
            CARD_TITLE_GAP,
        ))
    } else {
        body
    };
    Box::new(Container::new().with_background().with_child(inner))
}

impl Widget for Card {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.inner.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.inner.build_layout(taffy, theme, node_map);
        // In a definite row the surface is capped at that row's height. In
        // intrinsic measurement this percentage is unresolved, allowing the
        // natural minimum to contribute without redefining fixed heights.
        if let Ok(style) = taffy.style(child) {
            let mut style = style.clone();
            style.max_size.height = percent(1.0_f32);
            let _ = taffy.set_style(child, style);
        }
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Grid,
                // Preserve the surface Container's non-shrinking flex contract.
                flex_shrink: 0.0,
                // Honor an explicit outer row height even when body content is
                // taller. Overflow is the caller's fixed-size choice, not a larger
                // card surface or a changed row height.
                grid_template_rows: vec![minmax(taffy::style_helpers::min_content(), fr(1.0_f32))],
                grid_template_columns: vec![flex(1.0_f32)],
                size: Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    // Intrinsic Grid rows need this item's automatic block minimum;
                    // `flex_shrink` does not protect an item in Grid sizing.
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.inner.measure(known, available, text_renderer)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let origin = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        if let Some(child) = taffy
            .children(node)
            .ok()
            .and_then(|nodes| nodes.first().copied())
        {
            self.inner.apply_layout(taffy, child, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.inner.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let Some(child_node) = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|nodes| nodes.first().copied())
        else {
            return;
        };
        self.inner
            .handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.inner.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.inner.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner.as_mut() as &mut (dyn Widget + '_)]
    }

    fn mark_drawn(&mut self) {
        self.inner.mark_drawn();
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn name(&self) -> &'static str {
        "Card"
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }
}

impl crate::undo::UndoPoints for Card {
    fn needs_commit(&self) -> bool {
        crate::undo::needs_commit(self.inner.as_ref())
    }
    fn on_commit(&mut self) {
        crate::undo::on_commit(self.inner.as_mut());
    }
}

// ---------------------------------------------------------------------------
// CardGrid — a responsive grid of equal, aligned cards.
// ---------------------------------------------------------------------------

/// Reflows its card children into **equal, aligned columns** by width. Cards
/// share a fixed row height by default. [`content_sized`](Self::content_sized)
/// instead measures each visual row's largest content minimum. The column
/// count is derived from the allocated width via [`card_columns`].
/// Children participate in the host layout tree, so measurement, focus and
/// input have their final geometry before paint. Native Grid handles ordinary
/// fixed widths; manual hosts should use [`crate::widget::compute_widget_layout`]
/// for automatic content sizing and below-minimum single-column reflow.
pub struct CardGrid {
    cards: Vec<Element>,
    min_card_w: f64,
    card_h: Option<f64>,
    gap_override: Option<f64>,

    // ---- resolved geometry (from the last layout pass) ----
    rect: Rect,
    gap: f64,

    // Host identities, retained until a structural rebuild.
    child_nodes: Vec<NodeId>,
}

impl CardGrid {
    /// Build a grid over `cards`. Defaults: [`DEFAULT_CARD_MIN_WIDTH`] minimum
    /// column width, [`DEFAULT_CARD_HEIGHT`] row height, theme gap.
    pub fn new(cards: Vec<Element>) -> Self {
        Self {
            cards,
            min_card_w: DEFAULT_CARD_MIN_WIDTH,
            card_h: Some(DEFAULT_CARD_HEIGHT),
            gap_override: None,
            rect: Rect::ZERO,
            gap: 0.0,
            child_nodes: Vec::new(),
        }
    }

    /// Size each visual row to its tallest measured card at the final column width.
    /// App and `FrameHarness` settle this automatically. Manual hosts must use
    /// [`crate::widget::compute_widget_layout`] before applying or painting.
    /// Explicit child dimensions remain the caller's responsibility.
    pub fn content_sized(cards: Vec<Element>) -> Self {
        Self {
            card_h: None,
            ..Self::new(cards)
        }
    }

    #[must_use]
    pub fn with_min_card_width(mut self, px: f64) -> Self {
        self.min_card_w = px.max(1.0);
        self
    }

    /// Use an exact fixed row height, including after `content_sized`.
    /// Content exceeding this authored height may overflow.
    #[must_use]
    pub fn with_card_height(mut self, px: f64) -> Self {
        self.card_h = Some(px.max(1.0));
        self
    }

    /// Defaults to the theme gap.
    #[must_use]
    pub fn with_gap(mut self, px: f64) -> Self {
        self.gap_override = Some(px.max(0.0));
        self
    }

    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.min_card_w, self.gap).min(self.cards.len().max(1))
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical pixel sizes narrow to Taffy's f32"
    )]
    fn column_tracks(minimum: f64) -> Vec<taffy::GridTemplateComponent<String>> {
        vec![repeat(
            "auto-fit",
            vec![minmax(length(minimum as f32), fr(1.0_f32))],
        )]
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical pixel sizes narrow to Taffy's f32"
    )]
    fn grid_style(&self) -> Style {
        Style {
            display: taffy::Display::Grid,
            size: Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                // Intrinsic Grid rows need this item's automatic block minimum;
                // `flex_shrink` does not protect an item in Grid sizing.
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            align_self: Some(taffy::AlignSelf::Start),
            align_content: Some(taffy::AlignContent::Start),
            grid_template_columns: Self::column_tracks(self.min_card_w),
            grid_auto_rows: vec![
                self.card_h
                    .map_or_else(taffy::style_helpers::min_content, |height| {
                        length(height as f32)
                    }),
            ],
            gap: Size {
                width: length(self.gap as f32),
                height: length(self.gap as f32),
            },
            ..Default::default()
        }
    }
}

impl Widget for CardGrid {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = false;
        for card in &mut self.cards {
            changed |= card.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.gap = self
            .gap_override
            .unwrap_or_else(|| f64::from(theme.gap_standard));
        self.child_nodes = self
            .cards
            .iter_mut()
            .map(|card| card.build_layout(taffy, theme, node_map))
            .collect();
        let node = taffy.parent_node(self.grid_style(), &self.child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Ok(layout) = taffy.layout(node) else {
            return false;
        };
        // Resolve content tracks at the allocated width before measuring row
        // minima. Fixed mode only needs CSS min(100%, min_card_width), which
        // Taffy cannot express directly.
        let minimum = f64::from(layout.size.width).min(self.min_card_w).max(0.0);
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let tracks = if self.card_h.is_none() {
            let width = f64::from(layout.size.width);
            let columns = self.columns_for_width(width);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "host logical width narrows to Taffy precision"
            )]
            let column_width = card_column_width(width, columns, self.gap) as f32;
            vec![length(column_width); columns]
        } else {
            Self::column_tracks(minimum)
        };
        let mut style = current.clone();
        let mut changed = false;
        if style.grid_template_columns != tracks {
            style.grid_template_columns = tracks;
            changed |= taffy.set_style(node, style).is_ok();
        }
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let origin = Point::new(self.rect.x0, self.rect.y0);
        for (card, &child) in self.cards.iter_mut().zip(&self.child_nodes) {
            card.apply_layout(taffy, child, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for card in &mut self.cards {
            if presence.should_cull(card.get_rect()) {
                continue;
            }
            card.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        for (card, &node) in self.cards.iter_mut().zip(self.child_nodes.iter()) {
            card.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.cards.iter().map(std::convert::AsRef::as_ref).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for card in &self.cards {
            visitor(card.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cards
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn mark_drawn(&mut self) {
        for card in &mut self.cards {
            card.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "CardGrid"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for card in &mut self.cards {
            dirty |= card.take_structure_dirty();
        }
        dirty
    }
}

impl crate::undo::UndoPoints for CardGrid {
    fn needs_commit(&self) -> bool {
        self.cards
            .iter()
            .any(|c| crate::undo::needs_commit(c.as_ref()))
    }
    fn on_commit(&mut self) {
        for card in &mut self.cards {
            crate::undo::on_commit(card.as_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Column;

    // ---- pure reflow geometry ----

    #[test]
    fn card_columns_reflow_fill_and_align_by_width() {
        fn columns_reflow_by_width() {
            assert_eq!(card_columns(200.0, 260.0, 16.0), 1, "too narrow → 1 col");
            assert_eq!(card_columns(260.0, 260.0, 16.0), 1);
            // 2 cols need 260+16+260 = 536; +16 slack in the formula.
            assert_eq!(card_columns(536.0, 260.0, 16.0), 2);
            assert_eq!(card_columns(820.0, 260.0, 16.0), 3);
        }

        fn columns_never_zero() {
            assert_eq!(card_columns(0.0, 260.0, 16.0), 1);
            assert_eq!(card_columns(-50.0, 260.0, 16.0), 1);
            assert_eq!(card_columns(10.0, 260.0, 16.0), 1);
        }

        fn column_width_fills_and_aligns() {
            // 3 columns in 820 with 16 gaps: (820 - 2*16) / 3 = 262.67.
            let w = card_column_width(820.0, 3, 16.0);
            assert!((w - 262.6667).abs() < 0.01, "got {w}");
            // The columns + gaps exactly refill the width (they line up edge-to-edge).
            let total = 3.0 * w + 2.0 * 16.0;
            assert!(
                (total - 820.0).abs() < 0.01,
                "columns must refill width, got {total}"
            );
        }

        fn column_width_single_column_is_full_width() {
            assert!((card_column_width(500.0, 1, 16.0) - 500.0).abs() < 0.01);
        }

        columns_reflow_by_width();
        columns_never_zero();
        column_width_fills_and_aligns();
        column_width_single_column_is_full_width();
    }

    // ---- CardGrid layout ----

    use std::collections::HashMap;
    use vello::kurbo::Point;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value; f32 precision loss is \
                  immaterial"
    )]
    fn lay_out_grid(mut g: CardGrid, w: f64, h: f64) -> CardGrid {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = g.build_layout(&mut taffy, &theme, &mut map);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        unsafe {
            bindings.rebuild(&g, &taffy, node, &map);
        }
        unsafe {
            crate::widget::compute_widget_layout(
                &mut taffy,
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
                &map,
                &bindings,
                &mut tr,
            )
        }
        .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g
    }

    fn dummy_cards(n: usize) -> Vec<Element> {
        (0..n)
            .map(|i| Box::new(Card::new(Box::new(Text::new(&format!("card {i}"))))) as Element)
            .collect()
    }

    #[test]
    fn card_grid_resolves_columns_rows_and_height() {
        fn grid_resolves_columns_from_allocated_width() {
            let g = lay_out_grid(
                CardGrid::new(dummy_cards(6))
                    .with_min_card_width(260.0)
                    .with_gap(16.0),
                900.0,
                2000.0,
            );
            // 900px → 3 columns of ~(900-32)/3 = 289.3px each.
            assert_eq!(
                g.cards
                    .iter()
                    .filter(|card| card.get_rect().y0 == g.rect.y0)
                    .count(),
                3,
                "900px / 260-min → 3 columns"
            );
            assert!(
                (g.cards[0].get_rect().width() - 289.333).abs() < 1.0,
                "equal column width, got {}",
                g.cards[0].get_rect().width()
            );
        }

        fn grid_cells_align_to_shared_columns() {
            let g = lay_out_grid(
                CardGrid::new(dummy_cards(6))
                    .with_min_card_width(260.0)
                    .with_gap(16.0),
                900.0,
                2000.0,
            );
            // Card 0 and card 3 are in the same (first) column → identical x0.
            assert!(
                (g.cards[0].get_rect().x0 - g.cards[3].get_rect().x0).abs() < 0.01,
                "column 0 x aligns across rows"
            );
            // Card 0 and card 1 are in adjacent columns → x1 vs x2 separated by one gap.
            let gap_between = g.cards[1].get_rect().x0 - g.cards[0].get_rect().x1;
            assert!(
                (gap_between - 16.0).abs() < 0.5,
                "adjacent cards separated by one gap, got {gap_between}"
            );
            // Every cell has the same width → they line up.
            for i in 0..6 {
                assert!(
                    (g.cards[i].get_rect().width() - 289.333).abs() < 1.0,
                    "cell {i} width matches column"
                );
            }
        }

        fn grid_content_height_grows_with_rows() {
            // 6 cards, 3 cols → 2 rows; height = 2*card_h + 1*gap.
            let g = lay_out_grid(
                CardGrid::new(dummy_cards(6))
                    .with_min_card_width(260.0)
                    .with_card_height(150.0)
                    .with_gap(16.0),
                900.0,
                2000.0,
            );
            assert_eq!(g.get_rect().height(), 316.0);
        }

        fn grid_narrow_falls_to_single_column() {
            let g = lay_out_grid(
                CardGrid::new(dummy_cards(4))
                    .with_min_card_width(260.0)
                    .with_gap(16.0),
                300.0,
                2000.0,
            );
            assert_eq!(
                g.cards
                    .iter()
                    .filter(|card| card.get_rect().y0 == g.rect.y0)
                    .count(),
                1,
                "300px < 2-col stride → single column"
            );
            assert!(
                (g.cards[0].get_rect().width() - 300.0).abs() < 0.5,
                "single column spans full width"
            );
        }

        fn a_single_card_uses_the_full_grid_width() {
            let g = lay_out_grid(
                CardGrid::new(dummy_cards(1))
                    .with_min_card_width(260.0)
                    .with_gap(16.0),
                900.0,
                500.0,
            );
            assert_eq!(g.columns_for_width(900.0), 1);
            assert!((g.cards[0].get_rect().width() - 900.0).abs() < 0.5);
        }

        fn content_rows_use_each_rows_largest_natural_card() {
            let cards = || {
                vec![
                    Box::new(Card::new(Box::new(Text::new("Short")))) as Element,
                    Box::new(Card::new(Box::new(Text::new(
                        &"Long description with useful details. ".repeat(20),
                    )))) as Element,
                    Box::new(Card::new(Box::new(Text::new("Last row")))) as Element,
                ]
            };
            let wide = lay_out_grid(CardGrid::content_sized(cards()), 600.0, 2000.0);
            assert!(wide.cards[0].get_rect().height() > 160.0);
            assert_eq!(
                wide.cards[0].get_rect().height(),
                wide.cards[1].get_rect().height()
            );
            assert!(wide.cards[2].get_rect().height() < wide.cards[0].get_rect().height());
            let one = lay_out_grid(
                CardGrid::content_sized(vec![Box::new(Card::new(Box::new(Text::new(
                    &"Long description with useful details. ".repeat(20),
                ))))]),
                wide.cards[0].get_rect().width(),
                2000.0,
            );
            assert!(
                (one.cards[0].get_rect().height() - wide.cards[0].get_rect().height()).abs() < 1.0,
                "single {:?}, wide {:?}",
                one.cards[0].get_rect(),
                wide.cards[0].get_rect()
            );
            let empty = lay_out_grid(CardGrid::content_sized(vec![]), 600.0, 2000.0);
            assert_eq!(empty.rect.height(), 0.0);
        }

        grid_resolves_columns_from_allocated_width();
        grid_cells_align_to_shared_columns();
        grid_content_height_grows_with_rows();
        grid_narrow_falls_to_single_column();
        a_single_card_uses_the_full_grid_width();
        content_rows_use_each_rows_largest_natural_card();
    }

    // ---- Card surface ----

    fn lay_out_card(mut c: Card) -> Rect {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = c.build_layout(&mut taffy, &theme, &mut map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(600.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        c.apply_layout(&taffy, node, Point::ZERO);
        c.get_rect()
    }

    #[test]
    fn card_sizes_to_its_column_and_draws_a_themed_surface() {
        fn card_stretches_to_column_width() {
            // A card with no pinned width fills its column so a stack lines up.
            let rect = lay_out_card(Card::new(Box::new(Text::new("hi"))));
            assert!(
                (rect.width() - 600.0).abs() < 0.5,
                "card fills width, got {}",
                rect.width()
            );
        }

        fn card_keeps_intrinsic_height_in_a_short_flex_parent() {
            let mut parent = Column::new()
                .with_no_padding()
                .push(Box::new(Card::new(Box::new(
                    Container::new().with_no_padding().with_height(80.0),
                ))));
            let mut tree = TaffyTree::new();
            let mut map = HashMap::new();
            let node = parent.build_layout(&mut tree, &Theme::dark(), &mut map);
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
            )
            .unwrap();
            parent.apply_layout(&tree, node, Point::ZERO);
            assert!(
                parent.children()[0].get_rect().height() >= 80.0,
                "a manual Card retains its non-shrinking content height"
            );
        }

        fn card_draws_a_themed_surface() {
            use vello::peniko::Fill;
            let mut c = Card::titled("Group", Box::new(Text::new("body")));
            let _ = lay_out_card_mut(&mut c);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            c.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            // The card must emit at least one fill — the surface tint.
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == vello_encoding::DrawTag::COLOR)
                .count();
            assert!(fills >= 1, "a card must paint a themed surface fill");
            let _ = Fill::NonZero;
        }

        card_stretches_to_column_width();
        card_keeps_intrinsic_height_in_a_short_flex_parent();
        card_draws_a_themed_surface();
    }

    #[test]
    fn titled_card_title_is_a_scaled_heading_with_its_gap() {
        fn titled_card_keeps_title_gap_after_intrinsic_column_gap_fix() {
            let mut c = Card::titled(
                "Group",
                Box::new(Container::new().with_no_padding().with_height(10.0)),
            );
            let _ = lay_out_card_mut(&mut c);

            let surface = c.children()[0];
            let section = surface.children()[0];
            let column = section.children()[0];
            let title = column.children()[0].get_rect();
            let body = column.children()[2].get_rect();
            assert_eq!(body.y0 - title.y1, f64::from(CARD_TITLE_GAP));
        }

        fn titled_card_title_is_a_title_role_heading_that_follows_the_text_scale() {
            fn first_text(node: &crate::headless::InspectionNode) -> Option<[f64; 4]> {
                if node.widget == "Text" {
                    return Some(node.rect);
                }
                node.children.iter().find_map(first_text)
            }
            let title_height = |scale: f32| {
                let harness = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(Card::titled("Group", Box::new(Text::new("body")))),
                    (400, 200),
                    Theme::dark().with_text_scale(scale),
                );
                first_text(&harness.inspection().root).expect("card title text")[3]
            };
            let theme = Theme::dark();
            let typography = &theme.tokens.typography;
            let expected = f64::from(typography.scale.lg * typography.line_height_tight);
            let base = title_height(1.0);
            assert!(
                (base - expected).abs() < 1.0,
                "title line box {base} must come from the Title role ({expected})"
            );
            let large = title_height(2.0);
            assert!(
                (large - base * 2.0).abs() < 1.5,
                "title {base} -> {large} must follow the theme text scale"
            );
        }

        titled_card_keeps_title_gap_after_intrinsic_column_gap_fix();
        titled_card_title_is_a_title_role_heading_that_follows_the_text_scale();
    }

    fn lay_out_card_mut(c: &mut Card) -> Rect {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = c.build_layout(&mut taffy, &theme, &mut map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(600.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        c.apply_layout(&taffy, node, Point::ZERO);
        c.get_rect()
    }
}
