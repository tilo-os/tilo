//! Explicit logical rectangles for ordinary retained children.
use super::composition_parts::{Parts, retained_parts};
use super::layout_info::ManualLayout;
use super::{Element, LayoutInfo, Widget, WidgetLayoutError};
use crate::{PresenceCtx, Theme, bound_dispatch::BoundNodeMap, text_render::TextRenderer};
use std::{cell::Cell, collections::HashMap, rc::Rc};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// A proposal for one axis of a retained subtree, in logical pixels.
#[derive(Clone, Copy, Debug)]
pub enum MeasureConstraint {
    /// Natural size; percentage/fill on the subtree root becomes intrinsic.
    Intrinsic,
    /// Fit within this bound, allowing smaller natural content.
    AtMost(f32),
    /// Allocate precisely this size, including padding and borders.
    Exact(f32),
}
/// Allocated size and overflowing content are separate measurements.
#[derive(Clone, Copy, Debug)]
pub struct ChildMeasurement {
    pub size: Size<f32>,
    pub content_size: Size<f32>,
}

/// Explicit invalidation for inputs captured by a retained [`Manual`] policy.
/// After changing an input, call [`Self::invalidate`] and schedule a normal
/// host layout frame. This handle does not schedule a window redraw itself.
#[derive(Clone, Default)]
pub struct ManualInvalidation(Rc<PolicyRevision>);
#[derive(Default)]
struct PolicyRevision {
    requested: Cell<u64>,
    solved: Cell<Option<u64>>,
    applied: Cell<Option<u64>>,
}
impl ManualInvalidation {
    /// Request another solve without replacing the region or its children.
    pub fn invalidate(&self) {
        self.0.requested.set(self.0.requested.get().wrapping_add(1));
    }
    pub(crate) fn is_pending(&self) -> bool {
        self.0.applied.get() != Some(self.0.requested.get())
    }
    /// Acknowledge the solved revision once its geometry has been applied
    /// (what [`Manual`] does in `apply_layout`), for another widget that
    /// places its children through a [`ManualPolicy`].
    pub(crate) fn acknowledge_applied(&self) {
        self.0.applied.set(self.0.solved.get());
    }
}

