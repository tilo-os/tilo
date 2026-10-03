use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::any::Any;
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
    /// Stable graph / test identity when this row was built from a serialized
    /// UI node.
    id: Option<String>,
    rect: vello::kurbo::Rect,
    /// Overflow extent used for culling independently of an exact allocation.
    paint_rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When set, the row keeps all children on one line (no flex wrap) —
    /// the right choice for a toolbar of fixed-size buttons.
    no_wrap: bool,
    /// When set, the row grows to fill the remaining main-axis space of
    /// its parent (`flex_grow:1`) and stretches its children to its height —
    /// the right choice for a sidebar|content body row.
    fill_height: bool,
    /// Optional explicit insets for toolbars and sticky action rows.
    horizontal_padding: f32,
    vertical_padding: f32,
    /// See [`Row::with_gap`]. `None` is the theme's `gap_standard` on both
    /// axes (between children, and between wrapped lines).
    gap_override: Option<f32>,
    /// See [`Row::with_justify`]. `None` leaves taffy's start packing.
    justify: Option<taffy::JustifyContent>,
    /// See [`Row::with_align_items`]. `None` keeps the fill-height /
    /// toolbar default below.
    align_items_override: Option<taffy::AlignItems>,
    /// Align intrinsic-height children to the top edge. The default remains
    /// centred for toolbars and compact control rows.
    align_start: bool,
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
            id: None,
            rect: vello::kurbo::Rect::ZERO,
            paint_rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            no_wrap: false,
            fill_height: false,
            horizontal_padding: 0.0,
            vertical_padding: 0.0,
            gap_override: None,
            justify: None,
            align_items_override: None,
            align_start: false,
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

    /// Address this row by a stable id in inspection and graph reconcile.
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

    /// Apply layout flags from a serialized row node without rebuilding.
    pub fn apply_graph_props(&mut self, id: &str, no_wrap: bool, fill_remaining_height: bool) {
        self.id = Some(id.to_owned());
        self.no_wrap = no_wrap;
        self.fill_height = fill_remaining_height;
        self.cache_present = false;
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

    /// Inset row contents without introducing a second layout wrapper.
    #[must_use]
    pub fn with_padding(mut self, horizontal: f32, vertical: f32) -> Self {
        self.horizontal_padding = horizontal.max(0.0);
        self.vertical_padding = vertical.max(0.0);
        self
    }

    /// Override the gap between children (and between wrapped lines;
    /// default `theme.gap_standard`, `s3`). Pass a spacing token —
    /// `theme.tokens.spacing.s2` for a tight icon-and-label cluster, `s1`
    /// for a segmented pair — rather than a number. `0.0` for children that
    /// carry their own spacing.
    #[must_use]
    pub fn with_gap(mut self, px: f32) -> Self {
        self.gap_override = Some(px.max(0.0));
        self
    }

    /// Place children along the row: `SpaceBetween` for a title on the left
    /// and actions on the right, `End` for a trailing action group, `Center`
    /// for a centred pager. Without it children pack at the start, and the
    /// grow-to-fill [`super::Spacer`] remains the way to push one group away
    /// from another when the row also wraps.
    #[must_use]
    pub fn with_justify(mut self, justify: taffy::JustifyContent) -> Self {
        self.justify = Some(justify);
        self
    }

    /// Align children across the row (default: centred, or stretched for a
    /// [`Row::with_fill_remaining_height`] body row). `Start` keeps a
    /// single-line control level with the first line of a wrapping label
    /// beside it; `Baseline` lines text of different sizes up on one
    /// baseline.
    #[must_use]
    pub fn with_align_items(mut self, align: taffy::AlignItems) -> Self {
        self.align_items_override = Some(align);
        self
    }

    /// Top-align children with different intrinsic heights, as in a wrapping
    /// collection of cards.
    #[must_use]
    pub fn align_start(mut self) -> Self {
        self.align_start = true;
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
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        // This container owns no operation handler: offer each child once.
        crate::widget::start_in_children(self, op, doc, path)
    }

    /// Forward the per-frame update pass; every child runs, and a
    /// change in any of them is a change here. Not short-circuited —
    /// a child that skipped its pass would miss a frame of state.
    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
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
        yield_full_width_children(taffy, &self.child_nodes);

        let gap = self.gap_override.unwrap_or(theme.gap_standard);

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
                align_items: Some(self.align_items_override.unwrap_or(if self.fill_height {
                    taffy::AlignItems::Stretch
                } else if self.align_start {
                    taffy::AlignItems::FlexStart
                } else {
                    taffy::AlignItems::Center
                })),
                justify_content: self.justify,
                flex_grow: if self.fill_height { 1.0 } else { 0.0 },
                flex_shrink: 0.0, // Prevent vertical squashing
                gap: taffy::Size {
                    width: length(gap),
                    height: length(gap),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                // Keep optional toolbar padding inside the row's percentage
                // width instead of growing the parent's content bounds.
                box_sizing: taffy::BoxSizing::BorderBox,
                padding: super::layout::insets_xy(self.horizontal_padding, self.vertical_padding),
                ..Default::default()
            },
            &self.child_nodes,
        );
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
        // Same pointer-capture short-circuit as `Column::handle_event` —
        // a dragging child gets exclusive event routing so the drag
        // survives the cursor wandering through peer widgets.
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture())
            && let Some(&cnode) = self.child_nodes.get(idx)
            && let Some(child) = self.children.get_mut(idx)
        {
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

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn is_focused(&self) -> bool {
        false
    }

    // `.any` would short-circuit and skip `take_structure_dirty()` on later
    // children once one reports dirty, leaving their flags uncleared --
    // the non-short-circuiting `.fold` below is required, not incidental.
    #[allow(
        clippy::unnecessary_fold,
        reason = "The fold must visit every child to drain every structure-dirty flag; any would stop after the first dirty child."
    )]
    fn take_structure_dirty(&mut self) -> bool {
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

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
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

        if let Some(key) = crate::presence::capability::presence_key(self)
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

impl crate::undo::UndoPoints for Row {
    fn needs_commit(&self) -> bool {
        self.children()
            .iter()
            .any(|c| crate::undo::needs_commit(*c))
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            crate::undo::on_commit(child);
        }
    }
}

