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
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::Side;
use super::units::{At, Fit, Length, WidthClass};
use crate::interaction::InteractionEnvironment;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::DrawChild;
use crate::widget::{AppModel, Element, ModelPath, StructureChange, TaffyTreeExt, Widget};

/// What a layout reads of the theme: the `rem`, the rhythm gaps and a control's least side,
/// in logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    pub rem: f32,
    gaps: [f32; 5],
    target: f32,
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
            target: InteractionEnvironment::default().minimum_target(),
        }
    }

    /// Fixed values, for tests of layout math.
    #[must_use]
    pub const fn fixed(rem: f32, gap: f32) -> Self {
        Self {
            rem,
            gaps: [gap; 5],
            target: 32.0,
        }
    }

    /// The same, for a window the person reaches with a control of `target` pixels.
    #[must_use]
    pub const fn with_target(mut self, target: f32) -> Self {
        self.target = target;
        self
    }

    /// The row lead: the gutter a row's leading control stands in, its cell and the gap after
    /// it. A document lays its body one lead in, under the text its heading's control leads
    /// to, at whatever size the person's controls take.
    #[must_use]
    pub const fn lead(&self) -> f32 {
        self.target + self.gap(Rhythm::Related)
    }

    /// The gap of `step` in logical pixels.
    #[must_use]
    pub const fn gap(&self, step: Rhythm) -> f32 {
        match step {
            Rhythm::None => 0.0,
            Rhythm::Tight => self.gaps[0],
            Rhythm::Related => self.gaps[1],
            Rhythm::Sibling => self.gaps[2],
            Rhythm::Section => self.gaps[3],
            Rhythm::Group => self.gaps[4],
        }
    }
}

/// A child with how it is sized and placed.
#[crate::component::not_a_component(
    part,
    "a child's place in the language's `stack`, `row` and `grid`"
)]
pub struct Slot {
    pub(super) element: Element,
    pub fit: Fit,
    /// Columns a grid item spans.
    pub span: u16,
    /// Where a layer sits; `None` fills the cell.
    pub at: Option<At>,
    wrapper: Option<NodeId>,
    /// The block size the fit's `aspect` gives at the width the slot was solved at. A flex or
    /// grid box derives its cross size from a ratio only when its inline size is definite in
    /// its style, so a slot whose width the container decides takes the height the ratio gives
    /// once the width is solved ([`Arranged::derive_aspects`]).
    height: Option<f32>,
}

impl Slot {
    /// The child, out of its slot.
    #[must_use]
    pub fn into_element(self) -> Element {
        self.element
    }

