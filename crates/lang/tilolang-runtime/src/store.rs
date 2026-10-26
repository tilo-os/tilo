//! The `Store` trait (implementation design 4.7): where resources live.
//!
//! The runtime knows field ids and merge kinds, never the representation. A store maps the
//! operations below onto its own documents. Two departures from the design's sketch, both so
//! the host can keep the store while guest code runs: a transaction is an owned token
//! ([`Txn`]) and the operations are methods of the store, and the store speaks in
//! mutations and field values, not documents.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::flow::Journal;
use crate::ids::{AppId, DocId, FieldId, ItemId, RunId, TypeId};
use crate::meta::FieldKind;
use crate::value::ResourceRef;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("store failure: {0}")]
    Backend(String),
}

/// The content of a register field. Composite runtime values travel as `Json`.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Json(Json),
}

/// A field of a record type: its id and how it merges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub id: FieldId,
    pub kind: FieldKind,
}

/// What a new document or item needs to start out with: every structural field exists
/// from the first change, so concurrent first writes merge instead of conflicting.
#[derive(Clone, Debug)]
pub struct NewRecord {
    pub ty: TypeId,
    pub app: AppId,
    pub fields: Vec<FieldSpec>,
}

/// An open transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Txn(pub u64);

/// One mutation, addressed to a resource.
#[derive(Clone, Debug)]
pub enum Mutation {
    SetRegister {
        field: FieldId,
        value: Scalar,
    },
    /// Add to a counter (integer units).
    AddCounter {
        field: FieldId,
        delta: i64,
    },
    /// Replace the text; the store diffs and splices.
    SetText {
        field: FieldId,
        text: String,
    },
    /// Insert a new item with a fresh id; `index` of `None` appends.
    CollInsert {
        field: FieldId,
        item: NewRecord,
        index: Option<usize>,
    },
    CollRemove {
        field: FieldId,
        item: ItemId,
    },
    CollMove {
        field: FieldId,
        item: ItemId,
        index: usize,
    },
    SetAdd {
        field: FieldId,
        key: String,
    },
    SetRemove {
        field: FieldId,
        key: String,
    },
    MapPut {
        field: FieldId,
        key: String,
        value: Scalar,
    },
    MapRemove {
        field: FieldId,
        key: String,
    },
    /// Queue an outside effect in the change, on the document of this resource.
    OutboxPush {
        capability: String,
        payload: Json,
        /// What the entry is known by: queued twice under one key it is one entry. `None` gives
        /// each push an entry of its own.
        key: Option<String>,
    },
}

/// What a mutation produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Done,
    Item(ItemId),
}

/// A field as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// A register nobody has written.
    Absent,
    Scalar(Scalar),
    Counter(i64),
    Text(String),
    /// Collection items in order.
    Items(Vec<ItemId>),
    /// Set elements, sorted.
    Keys(Vec<String>),
    /// Map entries, sorted by key.
    Entries(Vec<(String, Scalar)>),
}

/// What a change records about its cause (spec 3.2 and 4.3, point 10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub action: String,
    pub args: Json,
    pub invoker: String,
    /// The authorship chain, outermost first.
    pub chain: Vec<String>,
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
}

/// One field of one resource: the resource as [`ResourceRef::id_string`] writes it (its
/// document, then a step per nested item), and the field id, or [`ANY_FIELD`] for every field
/// of that resource. Views record what they read as these, and stores report changes as them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldKey {
    pub resource: String,
    pub field: FieldId,
}

/// The field of a [`FieldKey`] that stands for every field of its resource: the resource was
/// created, replaced or removed as a whole.
pub const ANY_FIELD: &str = "*";

impl FieldKey {
    pub fn new(at: &ResourceRef, field: impl Into<FieldId>) -> Self {
        Self {
            resource: at.id_string(),
            field: field.into(),
        }
    }

    /// The document the resource lives in.
    pub fn doc(&self) -> &str {
        doc_of(&self.resource)
    }
}

/// The document of a resource written as [`ResourceRef::id_string`].
pub fn doc_of(resource: &str) -> &str {
    resource.split_once('/').map_or(resource, |(doc, _)| doc)
}

/// The items of a collection, in order: what [`Store::listing`] hands out, shared, so a view
/// reads a collection's length or one item without copying its items.
///
/// Item ids are shared too: what a view read of a listing (the item at an index) is kept to
/// compare with the listing after a change, which costs a count, not a copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub items: Vec<Rc<str>>,
}

