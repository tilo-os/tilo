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

use crate::widget::arrange::{Align, Fit, MEASURE_REM, Page, Part, Row, Stack, TranscriptEdges};
use crate::widget::message_day::ConversationClock;
use crate::widget::message_day::GROUPING_WINDOW_SECS;
use crate::widget::section_divider::SectionDivider;
use crate::{
    widget::Attachments, widget::Avatar, widget::ContextMenuArea, widget::ListRow,
    widget::ListRowProps, widget::MenuEntry, widget::MenuPart, widget::RowBuild,
    widget::SwipeActionProps, widget::Text,
};
use tilo_ui_base::selection::KeySelection;
use tilo_ui_base::widget::Length;
use tilo_ui_base::{
    component::Accepts, component::Component, component::Described, component::Event,
    component::Moment, component::PartType, component::Person, component::ResourceId,
    component::SharedRows, component::Source, component::Value,
};
use tilo_ui_base::{tokens::Rhythm, tokens::TextRole};
use tilo_ui_base::{
    widget::AvatarSize, widget::Decorator, widget::Element, widget::Spacer, widget::Tone,
    widget::Widget,
};

use super::{ListModel, TreeMarks, VirtualList};
use crate::widget::ChangeMarkProps;
use tilo_ui_base::collection_transition::ListModelChange;

#[path = "virtual_list_facts.rs"]
mod facts;

/// The height a row is reckoned at before it is laid out: a row with a secondary line.
pub(crate) const ROW_ESTIMATE: f32 = 56.0;

/// The height a message of a transcript is reckoned at before it is measured.
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
    /// What the row stands for when it shows no resource of the workspace (a path on this
    /// device): what the list's `select_value` and `selected_values` report of it, and what its
    /// `chosen_values` names it by. Activating such a row (a double click, Enter, an assistive
    /// technology) runs its own row's `activate`, since the row knows what it is and the list
    /// does not.
    pub value: Option<String>,
    /// Who else is at the item now (a member's or an agent's lens on it), shown on the
    /// row.
    pub presence: Option<Person>,
    /// Who wrote it, in a transcript: the list shows their avatar, their name and the time
    /// over each run of their consecutive messages, so the row holds only what they wrote.
    pub author: Option<Person>,
    /// When it was written, in a transcript: the time beside its author's name, and a divider
    /// over the first message of each day.
    pub time: Option<Moment>,
    /// Who else has read to it (`seen_by(c, m)`), in a transcript: their faces, small, once,
    /// on the line where their reading ends — each of them shows on no other line, and the
    /// author never beside their own words.
    pub read_by: Vec<Person>,
    /// What the item offers on its context menu (a right-click, the Menu key or a long press
    /// opens it, and selects the row unless the row opens where it stands): each of its
    /// allowed actions, after the menu the row's view placed (`attached`) and without those
    /// that menu placed already, so every action stays reachable.
    #[component(part)]
    pub menu: Vec<MenuPart>,
    /// The item offers a menu the row does not carry: opening the row's menu asks for the row
    /// again with it ([`RowSource::ask_menu`]), and the menu opens when it comes.
    pub offers_menu: bool,
    /// The keys the item's actions answer to ("Ctrl+Enter"): a press of one while the list holds
    /// the keyboard on the row reports `shortcut` with it, whether or not the row carries its
    /// menu, and whether the row is one list row or a view of any kind.
    pub keys: Vec<String>,
    /// A key of `keys` was pressed on the row: carries it.
    #[component(event(value = String))]
    pub shortcut: Event,
    /// What swiping the row from its trailing edge does, unless its view says
    /// (`attached`).
    #[component(part)]
    pub swipe: Option<SwipeActionProps>,
    /// What the row's view attached to it (`ui.ContextMenu`, `ui.HoverToolbar`, `ui.Swipe`
    /// anywhere in the row's view): its own menu, first in the row's menu; a toolbar shown
    /// while the row is hovered or holds focus; a swipe either way.
    #[component(part)]
    pub attached: Attachments,
    /// What someone else changed of the item that the person has not seen yet: the row shows
    /// it, in their colour, on its trailing edge. The UI host places it (task-legibility §3).
    #[component(part)]
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
    /// The item is one the person has finished with — its box checked off — though its row may
    /// still stand in the list until they leave the place: its title reads struck through, as a
    /// checked-off row's does, and a heading counts what is left to do, so it does not count this
    /// row ([`crate::widget::heading_count`]).
    pub done: bool,
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
    #[component(part(default))]
    pub row: ItemRow,
    /// The item's document, shown in the row's place while the row stands open into it: a row with a
    /// document opens where it stands in the list when it is chosen, and the list's own motion grows
    /// the row to hold it. The document opens with the item's own leading control and title, in the
    /// row's column — the row becomes the item, its title written once — and what the item holds
    /// follows under them.
    #[component(part(when_open))]
    pub detail: ItemRow,
    /// The row opens into a document of its own where it stands: what its view gives as `detail`
    /// comes once it stands open. The list's rows offer to open when this says so, whether or not
    /// one carries its document yet.
    pub opens: bool,
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
impl PartType for ItemRow {
    const MANY: bool = false;

