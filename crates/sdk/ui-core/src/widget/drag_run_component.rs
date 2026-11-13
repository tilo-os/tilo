//! [`DragRun`] as a described component: a run of items a person reorders by dragging one.

use crate::component::{
    Accepts, Component, Described, Event, ResourceId, SlotType, SlotViews, Source,
};

use super::drag_run::{DragDrop, DragRun, RunAxis, RunItem};

/// A run of items — a dock's icons, a toolbar's tiles, a row of cards — reordered by
/// dragging one.
#[derive(Component, Default)]
#[component(name = "DragRun", role = "Group", label = "name")]
pub struct DragRunProps {
    /// What the run is, for assistive technology.
    pub name: Option<String>,
    /// Whether the items lead to trailing (the default) or run top to bottom.
    pub axis: Option<RunAxis>,
    /// Where the run's packed items sit along it: its leading edge (the default), its
    /// centre, its trailing edge, or spread between its ends.
    pub distribute: Option<crate::widget::arrange::Distribute>,
    /// Where the items pack across the run: centred (the default).
    pub align: Option<crate::widget::arrange::Align>,
    /// The items, in order.
    #[component(slot(default))]
    pub items: DragItems,
    /// What stands beside the items at the run's trailing end and is never moved: a
    /// taskbar's time and device status.
    #[component(slot)]
    pub trailing: Vec<crate::widget::Element>,
    /// An item was let go in a new place: carries the resource it stands for and its place
    /// among the run's other items, from 0 — what `items.move(item, to: at)` takes.
    #[component(event(value = ResourceId, at = i64))]
    pub drop: Event,
}

/// One item of a run: what it stands for, and the view drawn in its slot.
#[derive(Component, Default)]
#[component(name = "DragItem", role = "ListItem", label = "title")]
pub struct DragItemProps {
    /// The item's stable key: what a drop names as moved, and as the item it lands before.
    #[component(key)]
    pub key: String,
    /// The resource the item stands for: what a drop carries, and what a drag out of the
    /// run reports.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What the item is called, for its ghost and for assistive technology.
    #[component(required)]
    pub title: String,
    /// The part of the run it belongs to: a bar's leading group, its centre, its trailing
    /// group, or the room between two groups.
    pub group: Option<super::drag_run::RunGroup>,
    /// What it shows.
    #[component(slot(default))]
    pub body: Vec<crate::widget::Element>,
    /// It was pressed and let go without moving: open what it stands for.
    #[component(event)]
    pub activate: Event,
}

/// The items of a run: `DragItem`s only.
#[derive(Default)]
pub struct DragItems(Vec<DragItemProps>);

impl SlotType for DragItems {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(DragItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut items = Vec::new();
        source.components(slot, &mut |child| {
            items.push(DragItemProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        self.0 = items;
        Ok(())
    }
}

impl Described for DragRun {
    type Props = DragRunProps;

    fn from_props(key: &str, props: DragRunProps) -> Self {
        let DragRunProps {
            name,
            axis,
            distribute,
            align,
            items,
            trailing,
            drop,
        } = props;
        let mut run = Self::new(key.to_owned());
        if let Some(name) = name {
            run = run.name(name);
        }
        if let Some(axis) = axis {
            run = run.axis(axis);
        }
        if let Some(distribute) = distribute {
            run = run.distribute(distribute);
        }
        if let Some(align) = align {
            run = run.align(align);
        }
        if !trailing.is_empty() {
            run = run.trailing(trailing);
        }
        let mut activates = Vec::with_capacity(items.0.len());
        for DragItemProps {
            key,
            title,
            item,
            group,
            body,
            activate,
        } in items.0
        {
            let view = body
                .into_iter()
                .next()
                // An item with no view of its own takes no room.
                .unwrap_or_else(|| {
                    Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0))
                });
            activates.push((key.clone(), activate));
            run = run.push(RunItem {
                key,
                item,
                label: title,
                group: group.unwrap_or_default(),
                view,
            });
        }
        run = run.on_activate(move |key| {
            if let Some((_, event)) = activates.iter().find(|(own, _)| own == key) {
                event
                    .clone()
                    .fire(Some(crate::component::Value::Text(key.to_owned())));
            }
        });
        // A drop reports the item moved and its place, as a board's lane reports
        // a card and its place: the event carries `[value, at]`.
        let items = std::rc::Rc::new(run.item_ids().into_iter().collect::<Vec<_>>());
        run = run.on_drop(move |dropped: &DragDrop| {
            let value = items
                .iter()
                .find(|(key, _)| key == &dropped.moved)
                .and_then(|(_, id)| id.clone())
                .map_or(crate::component::Value::None, |id| {
                    crate::component::Value::Resource(id.0)
                });
            drop.fire(Some(crate::component::Value::List(vec![
                value,
                crate::component::Value::Int(dropped.at),
            ])));
        });
        run
    }

    /// Take changed props in place: the items keep their keys and their order, so each item
    /// changes what it says and what it shows where it stands, and the run keeps what those
    /// views hold — a menu a person opened under a part of the bar stays open while the
    /// clock ticks or the desk switches the workspace the bar names.
    fn set_props(&mut self, props: DragRunProps) -> bool {
        let DragRunProps {
            name,
            axis,
            distribute,
            align,
            items,
            trailing,
            drop,
        } = props;
        if !trailing.is_empty()
            || name.as_deref() != self.name_of()
            || axis.is_some_and(|axis| axis != self.axis_of())
            || distribute.is_some_and(|distribute| distribute != self.distribute_of())
            || align.is_some_and(|align| align != self.align_of())
        {
            return false;
        }
        let mut activates = Vec::with_capacity(items.0.len());
        let mut taken = Vec::with_capacity(items.0.len());
        for DragItemProps {
            key,
            title,
            item,
            group,
            body,
            activate,
        } in items.0
        {
            let view = body
                .into_iter()
                .next()
                // An item with no view of its own takes no room.
                .unwrap_or_else(|| {
                    Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0))
                });
            activates.push((key.clone(), activate));
            taken.push(RunItem {
                key,
                item,
                label: title,
                group: group.unwrap_or_default(),
                view,
            });
        }
        if !self.take_items(taken) {
            return false;
        }
        self.replace_activate(Some(Box::new(move |key| {
            if let Some((_, event)) = activates.iter().find(|(own, _)| own == key) {
                event
                    .clone()
                    .fire(Some(crate::component::Value::Text(key.to_owned())));
            }
        })));
        // A drop reports the item moved and its place, as a board's lane reports
        // a card and its place: the event carries `[value, at]`.
        let items = std::rc::Rc::new(self.item_ids().into_iter().collect::<Vec<_>>());
        self.replace_reporter(Some(Box::new(move |dropped: &DragDrop| {
            let value = items
                .iter()
                .find(|(key, _)| key == &dropped.moved)
                .and_then(|(_, id)| id.clone())
                .map_or(crate::component::Value::None, |id| {
                    crate::component::Value::Resource(id.0)
                });
            drop.fire(Some(crate::component::Value::List(vec![
                value,
                crate::component::Value::Int(dropped.at),
            ])));
        })));
        true
    }

    /// The view of the item `key` shows, to change where the item stands.
    fn part_views(&mut self, slot: &str, key: &str, of: &str) -> Option<&mut dyn SlotViews> {
        if slot != "items" || of != "body" {
            return None;
        }
        let at = self.index_of(key)?;
        self.edit_item(Some(at));
        Some(self as &mut dyn SlotViews)
    }
}
