//! [`TiloStore`] — the runtime's [`Store`] over the workspace record.
//!
//! The runtime reads and mutates resources through the `Store` trait and expects its own commits
//! to be visible at once. The workspace record, though, belongs to the framework: every change to it
//! is a recorded action committed by the executor, so it carries the invoker, the group and the
//! encoded action, and replays on every peer. The store therefore writes to a **mirror**, a
//! replica of the workspace record, and reads from each package's **index** ([`crate::index`]):
//!
//! * a transaction applies each mutation to the mirror as it is made and records it as a
//!   [`DynEdit`]; what it changed is derived, field by field, into the index's **overlay** (the
//!   open writes as the index would hold them), so reads inside it see them;
//! * committing queues a [`DynAction`] for the host, which commits it through the executor
//!   ([`TiloStore::take_committed`]). The mirror keeps what the store wrote as its open
//!   transaction, never as a change of its own;
//! * when the workspace record moves on (the executor applied the queued action, or a peer's change
//!   arrived), [`TiloStore::sync`] drops the mirror's open transaction and the overlay,
//!   applies the record's new changes to the mirror, derives what they changed into each index
//!   (stamped with the record's heads) and tells each subscriber exactly which fields of which
//!   resources they changed, and how each collection's listing moved.
//!
//! Lists, filters and the fields rows show are read from the index, page by page, so opening a
//! window reads what it shows and not the record. The store may even open before the record is
//! loaded ([`TiloStore::detached`]): it reads indexes stamped with the heads they reflect, and
//! takes the record when it is had ([`TiloStore::sync`], or the loader the host gave it
//! for a write or a field no index holds), catching each index up from its heads.
//!
//! **The unsaved layer.** What an origin derives from nothing at every start (the clock, the
//! desk, the device's state: a type whose origin kind says [`crate::schema::TypeSchema::derived`])
//! is never put in the record. Its writes go into the same mirror open transaction as any
//! write, and so are read at once, but committing them queues nothing: they stay in the
//! mirror's open transaction and its overlay, which is what reads answer from. The record
//! moving on ([`TiloStore::sync`]) drops the store's saved writes (the record has them)
//! and leaves the derived ones where they are, so an origin's update never round-trips through
//! the record: no action is committed, no peer replays it, and no index is rebuilt for it.
//!
//! One transaction may be open at a time: rolling one back puts the mirror and the overlay back
//! as the store's earlier writes left them.
//!
//! **Chunks.** The record may be split: resources it does not hold itself are in chunk documents
//! above it ([`crate::chunk`]), each a document of its own the host sends when it is wanted. The
//! store holds the chunks it was sent ([`TiloStore::sync`]) beside the record, and every
//! read and write of a resource is routed to the chunk the record's manifest places it in, so a
//! write reaches one document. A record written before chunks is one chunk whose manifest is
//! empty, and reads and writes as it always did.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{ChangeHash, ObjType, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::{Model, Segment};
use tilolang_runtime::DOC_VERSIONS as VERSIONS;
use tilolang_runtime::Journal;
use tilolang_runtime::ids::{AppId, DocId, FieldId, ItemId, RunId, TypeId};
use tilolang_runtime::store::{
    ANY_FIELD, Applied, ChangeNote, ChangeSummary, FieldKey, FieldSpec, FieldValue, Link, Listing,
    ListingChange, Mutation, NewRecord, OutboxEntry, Pred, Scalar, Splice, Store, StoreError,
    SubId, Txn, Version, doc_of,
};
use tilolang_runtime::value::ResourceRef;

use crate::action::{
    Authorship, DynAction, DynEdit, RECORDS_KEY, apply_edits, outbox_entries, read_resource,
};
use crate::background::Waker;
use crate::chunk::{self, ChunkNo, Chunks, Stamps};
use crate::index::derive::{Derive, DocReads, Reached, linked_list, split_link};
use crate::index::lists::Entry;
use crate::index::overlay::{ListState, Overlay};
use crate::index::select::Carried;
use crate::index::view::{self, View};
use crate::index::{self as idx, Index, IndexError, IndexReads};
use crate::items::Nested;
use crate::reads::{changed_by, default_value, field_of_json, locate_ref, read_field, type_at};
use crate::record::DynRecord;
use crate::schema::{MergeKind, TYPE_KEY, lookup};

type Doc = tilo_resource::Doc;

/// An action the runtime committed, waiting for the host to commit it through the executor.
#[derive(Clone, Debug, PartialEq)]
pub struct Committed {
    /// Names the action when the host asks the executor to commit it.
    pub ticket: u64,
    pub action: DynAction,
}

struct Subscription {
    id: SubId,
    doc: DocId,
    callback: Box<dyn FnMut(&ChangeSummary)>,
}

#[derive(Default)]
struct TxnState {
    app: Option<AppId>,
    /// The resource kind of the first resource touched.
    kind: Option<String>,
    edits: Vec<DynEdit>,
    /// How many of the store's writes the mirror held when the transaction began.
    start: usize,
    touched: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The collections whose items joined, left or moved, by the document that holds them.
    collections: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The records the transaction made ([`Inner::create_in`]): a transaction that is rolled
    /// back takes them out of the index again.
    created: Vec<String>,
    /// How the collections it changed changed, in order.
    listings: Vec<ListingChange>,
    /// The resources it wrote that are saved to the record, and those their origin derives
    /// ([`crate::schema::TypeSchema::derived`]): the latter go to the store's unsaved layer.
    /// One action's transaction writes one kind or the other, never both.
    saved: usize,
    derived: usize,
}

/// What the record moved on from, when known: the heads of each chunk before, the versions its
/// new changes made, and where those changes placed the items they did.
type Since<'a> = Option<(&'a Stamps, &'a Ends, &'a idx::reflect::Placed)>;

/// The versions changes made, per resource they changed: its first and last, and the last
/// change of each of its fields ([`idx::reflect::Ends`]).
type Ends = BTreeMap<String, idx::reflect::Ends>;

/// Items of collections a change touched: per collection, the ids of the items added, removed
/// or moved. Each is moved in the collection's listing, which is not read again.
pub(crate) type ItemTouches = BTreeMap<FieldKey, BTreeSet<String>>;

/// The runtime's name for a bound control's edit (`ViewInstance::edit`).
const RUNTIME_EDIT: &str = "$edit";

/// The action a committed change is published as: what the runtime named it, except a bound
/// control's edit, which the runtime names `$edit` with the arguments `[field, value]`. Every
/// receiver, and the identity service that signs the change, decides it against the package's
/// surface, which publishes such an edit as `edit-flag` (a checkbox) or `edit-text` (anything
/// else) with the arguments `{field, value}`, as the door plans an edit of a field. Under that
/// name the keystrokes of one field also coalesce into one undo step.
fn published_name(note: &ChangeNote) -> (String, Json) {
    if note.action != RUNTIME_EDIT {
        return (note.action.clone(), note.args.clone());
    }
    let field = note.args.get(0).cloned().unwrap_or(Json::Null);
    let value = note.args.get(1).cloned().unwrap_or(Json::Null);
    let action = if value.is_boolean() {
        crate::action::EDIT_FLAG_ACTION
    } else {
        crate::action::EDIT_TEXT_ACTION
    };
    (
        action.to_owned(),
        serde_json::json!({"field": field, "value": value}),
    )
}

/// Add `splices` to the steps of the collection `key` in `out`: none at all says that it did
/// not change after all.
fn record_splices(out: &mut Vec<ListingChange>, key: &FieldKey, splices: Vec<Splice>) {
    match out.iter_mut().find(|change| change.key == *key) {
        Some(change) => change.splices.extend(splices),
        None => out.push(ListingChange {
            key: key.clone(),
            splices,
        }),
    }
}

/// One write the store made into the mirror's open transaction: what to apply again to put the
/// mirror back after a rollback, and what it changed.
#[derive(Clone)]
struct LocalWrite {
    app: AppId,
    /// The chunk it was written into.
    chunk: ChunkNo,
    edit: DynEdit,
    seed: Vec<u8>,
    touched: Vec<FieldKey>,
    /// The collection items it added, removed or moved, by each collection they joined or
    /// left.
    items: Vec<(FieldKey, String)>,
    /// The items whose placement it wrote, each with whether it made them.
    placed: Vec<(DocId, String, bool)>,
    /// For the last write of a committed action, the versions it made, provisional until the
    /// record holds the action: per resource it changed, the invoker and the invocation's
    /// recorded time ([`Inner::provisional_versions`]).
    versions: Option<Ends>,
    /// Whether it wrote only resources its origin derives ([`crate::schema::TypeSchema::derived`]):
    /// it is the store's unsaved layer, and no change commits it. The record moving on leaves
    /// it in the mirror ([`Inner::drop_committed_local`]).
    derived: bool,
}

impl LocalWrite {
    /// What it reached, as derivation takes it.
    fn reached(&self) -> Reached {
        let mut reached = Reached::default();
        for key in &self.touched {
            let fields = reached.docs.entry(key.doc().to_owned()).or_default();
            fields.push(key.clone());
        }
        for (doc, id, made) in &self.placed {
            let held = reached
                .placed
                .entry(doc.clone())
                .or_default()
                .entry(id.clone())
                .or_default();
            *held = *held || *made;
        }
        reached
    }
}

/// The collection items a write adds, removes or moves, by each collection they join or leave,
/// and the items whose placement it writes, each with whether it makes them.
type Moves = (Vec<(FieldKey, String)>, Vec<(DocId, String, bool)>);

