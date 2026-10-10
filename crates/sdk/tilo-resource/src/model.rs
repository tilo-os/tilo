//! How typed values live in a resource's CRDT document.
//!
//! [`Model`] is the whole storage contract: read a value out of the slot its
//! parent found, and write a value back touching only what changed. Every
//! write is a *diff* — an unchanged scalar writes nothing, a string is spliced
//! rather than replaced, a keyed collection moves one order key rather than
//! renumbering — so concurrent edits from collaborators merge field by field
//! and an action that changes nothing commits nothing.
//!
//! Plain Rust types implement it here; `#[derive(Model)]` implements it for
//! structs (a map, one key per field) and unit enums (a string). A resource
//! type with its own storage layout implements it — or skips it and reads and
//! writes the document itself, see [`crate::Resource`].

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::delta::{Delta, Touch};

/// The document every resource lives in.
pub type Doc = AutoCommit;

/// Why a document could not be read as, or written from, a typed value.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{path}: expected {expected}")]
    Shape {
        path: String,
        expected: &'static str,
    },
    #[error("{path}: required value is missing")]
    Missing { path: String },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error("document error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
}

impl ModelError {
    #[must_use]
    pub fn shape(expected: &'static str) -> Self {
        Self::Shape {
            path: String::new(),
            expected,
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            path: String::new(),
            message: message.into(),
        }
    }

    /// Prefix the location this error was found at, innermost last.
    #[must_use]
    pub fn at(mut self, segment: &str) -> Self {
        match &mut self {
            Self::Shape { path, .. } | Self::Missing { path } | Self::Invalid { path, .. } => {
                *path = if path.is_empty() {
                    segment.to_owned()
                } else {
                    format!("{segment}.{path}")
                };
            }
            Self::Automerge(_) => {}
        }
        self
    }
}

/// Where a value is written: a key of a map, an existing list element, or a
/// new list element inserted at an index.
#[derive(Clone, Copy, Debug)]
pub enum Slot<'a> {
    Key(&'a ObjId, &'a str),
    Index(&'a ObjId, usize),
    Insert(&'a ObjId, usize),
}

impl Slot<'_> {
    /// What currently occupies this slot. A new list element holds nothing.
    ///
    /// # Errors
    /// Returns the document's read error.
    pub fn current(self, doc: &Doc) -> Result<Option<(Value<'static>, ObjId)>, ModelError> {
        let found = match self {
            Slot::Key(obj, key) => doc.get(obj, key)?,
            Slot::Index(obj, index) => doc.get(obj, index)?,
            Slot::Insert(..) => None,
        };
        Ok(found.map(|(value, id)| (value.to_owned(), id)))
    }

    /// Write `value` unless the slot already holds an equal scalar.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn put_scalar(self, doc: &mut Doc, value: ScalarValue) -> Result<(), ModelError> {
        match self {
            Slot::Key(obj, key) => {
                if !matches!(doc.get(obj, key)?, Some((Value::Scalar(current), _)) if same_scalar(&current, &value))
                {
                    doc.put(obj, key, value)?;
                }
            }
            Slot::Index(obj, index) => {
                if !matches!(doc.get(obj, index)?, Some((Value::Scalar(current), _)) if same_scalar(&current, &value))
                {
                    doc.put(obj, index, value)?;
                }
            }
            Slot::Insert(obj, index) => doc.insert(obj, index, value)?,
        }
        Ok(())
    }

    /// The object of type `kind` in this slot, creating it (and replacing any
    /// value of another shape) when absent.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn object(self, doc: &mut Doc, kind: ObjType) -> Result<ObjId, ModelError> {
        if let Some((Value::Object(existing), id)) = self.current(doc)?
            && existing == kind
        {
            return Ok(id);
        }
        Ok(match self {
            Slot::Key(obj, key) => doc.put_object(obj, key, kind)?,
            Slot::Index(obj, index) => doc.put_object(obj, index, kind)?,
            Slot::Insert(obj, index) => doc.insert_object(obj, index, kind)?,
        })
    }

    /// Remove whatever occupies this slot (a no-op for a new list element).
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn remove(self, doc: &mut Doc) -> Result<(), ModelError> {
        match self {
            Slot::Key(obj, key) => {
                if doc.get(obj, key)?.is_some() {
                    doc.delete(obj, key)?;
                }
            }
            Slot::Index(obj, index) => doc.put(obj, index, ScalarValue::Null)?,
            Slot::Insert(obj, index) => doc.insert(obj, index, ScalarValue::Null)?,
        }
        Ok(())
    }
}

