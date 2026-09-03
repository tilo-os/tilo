//! The FUSE operations — and the filtered view they serve.
//!
//! # A denial is presented as an absence
//!
//! The access decision lives in [`crate::acl`]; this module decides how
//! a denial *looks*. It looks like nothing: a path outside the view is
//! `ENOENT` with no `readdir` entry; a path in the view but not writable
//! is `EROFS`; a path in the view and readable is the real thing.
//!
//! `EACCES` is never returned for a policy decision — it's the errno
//! most software handles by aborting, while "no such file" and
//! "read-only filesystem" are states programs already handle correctly.
//! `readdir` omits hidden entries rather than listing-then-failing them;
//! `lookup` does the *same work* (namespace consult + view check)
//! whether or not a name exists, so there's no fast path whose absence
//! is a signal; a directory's `nlink` is computed from the filtered
//! listing, never the real tree, so it can't announce hidden subdirectories.
//!
//! ## Where the illusion has an edge, and why it is not reachable
//!
//! Creating a name that already exists but is outside the view answers
//! `EROFS` rather than creating it — the alternative is writing over
//! state the app can't see. The capability model can't produce that
//! situation: a `Restrict` is a ceiling and every verb implies `Read`,
//! so no capability can hide a name inside a directory it left writable.
//! Reaching the edge needs a direct [`crate::acl::AclEntry`] with
//! `write` but not `read`, which no production caller writes. The same
//! reasoning covers `rmdir` on a directory whose only remaining children
//! are hidden: `EROFS`, not `ENOTEMPTY`.
use fuser::{
    BsdFileFlags, Errno, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    KernelConfig, LockOwner, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request,
    TimeOrNow, WriteFlags,
};
use libc::{EBUSY, EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;
use serde::Serialize;

use crate::acl::{Access, AclHandle};
use crate::availability::{
    AvailabilityPolicy, AvailabilityRegistry, RecordAvailability, RecordKind, Residency,
    TransferPhase,
};
use crate::eviction::{
    EvictionCatalog, EvictionMode, EvictionQueue, EvictionRequestId, EvictionRequestState,
};
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;
use crate::sync::SyncHandle;

const TTL: Duration = Duration::from_secs(10);
// Namespace changes normally wake the worker directly.  This is solely a
// repair path for changes received outside the FUSE callbacks (and for a
// process that died between persisting a pin and scheduling it).
const PINNED_HYDRATION_INTERVAL: Duration = Duration::from_secs(5);
const MAX_PINNED_FETCHES_PER_PASS: usize = 4;
const HYDRATION_PROGRESS_PERSIST_BYTES: u64 = 8 * 1024 * 1024;

type DocExports = HashMap<u64, (Vec<automerge::ChangeHash>, Vec<u8>)>;

#[derive(Serialize)]
struct VersionMetadataDigest {
    path: Option<String>,
    kind: u8,
    byte_len: u64,
    deleted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct VersionSnapshot {
    record_id: collab_types::FileRecordId,
    content_hash: Option<collab_types::ContentHash>,
    metadata_hash: collab_types::ContentHash,
    byte_len: u64,
    kind: FileType,
}

fn keep_both_uuid(
    domain: &[u8],
    workspace_id: uuid::Uuid,
    source_record: collab_types::FileRecordId,
    conflict_id: uuid::Uuid,
    selected_version: collab_types::FileVersionId,
    actor: collab_types::FileActor,
) -> uuid::Uuid {
    let mut hash = blake3::Hasher::new();
    hash.update(b"qos.collaborative-fs.keep-both.v2\0");
    hash.update(domain);
    hash.update(workspace_id.as_bytes());
    hash.update(source_record.0.as_bytes());
    hash.update(conflict_id.as_bytes());
    hash.update(selected_version.0.as_bytes());
    hash.update(actor.person.as_bytes());
    hash.update(actor.device.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn merge_uuid(
    domain: &[u8],
    workspace_id: uuid::Uuid,
    source_record: collab_types::FileRecordId,
    conflict_id: uuid::Uuid,
    versions: &[collab_types::FileVersionId],
    actor: collab_types::FileActor,
) -> uuid::Uuid {
    let mut versions = versions.to_vec();
    versions.sort_unstable();
    let mut hash = blake3::Hasher::new();
    hash.update(b"qos.collaborative-fs.merge.v2\0");
    hash.update(domain);
    hash.update(workspace_id.as_bytes());
    hash.update(source_record.0.as_bytes());
    hash.update(conflict_id.as_bytes());
    for version in versions {
        hash.update(version.0.as_bytes());
    }
    hash.update(actor.person.as_bytes());
    hash.update(actor.device.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn keep_both_name(
    source_name: impl AsRef<[u8]>,
    target_record: collab_types::FileRecordId,
) -> collab_types::FileNameV1 {
    let source_name = source_name.as_ref();
    let simple = target_record.0.simple().to_string();
    let suffix = format!(".keep-both-{}", simple.get(..12).unwrap_or(&simple));
    let max_prefix = collab_types::files::MAX_FILE_NAME_BYTES.saturating_sub(suffix.len());
    let prefix_len = source_name.len().min(max_prefix);
    let mut name = source_name
        .get(..prefix_len)
        .unwrap_or(source_name)
        .to_vec();
    name.extend_from_slice(suffix.as_bytes());
    name.into()
}

#[cfg(test)]
mod exact_conflict_name_tests {
    use super::*;

    #[test]
    fn keep_both_suffix_preserves_invalid_utf8_and_bounds_the_result() {
        let record = collab_types::FileRecordId(uuid::Uuid::from_u128(7));
        let original = [b'f', 0xff, b'o'];
        let name = keep_both_name(original, record);
        assert!(name.as_bytes().starts_with(&original));
        assert!(name.as_bytes().ends_with(b".keep-both-000000000000"));
        assert!(name.as_bytes().len() <= collab_types::MAX_FILE_NAME_BYTES);

        let long = keep_both_name(vec![0xfe; collab_types::MAX_FILE_NAME_BYTES * 2], record);
        assert_eq!(long.as_bytes().len(), collab_types::MAX_FILE_NAME_BYTES);
        assert!(long.as_bytes().ends_with(b".keep-both-000000000000"));
    }
}

struct ReplacementTransactionRuntime {
    journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
    namespace_path: std::path::PathBuf,
}

#[derive(Clone, Debug)]
struct OpenFileHandleState {
    ino: u64,
    /// Authorization path at open time. Once replacement rename moves this
    /// inode to trash there is intentionally no live path to recompute.
    opened_path: String,
    writable: bool,
}

struct PinnedHydrationTask {
    /// A watch channel deliberately retains only the latest generation: a
    /// burst of writes/imports must result in one reconciliation, not one
    /// full namespace walk per event.
    wake: tokio::sync::watch::Sender<u64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PinnedHydrationTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct PinnedCandidate {
    record_id: collab_types::FileRecordId,
    parent: Option<collab_types::FileRecordId>,
    kind: RecordKind,
    version: Option<collab_types::FileVersionId>,
    hash: Option<Hash>,
    size: u64,
    path: String,
}

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
    /// Codecs for recognized rich-text formats (odt/docx/md).
    registry: Arc<qdoc::Registry>,
    /// Broadcasts local CRDT deltas to peers (None when running standalone).
    sync: Option<Arc<SyncHandle>>,
    /// Per-inode in-flight write buffers (content-addressed on flush/release).
    write_buffers: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    open_file_handles: Mutex<HashMap<u64, OpenFileHandleState>>,
    next_file_handle: AtomicU64,
    /// Cached serialized form of each document inode, keyed by the doc's heads,
    /// so a multi-chunk read exports once (and getattr's size stays current).
    doc_exports: Mutex<DocExports>,
    /// Rich facts extracted only while current-version bytes are already local.
    /// Replicated peers consume these facts without hydrating the payload.
    rich_metadata: Mutex<
        HashMap<
            collab_types::FileRecordId,
            (collab_types::ContentHash, collab_types::FilePropertiesV1),
        >,
    >,
    /// The workspace ACL and the identity it is evaluated as — the real
    /// access-control check. `None` keeps Unix-veneer-only behaviour;
    /// `Some` checks every open/read/write against the ACL as that identity.
    /// One field so an ACL can never be attached without the identity it
    /// answers for. A *handle*, not the ACL itself, so the policy can be
    /// replaced while the mount is up — see [`crate::CapabilityFile`].
    enforcement: Option<(AclHandle, crate::acl::IdentityKey)>,
    /// Host-side thumbnail generation + cache. `Some` when the mount has a
    /// persistent cache directory (serves previews via `user.qos.thumb` and
    /// gossips the hash); `None` leaves the mount thumbnail-less.
    thumbs: Option<crate::thumbs::ThumbProvider>,
    /// Durable local pin/replica/transfer registry. The FUSE path only reads
    /// its in-memory state; persistence happens at mutation points.
    availability: Option<Arc<Mutex<AvailabilityRegistry>>>,
    /// One bounded worker owns automatic hydration for durable file/folder
    /// pins. It is woken by policy changes and also reconciles periodically so
    /// later descendants and later immutable versions inherit the promise.
    pinned_hydration: Option<PinnedHydrationTask>,
    eviction_queue: Option<Arc<tokio::sync::Mutex<EvictionQueue>>>,
    /// Durable signed-grant authority used for the bounded read-only share
    /// projection. The FUSE callback never holds or reconstructs link secrets.
    share_authority: Option<Arc<crate::share_authority::ShareAuthority>>,
    /// Durable immutable version/conflict history and the certified local
    /// actor attributed to mutations made through this mount.
    history: Option<(
        Arc<crate::history::FileHistoryStore>,
        collab_types::FileActor,
    )>,
    /// Signed exact-record projection used by partial-share network sessions.
    record_publisher: Option<Arc<crate::record_publisher::RecordPublisher>>,
    replacement_transactions: Option<ReplacementTransactionRuntime>,
    /// Explicit one-time owner migration permission. Joining peers disable
    /// this so unsigned legacy namespace bytes are never automatically
    /// blessed into their certified actor's signed history.
    allow_legacy_record_migration: bool,
    share_control_path: Option<std::path::PathBuf>,
}

impl QuickFS {
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
    ) -> Self {
        Self::with_registry(
            namespace,
            hydrator,
            heuristics,
            Arc::new(qdoc::builtin_registry()),
        )
    }

    pub fn with_registry(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
        registry: Arc<qdoc::Registry>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            heuristics,
            registry,
            sync: None,
            write_buffers: Arc::new(Mutex::new(HashMap::new())),
            open_file_handles: Mutex::new(HashMap::new()),
            next_file_handle: AtomicU64::new(1),
            doc_exports: Mutex::new(HashMap::new()),
            rich_metadata: Mutex::new(HashMap::new()),
            enforcement: None,
            thumbs: None,
            availability: None,
            pinned_hydration: None,
            eviction_queue: None,
            share_authority: None,
            history: None,
            record_publisher: None,
            replacement_transactions: None,
            allow_legacy_record_migration: true,
            share_control_path: None,
        }
    }

    #[must_use]
    pub fn without_legacy_record_migration(mut self) -> Self {
        self.allow_legacy_record_migration = false;
        self
    }

    /// Shared dirty-inode view for the storage authority.
    pub fn write_buffers_handle(&self) -> Arc<Mutex<HashMap<u64, Vec<u8>>>> {
        Arc::clone(&self.write_buffers)
    }

    fn ns(&self) -> std::sync::MutexGuard<'_, Namespace> {
        self.namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn buffers(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Vec<u8>>> {
        self.write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn doc_export_cache(&self) -> std::sync::MutexGuard<'_, DocExports> {
        self.doc_exports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The codec key for `name`'s extension if it's a recognized document.
    fn document_codec_for(&self, name: &str) -> Option<String> {
        let ext = name.rsplit('.').next().filter(|e| *e != name)?;
        self.registry
            .provider_for_ext(ext)
            .map(|_| ext.to_lowercase())
    }

    /// Whether `ino` is a CRDT-backed document (vs. opaque content).
    fn is_document(&self, ino: u64) -> bool {
        let namespace = self.ns();
        namespace.content_authority() == crate::namespace::ContentAuthority::LegacyNamespace
            && namespace.document_codec(ino).is_some()
    }

    /// The hydration state of `ino`, served for the `user.qos.sync` xattr.
    /// `None` when the inode doesn't exist. Directories, CRDT documents and
    /// empty files are always [`SyncState::Local`]; a regular file's state
    /// comes from the hydrator (is every chunk in the cache?).
    fn sync_state_for(&self, ino: u64) -> Option<crate::syncstate::SyncState> {
        use crate::syncstate::SyncState;
        let (exists, is_dir, is_doc, hash) = {
            let ns = self.ns();
            let attr = ns.getattr(ino);
            let exists = attr.is_some();
            let is_dir = matches!(attr, Some(a) if a.kind == FileType::Directory);
            let is_doc = ns.document_codec(ino).is_some();
            let hash = ns.hash_for(ino);
            drop(ns);
            (exists, is_dir, is_doc, hash)
        };
        if !exists {
            return None;
        }
        // A file open for writing has its bytes staged locally in the buffer.
        if self.buffers().contains_key(&ino) {
            return Some(SyncState::Local);
        }
        if is_dir || is_doc {
            return Some(SyncState::Local);
        }
        let Some(hash) = hash else {
            // A hashless (empty) file is trivially local.
            return Some(SyncState::Local);
        };
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        let state = tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.sync_state(hash).await })
        });
        Some(state)
    }

    /// Attribute/entry TTL: zero for documents (size is computed live from
    /// the CRDT and changes as peers edit, so the kernel must not cache a
    /// stale `i_size`), the normal TTL otherwise.
    fn ttl_for(is_doc: bool) -> Duration {
        if is_doc { Duration::ZERO } else { TTL }
    }

    /// Commit an in-flight write buffer: import it as a document, or ingest it
    /// as opaque chunked content.
    fn commit_buffer(&self, ino: u64, data: &[u8]) {
        self.remember_rich_metadata(ino, data);
        let is_doc = self.is_document(ino);
        if is_doc {
            if self
                .document_export(ino)
                .is_some_and(|current| current == data)
            {
                return;
            }
            // import_document validates before replacing; an invalid input
            // leaves the previous content intact and returns false.
            let delta = {
                let mut ns = self.ns();
                if ns.import_document(ino, &self.registry, data, now_ms()) {
                    ns.save_incremental()
                } else {
                    Vec::new()
                }
            };
            self.doc_export_cache().remove(&ino);
            if delta.is_empty() {
                eprintln!("document import rejected (invalid format); previous content kept");
            } else {
                self.publish(delta);
                self.record_version(
                    ino,
                    collab_types::FileProvenance::Edit,
                    Some(collab_types::ContentHash(*blake3::hash(data).as_bytes())),
                );
            }
        } else if let Err(e) = self.commit_content(ino, data) {
            eprintln!("commit failed (content preserved): {e}");
        }
    }

    #[allow(clippy::significant_drop_tightening)]
    fn remember_rich_metadata(
        &self,
        ino: u64,
        bytes: &[u8],
    ) -> Option<collab_types::FilePropertiesV1> {
        let (record_id, name) = {
            let ns = self.ns();
            let record_id = ns.record_id_for(ino)?;
            let name = ns.path_for(ino)?.rsplit('/').next()?.to_owned();
            (record_id, name)
        };
        let properties = crate::rich_metadata::extract(&name, bytes)?;
        let hash = collab_types::ContentHash(*blake3::hash(bytes).as_bytes());
        self.rich_metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(record_id, (hash, properties.clone()));
        Some(properties)
    }

    /// The serialized (on-wire) bytes of a document inode, exported once per
    /// change and cached so multi-chunk reads and `getattr` are cheap + consistent.
    fn document_export(&self, ino: u64) -> Option<Vec<u8>> {
        let heads = self.ns().heads();
        if let Some((h, bytes)) = self.doc_export_cache().get(&ino)
            && *h == heads
        {
            return Some(bytes.clone());
        }
        let (heads, bytes) = {
            let mut ns = self.ns();
            let bytes = ns.export_document(ino, &self.registry)?;
            (ns.heads(), bytes)
        };
        self.doc_export_cache().insert(ino, (heads, bytes.clone()));
        Some(bytes)
    }

    /// Attach a sync handle so write ops propagate to peers.
    #[must_use]
    pub fn with_sync(mut self, sync: Arc<SyncHandle>) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Attach a host-side thumbnail provider (cache under `dir`). The mount then
    /// answers the `user.qos.thumb` xattr with a generated preview and records
    /// each thumbnail's hash on the namespace node so it projects to peers.
    #[must_use]
    pub fn with_thumbs(mut self, provider: crate::thumbs::ThumbProvider) -> Self {
        self.thumbs = Some(provider);
        self
    }

    #[must_use]
    pub fn with_availability(mut self, registry: Arc<Mutex<AvailabilityRegistry>>) -> Self {
        self.availability = Some(registry);
        self
    }

    /// Start the mount-lifetime worker that turns durable pin policy into
    /// verified local residency. Call this after attaching the live ACL so the
    /// worker never hydrates records outside this mount's current view.
    #[must_use]
    pub fn with_pinned_hydration(mut self) -> Self {
        let Some(registry) = self.availability.clone() else {
            return self;
        };
        if self.pinned_hydration.is_some() {
            return self;
        }
        let namespace = Arc::clone(&self.namespace);
        let hydrator = Arc::clone(&self.hydrator);
        let enforcement = self.enforcement.clone();
        let (wake, mut wakes) = tokio::sync::watch::channel(0_u64);
        wake.send_modify(|generation| *generation = generation.wrapping_add(1));
        let task = self.hydrator.runtime_handle.spawn(async move {
            let mut interval = tokio::time::interval(PINNED_HYDRATION_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut immediately = false;
            loop {
                if !immediately {
                    tokio::select! {
                        _ = interval.tick() => {}
                        changed = wakes.changed() => if changed.is_err() { break; },
                    }
                }
                immediately = reconcile_pinned_hydration(
                    &namespace,
                    Arc::clone(&hydrator),
                    Arc::clone(&registry),
                    enforcement.as_ref(),
                    wakes.clone(),
                )
                .await;
            }
        });
        self.pinned_hydration = Some(PinnedHydrationTask { wake, task });
        self
    }

    #[must_use]
    pub fn with_eviction_queue(mut self, queue: Arc<tokio::sync::Mutex<EvictionQueue>>) -> Self {
        self.eviction_queue = Some(queue);
        self
    }

    #[must_use]
    pub fn with_share_authority(
        mut self,
        authority: Arc<crate::share_authority::ShareAuthority>,
    ) -> Self {
        self.share_authority = Some(authority);
        self
    }

    /// Advertise the owner-only response-bearing share service on the mount
    /// root. The path is not secret; socket permissions enforce local access.
    #[must_use]
    pub fn with_share_control_path(mut self, path: std::path::PathBuf) -> Self {
        self.share_control_path = Some(path);
        self
    }

    /// Attach durable history and create a migration baseline for records
    /// that predate history support. Failure refuses the mount rather than
    /// silently running with an incomplete audit trail.
    ///
    /// # Errors
    ///
    /// Returns a history error when the migration baseline cannot be written.
    pub fn with_history(
        mut self,
        history: Arc<crate::history::FileHistoryStore>,
        actor: collab_types::FileActor,
    ) -> Result<Self, crate::history::HistoryError> {
        self.history = Some((history, actor));
        if self.allow_legacy_record_migration {
            self.ensure_history_baseline()?;
        }
        Ok(self)
    }

    /// Attach the signed record publisher and backfill locally authored
    /// history that predates the record transport.
    ///
    /// # Errors
    ///
    /// Returns a publisher error when backfill or projection rebuilding fails.
    pub fn with_record_publisher(
        mut self,
        publisher: Arc<crate::record_publisher::RecordPublisher>,
    ) -> Result<Self, crate::record_publisher::RecordPublisherError> {
        self.record_publisher = Some(publisher);
        if self.allow_legacy_record_migration {
            self.ensure_record_publication()?;
        }
        self.rebuild_signed_content_projection()?;
        Ok(self)
    }

    /// Attach the durable replacement-transaction journal and recover any
    /// interrupted transaction.
    ///
    /// # Errors
    ///
    /// Returns a publisher error when recovery cannot be completed safely.
    pub fn with_replacement_transactions(
        mut self,
        journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
        namespace_path: std::path::PathBuf,
    ) -> Result<Self, crate::record_publisher::RecordPublisherError> {
        if let (Some(publisher), Some((history, _))) = (&self.record_publisher, &self.history) {
            let sync = self.sync.clone();
            publisher.recover_replacement_transactions(
                &mut journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                history,
                &self.namespace,
                &namespace_path,
                move |delta| {
                    if let Some(sync) = &sync {
                        sync.broadcast_delta(delta);
                    }
                    Ok(())
                },
            )?;
        }
        self.replacement_transactions = Some(ReplacementTransactionRuntime {
            journal,
            namespace_path,
        });
        Ok(self)
    }

    fn rebuild_signed_content_projection(
        &self,
    ) -> Result<(), crate::record_publisher::RecordPublisherError> {
        let Some(publisher) = &self.record_publisher else {
            return Ok(());
        };
        let file_records = {
            let mut namespace = self.ns();
            let records = namespace
                .live_node_ids()
                .into_iter()
                .filter_map(|id| {
                    let path = namespace.path_for_id(&id)?;
                    let ino = namespace.resolve_path_ino(&path)?;
                    let attr = namespace.getattr(ino)?;
                    (attr.kind == FileType::RegularFile)
                        .then(|| namespace.record_id_for(ino))
                        .flatten()
                })
                .collect::<Vec<_>>();
            namespace.enable_signed_content_authority();
            records
        };
        for record_id in file_records {
            let Some(snapshot) = publisher.snapshot(record_id)? else {
                continue;
            };
            let Some(revision) = snapshot.content_revision else {
                continue;
            };
            let thumbnail = snapshot.metadata.as_ref().and_then(|metadata| {
                metadata
                    .previews
                    .iter()
                    .filter(|preview| preview.status == collab_types::PreviewStatus::Ready)
                    .filter_map(|preview| {
                        preview
                            .hash
                            .map(|hash| (preview.max_edge, Hash::from(hash.0)))
                    })
                    .max_by_key(|(max_edge, _)| *max_edge)
                    .map(|(_, hash)| hash)
            });
            let mut namespace = self.ns();
            if !namespace.install_verified_content_revision(revision) {
                return Err(
                    crate::record_publisher::RecordPublisherError::ProjectionRejected(record_id),
                );
            }
            let _ = namespace.install_verified_thumbnail(record_id, thumbnail);
        }
        Ok(())
    }

    // Namespace guards are already bounded by lexical snapshot blocks; keep
    // the publication sequence together so signed baselines remain ordered.
    #[allow(clippy::significant_drop_tightening)]
    fn ensure_record_publication(
        &self,
    ) -> Result<(), crate::record_publisher::RecordPublisherError> {
        let (Some(publisher), Some((history, _))) = (&self.record_publisher, &self.history) else {
            return Ok(());
        };
        let binding_inodes = {
            let mut namespace = self.ns();
            let mut inodes = vec![crate::namespace::ROOT_INO];
            let paths = namespace
                .live_node_ids()
                .into_iter()
                .filter_map(|id| namespace.path_for_id(&id))
                .collect::<Vec<_>>();
            inodes.extend(
                paths
                    .iter()
                    .filter_map(|path| namespace.resolve_path_ino(path)),
            );
            inodes
        };
        for ino in binding_inodes {
            let binding = {
                let namespace = self.ns();
                let observed_at_ms = namespace.getattr(ino).map_or_else(now_ms, |attr| {
                    attr.mtime.duration_since(UNIX_EPOCH).map_or_else(
                        |_| now_ms(),
                        |duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    )
                });
                namespace.signed_binding_for(ino, publisher.actor(), observed_at_ms)
            };
            if let Some(binding) = binding {
                publisher.publish_binding(&binding)?;
            }
        }
        let inodes: Vec<u64> = {
            let mut namespace = self.ns();
            let paths = namespace
                .live_node_ids()
                .into_iter()
                .filter_map(|id| namespace.path_for_id(&id))
                .collect::<Vec<_>>();
            let inodes = paths
                .iter()
                .filter_map(|path| namespace.resolve_path_ino(path))
                .collect::<Vec<_>>();
            inodes
                .into_iter()
                .filter(|ino| {
                    namespace
                        .getattr(*ino)
                        .is_some_and(|attr| attr.kind == FileType::RegularFile)
                })
                .collect()
        };
        for ino in inodes {
            let Some(snapshot) = self.version_snapshot(ino, false, None) else {
                continue;
            };
            let history_state = history.snapshot(snapshot.record_id);
            let versions = history_state
                .as_ref()
                .map(|history| {
                    history
                        .versions
                        .iter()
                        .filter(|version| version.actor == publisher.actor())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let conflicts = history_state
                .map(|history| {
                    history
                        .conflicts
                        .into_iter()
                        .filter(|conflict| {
                            conflict
                                .resolution
                                .as_ref()
                                .is_none_or(|resolution| resolution.actor == publisher.actor())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let observed_at_ms = versions
                .iter()
                .map(|version| version.timestamp_ms)
                .max()
                .unwrap_or_else(now_ms);
            publisher.publish_state(
                &self.record_metadata(&snapshot, observed_at_ms, publisher.actor()),
                &versions,
                &conflicts,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)]
    fn ensure_history_baseline(&self) -> Result<(), crate::history::HistoryError> {
        let Some((history, _)) = &self.history else {
            return Ok(());
        };
        let inodes = {
            let mut namespace = self.ns();
            let mut inodes = vec![crate::namespace::ROOT_INO];
            let paths = namespace
                .live_node_ids()
                .into_iter()
                .filter_map(|id| namespace.path_for_id(&id))
                .collect::<Vec<_>>();
            inodes.extend(
                paths
                    .iter()
                    .filter_map(|path| namespace.resolve_path_ino(path)),
            );
            inodes
        };
        for ino in inodes {
            let Some(snapshot) = self.version_snapshot(ino, false, None) else {
                continue;
            };
            if history.snapshot(snapshot.record_id).is_none() {
                self.append_version(snapshot, collab_types::FileProvenance::Create, now_ms())?;
            }
        }
        Ok(())
    }

    fn version_snapshot(
        &self,
        ino: u64,
        deleted: bool,
        content_override: Option<collab_types::ContentHash>,
    ) -> Option<VersionSnapshot> {
        let namespace = self.ns();
        Self::version_snapshot_locked(&namespace, ino, deleted, content_override, None)
    }

    fn version_snapshot_locked(
        namespace: &Namespace,
        ino: u64,
        deleted: bool,
        content_override: Option<collab_types::ContentHash>,
        byte_len_override: Option<u64>,
    ) -> Option<VersionSnapshot> {
        let record_id = namespace.record_id_for(ino)?;
        let attr = namespace.getattr(ino)?;
        let content_hash = content_override.or_else(|| {
            namespace
                .hash_for(ino)
                .map(|hash| collab_types::ContentHash(*hash.as_bytes()))
        });
        let byte_len = byte_len_override.unwrap_or(attr.size);
        let metadata = VersionMetadataDigest {
            path: namespace.path_for(ino),
            kind: match attr.kind {
                FileType::Directory => 1,
                FileType::RegularFile => 2,
                _ => 0,
            },
            byte_len,
            deleted,
        };
        let bytes = postcard::to_allocvec(&metadata).ok()?;
        Some(VersionSnapshot {
            record_id,
            content_hash,
            metadata_hash: collab_types::ContentHash(*blake3::hash(&bytes).as_bytes()),
            byte_len,
            kind: attr.kind,
        })
    }

    fn record_metadata(
        &self,
        snapshot: &VersionSnapshot,
        observed_at_ms: u64,
        actor: collab_types::FileActor,
    ) -> collab_types::FileMetadataV1 {
        let properties = self
            .rich_metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&snapshot.record_id)
            .filter(|(hash, _)| Some(*hash) == snapshot.content_hash)
            .map(|(_, properties)| properties.clone())
            .or_else(|| {
                let published = self
                    .record_publisher
                    .as_ref()?
                    .snapshot(snapshot.record_id)
                    .ok()??;
                let metadata = published.metadata?;
                (metadata.content_hash == snapshot.content_hash)
                    .then_some(metadata.properties)
                    .flatten()
            });
        collab_types::FileMetadataV1 {
            schema_version: collab_types::files::FILE_METADATA_VERSION,
            record_id: snapshot.record_id,
            content_hash: snapshot.content_hash,
            byte_len: snapshot.byte_len,
            mime: None,
            previews: Vec::new(),
            latest_actor: Some(actor),
            observed_at_ms,
            index_status: collab_types::IndexStatus::Pending,
            properties,
        }
    }

    fn metadata_projection_for(&self, ino: u64) -> Option<Vec<u8>> {
        let record_id = self.ns().record_id_for(ino)?;
        let properties = self
            .record_publisher
            .as_ref()?
            .snapshot(record_id)
            .ok()??
            .metadata?
            .properties?;
        let projection = crate::syncstate::MetadataProjectionV1 {
            schema_version: crate::syncstate::METADATA_PROJECTION_VERSION,
            record_id,
            properties,
        };
        projection.validate().ok()?;
        let bytes = serde_json::to_vec(&projection).ok()?;
        (bytes.len() <= crate::syncstate::MAX_METADATA_XATTR_BYTES).then_some(bytes)
    }

    #[allow(clippy::significant_drop_tightening)]
    fn replacement_snapshot(
        &self,
        ino: u64,
        content_hash: Option<Hash>,
        byte_len: u64,
    ) -> Option<VersionSnapshot> {
        let namespace = self.ns();
        let record_id = namespace.record_id_for(ino)?;
        let attr = namespace.getattr(ino)?;
        let metadata = VersionMetadataDigest {
            path: namespace.path_for(ino),
            kind: match attr.kind {
                FileType::Directory => 1,
                FileType::RegularFile => 2,
                _ => 0,
            },
            byte_len,
            deleted: false,
        };
        let bytes = postcard::to_allocvec(&metadata).ok()?;
        Some(VersionSnapshot {
            record_id,
            content_hash: content_hash.map(|hash| collab_types::ContentHash(*hash.as_bytes())),
            metadata_hash: collab_types::ContentHash(*blake3::hash(&bytes).as_bytes()),
            byte_len,
            kind: attr.kind,
        })
    }

    fn append_version(
        &self,
        snapshot: VersionSnapshot,
        provenance: collab_types::FileProvenance,
        timestamp_ms: u64,
    ) -> Result<collab_types::FileVersionId, crate::history::HistoryError> {
        let Some((history, actor)) = &self.history else {
            return Ok(collab_types::FileVersionId::new());
        };
        let parents = history
            .snapshot(snapshot.record_id)
            .map_or_else(Vec::new, |history| history.heads);
        let id = collab_types::FileVersionId::new();
        let version = collab_types::FileVersionV1 {
            id,
            record_id: snapshot.record_id,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: snapshot.content_hash,
            metadata_hash: snapshot.metadata_hash,
            provenance,
        };
        if let Some(publisher) = &self.record_publisher {
            if snapshot.kind == FileType::Directory {
                publisher
                    .publish_structural_version(&version)
                    .map_err(|error| {
                        crate::history::HistoryError::Publication(error.to_string())
                    })?;
            } else {
                publisher
                    .publish(
                        &self.record_metadata(&snapshot, timestamp_ms, *actor),
                        std::slice::from_ref(&version),
                    )
                    .map_err(|error| {
                        crate::history::HistoryError::Publication(error.to_string())
                    })?;
            }
        }
        history.append(version)?;
        if let Some(registry) = &self.availability {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(mut record) = registry.get(snapshot.record_id).cloned() {
                record.current_version = Some(id);
                if let Err(error) = registry.put(record) {
                    eprintln!("could not project history version into availability: {error}");
                }
            }
        }
        Ok(id)
    }

    fn record_version(
        &self,
        ino: u64,
        provenance: collab_types::FileProvenance,
        content_override: Option<collab_types::ContentHash>,
    ) {
        if self.history.is_none() {
            return;
        }
        let Some(snapshot) = self.version_snapshot(ino, false, content_override) else {
            eprintln!("could not capture version state for inode {ino}");
            return;
        };
        if let Err(error) = self.append_version(snapshot, provenance, now_ms()) {
            eprintln!("could not append durable file history for inode {ino}: {error}");
        }
    }

    fn share_projection_for(&self, ino: u64, now_ms: u64) -> Option<Vec<u8>> {
        let authority = self.share_authority.as_ref()?;
        let record_id = self.ns().record_id_for(ino)?;
        authority
            .projection(record_id, now_ms)
            .to_xattr_bytes()
            .ok()
    }

    fn history_projection_for(&self, ino: u64) -> Option<Vec<u8>> {
        let (history, _) = self.history.as_ref()?;
        let record_id = self.ns().record_id_for(ino)?;
        history.projection(record_id)?.to_xattr_bytes().ok()
    }

    #[allow(clippy::significant_drop_tightening)]
    fn record_index_projection(&self) -> Option<Vec<u8>> {
        use crate::syncstate::{
            MAX_RECORD_INDEX_ENTRIES, MAX_RECORD_INDEX_XATTR_BYTES,
            RECORD_INDEX_PROJECTION_VERSION, RecordIndexEntryV1, RecordIndexProjectionV1,
        };
        let namespace = self.ns();
        let mut candidates = namespace
            .live_node_ids()
            .into_iter()
            .filter_map(|id| {
                let record_id = namespace.record_id_for_id(&id)?;
                let path = namespace.path_for_id(&id)?;
                let is_dir = namespace.node_is_dir(&id)?;
                self.in_view(&path).then_some(RecordIndexEntryV1 {
                    record_id,
                    path,
                    is_dir,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|entry| entry.record_id);
        candidates.dedup_by_key(|entry| entry.record_id);

        let mut projection = RecordIndexProjectionV1 {
            schema_version: RECORD_INDEX_PROJECTION_VERSION,
            truncated: candidates.len() > MAX_RECORD_INDEX_ENTRIES,
            records: Vec::new(),
        };
        for entry in candidates.into_iter().take(MAX_RECORD_INDEX_ENTRIES) {
            projection.records.push(entry);
            let Ok(bytes) = serde_json::to_vec(&projection) else {
                return None;
            };
            if bytes.len() > MAX_RECORD_INDEX_XATTR_BYTES {
                projection.records.pop();
                projection.truncated = true;
                break;
            }
        }
        projection.validate().ok()?;
        let bytes = serde_json::to_vec(&projection).ok()?;
        (bytes.len() <= MAX_RECORD_INDEX_XATTR_BYTES).then_some(bytes)
    }

    fn restore_version(
        &self,
        ino: u64,
        version_id: collab_types::FileVersionId,
    ) -> Result<(), i32> {
        self.check_access(ino, Access::Write)?;
        if self
            .write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&ino)
        {
            return Err(EBUSY);
        }
        if self.is_document(ino) {
            // Document restoration needs codec-aware CRDT checkout rather
            // than replacing its exported bytes as if it were a plain file.
            return Err(libc::ENOTSUP);
        }
        let (history, _) = self.history.as_ref().ok_or(libc::ENOTSUP)?;
        let record_id = self.ns().record_id_for(ino).ok_or(ENOENT)?;
        let version = history
            .snapshot(record_id)
            .and_then(|snapshot| {
                snapshot
                    .versions
                    .into_iter()
                    .find(|version| version.id == version_id)
            })
            .ok_or(EINVAL)?;
        let data = match version.content_hash {
            Some(content_hash) => {
                let hydrator = self.hydrator.clone();
                let runtime = hydrator.runtime_handle.clone();
                tokio::task::block_in_place(|| {
                    runtime.block_on(async move {
                        hydrator.read(Hash::from(content_hash.0), 0, u64::MAX).await
                    })
                })
                .map_err(|_| EIO)?
            }
            None => Vec::new(),
        };
        let restored_hash = self.write_content_blocking(&data).map_err(|_| EIO)?;
        if restored_hash.map(|hash| *hash.as_bytes()) != version.content_hash.map(|hash| hash.0) {
            return Err(EIO);
        }
        let (expected_hash, expected_len) = {
            let namespace = self.ns();
            let attr = namespace.getattr(ino).ok_or(ENOENT)?;
            (namespace.hash_for(ino), attr.size)
        };
        let snapshot = self
            .replacement_snapshot(ino, restored_hash, data.len() as u64)
            .ok_or(EIO)?;
        self.append_version(
            snapshot,
            collab_types::FileProvenance::Restore { from: version_id },
            now_ms(),
        )
        .map_err(|_| EIO)?;
        let revision = match &self.record_publisher {
            Some(publisher) => publisher
                .snapshot(snapshot.record_id)
                .map_err(|_| EIO)?
                .and_then(|snapshot| snapshot.content_revision)
                .ok_or(EIO)
                .map(Some)?,
            None => None,
        };
        let delta = {
            let mut namespace = self.ns();
            if namespace.record_id_for(ino) != Some(record_id)
                || namespace.hash_for(ino) != expected_hash
                || namespace
                    .getattr(ino)
                    .is_none_or(|attr| attr.size != expected_len)
            {
                return Err(EBUSY);
            }
            if let Some(revision) = &revision {
                if !namespace.set_content_revision(ino, revision, now_ms()) {
                    return Err(EIO);
                }
            } else {
                namespace.set_content(ino, restored_hash.as_ref(), data.len() as u64, now_ms());
            }
            namespace.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    /// Resolve an open signed conflict by copying one exact immutable member
    /// into a new, attributed result version. The request always names the
    /// member it saw: `KeepCurrent` is rejected if that member stopped being the
    /// current signed revision, and `UseOther` is rejected if it names current.
    /// This prevents a delayed UI request from being silently reinterpreted.
    // This is one ordered validation/publication/projection transaction.
    #[allow(clippy::too_many_lines)]
    fn resolve_conflict(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        strategy: collab_types::ConflictStrategy,
        version_id: collab_types::FileVersionId,
    ) -> Result<(), i32> {
        self.check_access(ino, Access::Write)?;
        if self
            .write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&ino)
        {
            return Err(EBUSY);
        }
        if self.is_document(ino) {
            return Err(libc::ENOTSUP);
        }
        if strategy == collab_types::ConflictStrategy::Merge {
            return self.resolve_conflict_merge(ino, conflict_id, version_id);
        }
        if strategy == collab_types::ConflictStrategy::KeepBoth {
            return self.resolve_conflict_keep_both(ino, conflict_id, version_id);
        }
        if !matches!(
            strategy,
            collab_types::ConflictStrategy::KeepCurrent | collab_types::ConflictStrategy::UseOther
        ) {
            // KeepBoth needs a signed two-record transaction and Merge needs a
            // codec-produced result. Neither can be represented truthfully by
            // this one-record control operation.
            return Err(libc::ENOTSUP);
        }
        let (history, actor) = self.history.as_ref().ok_or(libc::ENOTSUP)?;
        let publisher = self.record_publisher.as_ref().ok_or(libc::ENOTSUP)?;
        let (record_id, expected_revision) = {
            let namespace = self.ns();
            (
                namespace.record_id_for(ino).ok_or(ENOENT)?,
                namespace.content_revision_for(ino).ok_or(EIO)?,
            )
        };
        if expected_revision.record_id != record_id {
            return Err(EIO);
        }
        let state = history.snapshot(record_id).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .cloned()
            .ok_or(EINVAL)?;
        if conflict.status != collab_types::ConflictStatus::Open
            || !conflict.versions.contains(&version_id)
        {
            return Err(EINVAL);
        }
        match strategy {
            collab_types::ConflictStrategy::KeepCurrent
                if version_id != expected_revision.version_id =>
            {
                return Err(EBUSY);
            }
            collab_types::ConflictStrategy::UseOther
                if version_id == expected_revision.version_id =>
            {
                return Err(EINVAL);
            }
            _ => {}
        }
        let selected = state
            .versions
            .iter()
            .find(|version| version.id == version_id)
            .cloned()
            .ok_or(EINVAL)?;
        let data = match selected.content_hash {
            Some(content_hash) => {
                let hydrator = self.hydrator.clone();
                let runtime = hydrator.runtime_handle.clone();
                tokio::task::block_in_place(|| {
                    runtime.block_on(async move {
                        hydrator.read(Hash::from(content_hash.0), 0, u64::MAX).await
                    })
                })
                .map_err(|_| EIO)?
            }
            None => Vec::new(),
        };
        let result_hash = self.write_content_blocking(&data).map_err(|_| EIO)?;
        if result_hash.map(|hash| *hash.as_bytes()) != selected.content_hash.map(|hash| hash.0) {
            return Err(EIO);
        }
        let result_snapshot = self
            .replacement_snapshot(ino, result_hash, data.len() as u64)
            .ok_or(EIO)?;

        // The signed replica requires a resolution result to descend from
        // every member of the conflict. Preserve any additional current heads
        // as parents too so the resolution does not disconnect later work.
        let mut parents = state.heads;
        parents.extend(conflict.versions.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > collab_types::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let timestamp_ms = now_ms();
        let result_version = collab_types::FileVersionV1 {
            id: collab_types::FileVersionId::new(),
            record_id,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: result_snapshot.content_hash,
            metadata_hash: result_snapshot.metadata_hash,
            provenance: collab_types::FileProvenance::ConflictResolution,
        };
        let resolution = collab_types::ConflictResolution {
            strategy,
            result: result_version.id,
            actor: *actor,
            timestamp_ms,
        };
        let mut resolved = conflict;
        resolved.status = collab_types::ConflictStatus::Resolved;
        resolved.resolution = Some(resolution.clone());

        // Publish the immutable result and resolution before exposing its
        // bytes through the namespace. The repository operation stream is
        // signed, replay-safe and deterministically ordered across devices.
        publisher
            .publish_state(
                &self.record_metadata(&result_snapshot, timestamp_ms, *actor),
                std::slice::from_ref(&result_version),
                std::slice::from_ref(&resolved),
            )
            .map_err(|_| EIO)?;
        history.append(result_version).map_err(|_| EIO)?;
        history
            .resolve(record_id, conflict_id, resolution)
            .map_err(|_| EIO)?;

        let converged = publisher
            .snapshot(record_id)
            .map_err(|_| EIO)?
            .and_then(|snapshot| snapshot.content_revision)
            .ok_or(EIO)?;
        let delta = {
            let mut namespace = self.ns();
            if namespace.record_id_for(ino) != Some(record_id)
                || namespace.content_revision_for(ino) != Some(expected_revision)
            {
                return Err(EBUSY);
            }
            if !namespace.set_content_revision(ino, &converged, timestamp_ms) {
                return Err(EIO);
            }
            namespace.save_incremental()
        };
        self.publish(delta);
        if let Some(registry) = &self.availability {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(mut record) = registry.get(record_id).cloned() {
                record.current_version = Some(converged.version_id);
                registry.put(record).map_err(|_| EIO)?;
            }
        }
        Ok(())
    }

    // Keep the exact two-record replacement transaction in one auditable path.
    #[allow(clippy::significant_drop_tightening, clippy::too_many_lines)]
    fn resolve_conflict_merge(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        selected_version: collab_types::FileVersionId,
    ) -> Result<(), i32> {
        let (history, actor) = self.history.as_ref().ok_or(libc::ENOTSUP)?;
        let publisher = self.record_publisher.as_ref().ok_or(libc::ENOTSUP)?;
        let runtime = self
            .replacement_transactions
            .as_ref()
            .ok_or(libc::ENOTSUP)?;
        let (record_id, current_revision, parent_record, source_name) = {
            let mut namespace = self.ns();
            let record_id = namespace.record_id_for(ino).ok_or(ENOENT)?;
            let current = namespace.content_revision_for(ino).ok_or(EIO)?;
            let parent = namespace
                .parent_ino(ino)
                .and_then(|parent| namespace.record_id_for(parent))
                .ok_or(EIO)?;
            let name = namespace.file_name_for(ino).ok_or(EIO)?;
            (record_id, current, parent, name)
        };
        let extension = std::str::from_utf8(source_name.as_bytes())
            .ok()
            .and_then(|name| {
                name.rsplit_once('.')
                    .map(|(_, ext)| ext.to_ascii_lowercase())
            })
            .ok_or(libc::ENOTSUP)?;
        if !matches!(extension.as_str(), "txt" | "qdoc") {
            return Err(libc::ENOTSUP);
        }
        let state = history.snapshot(record_id).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .ok_or(EINVAL)?;
        let result_id = collab_types::FileVersionId(merge_uuid(
            b"result",
            publisher.workspace_id(),
            record_id,
            conflict_id,
            &conflict.versions,
            *actor,
        ));
        if let Some(resolution) = &conflict.resolution {
            return if resolution.strategy == collab_types::ConflictStrategy::Merge
                && resolution.result == result_id
                && current_revision.version_id == result_id
            {
                Ok(())
            } else {
                Err(EBUSY)
            };
        }
        if conflict.status != collab_types::ConflictStatus::Open
            || conflict.versions.len() != 2
            || !conflict.versions.contains(&selected_version)
            || !conflict.versions.contains(&current_revision.version_id)
            || selected_version == current_revision.version_id
        {
            return Err(EINVAL);
        }
        let load = |version_id| -> Result<Vec<u8>, i32> {
            let version = state
                .versions
                .iter()
                .find(|value| value.id == version_id)
                .ok_or(EINVAL)?;
            match version.content_hash {
                Some(hash) => {
                    let hydrator = self.hydrator.clone();
                    let handle = hydrator.runtime_handle.clone();
                    tokio::task::block_in_place(|| {
                        handle.block_on(async move {
                            hydrator.read(Hash::from(hash.0), 0, u64::MAX).await
                        })
                    })
                    .map_err(|_| EIO)
                }
                None => Ok(Vec::new()),
            }
        };
        let first_id = *conflict.versions.first().ok_or(EINVAL)?;
        let second_id = *conflict.versions.get(1).ok_or(EINVAL)?;
        let first = crate::conflict_merge::ExactMergeInput {
            version_id: first_id,
            bytes: load(first_id)?,
        };
        let second = crate::conflict_merge::ExactMergeInput {
            version_id: second_id,
            bytes: load(second_id)?,
        };
        let merged = if extension == "qdoc" {
            let merged = crate::conflict_merge::merge_exact(first, second, None)
                .map_err(|_| libc::ENOTSUP)?;
            (merged.codec == crate::conflict_merge::DeterministicMergeCodec::QdocAutomergeV1)
                .then_some(merged)
                .ok_or(libc::ENOTSUP)?
        } else {
            let base_id =
                crate::conflict_merge::exact_common_base(&state.versions, first_id, second_id)
                    .map_err(|_| libc::ENOTSUP)?;
            let merged = crate::conflict_merge::merge_exact(
                first,
                second,
                Some(crate::conflict_merge::ExactMergeBase {
                    version_id: base_id,
                    bytes: load(base_id)?,
                }),
            )
            .map_err(|_| libc::ENOTSUP)?;
            (merged.codec == crate::conflict_merge::DeterministicMergeCodec::PlainUtf8ThreeWayV1)
                .then_some(merged)
                .ok_or(libc::ENOTSUP)?
        };
        let hash = self
            .write_content_blocking(&merged.bytes)
            .map_err(|_| EIO)?;
        let snapshot = self
            .replacement_snapshot(ino, hash, merged.bytes.len() as u64)
            .ok_or(EIO)?;
        let timestamp_ms = conflict
            .versions
            .iter()
            .filter_map(|id| state.versions.iter().find(|version| version.id == *id))
            .map(|version| version.timestamp_ms)
            .max()
            .and_then(|value| value.checked_add(1))
            .ok_or(EINVAL)?;
        let mut parents = state.heads.clone();
        parents.extend(conflict.versions.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > collab_types::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let proposed_version = collab_types::FileVersionV1 {
            id: result_id,
            record_id,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: snapshot.content_hash,
            metadata_hash: snapshot.metadata_hash,
            provenance: collab_types::FileProvenance::ConflictResolution,
        };
        let mut participant = collab_types::FileMutationParticipantV1 {
            record_id,
            role: collab_types::FileMutationParticipantRoleV1::MergeSource,
            expected_version: current_revision.version_id,
            proposed_binding: collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent_record),
                name: source_name.clone(),
                kind: collab_types::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id,
                version_id: result_id,
                content_hash: snapshot.content_hash,
                byte_len: snapshot.byte_len,
                metadata_hash: snapshot.metadata_hash,
            }),
            proposed_version,
            projection_hash: collab_types::ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().map_err(|_| EIO)?;
        let codec = match merged.codec {
            crate::conflict_merge::DeterministicMergeCodec::QdocAutomergeV1 => {
                collab_types::ConflictMergeCodecV1::QdocAutomergeV1
            }
            crate::conflict_merge::DeterministicMergeCodec::PlainUtf8ThreeWayV1 => {
                collab_types::ConflictMergeCodecV1::PlainUtf8ThreeWayV1
            }
        };
        let manifest = collab_types::FileMutationTransactionManifestV1 {
            schema_version: collab_types::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id: merge_uuid(
                b"transaction",
                publisher.workspace_id(),
                record_id,
                conflict_id,
                &conflict.versions,
                *actor,
            ),
            workspace_id: publisher.workspace_id(),
            actor: *actor,
            timestamp_ms,
            operation: collab_types::FileMutationOperationV1::MergeContentConflict {
                source_record: record_id,
                parent_record,
                source_name,
                conflict_id,
                conflict_versions: conflict.versions.clone(),
                current_version: current_revision.version_id,
                base_version: merged.base_version,
                codec,
            },
            participants: vec![participant],
        };
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut runtime
                    .journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                history,
                &self.namespace,
                &runtime.namespace_path,
                move |delta| {
                    if let Some(sync) = &sync {
                        sync.broadcast_delta(delta);
                    }
                    Ok(())
                },
            )
            .map_err(|_| EIO)
    }

    // Keep the exact two-record replacement transaction in one auditable path.
    #[allow(clippy::significant_drop_tightening, clippy::too_many_lines)]
    fn resolve_conflict_keep_both(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        selected_version: collab_types::FileVersionId,
    ) -> Result<(), i32> {
        let (history, actor) = self.history.as_ref().ok_or(libc::ENOTSUP)?;
        let publisher = self.record_publisher.as_ref().ok_or(libc::ENOTSUP)?;
        let transaction_runtime = self
            .replacement_transactions
            .as_ref()
            .ok_or(libc::ENOTSUP)?;
        let (source_record, current_revision, parent_record, source_name, source_path) = {
            let mut namespace = self.ns();
            let source_record = namespace.record_id_for(ino).ok_or(ENOENT)?;
            let current_revision = namespace.content_revision_for(ino).ok_or(EIO)?;
            let parent_ino = namespace.parent_ino(ino).ok_or(EIO)?;
            let parent_record = namespace.record_id_for(parent_ino).ok_or(EIO)?;
            let source_path = namespace.path_for(ino).ok_or(EIO)?;
            let source_name = namespace.file_name_for(ino).ok_or(EIO)?;
            (
                source_record,
                current_revision,
                parent_record,
                source_name,
                source_path,
            )
        };
        let derive = |domain: &[u8]| {
            keep_both_uuid(
                domain,
                publisher.workspace_id(),
                source_record,
                conflict_id,
                selected_version,
                *actor,
            )
        };
        let target_record = collab_types::FileRecordId(derive(b"target-record"));
        let target_version_id = collab_types::FileVersionId(derive(b"target-version"));
        let result_version_id = collab_types::FileVersionId(derive(b"source-result"));
        let transaction_id = derive(b"transaction");
        let state = history.snapshot(source_record).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .ok_or(EINVAL)?;
        if let Some(resolution) = &conflict.resolution {
            return if resolution.strategy == collab_types::ConflictStrategy::KeepBoth
                && resolution.result == result_version_id
                && self.ns().ensure_ino_for_record_id(target_record).is_some()
            {
                Ok(())
            } else {
                Err(EBUSY)
            };
        }
        if conflict.status != collab_types::ConflictStatus::Open
            || conflict.versions.len() != 2
            || !conflict.versions.contains(&current_revision.version_id)
            || !conflict.versions.contains(&selected_version)
            || selected_version == current_revision.version_id
        {
            return Err(EINVAL);
        }
        let selected = state
            .versions
            .iter()
            .find(|version| version.id == selected_version)
            .ok_or(EINVAL)?;
        let current = state
            .versions
            .iter()
            .find(|version| version.id == current_revision.version_id)
            .ok_or(EBUSY)?;
        if current.content_hash != current_revision.content_hash {
            return Err(EBUSY);
        }
        let selected_data = match selected.content_hash {
            Some(content_hash) => {
                let hydrator = self.hydrator.clone();
                let runtime = hydrator.runtime_handle.clone();
                tokio::task::block_in_place(|| {
                    runtime.block_on(async move {
                        hydrator.read(Hash::from(content_hash.0), 0, u64::MAX).await
                    })
                })
                .map_err(|_| EIO)?
            }
            None => Vec::new(),
        };
        let timestamp_ms = conflict
            .versions
            .iter()
            .filter_map(|id| state.versions.iter().find(|version| version.id == *id))
            .map(|version| version.timestamp_ms)
            .max()
            .and_then(|timestamp| timestamp.checked_add(1))
            .ok_or(EINVAL)?;
        let target_name = keep_both_name(&source_name, target_record);
        let parent_path = source_path
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        let target_path = if parent_path.is_empty() {
            format!(
                "/{}",
                crate::namespace::escape_path_name(target_name.as_bytes())
            )
        } else {
            format!(
                "{parent_path}/{}",
                crate::namespace::escape_path_name(target_name.as_bytes())
            )
        };
        let metadata_hash = |path: String, byte_len: u64| -> Result<_, i32> {
            let bytes = postcard::to_allocvec(&VersionMetadataDigest {
                path: Some(path),
                kind: 2,
                byte_len,
                deleted: false,
            })
            .map_err(|_| EIO)?;
            Ok(collab_types::ContentHash(*blake3::hash(&bytes).as_bytes()))
        };
        let source_metadata_hash = metadata_hash(source_path, current_revision.byte_len)?;
        let target_metadata_hash = metadata_hash(target_path, selected_data.len() as u64)?;
        let mut parents = state.heads.clone();
        parents.extend(conflict.versions.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > collab_types::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let source_version = collab_types::FileVersionV1 {
            id: result_version_id,
            record_id: source_record,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: current.content_hash,
            metadata_hash: source_metadata_hash,
            provenance: collab_types::FileProvenance::ConflictResolution,
        };
        let target_version = collab_types::FileVersionV1 {
            id: target_version_id,
            record_id: target_record,
            parents: Vec::new(),
            actor: *actor,
            timestamp_ms,
            content_hash: selected.content_hash,
            metadata_hash: target_metadata_hash,
            provenance: collab_types::FileProvenance::Create,
        };
        let mut source_participant = collab_types::FileMutationParticipantV1 {
            record_id: source_record,
            role: collab_types::FileMutationParticipantRoleV1::KeepBothSource,
            expected_version: current_revision.version_id,
            proposed_binding: collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: source_record,
                parent: Some(parent_record),
                name: source_name.clone(),
                kind: collab_types::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id: source_record,
                version_id: result_version_id,
                content_hash: current.content_hash,
                byte_len: current_revision.byte_len,
                metadata_hash: source_metadata_hash,
            }),
            proposed_version: source_version,
            projection_hash: collab_types::ContentHash([0; 32]),
        };
        source_participant.projection_hash = source_participant
            .computed_projection_hash()
            .map_err(|_| EIO)?;
        let mut sibling_participant = collab_types::FileMutationParticipantV1 {
            record_id: target_record,
            role: collab_types::FileMutationParticipantRoleV1::KeepBothSibling,
            expected_version: selected_version,
            proposed_binding: collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: target_record,
                parent: Some(parent_record),
                name: target_name.clone(),
                kind: collab_types::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(collab_types::ContentRevisionV1 {
                schema_version: collab_types::CONTENT_REVISION_VERSION,
                record_id: target_record,
                version_id: target_version_id,
                content_hash: selected.content_hash,
                byte_len: selected_data.len() as u64,
                metadata_hash: target_metadata_hash,
            }),
            proposed_version: target_version,
            projection_hash: collab_types::ContentHash([0; 32]),
        };
        sibling_participant.projection_hash = sibling_participant
            .computed_projection_hash()
            .map_err(|_| EIO)?;
        let mut participants = vec![source_participant, sibling_participant];
        participants.sort_by_key(|participant| participant.record_id);
        let manifest = collab_types::FileMutationTransactionManifestV1 {
            schema_version: collab_types::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id,
            workspace_id: publisher.workspace_id(),
            actor: *actor,
            timestamp_ms,
            operation: collab_types::FileMutationOperationV1::KeepBothContentConflict {
                source_record,
                sibling_record: target_record,
                parent_record,
                source_name,
                sibling_name: target_name,
                conflict_id,
                conflict_versions: conflict.versions.clone(),
                selected_version,
            },
            participants,
        };
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut transaction_runtime
                    .journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                history,
                &self.namespace,
                &transaction_runtime.namespace_path,
                move |delta| {
                    if let Some(sync) = &sync {
                        sync.broadcast_delta(delta);
                    }
                    Ok(())
                },
            )
            .map_err(|_| EIO)
    }

    #[allow(clippy::significant_drop_tightening)]
    fn availability_for(&self, ino: u64) -> Option<Vec<u8>> {
        let registry = self.availability.as_ref()?;
        let (record_id, kind, parent, size, ancestry) = {
            let mut ns = self.ns();
            let record_id = ns.record_id_for(ino)?;
            let attr = ns.getattr(ino)?;
            let kind = if attr.kind == FileType::Directory {
                RecordKind::Directory
            } else {
                RecordKind::File
            };
            let parent = ns
                .parent_ino(ino)
                .and_then(|parent_ino| ns.record_id_for(parent_ino));
            let mut ancestry = Vec::new();
            let mut cursor = Some(ino);
            while let Some(current) = cursor {
                ancestry.push(ns.record_id_for(current)?);
                cursor = ns.parent_ino(current);
            }
            (record_id, kind, parent, attr.size, ancestry)
        };
        let (mut record, effective_policy) = {
            let registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let record = registry
                .get(record_id)
                .cloned()
                .unwrap_or_else(|| RecordAvailability::new(record_id, kind));
            let policy = if ancestry.iter().any(|id| {
                registry
                    .get(*id)
                    .is_some_and(|record| record.policy == AvailabilityPolicy::Pinned)
            }) {
                AvailabilityPolicy::Pinned
            } else {
                AvailabilityPolicy::Automatic
            };
            (record, policy)
        };
        record.parent = parent;
        match self.sync_state_for(ino)? {
            crate::syncstate::SyncState::Local => {
                record.residency = Residency::Resident { bytes: size };
                record.transfer.phase = TransferPhase::Idle;
            }
            crate::syncstate::SyncState::Projected => {
                record.residency = Residency::NotResident;
                record.transfer.phase = TransferPhase::Idle;
            }
            crate::syncstate::SyncState::Downloading => {
                record.transfer.phase = TransferPhase::Downloading;
            }
        }
        crate::syncstate::AvailabilityProjectionV1::from_record_with_policy(
            &record,
            effective_policy,
        )
        .to_xattr_bytes()
        .ok()
    }

    #[allow(clippy::significant_drop_tightening)]
    fn set_pin_policy(&self, ino: u64, policy: AvailabilityPolicy) -> Result<(), i32> {
        self.check_access(ino, Access::Read)?;
        let registry = self.availability.as_ref().ok_or(libc::ENOTSUP)?;
        let (record_id, mut ancestry) = {
            let mut ns = self.ns();
            let record_id = ns.record_id_for(ino).ok_or(ENOENT)?;
            let mut ancestry = Vec::new();
            let mut cursor = Some(ino);
            while let Some(current_ino) = cursor {
                let current_id = ns.record_id_for(current_ino).ok_or(ENOENT)?;
                let attr = ns.getattr(current_ino).ok_or(ENOENT)?;
                let kind = if attr.kind == FileType::Directory {
                    RecordKind::Directory
                } else {
                    RecordKind::File
                };
                let parent_ino = ns.parent_ino(current_ino);
                let parent = parent_ino.and_then(|parent_ino| ns.record_id_for(parent_ino));
                ancestry.push((current_id, kind, parent));
                cursor = parent_ino;
            }
            (record_id, ancestry)
        };
        {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ancestry.reverse();
            for (id, kind, parent) in ancestry {
                let mut record = registry
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| RecordAvailability::new(id, kind));
                record.kind = kind;
                record.parent = parent;
                if id == record_id {
                    record.policy = policy;
                }
                registry.put(record).map_err(|_| EIO)?;
            }
        }
        if let Some(worker) = &self.pinned_hydration {
            worker
                .wake
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)]
    fn retry_transfer(&self, ino: u64) -> Result<(), i32> {
        self.check_access(ino, Access::Read)?;
        let registry = self.availability.as_ref().ok_or(libc::ENOTSUP)?.clone();
        let (record_id, hash, size) = {
            let namespace = self.ns();
            let record_id = namespace.record_id_for(ino).ok_or(ENOENT)?;
            let hash = namespace.hash_for(ino);
            let size = namespace.getattr(ino).ok_or(ENOENT)?.size;
            (record_id, hash, size)
        };
        let scheduled_by_pin_worker = {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut record = registry
                .get(record_id)
                .cloned()
                .unwrap_or_else(|| RecordAvailability::new(record_id, RecordKind::File));
            if record.transfer.phase.is_active() {
                return Err(libc::EALREADY);
            }
            if hash.is_none() && size == 0 {
                record.residency = Residency::Resident { bytes: 0 };
                record.transfer.phase = TransferPhase::Idle;
                record.transfer.progress = None;
                record.transfer.last_error = None;
                record.transfer.retry_count = 0;
                record.transfer.retry_at_ms = None;
                record.freshness.local_verified_at_ms = Some(now_ms());
                registry.put(record).map_err(|_| EIO)?;
                return Ok(());
            }
            record.transfer.phase = TransferPhase::Queued;
            record.transfer.last_error = None;
            record.transfer.retry_count = 0;
            record.transfer.retry_at_ms = None;
            record.transfer.progress = Some(crate::availability::TransferProgress {
                completed_bytes: 0,
                total_bytes: Some(size),
            });
            registry.put(record).map_err(|_| EIO)?;
            self.pinned_hydration.is_some()
                && matches!(
                    registry.effective_policy(record_id),
                    Ok(Some(AvailabilityPolicy::Pinned))
                )
        };
        // A pinned retry joins the mount worker; spawning here used to bypass
        // its bound and could race the same record with an automatic pass.
        if scheduled_by_pin_worker {
            self.wake_pinned_hydration();
            return Ok(());
        }
        let hash = hash.ok_or(EIO)?;

        let hydrator = self.hydrator.clone();
        self.hydrator.runtime_handle.spawn(async move {
            update_retry_state(&registry, record_id, |record| {
                record.transfer.phase = TransferPhase::Downloading;
            });
            let mut last_persisted = 0_u64;
            match hydrator
                .prefetch_with_progress(hash, size, |completed_bytes, total_bytes| {
                    if completed_bytes != total_bytes
                        && completed_bytes.saturating_sub(last_persisted)
                            < HYDRATION_PROGRESS_PERSIST_BYTES
                    {
                        return;
                    }
                    last_persisted = completed_bytes;
                    update_retry_state(&registry, record_id, |record| {
                        record.transfer.phase = TransferPhase::Downloading;
                        record.transfer.progress = Some(crate::availability::TransferProgress {
                            completed_bytes,
                            total_bytes: Some(total_bytes),
                        });
                    });
                })
                .await
            {
                Ok(true) => update_retry_state(&registry, record_id, |record| {
                    record.residency = Residency::Resident { bytes: size };
                    record.transfer.phase = TransferPhase::Idle;
                    record.transfer.progress = None;
                    record.transfer.last_error = None;
                    record.transfer.retry_count = 0;
                    record.transfer.retry_at_ms = None;
                    record.freshness.local_verified_at_ms = Some(now_ms());
                }),
                Ok(false) => update_retry_state(&registry, record_id, |record| {
                    record.transfer.phase = TransferPhase::Idle;
                    record.transfer.progress = None;
                    record.transfer.last_error = Some(crate::availability::TransferError {
                        message: "The file exceeds this device's retry limit".into(),
                        occurred_at_ms: now_ms(),
                    });
                    record.transfer.retry_count = 1;
                    record.transfer.retry_at_ms = None;
                }),
                Err(_) => update_retry_state(&registry, record_id, |record| {
                    record.transfer.phase = TransferPhase::Idle;
                    record.transfer.progress = None;
                    record.transfer.last_error = Some(crate::availability::TransferError {
                        message: "No available replica could provide this file".into(),
                        occurred_at_ms: now_ms(),
                    });
                    record.transfer.retry_count = 1;
                    record.transfer.retry_at_ms = None;
                }),
            }
        });
        Ok(())
    }

    /// Queue a policy-authorized removal of this file's local payload.
    /// Residency remains local until an exact backend observation proves every
    /// requested blob is absent.
    fn free_local_space(&self, ino: u64, mode: EvictionMode) -> Result<(), i32> {
        self.check_access(ino, Access::Read)?;
        let record_id = self.ns().record_id_for(ino).ok_or(ENOENT)?;
        let request_id = self.prepare_eviction_request(record_id, mode, None)?;
        self.spawn_eviction_observer(record_id, request_id)?;
        Ok(())
    }

    // The availability guard intentionally spans final authorization and the
    // backend handoff: releasing it across either await would permit a pin or
    // receipt change to race destructive authorization.
    #[allow(
        clippy::await_holding_lock,
        clippy::significant_drop_tightening,
        clippy::too_many_lines
    )]
    fn prepare_eviction_request(
        &self,
        record_id: collab_types::FileRecordId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
    ) -> Result<EvictionRequestId, i32> {
        let registry = self.availability.as_ref().ok_or(libc::ENOTSUP)?.clone();
        let queue = self.eviction_queue.as_ref().ok_or(libc::ENOTSUP)?.clone();
        let history = self
            .history
            .as_ref()
            .map(|(history, _)| history.clone())
            .ok_or(libc::ENOTSUP)?;
        // Snapshot the policy roots before backend inspection. We compare the
        // snapshot again while holding the registry through authorization and
        // tag release, so a concurrent pin/version/receipt change fails closed.
        let (records, mut version_keys) = {
            let registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let records: Vec<_> = registry.records().cloned().collect();
            let mut version_keys = Vec::new();
            for record in &records {
                if record.kind != RecordKind::File {
                    continue;
                }
                // Catalog exactly the roots the planner can consult: the
                // target, other resident/pinned current versions, and every
                // explicitly retained historical version. A metadata-only,
                // automatic remote file must not block unrelated eviction.
                let protects_current = record.record_id == record_id
                    || record.residency != Residency::NotResident
                    || registry
                        .effective_policy(record.record_id)
                        .map_err(|_| EBUSY)?
                        == Some(AvailabilityPolicy::Pinned);
                if protects_current && let Some(version) = record.current_version {
                    version_keys.push((record.record_id, version));
                }
                version_keys.extend(
                    record
                        .retained_versions
                        .iter()
                        .copied()
                        .map(|version| (record.record_id, version)),
                );
            }
            (records, version_keys)
        };
        version_keys.sort_unstable();
        version_keys.dedup();

        let store = self.hydrator.store.clone();
        let runtime = self.hydrator.runtime_handle.clone();
        let catalog = tokio::task::block_in_place(|| {
            runtime.block_on(async {
                let mut payloads = Vec::new();
                let mut empty = Vec::new();
                for (record, version_id) in version_keys {
                    let version = history
                        .snapshot(record)
                        .and_then(|snapshot| {
                            snapshot
                                .versions
                                .into_iter()
                                .find(|version| version.id == version_id)
                        })
                        .ok_or(EBUSY)?;
                    match version.content_hash {
                        Some(hash) => payloads.push(
                            store
                                .inspect_version_payload(record, version_id, Hash::from(hash.0))
                                .await
                                .map_err(|_| EIO)?,
                        ),
                        None => empty.push((record, version_id)),
                    }
                }
                EvictionCatalog::with_empty_versions(payloads, empty).map_err(|_| EBUSY)
            })
        })?;

        tokio::task::block_in_place(|| {
            runtime.block_on(async {
                let registry_guard = registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let current: Vec<_> = registry_guard.records().cloned().collect();
                if current != records {
                    return Err(EBUSY);
                }
                let plan = match mode {
                    EvictionMode::Safe => crate::eviction::plan_safe_eviction(
                        &registry_guard,
                        &catalog,
                        record_id,
                        now_ms(),
                        crate::availability::EvictionRequirements::default(),
                    ),
                    EvictionMode::DestructiveOverride => {
                        crate::eviction::plan_destructive_eviction(
                            &registry_guard,
                            &catalog,
                            record_id,
                            now_ms(),
                            crate::availability::EvictionRequirements::default(),
                        )
                    }
                }
                .map_err(|_| EBUSY)?;
                let mut queue_guard = queue.lock().await;
                let request_id = match existing_request {
                    Some(request_id) => request_id,
                    None => queue_guard.enqueue(&plan, now_ms()).map_err(|_| EIO)?.id,
                };
                store
                    .release_eviction_to_backend_gc(
                        &queue_guard,
                        request_id,
                        &registry_guard,
                        &catalog,
                        now_ms(),
                        crate::availability::EvictionRequirements::default(),
                    )
                    .await
                    .map_err(|_| EBUSY)?;
                Ok(request_id)
            })
        })
    }

    fn spawn_eviction_observer(
        &self,
        record_id: collab_types::FileRecordId,
        request_id: EvictionRequestId,
    ) -> Result<(), i32> {
        let registry = self.availability.as_ref().ok_or(libc::ENOTSUP)?.clone();
        let queue = self.eviction_queue.as_ref().ok_or(libc::ENOTSUP)?.clone();
        let store = self.hydrator.store.clone();
        // Backend GC is asynchronous. Persist NotResident only after an exact
        // all-absent observation; errors leave the durable request pending.
        self.hydrator.runtime_handle.spawn(async move {
            for _ in 0..=75 {
                let result = {
                    let mut queue = queue.lock().await;
                    store
                        .observe_eviction_request(&mut queue, request_id, now_ms())
                        .await
                };
                match result {
                    Ok(observation)
                        if matches!(observation.state, EvictionRequestState::Completed { .. }) =>
                    {
                        update_retry_state(&registry, record_id, |record| {
                            record.residency = Residency::NotResident;
                            record.freshness.local_verified_at_ms = None;
                        });
                        break;
                    }
                    Ok(_) => tokio::time::sleep(Duration::from_secs(1)).await,
                    Err(error) => {
                        eprintln!("could not observe physical eviction completion: {error}");
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// Resume durable cache-reclamation intents after a daemon restart. Every
    /// pending request is freshly reauthorized against current policy/history
    /// before owned tags are released again; completed observations repair a
    /// registry update that may have been interrupted by a crash.
    pub fn resume_pending_evictions(&self) {
        let (Some(queue), Some(registry)) = (&self.eviction_queue, &self.availability) else {
            return;
        };
        let queue = queue.clone();
        let requests = tokio::task::block_in_place(|| {
            self.hydrator
                .runtime_handle
                .block_on(async { queue.lock().await.requests().cloned().collect::<Vec<_>>() })
        });
        for request in requests {
            if matches!(request.state, EvictionRequestState::Completed { .. }) {
                update_retry_state(registry, request.record_id, |record| {
                    record.residency = Residency::NotResident;
                    record.freshness.local_verified_at_ms = None;
                });
                continue;
            }
            match self.prepare_eviction_request(request.record_id, request.mode, Some(request.id)) {
                Ok(request_id) => {
                    if let Err(error) = self.spawn_eviction_observer(request.record_id, request_id)
                    {
                        eprintln!("could not resume eviction observation: errno {error}");
                    }
                }
                Err(error) => {
                    eprintln!(
                        "pending eviction no longer passes current safety checks: errno {error}"
                    );
                }
            }
        }
    }

    /// The thumbnail (PNG bytes) for a file, generated as metadata ahead
    /// of/independent of content, or `None` when unavailable. Resolution
    /// order: (1) content-addressed cache hit, (2) a peer-gossiped
    /// thumbnail blob hash, hydrated (tiny, ahead of the file's content) and
    /// adopted, (3) host generation from fully-local content, cached and
    /// ingested so the hash gossips to peers. Never hydrates the file's
    /// *content* from the network to make a preview — only the host, which
    /// already has the bytes, generates.
    pub fn thumbnail_for(&self, ino: u64) -> Option<Vec<u8>> {
        let provider = self.thumbs.as_ref()?;
        let (name, content_hash, thumb_hash) = {
            let ns = self.ns();
            let name = ns
                .path_for(ino)
                .and_then(|p| p.rsplit('/').next().map(std::string::ToString::to_string))?;
            let result = (name, ns.hash_for(ino), ns.thumb_hash(ino));
            drop(ns);
            result
        };
        let category = thumbnail::category_for_path(std::path::Path::new(&name));
        if !category.has_preview() {
            return None;
        }
        // No content hash → an empty (or CRDT-document) file → nothing to preview.
        let content_hash = content_hash?;
        let ch = *content_hash.as_bytes();

        if let Some(png) = provider.cached(ch) {
            return Some(png);
        }

        // A peer gossiped a thumbnail blob — hydrate the tiny blob (ahead of
        // the content) and adopt it.
        if let Some(tbh) = thumb_hash
            && let Some(png) = self.read_blob_blocking(tbh)
            && let Some(out) = provider.adopt(ch, png)
        {
            return Some(out);
        }

        let bytes = self.local_content_blocking(content_hash)?;
        let properties = self.remember_rich_metadata(ino, &bytes);
        let png = provider.generate(ch, category, &bytes)?;

        // Record the hash on the node so the pointer projects to peers, who
        // then hydrate the blob (not the whole file) to show a preview.
        if let Some(tbh) = self.ingest_blob_blocking(&png) {
            if let Some(publisher) = &self.record_publisher {
                let record_id = self.ns().record_id_for(ino)?;
                let published = (|| {
                    let snapshot = publisher.snapshot(record_id).ok()??;
                    let revision = snapshot.content_revision?;
                    if revision.content_hash != Some(collab_types::ContentHash(ch)) {
                        return None;
                    }
                    let mut metadata = snapshot.metadata?;
                    metadata.previews.retain(|preview| preview.max_edge != 256);
                    metadata.previews.push(collab_types::PreviewVariant {
                        max_edge: 256,
                        hash: Some(collab_types::ContentHash(*tbh.as_bytes())),
                        status: collab_types::PreviewStatus::Ready,
                    });
                    metadata.latest_actor = Some(publisher.actor());
                    if properties.is_some() {
                        metadata.properties.clone_from(&properties);
                    }
                    metadata.observed_at_ms =
                        now_ms().max(metadata.observed_at_ms.saturating_add(1));
                    publisher.publish(&metadata, &[]).ok()?;
                    publisher
                        .snapshot(record_id)
                        .ok()??
                        .metadata?
                        .previews
                        .into_iter()
                        .filter(|preview| preview.status == collab_types::PreviewStatus::Ready)
                        .filter_map(|preview| preview.hash.map(|hash| (preview.max_edge, hash)))
                        .max_by_key(|(max_edge, _)| *max_edge)
                        .map(|(_, hash)| hash)
                })();
                if let Some(verified_hash) = published {
                    let _ = self
                        .ns()
                        .install_verified_thumbnail(record_id, Some(Hash::from(verified_hash.0)));
                }
            } else {
                let delta = {
                    let mut ns = self.ns();
                    ns.set_thumb(ino, &tbh);
                    ns.save_incremental()
                };
                if !delta.is_empty() {
                    self.publish(delta);
                }
            }
        }
        Some(png)
    }

    /// Full content of `hash` **only if entirely local** (every chunk in the
    /// cache); `None` otherwise, so making a preview never pulls content
    /// from the network.
    fn local_content_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        use crate::syncstate::SyncState;
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move {
                if hydrator.sync_state(hash).await != SyncState::Local {
                    return None;
                }
                hydrator.read(hash, 0, u64::MAX).await.ok()
            })
        })
    }

    /// Read a raw blob (the thumbnail), serving it locally or hydrating it from
    /// a peer — the small transfer that lets a preview arrive before content.
    fn read_blob_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read_blob(hash, 0, u64::MAX).await.ok() })
        })
    }

    /// Ingest the thumbnail PNG as a content-addressed blob, returning its hash.
    fn ingest_blob_blocking(&self, png: &[u8]) -> Option<Hash> {
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let data = png.to_vec();
        tokio::task::block_in_place(|| rt.block_on(async move { store.ingest(data).await.ok() }))
    }

    /// Bind this mount to a workspace ACL, enforcing per-identity access for
    /// `identity`. Every subsequent open/read/write is checked against `acl`
    /// as `identity`; without this the mount keeps Unix-veneer-only
    /// behaviour. Takes anything that becomes an [`AclHandle`]: a
    /// [`WorkspaceAcl`] for a policy that won't change, or a handle held by
    /// a [`crate::CapabilityFile`] for one that reloads under the live mount.
    #[must_use]
    pub fn with_acl(
        mut self,
        acl: impl Into<AclHandle>,
        identity: crate::acl::IdentityKey,
    ) -> Self {
        self.enforcement = Some((acl.into(), identity));
        self
    }

    /// The identity ACL check every open/read/write routes through. `Ok(())`
    /// when the mount has no ACL, or the bound identity holds `access`.
    /// Never `EACCES`: outside the view is `ENOENT`; inside but not
    /// writable is `EROFS`. The single production decision point.
    ///
    /// # Errors
    ///
    /// Returns `Err(ENOENT)` when `ino` doesn't resolve to a path or the identity can't see it,
    /// or `Err(EROFS)` when the identity can see it but lacks `access`.
    pub fn check_access(&self, ino: u64, access: Access) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let path = self.ns().path_for(ino).ok_or(ENOENT)?;
        self.check_path(&path, access)
    }

    /// [`Self::check_access`] against a path rather than an inode, for the
    /// operations whose subject is a name that may not exist yet.
    fn check_path(&self, path: &str, access: Access) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        if acl.permits(identity, path, access) {
            return Ok(());
        }
        if acl.visible(identity, path) && access == Access::Write {
            // In the view, and the view is read-only here.
            return Err(EROFS);
        }
        // Outside the view, or in it but not readable (an ancestor
        // directory the app may walk but not read as content). Either way
        // the honest answer is that there is nothing here.
        Err(ENOENT)
    }

    #[allow(clippy::significant_drop_tightening)]
    fn check_open_handle_access(
        &self,
        ino: u64,
        fh: FileHandle,
        access: Access,
    ) -> Result<(), i32> {
        if self.ns().getattr(ino).is_some() {
            return self.check_access(ino, access);
        }
        let handles = self
            .open_file_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let handle = handles
            .get(&fh.0)
            .filter(|handle| handle.ino == ino)
            .ok_or(ENOENT)?;
        if access == Access::Write && !handle.writable {
            return Err(EROFS);
        }
        self.check_path(&handle.opened_path, access)
    }

    /// Is `path` part of the view this mount serves? `true` for every path
    /// on a mount with no ACL attached — the filtered view is the ACL's, so
    /// without one there is nothing to filter.
    fn in_view(&self, path: &str) -> bool {
        match &self.enforcement {
            Some((acl, identity)) => acl.visible(identity, path),
            None => true,
        }
    }

    /// Is `ino` part of the view? An inode with no resolvable path
    /// (trashed, cyclic) is not.
    fn ino_in_view(&self, ino: u64) -> bool {
        if self.enforcement.is_none() {
            return true;
        }
        let path = self.ns().path_for(ino);
        match path {
            Some(path) => self.in_view(&path),
            None => false,
        }
    }

    /// The entries of `ino` that belong in the view, as `readdir` would
    /// report them. `None` when `ino` is not a directory in the view. `.`
    /// and `..` always survive — both are reached through this directory.
    fn visible_entries(&self, ino: u64) -> Option<Vec<(u64, FileType, collab_types::FileNameV1)>> {
        let (entries, dir_path) = {
            let mut ns = self.ns();
            (ns.readdir(ino), ns.path_for(ino))
        };
        let entries = entries?;
        let Some((acl, identity)) = &self.enforcement else {
            return Some(entries);
        };
        let dir_path = dir_path?;
        if !acl.visible(identity, &dir_path) {
            return None;
        }
        Some(
            entries
                .into_iter()
                .filter(|(_, _, name)| {
                    name == "."
                        || name == ".."
                        || acl.visible(identity, &child_path(&dir_path, name.as_bytes()))
                })
                .collect(),
        )
    }

    /// Pick the next two visible sibling files of the same extension for
    /// conservative predictive hydration. `visible_entries` is deliberately
    /// the source: using the raw namespace here would fetch content the mount's
    /// identity is not authorised to see.
    #[allow(clippy::significant_drop_tightening)]
    fn predictive_candidates(&self, ino: u64) -> Vec<(Hash, u64)> {
        const LOOKAHEAD: usize = 2;
        let (parent, current_name) = {
            let mut ns = self.ns();
            let parent = ns.parent_ino(ino);
            let name = parent
                .and_then(|parent| ns.readdir(parent))
                .and_then(|entries| {
                    entries
                        .into_iter()
                        .find(|entry| entry.0 == ino)
                        .map(|entry| entry.2)
                });
            (parent, name)
        };
        let (Some(parent), Some(current_name)) = (parent, current_name) else {
            return Vec::new();
        };
        let extension = std::path::Path::new(OsStr::from_bytes(current_name.as_bytes()))
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase);
        let Some(entries) = self.visible_entries(parent) else {
            return Vec::new();
        };
        let Some(position) = entries.iter().position(|(entry, _, _)| *entry == ino) else {
            return Vec::new();
        };
        let ns = self.ns();
        entries
            .into_iter()
            .skip(position.saturating_add(1))
            .filter(|(_, kind, name)| {
                *kind == FileType::RegularFile
                    && std::path::Path::new(OsStr::from_bytes(name.as_bytes()))
                        .extension()
                        .and_then(OsStr::to_str)
                        .map(str::to_ascii_lowercase)
                        == extension
            })
            .filter_map(|(candidate, _, _)| {
                let hash = ns.hash_for(candidate)?;
                let size = ns.getattr(candidate)?.size;
                Some((hash, size))
            })
            .take(LOOKAHEAD)
            .collect()
    }

    /// The link count to report for a directory: `2` plus one per
    /// subdirectory **in the view**, taken from the filtered listing rather
    /// than the namespace so `nlink` and `readdir` can't disagree — a count
    /// from the real tree would announce hidden subdirectories.
    fn visible_dir_nlink(&self, ino: u64) -> u32 {
        let subdirs = self.visible_entries(ino).map_or(0, |entries| {
            entries
                .iter()
                .filter(|(_, kind, name)| {
                    *kind == FileType::Directory && name != "." && name != ".."
                })
                .count()
        });
        2u32.saturating_add(u32::try_from(subdirs).unwrap_or(u32::MAX))
    }

    /// The check every "make a new name in this directory" operation runs
    /// (`create`, `mkdir`, rename's destination). A parent outside the view
    /// is `ENOENT`. The new path must be writable, and **that question is
    /// asked before the name is looked up** — a name the app may not create
    /// gets the same answer whether or not something already sits on it
    /// (otherwise `create` becomes an existence oracle). Only once the app
    /// *may* create the name does a collision with something hidden matter,
    /// refused with `EROFS` rather than written over.
    fn check_new_name(&self, parent: u64, name: &[u8]) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
        if !acl.visible(identity, &parent_path) {
            return Err(ENOENT);
        }
        let child = child_path(&parent_path, name);
        self.check_path(&child, Access::Write)?;
        let taken = self.ns().lookup(parent, name).is_some();
        if taken && !acl.visible(identity, &child) {
            return Err(EROFS);
        }
        Ok(())
    }

    /// The check for removing or renaming an existing name in `parent`:
    /// the entry must be in the view (else it is not there) and writable
    /// (else the view is read-only).
    fn check_existing_name(&self, parent: u64, name: &[u8]) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
        if !self.in_view(&parent_path) {
            return Err(ENOENT);
        }
        self.check_path(&child_path(&parent_path, name), Access::Write)
    }

    fn publish(&self, delta: Vec<u8>) {
        if let Some(sync) = &self.sync {
            sync.broadcast_delta(delta);
        }
        self.wake_pinned_hydration();
    }

    /// Coalesce local namespace/version changes into one worker pass. A new
    /// generation also makes the current hydration batch stop before it can
    /// finish an obsolete version or an unpinned file.
    fn wake_pinned_hydration(&self) {
        if let Some(worker) = &self.pinned_hydration {
            worker
                .wake
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }

    /// Synchronously content-chunk + ingest bytes, returning the manifest
    /// hash. `Ok(None)` for empty data; `Err` if ingest fails, so callers
    /// never persist a non-empty file with a missing hash.
    fn write_content_blocking(&self, data: &[u8]) -> io::Result<Option<Hash>> {
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let data = data.to_vec();
        tokio::task::block_in_place(|| rt.block_on(async move { store.write_content(&data).await }))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Current full content of an inode: the in-flight buffer if any, else
    /// the hydrated blob. `Ok(empty)` only for a genuinely empty/hashless
    /// file; a hydration failure is `Err` so a write never seeds from empty
    /// and silently truncates a not-yet-local file.
    fn read_full_blocking(&self, ino: u64) -> io::Result<Vec<u8>> {
        if let Some(buf) = self.buffers().get(&ino) {
            return Ok(buf.clone());
        }
        // Documents seed from their current serialized form (so a partial /
        // append write modifies the real content rather than an empty buffer).
        if self.is_document(ino) {
            return self
                .document_export(ino)
                .ok_or_else(|| io::Error::other("document export failed"));
        }
        let hash = self.ns().hash_for(ino);
        let Some(hash) = hash else {
            return Ok(Vec::new());
        };
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read(hash, 0, u64::MAX).await })
        })
        .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Ingest `data` as the new content of `ino` and broadcast the change.
    /// Aborts WITHOUT mutating the entry if ingest fails, preserving the
    /// previously-committed content.
    #[allow(clippy::significant_drop_tightening)]
    fn commit_content(&self, ino: u64, data: &[u8]) -> io::Result<()> {
        let hash = self.write_content_blocking(data)?;
        let byte_len = data.len() as u64;
        let (expected_hash, expected_len) = {
            let ns = self.ns();
            let expected_hash = ns.hash_for(ino);
            let expected_len = ns
                .getattr(ino)
                .ok_or_else(|| io::Error::from_raw_os_error(ENOENT))?
                .size;
            if expected_hash == hash && expected_len == byte_len {
                return Ok(());
            }
            (expected_hash, expected_len)
        };
        let snapshot = self
            .replacement_snapshot(ino, hash, byte_len)
            .ok_or_else(|| io::Error::from_raw_os_error(ENOENT))?;
        self.append_version(snapshot, collab_types::FileProvenance::Edit, now_ms())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let revision = match &self.record_publisher {
            Some(publisher) => publisher
                .snapshot(snapshot.record_id)
                .map_err(|error| io::Error::other(error.to_string()))?
                .and_then(|snapshot| snapshot.content_revision)
                .ok_or_else(|| io::Error::other("published record has no content revision"))
                .map(Some)?,
            None => None,
        };
        let delta = {
            let mut ns = self.ns();
            if ns.hash_for(ino) != expected_hash
                || ns.getattr(ino).is_none_or(|attr| attr.size != expected_len)
            {
                return Err(io::Error::from_raw_os_error(EBUSY));
            }
            if let Some(revision) = &revision {
                if !ns.set_content_revision(ino, revision, now_ms()) {
                    return Err(io::Error::other(
                        "could not install signed content revision",
                    ));
                }
            } else {
                ns.set_content(ino, hash.as_ref(), data.len() as u64, now_ms());
            }
            ns.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    /// Create an empty regular file. Signed mounts publish the immutable
    /// baseline before exposing the move-tree node; a failed publication can
    /// leave an unreachable repository record, but never an unsigned live file.
    #[allow(clippy::significant_drop_tightening)]
    fn create_empty_file(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
    ) -> Result<(u64, Vec<u8>), i32> {
        let name: &[u8] = name.as_ref();
        let timestamp_ms = now_ms();
        if let (Some(publisher), Some((history, actor))) = (&self.record_publisher, &self.history) {
            let path = {
                let mut namespace = self.ns();
                if namespace.lookup(parent, name).is_some() {
                    return Err(EEXIST);
                }
                let parent_path = namespace.path_for(parent).ok_or(ENOENT)?;
                child_path(&parent_path, name)
            };
            let record_id = collab_types::FileRecordId::new();
            let metadata_digest = VersionMetadataDigest {
                path: Some(path),
                kind: 2,
                byte_len: 0,
                deleted: false,
            };
            let metadata_hash = collab_types::ContentHash(
                *blake3::hash(&postcard::to_allocvec(&metadata_digest).map_err(|_| EIO)?)
                    .as_bytes(),
            );
            let version = collab_types::FileVersionV1 {
                id: collab_types::FileVersionId::new(),
                record_id,
                parents: Vec::new(),
                actor: *actor,
                timestamp_ms,
                content_hash: None,
                metadata_hash,
                provenance: collab_types::FileProvenance::Create,
            };
            let snapshot = VersionSnapshot {
                record_id,
                content_hash: None,
                metadata_hash,
                byte_len: 0,
                kind: FileType::RegularFile,
            };
            let parent_record = self.ns().record_id_for(parent).ok_or(EIO)?;
            publisher
                .publish_binding(&collab_types::FileBindingV1 {
                    schema_version: collab_types::FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent_record),
                    name: name.to_vec().into(),
                    kind: collab_types::FileNodeKind::File,
                    tombstoned: false,
                    actor: *actor,
                    observed_at_ms: timestamp_ms,
                })
                .map_err(|_| EIO)?;
            publisher
                .publish(
                    &self.record_metadata(&snapshot, timestamp_ms, *actor),
                    std::slice::from_ref(&version),
                )
                .map_err(|_| EIO)?;
            history.append(version).map_err(|_| EIO)?;
            let revision = publisher
                .snapshot(record_id)
                .map_err(|_| EIO)?
                .and_then(|snapshot| snapshot.content_revision)
                .ok_or(EIO)?;
            let mut namespace = self.ns();
            if namespace.lookup(parent, name).is_some() {
                return Err(EEXIST);
            }
            let ino = namespace
                .create_projected_file(parent, name, record_id, None, 0, timestamp_ms)
                .ok_or(EIO)?;
            if !namespace.install_verified_content_revision(revision) {
                let _ = namespace.remove(ino);
                return Err(EIO);
            }
            return Ok((ino, namespace.save_incremental()));
        }

        let mut namespace = self.ns();
        let ino = namespace
            .create_file(parent, name, None, 0, timestamp_ms)
            .ok_or(ENOENT)?;
        Ok((ino, namespace.save_incremental()))
    }

    #[allow(clippy::significant_drop_tightening)]
    fn create_empty_dir(&self, parent: u64, name: impl AsRef<[u8]>) -> Result<(u64, Vec<u8>), i32> {
        let name: &[u8] = name.as_ref();
        let timestamp_ms = now_ms();
        if let (Some(publisher), Some((history, actor))) = (&self.record_publisher, &self.history) {
            let (parent_record, path) = {
                let mut namespace = self.ns();
                if namespace.lookup(parent, name).is_some() {
                    return Err(EEXIST);
                }
                let parent_record = namespace.record_id_for(parent).ok_or(ENOENT)?;
                let parent_path = namespace.path_for(parent).ok_or(ENOENT)?;
                (parent_record, child_path(&parent_path, name))
            };
            let record_id = collab_types::FileRecordId::new();
            let metadata_digest = VersionMetadataDigest {
                path: Some(path),
                kind: 1,
                byte_len: 0,
                deleted: false,
            };
            let metadata_hash = collab_types::ContentHash(
                *blake3::hash(&postcard::to_allocvec(&metadata_digest).map_err(|_| EIO)?)
                    .as_bytes(),
            );
            let version = collab_types::FileVersionV1 {
                id: collab_types::FileVersionId::new(),
                record_id,
                parents: Vec::new(),
                actor: *actor,
                timestamp_ms,
                content_hash: None,
                metadata_hash,
                provenance: collab_types::FileProvenance::Create,
            };
            publisher
                .publish_binding(&collab_types::FileBindingV1 {
                    schema_version: collab_types::FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent_record),
                    name: name.to_vec().into(),
                    kind: collab_types::FileNodeKind::Directory,
                    tombstoned: false,
                    actor: *actor,
                    observed_at_ms: timestamp_ms,
                })
                .map_err(|_| EIO)?;
            publisher
                .publish_structural_version(&version)
                .map_err(|_| EIO)?;
            history.append(version).map_err(|_| EIO)?;
            let mut namespace = self.ns();
            if namespace.lookup(parent, name).is_some() {
                return Err(EEXIST);
            }
            let ino = namespace
                .create_projected_dir(parent, name, record_id, timestamp_ms)
                .ok_or(EIO)?;
            return Ok((ino, namespace.save_incremental()));
        }
        let mut namespace = self.ns();
        let ino = namespace
            .create_dir(parent, name, timestamp_ms)
            .ok_or(ENOENT)?;
        Ok((ino, namespace.save_incremental()))
    }

    fn remove_entry(&self, parent: INodeNo, name: &OsStr, expect_dir: bool, reply: ReplyEmpty) {
        match self.delete_named(parent.0, name.as_bytes(), expect_dir) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(to_errno(error)),
        }
    }

    /// Durably append the delete version before making the move-tree
    /// tombstone visible. Publication failure leaves the live namespace
    /// untouched; a concurrent edit/name replacement fails the final CAS.
    fn delete_named(&self, parent: u64, name: &[u8], expect_dir: bool) -> Result<(), i32> {
        self.check_existing_name(parent, name)?;
        let timestamp_ms = now_ms();
        let (ino, deleted_snapshot) = {
            let mut ns = self.ns();
            let attr = ns.lookup(parent, name).ok_or(ENOENT)?;
            let is_dir = attr.kind == FileType::Directory;
            if expect_dir && !is_dir {
                return Err(ENOTDIR);
            }
            if !expect_dir && is_dir {
                return Err(EISDIR);
            }
            if expect_dir {
                // A directory must be empty to rmdir (entries beyond "." "..").
                if let Some(entries) = ns.readdir(attr.ino.0)
                    && entries.len() > 2
                {
                    // Emptiness is judged on the real tree, since removing the directory
                    // takes its hidden children with it — if the app's own listing said
                    // empty, `ENOTEMPTY` would announce something it can't see, so this
                    // gets the same `EROFS` as any other refusal to write over unseen state.
                    drop(ns);
                    let visible = self.visible_entries(attr.ino.0).map_or(0, |e| e.len());
                    return Err(if visible > 2 { ENOTEMPTY } else { EROFS });
                }
            }
            let Some(deleted_snapshot) =
                Self::version_snapshot_locked(&ns, attr.ino.0, true, None, None)
            else {
                return Err(EIO);
            };
            (attr.ino.0, deleted_snapshot)
        };
        if self
            .append_version(
                deleted_snapshot,
                collab_types::FileProvenance::Delete,
                timestamp_ms,
            )
            .is_err()
        {
            return Err(EIO);
        }
        if let (Some(publisher), Some((_, actor))) = (&self.record_publisher, &self.history) {
            let mut binding = self
                .ns()
                .signed_binding_for(ino, *actor, timestamp_ms)
                .ok_or(EIO)?;
            binding.tombstoned = true;
            publisher.publish_binding(&binding).map_err(|_| EIO)?;
        }
        let delta = {
            let mut ns = self.ns();
            if ns.lookup(parent, name).is_none_or(|attr| attr.ino.0 != ino)
                || Self::version_snapshot_locked(&ns, ino, true, None, None)
                    != Some(deleted_snapshot)
            {
                return Err(EBUSY);
            }
            if expect_dir && ns.readdir(ino).is_some_and(|entries| entries.len() > 2) {
                return Err(ENOTEMPTY);
            }
            if !ns.remove(ino) {
                return Err(ENOENT);
            }
            ns.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    // All parameters are independent authenticated rename facts; the body is
    // one atomic signed replacement transaction.
    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_arguments,
        clippy::too_many_lines
    )]
    fn replacement_rename_named(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        new_parent: u64,
        new_name: impl AsRef<[u8]>,
        source_ino: u64,
        victim_ino: u64,
        timestamp_ms: u64,
    ) -> Result<Vec<u8>, i32> {
        let name: &[u8] = name.as_ref();
        let new_name: &[u8] = new_name.as_ref();
        let runtime = self.replacement_transactions.as_ref().ok_or(EEXIST)?;
        let publisher = self.record_publisher.as_ref().ok_or(EIO)?;
        let (history, actor) = self.history.as_ref().ok_or(EIO)?;
        let (
            source_current,
            victim_current,
            source_proposed,
            victim_proposed,
            source_binding,
            victim_binding,
            source_parent_record,
            destination_parent_record,
        ) = {
            let mut namespace = self.ns();
            let source_attr = namespace.getattr(source_ino).ok_or(ENOENT)?;
            let victim_attr = namespace.getattr(victim_ino).ok_or(ENOENT)?;
            if source_attr.kind != victim_attr.kind {
                return Err(if source_attr.kind == FileType::Directory {
                    ENOTDIR
                } else {
                    EISDIR
                });
            }
            if victim_attr.kind == FileType::Directory
                && namespace
                    .readdir(victim_ino)
                    .is_some_and(|entries| entries.len() > 2)
            {
                return Err(ENOTEMPTY);
            }
            let source_current =
                Self::version_snapshot_locked(&namespace, source_ino, false, None, None)
                    .ok_or(EIO)?;
            let victim_current =
                Self::version_snapshot_locked(&namespace, victim_ino, false, None, None)
                    .ok_or(EIO)?;
            let source_parent_record = namespace.record_id_for(parent).ok_or(EIO)?;
            let destination_parent_record = namespace.record_id_for(new_parent).ok_or(EIO)?;
            let destination_parent_path = namespace.path_for(new_parent).ok_or(ENOENT)?;
            let source_metadata = VersionMetadataDigest {
                path: Some(child_path(&destination_parent_path, new_name)),
                kind: if source_attr.kind == FileType::Directory {
                    1
                } else {
                    2
                },
                byte_len: source_attr.size,
                deleted: false,
            };
            let victim_metadata = VersionMetadataDigest {
                path: namespace.path_for(victim_ino),
                kind: if victim_attr.kind == FileType::Directory {
                    1
                } else {
                    2
                },
                byte_len: victim_attr.size,
                deleted: true,
            };
            let source_proposed = VersionSnapshot {
                metadata_hash: collab_types::ContentHash(
                    *blake3::hash(&postcard::to_allocvec(&source_metadata).map_err(|_| EIO)?)
                        .as_bytes(),
                ),
                ..source_current
            };
            let victim_proposed = VersionSnapshot {
                metadata_hash: collab_types::ContentHash(
                    *blake3::hash(&postcard::to_allocvec(&victim_metadata).map_err(|_| EIO)?)
                        .as_bytes(),
                ),
                ..victim_current
            };
            let node_kind = if source_attr.kind == FileType::Directory {
                collab_types::FileNodeKind::Directory
            } else {
                collab_types::FileNodeKind::File
            };
            let source_binding = collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: source_current.record_id,
                parent: Some(destination_parent_record),
                name: new_name.to_vec().into(),
                kind: node_kind,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            };
            let victim_binding = collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: victim_current.record_id,
                parent: Some(destination_parent_record),
                name: new_name.to_vec().into(),
                kind: node_kind,
                tombstoned: true,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            };
            (
                source_current,
                victim_current,
                source_proposed,
                victim_proposed,
                source_binding,
                victim_binding,
                source_parent_record,
                destination_parent_record,
            )
        };
        let source_heads = history.snapshot(source_current.record_id).ok_or(EIO)?.heads;
        let victim_heads = history.snapshot(victim_current.record_id).ok_or(EIO)?.heads;
        let ([source_expected], [victim_expected]) =
            (source_heads.as_slice(), victim_heads.as_slice())
        else {
            return Err(EBUSY);
        };
        let participant = |snapshot: VersionSnapshot,
                           binding: collab_types::FileBindingV1,
                           expected: collab_types::FileVersionId,
                           role: collab_types::FileMutationParticipantRoleV1,
                           provenance: collab_types::FileProvenance| {
            let version = collab_types::FileVersionV1 {
                id: collab_types::FileVersionId::new(),
                record_id: snapshot.record_id,
                parents: vec![expected],
                actor: *actor,
                timestamp_ms,
                content_hash: snapshot.content_hash,
                metadata_hash: snapshot.metadata_hash,
                provenance,
            };
            let proposed_content_revision = (snapshot.kind == FileType::RegularFile).then_some(
                collab_types::ContentRevisionV1 {
                    schema_version: collab_types::CONTENT_REVISION_VERSION,
                    record_id: snapshot.record_id,
                    version_id: version.id,
                    content_hash: snapshot.content_hash,
                    byte_len: snapshot.byte_len,
                    metadata_hash: snapshot.metadata_hash,
                },
            );
            let mut value = collab_types::FileMutationParticipantV1 {
                record_id: snapshot.record_id,
                role,
                expected_version: expected,
                proposed_binding: binding,
                proposed_version: version,
                proposed_content_revision,
                projection_hash: collab_types::ContentHash([0; 32]),
            };
            value.projection_hash = value.computed_projection_hash().map_err(|_| EIO)?;
            Ok::<_, i32>(value)
        };
        let mut participants = vec![
            participant(
                source_proposed,
                source_binding,
                *source_expected,
                collab_types::FileMutationParticipantRoleV1::Source,
                if parent == new_parent {
                    collab_types::FileProvenance::Rename
                } else {
                    collab_types::FileProvenance::Move
                },
            )?,
            participant(
                victim_proposed,
                victim_binding,
                *victim_expected,
                collab_types::FileMutationParticipantRoleV1::Replaced,
                collab_types::FileProvenance::Delete,
            )?,
        ];
        participants.sort_by_key(|value| value.record_id);
        let manifest = collab_types::FileMutationTransactionManifestV1 {
            schema_version: collab_types::FILE_MUTATION_TRANSACTION_VERSION,
            transaction_id: uuid::Uuid::new_v4(),
            workspace_id: publisher.workspace_id(),
            actor: *actor,
            timestamp_ms,
            operation: collab_types::FileMutationOperationV1::ReplacementRename {
                source_parent: source_parent_record,
                source_name: name.to_vec().into(),
                destination_parent: destination_parent_record,
                destination_name: new_name.to_vec().into(),
                source_record: source_current.record_id,
                replaced_record: victim_current.record_id,
            },
            participants,
        };
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut runtime
                    .journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                history,
                &self.namespace,
                &runtime.namespace_path,
                move |delta| {
                    if let Some(sync) = &sync {
                        sync.broadcast_delta(delta);
                    }
                    Ok(())
                },
            )
            .map_err(|_| EIO)?;
        Ok(Vec::new())
    }

    // Keep signed preflight, publication, and namespace projection ordered.
    #[allow(clippy::significant_drop_tightening, clippy::too_many_lines)]
    fn rename_named(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        new_parent: u64,
        new_name: impl AsRef<[u8]>,
    ) -> Result<Vec<u8>, i32> {
        let name: &[u8] = name.as_ref();
        let new_name: &[u8] = new_name.as_ref();
        let signed = self.record_publisher.is_some() && self.history.is_some();
        if !signed {
            let (ino, delta) = {
                let mut namespace = self.ns();
                let ino = namespace.lookup(parent, name).ok_or(ENOENT)?.ino.0;
                if !namespace.rename(parent, name, new_parent, new_name, now_ms()) {
                    return Err(ENOENT);
                }
                (ino, namespace.save_incremental())
            };
            self.record_version(
                ino,
                if parent == new_parent {
                    collab_types::FileProvenance::Rename
                } else {
                    collab_types::FileProvenance::Move
                },
                None,
            );
            return Ok(delta);
        }

        let timestamp_ms = now_ms();
        let (ino, expected, proposed, proposed_binding) = {
            let mut namespace = self.ns();
            let ino = namespace.lookup(parent, name).ok_or(ENOENT)?.ino.0;
            if let Some(victim) = namespace.lookup(new_parent, new_name)
                && victim.ino.0 != ino
            {
                let victim_ino = victim.ino.0;
                drop(namespace);
                return self.replacement_rename_named(
                    parent,
                    name,
                    new_parent,
                    new_name,
                    ino,
                    victim_ino,
                    timestamp_ms,
                );
            }
            let expected =
                Self::version_snapshot_locked(&namespace, ino, false, None, None).ok_or(EIO)?;
            let parent_path = namespace.path_for(new_parent).ok_or(ENOENT)?;
            let attr = namespace.getattr(ino).ok_or(ENOENT)?;
            let metadata = VersionMetadataDigest {
                path: Some(child_path(&parent_path, new_name)),
                kind: match attr.kind {
                    FileType::Directory => 1,
                    FileType::RegularFile => 2,
                    _ => 0,
                },
                byte_len: attr.size,
                deleted: false,
            };
            let bytes = postcard::to_allocvec(&metadata).map_err(|_| EIO)?;
            let proposed = VersionSnapshot {
                metadata_hash: collab_types::ContentHash(*blake3::hash(&bytes).as_bytes()),
                ..expected
            };
            let parent_record = namespace.record_id_for(new_parent).ok_or(EIO)?;
            let actor = self.history.as_ref().ok_or(EIO)?.1;
            let kind = match attr.kind {
                FileType::Directory => collab_types::FileNodeKind::Directory,
                FileType::RegularFile => collab_types::FileNodeKind::File,
                _ => return Err(EINVAL),
            };
            let binding = collab_types::FileBindingV1 {
                schema_version: collab_types::FILE_BINDING_VERSION,
                record_id: proposed.record_id,
                parent: Some(parent_record),
                name: new_name.to_vec().into(),
                kind,
                tombstoned: false,
                actor,
                observed_at_ms: timestamp_ms,
            };
            (ino, expected, proposed, binding)
        };
        self.append_version(
            proposed,
            if parent == new_parent {
                collab_types::FileProvenance::Rename
            } else {
                collab_types::FileProvenance::Move
            },
            timestamp_ms,
        )
        .map_err(|_| EIO)?;
        self.record_publisher
            .as_ref()
            .ok_or(EIO)?
            .publish_binding(&proposed_binding)
            .map_err(|_| EIO)?;
        let revision = if proposed.kind == FileType::RegularFile {
            Some(
                self.record_publisher
                    .as_ref()
                    .ok_or(EIO)?
                    .snapshot(proposed.record_id)
                    .map_err(|_| EIO)?
                    .and_then(|snapshot| snapshot.content_revision)
                    .ok_or(EIO)?,
            )
        } else {
            None
        };
        let mut namespace = self.ns();
        if namespace
            .lookup(parent, name)
            .is_none_or(|attr| attr.ino.0 != ino)
            || Self::version_snapshot_locked(&namespace, ino, false, None, None) != Some(expected)
            || namespace
                .lookup(new_parent, new_name)
                .is_some_and(|attr| attr.ino.0 != ino)
        {
            return Err(EBUSY);
        }
        if !namespace.rename(parent, name, new_parent, new_name, timestamp_ms) {
            return Err(ENOENT);
        }
        if let Some(revision) = revision {
            if !namespace.set_content_revision_preserving_preview(ino, &revision, timestamp_ms) {
                return Err(EIO);
            }
            if namespace.content_revision_for(ino) != Some(revision) {
                return Err(EIO);
            }
        }
        Ok(namespace.save_incremental())
    }
}