/// What a mutation writes, planned against the store as it is: the edit, the item it makes,
/// what it changes, and the items it places.
struct Planned {
    /// What it writes, in order: the run's manifest entry first, then the edit of the chunk it
    /// places the item in.
    edits: Vec<DynEdit>,
    item: Option<String>,
    touched: Vec<FieldKey>,
    items: Vec<(FieldKey, String)>,
    placed: Vec<(DocId, String, bool)>,
}

/// Where an item is: the resource holding its collection, the field, the order key; where an
/// item of a record written before items were flat is nested; and its type.
struct ItemAt {
    holder: String,
    field: String,
    key: String,
    nesting: Vec<(String, String)>,
    ty: TypeId,
    /// The chunk its map is stored in.
    chunk: ChunkNo,
}

/// A trigger's run, as the store records it in the change it commits: the resource it runs
/// for, and what the trigger last did for it (the period it ran in, or the instant it fired at).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Triggering {
    pub doc: DocId,
    /// The trigger's id and the resource's path: one mark per trigger, per resource.
    pub key: String,
    pub value: String,
}

impl Triggering {
    fn edit(&self) -> DynEdit {
        DynEdit::TriggerRan {
            id: self.doc.clone(),
            key: self.key.clone(),
            value: self.value.clone(),
        }
    }
}

/// What the store did, for tests and the benchmark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Times the store took the record: its first copy of it, whole.
    pub attaches: u64,
    /// Syncs that applied the record's new changes to the mirror.
    pub incremental_syncs: u64,
    /// Syncs that had to copy the whole record: the mirror held a change of its own.
    pub full_syncs: u64,
    /// Indexes built from the record whole, there being none to read (or none to trust).
    pub builds: u64,
    /// Indexes brought up to date from the heads they were stamped with.
    pub catch_ups: u64,
}

/// How much the store read, counted since it was made: work that does not depend on the
/// machine or its load, which tests hold to how it grows with the record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Of the record: resources found by walking it, field values, collections listed by
    /// walking their entries, and those entries. Opening over an index reads none of them.
    pub doc: DocReads,
    /// Of the indexes: list directories and pages, and field values.
    pub index: IndexReads,
    /// Items a filter was evaluated on one by one (a filter no column answers).
    pub evaluated: u64,
    /// Times the versions of every resource were derived from the record's log: what a read
    /// of a resource's whole history costs, and what a first render reads none of.
    pub versions: u64,
}

mod listed;
use listed::ListedOnce;
mod scoped;
mod sync;
use scoped::{Changing, Held};
mod clock;
use clock::ClockSource;

mod index_build;
mod loading;
use loading::{planned_unheld, unloaded};
mod reading;
use index_build::AppIndex;
use reading::{
    collections_by_doc, everything, mirror_of, name_version_changes, note_derived, scan,
    version_fields, write_as_a_known_actor,
};
mod outbox;
mod plan;
mod run;
mod runtime;
pub(crate) use plan::rich_edit;
mod unsaved;
mod upgrade_fields;
mod versions;
mod written;
use versions::{NO_VERSIONS, note_versions};

/// Gives the store the workspace record when it opened without it: for a write, or a field no index
/// holds. `None` when the record cannot be had.
pub type Loader = Box<dyn FnMut() -> Option<Doc>>;

/// Gives the store the chunk [`ChunkNo`] when it was not sent it: when something reads into it (a
/// row a window shows, or a lookup the index routes to it) or a write is addressed to it. `None`
/// when the chunk cannot be had.
pub type ChunkLoader = Box<dyn FnMut(ChunkNo) -> Option<Doc>>;

struct Inner {
    /// The documents the store holds: the workspace record (chunk [`chunk::FIRST`]) and the
    /// chunks above it it was sent, each with the store's writes as its open transaction. The
    /// record is `None` until it is had (see [`TiloStore::detached`]). Each document is shared
    /// with the snapshots taken of it until it changes.
    chunks: Chunks,
    /// What the store wrote into the mirror since it last caught up with the record, oldest
    /// first: the mirror's open transaction.
    local: Vec<LocalWrite>,
    /// The transactions of `local` the record holds ([`Self::settle_versions`]): a change of the
    /// record's carries what they wrote, so reads hold it from the index and the writes are not
    /// derived into the overlay again.
    settled: BTreeSet<Vec<u8>>,
    stats: StoreStats,
    /// Every resource of the workspace: its app and type.
    index: BTreeMap<DocId, (AppId, TypeId)>,
    /// Per package, its index.
    apps: BTreeMap<AppId, AppIndex>,
    /// The store's writes as the indexes would hold them.
    overlay: Arc<Overlay>,
    /// Where the indexes are kept: `None` keeps them in memory, built when a package loads.
    index_dir: Option<PathBuf>,
    loader: Option<Loader>,
    /// Where a chunk the store was not sent comes from when something reads into it or a write
    /// is addressed to it ([`TiloStore::set_chunk_loader`]).
    chunk_loader: Option<ChunkLoader>,
    /// Called when an index built off the frame is ready to be taken in.
    wake: Option<Waker>,
    /// What the store read of the record so far: derivation, and reads no index answered.
    doc_reads: std::cell::Cell<DocReads>,
    /// Items filters were evaluated on one by one.
    evaluated: std::cell::Cell<u64>,
    /// Times the versions of every resource were derived from the log.
    versions_built: std::cell::Cell<u64>,
    /// How `now` reads and where the person is, as the host names them: the time the columns that
    /// read the clock are written at ([`crate::index::Clock`]).
    clock: ClockSource,
    listed: RefCell<Vec<ListedOnce>>,
    /// What each change wrote to the fields of each resource it changed, by the change and the
    /// resource, as [`Store::written`] read it: a change never changes, so it is read once.
    written: RefCell<HashMap<(String, String), written::Written>>,
    txns: HashMap<u64, TxnState>,
    next_txn: u64,
    subs: Vec<Subscription>,
    /// The migration being run, by its action id, and the resource it runs on: the action it
    /// commits records that it ran ([`DynEdit::Migrated`]).
    migrating: Option<(String, DocId)>,
    /// The trigger run under way: the resource it runs for, and what it last did for it, which
    /// the action it commits records in the same change ([`DynEdit::TriggerRan`]).
    triggering: Option<Triggering>,
    next_sub: u64,
    /// What each [`Watch`] has not taken yet.
    watches: Vec<std::rc::Weak<RefCell<Changes>>>,
    /// Trial runs under way (checkpoints not rewound yet).
    trials: usize,
    /// What changed during the trial runs under way, told to nobody: a trial run is taken back.
    held: Vec<ChangeSummary>,
    committed: VecDeque<Committed>,
    /// The runs that have not finished, as they began: where a run's first write stands in the
    /// record's local changes and in what it queued, so that a run that fails anywhere takes all
    /// it wrote back out ([`Store::abort_run`]).
    runs: HashMap<String, run::RunBegin>,
    /// Handed to the host and not yet answered by the door.
    handed: BTreeMap<u64, DynAction>,
    next_ticket: u64,
    journals: BTreeMap<RunId, Journal>,
    notes: BTreeMap<DocId, Vec<ChangeNote>>,
    /// The versions of each resource the record's changes made (reflection), built from the
    /// record the first time one is asked for and kept up as it moves.
    versions: Option<versions::Versions>,
    instance: String,
    /// The device that carries out the outside effects this store queues: they stay queued in
    /// the record, and this device alone settles them.
    device: String,
    counter: u64,
    /// The last snapshot taken, while the store has not changed since: what search and
    /// publication starting in one frame share.
    snapshot: std::sync::Weak<Snapshot>,
    /// Where the items of records written before items were flat are.
    nested: Nested,
    /// What a reference to a resource the reader does not hold says, in their language
    /// ([`crate::reading`]).
    unshared: Arc<str>,
    /// The schemas of the packages this store holds: which fields a record's type id names, by
    /// the package version this store loaded. One per store, not one per process, so two hosts
    /// on one thread (two packages, two versions) resolve a record's type by their own.
    schemas: Arc<crate::schema::Schemas>,
}

/// The store as it was when taken, to read on another thread: each package's index as its last
/// commit left it, with the store's writes over it. Taking one copies nothing of the workspace.
pub struct Snapshot {
    index: BTreeMap<DocId, (AppId, TypeId)>,
    apps: BTreeMap<AppId, Arc<idx::Snapshot>>,
    overlay: Arc<Overlay>,
    /// Every package loaded, whether or not its index was read from yet.
    packages: BTreeSet<AppId>,
    unshared: Arc<str>,
    /// The schemas of the store it was taken from: what its reads resolve a record's type by,
    /// wherever it is read (an index build on a thread of its own reads a snapshot).
    schemas: Arc<crate::schema::Schemas>,
    /// The time it was taken at: what a column reading the clock holds of an item the open
    /// writes changed ([`crate::index::Clock`]).
    clock: crate::index::Clock,
}

impl Snapshot {
    /// The schemas of the store this was taken from, installed on this thread for as long as
    /// the scope lives: a read of it on a thread of its own resolves a record's type by the
    /// package version that store holds.
    #[must_use]
    pub fn scoped(&self) -> crate::schema::Current {
        self.schemas.enter()
    }

    /// The schemas of the store this was taken from.
    #[must_use]
    pub fn schemas(&self) -> Arc<crate::schema::Schemas> {
        Arc::clone(&self.schemas)
    }

