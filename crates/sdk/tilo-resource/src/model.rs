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
    #[inline(never)]
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
pub trait Model: Sized + 'static {
    /// How the value is laid out in the document; see [`Shape`](crate::Shape).
    /// Types written whole keep the default.
    const SHAPE: crate::Shape = crate::Shape::Value;

    /// Whether this type or any value inside it has a `#[model(check)]`
    /// invariant; [`Self::check`] has nothing to do otherwise.
    const CHECKS: bool = false;

    /// Whether this type or a value inside it has a vocabulary of its own
    /// ([`Operations`](crate::Operations), rich text's): a record routes
    /// those edits' gestures, landing places and schemas only into such
    /// fields, and never instantiates the routing for the others.
    const OPS: bool = false;

    /// Whether `null` is a value of the type: an absent `Option`. A field
    /// edit setting any other type to `null` is refused.
    const NULLABLE: bool = false;

    /// Whether [`store`](Self::store) writes one scalar, with nothing inside
    /// it that a concurrent writer could add to. A record made just now
    /// leaves out such a field holding its [`missing`](Self::missing) value:
    /// an absent key reads as it. An object (text, a map, a list) is always
    /// made with its record, so two writers fill the same one.
    const SCALAR: bool = false;

    /// The type's functions as data, which a record holding it reaches it
    /// through ([`crate::table`]): by default its own functions, erased.
    const TABLE: &'static crate::table::ModelTable = &crate::table::TableOf::<Self>::TABLE;

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
    #[inline(never)]
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
    /// only what differs, item by item and field by field. A value that
    /// would store as it is ([`Self::stores_as`]) is compared in memory,
    /// without turning either side into JSON.
    #[inline(never)]
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        if after.stores_as(self) {
            return;
        }
        let value = after.to_json();
        if self.to_json() != value {
            out.push(crate::Edit::new(path.to_vec(), EditOp::Set { value }));
        }
    }

    /// The item collections inside this value, when it is an item of a
    /// collection itself (a slide's elements); `#[derive(Model)]` lists a
    /// record's, the default has none.
    fn nested_collections(&self) -> Vec<crate::items::ItemCollection> {
        Vec::new()
    }

    /// The JSON Schema of this value's JSON form ([`Self::to_json`]), as
    /// the published field edit describes what a path takes; the default
    /// accepts any value.
    fn value_schema() -> serde_json::Value {
        serde_json::json!({})
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

    /// Whether writing `self` over a slot that holds `before`, as a
    /// [`store`](Self::store) left it, would change nothing: an in-memory
    /// comparison, so an action that changed one item of a long collection
    /// does not visit the others. Must only answer `true` when `store` would
    /// write no operation; the default answers `false` (unknown).
    #[inline(never)]
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
    #[inline(never)]
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
    #[inline(never)]
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
#[inline(never)]
pub fn read_key<T: Model>(doc: &Doc, obj: &ObjId, key: &str) -> Result<T, ModelError> {
    let slot = get_slot(doc, obj, key)?;
    load_at(doc, slot.as_ref().map(|(value, id)| (value, id)), key)
}

/// What `key` of the map `obj` holds, as [`ReadDoc::get`] answers it.
///
/// Every lookup and walk of the document here is a function that is not
/// generic, and the generic ones only call it. A generic function is
/// compiled again into each app that instantiates it, and Automerge's
/// generic lookup and op-tree walk with it; these are compiled once, into
/// the shared runtime every app maps.
#[inline(never)]
pub(crate) fn get_slot<'doc>(
    doc: &'doc Doc,
    obj: &ObjId,
    key: &str,
) -> Result<Option<(Value<'doc>, ObjId)>, ModelError> {
    Ok(doc.get(obj, key)?)
}

/// What `index` of the list `obj` holds; not generic, as [`get_slot`].
#[inline(never)]
pub(crate) fn get_item<'doc>(
    doc: &'doc Doc,
    obj: &ObjId,
    index: usize,
) -> Result<Option<(Value<'doc>, ObjId)>, ModelError> {
    Ok(doc.get(obj, index)?)
}

