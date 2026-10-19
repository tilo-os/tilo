//! Items keyed by an id they carry, with no stored order: [`Keyed`].
//!
//! A [`Collection`](crate::Collection) keeps an order key per item, which is
//! right for a list someone arranges. Many items are ordered by their own
//! content instead — clips by where they sit on a timeline, markers by
//! their time, tracks by their index — and an order key would be a second
//! truth that can disagree with it. `Keyed<T>` stores each item under the id
//! it carries ([`Identified::id`]), merges concurrent edits per field of each
//! item, and holds them in memory as a plain `Vec<T>` in id order (then
//! [`Identified::order`]), so code that works with a vector keeps working.
//!
//! Clones share the vector until one of them changes it: a copy of a state
//! holding items (a view keeping the state it was shown while the reader
//! follows) costs a pointer, and a change copies only the items of the field
//! it touched.
//!
//! A refresh keeps each item's id beside it, so bringing the items up to
//! date after a change asks only the items the change touched for their
//! ids: an id may be costly to make (formatted from a UUID), and finding and
//! sorting by asking every item again cost more than the change itself.
//!
//! A copy changed one item at a time ([`Items::find_mut`]) remembers which
//! items it handed out since it was copied, so the edits between it and the
//! items it was copied from ([`Model::diff`], what a surface that edited a
//! local copy submits) compare only those: an edit of one clip of 600 costs
//! one clip, not 600. Items added at the end ([`Items::push`]) are remembered
//! the same way, so cutting a clip in two costs the two halves. Saving a
//! [`Keyed`] copy over the items it was copied from writes only the items
//! changed and added, and those someone else touched meanwhile.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as Atomic};

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, Value};

use crate::action::{ActionCx, Refusal};
use crate::collection::Id;
use crate::cow::Shared;
use crate::delta::Delta;
use crate::model::LocatedAt as _;
use crate::model::{
    Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key, write_key_changed,
};
use crate::path::{Edit, EditOp, Path, Segment};

/// An item of a [`Keyed`] or an [`Ordered`](crate::Ordered): what it is
/// stored under, and how items sort in memory beyond that.
pub trait Identified {
    /// The item's identity: the key it is stored under.
    fn id(&self) -> Id;

    /// How two items sort in memory after their ids; the default keeps id
    /// order. A timeline sorts its markers by time here.
    fn order(&self, other: &Self) -> Ordering {
        let _ = other;
        Ordering::Equal
    }
}

/// Items that carry their own ids, held as a plain `Vec<T>` in memory and
/// stored the way `S` says: [`Keyed`] under their ids alone, in their own
/// order; [`Ordered`](crate::Ordered) as someone arranged them.
pub struct Items<T, S> {
    /// Shared between clones until one of them changes the items.
    stock: Shared<Stock<T>>,
    storage: PhantomData<fn() -> S>,
}

/// The items, the id of each once a refresh has needed them, and what they
/// were copied from.
struct Stock<T> {
    items: Vec<T>,
    /// `ids[i]` is `items[i].id()`, except at the positions in `stale`. Kept
    /// by the refreshes that read it, dropped whenever all the items are
    /// handed out to change: whoever changes an item may change its id.
    ids: Option<Vec<Arc<str>>>,
    /// Positions, ascending, whose kept id may be out of date: items handed
    /// out one at a time to change ([`Items::find_mut`]).
    stale: Vec<usize>,
    /// Which state of the items this is, a mark no other has had: made anew
    /// whenever they are handed out to change, so items copied from these
    /// can tell whether these are still as they were copied.
    mark: u64,
    /// The items these were copied from, and which of them changed since.
    since: Option<Since>,
}

/// Items still as the ones they were copied from, except at `changed` and
/// past their number.
struct Since {
    /// The [`Stock::mark`] of the items copied.
    copied: u64,
    /// Positions, ascending, handed out one at a time to change since the
    /// copy. Every item past the ones copied was added at the end
    /// ([`Items::push`]) under an id none held. Nothing else changed:
    /// handing out all the items, or any change that moves one, forgets the
    /// copy.
    changed: Vec<usize>,
}

