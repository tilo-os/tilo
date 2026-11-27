// Without FUSE (an Android app, Windows) nothing is mounted: the parts of the filesystem only its
// FUSE adapter calls have no caller there.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(
        dead_code,
        unused_imports,
        reason = "only the FUSE adapter uses them, and nothing is mounted without FUSE"
    )
)]
//! The FUSE operations, and the filtered view they serve.
//!
//! A denial is presented as an absence: a path outside the view is `ENOENT` with no
//! `readdir` entry, a path in the view but not writable is `EROFS`, a path in the view
//! and readable is served normally. `EACCES` is never returned for a policy decision:
//! it is the errno most software handles by aborting, while "no such file" and
//! "read-only filesystem" are states programs already handle. `readdir` omits hidden
//! entries, `lookup` does the same work whether or not a name exists, and a
//! directory's `nlink` is computed from the filtered listing, so none of them
//! announces what is hidden.
//!
//! Creating a name that already exists but is outside the view answers `EROFS` rather
//! than writing over state the app cannot see. The capability model cannot produce
//! that situation: a `Restrict` is a ceiling and every verb implies `Read`, so
//! reaching it needs a direct [`crate::acl::AclEntry`] with `write` but not `read`.
use crate::errno::EDQUOT;
use crate::node::{FileAttr, FileType, INodeNo};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use fuser::{
    BackingId, BsdFileFlags, Errno, FileHandle, Filesystem, FopenFlags, Generation, InitFlags,
    KernelConfig, LockOwner, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite,
    ReplyXattr, Request, TimeOrNow, WriteFlags,
};
use libc::{EBUSY, EEXIST, EFBIG, EINVAL, EIO, EISDIR, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;

use crate::acl::{Access, AclHandle};
use crate::availability::{AvailabilityRegistry, DocumentAvailability, EntryKind};
use crate::clock::unix_now_ms as now_ms;
use crate::eviction::{EvictionMode, EvictionQueue, EvictionRequestId, EvictionRequestState};
use crate::hydration::Hydrator;
use crate::namespace::{IndexedDocument, InodeQuery, InodeView, Namespace};
use crate::smart::Heuristics;
use crate::smart::history::{AccessHistory, natural_cmp};
use crate::smart::kept_history::{KeptHistory, KeptHistorySource};
use fs_protocol::projection::{
    DOCUMENT_INDEX_PAGE_ENTRIES, DOCUMENT_INDEX_V2_VERSION, DocumentIndexEntryV1,
};
use fs_protocol::{AvailabilityPolicy, Residency, TransferPhase};

const TTL: Duration = Duration::from_secs(10);
/// Entries whose attributes one `readdirplus` pass reads while holding the namespace: a reply
/// page is at most a few hundred entries, so a pass covers the page without holding the lock
/// for a whole folder.
const ATTRIBUTES_PER_PASS: usize = 256;
// The pinned-hydration worker wakes for everything that can change its plan: a local
// publish, any namespace change (remote applies included), an ACL replacement, a
// change to what the availability registry says it acts on, and the earliest retry
// deadline. This backstop only bounds how long something none of those announce could
// go unrepaired; it compares cheap change tokens and walks the namespace only when one
// of them moved.
const PINNED_HYDRATION_BACKSTOP: Duration = Duration::from_secs(300);
// A namespace change wakes the worker after this long, so a burst of remote applies is
// one pass rather than one per change.
const PINNED_CHANGE_SETTLE: Duration = Duration::from_millis(500);
// While a pinned fetch runs, the worker cannot see a remote change that would make it
// stale; the fetch re-checks its candidate this often.
const PINNED_FETCH_RECHECK: Duration = Duration::from_secs(5);
const MAX_PINNED_FETCHES_PER_PASS: usize = 4;
const HYDRATION_PROGRESS_PERSIST_BYTES: u64 = 8 * 1024 * 1024;

/// The in-flight write stages of a mount, by inode. Holding this map's lock fixes the
/// set of inodes with pending writes (storage eviction and checked restore rely on
/// that); the bytes of a stage sit behind its own lock, so one inode's writes, reads
/// and commits never wait for another's.
pub type WriteStages = HashMap<u64, Arc<Stage>>;

/// Writes the namespace to disk unless the copy there already holds it
/// ([`QuickFS::set_durability_hook`]).
pub type DurabilityHook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// How long a mount syncing on a window ([`QuickFS::sync_on_window`]) lets its writes wait for
/// the disk: what a power loss may take of what `close` answered for, never what `fsync` did.
pub const SYNC_WINDOW: Duration = Duration::from_millis(200);

/// Keeps on disk what the namespace changed since it last did, the moment the mount changed
/// it ([`QuickFS::with_change_journal`]), before the request that made it is answered. What a
/// request was answered for then outlives the process, killed or not, however long the
/// namespace's own copy on disk lags. It takes the lock itself for what it reads, and writes
/// once it has let the lock go: a write to the disk is the writer's wait, never a reader's.
pub type ChangeJournal = Arc<dyn Fn(&Mutex<Namespace>) -> io::Result<()> + Send + Sync>;

/// The namespace, locked for one step of the mount; what the step changed is handed to the
/// mount's [`ChangeJournal`] as the lock is let go.
pub(crate) struct NamespaceGuard<'a> {
    /// Dropped by hand in [`Drop`], which lets the namespace go before its journal writes.
    namespace: std::mem::ManuallyDrop<std::sync::MutexGuard<'a, Namespace>>,
    journal: Option<&'a Journaled>,
}

/// A [`ChangeJournal`] and the announcements of the namespace it keeps, which say whether a
/// step changed anything: a step that only read hands nothing to it.
pub(crate) struct Journaled {
    journal: ChangeJournal,
    /// The namespace the journal locks itself, once the step that changed it has let it go.
    namespace: Arc<Mutex<Namespace>>,
    changes: Mutex<tokio::sync::watch::Receiver<()>>,
}

impl Journaled {
    /// Whether the namespace announced a change since this was last asked.
    fn announced(&self) -> bool {
        let mut announcements = self.changes.lock_recover();
        let announced = announcements.has_changed().unwrap_or(false);
        if announced {
            announcements.borrow_and_update();
        }
        drop(announcements);
        announced
    }
}

impl std::ops::Deref for NamespaceGuard<'_> {
    type Target = Namespace;

    fn deref(&self) -> &Namespace {
        &self.namespace
    }
}

impl std::ops::DerefMut for NamespaceGuard<'_> {
    fn deref_mut(&mut self) -> &mut Namespace {
        &mut self.namespace
    }
}

impl Drop for NamespaceGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `Drop` runs once, and nothing else takes the guard out of its `ManuallyDrop`.
        let namespace = unsafe { std::mem::ManuallyDrop::take(&mut self.namespace) };
        let Some(journaled) = self.journal else {
            return;
        };
        if !journaled.announced() {
            return;
        }
        // Let the namespace go before the journal writes: what it keeps is computed under the
        // lock and written after it, so a read of settled content beside a fresh write waits for
        // the change, never for the write that puts it on disk.
        drop(namespace);
        if let Err(error) = (journaled.journal)(&journaled.namespace) {
            eprintln!("the namespace's change could not be kept on disk: {error}");
        }
    }
}

/// What is told the content of a version written here ([`QuickFS::with_written_content`]).
pub type WrittenContent = dyn Fn(Hash) + Send + Sync;

