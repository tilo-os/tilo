//! The resource store: every resource of a workspace in documents the store places, written and
//! read the same way whichever app or language declared it.
//!
//! The store keeps of a resource its kind, the resource it is nested in, its place among its
//! parent's resources, and its fields. Where it is stored is the store's alone. A document holds
//! resources of one reader set only (who may read them, which picks the document's key), so
//! sealing, syncing and reading all go by document: a query walks only the documents its reader
//! may open, and a resource whose readers change moves to a document of the new ones. Within a
//! reader set, the resources nested in one parent fill a document of the parent's
//! (`<readers>/<parent>#<n>`, the top-level resources of a kind `<readers>/<kind>#<n>`, where
//! `<readers>` is `*` for the whole group and a short digest of any other set); a
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
//! **Content.** A resource's bulk content (rich text, lists, maps: what no filter or sort reads)
//! is never in the documents above. It is in a content document of its own, placed by the store
//! under the resource and in its reader set (`<readers>/<kind>/<id>~<n>`), so the same key seals
//! it and a reader is sent it with the resource. An app asks only for the content of a resource
//! ([`Store::content`], [`Store::edit_content`]). When the resource's readers change, its content
//! moves with it: copied, without its history, into a content document of the new readers, and
//! emptied in the old one, so nobody who lost access is sent what it holds next.
//!
//! **Starting.** A document the store kept is given back whole ([`Store::load`]), and its
//! resources are read from it; or, faster, the index's snapshot of it ([`Store::snapshot`]) is
//! given with the heads the document is at ([`Store::restore`]), and the document itself is
//! read only when a write or a peer's change needs it, from the source the store was made with
//! ([`Store::with_source`]). [`Store::evict`] lets a document go from memory again; its
//! resources stay indexed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{
    Automerge, Change as AmChange, ChangeHash, ObjId, ObjType, PatchLog, Prop, ROOT, ReadDoc,
    ScalarValue, Value as AmValue,
};

use crate::admit::{Admit, Proposal, Received, Refusal, Stamp};
use crate::index::{Change, Commit, CommitHook as _, Index, Indexed};
use crate::query::{Filter, Page, Query};
use crate::schema::Schema;
use crate::value::{Cell, Value};

/// The most resources one document holds before the next one starts.
pub const DOCUMENT_LIMIT: usize = 4096;

const READERS: &str = "w";
/// What a resource's key at the document's root starts with: each resource is a key of the root
/// of its own, so two devices starting one document both keep what they wrote.
const HELD: &str = "r:";
const KIND: &str = "k";
const PARENT: &str = "p";
const ORDER: &str = "o";
const FIELDS: &str = "f";

/// The content document of the resource `id` of `kind`, read by `readers`.
/// The field the index holds when a resource was made: the time its first version was stamped
/// at, as a query filters and orders by.
pub const CREATED: &str = "$created";

/// The field the index holds when a resource last changed, its content included.
pub const UPDATED: &str = "$updated";

/// What the field the index holds when one field of a resource last changed begins with; the
/// field's name follows. A field no change wrote after the resource was made has none: when it
/// was made ([`CREATED`]) stands for it.
pub const CHANGED: &str = "$changed.";

fn content_id(readers: &str, kind: &str, id: &str) -> String {
    format!("{}/{kind}/{id}~0", collab_types::reader_set_name(readers))
}

/// The kind and id of the resource whose content the document `doc` holds ([`content_id`]);
/// `None` for a document of resources.
fn content_resource(doc: &str) -> Option<(&str, &str)> {
    let (_, rest) = doc.strip_suffix("~0")?.split_once('/')?;
    // A kind names no `/`; an id may.
    rest.split_once('/')
}

/// A resource one change wrote ([`Store::wrote`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Wrote {
    pub kind: String,
    pub id: String,
    /// What it holds as the change left it: its indexed fields and its content, by name.
    pub holds: serde_json::Map<String, serde_json::Value>,
    /// The resource it is nested in, when it is nested in one: its kind, id and indexed fields.
    pub parent: Option<(String, String, serde_json::Map<String, serde_json::Value>)>,
}

/// Indexed `fields` as JSON: a scalar as itself, a set as the list of its members.
fn fields_json(fields: &BTreeMap<String, Cell>) -> serde_json::Map<String, serde_json::Value> {
    let scalar = |value: &Value| match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(flag) => serde_json::Value::Bool(*flag),
        Value::Int(number) => serde_json::Value::from(*number),
        Value::Float(number) => serde_json::Value::from(*number),
        Value::Text(text) => serde_json::Value::String(text.clone()),
    };
    fields
        .iter()
        .map(|(name, cell)| {
            let value = match cell {
                Cell::One(value) => scalar(value),
                Cell::Set(members) => members.iter().map(scalar).collect(),
            };
            (name.clone(), value)
        })
        .collect()
}

/// Why the store refused a write or a peer's changes.
#[derive(Debug)]
pub enum StoreError {
    /// No resource of this kind has this id.
    Missing { kind: String, id: String },
    /// The document refused the change, or what was given as one is none.
    Document(automerge::AutomergeError),
    /// A read was given where a write was asked for.
    NotAWrite,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { kind, id } => write!(f, "there is no {kind} `{id}`"),
            Self::Document(error) => write!(f, "the document refused the change: {error}"),
            Self::NotAWrite => write!(f, "that is a read, not a write"),
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resource {
    /// Its reader set: whatever names who may read it, which also picks the key of the
    /// document it is placed in.
    pub readers: String,
    pub parent: Option<String>,
    pub order: Option<String>,
    pub fields: BTreeMap<String, Cell>,
}

/// Where a removed resource was: the document it was last in, and its parent then.
#[derive(Clone, Debug)]
struct Removed {
    doc: String,
    parent: Option<String>,
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
pub type Source = Box<dyn FnMut(&str) -> Option<Vec<u8>> + Send>;

/// The resource store. See the module docs.
#[derive(Default)]
pub struct Store {
    /// The documents in memory.
    docs: HashMap<String, Arc<Automerge>>,
    filled: HashMap<(String, String), Filled>,
    index: Index,
    source: Option<Source>,
    /// Kinds an open query watches, by its watch.
    watches: BTreeMap<u64, String>,
    changed: BTreeSet<u64>,
    /// The resource types every app declared: what a peer's change is checked against.
    schema: Schema,
    /// Per document, the changes it refused: kept beside it, never applied, and refusing
    /// every change built on them.
    refused: HashMap<String, BTreeSet<ChangeHash>>,
    /// Per document, changes received before the ones they are built on.
    waiting: HashMap<String, Vec<Received>>,
    /// Each resource removed since the store started, by kind and id: the document it was last
    /// in, where its history is read now that the index holds it no more, and the resource it
    /// was nested in.
    removed: HashMap<(String, String), Removed>,
    /// The resources removed from each parent, by kind and id.
    removed_under: HashMap<String, BTreeSet<(String, String)>>,
    /// The documents the resources nested in each parent are placed in, by the parent.
    nested_in: HashMap<String, BTreeSet<String>>,
    /// Per group of invocations ([`Stamp::group`]), the documents a change of it is in: what
    /// reading the group's changes reads, rather than every document.
    groups: HashMap<String, BTreeSet<String>>,
    /// Per resource something was made from ([`Stamp::from`]), the documents a change stamped
    /// as made from it is in.
    made_from: HashMap<String, BTreeSet<String>>,
    /// Per document, the versions of each resource in it, read as far as its changes go.
    touches: std::sync::Mutex<HashMap<String, crate::history::Touches>>,
    /// Per resource, the time of the latest change that made a resource in it or removed one
    /// from it: a change of it too, which its [`UPDATED`] reflects without reading the
    /// history of what it holds.
    nested_at: HashMap<String, String>,
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

    /// Send a command against a private copy of the store. The command can follow the placement,
    /// index and undo history of its earlier steps without publishing any of them.
    pub(crate) fn stage<T>(
        &mut self,
        scope: &crate::wire::DraftScope,
        command: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let (index, filled) = if scope.undo {
            (self.index.clone(), self.filled.clone())
        } else {
            let filled = scope
                .placements
                .iter()
                .filter_map(|key| self.filled.get(key).map(|filled| (key.clone(), *filled)))
                .collect();
            (self.index.selected(&scope.resources), filled)
        };
        let mut staged = Self {
            docs: self.docs.clone(),
            filled,
            index,
            source: self.source.take(),
            ..Self::default()
        };
        let result = command(&mut staged);
        self.source = staged.source.take();
        result
    }

