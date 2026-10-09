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
use crate::model::{Doc, Model, ModelError, Slot, edit_whole, read_key, refresh_key, write_key};
use crate::path::{EditOp, Segment, Settable};

/// Nothing: a value that stores nothing, for the members of a container that
/// only orders ids (a [`Grid`]'s rows and columns).
impl Model for () {
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn from_json(_: &serde_json::Value) -> Result<Self, ModelError> {
        Ok(())
    }
    fn load(_: &Doc, _: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        Ok(())
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(())
    }
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
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        self.0.into()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        i64::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        i64::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self(0))
    }
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
}

/// The key an element of a [`Set`] is stored under: its JSON form, a string
/// as itself.
fn element_key<T: Model>(value: &T) -> String {
    match value.to_json() {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    }
}

fn element_of_key<T: Model>(key: &str) -> Result<T, ModelError> {
    T::from_json(&serde_json::Value::String(key.to_owned())).or_else(|_| {
        serde_json::from_str::<serde_json::Value>(key)
            .map_err(|error| ModelError::invalid(error.to_string()))
            .and_then(|value| T::from_json(&value))
    })
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
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.elements.values().map(Model::to_json).collect())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Array(values) => values
                .iter()
                .map(T::from_json)
                .collect::<Result<Vec<_>, _>>()
                .map(|values| values.into_iter().collect()),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(ModelError::shape("a list of elements")),
        }
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a set"));
        }
        doc.keys(id)
            .map(|key| element_of_key::<T>(&key).map(|value| (key, value)))
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(|elements| Self { elements })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let stale: Vec<String> = doc
            .keys(&map)
            .filter(|key| !self.elements.contains_key(key))
            .collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        for key in self.elements.keys() {
            if doc.get(&map, key.as_str())?.is_none() {
                doc.put(&map, key.as_str(), true)?;
            }
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
        for key in touched.keys() {
            if doc.get(id, key)?.is_some() {
                self.elements
                    .insert(key.to_owned(), element_of_key::<T>(key)?);
            } else {
                self.elements.remove(key);
            }
        }
        Ok(())
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match op {
            EditOp::Insert { value } if path.is_empty() => {
                let value = T::from_json(value)?;
                value.check().map_err(ModelError::refused)?;
                let map = slot.object(doc, ObjType::Map)?;
                let key = element_key(&value);
                if doc.get(&map, key.as_str())?.is_none() {
                    doc.put(&map, key.as_str(), true)?;
                }
                Ok(())
            }
            EditOp::Remove { value } if path.is_empty() => {
                let value = T::from_json(value)?;
                let map = slot.object(doc, ObjType::Map)?;
                Slot::Key(&map, &element_key(&value)).remove(doc)
            }
            _ => edit_whole::<Self>(doc, slot, path, op),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.elements.values().try_for_each(Model::check)?;
        }
        Ok(())
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        for value in self.elements.values() {
            value.collect_text(out);
        }
    }
}

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
    crate::model::leaf_paths!();
    /// Inline data as `{ "inline": [bytes] }`, a reference as its
    /// `blake3:<hex>:<len>` string.
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Empty => serde_json::Value::Null,
            Self::Inline(data) => serde_json::json!({ "inline": data }),
            Self::Blob { hash, len } => serde_json::Value::String(Self::reference(hash, *len)),
        }
    }
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

/// A table of cells addressed by stable row and column ids. Inserting or
/// removing a row or column keeps every other cell's identity, concurrent
/// edits merge per cell, and empty cells take no space.
///
/// Stored as a map: `rows` and `columns` are ordered like a
/// [`Collection`]'s items, `cells` maps each row id to a map of its
/// non-empty cells by column id.
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

fn grid_map(doc: &mut Doc, slot: Slot<'_>) -> Result<ObjId, ModelError> {
    slot.object(doc, ObjType::Map)
}

