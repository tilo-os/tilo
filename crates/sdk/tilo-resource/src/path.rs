//! Typed paths into a model and the field edits made at them.
//!
//! `#[derive(Model)]` gives every type a path struct: `Snippets::paths()`
//! has one field per model field, a [`Collection`](crate::Collection) path
//! offers [`item`](Path::item), a [`Grid`](crate::Grid) path
//! [`cell`](Path::cell), a map path [`key`](Path::key). A [`Path`] is plain
//! data — the stored field names and item ids from the record's root — so it
//! can be stored, compared and sent.
//!
//! Each path offers the edits its type supports. An edit is an [`Edit`]: a
//! path and an [`EditOp`]. It is one of the resource's actions, so it
//! replays, syncs, undoes and is named in history like a verb, and it
//! applies at its path without reading the rest of the record.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::collection::Id;
use crate::model::Model;

/// One step of a [`Path`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Segment {
    /// A field of a record, by the key it is stored under.
    Field(String),
    /// An item of a [`Collection`](crate::Collection) or an entry of an
    /// [`Outbox`](crate::Outbox), by its id.
    Item(Id),
    /// An entry of a map.
    Key(String),
    /// A cell of a [`Grid`](crate::Grid), by its row and column ids.
    Cell { row: Id, column: Id },
}

impl std::fmt::Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field(key) | Self::Key(key) => f.write_str(key),
            Self::Item(id) => write!(f, "{id}"),
            Self::Cell { row, column } => write!(f, "{row}:{column}"),
        }
    }
}

/// The location of a `T` inside the record `R`. See the module docs.
pub struct Path<R, T> {
    segments: Vec<Segment>,
    marker: PhantomData<fn() -> (R, T)>,
}

#[allow(
    clippy::mismatching_type_param_order,
    reason = "the root is the record seen as a value of itself"
)]
impl<R> Path<R, R> {
    /// The record itself.
    #[must_use]
    pub const fn root() -> Self {
        Self {
            segments: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<R: Model, T> Path<R, T> {
    /// The object the value at this path is stored in — a record's map, a
    /// list, a text — resolved through each container's own layout, for an
    /// editor that binds to the object itself (a rich-text field inside a
    /// collection item); `None` when absent or stored as a scalar.
    #[must_use]
    pub fn object(&self, doc: &crate::Doc) -> Option<automerge::ObjId> {
        object_at(R::SHAPE, &self.segments, doc)
    }
}

/// [`Path::object`] of the path `segments` into a record stored as `shape`.
fn object_at(
    shape: crate::Shape,
    segments: &[Segment],
    doc: &crate::Doc,
) -> Option<automerge::ObjId> {
    use crate::Shape;
    use automerge::ReadDoc;
    fn child(doc: &crate::Doc, obj: &automerge::ObjId, key: &str) -> Option<automerge::ObjId> {
        match doc.get(obj, key).ok()?? {
            (automerge::Value::Object(_), id) => Some(id),
            (automerge::Value::Scalar(_), _) => None,
        }
    }
    let mut obj = automerge::ROOT;
    let mut shape = shape;
    for segment in segments {
        match (shape, segment) {
            (Shape::Record(fields), Segment::Field(key)) => {
                shape = fields
                    .iter()
                    .find(|field| field.key == key)
                    .map_or(Shape::Value, |field| field.shape);
                obj = child(doc, &obj, key)?;
            }
            (Shape::Collection(item), Segment::Item(id)) => {
                let entry = child(doc, &obj, id.as_str())?;
                obj = child(doc, &entry, crate::collection::VALUE)?;
                shape = *item;
            }
            (Shape::Keyed(item), Segment::Item(id)) => {
                obj = child(doc, &obj, id.as_str())?;
                shape = *item;
            }
            (_, Segment::Field(key) | Segment::Key(key)) => {
                obj = child(doc, &obj, key)?;
                shape = Shape::Value;
            }
            (_, Segment::Item(id)) => {
                obj = child(doc, &obj, id.as_str())?;
                shape = Shape::Value;
            }
            (_, Segment::Cell { .. }) => return None,
        }
    }
    Some(obj)
}

impl<R, T> Path<R, T> {
    /// A path from stored segments. A path read back from storage or the
    /// wire is checked when an edit at it applies.
    #[must_use]
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self {
            segments,
            marker: PhantomData,
        }
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    #[must_use]
    pub fn into_segments(self) -> Vec<Segment> {
        self.segments
    }

    /// One step further down, to a value of type `U`.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn child<U>(&self, segment: Segment) -> Path<R, U> {
        Path::from_segments(extended(&self.segments, segment))
    }

    /// The same location seen as another type: the inner value of a `Box`,
    /// the present value of an `Option`.
    #[doc(hidden)]
    #[must_use]
    pub fn cast<U>(self) -> Path<R, U> {
        Path::from_segments(self.segments)
    }

    /// This path, rooted at an item, seen from the record containing it: a
    /// row's `favorite` as `Snippets › snippets › <id> › favorite`.
    #[must_use]
    pub fn under<P>(&self, prefix: &Path<P, R>) -> Path<P, T> {
        let mut segments = prefix.segments.clone();
        segments.extend(self.segments.iter().cloned());
        Path::from_segments(segments)
    }

    /// The value at this path in `root`, if it is there.
    pub fn get<'a>(&self, root: &'a R) -> Option<&'a T>
    where
        R: Model + 'static,
        T: 'static,
    {
        root.get_at(&self.segments)?.downcast_ref()
    }

    /// Whether `self` is `other` or lies inside it.
    pub fn starts_with<U>(&self, other: &Path<R, U>) -> bool {
        self.segments.starts_with(&other.segments)
    }

    /// The field edits that turn `before`, the value at this path, into
    /// `after`: what a surface that edited a local copy submits, field by
    /// field and item by item ([`Model::diff`]).
    #[must_use]
    pub fn edits(&self, before: &T, after: &T) -> Vec<Edit<R>>
    where
        T: Model,
    {
        let mut found: Vec<Edit<()>> = Vec::new();
        before.diff(after, &self.segments, &mut found);
        retyped_all(found)
    }

    /// The edit `op` at this path.
    #[must_use]
    #[inline]
    pub fn edit(&self, op: EditOp) -> Edit<R> {
        Edit {
            path: self.segments.clone(),
            op,
            marker: PhantomData,
        }
    }
}

/// `segments` and one step more.
fn extended(segments: &[Segment], segment: Segment) -> Vec<Segment> {
    let mut extended = Vec::with_capacity(segments.len().saturating_add(1));
    extended.extend_from_slice(segments);
    extended.push(segment);
    extended
}

impl<R, T> Clone for Path<R, T> {
    fn clone(&self) -> Self {
        Self::from_segments(self.segments.clone())
    }
}

impl<R, T> std::fmt::Debug for Path<R, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt_segments(&self.segments, f)
    }
}

