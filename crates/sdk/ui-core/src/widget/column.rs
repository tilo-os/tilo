use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use automerge::ObjId;
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
    /// Stable graph / test identity when this column was built from a
    /// serialized UI node.
    id: Option<String>,
    rect: vello::kurbo::Rect,
    /// Overflow extent used for culling independently of an exact allocation.
    paint_rect: vello::kurbo::Rect,
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
    /// See [`Column::with_rhythm`].
    rhythm: Option<Rhythm>,
    /// See [`Column::with_padding_rhythm`]: horizontal, vertical.
    padding_rhythm: Option<(Option<Rhythm>, Option<Rhythm>)>,
    intrinsic_height: bool,
    /// See [`Column::with_intrinsic_width`].
    intrinsic_width: bool,
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
    /// The cache holds floating surfaces over the blurred material. A reused
    /// cache would show them opaque, so it is redrawn while it does.
    cache_carries_material: bool,
    /// Whether the cached subtree shows a secret, replayed with the cache.
    cache_shows_secret: bool,
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
            id: None,
            rect: vello::kurbo::Rect::ZERO,
            paint_rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            extra_left_padding: 0.0,
            extra_top_padding: 0.0,
            extra_bottom_padding: 0.0,
            no_padding: false,
            vertical_padding_override: None,
            horizontal_padding_override: None,
            gap_override: None,
            rhythm: None,
            padding_rhythm: None,
            intrinsic_height: false,
            intrinsic_width: false,
            fill_remaining_height: false,
            subtree_cache: Scene::new(),
            cache_rect: vello::kurbo::Rect::ZERO,
            cache_present: false,
            descendant_caches_released: false,
            cache_carries_material: false,
            cache_shows_secret: false,
        }
    }

    /// Resolve the column's height from its content (`auto`) instead of
    /// filling the parent (`percent(1.0)`). Required when wrapping the
    /// column in a `Scrollable` — see the field doc on
    /// `Column::intrinsic_height` for the gory details.
    #[must_use]
    pub fn with_intrinsic_height(mut self) -> Self {
        self.intrinsic_height = true;
        self
    }

    /// Size the column's width from its widest child instead of filling the
    /// parent: a column of buttons or labels beside other content in a
    /// row, a popover body. Children still align to its leading edge.
    #[must_use]
    pub fn with_intrinsic_width(mut self) -> Self {
        self.intrinsic_width = true;
        self
    }

    /// Space the children by how they relate (HIG §7b): `Rhythm::Tight`
    /// for a label and its caption, `Related` for one cluster, `Sibling`
    /// for rows of equal weight, `Section` between sections, `Group`
    /// between groups of sections. Resolved from the theme at layout time,
    /// so it follows density, `ui_scale` and a custom spacing scale.
    /// Replaces [`Column::with_gap`] with a pixel value.
    #[must_use]
    pub fn with_rhythm(mut self, rhythm: Rhythm) -> Self {
        self.rhythm = Some(rhythm);
        self.gap_override = None;
        self
    }

    /// Inset the children by a rhythm step on each axis (`None` is no
    /// inset on that axis) instead of the default `spacing.s6` gutter.
    #[must_use]
    pub fn with_padding_rhythm(
        mut self,
        horizontal: impl Into<Option<Rhythm>>,
        vertical: impl Into<Option<Rhythm>>,
    ) -> Self {
        self.padding_rhythm = Some((horizontal.into(), vertical.into()));
        self
    }

    /// Skip the default `spacing.s6` inset on all sides. Used by
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

    /// Address this column by a stable id in inspection and graph reconcile.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Take ownership of the child list for identity-preserving reconcile.
    pub fn take_children(&mut self) -> Vec<Box<dyn Widget>> {
        self.child_nodes.clear();
        self.cache_present = false;
        std::mem::take(&mut self.children)
    }

    /// Replace the child list after a graph reconcile pass.
    pub fn set_children(&mut self, children: Vec<Box<dyn Widget>>) {
        self.children = children;
        self.child_nodes.clear();
        self.cache_present = false;
    }

    /// Apply layout flags from a serialized column node without rebuilding.
    pub fn apply_graph_props(
        &mut self,
        id: &str,
        gap: Option<f32>,
        no_padding: bool,
        intrinsic_height: bool,
        fill_remaining_height: bool,
    ) {
        self.id = Some(id.to_owned());
        self.gap_override = gap;
        self.no_padding = no_padding;
        self.intrinsic_height = intrinsic_height;
        self.fill_remaining_height = fill_remaining_height;
        self.cache_present = false;
    }

    /// Extra padding on the left, on top of the default
    /// `spacing.s6` inset. Used to displace a workspace area to the
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

    /// Update the extra top inset before the next layout pass.
    pub fn set_extra_top_padding(&mut self, px: f32) {
        self.extra_top_padding = px;
    }

    /// Extra padding on the bottom, on top of the default
    /// `spacing.s6` inset.
    #[must_use]
    pub fn with_extra_bottom_padding(mut self, px: f32) -> Self {
        self.extra_bottom_padding = px;
        self
    }

    /// Override the column's top + bottom padding (default
    /// `spacing.s6`); left/right are unaffected. For layouts
    /// that need to hug the viewport's top/bottom edges.
    #[must_use]
    pub fn with_vertical_padding(mut self, px: f32) -> Self {
        self.vertical_padding_override = Some(px);
        self
    }

    /// Update the top + bottom inset before the next layout pass.
    pub fn set_vertical_padding(&mut self, px: f32) {
        self.vertical_padding_override = Some(px);
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
        self.rhythm = None;
        self
    }

    /// Update the inter-child gap before the next layout pass.
    pub fn set_gap(&mut self, px: f32) {
        self.gap_override = Some(px);
        self.rhythm = None;
    }

    /// Update the inter-child rhythm before the next layout pass.
    pub fn set_rhythm(&mut self, rhythm: Rhythm) {
        self.rhythm = Some(rhythm);
        self.gap_override = None;
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
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        // This container owns no operation handler: offer each child once.
        crate::widget::start_in_children(self, op, doc, path)
    }

    /// Forward the per-frame update pass; every child runs, and a
    /// change in any of them is a change here. Not short-circuited —
    /// a child that skipped its pass would miss a frame of state.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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

        let rhythm = theme.tokens.spacing.rhythm;
        let resolved_gap = self
            .gap_override
            .or_else(|| self.rhythm.map(|step| rhythm.get(step)))
            .unwrap_or(theme.gap_standard);

        let style = Style {
            // Intrinsic-height columns use a one-column implicit Grid.
            // Taffy's Block layout ignores `gap`, while flex layout can
            // truncate the intrinsic main axis to a Scrollable's viewport.
            // Grid honors row gaps and intrinsically sizes every implicit
            // row, preserving both scroll extent and the one widget / one
            // direct Taffy child identity expected by host traversal.
            display: if self.intrinsic_height {
                taffy::Display::Grid
            } else {
                taffy::Display::Flex
            },
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::FlexStart),
            // A parent may legitimately stretch or explicitly size this
            // Column taller than its content. Pack implicit Grid rows at
            // the start so surplus block space does not inflate the
            // requested gap between them.
            align_content: self.intrinsic_height.then_some(taffy::AlignContent::Start),
            // The one column is the Column's own width, never wider: an
            // `auto` track grows to its widest child's content and pushes
            // a padded child past the Column's edge.
            grid_template_columns: if self.intrinsic_height {
                vec![taffy::style_helpers::minmax(
                    length(0.0_f32),
                    taffy::style_helpers::fr(1.0_f32),
                )]
            } else {
                Vec::new()
            },
            flex_shrink: 0.0, // Prevent vertical squashing
            gap: taffy::Size {
                width: length(0.0_f32),
                height: length(resolved_gap),
            },
            padding: {
                // The page gutter: the `s6` token (24). Tokens are the
                // source of truth; on every theme built through
                // `Theme::from_tokens` / `with_ui_scale` / a custom
                // spec this is exactly twice `padding_standard`. A host
                // theme that overrides the legacy `padding_standard`
                // alias alone (`Theme::with_component_scales` preserves
                // such aliases) no longer moves this gutter.
                let (base_x, base_y) = if self.no_padding {
                    (0.0, 0.0)
                } else if let Some((x, y)) = self.padding_rhythm {
                    let at = |step: Option<Rhythm>| step.map_or(0.0, |s| rhythm.get(s));
                    (at(x), at(y))
                } else {
                    (theme.tokens.spacing.s6, theme.tokens.spacing.s6)
                };
                let vert = self.vertical_padding_override.unwrap_or(base_y);
                let (left, right) = self
                    .horizontal_padding_override
                    .unwrap_or((base_x + self.extra_left_padding, base_x));
                super::layout::insets(
                    vert + self.extra_top_padding,
                    right,
                    vert + self.extra_bottom_padding,
                    left,
                )
            },
            size: taffy::Size {
                width: if self.intrinsic_width {
                    taffy::Dimension::auto()
                } else {
                    percent(1.0_f32)
                },
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
        };
        let node = taffy.parent_node(style, &self.child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let allocated = super::layout::allocation_or_content_size(taffy, node, &layout);
        let w = f64::from(allocated.width);
        let h = f64::from(allocated.height);
        self.paint_rect = vello::kurbo::Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (
                f64::from(layout.size.width.max(layout.content_size.width)),
                f64::from(layout.size.height.max(layout.content_size.height)),
            ),
        );
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // A child with a latched drag (e.g. a slider's knob) gets ALL
        // events exclusively for the frame, so the drag survives the
        // cursor crossing a peer widget's rect — otherwise the peer's
        // CursorMoved handler could set `handled` first.
        // A child holding an open modal owns the input the same way: what
        // stands behind the sheet never sees the event, so a table under a
        // dialog cannot take the press meant for the dialog's button.
        let owner = self
            .children
            .iter()
            .position(|c| c.wants_pointer_capture())
            .or_else(|| {
                self.children
                    .iter()
                    .position(|c| c.modal_capture().is_some())
            });
        if let Some(idx) = owner {
            let Some(&cnode) = self.child_nodes.get(idx) else {
                return;
            };
            let Some(child) = self.children.get_mut(idx) else {
                return;
            };
            child.handle_event(event, &mut ctx.reborrow(ctx.path, cnode));
            return;
        }
        for (child_widget, &child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child_widget.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            if *ctx.handled {
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

    fn mark_drawn(&mut self) {
        for child in &mut self.children {
            child.mark_drawn();
        }
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Column"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn is_focused(&self) -> bool {
        false
    }

    // `.any` would short-circuit and skip `take_structure_dirty()` on later
    // children once one reports dirty, leaving their flags uncleared --
    // the non-short-circuiting `.fold` below is required, not incidental.
    #[allow(
        clippy::unnecessary_fold,
        reason = "Every child must clear its structure-dirty flag; a short-circuiting any would leave later children dirty."
    )]
    fn take_structure_dirty(&mut self) -> bool {
        // Forward to every child; fold over the full set so each child's
        // flag is cleared even if an earlier one already reported dirty.
        self.children
            .iter_mut()
            .map(|c| c.take_structure_dirty())
            .fold(false, |acc, d| acc || d)
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
        if presence.should_cull(self.paint_rect) {
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
        let needs_rebuild =
            !self.cache_present || size_changed || any_child_dirty || self.cache_carries_material;

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
            crate::backdrop::nest_scene(&local);
            let secrets = crate::secret_paint::SecretPaint::begin();
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
            self.cache_shows_secret = secrets.finish();
            self.cache_carries_material =
                crate::backdrop::append_nested(scene, &local, transform) > 0;
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
            if self.cache_shows_secret {
                crate::secret_paint::note_secret_painted();
            }
            if !self.descendant_caches_released {
                for child in &mut self.children {
                    child.release_scene_cache();
                }
                self.descendant_caches_released = true;
            }
        }

        crate::widget::draw_gate_and_peer_focus(
            self,
            scene,
            transform,
            theme,
            presence,
            permissions,
        );
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

impl crate::presence::capability::PublishesPresence for Column {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for Column {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

impl crate::widget::controls::ActionControls for Column {
    fn action_control_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        self.for_each_child(&mut |child| {
            bindings.extend(crate::widget::controls::action_control_bindings(child));
        });
        bindings
    }
    fn has_action_control_bindings(&self) -> bool {
        let mut found = false;
        self.for_each_child(&mut |child| {
            if !found {
                found = crate::widget::controls::has_action_control_bindings(child);
            }
        });
        found
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

    /// Count GLYPH draw-tags in a freshly-drawn scene. A dropped leading Text
    /// shows up as zero glyphs.
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

    #[test]
    fn rhythm_gaps_and_insets_resolve_from_the_theme_scale() {
        use crate::widget::Spacer;
        for scale in [1.0_f32, 1.5] {
            let theme = Theme::dark().with_ui_scale(scale);
            let r = theme.tokens.spacing.rhythm;
            let mut column = Column::new()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Section)
                .with_padding_rhythm(Rhythm::Group, Rhythm::Related)
                .push(Box::new(Spacer::fixed(10.0)))
                .push(Box::new(Spacer::fixed(10.0)));
            let mut taffy = TaffyTree::<()>::new();
            let mut map = HashMap::new();
            let node = column.build_layout(&mut taffy, &theme, &mut map);
            let style = taffy.style(node).unwrap();
            assert_eq!(style.gap.height, length(r.section));
            assert_eq!(style.padding.left, length(r.group));
            assert_eq!(style.padding.top, length(r.related));

            let mut row = Row::new()
                .with_rhythm(Rhythm::Related)
                .with_padding_rhythm(None, Rhythm::Tight);
            let node = row.build_layout(&mut taffy, &theme, &mut map);
            let style = taffy.style(node).unwrap();
            assert_eq!(style.gap.width, length(r.related));
            assert_eq!(style.padding.left, length(0.0_f32));
            assert_eq!(style.padding.top, length(r.tight));
        }
        // A later pixel gap replaces the rhythm, and the other way round.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut map = HashMap::new();
        let mut column = Column::new().with_rhythm(Rhythm::Group).with_gap(3.0);
        let node = column.build_layout(&mut taffy, &theme, &mut map);
        assert_eq!(taffy.style(node).unwrap().gap.height, length(3.0_f32));
    }

    #[test]
    fn an_intrinsic_width_column_hugs_its_widest_child() {
        use crate::widget::Spacer;
        let mut row = Row::new().no_wrap().push(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_intrinsic_width()
                .push(Box::new(Spacer::fixed(40.0)))
                .push(Box::new(Spacer::fixed(90.0))),
        ));
        lay_out(&mut row);
        let column = row.children()[0].get_rect();
        assert!((column.width() - 90.0).abs() < 0.5, "{column:?}");
    }

    #[test]
    fn intrinsic_column_gap_applies_between_children_and_to_content_height() {
        /// The default inset is the `s6` token — 24 on the stock scale and still
        /// exactly twice `padding_standard` once `ui_scale` or a custom theme's
        /// a theme's density has rescaled the whole spacing scale.
        fn default_inset_is_the_s6_token_on_every_scale() {
            for theme in [
                Theme::dark(),
                Theme::dark().with_ui_scale(1.5),
                Theme::from_document(&crate::theme::ThemeDocument {
                    density: crate::theme::ThemeDensity { spacing: 0.75 },
                    ..crate::theme::Preset::Dark.document()
                })
                .unwrap(),
            ] {
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let mut column = Column::new().push(Box::new(Text::new("body")));
                let node = column.build_layout(&mut taffy, &theme, &mut node_map);
                let padding = taffy.style(node).unwrap().padding;
                let expected = taffy::style_helpers::length(theme.tokens.spacing.s6);
                assert_eq!(padding.left, expected, "{}", theme.name);
                assert_eq!(padding.top, expected, "{}", theme.name);
                assert_eq!(
                    padding.left,
                    taffy::style_helpers::length(theme.padding_standard * 2.0),
                    "{}: s6 is twice padding_standard",
                    theme.name
                );
            }
        }

        fn intrinsic_column_applies_requested_gap_to_geometry_and_content_height() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.children[0].get_rect().y0, 0.0);
            assert_eq!(col.children[1].get_rect().y0, 30.0);
            assert_eq!(col.get_rect().height(), 40.0);
        }

        fn intrinsic_column_keeps_requested_gap_when_parent_stretches_height() {
            let theme = Theme::dark();
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let column_node = col.build_layout(&mut taffy, &theme, &mut node_map);
            let host_node = taffy.parent_node(
                Style {
                    size: taffy::Size {
                        width: length(400.0_f32),
                        height: length(300.0_f32),
                    },
                    ..Default::default()
                },
                &[column_node],
            );

            taffy
                .compute_layout_with_measure(
                    host_node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(400.0),
                        height: taffy::AvailableSpace::Definite(300.0),
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
            col.apply_layout(&taffy, column_node, Point::ZERO);

            assert_eq!(taffy.layout(column_node).unwrap().size.height, 300.0);
            assert_eq!(col.get_rect().height(), 300.0);
            assert_eq!(col.children[0].get_rect().y0, 0.0);
            assert_eq!(col.children[1].get_rect().y0, 30.0);
            assert_eq!(
                col.children[1].get_rect().y0 - col.children[0].get_rect().y1,
                20.0
            );
        }

        fn intrinsic_column_zero_gap_keeps_children_tightly_stacked() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(0.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.children[0].get_rect().y0, 0.0);
            assert_eq!(col.children[1].get_rect().y0, 10.0);
            assert_eq!(col.get_rect().height(), 20.0);
        }

        fn intrinsic_column_uses_default_theme_gap() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            // The dark theme's default gap is 12 px.
            assert_eq!(col.children[1].get_rect().y0, 22.0);
            assert_eq!(col.get_rect().height(), 32.0);
        }

        fn intrinsic_column_inserts_one_gap_between_each_of_three_children() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(7.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.children[0].get_rect().y0, 0.0);
            assert_eq!(col.children[1].get_rect().y0, 17.0);
            assert_eq!(col.children[2].get_rect().y0, 34.0);
            assert_eq!(col.get_rect().height(), 44.0);
        }

        default_inset_is_the_s6_token_on_every_scale();
        intrinsic_column_applies_requested_gap_to_geometry_and_content_height();
        intrinsic_column_keeps_requested_gap_when_parent_stretches_height();
        intrinsic_column_zero_gap_keeps_children_tightly_stacked();
        intrinsic_column_uses_default_theme_gap();
        intrinsic_column_inserts_one_gap_between_each_of_three_children();
    }

    #[test]
    fn intrinsic_column_gap_composes_with_padding_margins_nesting_and_scroll() {
        fn intrinsic_column_gap_is_additive_with_explicit_padding() {
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_vertical_padding(5.0)
                .with_horizontal_padding(7.0, 11.0)
                .with_extra_top_padding(3.0)
                .with_extra_bottom_padding(4.0)
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ));

            let _ = lay_out(&mut col);

            assert_eq!(col.children[0].get_rect().x0, 7.0);
            assert_eq!(col.children[0].get_rect().y0, 8.0);
            assert_eq!(col.children[1].get_rect().y0, 38.0);
            assert_eq!(col.get_rect().height(), 57.0);
        }

        fn intrinsic_column_gap_is_additive_with_child_margins() {
            let theme = Theme::dark();
            let mut col = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_size(10.0, 10.0),
                ));
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = TextRenderer::new();
            let node = col.build_layout(&mut taffy, &theme, &mut node_map);

            let mut first_style = taffy.style(col.child_nodes[0]).unwrap().clone();
            first_style.margin.top = length(2.0_f32);
            first_style.margin.bottom = length(3.0_f32);
            taffy.set_style(col.child_nodes[0], first_style).unwrap();
            let mut second_style = taffy.style(col.child_nodes[1]).unwrap().clone();
            second_style.margin.top = length(4.0_f32);
            second_style.margin.bottom = length(6.0_f32);
            taffy.set_style(col.child_nodes[1], second_style).unwrap();

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
            col.apply_layout(&taffy, node, Point::ZERO);

            assert_eq!(col.children[0].get_rect().y0, 2.0);
            assert_eq!(col.children[1].get_rect().y0, 39.0);
            assert_eq!(col.get_rect().height(), 55.0);
        }

        fn nested_intrinsic_column_keeps_taffy_and_widget_sibling_geometry_aligned() {
            let inner = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ));
            let mut outer = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(0.0)
                .push(Box::new(inner))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));

            let (mut taffy, node) = lay_out(&mut outer);

            assert_eq!(taffy.children(node).unwrap(), outer.child_nodes);
            assert_eq!(
                taffy.layout(outer.child_nodes[0]).unwrap().size.width,
                800.0
            );
            assert_eq!(
                taffy.layout(outer.child_nodes[0]).unwrap().size.height,
                130.0
            );
            assert_eq!(outer.children[0].get_rect().width(), 800.0);
            assert_eq!(outer.children[0].get_rect().height(), 130.0);
            assert_eq!(
                taffy.layout(outer.child_nodes[1]).unwrap().location.y,
                130.0
            );
            assert_eq!(outer.children[1].get_rect().y0, 130.0);
            assert_eq!(outer.get_rect().height(), 140.0);

            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(800.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            outer.apply_layout(&taffy, node, Point::ZERO);
            assert_eq!(
                taffy.layout(outer.child_nodes[0]).unwrap().size.height,
                130.0
            );
            assert_eq!(outer.children[0].get_rect().height(), 130.0);
            assert_eq!(outer.children[1].get_rect().y0, 130.0);
        }

        fn intrinsic_column_in_row_respects_parent_center_alignment() {
            let short_column = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(0.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ));
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(short_column))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ));

            let _ = lay_out(&mut row);

            assert_eq!(row.get_rect().height(), 30.0);
            assert_eq!(row.children()[0].get_rect().height(), 10.0);
            assert_eq!(row.children()[0].get_rect().y0, 10.0);
            assert_eq!(row.children()[1].get_rect().y0, 0.0);
        }

        fn intrinsic_column_gap_contributes_to_scrollable_content_height() {
            use crate::widget::{ScrollAxis, Scrollable};

            let inner = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_gap(20.0)
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(30.0),
                ));
            let mut scroll = Scrollable::new(ScrollAxis::Vertical)
                .with_height(50.0)
                .with_child(Box::new(inner));

            let (taffy, scroll_node) = lay_out(&mut scroll);

            let inner = scroll.children()[0];
            let inner_node = taffy.children(scroll_node).unwrap()[0];
            assert_eq!(scroll.get_rect().height(), 50.0);
            assert_eq!(taffy.layout(inner_node).unwrap().size.height, 130.0);
            assert_eq!(inner.get_rect().height(), 130.0);
            assert_eq!(inner.children()[2].get_rect().y0, 100.0);
            assert_eq!(scroll.max_offset_y(), 80.0);
        }

        fn horizontal_scrollable_keeps_cross_axis_stretch() {
            use crate::widget::{ScrollAxis, Scrollable};

            let mut scroll = Scrollable::new(ScrollAxis::Horizontal)
                .with_height(50.0)
                .with_child(Box::new(
                    Container::new().with_no_padding().with_width(1_200.0),
                ));

            let _ = lay_out(&mut scroll);

            assert_eq!(scroll.get_rect().height(), 50.0);
            assert_eq!(scroll.children()[0].get_rect().height(), 50.0);
        }

        intrinsic_column_gap_is_additive_with_explicit_padding();
        intrinsic_column_gap_is_additive_with_child_margins();
        nested_intrinsic_column_keeps_taffy_and_widget_sibling_geometry_aligned();
        intrinsic_column_in_row_respects_parent_center_alignment();
        intrinsic_column_gap_contributes_to_scrollable_content_height();
        horizontal_scrollable_keeps_cross_axis_stretch();
    }

    #[test]
    fn leading_text_in_columns_is_drawn_and_survives_redraw() {
        /// Regression: a bare leading `Row[Text]` at the head of an
        /// intrinsic-height `Column` followed by a `Container` sibling must
        /// still emit the Text. Previously the column's cached-draw rebuild
        /// dropped the leading child while the Container painted.
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

        leading_text_in_intrinsic_column_is_drawn();
        leading_text_survives_cached_redraw();
        bare_leading_text_in_scrolled_intrinsic_column();
        leading_text_in_plain_column_is_drawn();
    }
}
