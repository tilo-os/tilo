//! The rows of a window's virtual lists (`ui.VirtualList`, a `list` of any length), as the app
//! renders them for what each list shows. The list asks for a range ([`RowSource::request_range`]),
//! with the row held open into its item, and the app answers with the keys of those rows and
//! the rows it rendered for them, the open row's document among them; until they arrive a row is
//! not shown. The app sends each change of the rows as it happens, and a shown row that changed is
//! sent again alone. Each row is read through the one adapter, as the list's item component
//! (`ui.ListItem`).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;

use tilo_render_protocol::patch::RowChange;
use ui::collection_transition::ListModelChange;
use ui::component::{RowSource, Source};

use crate::adapter::{NodeSource, description_of};
use crate::reconcile::Reconciler;
use crate::shell::prefixed;
use crate::{Level, UiNode};

/// What the bound controls of the window show, by their keys in the composed tree: rows read later
/// show what theirs are bound to.
pub type BoundValues = Rc<RefCell<HashMap<String, crate::Value>>>;

pub struct RemoteRows {
    level: Level,
    list: String,
    /// The kind of the component a row is: the list's item, or a tree's node.
    item: String,
    /// The prefix the view's node keys carry in the window's tree (the detail's keys carry
    /// another).
    prefix: String,
    reconciler: Rc<RefCell<Reconciler>>,
    bound: BoundValues,
    /// The key of each row the app has named, by index; `None` for a row not asked for yet.
    keys: Vec<Option<Rc<str>>>,
    /// Where each known key was last found, built when first asked; kept up to date by looking
    /// around where a key was, as a change moves each other row by at most one place.
    index: RefCell<Option<HashMap<Rc<str>, usize>>>,
    /// The rows the app rendered for the range the list shows, by key.
    rows: HashMap<String, UiNode>,
    changes: Vec<ListModelChange<String>>,
    asked: Option<Asked>,
    /// `asked` is the range the app sent before it was asked: the first rows, with the patch that
    /// shows the list. A range the list shows inside it is answered already.
    sent: bool,
    /// Rows the list shows that the app is still to be asked for.
    wanted: Option<Asked>,
    /// The row whose menu the person opened, by key: the app sends it again with its menu.
    menu: Option<String>,
    /// The words of the heading over its rows (`ui.Card`, `ui.Section`, the window's title), where
    /// one heads them: a row's day fact that says the same day is the heading's.
    under: Option<String>,
    /// The app was asked for rows and has not answered: the list keeps taking frames until it does,
    /// so what it shows is never a frame behind what it asked for.
    awaiting: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub range: Range<usize>,
    /// The row held open into its item, by key: the app renders its document, and no closed
    /// row's.
    pub open: Option<String>,
    /// The row whose menu the person opened, by key: the app renders its menu, and no other
    /// row's.
    pub menu: Option<String>,
}

impl RemoteRows {
    #[must_use]
    pub fn new(
        level: Level,
        list: &str,
        item: &str,
        prefix: &str,
        reconciler: Rc<RefCell<Reconciler>>,
        bound: BoundValues,
    ) -> Self {
        Self {
            level,
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
            sent: false,
            wanted: None,
            menu: None,
            awaiting: false,
            under: None,
        }
    }

    pub fn under(&mut self, heading: Option<String>) {
        self.under = heading;
    }

    /// Whether the list shows a range the app is still to be asked for
    /// ([`crate::AppWindow::owes`]).
    #[must_use]
    pub fn wants(&self) -> bool {
        self.wanted.is_some()
    }

    pub fn take_wanted(&mut self) -> Option<(Level, String, Asked)> {
        let asked = self.wanted.take()?;
        self.asked = Some(asked.clone());
        self.sent = false;
        self.awaiting = true;
        Some((self.level, self.list.clone(), asked))
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
        let sent_keys = keys.len();
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
        // Rows that come unasked are the first ones the app sends with the list: they answer an ask
        // for any range inside them, so showing the list takes no second round.
        if self.asked.is_none() && self.wanted.is_none() && !arrived.is_empty() {
            self.asked = Some(Asked {
                range: start.min(len)..start.saturating_add(sent_keys).min(len),
                open: None,
                menu: self.menu.clone(),
            });
            self.sent = true;
        }
        arrived.sort();
        arrived.dedup();
        if !arrived.is_empty() {
            self.changes
                .push(ListModelChange::RowsChanged { keys: arrived });
        }
    }

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
                    self.sent = false;
                    self.awaiting = false;
                    ListModelChange::Reset
                }
            };
            self.changes.push(change);
        }
        if self.keys.len() != len {
            self.resize(len);
        }
        // An inserted row comes as little as its key: the list reads it once the app renders it, so
        // the range it shows is asked for again.
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

    /// The list has `len` rows now: the keys grow or go with it, and it hears the rows it has, so
    /// it settles where it now ends and asks for the range it shows.
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

    #[must_use]
    pub fn reads(&self, level: Level, list: &str) -> bool {
        self.level == level && self.list == list
    }

    #[must_use]
    pub fn rendered(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.rows.keys().cloned().collect();
        keys.sort();
        keys
    }

    #[must_use]
    pub fn level(&self) -> Level {
        self.level
    }

    /// The keys of the rows the list holds, in the order it lists them: the model's order, which
    /// the list draws, then a row it holds outside that order (one of a range it no longer shows),
    /// by key, so the order is the same in every process and at every size.
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

    #[must_use]
    pub fn row(&self, key: &str) -> Option<&UiNode> {
        self.rows.get(key)
    }

    /// Whether a rendered row holds a control that would take the other path now that the window
    /// offers the actions `after` where it offered `before`: each such row is named changed, so
    /// its list builds it again from the action it is bound to now.
    pub fn rebind(
        &mut self,
        before: &crate::adapter::Published,
        after: &crate::adapter::Adapter,
    ) -> bool {
        let changed: Vec<String> = self
            .rows
            .iter()
            .filter(|(_, row)| crate::window::rebind::rebinding(before, after, row).is_some())
            .map(|(key, _)| key.clone())
            .collect();
        if changed.is_empty() {
            return false;
        }
        self.changes
            .push(ListModelChange::RowsChanged { keys: changed });
        true
    }

    /// Show again what each rendered row's bound controls hold. A row a control of which was given
    /// a value other than the one it showed is a row that changed: the list builds it again, so it
    /// reads what it is bound to now.
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