/// `T` read from what a key holds, or [`Model::missing`] when it holds
/// nothing: the one generic body behind [`read_key`] and [`load_slot`].
fn load_at<T: Model>(
    doc: &Doc,
    slot: Option<(&Value<'_>, &ObjId)>,
    key: &str,
) -> Result<T, ModelError> {
    match slot {
        Some((value, id)) => T::load(doc, value, id),
        None => T::missing(),
    }
    .located(key)
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
    fill_slots(doc, obj, &keys, &mut slots);
    slots
}

/// [`read_slots`]' walk, out of its generic shell so that it is compiled once
/// into the shared runtime rather than into every app that derives a
/// [`Model`] with another number of fields.
#[inline(never)]
pub(crate) fn fill_slots(doc: &Doc, obj: &ObjId, keys: &[&str], slots: &mut [MapSlot]) {
    let mut missing = keys.len();
    if missing == 0 {
        return;
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
}

/// Every key of the map `obj` with what it holds, in key order, in one walk
/// of the map. Not generic, for the reason [`fill_slots`] is not.
#[inline(never)]
pub(crate) fn map_slots(doc: &Doc, obj: &ObjId) -> Vec<(String, MapSlot)> {
    doc.map_range(obj, ..)
        .map(|item| {
            let slot = Some((item.value.clone().into_value(), item.id()));
            (item.key.into_owned(), slot)
        })
        .collect()
}

/// The slot at `index` of what [`read_slots`] returned, taken out of it, or
/// an empty slot past its end. Not generic over the number of slots, so the
/// derived reads share one copy.
#[must_use]
#[inline(never)]
pub fn take_slot(slots: &mut [MapSlot], index: usize) -> MapSlot {
    slots.get_mut(index).and_then(Option::take)
}

/// `T` read from a slot [`read_slots`] found under `key`, or
/// [`Model::missing`]: what [`read_key`] answers for that key.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
pub fn load_slot<T: Model>(doc: &Doc, slot: MapSlot, key: &str) -> Result<T, ModelError> {
    match slot {
        Some((value, id)) => load_at(doc, Some((&value, &id)), key),
        None => load_at(doc, None, key),
    }
}

/// Bring `target`, read earlier from `key` of the map `obj`, up to date
/// after the changes in `delta`: untouched keeps it, a written slot reads it
/// again, a change inside the object it holds refreshes it in place.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
#[inline(never)]
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
        Touch::Below => match get_slot(doc, obj, key)? {
            Some((value, id)) => target.refresh(doc, &value, &id, delta),
            None => match T::missing() {
                Ok(missing) => {
                    *target = missing;
                    Ok(())
                }
                Err(error) => Err(error),
            },
        }
        .located(key),
    }
}

/// An error located at a key, as one function per value type rather than
/// a closure per call.
pub trait LocatedAt<T> {
    /// The error, if any, located at `key`.
    ///
    /// # Errors
    /// The error, located.
    fn located(self, key: &str) -> Result<T, ModelError>;
}

impl<T> LocatedAt<T> for Result<T, ModelError> {
    fn located(self, key: &str) -> Result<T, ModelError> {
        match self {
            Ok(value) => Ok(value),
            Err(error) => Err(error.at(key)),
        }
    }
}