fn to_errno(e: i32) -> Errno {
    io::Error::from_raw_os_error(e).into()
}

/// Reply to a `getxattr` following the two-call protocol: `size == 0` asks for
/// the length; a non-zero size must be big enough for the value, else `ERANGE`.
fn reply_xattr_bytes(reply: ReplyXattr, bytes: &[u8], size: u32) {
    if size == 0 {
        reply.size(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    } else if (size as usize) < bytes.len() {
        reply.error(to_errno(libc::ERANGE));
    } else {
        reply.data(bytes);
    }
}

/// `dir` + `name`, as the ACL spells paths: absolute, no trailing slash,
/// `"/name"` directly beneath the root.
fn child_path(dir: &str, name: &[u8]) -> String {
    let name = crate::namespace::escape_path_name(name);
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn now_ms() -> u64 {
    crate::clock::unix_now_ms()
}

fn update_retry_state(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    record_id: collab_types::FileRecordId,
    update: impl FnOnce(&mut RecordAvailability),
) {
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut record) = registry.get(record_id).cloned() else {
        return;
    };
    update(&mut record);
    if let Err(error) = registry.put(record) {
        eprintln!("could not persist hydration retry state: {error}");
    }
}

fn pinned_candidates(
    namespace: &Arc<Mutex<Namespace>>,
    enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
) -> Vec<PinnedCandidate> {
    let namespace = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let visible =
        |path: &str| enforcement.is_none_or(|(acl, identity)| acl.visible(identity, path));
    let mut candidates = Vec::new();
    if let Some(record_id) = namespace.record_id_for(crate::namespace::ROOT_INO)
        && visible("/")
    {
        candidates.push(PinnedCandidate {
            record_id,
            parent: None,
            kind: RecordKind::Directory,
            version: None,
            hash: None,
            size: 0,
            path: "/".into(),
        });
    }
    for id in namespace.live_node_ids() {
        let Some(entry) = namespace.entry_for_node(&id) else {
            continue;
        };
        let Some(path) = namespace.path_for_id(&id) else {
            continue;
        };
        if !visible(&path) {
            continue;
        }
        candidates.push(PinnedCandidate {
            record_id: entry.record_id,
            parent: namespace.record_id_for_id(&entry.parent),
            kind: if namespace.node_is_dir(&id) == Some(true) {
                RecordKind::Directory
            } else {
                RecordKind::File
            },
            version: namespace
                .content_revision_for_id(&id)
                .map(|revision| revision.version_id),
            hash: entry.hash,
            size: entry.size,
            path,
        });
    }
    // Parents must enter the durable registry before descendants so ancestry
    // validation remains fail-closed without making initial scans fail.
    candidates.sort_by_key(|candidate| candidate.path.matches('/').count());
    candidates
}

fn update_candidate_state(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    candidate: &PinnedCandidate,
    update: impl FnOnce(&mut RecordAvailability),
) {
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut record) = registry.get(candidate.record_id).cloned() else {
        return;
    };
    if record.current_version != candidate.version {
        return;
    }
    update(&mut record);
    if let Err(error) = registry.put(record) {
        eprintln!("could not persist pinned hydration state: {error}");
    }
}