fn same_scalar(current: &ScalarValue, next: &ScalarValue) -> bool {
    match (current, next) {
        (ScalarValue::F64(a), ScalarValue::F64(b)) => a.to_bits() == b.to_bits(),
        (a, b) => a == b,
    }
}

/// A typed value stored in a CRDT document. See the module docs.
pub trait Model: Sized {
    /// How the value is laid out in the document; see [`Shape`](crate::Shape).
    /// Types written whole keep the default.
    const SHAPE: crate::Shape = crate::Shape::Value;

    /// Read from the value a parent found in its slot.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the stored value has another shape.
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError>;

    /// The value of an absent slot. Types with a natural empty value return it,
    /// so a field added to a schema reads older documents without migration.
    ///
    /// # Errors
    /// Returns [`ModelError::Missing`] for types without an empty value.
    fn missing() -> Result<Self, ModelError> {
        Err(ModelError::Missing {
            path: String::new(),
        })
    }

    /// Write `self` into `slot`, changing only what differs from what the slot
    /// already holds.
    ///
    /// # Errors
    /// Returns the document's write error.
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError>;

    /// Whether writing `self` over a slot that holds `before`, as a
    /// [`store`](Self::store) left it, would change nothing: an in-memory
    /// comparison, so an action that changed one item of a long collection
    /// does not visit the others. Must only answer `true` when `store` would
    /// write no operation; the default answers `false` (unknown).
    fn stores_as(&self, before: &Self) -> bool {
        let _ = before;
        false
    }

    /// Write `self` into `slot`, which holds `before`: exactly the operations
    /// [`store`](Self::store) would write, found by comparing with `before`
    /// where `store` would read the document.
    ///
    /// `before` is what [`load`](Self::load) reads from the slot now, and the
    /// slot is as a `store` left it except where `dirty` (changes written by
    /// someone else since) says otherwise: a slot `dirty` touched is written
    /// as `store` would. The default is `store`. An action's write is
    /// replayed on every replica, so an override that writes anything else
    /// forks the document.
    ///
    /// # Errors
    /// Returns the document's write error.
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let _ = (before, dirty);
        self.store(doc, slot)
    }

    /// Bring a value read earlier up to date after the changes in `delta`,
    /// re-reading only what they touched. `value` and `id` are what the
    /// parent's slot holds now — the same object as when `self` was read, or
    /// the parent would have read it anew. The default reads the value again,
    /// which is right for scalars and short values.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the stored value has another shape.
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let _ = delta;
        *self = Self::load(doc, value, id)?;
        Ok(())
    }

    /// Append the human-readable text this value carries, for search indexing.
    fn collect_text(&self, _out: &mut Vec<String>) {}
}

/// A [`Model`] stored as the fields of a map. `#[derive(Model)]` implements
/// it for structs with named fields; a record resource keeps its fields at
/// the document root.
pub trait Fields: Model {
    /// # Errors
    /// Returns [`ModelError`] when a field has another shape.
    fn read_fields(doc: &Doc, obj: &ObjId) -> Result<Self, ModelError>;
    /// # Errors
    /// Returns the document's write error.
    fn write_fields(&self, doc: &mut Doc, obj: &ObjId) -> Result<(), ModelError>;

