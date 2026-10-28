//! [`FsStore`]: the [`DurableStore`] over a local profile directory.
//!
//! Keys map onto the same files the canonical record owner has always used,
//! so existing profiles open unchanged:
//!
//! - a record is `<root>/<package>/rooms/<workspace>/records/<record>/`
//!   holding `state.automerge`, the writer identity in `actor` and the
//!   single-owner gate `canonical.lock`;
//! - a view is `<root>/instance-views/<hex device>/<hex workspace>/<n>.postcard`.
//!
//! Writes return at once. One background thread per store coalesces each
//! key's writes for a debounce window, then commits them: a replacement is
//! written to a temporary file, synced and renamed, followed by every
//! ancestor directory; an append is written and synced in place. A write is
//! [`Durability::Durable`] only after that commit.
//!
//! The same thread opens keys for a thread that must not wait on the disk
//! ([`durable_store::waiting`]): see [`prepared`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use durable_store::{
    Durability, DurableStore, Lease, ReadyWake, Revision, SettleWatch, SharedLease, StorageKey,
    StoreError as SeamError, Stored, WriteOp,
};

use crate::{
    ACTOR_FILE, CANONICAL_GATE, DEFAULT_DEBOUNCE, DOCUMENT_FILE, LOAD_HINT_FILE, LOCK_FILE,
    RECORD_SUCCESSOR_CLAIM, RECORDS_DIR, ROOMS_DIR, StoreError, acquire_gate, acquire_writer_lock,
    claim_marker, ensure_state_dir, io_err, open_directory, same_file, sync_directory_ancestors,
    validate_absolute_directory, validate_package_id, validate_record_key,
};
use prepared::{Access, Preparation, Snapshot};
use tilo_lock::MutexRecover as _;

mod prepared;

/// Directory below the profile root holding every instance's local view.
pub const INSTANCE_VIEWS_DIR: &str = "instance-views";

/// A [`DurableStore`] rooted at one profile directory. See the module docs
/// for the layout. Reads observe accepted writes at once, from memory while
/// they commit, and never wait for the writer thread; a write whose commit
/// failed is not visible to later reads.
pub struct FsStore {
    root: PathBuf,
    namespace: String,
    shared: Arc<Shared>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

struct Shared {
    root: PathBuf,
    /// The host provisioned `root` ([`FsStore::provisioned`]): no directory
    /// above it is the store's to sync.
    provisioned: bool,
    state: Mutex<State>,
    changed: Condvar,
    debounce: Duration,
    #[cfg(test)]
    faults: Mutex<Faults>,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Faults {
    pub(crate) append_prefix: Option<usize>,
    pub(crate) sync_failure: Option<PathBuf>,
    /// Every thread that has synced a file or directory, in order.
    pub(crate) sync_threads: Vec<thread::ThreadId>,
    /// Holds the next preparation, on the writer thread, until a value
    /// arrives: a disk that takes its time.
    pub(crate) prepare_hold: Option<std::sync::mpsc::Receiver<()>>,
    /// Holds the next append, on the writer thread, after its base length
    /// is noted and before any byte is written: says so on the sender, then
    /// waits for a value on the receiver.
    pub(crate) append_hold: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

#[derive(Default)]
struct State {
    issued: u64,
    queue: BTreeMap<StorageKey, Batch>,
    /// Commits the writer thread has taken, with what they write: a read
    /// meanwhile is answered from them, never by waiting for the disk.
    writing: BTreeMap<StorageKey, Writing>,
    keys: BTreeMap<StorageKey, KeyStatus>,
    owners: BTreeMap<StorageKey, Vec<Arc<Owner>>>,
    view_leases: BTreeMap<StorageKey, Arc<advisory_lock::FileLock>>,
    /// Keys whose existing bytes, read here rather than written here, still
    /// need their file and ancestors synced: taken by the writer thread, so
    /// no caller waits on the disk for them.
    sync_queue: BTreeSet<StorageKey>,
    syncing: BTreeSet<StorageKey>,
    /// Keys being opened for that thread, and what came of it.
    prepared: BTreeMap<StorageKey, Preparation>,
    /// Preparations the writer thread has yet to take.
    prepare_queue: VecDeque<(StorageKey, Access)>,
    /// What reads of a key leased by that thread answer: the bytes read for
    /// it, kept current with the writes accepted since. Gone with the lease.
    cache: BTreeMap<StorageKey, Snapshot>,
    /// Load hints that thread handed over, for the writer thread to keep.
    hints: VecDeque<(StorageKey, Vec<u8>)>,
    /// Wakes to call, off the lock, once nothing is outstanding for a key
    /// ([`DurableStore::wake_when_settled`]).
    settle_wakes: BTreeMap<StorageKey, Vec<ReadyWake>>,
    running: bool,
    stopping: bool,
}

impl State {
    fn busy(&self, key: &StorageKey) -> bool {
        self.queue.contains_key(key) || self.writing.contains_key(key)
    }

    /// Whether a write or a sync is still queued or under way for `key`.
    fn outstanding(&self, key: &StorageKey) -> bool {
        self.busy(key) || self.sync_queue.contains(key) || self.syncing.contains(key)
    }

    /// The wakes armed for `key`, once nothing is outstanding for it: the
    /// caller calls them after letting go of the lock.
    fn take_settle_wakes(&mut self, key: &StorageKey) -> Vec<ReadyWake> {
        if self.outstanding(key) {
            return Vec::new();
        }
        self.settle_wakes.remove(key).unwrap_or_default()
    }
}

/// [`DurableStore::wake_when_settled`] under the store's lock.
fn arm_settle_wake(state: &mut State, key: &StorageKey, wake: ReadyWake) -> SettleWatch {
    if !state.outstanding(key) {
        return SettleWatch::Settled;
    }
    let waiting = state.settle_wakes.entry(key.clone()).or_default();
    if !waiting
        .iter()
        .any(|armed| durable_store::waiting::same_wake(armed, &wake))
    {
        waiting.push(wake);
    }
    SettleWatch::Armed
}

fn call_all(wakes: Vec<ReadyWake>) {
    for wake in wakes {
        wake();
    }
}

/// A commit under way on the writer thread.
struct Writing {
    first: u64,
    last: u64,
    op: Arc<WriteOp>,
    writer: Option<Arc<[u8]>>,
    /// For an append: the file's length before the writer thread started
    /// appending, noted before its first byte is written. `None` until then.
    base_len: Option<u64>,
}

struct Batch {
    // A queued commit keeps its view slot exclusive even after the caller drops.
    view_gate: Option<Arc<advisory_lock::FileLock>>,
    /// The lease a document's write was accepted under, kept until it
    /// commits: a caller that may not wait (a frame loop closing an app)
    /// lets go of its lease before the write lands, and the document must
    /// stay its own until then.
    owner: Option<Arc<Owner>>,
    first: u64,
    last: u64,
    op: WriteOp,
    writer: Option<Vec<u8>>,
    /// Where a view's bytes live; records resolve through their owner.
    view: Option<PathBuf>,
    due: Instant,
    urgent: bool,
}

#[derive(Default)]
struct KeyStatus {
    durable: u64,
    /// Failed revision ranges not yet superseded by a durable replacement.
    failed: Vec<(u64, u64, String)>,
    /// A failed commit may have left a torn tail; appends wait for a
    /// replacement.
    poisoned: bool,
    /// Everything readable at this key is known to be on stable storage.
    synced: bool,
}

/// Live handles proving a leased document directory is still the one leased.
/// The descriptors a lease locks, opened and checked on the store's thread.
struct Files {
    /// The document's directory and its handle; `None` for a view.
    directory: Option<(PathBuf, File)>,
    /// The single-owner gate (`canonical.lock`, or a view slot's `.lock`).
    gate: File,
    /// The primary shared writer's lock file, for [`prepared::Access::Shared`].
    writer_lock: Option<File>,
}

struct Owner {
    directory: PathBuf,
    handle: File,
    gate: advisory_lock::FileLock,
    exclusive: bool,
    /// Held by the primary shared writer: the actor sidecar is its alone.
    /// Kept open, never read, for the lock it holds.
    writer_lock: Option<advisory_lock::FileLock>,
}

impl Owner {
    fn check(&self) -> Result<(), StoreError> {
        for (opened, path) in [
            (&self.handle, self.directory.clone()),
            (self.gate.file(), self.directory.join(CANONICAL_GATE)),
        ] {
            if !same_file(opened, &path) {
                return Err(StoreError::SourceChanged { path });
            }
        }
        Ok(())
    }
}

struct FsLease {
    _view_gate: Option<Arc<advisory_lock::FileLock>>,
    key: StorageKey,
    owner: Option<Arc<Owner>>,
    shared: Weak<Shared>,
}

impl Drop for FsLease {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = lock(&shared.state);
        let Some(owner) = &self.owner else {
            state.view_leases.remove(&self.key);
            state.cache.remove(&self.key);
            return;
        };
        if let Some(live) = state.owners.get_mut(&self.key) {
            live.retain(|candidate| !Arc::ptr_eq(candidate, owner));
            if live.is_empty() {
                state.owners.remove(&self.key);
                state.cache.remove(&self.key);
            }
        }
    }
}

enum Target {
    /// A directory holding `state.automerge`, its actor sidecar and gates.
    Document(PathBuf),
    View(PathBuf),
}

/// A package's directory below `root`.
fn package_dir_in(root: &Path, package: &str) -> Result<PathBuf, StoreError> {
    validate_package_id(package)?;
    validate_absolute_directory(root)?;
    Ok(root.join(package))
}

fn target_in(root: &Path, key: &StorageKey) -> Result<Target, SeamError> {
    match key {
        StorageKey::Record {
            package,
            workspace,
            record,
        } => {
            validate_record_key(workspace)?;
            validate_record_key(record)?;
            Ok(Target::Document(
                package_dir_in(root, package)?
                    .join(ROOMS_DIR)
                    .join(workspace)
                    .join(RECORDS_DIR)
                    .join(record),
            ))
        }
        StorageKey::Package { package } => Ok(Target::Document(package_dir_in(root, package)?)),
        StorageKey::RoomIndex { package, workspace } => {
            validate_record_key(workspace)?;
            Ok(Target::Document(
                package_dir_in(root, package)?
                    .join(ROOMS_DIR)
                    .join(workspace),
            ))
        }
        StorageKey::View {
            device,
            workspace,
            instance,
        } => {
            validate_absolute_directory(root)?;
            for identity in [device, workspace] {
                if identity.is_empty() {
                    return Err(SeamError::InvalidKey {
                        key: key.to_string(),
                        reason: "view identities must not be empty".into(),
                    });
                }
            }
            Ok(Target::View(
                root.join(INSTANCE_VIEWS_DIR)
                    .join(hex::encode(device.as_bytes()))
                    .join(hex::encode(workspace.as_bytes()))
                    .join(format!("{instance}.postcard")),
            ))
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock_recover()
}

/// A writer-identity sidecar's bytes; an empty or malformed one is unreadable.
fn unhex(text: &str) -> Option<Vec<u8>> {
    hex::decode(text).ok().filter(|bytes| !bytes.is_empty())
}

impl From<StoreError> for SeamError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::WriterBusy { path } => Self::Busy(path.display().to_string()),
            StoreError::SourceChanged { path } => Self::SourceChanged(path.display().to_string()),
            StoreError::Opening(key) => Self::Opening(key),
            StoreError::PackageId(key, reason) | StoreError::RecordKey(key, reason) => {
                Self::InvalidKey {
                    key,
                    reason: reason.to_owned(),
                }
            }
            StoreError::Io { context, source } => Self::Failed {
                context,
                message: source.to_string(),
            },
            other => Self::Failed {
                context: "storage".to_owned(),
                message: other.to_string(),
            },
        }
    }
}

impl FsStore {
    /// A store over `profile_root`. Nothing is created until a key is leased
    /// or written; a relative root is refused then, per key.
    pub fn new(profile_root: impl Into<PathBuf>) -> Self {
        Self::with_debounce(profile_root, DEFAULT_DEBOUNCE)
    }

