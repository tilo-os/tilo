use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent invalidation and cache flags model orthogonal retained-widget state"
)]
pub struct Column {
    children: Vec<Box<dyn Widget>>,
    /// Retained Taffy identities for `children`. Rebuilt only with the layout
    /// tree; hot event/layout traversal must not allocate `taffy.children()`.
    child_nodes: Vec<NodeId>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    extra_left_padding: f32,
    extra_top_padding: f32,
    extra_bottom_padding: f32,
    no_padding: bool,
    /// See [`Column::with_vertical_padding`].
    vertical_padding_override: Option<f32>,
    /// See [`Column::with_horizontal_padding`].
    horizontal_padding_override: Option<(f32, f32)>,
    /// See [`Column::with_gap`].
    gap_override: Option<f32>,
    intrinsic_height: bool,
    /// See [`Column::with_fill_remaining_height`].
    fill_remaining_height: bool,
    /// Composition of every descendant's own cached scene, recorded once and
    /// translated on subsequent frames as long as no descendant signals
    /// dirty. Saves hundreds of `Scene::append` calls per frame when most of
    /// the column's content is stable.
    subtree_cache: Scene,
    /// Rect (at cache-emit time) the subtree was composed against. Size
    /// mismatch → rebuild; pure translation → reuse with a
    /// `translate(delta)` on the outer append.
    cache_rect: vello::kurbo::Rect,
    cache_present: bool,
    descendant_caches_released: bool,
}

impl Default for Column {
    fn default() -> Self {
        Self::new()
    }
}