async fn reconcile_pinned_hydration(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
    wakes: tokio::sync::watch::Receiver<u64>,
) -> bool {
    let candidates = pinned_candidates(namespace, enforcement);
    let observed_at = now_ms();

    // First reconcile the complete visible ancestry. This makes a pin on a
    // stable directory automatically cover children introduced by a later
    // namespace generation and preserves that relation across restart.
    for candidate in &candidates {
        let mut registry = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut record = registry
            .get(candidate.record_id)
            .cloned()
            .unwrap_or_else(|| RecordAvailability::new(candidate.record_id, candidate.kind));
        let version_changed = record.current_version != candidate.version;
        let mut changed = record.kind != candidate.kind || record.parent != candidate.parent;
        record.kind = candidate.kind;
        record.parent = candidate.parent;
        if version_changed {
            record.current_version = candidate.version;
            record.residency = Residency::NotResident;
            record.transfer = crate::availability::TransferState::default();
            record.replica_observations.clear();
            record.replicas = crate::availability::ReplicaEvidence::default();
            record.freshness.local_verified_at_ms = None;
            record.freshness.replicas_observed_at_ms = None;
            record.freshness.metadata_observed_at_ms = Some(observed_at);
            changed = true;
        }
        if record.freshness.metadata_observed_at_ms.is_none() {
            record.freshness.metadata_observed_at_ms = Some(observed_at);
            changed = true;
        }
        if candidate.kind == RecordKind::Directory || candidate.hash.is_none() {
            let resident = Residency::Resident {
                bytes: candidate.size,
            };
            if record.residency != resident || record.transfer.phase != TransferPhase::Idle {
                record.residency = resident;
                record.transfer = crate::availability::TransferState::default();
                record.freshness.local_verified_at_ms = Some(observed_at);
                changed = true;
            }
        }
        if changed && let Err(error) = registry.put(record) {
            eprintln!("could not reconcile pinned record topology: {error}");
        }
    }

    let mut fetches = Vec::new();
    let mut more_due = false;
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.kind == RecordKind::File && candidate.hash.is_some())
    {
        // Policy is metadata-only and cheap to evaluate. Do it before probing
        // the chunk store so an automatic workspace does not re-walk every
        // manifest on each reconciliation tick.
        let pinned = {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let pinned = matches!(
                registry.effective_policy(candidate.record_id),
                Ok(Some(AvailabilityPolicy::Pinned))
            );
            if !pinned
                && let Some(mut record) = registry.get(candidate.record_id).cloned()
                && (record.transfer.retry_count > 0
                    || matches!(
                        record.transfer.phase,
                        TransferPhase::Queued | TransferPhase::Downloading
                    ))
            {
                record.transfer = crate::availability::TransferState::default();
                let _ = registry.put(record);
            }
            pinned
        };
        if !pinned {
            continue;
        }

        let state = hydrator
            .sync_state(candidate.hash.expect("filtered hash"))
            .await;
        if state == crate::syncstate::SyncState::Local {
            update_candidate_state(&registry, candidate, |record| {
                record.residency = Residency::Resident {
                    bytes: candidate.size,
                };
                record.transfer = crate::availability::TransferState::default();
                record.freshness.local_verified_at_ms = Some(observed_at);
            });
            continue;
        }
        let partial = hydrator
            .residency(candidate.hash.expect("filtered hash"))
            .await;
        let resident_bytes = partial.hydrated_bytes.min(candidate.size);
        let due = {
            let mut registry = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(mut record) = registry.get(candidate.record_id).cloned() else {
                continue;
            };
            // The worker owns queued work.  Do not turn a current download
            // back into another request when a coalesced wake arrives.
            if record.transfer.phase == TransferPhase::Downloading {
                continue;
            }
            record.residency = if resident_bytes == 0 {
                Residency::NotResident
            } else {
                Residency::Partial { resident_bytes }
            };
            record.transfer.phase = TransferPhase::Queued;
            record.transfer.progress = Some(crate::availability::TransferProgress {
                completed_bytes: resident_bytes,
                total_bytes: Some(candidate.size),
            });
            let due = record
                .transfer
                .retry_at_ms
                .is_none_or(|deadline| deadline <= observed_at);
            if let Err(error) = registry.put(record) {
                eprintln!("could not queue pinned hydration: {error}");
                false
            } else {
                due
            }
        };
        if due {
            if fetches.len() < MAX_PINNED_FETCHES_PER_PASS {
                fetches.push(candidate.clone());
            } else {
                more_due = true;
            }
        }
    }

    let mut in_flight = tokio::task::JoinSet::new();
    for candidate in fetches {
        in_flight.spawn(hydrate_pinned_candidate(
            candidate,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            wakes.clone(),
        ));
    }
    while in_flight.join_next().await.is_some() {}
    more_due
}

