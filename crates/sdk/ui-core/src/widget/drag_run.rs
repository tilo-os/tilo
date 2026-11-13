//! `DragRun`: a run of items — a dock's icons, a toolbar's tiles, a row of
//! cards — in which an item can be picked up and moved to another place, and
//! a resource from elsewhere (an app, a file, a search result) can be dropped
//! in.
//!
//! The gesture is Rust and runs at native refresh: a press starts a candidate,
//! pointer travel past the drag threshold lifts the item into a ghost, and the
//! place it would land is shown before it is let go, with an insertion line
//! across the run. Releasing reports **one** [`DragDrop`]: the key of the item
//! moved and the item it goes before, or the resource that was dropped and its
//! place. The owner turns that into one action on its model
//! (`items.move(item, before: next)`), so a reorder is attributed, permitted
//! and undone like every other change; the run then shows the item where the
//! model puts it.
//!
//! A host whose run takes drops that start in another widget (a resource
//! dragged from a search result onto a dock) says what is being dragged with
//! [`DragRun::set_foreign`]; the run then shows the same insertion line, and
//! [`DragRun::take_foreign`] answers where it would land when the host's drag
//! ends.
//!
//! A touchscreen holds an item to lift it and then drags; a drag that begins
//! without the hold scrolls what the run is in. Escape puts a lifted item down
//! where it was.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::dnd::{DRAG_START_THRESHOLD_PX, DragPayload};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::arrange::{Align, Distribute, Grid, Row as FlexRow, Stack as FlexStack};

use crate::component::SlotViews;
use crate::widget::DrawChild;
use crate::widget::{Element, TaffyTreeExt, Widget};

/// Which way a run goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunAxis {
    /// Leading to trailing: a dock, a toolbar.
    #[default]
    Horizontal,
    /// Top to bottom: a column of items.
    Vertical,
}

crate::component::choice!(RunAxis, "Axis", { Horizontal => "horizontal", Vertical => "vertical" });

impl RunAxis {
    /// The pointer's coordinate along this axis.
    fn along(self, point: Point) -> f64 {
        match self {
            Self::Horizontal => point.x,
            Self::Vertical => point.y,
        }
    }
}

/// The part of a run an item belongs to, as a bar or a dock is built: the items that lead
/// (a bar's mark and menus, a dock's launcher), those in the centre (a bar's clock), those
/// that trail (the people and the status), and the ones between groups, which push the groups
/// apart as a `spacer` does. A dock is one compact group at the centre of its edge, so its
/// items are all `Leading` and its run is packed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunGroup {
    /// The run's leading group, packed at the leading edge.
    #[default]
    Leading,
    /// The run's centre group, centred on the run.
    Center,
    /// The run's trailing group, packed at the trailing edge.
    Trailing,
    /// The room a group leaves the next: a part of the run that is only a gap.
    Space,
}

crate::component::choice!(RunGroup, "RunGroup", {
    Leading => "leading",
    Center => "center",
    Trailing => "trailing",
    Space => "space",
});

crate::component::choice!(crate::widget::arrange::Distribute, "Distribute", {
    Start => "start",
    Center => "center",
    End => "end",
    Between => "between",
});

crate::component::choice!(crate::widget::arrange::Align, "Align", {
    Start => "start",
    Center => "center",
    End => "end",
    Stretch => "stretch",
});

/// Where a dropped thing goes: the run that took it, what moved, and the
/// place it lands at, counted from the leading edge among the run's other
/// items.
///
/// A place, not a neighbour: that is what a collection's `move(x, to: i)`
/// takes, and it is resolved to the item it then comes before when the action
/// runs, so concurrent moves converge where each person meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragDrop {
    /// The key of the run that took the drop.
    pub run: String,
    /// What was dropped: the key of the item moved, or the resource dragged in.
    pub moved: String,
    /// The place it lands at among the run's other items, from 0.
    pub at: i64,
    /// The key of the item it lands before; `None` for the end of the run.
    pub before: Option<String>,
}

/// One item of a run.
pub struct RunItem {
    /// The stable key the item is known by, and what the run reports moving.
    pub key: String,
    /// The resource the item stands for, when it is one: what a drop reports.
    pub item: Option<crate::component::ResourceId>,
    /// The item's own label, for the ghost.
    pub label: String,
    /// The item's view.
    pub view: Element,
    /// The part of the run the item belongs to.
    pub group: RunGroup,
}

impl std::fmt::Debug for RunItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunItem")
            .field("key", &self.key)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The keys and ghost content of one item, in the order the model gave them.
struct Keys {
    key: String,
    label: String,
    /// The resource the item stands for, when it is one.
    item: Option<crate::component::ResourceId>,
    /// The part of the run the item belongs to.
    group: RunGroup,
}

/// A lifted item, from press to release.
struct Lift {
    key: String,
    /// The item's rect when it was lifted, and where in it the pointer took
    /// it.
    rect: Rect,
    grab: Point,
    /// The pointer's place when the press landed, for the drag threshold.
    start: Point,
    /// Whether it has travelled far enough to be a drag rather than a click.
    active: bool,
}

/// What a drag over the run would land on: the item it goes before, and the
/// line drawn there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Landing {
    /// The place among the run's other items.
    at: i64,
    before: Option<String>,
}