fn fmt_segments(segments: &[Segment], f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("Path(")?;
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            f.write_str(" › ")?;
        }
        write!(f, "{segment}")?;
    }
    f.write_str(")")
}

impl<R, T> PartialEq for Path<R, T> {
    fn eq(&self, other: &Self) -> bool {
        self.segments == other.segments
    }
}

impl<R, T> Eq for Path<R, T> {}

impl<R, T> PartialOrd for Path<R, T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<R, T> Ord for Path<R, T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.segments.cmp(&other.segments)
    }
}

impl<R, T> Hash for Path<R, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.segments.hash(state);
    }
}

impl<R, T> Serialize for Path<R, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.segments.serialize(serializer)
    }
}

impl<'de, R, T> Deserialize<'de> for Path<R, T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match segments_from_json(value) {
            Ok(segments) => Ok(Self::from_segments(segments)),
            Err(error) => Err(serde::de::Error::custom(error)),
        }
    }
}

// Paths and edits are decoded in every crate naming a record type, and serde
// instantiates a generic decoder per calling crate. Going through JSON values
// keeps one decoder, compiled here.
#[inline(never)]
fn segments_from_json(value: serde_json::Value) -> Result<Vec<Segment>, serde_json::Error> {
    Vec::<Segment>::deserialize(value)
}

#[inline(never)]
fn edit_from_json(value: serde_json::Value) -> Result<(Vec<Segment>, EditOp), serde_json::Error> {
    use serde::de::Error as _;
    let serde_json::Value::Object(mut map) = value else {
        return Err(serde_json::Error::custom("an edit is a JSON object"));
    };
    let path = map
        .remove("path")
        .ok_or_else(|| serde_json::Error::missing_field("path"))?;
    let path = segments_from_json(path)?;
    let op = op_from_json(serde_json::Value::Object(map))?;
    Ok((path, op))
}

/// Values a path can [`set`](Path::set) whole: scalars, enums, text, short
/// lists, options of them and nested records. Containers whose merge is per
/// element (`Counter`, `Set`, `Collection`, `Grid`, `Outbox`) are edited per
/// element instead.
pub trait Settable: Model {}

