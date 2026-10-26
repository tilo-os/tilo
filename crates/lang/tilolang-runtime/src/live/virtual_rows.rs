//! Virtual lists: the rows of a `list` built when the host shows them, and absent until then.
//!
//! A `list` whose rows come from one loop holds, as its children in the tree, only the rows
//! built so far (shown, in order); its length and the key of each row are read from what the
//! rows are made of (a collection's listing, or a list of values) when the host asks. Opening a
//! list of 10,000 items builds no row, no reference and no node per item: it costs the rows on
//! screen. The host holds every row's key (`list_len`, `row_key`); the patch stream speaks of
//! the rows as before (`Insert` at a row's index, `Remove` and `Move` by key), and a row not
//! shown is, for the patches, an empty `fragment`.
//!
//! The rows follow their collection's steps as the loops of other containers do. When the
//! boundary holding the list runs again, the rows built that still stand are kept by key, and
//! if what the rows are made of changed (a filter or an order chosen, steps that did not fit)
//! the steps from the rows the host holds to the new ones are found by key.

use std::collections::HashMap;
use std::rc::Rc;

use super::{Bound, BoundId, Builder, Live, Pending};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::render::{RowSource, RowsOf, VirtualRows, item_key};
use crate::store::{Listing, Splice};
use crate::value::{CollRef, Value};
use crate::view::{Patch, RowStep, RowsChange, UiNode, row_steps};

/// The rows of a virtual list as the host holds them: what they are made of, as of the last
/// steps the host was given.
#[derive(Clone)]
pub(crate) enum Rows {
    Coll {
        coll: Rc<CollRef>,
        listing: Rc<Listing>,
    },
    List(Rc<[Value]>),
}

impl Rows {
    fn len(&self) -> usize {
        match self {
            Self::Coll { listing, .. } => listing.len(),
            Self::List(values) => values.len(),
        }
    }

    /// The key of row `index`, built from `template` without a reference to its item.
    fn key(&self, template: &RowsOf, index: usize) -> Option<String> {
        match self {
            Self::Coll { coll, listing } => {
                listing.get(index).map(|id| template.coll_row_key(coll, id))
            }
            Self::List(values) => values
                .get(index)
                .map(|item| template.key_of(&item_key(item, index))),
        }
    }

    /// The item of row `index`.
    fn item(&self, core: &Core, index: usize) -> Option<Value> {
        match self {
            Self::Coll { coll, listing } => listing.get(index).map(|id| core.item_ref(coll, id)),
            Self::List(values) => values.get(index).cloned(),
        }
    }

    /// Whether these are the same rows as `other`, item for item.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Coll {
                    coll: a,
                    listing: x,
                },
                Self::Coll {
                    coll: b,
                    listing: y,
                },
            ) => a == b && (Rc::ptr_eq(x, y) || x == y),
            (Self::List(a), Self::List(b)) => Rc::ptr_eq(a, b) || a == b,
            _ => false,
        }
    }

    /// Every row's key: what the steps between two versions of a list are found by.
    fn keys(&self, template: &RowsOf) -> Vec<String> {
        (0..self.len())
            .filter_map(|index| self.key(template, index))
            .collect()
    }
}

/// A virtual list: the boundary holding it, how its rows are made and from what, the rows built
/// (each with its index, in order: the list node's children) and how many rows the host holds.
pub(crate) struct Virtual {
    pub holder: BoundId,
    template: RowsOf,
    rows: Rows,
    built: Vec<(usize, BoundId)>,
    pub len: usize,
    /// The host's rows are no longer known: steps were given that did not fit. The host reads
    /// the list anew when its holder next runs.
    lost: bool,
}

impl Virtual {
    /// The key of row `index`.
    pub fn key(&self, index: usize) -> Option<String> {
        if index >= self.len {
            return None;
        }
        self.rows.key(&self.template, index)
    }
}

