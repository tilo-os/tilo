//! [`TiloStore`]: the runtime's [`Store`] over the room record (implementation design 7.2).
//!
//! The runtime reads and mutates resources through the `Store` trait and expects its own commits
//! to be visible at once. The room record, though, belongs to the framework: every change to it
//! is a recorded action committed by the executor, so it carries the invoker, the group and the
//! encoded action, and replays on every peer. The store therefore writes to a **mirror**, a
//! replica of the room record, and reads from each package's **index** ([`crate::index`]):
//!
//! * a transaction applies each mutation to the mirror as it is made and records it as a
//!   [`DynEdit`]; what it changed is derived, field by field, into the index's **overlay** (the
//!   open writes as the index would hold them), so reads inside it see them;
//! * committing queues a [`DynAction`] for the host, which commits it through the executor
//!   ([`TiloStore::take_committed`]). The mirror keeps what the store wrote as its open
//!   transaction, never as a change of its own;
//! * when the room record moves on (the executor applied the queued action, or a peer's change
//!   arrived), [`TiloStore::sync_from`] drops the mirror's open transaction and the overlay,
//!   applies the record's new changes to the mirror, derives what they changed into each index
//!   (stamped with the record's heads) and tells each subscriber exactly which fields of which
//!   resources they changed, and how each collection's listing moved.
//!
//! Lists, filters and the fields rows show are read from the index, page by page, so opening a
//! window reads what it shows and not the record. The store may even open before the record is
//! loaded ([`TiloStore::detached`]): it reads indexes stamped with the heads they reflect, and
//! takes the record when it is had ([`TiloStore::sync_from`], or the loader the host gave it
//! for a write or a field no index holds), catching each index up from its heads.
//!
//! One transaction may be open at a time: rolling one back puts the mirror and the overlay back
//! as the store's earlier writes left them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{ChangeHash, ObjType, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Edit, EditOp, Id, Model, Segment};
use tilolang_runtime::Journal;
use tilolang_runtime::ids::{AppId, DocId, RunId, TypeId};
use tilolang_runtime::store::{
    ANY_FIELD, Applied, ChangeNote, ChangeSummary, FieldKey, FieldSpec, FieldValue, Listing,
    ListingChange, Mutation, NewRecord, OutboxEntry, Pred, Scalar, Splice, Store, StoreError,
    SubId, Txn,
};
use tilolang_runtime::value::ResourceRef;

use crate::action::{
    Authorship, DynAction, DynEdit, RECORDS_KEY, apply_edits, outbox_entries, read_resource,
};
use crate::background::{Pending, Poll, Waker};
use crate::index::derive::{Derive, DocReads, Reached};
use crate::index::overlay::{Overlay, OverlaySink};
use crate::index::view::{self, View};
use crate::index::{self as idx, Index, IndexError, IndexReads, Opened};
use crate::reads::{changed_by, default_value, locate_ref, read_field, type_at};
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
    /// How the collections it changed changed, in order.
    listings: Vec<ListingChange>,
}

/// Items of collections a change touched: per collection, the ids of the items added, removed
/// or moved. Each is moved in the collection's listing, which is not read again.
pub(crate) type ItemTouches = BTreeMap<FieldKey, BTreeSet<String>>;

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
    edit: DynEdit,
    seed: Vec<u8>,
    touched: Vec<FieldKey>,
    /// The collection items it added, removed or moved.
    items: Vec<(FieldKey, String)>,
}

impl LocalWrite {
    /// What it reached, as derivation takes it.
    fn reached(&self) -> Reached {
        let mut reached = Reached::default();
        for key in &self.touched {
            let fields = reached.docs.entry(key.doc().to_owned()).or_default();
            fields.push(key.clone());
        }
        for (key, id) in &self.items {
            reached
                .items
                .entry(key.clone())
                .or_default()
                .insert(id.clone());
        }
        reached
    }
}

/// What the store did, for tests and the benchmark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
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
}

/// Where a package's index is in being had.
enum IndexState {
    /// Read from: it reflects `heads`, with what the store wrote over it in the overlay.
    Ready,
    /// Being built from the record off the frame: reflecting the heads given, once done.
    Building(Pending<Result<(Vec<ChangeHash>, DocReads), IndexError>>),
    /// Nothing to read, and no record yet to build it from.
    Missing,
}

/// One package's index, as the store holds it.
struct AppIndex {
    index: Arc<Index>,
    /// The index as its last commit left it.
    snapshot: Arc<idx::Snapshot>,
    /// The record heads that commit reflects.
    heads: Vec<ChangeHash>,
    schema: String,
    state: IndexState,
}

impl AppIndex {
    fn ready(&self) -> bool {
        matches!(self.state, IndexState::Ready)
    }
}

/// A listing read once in this state of the store: until anything changes, a view's rows read
/// it again rather than the index.
struct ListedOnce {
    resource: String,
    field: String,
    pred: Option<Pred>,
    listing: Rc<Listing>,
}

/// Gives the store the room record when it opened without it: for a write, or a field no index
/// holds. `None` when the record cannot be had.
pub type Loader = Box<dyn FnMut() -> Option<Doc>>;

struct Inner {
    /// The record as the store last caught up with it, with the store's writes as its open
    /// transaction; `None` until it is had (see [`TiloStore::detached`]). Shared with the
    /// snapshots taken of it until it changes.
    mirror: Option<Arc<Doc>>,
    /// The heads of the room record the mirror holds every change of.
    seen: Vec<ChangeHash>,
    /// What the store wrote into the mirror since it last caught up with the record, oldest
    /// first: the mirror's open transaction.
    local: Vec<LocalWrite>,
    stats: StoreStats,
    /// Every resource of the room: its app and type.
    index: BTreeMap<DocId, (AppId, TypeId)>,
    /// Per package, its index.
    apps: BTreeMap<AppId, AppIndex>,
    /// The store's writes as the indexes would hold them.
    overlay: Arc<Overlay>,
    /// Where the indexes are kept: `None` keeps them in memory, built when a package loads.
    index_dir: Option<PathBuf>,
    loader: Option<Loader>,
    /// Called when an index built off the frame is ready to be taken in.
    wake: Option<Waker>,
    /// What the store read of the record so far: derivation, and reads no index answered.
    doc_reads: std::cell::Cell<DocReads>,
    /// Items filters were evaluated on one by one.
    evaluated: std::cell::Cell<u64>,
    listed: RefCell<Vec<ListedOnce>>,
    txns: HashMap<u64, TxnState>,
    next_txn: u64,
    subs: Vec<Subscription>,
    next_sub: u64,
    /// What each [`Watch`] has not taken yet.
    watches: Vec<std::rc::Weak<RefCell<Changes>>>,
    /// Trial runs under way (checkpoints not rewound yet).
    trials: usize,
    /// What changed during the trial runs under way, told to nobody: a trial run is taken back.
    held: Vec<ChangeSummary>,
    committed: VecDeque<Committed>,
    /// Handed to the host and not yet answered by the door.
    handed: BTreeMap<u64, DynAction>,
    next_ticket: u64,
    journals: BTreeMap<RunId, Journal>,
    notes: BTreeMap<DocId, Vec<ChangeNote>>,
    instance: String,
    /// The device that carries out the outside effects this store queues: they stay queued in
    /// the record, and this device alone settles them.
    device: String,
    counter: u64,
    /// The last snapshot taken, while the store has not changed since: what search and
    /// publication starting in one frame share.
    snapshot: std::sync::Weak<Snapshot>,
}

