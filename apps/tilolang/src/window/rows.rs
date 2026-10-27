//! The rows of a view's virtual lists (`ui.VirtualList`), rendered as the toolkit's list shows
//! them.
//!
//! The view's loop over a collection of any length renders a row only when the list asks for
//! it (`request_range`: the visible rows and the overscan) and forgets it when it scrolls
//! away, so a list of ten thousand notes costs the rows on screen. The view's changes reach the
//! list as they are: a row inserted, removed or moved is that change of the list, and a row
//! the view changed is read again and rebuilt alone. Each row is read through the one adapter,
//! as the list's item component (`ui.ListItem`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use tilolang_tilo::runtime::{Patch, UiNode, Value, ViewInstance};
use ui::component::{RowSource, Source};
use ui::widget::virtual_list::ListModelChange;

use crate::adapter::{NodeSource, description_of, kind};
use crate::reconcile::Reconciler;

/// The rows of one virtual list of a view.
pub struct HostRows {
    view: Rc<RefCell<ViewInstance>>,
    /// The key of the list node in the view.
    list: String,
    /// What the keys of the view's nodes carry in the window's tree (the window after the
    /// root's carries one).
    prefix: String,
    /// Reads the rows as the window's other components are read, reporting to the same place.
    reconciler: Rc<RefCell<Reconciler>>,
    keys: RefCell<Keys>,
    /// Whether `keys` holds the key of every row. Until a change needs them, the rows' keys are
    /// read from the view as the list asks for them: a list of a hundred thousand notes opens
    /// reading the pages its rows are on, not all of them.
    read: std::cell::Cell<bool>,
}

/// The keys of the rows of a list, rendered or not (every one, once read), and what changed of
/// them.
#[derive(Default)]
struct Keys {
    /// The rows' keys, in order.
    order: Vec<Rc<str>>,
    /// Where each key was last found, built when first asked. A row inserted, removed or moved
    /// moves each other row by at most one place, so a key is looked for around where it was
    /// and the place found is kept: a change costs the rows it touches, not the list's length.
    index: RefCell<Option<HashMap<Rc<str>, usize>>>,
    /// The rows rendered, by key.
    shown: HashSet<String>,
    changes: Vec<ListModelChange<String>>,
}

/// Every virtual list of the window's views, by the key of its node in the window's tree.
pub type Registry = Rc<RefCell<HashMap<String, Rc<RefCell<HostRows>>>>>;

impl HostRows {
    /// The rows of the list `list` of `view`, none rendered yet.
    pub fn of(
        view: &Rc<RefCell<ViewInstance>>,
        list: &str,
        prefix: &str,
        reconciler: Rc<RefCell<Reconciler>>,
    ) -> Self {
        Self {
            view: Rc::clone(view),
            list: list.to_owned(),
            prefix: prefix.to_owned(),
            reconciler,
            keys: RefCell::default(),
            read: std::cell::Cell::new(false),
        }
    }