impl<R, T: Settable> Path<R, T> {
    /// Replace the value.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "a value is set as written: `set(true)`, `set(Language::Rust)`"
    )]
    pub fn set(&self, value: T) -> Edit<R> {
        self.edit(EditOp::Set {
            value: value.to_json(),
        })
    }
}

/// A type with its own vocabulary of edits beside the generic ones: rich
/// text's marks and blocks. Its paths offer [`Path::perform`]; its `Model`
/// impl calls [`operations_model!`](crate::operations_model) so records
/// holding it route those edits' gestures, landing places and schema to it.
pub trait Operations: Model {
    /// One edit of the type, in its own terms.
    type Op: Serialize + serde::de::DeserializeOwned + crate::JsonSchema;

    /// Whether `next`, straight after `previous` by the same actor, is the
    /// same gesture (typing on in one word): one undo step.
    fn coalesces(previous: &Self::Op, next: &Self::Op) -> bool {
        let _ = (previous, next);
        false
    }

    /// Whether a gesture may go on after `op`.
    fn continuable(op: &Self::Op) -> bool {
        let _ = op;
        true
    }

    /// What `op` did, as a history entry records it: the kind of edit and
    /// its plain facts by name (`{mark: "strong", words: 3}`), which the
    /// reader's message receives.
    fn describe(op: &Self::Op) -> OpDescription {
        let _ = op;
        OpDescription::default()
    }

    /// Where the actor of `op` is after it applied (the caret after typed
    /// text), read from the value it applied to.
    fn lands(&self, op: &Self::Op) -> Option<crate::Place> {
        let _ = op;
        None
    }
}

/// [`Model::op_coalesces`] of an [`Operations`] type.
#[doc(hidden)]
pub fn op_coalesces<T: Operations>(
    path: &[Segment],
    previous: &serde_json::Value,
    next: &serde_json::Value,
) -> Option<bool> {
    if !path.is_empty() {
        return None;
    }
    let previous = serde_json::from_value(previous.clone()).ok()?;
    let next = serde_json::from_value(next.clone()).ok()?;
    Some(T::coalesces(&previous, &next))
}

/// What an edit in a type's own vocabulary did ([`Operations::describe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpDescription {
    pub op: tilo_actions::EditOp,
    pub args: std::collections::BTreeMap<String, tilo_actions::Scalar>,
}

impl Default for OpDescription {
    fn default() -> Self {
        Self {
            op: tilo_actions::EditOp::Mark,
            args: std::collections::BTreeMap::new(),
        }
    }
}