    /// The items of each [`Collection`](crate::Collection) field, so actions
    /// on an item can be offered on the item (see [`crate::items`]).
    /// `#[derive(Model)]` lists them; the default has none.
    fn item_collections(&self) -> Vec<crate::items::ItemCollection> {
        Vec::new()
    }

    /// How many items each [`Collection`](crate::Collection) field holds, by
    /// field, in the order of [`item_collections`](Self::item_collections),
    /// without listing them. `#[derive(Model)]` counts them; the default
    /// lists them.
    fn item_collection_sizes(&self) -> Vec<(&'static str, usize)> {
        self.item_collections()
            .iter()
            .map(|collection| (collection.field, collection.items.len()))
            .collect()
    }

    /// Write the fields to the map `obj`, which holds `before`: the
    /// operations [`write_fields`](Self::write_fields) would write, with
    /// each field compared to its value in `before` rather than read back
    /// (see [`Model::store_changed`]). `#[derive(Model)]` writes only the
    /// fields that changed; the default writes every field.
    ///
    /// # Errors
    /// Returns the document's write error.
    fn write_changed_fields(
        &self,
        before: &Self,
        doc: &mut Doc,
        obj: &ObjId,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        let _ = (before, dirty);
        self.write_fields(doc, obj)
    }

    /// Bring fields read earlier from the map `obj` up to date after the
    /// changes in `delta`, re-reading only the fields they touched.
    ///
    /// # Errors
    /// Returns [`ModelError`] when a touched field has another shape.
    fn refresh_fields(&mut self, doc: &Doc, obj: &ObjId, delta: &Delta) -> Result<(), ModelError> {
        if delta.touches(obj) {
            *self = Self::read_fields(doc, obj)?;
        }
        Ok(())
    }
}

/// Read the value under `key` of the map `obj`, or [`Model::missing`].
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn read_key<T: Model>(doc: &Doc, obj: &ObjId, key: &str) -> Result<T, ModelError> {
    match doc.get(obj, key)? {
        Some((value, id)) => T::load(doc, &value, &id),
        None => T::missing(),
    }
    .map_err(|error| error.at(key))
}

/// What a key of a map holds: its current value and the id of the op that
/// wrote it, as [`ReadDoc::get`] returns them.
pub type MapSlot = Option<(Value<'static>, ObjId)>;

/// What each of `keys` holds in the map `obj`, read in one ordered walk of
/// the map instead of one lookup per key: a lookup opens the map anew each
/// time, and a struct of `N` fields read field by field opens it `N` times.
/// The walk stops once every key is found. Each slot is exactly what
/// [`ReadDoc::get`] answers for its key (the winning value of a conflict, a
/// counter with its increments).
pub fn read_slots<const N: usize>(doc: &Doc, obj: &ObjId, keys: [&str; N]) -> [MapSlot; N] {
    let mut slots = [const { None }; N];
    let mut missing = N;
    if missing == 0 {
        return slots;
    }
    for item in doc.map_range(obj, ..) {
        let mut matched = false;
        for (key, slot) in keys.iter().zip(slots.iter_mut()) {
            if *key == item.key {
                *slot = Some((item.value.clone().into_value(), item.id()));
                missing = missing.saturating_sub(1);
                matched = true;
            }
        }
        if matched && missing == 0 {
            break;
        }
    }
    slots
}

/// `T` read from a slot [`read_slots`] found under `key`, or
/// [`Model::missing`]: what [`read_key`] answers for that key.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn load_slot<T: Model>(doc: &Doc, slot: MapSlot, key: &str) -> Result<T, ModelError> {
    match slot {
        Some((value, id)) => T::load(doc, &value, &id),
        None => T::missing(),
    }
    .map_err(|error| error.at(key))
}

