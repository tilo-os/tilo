//! [`TiloStore`]: the runtime's [`Store`] over the room record (implementation design 7.2).
//!
//! The runtime reads and mutates resources through the `Store` trait and expects its own commits
//! to be visible at once. The room record, though, belongs to the framework: every change to it
//! is a recorded action committed by the executor, so it carries the invoker, the group and the
//! encoded action, and replays on every peer. The store therefore works on a **mirror**, a
//! replica of the room record:
//!
//! * a transaction applies each mutation to the mirror as it is made (so reads inside it see
//!   them) and records it as a [`DynEdit`];
//! * committing queues a [`DynAction`] for the host, which commits it through the executor
//!   ([`TiloStore::take_committed`]). The mirror keeps what the store wrote as its open
//!   transaction, never as a change of its own;
//! * when the room record moves on (the executor applied the queued action, or a peer's change
//!   arrived), [`TiloStore::sync_from`] drops the mirror's open transaction, applies the record's
//!   new changes to the mirror and tells each subscriber exactly which fields of which resources
//!   they changed. The queued actions are gone from the mirror and present in the record.
//!
//! Nothing here costs in proportion to the size of the record: a read walks to one field, a sync
//! applies only the new changes, and the changed fields come from Automerge's patches.
//!
//! One transaction may be open at a time: rolling one back puts the mirror back as the
//! store's earlier writes left it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
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
use crate::cache::ReadCache;
use crate::reads::{changed_by, first_text, locate_ref, type_at};
use crate::record::DynRecord;
use crate::schema::{TYPE_KEY, lookup};

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
/// or moved. The store's read cache moves each in the collection's listing
/// ([`ReadCache::reconcile`]) instead of listing the collection again.
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

/// What the store did, for tests and the benchmark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Syncs that applied the record's new changes to the mirror.
    pub incremental_syncs: u64,
    /// Syncs that had to copy the whole record: the mirror held a change of its own.
    pub full_syncs: u64,
}

struct Inner {
    /// Shared with the snapshots taken of it until it changes: a change while one is read
    /// copies it then ([`Inner::mirror_mut`]).
    mirror: Arc<Doc>,
    /// The heads of the room record the mirror holds every change of.
    seen: Vec<ChangeHash>,
    /// What the store wrote into the mirror since it last caught up with the record, oldest
    /// first: the mirror's open transaction.
    local: Vec<LocalWrite>,
    stats: StoreStats,
    /// Every resource of the mirror: its app and type.
    index: BTreeMap<DocId, (AppId, TypeId)>,
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
    /// The last snapshot taken, while the mirror has not changed since: what search and
    /// publication starting in one frame share rather than copying the mirror twice.
    snapshot: std::sync::Weak<Snapshot>,
    /// What reads found in the mirror, until a change touches it.
    cache: RefCell<ReadCache>,
}

/// The mirror as it was when taken, to read on another thread: what search and publication read
/// a whole room from without holding up a frame. Taking one shares the mirror; the store copies
/// it only when it changes while a snapshot is still read. Reading it costs what reading the
/// store does.
pub struct Snapshot {
    mirror: Arc<Doc>,
    index: BTreeMap<DocId, (AppId, TypeId)>,
}

impl Snapshot {
    /// The record of the resource `doc`, as [`TiloStore::record`] read it when the snapshot was
    /// taken.
    ///
    /// # Errors
    /// The resource does not exist or its type is not registered.
    pub fn record(&self, doc: &str) -> Result<DynRecord, StoreError> {
        let (app, _) = self
            .index
            .get(doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        read_resource(&self.mirror, app, doc).map_err(backend)
    }

    /// [`TiloStore::first_text`], as it was when the snapshot was taken.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<(String, String)> {
        let (app, ty) = self.index.get(&at.doc)?;
        let located = locate_ref(&self.mirror, (app, ty), at).ok()?;
        let obj = located.obj?;
        first_text(&self.mirror, &obj, located.schema, &at.id_string())
    }
}

/// What changed in the mirror since a [`Watch`] last took it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Per document, the fields that changed (see [`FieldKey`]).
    pub docs: BTreeMap<DocId, BTreeSet<FieldKey>>,
    /// The documents that may have changed anywhere.
    pub whole: BTreeSet<DocId>,
    /// Anything anywhere may have changed: the mirror was copied from the record anew.
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