/// [`Model::op_describe`] of an [`Operations`] type.
#[doc(hidden)]
pub fn op_describe<T: Operations>(
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<OpDescription> {
    if !path.is_empty() {
        return None;
    }
    Some(T::describe(&serde_json::from_value(op.clone()).ok()?))
}

/// [`Model::op_continuable`] of an [`Operations`] type.
#[doc(hidden)]
pub fn op_continuable<T: Operations>(path: &[Segment], op: &serde_json::Value) -> Option<bool> {
    if !path.is_empty() {
        return None;
    }
    Some(T::continuable(&serde_json::from_value(op.clone()).ok()?))
}

/// [`Model::op_lands`] of an [`Operations`] type.
#[doc(hidden)]
pub fn op_lands<T: Operations>(
    value: &T,
    path: &[Segment],
    op: &serde_json::Value,
) -> Option<crate::Place> {
    if !path.is_empty() {
        return None;
    }
    value.lands(&serde_json::from_value(op.clone()).ok()?)
}

/// Implements the `Model` items that route a record's edits of an
/// [`Operations`] type to it; call it inside the type's `impl Model`.
#[macro_export]
macro_rules! operations_model {
    () => {
        const OPS: bool = true;
        fn op_coalesces(
            path: &[$crate::Segment],
            previous: &$crate::__private::serde_json::Value,
            next: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<bool> {
            $crate::path::op_coalesces::<Self>(path, previous, next)
        }
        fn op_continuable(
            path: &[$crate::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<bool> {
            $crate::path::op_continuable::<Self>(path, op)
        }
        fn op_describe(
            path: &[$crate::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<$crate::path::OpDescription> {
            $crate::path::op_describe::<Self>(path, op)
        }
        fn op_lands(
            &self,
            path: &[$crate::Segment],
            op: &$crate::__private::serde_json::Value,
        ) -> ::core::option::Option<$crate::Place> {
            $crate::path::op_lands::<Self>(self, path, op)
        }
        fn op_schemas(
            prefix: &str,
            out: &mut ::std::vec::Vec<(
                ::std::string::String,
                $crate::__private::serde_json::Value,
            )>,
        ) {
            out.push((
                ::std::string::String::from(prefix.trim_end_matches('.')),
                <<Self as $crate::Operations>::Op as $crate::JsonSchema>::json_schema(),
            ));
        }
    };
}

impl<R, T: Operations> Path<R, T> {
    /// The edit `op` of the value at this path, in the type's own vocabulary.
    ///
    /// # Errors
    /// Returns the serialization error of `op`.
    pub fn perform(&self, op: &T::Op) -> Result<Edit<R>, serde_json::Error> {
        Ok(self.edit(EditOp::Rich {
            rich: serde_json::to_value(op)?,
        }))
    }
}

impl<R, T: Model> Path<R, Option<T>> {
    /// The value when present; edits at it refuse while it is absent.
    #[must_use]
    pub fn some(&self) -> T::Paths<R> {
        T::paths_at(self.clone().cast())
    }
}

impl<R> Path<R, String> {
    /// Replace `delete` characters at `at` with `insert`.
    #[must_use]
    pub fn splice(&self, at: usize, delete: usize, insert: impl Into<String>) -> Edit<R> {
        self.edit(EditOp::Splice {
            at,
            delete,
            insert: insert.into(),
        })
    }

    /// Replace the whole text, written as the smallest splice that turns the
    /// stored text into it.
    #[must_use]
    pub fn replace(&self, text: impl Into<String>) -> Edit<R> {
        self.edit(EditOp::Replace { text: text.into() })
    }
}

impl<R> Path<R, crate::Counter> {
    /// Add `by` (negative to subtract). Concurrent additions add up.
    #[must_use]
    pub fn add(&self, by: i64) -> Edit<R> {
        self.edit(EditOp::Add { by })
    }
}

impl<R, T: Model> Path<R, crate::Set<T>> {
    /// Add an element.
    #[must_use]
    pub fn insert(&self, value: &T) -> Edit<R> {
        self.edit(EditOp::Insert {
            value: value.to_json(),
        })
    }

    /// Remove an element.
    #[must_use]
    pub fn remove(&self, value: &T) -> Edit<R> {
        self.edit(EditOp::Remove {
            value: value.to_json(),
        })
    }
}

impl<R, T: Model> Path<R, crate::Collection<T>> {
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

    /// Insert `value` at `at` (clamped to the end) under a fresh id minted
    /// when the edit applies.
    #[must_use]
    pub fn insert(&self, at: usize, value: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: None,
            at,
            value: value.to_json(),
        })
    }

    /// Insert `value` at `at` (clamped to the end) under `id`.
    #[must_use]
    pub fn insert_as(&self, id: &Id, at: usize, value: &T) -> Edit<R> {
        self.edit(EditOp::InsertItem {
            id: Some(id.clone()),
            at,
            value: value.to_json(),
        })
    }

    /// Remove the item `id`.
    #[must_use]
    pub fn remove(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveItem { id: id.clone() })
    }

    /// Move the item `id` to `at` (clamped to the end).
    #[must_use]
    pub fn move_to(&self, id: &Id, at: usize) -> Edit<R> {
        self.edit(EditOp::MoveTo { id: id.clone(), at })
    }
}

impl<R, T: Model> Path<R, std::collections::BTreeMap<String, T>> {
    /// The entry `key`, its own paths rooted here.
    #[must_use]
    pub fn key(&self, key: impl Into<String>) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Key(key.into())))
    }

    /// Remove the entry `key`.
    #[must_use]
    pub fn remove(&self, key: impl Into<String>) -> Edit<R> {
        self.edit(EditOp::RemoveKey { key: key.into() })
    }
}

impl<R, T: Model> Path<R, crate::Grid<T>> {
    /// The cell at `row`, `column`, its own paths rooted here.
    #[must_use]
    pub fn cell(&self, row: &Id, column: &Id) -> T::Paths<R> {
        T::paths_at(self.child(Segment::Cell {
            row: row.clone(),
            column: column.clone(),
        }))
    }

    /// Set or clear one cell.
    #[must_use]
    pub fn set_cell(&self, row: &Id, column: &Id, value: Option<&T>) -> Edit<R> {
        self.edit(EditOp::SetCell {
            row: row.clone(),
            column: column.clone(),
            value: value.map(Model::to_json),
        })
    }

    /// Insert a row at `at` (clamped) under a fresh id minted when the edit
    /// applies.
    #[must_use]
    pub fn insert_row(&self, at: usize) -> Edit<R> {
        self.edit(EditOp::InsertRow { id: None, at })
    }

    /// Insert a column at `at` (clamped) under a fresh id.
    #[must_use]
    pub fn insert_column(&self, at: usize) -> Edit<R> {
        self.edit(EditOp::InsertColumn { id: None, at })
    }