/// What changed in items since they were copied ([`Stock::changed_since`]).
#[derive(Clone, Copy)]
struct Changes<'a> {
    /// Positions, ascending, of items that may have changed in place; some
    /// may be past `copied`.
    changed: &'a [usize],
    /// How many items were copied: the ones past them were added at the end.
    copied: usize,
}

/// A [`Stock::mark`] no other stock has had.
fn fresh_mark() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Atomic::Relaxed)
}

/// Add `at` to the ascending positions `positions`, once.
fn note(positions: &mut Vec<usize>, at: usize) {
    if let Err(slot) = positions.binary_search(&at) {
        positions.insert(slot, at);
    }
}

impl<T> Stock<T> {
    fn new(items: Vec<T>) -> Self {
        Self {
            items,
            ids: None,
            stale: Vec::new(),
            mark: fresh_mark(),
            since: None,
        }
    }

    /// All the items, to change: their ids, and what they were copied
    /// from, are no longer known.
    fn all_mut(&mut self) -> &mut Vec<T> {
        self.mark = fresh_mark();
        self.ids = None;
        self.stale.clear();
        self.since = None;
        &mut self.items
    }

    /// The item at `at`, to change; the copy remembers it changed.
    fn one_mut(&mut self, at: usize) -> Option<&mut T> {
        if at >= self.items.len() {
            return None;
        }
        self.mark = fresh_mark();
        if self.ids.is_some() {
            note(&mut self.stale, at);
        }
        if let Some(since) = &mut self.since {
            note(&mut since.changed, at);
        }
        self.items.get_mut(at)
    }

    /// What changed since these items were copied from `origin`, when they
    /// were and `origin` is still as it was then.
    fn changed_since(&self, origin: &Self) -> Option<Changes<'_>> {
        let since = self.since.as_ref()?;
        let still_as_copied = since.copied == origin.mark;
        (still_as_copied && self.items.len() >= origin.items.len()).then_some(Changes {
            changed: since.changed.as_slice(),
            copied: origin.items.len(),
        })
    }
}

impl<T: Identified> Stock<T> {
    /// The kept ids, brought up to date where an item handed out may have
    /// changed its id; taken, to be put back.
    fn take_ids(&mut self) -> Option<Vec<Arc<str>>> {
        let mut ids = self.ids.take()?;
        for at in self.stale.drain(..) {
            if let (Some(held), Some(item)) = (ids.get_mut(at), self.items.get(at)) {
                let id = item.id();
                if **held != *id.as_str() {
                    *held = Arc::from(id.as_str());
                }
            }
        }
        Some(ids)
    }
}

impl<T> Default for Stock<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<T: Clone> Clone for Stock<T> {
    /// The copy made to change items another holder still shares: the ids
    /// come along (a pointer each), and the copy remembers these items as
    /// what it was copied from.
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            ids: self.ids.clone(),
            stale: self.stale.clone(),
            mark: fresh_mark(),
            since: Some(Since {
                copied: self.mark,
                changed: Vec::new(),
            }),
        }
    }
}

/// The id of each of `items`, asked once.
fn ids_of<T: Identified>(items: &[T]) -> Vec<Arc<str>> {
    items
        .iter()
        .map(|item| Arc::from(item.id().as_str()))
        .collect()
}

/// How two items sort in memory, given their ids: by
/// [`Identified::order`], then by id.
fn in_order<T: Identified>(a: (&T, &str), b: (&T, &str)) -> Ordering {
    a.0.order(b.0).then_with(|| a.1.cmp(b.1))
}

