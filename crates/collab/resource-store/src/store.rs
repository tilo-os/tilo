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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        format!(
            "{}/{owner}#{}",
            collab_types::reader_set_name(readers),
            filled.newest
        )
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
        tx.commit_with(CommitOptions::default().with_message(stamp.message()));
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
        let docs: Vec<String> = self.index.documents().map(str::to_owned).collect();
        let mut written = Vec::new();
        for doc_id in docs {
            let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
            let Some(doc) = self.existing(&doc_id)? else {
                continue;
            };
            let Some((before, after)) = span_of(doc, invocation) else {
                continue;
            };
            let was = doc.fork_at(&before)?;
            let became = doc.fork_at(&after)?;
            if let Some((kind, id)) = crate::admit::content_resource(&doc_id) {
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
                        restore_key(&was, &key, tx, root)?;
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
                if let Err(refusal) = self.judge(doc_id, readers, &received, admit) {
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
        crate::admit::declared(&self.schema, &touched, &writes, &|id| {
            index.kind_anywhere(id)
        })
        .map_err(Refusal::Undeclared)?;
        let stamp = Stamp::of(&received.change);
        crate::admit::within_verb(&self.schema, &touched, stamp.as_ref())
            .map_err(Refusal::Undeclared)?;
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

/// The heads `doc` stood at before the first change of `invocation`, and the heads its last one
/// made: the stretch of history it wrote. `None` for a document it did not change.
fn span_of(doc: &Automerge, invocation: &str) -> Option<(Vec<ChangeHash>, Vec<ChangeHash>)> {
    let mut first = None;
    let mut last = None;
    for change in doc.get_changes_meta(&[]) {
        let stamp = change
            .message
            .as_deref()
            .and_then(|message| serde_json::from_str::<Stamp>(message).ok());
        if stamp.is_some_and(|stamp| stamp.invocation == invocation) {
            first.get_or_insert(change.deps);
            last = Some(change.hash);
        }
    }
    Some((first?, vec![last?]))
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
fn restore_key(
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
    let (AmValue::Object(ObjType::Map), map) = doc.get(ROOT, held_key(id)).ok()?? else {
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
