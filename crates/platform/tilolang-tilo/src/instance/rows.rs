//! The rows of a view's virtual lists (`ui.VirtualList`, a `list` of any length), rendered as
//! the window's list shows them.
//!
//! The view's loop over a collection of any length renders a row only when the list asks for
//! it (the visible rows and the overscan) and forgets it when it scrolls away, so a list of ten
//! thousand notes costs the rows on screen. The view's changes reach the window as they are: a
//! row inserted, removed or moved is that change of the list, and a row the view changed is
//! sent again alone, when the window shows it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use tilo_render_protocol::patch::RowChange;

use crate::runtime::{Patch, UiNode, ViewInstance};

/// How many rows of a list are sent with it, before its window asks for any: the rows a
/// window shows first, so it shows them in its first frame.
pub const FIRST_ROWS: usize = 48;

/// The rows of one virtual list of a view.
pub struct ListRows {
    view: Rc<RefCell<ViewInstance>>,
    /// The key of the list node in the view.
    list: String,
    keys: Keys,
    /// Whether `keys` holds the key of every row. Until a change needs them, the rows' keys are
    /// read from the view as the window asks for them: a list of a hundred thousand notes opens
    /// reading the pages its rows are on, not all of them.
    read: bool,
}

/// The keys of the rows of a list, rendered or not (every one, once read), and which are
/// rendered.
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
}

impl ListRows {
    /// The rows of the list `list` of `view`, none rendered yet.
    #[must_use]
    pub fn of(view: &Rc<RefCell<ViewInstance>>, list: &str) -> Self {
        Self {
            view: Rc::clone(view),
            list: list.to_owned(),
            keys: Keys::default(),
            read: false,
        }
    }

    /// Whether these are the rows of `list` of `view`.
    #[must_use]
    pub fn reads(&self, view: &Rc<RefCell<ViewInstance>>, list: &str) -> bool {
        Rc::ptr_eq(&self.view, view) && self.list == list
    }

    /// How many rows the list has, rendered or not.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.read {
            return self.keys.order.len();
        }
        self.view
            .try_borrow()
            .ok()
            .and_then(|view| view.list_len(&self.list))
            .unwrap_or(0)
    }

    /// Whether the list has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The keys rendered now.
    #[must_use]
    pub fn shown(&self) -> &HashSet<String> {
        &self.keys.shown
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

    fn key_at(&self, view: &ViewInstance, index: usize) -> Option<String> {
        if self.read {
            return self.keys.order.get(index).map(ToString::to_string);
        }
        view.row_key(&self.list, index)
    }

    /// Render the rows of `range` not rendered yet and forget those rendered outside it: how
    /// many rows the list has, the keys from `range.start` on, and the rows rendered now.
    pub fn request(&mut self, range: Range<usize>) -> Requested {
        let mut keys = Vec::new();
        let mut rows = Vec::new();
        let len = self.len();
        if let Ok(mut view) = self.view.try_borrow_mut() {
            for patch in view.hide_rows(&self.list, range.clone()) {
                if let Patch::Remove { parent, .. } = patch
                    && let Some(row) = parent.last()
                {
                    self.keys.shown.remove(row);
                }
            }
            for index in range.clone() {
                let Some(key) = self.key_at(&view, index) else {
                    break;
                };
                keys.push(key.clone());
                if self.keys.shown.contains(&key) {
                    continue;
                }
                match view.row(&self.list, index) {
                    Ok(Some(row)) => {
                        rows.push(row.clone());
                        self.keys.shown.insert(key);
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "a row of the list did not render"),
                }
            }
        }
        Requested {
            len,
            start: range.start,
            keys,
            rows,
        }
    }

    /// The view replaced the list as a whole: its rows are read anew.
    pub fn reread(&mut self) -> Vec<RowChange> {
        self.keys = Keys::default();
        self.read = false;
        vec![RowChange::Reset]
    }

    /// Apply the view's changes of the list's own children, in order: rows inserted, removed
    /// and moved, each a change of the window's list against the order the ones before it left.
    pub fn apply(&mut self, patches: &[Patch]) -> Vec<RowChange> {
        if self.read {
            return self.keys.apply(patches);
        }
        let structural = patches.iter().filter(|patch| {
            matches!(
                patch,
                Patch::Insert { .. } | Patch::Remove { .. } | Patch::Move { .. }
            )
        });
        let inserted: Option<Vec<(usize, String)>> = structural
            .map(|patch| match patch {
                Patch::Insert { index, node, .. } => Some((*index, node.key.clone())),
                _ => None,
            })
            .collect();
        if let Some(inserted) = inserted {
            // Rows inserted say where: the keys need not be read for them.
            inserted
                .into_iter()
                .map(|(index, key)| RowChange::Inserted {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    key,
                })
                .collect()
        } else {
            // Where a removed or moved row was is in the order before the change, which was
            // never read: the window reads the rows anew, and the keys are read from now on.
            // The window drops the rows it holds, so each is sent again when it asks.
            let order = self.read_keys();
            self.keys = Keys {
                order,
                ..Keys::default()
            };
            self.read = true;
            vec![RowChange::Reset]
        }
    }

    /// The view changed what the rows of `keys` show: those rendered, to send again.
    #[must_use]
    pub fn changed(&self, keys: &[String]) -> Vec<UiNode> {
        let Ok(view) = self.view.try_borrow() else {
            return Vec::new();
        };
        keys.iter()
            .filter(|key| self.keys.shown.contains(*key))
            .filter_map(|key| view.find(key).cloned())
            .collect()
    }
}