/// Sort `items` and their `ids` alike, as a read from the document does: by
/// [`Identified::order`], then id. Stable, so this is a sort by id followed
/// by a stable sort by order; items already in place are not moved.
/// Whether any moved.
fn settle_with_ids<T: Identified>(items: &mut [T], ids: &mut [Arc<str>]) -> bool {
    let held = || items.iter().zip(ids.iter().map(|id| &**id));
    if held()
        .zip(held().skip(1))
        .all(|(a, b)| in_order(a, b).is_le())
    {
        return false;
    }
    let swaps = {
        let (held, held_ids): (&[T], &[Arc<str>]) = (items, ids);
        crate::sort::swaps_to_sort(held.len(), &mut |a, b| match (
            held.get(a).zip(held_ids.get(a)),
            held.get(b).zip(held_ids.get(b)),
        ) {
            (Some((a, a_id)), Some((b, b_id))) => in_order((a, &**a_id), (b, &**b_id)),
            _ => Ordering::Equal,
        })
    };
    for (a, b) in swaps {
        items.swap(a, b);
        ids.swap(a, b);
    }
    true
}

/// Items stored under the ids they carry. See the module docs.
pub type Keyed<T> = Items<T, ById>;

/// How [`Items`] are stored: the parts of their [`Model`] that differ.
pub trait ItemStorage: 'static {
    /// Whether the items keep an order someone arranged.
    const ARRANGED: bool;
    /// What a stored value that is not of this storage is called in errors.
    const NAME: &'static str;

    /// The items stored in the map `map`.
    ///
    /// # Errors
    /// Returns an item that does not read.
    fn load<T: Model + Identified>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError>;

    /// Write `items` into `slot`.
    ///
    /// # Errors
    /// Returns the document's write error.
    fn store<T: Model + Identified>(
        items: &[T],
        doc: &mut Doc,
        slot: Slot<'_>,
    ) -> Result<(), ModelError>;

    /// Bring `items` up to date after `delta`; the default reads them again.
    ///
    /// # Errors
    /// Returns an item that does not read.
    #[inline(never)]
    fn refresh<T: Model + Identified>(
        items: &mut Vec<T>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if delta.touches(map) {
            *items = Self::load(doc, map)?;
        }
        Ok(())
    }

    /// [`refresh`](Self::refresh), given the id of each item when known
    /// (`ids[i]` is `items[i].id()`) and leaving them known when it can.
    /// The default forgets them and refreshes.
    ///
    /// # Errors
    /// Returns an item that does not read; `ids` is then unknown.
    fn refresh_with_ids<T: Model + Identified>(
        items: &mut Vec<T>,
        ids: &mut Option<Vec<Arc<str>>>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        *ids = None;
        Self::refresh(items, doc, map, delta)
    }

    /// The edits that turn `before` into `after`, below `path`.
    fn diff<T: Model + Identified, R>(
        before: &[T],
        after: &[T],
        path: &[Segment],
        out: &mut Vec<Edit<R>>,
    );
}

/// [`Keyed`] storage: each item under its id, in memory by id and then
/// [`Identified::order`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ById;

impl<T: Clone, S> Clone for Items<T, S> {
    /// Shares the items, however many: the first of the clones to change
    /// them copies them.
    fn clone(&self) -> Self {
        Self {
            stock: self.stock.share(),
            storage: PhantomData,
        }
    }
}

impl<T: std::fmt::Debug, S> std::fmt::Debug for Items<T, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.items().fmt(f)
    }
}

impl<T: PartialEq, S> PartialEq for Items<T, S> {
    /// Items two clones still share are the same and not compared one by
    /// one.
    fn eq(&self, other: &Self) -> bool {
        self.stock.same(&other.stock) || self.items() == other.items()
    }
}

impl<T, S> Default for Items<T, S> {
    fn default() -> Self {
        Self::from(Vec::new())
    }
}

impl<T, S> Items<T, S> {
    fn items(&self) -> &Vec<T> {
        &self.stock.get().items
    }

    /// The items, to change: copied first when a clone still shares them.
    /// Their ids are forgotten, as the change may change one.
    fn items_mut(&mut self) -> &mut Vec<T> {
        self.stock.make_mut().all_mut()
    }
}

impl<T, S> Deref for Items<T, S> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        self.items()
    }
}

impl<T, S> DerefMut for Items<T, S> {
    /// The items, to change: copied first when a clone still shares them.
    fn deref_mut(&mut self) -> &mut Vec<T> {
        self.items_mut()
    }
}

