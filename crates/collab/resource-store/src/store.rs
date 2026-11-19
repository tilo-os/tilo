//! The resource store: every resource of a workspace in documents the store places, written and
//! read the same way whichever app or language declared it.
//!
//! The store keeps of a resource its kind, the resource it is nested in, its place among its
//! parent's resources, and its fields. Where it is stored is the store's alone. The resources
//! nested in one parent fill a document of the parent's (`<parent>#<n>`, the top-level resources of
//! a kind `<kind>#<n>`); a document holds at most [`DOCUMENT_LIMIT`] resources, and the next one starts
//! when the newest is full. So a write changes one small document, a newcomer is sent the documents
//! they open, and no document grows with the workspace.
//!
//! The layout of a document, which nothing outside this module reads: a map `r` of each resource by
//! its id, holding its kind (`k`), its parent (`p`), its place (`o`) and its fields (`f`), a
//! scalar field as a scalar and a set as a map keyed by its members.
//!
//! Every apply updates the index in the same call ([`CommitHook`]): a local write
//! ([`Store::put`], [`Store::remove`]) from what it wrote, and a peer's changes
//! ([`Store::receive`]) from the patches their apply logged, which name the resources they touched
//! and nothing else. Both cost what they changed, never the size of the store.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use automerge::transaction::Transactable as _;
use automerge::{
    Automerge, Change as AmChange, ObjId, ObjType, PatchLog, Prop, ROOT, ReadDoc, ScalarValue,
    Value as AmValue,
};

use crate::index::{Change, Commit, CommitHook as _, Index};
use crate::query::{Filter, Page, Query};
use crate::value::{Cell, Value};

/// The most resources one document holds before the next one starts.
pub const DOCUMENT_LIMIT: usize = 4096;

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
    /// The document refused the change.
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

/// A resource as a write gives it: where it is nested, its place, its indexed fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resource {
    pub parent: Option<String>,
    pub order: Option<String>,
    pub fields: BTreeMap<String, Cell>,
}

/// The documents of one parent (or one kind's top-level resources): how many there are and how
/// many resources the newest holds.
#[derive(Clone, Copy, Debug, Default)]
struct Filled {
    newest: u32,
    held: usize,
}

/// The resource store. See the module docs.
#[derive(Default)]
pub struct Store {
    docs: HashMap<String, Automerge>,
    filled: HashMap<String, Filled>,
    index: Index,
}

/// The change one local write made to its document, for whoever carries it to the other devices.
#[derive(Clone, Debug)]
pub struct Written {
    /// The document written to.
    pub doc: String,
    pub change: AmChange,
}

impl Store {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The index of everything the store holds.
    #[must_use]
    pub const fn index(&self) -> &Index {
        &self.index
    }

    /// One page of `query`.
    #[must_use]
    pub fn query(&self, query: &Query) -> Page {
        self.index.query(query)
    }

    /// How many resources of `kind` hold to `filters`.
    #[must_use]
    pub fn count(&self, kind: &str, filters: &[Filter]) -> usize {
        self.index.count(kind, filters)
    }

    /// The document a new resource of `kind` nested in `parent` goes to.
    fn place(&mut self, kind: &str, parent: Option<&str>) -> String {
        let owner = parent.unwrap_or(kind).to_owned();
        let filled = self.filled.entry(owner.clone()).or_default();
        if filled.held >= DOCUMENT_LIMIT {
            filled.newest = filled.newest.saturating_add(1);
            filled.held = 0;
        }
        filled.held = filled.held.saturating_add(1);
        format!("{owner}#{}", filled.newest)
    }