    /// Check a staged write against the live document and the same schema a received write uses.
    ///
    /// # Errors
    /// The change has no local base or the resource schema refuses it.
    pub fn check_write(&mut self, write: &Written, author: [u8; 32]) -> Result<(), Refusal> {
        let doc = self.doc(&write.doc).map_err(|_| Refusal::Unplaced)?;
        if write.change.deps().iter().any(|dep| !doc.has_change(dep)) {
            return Err(Refusal::Unplaced);
        }
        let received = Received {
            change: write.change.clone(),
            author,
        };
        self.judge(
            &write.doc,
            &write.readers,
            &received,
            None,
            &|_: &Proposal<'_>| true,
        )
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

    /// How many resources `query` lists, of those whose readers `may_read` admits.
    #[must_use]
    pub fn len(&self, query: &Query, may_read: &dyn Fn(&str) -> bool) -> usize {
        self.index.len(query, may_read)
    }

    /// The page of `query` starting at `position` in its listing. See [`Index::at`].
    #[must_use]
    pub fn at(&self, query: &Query, position: usize, may_read: &dyn Fn(&str) -> bool) -> Page {
        self.index.at(query, position, may_read)
    }

    /// Where the resource `id` stands in the listing of `query`. See [`Index::position`].
    #[must_use]
    pub fn position(
        &self,
        query: &Query,
        id: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Option<usize> {
        self.index.position(query, id, may_read)
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

    /// Notes that the resource `id` of `kind`, nested in `parent`, was removed from `doc`.
    fn note_removed(&mut self, kind: &str, id: &str, doc: &str, parent: Option<String>) {
        let key = (kind.to_owned(), id.to_owned());
        if let Some(parent) = &parent {
            self.removed_under
                .entry(parent.clone())
                .or_default()
                .insert(key.clone());
        }
        let earlier = self.removed.insert(
            key.clone(),
            Removed {
                doc: doc.to_owned(),
                parent,
            },
        );
        if let Some(parent) = earlier.and_then(|earlier| earlier.parent)
            && self.removed.get(&key).and_then(|now| now.parent.as_ref()) != Some(&parent)
            && let Some(under) = self.removed_under.get_mut(&parent)
        {
            under.remove(&key);
        }
    }

    /// The resource `id` of `kind` is held again: it is no longer a removed one.
    fn unremove(&mut self, kind: &str, id: &str) {
        let key = (kind.to_owned(), id.to_owned());
        if let Some(parent) = self.removed.remove(&key).and_then(|removed| removed.parent)
            && let Some(under) = self.removed_under.get_mut(&parent)
        {
            under.remove(&key);
        }
    }

    /// The ids of the resources of `kind` removed since the store started, nested in `parent`
    /// when it names one, whose readers `may_read` admits.
    #[must_use]
    pub fn removed(
        &self,
        kind: &str,
        parent: Option<&str>,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Vec<String> {
        let readable = |key: &(String, String)| {
            self.removed
                .get(key)
                .and_then(|removed| self.index.readers(&removed.doc))
                .is_some_and(may_read)
        };
        let mut ids: Vec<String> = match parent {
            Some(parent) => self
                .removed_under
                .get(parent)
                .into_iter()
                .flatten()
                .filter(|key| key.0 == kind && readable(key))
                .map(|(_, id)| id.clone())
                .collect(),
            None => self
                .removed
                .keys()
                .filter(|key| key.0 == kind && readable(key))
                .map(|(_, id)| id.clone())
                .collect(),
        };
        ids.sort();
        ids
    }

    /// The holders of what `changes`, to the document `doc`, make or remove, each noted as
    /// changed when that happened if it is later than it was: what was made in a resource or
    /// removed from it moves when it last changed.
    fn moved_holders(&mut self, doc: &str, changes: &[Change]) -> BTreeSet<String> {
        let mut holders: BTreeSet<String> = BTreeSet::new();
        for change in changes {
            let (holder, at) = match change {
                Change::Put {
                    kind,
                    id,
                    parent: Some(parent),
                    fields,
                    ..
                } if self.index.get(kind, id).is_none() => (
                    parent.clone(),
                    fields.get(CREATED).and_then(|cell| match cell {
                        Cell::One(Value::Text(at)) => Some(at.clone()),
                        _ => None,
                    }),
                ),
                Change::Remove { kind, id } => {
                    let Some(parent) = self
                        .index
                        .get(kind, id)
                        .and_then(|held| held.parent.clone())
                    else {
                        continue;
                    };
                    (parent, self.removed_at(doc, id))
                }
                Change::Put { .. } => continue,
            };
            let Some(at) = at.filter(|at| !at.is_empty()) else {
                continue;
            };
            let later = self.nested_at.get(&holder).is_none_or(|was| *was < at);
            if later {
                self.nested_at.insert(holder.clone(), at);
                holders.insert(holder);
            }
        }
        holders
    }

    /// Hands `commit` to the index and notes the watches it concerns. Each resource it puts is
    /// indexed with the times its history gives it ([`Self::reflected`]).
    fn commit(&mut self, commit: &Commit<'_>) {
        let mut changes: Vec<Change> = commit
            .changes
            .iter()
            .map(|change| match change {
                Change::Put {
                    kind,
                    id,
                    parent,
                    order,
                    fields,
                } => {
                    let mut fields = fields.clone();
                    fields.extend(self.reflected(commit.doc, commit.readers, kind, id));
                    Change::Put {
                        kind: kind.clone(),
                        id: id.clone(),
                        parent: parent.clone(),
                        order: order.clone(),
                        fields,
                    }
                }
                Change::Remove { .. } => change.clone(),
            })
            .collect();
        let mut holders = self.moved_holders(commit.doc, &changes);
        // A holder this commit puts itself is put with the time that moved.
        for change in &mut changes {
            if let Change::Put { id, fields, .. } = change
                && holders.remove(id.as_str())
                && let Some(nested) = self.nested_at.get(id.as_str())
            {
                let own = fields.get(UPDATED).and_then(|cell| match cell {
                    Cell::One(Value::Text(at)) => Some(at.as_str()),
                    _ => None,
                });
                if own.is_none_or(|own| own < nested.as_str()) {
                    fields.insert(UPDATED.to_owned(), Cell::One(Value::Text(nested.clone())));
                }
            }
        }
        let commit = &Commit {
            changes: &changes,
            ..*commit
        };
        for change in commit.changes {
            let kind = match change {
                Change::Put { kind, id, .. } => {
                    self.unremove(kind, id);
                    kind
                }
                Change::Remove { kind, id } => {
                    let parent = self
                        .index
                        .get(kind, id)
                        .and_then(|held| held.parent.clone());
                    self.note_removed(kind, id, commit.doc, parent);
                    kind
                }
            };
            for (id, watched) in &self.watches {
                if watched == kind {
                    self.changed.insert(*id);
                }
            }
        }
        self.index.committed(commit);
        // Each holder of what was made or removed, put again with the time that moved; one
        // this commit puts itself already reads it.
        for holder in holders {
            let Some(kind) = self.index.kind_anywhere(&holder) else {
                continue;
            };
            let Some(held) = self.index.get(&kind, &holder).cloned() else {
                continue;
            };
            let Some(readers) = self.index.readers(&held.doc).map(str::to_owned) else {
                continue;
            };
            let heads = self.index.heads(&held.doc).unwrap_or_default().to_vec();
            let mut fields = held.fields;
            fields.remove(UPDATED);
            self.commit(&Commit {
                doc: &held.doc,
                readers: &readers,
                heads: &heads,
                changes: &[Change::Put {
                    kind,
                    id: holder,
                    parent: held.parent,
                    order: held.order,
                    fields,
                }],
            });
        }
    }

    /// The time of the version of `doc` that removed `id`, as its stamp says.
    fn removed_at(&self, doc: &str, id: &str) -> Option<String> {
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = touches.entry(doc.to_owned()).or_default();
        if let Some(read) = self.docs.get(doc) {
            held.read(read, doc);
        }
        let at = held
            .versions_of(id)?
            .iter()
            .rev()
            .find(|version| version.kind == crate::history::VersionKind::Removed)?
            .stamp
            .as_ref()
            .map(|stamp| stamp.at.clone());
        drop(touches);
        at
    }

    /// The times the history of the resource `id` of `kind`, in the document `doc_id` read by
    /// `readers`, gives it, as the fields a query filters and orders by: when it was made
    /// ([`CREATED`]), when it last changed, its content included ([`UPDATED`]), and when each of
    /// its fields last changed ([`CHANGED`] and the field's name), or when it was made for one no
    /// change after that wrote. Each is its version's time, as the stamp of the action that wrote
    /// it says. Reading them reads only the changes the documents took since they were last read.
    fn reflected(&self, doc_id: &str, readers: &str, kind: &str, id: &str) -> Vec<(String, Cell)> {
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut made: Option<&crate::history::Version> = None;
        let mut last: Option<&crate::history::Version> = None;
        let mut changed: BTreeMap<&str, &crate::history::Version> = BTreeMap::new();
        let content = content_id(readers, kind, id);
        for doc in [doc_id, content.as_str()] {
            if let Some(held) = self.docs.get(doc) {
                touches.entry(doc.to_owned()).or_default().read(held, doc);
            }
        }
        for (doc, of_fields) in [(doc_id, true), (content.as_str(), false)] {
            let Some(versions) = touches.get(doc).and_then(|held| held.versions_of(id)) else {
                continue;
            };
            for version in versions {
                if version
                    .stamp
                    .as_ref()
                    .is_none_or(|stamp| stamp.at.is_empty())
                {
                    continue;
                }
                let newer = |held: Option<&crate::history::Version>| {
                    held.is_none_or(|held| held.order() <= version.order())
                };
                if version.kind == crate::history::VersionKind::Made && made.is_none() {
                    made = Some(version);
                }
                if newer(last) {
                    last = Some(version);
                }
                if of_fields {
                    for field in &version.fields {
                        if newer(changed.get(field.as_str()).copied()) {
                            changed.insert(field, version);
                        }
                    }
                }
            }
        }
        let at = |version: &crate::history::Version| {
            Cell::One(Value::Text(
                version
                    .stamp
                    .as_ref()
                    .map(|stamp| stamp.at.clone())
                    .unwrap_or_default(),
            ))
        };
        let made = made.map(&at);
        let last = last.map(&at);
        let changed: Vec<(String, Cell)> = changed
            .into_iter()
            .map(|(field, version)| (format!("{CHANGED}{field}"), at(version)))
            .collect();
        drop(touches);
        let mut out = Vec::new();
        if let Some(made) = made {
            out.push((CREATED.to_owned(), made));
        }
        // What was made in it or removed from it changed it too.
        let updated = match (last, self.nested_at.get(id)) {
            (Some(Cell::One(Value::Text(own))), Some(nested)) if *nested > own => {
                Some(Cell::One(Value::Text(nested.clone())))
            }
            (None, Some(nested)) => Some(Cell::One(Value::Text(nested.clone()))),
            (own, _) => own,
        };
        if let Some(updated) = updated {
            out.push((UPDATED.to_owned(), updated));
        }
        out.extend(changed);
        out
    }

    /// The document `doc`, read from the source when it is not in memory, made when it is
    /// nowhere.
    fn doc(&mut self, doc: &str) -> Result<&mut Automerge, StoreError> {
        if !self.docs.contains_key(doc) {
            let read = match self.source.as_mut().and_then(|source| source(doc)) {
                Some(bytes) => Automerge::load(&bytes)?,
                None => Automerge::new(),
            };
            self.docs.insert(doc.to_owned(), Arc::new(read));
        }
        self.docs
            .get_mut(doc)
            .map(Arc::make_mut)
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
        let doc = format!(
            "{}/{owner}#{}",
            collab_types::reader_set_name(readers),
            filled.newest
        );
        if parent.is_some() {
            self.nested_in.entry(owner).or_default().insert(doc.clone());
        }
        doc
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
        stamp: &Stamp,
    ) -> Result<Vec<Written>, StoreError> {
        let mut written = Vec::with_capacity(1);
        let held = self.index.get(kind, id).map(|held| held.doc.clone());
        let doc_id = match held {
            Some(doc) if self.index.readers(&doc) == Some(resource.readers.as_str()) => doc,
            Some(doc) => {
                let earlier = self.index.readers(&doc).unwrap_or_default().to_owned();
                written.push(self.remove(kind, id, stamp)?);
                written.extend(self.move_content(kind, id, &earlier, &resource.readers, stamp)?);
                self.place(kind, resource.parent.as_deref(), &resource.readers)
            }
            None => self.place(kind, resource.parent.as_deref(), &resource.readers),
        };
        let doc = self.doc(&doc_id)?;
        // A put of what the document holds already changes nothing, and writes nothing.
        let change = put_fields(doc, kind, id, &resource, stamp)?;
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
        let Some(change) = change else {
            return Ok(written);
        };
        self.note_group(&doc_id, stamp);
        written.push(Written {
            doc: doc_id,
            readers: resource.readers,
            change,
        });
        Ok(written)
    }

    /// Drafts a field update on an existing resource without changing its document or index.
    /// Its permissions and declared write-set are checked against the actual change.
    ///
    /// # Errors
    /// The resource is missing, its placement changes, or admission refuses the update.
    pub fn draft_field_update(
        &mut self,
        (kind, id): (&str, &str),
        resource: &Resource,
        stamp: &Stamp,
        author: [u8; 32],
        schema: Option<&Schema>,
        admit: &dyn Admit,
    ) -> Result<Option<Written>, String> {
        let held = self
            .index
            .get(kind, id)
            .ok_or("the resource does not exist")?;
        let doc = held.doc.clone();
        if self.index.readers(&doc) != Some(resource.readers.as_str())
            || held.parent != resource.parent
            || held.order != resource.order
        {
            return Err("an update must preserve the resource's placement and readers".into());
        }
        let mut draft = self.doc(&doc).map_err(|error| error.to_string())?.clone();
        // The hosted device supplies its own author even when the live store last
        // wrote the document locally.
        draft.set_actor(automerge::ActorId::from(author.as_slice()));
        let Some(change) =
            put_fields(&mut draft, kind, id, resource, stamp).map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let received = Received { change, author };
        self.judge(&doc, &resource.readers, &received, schema, admit)
            .map_err(|reason| format!("the resource change was refused: {reason:?}"))?;
        Ok(Some(Written {
            doc,
            readers: resource.readers.clone(),
            change: received.change,
        }))
    }

    /// Removes the resource `id` of `kind`, and with it nothing else: the resources nested in it
    /// are the caller's to remove first.
    ///
    /// # Errors
    /// When there is no such resource, or its document refuses the change.
    pub fn remove(&mut self, kind: &str, id: &str, stamp: &Stamp) -> Result<Written, StoreError> {
        let missing = || StoreError::Missing {
            kind: kind.to_owned(),
            id: id.to_owned(),
        };
        let doc_id = self.index.get(kind, id).ok_or_else(missing)?.doc.clone();
        let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
        let doc = self.doc(&doc_id)?;
        let mut tx = doc.transaction();
        if tx.get(ROOT, held_key(id))?.is_some() {
            tx.delete(ROOT, held_key(id))?;
        }
        tx.commit_with(CommitOptions::default().with_message(stamp.message()));
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
        self.note_group(&doc_id, stamp);
        Ok(Written {
            doc: doc_id,
            readers,
            change,
        })
    }

    /// Takes back the invocation `invocation`, in every document it changed: what its changes
    /// did, and nothing a later change did besides. A resource it made is removed, one it removed
    /// is put back, a field or place it changed holds what it held before, and a part of a
    /// content it changed holds what it held before. Written as `stamp`: an invocation of its
    /// own, which names the verb it takes back and the invocation in `undoes`, so a holder
    /// admits it as that verb and undoing it takes this back.
    ///
    /// # Errors
    /// When a document cannot be read, or refuses the change.
    pub fn undo(&mut self, invocation: &str, stamp: &Stamp) -> Result<Vec<Written>, StoreError> {
        // The resources first, then their contents: a resource the undo puts back holds its
        // content again, and the content of one it removes goes with it.
        let mut docs: Vec<String> = self.index.documents().map(str::to_owned).collect();
        docs.sort_by_key(|doc| crate::admit::content_resource(doc).is_some());
        let mut written = Vec::new();
        for doc_id in docs {
            let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
            let Some(doc) = self.existing(&doc_id)? else {
                continue;
            };
            let Some(span) = crate::undo::Span::of(doc, invocation) else {
                continue;
            };
            let was = doc.fork_at(&span.before)?;
            let became = doc.fork_at(&span.after)?;
            if let Some((kind, id)) = crate::admit::content_resource(&doc_id) {
                if self.index.get(&kind, &id).is_none() {
                    continue;
                }
                let (was_map, became_map) = (
                    ReadDoc::hydrate(&was, &ROOT, None)?,
                    ReadDoc::hydrate(&became, &ROOT, None)?,
                );
                let changed: Vec<String> = was
                    .keys(ROOT)
                    .chain(became.keys(ROOT))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter(|key| hydrated_key(&was_map, key) != hydrated_key(&became_map, key))
                    .collect();
                if changed.is_empty() {
                    continue;
                }
                let ((), change) = self.edit_content(&kind, &id, stamp, |tx, root| {
                    for key in changed {
                        crate::undo::undo_key(&span, (&was, &became), &key, tx, root)?;
                    }
                    Ok(())
                })?;
                written.extend(change);
                continue;
            }
            let ids: BTreeSet<String> = was
                .keys(ROOT)
                .chain(became.keys(ROOT))
                .filter_map(|key| key.strip_prefix(HELD).map(str::to_owned))
                .collect();
            let steps: Vec<_> = ids
                .into_iter()
                .filter_map(|id| {
                    let (was, became) = (read(&was, &id), read(&became, &id));
                    (was != became).then(|| (read(doc, &id), was, became, id))
                })
                .collect();
            for (now, was, became, id) in steps {
                match (was, now) {
                    (None, Some((kind, _))) => written.push(self.remove(&kind, &id, stamp)?),
                    (Some((kind, was)), None) => written.extend(self.put(
                        &kind,
                        &id,
                        Resource {
                            readers: readers.clone(),
                            ..was
                        },
                        stamp,
                    )?),
                    (Some((kind, was)), Some((_, mut now))) => {
                        let became = became.map(|(_, became)| became).unwrap_or_default();
                        let names: BTreeSet<&String> =
                            was.fields.keys().chain(became.fields.keys()).collect();
                        for name in names {
                            if was.fields.get(name) == became.fields.get(name) {
                                continue;
                            }
                            match was.fields.get(name) {
                                Some(cell) => now.fields.insert(name.clone(), cell.clone()),
                                None => now.fields.remove(name),
                            };
                        }
                        if was.parent != became.parent {
                            now.parent.clone_from(&was.parent);
                        }
                        if was.order != became.order {
                            now.order.clone_from(&was.order);
                        }
                        now.readers.clone_from(&readers);
                        written.extend(self.put(&kind, &id, now, stamp)?);
                    }
                    (None, None) => {}
                }
            }
        }
        Ok(written)
    }

    /// The document `doc` when it exists, in memory or at the source; `None` for one never
    /// written.
    fn existing(&mut self, doc: &str) -> Result<Option<&mut Automerge>, StoreError> {
        if !self.docs.contains_key(doc) {
            let Some(bytes) = self.source.as_mut().and_then(|source| source(doc)) else {
                return Ok(None);
            };
            self.docs
                .insert(doc.to_owned(), Arc::new(Automerge::load(&bytes)?));
        }
        Ok(self.docs.get_mut(doc).map(Arc::make_mut))
    }

    /// The content document of the resource `id` of `kind`, and its reader set.
    fn content_of(&self, kind: &str, id: &str) -> Result<(String, String), StoreError> {
        let held = self
            .index
            .get(kind, id)
            .ok_or_else(|| StoreError::Missing {
                kind: kind.to_owned(),
                id: id.to_owned(),
            })?;
        let readers = self.index.readers(&held.doc).unwrap_or_default().to_owned();
        Ok((content_id(&readers, kind, id), readers))
    }

    /// The content of the resource `id` of `kind`: the document it is in and the map at its
    /// root, `None` while nothing was written to it.
    ///
    /// # Errors
    /// When there is no such resource, or its content document cannot be read.
    pub fn content(
        &mut self,
        kind: &str,
        id: &str,
    ) -> Result<Option<(&Automerge, ObjId)>, StoreError> {
        let (doc_id, _) = self.content_of(kind, id)?;
        let Some(doc) = self.existing(&doc_id)? else {
            return Ok(None);
        };
        let doc: &Automerge = doc;
        Ok((doc.length(ROOT) > 0).then_some((doc, ROOT)))
    }

    /// Edits the content of the resource `id` of `kind` in one change: `edit` is given the map
    /// at its root. Answers what `edit` answered and the change, `None` when it changed nothing.
    ///
    /// # Errors
    /// When there is no such resource, `edit` failed (and nothing was written), or the content
    /// document cannot be read.
    pub fn edit_content<R>(
        &mut self,
        kind: &str,
        id: &str,
        stamp: &Stamp,
        edit: impl FnOnce(
            &mut automerge::transaction::Transaction<'_>,
            &ObjId,
        ) -> Result<R, automerge::AutomergeError>,
    ) -> Result<(R, Option<Written>), StoreError> {
        let (doc_id, readers) = self.content_of(kind, id)?;
        let doc = self.doc(&doc_id)?;
        let before = doc.get_heads();
        let mut tx = doc.transaction();
        let answer = edit(&mut tx, &ROOT);
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                tx.rollback();
                return Err(error.into());
            }
        };
        tx.commit_with(CommitOptions::default().with_message(stamp.message()));
        if doc.get_heads() == before {
            return Ok((answer, None));
        }
        let change = doc
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.index.committed(&Commit {
            doc: &doc_id,
            readers: &readers,
            heads: &heads,
            changes: &[],
        });
        // The resource changed when its content did: it is indexed again with the time.
        if let Some(held) = self.index.get(kind, id).cloned() {
            let heads = self
                .docs
                .get(&held.doc)
                .map(|doc| doc.get_heads())
                .unwrap_or_default();
            self.commit(&Commit {
                doc: &held.doc,
                readers: &readers,
                heads: &heads,
                changes: &[Change::Put {
                    kind: kind.to_owned(),
                    id: id.to_owned(),
                    parent: held.parent.clone(),
                    order: held.order.clone(),
                    fields: held.fields,
                }],
            });
        }
        self.note_group(&doc_id, stamp);
        Ok((
            answer,
            Some(Written {
                doc: doc_id,
                readers,
                change,
            }),
        ))
    }

    /// Moves the content of the resource `id` of `kind` from the reader set `from` to `to`:
    /// what it holds now, without its history, into a content document of `to`, and the old one
    /// emptied. Answers both changes; none for a resource without content.
    fn move_content(
        &mut self,
        kind: &str,
        id: &str,
        from: &str,
        to: &str,
        stamp: &Stamp,
    ) -> Result<Vec<Written>, StoreError> {
        let old_id = content_id(from, kind, id);
        let Some(old) = self.existing(&old_id)? else {
            return Ok(Vec::new());
        };
        if old.length(ROOT) == 0 {
            return Ok(Vec::new());
        }
        let copy = old.fork();
        let mut tx = old.transaction();
        let keys: Vec<String> = tx.keys(ROOT).collect();
        for key in keys {
            tx.delete(ROOT, key.as_str())?;
        }
        tx.commit_with(CommitOptions::default().with_message(stamp.message()));
        let emptied = old
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let new_id = content_id(to, kind, id);
        let new = self.doc(&new_id)?;
        let mut tx = new.transaction();
        copy_object(&copy, &ROOT, &mut tx, &ROOT)?;
        tx.commit_with(CommitOptions::default().with_message(stamp.message()));
        let copied = new
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        self.note_group(&old_id, stamp);
        self.note_group(&new_id, stamp);
        Ok(vec![
            Written {
                doc: old_id,
                readers: from.to_owned(),
                change: emptied,
            },
            Written {
                doc: new_id,
                readers: to.to_owned(),
                change: copied,
            },
        ])
    }

    /// Applies a peer's `changes` to the document `doc`, which `readers` may read, and indexes
    /// the resources they touched.
    ///
    /// # Errors
    /// When the document cannot be read, or refuses a change admitted to it.
    pub fn receive(
        &mut self,
        doc_id: &str,
        readers: &str,
        changes: Vec<Received>,
        admit: &dyn Admit,
    ) -> Result<Vec<(ChangeHash, Refusal)>, StoreError> {
        let mut pending: Vec<Received> = self.waiting.remove(doc_id).unwrap_or_default();
        pending.extend(changes);
        let mut refusals = Vec::new();
        // Each pass takes every change whose earlier changes are here; it ends when one takes
        // none.
        loop {
            let mut waiting = Vec::new();
            let mut took = false;
            for received in pending {
                let hash = received.change.hash();
                let refused = self.refused.entry(doc_id.to_owned()).or_default();
                if refused.contains(&hash) {
                    continue;
                }
                if received
                    .change
                    .deps()
                    .iter()
                    .any(|dep| refused.contains(dep))
                {
                    refused.insert(hash);
                    refusals.push((hash, Refusal::OnRefused));
                    took = true;
                    continue;
                }
                let doc = self.doc(doc_id)?;
                if doc.has_change(&hash) {
                    continue;
                }
                if received
                    .change
                    .deps()
                    .iter()
                    .any(|dep| !doc.has_change(dep))
                {
                    waiting.push(received);
                    continue;
                }
                took = true;
                if let Err(refusal) = self.judge(doc_id, readers, &received, None, admit) {
                    self.refused
                        .entry(doc_id.to_owned())
                        .or_default()
                        .insert(hash);
                    refusals.push((hash, refusal));
                    continue;
                }
                self.apply(doc_id, readers, received.change)?;
            }
            pending = waiting;
            if !took || pending.is_empty() {
                break;
            }
        }
        if !pending.is_empty() {
            self.waiting.insert(doc_id.to_owned(), pending);
        }
        Ok(refusals)
    }

    /// The changes `doc` refused: kept beside it, without effect.
    pub fn refused(&self, doc: &str) -> impl Iterator<Item = &ChangeHash> {
        self.refused.get(doc).into_iter().flatten()
    }

    /// The content of the resource `id` of `kind`, as JSON; `None` while nothing was written to
    /// it, or its document is not in memory.
    #[must_use]
    pub fn content_json(&self, kind: &str, id: &str) -> Option<serde_json::Value> {
        let (doc_id, _) = self.content_of(kind, id).ok()?;
        let doc = self.docs.get(&doc_id)?;
        (doc.length(ROOT) > 0).then(|| crate::wire::json_of(doc, &ROOT))
    }

    /// What the change `hash` of the document `doc_id` wrote: each resource it made or changed,
    /// by kind and id, with what it holds as the change left it ([`Wrote`]). A change of a
    /// resource's content names that resource; a change of a document of resources names each
    /// resource whose entry it changed. None for a document not in memory, or a change it lacks.
    #[must_use]
    pub fn wrote(&self, doc_id: &str, hash: ChangeHash) -> Vec<Wrote> {
        let Some(doc) = self.docs.get(doc_id) else {
            return Vec::new();
        };
        let Some(change) = doc.get_change_by_hash(&hash) else {
            return Vec::new();
        };
        let after = [hash];
        let now = crate::wire::AsOf {
            doc,
            heads: Some(&after),
        };
        if let Some((kind, id)) = content_resource(doc_id) {
            let content = now.json(&ROOT);
            return vec![self.wrote_one(kind, id, None, content)];
        }
        let before = crate::wire::AsOf {
            doc,
            heads: Some(change.deps()),
        };
        let entry = |look: &crate::wire::AsOf<'_>, key: &str| {
            look.get(&ROOT, key)
                .map(|(_, obj)| look.json(&obj))
                .unwrap_or_default()
        };
        now.keys(&ROOT)
            .into_iter()
            .filter_map(|key| {
                let id = key.strip_prefix(HELD)?;
                let held = entry(&now, &key);
                (held != entry(&before, &key)).then_some((id.to_owned(), held))
            })
            .filter_map(|(id, held)| {
                let kind = held.get(KIND)?.as_str()?.to_owned();
                let fields = read_as_of(doc, Some(&after), &id).map(|(_, resource)| resource);
                let content = self.content_json(&kind, &id).unwrap_or_default();
                Some(self.wrote_one(&kind, &id, fields, content))
            })
            .collect()
    }

    /// One resource [`Self::wrote`] names: its indexed fields (`fields`, or as the index holds
    /// them now), its `content` beside them, and the resource it is nested in with what that
    /// holds.
    fn wrote_one(
        &self,
        kind: &str,
        id: &str,
        fields: Option<Resource>,
        content: serde_json::Value,
    ) -> Wrote {
        let indexed = self.index.get(kind, id);
        let (fields, parent) = match fields {
            Some(resource) => (resource.fields, resource.parent),
            None => (
                indexed.map(|held| held.fields.clone()).unwrap_or_default(),
                indexed.and_then(|held| held.parent.clone()),
            ),
        };
        let mut holds = fields_json(&fields);
        if let serde_json::Value::Object(content) = content {
            holds.extend(content);
        }
        let parent = parent.map(|parent| {
            let kind = self.index.kind_anywhere(&parent).unwrap_or_default();
            let fields = self
                .index
                .get(&kind, &parent)
                .map(|held| fields_json(&held.fields))
                .unwrap_or_default();
            (kind, parent, fields)
        });
        Wrote {
            kind: kind.to_owned(),
            id: id.to_owned(),
            holds,
            parent,
        }
    }

    /// The versions of the resource `id` of `kind`, oldest first: each change that made, changed
    /// or removed it, or changed its content, as its action stamped it. One whose document is
    /// not in memory has none.
    #[must_use]
    pub fn versions(&self, kind: &str, id: &str) -> Vec<crate::history::Version> {
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.versions_unlocked(&mut touches, kind, id)
    }

    /// The first version of the resource `id` of `kind`, or its last with `last`. The versions
    /// it is picked from are not sent, so a resource whose history names thousands of nested
    /// resources (a root, through each change that put items in it) answers with the one.
    #[must_use]
    pub fn version_end(&self, kind: &str, id: &str, last: bool) -> Option<crate::history::Version> {
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (held, readers) = self.placed(kind, id)?;
        let order = |version: &crate::history::Version| {
            (
                version.stamp.as_ref().map_or(0, |stamp| stamp.moment),
                version.op,
            )
        };
        let mut ends: Vec<crate::history::Version> = Vec::new();
        let content = content_id(&readers, kind, id);
        for doc_id in [&held, &content] {
            if let Some(doc) = self.docs.get(doc_id) {
                let read = touches.entry(doc_id.clone()).or_default();
                read.read(doc, doc_id);
                // A document's changes stand in the order it took them in, which on a replica
                // that took another's change after its own is not when they were written.
                let own = read.versions_of(id).map(Vec::as_slice).unwrap_or_default();
                ends.extend(
                    if last {
                        own.iter().max_by_key(|version| order(version))
                    } else {
                        own.first()
                    }
                    .cloned(),
                );
            }
        }
        let none = BTreeSet::new();
        let removed_here = self.removed_under.get(id).unwrap_or(&none);
        for doc_id in self.nested_in.get(id).into_iter().flatten() {
            if let Some(doc) = self.docs.get(doc_id) {
                ends.extend(touches.entry(doc_id.clone()).or_default().nested_end(
                    doc,
                    doc_id,
                    id,
                    removed_here,
                    last,
                ));
            }
        }
        drop(touches);
        // As [`Self::versions`] orders them: by when their writer wrote them, a document's own
        // in its order, so the same one ends it.
        // The first is its making, whatever its content's document numbers that change.
        let mut end = if last {
            ends.into_iter().max_by_key(order)
        } else {
            ends.into_iter().min_by_key(|version| {
                (
                    version.kind != crate::history::VersionKind::Made,
                    order(version),
                )
            })
        }?;
        if last
            && end.kind == crate::history::VersionKind::Removed
            && let Some(removed) = self.removed.get(&(kind.to_owned(), id.to_owned()))
        {
            end.parent.clone_from(&removed.parent);
        }
        Some(end)
    }

    /// [`Self::versions`], with the cache of what each document's changes touched at hand.
    fn versions_unlocked(
        &self,
        touches: &mut HashMap<String, crate::history::Touches>,
        kind: &str,
        id: &str,
    ) -> Vec<crate::history::Version> {
        let mut versions = Vec::new();
        let Some((held, readers)) = self.placed(kind, id) else {
            return versions;
        };
        if let Some(doc) = self.docs.get(&held) {
            versions.extend(touches.entry(held.clone()).or_default().of(doc, &held, id));
        }
        let content = content_id(&readers, kind, id);
        if let Some(doc) = self.docs.get(&content) {
            versions.extend(
                touches
                    .entry(content.clone())
                    .or_default()
                    .of(doc, &content, id),
            );
        }
        // A change that made or removed a resource nested in it is a version of it too: what a
        // collection of it holds changed.
        let none = BTreeSet::new();
        let removed_here = self.removed_under.get(id).unwrap_or(&none);
        for doc_id in self.nested_in.get(id).into_iter().flatten() {
            if let Some(doc) = self.docs.get(doc_id) {
                versions.extend(touches.entry(doc_id.clone()).or_default().nested(
                    doc,
                    doc_id,
                    id,
                    removed_here,
                ));
            }
        }
        // Each document's changes in its own order, and the documents' interleaved by when their
        // writer wrote them.
        versions.sort_by_key(|version| {
            (
                version.stamp.as_ref().map_or(0, |stamp| stamp.moment),
                version.op,
            )
        });
        // Where a removed resource stood, which its removal says.
        if let Some(removed) = self.removed.get(&(kind.to_owned(), id.to_owned()))
            && let Some(last) = versions
                .iter_mut()
                .rev()
                .find(|version| version.kind == crate::history::VersionKind::Removed)
        {
            last.parent.clone_from(&removed.parent);
        }
        versions
    }

    /// The resource's versions, including documents evicted or indexed from snapshots: each is
    /// read back from the source first. The caller checks the resource's readers before asking
    /// for its history.
    ///
    /// # Errors
    /// A saved document does not decode.
    pub fn read_versions(
        &mut self,
        kind: &str,
        id: &str,
    ) -> Result<Vec<crate::history::Version>, StoreError> {
        self.read_back_history(kind, id)?;
        Ok(self.versions(kind, id))
    }

    /// Reads back from the source each document the history of the resource `id` of `kind` is
    /// read from: where it is stored, its content, and those it holds nested resources in.
    fn read_back_history(&mut self, kind: &str, id: &str) -> Result<(), StoreError> {
        let mut wanted: Vec<String> = Vec::new();
        if let Some((held, readers)) = self.placed(kind, id) {
            wanted.push(content_id(&readers, kind, id));
            wanted.push(held);
        }
        wanted.extend(self.nested_in.get(id).into_iter().flatten().cloned());
        for doc in wanted {
            self.existing(&doc)?;
        }
        Ok(())
    }

    /// [`Self::version_end`], with documents evicted or indexed from snapshots read back first,
    /// as [`Self::read_versions`] does.
    ///
    /// # Errors
    /// A saved document does not decode.
    pub fn read_version_end(
        &mut self,
        kind: &str,
        id: &str,
        last: bool,
    ) -> Result<Option<crate::history::Version>, StoreError> {
        self.read_back_history(kind, id)?;
        Ok(self.version_end(kind, id, last))
    }

    /// The newest version of each resource `within` names (it, and every resource whose id is
    /// under it, `within/…`, held or removed) whose readers `may_read` admits, of at most `limit`
    /// of them, most recent first, and whether there are more: what a window's catch-up lists
    /// and its mark counts. Each document is read once, as far as its changes go, rather than
    /// the history of every resource in it.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one walk over the documents, then the page it found, read in one pass"
    )]
    pub fn newest(
        &self,
        within: &str,
        limit: usize,
        may_read: &dyn Fn(&str) -> bool,
    ) -> (Vec<(String, String, crate::history::Version)>, bool) {
        let prefix = format!("{within}/");
        let under = |id: &str| id == within || id.starts_with(&prefix);
        let order = |version: &crate::history::Version| {
            (
                version.stamp.as_ref().map_or(0, |stamp| stamp.moment),
                version.op,
            )
        };
        let mut newest: Vec<(String, String, crate::history::Version)> = Vec::new();
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The documents that hold what `within` names, and the documents of their content: each
        // keeps its resources in the order of their newest version.
        let mut docs: Vec<(&str, Option<(&str, &str)>)> = Vec::new();
        for doc_id in self.index.documents() {
            if !self.index.readers(doc_id).is_some_and(may_read) {
                continue;
            }
            // `within` itself, by its own changes: what was made in it or removed from it is
            // a resource of the page, or a removed one, below.
            if let Some(kind) = self.index.kind_of(within, doc_id)
                && let Some(readers) = self.index.readers(doc_id)
            {
                let content = content_id(readers, &kind, within);
                let mut last: Option<crate::history::Version> = None;
                for doc in [doc_id, content.as_str()] {
                    let Some(held) = self.docs.get(doc) else {
                        continue;
                    };
                    let read = touches.entry(doc.to_owned()).or_default();
                    read.read(held, doc);
                    if let Some(version) = read.versions_of(within).and_then(|all| all.last())
                        && last.as_ref().is_none_or(|was| order(was) <= order(version))
                    {
                        last = Some(version.clone());
                    }
                }
                if let Some(last) = last {
                    newest.push((kind, within.to_owned(), last));
                }
            }
            docs.push((doc_id, None));
        }
        // What was written in a resource's content, in a document of its own.
        for content in self.docs.keys() {
            let Some((kind, id)) = content_resource(content) else {
                continue;
            };
            if !under(id) || id == within {
                continue;
            }
            let Some(held) = self.index.get(kind, id) else {
                continue;
            };
            if self.index.readers(&held.doc).is_some_and(may_read) {
                docs.push((content.as_str(), Some((kind, id))));
            }
        }
        for (doc_id, _) in &docs {
            if let Some(doc) = self.docs.get(*doc_id) {
                touches
                    .entry((*doc_id).to_owned())
                    .or_default()
                    .read(doc, doc_id);
            }
        }
        // The newest of each resource, most recent first, a page of them: the documents' orders
        // merged, each read only as far as the page goes.
        let mut found: Vec<(String, String, String)> = Vec::new();
        {
            let mut walks: Vec<_> = docs
                .iter()
                .filter_map(|(doc_id, content)| {
                    let held = touches.get(*doc_id)?;
                    Some((*doc_id, *content, held.by_newest().peekable()))
                })
                .collect();
            let mut next: std::collections::BinaryHeap<((u64, u64), usize)> = walks
                .iter_mut()
                .enumerate()
                .filter_map(|(at, (_, _, walk))| walk.peek().map(|(order, _, _)| (*order, at)))
                .collect();
            let mut seen = BTreeSet::new();
            while seen.len() <= limit {
                let Some((_, at)) = next.pop() else {
                    break;
                };
                let Some((doc_id, content, walk)) = walks.get_mut(at) else {
                    continue;
                };
                let Some((_, id, kind)) = walk.next() else {
                    continue;
                };
                if let Some((order, _, _)) = walk.peek() {
                    next.push((*order, at));
                }
                let kind = match content {
                    Some((kind, held)) if *held == id => (*kind).to_owned(),
                    Some(_) => continue,
                    None => {
                        if !under(id) || id == within {
                            continue;
                        }
                        let Some(kind) = kind
                            .map(str::to_owned)
                            .or_else(|| self.index.kind_of(id, doc_id))
                        else {
                            continue;
                        };
                        // Only what the document holds now: one moved on or removed is read
                        // where it is.
                        if self
                            .index
                            .get(&kind, id)
                            .is_none_or(|held| held.doc != **doc_id)
                        {
                            continue;
                        }
                        kind
                    }
                };
                if seen.insert(id.to_owned()) {
                    found.push((kind, id.to_owned(), (*doc_id).to_owned()));
                }
            }
        }
        for (kind, id, doc_id) in found {
            let last = touches
                .get(&doc_id)
                .and_then(|held| held.versions_of(&id))
                .and_then(|versions| versions.last().cloned());
            if let Some(last) = last {
                newest.push((kind, id, last));
            }
        }
        // A removed resource's history stays readable, by whoever could read it.
        for (kind, id) in self.removed.keys() {
            if !under(id)
                || !self
                    .readers_of(kind, id)
                    .is_some_and(|readers| may_read(&readers))
            {
                continue;
            }
            if let Some(last) = self.versions_unlocked(&mut touches, kind, id).pop() {
                newest.push((kind.clone(), id.clone(), last));
            }
        }
        drop(touches);
        newest.sort_by_key(|(_, _, version)| std::cmp::Reverse(order(version)));
        let more = newest.len() > limit;
        newest.truncate(limit);
        (newest, more)
    }