impl<T, S> From<Vec<T>> for Items<T, S> {
    fn from(items: Vec<T>) -> Self {
        Self {
            stock: Shared::new(Stock::new(items)),
            storage: PhantomData,
        }
    }
}

impl<T, S> FromIterator<T> for Items<T, S> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl<T, S> IntoIterator for Items<T, S> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.stock.into_inner().items.into_iter()
    }
}

impl<'a, T, S> IntoIterator for &'a Items<T, S> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items().iter()
    }
}

impl<'a, T, S> IntoIterator for &'a mut Items<T, S> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items_mut().iter_mut()
    }
}

impl<T: serde::Serialize, S> serde::Serialize for Items<T, S> {
    fn serialize<Ser: serde::Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        self.items().serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>, S> serde::Deserialize<'de> for Items<T, S> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

impl<T: crate::JsonSchema, S> crate::JsonSchema for Items<T, S> {
    fn json_schema() -> serde_json::Value {
        crate::schema::array(T::json_schema())
    }
}

impl<T: Identified, S> Items<T, S> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the item `id` is.
    pub fn position_of(&self, id: &Id) -> Option<usize> {
        let stock = self.stock.get();
        match &stock.ids {
            Some(ids) if stock.stale.is_empty() => {
                ids.iter().position(|held| **held == *id.as_str())
            }
            // An item handed out to change is asked: its id may have changed.
            Some(ids) => ids.iter().enumerate().position(|(at, held)| {
                if stock.stale.binary_search(&at).is_ok() {
                    stock.items.get(at).is_some_and(|item| &item.id() == id)
                } else {
                    **held == *id.as_str()
                }
            }),
            None => stock.items.iter().position(|item| &item.id() == id),
        }
    }

    /// The item `id`.
    pub fn find(&self, id: &Id) -> Option<&T> {
        self.items().get(self.position_of(id)?)
    }

    /// The item `id`, to change. The items are copied first when a clone
    /// still shares them, and only when the item is there; the copy
    /// remembers which items changed, so comparing it with what it was
    /// copied from compares only those. The ids asked to find it are kept,
    /// so finding the next one asks none.
    pub fn find_mut(&mut self, id: &Id) -> Option<&mut T> {
        let learned = self.stock.get().ids.is_none().then(|| ids_of(self.items()));
        let at = match &learned {
            Some(ids) => ids.iter().position(|held| **held == *id.as_str())?,
            None => self.position_of(id)?,
        };
        let stock = self.stock.make_mut();
        if learned.is_some() {
            stock.ids = learned;
        }
        stock.one_mut(at)
    }

    /// Add `item` at the end, as `Vec::push` does. The items are copied
    /// first when a clone still shares them; the copy remembers the item
    /// came, so comparing it with what it was copied from, or saving it over
    /// that, visits the items changed and added rather than every item. An
    /// id the items already hold is no addition but items leaving and
    /// coming, which only comparing them all can tell: the copy forgets
    /// what it was copied from. The ids kept beside the items are kept.
    pub fn push(&mut self, item: T) {
        let stock = self.stock.make_mut();
        if stock.since.is_some() || stock.ids.is_some() {
            let id = item.id();
            if stock.since.is_some() {
                // The ids, learned when unknown, to tell an addition.
                let ids = stock.take_ids().unwrap_or_else(|| ids_of(&stock.items));
                if ids.iter().any(|held| **held == *id.as_str()) {
                    stock.since = None;
                }
                stock.ids = Some(ids);
            }
            if let Some(ids) = &mut stock.ids {
                ids.push(Arc::from(id.as_str()));
            }
        }
        stock.mark = fresh_mark();
        stock.items.push(item);
    }

    /// Remove the item `id`.
    pub fn take(&mut self, id: &Id) -> Option<T> {
        let at = self.position_of(id)?;
        Some(self.items_mut().remove(at))
    }
}

impl<T: Identified> Keyed<T> {
    /// Sort as a read from the document does: by id, then [`Identified::order`].
    pub fn settle(&mut self) {
        let stock = self.stock.make_mut();
        let mut ids = stock.take_ids().unwrap_or_else(|| ids_of(&stock.items));
        if settle_with_ids(&mut stock.items, &mut ids) {
            // Moved: no longer as they were copied, position by position.
            stock.mark = fresh_mark();
            stock.since = None;
        }
        stock.ids = Some(ids);
    }
}