    /// Remove a row and its cells.
    #[must_use]
    pub fn remove_row(&self, row: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveRow { id: row.clone() })
    }

    /// Remove a column and its cells.
    #[must_use]
    pub fn remove_column(&self, column: &Id) -> Edit<R> {
        self.edit(EditOp::RemoveColumn { id: column.clone() })
    }
}

impl<R, T: Model, O: Serialize + serde::de::DeserializeOwned + 'static>
    Path<R, crate::Outbox<T, O>>
{
    /// Record how the owning device settled the entry `id`: content from
    /// outside, nobody's undo step.
    #[must_use]
    pub fn settle(&self, id: &Id, outcome: &crate::Settled<O>) -> Edit<R> {
        self.edit(EditOp::Settle {
            id: id.clone(),
            outcome: Model::to_json(outcome),
        })
    }

    /// Forget the settled entry `id`: housekeeping, nobody's undo step.
    #[must_use]
    pub fn forget(&self, id: &Id) -> Edit<R> {
        self.edit(EditOp::Forget { id: id.clone() })
    }
}

/// What an [`Edit`] does at its path. Text positions count characters.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum EditOp {
    /// Replace the value with one in its JSON form ([`Model::to_json`]).
    Set {
        value: serde_json::Value,
    },
    /// Replace `delete` characters at `at` with `insert`.
    Splice {
        at: usize,
        delete: usize,
        insert: String,
    },
    /// Replace the whole text by the smallest splice.
    Replace {
        text: String,
    },
    /// Add to a [`Counter`](crate::Counter).
    Add {
        by: i64,
    },
    /// Add an element to a [`Set`](crate::Set).
    Insert {
        value: serde_json::Value,
    },
    /// Remove an element from a [`Set`](crate::Set).
    Remove {
        value: serde_json::Value,
    },
    /// Insert an item into a collection; `id: None` mints one.
    InsertItem {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
        at: usize,
        value: serde_json::Value,
    },
    /// Remove an item from a collection.
    RemoveItem {
        id: Id,
    },
    /// Move an item within a collection.
    MoveTo {
        id: Id,
        at: usize,
    },
    /// Remove an entry of a map.
    RemoveKey {
        key: String,
    },
    /// Set (`Some`) or clear (`None`) a grid cell.
    SetCell {
        row: Id,
        column: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    InsertRow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
        at: usize,
    },
    InsertColumn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Id>,
        at: usize,
    },
    RemoveRow {
        id: Id,
    },
    RemoveColumn {
        id: Id,
    },
    /// Settle an outbox entry, with its [`Settled`](crate::Settled) in JSON
    /// form.
    Settle {
        id: Id,
        outcome: serde_json::Value,
    },
    /// Forget a settled outbox entry.
    Forget {
        id: Id,
    },
    /// An edit of a type with its own vocabulary ([`Operations`]: rich text
    /// marks and blocks), in that type's JSON form.
    Rich {
        rich: serde_json::Value,
    },
}

// Read through one JSON value: the tag taken out by hand and the rest read
// as an externally tagged twin, so no deserializer buffers the edit's content
// to find its tag, and the reading is compiled once.
impl<'de> Deserialize<'de> for EditOp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match op_from_json(value) {
            Ok(op) => Ok(op),
            Err(error) => Err(serde::de::Error::custom(error)),
        }
    }
}

/// [`EditOp`] as its JSON form tags it, the tag outside.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum EditOpWire {
    Set {
        value: serde_json::Value,
    },
    Splice {
        at: usize,
        delete: usize,
        insert: String,
    },
    Replace {
        text: String,
    },
    Add {
        by: i64,
    },
    Insert {
        value: serde_json::Value,
    },
    Remove {
        value: serde_json::Value,
    },
    InsertItem {
        #[serde(default)]
        id: Option<Id>,
        at: usize,
        value: serde_json::Value,
    },
    RemoveItem {
        id: Id,
    },
    MoveTo {
        id: Id,
        at: usize,
    },
    RemoveKey {
        key: String,
    },
    SetCell {
        row: Id,
        column: Id,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
    InsertRow {
        #[serde(default)]
        id: Option<Id>,
        at: usize,
    },
    InsertColumn {
        #[serde(default)]
        id: Option<Id>,
        at: usize,
    },
    RemoveRow {
        id: Id,
    },
    RemoveColumn {
        id: Id,
    },
    Settle {
        id: Id,
        outcome: serde_json::Value,
    },
    Forget {
        id: Id,
    },
    Rich {
        rich: serde_json::Value,
    },
}

