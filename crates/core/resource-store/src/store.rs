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
//! given with the heads the document is at ([`Store::restore_version`]), and the document itself is
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

use collab_types::store::{Cell, Filter, Indexed, Page, Query, Resource, Value};
use collab_types::touch::READERS;

use crate::admit::{Admit, Proposal, Received, Refusal, Stamp};
use crate::index::{Change, Commit, CommitHook as _, Index};
use crate::schema::{MIGRATE_VERB, Schema};
use tilo_actions::remote::argument_parts::Held as Kept;

mod named;
mod past;
mod readers;
mod recent;
pub use readers::RecordReader;

/// The most resources one document holds before the next one starts.
pub const DOCUMENT_LIMIT: usize = 4096;
/// How many calls the store keeps at hand by digest ([`Store::calls`]) before it lets them go.
const KEPT_CALLS: usize = 1024;

/// What a resource's key at the document's root starts with: each resource is a key of the root
/// of its own, so two devices starting one document both keep what they wrote.
const HELD: &str = collab_types::claim::STORE_RECORD;
const KIND: &str = collab_types::claim::STORE_RECORD_KIND;
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

/// The document that holds the content of the resource `id` of `kind`, read by `readers`.
#[must_use]
pub fn content_id(readers: &str, kind: &str, id: &str) -> String {
    format!("{}/{kind}/{id}~0", collab_types::reader_set_name(readers))
}

/// The storage version a change of the migration verb, `stamp`, names, and whether it is the app's
/// rewrite for it rather than the derived step: `None` when it names none.
fn migration_of(stamp: &Stamp) -> Option<(u32, bool)> {
    match &stamp.context.as_deref()?.what {
        Some(tilo_actions::Did::History(step)) => match step.op {
            tilo_actions::HistoryOp::Migrate { to, rewrite } => Some((to, rewrite)),
            _ => None,
        },
        _ => None,
    }
}

/// The kind and id of the resource whose content the document `doc` holds ([`content_id`]);
/// `None` for a document of resources.
#[must_use]
pub fn content_resource(doc: &str) -> Option<(&str, &str)> {
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
    /// The resources it is nested in, nearest first, up to the record that holds it: each with
    /// its kind, id, indexed fields and the content it holds. Empty for a resource at its app's
    /// top level.
    pub parents: Vec<(String, String, serde_json::Map<String, serde_json::Value>)>,
}

