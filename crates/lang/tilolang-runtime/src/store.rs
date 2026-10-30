//! The `Store` trait: where resources live.
//!
//! The runtime knows field ids and merge kinds, never the representation. A store maps the
//! operations below onto its own documents. So that the host can keep the store while guest
//! code runs, a transaction is an owned token ([`Txn`]), the operations are methods of the
//! store, and the store speaks in mutations and field values, not documents. A store also keeps
//! flow journals, so a suspended flow survives a restart.

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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldSpec {
    pub id: FieldId,
    pub kind: FieldKind,
}

/// A predicate over the items of a collection, which [`Store::listing_where`] answers (see
/// [`tilolang::pred`]).
pub type Pred = tilolang::pred::Pred<FieldRead>;

pub use tilolang::pred::Lit;

/// A field a [`Pred`] reads: a register, a counter or a text of the item, and what it holds
/// when nobody wrote it (the declared default).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldRead {
    pub spec: FieldSpec,
    pub absent: Lit,
}

impl FieldRead {
    /// What the field holds, `value` being what the store holds of it.
    ///
    /// # Errors
    ///
    /// `Invalid` for a stored value a predicate does not compare.
    pub fn lit(&self, value: FieldValue) -> Result<Lit, StoreError> {
        lit_of(&self.spec.id, value, &self.absent)
    }
}

/// What the field `id` holds, `value` being what the store holds of it and `absent` what an
/// unwritten one reads as, as a predicate compares it: a set as its members, a collection as
/// how many items it has.
///
/// # Errors
///
/// `Invalid` for a stored value a predicate does not compare.
pub fn lit_of(id: &str, value: FieldValue, absent: &Lit) -> Result<Lit, StoreError> {
    let invalid = |what: &dyn std::fmt::Debug| {
        StoreError::Invalid(format!(
            "field {id} holds {what:?}, which a filter does not compare"
        ))
    };
    match value {
        FieldValue::Absent => Ok(absent.clone()),
        FieldValue::Scalar(Scalar::Bool(b)) => Ok(Lit::Bool(b)),
        FieldValue::Scalar(Scalar::Int(i)) | FieldValue::Counter(i) => Ok(Lit::Int(i)),
        FieldValue::Scalar(Scalar::Float(f)) => Ok(Lit::Float(tilolang::pred::Float(f))),
        FieldValue::Scalar(Scalar::Text(t)) | FieldValue::Text(t) => Ok(Lit::Text(t)),
        // What rich text says depends on who reads it (its references): a filter compares none.
        FieldValue::Rich(_) => Err(invalid(&"rich text")),
        FieldValue::Scalar(Scalar::Json(j)) => Lit::of_json(&j).ok_or_else(|| invalid(&j)),
        FieldValue::Keys(keys) => Ok(Lit::Set(keys.into_iter().map(Lit::Text).collect())),
        FieldValue::Items(items) => Ok(Lit::Int(i64::try_from(items.len()).unwrap_or(i64::MAX))),
        FieldValue::Entries(entries) => Err(invalid(&entries)),
    }
}

/// How two values of the field a query orders by compare: an absent value first, text
/// without regard to case.
#[must_use]
pub fn order_cmp(a: &Lit, b: &Lit) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Lit::Absent, Lit::Absent) => Ordering::Equal,
        (Lit::Absent, _) => Ordering::Less,
        (_, Lit::Absent) => Ordering::Greater,
        (Lit::Text(x), Lit::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        _ => {
            if a.compare(tilolang::pred::CmpOp::Lt, b) == Some(true) {
                Ordering::Less
            } else if a.compare(tilolang::pred::CmpOp::Gt, b) == Some(true) {
                Ordering::Greater
            } else {
                a.cmp(b)
            }
        }
    }
}

impl FieldRead {
    /// The field read as a filter reads it, with what an unwritten one of type `ty` (declared
    /// default `default`) holds: `None` for a field no filter reads (a map).
    #[must_use]
    pub fn of(
        id: &str,
        kind: FieldKind,
        ty: &crate::meta::Ty,
        default: Option<&Json>,
    ) -> Option<Self> {
        use crate::meta::Ty;
        let absent = match kind {
            FieldKind::Counter | FieldKind::Collection => Lit::Int(0),
            FieldKind::Text | FieldKind::RichText => Lit::Text(String::new()),
            FieldKind::Set => Lit::Set(Vec::new()),
            FieldKind::Map => return None,
            FieldKind::Register => match default {
                Some(json) => Lit::of_json(json)?,
                None => match ty {
                    Ty::Bool => Lit::Bool(false),
                    Ty::Int => Lit::Int(0),
                    Ty::Float => Lit::Float(tilolang::pred::Float(0.0)),
                    Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => Lit::Text(String::new()),
                    _ => Lit::Absent,
                },
            },
        };
        Some(Self {
            spec: FieldSpec {
                id: id.to_owned(),
                kind,
            },
            absent,
        })
    }
}

