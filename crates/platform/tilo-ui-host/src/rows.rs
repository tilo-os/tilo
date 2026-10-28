//! The rows of a window's virtual lists (`ui.VirtualList`, a `list` of any length), as the app
//! renders them for what each list shows.
//!
//! The app renders a row only when the list shows it (the visible rows and the overscan): the
//! list asks for a range ([`RowSource::request_range`]), the window sends the app that range,
//! and the app answers with the keys of those rows and the rows it rendered for them. Until
//! they arrive a row is not shown, as a row scrolled in before it is built. The app sends
//! each change of the rows as it happens: a row inserted, removed or moved is that change of
//! the list, and a shown row that changed is sent again alone. Each row is read through the
//! one adapter, as the list's item component (`ui.ListItem`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use tilo_render_protocol::patch::RowChange;
use ui::component::{RowSource, Source};
use ui::widget::virtual_list::ListModelChange;

use crate::adapter::{NodeSource, description_of, kind};
use crate::reconcile::Reconciler;
use crate::shell::prefixed;
use crate::{Lane, UiNode};

/// What the bound controls of the window show, by their keys in the composed tree: rows read
/// later show what theirs are bound to from here.
pub type BoundValues = Rc<RefCell<HashMap<String, crate::Value>>>;

/// The rows of one virtual list of a window.
pub struct RemoteRows {
    lane: Lane,
    /// The key of the list node in its view.
    list: String,
    /// What the keys of the view's nodes carry in the window's tree (the detail's carry one).
    prefix: String,
    /// Reads the rows as the window's other components are read, reporting to the same place.
    reconciler: Rc<RefCell<Reconciler>>,
    bound: BoundValues,
    /// The key of each row the app has named, by index; `None` for a row not asked for yet.
    keys: Vec<Option<Rc<str>>>,
    /// Where each known key was last found, built when first asked; kept up to date by
    /// looking around where a key was, as a change moves each other row by one place at most.
    index: RefCell<Option<HashMap<Rc<str>, usize>>>,
    /// The rows the app rendered for the range the list shows, by key.
    rows: HashMap<String, UiNode>,
    changes: Vec<ListModelChange<String>>,
    /// The range the app was last asked for.
    asked: Option<Range<usize>>,
    /// A range the list shows that the app is still to be asked for.
    wanted: Option<Range<usize>>,
    /// The app was asked for rows and has not answered: the list keeps taking frames until it
    /// does, so what it shows is never a frame behind what it asked for.
    awaiting: bool,
}

impl RemoteRows {
    /// The rows of the list `list` of the view of `lane`, whose nodes carry `prefix` in the
    /// window's tree, none known yet.
    #[must_use]
    pub fn new(
        lane: Lane,
        list: &str,
        prefix: &str,
        reconciler: Rc<RefCell<Reconciler>>,
        bound: BoundValues,
    ) -> Self {
        Self {
            lane,
            list: list.to_owned(),
            prefix: prefix.to_owned(),
            reconciler,
            bound,
            keys: Vec::new(),
            index: RefCell::default(),
            rows: HashMap::new(),
            changes: Vec::new(),
            asked: None,
            wanted: None,
            awaiting: false,
        }
    }

    /// The range the list shows that the app is still to be asked for, taken.
    pub fn take_wanted(&mut self) -> Option<(Lane, String, Range<usize>)> {
        let range = self.wanted.take()?;
        self.asked = Some(range.clone());
        self.awaiting = true;
        Some((self.lane, self.list.clone(), range))
    }

