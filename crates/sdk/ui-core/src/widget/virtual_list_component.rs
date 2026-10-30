//! [`VirtualList`] as a described component: a list of items, each built into its row when it
//! scrolls into view, with what the row offers on its item (a context menu, a swipe) and who
//! else is at it.
//!
//! Its rows are [`ListItemProps`]: each holds the row itself (a [`ListRow`], or any view) and
//! what the list does with it. A host that renders its rows only as they are shown hands them
//! over as a [`SharedRows`] ([`Source::rows`]): a list of ten thousand items costs the rows on
//! screen, and the host queues what changed for the list to take. Rows given as components are
//! held whole.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use vello::kurbo::Point;

use crate::component::{
    Accepts, Component, Described, Event, Moment, Person, ResourceId, SharedRows, SlotType, Source,
    Value,
};
use crate::selection::KeySelection;
use crate::tokens::{Rhythm, TextRole};
use crate::widget::arrange::{Align, Fit, Page, Row, Slot, Stack, TranscriptEdges};
use crate::widget::conversation::list::RUN_WINDOW_SECS;
use crate::widget::conversation::{ConversationClock, DayDivider};
use crate::widget::{
    Attachments, Avatar, ContextMenuArea, Decorator, Element, ListRow, ListRowProps, MenuEntry,
    MenuPart, SwipeActionProps, Text, Widget,
};

use super::{ListModel, ListModelChange, VirtualList};
use crate::widget::ChangeMarkProps;

/// The height a row is reckoned at before it is laid out: a row with a secondary line.
// ratchet-allow(pixel_const): a height reckoned before layout measures it, not spacing
const ROW_ESTIMATE: f32 = 56.0;

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
    /// opens it, and selects the row): each of its allowed actions, after the menu the row's
    /// view placed (`attached`) and without those that menu placed already, so every action
    /// stays reachable.
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
    /// The item was removed and the person has not seen it go: the row stands in for it, its
    /// title struck through, at the row's own height. The UI host places such rows.
    pub removed: bool,
    /// The row: a `ui.ListRow`, or any view.
    #[component(slot(default))]
    pub row: ItemRow,
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
    /// A conversation: each row stands in the page column, the list opens at its newest row
    /// and follows it.
    pub transcript: bool,
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
    /// How far the list is scrolled, in logical pixels.
    #[component(state)]
    pub scroll: Option<f64>,
    /// A row was selected: carries the item it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A row was activated (a click, Enter, an assistive technology): carries the item it
    /// shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
}

/// The items of a described list: read from a host that renders them as they are shown, or
/// given whole.
#[derive(Default)]
pub struct DescribedRows {
    held: Held,
    /// Whether a row the list built shows an avatar: the rows without one then keep its room.
    /// Once on it stays on, so rows do not shift as others scroll in and out.
    avatars: Cell<bool>,
    /// The rows built keeping no room for an avatar, before one came in.
    spaced: RefCell<Vec<String>>,
    /// The rows to build again: the avatar column came in after they were built.
    restyled: RefCell<Vec<String>>,
    /// Rows standing in for removed items, after the items ([`VirtualListProps::removed`]).
    removed: Rc<Vec<ListItemProps>>,
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
            ..Self::default()
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

    /// The row of `key` to build: its props, with the room for an avatar kept while the list
    /// shows avatars.
    fn row_props(&self, row: &mut ListRowProps, key: &str) {
        if row.avatar.is_some() {
            if !self.avatars.replace(true) {
                self.restyled
                    .borrow_mut()
                    .append(&mut self.spaced.borrow_mut());
            }
        } else if row.avatar_space && !self.avatars.get() {
            row.avatar_space = false;
            self.spaced.borrow_mut().push(key.to_owned());
        }
    }