impl<R, T: Model + Identified, S> Path<R, Items<T, S>> {
    /// The item `id`, its own paths rooted here.
    #[must_use]
    pub fn item(&self, id: &Id) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Item(id.clone())))
    }

    /// The path of the item `id` itself.
    #[must_use]
    pub fn item_path(&self, id: &Id) -> Path<R, T> {
        self.child(Segment::Item(id.clone()))
    }

    /// Remove the item `id`.
    #[must_use]
    pub fn remove(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveItem { id: id.clone() })
    }
}

impl<R, T: Model + Identified> Path<R, Keyed<T>> {
    /// The edits that make the item `after` what it is: added when `before`
    /// is `None`, else changed field by field from `before`.
    #[must_use]
    pub fn upsert(&self, before: Option<&T>, after: &T) -> Vec<Edit<R>> {
        match before {
            Some(before) => self.item_path(&after.id()).edits(before, after),
            None => vec![self.insert(after)],
        }
    }

    /// Add `item` under its own id.
    #[must_use]
    pub fn insert(&self, item: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: Some(item.id()),
            at: 0,
            value: item.to_json(),
        })
    }
}

impl ItemStorage for ById {
    const ARRANGED: bool = false;
    const NAME: &'static str = "keyed items";

    #[inline(never)]
    fn load<T: Model + Identified>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError> {
        let mut items = doc
            .keys(map)
            .map(|key| read_key::<T>(doc, map, &key))
            .collect::<Result<Vec<_>, _>>()?;
        crate::sort::sort_by(&mut items, T::order);
        Ok(items)
    }

    #[inline(never)]
    fn store<T: Model + Identified>(
        items: &[T],
        doc: &mut Doc,
        slot: Slot<'_>,
    ) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let keys: std::collections::HashSet<String> = items
            .iter()
            .map(|item| item.id().as_str().to_owned())
            .collect();
        let stale: Vec<String> = doc.keys(&map).filter(|key| !keys.contains(key)).collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        for item in items {
            let id = item.id();
            item.store(doc, Slot::Key(&map, id.as_str()))
                .located(id.as_str())?;
        }
        Ok(())
    }

    #[inline(never)]
    fn refresh<T: Model + Identified>(
        items: &mut Vec<T>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        Self::refresh_with_ids(items, &mut None, doc, map, delta)
    }

    /// Finds each touched item by the id kept beside it and asks only the
    /// items it read for their ids; sorts only when they are out of place.
    #[inline(never)]
    fn refresh_with_ids<T: Model + Identified>(
        items: &mut Vec<T>,
        known: &mut Option<Vec<Arc<str>>>,
        doc: &Doc,
        map: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let touched = delta.under(map);
        if touched.is_all() {
            *known = None;
            *items = Self::load(doc, map)?;
            *known = Some(ids_of(items));
            return Ok(());
        }
        let mut ids = known.take().unwrap_or_else(|| ids_of(items));
        let mut changed = false;
        for key in touched.keys() {
            let at = ids.iter().position(|id| **id == *key);
            match (doc.get(map, key)?, at) {
                (None, Some(at)) => {
                    items.remove(at);
                    ids.remove(at);
                    changed = true;
                }
                (None, None) => {}
                (Some(_), Some(at)) => {
                    if let (Some(item), Some(held)) = (items.get_mut(at), ids.get_mut(at)) {
                        refresh_key(item, doc, map, key, delta)?;
                        let id = item.id();
                        if **held != *id.as_str() {
                            *held = Arc::from(id.as_str());
                        }
                    }
                    changed = true;
                }
                (Some(_), None) => {
                    let item: T = read_key(doc, map, key)?;
                    ids.push(Arc::from(item.id().as_str()));
                    items.push(item);
                    changed = true;
                }
            }
        }
        if changed {
            settle_with_ids(items, &mut ids);
        }
        *known = Some(ids);
        Ok(())
    }

    #[inline(never)]
    fn diff<T: Model + Identified, R>(
        before: &[T],
        after: &[T],
        path: &[Segment],
        out: &mut Vec<Edit<R>>,
    ) {
        let after_ids: Vec<Id> = after.iter().map(Identified::id).collect();
        let before_ids = removed_and_changed(before, after, &after_ids, path, out);
        let held: HashSet<&Id> = before_ids.iter().collect();
        for (item, id) in after.iter().zip(after_ids) {
            if !held.contains(&id) {
                out.push(Edit::new(
                    path.to_vec(),
                    EditOp::InsertItem {
                        id: Some(id),
                        at: 0,
                        value: item.to_json(),
                    },
                ));
            }
        }
    }
}

