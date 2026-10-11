//! What [`Column`](super::Column) and [`Row`](super::Row) share apart from
//! their axis: the child list and its retained Taffy nodes, the resolved
//! geometry, the presence / permission bindings, the theme-resolved spacing,
//! and the subtree scene cache that replays a stable subtree as one append.

use crate::permissions::PermissionScope;
use crate::presence::PathSegment;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::Widget;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent cache flags model orthogonal retained-widget state"
)]
pub(super) struct Flow {
    pub children: Vec<Box<dyn Widget>>,
    /// Retained Taffy identities for `children`. Rebuilt only with the layout
    /// tree; hot event/layout traversal must not allocate `taffy.children()`.
    pub child_nodes: Vec<NodeId>,
    /// Stable graph / test identity when the container was built from a
    /// serialized UI node.
    pub id: Option<String>,
    pub rect: Rect,
    /// Overflow extent used for culling independently of an exact allocation.
    pub paint_rect: Rect,
    pub presence_path: Vec<PathSegment>,
    pub permission_scope: Option<PermissionScope>,
    /// A pixel gap between children; replaced by a later `rhythm`.
    pub gap_override: Option<f32>,
    pub rhythm: Option<Rhythm>,
    /// Horizontal, vertical inset by rhythm step.
    pub padding_rhythm: Option<(Option<Rhythm>, Option<Rhythm>)>,
    /// Composition of every descendant's own cached scene, recorded once and
    /// translated on subsequent frames as long as no descendant signals
    /// dirty. Saves hundreds of `Scene::append` calls per frame when most of
    /// the container's content is stable.
    subtree_cache: Scene,
    /// Rect (at cache-emit time) the subtree was composed against. Size
    /// mismatch → rebuild; pure translation → reuse with a
    /// `translate(delta)` on the outer append.
    cache_rect: Rect,
    cache_present: bool,
    descendant_caches_released: bool,
    /// The cache holds floating surfaces over the blurred material. A reused
    /// cache would show them opaque, so it is redrawn while it does.
    cache_carries_material: bool,
    /// The scene parts the cache holds, recorded when it was drawn for a
    /// tracked section, so replaying it keeps the section's parts.
    cache_parts: Option<crate::scene_parts::SectionParts>,
    /// Whether the cached subtree shows a secret, replayed with the cache.
    cache_shows_secret: bool,
}