/// What a request for rows read: how many rows the list has, the keys of those from `start`
/// on, and the rows rendered for it.
pub struct Requested {
    pub len: usize,
    pub start: usize,
    pub keys: Vec<String>,
    pub rows: Vec<UiNode>,
}

impl Keys {
    fn apply(&mut self, patches: &[Patch]) -> Vec<RowChange> {
        let mut changes = Vec::new();
        for patch in patches {
            let change = match patch {
                Patch::Insert { index, node, .. } => {
                    let index = (*index).min(self.order.len());
                    let key: Rc<str> = Rc::from(node.key.as_str());
                    self.order.insert(index, Rc::clone(&key));
                    self.found(key, index);
                    RowChange::Inserted {
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                        key: node.key.clone(),
                    }
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
                    RowChange::Removed {
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                    }
                }
                Patch::Move { key, to, .. } => {
                    let Some(from) = self.index_of(key) else {
                        continue;
                    };
                    let moved = self.order.remove(from);
                    let to = (*to).min(self.order.len());
                    self.order.insert(to, Rc::clone(&moved));
                    self.found(moved, to);
                    RowChange::Moved {
                        from: u32::try_from(from).unwrap_or(u32::MAX),
                        to: u32::try_from(to).unwrap_or(u32::MAX),
                    }
                }
                _ => continue,
            };
            changes.push(change);
        }
        changes
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

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(keys: &[&str]) -> Keys {
        Keys {
            order: keys.iter().map(|key| Rc::from(*key)).collect(),
            ..Keys::default()
        }
    }

    /// Replay `changes` on `keys`, as the window's list applies them.
    fn replay(mut keys: Vec<String>, changes: &[RowChange]) -> Vec<String> {
        for change in changes {
            match change {
                RowChange::Removed { index } => {
                    keys.remove(*index as usize);
                }
                RowChange::Inserted { index, key } => keys.insert(*index as usize, key.clone()),
                RowChange::Moved { from, to } => {
                    let key = keys.remove(*from as usize);
                    keys.insert(*to as usize, key);
                }
                RowChange::Changed { .. } | RowChange::Reset => {}
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
        let changes = rows.apply(&[
            Patch::Remove {
                parent: parent.clone(),
                key: "b".to_owned(),
            },
            Patch::Insert {
                parent: parent.clone(),
                index: 0,
                node: UiNode::empty("fragment", "n"),
            },
            Patch::Move {
                parent,
                key: "c".to_owned(),
                to: 1,
            },
        ]);
        let new = keys(&["n", "c", "a", "d"]);
        assert_eq!(replay(old, &changes), new, "{changes:?}");
        for (at, key) in new.iter().enumerate() {
            assert_eq!(rows.index_of(key), Some(at), "{key}");
        }
        assert_eq!(rows.index_of("b"), None);
    }
}