    /// The index of `app` as it was, read through the store's writes.
    #[must_use]
    pub fn view(&self, app: &str) -> Option<View<'_>> {
        Some(View {
            snapshot: self.apps.get(app)?,
            overlay: &self.overlay,
            clock: self.clock,
        })
    }

    /// The app and type of the record `doc`.
    #[must_use]
    pub fn record_of(&self, doc: &str) -> Option<&(AppId, TypeId)> {
        self.index.get(doc)
    }

    /// [`TiloStore::first_text`], as it was when the snapshot was taken.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<(String, String)> {
        let (value, held) = self.first_stored(at)?;
        Some((crate::reading::said(self, &at.id_string(), value)?, held))
    }

    /// The text value `value` of the resource `of` (its id) as the store's reader reads it
    /// ([`crate::reading::said`]).
    #[must_use]
    pub fn said(&self, of: &str, value: FieldValue) -> Option<String> {
        crate::reading::said(self, of, value)
    }

    fn first_stored(&self, at: &ResourceRef) -> Option<(FieldValue, String)> {
        let (app, ty) = self.index.get(&at.doc)?;
        let view = self.view(app)?;
        let schema = lookup(&at.ty).or_else(|| lookup(ty))?;
        view.first_text(&at.id_string(), schema).ok().flatten()
    }
}

impl crate::reading::Holdings for Snapshot {
    fn held(&self, at: &ResourceRef) -> Option<String> {
        let (app, ty) = self.index.get(&at.doc)?;
        if at.item.is_none() {
            return Some(ty.clone());
        }
        self.view(app)?.type_of(&at.id_string()).ok().flatten()
    }

    fn first_value(&self, at: &ResourceRef) -> Option<FieldValue> {
        self.first_stored(at).map(|(value, _)| value)
    }

    fn keeps(&self, app: &str) -> bool {
        self.packages.contains(app)
    }

    fn unshared(&self) -> Arc<str> {
        Arc::clone(&self.unshared)
    }
}

impl crate::reading::Holdings for TiloStore {
    fn held(&self, at: &ResourceRef) -> Option<String> {
        Store::type_of(self, at).ok()
    }

    fn first_value(&self, at: &ResourceRef) -> Option<FieldValue> {
        self.first_stored(at).map(|(value, _)| value)
    }

    fn keeps(&self, app: &str) -> bool {
        self.held().apps.contains_key(app)
    }

    fn unshared(&self) -> Arc<str> {
        Arc::clone(&self.held().unshared)
    }
}

/// What changed in the store since a [`Watch`] last took it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Per document, the fields that changed (see [`FieldKey`]).
    pub docs: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The documents that may have changed anywhere.
    pub whole: BTreeSet<DocId>,
    /// Anything anywhere may have changed: the record was taken anew, or an index built.
    pub everything: bool,
    /// How collections' items changed, in the order the changes happened (see
    /// [`ChangeSummary::listings`]); a collection named in `docs` without steps here changed in
    /// a way the store could not say.
    pub listings: Vec<ListingChange>,
    /// The fields named by a change that did not say how its listing changed: steps for one of
    /// these collections are not the whole story.
    unsaid: BTreeSet<FieldKey>,
}

impl Changes {
    fn add(&mut self, summary: &ChangeSummary) {
        if summary.fields.is_empty() {
            self.whole.insert(summary.doc.clone());
        } else {
            self.docs
                .entry(summary.doc.clone())
                .or_default()
                .extend(summary.fields.iter().cloned());
        }
        for change in &summary.listings {
            record_splices(&mut self.listings, &change.key, change.splices.clone());
        }
        for key in &summary.fields {
            if summary.unsaid(key) {
                self.unsaid.insert(key.clone());
            }
        }
    }

    /// Whether a change to `key` did not say how its collection's items changed: what it took
    /// out or put in is unknown, so a reader of item text must read everything again.
    #[must_use]
    pub fn unsaid(&self, key: &FieldKey) -> bool {
        self.unsaid.contains(key)
    }

    /// The steps that changed the collection `key`, in order, when the store could say.
    #[must_use]
    pub fn listing(&self, key: &FieldKey) -> Option<&[Splice]> {
        if self.unsaid.contains(key) {
            return None;
        }
        self.listings
            .iter()
            .find(|change| change.key == *key)
            .map(|change| change.splices.as_slice())
    }

    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty() && self.whole.is_empty() && !self.everything
    }
}

/// A cursor over the store's changes; see [`TiloStore::watch`].
pub struct Watch {
    changes: Rc<RefCell<Changes>>,
}

impl Watch {
    /// What changed since the last call.
    #[must_use]
    pub fn take(&self) -> Changes {
        std::mem::take(&mut self.changes.borrow_mut())
    }
}

/// The store as it is now, to put back after a trial run; see [`TiloStore::checkpoint`].
pub struct Checkpoint {
    /// How many changes were held from the trial runs under way.
    held: usize,
    /// How many of the store's writes the mirror held.
    local: usize,
    index: BTreeMap<DocId, (AppId, TypeId)>,
    notes: BTreeMap<DocId, Vec<ChangeNote>>,
    overlay: Arc<Overlay>,
}

/// The runtime's store over the workspace record. A cheap handle: clones share the mirror.
#[derive(Clone)]
pub struct TiloStore {
    inner: Rc<RefCell<Inner>>,
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn invalid(what: impl Into<String>) -> StoreError {
    StoreError::Invalid(what.into())
}

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

impl TiloStore {
    /// A store whose mirror starts as `record`, whose outside effects `device` carries out.
    #[must_use]
    pub fn on_device(record: &Doc, device: &str) -> Self {
        let store = Self::new(record);
        device.clone_into(&mut store.mutate().device);
        store
    }

    /// A store whose mirror starts as `record`, its indexes in memory.
    #[must_use]
    pub fn new(record: &Doc) -> Self {
        let store = Self::empty(None);
        store.mutate().attach_document(chunk::FIRST, record);
        store
    }

    /// A store without the record yet, reading the indexes kept in `dir`: what opens a workspace
    /// before its record is loaded. Packages whose index is missing show nothing until the
    /// record is had ([`Self::sync_from`], or the [`Loader`]).
    #[must_use]
    pub fn detached(dir: PathBuf) -> Self {
        Self::empty(Some(dir))
    }

    fn empty(index_dir: Option<PathBuf>) -> Self {
        let instance: String = automerge::ActorId::random()
            .to_hex_string()
            .chars()
            .take(8)
            .collect();
        Self {
            inner: Rc::new(RefCell::new(Inner {
                chunks: Chunks::default(),
                local: Vec::new(),
                settled: BTreeSet::new(),
                stats: StoreStats::default(),
                index: BTreeMap::new(),
                apps: BTreeMap::new(),
                overlay: Arc::default(),
                index_dir,
                loader: None,
                chunk_loader: None,
                wake: None,
                doc_reads: std::cell::Cell::default(),
                evaluated: std::cell::Cell::new(0),
                versions_built: std::cell::Cell::new(0),
                clock: ClockSource::default(),
                listed: RefCell::default(),
                txns: HashMap::new(),
                next_txn: 1,
                subs: Vec::new(),
                migrating: None,
                triggering: None,
                next_sub: 1,
                watches: Vec::new(),
                trials: 0,
                held: Vec::new(),
                committed: VecDeque::new(),
                runs: HashMap::new(),
                handed: BTreeMap::new(),
                next_ticket: 1,
                journals: BTreeMap::new(),
                notes: BTreeMap::new(),
                versions: None,
                device: instance.clone(),
                instance,
                counter: 0,
                snapshot: std::sync::Weak::new(),
                nested: Nested::default(),
                unshared: Arc::from(tilolang_runtime::rich::UNSHARED),
                written: RefCell::default(),
                schemas: Arc::new(crate::schema::Schemas::store()),
            })),
        }
    }

    /// Keep the indexes of packages loaded from now on in `dir`, rather than in memory: a
    /// window's workspace, opened again later, reads them instead of building them.
    pub fn set_index_dir(&self, dir: PathBuf) {
        self.mutate().index_dir = Some(dir);
    }

    /// Set the device whose outside effects this store queues.
    pub fn set_device(&self, device: &str) {
        device.clone_into(&mut self.mutate().device);
    }

    /// How the store gets the workspace record when it opened without it.
    pub fn set_loader(&self, loader: Loader) {
        self.mutate().loader = Some(loader);
    }

    /// Have `wake` called (from another thread) when an index built off the frame is ready
    /// for [`Self::poll_index`] to take in.
    pub fn set_waker(&self, wake: Waker) {
        self.mutate().wake = Some(wake);
    }

    /// Whether the mirror holds exactly what `record` holds: the record it last brought itself
    /// up to date with, not one before or after it. A record of the past (a version an agent
    /// reads) is not it, even when it was handed to [`Self::sync_from`], which only moves forward.
    #[must_use]
    pub fn is_at(&self, record: &Doc) -> bool {
        self.held().chunks.first().is_some_and(|mirror| {
            mirror.committed_document().get_heads() == record.committed_document().get_heads()
        })
    }

    /// Whether the store holds the workspace record (else it reads indexes alone).
    #[must_use]
    pub fn has_record(&self) -> bool {
        self.held().chunks.has_record()
    }

    /// What the store holds of each chunk, the record first: how many changes of it the store
    /// has, and how many writes of its own are open in it. A resource placed in a chunk is read
    /// and written there, so its writes show in that chunk's count alone.
    #[must_use]
    pub fn held_chunks(&self) -> Vec<(ChunkNo, usize, usize)> {
        self.held()
            .chunks
            .docs()
            .map(|(chunk, doc)| {
                (
                    chunk,
                    doc.committed_document().get_changes(&[]).len(),
                    doc.pending_ops(),
                )
            })
            .collect()
    }

