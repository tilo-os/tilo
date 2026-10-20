//! Items with identity: [`Id`] and the ordered, keyed [`Collection`].

use std::fmt;
use tilo_sort::CompactSort as _;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use crate::action::ActionCx;
use crate::delta::{Delta, Touch};
use crate::model::LocatedAt as _;
use crate::model::{Doc, Model, ModelError, Slot, Stores, read_key};
use crate::order;
use crate::path::{EditOp, Segment, Settable};
use crate::table::{ModelOps, ModelTable};
use std::any::Any;

/// A stable identity for an item in a resource. Minted deterministically by
/// [`ActionCx::id`](crate::ActionCx::id), so replaying an action on another
/// replica mints the same id.
///
/// The default id is empty: an item that refers to no other yet.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    /// Adopt an id that was minted earlier (read from a document, a URL or an
    /// action argument).
    #[must_use]
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Id {
    fn from(raw: &str) -> Self {
        Self(raw.to_owned())
    }
}

impl From<String> for Id {
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

impl std::str::FromStr for Id {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl Settable for Id {}
impl Model for Id {
    const SCALAR: bool = true;
    crate::__own_table!(Id);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "string"})
    }
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.0.clone())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => Ok(Self(text.clone())),
            _ => Err(ModelError::shape("an id")),
        }
    }
    #[inline(never)]
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match value {
            Value::Scalar(scalar) => match scalar.as_ref() {
                ScalarValue::Str(text) => Ok(Self(text.to_string())),
                _ => Err(ModelError::shape("an id")),
            },
            Value::Object(_) => Err(ModelError::shape("an id")),
        }
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.0.as_str().into()))
    }
}

pub(crate) const ORDER: &str = "k";
pub(crate) const VALUE: &str = "v";

/// An ordered collection of items addressed by [`Id`].
///
/// Stored as a map from id to `{ k: order key, v: value }`. Editing an item
/// writes only that item; a move rewrites one order key; concurrent inserts
/// from several collaborators interleave deterministically; deleting an item
/// never disturbs another item's identity. This is the default container for
/// anything a user adds, reorders or removes.
///
/// A collection read from a document remembers each item's order key, so a
/// later change re-reads only the items it touched
/// ([`Model::refresh`]) instead of the whole collection.
///
/// Cloning a collection shares its items: a clone copies one pointer per
/// item, and an item is copied only when one of the collections sharing it
/// changes it. A view holding the state it was shown therefore costs the
/// next change one copied item, not a copy of every item.
#[derive(Debug)]
pub struct Collection<T> {
    items: Vec<Entry<T>>,
    /// Each id's position, built on the first lookup after the order
    /// changed, so finding an item by id is constant time.
    index: Index,
    membership: Membership,
    /// How to copy an item another collection still shares.
    copier: Copier<T>,
    /// Stored items that did not read, and why: left out of the items,
    /// kept in the document for their repair ([`Self::quarantined`]).
    quarantine: Vec<Quarantined>,
    /// A quarantined item was repaired or discarded since the collection
    /// was read: only a whole write stores that as a whole read and write
    /// would, keeping the stored order key or deleting what it never read.
    whole_write: bool,
}

/// A stored item of a [`Collection`] that does not read as its type, left
/// out of the collection rather than making the whole record unreadable.
///
/// It stays in the document as it is. It is repaired by pushing a whole
/// item under its id ([`Collection::push`]), which stores every field, or
/// removed with the remove-item edit (or [`Collection::discard`]); a
/// migration may rewrite it in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quarantined {
    /// The item's id.
    pub id: Id,
    /// Why it does not read, where in the item.
    pub reason: String,
}

impl<T: Clone> Clone for Collection<T> {
    fn clone(&self) -> Self {
        let copy: fn(&T) -> T = T::clone;
        // Sharing starts here, so from here on either side may need to copy.
        let _ = self.copier.0.set(copy);
        Self {
            items: self.items.clone(),
            index: self.index.clone(),
            membership: self.membership,
            copier: Copier(OnceLock::from(copy)),
            quarantine: self.quarantine.clone(),
            whole_write: self.whole_write,
        }
    }
}

/// How a collection copies an item it shares with a clone. Recorded by the
/// first clone: items are shared by cloning alone, and only a collection of
/// `Clone` items clones.
struct Copier<T>(OnceLock<fn(&T) -> T>);

impl<T> Default for Copier<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T> fmt::Debug for Copier<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Copier")
    }
}

impl<T> Copier<T> {
    /// The item `item` holds, to change: copied first when another
    /// collection still shares it, so that one keeps seeing it unchanged.
    /// `None` only for a shared item with no copier, which cannot be.
    fn unshare<'a>(&self, item: &'a mut Arc<T>) -> Option<&'a mut T> {
        if Arc::get_mut(item).is_none()
            && let Some(copy) = self.0.get()
        {
            *item = Arc::new(copy(item));
        }
        Arc::get_mut(item)
    }

    /// The item `item` holds, taken out of the collection: copied when
    /// another collection still shares it.
    fn take(&self, item: Arc<T>) -> Option<T> {
        match Arc::try_unwrap(item) {
            Ok(item) => Some(item),
            Err(shared) => self.0.get().map(|copy| copy(&shared)),
        }
    }
}

/// Which ids a [`Collection`] holds, in which order, as one value to
/// compare: two collections (or one collection at two times) with the same
/// membership hold the same ids in the same order. A list view keeps it to
/// tell that a change left its rows as they were without comparing every
/// id. Different memberships say nothing: the ids may still be the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Membership(u64);

impl Membership {
    /// The membership of every empty collection never changed since.
    const EMPTY: Self = Self(0);

    /// A membership no collection had before.
    fn fresh() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Shared between clones until one of them moves an item.
#[derive(Clone, Default)]
struct Index(OnceLock<Arc<HashMap<Id, usize>>>);

impl fmt::Debug for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Index")
    }
}

impl Index {
    fn get<T>(&self, items: &[Entry<T>], id: &Id) -> Option<usize> {
        if let Some(index) = self.0.get() {
            return index.get(id).copied();
        }
        self.build(
            items.len(),
            &|position| items.get(position).map(|entry| &entry.id),
            id,
        )
    }

    /// Build the index of `count` ids (`id_at` each), then find `id`.
    #[inline(never)]
    fn build<'a>(
        &self,
        count: usize,
        id_at: &dyn Fn(usize) -> Option<&'a Id>,
        id: &Id,
    ) -> Option<usize> {
        let mut index = HashMap::with_capacity(count);
        for position in 0..count {
            if let Some(entry) = id_at(position) {
                index.insert(entry.clone(), position);
            }
        }
        let _ = self.0.set(Arc::new(index));
        self.0.get()?.get(id).copied()
    }

    fn reset(&mut self) {
        self.0 = OnceLock::new();
    }

    /// Note an item appended at `position`, keeping a built index.
    fn appended(&mut self, id: &Id, position: usize) {
        let Some(index) = self.0.get_mut() else {
            return;
        };
        match Arc::get_mut(index) {
            Some(index) => {
                index.insert(id.clone(), position);
            }
            // A clone still looks items up in it: build a new one when next
            // asked rather than copy this one now.
            None => self.reset(),
        }
    }
}

#[derive(Debug)]
struct Entry<T> {
    id: Id,
    /// The stored order key, when this entry was read from a document and
    /// has not been moved since.
    key: Option<String>,
    item: Arc<T>,
}

impl<T> Clone for Entry<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            key: self.key.clone(),
            item: Arc::clone(&self.item),
        }
    }
}

impl<T> Entry<T> {
    fn new(id: Id, item: T) -> Self {
        Self {
            id,
            key: None,
            item: Arc::new(item),
        }
    }
}

impl<T: PartialEq> PartialEq for Collection<T> {
    /// Equal items in equal order; where each item's order key came from is
    /// bookkeeping, not content. An item two clones still share is the same
    /// value and is not compared field by field.
    fn eq(&self, other: &Self) -> bool {
        if self.items.len() != other.items.len() {
            return false;
        }
        for at in 0..self.items.len() {
            match (self.items.get(at), other.items.get(at)) {
                (Some(a), Some(b))
                    if a.id == b.id && (Arc::ptr_eq(&a.item, &b.item) || a.item == b.item) => {}
                _ => return false,
            }
        }
        true
    }
}

impl<T> Default for Collection<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            index: Index::default(),
            membership: Membership::EMPTY,
            copier: Copier::default(),
            quarantine: Vec::new(),
            whole_write: false,
        }
    }
}

