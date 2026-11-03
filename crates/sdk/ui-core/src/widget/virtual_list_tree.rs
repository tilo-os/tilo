//! How a [`VirtualList`]'s rows stand as a tree ([`VirtualList::with_tree`]): which row each
//! stands under, how deep it stands, and which rows a folded row hides.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;

use vello::kurbo::Rect;

use super::{ListModel, VirtualList};

/// How far a tree's rows stand in from their list for each row over them ([`VirtualList::with_tree`]):
/// the `s4` step, so a list indents as far as the outline beside it.
pub const TREE_INDENT: f64 = crate::tokens::Spacing::BASE.s4 as f64;
/// The room a tree row's disclosure takes at its leading edge: a regular icon's square, the same
/// room the column keeps for a row with no subtasks to fold away.
pub const TREE_DISCLOSURE: f64 = crate::tokens::Metrics::BASE.icon_md as f64;
/// How far up a row's chain of parents the list follows it: deeper than anyone nests a row, so
/// a chain that loops still ends.
const TREE_DEPTH_GUARD: u32 = 64;

/// What a row is in a tree of rows: the row of the item it is a subtask of, when that item is
/// one of the list's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeRow<K> {
    /// The row of the item this row is a subtask of, when that item is a row of the list.
    pub parent: Option<K>,
}

/// What a tree list works out of its rows, for the rows it builds: how deep each stands, which
/// of them are folded away, which have subtasks, and which stand under the row they follow. The
/// list fills it as it mounts rows; a row's builder reads it, and the person's folding stays
/// with the list, so a row folded away stays folded while its neighbours come and go.
#[derive(Debug)]
pub struct TreeMarks<K> {
    depth: std::cell::RefCell<HashMap<K, u32>>,
    folded: std::cell::RefCell<HashSet<K>>,
    branches: std::cell::RefCell<HashSet<K>>,
    nested: std::cell::RefCell<HashSet<K>>,
}

impl<K> Default for TreeMarks<K> {
    fn default() -> Self {
        Self {
            depth: std::cell::RefCell::default(),
            folded: std::cell::RefCell::default(),
            branches: std::cell::RefCell::default(),
            nested: std::cell::RefCell::default(),
        }
    }
}

impl<K: Clone + Eq + Hash> TreeMarks<K> {
    /// How many rows under its top row `key` stands.
    pub fn depth(&self, key: &K) -> u32 {
        self.depth.borrow().get(key).copied().unwrap_or(0)
    }

    /// Whether the person folded the row's subtasks away.
    pub fn is_folded(&self, key: &K) -> bool {
        self.folded.borrow().contains(key)
    }

    /// Whether the row has subtasks, a disclosure to show.
    pub fn is_branch(&self, key: &K) -> bool {
        self.branches.borrow().contains(key)
    }

    /// Whether the row stands under the row it follows, rather than away from the parent it
    /// names: a list keeps the order its filter gave it, and a subtask the sort put elsewhere
    /// says whose it is instead of standing indented under nothing.
    pub fn is_nested(&self, key: &K) -> bool {
        self.nested.borrow().contains(key)
    }

    /// Fold the row's subtasks away, or show them again.
    pub fn fold(&self, key: &K, folded: bool) {
        let mut folded_set = self.folded.borrow_mut();
        if folded {
            folded_set.insert(key.clone());
        } else {
            folded_set.remove(key);
        }
    }
}

impl<M: ListModel> VirtualList<M> {
    /// The rows stand as a tree: each under the row of the item it names as its parent, indented
    /// by the rows over it, with a disclosure on the rows that have subtasks, folding them away
    /// where the person folds them. `marks` is what the rows the list builds read, filled as it
    /// mounts them ([`TreeMarks`]).
    #[must_use]
    pub fn with_tree(mut self, marks: Rc<TreeMarks<M::Key>>) -> Self {
        self.tree = Some(marks);
        self.tree_rows.clear();
        self.tree_hidden.clear();
        self.visible_window_changed();
        self
    }

    /// Fold the row's subtasks away, or show them again, as its disclosure, its menu or the
    /// keyboard does. Whether the row is a tree row with subtasks.
    pub fn fold_row(&mut self, key: &M::Key, folded: bool) -> bool {
        let Some(marks) = self.tree.clone() else {
            return false;
        };
        if !marks.is_branch(key) || marks.is_folded(key) == folded {
            return false;
        }
        marks.fold(key, folded);
        // The row draws the disclosure the other way round.
        self.invalidated.insert(key.clone());
        self.visible_window_changed();
        true
    }

