//! A `list` frame's items: a [`VirtualList`] of [`ListRow`]s, or the empty state when there are
//! none.
//!
//! The list is one focus stop. Selection follows focus and shows the item in the next column;
//! activating a row brings that column to the front of a narrow window. Every action on an
//! item is on its row's context menu, the first destructive one (else the first) on its swipe.
//!
//! The rows come from a [`ListModel`], [`Rows`]: the keys of every row of the view's list, and
//! the rows the list shows, which the view renders when the list asks for them
//! (`request_range`: the visible rows and the overscan) and forgets when they scroll away. A
//! list of ten thousand notes costs the rows on screen. The view's changes reach the list as
//! they are: a row inserted, removed or moved is that change of the model, and a row the view
//! changed is read again and rebuilt alone.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Patch, ResourceRef, UiNode, Value, ViewInstance};
use ui::icons::Icon;
use ui::vello::kurbo::Point;
use ui::widget::virtual_list::ListModelChange;
use ui::widget::{
    AppModel, ContextMenuArea, Decorator, Element, EmptyState, ListModel, ListRow, MenuEntry,
    ModelPath, StructureChange, VirtualList, Widget,
};

use super::{Request, Requests};
use crate::names::Names;
use crate::reconcile::{EventSink, UiEvent};

/// An action a row offers on its item.
#[derive(Clone, Debug, PartialEq)]
pub struct RowAction {
    pub label: String,
    pub icon: Option<Icon>,
    /// The published resource it is invoked on and its id there.
    pub resource: String,
    pub action: String,
    /// The action declared `primary`: the header's Primary button and the empty state's action.
    pub primary: bool,
    pub destructive: bool,
    /// The arguments it is invoked with: the item, for an action of the list's owner that takes
    /// one (`delete(nb: Notebook, n: Note)`).
    pub args: Vec<(String, String)>,
    /// Whether the state the action toggles is on for the subject (a pinned note), when the
    /// action says (`on:`): its icon is drawn filled.
    pub on: Option<bool>,
    /// The id of the subject's field whose state the action toggles (`on: n.pinned`): a control
    /// bound to it in the window stands for the action.
    pub toggles: Option<String>,
}

/// One row of a list: the item, and the `row(...)` the view made for it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowData {
    /// The view's key of the row: the list's row of this item, the same however often it is
    /// shown.
    pub key: String,
    pub item: ResourceRef,
    /// The title when the view made no row for the item.
    pub label: String,
    /// The `list_row` node, when the view made one.
    pub row: Option<UiNode>,
    /// What the row's checkbox shows, when it is bound to a field.
    pub checked: Option<bool>,
    /// The row's avatar, when its view gives it one.
    pub avatar: Option<RowAvatar>,
    pub actions: Rc<[RowAction]>,
    /// Who else is at the item now: a collaborator or an agent whose window shows it.
    pub presence: Option<String>,
}

/// The participants other than this window's at each resource of the package (by its id), as
/// presence last reported them.
pub type Peers = Rc<RefCell<HashMap<String, String>>>;

/// Where the rows of a list come from: the view whose `list` it is, which renders a row when
/// the list shows it, and what a row shows of the room.
pub struct RowSource {
    pub view: Rc<RefCell<ViewInstance>>,
    /// The key of the `list` node.
    pub list: String,
    pub host: Host,
    /// The resource the view shows: the owner of the list's items, whose actions on an item
    /// its row offers too.
    pub owner: ResourceRef,
    pub peers: Peers,
    pub names: Names,
}

/// How the rows of a list are read from the view that renders them: which view and list, and
/// what a row the view rendered is kept as.
pub trait RowRead {
    type Data: Clone + PartialEq;

    /// The view whose list it is.
    fn view(&self) -> &Rc<RefCell<ViewInstance>>;

    /// The key of the `list` node.
    fn list(&self) -> &str;

    /// What `row` (a row of the list, as the view rendered it) shows.
    fn data(&self, view: &ViewInstance, row: &UiNode) -> Option<Self::Data>;

    /// What a row is called, for the list's type-ahead and accessibility.
    fn label(data: &Self::Data) -> Option<String>;

    /// The row's avatar, when its view gives it one.
    fn avatar(data: &Self::Data) -> Option<&RowAvatar> {
        let _ = data;
        None
    }
}

impl RowRead for RowSource {
    type Data = RowData;

    fn view(&self) -> &Rc<RefCell<ViewInstance>> {
        &self.view
    }

    fn list(&self) -> &str {
        &self.list
    }

    fn avatar(data: &RowData) -> Option<&RowAvatar> {
        data.avatar.as_ref()
    }

    fn label(data: &RowData) -> Option<String> {
        Some(
            data.row
                .as_ref()
                .and_then(|node| text(node, "title"))
                .unwrap_or_else(|| data.label.clone()),
        )
    }

