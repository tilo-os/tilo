//! [`VirtualList`] as a described component: a list of items, each built into its row when it
//! scrolls into view, with what the row offers on its item (a context menu, a swipe) and who
//! else is at it.
//!
//! Its rows are [`ListItemProps`]: each holds the row itself (a [`ListRow`], or any view) and
//! what the list does with it. A host that renders its rows only as they are shown hands them
//! over as a [`SharedRows`] ([`Source::rows`]): a list of ten thousand items costs the rows on
//! screen, and the host queues what changed for the list to take. Rows given as components are
//! held whole.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use vello::kurbo::Point;

use crate::component::{
    Accepts, Component, Described, Event, Moment, Person, ResourceId, SharedRows, SlotType, Source,
    Value,
};
use crate::selection::KeySelection;
use crate::tokens::{Rhythm, TextRole};
use crate::widget::arrange::{
    Align, Fit, Length, MEASURE_REM, Page, Row, Slot, Stack, TranscriptEdges,
};
use crate::widget::conversation::list::RUN_WINDOW_SECS;
use crate::widget::conversation::{ConversationClock, DayDivider};
use crate::widget::{
    Attachments, Avatar, ContextMenuArea, Decorator, Element, ListRow, ListRowProps, MenuEntry,
    MenuPart, RowBuild, Spacer, SwipeActionProps, Text, Tone, Widget,
};

use super::{ListModel, ListModelChange, TREE_INDENT, TreeMarks, VirtualList};
use crate::widget::ChangeMarkProps;

/// The height a row is reckoned at before it is laid out: a row with a secondary line.
// ratchet-allow(pixel_const): a height reckoned before layout measures it, not spacing
pub(crate) const ROW_ESTIMATE: f32 = 56.0;

/// The height a message of a transcript is reckoned at before it is measured.
// ratchet-allow(pixel_const): a height reckoned before layout measures it, not spacing
const MESSAGE_ESTIMATE: f32 = 64.0;

/// One item of a virtual list: the resource its row shows, the row, and what the row offers on
/// the item.
#[derive(Component, Default)]
#[component(name = "ListItem", role = "ListItem", presents = "item")]
pub struct ListItemProps {
    /// The item's stable key: selection, focus and motion follow it.
    #[component(key)]
    pub key: String,
    /// The resource the row shows: what selecting or activating the row reports.
    pub item: Option<ResourceId>,
    /// Who else is at the item now (a collaborator's or an agent's lens on it), shown on the
    /// row.
    pub presence: Option<Person>,
    /// Who wrote it, in a transcript: the list shows their avatar, their name and the time
    /// over each run of their consecutive messages, so the row holds only what they wrote.
    pub author: Option<Person>,
    /// When it was written, in a transcript: the time beside its author's name, and a divider
    /// over the first message of each day.
    pub time: Option<Moment>,
    /// What the item offers on its context menu (a right-click, the Menu key or a long press
    /// opens it, and selects the row unless the row opens where it stands): each of its
    /// allowed actions, after the menu the row's view placed (`attached`) and without those
    /// that menu placed already, so every action stays reachable.
    #[component(slot)]
    pub menu: Vec<MenuPart>,
    /// What swiping the row from its trailing edge does, unless its view says
    /// (`attached`).
    #[component(slot)]
    pub swipe: Option<SwipeActionProps>,
    /// What the row's view attached to it (`ui.ContextMenu`, `ui.HoverToolbar`, `ui.Swipe`
    /// anywhere in the row's view): its own menu, first in the row's menu; a toolbar shown
    /// while the row is hovered or holds focus; a swipe either way.
    #[component(slot)]
    pub attached: Attachments,
    /// What someone else changed of the item that the person has not seen yet: the row shows
    /// it, in their colour, on its trailing edge. The UI host places it (task-legibility §3).
    #[component(slot)]
    pub change: Option<ChangeMarkProps>,
    /// The group the row is in, as the platform reads it over the group's first row: a day
    /// for a date. Rows of one group stand together under its heading, in the order they come.
    pub group: Option<String>,
    /// The group's day is the day the item was done on, not the day it is due: it reads as the
    /// day it happened (`Yesterday`, `Sunday`, `Jun 21`), never as a day gone by, and in no
    /// tone of its own.
    pub group_done: bool,
    /// How the group's heading and its count read: left to the platform, it reads the tone the
    /// date the heading holds asks for (a day gone by is overdue), unless the view names one.
    pub group_tone: Option<Tone>,
    /// The row is one of the items done with: they stand after every group, under the list's
    /// tail title, closed until the person opens them.
    pub tail: bool,
    /// The item the view shows just over this one in its group: the place the actions offered
    /// on the row take when one reads a place (`above`). None for the first of a group.
    pub above: Option<ResourceId>,
    /// The item was removed and the person has not seen it go: the row stands in for it, its
    /// title struck through, at the row's own height. The UI host places such rows.
    pub removed: bool,
    /// The item this one is a subtask of: in a tree list (`tree`) the row stands indented under
    /// that item's row, with a disclosure that folds it away.
    pub parent: Option<ResourceId>,
    /// The title of the item it is under, for the row to say where it belongs while that item
    /// is no row of the list.
    pub parent_title: Option<String>,
    /// Its open subtasks of all of them, on the row's trailing edge: "1/3".
    pub progress: Option<String>,
    /// The row: a `ui.ListRow`, or any view.
    #[component(slot(default))]
    pub row: ItemRow,
    /// The item's document, shown under the row while the row stands open into it, one lead
    /// in under the row's text: a row with a document opens where it stands in the list when
    /// it is chosen, and the list's own motion grows the row to hold it. Without one, choosing
    /// the row opens the item elsewhere (`ui.VirtualList`'s `activate`).
    #[component(slot)]
    pub detail: ItemRow,
}

/// The row of a [`ListItemProps`].
#[derive(Default)]
pub enum ItemRow {
    /// None given: the row shows nothing.
    #[default]
    None,
    /// A list row, built from its props each time it scrolls into view.
    Row(Box<ListRowProps>),
    /// Any other view, built.
    View(Element),
}

/// The row of an item: one list row, or one view of any kind.
impl SlotType for ItemRow {
    const MANY: bool = false;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let kinds = source.slot_kinds(slot);
        if matches!(kinds.as_slice(), [Some(kind)] if kind == ListRowProps::NAME) {
            let mut row = None;
            source.components(slot, &mut |child| {
                row = Some(ListRowProps::from_source(child).map_err(|error| error.to_string())?);
                Ok(())
            })?;
            *self = row.map_or(Self::None, |row| Self::Row(Box::new(row)));
            return Ok(());
        }
        // A row's view: its call (one view, whatever it holds), or the views given one by one,
        // stacked as a view's body is.
        let mut views = source.children(slot);
        *self = match views.len() {
            0 => Self::None,
            1 => views.pop().map_or(Self::None, Self::View),
            _ => Self::View(Box::new(
                views
                    .into_iter()
                    .fold(Stack::stack().fill_width().gap(Rhythm::Tight), Stack::push),
            )),
        };
        Ok(())
    }
}

