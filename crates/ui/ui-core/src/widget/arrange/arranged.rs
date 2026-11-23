//! The one widget every layout primitive is: a container whose layout a [`Layout`]
//! decides, around child parts.
//!
//! Each child sits in a *wrapper* node whose style carries the child's [`Fit`] (fill weight,
//! bounds, aspect, span, place). The container style and the wrappers' styles come from the
//! layout, and are chosen within the solve from the width the container is given
//! ([`Widget::style_for_width`]): constraints flow down, the container decides its layout from
//! what it was offered, and a child never reads its own final size.

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
use crate::widget::layout::Styles;
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
    /// resized by the part operations is laid out again under this container's own node
    /// ([`StructureChange::Children`]) once it has one, and described before.
    structure: StructureChange,
    /// The parts changed since they were laid out: the next remount lays every part out again
    /// under this container's node ([`Widget::relaid_layout_node`]), and the rest of the window
    /// keeps its layout.
    relaid: bool,
    /// Where this container stood when its parts changed: what they drew there is repainted.
    relaid_from: Option<Rect>,
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
            relaid: false,
            relaid_from: None,
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
    /// its content — the gutters a page centres its column in, the room a gutter keeps for the
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
        let held = std::mem::replace(&mut part.element, element);
        // The new child has no layout until the parts are laid out again: nothing places it or
        // hands it input through the node its predecessor stood in.
        part.wrapper = None;
        self.parts_changed();
        Some(held)
    }

    /// Insert `part` at `index` (at the end past it). Described, as [`Self::replace`] is.
    pub fn insert(&mut self, index: usize, part: Part) {
        let index = index.min(self.parts.len());
        self.parts.insert(index, part);
        self.parts_changed();
    }

    /// Take the part at `index` out. Described, as [`Self::replace`] is.
    pub fn remove(&mut self, index: usize) -> Option<Part> {
        if index >= self.parts.len() {
            return None;
        }
        self.parts_changed();
        Some(self.parts.remove(index))
    }

    /// Move the part at `from` to `to`, its child kept. Described, as [`Self::replace`] is.
    pub fn move_part(&mut self, from: usize, to: usize) {
        if from == to || from >= self.parts.len() {
            return;
        }
        let part = self.parts.remove(from);
        self.parts.insert(to.min(self.parts.len()), part);
        self.parts_changed();
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
        part.fit = fit;
        part.span = span;
        part.at = at;
        self.parts_changed();
    }

    /// The part operations changed what this container holds: once it is laid out, its parts are
    /// laid out again under its own node; before, the change is described.
    fn parts_changed(&mut self) {
        if self.node.is_some() {
            self.relaid = true;
            self.relaid_from.get_or_insert(self.rect);
            self.structure = self.structure.join(StructureChange::Children);
        } else {
            self.structure = self.structure.join(StructureChange::Described);
        }
    }

    /// Put `inner`, the node part `index`'s child just built, in the part's wrapper in `tree`, styled
    /// as the layout places the part: the wrapper's node.
    fn wrap_part(&mut self, index: usize, tree: &mut TaffyTree<()>, inner: NodeId) -> NodeId {
        let mut style = match self.parts.get(index) {
            Some(part) => self.part_style(index, part),
            None => wrapper_style(),
        };
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
            if (filled_layer || filled_share) && grown.min_size.height == taffy::Dimension::auto() {
                grown.min_size.height = taffy::style_helpers::length(0.0_f32);
            }
            let _ = tree.set_style(inner, grown);
        }
        let wrapper = tree.parent_node(style, &[inner]);
        if let Some(part) = self.parts.get_mut(index) {
            part.wrapper = Some(wrapper);
            let _ = tree.set_restyled_for_width(wrapper, part.fit.aspect.is_some());
        }
        wrapper
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

    /// Part `index`'s wrapper style: what the layout arranges it with.
    ///
    /// A part whose fit asks for an `aspect` is as tall as its child, which the width its wrapper
    /// is solved at gives the height the ratio asks for ([`Self::hold_aspect`]): a layer that
    /// fills the cell or a share of the column gives way to it, since the ratio is the size the
    /// view asked for. Taffy derives one size from a ratio only where the other is definite in the
    /// style, and a part takes the width its container decides.
    fn part_style(&self, index: usize, part: &Part) -> Style {
        let mut style = self.layout.part(index, part, &self.resolved);
        if part.fit.aspect.is_some() {
            style.size.height = taffy::Dimension::auto();
            style.aspect_ratio = None;
        }
        style
    }

    /// Hold the child of `wrapper`, a part whose fit asks for an `aspect`, at the block size the
    /// ratio gives at `width`, within the fit's bounds.
    fn hold_aspect(&self, styles: &mut taffy::Restyle<'_>, wrapper: NodeId, width: f32) {
        let Some(height) = self
            .parts
            .iter()
            .find(|part| part.wrapper == Some(wrapper))
            .and_then(|part| self.aspect_height(part, width))
        else {
            return;
        };
        let Some(&inner) = styles.children(wrapper).first() else {
            return;
        };
        // The child is as tall as the ratio gives, whatever share or fill its own style asks for.
        let held = taffy::style_helpers::length(height);
        let mut style = styles.style(inner).clone();
        style.size.height = held;
        style.min_size.height = held;
        style.max_size.height = held;
        style.flex_basis = taffy::Dimension::auto();
        if *styles.style(inner) != style {
            styles.set_style(inner, style);
        }
    }

    /// The block size part `part`'s ratio gives at `width`, within its fit's bounds; none for a
    /// part that asks for no ratio.
    fn aspect_height(&self, part: &Part, width: f32) -> Option<f32> {
        let ratio = part.fit.aspect?;
        if !(width.is_finite() && width > 0.0 && ratio.is_finite() && ratio > 0.0) {
            return None;
        }
        let rem = self.resolved.rem;
        let mut height = width / ratio;
        if let Some(min) = part.fit.height.min {
            height = height.max(min.resolve(rem));
        }
        if let Some(max) = part.fit.height.max {
            height = height.min(max.resolve(rem));
        }
        Some(height)
    }

    /// Give each part that asks for a ratio and spans the container's content (a layer that
    /// fills the cell) the height the ratio gives at that width: its wrapper's height, which the
    /// container's own layout reads.
    fn hold_spanning_aspects(&self, styles: &mut taffy::Restyle<'_>, width: f32) {
        let node = styles.node();
        let padding = styles.style(node).padding;
        let border = styles.style(node).border;
        let side = |value: taffy::LengthPercentage| {
            let raw = value.into_raw();
            if raw.tag() == taffy::CompactLength::LENGTH_TAG {
                raw.value()
            } else {
                0.0
            }
        };
        let content = width
            - side(padding.left)
            - side(padding.right)
            - side(border.left)
            - side(border.right);
        let full = taffy::style_helpers::percent(1.0_f32);
        for part in &self.parts {
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            if styles.style(wrapper).size.width != full {
                continue;
            }
            let Some(height) = self.aspect_height(part, content) else {
                continue;
            };
            let held = taffy::style_helpers::length(height);
            if styles.style(wrapper).size.height != held {
                let mut style = styles.style(wrapper).clone();
                style.size.height = held;
                styles.set_style(wrapper, style);
            }
        }
    }

    /// Whether this container's style follows from the width a solve gives it: a layout its
    /// layout settles from the width (a grid's columns, a split's panes), or a lead kept only
    /// where the column is wide enough for it.
    fn styled_by_width(&self) -> bool {
        self.layout.settles() || self.lead > 0
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
    fn restyle(&self, tree: &mut impl Styles) -> bool {
        let Some(node) = self.node else {
            return false;
        };
        let mut style = self.container_style();
        let mut changed = false;
        if let Some(placed) = tree.style_of(node) {
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
                changed |= tree.set_style_of(node, style);
            }
        }
        self.rewrap(tree) || changed
    }

    /// Give each part's wrapper in `tree` the style its child there asks for, where it does not
    /// hold it: a child that left the flow (a banner on its way in, laid out nowhere while it
    /// moves) takes its part with it, and one that came back to rest brings its part back. The
    /// child decides that before its layout is solved ([`Widget::size_before_solve`], which a
    /// window asks of a container's children before the container), after this container was
    /// built: a wrapper kept at what the child was when the container was built would hold a
    /// banner that came to rest at no size, and the row it says never stands. Returns whether any
    /// changed.
    fn rewrap(&self, tree: &mut impl Styles) -> bool {
        let mut changed = false;
        for (index, part) in self.parts.iter().enumerate() {
            let Some(wrapper) = part.wrapper else {
                continue;
            };
            let Some(style) = tree
                .first_child_of(wrapper)
                .and_then(|inner| tree.style_of(inner))
                .map(|child| wrapped(self.part_style(index, part), child))
            else {
                continue;
            };
            if tree.style_of(wrapper).is_some_and(|held| *held != style) {
                changed |= tree.set_style_of(wrapper, style);
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
    /// What it holds, each where the layout puts it.
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
        self.relaid = false;
        // A part changed while the parts below build belongs to this build, not to the node the
        // container had in the tree it was last laid out in.
        self.node = None;
        let mut wrappers = Vec::with_capacity(self.parts.len());
        for index in 0..self.parts.len() {
            let Some(part) = self.parts.get_mut(index) else {
                continue;
            };
            let inner = part.element.build_layout(tree, theme, map);
            wrappers.push(self.wrap_part(index, tree, inner));
        }
        let node = tree.parent_node(self.container_style(), &wrappers);
        let _ = tree.set_restyled_for_width(
            node,
            self.styled_by_width() || self.parts.iter().any(|part| part.fit.aspect.is_some()),
        );
        self.node = Some(node);
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// The forms of what it holds line their labels up in one column, from what they show, and
    /// each part's wrapper follows its child in or out of the flow.
    fn size_before_solve(
        &mut self,
        tree: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) {
        if self.layout.shares_label_column() {
            let mut parts: Vec<&mut (dyn Widget + '_)> = self
                .parts
                .iter_mut()
                .map(|part| part.element.as_mut() as &mut (dyn Widget + '_))
                .collect();
            crate::widget::FormSection::share_label_column(&mut parts, tree, text);
        }
        let _ = self.rewrap(tree);
    }

    /// The layout settled from the width the container is solved at, and the lead kept where the
    /// column is wide enough for it; or, for one of its parts' wrappers, the height a ratio gives.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        _text: &mut TextRenderer,
    ) {
        let taffy::AvailableSpace::Definite(width) = width else {
            return;
        };
        let node = styles.node();
        if Some(node) != self.node {
            self.hold_aspect(styles, node, width);
            return;
        }
        if !self.styled_by_width() {
            self.hold_spanning_aspects(styles, width);
            return;
        }
        if self.lead > 0 {
            // What the column has left once the lead is reserved decides whether it is kept: the
            // box less one lead, which does not change with the answer.
            let left = (width - lead_width(self.lead, &self.resolved)).max(0.0);
            self.column = Some(WidthClass::with_history(
                left / self.resolved.rem,
                self.column,
            ));
        }
        let height = styles.last_layout(node).size.height;
        self.layout.settle(Size { width, height }, &self.resolved);
        let _ = self.restyle(styles);
        self.hold_spanning_aspects(styles, width);
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
        // Parts laid out again: what the old ones drew and what the new ones draw.
        if let Some(from) = self.relaid_from {
            regions.push(from);
            regions.push(self.rect);
        }
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

    /// What the part operations did, joined with what the children report. A child that replaced
    /// widgets of its own ([`StructureChange::Described`]) is laid out again with every part, under
    /// this container's node, once it has one: the rest of the window keeps its layout.
    fn take_structure_change(&mut self) -> StructureChange {
        let mut change = std::mem::take(&mut self.structure);
        let mut described = false;
        for part in &mut self.parts {
            match part.element.take_structure_change() {
                StructureChange::Described => described = true,
                child => change = change.join(child),
            }
        }
        if described {
            self.parts_changed();
            change = change.join(std::mem::take(&mut self.structure));
        }
        change
    }

    /// While its parts wait to be laid out again ([`Self::remount_children`]), none is named: the
    /// window lets go of every node under this container's.
    fn for_each_layout_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        self.relaid
    }

    fn relaid_layout_node(&self) -> Option<NodeId> {
        self.node.filter(|_| self.relaid)
    }

    /// Lay every part out again under this container's node, after the part operations changed
    /// them; otherwise carry out what the parts report.
    fn remount_children(&mut self, remount: &mut crate::widget::Remount<'_>) -> bool {
        if !self.relaid {
            return self
                .parts
                .iter_mut()
                .all(|part| part.element.remount_children(remount));
        }
        let Some(node) = self.node else {
            return false;
        };
        self.relaid = false;
        for child in remount.taffy().children(node).unwrap_or_default() {
            remount.unmount(child);
        }
        self.resolved =
            Resolved::of(remount.theme()).with_target(self.interaction.minimum_target());
        self.layout.items(self.parts.len());
        let mut wrappers = Vec::with_capacity(self.parts.len());
        for index in 0..self.parts.len() {
            let Some(part) = self.parts.get_mut(index) else {
                continue;
            };
            let inner = remount.mount(node, part.element.as_mut());
            wrappers.push(self.wrap_part(index, remount.taffy(), inner));
        }
        let _ = self.restyle(remount.taffy());
        remount.set_children(node, &wrappers).is_ok()
    }

    /// A layout paints nothing of its own (see [`Self::collect_damage`]), so it is
    /// dirty exactly when a shown child is. The default (`defaults::needs_redraw`) answers
    /// `true` for a widget with no children, which made an empty layout — a view whose
    /// every part is conditional and none stands — dirty forever, drawing a frame each time
    /// it was asked.
    fn needs_redraw(&self) -> bool {
        self.relaid_from.is_some()
            || self.parts.iter().enumerate().any(|(index, part)| {
                !self.layout.hidden(index)
                    && (part.element.needs_redraw() || part.element.needs_animation_frame())
            })
    }

    fn mark_drawn(&mut self) {
        self.relaid_from = None;
        crate::widget::defaults::mark_drawn(self.children_mut());
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