/// A cursor over the mirror's changes; see [`TiloStore::watch`].
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

/// The mirror as it was, to put back after a trial run; see [`TiloStore::checkpoint`].
pub struct Checkpoint {
    /// How many changes were held from the trial runs under way.
    held: usize,
    /// How many of the store's writes the mirror held.
    local: usize,
    index: BTreeMap<DocId, (AppId, TypeId)>,
    notes: BTreeMap<DocId, Vec<ChangeNote>>,
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

impl TiloStore {
    /// A store whose mirror starts as `record`, whose outside effects `device` carries out.
    #[must_use]
    pub fn on_device(record: &Doc, device: &str) -> Self {
        let store = Self::new(record);
        device.clone_into(&mut store.mutate().device);
        store
    }

    /// A store whose mirror starts as `record`.
    #[must_use]
    pub fn new(record: &Doc) -> Self {
        let mut mirror = record.clone();
        // Only the record's committed changes are the mirror's: an open transaction of the
        // record arrives as a change once the framework commits it.
        mirror.rollback();
        write_as_a_known_actor(&mut mirror);
        let instance: String = automerge::ActorId::random()
            .to_hex_string()
            .chars()
            .take(8)
            .collect();
        let seen = record.committed_document().get_heads();
        let index = scan(&mirror);
        Self {
            inner: Rc::new(RefCell::new(Inner {
                mirror: Arc::new(mirror),
                seen,
                local: Vec::new(),
                stats: StoreStats::default(),
                index,
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
                cache: RefCell::default(),
            })),
        }
    }

    /// Bring the mirror up to date with the room record: when the record's heads moved, the
    /// store's own writes are dropped from the mirror (the record holds them now, or will), the
    /// record's new changes are applied to it, and each subscriber is told which fields of
    /// which resources changed. Returns whether the record had moved.
    pub fn sync_from(&self, record: &Doc) -> bool {
        let heads = record.committed_document().get_heads();
        let mut inner = self.mutate();
        if inner.seen == heads {
            return false;
        }
        // The store's own writes leave the mirror: what they touched is forgotten, and the items
        // they added, removed or moved are moved again below, as the record has them.
        let mut items = inner.drop_local_from(0);
        inner.rollback();
        let changed = if inner.mirror.committed_document().get_heads() == inner.seen {
            let changes = record.committed_document().get_changes(&inner.seen);
            let mirror = inner.mirror_mut();
            let applied = mirror.apply_incoming(changes.iter().cloned());
            write_as_a_known_actor(mirror);
            applied
                .ok()
                .map(|()| changed_by(&changes, &inner.mirror, &inner.index))
        } else {
            None
        };
        inner.seen = heads;
        if let Some(mut changed) = changed {
            inner.stats.incremental_syncs = inner.stats.incremental_syncs.saturating_add(1);
            for (key, ids) in std::mem::take(&mut changed.items) {
                items.entry(key).or_default().extend(ids);
            }
            {
                let mut cache = inner.cache.borrow_mut();
                if changed.unplaced || changed.layout {
                    cache.clear();
                }
                for (doc, fields) in &changed.docs {
                    if fields.is_empty() {
                        cache.forget_doc(doc);
                    }
                    // A collection whose items the change names keeps its listing: they are
                    // moved in it below.
                    for key in fields.iter().filter(|key| !items.contains_key(*key)) {
                        cache.forget(key);
                    }
                }
            }
            if changed.structure || changed.unplaced {
                inner.index = scan(&inner.mirror);
            }
            let mut listings = inner.reconcile(&items);
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
        } else {
            // The mirror held a change of its own, which only a copy of the record removes.
            inner.stats.full_syncs = inner.stats.full_syncs.saturating_add(1);
            inner.cache.borrow_mut().clear();
            let mut mirror = record.clone();
            mirror.rollback();
            write_as_a_known_actor(&mut mirror);
            inner.mirror = Arc::new(mirror);
            inner.index = scan(&inner.mirror);
            inner.notify_all();
        }
        true
    }

    /// Whether the mirror holds exactly what `record` holds: the record it last brought itself
    /// up to date with, not one before or after it. A record of the past (a version an agent
    /// reads) is not it, even when it was handed to [`Self::sync_from`], which only moves forward.
    #[must_use]
    pub fn is_at(&self, record: &Doc) -> bool {
        self.inner.borrow().mirror.committed_document().get_heads()
            == record.committed_document().get_heads()
    }

