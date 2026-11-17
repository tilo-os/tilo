//! The one widget every layout primitive is: a container whose layout a [`Layout`]
//! decides, around child parts.
//!
//! Each child sits in a *wrapper* node whose style carries the child's [`Fit`] (fill weight,
//! bounds, aspect, span, place). The container style and the wrappers' styles come from the
//! layout, and are chosen again during layout from the width the container was given
//! ([`Widget::adjust_layout_style`]): constraints flow down, the container decides its
//! layout from what it was offered, and a child never reads its own final size.

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
pub struct Part {
    pub(super) element: Element,
    pub fit: Fit,
    /// Columns a grid item spans.
    pub span: u16,
    /// Where a layer sits; `None` fills the cell.
    pub at: Option<At>,
    wrapper: Option<NodeId>,
    /// The block size the fit's `aspect` gives at the width the part was solved at. A flex or
    /// grid box derives its cross size from a ratio only when its inline size is definite in
    /// its style, so a part whose width the container decides takes the height the ratio gives
    /// once the width is solved ([`Arranged::derive_aspects`]).
    height: Option<f32>,
}

impl Part {
    /// The child, out of its part.
    #[must_use]
    pub fn into_element(self) -> Element {
        self.element
    }

    /// The child in a part of its own: sized as the view asked, when a host wrapped it in a
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

/// A part's wrapper style around the child whose style is `child`.
///
/// A child out of the flow (an absent one, an effect endpoint) takes its part with it, gap and
/// all. A view that clips what it holds (a scroll view, a table) asks for the whole height
/// because that height is its viewport: through a wrapper that hugs its content it would never
/// see it. The wrapper takes the height the container gives its parts, and the view scrolls
/// inside it. Where the container has none, both stay as tall as the content.
fn wrapped(mut style: Style, child: &Style) -> Style {
    if child.display == taffy::Display::None {
        style.display = taffy::Display::None;
    }
    let full = taffy::style_helpers::percent(1.0_f32);
    let clips = child.overflow.x != taffy::Overflow::Visible
        || child.overflow.y != taffy::Overflow::Visible;
    if child.size.height == full && clips && style.size.height == taffy::Dimension::auto() {
        style.size.height = full;
    }
    style
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

/// How a primitive arranges its parts. See the module docs.
pub trait Layout {
    fn name(&self) -> &'static str;

    /// The readable column this layout keeps what it holds in, when it is one: a page's
    /// measure, which a bar at the foot of the column stands in too.
    fn readable_column(&self) -> Option<Length> {
        None
    }

    /// The container's style for the current layout, `lead` pixels of gutter reserved
    /// before what it holds.
    fn container(&self, resolved: &Resolved, lead: f32) -> Style;

    /// The side the layout leads on: where `lead` reserves its gutter. A row written
    /// right to left leads on the right.
    fn leading(&self) -> Side {
        Side::Leading
    }

    /// The wrapper style of part `index` for the current layout.
    fn part(&self, index: usize, part: &Part, resolved: &Resolved) -> Style;

    /// Whether the layout is decided from the allocated size ([`Self::settle`]).
    fn settles(&self) -> bool {
        false
    }

    /// Decide the layout from the allocated size. Returns whether the decision differs
    /// from the one it settled on last, in whichever tree that was.
    fn settle(&mut self, _allocated: Size<f32>, _resolved: &Resolved) -> bool {
        false
    }

    /// The layout was settled from the tree the container is placed from: what a host
    /// reads of it (whether a navigating split is collapsed) is published here, never from a
    /// tree that was only measured.
    fn applied(&mut self) {}

    /// How many parts the container holds, told before each layout: a layout that arranges by
    /// count (a grid that keeps no empty columns) reads it.
    fn items(&mut self, _count: usize) {}

    /// Whether part `index` is not shown in the current layout.
    fn hidden(&self, _index: usize) -> bool {
        false
    }

    /// Whether the forms this layout holds, in its parts and their sections, line their labels
    /// up in one column: a page's do.
    fn shares_label_column(&self) -> bool {
        false
    }
}

/// A container arranged by `L`. The primitives are this type with a layout:
/// [`Stack`](super::Stack), [`Row`](super::Row), [`Grid`](super::Grid), [`Split`](super::Split),
/// [`Page`](super::Page) and [`Layers`](super::Layers).
#[crate::component::not_a_component(layout, "the language's `stack`, `row` and `grid`")]
pub struct Arranged<L: Layout> {
    pub(super) layout: L,
    pub(super) parts: Vec<Part>,
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
    /// How the parts changed since the host last read it: replaced wholesale
    /// ([`Self::set_parts`]) is a rebuild; a child replaced, inserted, removed, moved or
    /// resized by the part operations is described.
    structure: StructureChange,
}

impl<L: Layout> Arranged<L> {
    pub(super) fn with_layout(layout: L) -> Self {
        Self {
            layout,
            parts: Vec::new(),
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
        self.parts.push(Part::new(child));
        self
    }

    /// Add a child with its own [`Fit`].
    #[must_use]
    pub fn push_fit(mut self, child: Element, fit: Fit) -> Self {
        let mut part = Part::new(child);
        part.fit = fit;
        self.parts.push(part);
        self
    }

    /// Add a prepared part.
    #[must_use]
    pub fn push_part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Address this container by a stable id, for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The layout, to read what it settled on.
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
        self.parts.len()
    }

    /// The box the children take, as far as they have been laid out: what a layout keeps around
    /// its content — the gutters a page centres its column in, the room a lane keeps for the
    /// column beside it — is not part of it.
    #[must_use]
    pub fn content_box(&self) -> Option<Rect> {
        self.parts
            .iter()
            .map(|part| part.element.get_rect())
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            .reduce(|whole, box_| whole.union(box_))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Replace the children wholesale: the host rebuilds and repaints whole. A keyed
    /// reconciler uses the part operations below instead, which it can describe.
    pub fn set_parts(&mut self, parts: Vec<Part>) {
        self.parts = parts;
        self.structure = StructureChange::Rebuild;
    }

    /// Take the children out, to put them back with [`Self::set_parts`].
    pub fn take_parts(&mut self) -> Vec<Part> {
        self.structure = StructureChange::Rebuild;
        std::mem::take(&mut self.parts)
    }

    /// The part at `index`: its child and how it is sized.
    #[must_use]
    pub fn part(&self, index: usize) -> Option<&Part> {
        self.parts.get(index)
    }

    /// The child at `index`, to change it in place: what it reports of the change is its own.
    pub fn child_mut(&mut self, index: usize) -> Option<&mut Element> {
        self.parts.get_mut(index).map(|part| &mut part.element)
    }

    /// Show `element` in part `index` instead of its child, which it returns; the part keeps
    /// its sizing. A change the frame's damage describes: the new child draws itself whole,
    /// and the host repaints where the boxes it moved were and are.
    pub fn replace(&mut self, index: usize, element: Element) -> Option<Element> {
        let part = self.parts.get_mut(index)?;
        self.structure = self.structure.join(StructureChange::Described);
        Some(std::mem::replace(&mut part.element, element))
    }

    /// Insert `part` at `index` (at the end past it). Described, as [`Self::replace`] is.
    pub fn insert(&mut self, index: usize, part: Part) {
        let index = index.min(self.parts.len());
        self.parts.insert(index, part);
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// Take the part at `index` out. Described, as [`Self::replace`] is.
    pub fn remove(&mut self, index: usize) -> Option<Part> {
        if index >= self.parts.len() {
            return None;
        }
        self.structure = self.structure.join(StructureChange::Described);
        Some(self.parts.remove(index))
    }

    /// Move the part at `from` to `to`, its child kept. Described, as [`Self::replace`] is.
    pub fn move_part(&mut self, from: usize, to: usize) {
        if from == to || from >= self.parts.len() {
            return;
        }
        let part = self.parts.remove(from);
        self.parts.insert(to.min(self.parts.len()), part);
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// Size part `index` as `fit`, `span` and `at` say; a change of any is described, as
    /// [`Self::replace`] is.
    pub fn set_placement(&mut self, index: usize, fit: Fit, span: u16, at: Option<At>) {
        let Some(part) = self.parts.get_mut(index) else {
            return;
        };
        if part.fit == fit && part.span == span && part.at == at {
            return;
        }
        // The block size derived from a ratio belongs to the fit that asked for it: a new fit
        // derives its own on the next solve.
        if part.fit.aspect != fit.aspect {
            part.height = None;
        }
        part.fit = fit;
        part.span = span;
        part.at = at;
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

    /// Part `index`'s wrapper style: what the layout arranges it with, and the block size the
    /// fit's `aspect` derived from the width it was solved at.
    ///
    /// A wrapper that holds a derived height keeps no ratio of its own: with one, taffy would
    /// derive the width back out of the height it was given, and a part whose width its
    /// container decides would shrink to a fraction of the room every other solve.
    fn part_style(&self, index: usize, part: &Part) -> Style {
        let mut style = self.layout.part(index, part, &self.resolved);
        if part.fit.aspect.is_some()
            && let Some(height) = part.height
        {
            // A ratio decides the block size; a layer that fills the cell or a share of the
            // column gives way to it, since the ratio is the size the view asked for.
            style.size.height = taffy::style_helpers::length(height);
            style.aspect_ratio = None;
        }
        style
    }

    /// Give every part whose fit asks for an `aspect` the block size the ratio gives at the
    /// inline size it was solved at in `tree`.
    ///
    /// Taffy derives one size from the other only where the known one is definite in the
    /// style. A part takes the width its container decides: stretched across a column, a share
    /// of a row, a layer's cell, a grid's column. What is solved is where the ratio applies, so
    /// the derivation reads the solved width and holds the part at the height that follows;
    /// [`Self::restyle`] gives `tree` the wrapper style that holds it, where it does not yet.
    fn derive_aspects(&mut self, tree: &TaffyTree<()>) {
        let rem = self.resolved.rem;
        for part in &mut self.parts {
            let Some(ratio) = part.fit.aspect else {
                continue;
            };
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            let Ok(laid) = tree.layout(wrapper) else {
                continue;
            };
            let width = laid.size.width;
            if !(width.is_finite() && width > 0.0 && ratio.is_finite() && ratio > 0.0) {
                continue;
            }
            let bounds = |sizing: super::units::Sizing| {
                (
                    sizing.min.map(|min| min.resolve(rem)),
                    sizing.max.map(|max| max.resolve(rem)),
                )
            };
            let (min, max) = bounds(part.fit.height);
            let mut height = width / ratio;
            if let Some(min) = min {
                height = height.max(min);
            }
            if let Some(max) = max {
                height = height.min(max);
            }
            if part.height.is_some_and(|held| (held - height).abs() < 0.5) {
                continue;
            }
            part.height = Some(height);
        }
    }

    /// Whether this container's styles follow from the room a solve gave it: a layout
    /// its layout settles from the width (a grid's columns, a split's panes), a lead kept only
    /// where the column has room for it, or a part whose height a ratio derives from its width.
    fn styled_by_room(&self) -> bool {
        self.layout.settles()
            || self.lead > 0
            || self.parts.iter().any(|part| part.fit.aspect.is_some())
    }

    /// Style the container and its parts in `tree` for what the layout settled on, wherever
    /// `tree` does not hold that style already, and return whether any style there changed.
    ///
    /// The comparison is with `tree`, never with what this container set last: one container
    /// is laid out in more than one tree (a region measures it in a copy of the window's tree
    /// before it places it), and each tree takes the layout its own solve settled on.
    ///
    /// How the node sits in its parent stays as the parent set it: a parent that places it (a
    /// virtual list positions its rows) or grows it into its part changed those after the
    /// layout styled it, and settling changes how the container arranges its children, not
    /// where it is or how its parent sizes it.
    fn restyle(&self, tree: &mut TaffyTree<()>) -> bool {
        let Some(node) = self.node else {
            return false;
        };
        let mut style = self.container_style();
        let mut changed = false;
        if let Ok(placed) = tree.style(node) {
            style.position = placed.position;
            style.inset = placed.inset;
            style.margin = placed.margin;
            style.align_self = placed.align_self;
            style.justify_self = placed.justify_self;
            style.grid_row = placed.grid_row.clone();
            style.grid_column = placed.grid_column.clone();
            style.flex_grow = placed.flex_grow;
            style.flex_shrink = placed.flex_shrink;
            style.flex_basis = placed.flex_basis;
            style.min_size.height = placed.min_size.height;
            if placed.position == taffy::Position::Absolute {
                style.size = placed.size;
                style.min_size = placed.min_size;
                style.max_size = placed.max_size;
            }
            if *placed != style {
                changed |= tree.set_style(node, style).is_ok();
            }
        }
        self.rewrap(tree) || changed
    }

    /// Give each part's wrapper in `tree` the style its child there asks for, where it does not
    /// hold it: a child that left the flow (a banner on its way in, laid out nowhere while it
    /// moves) takes its part with it, and one that came back to rest brings its part back. The
    /// child decides that in its own layout hook, after this container was built: a wrapper
    /// kept at what the child was when the container was built would hold a banner that came to
    /// rest at no size, and the row it says never stands. Returns whether any changed.
    fn rewrap(&self, tree: &mut TaffyTree<()>) -> bool {
        let mut changed = false;
        for (index, part) in self.parts.iter().enumerate() {
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            let Some(style) = Self::inner_node(tree, wrapper)
                .and_then(|inner| tree.style(inner).ok())
                .map(|child| wrapped(self.part_style(index, part), child))
            else {
                continue;
            };
            if tree.style(wrapper).is_ok_and(|held| *held != style) {
                changed |= tree.set_style(wrapper, style).is_ok();
            }
        }
        changed
    }

    /// Lay the child at `index` out again `by` from where the layout placed it: a layer
    /// that stands where something drawn inside a sibling says (a window's controls on the
    /// trailing end of its app's header row, which only that row's layout knows).
    pub(crate) fn offset_child(&mut self, tree: &TaffyTree<()>, index: usize, by: Vec2) {
        let start = Point::new(self.rect.x0, self.rect.y0);
        let Some(part) = self.parts.get_mut(index) else {
            return;
        };
        let Some(wrapper) = part.wrapper else {
            return;
        };
        let placed = tree.layout(wrapper).copied().unwrap_or_default();
        if let Some(inner) = Self::inner_node(tree, wrapper) {
            let at = Point::new(
                start.x + f64::from(placed.location.x),
                start.y + f64::from(placed.location.y),
            ) + by;
            part.element.apply_layout(tree, inner, at);
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
    /// What it holds, each slot where the arrangement puts it.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.children();
    }
    crate::provides!(dyn crate::widget::RowContentBox);

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolved = Resolved::of(theme).with_target(self.interaction.minimum_target());
        self.layout.items(self.parts.len());
        let mut wrappers = Vec::with_capacity(self.parts.len());
        for index in 0..self.parts.len() {
            let style = match self.parts.get(index) {
                Some(part) => self.part_style(index, part),
                None => wrapper_style(),
            };
            let Some(part) = self.parts.get_mut(index) else {
                continue;
            };
            let inner = part.element.build_layout(tree, theme, map);
            let mut style = style;
            // The child grows into a wrapper that is larger than its content (a filled part).
            if let Ok(current) = tree.style(inner) {
                let mut grown = current.clone();
                if grown.flex_grow == 0.0 {
                    grown.flex_grow = 1.0;
                }
                // A part whose height the layout gives (a page that fills, a filled layer)
                // holds its child to it; the mark is read before the wrapper below takes a
                // height of its own.
                let given = style.size.height;
                style = wrapped(style, current);
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
            part.wrapper = Some(wrapper);
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
        text: &mut TextRenderer,
    ) -> bool {
        let shared = self.layout.shares_label_column() && {
            let mut parts: Vec<&mut (dyn Widget + '_)> = self
                .parts
                .iter_mut()
                .map(|part| part.element.as_mut() as &mut (dyn Widget + '_))
                .collect();
            crate::widget::FormSection::share_label_column(&mut parts, tree, text)
        };
        let Ok(laid) = tree.layout(node) else {
            return shared;
        };
        if !self.styled_by_room() {
            return self.rewrap(tree) | shared;
        }
        let allocated = laid.size;
        self.derive_aspects(tree);
        // The column this pass gave the container decides whether what it holds keeps the lead.
        // A change of class is a change of style, solved again on the next pass.
        if self.lead > 0 {
            // What the column has left once the lead is reserved decides whether it is kept: the
            // box less one lead, which does not change with the answer.
            let room = (allocated.width - lead_width(self.lead, &self.resolved)).max(0.0);
            self.column = Some(WidthClass::with_history(
                room / self.resolved.rem,
                self.column,
            ));
        }
        // The layout is settled from what this tree allocated, and this tree is styled for
        // it wherever it is not yet. Whether the layout's decision changed since it last settled
        // says nothing about this tree: another tree's solve may have settled it.
        self.layout.settle(allocated, &self.resolved);
        self.restyle(tree) | shared
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(tree, node, origin);
        // What this container draws, hit-tests and tells its host is the layout of the
        // tree it is placed from, whichever tree was settled last.
        if self.layout.settles()
            && let Ok(laid) = tree.layout(node)
        {
            self.layout.settle(laid.size, &self.resolved);
            self.layout.applied();
        }
        let start = Point::new(self.rect.x0, self.rect.y0);
        for part in &mut self.parts {
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            let placed = tree.layout(wrapper).copied().unwrap_or_default();
            if let Some(inner) = Self::inner_node(tree, wrapper) {
                part.element.apply_layout(
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
        for (index, part) in self.parts.iter_mut().enumerate() {
            if self.layout.hidden(index) || presence.should_cull(part.element.get_rect()) {
                continue;
            }
            part.element
                .draw_child(scene, transform, text, theme, presence);
        }
    }

    /// A finger on a child is the child's to claim.
    fn touch_claim(&self, point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        self.parts
            .iter()
            .enumerate()
            .rev()
            .filter(|(index, _)| !self.layout.hidden(*index))
            .find(|(_, part)| part.element.get_rect().contains(point))
            .map_or(crate::interaction::TouchClaim::Pointer, |(_, part)| {
                part.element.touch_claim(point)
            })
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        // A child holding the pointer (a drag it latched, a menu open in it) owns the input:
        // what stands over its menu, drawn later and so asked first, never takes a press on it.
        let owner = self.parts.iter().enumerate().position(|(index, part)| {
            !self.layout.hidden(index) && crate::widget::holds_pointer(part.element.as_ref())
        });
        for (index, part) in self.parts.iter_mut().enumerate().rev() {
            if self.layout.hidden(index) || owner.is_some_and(|owner| owner != index) {
                continue;
            }
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            if let Some(inner) = Self::inner_node(ctx.taffy, wrapper) {
                let path = ctx.path;
                part.element
                    .handle_event(event, &mut ctx.reborrow(path, inner));
                if *ctx.handled {
                    break;
                }
            }
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        for part in &mut self.parts {
            part.element.sync(state, path);
        }
    }

    /// A layout paints nothing of its own: what changed is what its
    /// shown children repaint. The default would repaint its whole box
    /// whenever any child changed, which for a window's root layout is
    /// the whole window for each keystroke.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        for (index, part) in self.parts.iter().enumerate() {
            if !self.layout.hidden(index) {
                part.element.collect_damage(regions);
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.parts
            .iter()
            .map(|part| part.element.as_ref())
            .collect()
    }

    /// A shown child holding the pointer (a drag it latched, a menu open in it) owns input the
    /// same way through this layout: without the forward, a drag that began in a part and
    /// travelled out of it would stop receiving motion and release, and its drop would be lost
    /// at the edge. The hidden parts hold nothing that can be pressed.
    fn wants_pointer_capture(&self) -> bool {
        self.parts
            .iter()
            .enumerate()
            .any(|(index, part)| !self.layout.hidden(index) && part.element.wants_pointer_capture())
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for part in &self.parts {
            visitor(part.element.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.parts
            .iter_mut()
            .map(|part| part.element.as_mut() as &mut (dyn Widget + '_))
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
        for part in &mut self.parts {
            dirty |= part.element.take_structure_dirty();
        }
        dirty
    }

    /// Laid out once, a layout lays out again only for what a child reports.
    fn is_layout_dirty(&self) -> bool {
        self.node.is_none() || self.parts.iter().any(|part| part.element.is_layout_dirty())
    }

    fn needs_animation_frame(&self) -> bool {
        self.parts
            .iter()
            .any(|part| part.element.needs_animation_frame())
    }

    /// What the part operations did, joined with what the children report.
    fn take_structure_change(&mut self) -> StructureChange {
        let mut change = std::mem::take(&mut self.structure);
        for part in &mut self.parts {
            change = change.join(part.element.take_structure_change());
        }
        change
    }

    /// A layout paints nothing of its own (see [`Self::collect_damage`]), so it is
    /// dirty exactly when a shown child is. The default (`defaults::needs_redraw`) answers
    /// `true` for a widget with no children, which made an empty layout — a view whose
    /// every part is conditional and none stands — dirty forever, drawing a frame each time
    /// it was asked.
    fn needs_redraw(&self) -> bool {
        self.parts.iter().enumerate().any(|(index, part)| {
            !self.layout.hidden(index)
                && (part.element.needs_redraw() || part.element.needs_animation_frame())
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
