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

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Column, Container, Element, Row, Spacer, Text, Widget};

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
/// Gap (logical px) between a card's title and its body.
const CARD_TITLE_GAP: f32 = 12.0;
/// Title text size (logical px) — the group heading.
const CARD_TITLE_SIZE: f32 = 15.0;

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
/// as one cohesive card.
fn surface(title: Option<String>, action: Option<Element>, body: Element) -> Element {
    let inner: Element = if let Some(title) = title {
        let mut title_row = Row::new().push(Box::new(Text::new(&title).with_size(CARD_TITLE_SIZE)));
        if let Some(action) = action {
            title_row = title_row.push(Box::new(Spacer::new())).push(action);
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(title_row))
                .push(vgap(CARD_TITLE_GAP))
                .push(body),
        )
    } else {
        body
    };
    Box::new(Container::new().with_background().with_child(inner))
}

/// An intrinsic-height `Column` lays out as a block stack that ignores the
/// taffy `gap`, so explicit spacers are how per-gap spacing is expressed there.
fn vgap(px: f32) -> Element {
    Box::new(Container::new().with_no_padding().with_height(px))
}

impl Widget for Card {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.inner.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
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
        self.inner.apply_layout(taffy, node, origin);
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
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
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

    fn needs_commit(&self) -> bool {
        self.inner.needs_commit()
    }

    fn on_commit(&mut self) {
        self.inner.on_commit();
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner.wants_pointer_capture()
    }
}

// ---------------------------------------------------------------------------
// CardGrid — a responsive grid of equal, aligned cards.
// ---------------------------------------------------------------------------

/// Reflows its card children into **equal, aligned columns** by width. Cards
/// share a fixed row height ([`with_card_height`](CardGrid::with_card_height));
/// the column count is derived from the allocated width via [`card_columns`].
/// Children are laid out in an isolated inner tree (mirroring
/// [`crate::widget::NavShell`]), so their content still measures and draws
/// while the grid owns the placement.
pub struct CardGrid {
    cards: Vec<Element>,
    min_card_w: f64,
    card_h: f64,
    gap_override: Option<f64>,

    // ---- resolved geometry (from the last layout / draw pass) ----
    rect: Rect,
    columns: usize,
    cell_w: f64,
    gap: f64,

    // ---- inner tree for the children ----
    inner_taffy: Option<TaffyTree<()>>,
    child_nodes: Vec<NodeId>,
}

impl CardGrid {
    /// Build a grid over `cards`. Defaults: [`DEFAULT_CARD_MIN_WIDTH`] minimum
    /// column width, [`DEFAULT_CARD_HEIGHT`] row height, theme gap.
    pub fn new(cards: Vec<Element>) -> Self {
        Self {
            cards,
            min_card_w: DEFAULT_CARD_MIN_WIDTH,
            card_h: DEFAULT_CARD_HEIGHT,
            gap_override: None,
            rect: Rect::ZERO,
            columns: 1,
            cell_w: DEFAULT_CARD_MIN_WIDTH,
            gap: 0.0,
            inner_taffy: None,
            child_nodes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_min_card_width(mut self, px: f64) -> Self {
        self.min_card_w = px.max(1.0);
        self
    }

    #[must_use]
    pub fn with_card_height(mut self, px: f64) -> Self {
        self.card_h = px.max(1.0);
        self
    }

    /// Defaults to the theme gap.
    #[must_use]
    pub fn with_gap(mut self, px: f64) -> Self {
        self.gap_override = Some(px.max(0.0));
        self
    }

    pub fn columns_for_width(&self, width: f64) -> usize {
        card_columns(width, self.min_card_w, self.gap)
    }

    fn content_height(&self, columns: usize) -> f64 {
        let n = self.cards.len();
        if n == 0 {
            return 0.0;
        }
        let rows = n.div_ceil(columns.max(1));
        usize_to_f64(rows) * self.card_h + usize_to_f64(rows.saturating_sub(1)) * self.gap
    }

    fn cell_origin(&self, i: usize) -> Point {
        let cols = self.columns.max(1);
        // `cols >= 1` always (just clamped above), so these never hit their
        // fallback — `checked_*` just spells that out for the reader (and
        // the lint) instead of an unchecked `%`/`/`.
        let col = i.checked_rem(cols).unwrap_or(0);
        let row = i.checked_div(cols).unwrap_or(0);
        Point::new(
            self.rect.x0 + usize_to_f64(col) * (self.cell_w + self.gap),
            self.rect.y0 + usize_to_f64(row) * (self.card_h + self.gap),
        )
    }

    fn cell_rect(&self, i: usize) -> Rect {
        let o = self.cell_origin(i);
        Rect::new(o.x, o.y, o.x + self.cell_w, o.y + self.card_h)
    }

    fn resolve_columns(&mut self) {
        let w = self.rect.width();
        self.columns = self.columns_for_width(w);
        self.cell_w = card_column_width(w, self.columns, self.gap);
    }

    /// Called from `draw` (which has the theme + text renderer), mirroring
    /// `NavShell`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "cell width/height are bounded on-screen pixel values; f32 precision loss \
                  is immaterial for widget layout"
    )]
    fn place_children(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        let nodes: Vec<NodeId> = self
            .cards
            .iter_mut()
            .map(|c| c.build_layout(&mut taffy, theme, &mut map))
            .collect();

        for (i, &node) in nodes.iter().enumerate() {
            let cell = self.cell_rect(i);
            if cell.width() <= 0.0 || cell.height() <= 0.0 {
                continue;
            }
            let _ = taffy.compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(cell.width() as f32),
                    height: taffy::AvailableSpace::Definite(cell.height() as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            );
        }

        let rects: Vec<Rect> = (0..self.cards.len()).map(|i| self.cell_rect(i)).collect();
        for ((card, &node), cell) in self.cards.iter_mut().zip(nodes.iter()).zip(rects.iter()) {
            card.apply_layout(&taffy, node, Point::new(cell.x0, cell.y0));
        }

        self.inner_taffy = Some(taffy);
        self.child_nodes = nodes;
    }
}

