//! Items with identity: [`Id`] and the ordered, keyed [`Collection`].

use std::fmt;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::action::{ActionCx, Refusal};
use crate::delta::{Delta, Touch};
use crate::model::LocatedAt as _;
use crate::model::{
    Doc, Model, ModelError, Slot, Stores, edit_whole, read_key, refresh_key, write_key_changed,
};
use crate::order;
use crate::path::{EditOp, Segment, Settable};

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
#[derive(Clone, Debug)]
pub struct Collection<T> {
    items: Vec<Entry<T>>,
    /// Each id's position, built on the first lookup after the order
    /// changed, so finding an item by id is constant time.
    index: Index,
    membership: Membership,
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

#[derive(Clone, Default)]
struct Index(OnceLock<HashMap<Id, usize>>);

impl fmt::Debug for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Index")
    }
}

impl Index {
    fn get<T>(&self, items: &[Entry<T>], id: &Id) -> Option<usize> {
        if self.0.get().is_none() {
            let mut index = HashMap::with_capacity(items.len());
            for position in 0..items.len() {
                if let Some(entry) = items.get(position) {
                    index.insert(entry.id.clone(), position);
                }
            }
            let _ = self.0.set(index);
        }
        self.0.get()?.get(id).copied()
    }

    fn reset(&mut self) {
        self.0 = OnceLock::new();
    }

    /// Note an item appended at `position`, keeping a built index.
    fn appended(&mut self, id: &Id, position: usize) {
        if let Some(index) = self.0.get_mut() {
            index.insert(id.clone(), position);
        }
    }
}

#[derive(Clone, Debug)]
struct Entry<T> {
    id: Id,
    /// The stored order key, when this entry was read from a document and
    /// has not been moved since.
    key: Option<String>,
    item: T,
}

impl<T> Entry<T> {
    fn new(id: Id, item: T) -> Self {
        Self {
            id,
            key: None,
            item,
        }
    }
}

impl<T: PartialEq> PartialEq for Collection<T> {
    /// Equal items in equal order; where each item's order key came from is
    /// bookkeeping, not content.
    fn eq(&self, other: &Self) -> bool {
        if self.items.len() != other.items.len() {
            return false;
        }
        for at in 0..self.items.len() {
            match (self.items.get(at), other.items.get(at)) {
                (Some(a), Some(b)) if a.id == b.id && a.item == b.item => {}
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
            .map(|entry| (&entry.id, &entry.item))
    }

    #[allow(
        clippy::iter_without_into_iter,
        reason = "a collection iterates its items by id; `for` over it would hide which"
    )]
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        IterMut(self.items.iter_mut())
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
        self.items.get(position).map(|entry| &entry.item)
    }

    /// [`get`](Self::get) for a caller that remembers where the item was (a
    /// list row): looks at `near` and beside it first, so an item that
    /// stayed or moved by one is found without a scan. Answers where the
    /// item is now, to remember for next time, and the item.
    pub fn get_near(&self, id: &Id, near: usize) -> Option<(usize, &T)> {
        let around = [Some(near), near.checked_add(1), near.checked_sub(1)];
        around
            .into_iter()
            .flatten()
            .find(|&index| self.items.get(index).is_some_and(|entry| &entry.id == id))
            .or_else(|| self.position(id))
            .and_then(|index| self.items.get(index).map(|entry| (index, &entry.item)))
    }

    pub fn get_mut(&mut self, id: &Id) -> Option<&mut T> {
        let position = self.position(id)?;
        self.items.get_mut(position).map(|entry| &mut entry.item)
    }

    /// Append an item. Returns its id for chaining. An id already present
    /// is replaced in place.
    pub fn push(&mut self, id: Id, item: T) -> Id {
        if let Some(existing) = self.get_mut(&id) {
            *existing = item;
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
        Some(self.items.remove(index).item)
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
        self.items.retain(|entry| keep(&entry.id, &entry.item));
        // Retaining only removes: the same length is the same ids.
        if self.items.len() != before {
            self.index.reset();
            self.moved();
        }
    }

    pub fn sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        self.items.sort_by(|a, b| compare(&a.item, &b.item));
        for entry in &mut self.items {
            entry.key = None;
        }
        self.index.reset();
        self.moved();
    }
}

