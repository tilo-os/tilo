//! Storage types whose concurrent edits merge per element: [`Counter`],
//! [`Set`], [`Grid`] and [`Outbox`], and [`Bytes`] for binary values.

use std::collections::BTreeMap;
use std::fmt;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use crate::action::{ActionCx, Refusal};
use crate::collection::{Collection, Id};
use crate::delta::Delta;
use crate::model::LocatedAt as _;
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key, write_key};
use crate::path::{EditOp, Segment, Settable};
use crate::table::{MapEntries, ModelOps, ModelTable, Run};
use std::any::Any;

/// Nothing: a value that stores nothing, for the members of a container that
/// only orders ids (a [`Grid`]'s rows and columns).
impl Model for () {
    crate::__own_table!(());
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    #[inline(never)]
    fn from_json(_: &serde_json::Value) -> Result<Self, ModelError> {
        Ok(())
    }
    #[inline(never)]
    fn load(_: &Doc, _: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        Ok(())
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Null)
    }
}

/// A number whose concurrent changes add up: two people each adding one make
/// two. Change it with [`Self::add`]; writing it back records the difference
/// as an increment rather than a new value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Counter(i64);

impl Counter {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    pub fn add(&mut self, by: i64) {
        self.0 = self.0.saturating_add(by);
    }
}

impl fmt::Display for Counter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Model for Counter {
    crate::__own_table!(Counter);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "integer"})
    }
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        self.0.into()
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        i64::from_json(value).map(Self)
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        i64::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self(0))
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let stored = match slot.current(doc)? {
            Some((value, id)) => i64::load(doc, &value, &id).unwrap_or(0),
            None => 0,
        };
        let counter = matches!(
            slot.current(doc)?,
            Some((Value::Scalar(scalar), _)) if matches!(scalar.as_ref(), ScalarValue::Counter(_))
        );
        if counter && stored == self.0 {
            return Ok(());
        }
        slot.increment(doc, self.0.saturating_sub(if counter { stored } else { 0 }))
    }
    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match op {
            EditOp::Add { by } if path.is_empty() => slot.increment(doc, *by),
            other => Err(ModelError::unsupported(other)),
        }
    }
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        if self.0 != after.0 {
            out.push(crate::Edit::new(
                path.to_vec(),
                EditOp::Add {
                    by: after.0.saturating_sub(self.0),
                },
            ));
        }
    }
}

/// The key an element of a [`Set`] is stored under: its JSON form, a string
/// as itself.
fn element_key<T: Model>(value: &T) -> String {
    match value.to_json() {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

/// A set whose concurrent adds and removes merge per element. Stored as a map
/// keyed by each element's JSON form.
#[derive(Clone, Debug)]
pub struct Set<T> {
    elements: BTreeMap<String, T>,
}

impl<T> Default for Set<T> {
    fn default() -> Self {
        Self {
            elements: BTreeMap::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for Set<T> {
    fn eq(&self, other: &Self) -> bool {
        self.elements == other.elements
    }
}

impl<T: Model> Set<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `value`; returns whether it was absent.
    pub fn insert(&mut self, value: T) -> bool {
        self.elements.insert(element_key(&value), value).is_none()
    }

    /// Remove `value`; returns whether it was present.
    pub fn remove(&mut self, value: &T) -> bool {
        self.elements.remove(&element_key(value)).is_some()
    }

    pub fn contains(&self, value: &T) -> bool {
        self.elements.contains_key(&element_key(value))
    }
}

impl<T> Set<T> {
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The elements, ordered by their stored keys.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        self.elements.values()
    }
}

impl<T: Model> FromIterator<T> for Set<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut set = Self::new();
        for value in iter {
            set.insert(value);
        }
        set
    }
}

impl<T: Model> Model for Set<T> {
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema()})
    }
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    // Run by one set of functions for every set, over `T`'s table.
    const TABLE: &'static ModelTable = &SetOf::<T>::TABLE;
    crate::model::leaf_paths!();
    #[inline]
    fn to_json(&self) -> serde_json::Value {
        Run(Self::TABLE).to_json(self)
    }
    #[inline]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Run(Self::TABLE).from_json(value)
    }
    #[inline]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        Run(Self::TABLE).load(doc, value, id)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    #[inline]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        Run(Self::TABLE).store(self, doc, slot)
    }
    #[inline]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        Run(Self::TABLE).refresh(self, doc, value, id, delta)
    }
    #[inline]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        Run(Self::TABLE).edit(doc, slot, path, op, cx)
    }
    #[inline]
    fn check(&self) -> Result<(), Refusal> {
        Run(Self::TABLE).check(self)
    }
    #[inline]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        Run(Self::TABLE).diff(self, after, path, out);
    }
    #[inline]
    fn collect_text(&self, out: &mut Vec<String>) {
        Run(Self::TABLE).collect_text(self, out);
    }
}

/// What `Set<T>`'s table reaches into one with: its elements, a map from
/// each element's stored key.
#[allow(
    clippy::type_complexity,
    reason = "each field reaches into the set one way"
)]
pub struct SetDesc {
    item: &'static ModelTable,
    elements: for<'a> fn(&'a dyn Any) -> Option<&'a dyn MapEntries>,
    elements_mut: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn MapEntries>,
    /// Put an empty set into `out` (an `Option` of it), and hand back its
    /// elements.
    fresh: for<'a> fn(&'a mut dyn Any) -> Option<&'a mut dyn MapEntries>,
    staged: (fn() -> Box<dyn Any>, crate::table::Unstage),
    replace: fn(&mut dyn Any, &mut dyn Any) -> Result<(), ModelError>,
    item_staged: (fn() -> Box<dyn Any>, crate::table::Unstage),
}

/// The [`SetDesc`] of `Set<T>`.
pub struct SetOf<T>(std::marker::PhantomData<T>);