pub fn lists(tree: &UiNode) -> Vec<(String, String, Option<String>)> {
    lists_under(tree, None)
}

pub fn lists_under(tree: &UiNode, under: Option<&str>) -> Vec<(String, String, Option<String>)> {
    fn walk(node: &UiNode, heading: Option<&str>, out: &mut Vec<(String, String, Option<String>)>) {
        let here = crate::date::heading_of(node)
            .or_else(|| crate::date::group_heading(node))
            .or_else(|| heading.map(str::to_owned));
        if tilo_render_protocol::patch::is_virtual_list(&node.kind) {
            let item = if node.kind == "ui.TreeView" {
                crate::adapter::kind("TreeViewNode")
            } else if node.kind == "ui.MediaGrid" {
                crate::adapter::kind("MediaCard")
            } else {
                crate::adapter::kind("ListItem")
            };
            out.push((node.key.clone(), item, here.clone()));
        }
        for child in &node.children {
            walk(child, here.as_deref(), out);
        }
    }
    let mut out = Vec::new();
    walk(tree, under, &mut out);
    out
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
    /// held outside the range are kept until the app answers, so a list that asks for more than it
    /// did a frame ago shows them meanwhile.
    fn request_range(&mut self, range: Range<usize>, open: Option<String>) {
        // The app renders what the list shows and forgets the rest: it hears of every range, and of
        // the row that opened or closed, whose document it renders or drops.
        let menu = self.menu.clone();
        let asked = Asked { range, open, menu };
        if self.sent
            && let Some(sent) = &self.asked
            && (sent.open.as_ref(), sent.menu.as_ref())
                == (asked.open.as_ref(), asked.menu.as_ref())
            && sent.range.start <= asked.range.start
            && asked.range.end.min(self.keys.len()) <= sent.range.end
        {
            return;
        }
        if self.asked.as_ref() != Some(&asked) {
            self.wanted = Some(asked);
        }
    }

    /// Ask the app for the row `key` again with its menu, the range and the open row as they
    /// were asked last.
    fn ask_menu(&mut self, key: &str) {
        self.menu = Some(key.to_owned());
        if let Some(asked) = self.wanted.as_ref().or(self.asked.as_ref()) {
            let asked = Asked {
                menu: self.menu.clone(),
                ..asked.clone()
            };
            if self.asked.as_ref() != Some(&asked) {
                self.wanted = Some(asked);
            }
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
            let Some(item) = row.first_of_kind(&self.item) else {
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
            let mut source =
                NodeSource::under_heading(&row, description, &mut reconciler, self.under.clone());
            read(&mut source)?;
        }
        if let (Some(shown), Ok(reconciler)) = (self.rows.get(key), self.reconciler.try_borrow()) {
            crate::window::show_bound(shown, &self.prefix, &self.bound.borrow(), &reconciler);
        }
        Ok(true)
    }

    fn read_props(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        // What its props say does not depend on the keys its nodes carry in the window's tree: the
        // row is read as the app sent it.
        let Some(item) = self
            .rows
            .get(key)
            .and_then(|row| row.first_of_kind(&self.item))
        else {
            return Ok(false);
        };
        let Some(description) = description_of(item) else {
            return Ok(false);
        };
        let Ok(mut reconciler) = self.reconciler.try_borrow_mut() else {
            return Ok(false);
        };
        read(&mut NodeSource::props_of(
            item,
            description,
            &mut reconciler,
            self.under.clone(),
        ))?;
        drop(reconciler);
        if let (Some(shown), Ok(reconciler)) = (self.rows.get(key), self.reconciler.try_borrow()) {
            crate::window::show_bound(shown, &self.prefix, &self.bound.borrow(), &reconciler);
        }
        Ok(true)
    }

    /// A row of a loop over resources is keyed by the loop and the resource's id (`<loop>/<id>`):
    /// the key of the row showing `id`, when the list holds it.
    fn key_presenting(&self, id: &str) -> Option<String> {
        let first = self.keys.iter().flatten().next()?;
        let (prefix, _) = first.split_once('/')?;
        let key = format!("{prefix}/{id}");
        self.index_of_key(&key).map(|_| key)
    }

    fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
        std::mem::take(&mut self.changes)
    }

    fn has_changes(&self) -> bool {
        !self.changes.is_empty() || self.wanted.is_some() || self.awaiting
    }
}