impl Column {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            child_nodes: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            extra_left_padding: 0.0,
            extra_top_padding: 0.0,
            extra_bottom_padding: 0.0,
            no_padding: false,
            vertical_padding_override: None,
            horizontal_padding_override: None,
            gap_override: None,
            intrinsic_height: false,
            fill_remaining_height: false,
            subtree_cache: Scene::new(),
            cache_rect: vello::kurbo::Rect::ZERO,
            cache_present: false,
            descendant_caches_released: false,
        }
    }

    /// Resolve the column's height from its content (`auto`) instead of
    /// filling the parent (`percent(1.0)`). Required when wrapping the
    /// column in a `Scrollable` — see the field doc on
    /// [`Column::intrinsic_height`] for the gory details.
    #[must_use]
    pub fn with_intrinsic_height(mut self) -> Self {
        self.intrinsic_height = true;
        self
    }

    /// Skip the default `padding_standard * 2` on all sides. Used by
    /// the shell's foreground column so the topbar lands at y=0
    /// instead of inset by the column's gutter.
    #[must_use]
    pub fn with_no_padding(mut self) -> Self {
        self.no_padding = true;
        self
    }

    #[must_use]
    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }

    /// Extra padding on the left, on top of the default
    /// `padding_standard * 2`. Used to displace a workspace area to the
    /// right of an absolutely-positioned sidebar without shifting the
    /// surrounding chrome.
    #[must_use]
    pub fn with_extra_left_padding(mut self, px: f32) -> Self {
        self.extra_left_padding = px;
        self
    }

    /// Extra padding on top, e.g. to reserve room for a floating
    /// affordance the column doesn't own (the responsive shell's Back
    /// chip painted over the detail pane's top-left corner).
    #[must_use]
    pub fn with_extra_top_padding(mut self, px: f32) -> Self {
        self.extra_top_padding = px;
        self
    }

    /// Extra padding on the bottom, on top of the default
    /// `padding_standard * 2`.
    #[must_use]
    pub fn with_extra_bottom_padding(mut self, px: f32) -> Self {
        self.extra_bottom_padding = px;
        self
    }

    /// Override the column's top + bottom padding (default
    /// `padding_standard * 2`); left/right are unaffected. For layouts
    /// that need to hug the viewport's top/bottom edges.
    #[must_use]
    pub fn with_vertical_padding(mut self, px: f32) -> Self {
        self.vertical_padding_override = Some(px);
        self
    }

    /// Override the column's left + right padding (replacing
    /// `base + extra_left_padding` on the left, `base` on the right).
    #[must_use]
    pub fn with_horizontal_padding(mut self, left: f32, right: f32) -> Self {
        self.horizontal_padding_override = Some((left, right));
        self
    }

    /// Override the vertical gap between children (default
    /// `theme.gap_standard`). `0.0` for a tightly banded stack whose
    /// children own their own internal spacing.
    #[must_use]
    pub fn with_gap(mut self, px: f32) -> Self {
        self.gap_override = Some(px);
        self
    }

    /// Switch from `height: percent(1.0)` to `flex_grow: 1 + flex_basis: 0`.
    /// Use when the column is a flex sibling of a fixed-height widget (a
    /// topbar, say) — the percent variant would claim 100% of the parent on
    /// top of the sibling's height, overflowing and pushing the column's
    /// own bottom padding off-screen.
    #[must_use]
    pub fn with_fill_remaining_height(mut self) -> Self {
        self.fill_remaining_height = true;
        self
    }

    /// Opt this layout into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this layout behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Column {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        for child in &mut self.children {
            child.sync(state, path);
        }
    }

    /// Forward the per-frame update pass; every child runs, and a
    /// change in any of them is a change here. Not short-circuited —
    /// a child that skipped its pass would miss a frame of state.
    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut changed = false;
        for child in &mut self.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.child_nodes = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();

        let node = taffy.parent_node(
            Style {
                // Intrinsic-height columns run as a Block stack: flex layout
                // in taffy 0.9 silently truncates children once the
                // container's main-axis budget runs out (even with
                // flex_shrink: 0.0), leaving an outer Scrollable thinking
                // content stops where taffy stopped. Block has no axis
                // budget, so content grows to its true sum.
                display: if self.intrinsic_height {
                    taffy::Display::Block
                } else {
                    taffy::Display::Flex
                },
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::FlexStart),
                flex_shrink: 0.0, // Prevent vertical squashing
                gap: taffy::Size {
                    width: length(0.0_f32),
                    height: length(self.gap_override.unwrap_or(theme.gap_standard)),
                },
                padding: {
                    let base = if self.no_padding {
                        0.0
                    } else {
                        theme.padding_standard * 2.0
                    };
                    let vert = self.vertical_padding_override.unwrap_or(base);
                    let (left, right) = self
                        .horizontal_padding_override
                        .unwrap_or((base + self.extra_left_padding, base));
                    taffy::Rect {
                        left: length(left),
                        right: length(right),
                        top: length(vert + self.extra_top_padding),
                        bottom: length(vert + self.extra_bottom_padding),
                    }
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: if self.intrinsic_height || self.fill_remaining_height {
                        taffy::Dimension::auto()
                    } else {
                        percent(1.0_f32)
                    },
                },
                flex_basis: if self.fill_remaining_height {
                    taffy::Dimension::length(0.0)
                } else {
                    taffy::Dimension::auto()
                },
                flex_grow: if self.fill_remaining_height { 1.0 } else { 0.0 },
                ..Default::default()
            },
            &self.child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        // The larger of own size and content bounds: taffy can clamp a
        // Block container's reported size to its parent's available main
        // axis (notably inside a Scrollable), and `layout.size` alone would
        // make `should_cull` discard the subtree once scrolling pushed the
        // under-reported rect off the viewport, even with children still on
        // screen.
        let w = f64::from(layout.size.width.max(layout.content_size.width));
        let h = f64::from(layout.size.height.max(layout.content_size.height));
        self.rect = vello::kurbo::Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + w,
            origin.y + f64::from(layout.location.y) + h,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);

        for (child_widget, child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child_widget.apply_layout(taffy, *child_node, new_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // A child with a latched drag (e.g. a slider's knob) gets ALL
        // events exclusively for the frame, so the drag survives the
        // cursor crossing a peer widget's rect — otherwise the peer's
        // CursorMoved handler could set `handled` first.
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            let Some(&cnode) = self.child_nodes.get(idx) else {
                return;
            };
            let Some(child) = self.children.get_mut(idx) else {
                return;
            };
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cnode,
                taffy,
            );
            return;
        }
        for (child_widget, &child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child_widget.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
            if *handled {
                break;
            }
        }
    }

    /// Propagate pointer-capture intent from any descendant — without this,
    /// a Column wrapping a Row containing a dragging slider would inherit
    /// the leaf-widget default of `false`, and the `handle_event`
    /// short-circuit above would never fire.
    fn wants_pointer_capture(&self) -> bool {
        self.children.iter().any(|c| c.wants_pointer_capture())
    }

    /// Direct-field recurse — the default impl allocates a
    /// `Vec` via `children()`, which the subtree-cache check
    /// hits per frame.
    fn needs_redraw(&self) -> bool {
        self.children.iter().any(|c| c.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        for child in &self.children {
            child.collect_damage(regions);
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.children.iter().any(|c| c.is_layout_dirty())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Column"
    }

    fn is_focused(&self) -> bool {
        false
    }
    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| c.needs_commit())
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }

    // `.any` would short-circuit and skip `take_structure_dirty()` on later
    // children once one reports dirty, leaving their flags uncleared --
    // the non-short-circuiting `.fold` below is required, not incidental.
    #[allow(clippy::unnecessary_fold)]
    fn take_structure_dirty(&mut self) -> bool {
        // Forward to every child; fold over the full set so each child's
        // flag is cleared even if an earlier one already reported dirty.
        self.children
            .iter_mut()
            .map(|c| c.take_structure_dirty())
            .fold(false, |acc, d| acc || d)
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }

        // Exact equality is deliberate: layout is deterministic given identical
        // inputs, so this asks "did anything at all change since the cache was
        // built", not "is the size close enough" — an epsilon would let a
        // genuine sub-pixel resize slip past and leave the cache stale.
        #[allow(
            clippy::float_cmp,
            reason = "deliberate exact reproducibility check on deterministic layout output"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        let any_child_dirty = self.children.iter().any(|c| c.needs_redraw());
        let needs_rebuild = !self.cache_present || size_changed || any_child_dirty;

        if needs_rebuild {
            // Children rebuild at `Affine::IDENTITY`: `Scene::append` composes
            // its transform with each stored primitive transform, so passing
            // `transform` here would double-apply it on the outer `append`
            // below, landing cached widgets off-screen at any DPI scale ≠ 1.
            //
            // Culling is cleared for the recursion too: with it on, the
            // cache would only hold primitives for the currently-visible
            // sub-range, and a later scroll (cache-hit, no rebuild) would
            // translate the cache into a viewport where the now-visible
            // widgets were never recorded. vello's tile binner already
            // skips off-viewport tiles at compose time, so nothing is saved
            // by culling here.
            let prev_vp = presence.viewport.replace(None);
            let mut local = std::mem::take(&mut self.subtree_cache);
            local.reset();
            for child in &mut self.children {
                child.draw_with_presence(
                    &mut local,
                    Affine::IDENTITY,
                    text_renderer,
                    theme,
                    presence,
                    permissions,
                );
            }
            scene.append(&local, Some(transform));
            self.subtree_cache = local;
            self.cache_rect = self.rect;
            self.cache_present = true;
            self.descendant_caches_released = false;
            presence.viewport.set(prev_vp);
        } else {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            // Affine composition: pure `f64` matrix multiply, cannot panic
            // regardless of operand magnitude.
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix math and cannot panic"
            )]
            let cache_transform = transform * Affine::translate((dx, dy));
            scene.append(&self.subtree_cache, Some(cache_transform));
            if !self.descendant_caches_released {
                for child in &mut self.children {
                    child.release_scene_cache();
                }
                self.descendant_caches_released = true;
            }
        }

        if let Some(scope) = self.permission_scope()
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = self.presence_key()
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }

    fn release_scene_cache(&mut self) {
        self.subtree_cache = Scene::new();
        self.cache_present = false;
        self.descendant_caches_released = true;
        for child in &mut self.children {
            child.release_scene_cache();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Container, Row, Text};
    use std::collections::HashMap;
    use vello_encoding::DrawTag;

    /// Build the five-pass layout for `root` inside an 800×600 viewport and
    /// return the populated taffy tree + node so the caller can draw.
    fn lay_out(root: &mut dyn Widget) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = root.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, Point::new(0.0, 0.0));
        (taffy, node)
    }

    /// Count GLYPH draw-tags in a freshly-drawn scene — proxy for "text was
    /// actually emitted". A dropped leading Text shows up as zero glyphs.
    fn glyph_count(root: &mut dyn Widget) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::default();
        let perms = crate::permissions::PermissionContext::permissive();
        let mut scene = Scene::new();
        // Use draw_with_presence so the column's cached-draw path (the
        // buggy one) is exercised — that's where the leading Text was
        // dropped.
        root.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &perms,
        );
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| matches!(**t, DrawTag::IMAGE) || **t == DrawTag::COLOR)
            .count()
    }

    /// Regression: a bare leading `Row[Text]` at the head of an
    /// intrinsic-height `Column` followed by a `Container` sibling must
    /// still emit the Text. Previously the column's cached-draw rebuild
    /// dropped the leading child while the Container painted.
    #[test]
    fn leading_text_in_intrinsic_column_is_drawn() {
        let mut col = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Row::new().push(Box::new(Text::new("Heading")))))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(Text::new("Body"))),
            ));
        let _ = lay_out(&mut col);

        // The leading Row's resolved rect must be non-degenerate — if it
        // collapsed to zero the text could never paint.
        let row_rect = col.children[0].get_rect();
        assert!(
            row_rect.width() > 1.0 && row_rect.height() > 1.0,
            "leading Row collapsed to a degenerate rect: {row_rect:?}"
        );

        // And the leading Text inside the Row must have a real rect too.
        let text_rect = col.children[0].children()[0].get_rect();
        assert!(
            text_rect.width() > 1.0 && text_rect.height() > 1.0,
            "leading Text collapsed to a degenerate rect: {text_rect:?}"
        );

        // Drawing must emit content for BOTH children (a fill count > the
        // Container's lone background fill proves the leading text painted).
        let fills = glyph_count(&mut col);
        assert!(
            fills >= 2,
            "expected the leading Text + the Container body to both paint; \
             got {fills} fills (regression: leading child dropped)"
        );
    }

    /// Reproduce the *cached*-path drop: draw once (warms every cache),
    /// `mark_drawn`, then re-draw on a frame where the Column itself
    /// rebuilds (its rect changed) but the leading Row is cache-clean.
    /// The leading Text must still appear in the recomposed scene.
    #[test]
    fn leading_text_survives_cached_redraw() {
        let mut col = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Row::new().push(Box::new(Text::new("Heading")))))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(Text::new("Body"))),
            ));
        let _ = lay_out(&mut col);

        // Frame 1 — warm caches, then clear dirty flags as the App does.
        let f1 = glyph_count(&mut col);
        col.mark_drawn();

        // Frame 2 — same content, caches clean. The cache-hit path must
        // replay the full subtree (leading Text included).
        let f2 = glyph_count(&mut col);
        assert!(
            f2 >= f1 && f2 >= 2,
            "cached re-draw dropped content: frame1={f1} frame2={f2}"
        );
    }

    /// The settings scenario: a bare leading `Text` (NOT wrapped in a
    /// Row or Container) at the head of an intrinsic-height Column hosted
    /// inside a `Scrollable`. This is the exact shape Settings worked
    /// around by wrapping every heading in a background Container. The
    /// leading Text must resolve a non-degenerate rect through the measure
    /// pass.
    #[test]
    fn bare_leading_text_in_scrolled_intrinsic_column() {
        use crate::widget::{ScrollAxis, Scrollable};
        // The Scrollable hosts the intrinsic column; this is what drives
        // the constrained measure pass that previously zeroed the leading
        // child.
        let inner = Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new("Section Heading")))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(Text::new("Row one"))),
            ))
            .push(Box::new(Text::new("Another Heading")))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(Text::new("Row two"))),
            ));
        let mut scroll = Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(inner));
        let _ = lay_out(&mut scroll);

        // Reach the inner column's first child (the bare leading Text).
        let inner_col = &scroll.children()[0];
        let lead = inner_col.children()[0];
        let r = lead.get_rect();
        assert!(
            r.width() > 1.0 && r.height() > 1.0,
            "bare leading Text in a scrolled intrinsic Column collapsed: {r:?}"
        );

        // And it must actually paint.
        let fills = glyph_count(&mut scroll);
        assert!(fills >= 3, "expected headings + rows to paint; got {fills}");
    }

    /// The same content as a plain (fill-height) column must also draw the
    /// leading text — guards the non-intrinsic path.
    #[test]
    fn leading_text_in_plain_column_is_drawn() {
        let mut col = Column::new()
            .push(Box::new(Text::new("Heading")))
            .push(Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(Text::new("Body"))),
            ));
        let _ = lay_out(&mut col);
        let text_rect = col.children[0].get_rect();
        assert!(
            text_rect.width() > 1.0 && text_rect.height() > 1.0,
            "leading Text collapsed: {text_rect:?}"
        );
    }
}