/// Bring `target`, read earlier from `key` of the map `obj`, up to date
/// after the changes in `delta`: untouched keeps it, a written slot reads it
/// again, a change inside the object it holds refreshes it in place.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn refresh_key<T: Model>(
    target: &mut T,
    doc: &Doc,
    obj: &ObjId,
    key: &str,
    delta: &Delta,
) -> Result<(), ModelError> {
    match delta.key(obj, key) {
        Touch::Untouched => Ok(()),
        Touch::Written => {
            *target = read_key(doc, obj, key)?;
            Ok(())
        }
        Touch::Below => match doc.get(obj, key)? {
            Some((value, id)) => target.refresh(doc, &value, &id, delta),
            None => T::missing().map(|missing| *target = missing),
        }
        .map_err(|error| error.at(key)),
    }
}

/// Write `value` under `key` of the map `obj`.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn write_key<T: Model>(
    doc: &mut Doc,
    obj: &ObjId,
    key: &str,
    value: &T,
) -> Result<(), ModelError> {
    value
        .store(doc, Slot::Key(obj, key))
        .map_err(|error| error.at(key))
}

/// Write `value` under `key` of the map `obj`, which holds `before`: the
/// operations [`write_key`] would write, without reading back what did not
/// change. See [`Model::store_changed`].
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn write_key_changed<T: Model>(
    doc: &mut Doc,
    obj: &ObjId,
    key: &str,
    value: &T,
    before: &T,
    dirty: &Delta,
) -> Result<(), ModelError> {
    match dirty.key(obj, key) {
        Touch::Written => write_key(doc, obj, key, value),
        Touch::Untouched if value.stores_as(before) => Ok(()),
        Touch::Untouched | Touch::Below => value
            .store_changed(before, doc, Slot::Key(obj, key), dirty)
            .map_err(|error| error.at(key)),
    }
}

fn scalar<'v>(value: &'v Value<'_>) -> Option<&'v ScalarValue> {
    match value {
        Value::Scalar(scalar) => Some(scalar.as_ref()),
        Value::Object(_) => None,
    }
}

impl Model for bool {
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match scalar(value) {
            Some(ScalarValue::Boolean(value)) => Ok(*value),
            Some(ScalarValue::Null) => Ok(false),
            _ => Err(ModelError::shape("a boolean")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(false)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Boolean(*self))
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

macro_rules! integer_model {
    ($($ty:ty),*) => {$(
        impl Model for $ty {
            fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
                let wide: i128 = match scalar(value) {
                    Some(ScalarValue::Int(value)) => i128::from(*value),
                    Some(ScalarValue::Uint(value)) => i128::from(*value),
                    Some(ScalarValue::Counter(counter)) => i128::from(i64::from(counter)),
                    Some(ScalarValue::Timestamp(value)) => i128::from(*value),
                    Some(ScalarValue::Null) => 0,
                    _ => return Err(ModelError::shape("an integer")),
                };
                <$ty>::try_from(wide).map_err(|_| ModelError::invalid("integer out of range"))
            }
            fn missing() -> Result<Self, ModelError> {
                Ok(0)
            }
            fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
                let value = i64::try_from(*self)
                    .map(ScalarValue::Int)
                    .or_else(|_| u64::try_from(*self).map(ScalarValue::Uint))
                    .map_err(|_| ModelError::invalid("integer out of range"))?;
                slot.put_scalar(doc, value)
            }
            fn stores_as(&self, before: &Self) -> bool {
                self == before
            }
        }
    )*};
}
integer_model!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl Model for f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "integers written by older schemas widen to the stored float"
    )]
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match scalar(value) {
            Some(ScalarValue::F64(value)) => Ok(*value),
            Some(ScalarValue::Int(value)) => Ok(*value as f64),
            Some(ScalarValue::Uint(value)) => Ok(*value as f64),
            Some(ScalarValue::Null) => Ok(0.0),
            _ => Err(ModelError::shape("a number")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(0.0)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::F64(*self))
    }
    fn stores_as(&self, before: &Self) -> bool {
        self.to_bits() == before.to_bits()
    }
}