impl Widget for CardGrid {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        for card in &mut self.cards {
            card.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The cards are laid out in an inner tree during draw, so this node
        // is a measured leaf.
        self.gap = self
            .gap_override
            .unwrap_or_else(|| f64::from(theme.gap_standard));
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "width/height are bounded on-screen pixel values; f32 precision loss is \
                  immaterial for widget layout"
    )]
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(w) => f64::from(w),
                _ => self.min_card_w,
            },
            f64::from,
        );
        let columns = self.columns_for_width(width);
        let height = self.content_height(columns);
        taffy::Size {
            width: width as f32,
            height: height as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.resolve_columns();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.resolve_columns();
        self.place_children(theme, text_renderer);
        // Precompute cell rects so the cull check doesn't borrow `self` while
        // the cards are iterated mutably.
        let rects: Vec<Rect> = (0..self.cards.len()).map(|i| self.cell_rect(i)).collect();
        for (card, rect) in self.cards.iter_mut().zip(rects.iter()) {
            if presence.should_cull(*rect) {
                continue;
            }
            card.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Route to the child cells using the inner tree from the last draw.
        let Some(inner) = self.inner_taffy.take() else {
            return;
        };
        for (card, &node) in self.cards.iter_mut().zip(self.child_nodes.iter()) {
            card.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node,
                &inner,
            );
            if *handled {
                break;
            }
        }
        self.inner_taffy = Some(inner);
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

    fn needs_commit(&self) -> bool {
        self.cards.iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for card in &mut self.cards {
            card.on_commit();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for card in &mut self.cards {
            dirty |= card.take_structure_dirty();
        }
        dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure reflow geometry ----

    #[test]
    fn columns_reflow_by_width() {
        assert_eq!(card_columns(200.0, 260.0, 16.0), 1, "too narrow → 1 col");
        assert_eq!(card_columns(260.0, 260.0, 16.0), 1);
        // 2 cols need 260+16+260 = 536; +16 slack in the formula.
        assert_eq!(card_columns(536.0, 260.0, 16.0), 2);
        assert_eq!(card_columns(820.0, 260.0, 16.0), 3);
    }

    #[test]
    fn columns_never_zero() {
        assert_eq!(card_columns(0.0, 260.0, 16.0), 1);
        assert_eq!(card_columns(-50.0, 260.0, 16.0), 1);
        assert_eq!(card_columns(10.0, 260.0, 16.0), 1);
    }

    #[test]
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

    #[test]
    fn column_width_single_column_is_full_width() {
        assert!((card_column_width(500.0, 1, 16.0) - 500.0).abs() < 0.01);
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
        taffy
            .compute_layout_with_measure(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(w as f32),
                    height: taffy::AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
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
    fn grid_resolves_columns_from_allocated_width() {
        let g = lay_out_grid(
            CardGrid::new(dummy_cards(6))
                .with_min_card_width(260.0)
                .with_gap(16.0),
            900.0,
            2000.0,
        );
        // 900px → 3 columns of ~(900-32)/3 = 289.3px each.
        assert_eq!(g.columns, 3, "900px / 260-min → 3 columns");
        assert!(
            (g.cell_w - 289.333).abs() < 0.1,
            "equal column width, got {}",
            g.cell_w
        );
    }

    #[test]
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
            (g.cell_rect(0).x0 - g.cell_rect(3).x0).abs() < 0.01,
            "column 0 x aligns across rows"
        );
        // Card 0 and card 1 are in adjacent columns → x1 vs x2 separated by one gap.
        let gap_between = g.cell_rect(1).x0 - g.cell_rect(0).x1;
        assert!(
            (gap_between - 16.0).abs() < 0.5,
            "adjacent cards separated by one gap, got {gap_between}"
        );
        // Every cell has the same width → they line up.
        for i in 0..6 {
            assert!(
                (g.cell_rect(i).width() - g.cell_w).abs() < 0.01,
                "cell {i} width matches column"
            );
        }
    }

    #[test]
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
        assert_eq!(g.content_height(3), 2.0 * 150.0 + 16.0);
    }

    #[test]
    fn grid_narrow_falls_to_single_column() {
        let g = lay_out_grid(
            CardGrid::new(dummy_cards(4))
                .with_min_card_width(260.0)
                .with_gap(16.0),
            300.0,
            2000.0,
        );
        assert_eq!(g.columns, 1, "300px < 2-col stride → single column");
        assert!(
            (g.cell_w - 300.0).abs() < 0.5,
            "single column spans full width"
        );
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
    fn card_stretches_to_column_width() {
        // A card with no pinned width fills its column so a stack lines up.
        let rect = lay_out_card(Card::new(Box::new(Text::new("hi"))));
        assert!(
            (rect.width() - 600.0).abs() < 0.5,
            "card fills width, got {}",
            rect.width()
        );
    }

    #[test]
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