impl<T: Model> SetOf<T> {
    pub const DESC: SetDesc = SetDesc {
        item: T::TABLE,
        elements: |value| Some(&value.downcast_ref::<Set<T>>()?.elements as &dyn MapEntries),
        elements_mut: |value| {
            Some(&mut value.downcast_mut::<Set<T>>()?.elements as &mut dyn MapEntries)
        },
        fresh: |out| {
            let out = out.downcast_mut::<Option<Set<T>>>()?;
            Some(&mut out.insert(Set::default()).elements as &mut dyn MapEntries)
        },
        staged: (
            crate::table::empty_of::<Set<T>>,
            crate::table::unwrap_of::<Set<T>>,
        ),
        replace: crate::table::replace_of::<Set<T>>,
        item_staged: (crate::table::empty_of::<T>, crate::table::unwrap_of::<T>),
    };

    pub const TABLE: ModelTable = ModelTable::of_set(&Self::DESC, T::CHECKS);
}

fn sets(table: &ModelTable) -> Result<&'static SetDesc, ModelError> {
    table.set().ok_or_else(crate::table::mismatch)
}

/// The stored key of a value whose JSON form is `json`: a string as
/// itself.
fn key_of_json(json: serde_json::Value) -> String {
    match json {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

/// Put the element stored under `key` into `out`, an `Option` of its type:
/// its JSON form is the key as a string, else the key parsed as JSON.
fn element_into(item: &ModelTable, key: &str, out: &mut dyn Any) -> Result<(), ModelError> {
    if (item.ops.from_json)(item, &serde_json::Value::String(key.to_owned()), out).is_ok() {
        return Ok(());
    }
    let value = serde_json::from_str::<serde_json::Value>(key)
        .map_err(|error| ModelError::invalid(error.to_string()))?;
    (item.ops.from_json)(item, &value, out)
}

/// An element from its JSON form, staged, with the key it is stored under.
fn staged_element(
    desc: &SetDesc,
    value: &serde_json::Value,
) -> Result<(String, Box<dyn Any>), ModelError> {
    let mut staged = (desc.item_staged.0)();
    (desc.item.ops.from_json)(desc.item, value, staged.as_mut())?;
    let made = (desc.item_staged.1)(staged.as_ref()).ok_or_else(crate::table::mismatch)?;
    Ok((
        key_of_json((desc.item.ops.to_json)(desc.item, made)),
        staged,
    ))
}

/// The functions of every [`Set`]: each element a key of a map.
pub static SET_OPS: ModelOps = ModelOps {
    to_json: |table, value| {
        let mut out = Vec::new();
        if let Some(set) = table.set()
            && let Some(elements) = (set.elements)(value)
        {
            elements.each(&mut |_, value| out.push((set.item.ops.to_json)(set.item, value)));
        }
        serde_json::Value::Array(out)
    },
    from_json: |table, value, out| {
        let set = sets(table)?;
        match value {
            serde_json::Value::Array(values) => {
                let mut made = Vec::with_capacity(values.len());
                for value in values {
                    made.push(staged_element(set, value)?);
                }
                let elements = (set.fresh)(out).ok_or_else(crate::table::mismatch)?;
                for (key, mut staged) in made {
                    elements.insert_moved(key, staged.as_mut())?;
                }
                Ok(())
            }
            serde_json::Value::Null => {
                (set.fresh)(out).ok_or_else(crate::table::mismatch)?;
                Ok(())
            }
            _ => Err(ModelError::shape("a list of elements")),
        }
    },
    load: |table, doc, value, id, out| {
        let set = sets(table)?;
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a set"));
        }
        let elements = (set.fresh)(out).ok_or_else(crate::table::mismatch)?;
        for key in doc.keys(id) {
            elements.insert_filled(key.clone(), &mut |into| element_into(set.item, &key, into))?;
        }
        Ok(())
    },
    missing: |table, out| {
        (sets(table)?.fresh)(out).ok_or_else(crate::table::mismatch)?;
        Ok(())
    },
    reload: |table, target, doc, slot| {
        let set = sets(table)?;
        let mut staged = (set.staged.0)();
        match slot {
            Some((value, id)) => (SET_OPS.load)(table, doc, value, id, staged.as_mut())?,
            None => (SET_OPS.missing)(table, staged.as_mut())?,
        }
        (set.replace)(target, staged.as_mut())
    },
    store: |table, value, doc, slot| {
        let set = sets(table)?;
        let elements = (set.elements)(value).ok_or_else(crate::table::mismatch)?;
        let map = slot.object(doc, ObjType::Map)?;
        let stale: Vec<String> = doc
            .keys(&map)
            .filter(|key| elements.value(key).is_none())
            .collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        let mut keys = Vec::with_capacity(elements.count());
        elements.each(&mut |key, _| keys.push(key.to_owned()));
        for key in keys {
            if doc.get(&map, key.as_str())?.is_none() {
                doc.put(&map, key.as_str(), true)?;
            }
        }
        Ok(())
    },
    stores_as: |_, _, _| false,
    store_changed: |table, value, _, doc, slot, _| (SET_OPS.store)(table, value, doc, slot),
    refresh: |table, target, doc, value, id, delta| {
        let set = sets(table)?;
        let touched = delta.under(id);
        if touched.is_all() {
            return (SET_OPS.reload)(table, target, doc, Some((value, id)));
        }
        let elements = (set.elements_mut)(target).ok_or_else(crate::table::mismatch)?;
        for key in touched.keys() {
            if doc.get(id, key)?.is_some() {
                elements.insert_filled(key.to_owned(), &mut |into| {
                    element_into(set.item, key, into)
                })?;
            } else {
                elements.remove_key(key);
            }
        }
        Ok(())
    },
    read_whole: |_, _, _| Ok(()),
    edit: |table, doc, slot, path, op, _| {
        let set = sets(table)?;
        match op {
            EditOp::Insert { value } if path.is_empty() => {
                let (key, staged) = staged_element(set, value)?;
                let made =
                    (set.item_staged.1)(staged.as_ref()).ok_or_else(crate::table::mismatch)?;
                (set.item.ops.check)(set.item, made).map_err(ModelError::refused)?;
                let map = slot.object(doc, ObjType::Map)?;
                if doc.get(&map, key.as_str())?.is_none() {
                    doc.put(&map, key.as_str(), true)?;
                }
                Ok(())
            }
            EditOp::Remove { value } if path.is_empty() => {
                let (key, _) = staged_element(set, value)?;
                let map = slot.object(doc, ObjType::Map)?;
                Slot::Key(&map, &key).remove(doc)
            }
            _ => crate::table::edit_whole(
                table,
                ((set.staged.0)(), set.staged.1),
                false,
                doc,
                slot,
                path,
                op,
            ),
        }
    },
    diff: |table, before, after, path, out| {
        let Some(set) = table.set() else {
            return;
        };
        let (Some(was), Some(now)) = ((set.elements)(before), (set.elements)(after)) else {
            return;
        };
        was.each(&mut |key, value| {
            if now.value(key).is_none() {
                out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::Remove {
                        value: (set.item.ops.to_json)(set.item, value),
                    },
                ));
            }
        });
        now.each(&mut |key, value| {
            if was.value(key).is_none() {
                out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::Insert {
                        value: (set.item.ops.to_json)(set.item, value),
                    },
                ));
            }
        });
    },
    collect_text: |table, value, out| {
        if let Some(set) = table.set()
            && let Some(elements) = (set.elements)(value)
        {
            elements.each(&mut |_, value| (set.item.ops.collect_text)(set.item, value, out));
        }
    },
    check: |table, value| {
        let Some(set) = table.set() else {
            return Ok(());
        };
        if !set.item.checks {
            return Ok(());
        }
        let Some(elements) = (set.elements)(value) else {
            return Ok(());
        };
        let mut checked = Ok(());
        elements.each(&mut |_, value| {
            if checked.is_ok() {
                checked = (set.item.ops.check)(set.item, value);
            }
        });
        checked
    },
    get_at: |_, value, path| path.is_empty().then_some(value),
    label: crate::table::no_label,
    nested_collections: crate::table::no_nested,
    op_coalesces: |_, _, _, _| None,
    op_continuable: |_, _, _| None,
    op_describe: |_, _, _| None,
    op_lands: |_, _, _, _| None,
    op_schemas: |_, _, _| {},
};

