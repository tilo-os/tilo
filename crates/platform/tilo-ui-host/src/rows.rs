//! The rows of a window's virtual lists (`ui.VirtualList`, a `list` of any length), as the app
//! renders them for what each list shows.
//!
//! The app renders a row only when the list shows it (the visible rows and the overscan): the
//! list asks for a range ([`RowSource::request_range`]), and which row stands open into its
//! item where it stands, the window sends the app both, and the app answers with the keys of
//! those rows and the rows it rendered for them — the open row's document among them, and no
//! closed row's. Until they arrive a row is not shown, as a row scrolled in before it is built.
//! The app sends
//! each change of the rows as it happens: a row inserted, removed or moved is that change of
//! the list, and a shown row that changed is sent again alone. Each row is read through the
//! one adapter, as the list's item component (`ui.ListItem`).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;

use tilo_render_protocol::patch::RowChange;
use ui::component::{RowSource, Source};
use ui::widget::virtual_list::ListModelChange;

use crate::adapter::{NodeSource, description_of};
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
    /// The kind of the component a row is: the list's item, or a tree's node.
    item: String,
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
    /// The rows the app was last asked for: the range the list shows, and the row standing open
    /// into its item where it stands.
    asked: Option<Asked>,
    /// Rows the list shows that the app is still to be asked for.
    wanted: Option<Asked>,
    /// The app was asked for rows and has not answered: the list keeps taking frames until it
    /// does, so what it shows is never a frame behind what it asked for.
    awaiting: bool,
}

/// What the list asked the app for: the rows it shows, and the row standing open into its item
/// where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub range: Range<usize>,
    /// The row standing open into its item, by key, when one does: the app renders its
    /// document, and no closed row's.
    pub open: Option<String>,
}

