//! `AmStore`: the reference [`Store`], one Automerge document per resource
//! (implementation design 4.8).
//!
//! Document layout. The root holds `$type`, `$app`, `$outbox` and every field, keyed by its
//! field id:
//!
//! | kind | representation |
//! |---|---|
//! | register | a scalar; composite runtime values are JSON bytes |
//! | counter | an Automerge counter |
//! | text | a text object, changed by diff and splice |
//! | collection | a map `item id -> record object` plus the `$order` list of item ids |
//! | set | a map `key -> true` |
//! | map | a map `key -> register` |
//!
//! A collection move rewrites only `$order`. Concurrent moves can leave an id in `$order` twice
//! and a concurrent remove can leave one behind; readers keep the first occurrence and drop ids
//! with no record, so every replica reads the same order.
//!
//! The store is a cheap handle: clones share the documents, so a runtime and a test can both
//! hold one, and [`AmStore::sync_with`] syncs two replicas.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use automerge::sync::{self, SyncDoc};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{
    ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, TextEncoding, Value,
};
use serde_json::Value as Json;
use tilo_automerge::ApplyIncoming;

use crate::flow::Journal;
use crate::ids::{DocId, FieldId, RunId, TypeId};
use crate::meta::FieldKind;
use crate::store::{
    Applied, ChangeNote, ChangeSummary, FieldKey, FieldSpec, FieldValue, Mutation, NewRecord,
    OutboxEntry, Scalar, Store, StoreError, SubId, Txn,
};
use crate::textdiff;
use crate::value::ResourceRef;

const TYPE_KEY: &str = "$type";
const APP_KEY: &str = "$app";
const ORDER_KEY: &str = "$order";
const OUTBOX_KEY: &str = "$outbox";
/// The most rounds one sync of one document may take before it is reported as stuck.
const MAX_SYNC_ROUNDS: usize = 10_000;

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn not_found(what: impl Into<String>) -> StoreError {
    StoreError::NotFound(what.into())
}

struct Subscription {
    id: SubId,
    doc: DocId,
    callback: Box<dyn FnMut(&ChangeSummary)>,
}

#[derive(Default)]
struct TxnState {
    touched: BTreeMap<DocId, BTreeSet<FieldKey>>,
}

struct Inner {
    docs: BTreeMap<DocId, AutoCommit>,
    instance: String,
    counter: u64,
    next_txn: u64,
    txns: HashMap<u64, TxnState>,
    subs: Vec<Subscription>,
    next_sub: u64,
    journals: BTreeMap<RunId, String>,
}

/// The Automerge-backed reference store.
#[derive(Clone)]
pub struct AmStore {
    inner: Rc<RefCell<Inner>>,
}

impl Default for AmStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AmStore {
    pub fn new() -> Self {
        let instance: String = ActorId::random().to_hex_string().chars().take(8).collect();
        Self {
            inner: Rc::new(RefCell::new(Inner {
                docs: BTreeMap::new(),
                instance,
                counter: 0,
                next_txn: 1,
                txns: HashMap::new(),
                subs: Vec::new(),
                next_sub: 1,
                journals: BTreeMap::new(),
            })),
        }
    }

    /// Run Automerge's sync protocol between the two stores over every document either has,
    /// until both hold the same changes. A document only one side has is created empty on the
    /// other first. Subscribers of documents that changed are told.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when a sync message cannot be applied, or the same store is passed
    /// twice.
    pub fn sync_with(&mut self, other: &mut Self) -> Result<(), StoreError> {
        if Rc::ptr_eq(&self.inner, &other.inner) {
            return Err(StoreError::Invalid(
                "a store cannot sync with itself".into(),
            ));
        }
        let mut mine = self.inner.borrow_mut();
        let mut theirs = other.inner.borrow_mut();
        let ids: BTreeSet<DocId> = mine
            .docs
            .keys()
            .chain(theirs.docs.keys())
            .cloned()
            .collect();
        for id in ids {
            let a = mine.docs.entry(id.clone()).or_default();
            let b = theirs.docs.entry(id.clone()).or_default();
            let (before_a, before_b) = (a.get_heads(), b.get_heads());
            sync_pair(a, b)?;
            if a.get_heads() != before_a {
                mine.notify(&ChangeSummary::whole(id.clone()));
            }
            if theirs.docs.get_mut(&id).map(AutoCommit::get_heads).as_ref() != Some(&before_b) {
                theirs.notify(&ChangeSummary::whole(id));
            }
        }
        Ok(())
    }