/// Values up to this size are stored inside the document; larger ones by
/// reference to their content.
pub const BYTES_INLINE_LIMIT: usize = 1024;

/// Binary data. Small values live inside the document; larger ones are
/// referenced by their content hash and kept in the content store, which
/// carries them alongside the record. Written whole: last writer wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bytes {
    #[default]
    Empty,
    /// The data itself, at most [`BYTES_INLINE_LIMIT`] bytes.
    Inline(Vec<u8>),
    /// Data kept in the content store under its BLAKE3 hash.
    Blob { hash: [u8; 32], len: u64 },
}

impl Bytes {
    /// `data` inline when it is small, else a reference to its content; the
    /// caller puts larger data into the content store (see
    /// [`Self::needs_store`]).
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        if data.is_empty() {
            Self::Empty
        } else if data.len() <= BYTES_INLINE_LIMIT {
            Self::Inline(data.to_vec())
        } else {
            Self::Blob {
                hash: *blake3::hash(data).as_bytes(),
                len: u64::try_from(data.len()).unwrap_or(u64::MAX),
            }
        }
    }

    /// Whether the data is kept outside the document.
    #[must_use]
    pub const fn needs_store(&self) -> bool {
        matches!(self, Self::Blob { .. })
    }

    /// The data when it is inside the document.
    #[must_use]
    pub fn inline(&self) -> Option<&[u8]> {
        match self {
            Self::Empty => Some(&[]),
            Self::Inline(data) => Some(data),
            Self::Blob { .. } => None,
        }
    }

    /// The content hash of the data, however it is stored.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        match self {
            Self::Empty => *blake3::hash(&[]).as_bytes(),
            Self::Inline(data) => *blake3::hash(data).as_bytes(),
            Self::Blob { hash, .. } => *hash,
        }
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Inline(data) => u64::try_from(data.len()).unwrap_or(u64::MAX),
            Self::Blob { len, .. } => *len,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn reference(hash: &[u8; 32], len: u64) -> String {
        format!("blake3:{}:{len}", blake3::Hash::from_bytes(*hash).to_hex())
    }

    fn parse_reference(text: &str) -> Result<Self, ModelError> {
        let invalid = || ModelError::invalid(format!("not a content reference: {text}"));
        let rest = text.strip_prefix("blake3:").ok_or_else(invalid)?;
        let (hex, len) = rest.split_once(':').ok_or_else(invalid)?;
        let hash = blake3::Hash::from_hex(hex).map_err(|_| invalid())?;
        let len = len.parse().map_err(|_| invalid())?;
        Ok(Self::Blob {
            hash: *hash.as_bytes(),
            len,
        })
    }
}

impl Settable for Bytes {}
impl Model for Bytes {
    crate::__own_table!(Bytes);
    crate::model::leaf_paths!();
    /// `null` is the empty value.
    const NULLABLE: bool = true;
    /// Inline data as `{ "inline": [bytes] }`, a reference as its
    /// `blake3:<hex>:<len>` string.
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Empty => serde_json::Value::Null,
            Self::Inline(data) => serde_json::json!({ "inline": data }),
            Self::Blob { hash, len } => serde_json::Value::String(Self::reference(hash, *len)),
        }
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Null => Ok(Self::Empty),
            serde_json::Value::String(text) => Self::parse_reference(text),
            serde_json::Value::Object(fields) => {
                let data: Vec<u8> = fields
                    .get("inline")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| ModelError::invalid(error.to_string()))?
                    .unwrap_or_default();
                if data.len() > BYTES_INLINE_LIMIT {
                    return Err(ModelError::invalid("inline data is too large"));
                }
                Ok(if data.is_empty() {
                    Self::Empty
                } else {
                    Self::Inline(data)
                })
            }
            _ => Err(ModelError::shape("bytes")),
        }
    }
    #[inline(never)]
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match value {
            Value::Scalar(scalar) => match scalar.as_ref() {
                ScalarValue::Bytes(data) if data.is_empty() => Ok(Self::Empty),
                ScalarValue::Bytes(data) => Ok(Self::Inline(data.clone())),
                ScalarValue::Str(text) => Self::parse_reference(text),
                ScalarValue::Null => Ok(Self::Empty),
                _ => Err(ModelError::shape("bytes")),
            },
            Value::Object(_) => Err(ModelError::shape("bytes")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::Empty)
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        match self {
            Self::Empty => slot.put_scalar(doc, ScalarValue::Null),
            Self::Inline(data) => slot.put_scalar(doc, ScalarValue::Bytes(data.clone())),
            Self::Blob { hash, len } => {
                slot.put_scalar(doc, ScalarValue::Str(Self::reference(hash, *len).into()))
            }
        }
    }
}