impl<T> Collection<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Which ids the collection holds, in which order ([`Membership`]).
    /// Editing an item keeps it; adding, removing or moving one changes it.
    pub fn membership(&self) -> Membership {
        self.membership
    }

    /// The ids or their order may have changed.
    fn moved(&mut self) {
        self.membership = Membership::fresh();
    }

    /// Items in collection order.
    #[allow(
        clippy::iter_without_into_iter,
        reason = "a collection iterates its items by id; `for` over it would hide which"
    )]
    pub fn iter(&self) -> Iter<'_, T> {
        Iter(self.items.iter())
    }

    /// The item at `position` in collection order.
    pub fn at(&self, position: usize) -> Option<(&Id, &T)> {
        self.items
            .get(position)
            .map(|entry| (&entry.id, &*entry.item))
    }

    #[allow(
        clippy::iter_without_into_iter,
        reason = "a collection iterates its items by id; `for` over it would hide which"
    )]
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        IterMut {
            entries: self.items.iter_mut(),
            copier: &self.copier,
        }
    }

    pub fn values(&self) -> Values<'_, T> {
        Values(self.items.iter())
    }

    pub fn ids(&self) -> Ids<'_, T> {
        Ids(self.items.iter())
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.position(id).is_some()
    }

    pub fn position(&self, id: &Id) -> Option<usize> {
        self.index.get(&self.items, id)
    }

    pub fn get(&self, id: &Id) -> Option<&T> {
        let position = self.position(id)?;
        self.items.get(position).map(|entry| &*entry.item)
    }

    /// The item `id`, to change. Copies it first when a clone of this
    /// collection still shares it; no other item is copied.
    pub fn get_mut(&mut self, id: &Id) -> Option<&mut T> {
        let position = self.position(id)?;
        let entry = self.items.get_mut(position)?;
        self.copier.unshare(&mut entry.item)
    }

    /// The stored items that did not read, each with why: the view shows
    /// the rest, and these are repaired by pushing a whole item under the
    /// id or removed ([`Quarantined`]).
    #[must_use]
    pub fn quarantined(&self) -> &[Quarantined] {
        &self.quarantine
    }

    /// Forget the quarantined item `id`: the next write deletes it from the
    /// document, as it deletes any item no longer in the collection.
    /// Whether it was quarantined.
    pub fn discard(&mut self, id: &Id) -> bool {
        let before = self.quarantine.len();
        self.quarantine.retain(|held| &held.id != id);
        let discarded = self.quarantine.len() != before;
        self.whole_write |= discarded;
        discarded
    }

    /// Stop holding the quarantined item `id`, as an item takes its place.
    fn repaired(&mut self, id: &Id) {
        let before = self.quarantine.len();
        self.quarantine.retain(|held| &held.id != id);
        self.whole_write |= self.quarantine.len() != before;
    }

    /// Append an item. Returns its id for chaining. An id already present
    /// is replaced in place; a quarantined one is repaired by it.
    pub fn push(&mut self, id: Id, item: T) -> Id {
        self.repaired(&id);
        if let Some(position) = self.position(&id)
            && let Some(existing) = self.items.get_mut(position)
        {
            existing.item = Arc::new(item);
            return id;
        }
        self.items.push(Entry::new(id.clone(), item));
        let position = self.items.len().saturating_sub(1);
        self.index.appended(&id, position);
        self.moved();
        id
    }

    /// Insert an item at `index` (clamped to the end). An id already present
    /// is moved there.
    pub fn insert(&mut self, index: usize, id: Id, item: T) -> Id {
        self.repaired(&id);
        self.remove(&id);
        let index = index.min(self.items.len());
        self.items.insert(index, Entry::new(id.clone(), item));
        self.index.reset();
        self.moved();
        id
    }

    pub fn remove(&mut self, id: &Id) -> Option<T> {
        let index = self.position(id)?;
        self.index.reset();
        self.moved();
        self.copier.take(self.items.remove(index).item)
    }

    /// Move an item to `index` (clamped to the end). Returns whether it exists.
    pub fn move_to(&mut self, id: &Id, index: usize) -> bool {
        let Some(from) = self.position(id) else {
            return false;
        };
        let mut entry = self.items.remove(from);
        entry.key = None;
        let index = index.min(self.items.len());
        self.items.insert(index, entry);
        self.index.reset();
        self.moved();
        true
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Id, &T) -> bool) {
        let before = self.items.len();
        self.items.retain(|entry| keep(&entry.id, &*entry.item));
        // Retaining only removes: the same length is the same ids.
        if self.items.len() != before {
            self.index.reset();
            self.moved();
        }
    }

    pub fn sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        crate::sort::sort_by(&mut self.items, |a, b| compare(&*a.item, &*b.item));
        for entry in &mut self.items {
            entry.key = None;
        }
        self.index.reset();
        self.moved();
    }
}

// Named iterators rather than closures over entries: one small set of
// methods per item type, whatever a caller chains onto them.

/// The items of a [`Collection`] with their ids, in order.
pub struct Iter<'a, T>(std::slice::Iter<'a, Entry<T>>);
/// The items of a [`Collection`], to change, with their ids. Each item a
/// clone of the collection still shares is copied as it is reached.
pub struct IterMut<'a, T> {
    entries: std::slice::IterMut<'a, Entry<T>>,
    copier: &'a Copier<T>,
}
/// The items of a [`Collection`], in order.
pub struct Values<'a, T>(std::slice::Iter<'a, Entry<T>>);
/// The ids of a [`Collection`], in order.
pub struct Ids<'a, T>(std::slice::Iter<'a, Entry<T>>);

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (&'a Id, &'a T);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.0.next()?;
        Some((&entry.id, &*entry.item))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
    fn last(mut self) -> Option<Self::Item> {
        DoubleEndedIterator::next_back(&mut self)
    }
}
impl<T> DoubleEndedIterator for Iter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let entry = self.0.next_back()?;
        Some((&entry.id, &*entry.item))
    }
}
impl<T> ExactSizeIterator for Iter<'_, T> {}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = (&'a Id, &'a mut T);
    fn next(&mut self) -> Option<Self::Item> {
        // An item is skipped only if it could not be unshared, which an
        // item shared by cloning always can.
        for entry in self.entries.by_ref() {
            if let Some(item) = self.copier.unshare(&mut entry.item) {
                return Some((&entry.id, item));
            }
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.entries.size_hint()
    }
}

impl<'a, T> Iterator for Values<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        Some(&*self.0.next()?.item)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
    fn last(mut self) -> Option<Self::Item> {
        DoubleEndedIterator::next_back(&mut self)
    }
}
impl<T> DoubleEndedIterator for Values<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        Some(&*self.0.next_back()?.item)
    }
}
impl<T> ExactSizeIterator for Values<'_, T> {}

impl<'a, T> Iterator for Ids<'a, T> {
    type Item = &'a Id;
    fn next(&mut self) -> Option<Self::Item> {
        Some(&self.0.next()?.id)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
    fn last(mut self) -> Option<Self::Item> {
        DoubleEndedIterator::next_back(&mut self)
    }
}
impl<T> DoubleEndedIterator for Ids<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        Some(&self.0.next_back()?.id)
    }
}
impl<T> ExactSizeIterator for Ids<'_, T> {}

impl<T> std::ops::Index<&Id> for Collection<T> {
    type Output = T;
    #[allow(
        clippy::panic,
        reason = "indexing by an absent id is a caller bug, exactly like slice indexing"
    )]
    fn index(&self, id: &Id) -> &T {
        match self.get(id) {
            Some(item) => item,
            None => panic!("no item with id {id} in collection"),
        }
    }
}

impl<T> FromIterator<(Id, T)> for Collection<T> {
    fn from_iter<I: IntoIterator<Item = (Id, T)>>(iter: I) -> Self {
        let mut collection = Self::default();
        for (id, item) in iter {
            collection.push(id, item);
        }
        collection
    }
}

pub(crate) struct Stored {
    pub(crate) id: Id,
    pub(crate) key: String,
    /// The entry's map; `None` for an item stored as its order key alone.
    pub(crate) entry: Option<ObjId>,
    /// What the entry's value slot holds, for reading the item.
    pub(crate) value: crate::model::MapSlot,
}

/// Whether items of the type `item` are stored as their order key alone:
/// an item that stores nothing (`()`, a [`Grid`](crate::Grid)'s rows and
/// columns) needs no map around the key.
pub(crate) fn order_only(item: &ModelTable) -> bool {
    std::ptr::eq(item, <() as Model>::TABLE)
}