    /// Every version stamped as a step of the group `group` ([`Stamp::group`]), of every
    /// resource it touched, by kind and id: what a review of that work lists. Reads only the
    /// documents a change of the group is in.
    #[must_use]
    pub fn grouped(&self, group: &str) -> Vec<(String, String, crate::history::Version)> {
        let mut out = self.stamped(self.groups.get(group), &|stamp| {
            stamp.group.as_ref().is_some_and(|held| held.id == group)
        });
        // A resource made in or removed from another changed that one too: its collection.
        let mut holders: Vec<(String, String, crate::history::Version)> = Vec::new();
        for (kind, id, version) in &out {
            let made = match version.kind {
                crate::history::VersionKind::Made => true,
                crate::history::VersionKind::Removed => false,
                crate::history::VersionKind::Changed => continue,
            };
            if version.content {
                continue;
            }
            let key = (kind.clone(), id.clone());
            let parent = match self.index.get(kind, id) {
                Some(held) => held.parent.clone(),
                None => self
                    .removed
                    .get(&key)
                    .and_then(|removed| removed.parent.clone()),
            };
            let Some(parent) = parent else {
                continue;
            };
            let parent_kind = self.index.kind_anywhere(&parent).or_else(|| {
                self.removed
                    .keys()
                    .find(|(_, held)| *held == parent)
                    .map(|(kind, _)| kind.clone())
            });
            let Some(parent_kind) = parent_kind else {
                continue;
            };
            let nested = crate::history::Nested {
                kind: kind.clone(),
                id: id.clone(),
                made,
            };
            if let Some((_, _, held)) = holders.iter_mut().find(|(_, held, held_version)| {
                *held == parent && held_version.change == version.change
            }) {
                held.nested.push(nested);
                continue;
            }
            holders.push((
                parent_kind,
                parent,
                crate::history::Version {
                    stamp: version.stamp.clone(),
                    kind: crate::history::VersionKind::Changed,
                    fields: Vec::new(),
                    content: false,
                    change: version.change.clone(),
                    op: version.op,
                    parent: None,
                    nested: vec![nested],
                },
            ));
        }
        out.extend(holders);
        out
    }

