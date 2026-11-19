//! The resource store: every resource of a workspace in documents the store places, written and
//! read the same way whichever app or language declared it.
//!
//! The store keeps of a resource its kind, the resource it is nested in, its place among its
//! parent's resources, and its fields. Where it is stored is the store's alone. A document holds
//! resources of one reader set only (who may read them, which picks the document's key), so
//! sealing, syncing and reading all go by document: a query walks only the documents its reader
//! may open, and a resource whose readers change moves to a document of the new ones. Within a
//! reader set, the resources nested in one parent fill a document of the parent's
//! (`<readers>/<parent>#<n>`, the top-level resources of a kind `<readers>/<kind>#<n>`); a
//! document holds at most [`DOCUMENT_LIMIT`] resources, and the next one starts when the newest
//! is full. So a write changes one small document, a newcomer is sent the documents they open,
//! and no document grows with the workspace.
//!
//! The layout of a document, which nothing outside this module reads: its reader set under `w`,
//! and a map `r` of each resource by its id, holding its kind (`k`), its parent (`p`), its place
//! (`o`) and its fields (`f`), a scalar field as a scalar and a set as a map keyed by its members.
//!
//! Every apply updates the index in the same call ([`CommitHook`]): a local write
//! ([`Store::put`], [`Store::remove`]) from what it wrote, and a peer's changes
//! ([`Store::receive`]) from the patches their apply logged, which name the resources they touched
//! and nothing else. Both cost what they changed, never the size of the store.
//!
//! **Starting.** A document the store kept is given back whole ([`Store::load`]), and its
//! resources are read from it; or, faster, the index's snapshot of it ([`Store::snapshot`]) is
//! given with the heads the document is at ([`Store::restore`]), and the document itself is
//! read only when a write or a peer's change needs it, from the source the store was made with
//! ([`Store::with_source`]). [`Store::evict`] lets a document go from memory again; its
//! resources stay indexed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::transaction::Transactable as _;
use automerge::{
    Automerge, Change as AmChange, ChangeHash, ObjId, ObjType, PatchLog, Prop, ROOT, ReadDoc,
    ScalarValue, Value as AmValue,
};

use crate::index::{Change, Commit, CommitHook as _, Index, Indexed};
use crate::query::{Filter, Page, Query};
use crate::value::{Cell, Value};

/// The most resources one document holds before the next one starts.
pub const DOCUMENT_LIMIT: usize = 4096;

const READERS: &str = "w";
const HELD: &str = "r";
const KIND: &str = "k";
const PARENT: &str = "p";
const ORDER: &str = "o";
const FIELDS: &str = "f";

/// Why the store refused a write or a peer's changes.
#[derive(Debug)]
pub enum StoreError {
    /// No resource of this kind has this id.
    Missing { kind: String, id: String },
    /// The document refused the change, or what was given as one is none.
    Document(automerge::AutomergeError),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { kind, id } => write!(f, "there is no {kind} `{id}`"),
            Self::Document(error) => write!(f, "the document refused the change: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<automerge::AutomergeError> for StoreError {
    fn from(error: automerge::AutomergeError) -> Self {
        Self::Document(error)
    }
}

/// A resource as a write gives it: who may read it, where it is nested, its place, its indexed
/// fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resource {
    /// Its reader set: whatever names who may read it, which also picks the key of the
    /// document it is placed in.
    pub readers: String,
    pub parent: Option<String>,
    pub order: Option<String>,
    pub fields: BTreeMap<String, Cell>,
}

/// The documents of one owner (a parent, or one kind's top-level resources) of one reader set:
/// how many there are and how many resources the newest holds.
#[derive(Clone, Copy, Debug, Default)]
struct Filled {
    newest: u32,
    held: usize,
}

/// The change one local write made to its document, for whoever seals it with the document's
/// key and carries it to the other devices.
#[derive(Clone, Debug)]
pub struct Written {
    /// The document written to.
    pub doc: String,
    /// Its reader set, which picks its key.
    pub readers: String,
    pub change: AmChange,
}

/// Where the store reads a document it does not hold in memory: the saved document, or `None`
/// for one never kept.
pub type Source = Box<dyn FnMut(&str) -> Option<Vec<u8>>>;

/// The resource store. See the module docs.
#[derive(Default)]
pub struct Store {
    /// The documents in memory.
    docs: HashMap<String, Automerge>,
    filled: HashMap<(String, String), Filled>,
    index: Index,
    source: Option<Source>,
    /// Kinds an open query watches, by its watch.
    watches: BTreeMap<u64, String>,
    changed: BTreeSet<u64>,
}