/// A long list of rows that costs only the rows on screen: each is built when it scrolls into
/// view. Selection follows the person's choice and is reported with the item it shows; the list
/// is one focus stop.
#[derive(Component, Default)]
#[component(name = "VirtualList", role = "List", label = "name")]
pub struct VirtualListProps {
    /// What the list is, for assistive technology.
    pub name: Option<String>,
    /// Moving through the rows with the keyboard selects them, as a list whose selection shows
    /// beside it does; a click or a tap on a row then activates it, as choosing an entry of an
    /// overview does.
    pub selection_follows_focus: bool,
    /// The list is a column's whole body: its rows' text starts where the column's header
    /// starts its title.
    pub in_column: bool,
    /// The list is read to its end: it opens at its last row and stays there as rows arrive and
    /// settle, unless the person scrolled away ([`VirtualList::following_end`]).
    pub follow_end: bool,
    /// Each row stands in the page column: a section's gap under the row before it, each run one
    /// author wrote under that author's name and the time ([`ListItemProps::author`],
    /// [`ListItemProps::time`]), and a press that gives a row the keyboard, choosing none.
    pub page_column: bool,
    /// The page column's rows keep the readable measure every page sets its blocks on
    /// ([`MEASURE_REM`]), centred in the width the list is given. Without it they fill that
    /// width, as a conversation's messages do.
    pub at_measure: bool,
    /// The title over the list's tail group: the rows whose item says it is one of those done
    /// with ([`ListItemProps::tail`]) stand after every group, under it, closed until the
    /// person opens them. `None` leaves those rows among the others.
    pub tail_title: Option<String>,
    /// The rows stand as a tree: each indented under the row of the item it names as its parent
    /// ([`ListItemProps::parent`]), with a disclosure on the rows that have subtasks, folding
    /// them away. The list works the tree out of the items as it builds their rows.
    pub tree: bool,
    /// The title over the group of rows whose item names no group: a list grouped by a thing
    /// some items lack (a day, a place) heads those rows with it, never folding them under the
    /// last group that names one. The rows keep the order their list gives. `None` leaves them
    /// among the others.
    pub ungrouped_title: Option<String>,
    /// The list holds more than one selected row: a click with ctrl (or cmd) adds or takes
    /// away the row it lands on, one with shift takes the range from the last row chosen, and
    /// Escape puts the selection away. Without it a click chooses the one row the window shows.
    pub multi_select: bool,
    /// The item where the person looking has seen nothing yet (`first_unseen(c)`): the toolkit's
    /// [`UnreadDivider`](crate::widget::conversation::UnreadDivider) stands over the row that
    /// shows it, and while that is above the view a pill at the foot offers the way back to it
    /// ([`VirtualList::following_end`]). `None` draws neither. The list works out the row as it
    /// builds, so a list keeps its rows and only the two the divider moved between build again.
    pub unseen_from: Option<ResourceId>,
    /// The items, in order.
    #[component(slot(default))]
    pub rows: DescribedRows,
    /// Items someone else removed that the person has not seen go: each a row after the
    /// items, its title struck through. The UI host places them.
    #[component(slot)]
    pub removed: RemovedRows,
    /// The selected rows, by key.
    #[component(state)]
    pub selection: Option<Vec<String>>,
    /// The items selected, in the order they stand: the selection a host keeps for its person,
    /// given back so a list built again shows it as they left it.
    #[component(state)]
    pub chosen: Option<Vec<ResourceId>>,
    /// How far the list is scrolled, in logical pixels.
    #[component(state)]
    pub scroll: Option<f64>,
    /// The item standing open into its document where it stands in the list: its row grows to
    /// hold what the item's row gives as its `detail`, and Escape or a press beside it closes
    /// it. A host keeps which item is open and gives it back, so a list it builds again shows
    /// the item as the person left it.
    #[component(state)]
    pub open: Option<ResourceId>,
    /// The pace its rows arrive, leave and change places at, and the rows it builds move at
    /// when they say nothing themselves: the theme's unless it says otherwise.
    pub motion: Option<crate::motion::Feel>,
    /// A row was selected: carries the item it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A row was activated (a click, Enter, an assistive technology): carries the item it
    /// shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
    /// The selection changed: carries the items selected, in the order they stand, and none
    /// when the selection was put away.
    #[component(event)]
    pub selected: Event,
    /// The person opened a row into its item where it stands: carries the item.
    #[component(event(value = ResourceId))]
    pub opened: Event,
    /// The row standing open closed: pressed again, Escape, a press beside it, or its item
    /// left the list.
    #[component(event)]
    pub closed: Event,
}

/// The items of a described list: read from a host that renders them as they are shown, or
/// given whole.
#[derive(Default)]
pub struct DescribedRows {
    held: Held,
    /// Rows standing in for removed items, after the items ([`VirtualListProps::removed`]).
    removed: Rc<Vec<ListItemProps>>,
    /// The heading over the rows whose item names no group
    /// ([`VirtualListProps::ungrouped_title`]).
    ungrouped: Option<String>,
}

/// The rows of a list that stand in for items removed unseen: [`ListItemProps`] marked
/// `removed`, in order.
#[derive(Default)]
pub struct RemovedRows(Rc<Vec<ListItemProps>>);

impl RemovedRows {
    fn keys(&self) -> Vec<String> {
        self.0.iter().map(|item| item.key.clone()).collect()
    }
}

impl SlotType for RemovedRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(ListItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut items: Vec<ListItemProps> = Vec::new();
        source.components(slot, &mut |child| {
            let item = ListItemProps::from_source(child).map_err(|error| error.to_string())?;
            if !item.removed {
                return Err("the removed rows of a list stand for removed items".to_owned());
            }
            items.push(item);
            Ok(())
        })?;
        self.0 = Rc::new(items);
        Ok(())
    }
}

#[derive(Clone, Default)]
enum Held {
    #[default]
    Nothing,
    /// Rows a host renders as they are shown.
    Rendered(SharedRows),
    /// Rows given whole, with their keys.
    Given(Rc<Vec<ListItemProps>>),
}

/// The items of a list: [`ListItemProps`] only.
impl SlotType for DescribedRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(ListItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        if let Some(rows) = source.rows(slot) {
            self.held = Held::Rendered(rows);
            return Ok(());
        }
        let mut items: Vec<ListItemProps> = Vec::new();
        source.components(slot, &mut |child| {
            let item = ListItemProps::from_source(child).map_err(|error| error.to_string())?;
            match &item.row {
                _ if !item.attached.toolbar.is_empty() => {
                    return Err(
                        "a list given its items whole holds list rows; a row with a toolbar of its own renders as it is shown"
                            .to_owned(),
                    );
                }
                ItemRow::Row(row) if row.trailing.is_some() => {
                    return Err(
                        "a row of a virtual list is built again each time it scrolls into view, so it takes no trailing control"
                            .to_owned(),
                    );
                }
                ItemRow::View(_) => {
                    return Err(
                        "a list given its items whole holds list rows; a list of other views renders them as they are shown"
                            .to_owned(),
                    );
                }
                _ => {}
            }
            if let ItemRow::View(_) = item.detail {
                return Err(
                    "a list given its items whole holds list rows, and the document a row opens into is one too; a list of other views renders them as they are shown"
                        .to_owned(),
                );
            }
            if items.iter().any(|other| other.key == item.key) {
                return Err(format!("has two items of the key {}", item.key));
            }
            items.push(item);
            Ok(())
        })?;
        self.held = Held::Given(Rc::new(items));
        Ok(())
    }
}

impl DescribedRows {
    /// The same items, to read from elsewhere (what an event of the list reports).
    fn reader(&self) -> Self {
        Self {
            held: self.held.clone(),
            removed: Rc::clone(&self.removed),
            ungrouped: self.ungrouped.clone(),
        }
    }

    /// Whether `key` is a row standing in for a removed item.
    fn is_removed(&self, key: &str) -> bool {
        self.removed.iter().any(|item| item.key == key)
    }

    /// How many items the list holds, before the removed items' rows.
    fn held_len(&self) -> usize {
        match &self.held {
            Held::Nothing => 0,
            Held::Given(items) => items.len(),
            Held::Rendered(rows) => rows.try_borrow().map_or(0, |rows| rows.len()),
        }
    }

    /// Read the item `key` with `read`: `None` when there is no such item, or it is not
    /// rendered.
    fn with_item<R>(&self, key: &str, read: impl FnOnce(ListItemProps) -> R) -> Option<R> {
        if let Some(item) = self.removed.iter().find(|item| item.key == key) {
            return Some(read(reusable(item)?));
        }
        match &self.held {
            Held::Nothing => None,
            Held::Given(items) => {
                let item = items.iter().find(|item| item.key == key)?;
                Some(read(reusable(item)?))
            }
            Held::Rendered(rows) => {
                let mut found = None;
                let read_one = &mut |source: &mut dyn Source| {
                    found = Some(
                        ListItemProps::from_source(source).map_err(|error| error.to_string())?,
                    );
                    Ok(())
                };
                let shown = rows.try_borrow_mut().ok()?.read(key, read_one);
                match shown {
                    Ok(true) => found.map(read),
                    Ok(false) => None,
                    Err(error) => {
                        tracing::warn!(%error, key, "an item of the list did not read");
                        None
                    }
                }
            }
        }
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        match &self.held {
            Held::Nothing => None,
            Held::Given(items) => items
                .iter()
                .find(|item| item.item.as_ref().is_some_and(|shown| shown.0 == id))
                .map(|item| item.key.clone()),
            Held::Rendered(rows) => rows.try_borrow().ok()?.key_presenting(id),
        }
    }
}