const ROWS: &str = "rows";
const COLUMNS: &str = "columns";
const CELLS: &str = "cells";

/// The key a cell is stored under in the grid's `cells` map.
fn cell_key(row: &str, column: &str) -> String {
    format!("{row}/{column}")
}

/// Every cell `cells` (the grid's `cells` map) holds, by row then column.
fn read_cells<T: Model>(
    doc: &Doc,
    grid: &ObjId,
) -> Result<BTreeMap<String, BTreeMap<String, T>>, ModelError> {
    let mut out: BTreeMap<String, BTreeMap<String, T>> = BTreeMap::new();
    let Some((Value::Object(ObjType::Map), cells)) = doc.get(grid, CELLS)? else {
        return Ok(out);
    };
    for key in doc.keys(&cells) {
        let Some((row, column)) = key.split_once('/') else {
            continue;
        };
        let value = read_key::<T>(doc, &cells, &key)?;
        out.entry(row.to_owned())
            .or_default()
            .insert(column.to_owned(), value);
    }
    Ok(out)
}

/// A table of cells addressed by stable row and column ids. Inserting or
/// removing a row or column keeps every other cell's identity, concurrent
/// edits merge per cell, and empty cells take no space.
///
/// Stored as a map: `rows` and `columns` are ordered like a
/// [`Collection`]'s items, `cells` maps `<row>/<column>` to each non-empty
/// cell. Cells sit flat in one map made with the grid, so no write ever
/// makes an object per row: two people writing into a row they both just
/// added both keep their cells.
#[derive(Clone, Debug)]
pub struct Grid<T> {
    rows: Collection<()>,
    columns: Collection<()>,
    cells: BTreeMap<String, BTreeMap<String, T>>,
}