impl Store {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that reads each document it does not hold in memory from `source`.
    #[must_use]
    pub fn with_source(source: Source) -> Self {
        Self {
            source: Some(source),
            ..Self::default()
        }
    }

    /// The index of everything the store holds, whoever may read it.
    #[must_use]
    pub const fn index(&self) -> &Index {
        &self.index
    }

    /// One page of `query`, of the resources whose readers `may_read` admits. `may_read` is the
    /// one permission check, asked of a reader set: nothing it refuses is listed, counted or
    /// skipped over.
    #[must_use]
    pub fn query(&self, query: &Query, may_read: &dyn Fn(&str) -> bool) -> Page {
        self.index.query(query, may_read)
    }

    /// How many resources of `kind` whose readers `may_read` admits hold to `filters`.
    #[must_use]
    pub fn count(&self, kind: &str, filters: &[Filter], may_read: &dyn Fn(&str) -> bool) -> usize {
        self.index.count(kind, filters, may_read)
    }

    /// The resource `id` of `kind`, when its readers `may_read` admits.
    #[must_use]
    pub fn get(&self, kind: &str, id: &str, may_read: &dyn Fn(&str) -> bool) -> Option<&Indexed> {
        let held = self.index.get(kind, id)?;
        self.index
            .readers(&held.doc)
            .is_some_and(may_read)
            .then_some(held)
    }

    /// Watches the resources of `kind`: from now on, every commit that changes one names the
    /// watch in [`Self::changed`], so an open query reads its page again.
    pub fn watch(&mut self, kind: &str) -> u64 {
        let id = self
            .watches
            .last_key_value()
            .map_or(0, |(id, _)| id.saturating_add(1));
        self.watches.insert(id, kind.to_owned());
        id
    }

    /// Stops the watch `id`.
    pub fn unwatch(&mut self, id: u64) {
        self.watches.remove(&id);
        self.changed.remove(&id);
    }

    /// The watches whose kind a commit changed since this was last asked.
    pub fn changed(&mut self) -> BTreeSet<u64> {
        std::mem::take(&mut self.changed)
    }

    /// Hands `commit` to the index and notes the watches it concerns.
    fn commit(&mut self, commit: &Commit<'_>) {
        for change in commit.changes {
            let kind = match change {
                Change::Put { kind, .. } | Change::Remove { kind, .. } => kind,
            };
            for (id, watched) in &self.watches {
                if watched == kind {
                    self.changed.insert(*id);
                }
            }
        }
        self.index.committed(commit);
    }