    /// The key of the item showing the resource `id`.
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

/// An item given whole, again: its row's props are built anew each time it scrolls in.
fn reusable(item: &ListItemProps) -> Option<ListItemProps> {
    let row = match &item.row {
        ItemRow::None => ItemRow::None,
        ItemRow::Row(row) => ItemRow::Row(Box::new(row.reusable()?)),
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
        removed: item.removed,
        row,
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
        let mut changes = match &self.held {
            Held::Rendered(rows) => rows
                .try_borrow_mut()
                .map(|mut rows| rows.take_changes())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let restyled = std::mem::take(&mut *self.restyled.borrow_mut());
        if !restyled.is_empty() {
            changes.push(ListModelChange::RowsChanged { keys: restyled });
        }
        changes
    }

    fn needs_poll(&self) -> bool {
        let waiting = match &self.held {
            Held::Rendered(rows) => rows.try_borrow().is_ok_and(|rows| rows.has_changes()),
            _ => false,
        };
        waiting || !self.restyled.borrow().is_empty()
    }

    fn label(&self, index: usize) -> Option<String> {
        let key = self.key_at(index)?;
        self.with_item(&key, |item| match item.row {
            ItemRow::Row(row) if !row.title.is_empty() => Some(row.title),
            _ => None,
        })
        .flatten()
    }
}

/// The list a described [`VirtualList`] is: over its items, under the one context menu its
/// rows share.
pub struct DescribedList {
    body: ListBody,
    /// What the list was made with, but its items: the same again is taken in place.
    face: Face,
}

/// The props of a described list that decide the widget, its items aside.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    selection_follows_focus: bool,
    in_column: bool,
    transcript: bool,
    /// The rows the host renders as they are shown, by identity: they then change through
    /// the host, never through the props. `None` for items given whole.
    rendered: Option<usize>,
    /// The removed items' rows, by key.
    removed: Vec<String>,
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
    pub fn select_presenting(&mut self, id: Option<&str>) -> bool {
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
}

/// Whether the selection being made comes from a click or a tap the person just made, rather
/// than the keyboard or an assistive technology.
fn by_hand() -> bool {
    matches!(
        crate::motion::recent_input(crate::time::now()),
        Some(crate::tokens::MotionSource::Pointer | crate::tokens::MotionSource::Gesture)
    )
}

/// The resource the item `key` shows, as an event carries it.
fn item_of(rows: &DescribedRows, key: &str) -> Option<Value> {
    rows.with_item(key, |item| item.item.map(|id| Value::Resource(id.0)))
        .flatten()
}

/// The row of an item: its list row with the item's presence, or its view; under the swipe
/// and beside the hover toolbar the item has.
fn row_of(rows: &DescribedRows, key: &str) -> Element {
    match bare_row(rows, key) {
        Some((row, swipe, attached)) => attach(row, swipe, attached),
        None => Box::new(ListRow::new(key.to_owned(), "")),
    }
}

/// The row of an item, with the swipe and what its view attached, to put around it (or around
/// the message it is in).
fn bare_row(
    rows: &DescribedRows,
    key: &str,
) -> Option<(Element, Option<SwipeActionProps>, Attachments)> {
    rows.with_item(key, |item| {
        let ListItemProps {
            presence,
            swipe,
            attached,
            row,
            change,
            removed,
            ..
        } = item;
        // What someone else changed and the person has not seen, at the row's own height: on a
        // list row their face on its trailing edge and their colour under it; beside a row's
        // own view their face alone.
        let row = match row {
            ItemRow::Row(mut props) => {
                rows.row_props(&mut props, key);
                // The list activates its rows itself, by key: a row of it is not a target of
                // its own.
                props.activate = Event::None;
                let mut row = ListRow::from_props(key, *props);
                if let Some(person) = presence {
                    row = row.presence(person.identity);
                }
                if removed {
                    row = row.struck();
                }
                if let Some(mark) = change.and_then(ChangeMarkProps::row) {
                    row = row.change(mark.identity, mark.name, mark.spoken, mark.show);
                }
                Box::new(row) as Element
            }
            ItemRow::View(view) => with_mark(
                view,
                change.and_then(|change| change.face(&format!("{key}~change"))),
            ),
            ItemRow::None => Box::new(ListRow::new(key.to_owned(), "")),
        };
        (row, swipe, attached)
    })
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
fn message_of(rows: &DescribedRows, key: &str) -> Element {
    let said = rows.with_item(key, |item| (item.author, item.time));
    let Some((Some(author), at)) = said else {
        return page_row(row_of(rows, key), RUN_GAP);
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
    let Some((said, swipe, attached)) = bare_row(rows, key) else {
        return page_row(row_of(rows, key), RUN_GAP);
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
    match day.filter(|_| new_day) {
        Some(day) => page_row(
            Box::new(
                Stack::stack()
                    .gap(RUN_GAP)
                    .push(Box::new(DayDivider::new(clock.day_label(day))))
                    .push(message),
            ),
            gap,
        ),
        None => page_row(message, gap),
    }
}

/// The gap over a run of messages, and over a day's divider.
const RUN_GAP: Rhythm = Rhythm::Section;

/// `content` as a row of a transcript's page column, `gap` under the row before it.
fn page_row(content: Element, gap: Rhythm) -> Element {
    Box::new(Page::full().row_after(gap).push_slot(Slot::new(content)))
}

/// The menu of the row under `at` (its focused row when the keyboard opened it): what its
/// item offers. Opening it selects the row.
fn row_menu(list: &dyn Widget, at: Option<Point>, select: &Event) -> Vec<MenuEntry> {
    let Some(list) = list
        .as_any()
        .and_then(|list| list.downcast_ref::<VirtualList<DescribedRows>>())
    else {
        return Vec::new();
    };
    let key = match at {
        Some(point) => list.key_at_point(point),
        None => list.focused_key().cloned(),
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
    if let Some(id) = shown {
        select.fire(Some(Value::Resource(id.0)));
    }
    entries
}

impl Described for DescribedList {
    type Props = VirtualListProps;

    fn from_props(key: &str, props: VirtualListProps) -> Self {
        let VirtualListProps {
            name,
            selection_follows_focus,
            in_column,
            transcript,
            mut rows,
            removed,
            selection,
            scroll,
            select,
            activate,
        } = props;
        let face = Face {
            name: name.clone(),
            selection_follows_focus,
            in_column,
            transcript,
            rendered: rendered(&rows),
            removed: removed.keys(),
        };
        rows.removed = removed.0;
        if transcript {
            let list =
                VirtualList::new(rows, MESSAGE_ESTIMATE, |rows, _, key| message_of(rows, key))
                    .flush()
                    .with_edges(TranscriptEdges::new(RUN_GAP))
                    .filling()
                    .following_end()
                    .with_id(key);
            // A message offers its menu as every row does.
            let area = ContextMenuArea::over_content(Box::new(list), move |list, at| {
                row_menu(list, at, &Event::None)
            });
            return Self {
                body: ListBody::Menus(Box::new(area)),
                face,
            };
        }
        let reader = rows.reader();
        let mut list =
            VirtualList::with_selection_delegate(rows, ROW_ESTIMATE, |rows, _, key, _| {
                row_of(rows, key)
            })
            .filling();
        if in_column {
            list = list.in_column();
        }
        if selection_follows_focus {
            list = list.selection_follows_focus();
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
        // Each event carries the item its row shows, read from the list's items.
        if !select.is_none() || !activate.is_none() {
            let (on_select, on_pick) = (select.clone(), activate.clone());
            let (selecting, picking) = (reader.reader(), reader.reader());
            list = list
                .on_select(move |key, selected| {
                    if !selected {
                        return;
                    }
                    let item = item_of(&selecting, &key);
                    if selection_follows_focus && by_hand() {
                        on_pick.fire(item);
                    } else {
                        on_select.fire(item);
                    }
                })
                .on_activate(move |key| {
                    // A removed item's row opens nothing: its mark shows what removed it.
                    if !picking.is_removed(&key) {
                        activate.fire(item_of(&picking, &key));
                    }
                });
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
        }
    }

    /// The same list over items its host renders: it stays, and its items change through
    /// the host. Anything else builds it again.
    fn set_props(&mut self, props: VirtualListProps) -> bool {
        let face = Face {
            name: props.name.clone(),
            selection_follows_focus: props.selection_follows_focus,
            in_column: props.in_column,
            transcript: props.transcript,
            rendered: rendered(&props.rows),
            removed: props.removed.keys(),
        };
        face.rendered.is_some() && face == self.face
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
