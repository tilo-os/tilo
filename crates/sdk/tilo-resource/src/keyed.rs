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
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, Value};

use crate::action::{ActionCx, Refusal};
use crate::collection::Id;
use crate::delta::Delta;
use crate::model::LocatedAt as _;
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key};
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
    items: Vec<T>,
    storage: PhantomData<fn() -> S>,
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
    fn clone(&self) -> Self {
        Self::from(self.items.clone())
    }
}

impl<T: std::fmt::Debug, S> std::fmt::Debug for Items<T, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.items.fmt(f)
    }
}

impl<T: PartialEq, S> PartialEq for Items<T, S> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl<T, S> Default for Items<T, S> {
    fn default() -> Self {
        Self::from(Vec::new())
    }
}

impl<T, S> Deref for Items<T, S> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.items
    }
}

impl<T, S> DerefMut for Items<T, S> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.items
    }
}

impl<T, S> From<Vec<T>> for Items<T, S> {
    fn from(items: Vec<T>) -> Self {
        Self {
            items,
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
        self.items.into_iter()
    }
}

impl<'a, T, S> IntoIterator for &'a Items<T, S> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a, T, S> IntoIterator for &'a mut Items<T, S> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

impl<T: serde::Serialize, S> serde::Serialize for Items<T, S> {
    fn serialize<Ser: serde::Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        self.items.serialize(serializer)
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

impl<T: Identified> Keyed<T> {
    /// Sort as a read from the document does: by id, then [`Identified::order`].
    pub fn settle(&mut self) {
        settle(&mut self.items);
    }
}

fn settle<T: Identified>(items: &mut [T]) {
    items.sort_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
    items.sort_by(T::order);
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
        items.sort_by(T::order);
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
        let touched = delta.under(map);
        if touched.is_all() {
            *items = Self::load(doc, map)?;
            return Ok(());
        }
        let mut changed = false;
        for key in touched.keys() {
            let at = items.iter().position(|item| item.id().as_str() == key);
            match (doc.get(map, key)?, at) {
                (None, Some(at)) => {
                    items.remove(at);
                    changed = true;
                }
                (None, None) => {}
                (Some(_), Some(at)) => {
                    if let Some(item) = items.get_mut(at) {
                        refresh_key(item, doc, map, key, delta)?;
                    }
                    changed = true;
                }
                (Some(_), None) => {
                    items.push(read_key(doc, map, key)?);
                    changed = true;
                }
            }
        }
        if changed {
            settle(items);
        }
        Ok(())
    }

    #[inline(never)]
    fn diff<T: Model + Identified, R>(
        before: &[T],
        after: &[T],
        path: &[Segment],
        out: &mut Vec<Edit<R>>,
    ) {
        removed_and_changed(before, after, path, out);
        for item in after {
            let id = item.id();
            if !before.iter().any(|candidate| candidate.id() == id) {
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

/// The edits removing what `after` lacks and changing what both hold.
pub(crate) fn removed_and_changed<T: Model + Identified, R>(
    before: &[T],
    after: &[T],
    path: &[Segment],
    out: &mut Vec<Edit<R>>,
) {
    for item in before {
        let id = item.id();
        match after.iter().find(|candidate| candidate.id() == id) {
            Some(next) => {
                let mut at = path.to_vec();
                at.push(Segment::Item(id));
                item.diff(next, &at, out);
            }
            None => out.push(Edit::new(path.to_vec(), EditOp::RemoveItem { id })),
        }
    }
}

impl<T: Model + Identified, S> crate::items::ItemSource for Items<T, S> {
    fn item_collection(&self, field: &'static str) -> crate::items::ItemCollection {
        crate::collection::listed_items(T::TABLE, self, field)
    }
    fn item_presence(&self) -> usize {
        usize::from(!self.items.is_empty())
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
        self.items.len()
    }

    fn item(&self, at: usize) -> Option<(Id, &dyn std::any::Any)> {
        let item = self.items.get(at)?;
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
        serde_json::Value::Array(self.items.iter().map(Model::to_json).collect())
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
        S::store(&self.items, doc, slot)
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
        S::refresh(&mut self.items, doc, id, delta)
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
        };
        if S::ARRANGED {
            crate::collection::edit_entries(doc, slot, path, op, cx, &edits)
        } else {
            edit_by_id(doc, slot, path, op, cx, &edits)
        }
    }

    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<Edit<R>>) {
        S::diff(&self.items, &after.items, path, out);
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
        for item in &self.items {
            item.collect_text(out);
        }
    }
}
