//! The FUSE operations — and the filtered view they serve.
//!
//! # A denial is presented as an absence
//!
//! The access decision lives in [`crate::acl`]; this module decides how
//! a denial *looks*. It looks like nothing: a path outside the view is
//! `ENOENT` with no `readdir` entry; a path in the view but not writable
//! is `EROFS`; a path in the view and readable is served normally.
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
//! ## Edge cases covered by the capability model
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
    BackingId, BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
    Generation, INodeNo, InitFlags, KernelConfig, LockOwner, OpenAccMode, OpenFlags, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyWrite, ReplyXattr, Request, TimeOrNow, WriteFlags,
};
use libc::{
    EBUSY, EDQUOT, EEXIST, EFBIG, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EROFS,
};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;
use serde::Serialize;

use crate::acl::{Access, AclHandle};
use crate::availability::{AvailabilityRegistry, RecordAvailability, RecordKind};
use crate::clock::unix_now_ms as now_ms;
use crate::eviction::{EvictionMode, EvictionQueue, EvictionRequestId, EvictionRequestState};
use crate::hydration::Hydrator;
use crate::namespace::{InodeQuery, InodeView, Namespace};
use crate::smart::Heuristics;
use crate::smart::history::{AccessHistory, natural_cmp};
use crate::smart::kept_history::{KeptHistory, KeptHistorySource};
use crate::sync::SyncHandle;
use fs_protocol::projection::{
    RECORD_INDEX_PAGE_ENTRIES, RECORD_INDEX_V2_VERSION, RecordIndexEntryV1,
};
use fs_protocol::{AvailabilityPolicy, Residency, TransferPhase};

const TTL: Duration = Duration::from_secs(10);
// The pinned-hydration worker wakes for everything that can change its plan:
// a local publish (its own wake), any namespace change (remote applies
// included), an ACL replacement, a change to what the availability registry
// says it acts on, and the earliest retry deadline. This backstop only bounds
// how long something none of those announce could go unrepaired; it compares
// cheap change tokens and walks the namespace only when one of them moved.
// It used to be the repair path itself, at 5 s.
const PINNED_HYDRATION_BACKSTOP: Duration = Duration::from_secs(300);
// A namespace change wakes the worker after this long, so a burst of remote
// applies is one pass rather than one per change.
const PINNED_CHANGE_SETTLE: Duration = Duration::from_millis(500);
// While a pinned fetch runs, the worker cannot see a remote change that would
// make it stale; the fetch re-checks its candidate this often.
const PINNED_FETCH_RECHECK: Duration = Duration::from_secs(5);
const MAX_PINNED_FETCHES_PER_PASS: usize = 4;
const HYDRATION_PROGRESS_PERSIST_BYTES: u64 = 8 * 1024 * 1024;

/// Each document's serialized form and the namespace heads it was last
/// known current at; shared, so a read slices it instead of copying it.
type DocExports = HashMap<u64, (Vec<automerge::ChangeHash>, Arc<[u8]>)>;

/// The in-flight write stages of a mount, by inode. Holding this map's lock
/// fixes the set of inodes with pending writes (storage eviction and checked
/// restore rely on that); the bytes of a stage sit behind its own lock, so
/// one inode's writes, reads and commits never wait for another's.
pub type WriteStages = HashMap<u64, Arc<Stage>>;

/// Writes the namespace to disk unless the copy there already holds it
/// ([`QuickFS::set_durability_hook`]).
pub type DurabilityHook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

#[derive(Serialize)]
struct VersionMetadataDigest {
    path: Option<String>,
    kind: u8,
    byte_len: u64,
    deleted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct VersionSnapshot {
    record_id: fs_protocol::FileRecordId,
    content_hash: Option<fs_protocol::ContentHash>,
    metadata_hash: fs_protocol::ContentHash,
    byte_len: u64,
    kind: FileType,
}

fn keep_both_uuid(
    domain: &[u8],
    workspace_id: uuid::Uuid,
    source_record: fs_protocol::FileRecordId,
    conflict_id: uuid::Uuid,
    selected_version: fs_protocol::FileVersionId,
    actor: fs_protocol::FileActor,
) -> uuid::Uuid {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.collaborative-fs.keep-both.v2\0");
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
    source_record: fs_protocol::FileRecordId,
    conflict_id: uuid::Uuid,
    versions: &[fs_protocol::FileVersionId],
    actor: fs_protocol::FileActor,
) -> uuid::Uuid {
    let mut versions = versions.to_vec();
    versions.sort_unstable();
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.collaborative-fs.merge.v2\0");
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
    target_record: fs_protocol::FileRecordId,
) -> fs_protocol::FileNameV1 {
    let source_name = source_name.as_ref();
    let simple = target_record.0.simple().to_string();
    let suffix = format!(".keep-both-{}", simple.get(..12).unwrap_or(&simple));
    let max_prefix = fs_protocol::files::MAX_FILE_NAME_BYTES.saturating_sub(suffix.len());
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
        let record = fs_protocol::FileRecordId(uuid::Uuid::from_u128(7));
        let original = [b'f', 0xff, b'o'];
        let name = keep_both_name(original, record);
        assert!(name.as_bytes().starts_with(&original));
        assert!(name.as_bytes().ends_with(b".keep-both-000000000000"));
        assert!(name.as_bytes().len() <= fs_protocol::MAX_FILE_NAME_BYTES);

        let long = keep_both_name(vec![0xfe; fs_protocol::MAX_FILE_NAME_BYTES * 2], record);
        assert_eq!(long.as_bytes().len(), fs_protocol::MAX_FILE_NAME_BYTES);
        assert!(long.as_bytes().ends_with(b".keep-both-000000000000"));
    }
}

struct ReplacementTransactionRuntime {
    journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
    namespace_path: std::path::PathBuf,
}

/// Commit bookkeeping for one in-memory write buffer. `generation` counts
/// mutations of the buffer; a buffer whose latest generation is committed is
/// clean and may be dropped when its last handle closes.
#[derive(Clone, Copy, Debug, Default)]
struct BufferState {
    generation: u64,
    committed: Option<u64>,
    /// Latest generation made durable in the unsaved store by `fsync`.
    staged: Option<u64>,
    /// Committed content the buffer was seeded from (or last committed as).
    base_hash: Option<Hash>,
    /// Errno of the most recent failed commit, cleared by a successful one.
    failure: Option<i32>,
}

impl BufferState {
    const fn is_committed(&self) -> bool {
        matches!(self.committed, Some(generation) if generation == self.generation)
    }

    const fn is_durable(&self) -> bool {
        self.is_committed()
            || matches!(self.staged, Some(generation) if generation == self.generation)
    }
}

/// Kernel passthrough for read-only opens of fully local files: the kernel
/// reads a real backing file directly and the daemon only answers `open`.
///
/// Backing files are materialised once per content hash under `dir` (never
/// on this FUSE mount) and registered per open; the kernel keeps using them
/// until `release`. Access is decided at `open` like on a native file system:
/// reads through a passthrough handle are not re-checked against the ACL.
/// Registering a backing file needs `CAP_SYS_ADMIN` in the initial user
/// namespace; the first refusal disables passthrough for the mount.
struct Passthrough {
    dir: std::path::PathBuf,
    kernel_ready: std::sync::atomic::AtomicBool,
    disabled: std::sync::atomic::AtomicBool,
    backing: Mutex<HashMap<u64, (BackingId, std::fs::File)>>,
}

impl Passthrough {
    fn usable(&self) -> bool {
        self.kernel_ready.load(Ordering::Acquire) && !self.disabled.load(Ordering::Acquire)
    }

    fn hold(&self, handle: u64, id: BackingId, file: std::fs::File) {
        self.backing.lock_recover().insert(handle, (id, file));
    }

    fn release(&self, handle: u64) {
        self.backing.lock_recover().remove(&handle);
    }

    fn disable(&self, error: &io::Error) {
        if !self.disabled.swap(true, Ordering::AcqRel) {
            eprintln!("FUSE passthrough unavailable ({error}); serving reads through the daemon");
        }
    }
}

/// Where a version's content comes from.
pub(crate) enum VersionSource<'a> {
    /// Bytes in memory (a FUSE write, a merge, a restore).
    Bytes(&'a [u8]),
    /// A file that does not change while it is stored (a capture snapshot),
    /// with the manifest a capture already computed for it.
    File {
        path: &'a std::path::Path,
        manifest: Option<Arc<crate::content::Manifest>>,
    },
}

/// Create `dir` owner-only and remove stage files a previous run left.
fn prepare_stage_dir(dir: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry
            .file_name()
            .as_bytes()
            .starts_with(write_stage::STAGE_FILE_PREFIX.as_bytes())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Test-only hook run with an inode's stage locked for its commit.
#[cfg(test)]
type CommitPause = Arc<dyn Fn(u64) + Send + Sync>;

/// How a FUSE `read` is answered, decided by `QuickFS::plan_read` before
/// any reply is sent.
enum ReadPlan {
    /// Refused, or failed, with this errno.
    Error(i32),
    /// From the inode's write stage or a document's export.
    Buffered(BufferedRead),
    /// Bytes already on this device (empty for a file with no content).
    Local(Vec<u8>),
    /// Content a peer must supply, hydrated off the FUSE worker.
    Remote(Hash),
}

/// A read answered without the committed content's read path.
enum BufferedRead {
    /// A range of a document's shared export.
    Shared(Arc<[u8]>, std::ops::Range<usize>),
    /// A read of the inode's write stage.
    Staged(io::Result<StagedRead>),
    /// The document could not be exported.
    Failed,
}

/// What a caller of [`QuickFS::persist_buffer`] needs from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistIntent {
    /// `close`/`flush`: commit the bytes as the file's next version.
    Close,
    /// `fsync`: make the bytes durable; the version follows at close.
    Sync,
}

/// Commit failures that retrying the same bytes into the same record cannot
/// fix. A buffer that ends in one of these is kept as a separate copy when
/// its last handle closes instead of being held for a retry.
const fn is_permanent_commit_failure(errno: i32) -> bool {
    matches!(errno, EDQUOT | EFBIG | EINVAL)
}

fn commit_errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(EIO)
}

/// The audit's ceiling for a memory-backed stage. Sparse disk-backed stages
/// do not allocate their logical size and are not subject to this limit.
/// Memory stages reserve their eventual contiguous size before accepting a
/// write or truncate, including deferred zero filling at commit.
const MAX_STAGED_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The audit's aggregate ceiling for memory stages of one mount. Reservations
/// are held until the backing drops, including while a removed stage is still
/// in use by another worker; shrinking does not free a Vec's capacity.
const MAX_STAGED_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Refuses a staged size the daemon will not hold in memory.
fn check_staged_size(len: u64) -> Result<(), i32> {
    if len > MAX_STAGED_FILE_BYTES {
        Err(EFBIG)
    } else {
        Ok(())
    }
}

/// Refuses growth that would take every staged buffer past the total.
fn check_staged_growth(staged: u64, growth: u64) -> Result<(), i32> {
    if staged.saturating_add(growth) > MAX_STAGED_TOTAL_BYTES {
        Err(ENOSPC)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod staging_limit_tests {
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};

    async fn mount() -> (QuickFS, Endpoint, [u64; 3]) {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("staging-limits").unwrap()));
        let inos = ["a.bin", "b.bin", "c.bin"].map(|name| {
            namespace
                .lock()
                .unwrap()
                .create_file(ROOT_INO, name, None, 0, 1)
                .unwrap()
        });
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        (
            QuickFS::new(namespace, hydrator, heuristics),
            endpoint,
            inos,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn memory_writes_and_truncates_enforce_limits_without_allocating_holes() {
        let (fs, endpoint, [a, b, c]) = mount().await;
        let huge = MAX_STAGED_FILE_BYTES.saturating_mul(1024);
        assert_eq!(fs.write_buffer(a, huge, b"x"), Err(EFBIG));
        assert_eq!(fs.staged_len(a), Some(0));
        assert_eq!(fs.truncate_inode(a, huge), Err(EFBIG));
        // Unopened truncation uses a temporary stage, which must also be bounded.
        assert_eq!(fs.truncate_inode(b, huge), Err(EFBIG));
        assert_eq!(fs.staged_len(b), None);
        for ino in [a, b] {
            fs.write_buffer(ino, 0, b"x").unwrap();
            fs.truncate_inode(ino, MAX_STAGED_FILE_BYTES).unwrap();
        }
        fs.write_buffer(c, 0, b"").unwrap();
        assert_eq!(fs.write_buffer(c, 0, b"x"), Err(ENOSPC));
        assert_eq!(fs.truncate_inode(c, 1), Err(ENOSPC));
        assert_eq!(fs.staged_len(c), Some(0));
        assert_eq!(
            fs.truncate_inode(a, MAX_STAGED_FILE_BYTES.saturating_add(1)),
            Err(EFBIG)
        );
        assert_eq!(fs.staged_len(a), Some(MAX_STAGED_FILE_BYTES));
        // A removed stage held by another worker still owns its allocation.
        let retained = fs.stage(a).unwrap();
        fs.drop_buffer(a);
        assert_eq!(fs.truncate_inode(c, 1), Err(ENOSPC));
        drop(retained);
        fs.write_buffer(c, 0, b"x").unwrap();
        assert_eq!(fs.staged_bytes(c).unwrap(), b"x");
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sparse_stages_are_not_subject_to_memory_limits_but_fallback_is() {
        let (fs, endpoint, [a, b, c]) = mount().await;
        let dir = tempfile::tempdir().unwrap();
        let fs = fs.with_write_stage_dir(dir.path().join("stages"));
        // Above both audit ceilings, without allocating or hydrating the hole.
        let offset = MAX_STAGED_TOTAL_BYTES.saturating_add(1);
        fs.write_buffer(a, offset, b"x").unwrap();
        assert_eq!(fs.staged_len(a), Some(offset.saturating_add(1)));
        assert_eq!(
            fs.read_stage_blocking(&fs.stage(a).unwrap(), offset, 1)
                .unwrap(),
            b"x"
        );
        fs.write_buffer(b, 0, b"b").unwrap();
        fs.truncate_inode(b, offset).unwrap();
        assert_eq!(fs.staged_len(b), Some(offset));
        std::fs::remove_dir_all(fs.stage_dir.as_ref().unwrap()).unwrap();
        fs.buffers().insert(c, fs.empty_stage(false).shared());
        assert_eq!(fs.write_buffer(c, offset, b"x"), Err(EFBIG));
        assert_eq!(fs.truncate_inode(c, offset), Err(EFBIG));
        assert_eq!(fs.staged_len(c), Some(0));
        endpoint.close().await;
    }
}

/// The record ids from `ino` up to the root, or `None` when one of them has
/// no record id.
fn record_ancestry(ns: &mut Namespace, ino: u64) -> Option<Vec<fs_protocol::FileRecordId>> {
    let mut ancestry = Vec::new();
    let mut cursor = Some(ino);
    while let Some(current) = cursor {
        ancestry.push(ns.record_id_for(current)?);
        cursor = ns.parent_ino(current);
    }
    Some(ancestry)
}

/// Pinned when the record or a folder above it is pinned, else automatic.
fn inherited_policy(
    registry: &AvailabilityRegistry,
    ancestry: &[fs_protocol::FileRecordId],
) -> AvailabilityPolicy {
    if ancestry.iter().any(|id| {
        registry
            .get(*id)
            .is_some_and(|record| record.policy == AvailabilityPolicy::Pinned)
    }) {
        AvailabilityPolicy::Pinned
    } else {
        AvailabilityPolicy::Automatic
    }
}

/// Name of the sibling that preserves bytes which could not be committed to
/// their own record, derived from the bytes so a retry finds the same copy.
fn unsaved_copy_name(name: &[u8], bytes: &[u8]) -> Vec<u8> {
    unsaved_copy_name_for(name, &blake3::hash(bytes))
}

/// [`unsaved_copy_name`] of bytes whose blake3 hash is `digest`.
fn unsaved_copy_name_for(name: &[u8], digest: &blake3::Hash) -> Vec<u8> {
    let hex = digest.to_hex();
    let suffix = [
        b".unsaved-".as_slice(),
        hex.as_bytes().get(..16).unwrap_or_default(),
    ]
    .concat();
    let prefix_len = name
        .len()
        .min(fs_protocol::MAX_FILE_NAME_BYTES.saturating_sub(suffix.len()));
    let mut out = name.get(..prefix_len).unwrap_or(name).to_vec();
    out.extend_from_slice(&suffix);
    out
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
    /// How many times the worker has come back to wait, each pass or
    /// skipped tick done, for a test to count them.
    #[cfg(test)]
    turns: Arc<std::sync::atomic::AtomicU64>,
}

impl Drop for PinnedHydrationTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What a pinned-hydration pass is computed from, as cheap change tokens.
#[derive(PartialEq, Eq)]
struct PinnedPassInputs {
    namespace: crate::namespace::NamespaceChangeToken,
    acl_revision: u64,
    availability: u64,
}

impl PinnedPassInputs {
    fn capture(
        namespace: &Arc<Mutex<Namespace>>,
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
    ) -> Self {
        let namespace = namespace.lock_recover().change_token();
        let availability = registry.lock_recover().hydration_generation();
        Self {
            namespace,
            acl_revision: enforcement.map_or(0, |(acl, _)| acl.revision()),
            availability,
        }
    }
}

/// What a pinned-hydration pass leaves for the worker's scheduler.
#[derive(Default)]
struct PinnedPassOutcome {
    /// More pinned files were due than one pass fetches: run again at once.
    more_due: bool,
    /// The earliest backoff deadline of a pinned file that was not yet due.
    next_retry_at_ms: Option<u64>,
}

/// One pinned file's local content, probed outside every lock.
#[derive(Clone, Copy)]
enum PinnedProbe {
    Local,
    Missing { resident_bytes: u64 },
}

#[derive(Clone)]
struct PinnedCandidate {
    record_id: fs_protocol::FileRecordId,
    parent: Option<fs_protocol::FileRecordId>,
    kind: RecordKind,
    version: Option<fs_protocol::FileVersionId>,
    hash: Option<Hash>,
    size: u64,
    path: String,
}

#[derive(Clone)]
struct RecordIndexCursor {
    revision: String,
    /// Private source state captured when the cursor was issued.  It is never
    /// serialized: the public revision is the ACL-filtered projection below.
    source_revision: String,
    acl_revision: u64,
    offset: usize,
}

/// Retained, authority-specific catalogue.  Building this projection is a
/// mutation/mount-time operation; serving a v3 page is only a bounded slice
/// of it.  In particular, no v3 request walks denied namespace rows.
#[derive(Clone)]
struct AclVisibleRecordIndex {
    source_revision: String,
    acl_revision: u64,
    revision: String,
    records: Vec<RecordIndexEntryV1>,
    by_id: HashMap<fs_protocol::FileRecordId, RecordIndexEntryV1>,
    /// Rows this authority may not see, with the path they were judged at.
    /// With `by_id` it lets the next refresh under the same ACL revision
    /// re-evaluate only rows whose path changed.
    hidden: HashMap<fs_protocol::FileRecordId, String>,
    /// ACL evaluations made building this view.
    #[cfg(test)]
    acl_evaluations: usize,
}

impl AclVisibleRecordIndex {
    /// The visibility this view already decided for `record_id` at `path`,
    /// if it judged that exact row. Visibility is a function of the ACL
    /// revision and the path alone, so the verdict holds for the same pair.
    fn verdict(&self, record_id: fs_protocol::FileRecordId, path: &str) -> Option<bool> {
        if self
            .by_id
            .get(&record_id)
            .is_some_and(|entry| entry.path == path)
        {
            return Some(true);
        }
        self.hidden
            .get(&record_id)
            .is_some_and(|hidden| hidden == path)
            .then_some(false)
    }
}

#[path = "checked_restore.rs"]
mod checked_restore;
#[path = "history_jobs.rs"]
mod history_jobs;
#[path = "history_read.rs"]
mod history_read;
pub mod native_ops;
mod unavailable;
mod write_stage;
use tilo_lock::MutexRecover as _;
use write_stage::{MissingRanges, SharedStage, StagedRead};
pub use write_stage::{Stage, WriteStage};

/// The codec key for `name`'s extension if it's a recognized document.
pub fn document_codec_for(name: &str) -> Option<String> {
    let ext = name.rsplit('.').next().filter(|e| *e != name)?;
    tilo_doc_codecs::provider_for_ext(ext).map(|_| ext.to_lowercase())
}

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
    /// Which files this mount opened when: what prediction learns from.
    /// Local to this device and this workspace and bounded; its
    /// opened-together pairs survive restarts through `kept_history`.
    access_history: Arc<Mutex<AccessHistory>>,
    /// Keeps the opened-together pairs in the workspace's cache directory
    /// (`None`: memory only).
    kept_history: Option<Arc<KeptHistory>>,
    /// Binary content (images, binary package parts) that documents projected as files refer to.
    document_content: tilo_content::ContentStore,
    /// Broadcasts local CRDT deltas to peers (None when running standalone).
    sync: Option<Arc<SyncHandle>>,
    /// Per-inode in-flight write stages (content-addressed on flush/release).
    write_buffers: Arc<Mutex<WriteStages>>,
    /// Where stages keep written bytes; `None` keeps them in memory.
    stage_dir: Option<std::path::PathBuf>,
    memory_stage_budget: Arc<write_stage::MemoryBudget>,
    /// Commit bookkeeping for `write_buffers`. Lock order: `write_buffers`,
    /// then one inode's stage, then `buffer_states`; the namespace lock is
    /// taken after a stage lock, never before.
    buffer_states: Mutex<HashMap<u64, BufferState>>,
    /// Serialises persistence of one inode's buffer so concurrent FUSE workers
    /// never commit the same buffer twice or interleave stage and commit.
    commit_locks: Mutex<HashMap<u64, Arc<Mutex<()>>>>,
    /// Durable staging for `fsync`ed and not-yet-committed bytes. `None`
    /// (tests, memory stores) makes `fsync` commit a version directly.
    unsaved: Option<Arc<crate::unsaved::UnsavedStore>>,
    /// The content each inode's kernel page cache was filled from, so an
    /// `open` of unchanged content keeps the cache (`FOPEN_KEEP_CACHE`) and
    /// an open after any change drops it.
    kernel_cached: Mutex<HashMap<u64, Option<Hash>>>,
    passthrough: Option<Passthrough>,
    open_file_handles: Mutex<HashMap<u64, OpenFileHandleState>>,
    next_file_handle: AtomicU64,
    /// Cached serialized form of each document inode, keyed by the doc's heads,
    /// so a multi-chunk read exports once (and getattr's size stays current).
    doc_exports: Mutex<DocExports>,
    record_index_cursors: Mutex<HashMap<String, RecordIndexCursor>>,
    record_index_initial_cursors: Mutex<HashMap<(String, String, u64), String>>,
    acl_visible_record_index: Arc<Mutex<Option<Arc<AclVisibleRecordIndex>>>>,
    /// Rich facts extracted only while current-version bytes are already local.
    /// Replicated peers consume these facts without hydrating the payload.
    rich_metadata: Mutex<
        HashMap<
            fs_protocol::FileRecordId,
            (fs_protocol::ContentHash, fs_protocol::FilePropertiesV1),
        >,
    >,
    /// The workspace ACL and the identity it is evaluated as — the authoritative
    /// access-control check. `None` keeps Unix-veneer-only behaviour;
    /// `Some` checks every open/read/write against the ACL as that identity.
    /// One field so an ACL can never be attached without the identity it
    /// answers for. A *handle*, not the ACL itself, so the policy can be
    /// replaced while the mount is up — see [`crate::CapabilityFile`].
    enforcement: Option<(AclHandle, crate::acl::IdentityKey)>,
    /// Host-side thumbnail generation + cache. `Some` when the mount has a
    /// persistent cache directory (serves previews via `user.tilo.thumb` and
    /// gossips the hash); `None` leaves the mount thumbnail-less.
    thumbs: Option<crate::thumbs::ThumbProvider>,
    /// Durable local pin/replica/transfer registry. The FUSE path only reads
    /// its in-memory state; persistence happens at mutation points.
    availability: Option<Arc<Mutex<AvailabilityRegistry>>>,
    /// Files reads found whole, written to `availability` together.
    stored_writes: Arc<unavailable::StoredWrites>,
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
        fs_protocol::FileActor,
    )>,
    /// Signed exact-record projection used by partial-share network sessions.
    record_publisher: Option<Arc<crate::record_publisher::RecordPublisher>>,
    /// Which agent the desktop session says caused the writes to some paths
    /// ([`crate::write_attribution`]).
    write_attributions: Option<Arc<crate::write_attribution::WriteAttributions>>,
    replacement_transactions: Option<ReplacementTransactionRuntime>,
    /// Downloads behind restores and conflict resolutions whose content is
    /// not local yet, so no FUSE worker waits for them.
    history_jobs: history_jobs::HistoryJobs,
    checked_history_workspace: Option<uuid::Uuid>,
    mount_lifetime: uuid::Uuid,
    /// Makes the namespace on disk hold the current placements: run after a
    /// record group is durable (native plane), and by `fsync`/`fsyncdir`.
    durability_hook: std::sync::OnceLock<DurabilityHook>,
    /// Namespace deltas of the changeset being published, broadcast once it
    /// is durable (`None` outside a changeset).
    deferred_deltas: Mutex<Option<Vec<Vec<u8>>>>,
    /// Deltas of changesets whose durable write failed; broadcast with the
    /// next changeset that becomes durable.
    held_deltas: Mutex<Vec<Vec<u8>>>,
    /// Explicit one-time owner migration permission. Joining peers disable
    /// this so unsigned legacy namespace bytes are never automatically
    /// blessed into their certified actor's signed history.
    allow_legacy_record_migration: bool,
    share_control_path: Option<std::path::PathBuf>,
    /// Tells inotify of the changes the kernel did not make
    /// ([`crate::change_signal`]); `None` leaves watchers to poll.
    change_signal: Option<Arc<crate::change_signal::ChangeSignal>>,
    /// Test-only: make content commits of one inode fail with an errno.
    #[cfg(test)]
    commit_fault: Mutex<Option<(u64, i32)>>,
    /// Test-only: run while an inode's stage is locked for its commit.
    #[cfg(test)]
    commit_pause: Mutex<Option<CommitPause>>,
    /// Test-only: how many times [`Self::ns`] has taken the namespace lock.
    #[cfg(test)]
    namespace_locks: std::sync::atomic::AtomicUsize,
}