    /// What the store did so far.
    #[must_use]
    pub fn stats(&self) -> StoreStats {
        self.inner.borrow().stats
    }

    /// How much the store read from its mirror so far.
    #[must_use]
    pub fn read_stats(&self) -> crate::cache::ReadStats {
        self.inner.borrow().cache.borrow().stats()
    }

    /// The actions the runtime committed since the host last took them, oldest first. They stay
    /// available by ticket ([`Self::committed`]) until the door has answered ([`Self::take_ticket`]).
    #[must_use]
    pub fn take_committed(&self) -> Vec<Committed> {
        let mut inner = self.mutate();
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
        let mut inner = self.mutate();
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
        let mut inner = self.mutate();
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

    /// The mirror as it is now. A trial run (planning an action for the door) writes into the
    /// mirror like any run; [`Self::rewind`] takes those writes back, since only the door's
    /// commit makes a change.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        let mut inner = self.mutate();
        inner.trials = inner.trials.saturating_add(1);
        Checkpoint {
            held: inner.held.len(),
            local: inner.local.len(),
            index: inner.index.clone(),
            notes: inner.notes.clone(),
        }
    }

    /// Put the mirror back as the checkpoint held it. What the trial run wrote was told to
    /// nobody (not the views, not search or publication), so taking it back tells nobody
    /// either: planning an action, as publishing every item's actions does, changes nothing
    /// anyone follows.
    pub fn rewind(&self, checkpoint: Checkpoint) {
        let mut inner = self.mutate();
        let items = inner.drop_local_from(checkpoint.local);
        inner.reapply();
        // Nobody heard of the trial's steps, so nobody hears of them being taken back.
        inner.reconcile(&items);
        inner.index = checkpoint.index;
        inner.notes = checkpoint.notes;
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
        let mut inner = self.mutate();
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

    /// A cursor over what changes in the mirror from now on, for a consumer that keeps
    /// something derived from many resources (search, publication) up to date by redoing only
    /// what changed.
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

    /// Whether the resource `doc` exists in the mirror.
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

    /// The mirror's record of the resource `doc`, as the runtime sees it now.
    ///
    /// # Errors
    /// The resource does not exist or its type is not registered.
    pub fn record(&self, doc: &str) -> Result<DynRecord, StoreError> {
        let inner = self.inner.borrow();
        let (app, _) = inner
            .index
            .get(doc)
            .ok_or_else(|| not_found(format!("resource {doc}")))?;
        read_resource(&inner.mirror, app, doc).map_err(backend)
    }

    /// The mirror as it is now, to read on another thread: one copy for every reader until the
    /// mirror changes.
    #[must_use]
    pub fn snapshot(&self) -> std::sync::Arc<Snapshot> {
        let mut inner = self.inner.borrow_mut();
        if let Some(taken) = inner.snapshot.upgrade() {
            return taken;
        }
        let taken = std::sync::Arc::new(Snapshot {
            mirror: Arc::clone(&inner.mirror),
            index: inner.index.clone(),
        });
        inner.snapshot = std::sync::Arc::downgrade(&taken);
        taken
    }

    /// The store, to change: a snapshot taken before no longer stands for the mirror.
    fn mutate(&self) -> std::cell::RefMut<'_, Inner> {
        let mut inner = self.inner.borrow_mut();
        inner.snapshot = std::sync::Weak::new();
        inner
    }

    /// The mirror's record of the resource at `at` (a document or an item in one), as the
    /// runtime sees it now: an item is read without reading its siblings.
    ///
    /// # Errors
    /// The resource does not exist or its type is not registered.
    pub fn record_at(&self, at: &ResourceRef) -> Result<DynRecord, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        let located = locate_ref(&inner.mirror, (&app, &ty), at)?;
        let obj = located
            .obj
            .ok_or_else(|| not_found(format!("resource {}", at.id_string())))?;
        <DynRecord as tilo_resource::Fields>::read_fields(&inner.mirror, &obj).map_err(backend)
    }