impl From<String> for StoreError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
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
    /// Make the rich text read as `value`; the store splices and marks the paragraphs that
    /// differ ([`crate::rich::store_under`]).
    SetRich {
        field: FieldId,
        value: crate::rich::RichValue,
    },
    /// Insert a new item with a fresh id; `index` of `None` appends.
    CollInsert {
        field: FieldId,
        item: NewRecord,
        index: Option<usize>,
    },
    /// Remove the item, with every item in it ([`crate::place`]).
    CollRemove {
        field: FieldId,
        item: ItemId,
    },
    /// Put the item at the place `index` of this collection, among its other items: from where
    /// it is in this collection, or from another collection of the same document. It keeps its
    /// id ([`crate::place`]).
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
    /// A rich text's paragraphs.
    Rich(crate::rich::RichValue),
    /// Collection items in order.
    Items(Vec<ItemId>),
    /// Set elements, sorted.
    Keys(Vec<String>),
    /// Map entries, sorted by key.
    Entries(Vec<(String, Scalar)>),
}

/// What a change records about its cause: history shows it as "you, via Alex's view".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub action: String,
    pub args: Json,
    pub invoker: String,
    /// The authorship chain, outermost first.
    pub chain: Vec<String>,
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// When it was made: the invocation's time, recorded like an argument (convergence.md,
    /// "Actions"); a scenario's `as who at time` sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The resources it changed in the document it is a change of, as
    /// [`ResourceRef::id_string`] writes them: what reflection (`created(r)`, `history(r)`)
    /// reads a resource's versions from. The store fills it in when it commits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched: Vec<String>,
    /// The task run it was made in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<TaskRun>,
}

/// The action id of the change that made a resource, when the host made it.
pub const CREATE_ACTION: &str = "$create";

/// One version of a resource, as history keeps it: who made it, when, by which action, in
/// which task run, and what it did to the resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Version {
    pub who: String,
    /// ISO-8601 UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub at: String,
    /// The action's id (`$create` for a resource the host made).
    pub action: String,
    /// The task run that made it; `None` for a change made outside a run.
    pub task: Option<TaskRun>,
    /// What it did to the resource.
    pub kind: VersionKind,
    /// The fields it changed, by id; empty for a version that made or removed the resource.
    pub fields: Vec<FieldId>,
    /// The resource it is a version of, as [`ResourceRef::id_string`] writes it.
    pub resource: String,
    /// The change that made it, as its store names changes; empty while provisional. What a
    /// seen mark names ([`crate::seen`]).
    pub change: String,
}

/// One run of a task, as the changes it made are stamped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskRun {
    /// The run's group (`runtime_collab::task_run_group`): `<task>@<run>`.
    pub id: String,
    /// What the run was asked, as its task was titled when it ran.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl TaskRun {
    /// The task it is a run of: its group up to the `@`.
    #[must_use]
    pub fn task(&self) -> &str {
        self.id
            .split_once('@')
            .map_or(self.id.as_str(), |(task, _)| task)
    }
}

/// What a version did to its resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VersionKind {
    Created,
    #[default]
    Changed,
    Removed,
}

impl VersionKind {
    /// The word code reads it as: `created`, `changed` or `removed`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Changed => "changed",
            Self::Removed => "removed",
        }
    }

    /// The kind `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        match word {
            "created" => Some(Self::Created),
            "changed" => Some(Self::Changed),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// The version of a resource an order or a key reads, beside its fields: its first
/// (`created(r)`) or its last (`updated(r)`), ordered by their time. Named in an [`Order`] by
/// the pseudo-field ids `$created` and `$updated`, which no declared field has.
///
/// [`Order`]: crate::query::Order
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflected {
    Created,
    Updated,
}

impl Reflected {
    pub const CREATED: &'static str = "$created";
    pub const UPDATED: &'static str = "$updated";

