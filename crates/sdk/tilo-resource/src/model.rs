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

use crate::action::{ActionCx, Refusal};
use crate::delta::{Delta, Touch};
use crate::path::{EditOp, Path, Segment, Settable};

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
    /// A [`check`](Model::check) refused the change, in its own words.
    #[error("{0}")]
    Refused(String),
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
            Self::Automerge(_) | Self::Refused(_) => {}
        }
        self
    }

    /// A check's refusal.
    #[must_use]
    pub fn refused(refusal: Refusal) -> Self {
        Self::Refused(refusal.0)
    }

    /// An edit whose operation the value at its path does not support.
    #[must_use]
    pub fn unsupported(op: &EditOp) -> Self {
        Self::invalid(format!("this value has no `{}` edit", op.name()))
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

    /// Add `by` to the counter in this slot, creating it holding `by`.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn increment(self, doc: &mut Doc, by: i64) -> Result<(), ModelError> {
        let counter = matches!(
            self.current(doc)?,
            Some((Value::Scalar(scalar), _)) if matches!(scalar.as_ref(), ScalarValue::Counter(_))
        );
        if by == 0 && counter {
            return Ok(());
        }
        match (self, counter) {
            (Slot::Key(obj, key), true) => doc.increment(obj, key, by)?,
            (Slot::Index(obj, index), true) => doc.increment(obj, index, by)?,
            (slot, _) => {
                let start = match slot.current(doc)? {
                    Some((value, _)) => match scalar(&value) {
                        Some(ScalarValue::Int(value)) => *value,
                        _ => 0,
                    },
                    None => 0,
                };
                let value = ScalarValue::counter(start.saturating_add(by));
                match slot {
                    Slot::Key(obj, key) => doc.put(obj, key, value)?,
                    Slot::Index(obj, index) => doc.put(obj, index, value)?,
                    Slot::Insert(obj, index) => doc.insert(obj, index, value)?,
                }
            }
        }
        Ok(())
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

    /// Whether this type or any value inside it has a `#[model(check)]`
    /// invariant; [`Self::check`] has nothing to do otherwise.
    const CHECKS: bool = false;

    /// The typed paths into a value of this type, rooted in the record `R`:
    /// a struct of field paths for a record, the [`Path`] itself for a value
    /// edited whole or through its own operations.
    type Paths<R>: Clone;

    /// The paths of the value at `path`.
    fn paths_at<R>(path: Path<R, Self>) -> Self::Paths<R>;

    /// The value as JSON: what [`EditOp::Set`] carries and history shows.
    fn to_json(&self) -> serde_json::Value;

    /// A value from its [`Self::to_json`] form.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `value` has another shape.
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError>;

    /// Apply the field edit `op` at `path`, below the value in `slot`,
    /// touching only what it changes, then run the invariants of the values
    /// it touched. The default edits a value only as a whole.
    ///
    /// # Errors
    /// Returns why the path or operation does not fit, or a refusal.
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let _ = cx;
        edit_whole::<Self>(doc, slot, path, op)
    }

    /// The field edits that turn `self` into `after`, at `path` below the
    /// record: what a surface that edited a local copy submits. The default
    /// sets the value whole when it changed; containers and records edit
    /// only what differs, item by item and field by field.
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        let value = after.to_json();
        if self.to_json() != value {
            out.push(crate::Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    }

    /// Read everything this value holds, for a field marked
    /// `#[model(whole)]`: a value read lazily (rich text reads its
    /// paragraphs as views ask for them) reads all of it, so every reader of
    /// the record — a verb, search, `read_content` — sees all of it. The
    /// default has nothing more to read.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds the value.
    fn read_whole(&mut self, doc: &Doc) -> Result<(), ModelError> {
        let _ = doc;
        Ok(())
    }

    /// Run every `#[model(check)]` invariant of this value and the values
    /// inside it.
    ///
    /// # Errors
    /// Returns the first refusal.
    fn check(&self) -> Result<(), Refusal> {
        Ok(())
    }

    /// What names this value for people (`#[model(label)]`): history, search,
    /// agents and item resources use it.
    fn label(&self) -> Option<String> {
        None
    }

    /// Whether two edits in the own vocabulary of the value at `path` below
    /// this type are one gesture ([`Operations`](crate::Operations)); `None`
    /// when nothing there has one.
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        let _ = (path, previous, next);
        None
    }

    /// Whether a gesture may go on after the edit `op` at `path`.
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        let _ = (path, op);
        None
    }
    /// What the edit `op` at `path` below this type did, in the history
    /// entry's terms ([`Operations::describe`](crate::Operations::describe)).
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        let _ = (path, op);
        None
    }

    /// Where the actor of the edit `op` at `path` below this value is after
    /// it applied.
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        let _ = (path, op);
        None
    }

    /// The schemas of the own vocabularies of the values below this type,
    /// by path pattern from `prefix` (`*` for every item).
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        let _ = (prefix, out);
    }

    /// The value at `path` below this one, if it is there. A value with
    /// nothing inside it is found only at the empty path.
    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        path.is_empty().then_some(self as &dyn std::any::Any)
    }

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
    /// The collection fields whose items are published as item resources
    /// (`Collection`, `Keyed`, `Ordered`); `#[derive(Model)]` lists them.
    const ITEM_FIELDS: &'static [&'static str] = &[];

    /// # Errors
    /// Returns [`ModelError`] when a field has another shape.
    fn read_fields(doc: &Doc, obj: &ObjId) -> Result<Self, ModelError>;
    /// # Errors
    /// Returns the document's write error.
    fn write_fields(&self, doc: &mut Doc, obj: &ObjId) -> Result<(), ModelError>;

    /// The items of each [`Collection`](crate::Collection) field, so actions
    /// on an item can be offered on the item (see [`crate::items`]), at most
    /// [`ITEM_PAGE`](crate::items::ITEM_PAGE) of each. `#[derive(Model)]`
    /// lists them; the default has none.
    fn item_collections(&self) -> Vec<crate::items::ItemCollection> {
        Vec::new()
    }

    /// The label the item `id` of the collection field `field` is offered
    /// under, or `None` when there is no such item.
    fn item_label(&self, field: &str, id: &crate::Id) -> Option<String> {
        let _ = (field, id);
        None
    }

    /// Every item of the collection fields whose type has a
    /// `#[model(label)]`, with its text: what search indexes by default.
    fn item_search(&self) -> Vec<crate::items::ItemText> {
        Vec::new()
    }

    /// Apply the field edit `op` at `path` below the map `obj`; see
    /// [`Model::edit`]. `#[derive(Model)]` routes each field segment to its
    /// field; the default edits nothing.
    ///
    /// # Errors
    /// Returns why the path or operation does not fit, or a refusal.
    fn edit_fields(
        doc: &mut Doc,
        obj: &ObjId,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let _ = (doc, obj, path, cx);
        Err(ModelError::unsupported(op))
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

/// [`Model::edit`] for a value edited only as a whole: `set` at its own
/// path, checked before it is written.
///
/// # Errors
/// Returns why the path or operation does not fit, or a refusal.
pub fn edit_whole<T: Model>(
    doc: &mut Doc,
    slot: Slot<'_>,
    path: &[Segment],
    op: &EditOp,
) -> Result<(), ModelError> {
    if let Some(segment) = path.first() {
        return Err(ModelError::invalid(format!(
            "nothing inside this value is called `{segment}`"
        )));
    }
    match op {
        EditOp::Set { value } => {
            let value = T::from_json(value)?;
            value.check().map_err(ModelError::refused)?;
            value.store(doc, slot)
        }
        other => Err(ModelError::unsupported(other)),
    }
}

/// The JSON shape error of [`Model::from_json`].
pub(crate) fn json_shape(expected: &'static str) -> ModelError {
    ModelError::shape(expected)
}

/// Implements the path items of a type edited whole or through its own
/// operations: its paths are the [`Path`] itself.
#[macro_export]
macro_rules! leaf_paths {
    () => {
        type Paths<R> = $crate::Path<R, Self>;
        fn paths_at<R>(path: $crate::Path<R, Self>) -> $crate::Path<R, Self> {
            path
        }
    };
}
pub(crate) use crate::leaf_paths;

/// Implements [`Model`] (and [`Settable`]) for serde types stored whole as
/// one JSON string, last writer wins: values with no meaningful concurrent
/// merge (a tone curve, a keyframe track). The types need `Serialize`,
/// `Deserialize` and `Default` (the value of an absent slot).
#[macro_export]
macro_rules! json_model {
    ($($ty:ty),* $(,)?) => {$(
        impl $crate::Settable for $ty {}
        impl $crate::Model for $ty {
            $crate::leaf_paths!();
            fn to_json(&self) -> $crate::__private::serde_json::Value {
                $crate::__private::serde_json::to_value(self)
                    .unwrap_or($crate::__private::serde_json::Value::Null)
            }
            fn from_json(
                value: &$crate::__private::serde_json::Value,
            ) -> ::core::result::Result<Self, $crate::ModelError> {
                if value.is_null() {
                    return ::core::result::Result::Ok(<Self as ::core::default::Default>::default());
                }
                $crate::__private::serde_json::from_value(value.clone())
                    .map_err(|error| $crate::ModelError::invalid(error.to_string()))
            }
            fn load(
                doc: &$crate::Doc,
                value: &$crate::__private::Value<'_>,
                id: &$crate::__private::ObjId,
            ) -> ::core::result::Result<Self, $crate::ModelError> {
                let $crate::Json(inner) = <$crate::Json<Self> as $crate::Model>::load(doc, value, id)?;
                ::core::result::Result::Ok(inner)
            }
            fn missing() -> ::core::result::Result<Self, $crate::ModelError> {
                ::core::result::Result::Ok(<Self as ::core::default::Default>::default())
            }
            fn store(
                &self,
                doc: &mut $crate::Doc,
                slot: $crate::Slot<'_>,
            ) -> ::core::result::Result<(), $crate::ModelError> {
                let text = $crate::__private::serde_json::to_string(self)
                    .map_err(|error| $crate::ModelError::invalid(error.to_string()))?;
                slot.put_scalar(doc, $crate::__private::ScalarValue::Str(text.into()))
            }
        }
    )*};
}

fn scalar<'v>(value: &'v Value<'_>) -> Option<&'v ScalarValue> {
    match value {
        Value::Scalar(scalar) => Some(scalar.as_ref()),
        Value::Object(_) => None,
    }
}

impl Settable for bool {}
impl Model for bool {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Bool(*self)
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Bool(value) => Ok(*value),
            serde_json::Value::Null => Ok(false),
            _ => Err(json_shape("a boolean")),
        }
    }
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
}