    /// Writes `resource` as the resource `id` of `kind`: a new one into the document the store
    /// places it in, an existing one where it is. Its fields are replaced by those given.
    ///
    /// # Errors
    /// When the document's document refuses the change.
    pub fn put(&mut self, kind: &str, id: &str, resource: Resource) -> Result<Written, StoreError> {
        let doc_id = match self.index.get(kind, id) {
            Some(map) => map.doc.clone(),
            None => self.place(kind, resource.parent.as_deref()),
        };
        let doc = self.docs.entry(doc_id.clone()).or_default();
        let mut tx = doc.transaction();
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
            .ok_or_else(|| StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.index.committed(&Commit {
            doc: &doc_id,
            heads: &heads,
            changes: &[Change::Put {
                kind: kind.to_owned(),
                id: id.to_owned(),
                parent: resource.parent,
                order: resource.order,
                fields: resource.fields,
            }],
        });
        Ok(Written {
            doc: doc_id,
            change,
        })
    }

    /// Removes the resource `id` of `kind`, and with it nothing else: the resources nested in it
    /// are the caller's to remove first.
    ///
    /// # Errors
    /// When there is no such resource, or its document's document refuses the change.
    pub fn remove(&mut self, kind: &str, id: &str) -> Result<Written, StoreError> {
        let missing = || StoreError::Missing {
            kind: kind.to_owned(),
            id: id.to_owned(),
        };
        let doc_id = self.index.get(kind, id).ok_or_else(missing)?.doc.clone();
        let doc = self.docs.get_mut(&doc_id).ok_or_else(missing)?;
        let mut tx = doc.transaction();
        if let Some((AmValue::Object(ObjType::Map), held)) = tx.get(ROOT, HELD)? {
            tx.delete(&held, id)?;
        }
        tx.commit();
        let change = doc
            .get_last_local_change()
            .ok_or_else(|| StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.index.committed(&Commit {
            doc: &doc_id,
            heads: &heads,
            changes: &[Change::Remove {
                kind: kind.to_owned(),
                id: id.to_owned(),
            }],
        });
        Ok(Written {
            doc: doc_id,
            change,
        })
    }

    /// Applies a peer's `changes` to the document `doc`, and indexes the resources they touched.
    ///
    /// # Errors
    /// When the document refuses the changes.
    pub fn receive(&mut self, doc_id: &str, changes: Vec<AmChange>) -> Result<(), StoreError> {
        let doc = self.docs.entry(doc_id.to_owned()).or_default();
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
        let held = match doc.get(ROOT, HELD)? {
            Some((AmValue::Object(ObjType::Map), held)) => Some(held),
            _ => None,
        };
        if every && let Some(held) = &held {
            touched.extend(doc.keys(held));
        }
        let mut changes = Vec::with_capacity(touched.len());
        for id in touched {
            let read = held.as_ref().and_then(|held| read(doc, held, &id));
            match read {
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
        let heads = doc.get_heads();
        for change in &changes {
            if let Change::Put { parent, kind, .. } = change {
                self.know_part(doc_id, parent.as_deref().unwrap_or(kind));
            }
        }
        self.index.committed(&Commit {
            doc: doc_id,
            heads: &heads,
            changes: &changes,
        });
        Ok(())
    }

    /// Notes that `doc` is a document of `owner`'s, so a new resource goes to the newest document known.
    fn know_part(&mut self, doc: &str, owner: &str) {
        let Some(number) = doc
            .strip_prefix(owner)
            .and_then(|rest| rest.strip_prefix('#'))
            .and_then(|number| number.parse::<u32>().ok())
        else {
            return;
        };
        let held = self
            .docs
            .get(doc)
            .and_then(|doc| match doc.get(ROOT, HELD).ok()? {
                Some((AmValue::Object(ObjType::Map), held)) => Some(doc.length(&held)),
                _ => None,
            })
            .unwrap_or(0);
        let filled = self.filled.entry(owner.to_owned()).or_default();
        if number >= filled.newest {
            filled.newest = number;
            filled.held = held;
        }
    }

    /// The document `doc` as it stands, for whoever keeps or sends it whole.
    #[must_use]
    pub fn document(&self, doc: &str) -> Option<&Automerge> {
        self.docs.get(doc)
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
            parent: text_at(doc, &map, PARENT),
            order: text_at(doc, &map, ORDER),
            fields,
        },
    ))
}
