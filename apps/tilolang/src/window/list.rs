//! A `list` frame's items: a [`VirtualList`] of [`ListRow`]s, or the empty state when there are
//! none.
//!
//! The list is one focus stop. Selection follows focus and shows the item in the next column;
//! activating a row brings that column to the front of a narrow window. Every action on an
//! item is on its row's context menu, the first destructive one (else the first) on its swipe.
//!
//! The rows come from a [`ListModel`]: its length and keys, and each row when the list draws it.
//! A model that evaluates rows lazily asks for them in `request_range`; the model here holds the
//! rows the view evaluated.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tilolang_tilo::runtime::{ResourceRef, UiNode, Value};
use ui::icons::Icon;
use ui::vello::kurbo::Point;
use ui::widget::virtual_list::ListModelChange;
use ui::widget::{
    AppModel, ContextMenuArea, Decorator, Element, EmptyState, ListModel, ListRow, MenuEntry,
    ModelPath, VirtualList, Widget,
};

use super::{Request, Requests};
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
}

/// One row of a list: the item, and the `row(...)` the view made for it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowData {
    /// The view's key of the item.
    pub key: String,
    pub item: ResourceRef,
    /// The title when the view made no row for the item.
    pub label: String,
    /// The `list_row` node, when the view made one.
    pub row: Option<UiNode>,
    /// What the row's checkbox shows, when it is bound to a field.
    pub checked: Option<bool>,
    /// What a participant's avatar on the row is called.
    pub avatar: Option<(String, String)>,
    pub actions: Rc<[RowAction]>,
    /// Who else is at the item now: a collaborator or an agent whose window shows it.
    pub presence: Option<String>,
}

/// The rows of a list, by key.
#[derive(Default)]
pub struct Rows {
    rows: Vec<RowData>,
    index: HashMap<String, usize>,
}

impl Rows {
    #[must_use]
    pub fn new(rows: Vec<RowData>) -> Self {
        let index = rows
            .iter()
            .enumerate()
            .map(|(at, row)| (row.key.clone(), at))
            .collect();
        Self { rows, index }
    }

    #[must_use]
    pub fn row(&self, index: usize) -> Option<&RowData> {
        self.rows.get(index)
    }

    /// The row of `key`.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&RowData> {
        self.index.get(key).and_then(|at| self.rows.get(*at))
    }
}

impl ListModel for Rows {
    type Key = String;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.rows.get(index).map(|row| row.key.clone())
    }

    fn index_of(&self, key: &String) -> Option<usize> {
        self.index.get(key).copied()
    }
}