/// Write the entry of the item `id` under the order key `key` into the
/// collection map `map`: the key itself for an item stored as its order key
/// alone (`order_only`), else a map `{ k, v }` whose value `store` writes.
pub(crate) fn put_entry(
    doc: &mut Doc,
    map: &ObjId,
    id: &Id,
    key: String,
    order_only: bool,
    store: &dyn Fn(&mut Doc, Slot<'_>) -> Result<(), ModelError>,
) -> Result<(), ModelError> {
    if order_only {
        return Slot::Key(map, id.as_str()).put_scalar(doc, ScalarValue::Str(key.into()));
    }
    let entry = Slot::Key(map, id.as_str()).object(doc, ObjType::Map)?;
    Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
    store(doc, Slot::Key(&entry, VALUE)).located(id.as_str())
}

/// Write `items`, in this order, as the map from id to `{ k, v }` a
/// [`Collection`] is stored as: items no longer there (and not
/// `quarantined`) are deleted, every
/// order key still consistent with the new order is kept (the longest
/// increasing run), and keys are minted only for new or moved items.
pub(crate) fn store_in_order(
    doc: &mut Doc,
    slot: Slot<'_>,
    items: &[(&Id, &dyn Stores)],
    order_only: bool,
    quarantined: &dyn Fn(&Id) -> bool,
) -> Result<(), ModelError> {
    let map = slot.object(doc, ObjType::Map)?;
    let stored = stored_entries(doc, &map);
    let present: HashSet<&Id> = items.iter().map(|(id, _)| *id).collect();
    for entry in &stored {
        // An item that did not read is kept as it is stored, for its
        // repair: not reading it is no reason to delete it.
        if !present.contains(&entry.id) && !quarantined(&entry.id) {
            doc.delete(&map, entry.id.as_str())?;
        }
    }
    let by_id: HashMap<&Id, &str> = stored
        .iter()
        .map(|entry| (&entry.id, entry.key.as_str()))
        .collect();
    let keys = order_keys(items.iter().map(|(id, _)| *id), &by_id);
    for ((id, item), key) in items.iter().zip(keys) {
        put_entry(doc, &map, id, key, order_only, &|doc, slot| {
            item.store_to(doc, slot)
        })?;
    }
    Ok(())
}

/// The order key each of `ids` is stored under, given the keys `stored`
/// holds: every stored key still consistent with the order (the longest
/// increasing run) is kept; new and moved items get minted keys.
fn order_keys<'a>(ids: impl Iterator<Item = &'a Id>, stored: &HashMap<&Id, &str>) -> Vec<String> {
    let existing: Vec<Option<&str>> = ids.map(|id| stored.get(id).copied()).collect();
    let kept = longest_increasing(&existing);
    let mut keys: Vec<Option<String>> = vec![None; existing.len()];
    for &index in &kept {
        if let (Some(slot), Some(Some(key))) = (keys.get_mut(index), existing.get(index)) {
            *slot = Some((*key).to_owned());
        }
    }
    let mut index = 0;
    while index < keys.len() {
        if keys.get(index).is_some_and(Option::is_some) {
            index = index.saturating_add(1);
            continue;
        }
        let start = index;
        while keys.get(index).is_some_and(Option::is_none) {
            index = index.saturating_add(1);
        }
        let low = start
            .checked_sub(1)
            .and_then(|before| keys.get(before))
            .and_then(Clone::clone);
        let high = keys.get(index).and_then(Clone::clone);
        let minted = order::sequence(low.as_deref(), high.as_deref(), index.saturating_sub(start));
        for (offset, key) in minted.into_iter().enumerate() {
            if let Some(slot) = keys.get_mut(start.saturating_add(offset)) {
                *slot = Some(key);
            }
        }
    }
    keys.into_iter().map(Option::unwrap_or_default).collect()
}

/// Edit a map of `{ k, v }` entries (a [`Collection`], an
/// [`Ordered`](crate::Ordered)) at `path`: into the item a path names, or
/// insert, remove and move items. What depends on the item type comes in as
/// functions, so this compiles once.
pub(crate) struct EntryEdits<'a> {
    /// Apply an edit inside the value slot of an item.
    pub(crate) item: &'a EditAt<'a>,
    /// A new item from its JSON form, checked, and the id it carries.
    pub(crate) parse: &'a ParseItem<'a>,
    /// The edit of the whole value, for any other operation.
    pub(crate) whole: &'a EditWhole<'a>,
    /// Items are stored as their order key alone ([`order_only`]).
    pub(crate) order_only: bool,
}

/// [`Model::edit`] at a slot.
pub(crate) type EditAt<'a> =
    dyn Fn(&mut Doc, Slot<'_>, &[Segment], &EditOp, &mut ActionCx) -> Result<(), ModelError> + 'a;
/// A new item from its JSON form, and the id it carries.
pub(crate) type ParseItem<'a> =
    dyn Fn(&serde_json::Value) -> Result<(Option<Id>, Box<dyn Stores + 'a>), ModelError> + 'a;
/// An edit of a value as a whole.
pub(crate) type EditWhole<'a> =
    dyn Fn(&mut Doc, Slot<'_>, &[Segment], &EditOp) -> Result<(), ModelError> + 'a;
/// Write the item at a position over the one it was, in an entry's slot.
type WriteChanged<'a> =
    dyn Fn(usize, usize, &mut Doc, &ObjId, &Delta) -> Result<(), ModelError> + 'a;
/// Store the item at a position, or the whole value, into a slot.
type StoreAt<'a> = dyn Fn(usize, &mut Doc, Slot<'_>) -> Result<(), ModelError> + 'a;
type StoreWhole<'a> = dyn Fn(&mut Doc, Slot<'_>) -> Result<(), ModelError> + 'a;

pub(crate) fn edit_entries(
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
    cx: &mut ActionCx,
    edits: &EntryEdits<'_>,
) -> Result<(), ModelError> {
    match (path.split_first(), op) {
        (Some((Segment::Item(id), rest)), _) => {
            let map = slot.object(doc, ObjType::Map)?;
            let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? else {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            };
            // An edit inside an item whose value is gone would make the
            // value anew holding the edited field alone: an item missing
            // every other field.
            if !rest.is_empty() && doc.get(&entry, VALUE)?.is_none() {
                return Err(ModelError::invalid(format!(
                    "item `{id}` holds nothing to edit"
                )));
            }
            (edits.item)(doc, Slot::Key(&entry, VALUE), rest, op, cx).located(id.as_str())
        }
        (None, EditOp::InsertItem { id, at, value }) => {
            let (carried, item) = (edits.parse)(value)?;
            let map = slot.object(doc, ObjType::Map)?;
            let id = carried.or_else(|| id.clone()).unwrap_or_else(|| cx.id());
            if doc.get(&map, id.as_str())?.is_some() {
                return Err(ModelError::invalid(format!("an item `{id}` exists")));
            }
            let stored = stored_entries(doc, &map);
            let key = key_at(&stored, *at, None);
            put_entry(doc, &map, &id, key, edits.order_only, &|doc, slot| {
                item.store_to(doc, slot)
            })
        }
        (None, EditOp::RemoveItem { id }) => {
            let map = slot.object(doc, ObjType::Map)?;
            if doc.get(&map, id.as_str())?.is_none() {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            }
            doc.delete(&map, id.as_str())?;
            Ok(())
        }
        (None, EditOp::MoveTo { id, at }) => {
            let map = slot.object(doc, ObjType::Map)?;
            let stored = stored_entries(doc, &map);
            let Some(entry) = stored.iter().find(|entry| &entry.id == id) else {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            };
            let key = key_at(&stored, *at, Some(id));
            match &entry.entry {
                Some(entry) => {
                    Slot::Key(entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))
                }
                None => Slot::Key(&map, id.as_str()).put_scalar(doc, ScalarValue::Str(key.into())),
            }
        }
        _ => (edits.whole)(doc, slot, path, op),
    }
}

/// What [`store_entries_changed`] writes of the items through their type:
/// by position now (`now`) and in the record as it was (`was`).
pub(crate) struct EntryWrites<'a> {
    /// Whether the item now would store as the one it was.
    pub(crate) same: &'a dyn Fn(usize, usize) -> bool,
    /// Store the item now into a slot.
    pub(crate) store: &'a StoreAt<'a>,
    /// Write the item now over the value of the entry that held it.
    pub(crate) changed: &'a WriteChanged<'a>,
    /// Store the whole collection, when only the document can say.
    pub(crate) whole: &'a StoreWhole<'a>,
    /// Items are stored as their order key alone ([`order_only`]).
    pub(crate) order_only: bool,
}

/// [`Model::store_changed`] of a map of `{ k, v }` entries: the items `ids`
/// over the `stored` ones (id and order key each) they replace; only new,
/// moved and changed items are visited, and an item someone else touched
/// (`dirty`) is written as `store` writes it.
pub(crate) fn store_entries_changed(
    doc: &mut Doc,
    slot: Slot<'_>,
    ids: &[&Id],
    stored: &[(&Id, Option<&str>)],
    dirty: &Delta,
    writes: &EntryWrites<'_>,
) -> Result<(), ModelError> {
    let current: HashSet<&Id> = ids.iter().copied().collect();
    // What was read from a document knows each item's stored order key;
    // with one missing, or an id twice over, only the document can say.
    if current.len() != ids.len() || stored.iter().any(|(_, key)| key.is_none()) {
        return (writes.whole)(doc, slot);
    }
    let map = slot.object(doc, ObjType::Map)?;
    // `stored` is in stored order, the order `store` deletes in.
    for (id, _) in stored {
        if !current.contains(id) {
            doc.delete(&map, id.as_str())?;
        }
    }
    let was: HashMap<&Id, (usize, &str)> = stored
        .iter()
        .enumerate()
        .filter_map(|(at, (id, key))| Some((*id, (at, (*key)?))))
        .collect();
    let by_id: HashMap<&Id, &str> = was.iter().map(|(id, (_, key))| (*id, *key)).collect();
    for ((now, id), key) in ids
        .iter()
        .enumerate()
        .zip(order_keys(ids.iter().copied(), &by_id))
    {
        let touched = dirty.key(&map, id.as_str());
        let Some(&(at, stored_key)) = was.get(id).filter(|_| touched != Touch::Written) else {
            // New here, or replaced by someone else: written whole.
            put_entry(doc, &map, id, key, writes.order_only, &|doc, slot| {
                (writes.store)(now, doc, slot)
            })?;
            continue;
        };
        let kept = touched == Touch::Untouched && stored_key == key;
        if kept && (writes.same)(now, at) {
            continue;
        }
        // The entry this item was read from is gone from the document (a
        // change it raced deleted it): written whole, as a whole write
        // would, never as the fields that changed in an entry made now.
        if !writes.order_only
            && !matches!(
                doc.get(&map, id.as_str())?,
                Some((Value::Object(ObjType::Map), _))
            )
        {
            put_entry(doc, &map, id, key, false, &|doc, slot| {
                (writes.store)(now, doc, slot)
            })?;
            continue;
        }
        if writes.order_only {
            if !kept {
                Slot::Key(&map, id.as_str()).put_scalar(doc, ScalarValue::Str(key.into()))?;
            }
            continue;
        }
        let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
        if !kept {
            Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
        }
        (writes.changed)(now, at, doc, &entry, dirty).located(id.as_str())?;
    }
    Ok(())
}