    /// The document `doc`, read from the source when it is not in memory, made when it is
    /// nowhere.
    fn doc(&mut self, doc: &str) -> Result<&mut Automerge, StoreError> {
        if !self.docs.contains_key(doc) {
            let read = match self.source.as_mut().and_then(|source| source(doc)) {
                Some(bytes) => Automerge::load(&bytes)?,
                None => Automerge::new(),
            };
            self.docs.insert(doc.to_owned(), read);
        }
        self.docs
            .get_mut(doc)
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))
    }

    /// The document a new resource of `kind` nested in `parent`, read by `readers`, goes to.
    fn place(&mut self, kind: &str, parent: Option<&str>, readers: &str) -> String {
        let owner = parent.unwrap_or(kind).to_owned();
        let filled = self
            .filled
            .entry((readers.to_owned(), owner.clone()))
            .or_default();
        if filled.held >= DOCUMENT_LIMIT {
            filled.newest = filled.newest.saturating_add(1);
            filled.held = 0;
        }
        filled.held = filled.held.saturating_add(1);
        format!("{readers}/{owner}#{}", filled.newest)
    }

    /// Writes `resource` as the resource `id` of `kind`: a new one into the document the store
    /// places it in, an existing one where it is, or (its readers changed) moved to a document
    /// of its new readers. Its fields are replaced by those given. Answers each document's
    /// change: one, or two for a move.
    ///
    /// # Errors
    /// When a document refuses the change.
    pub fn put(
        &mut self,
        kind: &str,
        id: &str,
        resource: Resource,
    ) -> Result<Vec<Written>, StoreError> {
        let mut written = Vec::with_capacity(1);
        let held = self.index.get(kind, id).map(|held| held.doc.clone());
        let doc_id = match held {
            Some(doc) if self.index.readers(&doc) == Some(resource.readers.as_str()) => doc,
            Some(_) => {
                written.push(self.remove(kind, id)?);
                self.place(kind, resource.parent.as_deref(), &resource.readers)
            }
            None => self.place(kind, resource.parent.as_deref(), &resource.readers),
        };
        let doc = self.doc(&doc_id)?;
        let mut tx = doc.transaction();
        if text_at(&tx, &ROOT, READERS).as_deref() != Some(resource.readers.as_str()) {
            tx.put(ROOT, READERS, resource.readers.as_str())?;
        }
        let held = map_in(&mut tx, &ROOT, HELD)?;
        // Field by field, so two devices changing different fields of one resource both keep
        // their change.
        let map = map_in(&mut tx, &held, id)?;
        if text_at(&tx, &map, KIND).as_deref() != Some(kind) {
            tx.put(&map, KIND, kind)?;
        }
        for (key, value) in [(PARENT, &resource.parent), (ORDER, &resource.order)] {
            match value {
                Some(value) if text_at(&tx, &map, key).as_ref() != Some(value) => {
                    tx.put(&map, key, value.as_str())?;
                }
                None if tx.get(&map, key)?.is_some() => tx.delete(&map, key)?,
                Some(_) | None => {}
            }
        }
        let fields = map_in(&mut tx, &map, FIELDS)?;
        let dropped: Vec<String> = tx
            .keys(&fields)
            .filter(|name| !resource.fields.contains_key(name))
            .collect();
        for name in dropped {
            tx.delete(&fields, name.as_str())?;
        }
        for (name, cell) in &resource.fields {
            match cell {
                Cell::One(value) => {
                    let held = tx
                        .get(&fields, name.as_str())?
                        .map(|(held, _)| value_of(&held));
                    if held.as_ref() != Some(value) {
                        tx.put(&fields, name.as_str(), scalar(value))?;
                    }
                }
                Cell::Set(members) => {
                    let set = map_in(&mut tx, &fields, name)?;
                    let keys: BTreeSet<String> = members.iter().map(member_key).collect();
                    let gone: Vec<String> =
                        tx.keys(&set).filter(|key| !keys.contains(key)).collect();
                    for key in gone {
                        tx.delete(&set, key.as_str())?;
                    }
                    for key in keys {
                        if tx.get(&set, key.as_str())?.is_none() {
                            tx.put(&set, key.as_str(), true)?;
                        }
                    }
                }
            }
        }
        tx.commit();
        let change = doc
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.commit(&Commit {
            doc: &doc_id,
            readers: &resource.readers,
            heads: &heads,
            changes: &[Change::Put {
                kind: kind.to_owned(),
                id: id.to_owned(),
                parent: resource.parent,
                order: resource.order,
                fields: resource.fields,
            }],
        });
        written.push(Written {
            doc: doc_id,
            readers: resource.readers,
            change,
        });
        Ok(written)
    }

    /// Removes the resource `id` of `kind`, and with it nothing else: the resources nested in it
    /// are the caller's to remove first.
    ///
    /// # Errors
    /// When there is no such resource, or its document refuses the change.
    pub fn remove(&mut self, kind: &str, id: &str) -> Result<Written, StoreError> {
        let missing = || StoreError::Missing {
            kind: kind.to_owned(),
            id: id.to_owned(),
        };
        let doc_id = self.index.get(kind, id).ok_or_else(missing)?.doc.clone();
        let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
        let doc = self.doc(&doc_id)?;
        let mut tx = doc.transaction();
        if let Some((AmValue::Object(ObjType::Map), held)) = tx.get(ROOT, HELD)? {
            tx.delete(&held, id)?;
        }
        tx.commit();
        let change = doc
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.commit(&Commit {
            doc: &doc_id,
            readers: &readers,
            heads: &heads,
            changes: &[Change::Remove {
                kind: kind.to_owned(),
                id: id.to_owned(),
            }],
        });
        Ok(Written {
            doc: doc_id,
            readers,
            change,
        })
    }

    /// Applies a peer's `changes` to the document `doc`, which `readers` may read, and indexes
    /// the resources they touched.
    ///
    /// # Errors
    /// When the document refuses the changes.
    pub fn receive(
        &mut self,
        doc_id: &str,
        readers: &str,
        changes: Vec<AmChange>,
    ) -> Result<(), StoreError> {
        let doc = self.doc(doc_id)?;
        let mut log = PatchLog::active();
        doc.apply_changes_log_patches(changes, &mut log)?;
        let patches = doc.make_patches(&mut log);
        let mut touched = BTreeSet::new();
        let mut every = false;
        for patch in &patches {
            let mut keys = patch.path.iter().map(|(_, prop)| match prop {
                Prop::Map(key) => Some(key.as_str()),
                Prop::Seq(_) => None,
            });
            let own = match &patch.action {
                automerge::PatchAction::PutMap { key, .. }
                | automerge::PatchAction::DeleteMap { key } => Some(key.as_str()),
                _ => None,
            };
            let top = keys.next().flatten();
            // The document's resources arrived whole.
            every |= patch.path.is_empty() && own == Some(HELD);
            let id = keys.next().flatten().or_else(|| top.and(own));
            if top == Some(HELD)
                && let Some(id) = id
            {
                touched.insert(id.to_owned());
            }
        }
        self.index_resources(doc_id, readers, (!every).then_some(touched))
    }

    /// Reads from `doc` the resources `only` names (all of them when `None`) and commits what
    /// it holds of them to the index.
    fn index_resources(
        &mut self,
        doc_id: &str,
        readers: &str,
        only: Option<BTreeSet<String>>,
    ) -> Result<(), StoreError> {
        let doc = self.doc(doc_id)?;
        let held = match doc.get(ROOT, HELD)? {
            Some((AmValue::Object(ObjType::Map), held)) => Some(held),
            _ => None,
        };
        let ids: BTreeSet<String> = match only {
            Some(ids) => ids,
            None => held
                .as_ref()
                .map(|held| doc.keys(held).collect())
                .unwrap_or_default(),
        };
        let reads: Vec<(String, Option<(String, Resource)>)> = ids
            .into_iter()
            .map(|id| {
                let found = held.as_ref().and_then(|held| read(doc, held, &id));
                (id, found)
            })
            .collect();
        let heads = doc.get_heads();
        let count = held.as_ref().map_or(0, |held| doc.length(held));
        let mut changes = Vec::with_capacity(reads.len());
        for (id, found) in reads {
            match found {
                Some((kind, resource)) => changes.push(Change::Put {
                    kind,
                    id,
                    parent: resource.parent,
                    order: resource.order,
                    fields: resource.fields,
                }),
                None => {
                    // Gone from this document: the index names its kind.
                    if let Some(kind) = self.index.kind_of(&id, doc_id) {
                        changes.push(Change::Remove { kind, id });
                    }
                }
            }
        }
        for change in &changes {
            if let Change::Put { parent, kind, .. } = change {
                self.know_document(doc_id, readers, parent.as_deref().unwrap_or(kind), count);
            }
        }
        self.commit(&Commit {
            doc: doc_id,
            readers,
            heads: &heads,
            changes: &changes,
        });
        Ok(())
    }

    /// Notes that `doc` is a document of `owner`'s holding `count` resources, so a new resource
    /// goes to the newest document known.
    fn know_document(&mut self, doc: &str, readers: &str, owner: &str, count: usize) {
        let Some(number) = doc
            .strip_prefix(readers)
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_prefix(owner))
            .and_then(|rest| rest.strip_prefix('#'))
            .and_then(|number| number.parse::<u32>().ok())
        else {
            return;
        };
        let filled = self
            .filled
            .entry((readers.to_owned(), owner.to_owned()))
            .or_default();
        if number >= filled.newest {
            filled.newest = number;
            filled.held = count;
        }
    }

    /// Takes in the document `doc` the store kept (as [`Automerge::save`] wrote it) and indexes
    /// every resource it holds.
    ///
    /// # Errors
    /// When the bytes are no document.
    pub fn load(&mut self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let doc = Automerge::load(bytes)?;
        let readers = text_at(&doc, &ROOT, READERS).unwrap_or_default();
        self.index.forget(doc_id);
        self.docs.insert(doc_id.to_owned(), doc);
        self.index_resources(doc_id, &readers, None)
    }

    /// What the index holds of `doc`, as bytes [`Self::restore`] takes back: what a start reads
    /// instead of the document. `None` for a document the index has not seen.
    #[must_use]
    pub fn snapshot(&self, doc: &str) -> Option<Vec<u8>> {
        crate::snapshot::save(&self.index, doc)
    }

    /// Indexes the document `doc` from its `snapshot`, when the snapshot was taken at `heads`,
    /// the heads the document is at now; the document itself is read only when a write or a
    /// peer's change needs it. Whether the snapshot was taken: one of other heads, or no
    /// snapshot at all, takes nothing, and the document is then [loaded](Self::load) instead.
    pub fn restore(&mut self, doc: &str, snapshot: &[u8], heads: &[ChangeHash]) -> bool {
        let Some(restored) = crate::snapshot::read(snapshot) else {
            return false;
        };
        if restored.heads != heads {
            return false;
        }
        self.index.forget(doc);
        self.docs.remove(doc);
        let count = restored.changes.len();
        for change in &restored.changes {
            if let Change::Put { parent, kind, .. } = change {
                self.know_document(
                    doc,
                    &restored.readers,
                    parent.as_deref().unwrap_or(kind),
                    count,
                );
            }
        }
        self.commit(&Commit {
            doc,
            readers: &restored.readers,
            heads,
            changes: &restored.changes,
        });
        true
    }

    /// Lets the document `doc` go from memory, answering it saved; its resources stay indexed,
    /// and it is read again from the source when needed.
    pub fn evict(&mut self, doc: &str) -> Option<Vec<u8>> {
        self.docs.remove(doc).map(|doc| doc.save())
    }

    /// The document `doc` as it stands in memory, for whoever keeps or sends it whole.
    #[must_use]
    pub fn document(&self, doc: &str) -> Option<&Automerge> {
        self.docs.get(doc)
    }

    /// How many documents are in memory.
    #[must_use]
    pub fn in_memory(&self) -> usize {
        self.docs.len()
    }
}