/// An item given whole, again: its row's props are built anew each time it scrolls in, and the
/// document its row opens into, which is a row of the same kind. `None` for an item that is not
/// rows all the way down: a view given once cannot be built a second time, so a list given its
/// items whole holds rows (and their documents) rather than views.
fn reusable(item: &ListItemProps) -> Option<ListItemProps> {
    let row = match &item.row {
        ItemRow::None => ItemRow::None,
        ItemRow::Row(row) => ItemRow::Row(Box::new(row.reusable()?)),
        ItemRow::View(_) => return None,
    };
    let detail = match &item.detail {
        ItemRow::None => ItemRow::None,
        ItemRow::Row(document) => ItemRow::Row(Box::new(document.reusable()?)),
        ItemRow::View(_) => return None,
    };
    Some(ListItemProps {
        key: item.key.clone(),
        item: item.item.clone(),
        presence: item.presence.clone(),
        author: item.author.clone(),
        time: item.time,
        menu: item.menu.iter().map(MenuPart::again).collect(),
        swipe: item.swipe.as_ref().map(SwipeActionProps::again),
        attached: Attachments {
            menu: item.attached.menu.iter().map(MenuPart::again).collect(),
            toolbar: Vec::new(),
            swipe: crate::widget::SwipeProps {
                leading: item
                    .attached
                    .swipe
                    .leading
                    .as_ref()
                    .map(SwipeActionProps::again),
                trailing: item
                    .attached
                    .swipe
                    .trailing
                    .as_ref()
                    .map(SwipeActionProps::again),
            },
        },
        change: item.change.as_ref().map(ChangeMarkProps::again),
        group: item.group.clone(),
        group_done: item.group_done,
        group_tone: item.group_tone,
        tail: item.tail,
        above: item.above.clone(),
        removed: item.removed,
        parent: item.parent.clone(),
        parent_title: item.parent_title.clone(),
        progress: item.progress.clone(),
        row,
        detail,
    })
}

impl ListModel for DescribedRows {
    type Key = String;

    fn len(&self) -> usize {
        self.held_len().saturating_add(self.removed.len())
    }

    fn key_at(&self, index: usize) -> Option<String> {
        let held = self.held_len();
        if index >= held {
            return self
                .removed
                .get(index.saturating_sub(held))
                .map(|item| item.key.clone());
        }
        match &self.held {
            Held::Nothing => None,
            Held::Given(items) => items.get(index).map(|item| item.key.clone()),
            Held::Rendered(rows) => rows.try_borrow().ok()?.key_at(index),
        }
    }

    fn index_of(&self, key: &String) -> Option<usize> {
        if let Some(at) = self.removed.iter().position(|item| item.key == *key) {
            return Some(self.held_len().saturating_add(at));
        }
        match &self.held {
            Held::Nothing => None,
            Held::Given(items) => items.iter().position(|item| item.key == *key),
            Held::Rendered(rows) => rows.try_borrow().ok()?.index_of(key),
        }
    }