impl crate::presence::capability::PublishesPresence for Row {
    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }
}

impl crate::permissions::capability::PermissionGated for Row {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

/// Along a row's axis, children ask only for the room they need:
///
/// * A child that claims its parent's full width and refuses to shrink —
///   a nested `Row`, a fluid `Container` — keeps a `Column` from squashing
///   it vertically, but beside siblings in a row the same style takes the
///   whole row on top of them and overflows it, starving any spacer to
///   nothing. A nested row is as wide as its own children, and any other
///   such child gives back what its siblings need, down to its content's
///   width, as `Dropdown` and `SearchField` already do.
/// * A child that grows into the room its siblings leave and names the
///   least it can be (`Container::with_flex_grow().with_min_width(..)`)
///   needs no more than that: its content's width must not count towards
///   the row's own, or a row measured for its content (in a column with no
///   scrollable above it) is as wide as its longest label and overflows.
fn yield_full_width_children(taffy: &mut TaffyTree<()>, children: &[NodeId]) {
    for &child in children {
        let Ok(style) = taffy.style(child) else {
            continue;
        };
        let full_width = style.size.width == percent(1.0_f32) && style.flex_shrink == 0.0;
        let grows_from_its_least = style.flex_grow > 0.0
            && style.flex_basis == length(0.0_f32)
            && style.size.width == taffy::Dimension::auto()
            && style.min_size.width.into_option().is_some();
        if !full_width && !grows_from_its_least {
            continue;
        }
        let mut style = style.clone();
        if grows_from_its_least {
            style.size.width = style.min_size.width;
        } else {
            if style.display == taffy::Display::Flex
                && style.flex_direction == taffy::FlexDirection::Row
                && style.flex_grow == 0.0
            {
                style.size.width = taffy::Dimension::auto();
            }
            style.flex_shrink = 1.0;
        }
        let _ = taffy.set_style(child, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Text;

    /// Same five-pass layout the `Column` tests use, with the real text
    /// renderer measuring the `Text` children.
    fn lay_out(root: &mut dyn Widget, width: f32) -> Vec<vello::kurbo::Rect> {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let mut tr = TextRenderer::new();
        let node = root.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = nodes.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        root.apply_layout(&taffy, node, Point::ZERO);
        root.children().iter().map(|c| c.get_rect()).collect()
    }

    fn two_texts() -> Row {
        Row::new()
            .no_wrap()
            .push(Box::new(Text::new("lead")))
            .push(Box::new(Text::new("trail")))
    }

    /// A label, a spacer and a full-width child (a nested row of buttons,
    /// a fluid container) stay inside the row instead of overflowing it.
    #[test]
    fn a_full_width_child_beside_a_spacer_stays_inside_the_row() {
        use crate::widget::{Button, Container, Spacer};
        let buttons = || {
            Row::new()
                .no_wrap()
                .with_gap(4.0)
                .push(Box::new(Button::new("a", "Alpha", None)))
                .push(Box::new(Button::new("b", "Beta", None)))
        };
        let nested = Row::new()
            .no_wrap()
            .push(Box::new(Text::new("Label")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(buttons()));
        let fluid = Row::new()
            .no_wrap()
            .push(Box::new(Text::new("Label")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_child(Box::new(buttons())),
            ));
        for mut row in [nested, fluid] {
            let rects = lay_out(&mut row, 400.0);
            let right = rects.iter().map(|rect| rect.x1).fold(0.0, f64::max);
            assert!(right <= 400.5, "the row overflows its 400 px: {rects:?}");
            assert!(
                rects[2].width() >= 100.0,
                "the trailing child keeps its content's width: {rects:?}"
            );
            assert!(
                rects[2].x1 > 390.0,
                "the spacer pushes the trailing child to the far end: {rects:?}"
            );
            assert!(
                rects[0].width() > 20.0,
                "the label keeps its text: {rects:?}"
            );
        }
    }

    /// A child growing into the leftover room from the least it names
    /// does not make a row measured for its content (no scrollable above
    /// it) as wide as that child's longest label.
    #[test]
    fn a_growing_child_does_not_widen_a_row_measured_for_its_content() {
        use crate::widget::{Button, Column, Container};
        let label = "A long layer name that would not fit in a narrow panel at all";
        let row = Row::new()
            .no_wrap()
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(Text::new(label))),
            ))
            .push(Box::new(Button::new("eye", "Eye", None)));
        let mut panel = Container::new()
            .with_no_padding()
            .with_width(240.0)
            .with_child(Box::new(Column::new().with_no_padding().push(Box::new(
                Column::new().with_intrinsic_height().push(Box::new(row)),
            ))));
        lay_out(&mut panel, 600.0);
        let mut widest: f64 = 0.0;
        let mut stack: Vec<&dyn Widget> = vec![&panel];
        while let Some(widget) = stack.pop() {
            widest = widest.max(widget.get_rect().x1);
            stack.extend(widget.children());
        }
        assert!(
            widest <= 240.5,
            "the row overflows its 240 px panel: {widest}"
        );
    }

    #[test]
    fn row_gap_justify_and_alignment_place_children() {
        // Case `default_row_gap_is_the_theme_gap_standard_and_children_pack_at_start`.
        {
            let theme = Theme::dark();
            let rects = lay_out(&mut two_texts(), 600.0);
            assert!(
                (rects[1].x0 - rects[0].x1 - f64::from(theme.gap_standard)).abs() < 0.5,
                "default gap is gap_standard: {rects:?}"
            );
            assert!(
                (rects[0].x0).abs() < 0.5,
                "children start at the leading edge: {rects:?}"
            );
        }

        // Case `with_gap_overrides_the_gap_between_children`.
        {
            let rects = lay_out(&mut two_texts().with_gap(4.0), 600.0);
            assert!((rects[1].x0 - rects[0].x1 - 4.0).abs() < 0.5, "{rects:?}");
            let tight = lay_out(&mut two_texts().with_gap(0.0), 600.0);
            assert!((tight[1].x0 - tight[0].x1).abs() < 0.5, "{tight:?}");
        }

        // Case `with_justify_space_between_pushes_the_last_child_to_the_far_edge`.
        {
            let rects = lay_out(
                &mut two_texts().with_justify(taffy::JustifyContent::SpaceBetween),
                600.0,
            );
            assert!((rects[0].x0).abs() < 0.5, "{rects:?}");
            assert!((rects[1].x1 - 600.0).abs() < 0.5, "{rects:?}");
            let end = lay_out(
                &mut two_texts().with_justify(taffy::JustifyContent::End),
                600.0,
            );
            assert!((end[1].x1 - 600.0).abs() < 0.5, "{end:?}");
            assert!(
                end[0].x0 > 300.0,
                "both children sit at the trailing end: {end:?}"
            );
        }

        // Case `with_align_items_start_keeps_a_short_child_at_the_top_of_a_tall_row`.
        {
            let tall = Box::new(
                crate::widget::Container::new()
                    .with_no_padding()
                    .with_size(40.0, 80.0),
            );
            let centred = lay_out(
                &mut Row::new()
                    .no_wrap()
                    .push(Box::new(Text::new("x")))
                    .push(tall),
                600.0,
            );
            let text_mid = centred[0].y0.midpoint(centred[0].y1);
            let tall_mid = centred[1].y0.midpoint(centred[1].y1);
            assert!(
                (text_mid - tall_mid).abs() < 1.0,
                "default centres the short child: {centred:?}"
            );

            let tall = Box::new(
                crate::widget::Container::new()
                    .with_no_padding()
                    .with_size(40.0, 80.0),
            );
            let start = lay_out(
                &mut Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Start)
                    .push(Box::new(Text::new("x")))
                    .push(tall),
                600.0,
            );
            assert!(
                (start[0].y0 - start[1].y0).abs() < 0.5,
                "Start aligns the short child's top with the tall child's: {start:?}"
            );
        }
    }

    #[test]
    fn damage_merge_coalesces_near_children_but_keeps_wrap_endpoints() {
        // Case `damage_merge_keeps_progress_wrap_endpoints_separate`.
        {
            // These are the clipped old/new segments at opposite ends of a
            // 200px indeterminate progress bar. Their union would repaint the
            // entire track even though each changed strip is only 60px wide.
            let old_segment = vello::kurbo::Rect::new(140.0, 0.0, 200.0, 8.0);
            let new_segment = vello::kurbo::Rect::new(0.0, 0.0, 60.0, 8.0);

            assert!(!can_merge_damage(old_segment, new_segment));
        }

        // Case `damage_merge_coalesces_nearby_row_children`.
        {
            let left = vello::kurbo::Rect::new(0.0, 0.0, 40.0, 20.0);
            let right = vello::kurbo::Rect::new(44.0, 0.0, 84.0, 20.0);

            assert!(can_merge_damage(left, right));
        }
    }
}
