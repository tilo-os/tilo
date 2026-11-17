//! The namespace plane: an Automerge-backed move-tree CRDT, with nodes keyed by a
//! stable `NodeId` and an explicit `parent` register, so rename, move and delete are
//! real move operations: no duplicate parents, delete reparents to the `TRASH`
//! sentinel, and a concurrent cross-move cycle is deterministically broken by
//! [`Namespace::resolve`], so every peer converges identically.
//!
//! Inode numbers are a local `NodeId` to `u64` index; a rename keeps the node id, so
//! the inode stays stable.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, UNIX_EPOCH};

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::{
    ContentRevisionV1, FILE_BINDING_VERSION, FileActor, FileBindingV1, FileDocumentId,
    FileMutationTransactionManifest, FileNameV1, FileNodeKind, MAX_FILE_NAME_BYTES,
    TransactionFileRole,
};
use fuser::{FileAttr, FileType, INodeNo};
use iroh_blobs::Hash;
use uuid::Uuid;

pub const ROOT_INO: u64 = 1;
pub use fs_protocol::tree::ROOT_ID;
// The document's layout is `fs_protocol::tree`'s: the record that carries the tree reads
// the same document the mount writes.
use fs_protocol::tree::{
    CONTENT_REVISION_KEY, DOCUMENT_ID_KEY, NODES_KEY, PLACEMENT_NAME_KEY, ROOT_DOCUMENT_ID_KEY,
    TRASH_ID, document_node_id,
};
const RECORD_ID_MIGRATION_DOMAIN: &[u8] = b"tilo.collaborative-fs.namespace-record-id.v1";
/// Bounded fixed-point rounds for sibling name conflicts: each round gives a losing
/// record a longer, record-id-specific name, so a directory settles in a few dozen rounds.
const MAX_STRUCTURAL_CONFLICT_ROUNDS: usize = 64;
const STRUCTURAL_CONFLICT_MARKER: &[u8] = b".sync-conflict-";