    fn request_range(&mut self, range: Range<usize>) {
        if let Held::Rendered(rows) = &self.held
            && let Ok(mut rows) = rows.try_borrow_mut()
        {
            rows.request_range(range);
        }
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<String>> {
        match &self.held {
            Held::Rendered(rows) => rows
                .try_borrow_mut()
                .map(|mut rows| rows.take_changes())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn needs_poll(&self) -> bool {
        match &self.held {
            Held::Rendered(rows) => rows.try_borrow().is_ok_and(|rows| rows.has_changes()),
            _ => false,
        }
    }

    /// A row whose item brought a document of its own opens where it stands: choosing it
    /// shows the document under the row, in the list, rather than opening the item elsewhere.
    fn opens_in_place(&self, index: usize) -> bool {
        let Some(key) = self.key_at(index) else {
            return false;
        };
        self.with_item(&key, |item| !matches!(item.detail, ItemRow::None))
            .unwrap_or(false)
    }

    /// What the row is grouped by: its item's heading, the list's tail for an item done
    /// with, or the list's heading for the items that name none.
    fn group_at(&self, index: usize) -> Option<super::RowGroup> {
        let key = self.key_at(index)?;
        self.with_item(&key, |item| {
            // A row with something to report — a change nobody has seen, an item removed and
            // not yet seen go — is not folded away with its group: its group's rows are out of
            // the way, and what it says is for the person, wherever it would stand.
            if item.change.is_some() || item.removed {
                return None;
            }
            if item.tail {
                return Some(super::RowGroup::Tail);
            }
            let ungrouped = || {
                self.ungrouped.clone().map(|text| super::RowGroup::Heading {
                    text,
                    tone: Tone::Neutral,
                })
            };
            match item.group.as_deref().filter(|heading| !heading.is_empty()) {
                Some(heading) => Some(super::RowGroup::Heading {
                    text: heading.to_owned(),
                    tone: item.group_tone.unwrap_or(Tone::Neutral),
                }),
                None => ungrouped(),
            }
        })
        .flatten()
    }

    /// What the row stands as in a tree: the item it is a subtask of, when that item is one of
    /// the list's rows, and the count of its subtasks.
    fn tree_at(&self, index: usize) -> Option<super::TreeRow<String>> {
        let key = self.key_at(index)?;
        self.with_item(&key, |item| {
            let parent = item
                .parent
                .as_ref()
                .and_then(|parent| DescribedRows::key_presenting(self, &parent.0));
            super::TreeRow { parent }
        })
    }

    /// Whether any row stands under a heading, or was given a parent: the list keeps a
    /// disclosure gutter for its headings' and parents' chevrons then.
    fn discloses(&self) -> bool {
        self.ungrouped.is_some()
            || match &self.held {
                Held::Given(items) => items.iter().any(|item| {
                    item.tail || item.group.as_ref().is_some_and(|group| !group.is_empty())
                }),
                Held::Nothing | Held::Rendered(_) => false,
            }
    }

    fn label(&self, index: usize) -> Option<String> {
        let key = self.key_at(index)?;
        self.with_item(&key, |item| match item.row {
            ItemRow::Row(row) if !row.title.is_empty() => Some(row.title),
            _ => None,
        })
        .flatten()
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        DescribedRows::key_presenting(self, id)
    }
}

/// The list a described [`VirtualList`] is: over its items, under the one context menu its
/// rows share.
pub struct DescribedList {
    body: ListBody,
    /// What the list was made with, but its items: the same again is taken in place.
    face: Face,
    /// The items the list's events read, as its model holds them now: a list given its items
    /// again answers with the items it has, not the ones it was built with.
    reading: Rc<RefCell<DescribedRows>>,
}

/// The props of a described list that decide the widget, its items aside.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    selection_follows_focus: bool,
    in_column: bool,
    follow_end: bool,
    page_column: bool,
    at_measure: bool,
    /// The rows stand in a tree: each under the row of the item it names.
    tree: bool,
    /// The rows the host renders as they are shown, by identity: they then change through
    /// the host, never through the props. `None` for items given whole.
    rendered: Option<usize>,
    /// The removed items' rows, by key.
    removed: Vec<String>,
    /// The heading over the rows whose item names no group.
    ungrouped: Option<String>,
}

impl Face {
    /// What the props say of the list, its items aside.
    fn of(props: &VirtualListProps) -> Self {
        Self {
            name: props.name.clone(),
            selection_follows_focus: props.selection_follows_focus,
            in_column: props.in_column,
            follow_end: props.follow_end,
            page_column: props.page_column,
            at_measure: props.at_measure,
            tree: props.tree,
            rendered: rendered(&props.rows),
            removed: props.removed.keys(),
            ungrouped: props.ungrouped_title.clone(),
        }
    }
}

/// The identity of the rows a host renders, when it renders them.
fn rendered(rows: &DescribedRows) -> Option<usize> {
    match &rows.held {
        Held::Rendered(rows) => Some(Rc::as_ptr(rows).cast::<()>() as usize),
        _ => None,
    }
}

enum ListBody {
    /// The list under the menu every row shares.
    Menus(Box<ContextMenuArea>),
}

impl DescribedList {
    /// The list.
    #[must_use]
    pub fn list(&self) -> Option<&VirtualList<DescribedRows>> {
        match &self.body {
            ListBody::Menus(area) => area.content().as_any()?.downcast_ref(),
        }
    }

    /// The list, to change.
    pub fn list_mut(&mut self) -> Option<&mut VirtualList<DescribedRows>> {
        match &mut self.body {
            ListBody::Menus(area) => area.content_mut().as_any_mut()?.downcast_mut(),
        }
    }

    /// The menu over the list, for a test of what it offers.
    pub fn menu_area_mut(&mut self) -> Option<&mut ContextMenuArea> {
        match &mut self.body {
            ListBody::Menus(area) => Some(area),
        }
    }

    /// Select the row that shows the resource `id`, and only it: the item the next column
    /// shows. `None` selects nothing. Whether the selection changed.
    ///
    /// A choice of more than one row is the person's own while it holds: what the column
    /// beside the list shows, or none, says nothing about it.
    pub fn select_presenting(&mut self, id: Option<&str>) -> bool {
        if self.chosen_rows() > 1 {
            return false;
        }
        let Some(list) = self.list_mut() else {
            return false;
        };
        let key = id.and_then(|id| list.model().key_presenting(id));
        let selection = list.selection_mut();
        let now: Vec<String> = selection.stored_keys().cloned().collect();
        let want: Vec<String> = key.iter().cloned().collect();
        if now == want {
            return false;
        }
        selection.clear();
        if let Some(key) = key {
            selection.select_only(key);
        }
        true
    }

    /// How many rows of the list the person chose.
    fn chosen_rows(&self) -> usize {
        self.list()
            .map_or(0, |list| list.selection().len(list.model().len()))
    }
}

/// Whether the selection being made comes from a click or a tap the person just made, rather
/// than the keyboard or an assistive technology.
pub(crate) fn by_hand() -> bool {
    matches!(
        crate::motion::recent_input(crate::time::now()),
        Some(crate::tokens::MotionSource::Pointer | crate::tokens::MotionSource::Gesture)
    )
}

/// The list with `open` standing open where it stands in it: the row showing that item, unless
/// the item is no row of the list (a place that does not hold it).
fn with_open<M: ListModel<Key = String>>(
    mut list: VirtualList<M>,
    open: Option<&ResourceId>,
    rows: &DescribedRows,
) -> VirtualList<M> {
    if let Some(key) = open.and_then(|item| rows.key_presenting(&item.0)) {
        list.set_open(Some(key));
    }
    list
}

/// The resource the item `key` shows, as an event carries it.
fn item_of(rows: &DescribedRows, key: &str) -> Option<Value> {
    rows.with_item(key, |item| item.item.map(|id| Value::Resource(id.0)))
        .flatten()
}

/// The rows a host chose for its person ([`VirtualListProps::chosen`]), from the items it
/// names: none when it names none, so a choice the list made itself stands. A choice of several
/// rows is the host's while it holds: what the host kept of it gone, an action over the rows ran
/// and put it away, the list puts its own away with it. A single row chosen by the keyboard is
/// the list's own, kept while no record names it.
fn picked_rows(
    chosen: Option<Vec<ResourceId>>,
    reading: &DescribedRows,
    selected: usize,
) -> Option<KeySelection<String>> {
    let items = chosen?;
    if items.is_empty() {
        return (selected > 1).then(KeySelection::new);
    }
    let mut picked = KeySelection::new();
    for item in &items {
        if let Some(key) = reading.key_presenting(&item.0) {
            picked.toggle(key);
        }
    }
    Some(picked)
}

/// The commands of a row's menus that declare a shortcut (`key: "Ctrl+Enter"`), each with the
/// keys it answers, so the row runs it where it has the keyboard as choosing it does: the
/// item's own menu first, then what the view attached to the row. Text that is no chord names
/// no key, and the first command to name a shortcut holds it.
fn declared_shortcuts(
    menu: &[MenuPart],
    attached: &[MenuPart],
) -> Vec<(crate::platform::Shortcut, std::rc::Rc<dyn Fn()>)> {
    let mut out: Vec<(crate::platform::Shortcut, std::rc::Rc<dyn Fn()>)> = Vec::new();
    for part in menu.iter().chain(attached) {
        let MenuPart::Item(item) = part else {
            continue;
        };
        let Some(key) = item.key.as_deref() else {
            continue;
        };
        if tilo_ui_graph::keys::Chord::parse(key).is_err() {
            continue;
        }
        let shortcut = crate::platform::Shortcut::from(key);
        if out
            .iter()
            .any(|(declared, _)| declared.same_press(&shortcut))
        {
            continue;
        }
        // A command the view bound to a published action is that action, which the host runs
        // from its own key; only the view's own events run from here.
        let press = crate::widget::Press::of(item.choose.clone());
        if press.address().is_none() && !press.is_none() {
            out.push((shortcut, press.runner()));
        }
    }
    out
}

/// The row of an item: its list row with the item's presence, or its view; under the swipe
/// and beside the hover toolbar the item has.
fn row_of(
    rows: &DescribedRows,
    key: &str,
    feel: crate::motion::Feel,
    tree: Option<&TreeMarks<String>>,
    build: RowBuild,
) -> Element {
    match bare_row(rows, key, feel, tree, build) {
        Some((row, swipe, attached)) => attach(row, swipe, attached),
        None => Box::new(ListRow::new(key.to_owned(), "")),
    }
}

/// A row of a tree that is a view of the app's own: its content stands `depth` steps in, one
/// disclosure gutter each, and how far its branch got follows it.
fn indent_view(view: Element, depth: u32, progress: Option<String>) -> Element {
    let mut line = Row::row().fill_width().align(Align::Center);
    {
        // The row's own depth, and the step its chevron takes in front of it: a row with
        // nothing to fold stands there all the same, on the column of its depth.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "logical pixels are bounded far below f32::MAX"
        )]
        let indent = ((f64::from(depth) + 1.0) * TREE_INDENT) as f32;
        line = line.push(Box::new(Spacer::fixed(indent)));
    }
    let mut slot = Slot::new(view);
    slot.fit = Fit::hug().width(crate::widget::arrange::Sizing::fill(1.0));
    line = line.push_slot(slot);
    if let Some(progress) = progress {
        line = line.push(Box::new(Text::new(&progress).with_role(TextRole::Caption)));
    }
    Box::new(line)
}