/// See the [module docs](self).
pub struct DragRun {
    id: String,
    name: Option<String>,
    axis: RunAxis,
    /// Where the items pack in the room the run is given: the whole run's place when it holds
    /// no groups, the groups' place within it when it does.
    distribute: Distribute,
    /// Where the items pack across the run.
    align: Align,
    /// The items' views, in order, built into [`Self::items`] at the first
    /// layout.
    views: Vec<Element>,
    /// Views after the run's items at its trailing end, which the run never moves: the time
    /// and the device's status beside a taskbar's items.
    trailing: Vec<Element>,
    /// The items, in order: a `Row` or a `Column` by the axis.
    items: Option<Box<dyn Widget>>,
    /// The groups of items as [`Self::build_items`] arranged them: for each child of
    /// [`Self::items`], the items it holds, `None` for the room between two groups. Each
    /// item's rect is read from the widget `Self::item_rects` reaches through it.
    groups: Vec<Option<Vec<usize>>>,
    /// Whether the items stand one level down, inside `Self::groups`' children (a grouped
    /// bar's columns), rather than as the container's own children (a packed run).
    nested: bool,
    keys: Vec<Keys>,
    lift: Option<Lift>,
    cursor: Point,
    /// What another widget is dragging over the run, and its ghost's label.
    foreign: Option<(DragPayload, String)>,
    /// Where a foreign drag would land, while it is over the run.
    landing: Option<Landing>,
    /// What the run dropped since the host last read it, and what to tell the
    /// owner of each, when the run was built from a described component.
    drops: Vec<DragDrop>,
    reporter: Option<Box<dyn FnMut(&DragDrop)>>,
    /// Called with the key of an item pressed and let go without moving.
    activated: Option<Box<dyn FnMut(&str)>>,
    /// What inputs the host has, as the window told the run before its items existed: the
    /// items are built at the first layout, and a `Widget::set_interaction_environment` on the
    /// run reaches no child until then. Kept and given to the items as they are built, so a
    /// bar's or a dock's controls are a finger's target under a coarse pointer, as any
    /// button's are.
    interaction: crate::interaction::InteractionEnvironment,
    /// The item whose view a host is changing, in [`SlotViews`]' own terms.
    editing: Option<usize>,
    dirty: bool,
}