/// [`Keyed`] editing: an item lives under its id, in no entry of its own.
fn edit_by_id(
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
    edits: &crate::collection::EntryEdits<'_>,
) -> Result<(), ModelError> {
    match (path.split_first(), op) {
        (Some((Segment::Item(id), rest)), _) => {
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_none() {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            }
            (edits.item)(doc, Slot::Key(&map, id.as_str()), rest, op, cx).located(id.as_str())
        }
        (None, EditOp::InsertItem { value, .. }) => {
            let (id, item) = (edits.parse)(value)?;
            let id = id.unwrap_or_else(|| cx.id());
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_some() {
                return Err(ModelError::invalid(format!("an item `{id}` exists")));
            }
            item.store_to(doc, Slot::Key(&map, id.as_str()))
                .located(id.as_str())
        }
        (None, EditOp::RemoveItem { id }) => {
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_none() {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            }
            doc.delete(&map, id.as_str())?;
            Ok(())
        }
        _ => (edits.whole)(doc, slot, path, op),
    }
}

/// The edits removing what `after` lacks and changing what both hold, given
/// the id of each item of `after`; returns the id of each item of `before`.
/// Each item is asked for its id once.
pub(crate) fn removed_and_changed<T: Model + Identified, R>(
    before: &[T],
    after: &[T],
    after_ids: &[Id],
    path: &[Segment],
    out: &mut Vec<Edit<R>>,
) -> Vec<Id> {
    let mut found: HashMap<&str, usize> = HashMap::with_capacity(after_ids.len());
    for (at, id) in after_ids.iter().enumerate() {
        found.entry(id.as_str()).or_insert(at);
    }
    let mut before_ids = Vec::with_capacity(before.len());
    for item in before {
        let id = item.id();
        match found.get(id.as_str()).and_then(|at| after.get(*at)) {
            Some(next) => {
                let mut at = path.to_vec();
                at.push(Segment::Item(id.clone()));
                item.diff(next, &at, out);
            }
            None => out.push(Edit::new(
                path.to_vec(),
                EditOp::RemoveItem { id: id.clone() },
            )),
        }
        before_ids.push(id);
    }
    before_ids
}

/// The items of `changes` that changed in place and kept their ids, by
/// position, with their id: `None` when one changed its id, as then items
/// left and came, which only comparing them all can tell.
fn kept_ids<T: Identified>(
    before: &[T],
    after: &[T],
    changes: Changes<'_>,
) -> Option<Vec<(usize, Id)>> {
    let mut kept = Vec::with_capacity(changes.changed.len());
    for &at in changes.changed {
        if at >= changes.copied {
            // Added since: written whole.
            break;
        }
        let id = before.get(at)?.id();
        if after.get(at)?.id() != id {
            return None;
        }
        kept.push((at, id));
    }
    Some(kept)
}

