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
    /// What of the theme the cache was painted in: its colours, radii and
    /// material. A theme change (dark to light, a preset, high contrast)
    /// moves no child's size and dirties no child, yet every pixel of the
    /// cache is in the old theme, so a differing theme redraws it.
    cache_theme: Option<Box<DrawnTheme>>,
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
    /// A setter changed the container's own layout style (its padding or
    /// gap) after its node was built. The retained layout tree keeps the
    /// built style, so the change reports a structure rebuild.
    pub style_dirty: bool,
    /// The children stack vertically (a column): the widgets after a band
    /// disclosing move with its edge ([`Widget::disclosing`]).
    pub vertical: bool,
    /// The gap between children the layout was built with.
    pub laid_gap: f64,
    /// The column takes its content's height: a band moving in it makes it
    /// grow by transform ([`super::Disclosing::Grows`]) instead of being
    /// clipped to its box.
    pub content_sized: bool,
    /// The cache holds children drawn shifted: drawn again at rest.
    drawn_shifted: bool,
    /// Bands its view stopped building, drawn leaving (a column only).
    ghosts: super::Ghosts,
    /// The motion the person allows, from the last theme seen.
    motion: crate::tokens::EffectiveMotion,
    /// A column whose children's boxes stand in order down it, as the last
    /// layout placed them, each starting no higher than the one before
    /// ends: the children near a clip are then found by bisection.
    stacked: bool,
    /// How far past its box any child painted as the last layout placed
    /// them ([`Widget::paint_bounds`]): the bisection reaches that far. A
    /// child placed again since, or drawn, that paints further widens it.
    reach: f64,
    /// Where each child stands down a stacked column, from the column's top,
    /// as the last layout that solved them placed it: the bisection reads
    /// these, so it holds while children far from the view are not placed
    /// again ([`super::placing_again`]).
    extents: Vec<(f64, f64)>,
    /// The children placed where the column stands now, when the last
    /// placement placed only those near the view (`place_again`): those
    /// far from it keep the boxes an earlier placement gave them until they
    /// come near. `None` when every child is placed.
    placed: Option<std::ops::Range<usize>>,
    /// The children near the view at the last placement: those a later
    /// placement that only moves the column places once more, as they
    /// leave the view.
    near_placed: std::ops::Range<usize>,
    /// Whether a child was disclosing (a band on its way) when the layout
    /// last solved them: a band starts and comes to rest with a layout.
    disclosing_any: bool,
}

/// The parts of a [`Theme`] a cached subtree's pixels depend on beyond its
/// layout: every colour token, the radii, and whether floating surfaces are
/// see-through.
#[derive(Debug, PartialEq)]
struct DrawnTheme {
    colors: crate::tokens::Colors,
    radii: crate::tokens::Radii,
    reduce_transparency: bool,
}

impl DrawnTheme {
    fn of(theme: &Theme) -> Self {
        Self {
            colors: theme.tokens.colors.clone(),
            radii: theme.tokens.radii.clone(),
            reduce_transparency: theme.reduce_transparency,
        }
    }