    /// The ids of the documents this store holds.
    pub fn doc_ids(&self) -> Vec<DocId> {
        self.inner.borrow().docs.keys().cloned().collect()
    }
}

fn sync_pair(a: &mut AutoCommit, b: &mut AutoCommit) -> Result<(), StoreError> {
    let (mut state_a, mut state_b) = (sync::State::new(), sync::State::new());
    for _ in 0..MAX_SYNC_ROUNDS {
        let to_b = a.sync().generate_sync_message(&mut state_a);
        let sent_to_b = to_b.is_some();
        if let Some(msg) = to_b {
            b.receive_incoming_sync(&mut state_b, msg)
                .map_err(backend)?;
        }
        let to_a = b.sync().generate_sync_message(&mut state_b);
        let sent_to_a = to_a.is_some();
        if let Some(msg) = to_a {
            a.receive_incoming_sync(&mut state_a, msg)
                .map_err(backend)?;
        }
        if !sent_to_a && !sent_to_b {
            return Ok(());
        }
    }
    Err(StoreError::Backend("sync did not converge".into()))
}

impl Inner {
    fn notify(&mut self, summary: &ChangeSummary) {
        for s in self.subs.iter_mut().filter(|s| s.doc == summary.doc) {
            (s.callback)(summary);
        }
    }

    fn doc(&self, id: &str) -> Result<&AutoCommit, StoreError> {
        self.docs
            .get(id)
            .ok_or_else(|| not_found(format!("document {id}")))
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.counter = self.counter.wrapping_add(1);
        format!("{prefix}{}-{}", self.instance, self.counter)
    }
}

// ----- reading ---------------------------------------------------------------------------