/// The edits between `before` and `after`, which holds the same items at
/// the same positions except as `changes` says: each item changed in place,
/// field by field, then each added one, in the order [`ItemStorage::diff`]
/// would list them (an [`Ordered`](crate::Ordered) item inserted at its
/// position, `arranged`). `false`, with nothing written, when one changed
/// its id.
fn diff_changed<T: Model + Identified, R>(
    (before, after): (&[T], &[T]),
    changes: Changes<'_>,
    arranged: bool,
    path: &[Segment],
    out: &mut Vec<Edit<R>>,
) -> bool {
    let Some(kept) = kept_ids(before, after, changes) else {
        return false;
    };
    let mut at = Vec::with_capacity(path.len().saturating_add(1));
    at.extend_from_slice(path);
    for (position, id) in kept {
        if let (Some(was), Some(now)) = (before.get(position), after.get(position)) {
            at.push(Segment::Item(id));
            was.diff(now, &at, out);
            at.pop();
        }
    }
    for (index, item) in after.iter().enumerate().skip(changes.copied) {
        out.push(Edit::new(
            path.to_vec(),
            EditOp::InsertItem {
                id: Some(item.id()),
                at: if arranged { index } else { 0 },
                value: item.to_json(),
            },
        ));
    }
    true
}

/// [`Model::store_changed`] of [`Keyed`] items `now` over the items `was`
/// they were copied from, changed and added to as `changes` says: the
/// operations [`ById::store`] writes, found without visiting the other
/// items. The map holds each of `was` under its id as a store left it,
/// except where `dirty` says someone else wrote since; an item no change
/// touched is the very item `was` holds there, and storing it writes
/// nothing. `items` finds an item by id.
fn store_by_id_changed<T: Model + Identified, S>(
    items: &Items<T, S>,
    (now, was): (&[T], &[T]),
    changes: Changes<'_>,
    (doc, slot): (&mut Doc, Slot<'_>),
    dirty: &Delta,
) -> Result<(), ModelError> {
    // A slot holding no map yet is written whole, as is a map someone else
    // changed otherwise than item by item, or items that left and came.
    let Some((Value::Object(ObjType::Map), map)) = slot.current(doc)? else {
        return ById::store(now, doc, slot);
    };
    let touched = dirty.under(&map);
    let kept = match kept_ids(was, now, changes) {
        Some(kept) if !touched.is_all() => kept,
        _ => return ById::store(now, doc, slot),
    };
    // What to write: the items changed and added, and those someone else
    // touched; what to delete: a key no item is under, which only someone
    // else can have written.
    let mut writes: Vec<usize> = kept.iter().map(|(at, _)| *at).collect();
    writes.extend(changes.copied..now.len());
    let mut stale: Vec<&str> = Vec::new();
    for key in touched.keys() {
        match items.position_of(&Id::from_raw(key)) {
            Some(at) => writes.push(at),
            None if doc.get(&map, key)?.is_some() => stale.push(key),
            None => {}
        }
    }
    // In the order `store` writes: deletes by key, then items in order.
    stale.sort_unstable();
    for key in stale {
        doc.delete(&map, key)?;
    }
    writes.sort_unstable();
    writes.dedup();
    for at in writes {
        let Some(item) = now.get(at) else {
            continue;
        };
        let id = item.id();
        match was.get(at).filter(|_| at < changes.copied) {
            Some(held) => write_key_changed(doc, &map, id.as_str(), item, held, dirty)?,
            None => item
                .store(doc, Slot::Key(&map, id.as_str()))
                .located(id.as_str())?,
        }
    }
    Ok(())
}

impl<T: Model + Identified, S> crate::items::ItemSource for Items<T, S> {
    fn item_collection(&self, field: &'static str) -> crate::items::ItemCollection {
        crate::collection::listed_items(T::TABLE, self, field)
    }
    fn item_presence(&self) -> usize {
        usize::from(!self.items().is_empty())
    }
    fn label_of_item(&self, id: &Id) -> Option<String> {
        let item = self.find(id)?;
        Some(crate::collection::label_of(T::TABLE, id, item))
    }
    fn item_texts(&self, field: &'static str, found: &mut Vec<crate::items::ItemText>) {
        crate::collection::item_texts(T::TABLE, self, field, found);
    }
    fn nested_items(&self, field: &'static str, out: &mut Vec<crate::items::ItemCollection>) {
        crate::collection::nested_items(T::TABLE, self, field, out);
    }
}

impl<T: Model + Identified, S: 'static> crate::table::PublishedItems for Items<T, S> {
    const ITEM_TABLE: &'static crate::table::ModelTable = T::TABLE;
}