    /// The version the pseudo-field `field` names.
    #[must_use]
    pub fn of(field: &str) -> Option<Self> {
        match field {
            Self::CREATED => Some(Self::Created),
            Self::UPDATED => Some(Self::Updated),
            _ => None,
        }
    }

    /// The pseudo-field naming it.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::Created => Self::CREATED,
            Self::Updated => Self::UPDATED,
        }
    }
}

/// The versions of `at` among a document's change notes, oldest first: the changes whose
/// resources include it (named by id, or by the path an older change named it by).
#[must_use]
pub fn versions_in(notes: &[ChangeNote], at: &ResourceRef) -> Vec<Version> {
    let id = at.id_string();
    // A change is named by its place in the document's log.
    notes
        .iter()
        .enumerate()
        .filter(|(_, note)| {
            note.touched
                .iter()
                .any(|touched| *touched == id || crate::value::flat_id(touched) == id)
        })
        .map(|(n, note)| Version {
            who: note.invoker.clone(),
            at: note
                .at
                .clone()
                .unwrap_or_else(|| crate::core::EPOCH.to_owned()),
            action: note.action.clone(),
            task: note.group.clone(),
            kind: if note.action == CREATE_ACTION {
                VersionKind::Created
            } else {
                VersionKind::Changed
            },
            fields: Vec::new(),
            resource: id.clone(),
            change: n.to_string(),
        })
        .collect()
}

/// Who made the changes `newest_first` of `at`, each once, at most `limit` of them
/// ([`Store::recent_authors`]).
pub fn recent_authors_in<'n>(
    newest_first: impl Iterator<Item = &'n ChangeNote>,
    at: &ResourceRef,
    limit: usize,
) -> Vec<String> {
    let id = at.id_string();
    let mut out: Vec<String> = Vec::new();
    for note in newest_first {
        if out.len() >= limit {
            break;
        }
        let touches = note
            .touched
            .iter()
            .any(|touched| *touched == id || crate::value::flat_id(touched) == id);
        if touches && !out.contains(&note.invoker) {
            out.push(note.invoker.clone());
        }
    }
    out
}

/// One field of one resource: the resource as [`ResourceRef::id_string`] writes it (its
/// document, then `/item` for an item), and the field id, or [`ANY_FIELD`] for every field
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
///
/// A store may hand out the items it holds ([`Listing::from_items`]) or a listing it reads
/// page by page as items are asked for ([`Listing::paged`]): a view that shows the first rows
/// of 100,000 items then reads those rows' page, not the 100,000.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    /// The items, when held.
    held: Vec<Rc<str>>,
    /// Where the items are read from instead, when paged.
    paged: Option<Rc<dyn PagedItems>>,
}

/// Items a store reads as they are asked for: a listing as it was when handed out, which a
/// later change does not alter.
pub trait PagedItems: std::fmt::Debug {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at `index`, `None` past the end.
    fn get(&self, index: usize) -> Option<Rc<str>>;

    /// Where the item `id` is, found by what orders it (a page or two), not by reading the
    /// items one by one: `Some(None)` when it is not in the listing, `None` when this listing
    /// cannot say so.
    fn position(&self, id: &str) -> Option<Option<usize>> {
        let _ = id;
        None
    }
}

impl PartialEq for Listing {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for Listing {}

impl Listing {
    pub fn from_ids(ids: impl IntoIterator<Item = ItemId>) -> Self {
        Self::from_items(ids.into_iter().map(Rc::from).collect())
    }

    /// A listing of the items `items`, held.
    pub fn from_items(items: Vec<Rc<str>>) -> Self {
        Self {
            held: items,
            paged: None,
        }
    }

    /// A listing read page by page from `items`.
    pub fn paged(items: Rc<dyn PagedItems>) -> Self {
        Self {
            held: Vec::new(),
            paged: Some(items),
        }
    }