impl Model for f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an f32 field stores what it wrote, widened"
    )]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        f64::load(doc, value, id).map(|value| value as f32)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(0.0)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        f64::from(*self).store(doc, slot)
    }
    fn stores_as(&self, before: &Self) -> bool {
        self.to_bits() == before.to_bits()
    }
}

/// Collaborative text: stored as an Automerge `Text` object and updated by
/// splicing only the changed span, so concurrent typing in one field merges
/// character by character. A string scalar written by an older schema reads
/// too and is upgraded on the first write.
impl Model for String {
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        match value {
            Value::Object(ObjType::Text) => Ok(doc.text(id)?),
            Value::Scalar(scalar) => match scalar.as_ref() {
                ScalarValue::Str(text) => Ok(text.to_string()),
                ScalarValue::Null => Ok(Self::new()),
                _ => Err(ModelError::shape("text")),
            },
            Value::Object(_) => Err(ModelError::shape("text")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text = slot.object(doc, ObjType::Text)?;
        let current = doc.text(&text)?;
        if current != *self {
            splice_changed_span(doc, &text, &current, self)?;
        }
        Ok(())
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
    /// The one splice [`store`](Model::store) makes, found from `before`
    /// instead of reading the whole text back.
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        _dirty: &Delta,
    ) -> Result<(), ModelError> {
        let text = slot.object(doc, ObjType::Text)?;
        if before != self {
            splice_changed_span(doc, &text, before, self)?;
        }
        Ok(())
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        if !self.is_empty() {
            out.push(self.clone());
        }
    }
}

/// Replace the one span where `old` and `new` differ (after their common
/// prefix, before their common suffix) with a single splice.
///
/// One contiguous splice is what an edit is — typing, pasting, deleting a
/// selection — so it merges with a collaborator's concurrent edits elsewhere
/// in the text, and it is a change the host's undo inverts exactly. (A
/// character-level diff of two unrelated strings scatters one edit across
/// many splices.)
fn splice_changed_span(
    doc: &mut Doc,
    text: &ObjId,
    old: &str,
    new: &str,
) -> Result<(), ModelError> {
    let width = |part: &str| match doc.text_encoding() {
        automerge::TextEncoding::UnicodeCodePoint => Some(part.chars().count()),
        automerge::TextEncoding::Utf8CodeUnit => Some(part.len()),
        automerge::TextEncoding::Utf16CodeUnit => Some(part.encode_utf16().count()),
        automerge::TextEncoding::GraphemeCluster => None,
    };
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| old.len().min(new.len()), |((index, _), _)| index);
    let prefix = (0..=prefix)
        .rev()
        .find(|&index| old.is_char_boundary(index) && new.is_char_boundary(index))
        .unwrap_or(0);
    let (old_rest, new_rest) = (
        old.get(prefix..).unwrap_or(""),
        new.get(prefix..).unwrap_or(""),
    );
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let removed = old_rest
        .get(..old_rest.len().saturating_sub(suffix))
        .unwrap_or("");
    let inserted = new_rest
        .get(..new_rest.len().saturating_sub(suffix))
        .unwrap_or("");
    match (width(old.get(..prefix).unwrap_or("")), width(removed)) {
        (Some(position), Some(deleted)) => {
            let deleted =
                isize::try_from(deleted).map_err(|_| ModelError::invalid("text too long"))?;
            doc.splice_text(text, position, deleted, inserted)?;
        }
        _ => doc.update_text(text, new)?,
    }
    Ok(())
}

/// A string written whole: last writer wins. For identifiers, enum-like
/// codes and short values where character merging would be wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Atom(pub String);

impl Model for Atom {
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.0.as_str().into()))
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