impl Live {
    /// The host's rows of the virtual list `list`, if it is one, are no longer known.
    pub(super) fn lose_rows(&mut self, list: &str) {
        if let Some(rows) = self.lists.get_mut(list) {
            rows.lost = true;
        }
    }

    /// Treat every list as holding all its rows, built: for a host that draws the whole tree,
    /// once it has shown them all.
    pub fn devirtualize(&mut self) {
        self.lists.clear();
    }

    /// Forget the virtual lists the boundary `id` held, and the row `id` was in one.
    pub(super) fn forget_rows_of(&mut self, id: BoundId) {
        self.lists.retain(|_, list| list.holder != id);
        for list in self.lists.values_mut() {
            list.built.retain(|(_, row)| *row != id);
        }
    }

    /// The path of the virtual list `list` in the tree.
    fn virtual_path(&self, list: &str) -> Option<Vec<usize>> {
        let holder = self.lists.get(list)?.holder;
        self.node_path_in(holder, list)
    }

    /// Point the paths of the rows built in `list` (at `at`) at their places among its
    /// children.
    fn place_built(&mut self, list: &str, at: &[usize]) {
        let built: Vec<BoundId> = self
            .lists
            .get(list)
            .map(|rows| rows.built.iter().map(|(_, id)| *id).collect())
            .unwrap_or_default();
        for (place, id) in built.into_iter().enumerate() {
            self.set_index(id, at.len(), place);
        }
    }

    /// Build row `index` of the virtual list `list` from `item`: a boundary of its holder, its
    /// `fragment` put among the rows built, rendered. Returns its key.
    fn build_row(
        &mut self,
        core: &Rc<Core>,
        list: &str,
        (index, item): (usize, Value),
    ) -> Result<Option<String>, RuntimeError> {
        let Some(at) = self.virtual_path(list) else {
            return Ok(None);
        };
        let Some(rows) = self.lists.get(list) else {
            return Ok(None);
        };
        let holder = rows.holder;
        let (key, args) = rows.template.row(&item, index);
        let fragment = rows.template.fragment(key.clone(), Vec::new());
        let place = rows.built.partition_point(|(other, _)| *other < index);
        let stub = rows.template.stub(&key, args);
        let mut bound = Bound::new(stub, true);
        bound.path = [at.as_slice(), &[place]].concat();
        let id = self.add_bound(bound);
        if let Some(b) = self.bound_mut(holder) {
            b.children.push(id);
        }
        if let Some(rows) = self.lists.get_mut(list) {
            rows.built.insert(place, (index, id));
        }
        match self.node_at_mut(&at) {
            Some(node) if place <= node.children.len() => node.children.insert(place, fragment),
            _ => return Ok(None),
        }
        self.place_built(list, &at);
        self.render_row(core, id)?;
        Ok(Some(key))
    }

    /// Row `index` of the virtual list `list`, built and rendered now if it was not.
    pub(super) fn show_virtual(
        &mut self,
        core: &Rc<Core>,
        list: &str,
        index: usize,
    ) -> Result<Option<String>, RuntimeError> {
        let Some(rows) = self.lists.get(list) else {
            return Ok(None);
        };
        if let Some((_, id)) = rows.built.iter().find(|(other, _)| *other == index) {
            return Ok(self.bound(*id).map(|b| b.key.clone()));
        }
        if index >= rows.len {
            return Ok(None);
        }
        let Some(item) = rows.rows.item(core, index) else {
            return Ok(None);
        };
        let key = self.build_row(core, list, (index, item));
        self.release(core);
        key
    }

