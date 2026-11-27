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
use tilo_ui_base::collection_transition::ListModelChange;
use tilo_ui_base::{component::RowSource, component::Source};

use crate::adapter::{NodeSource, description_of};
use crate::reconcile::Reconciler;
use crate::shell::prefixed;
use crate::{Level, UiNode};

/// What the bound controls of the window show, by their keys in the composed tree: rows read later
/// show what theirs are bound to.
pub type BoundValues = Rc<RefCell<HashMap<String, crate::Value>>>;

/// The rows a window asks the app for at once, as many as the app sends with a list before it is
/// asked: a list asks for the range it shows out to whole pages of this many rows, so a list that
/// grows as it measures its rows, or that a scroll moves a few rows on, finds them in the page it
/// asked for and asks the app nothing more until it reaches past it.
const PAGE: usize = 48;

/// How many fewer rows than the app sent unasked a list holds before it tells the app so
/// ([`RemoteRows::request_range`]): a quarter of a page, more than the few rows past what a list
/// held that the app sends the next one.
const NARROWS_BY: usize = PAGE / 4;

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
    /// Each rendered row as it stands, by number ([`RowSource::revision`]): what reading it gives
    /// moves only with its node, the controls bound in it, or the heading over the list.
    revisions: HashMap<String, u64>,
    /// The number the next change of a row takes.
    revised: u64,
    changes: Vec<ListModelChange<String>>,
    asked: Option<Asked>,
    /// `asked` is the range the app sent before it was asked: the first rows, with the patch that
    /// shows the list. A range the list shows inside it is answered already.
    sent: bool,
    /// Rows the list shows that the app is still to be asked for.
    wanted: Option<Asked>,
    /// `wanted` tells the app the list holds fewer of the rows it sent unasked than it sent: no
    /// row the app has to render, only which it keeps.
    narrowing: bool,
    /// The row whose menu the person opened, by key: the app sends it again with its menu.
    menu: Option<String>,
    /// The words of the heading over its rows (`ui.Card`, `ui.Section`, the window's title), where
    /// one heads them: a row's day fact that says the same day is the heading's.
    under: Option<String>,
    /// The app was asked for rows and has not answered: the list keeps taking frames until it does,
    /// so what it shows is never a frame behind what it asked for.
    awaiting: bool,
    /// This store's identity, minted when it was made ([`Asked::store`]).
    store: u64,
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
    /// The identity of the store asking: minted when the store is made, so an app that rendered
    /// rows for a store the window has since replaced sends them all again for this one.
    pub store: u64,
}