    /// Every resource made by a change stamped as made from the resource `id`
    /// ([`Stamp::from`]), by kind and id: what was made from it, wherever it was placed.
    #[must_use]
    pub fn made_from(&self, id: &str) -> Vec<(String, String)> {
        let mut made: Vec<(String, String, crate::history::Version)> = self
            .stamped(self.made_from.get(id), &|stamp| {
                stamp.from.as_ref().is_some_and(|from| from.id == id)
            })
            .into_iter()
            .filter(|(_, _, version)| version.kind == crate::history::VersionKind::Made)
            .collect();
        // In the order they were made.
        made.sort_by(|(_, _, a), (_, _, b)| {
            let at = |version: &crate::history::Version| {
                version
                    .stamp
                    .as_ref()
                    .map(|stamp| stamp.at.clone())
                    .unwrap_or_default()
            };
            at(a).cmp(&at(b)).then(a.op.cmp(&b.op))
        });
        let mut out: Vec<(String, String)> = Vec::with_capacity(made.len());
        for (kind, id, _) in made {
            if !out.iter().any(|(_, held)| *held == id) {
                out.push((kind, id));
            }
        }
        out
    }

    /// The versions the changes stamped as `stamped` admits made, of every resource they
    /// touched, by kind and id, reading only the documents `docs`.
    fn stamped(
        &self,
        docs: Option<&BTreeSet<String>>,
        stamped: &dyn Fn(&Stamp) -> bool,
    ) -> Vec<(String, String, crate::history::Version)> {
        let mut out = Vec::new();
        for doc_id in docs.into_iter().flatten() {
            let Some(doc) = self.docs.get(doc_id) else {
                continue;
            };
            let content = crate::admit::content_resource(doc_id);
            for (version, id) in crate::history::stamped(doc, doc_id, stamped) {
                let kind = match &content {
                    Some((kind, _)) => Some(kind.clone()),
                    None => self.index.kind_of(&id, doc_id).or_else(|| {
                        self.removed
                            .iter()
                            .find(|((_, held), removed)| *held == id && removed.doc == *doc_id)
                            .map(|((kind, _), _)| kind.clone())
                    }),
                };
                if let Some(kind) = kind {
                    out.push((kind, id, version));
                }
            }
        }
        out
    }