    /// What the store did so far.
    #[must_use]
    pub fn stats(&self) -> StoreStats {
        self.held().stats
    }

    /// How much the store read so far.
    #[must_use]
    pub fn read_stats(&self) -> ReadStats {
        let inner = self.held();
        let index = inner
            .apps
            .values()
            .map(|held| held.index.stats().reads())
            .fold(IndexReads::default(), IndexReads::plus);
        ReadStats {
            doc: inner.doc_reads.get(),
            index,
            evaluated: inner.evaluated.get(),
            versions: inner.versions_built.get(),
        }
    }

    /// The versions of every resource the record's changes made, by resource: built from the
    /// record the first time they are asked for, and kept up as it moves.
    fn built_versions(&self) -> std::cell::Ref<'_, HashMap<String, Vec<Version>>> {
        {
            let mut held = self.changing();
            let inner = &mut *held;
            if inner.versions.is_none() {
                inner
                    .versions_built
                    .set(inner.versions_built.get().saturating_add(1));
                let mut built = versions::Versions::default();
                for (_, doc) in inner.chunks.docs() {
                    note_versions(
                        &mut built,
                        &doc.committed_document().get_changes(&[]),
                        doc,
                        &inner.index,
                    );
                }
                // Oldest first, by [`Version::order`]: the order the changes reached this
                // replica in says nothing of when they happened or of where they stand
                // causally, and every replica reads one history.
                for held in built.by_resource.values_mut() {
                    held.sort_by(|one, other| one.order().cmp(&other.order()));
                }
                inner.versions = Some(built);
            }
        }
        let (schemas, scope) = self.scoped();
        drop(schemas);
        std::cell::Ref::map(self.inner.borrow(), move |inner| {
            let _ = &scope;
            inner
                .versions
                .as_ref()
                .map_or(&*NO_VERSIONS, |held| &held.by_resource)
        })
    }

    /// The actions the runtime committed since the host last took them, oldest first. They stay
    /// available by ticket ([`Self::committed`]) until the door has answered ([`Self::take_ticket`]).
    #[must_use]
    pub fn take_committed(&self) -> Vec<Committed> {
        let mut inner = self.changing();
        let taken: Vec<Committed> = inner.committed.drain(..).collect();
        for entry in &taken {
            inner.handed.insert(entry.ticket, entry.action.clone());
        }
        taken
    }

    /// As [`Self::take_committed`], for what was committed under a ticket of `first` or later:
    /// what one invocation committed, leaving the rest queued.
    #[must_use]
    pub fn take_committed_from(&self, first: u64) -> Vec<Committed> {
        let mut inner = self.changing();
        let (mut taken, mut kept) = (Vec::new(), VecDeque::new());
        for entry in std::mem::take(&mut inner.committed) {
            if entry.ticket >= first {
                taken.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        inner.committed = kept;
        for entry in &taken {
            inner.handed.insert(entry.ticket, entry.action.clone());
        }
        taken
    }

    /// What was committed under a ticket of `first` or later and is still queued, left queued:
    /// what one invocation would hand over.
    #[must_use]
    pub fn committed_from(&self, first: u64) -> Vec<Committed> {
        self.held()
            .committed
            .iter()
            .filter(|entry| entry.ticket >= first)
            .map(|entry| Committed {
                ticket: entry.ticket,
                action: entry.action.clone(),
            })
            .collect()
    }

    /// The action committed under `ticket`, while it is queued or handed over.
    #[must_use]
    pub fn committed(&self, ticket: u64) -> Option<DynAction> {
        let inner = self.held();
        inner.handed.get(&ticket).cloned().or_else(|| {
            inner
                .committed
                .iter()
                .find(|entry| entry.ticket == ticket)
                .map(|entry| entry.action.clone())
        })
    }

    /// Forget the action committed under `ticket`: the door has answered for it.
    pub fn take_ticket(&self, ticket: u64) -> Option<DynAction> {
        let mut inner = self.changing();
        if let Some(action) = inner.handed.remove(&ticket) {
            return Some(action);
        }
        let at = inner
            .committed
            .iter()
            .position(|entry| entry.ticket == ticket)?;
        inner.committed.remove(at).map(|entry| entry.action)
    }

    /// The ticket the next committed action will get.
    #[must_use]
    pub fn next_ticket(&self) -> u64 {
        self.held().next_ticket
    }

    /// The store as it is now. A trial run (planning an action for the door) writes into the
    /// mirror like any run; [`Self::rewind`] takes those writes back, since only the door's
    /// commit makes a change.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        let mut inner = self.changing();
        inner.trials = inner.trials.saturating_add(1);
        Checkpoint {
            held: inner.held.len(),
            local: inner.local.len(),
            index: inner.index.clone(),
            notes: inner.notes.clone(),
            overlay: Arc::clone(&inner.overlay),
        }
    }

    /// Put the store back as the checkpoint held it. What the trial run wrote was told to
    /// nobody (not the views, not search or publication), so taking it back tells nobody
    /// either: planning an action, as publishing every item's actions does, changes nothing
    /// anyone follows.
    pub fn rewind(&self, checkpoint: Checkpoint) {
        let mut inner = self.mutate();
        let dropped = inner.local.len() > checkpoint.local;
        inner.drop_local_from(checkpoint.local);
        if dropped {
            inner.reapply();
        }
        inner.overlay = checkpoint.overlay;
        inner.index = checkpoint.index;
        inner.notes = checkpoint.notes;
        inner.changed();
        inner.held.truncate(checkpoint.held);
        inner.trials = inner.trials.saturating_sub(1);
        if inner.trials == 0 {
            // What arrived from elsewhere during the trial (nothing does today: a trial run is
            // one synchronous call) is told now.
            for summary in std::mem::take(&mut inner.held) {
                inner.notify(&summary);
            }
        }
    }

    /// Keep what was written since `checkpoint`, as a run that is not a trial keeps it: what it
    /// changed is told now, as every change is told.
    pub fn keep(&self, checkpoint: Checkpoint) {
        drop(checkpoint);
        let mut inner = self.mutate();
        inner.trials = inner.trials.saturating_sub(1);
        if inner.trials == 0 {
            for summary in std::mem::take(&mut inner.held) {
                inner.notify(&summary);
            }
        }
    }

    /// How many outside effects this device has yet to carry out.
    #[must_use]
    pub fn outbox_pending_count(&self) -> usize {
        Store::outbox_pending(self).len()
    }

    /// Take every action committed under a ticket of `first` or later, oldest first.
    #[must_use]
    pub fn take_from(&self, first: u64) -> Vec<Committed> {
        let mut inner = self.changing();
        let (mut taken, mut kept) = (Vec::new(), VecDeque::new());
        for entry in std::mem::take(&mut inner.committed) {
            if entry.ticket >= first {
                taken.push(entry);
            } else {
                kept.push_back(entry);
            }
        }
        inner.committed = kept;
        taken
    }

    /// A cursor over what changes in the store from now on, for a consumer that keeps
    /// something derived from many resources (publication) up to date by redoing only what
    /// changed.
    #[must_use]
    pub fn watch(&self) -> Watch {
        let changes = Rc::new(RefCell::new(Changes::default()));
        self.changing().watches.push(Rc::downgrade(&changes));
        Watch { changes }
    }

    /// The type of the resource `doc`.
    #[must_use]
    pub fn resource_type(&self, doc: &str) -> Option<TypeId> {
        self.held().index.get(doc).map(|(_, ty)| ty.clone())
    }

    /// The app and type of the resource `doc`, as the store's index has them now.
    #[must_use]
    pub fn resource_of(&self, doc: &str) -> Option<(AppId, TypeId)> {
        self.held().index.get(doc).cloned()
    }

    /// Whether the resource `doc` is one its origin derives: it lives in the store's unsaved
    /// layer, so a change of it is never written into the workspace record.
    #[must_use]
    pub fn derives(&self, doc: &str) -> bool {
        self.held()
            .index
            .get(doc)
            .and_then(|(_, ty)| lookup(ty))
            .is_some_and(crate::schema::TypeSchema::derived)
    }

    /// Whether the resource `doc` exists.
    #[must_use]
    pub fn contains(&self, doc: &str) -> bool {
        self.held().index.contains_key(doc)
    }

    /// Run what is committed from now on as the migration `migration` (its action id) of the
    /// resource `doc`, or, with `None`, as anything else.
    pub fn set_migrating(&self, migrating: Option<(String, DocId)>) {
        self.mutate().migrating = migrating;
    }

    /// Record in the change committed from now on, as the trigger `triggering` of `app`, what
    /// the trigger last did for its resource, or, with `None`, commit as not a trigger's run.
    pub fn set_triggering(&self, triggering: Option<Triggering>) {
        self.mutate().triggering = triggering;
    }

    /// Turn the plain text each record of `app` holds in a field its type now keeps as rich
    /// text into rich text with the same words, and write each as a change of the record
    /// (committed under `by`, the package version's migration principal). A record already
    /// holding rich text is not written, so a second run writes nothing. Returns how many
    /// records changed. The workspace's origin calls it once, as it opens (`Host::migrate`).
    pub fn upgrade_plain_text(&self, app: &str, module: &str, by: &str) -> usize {
        if self.load_record().is_err() {
            return 0;
        }
        let mut inner = self.mutate();
        inner.upgrade_plain_text(app, module, by)
    }

    /// Whether the resource `doc` holds anything in any of `fields`: what a migration reads to
    /// know its old field was ever written. Each field is read on its own, a collection by the
    /// placements of the record's items, so a record of ten thousand items is not built to ask
    /// whether one was written.
    #[must_use]
    pub fn holds_any(&self, doc: &str, fields: &[&str]) -> bool {
        if fields.is_empty() {
            return true;
        }
        if self.load_record().is_err() {
            return false;
        }
        let schema: &'static crate::schema::TypeSchema = {
            let inner = self.held();
            let Some((_, ty)) = inner.index.get(doc) else {
                return false;
            };
            let Some(schema) = lookup(ty) else {
                return false;
            };
            schema
        };
        let named: Vec<&'static crate::schema::FieldSchema> = schema
            .fields
            .iter()
            .filter(|field| fields.contains(&field.id.as_str()))
            .collect();
        if named.is_empty() {
            return false;
        }
        let inner = self.held();
        let Ok(mirror) = inner.held_doc(doc) else {
            return false;
        };
        let Some((app, _)) = inner.index.get(doc) else {
            return false;
        };
        let Some(root) = crate::reads::resource_obj(mirror, app, doc) else {
            return false;
        };
        let located = crate::reads::Located {
            obj: Some(root.clone()),
            schema,
            item: None,
            chunk: inner.chunks.chunk(doc),
        };
        named.iter().any(|field| {
            if field.kind == crate::schema::MergeKind::Collection {
                crate::reads::collection_holds(mirror, &root, field)
            } else {
                crate::reads::holds_something(mirror, &located, field)
            }
        })
    }