/// The row of an item, with the swipe and what its view attached, to put around it (or around
/// the message it is in).
fn bare_row(
    rows: &DescribedRows,
    key: &str,
    feel: crate::motion::Feel,
    tree: Option<&TreeMarks<String>>,
    RowBuild { expanded, .. }: RowBuild,
) -> Option<(Element, Option<SwipeActionProps>, Attachments)> {
    // What the row over it states, read before the row's own: a row that stands under its
    // parent says only what the parent leaves unsaid, so the day fact it shares with the parent
    // is the parent's, read once above it.
    let parent_meta = nesting_parent(rows, key, tree).and_then(|parent| {
        rows.with_item(&parent, |item| match item.row {
            ItemRow::Row(row) => row.meta,
            ItemRow::View(_) | ItemRow::None => None,
        })
        .flatten()
    });
    rows.with_item(key, |item| {
        let ListItemProps {
            presence,
            swipe,
            attached,
            row,
            detail,
            change,
            removed,
            menu,
            parent_title,
            progress,
            ..
        } = item;
        // Where the row belongs, while the item it is under is no row of this list: the row
        // says it, rather than standing indented under nothing.
        let breadcrumb =
            parent_title.filter(|_| !tree.is_some_and(|marks| marks.is_nested(&key.to_owned())));
        let shortcuts = declared_shortcuts(&menu, &attached.menu);
        // What someone else changed and the person has not seen, at the row's own height: on a
        // list row their face on its trailing edge and their colour under it; beside a row's
        // own view their face alone.
        let row = match row {
            ItemRow::Row(mut props) => {
                if props.secondary.is_none()
                    && let Some(title) = &breadcrumb
                {
                    props.secondary = Some(format!("in {title}"));
                }
                // The list paces the rows it builds unless a row says its own, so one option on
                // the list covers its content.
                props.motion = props.motion.or(Some(feel));
                // The fact the parent's row already states is the parent's.
                if let (Some(parent), Some(meta)) = (&parent_meta, &props.meta)
                    && parent == meta
                {
                    props.meta = None;
                    props.meta_tone = crate::widget::Tone::Neutral;
                }
                // The list activates its rows itself, by key: a row of it is not a target of
                // its own.
                props.activate = Event::None;
                // The row's own figure stands unless the tree's count takes that column.
                let value_free = props.value.is_none();
                let mut row = ListRow::from_props(key, *props);
                if let Some(person) = presence {
                    row = row.presence(person.identity);
                }
                if removed {
                    row = row.struck();
                }
                if let Some(mark) = change.and_then(ChangeMarkProps::row) {
                    row = row.change(mark.identity, mark.name, mark.spoken, mark.said, mark.show);
                }
                row = row.shortcuts(shortcuts);
                // A tree indents the row's content one disclosure gutter per depth, its lift
                // still filling the row; how far its branch got is its trailing figure.
                if let Some(marks) = tree {
                    // A tree's row stands one step per depth in from the column its list
                    // leads, whether or not it has subtasks to fold: the chevron that folds it
                    // hangs in the step in front of its content.
                    row = row.depth(marks.depth(&key.to_owned()));
                    if value_free && let Some(progress) = progress.clone() {
                        row = row.value(progress);
                    }
                }
                Box::new(row) as Element
            }
            ItemRow::View(view) => {
                let view = with_mark(
                    view,
                    change.and_then(|change| change.face(&format!("{key}~change"))),
                );
                match tree {
                    Some(marks) => {
                        indent_view(view, marks.depth(&key.to_owned()), progress.clone())
                    }
                    None => view,
                }
            }
            ItemRow::None => Box::new(ListRow::new(key.to_owned(), "")),
        };
        // A row standing open into its item: the item's document under the row, one lead in
        // under the text the row's leading control leads to, so the row becomes the item where
        // it stands in the list rather than opening a page of its own.
        let row = match (expanded, detail) {
            (true, ItemRow::View(document)) => {
                let mut open = Stack::stack().fill_width();
                open.set_lead(1);
                Box::new(open.push(row).push(document)) as Element
            }
            (true, ItemRow::Row(props)) => {
                let document = Box::new(ListRow::from_props(key, *props)) as Element;
                let mut open = Stack::stack().fill_width();
                open.set_lead(1);
                Box::new(open.push(row).push(document)) as Element
            }
            (true, ItemRow::None) | (false, _) => row,
        };
        (row, swipe, attached)
    })
}

/// The key of the row `key` stands under in its tree, when it stands nested under it: its
/// parent's row is the one above, not the item its sort put it away from.
fn nesting_parent(
    rows: &DescribedRows,
    key: &str,
    tree: Option<&TreeMarks<String>>,
) -> Option<String> {
    let marks = tree?;
    if !marks.is_nested(&key.to_owned()) {
        return None;
    }
    let parent = rows.with_item(key, |item| item.parent.clone()).flatten()?;
    rows.key_presenting(&parent.0)
}

/// `row` with a change mark after it, on its line, when it has one.
fn with_mark(row: Element, mark: Option<crate::widget::Chip>) -> Element {
    match mark {
        Some(mark) => {
            let mut row = Slot::new(row);
            row.fit =
                crate::widget::arrange::Fit::hug().width(crate::widget::arrange::Sizing::fill(1.0));
            Box::new(
                Row::row()
                    .fill_width()
                    .align(Align::Center)
                    .push_slot(row)
                    .push(Box::new(mark)),
            )
        }
        None => row,
    }
}

/// `row` with the swipe and the hover toolbar its item has: the view's own swipe each way, the
/// item's (`swipe`) from the trailing edge when the view gives none there.
fn attach(row: Element, swipe: Option<SwipeActionProps>, attached: Attachments) -> Element {
    let Attachments {
        toolbar,
        swipe: swiped,
        ..
    } = attached;
    let trailing = swiped.trailing.or(swipe);
    // The toolbar is over the row, so a swipe moves both.
    let row = if toolbar.is_empty() {
        row
    } else {
        Box::new(crate::widget::HoverToolbar::new(row, toolbar)) as Element
    };
    if trailing.is_some() || swiped.leading.is_some() {
        Box::new(crate::widget::SwipeRow::new(row, swiped.leading, trailing))
    } else {
        row
    }
}