    fn data(&self, view: &ViewInstance, row: &UiNode) -> Option<RowData> {
        let item = find_kind(row, "item")?;
        let Some(Value::Ref(reference)) = item.props.get("item") else {
            return None;
        };
        let target = ResourceRef::clone(&reference.target);
        let list_row = find_kind(item, "list_row").cloned();
        let checked = list_row
            .as_ref()
            .filter(|node| node.binding.is_some())
            .and_then(|node| match view.bound_value(&node.key) {
                Some(Value::Bool(flag)) => Some(flag),
                Some(Value::Int(number)) => Some(number != 0),
                _ => None,
            });
        let avatar = list_row
            .as_ref()
            .and_then(|node| match node.props.get("avatar") {
                Some(Value::Principal(who)) => {
                    Some(RowAvatar::Of(self.names.label(&who.0), who.0.clone()))
                }
                // No participant this time: the row's own icon stands in for them as the item's
                // avatar (a conversation's group), else the row keeps the room an avatar takes.
                Some(Value::Opt(None)) => {
                    Some(icon(node, "icon").map_or(RowAvatar::Space, |glyph| {
                        let name = text(node, "title")
                            .unwrap_or_else(|| super::describe::title_of(&self.host, &target));
                        RowAvatar::Glyph(name, glyph)
                    }))
                }
                _ => None,
            });
        Some(RowData {
            key: row.key.clone(),
            label: super::describe::title_of(&self.host, &target),
            actions: super::describe::offered_in(&self.host, &target, &self.owner),
            presence: self.peers.borrow().get(&target.id_string()).cloned(),
            item: target,
            row: list_row,
            checked,
            avatar,
        })
    }
}

/// The avatar of a row whose view gives it one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowAvatar {
    /// A participant: what they are called, and their identity.
    Of(String, String),
    /// No participant this time (`.none`), and the row names an icon: the item itself, as a
    /// neutral avatar with that glyph (a group conversation's), called by the item's title.
    Glyph(String, Icon),
    /// None this time (`.none`) and no icon: the row keeps the room one takes when other rows
    /// of its list show an avatar, so its text lines up with theirs, and takes none otherwise.
    Space,
}

impl RowAvatar {
    /// Whether the row shows an avatar rather than the room for one.
    #[must_use]
    pub fn shown(&self) -> bool {
        !matches!(self, Self::Space)
    }
}

/// The first node of `kind` at or under `node`.
fn find_kind<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_kind(child, kind))
}

/// The rows of a list: every row's key, in order, as the view's list holds them, and the rows
/// the list shows, rendered by the view when the list asks for them and read as `S` says.
pub struct Rows<S: RowRead = RowSource> {
    source: Option<Rc<S>>,
    keys: Vec<Rc<str>>,
    /// Where each key was last found, built when first asked. A row inserted, removed or moved
    /// moves each other row by at most one place, so a key is looked for around where it was
    /// and the place found is kept: a change costs the rows it touches, not the list's length.
    index: RefCell<Option<HashMap<Rc<str>, usize>>>,
    /// The rows shown, by key.
    shown: RefCell<HashMap<String, S::Data>>,
    /// Whether a row the list showed has an avatar: the rows without one then keep its room.
    /// Once on it stays on, so rows do not shift as others scroll in and out.
    avatars: Cell<bool>,
    /// The shown rows to build again because the avatar column came in after they were built.
    restyled: RefCell<Vec<String>>,
}

impl<S: RowRead> Default for Rows<S> {
    fn default() -> Self {
        Self {
            source: None,
            keys: Vec::new(),
            index: RefCell::new(None),
            shown: RefCell::default(),
            avatars: Cell::new(false),
            restyled: RefCell::default(),
        }
    }
}

impl<S: RowRead> Rows<S> {
    /// The rows of `source`'s list, none of them shown yet.
    #[must_use]
    pub fn of(source: Rc<S>) -> Self {
        let keys = {
            let view = source.view().borrow();
            let len = view.list_len(source.list()).unwrap_or(0);
            (0..len)
                .filter_map(|index| view.row_key(source.list(), index))
                .map(Rc::from)
                .collect()
        };
        Self {
            source: Some(source),
            keys,
            ..Self::default()
        }
    }

    /// Whether the rows without an avatar keep the room one takes: some row the list showed
    /// has one. A list where none has one reserves nothing.
    #[must_use]
    pub fn shows_avatars(&self) -> bool {
        self.avatars.get()
    }

    /// Turn the avatar column on when a row of `shown` has an avatar, and queue the rows
    /// already built without its room to be built again.
    fn note_avatars(&self, shown: &HashMap<String, S::Data>) {
        if self.avatars.get()
            || !shown
                .values()
                .any(|data| S::avatar(data).is_some_and(RowAvatar::shown))
        {
            return;
        }
        self.avatars.set(true);
        self.restyled.borrow_mut().extend(
            shown
                .iter()
                .filter(|(_, data)| S::avatar(data).is_some_and(|avatar| !avatar.shown()))
                .map(|(key, _)| key.clone()),
        );
    }

    /// Whether these are the rows of `source`'s list.
    pub fn reads(&self, source: &S) -> bool {
        self.source
            .as_ref()
            .is_some_and(|own| Rc::ptr_eq(own.view(), source.view()) && own.list() == source.list())
    }