    /// What the trigger `key` last did for the resource `doc`: the period it ran in, or the
    /// instant it fired at. `None` when it never ran on the resource.
    #[must_use]
    pub fn triggered(&self, doc: &str, key: &str) -> Option<String> {
        if self.load_record().is_err() {
            return None;
        }
        let inner = self.held();
        let (app, _) = inner.index.get(doc)?;
        let mirror = inner.held_doc(doc).ok()?;
        let obj = crate::reads::resource_obj(mirror, app, doc)?;
        crate::action::triggered(mirror, &obj, key)
    }

    /// Record that the migration `migration` (its action id) ran on the resource `doc`, as a
    /// change of its own: the platform keeps no other mark of it, and runs a migration on each
    /// resource of its subject type once, whether an earlier version wrote the resource or
    /// this one. A migration reads what an older version left, so one that finds nothing of it
    /// changes nothing, and a record made by this version starts with every migration of its
    /// type run.
    pub fn mark_migrated(&self, doc: &str, migration: &str) -> Result<(), StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let Some((app, ty)) = inner.index.get(doc).cloned() else {
            return Err(not_found(format!("resource {doc}")));
        };
        let Some(schema) = lookup(&ty) else {
            return Err(not_found(format!("type {ty}")));
        };
        let edit = DynEdit::Migrated {
            id: doc.to_owned(),
            migration: migration.to_owned(),
        };
        let seed = inner.fresh("migrated:").into_bytes();
        inner.write(&app, vec![edit.clone()], &seed, &[], Moves::default())?;
        let action = DynAction {
            app: app.clone(),
            kind: format!("tl.{app}.{}", schema.name),
            action_id: migration.to_owned(),
            args: Json::String(doc.to_owned()),
            authorship: Authorship::default(),
            edits: vec![edit],
        };
        inner.enqueue(action);
        Ok(())
    }

    /// Whether the migration `migration` (its action id) ran on the resource `doc`.
    pub fn migrated(&self, doc: &str, migration: &str) -> Result<bool, StoreError> {
        self.load_record()?;
        let inner = self.held();
        let (app, _) = inner
            .index
            .get(doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        let mirror = inner.held_doc(doc)?;
        let obj = crate::reads::resource_obj(mirror, app, doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        Ok(crate::action::migrated(mirror, &obj, migration))
    }

    /// Mark the notice `doc` seen, as `person`'s change: what a device writes when its person
    /// dismisses or opens one, so every device of theirs takes the notification back. The
    /// change sets the notice's `seen` flag, as a bound control's edit of a field does, and
    /// waits for the host to commit it (`Host::take_committed`).
    pub fn see(&self, doc: &str, person: &str) -> Result<(), StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let Some((app, ty)) = inner.index.get(doc).cloned() else {
            return Err(not_found(format!("resource {doc}")));
        };
        if ty != tilolang::NOTICE_TYPE {
            return Err(invalid(format!("resource {doc} is no notice")));
        }
        let Some(schema) = lookup(&ty) else {
            return Err(not_found(format!("type {ty}")));
        };
        let at = ResourceRef::root(doc.to_owned(), ty.clone());
        let mutation = Mutation::SetRegister {
            field: tilolang::NOTICE_SEEN.to_owned(),
            value: Scalar::Bool(true),
        };
        let Planned {
            edits,
            touched,
            items,
            placed,
            ..
        } = inner.plan(&at, &mutation)?;
        let seed = inner.fresh("seen:").into_bytes();
        inner.write(&app, edits.clone(), &seed, &touched, (items, placed))?;
        let action = DynAction {
            app: app.clone(),
            kind: format!("tl.{app}.{}", schema.name),
            action_id: crate::notices::SEEN_ACTION.to_owned(),
            args: Json::Object(
                [
                    (
                        "field".to_owned(),
                        Json::String(tilolang::NOTICE_SEEN.to_owned()),
                    ),
                    ("value".to_owned(), Json::Bool(true)),
                ]
                .into_iter()
                .collect(),
            ),
            authorship: Authorship {
                invoker: person.to_owned(),
                ..Authorship::default()
            },
            edits,
        };
        inner.enqueue(action);
        Ok(())
    }

    /// The ids of the resources of the type `ty` of `app`, in id order.
    #[must_use]
    pub fn resources_of(&self, app: &str, ty: &str) -> Vec<DocId> {
        self.held()
            .index
            .iter()
            .filter(|(_, (a, t))| a == app && t == ty)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The record of the resource `doc`, as the runtime sees it now: read from the workspace
    /// record, which is had first if the store opened without it.
    pub fn record(&self, doc: &str) -> Result<DynRecord, StoreError> {
        self.load_doc(doc)?;
        let inner = self.held();
        let (app, _) = inner
            .index
            .get(doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        read_resource(&inner.chunks, app, doc).map_err(backend)
    }

    /// The store as it is now, to read on another thread: one for every reader until the
    /// store changes.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        let mut inner = self.changing();
        if let Some(taken) = inner.snapshot.upgrade() {
            return taken;
        }
        let clock = inner.clock.read();
        let taken = Arc::new(Snapshot {
            clock,
            index: inner.index.clone(),
            apps: inner
                .apps
                .iter()
                .filter(|(_, held)| held.ready())
                .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
                .collect(),
            overlay: Arc::clone(&inner.overlay),
            packages: inner.apps.keys().cloned().collect(),
            unshared: Arc::clone(&inner.unshared),
            schemas: Arc::clone(&inner.schemas),
        });
        inner.snapshot = Arc::downgrade(&taken);
        taken
    }

    /// Say a reference to a resource the reader does not hold as `unshared`: the platform's
    /// phrase in the reader's language ([`crate::reading`]).
    pub fn set_unshared(&self, unshared: &str) {
        let unshared = Arc::from(unshared);
        if *self.held().unshared != *unshared {
            self.mutate().unshared = unshared;
        }
    }

    /// The store, to change: a snapshot taken before no longer stands for it.
    fn mutate(&self) -> Changing<'_> {
        let mut inner = self.changing();
        inner.snapshot = std::sync::Weak::new();
        inner
    }

    /// This store's schema table, to install while reading, writing or deriving: a record's
    /// type id resolves to the schema of the package version this store holds.
    pub(crate) fn schemas(&self) -> Arc<crate::schema::Schemas> {
        self.inner.borrow().schemas.clone()
    }

    /// This store's schema table, installed on this thread for as long as the scope lives.
    fn scoped(&self) -> (Arc<crate::schema::Schemas>, crate::schema::Current) {
        let schemas = self.schemas();
        let scope = schemas.enter();
        (schemas, scope)
    }

    /// The store's schema table, installed while `work` runs: what a closure that resolves type
    /// ids by [`crate::schema::lookup`] (a model's `Store` hooks, an action's apply) reads.
    pub(crate) fn with_schemas<R>(&self, work: impl FnOnce() -> R) -> R {
        let (_schemas, _scope) = self.scoped();
        work()
    }

    /// The store's contents, with its schema table installed ([`Held::of`]).
    fn held(&self) -> Held<'_> {
        Held::of(&self.schemas(), &self.inner)
    }

    /// [`Self::held`], to change ([`Changing::of`]).
    fn changing(&self) -> Changing<'_> {
        Changing::of(&self.schemas(), &self.inner)
    }

    /// The record of the resource at `at` (a document or an item in one), as the runtime
    /// sees it now: an item is read without reading its siblings.
    pub fn record_at(&self, at: &ResourceRef) -> Result<DynRecord, StoreError> {
        self.load_at(at)?;
        let inner = self.held();
        let (app, ty) = inner.locate(at)?;
        let mirror = inner.held_doc(&at.doc)?;
        let located = locate_ref(mirror, Some(&inner.chunks), (&app, &ty), at, &inner.nested)?;
        let source = inner.chunks.mirror(located.chunk).unwrap_or(mirror);
        match located.obj {
            Some(obj) => <DynRecord as tilo_resource::Fields>::read_fields(source, &obj),
            // An item stored as its order key alone: every field reads as its default.
            None => DynRecord::empty(located.schema),
        }
        .map_err(backend)
    }

    /// The first text of the resource at `at`, in the order of [`DynRecord`]'s text collection,
    /// as the store's reader reads it ([`crate::reading`]), and the path of the resource that
    /// holds it: what names a resource.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<(String, String)> {
        let (value, held) = self.first_stored(at)?;
        Some((crate::reading::said(self, &at.id_string(), value)?, held))
    }

    /// The text value `value` of the resource `of` (its id) as the store's reader reads it
    /// ([`crate::reading::said`]): each reference in rich text by what its resource is called
    /// now when they hold it, as not shared when they do not.
    #[must_use]
    pub fn said(&self, of: &str, value: FieldValue) -> Option<String> {
        crate::reading::said(self, of, value)
    }

    /// [`Self::first_text`] as stored.
    fn first_stored(&self, at: &ResourceRef) -> Option<(FieldValue, String)> {
        // A row the index answers needs no chunk: a window names what it shows from the index,
        // and reads the record only for what the index does not hold.
        let indexed = {
            let inner = self.held();
            let (app, ty) = inner.locate(at).ok()?;
            let view = inner.view(&app)?;
            let schema = lookup(&at.ty).or_else(|| lookup(&ty))?;
            view.first_text(&at.id_string(), schema).ok().flatten()
        };
        if let Some(found) = indexed {
            return Some(found);
        }
        let _ = self.load_at(at);
        let inner = self.held();
        let (app, ty) = inner.locate(at).ok()?;
        if let Some(view) = inner.view(&app) {
            let schema = lookup(&at.ty).or_else(|| lookup(&ty))?;
            return view.first_text(&at.id_string(), schema).ok().flatten();
        }
        let mirror = inner.chunks.doc(&at.doc)?;
        let located =
            locate_ref(mirror, Some(&inner.chunks), (&app, &ty), at, &inner.nested).ok()?;
        let source = inner.chunks.mirror(located.chunk).unwrap_or(mirror);
        let (items, resolved) = inner.resolved(at).ok()?;
        let record = crate::reads::RecordItems {
            doc: &at.doc,
            items: &items,
            resolved: &resolved,
        };
        crate::reads::first_text(
            source,
            &located.obj?,
            located.schema,
            (&at.id_string(), &record),
        )
        .map(|(text, held)| (FieldValue::Text(text), held))
    }

    /// How many items of the collection `field` of `at` hold each value of their column
    /// `column` (a toggle's two, an enum's cases), read from the column as the index holds it:
    /// the counts a filter's chips show.
    pub fn facets(
        &self,
        at: &ResourceRef,
        field: &str,
        column: &str,
    ) -> Result<Vec<(String, usize)>, StoreError> {
        let inner = self.held();
        let (app, ty) = inner.locate(at)?;
        let schema = lookup(&at.ty)
            .or_else(|| lookup(&ty))
            .ok_or_else(|| not_found(ty.clone()))?;
        let item = schema
            .field(field)
            .and_then(|f| f.record.as_deref())
            .and_then(lookup)
            .ok_or_else(|| not_found(format!("collection {field}")))?;
        let view = inner
            .view(&app)
            .ok_or_else(|| StoreError::Backend(format!("the index of {app} is being built")))?;
        view.facets((&at.id_string(), field), item, column)
            .map_err(backend)
    }

    /// Where the item `item` is in the collection `field` of `at`, as its listing has it: found
    /// by its order key, not by walking the listing.
    #[must_use]
    pub fn position(&self, at: &ResourceRef, field: &str, item: &str) -> Option<usize> {
        let inner = self.held();
        let (app, _) = inner.locate(at).ok()?;
        let view = inner.view(&app)?;
        view.position(&at.id_string(), field, item).ok().flatten()
    }

    /// Create the resource `id` as one committed change: how a per-workspace singleton gets the same
    /// id on every replica. A resource its origin derives is made in the unsaved layer instead:
    /// its package's root, whose whole content the origin writes again at every start.
    ///
    /// # Errors
    /// The type is not registered, the resource already exists, or the record cannot be had.
    pub fn create_with_id(&self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        self.load_doc(id)?;
        let mut inner = self.mutate();
        if inner.index.contains_key(id) {
            return Err(invalid(format!("resource {id} exists")));
        }
        let schema = lookup(&record.ty)
            .ok_or_else(|| not_found(format!("type {} is not registered", record.ty)))?;
        let empty = DynRecord::empty(schema).map_err(backend)?;
        let edit = DynEdit::Create {
            id: id.to_owned(),
            record: empty.to_json(),
        };
        let touched = vec![FieldKey {
            resource: id.to_owned(),
            field: ANY_FIELD.to_owned(),
        }];
        inner
            .index
            .insert(id.to_owned(), (record.app.clone(), record.ty.clone()));
        let written = inner.write(
            &record.app,
            vec![edit.clone()],
            id.as_bytes(),
            &touched,
            (Vec::new(), Vec::new()),
        );
        if let Err(error) = written {
            inner.index.remove(id);
            return Err(error);
        }
        // What its origin derives is the store's unsaved layer: it is made there, read there,
        // and gone at the next start, when the origin writes it again.
        if !schema.derived() {
            let action = DynAction {
                app: record.app.clone(),
                kind: format!("tl.{}.{}", record.app, schema.name),
                action_id: crate::action::CREATE_ACTION.to_owned(),
                args: Json::String(id.to_owned()),
                authorship: Authorship::default(),
                edits: vec![edit],
            };
            inner.enqueue(action);
        }
        Ok(ResourceRef::root(id, record.ty.clone()))
    }

    /// Create the record `id` inside the open transaction `txn`: what makes it is the change of
    /// the action that did, committed with the rest of it and taken back when it fails. What
    /// creates it writes nothing of an app's own — `notify` writes a notice of the host's
    /// ([`crate::notify`]) — so the change keeps the app and kind of what else the action wrote.
    ///
    /// # Errors
    /// The type is not registered, the resource already exists, no transaction is open, or the
    /// record cannot be had.
    pub fn create_in(
        &self,
        txn: Txn,
        record: &NewRecord,
        id: &str,
    ) -> Result<ResourceRef, StoreError> {
        self.load_doc(id)?;
        let mut inner = self.mutate();
        if !inner.txns.contains_key(&txn.0) {
            return Err(invalid("no such transaction"));
        }
        if inner.index.contains_key(id) {
            return Err(invalid(format!("resource {id} exists")));
        }
        let schema = lookup(&record.ty)
            .ok_or_else(|| not_found(format!("type {} is not registered", record.ty)))?;
        let empty = DynRecord::empty(schema).map_err(backend)?;
        let edit = DynEdit::Create {
            id: id.to_owned(),
            record: empty.to_json(),
        };
        let touched = vec![FieldKey {
            resource: id.to_owned(),
            field: ANY_FIELD.to_owned(),
        }];
        inner
            .index
            .insert(id.to_owned(), (record.app.clone(), record.ty.clone()));
        let written = inner.write(
            &record.app,
            vec![edit.clone()],
            id.as_bytes(),
            &touched,
            (Vec::new(), Vec::new()),
        );
        if let Err(error) = written {
            inner.index.remove(id);
            return Err(error);
        }
        let state = inner
            .txns
            .get_mut(&txn.0)
            .ok_or_else(|| invalid("no such transaction"))?;
        if state.app.is_none() {
            state.app = Some(record.app.clone());
            state.kind = Some(format!("tl.{}.{}", record.app, schema.name));
        }
        state.saved = state.saved.saturating_add(1);
        state.edits.push(edit);
        state.created.push(id.to_owned());
        state
            .touched
            .entry(id.to_owned())
            .or_default()
            .extend(touched);
        Ok(ResourceRef::root(id, record.ty.clone()))
    }
}