pub(crate) type Policy = dyn Fn(&mut ManualContext<'_>) -> Result<Size<f32>, WidgetLayoutError>;
/// An owned descriptor. Hosts consume this through admitted Widget dispatch.
#[doc(hidden)]
#[derive(Clone)]
pub struct ManualPolicy {
    policy: Rc<Policy>,
    /// The size the region was built with. The policy's answer can replace
    /// it in the region's style; a new solve starts again from it on the axes
    /// the region takes from its parent ([`reset_region`]).
    region: Size<Dimension>,
    children: Vec<(NodeId, Style)>,
    info: Rc<Cell<Option<ManualLayout>>>,
    invalidation: ManualInvalidation,
}

impl ManualPolicy {
    /// A policy for a toolkit widget that places its own retained children
    /// exactly (`NavShell`): `region` is the size the region's own node was
    /// built with, `children` the child nodes it built, in placement-index
    /// order, with the styles they were built with.
    pub(crate) fn for_children(
        policy: Rc<Policy>,
        region: Size<Dimension>,
        children: Vec<(NodeId, Style)>,
        invalidation: ManualInvalidation,
    ) -> Self {
        Self {
            policy,
            region,
            children,
            info: Rc::new(Cell::new(None)),
            invalidation,
        }
    }
}

/// A manual region whose children keep their standard rendering and interaction.
/// The policy measures children, places every child, and returns its desired size.
/// It can run repeatedly before painting and must not change child topology.
/// Rectangles are relative to this region, represented by Taffy's logical f32.
pub struct Manual {
    parts: Parts,
    policy: Rc<Policy>,
    region: Size<Dimension>,
    baseline: Vec<(NodeId, Style)>,
    info: Rc<Cell<Option<ManualLayout>>>,
    invalidation: ManualInvalidation,
}
impl Manual {
    pub fn new(
        policy: impl Fn(&mut ManualContext<'_>) -> Result<Size<f32>, WidgetLayoutError> + 'static,
    ) -> Self {
        Self {
            parts: Parts::default(),
            policy: Rc::new(policy),
            region: Self::region_size(),
            baseline: Vec::new(),
            info: Rc::new(Cell::new(None)),
            invalidation: ManualInvalidation::default(),
        }
    }
    #[must_use]
    pub fn child(mut self, child: Element) -> Self {
        self.parts.children.push(child);
        self
    }
    /// Retain this handle alongside captured policy inputs. Invalidate it
    /// after changing those inputs, then request the host's next layout frame.
    pub fn invalidation(&self) -> ManualInvalidation {
        self.invalidation.clone()
    }
    pub fn effective_layout(&self) -> Option<ManualLayout> {
        self.info.get()
    }
    /// The region fills its parent's width; its height is the policy's.
    fn region_size() -> Size<Dimension> {
        Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        }
    }
}
impl Widget for Manual {
    retained_parts!("Manual", custom_apply_layout);
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.parts.apply_layout(tree, node, origin);
        // Probes may solve policies, but only actual placement acknowledges
        // their revision. Failed solves never reach this point.
        self.invalidation
            .0
            .applied
            .set(self.invalidation.0.solved.get());
    }
    fn layout_needs_measure(&self) -> bool {
        self.invalidation.is_pending()
            || self
                .parts
                .children
                .iter()
                .any(|child| child.layout_needs_measure())
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn layout_info(&self) -> Option<LayoutInfo> {
        self.effective_layout().map(LayoutInfo::Manual)
    }
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        Some(ManualPolicy {
            policy: self.policy.clone(),
            region: self.region,
            children: self.baseline.clone(),
            info: self.info.clone(),
            invalidation: self.invalidation.clone(),
        })
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.parts.build(
            Style {
                size: self.region,
                ..Style::default()
            },
            tree,
            theme,
            map,
        );
        self.baseline = self
            .parts
            .nodes
            .iter()
            .map(|&child| (child, tree.style(child).cloned().unwrap_or_default()))
            .collect();
        self.info.set(None);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }

    /// Authored allocations may deliberately be smaller than their children,
    /// so negative as well as positive child extents count.
    fn paint_bounds(&self) -> Rect {
        self.info.get().map_or(self.parts.rect, |info| {
            let [x, y, width, height] = info.content_bounds;
            self.parts.rect.union(Rect::from_origin_size(
                (
                    self.parts.rect.x0 + f64::from(x),
                    self.parts.rect.y0 + f64::from(y),
                ),
                (f64::from(width), f64::from(height)),
            ))
        })
    }
}