/// What a change did to the entry `key` of a collection's map.
enum Touched {
    /// It is gone.
    Gone,
    /// Something inside the entry already read changed: its order key, when
    /// that changed, and its value's slot.
    Below { entry: ObjId, order: Option<String> },
    /// A new entry, or one replaced whole.
    Fresh { entry: ObjId, order: String },
    /// An item stored as its order key alone, new or moved.
    Order { order: String },
}

/// How `delta` touched the entry `key` of the collection map `map`; `below`
/// when the entry was read before and only changed inside.
fn touched_entry(
    doc: &Doc,
    map: &ObjId,
    key: &str,
    delta: &Delta,
    below: bool,
) -> Result<Touched, ModelError> {
    let entry = match doc.get(map, key)? {
        Some((Value::Object(ObjType::Map), entry)) => entry,
        Some((Value::Scalar(scalar), _)) => {
            return match scalar.as_ref() {
                ScalarValue::Str(order) => Ok(Touched::Order {
                    order: order.to_string(),
                }),
                _ => Err(ModelError::shape("an order key").at(key)),
            };
        }
        _ => return Ok(Touched::Gone),
    };
    let order = |doc: &Doc| {
        read_key::<crate::Atom>(doc, &entry, ORDER)
            .map(|k| k.0)
            .located(key)
    };
    if below {
        let order = if delta.key(&entry, ORDER) == Touch::Untouched {
            None
        } else {
            Some(order(doc)?)
        };
        return Ok(Touched::Below { entry, order });
    }
    let order = order(doc)?;
    Ok(Touched::Fresh { entry, order })
}

/// The JSON form's schema of a collection of items of the schema `item`.
#[inline(never)]
fn entries_schema(item: serde_json::Value) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert("id".into(), serde_json::json!({"type": "string"}));
    properties.insert("value".into(), item);
    let mut entry = serde_json::Map::new();
    entry.insert("type".into(), "object".into());
    entry.insert("properties".into(), properties.into());
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), "array".into());
    schema.insert("items".into(), entry.into());
    schema.into()
}

/// An item's entry in a collection's JSON form.
#[inline(never)]
fn entry_json(id: &Id, value: serde_json::Value) -> serde_json::Value {
    let mut entry = serde_json::Map::new();
    entry.insert("id".into(), id.as_str().into());
    entry.insert("value".into(), value);
    entry.into()
}

/// The id and value of the entry at `index` of a collection's JSON form.
#[inline(never)]
fn entry_parts(
    index: usize,
    entry: &serde_json::Value,
) -> Result<(Id, &serde_json::Value), ModelError> {
    let id = entry
        .get("id")
        .map(Id::from_json)
        .transpose()?
        .ok_or_else(|| {
            ModelError::Missing {
                path: index.to_string(),
            }
            .at("id")
        })?;
    Ok((id, entry.get("value").unwrap_or(&serde_json::Value::Null)))
}

/// Read every `{ k, v }` entry of the map `id` in stored order, handing
/// each entry's value slot to `read`.
pub(crate) fn load_entries(
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    read: &mut dyn FnMut(Stored) -> Result<(), ModelError>,
) -> Result<(), ModelError> {
    if !matches!(value, Value::Object(ObjType::Map)) {
        return Err(ModelError::shape("a collection"));
    }
    for stored in stored_entries(doc, id) {
        let at = stored.id.clone();
        read(stored).located(at.as_str())?;
    }
    Ok(())
}

/// The positions of entries in stored order: by order key, ties by id.
/// Sorting positions rather than entries compiles one sort for every item
/// type.
fn stored_order(keys: &[(Option<&str>, &Id)]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.compact_sort_by(|&a, &b| keys.get(a).cmp(&keys.get(b)));
    order
}

/// The stored entries of the collection map `map`, in order. One walk of
/// the map lists the entries and one walk of each entry reads its order key
/// and value slot: a lookup per key would open each map again.
pub(crate) fn stored_entries(doc: &Doc, map: &ObjId) -> Vec<Stored> {
    let mut entries = Vec::new();
    for item in doc.map_range(map, ..) {
        // An item stored as its order key alone.
        if let automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(order)) = &item.value {
            entries.push(Stored {
                id: Id(item.key.clone().into_owned()),
                key: order.to_string(),
                entry: None,
                value: None,
            });
            continue;
        }
        if item.value != automerge::ValueRef::Object(ObjType::Map) {
            continue;
        }
        let entry = item.id();
        let [order, value] = crate::model::read_slots(doc, &entry, [ORDER, VALUE]);
        // An entry without a readable order key is no entry anyone placed:
        // left out, never the reason its whole collection does not read.
        let Ok(order) = crate::model::load_slot::<crate::Atom>(doc, order, ORDER) else {
            continue;
        };
        entries.push(Stored {
            id: Id(item.key.into_owned()),
            key: order.0,
            entry: Some(entry),
            value,
        });
    }
    // `(order key, id)` is unique, so an unstable sort orders as a stable one.
    entries.compact_sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
    entries
}

/// An order key placing an item at `at` among `stored` (which may hold the
/// item itself, `moving`, which does not count).
pub(crate) fn key_at(stored: &[Stored], at: usize, moving: Option<&Id>) -> String {
    let others: Vec<&Stored> = stored
        .iter()
        .filter(|entry| Some(&entry.id) != moving)
        .collect();
    let at = at.min(others.len());
    let low = at
        .checked_sub(1)
        .and_then(|before| others.get(before))
        .map(|entry| entry.key.as_str());
    let high = others.get(at).map(|entry| entry.key.as_str());
    order::between(low, high)
}

/// Indices (into `keys`) of one longest strictly increasing subsequence.
fn longest_increasing(keys: &[Option<&str>]) -> Vec<usize> {
    // Patience sorting over the present keys, O(n log n).
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; keys.len()];
    for (index, key) in keys.iter().enumerate() {
        let Some(key) = key else { continue };
        let position =
            tails.partition_point(|&tail| keys.get(tail).copied().flatten() < Some(*key));
        if let Some(before) = position.checked_sub(1)
            && let Some(slot) = previous.get_mut(index)
        {
            *slot = tails.get(before).copied();
        }
        if position == tails.len() {
            tails.push(index);
        } else if let Some(tail) = tails.get_mut(position) {
            *tail = index;
        }
    }
    let mut chain = Vec::with_capacity(tails.len());
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        chain.push(index);
        cursor = previous.get(index).copied().flatten();
    }
    chain.reverse();
    chain
}

impl<T> Collection<T> {
    /// Sort the items as stored: by order key, ties by id.
    fn reorder_by_keys(&mut self) {
        let mut keys = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            keys.push((entry.key.as_deref(), &entry.id));
        }
        let mut order = stored_order(&keys);
        // In place, one cycle of the permutation at a time: the entry for
        // position `at` is the one at `order[at]`.
        for start in 0..order.len() {
            let mut at = start;
            while let Some(from) = order.get_mut(at).map(|from| std::mem::replace(from, at)) {
                if from == start {
                    break;
                }
                self.items.swap(at, from);
                at = from;
            }
        }
    }
}

impl<T: Model> crate::items::ItemSource for Collection<T> {
    fn item_collection(&self, field: &'static str) -> crate::items::ItemCollection {
        listed_items(T::TABLE, self, field)
    }
    fn item_presence(&self) -> usize {
        usize::from(!self.items.is_empty())
    }
    fn label_of_item(&self, id: &Id) -> Option<String> {
        let entries: &dyn Entries = self;
        let item = entries.item_at(entries.find(id)?)?;
        Some(label_of(T::TABLE, id, item))
    }
    fn item_texts(&self, field: &'static str, found: &mut Vec<crate::items::ItemText>) {
        item_texts(T::TABLE, self, field, found);
    }
    fn nested_items(&self, field: &'static str, out: &mut Vec<crate::items::ItemCollection>) {
        nested_items(T::TABLE, self, field, out);
    }
}

/// Items with their ids, whatever their type and wherever their ids are
/// kept: what listing them for publication and search reads.
#[doc(hidden)]
pub trait ItemList {
    fn count(&self) -> usize;
    fn item(&self, at: usize) -> Option<(Id, &dyn Any)>;
}

impl<T: Model> crate::table::PublishedItems for Collection<T> {
    const ITEM_TABLE: &'static ModelTable = T::TABLE;
}

impl<T: 'static> ItemList for Collection<T> {
    fn count(&self) -> usize {
        self.items.len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn Any)> {
        let entry = self.items.get(at)?;
        Some((entry.id.clone(), &*entry.item as &dyn Any))
    }
}