    /// The row of `key`, when the list shows it.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<S::Data> {
        self.shown.borrow().get(key).cloned()
    }

    /// The rows shown, in the list's order.
    #[cfg(test)]
    #[must_use]
    pub fn shown(&self) -> Vec<S::Data> {
        let shown = self.shown.borrow();
        self.keys
            .iter()
            .filter_map(|key| shown.get(&**key).cloned())
            .collect()
    }

    /// Apply the view's changes of the list's own children, in order: rows inserted, removed
    /// and moved. Returns them as the list widget takes them, each against the order the ones
    /// before it left.
    pub fn apply(&mut self, patches: &[Patch]) -> Vec<ListModelChange<String>> {
        let mut out = Vec::new();
        for patch in patches {
            let change = match patch {
                Patch::Insert { index, node, .. } => {
                    let index = (*index).min(self.keys.len());
                    let key: Rc<str> = Rc::from(node.key.as_str());
                    self.keys.insert(index, Rc::clone(&key));
                    self.found(key, index);
                    ListModelChange::RowsInserted { index, count: 1 }
                }
                Patch::Remove { key, .. } => {
                    let Some(index) = self.index_of(key) else {
                        continue;
                    };
                    self.keys.remove(index);
                    if let Some(index) = self.index.get_mut().as_mut() {
                        index.remove(key.as_str());
                    }
                    self.shown.get_mut().remove(key);
                    ListModelChange::RowsRemoved { index, count: 1 }
                }
                Patch::Move { key, to, .. } => {
                    let Some(from) = self.index_of(key) else {
                        continue;
                    };
                    let moved = self.keys.remove(from);
                    let to = (*to).min(self.keys.len());
                    self.keys.insert(to, Rc::clone(&moved));
                    self.found(moved, to);
                    ListModelChange::RowsMoved { from, to, count: 1 }
                }
                _ => continue,
            };
            out.push(change);
        }
        out
    }

    /// The changes `patches` would make, as [`Self::apply`] returns them, without making
    /// them: what the list widget is told before the rows change. One change is read against
    /// the rows as they are; several against a copy.
    #[must_use]
    pub fn changes_of(&self, patches: &[Patch]) -> Vec<ListModelChange<String>> {
        let one = match patches {
            [Patch::Insert { index, .. }] => ListModelChange::RowsInserted {
                index: (*index).min(self.keys.len()),
                count: 1,
            },
            [Patch::Remove { key, .. }] => match self.index_of(key) {
                Some(index) => ListModelChange::RowsRemoved { index, count: 1 },
                None => return Vec::new(),
            },
            [Patch::Move { key, to, .. }] => match self.index_of(key) {
                Some(from) => ListModelChange::RowsMoved {
                    from,
                    to: (*to).min(self.keys.len().saturating_sub(1)),
                    count: 1,
                },
                None => return Vec::new(),
            },
            [_] | [] => return Vec::new(),
            _ => {
                let mut copy: Rows<S> = Rows {
                    keys: self.keys.clone(),
                    ..Rows::default()
                };
                return copy.apply(patches);
            }
        };
        vec![one]
    }

    /// Keep that `key` is at `index` now.
    fn found(&mut self, key: Rc<str>, index: usize) {
        if let Some(found) = self.index.get_mut().as_mut() {
            found.insert(key, index);
        }
    }

    /// Read the shown rows of `keys` again from the view. Returns the keys whose row shows
    /// something else now.
    pub fn refresh(&self, keys: impl IntoIterator<Item = String>) -> Vec<String> {
        let Some(source) = &self.source else {
            return Vec::new();
        };
        let Ok(view) = source.view().try_borrow() else {
            return Vec::new();
        };
        let mut shown = self.shown.borrow_mut();
        let mut changed: Vec<String> = keys
            .into_iter()
            .filter(|key| {
                let Some(old) = shown.get(key) else {
                    return false;
                };
                let Some(new) = view.find(key).and_then(|row| source.data(&view, row)) else {
                    return false;
                };
                let changed = *old != new;
                if changed {
                    shown.insert(key.clone(), new);
                }
                changed
            })
            .collect();
        self.note_avatars(&shown);
        changed.append(&mut self.restyled.borrow_mut());
        changed
    }

    /// Read every shown row again: what is bound to its checkbox, who is at its item, what the
    /// participants are called. Returns the keys whose row shows something else now.
    pub fn refresh_shown(&self) -> Vec<String> {
        let keys: Vec<String> = self.shown.borrow().keys().cloned().collect();
        self.refresh(keys)
    }
}

impl Rows<RowSource> {
    /// Rows that are given rather than read from a view: for tests of the list's chrome.
    #[cfg(test)]
    #[must_use]
    pub fn new(rows: Vec<RowData>) -> Self {
        Self {
            source: None,
            keys: rows.iter().map(|row| Rc::from(row.key.as_str())).collect(),
            index: RefCell::new(None),
            avatars: Cell::new(
                rows.iter()
                    .any(|row| row.avatar.as_ref().is_some_and(RowAvatar::shown)),
            ),
            shown: RefCell::new(rows.into_iter().map(|row| (row.key.clone(), row)).collect()),
            restyled: RefCell::default(),
        }
    }

    /// The row of the item `target`, whether or not it is shown: the rows of a list are keyed
    /// by their loop and their item's id (`<loop>/<id>`).
    #[must_use]
    pub fn key_of(&self, target: &ResourceRef) -> Option<String> {
        // The loop's key is what a shown row's key has before its item's id (an id holds `/`
        // itself); with no row shown, the loop's key holds none.
        let prefix = self
            .shown
            .borrow()
            .values()
            .find_map(|row| {
                row.key
                    .strip_suffix(&row.item.id_string())
                    .map(str::to_owned)
            })
            .or_else(|| {
                let (prefix, _) = self.keys.first()?.split_once('/')?;
                Some(format!("{prefix}/"))
            })?;
        let key = format!("{prefix}{}", target.id_string());
        self.index_of(&key).map(|_| key)
    }
}