/// Run one member of a bounded scheduler batch.  All members observe the same
/// generation signal, so a policy or namespace version change cancels the
/// batch promptly; the subsequent coalesced pass chooses the current work.
async fn hydrate_pinned_candidate(
    candidate: PinnedCandidate,
    hydrator: Arc<Hydrator>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    mut wakes: tokio::sync::watch::Receiver<u64>,
) {
    let Some(hash) = candidate.hash else { return };
    update_candidate_state(&registry, &candidate, |record| {
        record.transfer.phase = TransferPhase::Downloading;
    });
    let mut last_persisted = 0_u64;
    let hydration = hydrator.prefetch_with_progress(hash, candidate.size, |completed, total| {
        if completed != total
            && completed.saturating_sub(last_persisted) < HYDRATION_PROGRESS_PERSIST_BYTES
        {
            return;
        }
        last_persisted = completed;
        update_candidate_state(&registry, &candidate, |record| {
            record.transfer.phase = TransferPhase::Downloading;
            record.transfer.progress = Some(crate::availability::TransferProgress {
                completed_bytes: completed,
                total_bytes: Some(total),
            });
        });
    });
    tokio::pin!(hydration);
    let result = tokio::select! {
        result = &mut hydration => Some(result),
        changed = wakes.changed() => {
            let _ = changed;
            None
        }
    };
    match result {
        Some(Ok(true)) => update_candidate_state(&registry, &candidate, |record| {
            record.residency = Residency::Resident {
                bytes: candidate.size,
            };
            record.transfer = crate::availability::TransferState::default();
            record.freshness.local_verified_at_ms = Some(now_ms());
        }),
        Some(Ok(false)) => update_candidate_state(&registry, &candidate, |record| {
            record.transfer.phase = TransferPhase::Idle;
            record.transfer.last_error = Some(crate::availability::TransferError {
                message: "The file exceeds this device's hydration limit".into(),
                occurred_at_ms: now_ms(),
            });
            record.transfer.retry_at_ms = None;
        }),
        Some(Err(_)) => update_candidate_state(&registry, &candidate, |record| {
            record.transfer.phase = TransferPhase::Queued;
            record.transfer.retry_count = record.transfer.retry_count.saturating_add(1);
            let delay = 1_u64 << record.transfer.retry_count.min(6);
            record.transfer.retry_at_ms = Some(now_ms().saturating_add(delay * 1_000));
            record.transfer.last_error = Some(crate::availability::TransferError {
                message: "No available replica could provide this file".into(),
                occurred_at_ms: now_ms(),
            });
        }),
        None => update_candidate_state(&registry, &candidate, |record| {
            record.transfer = crate::availability::TransferState::default();
        }),
    }
}