    pub fn len(&self) -> usize {
        self.paged
            .as_ref()
            .map_or(self.held.len(), |paged| paged.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<Rc<str>> {
        match &self.paged {
            Some(paged) => paged.get(index),
            None => self.held.get(index).cloned(),
        }
    }

    pub fn first(&self) -> Option<Rc<str>> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Rc<str>> {
        self.len().checked_sub(1).and_then(|last| self.get(last))
    }

    /// Where the item `id` is: `Some(None)` when it is not in the listing. A listing read page by
    /// page finds it by what orders it, reading a page or two; one that cannot gives `None`
    /// rather than read its pages one by one.
    pub fn position(&self, id: &str) -> Option<Option<usize>> {
        match &self.paged {
            Some(paged) => paged.position(id),
            None => Some(self.held.iter().position(|held| &**held == id)),
        }
    }

    /// The items in order, each read as it is reached.
    pub fn iter(&self) -> impl Iterator<Item = Rc<str>> + '_ {
        (0..self.len()).map_while(|index| self.get(index))
    }

    /// The items, held: read whole when paged.
    pub fn items(&self) -> Vec<Rc<str>> {
        match &self.paged {
            Some(_) => self.iter().collect(),
            None => self.held.clone(),
        }
    }

    /// The items to change in place: read whole, and held from then on, when paged.
    pub fn items_mut(&mut self) -> &mut Vec<Rc<str>> {
        if self.paged.is_some() {
            self.held = self.iter().collect();
            self.paged = None;
        }
        &mut self.held
    }

    /// The item ids, as owned text.
    pub fn ids(&self) -> Vec<ItemId> {
        self.iter().map(|id| id.to_string()).collect()
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

impl Splice {
    /// The item the step puts in or takes out.
    #[must_use]
    pub fn item(&self) -> &str {
        match self {
            Self::Insert { item, .. } | Self::Remove { item, .. } => item,
        }
    }
}

/// The steps from `before` to `after`, two listings of one query (a filtered or sorted view of a
/// collection), when only the items `moved` came, went or moved between them and the others
/// kept their order: each is found where it was and where it is ([`Listing::position`]), so the
/// steps cost the items that changed, not the listing's length. The removals come first, from
/// the last, then the insertions in order; an item in the same place in both takes no step.
/// `None` when a listing cannot say where an item is.
#[must_use]
pub fn moved_steps<'a>(
    before: &Listing,
    after: &Listing,
    moved: impl IntoIterator<Item = &'a str>,
) -> Option<Vec<Splice>> {
    let mut gone: Vec<(usize, &str)> = Vec::new();
    let mut come: Vec<(usize, &str)> = Vec::new();
    let mut same = true;
    for id in moved {
        let was = before.position(id)?;
        let now = after.position(id)?;
        same &= was == now;
        gone.extend(was.map(|at| (at, id)));
        come.extend(now.map(|at| (at, id)));
    }
    // Each where it was, and the others in their order around them: nothing moved.
    if same {
        return Some(Vec::new());
    }
    gone.sort_unstable_by(|a, b| b.cmp(a));
    come.sort_unstable();
    let mut out = Vec::with_capacity(gone.len().saturating_add(come.len()));
    out.extend(gone.into_iter().map(|(at, id)| Splice::Remove {
        at,
        item: id.to_owned(),
    }));
    out.extend(come.into_iter().map(|(at, id)| Splice::Insert {
        at,
        item: id.to_owned(),
    }));
    Some(out)
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
    pub state: OutboxState,
}

/// Where an outside effect that is not settled yet is on its way out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutboxState {
    /// Queued: nothing was tried yet.
    #[default]
    Queued,
    /// Being handed to the service by the process whose token `by` is. It is marked so before
    /// it is handed over, so one whose process stopped meanwhile reads as outcome unknown and
    /// is never handed over twice.
    Sending { by: String },
    /// The service could not be reached, for `reason`: it waits to go online.
    Waiting { reason: String },
}

/// How an outside effect ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The service took it.
    Done,
    /// The service refused it, for this reason.
    Rejected(String),
    /// The process handing it over stopped before the service answered: whether it went is
    /// not known, and it is not tried again.
    Unknown,
}

impl Settled {
    /// The outcome as the record keeps it: `done`, `rejected` or `unknown`.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Rejected(_) => "rejected",
            Self::Unknown => "unknown",
        }
    }

    /// Why, when the service said.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Rejected(reason) => reason,
            Self::Done | Self::Unknown => "",
        }
    }
}

/// How the outside effect a resource's actions queued last stands, as `delivery(r)` reads it:
/// `waiting`, `sending`, `sent`, `rejected` or `unknown`, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryState {
    pub state: String,
    pub reason: String,
}