impl<S: RowRead> ListModel for Rows<S> {
    type Key = String;

    fn len(&self) -> usize {
        self.keys.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.keys.get(index).map(ToString::to_string)
    }

    fn index_of(&self, key: &String) -> Option<usize> {
        let mut index = self.index.borrow_mut();
        let found = index.get_or_insert_with(|| {
            self.keys
                .iter()
                .enumerate()
                .map(|(at, key)| (Rc::clone(key), at))
                .collect()
        });
        let (stored, was) = found.get_key_value(key.as_str())?;
        let (stored, was) = (Rc::clone(stored), *was);
        // Out from where it was, one place each way at a time.
        let is_it = |at: usize| self.keys.get(at).is_some_and(|k| **k == **key);
        let len = self.keys.len();
        let at = (0..=len.max(was)).find_map(|step| {
            let below = was.checked_sub(step).filter(|at| is_it(*at));
            below.or_else(|| was.checked_add(step).filter(|at| is_it(*at)))
        })?;
        if at != was {
            found.insert(stored, at);
        }
        Some(at)
    }

    /// The view renders the rows in `range` it has not shown yet, and forgets those it showed
    /// outside it.
    fn request_range(&mut self, range: Range<usize>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let Ok(mut view) = source.view().try_borrow_mut() else {
            return;
        };
        let mut shown = self.shown.borrow_mut();
        for patch in view.hide_rows(source.list(), range.clone()) {
            if let Patch::Remove { parent, .. } = patch
                && let Some(row) = parent.last()
            {
                shown.remove(row);
            }
        }
        for index in range {
            let Some(key) = self.keys.get(index) else {
                break;
            };
            if shown.contains_key(&**key) {
                continue;
            }
            let row = match view.row(source.list(), index) {
                Ok(Some(row)) => row.clone(),
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "a row of the list did not render");
                    continue;
                }
            };
            if let Some(data) = source.data(&view, &row) {
                shown.insert(key.to_string(), data);
            }
        }
        self.note_avatars(&shown);
    }

    /// The rows built before the avatar column came in, built again with its room.
    fn poll_changes(&mut self) -> Vec<ListModelChange<String>> {
        let keys = std::mem::take(&mut *self.restyled.borrow_mut());
        if keys.is_empty() {
            return Vec::new();
        }
        vec![ListModelChange::RowsChanged { keys }]
    }

    fn needs_poll(&self) -> bool {
        !self.restyled.borrow().is_empty()
    }

    fn label(&self, index: usize) -> Option<String> {
        let key = self.keys.get(index)?;
        let shown = self.shown.borrow();
        S::label(shown.get(&**key)?)
    }
}

fn text(node: &UiNode, name: &str) -> Option<String> {
    crate::date::shown_brief(node, name).filter(|text| !text.is_empty())
}

/// An icon a view names (`.pin`), when the platform has it.
#[must_use]
pub fn icon_named(name: &str) -> Option<Icon> {
    Icon::from_name(&name.replace('_', "-"))
}