/// The resources the documents the store holds have, by resource: those of each chunk.
fn scan_of(chunks: &Chunks) -> BTreeMap<DocId, (AppId, TypeId)> {
    let mut out = BTreeMap::new();
    for (_, doc) in chunks.docs() {
        for (resource, held) in scan(doc) {
            out.entry(resource).or_insert(held);
        }
    }
    out
}

/// Whether the stamps `from` reflect every chunk the store holds: what a change's versions and
/// placements take in, so an index stamped with them has nothing else to read.
fn reflects_held(chunks: &Chunks, from: &Stamps) -> bool {
    chunks
        .docs()
        .all(|(chunk, doc)| from.get(&chunk) == Some(&doc.committed_document().get_heads()))
}

/// The stamps `from` brought up to what the store holds now: the heads of each chunk it holds,
/// and the stamps of the chunks it was not sent, which its index still reflects.
fn stamps_after(chunks: &Chunks, from: &Stamps) -> Stamps {
    let mut now = from.clone();
    for (chunk, doc) in chunks.docs() {
        now.insert(chunk, doc.committed_document().get_heads());
    }
    now
}

/// The versions the changes of the chunks `moved` since their stamps `from` made, and where they
/// placed the items they placed: what an index stamped with those heads takes in, and what the
/// items that left a collection are written from when the index never held them. A change that
/// wrote nothing but where the record places its resources (`$chunks`) made no version of one
/// and placed no item, and its message is not read. Each chunk's changes are walked once for
/// both.
fn ends_of_chunks(
    chunks: &Chunks,
    from: &Stamps,
    moved: &[ChunkNo],
    docs: &BTreeMap<DocId, (AppId, TypeId)>,
) -> (Ends, idx::reflect::Placed) {
    let mut ends = Ends::new();
    let mut placed = idx::reflect::Placed::new();
    for chunk in moved {
        if let Some(doc) = chunks.mirror(*chunk) {
            let since = doc
                .committed_document()
                .get_changes(from.get(chunk).map_or(&[], Vec::as_slice));
            let naming = crate::reads::naming(&since, doc);
            ends.extend(idx::reflect::ends_of(naming.iter().copied(), doc, docs));
            placed.extend(idx::reflect::placed_by(naming));
        }
    }
    (ends, placed)
}

