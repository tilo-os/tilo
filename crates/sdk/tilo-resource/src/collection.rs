//! Items with identity: [`Id`] and the ordered, keyed [`Collection`].

use std::collections::{HashMap, HashSet};
use std::fmt;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::delta::{Delta, Touch};
use crate::model::{Doc, Model, ModelError, Slot, read_key, refresh_key, write_key_changed};
use crate::order;

/// A stable identity for an item in a resource. Minted deterministically by
/// [`ActionCx::id`](crate::ActionCx::id), so replaying an action on another
/// replica mints the same id.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
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

impl std::str::FromStr for Id {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl Model for Id {
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match value {
            Value::Scalar(scalar) => match scalar.as_ref() {
                ScalarValue::Str(text) => Ok(Self(text.to_string())),
                _ => Err(ModelError::shape("an id")),
            },
            Value::Object(_) => Err(ModelError::shape("an id")),
        }
    }
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
        self.items.len() == other.items.len()
            && self
                .items
                .iter()
                .zip(&other.items)
                .all(|(a, b)| a.id == b.id && a.item == b.item)
    }
}

impl<T> Default for Collection<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
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
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Id, &T)> + ExactSizeIterator {
        self.items.iter().map(|entry| (&entry.id, &entry.item))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Id, &mut T)> {
        self.items
            .iter_mut()
            .map(|entry| (&entry.id, &mut entry.item))
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter().map(|entry| &entry.item)
    }

    pub fn ids(&self) -> impl DoubleEndedIterator<Item = &Id> + ExactSizeIterator {
        self.items.iter().map(|entry| &entry.id)
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.position(id).is_some()
    }

    pub fn position(&self, id: &Id) -> Option<usize> {
        self.items.iter().position(|entry| &entry.id == id)
    }

    pub fn get(&self, id: &Id) -> Option<&T> {
        self.items
            .iter()
            .find(|entry| &entry.id == id)
            .map(|entry| &entry.item)
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
        self.items
            .iter_mut()
            .find(|entry| &entry.id == id)
            .map(|entry| &mut entry.item)
    }

    /// Append an item. Returns its id for chaining.
    pub fn push(&mut self, id: Id, item: T) -> Id {
        self.items.push(Entry::new(id.clone(), item));
        self.moved();
        id
    }

    /// Insert an item at `index` (clamped to the end).
    pub fn insert(&mut self, index: usize, id: Id, item: T) -> Id {
        let index = index.min(self.items.len());
        self.items.insert(index, Entry::new(id.clone(), item));
        self.moved();
        id
    }

    pub fn remove(&mut self, id: &Id) -> Option<T> {
        let index = self.position(id)?;
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
        self.moved();
        true
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Id, &T) -> bool) {
        let before = self.items.len();
        self.items.retain(|entry| keep(&entry.id, &entry.item));
        // Retaining only removes: the same length is the same ids.
        if self.items.len() != before {
            self.moved();
        }
    }

    pub fn sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        self.items.sort_by(|a, b| compare(&a.item, &b.item));
        for entry in &mut self.items {
            entry.key = None;
        }
        self.moved();
    }
}

impl<T> Collection<T> {
    /// The order key each item is stored under, given the keys `stored`
    /// holds: every stored key still consistent with the order (the longest
    /// increasing run) is kept; new and moved items get minted keys.
    fn order_keys(&self, stored: &HashMap<&Id, &str>) -> Vec<String> {
        let existing: Vec<Option<&str>> = self
            .items
            .iter()
            .map(|entry| stored.get(&entry.id).copied())
            .collect();
        let kept = longest_increasing(&existing);
        let mut keys: Vec<Option<String>> = vec![None; self.items.len()];
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
            let minted =
                order::sequence(low.as_deref(), high.as_deref(), index.saturating_sub(start));
            for (offset, key) in minted.into_iter().enumerate() {
                if let Some(slot) = keys.get_mut(start.saturating_add(offset)) {
                    *slot = Some(key);
                }
            }
        }
        keys.into_iter().map(Option::unwrap_or_default).collect()
    }
}

/// Whether every item of a collection read from a document has a stored
/// order key, each above the one before: the keys a write keeps all of.
fn strictly_ordered<T>(collection: &Collection<T>) -> bool {
    collection.items.iter().all(|entry| entry.key.is_some())
        && collection
            .items
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a.key < b.key))
}

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
        Self {
            items: iter
                .into_iter()
                .map(|(id, item)| Entry::new(id, item))
                .collect(),
            membership: Membership::fresh(),
        }
    }
}

struct Stored {
    id: Id,
    key: String,
    entry: ObjId,
}