/// `offset`/`size` cross the FUSE boundary as untrusted request values,
/// so clamp rather than trust them to fit `usize` before bounding against `bytes.len()`.
fn slice(bytes: &[u8], offset: u64, size: u32) -> Vec<u8> {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// The ACL accesses an open of the given mode requires: read for
/// `O_RDONLY`, write for `O_WRONLY`, both for `O_RDWR`.
fn required_accesses(mode: OpenAccMode) -> &'static [Access] {
    match mode {
        OpenAccMode::O_RDONLY => &[Access::Read],
        OpenAccMode::O_WRONLY => &[Access::Write],
        OpenAccMode::O_RDWR => &[Access::Read, Access::Write],
    }
}

impl Filesystem for QuickFS {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), io::Error> {
        let _ = config.set_max_readahead(1024 * 1024);
        Ok(())
    }

    /// Resolve one name in a directory. Namespace lookup and view evaluation
    /// run on every call in the same order, so a nonexistent name and a name
    /// hidden by the view cost the same work and return the same `ENOENT`.
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.as_bytes();
        let (found, child) = {
            let mut ns = self.ns();
            let found = ns.lookup(parent.0, name);
            let child = ns.path_for(parent.0).map(|p| child_path(&p, name));
            drop(ns);
            (found, child)
        };
        let in_view = child.is_some_and(|c| self.in_view(&c));
        match found {
            Some(attr) if in_view => {
                let is_doc = self.ns().document_codec(attr.ino.0).is_some();
                let mut attr = attr;
                if attr.kind == FileType::Directory {
                    attr.nlink = self.visible_dir_nlink(attr.ino.0);
                }
                reply.entry(&Self::ttl_for(is_doc), &attr, Generation(0));
            }
            _ => reply.error(to_errno(ENOENT)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        // Reflect an in-flight write buffer so stat() sees read-your-writes size.
        let buf_len = self.buffers().get(&ino.0).map(|b| b.len() as u64);
        // Documents have no stored byte length — report the current export size
        // (so a collaboratively-edited document isn't read truncated).
        let is_doc = self.is_document(ino.0);
        let doc_len = if buf_len.is_none() && is_doc {
            self.document_export(ino.0).map(|b| b.len() as u64)
        } else {
            None
        };
        let open_handle = fh.and_then(|fh| {
            self.open_file_handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&fh.0)
                .filter(|handle| handle.ino == ino.0)
                .cloned()
        });
        let live = self.ns().getattr(ino.0).is_some();
        // Outside the live view there is nothing to describe, except an inode
        // retained by this exact open kernel handle after unlink/replacement.
        let visible = if live {
            self.ino_in_view(ino.0)
        } else {
            open_handle
                .as_ref()
                .is_some_and(|handle| self.in_view(&handle.opened_path))
        };
        if !visible {
            reply.error(to_errno(ENOENT));
            return;
        }
        let found = {
            let namespace = self.ns();
            namespace.getattr(ino.0).or_else(|| {
                open_handle
                    .as_ref()
                    .and_then(|_| namespace.getattr_including_unlinked(ino.0))
            })
        };
        match found {
            Some(mut attr) => {
                if let Some(len) = buf_len.or(doc_len) {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    // Matches the filtered listing: an identity granted
                    // nothing sees an ordinary empty directory (nlink == 2).
                    attr.nlink = self.visible_dir_nlink(ino.0);
                }
                reply.attr(&Self::ttl_for(is_doc), &attr);
            }
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        // Entries outside the view simply aren't in the directory; offsets
        // below index what the app can see, so paging never has a gap.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.visible_entries(ino.0) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, (e_ino, kind, name)) in entries.into_iter().enumerate().skip(skip) {
            let next_offset = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
            if reply.add(
                INodeNo(e_ino),
                next_offset,
                kind,
                OsStr::from_bytes(name.as_bytes()),
            ) {
                break;
            }
        }
        reply.ok();
        self.heuristics.on_readdir(ino.0);
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let opened_path = {
            let ns = self.ns();
            ns.getattr(ino.0).and_then(|_| ns.path_for(ino.0))
        };
        let Some(opened_path) = opened_path else {
            reply.error(to_errno(ENOENT));
            return;
        };
        // Requires the identity to hold the access the open mode implies.
        // Outside the view ⇒ `ENOENT`; in the view but not writable ⇒
        // `EROFS`, so opening for writing falls back to read-only.
        for access in required_accesses(flags.acc_mode()) {
            if let Err(e) = self.check_access(ino.0, *access) {
                reply.error(to_errno(e));
                return;
            }
        }
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        self.open_file_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle,
                OpenFileHandleState {
                    ino: ino.0,
                    opened_path,
                    writable: matches!(
                        flags.acc_mode(),
                        OpenAccMode::O_WRONLY | OpenAccMode::O_RDWR
                    ),
                },
            );
        reply.opened(FileHandle(handle), FopenFlags::empty());
        let candidates = self.predictive_candidates(ino.0);
        self.heuristics.on_open(ino.0, candidates);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let open_handle = self
            .open_file_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&fh.0)
            .filter(|handle| handle.ino == ino.0)
            .cloned();
        // Live inodes use their current path. An unlinked/replaced inode has
        // no path, so only the matching kernel handle may continue, with the
        // access path captured when it was opened.
        let live = self.ns().getattr(ino.0).is_some();
        let access = if live {
            self.check_access(ino.0, Access::Read)
        } else if let Some(handle) = &open_handle {
            self.check_path(&handle.opened_path, Access::Read)
        } else {
            Err(ENOENT)
        };
        if let Err(e) = access {
            reply.error(to_errno(e));
            return;
        }
        // Read-your-writes: serve from the in-flight buffer if present.
        if let Some(buf) = self.buffers().get(&ino.0) {
            reply.data(&slice(buf, offset, size));
            return;
        }

        // Documents: serve the cached serialization (one export per change).
        if self.is_document(ino.0) {
            match self.document_export(ino.0) {
                Some(b) => reply.data(&slice(&b, offset, size)),
                None => reply.error(to_errno(EIO)),
            }
            return;
        }

        let hash = {
            let ns = self.ns();
            match ns.getattr(ino.0).or_else(|| {
                open_handle
                    .as_ref()
                    .and_then(|_| ns.getattr_including_unlinked(ino.0))
            }) {
                Some(attr) if attr.kind == FileType::RegularFile => ns.hash_for(ino.0),
                Some(_) => {
                    reply.error(to_errno(EISDIR));
                    return;
                }
                None => {
                    reply.error(to_errno(ENOENT));
                    return;
                }
            }
        };
        let Some(hash) = hash else {
            reply.data(&[]);
            return;
        };

        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        let res = tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read(hash, offset, u64::from(size)).await })
        });
        match res {
            Ok(data) => reply.data(&data),
            Err(_) => reply.error(to_errno(EIO)),
        }
    }

    #[allow(clippy::significant_drop_tightening)]
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let name_bytes = name.as_bytes();
        if let Err(e) = self.check_new_name(parent.0, name_bytes) {
            reply.error(to_errno(e));
            return;
        }
        let signed = self.record_publisher.is_some() && self.history.is_some();
        let codec = (!signed)
            .then(|| name.to_str().and_then(|name| self.document_codec_for(name)))
            .flatten();
        let created =
            if let (false, Some(name), Some(codec)) = (signed, name.to_str(), codec.as_deref()) {
                let mut ns = self.ns();
                ns.create_document(parent.0, name, codec, now_ms())
                    .map(|ino| (ino, ns.save_incremental()))
                    .ok_or(ENOENT)
            } else {
                self.create_empty_file(parent.0, name_bytes)
            };
        let (ino, delta) = match created {
            Ok(created) => created,
            Err(error) => {
                reply.error(to_errno(error));
                return;
            }
        };
        let attr = self.ns().getattr(ino);
        let Some(attr) = attr else {
            reply.error(to_errno(EIO));
            return;
        };
        // Start a fresh empty buffer so subsequent writes accumulate.
        self.buffers().insert(attr.ino.0, Vec::new());
        self.publish(delta);
        if !signed {
            self.record_version(attr.ino.0, collab_types::FileProvenance::Create, None);
        }
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        let opened_path = self.ns().path_for(attr.ino.0);
        if let Some(opened_path) = opened_path {
            self.open_file_handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    handle,
                    OpenFileHandleState {
                        ino: attr.ino.0,
                        opened_path,
                        writable: true,
                    },
                );
        }
        reply.created(
            &Self::ttl_for(codec.is_some()),
            &attr,
            Generation(0),
            FileHandle(handle),
            FopenFlags::empty(),
        );
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is already scoped to its own block, after the blocking hydration \
                  read and offset validation happen outside it; what remains inside is the \
                  buffer lookup/resize/write itself, which needs the lock held throughout"
    )]
    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        // Per-identity ACL: writing requires the write cap on this path.
        if let Err(e) = self.check_open_handle_access(ino.0, fh, Access::Write) {
            reply.error(to_errno(e));
            return;
        }
        // Seed the buffer with current content if this is the first write,
        // outside the buffer lock (may do blocking I/O) — a hydration
        // failure must fail the write, never seed-from-empty and truncate.
        let absent = self.buffers().get(&ino.0).is_none();
        let seed = if absent {
            // Seed from current content (export for documents, hydrated bytes
            // for opaque files) so a partial/append write modifies it correctly.
            if let Ok(d) = self.read_full_blocking(ino.0) {
                Some(d)
            } else {
                reply.error(to_errno(EIO));
                return;
            }
        } else {
            None
        };

        // `offset` crosses the FUSE boundary as an untrusted request value.
        let Ok(start) = usize::try_from(offset) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let Some(end) = start.checked_add(data.len()) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        {
            let mut bufs = self.buffers();
            // or_insert keeps any buffer a racing op already created.
            let buf = match seed {
                Some(d) => bufs.entry(ino.0).or_insert(d),
                None => bufs.entry(ino.0).or_default(),
            };
            if end > buf.len() {
                buf.resize(end, 0);
            }
            let Some(dest) = buf.get_mut(start..end) else {
                reply.error(to_errno(EINVAL));
                return;
            };
            dest.copy_from_slice(data);
        }
        reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX));
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let buffered = self
            .write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ino.0)
            .cloned();
        let live = self.ns().getattr(ino.0).is_some();
        if live && let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let buffered = self
            .write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ino.0)
            .cloned();
        let live = self.ns().getattr(ino.0).is_some();
        if live && let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let (last_handle, released_writable) = {
            let mut handles = self
                .open_file_handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let released_writable = handles
                .remove(&fh.0)
                .is_some_and(|handle| handle.ino == ino.0 && handle.writable);
            (
                !handles.values().any(|handle| handle.ino == ino.0),
                released_writable,
            )
        };
        let buffered = self
            .write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ino.0)
            .cloned();
        let live = self.ns().getattr(ino.0).is_some();
        if live
            && released_writable
            && let Some(data) = buffered
        {
            self.commit_buffer(ino.0, &data);
        }
        if last_handle {
            self.write_buffers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&ino.0);
        }
        reply.ok();
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Truncation is a write. Everything else here is ignored anyway, but
        // a `setattr` on something outside the view must not describe it, so
        // the visibility check runs regardless.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if size.is_some()
            && let Err(e) = self.check_access(ino.0, Access::Write)
        {
            reply.error(to_errno(e));
            return;
        }
        // Handle truncation/extension; ignore perm/owner/time changes.
        if let Some(new_size) = size {
            let is_doc = self.is_document(ino.0);
            let Ok(mut data) = self.read_full_blocking(ino.0) else {
                reply.error(to_errno(EIO));
                return;
            };
            // `new_size` crosses the FUSE boundary as an untrusted request value.
            data.resize(usize::try_from(new_size).unwrap_or(usize::MAX), 0);
            let open = self.buffers().contains_key(&ino.0);
            // Documents always stage in the buffer (imported as a whole on
            // release); opaque files commit immediately when not open.
            if open || is_doc {
                self.buffers().insert(ino.0, data);
            } else if let Err(_e) = self.commit_content(ino.0, &data) {
                reply.error(to_errno(EIO));
                return;
            }
        }

        let buf_len = self.buffers().get(&ino.0).map(|b| b.len() as u64);
        let found = self.ns().getattr(ino.0);
        match found {
            Some(mut attr) => {
                if let Some(len) = buf_len {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    attr.nlink = self.visible_dir_nlink(ino.0);
                }
                reply.attr(&TTL, &attr);
            }
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name = name.as_bytes();
        if let Err(e) = self.check_new_name(parent.0, name) {
            reply.error(to_errno(e));
            return;
        }
        let (ino, delta) = match self.create_empty_dir(parent.0, name) {
            Ok(created) => created,
            Err(error) => {
                reply.error(to_errno(error));
                return;
            }
        };
        let attr = self.ns().getattr(ino);
        match attr {
            Some(attr) => {
                self.publish(delta);
                if self.record_publisher.is_none() || self.history.is_none() {
                    self.record_version(attr.ino.0, collab_types::FileProvenance::Create, None);
                }
                reply.entry(&TTL, &attr, Generation(0));
            }
            None => reply.error(to_errno(EIO)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, false, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, true, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let name = name.as_bytes();
        let newname = newname.as_bytes();
        if !flags.difference(RenameFlags::RENAME_NOREPLACE).is_empty() {
            reply.error(to_errno(libc::ENOTSUP));
            return;
        }
        if flags.contains(RenameFlags::RENAME_NOREPLACE)
            && self.ns().lookup(newparent.0, newname).is_some()
        {
            reply.error(to_errno(EEXIST));
            return;
        }
        // Both ends: the source must be a writable name in the view, the
        // destination a writable new name in a directory in the view.
        if let Err(e) = self
            .check_existing_name(parent.0, name)
            .and_then(|()| self.check_new_name(newparent.0, newname))
        {
            reply.error(to_errno(e));
            return;
        }
        match self.rename_named(parent.0, name, newparent.0, newname) {
            Ok(delta) => {
                if !delta.is_empty() {
                    self.publish(delta);
                }
                reply.ok();
            }
            Err(error) => reply.error(to_errno(error)),
        }
    }

    fn setxattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        name: &OsStr,
        value: &[u8],
        _flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if position != 0 {
            reply.error(to_errno(EINVAL));
            return;
        }
        if name.to_str() == Some(crate::syncstate::AVAILABILITY_CONTROL_XATTR) {
            let result = match value {
                b"retry" => self.retry_transfer(ino.0),
                b"evict" => self.free_local_space(ino.0, EvictionMode::Safe),
                b"evict-destructive" => {
                    self.free_local_space(ino.0, EvictionMode::DestructiveOverride)
                }
                _ => {
                    reply.error(to_errno(EINVAL));
                    return;
                }
            };
            match result {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if name.to_str() == Some(crate::history::HISTORY_CONTROL_XATTR) {
            if value.len() > collab_types::MAX_HISTORY_CONTROL_BYTES {
                reply.error(to_errno(libc::E2BIG));
                return;
            }
            let Ok(request) =
                serde_json::from_slice::<collab_types::HistoryControlRequestV1>(value)
            else {
                reply.error(to_errno(EINVAL));
                return;
            };
            if request.schema_version != collab_types::HISTORY_CONTROL_VERSION {
                reply.error(to_errno(EINVAL));
                return;
            }
            match request.operation {
                collab_types::HistoryControlOperationV1::Restore { version_id } => {
                    match self.restore_version(ino.0, version_id) {
                        Ok(()) => reply.ok(),
                        Err(errno) => reply.error(to_errno(errno)),
                    }
                }
                collab_types::HistoryControlOperationV1::ResolveConflict {
                    conflict_id,
                    strategy,
                    version_id,
                } => match self.resolve_conflict(ino.0, conflict_id, strategy, version_id) {
                    Ok(()) => reply.ok(),
                    Err(errno) => reply.error(to_errno(errno)),
                },
            }
            return;
        }
        if name.to_str() != Some(crate::syncstate::PIN_XATTR) {
            reply.error(to_errno(libc::ENOTSUP));
            return;
        }
        let policy = match value {
            b"pinned" => AvailabilityPolicy::Pinned,
            b"automatic" => AvailabilityPolicy::Automatic,
            _ => {
                reply.error(to_errno(EINVAL));
                return;
            }
        };
        match self.set_pin_policy(ino.0, policy) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    /// Serve the `user.qos.sync` extended attribute: the entry's hydration
    /// state (`local`/`projected`/`downloading`). Every other attribute name
    /// reports absent (`ENODATA`) — the collaborative FS owns only this one.
    // One dispatch point owns the bounded collaborative xattr protocol.
    #[allow(clippy::too_many_lines)]
    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        // Outside the view the inode doesn't exist — checked before the
        // attribute name, so the sync state never reports on a hidden file.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if name.to_str() == Some(crate::syncstate::SHARE_CONTROL_XATTR) {
            match (
                ino.0 == crate::namespace::ROOT_INO,
                &self.share_control_path,
            ) {
                (true, Some(path)) => {
                    reply_xattr_bytes(reply, path.as_os_str().as_encoded_bytes(), size);
                }
                _ => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::THUMB_XATTR) {
            match self.thumbnail_for(ino.0) {
                // The kernel caps a single xattr value at 64 KiB — report an
                // oversize preview as absent so the client falls back, rather than
                // failing with ERANGE.
                Some(png) if png.len() <= crate::thumbs::MAX_XATTR_THUMB => {
                    reply_xattr_bytes(reply, &png, size);
                }
                _ => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::CONTENT_HASH_XATTR) {
            let hash = self.ns().hash_for(ino.0);
            match hash {
                Some(hash) => reply_xattr_bytes(reply, hash.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::RECORD_ID_XATTR) {
            let record_id = self.ns().record_id_for(ino.0);
            match record_id {
                Some(record_id) => reply_xattr_bytes(reply, record_id.0.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::METADATA_XATTR) {
            match self.metadata_projection_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::RECORD_INDEX_XATTR) {
            if ino.0 != crate::namespace::ROOT_INO {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_projection() {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::AVAILABILITY_XATTR) {
            match self.availability_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::SHARE_XATTR) {
            match self.share_projection_for(ino.0, now_ms()) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::history::HISTORY_XATTR) {
            match self.history_projection_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(crate::syncstate::PIN_XATTR) {
            let Some(registry) = &self.availability else {
                reply.error(to_errno(libc::ENODATA));
                return;
            };
            let record_id = self.ns().record_id_for(ino.0);
            let policy = record_id.and_then(|id| {
                registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .effective_policy(id)
                    .ok()
                    .flatten()
            });
            match policy {
                Some(policy) => reply_xattr_bytes(
                    reply,
                    match policy {
                        AvailabilityPolicy::Pinned => b"pinned",
                        AvailabilityPolicy::Automatic => b"automatic",
                    },
                    size,
                ),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() != Some(crate::syncstate::SYNC_XATTR) {
            reply.error(to_errno(libc::ENODATA));
            return;
        }
        let Some(state) = self.sync_state_for(ino.0) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        reply_xattr_bytes(reply, state.as_str().as_bytes(), size);
    }

    /// List the extended attributes on `ino`: just `user.qos.sync` (NUL
    /// terminated), so `getfattr`/`listxattr` discover the sync attribute.
    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let (exists, name) = {
            let ns = self.ns();
            let exists = ns.getattr(ino.0).is_some();
            let name = ns
                .path_for(ino.0)
                .and_then(|p| p.rsplit('/').next().map(std::string::ToString::to_string));
            drop(ns);
            (exists, name)
        };
        if !exists {
            reply.error(to_errno(ENOENT));
            return;
        }
        let mut buf = Vec::with_capacity(crate::syncstate::SYNC_XATTR.len().saturating_add(1));
        buf.extend_from_slice(crate::syncstate::SYNC_XATTR.as_bytes());
        buf.push(0);
        // Cheap extension check, no generation — a `getxattr` of it may
        // still be `ENODATA` if the content isn't local yet.
        let previewable = self.thumbs.is_some()
            && name.as_deref().is_some_and(|n| {
                thumbnail::category_for_path(std::path::Path::new(n)).has_preview()
            });
        if previewable {
            buf.extend_from_slice(crate::syncstate::THUMB_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().hash_for(ino.0).is_some() {
            buf.extend_from_slice(crate::syncstate::CONTENT_HASH_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(crate::syncstate::RECORD_ID_XATTR.as_bytes());
            buf.push(0);
        }
        if self.metadata_projection_for(ino.0).is_some() {
            buf.extend_from_slice(crate::syncstate::METADATA_XATTR.as_bytes());
            buf.push(0);
        }
        if ino.0 == crate::namespace::ROOT_INO {
            buf.extend_from_slice(crate::syncstate::RECORD_INDEX_XATTR.as_bytes());
            buf.push(0);
        }
        if self.availability.is_some() {
            buf.extend_from_slice(crate::syncstate::AVAILABILITY_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(crate::syncstate::PIN_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(crate::syncstate::AVAILABILITY_CONTROL_XATTR.as_bytes());
            buf.push(0);
        }
        if self.share_authority.is_some() && self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(crate::syncstate::SHARE_XATTR.as_bytes());
            buf.push(0);
        }
        if self.history.is_some() && self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(crate::history::HISTORY_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(crate::history::HISTORY_CONTROL_XATTR.as_bytes());
            buf.push(0);
        }
        if ino.0 == crate::namespace::ROOT_INO && self.share_control_path.is_some() {
            buf.extend_from_slice(crate::syncstate::SHARE_CONTROL_XATTR.as_bytes());
            buf.push(0);
        }
        if size == 0 {
            reply.size(u32::try_from(buf.len()).unwrap_or(u32::MAX));
        } else if (size as usize) < buf.len() {
            reply.error(to_errno(libc::ERANGE));
        } else {
            reply.data(&buf);
        }
    }
}

#[cfg(test)]
mod availability_control_tests {
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    async fn wait_for_availability(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        record: collab_types::FileRecordId,
        predicate: impl Fn(&RecordAvailability) -> bool,
    ) {
        wait_for_availability_with_timeout(registry, record, Duration::from_secs(8), predicate)
            .await;
    }

    async fn wait_for_availability_with_timeout(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        record: collab_types::FileRecordId,
        timeout: Duration,
        predicate: impl Fn(&RecordAvailability) -> bool,
    ) {
        tokio::time::timeout(timeout, async {
            loop {
                if registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(record)
                    .is_some_and(&predicate)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("availability state barrier");
    }

    #[tokio::test]
    async fn pin_policy_persists_and_a_directory_covers_future_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("availability.json");
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(registry.clone());

        let folder = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_dir(ROOT_INO, "offline", 1)
            .unwrap();
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();

        // Created after the pin: no direct registry row is needed for the
        // effective projection to inherit the stable directory policy.
        let child = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(folder, "later.txt", None, 0, 2)
            .unwrap();
        let projected = crate::syncstate::AvailabilityProjectionV1::parse_xattr(
            &fs.availability_for(child).unwrap(),
        )
        .unwrap();
        assert_eq!(projected.policy, AvailabilityPolicy::Pinned);

        drop(fs);
        drop(registry);
        let reopened = AvailabilityRegistry::open(registry_path).unwrap();
        let folder_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(folder)
            .unwrap();
        assert_eq!(
            reopened.get(folder_id).map(|record| record.policy),
            Some(AvailabilityPolicy::Pinned)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pinned_folder_hydrates_future_child_retries_after_restart_and_tracks_new_version() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("availability.json");
        let source_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let source_store = ContentStore::memory();
        let first = source_store
            .write_content(b"first remote version")
            .await
            .unwrap()
            .unwrap();
        let second = source_store
            .write_content(b"second remote version")
            .await
            .unwrap()
            .unwrap();
        let source_router = Router::builder(source_endpoint.clone())
            .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
            .spawn();

        let client_endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(client_endpoint.clone(), client_store));
        let namespace = Arc::new(Mutex::new(Namespace::new("pinned-client").unwrap()));
        let folder = namespace
            .lock()
            .unwrap()
            .create_dir(ROOT_INO, "offline", 1)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(
            Arc::clone(&hydrator),
            Arc::clone(&namespace),
        ));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();

        // This file did not exist when its stable parent was pinned.
        let child = namespace
            .lock()
            .unwrap()
            .create_file(folder, "later.txt", Some(&first), 20, 2)
            .unwrap();
        let record_id = namespace.lock().unwrap().record_id_for(child).unwrap();
        let first_version = collab_types::FileVersionId::new();
        let first_revision = collab_types::ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id,
            version_id: first_version,
            content_hash: Some(collab_types::ContentHash(*first.as_bytes())),
            byte_len: 20,
            metadata_hash: collab_types::ContentHash([1; 32]),
        };
        assert!(
            namespace
                .lock()
                .unwrap()
                .set_content_revision(child, &first_revision, 2)
        );
        // This is the same wake issued after a local namespace publish. It
        // must not wait for the deliberately slow repair scan.
        fs.wake_pinned_hydration();

        // No peer is configured yet. The worker records an actionable error
        // and a durable backoff rather than spinning or claiming success.
        wait_for_availability(&registry, record_id, |record| {
            record.transfer.last_error.is_some()
                && record.transfer.retry_count > 0
                && record.transfer.retry_at_ms.is_some()
        })
        .await;
        drop(fs);
        drop(registry);

        // A process restart reopens the queued policy/error state. Once the
        // peer is reachable the inherited promise completes without another
        // user action.
        hydrator.add_peer(source_endpoint.addr());
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(
            Arc::clone(&hydrator),
            Arc::clone(&namespace),
        ));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        wait_for_availability(&registry, record_id, |record| {
            record.current_version == Some(first_version)
                && record.residency == Residency::Resident { bytes: 20 }
                && record.transfer.last_error.is_none()
        })
        .await;

        // A later immutable version invalidates the old residency proof and
        // is hydrated under the same durable ancestor pin.
        let second_version = collab_types::FileVersionId::new();
        let second_revision = collab_types::ContentRevisionV1 {
            version_id: second_version,
            content_hash: Some(collab_types::ContentHash(*second.as_bytes())),
            byte_len: 21,
            metadata_hash: collab_types::ContentHash([2; 32]),
            ..first_revision
        };
        assert!(
            namespace
                .lock()
                .unwrap()
                .set_content_revision(child, &second_revision, 3)
        );
        fs.wake_pinned_hydration();
        wait_for_availability_with_timeout(
            &registry,
            record_id,
            Duration::from_secs(2),
            |record| {
                record.current_version == Some(second_version)
                    && record.residency == Residency::Resident { bytes: 21 }
                    && record.transfer.last_error.is_none()
            },
        )
        .await;

        drop(fs);
        source_router.shutdown().await.unwrap();
        source_endpoint.close().await;
        client_endpoint.close().await;
    }

    #[tokio::test]
    async fn pinned_hydration_is_bounded_and_unpin_cancels_queued_retries() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let namespace = Arc::new(Mutex::new(Namespace::new("bounded-pins").unwrap()));
        let folder = namespace
            .lock()
            .unwrap()
            .create_dir(ROOT_INO, "offline", 1)
            .unwrap();
        for index in 0..10_u8 {
            namespace
                .lock()
                .unwrap()
                .create_file(
                    folder,
                    &format!("{index}.bin"),
                    Some(&Hash::from([index.saturating_add(1); 32])),
                    1,
                    2,
                )
                .unwrap();
        }
        let heuristics = Arc::new(Heuristics::new(
            Arc::clone(&hydrator),
            Arc::clone(&namespace),
        ));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry));
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();

        let (_wake, wakes) = tokio::sync::watch::channel(0_u64);
        reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes.clone(),
        )
        .await;
        let (attempted, queued) = {
            let registry = registry.lock().unwrap();
            let children = registry
                .records()
                .filter(|record| record.kind == RecordKind::File)
                .collect::<Vec<_>>();
            (
                children
                    .iter()
                    .filter(|record| record.transfer.retry_count == 1)
                    .count(),
                children
                    .iter()
                    .filter(|record| {
                        record.transfer.phase == TransferPhase::Queued
                            && record.transfer.retry_count == 0
                    })
                    .count(),
            )
        };
        assert_eq!(attempted, MAX_PINNED_FETCHES_PER_PASS);
        assert_eq!(queued, 10 - MAX_PINNED_FETCHES_PER_PASS);

        fs.set_pin_policy(folder, AvailabilityPolicy::Automatic)
            .unwrap();
        reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes,
        )
        .await;
        assert!(registry.lock().unwrap().records().all(|record| {
            record.kind == RecordKind::Directory
                || (record.transfer.phase == TransferPhase::Idle
                    && record.transfer.retry_count == 0
                    && record.transfer.retry_at_ms.is_none())
        }));
        endpoint.close().await;
    }

    #[tokio::test]
    async fn a_read_only_collaborator_can_manage_device_local_offline_policy() {
        const READER: [u8; 32] = [0x51; 32];
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "shared.txt", None, 0, 1)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let mut acl = crate::acl::WorkspaceAcl::new();
        acl.grant_read(READER, "/shared.txt");
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(Arc::clone(&registry))
            .with_acl(acl, READER);

        assert_eq!(fs.check_access(ino, Access::Write), Err(EROFS));
        fs.set_pin_policy(ino, AvailabilityPolicy::Pinned).unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        assert_eq!(
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(record_id)
                .unwrap()
                .policy,
            AvailabilityPolicy::Pinned
        );
    }

    #[tokio::test]
    async fn retry_transfer_runs_off_thread_and_persists_completion() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hash = store
            .write_content(b"available locally")
            .await
            .unwrap()
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(registry.clone());
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "retry.txt", Some(&hash), 17, 1)
            .unwrap();
        let record = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();

        fs.retry_transfer(ino).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let done = registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(record)
                    .is_some_and(|state| {
                        state.transfer.phase == TransferPhase::Idle
                            && state.transfer.last_error.is_none()
                            && state.residency == Residency::Resident { bytes: 17 }
                    });
                if done {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retry completion event");
    }

    #[tokio::test]
    async fn retrying_an_empty_file_completes_without_a_blob_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(registry.clone());
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "empty.txt", None, 0, 1)
            .unwrap();
        let record = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();

        fs.retry_transfer(ino).unwrap();
        assert_eq!(
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(record)
                .unwrap()
                .residency,
            Residency::Resident { bytes: 0 }
        );
    }

    #[tokio::test]
    async fn retry_failure_is_coarse_and_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(registry.clone());
        let missing = Hash::from([9; 32]);
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "missing.txt", Some(&missing), 4, 1)
            .unwrap();
        let record = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();

        fs.retry_transfer(ino).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let failure = registry
                    .lock()
                    .unwrap()
                    .get(record)
                    .and_then(|state| state.transfer.last_error.clone());
                if let Some(failure) = failure {
                    assert_eq!(
                        failure.message,
                        "No available replica could provide this file"
                    );
                    assert!(!failure.message.contains(&missing.to_string()));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retry failure event");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_reconciles_completed_eviction_into_availability() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "empty.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let version_id = collab_types::FileVersionId::new();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let mut availability = RecordAvailability::new(record_id, RecordKind::File);
        availability.current_version = Some(version_id);
        availability.residency = Residency::Resident { bytes: 0 };
        registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(availability)
            .unwrap();
        let mut queue = EvictionQueue::open(dir.path().join("evictions.json")).unwrap();
        let plan = crate::eviction::EvictionPlan {
            record_id,
            version_id,
            mode: crate::eviction::EvictionMode::Safe,
            reclaimable: Vec::new(),
            shared_and_protected: Vec::new(),
            already_absent: Vec::new(),
            reclaimable_bytes: 0,
            unaccounted_reclaimable_blobs: 0,
            protected_shared_bytes: 0,
            unaccounted_protected_shared_blobs: 0,
        };
        let request_id = queue.enqueue(&plan, 1).unwrap().id;
        let observation = store
            .observe_eviction_request(&mut queue, request_id, 2)
            .await
            .unwrap();
        assert!(matches!(
            observation.state,
            EvictionRequestState::Completed { .. }
        ));
        let queue = Arc::new(tokio::sync::Mutex::new(queue));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_availability(Arc::clone(&registry))
            .with_eviction_queue(queue);

        fs.resume_pending_evictions();
        assert_eq!(
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(record_id)
                .unwrap()
                .residency,
            Residency::NotResident
        );
    }
}

#[cfg(test)]
mod share_projection_tests {
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::share_authority::{ShareAuthority, ShareGrantStatus, ShareProjectionV1};
    use crate::store::ContentStore;
    use collab_types::{
        Ed25519PublicKey, FileInviteSpec, FileInviteSubject, FilePermission, mint_file_invite,
    };
    use iroh::endpoint::{Endpoint, presets};

    #[tokio::test]
    async fn quickfs_projects_authority_state_by_stable_record_id() {
        let dir = tempfile::tempdir().unwrap();
        let authority =
            Arc::new(ShareAuthority::open(dir.path().join("share-authority.json")).unwrap());
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "shared.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let (issuer_key, issuer) = collab_types::generate_keypair();
        let (_, recipient) = collab_types::generate_keypair();
        let token = mint_file_invite(
            FileInviteSpec {
                workspace_id: uuid::Uuid::new_v4(),
                record_id,
                subject: FileInviteSubject::Person(recipient),
                permission: FilePermission::View,
                expires_at_ms: None,
                max_redemptions: None,
            },
            &issuer_key,
        )
        .unwrap();
        authority.register(&token, &[issuer], 1).unwrap();

        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs =
            QuickFS::new(namespace, hydrator, heuristics).with_share_authority(authority.clone());

        let projected =
            ShareProjectionV1::parse_xattr(&fs.share_projection_for(ino, 2).expect("share xattr"))
                .unwrap();
        assert_eq!(projected.record_id, record_id);
        assert_eq!(projected.grants.len(), 1);
        assert_eq!(projected.grants[0].status, ShareGrantStatus::Active);
        assert_eq!(
            projected.grants[0].subject,
            crate::share_authority::ShareSubjectProjection::Person {
                person: Ed25519PublicKey::from_bytes(*recipient.as_bytes())
            }
        );

        authority.revoke(token.id).unwrap();
        let revoked =
            ShareProjectionV1::parse_xattr(&fs.share_projection_for(ino, 3).expect("share xattr"))
                .unwrap();
        assert_eq!(revoked.grants[0].status, ShareGrantStatus::Revoked);
    }
}

#[cfg(test)]
mod thumb_tests {
    //! The host-side thumbnail metadata path: `QuickFS::thumbnail_for` is
    //! the exact code `getxattr(user.qos.thumb)` runs, so driving it
    //! exercises the production path end to end.
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use crate::thumbs::ThumbProvider;
    use iroh::endpoint::Endpoint;
    use iroh::endpoint::presets;
    use std::io::Cursor;
    use uuid::Uuid;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = image::RgbaImage::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    async fn fixture(
        thumb_dir: std::path::PathBuf,
    ) -> (Arc<Mutex<Namespace>>, Arc<ContentStore>, QuickFS) {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let provider = ThumbProvider::open(&thumb_dir).unwrap();
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_thumbs(provider);
        (namespace, store, fs)
    }

    /// The headline: the host generates a preview from a file's local
    /// content, serves it as metadata, records its blob hash on the node so
    /// it projects to peers, and a second request hits the cache.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_generates_a_thumbnail_and_records_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

        // A real image file whose content is local to the host.
        let png = solid_png(600, 400, [40, 130, 220]);
        let mh = store.write_content(&png).await.unwrap().unwrap();
        let ino = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ns.create_file(ROOT_INO, "photo.png", Some(&mh), png.len() as u64, 1)
                .unwrap()
        };

        // No thumbnail recorded yet.
        assert!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .thumb_hash(ino)
                .is_none()
        );

        // The metadata path returns a real, sized PNG preview.
        let thumb = fs.thumbnail_for(ino).expect("host generates a preview");
        let decoded = thumbnail::decode_png(&thumb).expect("it is a valid PNG");
        assert!(decoded.is_valid());
        assert_eq!(
            decoded.max_edge(),
            crate::thumbs::HOST_EDGE,
            "downscaled to the host preview size"
        );
        assert!(
            decoded.max_edge() < u32::try_from(png.len()).unwrap_or(u32::MAX),
            "the preview is far smaller than the source"
        );

        // The thumbnail's blob hash is now on the node (so it gossips), and the
        // blob is a real content blob in the store peers can hydrate.
        let tbh = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .thumb_hash(ino)
            .expect("thumbnail hash recorded on the node → projects to peers");
        assert!(
            store.has(tbh).await,
            "the thumbnail blob is stored for peers"
        );

        // A second request returns the identical bytes from the cache.
        let again = fs.thumbnail_for(ino).expect("cached");
        assert_eq!(
            again, thumb,
            "second request is a cache hit, identical bytes"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signed_mount_publishes_and_installs_verified_thumbnail_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let image = solid_png(320, 240, [35, 90, 180]);
        let manifest = store.write_content(&image).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                ROOT_INO,
                "signed.png",
                Some(&manifest),
                image.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x72; 32]);
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
        let actor = collab_types::FileActor {
            person: collab_types::Ed25519PublicKey(person.verifying_key().to_bytes()),
            device: collab_types::Ed25519PublicKey(device.verifying_key().to_bytes()),
        };
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(dir.path().join("records")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                Uuid::from_u128(0x7065),
                actor,
                device,
            )
            .unwrap(),
        );
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_thumbs(ThumbProvider::open(dir.path().join("thumbs")).unwrap())
            .with_history(history, actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap();

        assert!(fs.thumbnail_for(ino).is_some());
        let verified = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .thumb_hash(ino)
            .expect("signed preview is installed in the verified projection");
        let snapshot = publisher.snapshot(record_id).unwrap().unwrap();
        assert!(snapshot.metadata.unwrap().previews.iter().any(|preview| {
            preview.status == collab_types::PreviewStatus::Ready
                && preview.hash == Some(collab_types::ContentHash(*verified.as_bytes()))
        }));
    }

    /// A preview survives a remount: a fresh mount over the same cache
    /// directory serves the same thumbnail without re-reading the file's content.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thumbnail_survives_a_remount() {
        let dir = tempfile::tempdir().unwrap();
        let png = solid_png(320, 240, [200, 60, 30]);

        let thumb = {
            let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;
            let mh = store.write_content(&png).await.unwrap().unwrap();
            let ino = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                    .unwrap()
            };
            fs.thumbnail_for(ino).expect("generated")
        };

        // A brand-new mount (fresh store, content NOT local) over the same
        // thumb cache still serves the preview — proof it came from the
        // persistent cache, not a re-decode.
        let (namespace2, store2, fs2) = fixture(dir.path().join("thumbs")).await;
        // Same content hash → same cache key. Register the node without the bytes.
        let mh = store2.write_content(&png).await.unwrap().unwrap();
        // Remove the content so it is not local on this mount.
        let ino2 = {
            let mut ns = namespace2
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                .unwrap()
        };
        let served = fs2
            .thumbnail_for(ino2)
            .expect("served from the on-disk cache");
        assert_eq!(
            served, thumb,
            "same content → same cached preview after remount"
        );
    }

    /// A file whose content is not local yields no thumbnail (the host never
    /// hydrates content just to preview it); a badge-only type never does either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_local_content_and_badge_types_yield_no_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, _store, fs) = fixture(dir.path().join("thumbs")).await;

        // A projected image: a hash the store never ingested (not local).
        let projected = iroh_blobs::Hash::from([3u8; 32]);
        let img_ino = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ns.create_file(ROOT_INO, "remote.png", Some(&projected), 4096, 1)
                .unwrap()
        };
        assert!(
            fs.thumbnail_for(img_ino).is_none(),
            "no preview without local content — never hydrate the file to make one"
        );

        // A non-previewable type is never a thumbnail regardless of content.
        let zip_ino = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ns.create_file(ROOT_INO, "bundle.zip", Some(&projected), 4096, 1)
                .unwrap()
        };
        assert!(
            fs.thumbnail_for(zip_ino).is_none(),
            "a .zip is a badge, not a preview"
        );
    }

    /// The projected-preview-before-content path: a peer with only gossiped
    /// metadata (thumbnail blob hash + blob) shows a preview WITHOUT the
    /// file's content ever being local.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gossiped_thumbnail_is_served_before_content() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

        // The real (small) thumbnail a host generated, present as a content blob
        // (as it would be after hydrating the tiny blob from the host).
        let preview = thumbnail::encode_png(
            &thumbnail::image_thumb(&solid_png(256, 256, [90, 200, 120]), 256).unwrap(),
        )
        .unwrap();
        let thumb_blob = store.ingest(preview.clone()).await.unwrap();

        // The file's content is projected (not local), but the node carries the
        // gossiped thumbnail hash.
        let projected = iroh_blobs::Hash::from([5u8; 32]);
        let ino = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ino = ns
                .create_file(ROOT_INO, "big-photo.png", Some(&projected), 9_000_000, 1)
                .unwrap();
            ns.set_thumb(ino, &thumb_blob);
            ino
        };

        let served = fs
            .thumbnail_for(ino)
            .expect("a projected preview is served before content");
        assert_eq!(
            served, preview,
            "the gossiped thumbnail is served without local content"
        );
    }
}