/// Safe child-indexed measurement/placement services supplied by the host.
/// Measurements solve current retained topology; they never apply geometry or
/// rebuild widgets. Actual placement participates in the normal host settlement.
pub struct ManualContext<'a> {
    tree: &'a TaffyTree<()>,
    nodes: &'a HashMap<NodeId, *mut dyn Widget>,
    bindings: &'a BoundNodeMap,
    text: &'a mut TextRenderer,
    children: &'a [(NodeId, Style)],
    available: Size<f32>,
    placements: Vec<Option<Rect>>,
    depth: usize,
}
impl ManualContext<'_> {
    pub fn available(&self) -> Size<f32> {
        self.available
    }
    pub fn len(&self) -> usize {
        self.children.len()
    }
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
    /// Allocate one child in local logical coordinates.
    /// # Errors
    /// Rejects out-of-range indices, duplicate placement and invalid geometry.
    pub fn place(&mut self, index: usize, rect: Rect) -> Result<(), WidgetLayoutError> {
        if ![
            rect.x0,
            rect.y0,
            rect.x1,
            rect.y1,
            rect.width(),
            rect.height(),
        ]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= f64::from(f32::MAX))
            || rect.width() < 0.0
            || rect.height() < 0.0
        {
            return Err(WidgetLayoutError::InvalidManualGeometry);
        }
        let placement = self
            .placements
            .get_mut(index)
            .ok_or(WidgetLayoutError::ManualChildIndex(index))?;
        if placement.is_some() {
            return Err(WidgetLayoutError::DuplicateManualPlacement(index));
        }
        *placement = Some(rect);
        Ok(())
    }
    /// Measure current retained topology without applying hypothetical geometry.
    /// # Errors
    /// Rejects invalid indices/proposals and propagates bounded layout errors.
    pub fn measure(
        &mut self,
        index: usize,
        proposal: Size<MeasureConstraint>,
    ) -> Result<ChildMeasurement, WidgetLayoutError> {
        self.measure_child(index, proposal)
    }

    /// Measure a retained child using layout constraints, without changing geometry.
    /// # Errors
    /// Rejects invalid indices/proposals and propagates bounded layout errors.
    pub fn measure_child(
        &mut self,
        index: usize,
        proposal: Size<MeasureConstraint>,
    ) -> Result<ChildMeasurement, WidgetLayoutError> {
        let (root, baseline) = self
            .children
            .get(index)
            .ok_or(WidgetLayoutError::ManualChildIndex(index))?;
        let mut probe = self.tree.clone();
        let mut style = baseline.clone();
        style.position = taffy::Position::Relative;
        style.inset = taffy::Rect::auto();
        style.margin = taffy::Rect::zero();
        let width = propose(
            proposal.width,
            &mut style.size.width,
            &mut style.min_size.width,
            &mut style.max_size.width,
        )?;
        let height = propose(
            proposal.height,
            &mut style.size.height,
            &mut style.min_size.height,
            &mut style.max_size.height,
        )?;
        probe.set_style(*root, style)?;
        let mut subtree = HashMap::new();
        let mut pending = vec![*root];
        while let Some(node) = pending.pop() {
            if let Some(&pointer) = self.nodes.get(&node) {
                subtree.insert(node, pointer);
            }
            pending.extend(probe.children(node)?);
        }
        // SAFETY: context is created only by the admitted host solver, with no
        // ancestor widget borrow alive. Reuse ORIGINAL admission, never rebuild
        // it from the filtered raw map. The cloned Taffy retains matching IDs.
        unsafe {
            super::layout::compute_widget_layout_depth(
                &mut probe,
                *root,
                Size { width, height },
                &subtree,
                self.bindings,
                self.text,
                self.depth.saturating_add(1),
            )?;
        }
        let layout = probe.layout(*root)?;
        Ok(ChildMeasurement {
            size: layout.size,
            content_size: layout.content_size,
        })
    }
}
fn propose(
    value: MeasureConstraint,
    size: &mut Dimension,
    min: &mut Dimension,
    max: &mut Dimension,
) -> Result<AvailableSpace, WidgetLayoutError> {
    match value {
        MeasureConstraint::Intrinsic => {
            if size.into_option().is_none() {
                *size = Dimension::auto();
            }
            Ok(AvailableSpace::MaxContent)
        }
        MeasureConstraint::AtMost(bound) | MeasureConstraint::Exact(bound) => {
            if !bound.is_finite() || bound < 0.0 {
                return Err(WidgetLayoutError::InvalidManualGeometry);
            }
            *max = Dimension::length(bound);
            if matches!(value, MeasureConstraint::Exact(_)) {
                *size = Dimension::length(bound);
                *min = *size;
            } else {
                *size = Dimension::auto();
                *min = Dimension::auto();
            }
            Ok(AvailableSpace::Definite(bound))
        }
    }
}