    /// As [`Self::new`], coalescing each key's writes for `debounce`.
    pub fn with_debounce(profile_root: impl Into<PathBuf>, debounce: Duration) -> Self {
        Self::over(profile_root.into(), false, debounce)
    }

    /// A store over `root`, a directory its host provisioned and owns, as
    /// the Linux launcher binds the person's records into an app's sandbox:
    /// the store makes what it creates beneath `root` durable, and never
    /// opens a directory above it, which is the host's and which the sandbox
    /// may not let it open.
    pub fn provisioned(root: impl Into<PathBuf>) -> Self {
        Self::over(root.into(), true, DEFAULT_DEBOUNCE)
    }

    fn over(root: PathBuf, provisioned: bool, debounce: Duration) -> Self {
        Self {
            namespace: root.to_string_lossy().into_owned(),
            root: root.clone(),
            shared: Arc::new(Shared {
                root,
                provisioned,
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
                debounce,
                #[cfg(test)]
                faults: Mutex::default(),
            }),
            worker: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A package's directory.
    fn package_dir(&self, package: &str) -> Result<PathBuf, StoreError> {
        package_dir_in(&self.root, package)
    }

    fn target(&self, key: &StorageKey) -> Result<Target, SeamError> {
        target_in(&self.root, key)
    }

    fn ensure_worker(&self) -> Result<(), SeamError> {
        let mut worker = lock(&self.worker);
        let mut state = lock(&self.shared.state);
        if state.running {
            return Ok(());
        }
        state.running = true;
        drop(state);
        if let Some(previous) = worker.take() {
            let _ = previous.join();
        }
        let shared = Arc::clone(&self.shared);
        let spawned = thread::Builder::new()
            .name("durable-store-writer".into())
            .spawn(move || writer_loop(&shared));
        match spawned {
            Ok(join) => {
                *worker = Some(join);
                drop(worker);
                Ok(())
            }
            Err(error) => {
                drop(worker);
                lock(&self.shared.state).running = false;
                Err(SeamError::Failed {
                    context: "starting the storage writer".into(),
                    message: error.to_string(),
                })
            }
        }
    }

    /// Make bytes that were read, rather than written by this store, durable.
    fn sync_existing(&self, key: &StorageKey) -> Result<(), StoreError> {
        sync_existing_in(&self.shared, key)
    }

    #[cfg(test)]
    pub(crate) fn faults(&self) -> MutexGuard<'_, Faults> {
        lock(&self.shared.faults)
    }
}

/// Make bytes that were read, rather than written by this store, durable:
/// the document file, then every directory above it.
fn sync_existing_in(shared: &Shared, key: &StorageKey) -> Result<(), StoreError> {
    let (file, directory) = match target_in(&shared.root, key).map_err(seam_to_store)? {
        Target::Document(directory) => (directory.join(DOCUMENT_FILE), directory),
        Target::View(path) => {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            (path, parent)
        }
    };
    if !directory.is_dir() {
        return Ok(());
    }
    if file.is_file() {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        options.write(true);
        #[cfg(test)]
        note_sync(shared);
        options
            .open(&file)
            .and_then(|f| f.sync_all())
            .map_err(|e| io_err(format!("syncing {}", file.display()), e))?;
    }
    sync_ancestors(shared, &directory)
}

/// What was accepted for a key and is not in its file yet, in order: the
/// commit the writer thread has taken, then the batch still queued.
struct Unwritten {
    writing: Option<WritingCopy>,
    queued: Option<QueuedCopy>,
}

struct QueuedCopy {
    first: u64,
    last: u64,
    op: WriteOp,
    writer: Option<Vec<u8>>,
}

struct WritingCopy {
    first: u64,
    last: u64,
    op: Arc<WriteOp>,
    writer: Option<Arc<[u8]>>,
}

impl Unwritten {
    fn of(state: &State, key: &StorageKey) -> Self {
        Self {
            writing: state.writing.get(key).map(|writing| WritingCopy {
                first: writing.first,
                last: writing.last,
                op: Arc::clone(&writing.op),
                writer: writing.writer.clone(),
            }),
            queued: state.queue.get(key).map(|batch| QueuedCopy {
                first: batch.first,
                last: batch.last,
                op: batch.op.clone(),
                writer: batch.writer.clone(),
            }),
        }
    }

    /// Whether `state` still holds exactly these unwritten writes: none
    /// was taken, committed or extended since they were copied.
    fn still_unwritten(&self, state: &State, key: &StorageKey) -> bool {
        let writing = state
            .writing
            .get(key)
            .map(|writing| (writing.first, writing.last));
        let queued = state.queue.get(key).map(|batch| (batch.first, batch.last));
        writing == self.writing.as_ref().map(|copy| (copy.first, copy.last))
            && queued == self.queued.as_ref().map(|copy| (copy.first, copy.last))
    }

    fn is_empty(&self) -> bool {
        self.writing.is_none() && self.queued.is_none()
    }

    /// Whether the file's bytes do not matter: a replacement is unwritten.
    fn replaces(&self) -> bool {
        self.writing
            .as_ref()
            .is_some_and(|writing| writing.op.is_replace())
            || self
                .queued
                .as_ref()
                .is_some_and(|copy| copy.op.is_replace())
    }

    fn apply(&self, bytes: &mut Vec<u8>) {
        if let Some(writing) = &self.writing {
            writing.op.apply(bytes);
        }
        if let Some(copy) = &self.queued {
            copy.op.apply(bytes);
        }
    }

    /// The writer identity stored with the latest unwritten write, if any.
    fn writer(&self) -> Option<Vec<u8>> {
        self.queued
            .as_ref()
            .and_then(|copy| copy.writer.clone())
            .or_else(|| {
                self.writing
                    .as_ref()
                    .and_then(|writing| writing.writer.as_deref().map(<[u8]>::to_vec))
            })
    }
}

fn seam_to_store(error: SeamError) -> StoreError {
    StoreError::from(error)
}

impl DurableStore for FsStore {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, SeamError> {
        let target = self.target(key)?;
        if let Some(wake) = durable_store::waiting::wake() {
            // What this store holds answers without the disk; anything else
            // is read by the store's thread while the caller is told to ask
            // again.
            if let Some(stored) = self.replaced_in_memory(key, &target) {
                return Ok(Some(stored));
            }
            self.ensure_worker()?;
            return self.shared.read_without_waiting(key, &wake);
        }
        let (path, sidecar) = match target {
            Target::Document(directory) => (
                directory.join(DOCUMENT_FILE),
                Some(directory.join(ACTOR_FILE)),
            ),
            Target::View(path) => (path, None),
        };
        // Read-your-writes without waiting for the writer thread: what it
        // has not committed yet is applied from memory over what the file
        // holds. A commit that finishes while the file is read changes what
        // the file means, so the read starts over; each retry follows a
        // finished commit, so this ends.
        loop {
            let unwritten = Unwritten::of(&lock(&self.shared.state), key);
            let mut bytes = if unwritten.replaces() {
                Some(Vec::new())
            } else {
                match fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(io_err(format!("reading {}", path.display()), error).into());
                    }
                }
            };
            if !unwritten.replaces() && !unwritten.is_empty() {
                let state = lock(&self.shared.state);
                if !unwritten.still_unwritten(&state, key) {
                    continue;
                }
                // An append under way: the file may already hold part of it,
                // and holds all it did before from the noted length down.
                if let (Some(base), Some(read)) = (
                    state.writing.get(key).and_then(|now| now.base_len),
                    bytes.as_mut(),
                ) {
                    let base = usize::try_from(base).unwrap_or(usize::MAX);
                    if read.len() < base {
                        continue;
                    }
                    read.truncate(base);
                }
            }
            if unwritten.is_empty() && bytes.is_none() {
                return Ok(None);
            }
            let mut bytes = bytes.unwrap_or_default();
            unwritten.apply(&mut bytes);
            let writer = match (unwritten.writer(), sidecar) {
                (Some(writer), _) => Some(writer),
                (None, Some(sidecar)) => read_writer(&sidecar)?,
                (None, None) => None,
            };
            return Ok(Some(Stored { bytes, writer }));
        }
    }