/// A value written through a vtable: code storing values of many types
/// (the items of every collection) compiles once rather than per type.
pub(crate) trait Stores {
    fn store_to(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError>;
}

impl<T: Model> Stores for T {
    fn store_to(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.store(doc, slot)
    }
}

/// Write `value` under `key` of the map `obj`.
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
#[inline(never)]
pub fn write_key<T: Model>(
    doc: &mut Doc,
    obj: &ObjId,
    key: &str,
    value: &T,
) -> Result<(), ModelError> {
    value.store(doc, Slot::Key(obj, key)).located(key)
}

/// [`Model::edit`] for a value edited only as a whole: `set` at its own
/// path, checked before it is written.
///
/// # Errors
/// Returns why the path or operation does not fit, or a refusal.
#[inline(never)]
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
        EditOp::Set { value } if value.is_null() && !T::NULLABLE => Err(ModelError::invalid(
            "`null` is no value of this field: set a value of its type",
        )),
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

/// Implements every [`Model`] function a type's [`TABLE`](Model::TABLE)
/// runs by forwarding it there: one set of functions for every type of its
/// kind (a container over its item type's table, a unit enum, a value
/// stored as JSON text).
#[doc(hidden)]
#[macro_export]
macro_rules! __table_functions {
    () => {
        #[inline]
        fn op_coalesces(
            path: &[$crate::path::Segment],
            previous: &$crate::__private::serde_json::Value,
            next: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<bool> {
            $crate::table::Run(Self::TABLE).op_coalesces(path, previous, next)
        }
        #[inline]
        fn op_continuable(
            path: &[$crate::path::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<bool> {
            $crate::table::Run(Self::TABLE).op_continuable(path, op)
        }
        #[inline]
        fn op_describe(
            path: &[$crate::path::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<$crate::path::OpDescription> {
            $crate::table::Run(Self::TABLE).op_describe(path, op)
        }
        #[inline]
        fn op_lands(
            &self,
            path: &[$crate::path::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<$crate::Place> {
            $crate::table::Run(Self::TABLE).op_lands(self, path, op)
        }
        #[inline]
        fn op_schemas(
            prefix: &str,
            out: &mut ::std::vec::Vec<(
                ::std::string::String,
                $crate::__private::serde_json::Value,
            )>,
        ) {
            $crate::table::Run(Self::TABLE).op_schemas(prefix, out);
        }
        #[inline]
        fn to_json(&self) -> $crate::__private::serde_json::Value {
            $crate::table::Run(Self::TABLE).to_json(self)
        }
        #[inline]
        fn from_json(
            value: &$crate::__private::serde_json::Value,
        ) -> ::core::result::Result<Self, $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).from_json(value)
        }
        #[inline]
        fn edit(
            doc: &mut $crate::model::Doc,
            slot: $crate::model::Slot<'_>,
            path: &[$crate::path::Segment],
            op: &$crate::path::EditOp,
            cx: &mut $crate::action::ActionCx,
        ) -> ::core::result::Result<(), $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).edit(doc, slot, path, op, cx)
        }
        #[inline]
        fn check(&self) -> ::core::result::Result<(), $crate::action::Refusal> {
            $crate::table::Run(Self::TABLE).check(self)
        }
        #[inline]
        fn get_at(
            &self,
            path: &[$crate::path::Segment],
        ) -> ::core::option::Option<&dyn ::core::any::Any> {
            $crate::table::Run(Self::TABLE).get_at(self, path)
        }
        #[inline]
        fn diff<R>(
            &self,
            after: &Self,
            path: &[$crate::path::Segment],
            out: &mut ::std::vec::Vec<$crate::Edit<R>>,
        ) {
            $crate::table::Run(Self::TABLE).diff(self, after, path, out);
        }
        #[inline]
        fn load(
            doc: &$crate::model::Doc,
            value: &$crate::__private::Value<'_>,
            id: &$crate::__private::ObjId,
        ) -> ::core::result::Result<Self, $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).load(doc, value, id)
        }
        #[inline]
        fn store(
            &self,
            doc: &mut $crate::model::Doc,
            slot: $crate::model::Slot<'_>,
        ) -> ::core::result::Result<(), $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).store(self, doc, slot)
        }
        #[inline]
        fn stores_as(&self, before: &Self) -> bool {
            $crate::table::Run(Self::TABLE).stores_as(self, before)
        }
        #[inline]
        fn store_changed(
            &self,
            before: &Self,
            doc: &mut $crate::model::Doc,
            slot: $crate::model::Slot<'_>,
            dirty: &$crate::delta::Delta,
        ) -> ::core::result::Result<(), $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).store_changed(self, before, doc, slot, dirty)
        }
        #[inline]
        fn refresh(
            &mut self,
            doc: &$crate::model::Doc,
            value: &$crate::__private::Value<'_>,
            id: &$crate::__private::ObjId,
            delta: &$crate::delta::Delta,
        ) -> ::core::result::Result<(), $crate::model::ModelError> {
            $crate::table::Run(Self::TABLE).refresh(self, doc, value, id, delta)
        }
        #[inline]
        fn collect_text(&self, out: &mut ::std::vec::Vec<::std::string::String>) {
            $crate::table::Run(Self::TABLE).collect_text(self, out);
        }
    };
}