impl DragRun {
    /// A run `id` (its test id), its items leading to trailing.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            axis: RunAxis::Horizontal,
            distribute: Distribute::Start,
            align: Align::Center,
            views: Vec::new(),
            trailing: Vec::new(),
            items: None,
            groups: Vec::new(),
            nested: false,
            keys: Vec::new(),
            lift: None,
            cursor: Point::ZERO,
            foreign: None,
            landing: None,
            drops: Vec::new(),
            reporter: None,
            activated: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
            editing: None,
            dirty: false,
        }
    }

    /// A run that goes `axis`.
    #[must_use]
    pub fn axis(mut self, axis: RunAxis) -> Self {
        self.axis = axis;
        self.items = None;
        self
    }

    /// Where the run's items pack along it: its leading edge (the default), its centre, its
    /// trailing edge, or spread between its ends. A run whose items fall in groups packs each
    /// group at its own edge whatever this says; a dock — one group — is centred by
    /// `Distribute::Center`.
    #[must_use]
    pub fn distribute(mut self, distribute: Distribute) -> Self {
        self.distribute = distribute;
        self.items = None;
        self
    }

    /// Where the run's items pack across it.
    #[must_use]
    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self.items = None;
        self
    }

    /// Views after the run's items at its trailing end, which the run holds still: what a
    /// taskbar shows beside its items (the time, the device's status). They are not items: a
    /// drag never lifts them and a drop never lands among them.
    #[must_use]
    pub fn trailing(mut self, views: Vec<Element>) -> Self {
        self.trailing = views;
        self.items = None;
        self
    }

    /// What the run is, for assistive technology.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Each item's key and the resource it stands for, when it is one.
    #[must_use]
    pub fn item_ids(&self) -> Vec<(String, Option<crate::component::ResourceId>)> {
        self.keys
            .iter()
            .map(|keys| (keys.key.clone(), keys.item.clone()))
            .collect()
    }

    /// Add `item` at the end of the run.
    #[must_use]
    pub fn push(mut self, item: RunItem) -> Self {
        self.keys.push(Keys {
            key: item.key,
            label: item.label,
            item: item.item,
            group: item.group,
        });
        self.views.push(item.view);
        self
    }

    /// Take the items of a run whose keys stay, in place: what each item says about itself
    /// changes where it stands, and its view changes where it stands, so the run keeps what
    /// those views hold (a menu a person opened on the bar) rather than being built again.
    /// `false` when these are other items: one key other, or another order.
    pub(crate) fn take_items(&mut self, items: Vec<RunItem>) -> bool {
        if items.len() != self.keys.len()
            || items
                .iter()
                .zip(&self.keys)
                .any(|(item, keys)| item.key != keys.key)
        {
            return false;
        }
        let mut views = Vec::with_capacity(items.len());
        self.keys = items
            .into_iter()
            .map(|item| {
                views.push(item.view);
                Keys {
                    key: item.key,
                    label: item.label,
                    item: item.item,
                    group: item.group,
                }
            })
            .collect();
        if self.items.is_none() {
            // Nothing is laid out yet: the views it holds are the ones it takes.
            self.views = views;
        }
        true
    }

    /// What the run was told it is.
    pub(crate) fn name_of(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The way it runs.
    pub(crate) fn axis_of(&self) -> RunAxis {
        self.axis
    }

    /// Where its items pack along it.
    pub(crate) fn distribute_of(&self) -> Distribute {
        self.distribute
    }

    /// Where its items pack across it.
    pub(crate) fn align_of(&self) -> Align {
        self.align
    }

    /// The item known by `key`, by its place in the run.
    pub(crate) fn index_of(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|keys| keys.key == key)
    }

    /// Change the view of the item at `index`, in [`SlotViews`]' own terms.
    pub(crate) fn edit_item(&mut self, index: Option<usize>) {
        self.editing = index;
    }

    /// Presses and let go without moving go to `activated` from now on.
    pub(crate) fn replace_activate(&mut self, activated: Option<Box<dyn FnMut(&str)>>) {
        self.activated = activated;
    }

    /// Drops go to `reporter` from now on.
    pub(crate) fn replace_reporter(&mut self, reporter: Option<Box<dyn FnMut(&DragDrop)>>) {
        self.reporter = reporter;
    }

    /// The row an item's view stands in, where it stands.
    fn item_row_mut(&mut self, index: usize) -> Option<&mut crate::widget::arrange::Row> {
        let at = self
            .groups
            .iter()
            .position(|members| members.as_ref().is_some_and(|held| held.contains(&index)))?;
        let inside = self.groups[at]
            .as_ref()?
            .iter()
            .position(|held| *held == index)?;
        let items = self.items.as_mut()?;
        if self.nested {
            let columns = items
                .as_any_mut()?
                .downcast_mut::<crate::widget::arrange::Grid>()?;
            let row = columns.child_mut(at)?;
            let row = row
                .as_any_mut()?
                .downcast_mut::<crate::widget::arrange::Row>()?;
            row.child_mut(inside)?
                .as_any_mut()?
                .downcast_mut::<crate::widget::arrange::Row>()
        } else {
            let rows = items
                .as_any_mut()?
                .downcast_mut::<crate::widget::arrange::Row>()?;
            rows.child_mut(at)?
                .as_any_mut()?
                .downcast_mut::<crate::widget::arrange::Row>()
        }
    }

    /// The view of the item at `index`, where it stands.
    fn item_view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if self.items.is_none() {
            return self.views.get_mut(index);
        }
        self.item_row_mut(index)?.child_mut(0)
    }

    /// Show `view` as the view of the item being changed instead of the one it holds.
    fn put_item_view(&mut self, view: Element) {
        let Some(index) = self.editing else {
            return;
        };
        if self.items.is_none() {
            if let Some(held) = self.views.get_mut(index) {
                *held = view;
            }
            return;
        }
        let Some(row) = self.item_row_mut(index) else {
            return;
        };
        <crate::widget::arrange::Row as SlotViews>::replace(row, 0, view);
        self.dirty = true;
    }

    /// The run's key, as a drop names it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether an item is lifted now.
    #[must_use]
    pub fn is_lifting(&self) -> bool {
        self.lift.as_ref().is_some_and(|lift| lift.active)
    }

    /// What another widget is dragging over this run: `payload`, shown in the
    /// ghost as `label`. The run then shows where it would land, and
    /// [`Self::take_foreign`] answers it when the drag ends.
    pub fn set_foreign(&mut self, foreign: Option<(DragPayload, String)>) {
        if self
            .foreign
            .as_ref()
            .map(|(p, l)| (format!("{p:?}"), l.clone()))
            != foreign.as_ref().map(|(p, l)| (format!("{p:?}"), l.clone()))
        {
            self.foreign = foreign;
            self.landing = None;
            self.dirty = true;
        }
    }

    /// Whether a foreign drag is over the run now.
    #[must_use]
    pub fn foreign_is_over(&self) -> bool {
        self.landing.is_some()
    }

    /// Where the foreign drag over the run would land, once its host ends it:
    /// the resource and the item it goes before.
    #[must_use]
    pub fn take_foreign(&mut self) -> Option<DragDrop> {
        let (payload, _) = self.foreign.take()?;
        let landing = self.landing.take()?;
        self.dirty = true;
        let DragPayload::Custom { kind, blob } = payload else {
            return None;
        };
        if kind != ITEM_PAYLOAD {
            return None;
        }
        Some(DragDrop {
            run: self.id.clone(),
            moved: String::from_utf8(blob).ok()?,
            at: landing.at,
            before: landing.before,
        })
    }

    /// What the run dropped since the host last read it: an item moved, or a
    /// resource dropped on it.
    pub fn take_drops(&mut self) -> Vec<DragDrop> {
        std::mem::take(&mut self.drops)
    }

    /// Each drop goes to `report`, as a described component's event: a host
    /// that keeps its own drops instead reads them with [`Self::take_drops`].
    #[must_use]
    pub fn on_drop(mut self, report: impl FnMut(&DragDrop) + 'static) -> Self {
        self.reporter = Some(Box::new(report));
        self
    }

    /// Each item pressed and let go without moving goes to `activated`, by its
    /// key: the item's view does what its press does.
    #[must_use]
    pub fn on_activate(mut self, activated: impl FnMut(&str) + 'static) -> Self {
        self.activated = Some(Box::new(activated));
        self
    }

    /// Send each drop to the reporter, if it has one.
    fn report(&mut self) {
        let Some(mut reporter) = self.reporter.take() else {
            return;
        };
        for drop in std::mem::take(&mut self.drops) {
            reporter(&drop);
        }
        self.reporter = Some(reporter);
    }

    /// What a lifted item hands another widget when the pointer leaves the
    /// run, carrying the item's key.
    fn payload_of(&self, key: &str) -> DragPayload {
        DragPayload::Custom {
            kind: ITEM_PAYLOAD.to_owned(),
            blob: key.as_bytes().to_vec(),
        }
    }

    /// The container this run's items go in, by its axis, and the items each of its children
    /// holds.
    ///
    /// A run is its items at their own size, one theme sibling gap apart — never an equal
    /// share of its length. A run whose items fall in groups (a bar's mark and menus leading,
    /// its clock centred, its people and status trailing) is three columns, so the centre group
    /// stands in the middle whatever the side groups weigh; a run of one group (a dock) packs
    /// that group as `distribute` and `align` say. A part that is only a gap (`RunGroup::Space`)
    /// draws nothing: the grouped run's columns are the room between its groups.
    fn build_items(
        &mut self,
        views: Vec<Element>,
    ) -> (Box<dyn Widget>, Vec<Option<Vec<usize>>>, bool) {
        let mut views: Vec<Option<Element>> = views.into_iter().map(Some).collect();
        // Each item is its own hugging row: its view keeps the size it takes, and the run
        // packs items by the theme's sibling gap, never by an equal share of its length.
        let item = |view: Element| -> Element {
            Box::new(
                FlexRow::row()
                    .gap(Rhythm::Tight)
                    .align(self.align)
                    .push(view),
            )
        };
        let members = |views: &Vec<Option<Element>>, group: RunGroup| -> Vec<usize> {
            self.keys
                .iter()
                .enumerate()
                .filter(|(index, keys)| {
                    keys.group == group && views.get(*index).is_some_and(Option::is_some)
                })
                .map(|(index, _)| index)
                .collect()
        };
        let mut trailing = std::mem::take(&mut self.trailing);
        let grouped = !trailing.is_empty()
            || self
                .keys
                .iter()
                .any(|keys| matches!(keys.group, RunGroup::Center | RunGroup::Trailing));
        // One group's items, packed at its edge in its own row, filling the room the run gives
        // it.
        let pack = |views: &mut Vec<Option<Element>>,
                    indices: Vec<usize>,
                    distribute: Distribute,
                    trailing: &mut Vec<Element>|
         -> (Element, Vec<usize>) {
            let mut row = FlexRow::row()
                .fill_width()
                .gap(Rhythm::Sibling)
                .align(self.align)
                .distribute(distribute);
            let mut held = Vec::with_capacity(indices.len());
            for index in indices {
                let Some(view) = views.get_mut(index).and_then(Option::take) else {
                    continue;
                };
                row = row.push(item(view));
                held.push(index);
            }
            for view in std::mem::take(trailing) {
                row = row.push(item(view));
            }
            (Box::new(row), held)
        };
        match self.axis {
            RunAxis::Horizontal if grouped => {
                // Three columns: each group stands at its own edge of the run, and the centre
                // group in the middle whatever the side groups weigh. A column never gives a
                // group less room than its items take, so a part is never squeezed into a
                // sliver of itself; a run too narrow for its parts overflows whole, which its
                // owner sees and gives parts up for.
                let mut groups = Vec::with_capacity(3);
                let mut columns = Grid::columns_keeping_content(3).gap(Rhythm::Tight);
                for (group, distribute) in [
                    (RunGroup::Leading, Distribute::Start),
                    (RunGroup::Center, Distribute::Center),
                    (RunGroup::Trailing, Distribute::End),
                ] {
                    let indices = members(&views, group);
                    let mut none = Vec::new();
                    let held_trailing = group == RunGroup::Trailing;
                    let has_fixed = held_trailing && !trailing.is_empty();
                    let (row, held) = pack(
                        &mut views,
                        indices,
                        distribute,
                        if held_trailing {
                            &mut trailing
                        } else {
                            &mut none
                        },
                    );
                    // A group that draws nothing at all is its column's room, not a row: an
                    // empty row would ask for a repaint every frame.
                    let empty = held.is_empty() && !has_fixed;
                    groups.push((!held.is_empty()).then_some(held));
                    if empty {
                        columns = columns.push(Box::new(crate::widget::Spacer::new()));
                    } else {
                        columns = columns.push(row);
                    }
                }
                (Box::new(columns), groups, true)
            }
            RunAxis::Horizontal => {
                let all: Vec<usize> = (0..self.keys.len()).collect();
                let (row, held) = pack(&mut views, all, self.distribute, &mut trailing);
                // The packed row's children are the items themselves, one each.
                let groups = held.iter().map(|index| Some(vec![*index])).collect();
                (row, groups, false)
            }
            RunAxis::Vertical => {
                let all: Vec<usize> = (0..self.keys.len()).collect();
                let mut column = FlexStack::stack().gap(Rhythm::Sibling).align(self.align);
                let mut held = Vec::with_capacity(all.len());
                for index in all {
                    let Some(view) = views.get_mut(index).and_then(Option::take) else {
                        continue;
                    };
                    column = column.push(item(view));
                    held.push(index);
                }
                for view in trailing {
                    column = column.push(item(view));
                }
                let groups = held.iter().map(|index| Some(vec![*index])).collect();
                (Box::new(column), groups, false)
            }
        }
    }

    /// Each item's rect, in order.
    fn item_rects(&self) -> Vec<Rect> {
        let Some(items) = &self.items else {
            return Vec::new();
        };
        let mut rects = vec![Rect::ZERO; self.keys.len()];
        for (child, members) in items.children().iter().zip(&self.groups) {
            let Some(members) = members else {
                continue;
            };
            if self.nested {
                for (&index, rect) in members.iter().zip(child.children().iter()) {
                    if let Some(slot) = rects.get_mut(index) {
                        *slot = rect.get_rect();
                    }
                }
            } else if let [index] = members.as_slice() {
                if let Some(slot) = rects.get_mut(*index) {
                    *slot = child.get_rect();
                }
            }
        }
        rects
    }

    /// The run's whole extent, for a drop to land in.
    #[must_use]
    pub fn extent(&self) -> Rect {
        self.get_rect()
    }

    /// The run's items' extent, from their rects: the items that draw, so a part that is only
    /// a gap never stretches it to the run's origin.
    fn band(&self) -> Option<Rect> {
        self.item_rects()
            .into_iter()
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            .reduce(|band, rect| band.union(rect))
    }

    /// The item under `point`, by its place in the run.
    fn item_at(&self, point: Point) -> Option<usize> {
        self.item_rects()
            .into_iter()
            .position(|rect| rect.width() > 0.0 && rect.contains(point))
    }

    /// The item a pointer at `point` lands before, or `None` when it is off
    /// the run.
    ///
    /// Measured against each item's middle, so a half-item is the dead zone
    /// every reorder uses: the line only moves once the pointer crosses a
    /// neighbour's middle. One item's reach past either end lets an item be
    /// dropped just outside the run, which is where a dock's last place is.
    fn target_at(&self, point: Point, lifted: Option<&str>) -> Option<Landing> {
        let rects = self.item_rects();
        let band = self.band()?;
        let pitch = match self.axis {
            RunAxis::Horizontal => band.width(),
            RunAxis::Vertical => band.height(),
        } / rects.len().max(1) as f64;
        let reach = rects
            .iter()
            .find(|rect| rect.width() > 0.0)
            .map_or(0.0, |rect| match self.axis {
                RunAxis::Horizontal => rect.width(),
                RunAxis::Vertical => rect.height(),
            });
        let band = match self.axis {
            RunAxis::Horizontal => Rect::new(band.x0, band.y0, band.x1 + pitch.min(reach), band.y1),
            RunAxis::Vertical => Rect::new(band.x0, band.y0, band.x1, band.y1 + pitch.min(reach)),
        };
        if !band.contains(point) {
            return None;
        }
        let along = self.axis.along(point);
        // The items that draw, in the order they stand along the run: a grouped run shows its
        // centre and trailing groups where their group stands, not where the model lists them.
        let mut shown: Vec<(usize, Rect)> = rects
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, rect)| rect.width() > 0.0)
            .collect();
        shown.sort_by(|a, b| {
            self.axis
                .along(a.1.center())
                .total_cmp(&self.axis.along(b.1.center()))
        });
        let before = shown
            .iter()
            .find(|(_, rect)| along < self.axis.along(rect.center()))
            .map(|(index, _)| *index);
        // The place among the run's other items: where the item it lands before stands once
        // the lifted one is out of the count.
        let at = match before {
            Some(target) => self
                .keys
                .iter()
                .enumerate()
                .filter(|(index, keys)| *index < target && Some(keys.key.as_str()) != lifted)
                .count(),
            None => self
                .keys
                .iter()
                .filter(|keys| Some(keys.key.as_str()) != lifted)
                .count(),
        };
        Some(Landing {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "a run's items, far fewer than i64::MAX"
            )]
            at: at as i64,
            before: before.map(|index| self.keys[index].key.clone()),
        })
    }

    /// The insertion line for `landing`, across the run: at the leading edge of the item it
    /// lands before, or past the last item that draws for the end.
    fn line_rect(&self, landing: &Landing, theme: &Theme) -> Option<Rect> {
        let gap = f64::from(theme.tokens.spacing.s2);
        let line = f64::from(theme.tokens.metrics.drag_line_px).max(1.0);
        let rects = self.item_rects();
        let band = self.band()?;
        let drawn = |index: usize| rects.get(index).copied().filter(|r| r.width() > 0.0);
        let at = landing
            .before
            .as_ref()
            .and_then(|key| self.keys.iter().position(|keys| &keys.key == key))
            .and_then(drawn);
        let last = rects
            .iter()
            .copied()
            .filter(|r| r.width() > 0.0)
            .reduce(|a, b| {
                let past = |r: Rect| match self.axis {
                    RunAxis::Horizontal => r.x1,
                    RunAxis::Vertical => r.y1,
                };
                if past(a) >= past(b) { a } else { b }
            });
        Some(match self.axis {
            RunAxis::Horizontal => {
                let x = at.map_or_else(
                    || last.map_or(band.x0, |r| r.x1) + gap / 2.0,
                    |r| r.x0 - gap / 2.0,
                );
                Rect::new(x - line / 2.0, band.y0, x + line / 2.0, band.y1)
            }
            RunAxis::Vertical => {
                let y = at.map_or_else(
                    || last.map_or(band.y0, |r| r.y1) + gap / 2.0,
                    |r| r.y0 - gap / 2.0,
                );
                Rect::new(band.x0, y - line / 2.0, band.x1, y + line / 2.0)
            }
        })
    }

    /// Take the press at `point`: lift the item under it, if there is one.
    fn press(&mut self, point: Point) -> bool {
        let Some(at) = self.item_at(point) else {
            return false;
        };
        let (Some(keys), Some(rect)) = (self.keys.get(at), self.item_rects().get(at).copied())
        else {
            return false;
        };
        self.lift = Some(Lift {
            key: keys.key.clone(),
            rect,
            grab: Point::new(point.x - rect.x0, point.y - rect.y0),
            start: point,
            active: false,
        });
        self.dirty = true;
        true
    }

    /// Follow the pointer while something is lifted: past the threshold the
    /// drag starts, and the landing it would take moves with it.
    fn travel(&mut self, point: Point) {
        self.cursor = point;
        let Some(lift) = self.lift.as_mut() else {
            return;
        };
        if !lift.active && lift.start.distance(point) > DRAG_START_THRESHOLD_PX {
            lift.active = true;
        }
        if !lift.active {
            return;
        }
        let key = lift.key.clone();
        let label = self
            .keys
            .iter()
            .find(|keys| keys.key == key)
            .map_or_else(String::new, |keys| keys.label.clone());
        self.landing = self.target_at(point, Some(&key));
        self.dirty = true;
        // Every other run in the window may take this item now, and this one
        // shows where it lands.
        crate::widget::drag_bus::publish(crate::widget::drag_bus::InFlight {
            source: self.id.clone(),
            payload: self.payload_of(&key),
            label,
            pointer: point,
            runs: vec![(self.id.clone(), self.extent())],
        });
    }

    /// Put a lifted item down where it was.
    fn cancel(&mut self) {
        if self.is_lifting() {
            crate::widget::drag_bus::end(None);
        }
        self.lift = None;
        self.landing = None;
        self.dirty = true;
    }

    /// Let go of a lifted item: report the one move it makes, if it makes one, and whether the
    /// press was only a click — the item's own view took it, so nothing moves.
    fn release(&mut self) -> (Option<DragDrop>, bool) {
        let Some(lift) = self.lift.take() else {
            return (None, false);
        };
        let landing = self.landing.take();
        self.dirty = true;
        if !lift.active {
            // A press that never moved is a click: the item's own view takes the release, and
            // nothing moves.
            return (None, true);
        }
        // Let go: another run whose extent holds the pointer takes it, or
        // nothing does.
        let landed = crate::widget::drag_bus::Landed {
            source: self.id.clone(),
            payload: self.payload_of(&lift.key),
            pointer: self.cursor,
        };
        let Some(landing) = landing else {
            // Let go outside this run: another run whose extent holds the
            // pointer takes it, or nothing does.
            crate::widget::drag_bus::end(Some(landed));
            return (None, false);
        };
        crate::widget::drag_bus::end(None);
        let Some(at) = self.keys.iter().position(|keys| keys.key == lift.key) else {
            return (None, false);
        };
        let next = match &landing.before {
            Some(before) => {
                let Some(next) = self.keys.iter().position(|keys| &keys.key == before) else {
                    return (None, false);
                };
                next
            }
            None => self.keys.len(),
        };
        // A place it already holds moves nothing.
        if next == at || next == at.saturating_add(1) {
            return (None, false);
        }
        (
            Some(DragDrop {
                run: self.id.clone(),
                moved: lift.key,
                at: landing.at,
                before: landing.before,
            }),
            false,
        )
    }

    fn paint(&self, scene: &mut Scene, transform: Affine, _text: &mut TextRenderer, theme: &Theme) {
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.control);
        let line_px = f64::from(theme.tokens.metrics.drag_line_px).max(1.0);
        // The line marks where what is dragged would land: the run's own
        // item while it is lifted, and a resource another widget drags over
        // it.
        let landing = if self.is_lifting() || self.foreign.is_some() {
            self.landing.as_ref()
        } else {
            None
        };
        if let Some(landing) = landing
            && let Some(line) = self.line_rect(landing, theme)
        {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent,
                None,
                &RoundedRect::from_rect(line, line_px / 2.0),
            );
        }
        // The ghost, following the pointer where the press took it: the
        // item's own size while the run's item is lifted, one target while a
        // resource from elsewhere is over it.
        let (size, grab) = if let (true, Some(lift)) = (self.is_lifting(), self.lift.as_ref()) {
            (lift.rect.size(), lift.grab)
        } else if self.foreign.is_some() && self.landing.is_some() {
            let target = f64::from(theme.tokens.metrics.min_pointer_target);
            (
                vello::kurbo::Size::new(target, target),
                Point::new(target / 2.0, target / 2.0),
            )
        } else {
            return;
        };
        let origin = Point::new(self.cursor.x - grab.x, self.cursor.y - grab.y);
        let shape = RoundedRect::from_rect(Rect::from_origin_size(origin, size), radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(line_px),
            transform,
            colors.accent,
            None,
            &shape,
        );
    }
}