impl Inner {
    fn notify(&mut self, summary: &ChangeSummary) {
        if self.trials > 0 {
            self.held.push(summary.clone());
            return;
        }
        self.watched(|changes| changes.add(summary));
        for sub in self.subs.iter_mut().filter(|sub| sub.doc == summary.doc) {
            (sub.callback)(summary);
        }
    }

    fn watched(&mut self, mut f: impl FnMut(&mut Changes)) {
        self.watches.retain(|watch| match watch.upgrade() {
            Some(changes) => {
                f(&mut changes.borrow_mut());
                true
            }
            None => false,
        });
    }

    /// Tell every subscriber that anything in its document may have changed.
    fn notify_all(&mut self) {
        self.watched(|changes| changes.everything = true);
        let docs: BTreeSet<DocId> = self.subs.iter().map(|sub| sub.doc.clone()).collect();
        for doc in docs {
            self.notify(&ChangeSummary::whole(doc));
        }
    }

    /// What anything read before may no longer be.
    fn changed(&mut self) {
        self.snapshot = std::sync::Weak::new();
        self.listed.borrow_mut().clear();
    }

    /// [`Self::changed`], for a change that reached only `fields` (per record; an empty list
    /// for a record changed as a whole): the listings of collections it did not reach are
    /// still what they were, and are kept.
    fn changed_within(&mut self, fields: &BTreeMap<DocId, Vec<FieldKey>>, items: &ItemTouches) {
        self.snapshot = std::sync::Weak::new();
        self.listed.borrow_mut().retain_mut(|held| {
            let Some(keys) = fields.get(tilolang_runtime::store::doc_of(&held.resource)) else {
                return true;
            };
            if keys.is_empty() {
                return false;
            }
            let mut carry = held.carry.clone().unwrap_or_default();
            let mut reached = false;
            for key in keys {
                if !held.reached_by(key) {
                    continue;
                }
                reached = true;
                let Some(ids) = held.touched_by(key, items) else {
                    return false;
                };
                carry.extend(ids);
            }
            if reached {
                held.carry = Some(carry);
            }
            true
        });
    }

    /// The document the resource `id` is stored in, when the store holds its chunk: the record
    /// for a resource its manifest names no chunk for.
    fn held_doc(&self, id: &str) -> Result<&Doc, StoreError> {
        self.chunks
            .doc(id)
            .ok_or_else(|| StoreError::Backend(format!("the chunk holding {id} is not loaded")))
    }

    /// Derive what the record's last changes reached into every index read from, stamped
    /// with the heads the store saw. An index that fails is built anew.
    ///
    /// `since` is, when known, the heads the record moved on from and the versions its new
    /// changes made ([`idx::reflect::ends_of`]): what an index stamped with those heads takes
    /// in. Another one reads its versions from the changes since its own heads.
    ///
    /// Returns the collections items joined or left, with those items, and the records
    /// derived anew whole.
    fn derive_all(
        &mut self,
        reached: &Reached,
        since: Since<'_>,
    ) -> (ItemTouches, BTreeSet<DocId>, BTreeMap<DocId, Vec<FieldKey>>) {
        let mut moved = ItemTouches::new();
        let mut wholes = BTreeSet::new();
        let mut notified: BTreeMap<DocId, Vec<FieldKey>> = BTreeMap::new();
        if !self.chunks.has_record() {
            return (moved, wholes, notified);
        }
        let apps: Vec<AppId> = self
            .apps
            .iter()
            .filter(|(_, held)| held.ready())
            .map(|(app, _)| app.clone())
            .collect();
        for app in apps {
            let Some(held) = self.apps.get(&app) else {
                continue;
            };
            let index = Arc::clone(&held.index);
            let seen = stamps_after(&self.chunks, &held.heads);
            let own;
            let docs = &self.index;
            let own_placed;
            let (ends, placed) = match since {
                // The index reflects every chunk the store held before the change: what the
                // change made is all it has to take in.
                Some((from, ends, placed)) if reflects_held(&self.chunks, from) => (ends, placed),
                _ => {
                    // The versions of what changed since the index's heads, and where it put
                    // the items it placed: the chunks it does not reflect yet, and only those.
                    let moved: Vec<ChunkNo> = self
                        .chunks
                        .docs()
                        .filter(|(chunk, doc)| {
                            held.heads.get(chunk) != Some(&doc.committed_document().get_heads())
                        })
                        .map(|(chunk, _)| chunk)
                        .collect();
                    (own, own_placed) = ends_of_chunks(&self.chunks, &held.heads, &moved, docs);
                    (&own, &own_placed)
                }
            };
            let nested = &self.nested;
            let written = index.write(|writer| {
                let mut derive = Derive::new(&self.chunks, docs, nested, self.clock.read());
                derive.removed_before(writer, &app, ends, placed)?;
                derive.changes(writer, &app, reached)?;
                derive.versions(writer, &app, ends)?;
                derive.removed(writer, &app, ends)?;
                writer.set_heads(&seen)?;
                Ok((derive.reads, derive.moved, derive.wholes, derive.notified))
            });
            match written {
                Ok((reads, app_moved, app_wholes, app_notified)) => {
                    for (key, ids) in app_moved {
                        moved.entry(key).or_default().extend(ids);
                    }
                    wholes.extend(app_wholes);
                    for (doc, keys) in app_notified {
                        notified.entry(doc).or_default().extend(keys);
                    }
                    self.count(reads);
                    if let Some(held) = self.apps.get_mut(&app) {
                        held.heads = seen;
                        match held.index.snapshot() {
                            Ok(snapshot) if !snapshot.stats().is_corrupt() => {
                                held.snapshot = snapshot;
                            }
                            _ => self.rebuild(&app),
                        }
                    }
                }
                Err(_) => self.rebuild(&app),
            }
        }
        (moved, wholes, notified)
    }

    /// Write `edit` on the record `doc` in the transaction `txn`, adding it to `state`: what a
    /// migration or a trigger did to a resource is a change of the record like any other.
    /// Whether the record's type is known here, which a mark needs.
    fn mark(
        &mut self,
        state: &mut TxnState,
        txn: Txn,
        doc: &str,
        edit: DynEdit,
    ) -> Result<bool, StoreError> {
        let Some((app, ty)) = self.index.get(doc).cloned() else {
            return Ok(false);
        };
        let Some(schema) = lookup(&ty) else {
            return Ok(false);
        };
        state.app = Some(app.clone());
        state.kind = Some(format!("tl.{app}.{}", schema.name));
        let seed = format!("{}:{}", txn.0, self.counter).into_bytes();
        self.write(&app, vec![edit.clone()], &seed, &[], Moves::default())?;
        state.edits.push(edit);
        Ok(true)
    }

    /// Apply one edit to the mirror's open transaction and derive what it changed into the
    /// overlay. `touched` names what it changes, and `items` the collection items it adds,
    /// removes or moves. Returns the steps each collection's listing took.
    fn write(
        &mut self,
        app: &str,
        edits: Vec<DynEdit>,
        seed: &[u8],
        touched: &[FieldKey],
        (items, placed): Moves,
    ) -> Result<BTreeMap<DocId, Vec<ListingChange>>, StoreError> {
        let before = (!items.is_empty()).then(|| Arc::clone(&self.overlay));
        for edit in edits {
            self.write_one(app, edit, seed, touched, (items.clone(), placed.clone()))?;
        }
        let mut listings: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        if let (Some(before), Some(held)) = (before, self.apps.get(app).filter(|h| h.ready())) {
            let mut moved = ItemTouches::new();
            for (key, id) in &items {
                moved.entry(key.clone()).or_default().insert(id.clone());
            }
            let clock = self.clock.read();
            let old = View {
                snapshot: &held.snapshot,
                overlay: &before,
                clock,
            };
            let new = View {
                snapshot: &held.snapshot,
                overlay: &self.overlay,
                clock,
            };
            for (key, ids) in &moved {
                if let Ok(splices) = view::splices(&old, &new, key, ids) {
                    record_splices(
                        listings.entry(key.doc().to_owned()).or_default(),
                        key,
                        splices,
                    );
                }
            }
        }
        Ok(listings)
    }

    /// One of the edits of a write, into the chunk it is addressed to: the run's manifest into
    /// the record itself, and an item into the chunk the run it joins is in, so one write reaches
    /// one document.
    fn write_one(
        &mut self,
        app: &str,
        edit: DynEdit,
        seed: &[u8],
        touched: &[FieldKey],
        (items, placed): Moves,
    ) -> Result<(), StoreError> {
        let derived = unsaved::writes_derived(self, edit.resource(), touched, &items);
        let chunk = edit
            .chunk()
            .unwrap_or_else(|| self.chunks.chunk(edit.resource()));
        if chunk == chunk::FIRST && !self.chunks.has_record() {
            return Err(StoreError::Backend(
                "the workspace record is not loaded".into(),
            ));
        }
        // A chunk the manifest names and the store was not sent holds resources this write
        // cannot see ([`unloaded`]).
        if unloaded(self, chunk) {
            return Err(StoreError::Backend(format!(
                "the chunk holding what is in chunk {chunk} is not loaded"
            )));
        }
        // A run's manifest is written before what it places reads it: the next edit of the same
        // action sees the run the last one recorded.
        let manifest = matches!(edit.bare(), DynEdit::RunChunk { .. });
        let applied = apply_edits(
            self.chunks.open(chunk),
            app,
            std::slice::from_ref(&edit),
            seed,
        );
        if manifest {
            self.chunks.reread_placement();
        }
        let mut write = LocalWrite {
            app: app.to_owned(),
            chunk,
            edit,
            seed: seed.to_vec(),
            touched: touched.to_vec(),
            items,
            placed,
            versions: None,
            derived,
        };
        // Derived either way: a failed edit may have written part of itself. What the manifest
        // change places is derived with the edit that places it.
        if !manifest {
            self.derive_write(&mut write);
        }
        self.touch(&write);
        applied.map_err(|error| invalid(error.to_string()))?;
        self.local.push(write);
        Ok(())
    }