/// Implements [`Model`] (and [`Settable`]) for serde types stored whole as
/// one JSON string, last writer wins: values with no meaningful concurrent
/// merge (a tone curve, a keyframe track). The types need `Serialize`,
/// `Deserialize`, `Default` (the value of an absent slot) and `PartialEq`
/// holding only between values of equal JSON forms, as a derived one does:
/// the edits between two copies compare each value in memory, and serialise
/// only the ones that differ.
#[macro_export]
macro_rules! json_model {
    ($($ty:ty),* $(,)?) => {$(
        impl $crate::Settable for $ty {}
        impl $crate::Model for $ty {
            $crate::leaf_paths!();
            // Its serde form decides; `null` reads as its default.
            const NULLABLE: bool = true;
            // Run by one set of functions for every value stored as JSON text.
            const TABLE: &'static $crate::table::ModelTable =
                &$crate::table::ModelTable::of_json(&$crate::table::JsonOf::<$ty>::COMPARED);
            $crate::__table_functions!();
            fn missing() -> ::core::result::Result<Self, $crate::ModelError> {
                ::core::result::Result::Ok(<Self as ::core::default::Default>::default())
            }
        }
    )*};
}

/// Write `value` under `key` of the map `obj`, which holds `before`: the
/// operations [`write_key`] would write, without reading back what did not
/// change. See [`Model::store_changed`].
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
#[inline(never)]
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
            .located(key),
    }
}

/// The key under which a data enum (`#[derive(Model)]` on an enum with
/// fields) names its variant, beside the variant's fields. No Rust field can
/// have this name, so it never collides with one.
pub const VARIANT_KEY: &str = "$variant";

/// The map a data enum's variant `variant` is stored in, in `slot`: made
/// when the slot holds anything else, its variant name written, and the
/// fields of another variant it held before removed, so concurrent edits of
/// the same variant merge field by field and a change of variant is one
/// last-writer-wins write of its name.
///
/// # Errors
/// Returns the document's write error.
#[doc(hidden)]
#[inline(never)]
pub fn variant_map(
    doc: &mut Doc,
    slot: Slot<'_>,
    variant: &str,
    fields: &[&str],
) -> Result<ObjId, ModelError> {
    let obj = slot.object(doc, ObjType::Map)?;
    Slot::Key(&obj, VARIANT_KEY).put_scalar(doc, ScalarValue::Str(variant.into()))?;
    let stale: Vec<String> = doc
        .keys(&obj)
        .filter(|key| key.as_str() != VARIANT_KEY && !fields.contains(&key.as_str()))
        .collect();
    for key in stale {
        doc.delete(&obj, key.as_str())?;
    }
    Ok(obj)
}