impl<T> Default for Grid<T> {
    fn default() -> Self {
        Self {
            rows: Collection::new(),
            columns: Collection::new(),
            cells: BTreeMap::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for Grid<T> {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows && self.columns == other.columns && self.cells == other.cells
    }
}

impl<T> Grid<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Row ids in order.
    pub fn rows(&self) -> impl DoubleEndedIterator<Item = &Id> + ExactSizeIterator {
        self.rows.ids()
    }

    /// Column ids in order.
    pub fn columns(&self) -> impl DoubleEndedIterator<Item = &Id> + ExactSizeIterator {
        self.columns.ids()
    }

    pub fn row_position(&self, row: &Id) -> Option<usize> {
        self.rows.position(row)
    }

    pub fn column_position(&self, column: &Id) -> Option<usize> {
        self.columns.position(column)
    }

    pub fn cell(&self, row: &Id, column: &Id) -> Option<&T> {
        self.cells.get(row.as_str())?.get(column.as_str())
    }

    pub fn cell_mut(&mut self, row: &Id, column: &Id) -> Option<&mut T> {
        self.cells.get_mut(row.as_str())?.get_mut(column.as_str())
    }

    /// Set (`Some`) or clear (`None`) the cell at `row`, `column`. A cell of
    /// a row or column that does not exist is not set.
    pub fn set_cell(&mut self, row: &Id, column: &Id, value: Option<T>) -> bool {
        if !self.rows.contains(row) || !self.columns.contains(column) {
            return false;
        }
        match value {
            Some(value) => {
                self.cells
                    .entry(row.as_str().to_owned())
                    .or_default()
                    .insert(column.as_str().to_owned(), value);
            }
            None => {
                if let Some(cells) = self.cells.get_mut(row.as_str()) {
                    cells.remove(column.as_str());
                    if cells.is_empty() {
                        self.cells.remove(row.as_str());
                    }
                }
            }
        }
        true
    }

    /// Insert a row at `at` (clamped).
    pub fn insert_row(&mut self, at: usize, id: Id) -> Id {
        self.rows.insert(at, id, ())
    }

    /// Insert a column at `at` (clamped).
    pub fn insert_column(&mut self, at: usize, id: Id) -> Id {
        self.columns.insert(at, id, ())
    }

    /// Remove a row and its cells.
    pub fn remove_row(&mut self, row: &Id) -> bool {
        self.cells.remove(row.as_str());
        self.rows.remove(row).is_some()
    }

    /// Remove a column and its cells.
    pub fn remove_column(&mut self, column: &Id) -> bool {
        for cells in self.cells.values_mut() {
            cells.remove(column.as_str());
        }
        self.cells.retain(|_, cells| !cells.is_empty());
        self.columns.remove(column).is_some()
    }

    pub fn move_row(&mut self, row: &Id, at: usize) -> bool {
        self.rows.move_to(row, at)
    }

    pub fn move_column(&mut self, column: &Id, at: usize) -> bool {
        self.columns.move_to(column, at)
    }

    /// Every non-empty cell: row, column, value.
    pub fn cells(&self) -> impl Iterator<Item = (Id, Id, &T)> {
        self.cells.iter().flat_map(|(row, cells)| {
            cells.iter().map(move |(column, value)| {
                (
                    Id::from_raw(row.clone()),
                    Id::from_raw(column.clone()),
                    value,
                )
            })
        })
    }
}

impl<T: Model> Grid<T> {
    /// Bring the cells read earlier up to date after `delta`, reading only
    /// the cells it touched.
    fn refresh_cells(&mut self, doc: &Doc, grid: &ObjId, delta: &Delta) -> Result<(), ModelError> {
        let Some((Value::Object(ObjType::Map), cells)) = doc.get(grid, CELLS)? else {
            self.cells.clear();
            return Ok(());
        };
        let touched = delta.under(&cells);
        if touched.is_all() {
            self.cells = read_cells(doc, grid)?;
            return Ok(());
        }
        for key in touched.keys() {
            let Some((row, column)) = key.split_once('/') else {
                continue;
            };
            let Some((value, id)) = doc.get(&cells, key)? else {
                if let Some(row_cells) = self.cells.get_mut(row) {
                    row_cells.remove(column);
                    if row_cells.is_empty() {
                        self.cells.remove(row);
                    }
                }
                continue;
            };
            let existing = self
                .cells
                .get_mut(row)
                .and_then(|row_cells| row_cells.get_mut(column));
            match existing {
                Some(cell) if touched.key(key) == crate::delta::Touch::Below => {
                    cell.refresh(doc, &value, &id, delta).located(key)?;
                }
                _ => {
                    let cell = T::load(doc, &value, &id).located(key)?;
                    self.cells
                        .entry(row.to_owned())
                        .or_default()
                        .insert(column.to_owned(), cell);
                }
            }
        }
        Ok(())
    }
}

fn grid_map(doc: &mut Doc, slot: Slot<'_>) -> Result<ObjId, ModelError> {
    slot.object(doc, ObjType::Map)
}

impl<T: Model> Model for Grid<T> {
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        let edit = |op| crate::Edit::new(path.to_vec(), op);
        for row in self.rows() {
            if !after.rows.contains(row) {
                out.push(edit(EditOp::RemoveRow { id: row.clone() }));
            }
        }
        for column in self.columns() {
            if !after.columns.contains(column) {
                out.push(edit(EditOp::RemoveColumn { id: column.clone() }));
            }
        }
        for (at, row) in after.rows().enumerate() {
            if !self.rows.contains(row) {
                out.push(edit(EditOp::InsertRow {
                    id: Some(row.clone()),
                    at,
                }));
            }
        }
        for (at, column) in after.columns().enumerate() {
            if !self.columns.contains(column) {
                out.push(edit(EditOp::InsertColumn {
                    id: Some(column.clone()),
                    at,
                }));
            }
        }
        for (row, column, value) in after.cells() {
            match self.cell(&row, &column) {
                Some(before) => {
                    let mut at = path.to_vec();
                    at.push(Segment::Cell { row, column });
                    before.diff(value, &at, out);
                }
                None => out.push(edit(EditOp::SetCell {
                    row,
                    column,
                    value: Some(value.to_json()),
                })),
            }
        }
        for (row, column, _) in self.cells() {
            if after.rows.contains(&row)
                && after.columns.contains(&column)
                && after.cell(&row, &column).is_none()
            {
                out.push(edit(EditOp::SetCell {
                    row,
                    column,
                    value: None,
                }));
            }
        }
    }
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Cell { .. }, rest)) if T::OPS => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Cell { .. }, rest)) if T::OPS => T::op_continuable(rest, op),
            _ => None,
        }
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        match path.split_first() {
            Some((Segment::Cell { .. }, rest)) if T::OPS => T::op_describe(rest, op),
            _ => None,
        }
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        match path.split_first() {
            Some((Segment::Cell { row, column }, rest)) => {
                self.cell(row, column)?.op_lands(rest, op)
            }
            _ => None,
        }
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        if T::OPS {
            T::op_schemas(&format!("{prefix}*."), out);
        }
    }
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "rows": self.rows.ids().map(Id::as_str).collect::<Vec<_>>(),
            "columns": self.columns.ids().map(Id::as_str).collect::<Vec<_>>(),
            "cells": self
                .cells()
                .map(|(row, column, value)| serde_json::json!({
                    "row": row.as_str(),
                    "column": column.as_str(),
                    "value": value.to_json(),
                }))
                .collect::<Vec<_>>(),
        })
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        if value.is_null() {
            return Ok(Self::new());
        }
        let ids = |key: &str| -> Result<Vec<Id>, ModelError> {
            value
                .get(key)
                .map_or(Ok(Vec::new()), Vec::<Id>::from_json)
                .located(key)
        };
        let mut grid = Self::new();
        for (at, id) in ids(ROWS)?.into_iter().enumerate() {
            grid.insert_row(at, id);
        }
        for (at, id) in ids(COLUMNS)?.into_iter().enumerate() {
            grid.insert_column(at, id);
        }
        for cell in value
            .get(CELLS)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |key: &str| {
                cell.get(key)
                    .map(Id::from_json)
                    .transpose()?
                    .ok_or_else(|| ModelError::Missing {
                        path: key.to_owned(),
                    })
            };
            let (row, column) = (field("row")?, field("column")?);
            let value = T::from_json(cell.get("value").unwrap_or(&serde_json::Value::Null))?;
            if !grid.set_cell(&row, &column, Some(value)) {
                return Err(ModelError::invalid(format!("no cell {row}:{column}")));
            }
        }
        Ok(grid)
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a grid"));
        }
        Ok(Self {
            rows: read_key(doc, id, ROWS)?,
            columns: read_key(doc, id, COLUMNS)?,
            cells: read_cells(doc, id)?,
        })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = grid_map(doc, slot)?;
        write_key(doc, &map, ROWS, &self.rows)?;
        write_key(doc, &map, COLUMNS, &self.columns)?;
        let cells = Slot::Key(&map, CELLS).object(doc, ObjType::Map)?;
        let held: std::collections::BTreeSet<String> = self
            .cells()
            .map(|(row, column, _)| cell_key(row.as_str(), column.as_str()))
            .collect();
        let stale: Vec<String> = doc.keys(&cells).filter(|key| !held.contains(key)).collect();
        for key in stale {
            doc.delete(&cells, key.as_str())?;
        }
        for (row, column, value) in self.cells() {
            let key = cell_key(row.as_str(), column.as_str());
            value.store(doc, Slot::Key(&cells, &key)).located(&key)?;
        }
        Ok(())
    }
    #[inline(never)]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if delta.under(id).is_all() {
            *self = Self::load(doc, value, id)?;
            return Ok(());
        }
        refresh_key(&mut self.rows, doc, id, ROWS, delta)?;
        refresh_key(&mut self.columns, doc, id, COLUMNS, delta)?;
        match delta.key(id, CELLS) {
            crate::delta::Touch::Untouched => Ok(()),
            crate::delta::Touch::Written => {
                self.cells = read_cells(doc, id)?;
                Ok(())
            }
            // Only the cells the change touched are read again: a keystroke
            // in one cell reads that cell, not the sheet.
            crate::delta::Touch::Below => self.refresh_cells(doc, id, delta),
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per grid operation, read top to bottom"
    )]
    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let map = grid_map(doc, slot)?;
        let exists = |doc: &Doc, key: &str, id: &Id| -> Result<bool, ModelError> {
            Ok(match doc.get(&map, key)? {
                Some((_, axis)) => doc.get(&axis, id.as_str())?.is_some(),
                None => false,
            })
        };
        match (path.split_first(), op) {
            (Some((Segment::Cell { row, column }, rest)), op) => {
                if !exists(doc, ROWS, row)? || !exists(doc, COLUMNS, column)? {
                    return Err(ModelError::invalid(format!("no cell {row}:{column}")));
                }
                let cells = Slot::Key(&map, CELLS).object(doc, ObjType::Map)?;
                let key = cell_key(row.as_str(), column.as_str());
                let slot = Slot::Key(&cells, &key);
                if rest.is_empty() && matches!(op, EditOp::Set { .. }) {
                    return edit_whole::<T>(doc, slot, rest, op);
                }
                if slot.current(doc)?.is_none() {
                    return Err(ModelError::invalid(format!(
                        "the cell {row}:{column} is empty"
                    )));
                }
                T::edit(doc, slot, rest, op, cx)
            }
            (None, EditOp::SetCell { row, column, value }) => {
                if !exists(doc, ROWS, row)? || !exists(doc, COLUMNS, column)? {
                    return Err(ModelError::invalid(format!("no cell {row}:{column}")));
                }
                let cells = Slot::Key(&map, CELLS).object(doc, ObjType::Map)?;
                let key = cell_key(row.as_str(), column.as_str());
                match value {
                    Some(value) => {
                        let value = T::from_json(value)?;
                        value.check().map_err(ModelError::refused)?;
                        value.store(doc, Slot::Key(&cells, &key))
                    }
                    None => Slot::Key(&cells, &key).remove(doc),
                }
            }
            (None, EditOp::InsertRow { id, at } | EditOp::InsertColumn { id, at }) => {
                let axis = if matches!(op, EditOp::InsertRow { .. }) {
                    ROWS
                } else {
                    COLUMNS
                };
                let id = id.clone().unwrap_or_else(|| cx.id());
                Collection::<()>::edit(
                    doc,
                    Slot::Key(&map, axis),
                    &[],
                    &EditOp::InsertItem {
                        id: Some(id),
                        at: *at,
                        value: serde_json::Value::Null,
                    },
                    cx,
                )?;
                Ok(())
            }
            (None, EditOp::RemoveRow { id }) => {
                Collection::<()>::edit(
                    doc,
                    Slot::Key(&map, ROWS),
                    &[],
                    &EditOp::RemoveItem { id: id.clone() },
                    cx,
                )?;
                if let Some((_, cells)) = doc.get(&map, CELLS)? {
                    let prefix = format!("{id}/");
                    let gone: Vec<String> = doc
                        .keys(&cells)
                        .filter(|key| key.starts_with(&prefix))
                        .collect();
                    for key in gone {
                        doc.delete(&cells, key.as_str())?;
                    }
                }
                Ok(())
            }
            (None, EditOp::RemoveColumn { id }) => {
                Collection::<()>::edit(
                    doc,
                    Slot::Key(&map, COLUMNS),
                    &[],
                    &EditOp::RemoveItem { id: id.clone() },
                    cx,
                )?;
                if let Some((_, cells)) = doc.get(&map, CELLS)? {
                    let suffix = format!("/{id}");
                    let gone: Vec<String> = doc
                        .keys(&cells)
                        .filter(|key| key.ends_with(&suffix))
                        .collect();
                    for key in gone {
                        doc.delete(&cells, key.as_str())?;
                    }
                }
                Ok(())
            }
            _ => edit_whole::<Self>(doc, slot, path, op),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            for cells in self.cells.values() {
                cells.values().try_for_each(Model::check)?;
            }
        }
        Ok(())
    }
    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        match path.split_first() {
            None => Some(self),
            Some((Segment::Cell { row, column }, rest)) => self.cell(row, column)?.get_at(rest),
            Some(_) => None,
        }
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        for cells in self.cells.values() {
            for value in cells.values() {
                value.collect_text(out);
            }
        }
    }
}