impl DeliveryState {
    /// How an entry stands: settled with `settled`'s outcome word and reason, else by `state`.
    #[must_use]
    pub fn of(state: &OutboxState, settled: Option<(&str, &str)>) -> Self {
        let (state, reason) = match settled {
            Some(("done", _)) => ("sent", ""),
            Some(("unknown", _)) => ("unknown", ""),
            Some((_, reason)) => ("rejected", reason),
            None => match state {
                OutboxState::Queued => ("waiting", ""),
                OutboxState::Sending { .. } => ("sending", ""),
                OutboxState::Waiting { reason } => ("waiting", reason.as_str()),
            },
        };
        Self {
            state: state.to_owned(),
            reason: reason.to_owned(),
        }
    }
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

    /// Create a document under the id `id` and commit it as one change: a package's per-room
    /// root, `root-<type id>`, which code names as `root T` or by the root's name.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the document exists or cannot be created.
    fn create_with_id(&mut self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        let _ = (record, id);
        Err(StoreError::Invalid(
            "this store makes documents under ids of its own".into(),
        ))
    }

    /// The type id stored at this resource.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document or item does not exist.
    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError>;

    /// Where the item `at` is: the resource whose collection holds it, and the collection's
    /// field. `None` for a document, which no collection holds.
    ///
    /// # Errors
    ///
    /// `NotFound` when the document or item does not exist.
    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, FieldId)>, StoreError>;
    /// How a reference in rich text reads for the person this store holds resources for: a
    /// resource they hold by what it is called now, one they cannot read as only being there.
    /// A store that cannot tell what its reader holds reads every resource as not shared: a
    /// reference never grants a read.
    fn reading(&self, reference: &crate::rich::Reference) -> crate::rich::Reading {
        if reference.as_person().is_some() {
            crate::rich::Reading::AsWritten
        } else {
            crate::rich::Reading::Unshared
        }
    }

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

    /// The items of the collection `field` of `at` (whose items are of the type `item`) that
    /// `pred` holds of, in order, shared. A store that keeps what it read keeps this too, and
    /// brings it up to date as the items change rather than evaluating every item again.
    ///
    /// # Errors
    ///
    /// As [`Store::listing`], and as reading a field `pred` reads.
    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        let all = self.listing(at, field)?;
        let mut items = Vec::new();
        for id in all.iter() {
            let target = at.item(&id, item.clone());
            let holds =
                pred.eval(&mut |read: &FieldRead| read.lit(self.read(&target, &read.spec)?))?;
            if holds {
                items.push(id);
            }
        }
        Ok(Rc::new(Listing::from_items(items)))
    }

    /// The items of the collection `field` of `at` (of the type `item`) that `pred` holds of,
    /// all of them when it is `None`, in the order `order` (the collection's when `None`),
    /// shared. A store with an index answers both from it.
    ///
    /// # Errors
    ///
    /// As [`Store::listing_where`], and as reading the field `order` is by.
    fn listing_query(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: Option<&Pred>,
        order: Option<&crate::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        let base = match pred {
            Some(pred) => self.listing_where(at, field, item, pred)?,
            None => self.listing(at, field)?,
        };
        let Some(order) = order else {
            return Ok(base);
        };
        let spec = FieldSpec {
            id: order.field.clone(),
            kind: order.kind,
        };
        let mut keyed = Vec::with_capacity(base.len());
        for id in base.iter() {
            let target = at.item(&id, item.clone());
            let value = match Reflected::of(&order.field) {
                Some(end) => self
                    .version_end(&target, end == Reflected::Updated)?
                    .map_or(Lit::Absent, |v| Lit::Time(v.at)),
                None => lit_of(&spec.id, self.read(&target, &spec)?, &Lit::Absent)?,
            };
            keyed.push((value, id));
        }
        keyed.sort_by(|(a, _), (b, _)| order_cmp(a, b));
        if order.descending {
            keyed.reverse();
        }
        Ok(Rc::new(Listing::from_items(
            keyed.into_iter().map(|(_, id)| id).collect(),
        )))
    }

    /// The resources of the type `ty` anywhere in the store that `pred` holds of, in the order
    /// `order`: a query that looks inside no one resource (search.md, "A query"). A store
    /// with an index answers it from the index's columns of the type; this one has none.
    ///
    /// # Errors
    ///
    /// A store that cannot answer it.
    fn select(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&crate::query::Order>,
    ) -> Result<Vec<ResourceRef>, StoreError> {
        let _ = (pred, order);
        Err(StoreError::Invalid(format!(
            "this store answers no query of every {ty}; look inside a resource (`in:`)"
        )))
    }

    /// The notes of a document's changes, oldest first.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing document.
    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError>;

    /// The versions of the resource `at`, oldest first: each change that changed it, with who
    /// made it, when, and which action (reflection: `created(r)`, `updated(r)`, `history(r)`).
    ///
    /// # Errors
    ///
    /// As [`Store::history`].
    fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        Ok(versions_in(&self.history(&at.doc)?, at))
    }

    /// The first version of the resource `at` (`created(r)`), or its last (`updated(r)`,
    /// `last`). A store with an index keeps both per resource, so a view showing them, or a
    /// list sorted by them, reads no history.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn version_end(&self, at: &ResourceRef, last: bool) -> Result<Option<Version>, StoreError> {
        let mut versions = self.versions(at)?;
        Ok(if last {
            versions.pop()
        } else {
            versions.into_iter().next()
        })
    }

    /// Every resource of the document `doc` (it and its items) with its versions, oldest
    /// first: what a window's mark counts the unseen changes of.
    ///
    /// # Errors
    ///
    /// As [`Store::versions`].
    fn versions_within(
        &self,
        doc: &DocId,
    ) -> Result<std::collections::BTreeMap<String, Vec<Version>>, StoreError> {
        let root = ResourceRef::root(doc.clone(), String::new());
        Ok(std::iter::once((doc.clone(), self.versions(&root)?)).collect())
    }

    /// Every version the task run `run` (its group) made, of every resource, oldest first:
    /// what `changes(of: run)` reads.
    ///
    /// # Errors
    ///
    /// A store that keeps no log says so.
    fn run_changes(&self, run: &str) -> Result<Vec<Version>, StoreError> {
        Err(StoreError::Invalid(format!(
            "this store keeps no log to find the changes of the run {run} in"
        )))
    }

    /// The field `field` of `at` as it was before the change `change` (a [`Version::change`]):
    /// what a bound field changed by someone else shows as its previous value.
    ///
    /// # Errors
    ///
    /// `NotFound` for a change the store does not hold; a store that keeps no log says so.
    fn read_before(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        change: &str,
    ) -> Result<FieldValue, StoreError> {
        let _ = (at, field);
        Err(StoreError::Invalid(format!(
            "this store keeps no log to read before the change {change} in"
        )))
    }

    /// What the change `change` (a [`Version::change`]) wrote to each of `fields` of `at`: the
    /// field's value before the change and after it, read from the change itself, for each
    /// field it changed, in the order of `fields`. A field it wrote with what the field held is
    /// left out.
    ///
    /// # Errors
    ///
    /// `NotFound` for a change the store does not hold; a store that keeps no log says so.
    fn written(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        change: &str,
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        let _ = (at, fields);
        Err(StoreError::Invalid(format!(
            "this store keeps no log to read what the change {change} wrote"
        )))
    }

    /// Who made the latest versions of `at`, newest first and each once: at most `limit` of
    /// them, from at most the last `window` changes of its document. What a rich input's `@`
    /// drawer offers first; its cost is bounded by `window`, never by how long the history is.
    ///
    /// # Errors
    ///
    /// As [`Store::history`].
    fn recent_authors(
        &self,
        at: &ResourceRef,
        limit: usize,
        window: usize,
    ) -> Result<Vec<String>, StoreError> {
        Ok(recent_authors_in(
            self.history(&at.doc)?.iter().rev().take(window),
            at,
            limit,
        ))
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId;
    fn unsubscribe(&mut self, sub: SubId);

    /// Outside effects queued and not yet settled.
    fn outbox_pending(&self) -> Vec<OutboxEntry>;

    /// Record where an entry that is not settled is now (handed over, or waiting to go
    /// online), as one change.
    ///
    /// # Errors
    ///
    /// `NotFound` when the entry is gone.
    fn outbox_mark(
        &mut self,
        entry: &OutboxEntry,
        state: &OutboxState,
        note: &ChangeNote,
    ) -> Result<(), StoreError>;

    /// Mark an entry settled with `outcome`, as one change.
    ///
    /// # Errors
    ///
    /// `NotFound` when the entry is gone.
    fn outbox_settle(
        &mut self,
        entry: &OutboxEntry,
        outcome: &Settled,
        note: &ChangeNote,
    ) -> Result<(), StoreError>;

    /// How the outside effect each resource's actions queued last stands, by document: what
    /// `delivery(r)` reads.
    fn outbox_states(&self) -> Vec<(DocId, DeliveryState)>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be saved.
    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError>;

    /// # Errors
    ///
    /// A [`StoreError`] when the journal cannot be read.
    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError>;
}