impl<T: Model> Model for Grid<T> {
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Cell { .. }, rest)) => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Cell { .. }, rest)) => T::op_continuable(rest, op),
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
        T::op_schemas(&format!("{prefix}*."), out);
    }
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
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        if value.is_null() {
            return Ok(Self::new());
        }
        let ids = |key: &str| -> Result<Vec<Id>, ModelError> {
            value
                .get(key)
                .map_or(Ok(Vec::new()), Vec::<Id>::from_json)
                .map_err(|error| error.at(key))
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
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a grid"));
        }
        Ok(Self {
            rows: read_key(doc, id, ROWS)?,
            columns: read_key(doc, id, COLUMNS)?,
            cells: read_key(doc, id, CELLS)?,
        })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = grid_map(doc, slot)?;
        write_key(doc, &map, ROWS, &self.rows)?;
        write_key(doc, &map, COLUMNS, &self.columns)?;
        write_key(doc, &map, CELLS, &self.cells)
    }
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
        refresh_key(&mut self.cells, doc, id, CELLS, delta)
    }
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
                let cells = Slot::Key(&cells, row.as_str()).object(doc, ObjType::Map)?;
                let slot = Slot::Key(&cells, column.as_str());
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
                match value {
                    Some(value) => {
                        let value = T::from_json(value)?;
                        value.check().map_err(ModelError::refused)?;
                        let cells = Slot::Key(&cells, row.as_str()).object(doc, ObjType::Map)?;
                        value.store(doc, Slot::Key(&cells, column.as_str()))
                    }
                    None => match doc.get(&cells, row.as_str())? {
                        Some((_, row_cells)) => Slot::Key(&row_cells, column.as_str()).remove(doc),
                        None => Ok(()),
                    },
                }
            }
            (None, EditOp::InsertRow { id, at } | EditOp::InsertColumn { id, at }) => {
                let axis = if matches!(op, EditOp::InsertRow { .. }) {
                    ROWS
                } else {
                    COLUMNS
                };
                Collection::<()>::edit(
                    doc,
                    Slot::Key(&map, axis),
                    &[],
                    &EditOp::InsertItem {
                        id: id.clone(),
                        at: *at,
                        value: serde_json::Value::Null,
                    },
                    cx,
                )
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
                    Slot::Key(&cells, id.as_str()).remove(doc)?;
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
                    let rows: Vec<String> = doc.keys(&cells).collect();
                    for row in rows {
                        if let Some((_, row_cells)) = doc.get(&cells, row.as_str())? {
                            Slot::Key(&row_cells, id.as_str()).remove(doc)?;
                        }
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

/// How the owning device settled an [`Outbox`] entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Settled {
    /// The outside consequence happened.
    Done,
    /// It failed for good, in the handler's words.
    Failed { reason: String },
}

impl Model for Settled {
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        serde_json::from_value(value.clone())
            .map_err(|error| ModelError::invalid(error.to_string()))
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        let text = String::load(doc, value, id)?;
        serde_json::from_str(&text).map_err(|error| ModelError::invalid(error.to_string()))
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text =
            serde_json::to_string(self).map_err(|error| ModelError::invalid(error.to_string()))?;
        slot.put_scalar(doc, ScalarValue::Str(text.into()))
    }
}

/// One queued outside consequence and, once the owning device ran it, how
/// it settled.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEntry<T> {
    pub value: T,
    pub settled: Option<Settled>,
}

const ENTRY_VALUE: &str = "value";
const ENTRY_SETTLED: &str = "settled";

impl<T: Model> Model for OutboxEntry<T> {
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            ENTRY_VALUE: self.value.to_json(),
            ENTRY_SETTLED: self.settled.to_json(),
        })
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let null = serde_json::Value::Null;
        Ok(Self {
            value: T::from_json(value.get(ENTRY_VALUE).unwrap_or(&null))?,
            settled: Option::<Settled>::from_json(value.get(ENTRY_SETTLED).unwrap_or(&null))?,
        })
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("an outbox entry"));
        }
        Ok(Self {
            value: read_key(doc, id, ENTRY_VALUE)?,
            settled: read_key(doc, id, ENTRY_SETTLED)?,
        })
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        write_key(doc, &map, ENTRY_VALUE, &self.value)?;
        write_key(doc, &map, ENTRY_SETTLED, &self.settled)
    }
    fn check(&self) -> Result<(), Refusal> {
        self.value.check()
    }
}

/// Outside consequences of the record's actions, queued in the document: a
/// mail to send, a flag to set on the server, a file to move. A verb that
/// takes `&mut Outbox<T>` pushes entries and is an Outside action.
///
/// Every replica replays the entries; only the device that owns the outbox
/// drains it: the framework runs the app's handler for each
/// [`pending`](Self::pending) entry there and records how it went with the
/// [`settle`](crate::Path::settle) edit, so the result syncs like any change.
#[derive(Clone, Debug)]
pub struct Outbox<T> {
    entries: Collection<OutboxEntry<T>>,
}

impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Self {
            entries: Collection::new(),
        }
    }
}

impl<T: PartialEq> PartialEq for Outbox<T> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<T> Outbox<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `value` under `id` (mint it with [`ActionCx::id`]).
    pub fn push(&mut self, id: Id, value: T) -> Id {
        self.entries.push(
            id,
            OutboxEntry {
                value,
                settled: None,
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
    pub fn entries(&self) -> impl ExactSizeIterator<Item = (&Id, &OutboxEntry<T>)> {
        self.entries.iter()
    }

    pub fn get(&self, id: &Id) -> Option<&OutboxEntry<T>> {
        self.entries.get(id)
    }

    /// Forget a settled entry.
    pub fn remove(&mut self, id: &Id) -> Option<OutboxEntry<T>> {
        self.entries.remove(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<T: Model> Model for Outbox<T> {
    const CHECKS: bool = T::CHECKS;
    crate::model::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        self.entries.to_json()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Collection::from_json(value).map(|entries| Self { entries })
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        Collection::load(doc, value, id).map(|entries| Self { entries })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.entries.store(doc, slot)
    }
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        self.entries.refresh(doc, value, id, delta)
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match (path, op) {
            ([], EditOp::Settle { id, outcome }) => {
                let map = slot.object(doc, ObjType::Map)?;
                let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? else {
                    return Err(ModelError::invalid(format!("no outbox entry `{id}`")));
                };
                let Some((Value::Object(ObjType::Map), value)) =
                    doc.get(&entry, crate::collection::VALUE)?
                else {
                    return Err(ModelError::invalid(format!("no outbox entry `{id}`")));
                };
                write_key(doc, &value, ENTRY_SETTLED, &Some(outcome.clone()))
            }
            _ => Err(ModelError::unsupported(op)),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        self.entries.check()
    }
}