    /// The resource `id` of `kind` as it stood just before the write that made the change
    /// `change` (its hash, in hex), or just after it with `after`: its indexed fields and its
    /// content then. One write may change both of a resource's documents, each change stamped
    /// alike, so each document is read before its first change of that write, or after its
    /// last; a document the write did not change reads as it stands. A write is the changes
    /// that name one invocation at one moment ([`Stamp::moment`]): an invocation that wrote the
    /// resource several times (the keystrokes of one burst of typing) is read write by write,
    /// and one whose writer kept no moment is read whole. What a resource's history reads a
    /// version's fields from. `None` where the change is not a hash, or the resource's
    /// documents are not in memory.
    #[must_use]
    pub fn at_change(
        &self,
        kind: &str,
        id: &str,
        change: &str,
        after: bool,
    ) -> Option<(Option<Indexed>, Option<serde_json::Value>)> {
        let hash: automerge::ChangeHash = change.parse().ok()?;
        let (index_doc, readers) = self.placed(kind, id)?;
        let content_doc = content_id(&readers, kind, id);
        let held = self.docs.get(&index_doc)?;
        let content = self.docs.get(&content_doc);
        let written = held
            .get_change_by_hash(&hash)
            .or_else(|| content.and_then(|doc| doc.get_change_by_hash(&hash)))
            .and_then(|change| Stamp::of(&change))?;
        let one_write = |stamp: &Stamp| {
            stamp.invocation == written.invocation
                && (written.moment == 0 || stamp.moment == written.moment)
        };
        // Each document as it stood before the write's first change of the resource, or after
        // its last: read at those heads, the rest of what the document holds unread.
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut heads_of = |doc: &Automerge, doc_id: &str| -> Option<Vec<automerge::ChangeHash>> {
            let versions = touches
                .entry(doc_id.to_owned())
                .or_default()
                .of(doc, doc_id, id);
            let mut own = versions
                .iter()
                .filter(|version| version.stamp.as_ref().is_some_and(one_write));
            let first = own.next()?;
            let last = own.next_back().unwrap_or(first);
            if after {
                Some(vec![last.change.parse().ok()?])
            } else {
                let first: automerge::ChangeHash = first.change.parse().ok()?;
                Some(doc.get_change_by_hash(&first)?.deps().to_vec())
            }
        };
        let index_heads = heads_of(held, &index_doc);
        let content_heads = content.and_then(|doc| heads_of(doc, &content_doc));
        drop(touches);
        let indexed = read_as_of(held, index_heads.as_deref(), id)
            .filter(|(held_kind, _)| held_kind == kind)
            .map(|(_, resource)| Indexed {
                doc: index_doc,
                parent: resource.parent,
                order: resource.order,
                fields: resource.fields,
            });
        let content = content.and_then(|doc| {
            let look = crate::wire::AsOf {
                doc,
                heads: content_heads.as_deref(),
            };
            (look.length(&ROOT) > 0).then(|| look.json(&ROOT))
        });
        Some((indexed, content))
    }