/// The identity of the next row store a window makes: no two stores share one, so an app tells a
/// store it answered before from one it has not.
fn next_store() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STORES: AtomicU64 = AtomicU64::new(1);
    STORES.fetch_add(1, Ordering::Relaxed)
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
            revisions: HashMap::new(),
            revised: 0,
            changes: Vec::new(),
            asked: None,
            sent: false,
            wanted: None,
            narrowing: false,
            menu: None,
            awaiting: false,
            under: None,
            store: next_store(),
        }
    }

    pub fn under(&mut self, heading: Option<String>) {
        if self.under != heading {
            self.under = heading;
            // A row's day fact reads against the heading: every row reads anew.
            let keys: Vec<String> = self.rows.keys().cloned().collect();
            for key in keys {
                self.revise(&key);
            }
        }
    }

    /// What reading the row `key` gives may have changed.
    fn revise(&mut self, key: &str) {
        self.revised = self.revised.wrapping_add(1);
        self.revisions.insert(key.to_owned(), self.revised);
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
        self.narrowing = false;
        self.awaiting = true;
        Some((self.level, self.list.clone(), asked))
    }

    /// Whether a row the list names inside `range` is not rendered: the app renders a range, and
    /// a row it did not render stands as a placeholder until it does.
    fn unrendered_in(&self, range: &Range<usize>) -> bool {
        self.keys
            .get(range.start.min(self.keys.len())..range.end.min(self.keys.len()))
            .unwrap_or_default()
            .iter()
            .flatten()
            .any(|key| !self.rows.contains_key(&**key))
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
            let rows = &self.rows;
            self.revisions.retain(|key, _| rows.contains_key(key));
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
            self.revise(&row.key);
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
                store: self.store,
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
                        self.revisions.remove(&*key);
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
                    self.revise(&key);
                    self.rows.insert(key.clone(), crate::attach::hoist(&row));
                    ListModelChange::RowsChanged { keys: vec![key] }
                }
                RowChange::Reset => {
                    self.keys.clear();
                    self.rows.clear();
                    self.revisions.clear();
                    *self.index.get_mut() = None;
                    self.asked = None;
                    self.sent = false;
                    // What a list held of the rows read before is nothing to say of the new ones.
                    if self.narrowing {
                        self.wanted = None;
                    }
                    self.narrowing = false;
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
        if let Some(asked) = self.asked.clone()
            && self.unrendered_in(&asked.range)
        {
            self.wanted = Some(asked);
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
            .filter(|(_, row)| crate::window::rebind::rebinds(before, after, row))
            .map(|(key, _)| key.clone())
            .collect();
        if changed.is_empty() {
            return false;
        }
        for key in &changed {
            self.revise(key);
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
            for key in &changed {
                self.revise(key);
            }
            self.changes
                .push(ListModelChange::RowsChanged { keys: changed });
        }
    }

    /// Show what the row `key`'s bound controls hold: a control given another value than it
    /// showed makes the row read anew.
    fn show_bound(&mut self, key: &str) {
        let shown = match (self.rows.get(key), self.reconciler.try_borrow()) {
            (Some(row), Ok(reconciler)) => {
                crate::window::show_bound(row, &self.prefix, &self.bound.borrow(), &reconciler)
            }
            _ => false,
        };
        if shown {
            self.revise(key);
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
        // The app renders the pages the list shows and forgets the rest: it hears of a range that
        // reaches past the pages it was asked for, and of the row that opened or closed, whose
        // document it renders or drops. A range inside what it was asked for (or sent unasked) is
        // answered there.
        let menu = self.menu.clone();
        let len = self.keys.len();
        // A list whose length the app has not said yet asks for what it shows.
        let end = if len == 0 {
            range.end
        } else {
            range.end.min(len)
        };
        let covers = |asked: &Asked| {
            (asked.open.as_ref(), asked.menu.as_ref()) == (open.as_ref(), menu.as_ref())
                && asked.range.start <= range.start
                && end <= asked.range.end
        };
        // The first rows come unasked, as many as the app knows a list of the window holds and a
        // few more, or as a tall window shows when it knows none. A list that holds a good deal
        // fewer tells
        // the app the rows it holds: the app forgets the others, and the next list it shows at
        // this level comes with as many rows as this one holds, so a place switched to renders
        // the rows the window shows and none past them. A list a few rows shorter keeps them:
        // saying so would cost the app its surface published again, more than those rows did.
        if self.sent
            && (self.wanted.is_none() || self.narrowing)
            && let Some(sent) = self.asked.as_ref().filter(|sent| covers(sent))
        {
            let holds = range.start..end;
            if holds.start == sent.range.start
                && holds.end.saturating_add(NARROWS_BY) > sent.range.end
            {
                if self.narrowing {
                    self.wanted = None;
                    self.narrowing = false;
                }
            } else {
                self.wanted = Some(Asked {
                    range: holds,
                    open: open.clone(),
                    menu: menu.clone(),
                    store: self.store,
                });
                self.narrowing = true;
            }
            return;
        }
        if self
            .wanted
            .as_ref()
            .or(self.asked.as_ref())
            .is_some_and(covers)
        {
            return;
        }
        self.narrowing = false;
        let start = (range.start / PAGE).saturating_mul(PAGE);
        let paged = end.div_ceil(PAGE).saturating_mul(PAGE);
        let end = if len == 0 { paged } else { paged.min(len) }.max(end);
        self.wanted = Some(Asked {
            range: start..end,
            open,
            menu,
            store: self.store,
        });
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
        self.show_bound(key);
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
        self.show_bound(key);
        Ok(true)
    }

    fn revision(&self, key: &str) -> Option<u64> {
        self.revisions.get(key).copied()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::kind;
    use crate::{Binding, Value};

    fn rows() -> RemoteRows {
        let reconciler = Rc::new(RefCell::new(Reconciler::new(Rc::default())));
        RemoteRows::new(
            Level::Root,
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

    /// Rows are read in the order the list lists them, its model's order — the order a range
    /// arrived in — and never a hash's: the row a person meets first is read first.
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
        // A row the list holds outside the order it names now — one of a range it no longer shows,
        // kept until the app answers for the next — comes after the rows it lists, by key, so the
        // first row is still first.
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

    /// A row whose bound control changed is the row the list reads again; a row whose controls show
    /// what they did is left alone, so a change costs a row.
    #[test]
    fn a_bound_control_that_changed_reads_its_row_again() {
        let reconciler = Rc::new(RefCell::new(Reconciler::new(Rc::default())));
        let bound = BoundValues::default();
        let mut rows = RemoteRows::new(
            Level::Root,
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
            kind: Some("tasks.Task".into()),
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

    /// A row's day fact that says the day of the heading over its list is the heading's: what the
    /// list reads of the row leaves it out, and keeps the time of day it is due.
    #[test]
    fn a_row_under_the_heading_its_list_says_keeps_only_its_time() {
        let today = tilo_ui_base::clock::today();
        let day = today.strftime("%Y-%m-%d").to_string();
        let mut rows = RemoteRows::new(
            Level::Root,
            "list",
            &kind("ListItem"),
            "",
            Rc::new(RefCell::new(Reconciler::new(Rc::default()))),
            Rc::default(),
        );
        let at_nine = {
            let at = tilo_ui_base::clock::instant_of(jiff::civil::date(2026, 6, 30).at(9, 0, 0, 0))
                .expect("nine in the morning exists");
            format!("{at:.0}")
        };
        let mut item = UiNode::empty(&kind("ListItem"), "list/v/a");
        let mut row = UiNode::empty(&kind("ListRow"), "list/v/a/row");
        for (name, value) in [
            ("title", Value::text("Water the plants")),
            ("meta", Value::text(&day)),
            ("meta:as", Value::text("date")),
            ("meta:time", Value::text(&at_nine)),
        ] {
            row.props.insert(name.to_owned(), value);
        }
        item.children.push(row);
        rows.rows(1, 0, vec!["list/v/a".into()], vec![item]);
        let meta = |rows: &mut RemoteRows| {
            let mut meta = None;
            rows.read("list/v/a", &mut |source| {
                source.components("row", &mut |row| {
                    meta = row.prop("meta");
                    Ok(())
                })?;
                Ok(())
            })
            .expect("the row is read");
            meta
        };
        let said = |meta: Option<tilo_ui_base::component::Value>| match meta {
            Some(tilo_ui_base::component::Value::Text(text)) => text,
            other => panic!("the row states its day as words: {other:?}"),
        };
        assert_eq!(
            said(meta(&mut rows)),
            tilo_ui_base::format::day_fact_today(
                today,
                Some(jiff::civil::Time::new(9, 0, 0, 0).expect("nine"))
            )
            .text,
            "under no heading the row says the day it is due, with the time"
        );
        rows.under(Some(tilo_ui_base::format::day_fact_today(today, None).text));
        assert_eq!(
            said(meta(&mut rows)),
            tilo_ui_base::format::time(jiff::civil::Time::new(9, 0, 0, 0).expect("nine")),
            "the heading says the day: the row keeps the time of day it is due"
        );
    }

    /// A window that builds its list anew mints a store of its own, and asks for every row it
    /// shows: the app hears an identity no answer of it was rendered for, so it sends them all
    /// again rather than leaving a row to a store that is gone.
    #[test]
    fn a_list_built_anew_asks_with_a_store_of_its_own() {
        let mut first = rows();
        first.request_range(0..2, None);
        let (_, _, asked) = first.take_wanted().expect("the store asks for its range");
        let mut again = rows();
        again.request_range(0..2, None);
        let (_, _, rebuilt) = again
            .take_wanted()
            .expect("the list built anew asks for what it shows");
        assert_ne!(
            asked.store, rebuilt.store,
            "each store the window makes is its own"
        );
        assert_eq!(
            rebuilt.range,
            0..2,
            "the new store asks for the range it shows"
        );
        assert!(
            again.row("v/a").is_none(),
            "a store the window made anew holds no row of the one before"
        );
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
                Level::Root,
                "list".to_owned(),
                Asked {
                    range: 0..PAGE,
                    open: None,
                    menu: None,
                    store: rows.store,
                }
            )),
            "the page the range is on is asked for"
        );
        rows.request_range(0..20, None);
        assert_eq!(rows.take_wanted(), None, "asked once");
        rows.request_range(4..PAGE, None);
        assert_eq!(
            rows.take_wanted(),
            None,
            "a list that grows or moves within the page it asked for asks nothing more"
        );
        assert!(!rows.wants());
        assert_eq!(rows.key_at(5), None, "a row not named yet");
    }

    /// The app sends a list's first rows with the patch that shows it: a range inside them is
    /// answered, so the list asks nothing, and a range past them is asked for.
    #[test]
    fn rows_sent_unasked_answer_the_range_they_cover() {
        let mut rows = rows();
        let keys = (0..48).map(|i| format!("v/{i}")).collect();
        rows.rows(200, 0, keys, Vec::new());
        rows.request_range(0..40, None);
        assert!(!rows.wants(), "the rows it shows came with the list");
        rows.request_range(30..60, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.range),
            Some(0..2 * PAGE),
            "rows past the ones sent are asked for, by the pages they are on"
        );
        rows.request_range(0..20, Some("v/4".to_owned()));
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.open),
            Some(Some("v/4".to_owned())),
            "the row the person opens is asked for"
        );
    }

    /// A list holding a good deal fewer of the rows sent with it than were sent tells the app the
    /// rows it holds, rendering nothing: the app forgets the others, and sends the next list as
    /// many. Holding more again asks for the page they are on.
    #[test]
    fn a_list_holding_fewer_rows_than_were_sent_says_which_it_holds() {
        let mut rows = rows();
        let keys = (0..48).map(|i| format!("v/{i}")).collect();
        rows.rows(200, 0, keys, Vec::new());
        rows.request_range(0..20, None);
        rows.request_range(0..30, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.range),
            Some(0..30),
            "the list says the rows it holds once, as it holds them last"
        );
        rows.rows(
            200,
            0,
            (0..30).map(|i| format!("v/{i}")).collect(),
            Vec::new(),
        );
        assert_eq!(rows.take_wanted(), None, "said once");
        rows.request_range(0..40, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked.range),
            Some(0..PAGE),
            "rows past the ones it holds are asked for by their page"
        );
    }

    /// The app names a list shorter than the one the window holds: the list hears it, so it settles
    /// where it now ends and asks for the rows it shows rather than staying empty.
    #[test]
    fn a_list_the_app_names_shorter_is_a_change_of_its_rows() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        let _ = rows.take_changes();
        rows.request_range(90..100, None);
        assert!(rows.take_wanted().is_some(), "asked for the range it shows");
        // The app reads a row past the list's end: the range it shows holds nothing, and the list
        // it names is the one it holds.
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

    /// The row held open into its item is the one whose document the app renders: the list asks
    /// again when it opens, in or out of the range it shows, and once for each.
    #[test]
    fn the_row_that_opens_is_asked_for_with_the_range() {
        let mut rows = rows();
        rows.rows(100, 0, vec!["v/0".into()], Vec::new());
        rows.request_range(0..20, None);
        assert_eq!(
            rows.take_wanted().map(|(_, _, asked)| asked),
            Some(Asked {
                range: 0..PAGE,
                open: None,
                menu: None,
                store: rows.store,
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