/// The payload kind a run's item travels as.
pub const ITEM_PAYLOAD: &str = "drag-run-item";

impl Widget for DragRun {
    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.report();
        // A drag from another run that was let go inside this one is a drop
        // here: one action on the owner's model, as a move of its own is.
        if let Some(landed) = crate::widget::drag_bus::take(&self.id, self.extent()) {
            if let Some(landing) = self.target_at(landed.pointer, None)
                && let crate::dnd::DragPayload::Custom { kind, blob } = landed.payload
                && kind == ITEM_PAYLOAD
                && let Ok(moved) = String::from_utf8(blob)
            {
                self.drops.push(DragDrop {
                    run: self.id.clone(),
                    moved,
                    at: landing.at,
                    before: landing.before,
                });
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.items.is_none() {
            let views = std::mem::take(&mut self.views);
            let (items, groups, nested) = self.build_items(views);
            self.items = Some(items);
            self.groups = groups;
            self.nested = nested;
            // The items are built now, after any input the window gave the run: a coarse
            // pointer makes their controls a finger's target.
            if let Some(items) = self.items.as_mut() {
                Widget::set_interaction_environment(&mut **items, self.interaction);
            }
        }
        let Some(items) = self.items.as_mut() else {
            return taffy.widget_leaf(Style::default(), node_map, self);
        };
        let node = Widget::build_layout(&mut **items, taffy, theme, node_map);
        // The run takes the width it is given, never more: a bar's content stretches it across
        // the edge, and a dock's packed group hugs its items where the run is given no width to
        // fill. A percentage width would collapse to nothing inside a hugging parent (a dock
        // beside nothing), and the run would then stand at its parent's middle with its items
        // spilling both ways.
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            style.min_size.width = taffy::Dimension::length(0.0_f32);
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(items) = &mut self.items {
            Widget::apply_layout(&mut **items, taffy, node, origin);
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
        if let Some(items) = &mut self.items {
            DrawChild::draw_child(&mut **items, scene, transform, text, theme, presence);
        }
        self.paint(scene, transform, text, theme);
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self
                .items
                .as_ref()
                .is_some_and(|items| Widget::needs_redraw(&**items))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.lift.is_some()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = point;
                if self.lift.is_some() {
                    self.travel(point);
                    // A drag that started cancels the press its item took, so
                    // letting go does not also activate it.
                    if self.is_lifting() {
                        let away = WindowEvent::CursorLeft {
                            device_id: winit::event::DeviceId::dummy(),
                        };
                        if let Some(items) = &mut self.items {
                            Widget::handle_event(&mut **items, &away, ctx);
                        }
                    }
                    *ctx.handled = true;
                    return;
                }
                if self.foreign.is_some() {
                    self.landing = self.target_at(point, None);
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.foreign.is_some() {
                    self.landing = None;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The press is the item's first: a button in it takes the press as any button
                // does, so letting go without moving presses it.
                if let Some(items) = &mut self.items {
                    Widget::handle_event(&mut **items, event, ctx);
                }
                if self.press(self.cursor) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.lift.is_some() => {
                let (drop, click) = self.release();
                if let Some(drop) = drop {
                    self.drops.push(drop);
                }
                // A press that travelled nowhere is the item's own press: its view takes the
                // release too, so a button on a run's item works like any button.
                if !click {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                    && self.lift.is_some() =>
            {
                self.cancel();
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        if let Some(items) = &mut self.items {
            Widget::handle_event(&mut **items, event, ctx);
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.item_at(point).map(|_| winit::window::CursorIcon::Grab)
    }

    fn get_rect(&self) -> Rect {
        self.items
            .as_ref()
            .map_or(Rect::ZERO, |items| Widget::get_rect(&**items))
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        self.name
            .as_ref()
            .map(|name| AccessNode::new(Role::Group).with_optional_name(Some(name.clone())))
    }

    fn name(&self) -> &'static str {
        "DragRun"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.items.iter().map(|items| &**items).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.items
            .iter_mut()
            .map(|items| items.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    /// The items are no children until the first layout builds them, so the input the window
    /// gives the run is kept here and handed to them as they are built (see
    /// [`Self::interaction`]).
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        if let Some(items) = self.items.as_mut() {
            Widget::set_interaction_environment(&mut **items, environment);
        }
    }
}

/// The view of the item a host is changing, where the item stands: the view in the slot its
/// run gave it, so a part of the bar changes what it shows without the bar being built again.
impl SlotViews for DragRun {
    fn len(&self) -> usize {
        usize::from(self.editing.is_some())
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        let at = self.editing?;
        self.item_view_mut(at)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put_item_view(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put_item_view(Box::new(crate::widget::Spacer::token(
            crate::tokens::SpacingStep::S0,
        )));
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put_item_view(view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Container, Spacer};

    fn item(key: &str, width: f32) -> RunItem {
        RunItem {
            key: key.to_owned(),
            label: key.to_owned(),
            item: None,
            group: RunGroup::Leading,
            view: Box::new(
                Container::new()
                    .with_size(width, 48.0)
                    .with_child(Box::new(Spacer::fixed(1.0))),
            ),
        }
    }

    fn group_item(key: &str, width: f32, group: RunGroup) -> RunItem {
        RunItem {
            group,
            ..item(key, width)
        }
    }

    /// A run of items of different widths, each in its own group.
    fn bar() -> DragRun {
        DragRun::new("bar")
            .name("Bar")
            .push(group_item("mark", 24.0, RunGroup::Leading))
            .push(group_item("menu", 90.0, RunGroup::Leading))
            .push(group_item("clock", 50.0, RunGroup::Center))
            .push(group_item("people", 70.0, RunGroup::Trailing))
            .push(group_item("status", 24.0, RunGroup::Trailing))
    }

    /// The bar a person's shell shows is a run: a host taking its parts' changing views in
    /// place reaches it by its own type, or every tick of the clock builds the bar again.
    #[test]
    fn the_run_answers_its_own_type() {
        let mut run: Element = Box::new(DragRun::new("bar").name("Bar").push(item("a", 24.0)));
        assert!(
            run.as_any().is_some_and(|any| any.is::<DragRun>())
                && run.as_any_mut().is_some_and(|any| any.is::<DragRun>()),
            "a run is a widget the adapter reaches by its type"
        );
    }

    fn harness(run: DragRun) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::for_root(crate::State::new(), run, (600, 200), Theme::dark())
    }

    fn run() -> DragRun {
        DragRun::new("dock")
            .name("Dock")
            .push(item("a", 48.0))
            .push(item("b", 48.0))
            .push(item("c", 48.0))
            .push(item("d", 48.0))
    }

    /// Every item keeps the width its view takes, packed one sibling gap apart: a run never
    /// gives its items an equal share of its length.
    #[test]
    fn a_run_is_its_items_at_their_own_size_one_gap_apart() {
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let host = harness(
            DragRun::new("dock")
                .push(item("a", 48.0))
                .push(item("b", 80.0))
                .push(item("c", 24.0)),
        );
        let rects = host.root::<DragRun>().item_rects();
        assert_eq!(rects.len(), 3);
        assert!((rects[0].width() - 48.0).abs() < 0.5, "{rects:?}");
        assert!((rects[1].width() - 80.0).abs() < 0.5, "{rects:?}");
        assert!((rects[2].width() - 24.0).abs() < 0.5, "{rects:?}");
        for pair in rects.windows(2) {
            let [left, right] = pair else { continue };
            assert!(
                (right.x0 - left.x1 - gap).abs() < 0.5,
                "one sibling gap between items: {rects:?}"
            );
        }
    }

    /// A bar's groups stand at their own edges: the leading group at the leading edge, the
    /// clock in the middle, the people and the status at the trailing edge — each packed at
    /// its own items' size, the groups' room between them.
    #[test]
    fn a_grouped_run_puts_its_groups_at_their_own_edges() {
        let width = 1280.0;
        let host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            bar(),
            (width as u32, 200),
            Theme::dark(),
        );
        let rects = host.root::<DragRun>().item_rects();
        assert_eq!(rects.len(), 5);
        assert!((rects[0].x0).abs() < 0.5, "the mark leads: {rects:?}");
        assert!(
            (rects[1].x0 - rects[0].x1 - 12.0).abs() < 0.5,
            "the menu follows the mark a sibling gap on: {rects:?}"
        );
        let clock = rects[2];
        assert!(
            (clock.center().x - f64::from(width) / 2.0).abs() < 1.0,
            "the clock stands in the middle: {rects:?}"
        );
        assert!(
            (rects[4].x1 - f64::from(width)).abs() < 0.5,
            "the status ends at the trailing edge: {rects:?}"
        );
        assert!(
            rects[4].x0 >= rects[3].x1,
            "the status follows the people: {rects:?}"
        );
    }

    /// A dock is one compact group: its items pack together where the run's `distribute`
    /// says, and `Distribute::Center` centres them.
    #[test]
    fn a_dock_packs_its_items_where_its_distribute_says() {
        let width = 1280.0;
        let host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            DragRun::new("dock")
                .distribute(crate::widget::arrange::Distribute::Center)
                .push(item("a", 48.0))
                .push(item("b", 48.0))
                .push(item("c", 48.0)),
            (width as u32, 200),
            Theme::dark(),
        );
        let rects = host.root::<DragRun>().item_rects();
        let left = rects[0].x0;
        let right = rects[2].x1;
        assert!(
            (left - (f64::from(width) - right)).abs() < 1.0,
            "equal room at both ends: {rects:?}"
        );
        assert!(
            (rects[1].x0 - rects[0].x1 - 12.0).abs() < 0.5,
            "packed a sibling gap apart: {rects:?}"
        );
    }

    /// What stands beside a taskbar's items is not an item: a press on it lifts nothing, and
    /// a dragged item never lands among it.
    #[test]
    fn what_stands_beside_the_items_is_not_an_item() {
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            DragRun::new("taskbar")
                .push(item("a", 48.0))
                .push(item("b", 48.0))
                .trailing(vec![Box::new(
                    Container::new()
                        .with_size(80.0, 24.0)
                        .with_child(Box::new(Spacer::fixed(1.0))),
                )]),
            (600, 200),
            Theme::dark(),
        );
        let rects = host.root::<DragRun>().item_rects();
        assert_eq!(rects.len(), 2, "the trailing views are not items");
        let trailing = host
            .root::<DragRun>()
            .children()
            .first()
            .map(|grid| grid.children().len())
            .unwrap_or_default();
        assert_eq!(trailing, 3, "the trailing column holds what stands beside");
        let a = rects[0];
        // Dropping past the trailing views lands after the last item, not among them.
        host.drag_at(
            (a.center().x, a.center().y),
            (a.center().x + 400.0, a.center().y),
        );
        assert!(host.root_mut::<DragRun>().take_drops().is_empty());
    }

    #[test]
    fn dragging_an_item_past_the_second_reports_one_move_before_it() {
        let mut host = harness(run());
        let rects = host.root::<DragRun>().item_rects();
        let (a, c) = (rects[0], rects[2]);
        host.drag_at((a.center().x, a.center().y), (c.x0 + 2.0, c.center().y));
        let drops = host.root_mut::<DragRun>().take_drops();
        assert_eq!(
            drops,
            [DragDrop {
                run: "dock".to_owned(),
                moved: "a".to_owned(),
                at: 1,
                before: Some("c".to_owned()),
            }]
        );
        assert!(!host.root::<DragRun>().is_lifting());
    }

    #[test]
    fn a_click_moves_nothing() {
        let mut host = harness(run());
        let a = host.root::<DragRun>().item_rects()[0];
        host.click_at(a.center().x, a.center().y);
        assert!(host.root_mut::<DragRun>().take_drops().is_empty());
    }

    #[test]
    fn letting_go_where_it_already_stands_moves_nothing() {
        let mut host = harness(run());
        let a = host.root::<DragRun>().item_rects()[0];
        host.drag_at((a.center().x, a.center().y), (a.x1 - 2.0, a.center().y));
        assert!(host.root_mut::<DragRun>().take_drops().is_empty());
    }

    #[test]
    fn dragging_past_the_end_lands_after_the_last_item() {
        let mut host = harness(run());
        let rects = host.root::<DragRun>().item_rects();
        let (a, d) = (rects[0], rects[3]);
        host.drag_at((a.center().x, a.center().y), (d.x1 - 2.0, d.center().y));
        let drops = host.root_mut::<DragRun>().take_drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].before, None, "the end of the run");
    }
}
