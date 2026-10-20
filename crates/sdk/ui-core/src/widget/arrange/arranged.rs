//! The one widget every layout primitive is: a container whose arrangement a [`Layout`]
//! decides, around child slots.
//!
//! Each child sits in a *wrapper* node whose style carries the child's [`Fit`] (fill weight,
//! bounds, aspect, span, place). The container style and the wrappers' styles come from the
//! layout, and are chosen again during layout from the width the container was given
//! ([`Widget::adjust_layout_style`]): constraints flow down, the container decides its
//! arrangement from what it was offered, and a child never reads its own final size.

use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::units::{At, Fit};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::{AppModel, Element, ModelPath, TaffyTreeExt, Widget};

/// What a layout reads of the theme: the `rem` and the rhythm gaps, in logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    pub rem: f32,
    gaps: [f32; 5],
}

impl Resolved {
    #[must_use]
    pub fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            rem: super::units::rem_of(theme),
            gaps: [
                rhythm.get(Rhythm::Tight),
                rhythm.get(Rhythm::Related),
                rhythm.get(Rhythm::Sibling),
                rhythm.get(Rhythm::Section),
                rhythm.get(Rhythm::Group),
            ],
        }
    }

    /// Fixed values, for tests of layout math.
    #[must_use]
    pub const fn fixed(rem: f32, gap: f32) -> Self {
        Self {
            rem,
            gaps: [gap; 5],
        }
    }

    /// The gap of `step` in logical pixels.
    #[must_use]
    pub const fn gap(&self, step: Rhythm) -> f32 {
        match step {
            Rhythm::Tight => self.gaps[0],
            Rhythm::Related => self.gaps[1],
            Rhythm::Sibling => self.gaps[2],
            Rhythm::Section => self.gaps[3],
            Rhythm::Group => self.gaps[4],
        }
    }
}

/// A child with how it is sized and placed.
pub struct Slot {
    pub(super) element: Element,
    pub fit: Fit,
    /// Columns a grid item spans.
    pub span: u16,
    /// Where a layer sits; `None` fills the cell.
    pub at: Option<At>,
    wrapper: Option<NodeId>,
}

impl Slot {
    /// The child, out of its slot.
    #[must_use]
    pub fn into_element(self) -> Element {
        self.element
    }

    #[must_use]
    pub fn new(element: Element) -> Self {
        Self {
            element,
            fit: Fit::hug(),
            span: 1,
            at: None,
            wrapper: None,
        }
    }
}

/// The base style of a child's wrapper: a column that stretches its one child.
#[must_use]
pub fn wrapper_style() -> Style {
    Style {
        display: taffy::Display::Flex,
        flex_direction: taffy::FlexDirection::Column,
        align_items: Some(taffy::AlignItems::Stretch),
        min_size: Size {
            width: taffy::style_helpers::length(0.0_f32),
            height: taffy::Dimension::auto(),
        },
        ..Style::default()
    }
}

/// How a primitive arranges its slots. See the module docs.
pub trait Layout {
    fn name(&self) -> &'static str;

    /// The container's style for the current arrangement.
    fn container(&self, resolved: &Resolved) -> Style;

    /// The wrapper style of slot `index` for the current arrangement.
    fn slot(&self, index: usize, slot: &Slot, resolved: &Resolved) -> Style;

    /// Decide the arrangement from the allocated size. Returns whether the styles must be
    /// computed again.
    fn settle(&mut self, _allocated: Size<f32>, _resolved: &Resolved) -> bool {
        false
    }

    /// Whether slot `index` is not shown in the current arrangement.
    fn hidden(&self, _index: usize) -> bool {
        false
    }
}

/// A container arranged by `L`. The primitives are this type with a layout:
/// [`Stack`](super::Stack), [`Row`](super::Row), [`Grid`](super::Grid), [`Split`](super::Split),
/// [`Page`](super::Page) and [`Layers`](super::Layers).
pub struct Arranged<L: Layout> {
    pub(super) layout: L,
    pub(super) slots: Vec<Slot>,
    node: Option<NodeId>,
    resolved: Resolved,
    rect: Rect,
    id: Option<String>,
}

impl<L: Layout> Arranged<L> {
    pub(super) fn with_layout(layout: L) -> Self {
        Self {
            layout,
            slots: Vec::new(),
            node: None,
            resolved: Resolved::fixed(16.0, 8.0),
            rect: Rect::ZERO,
            id: None,
        }
    }