    /// Forget the rows of the virtual list `list` built outside `keep`. Returns the patches
    /// that empty them: for the host, a row not shown is an empty `fragment`.
    pub(super) fn hide_virtual(
        &mut self,
        core: &Core,
        list: &str,
        keep: &std::ops::Range<usize>,
    ) -> Vec<Patch> {
        let Some(at) = self.virtual_path(list) else {
            return Vec::new();
        };
        let Some(rows) = self.lists.get(list) else {
            return Vec::new();
        };
        let holder = rows.holder;
        let gone: Vec<(usize, BoundId)> = rows
            .built
            .iter()
            .enumerate()
            .filter(|(_, (index, _))| !keep.contains(index))
            .map(|(place, (_, id))| (place, *id))
            .collect();
        let mut patches = Vec::new();
        // From the last, so each place is the one the removals before it left.
        for (place, id) in gone.into_iter().rev() {
            let path = [at.as_slice(), &[place]].concat();
            let keys = self.keys_at(&path);
            if let Some(node) = self.node_at_mut(&at)
                && place < node.children.len()
            {
                let fragment = node.children.remove(place);
                for child in fragment.children {
                    patches.push(Patch::Remove {
                        parent: keys.clone(),
                        key: child.key,
                    });
                }
            }
            self.drop_bound(id);
            if let Some(b) = self.bound_mut(holder) {
                b.children.retain(|child| *child != id);
            }
        }
        self.place_built(list, &at);
        self.release(core);
        patches
    }

    /// Apply one change's steps to the rows of a virtual list: every step is a patch for the
    /// host's rows, and only a row that is built moves in the tree. False when they do not fit.
    pub(super) fn splice_virtual(
        &mut self,
        core: &Rc<Core>,
        step: &Pending,
        patches: &mut Vec<Patch>,
    ) -> Result<bool, RuntimeError> {
        let list = step.rows.parent.as_str();
        let Some(at) = self.virtual_path(list) else {
            return Ok(false);
        };
        let parent = self.keys_at(&at);
        let mut splices = step.splices.iter().peekable();
        while let Some(splice) = splices.next() {
            let patch = match splice {
                // A removal and an insertion of the same item: the row moves, kept as it is.
                Splice::Remove { at: from, item }
                    if splices.peek().is_some_and(
                        |next| matches!(next, Splice::Insert { item: to, .. } if to == item),
                    ) =>
                {
                    let Some(Splice::Insert { at: to, .. }) = splices.next() else {
                        return Ok(false);
                    };
                    let key = step.rows.row_key(item);
                    if !self.move_built(list, &at, (*from, *to), &key) {
                        return Ok(false);
                    }
                    Patch::Move {
                        parent: parent.clone(),
                        key,
                        to: *to,
                    }
                }
                Splice::Remove { at: index, item } => {
                    let key = step.rows.row_key(item);
                    if !self.remove_built(list, &at, *index, &key) {
                        return Ok(false);
                    }
                    Patch::Remove {
                        parent: parent.clone(),
                        key,
                    }
                }
                Splice::Insert { at: index, item } => {
                    let key = step.rows.row_key(item);
                    let Some(rows) = self.lists.get_mut(list) else {
                        return Ok(false);
                    };
                    let taken = rows.built.iter().any(|(_, id)| {
                        self.bounds
                            .get(*id)
                            .and_then(Option::as_ref)
                            .is_some_and(|b| b.key == key)
                    });
                    if taken || *index > rows.len {
                        return Ok(false);
                    }
                    for (other, _) in &mut rows.built {
                        if *other >= *index {
                            *other = other.saturating_add(1);
                        }
                    }
                    rows.len = rows.len.saturating_add(1);
                    let fragment = rows.template.fragment(key.clone(), Vec::new());
                    let node = if self.lazy_rows {
                        fragment
                    } else {
                        let item = core.item_ref(&step.rows.coll, item);
                        self.build_row(core, list, (*index, item))?;
                        self.find(&key).cloned().unwrap_or(fragment)
                    };
                    Patch::Insert {
                        parent: parent.clone(),
                        index: *index,
                        node,
                    }
                }
            };
            patches.push(patch);
        }
        self.place_built(list, &at);
        Ok(true)
    }