impl Model for uuid::Uuid {
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match scalar(value) {
            Some(ScalarValue::Str(text)) => {
                Self::parse_str(text).map_err(|error| ModelError::invalid(error.to_string()))
            }
            _ => Err(ModelError::shape("a uuid")),
        }
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.to_string().into()))
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

/// `None` is an absent key (or a null list element).
impl<T: Model> Model for Option<T> {
    const SHAPE: crate::Shape = T::SHAPE;

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if matches!(scalar(value), Some(ScalarValue::Null)) {
            return Ok(None);
        }
        T::load(doc, value, id).map(Some)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(None)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        match self {
            Some(value) => value.store(doc, slot),
            None => slot.remove(doc),
        }
    }
    fn stores_as(&self, before: &Self) -> bool {
        match (self, before) {
            (Some(value), Some(before)) => value.stores_as(before),
            (None, None) => true,
            _ => false,
        }
    }
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        match (self, before) {
            (Some(value), Some(before)) => value.store_changed(before, doc, slot, dirty),
            _ => self.store(doc, slot),
        }
    }
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        match self {
            Some(inner) if !matches!(scalar(value), Some(ScalarValue::Null)) => {
                inner.refresh(doc, value, id, delta)
            }
            _ => {
                *self = Self::load(doc, value, id)?;
                Ok(())
            }
        }
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        if let Some(value) = self {
            value.collect_text(out);
        }
    }
}

impl<T: Model> Model for Box<T> {
    const SHAPE: crate::Shape = T::SHAPE;

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        T::load(doc, value, id).map(Box::new)
    }
    fn missing() -> Result<Self, ModelError> {
        T::missing().map(Box::new)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        (**self).store(doc, slot)
    }
    fn stores_as(&self, before: &Self) -> bool {
        (**self).stores_as(before)
    }
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        (**self).store_changed(before, doc, slot, dirty)
    }
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        (**self).refresh(doc, value, id, delta)
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        (**self).collect_text(out);
    }
}

/// A list reconciled by position: elements present on both sides are written
/// in place, the tail is inserted or deleted. Use
/// [`Collection`](crate::Collection) for items with identity, where a move or
/// a concurrent insert must not rewrite the neighbours.
impl<T: Model> Model for Vec<T> {
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::List)) {
            return Err(ModelError::shape("a list"));
        }
        (0..doc.length(id))
            .map(|index| {
                match doc.get(id, index)? {
                    Some((value, item)) => T::load(doc, &value, &item),
                    None => T::missing(),
                }
                .map_err(|error| error.at(&index.to_string()))
            })
            .collect()
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let list = slot.object(doc, ObjType::List)?;
        let existing = doc.length(&list);
        for (index, item) in self.iter().enumerate() {
            let slot = if index < existing {
                Slot::Index(&list, index)
            } else {
                Slot::Insert(&list, index)
            };
            item.store(doc, slot)
                .map_err(|error| error.at(&index.to_string()))?;
        }
        for index in (self.len()..existing).rev() {
            doc.delete(&list, index)?;
        }
        Ok(())
    }
    fn stores_as(&self, before: &Self) -> bool {
        self.len() == before.len()
            && self
                .iter()
                .zip(before)
                .all(|(value, before)| value.stores_as(before))
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        for item in self {
            item.collect_text(out);
        }
    }
}