/// What turns `old` into `new`, one row at a time, each change against the order the ones
/// before it left: the rows that went, the rows that came or moved, and the rows that stayed
/// and changed.
#[must_use]
pub fn changes(old: &[RowData], new: &[RowData]) -> Vec<ListModelChange<String>> {
    let mut out = Vec::new();
    let kept: HashSet<&str> = new.iter().map(|row| row.key.as_str()).collect();
    let mut order: Vec<&str> = old.iter().map(|row| row.key.as_str()).collect();
    for index in (0..order.len()).rev() {
        if order.get(index).is_some_and(|key| !kept.contains(key)) {
            order.remove(index);
            out.push(ListModelChange::RowsRemoved { index, count: 1 });
        }
    }
    for (index, row) in new.iter().enumerate() {
        if order.get(index) == Some(&row.key.as_str()) {
            continue;
        }
        if let Some(from) = order.iter().position(|key| *key == row.key) {
            let key = order.remove(from);
            order.insert(index, key);
            out.push(ListModelChange::RowsMoved {
                from,
                to: index,
                count: 1,
            });
        } else {
            order.insert(index, row.key.as_str());
            out.push(ListModelChange::RowsInserted { index, count: 1 });
        }
    }
    let before: HashMap<&str, &RowData> = old.iter().map(|row| (row.key.as_str(), row)).collect();
    let changed: Vec<String> = new
        .iter()
        .filter(|row| before.get(row.key.as_str()).is_some_and(|was| *was != *row))
        .map(|row| row.key.clone())
        .collect();
    if !changed.is_empty() {
        out.push(ListModelChange::RowsChanged { keys: changed });
    }
    out
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

/// The row of one item.
fn row(data: &RowData, requests: &Requests, sink: &EventSink) -> Element {
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
    if let Some((name, identity)) = &data.avatar {
        row = row.avatar(name.clone(), identity.clone());
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
            let invoke = invoke_request(data, action);
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
    pub(crate) rows: Vec<RowData>,
    pub(crate) empty: Empty,
    name: String,
    /// The item shown in the next column.
    pub(crate) selected: Option<String>,
    structure_dirty: bool,
}

impl ListPane {
    #[must_use]
    pub fn new(requests: &Requests, sink: &EventSink, name: String) -> Self {
        Self {
            requests: Rc::clone(requests),
            sink: Rc::clone(sink),
            body: Body::Empty(Box::new(EmptyState::new(""))),
            rows: Vec::new(),
            empty: Empty::default(),
            name,
            selected: None,
            structure_dirty: true,
        }
    }

    fn list(&self, rows: Rows) -> ContextMenuArea {
        let requests = Rc::clone(&self.requests);
        let sink = Rc::clone(&self.sink);
        let selecting = Rc::clone(&self.requests);
        let activating = Rc::clone(&self.requests);
        let list =
            VirtualList::with_selection_delegate(rows, 56.0, move |rows, index, key, _, _| {
                rows.row(index).map_or_else(
                    || Box::new(ListRow::new(key.clone(), "")) as Element,
                    |data| row(data, &requests, &sink),
                )
            })
            .filling()
            .selection_follows_focus()
            // Moving through the list shows the item without bringing it to the front of a narrow
            // window; activating it does.
            .on_select(move |key, selected| {
                if selected {
                    selecting.borrow_mut().push(Request::Select(key));
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

    /// Show `rows`, or `empty` when there are none.
    pub fn show(&mut self, rows: Vec<RowData>, empty: Empty) {
        if rows.is_empty() {
            let rebuild = !matches!(self.body, Body::Empty(_)) || self.empty != empty;
            if rebuild {
                self.body = Body::Empty(Box::new(Self::empty_state(&empty)));
                self.structure_dirty = true;
            }
        } else if matches!(self.body, Body::List(_)) {
            let changes = changes(&self.rows, &rows);
            if !changes.is_empty() {
                let next = Rows::new(rows.clone());
                if let Some(list) = self.virtual_list() {
                    list.update_model_changes(&changes, move |model| *model = next);
                }
            }
        } else {
            self.body = Body::List(Box::new(self.list(Rows::new(rows.clone()))));
            self.structure_dirty = true;
            self.select_shown();
        }
        self.rows = rows;
        self.empty = empty;
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

    /// Replay `changes` on the keys of `old`, as the list applies them.
    fn replay(
        old: &[RowData],
        new: &[RowData],
        changes: &[ListModelChange<String>],
    ) -> Vec<String> {
        let mut keys: Vec<String> = old.iter().map(|row| row.key.clone()).collect();
        for change in changes {
            match change {
                ListModelChange::RowsRemoved { index, count } => {
                    for _ in 0..*count {
                        keys.remove(*index);
                    }
                }
                ListModelChange::RowsInserted { index, .. } => {
                    keys.insert(*index, new[*index].key.clone());
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
    fn the_changes_of_a_list_turn_the_old_rows_into_the_new_ones() {
        let old = vec![
            data("a", "A"),
            data("b", "B"),
            data("c", "C"),
            data("d", "D"),
        ];
        let new = vec![
            data("n", "N"),
            data("c", "C"),
            data("a", "A2"),
            data("d", "D"),
        ];
        let changes = changes(&old, &new);
        assert_eq!(
            replay(&old, &new, &changes),
            vec!["n", "c", "a", "d"],
            "{changes:?}"
        );
        assert!(changes.contains(&ListModelChange::RowsChanged {
            keys: vec!["a".to_owned()]
        }));
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
        let element = row(&rows[0], &requests, &sink);
        assert!(
            element
                .as_any()
                .is_some_and(|row| row.downcast_ref::<ListRow>().is_some()),
            "a row is a bare ListRow"
        );

        let mut pane = ListPane::new(&requests, &sink, "Notes".to_owned());
        pane.show(rows, Empty::default());
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
    fn an_unchanged_list_has_no_changes() {
        let rows = vec![data("a", "A"), data("b", "B")];
        assert!(changes(&rows, &rows).is_empty());
    }
}
