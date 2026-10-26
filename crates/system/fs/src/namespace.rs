//! The namespace plane: an Automerge-backed move-tree CRDT (Kleppmann model).
//! Nodes are keyed by a stable `NodeId`, not path, with an explicit `parent`
//! register, so rename/move/delete are real move ops: no duplicate parents,
//! delete reparents to the `TRASH` sentinel (a concurrently-created child ends
//! up trashed, never dangling), and a concurrent cross-move cycle is
//! deterministically broken by [`Namespace::resolve`] (lowest-id node in the
//! cycle reverts to `prev_parent`/`ROOT`), so every peer converges identically.
//!
//! Inode numbers are a local `NodeId`<->`u64` index; renames keep the node id,
//! so the inode stays stable across renames.

#[cfg(all(feature = "filesystem", target_os = "linux"))]
pub(crate) mod external_projection;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, UNIX_EPOCH};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::{
    ContentRevisionV1, FILE_BINDING_VERSION, FileActor, FileBindingV1, FileMutationParticipantRole,
    FileMutationTransaction, FileNameV1, FileNodeKind, FileRecordId, MAX_FILE_NAME_BYTES,
};
use fuser::{FileAttr, FileType, INodeNo};
use iroh_blobs::Hash;
use tilo_automerge::ApplyIncoming;
use tilo_automerge::read::{map_at, str_at};
use uuid::Uuid;

pub const ROOT_INO: u64 = 1;
/// Sentinel parent for the tree root (not materialized as a node).
pub const ROOT_ID: &str = "ROOT";
/// Sentinel parent for deleted subtrees (not materialized as a node).
const TRASH_ID: &str = "TRASH";
const ROOT_RECORD_ID_KEY: &str = "root_record_id";
const RECORD_ID_KEY: &str = "record_id";
/// The name the record's latest signed binding asked for. The visible `name`
/// may differ when two records want one name; this is what makes that
/// resolution a pure function of the signed bindings rather than of arrival
/// order.
const PLACEMENT_NAME_KEY: &str = "placement_name";
const CONTENT_REVISION_KEY: &str = "content_revision_v1";
const RECORD_ID_MIGRATION_DOMAIN: &[u8] = b"tilo.collaborative-fs.namespace-record-id.v1";
/// Prefix of a node id derived from its stable record id, so every device
/// independently rebuilding the same record log creates the same node object.
const RECORD_NODE_ID_PREFIX: &str = "record:";
/// Bounded fixed-point rounds for sibling name conflicts. Each round a losing
/// record gets a longer, record-id-specific name, so a directory with N
/// records settles in at most a few dozen rounds in practice.
const MAX_STRUCTURAL_CONFLICT_ROUNDS: usize = 64;
const STRUCTURAL_CONFLICT_MARKER: &[u8] = b".sync-conflict-";

pub const MAX_LIVE_RECORD_BLOB_SCAN: usize = 100_000;
/// Most peer devices a namespace remembers sync progress with. An exchange
/// with a device beyond it starts from a full sync, as a first one does.
const MAX_PEER_SYNC_STATES: usize = 1024;
/// The persisted form of the sync progress ([`Namespace::encode_peer_sync_states`]):
/// this tag, which names its version, then the entry count (`u32` LE), then
/// per entry the device key (32 bytes), the encoded state's length (`u32`
/// LE) and the state; then a BLAKE3 hash of everything before it.
const PEER_SYNC_STATES_TAG: &[u8; 8] = b"TFPSYNC1";
/// The longest encoded sync state one entry may hold: a state is the agreed
/// heads, 32 bytes each, and a document rarely has more than a few heads.
const MAX_PEER_SYNC_STATE_BYTES: usize = 64 * 1024;

mod changes;
mod changesets;
mod kernel_changes;
mod leases;
mod posix;
use changes::ChangeTrackedDoc;
pub use changesets::ChangesetMark;
pub use kernel_changes::KernelChanges;
pub use leases::{LeaseRequest, VersionFence, WriteLease};
pub use posix::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, PosixAttrs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRecordBlobRoots {
    pub record_id: FileRecordId,
    pub manifest: Option<Hash>,
    pub thumbnail: Option<Hash>,
}

/// One live node naming content: its path (for path-scoped grants), its
/// record (for record-scoped grants) and the blobs it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveBlobOwner {
    pub path: String,
    pub record_id: Option<FileRecordId>,
    pub manifest: Option<Hash>,
    pub thumbnail: Option<Hash>,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveRecordBlobScanError {
    #[error("live-record blob scan limit must be between 1 and {MAX_LIVE_RECORD_BLOB_SCAN}")]
    InvalidLimit,
    #[error("live namespace exceeds the blob scan limit of {limit} records")]
    LimitExceeded { limit: usize },
    #[error("live namespace record is missing its stable record identity")]
    MissingRecordIdentity,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindingProjectionError {
    #[error("signed binding does not match a unique stable record")]
    UnknownOrDuplicateRecord,
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
    UnknownOrDuplicateRecord,
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

/// Which plane is authoritative for file bytes on this mount. Signed mode is
/// selected only from trusted local configuration when record services are
/// available; it is never negotiated through the unsigned namespace itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentAuthority {
    LegacyNamespace,
    SignedRecords,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VerifiedRecordProjection {
    revision: ContentRevisionV1,
    thumbnail: Option<Hash>,
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
    pub record_id: FileRecordId,
    pub parent: String,
    pub name: FileNameV1,
    pub kind: NodeKind,
    pub hash: Option<Hash>,
    pub size: u64,
    pub mtime: u64,
    pub author: String,
}

/// The fields of an [`Entry`] that serve FUSE attributes and content reads.
#[derive(Clone, Copy, Debug)]
struct NodeMeta {
    record_id: FileRecordId,
    kind: NodeKind,
    hash: Option<Hash>,
    size: u64,
    mtime: u64,
}

/// What [`Namespace::inode_view`] is asked for.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InodeQuery {
    /// Also describe an inode no longer reachable from the root, as a kernel
    /// handle opened before its unlink or replacement may.
    pub unlinked: bool,
    /// Build the inode's absolute path, for the ACL check.
    pub path: bool,
}

/// One inode as a single FUSE request sees it, read under one namespace
/// lock. See [`Namespace::inode_view`].
#[derive(Clone, Debug)]
pub(crate) struct InodeView {
    pub attr: FileAttr,
    pub hash: Option<Hash>,
    /// Reachable from the root, as [`Namespace::getattr`] requires.
    pub live: bool,
    /// The absolute path, when asked for and the inode is live.
    pub path: Option<String>,
    /// A CRDT-backed document whose bytes are its export: a codec under the
    /// legacy content authority.
    pub document: bool,
}

/// One authoritative, path-current row in the namespace-owned record index.
///
/// The record UUID is the durable identity. `path` is deliberately projection
/// metadata: it is replaced on a rename/move and is never accepted as an
/// authority token by a caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedRecord {
    pub record_id: FileRecordId,
    pub node_id: String,
    pub parent_id: String,
    pub path: String,
    pub is_dir: bool,
}

/// Structural index over every node in the document: where each node sits
/// and, per parent, its children by name. `lookup`, `readdir` and name checks
/// consult it instead of scanning every node. Local placement writes update
/// it node by node; every opaque document import rebuilds it.
#[derive(Clone, Debug, Default)]
struct TreeIndex {
    placement: HashMap<String, (String, FileNameV1)>,
    children: HashMap<String, BTreeMap<FileNameV1, BTreeSet<String>>>,
    /// The nodes carrying each stable record id, live or trashed. A node's
    /// record id is written once, when the node is created or migrated.
    records: HashMap<FileRecordId, BTreeSet<String>>,
}

impl TreeIndex {
    fn bind_record(&mut self, id: &str, record_id: FileRecordId) {
        self.records
            .entry(record_id)
            .or_default()
            .insert(id.to_string());
    }

    fn nodes_of(&self, record_id: FileRecordId) -> impl Iterator<Item = &String> {
        self.records.get(&record_id).into_iter().flatten()
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

    /// Nodes placed under `parent` as `name`, in id order.
    fn named<'a>(&'a self, parent: &str, name: &[u8]) -> impl Iterator<Item = &'a String> {
        let name = FileNameV1::from(name.to_vec());
        self.children
            .get(parent)
            .and_then(|names| names.get(&name))
            .into_iter()
            .flatten()
    }

    /// Every node placed under `parent`, in (name, id) order.
    fn children<'a>(&'a self, parent: &str) -> impl Iterator<Item = &'a String> {
        self.children
            .get(parent)
            .into_iter()
            .flat_map(BTreeMap::values)
            .flatten()
    }
}

/// Order-independent digest of the record index: the wrapping 256-bit sum of
/// one digest per row, so adding or removing a row updates it in O(1)
/// instead of rehashing every row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RecordIndexDigest([u64; 4]);

impl RecordIndexDigest {
    fn row(record: &IndexedRecord) -> [u64; 4] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.namespace-record-index.row.v2\0");
        hasher.update(record.record_id.0.as_bytes());
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

    fn add(&mut self, record: &IndexedRecord) {
        let row = Self::row(record);
        let mut carry = false;
        for (limb, value) in self.0.iter_mut().zip(row) {
            let (sum, overflow_a) = limb.overflowing_add(value);
            let (sum, overflow_b) = sum.overflowing_add(u64::from(carry));
            *limb = sum;
            carry = overflow_a || overflow_b;
        }
    }