fn scalar(value: &Value) -> ScalarValue {
    match value {
        Value::Null => ScalarValue::Null,
        Value::Bool(flag) => ScalarValue::Boolean(*flag),
        Value::Int(number) => ScalarValue::Int(*number),
        Value::Float(number) => ScalarValue::F64(*number),
        Value::Text(text) => ScalarValue::Str(text.as_str().into()),
    }
}

/// The key a set member is kept under: a text as itself, anything else as its value's text.
fn member_key(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
    }
}

fn value_of(value: &AmValue<'_>) -> Value {
    match value {
        AmValue::Scalar(scalar) => match scalar.as_ref() {
            ScalarValue::Str(text) => Value::Text(text.to_string()),
            ScalarValue::Int(number) | ScalarValue::Timestamp(number) => Value::Int(*number),
            ScalarValue::Uint(number) => Value::Int(i64::try_from(*number).unwrap_or(i64::MAX)),
            ScalarValue::F64(number) => Value::Float(*number),
            ScalarValue::Counter(counter) => Value::Int(i64::from(counter)),
            ScalarValue::Boolean(flag) => Value::Bool(*flag),
            ScalarValue::Bytes(_) | ScalarValue::Unknown { .. } | ScalarValue::Null => Value::Null,
        },
        AmValue::Object(_) => Value::Null,
    }
}