    /// The types and verbs declared so far: what a peer's change is checked against.
    #[must_use]
    pub const fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Takes the types `schema` declares into those a peer's change is checked against.
    pub fn declare(&mut self, schema: Schema) {
        for ty in schema.types {
            self.schema.types.retain(|held| held.name != ty.name);
            self.schema.types.push(ty);
        }
        // A kind is one app's, so a verb's write-sets merge kind by kind: one app's verb of a
        // name writes that app's kinds, another's of the same name its own.
        for verb in schema.verbs {
            match self
                .schema
                .verbs
                .iter_mut()
                .find(|held| held.name == verb.name)
            {
                Some(held) => held.writes.extend(verb.writes),
                None => self.schema.verbs.push(verb),
            }
        }
    }

    /// Whether `received` may take effect in `doc_id` as it stands.
    fn judge(
        &mut self,
        doc_id: &str,
        readers: &str,
        received: &Received,
        schema: Option<&Schema>,
        admit: &dyn Admit,
    ) -> Result<(), Refusal> {
        let doc = self.doc(doc_id).map_err(|_| Refusal::Unplaced)?;
        let (touched, writes) =
            crate::admit::touched(doc, doc_id, &received.change).ok_or(Refusal::Unplaced)?;
        // A document's reader set is the one it was placed by, whoever writes it.
        if touched
            .readers
            .as_ref()
            .is_some_and(|written| written != readers)
        {
            return Err(Refusal::Unplaced);
        }
        let index = &self.index;
        let stamp = Stamp::of(&received.change);
        for schema in std::iter::once(&self.schema).chain(schema) {
            crate::admit::declared(schema, &touched, &writes, &|id| index.kind_anywhere(id))
                .map_err(Refusal::Undeclared)?;
            crate::admit::within_verb(schema, &touched, stamp.as_ref())
                .map_err(Refusal::Undeclared)?;
        }
        let admitted = admit.admit(&Proposal {
            doc: doc_id,
            readers,
            author: &received.author,
            stamp: stamp.as_ref(),
            touched: &touched,
        });
        if admitted {
            Ok(())
        } else {
            Err(Refusal::NotPermitted)
        }
    }

