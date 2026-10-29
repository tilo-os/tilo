//! `AmStore`: the reference [`Store`], one Automerge document per resource. Tests and the
//! command line use it; the platform's store is `tilolang_tilo::store`.
//!
//! Document layout. The root holds `$type`, `$app`, `$outbox`, every field but a collection,
//! keyed by its field id, and the document's items flat under `$items`, each a map holding
//! `$type`, its placement `$at` and its fields ([`crate::place`]):
//!
//! | kind | representation |
//! |---|---|
//! | register | a scalar; composite runtime values are JSON bytes |
//! | counter | an Automerge counter |
//! | text | a text object, changed by diff and splice |
//! | collection | nothing of its own: the items placed in it, by order key |
//! | set | a map `key -> true` |
//! | map | a map `key -> register` |
//!
//! The store is a cheap handle: clones share the documents, so a runtime and a test can both
//! hold one, and [`AmStore::sync_with`] syncs two replicas.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use crate::place::{self, AT_KEY, ITEMS_KEY, Place, Placement, Resolved};
use crate::store::{
    Applied, ChangeNote, ChangeSummary, FieldKey, FieldSpec, FieldValue, ListingChange, Mutation,
    NewRecord, OutboxEntry, OutboxState, Scalar, Settled, Splice, Store, StoreError, SubId, Txn,
};
use crate::textdiff;
use crate::value::ResourceRef;