macro_rules! integer_model {
    ($($ty:ty),*) => {$(
        impl Settable for $ty {}
        impl Model for $ty {
            leaf_paths!();
            fn to_json(&self) -> serde_json::Value {
                serde_json::Value::from(*self)
            }
            fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
                let wide: i128 = match value {
                    serde_json::Value::Number(number) => number
                        .as_i64()
                        .map(i128::from)
                        .or_else(|| number.as_u64().map(i128::from))
                        .ok_or_else(|| json_shape("an integer"))?,
                    serde_json::Value::Null => 0,
                    _ => return Err(json_shape("an integer")),
                };
                <$ty>::try_from(wide).map_err(|_| ModelError::invalid("integer out of range"))
            }
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
        }
    )*};
}
integer_model!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl Settable for f64 {}
impl Model for f64 {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Number::from_f64(*self)
            .map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Number(number) => {
                number.as_f64().ok_or_else(|| json_shape("a number"))
            }
            serde_json::Value::Null => Ok(0.0),
            _ => Err(json_shape("a number")),
        }
    }
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
}

impl Settable for f32 {}
impl Model for f32 {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        f64::from(*self).to_json()
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an f32 field stores what it wrote, widened"
    )]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        f64::from_json(value).map(|value| value as f32)
    }
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
}