fn child_obj(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<Option<ObjId>, StoreError> {
    match doc.get(parent, key).map_err(backend)? {
        Some((Value::Object(_), id)) => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn require_child(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<ObjId, StoreError> {
    child_obj(doc, parent, key)?.ok_or_else(|| not_found(format!("field or item `{key}`")))
}

fn locate(doc: &AutoCommit, at: &ResourceRef) -> Result<ObjId, StoreError> {
    let mut obj = ROOT;
    for step in &at.path {
        let coll = require_child(doc, &obj, &step.field)?;
        obj = require_child(doc, &coll, &step.item)?;
    }
    Ok(obj)
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn from_am(v: &ScalarValue) -> Result<Scalar, StoreError> {
    Ok(match v {
        ScalarValue::Boolean(b) => Scalar::Bool(*b),
        ScalarValue::Int(i) => Scalar::Int(*i),
        ScalarValue::Uint(u) => Scalar::Int(i64::try_from(*u).map_err(backend)?),
        ScalarValue::F64(f) => Scalar::Float(*f),
        ScalarValue::Str(s) => Scalar::Text(s.to_string()),
        ScalarValue::Bytes(b) => Scalar::Json(serde_json::from_slice(b).map_err(backend)?),
        ScalarValue::Null => Scalar::Json(Json::Null),
        other => {
            return Err(StoreError::Invalid(format!(
                "unexpected stored value {other:?}"
            )));
        }
    })
}

fn to_am(s: &Scalar) -> Result<ScalarValue, StoreError> {
    Ok(match s {
        Scalar::Bool(b) => ScalarValue::Boolean(*b),
        Scalar::Int(i) => ScalarValue::Int(*i),
        Scalar::Float(f) => ScalarValue::F64(*f),
        Scalar::Text(t) => ScalarValue::Str(t.as_str().into()),
        Scalar::Json(j) => ScalarValue::Bytes(serde_json::to_vec(j).map_err(backend)?),
    })
}

fn scalar_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<Option<Scalar>, StoreError> {
    match doc.get(obj, key).map_err(backend)? {
        Some((Value::Scalar(v), _)) => from_am(v.as_ref()).map(Some),
        Some((Value::Object(_), _)) => Err(StoreError::Invalid(format!(
            "field `{key}` holds an object, expected a register"
        ))),
        None => Ok(None),
    }
}

/// The raw `$order` list: the id at each index, `None` where the entry is not a string.
fn raw_order(doc: &AutoCommit, order: &ObjId) -> Vec<Option<String>> {
    (0..doc.length(order))
        .map(|i| match doc.get(order, i) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(id) => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The visible order of a collection: first occurrence of each id that still has a record.
/// Each entry is the id with its index in the raw `$order` list.
fn visible_order(
    doc: &AutoCommit,
    coll: &ObjId,
    order: &ObjId,
) -> Result<Vec<(usize, String)>, StoreError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (raw, id) in raw_order(doc, order).into_iter().enumerate() {
        let Some(id) = id else { continue };
        if seen.contains(&id) || child_obj(doc, coll, &id)?.is_none() {
            continue;
        }
        seen.insert(id.clone());
        out.push((raw, id));
    }
    Ok(out)
}

fn read_field(doc: &AutoCommit, obj: &ObjId, field: &FieldSpec) -> Result<FieldValue, StoreError> {
    let key = field.id.as_str();
    match field.kind {
        FieldKind::Register => {
            Ok(scalar_at(doc, obj, key)?.map_or(FieldValue::Absent, FieldValue::Scalar))
        }
        FieldKind::Counter => match doc.get(obj, key).map_err(backend)? {
            Some((Value::Scalar(v), _)) => match v.as_ref() {
                ScalarValue::Counter(c) => Ok(FieldValue::Counter(i64::from(c))),
                _ => Err(StoreError::Invalid(format!(
                    "field `{key}` is not a counter"
                ))),
            },
            None => Ok(FieldValue::Counter(0)),
            Some(_) => Err(StoreError::Invalid(format!(
                "field `{key}` is not a counter"
            ))),
        },
        FieldKind::Text => match child_obj(doc, obj, key)? {
            Some(text) => Ok(FieldValue::Text(doc.text(&text).map_err(backend)?)),
            None => Ok(FieldValue::Text(String::new())),
        },
        FieldKind::Collection => {
            let Some(coll) = child_obj(doc, obj, key)? else {
                return Ok(FieldValue::Items(Vec::new()));
            };
            let order = require_child(doc, &coll, ORDER_KEY)?;
            let ids = visible_order(doc, &coll, &order)?;
            Ok(FieldValue::Items(
                ids.into_iter().map(|(_, id)| id).collect(),
            ))
        }
        FieldKind::Set => match child_obj(doc, obj, key)? {
            Some(set) => Ok(FieldValue::Keys(doc.keys(&set).collect())),
            None => Ok(FieldValue::Keys(Vec::new())),
        },
        FieldKind::Map => match child_obj(doc, obj, key)? {
            Some(map) => {
                let mut entries = Vec::new();
                for k in doc.keys(&map) {
                    if let Some(v) = scalar_at(doc, &map, &k)? {
                        entries.push((k, v));
                    }
                }
                Ok(FieldValue::Entries(entries))
            }
            None => Ok(FieldValue::Entries(Vec::new())),
        },
    }
}

// ----- writing ---------------------------------------------------------------------------

fn init_fields(doc: &mut AutoCommit, obj: &ObjId, fields: &[FieldSpec]) -> Result<(), StoreError> {
    for f in fields {
        let key = f.id.as_str();
        match f.kind {
            FieldKind::Register => {}
            FieldKind::Counter => doc
                .put(obj, key, ScalarValue::counter(0))
                .map_err(backend)?,
            FieldKind::Text => {
                doc.put_object(obj, key, ObjType::Text).map_err(backend)?;
            }
            FieldKind::Collection => {
                let coll = doc.put_object(obj, key, ObjType::Map).map_err(backend)?;
                doc.put_object(&coll, ORDER_KEY, ObjType::List)
                    .map_err(backend)?;
            }
            FieldKind::Set | FieldKind::Map => {
                doc.put_object(obj, key, ObjType::Map).map_err(backend)?;
            }
        }
    }
    Ok(())
}

fn width(encoding: TextEncoding, c: char) -> usize {
    match encoding {
        TextEncoding::Utf8CodeUnit => c.len_utf8(),
        TextEncoding::Utf16CodeUnit => c.len_utf16(),
        TextEncoding::UnicodeCodePoint | TextEncoding::GraphemeCluster => 1,
    }
}

/// The offset in encoding units of character index `at` of `text`.
fn units_before(text: &str, encoding: TextEncoding, at: usize) -> usize {
    text.chars()
        .take(at)
        .fold(0usize, |acc, c| acc.saturating_add(width(encoding, c)))
}

fn set_text(doc: &mut AutoCommit, obj: &ObjId, key: &str, new: &str) -> Result<(), StoreError> {
    let text = require_child(doc, obj, key)?;
    let old = doc.text(&text).map_err(backend)?;
    let encoding = doc.text_encoding();
    for s in textdiff::diff(&old, new).iter().rev() {
        let start = units_before(&old, encoding, s.at);
        let end = units_before(&old, encoding, s.at.saturating_add(s.delete));
        let del = isize::try_from(end.saturating_sub(start)).map_err(backend)?;
        doc.splice_text(&text, start, del, &s.insert)
            .map_err(backend)?;
    }
    Ok(())
}

struct CollTarget {
    coll: ObjId,
    order: ObjId,
}

fn coll_target(doc: &AutoCommit, obj: &ObjId, field: &str) -> Result<CollTarget, StoreError> {
    let coll = require_child(doc, obj, field)?;
    let order = require_child(doc, &coll, ORDER_KEY)?;
    Ok(CollTarget { coll, order })
}

/// Raw indexes at which `id` appears in the order list, highest first.
fn raw_positions(doc: &AutoCommit, order: &ObjId, id: &str) -> Vec<usize> {
    let mut at: Vec<usize> = raw_order(doc, order)
        .iter()
        .enumerate()
        .filter(|(_, x)| x.as_deref() == Some(id))
        .map(|(raw, _)| raw)
        .collect();
    at.reverse();
    at
}

fn coll_move(
    doc: &mut AutoCommit,
    t: &CollTarget,
    item: &str,
    index: usize,
) -> Result<(), StoreError> {
    if child_obj(doc, &t.coll, item)?.is_none() {
        return Err(not_found(format!("collection item `{item}`")));
    }
    for raw in raw_positions(doc, &t.order, item) {
        doc.delete(&t.order, raw).map_err(backend)?;
    }
    let visible = visible_order(doc, &t.coll, &t.order)?;
    let target = visible
        .get(index)
        .map_or_else(|| doc.length(&t.order), |(raw, _)| *raw);
    doc.insert(&t.order, target, item).map_err(backend)
}

fn coll_remove(doc: &mut AutoCommit, t: &CollTarget, item: &str) -> Result<(), StoreError> {
    if child_obj(doc, &t.coll, item)?.is_none() {
        return Err(not_found(format!("collection item `{item}`")));
    }
    doc.delete(&t.coll, item).map_err(backend)?;
    for raw in raw_positions(doc, &t.order, item) {
        doc.delete(&t.order, raw).map_err(backend)?;
    }
    Ok(())
}

fn delete_key(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Result<(), StoreError> {
    if doc.get(parent, key).map_err(backend)?.is_some() {
        doc.delete(parent, key).map_err(backend)?;
    }
    Ok(())
}

fn field_of(m: &Mutation) -> Option<&FieldId> {
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
        | Mutation::MapRemove { field, .. } => Some(field),
        Mutation::OutboxPush { .. } => None,
    }
}

impl Inner {
    fn apply_mutation(&mut self, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let new_item = match m {
            Mutation::CollInsert { .. } => Some(self.fresh("i")),
            _ => None,
        };
        let outbox_id = match m {
            Mutation::OutboxPush { key, .. } => {
                Some(key.clone().unwrap_or_else(|| self.fresh("o")))
            }
            _ => None,
        };
        let doc = self
            .docs
            .get_mut(&at.doc)
            .ok_or_else(|| not_found(format!("document {}", at.doc)))?;
        let obj = locate(doc, at)?;
        match m {
            Mutation::SetRegister { field, value } => {
                doc.put(&obj, field.as_str(), to_am(value)?)
                    .map_err(backend)?;
            }
            Mutation::AddCounter { field, delta } => {
                doc.increment(&obj, field.as_str(), *delta)
                    .map_err(backend)?;
            }
            Mutation::SetText { field, text } => set_text(doc, &obj, field, text)?,
            Mutation::CollInsert { field, item, index } => {
                let id = new_item.unwrap_or_default();
                let t = coll_target(doc, &obj, field)?;
                let rec = doc
                    .put_object(&t.coll, id.as_str(), ObjType::Map)
                    .map_err(backend)?;
                doc.put(&rec, TYPE_KEY, item.ty.as_str()).map_err(backend)?;
                init_fields(doc, &rec, &item.fields)?;
                let len = doc.length(&t.order);
                let pos = index.map_or(len, |i| i.min(len));
                doc.insert(&t.order, pos, id.as_str()).map_err(backend)?;
                return Ok(Applied::Item(id));
            }
            Mutation::CollRemove { field, item } => {
                coll_remove(doc, &coll_target(doc, &obj, field)?, item)?;
            }
            Mutation::CollMove { field, item, index } => {
                coll_move(doc, &coll_target(doc, &obj, field)?, item, *index)?;
            }
            Mutation::SetAdd { field, key } => {
                let set = require_child(doc, &obj, field)?;
                doc.put(&set, key.as_str(), true).map_err(backend)?;
            }
            Mutation::SetRemove { field, key } | Mutation::MapRemove { field, key } => {
                let container = require_child(doc, &obj, field)?;
                delete_key(doc, &container, key)?;
            }
            Mutation::MapPut { field, key, value } => {
                let map = require_child(doc, &obj, field)?;
                doc.put(&map, key.as_str(), to_am(value)?)
                    .map_err(backend)?;
            }
            Mutation::OutboxPush {
                capability,
                payload,
                ..
            } => {
                let id = outbox_id.unwrap_or_default();
                let outbox = require_child(doc, &ROOT, OUTBOX_KEY)?;
                let entry = doc
                    .put_object(&outbox, id.as_str(), ObjType::Map)
                    .map_err(backend)?;
                doc.put(&entry, "capability", capability.as_str())
                    .map_err(backend)?;
                let bytes = serde_json::to_vec(payload).map_err(backend)?;
                doc.put(&entry, "payload", ScalarValue::Bytes(bytes))
                    .map_err(backend)?;
                doc.put(&entry, "settled", false).map_err(backend)?;
            }
        }
        Ok(Applied::Done)
    }
}

fn commit_with_note(doc: &mut AutoCommit, note: &ChangeNote) -> Result<(), StoreError> {
    let message = serde_json::to_string(note).map_err(backend)?;
    let _hash = doc.commit_with(CommitOptions::default().with_message(message));
    Ok(())
}

impl Store for AmStore {
    fn fresh_id(&mut self, prefix: &str) -> String {
        self.inner.borrow_mut().fresh(prefix)
    }

    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError> {
        let mut inner = self.inner.borrow_mut();
        let id = inner.fresh("d");
        let mut doc = AutoCommit::new_with_encoding(TextEncoding::UnicodeCodePoint);
        doc.put(&ROOT, TYPE_KEY, record.ty.as_str())
            .map_err(backend)?;
        doc.put(&ROOT, APP_KEY, record.app.as_str())
            .map_err(backend)?;
        doc.put_object(&ROOT, OUTBOX_KEY, ObjType::Map)
            .map_err(backend)?;
        init_fields(&mut doc, &ROOT, &record.fields)?;
        commit_with_note(
            &mut doc,
            &ChangeNote {
                action: "$create".into(),
                args: Json::Null,
                invoker: String::new(),
                chain: Vec::new(),
                module: String::new(),
                approved_by: None,
            },
        )?;
        inner.docs.insert(id.clone(), doc);
        Ok(ResourceRef::root(id, record.ty.clone()))
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let inner = self.inner.borrow();
        let doc = inner.doc(&at.doc)?;
        let obj = locate(doc, at)?;
        str_at(doc, &obj, TYPE_KEY).ok_or_else(|| not_found(format!("type of {}", at.id_string())))
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_txn;
        inner.next_txn = id.wrapping_add(1);
        inner.txns.insert(id, TxnState::default());
        Ok(Txn(id))
    }

    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let mut inner = self.inner.borrow_mut();
        if !inner.txns.contains_key(&txn.0) {
            return Err(StoreError::Invalid("the transaction is closed".into()));
        }
        // The document joins the transaction before its first operation, so a failure half
        // way through is rolled back with the rest.
        let fields = inner
            .txns
            .get_mut(&txn.0)
            .map(|t| t.touched.entry(at.doc.clone()).or_default());
        if let (Some(set), Some(field)) = (fields, field_of(m)) {
            set.insert(FieldKey::new(at, field.clone()));
        }
        inner.apply_mutation(at, m)
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
        let state = inner
            .txns
            .remove(&txn.0)
            .ok_or_else(|| StoreError::Invalid("the transaction is closed".into()))?;
        for (id, fields) in state.touched {
            if let Some(doc) = inner.docs.get_mut(&id) {
                commit_with_note(doc, note)?;
            }
            inner.notify(&ChangeSummary {
                doc: id,
                fields: fields.into_iter().collect(),
                listings: Vec::new(),
            });
        }
        Ok(())
    }

    fn rollback(&mut self, txn: Txn) {
        let mut inner = self.inner.borrow_mut();
        let Some(state) = inner.txns.remove(&txn.0) else {
            return;
        };
        for id in state.touched.keys() {
            if let Some(doc) = inner.docs.get_mut(id) {
                doc.rollback();
            }
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let inner = self.inner.borrow();
        let doc = inner.doc(&at.doc)?;
        let obj = locate(doc, at)?;
        read_field(doc, &obj, field)
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        let inner = self.inner.borrow();
        let am = inner.doc(doc)?;
        Ok(am
            .committed_document()
            .get_changes(&[])
            .iter()
            .filter_map(|c| c.message().and_then(|m| serde_json::from_str(m).ok()))
            .collect())
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
        self.inner.borrow_mut().subs.retain(|s| s.id != sub);
    }

    fn outbox_pending(&self) -> Vec<OutboxEntry> {
        let inner = self.inner.borrow();
        let mut out = Vec::new();
        for (doc_id, doc) in &inner.docs {
            let Ok(Some(outbox)) = child_obj(doc, &ROOT, OUTBOX_KEY) else {
                continue;
            };
            for id in doc.keys(&outbox) {
                if let Some(entry) = read_outbox_entry(doc, &outbox, doc_id, &id) {
                    out.push(entry);
                }
            }
        }
        out
    }

    fn outbox_settle(&mut self, entry: &OutboxEntry, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
        let doc = inner
            .docs
            .get_mut(&entry.doc)
            .ok_or_else(|| not_found(format!("document {}", entry.doc)))?;
        let outbox = require_child(doc, &ROOT, OUTBOX_KEY)?;
        let obj = require_child(doc, &outbox, &entry.id)?;
        doc.put(&obj, "settled", true).map_err(backend)?;
        commit_with_note(doc, note)
    }

    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError> {
        let text = serde_json::to_string(journal).map_err(backend)?;
        self.inner.borrow_mut().journals.insert(run.clone(), text);
        Ok(())
    }

    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError> {
        let inner = self.inner.borrow();
        inner
            .journals
            .get(run)
            .map(|t| serde_json::from_str(t).map_err(backend))
            .transpose()
    }
}

fn read_outbox_entry(
    doc: &AutoCommit,
    outbox: &ObjId,
    doc_id: &str,
    id: &str,
) -> Option<OutboxEntry> {
    let obj = child_obj(doc, outbox, id).ok()??;
    let settled = matches!(
        doc.get(&obj, "settled"),
        Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true))
    );
    if settled {
        return None;
    }
    let capability = str_at(doc, &obj, "capability")?;
    let Scalar::Json(payload) = scalar_at(doc, &obj, "payload").ok()?? else {
        return None;
    };
    Some(OutboxEntry {
        doc: doc_id.to_owned(),
        id: id.to_owned(),
        capability,
        payload,
    })
}
