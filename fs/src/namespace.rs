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

use std::collections::{HashMap, HashSet};
use std::time::{Duration, UNIX_EPOCH};

use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::{
    ContentRevisionV1, FILE_BINDING_VERSION, FileActor, FileBindingV1,
    FileMutationParticipantRoleV1, FileMutationTransactionV1, FileNameV1, FileNodeKind,
    FileRecordId,
};
use fuser::{FileAttr, FileType, INodeNo};
use iroh_blobs::Hash;
use uuid::Uuid;

pub const ROOT_INO: u64 = 1;
/// Sentinel parent for the tree root (not materialized as a node).
pub const ROOT_ID: &str = "ROOT";
/// Sentinel parent for deleted subtrees (not materialized as a node).
const TRASH_ID: &str = "TRASH";
const ROOT_RECORD_ID_KEY: &str = "root_record_id";
const RECORD_ID_KEY: &str = "record_id";
const CONTENT_REVISION_KEY: &str = "content_revision_v1";
const RECORD_ID_MIGRATION_DOMAIN: &[u8] = b"qos.collaborative-fs.namespace-record-id.v1";

pub const MAX_LIVE_RECORD_BLOB_SCAN: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRecordBlobRoots {
    pub record_id: FileRecordId,
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

#[derive(Clone)]
pub struct Namespace {
    doc: AutoCommit,
    nodes: ObjId,
    ino_to_id: HashMap<u64, String>,
    id_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
    next_id: u64,
    content_authority: ContentAuthority,
    verified_records: HashMap<FileRecordId, VerifiedRecordProjection>,
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

fn read_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(x) => Some(x.to_string()),
            _ => None,
        },
        _ => None,
    }
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
    /// # Errors
    ///
    /// Returns an error if the fresh document's root `nodes` map cannot be created.
    pub fn new(author: impl Into<String>) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::new();
        let nodes = doc.put_object(&ROOT, "nodes", ObjType::Map)?;
        let mut ns = Self {
            doc,
            nodes,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author: author.into(),
            next_id: 0,
            content_authority: ContentAuthority::LegacyNamespace,
            verified_records: HashMap::new(),
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        let root_record_id = FileRecordId::new();
        let _ = ns
            .doc
            .put(&ROOT, ROOT_RECORD_ID_KEY, root_record_id.0.to_string());
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
        let mut ns = Self {
            doc,
            nodes,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author,
            next_id: max_local,
            content_authority: ContentAuthority::LegacyNamespace,
            verified_records: HashMap::new(),
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        ns.ensure_record_ids();
        Ok(ns)
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
    }

    /// Install a content tuple that has already passed record-repository
    /// signature, certification, grant, version, and sequence validation.
    pub fn install_verified_content_revision(&mut self, revision: ContentRevisionV1) -> bool {
        if revision.validate().is_err() || self.content_authority != ContentAuthority::SignedRecords
        {
            return false;
        }
        let matching = self
            .live_node_ids()
            .into_iter()
            .filter(|id| self.record_id_for_id(id) == Some(revision.record_id))
            .count();
        if matching != 1 {
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
        true
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.ensure_record_ids();
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
        self.ensure_record_ids();
        self.doc.save_incremental()
    }
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid incremental automerge change set for this document.
    pub fn apply_incremental(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        self.doc.load_incremental(data)?;
        self.ensure_record_ids();
        Ok(())
    }
    pub fn prime_baseline(&mut self) {
        let _ = self.doc.save_incremental();
    }

    /// # Errors
    ///
    /// Returns an error if `other`'s document cannot be merged into this one.
    pub fn merge_doc(&mut self, other: &mut Namespace) -> Result<(), automerge::AutomergeError> {
        self.doc.merge(&mut other.doc)?;
        self.ensure_record_ids();
        Ok(())
    }

    /// Apply a remote delta: advance the watermark, resolve, and return the resolution delta to broadcast (empty if none).
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid incremental automerge change set for this document.
    pub fn apply_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        self.doc.load_incremental(data)?;
        self.ensure_record_ids();
        let _ = self.doc.save_incremental();
        self.resolve();
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
        self.doc.merge(&mut other)?;
        self.ensure_record_ids();
        let _ = self.doc.save_incremental();
        self.resolve();
        Ok(self.doc.save_incremental())
    }

    pub fn sync_generate(&mut self, state: &mut automerge::sync::State) -> Option<Vec<u8>> {
        self.doc
            .sync()
            .generate_sync_message(state)
            .map(automerge::sync::Message::encode)
    }

    /// # Errors
    ///
    /// Returns an error if `data` is not a valid sync message, or applying it against `state` fails.
    pub fn sync_receive(
        &mut self,
        state: &mut automerge::sync::State,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let msg = automerge::sync::Message::decode(data)?;
        self.doc.sync().receive_sync_message(state, msg)?;
        self.ensure_record_ids();
        Ok(())
    }

    /// Discard the pulled changes from the watermark (peers already have them), resolve, and return only the resolution delta.
    pub fn post_sync_resolve(&mut self) -> Vec<u8> {
        let _ = self.doc.save_incremental();
        self.resolve();
        self.doc.save_incremental()
    }

    fn node(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.nodes, id) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        }
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
        if read_str(&self.doc, &ROOT, ROOT_RECORD_ID_KEY)
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
            if read_str(&self.doc, &node, RECORD_ID_KEY)
                .and_then(|value| Uuid::parse_str(&value).ok())
                .is_some()
            {
                continue;
            }
            let record_id = self.deterministic_record_id(&id);
            let _ = self.doc.put(&node, RECORD_ID_KEY, record_id.0.to_string());
        }
    }

    fn parent_of(&self, id: &str) -> Option<String> {
        read_str(&self.doc, &self.node(id)?, "parent")
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
    fn kind_of(&self, id: &str) -> NodeKind {
        self.node(id)
            .and_then(|o| read_str(&self.doc, &o, "kind"))
            .map_or(NodeKind::File, |k| NodeKind::from_str(&k))
    }

    fn set_field(&mut self, id: &str, key: &str, val: impl Into<ScalarValue>) {
        if let Some(obj) = self.node(id) {
            let _ = self.doc.put(&obj, key, val);
        }
    }

    /// Walk the parent chain: `true` if it reaches ROOT (visible), `false` if it
    /// hits TRASH, a missing node, or a cycle.
    fn reaches_root(&self, id: &str) -> bool {
        let mut cur = id.to_string();
        let mut seen = HashSet::new();
        loop {
            if cur == ROOT_ID {
                return true;
            }
            if cur == TRASH_ID {
                return false;
            }
            if !seen.insert(cur.clone()) {
                return false;
            }
            match self.parent_of(&cur) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    fn entry(&self, id: &str) -> Option<Entry> {
        let obj = self.node(id)?;
        let record_id = self.record_id_for_id(id)?;
        let revision = self.content_revision_for_id(id);
        Some(Entry {
            id: id.to_string(),
            record_id,
            parent: read_str(&self.doc, &obj, "parent").unwrap_or_default(),
            name: read_name(&self.doc, &obj, "name").unwrap_or_default(),
            kind: NodeKind::from_str(&read_str(&self.doc, &obj, "kind").unwrap_or_default()),
            hash: match (self.content_authority, revision) {
                (_, Some(revision)) => revision.content_hash.map(|hash| Hash::from(hash.0)),
                (ContentAuthority::SignedRecords, None) => None,
                (ContentAuthority::LegacyNamespace, None) => {
                    read_str(&self.doc, &obj, "hash").and_then(|h| hash_from_hex(&h))
                }
            },
            size: match (self.content_authority, revision) {
                (_, Some(revision)) => revision.byte_len,
                (ContentAuthority::SignedRecords, None) => 0,
                (ContentAuthority::LegacyNamespace, None) => read_u64(&self.doc, &obj, "size"),
            },
            mtime: read_u64(&self.doc, &obj, "mtime"),
            author: read_str(&self.doc, &obj, "author").unwrap_or_default(),
        })
    }

    fn children(&self, parent_id: &str) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.parent_of(id).as_deref() == Some(parent_id))
            .collect()
    }

    /// The live child of `parent_id` named `name` — smallest id wins on a tie.
    fn child_named(&self, parent_id: &str, name: impl AsRef<[u8]>) -> Option<String> {
        let name = name.as_ref();
        let mut matches: Vec<String> = self
            .children(parent_id)
            .into_iter()
            .filter(|id| {
                self.name_of(id)
                    .is_some_and(|candidate| candidate.as_bytes() == name)
                    && self.reaches_root(id)
            })
            .collect();
        matches.sort();
        matches.into_iter().next()
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
            read_str(&self.doc, &ROOT, ROOT_RECORD_ID_KEY)?
        } else {
            read_str(&self.doc, &self.node(id)?, RECORD_ID_KEY)?
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

    /// Project one already-verified structural binding. The operation is
    /// idempotent and never replaces an occupied destination: replacement is
    /// reserved for the complete-participant transaction materializer.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding does not describe one valid live node
    /// move in this namespace.
    pub fn apply_signed_binding(
        &mut self,
        binding: &FileBindingV1,
    ) -> Result<bool, BindingProjectionError> {
        if self.record_id_for(ROOT_INO) == Some(binding.record_id) {
            let exact_root = binding.parent.is_none()
                && binding.name.is_empty()
                && binding.kind == FileNodeKind::Directory
                && !binding.tombstoned;
            return exact_root
                .then_some(false)
                .ok_or(BindingProjectionError::InvalidMove);
        }
        let matches = self
            .doc
            .keys(&self.nodes)
            .filter(|id| self.record_id_for_id(id) == Some(binding.record_id))
            .collect::<Vec<_>>();
        let [id] = matches.as_slice() else {
            return Err(BindingProjectionError::UnknownOrDuplicateRecord);
        };
        let id = id.clone();
        let expected_kind = match binding.kind {
            FileNodeKind::File => NodeKind::File,
            FileNodeKind::Directory => NodeKind::Dir,
        };
        if self.kind_of(&id) != expected_kind {
            return Err(BindingProjectionError::KindMismatch);
        }
        let currently_live = self.reaches_root(&id);
        let ino = self.ino_for_id(&id);
        if binding.tombstoned {
            if !currently_live {
                return Ok(false);
            }
            return self
                .remove(ino)
                .then_some(true)
                .ok_or(BindingProjectionError::InvalidMove);
        }
        let parent_record = binding
            .parent
            .ok_or(BindingProjectionError::UnknownParent)?;
        let parent_ino = self
            .ino_for_record_id(parent_record)
            .ok_or(BindingProjectionError::UnknownParent)?;
        if self
            .getattr(parent_ino)
            .is_none_or(|attr| attr.kind != FileType::Directory)
        {
            return Err(BindingProjectionError::UnknownParent);
        }
        if !currently_live {
            // Resurrection is intentionally not inferred from a move. Delete
            // conflicts need their own signed structural-conflict transition.
            return Err(BindingProjectionError::InvalidMove);
        }
        let current_parent_id = self
            .parent_of(&id)
            .ok_or(BindingProjectionError::InvalidMove)?;
        let current_parent_record = self
            .record_id_for_id(&current_parent_id)
            .ok_or(BindingProjectionError::InvalidMove)?;
        let current_name = self
            .name_of(&id)
            .ok_or(BindingProjectionError::InvalidMove)?;
        if current_parent_record == parent_record && current_name == binding.name {
            return Ok(false);
        }
        if self
            .lookup(parent_ino, &binding.name)
            .is_some_and(|attr| attr.ino.0 != ino)
        {
            return Err(BindingProjectionError::OccupiedDestination);
        }
        let current_parent_ino = self
            .ino_for_record_id(current_parent_record)
            .ok_or(BindingProjectionError::InvalidMove)?;
        self.rename(
            current_parent_ino,
            &current_name,
            parent_ino,
            &binding.name,
            binding.observed_at_ms,
        )
        .then_some(true)
        .ok_or(BindingProjectionError::InvalidMove)
    }

    /// Apply one fully committed replacement-rename transaction while the
    /// namespace lock is held. All preconditions are checked before mutation;
    /// the returned delta contains both the victim tombstone and source move.
    ///
    /// # Errors
    ///
    /// Returns an error when transaction verification or projection fails.
    pub fn apply_replacement_transaction(
        &mut self,
        transaction: &FileMutationTransactionV1,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        // Automerge transactions make the document edits atomic, but the
        // verified-record projection is maintained alongside the document.
        // Apply to a complete clone so a late projection failure cannot leave
        // either authority half-mutated in the live namespace.
        let mut candidate = self.clone();
        let delta = candidate.apply_replacement_transaction_in_place(transaction)?;
        if delta.is_some() {
            *self = candidate;
        }
        Ok(delta)
    }

    /// Apply either supported signed multi-record mutation to a complete
    /// namespace clone, publishing the clone only after every precondition and
    /// both record projections succeed.
    ///
    /// # Errors
    ///
    /// Returns an error when transaction verification or projection fails.
    pub fn apply_mutation_transaction(
        &mut self,
        transaction: &FileMutationTransactionV1,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        match transaction.manifest.operation {
            collab_types::FileMutationOperationV1::ReplacementRename { .. } => {
                self.apply_replacement_transaction(transaction)
            }
            collab_types::FileMutationOperationV1::KeepBothContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_keep_both_transaction_in_place(transaction)?;
                if delta.is_some() {
                    *self = candidate;
                }
                Ok(delta)
            }
            collab_types::FileMutationOperationV1::MergeContentConflict { .. } => {
                let mut candidate = self.clone();
                let delta = candidate.apply_merge_transaction_in_place(transaction)?;
                if delta.is_some() {
                    *self = candidate;
                }
                Ok(delta)
            }
        }
    }

    /// Install a durably staged merge delta and its signed content revision.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction or delta is invalid, incomplete,
    /// or inconsistent with the resulting projection.
    pub fn apply_staged_merge_delta(
        &mut self,
        transaction: &FileMutationTransactionV1,
        delta: &[u8],
    ) -> Result<(), TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let collab_types::FileMutationOperationV1::MergeContentConflict {
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
            .filter(|participant| participant.role == FileMutationParticipantRoleV1::MergeSource)
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
        transaction: &FileMutationTransactionV1,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let collab_types::FileMutationOperationV1::MergeContentConflict {
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
            .filter(|participant| participant.role == FileMutationParticipantRoleV1::MergeSource)
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
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction or delta is invalid, incomplete,
    /// or inconsistent with the resulting projection.
    pub fn apply_staged_keep_both_delta(
        &mut self,
        transaction: &FileMutationTransactionV1,
        delta: &[u8],
    ) -> Result<(), TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let collab_types::FileMutationOperationV1::KeepBothContentConflict {
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
            .find(|participant| participant.role == FileMutationParticipantRoleV1::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRoleV1::KeepBothSibling)
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
        transaction: &FileMutationTransactionV1,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let collab_types::FileMutationOperationV1::KeepBothContentConflict {
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
            .find(|participant| participant.role == FileMutationParticipantRoleV1::KeepBothSource)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let sibling = transaction
            .manifest
            .participants
            .iter()
            .find(|participant| participant.role == FileMutationParticipantRoleV1::KeepBothSibling)
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

    #[allow(clippy::too_many_lines)]
    fn apply_replacement_transaction_in_place(
        &mut self,
        transaction: &FileMutationTransactionV1,
    ) -> Result<Option<Vec<u8>>, TransactionProjectionError> {
        transaction
            .verify()
            .map_err(|_| TransactionProjectionError::InvalidTransaction)?;
        let collab_types::FileMutationOperationV1::ReplacementRename {
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
            .find(|value| value.role == FileMutationParticipantRoleV1::Source)
            .ok_or(TransactionProjectionError::InvalidTransaction)?;
        let replaced_participant = transaction
            .manifest
            .participants
            .iter()
            .find(|value| value.role == FileMutationParticipantRoleV1::Replaced)
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
        let matches = self
            .live_node_ids()
            .into_iter()
            .filter(|id| self.record_id_for_id(id) == Some(record_id))
            .collect::<Vec<_>>();
        if self.content_authority == ContentAuthority::SignedRecords && matches.len() != 1 {
            return None;
        }
        matches
            .into_iter()
            .next()
            .and_then(|id| self.id_to_ino.get(&id).copied())
    }

    /// Resolve one live stable record while lazily assigning its process-local
    /// inode. Namespace snapshots intentionally do not persist inode maps, so
    /// authenticated startup reconciliation must use this before any FUSE
    /// lookup has warmed the cache. Ambiguous stable identities fail closed.
    pub(crate) fn ensure_ino_for_record_id(&mut self, record_id: FileRecordId) -> Option<u64> {
        if self.record_id_for(ROOT_INO) == Some(record_id) {
            return Some(ROOT_INO);
        }
        let mut matches = self
            .live_node_ids()
            .into_iter()
            .filter(|id| self.record_id_for_id(id) == Some(record_id));
        let only = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(self.ino_for_id(&only))
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
        let mut cur = id.to_string();
        let mut parts: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        loop {
            if cur == ROOT_ID {
                break;
            }
            if cur == TRASH_ID || !seen.insert(cur.clone()) {
                return None;
            }
            parts.push(escape_path_name(self.name_of(&cur)?.as_bytes()));
            cur = self.parent_of(&cur)?;
        }
        parts.reverse();
        Some(format!("/{}", parts.join("/")))
    }

    /// Every live path whose file content or generated thumbnail has `hash`.
    ///
    /// A content hash is not an authority token: peers can learn hashes from
    /// logs, caches, or another file with identical bytes. The blob provider
    /// uses this reverse lookup and then asks the ACL about the returned paths.
    /// Trashed nodes are omitted, so deleting the last readable reference also
    /// stops authorising future fetches of its old content.
    pub fn live_paths_for_blob(&self, hash: Hash) -> Vec<String> {
        self.live_node_ids()
            .into_iter()
            .filter(|id| {
                self.node_hash(id) == Some(hash) || self.thumbnail_for_id(id) == Some(hash)
            })
            .filter_map(|id| self.path_for_id(&id))
            .collect()
    }

    /// Bounded direct reverse lookup for manifest roots and thumbnails.
    /// Returns stable record IDs only—never paths, names, or sibling metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the scan limit is invalid or the bounded live
    /// namespace cannot be represented as a complete record graph.
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
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    pub fn getattr(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?.clone();
        if id == ROOT_ID {
            return Some(Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        if !self.reaches_root(&id) {
            return None;
        }
        let e = self.entry(&id)?;
        Some(Self::attr_for(ino, e.kind, e.size, e.mtime))
    }

    /// Attribute lookup for a kernel file handle whose directory entry has
    /// since been unlinked or replaced. Path lookup must continue using
    /// [`Self::getattr`]; only an already-open handle may reach this view.
    pub(crate) fn getattr_including_unlinked(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?.clone();
        if id == ROOT_ID {
            return Some(Self::attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        let entry = self.entry(&id)?;
        Some(Self::attr_for(ino, entry.kind, entry.size, entry.mtime))
    }

    pub fn lookup(&mut self, parent_ino: u64, name: impl AsRef<[u8]>) -> Option<FileAttr> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        let e = self.entry(&child)?;
        let ino = self.ino_for_id(&child);
        Some(Self::attr_for(ino, e.kind, e.size, e.mtime))
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
        self.entry(id)?.hash
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
        let name = name.as_ref();
        let id = self.gen_id();
        let obj = self.doc.put_object(&self.nodes, &id, ObjType::Map).ok()?;
        let _ = self.doc.put(&obj, "parent", parent_id);
        let _ = self.doc.put(&obj, "prev_parent", parent_id);
        let _ = self.doc.put(&obj, "name", name.to_vec());
        let _ = self.doc.put(&obj, "kind", kind.as_str());
        if let Some(h) = hash {
            let _ = self.doc.put(&obj, "hash", hash_to_hex(h));
        }
        let _ = self.doc.put(&obj, "size", size);
        let _ = self.doc.put(&obj, "mtime", mtime);
        let _ = self.doc.put(&obj, "author", self.author.clone());
        let record_id = FileRecordId::new();
        let _ = self.doc.put(&obj, RECORD_ID_KEY, record_id.0.to_string());
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
        Some(self.ino_for_id(&id))
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
        qdoc::model::init(&mut self.doc, &root);
        Some(self.ino_for_id(&id))
    }

    pub fn document_codec(&self, ino: u64) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        read_str(&self.doc, &self.node(id)?, "codec")
    }

    fn document_root(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.node(id)?, "doc") {
            Ok(Some((Value::Object(ObjType::Map), root))) => Some(root),
            _ => None,
        }
    }

    pub fn export_document(&self, ino: u64, registry: &qdoc::Registry) -> Option<Vec<u8>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let codec = read_str(&self.doc, &self.node(&id)?, "codec")?;
        let provider = registry.provider_for_ext(&codec)?;
        let root = self.document_root(&id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        provider
            .export(qdoc::DocRef {
                doc: &self.doc,
                c: &containers,
            })
            .ok()
    }

    /// Replace a document's content by importing `bytes` (whole-file write).
    /// Resets the embedded CRDT; use the in-place edits below to preserve merge history.
    pub fn import_document(
        &mut self,
        ino: u64,
        registry: &qdoc::Registry,
        bytes: &[u8],
        mtime: u64,
    ) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        let Some(node) = self.node(&id) else {
            return false;
        };
        let Some(codec) = read_str(&self.doc, &node, "codec") else {
            return false;
        };
        let Some(provider) = registry.provider_for_ext(&codec) else {
            return false;
        };
        // Parse into a throwaway document first — a malformed input must never destroy existing content.
        let mut probe = qdoc::Document::new();
        if provider.import(bytes, &mut probe.as_mut()).is_err() {
            return false;
        }
        let _ = self.doc.delete(&node, "doc");
        let Ok(root) = self.doc.put_object(&node, "doc", ObjType::Map) else {
            return false;
        };
        qdoc::model::init(&mut self.doc, &root);
        let Ok(containers) = qdoc::model::open(&self.doc, &root) else {
            return false;
        };
        let mut ids = qdoc::model::seeded_id_gen(&self.doc, &containers);
        {
            let mut dmut = qdoc::DocMut {
                doc: &mut self.doc,
                c: &containers,
                ids: &mut ids,
            };
            let _ = provider.import(bytes, &mut dmut);
        }
        self.set_field(&id, "mtime", mtime);
        true
    }

    /// Current heads of the document — a cheap change token (e.g. to invalidate a derived export cache).
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    // Fine-grained document edits: the path that merges semantically.

    fn with_doc_mut<R>(&mut self, ino: u64, f: impl FnOnce(&mut qdoc::DocMut) -> R) -> Option<R> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let root = self.document_root(&id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        let mut ids = qdoc::model::seeded_id_gen(&self.doc, &containers);
        let mut dmut = qdoc::DocMut {
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

    pub fn document_blocks(&self, ino: u64) -> Vec<String> {
        let Some(id) = self.ino_to_id.get(&ino) else {
            return Vec::new();
        };
        let Some(root) = self.document_root(id) else {
            return Vec::new();
        };
        match qdoc::model::open(&self.doc, &root) {
            Ok(c) => qdoc::model::blocks_in_order(&self.doc, &c),
            Err(_) => Vec::new(),
        }
    }

    pub fn document_block_text(&self, ino: u64, block_id: &str) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        qdoc::model::block_text(&self.doc, &containers, block_id)
    }

    pub fn document_remove_block(&mut self, ino: u64, block_id: &str) -> bool {
        self.with_doc_mut(ino, |d| d.remove_block(block_id))
            .is_some()
    }

    pub fn document_snapshot(&self, ino: u64) -> Option<qdoc::DocSnapshot> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        Some(
            qdoc::DocRef {
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
        read_str(&self.doc, &self.node(id)?, "thumb").and_then(|h| hash_from_hex(&h))
    }

    fn thumbnail_for_id(&self, id: &str) -> Option<Hash> {
        if self.content_authority == ContentAuthority::SignedRecords {
            let record_id = self.record_id_for_id(id)?;
            if self
                .live_node_ids()
                .into_iter()
                .filter(|candidate| self.record_id_for_id(candidate) == Some(record_id))
                .count()
                != 1
            {
                return None;
            }
            return self
                .verified_records
                .get(&record_id)
                .and_then(|projection| projection.thumbnail);
        }
        self.node(id)
            .and_then(|obj| read_str(&self.doc, &obj, "thumb"))
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
        self.set_field(&id, "prev_parent", cur);
        self.set_field(&id, "parent", TRASH_ID);
        true
    }

    pub fn directly_trashed(&self) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.parent_of(id).as_deref() == Some(TRASH_ID))
            .collect()
    }

    pub fn deleted_hash(&self, id: &str) -> Option<Hash> {
        read_str(&self.doc, &self.node(id)?, "deleted_hash").and_then(|h| hash_from_hex(&h))
    }

    pub fn prev_parent(&self, id: &str) -> Option<String> {
        read_str(&self.doc, &self.node(id)?, "prev_parent")
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
        self.set_field(&id, "prev_parent", cur);
        self.set_field(&id, "parent", &*new_parent_id);
        self.set_field(&id, "name", new_name.to_vec());
        self.set_field(&id, "mtime", mtime);
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
                .and_then(|o| read_str(&self.doc, &o, "prev_parent"))
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
mod tests {
    use super::*;

    fn h(b: u8) -> Hash {
        Hash::from([b; 32])
    }

    // This end-to-end scenario intentionally exercises every durable phase of
    // a two-record replacement transaction and its idempotent replay.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn root_is_inode_one_and_listable() {
        let mut ns = Namespace::new("a").unwrap();
        assert_eq!(ns.id_for_ino(ROOT_INO), Some(ROOT_ID));
        assert!(ns.record_id_for(ROOT_INO).is_some());
        assert_eq!(ns.getattr(ROOT_INO).unwrap().kind, FileType::Directory);
        assert_eq!(ns.readdir(ROOT_INO).unwrap().len(), 2); // "." ".."
    }

    #[test]
    fn verified_bindings_move_by_stable_parent_and_never_replace_a_sibling() {
        let mut ns = Namespace::new("a").unwrap();
        let left = ns.create_dir(ROOT_INO, "left", 1).unwrap();
        let right = ns.create_dir(ROOT_INO, "right", 1).unwrap();
        let file = ns
            .create_file(left, "before.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let occupied = ns
            .create_file(right, "occupied.txt", Some(&h(2)), 1, 1)
            .unwrap();
        let actor = FileActor {
            person: collab_types::Ed25519PublicKey([3; 32]),
            device: collab_types::Ed25519PublicKey([4; 32]),
        };
        let binding = FileBindingV1 {
            schema_version: FILE_BINDING_VERSION,
            record_id: ns.record_id_for(file).unwrap(),
            parent: Some(ns.record_id_for(right).unwrap()),
            name: "after.txt".into(),
            kind: FileNodeKind::File,
            tombstoned: false,
            actor,
            observed_at_ms: 2,
        };
        assert_eq!(ns.apply_signed_binding(&binding), Ok(true));
        assert_eq!(ns.lookup(right, "after.txt").unwrap().ino.0, file);
        assert_eq!(ns.apply_signed_binding(&binding), Ok(false));

        let occupied_binding = FileBindingV1 {
            name: "occupied.txt".into(),
            ..binding.clone()
        };
        assert_eq!(
            ns.apply_signed_binding(&occupied_binding),
            Err(BindingProjectionError::OccupiedDestination)
        );
        assert_eq!(ns.lookup(right, "occupied.txt").unwrap().ino.0, occupied);

        let deleted = FileBindingV1 {
            tombstoned: true,
            ..binding
        };
        assert_eq!(ns.apply_signed_binding(&deleted), Ok(true));
        assert_eq!(ns.apply_signed_binding(&deleted), Ok(false));
        assert!(ns.lookup(right, "after.txt").is_none());
    }

    // This scenario intentionally covers every durable phase and replay of a
    // two-record replacement transaction in one linear assertion trace.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn complete_replacement_transaction_moves_source_tombstones_victim_and_replays() {
        use collab_types::{
            ContentHash, FILE_MUTATION_TRANSACTION_VERSION, FileMutationOperationV1,
            FileMutationParticipantV1, FileMutationTransactionManifestV1, FileProvenance,
            FileVersionId, FileVersionV1,
        };
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let mut ns = Namespace::new("a").unwrap();
        let source_ino = ns
            .create_file(ROOT_INO, "source.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let victim_ino = ns
            .create_file(ROOT_INO, "destination.txt", Some(&h(2)), 1, 1)
            .unwrap();
        let source_record = ns.record_id_for(source_ino).unwrap();
        let victim_record = ns.record_id_for(victim_ino).unwrap();
        let root_record = ns.record_id_for(ROOT_INO).unwrap();
        let source_expected = FileVersionId::new();
        let victim_expected = FileVersionId::new();
        for (ino, record_id, version_id, byte) in [
            (source_ino, source_record, source_expected, 1_u8),
            (victim_ino, victim_record, victim_expected, 2_u8),
        ] {
            assert!(ns.set_content_revision(
                ino,
                &ContentRevisionV1 {
                    schema_version: collab_types::CONTENT_REVISION_VERSION,
                    record_id,
                    version_id,
                    content_hash: Some(ContentHash([byte; 32])),
                    byte_len: 1,
                    metadata_hash: ContentHash([byte + 2; 32]),
                },
                1,
            ));
        }
        let key = SigningKey::generate(&mut OsRng);
        let actor = FileActor {
            person: collab_types::Ed25519PublicKey([9; 32]),
            device: collab_types::Ed25519PublicKey(key.verifying_key().to_bytes()),
        };
        let participant = |record_id, expected_version, role, content_byte| {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id,
                parents: vec![expected_version],
                actor,
                timestamp_ms: 2,
                content_hash: Some(ContentHash([content_byte; 32])),
                metadata_hash: ContentHash([content_byte + 2; 32]),
                provenance: match role {
                    FileMutationParticipantRoleV1::Source => FileProvenance::Rename,
                    FileMutationParticipantRoleV1::Replaced => FileProvenance::Delete,
                    _ => unreachable!("replacement fixture role"),
                },
            };
            let mut participant = FileMutationParticipantV1 {
                record_id,
                role,
                expected_version,
                proposed_binding: FileBindingV1 {
                    schema_version: FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(root_record),
                    name: "destination.txt".into(),
                    kind: FileNodeKind::File,
                    tombstoned: role == FileMutationParticipantRoleV1::Replaced,
                    actor,
                    observed_at_ms: 2,
                },
                proposed_content_revision: Some(ContentRevisionV1 {
                    schema_version: collab_types::CONTENT_REVISION_VERSION,
                    record_id,
                    version_id: version.id,
                    content_hash: version.content_hash,
                    byte_len: 1,
                    metadata_hash: version.metadata_hash,
                }),
                proposed_version: version,
                projection_hash: ContentHash([0; 32]),
            };
            participant.projection_hash = participant.computed_projection_hash().unwrap();
            participant
        };
        let mut participants = vec![
            participant(
                source_record,
                source_expected,
                FileMutationParticipantRoleV1::Source,
                1,
            ),
            participant(
                victim_record,
                victim_expected,
                FileMutationParticipantRoleV1::Replaced,
                2,
            ),
        ];
        participants.sort_by_key(|participant| participant.record_id);
        let transaction = collab_types::FileMutationTransactionV1::signed(
            FileMutationTransactionManifestV1 {
                schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                transaction_id: Uuid::new_v4(),
                workspace_id: Uuid::new_v4(),
                actor,
                timestamp_ms: 2,
                operation: FileMutationOperationV1::ReplacementRename {
                    source_parent: root_record,
                    source_name: "source.txt".into(),
                    destination_parent: root_record,
                    destination_name: "destination.txt".into(),
                    source_record,
                    replaced_record: victim_record,
                },
                participants,
            },
            &key,
        )
        .unwrap();
        let delta = ns
            .apply_replacement_transaction(&transaction)
            .unwrap()
            .unwrap();
        assert!(!delta.is_empty());
        assert_eq!(
            ns.lookup(ROOT_INO, "destination.txt").unwrap().ino.0,
            source_ino
        );
        assert!(ns.lookup(ROOT_INO, "source.txt").is_none());
        assert!(ns.ino_for_record_id(victim_record).is_none());
        assert_eq!(ns.apply_replacement_transaction(&transaction), Ok(None));
    }

    #[test]
    fn record_ids_survive_save_reload_and_root_is_persisted() {
        let mut original = Namespace::new("a").unwrap();
        let file = original
            .create_file(ROOT_INO, "stable.txt", Some(&h(1)), 1, 1)
            .unwrap();
        let root_record = original.record_id_for(ROOT_INO).unwrap();
        let file_record = original.record_id_for(file).unwrap();
        assert_ne!(root_record, file_record);

        let bytes = original.save();
        let mut reloaded = Namespace::from_bytes("a", &bytes).unwrap();
        let reloaded_file = reloaded.lookup(ROOT_INO, "stable.txt").unwrap().ino.0;
        assert_eq!(reloaded.record_id_for(ROOT_INO), Some(root_record));
        assert_eq!(reloaded.record_id_for(reloaded_file), Some(file_record));
        assert_eq!(
            reloaded.entry_at("stable.txt").unwrap().record_id,
            file_record
        );
    }

    #[test]
    fn concurrent_content_revisions_never_expose_a_torn_hash_size_tuple() {
        let mut a = Namespace::new("a").unwrap();
        let ino = a
            .create_file(ROOT_INO, "atomic.bin", Some(&h(1)), 1, 1)
            .unwrap();
        let record_id = a.record_id_for(ino).unwrap();
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();
        let bino = b.lookup(ROOT_INO, "atomic.bin").unwrap().ino.0;
        let revision_a = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: collab_types::FileVersionId::new(),
            content_hash: Some(collab_types::ContentHash([10; 32])),
            byte_len: 10,
            metadata_hash: collab_types::ContentHash([11; 32]),
        };
        let revision_b = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: collab_types::FileVersionId::new(),
            content_hash: Some(collab_types::ContentHash([20; 32])),
            byte_len: 20,
            metadata_hash: collab_types::ContentHash([21; 32]),
        };
        assert!(a.set_content_revision(ino, &revision_a, 2));
        assert!(b.set_content_revision(bino, &revision_b, 2));
        a.merge_doc(&mut b).unwrap();

        let winner = a.content_revision_for(ino).unwrap();
        assert!(winner == revision_a || winner == revision_b);
        let entry = a.entry_at("atomic.bin").unwrap();
        assert_eq!(
            entry.hash,
            winner.content_hash.map(|hash| Hash::from(hash.0))
        );
        assert_eq!(entry.size, winner.byte_len);
        let id = a.id_for_ino(ino).unwrap();
        let conflicts = a.content_revision_conflicts(id);
        assert_eq!(conflicts.len(), 2);
        assert!(conflicts.iter().any(|(_, value)| value == &revision_a));
        assert!(conflicts.iter().any(|(_, value)| value == &revision_b));
    }

    #[test]
    fn legacy_content_write_explicitly_supersedes_atomic_revision() {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "compat.bin", Some(&h(1)), 1, 1)
            .unwrap();
        let revision = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id: ns.record_id_for(ino).unwrap(),
            version_id: collab_types::FileVersionId::new(),
            content_hash: None,
            byte_len: 0,
            metadata_hash: collab_types::ContentHash([7; 32]),
        };
        assert!(ns.set_content_revision(ino, &revision, 2));
        assert_eq!(ns.hash_for(ino), None, "None must not resurrect the mirror");
        assert!(ns.set_content(ino, Some(&h(3)), 3, 3));
        assert_eq!(ns.content_revision_for(ino), None);
        assert_eq!(ns.hash_for(ino), Some(h(3)));
        assert_eq!(ns.getattr(ino).unwrap().size, 3);
    }

    #[test]
    fn signed_mode_ignores_forged_namespace_content_and_thumbnail_fields() {
        let mut ns = Namespace::new("owner").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "verified.bin", Some(&h(1)), 1, 1)
            .unwrap();
        assert!(ns.set_thumb(ino, &h(2)));
        let record_id = ns.record_id_for(ino).unwrap();
        ns.enable_signed_content_authority();
        assert_eq!(ns.hash_for(ino), None);
        assert_eq!(ns.getattr(ino).unwrap().size, 0);
        assert_eq!(ns.thumb_hash(ino), None);
        assert!(ns.live_paths_for_blob(h(1)).is_empty());

        let revision = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: collab_types::FileVersionId::new(),
            content_hash: Some(collab_types::ContentHash([3; 32])),
            byte_len: 3,
            metadata_hash: collab_types::ContentHash([4; 32]),
        };
        assert!(ns.install_verified_content_revision(revision));
        assert!(ns.install_verified_thumbnail(record_id, Some(h(4))));

        let id = ns.id_for_ino(ino).unwrap().to_string();
        ns.set_field(&id, "hash", hash_to_hex(&h(9)));
        ns.set_field(&id, "size", 999_u64);
        ns.set_field(&id, "thumb", hash_to_hex(&h(9)));
        let mut forged = revision;
        forged.version_id = collab_types::FileVersionId::new();
        forged.content_hash = Some(collab_types::ContentHash([9; 32]));
        forged.byte_len = 999;
        ns.set_field(
            &id,
            CONTENT_REVISION_KEY,
            ScalarValue::Bytes(revision_to_bytes(&forged).unwrap()),
        );

        assert_eq!(ns.hash_for(ino), Some(h(3)));
        assert_eq!(ns.getattr(ino).unwrap().size, 3);
        assert_eq!(ns.thumb_hash(ino), Some(h(4)));
        assert_eq!(ns.content_revision_for(ino), Some(revision));
        assert!(ns.live_paths_for_blob(h(9)).is_empty());
        assert_eq!(ns.live_paths_for_blob(h(3)), vec!["/verified.bin"]);
        assert!(!ns.set_content(ino, Some(&h(8)), 8, 8));
        assert!(!ns.set_thumb(ino, &h(8)));
    }

    #[test]
    fn duplicate_live_record_bindings_fail_closed_in_signed_mode() {
        let mut ns = Namespace::new("owner").unwrap();
        let first = ns.create_file(ROOT_INO, "a", None, 0, 1).unwrap();
        let duplicate = ns.create_file(ROOT_INO, "b", None, 0, 1).unwrap();
        let record_id = ns.record_id_for(first).unwrap();
        let duplicate_id = ns.id_for_ino(duplicate).unwrap().to_string();
        let duplicate_obj = ns.node(&duplicate_id).unwrap();
        ns.doc
            .put(&duplicate_obj, RECORD_ID_KEY, record_id.0.to_string())
            .unwrap();
        ns.enable_signed_content_authority();
        let revision = ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: collab_types::FileVersionId::new(),
            content_hash: Some(collab_types::ContentHash([3; 32])),
            byte_len: 3,
            metadata_hash: collab_types::ContentHash([4; 32]),
        };
        assert!(!ns.install_verified_content_revision(revision));
        assert_eq!(ns.hash_for(first), None);
        assert_eq!(ns.hash_for(duplicate), None);
    }

    #[test]
    fn projected_file_uses_exact_remote_record_and_refuses_aliases() {
        let mut ns = Namespace::new("recipient").unwrap();
        let record = FileRecordId::new();
        let ino = ns
            .create_projected_file(ROOT_INO, "shared.png", record, Some(&h(8)), 42, 9)
            .unwrap();
        assert_eq!(ns.record_id_for(ino), Some(record));
        assert_eq!(ns.hash_for(ino), Some(h(8)));
        assert_eq!(ns.getattr(ino).unwrap().size, 42);
        assert!(
            ns.create_projected_file(ROOT_INO, "alias.png", record, None, 0, 0)
                .is_none()
        );
        assert!(
            ns.create_projected_file(ROOT_INO, "shared.png", FileRecordId::new(), None, 0, 0)
                .is_none()
        );
        assert!(
            ns.create_projected_file(ROOT_INO, "../escape", FileRecordId::new(), None, 0, 0)
                .is_none()
        );
    }

    #[test]
    fn legacy_snapshots_migrate_record_ids_deterministically_on_next_save() {
        let mut legacy = Namespace::new("legacy").unwrap();
        legacy
            .create_file(ROOT_INO, "old.txt", Some(&h(2)), 2, 1)
            .unwrap();
        let ids: Vec<_> = legacy.doc.keys(&legacy.nodes).collect();
        let _ = legacy.doc.delete(&ROOT, ROOT_RECORD_ID_KEY);
        for id in ids {
            let node = legacy.node(&id).unwrap();
            let _ = legacy.doc.delete(&node, RECORD_ID_KEY);
        }
        let legacy_bytes = legacy.doc.save();

        let mut first = Namespace::from_bytes("first", &legacy_bytes).unwrap();
        let mut second = Namespace::from_bytes("second", &legacy_bytes).unwrap();
        let first_ino = first.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        let second_ino = second.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        assert_eq!(
            first.record_id_for(ROOT_INO),
            second.record_id_for(ROOT_INO),
            "root migration must not depend on the loading peer"
        );
        assert_eq!(
            first.record_id_for(first_ino),
            second.record_id_for(second_ino),
            "node migration must not depend on the loading peer"
        );

        let expected_root = first.record_id_for(ROOT_INO).unwrap();
        let expected_file = first.record_id_for(first_ino).unwrap();
        let migrated_bytes = first.save();
        let mut persisted = Namespace::from_bytes("third", &migrated_bytes).unwrap();
        let persisted_ino = persisted.lookup(ROOT_INO, "old.txt").unwrap().ino.0;
        assert_eq!(persisted.record_id_for(ROOT_INO), Some(expected_root));
        assert_eq!(persisted.record_id_for(persisted_ino), Some(expected_file));
    }

    #[test]
    fn legacy_string_name_loads_exactly_and_next_rename_persists_bytes() {
        let mut legacy = Namespace::new("legacy-name").unwrap();
        let ino = legacy
            .create_file(ROOT_INO, "placeholder", None, 0, 1)
            .unwrap();
        let id = legacy.id_for_ino(ino).unwrap().to_string();
        let node = legacy.node(&id).unwrap();
        legacy
            .doc
            .put(&node, "name", ScalarValue::Str("café.txt".into()))
            .unwrap();

        let mut migrated = Namespace::from_bytes("migrated", &legacy.doc.save()).unwrap();
        assert!(migrated.lookup(ROOT_INO, "café.txt").is_some());
        assert!(migrated.rename(ROOT_INO, "café.txt", ROOT_INO, b"caf\xc3\xa9-final.txt", 2,));

        let node = migrated.node(&id).unwrap();
        assert!(matches!(
            migrated.doc.get(&node, "name").unwrap(),
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Bytes(bytes) if bytes.as_slice() == b"caf\xc3\xa9-final.txt")
        ));
        let mut reopened = Namespace::from_bytes("reopened", &migrated.save()).unwrap();
        assert!(
            reopened
                .lookup(ROOT_INO, b"caf\xc3\xa9-final.txt")
                .is_some(),
            "legacy UTF-8 identity must survive byte-scalar migration and restart"
        );
    }

    #[test]
    fn create_lookup_and_nested_readdir() {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns
            .create_file(ROOT_INO, "hello.txt", Some(&h(7)), 12, 1)
            .unwrap();
        let attr = ns.lookup(ROOT_INO, "hello.txt").unwrap();
        assert_eq!(attr.ino, INodeNo(ino));
        assert_eq!(attr.size, 12);
        assert_eq!(ns.hash_for(ino), Some(h(7)));

        let dir = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        ns.create_file(dir, "a.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(
            ns.readdir(dir)
                .unwrap()
                .into_iter()
                .any(|(_, _, name)| name == "a.txt")
        );
        let dotdot = ns
            .readdir(dir)
            .unwrap()
            .into_iter()
            .find(|(_, _, n)| n == "..")
            .unwrap();
        assert_eq!(dotdot.0, ROOT_INO);
    }

    #[test]
    fn byte_names_survive_save_lookup_and_rename_exactly() {
        let mut ns = Namespace::new("bytes").unwrap();
        let original = b"photo-\xFF.png";
        let renamed = b"photo-\xFE.png";
        let ino = ns.create_file(ROOT_INO, original, None, 0, 1).unwrap();
        assert_eq!(ns.lookup(ROOT_INO, original).unwrap().ino.0, ino);
        assert!(
            ns.readdir(ROOT_INO)
                .unwrap()
                .iter()
                .any(|entry| entry.2.as_bytes() == original)
        );

        let snapshot = ns.save();
        let mut reopened = Namespace::from_bytes("reopened", &snapshot).unwrap();
        let reopened_ino = reopened.lookup(ROOT_INO, original).unwrap().ino.0;
        let record_id = reopened.record_id_for(reopened_ino).unwrap();
        assert!(reopened.rename(ROOT_INO, original, ROOT_INO, renamed, 2));
        assert!(reopened.lookup(ROOT_INO, original).is_none());
        assert_eq!(
            reopened.lookup(ROOT_INO, renamed).unwrap().ino.0,
            reopened_ino
        );

        let renamed_snapshot = reopened.save();
        let mut final_open = Namespace::from_bytes("final", &renamed_snapshot).unwrap();
        assert!(final_open.lookup(ROOT_INO, original).is_none());
        let final_ino = final_open.lookup(ROOT_INO, renamed).unwrap().ino.0;
        assert_eq!(final_open.record_id_for(final_ino), Some(record_id));
    }

    #[test]
    fn byte_names_replicate_without_display_normalization() {
        let name = b"replicated-\xFF.bin";
        let mut source = Namespace::new("source").unwrap();
        source.create_file(ROOT_INO, name, None, 0, 1).unwrap();
        let snapshot = source.save();
        let mut peer = Namespace::from_bytes("peer", &snapshot).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_some());
        assert!(
            peer.readdir(ROOT_INO)
                .unwrap()
                .iter()
                .any(|entry| entry.2.as_bytes() == name)
        );

        let renamed = b"replicated-\xFE.bin";
        assert!(source.rename(ROOT_INO, name, ROOT_INO, renamed, 2));
        let delta = source.save_incremental();
        peer.apply_remote_and_resolve(&delta).unwrap();
        assert!(peer.lookup(ROOT_INO, name).is_none());
        assert!(peer.lookup(ROOT_INO, renamed).is_some());
    }

    #[test]
    fn remove_moves_subtree_to_trash() {
        let mut ns = Namespace::new("a").unwrap();
        let dir = ns.create_dir(ROOT_INO, "d", 1).unwrap();
        let child = ns.create_file(dir, "c.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(ns.remove(dir));
        assert!(ns.getattr(dir).is_none(), "removed dir not visible");
        assert!(
            ns.getattr(child).is_none(),
            "child follows parent into trash"
        );
        assert!(ns.lookup(ROOT_INO, "d").is_none());
    }

    #[test]
    fn rename_is_a_move_that_preserves_inode() {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_file(ROOT_INO, "old", Some(&h(9)), 4, 1).unwrap();
        let record_id = ns.record_id_for(ino).unwrap();
        let dir = ns.create_dir(ROOT_INO, "sub", 1).unwrap();
        assert!(ns.rename(ROOT_INO, "old", dir, "new", 2));
        assert!(ns.lookup(ROOT_INO, "old").is_none());
        let moved = ns.lookup(dir, "new").unwrap();
        assert_eq!(moved.ino, INodeNo(ino), "inode preserved across move");
        assert_eq!(
            ns.record_id_for(moved.ino.0),
            Some(record_id),
            "rename and move preserve collaborative identity"
        );
        assert_eq!(ns.hash_for(ino), Some(h(9)));
    }

    #[test]
    fn conflict_copy_gets_a_distinct_record_id() {
        let mut ns = Namespace::new("a").unwrap();
        let original = ns.create_file(ROOT_INO, "doc", Some(&h(1)), 1, 1).unwrap();
        let original_record = ns.record_id_for(original).unwrap();
        assert!(ns.create_conflict_copy(ROOT_ID, "doc.sync-conflict", &h(2), 1, 2));
        let copy = ns.resolve_path_ino("doc.sync-conflict").unwrap();
        assert_ne!(ns.record_id_for(copy), Some(original_record));
        assert!(ns.record_id_for(copy).is_some());
    }

    #[test]
    fn live_blob_paths_follow_rename_thumbnail_and_delete() {
        let mut ns = Namespace::new("a").unwrap();
        let docs = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        let ino = ns.create_file(docs, "old.png", Some(&h(1)), 4, 1).unwrap();
        assert!(ns.set_thumb(ino, &h(2)));

        assert_eq!(ns.live_paths_for_blob(h(1)), vec!["/docs/old.png"]);
        assert_eq!(ns.live_paths_for_blob(h(2)), vec!["/docs/old.png"]);

        assert!(ns.rename(docs, "old.png", ROOT_INO, "new.png", 2));
        assert_eq!(ns.live_paths_for_blob(h(1)), vec!["/new.png"]);
        assert_eq!(ns.live_paths_for_blob(h(2)), vec!["/new.png"]);

        assert!(ns.remove(ino));
        assert!(ns.live_paths_for_blob(h(1)).is_empty());
        assert!(ns.live_paths_for_blob(h(2)).is_empty());
    }

    #[test]
    fn direct_blob_lookup_returns_only_live_stable_records() {
        let mut ns = Namespace::new("a").unwrap();
        let first = ns
            .create_file(ROOT_INO, "first", Some(&h(1)), 4, 1)
            .unwrap();
        let second = ns
            .create_file(ROOT_INO, "second", Some(&h(9)), 4, 1)
            .unwrap();
        let first_record = ns.record_id_for(first).unwrap();
        let second_record = ns.record_id_for(second).unwrap();
        assert!(ns.set_thumb(second, &h(1)));
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 8).unwrap(),
            vec![first_record, second_record]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 1),
            Err(LiveRecordBlobScanError::LimitExceeded { limit: 1 })
        );

        assert!(ns.remove(first));
        assert_eq!(
            ns.live_record_ids_for_direct_blob(h(1), 8).unwrap(),
            vec![second_record]
        );
        assert!(
            ns.live_record_ids_for_direct_blob(h(77), 8)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rename_rejects_move_into_own_descendant() {
        let mut ns = Namespace::new("a").unwrap();
        let d1 = ns.create_dir(ROOT_INO, "d1", 1).unwrap();
        let d2 = ns.create_dir(d1, "d2", 1).unwrap();
        assert!(
            !ns.rename(ROOT_INO, "d1", d2, "d1", 2),
            "cannot move d1 under d1/d2"
        );
        assert!(ns.lookup(ROOT_INO, "d1").is_some());
    }

    #[test]
    fn disjoint_inserts_merge() {
        let mut a = Namespace::new("a").unwrap();
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();
        a.create_file(ROOT_INO, "from_a", Some(&h(1)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(2)), 1, 1)
            .unwrap();
        a.merge_doc(&mut b).unwrap();
        assert!(a.lookup(ROOT_INO, "from_a").is_some());
        assert!(a.lookup(ROOT_INO, "from_b").is_some());
    }

    #[test]
    fn concurrent_cross_move_resolves_acyclically_and_converges() {
        let mut a = Namespace::new("a").unwrap();
        let xino = a.create_dir(ROOT_INO, "X", 1).unwrap();
        let yino = a.create_dir(ROOT_INO, "Y", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        // A moves X under Y; B moves Y under X (concurrently).
        let a_y = a.lookup(ROOT_INO, "Y").unwrap().ino.0;
        a.rename(ROOT_INO, "X", a_y, "X", 5);
        let b_x = b.lookup(ROOT_INO, "X").unwrap().ino.0;
        b.rename(ROOT_INO, "Y", b_x, "Y", 5);
        let _ = (xino, yino);

        let mut a2 = Namespace::from_bytes("a", &a.save()).unwrap();
        let mut b2 = Namespace::from_bytes("b", &b.save()).unwrap();
        let _ = a2.merge_doc(&mut b2.clone_doc());
        let _ = b2.merge_doc(&mut a.clone_doc());
        a2.resolve();
        b2.resolve();

        assert!(
            a2.find_cycle().is_none(),
            "a side must be acyclic after resolve"
        );
        assert!(
            b2.find_cycle().is_none(),
            "b side must be acyclic after resolve"
        );

        let xid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some(b"X"));
        let yid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some(b"Y"));
        assert!(
            xid.is_some() && yid.is_some(),
            "both dirs survive resolution"
        );

        for id in a2.live_node_ids() {
            assert_eq!(
                a2.parent_of(&id),
                b2.parent_of(&id),
                "peers must converge on parent of {id}"
            );
        }
    }

    #[test]
    fn delete_dir_vs_create_child_converges_to_deleted() {
        let mut a = Namespace::new("a").unwrap();
        let dino = a.create_dir(ROOT_INO, "d", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        a.remove(dino);
        let b_d = b.lookup(ROOT_INO, "d").unwrap().ino.0;
        b.create_file(b_d, "new.txt", Some(&h(3)), 3, 1).unwrap();

        let _ = a.merge_doc(&mut b);
        a.resolve();

        assert!(a.lookup(ROOT_INO, "d").is_none(), "d stays deleted");
        let new_id = a
            .doc
            .keys(&a.nodes)
            .find(|i| a.name_of(i).as_deref() == Some(b"new.txt"));
        assert!(
            new_id.is_some(),
            "the node still exists in the doc (not lost)"
        );
        assert!(
            !a.reaches_root(&new_id.unwrap()),
            "but it is not reachable from root (consistently trashed, not orphaned)"
        );
    }

    #[test]
    fn tree_survives_save_and_reload() {
        let mut a = Namespace::new("a").unwrap();
        let dir = a.create_dir(ROOT_INO, "docs", 1).unwrap();
        a.create_file(dir, "f.txt", Some(&h(5)), 5, 1).unwrap();
        let bytes = a.save();

        let mut b = Namespace::from_bytes("a", &bytes).unwrap();
        let d = b.lookup(ROOT_INO, "docs").expect("dir restored");
        let f = b.lookup(d.ino.0, "f.txt").expect("nested file restored");
        assert_eq!(b.hash_for(f.ino.0), Some(h(5)));
    }

    #[test]
    fn incremental_apply_and_resolve() {
        let mut a = Namespace::new("a").unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let _ = a.save_incremental();
        a.create_file(ROOT_INO, "delta.txt", Some(&h(4)), 7, 1)
            .unwrap();
        let delta = a.save_incremental();
        b.apply_remote_and_resolve(&delta).unwrap();
        assert_eq!(b.lookup(ROOT_INO, "delta.txt").unwrap().size, 7);
    }

    #[test]
    fn sync_protocol_converges_two_namespaces() {
        // Diverge from a shared base doc; the incremental sync ping-pong must converge them.
        let mut a = Namespace::new("a").unwrap();
        a.create_file(ROOT_INO, "base", Some(&h(1)), 1, 1).unwrap();
        let mut b = Namespace::from_bytes("b", &a.save()).unwrap();

        a.create_file(ROOT_INO, "from_a", Some(&h(2)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(3)), 1, 1)
            .unwrap();

        let mut sa = automerge::sync::State::new();
        let mut sb = automerge::sync::State::new();
        for _ in 0..20 {
            let ma = a.sync_generate(&mut sa);
            if let Some(m) = &ma {
                b.sync_receive(&mut sb, m).unwrap();
            }
            let mb = b.sync_generate(&mut sb);
            if let Some(m) = &mb {
                a.sync_receive(&mut sa, m).unwrap();
            }
            if ma.is_none() && mb.is_none() {
                break;
            }
        }
        assert!(a.lookup(ROOT_INO, "from_b").is_some(), "a got b's change");
        assert!(b.lookup(ROOT_INO, "from_a").is_some(), "b got a's change");
    }

    #[test]
    fn document_import_export_roundtrips() {
        let reg = qdoc::builtin_registry();
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_document(ROOT_INO, "note.md", "md", 1).unwrap();
        assert_eq!(ns.document_codec(ino).as_deref(), Some("md"));

        let md = b"# Title\n\nHello world\n";
        assert!(ns.import_document(ino, &reg, md, 2));
        let out = ns.export_document(ino, &reg).expect("export");
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("Title") && s.contains("Hello world"),
            "round-trip lost content: {s}"
        );
    }

    #[test]
    fn failed_import_preserves_existing_document() {
        let reg = qdoc::builtin_registry();
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        assert!(ns.import_document(ino, &reg, b"# Keep\n\nimportant\n", 2));
        let before = ns.export_document(ino, &reg).unwrap();

        // invalid UTF-8 is rejected by the markdown codec
        assert!(
            !ns.import_document(ino, &reg, &[0xff, 0xfe, 0x00], 3),
            "malformed import must be rejected"
        );
        let after = ns.export_document(ino, &reg).unwrap();
        assert_eq!(
            before, after,
            "existing document must survive a rejected import"
        );
    }

    #[test]
    fn document_concurrent_edits_merge_semantically() {
        // The point of model B: concurrent edits to the same document merge at the text level, not a conflict copy.
        let mut a = Namespace::new("a").unwrap();
        let ino = a.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        let bid = a.document_insert_block(ino, None, "paragraph").unwrap();
        a.document_splice(ino, &bid, 0, 0, "Hello world");

        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc.md").unwrap().ino.0;

        // concurrent in-place edits to the same paragraph
        a.document_splice(ino, &bid, 5, 0, " dear");
        b.document_splice(bino, &bid, 11, 0, " (B)");

        a.merge_doc(&mut b).unwrap();
        let text = a.document_block_text(ino, &bid).unwrap();
        assert!(text.contains("dear"), "A's edit must survive: {text}");
        assert!(
            text.contains("(B)"),
            "B's edit must survive (text-level merge): {text}"
        );
    }

    // test-only helper to merge using a fresh loaded copy
    impl Namespace {
        fn clone_doc(&mut self) -> Namespace {
            Namespace::from_bytes(self.author.clone(), &self.save()).unwrap()
        }
    }
}