/// Commit bookkeeping for one in-memory write buffer. `generation` counts mutations of
/// the buffer; a buffer whose latest generation is committed is clean and may be
/// dropped when its last handle closes.
#[derive(Clone, Copy, Debug, Default)]
struct BufferState {
    generation: u64,
    committed: Option<u64>,
    staged: Option<u64>,
    base_hash: Option<Hash>,
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

/// Kernel passthrough for read-only opens of fully local files: the kernel reads a real
/// backing file directly and the daemon only answers `open`.
///
/// Backing files are materialised once per content hash under `dir` (never on this FUSE
/// mount) and registered per open; access is decided at `open`, like on a native file
/// system, so reads through a passthrough handle are not re-checked against the ACL.
/// Registering one needs `CAP_SYS_ADMIN` in the initial user namespace; the first
/// refusal disables passthrough for the mount.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Passthrough {
    dir: std::path::PathBuf,
    kernel_ready: std::sync::atomic::AtomicBool,
    disabled: std::sync::atomic::AtomicBool,
    backing: Mutex<HashMap<u64, (BackingId, std::fs::File)>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

pub(crate) enum VersionSource<'a> {
    Bytes(&'a [u8]),
    /// A file that does not change while it is stored (a capture snapshot), with the
    /// manifest a capture already computed for it.
    File {
        path: &'a std::path::Path,
        manifest: Option<Arc<crate::content::Manifest>>,
        /// The file is a write stage's, handed over: a store may take it as the content entry
        /// itself instead of reading it again.
        adopt: bool,
    },
}

/// Create `dir` owner-only and remove stage files a previous run left.
fn prepare_stage_dir(dir: &std::path::Path) -> io::Result<()> {
    tilo_files::native::private::directory(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(write_stage::STAGE_FILE_PREFIX.as_bytes())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
type CommitPause = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistIntent {
    Close,
    Sync,
}

/// Commit failures that retrying the same bytes into the same record cannot fix: a buffer
/// that ends in one is kept as a separate copy when its last handle closes.
const fn is_permanent_commit_failure(errno: i32) -> bool {
    matches!(errno, EDQUOT | EFBIG | EINVAL)
}

fn commit_errno(error: &io::Error) -> i32 {
    crate::errno::code(error)
}

/// The ceiling for a memory-backed stage. Sparse disk-backed stages do not allocate their
/// logical size and are not subject to it. A memory stage reserves its eventual size
/// before accepting a write or truncate, including deferred zero filling at commit.
const MAX_STAGED_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The aggregate ceiling for one mount's memory stages. Reservations are held until the
/// backing drops, including while a removed stage is still in use; shrinking does not
/// free a Vec's capacity.
const MAX_STAGED_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

fn check_staged_size(len: u64) -> Result<(), i32> {
    if len > MAX_STAGED_FILE_BYTES {
        Err(EFBIG)
    } else {
        Ok(())
    }
}

fn check_staged_growth(staged: u64, growth: u64) -> Result<(), i32> {
    if staged.saturating_add(growth) > MAX_STAGED_TOTAL_BYTES {
        Err(ENOSPC)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod staging_limit_tests;

/// The record ids from `ino` up to the root, or `None` when one of them has no record id.
fn document_ancestry(ns: &mut Namespace, ino: u64) -> Option<Vec<fs_protocol::FileDocumentId>> {
    let mut ancestry = Vec::new();
    let mut cursor = Some(ino);
    while let Some(current) = cursor {
        ancestry.push(ns.document_id_for(current)?);
        cursor = ns.parent_ino(current);
    }
    Some(ancestry)
}

fn inherited_policy(
    registry: &AvailabilityRegistry,
    ancestry: &[fs_protocol::FileDocumentId],
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

/// Name of the sibling that preserves bytes which could not be committed to their own
/// record, derived from the bytes so a retry finds the same copy.
fn unsaved_copy_name(name: &[u8], bytes: &[u8]) -> Vec<u8> {
    unsaved_copy_name_for(name, &blake3::hash(bytes))
}

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

/// Whom the file actions of the request this thread answers are done by, until the request
/// returns.
struct WriterNote(Option<crate::namespace::ActingAs>);

/// Who one request writes for.
struct Writing {
    /// Whether a writer resolver named it: where none does, the thread's own acting stands.
    resolved: bool,
    /// The invoker its file actions name: `None` for the person at this device.
    invoker: Option<tilo_actions::ActionInvoker>,
}

impl Writing {
    fn noted(self) -> WriterNote {
        WriterNote(self.resolved.then(|| {
            crate::namespace::acting_as(
                self.invoker
                    .map(|invoker| tilo_actions::ActionContext::default().with_invoker(invoker)),
            )
        }))
    }
}

impl Drop for WriterNote {
    fn drop(&mut self) {
        self.0.take();
    }
}

#[derive(Clone, Debug)]
struct OpenFileHandleState {
    ino: u64,
    /// Authorization path at open time. Once replacement rename moves this
    /// inode to trash there is intentionally no live path to recompute.
    opened_path: String,
    writable: bool,
    /// `O_APPEND`: every write goes to the staged end of the file. The kernel
    /// derives the offset it sends from its cached inode size, which a remote
    /// edit or a hydration can make stale, so trusting it would overwrite
    /// bytes instead of appending.
    append: bool,
}

struct PinnedHydrationTask {
    /// A watch channel deliberately keeps only the latest generation: a burst of
    /// writes or imports results in one reconciliation, not one walk per event.
    wake: tokio::sync::watch::Sender<u64>,
    task: tokio::task::JoinHandle<()>,
    #[cfg(test)]
    turns: Arc<std::sync::atomic::AtomicU64>,
}

impl Drop for PinnedHydrationTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

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

#[derive(Default)]
struct PinnedPassOutcome {
    more_due: bool,
    next_retry_at_ms: Option<u64>,
}

#[derive(Clone, Copy)]
enum PinnedProbe {
    Local,
    Missing { resident_bytes: u64 },
}

#[derive(Clone)]
struct PinnedCandidate {
    document_id: fs_protocol::FileDocumentId,
    parent: Option<fs_protocol::FileDocumentId>,
    kind: EntryKind,
    hash: Option<Hash>,
    size: u64,
    path: String,
}

#[derive(Clone)]
struct DocumentIndexCursor {
    revision: String,
    /// Private source state captured when the cursor was issued; never serialized.
    source_revision: String,
    acl_revision: u64,
    offset: usize,
}

/// Retained, authority-specific catalogue. Building it is a mutation or mount-time
/// operation; serving a v3 page is a bounded slice of it, and no v3 request walks denied
/// namespace rows.
#[derive(Clone)]
struct AclVisibleDocumentIndex {
    source_revision: String,
    acl_revision: u64,
    revision: String,
    records: Vec<DocumentIndexEntryV1>,
    by_id: HashMap<fs_protocol::FileDocumentId, DocumentIndexEntryV1>,
    /// Rows this authority may not see, with the path they were judged at.
    /// With `by_id` it lets the next refresh under the same ACL revision
    /// re-evaluate only rows whose path changed.
    hidden: HashMap<fs_protocol::FileDocumentId, String>,
    #[cfg(test)]
    acl_evaluations: usize,
}

impl AclVisibleDocumentIndex {
    /// The visibility this view already decided for `document_id` at `path`, if it judged
    /// that exact row. Visibility is a function of the ACL revision and the path alone, so
    /// the verdict holds for the same pair.
    fn verdict(&self, document_id: fs_protocol::FileDocumentId, path: &str) -> Option<bool> {
        if self
            .by_id
            .get(&document_id)
            .is_some_and(|entry| entry.path == path)
        {
            return Some(true);
        }
        self.hidden
            .get(&document_id)
            .is_some_and(|hidden| hidden == path)
            .then_some(false)
    }
}

pub mod activity;
mod attributes;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuse;
mod listing;
pub mod native_ops;
mod read;
mod recovery;
mod unavailable;
mod write_stage;
use listing::{OpTimer, ReaddirCache, note_request};
use read::ReadPlan;
use tilo_lock::MutexRecover as _;
use write_stage::{MissingRanges, SharedStage, StagedRead};
pub use write_stage::{Stage, WriteStage};

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
    /// Which files this mount opened when: what prediction learns from. Local to this
    /// device and workspace and bounded; its opened-together pairs survive restarts
    /// through `kept_history`.
    access_history: Arc<Mutex<AccessHistory>>,
    kept_history: Option<Arc<KeptHistory>>,
    write_buffers: Arc<Mutex<WriteStages>>,
    stage_dir: Option<std::path::PathBuf>,
    memory_stage_budget: Arc<write_stage::MemoryBudget>,
    /// Commit bookkeeping for `write_buffers`. Lock order: `write_buffers`, then one
    /// inode's stage, then `buffer_states`; the namespace lock is taken after a stage
    /// lock, never before.
    buffer_states: Mutex<HashMap<u64, BufferState>>,
    /// Serialises persistence of one inode's buffer, so concurrent FUSE workers never
    /// commit it twice or interleave stage and commit.
    commit_locks: Mutex<HashMap<u64, Arc<Mutex<()>>>>,
    /// Durable staging for `fsync`ed and not-yet-committed bytes. `None` (tests, memory
    /// stores) makes `fsync` commit a version directly.
    unsaved: Option<Arc<crate::unsaved::UnsavedStore>>,
    /// The content each inode's kernel page cache was filled from, so an `open` of
    /// unchanged content keeps the cache and an open after any change drops it.
    kernel_cached: Mutex<HashMap<u64, Option<Hash>>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    passthrough: Option<Passthrough>,
    /// Who the files on the mount are presented as owned by (`None`: each caller).
    owner: Option<(u32, u32)>,
    open_file_handles: Mutex<HashMap<u64, OpenFileHandleState>>,
    next_file_handle: AtomicU64,
    document_index_cursors: Mutex<HashMap<String, DocumentIndexCursor>>,
    document_index_initial_cursors: Mutex<HashMap<(String, String, u64), String>>,
    acl_visible_document_index: Arc<Mutex<Option<Arc<AclVisibleDocumentIndex>>>>,
    /// Asks the refresh worker to rebuild the retained view: a reader found it behind the tree.
    document_index_wanted: Arc<tokio::sync::Notify>,
    /// The last directory listing served, and the revision it was taken at: one `readdir` is
    /// many requests, and building the folder again for each of its pages costs the folder on
    /// every page.
    readdir_cache: Mutex<Option<ReaddirCache>>,
    /// Rich facts extracted while current-version bytes are already local; peers consume
    /// them without hydrating the payload.
    rich_metadata: Mutex<
        HashMap<
            fs_protocol::FileDocumentId,
            (fs_protocol::ContentHash, fs_protocol::FilePropertiesV1),
        >,
    >,
    /// The workspace ACL and the identity it is evaluated as. `None` keeps
    /// Unix-veneer-only behaviour. A handle rather than the ACL, so the policy can be
    /// replaced while the mount is up ([`crate::CapabilityFile`]).
    enforcement: Option<(AclHandle, crate::acl::IdentityKey)>,
    /// Host-side thumbnails, when the mount has a cache directory; `None` leaves the
    /// mount thumbnail-less.
    thumbs: Option<crate::thumbs::ThumbProvider>,
    availability: Option<Arc<Mutex<AvailabilityRegistry>>>,
    stored_writes: Arc<unavailable::StoredWrites>,
    /// The worker that hydrates durable file and folder pins, woken by policy changes and
    /// also reconciling periodically, so later descendants and versions inherit the
    /// promise.
    pinned_hydration: Option<PinnedHydrationTask>,
    eviction_queue: Option<Arc<tokio::sync::Mutex<EvictionQueue>>>,

    /// The person and device this mount writes as, when it knows them.
    actor: Option<fs_protocol::FileActor>,
    /// Who writes, from the process each request names ([`crate::writer`]).
    writers: Option<Arc<crate::writer::WriterResolver>>,
    /// Whether the workspace takes a file action from that writer ([`crate::admission`]).
    admission: Option<Arc<crate::admission::FileAdmission>>,
    /// Told the content of each version written here, once it is the file's
    /// ([`Self::with_written_content`]).
    written: Option<Arc<WrittenContent>>,
    /// Makes the namespace on disk hold the current placements: run after a record group
    /// is durable (native backend), and by `fsync`/`fsyncdir`.
    durability_hook: std::sync::OnceLock<DurabilityHook>,
    /// Where each change the mount makes is kept on disk before it is answered.
    journal: Option<Journaled>,
    /// Whether a changeset is publishing: its changes are announced once, when it is
    /// durable.
    changeset_open: AtomicBool,
    /// A changeset changed the namespace but was not durable: the next durable changeset
    /// announces both.
    announce_held: AtomicBool,

    /// Tells inotify of the changes the kernel did not make; `None` leaves watchers to
    /// poll.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    change_signal: Option<Arc<crate::change_signal::ChangeSignal>>,
    /// The requests the mount is answering, for a process ending to wait for.
    activity: Arc<activity::Activity>,
    #[cfg(test)]
    commit_fault: Mutex<Option<(u64, i32)>>,
    #[cfg(test)]
    commit_pause: Mutex<Option<CommitPause>>,
}

impl QuickFS {
    /// A file system over `namespace` and `hydrator` with access heuristics of its own: for a
    /// mount no prefetch switch or cache policy steers.
    pub fn over(namespace: Arc<Mutex<Namespace>>, hydrator: Arc<Hydrator>) -> Self {
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        Self::new(namespace, hydrator, heuristics)
    }

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
            write_buffers: Arc::new(Mutex::new(HashMap::new())),
            stage_dir: None,
            memory_stage_budget: Arc::new(write_stage::MemoryBudget::default()),
            buffer_states: Mutex::new(HashMap::new()),
            commit_locks: Mutex::new(HashMap::new()),
            unsaved: None,
            kernel_cached: Mutex::new(HashMap::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            passthrough: None,
            owner: None,
            open_file_handles: Mutex::new(HashMap::new()),
            next_file_handle: AtomicU64::new(1),
            document_index_cursors: Mutex::new(HashMap::new()),
            document_index_initial_cursors: Mutex::new(HashMap::new()),
            acl_visible_document_index: Arc::new(Mutex::new(None)),
            document_index_wanted: Arc::new(tokio::sync::Notify::new()),
            readdir_cache: Mutex::new(None),
            rich_metadata: Mutex::new(HashMap::new()),
            enforcement: None,
            thumbs: None,
            availability: None,
            stored_writes: Arc::default(),
            pinned_hydration: None,
            eviction_queue: None,

            actor: None,
            writers: None,
            admission: None,
            written: None,
            durability_hook: std::sync::OnceLock::new(),
            journal: None,
            changeset_open: AtomicBool::new(false),
            announce_held: AtomicBool::new(false),

            #[cfg(any(target_os = "linux", target_os = "macos"))]
            change_signal: None,
            activity: Arc::default(),
            #[cfg(test)]
            commit_fault: Mutex::new(None),
            #[cfg(test)]
            commit_pause: Mutex::new(None),
        }
    }

    /// Keep the opened-together history for `workspace` under `cache_root` across
    /// restarts, and restore what was kept ([`crate::smart::kept_history`]).
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

    pub fn write_buffers_handle(&self) -> Arc<Mutex<WriteStages>> {
        Arc::clone(&self.write_buffers)
    }

    /// Keep written bytes of opaque files in sparse files under `dir` instead of in
    /// memory, so a large write never has to fit in RAM. Stage files a previous run left
    /// are removed. When the directory cannot be prepared, writes keep staging in memory.
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

    /// The requests this mount is answering and when it last took one: what a process about to
    /// end waits on, so it ends no request in the middle.
    pub fn activity(&self) -> Arc<activity::Activity> {
        Arc::clone(&self.activity)
    }

    fn ns(&self) -> NamespaceGuard<'_> {
        NamespaceGuard {
            namespace: std::mem::ManuallyDrop::new(self.namespace.lock_recover()),
            journal: self.journal.as_ref(),
        }
    }

    /// The namespace for a step that only reads. A read never owes a journal entry, and never
    /// takes on the writing of one another step announced: at most it waits for the step in it.
    fn ns_read(&self) -> NamespaceGuard<'_> {
        NamespaceGuard {
            namespace: std::mem::ManuallyDrop::new(self.namespace.lock_recover()),
            journal: None,
        }
    }

    /// Keep each change the mount makes to the namespace with `journal` before the request
    /// that made it is answered ([`ChangeJournal`]).
    #[must_use]
    pub fn with_change_journal(mut self, journal: ChangeJournal) -> Self {
        let changes = self.namespace.lock_recover().subscribe_changes();
        self.journal = Some(Journaled {
            journal,
            namespace: Arc::clone(&self.namespace),
            changes: Mutex::new(changes),
        });
        self
    }

    /// [`Namespace::inode_view`] under one namespace lock, with the path built only when
    /// this mount has an ACL to check it against.
    fn inode_view(&self, ino: u64, unlinked: bool) -> Option<InodeView> {
        let query = InodeQuery {
            unlinked,
            path: self.enforcement.is_some(),
        };
        self.ns_read().inode_view(ino, query)
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

    /// Stage unsaved bytes durably in `store` ([`crate::unsaved`]): bytes whose commit
    /// failed are persisted so an unmount or crash cannot lose them, and `fsync` persists
    /// there instead of minting a version per call.
    #[must_use]
    pub fn with_unsaved_store(mut self, store: crate::unsaved::UnsavedStore) -> Self {
        self.unsaved = Some(Arc::new(store));
        self
    }

    fn unsaved_key(&self, ino: u64) -> (String, Option<fs_protocol::FileDocumentId>) {
        let document_id = self.ns().document_id_for(ino);
        let key = document_id.map_or_else(|| format!("ino-{ino}"), |id| id.0.to_string());
        (key, document_id)
    }

    /// Stage the complete bytes of `stage` (an `fsync`, or a failed commit) in `store`; a
    /// file-backed stage is copied file to file.
    fn stage_unsaved(
        &self,
        store: &crate::unsaved::UnsavedStore,
        ino: u64,
        stage: &mut WriteStage,
        base_hash: Option<Hash>,
        error: Option<String>,
    ) -> io::Result<()> {
        let (key, document_id) = self.unsaved_key(ino);
        let path = self.ns().path_for(ino);
        match stage.source(false)? {
            VersionSource::Bytes(data) => {
                let entry = crate::unsaved::UnsavedEntry::describe(
                    key,
                    document_id,
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
                    document_id,
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
    /// [`PersistIntent::Close`] commits the bytes as a version; on failure the errno is
    /// returned, the stage stays (reads keep seeing it) and the bytes are staged in the
    /// unsaved store, and the next flush, fsync or close retries. [`PersistIntent::Sync`]
    /// only guarantees durability, so a database that syncs every transaction produces
    /// one version per close; without an unsaved store it commits like `Close`.
    ///
    /// Ranges nobody wrote are read from the committed content first, outside the stage
    /// lock; the commit then runs under this inode's stage lock only.
    fn persist_buffer(&self, ino: u64, intent: PersistIntent, adopt: bool) -> Result<(), i32> {
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
        if self.ns().getattr(ino).is_none() {
            return Ok(());
        }
        let workspace = self.inode_workspace(ino);
        if staged.lock().workspace() != workspace {
            return Err(libc::EXDEV);
        }
        if workspace.is_some() {
            self.check_access(ino, Access::Write)?;
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
        if workspace.is_none()
            && intent == PersistIntent::Sync
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
        match self.commit_buffer(ino, &mut guard, adopt) {
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
                let detail = crate::errno::error(errno).to_string();
                match self.unsaved.as_ref().filter(|_| workspace.is_none()) {
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

    #[cfg(test)]
    fn install_stage(&self, ino: u64, bytes: Vec<u8>) {
        let stage =
            WriteStage::complete(&bytes, None, Arc::clone(&self.memory_stage_budget)).unwrap();
        drop(bytes);
        self.buffers().insert(ino, stage.shared());
    }

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

    fn staged_len(&self, ino: u64) -> Option<u64> {
        Some(self.stage(ino)?.len())
    }

    /// A stage for `ino` over its committed content, reading none of it.
    fn new_stage(&self, ino: u64) -> Result<(WriteStage, Option<Hash>), i32> {
        let (base, base_len, path) = {
            let ns = self.ns();
            let base = ns.hash_for(ino);
            let len = ns
                .getattr_including_unlinked(ino)
                .map_or(0, |attr| attr.size);
            (base, len, ns.path_for(ino))
        };
        let workspace = path.and_then(|path| self.hydrator.scoped_workspace(&path));
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
        Ok((stage.with_workspace(workspace), base))
    }

    fn empty_stage(&self) -> WriteStage {
        let memory = || WriteStage::empty(Arc::clone(&self.memory_stage_budget));
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

    fn stage_for_write(&self, ino: u64) -> Result<(SharedStage, bool, Option<Hash>), i32> {
        if let Some(stage) = self.stage(ino) {
            return Ok((stage, false, None));
        }
        let (fresh, base) = self.new_stage(ino)?;
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

    /// Read every range of `stage` nobody wrote from its committed content, so it can be
    /// committed. Bounded pieces, each fetched without the stage lock; a failure leaves
    /// the stage as it was and is the caller's error.
    fn fill_stage(&self, stage: &SharedStage) -> io::Result<()> {
        const PIECE: u64 = 8 * 1024 * 1024;
        let (base, gaps, workspace) = {
            let guard = stage.lock();
            (guard.base(), guard.gaps(), guard.workspace())
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
                let bytes = self.read_base_blocking(base, at, len, workspace)?;
                stage.lock().fill(at, &bytes)?;
                at = at.saturating_add(len);
            }
        }
        Ok(())
    }

    /// Read `ino`'s stage: `None` when it has none. Ranges nobody wrote come from the
    /// committed content, local bytes inline; anything that needs a peer is left in
    /// [`StagedRead::Missing`] for the caller to hydrate without holding the stage.
    #[cfg(test)]
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

    /// `truncate` of a file nobody has open: commit its first `new_size` bytes.
    fn commit_resized(&self, ino: u64, new_size: u64) -> io::Result<()> {
        if new_size == 0 {
            return self.commit_content(ino, &[]);
        }
        let (stage, _) = self.new_stage(ino).map_err(crate::errno::error)?;
        let stage = stage.shared();
        stage.lock().set_len(new_size)?;
        self.fill_stage(&stage)?;
        let mut guard = stage.lock();
        let workspace = guard.workspace();
        let committed = guard
            .source(true)
            .and_then(|source| self.commit_version_at(ino, source, now_ms(), None, workspace));
        drop(guard);
        committed
    }

    /// Keep the bytes of `stage` as a new sibling of `ino` because they cannot be
    /// committed to its own record. Returns the copy's workspace path.
    fn preserve_as_sibling(&self, ino: u64, stage: &mut WriteStage) -> Result<String, i32> {
        if stage.workspace() != self.inode_workspace(ino) {
            return Err(libc::EXDEV);
        }
        let (parent, name) = {
            let mut ns = self.ns();
            let name = ns.file_name_for(ino).ok_or(ENOENT)?;
            (ns.parent_ino(ino).ok_or(ENOENT)?, name)
        };
        let digest = stage.digest().map_err(|_| EIO)?;
        let copy_name = unsaved_copy_name_for(name.as_bytes(), &digest);
        let workspace = stage.workspace();
        let source = stage.source(false).map_err(|_| EIO)?;
        self.create_preserved_copy(parent, &copy_name, source, workspace)
    }

    fn create_preserved_copy(
        &self,
        parent: u64,
        copy_name: &[u8],
        source: VersionSource<'_>,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> Result<String, i32> {
        self.check_new_name(parent, copy_name)?;
        let existing = self.ns().lookup(parent, copy_name).map(|attr| attr.ino.0);
        let ino = if let Some(ino) = existing {
            // The name is derived from the bytes: an earlier attempt that got
            // as far as creating it is finished by committing into it.
            ino
        } else {
            let ino = self.create_empty_file(parent, copy_name)?;
            self.publish();
            ino
        };
        self.commit_version_at(ino, source, now_ms(), None, workspace)
            .map_err(|error| commit_errno(&error))?;
        self.ns().path_for(ino).ok_or(EIO)
    }

    /// Handle the last close of `ino`: a clean buffer is dropped; bytes that can never be
    /// committed to this record become a sibling copy; anything else stays staged for a
    /// later retry.
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
                    crate::errno::error(errno)
                );
                self.clear_unsaved(ino);
                self.drop_buffer(ino);
            }
            Err(copy_errno) => eprintln!(
                "inode {ino} could not take another version and no copy could be created ({}); bytes stay in memory and staged",
                crate::errno::error(copy_errno)
            ),
        }
    }

    pub fn persist_all_buffers(&self) {
        let inos: Vec<u64> = self.buffers().keys().copied().collect();
        for ino in inos {
            let _ = self.persist_buffer(ino, PersistIntent::Close, true);
        }
    }

    /// `write` after its access check: apply `data` at `offset` to the inode's stage. The
    /// committed content is never read: ranges nobody writes stay the base's until a read
    /// or the commit needs them.
    fn write_buffer(&self, ino: u64, offset: u64, data: &[u8]) -> Result<u32, i32> {
        self.write_staged(ino, Some(offset), data)
    }

    /// `write` on a descriptor opened with `O_APPEND`: the bytes land at the
    /// staged end, never at the offset the request carried. The kernel derives
    /// that offset from its cached inode size, which is stale whenever the
    /// content changed under it (a remote edit, a hydration), and a stale
    /// offset would overwrite the file's start instead of appending to it.
    fn append_buffer(&self, ino: u64, data: &[u8]) -> Result<u32, i32> {
        self.write_staged(ino, None, data)
    }

    /// `write` after its access check: a descriptor opened with `O_APPEND`
    /// writes at the staged end, every other descriptor at the offset the
    /// request carried.
    fn write_handle(&self, ino: u64, fh: u64, offset: u64, data: &[u8]) -> Result<u32, i32> {
        if self
            .open_handle(ino, fh)
            .is_some_and(|handle| handle.append)
        {
            self.append_buffer(ino, data)
        } else {
            self.write_buffer(ino, offset, data)
        }
    }

    fn write_staged(&self, ino: u64, position: Option<u64>, data: &[u8]) -> Result<u32, i32> {
        if let Some(offset) = position {
            offset.checked_add(data.len() as u64).ok_or(EINVAL)?;
        }
        loop {
            let (stage, opened, base) = self.stage_for_write(ino)?;
            let mut guard = stage.lock();
            if guard.retired() {
                drop(guard);
                self.replace_retired_stage(ino, &stage)?;
                continue;
            }
            let outcome = match position {
                Some(offset) => guard.write(offset, data),
                None => guard.append(data),
            };
            outcome.map_err(|error| crate::errno::code(&error))?;
            stage.note_len(&guard);
            // The generation moves under the stage lock, so a commit's
            // generation always describes the bytes it committed.
            self.note_buffer_mutation(ino, opened, base);
            drop(guard);
            return Ok(u32::try_from(data.len()).unwrap_or(u32::MAX));
        }
    }

    /// Give an inode a stage of its own when a commit made its file a content entry: the old
    /// stage still reads the committed bytes, but a write opens a stage over them rather than
    /// change bytes a version is named by.
    fn replace_retired_stage(&self, ino: u64, retired: &SharedStage) -> Result<(), i32> {
        let (base, len) = {
            let ns = self.ns();
            (
                ns.hash_for(ino),
                ns.getattr_including_unlinked(ino)
                    .map_or(0, |attr| attr.size),
            )
        };
        let fresh = WriteStage::over(
            base,
            len,
            self.stage_dir.as_deref(),
            Arc::clone(&self.memory_stage_budget),
        )
        .map_err(|error| {
            eprintln!(
                "inode {ino}: could not open a write stage over its committed content: {error}"
            );
            EIO
        })?;
        let fresh = fresh.shared();
        {
            let mut stages = self.buffers();
            if stages
                .get(&ino)
                .is_some_and(|stage| Arc::ptr_eq(stage, retired))
            {
                stages.insert(ino, fresh);
            }
        }
        Ok(())
    }

    /// `setattr(size)` after its access check. A file that is open stages it; any other
    /// commits the new length immediately.
    fn truncate_inode(&self, ino: u64, new_size: u64) -> Result<(), i32> {
        let open = self.buffers().contains_key(&ino);
        if !open {
            return self
                .commit_resized(ino, new_size)
                .map_err(|error| commit_errno(&error));
        }
        loop {
            let (stage, opened, base) = self.stage_for_write(ino)?;
            let mut guard = stage.lock();
            if guard.retired() {
                drop(guard);
                self.replace_retired_stage(ino, &stage)?;
                continue;
            }
            guard
                .set_len(new_size)
                .map_err(|error| crate::errno::code(&error))?;
            stage.note_len(&guard);
            self.note_buffer_mutation(ino, opened, base);
            drop(guard);
            return Ok(());
        }
    }

    /// Create the file `name` in `parent` with `mode` as a FUSE `create` does, open for
    /// writing with an empty stage. Returns its inode.
    fn create_for_write(&self, parent: u64, name: &[u8], mode: u32) -> Result<u64, i32> {
        let ino = self.create_empty_file(parent, name)?;
        self.set_mode(ino, mode)?;
        let stage = self
            .empty_stage()
            .with_workspace(self.inode_workspace(ino))
            .shared();
        self.buffers().insert(ino, stage);
        self.buffer_states().insert(
            ino,
            BufferState {
                committed: Some(0),
                ..BufferState::default()
            },
        );
        Ok(ino)
    }

    /// What a FUSE `getattr` answers for `ino` (through kernel handle `fh`, when it has
    /// one): its attributes. The inode is read under one namespace lock; a directory's
    /// visible link count is the only further read.
    fn attr_for_request(&self, ino: u64, fh: Option<u64>) -> Result<FileAttr, i32> {
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
        Ok(self.live_attr(view.attr, buf_len))
    }

    /// What a FUSE `lookup` answers for `name` in `parent`: the entry's attributes and
    /// TTL, or `None` when there is no such entry in the caller's view.
    fn lookup_entry(&self, parent: u64, name: &[u8]) -> Option<(FileAttr, Duration)> {
        let (found, child) = {
            let mut ns = self.ns();
            let found = ns.lookup(parent, name);
            let child = ns.path_for(parent).map(|p| child_path(&p, name));
            drop(ns);
            (found, child)
        };
        let attr = found.filter(|_| child.is_some_and(|c| self.in_view(&c)))?;
        let staged_len = self.staged_len(attr.ino.0);
        Some((self.live_attr(attr, staged_len), TTL))
    }

    fn flush_inode(&self, ino: u64) -> Result<(), i32> {
        self.persist_buffer(ino, PersistIntent::Close, true)
    }

    /// `fsync`: make the buffer durable, and the name it is reached by.
    ///
    /// Staged bytes alone would come back after a crash only as a sibling copy when the
    /// namespace on disk does not place the file yet: a file created, written and synced
    /// within the persister's settle time (`namespace_persist::QUIET`). So the namespace
    /// is made durable too.
    fn fsync_inode(&self, ino: u64) -> Result<(), i32> {
        self.persist_buffer(ino, PersistIntent::Sync, false)?;
        self.sync_namespace()
    }

    /// Make the namespace on disk hold every placement made so far: what `fsync` of a
    /// directory promises for the names in it. Without a hook there is nothing to make
    /// durable.
    fn sync_namespace(&self) -> Result<(), i32> {
        let Some(hook) = self.durability_hook.get() else {
            return Ok(());
        };
        hook().map_err(|error| {
            eprintln!("the namespace could not be made durable: {error}");
            EIO
        })
    }

    pub fn set_durability_hook(&self, hook: DurabilityHook) {
        let _ = self.durability_hook.set(hook);
    }

    /// Make the mount's writes durable on a window rather than each on its own. `content`'s
    /// commits stop waiting for the disk: its log of them keeps each for a process killed before
    /// the next sync, so a closed file is kept the moment `close` answers. Every
    /// [`SYNC_WINDOW`] while anything was written, and on `fsync`, `fsyncdir` and unmount, one
    /// barrier syncs the content first and then `names`, the journals that name it, so a power
    /// loss never leaves a name whose bytes are missing. A checkout of thousands of files pays
    /// one sync per window instead of one per file.
    ///
    /// The window ends with the mount. Must be called on a tokio runtime.
    pub fn sync_on_window(&self, content: Arc<crate::ContentStore>, names: Option<DurabilityHook>) {
        content.set_sync_writes(false);
        let barrier: DurabilityHook = Arc::new(move || {
            content.sync_pending()?;
            if let Some(names) = &names {
                names()?;
            }
            Ok(())
        });
        let window = Arc::downgrade(&barrier);
        self.set_durability_hook(barrier);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SYNC_WINDOW).await;
                let Some(barrier) = window.upgrade() else {
                    break;
                };
                let error = match tokio::task::spawn_blocking(move || barrier()).await {
                    Ok(Ok(())) => continue,
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => error.to_string(),
                };
                eprintln!("the workspace's writes could not be synced: {error}");
            }
        });
    }

    /// `release` of kernel handle `fh`: commit a writable handle's bytes if `flush` did
    /// not already, then settle the buffer on the last close.
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
            self.persist_buffer(ino, PersistIntent::Close, true)
        } else {
            Ok(())
        };
        if last_handle {
            self.settle_last_close(ino, live);
        }
        result
    }

    /// Present every file on the mount as owned by `uid:gid`, the local person whose files
    /// they are. Without one, each caller sees the files as their own: the mount acts for
    /// every caller as the one actor it serves. Ownership is only presented: it is not
    /// replicated, nothing is chowned, and access is the capability list's to decide.
    #[must_use]
    pub fn with_owner(mut self, owner: Option<(u32, u32)>) -> Self {
        self.owner = owner;
        self
    }

    /// Offer kernel passthrough for read-only opens of fully local files, with backing
    /// files under `dir`. Takes effect only when the kernel negotiates it at `init`.
    #[must_use]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn with_passthrough(mut self, dir: std::path::PathBuf) -> Self {
        self.passthrough = Some(Passthrough {
            dir,
            kernel_ready: std::sync::atomic::AtomicBool::new(false),
            disabled: std::sync::atomic::AtomicBool::new(false),
            backing: Mutex::new(HashMap::new()),
        });
        self
    }

    fn payload_not_resident(&self, ino: u64) -> bool {
        let Some(registry) = &self.availability else {
            return false;
        };
        let Some(document_id) = self.ns().document_id_for(ino) else {
            return false;
        };
        registry
            .lock_recover()
            .get(document_id)
            .is_some_and(|record| !matches!(record.residency, Residency::Resident { .. }))
    }

    /// `FOPEN_KEEP_CACHE` when the kernel's page cache for `ino` was filled from the content
    /// the file still has; otherwise no flag, which makes the kernel drop the cache.
    /// Buffered inodes never keep it, and neither do files whose payload is
    /// not resident, so "Free Local Space" also frees the kernel's copy.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn open_cache_flags(&self, ino: u64) -> FopenFlags {
        if self.inode_workspace(ino).is_some() {
            self.kernel_cached.lock_recover().remove(&ino);
            return FopenFlags::FOPEN_DIRECT_IO;
        }
        let volatile = self.payload_not_resident(ino) || self.buffers().contains_key(&ino);
        let hash = self.ns_read().hash_for(ino);
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

    /// A read-only backing file holding exactly the current content of `ino`, when
    /// passthrough is usable and every chunk is local.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn passthrough_backing(&self, ino: u64) -> Option<std::fs::File> {
        let passthrough = self.passthrough.as_ref().filter(|p| p.usable())?;
        if self.payload_not_resident(ino) || self.buffers().contains_key(&ino) {
            return None;
        }
        // A workspace read must go through live policy on each request.
        if self.inode_workspace(ino).is_some() {
            return None;
        }
        let hash = self.ns().hash_for(ino)?;
        if self
            .hydrator
            .block_on(self.hydrator.local_manifest_index(hash))
            .is_some_and(|manifest| manifest.sealed().is_some())
        {
            return None;
        }
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

    /// The hydration state of `ino`, served for the `user.tilo.sync` xattr. `None` when the
    /// inode does not exist. Directories and empty files are always
    /// [`SyncState::Local`]; a regular file's state comes from the hydrator.
    fn sync_state_for(&self, ino: u64) -> Option<fs_protocol::SyncState> {
        use fs_protocol::SyncState;
        let (exists, is_dir, hash) = {
            let ns = self.ns_read();
            let attr = ns.getattr(ino);
            let exists = attr.is_some();
            let is_dir = matches!(attr, Some(a) if a.kind == FileType::Directory);
            let hash = ns.hash_for(ino);
            drop(ns);
            (exists, is_dir, hash)
        };
        if !exists {
            return None;
        }
        if self.buffers().contains_key(&ino) {
            return Some(SyncState::Local);
        }
        if is_dir {
            return Some(SyncState::Local);
        }
        let Some(hash) = hash else {
            return Some(SyncState::Local);
        };
        Some(self.hydrator.block_on(self.hydrator.sync_state(hash)))
    }

    /// Commit a complete stage: ingest it as chunked content (a file-backed stage straight
    /// from its file). With `adopt`, a stage file becomes the content entry itself and the
    /// stage is retired: reads still answer from it, writes open a stage of their own.
    /// Returns the errno the writer should see when the bytes did not become the file's
    /// committed content.
    fn commit_buffer(&self, ino: u64, stage: &mut WriteStage, adopt: bool) -> Result<(), i32> {
        let tracing = crate::hydration::tracing_writes();
        let began = std::time::Instant::now();
        let workspace = stage.workspace();
        let previous = self
            .version_base(ino, workspace)
            .map_err(|error| commit_errno(&error))?;
        let source = stage
            .source(adopt && stage.is_file_backed())
            .map_err(|error| commit_errno(&error))?;
        let (stored, adopted) = self
            .store_version_blocking(source, previous, workspace)
            .map_err(|error| {
                eprintln!("storing a version of inode {ino} failed: {error}");
                commit_errno(&error)
            })?;
        let stored_at = began.elapsed();
        if adopted {
            stage.retire();
        }
        // The bytes are stored once; only the namespace step is retried when a revision moved
        // under it, since the stage's file has belonged to the store since it was moved.
        Self::commit_retrying(ino, || {
            self.install_version(ino, stored, now_ms(), None, workspace)
        })?;
        if tracing {
            eprintln!(
                "commit of inode {ino}: its bytes in {stored_at:?}, its name in {:?}",
                began.elapsed().saturating_sub(stored_at)
            );
        }
        self.remember_staged_metadata(ino, stage);
        Ok(())
    }

    /// Run `commit` until it succeeds or fails for good. `EBUSY` means the committed revision
    /// moved under the commit (a remote replay or a concurrent local commit); the bytes
    /// still have to land, so it is tried again against the new revision.
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

    fn remember_staged_metadata(&self, ino: u64, stage: &WriteStage) {
        let (document_id, name, hash) = {
            let ns = self.ns();
            let Some(document_id) = ns.document_id_for(ino) else {
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
            (document_id, name, hash)
        };
        if !crate::rich_metadata::recognizes(&name) {
            return;
        }
        let Ok(prefix) = stage.prefix(crate::rich_metadata::MAX_METADATA_SCAN_BYTES as u64) else {
            return;
        };
        if let Some(properties) = crate::rich_metadata::extract(&name, &prefix) {
            self.rich_metadata.lock_recover().insert(
                document_id,
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

    fn remember_rich_metadata_as(
        &self,
        ino: u64,
        bytes: &[u8],
        hash: fs_protocol::ContentHash,
    ) -> Option<fs_protocol::FilePropertiesV1> {
        let (document_id, name) = {
            let ns = self.ns();
            let document_id = ns.document_id_for(ino)?;
            let name = ns.path_for(ino)?.rsplit('/').next()?.to_owned();
            drop(ns);
            (document_id, name)
        };
        let properties = crate::rich_metadata::extract(&name, bytes)?;
        self.rich_metadata
            .lock_recover()
            .insert(document_id, (hash, properties.clone()));
        Some(properties)
    }

    fn start_document_index_acl_worker(&self) {
        let Some((acl, identity)) = self.enforcement.clone() else {
            return;
        };
        self.start_document_index_refresh_worker(acl.subscribe_revision(), (acl, identity));
    }

    fn start_document_index_refresh_worker(
        &self,
        mut generations: tokio::sync::watch::Receiver<u64>,
        enforcement: (AclHandle, crate::acl::IdentityKey),
    ) {
        let namespace = Arc::clone(&self.namespace);
        let retained = Arc::downgrade(&self.acl_visible_document_index);
        let wanted = Arc::clone(&self.document_index_wanted);
        drop(self.hydrator.runtime_handle.spawn(async move {
            loop {
                tokio::select! {
                    changed = generations.changed() => if changed.is_err() { break },
                    () = wanted.notified() => {}
                }
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

    /// Attach a host-side thumbnail provider (cache under `dir`). The mount then answers
    /// the `user.tilo.thumb` xattr with a generated preview and records each thumbnail's
    /// hash on the namespace node so it projects to peers.
    #[must_use]
    pub fn with_thumbs(mut self, provider: crate::thumbs::ThumbProvider) -> Self {
        self.thumbs = Some(provider);
        self
    }

    #[must_use]
    pub fn with_availability(mut self, registry: Arc<Mutex<AvailabilityRegistry>>) -> Self {
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

    /// Start the mount-lifetime worker that turns durable pin policy into verified local
    /// residency. Call this after attaching the live ACL, so the worker never hydrates
    /// records outside this mount's current view.
    #[must_use]
    pub fn with_pinned_hydration(self) -> Self {
        self.with_pinned_hydration_every(PINNED_HYDRATION_BACKSTOP)
    }

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
                // Only a local wake asks for a pass outright. Anything else acts when the
                // namespace, ACL or registry moved since the last pass, or a backoff fell
                // due.
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

    /// Answer and accept `signal`'s attribute, and tell it which directories the kernel
    /// lists; its worker raises the signals once the mount is up
    /// ([`crate::change_signal`]).
    #[must_use]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn with_change_signal(mut self, signal: Arc<crate::change_signal::ChangeSignal>) -> Self {
        self.change_signal = Some(signal);
        self
    }

    /// Write as `actor`: the person a writer acts for, unless the writer names another.
    #[must_use]
    pub fn with_actor(mut self, actor: fs_protocol::FileActor) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Name who writes from the process each request comes from: an agent session, a
    /// Tilo app, another program, or the person ([`crate::writer`]).
    #[must_use]
    pub fn with_writers(mut self, writers: Arc<crate::writer::WriterResolver>) -> Self {
        self.writers = Some(writers);
        self
    }

    /// Admit every file action this filesystem writes as the workspace decides it: refused
    /// ones answer `EROFS` ([`crate::admission`]).
    #[must_use]
    pub fn with_admission(mut self, admission: Arc<crate::admission::FileAdmission>) -> Self {
        self.admission = Some(admission);
        self
    }

    /// Tell `written` the content of every version written through this filesystem, the moment
    /// it is the file's: what the workspace's other devices have only once this device hands it
    /// to them.
    #[must_use]
    pub fn with_written_content(mut self, written: Arc<WrittenContent>) -> Self {
        self.written = Some(written);
        self
    }

    /// `hash` is the content of a version written here now: tell whoever
    /// [`Self::with_written_content`] names.
    fn wrote_content(&self, hash: Option<Hash>) {
        if let (Some(written), Some(hash)) = (&self.written, hash) {
            written(hash);
        }
    }

    /// Who process `pid` writes for.
    fn writing(&self, pid: u32) -> Writing {
        let Some(writers) = &self.writers else {
            return Writing {
                resolved: false,
                invoker: None,
            };
        };
        let person = self.actor.as_ref().map(|actor| hex::encode(actor.person.0));
        Writing {
            resolved: true,
            invoker: writers.resolve(pid).invoker(person.as_deref()),
        }
    }

    /// Note who process `pid` writes for, for the rest of this request on this thread: the
    /// file actions it makes are done by them.
    fn note_writer(&self, pid: u32) -> WriterNote {
        let _timing = OpTimer::new("writer-note");
        self.writing(pid).noted()
    }

    /// [`Self::note_writer`], once the workspace admits the file action `verb` from that
    /// writer on the node `ino` is (on the tree, for a node not made yet).
    ///
    /// # Errors
    /// `EROFS` when the workspace refuses it: what no holder of the workspace would take is
    /// read-only here.
    fn admit_writer(&self, pid: u32, ino: Option<u64>, verb: &str) -> Result<WriterNote, i32> {
        let _timing = OpTimer::new("admit");
        let writing = self.writing(pid);
        self.admit_as(writing.invoker.as_ref(), ino, verb)?;
        Ok(writing.noted())
    }

    /// [`Self::admit_writer`] for the node named `name` in the folder `parent`, when there is
    /// one: what it is is the caller's to answer.
    fn admit_named(
        &self,
        pid: u32,
        parent: u64,
        name: &[u8],
        verb: &str,
    ) -> Result<WriterNote, i32> {
        let ino = self.ns().lookup(parent, name).map(|attr| attr.ino.0);
        self.admit_writer(pid, ino, verb)
    }

    /// Whether the workspace admits the file action `verb` on `ino` by `invoker` (the person
    /// at this device when `None`). Admitted without an admission attached.
    fn admit_as(
        &self,
        invoker: Option<&tilo_actions::ActionInvoker>,
        ino: Option<u64>,
        verb: &str,
    ) -> Result<(), i32> {
        let Some(admission) = &self.admission else {
            return Ok(());
        };
        let node = ino.and_then(|ino| self.ns().id_for_ino(ino).map(str::to_owned));
        if admission.admits(
            invoker.map(|invoker| &invoker.acting),
            node.as_deref(),
            verb,
        ) {
            Ok(())
        } else {
            Err(EROFS)
        }
    }

    /// The metadata projection of `ino`'s current content: the rich facts this device
    /// extracted from exactly those bytes.
    fn metadata_projection_for(&self, ino: u64) -> Option<Vec<u8>> {
        let (document_id, hash) = {
            let namespace = self.ns();
            (
                namespace.document_id_for(ino)?,
                namespace
                    .hash_for(ino)
                    .map(|hash| fs_protocol::ContentHash(*hash.as_bytes())),
            )
        };
        let properties = self
            .rich_metadata
            .lock_recover()
            .get(&document_id)
            .filter(|(properties_hash, _)| Some(*properties_hash) == hash)
            .map(|(_, properties)| properties.clone())?;
        fs_protocol::MetadataProjectionV1 {
            schema_version: fs_protocol::projection::METADATA_PROJECTION_VERSION,
            document_id,
            properties,
        }
        .to_xattr_bytes()
        .ok()
    }

    /// Tell the availability record of `ino`, when this mount keeps one, that its content is
    /// now `hash`.
    fn note_current_content(&self, ino: u64, hash: Option<Hash>) {
        let Some(registry) = &self.availability else {
            return;
        };
        let Some(document_id) = self.ns().document_id_for(ino) else {
            return;
        };
        registry.lock_recover().update_current_content(
            document_id,
            hash.map(|hash| fs_protocol::ContentHash(*hash.as_bytes())),
        );
    }

    /// The namespace owns the record and path index. This only fingerprints its current
    /// CRDT heads plus the live ACL generation; it does not enumerate records. Domain and
    /// length framing prevent an ambiguous concatenation from producing an equivalent
    /// cursor generation.
    fn document_catalog_revision(&self) -> String {
        let namespace = self.ns();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        Self::document_catalog_revision_from_index(
            namespace.indexed_document_revision(),
            acl_revision,
        )
    }

    fn document_catalog_revision_from_index(index_revision: &str, acl_revision: u64) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v2.revision\\0");
        let index_revision = index_revision.as_bytes();
        hasher.update(&(index_revision.len() as u64).to_be_bytes());
        hasher.update(index_revision);
        hasher.update(b"acl-revision\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Return a retained projection by cheap shared reference. Serving an xattr never rebuilds
    /// it, and neither does a change of the tree: rebuilding copies the whole catalogue, so a
    /// copy of many files that rebuilt it for each would cost the folder's size per file. A
    /// view taken before the tree last changed is still a whole, coherent catalogue under the
    /// current ACL: it is served, and the refresh worker is asked for the current one, which
    /// the next request sees. A view of an older ACL is never served: its verdicts may show
    /// what is hidden now.
    fn acl_visible_document_index(&self) -> Option<Arc<AclVisibleDocumentIndex>> {
        let source_revision = self.document_catalog_revision();
        let acl_revision = self
            .enforcement
            .as_ref()
            .map_or(0, |(acl, _)| acl.revision());
        let index = self
            .acl_visible_document_index
            .lock_recover()
            .as_ref()
            .filter(|index| index.acl_revision == acl_revision)
            .cloned();
        if index
            .as_ref()
            .is_none_or(|index| index.source_revision != source_revision)
        {
            self.document_index_wanted.notify_one();
        }
        index
    }

    /// Rebuild the retained authority view under one coherent Namespace lock. Call only
    /// from mount, mutation and sync notification paths, never from an xattr request; it
    /// replaces the snapshot every subsequent bounded page uses.
    fn refresh_acl_visible_document_index(&self) {
        Self::refresh_acl_visible_record_index_into(
            &self.namespace,
            self.enforcement.as_ref(),
            &self.acl_visible_document_index,
        );
    }

    /// Rebuild the retained view when the namespace catalogue or the ACL moved. The
    /// Namespace lock covers only copying the catalogue rows; the ACL is evaluated after it
    /// is released, and under an unchanged ACL revision only for rows whose path the
    /// previous view did not judge.
    fn refresh_acl_visible_record_index_into(
        namespace: &Arc<Mutex<Namespace>>,
        enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
        retained: &Arc<Mutex<Option<Arc<AclVisibleDocumentIndex>>>>,
    ) {
        let acl_revision = enforcement.map_or(0, |(acl, _)| acl.revision());
        let previous = retained.lock_recover().clone();
        let (source_revision, rows) = {
            let namespace = namespace.lock_recover();
            let source_revision = Self::document_catalog_revision_from_index(
                namespace.indexed_document_revision(),
                acl_revision,
            );
            if previous.as_ref().is_some_and(|index| {
                index.source_revision == source_revision && index.acl_revision == acl_revision
            }) {
                return;
            }
            let mut rows = Vec::with_capacity(namespace.indexed_document_count());
            loop {
                let page = namespace.indexed_document_page(rows.len(), DOCUMENT_INDEX_PAGE_ENTRIES);
                if page.is_empty() {
                    break;
                }
                // Each row's path is derived here, where the tree is held, for the rows one
                // page shows; opening a workspace built no path at all.
                let paths: Vec<Option<String>> = page
                    .iter()
                    .map(|entry| namespace.path_for_document(entry))
                    .collect();
                rows.extend(page.into_iter().zip(paths));
            }
            drop(namespace);
            (source_revision, rows)
        };
        let reusable = previous.filter(|index| index.acl_revision == acl_revision);
        #[cfg(test)]
        let mut acl_evaluations = 0_usize;
        let mut hidden = HashMap::new();
        let mut records = Vec::with_capacity(rows.len());
        for (entry, path) in rows {
            let Some(path) = path else { continue };
            let visible = reusable
                .as_deref()
                .and_then(|index| index.verdict(entry.document_id, &path))
                .unwrap_or_else(|| {
                    #[cfg(test)]
                    {
                        acl_evaluations = acl_evaluations.saturating_add(1);
                    }
                    enforcement.is_none_or(|(acl, identity)| acl.visible(identity, &path))
                });
            if visible {
                records.push(DocumentIndexEntryV1 {
                    document_id: entry.document_id,
                    path,
                    is_dir: entry.is_dir,
                });
            } else {
                hidden.insert(entry.document_id, path);
            }
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.record-index.v3.visible-catalogue\\0");
        hasher.update(&acl_revision.to_be_bytes());
        hasher.update(&(records.len() as u64).to_be_bytes());
        for record in &records {
            hasher.update(record.document_id.0.as_bytes());
            hasher.update(&(record.path.len() as u64).to_be_bytes());
            hasher.update(record.path.as_bytes());
            hasher.update(&[u8::from(record.is_dir)]);
        }
        let revision = hasher.finalize().to_hex().to_string();
        let by_id = records
            .iter()
            .cloned()
            .map(|record| (record.document_id, record))
            .collect();
        let index = Arc::new(AclVisibleDocumentIndex {
            source_revision,
            acl_revision,
            revision,
            records,
            by_id,
            hidden,
            #[cfg(test)]
            acl_evaluations,
        });
        // A view a change of the tree overtook while it was built is still whole, and newer
        // than the one it replaces; one whose verdicts may mix two ACL generations is not: the
        // revision bump has its own refresh coming.
        if enforcement.map_or(0, |(acl, _)| acl.revision()) != acl_revision {
            return;
        }
        let mut retained = retained.lock_recover();
        *retained = Some(index);
    }

    fn document_index_manifest_v2(&self) -> Option<Vec<u8>> {
        let revision = self.document_catalog_revision();
        let records = self.ns().indexed_document_count();
        let manifest = fs_protocol::projection::DocumentIndexManifestV2 {
            schema_version: DOCUMENT_INDEX_V2_VERSION,
            revision,
            // A bounded-cursor ceiling, not an ACL-visible count: pages apply ACL filtering
            // locally and may be empty, and a visible count would need an unbounded
            // denied-row scan.
            records,
            page_entries: DOCUMENT_INDEX_PAGE_ENTRIES,
        };
        manifest.validate().ok()?;
        serde_json::to_vec(&manifest).ok()
    }

    fn document_index_page_v2(&self, revision: &str, offset: usize) -> Option<Vec<u8>> {
        (self.document_catalog_revision() == revision).then_some(())?;
        let page_entries = DOCUMENT_INDEX_PAGE_ENTRIES;
        let namespace = self.ns();
        let records = namespace.indexed_document_count();
        if offset > records || !offset.is_multiple_of(page_entries) {
            return None;
        }
        let end = offset.saturating_add(page_entries).min(records);
        // The rows' paths are derived under the tree's lock, which is let go before the page is
        // built; the pairs are what crosses that boundary.
        #[allow(
            clippy::needless_collect,
            reason = "the paths are derived under the lock the page is built without"
        )]
        let candidates: Vec<(IndexedDocument, Option<String>)> = namespace
            .indexed_document_page(offset, page_entries)
            .into_iter()
            .map(|entry| {
                let path = namespace.path_for_document(&entry);
                (entry, path)
            })
            .collect();
        drop(namespace);
        let page = fs_protocol::projection::DocumentIndexPageV2 {
            schema_version: DOCUMENT_INDEX_V2_VERSION,
            revision: revision.to_string(),
            offset,
            next_offset: (end < records).then_some(end),
            records: candidates
                .into_iter()
                .filter_map(|(entry, path)| {
                    let path = path?;
                    self.in_view(&path).then_some(DocumentIndexEntryV1 {
                        document_id: entry.document_id,
                        path,
                        is_dir: entry.is_dir,
                    })
                })
                .collect(),
        };
        page.validate().ok()?;
        let bytes = serde_json::to_vec(&page).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_DOCUMENT_INDEX_PAGE_BYTES).then_some(bytes)
    }

    fn document_index_cursor(
        &self,
        revision: String,
        source_revision: String,
        acl_revision: u64,
        offset: usize,
    ) -> String {
        let cursor = uuid::Uuid::new_v4().to_string();
        let mut cursors = self.document_index_cursors.lock_recover();
        if cursors.len() >= 1024
            && let Some(evicted) = cursors.keys().next().cloned()
        {
            cursors.remove(&evicted);
        }
        cursors.insert(
            cursor.clone(),
            DocumentIndexCursor {
                revision,
                source_revision,
                acl_revision,
                offset,
            },
        );
        cursor
    }

    fn document_index_manifest_v3(&self) -> Option<Vec<u8>> {
        let index = self.acl_visible_document_index()?;
        // Initial issuance is stable for an unchanged authority view. A hidden source change
        // keeps both the public revision and this opaque token stable, so polling cannot
        // correlate hidden activity.
        let key = (index.revision.clone(), String::new(), index.acl_revision);
        let initial_cursor = {
            let initials = self.document_index_initial_cursors.lock_recover();
            initials.get(&key).cloned()
        }
        .unwrap_or_else(|| {
            let cursor = self.document_index_cursor(
                index.revision.clone(),
                index.source_revision.clone(),
                index.acl_revision,
                0,
            );
            let mut initials = self.document_index_initial_cursors.lock_recover();
            if initials.len() >= 128
                && let Some(evicted) = initials.keys().next().cloned()
            {
                initials.remove(&evicted);
            }
            initials.insert(key, cursor.clone());
            cursor
        });
        let manifest = fs_protocol::projection::DocumentIndexManifestV3 {
            schema_version: 3,
            initial_cursor,
            revision: index.revision.clone(),
            page_entries: DOCUMENT_INDEX_PAGE_ENTRIES,
        };
        manifest.validate().ok()?;
        serde_json::to_vec(&manifest).ok()
    }

    fn document_index_page_v3(&self, revision: &str, cursor: &str) -> Option<Vec<u8>> {
        let (source_revision, acl_revision, offset) = self
            .document_index_cursors
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
        let index = self.acl_visible_document_index()?;
        // A changed visible projection (or any ACL replacement) fails closed; a hidden-only
        // source update leaves an equivalent view traversable under the same capability.
        (index.revision == revision && index.acl_revision == acl_revision).then_some(())?;
        let records = index.records.len();
        let end = offset
            .saturating_add(DOCUMENT_INDEX_PAGE_ENTRIES)
            .min(records);
        let candidates = index.records.get(offset..end)?.to_vec();
        let next_cursor = (end < records).then(|| {
            self.document_index_cursor(revision.to_string(), source_revision, acl_revision, end)
        });
        let page = fs_protocol::projection::DocumentIndexPageV3 {
            schema_version: 3,
            revision: revision.to_string(),
            next_cursor,
            records: candidates,
        };
        page.validate().ok()?;
        let bytes = serde_json::to_vec(&page).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_DOCUMENT_INDEX_PAGE_BYTES).then_some(bytes)
    }

    fn document_index_by_id_v2(&self, document_id: fs_protocol::FileDocumentId) -> Option<Vec<u8>> {
        let index = self.acl_visible_document_index()?;
        // One document is looked up in the tree itself when the view is behind it: a file made
        // a moment ago is found by its id at once.
        let entry = if index.source_revision == self.document_catalog_revision() {
            index.by_id.get(&document_id)?.clone()
        } else {
            self.visible_document(document_id)?
        };
        let projection = fs_protocol::projection::DocumentIndexExactV2 {
            schema_version: DOCUMENT_INDEX_V2_VERSION,
            revision: index.revision.clone(),
            record: entry,
        };
        projection.validate().ok()?;
        let bytes = serde_json::to_vec(&projection).ok()?;
        (bytes.len() <= fs_protocol::projection::MAX_DOCUMENT_INDEX_PAGE_BYTES).then_some(bytes)
    }

    /// The catalogue row of `document_id` as the tree has it now, if the ACL shows it.
    fn visible_document(&self, id: fs_protocol::FileDocumentId) -> Option<DocumentIndexEntryV1> {
        let ns = self.ns();
        let (is_dir, path) = ns
            .indexed_document(id)
            .and_then(|entry| Some((entry.is_dir, ns.path_for_document(&entry)?)))?;
        drop(ns);
        self.in_view(&path).then_some(DocumentIndexEntryV1 {
            document_id: id,
            path,
            is_dir,
        })
    }

    /// Read a bounded immutable version for an app-native history preview. Never changes
    /// heads, and is addressable only through a version id the caller's history projection
    /// already exposed.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "namespace and availability snapshots must describe one coherent record revision"
    )]
    fn availability_for(&self, ino: u64) -> Option<Vec<u8>> {
        let registry = self.availability.as_ref()?;
        let (document_id, kind, parent, size, ancestry) = {
            let mut ns = self.ns();
            let document_id = ns.document_id_for(ino)?;
            let attr = ns.getattr(ino)?;
            let kind = if attr.kind == FileType::Directory {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            let parent = ns
                .parent_ino(ino)
                .and_then(|parent_ino| ns.document_id_for(parent_ino));
            let ancestry = document_ancestry(&mut ns, ino)?;
            (document_id, kind, parent, attr.size, ancestry)
        };
        let (mut record, effective_policy) = {
            let registry = registry.lock_recover();
            let record = registry
                .get(document_id)
                .cloned()
                .unwrap_or_else(|| DocumentAvailability::new(document_id, kind));
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
                let current_id = ns.document_id_for(current_ino).ok_or(ENOENT)?;
                let attr = ns.getattr(current_ino).ok_or(ENOENT)?;
                let kind = if attr.kind == FileType::Directory {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                let parent_ino = ns.parent_ino(current_ino);
                let parent = parent_ino.and_then(|parent_ino| ns.document_id_for(parent_ino));
                ancestry.push((current_id, kind, parent));
                cursor = parent_ino;
            }
            drop(ns);
            ancestry
        };
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
        let (document_id, hash, size) = {
            let namespace = self.ns();
            let document_id = namespace.document_id_for(ino).ok_or(ENOENT)?;
            let hash = namespace.hash_for(ino);
            let size = namespace.getattr(ino).ok_or(ENOENT)?.size;
            (document_id, hash, size)
        };
        let scheduled_by_pin_worker = {
            let mut registry = registry.lock_recover();
            let mut record = registry
                .get(document_id)
                .cloned()
                .unwrap_or_else(|| DocumentAvailability::new(document_id, EntryKind::File));
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
                    registry.effective_policy(document_id),
                    Ok(Some(AvailabilityPolicy::Pinned))
                )
        };
        // A pinned retry joins the mount worker: spawning here would bypass its bound and
        // could race the same record with an automatic pass.
        if scheduled_by_pin_worker {
            self.wake_pinned_hydration();
            return Ok(());
        }
        let hash = hash.ok_or(EIO)?;

        let hydrator = self.hydrator.clone();
        self.hydrator.runtime_handle.spawn(async move {
            update_retry_state(&registry, document_id, |record| {
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
                        update_retry_state(&registry, document_id, |record| {
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
                Ok(true) => update_retry_state(&registry, document_id, |record| {
                    record.residency = Residency::Resident { bytes: size };
                    record.transfer.phase = TransferPhase::Idle;
                    record.transfer.progress = None;
                    record.transfer.last_error = None;
                    record.transfer.retry_count = 0;
                    record.transfer.retry_at_ms = None;
                    record.freshness.local_verified_at_ms = Some(now_ms());
                }),
                Ok(false) => update_retry_state(&registry, document_id, |record| {
                    record.transfer.phase = TransferPhase::Idle;
                    record.transfer.progress = None;
                    record.transfer.last_error = Some(crate::availability::TransferError {
                        message: "The file exceeds this device's retry limit".into(),
                        occurred_at_ms: now_ms(),
                    });
                    record.transfer.retry_count = 1;
                    record.transfer.retry_at_ms = None;
                }),
                Err(_) => update_retry_state(&registry, document_id, |record| {
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

    /// Queue a policy-authorized removal of this file's local payload. Residency remains
    /// local until an exact store observation proves every requested blob absent.
    fn free_local_space(&self, ino: u64, mode: EvictionMode) -> Result<(), i32> {
        self.check_access(ino, Access::Read)?;
        let document_id = self.ns().document_id_for(ino).ok_or(ENOENT)?;
        let request_id = self.prepare_eviction_request(document_id, mode, None)?;
        self.spawn_eviction_observer(document_id, request_id)?;
        Ok(())
    }

    fn prepare_eviction_request(
        &self,
        document_id: fs_protocol::FileDocumentId,
        mode: EvictionMode,
        existing_request: Option<EvictionRequestId>,
    ) -> Result<EvictionRequestId, i32> {
        let coordinator = self.eviction_coordinator()?;
        self.hydrator.block_on(async {
            coordinator
                .prepare(document_id, mode, existing_request, None)
                .await
                .map(|(request_id, _)| request_id)
                .map_err(|_| EBUSY)
        })
    }

    fn spawn_eviction_observer(
        &self,
        document_id: fs_protocol::FileDocumentId,
        request_id: EvictionRequestId,
    ) -> Result<(), i32> {
        self.eviction_coordinator()?
            .spawn_observer(document_id, request_id);
        Ok(())
    }

    fn eviction_coordinator(&self) -> Result<crate::eviction::EvictionCoordinator, i32> {
        Ok(crate::eviction::EvictionCoordinator::new(
            Arc::clone(&self.hydrator),
            self.availability.as_ref().ok_or(libc::ENOTSUP)?.clone(),
            self.eviction_queue.as_ref().ok_or(libc::ENOTSUP)?.clone(),
        ))
    }

    /// Resume durable cache-reclamation intents after a daemon restart. Every pending request
    /// is freshly reauthorized against current policy and history before owned tags are
    /// released again; completed observations repair a registry update a crash interrupted.
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
                // Reuse the exact-version guard: a completed old-version request must not
                // erase residency for a newer current file.
                if let Ok(coordinator) = self.eviction_coordinator() {
                    let _ = self
                        .hydrator
                        .block_on(coordinator.observe_once(request.document_id, request.id));
                }
                continue;
            }
            // Settings persists an intent before the blobs are removed from the store.
            // Availability and history can lag a newly applied namespace revision, so a later
            // restart must not turn an old intent into authority for the current file.
            if !self.eviction_request_matches_live_namespace(&request) {
                eprintln!(
                    "pending eviction no longer matches the live namespace version; leaving it unexecuted"
                );
                continue;
            }
            match self.prepare_eviction_request(request.document_id, request.mode, Some(request.id))
            {
                Ok(request_id) => {
                    if let Err(error) =
                        self.spawn_eviction_observer(request.document_id, request_id)
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
            namespace.document_id_for_id(&node_id) == Some(request.document_id)
                && namespace
                    .entry_for_node(&node_id)
                    .and_then(|entry| entry.hash)
                    == Some(request.content_hash)
        })
    }

    /// The thumbnail (PNG bytes) for a file, generated as metadata ahead of its content, or
    /// `None`. Resolution order: a cache hit, a peer-published thumbnail blob hash (hydrated
    /// ahead of the file's content) and adopted, then host generation from fully local
    /// content, cached and ingested so the hash reaches peers. The file's content is never
    /// hydrated from the network to make a preview: only the host, which has the bytes,
    /// generates.
    pub fn thumbnail_for(&self, ino: u64) -> Option<Vec<u8>> {
        let provider = self.thumbs.as_ref()?;
        let workspace = self.inode_workspace(ino);
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
        let content_hash = content_hash?;
        if workspace.is_some() {
            // Preview bytes are derived from the file and must not enter the unsealed blob store.
            self.check_access(ino, Access::Read).ok()?;
            let bytes = self.hydrator.block_on(self.hydrator.read_local_scoped(
                content_hash,
                0,
                thumbnail::MAX_SOURCE_BYTES.saturating_add(1) as u64,
                workspace,
            ))?;
            if bytes.len() > thumbnail::MAX_SOURCE_BYTES {
                return None;
            }
            let image =
                thumbnail::generate(&thumbnail::Source::Bytes(&bytes), category, provider.edge())?;
            return thumbnail::encode_png(&image);
        }
        let ch = *content_hash.as_bytes();

        if let Some(png) = provider.cached(ch) {
            return Some(png);
        }

        // A peer published a thumbnail blob: hydrate the tiny blob (ahead of the content) and
        // adopt it.
        if let Some(tbh) = thumb_hash
            && let Some(png) = self.read_blob_blocking(tbh)
            && let Some(out) = provider.adopt(ch, png)
        {
            return Some(out);
        }

        // A preview comes from a bounded prefix: decoding more would not make a better
        // thumbnail, and a huge file must never be read whole to preview it.
        let bytes =
            self.local_content_blocking(content_hash, thumbnail::MAX_SOURCE_BYTES as u64)?;
        self.remember_rich_metadata(ino, &bytes);
        let png = provider.generate(ch, category, &bytes)?;

        // Record the hash on the node so the pointer projects to peers, which then hydrate the
        // blob rather than the whole file.
        if let Some(tbh) = self.ingest_blob_blocking(&png) {
            {
                let mut ns = self.ns();
                ns.set_thumb(ino, &tbh);
            }
            self.publish();
        }
        Some(png)
    }

    /// Up to `max` bytes of `hash`'s content, only if they are local; `None` otherwise, so making
    /// a preview never pulls content from the network and never reads a file whole.
    fn local_content_blocking(&self, hash: Hash, max: u64) -> Option<Vec<u8>> {
        self.hydrator
            .block_on(self.hydrator.read_local(hash, 0, max))
    }

    /// Read a raw blob (the thumbnail), serving it locally or hydrating it from a peer: the
    /// small transfer that lets a preview arrive before the content.
    fn read_blob_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        self.hydrator
            .block_on(self.hydrator.read_blob(hash, 0, u64::MAX))
            .ok()
    }

    fn ingest_blob_blocking(&self, png: &[u8]) -> Option<Hash> {
        self.hydrator
            .block_on(self.hydrator.store.ingest(png.to_vec()))
            .ok()
    }

    /// Bind this mount to a workspace ACL, enforcing per-identity access for `identity`:
    /// every subsequent open, read and write is checked against it, and without one the mount
    /// keeps Unix-veneer-only behaviour. Takes anything that becomes an [`AclHandle`].
    #[must_use]
    pub fn with_acl(
        mut self,
        acl: impl Into<AclHandle>,
        identity: crate::acl::IdentityKey,
    ) -> Self {
        self.enforcement = Some((acl.into(), identity));
        // Subscribe before the initial snapshot, so a live ACL or sync mutation in between has a
        // receiver to wake instead of becoming a watch value no `.changed()` can observe.
        self.start_document_index_acl_worker();
        // Build the authority view once when the mount is bound; later v3 manifest and page
        // requests use only this retained snapshot.
        self.refresh_acl_visible_document_index();
        self
    }

    /// The identity ACL check every open, read and write routes through: `Ok(())` when the
    /// mount has no ACL, or the bound identity holds `access`. Never `EACCES`: outside the
    /// view is `ENOENT`, inside but not writable is `EROFS`.
    ///
    /// # Errors
    ///
    /// `ENOENT` when `ino` does not resolve to a path the identity can see, `EROFS` when it
    /// can see it but lacks `access`.
    pub fn check_access(&self, ino: u64, access: Access) -> Result<(), i32> {
        if !self.may_read_workspace() {
            return Err(ENOENT);
        }
        if self.enforcement.is_none() {
            return Ok(());
        }
        let path = self.ns().path_for(ino).ok_or(ENOENT)?;
        self.check_path(&path, access)
    }

    fn check_view_access(&self, view: &InodeView, access: Access) -> Result<(), i32> {
        if !self.may_read_workspace() {
            return Err(ENOENT);
        }
        if self.enforcement.is_none() {
            return Ok(());
        }
        self.check_path(view.path.as_deref().ok_or(ENOENT)?, access)
    }

    fn view_in_view(&self, view: &InodeView) -> bool {
        self.may_read_workspace()
            && (self.enforcement.is_none()
                || view.path.as_deref().is_some_and(|path| self.in_view(path)))
    }

    pub(crate) fn may_read_workspace(&self) -> bool {
        self.admission
            .as_ref()
            .is_none_or(|admission| admission.reads())
    }

    fn check_path(&self, path: &str, access: Access) -> Result<(), i32> {
        if !self.may_read_workspace() {
            return Err(ENOENT);
        }
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        if acl.permits(identity, path, access) {
            return Ok(());
        }
        if acl.visible(identity, path) && access == Access::Write {
            return Err(EROFS);
        }
        // Outside the view, or in it but not readable (an ancestor directory the app may walk
        // but not read as content). Either way the result is ENOENT.
        Err(ENOENT)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the open-handle snapshot must remain stable across authorization and inode validation"
    )]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// Whether `path` is part of the view this mount serves. `true` for every path on a mount
    /// with no ACL attached: without one there is nothing to filter.
    fn in_view(&self, path: &str) -> bool {
        match &self.enforcement {
            Some((acl, identity)) => acl.visible(identity, path),
            None => true,
        }
    }

    /// Whether `ino` is part of the view. An inode with no resolvable path (trashed, cyclic) is
    /// not.
    fn ino_in_view(&self, ino: u64) -> bool {
        if !self.may_read_workspace() {
            return false;
        }
        if self.enforcement.is_none() {
            return true;
        }
        let path = self.ns().path_for(ino);
        match path {
            Some(path) => self.in_view(&path),
            None => false,
        }
    }

    /// Hand what an authorised open of `ino` predicts to the predictive worker, after the open
    /// is answered, so prediction never delays it.
    fn predict_after_open(&self, ino: u64) {
        self.heuristics
            .on_open(ino, || self.predictive_candidates(ino));
    }

    /// What an open of `ino` predicts, as `(content id, declared size)` pairs to hydrate, and
    /// the open recorded in the mount's access history: the visible siblings of the same
    /// extension ahead of it in natural order, as far ahead as the run of in-order opens
    /// earns, then files opened together with it before.
    ///
    /// Every co-access partner passes the same read check an open does, so prediction never
    /// fetches content the mount's identity may not see.
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
            std::path::Path::new(&*crate::node::host_name(name))
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
        let (prediction, changed) = {
            let mut history = self.access_history.lock_recover();
            let before = history.revision();
            history.record(parent, ino, std::time::Instant::now());
            (
                history.predict(parent, ino, &order),
                history.revision() != before,
            )
        };
        if changed && let Some(kept) = &self.kept_history {
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

    /// The check every "make a new name in this directory" operation runs (`create`, `mkdir`,
    /// rename's destination). A parent outside the view is `ENOENT`. The new path must be
    /// writable, and that is asked before the name is looked up, so a name the app may not
    /// create gets the same answer whether or not something sits on it: the folder is in the
    /// view but not writable, `EROFS`, as `touch` in a folder one may only read reports a
    /// read-only file system, not a missing one. A collision with something hidden is refused
    /// with `EROFS` rather than written over.
    fn check_new_name(&self, parent: u64, name: &[u8]) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        if !self.may_read_workspace() {
            return Err(ENOENT);
        }
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
        if !acl.visible(identity, &parent_path) {
            return Err(ENOENT);
        }
        let child = child_path(&parent_path, name);
        if !acl.permits(identity, &child, Access::Write) {
            return Err(EROFS);
        }
        let taken = self.ns().lookup(parent, name).is_some();
        if taken && !acl.visible(identity, &child) {
            return Err(EROFS);
        }
        Ok(())
    }

    /// The check for removing or renaming an existing name in `parent`: the entry must be in
    /// the view (else it is not there) and writable (else the view is read-only).
    fn check_existing_name(&self, parent: u64, name: &[u8]) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
        if !self.in_view(&parent_path) {
            return Err(ENOENT);
        }
        let path = child_path(&parent_path, name);
        self.check_path(&path, Access::Write)?;
        self.check_movable_path(&path)
    }

    fn check_movable_path(&self, path: &str) -> Result<(), i32> {
        if self
            .enforcement
            .as_ref()
            .is_some_and(|(acl, _)| acl.protects_workspace_tree(path))
        {
            return Err(EROFS);
        }
        Ok(())
    }

    /// Announce a local namespace change to the peers, the ACL view and the hydration workers.
    /// A change inside a changeset waits until the changeset is durable, so peers never learn
    /// of placements whose records are not.
    fn publish(&self) {
        if self.changeset_open.load(Ordering::Acquire) {
            self.announce_held.store(true, Ordering::Release);
            return;
        }
        self.announce();
    }

    fn announce(&self) {
        self.wake_pinned_hydration();
    }

    /// Coalesce local namespace and version changes into one worker pass. A new generation also
    /// makes the current batch stop before it finishes an obsolete version or unpinned file.
    fn wake_pinned_hydration(&self) {
        if let Some(worker) = &self.pinned_hydration {
            worker
                .wake
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }

    fn inode_workspace(&self, ino: u64) -> Option<collab_types::WorkspaceId> {
        self.ns()
            .path_for(ino)
            .and_then(|path| self.hydrator.scoped_workspace(&path))
    }

    /// Stores one version's content, answering its content id and length (`None` when empty)
    /// and whether a staged file became the entry itself.
    /// `previous` is the file's current content, whose chunk tier the new version keeps.
    fn store_version_blocking(
        &self,
        source: VersionSource<'_>,
        previous: Option<Hash>,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> io::Result<(Option<(Hash, u64)>, bool)> {
        let wrap = |error: anyhow::Error| io::Error::other(format!("{error:#}"));
        match source {
            VersionSource::Bytes(data) => {
                let len = data.len() as u64;
                let hash = self
                    .hydrator
                    .block_on(self.hydrator.write_file_content(data, previous, workspace))
                    .map_err(wrap)?;
                Ok((hash.map(|hash| (hash, len)), false))
            }
            VersionSource::File {
                path,
                manifest,
                adopt,
            } => {
                // A mount's close hands a stage's file over: the bytes are read, chunked and
                // linked on this thread, so a photo costs its one write and nothing beside it.
                // A version that replaces another (whose chunk tier matters), a workspace that
                // seals its content under its own keys, or bytes the store already holds take
                // the path that waits on the store's own threads.
                if adopt
                    && manifest.is_none()
                    && previous.is_none()
                    && self.hydrator.stores_plainly(workspace)
                {
                    match self.hydrator.adopt_staged_inline(path) {
                        Ok(Some(stored)) => return Ok(stored),
                        Ok(None) => {}
                        Err(error) => return Err(wrap(error)),
                    }
                }
                self.hydrator
                    .block_on(
                        self.hydrator
                            .publish_file_content(path, manifest, previous, workspace, adopt),
                    )
                    .map_err(wrap)
            }
        }
    }

    /// Current full content of an inode: its stage if any, else the hydrated blob. `Ok(empty)` only for an empty or hashless file; a hydration failure is
    /// `Err`.
    fn read_full_blocking(&self, ino: u64) -> io::Result<Vec<u8>> {
        let workspace = self.inode_workspace(ino);
        if let Some(stage) = self.stage(ino) {
            if stage.lock().workspace() != workspace {
                return Err(crate::errno::error(ENOENT));
            }
            return self.read_stage_blocking(&stage, 0, u64::MAX);
        }
        let hash = self.ns().hash_for(ino);
        let Some(hash) = hash else {
            return Ok(Vec::new());
        };
        self.hydrator
            .block_on(self.hydrator.read_scoped(hash, 0, u64::MAX, workspace))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Ingest `data` as the new content of `ino` and broadcast the change. Aborts without
    /// mutating the entry if ingest fails, preserving the committed content.
    fn commit_content(&self, ino: u64, data: &[u8]) -> io::Result<()> {
        self.commit_content_at(ino, data, now_ms())
    }

    fn commit_content_at(&self, ino: u64, data: &[u8], mtime_ms: u64) -> io::Result<()> {
        self.commit_version_at(
            ino,
            VersionSource::Bytes(data),
            mtime_ms,
            None,
            self.inode_workspace(ino),
        )
    }

    /// [`Self::commit_content_at`] with the version's content from `source`, and for a version
    /// written under a write lease its tenancy (`fence`), published in the same change.
    fn commit_version_at(
        &self,
        ino: u64,
        source: VersionSource<'_>,
        mtime_ms: u64,
        fence: Option<crate::namespace::VersionFence>,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> io::Result<()> {
        let previous = self.version_base(ino, workspace)?;
        let (stored, _) = self.store_version_blocking(source, previous, workspace)?;
        self.install_version(ino, stored, mtime_ms, fence, workspace)
    }

    fn version_base(
        &self,
        ino: u64,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> io::Result<Option<Hash>> {
        let ns = self.ns();
        self.check_version_scope(&ns, ino, workspace)?;
        Ok(ns.hash_for(ino))
    }

    fn check_version_scope(
        &self,
        ns: &crate::namespace::Namespace,
        ino: u64,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> io::Result<()> {
        let current = ns
            .path_for(ino)
            .and_then(|path| self.hydrator.scoped_workspace(&path));
        if current != workspace {
            return Err(crate::errno::error(libc::EXDEV));
        }
        Ok(())
    }

    /// Make the stored version `stored` the content of `ino`, published in one change with the
    /// node's size and mtime. `EBUSY` when the committed revision moved under it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "content publication and namespace replacement must preserve the previous committed revision on failure"
    )]
    fn install_version(
        &self,
        ino: u64,
        stored: Option<(Hash, u64)>,
        mtime_ms: u64,
        fence: Option<crate::namespace::VersionFence>,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> io::Result<()> {
        #[cfg(test)]
        if let Some((fault_ino, errno)) = *self.commit_fault.lock_recover()
            && fault_ino == ino
        {
            return Err(crate::errno::error(errno));
        }
        let (hash, byte_len) = match stored {
            Some((hash, len)) => (Some(hash), len),
            None => (None, 0),
        };
        if workspace.is_some() {
            self.check_access(ino, Access::Write)
                .map_err(crate::errno::error)?;
        }
        let (expected_hash, expected_len, stamp) = {
            let ns = self.ns();
            self.check_version_scope(&ns, ino, workspace)?;
            let expected_hash = ns.hash_for(ino);
            let attr = ns.getattr(ino).ok_or_else(|| crate::errno::error(ENOENT))?;
            if expected_hash == hash && attr.size == byte_len {
                return Ok(());
            }
            // Signed metadata times never go backwards: a file given an older mtime than its
            // current version keeps the current one.
            let current_ms = attr
                .mtime
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            (expected_hash, attr.size, mtime_ms.max(current_ms))
        };
        {
            let mut ns = self.ns();
            self.check_version_scope(&ns, ino, workspace)?;
            let current_len = ns.getattr(ino).map(|attr| attr.size);
            let installed = ns.hash_for(ino) == hash && current_len == Some(byte_len);
            if !installed
                && (ns.hash_for(ino) != expected_hash || current_len != Some(expected_len))
            {
                return Err(crate::errno::error(EBUSY));
            }
            if let Some(fence) = fence
                && let Some(record) = ns.document_id_for(ino)
                && !ns.put_version_fence(record, hash.map(|h| *h.as_bytes()), byte_len, fence)
            {
                return Err(io::Error::other("could not tag the version with its lease"));
            }
            ns.set_content(ino, hash.as_ref(), byte_len, stamp);
        }
        self.wrote_content(hash);
        self.note_current_content(ino, hash);
        self.publish();
        Ok(())
    }

    /// Create an empty regular file. Signed mounts publish the immutable baseline before
    /// exposing the move-tree node, so a failed publication can leave an unreachable
    /// repository record, but never an unsigned live file.
    fn create_empty_file(&self, parent: u64, name: impl AsRef<[u8]>) -> Result<u64, i32> {
        self.create_empty_file_stamped(parent, name, now_ms())
    }

    fn create_empty_file_stamped(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        observed_at_ms: u64,
    ) -> Result<u64, i32> {
        self.create_file_stamped(parent, name, observed_at_ms, None, 0)
    }

    /// Create a regular file whose first version already has `content` (`byte_len` bytes): one
    /// signed version, content revision and metadata, instead of an empty file and an edit.
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
    ) -> Result<u64, i32> {
        let name: &[u8] = name.as_ref();
        let byte_len = if content.is_some() { byte_len } else { 0 };

        let mut namespace = self.ns();
        let ino = namespace
            .create_file(parent, name, content.as_ref(), byte_len, observed_at_ms)
            .ok_or(ENOENT)?;
        drop(namespace);
        self.wrote_content(content);
        Ok(ino)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "directory creation publishes one namespace mutation and its incremental document snapshot"
    )]
    fn create_empty_dir(&self, parent: u64, name: impl AsRef<[u8]>) -> Result<u64, i32> {
        let name: &[u8] = name.as_ref();
        let timestamp_ms = now_ms();
        let mut namespace = self.ns();
        let ino = namespace
            .create_dir(parent, name, timestamp_ms)
            .ok_or(ENOENT)?;
        Ok(ino)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn remove_entry(&self, parent: INodeNo, name: &OsStr, expect_dir: bool, reply: ReplyEmpty) {
        match self.delete_named(parent.0, name.as_encoded_bytes(), expect_dir) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(to_errno(error)),
        }
    }

    /// Put the node `name` in `parent` in the trash: one change of the tree.
    fn delete_named(&self, parent: u64, name: &[u8], expect_dir: bool) -> Result<(), i32> {
        self.check_existing_name(parent, name)?;
        {
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
                // A directory must be empty to rmdir.
                if let Some(entries) = ns.readdir(attr.ino.0)
                    && entries.len() > 2
                {
                    // Emptiness is judged on the real tree, since removing the directory takes its
                    // hidden children with it. If the app's own listing said empty, `ENOTEMPTY`
                    // would announce something it cannot see, so this gets `EROFS` like any other
                    // refusal to write over unseen state.
                    drop(ns);
                    let visible = self.visible_entries(attr.ino.0).map_or(0, |e| e.len());
                    return Err(if visible > 2 { ENOTEMPTY } else { EROFS });
                }
            }
            if !ns.remove(attr.ino.0) {
                return Err(ENOENT);
            }
        }
        self.publish();
        Ok(())
    }

    fn rename_named(
        &self,
        parent: u64,
        name: impl AsRef<[u8]>,
        new_parent: u64,
        new_name: impl AsRef<[u8]>,
    ) -> Result<(), i32> {
        let name: &[u8] = name.as_ref();
        let new_name: &[u8] = new_name.as_ref();
        let mut namespace = self.ns();
        namespace.lookup(parent, name).ok_or(ENOENT)?;
        if self.enforcement.is_some() {
            let source = namespace.path_for(parent).ok_or(ENOENT)?;
            let target = namespace.path_for(new_parent).ok_or(ENOENT)?;
            let from = child_path(&source, name);
            let to = child_path(&target, new_name);
            self.check_movable_path(&to)?;
            if self.hydrator.scoped_workspace(&from) != self.hydrator.scoped_workspace(&to) {
                return Err(libc::EXDEV);
            }
        }
        if !namespace.rename(parent, name, new_parent, new_name, now_ms()) {
            return Err(ENOENT);
        }
        drop(namespace);
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn to_errno(e: i32) -> Errno {
    crate::errno::error(e).into()
}

/// Reply to a `getxattr` following the two-call protocol: `size == 0` asks for the
/// length; a non-zero size must fit the value, else `ERANGE`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reply_xattr_bytes(reply: ReplyXattr, bytes: &[u8], size: u32) {
    if size == 0 {
        reply.size(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    } else if (size as usize) < bytes.len() {
        reply.error(to_errno(libc::ERANGE));
    } else {
        reply.data(bytes);
    }
}

/// `dir` + `name`, as the ACL spells paths: absolute, no trailing slash, `"/name"` directly
/// beneath the root.
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
    document_id: fs_protocol::FileDocumentId,
    update: impl FnOnce(&mut DocumentAvailability),
) {
    let mut registry = registry.lock_recover();
    let Some(mut record) = registry.get(document_id).cloned() else {
        return;
    };
    update(&mut record);
    if let Err(error) = registry.put(record) {
        eprintln!("could not persist hydration retry state: {error}");
    }
}

/// Every live node the mount may see, parents before descendants. The Namespace lock covers
/// only reading the nodes; the ACL is evaluated after it is released. Blocking: run it off the
/// async runtime's workers.
fn pinned_candidates(
    namespace: &Arc<Mutex<Namespace>>,
    enforcement: Option<&(AclHandle, crate::acl::IdentityKey)>,
) -> Vec<PinnedCandidate> {
    let mut candidates = Vec::new();
    {
        let namespace = namespace.lock_recover();
        if let Some(document_id) = namespace.document_id_for(crate::namespace::ROOT_INO) {
            candidates.push(PinnedCandidate {
                document_id,
                parent: None,
                kind: EntryKind::Directory,
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
                document_id: entry.document_id,
                parent: namespace.document_id_for_id(&entry.parent),
                kind: if namespace.node_is_dir(&id) == Some(true) {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                hash: entry.hash,
                size: entry.size,
                path,
            });
        }
    }
    if let Some((acl, identity)) = enforcement {
        candidates.retain(|candidate| acl.visible(identity, &candidate.path));
    }
    // Parents must enter the durable registry before descendants so ancestry
    // validation remains fail-closed without making initial scans fail.
    candidates.sort_by_key(|candidate| candidate.path.matches('/').count());
    candidates
}

/// A candidate's content as its availability record names it.
fn candidate_content(candidate: &PinnedCandidate) -> Option<fs_protocol::ContentHash> {
    candidate
        .hash
        .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()))
}

fn update_candidate_state(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    candidate: &PinnedCandidate,
    update: impl FnOnce(&mut DocumentAvailability),
) {
    let mut registry = registry.lock_recover();
    let Some(mut record) = registry.get(candidate.document_id).cloned() else {
        return;
    };
    if record.current_hash != candidate_content(candidate) {
        return;
    }
    update(&mut record);
    if let Err(error) = registry.put(record) {
        eprintln!("could not persist pinned hydration state: {error}");
    }
}

/// Whether a pin covers `document_id` once this pass records the namespace's topology: the walk
/// follows each visible candidate's current parent, and the registry's recorded parent above
/// them.
fn covered_by_pin(
    registry: &AvailabilityRegistry,
    parents: &HashMap<fs_protocol::FileDocumentId, Option<fs_protocol::FileDocumentId>>,
    document_id: fs_protocol::FileDocumentId,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(document_id);
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

fn topology_update(
    current: Option<&DocumentAvailability>,
    candidate: &PinnedCandidate,
    observed_at: u64,
) -> Option<DocumentAvailability> {
    let mut record = current
        .cloned()
        .unwrap_or_else(|| DocumentAvailability::new(candidate.document_id, candidate.kind));
    let content_changed = record.current_hash != candidate_content(candidate);
    let mut changed =
        current.is_none() || record.kind != candidate.kind || record.parent != candidate.parent;
    record.kind = candidate.kind;
    record.parent = candidate.parent;
    if content_changed {
        record.current_hash = candidate_content(candidate);
        record.residency = Residency::NotResident;
        record.transfer = crate::availability::TransferState::default();
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
    if candidate.kind == EntryKind::Directory || candidate.hash.is_none() {
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

fn pinned_state_update(
    current: &DocumentAvailability,
    candidate: &PinnedCandidate,
    probe: PinnedProbe,
    observed_at: u64,
) -> (Option<DocumentAvailability>, bool) {
    let mut record = current.clone();
    match probe {
        PinnedProbe::Local => {
            if record.current_hash != candidate_content(candidate) {
                return (None, false);
            }
            let resident = Residency::Resident {
                bytes: candidate.size,
            };
            // Re-stamping a resident file would rewrite the registry on every pass for nothing.
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
            // The worker owns queued work: a coalesced wake must not turn a current download into
            // another request.
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

#[derive(Default)]
struct PinnedPassPlan {
    writes: Vec<DocumentAvailability>,
    fetches: Vec<PinnedCandidate>,
    outcome: PinnedPassOutcome,
}

/// Record the visible topology, then every pinned file's probed state, in `registry`'s memory;
/// the caller commits. Topology goes first, so a pin on a stable directory covers children
/// introduced by a later namespace generation and keeps covering them across restarts.
fn plan_pinned_pass(
    registry: &mut AvailabilityRegistry,
    candidates: &[PinnedCandidate],
    probes: &HashMap<fs_protocol::FileDocumentId, PinnedProbe>,
    observed_at: u64,
) -> PinnedPassPlan {
    let mut plan = PinnedPassPlan::default();
    let mut unparented = 0_usize;
    for candidate in candidates {
        let Some(record) =
            topology_update(registry.get(candidate.document_id), candidate, observed_at)
        else {
            continue;
        };
        // Ancestry stays fail-closed: a row whose parent is not a recorded directory would fail
        // validation, so it waits for its parent.
        if record.parent.is_some_and(|parent| {
            registry
                .get(parent)
                .is_none_or(|parent| parent.kind != EntryKind::Directory)
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
        .filter(|candidate| candidate.kind == EntryKind::File && candidate.hash.is_some())
    {
        let Some(current) = registry.get(candidate.document_id).cloned() else {
            continue;
        };
        let pinned = matches!(
            registry.effective_policy(candidate.document_id),
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
        let Some(probe) = probes.get(&candidate.document_id).copied() else {
            continue;
        };
        let (update, due) = pinned_state_update(&current, candidate, probe, observed_at);
        if let Some(record) = update {
            let _ = registry.put(record.clone());
            plan.writes.push(record);
        }
        if !matches!(probe, PinnedProbe::Missing { .. })
            || registry
                .get(candidate.document_id)
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
            .get(candidate.document_id)
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

/// Commit a pass with one validation and one durable write of the registry (none when it changes
/// nothing). Should the rows be invalid as a whole, the batch leaves memory and disk untouched
/// and each row is committed on its own instead, so one bad row costs only itself.
fn commit_pinned_pass(
    registry: &Arc<Mutex<AvailabilityRegistry>>,
    candidates: &[PinnedCandidate],
    probes: &HashMap<fs_protocol::FileDocumentId, PinnedProbe>,
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
            let document_id = record.document_id;
            if let Err(error) = registry.put(record) {
                eprintln!("could not reconcile pinned record: {error}");
                rejected.insert(document_id);
            }
        }
        plan.fetches
            .retain(|candidate| !rejected.contains(&candidate.document_id));
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
        // Nothing is kept offline any more: what was goes back to the cache budget, in memory, with
        // no namespace walk.
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

    // Policy is metadata-only and cheap, so it is evaluated before probing the chunk store and an
    // automatic workspace does not re-walk every manifest on each reconciliation.
    let pinned_files = {
        let registry = registry.lock_recover();
        let parents = candidates
            .iter()
            .map(|candidate| (candidate.document_id, candidate.parent))
            .collect::<HashMap<_, _>>();
        candidates
            .iter()
            .filter(|candidate| {
                candidate.kind == EntryKind::File
                    && candidate.hash.is_some()
                    && covered_by_pin(&registry, &parents, candidate.document_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    // The pins decide what the cache budget may take before this pass fetches anything: a fetch
    // counts against the budget when it ends and a pinned download can run for hours, so a file
    // already here when it was pinned is held from now on. What no pin covers any more
    // (unpinned, an earlier version, a deleted file) is cache again.
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
        probes.insert(candidate.document_id, probe);
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
/// ([`crate::store::area::FileArea::keep_offline_only`]), off the async runtime's workers.
async fn keep_offline_only(hydrator: &Hydrator, kept: HashSet<[u8; 32]>) {
    let files = Arc::clone(hydrator.store.files());
    let applied = tokio::task::spawn_blocking(move || files.keep_offline_only(&kept)).await;
    if let Ok(Err(error)) = applied {
        eprintln!("could not record which content is kept offline: {error}");
    }
}

/// Run one member of a bounded scheduler batch. All members observe the same generation signal, so
/// a policy or namespace version change cancels the batch promptly; the coalesced pass that
/// follows chooses the current work.
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

/// Await one fetch, cancelling it only for a change that invalidates its own residency promise.
/// The global generation still coalesces namespace reconciliation, but unrelated writes must not
/// starve a large pinned download.
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

/// Resolves when `changes` moves on; never when there is nothing to watch or its sender is gone.
async fn next_change<T: Send + Sync>(changes: Option<&mut tokio::sync::watch::Receiver<T>>) {
    if let Some(changes) = changes
        && changes.changed().await.is_ok()
    {
        return;
    }
    std::future::pending::<()>().await;
}

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
        .effective_policy(candidate.document_id)
        .is_ok_and(|policy| policy == Some(AvailabilityPolicy::Pinned));
    if !pinned {
        return false;
    }
    let namespace = namespace.lock_recover();
    let Some(ino) = namespace.ino_for_document_id(candidate.document_id) else {
        return false;
    };
    namespace.hash_for(ino) == candidate.hash
        && namespace
            .getattr(ino)
            .is_some_and(|attr| attr.size == candidate.size)
}

/// The ACL accesses an open of the given mode requires: read for `O_RDONLY`, write for
/// `O_WRONLY`, both for `O_RDWR`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn required_accesses(mode: OpenAccMode) -> &'static [Access] {
    match mode {
        OpenAccMode::O_RDONLY => &[Access::Read],
        OpenAccMode::O_WRONLY => &[Access::Write],
        OpenAccMode::O_RDWR => &[Access::Read, Access::Write],
    }
}

#[cfg(all(test, target_os = "linux"))]
mod mount_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod durability_tests;

#[cfg(test)]
mod prediction_tests;