/// How the owning device settled an [`Outbox`] entry, with what its handler
/// reports back (`O`): where a trashed file went, the name a copy got, the
/// id a server gave the upload.
///
/// Its JSON form is `{"state": "done", "result": O}` or `{"state": "failed",
/// "result": {"reason": .., "partial": O}}`. It is read through one JSON value
/// ([`Settled::from_json_value`]), so `O` is decoded by its value decoder
/// alone, never through serde's buffered copy of a tagged enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "result", rename_all = "kebab-case")]
pub enum Settled<O = ()> {
    /// The outside consequence happened.
    Done(O),
    /// It failed for good, in the handler's words, after doing `partial`.
    Failed { reason: String, partial: O },
}

impl<O> Settled<O> {
    /// Whether the consequence happened.
    #[must_use]
    pub const fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }

    /// What the handler did: all of it, or the part done before it failed.
    #[must_use]
    pub const fn result(&self) -> &O {
        match self {
            Self::Done(result)
            | Self::Failed {
                partial: result, ..
            } => result,
        }
    }

    /// Why it failed, if it did.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Done(_) => None,
            Self::Failed { reason, .. } => Some(reason),
        }
    }
}

impl<'de, O: serde::de::DeserializeOwned> Deserialize<'de> for Settled<O> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
        Self::from_json_value(value).map_err(serde::de::Error::custom)
    }
}

