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

use std::cmp::Ordering;
use std::ops::{Deref, DerefMut};

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, Value};

use crate::action::{ActionCx, Refusal};
use crate::collection::Id;
use crate::delta::Delta;
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key};
use crate::path::{Edit, EditOp, Path, Segment};

/// An item of a [`Keyed`]: what it is stored under, and how items sort in
/// memory beyond that.
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

/// Items stored under the ids they carry. See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyed<T> {
    items: Vec<T>,
}

impl<T> Default for Keyed<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T> Deref for Keyed<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.items
    }
}

impl<T> DerefMut for Keyed<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.items
    }
}

impl<T> From<Vec<T>> for Keyed<T> {
    fn from(items: Vec<T>) -> Self {
        Self { items }
    }
}

impl<T> FromIterator<T> for Keyed<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self {
            items: iter.into_iter().collect(),
        }
    }
}

impl<T> IntoIterator for Keyed<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Keyed<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Keyed<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

impl<T: serde::Serialize> serde::Serialize for Keyed<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.items.serialize(serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Keyed<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(|items| Self { items })
    }
}

impl<T: crate::JsonSchema> crate::JsonSchema for Keyed<T> {
    fn json_schema() -> serde_json::Value {
        crate::schema::array(T::json_schema())
    }
}

impl<T: Identified> Keyed<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let at = self.items.iter().position(|item| &item.id() == id)?;
        Some(self.items.remove(at))
    }

    /// Sort as a read from the document does: by id, then [`Identified::order`].
    pub fn settle(&mut self) {
        self.items
            .sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
        self.items.sort_by(T::order);
    }
}

impl<R, T: Model + Identified> Path<R, Keyed<T>> {
    /// The item `id`, its own paths rooted here.
    #[must_use]
    pub fn item(&self, id: &Id) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Item(id.clone())))
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

    /// Remove the item `id`.
    #[must_use]
    pub fn remove(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveItem { id: id.clone() })
    }
}

fn entries<T: Model>(doc: &Doc, map: &ObjId) -> Result<Vec<T>, ModelError> {
    doc.keys(map)
        .map(|key| read_key::<T>(doc, map, &key))
        .collect()
}

impl<T: Model + Identified> Model for Keyed<T> {
    const SHAPE: crate::Shape = crate::Shape::Keyed(&T::SHAPE);
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
            return Err(ModelError::shape("keyed items"));
        }
        let mut items = Self {
            items: entries(doc, id)?,
        };
        items.items.sort_by(T::order);
        Ok(items)
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let keys: std::collections::HashSet<String> = self
            .items
            .iter()
            .map(|item| item.id().as_str().to_owned())
            .collect();
        let stale: Vec<String> = doc.keys(&map).filter(|key| !keys.contains(key)).collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        for item in &self.items {
            let id = item.id();
            item.store(doc, Slot::Key(&map, id.as_str()))
                .map_err(|error| error.at(id.as_str()))?;
        }
        Ok(())
    }

    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let touched = delta.under(id);
        if touched.is_all() {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        let mut changed = false;
        for key in touched.keys() {
            let at = self.items.iter().position(|item| item.id().as_str() == key);
            match (doc.get(id, key)?, at) {
                (None, Some(at)) => {
                    self.items.remove(at);
                    changed = true;
                }
                (None, None) => {}
                (Some(_), Some(at)) => {
                    if let Some(item) = self.items.get_mut(at) {
                        refresh_key(item, doc, id, key, delta)?;
                    }
                    changed = true;
                }
                (Some(_), None) => {
                    self.items.push(read_key(doc, id, key)?);
                    changed = true;
                }
            }
        }
        if changed {
            self.items
                .sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
            self.items.sort_by(T::order);
        }
        Ok(())
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
                if doc.get(&map, id.as_str())?.is_none() {
                    return Err(ModelError::invalid(format!("no item `{id}`")));
                }
                T::edit(doc, Slot::Key(&map, id.as_str()), rest, op, cx)
                    .map_err(|error| error.at(id.as_str()))
            }
            Some(_) => edit_whole::<Self>(doc, slot, path, op),
            None => match op {
                EditOp::InsertItem { value, .. } => {
                    let item = T::from_json(value)?;
                    item.check().map_err(ModelError::refused)?;
                    let map = slot.object(doc, ObjType::Map)?;
                    let id = item.id();
                    if doc.get(&map, id.as_str())?.is_some() {
                        return Err(ModelError::invalid(format!("an item `{id}` exists")));
                    }
                    item.store(doc, Slot::Key(&map, id.as_str()))
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
                _ => edit_whole::<Self>(doc, slot, path, op),
            },
        }
    }

    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<Edit<R>>) {
        for item in &self.items {
            let id = item.id();
            match after.items.iter().find(|candidate| candidate.id() == id) {
                Some(next) => {
                    let mut at = path.to_vec();
                    at.push(Segment::Item(id));
                    item.diff(next, &at, out);
                }
                None => out.push(Edit::new(path.to_vec(), EditOp::RemoveItem { id })),
            }
        }
        for item in &after.items {
            let id = item.id();
            if !self.items.iter().any(|candidate| candidate.id() == id) {
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
            Some((Segment::Item(id), rest)) => self
                .items
                .iter()
                .find(|item| &item.id() == id)?
                .get_at(rest),
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
            Some((Segment::Item(id), rest)) => self
                .items
                .iter()
                .find(|item| &item.id() == id)?
                .op_lands(rest, op),
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
