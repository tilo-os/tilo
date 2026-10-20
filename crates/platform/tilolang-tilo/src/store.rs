//! [`TiloStore`]: the runtime's [`Store`] over the room record (implementation design 7.2).
//!
//! The runtime reads and mutates resources through the `Store` trait and expects its own commits
//! to be visible at once. The room record, though, belongs to the framework: every change to it
//! is a recorded action committed by the executor, so it carries the invoker, the group and the
//! encoded action, and replays on every peer. The store therefore works on a **mirror**, a copy
//! of the room record:
//!
//! * a transaction applies each mutation to the mirror as it is made (so reads inside it see
//!   them) and records it as a [`DynEdit`];
//! * committing finalizes the mirror's change and queues a [`DynAction`] for the host, which
//!   commits it through the executor ([`TiloStore::take_committed`]);
//! * when the room record moves on (the executor applied the queued action, or a peer's change
//!   arrived), [`TiloStore::sync_from`] replaces the mirror with the record and tells the
//!   subscribers: the queued actions are gone from the mirror and present in the record.
//!
//! One transaction may be open at a time: rolling one back discards the mirror's open change.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::rc::Rc;

use automerge::{ChangeHash, ObjType, ReadDoc, ScalarValue, Value as AmValue};
use serde_json::Value as Json;
use tilo_resource::{Edit, EditOp, Id, Model, Segment};
use tilolang_runtime::Journal;
use tilolang_runtime::ids::{AppId, DocId, FieldId, RunId, TypeId};
use tilolang_runtime::store::{
    Applied, ChangeNote, ChangeSummary, FieldSpec, FieldValue, Mutation, NewRecord, OutboxEntry,
    Scalar, Store, StoreError, SubId, Txn,
};
use tilolang_runtime::value::ResourceRef;

use crate::action::{Authorship, DynAction, DynEdit, RECORDS_KEY, apply_edits, read_resource};
use crate::record::{DynRecord, DynValue};
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
    touched: BTreeMap<DocId, BTreeSet<FieldId>>,
}

struct Inner {
    mirror: Doc,
    /// The heads of the room record the mirror was last copied from.
    seen: Vec<ChangeHash>,
    /// Every resource of the mirror: its app and type.
    index: BTreeMap<DocId, (AppId, TypeId)>,
    txns: HashMap<u64, TxnState>,
    next_txn: u64,
    subs: Vec<Subscription>,
    next_sub: u64,
    committed: VecDeque<Committed>,
    /// Handed to the host and not yet answered by the door.
    handed: BTreeMap<u64, DynAction>,
    next_ticket: u64,
    journals: BTreeMap<RunId, Journal>,
    notes: BTreeMap<DocId, Vec<ChangeNote>>,
    instance: String,
    counter: u64,
}

/// The mirror as it was, to put back after a trial run; see [`TiloStore::checkpoint`].
pub struct Checkpoint {
    mirror: Doc,
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

fn scalar_of_json(value: &Json, composite: bool) -> Scalar {
    if composite {
        return Scalar::Json(value.clone());
    }
    match value {
        Json::Bool(flag) => Scalar::Bool(*flag),
        Json::Number(number) => number.as_i64().map_or_else(
            || Scalar::Float(number.as_f64().unwrap_or(0.0)),
            Scalar::Int,
        ),
        Json::String(text) => Scalar::Text(text.clone()),
        other => Scalar::Json(other.clone()),
    }
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

fn element_key(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
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

/// The record `at` names inside `root`.
fn descend<'a>(root: &'a DynRecord, at: &ResourceRef) -> Result<&'a DynRecord, StoreError> {
    let mut record = root;
    for step in &at.path {
        let Some(DynValue::Collection(items)) = record.get(&step.field) else {
            return Err(not_found(format!("collection `{}`", step.field)));
        };
        record = items
            .get(&Id::from_raw(step.item.clone()))
            .ok_or_else(|| not_found(format!("item `{}`", step.item)))?;
    }
    Ok(record)
}

impl TiloStore {
    /// A store whose mirror starts as `record`.
    #[must_use]
    pub fn new(record: &Doc) -> Self {
        let mirror = record.clone();
        let instance = automerge::ActorId::random()
            .to_hex_string()
            .chars()
            .take(8)
            .collect();
        let seen = record.committed_document().get_heads();
        let index = scan(&mirror);
        Self {
            inner: Rc::new(RefCell::new(Inner {
                mirror,
                seen,
                index,
                txns: HashMap::new(),
                next_txn: 1,
                subs: Vec::new(),
                next_sub: 1,
                committed: VecDeque::new(),
                handed: BTreeMap::new(),
                next_ticket: 1,
                journals: BTreeMap::new(),
                notes: BTreeMap::new(),
                instance,
                counter: 0,
            })),
        }
    }

    /// Bring the mirror up to date with the room record: when the record's heads moved, the
    /// mirror becomes a copy of it and every subscriber is told its document may have changed.
    /// Returns whether the record had moved.
    pub fn sync_from(&self, record: &Doc) -> bool {
        let heads = record.committed_document().get_heads();
        let mut inner = self.inner.borrow_mut();
        if inner.seen == heads {
            return false;
        }
        inner.mirror = record.clone();
        inner.seen = heads;
        inner.index = scan(&inner.mirror);
        let docs: BTreeSet<DocId> = inner.subs.iter().map(|sub| sub.doc.clone()).collect();
        for doc in docs {
            inner.notify(&ChangeSummary {
                doc,
                fields: Vec::new(),
            });
        }
        true
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

    /// The mirror as it is now. A trial run (planning an action for the door) writes into the
    /// mirror like any run; [`Self::rewind`] takes those writes back, since only the door's
    /// commit makes a change.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        let inner = self.inner.borrow();
        Checkpoint {
            mirror: inner.mirror.clone(),
            index: inner.index.clone(),
            notes: inner.notes.clone(),
        }
    }