fn icon(node: &UiNode, name: &str) -> Option<Icon> {
    match node.props.get(name) {
        Some(Value::Text(name)) => icon_named(name),
        Some(Value::Opt(Some(inner))) => match inner.as_ref() {
            Value::Text(name) => icon_named(name),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the selection being made comes from a click or a tap the person just made, rather
/// than the keyboard or an assistive technology.
fn by_hand() -> bool {
    matches!(
        ui::motion::recent_input(ui::time::now()),
        Some(ui::tokens::MotionSource::Pointer | ui::tokens::MotionSource::Gesture)
    )
}

/// The row of one item. `avatars`: rows of its list show avatars, so one without keeps the room.
fn row(data: &RowData, avatars: bool, requests: &Requests, sink: &EventSink) -> Element {
    let title = data
        .row
        .as_ref()
        .and_then(|node| text(node, "title"))
        .unwrap_or_else(|| data.label.clone());
    let mut row = ListRow::new(data.key.clone(), title);
    if let Some(node) = &data.row {
        if let Some(secondary) = text(node, "secondary") {
            row = row.secondary(secondary);
        }
        if let Some(meta) = text(node, "meta") {
            row = row.meta(meta);
        }
        if let Some(value) = text(node, "value") {
            row = row.value(value);
        }
        if let Some(badge) = text(node, "badge") {
            row = row.badge(badge);
        }
        if let Some(mark) = icon(node, "mark") {
            row = row.mark(mark);
        }
        if let Some(leading) = icon(node, "icon") {
            row = row.icon(leading);
        }
        if let Some(checked) = data.checked {
            let sink = Rc::clone(sink);
            let key = node.key.clone();
            row = row.checkbox(checked, move |checked| {
                sink.borrow_mut()
                    .push(UiEvent::Toggle(key.clone(), checked));
            });
        }
    }
    match &data.avatar {
        Some(RowAvatar::Of(name, identity)) => row = row.avatar(name.clone(), identity.clone()),
        Some(RowAvatar::Glyph(name, glyph)) => {
            row = row
                .avatar(name.clone(), data.item.id_string())
                .avatar_glyph(*glyph);
        }
        Some(RowAvatar::Space) if avatars => row = row.avatar_space(),
        Some(RowAvatar::Space) | None => {}
    }
    if let Some(identity) = &data.presence {
        row = row.presence(identity.clone());
    }
    let swipe = data
        .actions
        .iter()
        .find(|action| action.destructive)
        .or_else(|| data.actions.first());
    if let Some(action) = swipe {
        let requests = Rc::clone(requests);
        let invoke = invoke_request(data, action);
        row = row.swipe_action(
            action.icon.unwrap_or(if action.destructive {
                Icon::Trash
            } else {
                Icon::Check
            }),
            action.label.clone(),
            action.destructive,
            move || requests.borrow_mut().push(invoke.clone()),
        );
    }
    Box::new(row)
}

/// The menu of the row under `at` (its focused row when the keyboard opened it): every action
/// on its item, destructive ones styled so. Opening it selects the row, as the next column shows.
fn row_menu(list: &dyn Widget, at: Option<Point>, requests: &Requests) -> Vec<MenuEntry> {
    let Some(list) = list
        .as_any()
        .and_then(|list| list.downcast_ref::<VirtualList<Rows>>())
    else {
        return Vec::new();
    };
    let key = match at {
        Some(point) => list.key_at_point(point),
        None => list.focused_key().cloned(),
    };
    let Some(data) = key.and_then(|key| list.model().find(&key)) else {
        return Vec::new();
    };
    if data.actions.is_empty() {
        return Vec::new();
    }
    requests
        .borrow_mut()
        .push(Request::Select(data.key.clone()));
    data.actions
        .iter()
        .map(|action| {
            let requests = Rc::clone(requests);
            let invoke = invoke_request(&data, action);
            let run = move || requests.borrow_mut().push(invoke.clone());
            let entry = if action.destructive {
                MenuEntry::destructive(action.label.clone(), run)
            } else {
                MenuEntry::new(action.label.clone(), run)
            };
            match action.icon {
                Some(icon) => entry.with_icon(icon),
                None => entry,
            }
        })
        .collect()
}

fn invoke_request(data: &RowData, action: &RowAction) -> Request {
    Request::Invoke {
        action: action.clone(),
        title: data
            .row
            .as_ref()
            .and_then(|node| text(node, "title"))
            .unwrap_or_else(|| data.label.clone()),
    }
}

/// What a list without items says, and the action it offers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Empty {
    pub sentence: String,
    /// The app's icon, which the empty state shows above the sentence as native apps do.
    pub icon: Option<Icon>,
    /// The frame's primary action, when the list is empty because nothing was made yet (not
    /// because a filter hides everything).
    pub action: Option<RowAction>,
}

enum Body {
    /// The list under the one context menu every row shares.
    List(Box<ContextMenuArea>),
    Empty(Box<EmptyState>),
}

/// The list column, kept in step with the view.
pub struct ListPane {
    requests: Requests,
    sink: EventSink,
    body: Body,
    /// The rows while the list is empty: the empty state shows instead of the list, and the
    /// list is built with them when the first row comes.
    waiting: Option<Rows>,
    pub(crate) empty: Empty,
    name: String,
    /// The item shown in the next column.
    pub(crate) selected: Option<String>,
    /// Whether the rows are to be read anew.
    stale: bool,
    structure_dirty: bool,
}

impl ListPane {
    #[must_use]
    pub fn new(requests: &Requests, sink: &EventSink, name: String) -> Self {
        Self {
            requests: Rc::clone(requests),
            sink: Rc::clone(sink),
            body: Body::Empty(Box::new(EmptyState::new(""))),
            waiting: Some(Rows::default()),
            empty: Empty::default(),
            name,
            selected: None,
            stale: false,
            structure_dirty: true,
        }
    }

    fn list(&self, rows: Rows) -> ContextMenuArea {
        let requests = Rc::clone(&self.requests);
        let sink = Rc::clone(&self.sink);
        let selecting = Rc::clone(&self.requests);
        let activating = Rc::clone(&self.requests);
        let list = VirtualList::with_selection_delegate(rows, 56.0, move |rows, _, key, _, _| {
            rows.find(key).map_or_else(
                || Box::new(ListRow::new(key.clone(), "")) as Element,
                |data| row(&data, rows.shows_avatars(), &requests, &sink),
            )
        })
        .filling()
        // The list is the column's body: its rows' text starts where the column's header
        // starts its title (HIG's one content edge).
        .in_column()
        .selection_follows_focus()
        // A click or a tap on a row opens its item: a narrow window pushes it in front, as
        // selecting an entry of an overview does (HIG §1b). Moving through the list with the
        // keyboard shows the item without bringing it to the front; Enter does.
        .on_select(move |key, selected| {
            if selected {
                let request = if by_hand() {
                    Request::Activate(key)
                } else {
                    Request::Select(key)
                };
                selecting.borrow_mut().push(request);
            }
        })
        .on_activate(move |key| activating.borrow_mut().push(Request::Activate(key)))
        .with_id("tilolang.list")
        .with_accessible_name(self.name.clone())
        .with_semantic_key(Clone::clone);
        let menu = Rc::clone(&self.requests);
        ContextMenuArea::over_content(Box::new(list), move |list, at| row_menu(list, at, &menu))
    }

    /// The list, when there are rows.
    fn virtual_list(&mut self) -> Option<&mut VirtualList<Rows>> {
        let Body::List(area) = &mut self.body else {
            return None;
        };
        area.content_mut()
            .as_any_mut()
            .and_then(|list| list.downcast_mut::<VirtualList<Rows>>())
    }

    fn virtual_list_ref(&self) -> Option<&VirtualList<Rows>> {
        let Body::List(area) = &self.body else {
            return None;
        };
        area.content()
            .as_any()
            .and_then(|list| list.downcast_ref::<VirtualList<Rows>>())
    }

    /// The rows, wherever they are.
    fn rows(&self) -> Option<&Rows> {
        self.virtual_list_ref()
            .map(VirtualList::model)
            .or(self.waiting.as_ref())
    }

    fn empty_state(empty: &Empty) -> EmptyState {
        let state = EmptyState::new(empty.sentence.clone());
        let state = match empty.icon {
            Some(icon) => state.with_icon(icon),
            None => state,
        };
        match &empty.action {
            Some(action) => state.with_action(action.label.clone()),
            None => state,
        }
    }

    /// Show the rows of `source`'s list, unless they are already the ones shown.
    pub fn read(&mut self, source: &Rc<RowSource>) {
        if !self.stale && self.rows().is_some_and(|rows| rows.reads(source)) {
            return;
        }
        self.stale = false;
        self.show(Rows::of(Rc::clone(source)));
    }

    /// Read the rows anew at the next [`Self::read`]: the view replaced its list as a whole.
    pub fn forget(&mut self) {
        self.stale = true;
    }

    /// Show `rows`: the list, or the empty state when there are none.
    fn show(&mut self, rows: Rows) {
        if rows.is_empty() {
            self.body = Body::Empty(Box::new(Self::empty_state(&self.empty)));
            self.waiting = Some(rows);
        } else {
            self.body = Body::List(Box::new(self.list(rows)));
            self.waiting = None;
            self.select_shown();
        }
        self.structure_dirty = true;
    }

    /// What the list says when it has no rows.
    pub fn set_empty(&mut self, empty: Empty) {
        if self.empty == empty {
            return;
        }
        if let Body::Empty(_) = self.body {
            self.body = Body::Empty(Box::new(Self::empty_state(&empty)));
            self.structure_dirty = true;
        }
        self.empty = empty;
    }

    /// The view inserted, removed or moved rows of the list (`structure`, in order) and changed
    /// the rows of `rows` (by key).
    pub fn changed(&mut self, structure: &[Patch], rows: Vec<String>) {
        if let Some(mut rows) = self.waiting.take() {
            rows.apply(structure);
            if rows.is_empty() {
                self.waiting = Some(rows);
            } else {
                self.show(rows);
            }
            return;
        }
        let Some(list) = self.virtual_list() else {
            return;
        };
        apply_changes(list, structure, rows);
        if list.model().is_empty() {
            let rows = list.update_model(std::mem::take);
            self.show(rows);
        }
    }

    /// Read every shown row again, after what they show of the room moved: a bound checkbox's
    /// field, who is at an item, a participant's name.
    pub fn refresh_shown(&mut self) {
        if let Some(list) = self.virtual_list() {
            let refreshed = list.model().refresh_shown();
            if !refreshed.is_empty() {
                list.invalidate_rows(refreshed);
            }
        }
    }

    /// The row of `key`, when the list shows it.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<RowData> {
        self.rows().and_then(|rows| rows.find(key))
    }

    /// The row of the item `target`, shown or not.
    #[must_use]
    pub fn key_of(&self, target: &ResourceRef) -> Option<String> {
        self.rows().and_then(|rows| rows.key_of(target))
    }

    /// How many rows the list has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows().map_or(0, ListModel::len)
    }

    /// The rows the list shows, in order.
    #[cfg(test)]
    #[must_use]
    pub fn shown(&self) -> Vec<RowData> {
        self.rows().map(Rows::shown).unwrap_or_default()
    }

    /// Show the rows of `range`, as the list does when they scroll into view.
    #[cfg(test)]
    pub fn show_rows(&mut self, range: Range<usize>) {
        if let Some(list) = self.virtual_list() {
            list.update_model(|rows| rows.request_range(range));
        }
    }

    /// Show `key` as the selected row: the item the next column shows.
    pub fn select(&mut self, key: Option<String>) {
        if self.selected == key {
            return;
        }
        self.selected = key;
        self.select_shown();
    }

    fn select_shown(&mut self) {
        let selected = self.selected.clone();
        if let Some(list) = self.virtual_list() {
            let selection = list.selection_mut();
            selection.clear();
            if let Some(key) = selected {
                selection.select_only(key);
            }
        }
    }

    /// The empty state's action, when it was activated.
    fn take_empty_action(&mut self) {
        let Body::Empty(state) = &mut self.body else {
            return;
        };
        if !state.take_action() {
            return;
        }
        if let Some(action) = &self.empty.action {
            self.requests.borrow_mut().push(Request::Invoke {
                action: action.clone(),
                title: String::new(),
            });
        }
    }
}