    fn write(
        &self,
        key: &StorageKey,
        op: WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<Revision, SeamError> {
        let view = match self.target(key)? {
            Target::Document(_) => {
                if !lock(&self.shared.state).owners.contains_key(key) {
                    return Err(SeamError::NotLeased(key.to_string()));
                }
                None
            }
            Target::View(path) => {
                if writer.is_some() {
                    return Err(SeamError::InvalidKey {
                        key: key.to_string(),
                        reason: "views carry no writer identity".into(),
                    });
                }
                Some(path)
            }
        };
        self.ensure_worker()?;
        let now = Instant::now();
        let due = now.checked_add(self.shared.debounce).unwrap_or(now);
        let revision = {
            let mut state = lock(&self.shared.state);
            let revision = state
                .issued
                .checked_add(1)
                .ok_or_else(|| SeamError::Failed {
                    context: key.to_string(),
                    message: "revision space exhausted".into(),
                })?;
            state.issued = revision;
            if let Some(snapshot) = state.cache.get_mut(key) {
                snapshot.apply(&op, writer);
            }
            let writer = writer.map(<[u8]>::to_vec);
            let view_gate = state.view_leases.get(key).cloned();
            let owner = state.owners.get(key).and_then(|live| live.first()).cloned();
            if let Some(batch) = state.queue.get_mut(key) {
                batch.op.coalesce(op);
                batch.last = revision;
                batch.due = due;
                if writer.is_some() {
                    batch.writer = writer;
                }
            } else {
                state.queue.insert(
                    key.clone(),
                    Batch {
                        view_gate,
                        owner,
                        first: revision,
                        last: revision,
                        op,
                        writer,
                        view,
                        due,
                        urgent: false,
                    },
                );
            }
            revision
        };
        self.shared.changed.notify_all();
        Ok(Revision(revision))
    }

    fn durability(&self, key: &StorageKey, revision: Revision) -> Durability {
        durability_in(&lock(&self.shared.state), key, revision)
    }

    fn flush(&self, key: Option<&StorageKey>) -> Result<(), SeamError> {
        let matches = |candidate: &StorageKey| key.is_none_or(|key| key == candidate);
        let marked = {
            let mut state = lock(&self.shared.state);
            let mut marked = false;
            for (candidate, batch) in &mut state.queue {
                if matches(candidate) {
                    batch.urgent = true;
                    marked = true;
                }
            }
            drop(state);
            marked
        };
        if marked {
            self.ensure_worker()?;
            self.shared.changed.notify_all();
        }
        let mut state = lock(&self.shared.state);
        let mut told = false;
        while state
            .queue
            .keys()
            .chain(state.writing.keys())
            .chain(state.sync_queue.iter())
            .chain(state.syncing.iter())
            .any(&matches)
        {
            if !told {
                told = true;
                durable_store::waiting::refused_commit(&format!(
                    "FsStore::flush waiting for the writer thread to commit {}",
                    key.map_or_else(|| "every key".to_owned(), ToString::to_string)
                ));
            }
            if !state.running {
                // The writer exited unexpectedly with work still queued.
                drop(state);
                self.ensure_worker()?;
                self.shared.changed.notify_all();
                state = lock(&self.shared.state);
                continue;
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let failure = state
            .keys
            .iter()
            .filter(|(candidate, _)| matches(candidate))
            .find_map(|(candidate, status)| {
                let (_, _, message) = status.failed.last()?;
                Some(SeamError::Failed {
                    context: candidate.to_string(),
                    message: message.clone(),
                })
            });
        let unsynced = key
            .filter(|key| !state.keys.get(key).is_some_and(|status| status.synced))
            .cloned();
        drop(state);
        if let Some(failure) = failure {
            return Err(failure);
        }
        if let Some(key) = unsynced {
            durable_store::waiting::refused_commit(&format!("FsStore::flush syncing {key}"));
            self.sync_existing(&key)?;
            lock(&self.shared.state).keys.entry(key).or_default().synced = true;
        }
        Ok(())
    }

    fn flush_without_waiting(&self, key: &StorageKey) -> Result<(), SeamError> {
        self.target(key)?;
        let mut state = lock(&self.shared.state);
        if let Some((_, _, message)) = state.keys.get(key).and_then(|status| status.failed.last()) {
            return Err(SeamError::Failed {
                context: key.to_string(),
                message: message.clone(),
            });
        }
        if settled_in(&state, key) {
            return Ok(());
        }
        // Whatever is queued commits now, and what was only read is synced:
        // both on the writer thread, which is the one place that waits on the
        // disk. The caller learns of it through `is_settled`.
        for (candidate, batch) in &mut state.queue {
            if candidate == key {
                batch.urgent = true;
            }
        }
        if !state.keys.get(key).is_some_and(|status| status.synced) {
            state.sync_queue.insert(key.clone());
        }
        drop(state);
        self.ensure_worker()?;
        self.shared.changed.notify_all();
        Err(SeamError::Pending(format!("{key} is being made durable")))
    }

    fn is_settled(&self, key: &StorageKey) -> bool {
        settled_in(&lock(&self.shared.state), key)
    }

    fn wake_when_settled(&self, key: &StorageKey, wake: ReadyWake) -> SettleWatch {
        arm_settle_wake(&mut lock(&self.shared.state), key, wake)
    }

    fn lease(&self, key: &StorageKey) -> Result<Lease, SeamError> {
        let target = self.target(key)?;
        if let Some(wake) = durable_store::waiting::wake() {
            return self
                .lease_without_waiting(key, &target, Access::Exclusive, &wake)
                .map(|(lease, _)| lease);
        }
        match target {
            Target::Document(directory) => {
                if lock(&self.shared.state).owners.contains_key(key) {
                    return Err(SeamError::Busy(key.to_string()));
                }
                let owner = Arc::new(open_owner(&directory)?);
                Ok(Box::new(self.register(key, owner)?))
            }
            Target::View(path) => {
                let mut state = lock(&self.shared.state);
                if state.view_leases.contains_key(key) {
                    return Err(SeamError::Busy(key.to_string()));
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_err("creating view directory", e))?;
                }
                // Never unlink this gate: an old handle and a new pathname
                // must not admit two writers to the same bounded slot.
                let gate = Arc::new(acquire_gate(&path.with_extension("lock"), true)?);
                state.view_leases.insert(key.clone(), gate.clone());
                drop(state);
                Ok(Box::new(FsLease {
                    _view_gate: Some(gate),
                    key: key.clone(),
                    owner: None,
                    shared: Arc::downgrade(&self.shared),
                }))
            }
        }
    }

    fn lease_shared(&self, key: &StorageKey) -> Result<SharedLease, SeamError> {
        let target = self.target(key)?;
        if let (Target::Document(_), Some(wake)) = (&target, durable_store::waiting::wake()) {
            let (lease, primary) =
                self.lease_without_waiting(key, &target, Access::Shared, &wake)?;
            return Ok(SharedLease { lease, primary });
        }
        match target {
            Target::Document(directory) => {
                if lock(&self.shared.state)
                    .owners
                    .get(key)
                    .is_some_and(|live| live.iter().any(|owner| owner.exclusive))
                {
                    return Err(SeamError::Busy(key.to_string()));
                }
                let owner = Arc::new(open_shared_owner(&directory)?);
                let primary = owner.writer_lock.is_some();
                Ok(SharedLease {
                    lease: Box::new(self.register(key, owner)?),
                    primary,
                })
            }
            Target::View(_) => Err(SeamError::InvalidKey {
                key: key.to_string(),
                reason: "a view has exactly one writer".into(),
            }),
        }
    }

    fn declined_waiting_here(&self) -> bool {
        durable_store::waiting::disk_forbidden()
    }

    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, SeamError> {
        validate_record_key(claimant)?;
        let marker = match key {
            // A room record adopted under a new identity (see
            // `ui::RecordCheckout::formerly`): the claimant is its successor.
            StorageKey::Record { .. } => match self.target(key)? {
                Target::Document(directory) => directory.join(RECORD_SUCCESSOR_CLAIM),
                Target::View(_) => {
                    return Err(SeamError::InvalidKey {
                        key: key.to_string(),
                        reason: "a record is a document".into(),
                    });
                }
            },
            _ => {
                return Err(SeamError::InvalidKey {
                    key: key.to_string(),
                    reason: "only room records are claimed".into(),
                });
            }
        };
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| io_err(format!("creating {}", parent.display()), e))?;
        }
        Ok(claim_marker(&marker, claimant)?)
    }

    /// Records whose document file holds committed bytes. A write still in
    /// its debounce window is not listed until it commits.
    fn records(&self, package: &str, workspace: &str) -> Result<Vec<String>, SeamError> {
        validate_record_key(workspace)?;
        let directory = self
            .package_dir(package)?
            .join(ROOMS_DIR)
            .join(workspace)
            .join(RECORDS_DIR);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(io_err(format!("listing {}", directory.display()), error).into());
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err(format!("listing {}", directory.display()), e))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_record_key(&name).is_err() {
                continue;
            }
            if fs::metadata(entry.path().join(DOCUMENT_FILE)).is_ok_and(|m| m.len() > 0) {
                records.push(name);
            }
        }
        records.sort();
        Ok(records)
    }

    fn local_path(&self, key: &StorageKey) -> Option<PathBuf> {
        match self.target(key).ok()? {
            Target::Document(directory) => Some(directory.join(DOCUMENT_FILE)),
            Target::View(path) => Some(path),
        }
    }

    /// Kept in the record's directory, which only this device writes and
    /// never syncs. Written at once, unsynced: a hint lost to a crash, or
    /// written ahead of a replacement that then fails, names bytes the
    /// document no longer holds and the reader ignores it.
    fn write_hint(&self, key: &StorageKey, hint: &[u8]) -> Result<(), SeamError> {
        let Target::Document(directory) = self.target(key)? else {
            return Ok(());
        };
        if !lock(&self.shared.state).owners.contains_key(key) {
            return Err(SeamError::NotLeased(key.to_string()));
        }
        if durable_store::waiting::disk_forbidden() {
            self.ensure_worker()?;
            let mut state = lock(&self.shared.state);
            if let Some(snapshot) = state.cache.get_mut(key) {
                snapshot.hint = Some(hint.to_vec());
            }
            state.hints.push_back((key.clone(), hint.to_vec()));
            drop(state);
            self.shared.changed.notify_all();
            return Ok(());
        }
        keep_hint(&directory, hint).map_err(Into::into)
    }

    fn read_hint(&self, key: &StorageKey) -> Option<Vec<u8>> {
        let Target::Document(directory) = self.target(key).ok()? else {
            return None;
        };
        if durable_store::waiting::disk_forbidden() {
            return self.shared.hint_without_waiting(key);
        }
        read_hint_in(&directory)
    }
}