    /// Drop the open transaction of the chunk `chunk`'s mirror, if it has one.
    fn rollback(&mut self, chunk: ChunkNo) {
        if self
            .chunks
            .mirror(chunk)
            .is_some_and(|m| m.pending_ops() > 0)
            && let Some(mirror) = self.chunks.mirror_mut(chunk)
        {
            mirror.rollback();
        }
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.counter = self.counter.wrapping_add(1);
        format!("{prefix}{}-{}", self.instance, self.counter)
    }

    fn enqueue(&mut self, action: DynAction) {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.committed.push_back(Committed { ticket, action });
    }

    /// The type the index names the resource `at` to be, on the index's answer alone: the type
    /// of a document it lists, or of an item whose row it holds. `None` where the index names
    /// the resource nowhere, which is where the record is read for it.
    fn named_type(&self, at: &ResourceRef) -> Option<TypeId> {
        let (app, ty) = self.locate(at).ok()?;
        if at.item.is_none() {
            return Some(ty);
        }
        self.view(&app)?.type_of(&at.id_string()).ok().flatten()
    }

    fn locate(&self, at: &ResourceRef) -> Result<(AppId, TypeId), StoreError> {
        self.index
            .get(&at.doc)
            .cloned()
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))
    }

    /// The value of `field` of `at`, read from the chunk that holds it: for a package whose
    /// index is not read from yet, or a field no index holds.
    fn read_mirror(
        &self,
        (app, ty): (&AppId, &TypeId),
        at: &ResourceRef,
        field: &str,
    ) -> Result<FieldValue, StoreError> {
        let mirror = self.held_doc(&at.doc)?;
        let located = locate_ref(mirror, Some(&self.chunks), (app, ty), at, &self.nested)?;
        let source = self.chunks.mirror(located.chunk).unwrap_or(mirror);
        let spec = located
            .schema
            .field(field)
            .ok_or_else(|| not_found(format!("field {field}")))?;
        if spec.kind == MergeKind::Collection {
            let (_, resolved) = self.resolved(at)?;
            return Ok(FieldValue::Items(resolved.items(at.item.as_deref(), field)));
        }
        self.count(DocReads {
            located: 1,
            fields: 1,
            listings: 0,
            entries: 0,
        });
        read_field(source, &located, spec)
    }

    /// Count what was read of the record.
    fn count(&self, reads: DocReads) {
        let mut held = self.doc_reads.get();
        held.take_in(reads);
        self.doc_reads.set(held);
    }

    /// The listing (filtered by `pred`) of the collection `field` of `at`, read once in this
    /// state of the store.
    fn listed(
        &self,
        at: &ResourceRef,
        field: &str,
        pred: Option<(&TypeId, &Pred)>,
    ) -> Result<Rc<Listing>, StoreError> {
        self.listed_in(at, field, pred.map(|(item, p)| (item, Some(p))), None)
    }

    /// The first or last version the index keeps of the resource `at`; `None` when no index
    /// of its package is read from yet.
    fn version_end(
        &self,
        at: &ResourceRef,
        last: bool,
    ) -> Option<Result<Option<Version>, StoreError>> {
        let (app, _) = self.index.get(&at.doc)?;
        let view = self.view(app)?;
        let field = if last {
            idx::reflect::UPDATED
        } else {
            idx::reflect::CREATED
        };
        Some(
            view.field(&at.id_string(), field)
                .map_err(backend)
                .map(|held| {
                    held.as_ref()
                        .and_then(idx::reflect::Stamp::of_value)
                        .map(|stamp| stamp.version)
                }),
        )
    }

    /// The items of the collection `field` of `at` under `node` (above it, for `down` false)
    /// by the reference field `parent`, in the collection's order, each with the type it is:
    /// what [`View::relatives`] answers, for a collection of one record type. `None` when the
    /// index does not hold the collection's items, or holds them of several types: one column
    /// per type's field answers no walk over both.
    fn relatives(
        &self,
        at: &ResourceRef,
        field: &str,
        parent: &str,
        node: &ResourceRef,
        down: bool,
    ) -> Result<Option<Vec<(ItemId, TypeId)>>, StoreError> {
        let (app, _) = self.locate(at)?;
        let Some(view) = self.view(&app) else {
            return Ok(None);
        };
        let resource = at.id_string();
        let Some(ty) = view.type_of(&resource).map_err(backend)? else {
            return Ok(None);
        };
        let Some(schema) = lookup(&ty) else {
            return Ok(None);
        };
        let Some(spec) = schema
            .field(field)
            .filter(|f| f.kind == MergeKind::Collection)
        else {
            return Ok(None);
        };
        let Some(item) = spec.record.as_deref().and_then(lookup) else {
            return Ok(None);
        };
        let path = node.id_string();
        let Some(node) = crate::index::derive::id_of(&path) else {
            return Ok(None);
        };
        view.relatives((&resource, field), item, parent, node, down)
            .map_err(backend)
    }

    /// The listing of the collection `field` of `at` (of items of the type `item`), filtered
    /// by `pred` and in `order`, read once in this state of the store.
    fn listed_in(
        &self,
        at: &ResourceRef,
        field: &str,
        query: Option<(&TypeId, Option<&Pred>)>,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        let pred = query.and_then(|(_, p)| p);
        let resource = at.id_string();
        let same = |held: &ListedOnce| {
            held.resource == resource
                && held.field == field
                && held.pred.as_ref() == pred
                && held.order.as_ref() == order
        };
        // What it read before a change, and the items the change wrote: what it keeps of its
        // checks rather than checking the whole run again.
        let (before, carried) = {
            let listed = self.listed.borrow();
            match listed.iter().position(same) {
                Some(before) => match listed.get(before) {
                    Some(held) if held.carry.is_none() => {
                        return Ok(Rc::clone(&held.listing));
                    }
                    Some(held) => (Some(before), held.filtered.clone().zip(held.carry.clone())),
                    None => (Some(before), None),
                },
                None => (None, None),
            }
        };
        let carried = carried
            .as_ref()
            .map(|(before, touched)| Carried { before, touched });
        let (app, ty) = self.locate(at)?;
        let mut kept = None;
        let listing = match self.view(&app) {
            Some(view) => {
                let ty = view
                    .type_of(&resource)
                    .map_err(backend)?
                    .ok_or_else(|| not_found(format!("resource {resource}")))?;
                let schema = lookup(&ty).ok_or_else(|| not_found(format!("type {ty}")))?;
                let spec = schema
                    .field(field)
                    .filter(|f| f.kind == MergeKind::Collection)
                    .ok_or_else(|| not_found(format!("collection {field}")))?;
                match query {
                    None => view.listing(&resource, field).map_err(backend)?,
                    Some((item, pred)) => {
                        let item_schema = spec
                            .record
                            .as_deref()
                            .and_then(lookup)
                            .or_else(|| lookup(item))
                            .ok_or_else(|| not_found(format!("type {item}")))?;
                        let answer = view.query(
                            (&resource, field),
                            item_schema,
                            (pred, order),
                            (carried.as_ref(), &self.evaluated),
                        )?;
                        kept = answer.carry;
                        answer.listing
                    }
                }
            }
            None if self.chunks.has_record() => {
                // The package's index is being built: its lists show nothing until it is,
                // as the platform's loading state; the record says the resource is there.
                let mirror = self.held_doc(&at.doc)?;
                locate_ref(mirror, Some(&self.chunks), (&app, &ty), at, &self.nested)?;
                Listing::default()
            }
            None => Listing::default(),
        };
        let listing = Rc::new(listing);
        let held = ListedOnce {
            resource,
            field: field.to_owned(),
            pred: pred.cloned(),
            order: order.cloned(),
            listing: Rc::clone(&listing),
            filtered: kept,
            carry: None,
        };
        let mut listed = self.listed.borrow_mut();
        match before.and_then(|at| listed.get_mut(at)) {
            Some(entry) => *entry = held,
            None => listed.push(held),
        }
        Ok(listing)
    }
}

/// A key that is safe as the name of an entry in the record.
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '.'
            }
        })
        .collect()
}

/// The mark a change records that a migration ran on its resource, or what a trigger last did
/// for its own: a change is one or the other, never both.
fn mark_of(
    migration: Option<&(String, DocId)>,
    triggering: Option<&Triggering>,
) -> Option<(DocId, DynEdit)> {
    if let Some((migration, doc)) = migration {
        let id = doc.clone();
        return Some((
            doc.clone(),
            DynEdit::Migrated {
                id,
                migration: migration.clone(),
            },
        ));
    }
    triggering.map(|triggering| (triggering.doc.clone(), triggering.edit()))
}