/// Give a region back the size its parent allocates it before a new solve.
///
/// On an axis the region was built to take from its parent (a length or a
/// percentage), a policy that answers a size of its own leaves that size in
/// the region's style. The next solve would then hand the policy its own
/// answer as the allocation, and a retained tree could never grow the region
/// with a wider parent. Only a region whose allocation can have changed (it,
/// or an ancestor, is dirty) is reset; a clean one keeps its cached solve. An
/// axis the region sizes itself (`auto`) keeps the policy's answer.
pub(super) fn reset_region(
    descriptor: &ManualPolicy,
    tree: &mut TaffyTree<()>,
    node: NodeId,
) -> Result<(), taffy::TaffyError> {
    let mut style = tree.style(node)?.clone();
    let authored = descriptor.region;
    let reset = |authored: Dimension, current: &mut Dimension| {
        if !authored.is_auto() {
            *current = authored;
        }
    };
    reset(authored.width, &mut style.size.width);
    reset(authored.height, &mut style.size.height);
    if style.size == tree.style(node)?.size {
        return Ok(());
    }
    let mut current = Some(node);
    while let Some(id) = current {
        if tree.dirty(id)? {
            set_style(tree, node, style)?;
            return Ok(());
        }
        current = tree.parent(id);
    }
    Ok(())
}

/// The region's size on one axis after its policy answered `answer` for an
/// `allocated` extent, with `current` the size in its style now. An axis that
/// takes its size from the parent keeps doing so while the policy accepts
/// the allocation, so a resize re-solves it without a pass to undo a fixed
/// size. A size the policy chose and still answers stays put, or the region
/// would flip between it and the parent's allocation.
#[allow(
    clippy::float_cmp,
    reason = "exact f32 logical allocations must match, not merely fall within a tolerance"
)]
fn region_axis(authored: Dimension, current: Dimension, answer: f32, allocated: f32) -> Dimension {
    let chosen = Dimension::length(answer);
    if current == chosen {
        current
    } else if !authored.is_auto() && answer == allocated {
        authored
    } else {
        chosen
    }
}