/// The variant a data enum stored at `value` (the object `id`) names.
///
/// # Errors
/// Returns a shape error when the slot holds no map naming a variant.
#[doc(hidden)]
#[inline(never)]
pub fn stored_variant(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<String, ModelError> {
    if !matches!(value, Value::Object(ObjType::Map)) {
        return Err(ModelError::shape("a map naming its variant"));
    }
    match doc.get(id, VARIANT_KEY)? {
        Some((Value::Scalar(name), _)) => match name.as_ref() {
            ScalarValue::Str(name) => Ok(name.to_string()),
            _ => Err(ModelError::shape("a variant name").at(VARIANT_KEY)),
        },
        _ => Err(ModelError::shape("a variant name").at(VARIANT_KEY)),
    }
}

/// The variant name and fields of a data enum's JSON form,
/// `{"$variant": name, field: value, ..}`.
///
/// # Errors
/// Returns a shape error for any other JSON.
#[doc(hidden)]
#[inline(never)]
pub fn json_variant(
    value: &serde_json::Value,
) -> Result<(&str, &serde_json::Map<String, serde_json::Value>), ModelError> {
    let serde_json::Value::Object(fields) = value else {
        return Err(json_shape("an object naming its variant"));
    };
    match fields.get(VARIANT_KEY) {
        Some(serde_json::Value::String(name)) => Ok((name.as_str(), fields)),
        _ => Err(json_shape("a variant name").at(VARIANT_KEY)),
    }
}

/// The field `key` of a data enum's JSON form, or [`Model::missing`].
///
/// # Errors
/// Returns [`ModelError`] located at `key`.
#[doc(hidden)]
pub fn json_field<T: Model>(
    fields: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<T, ModelError> {
    match fields.get(key) {
        Some(value) => T::from_json(value),
        None => T::missing(),
    }
    .located(key)
}

/// A data enum's JSON form: its variant name and its fields.
#[doc(hidden)]
#[must_use]
#[inline(never)]
pub fn json_of_variant(variant: &str, fields: Vec<(&str, serde_json::Value)>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(VARIANT_KEY.to_owned(), serde_json::Value::from(variant));
    for (key, value) in fields {
        map.insert(key.to_owned(), value);
    }
    serde_json::Value::Object(map)
}

/// That a data enum names a variant it does not have.
#[doc(hidden)]
#[must_use]
#[inline(never)]
pub fn unknown_variant(variant: &str) -> ModelError {
    ModelError::invalid(format!("no variant `{variant}`")).at(VARIANT_KEY)
}

/// The JSON Schema of a data enum's variant `variant` with the fields
/// `fields` as `(key, schema)`.
#[doc(hidden)]
#[must_use]
#[inline(never)]
pub fn variant_schema(variant: &str, fields: Vec<(&str, serde_json::Value)>) -> serde_json::Value {
    let mut properties = vec![(VARIANT_KEY, crate::schema::constant(variant), true)];
    properties.extend(fields.into_iter().map(|(key, schema)| (key, schema, false)));
    crate::schema::object(properties)
}

fn scalar<'v>(value: &'v Value<'_>) -> Option<&'v ScalarValue> {
    match value {
        Value::Scalar(scalar) => Some(scalar.as_ref()),
        Value::Object(_) => None,
    }
}

impl Settable for bool {}
impl Model for bool {
    const SCALAR: bool = true;
    crate::__own_table!(bool);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "boolean"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Bool(*self)
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::Bool(value) => Ok(*value),
            serde_json::Value::Null => Ok(false),
            _ => Err(json_shape("a boolean")),
        }
    }
    #[inline(never)]
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
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Boolean(*self))
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

macro_rules! integer_model {
    ($($ty:ty),*) => {$(
        impl Settable for $ty {}
        impl Model for $ty {
            const SCALAR: bool = true;
            crate::__own_table!($ty);
            fn value_schema() -> serde_json::Value {
                serde_json::json!({"type": "integer"})
            }
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
            fn stores_as(&self, before: &Self) -> bool {
                self == before
            }
        }
    )*};
}
integer_model!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl Settable for f64 {}
impl Model for f64 {
    const SCALAR: bool = true;
    crate::__own_table!(f64);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "number"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Number::from_f64(*self)
            .map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
    #[inline(never)]
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
    #[inline(never)]
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
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::F64(*self))
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self.to_bits() == before.to_bits()
    }
}