    /// The app's rows: the list has `len`, the keys from `start` on are `keys`, and `rows` are
    /// rendered.
    pub fn rows(&mut self, len: usize, start: usize, keys: Vec<String>, rows: Vec<UiNode>) {
        self.awaiting = false;
        self.resize(len);
        // The app forgot the rows outside the range it answers for: so does the window.
        if let Some(asked) = &self.asked {
            let keep: std::collections::HashSet<Rc<str>> = self
                .keys
                .get(asked.start.min(self.keys.len())..asked.end.min(self.keys.len()))
                .unwrap_or_default()
                .iter()
                .flatten()
                .cloned()
                .collect();
            self.rows.retain(|key, _| keep.contains(key.as_str()));
        }
        let mut arrived = Vec::new();
        for (offset, key) in keys.into_iter().enumerate() {
            let Some(index) = start.checked_add(offset).filter(|index| *index < len) else {
                break;
            };
            let key: Rc<str> = Rc::from(key);
            if let Some(slot) = self.keys.get_mut(index)
                && slot.as_deref() != Some(&*key)
            {
                *slot = Some(Rc::clone(&key));
                arrived.push(key.to_string());
                if let Some(found) = self.index.get_mut().as_mut() {
                    found.insert(key, index);
                }
            }
        }
        for row in rows {
            arrived.push(row.key.clone());
            self.rows.insert(row.key.clone(), row);
        }
        arrived.sort();
        arrived.dedup();
        if !arrived.is_empty() {
            self.changes
                .push(ListModelChange::RowsChanged { keys: arrived });
        }
    }

    /// The app's changes of the rows, in order; the list has `len` rows after them.
    pub fn changed(&mut self, len: usize, changes: Vec<RowChange>) {
        for change in changes {
            let change = match change {
                RowChange::Inserted { index, key } => {
                    let index = (index as usize).min(self.keys.len());
                    let key: Rc<str> = Rc::from(key);
                    self.keys.insert(index, Some(Rc::clone(&key)));
                    *self.index.get_mut() = None;
                    ListModelChange::RowsInserted { index, count: 1 }
                }
                RowChange::Removed { index } => {
                    let index = index as usize;
                    if index >= self.keys.len() {
                        continue;
                    }
                    if let Some(key) = self.keys.remove(index) {
                        self.rows.remove(&*key);
                    }
                    *self.index.get_mut() = None;
                    ListModelChange::RowsRemoved { index, count: 1 }
                }
                RowChange::Moved { from, to } => {
                    let (from, to) = (from as usize, to as usize);
                    if from >= self.keys.len() {
                        continue;
                    }
                    let moved = self.keys.remove(from);
                    let to = to.min(self.keys.len());
                    self.keys.insert(to, moved);
                    *self.index.get_mut() = None;
                    ListModelChange::RowsMoved { from, to, count: 1 }
                }
                RowChange::Changed { row } => {
                    let key = row.key.clone();
                    self.rows.insert(key.clone(), row);
                    ListModelChange::RowsChanged { keys: vec![key] }
                }
                RowChange::Reset => {
                    self.keys.clear();
                    self.rows.clear();
                    *self.index.get_mut() = None;
                    self.asked = None;
                    self.awaiting = false;
                    ListModelChange::Reset
                }
            };
            self.changes.push(change);
        }
        if self.keys.len() != len {
            self.resize(len);
        }
    }

    fn resize(&mut self, len: usize) {
        if self.keys.len() != len {
            self.keys.resize(len, None);
            *self.index.get_mut() = None;
        }
    }

    /// Whether these are the rows of `list` of the view of `lane`.
    #[must_use]
    pub fn reads(&self, lane: Lane, list: &str) -> bool {
        self.lane == lane && self.list == list
    }