impl FsStore {
    /// A read answered from memory alone: a replacement this store accepted
    /// and has not written yet holds the whole value, and its writer where a
    /// document keeps one.
    fn replaced_in_memory(&self, key: &StorageKey, target: &Target) -> Option<Stored> {
        let unwritten = Unwritten::of(&lock(&self.shared.state), key);
        if !unwritten.replaces() {
            return None;
        }
        let writer = unwritten.writer();
        if writer.is_none() && matches!(target, Target::Document(_)) {
            return None;
        }
        let mut bytes = Vec::new();
        unwritten.apply(&mut bytes);
        Some(Stored { bytes, writer })
    }

    /// [`DurableStore::lease`] or [`DurableStore::lease_shared`] on the
    /// thread that declined waiting: lock what the store's thread opened for
    /// it, or have that thread open it ([`prepared`]). With whether the
    /// lease is its document's primary writer.
    fn lease_without_waiting(
        &self,
        key: &StorageKey,
        target: &Target,
        access: Access,
        wake: &ReadyWake,
    ) -> Result<(Lease, bool), SeamError> {
        let state = lock(&self.shared.state);
        let held = match target {
            Target::Document(_) => state.owners.get(key).is_some_and(|live| {
                access == Access::Exclusive || live.iter().any(|owner| owner.exclusive)
            }),
            Target::View(_) => state.view_leases.contains_key(key),
        };
        drop(state);
        if held {
            return Err(SeamError::Busy(key.to_string()));
        }
        self.ensure_worker()?;
        let prepared = self.shared.take_prepared(key, access, wake)?;
        let files = prepared.files.ok_or_else(|| SeamError::Failed {
            context: format!("opening {key}"),
            message: "prepared for reading only".into(),
        })?;
        let mode = if access == Access::Shared {
            advisory_lock::LockMode::Shared
        } else {
            advisory_lock::LockMode::Exclusive
        };
        let gate =
            advisory_lock::FileLock::try_lock(files.gate, mode).map_err(|error| match error {
                advisory_lock::TryLockError::WouldBlock => SeamError::Busy(key.to_string()),
                advisory_lock::TryLockError::Error(error) => SeamError::Failed {
                    context: format!("acquiring the gate of {key}"),
                    message: error.to_string(),
                },
            })?;
        let Some((directory, handle)) = files.directory else {
            let gate = Arc::new(gate);
            let mut state = lock(&self.shared.state);
            if state.view_leases.contains_key(key) {
                return Err(SeamError::Busy(key.to_string()));
            }
            state.view_leases.insert(key.clone(), Arc::clone(&gate));
            state.cache.insert(key.clone(), prepared.snapshot);
            drop(state);
            let lease: Lease = Box::new(FsLease {
                _view_gate: Some(gate),
                key: key.clone(),
                owner: None,
                shared: Arc::downgrade(&self.shared),
            });
            return Ok((lease, true));
        };
        let writer_lock = files
            .writer_lock
            .and_then(|file| advisory_lock::FileLock::try_exclusive(file).ok());
        if access == Access::Shared && writer_lock.is_none() {
            // Designed, not a fault: `open_shared_owner`'s primary holds the
            // writer lock and continues the stored actor; every other owner
            // appends under an actor of its own that is never stored. The
            // document keeps merging them, so a second window of the app
            // works beside the first without touching its identity.
            tracing::info!(
                directory = %directory.display(),
                "joining another instance's document as a secondary writer (fresh actor for \
                 this session); the first instance keeps the stored author"
            );
        }
        let primary = access == Access::Exclusive || writer_lock.is_some();
        let owner = Arc::new(Owner {
            directory,
            handle,
            gate,
            exclusive: access == Access::Exclusive,
            writer_lock,
        });
        let lease = self.register(key, owner)?;
        lock(&self.shared.state)
            .cache
            .entry(key.clone())
            .or_insert(prepared.snapshot);
        Ok((Box::new(lease), primary))
    }

    fn register(&self, key: &StorageKey, owner: Arc<Owner>) -> Result<FsLease, SeamError> {
        let mut state = lock(&self.shared.state);
        let live = state.owners.entry(key.clone()).or_default();
        if live.iter().any(|other| other.exclusive || owner.exclusive) {
            return Err(SeamError::Busy(key.to_string()));
        }
        let first = live.is_empty();
        live.push(Arc::clone(&owner));
        if first {
            // A new owner starts from bytes it verifies itself, not from an
            // earlier owner's failures.
            let status = state.keys.entry(key.clone()).or_default();
            status.failed.clear();
            status.poisoned = false;
            status.synced = false;
        }
        drop(state);
        Ok(FsLease {
            _view_gate: None,
            key: key.clone(),
            owner: Some(owner),
            shared: Arc::downgrade(&self.shared),
        })
    }
}

impl Drop for FsStore {
    fn drop(&mut self) {
        if durable_store::waiting::forbidden() {
            // Dropped on a thread that must not wait: the writer thread
            // commits everything queued as it stops, and ends by itself.
            lock(&self.shared.state).stopping = true;
            self.shared.changed.notify_all();
            drop(lock(&self.worker).take());
            return;
        }
        if let Err(error) = self.flush(None) {
            tracing::error!(%error, "storage writes failed before shutdown");
        }
        lock(&self.shared.state).stopping = true;
        self.shared.changed.notify_all();
        let worker = lock(&self.worker).take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Whether everything read from or accepted for `key` is on stable storage:
/// nothing queued, committing or waiting to be synced, and no failure.
fn settled_in(state: &State, key: &StorageKey) -> bool {
    state
        .keys
        .get(key)
        .is_some_and(|status| status.synced && status.failed.is_empty())
        && !state.busy(key)
        && !state.sync_queue.contains(key)
        && !state.syncing.contains(key)
}

fn durability_in(state: &State, key: &StorageKey, revision: Revision) -> Durability {
    let n = revision.0;
    if n == 0 || n > state.issued {
        return Durability::Failed(format!("revision {n} was never issued"));
    }
    let covers = |(first, last): (u64, u64)| (first..=last).contains(&n);
    if state
        .queue
        .get(key)
        .is_some_and(|batch| covers((batch.first, batch.last)))
        || state
            .writing
            .get(key)
            .is_some_and(|writing| covers((writing.first, writing.last)))
    {
        return Durability::Pending;
    }
    let foreign = || Durability::Failed(format!("revision {n} does not belong to {key}"));
    let Some(status) = state.keys.get(key) else {
        return foreign();
    };
    if let Some((_, _, message)) = status
        .failed
        .iter()
        .find(|(first, last, _)| covers((*first, *last)))
    {
        return Durability::Failed(message.clone());
    }
    if n <= status.durable {
        Durability::Durable
    } else {
        foreign()
    }
}

/// A key's stored bytes and writer identity, read from the disk.
fn read_stored(target: &Target) -> Result<Option<Stored>, SeamError> {
    let (path, writer) = match target {
        Target::Document(directory) => (
            directory.join(DOCUMENT_FILE),
            Some(directory.join(ACTOR_FILE)),
        ),
        Target::View(path) => (path.clone(), None),
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_err(format!("reading {}", path.display()), error).into()),
    };
    let writer = match writer {
        Some(sidecar) => read_writer(&sidecar)?,
        None => None,
    };
    Ok(Some(Stored { bytes, writer }))
}

/// A document's load hint, if it has a readable one.
fn read_hint_in(directory: &Path) -> Option<Vec<u8>> {
    fs::read(directory.join(LOAD_HINT_FILE)).ok()
}

/// Replace a document's load hint. Unsynced, as
/// [`DurableStore::write_hint`] allows.
fn keep_hint(directory: &Path, hint: &[u8]) -> Result<(), StoreError> {
    let path = directory.join(LOAD_HINT_FILE);
    let staged = directory.join(format!("{LOAD_HINT_FILE}.tmp"));
    fs::write(&staged, hint)
        .and_then(|()| fs::rename(&staged, &path))
        .map_err(|e| io_err(format!("writing {}", path.display()), e))
}

fn read_writer(path: &Path) -> Result<Option<Vec<u8>>, SeamError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let parsed = unhex(text.trim());
            if parsed.is_none() {
                tracing::warn!(
                    path = %path.display(),
                    "writer identity sidecar is unreadable; the next owner mints a new one"
                );
            }
            Ok(parsed)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(format!("reading {}", path.display()), error).into()),
    }
}

fn open_owner(directory: &Path) -> Result<Owner, StoreError> {
    fs::create_dir_all(directory).map_err(|e| io_err("creating canonical directory", e))?;
    let handle =
        open_directory(directory, false).map_err(|e| io_err("opening canonical directory", e))?;
    let gate = acquire_gate(&directory.join(CANONICAL_GATE), true)?;
    let owner = Owner {
        directory: directory.to_path_buf(),
        handle,
        gate,
        exclusive: true,
        writer_lock: None,
    };
    owner.check()?;
    // The directory's ancestors are synced by the writer thread with the
    // key's first commit or `flush_without_waiting`, never here: leasing is
    // done on the thread that opens the record, and nothing durable depends
    // on the directory entry before a write commits (every commit syncs it).
    Ok(owner)
}