impl<O: serde::de::DeserializeOwned> Settled<O> {
    /// Read from its JSON form.
    ///
    /// # Errors
    /// Why `value` is not a settlement with an `O`.
    pub fn from_json_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(match settled_parts(value)? {
            (true, result) => Self::Done(serde_json::from_value(result)?),
            (false, result) => {
                let (reason, partial) = failed_parts(result)?;
                Self::Failed {
                    reason,
                    partial: serde_json::from_value(partial)?,
                }
            }
        })
    }
}

/// Whether a settlement's JSON form says done, and its `result`.
#[inline(never)]
fn settled_parts(value: serde_json::Value) -> Result<(bool, serde_json::Value), serde_json::Error> {
    use serde::de::Error as _;
    let serde_json::Value::Object(mut fields) = value else {
        return Err(serde_json::Error::custom("a settlement is an object"));
    };
    let done = match fields.remove("state") {
        Some(serde_json::Value::String(state)) if state == "done" => true,
        Some(serde_json::Value::String(state)) if state == "failed" => false,
        Some(serde_json::Value::String(state)) => {
            return Err(serde_json::Error::unknown_variant(
                &state,
                &["done", "failed"],
            ));
        }
        Some(_) => return Err(serde_json::Error::custom("`state` is a string")),
        None => return Err(serde_json::Error::missing_field("state")),
    };
    let result = fields
        .remove("result")
        .ok_or_else(|| serde_json::Error::missing_field("result"))?;
    Ok((done, result))
}

/// A failed settlement's `reason` and its `partial` result.
#[inline(never)]
fn failed_parts(
    result: serde_json::Value,
) -> Result<(String, serde_json::Value), serde_json::Error> {
    use serde::de::Error as _;
    let serde_json::Value::Object(mut fields) = result else {
        return Err(serde_json::Error::custom("a failure is an object"));
    };
    let reason = match fields.remove("reason") {
        Some(serde_json::Value::String(reason)) => reason,
        Some(_) => return Err(serde_json::Error::custom("`reason` is a string")),
        None => return Err(serde_json::Error::missing_field("reason")),
    };
    let partial = fields
        .remove("partial")
        .ok_or_else(|| serde_json::Error::missing_field("partial"))?;
    Ok((reason, partial))
}

impl<O: Default> Settled<O> {
    /// Failed for good with nothing done.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            partial: O::default(),
        }
    }
}

/// A settlement stores as one JSON value: it is written once, by the owning
/// device.
impl<O: Serialize + serde::de::DeserializeOwned + 'static> Model for Settled<O> {
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        crate::__private::json_via_bytes(self).unwrap_or(serde_json::Value::Null)
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Self::from_json_value(value.clone()).map_err(|error| ModelError::invalid(error.to_string()))
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        let text = String::load(doc, value, id)?;
        serde_json::from_str::<serde_json::Value>(&text)
            .and_then(Self::from_json_value)
            .map_err(|error| ModelError::invalid(error.to_string()))
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text =
            serde_json::to_string(self).map_err(|error| ModelError::invalid(error.to_string()))?;
        slot.put_scalar(doc, ScalarValue::Str(text.into()))
    }
}

/// One queued outside consequence and, once the owning device ran it, how
/// it settled.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEntry<T, O = ()> {
    pub value: T,
    pub settled: Option<Settled<O>>,
    /// The steps (change hashes) whose undo or redo queued this entry: they
    /// count as taken back (or brought back) once it settles done.
    pub compensates: Vec<String>,
}

const ENTRY_VALUE: &str = "value";
const ENTRY_SETTLED: &str = "settled";
const ENTRY_COMPENSATES: &str = "compensates";

/// The undo or redo of `steps` completed: the entry they waited for settled
/// done. A step whose undo waits for nothing (settled already, or another
/// device's history) is left as it is.
fn settle_steps(doc: &mut Doc, steps: &[String]) {
    for step in steps {
        let _ = tilo_actions::settle_compensation(doc, step);
    }
}

impl<T: Model, O: Serialize + serde::de::DeserializeOwned + 'static> Model for OutboxEntry<T, O> {
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            ENTRY_VALUE: self.value.to_json(),
            ENTRY_SETTLED: self.settled.to_json(),
            ENTRY_COMPENSATES: self.compensates.to_json(),
        })
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let null = serde_json::Value::Null;
        Ok(Self {
            value: T::from_json(value.get(ENTRY_VALUE).unwrap_or(&null))?,
            settled: Option::<Settled<O>>::from_json(value.get(ENTRY_SETTLED).unwrap_or(&null))?,
            compensates: match value.get(ENTRY_COMPENSATES) {
                Some(steps) => Vec::from_json(steps)?,
                None => Vec::new(),
            },
        })
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("an outbox entry"));
        }
        Ok(Self {
            value: read_key(doc, id, ENTRY_VALUE)?,
            settled: read_key(doc, id, ENTRY_SETTLED)?,
            compensates: read_key(doc, id, ENTRY_COMPENSATES)?,
        })
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let was_done = read_key::<Option<Settled<O>>>(doc, &map, ENTRY_SETTLED)
            .ok()
            .flatten()
            .is_some_and(|settled| settled.is_done());
        write_key(doc, &map, ENTRY_VALUE, &self.value)?;
        write_key(doc, &map, ENTRY_SETTLED, &self.settled)?;
        write_key(doc, &map, ENTRY_COMPENSATES, &self.compensates)?;
        // A verb that settles the entry done completes the undo it was.
        if !was_done && self.settled.as_ref().is_some_and(Settled::is_done) {
            settle_steps(doc, &self.compensates);
        }
        Ok(())
    }
    fn check(&self) -> Result<(), Refusal> {
        self.value.check()
    }
}