#[cfg(test)]
mod predictive_tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};

    async fn fixture() -> (Arc<Mutex<Namespace>>, QuickFS) {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        (
            namespace.clone(),
            QuickFS::new(namespace, hydrator, heuristics),
        )
    }

    #[allow(clippy::significant_drop_tightening)]
    #[tokio::test]
    async fn opening_an_image_selects_the_next_two_images_not_other_types() {
        let (namespace, fs) = fixture().await;
        let (first, second_hash, third_hash) = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = ns
                .create_file(ROOT_INO, "01.png", Some(&Hash::from([1; 32])), 10, 1)
                .unwrap();
            ns.create_file(ROOT_INO, "02.txt", Some(&Hash::from([2; 32])), 20, 1)
                .unwrap();
            let second_hash = Hash::from([3; 32]);
            ns.create_file(ROOT_INO, "03.PNG", Some(&second_hash), 30, 1)
                .unwrap();
            let third_hash = Hash::from([4; 32]);
            ns.create_file(ROOT_INO, "04.png", Some(&third_hash), 40, 1)
                .unwrap();
            ns.create_file(ROOT_INO, "05.png", Some(&Hash::from([5; 32])), 50, 1)
                .unwrap();
            (first, second_hash, third_hash)
        };

        assert_eq!(
            fs.predictive_candidates(first),
            vec![(second_hash, 30), (third_hash, 40)]
        );
    }

    #[allow(clippy::significant_drop_tightening)]
    #[tokio::test]
    async fn prediction_never_includes_a_sibling_hidden_by_the_acl() {
        const ME: [u8; 32] = [0xA1; 32];
        let (namespace, fs) = fixture().await;
        let (first, visible_hash) = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = ns
                .create_file(ROOT_INO, "01.png", Some(&Hash::from([1; 32])), 10, 1)
                .unwrap();
            ns.create_file(ROOT_INO, "02.png", Some(&Hash::from([2; 32])), 20, 1)
                .unwrap();
            let visible_hash = Hash::from([3; 32]);
            ns.create_file(ROOT_INO, "03.png", Some(&visible_hash), 30, 1)
                .unwrap();
            (first, visible_hash)
        };
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ME, "/01.png").grant_read(ME, "/03.png");
        let fs = fs.with_acl(acl, ME);

        assert_eq!(
            fs.predictive_candidates(first),
            vec![(visible_hash, 30)],
            "the raw namespace's /02.png must not cross the filtered-view boundary"
        );
    }

    #[tokio::test]
    async fn record_catalog_contains_nested_records_but_not_acl_hidden_siblings() {
        const ME: [u8; 32] = [0xA2; 32];
        let (namespace, fs) = fixture().await;
        let (visible_record, hidden_record) = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = ns.create_dir(ROOT_INO, "folder", 1).unwrap();
            let visible = ns
                .create_file(folder, "visible.txt", Some(&Hash::from([1; 32])), 10, 1)
                .unwrap();
            let hidden = ns
                .create_file(ROOT_INO, "hidden.txt", Some(&Hash::from([2; 32])), 20, 1)
                .unwrap();
            (
                ns.record_id_for(visible).unwrap(),
                ns.record_id_for(hidden).unwrap(),
            )
        };
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ME, "/folder/visible.txt");
        let fs = fs.with_acl(acl, ME);

        let bytes = fs.record_index_projection().expect("root record catalog");
        let projection: crate::syncstate::RecordIndexProjectionV1 =
            serde_json::from_slice(&bytes).unwrap();
        projection.validate().unwrap();
        assert!(projection.records.iter().any(|entry| {
            entry.record_id == visible_record && entry.path == "/folder/visible.txt"
        }));
        assert!(
            projection
                .records
                .iter()
                .all(|entry| entry.record_id != hidden_record),
            "a record outside the mount's ACL-filtered view cannot enter search metadata"
        );
    }
}