    fn is(&self, theme: &Theme) -> bool {
        self.colors == theme.tokens.colors
            && self.radii == theme.tokens.radii
            && self.reduce_transparency == theme.reduce_transparency
    }
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
            cache_theme: None,
            cache_present: false,
            descendant_caches_released: false,
            cache_carries_material: false,
            cache_parts: None,
            cache_shows_secret: false,
            style_dirty: false,
            vertical: false,
            laid_gap: 0.0,
            content_sized: false,
            drawn_shifted: false,
            ghosts: super::Ghosts::default(),
            motion: crate::tokens::EffectiveMotion::default(),
            stacked: false,
            reach: 0.0,
            extents: Vec::new(),
            placed: None,
            near_placed: 0..0,
            disclosing_any: true,
        }
    }

    /// The widgets after each child are drawn this much lower while a band
    /// before them moves, then how much taller the column is drawn; `None`
    /// at rest and in a row.
    pub fn follower_shifts(&self) -> Option<Vec<f64>> {
        if !self.vertical || (!self.disclosing_any && self.ghosts.is_empty()) {
            return None;
        }
        let gap = self.laid_gap;
        let shifts = super::follower_shifts(self.children.iter().map(|child| &**child), |_| gap);
        // A band leaving moves what followed it up with its edge.
        self.ghosts.shift(shifts, self.children.len())
    }

    /// Whether a band its view stopped building is still leaving.
    pub fn has_ghosts(&self) -> bool {
        !self.ghosts.is_empty()
    }

    /// Leave the bands still leaving for the column a rebuild makes.
    fn keep_ghosts(&mut self) {
        self.ghosts.keep();
    }

    /// While a band moves in a content-sized column, how much taller the
    /// column is drawn than laid out.
    pub fn disclosing(&self) -> Option<super::Disclosing> {
        if !self.content_sized {
            return None;
        }
        let shifts = self.follower_shifts()?;
        Some(super::Disclosing::Grows(
            shifts.last().copied().unwrap_or(0.0),
        ))
    }

    /// Redraw the subtree on the next frame instead of replaying the cache.
    pub fn invalidate(&mut self) {
        self.cache_present = false;
    }

    pub fn take_children(&mut self) -> Vec<Box<dyn Widget>> {
        self.keep_ghosts();
        self.child_nodes.clear();
        self.invalidate();
        std::mem::take(&mut self.children)
    }

    pub fn set_children(&mut self, children: Vec<Box<dyn Widget>>) {
        self.keep_ghosts();
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
        self.motion = theme.motion();
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
        #[cfg(test)]
        let _probe = crate::profiling::frame_probe::span("flow.apply");
        (self.rect, self.paint_rect) = super::layout::flow_rects(taffy, node, origin);
        if self.place_again(taffy) {
            return;
        }
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        for (child_widget, child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child_widget.apply_layout(taffy, *child_node, new_origin);
        }
        if self.vertical {
            // A band on its way stands out of the layout, in its slot.
            let gap = self.laid_gap;
            let content = super::content_box(taffy, node, self.rect);
            super::place_bands(&mut self.children, content, |_| gap);
            // A band its view stopped building leaves where it stood.
            self.ghosts
                .find("Column", &mut self.children, content, |_| gap, self.motion);
        }
        self.disclosing_any = self
            .children
            .iter()
            .any(|child| child.disclosing().is_some());
        self.stacked = self.vertical
            && self.children.windows(2).all(|pair| match pair {
                [above, below] => above.get_rect().y1 <= below.get_rect().y0,
                _ => true,
            });
        let top = self.rect.y0;
        self.extents.clear();
        self.reach = 0.0;
        if self.stacked {
            self.extents.extend(self.children.iter().map(|child| {
                let rect = child.get_rect();
                (rect.y0 - top, rect.y1 - top)
            }));
            self.reach = self
                .children
                .iter()
                .map(|child| paint_reach(child.as_ref()))
                .fold(0.0, f64::max);
        }
        self.placed = None;
        self.near_placed = self.children_near(super::layout_clip());
    }

    /// Place again only the children near the view, when the layout they
    /// were last placed from is unchanged ([`super::placing_again`]) and
    /// only the column moved: a scroll, a glide. The children that were
    /// near the view before are placed once more, so the copy and the
    /// pointer find them where they now stand, and so is any child asking
    /// for a layout of its own (a row whose paint grew). Returns whether it
    /// did; otherwise every child is placed.
    fn place_again(&mut self, taffy: &TaffyTree<()>) -> bool {
        if !super::placing_again(taffy)
            || !self.stacked
            || self.disclosing_any
            || !self.ghosts.is_empty()
            || self.extents.len() != self.children.len()
            || self.child_nodes.len() != self.children.len()
        {
            return false;
        }
        let clip = super::layout_clip();
        let mut near = self.children_near(clip);
        let before = std::mem::replace(&mut self.near_placed, 0..0);
        // A child far from the view that asks for a layout is placed too:
        // what it paints now may reach the view.
        let asking: Vec<usize> = (0..self.children.len())
            .filter(|index| !near.contains(index) && !before.contains(index))
            .filter(|&index| {
                self.children
                    .get(index)
                    .is_some_and(|child| child.is_layout_dirty())
            })
            .collect();
        let origin = Point::new(self.rect.x0, self.rect.y0);
        let mut kept = true;
        let mut placed: Vec<usize> = near
            .clone()
            .chain(before.filter(|index| !near.contains(index)))
            .chain(asking)
            .collect();
        loop {
            for &index in &placed {
                kept &= self.place_child(taffy, index, origin);
            }
            // A child placed again that paints further than any did widens
            // what is near the view: those it brings in are placed too.
            let wider = self.children_near(clip);
            let more: Vec<usize> = wider
                .clone()
                .filter(|index| !near.contains(index))
                .collect();
            near = wider;
            if more.is_empty() || !kept {
                break;
            }
            placed = more;
        }
        if !kept {
            self.placed = None;
            return false;
        }
        self.placed = Some(near.clone());
        self.near_placed = near;
        true
    }

    /// Place the child at `index` again at `origin` from the unchanged
    /// layout. Returns whether it stands where it stood in the column: one
    /// that moved within it was laid out anew by itself.
    fn place_child(&mut self, taffy: &TaffyTree<()>, index: usize, origin: Point) -> bool {
        let (Some(child), Some(&node), Some(extent)) = (
            self.children.get_mut(index),
            self.child_nodes.get(index),
            self.extents.get(index),
        ) else {
            return true;
        };
        child.apply_layout(taffy, node, origin);
        let rect = child.get_rect();
        let (above, below) = (rect.y0 - origin.y, rect.y1 - origin.y);
        self.reach = self.reach.max(paint_reach(child.as_ref()));
        (above - extent.0).abs() <= SAME_PLACE && (below - extent.1).abs() <= SAME_PLACE
    }

    /// The children that may paint inside `clip`. In a stacked column
    /// those whose boxes, reaching as far as any child painted past its own
    /// at the last layout, meet it, found by bisection; with a band moving
    /// or the children out of order, every one.
    pub fn children_near(&self, clip: Option<Rect>) -> std::ops::Range<usize> {
        let all = 0..self.children.len();
        let Some(clip) = clip else {
            return all;
        };
        if !self.stacked
            || self.extents.len() != self.children.len()
            || self.follower_shifts().is_some()
        {
            return all;
        }
        let top = self.rect.y0;
        let start = self
            .extents
            .partition_point(|&(_, bottom)| top + bottom + self.reach <= clip.y0);
        let end = self
            .extents
            .partition_point(|&(above, _)| top + above - self.reach < clip.y1);
        start..end.max(start)
    }

    /// The children [`Self::children_near`] `clip` that stand where the
    /// column stands now: those a pointer and the copy may meet.
    pub fn children_placed_near(&self, clip: Option<Rect>) -> std::ops::Range<usize> {
        let near = self.children_near(clip);
        let Some(placed) = &self.placed else {
            return near;
        };
        let start = near.start.max(placed.start);
        start..near.end.min(placed.end).max(start)
    }

    /// The children `event` may reach: every one, but for an event at a
    /// place (a pointer, a touch) while only the children near the view
    /// stand where the column does ([`Self::children_placed_near`]), those,
    /// unless one far from it holds the pointer.
    pub fn children_reached(&self, event: &winit::event::WindowEvent) -> std::ops::Range<usize> {
        let all = 0..self.children.len();
        let Some(placed) = &self.placed else {
            return all;
        };
        if !super::layout::at_a_place(event)
            || self
                .children
                .iter()
                .any(|child| child.wants_pointer_capture() || child.modal_capture().is_some())
        {
            return all;
        }
        placed.start.min(all.end)..placed.end.min(all.end)
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
        if self.draw_near_view(scene, transform, text_renderer, theme, presence) {
            return true;
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
        let theme_changed = self
            .cache_theme
            .as_deref()
            .is_none_or(|drawn| !drawn.is(theme));
        let tracked = crate::scene_parts::is_tracked(scene);
        let needs_rebuild = !self.cache_present
            || size_changed
            || theme_changed
            || any_child_dirty
            || self.cache_carries_material
            || self.drawn_shifted
            || !self.ghosts.is_empty()
            || (tracked && self.cache_parts.is_none());

        // Drawn into a frame that leaves out what it keeps on screen, a
        // cache recorded now would hold every child the frame leaves out,
        // and recording it would cost what the frame saves: the children
        // draw themselves into the frame, and the cache is recorded again
        // once a frame draws whole (`crate::draw_cull`).
        if needs_rebuild
            && self.ghosts.is_empty()
            && self.follower_shifts().is_none()
            && crate::draw_cull::culls_into(scene)
        {
            self.cache_present = false;
            self.descendant_caches_released = false;
            self.drawn_shifted = false;
            for child in &mut self.children {
                super::draw_child(
                    child.as_mut(),
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
            return true;
        }
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
            // While a band moves, what follows it is drawn shifted by
            // transform, laid out once without the band, and clipped to a
            // column of its own height; a content-sized one grows instead.
            self.ghosts.retain_drawn();
            let shifts = self.follower_shifts();
            self.drawn_shifted = shifts.is_some();
            let ghosts = &self.ghosts;
            let clip = (shifts.is_some() && !self.content_sized).then_some(self.rect);
            let mut draw_children = |local: &mut Scene| {
                if let Some(clip) = clip {
                    local.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0,
                        Affine::IDENTITY,
                        &clip,
                    );
                }
                for (index, child) in self.children.iter_mut().enumerate() {
                    let dy = shifts
                        .as_deref()
                        .and_then(|shifts| shifts.get(index).copied())
                        .unwrap_or(0.0);
                    child.draw(
                        local,
                        Affine::translate((0.0, dy)),
                        text_renderer,
                        theme,
                        presence,
                    );
                }
                ghosts.draw(local, Affine::IDENTITY, shifts.as_deref());
                if clip.is_some() {
                    local.pop_layer();
                }
            };
            // A tracked section compares its parts frame to frame; a part
            // drawn into this cache would otherwise vanish from it.
            // The cache lands on screen through `transform`; a scroller
            // drawn into it offers copies in screen space.
            self.cache_parts = crate::scroll_copy::drawing_into_cache(transform, || {
                if tracked {
                    crate::scene_parts::record(&mut local, draw_children).1
                } else {
                    draw_children(&mut local);
                    None
                }
            });
            self.cache_shows_secret = secrets.finish();
            self.cache_carries_material = crate::scene_parts::append_with(
                scene,
                self.cache_parts.as_ref(),
                Some(transform),
                |scene| crate::backdrop::append_nested(scene, &local, transform),
            ) > 0;
            self.subtree_cache = local;
            self.cache_rect = self.rect;
            if theme_changed {
                self.cache_theme = Some(Box::new(DrawnTheme::of(theme)));
            }
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

    /// A stacked column the view shows only part of draws the children
    /// near the view, each as it draws itself, instead of a cache of all of
    /// them: a frame costs what is shown, not what the column holds. A
    /// child whose pixels the frame keeps where they were, or moves by a
    /// copy, is not drawn at all ([`crate::draw_cull`]). Returns whether it
    /// drew so; a column shown whole, or with a band moving, draws through
    /// the cache.
    fn draw_near_view(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool {
        let Some(view) = presence.viewport() else {
            return false;
        };
        if !self.stacked
            || self.disclosing_any
            || !self.ghosts.is_empty()
            || self.extents.len() != self.children.len()
        {
            return false;
        }
        let near = self.children_placed_near(Some(view));
        if near.len() == self.children.len() {
            return false;
        }
        // The cache no longer holds what is on screen, and the children
        // keep their own.
        self.cache_present = false;
        self.descendant_caches_released = false;
        self.drawn_shifted = false;
        for child in self.children.get_mut(near).unwrap_or_default() {
            // What the frame keeps on screen, unchanged, is not drawn again.
            if !child.needs_redraw()
                && !child.needs_animation_frame()
                && crate::draw_cull::kept(scene, transform, child.paint_bounds())
            {
                continue;
            }
            super::draw_child(
                child.as_mut(),
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
            // What it paints now, should that have grown since its layout.
            self.reach = self.reach.max(paint_reach(child.as_ref()));
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

/// How far, in logical pixels, a child placed again may stand from where
/// it stood in the column and still count as unmoved: the column's origin
/// is added and taken away again, so only rounding separates the two.
const SAME_PLACE: f64 = 1e-6;

/// How far past its box `child` paints, up or down.
fn paint_reach(child: &dyn Widget) -> f64 {
    let (rect, paints) = (child.get_rect(), child.paint_bounds());
    (rect.y0 - paints.y0).max(paints.y1 - rect.y1).max(0.0)
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
            let shifts = self.flow.follower_shifts();
            let reached = self.flow.children_reached(event);
            if reached.len() == self.flow.children.len() {
                $crate::widget::route_to_shifted_children(
                    &mut self.flow.children,
                    &self.flow.child_nodes,
                    shifts.as_deref(),
                    event,
                    ctx,
                );
            } else if let (Some(children), Some(nodes)) = (
                self.flow.children.get_mut(reached.clone()),
                self.flow.child_nodes.get(reached),
            ) {
                // Only the children near the view stand where they are
                // drawn; none is moving (`Flow::children_reached`).
                $crate::widget::route_to_children(children, nodes, event, ctx);
            }
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
            self.flow.has_ghosts() || self.flow.children.iter().any(|c| c.needs_redraw())
        }

        /// Frames while a band its view stopped building leaves, and one
        /// more to draw it gone.
        fn needs_animation_frame(&self) -> bool {
            self.flow.has_ghosts()
                || $crate::widget::defaults::needs_animation_frame(&|visit| {
                    self.for_each_child(visit)
                })
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

        fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn $crate::widget::Widget)) {
            let near = self.flow.children_placed_near($crate::widget::layout_clip());
            for child in self.flow.children.get(near).unwrap_or_default() {
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

        /// A flow adds no structure of its own: a change a child can
        /// describe stays described.
        fn take_structure_change(&mut self) -> $crate::widget::StructureChange {
            let mut change = if std::mem::take(&mut self.flow.style_dirty) {
                $crate::widget::StructureChange::Rebuild
            } else {
                $crate::widget::StructureChange::None
            };
            for child in &mut self.flow.children {
                change = change.join(child.take_structure_change());
            }
            change
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
            let own = std::mem::take(&mut self.flow.style_dirty);
            self.flow
                .children
                .iter_mut()
                .map(|c| c.take_structure_dirty())
                .fold(own, |acc, d| acc || d)
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