/// Whether every item of a collection read from a document has a stored
/// order key, each above the one before: the keys a write keeps all of.
fn strictly_ordered<T>(collection: &Collection<T>) -> bool {
    let mut previous: Option<&str> = None;
    for entry in &collection.items {
        let Some(key) = entry.key.as_deref() else {
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

// Named iterators rather than closures over entries: one small set of
// methods per item type, whatever a caller chains onto them.

/// The items of a [`Collection`] with their ids, in order.
pub struct Iter<'a, T>(std::slice::Iter<'a, Entry<T>>);
/// The items of a [`Collection`], to change, with their ids.
pub struct IterMut<'a, T>(std::slice::IterMut<'a, Entry<T>>);
/// The items of a [`Collection`], in order.
pub struct Values<'a, T>(std::slice::Iter<'a, Entry<T>>);
/// The ids of a [`Collection`], in order.
pub struct Ids<'a, T>(std::slice::Iter<'a, Entry<T>>);

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (&'a Id, &'a T);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.0.next()?;
        Some((&entry.id, &entry.item))
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
        Some((&entry.id, &entry.item))
    }
}
impl<T> ExactSizeIterator for Iter<'_, T> {}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = (&'a Id, &'a mut T);
    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.0.next()?;
        Some((&entry.id, &mut entry.item))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<'a, T> Iterator for Values<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        Some(&self.0.next()?.item)
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
        Some(&self.0.next_back()?.item)
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
    pub(crate) entry: ObjId,
    /// What the entry's value slot holds, for reading the item.
    pub(crate) value: crate::model::MapSlot,
}