    fn remove(&mut self, record: &IndexedRecord) {
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
    /// The document; its every change is announced to
    /// [`Self::subscribe_changes`] (see [`changes`]).
    doc: ChangeTrackedDoc,
    nodes: ObjId,
    ino_to_id: HashMap<u64, String>,
    id_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
    next_id: u64,
    content_authority: ContentAuthority,
    verified_records: HashMap<FileRecordId, VerifiedRecordProjection>,
    /// Stable-record catalogue used by the mount's paged record projection.
    /// It is updated with structural mutations rather than being reconstructed
    /// by every xattr manifest/page request.
    record_index: BTreeMap<FileRecordId, IndexedRecord>,
    record_index_order: Vec<FileRecordId>,
    record_index_revision: String,
    record_index_by_node: HashMap<String, FileRecordId>,
    record_index_children: HashMap<String, BTreeSet<String>>,
    /// Stable record identities that are currently multiply bound.  They are
    /// deliberately absent from the public index until a full reconciliation
    /// proves the ambiguity gone.
    record_index_ambiguous: HashSet<FileRecordId>,
    record_index_digest: RecordIndexDigest,
    tree: TreeIndex,
    /// Every node carries a record id. Local creation always writes one, so
    /// only a document import can clear this (and each import re-runs the
    /// migration), which keeps `save_incremental` from rescanning all nodes.
    record_ids_complete: bool,
    /// Heads after the last post-sync resolution: a sync that brought
    /// nothing new needs none.
    resolved_heads: Option<Vec<automerge::ChangeHash>>,
    /// While set (to the heads the batch began at, and the changes not yet
    /// taken then), [`Self::save_incremental`] returns nothing, so a batch
    /// of changes becomes one delta when it ends. Encoding a delta
    /// regenerates its changes from the op set (a pass over the document),
    /// so one per changed file is quadratic in a changeset.
    incremental_batch: Option<(Vec<automerge::ChangeHash>, Vec<u8>)>,
    /// Per peer device (its certified key): the heads the last completed
    /// sync exchange with it agreed both sides hold, as an encoded
    /// [`automerge::sync::State`]. Without them every exchange would start
    /// by summarising the document's whole history.
    peer_sync_states: HashMap<[u8; 32], Vec<u8>>,
    /// Moves on whenever `peer_sync_states` changes, so a persister writes
    /// them only then ([`Self::encode_peer_sync_states`]).
    peer_sync_generation: u64,
    /// Bytes of every sync message this namespace generated.
    #[cfg(test)]
    sync_bytes_generated: usize,
    #[cfg(test)]
    record_index_rebuilds: u64,
    /// Changes whenever the verified (signed) content projection changes:
    /// that state lives beside the document, so its heads alone miss it.
    /// Drawn from a process-wide counter so no two instances share a value.
    verified_generation: u64,
    /// Changes whenever changes another replica made are merged into the
    /// document. Concurrent edits, and so content conflicts, can only arrive
    /// that way: this replica's own changes are causally ordered. Drawn from
    /// the same process-wide counter as `verified_generation`.
    merge_generation: u64,
    /// Calls of [`crate::conflict::detect`] on this namespace.
    #[cfg(test)]
    conflict_scans: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    record_index_page_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    node_entry_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Node objects looked up in the document; see [`Self::node_lookup_count`].
    node_lookups: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Nodes whose verified projection changed beside the document since
    /// [`Self::kernel_changes_since`] last took them; `None` while nothing
    /// collects them ([`Self::collect_projected_changes`]).
    projected_changes: Option<BTreeSet<String>>,
}

/// Source of `Namespace::verified_generation` values.
static NEXT_VERIFIED_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn next_verified_generation() -> u64 {
    NEXT_VERIFIED_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Cheap fingerprint of everything a [`Namespace`] projects: the document
/// heads plus the generation of the verified content projection beside it.
/// Equal tokens mean no node, path, hash or content revision changed, so a
/// background reconciler can skip a pass without walking the namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceChangeToken {
    heads: Vec<automerge::ChangeHash>,
    verified_generation: u64,
}

/// The entries of a [`Namespace::encode_peer_sync_states`] encoding, or why
/// `bytes` is not exactly one.
fn decode_peer_sync_states(bytes: &[u8]) -> anyhow::Result<HashMap<[u8; 32], Vec<u8>>> {
    use anyhow::{Context, bail, ensure};
    let (body, checksum) = bytes
        .split_last_chunk::<32>()
        .context("truncated sync progress")?;
    ensure!(
        blake3::hash(body).as_bytes() == checksum,
        "damaged sync progress: checksum mismatch"
    );
    let rest = body
        .strip_prefix(PEER_SYNC_STATES_TAG)
        .context("not sync progress of this version")?;
    let (count, mut rest) = rest
        .split_first_chunk::<4>()
        .context("truncated sync progress")?;
    let count = usize::try_from(u32::from_le_bytes(*count))?;
    ensure!(
        count <= MAX_PEER_SYNC_STATES,
        "sync progress names {count} peers"
    );
    let mut entries = HashMap::with_capacity(count);
    for _ in 0..count {
        let (peer, after) = rest
            .split_first_chunk::<32>()
            .context("truncated sync progress")?;
        let (len, after) = after
            .split_first_chunk::<4>()
            .context("truncated sync progress")?;
        let len = usize::try_from(u32::from_le_bytes(*len))?;
        ensure!(
            len <= MAX_PEER_SYNC_STATE_BYTES,
            "a sync state of {len} bytes"
        );
        ensure!(after.len() >= len, "truncated sync progress");
        let (state, after) = after.split_at(len);
        automerge::sync::State::decode(state).context("undecodable sync state")?;
        if entries.insert(*peer, state.to_vec()).is_some() {
            bail!("sync progress names a peer twice");
        }
        rest = after;
    }
    ensure!(rest.is_empty(), "trailing bytes after sync progress");
    Ok(entries)
}

fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h.as_bytes())
}
fn hash_from_hex(s: &str) -> Option<Hash> {
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

/// The Automerge object key every device derives for one stable record.
fn record_node_id(record_id: FileRecordId) -> String {
    format!("{RECORD_NODE_ID_PREFIX}{}", record_id.0)
}

/// A bounded, valid sibling name for a record that lost a name to a smaller
/// stable record id. The record id makes the choice independent of arrival
/// order and collision-free without a local counter.
fn structural_conflict_name(name: &[u8], record_id: FileRecordId) -> FileNameV1 {
    let suffix = format!(
        "{}{}",
        String::from_utf8_lossy(STRUCTURAL_CONFLICT_MARKER),
        record_id.0.simple()
    );
    let suffix = suffix.as_bytes();
    let prefix_len = name
        .len()
        .min(MAX_FILE_NAME_BYTES.saturating_sub(suffix.len()));
    let mut out = name.get(..prefix_len).unwrap_or(name).to_vec();
    out.extend_from_slice(suffix);
    out.into()
}

fn read_name(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<FileNameV1> {
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
fn read_u64(doc: &AutoCommit, obj: &ObjId, key: &str) -> u64 {
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
    /// A namespace over `doc` whose `nodes` map is already in place, with
    /// only the root bound and every derived index empty.
    fn over(doc: AutoCommit, nodes: ObjId, author: String, next_id: u64) -> Self {
        let mut ns = Self {
            doc: ChangeTrackedDoc::new(doc),
            nodes,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author,
            next_id,
            content_authority: ContentAuthority::LegacyNamespace,
            verified_records: HashMap::new(),
            record_index: BTreeMap::new(),
            record_index_order: Vec::new(),
            record_index_revision: String::new(),
            record_index_by_node: HashMap::new(),
            record_index_children: HashMap::new(),
            record_index_ambiguous: HashSet::new(),
            record_index_digest: RecordIndexDigest::default(),
            tree: TreeIndex::default(),
            record_ids_complete: false,
            resolved_heads: None,
            incremental_batch: None,
            peer_sync_states: HashMap::new(),
            peer_sync_generation: 0,
            #[cfg(test)]
            sync_bytes_generated: 0,
            #[cfg(test)]
            record_index_rebuilds: 0,
            verified_generation: next_verified_generation(),
            merge_generation: next_verified_generation(),
            #[cfg(test)]
            conflict_scans: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            record_index_page_reads: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            node_entry_reads: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            node_lookups: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            projected_changes: None,
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        ns
    }

    /// # Errors
    ///
    /// Returns an error if the fresh document's root `nodes` map cannot be created.
    pub fn new(author: impl Into<String>) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::new();
        let nodes = doc.put_object(&ROOT, "nodes", ObjType::Map)?;
        let mut ns = Self::over(doc, nodes, author.into(), 0);
        let root_record_id = FileRecordId::new();
        let _ = ns
            .doc
            .put(&ROOT, ROOT_RECORD_ID_KEY, root_record_id.0.to_string());
        ns.rebuild_record_index();
        Ok(ns)
    }

    /// # Errors
    ///
    /// Returns an error if `data` is not a valid automerge document, or its root `nodes` map can't be created.
    pub fn from_bytes(
        author: impl Into<String>,
        data: &[u8],
    ) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let nodes = match doc.get(&ROOT, "nodes") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, "nodes", ObjType::Map)?,
        };
        let author = author.into();
        // Resume the id counter past ids we authored before, so a restart can't reuse a node id.
        let prefix = format!("{author}/");
        let mut max_local = 0u64;
        for id in doc.keys(&nodes) {
            if let Some(n) = id.strip_prefix(&prefix)
                && let Ok(v) = n.parse::<u64>()
            {
                max_local = max_local.max(v.saturating_add(1));
            }
        }
        let mut ns = Self::over(doc, nodes, author, max_local);
        ns.ensure_record_ids();
        ns.rebuild_record_index();
        Ok(ns)
    }

    /// Complete visible register operation IDs for a record and its ancestor
    /// bindings. IDs retain same-value/ABA changes and losing concurrent values.
    pub(crate) fn checked_record_frontier(
        &self,
        ino: u64,
    ) -> Option<Vec<(String, String, Vec<String>)>> {
        let mut id = self.ino_to_id.get(&ino)?.clone();
        let mut seen = std::collections::BTreeSet::new();
        let mut frontier = Vec::new();
        while id != ROOT_ID {
            if !seen.insert(id.clone()) {
                return None;
            }
            let node = self.node(&id)?;
            let mut keys = self.doc.keys(&node).collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let mut operations = self
                    .doc
                    .get_all(&node, &key)
                    .ok()?
                    .into_iter()
                    .map(|(_, operation)| operation.to_string())
                    .collect::<Vec<_>>();
                operations.sort();
                frontier.push((id.clone(), key, operations));
            }
            id = str_at(&self.doc, &node, "parent")?;
        }
        Some(frontier)
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    #[must_use]
    pub fn content_authority(&self) -> ContentAuthority {
        self.content_authority
    }

    /// Cut this local projection over to authenticated record state. Raw
    /// Automerge content fields continue to merge for structural compatibility
    /// but immediately cease to authorize reads or blob access.
    pub fn enable_signed_content_authority(&mut self) {
        if self.content_authority == ContentAuthority::SignedRecords {
            return;
        }
        self.content_authority = ContentAuthority::SignedRecords;
        self.verified_records.clear();
        self.verified_generation = next_verified_generation();
    }

    /// Install a content tuple that has already passed record-repository
    /// signature, certification, grant, version, and sequence validation.
    pub fn install_verified_content_revision(&mut self, revision: ContentRevisionV1) -> bool {
        if revision.validate().is_err() || self.content_authority != ContentAuthority::SignedRecords
        {
            return false;
        }
        if self
            .unique_live_node_for_record(revision.record_id)
            .is_none()
        {
            return false;
        }
        let thumbnail = self
            .verified_records
            .get(&revision.record_id)
            .and_then(|projection| projection.thumbnail);
        self.verified_records.insert(
            revision.record_id,
            VerifiedRecordProjection {
                revision,
                thumbnail,
            },
        );
        self.verified_generation = next_verified_generation();
        self.note_projected_change(revision.record_id);
        true
    }

    /// Update fetch-free preview metadata only after accepting its signed
    /// `SetMetadata` envelope and converged repository snapshot.
    pub fn install_verified_thumbnail(
        &mut self,
        record_id: FileRecordId,
        thumbnail: Option<Hash>,
    ) -> bool {
        if self.content_authority != ContentAuthority::SignedRecords {
            return false;
        }
        let Some(projection) = self.verified_records.get_mut(&record_id) else {
            return false;
        };
        projection.thumbnail = thumbnail;
        self.note_projected_change(record_id);
        true
    }

    pub fn save(&mut self) -> Vec<u8> {
        if !self.record_ids_complete {
            self.ensure_record_ids();
        }
        self.doc.save()
    }
    /// Changes since the last `save_incremental`/load/merge, advancing the shared
    /// automerge watermark.
    ///
    /// INVARIANT: pair every mutation with its `save_incremental` inside the same
    /// `Namespace` lock scope — the watermark is one shared cursor, so a mutation
    /// saved outside that scope can have its delta silently swallowed by a
    /// concurrent remote-apply.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        if !self.record_ids_complete {
            self.ensure_record_ids();
        }
        if self.incremental_batch.is_some() {
            return Vec::new();
        }
        self.doc.save_incremental()
    }

    /// Collect every change from now on into the one delta
    /// [`Self::end_incremental_batch`] returns; meanwhile
    /// [`Self::save_incremental`] returns nothing.
    pub fn begin_incremental_batch(&mut self) {
        if self.incremental_batch.is_none() {
            // Changes not yet taken lead the batch's delta.
            let pending = self.doc.save_incremental();
            self.incremental_batch = Some((self.doc.get_heads(), pending));
        }
    }

    /// The delta of every change since the batch began. A full save made
    /// meanwhile (which moves the incremental cursor) loses nothing.
    pub fn end_incremental_batch(&mut self) -> Vec<u8> {
        let Some((heads, mut delta)) = self.incremental_batch.take() else {
            return self.save_incremental();
        };
        delta.extend(self.doc.save_after(&heads));
        let _ = self.doc.save_incremental();
        delta
    }
    pub fn apply_incremental(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        let before = self.doc.get_heads();
        self.doc.load_incoming(data)?;
        self.note_merge(&before);
        self.ensure_record_ids();
        self.rebuild_record_index();
        Ok(())
    }
    /// Isolate a durable restore candidate from subsequent changes by the live
    /// namespace actor. This is projection identity, never certified authorship.
    pub(crate) fn begin_restore_projection(
        &mut self,
        workspace: uuid::Uuid,
        record: FileRecordId,
        intent: uuid::Uuid,
    ) -> automerge::ActorId {
        let original = self.doc.get_actor().clone();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.namespace.restore-projection.v1\0");
        hasher.update(workspace.as_bytes());
        hasher.update(record.0.as_bytes());
        hasher.update(intent.as_bytes());
        hasher.update(original.to_bytes());
        self.doc.set_actor(automerge::ActorId::from(
            hasher.finalize().as_bytes().as_slice(),
        ));
        original
    }

    pub(crate) fn end_restore_projection(&mut self, original: automerge::ActorId) {
        // save_incremental closes the candidate's pending transaction before
        // returning to the live actor; retries retain these exact saved bytes.
        let _ = self.doc.save_incremental();
        self.doc.set_actor(original);
    }

    pub fn prime_baseline(&mut self) {
        let _ = self.doc.save_incremental();
    }

    /// # Errors
    ///
    /// Returns an error if `other`'s document cannot be merged into this one.
    pub fn merge_doc(&mut self, other: &mut Namespace) -> Result<(), automerge::AutomergeError> {
        let before = self.doc.get_heads();
        self.doc.merge_incoming(&mut other.doc)?;
        self.note_merge(&before);
        self.ensure_record_ids();
        self.rebuild_record_index();
        Ok(())
    }

    /// Apply a remote delta: advance the watermark, resolve, and return the resolution delta to broadcast (empty if none).
    pub fn apply_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let before = self.doc.get_heads();
        self.doc.load_incoming(data)?;
        self.note_merge(&before);
        self.ensure_record_ids();
        let _ = self.doc.save_incremental();
        self.resolve();
        self.rebuild_record_index();
        Ok(self.doc.save_incremental())
    }

    /// # Errors
    ///
    /// Returns an error if `data` is not a valid automerge document, or cannot be merged into this one.
    pub fn merge_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let mut other = AutoCommit::load(data)?;
        let before = self.doc.get_heads();
        self.doc.merge_incoming(&mut other)?;
        self.note_merge(&before);
        self.ensure_record_ids();
        let _ = self.doc.save_incremental();
        self.resolve();
        self.rebuild_record_index();
        Ok(self.doc.save_incremental())
    }

    /// Moves [`Self::merge_generation`] on when a merge since `before`
    /// brought changes.
    fn note_merge(&mut self, before: &[automerge::ChangeHash]) {
        if self.doc.get_heads() != before {
            self.merge_generation = next_verified_generation();
        }
    }

    /// Changes whenever changes another replica made were merged in, and
    /// only then: equal values mean no new content conflict can have
    /// arrived, so a reconciler can skip [`crate::conflict::detect`].
    #[must_use]
    pub fn merge_generation(&self) -> u64 {
        self.merge_generation
    }

    /// Counts a [`crate::conflict::detect`] scan.
    #[cfg(test)]
    pub(crate) fn note_conflict_scan(&self) {
        self.conflict_scans
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The sync state an exchange with the device `peer` begins at: where
    /// the last exchange completed with it ([`Self::finish_sync_with`])
    /// left off, so an idle exchange summarises only the changes since
    /// rather than the document's whole history. A device this namespace
    /// has completed no exchange with, or whose agreed heads this document
    /// does not hold, gets a fresh state and so a full sync. A peer that
    /// lost what it agreed to is caught by Automerge itself: it answers
    /// with a reset, and the exchange falls back to a full sync.
    pub fn begin_sync_with(&mut self, peer: &[u8; 32]) -> automerge::sync::State {
        let resumed = self
            .peer_sync_states
            .get(peer)
            .and_then(|encoded| automerge::sync::State::decode(encoded).ok());
        match resumed {
            Some(state)
                if state
                    .shared_heads
                    .iter()
                    .all(|hash| self.doc.has_change(hash)) =>
            {
                state
            }
            Some(_) => {
                self.peer_sync_states.remove(peer);
                automerge::sync::State::new()
            }
            None => automerge::sync::State::new(),
        }
    }

    /// Remembers where a completed exchange with the device `peer` left
    /// off, for [`Self::begin_sync_with`]. Only the heads both sides hold
    /// persist ([`automerge::sync::State::encode`]); everything else in
    /// `state` belongs to that one exchange.
    pub fn finish_sync_with(&mut self, peer: [u8; 32], state: &automerge::sync::State) {
        if !self.peer_sync_states.contains_key(&peer)
            && self.peer_sync_states.len() >= MAX_PEER_SYNC_STATES
            && let Some(evicted) = self.peer_sync_states.keys().next().copied()
        {
            self.peer_sync_states.remove(&evicted);
        }
        let encoded = state.encode();
        if self.peer_sync_states.get(&peer) != Some(&encoded) {
            self.peer_sync_states.insert(peer, encoded);
            self.peer_sync_generation = self.peer_sync_generation.wrapping_add(1);
            self.doc.announce();
        }
    }

    /// Whether this namespace has completed a sync exchange with any other
    /// device (restored progress included): someone has dialled it, or been
    /// dialled, before (#4224, [`crate::reachability`]).
    #[must_use]
    pub fn has_synced_with_peers(&self) -> bool {
        !self.peer_sync_states.is_empty()
    }

    /// Whether the last exchange completed with the device `peer` left both
    /// sides holding exactly this document's current heads: nothing here is
    /// new to that peer since. Says nothing about what the peer has that
    /// this document lacks.
    pub fn in_sync_with(&mut self, peer: &[u8; 32]) -> bool {
        let Some(state) = self
            .peer_sync_states
            .get(peer)
            .and_then(|encoded| automerge::sync::State::decode(encoded).ok())
        else {
            return false;
        };
        let mut agreed = state.shared_heads;
        let mut heads = self.doc.get_heads();
        agreed.sort_unstable();
        heads.sort_unstable();
        agreed == heads
    }

    /// Moves on whenever the remembered sync progress changes.
    #[must_use]
    pub const fn peer_sync_generation(&self) -> u64 {
        self.peer_sync_generation
    }

    /// The sync progress with every peer device, to persist beside the
    /// document so a restart resumes rather than starting each exchange
    /// over ([`Self::restore_peer_sync_states`]).
    #[must_use]
    pub fn encode_peer_sync_states(&self) -> Vec<u8> {
        let mut entries: Vec<_> = self.peer_sync_states.iter().collect();
        entries.sort_unstable_by_key(|(peer, _)| **peer);
        let mut out = Vec::new();
        out.extend_from_slice(PEER_SYNC_STATES_TAG);
        out.extend_from_slice(
            &u32::try_from(entries.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for (peer, state) in entries {
            out.extend_from_slice(peer);
            out.extend_from_slice(&u32::try_from(state.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(state);
        }
        let checksum = blake3::hash(&out);
        out.extend_from_slice(checksum.as_bytes());
        out
    }

    /// Takes back sync progress [`Self::encode_peer_sync_states`] wrote,
    /// replacing any held, and returns how many peers it resumes with.
    /// Persisted progress is a hint, never trusted: bytes that are not
    /// exactly such an encoding are refused whole and nothing changes, and
    /// a peer whose agreed heads this document does not hold (a document
    /// restored from an older copy, or another workspace's) is dropped, so
    /// its next exchange is a full sync. A peer that lost what it agreed to
    /// is caught by Automerge itself, as in [`Self::begin_sync_with`].
    pub fn restore_peer_sync_states(&mut self, bytes: &[u8]) -> anyhow::Result<usize> {
        let decoded = decode_peer_sync_states(bytes)?;
        self.peer_sync_states = decoded
            .into_iter()
            .filter(|(_, encoded)| {
                automerge::sync::State::decode(encoded).is_ok_and(|state| {
                    state
                        .shared_heads
                        .iter()
                        .all(|hash| self.doc.has_change(hash))
                })
            })
            .collect();
        self.peer_sync_generation = self.peer_sync_generation.wrapping_add(1);
        self.doc.announce();
        Ok(self.peer_sync_states.len())
    }

    pub fn sync_generate(&mut self, state: &mut automerge::sync::State) -> Option<Vec<u8>> {
        let message = self
            .doc
            .generate_sync_message(state)
            .map(automerge::sync::Message::encode);
        #[cfg(test)]
        {
            self.sync_bytes_generated = self
                .sync_bytes_generated
                .saturating_add(message.as_ref().map_or(0, Vec::len));
        }
        message
    }

    /// Bytes of every sync message this namespace has generated.
    #[cfg(test)]
    pub(crate) fn sync_bytes_generated(&self) -> usize {
        self.sync_bytes_generated
    }

    pub fn sync_receive(
        &mut self,
        state: &mut automerge::sync::State,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let msg = automerge::sync::Message::decode(data)?;
        // Most sync rounds carry heads and bloom filters, not changes; the
        // indexes are rebuilt, and the change announced, only when the
        // document actually changed.
        if self.doc.receive_sync_message(state, msg)? {
            self.merge_generation = next_verified_generation();
            self.ensure_record_ids();
            self.rebuild_record_index();
        }
        Ok(())
    }

    /// Discard the pulled changes from the watermark (peers already have them), resolve, and return only the resolution delta.
    pub fn post_sync_resolve(&mut self) -> Vec<u8> {
        let _ = self.doc.save_incremental();
        let heads = self.doc.get_heads();
        if self.resolved_heads.as_ref() == Some(&heads) {
            // Resolution depends only on the document, which has not
            // changed since it last ran.
            return Vec::new();
        }
        self.resolve();
        self.rebuild_record_index();
        let delta = self.doc.save_incremental();
        self.resolved_heads = Some(self.doc.get_heads());
        delta
    }

    fn node(&self, id: &str) -> Option<ObjId> {
        self.node_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        map_at(&self.doc, &self.nodes, id)
    }

    /// Node objects looked up in the document so far: the unit of work of
    /// every metadata read, so a test can bound what a listing costs.
    #[must_use]
    pub fn node_lookup_count(&self) -> u64 {
        self.node_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The structural index of every node, read from the document.
    fn scan_tree_index(&self) -> TreeIndex {
        let mut tree = TreeIndex::default();
        for id in self.doc.keys(&self.nodes) {
            let Some(node) = self.node(&id) else {
                continue;
            };
            if let Some(parent) = str_at(&self.doc, &node, "parent") {
                let name = read_name(&self.doc, &node, "name").unwrap_or_default();
                tree.place(&id, parent, name);
            }
            if let Some(record_id) = str_at(&self.doc, &node, RECORD_ID_KEY)
                .and_then(|value| Uuid::parse_str(&value).ok())
            {
                tree.bind_record(&id, FileRecordId(record_id));
            }
        }
        tree
    }

    /// Reconstruct the index once while loading or after an opaque remote
    /// Automerge merge.  Serving a record-index page never calls this: page
    /// work is a slice of `record_index`, not a document traversal.
    fn rebuild_record_index(&mut self) {
        #[derive(Clone)]
        struct Raw {
            record_id: Option<FileRecordId>,
            parent_id: String,
            name: FileNameV1,
            is_dir: bool,
        }

        let mut raw = HashMap::<String, Raw>::new();
        let mut occurrences = HashMap::<FileRecordId, usize>::new();
        self.tree = self.scan_tree_index();
        for id in self.doc.keys(&self.nodes) {
            let Some(node) = self.node(&id) else {
                continue;
            };
            let record_id = self.record_id_for_id(&id);
            if let Some(record_id) = record_id {
                let count = occurrences
                    .get(&record_id)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(1);
                occurrences.insert(record_id, count);
            }
            let is_dir = self.kind_of(&id) == NodeKind::Dir;
            raw.insert(
                id,
                Raw {
                    record_id,
                    parent_id: str_at(&self.doc, &node, "parent").unwrap_or_default(),
                    name: read_name(&self.doc, &node, "name").unwrap_or_default(),
                    is_dir,
                },
            );
        }
        let mut children = HashMap::<String, BTreeSet<String>>::new();
        for (id, item) in &raw {
            children
                .entry(item.parent_id.clone())
                .or_default()
                .insert(id.clone());
        }
        let mut record_index = BTreeMap::new();
        let mut by_node = HashMap::new();
        let mut queue = VecDeque::from([(ROOT_ID.to_string(), String::from("/"))]);
        let mut visited = HashSet::from([ROOT_ID.to_string()]);
        while let Some((parent, parent_path)) = queue.pop_front() {
            for id in children.get(&parent).into_iter().flatten() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                let Some(item) = raw.get(id) else { continue };
                let path = if parent_path == "/" {
                    format!("/{}", escape_path_name(item.name.as_bytes()))
                } else {
                    format!("{parent_path}/{}", escape_path_name(item.name.as_bytes()))
                };
                // Traverse all structurally reachable nodes, even a legacy
                // duplicate-ID node.  Only the unique stable record itself is
                // eligible for the public index.
                queue.push_back((id.clone(), path.clone()));
                let Some(record_id) = item.record_id else {
                    continue;
                };
                if occurrences.get(&record_id) != Some(&1) {
                    continue;
                }
                by_node.insert(id.clone(), record_id);
                record_index.insert(
                    record_id,
                    IndexedRecord {
                        record_id,
                        node_id: id.clone(),
                        parent_id: item.parent_id.clone(),
                        path,
                        is_dir: item.is_dir,
                    },
                );
            }
        }
        self.record_index_digest = RecordIndexDigest::default();
        for record in record_index.values() {
            self.record_index_digest.add(record);
        }
        self.record_index = record_index;
        self.record_index_order = self.record_index.keys().copied().collect();
        self.record_index_by_node = by_node;
        self.record_index_children = children;
        self.record_index_ambiguous = occurrences
            .into_iter()
            .filter_map(|(record_id, count)| (count > 1).then_some(record_id))
            .collect();
        self.update_record_index_revision();
        #[cfg(test)]
        {
            self.record_index_rebuilds = self.record_index_rebuilds.saturating_add(1);
        }
    }

    /// Snapshot one bounded canonical page.  The returned rows are ordered by
    /// stable record UUID and clone at most `limit` entries.
    pub fn indexed_record_page(&self, offset: usize, limit: usize) -> Vec<IndexedRecord> {
        #[cfg(test)]
        self.record_index_page_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let end = offset
            .saturating_add(limit)
            .min(self.record_index_order.len());
        self.record_index_order
            .get(offset..end)
            .unwrap_or_default()
            .iter()
            .filter_map(|record_id| self.record_index.get(record_id))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn indexed_record_count(&self) -> usize {
        self.record_index_order.len()
    }

    #[must_use]
    pub fn indexed_record(&self, record_id: FileRecordId) -> Option<IndexedRecord> {
        self.record_index.get(&record_id).cloned()
    }

    /// Opaque generation captured when the index changed. Reading it is O(1):
    /// manifest/page serving must never enumerate an adversarially large set
    /// of Automerge heads merely to validate a cursor.
    #[must_use]
    pub fn indexed_record_revision(&self) -> &str {
        &self.record_index_revision
    }

    /// O(1): derived from the row count and the incrementally maintained
    /// row digest, so a create does not rehash the whole catalogue.
    fn update_record_index_revision(&mut self) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.namespace-record-index.v2\0");
        hasher.update(&(self.record_index_order.len() as u64).to_be_bytes());
        for limb in self.record_index_digest.0 {
            hasher.update(&limb.to_le_bytes());
        }
        self.record_index_revision = hasher.finalize().to_hex().to_string();
    }

    /// Incrementally admit a freshly created node. Creation is the hot path
    /// for imports, so it must not turn N creates into N full document walks.
    /// More complex remote/reparent operations deliberately use the one-shot
    /// reconciler above, outside xattr serving.
    fn index_insert_node(&mut self, id: &str) {
        let Some(node) = self.node(id) else { return };
        let Some(record_id) = self.record_id_for_id(id) else {
            return;
        };
        if self.record_index_ambiguous.contains(&record_id) {
            return;
        }
        if let Some(previous) = self.record_index.remove(&record_id) {
            // A duplicate durable identity is withheld until a full
            // reconciliation can represent the ambiguity fail-closed.
            self.record_index_digest.remove(&previous);
            self.record_index_by_node.remove(&previous.node_id);
            if let Some(children) = self.record_index_children.get_mut(&previous.parent_id) {
                children.remove(&previous.node_id);
            }
            if let Ok(position) = self.record_index_order.binary_search(&record_id) {
                self.record_index_order.remove(position);
            }
            self.record_index_ambiguous.insert(record_id);
            self.update_record_index_revision();
            return;
        }
        let parent_id = str_at(&self.doc, &node, "parent").unwrap_or_default();
        let parent_path = if parent_id == ROOT_ID {
            Some(String::from("/"))
        } else {
            self.record_index_by_node
                .get(&parent_id)
                .and_then(|record_id| self.record_index.get(record_id))
                .map(|parent| parent.path.clone())
        };
        let Some(parent_path) = parent_path else {
            return;
        };
        let name = read_name(&self.doc, &node, "name").unwrap_or_default();
        let path = if parent_path == "/" {
            format!("/{}", escape_path_name(name.as_bytes()))
        } else {
            format!("{parent_path}/{}", escape_path_name(name.as_bytes()))
        };
        self.record_index_children
            .entry(parent_id.clone())
            .or_default()
            .insert(id.to_string());
        self.record_index_by_node.insert(id.to_string(), record_id);
        let record = IndexedRecord {
            record_id,
            node_id: id.to_string(),
            parent_id,
            path,
            is_dir: self.kind_of(id) == NodeKind::Dir,
        };
        self.record_index_digest.add(&record);
        self.record_index.insert(record_id, record);
        let position = self
            .record_index_order
            .binary_search(&record_id)
            .unwrap_or_else(|position| position);
        self.record_index_order.insert(position, record_id);
        self.update_record_index_revision();
    }

    #[cfg(test)]
    #[must_use]
    pub fn record_index_rebuild_count(&self) -> u64 {
        self.record_index_rebuilds
    }

    #[cfg(test)]
    #[must_use]
    pub fn record_index_page_read_count(&self) -> u64 {
        self.record_index_page_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Calls of [`Self::entry_for_node`]: a whole-namespace walk makes one
    /// per live node, so a test can prove a pass did not walk.
    #[cfg(test)]
    #[must_use]
    pub fn node_entry_read_count(&self) -> u64 {
        self.node_entry_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn deterministic_record_id(&self, id: &str) -> FileRecordId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(RECORD_ID_MIGRATION_DOMAIN);
        // The nodes-map object id is minted once with the namespace and remains
        // stable across save/load and replica merges. Including it prevents
        // legacy ids such as `alice/0` from colliding across workspaces.
        hasher.update(self.nodes.to_string().as_bytes());
        hasher.update(&[0]);
        hasher.update(id.as_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        // Mark deterministic migrations as RFC 4122, version-5-shaped UUIDs.
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        FileRecordId(Uuid::from_bytes(bytes))
    }

    /// Populate snapshots written before record-scoped replication. Peers
    /// migrating the same legacy namespace independently derive the same ids.
    fn ensure_record_ids(&mut self) {
        if str_at(&self.doc, &ROOT, ROOT_RECORD_ID_KEY)
            .and_then(|value| Uuid::parse_str(&value).ok())
            .is_none()
        {
            let record_id = self.deterministic_record_id(ROOT_ID);
            let _ = self
                .doc
                .put(&ROOT, ROOT_RECORD_ID_KEY, record_id.0.to_string());
        }

        let ids: Vec<_> = self.doc.keys(&self.nodes).collect();
        for id in ids {
            let Some(node) = self.node(&id) else {
                continue;
            };
            if str_at(&self.doc, &node, RECORD_ID_KEY)
                .and_then(|value| Uuid::parse_str(&value).ok())
                .is_some()
            {
                continue;
            }
            let record_id = self.deterministic_record_id(&id);
            let _ = self.doc.put(&node, RECORD_ID_KEY, record_id.0.to_string());
            self.tree.bind_record(&id, record_id);
        }
        self.record_ids_complete = true;
    }

    fn parent_of(&self, id: &str) -> Option<String> {
        str_at(&self.doc, &self.node(id)?, "parent")
    }
    fn name_of(&self, id: &str) -> Option<FileNameV1> {
        read_name(&self.doc, &self.node(id)?, "name")
    }
    fn unique_node_id_for_record(&self, record_id: FileRecordId) -> Option<String> {
        if self.record_id_for_id(ROOT_ID) == Some(record_id) {
            return Some(ROOT_ID.to_string());
        }
        let mut matches = self
            .doc
            .keys(&self.nodes)
            .filter(|id| self.record_id_for_id(id) == Some(record_id));
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }
    /// The kind of the live node `ino`: what [`Self::getattr`] tells of it,
    /// without reading the node's other fields.
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

    /// Bring one node's entry in the structural index in line with the
    /// document after its parent or name changed.
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

    /// Walk the parent chain: `true` if it reaches ROOT (visible), `false` if it
    /// hits TRASH, a missing node, or a cycle.
    /// Whether `id`'s parent chain reaches the root. Walks the structural
    /// index, which mirrors every node's document parent, instead of
    /// reading the document once per hop.
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
            record_id: meta.record_id,
            parent: str_at(&self.doc, &obj, "parent").unwrap_or_default(),
            name: read_name(&self.doc, &obj, "name").unwrap_or_default(),
            kind: meta.kind,
            hash: meta.hash,
            size: meta.size,
            mtime: meta.mtime,
            author: str_at(&self.doc, &obj, "author").unwrap_or_default(),
        })
    }

    /// The fields of [`Self::entry`] a FUSE request needs, read from the
    /// node once: no parent, name or author strings, and the record id and
    /// content revision resolved without looking the node up again.
    fn node_meta(&self, id: &str) -> Option<NodeMeta> {
        self.node_meta_at(id, &self.node(id)?)
    }

    /// [`Self::node_meta`] of the node object `obj` for `id`. A node with no
    /// record id has no entry, exactly as [`Self::entry`] requires.
    fn node_meta_at(&self, id: &str, obj: &ObjId) -> Option<NodeMeta> {
        let record_id = if id == ROOT_ID {
            self.record_id_for_id(id)?
        } else {
            FileRecordId(Uuid::parse_str(&str_at(&self.doc, obj, RECORD_ID_KEY)?).ok()?)
        };
        let revision = self.content_revision_of(obj, record_id);
        Some(NodeMeta {
            record_id,
            kind: NodeKind::from_str(&str_at(&self.doc, obj, "kind").unwrap_or_default()),
            hash: match (self.content_authority, revision) {
                (_, Some(revision)) => revision.content_hash.map(|hash| Hash::from(hash.0)),
                (ContentAuthority::SignedRecords, None) => None,
                (ContentAuthority::LegacyNamespace, None) => {
                    str_at(&self.doc, obj, "hash").and_then(|h| hash_from_hex(&h))
                }
            },
            size: match (self.content_authority, revision) {
                (_, Some(revision)) => revision.byte_len,
                (ContentAuthority::SignedRecords, None) => 0,
                (ContentAuthority::LegacyNamespace, None) => read_u64(&self.doc, obj, "size"),
            },
            mtime: read_u64(&self.doc, obj, "mtime"),
        })
    }

    fn children(&self, parent_id: &str) -> Vec<String> {
        self.tree.children(parent_id).cloned().collect()
    }

    /// The live child of `parent_id` named `name` — smallest id wins on a tie.
    fn child_named(&self, parent_id: &str, name: impl AsRef<[u8]>) -> Option<String> {
        // The index yields same-named nodes in id order.
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

    /// Return the exact byte-preserving basename used for signed mutations.
    pub fn file_name_for(&self, ino: u64) -> Option<FileNameV1> {
        self.id_for_ino(ino).and_then(|id| self.name_of(id))
    }

    /// Stable collaborative record id for a local inode.
    pub fn record_id_for(&self, ino: u64) -> Option<FileRecordId> {
        self.record_id_for_id(self.id_for_ino(ino)?)
    }

    /// Stable collaborative record id for a namespace node id.
    pub fn record_id_for_id(&self, id: &str) -> Option<FileRecordId> {
        let value = if id == ROOT_ID {
            str_at(&self.doc, &ROOT, ROOT_RECORD_ID_KEY)?
        } else {
            str_at(&self.doc, &self.node(id)?, RECORD_ID_KEY)?
        };
        Uuid::parse_str(&value).ok().map(FileRecordId)
    }

    /// Describe the currently visible namespace node as a signed-binding
    /// payload. The caller supplies the certified actor and observation time
    /// that will be covered by the record envelope signature.
    #[must_use]
    pub fn signed_binding_for(
        &self,
        ino: u64,
        actor: FileActor,
        observed_at_ms: u64,
    ) -> Option<FileBindingV1> {
        let record_id = self.record_id_for(ino)?;
        if ino == ROOT_INO {
            return Some(FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
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
        let parent = self.record_id_for_id(&parent_id)?;
        let kind = match self.kind_of(id) {
            NodeKind::File => FileNodeKind::File,
            NodeKind::Dir => FileNodeKind::Directory,
        };
        Some(FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            record_id,
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
    /// The latest converged binding is the authority for this record's
    /// existence and placement:
    ///
    /// - an unknown record with a non-tombstone binding is created at its
    ///   signed parent/name (the parent must already be a unique live
    ///   directory, so a child whose parent has not arrived yet is deferred);
    /// - a live record moves, or becomes visible again after a concurrent
    ///   delete when its newer binding wins the replica's deterministic
    ///   binding order;
    /// - a tombstone trashes a live record and is a no-op for one never seen;
    /// - records that want one name are assigned it in stable-record-id order,
    ///   the others keep a deterministic `.sync-conflict-<record>` sibling.
    ///
    /// Unsigned nodes are never renamed or trusted: they always keep their
    /// name, and a signed record that collides with one is the one that moves.
    /// Repeated application is idempotent.
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
        let parent_record = binding
            .parent
            .ok_or(BindingProjectionError::UnknownParent)?;
        let expected_kind = match binding.kind {
            FileNodeKind::File => NodeKind::File,
            FileNodeKind::Directory => NodeKind::Dir,
        };
        let parent_id = self.resolve_signed_parent(parent_record)?;
        let existing = self.signed_record_node(binding.record_id)?;
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
                // The settled current name may be a conflict sibling; the
                // signed placement intent, not the visible name, decides
                // whether this binding is already applied.
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
            // A newer binding than the tombstone resurrects the record. The
            // replica selects bindings by their signed stamp, so this is the
            // same deterministic delete/move conflict decision on every peer.
            self.set_signed_placement(&id, &parent_id, &binding.name, binding.observed_at_ms);
            self.settle_structural_conflicts(&parent_id)?;
            return Ok(true);
        }
        if binding.tombstoned {
            // A delete for a record this device never held changes no path.
            // Keeping it out of the tree is the whole effect.
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
                binding.record_id,
            )
            .ok_or(BindingProjectionError::InvalidMove)?;
        self.settle_structural_conflicts(&parent_id)?;
        let _ = id;
        Ok(true)
    }

    /// Admit a binding only to create a missing node, never to move,
    /// resurrect or tombstone an existing one. Transaction participants use
    /// this to make their pre-transaction nodes available to the atomic
    /// transaction projector without letting a stale ordinary binding undo
    /// the transaction's result on replay.
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
        if self.signed_record_node(binding.record_id)?.is_some() || binding.tombstoned {
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
            binding.record_id,
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
        match self.record_id_for(ROOT_INO) {
            Some(current) if current == binding.record_id => return Ok(false),
            Some(_) if !self.live_node_ids().is_empty() => {
                return Err(BindingProjectionError::RootIdentityConflict);
            }
            _ => {}
        }
        self.doc
            .put(&ROOT, ROOT_RECORD_ID_KEY, binding.record_id.0.to_string())
            .map_err(|_| BindingProjectionError::InvalidMove)?;
        Ok(true)
    }

    /// The unique live directory a signed binding may name as its parent.
    fn resolve_signed_parent(
        &self,
        parent_record: FileRecordId,
    ) -> Result<String, BindingProjectionError> {
        if self.record_id_for(ROOT_INO) == Some(parent_record) {
            return Ok(ROOT_ID.to_string());
        }
        let id = self
            .unique_live_node_for_record(parent_record)
            .ok_or(BindingProjectionError::UnknownParent)?;
        if self.kind_of(&id) != NodeKind::Dir {
            return Err(BindingProjectionError::UnknownParent);
        }
        Ok(id)
    }

    /// The one node carrying a stable record id, whether currently live or
    /// trashed. Duplicate identities stay fail-closed.
    fn signed_record_node(
        &self,
        record_id: FileRecordId,
    ) -> Result<Option<String>, BindingProjectionError> {
        let mut matches = self
            .tree
            .nodes_of(record_id)
            .filter(|id| self.record_id_for_id(id) == Some(record_id));
        let Some(id) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(BindingProjectionError::UnknownOrDuplicateRecord);
        }
        Ok(Some(id.clone()))
    }

    fn placement_name_of(&self, id: &str) -> Option<FileNameV1> {
        self.node(id)
            .and_then(|node| read_name(&self.doc, &node, PLACEMENT_NAME_KEY))
            .or_else(|| self.name_of(id))
    }

    /// Record the signed placement intent and provisionally move the node
    /// there. [`Self::settle_structural_conflicts`] then assigns the names of
    /// every sibling that wants the same one.
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
    }

    /// Assign the names of all live children of `parent_id` that ask for one
    /// name. The smallest stable record id keeps the requested name; the
    /// others get a deterministic conflict sibling. This is a fixed point over
    /// the signed placement names, so every peer that holds the same records
    /// computes the same tree regardless of the order they arrived in.
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
                    let Some(record_id) = self.record_id_for_id(&loser) else {
                        // An unsigned node is not this authority's to rename;
                        // leave it where it is and never move a signed record
                        // onto or over it silently.
                        continue;
                    };
                    next.insert(loser, structural_conflict_name(name.as_bytes(), record_id));
                    moved = true;
                }
            }
            if !moved {
                let mut changed = false;
                for (id, name) in current {
                    if self.name_of(&id).as_ref() != Some(&name) {
                        self.set_field(&id, "name", name.as_bytes().to_vec());
                        changed = true;
                    }
                }
                if changed {
                    self.rebuild_record_index();
                }
                return Ok(changed);
            }
            current = next;
        }
        Err(BindingProjectionError::StructuralConflictLoop)
    }

    /// Total order for a contested name: nodes without a stable record id
    /// keep precedence (they are never renamed), then the stable record id,
    /// then the node id for an impossible tie.
    fn structural_owner_key(&self, id: &str) -> (u8, FileRecordId, String) {
        match self.record_id_for_id(id) {
            Some(record_id) => (1, record_id, id.to_string()),
            None => (0, FileRecordId(Uuid::nil()), id.to_string()),
        }
    }

    /// Apply one fully committed replacement-rename transaction while the
    /// namespace lock is held. All preconditions are checked before mutation;
    /// the returned delta contains both the victim tombstone and source move.
    pub fn apply_replacement_transaction(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        // Automerge transactions make the document edits atomic, but the
        // verified-record projection is maintained alongside the document.
        // Apply to a complete clone so a late projection failure cannot leave
        // either authority half-mutated in the live namespace.
        let mut candidate = self.clone();
        let delta = candidate.apply_replacement_transaction_in_place(transaction)?;
        if delta.is_some() {
            candidate.rebuild_record_index();
            *self = candidate;
        }
        Ok(delta)
    }

    fn apply_atomic_save_in_place(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::AtomicSave {
            parent_record,
            document_name,
            temporary_name,
            document_record,
            temporary_record,
            prior_document_version,
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let document = transaction
            .manifest
            .participants
            .iter()
            .find(|p| p.role == FileMutationParticipantRole::AtomicSaveDocument)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let temporary = transaction
            .manifest
            .participants
            .iter()
            .find(|p| p.role == FileMutationParticipantRole::AtomicSaveTemporary)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let document_id = self
            .unique_node_id_for_record(*document_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let temporary_id = self
            .unique_node_id_for_record(*temporary_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let revision = document
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let document_ino = self.ino_for_id(&document_id);
        if self.reaches_root(&document_id)
            && !self.reaches_root(&temporary_id)
            && self.content_revision_for(document_ino) == Some(revision)
        {
            return Ok(None);
        }
        for (id, name, expected) in [
            (&document_id, document_name, *prior_document_version),
            (&temporary_id, temporary_name, temporary.expected_version),
        ] {
            if !self.reaches_root(id)
                || self.kind_of(id) != NodeKind::File
                || self
                    .parent_of(id)
                    .and_then(|parent| self.record_id_for_id(&parent))
                    != Some(*parent_record)
                || self.name_of(id).as_ref() != Some(name)
                || self
                    .content_revision_for_id(id)
                    .is_none_or(|revision| revision.version_id != expected)
            {
                return Err(TransactionProjectionError::PreconditionFailed);
            }
        }
        self.apply_signed_binding(&document.proposed_binding)
            .map_err(|_| TransactionProjectionError::PreconditionFailed)?;
        if !self.set_content_revision_preserving_preview(
            document_ino,
            &revision,
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        self.apply_signed_binding(&temporary.proposed_binding)
            .map_err(|_| TransactionProjectionError::PreconditionFailed)?;
        Ok(Some(self.save_incremental()))
    }

    /// Apply either supported signed multi-record mutation to a complete
    /// namespace clone, publishing the clone only after every precondition and
    /// both record projections succeed.
    pub fn apply_mutation_transaction(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        match transaction.manifest.operation {
            fs_protocol::FileMutationOperation::RestoreContent { prior_revision, .. } => {
                transaction
                    .verify()
                    .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
                let participant = transaction
                    .manifest
                    .participants
                    .first()
                    .ok_or(TransactionProjectionError::InvalidTransaction)?;
                let revision = participant
                    .proposed_content_revision
                    .ok_or(TransactionProjectionError::InvalidTransaction)?;
                let ino = self
                    .ensure_ino_for_record_id(participant.record_id)
                    .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
                let current = self.content_revision_for(ino);
                if current == Some(revision) {
                    return Ok(None);
                }
                if current != Some(prior_revision) {
                    return Err(TransactionProjectionError::PreconditionFailed);
                }
                if !self.set_content_revision(ino, &revision, transaction.manifest.timestamp_ms) {
                    return Err(TransactionProjectionError::ContentProjectionFailed);
                }
                Ok(Some(self.save_incremental()))
            }
            fs_protocol::FileMutationOperation::AtomicSave { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_atomic_save_in_place(transaction)?;
                if delta.is_some() {
                    candidate.rebuild_record_index();
                    *self = candidate;
                }
                Ok(delta)
            }
            fs_protocol::FileMutationOperation::ReplacementRename { .. } => {
                self.apply_replacement_transaction(transaction)
            }
            fs_protocol::FileMutationOperation::KeepBothContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_keep_both_transaction_in_place(transaction)?;
                if delta.is_some() {
                    candidate.rebuild_record_index();
                    *self = candidate;
                }
                Ok(delta)
            }
            fs_protocol::FileMutationOperation::MergeContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_merge_transaction_in_place(transaction)?;
                if delta.is_some() {
                    candidate.rebuild_record_index();
                    *self = candidate;
                }
                Ok(delta)
            }
        }
    }

    /// Install a durably staged merge delta and its signed content revision.
    pub fn apply_staged_merge_delta(
        &mut self,
        transaction: &FileMutationTransaction,
        delta: &[u8],
    ) -> Result<(), TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::MergeContentConflict {
            source_record,
            parent_record,
            source_name,
            ..
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = transaction
            .manifest
            .participants
            .first()
            .filter(|participant| participant.role == FileMutationParticipantRole::MergeSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let mut candidate = self.clone();
        let source_id = candidate
            .unique_node_id_for_record(*source_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let source_ino = candidate.ino_for_id(&source_id);
        let parent_id = candidate
            .unique_node_id_for_record(*parent_record)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        if candidate.parent_of(&source_id) != Some(parent_id)
            || candidate.name_of(&source_id).as_deref() != Some(source_name.as_bytes())
            || !matches!(candidate.content_revision_for(source_ino), Some(revision)
                if revision.version_id == source.expected_version
                    || Some(revision) == source.proposed_content_revision)
        {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        if !delta.is_empty() {
            candidate
                .apply_incremental(delta)
                .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        }
        let revision = source
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        if !candidate.set_content_revision_preserving_preview(
            source_ino,
            &revision,
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        *self = candidate;
        Ok(())
    }

    fn apply_merge_transaction_in_place(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::MergeContentConflict {
            source_record,
            parent_record,
            source_name,
            ..
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = transaction
            .manifest
            .participants
            .first()
            .filter(|participant| participant.role == FileMutationParticipantRole::MergeSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_record(*source_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let ino = self.ino_for_id(&source_id);
        let parent_id = self
            .unique_node_id_for_record(*parent_record)
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
        if !self.set_content_revision_preserving_preview(
            ino,
            &revision,
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        Ok(Some(self.save_incremental()))
    }

    /// Install the exact namespace delta durably staged for a Keep Both
    /// transaction, then project both signed content revisions. Automerge node
    /// ids are actor-local, so independently replaying the semantic mutation
    /// can produce different (but equivalent) bytes; recovery must instead
    /// apply the journaled bytes that peers will receive.
    pub fn apply_staged_keep_both_delta(
        &mut self,
        transaction: &FileMutationTransaction,
        delta: &[u8],
    ) -> Result<(), TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::KeepBothContentConflict {
            source_record,
            sibling_record,
            parent_record,
            source_name,
            sibling_name,
            ..
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRole::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRole::KeepBothSibling)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let mut candidate = self.clone();
        let source_id = candidate
            .unique_node_id_for_record(*source_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let source_ino = candidate.ino_for_id(&source_id);
        let parent_id = candidate
            .unique_node_id_for_record(*parent_record)
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
            .unique_node_id_for_record(*sibling_record)
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
            transaction.manifest.timestamp_ms,
        ) || !candidate.set_content_revision_preserving_preview(
            sibling_ino,
            &sibling_revision,
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        *self = candidate;
        Ok(())
    }

    fn apply_keep_both_transaction_in_place(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::KeepBothContentConflict {
            source_record,
            sibling_record,
            parent_record,
            source_name,
            sibling_name,
            ..
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRole::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRole::KeepBothSibling)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_record(*source_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let source_ino = self.ino_for_id(&source_id);
        let parent_id = self
            .unique_node_id_for_record(*parent_record)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let parent_ino = self.ino_for_id(&parent_id);

        // Exact replay after the full namespace snapshot was persisted.
        if let Some(sibling_id) = self.unique_node_id_for_record(*sibling_record) {
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
                *sibling_record,
                sibling_hash.as_ref(),
                sibling_revision.byte_len,
                transaction.manifest.timestamp_ms,
            )
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let source_revision = source
            .proposed_content_revision
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        if !self.set_content_revision_preserving_preview(
            source_ino,
            &source_revision,
            transaction.manifest.timestamp_ms,
        ) || !self.set_content_revision_preserving_preview(
            sibling_ino,
            &sibling_revision,
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        Ok(Some(self.save_incremental()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Replacement projection keeps signature/participant checks, replay detection, namespace preconditions, rename, and revision installation in order."
    )]
    fn apply_replacement_transaction_in_place(
        &mut self,
        transaction: &FileMutationTransaction,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let fs_protocol::FileMutationOperation::ReplacementRename {
            source_parent,
            source_name,
            destination_parent,
            destination_name,
            source_record,
            replaced_record,
        } = &transaction.manifest.operation
        else {
            return Err(TransactionProjectionError::InvalidTransaction);
        };
        let source_participant = transaction
            .manifest
            .participants
            .iter()
            .find(|value| value.role == FileMutationParticipantRole::Source)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let replaced_participant = transaction
            .manifest
            .participants
            .iter()
            .find(|value| value.role == FileMutationParticipantRole::Replaced)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let source_id = self
            .unique_node_id_for_record(*source_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;
        let replaced_id = self
            .unique_node_id_for_record(*replaced_record)
            .ok_or(TransactionProjectionError::UnknownOrDuplicateRecord)?;

        let source_live = self.reaches_root(&source_id);
        let replaced_live = self.reaches_root(&replaced_id);
        let source_parent_now = self
            .parent_of(&source_id)
            .and_then(|id| self.record_id_for_id(&id));
        let source_name_now = self.name_of(&source_id);
        let replaced_parent_now = self
            .parent_of(&replaced_id)
            .and_then(|id| self.record_id_for_id(&id));
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
            .unique_node_id_for_record(*destination_parent)
            .ok_or(TransactionProjectionError::PreconditionFailed)?;
        let source_parent_id = self
            .unique_node_id_for_record(*source_parent)
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
            transaction.manifest.timestamp_ms,
        ) {
            return Err(TransactionProjectionError::PreconditionFailed);
        }
        if let Some(revision) = &source_participant.proposed_content_revision
            && !self.set_content_revision_preserving_preview(
                source_ino,
                revision,
                transaction.manifest.timestamp_ms,
            )
        {
            return Err(TransactionProjectionError::ContentProjectionFailed);
        }
        debug_assert_eq!(replaced_participant.record_id, *replaced_record);
        Ok(Some(self.save_incremental()))
    }

    /// Winning atomic content pointer for this inode. Malformed or
    /// cross-record values fail closed to the legacy projection. Callers that
    /// cross a trust boundary must additionally verify the referenced version
    /// in the signed record repository.
    pub fn content_revision_for(&self, ino: u64) -> Option<ContentRevisionV1> {
        self.content_revision_for_id(self.id_for_ino(ino)?)
    }

    pub(crate) fn content_revision_for_id(&self, id: &str) -> Option<ContentRevisionV1> {
        if self.content_authority == ContentAuthority::SignedRecords {
            let record_id = self.record_id_for_id(id)?;
            return self
                .verified_records
                .get(&record_id)
                .map(|projection| projection.revision);
        }
        self.raw_content_revision_for_id(id)
    }

    /// [`Self::content_revision_for_id`] for a node whose object and record
    /// id the caller has already read.
    fn content_revision_of(
        &self,
        obj: &ObjId,
        record_id: FileRecordId,
    ) -> Option<ContentRevisionV1> {
        if self.content_authority == ContentAuthority::SignedRecords {
            return self
                .verified_records
                .get(&record_id)
                .map(|projection| projection.revision);
        }
        let revision = match self.doc.get(obj, CONTENT_REVISION_KEY) {
            Ok(Some((Value::Scalar(value), _))) => revision_from_scalar(value.as_ref())?,
            _ => return None,
        };
        (revision.record_id == record_id).then_some(revision)
    }

    fn raw_content_revision_for_id(&self, id: &str) -> Option<ContentRevisionV1> {
        let obj = self.node(id)?;
        let revision = match self.doc.get(&obj, CONTENT_REVISION_KEY) {
            Ok(Some((Value::Scalar(value), _))) => revision_from_scalar(value.as_ref())?,
            _ => return None,
        };
        (self.record_id_for_id(id) == Some(revision.record_id)).then_some(revision)
    }

    /// Resolve a live stable record without going through a mutable path.
    /// Shared-record replication uses this to apply content metadata after a
    /// rename while preserving the namespace's existing name and parent.
    pub fn ino_for_record_id(&self, record_id: FileRecordId) -> Option<u64> {
        if self.record_id_for(ROOT_INO) == Some(record_id) {
            return Some(ROOT_INO);
        }
        if self.content_authority == ContentAuthority::SignedRecords {
            let only = self.unique_live_node_for_record(record_id)?;
            return self.id_to_ino.get(&only).copied();
        }
        self.tree
            .nodes_of(record_id)
            .find(|id| self.record_id_for_id(id) == Some(record_id) && self.reaches_root(id))
            .and_then(|id| self.id_to_ino.get(id).copied())
    }

    /// Resolve one live stable record while lazily assigning its process-local
    /// inode. Namespace snapshots intentionally do not persist inode maps, so
    /// authenticated startup reconciliation must use this before any FUSE
    /// lookup has warmed the cache. Ambiguous stable identities fail closed.
    pub(crate) fn ensure_ino_for_record_id(&mut self, record_id: FileRecordId) -> Option<u64> {
        if self.record_id_for(ROOT_INO) == Some(record_id) {
            return Some(ROOT_INO);
        }
        let only = self.unique_live_node_for_record(record_id)?;
        Some(self.ino_for_id(&only))
    }

    /// The one live node bound to `record_id`, `None` when there is none or
    /// more than one. The structural index knows every node carrying the
    /// id, so this costs O(nodes of the record × depth), not a scan of the
    /// tree; each candidate is checked against the document.
    fn unique_live_node_for_record(&self, record_id: FileRecordId) -> Option<String> {
        let mut live = self
            .tree
            .nodes_of(record_id)
            .filter(|id| self.record_id_for_id(id) == Some(record_id) && self.reaches_root(id));
        let only = live.next()?.clone();
        if live.next().is_some() {
            return None;
        }
        Some(only)
    }

    /// Absolute path of `ino` within the mount — `"/"` for the root, `"/a/b/c"`
    /// otherwise; `None` if unknown or its parent chain doesn't reach ROOT
    /// (trashed / cyclic).
    ///
    /// This is what FUSE's per-identity ACL enforcement keys on: the check is
    /// "may this identity read/write *this path*", so it needs the resolved path.
    pub fn path_for(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some("/".to_string());
        }
        self.path_for_id(self.id_for_ino(ino)?)
    }

    /// Absolute path of a stable namespace node id. Unlike inode numbers,
    /// node ids are present before a FUSE lookup allocates a local inode, so
    /// the network content gate can map a requested blob back to every live
    /// path that names it without mutating the namespace index.
    pub fn path_for_id(&self, id: &str) -> Option<String> {
        if id == ROOT_ID {
            return Some("/".to_string());
        }
        // The structural index holds every node's parent and name.
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

    /// Every live path whose file content or generated thumbnail has `hash`,
    /// as tests observe it; the blob provider indexes
    /// [`Self::live_blob_owners`] once instead. Trashed nodes are omitted.
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

    /// Bounded direct reverse lookup for manifest roots and thumbnails, as
    /// tests observe [`Self::live_record_blob_roots`]. Returns stable record
    /// IDs only—never paths, names, or sibling metadata.
    ///
    /// # Errors
    ///
    /// As [`Self::live_record_blob_roots`].
    #[cfg(test)]
    pub fn live_record_ids_for_direct_blob(
        &self,
        hash: Hash,
        max_live_records: usize,
    ) -> Result<Vec<FileRecordId>, LiveRecordBlobScanError> {
        let mut records: Vec<_> = self
            .live_record_blob_roots(max_live_records)?
            .into_iter()
            .filter(|roots| roots.manifest == Some(hash) || roots.thumbnail == Some(hash))
            .map(|roots| roots.record_id)
            .collect();
        records.sort();
        records.dedup();
        Ok(records)
    }

    /// Minimal live record graph used by the content store to resolve manifest
    /// chunks. Kept crate-private so callers cannot accidentally disclose
    /// another record's roots while checking one requested blob.
    pub fn live_record_blob_roots(
        &self,
        max_live_records: usize,
    ) -> Result<Vec<LiveRecordBlobRoots>, LiveRecordBlobScanError> {
        if max_live_records == 0 || max_live_records > MAX_LIVE_RECORD_BLOB_SCAN {
            return Err(LiveRecordBlobScanError::InvalidLimit);
        }
        let mut live_count = 0usize;
        let mut roots = Vec::new();
        for id in self.doc.keys(&self.nodes) {
            if !self.reaches_root(&id) {
                continue;
            }
            live_count = live_count.saturating_add(1);
            if live_count > max_live_records {
                return Err(LiveRecordBlobScanError::LimitExceeded {
                    limit: max_live_records,
                });
            }
            let record_id = self
                .record_id_for_id(&id)
                .ok_or(LiveRecordBlobScanError::MissingRecordIdentity)?;
            let manifest = self.node_hash(&id);
            let thumbnail = self.thumbnail_for_id(&id);
            if manifest.is_some() || thumbnail.is_some() {
                roots.push(LiveRecordBlobRoots {
                    record_id,
                    manifest,
                    thumbnail,
                });
            }
        }
        Ok(roots)
    }

    /// Parent inode of a visible node. The root has no parent. This is kept in
    /// the namespace layer so callers never try to recover identity from a
    /// path string (renames preserve node identity and inode number).
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
        // Saturates rather than panics: `mtime_ms` is stored CRDT state, not
        // computed here, so an out-of-range value must degrade, not take the mount down.
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

    pub fn getattr(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        if !self.reaches_root(id) {
            return None;
        }
        let meta = self.node_meta(id)?;
        Some(Self::attr_for(ino, meta.kind, meta.size, meta.mtime))
    }

    /// Attribute lookup for a kernel file handle whose directory entry has
    /// since been unlinked or replaced. Path lookup must continue using
    /// [`Self::getattr`]; only an already-open handle may reach this view.
    pub(crate) fn getattr_including_unlinked(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        let meta = self.node_meta(id)?;
        Some(Self::attr_for(ino, meta.kind, meta.size, meta.mtime))
    }

    /// [`Self::getattr`] and [`Self::hash_for`] of a live node, reading it
    /// once.
    pub(crate) fn attr_and_hash(&self, ino: u64) -> Option<(FileAttr, Option<Hash>)> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some((Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0), None));
        }
        if !self.reaches_root(id) {
            return None;
        }
        let meta = self.node_meta(id)?;
        Some((
            Self::attr_for(ino, meta.kind, meta.size, meta.mtime),
            meta.hash,
        ))
    }

    /// Everything one FUSE request asks about `ino`, read in one pass so the
    /// caller takes the namespace lock once: what [`Self::getattr`] (or,
    /// with `unlinked`, [`Self::getattr_including_unlinked`]),
    /// [`Self::hash_for`], [`Self::path_for`] and [`Self::document_codec`]
    /// would each answer. The path is only built when `query.path` asks for
    /// it — a mount without an ACL never looks at it.
    ///
    /// `None` when [`Self::getattr`] is `None` and either `unlinked` is not
    /// set or the inode is unknown.
    pub(crate) fn inode_view(&self, ino: u64, query: InodeQuery) -> Option<InodeView> {
        let id = self.ino_to_id.get(&ino)?;
        if id == ROOT_ID {
            return Some(InodeView {
                attr: Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0),
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
            attr: Self::attr_for(ino, meta.kind, meta.size, meta.mtime),
            hash: meta.hash,
            live,
            path: if live && query.path {
                self.path_for_id(id)
            } else {
                None
            },
            document: self.content_authority == ContentAuthority::LegacyNamespace
                && str_at(&self.doc, &obj, "codec").is_some(),
        })
    }

    /// The inode and kind of `parent_ino`'s live child `name`: what
    /// [`Self::lookup`] finds, without reading the child's other fields.
    pub fn lookup_kind(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
    ) -> Option<(u64, FileType)> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        self.record_id_for_id(&child)?;
        let kind = self.kind_of(&child).to_file_type();
        Some((self.ino_for_id(&child), kind))
    }

    pub fn lookup(&mut self, parent_ino: u64, name: impl AsRef<[u8]>) -> Option<FileAttr> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        let meta = self.node_meta(&child)?;
        let ino = self.ino_for_id(&child);
        Some(Self::attr_for(ino, meta.kind, meta.size, meta.mtime))
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

    /// `None` only if the document's node map itself has become unwritable
    /// (never observed against a live `Namespace`); every caller already
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
            FileRecordId::new(),
        )
    }

    /// [`Self::create_node`] bound to an existing stable `record_id`.
    fn create_node_as(
        &mut self,
        parent_id: &str,
        name: impl AsRef<[u8]>,
        kind: NodeKind,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
        record_id: FileRecordId,
    ) -> Option<String> {
        let name = name.as_ref();
        // Node identity is derived from the stable record identity, not from
        // the local author counter. A peer rebuilding this record from the
        // signed log therefore creates the same Automerge object key as the
        // device that published it, and the two namespaces merge instead of
        // producing two nodes for one record.
        let derived = record_node_id(record_id);
        let id = if self.node(&derived).is_some() {
            if self.record_id_for_id(&derived) == Some(record_id) {
                return None;
            }
            // A pre-existing legacy object happens to use the derived key.
            // Keep the stable record itself, with a local node key; the
            // record id, not the object key, is the durable identity.
            self.gen_id()
        } else {
            derived
        };
        let obj = self.doc.put_object(&self.nodes, &id, ObjType::Map).ok()?;
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
        let _ = self.doc.put(&obj, RECORD_ID_KEY, record_id.0.to_string());
        self.tree
            .place(&id, parent_id.to_string(), name.to_vec().into());
        self.tree.bind_record(&id, record_id);
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
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        Some(ino)
    }

    /// Create a projected file for an already-authoritative stable record.
    /// Used only by exact-record share admission: it refuses nil/duplicate
    /// record ids and name collisions instead of minting or aliasing identity.
    pub fn create_projected_file(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        record_id: FileRecordId,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<u64> {
        let name = name.as_ref();
        if record_id.0.is_nil()
            || name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || self.ino_for_record_id(record_id).is_some()
            || self.lookup(parent_ino, name).is_some()
        {
            return None;
        }
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, hash, size, mtime)?;
        let obj = self.node(&id)?;
        self.doc
            .put(&obj, RECORD_ID_KEY, record_id.0.to_string())
            .ok()?;
        self.tree.bind_record(&id, record_id);
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        Some(ino)
    }

    /// [`Self::create_projected_file`] whose content is the verified
    /// `revision`: the node is written with the revision's fields once, not
    /// created and then updated.
    pub fn create_projected_file_at_revision(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        revision: &ContentRevisionV1,
        mtime: u64,
    ) -> Option<u64> {
        if self.content_authority != ContentAuthority::SignedRecords {
            let hash = revision.content_hash.map(|hash| Hash::from(hash.0));
            let ino = self.create_projected_file(
                parent_ino,
                name,
                revision.record_id,
                hash.as_ref(),
                revision.byte_len,
                mtime,
            )?;
            return self
                .set_content_revision(ino, revision, mtime)
                .then_some(ino);
        }
        let name = name.as_ref();
        let encoded = revision_to_bytes(revision)?;
        if revision.record_id.0.is_nil()
            || name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || self.ino_for_record_id(revision.record_id).is_some()
            || self.lookup_kind(parent_ino, name).is_some()
        {
            return None;
        }
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let hash = revision.content_hash.map(|hash| Hash::from(hash.0));
        let id = self.create_node_as(
            &parent_id,
            name,
            NodeKind::File,
            hash.as_ref(),
            revision.byte_len,
            mtime,
            revision.record_id,
        )?;
        let obj = self.node(&id)?;
        self.doc
            .put(&obj, CONTENT_REVISION_KEY, ScalarValue::Bytes(encoded))
            .ok()?;
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        if !self.install_verified_content_revision(*revision) {
            let _ = self.remove(ino);
            return None;
        }
        Some(ino)
    }

    pub fn create_dir(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::Dir, None, 0, mtime)?;
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        Some(ino)
    }

    /// Create a directory whose stable identity has already been signed.
    pub fn create_projected_dir(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        record_id: FileRecordId,
        mtime: u64,
    ) -> Option<u64> {
        let name = name.as_ref();
        if record_id.0.is_nil()
            || name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || self.ino_for_record_id(record_id).is_some()
            || self.lookup(parent_ino, name).is_some()
        {
            return None;
        }
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::Dir, None, 0, mtime)?;
        let obj = self.node(&id)?;
        self.doc
            .put(&obj, RECORD_ID_KEY, record_id.0.to_string())
            .ok()?;
        self.tree.bind_record(&id, record_id);
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        Some(ino)
    }

    // Document files: content lives as a CRDT, not opaque bytes.

    /// Create a file whose content is a structured rich-text document stored as a
    /// CRDT (concurrent edits merge). `codec` is the format key (e.g. `odt`, `md`).
    pub fn create_document(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        codec: &str,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, None, 0, mtime)?;
        self.set_field(&id, "codec", codec);
        let node = self.node(&id)?;
        let root = self.doc.put_object(&node, "doc", ObjType::Map).ok()?;
        tilo_doc::model::init(&mut self.doc, &root);
        let ino = self.ino_for_id(&id);
        self.index_insert_node(&id);
        Some(ino)
    }

    pub fn document_codec(&self, ino: u64) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        str_at(&self.doc, &self.node(id)?, "codec")
    }

    fn document_root(&self, id: &str) -> Option<ObjId> {
        map_at(&self.doc, &self.node(id)?, "doc")
    }

    /// Project a document into its file format, embedding the binary content (images, binary
    /// package parts) it references from `content`.
    pub fn export_document(
        &self,
        ino: u64,
        content: &dyn tilo_content::ContentSource,
    ) -> Option<Vec<u8>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let codec = str_at(&self.doc, &self.node(&id)?, "codec")?;
        let provider = tilo_doc_codecs::provider_for_ext(&codec)?;
        let root = self.document_root(&id)?;
        let containers = tilo_doc::model::open(&self.doc, &root).ok()?;
        provider
            .export(
                tilo_doc::DocRef {
                    doc: &self.doc,
                    c: &containers,
                },
                content,
            )
            .ok()
    }

    /// Bring a document's content to what `bytes` says (whole-file write), by
    /// reconciling it into the CRDT the document already is.
    ///
    /// The file states what the document should read, not what its CRDT should
    /// hold: blocks the file leaves unchanged keep their ids, a peer's
    /// concurrent edit merges as any other edit does, and a save adds only its
    /// own difference to the history. The binary content the file embeds is kept
    /// in `content`; the document only references it.
    ///
    /// Returns whether the write happened: a malformed file leaves the document
    /// as it was.
    pub fn import_document(
        &mut self,
        ino: u64,
        bytes: &[u8],
        mtime: u64,
        content: &tilo_content::ContentStore,
    ) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        let Some(node) = self.node(&id) else {
            return false;
        };
        let Some(codec) = str_at(&self.doc, &node, "codec") else {
            return false;
        };
        let Some(provider) = tilo_doc_codecs::provider_for_ext(&codec) else {
            return false;
        };
        let Some(root) = self.document_root(&id) else {
            return false;
        };
        // Parse into a throwaway document first — a malformed input must never
        // touch the stored one.
        let mut probe = tilo_doc::Document::new();
        let mut inputs = tilo_content::ContentInputs::new();
        if provider
            .import(bytes, &mut probe.as_mut(), &mut inputs)
            .is_err()
        {
            return false;
        }
        let wanted = probe.as_ref().materialize();
        // Keep the file's binary content before any document refers to it.
        if content.put_inputs(&inputs).is_err() {
            return false;
        }
        // The write is one transaction: either it lands as the file says, or it
        // is rolled back and the document reads as it did.
        if tilo_doc::reconcile_under(&mut self.doc, &root, &wanted).is_err()
            || !tilo_doc::reads_as(&self.doc, &root, &wanted)
        {
            self.doc.rollback();
            return false;
        }
        self.set_field(&id, "mtime", mtime);
        true
    }

    /// Current heads of the document — a cheap change token (e.g. to invalidate a derived export cache).
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// A receiver that sees, after this call, every change to the
    /// document and to the sync progress beside it
    /// ([`Self::peer_sync_generation`]). It says that something may have
    /// changed, never what; compare [`Self::heads`] to know. Persisting
    /// and repairing on it replaces polling.
    #[must_use]
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.doc.subscribe()
    }

    /// O(heads): a token that changes whenever anything this namespace
    /// projects changes — document edits (local or remote) and verified
    /// content installs alike. Background reconcilers compare it instead of
    /// walking every node to discover that nothing happened.
    pub fn change_token(&mut self) -> NamespaceChangeToken {
        NamespaceChangeToken {
            heads: self.doc.get_heads(),
            verified_generation: self.verified_generation,
        }
    }

    /// Whether the document file `ino` (its node: codec and content) changed
    /// between namespace heads `before` and `after`. The diff walks only
    /// that node's objects, so a change to another file costs little and
    /// keeps a derived export valid. Unknown inodes count as changed.
    pub fn document_changed_between(
        &mut self,
        ino: u64,
        before: &[automerge::ChangeHash],
        after: &[automerge::ChangeHash],
    ) -> bool {
        let Some(node) = self.ino_to_id.get(&ino).and_then(|id| self.node(id)) else {
            return true;
        };
        self.doc
            .diff_obj(&node, before, after, true)
            .map_or(true, |patches| !patches.is_empty())
    }

    /// Every change this document holds beyond `heads`, as one incremental
    /// chunk for [`Self::apply_remote_and_resolve`]. Heads this document
    /// does not know (the asker's own changes) are ignored, so an asker that
    /// shares no history receives everything.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<u8> {
        let known: Vec<automerge::ChangeHash> = heads
            .iter()
            .copied()
            .filter(|hash| self.doc.has_change(hash))
            .collect();
        self.doc.save_after(&known)
    }

    // Fine-grained document edits: the path that merges semantically.

    fn with_doc_mut<R>(
        &mut self,
        ino: u64,
        f: impl FnOnce(&mut tilo_doc::DocMut) -> R,
    ) -> Option<R> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let root = self.document_root(&id)?;
        let containers = tilo_doc::model::open(&self.doc, &root).ok()?;
        let mut ids = tilo_doc::model::seeded_id_gen(&self.doc, &containers);
        let mut dmut = tilo_doc::DocMut {
            doc: &mut self.doc,
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
        let containers = tilo_doc::model::open(&self.doc, &root).ok()?;
        tilo_doc::model::block_text(&self.doc, &containers, block_id)
    }

    pub fn document_remove_block(&mut self, ino: u64, block_id: &str) -> bool {
        self.with_doc_mut(ino, |d| d.remove_block(block_id))
            .is_some()
    }

    pub fn document_snapshot(&self, ino: u64) -> Option<tilo_doc::DocSnapshot> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let containers = tilo_doc::model::open(&self.doc, &root).ok()?;
        Some(
            tilo_doc::DocRef {
                doc: &self.doc,
                c: &containers,
            }
            .materialize(),
        )
    }

    pub fn set_content(&mut self, ino: u64, hash: Option<&Hash>, size: u64, mtime: u64) -> bool {
        if self.content_authority == ContentAuthority::SignedRecords {
            return false;
        }
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        // A legacy mutation explicitly supersedes any atomic revision. This
        // prevents remaining old call sites from changing mirror fields that
        // revision-aware readers would otherwise ignore.
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
        // Content changed, so any recorded thumbnail is stale — drop the pointer (regenerates lazily).
        if let Some(obj) = self.node(&id) {
            let _ = self.doc.delete(&obj, "thumb");
        }
        self.set_field(&id, "size", size);
        self.set_field(&id, "mtime", mtime);
        let author = self.author.clone();
        self.set_field(&id, "author", author);
        true
    }

    /// Atomically advertise one immutable signed history version as the
    /// current file content. Loose hash/size fields are mirrored only for
    /// legacy readers; revision-aware readers always select this single CRDT
    /// scalar and therefore cannot observe a torn content tuple.
    pub fn set_content_revision(
        &mut self,
        ino: u64,
        revision: &ContentRevisionV1,
        mtime: u64,
    ) -> bool {
        self.install_content_revision(ino, revision, mtime, true)
    }

    /// Advance only the signed record/version pointer while retaining a
    /// preview of unchanged bytes (for rename/move metadata versions).
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
            || self.record_id_for_id(&id) != Some(revision.record_id)
            || revision.validate().is_err()
        {
            return false;
        }
        // When the document already carries this revision at this time (the
        // author wrote it, and it arrived with the namespace; or it is being
        // projected again), verifying it here is all that is left: writing
        // the same fields again would only add document operations that
        // every peer then has to sync. Concurrent values (two devices
        // projected their own heads while apart) are not carried: writing
        // the winner resolves them, as a namespace conflict copy would
        // otherwise surface the loser.
        if self.content_authority == ContentAuthority::SignedRecords
            && self.raw_content_revision_for_id(&id) == Some(*revision)
            && !self.has_concurrent_content(&id)
            && self
                .node(&id)
                .is_some_and(|obj| read_u64(&self.doc, &obj, "mtime") == mtime)
        {
            return self.install_verified_content_revision(*revision);
        }
        if self.content_authority == ContentAuthority::SignedRecords
            && !self.install_verified_content_revision(*revision)
        {
            return false;
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

    /// Attach the content hash of a generated thumbnail blob to `ino`, so the
    /// pointer rides namespace gossip and a peer can preview before fetching the full content.
    pub fn set_thumb(&mut self, ino: u64, thumb: &Hash) -> bool {
        if self.content_authority == ContentAuthority::SignedRecords {
            return false;
        }
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
        if self.content_authority == ContentAuthority::SignedRecords {
            let record_id = self.record_id_for(ino)?;
            return self
                .verified_records
                .get(&record_id)
                .and_then(|projection| projection.thumbnail);
        }
        let id = self.ino_to_id.get(&ino)?;
        str_at(&self.doc, &self.node(id)?, "thumb").and_then(|h| hash_from_hex(&h))
    }

    /// Every live node naming content, with its path: the input of the blob
    /// provider's reverse index. One pass over the tree, where a lookup per
    /// blob would scan it per request.
    /// A record placed at more than one live node publishes no thumbnail,
    /// as in [`Self::thumbnail_for_id`].
    pub(crate) fn live_blob_owners(&self) -> Vec<LiveBlobOwner> {
        let live = self.live_node_ids();
        let signed = self.content_authority == ContentAuthority::SignedRecords;
        let mut placements: HashMap<FileRecordId, usize> = HashMap::new();
        if signed {
            for id in &live {
                if let Some(record) = self.record_id_for_id(id) {
                    let count = placements.entry(record).or_insert(0);
                    *count = count.saturating_add(1);
                }
            }
        }
        live.iter()
            .filter_map(|id| {
                let record_id = self.record_id_for_id(id);
                let manifest = self.node_hash(id);
                let thumbnail = if signed {
                    record_id
                        .filter(|record| placements.get(record) == Some(&1))
                        .and_then(|record| self.verified_records.get(&record))
                        .and_then(|projection| projection.thumbnail)
                } else {
                    self.node(id)
                        .and_then(|obj| str_at(&self.doc, &obj, "thumb"))
                        .and_then(|value| hash_from_hex(&value))
                };
                if manifest.is_none() && thumbnail.is_none() {
                    return None;
                }
                Some(LiveBlobOwner {
                    path: self.path_for_id(id)?,
                    record_id,
                    manifest,
                    thumbnail,
                })
            })
            .collect()
    }

    fn thumbnail_for_id(&self, id: &str) -> Option<Hash> {
        if self.content_authority == ContentAuthority::SignedRecords {
            let record_id = self.record_id_for_id(id)?;
            if self.unique_live_node_for_record(record_id).as_deref() != Some(id) {
                return None;
            }
            return self
                .verified_records
                .get(&record_id)
                .and_then(|projection| projection.thumbnail);
        }
        self.node(id)
            .and_then(|obj| str_at(&self.doc, &obj, "thumb"))
            .and_then(|value| hash_from_hex(&value))
    }

    /// Delete = move the node (and its subtree) to TRASH.
    pub fn remove(&mut self, ino: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() || !self.reaches_root(&id) {
            return false;
        }
        // Record the content hash at deletion time, so a concurrent edit can be detected and resurrected.
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
        self.rebuild_record_index();
        // A delete may release a name another signed record was waiting for.
        let _ = self.settle_structural_conflicts(&cur);
        true
    }

    pub fn directly_trashed(&self) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.is_directly_trashed(id))
            .collect()
    }

    /// Whether `id` itself was moved to the trash, not just an ancestor.
    pub fn is_directly_trashed(&self, id: &str) -> bool {
        self.parent_of(id).as_deref() == Some(TRASH_ID)
    }

    /// Up to `limit` node ids, live or not, in key order, starting after
    /// `after` (at the first when `None`). Seeking to `after` is a search,
    /// not a walk, so a caller can visit every node in bounded steps and
    /// release the namespace lock between them.
    pub fn node_ids_after(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        let start = after.map_or(std::ops::Bound::Unbounded, |id| {
            std::ops::Bound::Excluded(id.to_string())
        });
        self.doc
            .map_range(&self.nodes, (start, std::ops::Bound::Unbounded))
            .take(limit)
            .map(|item| item.key.into_owned())
            .collect()
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

    /// Rename / move: a single reparent (node keeps its id, so inode stays stable). Rejects moving a directory into its own descendant.
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
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        self.set_field(&id, "prev_parent", cur.clone());
        self.set_field(&id, "parent", &*new_parent_id);
        self.set_field(&id, PLACEMENT_NAME_KEY, new_name.to_vec());
        self.set_field(&id, "name", new_name.to_vec());
        self.set_field(&id, "mtime", mtime);
        self.rebuild_record_index();
        let _ = self.settle_structural_conflicts(&new_parent_id);
        if cur != new_parent_id {
            let _ = self.settle_structural_conflicts(&cur);
        }
        true
    }

    // Structural resolution (cycle breaking).

    /// Find one cycle in the parent graph (never reaching ROOT/TRASH), as the node ids forming the loop.
    fn find_cycle(&self) -> Option<Vec<String>> {
        let ids: Vec<String> = self.doc.keys(&self.nodes).collect();
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
                if !seen.insert(cur.clone()) || self.node(&cur).is_none() {
                    break;
                }
                seq.push(cur.clone());
                match self.parent_of(&cur) {
                    Some(p) => cur = p,
                    None => break,
                }
            }
        }
        None
    }

    /// Break every move-cycle deterministically: the lowest-id node in each cycle
    /// reverts to `prev_parent` (or ROOT). Pure function of the doc, so every peer computes the same repair.
    pub fn resolve(&mut self) -> bool {
        let mut changed = false;
        let guard = self.doc.keys(&self.nodes).count().saturating_add(2);
        for _ in 0..guard {
            let Some(cycle) = self.find_cycle() else {
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
            changed = true;
        }
        if changed {
            self.rebuild_record_index();
        }
        changed
    }

    // Path + conflict helpers (used by tests and conflict.rs).

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

    /// Resolve a `/`-style path to an inode (allocating one if needed).
    pub fn resolve_path_ino(&mut self, path: &str) -> Option<u64> {
        let mut cur = ROOT_ID.to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            cur = self.child_named(&cur, seg)?;
        }
        Some(self.ino_for_id(&cur))
    }

    pub fn live_node_ids(&self) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.reaches_root(id))
            .collect()
    }

    /// Stable entry lookup used by the storage authority.  It intentionally
    /// exposes logical metadata, never an underlying host path.
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

    /// Record-index presentation type without allocating a process-local
    /// inode. The caller still needs to apply its live mount ACL to the path.
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

    /// Whether the node's content hash or revision holds concurrent values.
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

    /// Concurrent atomic revision values for a node. Cross-record or malformed
    /// scalars are omitted; callers must still verify each referenced signed
    /// version against the record repository before accepting its bytes.
    pub fn content_revision_conflicts(&self, id: &str) -> Vec<(String, ContentRevisionV1)> {
        if self.content_authority == ContentAuthority::SignedRecords {
            return self
                .content_revision_for_id(id)
                .map(|revision| vec![("verified-record".to_string(), revision)])
                .unwrap_or_default();
        }
        let Some(obj) = self.node(id) else {
            return Vec::new();
        };
        let record_id = self.record_id_for_id(id);
        match self.doc.get_all(&obj, CONTENT_REVISION_KEY) {
            Ok(values) => values
                .into_iter()
                .filter_map(|(value, opid)| match value {
                    Value::Scalar(value) => revision_from_scalar(value.as_ref())
                        .filter(|revision| Some(revision.record_id) == record_id)
                        .map(|revision| (opid.to_string(), revision)),
                    Value::Object(_) => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Create a conflict-copy sibling node under `parent_id`, unless one named `name` already exists.
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

/// Reversible presentation for byte names in path-based policy and metadata.
/// Valid UTF-8 remains readable; `%` and invalid bytes are escaped so distinct
/// kernel names cannot collapse onto the same authorization path.
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
