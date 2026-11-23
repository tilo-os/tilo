//! The namespace backend: an Automerge-backed move-tree CRDT, with nodes keyed by a
//! stable `NodeId` and an explicit `parent` register, so rename, move and delete are
//! real move operations: no duplicate parents, delete reparents to the `TRASH`
//! sentinel, and a concurrent cross-move cycle is deterministically broken by
//! [`Namespace::resolve`], so every peer converges identically.
//!
//! Inode numbers are a local `NodeId` to `u64` index; a rename keeps the node id, so
//! the inode stays stable.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, UNIX_EPOCH};

use crate::node::{FileAttr, FileType, INodeNo};
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::{
    FILE_BINDING_VERSION, FileActor, FileBindingV1, FileDocumentId, FileNameV1, FileNodeKind,
    MAX_FILE_NAME_BYTES, TreeChunk,
};
use iroh_blobs::Hash;
use uuid::Uuid;

pub const ROOT_INO: u64 = 1;
pub use fs_protocol::tree::ROOT_ID;
// The document's layout is `fs_protocol::tree`'s: the record that carries the tree reads
// the same document the mount writes.
use fs_protocol::tree::{
    DOCUMENT_ID_KEY, PLACEMENT_NAME_KEY, ROOT_DOCUMENT_ID_KEY, TRASH_ID, document_node_id,
};
/// Bounded fixed-point rounds for sibling name conflicts: each round gives a losing
/// record a longer, record-id-specific name, so a directory settles in a few dozen rounds.
const MAX_STRUCTURAL_CONFLICT_ROUNDS: usize = 64;
const STRUCTURAL_CONFLICT_MARKER: &[u8] = b".sync-conflict-";