/// A message of a transcript: its item's row, under its author and the time when the item
/// names them. A run of one author's messages, each within [`RUN_WINDOW_SECS`] of the one
/// before it on the same day, shows the author's avatar and name once, over its first message;
/// the others line up under it. The first message of a day has the day's divider over it.
///
/// Each message is one row of the page's column: a run's next line stands a tight gap under
/// the one before, a new run a section's gap under the last.
fn message_of(
    rows: &DescribedRows,
    key: &str,
    unread: Option<&str>,
    feel: crate::motion::Feel,
    measure: Option<Length>,
) -> Element {
    // The row that shows the item the reader has not seen, of the rows the host rendered. An
    // item is named by its id (`root-x/iy`), the one the app said and the one the row shows.
    let first_unread = unread.is_some_and(|item| {
        rows.with_item(key, |props| {
            props
                .item
                .as_ref()
                .is_some_and(|shown| shown.0.as_str() == item)
        })
        .unwrap_or(false)
    });
    let said = rows.with_item(key, |item| (item.author, item.time));
    let Some((Some(author), at)) = said else {
        let row = page_row(
            row_of(rows, key, feel, None, RowBuild::default()),
            RUN_GAP,
            measure,
        );
        return if first_unread { with_unread(row) } else { row };
    };
    let before = rows
        .index_of(&key.to_owned())
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| rows.key_at(index))
        .and_then(|key| rows.with_item(&key, |item| (item.author, item.time)));
    let clock = ConversationClock::local();
    let day = at.map(|at| clock.day(at.0));
    let new_day = day.is_some()
        && day
            != before
                .as_ref()
                .and_then(|(_, at)| at.map(|at| clock.day(at.0)));
    let continues = !new_day
        && before.as_ref().is_some_and(|(who, then)| {
            who.as_ref()
                .is_some_and(|who| who.identity == author.identity)
                && match (then, at) {
                    (Some(then), Some(at)) => {
                        (0..=RUN_WINDOW_SECS).contains(&at.0.saturating_sub(then.0))
                    }
                    _ => true,
                }
        });
    let Some((said, swipe, attached)) = bare_row(rows, key, feel, None, RowBuild::default()) else {
        return page_row(
            row_of(rows, key, feel, None, RowBuild::default()),
            RUN_GAP,
            measure,
        );
    };
    let avatar = Avatar::person(author.name.clone(), author.identity.clone());
    let message = Row::row().fill_width().align(Align::Start);
    let message = if continues {
        message.push(Box::new(avatar.room()))
    } else {
        message.push(Box::new(avatar))
    };
    let said = if continues {
        said
    } else {
        let name = author.name.clone();
        let mut head = Row::row().push(Box::new(
            Text::dynamic(move || name.clone()).with_role(TextRole::Label),
        ));
        if let Some(at) = at {
            let time = clock.clock(at.0);
            head = head.push(Box::new(
                Text::dynamic(move || time.clone())
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        Box::new(
            Stack::stack()
                .gap(Rhythm::Tight)
                .push(Box::new(head))
                .push(said),
        )
    };
    // The message swipes and shows its toolbar whole, its author with it; the day's divider
    // stays.
    let message = attach(
        Box::new(message.push_fit(said, Fit::fill(1.0))),
        swipe,
        attached,
    );
    let gap = if continues { Rhythm::Tight } else { RUN_GAP };
    let row = match day.filter(|_| new_day) {
        Some(day) => page_row(
            Box::new(
                Stack::stack()
                    .gap(RUN_GAP)
                    .push(Box::new(DayDivider::new(clock.day_label(day))))
                    .push(message),
            ),
            gap,
            measure,
        ),
        None => page_row(message, gap, measure),
    };
    if first_unread { with_unread(row) } else { row }
}

/// A transcript's row for the first message the reader had not read, with the "New messages"
/// divider over it: the divider is the toolkit's
/// ([`crate::widget::conversation::UnreadDivider`]), placed from what the app says is unread, so
/// every conversation draws the same one.
fn with_unread(row: Element) -> Element {
    Box::new(
        Page::full()
            .row_after(RUN_GAP)
            .push_slot(Slot::new(Box::new(
                crate::widget::conversation::UnreadDivider::new(),
            )))
            .push_slot(Slot::new(row)),
    )
}

/// The gap over a run of messages, and over a day's divider.
const RUN_GAP: Rhythm = Rhythm::Section;

/// `content` as a row of a transcript's page column, `gap` under the row before it, at `measure`
/// when the list keeps a page's readable width.
fn page_row(content: Element, gap: Rhythm, measure: Option<Length>) -> Element {
    let page = Page::full().row_after(gap);
    let page = match measure {
        Some(measure) => page.measure(measure),
        None => page,
    };
    Box::new(page.push_slot(Slot::new(content)))
}

/// The menu of the row under `at` (its focused row when the keyboard opened it): what its
/// item offers. Opening it selects the row, unless the row opens where it stands: that row
/// shows its item under itself, and a menu over it is not the window opening it elsewhere.
fn row_menu(list: &dyn Widget, at: Option<Point>, select: &Event) -> Vec<MenuEntry> {
    let Some(list) = list
        .as_any()
        .and_then(|list| list.downcast_ref::<VirtualList<DescribedRows>>())
    else {
        return Vec::new();
    };
    let key = match at {
        Some(point) => list.key_at_point(point),
        None => list.focused_row_key(),
    };
    let Some(key) = key else {
        return Vec::new();
    };
    let rows = list.model();
    let entries = rows
        .with_item(&key, |item| {
            let shown = item.item.clone();
            let menu = crate::widget::attached::item_menu(item.attached.menu, item.menu);
            (shown, crate::widget::attached::menu_entries(menu))
        })
        .unwrap_or_default();
    let (shown, entries) = entries;
    if entries.is_empty() {
        return Vec::new();
    }
    let opens_here = rows
        .index_of(&key)
        .is_some_and(|index| rows.opens_in_place(index));
    if let (Some(id), false) = (shown, opens_here) {
        select.fire(Some(Value::Resource(id.0)));
    }
    entries
}

impl Described for DescribedList {
    type Props = VirtualListProps;

    fn from_props(key: &str, props: VirtualListProps) -> Self {
        let face = Face::of(&props);
        let VirtualListProps {
            name,
            selection_follows_focus,
            in_column,
            follow_end,
            page_column,
            at_measure,
            tail_title,
            tree,
            ungrouped_title,
            multi_select,
            unseen_from,
            mut rows,
            removed,
            selection,
            chosen,
            scroll,
            open,
            motion,
            select,
            activate,
            selected: selected_changed,
            opened: opened_changed,
            closed: closed_changed,
        } = props;
        rows.removed = removed.0;
        rows.ungrouped = ungrouped_title;
        let feel = motion.unwrap_or(crate::motion::Feel::Standard);
        // The items the list's events read, kept as the list holds them.
        let reading = Rc::new(RefCell::new(rows.reader()));
        if page_column {
            // The divider of the unseen part stands over the row that shows the item: a row says
            // which item it shows, so the list finds it as the rows arrive.
            let marked = unseen_from.map(|item| item.0);
            let divider = marked.clone();
            let measure = at_measure.then_some(Length::Rem(MEASURE_REM));
            let mut list = VirtualList::new(rows, MESSAGE_ESTIMATE, move |rows, _, key| {
                message_of(rows, key, divider.as_deref(), feel, measure)
            })
            .flush()
            .with_edges(TranscriptEdges::new(RUN_GAP))
            .filling()
            .with_id(key)
            .with_feel(feel)
            // A message is read, not chosen: a tap on one gives it the keyboard and leaves
            // nothing tinted behind it.
            .reading();
            if follow_end {
                list = list.following_end();
            }
            let list = list.unread_from(marked);
            // A message offers its menu as every row does.
            let area = ContextMenuArea::over_content(Box::new(list), move |list, at| {
                row_menu(list, at, &Event::None)
            });
            return Self {
                body: ListBody::Menus(Box::new(area)),
                face,
                reading,
            };
        }
        // What the rows of a tree are: the list fills it as it mounts them, and reads it as it
        // builds each one.
        let marks = tree.then(|| Rc::new(TreeMarks::<String>::default()));
        let mut list = VirtualList::with_selection_delegate(rows, ROW_ESTIMATE, {
            let marks = marks.clone();
            move |rows, _, key, build| row_of(rows, key, feel, marks.as_deref(), build)
        });
        if let Some(marks) = &marks {
            list = list.with_tree(Rc::clone(marks));
        }
        if let Some(title) = tail_title {
            list = list.with_tail_title(title);
        }
        // A column's one list takes the column's height and scrolls its rows; a list among a
        // page's other content is as tall as its rows, and the page scrolls them with the rest.
        list = if in_column {
            list.filling().in_column()
        } else {
            list.fitting_rows()
        };
        // Whatever its rows, a list read to its end opens at the last one and stays there.
        if follow_end {
            list = list.following_end();
        }
        if selection_follows_focus {
            list = list.selection_follows_focus();
        }
        if multi_select {
            list = list.multi_select(true);
        }
        list = list.with_id(key).with_semantic_key(Clone::clone);
        if let Some(name) = name {
            list = list.with_accessible_name(name);
        }
        if let Some(keys) = selection {
            let mut chosen = KeySelection::new();
            for key in keys {
                chosen.toggle(key);
            }
            list = list.with_selection(chosen);
        }
        // The items a host chose for its person: their rows are the selection.
        if let Some(picked) = picked_rows(chosen, &reading.borrow(), 0) {
            list = list.with_selection(picked);
        }
        // Each event carries the item its row shows, read from the list's items.
        if !select.is_none() || !activate.is_none() {
            let (on_select, on_pick) = (select.clone(), activate.clone());
            let (selecting, picking) = (Rc::clone(&reading), Rc::clone(&reading));
            list = list
                .on_select(move |key, selected| {
                    if !selected {
                        return;
                    }
                    let item = item_of(&selecting.borrow(), &key);
                    if selection_follows_focus && by_hand() {
                        on_pick.fire(item);
                    } else {
                        on_select.fire(item);
                    }
                })
                .on_activate(move |key| {
                    // A removed item's row opens nothing: its mark shows what removed it.
                    if !picking.borrow().is_removed(&key) {
                        activate.fire(item_of(&picking.borrow(), &key));
                    }
                });
        }
        // What is selected now, as the items it is of: a selection a host keeps, so the list
        // it builds again shows it as the person left it. The list reports what a window keeps
        // (a choice made by hand, a choice of more than one row, and the way back to one or
        // none), so whatever arrives here is worth keeping.
        if !selected_changed.is_none() {
            let fired = selected_changed.clone();
            let selecting = Rc::clone(&reading);
            list = list.on_selected(move |keys| {
                let items: Vec<Value> = {
                    let selecting = selecting.borrow();
                    keys.iter()
                        .filter_map(|key| item_of(&selecting, key))
                        .collect()
                };
                fired.fire(Some(Value::List(items)));
            });
        }
        list = with_open(list, open.as_ref(), &reading.borrow());
        if !opened_changed.is_none() || !closed_changed.is_none() {
            let (opened, closed) = (opened_changed, closed_changed);
            let opening = Rc::clone(&reading);
            list = list
                .on_opened(move |key| {
                    opened.fire(item_of(&opening.borrow(), &key));
                })
                .on_closed(move || closed.fire(None));
        }
        if let Some(offset) = scroll {
            list.set_scroll_offset(offset);
        }
        let area = ContextMenuArea::over_content(Box::new(list), move |list, at| {
            row_menu(list, at, &select)
        });
        Self {
            body: ListBody::Menus(Box::new(area)),
            face,
            reading,
        }
    }

    /// The same list over the same rows: what the props say around the items is taken where
    /// the list stands, and items given whole are replaced where they stand. The person's
    /// scroll, the row their keyboard holds and what they opened stay. Rows a host renders
    /// change through the host; anything that makes the list another list builds it again.
    fn set_props(&mut self, props: VirtualListProps) -> bool {
        if Face::of(&props) != self.face {
            return false;
        }
        let VirtualListProps {
            selection_follows_focus,
            tail_title,
            ungrouped_title,
            multi_select,
            unseen_from,
            mut rows,
            removed,
            chosen,
            open,
            motion,
            select,
            activate,
            selected: selected_changed,
            opened: opened_changed,
            closed: closed_changed,
            ..
        } = props;
        let feel = motion.unwrap_or(crate::motion::Feel::Standard);
        rows.removed = removed.0;
        rows.ungrouped = ungrouped_title;
        let replaced = rendered(&rows).is_none();
        let unseen_from = unseen_from.as_ref().map(|item| item.0.clone());
        // The items given again, as the list's events read them.
        let reading = rows.reader();
        let picked = picked_rows(chosen, &reading, self.chosen_rows());
        *self.reading.borrow_mut() = reading;
        let reading = Rc::clone(&self.reading);
        let Some(list) = self.list_mut() else {
            return false;
        };
        list.set_multi_select(multi_select);
        list.set_tail_title(tail_title);
        list.set_feel(feel);
        if replaced {
            // The rows build anew where they stand: the list's scroll, its keyboard row and
            // the menus its rows open are the list's own and stay.
            list.update_model(move |model| *model = rows);
        }
        // The unseen row moved without the list's items changing: the list takes it, and the
        // rows it left and arrived at build again.
        list.set_unread(unseen_from);
        // The item the host holds open: its row grows into its document where it stands, and
        // an item that left the list opens nothing.
        let standing = open
            .as_ref()
            .and_then(|item| reading.borrow().key_presenting(&item.0));
        list.set_open(standing);
        if let Some(picked) = picked {
            list.set_selection(picked);
        }
        // Each event carries the item its row shows, read from the items the list holds now.
        let selecting = Rc::clone(&reading);
        let picking = Rc::clone(&reading);
        let (on_select, on_pick) = (select, activate.clone());
        list.set_on_select(move |key, selected| {
            if !selected {
                return;
            }
            let item = item_of(&selecting.borrow(), &key);
            if selection_follows_focus && by_hand() {
                on_pick.fire(item);
            } else {
                on_select.fire(item);
            }
        });
        list.set_on_activate(move |key| {
            // A removed item's row opens nothing: its mark shows what removed it.
            if !picking.borrow().is_removed(&key) {
                activate.fire(item_of(&picking.borrow(), &key));
            }
        });
        let fired = selected_changed;
        let opening = Rc::clone(&reading);
        list.set_on_selected(move |keys| {
            let items: Vec<Value> = {
                let selecting = reading.borrow();
                keys.iter()
                    .filter_map(|key| item_of(&selecting, key))
                    .collect()
            };
            fired.fire(Some(Value::List(items)));
        });
        if !opened_changed.is_none() {
            list.set_on_opened(move |key| {
                opened_changed.fire(item_of(&opening.borrow(), &key));
            });
        }
        if !closed_changed.is_none() {
            list.set_on_closed(move || closed_changed.fire(None));
        }
        true
    }
}

impl Decorator for DescribedList {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        match &self.body {
            ListBody::Menus(area) => area.as_ref(),
        }
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.body {
            ListBody::Menus(area) => area.as_mut(),
        }
    }

    fn name(&self) -> &'static str {
        "DescribedList"
    }

    /// The list's change is the described list's: it adds no structure of its own, so rows
    /// the list builds anew stay mounted in place and the window keeps its tree.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.inner_mut().take_structure_change()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Event, Fired, Intent};
    use crate::widget::MenuItemProps;

    /// The rows of a list given its items whole: a row a title each.
    fn given_rows(count: usize) -> DescribedRows {
        let items = (0..count)
            .map(|at| ListItemProps {
                key: format!("row-{at}"),
                row: ItemRow::Row(Box::new(ListRowProps {
                    title: format!("row {at}"),
                    ..ListRowProps::default()
                })),
                ..ListItemProps::default()
            })
            .collect();
        DescribedRows {
            held: Held::Given(Rc::new(items)),
            ..DescribedRows::default()
        }
    }

    /// The props of a list of `rows` that is a column's whole body.
    fn column_of(rows: DescribedRows) -> VirtualListProps {
        VirtualListProps {
            in_column: true,
            rows,
            ..VirtualListProps::default()
        }
    }

    /// Lay the list out in a column of `height`, as the window does.
    fn lay_out(list: &mut VirtualList<DescribedRows>, height: f32) {
        lay_out_tree(list, height);
    }

    /// Lay the list out in a column of `height` and give back the tree it was laid out in, for
    /// the events sent to it afterwards.
    fn lay_out_tree(
        list: &mut VirtualList<DescribedRows>,
        height: f32,
    ) -> (taffy::TaffyTree<()>, taffy::NodeId) {
        list.set_viewport_height(height);
        let doc = automerge::AutoCommit::new();
        Widget::sync(list, &doc, &automerge::ROOT);
        let mut taffy = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let node = Widget::build_layout(list, &mut taffy, &crate::theme::Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(height),
                },
            )
            .unwrap();
        Widget::apply_layout(list, &taffy, node, Point::ZERO);
        (taffy, node)
    }

    /// The items of a list where each carries a document of its own: they open where they
    /// stand.
    fn rows_with_documents(count: usize) -> DescribedRows {
        let items = (0..count)
            .map(|at| ListItemProps {
                key: format!("row-{at}"),
                item: Some(ResourceId(format!("task-{at}"))),
                row: ItemRow::Row(Box::new(ListRowProps {
                    title: format!("row {at}"),
                    ..ListRowProps::default()
                })),
                detail: ItemRow::Row(Box::new(ListRowProps {
                    title: format!("what row {at} is about"),
                    secondary: Some("with the notes written about it".to_owned()),
                    ..ListRowProps::default()
                })),
                ..ListItemProps::default()
            })
            .collect();
        DescribedRows {
            held: Held::Given(Rc::new(items)),
            ..DescribedRows::default()
        }
    }

    /// The rows of a list whose items offer on their menu the one command their type offers:
    /// each opens where it stands when `opens_here`, and elsewhere when not.
    fn rows_offering(count: usize, opens_here: bool) -> DescribedRows {
        let items = (0..count)
            .map(|at| ListItemProps {
                key: format!("row-{at}"),
                item: Some(ResourceId(format!("task-{at}"))),
                row: ItemRow::Row(Box::new(ListRowProps {
                    title: format!("row {at}"),
                    ..ListRowProps::default()
                })),
                detail: if opens_here {
                    ItemRow::Row(Box::new(ListRowProps {
                        title: format!("what row {at} is about"),
                        ..ListRowProps::default()
                    }))
                } else {
                    ItemRow::None
                },
                menu: vec![MenuPart::Item(MenuItemProps {
                    label: "Complete".to_owned(),
                    ..MenuItemProps::default()
                })],
                ..ListItemProps::default()
            })
            .collect();
        DescribedRows {
            held: Held::Given(Rc::new(items)),
            ..DescribedRows::default()
        }
    }

    /// Lay `list` out in a column of `height` and press `button` over the row `index`, as a
    /// person does: the left button to tap the row, the right one to see what its item offers
    /// there.
    fn press_row(
        list: &mut DescribedList,
        index: usize,
        button: crate::winit::event::MouseButton,
        height: f32,
    ) {
        let (taffy, node) = {
            let inner = list.list_mut().expect("the list stands");
            lay_out_tree(inner, height)
        };
        let row = list
            .list()
            .and_then(|list| list.row_rect(&format!("row-{index}")))
            .expect("the row is laid out");
        let at = Point::new(20.0, row.center().y);
        let pressed = crate::winit::event::ElementState::Pressed;
        let mut events = vec![
            crate::synthetic_input::cursor_moved(at.x, at.y),
            crate::synthetic_input::mouse_input(pressed, button),
        ];
        if button == crate::winit::event::MouseButton::Left {
            events.push(crate::synthetic_input::mouse_input(
                crate::winit::event::ElementState::Released,
                button,
            ));
        }
        for event in &events {
            crate::testing::Dispatch::new()
                .layout(&taffy, node)
                .send(list, event);
        }
    }

    /// A page column's rows are read, not chosen: a press on a message gives it the keyboard
    /// and leaves nothing tinted behind, and the keyboard's way through the rows moves (and
    /// rings) the row it is on without choosing that one either. A column's rows stand for
    /// what a window opens, and are chosen as they always were.
    #[test]
    fn a_press_on_a_transcript_row_chooses_nothing() {
        let left = crate::winit::event::MouseButton::Left;
        let mut transcript = DescribedList::from_props(
            "transcript",
            VirtualListProps {
                follow_end: true,
                page_column: true,
                rows: given_rows(6),
                ..VirtualListProps::default()
            },
        );
        press_row(&mut transcript, 1, left, 400.0);
        let list = transcript.list_mut().expect("the list stands");
        assert_eq!(
            list.selection_len(),
            0,
            "a message is read, not chosen: nothing stays selected"
        );
        assert_eq!(
            list.focused_key().map(String::as_str),
            Some("row-1"),
            "the row the person pressed takes the keyboard"
        );
        // The keyboard's way through the rows rings the row it is on, and chooses none.
        let (taffy, node) = lay_out_tree(list, 400.0);
        Widget::set_focus(list, true);
        let up = crate::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowUp);
        assert!(
            crate::testing::Dispatch::new()
                .layout(&taffy, node)
                .send(list, &up)
                .handled,
            "the list follows the keyboard"
        );
        assert_eq!(
            list.focused_key().map(String::as_str),
            Some("row-0"),
            "the keyboard moved up a row"
        );
        assert_eq!(list.selection_len(), 0, "no row was chosen on the way");

        // A column's rows are chosen, as a list of things to open always was.
        let mut column = DescribedList::from_props("column", column_of(given_rows(6)));
        press_row(&mut column, 1, left, 400.0);
        let list = column.list().expect("the list stands");
        assert_eq!(list.selection_len(), 1, "a column's rows are chosen");
    }

    /// A menu opened over a row that opens where it stands is about that row alone: choosing
    /// the row shows its item under it, so opening the menu reports no choice of it for the
    /// window to open the item elsewhere.
    #[test]
    fn a_menu_over_a_row_that_opens_in_place_reports_no_choice() {
        let queue = crate::Intents::<Fired>::default();
        let select = Event::View(Intent::new(&queue, "list", "select"));
        let props = VirtualListProps {
            select,
            ..column_of(rows_offering(4, true))
        };
        let mut list = DescribedList::from_props("list", props);
        press_row(&mut list, 1, crate::winit::event::MouseButton::Right, 400.0);
        assert!(
            list.menu_area_mut()
                .as_deref()
                .is_some_and(ContextMenuArea::is_open),
            "the row's menu opens where the press was"
        );
        assert!(
            queue.take().is_empty(),
            "the list reports no choice of a row it opens where it stands"
        );

        // A row of a list whose items carry no document opens elsewhere, and the menu over it
        // chooses it, as a press on it would.
        let props = VirtualListProps {
            select: Event::View(Intent::new(&queue, "list", "select")),
            ..column_of(rows_offering(4, false))
        };
        let mut plain = DescribedList::from_props("list", props);
        press_row(
            &mut plain,
            1,
            crate::winit::event::MouseButton::Right,
            400.0,
        );
        assert!(
            plain
                .menu_area_mut()
                .as_deref()
                .is_some_and(ContextMenuArea::is_open),
            "the row's menu opens where the press was"
        );
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "the menu chooses the row: {fired:?}");
        assert_eq!(fired[0].event, "select");
    }

    /// The item a host holds open stands open where it stands: the list it builds again shows
    /// it as the person left it, and an item it does not hold open does not.
    #[test]
    fn the_item_a_host_holds_open_stands_open() {
        let props = VirtualListProps {
            open: Some(ResourceId("task-2".into())),
            ..column_of(rows_with_documents(4))
        };
        let mut list = DescribedList::from_props("list", props);
        let inner = list.list_mut().expect("the list stands");
        assert_eq!(
            inner.open_row().map(String::as_str),
            Some("row-2"),
            "the item the host holds open is the row standing open"
        );
        lay_out(inner, 400.0);
        let closed = inner.viewport.item_height(0).unwrap_or_default();
        let open = inner.viewport.item_height(2).unwrap_or_default();
        assert!(
            open > closed,
            "the row grows into its document: {open} over a closed row's {closed}"
        );

        // The same list given the item again keeps it open, and given none closes it.
        let same = VirtualListProps {
            open: Some(ResourceId("task-2".into())),
            ..column_of(rows_with_documents(4))
        };
        assert!(
            list.set_props(same),
            "the list takes its items where it stands"
        );
        assert_eq!(
            list.list()
                .and_then(VirtualList::open_row)
                .map(String::as_str),
            Some("row-2")
        );
        let closed = VirtualListProps {
            open: None,
            ..column_of(rows_with_documents(4))
        };
        assert!(
            list.set_props(closed),
            "the list takes its items where it stands"
        );
        assert_eq!(list.list().and_then(VirtualList::open_row), None);
    }

    /// The person opening and closing the row they read is reported as the item it shows, so a
    /// host keeping it can put it back.
    #[test]
    fn the_person_opening_a_row_reports_the_item_it_shows() {
        let queue = crate::Intents::<Fired>::default();
        let opened = Event::View(Intent::new(&queue, "list", "opened"));
        let closed = Event::View(Intent::new(&queue, "list", "closed"));
        let props = VirtualListProps {
            opened: opened.clone(),
            closed: closed.clone(),
            ..column_of(rows_with_documents(4))
        };
        let mut list = DescribedList::from_props("list", props);
        let inner = list.list_mut().expect("the list stands");
        lay_out(inner, 400.0);
        inner.activate_key("row-1".to_owned());
        assert_eq!(inner.open_row().map(String::as_str), Some("row-1"));
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "the host hears one event: {fired:?}");
        assert_eq!(fired[0].event, "opened");
        assert!(
            matches!(fired[0].value.clone(), Some(Value::Resource(id)) if id == "task-1"),
            "the host hears the item the person opened: {fired:?}"
        );
        inner.close_open();
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "the host hears it close: {fired:?}");
        assert_eq!(fired[0].event, "closed");
    }

    /// A list given its items again keeps where the person scrolled: the items are replaced
    /// where the list stands, not the list built again around them.
    #[test]
    fn a_list_given_its_items_again_keeps_where_the_person_scrolled() {
        let mut list = DescribedList::from_props("list", column_of(given_rows(200)));
        {
            let inner = list.list_mut().expect("the list stands");
            lay_out(inner, 200.0);
            lay_out(inner, 200.0);
            inner.set_scroll_offset(600.0);
            assert!(inner.scroll_offset() > 0.0, "the list scrolled");
        }
        let scrolled = list.list().expect("the list stands").scroll_offset();
        let props = VirtualListProps {
            tail_title: Some("Done".into()),
            ..column_of(given_rows(200))
        };
        assert!(
            list.set_props(props),
            "the list takes its items where it stands"
        );
        let inner = list.list().expect("the list stands");
        assert!(
            (inner.scroll_offset() - scrolled).abs() < 0.01,
            "the rows stayed where the person scrolled them: {} then {}",
            scrolled,
            inner.scroll_offset()
        );
    }

    /// A menu command that reports its choose to `queue` when run.
    fn command(label: &str, key: &str, queue: &crate::Intents<Fired>) -> MenuPart {
        let mut item = MenuItemProps {
            label: label.to_owned(),
            key: Some(key.to_owned()),
            ..MenuItemProps::default()
        };
        item.choose = Event::View(Intent::new(queue, "row", "choose"));
        MenuPart::Item(item)
    }

    /// The row's menus reach it with the keys they declare, matched as every shortcut is.
    #[test]
    fn a_row_reads_the_shortcuts_its_menu_declares() {
        let queue = crate::Intents::<Fired>::default();
        let menu = vec![command("Complete", "Ctrl+Enter", &queue)];
        let attached = vec![
            command("Double click", "Double click", &queue),
            MenuPart::Separator,
        ];
        let shortcuts = declared_shortcuts(&menu, &attached);
        assert_eq!(shortcuts.len(), 1, "text that is no chord names no key");
        assert!(
            shortcuts[0]
                .0
                .same_press(&crate::platform::Shortcut::from("Control+Return"))
        );
        (shortcuts[0].1)();
        assert_eq!(
            queue.take(),
            [Fired {
                key: "row".to_owned(),
                event: "choose".to_owned(),
                value: None,
            }]
        );
    }

    /// One key runs one command: the first the menus offer it on holds it.
    #[test]
    fn one_key_runs_one_command_of_the_row() {
        let queue = crate::Intents::<Fired>::default();
        let menu = vec![command("Complete", "Ctrl+Enter", &queue)];
        let attached = vec![command("Reopen", "control+enter", &queue)];
        assert_eq!(declared_shortcuts(&menu, &attached).len(), 1);
    }
}