    /// Add a child sized by its content.
    #[must_use]
    pub fn push(mut self, child: Element) -> Self {
        self.slots.push(Slot::new(child));
        self
    }

    /// Add a child with its own [`Fit`].
    #[must_use]
    pub fn push_fit(mut self, child: Element, fit: Fit) -> Self {
        let mut slot = Slot::new(child);
        slot.fit = fit;
        self.slots.push(slot);
        self
    }

    /// Add a prepared slot.
    #[must_use]
    pub fn push_slot(mut self, slot: Slot) -> Self {
        self.slots.push(slot);
        self
    }

    /// Address this container by a stable id, for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The layout, to read the arrangement it settled on.
    pub const fn layout(&self) -> &L {
        &self.layout
    }

    /// The layout, to change its state (which pane a split shows).
    pub fn layout_mut(&mut self) -> &mut L {
        &mut self.layout
    }

    /// The number of children.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Replace the children, as a keyed reconciler does when the set changed.
    pub fn set_slots(&mut self, slots: Vec<Slot>) {
        self.slots = slots;
    }

    /// Take the children out, to put them back reordered.
    pub fn take_slots(&mut self) -> Vec<Slot> {
        std::mem::take(&mut self.slots)
    }

    fn restyle(&self, tree: &mut TaffyTree<()>) -> bool {
        let Some(node) = self.node else {
            return false;
        };
        let mut changed = tree
            .set_style(node, self.layout.container(&self.resolved))
            .is_ok();
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(wrapper) = slot.wrapper {
                changed |= tree
                    .set_style(wrapper, self.layout.slot(index, slot, &self.resolved))
                    .is_ok();
            }
        }
        changed
    }

    fn inner_node(tree: &TaffyTree<()>, wrapper: NodeId) -> Option<NodeId> {
        tree.children(wrapper)
            .ok()
            .and_then(|children| children.first().copied())
    }
}

impl<L: Layout + 'static> Widget for Arranged<L> {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolved = Resolved::of(theme);
        let resolved = self.resolved;
        let mut wrappers = Vec::with_capacity(self.slots.len());
        for index in 0..self.slots.len() {
            let style = match self.slots.get(index) {
                Some(slot) => self.layout.slot(index, slot, &resolved),
                None => wrapper_style(),
            };
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            let inner = slot.element.build_layout(tree, theme, map);
            // The child grows into a wrapper that is larger than its content (a filled slot).
            if let Ok(current) = tree.style(inner) {
                let mut grown = current.clone();
                if grown.flex_grow == 0.0 {
                    grown.flex_grow = 1.0;
                }
                let _ = tree.set_style(inner, grown);
            }
            let wrapper = tree.parent_node(style, &[inner]);
            slot.wrapper = Some(wrapper);
            wrappers.push(wrapper);
        }
        let node = tree.parent_node(self.layout.container(&resolved), &wrappers);
        self.node = Some(node);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        let Ok(laid) = tree.layout(node) else {
            return false;
        };
        let allocated = laid.size;
        if self.layout.settle(allocated, &self.resolved) {
            return self.restyle(tree);
        }
        false
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(tree, node, origin);
        let start = Point::new(self.rect.x0, self.rect.y0);
        for slot in &mut self.slots {
            let Some(wrapper) = slot.wrapper else {
                continue;
            };
            let placed = tree.layout(wrapper).copied().unwrap_or_default();
            if let Some(inner) = Self::inner_node(tree, wrapper) {
                slot.element.apply_layout(
                    tree,
                    inner,
                    Point::new(
                        start.x + f64::from(placed.location.x),
                        start.y + f64::from(placed.location.y),
                    ),
                );
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if self.layout.hidden(index) || presence.should_cull(slot.element.get_rect()) {
                continue;
            }
            slot.element.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        for (index, slot) in self.slots.iter_mut().enumerate().rev() {
            if self.layout.hidden(index) {
                continue;
            }
            let Some(wrapper) = slot.wrapper else {
                continue;
            };
            if let Some(inner) = Self::inner_node(ctx.taffy, wrapper) {
                let path = ctx.path;
                slot.element
                    .handle_event(event, &mut ctx.reborrow(path, inner));
                if *ctx.handled {
                    break;
                }
            }
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        for slot in &mut self.slots {
            slot.element.sync(state, path);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.slots
            .iter()
            .map(|slot| slot.element.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for slot in &self.slots {
            visitor(slot.element.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.slots
            .iter_mut()
            .map(|slot| slot.element.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for slot in &mut self.slots {
            dirty |= slot.element.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &str {
        self.layout.name()
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