pub const MAX_LIVE_RECORD_BLOB_SCAN: usize = 100_000;
mod changes;
mod changesets;
mod chunks;
mod records;
pub use records::ResourceWrite;
use records::{bundle, founding_document};
mod document_index;
#[cfg(test)]
mod index_walk_tests;
#[cfg(test)]
mod journey_tests;
mod kernel_changes;
mod leases;
mod posix;
mod trash;
mod verbs;
pub use changesets::ChangesetMark;
use chunks::Chunks;
pub(crate) use chunks::Obj;
pub use kernel_changes::KernelChanges;
pub use leases::{LeaseRequest, VersionFence, WriteLease};
pub use posix::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, PosixAttrs};
pub use verbs::{
    Changed, FileAt, FileToPut, MOST_COPIED, MOST_LISTED, Page, PlacedFile, Row, Touched, Trashed,
    Written,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveDocumentBlobRoots {
    pub document_id: FileDocumentId,
    pub manifest: Option<Hash>,
    pub thumbnail: Option<Hash>,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveDocumentBlobScanError {
    #[error("live-record blob scan limit must be between 1 and {MAX_LIVE_RECORD_BLOB_SCAN}")]
    InvalidLimit,
    #[error("live namespace exceeds the blob scan limit of {limit} records")]
    LimitExceeded { limit: usize },
    #[error("live namespace record is missing its stable record identity")]
    MissingDocumentIdentity,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindingProjectionError {
    #[error("signed binding does not match a unique stable record")]
    UnknownOrDuplicateDocument,
    #[error("signed binding parent is not a unique live directory")]
    UnknownParent,
    #[error("signed binding changes the immutable node kind")]
    KindMismatch,
    #[error("signed binding destination is occupied by another stable record")]
    OccupiedDestination,
    #[error("signed binding violates namespace move constraints")]
    InvalidMove,
    #[error("signed root binding conflicts with an established workspace root")]
    RootIdentityConflict,
    #[error("signed structural names did not settle deterministically")]
    StructuralConflictLoop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
}

impl NodeKind {
    pub fn to_file_type(self) -> FileType {
        match self {
            NodeKind::File => FileType::RegularFile,
            NodeKind::Dir => FileType::Directory,
        }
    }
    fn from_str(s: &str) -> Self {
        if s == "dir" {
            NodeKind::Dir
        } else {
            NodeKind::File
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub document_id: FileDocumentId,
    pub parent: String,
    pub name: FileNameV1,
    pub kind: NodeKind,
    pub hash: Option<Hash>,
    pub size: u64,
    pub mtime: u64,
    pub author: String,
}

#[derive(Clone, Copy, Debug)]
struct NodeMeta {
    document_id: FileDocumentId,
    kind: NodeKind,
    hash: Option<Hash>,
    size: u64,
    mtime: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InodeQuery {
    /// Also describe an inode no longer reachable from the root, as a kernel handle
    /// opened before its unlink or replacement may.
    pub unlinked: bool,
    pub path: bool,
}

/// One inode as a single FUSE request sees it, read under one namespace lock
/// ([`Namespace::inode_view`]).
#[derive(Clone, Debug)]
pub(crate) struct InodeView {
    pub attr: FileAttr,
    pub hash: Option<Hash>,
    pub live: bool,
    pub path: Option<String>,
}

/// One authoritative, path-current row in the namespace-owned record index.
///
/// The record UUID is the durable identity. `path` is projection metadata: it is
/// replaced on a rename or move and never accepted as an authority token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedDocument {
    pub document_id: FileDocumentId,
    pub node_id: String,
    pub parent_id: String,
    /// The name the node sits under: a page that shows a path derives it from the node when
    /// it asks, so opening a tree never builds one path per file.
    pub name: FileNameV1,
    pub is_dir: bool,
}

#[derive(Clone)]
struct NodeRow {
    document_id: Option<FileDocumentId>,
    parent_id: String,
    name: FileNameV1,
    is_dir: bool,
}

struct NodeWalk {
    tree: TreeIndex,
    rows: HashMap<String, NodeRow>,
    occurrences: HashMap<FileDocumentId, usize>,
}

/// Structural index over every node in the document: where each node sits and, per
/// parent, its children by name. `lookup`, `readdir` and name checks consult it instead
/// of scanning every node. Local placement writes update it node by node; an opaque
/// document import rebuilds it.
#[derive(Clone, Debug, Default)]
struct TreeIndex {
    placement: HashMap<String, (String, FileNameV1)>,
    children: HashMap<String, BTreeMap<FileNameV1, BTreeSet<String>>>,
    /// The nodes carrying each stable record id, live or trashed; a node's record id is
    /// written once, when the node is created or migrated.
    records: HashMap<FileDocumentId, BTreeSet<String>>,
    resource_of: HashMap<String, FileDocumentId>,
}

impl TreeIndex {
    fn bind_document(&mut self, id: &str, document_id: FileDocumentId) {
        if self.resource_of.get(id) == Some(&document_id) {
            return;
        }
        self.unbind_document(id);
        self.records
            .entry(document_id)
            .or_default()
            .insert(id.to_string());
        self.resource_of.insert(id.to_string(), document_id);
    }

    fn unbind_document(&mut self, id: &str) {
        let Some(document_id) = self.resource_of.remove(id) else {
            return;
        };
        if let Some(ids) = self.records.get_mut(&document_id) {
            ids.remove(id);
            if ids.is_empty() {
                self.records.remove(&document_id);
            }
        }
    }

    fn nodes_of(&self, document_id: FileDocumentId) -> impl Iterator<Item = &String> {
        self.records.get(&document_id).into_iter().flatten()
    }

    fn remove(&mut self, id: &str) {
        let Some((parent, name)) = self.placement.remove(id) else {
            return;
        };
        if let Some(names) = self.children.get_mut(&parent) {
            if let Some(ids) = names.get_mut(&name) {
                ids.remove(id);
                if ids.is_empty() {
                    names.remove(&name);
                }
            }
            if names.is_empty() {
                self.children.remove(&parent);
            }
        }
    }

    fn place(&mut self, id: &str, parent: String, name: FileNameV1) {
        if self
            .placement
            .get(id)
            .is_some_and(|(current_parent, current_name)| {
                *current_parent == parent && *current_name == name
            })
        {
            return;
        }
        self.remove(id);
        self.children
            .entry(parent.clone())
            .or_default()
            .entry(name.clone())
            .or_default()
            .insert(id.to_string());
        self.placement.insert(id.to_string(), (parent, name));
    }

    fn named<'a>(&'a self, parent: &str, name: &[u8]) -> impl Iterator<Item = &'a String> {
        let name = FileNameV1::from(name.to_vec());
        self.children
            .get(parent)
            .and_then(|names| names.get(&name))
            .into_iter()
            .flatten()
    }

    fn children<'a>(&'a self, parent: &str) -> impl Iterator<Item = &'a String> {
        self.children
            .get(parent)
            .into_iter()
            .flat_map(BTreeMap::values)
            .flatten()
    }
}

/// Order-independent digest of the record index: the wrapping 256-bit sum of one digest
/// per row, so adding or removing a row updates it in O(1) instead of rehashing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DocumentIndexDigest([u64; 4]);

impl DocumentIndexDigest {
    fn row(record: &IndexedDocument) -> [u64; 4] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.namespace-record-index.row.v3\0");
        hasher.update(record.document_id.0.as_bytes());
        for field in [&record.parent_id, &record.node_id] {
            hasher.update(&(field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.update(&(record.name.as_bytes().len() as u64).to_be_bytes());
        hasher.update(record.name.as_bytes());
        hasher.update(&[u8::from(record.is_dir)]);
        let bytes = hasher.finalize();
        let mut limbs = [0_u64; 4];
        for (limb, chunk) in limbs.iter_mut().zip(bytes.as_bytes().chunks_exact(8)) {
            let mut word = [0_u8; 8];
            word.copy_from_slice(chunk);
            *limb = u64::from_le_bytes(word);
        }
        limbs
    }

    fn add(&mut self, record: &IndexedDocument) {
        let row = Self::row(record);
        let mut carry = false;
        for (limb, value) in self.0.iter_mut().zip(row) {
            let (sum, overflow_a) = limb.overflowing_add(value);
            let (sum, overflow_b) = sum.overflowing_add(u64::from(carry));
            *limb = sum;
            carry = overflow_a || overflow_b;
        }
    }

    fn remove(&mut self, record: &IndexedDocument) {
        let row = Self::row(record);
        let mut borrow = false;
        for (limb, value) in self.0.iter_mut().zip(row) {
            let (difference, underflow_a) = limb.overflowing_sub(value);
            let (difference, underflow_b) = difference.overflowing_sub(u64::from(borrow));
            *limb = difference;
            borrow = underflow_a || underflow_b;
        }
    }
}

/// Whose device a mount is: the id its nodes are named by, and the person whose certified device
/// it is. A mount's changes name that person ([`collab_types::actors::device_actor`], the actor
/// every mount of a record writes as) and so do the nodes it makes, so a page reads who copied a
/// file in instead of a member it cannot name.
#[derive(Clone, Debug)]
pub struct MountAuthor {
    author: String,
    person: Option<tilo_actions::MemberId>,
    /// The workspace whose tree this mount holds, when it is a workspace's: every record of the
    /// tree names itself as Files' record of it before the mount publishes it.
    workspace: Option<String>,
}

impl MountAuthor {
    /// A mount nobody's certified device is: a test's, or a program that writes no one person's
    /// files.
    pub fn device(author: impl Into<String>) -> Self {
        Self {
            author: author.into(),
            person: None,
            workspace: None,
        }
    }

    /// The mount of `person`'s certified device, which `author` (its device key) names.
    #[must_use]
    pub fn person(author: impl Into<String>, person: tilo_actions::MemberId) -> Self {
        Self {
            author: author.into(),
            person: Some(person),
            workspace: None,
        }
    }

    /// The mount of `workspace`'s tree: its records name themselves as Files' records of it, so
    /// every page opens the tree this device wrote.
    #[must_use]
    pub fn in_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// The document that founds `chunk` of this mount's tree, when it holds a workspace's: the
    /// identity every founder derives, so this mount's founding and a page's Files that started
    /// the same tree merge as one document.
    pub(super) fn founding(&self, chunk: TreeChunk) -> Option<AutoCommit> {
        founding_document(self.workspace.as_deref()?, chunk)
    }
}

impl From<String> for MountAuthor {
    fn from(author: String) -> Self {
        Self::device(author)
    }
}

impl From<&str> for MountAuthor {
    fn from(author: &str) -> Self {
        Self::device(author)
    }
}

impl From<&String> for MountAuthor {
    fn from(author: &String) -> Self {
        Self::device(author.clone())
    }
}

#[derive(Clone)]
pub struct Namespace {
    /// The document; its every change is announced to [`Self::subscribe_changes`]
    /// ([`changes`]).
    doc: Chunks,
    ino_to_id: HashMap<u64, String>,
    id_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
    /// The person whose certified device this mount is, when it is one: its changes and the
    /// nodes it makes name them.
    person: Option<tilo_actions::MemberId>,
    /// The workspace whose tree this mount holds, when it is a workspace's: each record names
    /// itself as Files' record of it when the mount publishes it.
    workspace: Option<String>,
    next_id: u64,
    /// Stable-record catalogue for the mount's paged record projection, updated with
    /// structural mutations rather than rebuilt by every page request.
    document_index: BTreeMap<FileDocumentId, IndexedDocument>,
    document_index_revision: String,
    /// Stable record identities that are currently multiply bound; they stay absent from
    /// the public index until a full reconciliation proves the ambiguity gone.
    document_index_ambiguous: HashSet<FileDocumentId>,
    document_index_digest: DocumentIndexDigest,
    tree: TreeIndex,
    /// Nodes a delta placed since the last resolution: the only nodes a cycle can newly
    /// run through. The next resolution clears it, and it never holds more than the tree.
    placed_since_resolve: HashSet<String>,
    /// A document read as saved may carry a cycle no resolution has run on yet, so the
    /// next resolution scans it whole, once.
    cycle_scan_pending: bool,
    /// While set (to the heads the batch began at, and the changes not yet taken then),
    /// [`Self::save_incremental`] returns nothing, so a batch of changes becomes one
    /// delta. Encoding a delta regenerates its changes from the op set, so one per changed
    /// file is quadratic in a changeset.
    #[cfg(test)]
    document_index_rebuilds: u64,
    /// When this namespace was opened, in milliseconds: the time an empty folder reads as changed.
    opened_ms: u64,
    #[cfg(test)]
    document_index_page_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    node_entry_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    node_lookups: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Cheap fingerprint of a [`Namespace`]: its document heads. Equal tokens mean no node,
/// path or hash changed, so a reconciler can skip a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceChangeToken {
    heads: Vec<automerge::ChangeHash>,
}

thread_local! {
    /// What the file actions this thread writes are part of ([`acting_as`]).
    static ACTING: std::cell::RefCell<Option<tilo_actions::ActionContext>> =
        const { std::cell::RefCell::new(None) };
}

/// Write this thread's file actions as part of `ambient` until the guard is dropped: who does
/// them (an agent writing through the mount, an agent's call carried out where the tree is
/// kept), and the invocation and run they belong to. `None` is the person at this device.
/// Each change of the tree then names who made it and in which call, which History shows,
/// grants decide and Undo takes back as theirs. A thread, not the namespace, carries it:
/// requests of several writers share one namespace at once.
#[must_use]
pub fn acting_as(ambient: Option<tilo_actions::ActionContext>) -> ActingAs {
    ActingAs(ACTING.with(|acting| acting.replace(ambient)))
}

/// Puts back what the thread acted as before [`acting_as`].
pub struct ActingAs(Option<tilo_actions::ActionContext>);

impl Drop for ActingAs {
    fn drop(&mut self) {
        let before = self.0.take();
        ACTING.with(|acting| *acting.borrow_mut() = before);
    }
}

fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h.as_bytes())
}
pub(crate) fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

fn hash_of(hex: &str) -> Option<Hash> {
    hash_from_hex(hex)
}

/// A name no file or folder may have: empty, `.`, `..`, or with a `/`.
pub(crate) fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// `name` in `folder`, as a path from the tree's root; either may be empty.
pub(crate) fn join(folder: &str, name: &str) -> String {
    match (folder.trim_matches('/'), name) {
        ("", name) => name.to_owned(),
        (folder, "") => folder.to_owned(),
        (folder, name) => format!("{folder}/{name}"),
    }
}

/// A bounded, valid sibling name for a record that lost a name to a smaller stable record
/// id. The record id makes the choice independent of arrival order and collision-free
/// without a local counter.
fn structural_conflict_name(name: &[u8], document_id: FileDocumentId) -> FileNameV1 {
    let suffix = format!(
        "{}{}",
        String::from_utf8_lossy(STRUCTURAL_CONFLICT_MARKER),
        document_id.0.simple()
    );
    let suffix = suffix.as_bytes();
    let prefix_len = name
        .len()
        .min(MAX_FILE_NAME_BYTES.saturating_sub(suffix.len()));
    let mut out = name.get(..prefix_len).unwrap_or(name).to_vec();
    out.extend_from_slice(suffix);
    out.into()
}

fn str_at(doc: &Chunks, obj: &Obj, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(value) => Some(value.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// [`str_at`] of a document's root.
fn str_at_doc(doc: &AutoCommit, key: &str) -> Option<String> {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(value) => Some(value.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The kind a node object names, read once so a listing asks for one node instead of two.
fn node_kind(doc: &Chunks, obj: &Obj) -> NodeKind {
    str_at(doc, obj, "kind").map_or(NodeKind::File, |k| NodeKind::from_str(&k))
}

fn read_name(doc: &Chunks, obj: &Obj, key: &str) -> Option<FileNameV1> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(bytes) => Some(FileNameV1(bytes.clone())),
            // Existing namespaces stored UTF-8 names as Automerge strings.
            ScalarValue::Str(value) => Some(value.as_bytes().to_vec().into()),
            _ => None,
        },
        _ => None,
    }
}
fn read_u64(doc: &Chunks, obj: &Obj, key: &str) -> u64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Uint(n) => *n,
            ScalarValue::Int(n) => u64::try_from(*n).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// `founding`, with an actor of the mount's own for what follows it: the identity change is
/// nobody's edit, and the root id and the nodes the mount writes after it are its own. An empty
/// document where the record is not one a founder derives.
fn started_from(founding: Option<AutoCommit>) -> AutoCommit {
    match founding {
        Some(mut doc) => {
            doc.set_actor(automerge::ActorId::random());
            doc
        }
        None => AutoCommit::new(),
    }
}

impl Namespace {
    /// A namespace over `doc`, its documents already in place, with only the root bound and
    /// every derived index empty.
    fn over(mut doc: Chunks, mount: MountAuthor, next_id: u64) -> Self {
        // The person's device writes every document of the mount: the handle keeps two of their
        // computers' actors apart, and every reader names the person from it.
        if let Some(person) = &mount.person {
            let writer = automerge::ActorId::from(
                collab_types::actors::device_actor(&person.actor_bytes(), doc.index().get_actor())
                    .as_slice(),
            );
            for (_, held) in doc.each_mut() {
                held.set_actor(writer.clone());
            }
        }
        let mut ns = Self {
            doc,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author: mount.author,
            person: mount.person,
            workspace: mount.workspace,
            next_id,
            document_index: BTreeMap::new(),
            document_index_revision: String::new(),
            document_index_ambiguous: HashSet::new(),
            document_index_digest: DocumentIndexDigest::default(),
            tree: TreeIndex::default(),
            placed_since_resolve: HashSet::new(),
            cycle_scan_pending: true,
            #[cfg(test)]
            document_index_rebuilds: 0,
            opened_ms: tilo_clock::unix_now_ns() / 1_000_000,
            #[cfg(test)]
            document_index_page_reads: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            node_entry_reads: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            node_lookups: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        ns
    }

    pub fn new(author: impl Into<MountAuthor>) -> Result<Self, automerge::AutomergeError> {
        let mount: MountAuthor = author.into();
        let index = started_from(mount.founding(0));
        let mut ns = Self::over(Chunks::new(index, mount.workspace.clone()), mount, 0);
        let root_document_id = FileDocumentId::new();
        ns.doc.put(
            &Obj::index(),
            ROOT_DOCUMENT_ID_KEY,
            root_document_id.0.to_string(),
        )?;
        // Name the founding change now, as the making of the tree's root folder: a merge made
        // later settles a pending change without its name, and a device signs only the changes
        // that name what made them.
        let ambient = ACTING.with(|acting| acting.borrow().clone());
        let index = ns.doc.index_mut();
        index.name_as(
            &fs_protocol::tree::document_id(),
            fs_protocol::file_action::did(fs_protocol::file_action::verbs::NEW_FOLDER),
            ROOT_ID,
            ambient.as_ref(),
        );
        index.commit_named();
        ns.rebuild_document_index();
        Ok(ns)
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    /// Who the nodes this mount makes are named by: the person whose certified device it is,
    /// else the device id a bare mount names itself by.
    fn node_author(&self) -> String {
        self.person
            .as_ref()
            .map_or_else(|| self.author.clone(), |person| person.id().to_owned())
    }

    /// Write `action` into the tree as whoever this thread acts for ([`acting_as`]).
    fn write_action(&mut self, action: &fs_protocol::FileAction) -> bool {
        let ambient = ACTING.with(|acting| acting.borrow().clone());
        let written = self.doc.write_action(action, ambient.as_ref());
        if let Err(error) = &written {
            tracing::warn!(?error, ?action, "a file action did not apply to the tree");
        }
        written.is_ok()
    }

    fn node(&self, id: &str) -> Option<Obj> {
        self.node_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.doc.node(id)
    }

    #[must_use]
    pub fn node_lookup_count(&self) -> u64 {
        self.node_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn parent_of(&self, id: &str) -> Option<String> {
        str_at(&self.doc, &self.node(id)?, "parent")
    }
    fn name_of(&self, id: &str) -> Option<FileNameV1> {
        read_name(&self.doc, &self.node(id)?, "name")
    }
    pub fn live_kind(&self, ino: u64) -> Option<FileType> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(NodeKind::Dir.to_file_type());
        }
        if !self.reaches_root(id) {
            return None;
        }
        Some(self.kind_of(id).to_file_type())
    }

    fn kind_of(&self, id: &str) -> NodeKind {
        self.node(id)
            .map_or(NodeKind::File, |o| node_kind(&self.doc, &o))
    }

    fn set_field(&mut self, id: &str, key: &str, val: impl Into<ScalarValue>) {
        if let Some(obj) = self.node(id) {
            let _ = self.doc.put(&obj, key, val);
            if key == "parent" || key == "name" {
                self.reindex_node(id);
            }
        }
    }

    fn reindex_node(&mut self, id: &str) {
        let placement = self.node(id).and_then(|node| {
            let parent = str_at(&self.doc, &node, "parent")?;
            let name = read_name(&self.doc, &node, "name").unwrap_or_default();
            Some((parent, name))
        });
        match placement {
            Some((parent, name)) => self.tree.place(id, parent, name),
            None => self.tree.remove(id),
        }
    }

    /// Whether `id`'s parent chain reaches the root. Walks the structural index, which mirrors
    /// every node's document parent, instead of reading the document once per hop.
    fn reaches_root(&self, id: &str) -> bool {
        let mut cur = id;
        // A chain longer than the tree has nodes is a cycle.
        let mut hops = self.tree.placement.len().saturating_add(1);
        loop {
            if cur == ROOT_ID {
                return true;
            }
            if cur == TRASH_ID || hops == 0 {
                return false;
            }
            hops = hops.saturating_sub(1);
            match self.tree.placement.get(cur) {
                Some((parent, _)) => cur = parent,
                None => return false,
            }
        }
    }

    fn entry(&self, id: &str) -> Option<Entry> {
        let obj = self.node(id)?;
        let meta = self.node_meta_at(id, &obj)?;
        Some(Entry {
            id: id.to_string(),
            document_id: meta.document_id,
            parent: str_at(&self.doc, &obj, "parent").unwrap_or_default(),
            name: read_name(&self.doc, &obj, "name").unwrap_or_default(),
            kind: meta.kind,
            hash: meta.hash,
            size: meta.size,
            mtime: meta.mtime,
            author: str_at(&self.doc, &obj, "author").unwrap_or_default(),
        })
    }

    /// The fields of [`Self::entry`] a FUSE request needs, read from the node once: no parent,
    /// name or author strings, and the record id resolved without looking the node up again.
    fn node_meta(&self, id: &str) -> Option<NodeMeta> {
        self.node_meta_at(id, &self.node(id)?)
    }

    /// [`Self::node_meta`] of the node object `obj` for `id`. A node with no record id has no
    /// entry, exactly as [`Self::entry`] requires.
    fn node_meta_at(&self, id: &str, obj: &Obj) -> Option<NodeMeta> {
        let document_id = if id == ROOT_ID {
            self.document_id_for_id(id)?
        } else {
            FileDocumentId(Uuid::parse_str(&str_at(&self.doc, obj, DOCUMENT_ID_KEY)?).ok()?)
        };
        Some(NodeMeta {
            document_id,
            kind: NodeKind::from_str(&str_at(&self.doc, obj, "kind").unwrap_or_default()),
            // A branch a file version was carried into names its re-addressed version in the
            // index's aliases; a reader opens what that names.
            hash: str_at(&self.doc, obj, "hash").and_then(|h| {
                hash_from_hex(
                    &fs_protocol::tree::file_version_alias(&**self.doc.index(), &h).unwrap_or(h),
                )
            }),
            size: read_u64(&self.doc, obj, "size"),
            mtime: read_u64(&self.doc, obj, "mtime"),
        })
    }

    fn children(&self, parent_id: &str) -> Vec<String> {
        self.tree.children(parent_id).cloned().collect()
    }

    fn child_named(&self, parent_id: &str, name: impl AsRef<[u8]>) -> Option<String> {
        self.tree
            .named(parent_id, name.as_ref())
            .find(|id| self.reaches_root(id))
            .cloned()
    }

    fn is_ancestor(&self, maybe_ancestor: &str, id: &str) -> bool {
        let mut cur = id.to_string();
        let mut seen = HashSet::new();
        loop {
            if cur == maybe_ancestor {
                return true;
            }
            if cur == ROOT_ID || cur == TRASH_ID || !seen.insert(cur.clone()) {
                return false;
            }
            match self.parent_of(&cur) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    fn ino_for_id(&mut self, id: &str) -> u64 {
        if let Some(&ino) = self.id_to_ino.get(id) {
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino = self.next_ino.saturating_add(1);
        self.id_to_ino.insert(id.to_string(), ino);
        self.ino_to_id.insert(ino, id.to_string());
        ino
    }

    pub fn id_for_ino(&self, ino: u64) -> Option<&str> {
        self.ino_to_id.get(&ino).map(std::string::String::as_str)
    }

    pub fn file_name_for(&self, ino: u64) -> Option<FileNameV1> {
        self.id_for_ino(ino).and_then(|id| self.name_of(id))
    }

    pub fn document_id_for(&self, ino: u64) -> Option<FileDocumentId> {
        self.document_id_for_id(self.id_for_ino(ino)?)
    }

    /// Stable collaborative record id for a namespace node id.
    pub fn document_id_for_id(&self, id: &str) -> Option<FileDocumentId> {
        let value = if id == ROOT_ID {
            str_at(&self.doc, &Obj::index(), ROOT_DOCUMENT_ID_KEY)?
        } else {
            str_at(&self.doc, &self.node(id)?, DOCUMENT_ID_KEY)?
        };
        Uuid::parse_str(&value).ok().map(FileDocumentId)
    }

    /// Describe the currently visible namespace node as a signed-binding payload. The caller
    /// supplies the certified actor and observation time the record envelope signature covers.
    #[must_use]
    pub fn signed_binding_for(
        &self,
        ino: u64,
        actor: FileActor,
        observed_at_ms: u64,
    ) -> Option<FileBindingV1> {
        let document_id = self.document_id_for(ino)?;
        if ino == ROOT_INO {
            return Some(FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                document_id,
                parent: None,
                name: FileNameV1::default(),
                kind: FileNodeKind::Directory,
                tombstoned: false,
                actor,
                observed_at_ms,
            });
        }
        let id = self.id_for_ino(ino)?;
        if !self.reaches_root(id) {
            return None;
        }
        let parent_id = self.parent_of(id)?;
        let parent = self.document_id_for_id(&parent_id)?;
        let kind = match self.kind_of(id) {
            NodeKind::File => FileNodeKind::File,
            NodeKind::Dir => FileNodeKind::Directory,
        };
        Some(FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            document_id,
            parent: Some(parent),
            name: self.name_of(id)?,
            kind,
            tombstoned: false,
            actor,
            observed_at_ms,
        })
    }

    /// Project one already-verified structural binding.
    ///
    /// The latest converged binding is the authority for this record's existence and
    /// placement:
    ///
    /// - an unknown record with a non-tombstone binding is created at its signed
    ///   parent and name (the parent must already be a unique live directory, so a
    ///   child whose parent has not arrived yet is deferred);
    /// - a live record moves, or becomes visible again after a concurrent delete when
    ///   its newer binding wins the replica's deterministic binding order;
    /// - a tombstone trashes a live record and is a no-op for one never seen;
    /// - records that want one name are assigned it in stable-record-id order, the
    ///   others keep a deterministic `.sync-conflict-<record>` sibling.
    ///
    /// Unsigned nodes are never renamed or trusted: they keep their name, and a signed
    /// record that collides with one is the one that moves. Repeated application is
    /// idempotent.
    pub fn apply_signed_binding(
        &mut self,
        binding: &FileBindingV1,
    ) -> Result<bool, BindingProjectionError> {
        binding
            .validate()
            .map_err(|_| BindingProjectionError::InvalidMove)?;
        if binding.parent.is_none() {
            return self.apply_signed_root_binding(binding);
        }
        let parent_document = binding
            .parent
            .ok_or(BindingProjectionError::UnknownParent)?;
        let expected_kind = match binding.kind {
            FileNodeKind::File => NodeKind::File,
            FileNodeKind::Directory => NodeKind::Dir,
        };
        let parent_id = self.resolve_signed_parent(parent_document)?;
        let existing = self.signed_document_node(binding.document_id)?;
        if let Some(id) = existing {
            if self.kind_of(&id) != expected_kind {
                return Err(BindingProjectionError::KindMismatch);
            }
            if binding.tombstoned {
                if !self.reaches_root(&id) {
                    return Ok(false);
                }
                let ino = self.ino_for_id(&id);
                return self
                    .remove(ino)
                    .then_some(true)
                    .ok_or(BindingProjectionError::InvalidMove);
            }
            let currently_live = self.reaches_root(&id);
            if currently_live {
                let current_parent = self
                    .parent_of(&id)
                    .ok_or(BindingProjectionError::InvalidMove)?;
                // The settled current name may be a conflict sibling; the signed placement
                // intent, not the visible name, decides whether this binding is already applied.
                if current_parent == parent_id
                    && self.placement_name_of(&id).as_ref() == Some(&binding.name)
                {
                    return Ok(false);
                }
                if parent_id == id || self.is_ancestor(&id, &parent_id) {
                    return Err(BindingProjectionError::InvalidMove);
                }
                self.set_signed_placement(&id, &parent_id, &binding.name, binding.observed_at_ms);
                self.settle_structural_conflicts(&parent_id)?;
                if current_parent != parent_id {
                    self.settle_structural_conflicts(&current_parent)?;
                }
                return Ok(true);
            }
            // A newer binding than the tombstone resurrects the record. The replica selects
            // bindings by their signed stamp, so this is the same deterministic delete/move
            // conflict decision on every peer.
            self.set_signed_placement(&id, &parent_id, &binding.name, binding.observed_at_ms);
            self.settle_structural_conflicts(&parent_id)?;
            return Ok(true);
        }
        if binding.tombstoned {
            return Ok(false);
        }
        let id = self
            .create_node_as(
                &parent_id,
                &binding.name,
                expected_kind,
                None,
                0,
                binding.observed_at_ms,
                binding.document_id,
            )
            .ok_or(BindingProjectionError::InvalidMove)?;
        self.settle_structural_conflicts(&parent_id)?;
        let _ = id;
        Ok(true)
    }

    /// Admit a binding only to create a missing node, never to move, resurrect or tombstone an
    /// existing one: transaction files make their pre-transaction nodes available to the
    /// projector without letting a stale ordinary binding undo the transaction on replay.
    pub fn apply_signed_binding_create_only(
        &mut self,
        binding: &FileBindingV1,
    ) -> Result<bool, BindingProjectionError> {
        binding
            .validate()
            .map_err(|_| BindingProjectionError::InvalidMove)?;
        if binding.parent.is_none() {
            return self.apply_signed_root_binding(binding);
        }
        if self.signed_document_node(binding.document_id)?.is_some() || binding.tombstoned {
            return Ok(false);
        }
        let expected_kind = match binding.kind {
            FileNodeKind::File => NodeKind::File,
            FileNodeKind::Directory => NodeKind::Dir,
        };
        let parent = binding
            .parent
            .ok_or(BindingProjectionError::UnknownParent)?;
        let parent_id = self.resolve_signed_parent(parent)?;
        self.create_node_as(
            &parent_id,
            &binding.name,
            expected_kind,
            None,
            0,
            binding.observed_at_ms,
            binding.document_id,
        )
        .ok_or(BindingProjectionError::InvalidMove)?;
        self.settle_structural_conflicts(&parent_id)?;
        Ok(true)
    }

    fn apply_signed_root_binding(
        &mut self,
        binding: &FileBindingV1,
    ) -> Result<bool, BindingProjectionError> {
        if binding.tombstoned || !binding.name.is_empty() || binding.kind != FileNodeKind::Directory
        {
            return Err(BindingProjectionError::InvalidMove);
        }
        match self.document_id_for(ROOT_INO) {
            Some(current) if current == binding.document_id => return Ok(false),
            Some(_) if !self.live_node_ids().is_empty() => {
                return Err(BindingProjectionError::RootIdentityConflict);
            }
            _ => {}
        }
        self.doc
            .put(
                &Obj::index(),
                ROOT_DOCUMENT_ID_KEY,
                binding.document_id.0.to_string(),
            )
            .map_err(|_| BindingProjectionError::InvalidMove)?;
        Ok(true)
    }

    fn resolve_signed_parent(
        &self,
        parent_document: FileDocumentId,
    ) -> Result<String, BindingProjectionError> {
        if self.document_id_for(ROOT_INO) == Some(parent_document) {
            return Ok(ROOT_ID.to_string());
        }
        let id = self
            .unique_live_node_for_document(parent_document)
            .ok_or(BindingProjectionError::UnknownParent)?;
        if self.kind_of(&id) != NodeKind::Dir {
            return Err(BindingProjectionError::UnknownParent);
        }
        Ok(id)
    }

    /// The one node carrying a stable record id, whether currently live or trashed. Duplicate
    /// identities stay fail-closed.
    fn signed_document_node(
        &self,
        document_id: FileDocumentId,
    ) -> Result<Option<String>, BindingProjectionError> {
        let mut matches = self
            .tree
            .nodes_of(document_id)
            .filter(|id| self.document_id_for_id(id) == Some(document_id));
        let Some(id) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(BindingProjectionError::UnknownOrDuplicateDocument);
        }
        Ok(Some(id.clone()))
    }

    fn placement_name_of(&self, id: &str) -> Option<FileNameV1> {
        self.node(id)
            .and_then(|node| read_name(&self.doc, &node, PLACEMENT_NAME_KEY))
            .or_else(|| self.name_of(id))
    }

    /// Record the signed placement intent and provisionally move the node there.
    /// [`Self::settle_structural_conflicts`] then assigns the names of every sibling that wants
    /// the same one.
    fn set_signed_placement(&mut self, id: &str, parent_id: &str, name: &FileNameV1, mtime: u64) {
        let previous_parent = self.parent_of(id);
        if previous_parent.as_deref() != Some(parent_id) {
            let prev = previous_parent.unwrap_or_else(|| ROOT_ID.to_string());
            self.set_field(id, "prev_parent", prev);
        }
        self.set_field(id, "parent", parent_id.to_string());
        self.set_field(id, PLACEMENT_NAME_KEY, name.as_bytes().to_vec());
        self.set_field(id, "name", name.as_bytes().to_vec());
        self.set_field(id, "mtime", mtime);
        self.index_refresh_subtree(id);
    }

    /// Assign the names of all live children of `parent_id` that ask for one name. The smallest
    /// stable record id keeps the requested name; the others get a deterministic conflict
    /// sibling. A fixed point over the signed placement names, so every peer that holds the same
    /// records computes the same tree whatever order they arrived in.
    fn settle_structural_conflicts(
        &mut self,
        parent_id: &str,
    ) -> Result<bool, BindingProjectionError> {
        let children = self
            .children(parent_id)
            .into_iter()
            .filter(|id| self.reaches_root(id))
            .collect::<Vec<_>>();
        if children.is_empty() {
            return Ok(false);
        }
        let mut current = BTreeMap::<String, FileNameV1>::new();
        for id in &children {
            current.insert(
                id.clone(),
                self.placement_name_of(id)
                    .ok_or(BindingProjectionError::InvalidMove)?,
            );
        }
        for _ in 0..MAX_STRUCTURAL_CONFLICT_ROUNDS {
            let mut groups = BTreeMap::<FileNameV1, Vec<String>>::new();
            for (id, name) in &current {
                groups.entry(name.clone()).or_default().push(id.clone());
            }
            let mut moved = false;
            let mut next = current.clone();
            for (name, mut ids) in groups {
                if ids.len() < 2 {
                    continue;
                }
                ids.sort_by_key(|id| self.structural_owner_key(id));
                for loser in ids.into_iter().skip(1) {
                    let Some(document_id) = self.document_id_for_id(&loser) else {
                        // An unsigned node is not this authority's to rename: leave it where it
                        // is, and never move a signed record onto or over it silently.
                        continue;
                    };
                    next.insert(
                        loser,
                        structural_conflict_name(name.as_bytes(), document_id),
                    );
                    moved = true;
                }
            }
            if !moved {
                let mut renamed = Vec::new();
                for (id, name) in current {
                    if self.name_of(&id).as_ref() != Some(&name) {
                        self.set_field(&id, "name", name.as_bytes().to_vec());
                        renamed.push(id);
                    }
                }
                for id in &renamed {
                    self.index_refresh_subtree(id);
                }
                return Ok(!renamed.is_empty());
            }
            current = next;
        }
        Err(BindingProjectionError::StructuralConflictLoop)
    }

    /// Total order for a contested name: nodes without a stable record id keep precedence
    /// (they are never renamed), then the stable record id, then the node id for an
    /// impossible tie.
    fn structural_owner_key(&self, id: &str) -> (u8, FileDocumentId, String) {
        match self.document_id_for_id(id) {
            Some(document_id) => (1, document_id, id.to_string()),
            None => (0, FileDocumentId(Uuid::nil()), id.to_string()),
        }
    }

    /// Resolve a live stable record without going through a mutable path: shared-record
    /// replication applies content metadata after a rename while keeping the existing name and
    /// parent.
    pub fn ino_for_document_id(&self, document_id: FileDocumentId) -> Option<u64> {
        if self.document_id_for(ROOT_INO) == Some(document_id) {
            return Some(ROOT_INO);
        }
        self.tree
            .nodes_of(document_id)
            .find(|id| self.document_id_for_id(id) == Some(document_id) && self.reaches_root(id))
            .and_then(|id| self.id_to_ino.get(id).copied())
    }

    /// Resolve one live stable record while lazily assigning its process-local inode. Namespace
    /// snapshots do not persist inode maps, so authenticated startup reconciliation uses this
    /// before any FUSE lookup has warmed the cache. Ambiguous identities fail closed.
    pub(crate) fn ensure_ino_for_document_id(
        &mut self,
        document_id: FileDocumentId,
    ) -> Option<u64> {
        if self.document_id_for(ROOT_INO) == Some(document_id) {
            return Some(ROOT_INO);
        }
        let only = self.unique_live_node_for_document(document_id)?;
        Some(self.ino_for_id(&only))
    }

    /// The one live node bound to `document_id`, `None` when there is none or more than one. The
    /// structural index knows every node carrying the id, so this costs O(nodes of the record ×
    /// depth), not a scan of the tree; each candidate is checked against the document.
    fn unique_live_node_for_document(&self, document_id: FileDocumentId) -> Option<String> {
        let mut live = self
            .tree
            .nodes_of(document_id)
            .filter(|id| self.document_id_for_id(id) == Some(document_id) && self.reaches_root(id));
        let only = live.next()?.clone();
        if live.next().is_some() {
            return None;
        }
        Some(only)
    }

    /// Absolute path of `ino` within the mount, `"/"` for the root, `None` if unknown or its
    /// parent chain does not reach the root (trashed or cyclic).
    ///
    /// This is what the per-identity ACL check keys on: "may this identity read or write this
    /// path".
    pub fn path_for(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some("/".to_string());
        }
        self.path_for_id(self.id_for_ino(ino)?)
    }

    /// Absolute path of a stable namespace node id. Node ids exist before a FUSE lookup allocates
    /// a local inode, so the network content check can map a requested blob back to every live
    /// path that names it without mutating the namespace index.
    pub fn path_for_id(&self, id: &str) -> Option<String> {
        if id == ROOT_ID {
            return Some("/".to_string());
        }
        let mut cur = id;
        let mut parts: Vec<String> = Vec::new();
        let mut hops = self.tree.placement.len().saturating_add(1);
        loop {
            if cur == ROOT_ID {
                break;
            }
            if cur == TRASH_ID || hops == 0 {
                return None;
            }
            hops = hops.saturating_sub(1);
            let (parent, name) = self.tree.placement.get(cur)?;
            parts.push(escape_path_name(name.as_bytes()));
            cur = parent;
        }
        parts.reverse();
        Some(format!("/{}", parts.join("/")))
    }

    /// Minimal live record graph the content store uses to resolve manifest chunks. Crate-private,
    /// so a caller checking one requested blob cannot disclose another record's roots.
    pub fn live_document_blob_roots(
        &self,
        max_live_documents: usize,
    ) -> Result<Vec<LiveDocumentBlobRoots>, LiveDocumentBlobScanError> {
        if max_live_documents == 0 || max_live_documents > MAX_LIVE_RECORD_BLOB_SCAN {
            return Err(LiveDocumentBlobScanError::InvalidLimit);
        }
        let mut live_count = 0usize;
        let mut roots = Vec::new();
        for id in self.doc.sorted_node_ids() {
            if !self.reaches_root(&id) {
                continue;
            }
            live_count = live_count.saturating_add(1);
            if live_count > max_live_documents {
                return Err(LiveDocumentBlobScanError::LimitExceeded {
                    limit: max_live_documents,
                });
            }
            let document_id = self
                .document_id_for_id(&id)
                .ok_or(LiveDocumentBlobScanError::MissingDocumentIdentity)?;
            let manifest = self.node_hash(&id);
            let thumbnail = self.thumbnail_for_id(&id);
            if manifest.is_some() || thumbnail.is_some() {
                roots.push(LiveDocumentBlobRoots {
                    document_id,
                    manifest,
                    thumbnail,
                });
            }
        }
        Ok(roots)
    }

    /// Parent inode of a visible node; the root has none. Callers recover identity here rather
    /// than from a path string, since renames keep the node identity and inode number.
    pub fn parent_ino(&mut self, ino: u64) -> Option<u64> {
        if ino == ROOT_INO {
            return None;
        }
        let id = self.id_for_ino(ino)?.to_string();
        if !self.reaches_root(&id) {
            return None;
        }
        let parent = self.parent_of(&id)?;
        Some(self.ino_for_id(&parent))
    }

    fn gen_id(&mut self) -> String {
        let id = format!("{}/{}", self.author, self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    fn attr_for(ino: u64, kind: NodeKind, size: u64, mtime_ms: u64) -> FileAttr {
        // Saturates rather than panics: `mtime_ms` is stored CRDT state, so an out-of-range value
        // must degrade, not take the mount down.
        let t = UNIX_EPOCH
            .checked_add(Duration::from_millis(mtime_ms))
            .unwrap_or(UNIX_EPOCH);
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: kind.to_file_type(),
            perm: match kind {
                NodeKind::Dir => 0o755,
                NodeKind::File => 0o644,
            },
            nlink: match kind {
                NodeKind::Dir => 2,
                NodeKind::File => 1,
            },
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    /// The folder's own attributes: changed when the newest of its entries changed, as a
    /// directory's time moves with what is put into it, rather than never (the epoch, which a
    /// file manager shows as 1970). An empty folder reads as changed when this namespace opened.
    fn root_attr(&self) -> FileAttr {
        let newest = self
            .tree
            .children
            .get(ROOT_ID)
            .into_iter()
            .flat_map(BTreeMap::values)
            .flatten()
            .filter_map(|id| self.node_meta(id))
            .map(|meta| meta.mtime)
            .max();
        Self::attr_for(ROOT_INO, NodeKind::Dir, 0, newest.unwrap_or(self.opened_ms))
    }

    pub fn getattr(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(self.root_attr());
        }
        if !self.reaches_root(id) {
            return None;
        }
        let meta = self.node_meta(id)?;
        Some(self.node_attr(ino, id, &meta))
    }

    /// The attributes the mount presents for node `id` at `ino`: its kind, size and time, and the
    /// mode the record was given (`chmod`, a checkout's executable scripts), which replicates with
    /// it; a record nobody gave one keeps the default.
    fn node_attr(&self, ino: u64, id: &str, meta: &NodeMeta) -> FileAttr {
        let mut attr = Self::attr_for(ino, meta.kind, meta.size, meta.mtime);
        if let Some(posix) = self
            .document_id_for_id(id)
            .and_then(|record| self.posix_attrs(record))
            .filter(|posix| !posix.symlink)
        {
            attr.perm = u16::try_from(posix.mode & 0o7777).unwrap_or(attr.perm);
        }
        attr
    }

    /// Attribute lookup for a kernel file handle whose directory entry has since been unlinked or
    /// replaced. Path lookup keeps using [`Self::getattr`]; only an open handle reaches this view.
    pub(crate) fn getattr_including_unlinked(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(self.root_attr());
        }
        let meta = self.node_meta(id)?;
        Some(self.node_attr(ino, id, &meta))
    }

    pub(crate) fn attr_and_hash(&self, ino: u64) -> Option<(FileAttr, Option<Hash>)> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some((self.root_attr(), None));
        }
        if !self.reaches_root(id) {
            return None;
        }
        let meta = self.node_meta(id)?;
        Some((self.node_attr(ino, id, &meta), meta.hash))
    }

    /// Everything one FUSE request asks about `ino`, read in one pass so the caller takes the
    /// namespace lock once: what [`Self::getattr`] (or, with `unlinked`,
    /// [`Self::getattr_including_unlinked`]), [`Self::hash_for`] and [`Self::path_for`] would each
    /// answer. The path is built only when `query.path` asks
    /// for it, so a mount without an ACL never looks at it.
    pub(crate) fn inode_view(&self, ino: u64, query: InodeQuery) -> Option<InodeView> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(InodeView {
                attr: self.root_attr(),
                hash: None,
                live: true,
                path: query.path.then(|| "/".to_string()),
            });
        }
        let live = self.reaches_root(id);
        if !live && !query.unlinked {
            return None;
        }
        let obj = self.node(id)?;
        let meta = self.node_meta_at(id, &obj)?;
        Some(InodeView {
            attr: self.node_attr(ino, id, &meta),
            hash: meta.hash,
            live,
            path: if live && query.path {
                self.path_for_id(id)
            } else {
                None
            },
        })
    }

    pub fn lookup_kind(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
    ) -> Option<(u64, FileType)> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        self.document_id_for_id(&child)?;
        let kind = self.kind_of(&child).to_file_type();
        Some((self.ino_for_id(&child), kind))
    }

    pub fn lookup(&mut self, parent_ino: u64, name: impl AsRef<[u8]>) -> Option<FileAttr> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        let meta = self.node_meta(&child)?;
        let ino = self.ino_for_id(&child);
        Some(self.node_attr(ino, &child, &meta))
    }

    pub fn readdir(&mut self, ino: u64) -> Option<Vec<(u64, FileType, FileNameV1)>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        if id != ROOT_ID {
            match self.entry(&id) {
                Some(e) if e.kind == NodeKind::Dir && self.reaches_root(&id) => {}
                _ => return None,
            }
        }
        let parent_id = if id == ROOT_ID {
            ROOT_ID.to_string()
        } else {
            self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string())
        };
        let parent_ino = self.ino_for_id(&parent_id);

        let mut out = vec![
            (ino, FileType::Directory, ".".into()),
            (parent_ino, FileType::Directory, "..".into()),
        ];

        let mut kids: Vec<(String, FileNameV1, NodeKind)> = self
            .children(&id)
            .into_iter()
            .filter(|c| self.reaches_root(c))
            .filter_map(|c| {
                // A listed entry's name and kind come from one read of its node: a folder of a
                // thousand entries asks the document for each once, not twice.
                let node = self.node(&c)?;
                let name = read_name(&self.doc, &node, "name")?;
                Some((c, name, node_kind(&self.doc, &node)))
            })
            .collect();
        kids.sort_by(|a, b| a.1.cmp(&b.1));
        for (cid, name, kind) in kids {
            let cino = self.ino_for_id(&cid);
            out.push((cino, kind.to_file_type(), name));
        }
        Some(out)
    }

    pub fn hash_for(&self, ino: u64) -> Option<Hash> {
        let id = self.ino_to_id.get(&ino)?;
        self.node_meta(id)?.hash
    }

    /// `None` only if the document's node map itself has become unwritable; every caller already
    /// returns `Option`/`bool` for other failure modes, so this folds into the same contract.
    fn create_node(
        &mut self,
        parent_id: &str,
        name: impl AsRef<[u8]>,
        kind: NodeKind,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<String> {
        self.create_node_as(
            parent_id,
            name,
            kind,
            hash,
            size,
            mtime,
            self.document_id_for_new(),
        )
    }

    /// A record id for a node made now: in the newest chunk while it has room, else in the
    /// next ([`fs_protocol::tree::chunk_for_new`]).
    fn document_id_for_new(&self) -> FileDocumentId {
        let newest = self.doc.newest_chunk();
        let nodes = newest.map_or(0, |chunk| self.doc.nodes_in(chunk));
        FileDocumentId::in_chunk(fs_protocol::tree::chunk_for_new(newest, nodes))
    }

    fn create_node_as(
        &mut self,
        parent_id: &str,
        name: impl AsRef<[u8]>,
        kind: NodeKind,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
        document_id: FileDocumentId,
    ) -> Option<String> {
        let name = name.as_ref();
        // Node identity is derived from the stable record identity, not from the local author
        // counter, so a peer rebuilding this record from the signed log creates the same Automerge
        // object key and the two namespaces merge instead of making two nodes for one record.
        let derived = document_node_id(document_id);
        let id = if self.node(&derived).is_some() {
            if self.document_id_for_id(&derived) == Some(document_id) {
                return None;
            }
            // A pre-existing legacy object happens to use the derived key. Keep the stable record
            // with a local node key; the record id, not the object key, is the durable identity.
            self.gen_id()
        } else {
            derived
        };
        let created = fs_protocol::FileAction::Create {
            node: id.clone(),
            document_id,
            parent: parent_id.to_owned(),
            name: FileNameV1(name.to_vec()),
            kind: match kind {
                NodeKind::Dir => fs_protocol::FileNodeKind::Directory,
                NodeKind::File => fs_protocol::FileNodeKind::File,
            },
            hash: hash.map(hash_to_hex),
            size,
            mtime,
            author: self.node_author(),
        };
        if !self.write_action(&created) {
            return None;
        }
        self.tree
            .place(&id, parent_id.to_string(), name.to_vec().into());
        self.tree.bind_document(&id, document_id);
        self.index_refresh_node(&id);
        Some(id)
    }

    pub fn create_file(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, hash, size, mtime)?;
        Some(self.ino_for_id(&id))
    }

    /// Create a projected file for an already-authoritative stable record, for exact-record share
    /// admission: it refuses nil or duplicate record ids and name collisions instead of minting
    /// or aliasing identity.
    pub fn create_projected_file(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        document_id: FileDocumentId,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<u64> {
        let name = name.as_ref();
        if document_id.0.is_nil()
            || name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || self.ino_for_document_id(document_id).is_some()
            || self.lookup(parent_ino, name).is_some()
        {
            return None;
        }
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node_as(
            &parent_id,
            name,
            NodeKind::File,
            hash,
            size,
            mtime,
            document_id,
        )?;
        Some(self.ino_for_id(&id))
    }

    pub fn create_dir(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::Dir, None, 0, mtime)?;
        Some(self.ino_for_id(&id))
    }

    pub fn create_projected_dir(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        document_id: FileDocumentId,
        mtime: u64,
    ) -> Option<u64> {
        let name = name.as_ref();
        if document_id.0.is_nil()
            || name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || self.ino_for_document_id(document_id).is_some()
            || self.lookup(parent_ino, name).is_some()
        {
            return None;
        }
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id =
            self.create_node_as(&parent_id, name, NodeKind::Dir, None, 0, mtime, document_id)?;
        Some(self.ino_for_id(&id))
    }

    /// The heads of every record of the tree, together: equal heads mean an equal tree.
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        let mut heads: Vec<automerge::ChangeHash> =
            self.document_heads().into_values().flatten().collect();
        heads.sort_unstable();
        heads
    }

    /// A receiver that sees, after this call, every change to the document. It says that something
    /// may have changed, never what; compare [`Self::heads`] to know.
    #[must_use]
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.doc.subscribe()
    }

    /// O(heads): a token that changes whenever anything this namespace projects changes, so a
    /// reconciler compares it instead of walking every node to discover that nothing happened.
    pub fn change_token(&mut self) -> NamespaceChangeToken {
        NamespaceChangeToken {
            heads: self.heads(),
        }
    }

    /// Every change this document holds beyond `heads`, as one incremental
    /// chunk for [`Self::apply_remote_and_resolve`]. Heads this document
    /// does not know (the asker's own changes) are ignored, so an asker that
    /// shares no history receives everything.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<u8> {
        let records = self
            .doc
            .each_mut()
            .map(|(chunk, doc)| {
                let known: Vec<automerge::ChangeHash> = heads
                    .iter()
                    .copied()
                    .filter(|hash| doc.has_change(hash))
                    .collect();
                (chunk, doc.save_after(&known))
            })
            .collect();
        bundle(records)
    }

    pub fn set_content(&mut self, ino: u64, hash: Option<&Hash>, size: u64, mtime: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        self.write_action(&fs_protocol::FileAction::Write {
            node: id,
            hash: hash.map(hash_to_hex),
            size,
            mtime,
            author: self.node_author(),
        })
    }

    /// Attach the content hash of a generated thumbnail blob to `ino`, so the pointer rides the
    /// namespace replica and a peer can preview before fetching the full content.
    pub fn set_thumb(&mut self, ino: u64, thumb: &Hash) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        self.set_field(&id, "thumb", hash_to_hex(thumb));
        true
    }

    pub fn thumb_hash(&self, ino: u64) -> Option<Hash> {
        let id = self.ino_to_id.get(&ino)?;
        str_at(&self.doc, &self.node(id)?, "thumb").and_then(|h| hash_from_hex(&h))
    }

    fn thumbnail_for_id(&self, id: &str) -> Option<Hash> {
        self.node(id)
            .and_then(|obj| str_at(&self.doc, &obj, "thumb"))
            .and_then(|value| hash_from_hex(&value))
    }

    pub fn remove(&mut self, ino: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() || !self.reaches_root(&id) {
            return false;
        }
        // The trash keeps the content hash it had, so a concurrent edit can be detected and
        // resurrected.
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        if !self.write_action(&fs_protocol::FileAction::Trash { node: id.clone() }) {
            return false;
        }
        self.reindex_node(&id);
        self.index_refresh_subtree(&id);
        // A delete may release a name another signed record was waiting for.
        let _ = self.settle_structural_conflicts(&cur);
        true
    }

    pub fn directly_trashed(&self) -> Vec<String> {
        self.doc
            .sorted_node_ids()
            .into_iter()
            .filter(|id| self.is_directly_trashed(id))
            .collect()
    }

    pub fn is_directly_trashed(&self, id: &str) -> bool {
        self.parent_of(id).as_deref() == Some(TRASH_ID)
    }

    /// Up to `limit` node ids, live or not, in key order, starting after `after` (at the first when
    /// `None`). Seeking to `after` is a search, not a walk, so a caller can visit every node in
    /// bounded steps and release the namespace lock between them.
    pub fn node_ids_after(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        self.doc.node_ids_after(after, limit)
    }

    pub fn deleted_hash(&self, id: &str) -> Option<Hash> {
        str_at(&self.doc, &self.node(id)?, "deleted_hash").and_then(|h| hash_from_hex(&h))
    }

    pub fn prev_parent(&self, id: &str) -> Option<String> {
        str_at(&self.doc, &self.node(id)?, "prev_parent")
    }

    pub fn is_visible(&self, id: &str) -> bool {
        self.reaches_root(id)
    }

    /// Rename or move: a single reparent, so the node keeps its id and the inode stays stable.
    /// Rejects moving a directory into its own descendant.
    pub fn rename(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        new_parent_ino: u64,
        new_name: impl AsRef<[u8]>,
        mtime: u64,
    ) -> bool {
        let name: &[u8] = name.as_ref();
        let new_name = new_name.as_ref();
        let Some(parent_id) = self.ino_to_id.get(&parent_ino).cloned() else {
            return false;
        };
        let Some(new_parent_id) = self.ino_to_id.get(&new_parent_ino).cloned() else {
            return false;
        };
        let Some(id) = self.child_named(&parent_id, name) else {
            return false;
        };
        if new_parent_id == id || self.is_ancestor(&id, &new_parent_id) {
            return false;
        }
        // POSIX rename replaces the target: if the destination name is occupied, trash it.
        let mut replaced = None;
        if let Some(existing) = self.child_named(&new_parent_id, new_name)
            && existing != id
        {
            if !self.write_action(&fs_protocol::FileAction::Trash {
                node: existing.clone(),
            }) {
                return false;
            }
            self.reindex_node(&existing);
            replaced = Some(existing);
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        if !self.write_action(&fs_protocol::FileAction::Move {
            node: id.clone(),
            parent: new_parent_id.clone(),
            name: FileNameV1(new_name.to_vec()),
            mtime,
        }) {
            return false;
        }
        self.reindex_node(&id);
        self.index_refresh_subtree(&id);
        if let Some(replaced) = replaced {
            self.index_refresh_subtree(&replaced);
        }
        let _ = self.settle_structural_conflicts(&new_parent_id);
        if cur != new_parent_id {
            let _ = self.settle_structural_conflicts(&cur);
        }
        true
    }

    // Structural resolution (cycle breaking).

    /// Find one cycle in the parent graph (never reaching the root or trash), as the node ids
    /// forming the loop. Found in node-id order, which is part of the repair every peer must agree
    /// on; `starts` narrows the search to the nodes a delta placed.
    fn find_cycle(&self, starts: Option<&HashSet<String>>) -> Option<Vec<String>> {
        let mut ids: Vec<String> = match starts {
            Some(starts) => starts.iter().cloned().collect(),
            None => self.tree.placement.keys().cloned().collect(),
        };
        ids.sort_unstable();
        for start in &ids {
            let mut cur = start.clone();
            let mut seq: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            loop {
                if cur == ROOT_ID || cur == TRASH_ID {
                    break;
                }
                if let Some(pos) = seq.iter().position(|x| x == &cur) {
                    return seq.get(pos..).map(<[String]>::to_vec);
                }
                if !seen.insert(cur.clone()) {
                    break;
                }
                seq.push(cur.clone());
                match self.tree.placement.get(&cur) {
                    Some((parent, _)) => cur = parent.clone(),
                    None => break,
                }
            }
        }
        None
    }

    /// Break every move cycle deterministically: the lowest-id node in each reverts to `prev_parent`
    /// (or the root). A pure function of the doc, so every peer computes the same repair.
    pub fn resolve(&mut self) -> bool {
        self.resolve_nodes(None)
    }

    /// [`Self::resolve`] searching only the nodes a delta placed: the state it applied to held no
    /// cycle, so a cycle it made runs through one of them, and the repair is the one a whole scan
    /// would make.
    fn resolve_nodes(&mut self, candidates: Option<HashSet<String>>) -> bool {
        self.cycle_scan_pending = false;
        let mut starts = candidates;
        let mut repaired = Vec::new();
        let guard = match &starts {
            Some(starts) => starts.len().saturating_add(2),
            None => self.tree.placement.len().saturating_add(2),
        };
        for _ in 0..guard {
            let Some(cycle) = self.find_cycle(starts.as_ref()) else {
                break;
            };
            // `find_cycle` never returns an empty `Vec`.
            let Some(loser) = cycle.iter().min().cloned() else {
                break;
            };
            let pp = self
                .node(&loser)
                .and_then(|o| str_at(&self.doc, &o, "prev_parent"))
                .unwrap_or_else(|| ROOT_ID.to_string());
            let target = if cycle.contains(&pp)
                || (pp != ROOT_ID && pp != TRASH_ID && self.node(&pp).is_none())
            {
                ROOT_ID.to_string()
            } else {
                pp
            };
            self.set_field(&loser, "parent", &*target);
            if let Some(starts) = starts.as_mut() {
                starts.insert(loser.clone());
            }
            repaired.push(loser);
        }
        for id in &repaired {
            self.index_refresh_subtree(id);
        }
        !repaired.is_empty()
    }

    /// Break the cycles made since the last resolution. A document opened as saved may carry a
    /// cycle no resolution ran on, so it is searched whole once; after that only the nodes a delta
    /// placed are ([`Self::resolve_nodes`]).
    fn resolve_pending(&mut self) -> bool {
        let placed = std::mem::take(&mut self.placed_since_resolve);
        if std::mem::take(&mut self.cycle_scan_pending) {
            return self.resolve_nodes(None);
        }
        if placed.is_empty() {
            return false;
        }
        self.resolve_nodes(Some(placed))
    }

    /// Resolve a `/`-style path (root-relative, no leading slash) to a live entry.
    pub fn entry_at(&self, path: &str) -> Option<Entry> {
        let mut cur = ROOT_ID.to_string();
        if !path.is_empty() {
            for seg in path.split('/') {
                cur = self.child_named(&cur, seg)?;
            }
        }
        if cur == ROOT_ID || !self.reaches_root(&cur) {
            return None;
        }
        self.entry(&cur)
    }

    pub fn resolve_path_ino(&mut self, path: &str) -> Option<u64> {
        let mut cur = ROOT_ID.to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            cur = self.child_named(&cur, seg)?;
        }
        Some(self.ino_for_id(&cur))
    }

    pub fn live_node_ids(&self) -> Vec<String> {
        self.doc
            .sorted_node_ids()
            .into_iter()
            .filter(|id| self.reaches_root(id))
            .collect()
    }

    /// Stable entry lookup used by the storage authority; it exposes logical metadata only, never a
    /// host path.
    pub fn entry_for_node(&self, id: &str) -> Option<Entry> {
        #[cfg(test)]
        self.node_entry_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.entry(id).filter(|_| self.reaches_root(id))
    }

    /// Resolve the current kernel inode for a stable CRDT node identity.
    pub fn inode_for_node(&mut self, id: &str) -> Option<u64> {
        self.entry_for_node(id)?;
        Some(self.ino_for_id(id))
    }

    pub fn node_hash(&self, id: &str) -> Option<Hash> {
        self.entry(id)?.hash
    }

    pub fn node_parent_name(&self, id: &str) -> Option<(String, FileNameV1)> {
        Some((self.parent_of(id)?, self.name_of(id)?))
    }

    /// Record-index presentation without allocating a process-local inode. The caller still applies
    /// its live mount ACL to the path.
    pub fn node_is_dir(&self, id: &str) -> Option<bool> {
        self.node(id)?;
        Some(self.kind_of(id) == NodeKind::Dir)
    }

    pub fn child_id_named(&self, parent_id: &str, name: impl AsRef<[u8]>) -> Option<String> {
        self.child_named(parent_id, name)
    }

    pub fn content_conflicts(&self, id: &str) -> Vec<(String, String)> {
        let Some(obj) = self.node(id) else {
            return Vec::new();
        };
        match self.doc.get_all(&obj, "hash") {
            Ok(values) => values
                .into_iter()
                .filter_map(|(v, opid)| match v {
                    Value::Scalar(s) => match s.as_ref() {
                        ScalarValue::Str(x) => Some((opid.to_string(), x.to_string())),
                        _ => None,
                    },
                    Value::Object(_) => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn create_conflict_copy(
        &mut self,
        parent_id: &str,
        name: impl AsRef<[u8]>,
        hash: &Hash,
        size: u64,
        mtime: u64,
    ) -> bool {
        let name = name.as_ref();
        if self.child_named(parent_id, name).is_some() {
            return false;
        }
        self.create_node(parent_id, name, NodeKind::File, Some(hash), size, mtime)
            .is_some()
    }
}

/// Reversible presentation for byte names in path-based policy and metadata. Valid UTF-8 stays
/// readable; `%` and invalid bytes are escaped so distinct kernel names cannot collapse onto one
/// authorization path.
pub(crate) fn escape_path_name(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        if byte == b'%' {
            out.push_str("%25");
            offset = offset.saturating_add(1);
            continue;
        }
        let remaining = bytes.get(offset..).unwrap_or_default();
        match std::str::from_utf8(remaining) {
            Ok(rest) => {
                out.push_str(&rest.replace('%', "%25"));
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid != 0 {
                    let end = offset.saturating_add(valid);
                    if let Some(valid_bytes) = bytes.get(offset..end)
                        && let Ok(valid_text) = std::str::from_utf8(valid_bytes)
                    {
                        out.push_str(&valid_text.replace('%', "%25"));
                    }
                    offset = end;
                } else {
                    use std::fmt::Write as _;
                    let _ = write!(out, "%{byte:02X}");
                    offset = offset.saturating_add(1);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
