//! Items with identity: [`Id`] and the ordered, keyed [`Collection`].

use std::fmt;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{Doc, Model, ModelError, Slot, read_key};
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
#[derive(Clone, Debug, PartialEq)]
pub struct Collection<T> {
    items: Vec<(Id, T)>,
}

impl<T> Default for Collection<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
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

    /// Items in collection order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Id, &T)> + ExactSizeIterator {
        self.items.iter().map(|(id, item)| (id, item))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Id, &mut T)> {
        self.items.iter_mut().map(|(id, item)| (&*id, item))
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter().map(|(_, item)| item)
    }

    pub fn ids(&self) -> impl DoubleEndedIterator<Item = &Id> + ExactSizeIterator {
        self.items.iter().map(|(id, _)| id)
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.position(id).is_some()
    }

    pub fn position(&self, id: &Id) -> Option<usize> {
        self.items.iter().position(|(candidate, _)| candidate == id)
    }

    pub fn get(&self, id: &Id) -> Option<&T> {
        self.items
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, item)| item)
    }

    pub fn get_mut(&mut self, id: &Id) -> Option<&mut T> {
        self.items
            .iter_mut()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, item)| item)
    }

    /// Append an item. Returns its id for chaining.
    pub fn push(&mut self, id: Id, item: T) -> Id {
        self.items.push((id.clone(), item));
        id
    }

    /// Insert an item at `index` (clamped to the end).
    pub fn insert(&mut self, index: usize, id: Id, item: T) -> Id {
        let index = index.min(self.items.len());
        self.items.insert(index, (id.clone(), item));
        id
    }

    pub fn remove(&mut self, id: &Id) -> Option<T> {
        let index = self.position(id)?;
        Some(self.items.remove(index).1)
    }

    /// Move an item to `index` (clamped to the end). Returns whether it exists.
    pub fn move_to(&mut self, id: &Id, index: usize) -> bool {
        let Some(from) = self.position(id) else {
            return false;
        };
        let entry = self.items.remove(from);
        let index = index.min(self.items.len());
        self.items.insert(index, entry);
        true
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Id, &T) -> bool) {
        self.items.retain(|(id, item)| keep(id, item));
    }

    pub fn sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        self.items.sort_by(|(_, a), (_, b)| compare(a, b));
    }
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
            items: iter.into_iter().collect(),
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
                    .map(|item| (stored.id.clone(), item))
                    .map_err(|error| error.at(stored.id.as_str()))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|items| Self { items })
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
        // Keep every order key that is still consistent with the new order
        // (the longest increasing run); mint keys only for new or moved items.
        let existing: Vec<Option<&str>> = self
            .items
            .iter()
            .map(|(id, _)| {
                stored
                    .iter()
                    .find(|entry| &entry.id == id)
                    .map(|entry| entry.key.as_str())
            })
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
        for ((id, item), key) in self.items.iter().zip(keys) {
            let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
            let key = key.unwrap_or_default();
            Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
            item.store(doc, Slot::Key(&entry, VALUE))
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