    fn accepts() -> Accepts {
        Accepts::View
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let kinds = source.part_kinds(part);
        if matches!(kinds.as_slice(), [Some(kind)] if kind == ListRowProps::NAME) {
            let mut row = None;
            source.components(part, &mut |child| {
                row = Some(ListRowProps::from_source(child).map_err(|error| error.to_string())?);
                Ok(())
            })?;
            *self = row.map_or(Self::None, |row| Self::Row(Box::new(row)));
            return Ok(());
        }
        let mut views = source.children(part);
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
    /// The window is too narrow to hold an item under its row: a row that carries an item opens
    /// it in front of the list, where the window's back leads back to the rows, and a row the
    /// host held open closes as the window narrows.
    pub narrow: bool,
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
    /// The title over the group of rows whose item names no group: a list grouped by a thing some items
    /// lack (a day, a place) heads those rows with it, never folding them under the last group that
    /// names one. The rows keep the order their list gives.
    pub ungrouped_title: Option<String>,
    /// The title of the view this list stands under, as the view's header reads it, and how
    /// many things it counts beside it (`view_count`): a list whose rows all stand in one group
    /// heads them with nothing when that heading says what the title already says — the same
    /// words, counting the same rows — since the title over the list says it once. The host
    /// fills both from the shell of the window the list stands in
    /// ([`VirtualList::in_view`]).
    pub view_title: Option<String>,
    /// How many things the view's title counts beside it (`view_title`).
    pub view_count: Option<i64>,
    /// As tall as its rows, but no taller than this, in logical pixels, and it scrolls what it
    /// does not show: a list in a place that hugs what it holds (a roster in a panel, a picker)
    /// whose rows would otherwise make the place as tall as all of them. A list that takes its
    /// parent's height already (`in_column`) needs none.
    pub most: Option<f64>,
    /// The list holds more than one selected row: a click with ctrl (or cmd) adds or takes
    /// away the row it lands on, one with shift takes the range from the last row chosen, and
    /// Escape puts the selection away. Without it a click chooses the one row the window shows.
    pub multi_select: bool,
    /// The item where the person looking has seen nothing yet (`first_unseen(c)`): the toolkit's
    /// [`SectionDivider`](crate::widget::section_divider::SectionDivider) stands over the row that shows
    /// it, and while that is above the view a pill at the foot offers the way back to it
    /// ([`VirtualList::following_end`]). `None` draws neither.
    pub unseen_from: Option<ResourceId>,
    /// The items, in order.
    #[component(part(default))]
    pub rows: DescribedRows,
    /// Items someone else removed that the person has not seen go: each a row after the
    /// items, its title struck through. The UI host places them.
    #[component(part)]
    pub removed: RemovedRows,
    /// The selected rows, by key.
    #[component(state)]
    pub selection: Option<Vec<String>>,
    /// The rows selected, by the values they stand for (`ListItem.value`): a selection the host
    /// keeps for its person, given back whole each time, so a list whose rows come and go (a
    /// folder read again, another folder) selects what the host chose and nothing else. Empty
    /// selects nothing; none leaves the list's own choice.
    pub chosen_values: Option<Vec<String>>,
    /// The items selected, in the order they stand: the selection a host keeps for its person,
    /// given back so a list built again shows it as they left it. An item whose row the list does
    /// not hold yet — a list whose host renders its rows as they show — is held and selected as
    /// its row arrives.
    #[component(state)]
    pub chosen: Option<Vec<ResourceId>>,
    /// How far the list is scrolled, in logical pixels.
    #[component(state)]
    pub scroll: Option<f64>,
    /// The item held open into its document where it stands in the list: its row grows to
    /// hold what the item's row gives as its `detail`, and Escape or a press beside it closes
    /// it. A host keeps which item is open and gives it back, so a list it builds again shows
    /// the item as the person left it.
    #[component(state)]
    pub open: Option<ResourceId>,
    /// The row the view asks the list to show, by its key: a row below the fold scrolls into
    /// view at the list's motion, without taking the keyboard or the selection, as an address
    /// book does for the first card under a letter. It moves the list when the key it names
    /// changes; a key the list does not hold yet is shown as soon as it does, and one already
    /// in view stays where the person scrolled it.
    pub reveal: Option<String>,
    /// The row the list brought up for `reveal`, once it moved to it: the view takes the ask back
    /// on it (`reveal: .none`), so the same row can be asked for again after a person scrolled
    /// away.
    #[component(event(value = ResourceId))]
    pub revealed: Event,
    /// The pace its rows arrive, leave and change places at, and the rows it builds move at
    /// when they say nothing themselves: the theme's unless it says otherwise.
    pub motion: Option<tilo_ui_base::motion::Feel>,
    /// A row was selected: carries the item it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A row was activated (a click, Enter, an assistive technology): carries the item it
    /// shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
    /// The edit key was pressed on the focused row: carries the item it shows, for the app's
    /// own edit action.
    #[component(event(value = ResourceId))]
    pub edit: Event,
    /// Delete was pressed on the focused row: carries the item it shows, for the app's
    /// destructive action.
    #[component(event(value = ResourceId))]
    pub delete: Event,
    /// The selection changed: carries the items selected, in the order they stand, and none
    /// when the selection was put away.
    #[component(event)]
    pub selected: Event,
    /// A row that stands for a value was selected: carries its value.
    #[component(event(value = String))]
    pub select_value: Event,
    /// The selection changed: carries the values of the rows selected that stand for one, in the
    /// order they stand.
    #[component(event(value = Vec<String>))]
    pub selected_values: Event,
    /// The person opened a row into its item where it stands: carries the item.
    #[component(event(value = ResourceId))]
    pub opened: Event,
    /// The row held open closed: pressed again, Escape, a press beside it, or its item
    /// left the list.
    #[component(event)]
    pub closed: Event,
    /// The person read the list to its end: the last screen of its rows came into view. It fires
    /// once each time they arrive at the end and not while they stay there, so a list that shows
    /// one page of a longer thing asks for the page after it once per reading
    /// ([`VirtualList::set_on_end_reached`]).
    #[component(event)]
    pub end: Event,
    /// A row was dropped in another place: carries the row's id and the id of the row it was
    /// dropped on, in that order. The app moves its own list; the list keeps no order of its own.
    #[component(event(value = Vec<String>))]
    pub reorder: Event,
}

/// The items of a described list: read from a host that renders them as they are shown, or
/// given whole.
#[derive(Default)]
pub struct DescribedRows {
    pub held: Held,
    /// Rows in place of removed items, after the items ([`VirtualListProps::removed`]).
    removed: Rc<Vec<ListItemProps>>,
    /// The heading over the rows whose item names no group
    /// ([`VirtualListProps::ungrouped_title`]).
    ungrouped: Option<String>,
    /// What the list read of the rows it holds, until they change.
    kept: Rc<RefCell<facts::KeptReads>>,
}

/// The rows of a list that stand in for items removed unseen: [`ListItemProps`] marked
/// `removed`, in order.
#[derive(Default)]
pub struct RemovedRows(pub Rc<Vec<ListItemProps>>);

impl RemovedRows {
    fn keys(&self) -> Vec<String> {
        self.0.iter().map(|item| item.key.clone()).collect()
    }
}

impl PartType for RemovedRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(ListItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut items: Vec<ListItemProps> = Vec::new();
        source.components(part, &mut |child| {
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

/// How the item of a row is read: built into its widget, or for what its props alone say.
#[derive(Clone, Copy)]
enum RowRead {
    Built,
    PropsOnly,
}

#[derive(Clone, Default)]
pub enum Held {
    #[default]
    Nothing,
    /// Rows a host renders as they are shown.
    Rendered(SharedRows),
    /// Rows given whole, with their keys.
    Given(Rc<Vec<ListItemProps>>),
}

/// The items of a list: [`ListItemProps`] only.
impl PartType for DescribedRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(ListItemProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        self.kept.borrow_mut().clear();
        if let Some(rows) = source.rows(part) {
            self.held = Held::Rendered(rows);
            return Ok(());
        }
        let mut items: Vec<ListItemProps> = Vec::new();
        source.components(part, &mut |child| {
            let item = ListItemProps::from_source(child).map_err(|error| error.to_string())?;
            match &item.row {
                _ if !item.attached.toolbar.is_empty() => {
                    return Err(
                        "a list given its items whole holds list rows; a row with a toolbar of its own renders as it is shown"
                            .to_owned(),
                    );
                }
                ItemRow::Row(row) if row.trailing.is_some() || row.below.is_some() => {
                    return Err(
                        "a row of a virtual list is built again each time it scrolls into view, so it takes no trailing control and no part under its text"
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
    /// Rows held as `held` says, for a test that builds them from outside the module.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_holding(held: Held) -> Self {
        Self {
            held,
            ..Self::default()
        }
    }

    /// Rows of `items` given whole.
    #[cfg(any(test, feature = "test-support"))]
    pub fn given(items: Vec<ListItemProps>) -> Self {
        Self {
            held: Held::Given(Rc::new(items)),
            ..Self::default()
        }
    }

    /// The same items, to read from elsewhere (what an event of the list reports).
    fn reader(&self) -> Self {
        Self {
            held: self.held.clone(),
            removed: Rc::clone(&self.removed),
            ungrouped: self.ungrouped.clone(),
            kept: Rc::clone(&self.kept),
        }
    }

    /// Whether `key` is a row in place of a removed item.
    fn is_removed(&self, key: &str) -> bool {
        self.removed.iter().any(|item| item.key == key)
    }

    /// Whether the rows the list holds lead with a control — a box or a glyph — so a row the
    /// list builds itself, the struck row of an item removed unseen, keeps their title column.
    /// The first row speaks for them, as a list's rows read alike.
    fn leads_with_a_control(&self) -> bool {
        let Some(first) = self.key_at(0) else {
            return false;
        };
        if self.is_removed(&first) {
            return false;
        }
        self.with_item(&first, |item| match &item.row {
            ItemRow::Row(props) => props.checkbox.is_some() || props.icon.is_some(),
            ItemRow::View(_) | ItemRow::None => false,
        })
        .unwrap_or(false)
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
    /// Ask for the row `key` again with its item's menu.
    fn ask_menu(&self, key: &str) {
        if let Held::Rendered(rows) = &self.held
            && let Ok(mut rows) = rows.try_borrow_mut()
        {
            rows.ask_menu(key);
        }
    }

    fn with_item<R>(&self, key: &str, read: impl FnOnce(ListItemProps) -> R) -> Option<R> {
        self.with(key, RowRead::Built, read)
    }

    /// The same item, for what its props alone say (the keys its commands answer, the menu it
    /// offers): the host renders its props without building its views, where it renders them at
    /// all, which each key the list answers asks of the row it is on.
    fn with_props<R>(&self, key: &str, read: impl FnOnce(ListItemProps) -> R) -> Option<R> {
        self.with(key, RowRead::PropsOnly, read)
    }

    fn with<R>(&self, key: &str, how: RowRead, read: impl FnOnce(ListItemProps) -> R) -> Option<R> {
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
                let Ok(mut rows) = rows.try_borrow_mut() else {
                    return None;
                };
                let shown = match how {
                    RowRead::Built => rows.read(key, read_one),
                    RowRead::PropsOnly => rows.read_props(key, read_one),
                };
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
        value: item.value.clone(),
        item: item.item.clone(),
        opens: item.opens,
        presence: item.presence.clone(),
        author: item.author.clone(),
        time: item.time,
        read_by: item.read_by.clone(),
        menu: item.menu.iter().map(MenuPart::again).collect(),
        offers_menu: item.offers_menu,
        keys: item.keys.clone(),
        shortcut: item.shortcut.clone(),
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
        done: item.done,
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

    fn request_range(&mut self, range: Range<usize>, open: Option<&String>) {
        if let Held::Rendered(rows) = &self.held
            && let Ok(mut rows) = rows.try_borrow_mut()
        {
            rows.request_range(range, open.cloned());
        }
    }

    /// A row the host has named but not rendered is on its way: the list keeps its part, of the
    /// height the rows around it take, and shows a placeholder there until it arrives, rather
    /// than a row of nothing.
    ///
    /// A row in place of an item someone removed is the list's own, given whole beside the
    /// rows the host renders: the host never renders it, and asking it for one would leave the
    /// row on its way forever.
    fn row_state(&self, index: usize) -> tilo_ui_base::arriving::Arrival<(), &str> {
        if let Held::Rendered(rows) = &self.held
            && let Some(key) = self.key_at(index)
            && !self.is_removed(&key)
            && let Ok(rows) = rows.try_borrow()
            && !rows.rendered(&key)
        {
            return tilo_ui_base::arriving::Arrival::local();
        }
        tilo_ui_base::arriving::Arrival::Ready(())
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<String>> {
        match &self.held {
            Held::Rendered(rows) => {
                let Ok(mut rows) = rows.try_borrow_mut() else {
                    return Vec::new();
                };
                let changes = rows.take_changes();
                if !changes.is_empty() {
                    self.kept.borrow_mut().retain(|key| rows.revision(key));
                }
                changes
            }
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
        self.with_facts(&key, |item| item.opens || item.detail)
            .unwrap_or(false)
    }

    /// What the row is grouped by: its item's heading, the list's tail for an item done
    /// with, or the list's heading for the items that name none.
    fn group_at(&self, index: usize) -> Option<super::RowGroup> {
        let key = self.key_at(index)?;
        self.with_facts(&key, |item| {
            if item.change || item.removed {
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

    /// Whether the row's item is one the person is done with — checked off, or removed and not
    /// yet seen go — which a heading over it does not count ([`super::heading_count`]).
    fn done(&self, index: usize) -> bool {
        let Some(key) = self.key_at(index) else {
            return false;
        };
        self.with_facts(&key, |item| item.done || item.removed)
            .unwrap_or(false)
    }

    /// What the row stands as in a tree: the item it is a subtask of, when that item is one of
    /// the list's rows, and the count of its subtasks.
    fn tree_at(&self, index: usize) -> Option<super::TreeRow<String>> {
        let key = self.key_at(index)?;
        self.with_facts(&key, |item| {
            let parent = item
                .parent
                .as_ref()
                .and_then(|parent| DescribedRows::key_presenting(self, &parent.0));
            super::TreeRow {
                parent,
                depth: None,
                above: None,
            }
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
        self.with_facts(&key, |item| item.title.filter(|title| !title.is_empty()))
            .flatten()
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        DescribedRows::key_presenting(self, id)
    }

    fn item_presented(&self, key: &String) -> Option<String> {
        if let Held::Given(items) = &self.held
            && let Some(item) = items.iter().find(|item| item.key == *key)
        {
            return item.item.as_ref().map(|shown| shown.0.clone());
        }
        key.split_once('/').map(|(_, item)| item.to_owned())
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
    /// The item where the person looking has seen nothing yet, as the rows build: the props
    /// say it after the list is built as well as with it, and the rows it moves between build
    /// again, so a row reads it as it builds rather than as the list was made. `None` for a
    /// list whose rows draw no divider.
    unseen: Option<Rc<RefCell<Option<String>>>>,
    /// The row the view last asked the list to show: the list moves when the key it names
    /// changes, and never again for the same key, so a person's own scroll stays.
    revealed: Option<String>,
}

/// The props of a described list that decide the widget, its items aside.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    selection_follows_focus: bool,
    in_column: bool,
    /// The window is too narrow to hold an item under its row.
    narrow: bool,
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
            narrow: props.narrow,
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

    /// Select the row that shows the resource `id`, and only it: the item the next column shows. `None`
    /// selects nothing.
    pub fn select_presenting(&mut self, id: Option<&str>) -> bool {
        if self.chosen_rows() > 1 {
            return false;
        }
        let Some(list) = self.list_mut() else {
            return false;
        };
        let key = id.and_then(|id| list.model().key_presenting(id));
        let now: Vec<String> = list.selection().stored_keys().cloned().collect();
        let want: Vec<String> = key.iter().cloned().collect();
        if now == want {
            return false;
        }
        list.select_only(key);
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
        tilo_ui_base::motion::recent_input(crate::time::now()),
        Some(
            tilo_ui_base::tokens::MotionSource::Pointer
                | tilo_ui_base::tokens::MotionSource::Gesture
        )
    )
}

/// `list`, reporting a row dropped in another place as the `reorder` event: the row's id and
/// the id of the row it was dropped on. The commit hands the move to the app, which keeps its
/// own order, and says it took it.
fn reorderable(list: VirtualList<DescribedRows>, reorder: &Event) -> VirtualList<DescribedRows> {
    if reorder.is_none() {
        return list;
    }
    let moved = reorder.clone();
    list.with_reorder(
        |_rows, _source, _target| true,
        move |_rows, dropped| {
            moved.fire(Some(Value::List(vec![
                Value::Text(dropped.source),
                Value::Text(dropped.target.key),
            ])));
            true
        },
    )
}

/// The resource the item `key` shows, as an event carries it.
fn item_of(rows: &DescribedRows, key: &str) -> Option<Value> {
    rows.with_facts(key, |item| item.item.map(|id| Value::Resource(id.0)))
        .flatten()
}

/// The value the row `key` stands for, when it stands for one.
fn value_of(rows: &DescribedRows, key: &str) -> Option<String> {
    rows.with_facts(key, |item| item.value).flatten()
}

/// Have `list` report its edit and Delete keys with the item of the row they were pressed on,
/// for the app's own edit and destructive actions.
fn wire_keys(
    list: &mut VirtualList<DescribedRows>,
    reading: &Rc<RefCell<DescribedRows>>,
    edit: &Event,
    delete: &Event,
) {
    if !edit.is_none() {
        let (fired, rows) = (edit.clone(), Rc::clone(reading));
        list.set_on_edit(move |key| {
            if let Some(key) = key.get::<String>() {
                fired.fire(item_of(&rows.borrow(), key));
            }
        });
    }
    if !delete.is_none() {
        let (fired, rows) = (delete.clone(), Rc::clone(reading));
        list.set_on_delete(move |key| {
            if let Some(key) = key.get::<String>() {
                fired.fire(item_of(&rows.borrow(), key));
            }
        });
    }
}

/// Have `list` run the command the focused row's own menus and keys declare for a pressed chord,
/// on that row: the item's menu first, then what the view attached to the row, then the keys the
/// app names for it ([`ListItemProps::keys`]). The row answers however it is drawn, a list row
/// and a view of any kind alike, because what it declares is the item's, not its widget's.
fn wire_row_shortcuts(list: &mut VirtualList<DescribedRows>, reading: &Rc<RefCell<DescribedRows>>) {
    let rows = Rc::clone(reading);
    list.set_on_row_shortcut(move |key, pressed| {
        let run = {
            let rows = rows.borrow();
            rows.with_props(key, |item| {
                declared_shortcuts(
                    &item.menu,
                    &item.attached.menu,
                    (&item.keys, &item.shortcut),
                )
                .into_iter()
                .find(|(declared, _)| declared.same_press(pressed))
                .map(|(_, run)| run)
            })
            .flatten()
        };
        if let Some(run) = run {
            run();
            true
        } else {
            false
        }
    });
}

/// What the list reports of the rows `keys` (a selection): the items they show, and the values
/// those that stand for one stand for.
fn chosen_of(rows: &DescribedRows, keys: &[String]) -> (Vec<Value>, Vec<Value>) {
    let items = keys.iter().filter_map(|key| item_of(rows, key)).collect();
    let values = keys
        .iter()
        .filter_map(|key| value_of(rows, key))
        .map(Value::Text)
        .collect();
    (items, values)
}

/// The events of a list's rows that stand for values: one selected, and the values the
/// selection stands for.
#[derive(Clone)]
struct ValueEvents {
    select: Event,
    selected: Event,
}

impl ValueEvents {
    fn selected(&self, rows: &DescribedRows, key: &str) {
        if let Some(value) = value_of(rows, key) {
            self.select.fire(Some(Value::Text(value)));
        }
    }

    /// The row `key` was activated: one that stands for a value runs its own row's `activate`.
    fn activated(&self, rows: &DescribedRows, key: &str) {
        let own = rows
            .with_facts(key, |item| item.value.is_some().then_some(item.activate))
            .flatten();
        if let Some(activate) = own {
            activate.fire(None);
        }
    }
}

/// The rows that stand for `values`, as a selection: the host's choice, given back whole. A row
/// the list has not built yet cannot say what it stands for, so it is chosen once it is built
/// and the host gives its choice again.
fn rows_standing_for(rows: &DescribedRows, values: &[String]) -> KeySelection<String> {
    let mut picked = KeySelection::new();
    if values.is_empty() {
        return picked;
    }
    for index in 0..rows.len() {
        let Some(key) = rows.key_at(index) else {
            continue;
        };
        if value_of(rows, &key).is_some_and(|value| values.contains(&value)) {
            picked.toggle(key);
        }
    }
    picked
}

/// The commands of a row's menus that declare a shortcut (`key: "Ctrl+Enter"`), each with the
/// keys it answers, so the row runs it where it has the keyboard as choosing it does: the
/// item's own menu first, then what the view attached to the row. Text that is no chord names
/// no key, and the first command to name a shortcut holds it.
pub fn declared_shortcuts(
    menu: &[MenuPart],
    attached: &[MenuPart],
    (keys, shortcut): (&[String], &Event),
) -> Vec<(tilo_ui_base::platform::Shortcut, std::rc::Rc<dyn Fn()>)> {
    let mut out: Vec<(tilo_ui_base::platform::Shortcut, std::rc::Rc<dyn Fn()>)> = Vec::new();
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
        let shortcut = tilo_ui_base::platform::Shortcut::from(key);
        if out
            .iter()
            .any(|(declared, _)| declared.same_press(&shortcut))
        {
            continue;
        }
        let press = tilo_ui_base::widget::Press::of(item.choose.clone());
        if press.address().is_none() && !press.is_none() {
            out.push((shortcut, press.runner()));
        }
    }
    // The keys of a row whose menu is not here: the press goes to the item's app, which runs the
    // action the key names.
    for key in keys {
        if tilo_ui_graph::keys::Chord::parse(key).is_err() {
            continue;
        }
        let pressed = tilo_ui_base::platform::Shortcut::from(key.as_str());
        if out
            .iter()
            .any(|(declared, _)| declared.same_press(&pressed))
        {
            continue;
        }
        let (fires, key) = (shortcut.clone(), key.clone());
        out.push((
            pressed,
            Rc::new(move || fires.fire(Some(tilo_ui_base::component::Value::Text(key.clone())))),
        ));
    }
    out
}

/// The row of an item: its list row with the item's presence, or its view; under the swipe
/// and beside the hover toolbar the item has.
fn row_of(
    rows: &DescribedRows,
    key: &str,
    feel: tilo_ui_base::motion::Feel,
    tree: Option<&TreeMarks<String>>,
    build: RowBuild,
) -> Element {
    match bare_row(rows, key, feel, tree, build) {
        Some((row, swipe, attached)) => attach(row, swipe, attached),
        None => Box::new(ListRow::new(key.to_owned(), "")),
    }
}

/// A row of a tree that is a view of the app's own: the view stands in the box a list gives a
/// row's content — one disclosure gutter (`nest`) in for each depth it stands in — so its leading
/// control and its title stand on the column a [`ListRow`]'s do at that depth, and how far its
/// branch got follows it.
fn indent_view(
    view: Element,
    depth: u32,
    nest: f32,
    chevron_column: f32,
    progress: Option<String>,
) -> Element {
    let mut line = Row::row()
        .fill_width()
        .align(Align::Center)
        .gap(Rhythm::None);
    let indent = chevron_column + tilo_ui_base::num::f32_of_f64(f64::from(depth) * f64::from(nest));
    if indent > 0.0 {
        line = line.push(Box::new(Spacer::fixed(indent)));
    }
    let mut part = Part::new(view);
    part.fit = Fit::hug().width(crate::widget::arrange::Sizing::fill(1.0));
    line = line.push_part(part);
    if let Some(progress) = progress {
        line = line
            .push(Box::new(Spacer::token(
                tilo_ui_base::tokens::SpacingStep::S2,
            )))
            .push(Box::new(Text::new(&progress).with_role(TextRole::Caption)));
    }
    Box::new(line)
}

/// The row of an item, with the swipe and what its view attached, to put around it (or around
/// the message it is in).
pub fn bare_row(
    rows: &DescribedRows,
    key: &str,
    feel: tilo_ui_base::motion::Feel,
    tree: Option<&TreeMarks<String>>,
    RowBuild {
        expanded,
        nest,
        chevron_column,
        ..
    }: RowBuild,
) -> Option<(Element, Option<SwipeActionProps>, Attachments)> {
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
            done,
            menu,
            keys,
            shortcut,
            parent_title,
            progress,
            ..
        } = item;
        let breadcrumb =
            parent_title.filter(|_| !tree.is_some_and(|marks| marks.is_nested(&key.to_owned())));
        let shortcuts = declared_shortcuts(&menu, &attached.menu, (&keys, &shortcut));
        // // The mark the item's page takes where the row stands open into it: the row the page // takes
        // the place of is not drawn, and the change the person has not seen goes with the // item, never
        // away with it.
        let page_mark = expanded
            .then(|| change.as_ref().map(ChangeMarkProps::again))
            .flatten();
        let row = match row {
            ItemRow::Row(mut props) => {
                if let Some(title) = &breadcrumb {
                    props.secondary = Some(match props.secondary.take() {
                        Some(own) if !own.is_empty() => format!("in {title} · {own}"),
                        _ => format!("in {title}"),
                    });
                }
                // // The list paces the rows it builds unless a row says its own, so one option on // the list
                // covers its content.
                props.motion = props.motion.or(Some(feel));
                if let (Some(parent), Some(meta)) = (&parent_meta, &props.meta)
                    && parent == meta
                {
                    props.meta = None;
                    props.meta_tone = tilo_ui_base::widget::Tone::Neutral;
                }
                props.activate = Event::None;
                // // The row's own figure stands unless the tree's count takes that column.
                let value_free = props.value.is_none();
                let mut row = ListRow::from_props(key, *props);
                if let Some(person) = presence {
                    row = row.presence(&person);
                }
                if removed || done {
                    row = row.struck();
                }
                if removed && rows.leads_with_a_control() {
                    // The row stands in for one that had a leading control; it keeps the title
                    // column of the rows around it with that column left empty.
                    row = row.leading_empty(true);
                }
                if let Some(mark) = change.and_then(ChangeMarkProps::row) {
                    row = row.change(mark.by, mark.spoken, mark.said, mark.show);
                }
                row = row.shortcuts(shortcuts);
                if let Some(marks) = tree {
                    row = row
                        .depth(marks.depth(&key.to_owned()))
                        .chevron_column(f64::from(chevron_column));
                    if value_free && let Some(progress) = progress.clone() {
                        row = row.value(progress);
                    }
                }
                Box::new(row) as Element
            }
            ItemRow::View(mut view) => {
                if done {
                    strike_row(view.as_mut());
                }
                let view = with_mark(
                    view,
                    change.and_then(|change| change.face(&format!("{key}~change"))),
                    Align::Center,
                );
                match tree {
                    Some(marks) => indent_view(
                        view,
                        marks.depth(&key.to_owned()),
                        nest,
                        chevron_column,
                        progress.clone(),
                    ),
                    None => view,
                }
            }
            ItemRow::None => Box::new(ListRow::new(key.to_owned(), "")),
        };
        let row = match (expanded, detail) {
            (true, ItemRow::View(document)) => {
                let document = with_mark(
                    document,
                    page_mark.and_then(|change| change.face(&format!("{key}~change"))),
                    Align::Start,
                );
                let document = match tree {
                    Some(marks) => indent_view(
                        document,
                        marks.depth(&key.to_owned()),
                        nest,
                        chevron_column,
                        None,
                    ),
                    None => document,
                };
                Box::new(super::open::RowOpen::new(document, tree.is_some())) as Element
            }
            (true, ItemRow::Row(props)) => {
                let mut document = ListRow::from_props(key, *props);
                if let Some(mark) = page_mark.and_then(ChangeMarkProps::row) {
                    document = document.change(mark.by, mark.spoken, mark.said, mark.show);
                }
                if let Some(marks) = tree {
                    document = document.depth(marks.depth(&key.to_owned()));
                }
                Box::new(document) as Element
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
    let parent = rows.with_facts(key, |item| item.parent).flatten()?;
    rows.key_presenting(&parent.0)
}

/// `row` with a change mark after it, on its line, when it has one: beside the row's own line,
/// or at `align` down it where what the mark stands beside is an item's page.
/// Strike the title of the first list row in an item's view: an item done with reads as done
/// whether its view is a bare row or a row with more under it (its reactions, its replies).
pub fn strike_row(view: &mut dyn Widget) -> bool {
    if let Some(row) = view
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<ListRow>())
    {
        row.set_struck();
        return true;
    }
    view.children_mut().into_iter().any(strike_row)
}

fn with_mark(row: Element, mark: Option<crate::widget::Chip>, align: Align) -> Element {
    match mark {
        Some(mark) => {
            let mut row = Part::new(row);
            row.fit =
                crate::widget::arrange::Fit::hug().width(crate::widget::arrange::Sizing::fill(1.0));
            Box::new(
                Row::row()
                    .fill_width()
                    .align(align)
                    .push_part(row)
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

/// A message of a transcript: its item's row, under its author and the time when the item names
/// them. One author's messages, each within [`GROUPING_WINDOW_SECS`] of the one before it on the
/// same day, show the author's avatar and name once, over the first of them; the others line up
/// under it.
fn message_of(
    rows: &DescribedRows,
    key: &str,
    unread: Option<&str>,
    feel: tilo_ui_base::motion::Feel,
    measure: Option<Length>,
) -> Element {
    let first_unread = unread.is_some_and(|item| {
        rows.with_facts(key, |props| {
            props
                .item
                .as_ref()
                .is_some_and(|shown| shown.0.as_str() == item)
        })
        .unwrap_or(false)
    });
    let said = rows.with_facts(key, |item| (item.author, item.time));
    let Some((Some(author), at)) = said else {
        let row = row_of(rows, key, feel, None, RowBuild::default());
        return page_row(under_unread(row, first_unread), RUN_GAP, measure);
    };
    let before = rows
        .index_of(&key.to_owned())
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| rows.key_at(index))
        .and_then(|key| rows.with_facts(&key, |item| (item.author, item.time)));
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
                        (0..=GROUPING_WINDOW_SECS).contains(&at.0.saturating_sub(then.0))
                    }
                    _ => true,
                }
        });
    let Some((said, swipe, attached)) = bare_row(rows, key, feel, None, RowBuild::default()) else {
        let row = row_of(rows, key, feel, None, RowBuild::default());
        return page_row(under_unread(row, first_unread), RUN_GAP, measure);
    };
    // The author's face as every surface draws a member: an agent's mark or a person's disc,
    // in the hue the workspace wears them in, showing the picture they set where they set one.
    let avatar = Avatar::of(author.clone());
    let message = Row::row().fill_width().align(Align::Start);
    let message = if continues {
        message.push(Box::new(avatar.placeholder()))
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
    let read_by: Vec<Person> = rows
        .with_facts(key, |item| item.read_by)
        .unwrap_or_default()
        .into_iter()
        .filter(|who| who.identity != author.identity)
        .collect();
    let message = message.push_fit(said, Fit::fill(1.0));
    let message = if read_by.is_empty() {
        message
    } else {
        message.push(read_faces(key, read_by))
    };
    let message = attach(Box::new(message), swipe, attached);
    let gap = if continues { Rhythm::Tight } else { RUN_GAP };
    // // The band over this line: the day it opens, the place reading starts again, or — where the //
    // day turns over at the reader's first unseen line — one band in the accent saying both, // never
    // a day's divider stacked over the unread one.
    let divider = match (day.filter(|_| new_day), first_unread) {
        (Some(day), true) => Some(SectionDivider::unread_day(&clock.day_label(day))),
        (Some(day), false) => Some(SectionDivider::new(clock.day_label(day))),
        (None, true) => Some(SectionDivider::unread()),
        (None, false) => None,
    };
    let shown = match divider {
        Some(divider) => Box::new(
            Stack::stack()
                .gap(RUN_GAP)
                .push(Box::new(divider))
                .push(message),
        ) as Element,
        None => message,
    };
    page_row(shown, gap, measure)
}

/// The faces of those who have read to the row `key`, small, at the trailing end of its line:
/// each of them stands on the one line their reading ends at, and nowhere else.
fn read_faces(key: &str, read: Vec<Person>) -> Element {
    let mut faces = Row::row().gap(Rhythm::Tight).align(Align::Center);
    for who in &read {
        faces = faces.push(Box::new(
            Avatar::person(who.name.clone(), who.identity.id()).size(AvatarSize::Small),
        ));
    }
    let names = read
        .iter()
        .map(|who| who.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    Box::new(
        crate::widget::Container::new()
            .with_no_padding()
            .landmark(
                tilo_ui_base::i18n::widget_format(
                    "conversation-read-by",
                    &[("names", names.into())],
                ),
                format!("{key}~read"),
            )
            .with_child(Box::new(faces)),
    )
}

/// `content` under the "New messages" divider, for the row of the first message the reader had
/// not read: the divider is the toolkit's ([`crate::widget::section_divider::SectionDivider`], in
/// the accent where reading starts again), placed from what the app says is unread, so every
/// conversation draws the same one.
///
/// It stands in the row's own column, one gap above what the row shows, so the row keeps the
/// column it has without it: the divider adds its band and nothing else.
fn under_unread(content: Element, first_unread: bool) -> Element {
    if !first_unread {
        return content;
    }
    Box::new(
        Stack::stack()
            .gap(RUN_GAP)
            .push(Box::new(SectionDivider::unread()))
            .push(content),
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
    Box::new(page.push_part(Part::new(content)))
}

/// Whether the row of `key` offers a menu: what its item offers on one, or the view put on it.
fn offers_menu(rows: &DescribedRows, key: &str) -> bool {
    rows.offers_menu(key)
}

/// The list of rows a menu over `list` opens on, and the key of the row under `at` (its
/// focused row when the keyboard opened it).
fn menu_row_key(
    list: &dyn Widget,
    at: Option<Point>,
) -> Option<(&VirtualList<DescribedRows>, String)> {
    let list = list
        .as_any()
        .and_then(|list| list.downcast_ref::<VirtualList<DescribedRows>>())?;
    let key = match at {
        Some(point) => list.key_at_point(point),
        None => list
            .focused_row_key()
            .and_then(|key| key.get::<String>().cloned()),
    }?;
    Some((list, key))
}

/// Whether the menu of the row under `at` is on its way: its item offers one the row does not
/// carry, which it was asked for ([`row_menu`]).
fn row_menu_waits(list: &dyn Widget, at: Option<Point>) -> bool {
    menu_row_key(list, at).is_some_and(|(list, key)| {
        list.model()
            .with_item(&key, |item| item.offers_menu)
            .unwrap_or(false)
    })
}

/// The menu of the row under `at` (its focused row when the keyboard opened it): what its
/// item offers. Opening it selects the row, unless the row opens where it stands: that row
/// shows its item under itself, and a menu over it is not the window opening it elsewhere.
///
/// A row whose item offers a menu it does not carry is asked for again with it, and the menu
/// waits for it ([`row_menu_waits`]).
fn row_menu(list: &dyn Widget, at: Option<Point>, select: &Event) -> Vec<MenuEntry> {
    let Some((list, key)) = menu_row_key(list, at) else {
        return Vec::new();
    };
    let rows = list.model();
    if rows
        .with_item(&key, |item| item.offers_menu)
        .unwrap_or(false)
    {
        rows.ask_menu(&key);
        return Vec::new();
    }
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
    let opens_here = list
        .model()
        .index_of(&key)
        .is_some_and(|index| list.opens_here(index));
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
            narrow,
            page_column,
            at_measure,
            tail_title,
            tree,
            ungrouped_title,
            view_title,
            view_count,
            most,
            multi_select,
            unseen_from,
            mut rows,
            removed,
            selection,
            chosen,
            scroll,
            open,
            reveal,
            motion,
            select,
            activate,
            edit,
            delete,
            selected: selected_changed,
            chosen_values,
            select_value,
            selected_values,
            opened: opened_changed,
            closed: closed_changed,
            revealed: revealed_changed,
            end: end_reached,
            reorder,
        } = props;
        rows.removed = removed.0;
        rows.ungrouped = ungrouped_title;
        let feel = motion.unwrap_or(tilo_ui_base::motion::Feel::Standard);
        let reading = Rc::new(RefCell::new(rows.reader()));
        if page_column {
            let unseen = Rc::new(RefCell::new(unseen_from.map(|item| item.0)));
            let marked = unseen.borrow().clone();
            let divider = Rc::clone(&unseen);
            let measure = at_measure.then_some(Length::Rem(MEASURE_REM));
            let mut list = VirtualList::new(rows, MESSAGE_ESTIMATE, move |rows, _, key| {
                let unread = divider.borrow().clone();
                message_of(rows, key, unread.as_deref(), feel, measure)
            })
            .flush()
            .with_edges(TranscriptEdges::new(RUN_GAP))
            .filling()
            .with_id(key)
            .with_feel(feel)
            .reading();
            if follow_end {
                list = list.following_end();
            }
            let list = list
                .unread_from(marked)
                .with_menu_around(|rows, key| offers_menu(rows, key));
            let mut list = list;
            if let Some(key) = &reveal {
                list = list.revealing(key.clone());
            }
            if !revealed_changed.is_none() {
                let told = revealed_changed.clone();
                let reading = Rc::clone(&reading);
                list.set_on_revealed(move |key| told.fire(item_of(&reading.borrow(), &key)));
            }
            list.set_on_end_reached(end_reached.handler());
            wire_keys(&mut list, &reading, &edit, &delete);
            wire_row_shortcuts(&mut list, &reading);
            let list = reorderable(list, &reorder);
            let area = ContextMenuArea::over_content(Box::new(list), move |list, at| {
                row_menu(list, at, &Event::None)
            })
            .waiting_on(row_menu_waits);
            return Self {
                body: ListBody::Menus(Box::new(area)),
                face,
                reading,
                unseen: Some(unseen),
                revealed: reveal,
            };
        }
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
        if let Some(title) = view_title {
            // // What the title counts, as the rows a heading counts: a number never negative.
            let count = view_count.map(|count| usize::try_from(count.max(0)).unwrap_or(usize::MAX));
            list = list.in_view(title, count);
        }
        list = if in_column {
            list.filling().in_column()
        } else {
            list.fitting_rows()
        };
        if let Some(most) = most {
            list.set_most_height(Some(most));
        }
        if follow_end {
            list = list.following_end();
        }
        list = list.in_place(!narrow);
        if selection_follows_focus {
            list = list.selection_follows_focus();
        }
        if multi_select {
            list = list.multi_select(true);
        }
        list = list
            .with_id(key)
            .with_semantic_key(Clone::clone)
            .unread_from(unseen_from.map(|item| item.0));
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
        // The rows the host chose, by the items they present: the model answers for them, and
        // holds the ones it cannot answer for yet until their rows arrive.
        let chosen_items: Option<Vec<String>> =
            chosen.map(|ids| ids.into_iter().map(|id| id.0).collect());
        list.set_chosen(chosen_items.as_deref());
        if let Some(values) = &chosen_values {
            list = list.with_selection(rows_standing_for(&reading.borrow(), values));
        }
        list = reorderable(list, &reorder);
        let values = ValueEvents {
            select: select_value,
            selected: selected_values,
        };
        // A row that stands for a value runs its own `activate`, whatever the list's events, so
        // the callbacks are always there.
        {
            let (on_select, on_pick) = (select.clone(), activate.clone());
            let (selecting, picking) = (Rc::clone(&reading), Rc::clone(&reading));
            let (valued, valuing) = (values.clone(), values.clone());
            list = list
                .on_select(move |key, selected| {
                    if !selected {
                        return;
                    }
                    let rows = selecting.borrow();
                    let item = item_of(&rows, &key);
                    if selection_follows_focus && by_hand() {
                        on_pick.fire(item);
                        valued.activated(&rows, &key);
                    } else {
                        on_select.fire(item);
                        valued.selected(&rows, &key);
                    }
                })
                .on_activate(move |key| {
                    let rows = picking.borrow();
                    if !rows.is_removed(&key) {
                        activate.fire(item_of(&rows, &key));
                        valuing.activated(&rows, &key);
                    }
                });
        }
        wire_keys(&mut list, &reading, &edit, &delete);
        wire_row_shortcuts(&mut list, &reading);
        if !selected_changed.is_none() || !values.selected.is_none() {
            let fired = selected_changed.clone();
            let valued = values.selected.clone();
            let selecting = Rc::clone(&reading);
            list = list.on_selected(move |keys| {
                let (items, values) = chosen_of(&selecting.borrow(), keys);
                fired.fire(Some(Value::List(items)));
                valued.fire(Some(Value::List(values)));
            });
        }
        // // The item the host holds open stands open where its row stands, unless the item is no // row
        // of the list (a place that does not hold it).
        list.hold_open(open.as_ref().map(|item| item.0.as_str()));
        if let Some(key) = &reveal {
            list = list.revealing(key.clone());
        }
        if !revealed_changed.is_none() {
            let told = revealed_changed.clone();
            let telling = Rc::clone(&reading);
            list.set_on_revealed(move |key| told.fire(item_of(&telling.borrow(), &key)));
        }
        if !opened_changed.is_none() || !closed_changed.is_none() {
            let (opened, closed) = (opened_changed, closed_changed);
            let opening = Rc::clone(&reading);
            list = list
                .on_opened(move |key| {
                    opened.fire(item_of(&opening.borrow(), &key));
                })
                .on_closed(move || closed.fire(None));
        }
        list.set_on_end_reached(end_reached.handler());
        if let Some(offset) = scroll {
            list.set_scroll_offset(offset);
        }
        let list = list.with_menu_around(|rows, key| offers_menu(rows, key));
        let area = ContextMenuArea::over_content(Box::new(list), move |list, at| {
            row_menu(list, at, &select)
        })
        .waiting_on(row_menu_waits);
        Self {
            body: ListBody::Menus(Box::new(area)),
            face,
            reading,
            unseen: None,
            revealed: reveal,
        }
    }

    /// The same list over the same rows: what the props say around the items is taken where the list
    /// stands, and items given whole are replaced where they stand. The person's scroll, the row their
    /// keyboard holds and what they opened stay.
    fn set_props(&mut self, props: VirtualListProps) -> bool {
        if Face::of(&props) != self.face {
            return false;
        }
        let VirtualListProps {
            selection_follows_focus,
            tail_title,
            ungrouped_title,
            view_title,
            view_count,
            most,
            multi_select,
            unseen_from,
            mut rows,
            removed,
            chosen,
            open,
            motion,
            select,
            activate,
            edit,
            delete,
            selected: selected_changed,
            chosen_values,
            select_value,
            selected_values,
            opened: opened_changed,
            closed: closed_changed,
            reveal,
            revealed: revealed_changed,
            end: end_reached,
            ..
        } = props;
        let feel = motion.unwrap_or(tilo_ui_base::motion::Feel::Standard);
        rows.removed = removed.0;
        rows.ungrouped = ungrouped_title;
        let replaced = rendered(&rows).is_none();
        let unseen_from = unseen_from.as_ref().map(|item| item.0.clone());
        if let Some(unseen) = &self.unseen {
            (*unseen.borrow_mut()).clone_from(&unseen_from);
        }
        // The rows the host chose, by the items they present: the model answers for them, and
        // holds the ones it cannot answer for yet until their rows arrive.
        let chosen_items: Option<Vec<String>> =
            chosen.map(|ids| ids.into_iter().map(|id| id.0).collect());
        let reading = rows.reader();
        *self.reading.borrow_mut() = reading;
        // The row the view asks to show moves the list when the key it names changes, and never
        // for the same key again, so a person's own scroll stays. It is taken before the list is
        // borrowed, since the ask is the component's.
        // The row the view asks to show moves the list when the key it names changes, and also
        // when that row is no longer in the window (a person scrolled away and pressed the same
        // letter again); a row already in view stays where they scrolled it.
        let outside = |key: &str| self.list().is_some_and(|list| !list.row_is_visible(key));
        let asking = match &reveal {
            Some(key) if reveal != self.revealed || outside(key) => Some(key.clone()),
            _ => None,
        };
        self.revealed.clone_from(&reveal);
        let reading = Rc::clone(&self.reading);
        let Some(list) = self.list_mut() else {
            return false;
        };
        list.set_multi_select(multi_select);
        list.set_tail_title(tail_title);
        let count = view_count.map(|count| usize::try_from(count.max(0)).unwrap_or(usize::MAX));
        list.set_view_title(view_title, count);
        list.set_most_height(most);
        list.set_feel(feel);
        if replaced {
            list.update_model(move |model| *model = rows);
        }
        list.set_unread(unseen_from);
        if let Some(key) = &asking {
            list.scroll_reveal(key.as_str());
        }
        if !revealed_changed.is_none() {
            let told = revealed_changed.clone();
            let telling = Rc::clone(&reading);
            list.set_on_revealed(move |key| told.fire(item_of(&telling.borrow(), &key)));
        }
        // // The item the host holds open: its row grows into its document where it stands, and // an item
        // that left the list opens nothing. An item the rows cannot answer for yet // leaves the row the
        // person has open as it is, and opens as its row arrives.
        list.hold_open(open.as_ref().map(|item| item.0.as_str()));
        list.set_chosen(chosen_items.as_deref());
        if let Some(values) = &chosen_values {
            list.set_selection(rows_standing_for(&reading.borrow(), values));
        }
        let values = ValueEvents {
            select: select_value,
            selected: selected_values,
        };
        let selecting = Rc::clone(&reading);
        let picking = Rc::clone(&reading);
        let (on_select, on_pick) = (select, activate.clone());
        let (valued, valuing) = (values.clone(), values.clone());
        list.set_on_select(move |key, selected| {
            if !selected {
                return;
            }
            let rows = selecting.borrow();
            let item = item_of(&rows, &key);
            if selection_follows_focus && by_hand() {
                on_pick.fire(item);
                valued.activated(&rows, &key);
            } else {
                on_select.fire(item);
                valued.selected(&rows, &key);
            }
        });
        list.set_on_activate(move |key| {
            let rows = picking.borrow();
            if !rows.is_removed(&key) {
                activate.fire(item_of(&rows, &key));
                valuing.activated(&rows, &key);
            }
        });
        wire_keys(list, &reading, &edit, &delete);
        let fired = selected_changed;
        let opening = Rc::clone(&reading);
        let valued = values.selected;
        list.set_on_selected(move |keys| {
            let (items, values) = chosen_of(&reading.borrow(), keys);
            fired.fire(Some(Value::List(items)));
            valued.fire(Some(Value::List(values)));
        });
        if !opened_changed.is_none() {
            list.set_on_opened(move |key| {
                opened_changed.fire(item_of(&opening.borrow(), &key));
            });
        }
        if !closed_changed.is_none() {
            list.set_on_closed(move || closed_changed.fire(None));
        }
        list.set_on_end_reached(end_reached.handler());
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

    /// The list's node is its body's, which it does not rewrite: read without its rows.
    fn accessibility_root(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.inner().accessibility_root()
    }

    fn accessibility_hidden(&self) -> bool {
        self.inner().accessibility_hidden()
    }

    fn name(&self) -> &'static str {
        "DescribedList"
    }

    /// The column its rows are read in: a list whose rows keep the readable measure says so, so
    /// a bar at the foot of the column stands in it.
    fn readable_column(&self) -> Option<Length> {
        let Face {
            page_column,
            at_measure,
            ..
        } = self.face;
        (page_column && at_measure).then_some(Length::Rem(MEASURE_REM))
    }

    /// The list's change is the described list's: it adds no structure of its own, so rows
    /// the list builds anew stay mounted in place and the window keeps its tree.
    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        self.inner_mut().take_structure_change()
    }
}