impl QuickFS {
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            heuristics,
            access_history: Arc::new(Mutex::new(AccessHistory::new())),
            kept_history: None,
            document_content: tilo_content::ContentStore::memory(),
            sync: None,
            write_buffers: Arc::new(Mutex::new(HashMap::new())),
            stage_dir: None,
            memory_stage_budget: Arc::new(write_stage::MemoryBudget::default()),
            buffer_states: Mutex::new(HashMap::new()),
            commit_locks: Mutex::new(HashMap::new()),
            unsaved: None,
            kernel_cached: Mutex::new(HashMap::new()),
            passthrough: None,
            open_file_handles: Mutex::new(HashMap::new()),
            next_file_handle: AtomicU64::new(1),
            doc_exports: Mutex::new(HashMap::new()),
            record_index_cursors: Mutex::new(HashMap::new()),
            record_index_initial_cursors: Mutex::new(HashMap::new()),
            acl_visible_record_index: Arc::new(Mutex::new(None)),
            rich_metadata: Mutex::new(HashMap::new()),
            enforcement: None,
            thumbs: None,
            availability: None,
            stored_writes: Arc::default(),
            pinned_hydration: None,
            eviction_queue: None,
            share_authority: None,
            history: None,
            record_publisher: None,
            write_attributions: None,
            replacement_transactions: None,
            history_jobs: history_jobs::HistoryJobs::default(),
            checked_history_workspace: None,
            mount_lifetime: uuid::Uuid::new_v4(),
            durability_hook: std::sync::OnceLock::new(),
            deferred_deltas: Mutex::new(None),
            held_deltas: Mutex::new(Vec::new()),
            allow_legacy_record_migration: true,
            share_control_path: None,
            change_signal: None,
            #[cfg(test)]
            commit_fault: Mutex::new(None),
            #[cfg(test)]
            commit_pause: Mutex::new(None),
            #[cfg(test)]
            namespace_locks: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Keep the opened-together history for `workspace` under `cache_root`
    /// across restarts, and restore what was kept. See
    /// [`crate::smart::kept_history`].
    #[must_use]
    pub fn with_kept_prefetch_history(
        self,
        cache_root: &std::path::Path,
        workspace: uuid::Uuid,
    ) -> Self {
        self.with_kept_prefetch_history_after(
            cache_root,
            workspace,
            crate::smart::kept_history::FLUSH_DELAY,
        )
    }

    /// [`Self::with_kept_prefetch_history`], writing `flush_delay` after a
    /// change.
    #[must_use]
    pub fn with_kept_prefetch_history_after(
        mut self,
        cache_root: &std::path::Path,
        workspace: uuid::Uuid,
        flush_delay: Duration,
    ) -> Self {
        let runtime = self.hydrator.runtime_handle.clone();
        self.kept_history = Some(KeptHistory::open(
            KeptHistorySource {
                cache_root,
                workspace,
                history: Arc::clone(&self.access_history),
                namespace: Arc::clone(&self.namespace),
                heuristics: Arc::clone(&self.heuristics),
            },
            &runtime,
            flush_delay,
        ));
        self
    }

    #[must_use]
    pub fn without_legacy_record_migration(mut self) -> Self {
        self.allow_legacy_record_migration = false;
        self
    }

    /// Shared dirty-inode view for the storage authority.
    pub fn write_buffers_handle(&self) -> Arc<Mutex<WriteStages>> {
        Arc::clone(&self.write_buffers)
    }

    /// Keep written bytes of opaque files in sparse files under `dir`
    /// instead of in memory, so a large write never has to fit in RAM.
    /// Stage files a previous run left are removed: bytes that had to
    /// survive it were staged in the unsaved store. When the directory
    /// cannot be prepared, writes keep staging in memory.
    #[must_use]
    pub fn with_write_stage_dir(mut self, dir: std::path::PathBuf) -> Self {
        match prepare_stage_dir(&dir) {
            Ok(()) => self.stage_dir = Some(dir),
            Err(error) => eprintln!(
                "write staging directory unavailable at {} ({error}); staging writes in memory",
                dir.display()
            ),
        }
        self
    }