/// The label an item is offered under: its `#[model(label)]`, else its
/// first text, else its id.
pub(crate) fn label_of(item: &ModelTable, id: &Id, value: &dyn Any) -> String {
    if let Some(label) = (item.ops.label)(item, value)
        && !label.is_empty()
    {
        return label;
    }
    let mut text = Vec::new();
    (item.ops.collect_text)(item, value, &mut text);
    crate::__private::first_line_of(text).unwrap_or_else(|| id.to_string())
}

/// [`ItemSource::item_collection`](crate::items::ItemSource::item_collection)
/// of `entries`, items of the type `item`.
pub(crate) fn listed_items(
    item: &ModelTable,
    entries: &dyn ItemList,
    field: &'static str,
) -> crate::items::ItemCollection {
    let mut labelled = false;
    let mut listed = Vec::new();
    for at in 0..entries.count().min(crate::items::ITEM_PAGE) {
        let Some((id, value)) = entries.item(at) else {
            continue;
        };
        if at == 0 {
            labelled = (item.ops.label)(item, value).is_some();
        }
        let label = label_of(item, &id, value);
        listed.push((id, label));
    }
    crate::items::ItemCollection {
        within: Vec::new(),
        field,
        labelled,
        items: listed,
    }
}

/// [`ItemSource::item_texts`](crate::items::ItemSource::item_texts) of
/// `entries`.
pub(crate) fn item_texts(
    item: &ModelTable,
    entries: &dyn ItemList,
    field: &'static str,
    found: &mut Vec<crate::items::ItemText>,
) {
    for at in 0..entries.count() {
        let Some((id, value)) = entries.item(at) else {
            continue;
        };
        if let Some(label) = (item.ops.label)(item, value) {
            let mut text = Vec::new();
            (item.ops.collect_text)(item, value, &mut text);
            found.push(crate::items::ItemText {
                field,
                id,
                label,
                text: text.join("\n"),
            });
        }
    }
}

/// [`ItemSource::nested_items`](crate::items::ItemSource::nested_items) of
/// `entries`.
pub(crate) fn nested_items(
    item: &ModelTable,
    entries: &dyn ItemList,
    field: &'static str,
    out: &mut Vec<crate::items::ItemCollection>,
) {
    for at in 0..entries.count().min(crate::items::ITEM_PAGE) {
        let Some((id, value)) = entries.item(at) else {
            continue;
        };
        for mut inner in (item.ops.nested_collections)(item, value) {
            inner.within.insert(0, (field, id.clone()));
            out.push(inner);
        }
    }
}

impl<T: Model> Model for Collection<T> {
    fn value_schema() -> serde_json::Value {
        entries_schema(T::value_schema())
    }
    const SHAPE: crate::Shape = crate::Shape::Collection(&T::SHAPE);
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    // Run by one set of functions for every collection, over `T`'s table.
    const TABLE: &'static ModelTable = &CollectionOf::<T>::TABLE;
    crate::model::leaf_paths!();
    crate::__table_functions!();
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
}

// --- A collection through its item type's table -----------------------------

/// A collection's entries, whatever its item type: what the collection
/// functions below reach into one with.
pub(crate) trait Entries {
    fn count(&self) -> usize;
    fn id_at(&self, at: usize) -> Option<&Id>;
    fn order_at(&self, at: usize) -> Option<&str>;
    fn item_at(&self, at: usize) -> Option<&dyn Any>;
    fn item_at_mut(&mut self, at: usize) -> Option<&mut dyn Any>;
    fn find(&self, id: &Id) -> Option<usize>;
    /// Append an entry (with `replace`, replace the item of an id already
    /// present), its item put by `fill` into an `Option` of the item type.
    fn push_filled(
        &mut self,
        entry: (Id, Option<String>, bool),
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
    /// Replace the entry at `at`, as [`Self::push_filled`].
    fn replace_filled(
        &mut self,
        at: usize,
        order: Option<String>,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
    fn remove_at(&mut self, at: usize);
    fn set_order(&mut self, at: usize, order: Option<String>);
    /// Sort the entries by their order keys; the ids may have moved.
    fn reorder(&mut self);
    /// The ids changed.
    fn changed(&mut self);
    /// Whether the stored item `id` is quarantined.
    fn is_quarantined(&self, id: &Id) -> bool;
    /// Hold the stored item `id` as quarantined, for `reason`.
    fn quarantine(&mut self, id: Id, reason: String);
    /// Stop holding `id` as quarantined: it reads again, or is gone.
    fn release(&mut self, id: &Id);
    /// Only a whole write stores the collection as it now is.
    fn needs_whole_write(&self) -> bool;
}

impl<T: 'static> Entries for Collection<T> {
    fn count(&self) -> usize {
        self.items.len()
    }

    fn id_at(&self, at: usize) -> Option<&Id> {
        self.items.get(at).map(|entry| &entry.id)
    }

    fn order_at(&self, at: usize) -> Option<&str> {
        self.items.get(at)?.key.as_deref()
    }

    fn item_at(&self, at: usize) -> Option<&dyn Any> {
        self.items.get(at).map(|entry| &*entry.item as &dyn Any)
    }

    /// Copies the item first when a clone of the collection still shares
    /// it: a refresh changes only the items a change touched.
    fn item_at_mut(&mut self, at: usize) -> Option<&mut dyn Any> {
        let entry = self.items.get_mut(at)?;
        let item = self.copier.unshare(&mut entry.item)?;
        Some(item as &mut dyn Any)
    }

    fn find(&self, id: &Id) -> Option<usize> {
        self.position(id)
    }

    fn push_filled(
        &mut self,
        (id, order, replace): (Id, Option<String>, bool),
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut slot: Option<T> = None;
        fill(&mut slot)?;
        let item = crate::table::filled(slot)?;
        if replace {
            self.push(id, item);
        } else {
            self.items.push(Entry {
                id,
                key: order,
                item: Arc::new(item),
            });
            self.index.reset();
        }
        Ok(())
    }

    fn replace_filled(
        &mut self,
        at: usize,
        order: Option<String>,
        fill: &mut dyn FnMut(&mut dyn Any) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let mut slot: Option<T> = None;
        fill(&mut slot)?;
        let item = crate::table::filled(slot)?;
        if let Some(entry) = self.items.get_mut(at) {
            entry.item = Arc::new(item);
            entry.key = order;
        }
        Ok(())
    }

    fn remove_at(&mut self, at: usize) {
        if at < self.items.len() {
            self.items.remove(at);
        }
    }

    fn set_order(&mut self, at: usize, order: Option<String>) {
        if let Some(entry) = self.items.get_mut(at) {
            entry.key = order;
        }
    }

    fn reorder(&mut self) {
        self.reorder_by_keys();
        self.index.reset();
        self.moved();
    }

    fn changed(&mut self) {
        self.index.reset();
        self.moved();
    }

    fn is_quarantined(&self, id: &Id) -> bool {
        self.quarantine.iter().any(|held| &held.id == id)
    }

    fn quarantine(&mut self, id: Id, reason: String) {
        match self.quarantine.iter_mut().find(|held| held.id == id) {
            Some(held) => held.reason = reason,
            None => self.quarantine.push(Quarantined { id, reason }),
        }
    }

    fn release(&mut self, id: &Id) {
        self.quarantine.retain(|held| &held.id != id);
    }

    fn needs_whole_write(&self) -> bool {
        self.whole_write
    }
}

/// What `Collection<T>`'s table reaches into one with.
pub struct CollectionDesc {
    /// `T`'s table.
    item: &'static ModelTable,
    entries: for<'a> fn(&'a dyn Any) -> Option<&'a dyn Entries>,
    entries_mut: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn Entries>,
    /// Put an empty collection into `out` (an `Option` of it), and hand it
    /// back.
    fresh: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn Entries>,
    /// An empty `Option` of a collection, the collection in a filled one,
    /// and moving it out over another.
    staged: (fn() -> Box<dyn Any>, crate::table::Unstage),
    replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
    /// An empty `Option` of an item and the item in a filled one.
    item_staged: (fn() -> Box<dyn Any>, crate::table::Unstage),
}

/// The [`CollectionDesc`] of `Collection<T>`.
pub struct CollectionOf<T>(std::marker::PhantomData<T>);

impl<T: Model> CollectionOf<T> {
    pub const DESC: CollectionDesc = CollectionDesc {
        item: T::TABLE,
        entries: entries_of::<T>,
        entries_mut: entries_mut_of::<T>,
        fresh: fresh_of::<T>,
        staged: (
            crate::table::empty_of::<Collection<T>>,
            crate::table::unwrap_of::<Collection<T>>,
        ),
        replace: crate::table::replace_of::<Collection<T>>,
        item_staged: (crate::table::empty_of::<T>, crate::table::unwrap_of::<T>),
    };

    pub const TABLE: ModelTable = ModelTable::of_collection(&Self::DESC, T::CHECKS, T::OPS);
}

fn entries_of<T: 'static>(value: &dyn Any) -> Option<&dyn Entries> {
    Some(value.downcast_ref::<Collection<T>>()? as &dyn Entries)
}