impl From<EditOpWire> for EditOp {
    fn from(wire: EditOpWire) -> Self {
        match wire {
            EditOpWire::Set { value } => Self::Set { value },
            EditOpWire::Splice { at, delete, insert } => Self::Splice { at, delete, insert },
            EditOpWire::Replace { text } => Self::Replace { text },
            EditOpWire::Add { by } => Self::Add { by },
            EditOpWire::Insert { value } => Self::Insert { value },
            EditOpWire::Remove { value } => Self::Remove { value },
            EditOpWire::InsertItem { id, at, value } => Self::InsertItem { id, at, value },
            EditOpWire::RemoveItem { id } => Self::RemoveItem { id },
            EditOpWire::MoveTo { id, at } => Self::MoveTo { id, at },
            EditOpWire::RemoveKey { key } => Self::RemoveKey { key },
            EditOpWire::SetCell { row, column, value } => Self::SetCell { row, column, value },
            EditOpWire::InsertRow { id, at } => Self::InsertRow { id, at },
            EditOpWire::InsertColumn { id, at } => Self::InsertColumn { id, at },
            EditOpWire::RemoveRow { id } => Self::RemoveRow { id },
            EditOpWire::RemoveColumn { id } => Self::RemoveColumn { id },
            EditOpWire::Settle { id, outcome } => Self::Settle { id, outcome },
            EditOpWire::Forget { id } => Self::Forget { id },
            EditOpWire::Rich { rich } => Self::Rich { rich },
        }
    }
}

/// The operation in `value`, an edit's JSON form without its path.
#[inline(never)]
fn op_from_json(value: serde_json::Value) -> Result<EditOp, serde_json::Error> {
    use serde::de::Error as _;
    let serde_json::Value::Object(mut fields) = value else {
        return Err(serde_json::Error::custom(
            "an edit operation is a JSON object",
        ));
    };
    let Some(serde_json::Value::String(op)) = fields.remove("op") else {
        return Err(serde_json::Error::missing_field("op"));
    };
    let mut tagged = serde_json::Map::new();
    tagged.insert(op, serde_json::Value::Object(fields));
    EditOpWire::deserialize(serde_json::Value::Object(tagged)).map(EditOp::from)
}

impl EditOp {
    /// The operation's name, as its JSON form tags it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::Splice { .. } => "splice",
            Self::Replace { .. } => "replace",
            Self::Add { .. } => "add",
            Self::Insert { .. } => "insert",
            Self::Remove { .. } => "remove",
            Self::InsertItem { .. } => "insert-item",
            Self::RemoveItem { .. } => "remove-item",
            Self::MoveTo { .. } => "move-to",
            Self::RemoveKey { .. } => "remove-key",
            Self::SetCell { .. } => "set-cell",
            Self::InsertRow { .. } => "insert-row",
            Self::InsertColumn { .. } => "insert-column",
            Self::RemoveRow { .. } => "remove-row",
            Self::RemoveColumn { .. } => "remove-column",
            Self::Settle { .. } => "settle",
            Self::Forget { .. } => "forget",
            Self::Rich { .. } => "rich",
        }
    }

    /// Whether this operation removes something a person may want back
    /// (an item, a row, a map entry).
    #[must_use]
    pub const fn removes(&self) -> bool {
        matches!(
            self,
            Self::RemoveItem { .. }
                | Self::RemoveKey { .. }
                | Self::RemoveRow { .. }
                | Self::RemoveColumn { .. }
        )
    }
}

/// A field edit of the record `R`: a path and an operation. See the module
/// docs.
pub struct Edit<R> {
    path: Vec<Segment>,
    op: EditOp,
    marker: PhantomData<fn() -> R>,
}

impl<R> Edit<R> {
    /// An edit from its parts, as read back from the wire.
    #[must_use]
    pub fn new(path: Vec<Segment>, op: EditOp) -> Self {
        Self {
            path,
            op,
            marker: PhantomData,
        }
    }

    pub fn path(&self) -> &[Segment] {
        &self.path
    }

    pub fn op(&self) -> &EditOp {
        &self.op
    }

    /// The same edit, of the record `S`: the path and operation it carries
    /// do not depend on the record type.
    #[must_use]
    pub fn retyped<S>(self) -> Edit<S> {
        Edit::new(self.path, self.op)
    }