    /// Whether the row folds its subtasks away now.
    #[must_use]
    pub fn row_folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    /// What the tree's rows are in `range`: what stands under what, how deep each row stands,
    /// and which rows a folded one hides. A tree's rows come in order: a row stands under the
    /// row it follows when it names it, so the walk reads each row of the range once, and only
    /// the rows in view.
    pub(super) fn shape_tree(&mut self, range: &Range<usize>) {
        if self.tree.is_none() {
            return;
        }
        // Rows rebuilt since the last pass are asked again.
        let rebuilt: Vec<M::Key> = self.invalidated.iter().cloned().collect();
        for key in rebuilt {
            self.tree_rows.remove(&key);
        }
        let keys: Vec<M::Key> = range
            .clone()
            .filter_map(|index| self.model.key_at(index))
            .collect();
        // The rows over the row being read, outermost first, each with how deep it stands and
        // whether a row over it hides it.
        let mut over: Vec<(M::Key, u32, bool)> = Vec::new();
        let mut depth: HashMap<M::Key, u32> = HashMap::new();
        let mut hidden: HashSet<M::Key> = HashSet::new();
        let mut branches: Vec<M::Key> = Vec::new();
        let mut nested: Vec<M::Key> = Vec::new();
        for key in &keys {
            let parent = self.tree_facts(key).and_then(|facts| facts.parent);
            let at = parent
                .as_ref()
                .and_then(|parent| over.iter().rposition(|(row, _, _)| row == parent));
            // The row it follows is the row it names, or a row over that one: it stands under
            // its parent, as deep as its parent plus one.
            let beneath = at.zip(parent.clone());
            let (found, away) = if let Some((at, parent)) = beneath {
                let (_, depth, hidden) = over[at];
                branches.push(parent.clone());
                nested.push(key.clone());
                over.truncate(at + 1);
                (depth + 1, hidden || self.folded(&parent))
            } else {
                // The list holds its order, not a tree: a row whose parent the sort put
                // elsewhere stands on its own, and names the parent instead.
                over.clear();
                (0, false)
            };
            let found = found.min(TREE_DEPTH_GUARD);
            depth.insert(key.clone(), found);
            if away {
                hidden.insert(key.clone());
            }
            over.push((key.clone(), found, away));
        }
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().extend(depth);
            marks.branches.borrow_mut().extend(branches);
            marks.nested.borrow_mut().extend(nested);
        }
        self.tree_hidden = hidden;
    }

    /// What the model says row `key` is in a tree, read once while its props stand as they
    /// were.
    fn tree_facts(&mut self, key: &M::Key) -> Option<TreeRow<M::Key>> {
        if let Some(facts) = self.tree_rows.get(key) {
            return Some(facts.clone());
        }
        let index = self.model.index_of(key)?;
        let facts = self.model.tree_at(index)?;
        self.tree_rows.insert(key.clone(), facts.clone());
        Some(facts)
    }

    /// Whether the person folded the row's subtasks away.
    fn folded(&self, key: &M::Key) -> bool {
        self.tree.as_ref().is_some_and(|marks| marks.is_folded(key))
    }

    /// The tree as the model gives it changed: its rows are read again.
    pub(super) fn forget_tree(&mut self) {
        self.tree_rows.clear();
        self.tree_hidden.clear();
        if let Some(marks) = &self.tree {
            marks.depth.borrow_mut().clear();
            marks.branches.borrow_mut().clear();
            marks.nested.borrow_mut().clear();
        }
    }

    /// Whether the pointer is over the disclosure at the leading edge of the row at `index`:
    /// the room the row indents by, then the column that folds its subtasks.
    fn disclosure_at(&self, index: usize, key: &M::Key) -> bool {
        let Some(cursor) = self.last_cursor else {
            return false;
        };
        let Some(marks) = self.tree.as_ref() else {
            return false;
        };
        if !marks.is_branch(key) {
            return false;
        }
        let Some(top) = self.viewport.item_view_top(index) else {
            return false;
        };
        let Some(height) = self.viewport.item_height(index) else {
            return false;
        };
        let x0 = self.rect.x0 + f64::from(marks.depth(key)) * TREE_INDENT;
        // The rows stand where the layout put them, moved by the scroll since, as a press is
        // read (`Self::row_at`).
        let y0 = self.rect.y0 + self.end_slack() + top;
        let rect = Rect::new(x0, y0, x0 + TREE_DISCLOSURE, y0 + height);
        rect.contains(cursor)
    }

    /// A completed click on the row of `key` at `index`: its disclosure folds what is under
    /// it, and anywhere else the click is the row's.
    pub(super) fn press_row_at(
        &mut self,
        index: usize,
        key: M::Key,
        modifiers: winit::event::Modifiers,
    ) {
        if self.disclosure_at(index, &key) {
            let folded = self.row_folded(&key);
            self.fold_row(&key, !folded);
            return;
        }
        self.click_row(key, modifiers);
    }

    /// As [`Self::press_row_at`], for a row found by its key.
    pub(super) fn press_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        if let Some(index) = self.model.index_of(&key) {
            self.press_row_at(index, key, modifiers);
        }
    }
}