/// The view inserted, removed or moved rows of the list `list` shows (`structure`, in order)
/// and changed the rows of `rows` (by key): the list takes each change as it is, and rebuilds
/// alone each shown row that shows something else now.
pub fn apply_changes<S: RowRead>(
    list: &mut VirtualList<Rows<S>>,
    structure: &[Patch],
    rows: Vec<String>,
) {
    if !structure.is_empty() {
        let changes = list.model().changes_of(structure);
        list.update_model_changes(&changes, |rows| {
            rows.apply(structure);
        });
    }
    let refreshed = list.model().refresh(rows);
    if !refreshed.is_empty() {
        list.invalidate_rows(refreshed);
    }
}

impl Decorator for ListPane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        match &self.body {
            Body::List(list) => list.as_ref(),
            Body::Empty(empty) => empty.as_ref(),
        }
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.body {
            Body::List(list) => list.as_mut(),
            Body::Empty(empty) => empty.as_mut(),
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.inner_mut().update(state, path);
        self.take_empty_action();
        changed || self.structure_dirty
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.inner_mut().take_structure_dirty() | own
    }

    /// A body swapped (the list for the empty state) is a rebuild; anything else is what the
    /// body says of itself, so rows a scroll mounts stay described.
    fn take_structure_change(&mut self) -> StructureChange {
        let own = std::mem::take(&mut self.structure_dirty);
        let inner = self.inner_mut().take_structure_change();
        if own { StructureChange::Rebuild } else { inner }
    }

    fn name(&self) -> &'static str {
        "TilolangList"
    }
}