/// Join a document's shared writers: the shared gate keeps exclusive owners
/// out, and whoever wins the writer lock is primary.
fn open_shared_owner(directory: &Path) -> Result<Owner, StoreError> {
    ensure_state_dir(directory)?;
    let handle =
        open_directory(directory, false).map_err(|e| io_err("opening state directory", e))?;
    let gate = acquire_gate(&directory.join(CANONICAL_GATE), false)?;
    let writer_lock = acquire_writer_lock(&directory.join(LOCK_FILE));
    if writer_lock.is_none() {
        // Designed, not a fault: the instance that won the writer lock is the
        // stored author, and this one appends under a fresh session actor.
        tracing::info!(
            directory = %directory.display(),
            "joining another instance's document as a secondary writer (fresh actor for this \
             session); the first instance keeps the stored author"
        );
    }
    let owner = Owner {
        directory: directory.to_path_buf(),
        handle,
        gate,
        exclusive: false,
        writer_lock,
    };
    owner.check()?;
    Ok(owner)
}

fn sync_ancestors(shared: &Shared, directory: &Path) -> Result<(), StoreError> {
    #[cfg(not(test))]
    let _ = shared;
    let provisioned = shared.provisioned.then_some(shared.root.as_path());
    sync_directory_ancestors(directory, provisioned, |path| {
        #[cfg(test)]
        note_sync(shared);
        #[cfg(test)]
        if lock(&shared.faults).sync_failure.as_deref() == Some(path) {
            return Err(io_err(
                "injected directory sync failure",
                io::Error::from(io::ErrorKind::Unsupported),
            ));
        }
        let _ = path;
        Ok(())
    })
}

/// Note, in a test, which thread synced.
#[cfg(test)]
fn note_sync(shared: &Shared) {
    lock(&shared.faults)
        .sync_threads
        .push(thread::current().id());
}

/// Marks the key being written as failed if the writer thread dies mid-commit.
struct WorkerGuard<'a> {
    shared: &'a Shared,
    current: Option<(StorageKey, u64, u64)>,
    finished: bool,
}

impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut state = lock(&self.shared.state);
        if let Some((key, first, last)) = self.current.take() {
            state.writing.remove(&key);
            let status = state.keys.entry(key).or_default();
            status.failed.push((
                first,
                last,
                "the storage writer exited unexpectedly".to_owned(),
            ));
            status.poisoned = true;
        }
        state.running = false;
        let keys: Vec<StorageKey> = state.settle_wakes.keys().cloned().collect();
        let wakes: Vec<ReadyWake> = keys
            .iter()
            .flat_map(|key| state.take_settle_wakes(key))
            .collect();
        drop(state);
        call_all(wakes);
        self.shared.changed.notify_all();
    }
}

/// What the writer thread does next.
enum Work {
    Commit(Taken),
    /// Sync the key's file and ancestors: bytes it read, not wrote.
    Sync(StorageKey),
    /// Open the key for the thread that declined waiting.
    Prepare(StorageKey, Access),
    /// Keep a load hint that thread handed over.
    Hint(StorageKey, Vec<u8>),
}

struct Taken {
    key: StorageKey,
    /// The batch's bookkeeping; its write is `op`, shared with
    /// [`State::writing`] so reads see it while it commits.
    batch: Batch,
    op: Arc<WriteOp>,
    writer: Option<Arc<[u8]>>,
    owner: Option<Arc<Owner>>,
    poisoned: bool,
}

fn writer_loop(shared: &Shared) {
    let mut guard = WorkerGuard {
        shared,
        current: None,
        finished: false,
    };
    while let Some(work) = next_work(shared) {
        let taken = match work {
            Work::Commit(taken) => taken,
            Work::Sync(key) => {
                let result = sync_existing_in(shared, &key);
                finish_sync(shared, &key, &result);
                continue;
            }
            Work::Prepare(key, access) => {
                let result = prepared::prepare_in(shared, &key, access);
                prepared::finish_prepare(shared, &key, access, result);
                continue;
            }
            Work::Hint(key, hint) => {
                prepared::write_hint_in(shared, &key, &hint);
                continue;
            }
        };
        guard.current = Some((taken.key.clone(), taken.batch.first, taken.batch.last));
        let started = Instant::now();
        let result = commit(shared, &taken);
        guard.current = None;
        // Release the owner's handles before waking flushers: a caller that
        // then drops its lease must find the gate free.
        let Taken {
            key,
            mut batch,
            op,
            owner,
            ..
        } = taken;
        drop(owner);
        drop(batch.view_gate.take());
        finish(shared, &key, &batch, &op, result, started);
    }
    guard.finished = true;
}