    /// The first text of the resource at `at`, in the order of [`DynRecord`]'s text collection,
    /// and the path of the resource that holds it: what names a resource.
    #[must_use]
    pub fn first_text(&self, at: &ResourceRef) -> Option<(String, String)> {
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at).ok()?;
        inner
            .cache
            .borrow_mut()
            .first_text(&inner.mirror, (&app, &ty), at)
    }

    /// Where the item `item` is in the collection `field` of `at`, as its listing has it: found
    /// by its order key, not by walking the listing.
    #[must_use]
    pub fn position(&self, at: &ResourceRef, field: &str, item: &str) -> Option<usize> {
        let spec = FieldSpec {
            id: field.to_owned(),
            kind: tilolang_runtime::meta::FieldKind::Collection,
        };
        // Listed once, then kept up to date by the changes.
        Store::listing(self, at, &spec).ok()?;
        let inner = self.inner.borrow();
        inner.cache.borrow().position(&at.id_string(), field, item)
    }

    /// Create the resource `id` as one committed change: how a per-room singleton gets the same
    /// id on every replica.
    ///
    /// # Errors
    /// The type is not registered, or the resource already exists.
    pub fn create_with_id(&self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
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
        inner.write(
            &record.app,
            edit.clone(),
            id.as_bytes().to_vec(),
            touched,
            Vec::new(),
        )?;
        inner
            .index
            .insert(id.to_owned(), (record.app.clone(), record.ty.clone()));
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

    /// Apply one edit to the mirror's open transaction. `touched` names what it changes, and
    /// `items` the collection items it adds, removes or moves, which are moved in what the
    /// cache listed of their collections. Returns the steps each listed collection took.
    fn write(
        &mut self,
        app: &str,
        edit: DynEdit,
        seed: Vec<u8>,
        touched: Vec<FieldKey>,
        items: Vec<(FieldKey, String)>,
    ) -> Result<BTreeMap<DocId, Vec<ListingChange>>, StoreError> {
        let applied = apply_edits(self.mirror_mut(), app, std::slice::from_ref(&edit), &seed);
        // Forgotten or moved either way: a failed edit may have written part of itself.
        self.forget(&touched);
        let mut moved = ItemTouches::new();
        for (key, id) in &items {
            moved.entry(key.clone()).or_default().insert(id.clone());
        }
        let listings = self.reconcile(&moved);
        applied.map_err(|error| invalid(error.to_string()))?;
        self.local.push(LocalWrite {
            app: app.to_owned(),
            edit,
            seed,
            touched,
            items,
        });
        Ok(listings)
    }

    fn forget(&self, touched: &[FieldKey]) {
        let mut cache = self.cache.borrow_mut();
        for key in touched {
            cache.forget(key);
        }
    }

    /// Move the items `items` in the listings of their collections as the mirror holds them
    /// now; returns the steps taken, per document.
    fn reconcile(&self, items: &ItemTouches) -> BTreeMap<DocId, Vec<ListingChange>> {
        let mut cache = self.cache.borrow_mut();
        let mut out: BTreeMap<DocId, Vec<ListingChange>> = BTreeMap::new();
        for (key, ids) in items {
            if let Some(splices) = cache.reconcile(&self.mirror, key, ids) {
                record_splices(out.entry(key.doc().to_owned()).or_default(), key, splices);
            }
        }
        out
    }

    /// Drop the store's writes from the `start`th on (the caller applies the rest again), and
    /// forget what they changed. Returns the collection items they added, removed or moved:
    /// once the mirror is as it will be, the caller moves them back ([`Self::reconcile`]).
    fn drop_local_from(&mut self, start: usize) -> ItemTouches {
        let start = start.min(self.local.len());
        let mut items = ItemTouches::new();
        for write in self.local.drain(start..) {
            let mut cache = self.cache.borrow_mut();
            for key in &write.touched {
                cache.forget(key);
            }
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
        let mirror = Arc::make_mut(&mut self.mirror);
        for write in &self.local {
            // Each applied once already, on this same base.
            let _ = apply_edits(
                mirror,
                &write.app,
                std::slice::from_ref(&write.edit),
                &write.seed,
            );
        }
    }

    /// The mirror, to change: a copy of it when a snapshot still reads it.
    fn mirror_mut(&mut self) -> &mut Doc {
        Arc::make_mut(&mut self.mirror)
    }

    /// Drop the mirror's open transaction, if it has one.
    fn rollback(&mut self) {
        if self.mirror.pending_ops() > 0 {
            self.mirror_mut().rollback();
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

/// The place an item goes when the program named position `index`: before the item it saw
/// there, the one being moved left out, else the end. Read as the runtime reads a position.
fn beside(listing: &Listing, index: usize, moving: Option<&str>) -> tilo_resource::At {
    listing
        .items
        .iter()
        .filter(|id| Some(&***id) != moving)
        .nth(index)
        .map_or(tilo_resource::At::End, |id| {
            tilo_resource::At::Before(Id::from_raw(id.to_string()))
        })
}

/// The edit a mutation makes below the resource `at`, placing an inserted or moved item at
/// `place`, and the id of the item it inserts.
fn edit_of(
    at: &ResourceRef,
    m: &Mutation,
    new_id: &str,
    place: tilo_resource::At,
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
        Mutation::CollInsert { field, item, .. } => {
            let schema = lookup(&item.ty)
                .ok_or_else(|| not_found(format!("type {} is not registered", item.ty)))?;
            let empty = DynRecord::empty(schema).map_err(backend)?;
            (
                field,
                EditOp::InsertItem {
                    id: Some(Id::from_raw(new_id)),
                    at: place,
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
        Mutation::CollMove { field, item, .. } => (
            field,
            EditOp::MoveTo {
                id: Id::from_raw(item.clone()),
                at: place,
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
        self.mutate().fresh(prefix)
    }

    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError> {
        let id = self.mutate().fresh("r");
        self.create_with_id(record, &id)
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        let (obj, schema) =
            inner
                .cache
                .borrow_mut()
                .locate(&inner.mirror, (&app, &ty), at, &at.id_string())?;
        Ok(obj
            .as_ref()
            .and_then(|obj| type_at(&inner.mirror, obj))
            .unwrap_or_else(|| schema.id.clone()))
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
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
            // A position the program counted is named by the item it saw there, so a concurrent
            // insert, removal or sort elsewhere does not shift where the item lands.
            let place = match m {
                Mutation::CollInsert {
                    field,
                    index: Some(index),
                    ..
                } => Some((field, *index, None)),
                Mutation::CollMove { field, item, index } => Some((field, *index, Some(item))),
                _ => None,
            };
            let place = match place {
                None => tilo_resource::At::End,
                Some((field, index, moving)) => {
                    let listing =
                        inner
                            .cache
                            .borrow_mut()
                            .listing(&inner.mirror, (&app, &ty), at, field)?;
                    beside(&listing, index, moving.map(String::as_str))
                }
            };
            let new_id = inner.fresh("i");
            let (edit, item) = edit_of(at, m, &new_id, place)?;
            let edit = DynEdit::Edit {
                id: at.doc.clone(),
                edit,
            };
            (edit, item, Some(field_of(m)))
        };
        let seed = format!("{}:{}", txn.0, inner.counter).into_bytes();
        // A collection's items are moved in its listing; any other field is forgotten.
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
            let items = inner.drop_local_from(state.start);
            inner.reapply();
            inner.reconcile(&items);
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner
            .index
            .get(&at.doc)
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
        inner
            .cache
            .borrow_mut()
            .read(&inner.mirror, (app, ty), at, &field.id)
    }

    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner
            .index
            .get(&at.doc)
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
        inner
            .cache
            .borrow_mut()
            .listing(&inner.mirror, (app, ty), at, &field.id)
    }

    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        let inner = self.inner.borrow();
        let (app, ty) = inner
            .index
            .get(&at.doc)
            .ok_or_else(|| not_found(format!("resource {}", at.doc)))?;
        inner.cache.borrow_mut().listing_where(
            &inner.mirror,
            (app, ty),
            at,
            (&field.id, item),
            pred,
        )
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        let inner = self.inner.borrow();
        if !inner.index.contains_key(doc) {
            return Err(not_found(format!("resource {doc}")));
        }
        Ok(inner.notes.get(doc).cloned().unwrap_or_default())
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId {
        let mut inner = self.mutate();
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
        self.mutate().subs.retain(|held| held.id != sub);
    }

    fn outbox_pending(&self) -> Vec<OutboxEntry> {
        let inner = self.inner.borrow();
        let mut out = Vec::new();
        for (doc, (app, _)) in &inner.index {
            for held in outbox_entries(&inner.mirror, app, doc) {
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