    /// Put the mirror back as the checkpoint held it and tell every subscriber.
    pub fn rewind(&self, checkpoint: Checkpoint) {
        let mut inner = self.inner.borrow_mut();
        inner.mirror = checkpoint.mirror;
        inner.index = checkpoint.index;
        inner.notes = checkpoint.notes;
        let docs: BTreeSet<DocId> = inner.subs.iter().map(|sub| sub.doc.clone()).collect();
        for doc in docs {
            inner.notify(&ChangeSummary {
                doc,
                fields: Vec::new(),
            });
        }
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

    /// Create the resource `id` as one committed change: how a per-room singleton gets the same
    /// id on every replica.
    ///
    /// # Errors
    /// The type is not registered, or the resource already exists.
    pub fn create_with_id(&self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        let mut inner = self.inner.borrow_mut();
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
        apply_edits(
            &mut inner.mirror,
            &record.app,
            std::slice::from_ref(&edit),
            id.as_bytes(),
        )
        .map_err(backend)?;
        inner.mirror.commit();
        inner
            .index
            .insert(id.to_owned(), (record.app.clone(), record.ty.clone()));
        let action = DynAction {
            app: record.app.clone(),
            kind: format!("tl.{}.{}", record.app, schema.name),
            action_id: "$create".to_owned(),
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
        for sub in self.subs.iter_mut().filter(|sub| sub.doc == summary.doc) {
            (sub.callback)(summary);
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

    fn root(&self, at: &ResourceRef) -> Result<DynRecord, StoreError> {
        let (app, _) = self.locate(at)?;
        read_resource(&self.mirror, &app, &at.doc).map_err(backend)
    }
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
        let root = inner.root(at)?;
        Ok(descend(&root, at)?.schema().id.clone())
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_txn;
        inner.next_txn = inner.next_txn.wrapping_add(1);
        inner.txns.insert(id, TxnState::default());
        Ok(Txn(id))
    }

    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let mut inner = self.inner.borrow_mut();
        if !inner.txns.contains_key(&txn.0) {
            return Err(invalid("no such transaction"));
        }
        let (app, ty) = inner.locate(at)?;
        let root = inner.root(at)?;
        let target = descend(&root, at)?;
        let length = match (m, target.get(&field_of(m))) {
            (Mutation::CollInsert { .. }, Some(DynValue::Collection(items))) => items.len(),
            _ => 0,
        };
        let new_id = inner.fresh("i");
        let (edit, item) = edit_of(at, m, &new_id, length)?;
        let dyn_edit = DynEdit::Edit {
            id: at.doc.clone(),
            edit,
        };
        let seed = format!("{}:{}", txn.0, inner.counter);
        apply_edits(
            &mut inner.mirror,
            &app,
            std::slice::from_ref(&dyn_edit),
            seed.as_bytes(),
        )
        .map_err(|error| invalid(error.to_string()))?;
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
        state
            .touched
            .entry(at.doc.clone())
            .or_default()
            .insert(field_of(m));
        Ok(item.map_or(Applied::Done, Applied::Item))
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
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
        inner.mirror.commit();
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
        for (doc, fields) in state.touched {
            inner
                .notes
                .entry(doc.clone())
                .or_default()
                .push(note.clone());
            inner.notify(&ChangeSummary {
                doc,
                fields: fields
                    .into_iter()
                    .filter(|field| !field.is_empty())
                    .collect(),
            });
        }
        Ok(())
    }

    fn rollback(&mut self, txn: Txn) {
        let mut inner = self.inner.borrow_mut();
        if inner.txns.remove(&txn.0).is_some() {
            inner.mirror.rollback();
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let inner = self.inner.borrow();
        let root = inner.root(at)?;
        let record = descend(&root, at)?;
        let value = record
            .get(&field.id)
            .ok_or_else(|| not_found(format!("field {}", field.id)))?;
        let composite = record
            .schema()
            .field(&field.id)
            .is_some_and(|schema| schema.composite);
        Ok(match value {
            DynValue::Register(json) if json.is_null() => FieldValue::Absent,
            DynValue::Register(json) => FieldValue::Scalar(scalar_of_json(json, composite)),
            DynValue::Counter(counter) => FieldValue::Counter(counter.get()),
            DynValue::Text(text) => FieldValue::Text(text.clone()),
            DynValue::Collection(items) => {
                FieldValue::Items(items.iter().map(|(id, _)| id.as_str().to_owned()).collect())
            }
            DynValue::Set(set) => {
                let mut keys: Vec<String> = set.iter().map(element_key).collect();
                keys.sort();
                FieldValue::Keys(keys)
            }
            DynValue::Map(map) => FieldValue::Entries(
                map.iter()
                    .map(|(key, value)| (key.clone(), scalar_of_json(value, false)))
                    .collect(),
            ),
            DynValue::Record(_) => {
                return Err(invalid("a nested record is not a field the runtime reads"));
            }
        })
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
        Vec::new()
    }

    fn outbox_settle(
        &mut self,
        _entry: &OutboxEntry,
        _note: &ChangeNote,
    ) -> Result<(), StoreError> {
        Err(not_found("outbox entry"))
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