/// The store as it was when taken, to read on another thread: each package's index as its last
/// commit left it, with the store's writes over it. Taking one copies nothing of the room.
pub struct Snapshot {
    index: BTreeMap<DocId, (AppId, TypeId)>,
    apps: BTreeMap<AppId, Arc<idx::Snapshot>>,
    overlay: Arc<Overlay>,
}

impl Snapshot {
    /// The index of `app` as it was, read through the store's writes.
    #[must_use]
    pub fn view(&self, app: &str) -> Option<View<'_>> {
        Some(View {
            snapshot: self.apps.get(app)?,
            overlay: &self.overlay,
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
        let (app, ty) = self.index.get(&at.doc)?;
        let view = self.view(app)?;
        let schema = lookup(&at.ty).or_else(|| lookup(ty))?;
        view.first_text(&at.id_string(), schema).ok().flatten()
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
            if summary.listing(key).is_none() {
                self.unsaid.insert(key.clone());
            }
        }
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

/// The runtime's store over the room record. A cheap handle: clones share the mirror.
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

/// Every resource of `doc`, with its app and type id.
fn scan(doc: &Doc) -> BTreeMap<DocId, (AppId, TypeId)> {
    let mut out = BTreeMap::new();
    let Ok(Some((AmValue::Object(ObjType::Map), records))) = doc.get(&automerge::ROOT, RECORDS_KEY)
    else {
        return out;
    };
    for app in doc.keys(&records) {
        let Ok(Some((AmValue::Object(ObjType::Map), app_obj))) = doc.get(&records, app.as_str())
        else {
            continue;
        };
        for id in doc.keys(&app_obj) {
            let Ok(Some((AmValue::Object(ObjType::Map), obj))) = doc.get(&app_obj, id.as_str())
            else {
                continue;
            };
            if let Ok(Some((AmValue::Scalar(scalar), _))) = doc.get(&obj, TYPE_KEY)
                && let ScalarValue::Str(ty) = scalar.as_ref()
            {
                out.insert(id, (app.clone(), ty.to_string()));
            }
        }
    }
    out
}

fn json_of_scalar(scalar: &Scalar) -> Json {
    match scalar {
        Scalar::Bool(flag) => Json::Bool(*flag),
        Scalar::Int(value) => Json::from(*value),
        Scalar::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
        }
        Scalar::Text(text) => Json::String(text.clone()),
        Scalar::Json(value) => value.clone(),
    }
}

/// The path from a resource's root to the item `at` names, then `field`.
fn path_to(at: &ResourceRef, field: &str) -> Vec<Segment> {
    let mut path = Vec::new();
    for step in &at.path {
        path.push(Segment::Field(step.field.clone()));
        path.push(Segment::Item(Id::from_raw(step.item.clone())));
    }
    path.push(Segment::Field(field.to_owned()));
    path
}

/// Have the mirror's writes carry the actor of a change the mirror already holds.
///
/// Under an actor of its own, each of the mirror's transactions would add that actor to the
/// document and its rollback remove it again, and Automerge renumbers every op's actor column
/// for each: two passes over the whole document per keystroke. The mirror never commits (its
/// writes are taken back before any change of the record is applied, see
/// [`TiloStore::sync_from`]), so its ops under a borrowed actor never meet that actor's next
/// real change.
fn write_as_a_known_actor(mirror: &mut Doc) {
    let actor = mirror
        .committed_document()
        .get_heads()
        .first()
        .and_then(|head| mirror.committed_document().get_change_meta_by_hash(head))
        .map(|meta| meta.actor.into_owned());
    if let Some(actor) = actor {
        mirror.set_actor(actor);
    }
}

/// A copy of `record`'s committed changes, to write into.
fn mirror_of(record: &Doc) -> Doc {
    let mut mirror = record.clone();
    // Only the record's committed changes are the mirror's: an open transaction of the
    // record arrives as a change once the framework commits it.
    mirror.rollback();
    write_as_a_known_actor(&mut mirror);
    mirror
}

/// Whether `doc` holds every change of `heads`.
fn holds_heads(doc: &Doc, heads: &[ChangeHash]) -> bool {
    heads
        .iter()
        .all(|head| doc.committed_document().get_change_by_hash(head).is_some())
}

/// Every record of `app` in `docs`, reached as a whole, with those of `before` that are gone:
/// what derives a package's index anew where it stands.
fn everything(
    before: &BTreeMap<DocId, (AppId, TypeId)>,
    now: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Reached {
    let mut reached = Reached::default();
    for doc in before.keys().chain(now.keys()) {
        reached.docs.insert(doc.clone(), Vec::new());
    }
    reached
}

/// Build the index of `app` from `doc` whole, stamped `heads`.
fn build(
    index: &Index,
    doc: &Doc,
    docs: &BTreeMap<DocId, (AppId, TypeId)>,
    app: &str,
    heads: &[ChangeHash],
) -> Result<DocReads, IndexError> {
    index.write(|writer| {
        let mut derive = Derive::new(doc, docs);
        derive.all(writer, app)?;
        writer.set_heads(heads)?;
        Ok(derive.reads)
    })
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
        {
            let mut inner = store.inner.borrow_mut();
            let mirror = mirror_of(record);
            inner.seen = record.committed_document().get_heads();
            inner.index = scan(&mirror);
            inner.mirror = Some(Arc::new(mirror));
        }
        store
    }

    /// A store without the record yet, reading the indexes kept in `dir`: what opens a room
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
                mirror: None,
                seen: Vec::new(),
                local: Vec::new(),
                stats: StoreStats::default(),
                index: BTreeMap::new(),
                apps: BTreeMap::new(),
                overlay: Arc::default(),
                index_dir,
                loader: None,
                wake: None,
                doc_reads: std::cell::Cell::default(),
                evaluated: std::cell::Cell::new(0),
                listed: RefCell::default(),
                txns: HashMap::new(),
                next_txn: 1,
                subs: Vec::new(),
                next_sub: 1,
                watches: Vec::new(),
                trials: 0,
                held: Vec::new(),
                committed: VecDeque::new(),
                handed: BTreeMap::new(),
                next_ticket: 1,
                journals: BTreeMap::new(),
                notes: BTreeMap::new(),
                device: instance.clone(),
                instance,
                counter: 0,
                snapshot: std::sync::Weak::new(),
            })),
        }
    }

    /// Keep the indexes of packages loaded from now on in `dir`, rather than in memory: a
    /// window's room, opened again later, reads them instead of building them.
    pub fn set_index_dir(&self, dir: PathBuf) {
        self.mutate().index_dir = Some(dir);
    }

    /// Set the device whose outside effects this store queues.
    pub fn set_device(&self, device: &str) {
        device.clone_into(&mut self.mutate().device);
    }

    /// How the store gets the room record when it opened without it.
    pub fn set_loader(&self, loader: Loader) {
        self.mutate().loader = Some(loader);
    }

    /// Have `wake` called (from another thread) when an index built off the frame is ready
    /// for [`Self::poll_index`] to take in.
    pub fn set_waker(&self, wake: Waker) {
        self.mutate().wake = Some(wake);
    }

    /// Open (or build) the index of the package `app`, whose record schemas are `schema` (as
    /// [`crate::meta::MetaExt::schema_text`] writes them). An index of another schema is built
    /// anew; one already open with this schema is kept.
    pub fn open_index(&self, app: &str, schema: &str) {
        let mut inner = self.mutate();
        if inner
            .apps
            .get(app)
            .is_some_and(|held| held.schema == schema)
        {
            return;
        }
        inner.apps.remove(app);
        let opened = match inner.index_dir.as_deref() {
            Some(dir) => Index::open(&idx::file_of(dir, app), app, schema),
            None => Index::in_memory(app, schema).map(Opened::Empty),
        };
        // A file that cannot be had (another window holds it) leaves an index in memory.
        let opened = opened.or_else(|_| Index::in_memory(app, schema).map(Opened::Empty));
        let Ok(opened) = opened else {
            return;
        };
        let (index, heads) = match opened {
            Opened::Ready(index, heads) => (index, Some(heads)),
            Opened::Empty(index) => (index, None),
        };
        let Ok(snapshot) = index.snapshot() else {
            return;
        };
        let held = AppIndex {
            index: Arc::new(index),
            snapshot,
            heads: heads.clone().unwrap_or_default(),
            schema: schema.to_owned(),
            state: if heads.is_some() {
                IndexState::Ready
            } else {
                IndexState::Missing
            },
        };
        inner.apps.insert(app.to_owned(), held);
        if inner.mirror.is_some() {
            inner.bring_up(app);
        } else {
            inner.index_docs();
        }
        inner.changed();
        drop(inner);
        self.inner.borrow_mut().notify_all();
    }

    /// Whether every package's index is read from: none is being built, or waits for the
    /// record. Until then its lists show nothing, as the platform's loading state.
    #[must_use]
    pub fn index_ready(&self) -> bool {
        self.inner.borrow().apps.values().all(AppIndex::ready)
    }

    /// Take in the indexes built off the frame since the last call. Returns whether one was:
    /// what every view reads may have changed.
    pub fn poll_index(&self) -> bool {
        self.take_in_builds(false)
    }

    /// [`Self::poll_index`], waiting for the indexes being built: for a host without frames.
    pub fn wait_index(&self) -> bool {
        self.take_in_builds(true)
    }

    fn take_in_builds(&self, wait: bool) -> bool {
        let mut inner = self.mutate();
        let apps: Vec<AppId> = inner.apps.keys().cloned().collect();
        let mut any = false;
        // An index a read found corrupt is not read again: it is built anew from the record.
        let broken: Vec<AppId> = inner
            .apps
            .iter()
            .filter(|(_, held)| held.ready() && held.index.stats().is_corrupt())
            .map(|(app, _)| app.clone())
            .collect();
        for app in broken {
            any = true;
            inner.reopen_broken(&app);
        }
        for app in apps {
            let Some(held) = inner.apps.get_mut(&app) else {
                continue;
            };
            let IndexState::Building(pending) = &held.state else {
                continue;
            };
            let poll = if wait { pending.wait() } else { pending.poll() };
            let built = match poll {
                Poll::Running => continue,
                Poll::Ready(built) => built,
                Poll::Lost => Err(IndexError::Io("the build ended without a result".into())),
            };
            any = true;
            if let Ok((heads, reads)) = built {
                inner.count(reads);
                inner.stats.builds = inner.stats.builds.saturating_add(1);
                if let Some(held) = inner.apps.get_mut(&app) {
                    held.state = IndexState::Ready;
                    held.heads = heads;
                    if let Ok(snapshot) = held.index.snapshot() {
                        held.snapshot = snapshot;
                    }
                }
                // What the record gained while it was built.
                inner.catch_up(&app);
            } else {
                // Built in memory instead, on the frame: an index is owed either way.
                let schema = inner
                    .apps
                    .get(&app)
                    .map(|held| held.schema.clone())
                    .unwrap_or_default();
                if let Ok(index) = Index::in_memory(&app, &schema)
                    && let Ok(snapshot) = index.snapshot()
                {
                    inner.apps.insert(
                        app.clone(),
                        AppIndex {
                            index: Arc::new(index),
                            snapshot,
                            heads: Vec::new(),
                            schema,
                            state: IndexState::Missing,
                        },
                    );
                    inner.build_now(&app);
                }
            }
        }
        if any {
            inner.rebuild_overlay();
            inner.changed();
            inner.notify_all();
        }
        any
    }

    /// Everything the index of `app` holds now (the store's open writes apart): what tests
    /// compare with an index built from the record whole.
    #[must_use]
    pub fn index_contents(&self, app: &str) -> Option<idx::Contents> {
        let inner = self.inner.borrow();
        let held = inner.apps.get(app).filter(|held| held.ready())?;
        held.snapshot.contents().ok()
    }

    /// Wait for the commits to the indexes so far to reach the disk: when a window closes.
    pub fn persist_index(&self) {
        for held in self.inner.borrow().apps.values() {
            if held.index.path().is_some() {
                let _ = held.index.persist();
            }
        }
    }

    /// Bring the store up to date with the room record: when the record's heads moved, the
    /// store's own writes are dropped from the mirror and the overlay (the record holds them
    /// now, or will), the record's new changes are applied to the mirror and derived into each
    /// index, and each subscriber is told which fields of which resources changed. A store
    /// without the record takes it, and catches each index up from its heads. Returns whether
    /// the record had moved.
    pub fn sync_from(&self, record: &Doc) -> bool {
        let mut inner = self.mutate();
        if inner.mirror.is_none() {
            inner.attach(record);
            drop(inner);
            self.inner.borrow_mut().notify_all();
            return true;
        }
        let heads = record.committed_document().get_heads();
        if inner.seen == heads {
            return false;
        }
        let old_overlay = std::mem::take(&mut inner.overlay);
        let old_snapshots: BTreeMap<AppId, Arc<idx::Snapshot>> = inner
            .apps
            .iter()
            .filter(|(_, held)| held.ready())
            .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
            .collect();
        // The store's own writes leave the mirror: the items they added, removed or moved are
        // moved again below, as the record has them.
        let mut items = inner.drop_local_from(0);
        inner.rollback();
        let before = inner.index.clone();
        let caught_up = inner
            .mirror
            .as_ref()
            .is_some_and(|mirror| mirror.committed_document().get_heads() == inner.seen);
        let changed = if caught_up {
            let changes = record.committed_document().get_changes(&inner.seen);
            let applied = inner.mirror_mut().map(|mirror| {
                let applied = mirror.apply_incoming(changes.iter().cloned());
                write_as_a_known_actor(mirror);
                applied
            });
            match (applied, inner.mirror.as_deref()) {
                (Some(Ok(())), Some(mirror)) => Some(changed_by(&changes, mirror, &before)),
                _ => None,
            }
        } else {
            None
        };
        inner.seen = heads;
        let Some(mut changed) = changed else {
            // The mirror held a change of its own, which only a copy of the record removes.
            inner.stats.full_syncs = inner.stats.full_syncs.saturating_add(1);
            inner.mirror = Some(Arc::new(mirror_of(record)));
            inner.index = scan_of(inner.mirror.as_deref());
            let reached = everything(&before, &inner.index);
            inner.derive_all(&reached);
            inner.changed();
            inner.notify_all();
            return true;
        };
        inner.stats.incremental_syncs = inner.stats.incremental_syncs.saturating_add(1);
        if changed.structure || changed.unplaced {
            inner.index = scan_of(inner.mirror.as_deref());
        }
        for (key, ids) in std::mem::take(&mut changed.items) {
            items
                .entry(key.clone())
                .or_default()
                .extend(ids.iter().cloned());
            changed.items.insert(key, ids);
        }
        let reached = if changed.unplaced || changed.layout {
            everything(&before, &inner.index)
        } else {
            Reached {
                docs: changed.docs.clone(),
                items: changed.items.clone(),
            }
        };
        inner.derive_all(&reached);
        inner.changed();
        let mut listings = inner.splices_since(&old_snapshots, &old_overlay, &items);
        if changed.unplaced {
            inner.notify_all();
        } else {
            for (doc, fields) in changed.docs {
                let listings = listings.remove(&doc).unwrap_or_default();
                inner.notify(&ChangeSummary {
                    doc,
                    fields,
                    listings,
                });
            }
        }
        true
    }

    /// Whether the mirror holds exactly what `record` holds: the record it last brought itself
    /// up to date with, not one before or after it. A record of the past (a version an agent
    /// reads) is not it, even when it was handed to [`Self::sync_from`], which only moves forward.
    #[must_use]
    pub fn is_at(&self, record: &Doc) -> bool {
        self.inner.borrow().mirror.as_ref().is_some_and(|mirror| {
            mirror.committed_document().get_heads() == record.committed_document().get_heads()
        })
    }

    /// Whether the store holds the room record (else it reads indexes alone).
    #[must_use]
    pub fn has_record(&self) -> bool {
        self.inner.borrow().mirror.is_some()
    }

    /// What the store did so far.
    #[must_use]
    pub fn stats(&self) -> StoreStats {
        self.inner.borrow().stats
    }

    /// How much the store read so far.
    #[must_use]
    pub fn read_stats(&self) -> ReadStats {
        let inner = self.inner.borrow();
        let index = inner
            .apps
            .values()
            .map(|held| held.index.stats().reads())
            .fold(IndexReads::default(), IndexReads::plus);
        ReadStats {
            doc: inner.doc_reads.get(),
            index,
            evaluated: inner.evaluated.get(),
        }
    }

    /// The actions the runtime committed since the host last took them, oldest first. They stay
    /// available by ticket ([`Self::committed`]) until the door has answered ([`Self::take_ticket`]).
    #[must_use]
    pub fn take_committed(&self) -> Vec<Committed> {
        let mut inner = self.inner.borrow_mut();
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
        let mut inner = self.inner.borrow_mut();
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

    /// The action committed under `ticket`, while it is queued or handed over.
    #[must_use]
    pub fn committed(&self, ticket: u64) -> Option<DynAction> {
        let inner = self.inner.borrow();
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
        let mut inner = self.inner.borrow_mut();
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
        self.inner.borrow().next_ticket
    }

    /// The store as it is now. A trial run (planning an action for the door) writes into the
    /// mirror like any run; [`Self::rewind`] takes those writes back, since only the door's
    /// commit makes a change.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        let mut inner = self.inner.borrow_mut();
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

    /// How many outside effects this device has yet to carry out.
    #[must_use]
    pub fn outbox_pending_count(&self) -> usize {
        Store::outbox_pending(self).len()
    }

    /// Take every action committed under a ticket of `first` or later, oldest first.
    #[must_use]
    pub fn take_from(&self, first: u64) -> Vec<Committed> {
        let mut inner = self.inner.borrow_mut();
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
        self.inner
            .borrow_mut()
            .watches
            .push(Rc::downgrade(&changes));
        Watch { changes }
    }

    /// The type of the resource `doc`.
    #[must_use]
    pub fn resource_type(&self, doc: &str) -> Option<TypeId> {
        self.inner.borrow().index.get(doc).map(|(_, ty)| ty.clone())
    }

    /// Whether the resource `doc` exists.
    #[must_use]
    pub fn contains(&self, doc: &str) -> bool {
        self.inner.borrow().index.contains_key(doc)
    }

    /// The ids of the resources of the type `ty` of `app`, in id order.
    #[must_use]
    pub fn resources_of(&self, app: &str, ty: &str) -> Vec<DocId> {
        self.inner
            .borrow()
            .index
            .iter()
            .filter(|(_, (a, t))| a == app && t == ty)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The record of the resource `doc`, as the runtime sees it now: read from the room
    /// record, which is had first if the store opened without it.
    ///
    /// # Errors
    /// The resource does not exist, its type is not registered, or the record cannot be had.
    pub fn record(&self, doc: &str) -> Result<DynRecord, StoreError> {
        self.load_record()?;
        let inner = self.inner.borrow();
        let (app, _) = inner
            .index
            .get(doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        let mirror = inner.mirror()?;
        read_resource(mirror, app, doc).map_err(backend)
    }

    /// The store as it is now, to read on another thread: one for every reader until the
    /// store changes.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        let mut inner = self.inner.borrow_mut();
        if let Some(taken) = inner.snapshot.upgrade() {
            return taken;
        }
        let taken = Arc::new(Snapshot {
            index: inner.index.clone(),
            apps: inner
                .apps
                .iter()
                .filter(|(_, held)| held.ready())
                .map(|(app, held)| (app.clone(), Arc::clone(&held.snapshot)))
                .collect(),
            overlay: Arc::clone(&inner.overlay),
        });
        inner.snapshot = Arc::downgrade(&taken);
        taken
    }

    /// The store, to change: a snapshot taken before no longer stands for it.
    fn mutate(&self) -> std::cell::RefMut<'_, Inner> {
        let mut inner = self.inner.borrow_mut();
        inner.snapshot = std::sync::Weak::new();
        inner
    }

    /// Have the room record, if the store opened without it: from the loader.
    fn load_record(&self) -> Result<(), StoreError> {
        let loader = {
            let mut inner = self.inner.borrow_mut();
            if inner.mirror.is_some() {
                return Ok(());
            }
            inner.loader.take()
        };
        let Some(mut loader) = loader else {
            return Err(StoreError::Backend("the room record is not loaded".into()));
        };
        let record = loader();
        let mut inner = self.mutate();
        inner.loader = Some(loader);
        let Some(record) = record else {
            return Err(StoreError::Backend(
                "the room record cannot be loaded".into(),
            ));
        };
        inner.attach(&record);
        drop(inner);
        self.inner.borrow_mut().notify_all();
        Ok(())
    }

    /// The record of the resource at `at` (a document or an item in one), as the runtime
    /// sees it now: an item is read without reading its siblings.
    ///
    /// # Errors
    /// The resource does not exist, its type is not registered, or the record cannot be had.
    pub fn record_at(&self, at: &ResourceRef) -> Result<DynRecord, StoreError> {
        self.load_record()?;
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        let mirror = inner.mirror()?;
        let located = locate_ref(mirror, (&app, &ty), at)?;
        let obj = located
            .obj
            .ok_or_else(|| not_found(format!("resource {}", at.id_string())))?;
        <DynRecord as tilo_resource::Fields>::read_fields(mirror, &obj).map_err(backend)
    }

    /// The first text of the resource at `at`, in the order of [`DynRecord`]'s text collection,
    /// and the path of the resource that holds it: what names a resource.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<(String, String)> {
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at).ok()?;
        if let Some(view) = inner.view(&app) {
            let schema = lookup(&at.ty).or_else(|| lookup(&ty))?;
            return view.first_text(&at.id_string(), schema).ok().flatten();
        }
        let mirror = inner.mirror.as_deref()?;
        let located = locate_ref(mirror, (&app, &ty), at).ok()?;
        crate::reads::first_text(mirror, &located.obj?, located.schema, &at.id_string())
    }

    /// Where the item `item` is in the collection `field` of `at`, as its listing has it: found
    /// by its order key, not by walking the listing.
    #[must_use]
    pub fn position(&self, at: &ResourceRef, field: &str, item: &str) -> Option<usize> {
        let inner = self.inner.borrow();
        let (app, _) = inner.locate(at).ok()?;
        let view = inner.view(&app)?;
        view.position(&at.id_string(), field, item).ok().flatten()
    }

    /// Create the resource `id` as one committed change: how a per-room singleton gets the same
    /// id on every replica.
    ///
    /// # Errors
    /// The type is not registered, the resource already exists, or the record cannot be had.
    pub fn create_with_id(&self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        self.load_record()?;
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
            edit.clone(),
            id.as_bytes().to_vec(),
            touched,
            Vec::new(),
        );
        if let Err(error) = written {
            inner.index.remove(id);
            return Err(error);
        }
        let action = DynAction {
            app: record.app.clone(),
            kind: format!("tl.{}.{}", record.app, schema.name),
            action_id: crate::action::CREATE_ACTION.to_owned(),
            args: Json::String(id.to_owned()),
            authorship: Authorship::default(),
            edits: vec![edit],
        };
        inner.enqueue(action);
        Ok(ResourceRef::root(id, record.ty.clone()))
    }
}