#[cfg(test)]
mod tests {
    use ui::widget::MenuItem;

    use super::*;

    fn data(key: &str, title: &str) -> RowData {
        RowData {
            key: key.to_owned(),
            item: ResourceRef::root(key, "t"),
            label: title.to_owned(),
            row: None,
            checked: None,
            avatar: None,
            actions: Rc::from(Vec::new()),
            presence: None,
        }
    }

    fn node(key: &str) -> UiNode {
        UiNode {
            kind: "fragment".to_owned(),
            key: key.to_owned(),
            props: std::collections::BTreeMap::new(),
            binding: None,
            events: std::collections::BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        }
    }

    /// Replay `changes` on `keys`, as the list applies them.
    fn replay(
        mut keys: Vec<String>,
        new: &[String],
        changes: &[ListModelChange<String>],
    ) -> Vec<String> {
        for change in changes {
            match change {
                ListModelChange::RowsRemoved { index, count } => {
                    for _ in 0..*count {
                        keys.remove(*index);
                    }
                }
                ListModelChange::RowsInserted { index, .. } => {
                    keys.insert(*index, new[*index].clone());
                }
                ListModelChange::RowsMoved { from, to, .. } => {
                    let key = keys.remove(*from);
                    keys.insert(*to, key);
                }
                ListModelChange::RowsChanged { .. } | ListModelChange::Reset => {}
            }
        }
        keys
    }

    #[test]
    fn the_views_patches_of_a_list_are_the_changes_of_its_rows() {
        let keys =
            |list: &[&str]| -> Vec<String> { list.iter().map(|k| (*k).to_owned()).collect() };
        let shared = |list: &[String]| -> Vec<Rc<str>> {
            list.iter().map(|k| Rc::from(k.as_str())).collect()
        };
        let old = keys(&["a", "b", "c", "d"]);
        let mut rows: Rows = Rows {
            keys: shared(&old),
            ..Rows::default()
        };
        // Where each row is was looked up before the change: it is kept up to date.
        assert_eq!(rows.index_of(&"d".to_owned()), Some(3));
        let parent = vec!["list".to_owned()];
        let changes = rows.apply(&[
            Patch::Remove {
                parent: parent.clone(),
                key: "b".to_owned(),
            },
            Patch::Insert {
                parent: parent.clone(),
                index: 0,
                node: node("n"),
            },
            Patch::Move {
                parent,
                key: "c".to_owned(),
                to: 1,
            },
        ]);
        let new = keys(&["n", "c", "a", "d"]);
        assert_eq!(rows.keys, shared(&new));
        assert_eq!(replay(old, &new, &changes), new, "{changes:?}");
        for (at, key) in new.iter().enumerate() {
            assert_eq!(rows.index_of(key), Some(at), "{key}");
        }
        assert_eq!(rows.index_of(&"b".to_owned()), None);
    }

    #[test]
    fn one_change_is_told_against_the_rows_as_they_are() {
        let keys: Vec<Rc<str>> = (0..1_000).map(|i| Rc::from(format!("r{i}"))).collect();
        let mut rows: Rows = Rows {
            keys,
            ..Rows::default()
        };
        assert_eq!(rows.index_of(&"r999".to_owned()), Some(999));
        let parent = vec!["list".to_owned()];
        let insert = [Patch::Insert {
            parent: parent.clone(),
            index: 0,
            node: node("new"),
        }];
        let told = rows.changes_of(&insert);
        assert_eq!(rows.apply(&insert), told);
        let remove = [Patch::Remove {
            parent,
            key: "r500".to_owned(),
        }];
        let told = rows.changes_of(&remove);
        assert_eq!(
            told,
            [ListModelChange::RowsRemoved {
                index: 501,
                count: 1
            }]
        );
        assert_eq!(rows.apply(&remove), told);
        assert_eq!(rows.index_of(&"r999".to_owned()), Some(999));
        assert_eq!(rows.index_of(&"r0".to_owned()), Some(1));
    }