/// Collaborative text: stored as an Automerge `Text` object and updated by
/// splicing only the changed span, so concurrent typing in one field merges
/// character by character. A string scalar written by an older schema reads
/// too and is upgraded on the first write.
impl Settable for String {}
impl Model for String {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.clone())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => Ok(text.clone()),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(json_shape("text")),
        }
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        let edit = match op {
            EditOp::Splice { at, delete, insert } if path.is_empty() => crate::TextEdit::Splice {
                at: *at,
                delete: *delete,
                insert: insert.clone(),
            },
            EditOp::Replace { text } if path.is_empty() => {
                crate::TextEdit::ReplaceText { text: text.clone() }
            }
            _ => return edit_whole::<Self>(doc, slot, path, op),
        };
        let text = slot.object(doc, ObjType::Text)?;
        edit.apply_under(doc, &text).map_err(|error| match error {
            tilo_actions::ActionError::Refused(reason) => ModelError::Refused(reason),
            other => ModelError::invalid(other.to_string()),
        })
    }
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

impl Settable for Atom {}
impl Model for Atom {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.0.clone())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.0.as_str().into()))
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        if !self.0.is_empty() {
            out.push(self.0.clone());
        }
    }
}

impl Settable for uuid::Uuid {}
impl Model for uuid::Uuid {
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_string())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => {
                Self::parse_str(text).map_err(|error| ModelError::invalid(error.to_string()))
            }
            _ => Err(json_shape("a uuid")),
        }
    }
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
}