const TYPE_KEY: &str = "$type";
const APP_KEY: &str = "$app";
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
    /// How the listings of the collections it changed moved, step by step.
    listings: BTreeMap<FieldKey, Vec<Splice>>,
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
    /// Each document's items resolved into their collections, as the document is now.
    resolved: RefCell<HashMap<DocId, Rc<Resolved>>>,
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
                resolved: RefCell::new(HashMap::new()),
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
            mine.forget(&id);
            theirs.forget(&id);
            if mine.docs.get_mut(&id).map(AutoCommit::get_heads).as_ref() != Some(&before_a) {
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

    /// The document `doc` changed: its items are resolved again when next read.
    fn forget(&self, doc: &str) {
        self.resolved.borrow_mut().remove(doc);
    }

    /// The items of `doc` resolved into their collections.
    fn resolved(&self, doc: &str) -> Result<Rc<Resolved>, StoreError> {
        if let Some(held) = self.resolved.borrow().get(doc) {
            return Ok(Rc::clone(held));
        }
        let resolved = Rc::new(resolve_doc(self.doc(doc)?));
        self.resolved
            .borrow_mut()
            .insert(doc.to_owned(), Rc::clone(&resolved));
        Ok(resolved)
    }

    /// The map holding the fields of `at`: the document's root, or the item's map while the
    /// item is in a collection.
    fn locate(&self, at: &ResourceRef) -> Result<ObjId, StoreError> {
        let doc = self.doc(&at.doc)?;
        let Some(item) = &at.item else {
            return Ok(ROOT);
        };
        if !self.resolved(&at.doc)?.places.contains_key(item) {
            return Err(not_found(format!("item `{item}`")));
        }
        item_obj(doc, item).ok_or_else(|| not_found(format!("item `{item}`")))
    }

    /// The items of the collection `field` of `at`, with their order keys.
    fn members(&self, at: &ResourceRef, field: &str) -> Result<Vec<(String, String)>, StoreError> {
        Ok(self
            .resolved(&at.doc)?
            .children
            .get(&(at.item.clone(), field.to_owned()))
            .cloned()
            .unwrap_or_default())
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

/// The map of the item `id`, wherever it is placed.
fn item_obj(doc: &AutoCommit, id: &str) -> Option<ObjId> {
    let items = child_obj(doc, &ROOT, ITEMS_KEY).ok()??;
    child_obj(doc, &items, id).ok()?
}

/// Every item of `doc` resolved into the collection it is in.
fn resolve_doc(doc: &AutoCommit) -> Resolved {
    let mut held = BTreeMap::new();
    if let Ok(Some(items)) = child_obj(doc, &ROOT, ITEMS_KEY) {
        for id in doc.keys(&items) {
            if let Ok(Some(obj)) = child_obj(doc, &items, &id)
                && let Some(at) = place::held_at(doc, &obj)
            {
                held.insert(id, at);
            }
        }
    }
    place::resolve(&held, &|_, _, _| true)
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
        FieldKind::RichText => Ok(FieldValue::Rich(match child_obj(doc, obj, key)? {
            Some(rich) => crate::rich::read_under(doc, &rich),
            None => crate::rich::RichValue::default(),
        })),
        FieldKind::Collection => Err(StoreError::Invalid(format!(
            "collection `{key}` is read from its items' placements"
        ))),
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
            // A collection is its items' placements; nothing of it is stored with its holder.
            FieldKind::Register | FieldKind::Collection => {}
            FieldKind::Counter => doc
                .put(obj, key, ScalarValue::counter(0))
                .map_err(backend)?,
            FieldKind::Text => {
                doc.put_object(obj, key, ObjType::Text).map_err(backend)?;
            }
            FieldKind::RichText => {
                crate::rich::ensure_field(doc, obj, key).map_err(backend)?;
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
        | Mutation::SetRich { field, .. }
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

/// The collection a place names, as a field key of the document `doc`.
fn collection_key(doc: &str, place: &Place) -> FieldKey {
    FieldKey {
        resource: match &place.parent {
            Some(parent) => format!("{doc}/{parent}"),
            None => doc.to_owned(),
        },
        field: place.field.clone(),
    }
}

/// Where an item is, as a collection's listing has it: the collection and the index.
type Listed = (FieldKey, usize);

impl Inner {
    /// Where the item `item` of `doc` is listed now.
    fn listed(&self, doc: &str, item: &str) -> Option<Listed> {
        let resolved = self.resolved(doc).ok()?;
        let place = resolved.places.get(item)?;
        let index = resolved
            .children
            .get(&(place.parent.clone(), place.field.clone()))?
            .iter()
            .position(|(_, id)| id == item)?;
        Some((collection_key(doc, place), index))
    }

    /// Write the placement of the item `item` of `at`'s document.
    fn place(&mut self, doc_id: &str, item: &str, placement: &Placement) -> Result<(), StoreError> {
        let doc = self
            .docs
            .get_mut(doc_id)
            .ok_or_else(|| not_found(format!("document {doc_id}")))?;
        let obj = item_obj(doc, item).ok_or_else(|| not_found(format!("item `{item}`")))?;
        doc.put(&obj, AT_KEY, placement.encode()).map_err(backend)?;
        self.forget(doc_id);
        Ok(())
    }

    /// The item `item` (to be) placed at `index` of the collection `field` of `at`, from where
    /// it is now (`was`).
    fn placed_at(
        &self,
        at: &ResourceRef,
        field: &str,
        (item, index): (&str, Option<usize>),
        was: Option<Place>,
    ) -> Result<Placement, StoreError> {
        let others: Vec<(String, String)> = self
            .members(at, field)?
            .into_iter()
            .filter(|(_, id)| id != item)
            .collect();
        let key = place::key_at(&others, index.unwrap_or(others.len()));
        Ok(Placement::At {
            place: Place {
                parent: at.item.clone(),
                field: field.to_owned(),
                key,
            },
            was,
        })
    }

    /// A new item of the type `item` at `index` (the end for `None`) of the collection `field`
    /// of `at`, stored by its fresh id.
    fn insert_item(
        &mut self,
        at: &ResourceRef,
        field: &str,
        (item, index): (&NewRecord, Option<usize>),
    ) -> Result<Applied, StoreError> {
        let id = self.fresh("i");
        let placement = self.placed_at(at, field, (&id, index), None)?;
        let doc = self
            .docs
            .get_mut(&at.doc)
            .ok_or_else(|| not_found(format!("document {}", at.doc)))?;
        let items = match child_obj(doc, &ROOT, ITEMS_KEY)? {
            Some(items) => items,
            None => doc
                .put_object(&ROOT, ITEMS_KEY, ObjType::Map)
                .map_err(backend)?,
        };
        let rec = doc
            .put_object(&items, id.as_str(), ObjType::Map)
            .map_err(backend)?;
        doc.put(&rec, TYPE_KEY, item.ty.as_str()).map_err(backend)?;
        init_fields(doc, &rec, &item.fields)?;
        doc.put(&rec, AT_KEY, placement.encode()).map_err(backend)?;
        self.forget(&at.doc);
        Ok(Applied::Item(id))
    }

    /// The item `item`, from wherever it is in `at`'s document, to `index` of the collection
    /// `field` of `at`.
    fn move_item(
        &mut self,
        at: &ResourceRef,
        field: &str,
        (item, index): (&str, usize),
    ) -> Result<Applied, StoreError> {
        let resolved = self.resolved(&at.doc)?;
        let was = resolved
            .places
            .get(item)
            .cloned()
            .ok_or_else(|| not_found(format!("item `{item}`")))?;
        if at
            .item
            .as_deref()
            .is_some_and(|holder| resolved.within(holder, item))
        {
            return Err(StoreError::Invalid(
                "an item cannot go inside itself".into(),
            ));
        }
        let placement = self.placed_at(at, field, (item, Some(index)), Some(was))?;
        self.place(&at.doc.clone(), item, &placement)?;
        Ok(Applied::Done)
    }

    fn apply_mutation(&mut self, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        let outbox_id = match m {
            Mutation::OutboxPush { key, .. } => {
                Some(key.clone().unwrap_or_else(|| self.fresh("o")))
            }
            _ => None,
        };
        let obj = self.locate(at)?;
        match m {
            Mutation::CollInsert { field, item, index } => {
                return self.insert_item(at, field, (item, *index));
            }
            Mutation::CollRemove { item, .. } => {
                self.place(&at.doc.clone(), item, &Placement::Removed)?;
                return Ok(Applied::Done);
            }
            Mutation::CollMove { field, item, index } => {
                return self.move_item(at, field, (item, *index));
            }
            _ => {}
        }
        let doc = self
            .docs
            .get_mut(&at.doc)
            .ok_or_else(|| not_found(format!("document {}", at.doc)))?;
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
            Mutation::SetRich { field, value } => {
                let rich = crate::rich::ensure_field(doc, &obj, field).map_err(backend)?;
                crate::rich::store_under(doc, &rich, value).map_err(backend)?;
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
            Mutation::CollInsert { .. }
            | Mutation::CollRemove { .. }
            | Mutation::CollMove { .. } => {}
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
        let id = self.inner.borrow_mut().fresh("d");
        self.create_with_id(record, &id)
    }

    fn create_with_id(&mut self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        let mut inner = self.inner.borrow_mut();
        if inner.docs.contains_key(id) {
            return Err(StoreError::Invalid(format!("resource {id} exists")));
        }
        let id = id.to_owned();
        let mut doc = AutoCommit::new_with_encoding(TextEncoding::UnicodeCodePoint);
        doc.put(&ROOT, TYPE_KEY, record.ty.as_str())
            .map_err(backend)?;
        doc.put(&ROOT, APP_KEY, record.app.as_str())
            .map_err(backend)?;
        doc.put_object(&ROOT, OUTBOX_KEY, ObjType::Map)
            .map_err(backend)?;
        doc.put_object(&ROOT, ITEMS_KEY, ObjType::Map)
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
                touched: vec![id.clone()],
                ..ChangeNote::default()
            },
        )?;
        inner.docs.insert(id.clone(), doc);
        inner.forget(&id);
        Ok(ResourceRef::root(id, record.ty.clone()))
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let inner = self.inner.borrow();
        let obj = inner.locate(at)?;
        let doc = inner.doc(&at.doc)?;
        str_at(doc, &obj, TYPE_KEY).ok_or_else(|| not_found(format!("type of {}", at.id_string())))
    }

    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, FieldId)>, StoreError> {
        let inner = self.inner.borrow();
        let Some(item) = &at.item else {
            inner.doc(&at.doc)?;
            return Ok(None);
        };
        let place = inner
            .resolved(&at.doc)?
            .places
            .get(item)
            .cloned()
            .ok_or_else(|| not_found(format!("item `{item}`")))?;
        let holder = match &place.parent {
            Some(parent) => at.item(parent, String::new()),
            None => ResourceRef::root(at.doc.clone(), String::new()),
        };
        let obj = inner.locate(&holder)?;
        let ty = str_at(inner.doc(&at.doc)?, &obj, TYPE_KEY).unwrap_or_default();
        Ok(Some((ResourceRef { ty, ..holder }, place.field)))
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
        let moved = match m {
            Mutation::CollRemove { item, .. } | Mutation::CollMove { item, .. } => {
                Some(item.as_str())
            }
            Mutation::CollInsert { .. } => None,
            _ => return inner.apply_mutation(at, m),
        };
        // A collection's steps: where its item was, and where it is now, in whichever
        // collections those are.
        let was = moved.and_then(|item| inner.listed(&at.doc, item));
        let applied = inner.apply_mutation(at, m)?;
        let item = match &applied {
            Applied::Item(id) => Some(id.clone()),
            Applied::Done => moved.map(str::to_owned),
        };
        let now = item.as_deref().and_then(|item| inner.listed(&at.doc, item));
        let mut steps: Vec<(FieldKey, Splice)> = Vec::new();
        if let (Some((key, at)), Some(item)) = (was, &item) {
            steps.push((
                key,
                Splice::Remove {
                    at,
                    item: item.clone(),
                },
            ));
        }
        if let (Some((key, at)), Some(item)) = (now, &item) {
            steps.push((
                key,
                Splice::Insert {
                    at,
                    item: item.clone(),
                },
            ));
        }
        if let Some(t) = inner.txns.get_mut(&txn.0) {
            for (key, step) in steps {
                t.touched
                    .entry(at.doc.clone())
                    .or_default()
                    .insert(key.clone());
                t.listings.entry(key).or_default().push(step);
            }
        }
        Ok(applied)
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
        let state = inner
            .txns
            .remove(&txn.0)
            .ok_or_else(|| StoreError::Invalid("the transaction is closed".into()))?;
        let mut listings = state.listings;
        for (id, fields) in state.touched {
            if let Some(doc) = inner.docs.get_mut(&id) {
                commit_with_note(doc, note)?;
            }
            let listings = fields
                .iter()
                .filter_map(|key| {
                    listings.remove(key).map(|splices| ListingChange {
                        key: key.clone(),
                        splices,
                    })
                })
                .collect();
            inner.notify(&ChangeSummary {
                doc: id,
                fields: fields.into_iter().collect(),
                listings,
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
            inner.forget(id);
        }
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        let inner = self.inner.borrow();
        let obj = inner.locate(at)?;
        if field.kind == FieldKind::Collection {
            return Ok(FieldValue::Items(
                inner
                    .members(at, &field.id)?
                    .into_iter()
                    .map(|(_, id)| id)
                    .collect(),
            ));
        }
        read_field(inner.doc(&at.doc)?, &obj, field)
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

    /// Only the last `window` changes are read: their notes, newest first.
    fn recent_authors(
        &self,
        at: &ResourceRef,
        limit: usize,
        window: usize,
    ) -> Result<Vec<String>, StoreError> {
        let inner = self.inner.borrow();
        let am = inner.doc(&at.doc)?;
        let notes: Vec<ChangeNote> = am
            .committed_document()
            .get_changes(&[])
            .iter()
            .rev()
            .take(window)
            .filter_map(|c| c.message().and_then(|m| serde_json::from_str(m).ok()))
            .collect();
        Ok(crate::store::recent_authors_in(notes.iter(), at, limit))
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
                if let Some((entry, None)) = read_outbox_entry(doc, &outbox, doc_id, &id) {
                    out.push(entry);
                }
            }
        }
        out
    }

    fn outbox_mark(
        &mut self,
        entry: &OutboxEntry,
        state: &OutboxState,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
        let doc = inner
            .docs
            .get_mut(&entry.doc)
            .ok_or_else(|| not_found(format!("document {}", entry.doc)))?;
        let outbox = require_child(doc, &ROOT, OUTBOX_KEY)?;
        let obj = require_child(doc, &outbox, &entry.id)?;
        let (word, detail) = match state {
            OutboxState::Queued => ("queued", ""),
            OutboxState::Sending { by } => ("sending", by.as_str()),
            OutboxState::Waiting { reason } => ("waiting", reason.as_str()),
        };
        doc.put(&obj, "state", word).map_err(backend)?;
        doc.put(&obj, "detail", detail).map_err(backend)?;
        commit_with_note(doc, note)
    }

    fn outbox_settle(
        &mut self,
        entry: &OutboxEntry,
        outcome: &Settled,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.borrow_mut();
        let doc = inner
            .docs
            .get_mut(&entry.doc)
            .ok_or_else(|| not_found(format!("document {}", entry.doc)))?;
        let outbox = require_child(doc, &ROOT, OUTBOX_KEY)?;
        let obj = require_child(doc, &outbox, &entry.id)?;
        doc.put(&obj, "settled", true).map_err(backend)?;
        doc.put(&obj, "outcome", outcome.word()).map_err(backend)?;
        doc.put(&obj, "reason", outcome.reason()).map_err(backend)?;
        commit_with_note(doc, note)
    }

    fn outbox_states(&self) -> Vec<(String, crate::store::DeliveryState)> {
        let inner = self.inner.borrow();
        let mut out = Vec::new();
        for (doc_id, doc) in &inner.docs {
            let Ok(Some(outbox)) = child_obj(doc, &ROOT, OUTBOX_KEY) else {
                continue;
            };
            let mut keys: Vec<String> = doc.keys(&outbox).collect();
            keys.sort();
            // The one not settled when there is one, else the last.
            let entries: Vec<_> = keys
                .iter()
                .filter_map(|id| read_outbox_entry(doc, &outbox, doc_id, id))
                .collect();
            let last = entries
                .iter()
                .find(|(_, settled)| settled.is_none())
                .or_else(|| entries.last());
            if let Some((entry, settled)) = last {
                let settled = settled.as_ref().map(|(w, r)| (w.as_str(), r.as_str()));
                out.push((
                    doc_id.clone(),
                    crate::store::DeliveryState::of(&entry.state, settled),
                ));
            }
        }
        out
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

/// The entry `id`, and how it was settled (its outcome and reason) once it was.
fn read_outbox_entry(
    doc: &AutoCommit,
    outbox: &ObjId,
    doc_id: &str,
    id: &str,
) -> Option<(OutboxEntry, Option<(String, String)>)> {
    let obj = child_obj(doc, outbox, id).ok()??;
    let settled = matches!(
        doc.get(&obj, "settled"),
        Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true))
    );
    let settled = settled.then(|| {
        (
            str_at(doc, &obj, "outcome").unwrap_or_else(|| "done".to_owned()),
            str_at(doc, &obj, "reason").unwrap_or_default(),
        )
    });
    let capability = str_at(doc, &obj, "capability")?;
    let Scalar::Json(payload) = scalar_at(doc, &obj, "payload").ok()?? else {
        return None;
    };
    let detail = str_at(doc, &obj, "detail").unwrap_or_default();
    let state = match str_at(doc, &obj, "state").as_deref() {
        Some("sending") => OutboxState::Sending { by: detail },
        Some("waiting") => OutboxState::Waiting { reason: detail },
        _ => OutboxState::Queued,
    };
    Some((
        OutboxEntry {
            doc: doc_id.to_owned(),
            id: id.to_owned(),
            capability,
            payload,
            state,
        },
        settled,
    ))
}