    fn ns(&self) -> std::sync::MutexGuard<'_, Namespace> {
        #[cfg(test)]
        self.namespace_locks.fetch_add(1, Ordering::Relaxed);
        self.namespace.lock_recover()
    }

    /// [`Namespace::inode_view`] under one namespace lock, with the path
    /// built only when this mount has an ACL to check it against.
    fn inode_view(&self, ino: u64, unlinked: bool) -> Option<InodeView> {
        let query = InodeQuery {
            unlinked,
            path: self.enforcement.is_some(),
        };
        self.ns().inode_view(ino, query)
    }

    /// The state of kernel handle `fh`, when it was opened on `ino`.
    fn open_handle(&self, ino: u64, fh: u64) -> Option<OpenFileHandleState> {
        self.open_file_handles
            .lock_recover()
            .get(&fh)
            .filter(|handle| handle.ino == ino)
            .cloned()
    }

    fn buffers(&self) -> std::sync::MutexGuard<'_, WriteStages> {
        self.write_buffers.lock_recover()
    }

    fn buffer_states(&self) -> std::sync::MutexGuard<'_, HashMap<u64, BufferState>> {
        self.buffer_states.lock_recover()
    }

    fn commit_lock_for(&self, ino: u64) -> Arc<Mutex<()>> {
        Arc::clone(self.commit_locks.lock_recover().entry(ino).or_default())
    }

    /// Stage unsaved bytes durably in `store` (see [`crate::unsaved`]):
    /// bytes whose commit failed are persisted there so an unmount or crash
    /// cannot lose them, and `fsync` persists there instead of minting a
    /// version per call.
    #[must_use]
    pub fn with_unsaved_store(mut self, store: crate::unsaved::UnsavedStore) -> Self {
        self.unsaved = Some(Arc::new(store));
        self
    }

    fn unsaved_key(&self, ino: u64) -> (String, Option<fs_protocol::FileRecordId>) {
        let record_id = self.ns().record_id_for(ino);
        let key = record_id.map_or_else(|| format!("ino-{ino}"), |id| id.0.to_string());
        (key, record_id)
    }

    /// Stage the complete bytes of `stage` (an `fsync`, or a failed commit)
    /// in `store`; a file-backed stage is copied file to file.
    fn stage_unsaved(
        &self,
        store: &crate::unsaved::UnsavedStore,
        ino: u64,
        stage: &mut WriteStage,
        base_hash: Option<Hash>,
        error: Option<String>,
    ) -> io::Result<()> {
        let (key, record_id) = self.unsaved_key(ino);
        let path = self.ns().path_for(ino);
        match stage.source()? {
            VersionSource::Bytes(data) => {
                let entry = crate::unsaved::UnsavedEntry::describe(
                    key,
                    record_id,
                    path,
                    base_hash,
                    data,
                    now_ms(),
                    error,
                );
                store.stage(&entry, data)
            }
            VersionSource::File { path: file, .. } => {
                let entry = crate::unsaved::UnsavedEntry::describe_file(
                    key,
                    record_id,
                    path,
                    base_hash,
                    file,
                    now_ms(),
                    error,
                )?;
                store.stage_file(&entry, file)
            }
        }
    }

    fn clear_unsaved(&self, ino: u64) {
        if let Some(store) = &self.unsaved {
            let (key, _) = self.unsaved_key(ino);
            if store.contains(&key)
                && let Err(error) = store.remove(&key)
            {
                eprintln!("could not clear staged bytes for inode {ino}: {error}");
            }
        }
    }

    fn drop_buffer(&self, ino: u64) {
        self.buffers().remove(&ino);
        self.buffer_states().remove(&ino);
        self.commit_locks.lock_recover().remove(&ino);
    }

    /// Record a mutation of `ino`'s buffer. `seeded` marks a buffer that was
    /// just created from the committed content `base`.
    fn note_buffer_mutation(&self, ino: u64, seeded: bool, base: Option<Hash>) {
        let mut states = self.buffer_states();
        let state = states.entry(ino).or_default();
        if seeded {
            state.base_hash = base;
        }
        state.generation = state.generation.wrapping_add(1);
        drop(states);
    }

    /// Persist the stage of `ino`, if it has unpersisted bytes.
    ///
    /// [`PersistIntent::Close`] commits the bytes as a version. On failure
    /// the errno is returned, the stage stays (reads keep seeing it) and the
    /// bytes are staged in the unsaved store; the next flush, fsync or close
    /// retries.
    ///
    /// [`PersistIntent::Sync`] only guarantees durability: with an unsaved
    /// store the bytes are staged there, so a database that syncs every
    /// transaction produces one version per close instead of one per sync.
    /// Without a store it commits like `Close`.
    ///
    /// Ranges nobody wrote are read from the committed content first, outside
    /// the stage lock. The commit then runs under this inode's stage lock
    /// only, so the bytes and their generation describe one state while other
    /// inodes' writes and reads proceed.
    fn persist_buffer(&self, ino: u64, intent: PersistIntent) -> Result<(), i32> {
        let lock = self.commit_lock_for(ino);
        let _serialised = lock.lock_recover();
        let Some(staged) = self.stage(ino) else {
            return Ok(());
        };
        let settled = |state: &BufferState| match intent {
            PersistIntent::Close => state.is_committed(),
            PersistIntent::Sync => state.is_durable(),
        };
        if settled(&self.buffer_states().get(&ino).copied().unwrap_or_default()) {
            return Ok(());
        }
        // An unlinked inode's bytes belong to no file any more.
        if self.ns().getattr(ino).is_none() {
            return Ok(());
        }
        if let Err(error) = self.fill_stage(&staged) {
            self.buffer_states().entry(ino).or_default().failure = Some(EIO);
            eprintln!(
                "inode {ino}: the unwritten parts of its committed content could not be read ({error}); the written bytes stay in its stage and are not staged durably"
            );
            return Err(EIO);
        }
        let mut guard = staged.lock();
        let state = self.buffer_states().get(&ino).copied().unwrap_or_default();
        if settled(&state) {
            return Ok(());
        }
        #[cfg(test)]
        self.pause_commit(ino);
        if intent == PersistIntent::Sync
            && let Some(store) = &self.unsaved
        {
            return match self.stage_unsaved(store, ino, &mut guard, state.base_hash, None) {
                Ok(()) => {
                    if let Some(current) = self.buffer_states().get_mut(&ino) {
                        current.staged = Some(state.generation);
                    }
                    Ok(())
                }
                Err(error) => {
                    eprintln!("fsync of inode {ino} could not stage its bytes: {error}");
                    Err(commit_errno(&error))
                }
            };
        }
        match self.commit_buffer(ino, &mut guard) {
            Ok(()) => {
                let committed_hash = self.ns().hash_for(ino);
                if let Some(current) = self.buffer_states().get_mut(&ino) {
                    current.committed = Some(state.generation);
                    current.staged = None;
                    current.failure = None;
                    current.base_hash = committed_hash;
                }
                self.clear_unsaved(ino);
                Ok(())
            }
            Err(errno) => {
                self.buffer_states().entry(ino).or_default().failure = Some(errno);
                let detail = io::Error::from_raw_os_error(errno).to_string();
                match &self.unsaved {
                    Some(store) => match self.stage_unsaved(
                        store,
                        ino,
                        &mut guard,
                        state.base_hash,
                        Some(detail.clone()),
                    ) {
                        Ok(()) => eprintln!(
                            "commit of inode {ino} failed ({detail}); bytes kept in memory and staged in {}",
                            store.dir().display()
                        ),
                        Err(error) => eprintln!(
                            "commit of inode {ino} failed ({detail}); bytes kept in memory only, staging failed: {error}"
                        ),
                    },
                    None => {
                        eprintln!("commit of inode {ino} failed ({detail}); bytes kept in memory");
                    }
                }
                Err(errno)
            }
        }
    }

    /// Test-only: give `ino` a stage holding exactly `bytes`.
    #[cfg(test)]
    fn install_stage(&self, ino: u64, bytes: Vec<u8>) {
        let stage =
            WriteStage::complete(&bytes, None, Arc::clone(&self.memory_stage_budget)).unwrap();
        drop(bytes);
        self.buffers().insert(ino, stage.shared());
    }

    /// Test-only: every byte `ino`'s stage reads as, if it has one.
    #[cfg(test)]
    fn staged_bytes(&self, ino: u64) -> Option<Vec<u8>> {
        let stage = self.stage(ino)?;
        self.read_stage_blocking(&stage, 0, u64::MAX).ok()
    }

    #[cfg(test)]
    fn pause_commit(&self, ino: u64) {
        let pause = self.commit_pause.lock_recover().clone();
        if let Some(pause) = pause {
            pause(ino);
        }
    }

    fn stage(&self, ino: u64) -> Option<SharedStage> {
        self.buffers().get(&ino).cloned()
    }

    /// The length of `ino`'s stage: what `stat` reports while it is open.
    fn staged_len(&self, ino: u64) -> Option<u64> {
        Some(self.stage(ino)?.len())
    }

    /// A stage for `ino` over its committed content, reading none of it. A
    /// document is staged whole from its export (local, never a peer).
    fn new_stage(&self, ino: u64) -> Result<(WriteStage, Option<Hash>), i32> {
        if self.is_document(ino) {
            let base = self.ns().hash_for(ino);
            let bytes = self.document_export(ino).ok_or(EIO)?;
            let stage = WriteStage::complete(&bytes, base, Arc::clone(&self.memory_stage_budget))
                .map_err(|error| commit_errno(&error))?;
            return Ok((stage, base));
        }
        let (base, base_len) = {
            let ns = self.ns();
            let base = ns.hash_for(ino);
            let len = ns
                .getattr_including_unlinked(ino)
                .map_or(0, |attr| attr.size);
            drop(ns);
            (base, len)
        };
        let stage = WriteStage::over(
            base,
            base_len,
            self.stage_dir.as_deref(),
            Arc::clone(&self.memory_stage_budget),
        )
        .map_err(|error| {
            eprintln!("inode {ino}: could not open a write stage: {error}");
            EIO
        })?;
        Ok((stage, base))
    }

    /// An empty stage for a file `create` just committed empty.
    fn empty_stage(&self, document: bool) -> WriteStage {
        let memory = || WriteStage::empty(Arc::clone(&self.memory_stage_budget));
        if document {
            return memory();
        }
        WriteStage::over(
            None,
            0,
            self.stage_dir.as_deref(),
            Arc::clone(&self.memory_stage_budget),
        )
        .unwrap_or_else(|error| {
            eprintln!("could not open a file-backed write stage ({error}); staging in memory");
            memory()
        })
    }

    /// `ino`'s stage, opened if it has none. Returns whether this call
    /// opened it and the committed content it was opened over.
    fn stage_for_write(&self, ino: u64) -> Result<(SharedStage, bool, Option<Hash>), i32> {
        if let Some(stage) = self.stage(ino) {
            return Ok((stage, false, None));
        }
        let (fresh, base) = self.new_stage(ino)?;
        // A racing write may have opened one meanwhile; it wins.
        let mut stages = self.buffers();
        Ok(match stages.entry(ino) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                (Arc::clone(entry.get()), false, None)
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                (Arc::clone(entry.insert(fresh.shared())), true, base)
            }
        })
    }

    /// Read every range of `stage` nobody wrote from its committed content,
    /// so it can be committed. Bounded pieces, each fetched without the stage
    /// lock; a failure leaves the stage as it was and is the caller's error.
    fn fill_stage(&self, stage: &SharedStage) -> io::Result<()> {
        const PIECE: u64 = 8 * 1024 * 1024;
        let (base, gaps) = {
            let guard = stage.lock();
            (guard.base(), guard.gaps())
        };
        if gaps.is_empty() {
            return Ok(());
        }
        let base = base
            .ok_or_else(|| io::Error::other("staged file lacks bytes but has no base content"))?;
        for gap in gaps {
            let mut at = gap.start;
            while at < gap.end {
                let len = gap.end.saturating_sub(at).min(PIECE);
                let bytes = self.read_base_blocking(base, at, len)?;
                stage.lock().fill(at, &bytes)?;
                at = at.saturating_add(len);
            }
        }
        Ok(())
    }

    /// Exactly `len` bytes of committed content `base` from `offset`: local
    /// bytes directly, the rest from a peer.
    fn read_base_blocking(&self, base: Hash, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let hydrator = &self.hydrator;
        let bytes = hydrator
            .block_on(async {
                match hydrator.read_local(base, offset, len).await {
                    Some(bytes) => Ok(bytes),
                    None => hydrator.read(base, offset, len).await,
                }
            })
            .map_err(|error| io::Error::other(error.to_string()))?;
        if bytes.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "committed content is shorter than the file it backs",
            ));
        }
        Ok(bytes)
    }

    /// Read `ino`'s stage: `None` when it has none. Ranges nobody wrote come
    /// from the committed content: local bytes inline, anything that needs a
    /// peer is left in [`StagedRead::Missing`] for the caller to hydrate
    /// without holding the stage.
    fn read_staged(&self, ino: u64, offset: u64, size: u64) -> Option<io::Result<StagedRead>> {
        let stage = self.stage(ino)?;
        Some(self.read_stage(&stage, offset, size))
    }

    fn read_stage(&self, stage: &SharedStage, offset: u64, size: u64) -> io::Result<StagedRead> {
        let read = stage.lock().read(offset, size)?;
        let StagedRead::Missing(missing) = read else {
            return Ok(read);
        };
        let hydrator = &self.hydrator;
        Ok(match hydrator.block_on(missing.finish_local(hydrator)) {
            Ok(bytes) => StagedRead::Ready(bytes),
            Err(missing) => StagedRead::Missing(missing),
        })
    }

    /// [`Self::read_stage`], waiting for ranges a peer has to supply.
    fn read_stage_blocking(
        &self,
        stage: &SharedStage,
        offset: u64,
        size: u64,
    ) -> io::Result<Vec<u8>> {
        match self.read_stage(stage, offset, size)? {
            StagedRead::Ready(bytes) => Ok(bytes),
            StagedRead::Missing(missing) => Self::finish_blocking(&self.hydrator, missing),
        }
    }

    fn finish_blocking(hydrator: &Arc<Hydrator>, missing: MissingRanges) -> io::Result<Vec<u8>> {
        hydrator.block_on(missing.finish(hydrator))
    }

    /// `truncate` of a file nobody has open: commit its first `new_size`
    /// bytes. Truncating to zero reads nothing; otherwise only the kept
    /// prefix of the committed content is read.
    fn commit_resized(&self, ino: u64, new_size: u64) -> io::Result<()> {
        if new_size == 0 {
            return self.commit_content(ino, &[]);
        }
        let (stage, _) = self.new_stage(ino).map_err(io::Error::from_raw_os_error)?;
        let stage = stage.shared();
        stage.lock().set_len(new_size)?;
        self.fill_stage(&stage)?;
        let mut guard = stage.lock();
        let committed = guard
            .source()
            .and_then(|source| self.commit_version_at(ino, source, now_ms(), None));
        drop(guard);
        committed
    }

    /// Keep the bytes of `stage` as a new sibling of `ino` because they
    /// cannot be committed to `ino`'s own record. Returns the copy's
    /// workspace path.
    fn preserve_as_sibling(&self, ino: u64, stage: &mut WriteStage) -> Result<String, i32> {
        let (parent, name) = {
            let mut ns = self.ns();
            let name = ns.file_name_for(ino).ok_or(ENOENT)?;
            (ns.parent_ino(ino).ok_or(ENOENT)?, name)
        };
        let digest = stage.digest().map_err(|_| EIO)?;
        let copy_name = unsaved_copy_name_for(name.as_bytes(), &digest);
        let source = stage.source().map_err(|_| EIO)?;
        self.create_preserved_copy(parent, &copy_name, source)
    }

    fn create_preserved_copy(
        &self,
        parent: u64,
        copy_name: &[u8],
        source: VersionSource<'_>,
    ) -> Result<String, i32> {
        self.check_new_name(parent, copy_name)?;
        let existing = self.ns().lookup(parent, copy_name).map(|attr| attr.ino.0);
        let ino = if let Some(ino) = existing {
            // The name is derived from the bytes: an earlier attempt that got
            // as far as creating it is finished by committing into it.
            ino
        } else {
            let (ino, delta) = self.create_empty_file(parent, copy_name)?;
            self.publish(delta);
            ino
        };
        self.commit_version_at(ino, source, now_ms(), None)
            .map_err(|error| commit_errno(&error))?;
        self.ns().path_for(ino).ok_or(EIO)
    }

    /// Handle the last close of `ino`: a clean buffer is dropped; bytes that
    /// can never be committed to this record are preserved as a sibling
    /// copy; anything else stays in memory (and staged) for a later retry.
    fn settle_last_close(&self, ino: u64, live: bool) {
        let Some(staged) = self.stage(ino) else {
            self.drop_buffer(ino);
            return;
        };
        if !live {
            self.clear_unsaved(ino);
            self.drop_buffer(ino);
            return;
        }
        let state = self.buffer_states().get(&ino).copied().unwrap_or_default();
        if state.is_committed() {
            self.drop_buffer(ino);
            return;
        }
        let Some(errno) = state
            .failure
            .filter(|errno| is_permanent_commit_failure(*errno))
        else {
            eprintln!(
                "inode {ino} closed with uncommitted bytes; they stay readable and are retried on the next flush"
            );
            return;
        };
        let preserved = self.preserve_as_sibling(ino, &mut staged.lock());
        match preserved {
            Ok(path) => {
                eprintln!(
                    "inode {ino} could not take another version ({}); its bytes were kept as {path}",
                    io::Error::from_raw_os_error(errno)
                );
                self.clear_unsaved(ino);
                self.drop_buffer(ino);
            }
            Err(copy_errno) => eprintln!(
                "inode {ino} could not take another version and no copy could be created ({}); bytes stay in memory and staged",
                io::Error::from_raw_os_error(copy_errno)
            ),
        }
    }

    /// Commit every dirty buffer, e.g. before unmount. Failures stay staged.
    pub fn persist_all_buffers(&self) {
        let inos: Vec<u64> = self.buffers().keys().copied().collect();
        for ino in inos {
            let _ = self.persist_buffer(ino, PersistIntent::Close);
        }
    }

    /// Replay bytes staged by an earlier run (an `fsync` before a crash, or
    /// a commit that never succeeded). Bytes go back into their own record
    /// only while it still holds the content they were derived from;
    /// otherwise, or when that commit is refused permanently, they become a
    /// sibling copy. Nothing is discarded: entries that cannot be placed stay
    /// staged and are reported. Returns how many entries were placed.
    pub fn recover_unsaved_writes(&self) -> usize {
        let Some(store) = self.unsaved.clone() else {
            return 0;
        };
        let (loaded, damaged) = store.load_all();
        for (path, error) in damaged {
            eprintln!(
                "staged unsaved bytes at {} are unreadable and were left in place: {error}",
                path.display()
            );
        }
        let mut placed = 0_usize;
        for item in loaded {
            let entry = &item.entry;
            let label = entry.path.as_deref().unwrap_or(&entry.key);
            let target = {
                let ns = self.ns();
                entry
                    .record_id
                    .and_then(|record_id| ns.ino_for_record_id(record_id))
                    .filter(|ino| ns.getattr(*ino).is_some())
            };
            if let Some(ino) = target
                && self.ns().hash_for(ino) == entry.base()
                && self.check_access(ino, Access::Write).is_ok()
            {
                match self.commit_content(ino, &item.bytes) {
                    Ok(()) => {
                        let _ = store.remove(&entry.key);
                        placed = placed.saturating_add(1);
                        continue;
                    }
                    Err(error) if !is_permanent_commit_failure(commit_errno(&error)) => {
                        eprintln!(
                            "staged bytes for {label} could not be committed yet ({error}); kept in {}",
                            store.dir().display()
                        );
                        continue;
                    }
                    Err(_) => {}
                }
            }
            let location = if let Some(ino) = target {
                let mut ns = self.ns();
                let name = ns.file_name_for(ino).map(|name| name.as_bytes().to_vec());
                ns.parent_ino(ino).zip(name)
            } else {
                entry.path.as_deref().and_then(|path| {
                    let (dir, name) = path.rsplit_once('/')?;
                    let dir = if dir.is_empty() { "/" } else { dir };
                    let parent = self.ns().resolve_path_ino(dir)?;
                    Some((parent, name.as_bytes().to_vec()))
                })
            };
            let (parent, name) = location
                .unwrap_or_else(|| (crate::namespace::ROOT_INO, entry.key.clone().into_bytes()));
            let copy_name = unsaved_copy_name(&name, &item.bytes);
            match self.create_preserved_copy(parent, &copy_name, VersionSource::Bytes(&item.bytes))
            {
                Ok(path) => {
                    eprintln!("staged bytes for {label} were restored as {path}");
                    let _ = store.remove(&entry.key);
                    placed = placed.saturating_add(1);
                }
                Err(errno) => eprintln!(
                    "staged bytes for {label} could not be restored ({}); kept in {}",
                    io::Error::from_raw_os_error(errno),
                    store.dir().display()
                ),
            }
        }
        placed
    }

    /// `write` after its access check: apply `data` at `offset` to the
    /// inode's stage. Never reads the committed content: ranges nobody
    /// writes stay the base's until a read or the commit needs them.
    fn write_buffer(&self, ino: u64, offset: u64, data: &[u8]) -> Result<u32, i32> {
        // `offset` crosses the FUSE boundary as an untrusted request value.
        offset.checked_add(data.len() as u64).ok_or(EINVAL)?;
        let (stage, opened, base) = self.stage_for_write(ino)?;
        let mut guard = stage.lock();
        guard
            .write(offset, data)
            .map_err(|error| error.raw_os_error().unwrap_or(EIO))?;
        stage.note_len(&guard);
        // The generation moves under the stage lock, so a commit's
        // generation always describes the bytes it committed.
        self.note_buffer_mutation(ino, opened, base);
        drop(guard);
        Ok(u32::try_from(data.len()).unwrap_or(u32::MAX))
    }

    /// `setattr(size)` after its access check. Documents always stage
    /// (imported as a whole on release), as do files that are open; other
    /// opaque files commit the new length immediately.
    fn truncate_inode(&self, ino: u64, new_size: u64) -> Result<(), i32> {
        let open = self.buffers().contains_key(&ino);
        if !open && !self.is_document(ino) {
            return self
                .commit_resized(ino, new_size)
                .map_err(|error| commit_errno(&error));
        }
        let (stage, opened, base) = self.stage_for_write(ino)?;
        let mut guard = stage.lock();
        guard
            .set_len(new_size)
            .map_err(|error| error.raw_os_error().unwrap_or(EIO))?;
        stage.note_len(&guard);
        self.note_buffer_mutation(ino, opened, base);
        drop(guard);
        Ok(())
    }

    /// A range of document `ino`'s shared export, clamped to its length.
    ///
    /// `offset`/`size` cross the FUSE boundary as untrusted request values,
    /// so they are clamped rather than trusted to fit `usize`.
    fn read_document_export(&self, ino: u64, offset: u64, size: u32) -> BufferedRead {
        match self.document_export(ino) {
            Some(bytes) => {
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let end = start.saturating_add(size as usize).min(bytes.len());
                BufferedRead::Shared(bytes, start..end)
            }
            None => BufferedRead::Failed,
        }
    }

    /// What a FUSE `getattr` answers for `ino` (through kernel handle `fh`,
    /// when it has one): the attributes and whether `ino` is a document.
    /// The inode is read under one namespace lock; a document's export size
    /// and a directory's visible link count are the only further reads.
    fn attr_for_request(&self, ino: u64, fh: Option<u64>) -> Result<(FileAttr, bool), i32> {
        // Reflect an in-flight write stage so stat() sees read-your-writes size.
        let buf_len = self.staged_len(ino);
        let open_handle = fh.and_then(|fh| self.open_handle(ino, fh));
        let view = self.inode_view(ino, open_handle.is_some()).ok_or(ENOENT)?;
        // Outside the live view there is nothing to describe, except an inode
        // retained by this exact open kernel handle after unlink/replacement.
        let visible = if view.live {
            self.view_in_view(&view)
        } else {
            open_handle
                .as_ref()
                .is_some_and(|handle| self.in_view(&handle.opened_path))
        };
        if !visible {
            return Err(ENOENT);
        }
        let mut attr = view.attr;
        // Documents have no stored byte length — report the current export
        // size (so a collaboratively-edited document isn't read truncated).
        let doc_len = if buf_len.is_none() && view.document {
            self.document_export(ino).map(|b| b.len() as u64)
        } else {
            None
        };
        if let Some(len) = buf_len.or(doc_len) {
            attr.size = len;
            attr.blocks = len.div_ceil(512);
        }
        if attr.kind == FileType::Directory {
            // Matches the filtered listing: an identity granted
            // nothing sees an ordinary empty directory (nlink == 2).
            attr.nlink = self.visible_dir_nlink(ino);
        }
        Ok((attr, view.document))
    }

    /// Everything a FUSE `read` decides before it replies: authorization,
    /// then the write stage, a document's export, or the committed content.
    /// The inode is read under **one** namespace lock — its liveness, path,
    /// kind, content hash and whether it is a document come back together —
    /// so a read of local content never queues behind a sync apply more than
    /// once. Only a document read takes the lock again, to export it.
    fn plan_read(&self, ino: u64, fh: u64, offset: u64, size: u32) -> ReadPlan {
        let open_handle = self.open_handle(ino, fh);
        let Some(view) = self.inode_view(ino, open_handle.is_some()) else {
            return ReadPlan::Error(ENOENT);
        };
        // Live inodes use their current path. An unlinked/replaced inode has
        // no path, so only the matching kernel handle may continue, with the
        // access path captured when it was opened.
        let access = if view.live {
            self.check_view_access(&view, Access::Read)
        } else if let Some(handle) = &open_handle {
            self.check_path(&handle.opened_path, Access::Read)
        } else {
            Err(ENOENT)
        };
        if let Err(e) = access {
            return ReadPlan::Error(e);
        }
        // Read-your-writes from the stage, else a document's shared export.
        if let Some(staged) = self.read_staged(ino, offset, u64::from(size)) {
            return ReadPlan::Buffered(BufferedRead::Staged(staged));
        }
        if view.document {
            return ReadPlan::Buffered(self.read_document_export(ino, offset, size));
        }
        if view.attr.kind != FileType::RegularFile {
            return ReadPlan::Error(EISDIR);
        }
        let Some(hash) = view.hash else {
            return ReadPlan::Local(Vec::new());
        };
        // Local bytes are answered inline; bytes a peer must supply are
        // left to the caller to hydrate off the FUSE worker.
        let size = u64::from(size);
        match self
            .hydrator
            .block_on(self.hydrator.read_local(hash, offset, size))
        {
            Some(data) => ReadPlan::Local(data),
            None => ReadPlan::Remote(hash),
        }
    }

    /// Answer a `read` whose bytes must come from a peer. The read still
    /// blocks its caller as POSIX expects, but no FUSE worker waits on the
    /// network, so a slow peer stalls only the readers of that file.
    fn reply_remote(
        &self,
        ino: u64,
        hash: Hash,
        range: (u64, u64),
        nonblocking: bool,
        reply: ReplyData,
    ) {
        let (offset, size) = range;
        let hydrator = Arc::clone(&self.hydrator);
        let rt = hydrator.runtime_handle.clone();
        let note = self.unavailable_note(ino, Some(hash));
        if nonblocking {
            // A non-blocking reader gets EAGAIN at once; the range keeps
            // hydrating so a retry finds it local.
            rt.spawn(async move {
                match hydrator.read(hash, offset, size).await {
                    Ok(_) => {
                        if let Some(note) = note {
                            note.arrived().await;
                        }
                    }
                    Err(error) => eprintln!("content hydration failed for {hash}: {error:#}"),
                }
            });
            reply.error(to_errno(libc::EAGAIN));
            return;
        }
        // The kernel holds the page locked while the read waits, so anyone
        // else touching it waits too: the read fails at its bound rather
        // than wait on a peer that went quiet, and the file's record says it
        // is unavailable until bytes arrive, and stored once it is whole.
        rt.spawn(async move {
            let read = async move { hydrator.read(hash, offset, size).await };
            let bound = crate::hydration::BLOCKED_READ_BOUND;
            match unavailable::noted_bounded_read(bound, read, note).await {
                Ok(data) => reply.data(&data),
                Err(error) => {
                    eprintln!("content hydration failed for {hash}: {error:#}");
                    reply.error(to_errno(EIO));
                }
            }
        });
    }

    /// Where a blocked read of `ino`'s bytes notes whether they arrived:
    /// its availability record, when this mount keeps them. `hash` is the
    /// committed content the read fetches, if it does: when it is the
    /// file's current version, the record says the file is stored once
    /// every byte of it is local.
    fn unavailable_note(
        &self,
        ino: u64,
        hash: Option<Hash>,
    ) -> Option<unavailable::UnavailableNote> {
        let registry = Arc::clone(self.availability.as_ref()?);
        let (record_id, revision) = {
            let ns = self.ns();
            (ns.record_id_for(ino)?, ns.content_revision_for(ino))
        };
        let content = hash.zip(revision).and_then(|(hash, revision)| {
            (revision.content_hash.map(|content| content.0) == Some(*hash.as_bytes())).then(|| {
                unavailable::NotedContent {
                    hydrator: Arc::clone(&self.hydrator),
                    hash,
                    version: revision.version_id,
                    stored: Arc::clone(&self.stored_writes),
                }
            })
        });
        Some(unavailable::UnavailableNote {
            registry,
            record_id,
            content,
        })
    }

    /// Answer a FUSE `read` that [`Self::plan_read`] sent to a buffer.
    /// Unwritten ranges a peer must supply are hydrated on the runtime, like
    /// an unbuffered read: no FUSE worker waits on the network.
    fn reply_buffered(&self, ino: u64, buffered: BufferedRead, reply: ReplyData) {
        match buffered {
            BufferedRead::Shared(bytes, range) => reply.data(bytes.get(range).unwrap_or_default()),
            BufferedRead::Staged(Ok(StagedRead::Ready(bytes))) => reply.data(&bytes),
            BufferedRead::Staged(Ok(StagedRead::Missing(missing))) => {
                let hydrator = Arc::clone(&self.hydrator);
                let rt = hydrator.runtime_handle.clone();
                let note = self.unavailable_note(ino, None);
                rt.spawn(async move {
                    let read = async move { Ok(missing.finish(&hydrator).await?) };
                    let bound = crate::hydration::BLOCKED_READ_BOUND;
                    match unavailable::noted_bounded_read(bound, read, note).await {
                        Ok(bytes) => reply.data(&bytes),
                        Err(error) => {
                            eprintln!("content hydration for a staged read failed: {error:#}");
                            reply.error(to_errno(EIO));
                        }
                    }
                });
            }
            BufferedRead::Staged(Err(error)) => {
                eprintln!("read of the write stage of inode {ino} failed: {error}");
                reply.error(to_errno(EIO));
            }
            BufferedRead::Failed => reply.error(to_errno(EIO)),
        }
    }

    /// `flush` (every `close` of a descriptor): commit the buffer.
    fn flush_inode(&self, ino: u64) -> Result<(), i32> {
        self.persist_buffer(ino, PersistIntent::Close)
    }

    /// `fsync`: make the buffer durable, and the name it is reached by.
    ///
    /// Staged bytes alone would come back after a crash only as a sibling
    /// copy when the namespace on disk does not place the file yet: a file
    /// created, written and synced within the persister's settle time
    /// (`namespace_persist::QUIET`). So the namespace is made durable too.
    fn fsync_inode(&self, ino: u64) -> Result<(), i32> {
        self.persist_buffer(ino, PersistIntent::Sync)?;
        self.sync_namespace()
    }

    /// Make the namespace on disk hold every placement made so far: what
    /// `fsync` of a directory promises for the names in it. Without a hook
    /// (no persistent namespace) there is nothing to make durable.
    fn sync_namespace(&self) -> Result<(), i32> {
        let Some(hook) = self.durability_hook.get() else {
            return Ok(());
        };
        hook().map_err(|error| {
            eprintln!("the namespace could not be made durable: {error}");
            EIO
        })
    }

    /// Sets how the namespace is made durable: after each native changeset,
    /// and for every `fsync` and `fsyncdir`. Set once.
    pub fn set_durability_hook(&self, hook: DurabilityHook) {
        let _ = self.durability_hook.set(hook);
    }

    /// `release` of kernel handle `fh`: commit a writable handle's bytes if
    /// `flush` did not already, then settle the buffer on the last close.
    fn release_handle(&self, ino: u64, fh: u64) -> Result<(), i32> {
        let (last_handle, released_writable) = {
            let mut handles = self.open_file_handles.lock_recover();
            let released_writable = handles
                .remove(&fh)
                .is_some_and(|handle| handle.ino == ino && handle.writable);
            (
                !handles.values().any(|handle| handle.ino == ino),
                released_writable,
            )
        };
        let live = self.ns().getattr(ino).is_some();
        let result = if live && released_writable {
            self.persist_buffer(ino, PersistIntent::Close)
        } else {
            Ok(())
        };
        if last_handle {
            self.settle_last_close(ino, live);
        }
        result
    }

    /// Offer kernel passthrough for read-only opens of fully local files,
    /// with backing files materialised under `dir`. Takes effect only when
    /// the kernel negotiates it at `init`.
    #[must_use]
    pub fn with_passthrough(mut self, dir: std::path::PathBuf) -> Self {
        self.passthrough = Some(Passthrough {
            dir,
            kernel_ready: std::sync::atomic::AtomicBool::new(false),
            disabled: std::sync::atomic::AtomicBool::new(false),
            backing: Mutex::new(HashMap::new()),
        });
        self
    }

    /// The attributes `lookup`/`getattr` report for a live inode: in-flight
    /// buffer or document export size, and the view-filtered link count.
    fn presented_attr(&self, ino: u64) -> Option<(fuser::FileAttr, Duration)> {
        let buf_len = self.staged_len(ino);
        let is_doc = self.is_document(ino);
        let doc_len = if buf_len.is_none() && is_doc {
            self.document_export(ino).map(|b| b.len() as u64)
        } else {
            None
        };
        let mut attr = self.ns().getattr(ino)?;
        if let Some(len) = buf_len.or(doc_len) {
            attr.size = len;
            attr.blocks = len.div_ceil(512);
        }
        if attr.kind == FileType::Directory {
            attr.nlink = self.visible_dir_nlink(ino);
        }
        Some((attr, Self::ttl_for(is_doc)))
    }

    /// Whether the availability registry records `ino`'s payload as not
    /// fully resident (never fetched, partially fetched, or evicted).
    fn payload_not_resident(&self, ino: u64) -> bool {
        let Some(registry) = &self.availability else {
            return false;
        };
        let Some(record_id) = self.ns().record_id_for(ino) else {
            return false;
        };
        registry
            .lock_recover()
            .get(record_id)
            .is_some_and(|record| !matches!(record.residency, Residency::Resident { .. }))
    }

    /// `FOPEN_KEEP_CACHE` when the kernel's page cache for `ino` was filled
    /// from the content the file still has; otherwise no flag, which makes
    /// the kernel drop the cache. Buffered and document inodes never keep it,
    /// and neither do files whose payload is not (or no longer) resident, so
    /// "Free Local Space" also frees the kernel's copy.
    fn open_cache_flags(&self, ino: u64) -> FopenFlags {
        let volatile = self.payload_not_resident(ino)
            || self.buffers().contains_key(&ino)
            || self.is_document(ino);
        let hash = self.ns().hash_for(ino);
        let mut cached = self.kernel_cached.lock_recover();
        if volatile {
            cached.remove(&ino);
            return FopenFlags::empty();
        }
        match cached.insert(ino, hash) {
            Some(previous) if previous == hash => FopenFlags::FOPEN_KEEP_CACHE,
            _ => FopenFlags::empty(),
        }
    }

    /// A read-only backing file holding exactly the current content of
    /// `ino`, when passthrough is usable and every chunk is local.
    fn passthrough_backing(&self, ino: u64) -> Option<std::fs::File> {
        let passthrough = self.passthrough.as_ref().filter(|p| p.usable())?;
        if self.payload_not_resident(ino)
            || self.buffers().contains_key(&ino)
            || self.is_document(ino)
        {
            return None;
        }
        let hash = self.ns().hash_for(ino)?;
        // Complete file-backed content is its own backing file.
        let files = self.hydrator.store.files();
        if files
            .entry(hash.as_bytes())
            .is_some_and(|entry| entry.complete())
        {
            return std::fs::File::open(files.path_of(hash.as_bytes())).ok();
        }
        let path = passthrough.dir.join(hash.to_hex());
        if !path.exists() {
            match self
                .hydrator
                .block_on(self.hydrator.export_local(hash, &path))
            {
                Ok(true) => {}
                Ok(false) => return None,
                Err(error) => {
                    eprintln!("could not materialise a passthrough file for {hash}: {error:#}");
                    return None;
                }
            }
        }
        std::fs::File::open(&path).ok()
    }

    fn doc_export_cache(&self) -> std::sync::MutexGuard<'_, DocExports> {
        self.doc_exports.lock_recover()
    }

    /// Whether `ino` is a CRDT-backed document (vs. opaque content).
    fn is_document(&self, ino: u64) -> bool {
        let namespace = self.ns();
        namespace.content_authority() == crate::namespace::ContentAuthority::LegacyNamespace
            && namespace.document_codec(ino).is_some()
    }

    /// The hydration state of `ino`, served for the `user.tilo.sync` xattr.
    /// `None` when the inode doesn't exist. Directories, CRDT documents and
    /// empty files are always [`SyncState::Local`]; a regular file's state
    /// comes from the hydrator (is every chunk in the cache?).
    fn sync_state_for(&self, ino: u64) -> Option<fs_protocol::SyncState> {
        use fs_protocol::SyncState;
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
        Some(self.hydrator.block_on(self.hydrator.sync_state(hash)))
    }

    /// Attribute/entry TTL: zero for documents (size is computed live from
    /// the CRDT and changes as peers edit, so the kernel must not cache a
    /// stale `i_size`), the normal TTL otherwise.
    fn ttl_for(is_doc: bool) -> Duration {
        if is_doc { Duration::ZERO } else { TTL }
    }

    /// Commit a complete stage: import it as a document, or ingest it as
    /// opaque chunked content (a file-backed stage straight from its file).
    /// Returns the errno the writer should see when the bytes did not become
    /// the file's committed content.
    fn commit_buffer(&self, ino: u64, stage: &mut WriteStage) -> Result<(), i32> {
        if self.is_document(ino) {
            let data = stage.contiguous().map_err(|error| commit_errno(&error))?;
            return self.commit_document(ino, &data);
        }
        Self::commit_retrying(ino, || {
            stage
                .source()
                .and_then(|source| self.commit_version_at(ino, source, now_ms(), None))
        })?;
        self.remember_staged_metadata(ino, stage);
        Ok(())
    }

    /// [`Self::commit_buffer`] of whole bytes held in memory.
    fn commit_bytes(&self, ino: u64, data: &[u8]) -> Result<(), i32> {
        if self.is_document(ino) {
            return self.commit_document(ino, data);
        }
        self.remember_rich_metadata(ino, data);
        Self::commit_retrying(ino, || self.commit_content(ino, data))
    }

    /// Run `commit` until it succeeds or fails for good. `EBUSY` means the
    /// committed revision moved under the commit (a remote replay or a
    /// concurrent local commit); the bytes still have to land, so it is
    /// tried again against the new revision.
    fn commit_retrying(ino: u64, mut commit: impl FnMut() -> io::Result<()>) -> Result<(), i32> {
        let mut attempts = 0_u8;
        loop {
            match commit() {
                Ok(()) => return Ok(()),
                Err(error) if commit_errno(&error) == EBUSY && attempts < 3 => {
                    attempts = attempts.saturating_add(1);
                }
                Err(error) => {
                    eprintln!("commit of inode {ino} failed: {error}");
                    return Err(commit_errno(&error));
                }
            }
        }
    }

    /// Import `data` as the content of document `ino`.
    fn commit_document(&self, ino: u64, data: &[u8]) -> Result<(), i32> {
        if self
            .document_export(ino)
            .is_some_and(|current| *current == *data)
        {
            self.remember_rich_metadata(ino, data);
            return Ok(());
        }
        // The version records the written bytes by the content id the store
        // serves them under, like any other file's, so history can read and
        // restore it. A failed ingest fails the write before anything changes.
        let content = self
            .write_content_blocking(data)
            .map_err(|error| commit_errno(&error))?
            .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()));
        // The version's metadata finds these facts by its content id.
        self.remember_rich_metadata_as(
            ino,
            data,
            content.unwrap_or_else(|| fs_protocol::ContentHash(*blake3::hash(data).as_bytes())),
        );
        // import_document validates before replacing; an invalid input
        // leaves the previous content intact and returns false.
        let delta = {
            let mut ns = self.ns();
            if ns.import_document(ino, data, now_ms(), &self.document_content) {
                ns.save_incremental()
            } else {
                Vec::new()
            }
        };
        self.doc_export_cache().remove(&ino);
        if delta.is_empty() {
            eprintln!("document import rejected (invalid format); previous content kept");
            return Err(EINVAL);
        }
        self.publish(delta);
        self.record_version(ino, fs_protocol::FileProvenance::Edit, content);
        Ok(())
    }

    /// Rich facts of a just-committed opaque stage, from the bounded prefix
    /// the extractor reads, keyed by the content id the version committed.
    fn remember_staged_metadata(&self, ino: u64, stage: &WriteStage) {
        let (record_id, name, hash) = {
            let ns = self.ns();
            let Some(record_id) = ns.record_id_for(ino) else {
                return;
            };
            let Some(path) = ns.path_for(ino) else {
                return;
            };
            let Some(hash) = ns.hash_for(ino) else {
                return;
            };
            drop(ns);
            let name = path.rsplit('/').next().unwrap_or_default().to_owned();
            (record_id, name, hash)
        };
        if !crate::rich_metadata::recognizes(&name) {
            return;
        }
        let Ok(prefix) = stage.prefix(crate::rich_metadata::MAX_METADATA_SCAN_BYTES as u64) else {
            return;
        };
        if let Some(properties) = crate::rich_metadata::extract(&name, &prefix) {
            self.rich_metadata.lock_recover().insert(
                record_id,
                (fs_protocol::ContentHash(*hash.as_bytes()), properties),
            );
        }
    }

    fn remember_rich_metadata(
        &self,
        ino: u64,
        bytes: &[u8],
    ) -> Option<fs_protocol::FilePropertiesV1> {
        let hash = fs_protocol::ContentHash(*blake3::hash(bytes).as_bytes());
        self.remember_rich_metadata_as(ino, bytes, hash)
    }

    /// [`Self::remember_rich_metadata`] keyed by `hash`, the content id the
    /// version holding `bytes` records.
    fn remember_rich_metadata_as(
        &self,
        ino: u64,
        bytes: &[u8],
        hash: fs_protocol::ContentHash,
    ) -> Option<fs_protocol::FilePropertiesV1> {
        let (record_id, name) = {
            let ns = self.ns();
            let record_id = ns.record_id_for(ino)?;
            let name = ns.path_for(ino)?.rsplit('/').next()?.to_owned();
            drop(ns);
            (record_id, name)
        };
        let properties = crate::rich_metadata::extract(&name, bytes)?;
        self.rich_metadata
            .lock_recover()
            .insert(record_id, (hash, properties.clone()));
        Some(properties)
    }

    /// The serialized (on-wire) bytes of a document inode, exported once per
    /// change of that document and shared, so multi-chunk reads and `getattr`
    /// are cheap and consistent. A change elsewhere in the namespace only
    /// advances the heads the cached export is known current at.
    fn document_export(&self, ino: u64) -> Option<Arc<[u8]>> {
        let cached = self.doc_export_cache().get(&ino).cloned();
        let mut ns = self.ns();
        let heads = ns.heads();
        if let Some((known, bytes)) = cached
            && (known == heads || !ns.document_changed_between(ino, &known, &heads))
        {
            drop(ns);
            if known != heads {
                self.doc_export_cache()
                    .insert(ino, (heads, Arc::clone(&bytes)));
            }
            return Some(bytes);
        }
        let bytes: Arc<[u8]> = ns.export_document(ino, &self.document_content)?.into();
        let heads = ns.heads();
        drop(ns);
        self.doc_export_cache()
            .insert(ino, (heads, Arc::clone(&bytes)));
        Some(bytes)
    }

    /// Attach a sync handle so write ops propagate to peers.
    #[must_use]
    pub fn with_sync(mut self, sync: Arc<SyncHandle>) -> Self {
        self.sync = Some(sync);
        self.start_record_index_sync_worker();
        self
    }

    fn start_record_index_sync_worker(&self) {
        let (Some(sync), Some(enforcement)) = (&self.sync, self.enforcement.clone()) else {
            return;
        };
        self.start_record_index_refresh_worker(
            sync.subscribe_record_index_generation(),
            enforcement,
        );
    }

    fn start_record_index_acl_worker(&self) {
        let Some((acl, identity)) = self.enforcement.clone() else {
            return;
        };
        self.start_record_index_refresh_worker(acl.subscribe_revision(), (acl, identity));
    }

    fn start_record_index_refresh_worker(
        &self,
        mut generations: tokio::sync::watch::Receiver<u64>,
        enforcement: (AclHandle, crate::acl::IdentityKey),
    ) {
        let namespace = Arc::clone(&self.namespace);
        let retained = Arc::downgrade(&self.acl_visible_record_index);
        drop(self.hydrator.runtime_handle.spawn(async move {
            while generations.changed().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                while generations.has_changed().unwrap_or(false) {
                    let _ = generations.borrow_and_update();
                }
                let Some(retained) = retained.upgrade() else {
                    break;
                };
                // The refresh copies the catalogue and evaluates the ACL:
                // blocking work that must not occupy a runtime worker.
                let namespace = Arc::clone(&namespace);
                let enforcement = enforcement.clone();
                let refreshed = tokio::task::spawn_blocking(move || {
                    QuickFS::refresh_acl_visible_record_index_into(
                        &namespace,
                        Some(&enforcement),
                        &retained,
                    );
                })
                .await;
                if refreshed.is_err() {
                    break;
                }
            }
        }));
    }

    /// Attach a host-side thumbnail provider (cache under `dir`). The mount then
    /// answers the `user.tilo.thumb` xattr with a generated preview and records
    /// each thumbnail's hash on the namespace node so it projects to peers.
    #[must_use]
    pub fn with_thumbs(mut self, provider: crate::thumbs::ThumbProvider) -> Self {
        self.thumbs = Some(provider);
        self
    }

    /// Keep the binary content of documents projected as files in `content` (in memory unless set).
    #[must_use]
    pub fn with_document_content(mut self, content: tilo_content::ContentStore) -> Self {
        self.document_content = content;
        self
    }

    #[must_use]
    pub fn with_availability(mut self, registry: Arc<Mutex<AvailabilityRegistry>>) -> Self {
        // The cache budget gives up what a host acknowledged holding first.
        let receipts = Arc::downgrade(&registry);
        self.hydrator
            .store
            .files()
            .set_held_elsewhere(Arc::new(move || {
                receipts
                    .upgrade()
                    .map_or_else(Default::default, |registry| {
                        registry.lock_recover().held_elsewhere()
                    })
            }));
        self.availability = Some(registry);
        self
    }

    /// Start the mount-lifetime worker that turns durable pin policy into
    /// verified local residency. Call this after attaching the live ACL so the
    /// worker never hydrates records outside this mount's current view.
    #[must_use]
    pub fn with_pinned_hydration(self) -> Self {
        self.with_pinned_hydration_every(PINNED_HYDRATION_BACKSTOP)
    }

    /// [`Self::with_pinned_hydration`] with its backstop tick every `repair`.
    fn with_pinned_hydration_every(mut self, repair: Duration) -> Self {
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
        let mut namespace_changes = namespace.lock_recover().subscribe_changes();
        let mut registry_changes = registry.lock_recover().subscribe_hydration();
        let mut acl_changes = enforcement
            .as_ref()
            .map(|(acl, _)| acl.subscribe_revision());
        #[cfg(test)]
        let worker_turns = Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let turns = Arc::clone(&worker_turns);
        let task = self.hydrator.runtime_handle.spawn(async move {
            let mut interval = tokio::time::interval(repair);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_inputs = None;
            let mut outcome = PinnedPassOutcome::default();
            loop {
                #[cfg(test)]
                turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut woken = outcome.more_due;
                if !outcome.more_due {
                    let retry = outcome.next_retry_at_ms.map(retry_instant);
                    tokio::select! {
                        _ = interval.tick() => {}
                        changed = wakes.changed() => {
                            if changed.is_err() { break; }
                            woken = true;
                        }
                        () = next_change(Some(&mut namespace_changes)) => {
                            tokio::time::sleep(PINNED_CHANGE_SETTLE).await;
                        }
                        () = next_change(Some(&mut registry_changes)) => {}
                        () = next_change(acl_changes.as_mut()) => {}
                        () = until(retry) => {}
                    }
                }
                let inputs = PinnedPassInputs::capture(&namespace, &registry, enforcement.as_ref());
                // Only a local wake asks for a pass outright. Anything else
                // is acted on when the namespace, ACL or registry moved since
                // the last pass, or a backoff fell due: otherwise there is
                // nothing to walk.
                if !woken
                    && last_inputs.as_ref() == Some(&inputs)
                    && outcome
                        .next_retry_at_ms
                        .is_none_or(|retry_at| retry_at > now_ms())
                {
                    continue;
                }
                outcome = reconcile_pinned_hydration(
                    &namespace,
                    Arc::clone(&hydrator),
                    Arc::clone(&registry),
                    enforcement.as_ref(),
                    wakes.clone(),
                )
                .await;
                last_inputs = Some(inputs);
            }
        });
        self.pinned_hydration = Some(PinnedHydrationTask {
            wake,
            task,
            #[cfg(test)]
            turns: worker_turns,
        });
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

    /// Answer and accept `signal`'s attribute, and tell it which directories
    /// the kernel lists; its worker raises the signals once the mount is up
    /// ([`crate::change_signal`]).
    #[must_use]
    pub fn with_change_signal(mut self, signal: Arc<crate::change_signal::ChangeSignal>) -> Self {
        self.change_signal = Some(signal);
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
        actor: fs_protocol::FileActor,
    ) -> Result<Self, crate::history::HistoryError> {
        self.history = Some((history, actor));
        if self.allow_legacy_record_migration {
            self.ensure_history_baseline()?;
        }
        Ok(self)
    }

    /// Attribute this mount's versions of the paths the desktop session
    /// leases to the agent each lease names ([`crate::write_attribution`]).
    #[must_use]
    pub fn with_write_attributions(
        mut self,
        attributions: Arc<crate::write_attribution::WriteAttributions>,
    ) -> Self {
        self.write_attributions = Some(attributions);
        self
    }

    /// Record which agent caused this device's `version`, when a session
    /// lease covers `path` (or, without one, the record's current path).
    /// Signed by this device like the version itself. A failure leaves the
    /// version the person's and never fails the write.
    fn attribute_version(&self, version: &fs_protocol::FileVersionV1, path: Option<&str>) {
        let (Some(attributions), Some(publisher)) =
            (&self.write_attributions, &self.record_publisher)
        else {
            return;
        };
        // Most writes happen with no lease at all: they cost no path lookup.
        if attributions.is_idle() {
            return;
        }
        let path = if let Some(path) = path {
            Some(path.to_owned())
        } else {
            let namespace = self.ns();
            namespace
                .ino_for_record_id(version.record_id)
                .and_then(|ino| namespace.path_for(ino))
        };
        let Some(attribution) = path.and_then(|path| attributions.for_path(&path)) else {
            return;
        };
        if let Err(error) = publisher.attribute(version.record_id, version.id, &attribution) {
            eprintln!(
                "could not attribute version {:?} to {}: {error}",
                version.id, attribution.actor
            );
        }
    }

    /// [`Self::attribute_version`] for each of a committed transaction's
    /// versions.
    fn attribute_versions(&self, versions: &[fs_protocol::FileVersionV1], path: Option<&str>) {
        for version in versions {
            self.attribute_version(version, path);
        }
    }

    /// Whether the session leases any write attribution right now.
    fn attributing(&self) -> bool {
        self.write_attributions
            .as_ref()
            .is_some_and(|attributions| !attributions.is_idle())
    }

    /// Attach the signed record publisher and backfill locally authored
    /// history that predates the record transport.
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
            publisher
                .repository()
                .restore_admission_fences(&*journal.lock().map_err(|_| {
                    crate::record_publisher::RecordPublisherError::PublicationUnavailable
                })?)?;
            let sync = self.sync.clone();
            publisher.recover_replacement_transactions(
                &mut journal.lock_recover(),
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
                    .filter(|preview| preview.status == fs_protocol::PreviewStatus::Ready)
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

    fn ensure_record_publication(
        &self,
    ) -> Result<(), crate::record_publisher::RecordPublisherError> {
        let (Some(publisher), Some((history, _))) = (&self.record_publisher, &self.history) else {
            return Ok(());
        };
        let binding_inodes = self.live_inodes_with_root();
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
        let inodes = self.live_regular_file_inodes();
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

    fn ensure_history_baseline(&self) -> Result<(), crate::history::HistoryError> {
        let Some((history, _)) = &self.history else {
            return Ok(());
        };
        let inodes = self.live_inodes_with_root();
        for ino in inodes {
            let Some(snapshot) = self.version_snapshot(ino, false, None) else {
                continue;
            };
            if history.snapshot(snapshot.record_id).is_none() {
                self.append_version(snapshot, fs_protocol::FileProvenance::Create, now_ms())?;
            }
        }
        Ok(())
    }

    fn live_inodes_with_root(&self) -> Vec<u64> {
        let mut namespace = self.ns();
        let mut inodes = Self::live_inodes_locked(&mut namespace);
        drop(namespace);
        inodes.insert(0, crate::namespace::ROOT_INO);
        inodes
    }

    fn live_inodes_locked(namespace: &mut Namespace) -> Vec<u64> {
        let paths = namespace
            .live_node_ids()
            .into_iter()
            .filter_map(|id| namespace.path_for_id(&id))
            .collect::<Vec<_>>();
        paths
            .iter()
            .filter_map(|path| namespace.resolve_path_ino(path))
            .collect()
    }

    fn live_regular_file_inodes(&self) -> Vec<u64> {
        let mut namespace = self.ns();
        let live_inodes = Self::live_inodes_locked(&mut namespace);
        let regular_file_inodes = live_inodes
            .into_iter()
            .filter(|ino| {
                namespace
                    .getattr(*ino)
                    .is_some_and(|attr| attr.kind == FileType::RegularFile)
            })
            .collect();
        drop(namespace);
        regular_file_inodes
    }

    fn version_snapshot(
        &self,
        ino: u64,
        deleted: bool,
        content_override: Option<fs_protocol::ContentHash>,
    ) -> Option<VersionSnapshot> {
        let namespace = self.ns();
        Self::version_snapshot_locked(&namespace, ino, deleted, content_override, None)
    }

    fn version_snapshot_locked(
        namespace: &Namespace,
        ino: u64,
        deleted: bool,
        content_override: Option<fs_protocol::ContentHash>,
        byte_len_override: Option<u64>,
    ) -> Option<VersionSnapshot> {
        let record_id = namespace.record_id_for(ino)?;
        let attr = namespace.getattr(ino)?;
        let content_hash = content_override.or_else(|| {
            namespace
                .hash_for(ino)
                .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()))
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
            metadata_hash: fs_protocol::ContentHash(*blake3::hash(&bytes).as_bytes()),
            byte_len,
            kind: attr.kind,
        })
    }

    fn record_metadata(
        &self,
        snapshot: &VersionSnapshot,
        observed_at_ms: u64,
        actor: fs_protocol::FileActor,
    ) -> fs_protocol::FileMetadataV1 {
        let properties = self
            .rich_metadata
            .lock_recover()
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
        fs_protocol::FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: snapshot.record_id,
            content_hash: snapshot.content_hash,
            byte_len: snapshot.byte_len,
            mime: None,
            previews: Vec::new(),
            latest_actor: Some(actor),
            observed_at_ms,
            index_status: fs_protocol::IndexStatus::Pending,
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
        fs_protocol::MetadataProjectionV1 {
            schema_version: fs_protocol::projection::METADATA_PROJECTION_VERSION,
            record_id,
            properties,
        }
        .to_xattr_bytes()
        .ok()
    }

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
        drop(namespace);
        let bytes = postcard::to_allocvec(&metadata).ok()?;
        Some(VersionSnapshot {
            record_id,
            content_hash: content_hash.map(|hash| fs_protocol::ContentHash(*hash.as_bytes())),
            metadata_hash: fs_protocol::ContentHash(*blake3::hash(&bytes).as_bytes()),
            byte_len,
            kind: attr.kind,
        })
    }

    fn append_version(
        &self,
        snapshot: VersionSnapshot,
        provenance: fs_protocol::FileProvenance,
        timestamp_ms: u64,
    ) -> Result<fs_protocol::FileVersionId, crate::history::HistoryError> {
        self.append_version_at(snapshot, provenance, timestamp_ms, timestamp_ms)
    }

    /// [`Self::append_version`] with the signed metadata stamped at
    /// `observed_at_ms`, the version's modification time, which peers
    /// materialise as the file's mtime.
    fn append_version_at(
        &self,
        snapshot: VersionSnapshot,
        provenance: fs_protocol::FileProvenance,
        timestamp_ms: u64,
        observed_at_ms: u64,
    ) -> Result<fs_protocol::FileVersionId, crate::history::HistoryError> {
        let Some((history, actor)) = &self.history else {
            return Ok(fs_protocol::FileVersionId::new());
        };
        let parents = history
            .snapshot(snapshot.record_id)
            .map_or_else(Vec::new, |history| history.heads);
        let id = fs_protocol::FileVersionId::new();
        let version = fs_protocol::FileVersionV1 {
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
            let published = if snapshot.kind == FileType::Directory {
                publisher.publish_structural_version(&version).map(|_| ())
            } else {
                publisher
                    .publish(
                        &self.record_metadata(&snapshot, observed_at_ms, *actor),
                        std::slice::from_ref(&version),
                    )
                    .map(|_| ())
            };
            published.map_err(|error| {
                if error.is_capacity_exhausted() {
                    crate::history::HistoryError::CapacityExhausted(error.to_string())
                } else {
                    crate::history::HistoryError::Publication(error.to_string())
                }
            })?;
        }
        self.attribute_version(&version, None);
        history.append(version)?;
        if let Some(registry) = &self.availability {
            let mut registry = registry.lock_recover();
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
        provenance: fs_protocol::FileProvenance,
        content_override: Option<fs_protocol::ContentHash>,
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

    /// The namespace owns the record/path index.  This method only fingerprints
    /// its current CRDT heads plus live ACL generation; it does not enumerate
    /// records.  Domain and length framing prevent ambiguous concatenation
    /// from producing an equivalent cursor generation.
    fn record_catalog_revision(&self) -> String {
        let namespace = self.ns();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        Self::record_catalog_revision_from_index(namespace.indexed_record_revision(), acl_revision)
    }

    fn record_catalog_revision_from_index(index_revision: &str, acl_revision: u64) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v2.revision\\0");
        let index_revision = index_revision.as_bytes();
        hasher.update(&(index_revision.len() as u64).to_be_bytes());
        hasher.update(index_revision);
        hasher.update(b"acl-revision\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Return a retained projection by cheap shared reference. xattr serving
    /// deliberately never rebuilds this view: a remote replay that has not
    /// yet delivered its refresh notification is transiently unavailable
    /// rather than turning a manifest/page request into a corpus walk.
    fn acl_visible_record_index(&self) -> Option<Arc<AclVisibleRecordIndex>> {
        let source_revision = self.record_catalog_revision();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        self.acl_visible_record_index
            .lock_recover()
            .as_ref()
            .filter(|index| {
                index.source_revision == source_revision && index.acl_revision == acl_revision
            })
            .cloned()
    }

    /// Rebuild the retained authority view under one coherent Namespace lock.
    /// Call only from mount/mutation/sync notification paths, never from an
    /// xattr request.  This is allowed to visit the catalogue once because it
    /// replaces an immutable snapshot used by all subsequent bounded pages.
    fn refresh_acl_visible_record_index(&self) {
        Self::refresh_acl_visible_record_index_into(
            &self.namespace,
            self.enforcement.as_ref(),
            &self.acl_visible_record_index,
        );
    }

    /// Rebuild the retained view when the namespace catalogue or the ACL
    /// moved. The Namespace lock covers only copying the catalogue rows; the
    /// ACL is evaluated after it is released, and under an unchanged ACL
    /// revision only for rows whose path the previous view did not judge.
    fn refresh_acl_visible_record_index_into(
        namespace: &Arc<Mutex<Namespace>>,
        enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
        retained: &Arc<Mutex<Option<Arc<AclVisibleRecordIndex>>>>,
    ) {
        let acl_revision = enforcement.map_or(0, |(acl, _)| acl.revision());
        let previous = retained.lock_recover().clone();
        let (source_revision, rows) = {
            let namespace = namespace.lock_recover();
            let source_revision = Self::record_catalog_revision_from_index(
                namespace.indexed_record_revision(),
                acl_revision,
            );
            // A content edit or a coalesced notification that moved no row
            // leaves the retained view exactly current.
            if previous.as_ref().is_some_and(|index| {
                index.source_revision == source_revision && index.acl_revision == acl_revision
            }) {
                return;
            }
            let mut rows = Vec::with_capacity(namespace.indexed_record_count());
            loop {
                let page = namespace.indexed_record_page(rows.len(), RECORD_INDEX_PAGE_ENTRIES);
                if page.is_empty() {
                    break;
                }
                rows.extend(page);
            }
            drop(namespace);
            (source_revision, rows)
        };
        let reusable = previous.filter(|index| index.acl_revision == acl_revision);
        #[cfg(test)]
        let mut acl_evaluations = 0_usize;
        let mut hidden = HashMap::new();
        let mut records = Vec::with_capacity(rows.len());
        for entry in rows {
            let visible = reusable
                .as_deref()
                .and_then(|index| index.verdict(entry.record_id, &entry.path))
                .unwrap_or_else(|| {
                    #[cfg(test)]
                    {
                        acl_evaluations = acl_evaluations.saturating_add(1);
                    }
                    enforcement.is_none_or(|(acl, identity)| acl.visible(identity, &entry.path))
                });
            if visible {
                records.push(RecordIndexEntryV1 {
                    record_id: entry.record_id,
                    path: entry.path,
                    is_dir: entry.is_dir,
                });
            } else {
                hidden.insert(entry.record_id, entry.path);
            }
        }

        // The public generation binds exactly the data this authority can
        // observe (and its ACL generation), with unambiguous field framing.
        // Hidden namespace edits cannot perturb it.
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v3.visible-catalogue\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.update(&(records.len() as u64).to_be_bytes());
        for record in &records {
            hasher.update(record.record_id.0.as_bytes());
            hasher.update(&(record.path.len() as u64).to_be_bytes());
            hasher.update(record.path.as_bytes());
            hasher.update(&[u8::from(record.is_dir)]);
        }
        let revision = hasher.finalize().to_hex().to_string();
        let by_id = records
            .iter()
            .cloned()
            .map(|record| (record.record_id, record))
            .collect();
        let index = Arc::new(AclVisibleRecordIndex {
            source_revision: source_revision.clone(),
            acl_revision,
            revision,
            records,
            by_id,
            hidden,
            #[cfg(test)]
            acl_evaluations,
        });
        // Do not publish a coherent-but-old snapshot if a concurrent mutation
        // landed after releasing the namespace lock above. Its notification
        // will build the next generation; serving mixed state is never okay.
        let current_source = {
            let namespace = namespace.lock_recover();
            Self::record_catalog_revision_from_index(
                namespace.indexed_record_revision(),
                acl_revision,
            )
        };
        // Nor one whose verdicts may mix two ACL generations: the revision
        // bump has its own refresh coming.
        if current_source != source_revision
            || enforcement.map_or(0, |(acl, _)| acl.revision()) != acl_revision
        {
            return;
        }
        let mut retained = retained.lock_recover();
        *retained = Some(index);
    }

    fn record_index_manifest_v2(&self) -> Option<Vec<u8>> {
        let revision = self.record_catalog_revision();
        let records = self.ns().indexed_record_count();
        let manifest = fs_protocol::projection::RecordIndexManifestV2 {
            schema_version: RECORD_INDEX_V2_VERSION,
            revision,
            // This is a bounded-cursor ceiling, not an ACL-visible count.
            // Pages apply ACL filtering locally and may be empty; publishing a
            // visible count would require an unbounded denied-row scan.
            records,
            page_entries: RECORD_INDEX_PAGE_ENTRIES,
        };
        manifest.validate().ok()?;
        serde_json::to_vec(&manifest).ok()
    }

    fn record_index_page_v2(&self, revision: &str, offset: usize) -> Option<Vec<u8>> {
        (self.record_catalog_revision() == revision).then_some(())?;
        let page_entries = RECORD_INDEX_PAGE_ENTRIES;
        let namespace = self.ns();
        let records = namespace.indexed_record_count();
        if offset > records || !offset.is_multiple_of(page_entries) {
            return None;
        }
        let end = offset.saturating_add(page_entries).min(records);
        let candidates = namespace.indexed_record_page(offset, page_entries);
        drop(namespace);
        let page = fs_protocol::projection::RecordIndexPageV2 {
            schema_version: RECORD_INDEX_V2_VERSION,
            revision: revision.to_string(),
            offset,
            next_offset: (end < records).then_some(end),
            records: candidates
                .into_iter()
                .filter(|entry| self.in_view(&entry.path))
                .map(|entry| RecordIndexEntryV1 {
                    record_id: entry.record_id,
                    path: entry.path,
                    is_dir: entry.is_dir,
                })
                .collect(),
        };
        page.validate().ok()?;
        let bytes = serde_json::to_vec(&page).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_RECORD_INDEX_PAGE_BYTES).then_some(bytes)
    }

    fn record_index_cursor(
        &self,
        revision: String,
        source_revision: String,
        acl_revision: u64,
        offset: usize,
    ) -> String {
        let cursor = uuid::Uuid::new_v4().to_string();
        let mut cursors = self.record_index_cursors.lock_recover();
        if cursors.len() >= 1024
            && let Some(evicted) = cursors.keys().next().cloned()
        {
            cursors.remove(&evicted);
        }
        cursors.insert(
            cursor.clone(),
            RecordIndexCursor {
                revision,
                source_revision,
                acl_revision,
                offset,
            },
        );
        cursor
    }

    fn record_index_manifest_v3(&self) -> Option<Vec<u8>> {
        let index = self.acl_visible_record_index()?;
        // Initial issuance is stable for an unchanged authority view.  A
        // hidden source change keeps both the public revision and this opaque
        // token stable, so polling cannot correlate hidden activity.
        let key = (index.revision.clone(), String::new(), index.acl_revision);
        let initial_cursor = {
            let initials = self.record_index_initial_cursors.lock_recover();
            initials.get(&key).cloned()
        }
        .unwrap_or_else(|| {
            let cursor = self.record_index_cursor(
                index.revision.clone(),
                index.source_revision.clone(),
                index.acl_revision,
                0,
            );
            let mut initials = self.record_index_initial_cursors.lock_recover();
            if initials.len() >= 128
                && let Some(evicted) = initials.keys().next().cloned()
            {
                initials.remove(&evicted);
            }
            initials.insert(key, cursor.clone());
            cursor
        });
        let manifest = fs_protocol::projection::RecordIndexManifestV3 {
            schema_version: 3,
            initial_cursor,
            revision: index.revision.clone(),
            page_entries: RECORD_INDEX_PAGE_ENTRIES,
        };
        manifest.validate().ok()?;
        serde_json::to_vec(&manifest).ok()
    }

    fn record_index_page_v3(&self, revision: &str, cursor: &str) -> Option<Vec<u8>> {
        let (source_revision, acl_revision, offset) = self
            .record_index_cursors
            .lock_recover()
            .get(cursor)
            .filter(|state| state.revision == revision)
            .map(|state| {
                (
                    state.source_revision.clone(),
                    state.acl_revision,
                    state.offset,
                )
            })?;
        let index = self.acl_visible_record_index()?;
        // A changed visible projection (or any ACL replacement) fails closed.
        // The private source revision is retained in the token state but a
        // hidden-only source update intentionally leaves an equivalent view
        // traversable under the same public capability.
        (index.revision == revision && index.acl_revision == acl_revision).then_some(())?;
        let records = index.records.len();
        let end = offset
            .saturating_add(RECORD_INDEX_PAGE_ENTRIES)
            .min(records);
        let candidates = index.records.get(offset..end)?.to_vec();
        let next_cursor = (end < records).then(|| {
            self.record_index_cursor(revision.to_string(), source_revision, acl_revision, end)
        });
        let page = fs_protocol::projection::RecordIndexPageV3 {
            schema_version: 3,
            revision: revision.to_string(),
            next_cursor,
            records: candidates,
        };
        page.validate().ok()?;
        let bytes = serde_json::to_vec(&page).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_RECORD_INDEX_PAGE_BYTES).then_some(bytes)
    }

    fn record_index_by_id_v2(&self, record_id: fs_protocol::FileRecordId) -> Option<Vec<u8>> {
        let index = self.acl_visible_record_index()?;
        let entry = index.by_id.get(&record_id)?.clone();
        let projection = fs_protocol::projection::RecordIndexExactV2 {
            schema_version: RECORD_INDEX_V2_VERSION,
            revision: index.revision.clone(),
            record: entry,
        };
        projection.validate().ok()?;
        let bytes = serde_json::to_vec(&projection).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_RECORD_INDEX_PAGE_BYTES).then_some(bytes)
    }

    /// Read a bounded immutable version for an app-native history preview.
    /// This never changes heads and is addressable only through a version id
    /// already exposed by the caller's history projection.
    fn history_version_preview(
        &self,
        ino: u64,
        version_id: fs_protocol::FileVersionId,
    ) -> Result<Vec<u8>, i32> {
        self.check_access(ino, Access::Read)?;
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
        let Some(content_hash) = version.content_hash else {
            return Ok(Vec::new());
        };
        // Bounded: a preview reads at most one byte past its limit.
        let limit = u64::try_from(fs_protocol::MAX_HISTORY_PREVIEW_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let bytes = self
            .hydrator
            .block_on(self.hydrator.read(Hash::from(content_hash.0), 0, limit))
            .map_err(|_| EIO)?;
        if bytes.len() > fs_protocol::MAX_HISTORY_PREVIEW_BYTES {
            return Err(libc::E2BIG);
        }
        Ok(bytes)
    }

    /// Run the same bounded deterministic merge check used by the commit path,
    /// but expose only a small explanation for File Explorer.  This is a
    /// read-only operation: a failed or stale preflight leaves the conflict
    /// exactly as it was.
    #[allow(
        clippy::too_many_lines,
        reason = "the bounded preflight keeps access checks, exact immutable-version hydration, deterministic merge classification, and the validated wire projection in one auditable read-only path"
    )]
    fn history_merge_preflight(&self, ino: u64, conflict_id: uuid::Uuid) -> Result<Vec<u8>, i32> {
        self.check_access(ino, Access::Read)?;
        let (history, _) = self.history.as_ref().ok_or(libc::ENOTSUP)?;
        let (record_id, source_name) = {
            let namespace = self.ns();
            (
                namespace.record_id_for(ino).ok_or(ENOENT)?,
                namespace.file_name_for(ino).ok_or(EIO)?,
            )
        };
        let extension = std::str::from_utf8(source_name.as_bytes())
            .ok()
            .and_then(|name| {
                name.rsplit_once('.')
                    .map(|(_, ext)| ext.to_ascii_lowercase())
            })
            .ok_or(libc::ENOTSUP)?;
        if !matches!(extension.as_str(), "txt" | "qdoc" | "tilo-doc") {
            return Err(libc::ENOTSUP);
        }
        let state = history.snapshot(record_id).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .filter(|conflict| {
                conflict.status == fs_protocol::ConflictStatus::Open && conflict.versions.len() == 2
            })
            .ok_or(EINVAL)?;
        let load = |version_id| -> Result<Vec<u8>, i32> {
            let version = state
                .versions
                .iter()
                .find(|value| value.id == version_id)
                .ok_or(EINVAL)?;
            let Some(hash) = version.content_hash else {
                return Ok(Vec::new());
            };
            // Bounded: one byte past the largest input a merge accepts.
            let limit = u64::try_from(crate::conflict_merge::MAX_MERGE_INPUT_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let bytes = self
                .hydrator
                .block_on(self.hydrator.read(Hash::from(hash.0), 0, limit))
                .map_err(|_| EIO)?;
            if bytes.len() > crate::conflict_merge::MAX_MERGE_INPUT_BYTES {
                return Err(libc::E2BIG);
            }
            Ok(bytes)
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
        let preflight = if matches!(extension.as_str(), "qdoc" | "tilo-doc") {
            crate::conflict_merge::preflight_exact(first, second, None)
        } else {
            let base_id =
                crate::conflict_merge::exact_common_base(&state.versions, first_id, second_id)
                    .map_err(|_| libc::ENOTSUP)?;
            crate::conflict_merge::preflight_exact(
                first,
                second,
                Some(crate::conflict_merge::ExactMergeBase {
                    version_id: base_id,
                    bytes: load(base_id)?,
                }),
            )
        };
        let (safe, explanation, diff_summary) = match preflight {
            crate::conflict_merge::MergePreflight::Safe {
                result,
                explanation,
            } => (
                true,
                explanation.to_string(),
                format!(
                    "Both immutable revisions are included in a {} byte proposed result.",
                    result.bytes.len()
                ),
            ),
            crate::conflict_merge::MergePreflight::NeedsReview { explanation, .. } => (
                false,
                explanation.to_string(),
                "No proposed content was produced; choose a version or keep both after reviewing the diff."
                    .to_string(),
            ),
        };
        let projection = fs_protocol::MergePreflightProjectionV1 {
            schema_version: fs_protocol::MERGE_PREFLIGHT_VERSION,
            conflict_id,
            safe,
            explanation,
            diff_summary,
        };
        projection.validate().map_err(|_| EIO)?;
        let bytes = serde_json::to_vec(&projection).map_err(|_| EIO)?;
        if bytes.len() > fs_protocol::MAX_MERGE_PREFLIGHT_BYTES {
            return Err(libc::E2BIG);
        }
        Ok(bytes)
    }

    /// Resolve an open signed conflict by copying one exact immutable member
    /// into a new, attributed result version. The request always names the
    /// member it saw: `KeepCurrent` is rejected if that member stopped being the
    /// current signed revision, and `UseOther` is rejected if it names current.
    /// This prevents a delayed UI request from being silently reinterpreted.
    // This is one ordered validation/publication/projection transaction.
    #[allow(
        clippy::too_many_lines,
        reason = "conflict resolution is one ordered validation, publication, and projection transaction"
    )]
    fn resolve_conflict(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        strategy: fs_protocol::ConflictStrategy,
        version_id: fs_protocol::FileVersionId,
    ) -> Result<(), i32> {
        self.check_access(ino, Access::Write)?;
        if self.write_buffers.lock_recover().contains_key(&ino) {
            return Err(EBUSY);
        }
        if self.is_document(ino) {
            return Err(libc::ENOTSUP);
        }
        if strategy == fs_protocol::ConflictStrategy::Merge {
            return self.resolve_conflict_merge(ino, conflict_id, version_id);
        }
        if strategy == fs_protocol::ConflictStrategy::KeepBoth {
            return self.resolve_conflict_keep_both(ino, conflict_id, version_id);
        }
        if !matches!(
            strategy,
            fs_protocol::ConflictStrategy::KeepCurrent | fs_protocol::ConflictStrategy::UseOther
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
        if conflict.status != fs_protocol::ConflictStatus::Open
            || !conflict.versions.contains(&version_id)
        {
            return Err(EINVAL);
        }
        match strategy {
            fs_protocol::ConflictStrategy::KeepCurrent
                if version_id != expected_revision.version_id =>
            {
                return Err(EBUSY);
            }
            fs_protocol::ConflictStrategy::UseOther
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
        // The selected member is already content-addressed: once it is
        // local, the result points at it by hash, without reading it back.
        self.history_content_local(ino, conflict_id, &[selected.content_hash])?;
        let byte_len = self.local_version_len(selected.content_hash).ok_or(EIO)?;
        let result_hash = selected.content_hash.map(|hash| Hash::from(hash.0));
        let result_snapshot = self
            .replacement_snapshot(ino, result_hash, byte_len)
            .ok_or(EIO)?;

        // The signed replica requires a resolution result to descend from
        // every member of the conflict. Preserve any additional current heads
        // as parents too so the resolution does not disconnect later work.
        let mut parents = state.heads;
        parents.extend(conflict.versions.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > fs_protocol::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let timestamp_ms = now_ms();
        let result_version = fs_protocol::FileVersionV1 {
            id: fs_protocol::FileVersionId::new(),
            record_id,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: result_snapshot.content_hash,
            metadata_hash: result_snapshot.metadata_hash,
            provenance: fs_protocol::FileProvenance::ConflictResolution,
        };
        let resolution = fs_protocol::ConflictResolution {
            strategy,
            result: result_version.id,
            actor: *actor,
            timestamp_ms,
        };
        let mut resolved = conflict;
        resolved.status = fs_protocol::ConflictStatus::Resolved;
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
        self.attribute_version(&result_version, None);
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
            let mut registry = registry.lock_recover();
            if let Some(mut record) = registry.get(record_id).cloned() {
                record.current_version = Some(converged.version_id);
                registry.put(record).map_err(|_| EIO)?;
            }
        }
        Ok(())
    }

    // Keep the exact two-record replacement transaction in one auditable path.
    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "the exact two-record merge replacement is one auditable signed transaction"
    )]
    fn resolve_conflict_merge(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        selected_version: fs_protocol::FileVersionId,
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
        if !matches!(extension.as_str(), "txt" | "qdoc" | "tilo-doc") {
            return Err(libc::ENOTSUP);
        }
        let state = history.snapshot(record_id).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .ok_or(EINVAL)?;
        let result_id = fs_protocol::FileVersionId(merge_uuid(
            b"result",
            publisher.workspace_id(),
            record_id,
            conflict_id,
            &conflict.versions,
            *actor,
        ));
        if let Some(resolution) = &conflict.resolution {
            return if resolution.strategy == fs_protocol::ConflictStrategy::Merge
                && resolution.result == result_id
                && current_revision.version_id == result_id
            {
                Ok(())
            } else {
                Err(EBUSY)
            };
        }
        if conflict.status != fs_protocol::ConflictStatus::Open
            || conflict.versions.len() != 2
            || !conflict.versions.contains(&selected_version)
            || !conflict.versions.contains(&current_revision.version_id)
            || selected_version == current_revision.version_id
        {
            return Err(EINVAL);
        }
        let content = |version_id| -> Result<Option<fs_protocol::ContentHash>, i32> {
            state
                .versions
                .iter()
                .find(|value| value.id == version_id)
                .map(|version| version.content_hash)
                .ok_or(EINVAL)
        };
        let first_id = *conflict.versions.first().ok_or(EINVAL)?;
        let second_id = *conflict.versions.get(1).ok_or(EINVAL)?;
        let document = matches!(extension.as_str(), "qdoc" | "tilo-doc");
        let base_id = if document {
            None
        } else {
            Some(
                crate::conflict_merge::exact_common_base(&state.versions, first_id, second_id)
                    .map_err(|_| libc::ENOTSUP)?,
            )
        };
        let mut inputs = vec![content(first_id)?, content(second_id)?];
        if let Some(base_id) = base_id {
            inputs.push(content(base_id)?);
        }
        self.history_content_local(ino, conflict_id, &inputs)?;
        // An input the merge would refuse for its size is not read into
        // memory at all.
        let load = |version_id| -> Result<Vec<u8>, i32> {
            let version = content(version_id)?;
            let len = self.local_version_len(version).ok_or(EIO)?;
            if usize::try_from(len)
                .ok()
                .is_none_or(|len| len > crate::conflict_merge::MAX_MERGE_INPUT_BYTES)
            {
                return Err(libc::ENOTSUP);
            }
            self.local_version_bytes(version).ok_or(EIO)
        };
        let first = crate::conflict_merge::ExactMergeInput {
            version_id: first_id,
            bytes: load(first_id)?,
        };
        let second = crate::conflict_merge::ExactMergeInput {
            version_id: second_id,
            bytes: load(second_id)?,
        };
        let merged = if let Some(base_id) = base_id {
            let preflight = crate::conflict_merge::preflight_exact(
                first,
                second,
                Some(crate::conflict_merge::ExactMergeBase {
                    version_id: base_id,
                    bytes: load(base_id)?,
                }),
            );
            let crate::conflict_merge::MergePreflight::Safe { result, .. } = preflight else {
                return Err(libc::ENOTSUP);
            };
            (result.codec == crate::conflict_merge::DeterministicMergeCodec::PlainUtf8ThreeWayV1)
                .then_some(result)
                .ok_or(libc::ENOTSUP)?
        } else {
            let preflight = crate::conflict_merge::preflight_exact(first, second, None);
            let crate::conflict_merge::MergePreflight::Safe { result, .. } = preflight else {
                // A failed preflight is deliberately indistinguishable from
                // an unsupported merge at the FUSE boundary: the open signed
                // conflict remains untouched for Keep Both / manual review.
                return Err(libc::ENOTSUP);
            };
            (result.codec == crate::conflict_merge::DeterministicMergeCodec::TiloDocAutomergeV1)
                .then_some(result)
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
        if parents.len() > fs_protocol::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let proposed_version = fs_protocol::FileVersionV1 {
            id: result_id,
            record_id,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: snapshot.content_hash,
            metadata_hash: snapshot.metadata_hash,
            provenance: fs_protocol::FileProvenance::ConflictResolution,
        };
        let mut participant = fs_protocol::FileMutationParticipant {
            record_id,
            role: fs_protocol::FileMutationParticipantRole::MergeSource,
            expected_version: current_revision.version_id,
            proposed_binding: fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id,
                parent: Some(parent_record),
                name: source_name.clone(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id,
                version_id: result_id,
                content_hash: snapshot.content_hash,
                byte_len: snapshot.byte_len,
                metadata_hash: snapshot.metadata_hash,
            }),
            proposed_version,
            projection_hash: fs_protocol::ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().map_err(|_| EIO)?;
        let codec = match merged.codec {
            crate::conflict_merge::DeterministicMergeCodec::TiloDocAutomergeV1 => {
                fs_protocol::ConflictMergeCodecV1::TiloDocAutomergeV1
            }
            crate::conflict_merge::DeterministicMergeCodec::PlainUtf8ThreeWayV1 => {
                fs_protocol::ConflictMergeCodecV1::PlainUtf8ThreeWayV1
            }
        };
        let manifest = fs_protocol::FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
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
            operation: fs_protocol::FileMutationOperation::MergeContentConflict {
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
        // Who caused these versions, when an agent did, is recorded once
        // the transaction committed them.
        let attributed: Vec<_> = manifest
            .participants
            .iter()
            .map(|participant| participant.proposed_version.clone())
            .collect();
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut runtime.journal.lock_recover(),
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
        self.attribute_versions(&attributed, None);
        Ok(())
    }

    // Keep the exact two-record replacement transaction in one auditable path.
    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "the exact keep-both replacement is one auditable signed transaction"
    )]
    fn resolve_conflict_keep_both(
        &self,
        ino: u64,
        conflict_id: uuid::Uuid,
        selected_version: fs_protocol::FileVersionId,
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
        let target_record = fs_protocol::FileRecordId(derive(b"target-record"));
        let target_version_id = fs_protocol::FileVersionId(derive(b"target-version"));
        let result_version_id = fs_protocol::FileVersionId(derive(b"source-result"));
        let transaction_id = derive(b"transaction");
        let state = history.snapshot(source_record).ok_or(EINVAL)?;
        let conflict = state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == conflict_id)
            .ok_or(EINVAL)?;
        if let Some(resolution) = &conflict.resolution {
            return if resolution.strategy == fs_protocol::ConflictStrategy::KeepBoth
                && resolution.result == result_version_id
                && self.ns().ensure_ino_for_record_id(target_record).is_some()
            {
                Ok(())
            } else {
                Err(EBUSY)
            };
        }
        if conflict.status != fs_protocol::ConflictStatus::Open
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
        // The sibling points at the selected member by hash; only its length
        // is needed, once its bytes are local.
        self.history_content_local(ino, conflict_id, &[selected.content_hash])?;
        let selected_len = self.local_version_len(selected.content_hash).ok_or(EIO)?;
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
            Ok(fs_protocol::ContentHash(*blake3::hash(&bytes).as_bytes()))
        };
        let source_metadata_hash = metadata_hash(source_path, current_revision.byte_len)?;
        let target_metadata_hash = metadata_hash(target_path, selected_len)?;
        let mut parents = state.heads.clone();
        parents.extend(conflict.versions.iter().copied());
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > fs_protocol::files::MAX_VERSION_PARENTS {
            return Err(libc::ENOTSUP);
        }
        let source_version = fs_protocol::FileVersionV1 {
            id: result_version_id,
            record_id: source_record,
            parents,
            actor: *actor,
            timestamp_ms,
            content_hash: current.content_hash,
            metadata_hash: source_metadata_hash,
            provenance: fs_protocol::FileProvenance::ConflictResolution,
        };
        let target_version = fs_protocol::FileVersionV1 {
            id: target_version_id,
            record_id: target_record,
            parents: Vec::new(),
            actor: *actor,
            timestamp_ms,
            content_hash: selected.content_hash,
            metadata_hash: target_metadata_hash,
            provenance: fs_protocol::FileProvenance::Create,
        };
        let mut source_participant = fs_protocol::FileMutationParticipant {
            record_id: source_record,
            role: fs_protocol::FileMutationParticipantRole::KeepBothSource,
            expected_version: current_revision.version_id,
            proposed_binding: fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: source_record,
                parent: Some(parent_record),
                name: source_name.clone(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id: source_record,
                version_id: result_version_id,
                content_hash: current.content_hash,
                byte_len: current_revision.byte_len,
                metadata_hash: source_metadata_hash,
            }),
            proposed_version: source_version,
            projection_hash: fs_protocol::ContentHash([0; 32]),
        };
        source_participant.projection_hash = source_participant
            .computed_projection_hash()
            .map_err(|_| EIO)?;
        let mut sibling_participant = fs_protocol::FileMutationParticipant {
            record_id: target_record,
            role: fs_protocol::FileMutationParticipantRole::KeepBothSibling,
            expected_version: selected_version,
            proposed_binding: fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: target_record,
                parent: Some(parent_record),
                name: target_name.clone(),
                kind: fs_protocol::FileNodeKind::File,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            },
            proposed_content_revision: Some(fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id: target_record,
                version_id: target_version_id,
                content_hash: selected.content_hash,
                byte_len: selected_len,
                metadata_hash: target_metadata_hash,
            }),
            proposed_version: target_version,
            projection_hash: fs_protocol::ContentHash([0; 32]),
        };
        sibling_participant.projection_hash = sibling_participant
            .computed_projection_hash()
            .map_err(|_| EIO)?;
        let mut participants = vec![source_participant, sibling_participant];
        participants.sort_by_key(|participant| participant.record_id);
        let manifest = fs_protocol::FileMutationTransactionManifest {
            schema_version: fs_protocol::files::FILE_MUTATION_TRANSACTION_VERSION_V2,
            transaction_id,
            workspace_id: publisher.workspace_id(),
            actor: *actor,
            timestamp_ms,
            operation: fs_protocol::FileMutationOperation::KeepBothContentConflict {
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
        // Who caused these versions, when an agent did, is recorded once
        // the transaction committed them.
        let attributed: Vec<_> = manifest
            .participants
            .iter()
            .map(|participant| participant.proposed_version.clone())
            .collect();
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut transaction_runtime.journal.lock_recover(),
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
            .map_err(|_| EIO)?;
        self.attribute_versions(&attributed, None);
        Ok(())
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "namespace and availability snapshots must describe one coherent record revision"
    )]
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
            let ancestry = record_ancestry(&mut ns, ino)?;
            (record_id, kind, parent, attr.size, ancestry)
        };
        let (mut record, effective_policy) = {
            let registry = registry.lock_recover();
            let record = registry
                .get(record_id)
                .cloned()
                .unwrap_or_else(|| RecordAvailability::new(record_id, kind));
            (record, inherited_policy(&registry, &ancestry))
        };
        record.parent = parent;
        match self.sync_state_for(ino)? {
            fs_protocol::SyncState::Local => {
                record.residency = Residency::Resident { bytes: size };
                record.transfer.phase = TransferPhase::Idle;
            }
            fs_protocol::SyncState::Projected => {
                record.residency = Residency::NotResident;
                record.transfer.phase = TransferPhase::Idle;
            }
            fs_protocol::SyncState::Downloading => {
                record.transfer.phase = TransferPhase::Downloading;
            }
        }
        record
            .projection_with_policy(effective_policy)
            .to_xattr_bytes()
            .ok()
    }

    fn set_pin_policy(&self, ino: u64, policy: AvailabilityPolicy) -> Result<(), i32> {
        self.check_access(ino, Access::Read)?;
        let registry = self.availability.as_ref().ok_or(libc::ENOTSUP)?;
        let mut ancestry = {
            let mut ns = self.ns();
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
            drop(ns);
            ancestry
        };
        // From the workspace root down to the record itself.
        ancestry.reverse();
        registry
            .lock_recover()
            .set_policy_along(&ancestry, policy)
            .map_err(|_| EIO)?;
        if let Some(worker) = &self.pinned_hydration {
            worker
                .wake
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        Ok(())
    }

    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "retrying a transfer keeps the persisted availability transition, authorization, hydration dispatch, and error rollback in one auditable operation"
    )]
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
            let mut registry = registry.lock_recover();
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
            record.transfer.progress = Some(fs_protocol::TransferProgress {
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
                .prefetch_with_progress(
                    hash,
                    size,
                    crate::hydration::Priority::Pinned,
                    |completed_bytes, total_bytes| {
                        if completed_bytes != total_bytes
                            && completed_bytes.saturating_sub(last_persisted)
                                < HYDRATION_PROGRESS_PERSIST_BYTES
                        {
                            return;
                        }
                        last_persisted = completed_bytes;
                        update_retry_state(&registry, record_id, |record| {
                            record.transfer.phase = TransferPhase::Downloading;
                            record.transfer.progress = Some(fs_protocol::TransferProgress {
                                completed_bytes,
                                total_bytes: Some(total_bytes),
                            });
                        });
                    },
                )
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
                        message: fs_protocol::NO_PEER_DELIVERED_ERROR.into(),
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

    fn prepare_eviction_request(
        &self,
        record_id: fs_protocol::FileRecordId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
    ) -> Result<EvictionRequestId, i32> {
        let coordinator = self.eviction_coordinator()?;
        self.hydrator.block_on(async {
            coordinator
                .prepare(record_id, mode, existing_request, None)
                .await
                .map(|(request_id, _)| request_id)
                .map_err(|_| EBUSY)
        })
    }

    fn spawn_eviction_observer(
        &self,
        record_id: fs_protocol::FileRecordId,
        request_id: EvictionRequestId,
    ) -> Result<(), i32> {
        self.eviction_coordinator()?
            .spawn_observer(record_id, request_id);
        Ok(())
    }

    fn eviction_coordinator(&self) -> Result<crate::eviction::EvictionCoordinator, i32> {
        Ok(crate::eviction::EvictionCoordinator::new(
            Arc::clone(&self.hydrator),
            self.availability.as_ref().ok_or(libc::ENOTSUP)?.clone(),
            self.eviction_queue.as_ref().ok_or(libc::ENOTSUP)?.clone(),
            self.history
                .as_ref()
                .map(|(history, _)| Arc::clone(history)),
        ))
    }

    /// Resume durable cache-reclamation intents after a daemon restart. Every
    /// pending request is freshly reauthorized against current policy/history
    /// before owned tags are released again; completed observations repair a
    /// registry update that may have been interrupted by a crash.
    pub fn resume_pending_evictions(&self) {
        let Some(queue) = &self.eviction_queue else {
            return;
        };
        let queue = queue.clone();
        let requests = self
            .hydrator
            .block_on(async { queue.lock().await.requests().cloned().collect::<Vec<_>>() });
        for request in requests {
            if matches!(request.state, EvictionRequestState::Completed { .. }) {
                // Reuse the exact-version guard. A completed old-version
                // request must not erase residency for a newer current file.
                if let Ok(coordinator) = self.eviction_coordinator() {
                    let _ = self
                        .hydrator
                        .block_on(coordinator.observe_once(request.record_id, request.id));
                }
                continue;
            }
            // Settings persists an intent before asking the backend GC to
            // collect it.  Availability/history can lag a newly applied
            // namespace revision, so do not let a later restart turn an old
            // intent into authority for the current file.
            if !self.eviction_request_matches_live_namespace(&request) {
                eprintln!(
                    "pending eviction no longer matches the live namespace version; leaving it unexecuted"
                );
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

    fn eviction_request_matches_live_namespace(
        &self,
        request: &crate::eviction::EvictionRequest,
    ) -> bool {
        let namespace = self.ns();
        namespace.live_node_ids().into_iter().any(|node_id| {
            namespace.record_id_for_id(&node_id) == Some(request.record_id)
                && namespace
                    .content_revision_for_id(&node_id)
                    .is_some_and(|revision| revision.version_id == request.version_id)
        })
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
                    if revision.content_hash != Some(fs_protocol::ContentHash(ch)) {
                        return None;
                    }
                    let mut metadata = snapshot.metadata?;
                    metadata.previews.retain(|preview| preview.max_edge != 256);
                    metadata.previews.push(fs_protocol::PreviewVariant {
                        max_edge: 256,
                        hash: Some(fs_protocol::ContentHash(*tbh.as_bytes())),
                        status: fs_protocol::PreviewStatus::Ready,
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
                        .filter(|preview| preview.status == fs_protocol::PreviewStatus::Ready)
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
        self.hydrator
            .block_on(self.hydrator.read_local(hash, 0, u64::MAX))
    }

    /// Read a raw blob (the thumbnail), serving it locally or hydrating it from
    /// a peer — the small transfer that lets a preview arrive before content.
    fn read_blob_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        self.hydrator
            .block_on(self.hydrator.read_blob(hash, 0, u64::MAX))
            .ok()
    }

    /// Ingest the thumbnail PNG as a content-addressed blob, returning its hash.
    fn ingest_blob_blocking(&self, png: &[u8]) -> Option<Hash> {
        self.hydrator
            .block_on(self.hydrator.store.ingest(png.to_vec()))
            .ok()
    }

    /// Bind this mount to a workspace ACL, enforcing per-identity access for
    /// `identity`. Every subsequent open/read/write is checked against `acl`
    /// as `identity`; without this the mount keeps Unix-veneer-only
    /// behaviour. Takes anything that becomes an [`AclHandle`]: a
    /// [`WorkspaceAcl`](crate::acl::WorkspaceAcl) for a policy that won't change, or a handle held by
    /// a [`crate::CapabilityFile`] for one that reloads under the live mount.
    #[must_use]
    pub fn with_acl(
        mut self,
        acl: impl Into<AclHandle>,
        identity: crate::acl::IdentityKey,
    ) -> Self {
        self.enforcement = Some((acl.into(), identity));
        // Subscribe before the initial snapshot. A live ACL/sync mutation in
        // between these operations then has a receiver to wake, rather than
        // becoming an initial watch value that no `.changed()` can observe.
        self.start_record_index_acl_worker();
        self.start_record_index_sync_worker();
        // Build the authority view once when the mount is bound. Thereafter
        // ordinary v3 manifest/page requests use only this retained snapshot.
        self.refresh_acl_visible_record_index();
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

    /// [`Self::check_access`] of an inode already read by
    /// [`Self::inode_view`], so the check takes no namespace lock of its own.
    fn check_view_access(&self, view: &InodeView, access: Access) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        self.check_path(view.path.as_deref().ok_or(ENOENT)?, access)
    }

    /// [`Self::ino_in_view`] of an inode already read by [`Self::inode_view`].
    fn view_in_view(&self, view: &InodeView) -> bool {
        self.enforcement.is_none() || view.path.as_deref().is_some_and(|path| self.in_view(path))
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
        // The result is ENOENT.
        Err(ENOENT)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the open-handle snapshot must remain stable across authorization and inode validation"
    )]
    fn check_open_handle_access(
        &self,
        ino: u64,
        fh: FileHandle,
        access: Access,
    ) -> Result<(), i32> {
        if self.ns().getattr(ino).is_some() {
            return self.check_access(ino, access);
        }
        let handles = self.open_file_handles.lock_recover();
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
    fn visible_entries(&self, ino: u64) -> Option<Vec<(u64, FileType, fs_protocol::FileNameV1)>> {
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

    /// Hand what an authorised open of `ino` predicts to the predictive
    /// worker. Runs after the open is answered, so prediction never delays
    /// it.
    fn predict_after_open(&self, ino: u64) {
        let candidates = self.predictive_candidates(ino);
        self.heuristics.on_open(ino, candidates);
    }

    /// What an open of `ino` predicts, as `(content id, declared size)`
    /// pairs to hydrate, and the open recorded in the mount's access
    /// history: the visible siblings of the same extension ahead of it in
    /// natural order, as far ahead as the run of in-order opens it
    /// continues earns, then files opened together with it before.
    ///
    /// Siblings come from `visible_entries`, and every co-access partner
    /// passes the same read check an open does: the raw namespace would
    /// fetch content the mount's identity is not authorised to see.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the namespace guard protects one ACL-filtered sibling snapshot used for prediction"
    )]
    fn predictive_candidates(&self, ino: u64) -> Vec<(Hash, u64)> {
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
        let extension_of = |name: &[u8]| {
            std::path::Path::new(OsStr::from_bytes(name))
                .extension()
                .and_then(OsStr::to_str)
                .map(str::to_ascii_lowercase)
        };
        let extension = extension_of(current_name.as_bytes());
        let Some(entries) = self.visible_entries(parent) else {
            return Vec::new();
        };
        if !entries.iter().any(|(entry, _, _)| *entry == ino) {
            return Vec::new();
        }
        let mut siblings: Vec<(u64, fs_protocol::FileNameV1)> = entries
            .into_iter()
            .filter(|(entry, kind, name)| {
                *entry == ino
                    || (*kind == FileType::RegularFile
                        && extension_of(name.as_bytes()) == extension)
            })
            .map(|(entry, _, name)| (entry, name))
            .collect();
        siblings.sort_by(|(_, a), (_, b)| natural_cmp(a.as_bytes(), b.as_bytes()));
        let order: Vec<u64> = siblings.into_iter().map(|(entry, _)| entry).collect();
        // Nothing is learned while the person has turned prefetching off.
        let learn = self.heuristics.prefetch_enabled();
        let (prediction, changed) = {
            let mut history = self.access_history.lock_recover();
            let before = history.revision();
            if learn {
                history.record(parent, ino, std::time::Instant::now());
            }
            (
                history.predict(parent, ino, &order),
                history.revision() != before,
            )
        };
        if changed && let Some(kept) = &self.kept_history {
            // Only wakes the keeper: the request path never writes.
            kept.changed();
        }
        let together: Vec<u64> = prediction
            .together
            .into_iter()
            .filter(|partner| self.check_access(*partner, Access::Read).is_ok())
            .collect();
        let ns = self.ns();
        prediction
            .siblings
            .into_iter()
            .chain(together)
            .filter_map(|candidate| {
                let attr = ns.getattr(candidate)?;
                if attr.kind != FileType::RegularFile {
                    return None;
                }
                Some((ns.hash_for(candidate)?, attr.size))
            })
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
        {
            let mut deferred = self.deferred_deltas.lock_recover();
            if let Some(deltas) = deferred.as_mut() {
                // Peers learn a changeset's placements only once its records
                // are durable; the visible index is refreshed once for the
                // whole changeset then.
                deltas.push(delta);
                return;
            }
        }
        if self.enforcement.is_some() {
            // Local structural changes have already updated Namespace's
            // incremental record index. Refresh the retained ACL projection
            // before advertising the mutation, so a completed pager's next
            // poll observes a visible addition without a request-time scan.
            self.refresh_acl_visible_record_index();
        }
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
        self.store_version_blocking(VersionSource::Bytes(data), None)
            .map(|stored| stored.map(|(hash, _)| hash))
    }

    /// Stores one version's content and returns its content id and length
    /// (`None` when empty). `previous` is the file's current content, whose
    /// chunk tier the new version keeps.
    fn store_version_blocking(
        &self,
        source: VersionSource<'_>,
        previous: Option<Hash>,
    ) -> io::Result<Option<(Hash, u64)>> {
        let store = &self.hydrator.store;
        let stored = match source {
            VersionSource::Bytes(data) => {
                let len = data.len() as u64;
                self.hydrator
                    .block_on(store.write_content_after(data, previous))
                    .map(|hash| hash.map(|hash| (hash, len)))
            }
            VersionSource::File { path, manifest } => self
                .hydrator
                .block_on(store.publish_file(path, manifest, previous)),
        };
        stored.map_err(|e| io::Error::other(e.to_string()))
    }

    /// Current full content of an inode: its stage if any, else the
    /// document export or the hydrated blob. `Ok(empty)` only for an
    /// empty/hashless file; a hydration failure is `Err`.
    fn read_full_blocking(&self, ino: u64) -> io::Result<Vec<u8>> {
        if let Some(stage) = self.stage(ino) {
            return self.read_stage_blocking(&stage, 0, u64::MAX);
        }
        if self.is_document(ino) {
            return self
                .document_export(ino)
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| io::Error::other("document export failed"));
        }
        let hash = self.ns().hash_for(ino);
        let Some(hash) = hash else {
            return Ok(Vec::new());
        };
        self.hydrator
            .block_on(self.hydrator.read(hash, 0, u64::MAX))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Ingest `data` as the new content of `ino` and broadcast the change.
    /// Aborts WITHOUT mutating the entry if ingest fails, preserving the
    /// previously-committed content.
    fn commit_content(&self, ino: u64, data: &[u8]) -> io::Result<()> {
        self.commit_content_at(ino, data, now_ms())
    }

    /// [`Self::commit_content`] with the version's modification time given by
    /// the caller: the native data plane publishes the time the file was
    /// written, which peers then materialise.
    fn commit_content_at(&self, ino: u64, data: &[u8], mtime_ms: u64) -> io::Result<()> {
        self.commit_version_at(ino, VersionSource::Bytes(data), mtime_ms, None)
    }

    /// [`Self::commit_content_at`] with the version's content from
    /// `source`, and for a version written under a write lease its tenancy
    /// (`fence`), published in the same namespace change as the content.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "content publication and namespace replacement must preserve the previous committed revision on failure"
    )]
    fn commit_version_at(
        &self,
        ino: u64,
        source: VersionSource<'_>,
        mtime_ms: u64,
        fence: Option<crate::namespace::VersionFence>,
    ) -> io::Result<()> {
        #[cfg(test)]
        if let Some((fault_ino, errno)) = *self.commit_fault.lock_recover()
            && fault_ino == ino
        {
            return Err(io::Error::from_raw_os_error(errno));
        }
        let previous = self.ns().hash_for(ino);
        let (hash, byte_len) = match self.store_version_blocking(source, previous)? {
            Some((hash, len)) => (Some(hash), len),
            None => (None, 0),
        };
        let (expected_hash, expected_len, stamp) = {
            let ns = self.ns();
            let expected_hash = ns.hash_for(ino);
            let attr = ns
                .getattr(ino)
                .ok_or_else(|| io::Error::from_raw_os_error(ENOENT))?;
            if expected_hash == hash && attr.size == byte_len {
                return Ok(());
            }
            // Signed metadata times never go backwards; a file given an
            // older mtime than its current version keeps the current one.
            let current_ms = attr
                .mtime
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            (expected_hash, attr.size, mtime_ms.max(current_ms))
        };
        let snapshot = self
            .replacement_snapshot(ino, hash, byte_len)
            .ok_or_else(|| io::Error::from_raw_os_error(ENOENT))?;
        self.append_version_at(snapshot, fs_protocol::FileProvenance::Edit, now_ms(), stamp)
            .map_err(|error| match error {
                crate::history::HistoryError::CapacityExhausted(detail) => {
                    eprintln!("inode {ino}: {detail}");
                    io::Error::from_raw_os_error(EDQUOT)
                }
                error => io::Error::other(error.to_string()),
            })?;
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
            let current_len = ns.getattr(ino).map(|attr| attr.size);
            let installed = ns.hash_for(ino) == hash && current_len == Some(byte_len);
            if !installed
                && (ns.hash_for(ino) != expected_hash || current_len != Some(expected_len))
            {
                return Err(io::Error::from_raw_os_error(EBUSY));
            }
            if let Some(fence) = fence
                && let Some(record) = ns.record_id_for(ino)
                && !ns.put_version_fence(record, hash.map(|h| *h.as_bytes()), byte_len, fence)
            {
                return Err(io::Error::other("could not tag the version with its lease"));
            }
            if let Some(revision) = &revision {
                if !ns.set_content_revision(ino, revision, stamp) {
                    return Err(io::Error::other(
                        "could not install signed content revision",
                    ));
                }
            } else {
                ns.set_content(ino, hash.as_ref(), byte_len, stamp);
            }
            ns.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    /// Create an empty regular file. Signed mounts publish the immutable
    /// baseline before exposing the move-tree node; a failed publication can
    /// leave an unreachable repository record, but never an unsigned live file.
    fn create_empty_file(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
    ) -> Result<(u64, Vec<u8>), i32> {
        self.create_empty_file_stamped(parent, name, now_ms())
    }

    /// [`Self::create_empty_file`] whose binding and metadata are stamped at
    /// `observed_at_ms` (the file's own time) rather than now.
    fn create_empty_file_stamped(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        observed_at_ms: u64,
    ) -> Result<(u64, Vec<u8>), i32> {
        self.create_file_stamped(parent, name, observed_at_ms, None, 0)
    }

    /// Create a regular file whose first version already has `content`
    /// (`byte_len` bytes): one signed version, content revision and
    /// metadata, instead of an empty file followed by an edit.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "signed baseline publication must complete before the new file becomes visible in the namespace"
    )]
    fn create_file_stamped(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        observed_at_ms: u64,
        content: Option<Hash>,
        byte_len: u64,
    ) -> Result<(u64, Vec<u8>), i32> {
        let name: &[u8] = name.as_ref();
        let byte_len = if content.is_some() { byte_len } else { 0 };
        let content_hash = content.map(|hash| fs_protocol::ContentHash(*hash.as_bytes()));
        let timestamp_ms = now_ms();
        if let (Some(publisher), Some((history, actor))) = (&self.record_publisher, &self.history) {
            let path = {
                let mut namespace = self.ns();
                if namespace.lookup_kind(parent, name).is_some() {
                    return Err(EEXIST);
                }
                let parent_path = namespace.path_for(parent).ok_or(ENOENT)?;
                child_path(&parent_path, name)
            };
            let record_id = fs_protocol::FileRecordId::new();
            let metadata_digest = VersionMetadataDigest {
                path: Some(path.clone()),
                kind: 2,
                byte_len,
                deleted: false,
            };
            let metadata_hash = fs_protocol::ContentHash(
                *blake3::hash(&postcard::to_allocvec(&metadata_digest).map_err(|_| EIO)?)
                    .as_bytes(),
            );
            let version = fs_protocol::FileVersionV1 {
                id: fs_protocol::FileVersionId::new(),
                record_id,
                parents: Vec::new(),
                actor: *actor,
                timestamp_ms,
                content_hash,
                metadata_hash,
                provenance: fs_protocol::FileProvenance::Create,
            };
            let snapshot = VersionSnapshot {
                record_id,
                content_hash,
                metadata_hash,
                byte_len,
                kind: FileType::RegularFile,
            };
            let parent_record = self.ns().record_id_for(parent).ok_or(EIO)?;
            publisher
                .publish_binding(&fs_protocol::FileBindingV1 {
                    schema_version: fs_protocol::FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent_record),
                    name: name.to_vec().into(),
                    kind: fs_protocol::FileNodeKind::File,
                    tombstoned: false,
                    actor: *actor,
                    observed_at_ms,
                })
                .map_err(|_| EIO)?;
            publisher
                .publish(
                    &self.record_metadata(&snapshot, observed_at_ms, *actor),
                    std::slice::from_ref(&version),
                )
                .map_err(|_| EIO)?;
            self.attribute_version(&version, Some(&path));
            history.append(version).map_err(|_| EIO)?;
            let revision = publisher
                .snapshot(record_id)
                .map_err(|_| EIO)?
                .and_then(|snapshot| snapshot.content_revision)
                .ok_or(EIO)?;
            let mut namespace = self.ns();
            if namespace.lookup_kind(parent, name).is_some() {
                return Err(EEXIST);
            }
            let ino = if content.is_some() {
                namespace
                    .create_projected_file_at_revision(parent, name, &revision, observed_at_ms)
                    .ok_or(EIO)?
            } else {
                let ino = namespace
                    .create_projected_file(parent, name, record_id, None, 0, observed_at_ms)
                    .ok_or(EIO)?;
                if !namespace.install_verified_content_revision(revision) {
                    let _ = namespace.remove(ino);
                    return Err(EIO);
                }
                ino
            };
            return Ok((ino, namespace.save_incremental()));
        }

        let mut namespace = self.ns();
        let ino = namespace
            .create_file(parent, name, content.as_ref(), byte_len, observed_at_ms)
            .ok_or(ENOENT)?;
        Ok((ino, namespace.save_incremental()))
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "directory creation publishes one namespace mutation and its incremental document snapshot"
    )]
    fn create_empty_dir(&self, parent: u64, name: impl AsRef<[u8]>) -> Result<(u64, Vec<u8>), i32> {
        let name: &[u8] = name.as_ref();
        let timestamp_ms = now_ms();
        if let (Some(publisher), Some((history, actor))) = (&self.record_publisher, &self.history) {
            let (parent_record, path) = {
                let mut namespace = self.ns();
                if namespace.lookup_kind(parent, name).is_some() {
                    return Err(EEXIST);
                }
                let parent_record = namespace.record_id_for(parent).ok_or(ENOENT)?;
                let parent_path = namespace.path_for(parent).ok_or(ENOENT)?;
                (parent_record, child_path(&parent_path, name))
            };
            let record_id = fs_protocol::FileRecordId::new();
            let metadata_digest = VersionMetadataDigest {
                path: Some(path.clone()),
                kind: 1,
                byte_len: 0,
                deleted: false,
            };
            let metadata_hash = fs_protocol::ContentHash(
                *blake3::hash(&postcard::to_allocvec(&metadata_digest).map_err(|_| EIO)?)
                    .as_bytes(),
            );
            let version = fs_protocol::FileVersionV1 {
                id: fs_protocol::FileVersionId::new(),
                record_id,
                parents: Vec::new(),
                actor: *actor,
                timestamp_ms,
                content_hash: None,
                metadata_hash,
                provenance: fs_protocol::FileProvenance::Create,
            };
            publisher
                .publish_binding(&fs_protocol::FileBindingV1 {
                    schema_version: fs_protocol::FILE_BINDING_VERSION,
                    record_id,
                    parent: Some(parent_record),
                    name: name.to_vec().into(),
                    kind: fs_protocol::FileNodeKind::Directory,
                    tombstoned: false,
                    actor: *actor,
                    observed_at_ms: timestamp_ms,
                })
                .map_err(|_| EIO)?;
            publisher
                .publish_structural_version(&version)
                .map_err(|_| EIO)?;
            self.attribute_version(&version, Some(&path));
            history.append(version).map_err(|_| EIO)?;
            let mut namespace = self.ns();
            if namespace.lookup_kind(parent, name).is_some() {
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
                fs_protocol::FileProvenance::Delete,
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
        clippy::too_many_lines,
        reason = "the explicit authenticated rename facts feed one atomic signed replacement transaction"
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
                metadata_hash: fs_protocol::ContentHash(
                    *blake3::hash(&postcard::to_allocvec(&source_metadata).map_err(|_| EIO)?)
                        .as_bytes(),
                ),
                ..source_current
            };
            let victim_proposed = VersionSnapshot {
                metadata_hash: fs_protocol::ContentHash(
                    *blake3::hash(&postcard::to_allocvec(&victim_metadata).map_err(|_| EIO)?)
                        .as_bytes(),
                ),
                ..victim_current
            };
            let node_kind = if source_attr.kind == FileType::Directory {
                fs_protocol::FileNodeKind::Directory
            } else {
                fs_protocol::FileNodeKind::File
            };
            let source_binding = fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
                record_id: source_current.record_id,
                parent: Some(destination_parent_record),
                name: new_name.to_vec().into(),
                kind: node_kind,
                tombstoned: false,
                actor: *actor,
                observed_at_ms: timestamp_ms,
            };
            let victim_binding = fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
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
                           binding: fs_protocol::FileBindingV1,
                           expected: fs_protocol::FileVersionId,
                           role: fs_protocol::FileMutationParticipantRole,
                           provenance: fs_protocol::FileProvenance| {
            let version = fs_protocol::FileVersionV1 {
                id: fs_protocol::FileVersionId::new(),
                record_id: snapshot.record_id,
                parents: vec![expected],
                actor: *actor,
                timestamp_ms,
                content_hash: snapshot.content_hash,
                metadata_hash: snapshot.metadata_hash,
                provenance,
            };
            let proposed_content_revision = (snapshot.kind == FileType::RegularFile).then_some(
                fs_protocol::ContentRevisionV1 {
                    schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                    record_id: snapshot.record_id,
                    version_id: version.id,
                    content_hash: snapshot.content_hash,
                    byte_len: snapshot.byte_len,
                    metadata_hash: snapshot.metadata_hash,
                },
            );
            let mut value = fs_protocol::FileMutationParticipant {
                record_id: snapshot.record_id,
                role,
                expected_version: expected,
                proposed_binding: binding,
                proposed_version: version,
                proposed_content_revision,
                projection_hash: fs_protocol::ContentHash([0; 32]),
            };
            value.projection_hash = value.computed_projection_hash().map_err(|_| EIO)?;
            Ok::<_, i32>(value)
        };
        let mut participants = vec![
            participant(
                source_proposed,
                source_binding,
                *source_expected,
                fs_protocol::FileMutationParticipantRole::Source,
                if parent == new_parent {
                    fs_protocol::FileProvenance::Rename
                } else {
                    fs_protocol::FileProvenance::Move
                },
            )?,
            participant(
                victim_proposed,
                victim_binding,
                *victim_expected,
                fs_protocol::FileMutationParticipantRole::Replaced,
                fs_protocol::FileProvenance::Delete,
            )?,
        ];
        participants.sort_by_key(|value| value.record_id);
        let manifest = fs_protocol::FileMutationTransactionManifest {
            schema_version: fs_protocol::FILE_MUTATION_TRANSACTION_VERSION,
            transaction_id: uuid::Uuid::new_v4(),
            workspace_id: publisher.workspace_id(),
            actor: *actor,
            timestamp_ms,
            operation: fs_protocol::FileMutationOperation::ReplacementRename {
                source_parent: source_parent_record,
                source_name: name.to_vec().into(),
                destination_parent: destination_parent_record,
                destination_name: new_name.to_vec().into(),
                source_record: source_current.record_id,
                replaced_record: victim_current.record_id,
            },
            participants,
        };
        // Who caused these versions, when an agent did, is recorded once
        // the transaction committed them.
        let attributed: Vec<_> = manifest
            .participants
            .iter()
            .map(|participant| participant.proposed_version.clone())
            .collect();
        // Both versions now name the destination: the moved record, and the
        // one it replaced.
        let destination = self
            .attributing()
            .then(|| {
                self.ns()
                    .path_for(new_parent)
                    .map(|dir| child_path(&dir, new_name))
            })
            .flatten();
        let sync = self.sync.clone();
        publisher
            .execute_replacement_transaction(
                manifest,
                &mut runtime.journal.lock_recover(),
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
        self.attribute_versions(&attributed, destination.as_deref());
        Ok(Vec::new())
    }

    // Keep signed preflight, publication, and namespace projection ordered.
    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "signed rename preflight, publication, and namespace projection must remain ordered"
    )]
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
                    fs_protocol::FileProvenance::Rename
                } else {
                    fs_protocol::FileProvenance::Move
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
                metadata_hash: fs_protocol::ContentHash(*blake3::hash(&bytes).as_bytes()),
                ..expected
            };
            let parent_record = namespace.record_id_for(new_parent).ok_or(EIO)?;
            let actor = self.history.as_ref().ok_or(EIO)?.1;
            let kind = match attr.kind {
                FileType::Directory => fs_protocol::FileNodeKind::Directory,
                FileType::RegularFile => fs_protocol::FileNodeKind::File,
                _ => return Err(EINVAL),
            };
            let binding = fs_protocol::FileBindingV1 {
                schema_version: fs_protocol::FILE_BINDING_VERSION,
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
                fs_protocol::FileProvenance::Rename
            } else {
                fs_protocol::FileProvenance::Move
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

fn update_retry_state(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    record_id: fs_protocol::FileRecordId,
    update: impl FnOnce(&mut RecordAvailability),
) {
    let mut registry = registry.lock_recover();
    let Some(mut record) = registry.get(record_id).cloned() else {
        return;
    };
    update(&mut record);
    if let Err(error) = registry.put(record) {
        eprintln!("could not persist hydration retry state: {error}");
    }
}

/// Every live node the mount may see, parents before descendants. The
/// Namespace lock covers only reading the nodes; the ACL is evaluated after
/// it is released. Blocking: run it off the async runtime's workers.
fn pinned_candidates(
    namespace: &Arc<Mutex<Namespace>>,
    enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
) -> Vec<PinnedCandidate> {
    let mut candidates = Vec::new();
    {
        let namespace = namespace.lock_recover();
        if let Some(record_id) = namespace.record_id_for(crate::namespace::ROOT_INO) {
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
    }
    // Fail closed: nothing outside this mount's current view is registered
    // or hydrated.
    if let Some((acl, identity)) = enforcement {
        candidates.retain(|candidate| acl.visible(identity, &candidate.path));
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
    let mut registry = registry.lock_recover();
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

/// Whether a pin covers `record_id` once this pass has recorded the
/// namespace's topology: the walk follows each visible candidate's current
/// parent, and the registry's recorded parent above them.
fn covered_by_pin(
    registry: &AvailabilityRegistry,
    parents: &HashMap<fs_protocol::FileRecordId, Option<fs_protocol::FileRecordId>>,
    record_id: fs_protocol::FileRecordId,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(record_id);
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return false;
        }
        if registry
            .get(id)
            .is_some_and(|record| record.policy == AvailabilityPolicy::Pinned)
        {
            return true;
        }
        cursor = match parents.get(&id) {
            Some(parent) => *parent,
            None => registry.get(id).and_then(|record| record.parent),
        };
    }
    false
}

/// The row a pass records for `candidate`'s place in the tree, or `None`
/// when the registry already holds it.
fn topology_update(
    current: Option<&RecordAvailability>,
    candidate: &PinnedCandidate,
    observed_at: u64,
) -> Option<RecordAvailability> {
    let mut record = current
        .cloned()
        .unwrap_or_else(|| RecordAvailability::new(candidate.record_id, candidate.kind));
    let version_changed = record.current_version != candidate.version;
    let mut changed =
        current.is_none() || record.kind != candidate.kind || record.parent != candidate.parent;
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
    changed.then_some(record)
}

/// The row a pass records for a pinned file whose content `probe` found,
/// or `None` to leave it alone, and whether its fetch may start now.
fn pinned_state_update(
    current: &RecordAvailability,
    candidate: &PinnedCandidate,
    probe: PinnedProbe,
    observed_at: u64,
) -> (Option<RecordAvailability>, bool) {
    let mut record = current.clone();
    match probe {
        PinnedProbe::Local => {
            if record.current_version != candidate.version {
                return (None, false);
            }
            let resident = Residency::Resident {
                bytes: candidate.size,
            };
            // Re-stamping a file that is still resident would rewrite the
            // registry on every pass for nothing.
            if record.residency == resident
                && record.transfer == crate::availability::TransferState::default()
                && record.freshness.local_verified_at_ms.is_some()
            {
                return (None, false);
            }
            record.residency = resident;
            record.transfer = crate::availability::TransferState::default();
            record.freshness.local_verified_at_ms = Some(observed_at);
            (Some(record), false)
        }
        PinnedProbe::Missing { resident_bytes } => {
            // The worker owns queued work.  Do not turn a current download
            // back into another request when a coalesced wake arrives.
            if record.transfer.phase == TransferPhase::Downloading {
                return (None, false);
            }
            record.residency = if resident_bytes == 0 {
                Residency::NotResident
            } else {
                Residency::Partial { resident_bytes }
            };
            record.transfer.phase = TransferPhase::Queued;
            record.transfer.progress = Some(fs_protocol::TransferProgress {
                completed_bytes: resident_bytes,
                total_bytes: Some(candidate.size),
            });
            let due = record
                .transfer
                .retry_at_ms
                .is_none_or(|deadline| deadline <= observed_at);
            ((record != *current).then_some(record), due)
        }
    }
}

/// What one pass decided: the registry rows it writes, in order, the files
/// whose fetch it starts, and what it leaves for the scheduler.
#[derive(Default)]
struct PinnedPassPlan {
    writes: Vec<RecordAvailability>,
    fetches: Vec<PinnedCandidate>,
    outcome: PinnedPassOutcome,
}

/// Record the visible topology, then every pinned file's probed state, in
/// `registry`'s memory; the caller commits. Topology goes first, so a pin on
/// a stable directory covers children introduced by a later namespace
/// generation and keeps covering them across restarts.
fn plan_pinned_pass(
    registry: &mut AvailabilityRegistry,
    candidates: &[PinnedCandidate],
    probes: &HashMap<fs_protocol::FileRecordId, PinnedProbe>,
    observed_at: u64,
) -> PinnedPassPlan {
    let mut plan = PinnedPassPlan::default();
    let mut unparented = 0_usize;
    for candidate in candidates {
        let Some(record) =
            topology_update(registry.get(candidate.record_id), candidate, observed_at)
        else {
            continue;
        };
        // Ancestry stays fail-closed: a row whose parent is not a recorded
        // directory would fail validation, so it waits for its parent.
        if record.parent.is_some_and(|parent| {
            registry
                .get(parent)
                .is_none_or(|parent| parent.kind != RecordKind::Directory)
        }) {
            unparented = unparented.saturating_add(1);
            continue;
        }
        let _ = registry.put(record.clone());
        plan.writes.push(record);
    }
    if unparented > 0 {
        eprintln!("pinned hydration: {unparented} records wait for an unrecorded parent");
    }

    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.kind == RecordKind::File && candidate.hash.is_some())
    {
        let Some(current) = registry.get(candidate.record_id).cloned() else {
            continue;
        };
        let pinned = matches!(
            registry.effective_policy(candidate.record_id),
            Ok(Some(AvailabilityPolicy::Pinned))
        );
        if !pinned {
            if current.transfer.retry_count > 0
                || matches!(
                    current.transfer.phase,
                    TransferPhase::Queued | TransferPhase::Downloading
                )
            {
                let mut record = current;
                record.transfer = crate::availability::TransferState::default();
                let _ = registry.put(record.clone());
                plan.writes.push(record);
            }
            continue;
        }
        let Some(probe) = probes.get(&candidate.record_id).copied() else {
            continue;
        };
        let (update, due) = pinned_state_update(&current, candidate, probe, observed_at);
        if let Some(record) = update {
            let _ = registry.put(record.clone());
            plan.writes.push(record);
        }
        if !matches!(probe, PinnedProbe::Missing { .. })
            || registry
                .get(candidate.record_id)
                .is_none_or(|record| record.transfer.phase != TransferPhase::Queued)
        {
            continue;
        }
        if due {
            if plan.fetches.len() < MAX_PINNED_FETCHES_PER_PASS {
                plan.fetches.push(candidate.clone());
            } else {
                plan.outcome.more_due = true;
            }
        } else if let Some(retry_at) = registry
            .get(candidate.record_id)
            .and_then(|record| record.transfer.retry_at_ms)
        {
            plan.outcome.next_retry_at_ms = Some(
                plan.outcome
                    .next_retry_at_ms
                    .map_or(retry_at, |earliest| earliest.min(retry_at)),
            );
        }
    }
    plan
}

/// Commit a pass with one validation and one durable write of the registry
/// (none when it changes nothing). Should the rows be invalid as a whole,
/// the batch leaves memory and disk untouched and each row is committed on
/// its own instead, so one bad row costs only itself.
fn commit_pinned_pass(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    candidates: &[PinnedCandidate],
    probes: &HashMap<fs_protocol::FileRecordId, PinnedProbe>,
    observed_at: u64,
) -> PinnedPassPlan {
    let mut registry = registry.lock_recover();
    let mut plan = PinnedPassPlan::default();
    let committed = registry.batch(|registry| {
        plan = plan_pinned_pass(registry, candidates, probes, observed_at);
    });
    if let Err(error) = committed {
        eprintln!(
            "could not commit pinned hydration pass at once ({error}); committing per record"
        );
        let mut rejected = std::collections::HashSet::new();
        for record in std::mem::take(&mut plan.writes) {
            let record_id = record.record_id;
            if let Err(error) = registry.put(record) {
                eprintln!("could not reconcile pinned record: {error}");
                rejected.insert(record_id);
            }
        }
        plan.fetches
            .retain(|candidate| !rejected.contains(&candidate.record_id));
    }
    plan
}

async fn reconcile_pinned_hydration(
    namespace: &Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
    wakes: tokio::sync::watch::Receiver<u64>,
) -> PinnedPassOutcome {
    let has_pinned_policy = registry.lock_recover().has_pinned_policy();
    if !has_pinned_policy {
        // Nothing is kept offline any more: what was goes back to the cache
        // budget. An in-memory pass over the area, no namespace walk.
        keep_offline_only(&hydrator, HashSet::new()).await;
        return PinnedPassOutcome::default();
    }
    let candidates = {
        let namespace = Arc::clone(namespace);
        let enforcement = enforcement.cloned();
        let walk = tokio::task::spawn_blocking(move || {
            pinned_candidates(&namespace, enforcement.as_ref())
        });
        let Ok(candidates) = walk.await else {
            return PinnedPassOutcome::default();
        };
        candidates
    };
    let observed_at = now_ms();

    // Policy is metadata-only and cheap to evaluate. Do it before probing
    // the chunk store so an automatic workspace does not re-walk every
    // manifest on each reconciliation.
    let pinned_files = {
        let registry = registry.lock_recover();
        let parents = candidates
            .iter()
            .map(|candidate| (candidate.record_id, candidate.parent))
            .collect::<HashMap<_, _>>();
        candidates
            .iter()
            .filter(|candidate| {
                candidate.kind == RecordKind::File
                    && candidate.hash.is_some()
                    && covered_by_pin(&registry, &parents, candidate.record_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    // The pins decide what the cache budget may take, before this pass
    // fetches anything: a fetch counts against the budget when it ends, and
    // a pinned download can run for hours, so a file already here when it
    // was pinned is held from now on, not from the end of the pass. What no
    // pin covers any more (unpinned, an earlier version, a deleted file) is
    // cache again.
    let kept = pinned_files
        .iter()
        .filter_map(|candidate| candidate.hash.map(|hash| *hash.as_bytes()))
        .collect();
    keep_offline_only(&hydrator, kept).await;
    let mut probes = HashMap::with_capacity(pinned_files.len());
    for candidate in &pinned_files {
        let Some(hash) = candidate.hash else {
            continue;
        };
        let probe = if hydrator.sync_state(hash).await == fs_protocol::SyncState::Local {
            PinnedProbe::Local
        } else {
            PinnedProbe::Missing {
                resident_bytes: hydrator
                    .residency(hash)
                    .await
                    .hydrated_bytes
                    .min(candidate.size),
            }
        };
        probes.insert(candidate.record_id, probe);
    }

    let plan = commit_pinned_pass(&registry, &candidates, &probes, observed_at);
    let mut in_flight = tokio::task::JoinSet::new();
    for candidate in plan.fetches {
        in_flight.spawn(hydrate_pinned_candidate(
            candidate,
            Arc::clone(&hydrator),
            Arc::clone(namespace),
            Arc::clone(&registry),
            wakes.clone(),
        ));
    }
    while in_flight.join_next().await.is_some() {}
    plan.outcome
}

/// Holds exactly the file-backed content in `kept` against the cache budget
/// ([`crate::store::area::FileArea::keep_offline_only`]), off the async
/// runtime's workers: it records every change it makes.
async fn keep_offline_only(hydrator: &Hydrator, kept: HashSet<[u8; 32]>) {
    let files = Arc::clone(hydrator.store.files());
    let applied = tokio::task::spawn_blocking(move || files.keep_offline_only(&kept)).await;
    if let Ok(Err(error)) = applied {
        eprintln!("could not record which content is kept offline: {error}");
    }
}

/// Run one member of a bounded scheduler batch.  All members observe the same
/// generation signal, so a policy or namespace version change cancels the
/// batch promptly; the subsequent coalesced pass chooses the current work.
async fn hydrate_pinned_candidate(
    candidate: PinnedCandidate,
    hydrator: Arc<Hydrator>,
    namespace: Arc<Mutex<Namespace>>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    mut wakes: tokio::sync::watch::Receiver<u64>,
) {
    let Some(hash) = candidate.hash else { return };
    update_candidate_state(&registry, &candidate, |record| {
        record.transfer.phase = TransferPhase::Downloading;
    });
    let mut last_persisted = 0_u64;
    let hydration = hydrator.prefetch_with_progress(
        hash,
        candidate.size,
        crate::hydration::Priority::Pinned,
        |completed, total| {
            if completed != total
                && completed.saturating_sub(last_persisted) < HYDRATION_PROGRESS_PERSIST_BYTES
            {
                return;
            }
            last_persisted = completed;
            update_candidate_state(&registry, &candidate, |record| {
                record.transfer.phase = TransferPhase::Downloading;
                record.transfer.progress = Some(fs_protocol::TransferProgress {
                    completed_bytes: completed,
                    total_bytes: Some(total),
                });
            });
        },
    );
    let result =
        await_current_pinned_hydration(hydration, &candidate, &namespace, &registry, &mut wakes)
            .await;
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
            record.transfer.retry_at_ms =
                Some(now_ms().saturating_add(delay.saturating_mul(1_000)));
            record.transfer.last_error = Some(crate::availability::TransferError {
                message: fs_protocol::NO_PEER_DELIVERED_ERROR.into(),
                occurred_at_ms: now_ms(),
            });
        }),
        None => update_candidate_state(&registry, &candidate, |record| {
            record.transfer = crate::availability::TransferState::default();
        }),
    }
}

/// Await one fetch while only cancelling it for a change that invalidates its
/// own residency promise.  The global generation is still useful to coalesce
/// namespace reconciliation, but unrelated writes must not starve a large
/// pinned download.
async fn await_current_pinned_hydration<T>(
    hydration: impl Future<Output = T>,
    candidate: &PinnedCandidate,
    namespace: &Arc<Mutex<Namespace>>,
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    wakes: &mut tokio::sync::watch::Receiver<u64>,
) -> Option<T> {
    tokio::pin!(hydration);
    // The worker cannot observe a remote namespace change while this batch is
    // active, so periodically validate the candidate as well as validating on
    // every local wake.
    let mut repair = tokio::time::interval(PINNED_FETCH_RECHECK);
    repair.tick().await;
    loop {
        tokio::select! {
            result = &mut hydration => return Some(result),
            changed = wakes.changed() => {
                if changed.is_err() || !pinned_candidate_is_current(candidate, namespace, registry) {
                    return None;
                }
            }
            _ = repair.tick() => {
                if !pinned_candidate_is_current(candidate, namespace, registry) {
                    return None;
                }
            }
        }
    }
}

/// Resolves when `changes` moves on. Never, when there is nothing to watch
/// or its sender is gone (a dropped sender cannot announce anything again).
async fn next_change<T: Send + Sync>(changes: Option<&mut tokio::sync::watch::Receiver<T>>) {
    if let Some(changes) = changes
        && changes.changed().await.is_ok()
    {
        return;
    }
    std::future::pending::<()>().await;
}

/// Resolves at `deadline`; never without one.
async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The runtime instant of a wall-clock retry deadline, at least a moment
/// from now, so a deadline already past cannot spin the worker.
fn retry_instant(retry_at_ms: u64) -> tokio::time::Instant {
    let wait =
        Duration::from_millis(retry_at_ms.saturating_sub(now_ms())).max(Duration::from_millis(100));
    let now = tokio::time::Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

fn pinned_candidate_is_current(
    candidate: &PinnedCandidate,
    namespace: &Arc<Mutex<Namespace>>,
    registry: &Arc<Mutex<AvailabilityRegistry>>,
) -> bool {
    let pinned = registry
        .lock_recover()
        .effective_policy(candidate.record_id)
        .is_ok_and(|policy| policy == Some(AvailabilityPolicy::Pinned));
    if !pinned {
        return false;
    }
    let namespace = namespace.lock_recover();
    let Some(ino) = namespace.ino_for_record_id(candidate.record_id) else {
        return false;
    };
    namespace
        .content_revision_for(ino)
        .map(|revision| revision.version_id)
        == candidate.version
        && namespace.hash_for(ino) == candidate.hash
        && namespace
            .getattr(ino)
            .is_some_and(|attr| attr.size == candidate.size)
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
        // Every directory operation takes the namespace lock, so the kernel
        // need not serialise lookups/creates per directory for us.
        let _ = config.add_capabilities(InitFlags::FUSE_PARALLEL_DIROPS);
        // `readdirplus` answers a listing and its attributes in one round
        // trip (`ls -l`, file managers); the kernel picks it adaptively.
        let _ = config
            .add_capabilities(InitFlags::FUSE_DO_READDIRPLUS | InitFlags::FUSE_READDIRPLUS_AUTO);
        // Writeback caching stays off: with it the kernel owns `i_size` and
        // mtime and ignores the sizes this daemon reports, so a remote edit
        // that changes a file's length would not become visible without
        // explicit invalidation.
        if let Some(passthrough) = &self.passthrough {
            match (
                config.add_capabilities(InitFlags::FUSE_PASSTHROUGH),
                config.set_max_stack_depth(1),
            ) {
                (Ok(()), Ok(_)) => passthrough.kernel_ready.store(true, Ordering::Release),
                _ => eprintln!("FUSE passthrough is not offered by this kernel; serving reads"),
            }
        }
        Ok(())
    }

    /// Resolve one name in a directory. Namespace lookup and view evaluation
    /// run on every call in the same order, so a nonexistent name and a name
    /// hidden by the view cost the same work and return the same `ENOENT`.
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.as_bytes();
        let (found, child, is_doc) = {
            let mut ns = self.ns();
            let found = ns.lookup(parent.0, name);
            let child = ns.path_for(parent.0).map(|p| child_path(&p, name));
            let is_doc = found
                .as_ref()
                .is_some_and(|attr| ns.document_codec(attr.ino.0).is_some());
            drop(ns);
            (found, child, is_doc)
        };
        let in_view = child.is_some_and(|c| self.in_view(&c));
        match found {
            Some(attr) if in_view => {
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
        match self.attr_for_request(ino.0, fh.map(|fh| fh.0)) {
            Ok((attr, is_doc)) => reply.attr(&Self::ttl_for(is_doc), &attr),
            Err(e) => reply.error(to_errno(e)),
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
        if let Some(signal) = &self.change_signal {
            signal.note_listed(ino.0);
        }
        self.heuristics.on_readdir(ino.0);
    }

    /// `readdir` plus each entry's attributes, exactly as `lookup` would
    /// report them, for the same filtered view.
    fn readdirplus(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectoryPlus,
    ) {
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.visible_entries(ino.0) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, (e_ino, _, name)) in entries.into_iter().enumerate().skip(skip) {
            let Some((attr, ttl)) = self.presented_attr(e_ino) else {
                continue;
            };
            let next_offset = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
            if reply.add(
                INodeNo(e_ino),
                next_offset,
                OsStr::from_bytes(name.as_bytes()),
                &ttl,
                &attr,
                Generation(0),
            ) {
                break;
            }
        }
        reply.ok();
        if let Some(signal) = &self.change_signal {
            signal.note_listed(ino.0);
        }
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
            if let Err(e) = self.check_path(&opened_path, *access) {
                reply.error(to_errno(e));
                return;
            }
        }
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        self.open_file_handles.lock_recover().insert(
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
        let cache = self.open_cache_flags(ino.0);
        if flags.acc_mode() == OpenAccMode::O_RDONLY
            && let Some(backing) = self.passthrough_backing(ino.0)
        {
            match reply.open_backing(&backing) {
                Ok(id) => {
                    reply.opened_passthrough(FileHandle(handle), cache, &id);
                    if let Some(passthrough) = &self.passthrough {
                        passthrough.hold(handle, id, backing);
                    }
                    self.predict_after_open(ino.0);
                    return;
                }
                Err(error) => {
                    if let Some(passthrough) = &self.passthrough {
                        passthrough.disable(&error);
                    }
                }
            }
        }
        reply.opened(FileHandle(handle), cache);
        self.predict_after_open(ino.0);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.plan_read(ino.0, fh.0, offset, size) {
            ReadPlan::Error(e) => reply.error(to_errno(e)),
            ReadPlan::Buffered(buffered) => self.reply_buffered(ino.0, buffered, reply),
            ReadPlan::Local(data) => reply.data(&data),
            ReadPlan::Remote(hash) => self.reply_remote(
                ino.0,
                hash,
                (offset, u64::from(size)),
                flags.0 & libc::O_NONBLOCK != 0,
                reply,
            ),
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the FUSE create callback keeps authorization, creation, and returned attributes on one inode snapshot"
    )]
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
            .then(|| name.to_str().and_then(document_codec_for))
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
        // Start a fresh empty stage so subsequent writes accumulate. The
        // empty file was committed by the create itself.
        {
            let stage = self.empty_stage(self.is_document(attr.ino.0)).shared();
            let mut buffers = self.buffers();
            buffers.insert(attr.ino.0, stage);
            self.buffer_states().insert(
                attr.ino.0,
                BufferState {
                    committed: Some(0),
                    ..BufferState::default()
                },
            );
        }
        self.publish(delta);
        if !signed {
            self.record_version(attr.ino.0, fs_protocol::FileProvenance::Create, None);
        }
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        let opened_path = self.ns().path_for(attr.ino.0);
        if let Some(opened_path) = opened_path {
            self.open_file_handles.lock_recover().insert(
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
        match self.write_buffer(ino.0, offset, data) {
            Ok(written) => reply.written(written),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.flush_inode(ino.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.fsync_inode(ino.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    /// `fsync` of a directory: the names in it survive a crash. Left
    /// unimplemented, the kernel would take the `ENOSYS` as success for every
    /// later directory sync while a rename or create stayed in memory until
    /// the persister's next write.
    fn fsyncdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        match self.sync_namespace() {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
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
        if let Some(passthrough) = &self.passthrough {
            passthrough.release(fh.0);
        }
        // The kernel ignores release errors (close already returned flush's
        // result); the error is still reported for tracing.
        match self.release_handle(ino.0, fh.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn destroy(&mut self) {
        self.persist_all_buffers();
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
        // Truncation never reads the bytes it drops: a stage only changes its
        // length, and an unopened file commits the kept prefix.
        if let Some(new_size) = size
            && let Err(errno) = self.truncate_inode(ino.0, new_size)
        {
            reply.error(to_errno(errno));
            return;
        }

        let buf_len = self.staged_len(ino.0);
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
                    self.record_version(attr.ino.0, fs_protocol::FileProvenance::Create, None);
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
        #[cfg(target_os = "linux")]
        let unsupported_flags = flags.difference(RenameFlags::RENAME_NOREPLACE);
        #[cfg(not(target_os = "linux"))]
        let unsupported_flags = flags;
        if !unsupported_flags.is_empty() {
            reply.error(to_errno(libc::ENOTSUP));
            return;
        }
        #[cfg(target_os = "linux")]
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
        // The daemon's own change signal: it changes nothing, and is only
        // ever raised by the daemon, which alone knows the token.
        if name.to_str() == Some(tilo_files::native::CHANGE_SIGNAL_XATTR) {
            match &self.change_signal {
                Some(signal) if signal.accepts(value) => reply.ok(),
                _ => reply.error(to_errno(libc::EPERM)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::files::CHECKED_HISTORY_RESTORE_XATTR) {
            match self.checked_restore_xattr(ino.0, value) {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(fs_protocol::files::HISTORY_JOB_XATTR_PREFIX))
        {
            let result = match uuid::Uuid::parse_str(id) {
                Ok(id) if value == fs_protocol::files::HISTORY_JOB_CANCEL => {
                    self.cancel_history_job(ino.0, id)
                }
                _ => Err(EINVAL),
            };
            match result {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::AVAILABILITY_CONTROL_XATTR) {
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
        if name.to_str() == Some(fs_protocol::HISTORY_CONTROL_XATTR) {
            if value.len() > fs_protocol::MAX_HISTORY_CONTROL_BYTES {
                reply.error(to_errno(libc::E2BIG));
                return;
            }
            let Ok(request) = serde_json::from_slice::<fs_protocol::HistoryControlRequestV1>(value)
            else {
                reply.error(to_errno(EINVAL));
                return;
            };
            if request.schema_version != fs_protocol::HISTORY_CONTROL_VERSION {
                reply.error(to_errno(EINVAL));
                return;
            }
            match request.operation {
                fs_protocol::HistoryControlOperationV1::ResolveConflict {
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
        if name.to_str() != Some(fs_protocol::PIN_XATTR) {
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

    /// Serve the `user.tilo.sync` extended attribute: the entry's hydration
    /// state (`local`/`projected`/`downloading`). Every other attribute name
    /// reports absent (`ENODATA`) — the collaborative FS owns only this one.
    // One dispatch point owns the bounded collaborative xattr protocol.
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch point owns the bounded collaborative extended-attribute protocol"
    )]
    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        // Outside the view the inode doesn't exist — checked before the
        // attribute name, so the sync state never reports on a hidden file.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if name.to_str() == Some(tilo_files::native::CHANGE_SIGNAL_XATTR) {
            match self
                .change_signal
                .as_ref()
                .and_then(|signal| signal.answer())
            {
                Some(live) => reply_xattr_bytes(reply, live, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some(name) = name
            .to_str()
            .filter(|name| name.starts_with(fs_protocol::files::HISTORY_READ_XATTR))
        {
            use fs_protocol::files::{FileHistoryReadQueryV1 as Query, FileHistoryReadResponseV1};
            let query = match Query::parse_xattr(name) {
                Ok(query) => query,
                Err(error) => {
                    reply.error(to_errno(history_read::errno(error)));
                    return;
                }
            };
            if let Query::Preview {
                frontier,
                version_id,
            } = query
            {
                match self.read_file_history_preview_at_frontier(ino.0, frontier, version_id) {
                    Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                    Err(error) => reply.error(to_errno(history_read::errno(error))),
                }
            } else {
                let response = FileHistoryReadResponseV1 {
                    schema_version: 1,
                    result: self.read_file_history(ino.0, query),
                };
                match serde_json::to_vec(&response) {
                    Ok(bytes) if bytes.len() <= fs_protocol::files::MAX_HISTORY_READ_BYTES => {
                        reply_xattr_bytes(reply, &bytes, size);
                    }
                    _ => reply.error(to_errno(EIO)),
                }
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::files::CHECKED_HISTORY_REVIEW_XATTR) {
            let projection = fs_protocol::files::CheckedHistoryReviewResponseV1 {
                schema_version: 1,
                result: self.checked_history_review(ino.0),
            };
            match serde_json::to_vec(&projection) {
                Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                Err(_) => reply.error(to_errno(EIO)),
            }
            return;
        }
        if let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(fs_protocol::files::HISTORY_JOB_XATTR_PREFIX))
        {
            match uuid::Uuid::parse_str(id)
                .map_err(|_| EINVAL)
                .and_then(|id| self.history_job_xattr(ino.0, id))
            {
                Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if let Some(id) = name.to_str().and_then(|name| {
            name.strip_prefix(fs_protocol::files::CHECKED_HISTORY_RESULT_XATTR_PREFIX)
        }) {
            let Ok(id) = uuid::Uuid::parse_str(id) else {
                reply.error(to_errno(EINVAL));
                return;
            };
            let projection = fs_protocol::files::CheckedHistoryResponseV1 {
                schema_version: 1,
                outcome: self.checked_history_result(ino.0, id),
            };
            match serde_json::to_vec(&projection) {
                Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                Err(_) => reply.error(to_errno(EIO)),
            }
            return;
        }
        if let Some(version) = name
            .to_str()
            .and_then(|name| name.strip_prefix(fs_protocol::HISTORY_PREVIEW_XATTR_PREFIX))
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(fs_protocol::FileVersionId)
        {
            match self.history_version_preview(ino.0, version) {
                Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if let Some(conflict_id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(fs_protocol::MERGE_PREFLIGHT_XATTR_PREFIX))
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
        {
            match self.history_merge_preflight(ino.0, conflict_id) {
                Ok(bytes) => reply_xattr_bytes(reply, &bytes, size),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::SHARE_CONTROL_XATTR) {
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
        if name.to_str() == Some(fs_protocol::THUMB_XATTR) {
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
        if name.to_str() == Some(fs_protocol::CONTENT_HASH_XATTR) {
            let hash = self.ns().hash_for(ino.0);
            match hash {
                Some(hash) => reply_xattr_bytes(reply, hash.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::RECORD_ID_XATTR) {
            let record_id = self.ns().record_id_for(ino.0);
            match record_id {
                Some(record_id) => reply_xattr_bytes(reply, record_id.0.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::METADATA_XATTR) {
            match self.metadata_projection_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::projection::RECORD_INDEX_V2_XATTR) {
            if ino.0 != crate::namespace::ROOT_INO {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            // Raw-offset v2 is safe only where all records are observable.
            // ACL-constrained mounts use v3's private opaque continuations.
            if self.enforcement.is_some() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_manifest_v2() {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::projection::RECORD_INDEX_V3_XATTR) {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_none() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_manifest_v3() {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some((revision, cursor)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("user.tilo.record-index.v3."))
            .and_then(|page| page.split_once('.'))
        {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_none() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_page_v3(revision, cursor) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some(record_id) = name
            .to_str()
            .and_then(|name| {
                name.strip_prefix(fs_protocol::projection::RECORD_INDEX_BY_ID_XATTR_PREFIX)
            })
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(fs_protocol::FileRecordId)
        {
            if ino.0 != crate::namespace::ROOT_INO {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_by_id_v2(record_id) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some((revision, offset)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("user.tilo.record-index.v2."))
            .and_then(|page| page.rsplit_once('.'))
            .and_then(|(revision, offset)| {
                offset
                    .parse::<usize>()
                    .ok()
                    .map(|offset| (revision, offset))
            })
        {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_some() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.record_index_page_v2(revision, offset) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                // A changed namespace makes the old revision intentionally
                // absent; clients restart from the fresh manifest.
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::AVAILABILITY_XATTR) {
            match self.availability_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::SHARE_XATTR) {
            match self.share_projection_for(ino.0, now_ms()) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::HISTORY_XATTR) {
            match self.history_projection_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::PIN_XATTR) {
            let Some(registry) = &self.availability else {
                reply.error(to_errno(libc::ENODATA));
                return;
            };
            // The policy `user.tilo.availability` reports: the file's own
            // or a folder's above it. The registry's row for the file alone
            // would say nothing about a file created in a pinned folder
            // until a restart registered it.
            let ancestry = record_ancestry(&mut self.ns(), ino.0);
            let policy =
                ancestry.map(|ancestry| inherited_policy(&registry.lock_recover(), &ancestry));
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
        if name.to_str() != Some(fs_protocol::SYNC_XATTR) {
            reply.error(to_errno(libc::ENODATA));
            return;
        }
        let Some(state) = self.sync_state_for(ino.0) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        reply_xattr_bytes(reply, state.as_str().as_bytes(), size);
    }

    /// List the extended attributes on `ino`: just `user.tilo.sync` (NUL
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
        let mut buf = Vec::with_capacity(fs_protocol::SYNC_XATTR.len().saturating_add(1));
        buf.extend_from_slice(fs_protocol::SYNC_XATTR.as_bytes());
        buf.push(0);
        // Cheap extension check, no generation — a `getxattr` of it may
        // still be `ENODATA` if the content isn't local yet.
        let previewable = self.thumbs.is_some()
            && name.as_deref().is_some_and(|n| {
                thumbnail::category_for_path(std::path::Path::new(n)).has_preview()
            });
        if previewable {
            buf.extend_from_slice(fs_protocol::THUMB_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().hash_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::CONTENT_HASH_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::RECORD_ID_XATTR.as_bytes());
            buf.push(0);
        }
        if self.metadata_projection_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::METADATA_XATTR.as_bytes());
            buf.push(0);
        }
        if ino.0 == crate::namespace::ROOT_INO {
            buf.extend_from_slice(fs_protocol::projection::RECORD_INDEX_V2_XATTR.as_bytes());
            buf.push(0);
        }
        if self.availability.is_some() {
            buf.extend_from_slice(fs_protocol::AVAILABILITY_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(fs_protocol::PIN_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(fs_protocol::AVAILABILITY_CONTROL_XATTR.as_bytes());
            buf.push(0);
        }
        if self.share_authority.is_some() && self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::SHARE_XATTR.as_bytes());
            buf.push(0);
        }
        if self.history.is_some() && self.ns().record_id_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::HISTORY_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(fs_protocol::HISTORY_CONTROL_XATTR.as_bytes());
            buf.push(0);
            if self.replacement_transactions.is_some() {
                buf.extend_from_slice(fs_protocol::files::CHECKED_HISTORY_REVIEW_XATTR.as_bytes());
                buf.push(0);
                buf.extend_from_slice(fs_protocol::files::CHECKED_HISTORY_RESTORE_XATTR.as_bytes());
                buf.push(0);
            }
        }
        if ino.0 == crate::namespace::ROOT_INO && self.share_control_path.is_some() {
            buf.extend_from_slice(fs_protocol::SHARE_CONTROL_XATTR.as_bytes());
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
mod tests;

#[cfg(test)]
mod durability_tests;

#[cfg(test)]
mod write_attribution_tests;

#[cfg(test)]
mod prediction_tests;