impl Flow {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            child_nodes: Vec::new(),
            id: None,
            rect: Rect::ZERO,
            paint_rect: Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            gap_override: None,
            rhythm: None,
            padding_rhythm: None,
            subtree_cache: Scene::new(),
            cache_rect: Rect::ZERO,
            cache_present: false,
            descendant_caches_released: false,
            cache_carries_material: false,
            cache_parts: None,
            cache_shows_secret: false,
        }
    }

    /// Redraw the subtree on the next frame instead of replaying the cache.
    pub fn invalidate(&mut self) {
        self.cache_present = false;
    }

    pub fn take_children(&mut self) -> Vec<Box<dyn Widget>> {
        self.child_nodes.clear();
        self.invalidate();
        std::mem::take(&mut self.children)
    }

    pub fn set_children(&mut self, children: Vec<Box<dyn Widget>>) {
        self.children = children;
        self.child_nodes.clear();
        self.invalidate();
    }

    /// Build every child's layout node, retaining their identities.
    pub fn build_children(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) {
        self.child_nodes = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
    }

    /// The gap between children: the pixel override, else the rhythm step,
    /// else the theme's `gap_standard`.
    pub fn gap(&self, theme: &Theme) -> f32 {
        self.gap_override
            .or_else(|| {
                self.rhythm
                    .map(|step| theme.tokens.spacing.rhythm.get(step))
            })
            .unwrap_or(theme.gap_standard)
    }

    /// The horizontal and vertical inset named by rhythm steps, if any.
    pub fn rhythm_padding(&self, theme: &Theme) -> Option<(f32, f32)> {
        let rhythm = theme.tokens.spacing.rhythm;
        let at = |step: Option<Rhythm>| step.map_or(0.0, |s| rhythm.get(s));
        self.padding_rhythm.map(|(x, y)| (at(x), at(y)))
    }

    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        (self.rect, self.paint_rect) = super::layout::flow_rects(taffy, node, origin);
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        for (child_widget, child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child_widget.apply_layout(taffy, *child_node, new_origin);
        }
    }

    /// Draw the children through the subtree cache. Returns `false` when the
    /// container is culled, so the caller skips its own decorations too.
    pub fn draw_cached(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool {
        if presence.should_cull(self.paint_rect) {
            return false;
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
        let tracked = crate::scene_parts::is_tracked(scene);
        let needs_rebuild = !self.cache_present
            || size_changed
            || any_child_dirty
            || self.cache_carries_material
            || (tracked && self.cache_parts.is_none());

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
            let mut draw_children = |local: &mut Scene| {
                for child in &mut self.children {
                    child.draw(local, Affine::IDENTITY, text_renderer, theme, presence);
                }
            };
            // A tracked section compares its parts frame to frame; a part
            // drawn into this cache would otherwise vanish from it.
            self.cache_parts = if tracked {
                crate::scene_parts::record(&mut local, draw_children).1
            } else {
                draw_children(&mut local);
                None
            };
            self.cache_shows_secret = secrets.finish();
            self.cache_carries_material = crate::scene_parts::append_with(
                scene,
                self.cache_parts.as_ref(),
                Some(transform),
                |scene| crate::backdrop::append_nested(scene, &local, transform),
            ) > 0;
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
            crate::scene_parts::append(
                scene,
                &self.subtree_cache,
                self.cache_parts.as_ref(),
                Some(cache_transform),
            );
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
        true
    }

    pub fn release_scene_cache(&mut self) {
        self.subtree_cache = Scene::new();
        self.cache_parts = None;
        self.cache_present = false;
        self.descendant_caches_released = true;
        for child in &mut self.children {
            child.release_scene_cache();
        }
    }
}

/// The builders every flow container offers, inside its inherent `impl`
/// (apps call them without importing a trait). The type has a `flow: Flow`.
macro_rules! flow_builders {
    () => {
        #[must_use]
        pub fn push(mut self, child: Box<dyn $crate::widget::Widget>) -> Self {
            self.flow.children.push(child);
            self
        }

        /// Address this container by a stable id in inspection and graph
        /// reconcile.
        #[must_use]
        pub fn with_id(mut self, id: impl Into<String>) -> Self {
            self.flow.id = Some(id.into());
            self
        }

        /// Take ownership of the child list for identity-preserving reconcile.
        pub fn take_children(&mut self) -> Vec<Box<dyn $crate::widget::Widget>> {
            self.flow.take_children()
        }

        /// Replace the child list after a graph reconcile pass.
        pub fn set_children(&mut self, children: Vec<Box<dyn $crate::widget::Widget>>) {
            self.flow.set_children(children);
        }

        /// Space the children (and a row's wrapped lines) by how they relate
        /// (HIG §7b): `Rhythm::Tight` for a label and its caption, `Related`
        /// for one cluster, `Sibling` for items of equal weight, `Section`
        /// between sections, `Group` between groups of sections. Resolved
        /// from the theme at layout time, so it follows density, `ui_scale`
        /// and a custom spacing scale. Replaces [`Self::with_gap`] with a
        /// pixel value.
        #[must_use]
        pub fn with_rhythm(mut self, rhythm: $crate::tokens::Rhythm) -> Self {
            self.flow.rhythm = Some(rhythm);
            self.flow.gap_override = None;
            self
        }

        /// Inset the children by a rhythm step on each axis (`None` is no
        /// inset on that axis), resolved from the theme like
        /// [`Self::with_rhythm`].
        #[must_use]
        pub fn with_padding_rhythm(
            mut self,
            horizontal: impl Into<Option<$crate::tokens::Rhythm>>,
            vertical: impl Into<Option<$crate::tokens::Rhythm>>,
        ) -> Self {
            self.flow.padding_rhythm = Some((horizontal.into(), vertical.into()));
            self
        }

        /// Opt this layout into presence tracking by binding it to a CRDT-style path.
        #[must_use]
        pub fn with_presence(mut self, path: Vec<$crate::presence::PathSegment>) -> Self {
            self.flow.presence_path = path;
            self
        }

        /// Gate this layout behind a permission scope.
        #[must_use]
        pub fn with_permission_scope(
            mut self,
            scope: $crate::permissions::PermissionScope,
        ) -> Self {
            self.flow.permission_scope = Some(scope);
            self
        }
    };
}
pub(super) use flow_builders;

/// The [`Widget`] methods a flow container answers from its [`Flow`],
/// inside its `impl Widget`. The container still names its own
/// `build_layout`, `name` and anything else that differs by axis.
macro_rules! flow_widget_methods {
    () => {
        fn start_operation(
            &mut self,
            op: &$crate::widget::ResourceOperation<'_>,
            doc: &mut $crate::widget::Edits<'_>,
            path: &automerge::ObjId,
        ) -> Result<Option<$crate::widget::ResourceOperationStart>, String> {
            // This container owns no operation handler: offer each child once.
            $crate::widget::start_in_children(self, op, doc, path)
        }

        /// Forward the per-frame update pass; every child runs, and a
        /// change in any of them is a change here. Not short-circuited —
        /// a child that skipped its pass would miss a frame of state.
        fn update(
            &mut self,
            state: &$crate::widget::AppModel,
            path: &$crate::widget::ModelPath,
        ) -> bool {
            let mut changed = false;
            for child in &mut self.flow.children {
                changed |= child.update(state, path);
            }
            changed
        }

        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.flow.apply_layout(taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text_renderer: &mut $crate::text_render::TextRenderer,
            theme: &$crate::theme::Theme,
            presence: &$crate::presence::PresenceCtx,
        ) {
            if self
                .flow
                .draw_cached(scene, transform, text_renderer, theme, presence)
            {
                $crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
            }
        }

        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut $crate::widget::EventCtx<'_>,
        ) {
            $crate::widget::route_to_children(
                &mut self.flow.children,
                &self.flow.child_nodes,
                event,
                ctx,
            );
        }

        /// Propagate pointer-capture intent from any descendant — without
        /// this, a column wrapping a row containing a dragging slider would
        /// inherit the leaf-widget default of `false`, and the capture
        /// routing in `route_to_children` would never fire.
        fn wants_pointer_capture(&self) -> bool {
            self.flow.children.iter().any(|c| c.wants_pointer_capture())
        }

        /// Direct-field recurse — the default impl allocates a
        /// `Vec` via `children()`, which the subtree-cache check
        /// hits per frame.
        fn needs_redraw(&self) -> bool {
            self.flow.children.iter().any(|c| c.needs_redraw())
        }

        fn is_layout_dirty(&self) -> bool {
            self.flow.children.iter().any(|c| c.is_layout_dirty())
        }

        fn children(&self) -> Vec<&dyn $crate::widget::Widget> {
            self.flow
                .children
                .iter()
                .map(std::convert::AsRef::as_ref)
                .collect()
        }

        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn $crate::widget::Widget)) {
            for child in &self.flow.children {
                visitor(child.as_ref());
            }
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn $crate::widget::Widget + '_)> {
            self.flow
                .children
                .iter_mut()
                .map(|c| c.as_mut() as &mut (dyn $crate::widget::Widget + '_))
                .collect()
        }

        fn mark_drawn(&mut self) {
            for child in &mut self.flow.children {
                child.mark_drawn();
            }
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.flow.rect
        }

        fn paint_bounds(&self) -> vello::kurbo::Rect {
            self.flow.paint_rect
        }

        fn test_id(&self) -> Option<&str> {
            self.flow.id.as_deref()
        }

        // `.any` would short-circuit and skip `take_structure_dirty()` on later
        // children once one reports dirty, leaving their flags uncleared --
        // the non-short-circuiting `.fold` below is required, not incidental.
        #[allow(
            clippy::unnecessary_fold,
            reason = "Every child must clear its structure-dirty flag; a short-circuiting any would leave later children dirty."
        )]
        fn take_structure_dirty(&mut self) -> bool {
            self.flow
                .children
                .iter_mut()
                .map(|c| c.take_structure_dirty())
                .fold(false, |acc, d| acc || d)
        }

        fn release_scene_cache(&mut self) {
            self.flow.release_scene_cache();
        }
    };
}
pub(super) use flow_widget_methods;

/// The presence and permission capabilities of a flow container.
macro_rules! flow_capabilities {
    ($ty:ty) => {
        impl $crate::presence::capability::PublishesPresence for $ty {
            fn presence_key(&self) -> Option<$crate::presence::PresenceKey> {
                $crate::presence::PresenceKey::widget(&self.flow.presence_path)
            }
        }

        impl $crate::permissions::capability::PermissionGated for $ty {
            fn permission_scope(&self) -> Option<$crate::permissions::PermissionScope> {
                self.flow.permission_scope.clone()
            }
        }
    };
}
pub(super) use flow_capabilities;