pub const MAX_LIVE_RECORD_BLOB_SCAN: usize = 100_000;
mod changes;
mod changesets;
mod chunks;
mod documents;
mod records;
pub use records::ResourceWrite;
use records::bundle;
mod document_index;
pub mod history;
#[cfg(test)]
mod index_walk_tests;
mod kernel_changes;
mod leases;
mod posix;
mod projection;
pub use changesets::ChangesetMark;
use chunks::Chunks;
pub(crate) use chunks::Obj;
pub use kernel_changes::KernelChanges;
pub use leases::{LeaseRequest, VersionFence, WriteLease};
pub use posix::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, PosixAttrs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveDocumentBlobRoots {
    pub document_id: FileDocumentId,
    pub manifest: Option<Hash>,
    pub thumbnail: Option<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveBlobOwner {
    pub path: String,
    pub document_id: Option<FileDocumentId>,
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

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransactionProjectionError {
    #[error("signed transaction is invalid")]
    InvalidTransaction,
    #[error("transaction records do not resolve uniquely")]
    UnknownOrDuplicateDocument,
    #[error("transaction structural precondition no longer holds")]
    PreconditionFailed,
    #[error("replacement has incompatible node kinds")]
    IncompatibleKinds,
    #[error("replaced directory is not empty")]
    NonEmptyDirectory,
    #[error("replacement move would create a cycle")]
    DescendantMove,
    #[error("transaction content revision could not be installed")]
    ContentProjectionFailed,
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
    fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Dir => "dir",
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
    /// A CRDT-backed document whose bytes are its export: a codec under the legacy
    /// content authority.
    pub document: bool,
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
    pub path: String,
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
        hasher.update(b"tilo.fs.namespace-record-index.row.v2\0");
        hasher.update(record.document_id.0.as_bytes());
        for field in [&record.parent_id, &record.node_id, &record.path] {
            hasher.update(&(field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
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

#[derive(Clone)]
pub struct Namespace {
    /// The document; its every change is announced to [`Self::subscribe_changes`]
    /// ([`changes`]).
    doc: Chunks,
    ino_to_id: HashMap<u64, String>,
    id_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
    next_id: u64,
    /// Stable-record catalogue for the mount's paged record projection, updated with
    /// structural mutations rather than rebuilt by every page request.
    document_index: BTreeMap<FileDocumentId, IndexedDocument>,
    document_index_order: Vec<FileDocumentId>,
    document_index_revision: String,
    /// Stable record identities that are currently multiply bound; they stay absent from
    /// the public index until a full reconciliation proves the ambiguity gone.
    document_index_ambiguous: HashSet<FileDocumentId>,
    document_index_digest: DocumentIndexDigest,
    tree: TreeIndex,
    /// Every node carries a record id. Local creation always writes one, so only a
    /// document import can clear this (and each import re-runs the migration), which keeps
    /// `save_incremental` from rescanning all nodes.
    document_ids_complete: bool,
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
    /// Changes whenever changes another replica made are merged into the document, where
    /// concurrent edits and content conflicts can only arrive. Drawn from a process-wide
    /// counter so no two instances share a value.
    merge_generation: u64,
    /// When this namespace was opened, in milliseconds: the time an empty folder reads as changed.
    opened_ms: u64,
    #[cfg(test)]
    conflict_scans: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    document_index_page_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    node_entry_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    node_lookups: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

static NEXT_MERGE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_merge_generation() -> u64 {
    NEXT_MERGE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Cheap fingerprint of a [`Namespace`]: its document heads. Equal tokens mean no node,
/// path, hash or content revision changed, so a reconciler can skip a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceChangeToken {
    heads: Vec<automerge::ChangeHash>,
}

fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h.as_bytes())
}
pub(crate) fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

fn revision_to_bytes(revision: &ContentRevisionV1) -> Option<Vec<u8>> {
    revision.validate().ok()?;
    collab_types::canonical_bytes(revision).ok()
}

fn revision_from_bytes(bytes: &[u8]) -> Option<ContentRevisionV1> {
    let revision: ContentRevisionV1 = collab_types::canonical::from_canonical_bytes(bytes).ok()?;
    revision.validate().ok()?;
    Some(revision)
}

fn revision_from_scalar(value: &ScalarValue) -> Option<ContentRevisionV1> {
    match value {
        ScalarValue::Bytes(bytes) => revision_from_bytes(bytes),
        // Transitional compatibility with the earliest development encoding.
        ScalarValue::Str(value) => hex::decode(value)
            .ok()
            .and_then(|bytes| revision_from_bytes(&bytes)),
        _ => None,
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

fn map_at(doc: &Chunks, obj: &Obj, key: &str) -> Option<Obj> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
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

impl Namespace {
    /// A namespace over `doc`, its documents already in place, with only the root bound and
    /// every derived index empty.
    fn over(doc: Chunks, author: String, next_id: u64) -> Self {
        let mut ns = Self {
            doc,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author,
            next_id,
            document_index: BTreeMap::new(),
            document_index_order: Vec::new(),
            document_index_revision: String::new(),
            document_index_ambiguous: HashSet::new(),
            document_index_digest: DocumentIndexDigest::default(),
            tree: TreeIndex::default(),
            document_ids_complete: false,
            placed_since_resolve: HashSet::new(),
            cycle_scan_pending: true,
            #[cfg(test)]
            document_index_rebuilds: 0,
            merge_generation: next_merge_generation(),
            opened_ms: tilo_clock::unix_now_ns() / 1_000_000,
            #[cfg(test)]
            conflict_scans: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
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

    pub fn new(author: impl Into<String>) -> Result<Self, automerge::AutomergeError> {
        let mut ns = Self::over(Chunks::new(AutoCommit::new()), author.into(), 0);
        let root_document_id = FileDocumentId::new();
        ns.doc.put(
            &Obj::index(),
            ROOT_DOCUMENT_ID_KEY,
            root_document_id.0.to_string(),
        )?;
        // Name the founding change as the mount's now: a merge made later settles a pending
        // change without its name, and a device signs only the changes that name what made them.
        ns.doc.index_mut().commit_named();
        ns.rebuild_document_index();
        Ok(ns)
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    /// Changes whenever changes another replica made were merged in, and only then: equal
    /// values mean no new content conflict can have arrived, so a reconciler can skip
    /// [`crate::conflict::detect`].
    #[must_use]
    pub fn merge_generation(&self) -> u64 {
        self.merge_generation
    }

    #[cfg(test)]
    pub(crate) fn note_conflict_scan(&self) {
        self.conflict_scans
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    fn unique_node_id_for_document(&self, document_id: FileDocumentId) -> Option<String> {
        if self.document_id_for_id(ROOT_ID) == Some(document_id) {
            return Some(ROOT_ID.to_string());
        }
        let mut matches = self
            .doc
            .sorted_node_ids()
            .into_iter()
            .filter(|id| self.document_id_for_id(id) == Some(document_id));
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
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
            .and_then(|o| str_at(&self.doc, &o, "kind"))
            .map_or(NodeKind::File, |k| NodeKind::from_str(&k))
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
    /// name or author strings, and the record id and content revision resolved without looking
    /// the node up again.
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
        let revision = self.content_revision_of(obj, document_id);
        Some(NodeMeta {
            document_id,
            kind: NodeKind::from_str(&str_at(&self.doc, obj, "kind").unwrap_or_default()),
            hash: match revision {
                Some(revision) => revision.content_hash.map(|hash| Hash::from(hash.0)),
                None => str_at(&self.doc, obj, "hash").and_then(|h| hash_from_hex(&h)),
            },
            size: match revision {
                Some(revision) => revision.byte_len,
                None => read_u64(&self.doc, obj, "size"),
            },
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

    /// Apply one fully committed replacement-rename transaction while the namespace lock is
    /// held. All preconditions are checked before mutation; the returned delta holds both the
    /// victim tombstone and the source move.
    pub fn apply_replacement_transaction(
        &mut self,
        manifest: &FileMutationTransactionManifest,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        // Automerge transactions make the document edits atomic, but the verified-record
        // projection is maintained alongside the document. Apply to a complete clone so a late
        // projection failure cannot leave either authority half-mutated.
        let mut candidate = self.clone();
        let delta = candidate.apply_replacement_transaction_in_place(manifest)?;
        if delta.is_some() {
            *self = candidate;
        }
        Ok(delta)
    }

    /// Apply one multi-record mutation to a complete namespace clone, publishing the clone only
    /// after every precondition and both record projections succeed.
    pub fn apply_manifest(
        &mut self,
        manifest: &FileMutationTransactionManifest,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        match manifest.operation {
            fs_protocol::FileMutationOperation::RestoreContent { prior_revision, .. } => {
                let transaction_file = manifest
                    .files
                    .first()
                    .ok_or(TransactionProjectionError::InvalidTransaction)?;
                let revision = transaction_file
                    .proposed_content_revision
                    .ok_or(TransactionProjectionError::InvalidTransaction)?;
                let ino = self
                    .ensure_ino_for_document_id(transaction_file.document_id)
                    .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;
                let current = self.content_revision_for(ino);
                if current == Some(revision) {
                    return Ok(None);
                }
                if current != Some(prior_revision) {
                    return Err(TransactionProjectionError::PreconditionFailed);
                }
                if !self.set_content_revision(ino, &revision, manifest.timestamp_ms) {
                    return Err(TransactionProjectionError::ContentProjectionFailed);
                }
                Ok(Some(self.save_incremental()))
            }
            fs_protocol::FileMutationOperation::AtomicSave { .. } => {
                Err(TransactionProjectionError::InvalidTransaction)
            }
            fs_protocol::FileMutationOperation::ReplacementRename { .. } => {
                self.apply_replacement_transaction(manifest)
            }
            fs_protocol::FileMutationOperation::KeepBothContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_keep_both_transaction_in_place(manifest)?;
                if delta.is_some() {
                    *self = candidate;
                }
                Ok(delta)
            }
            fs_protocol::FileMutationOperation::MergeContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_merge_transaction_in_place(manifest)?;
                if delta.is_some() {
                    *self = candidate;
                }
                Ok(delta)
            }
        }
    }

    fn apply_merge_transaction_in_place(
        &mut self,
        manifest: &FileMutationTransactionManifest,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        let fs_protocol::FileMutationOperation::MergeContentConflict {
            source_document,
            parent_document,
            source_name,
            ..
        } = &manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = manifest
            .files
            .first()
            .filter(|transaction_file| transaction_file.role == TransactionFileRole::MergeSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_document(*source_document)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;
        let ino = self.ino_for_id(&source_id);
        let parent_id = self
            .unique_node_id_for_document(*parent_document)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let current = self.content_revision_for(ino);
        if current == source.proposed_content_revision {
            return Ok(None);
        }
        if self.parent_of(&source_id) != Some(parent_id)
            || self.name_of(&source_id).as_deref() != Some(source_name.as_bytes())
            || current.is_none_or(|revision| revision.version_id != source.expected_version)
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        let revision = source
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        if !self.set_content_revision_preserving_preview(ino, &revision, manifest.timestamp_ms) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        Ok(Some(self.save_incremental()))
    }

    /// Install the exact namespace delta durably staged for a Keep Both transaction, then project
    /// both signed content revisions. Automerge node ids are actor-local, so replaying the
    /// semantic mutation could produce different bytes; recovery applies the journaled bytes
    /// peers will receive.
    pub fn apply_staged_keep_both_delta(
        &mut self,
        manifest: &FileMutationTransactionManifest,
        delta: &[u8],
    ) -> Result<(), TransactionProjectionError> {
        let fs_protocol::FileMutationOperation::KeepBothContentConflict {
            source_document,
            sibling_document,
            parent_document,
            source_name,
            sibling_name,
            ..
        } = &manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = manifest
            .files
            .iter()
            .find(|transaction_file| transaction_file.role == TransactionFileRole::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = manifest
            .files
            .iter()
            .find(|transaction_file| transaction_file.role == TransactionFileRole::KeepBothSibling)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let mut candidate = self.clone();
        let source_id = candidate
            .unique_node_id_for_document(*source_document)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;
        let source_ino = candidate.ino_for_id(&source_id);
        let parent_id = candidate
            .unique_node_id_for_document(*parent_document)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let current_source = candidate.content_revision_for(source_ino);
        if candidate.parent_of(&source_id) != Some(parent_id.clone())
            || candidate.name_of(&source_id).as_deref() != Some(source_name.as_bytes())
            || !matches!(current_source, Some(revision)
                if revision.version_id == source.expected_version
                    || Some(revision) == source.proposed_content_revision)
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        candidate
            .apply_incremental(delta)
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let sibling_id = candidate
            .unique_node_id_for_document(*sibling_document)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let sibling_ino = candidate.ino_for_id(&sibling_id);
        if candidate.parent_of(&sibling_id) != Some(parent_id)
            || candidate.name_of(&sibling_id).as_deref() != Some(sibling_name.as_bytes())
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        let source_revision = source
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling_revision = sibling
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        if !candidate.set_content_revision_preserving_preview(
            source_ino,
            &source_revision,
            manifest.timestamp_ms,
        ) || !candidate.set_content_revision_preserving_preview(
            sibling_ino,
            &sibling_revision,
            manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        *self = candidate;
        Ok(())
    }

    fn apply_keep_both_transaction_in_place(
        &mut self,
        manifest: &FileMutationTransactionManifest,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        let fs_protocol::FileMutationOperation::KeepBothContentConflict {
            source_document,
            sibling_document,
            parent_document,
            source_name,
            sibling_name,
            ..
        } = &manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = manifest
            .files
            .iter()
            .find(|transaction_file| transaction_file.role == TransactionFileRole::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = manifest
            .files
            .iter()
            .find(|transaction_file| transaction_file.role == TransactionFileRole::KeepBothSibling)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_document(*source_document)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;
        let source_ino = self.ino_for_id(&source_id);
        let parent_id = self
            .unique_node_id_for_document(*parent_document)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let parent_ino = self.ino_for_id(&parent_id);

        // Exact replay after the full namespace snapshot was persisted.
        if let Some(sibling_id) = self.unique_node_id_for_document(*sibling_document) {
            let sibling_ino = self.ino_for_id(&sibling_id);
            if self.parent_of(&source_id) == Some(parent_id.clone())
                && self.name_of(&source_id).as_deref() == Some(source_name.as_bytes())
                && self.parent_of(&sibling_id) == Some(parent_id)
                && self.name_of(&sibling_id).as_deref() == Some(sibling_name.as_bytes())
                && self.content_revision_for(source_ino) == source.proposed_content_revision
                && self.content_revision_for(sibling_ino) == sibling.proposed_content_revision
            {
                return Ok(None);
            }
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        if self.parent_of(&source_id) != Some(parent_id.clone())
            || self.name_of(&source_id).as_deref() != Some(source_name.as_bytes())
            || !matches!(
                self.content_revision_for(source_ino),
                Some(revision)
                    if revision.version_id == source.expected_version
                        || Some(revision) == source.proposed_content_revision
            )
            || self.lookup(parent_ino, sibling_name).is_some()
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        let sibling_revision = sibling
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling_hash = sibling_revision.content_hash.map(|hash| Hash::from(hash.0));
        let sibling_ino = self
            .create_projected_file(
                parent_ino,
                sibling_name,
                *sibling_document,
                sibling_hash.as_ref(),
                sibling_revision.byte_len,
                manifest.timestamp_ms,
            )
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let source_revision = source
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        if !self.set_content_revision_preserving_preview(
            source_ino,
            &source_revision,
            manifest.timestamp_ms,
        ) || !self.set_content_revision_preserving_preview(
            sibling_ino,
            &sibling_revision,
            manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        Ok(Some(self.save_incremental()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Replacement projection keeps signature and file checks, replay detection, namespace preconditions, rename, and revision installation in order."
    )]
    fn apply_replacement_transaction_in_place(
        &mut self,
        manifest: &FileMutationTransactionManifest,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        let fs_protocol::FileMutationOperation::ReplacementRename {
            source_parent,
            source_name,
            destination_parent,
            destination_name,
            source_document,
            replaced_document,
        } = &manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source_file = manifest
            .files
            .iter()
            .find(|value| value.role == TransactionFileRole::Source)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let replaced_file = manifest
            .files
            .iter()
            .find(|value| value.role == TransactionFileRole::Replaced)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_document(*source_document)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;
        let replaced_id = self
            .unique_node_id_for_document(*replaced_document)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateDocument)?;

        let source_live = self.reaches_root(&source_id);
        let replaced_live = self.reaches_root(&replaced_id);
        let source_parent_now = self
            .parent_of(&source_id)
            .and_then(|id| self.document_id_for_id(&id));
        let source_name_now = self.name_of(&source_id);
        let replaced_parent_now = self
            .parent_of(&replaced_id)
            .and_then(|id| self.document_id_for_id(&id));
        let replaced_name_now = self.name_of(&replaced_id);

        // Exact replay after namespace persistence is success and emits no
        // second Automerge change.
        if source_live
            && !replaced_live
            && source_parent_now == Some(*destination_parent)
            && source_name_now.as_deref() == Some(destination_name.as_bytes())
            && self.parent_of(&replaced_id).as_deref() == Some(TRASH_ID)
        {
            return Ok(None);
        }
        if !source_live
            || !replaced_live
            || source_parent_now != Some(*source_parent)
            || source_name_now.as_deref() != Some(source_name.as_bytes())
            || replaced_parent_now != Some(*destination_parent)
            || replaced_name_now.as_deref() != Some(destination_name.as_bytes())
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        // Inodes are local cache identities and are intentionally not
        // persisted. A remote transaction can be the first operation after a
        // restart, before FUSE lookup has populated either mapping.
        let source_ino = self.ino_for_id(&source_id);
        let replaced_ino = self.ino_for_id(&replaced_id);
        let source_kind = self.kind_of(&source_id);
        let replaced_kind = self.kind_of(&replaced_id);
        if source_kind != replaced_kind {
            return Err(TransactionProjectionError::IncompatibleKinds);
        }
        if replaced_kind == NodeKind::Dir
            && self
                .readdir(replaced_ino)
                .is_some_and(|entries| entries.len() > 2)
        {
            return Err(TransactionProjectionError::NonEmptyDirectory);
        }
        let destination_parent_id = self
            .unique_node_id_for_document(*destination_parent)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let source_parent_id = self
            .unique_node_id_for_document(*source_parent)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        if source_id == destination_parent_id
            || self.is_ancestor(&source_id, &destination_parent_id)
        {
            return Err(TransactionProjectionError::DescendantMove);
        }
        let destination_parent_ino = self.ino_for_id(&destination_parent_id);
        let source_parent_ino = self.ino_for_id(&source_parent_id);
        if !self.rename(
            source_parent_ino,
            source_name,
            destination_parent_ino,
            destination_name,
            manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        if let Some(revision) = &source_file.proposed_content_revision
            && !self.set_content_revision_preserving_preview(
                source_ino,
                revision,
                manifest.timestamp_ms,
            )
        {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        debug_assert_eq!(replaced_file.document_id, *replaced_document);
        Ok(Some(self.save_incremental()))
    }

    /// Winning atomic content pointer for this inode. Malformed or cross-record values fail
    /// closed to the legacy projection. Callers that cross a trust boundary must additionally
    /// verify the referenced version in the signed record repository.
    pub fn content_revision_for(&self, ino: u64) -> Option<ContentRevisionV1> {
        self.content_revision_for_id(self.id_for_ino(ino)?)
    }

    pub(crate) fn content_revision_for_id(&self, id: &str) -> Option<ContentRevisionV1> {
        self.raw_content_revision_for_id(id)
    }

    fn content_revision_of(
        &self,
        obj: &Obj,
        document_id: FileDocumentId,
    ) -> Option<ContentRevisionV1> {
        let revision = match self.doc.get(obj, CONTENT_REVISION_KEY) {
            Ok(Some((Value::Scalar(value), _))) => revision_from_scalar(value.as_ref())?,
            _ => return None,
        };
        (revision.document_id == document_id).then_some(revision)
    }

    fn raw_content_revision_for_id(&self, id: &str) -> Option<ContentRevisionV1> {
        let obj = self.node(id)?;
        let revision = match self.doc.get(&obj, CONTENT_REVISION_KEY) {
            Ok(Some((Value::Scalar(value), _))) => revision_from_scalar(value.as_ref())?,
            _ => return None,
        };
        (self.document_id_for_id(id) == Some(revision.document_id)).then_some(revision)
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
    /// a local inode, so the network content gate can map a requested blob back to every live
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

    #[cfg(test)]
    pub fn live_paths_for_blob(&self, hash: Hash) -> Vec<String> {
        self.live_node_ids()
            .into_iter()
            .filter(|id| {
                self.node_hash(id) == Some(hash) || self.thumbnail_for_id(id) == Some(hash)
            })
            .filter_map(|id| self.path_for_id(&id))
            .collect()
    }

    #[cfg(test)]
    pub fn live_document_ids_for_direct_blob(
        &self,
        hash: Hash,
        max_live_documents: usize,
    ) -> Result<Vec<FileDocumentId>, LiveDocumentBlobScanError> {
        let mut records: Vec<_> = self
            .live_document_blob_roots(max_live_documents)?
            .into_iter()
            .filter(|roots| roots.manifest == Some(hash) || roots.thumbnail == Some(hash))
            .map(|roots| roots.document_id)
            .collect();
        records.sort();
        records.dedup();
        Ok(records)
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
    /// [`Self::getattr_including_unlinked`]), [`Self::hash_for`], [`Self::path_for`] and
    /// [`Self::document_codec`] would each answer. The path is built only when `query.path` asks
    /// for it, so a mount without an ACL never looks at it.
    pub(crate) fn inode_view(&self, ino: u64, query: InodeQuery) -> Option<InodeView> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(InodeView {
                attr: self.root_attr(),
                hash: None,
                live: true,
                path: query.path.then(|| "/".to_string()),
                document: false,
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
            document: str_at(&self.doc, &obj, "codec").is_some(),
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
                let name = self.name_of(&c)?;
                Some((c.clone(), name, self.kind_of(&c)))
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
        let newest = self.doc.chunk_numbers().last().copied();
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
        let obj = self.doc.put_node(document_id.chunk(), &id).ok()?;
        let _ = self.doc.put(&obj, "parent", parent_id);
        let _ = self.doc.put(&obj, "prev_parent", parent_id);
        let _ = self.doc.put(&obj, "name", name.to_vec());
        let _ = self.doc.put(&obj, PLACEMENT_NAME_KEY, name.to_vec());
        let _ = self.doc.put(&obj, "kind", kind.as_str());
        if let Some(h) = hash {
            let _ = self.doc.put(&obj, "hash", hash_to_hex(h));
        }
        let _ = self.doc.put(&obj, "size", size);
        let _ = self.doc.put(&obj, "mtime", mtime);
        let _ = self.doc.put(&obj, "author", self.author.clone());
        let _ = self
            .doc
            .put(&obj, DOCUMENT_ID_KEY, document_id.0.to_string());
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

    pub fn create_projected_file_at_revision(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        revision: &ContentRevisionV1,
        mtime: u64,
    ) -> Option<u64> {
        let hash = revision.content_hash.map(|hash| Hash::from(hash.0));
        let ino = self.create_projected_file(
            parent_ino,
            name,
            revision.document_id,
            hash.as_ref(),
            revision.byte_len,
            mtime,
        )?;
        self.set_content_revision(ino, revision, mtime)
            .then_some(ino)
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
            self.record_heads().into_values().flatten().collect();
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

    /// Whether the document file `ino` (its node: codec and content) changed between namespace heads
    /// `before` and `after`. The diff walks only that node's objects. Unknown inodes count as
    /// changed.
    pub fn document_changed_between(
        &mut self,
        ino: u64,
        before: &[automerge::ChangeHash],
        after: &[automerge::ChangeHash],
    ) -> bool {
        let Some(node) = self.ino_to_id.get(&ino).and_then(|id| self.node(id)) else {
            return true;
        };
        let Some(doc) = self.doc.doc_mut(node.chunk) else {
            return true;
        };
        let mut of_doc = |heads: &[automerge::ChangeHash]| -> Vec<automerge::ChangeHash> {
            heads
                .iter()
                .copied()
                .filter(|hash| doc.has_change(hash))
                .collect()
        };
        let (before, after) = (of_doc(before), of_doc(after));
        doc.diff_obj(&node.id, &before, &after, true)
            .map_or(true, |patches| !patches.is_empty())
    }

    /// Every change this document holds beyond `heads`, as one incremental
    /// chunk for [`Self::apply_remote_and_resolve`]. Heads this document
    /// does not know (the asker's own changes) are ignored, so an asker that
    /// shares no history receives everything.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<u8> {
        self.note_change();
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

    fn with_doc_mut<R>(
        &mut self,
        ino: u64,
        f: impl FnOnce(&mut tilo_doc::DocMut) -> R,
    ) -> Option<R> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let root = self.document_root(&id)?;
        let doc = &mut **self.doc.doc_mut(root.chunk)?;
        let containers = tilo_doc::model::open(doc, &root.id).ok()?;
        let mut ids = tilo_doc::model::seeded_id_gen(doc, &containers);
        let mut dmut = tilo_doc::DocMut {
            doc,
            c: &containers,
            ids: &mut ids,
        };
        Some(f(&mut dmut))
    }

    pub fn document_insert_block(
        &mut self,
        ino: u64,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        self.with_doc_mut(ino, |d| d.insert_block(after, kind))
    }

    pub fn document_splice(
        &mut self,
        ino: u64,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) -> bool {
        self.with_doc_mut(ino, |d| d.splice_text(block_id, idx, del, ins))
            .is_some()
    }

    pub fn document_block_text(&self, ino: u64, block_id: &str) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let doc = &**self.doc.doc(root.chunk)?;
        let containers = tilo_doc::model::open(doc, &root.id).ok()?;
        tilo_doc::model::block_text(doc, &containers, block_id)
    }

    pub fn document_snapshot(&self, ino: u64) -> Option<tilo_doc::DocSnapshot> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let doc = &**self.doc.doc(root.chunk)?;
        let containers = tilo_doc::model::open(doc, &root.id).ok()?;
        Some(
            tilo_doc::DocRef {
                doc,
                c: &containers,
            }
            .materialize(),
        )
    }

    pub fn set_content(&mut self, ino: u64, hash: Option<&Hash>, size: u64, mtime: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        // A legacy mutation explicitly supersedes any atomic revision, so remaining old call sites
        // cannot change mirror fields that revision-aware readers ignore.
        if let Some(obj) = self.node(&id) {
            let _ = self.doc.delete(&obj, CONTENT_REVISION_KEY);
        }
        match hash {
            Some(h) => self.set_field(&id, "hash", hash_to_hex(h)),
            None => {
                if let Some(obj) = self.node(&id) {
                    let _ = self.doc.delete(&obj, "hash");
                }
            }
        }
        if let Some(obj) = self.node(&id) {
            let _ = self.doc.delete(&obj, "thumb");
        }
        self.set_field(&id, "size", size);
        self.set_field(&id, "mtime", mtime);
        let author = self.author.clone();
        self.set_field(&id, "author", author);
        true
    }

    /// Atomically advertise one immutable signed history version as the current file content. Loose
    /// hash and size fields are mirrored only for legacy readers; revision-aware readers select this
    /// single CRDT scalar and so cannot observe a torn content tuple.
    pub fn set_content_revision(
        &mut self,
        ino: u64,
        revision: &ContentRevisionV1,
        mtime: u64,
    ) -> bool {
        self.install_content_revision(ino, revision, mtime, true)
    }

    /// Advance only the signed record and version pointer while retaining a preview of unchanged
    /// bytes (for rename and move metadata versions).
    pub fn set_content_revision_preserving_preview(
        &mut self,
        ino: u64,
        revision: &ContentRevisionV1,
        mtime: u64,
    ) -> bool {
        self.install_content_revision(ino, revision, mtime, false)
    }

    fn install_content_revision(
        &mut self,
        ino: u64,
        revision: &ContentRevisionV1,
        mtime: u64,
        invalidate_preview: bool,
    ) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none()
            || self.document_id_for_id(&id) != Some(revision.document_id)
            || revision.validate().is_err()
        {
            return false;
        }
        // When the document already carries this revision at this time (the author wrote it, or it
        // is projected again), verifying it is all that is left: writing the same fields again would
        // only add operations every peer has to sync. Concurrent values are resolved by writing the
        // winner.
        if self.raw_content_revision_for_id(&id) == Some(*revision)
            && !self.has_concurrent_content(&id)
            && self
                .node(&id)
                .is_some_and(|obj| read_u64(&self.doc, &obj, "mtime") == mtime)
        {
            return true;
        }
        let Some(encoded) = revision_to_bytes(revision) else {
            return false;
        };
        self.set_field(&id, CONTENT_REVISION_KEY, ScalarValue::Bytes(encoded));
        match revision.content_hash {
            Some(hash) => self.set_field(&id, "hash", hash_to_hex(&Hash::from(hash.0))),
            None => {
                if let Some(obj) = self.node(&id) {
                    let _ = self.doc.delete(&obj, "hash");
                }
            }
        }
        if invalidate_preview && let Some(obj) = self.node(&id) {
            let _ = self.doc.delete(&obj, "thumb");
        }
        self.set_field(&id, "size", revision.byte_len);
        self.set_field(&id, "mtime", mtime);
        let author = self.author.clone();
        self.set_field(&id, "author", author);
        true
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

    /// Every live node naming content, with its path: the input of the blob provider's reverse
    /// index. One pass over the tree, where a lookup per blob would scan it per request. A record
    /// placed at more than one live node publishes no thumbnail, as in
    /// [`Self::thumbnail_for_id`].
    pub(crate) fn live_blob_owners(&self) -> Vec<LiveBlobOwner> {
        let live = self.live_node_ids();
        live.iter()
            .filter_map(|id| {
                let document_id = self.document_id_for_id(id);
                let manifest = self.node_hash(id);
                let thumbnail = self
                    .node(id)
                    .and_then(|obj| str_at(&self.doc, &obj, "thumb"))
                    .and_then(|value| hash_from_hex(&value));
                if manifest.is_none() && thumbnail.is_none() {
                    return None;
                }
                Some(LiveBlobOwner {
                    path: self.path_for_id(id)?,
                    document_id,
                    manifest,
                    thumbnail,
                })
            })
            .collect()
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
        // Record the content hash at deletion time, so a concurrent edit can be detected and
        // resurrected.
        match self.node_hash(&id) {
            Some(h) => self.set_field(&id, "deleted_hash", hash_to_hex(&h)),
            None => {
                if let Some(obj) = self.node(&id) {
                    let _ = self.doc.delete(&obj, "deleted_hash");
                }
            }
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        self.set_field(&id, "prev_parent", cur.clone());
        self.set_field(&id, "parent", TRASH_ID);
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
            if let Some(h) = self.node_hash(&existing) {
                self.set_field(&existing, "deleted_hash", hash_to_hex(&h));
            }
            let ep = self
                .parent_of(&existing)
                .unwrap_or_else(|| ROOT_ID.to_string());
            self.set_field(&existing, "prev_parent", ep);
            self.set_field(&existing, "parent", TRASH_ID);
            replaced = Some(existing);
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        self.set_field(&id, "prev_parent", cur.clone());
        self.set_field(&id, "parent", &*new_parent_id);
        self.set_field(&id, PLACEMENT_NAME_KEY, new_name.to_vec());
        self.set_field(&id, "name", new_name.to_vec());
        self.set_field(&id, "mtime", mtime);
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

    fn has_concurrent_content(&self, id: &str) -> bool {
        let Some(obj) = self.node(id) else {
            return false;
        };
        [CONTENT_REVISION_KEY, "hash"].into_iter().any(|key| {
            self.doc
                .get_all(&obj, key)
                .is_ok_and(|values| values.len() > 1)
        })
    }

    /// Concurrent atomic revision values for a node. Cross-record or malformed scalars are omitted.
    pub fn content_revision_conflicts(&self, id: &str) -> Vec<(String, ContentRevisionV1)> {
        let Some(obj) = self.node(id) else {
            return Vec::new();
        };
        let document_id = self.document_id_for_id(id);
        match self.doc.get_all(&obj, CONTENT_REVISION_KEY) {
            Ok(values) => values
                .into_iter()
                .filter_map(|(value, opid)| match value {
                    Value::Scalar(value) => revision_from_scalar(value.as_ref())
                        .filter(|revision| Some(revision.document_id) == document_id)
                        .map(|revision| (opid.to_string(), revision)),
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