/// Called with no widget references alive and with the owner's execution lease.
#[allow(
    clippy::float_cmp,
    reason = "exact f32 logical allocations must match, not merely fall within a tolerance"
)]
pub(super) unsafe fn run_policy(
    descriptor: &ManualPolicy,
    tree: &mut TaffyTree<()>,
    node: NodeId,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    text: &mut TextRenderer,
    depth: usize,
) -> Result<bool, WidgetLayoutError> {
    let revision = descriptor.invalidation.0.requested.get();
    let layout = *tree.layout(node)?;
    let mut context = ManualContext {
        tree,
        nodes,
        bindings,
        text,
        children: &descriptor.children,
        available: layout.size,
        placements: vec![None; descriptor.children.len()],
        depth,
    };
    let size = (descriptor.policy)(&mut context)?;
    if !size.width.is_finite() || !size.height.is_finite() || size.width < 0.0 || size.height < 0.0
    {
        return Err(WidgetLayoutError::InvalidManualGeometry);
    }
    let placements = context.placements;
    let mut changed = false;
    let mut requested = Rect::ZERO;
    for ((child, _), rect) in descriptor.children.iter().zip(&placements) {
        let rect = rect.ok_or(WidgetLayoutError::UnplacedManualChild)?;
        requested = requested.union(rect);
        let [x, y, width, height] = rect_array(rect);
        let mut style = tree.style(*child)?.clone();
        style.position = taffy::Position::Absolute;
        style.inset = taffy::Rect {
            left: taffy::LengthPercentageAuto::length(x),
            top: taffy::LengthPercentageAuto::length(y),
            right: taffy::LengthPercentageAuto::auto(),
            bottom: taffy::LengthPercentageAuto::auto(),
        };
        style.margin = taffy::Rect::zero();
        style.min_size = Size {
            width: Dimension::length(width),
            height: Dimension::length(height),
        };
        style.max_size = style.min_size;
        // Size may still be maintained by a control's own fit-content hook.
        // Equal min/max give authored allocation priority without oscillation.
        changed |= set_style(tree, *child, style)?;
    }
    let mut style = tree.style(node)?.clone();
    style.size = Size {
        width: region_axis(
            descriptor.region.width,
            style.size.width,
            size.width,
            layout.size.width,
        ),
        height: region_axis(
            descriptor.region.height,
            style.size.height,
            size.height,
            layout.size.height,
        ),
    };
    changed |= set_style(tree, node, style)?;
    let mut overflow_bounds = Rect::ZERO;
    let mut conflict = None;
    for (index, ((child, _), requested)) in descriptor.children.iter().zip(placements).enumerate() {
        let layout = tree.layout(*child)?;
        let effective = Rect::from_origin_size(
            (f64::from(layout.location.x), f64::from(layout.location.y)),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        overflow_bounds = overflow_bounds.union(descendant_bounds(tree, *child)?);
        let requested = requested.ok_or(WidgetLayoutError::UnplacedManualChild)?;
        if rect_array(effective) != rect_array(requested) && conflict.is_none() {
            conflict = Some(super::layout_info::ManualConflict {
                child: index,
                requested: rect_array(requested),
                effective: rect_array(effective),
            });
        }
    }
    descriptor.info.set(Some(ManualLayout {
        allocation: [layout.size.width, layout.size.height],
        requested_bounds: rect_array(requested),
        content_bounds: rect_array(overflow_bounds),
        overflow: overflow_bounds.x0 < 0.0
            || overflow_bounds.y0 < 0.0
            || overflow_bounds.x1 > f64::from(layout.size.width)
            || overflow_bounds.y1 > f64::from(layout.size.height),
        first_conflict: conflict,
    }));
    descriptor.invalidation.0.solved.set(Some(revision));
    Ok(changed)
}
/// Conservative solved-layout paint bounds, relative to the root's parent.
/// Visit actual descendant origins: Taffy's content Size cannot carry negative
/// overflow. Anonymous layout nodes participate without any widget callbacks.
/// Taffy clipping bounds descendants; custom paint-only clips may be tighter.
fn descendant_bounds(tree: &TaffyTree<()>, root: NodeId) -> Result<Rect, taffy::TaffyError> {
    let unbounded = Rect::new(
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::INFINITY,
    );
    let mut pending = vec![(root, Point::ZERO, unbounded)];
    let mut extent = Rect::ZERO;
    while let Some((node, parent_origin, clip)) = pending.pop() {
        let layout = tree.layout(node)?;
        let origin = Point::new(
            parent_origin.x + f64::from(layout.location.x),
            parent_origin.y + f64::from(layout.location.y),
        );
        let allocation = Rect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        let children = tree.children(node)?;
        // Composite content is accounted for by the traversal, so clipped
        // descendants cannot leak back in through a container's aggregate size.
        let own_bounds = if children.is_empty() {
            Rect::from_origin_size(
                origin,
                (
                    f64::from(layout.size.width.max(layout.content_size.width)),
                    f64::from(layout.size.height.max(layout.content_size.height)),
                ),
            )
        } else {
            allocation
        };
        let visible = own_bounds.intersect(clip);
        if visible.width() > 0.0 && visible.height() > 0.0 {
            extent = extent.union(visible);
        }
        let overflow = tree.style(node)?.overflow;
        let mut child_clip = clip;
        if overflow.x != taffy::Overflow::Visible {
            child_clip.x0 = child_clip.x0.max(allocation.x0);
            child_clip.x1 = child_clip.x1.min(allocation.x1);
        }
        if overflow.y != taffy::Overflow::Visible {
            child_clip.y0 = child_clip.y0.max(allocation.y0);
            child_clip.y1 = child_clip.y1.min(allocation.y1);
        }
        if child_clip.width() > 0.0 && child_clip.height() > 0.0 {
            pending.extend(
                children
                    .into_iter()
                    .map(|child| (child, origin, child_clip)),
            );
        }
    }
    Ok(extent)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "Taffy uses f32 logical geometry; author values are range-checked before conversion"
)]
fn rect_array(rect: Rect) -> [f32; 4] {
    [
        rect.x0 as f32,
        rect.y0 as f32,
        rect.width() as f32,
        rect.height() as f32,
    ]
}
fn set_style(
    tree: &mut TaffyTree<()>,
    node: NodeId,
    style: Style,
) -> Result<bool, taffy::TaffyError> {
    if *tree.style(node)? == style {
        return Ok(false);
    }
    tree.set_style(node, style)?;
    Ok(true)
}
#[cfg(test)]
mod tests;

#[cfg(test)]
mod ownership_tests;