impl RemoteRows {
    /// The rows of the list `list` of the view of `lane`, whose nodes carry `prefix` in the
    /// window's tree, none known yet.
    #[must_use]
    pub fn new(
        lane: Lane,
        list: &str,
        item: &str,
        prefix: &str,
        reconciler: Rc<RefCell<Reconciler>>,
        bound: BoundValues,
    ) -> Self {
        Self {
            lane,
            list: list.to_owned(),
            item: item.to_owned(),
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

    /// Whether the list shows a range the app is still to be asked for: the window owes the
    /// app that ask ([`crate::AppWindow::owes`]).
    #[must_use]
    pub fn wants(&self) -> bool {
        self.wanted.is_some()
    }

    /// The rows the list shows that the app is still to be asked for, taken.
    pub fn take_wanted(&mut self) -> Option<(Lane, String, Asked)> {
        let asked = self.wanted.take()?;
        self.asked = Some(asked.clone());
        self.awaiting = true;
        Some((self.lane, self.list.clone(), asked))
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
                .get(asked.range.start.min(self.keys.len())..asked.range.end.min(self.keys.len()))
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
            self.rows
                .insert(row.key.clone(), crate::attach::hoist(&row));
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
                    self.rows.insert(key.clone(), crate::attach::hoist(&row));
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
        // A row the app inserted comes as little as its key: the list reads it once the app
        // renders it, so the range it shows is asked for again.
        if let Some(asked) = self.asked.clone() {
            let held = self
                .keys
                .get(asked.range.start.min(self.keys.len())..asked.range.end.min(self.keys.len()))
                .unwrap_or_default()
                .iter()
                .flatten()
                .any(|key| !self.rows.contains_key(&**key));
            if held {
                self.wanted = Some(asked);
            }
        }
    }

    /// The list has `len` rows now: the keys grow or go with it, and the list hears it as the
    /// rows it is, so it settles where it now stands and asks for the range it shows.
    fn resize(&mut self, len: usize) {
        let was = self.keys.len();
        if was == len {
            return;
        }
        if len > was {
            self.keys.resize(len, None);
            self.changes.push(ListModelChange::RowsInserted {
                index: was,
                count: len.saturating_sub(was),
            });
        } else {
            self.keys.truncate(len);
            self.changes.push(ListModelChange::RowsRemoved {
                index: len,
                count: was.saturating_sub(len),
            });
        }
        *self.index.get_mut() = None;
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

    /// The lane of the window whose view holds this list.
    #[must_use]
    pub fn lane(&self) -> Lane {
        self.lane
    }

    /// The keys of the rows the list holds, in the order it lists them: the model's own order,
    /// which the list draws, and a row it holds outside that order (one of a range it no longer
    /// shows) after them, by key, so the order is the same in every process and at every size.
    #[must_use]
    pub fn in_order(&self) -> Vec<String> {
        let mut keys: Vec<String> = (0..RowSource::len(self))
            .filter_map(|index| RowSource::key_at(self, index))
            .filter(|key| self.rows.contains_key(key))
            .collect();
        let listed: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        let mut rest: Vec<String> = self
            .rows
            .keys()
            .filter(|key| !listed.contains(key.as_str()))
            .cloned()
            .collect();
        rest.sort();
        keys.extend(rest);
        keys
    }

    /// The rendered row `key`, as the app sent it.
    #[must_use]
    pub fn row(&self, key: &str) -> Option<&UiNode> {
        self.rows.get(key)
    }

    /// Show again what each rendered row's bound controls hold. A row a control of which was
    /// given a value other than the one it showed is a row that changed: the list builds it
    /// again, so it reads what it is bound to now.
    pub fn refresh_bound(&mut self) {
        let Ok(reconciler) = self.reconciler.try_borrow() else {
            return;
        };
        let bound = self.bound.borrow();
        let mut changed = Vec::new();
        for (key, row) in &self.rows {
            if crate::window::show_bound(row, &self.prefix, &bound, &reconciler) {
                changed.push(key.clone());
            }
        }
        drop(bound);
        drop(reconciler);
        if !changed.is_empty() {
            changed.sort();
            self.changes
                .push(ListModelChange::RowsChanged { keys: changed });
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
pub(crate) fn find_kind<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
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

    /// Ask the app for the rows of `range`, and for the document of the row at `open`. The rows
    /// held outside the range are kept until the app answers, so a list that asks for more than
    /// it did a frame ago shows them meanwhile.
    fn request_range(&mut self, range: Range<usize>, open: Option<String>) {
        // The app renders what the list shows and forgets the rest: it hears of every range, and
        // of the row that opened or closed, whose document it renders or drops.
        let asked = Asked { range, open };
        if self.asked.as_ref() != Some(&asked) {
            self.wanted = Some(asked);
        }
    }

    fn rendered(&self, key: &str) -> bool {
        self.rows.contains_key(key)
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
            let Some(item) = find_kind(row, &self.item) else {
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
    use crate::adapter::kind;
    use crate::{Binding, Value};

    fn rows() -> RemoteRows {
        let reconciler = Rc::new(RefCell::new(Reconciler::new(Rc::default())));
        RemoteRows::new(
            Lane::Root,
            "list",
            &kind("ListItem"),
            "",
            reconciler,
            Rc::default(),
        )
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

    /// The rows a list holds are read in the order the list lists them, which is its model's
    /// order — the keys a range arrived in — and never the order a hash of them happens to
    /// give: what is read first is the row a person meets first, in this process and the next.
    #[test]
    fn the_rows_are_read_in_the_order_the_list_lists_them() {
        let mut rows = rows();
        let shown = vec!["v/c".to_owned(), "v/a".to_owned(), "v/b".to_owned()];
        let rendered = shown
            .iter()
            .map(|key| UiNode::empty(&kind("ListItem"), key))
            .collect();
        rows.rows(3, 0, shown.clone(), rendered);
        assert_eq!(rows.in_order(), shown, "the model's order, key for key");
        // A row the list holds outside the order it names now — a row of a range it no longer
        // shows, kept until the app answers for the next — comes after the rows it lists, by
        // key, so it never stands in front of the first row.
        rows.rows(
            3,
            0,
            Vec::new(),
            vec![
                UiNode::empty(&kind("ListItem"), "v/z"),
                UiNode::empty(&kind("ListItem"), "v/y"),
            ],
        );
        assert_eq!(
            rows.in_order(),
            ["v/c", "v/a", "v/b", "v/y", "v/z"],
            "the held rows follow the listed ones"
        );
    }

    /// A row whose bound control changed is the row the list reads again; a row whose controls
    /// show what they did is left alone, so a change of a row costs a row.
    #[test]
    fn a_bound_control_that_changed_reads_its_row_again() {
        let reconciler = Rc::new(RefCell::new(Reconciler::new(Rc::default())));
        let bound = BoundValues::default();
        let mut rows = RemoteRows::new(
            Lane::Root,
            "list",
            &kind("ListItem"),
            "",
            Rc::clone(&reconciler),
            Rc::clone(&bound),
        );
        let mut row = UiNode::empty(&kind("ListItem"), "list/v/a");
        let mut checked = UiNode::empty(&kind("ListRow"), "list/v/a/row");
        checked.binding = Some(Binding {
            field: Some(("v/a".into(), "done".into())),
            name: "done".into(),
        });
        row.children.push(checked);
        reconciler.borrow_mut().flag_state("list/v/a/row");
        rows.rows(1, 0, vec!["list/v/a".into()], vec![row]);
        rows.take_changes();

        rows.refresh_bound();
        assert!(rows.take_changes().is_empty(), "nothing changed yet");

        bound
            .borrow_mut()
            .insert("list/v/a/row".to_owned(), Value::Bool(true));
        rows.refresh_bound();
        let changes = rows.take_changes();
        assert!(
            matches!(
                changes.as_slice(),
                [ListModelChange::RowsChanged { keys }] if keys == &["list/v/a".to_owned()]
            ),
            "the row that reads checked is read again: {changes:?}"
        );
        rows.refresh_bound();
        assert!(rows.take_changes().is_empty(), "read once");
    }

    #[test]
    fn a_range_not_sent_is_asked_for_once() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        rows.request_range(0..20, None);
        assert!(rows.wants(), "the window owes the app the ask");
        assert_eq!(
            rows.take_wanted(),
            Some((
                Lane::Root,
                "list".to_owned(),
                Asked {
                    range: 0..20,
                    open: None,
                }
            ))
        );
        rows.request_range(0..20, None);
        assert_eq!(rows.take_wanted(), None, "asked once");
        assert!(!rows.wants());
        assert_eq!(rows.key_at(5), None, "a row not named yet");
    }

    /// The app names a list shorter than the one the window holds: the list hears it, so it
    /// settles where it now ends and asks for the rows it shows rather than staying empty.
    #[test]
    fn a_list_the_app_names_shorter_is_a_change_of_its_rows() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        let _ = rows.take_changes();
        rows.request_range(90..100, None);
        assert!(rows.take_wanted().is_some(), "asked for the range it shows");
        // The app reads a row past the list's end: the range it shows holds nothing, and the
        // list it names is the one it holds.
        rows.rows(50, 90, Vec::new(), Vec::new());
        assert_eq!(rows.len(), 50, "the list is as long as the app names it");
        assert_eq!(
            rows.take_changes(),
            [ListModelChange::RowsRemoved {
                index: 50,
                count: 50
            }],
            "the list hears that its last rows went"
        );
    }

    /// The row standing open into its item is the one whose document the app renders: the list
    /// asks again when it opens, before or within the range it shows, and once for each.
    #[test]
    fn the_row_that_opens_is_asked_for_with_the_range() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        rows.request_range(0..20, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked),
            Some(Asked {
                range: 0..20,
                open: None,
            })
        );
        rows.request_range(0..20, Some("v/4".to_owned()));
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.open),
            Some(Some("v/4".to_owned())),
            "the row the person opened is asked for once"
        );
        rows.request_range(0..20, Some("v/4".to_owned()));
        assert_eq!(rows.take_wanted(), None, "asked once");
        rows.request_range(0..20, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.open),
            Some(None),
            "the row that closed takes its document with it"
        );
    }
}