/// Write `items`, in this order, as the map from id to `{ k, v }` a
/// [`Collection`] is stored as: items no longer there are deleted, every
/// order key still consistent with the new order is kept (the longest
/// increasing run), and keys are minted only for new or moved items.
pub(crate) fn store_in_order(
    doc: &mut Doc,
    slot: Slot<'_>,
    items: &[(&Id, &dyn Stores)],
) -> Result<(), ModelError> {
    let map = slot.object(doc, ObjType::Map)?;
    let stored = stored_entries(doc, &map)?;
    let present: HashSet<&Id> = items.iter().map(|(id, _)| *id).collect();
    for entry in &stored {
        if !present.contains(&entry.id) {
            doc.delete(&map, entry.id.as_str())?;
        }
    }
    let by_id: HashMap<&Id, &str> = stored
        .iter()
        .map(|entry| (&entry.id, entry.key.as_str()))
        .collect();
    let keys = order_keys(items.iter().map(|(id, _)| *id), &by_id);
    for ((id, item), key) in items.iter().zip(keys) {
        let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
        Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
        item.store_to(doc, Slot::Key(&entry, VALUE))
            .located(id.as_str())?;
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
            (edits.item)(doc, Slot::Key(&entry, VALUE), rest, op, cx).located(id.as_str())
        }
        (None, EditOp::InsertItem { id, at, value }) => {
            let (carried, item) = (edits.parse)(value)?;
            let map = slot.object(doc, ObjType::Map)?;
            let id = carried.or_else(|| id.clone()).unwrap_or_else(|| cx.id());
            if doc.get(&map, id.as_str())?.is_some() {
                return Err(ModelError::invalid(format!("an item `{id}` exists")));
            }
            let stored = stored_entries(doc, &map)?;
            let key = key_at(&stored, *at, None);
            let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
            Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
            item.store_to(doc, Slot::Key(&entry, VALUE))
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
        (None, EditOp::MoveTo { id, at }) => {
            let map = slot.object(doc, ObjType::Map)?;
            let stored = stored_entries(doc, &map)?;
            let Some(entry) = stored.iter().find(|entry| &entry.id == id) else {
                return Err(ModelError::invalid(format!("no item `{id}`")));
            };
            let key = key_at(&stored, *at, Some(id));
            Slot::Key(&entry.entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))
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
            let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
            Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
            (writes.store)(now, doc, Slot::Key(&entry, VALUE)).located(id.as_str())?;
            continue;
        };
        let kept = touched == Touch::Untouched && stored_key == key;
        if kept && (writes.same)(now, at) {
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
    let Some((Value::Object(ObjType::Map), entry)) = doc.get(map, key)? else {
        return Ok(Touched::Gone);
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
    for stored in stored_entries(doc, id)? {
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
    order.sort_by(|&a, &b| keys.get(a).cmp(&keys.get(b)));
    order
}

/// The stored entries of the collection map `map`, in order. One walk of
/// the map lists the entries and one walk of each entry reads its order key
/// and value slot: a lookup per key would open each map again.
pub(crate) fn stored_entries(doc: &Doc, map: &ObjId) -> Result<Vec<Stored>, ModelError> {
    let mut entries = Vec::new();
    for item in doc.map_range(map, ..) {
        if item.value != automerge::ValueRef::Object(ObjType::Map) {
            continue;
        }
        let entry = item.id();
        let [order, value] = crate::model::read_slots(doc, &entry, [ORDER, VALUE]);
        let order: crate::Atom = crate::model::load_slot(doc, order, ORDER)?;
        entries.push(Stored {
            id: Id(item.key.into_owned()),
            key: order.0,
            entry,
            value,
        });
    }
    // `(order key, id)` is unique, so an unstable sort orders as a stable one.
    entries.sort_unstable_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
    Ok(entries)
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
    /// Whether an item lost its stored order key (moved in memory).
    fn any_unkeyed(&self) -> bool {
        for entry in &self.items {
            if entry.key.is_none() {
                return true;
            }
        }
        false
    }

    /// Sort the items as stored: by order key, ties by id.
    fn reorder_by_keys(&mut self) {
        let mut keys = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            keys.push((entry.key.as_deref(), &entry.id));
        }
        let order = stored_order(&keys);
        let mut slots: Vec<Option<Entry<T>>> = Vec::with_capacity(self.items.len());
        for entry in std::mem::take(&mut self.items) {
            slots.push(Some(entry));
        }
        for at in order {
            if let Some(entry) = slots.get_mut(at).and_then(Option::take) {
                self.items.push(entry);
            }
        }
    }
}

impl<T: Model> crate::items::ItemSource for Collection<T> {
    fn item_collection(&self, field: &'static str) -> crate::items::ItemCollection {
        crate::items::collection_of(field, self.entries(crate::items::ITEM_PAGE))
    }
    fn item_presence(&self) -> usize {
        usize::from(!self.items.is_empty())
    }
    fn label_of_item(&self, id: &Id) -> Option<String> {
        let item = self.get(id)?;
        Some(crate::__private::item_label(id, item))
    }
    fn item_texts(&self, field: &'static str, found: &mut Vec<crate::items::ItemText>) {
        crate::items::texts_of(field, self.entries(usize::MAX), found);
    }
    fn nested_items(&self, field: &'static str, out: &mut Vec<crate::items::ItemCollection>) {
        crate::items::nested_of(field, self.entries(crate::items::ITEM_PAGE), out);
    }
}

impl<T> Collection<T> {
    /// Each item with its id, in order.
    fn entries(&self, limit: usize) -> Vec<(Id, &T)> {
        let mut out = Vec::with_capacity(self.items.len().min(limit));
        for entry in &self.items {
            if out.len() == limit {
                break;
            }
            out.push((entry.id.clone(), &entry.item));
        }
        out
    }
}

impl<T: Model> Model for Collection<T> {
    fn value_schema() -> serde_json::Value {
        entries_schema(T::value_schema())
    }
    const SHAPE: crate::Shape = crate::Shape::Collection(&T::SHAPE);
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();
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
            Some((Segment::Item(id), rest)) if T::OPS => self.get(id)?.op_lands(rest, op),
            _ => None,
        }
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        if T::OPS {
            T::op_schemas(&format!("{prefix}*."), out);
        }
    }

    /// `[{ "id": .., "value": .. }, ..]` in collection order.
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        let mut entries = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            entries.push(entry_json(&entry.id, entry.item.to_json()));
        }
        serde_json::Value::Array(entries)
    }

    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let serde_json::Value::Array(entries) = value else {
            return Err(ModelError::shape("a list of items"));
        };
        let mut collection = Self::default();
        for (index, entry) in entries.iter().enumerate() {
            let (id, value) = entry_parts(index, entry)?;
            let item = T::from_json(value).located(id.as_str())?;
            collection.push(id, item);
        }
        Ok(collection)
    }

    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        edit_entries(
            doc,
            slot,
            path,
            op,
            cx,
            &EntryEdits {
                item: &T::edit,
                parse: &|value| {
                    let item = T::from_json(value)?;
                    item.check().map_err(ModelError::refused)?;
                    Ok((None, Box::new(item) as Box<dyn Stores + '_>))
                },
                whole: &edit_whole::<Self>,
            },
        )
    }

    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.values().try_for_each(Model::check)?;
        }
        Ok(())
    }

    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        match path.split_first() {
            None => Some(self),
            Some((Segment::Item(id), rest)) => self.get(id)?.get_at(rest),
            Some(_) => None,
        }
    }

    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        for (id, before) in self.iter() {
            match after.get(id) {
                Some(next) => {
                    let mut at = path.to_vec();
                    at.push(Segment::Item(id.clone()));
                    before.diff(next, &at, out);
                }
                None => out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::RemoveItem { id: id.clone() },
                )),
            }
        }
        for index in 0..after.items.len() {
            let Some(entry) = after.items.get(index) else {
                continue;
            };
            let (id, next) = (&entry.id, &entry.item);
            if self.contains(id) {
                if self.position(id) != Some(index) {
                    out.push(crate::Edit::new(
                        path.to_vec(),
                        EditOp::MoveTo {
                            id: id.clone(),
                            at: index,
                        },
                    ));
                }
            } else {
                out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::InsertItem {
                        id: Some(id.clone()),
                        at: index,
                        value: next.to_json(),
                    },
                ));
            }
        }
    }

    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        let mut items = Vec::new();
        load_entries(doc, value, id, &mut |stored| {
            items.push(Entry {
                item: crate::model::load_slot::<T>(doc, stored.value, VALUE)?,
                id: stored.id,
                key: Some(stored.key),
            });
            Ok(())
        })?;
        Ok(Self {
            membership: Membership::fresh(),
            items,
            index: Index::default(),
        })
    }

    #[inline(never)]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let touched = delta.under(id);
        // A collection edited in memory since it was read no longer knows
        // every stored order key; read it again rather than guess.
        if touched.is_all() || self.any_unkeyed() {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        let mut reorder = false;
        for key in touched.keys() {
            let item_id = Id(key.to_owned());
            let position = self.position(&item_id);
            let below = touched.key(key) == Touch::Below && position.is_some();
            match touched_entry(doc, id, key, delta, below)? {
                Touched::Gone => {
                    if let Some(position) = position {
                        self.items.remove(position);
                        self.index.reset();
                        self.moved();
                    }
                }
                // Something inside an item already read: its order key, its value.
                Touched::Below { entry, order } => {
                    if let Some(existing) =
                        position.and_then(|position| self.items.get_mut(position))
                    {
                        if order.is_some() {
                            existing.key = order;
                            reorder = true;
                        }
                        refresh_key(&mut existing.item, doc, &entry, VALUE, delta).located(key)?;
                    }
                }
                // A new item, or one whose entry was replaced.
                Touched::Fresh { entry, order } => {
                    let fresh = Entry {
                        id: item_id,
                        key: Some(order),
                        item: read_key::<T>(doc, &entry, VALUE).located(key)?,
                    };
                    if let Some(existing) =
                        position.and_then(|position| self.items.get_mut(position))
                    {
                        *existing = fresh;
                    } else {
                        self.items.push(fresh);
                        self.index.reset();
                        self.moved();
                    }
                    reorder = true;
                }
            }
        }
        // A moved order key or a new entry: sorting may move items.
        if reorder {
            self.reorder_by_keys();
            self.index.reset();
            self.moved();
        }
        Ok(())
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let mut items: Vec<(&Id, &dyn Stores)> = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            items.push((&entry.id, &entry.item as &dyn Stores));
        }
        store_in_order(doc, slot, &items)
    }

    /// Equal ids in equal order, each item storing as before, over order
    /// keys a write would keep.
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        if self.items.len() != before.items.len() || !strictly_ordered(before) {
            return false;
        }
        for at in 0..self.items.len() {
            match (self.items.get(at), before.items.get(at)) {
                (Some(entry), Some(was))
                    if entry.id == was.id && entry.item.stores_as(&was.item) => {}
                _ => return false,
            }
        }
        true
    }

    /// What [`store`](Model::store) writes, from `before` (which knows every
    /// stored entry and order key) instead of listing the stored entries:
    /// only new, moved and changed items are visited, and an item someone
    /// else touched (`dirty`) is written as `store` writes it.
    #[inline(never)]
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let mut ids: Vec<&Id> = Vec::with_capacity(self.items.len());
        for entry in &self.items {
            ids.push(&entry.id);
        }
        let mut stored: Vec<(&Id, Option<&str>)> = Vec::with_capacity(before.items.len());
        for entry in &before.items {
            stored.push((&entry.id, entry.key.as_deref()));
        }
        store_entries_changed(
            doc,
            slot,
            &ids,
            &stored,
            dirty,
            &EntryWrites {
                same: &|now, was| {
                    matches!(
                        (self.items.get(now), before.items.get(was)),
                        (Some(entry), Some(was)) if entry.item.stores_as(&was.item)
                    )
                },
                store: &|now, doc, slot| match self.items.get(now) {
                    Some(entry) => entry.item.store(doc, slot),
                    None => Ok(()),
                },
                changed: &|now, was, doc, entry, dirty| match (
                    self.items.get(now),
                    before.items.get(was),
                ) {
                    (Some(entry_now), Some(entry_was)) => write_key_changed(
                        doc,
                        entry,
                        VALUE,
                        &entry_now.item,
                        &entry_was.item,
                        dirty,
                    ),
                    _ => Ok(()),
                },
                whole: &|doc, slot| self.store(doc, slot),
            },
        )
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        for item in self.values() {
            item.collect_text(out);
        }
    }
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
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
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
        let stored = stored_entries(&doc, &map).unwrap();
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
            .map(|entry| (entry.id, entry.key, entry.item))
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
