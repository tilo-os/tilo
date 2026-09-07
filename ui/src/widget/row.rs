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

/// Keep the small savings from coalescing adjacent row children without
/// turning sparse animation damage into a broad horizontal strip. A merged
/// rectangle may cover at most 25% more pixels than its two inputs.
const MAX_DAMAGE_MERGE_OVERDRAW: f64 = 1.25;

fn can_merge_damage(a: vello::kurbo::Rect, b: vello::kurbo::Rect) -> bool {
    let covered_area = a.width() * a.height() + b.width() * b.height();
    covered_area > 0.0
        && a.union(b).width() * a.union(b).height() <= covered_area * MAX_DAMAGE_MERGE_OVERDRAW
}

/// `base` composed with a translation to `(dx, dy)`. Wrapped so the one
/// `arithmetic_side_effects` allow lives in a single place: `Affine`'s
/// `Mul` is pure floating-point matrix composition and cannot panic or
/// overflow.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent invalidation and cache flags model orthogonal retained-widget state"
)]
pub struct Row {
    children: Vec<Box<dyn Widget>>,
    /// Retained Taffy identities for `children`. Rebuilt only with the layout
    /// tree; hot event/layout traversal must not allocate `taffy.children()`.
    child_nodes: Vec<NodeId>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When set, the row keeps all children on one line (no flex wrap) —
    /// the right choice for a toolbar of fixed-size buttons.
    no_wrap: bool,
    /// When set, the row grows to fill the remaining main-axis space of
    /// its parent (`flex_grow:1`) and stretches its children to its height —
    /// the right choice for a sidebar|content body row.
    fill_height: bool,
    /// Subtree cache — same pattern as `Column::subtree_cache`.
    /// See that field for the gory rationale.
    subtree_cache: Scene,
    cache_rect: vello::kurbo::Rect,
    cache_present: bool,
    descendant_caches_released: bool,
}

impl Default for Row {
    fn default() -> Self {
        Self::new()
    }
}

impl Row {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            child_nodes: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            no_wrap: false,
            fill_height: false,
            subtree_cache: Scene::new(),
            cache_rect: vello::kurbo::Rect::ZERO,
            cache_present: false,
            descendant_caches_released: false,
        }
    }

    #[must_use]
    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn no_wrap(mut self) -> Self {
        self.no_wrap = true;
        self
    }

    #[must_use]
    pub fn with_fill_remaining_height(mut self) -> Self {
        self.fill_height = true;
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

impl Widget for Row {
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
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: if self.no_wrap {
                    taffy::FlexWrap::NoWrap
                } else {
                    taffy::FlexWrap::Wrap
                },
                // Stretch children to the row height when it is a fill-body
                // row (so the file list spans the available height);
                // otherwise centre them (toolbar/button rows).
                align_items: Some(if self.fill_height {
                    taffy::AlignItems::Stretch
                } else {
                    taffy::AlignItems::Center
                }),
                flex_grow: if self.fill_height { 1.0 } else { 0.0 },
                flex_shrink: 0.0, // Prevent vertical squashing
                gap: taffy::Size {
                    width: length(theme.gap_standard),
                    height: length(theme.gap_standard),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &self.child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        // See `Column::apply_layout` for why we max() against
        // content_size — covers taffy under-reporting under a
        // constrained-height parent.
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
        // Same pointer-capture short-circuit as `Column::handle_event` —
        // a dragging child gets exclusive event routing so the drag
        // survives the cursor wandering through peer widgets.
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture())
            && let Some(&cnode) = self.child_nodes.get(idx)
            && let Some(child) = self.children.get_mut(idx)
        {
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

    /// Propagate pointer-capture intent from any descendant — mirrors
    /// `Column::wants_pointer_capture`. See that comment for why.
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
        let first = regions.len();
        for child in &self.children {
            child.collect_damage(regions);
        }
        // Adjacent row children can share a pass, but unconditionally
        // unioning them turns the two narrow endpoints of an indeterminate
        // ProgressBar's wrap into a full-width strip. Coalesce only when the
        // added bounding-box area is tightly bounded; preserving sparse
        // regions is materially cheaper than one broad redraw at 4K.
        let mut merged = Vec::with_capacity(regions.len().saturating_sub(first));
        for candidate in regions.drain(first..) {
            if let Some(previous) = merged.last_mut()
                && can_merge_damage(*previous, candidate)
            {
                *previous = previous.union(candidate);
            } else {
                merged.push(candidate);
            }
        }
        regions.extend(merged);
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
        "Row"
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
        // Subtree cache — see `Column::draw_with_presence` for the
        // `Affine::IDENTITY` rebuild requirement.
        let size_changed = (self.rect.width() - self.cache_rect.width()).abs() > f64::EPSILON
            || (self.rect.height() - self.cache_rect.height()).abs() > f64::EPSILON;
        let any_child_dirty = self.children.iter().any(|c| c.needs_redraw());
        let needs_rebuild = !self.cache_present || size_changed || any_child_dirty;
        if needs_rebuild {
            // Drop the active viewport for the recursion — see
            // `Column::draw_with_presence` for why.
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
            scene.append(&self.subtree_cache, Some(translated(transform, dx, dy)));
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

    #[test]
    fn damage_merge_keeps_progress_wrap_endpoints_separate() {
        // These are the clipped old/new segments at opposite ends of a
        // 200px indeterminate progress bar. Their union would repaint the
        // entire track even though each changed strip is only 60px wide.
        let old_segment = vello::kurbo::Rect::new(140.0, 0.0, 200.0, 8.0);
        let new_segment = vello::kurbo::Rect::new(0.0, 0.0, 60.0, 8.0);

        assert!(!can_merge_damage(old_segment, new_segment));
    }

    #[test]
    fn damage_merge_coalesces_nearby_row_children() {
        let left = vello::kurbo::Rect::new(0.0, 0.0, 40.0, 20.0);
        let right = vello::kurbo::Rect::new(44.0, 0.0, 84.0, 20.0);

        assert!(can_merge_damage(left, right));
    }
}