fn stored_entries(doc: &Doc, map: &ObjId) -> Result<Vec<Stored>, ModelError> {
    let mut entries = Vec::new();
    for key in doc.keys(map) {
        let Some((Value::Object(ObjType::Map), entry)) = doc.get(map, key.as_str())? else {
            continue;
        };
        let order: crate::Atom = read_key(doc, &entry, ORDER)?;
        entries.push(Stored {
            id: Id(key),
            key: order.0,
            entry,
        });
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
    Ok(entries)
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

impl<T: Model> Model for Collection<T> {
    const SHAPE: crate::Shape = crate::Shape::Collection(&T::SHAPE);

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a collection"));
        }
        stored_entries(doc, id)?
            .into_iter()
            .map(|stored| {
                read_key::<T>(doc, &stored.entry, VALUE)
                    .map(|item| Entry {
                        id: stored.id.clone(),
                        key: Some(stored.key),
                        item,
                    })
                    .map_err(|error| error.at(stored.id.as_str()))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|items| Self {
                items,
                membership: Membership::fresh(),
            })
    }

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
        if touched.is_all() || self.items.iter().any(|entry| entry.key.is_none()) {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        let mut reorder = false;
        for key in touched.keys() {
            let item_id = Id(key.to_owned());
            let position = self.position(&item_id);
            let Some((Value::Object(ObjType::Map), entry)) = doc.get(id, key)? else {
                if let Some(position) = position {
                    self.items.remove(position);
                    self.moved();
                }
                continue;
            };
            let order = |doc: &Doc| read_key::<crate::Atom>(doc, &entry, ORDER).map(|k| k.0);
            // Something inside an item already read: its order key, its value.
            if touched.key(key) == Touch::Below
                && let Some(existing) = position.and_then(|position| self.items.get_mut(position))
            {
                if delta.key(&entry, ORDER) != Touch::Untouched {
                    existing.key = Some(order(doc).map_err(|error| error.at(key))?);
                    reorder = true;
                }
                refresh_key(&mut existing.item, doc, &entry, VALUE, delta)
                    .map_err(|error| error.at(key))?;
                continue;
            }
            // A new item, or one whose entry was replaced.
            let fresh = Entry {
                id: item_id,
                key: Some(order(doc).map_err(|error| error.at(key))?),
                item: read_key::<T>(doc, &entry, VALUE).map_err(|error| error.at(key))?,
            };
            if let Some(existing) = position.and_then(|position| self.items.get_mut(position)) {
                *existing = fresh;
            } else {
                self.items.push(fresh);
                self.moved();
            }
            reorder = true;
        }
        // A moved order key or a new entry: sorting may move items.
        if reorder {
            self.moved();
            self.items
                .sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
        }
        Ok(())
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let stored = stored_entries(doc, &map)?;
        for entry in &stored {
            if !self.contains(&entry.id) {
                doc.delete(&map, entry.id.as_str())?;
            }
        }
        let by_id: HashMap<&Id, &str> = stored
            .iter()
            .map(|entry| (&entry.id, entry.key.as_str()))
            .collect();
        for (Entry { id, item, .. }, key) in self.items.iter().zip(self.order_keys(&by_id)) {
            let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
            Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
            item.store(doc, Slot::Key(&entry, VALUE))
                .map_err(|error| error.at(id.as_str()))?;
        }
        Ok(())
    }

    /// Equal ids in equal order, each item storing as before, over order
    /// keys a write would keep.
    fn stores_as(&self, before: &Self) -> bool {
        self.items.len() == before.items.len()
            && strictly_ordered(before)
            && self
                .items
                .iter()
                .zip(&before.items)
                .all(|(entry, was)| entry.id == was.id && entry.item.stores_as(&was.item))
    }

    /// What [`store`](Model::store) writes, from `before` (which knows every
    /// stored entry and order key) instead of listing the stored entries:
    /// only new, moved and changed items are visited, and an item someone
    /// else touched (`dirty`) is written as `store` writes it.
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let current: HashSet<&Id> = self.items.iter().map(|entry| &entry.id).collect();
        // `before` read from a document knows each item's stored order key;
        // with one missing, or an id twice over, only the document can say.
        if current.len() != self.items.len() || before.items.iter().any(|e| e.key.is_none()) {
            return self.store(doc, slot);
        }
        let map = slot.object(doc, ObjType::Map)?;
        // `before` is in stored order, the order `store` deletes in.
        for entry in &before.items {
            if !current.contains(&entry.id) {
                doc.delete(&map, entry.id.as_str())?;
            }
        }
        let stored: HashMap<&Id, (&str, &T)> = before
            .items
            .iter()
            .filter_map(|entry| Some((&entry.id, (entry.key.as_deref()?, &entry.item))))
            .collect();
        let by_id: HashMap<&Id, &str> = stored.iter().map(|(id, (key, _))| (*id, *key)).collect();
        for (Entry { id, item, .. }, key) in self.items.iter().zip(self.order_keys(&by_id)) {
            let touched = dirty.key(&map, id.as_str());
            let Some(&(stored_key, previous)) =
                stored.get(id).filter(|_| touched != Touch::Written)
            else {
                // New here, or replaced by someone else: written whole.
                let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
                Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
                item.store(doc, Slot::Key(&entry, VALUE))
                    .map_err(|error| error.at(id.as_str()))?;
                continue;
            };
            let kept = touched == Touch::Untouched && stored_key == key;
            if kept && item.stores_as(previous) {
                continue;
            }
            let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
            if !kept {
                Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
            }
            write_key_changed(doc, &entry, VALUE, item, previous, dirty)
                .map_err(|error| error.at(id.as_str()))?;
        }
        Ok(())
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