/// A map reconciled key by key.
impl<T: Model> Model for BTreeMap<String, T> {
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a map"));
        }
        // One walk of the map rather than a lookup per key (TIL-758).
        doc.map_range(id, ..)
            .map(|item| {
                let slot = Some((item.value.clone().into_value(), item.id()));
                load_slot(doc, slot, &item.key).map(|value| (item.key.into_owned(), value))
            })
            .collect()
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let map = slot.object(doc, ObjType::Map)?;
        let stale: Vec<String> = doc
            .keys(&map)
            .filter(|key| !self.contains_key(key))
            .collect();
        for key in stale {
            doc.delete(&map, key.as_str())?;
        }
        for (key, value) in self {
            write_key(doc, &map, key, value)?;
        }
        Ok(())
    }
    fn stores_as(&self, before: &Self) -> bool {
        self.len() == before.len()
            && self
                .iter()
                .zip(before)
                .all(|((key, value), (was, before))| key == was && value.stores_as(before))
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
            match doc.get(id, key)? {
                None => {
                    self.remove(key);
                }
                Some(_) => match self.get_mut(key) {
                    Some(existing) => refresh_key(existing, doc, id, key, delta)?,
                    None => {
                        self.insert(key.to_owned(), read_key(doc, id, key)?);
                    }
                },
            }
        }
        Ok(())
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        for value in self.values() {
            value.collect_text(out);
        }
    }
}

/// An opaque value stored as one JSON string: written whole, last writer
/// wins. The escape hatch for nested data that has no meaningful concurrent
/// merge (a colour curve, a filter preset).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Json<T>(pub T);

impl<T> Model for Json<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match scalar(value) {
            Some(ScalarValue::Str(text)) => serde_json::from_str(text)
                .map(Self)
                .map_err(|error| ModelError::invalid(error.to_string())),
            Some(ScalarValue::Null) => Ok(Self::default()),
            _ => Err(ModelError::shape("a JSON string")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text = serde_json::to_string(&self.0)
            .map_err(|error| ModelError::invalid(error.to_string()))?;
        slot.put_scalar(doc, ScalarValue::Str(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn writes_become_minimal_mergeable_operations() {
        // phase: unchanged_writes_produce_no_operations
        {
            let mut doc = Doc::new();
            write_key(&mut doc, &ROOT, "title", &"hello".to_owned()).unwrap();
            write_key(&mut doc, &ROOT, "count", &3_u32).unwrap();
            write_key(&mut doc, &ROOT, "tags", &vec![1_i64, 2]).unwrap();
            doc.commit();
            write_key(&mut doc, &ROOT, "title", &"hello".to_owned()).unwrap();
            write_key(&mut doc, &ROOT, "count", &3_u32).unwrap();
            write_key(&mut doc, &ROOT, "tags", &vec![1_i64, 2]).unwrap();
            assert_eq!(doc.pending_ops(), 0);
            assert_eq!(read_key::<String>(&doc, &ROOT, "title").unwrap(), "hello");
            assert_eq!(read_key::<Vec<i64>>(&doc, &ROOT, "tags").unwrap(), [1, 2]);
            assert_eq!(read_key::<Option<u8>>(&doc, &ROOT, "absent").unwrap(), None);
        }
        // phase: text_splices_merge_concurrent_edits
        {
            let mut base = Doc::new();
            write_key(&mut base, &ROOT, "body", &"hello world".to_owned()).unwrap();
            base.commit();
            let mut left = base.fork();
            let mut right = base.fork();
            write_key(&mut left, &ROOT, "body", &"hello brave world".to_owned()).unwrap();
            write_key(&mut right, &ROOT, "body", &"hello world!".to_owned()).unwrap();
            left.merge_incoming(&mut right).unwrap();
            assert_eq!(
                read_key::<String>(&left, &ROOT, "body").unwrap(),
                "hello brave world!"
            );
        }
        // phase: a_text_edit_is_one_splice_of_the_changed_span
        {
            for (old, new) in [
                ("first thought", "second thought"),
                ("café 日本語", "café 日本 語!"),
                ("abc", ""),
                ("", "abc"),
                ("aaa", "aa"),
                ("ab", "ba"),
            ] {
                let mut doc = Doc::new();
                write_key(&mut doc, &ROOT, "t", &old.to_owned()).unwrap();
                doc.commit();
                write_key(&mut doc, &ROOT, "t", &new.to_owned()).unwrap();
                doc.commit();
                assert_eq!(read_key::<String>(&doc, &ROOT, "t").unwrap(), new);
            }
        }
    }
}