fn next_work(shared: &Shared) -> Option<Work> {
    let mut state = lock(&shared.state);
    loop {
        // An opener is waiting on these (a new tab, say); a commit is not.
        if let Some((key, access)) = state.prepare_queue.pop_front() {
            return Some(Work::Prepare(key, access));
        }
        if let Some((key, hint)) = state.hints.pop_front() {
            return Some(Work::Hint(key, hint));
        }
        let now = Instant::now();
        let stopping = state.stopping;
        let due = state
            .queue
            .iter()
            .find(|(_, batch)| stopping || batch.urgent || batch.due <= now)
            .map(|(key, _)| key.clone());
        if let Some(key) = due
            && let Some(mut batch) = state.queue.remove(&key)
        {
            let op = Arc::new(std::mem::replace(
                &mut batch.op,
                WriteOp::Append(Vec::new()),
            ));
            let writer: Option<Arc<[u8]>> = batch.writer.take().map(Arc::from);
            state.writing.insert(
                key.clone(),
                Writing {
                    first: batch.first,
                    last: batch.last,
                    op: Arc::clone(&op),
                    writer: writer.clone(),
                    base_len: None,
                },
            );
            let owner = batch.owner.take().or_else(|| {
                state
                    .owners
                    .get(&key)
                    .and_then(|live| live.first())
                    .cloned()
            });
            let poisoned = state.keys.get(&key).is_some_and(|status| status.poisoned);
            return Some(Work::Commit(Taken {
                key,
                batch,
                op,
                writer,
                owner,
                poisoned,
            }));
        }
        // Nothing due to commit: sync what was only read. A key with a commit
        // still queued or running is left to it, which syncs everything.
        let ready = state
            .sync_queue
            .iter()
            .find(|key| !state.busy(key))
            .cloned();
        if let Some(key) = ready {
            state.sync_queue.remove(&key);
            if state.keys.get(&key).is_some_and(|status| status.synced) {
                let wakes = state.take_settle_wakes(&key);
                if !wakes.is_empty() {
                    drop(state);
                    call_all(wakes);
                    state = lock(&shared.state);
                }
                shared.changed.notify_all();
                continue;
            }
            state.syncing.insert(key.clone());
            return Some(Work::Sync(key));
        }
        if stopping {
            state.running = false;
            drop(state);
            shared.changed.notify_all();
            return None;
        }
        let next = state.queue.values().map(|batch| batch.due).min();
        state = match next {
            Some(due) => {
                shared
                    .changed
                    .wait_timeout(state, due.saturating_duration_since(now))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
}

fn finish_sync(shared: &Shared, key: &StorageKey, result: &Result<(), StoreError>) {
    let mut state = lock(&shared.state);
    state.syncing.remove(key);
    match result {
        Ok(()) => state.keys.entry(key.clone()).or_default().synced = true,
        // Left unsynced: the next blocking `flush` retries it and reports it.
        Err(error) => tracing::warn!(%key, %error, "could not sync stored bytes in the background"),
    }
    let wakes = state.take_settle_wakes(key);
    drop(state);
    // Before a flusher waiting on `changed` returns: whoever it tells the
    // write landed has been woken by then.
    call_all(wakes);
    shared.changed.notify_all();
}

fn finish(
    shared: &Shared,
    key: &StorageKey,
    batch: &Batch,
    op: &WriteOp,
    result: Result<(), StoreError>,
    started: Instant,
) {
    let mut state = lock(&shared.state);
    state.writing.remove(key);
    let status = state.keys.entry(key.clone()).or_default();
    match result {
        Ok(()) => {
            tracing::debug!(
                target: "tilo.state",
                event = "document_persist",
                kind = if op.is_replace() { "full" } else { "append" },
                bytes = op.bytes().len(),
                elapsed_us = started.elapsed().as_micros(),
                "persisted application document"
            );
            status.durable = status.durable.max(batch.last);
            status.synced = true;
            if op.is_replace() {
                status.poisoned = false;
                status.failed.retain(|(_, last, _)| *last > batch.last);
            }
        }
        Err(error) => {
            tracing::error!(%key, %error, "app state write failed");
            status
                .failed
                .push((batch.first, batch.last, error.to_string()));
            status.poisoned = true;
            // A write whose commit failed is not visible to later reads: the
            // next read of the key reads what is stored.
            state.cache.remove(key);
        }
    }
    let wakes = state.take_settle_wakes(key);
    drop(state);
    // Before a flusher waiting on `changed` returns: whoever it tells the
    // write landed has been woken by then.
    call_all(wakes);
    shared.changed.notify_all();
}

fn commit(shared: &Shared, taken: &Taken) -> Result<(), StoreError> {
    let batch = &taken.batch;
    if taken.poisoned && !taken.op.is_replace() {
        return Err(io_err(
            format!("appending to {}", taken.key),
            io::Error::other("an earlier write failed; a full replacement is required"),
        ));
    }
    let Some(view) = &batch.view else {
        let owner = taken.owner.as_deref().ok_or_else(|| {
            io_err(
                format!("writing {}", taken.key),
                io::Error::other("the key is no longer leased"),
            )
        })?;
        owner.check()?;
        if let Some(writer) = &taken.writer {
            let sidecar = owner.directory.join(ACTOR_FILE);
            tilo_files::native::atomic::write(&sidecar, hex::encode(writer).as_bytes())
                .map_err(|e| io_err(format!("writing {}", sidecar.display()), e))?;
        }
        let document = owner.directory.join(DOCUMENT_FILE);
        return match &*taken.op {
            WriteOp::Replace(bytes) => {
                tilo_files::native::atomic::write(&document, bytes)
                    .map_err(|e| io_err(format!("writing {}", document.display()), e))?;
                sync_ancestors(shared, &owner.directory)?;
                owner.check()
            }
            WriteOp::Append(bytes) => {
                note_append_base(shared, &taken.key, &document);
                append_synced(shared, &document, bytes, false)
            }
        };
    };
    // Atomic replacement syncs the immediate parent, not the new directories
    // above it. Establish the whole chain once before acknowledging this key.
    // A flush of an absent key can set `synced` without creating anything, so
    // only a successful prior commit proves the chain was established here.
    let ancestors_synced = lock(&shared.state)
        .keys
        .get(&taken.key)
        .is_some_and(|status| status.synced && status.durable > 0);
    if let Some(parent) = view.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| io_err(format!("creating {}", parent.display()), e))?;
    }
    match &*taken.op {
        WriteOp::Replace(bytes) => {
            // atomic::write requires a durably provisioned parent.
            if !ancestors_synced && let Some(parent) = view.parent() {
                sync_ancestors(shared, parent)?;
            }
            tilo_files::native::atomic::write(view, bytes)
                .map_err(|e| io_err(format!("writing {}", view.display()), e))?;
        }
        WriteOp::Append(bytes) => {
            note_append_base(shared, &taken.key, view);
            append_synced(shared, view, bytes, true)?;
            // Unlike replacement, append does not sync its new file's entry.
            if !ancestors_synced && let Some(parent) = view.parent() {
                sync_ancestors(shared, parent)?;
            }
        }
    }
    Ok(())
}

/// Before appending to `path` for `key`, note its length, so a reader
/// meanwhile takes only the bytes before the append from the file and the
/// rest from memory. On the writer thread: the stat may wait on the disk.
fn note_append_base(shared: &Shared, key: &StorageKey, path: &Path) {
    let len = fs::metadata(path).map_or(0, |metadata| metadata.len());
    if let Some(writing) = lock(&shared.state).writing.get_mut(key) {
        writing.base_len = Some(len);
    }
    #[cfg(test)]
    {
        let hold = lock(&shared.faults).append_hold.take();
        if let Some((reached, release)) = hold {
            let _ = reached.send(());
            let _ = release.recv();
        }
    }
}

fn append_synced(
    shared: &Shared,
    path: &Path,
    bytes: &[u8],
    create: bool,
) -> Result<(), StoreError> {
    #[cfg(not(test))]
    let _ = shared;
    if bytes.is_empty() {
        return Ok(());
    }
    let context = || format!("appending to {}", path.display());
    let mut file = OpenOptions::new()
        .create(create)
        .append(true)
        .open(path)
        .map_err(|e| io_err(context(), e))?;
    #[cfg(test)]
    let prefix = lock(&shared.faults).append_prefix.take();
    #[cfg(test)]
    if let Some(prefix) = prefix {
        let _ = file.write_all(bytes.get(..prefix.min(bytes.len())).unwrap_or_default());
        let _ = file.sync_all();
        return Err(io_err(
            "injected append failure after prefix",
            io::Error::from(io::ErrorKind::WriteZero),
        ));
    }
    file.write_all(bytes).map_err(|e| io_err(context(), e))?;
    file.sync_all()
        .map_err(|e| io_err(format!("syncing {}", path.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocStore;
    use tempfile::TempDir;

    fn record(name: &str) -> StorageKey {
        StorageKey::record("so.tilo.test", "room-a", name)
    }

    /// An idle frame loop told "not yet" hears when the answer is ready:
    /// nothing else happens on it, and its wake is called once, however
    /// often it asked, after which asking again answers.
    #[test]
    fn an_idle_thread_that_must_not_wait_is_woken_once_its_read_is_done() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        let key = record("idle");
        {
            let _lease = fs.lease(&key).unwrap();
            fs.write(&key, WriteOp::Replace(b"kept".to_vec()), Some(b"me"))
                .unwrap();
            fs.flush(Some(&key)).unwrap();
        }
        let (release, held) = std::sync::mpsc::channel();
        fs.faults().prepare_hold = Some(held);
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (notify, notified) = std::sync::mpsc::channel();
        let counted = Arc::clone(&woken);
        let wake: ReadyWake = Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = notify.send(());
        });
        {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread().waking(wake);
            assert!(matches!(fs.read(&key), Err(SeamError::Opening(_))));
            assert!(matches!(fs.read(&key), Err(SeamError::Opening(_))));
            assert!(
                notified.try_recv().is_err(),
                "woken before the work was done"
            );
            release.send(()).unwrap();
            tilo_test_env::recv("the wake once the read is done", &notified);
            let stored = fs.read(&key).unwrap().unwrap();
            assert_eq!(stored.bytes, b"kept");
            assert_eq!(stored.writer.as_deref(), Some(&b"me"[..]));
        }
        // Dropping the store joins its thread: every wake it would call has been.
        drop(fs);
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A wake that reports which thread called it.
    fn thread_wake() -> (ReadyWake, std::sync::mpsc::Receiver<thread::ThreadId>) {
        let (notify, notified) = std::sync::mpsc::channel();
        let wake: ReadyWake = Arc::new(move || {
            let _ = notify.send(thread::current().id());
        });
        (wake, notified)
    }

    /// A thread that must not wait on the disk (the compositor opening a new
    /// tab's record) is told to come back while the store's thread opens
    /// the document, is woken when it is done, and then leases and reads
    /// what that thread opened and read for it, without the disk.
    /// A window showing "Saving…" hears from the store's thread when the
    /// write lands, once, and is never woken for a key with nothing
    /// outstanding.
    #[test]
    fn a_write_landing_wakes_who_waits_on_it_once() {
        let tmp = TempDir::new().unwrap();
        // Nothing commits unless flushed: the wake cannot come early.
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let key = record("saving");
        let _lease = fs.lease(&key).unwrap();
        let (wake, notified) = thread_wake();
        assert_eq!(
            fs.wake_when_settled(&key, Arc::clone(&wake)),
            SettleWatch::Settled,
            "nothing written, nothing to wait for"
        );
        let revision = fs
            .write(&key, WriteOp::Replace(b"draft".to_vec()), Some(b"me"))
            .unwrap();
        assert_eq!(
            fs.wake_when_settled(&key, Arc::clone(&wake)),
            SettleWatch::Armed
        );
        assert_eq!(
            fs.wake_when_settled(&key, Arc::clone(&wake)),
            SettleWatch::Armed,
            "the same wake armed again is called once"
        );
        assert!(
            notified.try_recv().is_err(),
            "woken before the write landed"
        );
        fs.flush(Some(&key)).unwrap();
        // Woken before the flush returned.
        let worker = notified.try_recv().expect("the landed write's wake");
        assert_ne!(
            worker,
            thread::current().id(),
            "the store's thread wakes it"
        );
        assert_eq!(fs.durability(&key, revision), Durability::Durable);
        assert!(notified.try_recv().is_err(), "woken once");
        assert_eq!(fs.wake_when_settled(&key, wake), SettleWatch::Settled);
    }

    #[test]
    fn a_thread_that_declined_waiting_leases_what_the_stores_thread_opened() {
        let tmp = TempDir::new().unwrap();
        // Nothing commits during the test unless flushed: a read that went
        // to the disk would miss the write below.
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let key = record("fresh");
        let Target::Document(directory) = fs.target(&key).unwrap() else {
            panic!("a record is a document");
        };
        let (release, held) = std::sync::mpsc::channel();
        fs.faults().prepare_hold = Some(held);
        let (wake, notified) = thread_wake();
        durable_store::waiting::decline_on_this_thread(wake);

        // The disk is held until after these return: a caller that waited
        // for it would never return.
        assert!(matches!(fs.lease_shared(&key), Err(SeamError::Opening(_))));
        assert!(matches!(fs.read(&key), Err(SeamError::Opening(_))));
        assert!(!directory.exists(), "the caller created the directory");
        assert!(
            notified.try_recv().is_err(),
            "woken before the work was done"
        );

        release.send(()).unwrap();
        let worker = tilo_test_env::recv("the preparation's wake", &notified);
        assert_ne!(worker, thread::current().id());
        for name in [CANONICAL_GATE, LOCK_FILE] {
            assert!(directory.join(name).is_file(), "{name} was not created");
        }
        let lease = fs.lease_shared(&key).unwrap();
        assert!(lease.primary);
        assert!(lock(&fs.shared.state).prepared.is_empty());
        assert_eq!(fs.read(&key).unwrap(), None);

        // Reads follow this thread's writes without waiting for them to
        // commit.
        fs.write(&key, WriteOp::Replace(b"doc".to_vec()), Some(b"actor"))
            .unwrap();
        fs.write(&key, WriteOp::Append(b"+1".to_vec()), None)
            .unwrap();
        assert!(lock(&fs.shared.state).queue.contains_key(&key));
        assert_eq!(
            fs.read(&key).unwrap(),
            Some(Stored {
                bytes: b"doc+1".to_vec(),
                writer: Some(b"actor".to_vec()),
            })
        );
        // A hint handed over is kept by the store's thread.
        fs.write_hint(&key, b"hint").unwrap();
        assert_eq!(fs.read_hint(&key).as_deref(), Some(b"hint".as_slice()));

        // Another thread still works in place.
        thread::scope(|scope| {
            scope
                .spawn(|| {
                    let other = fs.lease_shared(&key).unwrap();
                    assert!(!other.primary, "the declined thread holds the writer lock");
                })
                .join()
                .unwrap();
        });
        fs.flush(Some(&key)).unwrap();
        assert_eq!(fs::read(directory.join(DOCUMENT_FILE)).unwrap(), b"doc+1");
        drop(lease);
        assert!(lock(&fs.shared.state).cache.is_empty());
        assert_eq!(
            fs::read(directory.join(LOAD_HINT_FILE)).unwrap(),
            b"hint",
            "the hint was not kept"
        );
    }

    /// What the thread that declined waiting reads includes writes another
    /// owner in this store accepted and has yet to commit.
    #[test]
    fn a_read_without_waiting_includes_writes_still_queued() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let view = StorageKey::view("device", "room-a", 3);
        let (written, wait_written) = std::sync::mpsc::channel();
        let (done, wait_done) = std::sync::mpsc::channel::<()>();
        thread::scope(|scope| {
            let fs = &fs;
            let view = &view;
            scope.spawn(move || {
                let _lease = fs.lease(view).unwrap();
                fs.write(view, WriteOp::Replace(b"layout".to_vec()), None)
                    .unwrap();
                fs.flush(Some(view)).unwrap();
                fs.write(view, WriteOp::Append(b"+queued".to_vec()), None)
                    .unwrap();
                written.send(()).unwrap();
                let _ = wait_done.recv();
            });
            tilo_test_env::recv("the other owner's writes", &wait_written);
            let (wake, notified) = thread_wake();
            durable_store::waiting::decline_on_this_thread(wake);
            assert!(matches!(fs.read(view), Err(SeamError::Opening(_))));
            tilo_test_env::recv("the read's wake", &notified);
            assert_eq!(
                fs.read(view).unwrap().map(|stored| stored.bytes),
                Some(b"layout+queued".to_vec())
            );
            // A snapshot read only to be read is handed over once.
            assert!(matches!(fs.read(view), Err(SeamError::Opening(_))));
            tilo_test_env::recv("the read's wake", &notified);
            done.send(()).unwrap();
        });
    }

    /// A gate another process holds refuses the lease once the store's
    /// thread has opened it, as it does a caller that waits.
    #[test]
    fn a_lease_without_waiting_is_refused_while_another_process_holds_the_gate() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        let key = record("held");
        let Target::Document(directory) = fs.target(&key).unwrap() else {
            panic!("a record is a document");
        };
        fs::create_dir_all(&directory).unwrap();
        // Another open of the gate is another description: its lock
        // conflicts as another process's would.
        let _elsewhere = advisory_lock::FileLock::open(
            &directory.join(CANONICAL_GATE),
            advisory_lock::LockMode::Exclusive,
        )
        .unwrap();
        let (wake, notified) = thread_wake();
        durable_store::waiting::decline_on_this_thread(wake);
        assert!(matches!(fs.lease(&key), Err(SeamError::Opening(_))));
        tilo_test_env::recv("the preparation's wake", &notified);
        assert!(matches!(fs.lease(&key), Err(SeamError::Busy(_))));
    }

    /// A frame loop reads back what it just wrote while the store's thread
    /// has yet to commit it, and while it commits: from memory, never by
    /// waiting for that thread. The strict guard fails the test on a wait.
    #[test]
    fn a_read_on_a_thread_that_must_not_wait_sees_unwritten_writes() {
        let tmp = TempDir::new().unwrap();
        // A long debounce: nothing commits unless asked to.
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let key = record("typed");
        let _lease = fs.lease(&key).unwrap();
        fs.write(&key, WriteOp::Replace(b"ab".to_vec()), Some(b"me"))
            .unwrap();
        {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            let stored = fs.read(&key).unwrap().unwrap();
            assert_eq!(stored.bytes, b"ab", "a queued replacement");
            assert_eq!(stored.writer.as_deref(), Some(&b"me"[..]));
        }
        fs.flush(Some(&key)).unwrap();

        // An append the writer thread holds after noting where it starts.
        let (reached_tx, reached) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        fs.faults().append_hold = Some((reached_tx, held));
        fs.write(&key, WriteOp::Append(b"cd".to_vec()), None)
            .unwrap();
        let (wake, notified) = thread_wake();
        {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread().waking(wake);
            assert!(matches!(
                fs.flush_where_allowed(&key),
                Err(SeamError::Pending(_))
            ));
            tilo_test_env::recv("the writer thread to take the append", &reached);
            fs.write(&key, WriteOp::Append(b"ef".to_vec()), None)
                .unwrap();
            assert!(
                matches!(fs.read(&key), Err(SeamError::Opening(_))),
                "what only the file holds is the store's thread to read"
            );
        }
        let stored = fs.read(&key).unwrap().unwrap();
        assert_eq!(
            stored.bytes, b"abcdef",
            "the file, the append under way and the queued one, once each"
        );
        assert_eq!(stored.writer.as_deref(), Some(&b"me"[..]), "from the file");
        release.send(()).unwrap();
        tilo_test_env::recv("the store's thread to read the file", &notified);
        {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            let stored = fs.read(&key).unwrap().unwrap();
            assert_eq!(
                stored.bytes, b"abcdef",
                "read for it: the file with the queued append, once"
            );
            assert_eq!(stored.writer.as_deref(), Some(&b"me"[..]), "from the file");
        }
        fs.flush(Some(&key)).unwrap();
        assert_eq!(fs.read(&key).unwrap().unwrap().bytes, b"abcdef");
        let Target::Document(directory) = fs.target(&key).unwrap() else {
            panic!("a record is a document");
        };
        assert_eq!(fs::read(directory.join(DOCUMENT_FILE)).unwrap(), b"abcdef");
    }

    /// A caller that may not wait lets go of its lease while its last write
    /// is still queued: the write lands anyway, and nobody else, in this
    /// process or another, can lease the document until it has.
    #[test]
    fn a_write_outlives_its_lease_and_keeps_the_document_until_it_lands() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let key = record("closing");
        let lease = fs.lease(&key).unwrap();
        let revision = fs
            .write(&key, WriteOp::Replace(b"last words".to_vec()), Some(b"me"))
            .unwrap();
        drop(lease);
        let other = FsStore::new(tmp.path());
        assert!(
            matches!(other.lease(&key), Err(SeamError::Busy(_))),
            "another process waits for the queued write"
        );
        assert!(
            matches!(fs.lease(&key), Err(SeamError::Busy(_))),
            "so does a new owner in this one"
        );
        fs.flush(Some(&key)).unwrap();
        assert_eq!(fs.durability(&key, revision), Durability::Durable);
        drop(other.lease(&key).unwrap());
        let Target::Document(directory) = fs.target(&key).unwrap() else {
            panic!("a record is a document");
        };
        assert_eq!(
            fs::read(directory.join(DOCUMENT_FILE)).unwrap(),
            b"last words"
        );
    }

    /// A blocking flush on a thread that must not wait is reported: a test
    /// that forbids waits strictly fails, naming the flush.
    #[test]
    fn a_blocking_flush_on_a_thread_that_must_not_wait_is_refused() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::from_secs(3600));
        let key = record("waited");
        let _lease = fs.lease(&key).unwrap();
        fs.write(&key, WriteOp::Replace(b"x".to_vec()), None)
            .unwrap();
        let refused = {
            let _loop = durable_store::waiting::forbid_strictly_on_this_thread();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fs.flush(Some(&key))))
        };
        let message = refused.expect_err("the flush waited on a loop thread");
        let message = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(message.contains("FsStore::flush"), "{message}");
        fs.flush(Some(&key)).unwrap();
    }

    #[test]
    fn a_failed_preparation_is_reported_to_the_next_caller() {
        let tmp = TempDir::new().unwrap();
        // The package directory cannot be created: a file stands there.
        fs::write(tmp.path().join("so.tilo.test"), b"not a directory").unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        let key = record("blocked");
        let (wake, notified) = thread_wake();
        durable_store::waiting::decline_on_this_thread(wake);
        assert!(matches!(fs.lease_shared(&key), Err(SeamError::Opening(_))));
        tilo_test_env::recv("the preparation's wake", &notified);
        assert!(matches!(
            fs.lease_shared(&key),
            Err(SeamError::Failed { .. })
        ));
        // Reported once; the next call tries again.
        assert!(matches!(fs.lease_shared(&key), Err(SeamError::Opening(_))));
    }

    #[test]
    fn records_lists_one_rooms_committed_documents_sorted() {
        let tmp = TempDir::new().unwrap();
        let store = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        assert!(store.records("so.tilo.test", "room-a").unwrap().is_empty());
        for (room, name, bytes) in [
            ("room-a", "b-rec", b"doc".as_slice()),
            ("room-a", "a-rec", b"doc"),
            ("room-a", "empty", b""),
            ("room-b", "elsewhere", b"doc"),
        ] {
            let key = StorageKey::record("so.tilo.test", room, name);
            let _lease = store.lease(&key).unwrap();
            store
                .write(&key, WriteOp::Replace(bytes.to_vec()), None)
                .unwrap();
            store.flush(Some(&key)).unwrap();
        }
        assert_eq!(
            store.records("so.tilo.test", "room-a").unwrap(),
            ["a-rec", "b-rec"]
        );
        assert!(store.records("so.tilo.test", "../room-a").is_err());
    }

    #[test]
    fn accepted_writes_become_durable_on_debounce_flush_drop_and_reopen() {
        // Phase `passes_the_shared_conformance_suite_and_survives_reopening`.
        {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("profile");
            let store = FsStore::with_debounce(&root, Duration::from_millis(5));
            let subject = durable_store::conformance::Subject {
                store: &store,
                settle: &|| {},
            };
            durable_store::conformance::run(&subject);
            durable_store::conformance::reopen(&subject, &|| Box::new(FsStore::new(&root)));
        }
        // Phase `writes_coalesce_until_their_debounce_or_a_flush`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::with_debounce(tmp.path(), Duration::from_secs(600));
            let key = StorageKey::view("D", "A", 1);
            let first = store
                .write(&key, WriteOp::Replace(b"a".to_vec()), None)
                .unwrap();
            let second = store
                .write(&key, WriteOp::Append(b"b".to_vec()), None)
                .unwrap();
            assert_eq!(store.durability(&key, first), Durability::Pending);
            assert!(!tmp.path().join(INSTANCE_VIEWS_DIR).exists());
            store.flush(Some(&key)).unwrap();
            assert_eq!(store.durability(&key, first), Durability::Durable);
            assert_eq!(store.durability(&key, second), Durability::Durable);
            assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"ab");
        }
        // Phase `dropping_the_store_commits_accepted_writes`.
        {
            let tmp = TempDir::new().unwrap();
            let key = StorageKey::view("D", "A", 1);
            {
                let store = FsStore::with_debounce(tmp.path(), Duration::from_secs(600));
                store
                    .write(&key, WriteOp::Replace(b"kept".to_vec()), None)
                    .unwrap();
            }
            assert_eq!(
                FsStore::new(tmp.path()).read(&key).unwrap().unwrap().bytes,
                b"kept"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn keys_map_onto_profile_and_launch_layouts_and_invalid_keys_are_refused() {
        // Phase `keys_map_onto_the_existing_profile_layout`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::new(tmp.path());
            let key = record("rec");
            let lease = store.lease(&key).unwrap();
            store
                .write(&key, WriteOp::Replace(b"doc".to_vec()), Some(&[0xab, 0x01]))
                .unwrap();
            let view = StorageKey::view("D", "A", 3);
            store
                .write(&view, WriteOp::Replace(b"view".to_vec()), None)
                .unwrap();
            store.flush(None).unwrap();
            let directory =
                DocStore::room_record_dir_at(tmp.path(), "so.tilo.test", "room-a", "rec").unwrap();
            assert_eq!(fs::read(directory.join(DOCUMENT_FILE)).unwrap(), b"doc");
            assert_eq!(fs::read(directory.join(ACTOR_FILE)).unwrap(), b"ab01");
            assert!(directory.join(CANONICAL_GATE).is_file());
            assert_eq!(
                fs::read(tmp.path().join("instance-views/44/41/3.postcard")).unwrap(),
                b"view"
            );
            // The gate is the one legacy and canonical openers already respect.
            assert!(matches!(
                DocStore::open_canonical_at("so.tilo.test", &directory),
                Err(StoreError::WriterBusy { .. })
            ));
            drop(lease);
            assert!(DocStore::open_canonical_at("so.tilo.test", &directory).is_ok());
        }
        // Phase `package_scoped_keys_map_onto_the_launch_environment_layout`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::with_debounce(tmp.path(), Duration::ZERO);
            let package = tmp.path().join("so.tilo.test");
            for (key, directory) in [
                (StorageKey::package("so.tilo.test"), package.clone()),
                (
                    StorageKey::room_index("so.tilo.test", "room-a"),
                    package.join(ROOMS_DIR).join("room-a"),
                ),
            ] {
                let shared = store.lease_shared(&key).unwrap();
                assert!(shared.primary);
                store
                    .write(&key, WriteOp::Replace(b"doc".to_vec()), None)
                    .unwrap();
                store.flush(Some(&key)).unwrap();
                assert_eq!(fs::read(directory.join(DOCUMENT_FILE)).unwrap(), b"doc");
                assert!(directory.join(LOCK_FILE).is_file());
                assert_eq!(store.local_path(&key), Some(directory.join(DOCUMENT_FILE)));
            }
        }
        // Phase `record_writes_require_a_lease_and_invalid_keys_are_refused`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::new(tmp.path());
            assert!(matches!(
                store.write(&record("rec"), WriteOp::Replace(vec![1]), None),
                Err(SeamError::NotLeased(_))
            ));
            for key in [record("../escape"), record(""), record("a/b")] {
                assert!(store.lease(&key).is_err(), "{key}");
                assert!(store.read(&key).is_err(), "{key}");
            }
            let relative = FsStore::new("relative/profile");
            assert!(relative.lease(&record("rec")).is_err());
            assert!(
                relative
                    .write(
                        &StorageKey::view("D", "A", 1),
                        WriteOp::Replace(vec![1]),
                        None
                    )
                    .is_err()
            );
        }
        // Phase `hex_round_trips_and_rejects_malformed_sidecars`.
        {
            assert_eq!(
                unhex(&hex::encode([0, 0xff, 0x10])),
                Some(vec![0, 0xff, 0x10])
            );
            for bad in ["", "a", "zz", "é0"] {
                assert_eq!(unhex(bad), None, "{bad}");
            }
        }
    }

    #[test]
    fn a_dropped_view_lease_keeps_queued_commits_exclusive_until_settled() {
        let tmp = TempDir::new().unwrap();
        let store = FsStore::with_debounce(tmp.path(), Duration::from_secs(600));
        let peer = FsStore::new(tmp.path());
        let key = StorageKey::view("private-action-history", "owner", 0);
        let lease = store.lease(&key).unwrap();
        let revision = store
            .write(&key, WriteOp::Replace(b"evidence".to_vec()), None)
            .unwrap();
        drop(lease);
        assert!(matches!(peer.lease(&key), Err(SeamError::Busy(_))));
        store.flush(Some(&key)).unwrap();
        assert_eq!(store.durability(&key, revision), Durability::Durable);
        // The worker must release the gate before flush wakes its caller.
        let _next = peer.lease(&key).unwrap();
        assert_eq!(peer.read(&key).unwrap().unwrap().bytes, b"evidence");
    }

    #[test]
    fn view_creation_syncs_new_ancestors_once_and_retries_failed_durability() {
        for initial in [
            WriteOp::Replace(b"checkpoint".to_vec()),
            WriteOp::Append(b"checkpoint".to_vec()),
        ] {
            let tmp = TempDir::new().unwrap();
            let ancestor = fs::canonicalize(tmp.path()).unwrap().join("new");
            let root = ancestor.join("profile");
            let store = FsStore::with_debounce(&root, Duration::from_secs(600));
            let key = StorageKey::view("D", "A", 1);
            // Even an earlier flush of a missing key does not prove that its
            // not-yet-created parent chain is durable.
            store.flush(Some(&key)).unwrap();
            assert!(!ancestor.exists());
            store.faults().sync_failure = Some(ancestor.clone());
            let replacement = initial.is_replace();
            let first = store.write(&key, initial, None).unwrap();
            assert!(store.flush(Some(&key)).is_err());
            assert!(matches!(
                store.durability(&key, first),
                Durability::Failed(message) if message.contains("injected directory sync failure")
            ));
            assert!(!store.is_settled(&key));
            if replacement {
                assert!(
                    store.read(&key).unwrap().is_none(),
                    "no publication before durable parents"
                );
            } else {
                assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"checkpoint");
            }

            {
                let mut faults = store.faults();
                faults.sync_failure = None;
                faults.sync_threads.clear();
            }
            let retry = store
                .write(&key, WriteOp::Replace(b"checkpoint".to_vec()), None)
                .unwrap();
            store.flush(Some(&key)).unwrap();
            assert_eq!(store.durability(&key, retry), Durability::Durable);
            assert!(store.is_settled(&key));
            assert!(!store.faults().sync_threads.is_empty());

            // Re-arm the same ancestor failure: later edits must not revisit
            // the established chain (the atomic rename still syncs its parent).
            {
                let mut faults = store.faults();
                faults.sync_threads.clear();
                faults.sync_failure = Some(ancestor);
            }
            for edit in [
                WriteOp::Replace(b"next".to_vec()),
                WriteOp::Append(b" edit".to_vec()),
            ] {
                let revision = store.write(&key, edit, None).unwrap();
                store.flush(Some(&key)).unwrap();
                assert_eq!(store.durability(&key, revision), Durability::Durable);
                assert!(store.faults().sync_threads.is_empty());
            }
            assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"next edit");
        }
    }

    #[test]
    fn failed_writes_stay_unacknowledged_until_a_safe_replacement() {
        // Phase `a_torn_append_requires_a_replacement_without_duplicating_bytes`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::with_debounce(tmp.path(), Duration::from_secs(600));
            let key = record("rec");
            let _lease = store.lease(&key).unwrap();
            let base = store
                .write(&key, WriteOp::Replace(b"base".to_vec()), None)
                .unwrap();
            store.flush(Some(&key)).unwrap();
            store.faults().append_prefix = Some(2);
            let torn = store
                .write(&key, WriteOp::Append(b"+one".to_vec()), None)
                .unwrap();
            assert!(store.flush(Some(&key)).is_err());
            assert!(matches!(
                store.durability(&key, torn),
                Durability::Failed(_)
            ));
            assert_eq!(store.durability(&key, base), Durability::Durable);
            let refused = store
                .write(&key, WriteOp::Append(b"+two".to_vec()), None)
                .unwrap();
            assert!(store.flush(Some(&key)).is_err());
            assert!(matches!(
                store.durability(&key, refused),
                Durability::Failed(_)
            ));
            assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"base+o");
            let replaced = store
                .write(&key, WriteOp::Replace(b"base+one+two".to_vec()), None)
                .unwrap();
            store.flush(Some(&key)).unwrap();
            assert_eq!(store.durability(&key, replaced), Durability::Durable);
            assert_eq!(store.read(&key).unwrap().unwrap().bytes, b"base+one+two");
        }
        // Phase `directory_sync_failure_leaves_the_replacement_unacknowledged`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::with_debounce(tmp.path(), Duration::ZERO);
            let key = record("rec");
            let _lease = store.lease(&key).unwrap();
            let directory =
                DocStore::room_record_dir_at(tmp.path(), "so.tilo.test", "room-a", "rec").unwrap();
            store.faults().sync_failure = Some(fs::canonicalize(&directory).unwrap());
            let revision = store
                .write(&key, WriteOp::Replace(b"visible".to_vec()), None)
                .unwrap();
            assert!(store.flush(Some(&key)).is_err());
            assert!(matches!(
                store.durability(&key, revision),
                Durability::Failed(_)
            ));
            store.faults().sync_failure = None;
            let retry = store
                .write(&key, WriteOp::Replace(b"visible".to_vec()), None)
                .unwrap();
            store.flush(Some(&key)).unwrap();
            assert_eq!(store.durability(&key, retry), Durability::Durable);
        }
        // Phase `a_replaced_record_directory_is_never_written_or_recreated`.
        {
            let tmp = TempDir::new().unwrap();
            let store = FsStore::with_debounce(tmp.path(), Duration::ZERO);
            let key = record("rec");
            let _lease = store.lease(&key).unwrap();
            let directory =
                DocStore::room_record_dir_at(tmp.path(), "so.tilo.test", "room-a", "rec").unwrap();
            fs::remove_file(directory.join(CANONICAL_GATE)).unwrap();
            fs::write(directory.join(CANONICAL_GATE), b"replacement").unwrap();
            store
                .write(&key, WriteOp::Replace(b"forbidden".to_vec()), None)
                .unwrap();
            assert!(store.flush(Some(&key)).is_err());
            assert!(!directory.join(DOCUMENT_FILE).exists());
        }
    }
}
