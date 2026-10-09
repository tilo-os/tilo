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
    pub fn child<U>(&self, segment: Segment) -> Path<R, U> {
        let mut segments = Vec::with_capacity(self.segments.len().saturating_add(1));
        segments.extend(self.segments.iter().cloned());
        segments.push(segment);
        Path::from_segments(segments)
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

    /// The edit `op` at this path.
    #[must_use]
    pub fn edit(&self, op: EditOp) -> Edit<R> {
        Edit {
            path: self.segments.clone(),
            op,
            marker: PhantomData,
        }
    }
}

impl<R, T> Clone for Path<R, T> {
    fn clone(&self) -> Self {
        Self::from_segments(self.segments.clone())
    }
}

impl<R, T> std::fmt::Debug for Path<R, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Path(")?;
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                f.write_str(" › ")?;
            }
            write!(f, "{segment}")?;
        }
        f.write_str(")")
    }
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
        Vec::<Segment>::deserialize(deserializer).map(Self::from_segments)
    }
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
/// text's marks and blocks. Its paths offer [`Path::perform`].
pub trait Operations: Model {
    /// One edit of the type, in its own terms.
    type Op: Serialize;
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

impl<R, T: Model> Path<R, crate::Outbox<T>> {
    /// Record how the owning device settled the entry `id`.
    #[must_use]
    pub fn settle(&self, id: &Id, outcome: crate::Settled) -> Edit<R> {
        self.edit(EditOp::Settle {
            id: id.clone(),
            outcome,
        })
    }
}

/// What an [`Edit`] does at its path. Text positions count characters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Settle an outbox entry.
    Settle {
        id: Id,
        outcome: crate::Settled,
    },
    /// An edit of a type with its own vocabulary ([`Operations`]: rich text
    /// marks and blocks), in that type's JSON form.
    Rich {
        rich: serde_json::Value,
    },
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

    /// Whether `next`, by the same actor straight after `self`, continues
    /// the same gesture: one undo step. Setting one path again (dragging a
    /// slider) and typing on at the end of the previous splice (a word)
    /// continue; anything else starts a new step.
    #[must_use]
    pub fn coalesces(&self, next: &Self) -> bool {
        if self.path != next.path {
            return false;
        }
        match (&self.op, &next.op) {
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
                // Typing on after the previous insertion, within one word.
                let typing = *delete == 0
                    && *next_delete == 0
                    && *next_at == at.saturating_add(typed)
                    && !next_insert.chars().any(char::is_whitespace);
                // Deleting backwards from where the previous deletion was.
                let erasing = insert.is_empty()
                    && next_insert.is_empty()
                    && next_at.saturating_add(*next_delete) == *at;
                typing || erasing
            }
            _ => false,
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

#[derive(Serialize, Deserialize)]
struct EditForm {
    path: Vec<Segment>,
    #[serde(flatten)]
    op: EditOp,
}

impl<R> Serialize for Edit<R> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        EditForm {
            path: self.path.clone(),
            op: self.op.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de, R> Deserialize<'de> for Edit<R> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        EditForm::deserialize(deserializer).map(|form| Self::new(form.path, form.op))
    }
}

impl<R> crate::JsonSchema for Edit<R> {
    fn json_schema() -> serde_json::Value {
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
}

/// The paths of a type whose values are edited only as a whole or through
/// their own operations: the path itself.
#[doc(hidden)]
pub fn leaf<R, T>(path: Path<R, T>) -> Path<R, T> {
    path
}