impl Listing {
    pub fn from_ids(ids: impl IntoIterator<Item = ItemId>) -> Self {
        Self {
            items: ids.into_iter().map(Rc::from).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Rc<str>> {
        self.items.get(index)
    }

    /// The item ids, as owned text.
    pub fn ids(&self) -> Vec<ItemId> {
        self.items.iter().map(ToString::to_string).collect()
    }
}

/// One step of a change to a collection's items, at an index of the listing as the steps
/// before it left it. A move is a removal and an insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Splice {
    Insert {
        at: usize,
        item: ItemId,
    },
    /// `item` is the one that was at `at`: a reader checks it before applying the step.
    Remove {
        at: usize,
        item: ItemId,
    },
}

/// How a collection's listing changed: the steps, in order, from the listing as the store held
/// it before the change to the listing after it.
///
/// A reader that keeps something per item (the rows of a list, the published children of a
/// resource) applies the steps instead of comparing every item. It checks them as it goes (a
/// removal names the item it removes, an insertion must not find its item already there) and
/// its length against the listing's at the end, and rebuilds from the listing when they do not
/// fit: what it holds was not the listing the steps start from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingChange {
    /// The collection: its resource and field.
    pub key: FieldKey,
    pub splices: Vec<Splice>,
}

/// Which fields of which document changed. An empty field list means "unknown: every field of
/// every resource of the document may have changed".
///
/// A collection whose items changed is named in `fields` like any field; when the store could
/// say how, `listings` holds the steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSummary {
    pub doc: DocId,
    pub fields: Vec<FieldKey>,
    pub listings: Vec<ListingChange>,
}

impl ChangeSummary {
    /// Every field of `doc` may have changed.
    pub fn whole(doc: impl Into<DocId>) -> Self {
        Self {
            doc: doc.into(),
            fields: Vec::new(),
            listings: Vec::new(),
        }
    }

    /// The steps that changed the collection `key`, when the store could say.
    pub fn listing(&self, key: &FieldKey) -> Option<&[Splice]> {
        self.listings
            .iter()
            .find(|change| change.key == *key)
            .map(|change| change.splices.as_slice())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubId(pub u64);

/// An outside effect waiting to be settled.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEntry {
    pub doc: DocId,
    pub id: String,
    pub capability: String,
    pub payload: Json,
}

pub trait Store {
    /// A fresh id, unique across replicas and restarts.
    fn fresh_id(&mut self, prefix: &str) -> String;

    /// Create a document and commit it as one change.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the document cannot be created.
    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError>;

    /// The type id stored at this resource.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document or item does not exist.
    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError>;

    /// Open a transaction. Documents join it when first mutated.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when none can be opened.
    fn begin(&mut self) -> Result<Txn, StoreError>;

    /// Apply one mutation inside `txn`.
    ///
    /// # Errors
    ///
    /// `Invalid` for a mutation the field cannot take; `NotFound` for a missing target.
    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError>;

    /// Commit one change per touched document, each carrying `note`.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when a document cannot take the change.
    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError>;

    /// Discard everything the transaction did.
    fn rollback(&mut self, txn: Txn);

    /// Read a field. Sees the transaction's own uncommitted changes.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing target; `Invalid` when the stored shape does not fit the kind.
    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError>;

    /// The items of the collection `field` of `at`, in order, shared: a view reads the length
    /// or one item of a large collection through this without copying the others. A store that
    /// keeps what it read hands out what it keeps.
    ///
    /// # Errors
    ///
    /// As [`Store::read`], and `Invalid` for a field that is not a collection.
    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        match self.read(at, field)? {
            FieldValue::Items(items) => Ok(Rc::new(Listing::from_ids(items))),
            other => Err(StoreError::Invalid(format!(
                "field {} is not a collection: {other:?}",
                field.id
            ))),
        }
    }

    /// The notes of a document's changes, oldest first.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing document.
    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError>;

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId;
    fn unsubscribe(&mut self, sub: SubId);

    /// Outside effects queued and not yet settled.
    fn outbox_pending(&self) -> Vec<OutboxEntry>;

    /// Mark an entry settled, as one change.
    ///
    /// # Errors
    ///
    /// `NotFound` when the entry is gone.
    fn outbox_settle(&mut self, entry: &OutboxEntry, note: &ChangeNote) -> Result<(), StoreError>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be saved.
    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be read.
    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError>;
}