impl Settable for f32 {}
impl Model for f32 {
    const SCALAR: bool = true;
    crate::__own_table!(f32);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "number"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        f64::from(*self).to_json()
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an f32 field stores what it wrote, widened"
    )]
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        f64::from_json(value).map(|value| value as f32)
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an f32 field stores what it wrote, widened"
    )]
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        f64::load(doc, value, id).map(|value| value as f32)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(0.0)
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        f64::from(*self).store(doc, slot)
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self.to_bits() == before.to_bits()
    }
}

/// Collaborative text: stored as an Automerge `Text` object and updated by
/// splicing only the changed span, so concurrent typing in one field merges
/// character by character. A string scalar written by an older schema reads
/// too and is upgraded on the first write.
impl Settable for String {}
impl Model for String {
    crate::__own_table!(String);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "string"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.clone())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => Ok(text.clone()),
            serde_json::Value::Null => Ok(Self::new()),
            _ => Err(json_shape("text")),
        }
    }
    #[inline(never)]
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
    #[inline(never)]
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
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text = slot.object(doc, ObjType::Text)?;
        let current = doc.text(&text)?;
        if current != *self {
            splice_changed_span(doc, &text, &current, self)?;
        }
        Ok(())
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
    /// The one splice [`store`](Model::store) makes, found from `before`
    /// instead of reading the whole text back.
    #[inline(never)]
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

impl Settable for Atom {}
impl Model for Atom {
    const SCALAR: bool = true;
    crate::__own_table!(Atom);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "string"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.0.clone())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(Self)
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.0.as_str().into()))
    }
    fn collect_text(&self, out: &mut Vec<String>) {
        if !self.0.is_empty() {
            out.push(self.0.clone());
        }
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

impl Settable for uuid::Uuid {}
impl Model for uuid::Uuid {
    crate::__own_table!(uuid::Uuid);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "string", "format": "uuid"})
    }
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_string())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => {
                Self::parse_str(text).map_err(|error| ModelError::invalid(error.to_string()))
            }
            _ => Err(json_shape("a uuid")),
        }
    }
    #[inline(never)]
    fn load(_: &Doc, value: &Value<'_>, _: &ObjId) -> Result<Self, ModelError> {
        match scalar(value) {
            Some(ScalarValue::Str(text)) => {
                Self::parse_str(text).map_err(|error| ModelError::invalid(error.to_string()))
            }
            _ => Err(ModelError::shape("a uuid")),
        }
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, ScalarValue::Str(self.to_string().into()))
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

/// `None` is an absent key (or a null list element).
impl<T: Settable> Settable for Option<T> {}
impl<T: Model> Model for Option<T> {
    fn value_schema() -> serde_json::Value {
        crate::schema::nullable(T::value_schema())
    }
    const SHAPE: crate::Shape = T::SHAPE;
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    const NULLABLE: bool = true;
    // Run by one set of functions for every option, over `T`'s table.
    const TABLE: &'static crate::table::ModelTable = &crate::table::OptionOf::<T>::TABLE;
    leaf_paths!();
    crate::__table_functions!();
    fn label(&self) -> Option<String> {
        self.as_ref().and_then(Model::label)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(None)
    }
}