/// The map under `key` of `obj`, made when there is none.
fn map_in(
    tx: &mut automerge::transaction::Transaction<'_>,
    obj: &ObjId,
    key: &str,
) -> Result<ObjId, automerge::AutomergeError> {
    match tx.get(obj, key)? {
        Some((AmValue::Object(ObjType::Map), map)) => Ok(map),
        _ => tx.put_object(obj, key, ObjType::Map),
    }
}

fn text_at(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok()?? {
        (AmValue::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The resource `id` of the document `doc` as the store wrote it: its kind and the resource.
fn read(doc: &Automerge, held: &ObjId, id: &str) -> Option<(String, Resource)> {
    let (AmValue::Object(ObjType::Map), map) = doc.get(held, id).ok()?? else {
        return None;
    };
    let kind = text_at(doc, &map, KIND)?;
    let mut fields = BTreeMap::new();
    if let Some((AmValue::Object(ObjType::Map), held)) = doc.get(&map, FIELDS).ok().flatten() {
        for name in doc.keys(&held) {
            let Some((value, obj)) = doc.get(&held, name.as_str()).ok().flatten() else {
                continue;
            };
            let cell = match value {
                AmValue::Object(ObjType::Map) => {
                    Cell::Set(doc.keys(&obj).map(Value::Text).collect())
                }
                value => Cell::One(value_of(&value)),
            };
            fields.insert(name, cell);
        }
    }
    Some((
        kind,
        Resource {
            readers: String::new(),
            parent: text_at(doc, &map, PARENT),
            order: text_at(doc, &map, ORDER),
            fields,
        },
    ))
}
