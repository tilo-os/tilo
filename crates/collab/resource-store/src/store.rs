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
fn content_id(readers: &str, kind: &str, id: &str) -> String {
    format!("{}/{kind}/{id}~0", collab_types::reader_set_name(readers))
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
    docs: HashMap<String, Automerge>,
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

    /// Hands `commit` to the index and notes the watches it concerns.
    fn commit(&mut self, commit: &Commit<'_>) {
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
            self.docs.insert(doc.to_owned(), Automerge::load(&bytes)?);
        }
        Ok(self.docs.get_mut(doc))
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
        let mut wanted: Vec<String> = Vec::new();
        if let Some((held, readers)) = self.placed(kind, id) {
            wanted.push(content_id(&readers, kind, id));
            wanted.push(held);
        }
        wanted.extend(self.nested_in.get(id).into_iter().flatten().cloned());
        for doc in wanted {
            self.existing(&doc)?;
        }
        Ok(self.versions(kind, id))
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
        // Each resource's newest version, borrowed while the documents are walked: only the
        // page read is copied.
        let mut found: Vec<((u64, u64), String, String, String)> = Vec::new();
        for doc_id in self.index.documents() {
            let Some(readers) = self
                .index
                .readers(doc_id)
                .filter(|readers| may_read(readers))
            else {
                continue;
            };
            let Some(doc) = self.docs.get(doc_id) else {
                continue;
            };
            let held: Vec<(&str, &str)> = self
                .index
                .held(doc_id)
                .filter(|(_, id)| under(id) && *id != within)
                .collect();
            if self.index.held(doc_id).any(|(_, id)| id == within) {
                for (kind, id) in self.index.held(doc_id).filter(|(_, id)| *id == within) {
                    if let Some(last) = self.versions_unlocked(&mut touches, kind, id).pop() {
                        newest.push((kind.to_owned(), id.to_owned(), last));
                    }
                }
            }
            if held.is_empty() {
                continue;
            }
            let lasts = touches
                .entry(doc_id.to_owned())
                .or_default()
                .lasts(doc, doc_id);
            for (kind, id) in &held {
                if let Some(last) = lasts.get(*id).and_then(|versions| versions.last()) {
                    found.push((
                        order(last),
                        (*kind).to_owned(),
                        (*id).to_owned(),
                        doc_id.to_owned(),
                    ));
                }
            }
            // What was written in a resource's content, in a document of its own.
            for (kind, id) in held {
                let content = content_id(readers, kind, id);
                let Some(doc) = self.docs.get(&content) else {
                    continue;
                };
                let written = touches
                    .entry(content.clone())
                    .or_default()
                    .lasts(doc, &content)
                    .get(id)
                    .and_then(|versions| versions.last())
                    .map(order);
                if let Some(written) = written {
                    found.push((written, kind.to_owned(), id.to_owned(), content));
                }
            }
        }
        // The newest of each resource, most recent first, a page of them.
        found.sort_by_key(|found| std::cmp::Reverse(found.0));
        let mut seen = BTreeSet::new();
        for (_, kind, id, doc_id) in found {
            if newest.len() > limit {
                break;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
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

    /// The resource `id` of `kind` as it stood just before the invocation that made the change
    /// `change` (its hash, in hex), or just after it with `after`: its indexed fields and its
    /// content then. One invocation may change both of a resource's documents, so each is read
    /// before its first change of that invocation, or after its last; a document the
    /// invocation did not change reads as it stands. What a resource's history reads a step's
    /// fields from. `None` where the change is not a hash, or the resource's documents are not
    /// in memory.
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
        let invocation = held
            .get_change_by_hash(&hash)
            .or_else(|| content.and_then(|doc| doc.get_change_by_hash(&hash)))
            .and_then(|change| Stamp::of(&change))
            .map(|stamp| stamp.invocation)?;
        // Each document as it stood before the invocation's first change of the resource, or
        // after its last: read at those heads, the rest of what the document holds unread.
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut heads_of = |doc: &Automerge, doc_id: &str| -> Option<Vec<automerge::ChangeHash>> {
            let versions = touches
                .entry(doc_id.to_owned())
                .or_default()
                .of(doc, doc_id, id);
            let mut own = versions.iter().filter(|version| {
                version
                    .stamp
                    .as_ref()
                    .is_some_and(|stamp| stamp.invocation == invocation)
            });
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
        self.docs.insert(doc_id.to_owned(), doc);
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