    /// The keys of the rows rendered now.
    #[must_use]
    pub fn rendered(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.rows.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// The rendered row `key`, as the app sent it.
    #[must_use]
    pub fn row(&self, key: &str) -> Option<&UiNode> {
        self.rows.get(key)
    }

    /// Show again what each rendered row's bound controls hold.
    pub fn refresh_bound(&self) {
        let Ok(reconciler) = self.reconciler.try_borrow() else {
            return;
        };
        let bound = self.bound.borrow();
        for row in self.rows.values() {
            crate::window::show_bound(row, &self.prefix, &bound, &reconciler);
        }
    }

    fn index_of_key(&self, key: &str) -> Option<usize> {
        let mut index = self.index.borrow_mut();
        let found = index.get_or_insert_with(|| {
            self.keys
                .iter()
                .enumerate()
                .filter_map(|(at, key)| key.as_ref().map(|key| (Rc::clone(key), at)))
                .collect()
        });
        let (stored, was) = found.get_key_value(key)?;
        let (stored, was) = (Rc::clone(stored), *was);
        // Out from where it was, one place each way at a time.
        let is_it = |at: usize| self.keys.get(at).is_some_and(|k| k.as_deref() == Some(key));
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

impl RowSource for RemoteRows {
    fn len(&self) -> usize {
        self.keys.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.keys.get(index)?.as_deref().map(str::to_owned)
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.index_of_key(key)
    }

    /// Ask the app for the rows of `range`. The rows held outside it are kept until the app
    /// answers, so a list that asks for more than it did a frame ago shows them meanwhile.
    fn request_range(&mut self, range: Range<usize>) {
        // The app renders what the list shows and forgets the rest: it hears of every range.
        if self.asked.as_ref() != Some(&range) {
            self.wanted = Some(range);
        }
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        let row = {
            let Some(row) = self.rows.get(key) else {
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
        if let (Some(shown), Ok(reconciler)) = (self.rows.get(key), self.reconciler.try_borrow()) {
            crate::window::show_bound(shown, &self.prefix, &self.bound.borrow(), &reconciler);
        }
        Ok(true)
    }

    /// A row of a loop over resources is keyed by the loop and the resource's id
    /// (`<loop>/<id>`): the key of the row showing `id`, when the list holds it.
    fn key_presenting(&self, id: &str) -> Option<String> {
        let first = self.keys.iter().flatten().next()?;
        let (prefix, _) = first.split_once('/')?;
        let key = format!("{prefix}/{id}");
        self.index_of_key(&key).map(|_| key)
    }

    fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
        std::mem::take(&mut self.changes)
    }

    /// Changes wait, or rows the list asked for are still to come.
    fn has_changes(&self) -> bool {
        !self.changes.is_empty() || self.wanted.is_some() || self.awaiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> RemoteRows {
        let reconciler = Rc::new(RefCell::new(Reconciler::new(Rc::default())));
        RemoteRows::new(Lane::Root, "list", "", reconciler, Rc::default())
    }

    #[test]
    fn the_apps_rows_and_changes_are_the_lists() {
        let mut rows = rows();
        rows.rows(
            4,
            0,
            vec!["v/a".into(), "v/b".into(), "v/c".into(), "v/d".into()],
            Vec::new(),
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.index_of("v/d"), Some(3));
        rows.changed(
            4,
            vec![
                RowChange::Removed { index: 1 },
                RowChange::Inserted {
                    index: 0,
                    key: "v/n".into(),
                },
                RowChange::Moved { from: 2, to: 1 },
            ],
        );
        let keys: Vec<String> = (0..rows.len()).filter_map(|i| rows.key_at(i)).collect();
        assert_eq!(keys, ["v/n", "v/c", "v/a", "v/d"]);
        assert_eq!(rows.key_presenting("b"), None);
        assert_eq!(rows.key_presenting("c").as_deref(), Some("v/c"));
        let changes = rows.take_changes();
        assert!(matches!(
            changes.last(),
            Some(ListModelChange::RowsMoved { from: 2, to: 1, .. })
        ));
    }

    #[test]
    fn a_range_not_sent_is_asked_for_once() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        rows.request_range(0..20);
        assert_eq!(
            rows.take_wanted(),
            Some((Lane::Root, "list".to_owned(), 0..20))
        );
        rows.request_range(0..20);
        assert_eq!(rows.take_wanted(), None, "asked once");
        assert_eq!(rows.key_at(5), None, "a row not named yet");
    }
}