/// Outside consequences of the record's actions, queued in the document: a
/// mail to send, an event to upload, a file to move. A verb that takes
/// `&mut Outbox<T, O>` pushes entries and is an Outside action.
///
/// Every replica replays the entries; only the device that owns an entry
/// carries it out. The framework drains the outbox there, runs the app's
/// handler for each [`pending`](Self::pending) entry and records how it
/// settled, with the handler's result `O`, through the
/// [`settle`](crate::Path::settle) edit: content from outside, nobody's undo
/// step. A verb reads a settlement with [`Self::outcome`] (a revert needs
/// to know where the trashed file went), may settle an entry itself
/// ([`Self::settle`]) and forgets settled entries ([`Self::forget`],
/// [`Self::prune`]).
#[derive(Clone, Debug)]
pub struct Outbox<T, O = ()> {
    entries: Collection<OutboxEntry<T, O>>,
}

impl<T, O> Default for Outbox<T, O> {
    fn default() -> Self {
        Self {
            entries: Collection::new(),
        }
    }
}

impl<T: PartialEq, O: PartialEq> PartialEq for Outbox<T, O> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<T, O> Outbox<T, O> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `value` under `id` (mint it with [`ActionCx::id`]). Queued by
    /// an undo's or a redo's compensating action, it records the steps it
    /// settles.
    pub fn push(&mut self, id: Id, value: T) -> Id {
        self.entries.push(
            id,
            OutboxEntry {
                value,
                settled: None,
                compensates: tilo_actions::await_compensation(),
            },
        )
    }

    /// The entries the owning device has yet to run, oldest first.
    pub fn pending(&self) -> impl Iterator<Item = (&Id, &T)> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.settled.is_none())
            .map(|(id, entry)| (id, &entry.value))
    }

    /// Every entry, settled or not, oldest first.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&Id, &OutboxEntry<T, O>)> {
        self.entries.iter()
    }

    pub fn get(&self, id: &Id) -> Option<&OutboxEntry<T, O>> {
        self.entries.get(id)
    }

    /// How the entry `id` settled; `None` while it is pending or when there
    /// is no such entry.
    pub fn outcome(&self, id: &Id) -> Option<&Settled<O>> {
        self.entries.get(id)?.settled.as_ref()
    }

    /// Record how the entry `id` settled, from a verb. Whether there is
    /// such an entry.
    pub fn settle(&mut self, id: &Id, outcome: Settled<O>) -> bool {
        self.entries.get_mut(id).is_some_and(|entry| {
            entry.settled = Some(outcome);
            true
        })
    }

    /// Forget the entry `id`. A pending entry forgotten is never carried
    /// out.
    pub fn forget(&mut self, id: &Id) -> Option<OutboxEntry<T, O>> {
        self.entries.remove(id)
    }

    /// Forget the oldest settled entries beyond the newest `keep`; pending
    /// ones stay.
    pub fn prune(&mut self, keep: usize) {
        let settled = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.settled.is_some())
            .count();
        let mut excess = settled.saturating_sub(keep);
        self.entries.retain(|_, entry| {
            if excess > 0 && entry.settled.is_some() {
                excess = excess.saturating_sub(1);
                return false;
            }
            true
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The map of entries an outbox stored in `slot` keeps, and the entry `id`
/// in it.
fn outbox_entry(doc: &mut Doc, slot: Slot<'_>, id: &Id) -> Result<(ObjId, ObjId), ModelError> {
    let map = slot.object(doc, ObjType::Map)?;
    let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? else {
        return Err(ModelError::invalid(format!("no outbox entry `{id}`")));
    };
    let Some((Value::Object(ObjType::Map), value)) = doc.get(&entry, crate::collection::VALUE)?
    else {
        return Err(ModelError::invalid(format!("no outbox entry `{id}`")));
    };
    Ok((map, value))
}

impl<T: Model, O: Serialize + serde::de::DeserializeOwned + 'static> Model for Outbox<T, O> {
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        self.entries.to_json()
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Collection::from_json(value).map(|entries| Self { entries })
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        Collection::load(doc, value, id).map(|entries| Self { entries })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.entries.store(doc, slot)
    }
    #[inline(never)]
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        self.entries.refresh(doc, value, id, delta)
    }
    /// Entries are queued by verbs; an edit settles one or forgets a
    /// settled one.
    #[inline(never)]
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match (path, op) {
            ([], EditOp::Settle { id, outcome }) => {
                let outcome = Settled::<O>::from_json(outcome)?;
                let (_, value) = outbox_entry(doc, slot, id)?;
                let done = outcome.is_done();
                write_key(doc, &value, ENTRY_SETTLED, &Some(outcome))?;
                // Done, it completes the undo or redo it was queued by; a
                // failure leaves that step as it was.
                if done {
                    let steps: Vec<String> = read_key(doc, &value, ENTRY_COMPENSATES)?;
                    settle_steps(doc, &steps);
                }
                Ok(())
            }
            ([], EditOp::Forget { id }) => {
                let (map, value) = outbox_entry(doc, slot, id)?;
                if read_key::<Option<Settled<O>>>(doc, &value, ENTRY_SETTLED)?.is_none() {
                    return Err(ModelError::refused(Refusal::new(format!(
                        "outbox entry `{id}` is still pending"
                    ))));
                }
                doc.delete(&map, id.as_str())?;
                Ok(())
            }
            _ => Err(ModelError::unsupported(op)),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        self.entries.check()
    }
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        for (id, entry) in after.entries.iter() {
            if let (Some(outcome), Some(before)) = (&entry.settled, self.entries.get(id))
                && before.settled.as_ref().map(Model::to_json) != Some(outcome.to_json())
            {
                out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::Settle {
                        id: id.clone(),
                        outcome: outcome.to_json(),
                    },
                ));
            }
        }
        for (id, entry) in self.entries.iter() {
            if entry.settled.is_some() && after.entries.get(id).is_none() {
                out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::Forget { id: id.clone() },
                ));
            }
        }
    }
}
