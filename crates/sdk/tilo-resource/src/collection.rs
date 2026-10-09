//! Items with identity: [`Id`] and the ordered, keyed [`Collection`].

use std::fmt;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::action::{ActionCx, Refusal};
use crate::delta::{Delta, Touch};
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key};
use crate::order;
use crate::path::{EditOp, Segment, Settable};

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
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.0.clone())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => Ok(Self(text.clone())),
            _ => Err(ModelError::shape("an id")),
        }
    }
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
    /// Each id's position, built on the first lookup after the order
    /// changed, so finding an item by id is constant time.
    index: Index,
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
        self.0
            .get_or_init(|| {
                items
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| (entry.id.clone(), position))
                    .collect()
            })
            .get(id)
            .copied()
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
            index: Index::default(),
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

    /// Items in collection order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Id, &T)> + ExactSizeIterator {
        self.items.iter().map(|entry| (&entry.id, &entry.item))
    }

    /// The item at `position` in collection order.
    pub fn at(&self, position: usize) -> Option<(&Id, &T)> {
        self.items
            .get(position)
            .map(|entry| (&entry.id, &entry.item))
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
        self.index.get(&self.items, id)
    }

    pub fn get(&self, id: &Id) -> Option<&T> {
        let position = self.position(id)?;
        self.items.get(position).map(|entry| &entry.item)
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
        id
    }

    /// Insert an item at `index` (clamped to the end). An id already present
    /// is moved there.
    pub fn insert(&mut self, index: usize, id: Id, item: T) -> Id {
        self.remove(&id);
        let index = index.min(self.items.len());
        self.items.insert(index, Entry::new(id.clone(), item));
        self.index.reset();
        id
    }

    pub fn remove(&mut self, id: &Id) -> Option<T> {
        let index = self.position(id)?;
        self.index.reset();
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
        true
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Id, &T) -> bool) {
        self.items.retain(|entry| keep(&entry.id, &entry.item));
        self.index.reset();
    }

    pub fn sort_by(&mut self, mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering) {
        self.items.sort_by(|a, b| compare(&a.item, &b.item));
        for entry in &mut self.items {
            entry.key = None;
        }
        self.index.reset();
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
        let mut collection = Self::default();
        for (id, item) in iter {
            collection.push(id, item);
        }
        collection
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

/// An order key placing an item at `at` among `stored` (which may hold the
/// item itself, `moving`, which does not count).
fn key_at(stored: &[Stored], at: usize, moving: Option<&Id>) -> String {
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

impl<T: Model> Model for Collection<T> {
    const SHAPE: crate::Shape = crate::Shape::Collection(&T::SHAPE);
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) => T::op_continuable(rest, op),
            _ => None,
        }
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        match path.split_first() {
            Some((Segment::Item(_), rest)) => T::op_describe(rest, op),
            _ => None,
        }
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        match path.split_first() {
            Some((Segment::Item(id), rest)) => self.get(id)?.op_lands(rest, op),
            _ => None,
        }
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        T::op_schemas(&format!("{prefix}*."), out);
    }

    /// `[{ "id": .., "value": .. }, ..]` in collection order.
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.items
                .iter()
                .map(|entry| serde_json::json!({ "id": entry.id.as_str(), "value": entry.item.to_json() }))
                .collect(),
        )
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let serde_json::Value::Array(entries) = value else {
            return Err(ModelError::shape("a list of items"));
        };
        let mut collection = Self::default();
        for (index, entry) in entries.iter().enumerate() {
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
            let item = T::from_json(entry.get("value").unwrap_or(&serde_json::Value::Null))
                .map_err(|error| error.at(id.as_str()))?;
            collection.push(id, item);
        }
        Ok(collection)
    }

    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match path.split_first() {
            Some((Segment::Item(id), rest)) => {
                let map = slot.object(doc, ObjType::Map)?;
                let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? else {
                    return Err(ModelError::invalid(format!("no item `{id}`")));
                };
                T::edit(doc, Slot::Key(&entry, VALUE), rest, op, cx)
                    .map_err(|error| error.at(id.as_str()))
            }
            Some(_) => edit_whole::<Self>(doc, slot, path, op),
            None => match op {
                EditOp::InsertItem { id, at, value } => {
                    let item = T::from_json(value)?;
                    item.check().map_err(ModelError::refused)?;
                    let map = slot.object(doc, ObjType::Map)?;
                    let id = id.clone().unwrap_or_else(|| cx.id());
                    if doc.get(&map, id.as_str())?.is_some() {
                        return Err(ModelError::invalid(format!("an item `{id}` exists")));
                    }
                    let stored = stored_entries(doc, &map)?;
                    let key = key_at(&stored, *at, None);
                    let entry = Slot::Key(&map, id.as_str()).object(doc, ObjType::Map)?;
                    Slot::Key(&entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))?;
                    item.store(doc, Slot::Key(&entry, VALUE))
                        .map_err(|error| error.at(id.as_str()))
                }
                EditOp::RemoveItem { id } => {
                    let map = slot.object(doc, ObjType::Map)?;
                    if doc.get(&map, id.as_str())?.is_none() {
                        return Err(ModelError::invalid(format!("no item `{id}`")));
                    }
                    doc.delete(&map, id.as_str())?;
                    Ok(())
                }
                EditOp::MoveTo { id, at } => {
                    let map = slot.object(doc, ObjType::Map)?;
                    let stored = stored_entries(doc, &map)?;
                    let Some(entry) = stored.iter().find(|entry| &entry.id == id) else {
                        return Err(ModelError::invalid(format!("no item `{id}`")));
                    };
                    let key = key_at(&stored, *at, Some(id));
                    Slot::Key(&entry.entry, ORDER).put_scalar(doc, ScalarValue::Str(key.into()))
                }
                _ => edit_whole::<Self>(doc, slot, path, op),
            },
        }
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
        for (index, (id, next)) in after.iter().enumerate() {
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
                index: Index::default(),
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
                    self.index.reset();
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
                self.index.reset();
            }
            reorder = true;
        }
        if reorder {
            self.items
                .sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
            self.index.reset();
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
        // Keep every order key that is still consistent with the new order
        // (the longest increasing run); mint keys only for new or moved items.
        let by_id: std::collections::HashMap<&Id, &str> = stored
            .iter()
            .map(|entry| (&entry.id, entry.key.as_str()))
            .collect();
        let existing: Vec<Option<&str>> = self
            .items
            .iter()
            .map(|entry| by_id.get(&entry.id).copied())
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
        for (Entry { id, item, .. }, key) in self.items.iter().zip(keys) {
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