    /// Move the row keyed `key` from index `from` to `to` among the rows of `list`, its node
    /// too if it is built. False when a row built at `from` is another.
    fn move_built(
        &mut self,
        list: &str,
        at: &[usize],
        (from, to): (usize, usize),
        key: &str,
    ) -> bool {
        let built = self.built_at(list, from);
        if let Some((_, id)) = built
            && self.bound(id).is_none_or(|b| b.key != key)
        {
            return false;
        }
        let Some(rows) = self.lists.get_mut(list) else {
            return false;
        };
        if from >= rows.len || to >= rows.len {
            return false;
        }
        let taken = built.map(|(place, id)| (rows.built.remove(place), place, id));
        for (other, _) in &mut rows.built {
            if *other > from {
                *other = other.saturating_sub(1);
            }
            if *other >= to {
                *other = other.saturating_add(1);
            }
        }
        let Some(((_, id), place, _)) = taken else {
            return true;
        };
        let new_place = rows.built.partition_point(|(other, _)| *other < to);
        rows.built.insert(new_place, (to, id));
        if let Some(node) = self.node_at_mut(at)
            && place < node.children.len()
        {
            let fragment = node.children.remove(place);
            node.children
                .insert(new_place.min(node.children.len()), fragment);
        }
        true
    }

    /// Remove the row keyed `key` at `index` from the rows of `list`, and its boundary if it is
    /// built. False when a row built there is another.
    fn remove_built(&mut self, list: &str, at: &[usize], index: usize, key: &str) -> bool {
        let built = self.built_at(list, index);
        if let Some((_, id)) = built
            && self.bound(id).is_none_or(|b| b.key != key)
        {
            return false;
        }
        let Some(rows) = self.lists.get_mut(list) else {
            return false;
        };
        if index >= rows.len {
            return false;
        }
        rows.len = rows.len.saturating_sub(1);
        let holder = rows.holder;
        if let Some((place, id)) = built {
            rows.built.remove(place);
            if let Some(node) = self.node_at_mut(at)
                && place < node.children.len()
            {
                node.children.remove(place);
            }
            self.drop_bound(id);
            if let Some(b) = self.bound_mut(holder) {
                b.children.retain(|child| *child != id);
            }
        }
        if let Some(rows) = self.lists.get_mut(list) {
            for (other, _) in &mut rows.built {
                if *other > index {
                    *other = other.saturating_sub(1);
                }
            }
        }
        true
    }

    /// The place among the rows built of `list` and the boundary of the row at `index`, if it
    /// is built.
    fn built_at(&self, list: &str, index: usize) -> Option<(usize, BoundId)> {
        self.lists.get(list).and_then(|rows| {
            rows.built
                .iter()
                .position(|(other, _)| *other == index)
                .and_then(|place| rows.built.get(place).map(|(_, id)| (place, *id)))
        })
    }

    /// Whether the rows of a virtual list over a collection are its items after their steps:
    /// as many, and each row built where its item is. They then stand for the listing as it is.
    pub(super) fn virtual_fits(&mut self, core: &Core, rows: &crate::render::LoopRows) -> bool {
        let Ok(listing) = core.coll_listing(&rows.coll) else {
            return false;
        };
        let Some(list) = self.lists.get(&rows.parent) else {
            return false;
        };
        let fits = list.len == listing.len()
            && list.built.iter().all(|(index, id)| {
                let key = listing.get(*index).map(|item| rows.row_key(item));
                self.bound(*id)
                    .is_some_and(|b| Some(&b.key) == key.as_ref())
            });
        if fits && let Some(list) = self.lists.get_mut(&rows.parent) {
            list.rows = Rows::Coll {
                coll: Rc::clone(&rows.coll),
                listing,
            };
        }
        fits
    }
}