impl<T: Settable> Settable for Box<T> {}
impl<T: Model> Model for Box<T> {
    const SCALAR: bool = T::SCALAR;
    fn value_schema() -> serde_json::Value {
        T::value_schema()
    }
    const SHAPE: crate::Shape = T::SHAPE;
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    const NULLABLE: bool = T::NULLABLE;
    fn op_coalesces(
        path: &[Segment],
        previous: &serde_json::Value,
        next: &serde_json::Value,
    ) -> Option<bool> {
        if T::OPS {
            T::op_coalesces(path, previous, next)
        } else {
            None
        }
    }
    fn op_continuable(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
        if T::OPS {
            T::op_continuable(path, op)
        } else {
            None
        }
    }
    fn op_describe(path: &[Segment], op: &serde_json::Value) -> Option<crate::path::OpDescription> {
        if T::OPS {
            T::op_describe(path, op)
        } else {
            None
        }
    }
    fn op_lands(&self, path: &[Segment], op: &serde_json::Value) -> Option<crate::Place> {
        if T::OPS {
            (**self).op_lands(path, op)
        } else {
            None
        }
    }
    fn op_schemas(prefix: &str, out: &mut Vec<(String, serde_json::Value)>) {
        if T::OPS {
            T::op_schemas(prefix, out);
        }
    }
    type Paths<R> = T::Paths<R>;
    fn paths_at<R>(path: Path<R, Self>) -> T::Paths<R> {
        T::paths_at(path.cast())
    }
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        (**self).to_json()
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        T::from_json(value).map(Box::new)
    }
    #[inline(never)]
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
    #[inline(never)]
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

    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        T::load(doc, value, id).map(Box::new)
    }
    fn missing() -> Result<Self, ModelError> {
        T::missing().map(Box::new)
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        (**self).store(doc, slot)
    }
    #[inline(never)]
    fn stores_as(&self, before: &Self) -> bool {
        (**self).stores_as(before)
    }
    #[inline(never)]
    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        (**self).store_changed(before, doc, slot, dirty)
    }
    #[inline(never)]
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
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema()})
    }
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    // Run by one set of functions for every list, over `T`'s table.
    const TABLE: &'static crate::table::ModelTable = &crate::table::VecOf::<T>::TABLE;
    leaf_paths!();
    crate::__table_functions!();
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
}

/// A fixed number of values, stored as a list.
impl<T: Model, const N: usize> Settable for [T; N] {}
impl<T: Model, const N: usize> Model for [T; N] {
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "array", "items": T::value_schema(), "minItems": N, "maxItems": N})
    }
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.iter().map(Model::to_json).collect())
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let items = Vec::<T>::from_json(value)?;
        let count = items.len();
        items
            .try_into()
            .map_err(|_| ModelError::invalid(format!("expected {N} values, not {count}")))
    }
    #[inline(never)]
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
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let list = slot.object(doc, ObjType::List)?;
        let existing = doc.length(&list);
        for (index, item) in self.iter().enumerate() {
            let slot = if index < existing {
                Slot::Index(&list, index)
            } else {
                Slot::Insert(&list, index)
            };
            item.store(doc, slot).located(&index.to_string())?;
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
    const SHAPE: crate::Shape = crate::Shape::Map(&T::SHAPE);
    fn value_schema() -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    const CHECKS: bool = T::CHECKS;
    const OPS: bool = T::OPS;
    // Run by one set of functions for every map, over `T`'s table.
    const TABLE: &'static crate::table::ModelTable = &crate::table::MapOf::<T>::TABLE;
    leaf_paths!();
    crate::__table_functions!();
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }
}

/// An opaque value stored as one JSON string: written whole, last writer
/// wins. Only for a payload whose shape is another party's and opaque to the
/// app's model: an account type's connection settings, a shortcut step's
/// bindings (literal JSON values, other apps' parameter declarations with
/// their JSON Schemas). Data the app shapes itself is a model type — a
/// struct, an enum (a data enum is a map naming its variant), a list, a map —
/// so its parts merge and migrate as values; a field that was `Json<T>` moves
/// to one with `evolution::retype_json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Json<T>(pub T);

impl<T> Settable for Json<T> where
    T: serde::Serialize + serde::de::DeserializeOwned + Default + 'static
{
}
impl<T> Model for Json<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
{
    leaf_paths!();
    /// Any JSON value, `null` among them.
    const NULLABLE: bool = true;
    // Run by one set of functions for every value stored as JSON text.
    const TABLE: &'static crate::table::ModelTable =
        &crate::table::ModelTable::of_json(&crate::table::JsonOf::<T>::WRAPPED);
    crate::__table_functions!();
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
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