    /// Applies `change`, admitted, to `doc_id`, and indexes the resources it touched.
    fn apply(&mut self, doc_id: &str, readers: &str, change: AmChange) -> Result<(), StoreError> {
        if let Some(stamp) = Stamp::of(&change) {
            self.note_group(doc_id, &stamp);
        }
        let doc = self.doc(doc_id)?;
        let mut log = PatchLog::active();
        doc.apply_changes_log_patches(vec![change], &mut log)?;
        let patches = doc.make_patches(&mut log);
        let mut touched = BTreeSet::new();
        for patch in &patches {
            let top = match (patch.path.first(), &patch.action) {
                (Some((_, Prop::Map(key))), _)
                | (
                    None,
                    automerge::PatchAction::PutMap { key, .. }
                    | automerge::PatchAction::DeleteMap { key },
                ) => Some(key.as_str()),
                _ => None,
            };
            if let Some(id) = top.and_then(|key| key.strip_prefix(HELD)) {
                touched.insert(id.to_owned());
            }
        }
        self.index_resources(doc_id, readers, Some(touched))
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
        let ids: BTreeSet<String> = match only {
            Some(ids) => ids,
            None => doc
                .keys(ROOT)
                .filter_map(|key| key.strip_prefix(HELD).map(str::to_owned))
                .collect(),
        };
        let reads: Vec<(String, Option<(String, Resource)>)> = ids
            .into_iter()
            .map(|id| {
                let found = read(doc, &id);
                (id, found)
            })
            .collect();
        let heads = doc.get_heads();
        let count = doc.keys(ROOT).filter(|key| key.starts_with(HELD)).count();
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
            .strip_prefix(collab_types::reader_set_name(readers).as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_prefix(owner))
            .and_then(|rest| rest.strip_prefix('#'))
            .and_then(|number| number.parse::<u32>().ok())
        else {
            return;
        };
        self.nested_in
            .entry(owner.to_owned())
            .or_default()
            .insert(doc.to_owned());
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
        for change in doc.get_changes(&[]) {
            if let Some(stamp) = Stamp::of(&change) {
                self.note_group(doc_id, &stamp);
            }
        }
        // What it held before and holds no more was removed: its history stays readable.
        let held: Vec<(String, String, Option<String>)> = self
            .index
            .held(doc_id)
            .map(|(kind, id)| {
                let parent = self
                    .index
                    .get(kind, id)
                    .and_then(|held| held.parent.clone());
                (kind.to_owned(), id.to_owned(), parent)
            })
            .collect();
        self.index.forget(doc_id);
        self.docs.insert(doc_id.to_owned(), Arc::new(doc));
        self.index_resources(doc_id, &readers, None)?;
        for (kind, id, parent) in held {
            if self.index.get(&kind, &id).is_none() {
                self.note_removed(&kind, &id, doc_id, parent);
            }
        }
        Ok(())
    }