    fn action(label: &str, destructive: bool) -> RowAction {
        RowAction {
            label: label.to_owned(),
            icon: None,
            resource: "tl:notes:root".to_owned(),
            action: label.to_lowercase(),
            primary: false,
            destructive,
            args: Vec::new(),
            on: None,
            toggles: None,
        }
    }

    /// The list selects and separates its rows only when each is the [`ListRow`] itself, so the
    /// rows share one context menu over the list rather than each wearing its own.
    #[test]
    fn every_action_of_a_row_is_on_the_one_menu_over_the_list() {
        let requests: Requests = Rc::default();
        let sink: EventSink = Rc::default();
        let actions: Rc<[RowAction]> = Rc::from(vec![action("Pin", false), action("Delete", true)]);
        let rows: Vec<RowData> = ["a", "b"]
            .into_iter()
            .map(|key| RowData {
                actions: Rc::clone(&actions),
                ..data(key, &key.to_uppercase())
            })
            .collect();
        let element = row(&rows[0], false, &requests, &sink);
        assert!(
            element
                .as_any()
                .is_some_and(|row| row.downcast_ref::<ListRow>().is_some()),
            "a row is a bare ListRow"
        );

        let mut pane = ListPane::new(&requests, &sink, "Notes".to_owned());
        pane.show(Rows::new(rows));
        assert!(pane.virtual_list().expect("rows").reveal(&"b".to_owned()));
        let Body::List(area) = &mut pane.body else {
            panic!("rows show a list");
        };
        assert!(
            area.open_at(Point::ZERO, false),
            "the keyboard opens the focused row's menu"
        );
        let labels: Vec<Option<&str>> = area.menu().items().iter().map(MenuItem::label).collect();
        assert_eq!(labels, vec![Some("Pin"), Some("Delete")]);
        assert_eq!(
            requests.borrow().last(),
            Some(&Request::Select("b".to_owned())),
            "opening the menu selects its row"
        );
        area.choose(1);
        assert_eq!(
            requests.borrow().last(),
            Some(&Request::Invoke {
                action: action("Delete", true),
                title: "B".to_owned(),
            })
        );
    }

    #[test]
    fn a_list_whose_last_row_goes_shows_its_empty_state() {
        let requests: Requests = Rc::default();
        let sink: EventSink = Rc::default();
        let mut pane = ListPane::new(&requests, &sink, "Notes".to_owned());
        pane.show(Rows::new(vec![data("a", "A")]));
        assert!(matches!(pane.body, Body::List(_)));
        Decorator::take_structure_change(&mut pane);
        pane.changed(
            &[Patch::Remove {
                parent: vec!["list".to_owned()],
                key: "a".to_owned(),
            }],
            Vec::new(),
        );
        assert!(matches!(pane.body, Body::Empty(_)));
        assert_eq!(
            Decorator::take_structure_change(&mut pane),
            StructureChange::Rebuild
        );
        pane.changed(
            &[Patch::Insert {
                parent: vec!["list".to_owned()],
                index: 0,
                node: node("b"),
            }],
            Vec::new(),
        );
        assert!(matches!(pane.body, Body::List(_)));
        assert_eq!(pane.len(), 1);
    }

    fn with_avatar(key: &str, avatar: RowAvatar) -> RowData {
        RowData {
            avatar: Some(avatar),
            ..data(key, &key.to_uppercase())
        }
    }

    #[test]
    fn a_list_where_no_row_has_an_avatar_keeps_no_room_for_one() {
        let rows = Rows::new(vec![
            with_avatar("a", RowAvatar::Space),
            with_avatar("b", RowAvatar::Space),
        ]);
        assert!(!rows.shows_avatars(), "no row shows an avatar");
        let mixed = Rows::new(vec![
            with_avatar("a", RowAvatar::Space),
            with_avatar("b", RowAvatar::Of("Ada".to_owned(), "ada".to_owned())),
        ]);
        assert!(
            mixed.shows_avatars(),
            "beside one that does, a row keeps the room"
        );
        let group = Rows::new(vec![with_avatar(
            "a",
            RowAvatar::Glyph("Weekend".to_owned(), Icon::Users),
        )]);
        assert!(group.shows_avatars(), "a group's avatar is an avatar");
    }

    #[test]
    fn rows_built_before_an_avatar_came_in_are_built_again_with_its_room() {
        let mut rows = Rows::new(vec![with_avatar("a", RowAvatar::Space)]);
        assert!(!rows.shows_avatars());
        assert!(rows.poll_changes().is_empty());
        // A row with an avatar scrolls in.
        let mut shown = rows.shown.borrow().clone();
        shown.insert(
            "b".to_owned(),
            with_avatar("b", RowAvatar::Of("Ada".to_owned(), "ada".to_owned())),
        );
        rows.note_avatars(&shown);
        assert!(rows.shows_avatars());
        assert!(rows.needs_poll());
        assert_eq!(
            rows.poll_changes(),
            vec![ListModelChange::RowsChanged {
                keys: vec!["a".to_owned()]
            }],
            "the row without one is built again, with the room"
        );
        assert!(!rows.needs_poll());
    }
}