impl Builder<'_> {
    /// The rows of a virtual list as the boundary holding it runs: those built before that still
    /// stand, rendered again if they changed, as the list's children. What became of the rows
    /// is kept for the diff.
    pub(super) fn build_virtual(&mut self, list: VirtualRows) -> Result<Vec<UiNode>, RuntimeError> {
        let VirtualRows {
            list: key,
            rows: template,
            source,
        } = list;
        let rows = match source {
            RowSource::Coll(coll) => {
                let listing = self
                    .core
                    .coll_listing(&coll)
                    .map_err(RuntimeError::Package)?;
                Rows::Coll { coll, listing }
            }
            RowSource::List(values) => Rows::List(values),
        };
        let before = self
            .live
            .lists
            .remove(&key)
            .filter(|old| old.holder == self.id && !old.lost);
        let Some(before) = before else {
            // A list new to the tree: the host reads its rows.
            self.live
                .row_changes
                .insert(key.clone(), RowsChange::Replace);
            let len = rows.len();
            self.live.lists.insert(
                key,
                Virtual {
                    holder: self.id,
                    template,
                    rows,
                    built: Vec::new(),
                    len,
                    lost: false,
                },
            );
            return Ok(Vec::new());
        };
        // Where each row built before is now, by key.
        let (steps, places): (Vec<RowStep>, Vec<(usize, usize)>) =
            if before.rows.same(&rows) && before.len == rows.len() {
                let kept = before
                    .built
                    .iter()
                    .map(|(index, _)| (*index, *index))
                    .collect();
                (Vec::new(), kept)
            } else {
                let old = before.rows.keys(&before.template);
                let new = rows.keys(&template);
                let at: HashMap<&str, usize> = new
                    .iter()
                    .enumerate()
                    .map(|(index, key)| (key.as_str(), index))
                    .collect();
                let kept = before
                    .built
                    .iter()
                    .filter_map(|(index, _)| {
                        let key = old.get(*index)?;
                        at.get(key.as_str()).map(|now| (*index, *now))
                    })
                    .collect();
                let fragment = |key: &str| template.fragment(key.to_owned(), Vec::new());
                (row_steps(&old, &new, &fragment), kept)
            };
        let mut built: Vec<(usize, BoundId)> = Vec::with_capacity(places.len());
        let mut children: Vec<(usize, UiNode)> = Vec::with_capacity(places.len());
        for (_, now) in places {
            let Some(item) = rows.item(self.core, now) else {
                continue;
            };
            let (row_key, args) = template.row(&item, now);
            let (id, content) = self.keep_row(template.stub(&row_key, args))?;
            built.push((now, id));
            children.push((now, template.fragment(row_key, content)));
        }
        built.sort_unstable_by_key(|(index, _)| *index);
        children.sort_unstable_by_key(|(index, _)| *index);
        self.live
            .row_changes
            .insert(key.clone(), RowsChange::Steps(steps));
        let len = rows.len();
        self.live.lists.insert(
            key,
            Virtual {
                holder: self.id,
                template,
                rows,
                built,
                len,
                lost: false,
            },
        );
        Ok(children.into_iter().map(|(_, node)| node).collect())
    }

    /// A row built before, kept: its subtree as it is when its view, arguments and reads are
    /// unchanged, else rendered again. Returns its boundary and the fragment's children.
    fn keep_row(
        &mut self,
        stub: crate::render::Stub<'_>,
    ) -> Result<(BoundId, Vec<UiNode>), RuntimeError> {
        if let Some(&id) = self.old.get(stub.key) {
            let same = self.live.bound(id).is_some_and(|b| {
                b.target.module == stub.target.module
                    && b.target.export == stub.target.export
                    && b.args == stub.args
            });
            if same {
                self.new.push(id);
                let fresh = self
                    .live
                    .bound(id)
                    .is_some_and(|b| b.rendered && !self.live.dirty.contains(&id));
                if fresh {
                    self.live.reused.insert(stub.key.to_owned());
                    return Ok((id, Vec::new()));
                }
                return Ok((id, vec![self.live.render(self.core, id)?]));
            }
            self.old.remove(stub.key);
            self.live.drop_bound(id);
        }
        let id = self.live.add_bound(Bound::new(stub, true));
        self.new.push(id);
        Ok((id, vec![self.live.render(self.core, id)?]))
    }
}