    /// The keys, with every row's read first when they are not.
    fn every_key(&self) -> std::cell::RefMut<'_, Keys> {
        let mut keys = self.keys.borrow_mut();
        if !self.read.replace(true) {
            keys.order = self.read_keys();
        }
        keys
    }

    /// Whether these are the rows of `list` of `view`.
    #[must_use]
    pub fn reads(&self, view: &Rc<RefCell<ViewInstance>>, list: &str) -> bool {
        Rc::ptr_eq(&self.view, view) && self.list == list
    }

    fn read_keys(&self) -> Vec<Rc<str>> {
        let Ok(view) = self.view.try_borrow() else {
            return Vec::new();
        };
        let len = view.list_len(&self.list).unwrap_or(0);
        (0..len)
            .filter_map(|index| view.row_key(&self.list, index))
            .map(Rc::from)
            .collect()
    }

    /// The view replaced the list as a whole: read its rows anew.
    pub fn reread(&mut self) {
        let keys = self.keys.get_mut();
        *keys = Keys::default();
        self.read.set(false);
        keys.changes.push(ListModelChange::Reset);
    }

    /// Apply the view's changes of the list's own children, in order: rows inserted, removed
    /// and moved, each queued for the list against the order the ones before it left.
    pub fn apply(&mut self, patches: &[Patch]) {
        if self.read.get() {
            self.keys.get_mut().apply(patches);
            return;
        }
        let structural = patches.iter().filter(|patch| {
            matches!(
                patch,
                Patch::Insert { .. } | Patch::Remove { .. } | Patch::Move { .. }
            )
        });
        let inserted: Option<Vec<usize>> = structural
            .map(|patch| match patch {
                Patch::Insert { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        if let Some(indices) = inserted {
            // Rows inserted say where: the keys need not be read for them.
            let keys = self.keys.get_mut();
            for index in indices {
                keys.changes
                    .push(ListModelChange::RowsInserted { index, count: 1 });
            }
        } else {
            // Where a removed or moved row was is in the order before the change, which was
            // never read: the list reads its rows anew, and the keys are read from now on.
            let order = self.read_keys();
            let keys = self.keys.get_mut();
            *keys = Keys {
                order,
                ..Keys::default()
            };
            self.read.set(true);
            keys.changes.push(ListModelChange::Reset);
        }
    }

    /// The view changed what the rendered rows of `keys` show: the list builds them again.
    pub fn changed(&mut self, keys: impl IntoIterator<Item = String>) {
        self.keys.get_mut().changed(keys);
    }

    /// Show again what each rendered row's bound fields hold: a checkbox's state.
    pub fn refresh_bound(&self) {
        let (Ok(view), Ok(reconciler)) = (self.view.try_borrow(), self.reconciler.try_borrow())
        else {
            return;
        };
        for key in &self.keys.borrow().shown {
            let Some(row) = view.find(key) else { continue };
            bound_values(&view, row, &self.prefix, &reconciler);
        }
    }
}

impl Keys {
    fn apply(&mut self, patches: &[Patch]) {
        for patch in patches {
            let change = match patch {
                Patch::Insert { index, node, .. } => {
                    let index = (*index).min(self.order.len());
                    let key: Rc<str> = Rc::from(node.key.as_str());
                    self.order.insert(index, Rc::clone(&key));
                    self.found(key, index);
                    ListModelChange::RowsInserted { index, count: 1 }
                }
                Patch::Remove { key, .. } => {
                    let Some(index) = self.index_of(key) else {
                        continue;
                    };
                    self.order.remove(index);
                    if let Some(index) = self.index.get_mut().as_mut() {
                        index.remove(key.as_str());
                    }
                    self.shown.remove(key);
                    ListModelChange::RowsRemoved { index, count: 1 }
                }
                Patch::Move { key, to, .. } => {
                    let Some(from) = self.index_of(key) else {
                        continue;
                    };
                    let moved = self.order.remove(from);
                    let to = (*to).min(self.order.len());
                    self.order.insert(to, Rc::clone(&moved));
                    self.found(moved, to);
                    ListModelChange::RowsMoved { from, to, count: 1 }
                }
                _ => continue,
            };
            self.changes.push(change);
        }
    }

    fn changed(&mut self, keys: impl IntoIterator<Item = String>) {
        let keys: Vec<String> = keys
            .into_iter()
            .filter(|key| self.shown.contains(key))
            .collect();
        if !keys.is_empty() {
            self.changes.push(ListModelChange::RowsChanged { keys });
        }
    }

    /// A row of a loop over resources is keyed by the loop and the resource's id
    /// (`<loop>/<id>`): the key of the row showing `id`, when it is one of the list's.
    fn key_presenting(&self, id: &str) -> Option<String> {
        let (prefix, _) = self.order.first()?.split_once('/')?;
        let key = format!("{prefix}/{id}");
        self.index_of(&key).map(|_| key)
    }

    fn found(&mut self, key: Rc<str>, index: usize) {
        if let Some(found) = self.index.get_mut().as_mut() {
            found.insert(key, index);
        }
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        let mut index = self.index.borrow_mut();
        let found = index.get_or_insert_with(|| {
            self.order
                .iter()
                .enumerate()
                .map(|(at, key)| (Rc::clone(key), at))
                .collect()
        });
        let (stored, was) = found.get_key_value(key)?;
        let (stored, was) = (Rc::clone(stored), *was);
        // Out from where it was, one place each way at a time.
        let is_it = |at: usize| self.order.get(at).is_some_and(|k| **k == *key);
        let len = self.order.len();
        let at = (0..=len.max(was)).find_map(|step| {
            let below = was.checked_sub(step).filter(|at| is_it(*at));
            below.or_else(|| was.checked_add(step).filter(|at| is_it(*at)))
        })?;
        if at != was {
            found.insert(stored, at);
        }
        Some(at)
    }
}

/// Show what the controls bound in `node` (a row of `view`) are bound to now, in the states the
/// reconciler keeps under their keys in the window's tree.
fn bound_values(view: &ViewInstance, node: &UiNode, prefix: &str, reconciler: &Reconciler) {
    if node.binding.is_some() {
        let key = format!("{prefix}{}", node.key);
        match view.bound_value(&node.key) {
            Some(Value::Bool(flag)) => reconciler.set_flag(&key, flag),
            Some(Value::Int(number)) => reconciler.set_flag(&key, number != 0),
            Some(Value::Text(text)) => reconciler.set_text(&key, &text),
            _ => {}
        }
    }
    for child in &node.children {
        bound_values(view, child, prefix, reconciler);
    }
}

/// `node` with each key carrying `prefix`.
#[must_use]
pub fn prefixed(node: &UiNode, prefix: &str) -> UiNode {
    if prefix.is_empty() {
        return node.clone();
    }
    UiNode {
        kind: node.kind.clone(),
        key: format!("{prefix}{}", node.key),
        props: node.props.clone(),
        binding: node.binding.clone(),
        events: node.events.clone(),
        anchor: node.anchor.clone(),
        children: node
            .children
            .iter()
            .map(|child| prefixed(child, prefix))
            .collect(),
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

impl RowSource for HostRows {
    fn len(&self) -> usize {
        if self.read.get() {
            return self.keys.borrow().order.len();
        }
        self.view
            .try_borrow()
            .ok()
            .and_then(|view| view.list_len(&self.list))
            .unwrap_or(0)
    }

    fn key_at(&self, index: usize) -> Option<String> {
        if self.read.get() {
            return self.keys.borrow().order.get(index).map(ToString::to_string);
        }
        self.view.try_borrow().ok()?.row_key(&self.list, index)
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.every_key().index_of(key)
    }

    /// The view renders the rows in `range` it has not rendered yet, and forgets those it
    /// rendered outside it.
    fn request_range(&mut self, range: Range<usize>) {
        let Ok(mut view) = self.view.try_borrow_mut() else {
            return;
        };
        for patch in view.hide_rows(&self.list, range.clone()) {
            if let Patch::Remove { parent, .. } = patch
                && let Some(row) = parent.last()
            {
                self.keys.get_mut().shown.remove(row);
            }
        }
        for index in range {
            let key = if self.read.get() {
                self.keys
                    .get_mut()
                    .order
                    .get(index)
                    .map(ToString::to_string)
            } else {
                view.row_key(&self.list, index)
            };
            let Some(key) = key else {
                break;
            };
            if self.keys.get_mut().shown.contains(&key) {
                continue;
            }
            match view.row(&self.list, index) {
                Ok(Some(_)) => {
                    self.keys.get_mut().shown.insert(key);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "a row of the list did not render"),
            }
        }
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        let row = {
            let Ok(view) = self.view.try_borrow() else {
                return Ok(false);
            };
            let Some(row) = view.find(key) else {
                return Ok(false);
            };
            let item_kind = kind("ListItem");
            let Some(item) = find_kind(row, &item_kind) else {
                return Ok(false);
            };
            prefixed(item, &self.prefix)
        };
        let Some(description) = description_of(&row) else {
            return Ok(false);
        };
        {
            let Ok(mut reconciler) = self.reconciler.try_borrow_mut() else {
                return Ok(false);
            };
            let mut source = NodeSource::new(&row, description, &mut reconciler);
            read(&mut source)?;
        }
        // The row's controls hold the states their bindings keep now that it is read: show
        // what they are bound to.
        if let (Ok(view), Ok(reconciler)) = (self.view.try_borrow(), self.reconciler.try_borrow())
            && let Some(shown) = view.find(key)
        {
            bound_values(&view, shown, &self.prefix, &reconciler);
        }
        Ok(true)
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        self.every_key().key_presenting(id)
    }

    fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
        std::mem::take(&mut self.keys.get_mut().changes)
    }

    fn has_changes(&self) -> bool {
        !self.keys.borrow().changes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(keys: &[&str]) -> Keys {
        Keys {
            order: keys.iter().map(|key| Rc::from(*key)).collect(),
            ..Keys::default()
        }
    }

    fn node(key: &str) -> UiNode {
        UiNode::empty("fragment", key)
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
        let old = keys(&["a", "b", "c", "d"]);
        let mut rows = keys_of(&["a", "b", "c", "d"]);
        // Where each row is was looked up before the change: it is kept up to date.
        assert_eq!(rows.index_of("d"), Some(3));
        let parent = vec!["list".to_owned()];
        rows.apply(&[
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
        let changes = std::mem::take(&mut rows.changes);
        assert_eq!(replay(old, &new, &changes), new, "{changes:?}");
        for (at, key) in new.iter().enumerate() {
            assert_eq!(rows.index_of(key), Some(at), "{key}");
        }
        assert_eq!(rows.index_of("b"), None);
    }

    #[test]
    fn only_rendered_rows_are_built_again_and_a_row_is_found_by_its_resource() {
        let mut rows = keys_of(&["v.for0/doc/notes:a", "v.for0/doc/notes:b"]);
        rows.shown.insert("v.for0/doc/notes:a".to_owned());
        rows.changed([
            "v.for0/doc/notes:a".to_owned(),
            "v.for0/doc/notes:b".to_owned(),
        ]);
        assert_eq!(
            rows.changes,
            [ListModelChange::RowsChanged {
                keys: vec!["v.for0/doc/notes:a".to_owned()]
            }],
            "a row not rendered is read when it is"
        );
        assert_eq!(
            rows.key_presenting("doc/notes:b").as_deref(),
            Some("v.for0/doc/notes:b")
        );
        assert_eq!(rows.key_presenting("doc/notes:z"), None);
    }
}