/// The resources of the record `mirror` holds, if it is had.
fn scan_of(mirror: Option<&Doc>) -> BTreeMap<DocId, (AppId, TypeId)> {
    mirror.map(scan).unwrap_or_default()
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

    fn mirror(&self) -> Result<&Doc, StoreError> {
        self.mirror
            .as_deref()
            .ok_or_else(|| StoreError::Backend("the room record is not loaded".into()))
    }

    /// The index of `app` as the store's writes leave it, when it is read from.
    fn view(&self, app: &str) -> Option<View<'_>> {
        let held = self.apps.get(app).filter(|held| held.ready())?;
        Some(View {
            snapshot: &held.snapshot,
            overlay: &self.overlay,
        })
    }

    /// The records every index read from holds: what a store without the record knows of
    /// the room.
    fn index_docs(&mut self) {
        let mut index = BTreeMap::new();
        for (app, held) in self.apps.iter().filter(|(_, held)| held.ready()) {
            for (doc, ty) in held.snapshot.docs().unwrap_or_default() {
                index.insert(doc, (app.clone(), ty));
            }
        }
        self.index = index;
    }

    /// Take the room record: the mirror is a copy of it, and each index catches up with it.
    fn attach(&mut self, record: &Doc) {
        let mirror = mirror_of(record);
        self.seen = record.committed_document().get_heads();
        self.index = scan(&mirror);
        self.mirror = Some(Arc::new(mirror));
        let apps: Vec<AppId> = self.apps.keys().cloned().collect();
        for app in apps {
            self.bring_up(&app);
        }
        self.rebuild_overlay();
        self.changed();
    }

    /// Have the index of `app` reflect the mirror: caught up from its heads when the mirror
    /// holds them, else built anew (off the frame when it is kept on disk).
    fn bring_up(&mut self, app: &str) {
        let Some(held) = self.apps.get(app) else {
            return;
        };
        match held.state {
            IndexState::Building(_) => {}
            IndexState::Ready => {
                let holds = self
                    .mirror
                    .as_deref()
                    .is_some_and(|mirror| holds_heads(mirror, &held.heads));
                if holds {
                    self.catch_up(app);
                } else {
                    self.rebuild(app);
                }
            }
            IndexState::Missing => self.rebuild(app),
        }
    }

    /// Put a fresh index in place of the broken one of `app`, built from the record (once it
    /// is had).
    fn reopen_broken(&mut self, app: &str) {
        let Some(held) = self.apps.remove(app) else {
            return;
        };
        let path = held.index.path().map(Path::to_path_buf);
        let schema = held.schema.clone();
        drop(held);
        let fresh = path
            .as_deref()
            .and_then(|path| {
                let _ = std::fs::remove_file(path);
                Index::open(path, app, &schema).ok()
            })
            .map(|opened| match opened {
                Opened::Ready(index, _) | Opened::Empty(index) => index,
            })
            .map_or_else(|| Index::in_memory(app, &schema), Ok);
        let Ok(index) = fresh else {
            return;
        };
        let Ok(snapshot) = index.snapshot() else {
            return;
        };
        self.apps.insert(
            app.to_owned(),
            AppIndex {
                index: Arc::new(index),
                snapshot,
                heads: Vec::new(),
                schema,
                state: IndexState::Missing,
            },
        );
        self.rebuild(app);
        if self.mirror.is_none() {
            self.index_docs();
        }
    }

    /// Build the index of `app` anew from the mirror.
    fn rebuild(&mut self, app: &str) {
        let Some(mirror) = self.mirror.clone() else {
            if let Some(held) = self.apps.get_mut(app) {
                held.state = IndexState::Missing;
            }
            return;
        };
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let on_disk = held.index.path().is_some() && held.index.clear_for_build().is_ok();
        if !on_disk {
            self.build_now(app);
            return;
        }
        let index = Arc::clone(&held.index);
        let docs = self.index.clone();
        let heads = mirror.committed_document().get_heads();
        let app_id = app.to_owned();
        let pending = Pending::start(
            move || {
                let reads = build(&index, &mirror, &docs, &app_id, &heads)?;
                Ok((heads, reads))
            },
            self.wake.clone(),
        );
        if let Some(held) = self.apps.get_mut(app) {
            held.state = IndexState::Building(pending);
        }
    }

    /// Build the index of `app` from the mirror, here: one kept in memory.
    fn build_now(&mut self, app: &str) {
        let Some(mirror) = self.mirror.clone() else {
            return;
        };
        let heads = mirror.committed_document().get_heads();
        let Some(index) = self.apps.get(app).map(|held| Arc::clone(&held.index)) else {
            return;
        };
        let built = index
            .clear_for_build()
            .and_then(|()| build(&index, &mirror, &self.index, app, &heads));
        if let Ok(reads) = built {
            self.count(reads);
            self.stats.builds = self.stats.builds.saturating_add(1);
        }
        let snapshot = index.snapshot();
        let Some(held) = self.apps.get_mut(app) else {
            return;
        };
        match (built, snapshot) {
            (Ok(_), Ok(snapshot)) => {
                held.state = IndexState::Ready;
                held.heads = heads;
                held.snapshot = snapshot;
            }
            _ => held.state = IndexState::Missing,
        }
    }

    /// Apply to the index of `app` what the record gained since the heads it reflects.
    fn catch_up(&mut self, app: &str) {
        let Some(mirror) = self.mirror.clone() else {
            return;
        };
        let Some(held) = self.apps.get(app) else {
            return;
        };
        let now = mirror.committed_document().get_heads();
        if held.heads == now {
            return;
        }
        if !holds_heads(&mirror, &held.heads) {
            self.rebuild(app);
            return;
        }
        // Named against the records the index held: a record made since is derived whole.
        let mut before: BTreeMap<DocId, (AppId, TypeId)> = BTreeMap::new();
        for (doc, ty) in held.snapshot.docs().unwrap_or_default() {
            before.insert(doc, (app.to_owned(), ty));
        }
        let since = mirror.committed_document().get_changes(&held.heads);
        let changed = changed_by(&since, &mirror, &before);
        let reached = if changed.unplaced || changed.layout {
            everything(&before, &self.index)
        } else {
            Reached {
                docs: changed.docs,
                items: changed.items,
            }
        };
        let index = Arc::clone(&held.index);
        let docs = self.index.clone();
        let written = index.write(|writer| {
            let mut derive = Derive::new(&mirror, &docs);
            derive.changes(writer, app, &reached)?;
            writer.set_heads(&now)?;
            Ok(derive.reads)
        });
        match written {
            Ok(reads) => {
                self.count(reads);
                self.stats.catch_ups = self.stats.catch_ups.saturating_add(1);
                if let Some(held) = self.apps.get_mut(app) {
                    held.heads = now;
                    if let Ok(snapshot) = held.index.snapshot() {
                        held.snapshot = snapshot;
                    }
                }
            }
            Err(_) => self.rebuild(app),
        }
    }

    /// Derive what the record's last changes reached into every index read from, stamped
    /// with the heads the store saw. An index that fails is built anew.
    fn derive_all(&mut self, reached: &Reached) {
        let Some(mirror) = self.mirror.clone() else {
            return;
        };
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
            let seen = self.seen.clone();
            let written = index.write(|writer| {
                let mut derive = Derive::new(&mirror, &self.index);
                derive.changes(writer, &app, reached)?;
                writer.set_heads(&seen)?;
                Ok(derive.reads)
            });
            match written {
                Ok(reads) => {
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
    }

    /// The steps each listed collection took from the indexes `before` (with the overlay
    /// `overlay` over them) to the indexes now, for the items `items`; per document.
    fn splices_since(
        &self,
        before: &BTreeMap<AppId, Arc<idx::Snapshot>>,
        overlay: &Overlay,
        items: &ItemTouches,
    ) -> BTreeMap<DocId, Vec<ListingChange>> {
        let mut out: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        for (key, ids) in items {
            let Some((app, _)) = self.index.get(key.doc()) else {
                continue;
            };
            let (Some(old), Some(new)) = (before.get(app), self.view(app)) else {
                continue;
            };
            let old = View {
                snapshot: old,
                overlay,
            };
            if let Ok(splices) = view::splices(&old, &new, key, ids) {
                record_splices(out.entry(key.doc().to_owned()).or_default(), key, splices);
            }
        }
        out
    }

    /// Derive the store's writes anew into an empty overlay: after a rollback, or once an
    /// index they were written over changed.
    fn rebuild_overlay(&mut self) {
        let mut overlay = Overlay::default();
        if let Some(mirror) = self.mirror.clone() {
            for write in &self.local {
                let Some(held) = self.apps.get(&write.app).filter(|held| held.ready()) else {
                    continue;
                };
                let mut sink = OverlaySink {
                    overlay: &mut overlay,
                    snapshot: Some(&held.snapshot),
                };
                let mut derive = Derive::new(&mirror, &self.index);
                let _ = derive.changes(&mut sink, &write.app, &write.reached());
            }
        }
        self.overlay = Arc::new(overlay);
    }

    /// Apply one edit to the mirror's open transaction and derive what it changed into the
    /// overlay. `touched` names what it changes, and `items` the collection items it adds,
    /// removes or moves. Returns the steps each collection's listing took.
    fn write(
        &mut self,
        app: &str,
        edit: DynEdit,
        seed: Vec<u8>,
        touched: Vec<FieldKey>,
        items: Vec<(FieldKey, String)>,
    ) -> Result<BTreeMap<DocId, Vec<ListingChange>>, StoreError> {
        let applied = {
            let mirror = self
                .mirror_mut()
                .ok_or_else(|| StoreError::Backend("the room record is not loaded".into()))?;
            apply_edits(mirror, app, std::slice::from_ref(&edit), &seed)
        };
        let write = LocalWrite {
            app: app.to_owned(),
            edit,
            seed,
            touched,
            items,
        };
        let before = (!write.items.is_empty()).then(|| Arc::clone(&self.overlay));
        // Derived either way: a failed edit may have written part of itself.
        self.derive_write(&write);
        self.changed();
        let mut listings: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        if let (Some(before), Some(held)) = (before, self.apps.get(app).filter(|h| h.ready())) {
            let mut moved = ItemTouches::new();
            for (key, id) in &write.items {
                moved.entry(key.clone()).or_default().insert(id.clone());
            }
            let old = View {
                snapshot: &held.snapshot,
                overlay: &before,
            };
            let new = View {
                snapshot: &held.snapshot,
                overlay: &self.overlay,
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
        applied.map_err(|error| invalid(error.to_string()))?;
        self.local.push(write);
        Ok(listings)
    }

    /// Derive one write into the overlay.
    fn derive_write(&mut self, write: &LocalWrite) {
        let Some(mirror) = self.mirror.clone() else {
            return;
        };
        let Some(held) = self.apps.get(&write.app).filter(|held| held.ready()) else {
            return;
        };
        let snapshot = Arc::clone(&held.snapshot);
        let overlay = Arc::make_mut(&mut self.overlay);
        let mut sink = OverlaySink {
            overlay,
            snapshot: Some(&snapshot),
        };
        let mut derive = Derive::new(&mirror, &self.index);
        let _ = derive.changes(&mut sink, &write.app, &write.reached());
    }

    /// Drop the store's writes from the `start`th on (the caller applies the rest again).
    /// Returns the collection items they added, removed or moved.
    fn drop_local_from(&mut self, start: usize) -> ItemTouches {
        let start = start.min(self.local.len());
        let mut items = ItemTouches::new();
        for write in self.local.drain(start..) {
            for (key, id) in write.items {
                items.entry(key).or_default().insert(id);
            }
        }
        items
    }

    /// Drop the mirror's open transaction and apply the store's committed writes again: the
    /// mirror as those writes left it, without whatever was written after them.
    fn reapply(&mut self) {
        self.rollback();
        let local = self.local.clone();
        if let Some(mirror) = self.mirror_mut() {
            for write in &local {
                // Each applied once already, on this same base.
                let _ = apply_edits(
                    mirror,
                    &write.app,
                    std::slice::from_ref(&write.edit),
                    &write.seed,
                );
            }
        }
    }

    /// The mirror, to change: a copy of it when a snapshot still reads it.
    fn mirror_mut(&mut self) -> Option<&mut Doc> {
        self.mirror.as_mut().map(Arc::make_mut)
    }

    /// Drop the mirror's open transaction, if it has one.
    fn rollback(&mut self) {
        if self.mirror.as_ref().is_some_and(|m| m.pending_ops() > 0)
            && let Some(mirror) = self.mirror_mut()
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

    fn locate(&self, at: &ResourceRef) -> Result<(AppId, TypeId), StoreError> {
        self.index
            .get(&at.doc)
            .cloned()
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))
    }

    /// The value of `field` of `at`, read from the mirror: for a package whose index is not
    /// read from yet, or a field no index holds.
    fn read_mirror(
        &self,
        (app, ty): (&AppId, &TypeId),
        at: &ResourceRef,
        field: &str,
    ) -> Result<FieldValue, StoreError> {
        let mirror = self.mirror()?;
        let located = locate_ref(mirror, (app, ty), at)?;
        let spec = located
            .schema
            .field(field)
            .ok_or_else(|| not_found(format!("field {field}")))?;
        self.count(DocReads {
            located: 1,
            fields: 1,
            listings: u64::from(spec.kind == MergeKind::Collection),
            entries: 0,
        });
        read_field(mirror, &located, spec)
    }

    /// Count what was read of the record.
    fn count(&self, reads: DocReads) {
        let mut held = self.doc_reads.get();
        held.take_in(reads);
        self.doc_reads.set(held);
    }

    /// How many items the collection `field` of `at` holds now.
    fn collection_len(&self, at: &ResourceRef, field: &str) -> Result<usize, StoreError> {
        let (app, ty) = self.locate(at)?;
        if self.view(&app).is_some() {
            return Ok(self.listed(at, field, None)?.len());
        }
        let mirror = self.mirror()?;
        let located = locate_ref(mirror, (&app, &ty), at)?;
        Ok(crate::reads::collection_len(mirror, &located, field))
    }

    /// The listing (filtered by `pred`) of the collection `field` of `at`, read once in this
    /// state of the store.
    fn listed(
        &self,
        at: &ResourceRef,
        field: &str,
        pred: Option<(&TypeId, &Pred)>,
    ) -> Result<Rc<Listing>, StoreError> {
        let resource = at.id_string();
        if let Some(held) = self.listed.borrow().iter().find(|held| {
            held.resource == resource
                && held.field == field
                && held.pred.as_ref() == pred.map(|(_, p)| p)
        }) {
            return Ok(Rc::clone(&held.listing));
        }
        let (app, ty) = self.locate(at)?;
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
                match pred {
                    None => view.listing(&resource, field).map_err(backend)?,
                    Some((item, pred)) => {
                        let item_schema = spec
                            .record
                            .as_deref()
                            .and_then(lookup)
                            .or_else(|| lookup(item))
                            .ok_or_else(|| not_found(format!("type {item}")))?;
                        view.filtered((&resource, field), item_schema, pred, &self.evaluated)?
                    }
                }
            }
            None if self.mirror.is_some() => {
                // The package's index is being built: its lists show nothing until it is,
                // as the platform's loading state; the record says the resource is there.
                let mirror = self.mirror()?;
                locate_ref(mirror, (&app, &ty), at)?;
                Listing::default()
            }
            None => Listing::default(),
        };
        let listing = Rc::new(listing);
        self.listed.borrow_mut().push(ListedOnce {
            resource,
            field: field.to_owned(),
            pred: pred.map(|(_, p)| p.clone()),
            listing: Rc::clone(&listing),
        });
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

/// The field a mutation writes.
fn field_of(m: &Mutation) -> String {
    match m {
        Mutation::SetRegister { field, .. }
        | Mutation::AddCounter { field, .. }
        | Mutation::SetText { field, .. }
        | Mutation::CollInsert { field, .. }
        | Mutation::CollRemove { field, .. }
        | Mutation::CollMove { field, .. }
        | Mutation::SetAdd { field, .. }
        | Mutation::SetRemove { field, .. }
        | Mutation::MapPut { field, .. }
        | Mutation::MapRemove { field, .. } => field.clone(),
        Mutation::OutboxPush { .. } => String::new(),
    }
}

/// The edit a mutation makes below the resource `at`, and the id of the item it inserts.
fn edit_of(
    at: &ResourceRef,
    m: &Mutation,
    new_id: &str,
    length: usize,
) -> Result<(Edit<DynRecord>, Option<String>), StoreError> {
    let (field, op, item) = match m {
        Mutation::SetRegister { field, value } => (
            field,
            EditOp::Set {
                value: json_of_scalar(value),
            },
            None,
        ),
        Mutation::AddCounter { field, delta } => (field, EditOp::Add { by: *delta }, None),
        Mutation::SetText { field, text } => (field, EditOp::Replace { text: text.clone() }, None),
        Mutation::CollInsert { field, item, index } => {
            let schema = lookup(&item.ty)
                .ok_or_else(|| not_found(format!("type {} is not registered", item.ty)))?;
            let empty = DynRecord::empty(schema).map_err(backend)?;
            (
                field,
                EditOp::InsertItem {
                    id: Some(Id::from_raw(new_id)),
                    at: index.unwrap_or(length),
                    value: empty.to_json(),
                },
                Some(new_id.to_owned()),
            )
        }
        Mutation::CollRemove { field, item } => (
            field,
            EditOp::RemoveItem {
                id: Id::from_raw(item.clone()),
            },
            None,
        ),
        Mutation::CollMove { field, item, index } => (
            field,
            EditOp::MoveTo {
                id: Id::from_raw(item.clone()),
                at: *index,
            },
            None,
        ),
        Mutation::SetAdd { field, key } => (
            field,
            EditOp::Insert {
                value: Json::String(key.clone()),
            },
            None,
        ),
        Mutation::SetRemove { field, key } => (
            field,
            EditOp::Remove {
                value: Json::String(key.clone()),
            },
            None,
        ),
        Mutation::MapPut { field, key, value } => {
            let mut path = path_to(at, field);
            path.push(Segment::Key(key.clone()));
            let op = EditOp::Set {
                value: json_of_scalar(value),
            };
            return Ok((Edit::new(path, op), None));
        }
        Mutation::MapRemove { field, key } => (field, EditOp::RemoveKey { key: key.clone() }, None),
        Mutation::OutboxPush { .. } => {
            return Err(invalid(
                "outside effects are not stored in the room record yet",
            ));
        }
    };
    Ok((Edit::new(path_to(at, field), op), item))
}

impl Store for TiloStore {
    fn fresh_id(&mut self, prefix: &str) -> String {
        self.inner.borrow_mut().fresh(prefix)
    }

    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError> {
        let id = self.inner.borrow_mut().fresh("r");
        self.create_with_id(record, &id)
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        if let Some(view) = inner.view(&app) {
            return view
                .type_of(&at.id_string())
                .map_err(backend)?
                .ok_or_else(|| not_found(format!("resource {}", at.id_string())));
        }
        let mirror = inner.mirror()?;
        let located = locate_ref(mirror, (&app, &ty), at)?;
        Ok(located
            .obj
            .as_ref()
            .and_then(|obj| type_at(mirror, obj))
            .unwrap_or_else(|| located.schema.id.clone()))
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let id = inner.next_txn;
        inner.next_txn = inner.next_txn.wrapping_add(1);
        let start = inner.local.len();
        inner.txns.insert(
            id,
            TxnState {
                start,
                ..TxnState::default()
            },
        );
        Ok(Txn(id))
    }

    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let length = match m {
            Mutation::CollInsert { field, .. } => self.inner.borrow().collection_len(at, field)?,
            _ => 0,
        };
        let mut inner = self.mutate();
        if !inner.txns.contains_key(&txn.0) {
            return Err(invalid("no such transaction"));
        }
        let (app, ty) = inner.locate(at)?;
        let (dyn_edit, item, field) = if let Mutation::OutboxPush {
            capability,
            payload,
            key,
        } = m
        {
            let entry = key
                .as_deref()
                .map_or_else(|| inner.fresh("o"), sanitize_key);
            let edit = DynEdit::OutboxPush {
                id: at.doc.clone(),
                entry,
                capability: capability.clone(),
                payload: payload.clone(),
                owner: inner.device.clone(),
            };
            (edit, None, None)
        } else {
            let new_id = inner.fresh("i");
            let (edit, item) = edit_of(at, m, &new_id, length)?;
            let edit = DynEdit::Edit {
                id: at.doc.clone(),
                edit,
            };
            (edit, item, Some(field_of(m)))
        };
        let seed = format!("{}:{}", txn.0, inner.counter).into_bytes();
        // A collection's items are moved in its listing; any other field is derived again.
        let moved = match m {
            Mutation::CollInsert { field, .. } => item.clone().map(|id| (field, id)),
            Mutation::CollRemove { field, item } | Mutation::CollMove { field, item, .. } => {
                Some((field, item.clone()))
            }
            _ => None,
        };
        let items: Vec<(FieldKey, String)> = moved
            .iter()
            .map(|(field, id)| (FieldKey::new(at, (*field).clone()), id.clone()))
            .collect();
        let mut touched: Vec<FieldKey> = field
            .iter()
            .filter(|_| items.is_empty())
            .map(|field| FieldKey::new(at, field.clone()))
            .collect();
        if let Mutation::CollRemove { field, item } = m {
            // The item is gone: whatever was read of it.
            touched.push(FieldKey::new(
                &at.item(field, item, String::new()),
                ANY_FIELD,
            ));
        }
        let listings = inner.write(&app, dyn_edit.clone(), seed, touched, items)?;
        let kind = lookup(&ty).map(|schema| format!("tl.{app}.{}", schema.name));
        let state = inner
            .txns
            .get_mut(&txn.0)
            .ok_or_else(|| invalid("no such transaction"))?;
        if state.app.as_deref().is_some_and(|held| held != app) {
            return Err(invalid("a transaction cannot write resources of two apps"));
        }
        state.app = Some(app);
        if state.kind.is_none() {
            state.kind = kind;
        }
        state.edits.push(dyn_edit);
        for change in listings.into_values().flatten() {
            record_splices(&mut state.listings, &change.key, change.splices);
        }
        let touched = state.touched.entry(at.doc.clone()).or_default();
        if let Some(field) = field {
            touched.insert(FieldKey::new(at, field));
        }
        Ok(item.map_or(Applied::Done, Applied::Item))
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.mutate();
        let state = inner
            .txns
            .remove(&txn.0)
            .ok_or_else(|| invalid("no such transaction"))?;
        let (Some(app), Some(kind)) = (state.app, state.kind) else {
            return Ok(());
        };
        if state.edits.is_empty() {
            return Ok(());
        }
        let action = DynAction {
            app,
            kind,
            action_id: note.action.clone(),
            args: note.args.clone(),
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
            },
            edits: state.edits,
        };
        inner.enqueue(action);
        let mut listings = state.listings;
        for (doc, fields) in state.touched {
            inner
                .notes
                .entry(doc.clone())
                .or_default()
                .push(note.clone());
            // An outside effect queued alone changes no field a view reads.
            if fields.is_empty() {
                continue;
            }
            let (own, rest): (Vec<ListingChange>, Vec<ListingChange>) = listings
                .into_iter()
                .partition(|change| change.key.doc() == doc);
            listings = rest;
            inner.notify(&ChangeSummary {
                doc,
                fields: fields.into_iter().collect(),
                listings: own,
            });
        }
        Ok(())
    }

    fn rollback(&mut self, txn: Txn) {
        let mut inner = self.mutate();
        if let Some(state) = inner.txns.remove(&txn.0) {
            inner.drop_local_from(state.start);
            inner.reapply();
            inner.rebuild_overlay();
            inner.changed();
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let needs_record = {
            let inner = self.inner.borrow();
            let (app, ty) = inner.locate(at)?;
            if let Some(view) = inner.view(&app) {
                let resource = at.id_string();
                let stored = view
                    .type_of(&resource)
                    .map_err(backend)?
                    .ok_or_else(|| not_found(format!("resource {resource}")))?;
                let schema = lookup(&stored).ok_or_else(|| not_found(format!("type {stored}")))?;
                let spec = schema
                    .field(&field.id)
                    .ok_or_else(|| not_found(format!("field {}", field.id)))?;
                match spec.kind {
                    MergeKind::Collection => {
                        return Ok(FieldValue::Items(inner.listed(at, &field.id, None)?.ids()));
                    }
                    // Not a field the index holds: read from the record.
                    MergeKind::Record => true,
                    _ => match view.field(&resource, &field.id) {
                        Ok(value) => return Ok(value.unwrap_or_else(|| default_value(spec))),
                        // Never read from a corrupt index: the record says, until the index
                        // is built anew.
                        Err(_) if inner.mirror.is_some() => {
                            return inner.read_mirror((&app, &ty), at, &field.id);
                        }
                        Err(error) => return Err(backend(error)),
                    },
                }
            } else if inner.mirror.is_some() {
                return inner.read_mirror((&app, &ty), at, &field.id);
            } else {
                true
            }
        };
        if needs_record {
            self.load_record()?;
        }
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        inner.read_mirror((&app, &ty), at, &field.id)
    }

    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        self.inner.borrow().listed(at, &field.id, None)
    }

    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        self.inner
            .borrow()
            .listed(at, &field.id, Some((item, pred)))
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        let inner = self.inner.borrow();
        if !inner.index.contains_key(doc) {
            return Err(not_found(format!("resource {doc}")));
        }
        Ok(inner.notes.get(doc).cloned().unwrap_or_default())
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId {
        let mut inner = self.inner.borrow_mut();
        let id = SubId(inner.next_sub);
        inner.next_sub = inner.next_sub.wrapping_add(1);
        inner.subs.push(Subscription {
            id,
            doc: doc.clone(),
            callback: f,
        });
        id
    }

    fn unsubscribe(&mut self, sub: SubId) {
        self.inner.borrow_mut().subs.retain(|held| held.id != sub);
    }

    fn outbox_pending(&self) -> Vec<OutboxEntry> {
        let inner = self.inner.borrow();
        // A store without the record has queued nothing yet; what the record holds is carried
        // out once it is had.
        let Some(mirror) = inner.mirror.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (doc, (app, _)) in &inner.index {
            for held in outbox_entries(mirror, app, doc) {
                if held.settled.is_none() && held.owner == inner.device {
                    out.push(OutboxEntry {
                        doc: doc.clone(),
                        id: held.entry,
                        capability: held.capability,
                        payload: held.payload,
                    });
                }
            }
        }
        out
    }

    fn outbox_settle(&mut self, entry: &OutboxEntry, note: &ChangeNote) -> Result<(), StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let (app, ty) = inner
            .index
            .get(&entry.doc)
            .cloned()
            .ok_or_else(|| not_found(format!("resource {}", entry.doc)))?;
        let edit = DynEdit::OutboxSettle {
            id: entry.doc.clone(),
            entry: entry.id.clone(),
            outcome: "done".to_owned(),
        };
        let seed = format!("settle:{}:{}", entry.doc, entry.id).into_bytes();
        inner.write(&app, edit.clone(), seed, Vec::new(), Vec::new())?;
        let kind =
            lookup(&ty).map_or_else(String::new, |schema| format!("tl.{app}.{}", schema.name));
        inner.enqueue(DynAction {
            app,
            kind,
            action_id: note.action.clone(),
            args: note.args.clone(),
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
            },
            edits: vec![edit],
        });
        Ok(())
    }

    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError> {
        self.inner
            .borrow_mut()
            .journals
            .insert(run.clone(), journal.clone());
        Ok(())
    }

    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError> {
        Ok(self.inner.borrow().journals.get(run).cloned())
    }
}