fn entries_mut_of<T: 'static>(value: &mut dyn Any) -> Option<&mut dyn Entries> {
    Some(value.downcast_mut::<Collection<T>>()? as &mut dyn Entries)
}

fn fresh_of<T: 'static>(out: &mut dyn Any) -> Option<&mut dyn Entries> {
    let out = out.downcast_mut::<Option<Collection<T>>>()?;
    let collection = out.insert(Collection::default());
    Some(collection as &mut dyn Entries)
}

fn listed(table: &ModelTable) -> Result<&'static CollectionDesc, ModelError> {
    table.collection().ok_or_else(crate::table::mismatch)
}

/// An item as [`store_in_order`] writes it, through its table.
struct TableItem<'a> {
    table: &'static ModelTable,
    value: &'a dyn Any,
}

impl Stores for TableItem<'_> {
    fn store_to(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        (self.table.ops.store)(self.table, self.value, doc, slot)
    }
}

/// An item made from its JSON form, staged in an `Option` of its type.
struct StagedItem {
    table: &'static ModelTable,
    staged: Box<dyn Any>,
    unstage: crate::table::Unstage,
}

impl Stores for StagedItem {
    fn store_to(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let value = (self.unstage)(self.staged.as_ref()).ok_or_else(crate::table::mismatch)?;
        (self.table.ops.store)(self.table, value, doc, slot)
    }
}

/// Whether `error` says a stored value does not read as its type (absent,
/// of another shape, invalid): what quarantines one item rather than
/// failing its whole record. A document error is no such thing.
fn quarantines(error: &ModelError) -> bool {
    matches!(
        error,
        ModelError::Missing { .. } | ModelError::Shape { .. } | ModelError::Invalid { .. }
    )
}