/// `None` is an absent key (or a null list element).
impl<T: Settable> Settable for Option<T> {}
impl<T: Model> Model for Option<T> {
    const SHAPE: crate::Shape = T::SHAPE;
    const CHECKS: bool = T::CHECKS;
    leaf_paths!();
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        T::op_coalesces(path, previous, next)
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        T::op_continuable(path, op)
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        T::op_describe(path, op)
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        self.as_ref()?.op_lands(path, op)
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        T::op_schemas(prefix, out);
    }
    fn to_json(&self) -> serde_json::Value {
        self.as_ref()
            .map_or(serde_json::Value::Null, Model::to_json)
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Null => Ok(None),
            value => T::from_json(value).map(Some),
        }
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if path.is_empty() && matches!(op, EditOp::Set { .. }) {
            return edit_whole::<Self>(doc, slot, path, op);
        }
        match slot.current(doc)? {
            Some((value, _)) if !matches!(scalar(&value), Some(ScalarValue::Null)) => {
                T::edit(doc, slot, path, op, cx)
            }
            _ => Err(ModelError::invalid("the value is absent")),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        self.as_ref().map_or(Ok(()), Model::check)
    }
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        match (self, after) {
            (Some(before), Some(next)) => before.diff(next, path, out),
            (None, None) => {}
            _ => out.push(crate::Edit::new(
                path.to_vec(),
                EditOp::Set {
                    value: after.to_json(),
                },
            )),
        }
    }
    fn label(&self) -> Option<String> {
        self.as_ref().and_then(Model::label)
    }
    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        if path.is_empty() {
            return Some(self);
        }
        self.as_ref().and_then(|value| value.get_at(path))
    }

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

impl<T: Settable> Settable for Box<T> {}
impl<T: Model> Model for Box<T> {
    const SHAPE: crate::Shape = T::SHAPE;
    const CHECKS: bool = T::CHECKS;
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        T::op_coalesces(path, previous, next)
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        T::op_continuable(path, op)
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        T::op_describe(path, op)
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        (**self).op_lands(path, op)
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        T::op_schemas(prefix, out);
    }
    type Paths<R> = T::Paths<R>;
    fn paths_at<R>(path: Path<R, Self>) -> T::Paths<R> {
        T::paths_at(path.cast())
    }
    fn to_json(&self) -> serde_json::Value {
        (**self).to_json()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        T::from_json(value).map(Box::new)
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        T::edit(doc, slot, path, op, cx)
    }
    fn check(&self) -> Result<(), Refusal> {
        (**self).check()
    }
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        (**self).diff(after, path, out);
    }
    fn label(&self) -> Option<String> {
        (**self).label()
    }
    fn get_at(&self, path: &[Segment]) -> Option<&dyn std::any::Any>
    where
        Self: 'static,
    {
        (**self).get_at(path)
    }

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        T::load(doc, value, id).map(Box::new)
    }
    fn missing() -> Result<Self, ModelError> {
        T::missing().map(Box::new)
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        (**self).store(doc, slot)
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
impl<T: Model> Settable for Vec<T> {}
impl<T: Model> Model for Vec<T> {
    const CHECKS: bool = T::CHECKS;
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.iter().map(Model::to_json).collect())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    T::from_json(item).map_err(|error| error.at(&index.to_string()))
                })
                .collect(),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(json_shape("a list")),
        }
    }
    fn check(&self) -> Result<(), Refusal> {
        if T::CHECKS {
            self.iter().try_for_each(Model::check)?;
        }
        Ok(())
    }
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
    fn collect_text(&self, out: &mut Vec<String>) {
        for item in self {
            item.collect_text(out);
        }
    }
}