#[cfg(test)]
mod history_integration_tests {
    use super::*;
    use collab_types::{Ed25519PublicKey, FileActor, FileProvenance};
    use iroh::endpoint::{Endpoint, presets};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn existing_record_edit_and_rename_append_durable_versions() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(crate::namespace::ROOT_INO, "before.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();

        let baseline = history.snapshot(record_id).unwrap();
        assert_eq!(baseline.versions.len(), 1);
        assert_eq!(baseline.versions[0].provenance, FileProvenance::Create);

        fs.commit_content(ino, b"durable bytes").unwrap();
        let after_edit = history.snapshot(record_id).unwrap();
        assert_eq!(after_edit.versions.len(), 2);
        assert_eq!(after_edit.versions[1].provenance, FileProvenance::Edit);
        assert_eq!(after_edit.versions[1].parents, baseline.heads);
        assert_eq!(
            after_edit.versions[1].content_hash,
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(ino)
                .map(|hash| collab_types::ContentHash(*hash.as_bytes()))
        );
        fs.commit_content(ino, b"durable bytes").unwrap();
        assert_eq!(
            history.snapshot(record_id).unwrap().versions.len(),
            2,
            "flush/fsync/release of unchanged bytes must not invent versions"
        );

        assert!(
            !fs.rename_named(
                crate::namespace::ROOT_INO,
                "before.txt",
                crate::namespace::ROOT_INO,
                "after.txt",
            )
            .unwrap()
            .is_empty()
        );
        let after_rename = history.snapshot(record_id).unwrap();
        assert_eq!(after_rename.versions.len(), 3);
        assert_eq!(after_rename.versions[2].provenance, FileProvenance::Rename);
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino),
            Some(record_id)
        );

        drop(fs);
        drop(history);
        let reopened =
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap();
        assert_eq!(reopened.snapshot(record_id).unwrap().versions.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restoring_history_rehydrates_bytes_and_appends_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"first durable revision";
        let original_hash = store.write_content(original).await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "history.txt",
                Some(&original_hash),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();
        let original_version = history.snapshot(record_id).unwrap().versions[0].id;

        fs.commit_content(ino, b"second revision").unwrap();
        fs.restore_version(ino, original_version).unwrap();

        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        let snapshot = history.snapshot(record_id).unwrap();
        assert_eq!(snapshot.versions.len(), 3);
        assert_eq!(
            snapshot.versions[2].provenance,
            FileProvenance::Restore {
                from: original_version
            }
        );
        assert_eq!(
            snapshot.versions[0].content_hash,
            snapshot.versions[2].content_hash
        );
    }
}

#[cfg(test)]
mod record_publisher_integration_tests {
    use super::*;
    use collab_types::{Ed25519PublicKey, FileActor, FileProvenance};
    use ed25519_dalek::SigningKey;
    use iroh::endpoint::{Endpoint, presets};
    use uuid::Uuid;

    fn actor_and_signer() -> (FileActor, SigningKey) {
        let person = SigningKey::from_bytes(&[0x41; 32]);
        let device = SigningKey::from_bytes(&[0x52; 32]);
        (
            FileActor {
                person: Ed25519PublicKey(person.verifying_key().to_bytes()),
                device: Ed25519PublicKey(device.verifying_key().to_bytes()),
            },
            device,
        )
    }