    /// The edit in `value`, its JSON form.
    ///
    /// # Errors
    /// Why `value` is no field edit.
    pub fn from_json(value: serde_json::Value) -> Result<Self, String> {
        match edit_from_json(value) {
            Ok((path, op)) => Ok(Self::new(path, op)),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Whether `next`, by the same actor straight after `self`, continues
    /// the same gesture: one undo step. Setting one path again (dragging a
    /// slider) and typing on at the end of the previous splice (a word)
    /// continue; an edit in a type's own vocabulary continues as that type
    /// says ([`Operations::coalesces`]); anything else starts a new step.
    #[must_use]
    pub fn coalesces(&self, next: &Self) -> bool
    where
        R: Model,
    {
        if self.path != next.path {
            return false;
        }
        match (&self.op, &next.op) {
            (EditOp::Rich { rich: previous }, EditOp::Rich { rich: next }) => {
                R::op_coalesces(&self.path, previous, next).unwrap_or(false)
            }
            (EditOp::Set { .. }, EditOp::Set { .. })
            | (EditOp::Replace { .. }, EditOp::Replace { .. })
            | (EditOp::Add { .. }, EditOp::Add { .. }) => true,
            (
                EditOp::Splice {
                    at, delete, insert, ..
                },
                EditOp::Splice {
                    at: next_at,
                    delete: next_delete,
                    insert: next_insert,
                },
            ) => {
                let typed = insert.chars().count();
                // Typing on after the previous insertion, within one word:
                // a space ends the word it follows, so undo takes back
                // `world` and leaves `hello `.
                let typing = *delete == 0
                    && *next_delete == 0
                    && !next_insert.is_empty()
                    && *next_at == at.saturating_add(typed)
                    && !insert.ends_with(char::is_whitespace);
                // Deleting backwards from where the previous deletion was.
                let erasing = insert.is_empty()
                    && next_insert.is_empty()
                    && next_at.saturating_add(*next_delete) == *at;
                typing || erasing
            }
            _ => false,
        }
    }

    /// How much harm this edit can do: removing an item of a collection is
    /// destructive, like a delete verb, unless the collection is
    /// `#[model(remove = "quiet")]`; every other edit is low.
    #[must_use]
    pub fn risk(&self) -> tilo_actions::RiskLevel
    where
        R: Model,
    {
        use tilo_action_policy::edits::{EditStep, field_edit_asks};
        let steps: Vec<EditStep<'_>> = self
            .path
            .iter()
            .map(|segment| match segment {
                Segment::Field(key) => EditStep::Field(key),
                Segment::Item(_) | Segment::Key(_) | Segment::Cell { .. } => EditStep::Item,
            })
            .collect();
        let quiet = R::SHAPE.quiet_removals();
        let quiet: Vec<&str> = quiet.iter().map(String::as_str).collect();
        if field_edit_asks(self.op.name(), &steps, &quiet) {
            tilo_actions::RiskLevel::Destructive
        } else {
            tilo_actions::RiskLevel::Low
        }
    }

    /// The node of the published tree this edit acts on, for deciding
    /// grants: `room › app › kind › record [› item] › field… › field-edit`.
    /// The collection field holding an item is not a node of its own.
    #[must_use]
    pub fn tree_path(&self, room: &str, app: &str, record: &str) -> tilo_action_policy::TreePath
    where
        R: crate::Resource,
    {
        use tilo_action_policy::Level;
        let mut tree = tilo_action_policy::TreePath::room(room)
            .app(app)
            .kind(R::KIND)
            .resource(record);
        let mut item = false;
        let mut segments = self.path.iter().peekable();
        while let Some(segment) = segments.next() {
            tree = match segment {
                Segment::Field(_) if !item && matches!(segments.peek(), Some(Segment::Item(_))) => {
                    tree
                }
                Segment::Item(id) if !item => {
                    item = true;
                    tree.item(id.as_str())
                }
                Segment::Field(key) | Segment::Key(key) => tree.push(Level::Field, key.as_str()),
                Segment::Item(id) => tree.push(Level::Field, id.as_str()),
                Segment::Cell { row, column } => tree
                    .push(Level::Field, row.as_str())
                    .push(Level::Field, column.as_str()),
            };
        }
        tree.action(crate::FIELD_EDIT)
    }

    /// Apply to a document held apart from any resource (a form's local
    /// value): no action, no history, ids minted from the document alone.
    ///
    /// # Errors
    /// As [`Self::apply`].
    pub fn apply_local(&self, doc: &mut crate::Doc) -> Result<(), crate::ModelError>
    where
        R: crate::Fields,
    {
        let payload = serde_json::to_vec(self)
            .map_err(|error| crate::ModelError::invalid(error.to_string()))?;
        let mut cx = crate::ActionCx::for_write(doc, &payload);
        self.apply(doc, &mut cx)
    }

    /// Apply to a value held as JSON, field by field: a form's local
    /// parameter struct. Only `set`, `replace` and `splice` apply.
    ///
    /// # Errors
    /// Returns why the path or operation does not fit.
    pub fn apply_json(&self, value: &mut serde_json::Value) -> Result<(), crate::ModelError> {
        let mut target = value;
        for segment in &self.path {
            let (Segment::Field(key) | Segment::Key(key)) = segment else {
                return Err(crate::ModelError::invalid("a form holds fields only"));
            };
            if !target.is_object() {
                *target = serde_json::Value::Object(serde_json::Map::new());
            }
            let serde_json::Value::Object(fields) = target else {
                return Err(crate::ModelError::shape("an object"));
            };
            target = fields.entry(key.clone()).or_insert(serde_json::Value::Null);
        }
        match &self.op {
            EditOp::Set { value } => *target = value.clone(),
            EditOp::Replace { text } => *target = serde_json::Value::String(text.clone()),
            EditOp::Splice { at, delete, insert } => {
                let current: Vec<char> = target.as_str().unwrap_or_default().chars().collect();
                if *at > current.len() || *delete > current.len().saturating_sub(*at) {
                    return Err(crate::ModelError::invalid(
                        "the splice reaches past the text",
                    ));
                }
                let end = at.saturating_add(*delete);
                let text: String = current
                    .get(..*at)
                    .unwrap_or_default()
                    .iter()
                    .chain(insert.chars().collect::<Vec<_>>().iter())
                    .chain(current.get(end..).unwrap_or_default())
                    .collect();
                *target = serde_json::Value::String(text);
            }
            other => return Err(crate::ModelError::unsupported(other)),
        }
        Ok(())
    }

    /// Apply at its path in `doc`, then run the invariants of every value
    /// the edit touched.
    ///
    /// # Errors
    /// Returns why the path or operation does not fit the model, or the
    /// refusal of a [`check`](crate::Model::check).
    pub fn apply(
        &self,
        doc: &mut crate::Doc,
        cx: &mut crate::ActionCx,
    ) -> Result<(), crate::ModelError>
    where
        R: crate::Fields,
    {
        R::edit_fields(doc, &automerge::ROOT, &self.path, &self.op, cx)
    }
}

impl<R> Clone for Edit<R> {
    fn clone(&self) -> Self {
        Self::new(self.path.clone(), self.op.clone())
    }
}

impl<R> std::fmt::Debug for Edit<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Edit")
            .field("path", &Path::<R, ()>::from_segments(self.path.clone()))
            .field("op", &self.op)
            .finish()
    }
}

