//! A list someone arranges, of items that carry their own ids: [`Ordered`].
//!
//! [`Collection`](crate::Collection) keeps each item's id beside it, which
//! is right when the id is only the collection's business. Items that carry
//! their id themselves ([`Identified`]) — an effect in a clip's stack, a step
//! in a recipe — read best as a plain vector, so `Ordered<T>` holds them as a
//! `Vec<T>` in memory and stores them exactly as a `Collection` is stored:
//! a map from id to `{ k: order key, v: value }`. Editing an item writes only
//! that item, a move rewrites one order key, concurrent inserts interleave
//! deterministically and a delete never disturbs another item's identity —
//! none of which a list stored by position can promise.

use std::ops::{Deref, DerefMut};

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::action::{ActionCx, Refusal};
use crate::collection::{Id, ORDER, VALUE, key_at, store_in_order, stored_entries};
use crate::keyed::Identified;
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key};
use crate::path::{Edit, EditOp, Path, Segment};

/// Items in the order they were arranged, stored under the ids they carry.
/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Ordered<T> {
    items: Vec<T>,
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> Deref for Ordered<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.items
    }
}

impl<T> DerefMut for Ordered<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.items
    }
}

impl<T> From<Vec<T>> for Ordered<T> {
    fn from(items: Vec<T>) -> Self {
        Self { items }
    }
}

impl<T> FromIterator<T> for Ordered<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self {
            items: iter.into_iter().collect(),
        }
    }
}

impl<T> IntoIterator for Ordered<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Ordered<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Ordered<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

impl<T: serde::Serialize> serde::Serialize for Ordered<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.items.serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Ordered<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(|items| Self { items })
    }
}

impl<T: crate::JsonSchema> crate::JsonSchema for Ordered<T> {
    fn json_schema() -> serde_json::Value {
        crate::schema::array(T::json_schema())
    }
}

impl<T: Identified> Ordered<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the item `id` is.
    pub fn position_of(&self, id: &Id) -> Option<usize> {
        self.items.iter().position(|item| &item.id() == id)
    }

    /// The item `id`.
    pub fn find(&self, id: &Id) -> Option<&T> {
        self.items.iter().find(|item| &item.id() == id)
    }

    /// The item `id`, to change.
    pub fn find_mut(&mut self, id: &Id) -> Option<&mut T> {
        self.items.iter_mut().find(|item| &item.id() == id)
    }

    /// Remove the item `id`.
    pub fn take(&mut self, id: &Id) -> Option<T> {
        let at = self.position_of(id)?;
        Some(self.items.remove(at))
    }
}

impl<R, T: Model + Identified> Path<R, Ordered<T>> {
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

    /// Add `item` under its own id at position `at`.
    #[must_use]
    pub fn insert(&self, at: usize, item: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: Some(item.id()),
            at,
            value: item.to_json(),
        })
    }

    /// Remove the item `id`.
    #[must_use]
    pub fn remove(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveItem { id: id.clone() })
    }

    /// Move the item `id` to position `at`.
    #[must_use]
    pub fn move_to(&self, id: &Id, at: usize) -> Edit<R> {
        self.edit(EditOp::MoveTo { id: id.clone(), at })
    }
}

impl<T: Model + Identified> Model for Ordered<T> {
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema()})
    }
    const SHAPE: crate::Shape = crate::Shape::Collection(&T::SHAPE);
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();

    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.items.iter().map(Model::to_json).collect())
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Array(items) => items
                .iter()
                .map(T::from_json)
                .collect::<Result<Vec<_>, _>>()
                .map(|items| Self { items }),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(ModelError::shape("a list of items")),
        }
    }

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("arranged items"));
        }
        stored_entries(doc, id)?
            .into_iter()
            .map(|stored| {
                read_key::<T>(doc, &stored.entry, VALUE)
                    .map_err(|error| error.at(stored.id.as_str()))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|items| Self { items })
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let ids: Vec<Id> = self.items.iter().map(Identified::id).collect();
        let items: Vec<(&Id, &T)> = ids.iter().zip(&self.items).collect();
        store_in_order(doc, slot, &items)
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
                EditOp::InsertItem { at, value, .. } => {
                    let item = T::from_json(value)?;
                    item.check().map_err(ModelError::refused)?;
                    let map = slot.object(doc, ObjType::Map)?;
                    let id = item.id();
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

    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<Edit<R>>) {
        for item in &self.items {
            let id = item.id();
            match after.find(&id) {
                Some(next) => {
                    let mut at = path.to_vec();
                    at.push(Segment::Item(id));
                    item.diff(next, &at, out);
                }
                None => out.push(Edit::new(path.to_vec(), EditOp::RemoveItem { id })),
            }
        }
        // Walk `after` against the order the edits so far leave: what
        // survived of `self`, then each item put where `after` has it.
        let mut order: Vec<Id> = self
            .items
            .iter()
            .map(Identified::id)
            .filter(|id| after.find(id).is_some())
            .collect();
        for (index, item) in after.items.iter().enumerate() {
            let id = item.id();
            match order.iter().position(|candidate| candidate == &id) {
                Some(now) if now == index => {}
                Some(now) => {
                    order.remove(now);
                    order.insert(index.min(order.len()), id.clone());
                    out.push(Edit::new(path.to_vec(), EditOp::MoveTo { id, at: index }));
                }
                None => {
                    order.insert(index.min(order.len()), id.clone());
                    out.push(Edit::new(
                        path.to_vec(),
                        EditOp::InsertItem {
                            id: Some(id),
                            at: index,
                            value: item.to_json(),
                        },
                    ));
                }
            }
        }
    }

    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.items.iter().try_for_each(Model::check)?;
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
            Some((Segment::Item(id), rest)) => self.find(id)?.op_lands(rest, op),
            _ => None,
        }
    }

    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        T::op_schemas(&format!("{prefix}*."), out);
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        for item in &self.items {
            item.collect_text(out);
        }
    }
}