    /// The child in a slot of its own: sized as the view asked, when a host wrapped it in a
    /// [`Fitted`](crate::widget::Fitted) carrying the fit its node wrote, and hugging its
    /// content otherwise.
    #[must_use]
    pub fn new(element: Element) -> Self {
        let fit = element
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::widget::Fitted>())
            .map_or_else(Fit::hug, crate::widget::Fitted::fit);
        Self {
            element,
            fit,
            span: 1,
            at: None,
            wrapper: None,
            height: None,
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

/// How far `lead` leads come to, in logical pixels.
#[must_use]
fn lead_width(lead: i16, resolved: &Resolved) -> f32 {
    resolved.lead() * f32::from(lead)
}

/// A container's style with the `lead` it reserves before its content: what it holds stands
/// that many logical pixels in from its leading edge, which is where a document's body starts
/// under the text its heading's control leads to.
pub(super) fn lead_container(mut style: Style, lead: f32, leading: Side) -> Style {
    if lead > 0.0 {
        let at = taffy::style_helpers::length(lead);
        match leading {
            Side::Leading => style.padding.left = at,
            Side::Trailing => style.padding.right = at,
        }
    }
    style
}

/// How a primitive arranges its slots. See the module docs.
pub trait Layout {
    fn name(&self) -> &'static str;

    /// The readable column this arrangement keeps what it holds in, when it is one: a page's
    /// measure, which a bar at the foot of the column stands in too.
    fn readable_column(&self) -> Option<Length> {
        None
    }

    /// The container's style for the current arrangement, `lead` pixels of gutter reserved
    /// before what it holds.
    fn container(&self, resolved: &Resolved, lead: f32) -> Style;

    /// The side the arrangement leads on: where `lead` reserves its gutter. A row written
    /// right to left leads on the right.
    fn leading(&self) -> Side {
        Side::Leading
    }

    /// The wrapper style of slot `index` for the current arrangement.
    fn slot(&self, index: usize, slot: &Slot, resolved: &Resolved) -> Style;

    /// Decide the arrangement from the allocated size. Returns whether the styles must be
    /// computed again.
    fn settle(&mut self, _allocated: Size<f32>, _resolved: &Resolved) -> bool {
        false
    }

    /// How many slots the container holds, told before each layout: a layout that arranges by
    /// count (a grid that keeps no empty columns) reads it.
    fn items(&mut self, _count: usize) {}

    /// Whether slot `index` is not shown in the current arrangement.
    fn hidden(&self, _index: usize) -> bool {
        false
    }
}

/// A container arranged by `L`. The primitives are this type with a layout:
/// [`Stack`](super::Stack), [`Row`](super::Row), [`Grid`](super::Grid), [`Split`](super::Split),
/// [`Page`](super::Page) and [`Layers`](super::Layers).
#[crate::component::not_a_component(layout, "the language's `stack`, `row` and `grid`")]
pub struct Arranged<L: Layout> {
    pub(super) layout: L,
    pub(super) slots: Vec<Slot>,
    node: Option<NodeId>,
    resolved: Resolved,
    /// Leads this container reserves before its content: a document's body stands one lead
    /// in, under the text its heading's control leads to. Kept only where the column what it
    /// holds is read in has the room for it: on a phone's screen what it holds starts on the
    /// page's gutter, and the words keep the width the indent would have taken.
    lead: i16,
    /// The width class of the column what this container holds was last laid out in, its lead
    /// taken out: what decides whether that column has room for the lead.
    column: Option<WidthClass>,
    /// How the person reaches the window, which sizes a lead.
    interaction: InteractionEnvironment,
    rect: Rect,
    id: Option<String>,
    /// How the slots changed since the host last read it: replaced wholesale
    /// ([`Self::set_slots`]) is a rebuild; a child replaced, inserted, removed, moved or
    /// resized by the slot operations is described.
    structure: StructureChange,
}

impl<L: Layout> Arranged<L> {
    pub(super) fn with_layout(layout: L) -> Self {
        Self {
            layout,
            slots: Vec::new(),
            node: None,
            resolved: Resolved::fixed(16.0, 8.0),
            lead: 0,
            column: None,
            interaction: InteractionEnvironment::default(),
            rect: Rect::ZERO,
            id: None,
            structure: StructureChange::None,
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

    /// The box the children take, as far as they have been laid out: what a layout keeps around
    /// its content — the gutters a page centres its column in, the room a lane keeps for the
    /// column beside it — is not part of it.
    #[must_use]
    pub fn content_box(&self) -> Option<Rect> {
        self.slots
            .iter()
            .map(|slot| slot.element.get_rect())
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            .reduce(|whole, box_| whole.union(box_))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Replace the children wholesale: the host rebuilds and repaints whole. A keyed
    /// reconciler uses the slot operations below instead, which it can describe.
    pub fn set_slots(&mut self, slots: Vec<Slot>) {
        self.slots = slots;
        self.structure = StructureChange::Rebuild;
    }

    /// Take the children out, to put them back with [`Self::set_slots`].
    pub fn take_slots(&mut self) -> Vec<Slot> {
        self.structure = StructureChange::Rebuild;
        std::mem::take(&mut self.slots)
    }

    /// The slot at `index`: its child and how it is sized.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<&Slot> {
        self.slots.get(index)
    }

    /// The child at `index`, to change it in place: what it reports of the change is its own.
    pub fn child_mut(&mut self, index: usize) -> Option<&mut Element> {
        self.slots.get_mut(index).map(|slot| &mut slot.element)
    }

    /// Show `element` in slot `index` instead of its child, which it returns; the slot keeps
    /// its sizing. A change the frame's damage describes: the new child draws itself whole,
    /// and the host repaints where the boxes it moved were and are.
    pub fn replace(&mut self, index: usize, element: Element) -> Option<Element> {
        let slot = self.slots.get_mut(index)?;
        self.structure = self.structure.join(StructureChange::Described);
        Some(std::mem::replace(&mut slot.element, element))
    }

    /// Insert `slot` at `index` (at the end past it). Described, as [`Self::replace`] is.
    pub fn insert(&mut self, index: usize, slot: Slot) {
        let index = index.min(self.slots.len());
        self.slots.insert(index, slot);
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// Take the slot at `index` out. Described, as [`Self::replace`] is.
    pub fn remove(&mut self, index: usize) -> Option<Slot> {
        if index >= self.slots.len() {
            return None;
        }
        self.structure = self.structure.join(StructureChange::Described);
        Some(self.slots.remove(index))
    }

    /// Move the slot at `from` to `to`, its child kept. Described, as [`Self::replace`] is.
    pub fn move_slot(&mut self, from: usize, to: usize) {
        if from == to || from >= self.slots.len() {
            return;
        }
        let slot = self.slots.remove(from);
        self.slots.insert(to.min(self.slots.len()), slot);
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// Size slot `index` as `fit`, `span` and `at` say; a change of any is described, as
    /// [`Self::replace`] is.
    pub fn set_placement(&mut self, index: usize, fit: Fit, span: u16, at: Option<At>) {
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        if slot.fit == fit && slot.span == span && slot.at == at {
            return;
        }
        // The block size derived from a ratio belongs to the fit that asked for it: a new fit
        // derives its own on the next solve.
        if slot.fit.aspect != fit.aspect {
            slot.height = None;
        }
        slot.fit = fit;
        slot.span = span;
        slot.at = at;
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// Reserve `lead` leads before what this container holds (`lead: 1`), or none.
    pub fn set_lead(&mut self, lead: i16) {
        self.lead = lead;
    }

    /// How many leads this container reserves before its content.
    #[must_use]
    pub const fn lead(&self) -> i16 {
        self.lead
    }

    /// The container's style: what its layout arranges with, and the lead it reserves before
    /// its content. A column without the room for a lead reserves none.
    fn container_style(&self) -> Style {
        let lead = if self.lead > 0 && self.column != Some(WidthClass::Compact) {
            lead_width(self.lead, &self.resolved)
        } else {
            0.0
        };
        self.layout.container(&self.resolved, lead)
    }

    /// Slot `index`'s wrapper style: what the layout arranges it with, and the block size the
    /// fit's `aspect` derived from the width it was solved at.
    ///
    /// A wrapper that holds a derived height keeps no ratio of its own: with one, taffy would
    /// derive the width back out of the height it was given, and a slot whose width its
    /// container decides would shrink to a fraction of the room every other solve.
    fn slot_style(&self, index: usize, slot: &Slot) -> Style {
        let mut style = self.layout.slot(index, slot, &self.resolved);
        if slot.fit.aspect.is_some()
            && let Some(height) = slot.height
        {
            // A ratio decides the block size; a layer that fills the cell or a share of the
            // column gives way to it, since the ratio is the size the view asked for.
            style.size.height = taffy::style_helpers::length(height);
            style.aspect_ratio = None;
        }
        style
    }

    /// Give every slot whose fit asks for an `aspect` the block size the ratio gives at the
    /// inline size it was solved at.
    ///
    /// Taffy derives one size from the other only where the known one is definite in the
    /// style. A slot takes the width its container decides: stretched across a column, a share
    /// of a row, a layer's cell, a grid's column. What is solved is where the ratio applies, so
    /// the derivation reads the solved width and holds the slot at the height that follows.
    /// Returns whether any slot's height changed, so the host solves again.
    fn derive_aspects(&mut self, tree: &mut TaffyTree<()>) -> bool {
        let mut changed = false;
        for index in 0..self.slots.len() {
            let Some(slot) = self.slots.get(index) else {
                continue;
            };
            let Some(ratio) = slot.fit.aspect else {
                continue;
            };
            let Some(wrapper) = slot.wrapper else {
                continue;
            };
            let Ok(laid) = tree.layout(wrapper) else {
                continue;
            };
            let width = laid.size.width;
            if !(width.is_finite() && width > 0.0 && ratio.is_finite() && ratio > 0.0) {
                continue;
            }
            let rem = self.resolved.rem;
            let bounds = |sizing: super::units::Sizing| {
                (
                    sizing.min.map(|min| min.resolve(rem)),
                    sizing.max.map(|max| max.resolve(rem)),
                )
            };
            let (min, max) = bounds(slot.fit.height);
            let mut height = width / ratio;
            if let Some(min) = min {
                height = height.max(min);
            }
            if let Some(max) = max {
                height = height.min(max);
            }
            if slot.height.is_some_and(|held| (held - height).abs() < 0.5) {
                continue;
            }
            if let Some(slot) = self.slots.get_mut(index) {
                slot.height = Some(height);
            }
            let style = self
                .slots
                .get(index)
                .map(|slot| self.slot_style(index, slot));
            let Some(style) = style else { continue };
            if tree.set_style(wrapper, style).is_ok() {
                changed = true;
            }
        }
        changed
    }

    /// Style the container and its slots again for what the layout settled on. Where the node
    /// sits in its parent stays as the parent set it: a parent that places it (a virtual list
    /// positions its rows) changed those after the layout styled it, and settling changes how
    /// the container arranges its children, not where it is.
    fn restyle(&self, tree: &mut TaffyTree<()>) -> bool {
        let Some(node) = self.node else {
            return false;
        };
        let mut style = self.container_style();
        if let Ok(placed) = tree.style(node) {
            style.position = placed.position;
            style.inset = placed.inset;
            style.margin = placed.margin;
            style.align_self = placed.align_self;
            style.justify_self = placed.justify_self;
            style.grid_row = placed.grid_row.clone();
            style.grid_column = placed.grid_column.clone();
            if placed.position == taffy::Position::Absolute {
                style.size = placed.size;
                style.min_size = placed.min_size;
                style.max_size = placed.max_size;
            }
        }
        let mut changed = tree.set_style(node, style).is_ok();
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(wrapper) = slot.wrapper {
                changed |= tree
                    .set_style(wrapper, self.slot_style(index, slot))
                    .is_ok();
            }
        }
        changed
    }

    /// Lay the child at `index` out again `by` from where the arrangement placed it: a layer
    /// that stands where something drawn inside a sibling says (a window's controls on the
    /// trailing end of its app's header row, which only that row's layout knows).
    pub(crate) fn offset_child(&mut self, tree: &TaffyTree<()>, index: usize, by: Vec2) {
        let start = Point::new(self.rect.x0, self.rect.y0);
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        let Some(wrapper) = slot.wrapper else {
            return;
        };
        let placed = tree.layout(wrapper).copied().unwrap_or_default();
        if let Some(inner) = Self::inner_node(tree, wrapper) {
            let at = Point::new(
                start.x + f64::from(placed.location.x),
                start.y + f64::from(placed.location.y),
            ) + by;
            slot.element.apply_layout(tree, inner, at);
        }
    }

    fn inner_node(tree: &TaffyTree<()>, wrapper: NodeId) -> Option<NodeId> {
        tree.children(wrapper)
            .ok()
            .and_then(|children| children.first().copied())
    }
}

impl<L: Layout> crate::widget::RowContentBox for Arranged<L> {
    fn row_content_box(&self) -> Option<Rect> {
        self.content_box()
    }
}

impl<L: Layout + 'static> Widget for Arranged<L> {
    crate::provides!(dyn crate::widget::RowContentBox);

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolved = Resolved::of(theme).with_target(self.interaction.minimum_target());
        self.layout.items(self.slots.len());
        let mut wrappers = Vec::with_capacity(self.slots.len());
        for index in 0..self.slots.len() {
            let style = match self.slots.get(index) {
                Some(slot) => self.slot_style(index, slot),
                None => wrapper_style(),
            };
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            let inner = slot.element.build_layout(tree, theme, map);
            let mut style = style;
            // The child grows into a wrapper that is larger than its content (a filled slot).
            if let Ok(current) = tree.style(inner) {
                // A child out of the flow (an absent one, an effect endpoint) takes its slot
                // with it, gap and all.
                if current.display == taffy::Display::None {
                    style.display = taffy::Display::None;
                }
                let mut grown = current.clone();
                if grown.flex_grow == 0.0 {
                    grown.flex_grow = 1.0;
                }
                // A slot whose height the layout gives (a page that fills, a filled layer)
                // holds its child to it; the mark is read before the wrapper below takes a
                // height of its own.
                let given = style.size.height;
                // A view that clips what it holds (a scroll view, a table) asks for the whole
                // height because that height is its viewport: through a wrapper that hugs its
                // content it would never see it. The wrapper takes the height the container
                // gives its slots, and the view scrolls inside it. Where the container has
                // none, both stay as tall as the content.
                let full = taffy::style_helpers::percent(1.0_f32);
                let clips = current.overflow.x != taffy::Overflow::Visible
                    || current.overflow.y != taffy::Overflow::Visible;
                if current.size.height == full
                    && clips
                    && style.size.height == taffy::Dimension::auto()
                {
                    style.size.height = full;
                }
                // A wrapper whose height is given (a filled layer, a filled share of a stack)
                // holds the child to it: the child shrinks to that height, and what it holds
                // scrolls or overflows inside it rather than pushing past the wrapper. A layer
                // straddling an edge stands in a wrapper of no height and keeps its own.
                let zero = taffy::style_helpers::length(0.0_f32);
                let filled_layer = given == taffy::style_helpers::percent(1.0_f32);
                let filled_share = style.flex_grow > 0.0
                    && style.flex_basis == zero
                    && style.min_size.height == zero
                    && style.size.height != zero;
                if (filled_layer || filled_share)
                    && grown.min_size.height == taffy::Dimension::auto()
                {
                    grown.min_size.height = taffy::style_helpers::length(0.0_f32);
                }
                let _ = tree.set_style(inner, grown);
            }
            let wrapper = tree.parent_node(style, &[inner]);
            slot.wrapper = Some(wrapper);
            wrappers.push(wrapper);
        }
        let node = tree.parent_node(self.container_style(), &wrappers);
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
        let aspects = self.derive_aspects(tree);
        // The column this pass gave the container decides whether what it holds keeps the lead.
        // A change of class is a change of style, solved again on the next pass.
        let mut room_changed = false;
        if self.lead > 0 {
            // What the column has left once the lead is reserved decides whether it is kept: the
            // box less one lead, which does not change with the answer.
            let room = (allocated.width - lead_width(self.lead, &self.resolved)).max(0.0);
            let column = WidthClass::with_history(room / self.resolved.rem, self.column);
            room_changed = self.column != Some(column);
            self.column = Some(column);
        }
        if self.layout.settle(allocated, &self.resolved) || room_changed {
            return self.restyle(tree) || aspects;
        }
        aspects
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
            slot.element
                .draw_child(scene, transform, text, theme, presence);
        }
    }

    /// A finger on a child is the child's to claim.
    fn touch_claim(&self, point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        self.slots
            .iter()
            .enumerate()
            .rev()
            .filter(|(index, _)| !self.layout.hidden(*index))
            .find(|(_, slot)| slot.element.get_rect().contains(point))
            .map_or(crate::interaction::TouchClaim::Pointer, |(_, slot)| {
                slot.element.touch_claim(point)
            })
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        // A child holding the pointer (a drag it latched, a menu open in it) owns the input:
        // what stands over its menu, drawn later and so asked first, never takes a press on it.
        let owner = self.slots.iter().enumerate().position(|(index, slot)| {
            !self.layout.hidden(index) && crate::widget::holds_pointer(slot.element.as_ref())
        });
        for (index, slot) in self.slots.iter_mut().enumerate().rev() {
            if self.layout.hidden(index) || owner.is_some_and(|owner| owner != index) {
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

    /// An arrangement paints nothing of its own: what changed is what its
    /// shown children repaint. The default would repaint its whole box
    /// whenever any child changed, which for a window's root arrangement is
    /// the whole window for each keystroke.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        for (index, slot) in self.slots.iter().enumerate() {
            if !self.layout.hidden(index) {
                slot.element.collect_damage(regions);
            }
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

    /// Receive how the person reaches the window: a lead is a gutter for a control, and a
    /// control answers to the least target the window offers.
    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.interaction = environment;
        crate::widget::defaults::set_interaction_environment(self.children_mut(), environment);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure).rebuilds();
        for slot in &mut self.slots {
            dirty |= slot.element.take_structure_dirty();
        }
        dirty
    }

    /// Laid out once, an arrangement lays out again only for what a child reports.
    fn is_layout_dirty(&self) -> bool {
        self.node.is_none() || self.slots.iter().any(|slot| slot.element.is_layout_dirty())
    }

    fn needs_animation_frame(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.element.needs_animation_frame())
    }

    /// What the slot operations did, joined with what the children report.
    fn take_structure_change(&mut self) -> StructureChange {
        let mut change = std::mem::take(&mut self.structure);
        for slot in &mut self.slots {
            change = change.join(slot.element.take_structure_change());
        }
        change
    }

    /// An arrangement paints nothing of its own (see [`Self::collect_damage`]), so it is
    /// dirty exactly when a shown child is. The default (`defaults::needs_redraw`) answers
    /// `true` for a widget with no children, which made an empty arrangement — a view whose
    /// every part is conditional and none stands — dirty forever, drawing a frame each time
    /// it was asked.
    fn needs_redraw(&self) -> bool {
        self.slots.iter().enumerate().any(|(index, slot)| {
            !self.layout.hidden(index)
                && (slot.element.needs_redraw() || slot.element.needs_animation_frame())
        })
    }

    fn name(&self) -> &str {
        self.layout.name()
    }

    fn readable_column(&self) -> Option<Length> {
        self.layout.readable_column()
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