impl<R> PartialEq for Edit<R> {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.op == other.op
    }
}

impl<R> Serialize for Edit<R> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        edit_to_json(&self.path, &self.op).serialize(serializer)
    }
}

#[inline(never)]
fn edit_to_json(path: &[Segment], op: &EditOp) -> serde_json::Value {
    // Both are plain data: their JSON never fails to build.
    let mut form = serde_json::to_value(op).unwrap_or_default();
    if let serde_json::Value::Object(map) = &mut form {
        map.insert(
            "path".into(),
            serde_json::to_value(path).unwrap_or_default(),
        );
    }
    form
}

impl<'de, R> Deserialize<'de> for Edit<R> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match edit_from_json(value) {
            Ok((path, op)) => Ok(Self::new(path, op)),
            Err(error) => Err(serde::de::Error::custom(error)),
        }
    }
}

impl<R> crate::JsonSchema for Edit<R> {
    fn json_schema() -> serde_json::Value {
        edit_schema()
    }
}

#[inline(never)]
fn edit_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "A field edit: the path from the record's root (fields by name, items by id) and an operation the field's type supports",
        "properties": {
            "path": {
                "type": "array",
                "items": {"type": "object"},
            },
            "op": {"type": "string", "enum": [
                "set", "splice", "replace", "add", "insert", "remove", "insert-item",
                "remove-item", "move-to", "remove-key", "set-cell", "insert-row",
                "insert-column", "remove-row", "remove-column", "settle", "rich",
            ]},
        },
        "required": ["path", "op"],
    })
}

/// `edits`, of the record `R`.
#[must_use]
pub fn retyped_all<R>(edits: Vec<Edit<()>>) -> Vec<Edit<R>> {
    let mut out = Vec::with_capacity(edits.len());
    for edit in edits {
        out.push(edit.retyped());
    }
    out
}

/// The paths of a type whose values are edited only as a whole or through
/// their own operations: the path itself.
#[doc(hidden)]
pub fn leaf<R, T>(path: Path<R, T>) -> Path<R, T> {
    path
}