/// The most resources [`Wrote::parents`] names above one: a chain deeper than this is one a
/// write of it does not walk, so a cycle a damaged document holds never loops.
const MOST_PARENTS: usize = 32;

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
    /// A write places a resource where its type's reader rule does not.
    Readers(String),
    /// A step through history that cannot be taken: why.
    History(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { kind, id } => write!(f, "there is no {kind} `{id}`"),
            Self::Document(error) => write!(f, "the document refused the change: {error}"),
            Self::NotAWrite => write!(f, "that is a read, not a write"),
            Self::Readers(reason) | Self::History(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<automerge::AutomergeError> for StoreError {
    fn from(error: automerge::AutomergeError) -> Self {
        Self::Document(error)
    }
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
pub struct Store {
    /// The workspace whose resources this store keeps: what a record's derived founding and steps
    /// are derived within ([`collab_types::document::founding::Scope`]), so a record of the same
    /// kind and id in another workspace never shares a change with one here.
    workspace: String,
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
    /// The surface kind that declared each type, as [`Store::declare`] took it: a type's newer
    /// version is only compared with the one the same surface declared, since two apps' types of
    /// one name are unrelated.
    types_by: BTreeMap<String, String>,
    /// The reader rule of each kind whose type names its readers, as [`Store::declare`] took it.
    rules: BTreeMap<String, tilo_actions::surface::ReaderRule>,
    /// The workspace's one read decision, for the reader fields the store places: given by the
    /// host that made this store, so an agent in a field is read as the person it acts for.
    reader: Option<std::sync::Arc<dyn RecordReader>>,
    /// Who the writes drafted now are made by, and at which second, when the store drafts one
    /// every device makes alike ([`Self::drafting`]): a repair, which two devices must write as
    /// the one change.
    drafting: Option<(automerge::ActorId, i64)>,
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
    /// Per document, the versions of each resource in it, read as far as its changes go. A
    /// staged copy of the store ([`Self::stage`]) reads into the same one: a document's changes
    /// make the same versions whoever reads them, so a draft reads only the changes it adds, not
    /// every change the document took since it was made, and the store taking those changes in
    /// reads nothing again. What a stage read and the store never took in is let go when a
    /// document is next read ([`collab_types::history::Touches::read`]).
    touches: std::sync::Arc<std::sync::Mutex<HashMap<String, collab_types::history::Touches>>>,
    /// Per resource, the time of the latest change that made a resource in it or removed one
    /// from it: a change of it too, which its [`UPDATED`] reflects without reading the
    /// history of what it holds.
    nested_at: HashMap<String, String>,
    /// The resources live now, by kind and id: those whose type's condition holds of them as the
    /// index holds them ([`Self::live`]).
    live: BTreeMap<(String, String), Live>,
    /// Grows whenever a resource goes live or stops being.
    live_revision: u64,
    /// The documents of resources' content by the newest version each holds, the most recent
    /// first: what [`Self::newest`] reads a page of.
    recent: std::sync::Mutex<recent::Recent>,
    /// In a staged copy ([`Self::stage`]), each document a change of it keeps a call in, with
    /// the call's digest: every later change of that call to the document names it by its
    /// digest alone, so a call that writes many records keeps itself once per document. `None`
    /// in the live store, whose every change keeps its call whole.
    called: Option<BTreeSet<(String, String)>>,
    /// The calls the changes taken in lately keep, by digest: what a change that names its call
    /// by digest is read with, its call kept by an earlier change of its document.
    calls: tilo_actions::stamp::KeptCalls,
}

/// A resource that is live now (a track playing, a timer running): its type's condition holds
/// of it. Derived from what the store holds, like the index; nothing writes it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Live {
    pub kind: String,
    pub id: String,
    /// Who may read it: its document's reader set, `*` for everyone.
    pub readers: String,
    /// The device whose change made it live, its key in hex ([`Stamp::device`]); empty where
    /// that change named none.
    pub device: String,
    /// What that device calls itself ([`Stamp::device_name`]).
    pub device_name: String,
    /// When it went live: the time that change was invoked at, ISO-8601 UTC; empty where it
    /// named none.
    pub since: String,
}

impl Store {
    /// An empty store of the resources of `workspace`.
    #[must_use]
    pub fn new(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            docs: HashMap::new(),
            filled: HashMap::new(),
            index: Index::default(),
            source: None,
            watches: BTreeMap::new(),
            changed: BTreeSet::new(),
            schema: Schema::default(),
            types_by: BTreeMap::new(),
            rules: BTreeMap::new(),
            reader: None,
            drafting: None,
            refused: HashMap::new(),
            waiting: HashMap::new(),
            removed: HashMap::new(),
            removed_under: HashMap::new(),
            nested_in: HashMap::new(),
            groups: HashMap::new(),
            made_from: HashMap::new(),
            touches: std::sync::Arc::default(),
            nested_at: HashMap::new(),
            live: BTreeMap::new(),
            live_revision: 0,
            recent: std::sync::Mutex::default(),
            called: None,
            calls: tilo_actions::stamp::KeptCalls::default(),
        }
    }

    /// A store of the resources of `workspace` that reads each document it does not hold in
    /// memory from `source`.
    #[must_use]
    pub fn with_source(workspace: impl Into<String>, source: Source) -> Self {
        Self {
            source: Some(source),
            ..Self::new(workspace)
        }
    }

    /// The workspace whose resources this store keeps.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Who reads a record this store places, as the workspace's one read decision answers. The
    /// host gives it where it gives this store a workspace's policy; a reader field placed
    /// before it is given is refused.
    pub fn set_record_reader(&mut self, reader: std::sync::Arc<dyn RecordReader>) {
        self.reader = Some(reader);
    }

    /// Send a command against a private copy of the store. The command can follow the placement,
    /// index and undo history of its earlier steps without publishing any of them, and its writes
    /// are judged by the types, reader rules and read decision the live store holds.
    pub(crate) fn stage<T>(
        &mut self,
        scope: &crate::wire::DraftScope,
        command: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let (index, filled, nested_in) = if scope.undo {
            // A step through history reads every document a resource is nested in: what it
            // takes back is in theirs, not only in the document of what it names.
            (
                self.index.clone(),
                self.filled.clone(),
                self.nested_in.clone(),
            )
        } else if scope.check {
            (
                self.index.selected(&scope.resources),
                self.filled.clone(),
                self.nested_in.clone(),
            )
        } else {
            let filled = scope
                .placements
                .iter()
                .filter_map(|key| self.filled.get(key).map(|filled| (key.clone(), *filled)))
                .collect();
            (
                self.index.selected(&scope.resources),
                filled,
                HashMap::new(),
            )
        };
        let mut staged = Self {
            docs: self.docs.clone(),
            filled,
            index,
            nested_in,
            source: self.source.take(),
            // What a write is judged by: the types every app declared, the reader rules, and
            // the workspace's read decision.
            schema: self.schema.clone(),
            rules: self.rules.clone(),
            reader: self.reader.clone(),
            drafting: self.drafting.clone(),
            touches: std::sync::Arc::clone(&self.touches),
            called: Some(BTreeSet::new()),
            calls: self.calls.clone(),
            ..Self::new(self.workspace.clone())
        };
        let result = command(&mut staged);
        self.source = staged.source.take();
        result
    }

    /// Check each of `writes`, in order, as the store would hold it once the writes before it
    /// are taken in, and take it into a private copy of the store so the next one is judged
    /// against it. A batch is kept whole or not at all, so none of it may be retained before
    /// the last of it passes; this store is left as it was.
    ///
    /// # Errors
    /// The first write it refuses.
    pub fn check_batch(&mut self, writes: &[Written], author: [u8; 32]) -> Result<(), Refusal> {
        self.check_batch_with(writes, author, &|_: &Proposal<'_>| true)
    }

    /// [`Self::check_batch`], admitting each write's author by `admit`: what the workspace judges
    /// a write of this device's own by, as it judges a received one.
    ///
    /// # Errors
    /// The first write it refuses.
    pub fn check_batch_with(
        &mut self,
        writes: &[Written],
        author: [u8; 32],
        admit: &dyn Admit,
    ) -> Result<(), Refusal> {
        let Some((last, before)) = writes.split_last() else {
            return Ok(());
        };
        // A write the store judges as it stands, a batch's only one, is judged against this
        // store: judging reads it and changes nothing, so no copy of it is made. One into a
        // document the store does not hold yet is judged in a copy, which makes the document.
        if before.is_empty() && self.docs.contains_key(&last.doc) {
            return self.check_write_with(last, author, admit);
        }
        // Each write is taken in so the next is judged against it; the last is judged and taken
        // in by no one.
        let scope = self.check_scope(writes);
        self.stage(&scope, |staged| {
            for write in before {
                staged.check_write_with(write, author, admit)?;
                staged
                    .apply(&write.doc, &write.readers, write.change.clone())
                    .map_err(|_| Refusal::Unplaced)?;
            }
            staged.check_write_with(last, author, admit)
        })
    }

    /// What a check of `writes` reads of the index: the rows of every resource they touch, of
    /// what those are nested in, and of the record whose content a document holds: the only rows
    /// judging a write and taking it in look up. A copy of the whole index costs every record the
    /// workspace holds, on every batch. A document the batch makes is read as the batch makes it;
    /// a write the store cannot read against what it holds is checked against the whole index.
    fn check_scope(&self, writes: &[Written]) -> crate::wire::DraftScope {
        let whole = crate::wire::DraftScope {
            undo: true,
            ..crate::wire::DraftScope::default()
        };
        let mut ids: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut made: HashMap<&str, Automerge> = HashMap::new();
        for write in writes {
            if let Some((_, id)) = content_resource(&write.doc) {
                ids.entry(id.to_owned()).or_default();
            }
            let touched = if let Some(doc) = self.docs.get(&write.doc) {
                collab_types::touch::touched(doc, &write.doc, &write.change)
            } else {
                let doc = made.entry(write.doc.as_str()).or_default();
                let touched = collab_types::touch::touched(doc, &write.doc, &write.change);
                if doc.apply_changes([write.change.clone()]).is_err() {
                    return whole;
                }
                touched
            };
            let Some((touched, _)) = touched else {
                return whole;
            };
            for (id, touch) in touched.resources {
                if let Some(parent) = touch.parent {
                    ids.entry(parent).or_default();
                }
                if let Some(was) = touch.retyped_from {
                    ids.entry(id.clone()).or_default().get_or_insert(was);
                }
                ids.entry(id).or_default();
            }
        }
        let resources = ids
            .into_iter()
            .flat_map(|(id, was)| {
                let held = self.index.kind_anywhere(&id);
                [held, was]
                    .into_iter()
                    .flatten()
                    .map(move |kind| (kind, id.clone()))
            })
            .collect();
        crate::wire::DraftScope {
            resources,
            check: true,
            ..crate::wire::DraftScope::default()
        }
    }

    /// Check a staged write against the live document and the same schema a received write uses.
    ///
    /// # Errors
    /// The change has no local base or the resource schema refuses it.
    pub fn check_write(&mut self, write: &Written, author: [u8; 32]) -> Result<(), Refusal> {
        self.check_write_with(write, author, &|_: &Proposal<'_>| true)
    }

    /// [`Self::check_write`], admitting its author by `admit`.
    ///
    /// # Errors
    /// The change has no local base or the resource schema refuses it.
    pub fn check_write_with(
        &mut self,
        write: &Written,
        author: [u8; 32],
        admit: &dyn Admit,
    ) -> Result<(), Refusal> {
        let doc = self.doc(&write.doc).map_err(|_| Refusal::Unplaced)?;
        if write.change.deps().iter().any(|dep| !doc.has_change(dep)) {
            return Err(Refusal::Unplaced);
        }
        let received = Received {
            change: write.change.clone(),
            author,
            claimed: None,
        };
        self.judge(&write.doc, &write.readers, &received, None, admit)
    }

    /// The kinds of the records `write`, staged against this store, writes, as each holder that
    /// receives it reads them ([`collab_types::claim::store_record_kinds`]): what the sealed op
    /// that carries it claims.
    #[must_use]
    pub fn written_kinds(&mut self, write: &Written) -> BTreeSet<String> {
        let held = self.doc(&write.doc).ok().map(|doc| &*doc);
        collab_types::claim::store_record_kinds(
            &write.doc,
            held,
            std::slice::from_ref(&write.change),
        )
        .remove(&write.change.hash().0)
        .unwrap_or_default()
    }

    /// The index of everything the store holds, whoever may read it.
    #[must_use]
    pub const fn index(&self) -> &Index {
        &self.index
    }

    /// Every resource live now whose readers `may_read` admits, the one live longest first: what
    /// the shell shows in the bar, quick settings and on the lock screen. A reader is never told
    /// of a live resource it may not read.
    #[must_use]
    pub fn live(&self, may_read: &dyn Fn(&str) -> bool) -> Vec<Live> {
        let mut live: Vec<Live> = self
            .live
            .values()
            .filter_map(|live| {
                let readers = self
                    .index
                    .get(&live.kind, &live.id)
                    .and_then(|held| self.index.readers(&held.doc))
                    .filter(|readers| may_read(readers))?;
                Some(Live {
                    readers: readers.to_owned(),
                    ..live.clone()
                })
            })
            .collect();
        live.sort_by(|a, b| (&a.since, &a.kind, &a.id).cmp(&(&b.since, &b.kind, &b.id)));
        live
    }

    /// Grows whenever a resource goes live or stops being: whoever shows [`Self::live`] reads it
    /// again when this moved.
    #[must_use]
    pub const fn live_revision(&self) -> u64 {
        self.live_revision
    }

    /// Whether the resource `id` of `kind` is live as the index now holds it, kept in
    /// [`Self::live`]: one that goes live is listed with the device and the time of the change
    /// that made it so, read from its history, and keeps them while it stays live.
    fn follow_live(&mut self, kind: &str, id: &str) {
        let key = (kind.to_owned(), id.to_owned());
        let when = self
            .schema
            .get(kind)
            .and_then(|ty| ty.live.as_ref())
            .cloned();
        let holds = when.as_ref().is_some_and(|when| {
            self.index
                .get(kind, id)
                .is_some_and(|held| when.iter().all(|filter| filter.holds(held)))
        });
        let Some(when) = when.filter(|_| holds) else {
            if self.live.remove(&key).is_some() {
                self.live_revision = self.live_revision.wrapping_add(1);
            }
            return;
        };
        if self.live.contains_key(&key) {
            return;
        }
        let read: BTreeSet<&str> = when.iter().flat_map(Filter::fields).collect();
        let versions = self.versions(kind, id);
        let made_live = versions
            .iter()
            .filter(|version| {
                version.kind == collab_types::history::VersionKind::Made
                    || version
                        .fields
                        .iter()
                        .any(|field| read.contains(field.as_str()))
            })
            .max_by_key(|version| version.order());
        let (device, device_name, since) = made_live
            .and_then(|version| version.stamp.as_ref())
            .map(|stamp| {
                (
                    stamp.device.clone(),
                    stamp.device_name.clone(),
                    stamp.at.clone(),
                )
            })
            .unwrap_or_default();
        self.live.insert(
            key,
            Live {
                kind: kind.to_owned(),
                id: id.to_owned(),
                // Read from the index when the rows are listed: a resource moved to another
                // document keeps its row.
                readers: String::new(),
                device,
                device_name,
                since,
            },
        );
        self.live_revision = self.live_revision.wrapping_add(1);
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

    /// The values the resources of `kind` whose readers `may_read` admits and that hold to
    /// `filters` hold in `field`, each once, in order ([`Index::values`]).
    #[must_use]
    pub fn values(
        &self,
        kind: &str,
        filters: &[Filter],
        field: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Vec<Value> {
        self.index.values(kind, filters, field, may_read)
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
            .filed(kind, id)
            .is_some_and(may_read)
            .then_some(held)
    }

    /// Every resource whose fields or content (its rich text, lists and maps) name the content at
    /// `address` and whose readers `may_read` admits, by its kind and id
    /// ([`Index::naming_content`]): whoever reads one of them reads that content by its address,
    /// and nobody else does.
    #[must_use]
    pub fn naming_content(
        &self,
        address: &str,
        may_read: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, String)> {
        // A resource naming it in a field and in its content is one resource.
        self.index
            .naming_content(address)
            .filter(|(_, _, readers)| may_read(readers))
            .map(|(kind, id, _)| (kind.to_owned(), id.to_owned()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Every resource nested directly in the resource `id` of `kind`, each by its kind, its id and
    /// what the index holds of it: a message's attachments, under each collection its type declares.
    /// Whoever reads them is the caller's to decide; none for a kind this store declares no type
    /// of.
    #[must_use]
    pub fn nested(&self, kind: &str, id: &str) -> Vec<(String, String, Indexed)> {
        let Some(ty) = self.schema.get(kind) else {
            return Vec::new();
        };
        let kinds: BTreeSet<&str> = ty.nested.iter().map(|(_, kind)| kind.as_str()).collect();
        let mut nested = Vec::new();
        for kind in kinds {
            let mut query = Query::of(kind, 256).filter(Filter::Is(
                collab_types::store::PARENT.to_owned(),
                Value::Text(id.to_owned()),
            ));
            loop {
                let page = self.index.query(&query, &|_| true);
                nested.extend(
                    page.resources
                        .into_iter()
                        .map(|(id, held)| (kind.to_owned(), id, held)),
                );
                match page.next {
                    Some(next) => query.after = Some(next),
                    None => break,
                }
            }
        }
        nested
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
            self.removed.contains_key(key)
                && self
                    .readers_of(&key.0, &key.1)
                    .is_some_and(|readers| may_read(&readers))
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
        for change in commit.changes {
            if let Change::Put { kind, id, .. } = change {
                self.file_by_rule(kind, id);
            }
        }
        for change in commit.changes {
            let (Change::Put { kind, id, .. } | Change::Remove { kind, id }) = change;
            self.follow_live(kind, id);
        }
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
            .find(|version| version.kind == collab_types::history::VersionKind::Removed)?
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
        let mut made: Option<&collab_types::history::Version> = None;
        let mut last: Option<&collab_types::history::Version> = None;
        let mut changed: BTreeMap<&str, &collab_types::history::Version> = BTreeMap::new();
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
                let newer = |held: Option<&collab_types::history::Version>| {
                    held.is_none_or(|held| held.order() <= version.order())
                };
                if version.kind == collab_types::history::VersionKind::Made && made.is_none() {
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
        let at = |version: &collab_types::history::Version| {
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

    /// The document `doc` is written, read in or let go: [`Self::newest`] places it again.
    fn written(&mut self, doc: &str) {
        self.recent
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .written(doc);
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
        self.written(doc);
        self.docs
            .get_mut(doc)
            .map(Arc::make_mut)
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))
    }

    /// Drafts the writes made from now on as `actor`'s, at the second `time` (Unix seconds),
    /// which every device repairing the same record derives alike: the two devices' repairs are
    /// then the one change, and a workspace that takes both holds one copy. `None` drafts them
    /// as this device writes them.
    pub fn drafting(&mut self, under: Option<(automerge::ActorId, i64)>) {
        self.drafting = under;
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
        let under = self.drafting.clone();
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
        let called = self.stamp_for(&doc_id, stamp, &held_by_put(kind, id, &resource));
        let doc = self.doc(&doc_id)?;
        // A put of what the document holds already changes nothing, and writes nothing.
        let change = put_fields(doc, kind, id, &resource, &called, under.as_ref())?;
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
        self.kept_call(&doc_id, &called);
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
        let stamp = stamp.for_change(&held_by_put(kind, id, resource));
        let Some(change) =
            put_fields(&mut draft, kind, id, resource, &stamp, None).map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let received = Received {
            change,
            author,
            claimed: None,
        };
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
        let under = self.drafting.clone();
        let called = self.stamp_for(&doc_id, stamp, &held_record(Kept::nothing(), kind, id));
        let doc = self.doc(&doc_id)?;
        if let Some((actor, _)) = &under {
            doc.set_actor(actor.clone());
        }
        let mut tx = doc.transaction();
        if tx.get(ROOT, held_key(id))?.is_some() {
            tx.delete(ROOT, held_key(id))?;
        }
        let (made, _) = tx.commit_with(commit_options(under.as_ref(), &called));
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
        if made.is_some() {
            self.kept_call(&doc_id, &called);
        }
        Ok(Written {
            doc: doc_id,
            readers,
            change,
        })
    }

    /// Marks the history of the resource `id` of `kind` as `stamp`, a verb of history
    /// ([`crate::HISTORY_VERBS`]): one change with no operations in the resource's document, so
    /// every holder reads the mark where the resource stood when it was made.
    ///
    /// # Errors
    /// When there is no such resource, or its document cannot be read.
    pub fn mark(&mut self, kind: &str, id: &str, stamp: &Stamp) -> Result<Written, StoreError> {
        let doc_id = self
            .index
            .get(kind, id)
            .ok_or_else(|| StoreError::Missing {
                kind: kind.to_owned(),
                id: id.to_owned(),
            })?
            .doc
            .clone();
        let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
        let called = self.stamp_for(&doc_id, stamp, &held_record(Kept::mark(), kind, id));
        self.kept_call(&doc_id, &called);
        let doc = self.doc(&doc_id)?;
        let hash = doc.empty_commit(CommitOptions::default().with_message(called.message()));
        let change = doc
            .get_change_by_hash(&hash)
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let heads = doc.get_heads();
        self.commit(&Commit {
            doc: &doc_id,
            readers: &readers,
            heads: &heads,
            changes: &[],
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
        // The resources first, then their contents: a resource the undo puts back holds its
        // content again, and the content of one it removes goes with it.
        let mut docs: Vec<String> = self.index.documents().map(str::to_owned).collect();
        docs.sort_by_key(|doc| collab_types::touch::content_resource(doc).is_some());
        let mut written = Vec::new();
        for doc_id in docs {
            let readers = self.index.readers(&doc_id).unwrap_or_default().to_owned();
            // A step through history says what it takes back in arguments of its own.
            let called = self.stamp_for(&doc_id, stamp, &Kept::mark());
            let Some(doc) = self.existing(&doc_id)? else {
                continue;
            };
            let Some(change) =
                collab_types::history::revert::revert_stamped(doc, invocation, &called)?
            else {
                continue;
            };
            self.kept_call(&doc_id, &called);
            self.apply(&doc_id, &readers, change.clone())?;
            written.push(Written {
                doc: doc_id,
                readers,
                change,
            });
        }
        Ok(written)
    }

    /// The invocations of `invoker`'s last `steps` steps still in effect, newest first: a step is
    /// one invocation, and a change an undo took back is no step of its own until a redo brings it
    /// back. With `redo`, the invocations of the undos of theirs still in effect instead, newest
    /// first: taking each back is bringing back the step it took back.
    ///
    /// Read from the change graph of every document the store holds that keeps a record of
    /// `kinds` (every document when it names none), so it says the same on every replica and after
    /// a restart, and needs no list of what the caller wrote. What an app's `undo` of its last
    /// steps takes back, where the store was opened for the call that asked.
    ///
    /// What makes a record — the platform's own making of a package's root, and its initial
    /// content — is housekeeping about the record rather than a step of whoever asked for it, so
    /// no undo takes it back: [`tilo_actions::CREATE_ACTION`] and a change an [`ChangeKind`]
    /// outside the content name no step.
    ///
    /// [`ChangeKind`]: tilo_actions::ChangeKind
    pub fn steps(
        &mut self,
        steps: usize,
        invoker: &str,
        kinds: &[String],
        redo: bool,
    ) -> Vec<String> {
        struct Step {
            moment: u64,
            op: u64,
            invocation: String,
            undoes: Option<String>,
        }
        let named = Stamp {
            invoker: invoker.to_owned(),
            ..Stamp::default()
        };
        // A step names nobody the store could take back: no change of it is any caller's.
        let Some(by) = named.invoked_by() else {
            return Vec::new();
        };
        // The documents a record of `kinds` is kept in, by what each holds: a content document by
        // the resource its id names, a store document by the resources the index places in it.
        let of_kinds = |kind: &str| kinds.is_empty() || kinds.iter().any(|named| named == kind);
        let mut documents: Vec<String> = Vec::new();
        let mut emptied: Vec<String> = Vec::new();
        for document in self.index.documents() {
            let kept = if let Some((kind, _)) = collab_types::touch::content_resource(document) {
                of_kinds(&kind)
            } else if self.index.held(document).next().is_none() {
                emptied.push(document.to_owned());
                false
            } else {
                self.index.held(document).any(|(kind, _)| of_kinds(kind))
            };
            if kept {
                documents.push(document.to_owned());
            }
        }
        // A document whose records were all removed holds none now, and the kinds it held are
        // read from its own history: removing the last of them is still a step of their app.
        for document in emptied {
            if let Ok(Some(doc)) = self.existing(&document)
                && kinds_held(doc).iter().any(|kind| of_kinds(kind))
            {
                documents.push(document);
            }
        }
        let mut all: Vec<Step> = Vec::new();
        for document in documents {
            let Ok(Some(doc)) = self.existing(&document) else {
                continue;
            };
            for change in doc.get_changes(&[]) {
                let Some(stamp) = collab_types::Stamp::of(&change) else {
                    continue;
                };
                // What makes a record is the platform's, not the step of a person naming the
                // record's kind: a root made at a host's start names the person it made it for,
                // and undo would otherwise take that making back instead of their last edit.
                if stamp.verb == tilo_actions::CREATE_ACTION {
                    continue;
                }
                // Housekeeping and content from outside are no one's step
                // ([`tilo_actions::ChangeKind`]).
                if stamp
                    .context
                    .as_ref()
                    .is_some_and(|context| !context.change.is_content())
                {
                    continue;
                }
                if stamp.invocation.is_empty() || stamp.invoked_by().as_ref() != Some(&by) {
                    continue;
                }
                all.push(Step {
                    moment: stamp.moment,
                    op: change.start_op().get(),
                    invocation: stamp.invocation,
                    undoes: stamp.undoes,
                });
            }
        }
        // An undo stands while nothing has taken it back since: the newest changes decide which
        // steps are in effect.
        let undos: Vec<(String, String)> = all
            .iter()
            .filter_map(|step| Some((step.invocation.clone(), step.undoes.clone()?)))
            .collect();
        let undone: std::collections::HashSet<&str> = undos
            .iter()
            .filter(|(undo, _)| !undos.iter().any(|(_, taken)| taken == undo))
            .map(|(_, taken)| taken.as_str())
            .collect();
        all.sort_by_key(|step| std::cmp::Reverse((step.moment, step.op)));
        let mut seen = std::collections::HashSet::new();
        let mut picked = Vec::new();
        for step in all {
            let in_effect = !undone.contains(step.invocation.as_str());
            let claimed = if redo {
                step.undoes.is_some() && in_effect
            } else {
                step.undoes.is_none() && in_effect
            };
            if !claimed || !seen.insert(step.invocation.clone()) {
                continue;
            }
            picked.push(step.invocation);
            if picked.len() >= steps {
                break;
            }
        }
        picked
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
        self.written(doc);
        Ok(self.docs.get_mut(doc).map(Arc::make_mut))
    }

    /// Whether the document `doc` holds the change `hash`.
    pub fn has_change(&mut self, doc: &str, hash: &ChangeHash) -> bool {
        self.existing(doc)
            .ok()
            .flatten()
            .is_some_and(|held| held.get_change_by_hash(hash).is_some())
    }

    /// The document the content of the resource `id` of `kind` is written to: `None` for a
    /// resource the store holds none of.
    #[must_use]
    pub fn content_doc(&self, kind: &str, id: &str) -> Option<String> {
        self.content_of(kind, id).ok().map(|(doc, _)| doc)
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

    /// Whether `kind`'s type names who reads a resource of it: a write founding one names no
    /// readers, and the store places it by the type's rule.
    #[must_use]
    pub fn reads_by_rule(&self, kind: &str) -> bool {
        self.rules.contains_key(kind)
    }

    /// The changes every holder makes alike to found the content of the resource `id` of `kind`
    /// in this store's workspace:
    /// the founding of the containers its type declares ([`collab_types::document::founding`]),
    /// none for a kind that declares none, and the steps to the version its type stands at
    /// ([`collab_types::document::founding::chain`]). `None` for a kind this store declares no
    /// type of.
    ///
    /// # Errors
    /// When the founding or a step cannot be made ([`collab_types::document::founding::chain`]).
    pub fn founding(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<Vec<AmChange>>, automerge::AutomergeError> {
        self.schema
            .get(kind)
            .map(|ty| {
                collab_types::document::founding::chain(
                    self.scope(kind, id),
                    ty.version,
                    &ty.founding,
                )
            })
            .transpose()
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

    /// The document holding the content of the resource `id` of `kind`, as it stands: what a
    /// process showing the record reads its changes from. `None` while nothing was written to
    /// it.
    ///
    /// # Errors
    /// When there is no such resource, or its content document cannot be read.
    pub fn content_document(
        &mut self,
        kind: &str,
        id: &str,
    ) -> Result<Option<&Automerge>, StoreError> {
        let (doc_id, _) = self.content_of(kind, id)?;
        Ok(self.existing(&doc_id)?.map(|doc| &*doc))
    }

    /// `changes`, made to the content of the resource `id` of `kind` by the process that shows
    /// it, as the writes that keep them, in order: each is judged and applied as any write is,
    /// and keeps its hash, so the process's copy and the store hold the same changes.
    ///
    /// # Errors
    /// When there is no such resource.
    pub fn content_writes(
        &self,
        kind: &str,
        id: &str,
        changes: Vec<AmChange>,
    ) -> Result<Vec<Written>, StoreError> {
        let (doc, readers) = self.content_of(kind, id)?;
        Ok(changes
            .into_iter()
            .map(|change| Written {
                doc: doc.clone(),
                readers: readers.clone(),
                change,
            })
            .collect())
    }

    /// The ids of the resources of `kind` the store holds, whoever may read them.
    pub fn ids_of<'a>(&'a self, kind: &str) -> impl Iterator<Item = &'a str> + 'a {
        self.index.ids_of(kind)
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
        // A stamp whose arguments the change keeps by what it holds is stamped once the edit is
        // made, by what the content holds then; any other before.
        let (early, called_before) = if stamp.keeps_by_held() {
            (None, self.called.clone())
        } else {
            (Some(self.stamp_for(&doc_id, stamp, &Kept::nothing())), None)
        };
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
        let called = early.unwrap_or_else(|| {
            let mut held = held_record(Kept::nothing(), kind, id);
            held_in(&tx, &ROOT, &mut held);
            named_in(called_before.as_ref(), &doc_id, stamp.for_change(&held))
        });
        tx.commit_with(CommitOptions::default().with_message(called.message()));
        if doc.get_heads() == before {
            return Ok((answer, None));
        }
        self.kept_call(&doc_id, &called);
        let doc = self.doc(&doc_id)?;
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
        self.note_named_content(&doc_id);
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
        let new_id = content_id(to, kind, id);
        let under = self.drafting.clone();
        let held = held_record(Kept::nothing(), kind, id);
        let (emptying, copying) = (
            self.stamp_for(&old_id, stamp, &held),
            self.stamp_for(&new_id, stamp, &held),
        );
        let Some(old) = self.existing(&old_id)? else {
            return Ok(Vec::new());
        };
        if old.length(ROOT) == 0 {
            return Ok(Vec::new());
        }
        let copy = old.fork();
        if let Some((actor, _)) = &under {
            old.set_actor(actor.clone());
        }
        let mut tx = old.transaction();
        let keys: Vec<String> = tx.keys(ROOT).collect();
        for key in keys {
            tx.delete(ROOT, key.as_str())?;
        }
        tx.commit_with(commit_options(under.as_ref(), &emptying));
        let emptied = old
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        let new = self.doc(&new_id)?;
        if let Some((actor, _)) = &under {
            new.set_actor(actor.clone());
        }
        let mut tx = new.transaction();
        copy_object(&copy, &ROOT, &mut tx, &ROOT)?;
        tx.commit_with(commit_options(under.as_ref(), &copying));
        let copied = new
            .get_last_local_change()
            .ok_or(StoreError::Document(automerge::AutomergeError::Fail))?;
        self.note_named_content(&old_id);
        self.note_named_content(&new_id);
        self.note_group(&old_id, stamp);
        self.note_group(&new_id, stamp);
        self.kept_call(&old_id, &emptying);
        self.kept_call(&new_id, &copying);
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

    /// The indexed fields of the resource `id` of `kind`, as JSON: a scalar as itself, a set as
    /// the list of its members. `None` for a resource the store holds none of.
    #[must_use]
    pub fn fields_of(
        &self,
        kind: &str,
        id: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.index
            .get(kind, id)
            .map(|held| fields_json(&held.fields))
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
    /// them now), its `content` beside them, and the resources it is nested in, nearest first,
    /// with what each holds.
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
        let mut parents: Vec<(String, String, serde_json::Map<String, serde_json::Value>)> =
            Vec::new();
        let mut nested = parent;
        while let Some(parent) = nested {
            if parents.len() >= MOST_PARENTS || parents.iter().any(|(_, seen, _)| *seen == parent) {
                break;
            }
            let kind = self.index.kind_anywhere(&parent).unwrap_or_default();
            let held = self.index.get(&kind, &parent);
            let mut fields = held
                .map(|held| fields_json(&held.fields))
                .unwrap_or_default();
            // What it says when a trigger reads a field of it a nested item is found by (a
            // conversation's people), as well as what the index holds of it.
            if let Some(serde_json::Value::Object(content)) = self.content_json(&kind, &parent) {
                fields.extend(content);
            }
            nested = held.and_then(|held| held.parent.clone());
            parents.push((kind, parent, fields));
        }
        Wrote {
            kind: kind.to_owned(),
            id: id.to_owned(),
            holds,
            parents,
        }
    }

    /// The versions of the resource `id` of `kind`, oldest first: each change that made, changed
    /// or removed it, or changed its content, as its action stamped it. One whose document is
    /// not in memory has none.
    #[must_use]
    pub fn versions(&self, kind: &str, id: &str) -> Vec<collab_types::history::Version> {
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
    pub fn version_end(
        &self,
        kind: &str,
        id: &str,
        last: bool,
    ) -> Option<collab_types::history::Version> {
        let mut touches = self
            .touches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (held, readers) = self.placed(kind, id)?;
        let mut ends: Vec<collab_types::history::Version> = Vec::new();
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
                        own.iter().max_by_key(|version| version.order())
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
            ends.into_iter()
                .max_by_key(collab_types::history::Version::order)
        } else {
            ends.into_iter().min_by_key(|version| {
                (
                    version.kind != collab_types::history::VersionKind::Made,
                    version.order(),
                )
            })
        }?;
        if last
            && end.kind == collab_types::history::VersionKind::Removed
            && let Some(removed) = self.removed.get(&(kind.to_owned(), id.to_owned()))
        {
            end.parent.clone_from(&removed.parent);
        }
        Some(end)
    }

    /// [`Self::versions`], with the cache of what each document's changes touched at hand.
    fn versions_unlocked(
        &self,
        touches: &mut HashMap<String, collab_types::history::Touches>,
        kind: &str,
        id: &str,
    ) -> Vec<collab_types::history::Version> {
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
        versions.sort_by_key(collab_types::history::Version::order);
        // Where a removed resource stood, which its removal says.
        if let Some(removed) = self.removed.get(&(kind.to_owned(), id.to_owned()))
            && let Some(last) = versions
                .iter_mut()
                .rev()
                .find(|version| version.kind == collab_types::history::VersionKind::Removed)
        {
            last.parent.clone_from(&removed.parent);
        }
        versions
    }

    /// The resource's versions, including documents evicted or indexed from snapshots: each is
    /// read back from the source first. With `end`, only the first version, or the last with
    /// `true`. The caller checks the resource's readers before asking for its history.
    ///
    /// # Errors
    /// A saved document does not decode.
    pub fn read_versions(
        &mut self,
        kind: &str,
        id: &str,
        end: Option<bool>,
    ) -> Result<Vec<collab_types::history::Version>, StoreError> {
        self.read_back_history(kind, id)?;
        Ok(match end {
            Some(last) => self.version_end(kind, id, last).into_iter().collect(),
            None => self.versions(kind, id),
        })
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
    ) -> (Vec<(String, String, collab_types::history::Version)>, bool) {
        let prefix = format!("{within}/");
        let under = |id: &str| id == within || id.starts_with(&prefix);
        let mut newest: Vec<(String, String, collab_types::history::Version)> = Vec::new();
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
                let mut last: Option<collab_types::history::Version> = None;
                for doc in [doc_id, content.as_str()] {
                    let Some(held) = self.docs.get(doc) else {
                        continue;
                    };
                    let read = touches.entry(doc.to_owned()).or_default();
                    read.read(held, doc);
                    if let Some(version) = read.versions_of(within).and_then(|all| all.last())
                        && last
                            .as_ref()
                            .is_none_or(|was| was.order() <= version.order())
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
        for (doc_id, _) in &docs {
            if let Some(doc) = self.docs.get(*doc_id) {
                touches
                    .entry((*doc_id).to_owned())
                    .or_default()
                    .read(doc, doc_id);
            }
        }
        // What was written in a resource's content, in a document of its own: the documents in
        // the order of their newest version, each joining the walk once the page reaches it.
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut contents = recent.read(&self.docs, &mut touches).peekable();
        // The resource a content document holds the content of, when the page lists it.
        let lists = |kind: &str, id: &str| {
            under(id)
                && id != within
                && self
                    .index
                    .get(kind, id)
                    .is_some_and(|held| self.index.readers(&held.doc).is_some_and(may_read))
        };
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
                // Each content document as recent as the walk has come to joins it.
                while let Some((newest, content)) = contents.peek().copied()
                    && next.peek().is_none_or(|(order, _)| newest >= *order)
                {
                    contents.next();
                    let Some((kind, id)) = content_resource(content) else {
                        continue;
                    };
                    if !lists(kind, id) {
                        continue;
                    }
                    let Some(mut walk) =
                        touches.get(content).map(|held| held.by_newest().peekable())
                    else {
                        continue;
                    };
                    if let Some((order, _, _)) = walk.peek() {
                        next.push((*order, walks.len()));
                    }
                    walks.push((content, Some((kind, id)), walk));
                }
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
        drop(contents);
        drop(recent);
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
        // (Its type's rule is judged below, once the documents' reading is let go.)
        for (kind, id) in self.removed.keys() {
            if !under(id)
                || !self
                    .placed(kind, id)
                    .is_some_and(|(_, readers)| may_read(&readers))
            {
                continue;
            }
            if let Some(last) = self.versions_unlocked(&mut touches, kind, id).pop() {
                newest.push((kind.clone(), id.clone(), last));
            }
        }
        drop(touches);
        // A record whose type names its readers is listed to them, wherever it is held.
        newest.retain(|(kind, id, _)| {
            !self.rules.contains_key(kind.as_str())
                || self
                    .readers_of(kind, id)
                    .is_some_and(|readers| may_read(&readers))
        });
        newest.sort_by_key(|(_, _, version)| std::cmp::Reverse(version.order()));
        let more = newest.len() > limit;
        newest.truncate(limit);
        (newest, more)
    }

    /// Every version stamped as a step of the group `group` ([`Stamp::group`]), of every
    /// resource it touched, by kind and id: what a review of that work lists. Reads only the
    /// documents a change of the group is in.
    #[must_use]
    pub fn grouped(&self, group: &str) -> Vec<(String, String, collab_types::history::Version)> {
        let mut out = self.stamped(self.groups.get(group), &|stamp| {
            stamp.group.as_ref().is_some_and(|held| held.id == group)
        });
        // A resource made in or removed from another changed that one too: its collection.
        let mut holders: Vec<(String, String, collab_types::history::Version)> = Vec::new();
        for (kind, id, version) in &out {
            let made = match version.kind {
                collab_types::history::VersionKind::Made => true,
                collab_types::history::VersionKind::Removed => false,
                collab_types::history::VersionKind::Changed => continue,
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
            let Some(parent_kind) = self.kind_of_any(&parent) else {
                continue;
            };
            let nested = collab_types::history::Nested {
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
                collab_types::history::Version {
                    stamp: version.stamp.clone(),
                    kind: collab_types::history::VersionKind::Changed,
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

    /// Every resource made by a change stamped as made from each of `ids` ([`Stamp::from`]),
    /// by kind and id, in their order: what was made from each, wherever it was placed. A
    /// single resource is a list of one.
    #[must_use]
    pub fn made_from_many(&self, ids: &[String]) -> Vec<Vec<(String, String)>> {
        let mut docs: BTreeSet<String> = BTreeSet::new();
        for id in ids {
            docs.extend(self.made_from.get(id).into_iter().flatten().cloned());
        }
        // One walk over the documents, each read once however many of `ids` it answers.
        let stamped = self.stamped(Some(&docs), &|stamp| {
            stamp
                .from
                .as_ref()
                .is_some_and(|from| ids.contains(&from.id))
        });
        ids.iter()
            .map(|id| {
                let mut made: Vec<&(String, String, collab_types::history::Version)> = stamped
                    .iter()
                    .filter(|(_, _, version)| {
                        version.kind == collab_types::history::VersionKind::Made
                            && version
                                .stamp
                                .as_ref()
                                .and_then(|stamp| stamp.from.as_ref())
                                .is_some_and(|from| from.id == *id)
                    })
                    .collect();
                // In the order they were made, each once: by the time their writer recorded,
                // then their change's order in its document. Each carries a stamp.
                made.sort_by_key(|(_, _, v)| (v.stamp.as_ref().map(|s| s.at.clone()), v.op));
                let mut out: Vec<(String, String)> = Vec::with_capacity(made.len());
                for (kind, id, _) in made {
                    if !out.iter().any(|(_, held)| held == id) {
                        out.push((kind.clone(), id.clone()));
                    }
                }
                out
            })
            .collect()
    }

    /// Every resource made by a change stamped as made from the resource `id`
    /// ([`Stamp::from`]), by kind and id: what was made from it, wherever it was placed.
    #[must_use]
    pub fn made_from(&self, id: &str) -> Vec<(String, String)> {
        self.made_from_many(&[id.to_owned()])
            .pop()
            .unwrap_or_default()
    }

    /// The versions the changes stamped as `stamped` admits made, of every resource they
    /// touched, by kind and id, reading only the documents `docs`.
    fn stamped(
        &self,
        docs: Option<&BTreeSet<String>>,
        stamped: &dyn Fn(&Stamp) -> bool,
    ) -> Vec<(String, String, collab_types::history::Version)> {
        let mut out = Vec::new();
        for doc_id in docs.into_iter().flatten() {
            let Some(doc) = self.docs.get(doc_id) else {
                continue;
            };
            let content = collab_types::touch::content_resource(doc_id);
            for (version, id) in collab_types::history::stamped(doc, doc_id, stamped) {
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

    /// The resource `id` of `kind` as it stood at a version that is a change of another
    /// document: of a resource nested in it, or of one it is nested in, read across the
    /// documents at the cut that change makes.
    fn at_cut(
        &self,
        kind: &str,
        id: &str,
        change: &str,
    ) -> Option<(Option<Indexed>, Option<serde_json::Value>)> {
        let cut = self.cut_of(id, change)?;
        let (index_doc, readers) = self.placed(kind, id)?;
        let held = self.docs.get(&index_doc)?;
        let heads = cut.heads(held);
        let indexed = read_as_of(held, Some(&heads), id)
            .filter(|(held_kind, _)| held_kind == kind)
            .map(|(_, resource)| Indexed {
                doc: index_doc.clone(),
                parent: resource.parent,
                order: resource.order,
                fields: resource.fields,
            });
        let content = self
            .docs
            .get(&content_id(&readers, kind, id))
            .and_then(|doc| {
                let heads = cut.heads(doc);
                let look = crate::wire::AsOf {
                    doc,
                    heads: Some(&heads),
                };
                (look.length(&ROOT) > 0).then(|| look.json(&ROOT))
            });
        Some((indexed, content))
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
        let Some(written) = held
            .get_change_by_hash(&hash)
            .or_else(|| content.and_then(|doc| doc.get_change_by_hash(&hash)))
            .and_then(|change| Stamp::of(&change))
        else {
            // A change of what it holds, or of what holds it, is a version of it too: it is
            // read as it stood then, at the cut that change makes.
            return after.then(|| self.at_cut(kind, id, change)).flatten();
        };
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

    /// The outside service kind the type `kind` declares (`imap.message`), for a kind an origin
    /// keeps in step with a service; `None` for any other kind.
    #[must_use]
    pub fn origin_of(&self, kind: &str) -> Option<&str> {
        self.schema.get(kind).and_then(|ty| ty.origin.as_deref())
    }

    /// Takes the types `schema` declares into those a peer's change is checked against.
    ///
    /// A type's declaration only adds to the one the store holds of it: fields and containers
    /// keep their names and kinds, so the records the earlier version wrote keep reading. A
    /// declaration that drops, moves, renames or retypes one is refused, and the store keeps
    /// what it holds.
    ///
    /// # Errors
    /// What a newer version of a type drops, moves, renames or retypes.
    pub fn declare(&mut self, schema: Schema) -> Result<(), String> {
        self.refuses_evolution(&schema)?;
        // What the store holds of a kind whose rule it takes now is read as that rule says.
        let ruled: Vec<String> = schema
            .readers
            .keys()
            .filter(|kind| !self.rules.contains_key(kind.as_str()))
            .cloned()
            .collect();
        self.rules.extend(schema.readers);
        for kind in ruled {
            for id in self.index.ids(&kind) {
                self.file_by_rule(&kind, &id);
            }
        }
        let mut live_kinds = Vec::new();
        for ty in schema.types {
            let was = self.schema.get(&ty.name).and_then(|held| held.live.clone());
            if was != ty.live {
                live_kinds.push(ty.name.clone());
            }
            self.schema.types.retain(|held| held.name != ty.name);
            self.types_by.insert(ty.name.clone(), schema.kind.clone());
            self.schema.types.push(ty);
        }
        // What the store held before a kind said when it is live is live from now on, and what
        // it held live under an earlier condition is read again.
        for kind in live_kinds {
            let held: BTreeSet<String> = self
                .live
                .keys()
                .filter(|(live, _)| *live == kind)
                .map(|(_, id)| id.clone())
                .chain(self.index.ids(&kind))
                .collect();
            for id in held {
                self.live.remove(&(kind.clone(), id.clone()));
                self.follow_live(&kind, &id);
            }
            self.live_revision = self.live_revision.wrapping_add(1);
        }
        // A kind is one app's, so a verb's write-sets merge kind by kind: one app's verb of a
        // name writes that app's kinds, another's of the same name its own.
        for verb in schema.verbs {
            match self
                .schema
                .verbs
                .iter_mut()
                .find(|known| known.name == verb.name)
            {
                Some(known) => {
                    known.writes.extend(verb.writes);
                    known.outside.extend(verb.outside);
                    known.notice |= verb.notice;
                    for (app, declared) in verb.declared {
                        match known.declared.get_mut(&app) {
                            // One app's resources declaring it apart: the most it is able to do,
                            // and the field its declaration places it under.
                            Some(was) if was.kind < declared.kind => *was = declared,
                            Some(was) => {
                                was.field = was.field.take().or(declared.field);
                            }
                            None => {
                                known.declared.insert(app, declared);
                            }
                        }
                    }
                }
                None => self.schema.verbs.push(verb),
            }
        }
        Ok(())
    }

    /// Whether `schema`'s types may replace the ones the store holds: a type whose version rises
    /// only adds to the one the same surface declared, since records the older version wrote
    /// keep reading. Two apps' types of one name are unrelated, so only the type a surface's own
    /// kind last declared is compared.
    ///
    /// # Errors
    /// What a newer version of a type drops, moves, renames or retypes.
    fn refuses_evolution(&self, schema: &Schema) -> Result<(), String> {
        for ty in &schema.types {
            if let Some(was) = self.schema.get(&ty.name)
                && self
                    .types_by
                    .get(&ty.name)
                    .is_some_and(|owner| *owner == schema.kind)
                && ty.version > was.version
            {
                ty.adds_to(was)?;
            }
        }
        Ok(())
    }

    /// The record `id` of `kind` in this store's workspace, as its founding and steps are derived.
    fn scope<'a>(
        &'a self,
        kind: &'a str,
        id: &'a str,
    ) -> collab_types::document::founding::Scope<'a> {
        collab_types::document::founding::Scope {
            workspace: &self.workspace,
            kind,
            id,
        }
    }

    /// Whether `received`, a founding of the record `id` of `kind`, is the one its kind and id
    /// derive in this store's workspace: the one every holder makes alike, and the only founding
    /// admitted.
    fn judge_founding(
        &self,
        schema: Option<&Schema>,
        (kind, id): (&str, &str),
        received: &Received,
    ) -> Result<(), Refusal> {
        let derived = std::iter::once(&self.schema)
            .chain(schema)
            .find_map(|schema| schema.get(kind))
            .map(|ty| {
                collab_types::document::founding::structure(self.scope(kind, id), &ty.founding)
            })
            .transpose()
            .map_err(|error| {
                Refusal::Undeclared(format!(
                    "the founding `{kind}` derives cannot be made: {error}"
                ))
            })?;
        if derived.is_some_and(|derived| derived.hash() == received.change.hash()) {
            Ok(())
        } else {
            Err(Refusal::Undeclared(format!(
                "a founding of `{id}` that is not the one its kind derives"
            )))
        }
    }

    /// Whether `received`, a migration of the record `id` of `kind` that `named` reads, is the step
    /// its kind and id derive in this store's workspace to a version its type stands at, admitted
    /// whole: `false` for the app's rewrite of its content for a version, decided as any write of
    /// the record's.
    fn judge_migration(
        &self,
        schema: Option<&Schema>,
        (kind, id): (&str, &str),
        named: Option<(u32, bool)>,
        received: &Received,
    ) -> Result<bool, Refusal> {
        let ty = std::iter::once(&self.schema)
            .chain(schema)
            .find_map(|schema| schema.get(kind));
        let version = ty.map_or(0, |ty| ty.version);
        let Some((to, rewrite)) = named.filter(|(to, _)| (1..=version).contains(to)) else {
            return Err(Refusal::Undeclared(format!(
                "a migration of `{id}` to no storage version its kind stands at"
            )));
        };
        if rewrite {
            return Ok(false);
        }
        let derived = ty
            .map(|ty| {
                collab_types::document::founding::migrate(self.scope(kind, id), to, &ty.founding)
            })
            .transpose()
            .map_err(|error| {
                Refusal::Undeclared(format!(
                    "the step `{kind}` derives to version {to} cannot be made: {error}"
                ))
            })?;
        if derived.is_some_and(|derived| derived.hash() == received.change.hash()) {
            Ok(true)
        } else {
            Err(Refusal::Undeclared(format!(
                "a step of `{id}` to version {to} that is not the one its kind derives"
            )))
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
        let undeclared =
            self.schema.types.is_empty() && schema.is_none_or(|schema| schema.types.is_empty());
        let doc = self.doc(doc_id).map_err(|_| Refusal::Unplaced)?;
        let (touched, writes) =
            collab_types::touch::touched(doc, doc_id, &received.change).ok_or(Refusal::Unplaced)?;
        let queued_by = apps_of_queued_effects(doc, &touched);
        // A document's reader set is the one it was placed by, whoever writes it.
        if touched
            .readers
            .as_ref()
            .is_some_and(|written| written != readers)
        {
            return Err(Refusal::Unplaced);
        }
        // A record whose id is bound to a sealed kind is kept as no other: the kind a write names
        // for it never takes it out from under its own keys (`collab_types::sealed_kind`).
        if let Some((id, bound)) = touched.resources.iter().find_map(|(id, touch)| {
            let bound = collab_types::bound_kind(id)?;
            (touch.kind.as_deref().is_some_and(|kind| kind != bound)
                && collab_types::is_sealed_kind(bound))
            .then_some((id, bound))
        }) {
            return Err(Refusal::Undeclared(format!(
                "`{id}` is a record of the sealed `{bound}` and is kept as no other kind"
            )));
        }
        // A store that declares no type has nothing to judge a change by: it admits none, so a
        // writer whose surface was never named here is refused rather than taken on trust.
        if undeclared {
            return Err(Refusal::Undeclared(
                "the store declares no type, so it admits no change".to_owned(),
            ));
        }
        if received.claimed.is_some() {
            let read = collab_types::claim::store_record_kinds(
                doc_id,
                Some(&*doc),
                std::slice::from_ref(&received.change),
            )
            .remove(&received.change.hash().0)
            .unwrap_or_default();
            crate::admit::as_claimed(&read, received.claimed.as_ref())
                .map_err(Refusal::Misclaimed)?;
        }
        // A record's derived founding names no verb: it is admitted only as the change its kind
        // and id derive, the one every holder makes alike.
        if let Some((kind, id)) = content_resource(doc_id)
            && matches!(
                Stamp::read(received.change.message().unwrap_or_default()),
                Some((
                    tilo_actions::stamp::Label::Framework(tilo_actions::FrameworkChange::Genesis),
                    _
                ))
            )
        {
            return self.judge_founding(schema, (kind, id), received);
        }
        let stamp = self.stamp_of(doc_id, &received.change)?;
        // The migration verb's two forms. A record's step to a storage version is admitted only
        // as the one its kind and id derive for that version, up to the version its type stands
        // at: the step every holder makes, whoever sends it. The app's rewrite of its content for
        // a version is decided as any write of the record's, by who may write it, and its verb
        // declares no write-set of its own.
        let migration = content_resource(doc_id).zip(
            stamp
                .as_ref()
                .filter(|stamp| stamp.verb == MIGRATE_VERB)
                .map(migration_of),
        );
        if let Some(((kind, id), named)) = &migration
            && self.judge_migration(schema, (*kind, *id), *named, received)?
        {
            return Ok(());
        }
        let index = &self.index;
        for schema in std::iter::once(&self.schema).chain(schema) {
            crate::admit::declared(
                schema,
                &touched,
                &writes,
                &|id| index.kind_anywhere(id),
                &|kind, id| self.rule_places_apart(kind, id, &writes),
            )
            .map_err(Refusal::Undeclared)?;
            if migration.is_none() {
                crate::admit::within_verb(
                    schema,
                    &touched,
                    &writes,
                    stamp.as_ref(),
                    received.change.len(),
                    &queued_by,
                )
                .map_err(Refusal::Undeclared)?;
            }
        }
        self.judge_readers(doc_id, readers, &touched, &received.change)?;
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
            // A store takes many documents' writes; the calls of writes long taken in are read
            // from their documents again when a change names one.
            if self.calls.len() >= KEPT_CALLS {
                self.calls = tilo_actions::stamp::KeptCalls::default();
            }
            self.calls.keep(&stamp);
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
        // A content document holds no resources: what its content names is noted instead.
        self.note_named_content(doc_id);
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
        self.written(doc_id);
        self.index_resources(doc_id, &readers, None)?;
        for (kind, id, parent) in held {
            if self.index.get(&kind, &id).is_none() {
                self.note_removed(&kind, &id, doc_id, parent);
            }
        }
        Ok(())
    }

    /// `stamp` as a change of `doc` drafted now keeps it: in a staged copy, its call named by
    /// its digest, and kept whole only by the first change of the call to `doc`
    /// ([`Self::kept_call`] notes it), so readers of `doc` receive the call once, with the
    /// changes that name it.
    ///
    /// `held` is what the change holds once written: of a call whose caller signed its argument's
    /// parts, the change keeps only the parts it holds ([`Stamp::for_change`]).
    fn stamp_for(&self, doc: &str, stamp: &Stamp, held: &Kept) -> Stamp {
        named_in(self.called.as_ref(), doc, stamp.for_change(held))
    }

    /// Note that a change of `doc` drafted under `stamp` ([`Self::stamp_for`]) keeps its call.
    fn kept_call(&mut self, doc: &str, stamp: &Stamp) {
        if let (Some(called), Some(_), Some(digest)) =
            (&mut self.called, &stamp.signed, &stamp.call)
        {
            called.insert((doc.to_owned(), digest.clone()));
        }
    }

    /// The stamp `change` of `doc_id` carries, with the call it names by digest read from the
    /// change of its document that keeps it.
    ///
    /// # Errors
    /// [`Refusal::Uncalled`] when no change of the document keeps the call it names, or it
    /// keeps another than the one it names.
    fn stamp_of(&mut self, doc_id: &str, change: &AmChange) -> Result<Option<Stamp>, Refusal> {
        let Some(mut stamp) = Stamp::of(change) else {
            return Ok(None);
        };
        if let Some(named) = stamp.call.as_deref()
            && stamp.signed.is_none()
            && !self.calls.holds(named)
        {
            // Kept by an earlier change of the same write, taken in before the store last
            // started or long enough ago that it was let go: read from the document itself.
            let doc = self.doc(doc_id).map_err(|_| Refusal::Unplaced)?;
            let kept = tilo_actions::stamp::KeptCalls::of_messages(
                doc.get_changes_meta(&[])
                    .iter()
                    .filter_map(|change| change.message.as_deref()),
            );
            self.calls.extend(kept);
        }
        stamp.resolve(&self.calls).map_err(Refusal::Uncalled)?;
        Ok(Some(stamp))
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
        // A record whose type names its readers is read by them wherever it is held
        // ([`Index::filed`]); a removed one held where every member read it, by those its rule
        // gives a record of no fields.
        if self.index.get(kind, id).is_some() {
            return self.index.filed(kind, id).map(str::to_owned);
        }
        let (_, readers) = self.placed(kind, id)?;
        if readers == collab_types::EVERY_MEMBER && self.rules.contains_key(kind) {
            return Some(self.removed_readers(kind, id));
        }
        Some(readers)
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
        self.written(doc);
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
        // A content document is read only once a write needs it: what it names is the snapshot's.
        if let Some((kind, id)) = content_resource(doc) {
            let (kind, id) = (kind.to_owned(), id.to_owned());
            self.index
                .name_in_content(doc, (&kind, &id), restored.names.into_iter().collect());
        }
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
        self.written(doc);
        self.refused.remove(doc);
        self.waiting.remove(doc);
    }

    /// Lets the document `doc` go from memory, answering it saved; its resources stay indexed,
    /// and it is read again from the source when needed.
    pub fn evict(&mut self, doc: &str) -> Option<Vec<u8>> {
        self.written(doc);
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
        self.written(doc);
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

/// The app each queued effect the change `touched` touches says it is for, as `doc` stands:
/// taking an entry back before it goes is that app's, and the entry names it when it is queued.
fn apps_of_queued_effects(
    doc: &Automerge,
    touched: &collab_types::touch::Touched,
) -> std::collections::BTreeMap<String, String> {
    touched
        .resources
        .iter()
        .filter(|(_, touch)| touch.kind.as_deref() == Some(crate::admit::QUEUED_EFFECT_KIND))
        .filter_map(|(id, _)| {
            let (_, held) = read(doc, id)?;
            match held.fields.get(crate::admit::QUEUED_EFFECT_APP)? {
                Cell::One(Value::Text(app)) => Some((id.clone(), app.clone())),
                Cell::Set(_) | Cell::One(_) => None,
            }
        })
        .collect()
}

/// The kinds of the records `doc` held at any of its changes.
fn kinds_held(doc: &Automerge) -> BTreeSet<String> {
    let mut kinds = BTreeSet::new();
    for change in doc.get_changes(&[]) {
        let heads = [change.hash()];
        for key in doc.keys_at(&ROOT, &heads) {
            if key.starts_with(HELD)
                && let Ok(Some((AmValue::Object(ObjType::Map), held))) =
                    doc.get_at(&ROOT, key.as_str(), &heads)
                && let Ok(Some((AmValue::Scalar(kind), _))) = doc.get_at(&held, KIND, &heads)
                && let ScalarValue::Str(kind) = kind.as_ref()
            {
                kinds.insert(kind.to_string());
            }
        }
    }
    kinds
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

/// The options a change drafted under `under` commits with: its message, and the second it says
/// it was made at, so the change every device drafts alike is the same change.
/// `stamp` as a change of `doc` drafted now keeps it, where `called` holds the documents a
/// change of the staged write keeps a call in ([`Store::stamp_for`]).
fn named_in(called: Option<&BTreeSet<(String, String)>>, doc: &str, mut stamp: Stamp) -> Stamp {
    if let Some(called) = called
        && stamp.signed.is_some()
    {
        stamp.name_call(false);
        let digest = stamp.call.clone().unwrap_or_default();
        stamp.name_call(called.contains(&(doc.to_owned(), digest)));
    }
    stamp
}

/// `held` with the record a change writes, its kind and id: what the change addresses.
fn held_record(mut held: Kept, kind: &str, id: &str) -> Kept {
    held.add_text(kind);
    held.add_text(id);
    held
}

/// What a put of `resource` as the record `id` of `kind` holds once written: the record, and
/// every value of its fields and place.
fn held_by_put(kind: &str, id: &str, resource: &Resource) -> Kept {
    let mut held = held_record(Kept::nothing(), kind, id);
    for value in fields_json(&resource.fields).values() {
        held.add(value);
    }
    for text in [&resource.parent, &resource.order].into_iter().flatten() {
        held.add_text(text);
    }
    held
}

/// Every value and key the object `obj` of `doc` holds, into `held`.
fn held_in(doc: &impl automerge::ReadDoc, obj: &ObjId, held: &mut Kept) {
    let value_at = |value: automerge::Value<'_>, inner: ObjId, held: &mut Kept| match value {
        automerge::Value::Object(automerge::ObjType::Text) => {
            held.add_text(&doc.text(&inner).unwrap_or_default());
        }
        automerge::Value::Object(_) => held_in(doc, &inner, held),
        automerge::Value::Scalar(scalar) => match scalar.as_ref() {
            ScalarValue::Str(text) => held.add_text(text),
            ScalarValue::Int(number) => held.add(&serde_json::Value::from(*number)),
            ScalarValue::Uint(number) => held.add(&serde_json::Value::from(*number)),
            ScalarValue::F64(number) => held.add(&serde_json::Value::from(*number)),
            ScalarValue::Counter(counter) => {
                held.add(&serde_json::Value::from(i64::from(counter)));
            }
            ScalarValue::Boolean(flag) => held.add(&serde_json::Value::Bool(*flag)),
            _ => held.add(&serde_json::Value::Null),
        },
    };
    match doc.object_type(obj) {
        Ok(automerge::ObjType::Map | automerge::ObjType::Table) => {
            for key in doc.keys(obj).collect::<Vec<_>>() {
                held.add_text(&key);
                if let Ok(Some((value, inner))) = doc.get(obj, key.as_str()) {
                    value_at(value, inner, held);
                }
            }
        }
        Ok(automerge::ObjType::List) => {
            for at in 0..doc.length(obj) {
                if let Ok(Some((value, inner))) = doc.get(obj, at) {
                    value_at(value, inner, held);
                }
            }
        }
        Ok(automerge::ObjType::Text) => held.add_text(&doc.text(obj).unwrap_or_default()),
        Err(_) => {}
    }
}

fn commit_options(under: Option<&(automerge::ActorId, i64)>, stamp: &Stamp) -> CommitOptions {
    let options = CommitOptions::default().with_message(stamp.message());
    match under {
        Some((_, time)) => options.with_time(*time),
        None => options,
    }
}

fn put_fields(
    doc: &mut Automerge,
    kind: &str,
    id: &str,
    resource: &Resource,
    stamp: &Stamp,
    under: Option<&(automerge::ActorId, i64)>,
) -> Result<Option<AmChange>, StoreError> {
    if let Some((actor, _)) = under {
        doc.set_actor(actor.clone());
    }
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
    let (hash, _) = tx.commit_with(commit_options(under, stamp));
    Ok(hash.and_then(|_| doc.get_last_local_change()))
}

/// One document a restore writes: where it goes back to, and which resources of it go back.
struct Restoring {
    doc: String,
    readers: String,
    /// The document's heads as it stood at the version.
    target: Vec<ChangeHash>,
    /// The resources it brings back, by id: those an index document holds, or the one whose
    /// content a content document is.
    ids: BTreeSet<String>,
    /// It is a resource's content, which goes back whole.
    content: bool,
}

/// The resources a restore brings back, by the documents they are in.
#[derive(Default)]
struct Held {
    /// Each document's reader set and the resources of it.
    docs: BTreeMap<String, (String, BTreeSet<String>)>,
    /// Each resource's kind.
    kinds: BTreeMap<String, String>,
}

impl Held {
    fn add(&mut self, doc: &str, readers: &str, kind: &str, id: &str) {
        self.docs
            .entry(doc.to_owned())
            .or_insert_with(|| (readers.to_owned(), BTreeSet::new()))
            .1
            .insert(id.to_owned());
        self.kinds.insert(id.to_owned(), kind.to_owned());
    }

    /// The documents they are in, and their contents.
    fn documents(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (doc, (readers, ids)) in &self.docs {
            out.push(doc.clone());
            for id in ids {
                if let Some(kind) = self.kinds.get(id) {
                    out.push(content_id(readers, kind, id));
                }
            }
        }
        out
    }
}

pub use collab_types::store::Review;

impl Store {
    /// Names the version the resource `id` of `kind` stands at now `name`, as `stamp`: a change
    /// of its document with no operations, whose verb is [`Stamp::CHECKPOINT_VERB`], which its
    /// history lists as a version of it, and which a restore can go back to.
    ///
    /// # Errors
    /// When there is no such resource.
    pub fn checkpoint(
        &mut self,
        kind: &str,
        id: &str,
        name: &str,
        stamp: &Stamp,
    ) -> Result<Written, StoreError> {
        let stamp = Stamp {
            verb: Stamp::CHECKPOINT_VERB.to_owned(),
            args: vec![name.into(), id.into()],
            ..stamp.clone()
        };
        self.mark(kind, id, &stamp)
    }

    /// Brings the resources `resources` (by kind and id), and those nested in them then or now,
    /// back to how they stood at the version `change` (a version's change, or a checkpoint's), as
    /// one change of each document that differs, stamped `stamp`: an invocation of its own,
    /// which an undo takes back. With `only`, just the resources it names go back; with `live`,
    /// the restore is refused unless the documents stand where a review saw them.
    ///
    /// # Errors
    /// When the version is none of theirs, the documents moved since the review, or everything
    /// already stands as it did then.
    pub fn restore_version(
        &mut self,
        resources: &[(String, String)],
        change: &str,
        only: Option<&[String]>,
        live: Option<&[String]>,
        stamp: &Stamp,
    ) -> Result<Vec<Written>, StoreError> {
        let restoring = self.load_restoring(resources, change)?;
        if let Some(live) = live
            && self.review_of(&restoring).live != live
        {
            return Err(StoreError::History(
                "what you reviewed changed since; review it again".to_owned(),
            ));
        }
        let mut context = stamp.action_context();
        context.what = Some(tilo_actions::Did::History(
            tilo_actions::entry::HistoryStep {
                op: tilo_actions::HistoryOp::Restore,
                steps: 0,
                of: None,
                undoes: None,
                verb: String::new(),
            },
        ));
        let stamp = Stamp {
            context: Some(Box::new(context)),
            ..stamp.clone()
        };
        let mut written = Vec::new();
        for part in restoring {
            let ids: Vec<String> = part
                .ids
                .iter()
                .filter(|id| only.is_none_or(|only| only.contains(id)))
                .map(|id| held_key(id))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let Some(doc) = self.docs.get(&part.doc) else {
                continue;
            };
            let mut working = automerge::AutoCommit::from(Automerge::clone(doc));
            // A content goes back whole; a document of resources, only in theirs.
            let keys = if part.content { Vec::new() } else { ids };
            // A document that stands as it did then writes nothing.
            if collab_types::history::steps::restore(&mut working, &part.target, &keys).is_err() {
                continue;
            }
            working.commit_with(CommitOptions::default().with_message(stamp.message()));
            let Some(change) = working.get_last_local_change() else {
                continue;
            };
            self.apply(&part.doc, &part.readers, change.clone())?;
            written.push(Written {
                doc: part.doc,
                readers: part.readers,
                change,
            });
        }
        if written.is_empty() {
            return Err(StoreError::History(
                "it already stands as it did then".to_owned(),
            ));
        }
        Ok(written)
    }

    /// What [`Self::restore_version`] of `resources` to the version `change` would change, their
    /// documents read from the source where they are not in memory.
    ///
    /// # Errors
    /// When the version is none of theirs, or a document does not read.
    pub fn review_restore(
        &mut self,
        resources: &[(String, String)],
        change: &str,
    ) -> Result<Review, StoreError> {
        let restoring = self.load_restoring(resources, change)?;
        Ok(self.review_of(&restoring))
    }

    /// [`Self::review_restore`] of the documents in memory.
    ///
    /// # Errors
    /// When the version is none of theirs.
    pub fn review_restore_held(
        &self,
        resources: &[(String, String)],
        change: &str,
    ) -> Result<Review, StoreError> {
        let restoring = self.restoring(resources, change, &|doc| self.docs.get(doc).cloned())?;
        Ok(self.review_of(&restoring))
    }

    /// [`Self::restoring`], each document it reads read from the source first.
    fn load_restoring(
        &mut self,
        resources: &[(String, String)],
        change: &str,
    ) -> Result<Vec<Restoring>, StoreError> {
        loop {
            let missing = std::cell::RefCell::new(Vec::new());
            let restoring = self.restoring(resources, change, &|doc| {
                let held = self.docs.get(doc).cloned();
                if held.is_none() {
                    missing.borrow_mut().push(doc.to_owned());
                }
                held
            });
            let mut read = false;
            for doc in missing.into_inner() {
                read |= self.existing(&doc)?.is_some();
            }
            if !read {
                return restoring;
            }
        }
    }

    fn review_of(&self, restoring: &[Restoring]) -> Review {
        let mut review = Review::default();
        for part in restoring {
            let Some(doc) = self.docs.get(&part.doc) else {
                continue;
            };
            review
                .live
                .extend(doc.get_heads().iter().map(ToString::to_string));
            for id in &part.ids {
                let differs = if part.content {
                    let now = crate::wire::AsOf { doc, heads: None };
                    let then = crate::wire::AsOf {
                        doc,
                        heads: Some(&part.target),
                    };
                    now.json(&ROOT) != then.json(&ROOT)
                } else {
                    read_as_of(doc, None, id) != read_as_of(doc, Some(&part.target), id)
                };
                if differs {
                    review.differing.push(id.clone());
                }
            }
        }
        review.live.sort_unstable();
        review.live.dedup();
        review.differing.sort_unstable();
        review.differing.dedup();
        review
    }

    /// The documents a restore of `resources` to the version `change` writes: the documents
    /// they are placed in, with the resources nested in them then or now, and the content of
    /// each.
    fn restoring(
        &self,
        resources: &[(String, String)],
        change: &str,
        loaded: &dyn Fn(&str) -> Option<Arc<Automerge>>,
    ) -> Result<Vec<Restoring>, StoreError> {
        let not_theirs = || StoreError::History(format!("`{change}` is no version of these"));
        let mut held = Held::default();
        for (kind, id) in resources {
            if let Some((doc, readers)) = self.placed(kind, id) {
                held.add(&doc, &readers, kind, id);
            }
        }
        // What is nested in them now, then the version's change in one of their documents (a
        // checkpoint's name names its newest checkpoint), and when it was written.
        self.nest(&mut held, loaded, None);
        let version: ChangeHash = match change.parse() {
            Ok(version) => version,
            Err(_) => named(&held, change, loaded).ok_or_else(not_theirs)?,
        };
        let mut moment = None;
        for doc_id in held.documents() {
            if let Some(doc) = loaded(&doc_id)
                && let Some(found) = doc.get_change_by_hash(&version)
            {
                moment = Some(Stamp::of(&found).map_or(0, |stamp| stamp.moment));
                break;
            }
        }
        let moment = moment.ok_or_else(not_theirs)?;
        // What was nested in them then goes back with them too.
        self.nest(&mut held, loaded, Some((version, moment)));
        let mut restoring = Vec::new();
        for (doc_id, (readers, ids)) in held.docs {
            let Some(doc) = loaded(&doc_id) else {
                continue;
            };
            for id in &ids {
                let Some(kind) = held.kinds.get(id) else {
                    continue;
                };
                let content = content_id(&readers, kind, id);
                if let Some(content_doc) = loaded(&content) {
                    restoring.push(Restoring {
                        target: heads_at(&content_doc, version, moment),
                        doc: content,
                        readers: readers.clone(),
                        ids: BTreeSet::from([id.clone()]),
                        content: true,
                    });
                }
            }
            restoring.push(Restoring {
                target: heads_at(&doc, version, moment),
                doc: doc_id,
                readers,
                ids,
                content: false,
            });
        }
        Ok(restoring)
    }

    /// Adds to `held` every resource nested in one it holds, now, or with `then` (a version
    /// and its moment) as it stood then too.
    fn nest(
        &self,
        held: &mut Held,
        loaded: &dyn Fn(&str) -> Option<Arc<Automerge>>,
        then: Option<(ChangeHash, u64)>,
    ) {
        let mut queue: Vec<String> = held.kinds.keys().cloned().collect();
        while let Some(parent) = queue.pop() {
            for doc_id in self.nested_in.get(&parent).into_iter().flatten() {
                let (Some(doc), Some(readers)) = (loaded(doc_id), self.index.readers(doc_id))
                else {
                    continue;
                };
                let target = then.map(|(version, moment)| heads_at(&doc, version, moment));
                for heads in std::iter::once(None).chain(target.as_deref().map(Some)) {
                    let look = crate::wire::AsOf { doc: &doc, heads };
                    for key in look.keys(&ROOT) {
                        let Some(id) = key.strip_prefix(HELD) else {
                            continue;
                        };
                        if held.kinds.contains_key(id) {
                            continue;
                        }
                        if let Some((kind, resource)) = read_as_of(&doc, heads, id)
                            && resource.parent.as_deref() == Some(parent.as_str())
                        {
                            held.add(doc_id, readers, &kind, id);
                            queue.push(id.to_owned());
                        }
                    }
                }
            }
        }
    }
}

/// The newest checkpoint named `name` of the resources `held` holds, in the documents they are in.
fn named(
    held: &Held,
    name: &str,
    loaded: &dyn Fn(&str) -> Option<Arc<Automerge>>,
) -> Option<ChangeHash> {
    held.docs
        .iter()
        .filter_map(|(doc, (_, ids))| Some((loaded(doc)?, ids)))
        .flat_map(|(doc, ids)| {
            doc.get_changes(&[])
                .into_iter()
                .filter(automerge::Change::is_empty)
                .filter_map(|change| {
                    let stamp = Stamp::of(&change)?;
                    let names = stamp.verb == Stamp::CHECKPOINT_VERB
                        && stamp.args.first().and_then(serde_json::Value::as_str) == Some(name)
                        && stamp
                            .args
                            .get(1)
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|id| ids.contains(id));
                    names.then(|| (stamp.moment, change.hash()))
                })
                .collect::<Vec<_>>()
        })
        .max()
        .map(|(_, hash)| hash)
}

/// The heads of `doc` as it stood at the version `version`, written at `moment`: just after it,
/// in the document that holds it; in another, every change written by then.
fn heads_at(doc: &Automerge, version: ChangeHash, moment: u64) -> Vec<ChangeHash> {
    if doc.get_change_by_hash(&version).is_some() {
        return vec![version];
    }
    let mut kept: BTreeSet<ChangeHash> = BTreeSet::new();
    let mut deps: BTreeSet<ChangeHash> = BTreeSet::new();
    for change in doc.get_changes(&[]) {
        let at = Stamp::of(&change).map_or(0, |stamp| stamp.moment);
        if at <= moment && change.deps().iter().all(|dep| kept.contains(dep)) {
            kept.insert(change.hash());
            deps.extend(change.deps().iter().copied());
        }
    }
    kept.retain(|hash| !deps.contains(hash));
    kept.into_iter().collect()
}