/// The value maps the entry `key` of the collection map `map` holds: the
/// winning entry's first, then those of the entries (and values) that
/// replicas made concurrently and the document keeps in conflict beside it.
fn conflicting_values(doc: &Doc, (map, key): (&ObjId, &str)) -> Vec<ObjId> {
    fn maps(doc: &Doc, obj: &ObjId, key: &str, out: &mut Vec<ObjId>) {
        if let Ok(Some((Value::Object(ObjType::Map), winner))) = doc.get(obj, key)
            && !out.contains(&winner)
        {
            out.push(winner);
        }
        for (value, id) in doc.get_all(obj, key).unwrap_or_default() {
            if matches!(value, Value::Object(ObjType::Map)) && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    let mut entries = Vec::new();
    maps(doc, map, key, &mut entries);
    let mut values = Vec::new();
    for entry in &entries {
        maps(doc, entry, VALUE, &mut values);
    }
    values
}

/// Read the item stored under the entry `key` of the collection map `map`
/// into `into` (an `Option` of `item`'s type), from the value slot `slot`
/// its entry holds. When that does not read and replicas made the entry
/// concurrently, the copies the document keeps in conflict are read as one
/// item, each field from the first copy holding it: the same id made on two
/// devices is one complete item, never one missing what the other copy
/// holds. `Ok(true)` when it was read so.
///
/// # Errors
/// Why the item does not read, from its winning copy.
fn fill_item(
    item: &ModelTable,
    doc: &Doc,
    entry: (&ObjId, &str),
    slot: Option<(&Value<'_>, &ObjId)>,
    into: &mut dyn Any,
) -> Result<bool, ModelError> {
    let error = match crate::table::load_slot(item, doc, slot, into) {
        Ok(()) => return Ok(false),
        Err(error) if quarantines(&error) => error,
        Err(error) => return Err(error),
    };
    let copies = conflicting_values(doc, entry);
    if copies.len() < 2 {
        return Err(error);
    }
    let merged = match item.record() {
        Some(record) => crate::table::read_fields_merged(record, doc, &copies, into).is_ok(),
        None => copies.iter().any(|copy| {
            crate::table::load_slot(item, doc, Some((&Value::Object(ObjType::Map), copy)), into)
                .is_ok()
        }),
    };
    if merged { Ok(true) } else { Err(error) }
}

/// The item just appended to `entries` was read from conflicting copies:
/// it forgets its order key, so the next write stores it whole, over the
/// winning copy, and the document holds it complete again.
fn mark_merged(entries: &mut dyn Entries) {
    if let Some(last) = entries.count().checked_sub(1) {
        entries.set_order(last, None);
    }
}

/// Read a collection stored in the map `id` into `out`, a fresh one.
fn load_into(
    desc: &CollectionDesc,
    doc: &Doc,
    value: &Value<'_>,
    id: &ObjId,
    out: &mut dyn Entries,
) -> Result<(), ModelError> {
    let item = desc.item;
    load_entries(doc, value, id, &mut |stored| {
        let slot = stored.value;
        let key = stored.id.clone();
        let mut merged = false;
        let read = out.push_filled((stored.id, Some(stored.key), false), &mut |into| {
            let slot = slot.as_ref().map(|(v, i)| (v, i));
            merged = fill_item(item, doc, (id, key.as_str()), slot, into).located(VALUE)?;
            Ok(())
        });
        match read {
            Ok(()) => {
                if merged {
                    mark_merged(&mut *out);
                }
                Ok(())
            }
            Err(error) if quarantines(&error) => {
                let reason = error.at(key.as_str()).to_string();
                out.quarantine(key, reason);
                Ok(())
            }
            Err(error) => Err(error),
        }
    })?;
    // What was read is a membership of its own.
    out.changed();
    Ok(())
}

/// The functions of every [`Collection`], over its item type's table.
pub static COLLECTION_OPS: ModelOps = ModelOps {
    without_absent: crate::shape::kept_whole,
    to_json: |table, value| {
        let (Ok(desc), Some(entries)) = (
            listed(table),
            table.collection().and_then(|desc| (desc.entries)(value)),
        ) else {
            return serde_json::Value::Null;
        };
        let mut out = Vec::with_capacity(entries.count());
        for at in 0..entries.count() {
            if let (Some(id), Some(item)) = (entries.id_at(at), entries.item_at(at)) {
                out.push(entry_json(id, (desc.item.ops.to_json)(desc.item, item)));
            }
        }
        serde_json::Value::Array(out)
    },
    from_json: |table, value, out| {
        let desc = listed(table)?;
        let serde_json::Value::Array(items) = value else {
            return Err(ModelError::shape("a list of items"));
        };
        let collection = (desc.fresh)(out).ok_or_else(crate::table::mismatch)?;
        for (index, entry) in items.iter().enumerate() {
            let (id, value) = entry_parts(index, entry)?;
            let at = id.clone();
            collection.push_filled((id, None, true), &mut |into| {
                (desc.item.ops.from_json)(desc.item, value, into).located(at.as_str())
            })?;
        }
        Ok(())
    },
    load: |table, doc, value, id, out| {
        let desc = listed(table)?;
        let collection = (desc.fresh)(out).ok_or_else(crate::table::mismatch)?;
        load_into(desc, doc, value, id, collection)
    },
    missing: |table, out| {
        let desc = listed(table)?;
        (desc.fresh)(out).ok_or_else(crate::table::mismatch)?;
        Ok(())
    },
    reload: |table, target, doc, slot| {
        let desc = listed(table)?;
        let mut staged = (desc.staged.0)();
        match slot {
            Some((value, id)) => (COLLECTION_OPS.load)(table, doc, value, id, staged.as_mut())?,
            None => (COLLECTION_OPS.missing)(table, staged.as_mut())?,
        }
        (desc.replace)(target, staged.as_mut())
    },
    store: |table, value, doc, slot| {
        let desc = listed(table)?;
        let entries = (desc.entries)(value).ok_or_else(crate::table::mismatch)?;
        let mut held: Vec<(&Id, TableItem<'_>)> = Vec::with_capacity(entries.count());
        for at in 0..entries.count() {
            if let (Some(id), Some(item)) = (entries.id_at(at), entries.item_at(at)) {
                held.push((
                    id,
                    TableItem {
                        table: desc.item,
                        value: item,
                    },
                ));
            }
        }
        let mut items: Vec<(&Id, &dyn Stores)> = Vec::with_capacity(held.len());
        for (id, item) in &held {
            items.push((*id, item as &dyn Stores));
        }
        store_in_order(doc, slot, &items, order_only(desc.item), &|id| {
            entries.is_quarantined(id)
        })
    },
    stores_as: |table, value, before| {
        let Some(desc) = table.collection() else {
            return false;
        };
        let (Some(now), Some(was)) = ((desc.entries)(value), (desc.entries)(before)) else {
            return false;
        };
        if now.needs_whole_write() || now.count() != was.count() || !strictly_ordered_entries(was) {
            return false;
        }
        for at in 0..now.count() {
            match (
                now.id_at(at),
                was.id_at(at),
                now.item_at(at),
                was.item_at(at),
            ) {
                (Some(id), Some(was_id), Some(item), Some(was_item))
                    if id == was_id
                        && (one_item(item, was_item)
                            || (desc.item.ops.stores_as)(desc.item, item, was_item)) => {}
                _ => return false,
            }
        }
        true
    },
    store_changed: |table, value, before, doc, slot, dirty| {
        let desc = listed(table)?;
        let now = (desc.entries)(value).ok_or_else(crate::table::mismatch)?;
        let was = (desc.entries)(before).ok_or_else(crate::table::mismatch)?;
        if now.needs_whole_write() {
            return (COLLECTION_OPS.store)(table, value, doc, slot);
        }
        let mut ids: Vec<&Id> = Vec::with_capacity(now.count());
        for at in 0..now.count() {
            if let Some(id) = now.id_at(at) {
                ids.push(id);
            }
        }
        let mut stored: Vec<(&Id, Option<&str>)> = Vec::with_capacity(was.count());
        for at in 0..was.count() {
            if let Some(id) = was.id_at(at) {
                stored.push((id, was.order_at(at)));
            }
        }
        let item = desc.item;
        store_entries_changed(
            doc,
            slot,
            &ids,
            &stored,
            dirty,
            &EntryWrites {
                same: &|at_now, at_was| match (now.item_at(at_now), was.item_at(at_was)) {
                    (Some(now), Some(was)) => {
                        one_item(now, was) || (item.ops.stores_as)(item, now, was)
                    }
                    _ => false,
                },
                store: &|at_now, doc, slot| match now.item_at(at_now) {
                    Some(value) => (item.ops.store)(item, value, doc, slot),
                    None => Ok(()),
                },
                changed: &|at_now, at_was, doc, entry, dirty| match (
                    now.item_at(at_now),
                    was.item_at(at_was),
                ) {
                    (Some(value), Some(before)) => crate::table::write_key_changed(
                        item,
                        (value, before),
                        doc,
                        entry,
                        VALUE,
                        dirty,
                    ),
                    _ => Ok(()),
                },
                whole: &|doc, slot| (COLLECTION_OPS.store)(table, value, doc, slot),
                order_only: order_only(item),
            },
        )
    },
    refresh: |table, target, doc, value, id, delta| {
        refresh_entries(listed(table)?, table, target, doc, (value, id), delta)
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, cx| {
        let desc = listed(table)?;
        let item = desc.item;
        edit_entries(
            doc,
            slot,
            path,
            op,
            cx,
            &EntryEdits {
                item: &|doc, slot, path, op, cx| (item.ops.edit)(item, doc, slot, path, op, cx),
                parse: &|value| {
                    let mut staged = (desc.item_staged.0)();
                    (item.ops.from_json)(item, value, staged.as_mut())?;
                    let made =
                        (desc.item_staged.1)(staged.as_ref()).ok_or_else(crate::table::mismatch)?;
                    (item.ops.check)(item, made).map_err(ModelError::refused)?;
                    Ok((
                        None,
                        Box::new(StagedItem {
                            table: item,
                            staged,
                            unstage: desc.item_staged.1,
                        }) as Box<dyn Stores + '_>,
                    ))
                },
                whole: &|doc, slot, path, op| {
                    let staged = ((desc.staged.0)(), desc.staged.1);
                    crate::table::edit_whole(table, staged, false, doc, slot, path, op)
                },
                order_only: order_only(item),
            },
        )
    },
    diff: |table, before, after, path, out| {
        let Some(desc) = table.collection() else {
            return;
        };
        let (Some(was), Some(now)) = ((desc.entries)(before), (desc.entries)(after)) else {
            return;
        };
        diff_entries(desc.item, (was, now), path, out);
    },
    collect_text: |table, value, out| {
        if let Some(desc) = table.collection()
            && let Some(entries) = (desc.entries)(value)
        {
            for at in 0..entries.count() {
                if let Some(item) = entries.item_at(at) {
                    (desc.item.ops.collect_text)(desc.item, item, out);
                }
            }
        }
    },
    check: |table, value| {
        let Some(desc) = table.collection() else {
            return Ok(());
        };
        if !desc.item.checks {
            return Ok(());
        }
        if let Some(entries) = (desc.entries)(value) {
            for at in 0..entries.count() {
                if let Some(item) = entries.item_at(at) {
                    (desc.item.ops.check)(desc.item, item)?;
                }
            }
        }
        Ok(())
    },
    get_at: |table, value, path| match path.split_first() {
        None => Some(value),
        Some((Segment::Item(id), rest)) => {
            let desc = table.collection()?;
            let entries = (desc.entries)(value)?;
            let item = entries.item_at(entries.find(id)?)?;
            (desc.item.ops.get_at)(desc.item, item, rest)
        }
        Some(_) => None,
    },
    label: crate::table::no_label,
    nested_collections: crate::table::no_nested,
    op_coalesces: |table, path, previous, next| match path.split_first() {
        Some((Segment::Item(_), rest)) if table.has_ops => {
            let item = table.collection()?.item;
            (item.ops.op_coalesces)(item, rest, previous, next)
        }
        _ => None,
    },
    op_continuable: |table, path, op| match path.split_first() {
        Some((Segment::Item(_), rest)) if table.has_ops => {
            let item = table.collection()?.item;
            (item.ops.op_continuable)(item, rest, op)
        }
        _ => None,
    },
    op_describe: |table, path, op| match path.split_first() {
        Some((Segment::Item(_), rest)) if table.has_ops => {
            let item = table.collection()?.item;
            (item.ops.op_describe)(item, rest, op)
        }
        _ => None,
    },
    op_lands: |table, value, path, op| match path.split_first() {
        Some((Segment::Item(id), rest)) if table.has_ops => {
            let desc = table.collection()?;
            let entries = (desc.entries)(value)?;
            let item = entries.item_at(entries.find(id)?)?;
            (desc.item.ops.op_lands)(desc.item, item, rest, op)
        }
        _ => None,
    },
    op_schemas: |table, prefix, out| {
        if table.has_ops
            && let Some(desc) = table.collection()
        {
            (desc.item.ops.op_schemas)(desc.item, &format!("{prefix}*."), out);
        }
    },
};

/// Whether two items are one item, which two clones of a collection still
/// share: it stores as it is without being compared, however its fields
/// compare (a value stored as JSON text compares only by writing it).
fn one_item(item: &dyn Any, other: &dyn Any) -> bool {
    std::ptr::addr_eq(item, other)
}

/// Whether every entry has a stored order key, each above the one before:
/// the keys a write keeps all of.
fn strictly_ordered_entries(entries: &dyn Entries) -> bool {
    let mut previous: Option<&str> = None;
    for at in 0..entries.count() {
        let Some(key) = entries.order_at(at) else {
            return false;
        };
        if let Some(previous) = previous
            && previous >= key
        {
            return false;
        }
        previous = Some(key);
    }
    true
}

/// The field edits that turn the entries `was` into `now` at `path`.
fn diff_entries(
    item: &ModelTable,
    (was, now): (&dyn Entries, &dyn Entries),
    path: &[Segment],
    out: &mut Vec<crate::Edit<()>>,
) {
    for at in 0..was.count() {
        let (Some(id), Some(before)) = (was.id_at(at), was.item_at(at)) else {
            continue;
        };
        match now.find(id).and_then(|position| now.item_at(position)) {
            // One item, which both still share: no edit.
            Some(next) if one_item(before, next) => {}
            Some(next) => {
                let mut path = path.to_vec();
                path.push(Segment::Item(id.clone()));
                (item.ops.diff)(item, before, next, &path, out);
            }
            None => out.push(crate::Edit::new(
                path.to_vec(),
                EditOp::RemoveItem { id: id.clone() },
            )),
        }
    }
    for index in 0..now.count() {
        let (Some(id), Some(next)) = (now.id_at(index), now.item_at(index)) else {
            continue;
        };
        match was.find(id) {
            Some(position) => {
                if position != index {
                    out.push(crate::Edit::new(
                        path.to_vec(),
                        EditOp::MoveTo {
                            id: id.clone(),
                            at: index,
                        },
                    ));
                }
            }
            None => out.push(crate::Edit::new(
                path.to_vec(),
                EditOp::InsertItem {
                    id: Some(id.clone()),
                    at: index,
                    value: (item.ops.to_json)(item, next),
                },
            )),
        }
    }
}

/// [`Model::refresh`] of a collection: read anew when changed all over or
/// edited in memory since it was read, else only the entries `delta`
/// touched.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the touched entries: read, place, order and quarantine share its state"
)]
fn refresh_entries(
    desc: &CollectionDesc,
    table: &ModelTable,
    target: &mut dyn Any,
    doc: &Doc,
    (value, id): (&Value<'_>, &ObjId),
    delta: &Delta,
) -> Result<(), ModelError> {
    let touched = delta.under(id);
    let unkeyed = {
        let entries = (desc.entries)(target).ok_or_else(crate::table::mismatch)?;
        (0..entries.count()).any(|at| entries.order_at(at).is_none())
    };
    // A collection edited in memory since it was read no longer knows
    // every stored order key; read it again rather than guess.
    if touched.is_all() || unkeyed {
        return (COLLECTION_OPS.reload)(table, target, doc, Some((value, id)));
    }
    let item = desc.item;
    let entries = (desc.entries_mut)(target).ok_or_else(crate::table::mismatch)?;
    let mut reorder = false;
    for key in touched.keys() {
        let item_id = Id(key.to_owned());
        let position = entries.find(&item_id);
        let below = touched.key(key) == Touch::Below && position.is_some();
        let touched_now = match touched_entry(doc, id, key, delta, below) {
            Ok(touched_now) => touched_now,
            Err(error) if quarantines(&error) => {
                if let Some(position) = position {
                    entries.remove_at(position);
                    entries.changed();
                }
                entries.quarantine(item_id, error.to_string());
                continue;
            }
            Err(error) => return Err(error),
        };
        match touched_now {
            Touched::Gone => {
                entries.release(&item_id);
                if let Some(position) = position {
                    entries.remove_at(position);
                    entries.changed();
                }
            }
            // Something inside an item already read: its order key, its value.
            Touched::Below { entry, order } => {
                if let Some(position) = position {
                    if order.is_some() {
                        entries.set_order(position, order);
                        reorder = true;
                    }
                    let refreshed = match entries.item_at_mut(position) {
                        Some(existing) => {
                            crate::table::refresh_slot(item, existing, doc, &entry, VALUE, delta)
                                .located(key)
                        }
                        None => Ok(()),
                    };
                    match refreshed {
                        Ok(()) => {}
                        // It no longer reads: out of the items, held for repair.
                        Err(error) if quarantines(&error) => {
                            entries.remove_at(position);
                            entries.changed();
                            entries.quarantine(item_id, error.to_string());
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            // An item stored as its order key: new, or moved.
            Touched::Order { order } => {
                if let Some(position) = position {
                    entries.set_order(position, Some(order));
                } else {
                    entries.push_filled(
                        (item_id, Some(order), false),
                        &mut |into: &mut dyn Any| {
                            crate::table::load_slot(item, doc, None, into)
                                .located(VALUE)
                                .located(key)
                        },
                    )?;
                    entries.changed();
                }
                reorder = true;
            }
            // A new item, or one whose entry was replaced.
            Touched::Fresh { entry, order } => {
                let slot = crate::model::get_slot(doc, &entry, VALUE).located(key)?;
                let mut merged = false;
                let mut fill = |into: &mut dyn Any| -> Result<(), ModelError> {
                    let slot = slot.as_ref().map(|(v, i)| (v, i));
                    merged = fill_item(item, doc, (id, key), slot, into)
                        .located(VALUE)
                        .located(key)?;
                    Ok(())
                };
                let read = match position {
                    Some(position) => entries.replace_filled(position, Some(order), &mut fill),
                    None => entries.push_filled((item_id.clone(), Some(order), false), &mut fill),
                };
                match read {
                    Ok(()) => {
                        entries.release(&item_id);
                        let at = if position.is_some() {
                            position
                        } else {
                            entries.changed();
                            entries.count().checked_sub(1)
                        };
                        if merged && let Some(at) = at {
                            entries.set_order(at, None);
                        }
                    }
                    // It does not read: out of the items, held for repair.
                    Err(error) if quarantines(&error) => {
                        if let Some(position) = position {
                            entries.remove_at(position);
                            entries.changed();
                        }
                        entries.quarantine(item_id, error.to_string());
                    }
                    Err(error) => return Err(error),
                }
                reorder = true;
            }
        }
    }
    // A moved order key or a new entry: sorting may move items.
    if reorder {
        entries.reorder();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::write_key;
    use automerge::ROOT;
    use tilo_automerge::ApplyIncoming;

    fn ids(collection: &Collection<String>) -> Vec<String> {
        collection.values().cloned().collect()
    }

    /// The collection read key by key: the oracle the one-walk read
    /// must equal, order keys included.
    fn read_key_by_key<T: Model>(doc: &Doc, map: &ObjId) -> Vec<(Id, Option<String>, T)> {
        let mut entries = Vec::new();
        for key in doc.keys(map) {
            let Some((Value::Object(ObjType::Map), entry)) = doc.get(map, key.as_str()).unwrap()
            else {
                continue;
            };
            let order: crate::Atom = read_key(doc, &entry, ORDER).unwrap();
            let item: T = read_key(doc, &entry, VALUE).unwrap();
            entries.push((Id(key), Some(order.0), item));
        }
        entries.compact_sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        entries
    }

    #[test]
    fn appended_items_keep_short_order_keys() {
        // One push per action, as a list someone adds to item by item, and a
        // bulk push: either way the stored keys stay a few characters long
        // (they grew by one character every ~6 items).
        let mut doc = Doc::new();
        let mut list: Collection<u32> = Collection::new();
        for n in 0..300_u32 {
            list.push(Id::from_raw(format!("one{n:05}")), n);
            write_key(&mut doc, &ROOT, "items", &list).unwrap();
            list = read_key(&doc, &ROOT, "items").unwrap();
        }
        for n in 0..10_000_u32 {
            list.push(Id::from_raw(format!("bulk{n:05}")), n);
        }
        write_key(&mut doc, &ROOT, "items", &list).unwrap();
        let Some((_, map)) = doc.get(&ROOT, "items").unwrap() else {
            panic!("the collection is stored");
        };
        let stored = stored_entries(&doc, &map);
        assert_eq!(stored.len(), 10_300);
        let longest = stored.iter().map(|entry| entry.key.len()).max().unwrap();
        assert!(longest <= 5, "longest order key: {longest} characters");
        let read: Collection<u32> = read_key(&doc, &ROOT, "items").unwrap();
        assert!(read.values().copied().eq((0..300).chain(0..10_000)));
    }

    #[test]
    fn one_walk_reads_what_key_by_key_reads() {
        let mut doc = Doc::new();
        let mut list: Collection<Vec<i64>> = Collection::new();
        for n in 0..40 {
            list.push(Id::from_raw(format!("i{n:02}")), vec![n]);
        }
        write_key(&mut doc, &ROOT, "items", &list).unwrap();
        doc.commit();
        // History under every key: moved order keys, rewritten values, a
        // removed item, and a stray non-entry value beside the entries.
        for round in 0..3_i64 {
            list.move_to(&Id::from_raw(format!("i{:02}", 39 - round)), 0);
            if let Some(item) = list.get_mut(&Id::from_raw("i05")) {
                item.push(round);
            }
            write_key(&mut doc, &ROOT, "items", &list).unwrap();
            doc.commit();
        }
        list.remove(&Id::from_raw("i07"));
        write_key(&mut doc, &ROOT, "items", &list).unwrap();
        let Some((_, map)) = doc.get(&ROOT, "items").unwrap() else {
            panic!("the collection is stored");
        };
        doc.put(&map, "stray", 1_i64).unwrap();
        doc.commit();
        // Concurrent moves and inserts from two actors: conflicting order keys.
        let mut left = doc.fork();
        let mut right = doc.fork();
        let read: Collection<Vec<i64>> = read_key(&doc, &ROOT, "items").unwrap();
        let mut l = read.clone();
        l.move_to(&Id::from_raw("i10"), 2);
        l.insert(3, Id::from_raw("left"), vec![-1]);
        write_key(&mut left, &ROOT, "items", &l).unwrap();
        let mut r = read;
        r.move_to(&Id::from_raw("i10"), 30);
        r.insert(3, Id::from_raw("right"), vec![-2]);
        write_key(&mut right, &ROOT, "items", &r).unwrap();
        left.merge_incoming(&mut right).unwrap();

        let Some((value, map)) = left.get(&ROOT, "items").unwrap() else {
            panic!("the collection is stored");
        };
        let read = Collection::<Vec<i64>>::load(&left, &value, &map).unwrap();
        let walked: Vec<_> = read
            .items
            .into_iter()
            .map(|entry| (entry.id, entry.key, (*entry.item).clone()))
            .collect();
        assert_eq!(walked, read_key_by_key::<Vec<i64>>(&left, &map));
        assert_eq!(walked.len(), 41);
    }

    #[test]
    fn moves_touch_one_key_and_concurrent_inserts_keep_both() {
        let mut doc = Doc::new();
        let mut list: Collection<String> = Collection::new();
        for (n, word) in ["a", "b", "c", "d"].iter().enumerate() {
            list.push(Id::from_raw(format!("i{n}")), (*word).to_owned());
        }
        write_key(&mut doc, &ROOT, "items", &list).unwrap();
        doc.commit();
        list.move_to(&Id::from_raw("i3"), 0);
        write_key(&mut doc, &ROOT, "items", &list).unwrap();
        // One order key rewritten.
        assert_eq!(doc.pending_ops(), 1);
        doc.commit();
        let read: Collection<String> = read_key(&doc, &ROOT, "items").unwrap();
        assert_eq!(ids(&read), ["d", "a", "b", "c"]);

        let mut left = doc.fork();
        let mut right = doc.fork();
        let mut l = read.clone();
        l.insert(1, Id::from_raw("left"), "L".into());
        write_key(&mut left, &ROOT, "items", &l).unwrap();
        let mut r = read;
        r.insert(1, Id::from_raw("right"), "R".into());
        r.remove(&Id::from_raw("i1"));
        write_key(&mut right, &ROOT, "items", &r).unwrap();
        left.merge_incoming(&mut right).unwrap();
        let merged: Collection<String> = read_key(&left, &ROOT, "items").unwrap();
        let merged = ids(&merged);
        assert_eq!(merged.len(), 5);
        assert_eq!(merged.first().map(String::as_str), Some("d"));
        assert!(merged.contains(&"L".to_owned()) && merged.contains(&"R".to_owned()));
        assert!(!merged.contains(&"b".to_owned()));
    }
}