    fn published_snapshot(
        repository: &crate::record_repository::RecordRepository,
        publisher: &crate::record_publisher::RecordPublisher,
        record_id: collab_types::FileRecordId,
    ) -> crate::record_protocol::RecordProtocolSnapshotV1 {
        let key = crate::record_repository::RecordRepositoryKey {
            workspace_id: publisher.workspace_id(),
            record_id,
        };
        repository
            .snapshot(
                key,
                &publisher.actor(),
                &publisher.local_authorizer(record_id),
            )
            .unwrap()
            .into()
    }

    // End-to-end signed publication/restart scenario; keeping its ordered
    // durability assertions together makes partial publication visible.
    #[allow(clippy::too_many_lines)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publisher_attach_mutations_and_restart_preserve_exact_record_state() {
        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join("history.json");
        let repository_path = dir.path().join("records");
        let workspace_id = Uuid::from_u128(0x51);
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"opaque bytes that predate record publishing";
        let original_manifest = store.write_content(original).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "before.txt",
                Some(&original_manifest),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&repository_path).unwrap());
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository.clone(),
                workspace_id,
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let key = crate::record_repository::RecordRepositoryKey {
            workspace_id,
            record_id,
        };

        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();
        assert!(
            !repository.contains(key),
            "history attachment alone must not masquerade as signed publication"
        );
        let fs = fs.with_record_publisher(publisher.clone()).unwrap();

        let baseline = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(baseline.record_id, record_id);
        assert_eq!(baseline.binding.as_ref().unwrap().name, "before.txt");
        assert_eq!(baseline.versions.len(), 1);
        assert_eq!(baseline.versions[0].provenance, FileProvenance::Create);
        let baseline_metadata = baseline.metadata.as_ref().unwrap();
        assert_eq!(
            baseline_metadata.content_hash,
            Some(collab_types::ContentHash(*original_manifest.as_bytes()))
        );
        assert_eq!(baseline_metadata.byte_len, original.len() as u64);

        // The protocol-visible content hash is the manifest root itself, not
        // a hash of reconstructed file bytes or an incidental chunk hash.
        let manifest_bytes = store.local(original_manifest).await.unwrap();
        let manifest = crate::content::Manifest::decode(&manifest_bytes).unwrap();
        let mut reconstructed = Vec::new();
        for chunk in manifest.chunks {
            let chunk_hash = Hash::from(chunk.hash);
            let bytes = store.local(chunk_hash).await.unwrap();
            assert_eq!(bytes.len(), chunk.len as usize);
            reconstructed.extend_from_slice(&bytes);
        }
        assert_eq!(reconstructed, original);

        let baseline_sequence = repository.highest_sequence(key, &actor).unwrap();
        let edited = b"new durable bytes";
        let record_file = repository_path
            .join(workspace_id.to_string())
            .join(format!("{}.json", record_id.0));
        let atomic_temp = record_file.parent().unwrap().join(format!(
            ".{}.{}.tmp",
            record_file.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        std::fs::create_dir(&atomic_temp).unwrap();
        assert!(
            fs.commit_content(ino, edited).is_err(),
            "namespace mutation must wait for durable signed publication"
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(ino),
            Some(original_manifest)
        );
        assert_eq!(history.snapshot(record_id).unwrap().versions.len(), 1);
        std::fs::remove_dir(&atomic_temp).unwrap();

        fs.commit_content(ino, edited).unwrap();
        let after_edit = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_edit.versions.len(), 2);
        assert!(
            after_edit
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Edit)
        );
        let edited_manifest = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hash_for(ino)
            .unwrap();
        assert_eq!(
            after_edit.metadata.as_ref().unwrap().content_hash,
            Some(collab_types::ContentHash(*edited_manifest.as_bytes()))
        );
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(baseline_sequence + 3),
            "an edit publishes a signed version, atomic revision, and metadata"
        );

        let before_noop = after_edit;
        let before_noop_sequence = repository.highest_sequence(key, &actor);
        fs.commit_content(ino, edited).unwrap();
        assert_eq!(
            published_snapshot(&repository, &publisher, record_id),
            before_noop,
            "an unchanged commit must add neither a version nor metadata"
        );
        assert_eq!(
            repository.highest_sequence(key, &actor),
            before_noop_sequence
        );

        assert!(
            !fs.rename_named(
                crate::namespace::ROOT_INO,
                "before.txt",
                crate::namespace::ROOT_INO,
                "after.txt",
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino),
            Some(record_id)
        );
        let after_rename = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_rename.binding.as_ref().unwrap().name, "after.txt");
        assert_eq!(
            after_rename.binding.as_ref().unwrap().parent,
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(crate::namespace::ROOT_INO)
        );
        assert_eq!(after_rename.versions.len(), 3);
        assert!(
            after_rename
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Rename)
        );
        assert_eq!(
            after_rename.metadata.as_ref().unwrap().content_hash,
            Some(collab_types::ContentHash(*edited_manifest.as_bytes()))
        );
        let installed_revision = {
            let namespace = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let node_id = namespace.id_for_ino(ino).unwrap();
            namespace.content_revision_for_id(node_id)
        };
        assert_eq!(
            installed_revision, after_rename.content_revision,
            "rename installs the exact signed revision while preserving content"
        );
        let sequence_before_restart = repository.highest_sequence(key, &actor).unwrap();
        let namespace_bytes = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .save();

        drop(fs);
        drop(publisher);
        drop(repository);
        drop(history);

        let namespace = Arc::new(Mutex::new(
            Namespace::from_bytes("restarted-host", &namespace_bytes).unwrap(),
        ));
        let restarted_ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lookup(crate::namespace::ROOT_INO, "after.txt")
            .unwrap()
            .ino
            .0;
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(restarted_ino),
            Some(record_id)
        );
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&repository_path).unwrap());
        let (_, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository.clone(),
                workspace_id,
                actor,
                signer,
            )
            .unwrap(),
        );
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let restarted = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap();
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(sequence_before_restart),
            "restart backfill must observe the durable watermark and replay nothing"
        );
        assert_eq!(
            published_snapshot(&repository, &publisher, record_id),
            after_rename
        );

        restarted
            .commit_content(restarted_ino, b"edit after restart")
            .unwrap();
        let after_restart_edit = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_restart_edit.versions.len(), 4);
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(sequence_before_restart + 3),
            "new operations must continue after the recovered actor watermark"
        );

        let (created_ino, delta) = restarted
            .create_empty_file(crate::namespace::ROOT_INO, "signed-new.txt")
            .unwrap();
        assert!(!delta.is_empty());
        let created_record = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(created_ino)
            .unwrap();
        let created = publisher.snapshot(created_record).unwrap().unwrap();
        assert_eq!(created.binding.as_ref().unwrap().name, "signed-new.txt");
        assert_eq!(created.versions.len(), 1);
        assert_eq!(created.versions[0].provenance, FileProvenance::Create);
        assert_eq!(
            created.content_revision.unwrap().version_id,
            created.versions[0].id
        );
        assert_eq!(
            restarted
                .namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(created_ino),
            None,
            "the empty signed baseline is visible only through its verified revision"
        );

        let (created_dir_ino, dir_delta) = restarted
            .create_empty_dir(crate::namespace::ROOT_INO, "signed-folder")
            .unwrap();
        assert!(!dir_delta.is_empty());
        let created_dir_record = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(created_dir_ino)
            .unwrap();
        let created_dir = publisher.snapshot(created_dir_record).unwrap().unwrap();
        assert_eq!(created_dir.binding.as_ref().unwrap().name, "signed-folder");
        assert_eq!(
            created_dir.binding.as_ref().unwrap().kind,
            collab_types::FileNodeKind::Directory
        );
        assert_eq!(created_dir.versions.len(), 1);
        assert!(created_dir.metadata.is_none());
        restarted
            .rename_named(
                crate::namespace::ROOT_INO,
                "signed-folder",
                crate::namespace::ROOT_INO,
                "renamed-folder",
            )
            .unwrap();
        let renamed_dir = publisher.snapshot(created_dir_record).unwrap().unwrap();
        assert_eq!(renamed_dir.binding.as_ref().unwrap().name, "renamed-folder");
        assert_eq!(renamed_dir.versions.len(), 2);
        assert!(
            renamed_dir
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Rename)
        );
        assert!(renamed_dir.metadata.is_none());

        // Until the durable transaction coordinator is wired through this
        // FUSE entry point, occupied destination must fail before either
        // participant acquires a version or repository operation.
        let source_before = publisher.snapshot(record_id).unwrap().unwrap();
        let destination_before = publisher.snapshot(created_record).unwrap().unwrap();
        let source_sequence = repository.highest_sequence(key, &actor);
        let destination_key = crate::record_repository::RecordRepositoryKey {
            workspace_id,
            record_id: created_record,
        };
        let destination_sequence = repository.highest_sequence(destination_key, &actor);
        let source_history = history.snapshot(record_id).unwrap();
        let destination_history = history.snapshot(created_record).unwrap();
        assert_eq!(
            restarted.rename_named(
                crate::namespace::ROOT_INO,
                "after.txt",
                crate::namespace::ROOT_INO,
                "signed-new.txt",
            ),
            Err(EEXIST)
        );
        let mut namespace = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "after.txt")
                .unwrap()
                .ino
                .0,
            restarted_ino
        );
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "signed-new.txt")
                .unwrap()
                .ino
                .0,
            created_ino
        );
        drop(namespace);
        assert_eq!(
            publisher.snapshot(record_id).unwrap().unwrap(),
            source_before
        );
        assert_eq!(
            publisher.snapshot(created_record).unwrap().unwrap(),
            destination_before
        );
        assert_eq!(repository.highest_sequence(key, &actor), source_sequence);
        assert_eq!(
            repository.highest_sequence(destination_key, &actor),
            destination_sequence
        );
        assert_eq!(history.snapshot(record_id).unwrap(), source_history);
        assert_eq!(
            history.snapshot(created_record).unwrap(),
            destination_history
        );

        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("record-transactions"),
            )
            .unwrap(),
        ));
        let namespace_path = dir.path().join("namespace-transaction.bin");
        let restarted = restarted
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        assert!(
            restarted
                .rename_named(
                    crate::namespace::ROOT_INO,
                    "after.txt",
                    crate::namespace::ROOT_INO,
                    "signed-new.txt",
                )
                .unwrap()
                .is_empty(),
            "the coordinator broadcasts the staged delta itself"
        );
        let mut namespace = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "after.txt")
                .is_none()
        );
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "signed-new.txt")
                .unwrap()
                .ino
                .0,
            restarted_ino
        );
        assert!(namespace.ino_for_record_id(created_record).is_none());
        drop(namespace);
        assert!(namespace_path.exists());
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .count(),
            0
        );
        let source_after_replacement = publisher.snapshot(record_id).unwrap().unwrap();
        let victim_after_replacement = publisher.snapshot(created_record).unwrap().unwrap();
        assert_eq!(
            source_after_replacement.binding.as_ref().unwrap().name,
            "signed-new.txt"
        );
        assert!(
            victim_after_replacement
                .binding
                .as_ref()
                .unwrap()
                .tombstoned
        );
        assert_eq!(
            source_after_replacement.versions.len(),
            source_before.versions.len() + 1
        );
        assert_eq!(
            victim_after_replacement.versions.len(),
            destination_before.versions.len() + 1
        );
    }

    // Full conflict-resolution matrix across durable history and restart.
    #[allow(clippy::too_many_lines)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keep_current_and_use_other_append_signed_convergent_resolutions() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"original branch";
        let original_manifest = store.write_content(original).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "conflicted.txt",
                Some(&original_manifest),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(dir.path().join("records")).unwrap(),
        );
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let namespace_path = dir.path().join("namespace.automerge");
        let fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        let original_version = history.snapshot(record_id).unwrap().versions[0].id;

        fs.commit_content(ino, b"current branch").unwrap();
        let current_version = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .content_revision_for(ino)
            .unwrap()
            .version_id;
        let first_conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![original_version, current_version],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(first_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&first_conflict))
            .unwrap();

        fs.resolve_conflict(
            ino,
            first_conflict.id,
            collab_types::ConflictStrategy::KeepCurrent,
            current_version,
        )
        .unwrap();
        assert_eq!(fs.read_full_blocking(ino).unwrap(), b"current branch");
        let after_keep = history.snapshot(record_id).unwrap();
        let keep_resolution = after_keep
            .conflicts
            .iter()
            .find(|conflict| conflict.id == first_conflict.id)
            .unwrap()
            .resolution
            .clone()
            .unwrap();
        assert_eq!(
            keep_resolution.strategy,
            collab_types::ConflictStrategy::KeepCurrent
        );
        let keep_result = after_keep
            .versions
            .iter()
            .find(|version| version.id == keep_resolution.result)
            .unwrap();
        assert_eq!(
            keep_result.provenance,
            collab_types::FileProvenance::ConflictResolution
        );
        assert!(
            first_conflict
                .versions
                .iter()
                .all(|member| keep_result.parents.contains(member))
        );

        let second_conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![keep_result.id, original_version],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(second_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&second_conflict))
            .unwrap();
        fs.resolve_conflict(
            ino,
            second_conflict.id,
            collab_types::ConflictStrategy::UseOther,
            original_version,
        )
        .unwrap();
        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        let signed_state = publisher.snapshot(record_id).unwrap().unwrap();
        let resolution = signed_state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == second_conflict.id)
            .unwrap()
            .resolution
            .as_ref()
            .unwrap();
        assert_eq!(
            resolution.strategy,
            collab_types::ConflictStrategy::UseOther
        );
        assert_eq!(
            signed_state.content_revision.unwrap().version_id,
            resolution.result,
            "the signed visible revision and deterministic resolution winner must agree"
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .content_revision_for(ino)
                .unwrap()
                .version_id,
            resolution.result
        );

        let source_current = resolution.result;
        let keep_both_conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![source_current, keep_result.id],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(keep_both_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&keep_both_conflict))
            .unwrap();
        let keep_both_transaction_id = keep_both_uuid(
            b"transaction",
            publisher.workspace_id(),
            record_id,
            keep_both_conflict.id,
            keep_result.id,
            actor,
        );
        let target_record = collab_types::FileRecordId(keep_both_uuid(
            b"target-record",
            publisher.workspace_id(),
            record_id,
            keep_both_conflict.id,
            keep_result.id,
            actor,
        ));
        crate::transaction_coordinator::arm_coordinator_fault(
            keep_both_transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterHistoryAppend,
        );
        assert_eq!(
            fs.resolve_conflict(
                ino,
                keep_both_conflict.id,
                collab_types::ConflictStrategy::KeepBoth,
                keep_result.id,
            ),
            Err(EIO)
        );
        assert!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(
                    crate::namespace::ROOT_INO,
                    keep_both_name("conflicted.txt", target_record)
                )
                .is_none(),
            "history-only crash must expose no sibling"
        );
        assert!(
            history
                .snapshot(record_id)
                .unwrap()
                .conflicts
                .iter()
                .find(|value| value.id == keep_both_conflict.id)
                .unwrap()
                .resolution
                .is_none()
        );
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        crate::transaction_coordinator::arm_coordinator_fault(
            keep_both_transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterNamespaceDeltaStaged,
        );
        let staged_recovery = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone());
        assert!(staged_recovery.is_err());
        assert!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(
                    crate::namespace::ROOT_INO,
                    keep_both_name("conflicted.txt", target_record),
                )
                .is_none(),
            "durably staged namespace bytes remain invisible before publication"
        );
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(keep_both_transaction_id)
                .unwrap()
                .phase,
            crate::record_transaction::RecordTransactionPhase::HistoryCommitted
        );
        drop(publisher);
        drop(history);
        drop(journal);
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        fs.resolve_conflict(
            ino,
            keep_both_conflict.id,
            collab_types::ConflictStrategy::KeepBoth,
            keep_result.id,
        )
        .unwrap();
        let target_name = keep_both_name("conflicted.txt", target_record);
        let target_ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lookup(crate::namespace::ROOT_INO, &target_name)
            .expect("Keep Both sibling is atomically visible")
            .ino
            .0;
        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        assert_eq!(
            fs.read_full_blocking(target_ino).unwrap(),
            b"current branch"
        );
        let source_after = publisher.snapshot(record_id).unwrap().unwrap();
        let target_after = publisher.snapshot(target_record).unwrap().unwrap();
        assert_eq!(target_after.binding.as_ref().unwrap().name, target_name);
        assert_eq!(target_after.versions.len(), 1);
        assert_eq!(
            source_after
                .conflicts
                .iter()
                .find(|conflict| conflict.id == keep_both_conflict.id)
                .unwrap()
                .resolution
                .as_ref()
                .unwrap()
                .strategy,
            collab_types::ConflictStrategy::KeepBoth
        );
        let follow_on_conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![
                source_after.content_revision.unwrap().version_id,
                keep_result.id,
            ],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(follow_on_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&follow_on_conflict))
            .unwrap();
        assert!(
            publisher
                .snapshot(record_id)
                .unwrap()
                .unwrap()
                .conflicts
                .iter()
                .any(|conflict| conflict.id == follow_on_conflict.id),
            "a later signed conflict may reference the committed transaction revision"
        );
        let source_versions = history.snapshot(record_id).unwrap().versions.len();
        fs.resolve_conflict(
            ino,
            keep_both_conflict.id,
            collab_types::ConflictStrategy::KeepBoth,
            keep_result.id,
        )
        .unwrap();
        assert_eq!(
            history.snapshot(record_id).unwrap().versions.len(),
            source_versions
        );
        assert_eq!(history.snapshot(target_record).unwrap().versions.len(), 1);
        assert!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
        assert!(namespace_path.exists());

        // A real reopen reconstructs both signed record projections from the
        // atomic namespace snapshot and treats the same request as an exact
        // replay, without another version or sibling.
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);
        drop(namespace);
        let restarted_namespace = Arc::new(Mutex::new(
            Namespace::from_bytes("host-restart", &std::fs::read(&namespace_path).unwrap())
                .unwrap(),
        ));
        let restarted_history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let restarted_publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer,
            )
            .unwrap(),
        );
        let restarted_journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let restarted_hydrator = Arc::new(Hydrator::new(endpoint, store));
        let restarted_heuristics = Arc::new(Heuristics::new(
            restarted_hydrator.clone(),
            restarted_namespace.clone(),
        ));
        let restarted = QuickFS::new(
            restarted_namespace.clone(),
            restarted_hydrator,
            restarted_heuristics,
        )
        .without_legacy_record_migration()
        .with_history(restarted_history.clone(), actor)
        .unwrap()
        .with_record_publisher(restarted_publisher)
        .unwrap()
        .with_replacement_transactions(restarted_journal.clone(), namespace_path)
        .unwrap();
        let restarted_source = restarted_namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ensure_ino_for_record_id(record_id)
            .unwrap();
        restarted
            .resolve_conflict(
                restarted_source,
                keep_both_conflict.id,
                collab_types::ConflictStrategy::KeepBoth,
                keep_result.id,
            )
            .unwrap();
        assert_eq!(
            restarted_history
                .snapshot(record_id)
                .unwrap()
                .versions
                .len(),
            source_versions
        );
        assert_eq!(
            restarted_history
                .snapshot(target_record)
                .unwrap()
                .versions
                .len(),
            1
        );
        assert!(
            restarted_journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
    }

    // Shared deterministic merge/recovery scenario used by both supported
    // codecs; each crash boundary is intentionally asserted in sequence.
    #[allow(clippy::too_many_lines)]
    async fn merge_recovers_after_history_crash_and_replays_idempotently(
        name: &str,
        base_bytes: Vec<u8>,
        current_bytes: Vec<u8>,
        other_bytes: Vec<u8>,
        qdoc: bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let base_hash = store.write_content(&base_bytes).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                name,
                Some(&base_hash),
                base_bytes.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let history_path = dir.path().join("history.json");
        let records_path = dir.path().join("records");
        let journal_path = dir.path().join("transactions");
        let namespace_path = dir.path().join("namespace.automerge");
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&records_path).unwrap());
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        let base_version = history.snapshot(record_id).unwrap().versions[0].id;
        fs.commit_content(ino, &current_bytes).unwrap();
        let current = history
            .snapshot(record_id)
            .unwrap()
            .versions
            .last()
            .unwrap()
            .clone();
        let other_hash = store.write_content(&other_bytes).await.unwrap().unwrap();
        let other = collab_types::FileVersionV1 {
            id: collab_types::FileVersionId::new(),
            record_id,
            parents: vec![base_version],
            actor,
            timestamp_ms: current.timestamp_ms.saturating_add(1),
            content_hash: Some(collab_types::ContentHash(*other_hash.as_bytes())),
            metadata_hash: collab_types::ContentHash([77; 32]),
            provenance: collab_types::FileProvenance::Edit,
        };
        history.append(other.clone()).unwrap();
        let conflict = collab_types::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![current.id, other.id],
            status: collab_types::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(conflict.clone()).unwrap();
        let expected = crate::conflict_merge::merge_exact(
            crate::conflict_merge::ExactMergeInput {
                version_id: current.id,
                bytes: current_bytes.clone(),
            },
            crate::conflict_merge::ExactMergeInput {
                version_id: other.id,
                bytes: other_bytes.clone(),
            },
            (!qdoc).then_some(crate::conflict_merge::ExactMergeBase {
                version_id: base_version,
                bytes: base_bytes.clone(),
            }),
        )
        .unwrap()
        .bytes;
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(
                &metadata,
                std::slice::from_ref(&other),
                std::slice::from_ref(&conflict),
            )
            .unwrap();
        let transaction_id = merge_uuid(
            b"transaction",
            publisher.workspace_id(),
            record_id,
            conflict.id,
            &conflict.versions,
            actor,
        );
        crate::transaction_coordinator::arm_coordinator_fault(
            transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterHistoryAppend,
        );
        assert_eq!(
            fs.resolve_conflict(
                ino,
                conflict.id,
                collab_types::ConflictStrategy::Merge,
                other.id
            ),
            Err(EIO)
        );
        assert_eq!(fs.read_full_blocking(ino).unwrap(), current_bytes);
        assert!(
            history
                .snapshot(record_id)
                .unwrap()
                .conflicts
                .iter()
                .find(|value| value.id == conflict.id)
                .unwrap()
                .resolution
                .is_none()
        );
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);

        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(crate::record_repository::RecordRepository::open(&records_path).unwrap()),
                Uuid::from_u128(0xc011),
                actor,
                signer,
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let restarted = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher)
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path)
            .unwrap();
        assert_eq!(
            restarted.read_full_blocking(ino).unwrap(),
            expected.as_slice()
        );
        let resolution = history
            .snapshot(record_id)
            .unwrap()
            .conflicts
            .into_iter()
            .find(|value| value.id == conflict.id)
            .unwrap()
            .resolution
            .unwrap();
        assert_eq!(resolution.strategy, collab_types::ConflictStrategy::Merge);
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .content_revision_for(ino)
                .unwrap()
                .version_id,
            resolution.result
        );
        assert!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
        restarted
            .resolve_conflict(
                ino,
                conflict.id,
                collab_types::ConflictStrategy::Merge,
                other.id,
            )
            .unwrap();
        assert_eq!(
            restarted.read_full_blocking(ino).unwrap(),
            expected.as_slice()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn text_merge_recovers_after_history_crash_and_replays_idempotently() {
        merge_recovers_after_history_crash_and_replays_idempotently(
            "merge.txt",
            b"base".to_vec(),
            b"base-tail".to_vec(),
            b"head-base".to_vec(),
            false,
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn qdoc_merge_recovers_after_history_crash_and_replays_idempotently() {
        let mut base = qdoc::Document::new();
        let block = base.insert_block(None, "paragraph");
        base.splice_text(&block, 0, 0, "base");
        let base_bytes = base.save();
        let mut current = base.fork();
        current.splice_text(&block, 0, 0, "L");
        let current_bytes = current.save();
        let mut other = base.fork();
        other.splice_text(&block, 4, 0, "R");
        let other_bytes = other.save();
        merge_recovers_after_history_crash_and_replays_idempotently(
            "merge.qdoc",
            base_bytes,
            current_bytes,
            other_bytes,
            true,
        )
        .await;
    }
}