/// A fixed number of values, stored as a list.
impl<T: Model, const N: usize> Settable for [T; N] {}
impl<T: Model, const N: usize> Model for [T; N] {
    const CHECKS: bool = T::CHECKS;
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.iter().map(Model::to_json).collect())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let items = Vec::<T>::from_json(value)?;
        let count = items.len();
        items
            .try_into()
            .map_err(|_| ModelError::invalid(format!("expected {N} values, not {count}")))
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        let items = Vec::<T>::load(doc, value, id)?;
        let count = items.len();
        items
            .try_into()
            .map_err(|_| ModelError::invalid(format!("expected {N} values, not {count}")))
    }
    fn missing() -> Result<Self, ModelError> {
        let items = (0..N)
            .map(|_| T::missing())
            .collect::<Result<Vec<_>, _>>()?;
        items
            .try_into()
            .map_err(|_| ModelError::invalid("an empty array"))
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
        for index in (N..existing).rev() {
            doc.delete(&list, index)?;
        }
        Ok(())
    }
}

/// A map reconciled key by key.
impl<T: Model> Settable for BTreeMap<String, T> {}
impl<T: Model> Model for BTreeMap<String, T> {
    const CHECKS: bool = T::CHECKS;
    leaf_paths!();
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Key(_), rest)) => T::op_coalesces(rest, previous, next),
            _ => None,
        }
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        match path.split_first() {
            Some((Segment::Key(_), rest)) => T::op_continuable(rest, op),
            _ => None,
        }
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        match path.split_first() {
            Some((Segment::Key(_), rest)) => T::op_describe(rest, op),
            _ => None,
        }
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        match path.split_first() {
            Some((Segment::Key(key), rest)) => self.get(key)?.op_lands(rest, op),
            _ => None,
        }
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        T::op_schemas(&format!("{prefix}*."), out);
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.iter()
                .map(|(key, value)| (key.clone(), value.to_json()))
                .collect(),
        )
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Object(entries) => entries
                .iter()
                .map(|(key, value)| {
                    T::from_json(value)
                        .map(|value| (key.clone(), value))
                        .map_err(|error| error.at(key))
                })
                .collect(),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(json_shape("a map")),
        }
    }
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        match (path.split_first(), op) {
            (None, EditOp::RemoveKey { key }) => {
                let map = slot.object(doc, ObjType::Map)?;
                Slot::Key(&map, key).remove(doc)
            }
            (Some((Segment::Key(key), rest)), op) => {
                let map = slot.object(doc, ObjType::Map)?;
                if rest.is_empty() && matches!(op, EditOp::Set { .. }) {
                    return edit_whole::<T>(doc, Slot::Key(&map, key), rest, op)
                        .map_err(|error| error.at(key));
                }
                if doc.get(&map, key.as_str())?.is_none() {
                    return Err(ModelError::invalid(format!("no entry `{key}`")));
                }
                T::edit(doc, Slot::Key(&map, key), rest, op, cx).map_err(|error| error.at(key))
            }
            _ => edit_whole::<Self>(doc, slot, path, op),
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
            Some((Segment::Key(key), rest)) => self.get(key)?.get_at(rest),
            Some(_) => None,
        }
    }
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        for (key, before) in self {
            let mut at = path.to_vec();
            at.push(Segment::Key(key.clone()));
            match after.get(key) {
                Some(next) => before.diff(next, &at, out),
                None => out.push(crate::Edit::new(
                    path.to_vec(),
                    EditOp::RemoveKey { key: key.clone() },
                )),
            }
        }
        for (key, next) in after {
            if !self.contains_key(key) {
                let mut at = path.to_vec();
                at.push(Segment::Key(key.clone()));
                out.push(crate::Edit::new(
                    at,
                    EditOp::Set {
                        value: next.to_json(),
                    },
                ));
            }
        }
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Err(ModelError::shape("a map"));
        }
        doc.keys(id)
            .map(|key| read_key(doc, id, &key).map(|value| (key, value)))
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

impl<T> Settable for Json<T> where T: serde::Serialize + serde::de::DeserializeOwned + Default {}
impl<T> Model for Json<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.0).unwrap_or(serde_json::Value::Null)
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        serde_json::from_value(value.clone())
            .map(Self)
            .map_err(|error| ModelError::invalid(error.to_string()))
    }
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