impl<T: Model + Identified, S> crate::collection::ItemList for Items<T, S> {
    fn count(&self) -> usize {
        self.items().len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn std::any::Any)> {
        let item = self.items().get(at)?;
        Some((item.id(), item as &dyn std::any::Any))
    }
}

impl<T: Model + Identified, S: ItemStorage> Model for Items<T, S> {
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema()})
    }
    const SHAPE: crate::Shape = if S::ARRANGED {
        crate::Shape::Collection(&T::SHAPE)
    } else {
        crate::Shape::Keyed(&T::SHAPE)
    };
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();

    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.items().iter().map(Model::to_json).collect())
    }

    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Array(items) => items
                .iter()
                .map(T::from_json)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::from),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(ModelError::shape("a list of items")),
        }
    }

    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape(S::NAME));
        }
        S::load(doc, id).map(Self::from)
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        S::store(self.items(), doc, slot)
    }

    /// Items two clones still share are the same items, and store as they
    /// are: saving a record whose items no change touched visits none.
    fn stores_as(&self, before: &Self) -> bool {
        self.stock.same(&before.stock)
    }

    /// A [`Keyed`] copy changed one item at a time and added to writes
    /// only those items, and the ones someone else touched; anything else
    /// is written as [`store`](Model::store) writes it.
    #[inline(never)]
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let (now, was) = (self.stock.get(), before.stock.get());
        let changes = if self.stock.same(&before.stock) {
            Some(Changes {
                changed: &[],
                copied: was.items.len(),
            })
        } else {
            now.changed_since(was)
        };
        match changes {
            Some(changes) if !S::ARRANGED => {
                store_by_id_changed(self, (&now.items, &was.items), changes, (doc, slot), dirty)
            }
            _ => S::store(self.items(), doc, slot),
        }
    }

    #[inline(never)]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        // Untouched items stay shared with any clone of them.
        if !delta.touches(id) {
            return Ok(());
        }
        let stock = self.stock.make_mut();
        let mut ids = stock.take_ids();
        let refreshed = S::refresh_with_ids(stock.all_mut(), &mut ids, doc, id, delta);
        stock.ids = ids;
        refreshed
    }

    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let edits = crate::collection::EntryEdits {
            item: &T::edit,
            parse: &|value| {
                let item = T::from_json(value)?;
                item.check().map_err(ModelError::refused)?;
                Ok((
                    Some(item.id()),
                    Box::new(item) as Box<dyn crate::model::Stores + '_>,
                ))
            },
            whole: &edit_whole::<Self>,
            order_only: false,
        };
        if S::ARRANGED {
            crate::collection::edit_entries(doc, slot, path, op, cx, &edits)
        } else {
            edit_by_id(doc, slot, path, op, cx, &edits)
        }
    }

    /// Items two clones still share are not compared; a copy changed one
    /// item at a time compares only the items it changed.
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<Edit<R>>) {
        if self.stock.same(&after.stock) {
            return;
        }
        if let Some(changes) = after.stock.get().changed_since(self.stock.get())
            && diff_changed(
                (self.items(), after.items()),
                changes,
                S::ARRANGED,
                path,
                out,
            )
        {
            return;
        }
        S::diff(self.items(), after.items(), path, out);
    }

    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.items().iter().try_for_each(Model::check)?;
        }
        Ok(())
    }

    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        match path.split_first() {
            None => Some(self),
            Some((Segment::Item(id), rest)) => self.find(id)?.get_at(rest),
            Some(_) => None,
        }
    }

    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }

    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_continuable(rest, op),
            _ => None,
        }
    }

    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) if T::OPS => T::op_describe(rest, op),
            _ => None,
        }
    }

    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        match path.split_first() {
            Some((Segment::Item(id), rest)) if T::OPS => self.find(id)?.op_lands(rest, op),
            _ => None,
        }
    }

    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        if T::OPS {
            T::op_schemas(&format!("{prefix}*."), out);
        }
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        for item in self.items() {
            item.collect_text(out);
        }
    }
}