    /// Notes that `doc` holds a change stamped `stamp`, when the stamp names a group.
    fn note_group(&mut self, doc: &str, stamp: &Stamp) {
        if let Some(group) = &stamp.group {
            self.groups
                .entry(group.id.clone())
                .or_default()
                .insert(doc.to_owned());
        }
        if let Some(from) = &stamp.from {
            self.made_from
                .entry(from.id.clone())
                .or_default()
                .insert(doc.to_owned());
        }
    }

    /// The document that holds the resource `id` of `kind`, or held it last when it was
    /// removed, and its reader set.
    fn placed(&self, kind: &str, id: &str) -> Option<(String, String)> {
        let doc = match self.index.get(kind, id) {
            Some(held) => held.doc.clone(),
            None => self
                .removed
                .get(&(kind.to_owned(), id.to_owned()))?
                .doc
                .clone(),
        };
        let readers = self.index.readers(&doc)?.to_owned();
        Some((doc, readers))
    }

    /// The reader set of the resource `id` of `kind`, held now or removed: what a read of its
    /// history is admitted by.
    #[must_use]
    pub fn readers_of(&self, kind: &str, id: &str) -> Option<String> {
        self.placed(kind, id).map(|(_, readers)| readers)
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

    /// Forget the local document and its index rows after access is withdrawn.
    /// A later authorized read can load the document again.
    pub fn forget_document(&mut self, doc: &str) {
        let kinds: BTreeSet<_> = self.index.held(doc).map(|(kind, _)| kind).collect();
        self.changed.extend(
            self.watches
                .iter()
                .filter_map(|(id, kind)| kinds.contains(kind.as_str()).then_some(*id)),
        );
        self.index.forget(doc);
        self.docs.remove(doc);
        self.refused.remove(doc);
        self.waiting.remove(doc);
    }

    /// Lets the document `doc` go from memory, answering it saved; its resources stay indexed,
    /// and it is read again from the source when needed.
    pub fn evict(&mut self, doc: &str) -> Option<Vec<u8>> {
        self.docs.remove(doc).map(|doc| doc.save())
    }

    /// Takes back what `doc` took since `before`, the heads it stood at: a write its writer
    /// could not send anywhere, which must not stay readable here as if it had been. A
    /// document that held nothing before is let go whole. Open queries over what it held
    /// are told it changed.
    ///
    /// # Errors
    /// When the document cannot be read back at `before`.
    pub fn take_back(&mut self, doc: &str, before: &[ChangeHash]) -> Result<(), StoreError> {
        let Some(now) = self.docs.get(doc) else {
            return Ok(());
        };
        let back = if before.is_empty() {
            None
        } else {
            Some(now.fork_at(before)?)
        };
        let readers = self.index.readers(doc).map(str::to_owned);
        let kinds: BTreeSet<String> = self
            .index
            .held(doc)
            .map(|(kind, _)| kind.to_owned())
            .collect();
        self.index.forget(doc);
        self.docs.remove(doc);
        for (id, watched) in &self.watches {
            if kinds.contains(watched) {
                self.changed.insert(*id);
            }
        }
        if let Some(back) = back {
            let readers = readers
                .or_else(|| text_at(&back, &ROOT, READERS))
                .unwrap_or_default();
            self.docs.insert(doc.to_owned(), Arc::new(back));
            self.index_resources(doc, &readers, None)?;
        }
        Ok(())
    }

    /// The document `doc` as it stands in memory, for whoever keeps or sends it whole.
    #[must_use]
    pub fn document(&self, doc: &str) -> Option<&Automerge> {
        self.docs.get(doc).map(Arc::as_ref)
    }

    /// How many documents are in memory.
    #[must_use]
    pub fn in_memory(&self) -> usize {
        self.docs.len()
    }
}

/// What the key `key` of the map `map` holds; `None` where it holds nothing.
fn hydrated_key(map: &automerge::hydrate::Value, key: &str) -> Option<automerge::hydrate::Value> {
    match map {
        automerge::hydrate::Value::Map(map) => map.get(key).cloned(),
        _ => None,
    }
}

/// Gives the key `key` of the map `to` in `tx` what it holds in the map at the root of `was`:
/// its value as it stood, texts with their marks; nothing where it held nothing.
pub(crate) fn restore_key(
    was: &Automerge,
    key: &str,
    tx: &mut automerge::transaction::Transaction<'_>,
    to: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    if tx.get(to, key)?.is_some() {
        tx.delete(to, key)?;
    }
    match was.get(ROOT, key)? {
        None => Ok(()),
        Some((AmValue::Scalar(value), _)) => tx.put(to, key, value.into_owned()),
        Some((AmValue::Object(kind), from)) => {
            let made = tx.put_object(to, key, kind)?;
            copy_object(was, &from, tx, &made)
        }
    }
}

/// Writes what the object `from` of `source` holds into the empty object `to` of `tx`, as it
/// stands now: its maps, lists and scalars, and its texts with their marks and blocks.
fn copy_object(
    source: &Automerge,
    from: &ObjId,
    tx: &mut automerge::transaction::Transaction<'_>,
    to: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    let value = ReadDoc::hydrate(source, from, None)?;
    tx.update_object(to, &value)
        .map_err(|_| automerge::AutomergeError::Fail)?;
    copy_texts(source, from, tx, to)
}

/// Gives each text under `from` its marks and blocks in its copy under `to`.
fn copy_texts(
    source: &Automerge,
    from: &ObjId,
    tx: &mut automerge::transaction::Transaction<'_>,
    to: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    let props: Vec<Prop> = match source.object_type(from)? {
        ObjType::Map | ObjType::Table => source.keys(from).map(Prop::Map).collect(),
        ObjType::List => (0..source.length(from)).map(Prop::Seq).collect(),
        ObjType::Text => {
            let spans: Vec<automerge::iter::Span> = source.spans(from)?.collect();
            return tx.update_spans(to, automerge::marks::UpdateSpansConfig::default(), spans);
        }
    };
    for prop in props {
        let Some((AmValue::Object(_), inner)) = source.get(from, prop.clone())? else {
            continue;
        };
        let Some((AmValue::Object(_), copied)) = tx.get(to, prop)? else {
            continue;
        };
        copy_texts(source, &inner, tx, &copied)?;
    }
    Ok(())
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
/// The key of the resource `id` at its document's root.
fn held_key(id: &str) -> String {
    format!("{HELD}{id}")
}

fn read(doc: &Automerge, id: &str) -> Option<(String, Resource)> {
    read_as_of(doc, None, id)
}

/// [`read`] as the document stood at `heads`, or as it stands with none.
fn read_as_of(
    doc: &Automerge,
    heads: Option<&[automerge::ChangeHash]>,
    id: &str,
) -> Option<(String, Resource)> {
    let look = crate::wire::AsOf { doc, heads };
    let (AmValue::Object(ObjType::Map), map) = look.get(&ROOT, held_key(id))? else {
        return None;
    };
    let text = |key: &str| match look.get(&map, key)? {
        (AmValue::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    };
    let kind = text(KIND)?;
    let mut fields = BTreeMap::new();
    if let Some((AmValue::Object(ObjType::Map), held)) = look.get(&map, FIELDS) {
        for name in look.keys(&held) {
            let Some((value, obj)) = look.get(&held, name.as_str()) else {
                continue;
            };
            let cell = match value {
                AmValue::Object(ObjType::Map) => {
                    Cell::Set(look.keys(&obj).into_iter().map(Value::Text).collect())
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
            parent: text(PARENT),
            order: text(ORDER),
            fields,
        },
    ))
}

fn put_fields(
    doc: &mut Automerge,
    kind: &str,
    id: &str,
    resource: &Resource,
    stamp: &Stamp,
) -> Result<Option<AmChange>, StoreError> {
    let mut tx = doc.transaction();
    if text_at(&tx, &ROOT, READERS).as_deref() != Some(resource.readers.as_str()) {
        tx.put(ROOT, READERS, resource.readers.as_str())?;
    }
    // Field by field, so two devices changing different fields of one resource both keep
    // their change.
    let map = map_in(&mut tx, &ROOT, &held_key(id))?;
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
                let gone: Vec<String> = tx.keys(&set).filter(|key| !keys.contains(key)).collect();
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
    let (hash, _) = tx.commit_with(CommitOptions::default().with_message(stamp.message()));
    Ok(hash.and_then(|_| doc.get_last_local_change()))
}
