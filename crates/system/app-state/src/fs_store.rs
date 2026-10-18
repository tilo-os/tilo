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
//! The same thread opens documents for a caller that must not wait on the
//! disk ([`DurableStore::prepare_shared`]): it creates the directory and its
//! gate and lock files and reads what an opener reads, so the caller's lease
//! then creates nothing and finds its reads in the page cache.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use durable_store::{
    Durability, DurableStore, Lease, ReadyWake, Revision, SharedLease, StorageKey,
    StoreError as SeamError, Stored, WriteOp,
};

use crate::{
    ACTOR_FILE, CANONICAL_GATE, DEFAULT_DEBOUNCE, DOCUMENT_FILE, LOAD_HINT_FILE, LOCK_FILE,
    RECORD_SUCCESSOR_CLAIM, RECORDS_DIR, ROOMS_DIR, StoreError, acquire_gate, acquire_writer_lock,
    claim_marker, ensure_state_dir, io_err, open_directory, same_file, sync_directory_ancestors,
    validate_absolute_directory, validate_package_id, validate_record_key,
};
use tilo_lock::MutexRecover as _;

/// Directory below the profile root holding every instance's local view.
pub const INSTANCE_VIEWS_DIR: &str = "instance-views";

/// A [`DurableStore`] rooted at one profile directory. See the module docs
/// for the layout. Reads observe accepted writes by committing them first; a
/// write whose commit failed is not visible to later reads.
pub struct FsStore {
    root: PathBuf,
    namespace: String,
    shared: Arc<Shared>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

struct Shared {
    root: PathBuf,
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
}

#[derive(Default)]
struct State {
    issued: u64,
    queue: BTreeMap<StorageKey, Batch>,
    writing: BTreeMap<StorageKey, (u64, u64)>,
    keys: BTreeMap<StorageKey, KeyStatus>,
    owners: BTreeMap<StorageKey, Vec<Arc<Owner>>>,
    view_leases: BTreeMap<StorageKey, Arc<advisory_lock::FileLock>>,
    /// Keys whose existing bytes, read here rather than written here, still
    /// need their file and ancestors synced: taken by the writer thread, so
    /// no caller waits on the disk for them.
    sync_queue: BTreeSet<StorageKey>,
    syncing: BTreeSet<StorageKey>,
    /// Documents being opened for a caller that would not wait
    /// ([`DurableStore::prepare_shared`]), and what came of it.
    prepared: BTreeMap<StorageKey, Preparation>,
    /// Keys whose preparation the writer thread has yet to take.
    prepare_queue: VecDeque<StorageKey>,
    running: bool,
    stopping: bool,
}

impl State {
    fn busy(&self, key: &StorageKey) -> bool {
        self.queue.contains_key(key) || self.writing.contains_key(key)
    }
}

enum Preparation {
    /// On its way; each waker is called once it is done.
    Queued(Vec<ReadyWake>),
    /// The directory and its gate and lock files exist, and what an opener
    /// reads was just read.
    Ready,
    /// The work failed; reported once, to the next caller.
    Failed(String),
}

struct Batch {
    // A queued commit keeps its view slot exclusive even after the caller drops.
    view_gate: Option<Arc<advisory_lock::FileLock>>,
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
        match &self.owner {
            Some(owner) => {
                if let Some(live) = state.owners.get_mut(&self.key) {
                    live.retain(|candidate| !Arc::ptr_eq(candidate, owner));
                    if live.is_empty() {
                        state.owners.remove(&self.key);
                    }
                }
            }
            None => {
                state.view_leases.remove(&self.key);
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
        let root = profile_root.into();
        Self {
            namespace: root.to_string_lossy().into_owned(),
            root: root.clone(),
            shared: Arc::new(Shared {
                root,
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

fn seam_to_store(error: SeamError) -> StoreError {
    StoreError::from(error)
}

impl DurableStore for FsStore {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, SeamError> {
        let target = self.target(key)?;
        if lock(&self.shared.state).busy(key) {
            // Read-your-writes: commit what was accepted before reading it back.
            let _ = self.flush(Some(key));
        }
        let (path, writer) = match target {
            Target::Document(directory) => (
                directory.join(DOCUMENT_FILE),
                Some(directory.join(ACTOR_FILE)),
            ),
            Target::View(path) => (path, None),
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
            let writer = writer.map(<[u8]>::to_vec);
            let view_gate = state.view_leases.get(key).cloned();
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
        while state
            .queue
            .keys()
            .chain(state.writing.keys())
            .chain(state.sync_queue.iter())
            .chain(state.syncing.iter())
            .any(&matches)
        {
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

    fn lease(&self, key: &StorageKey) -> Result<Lease, SeamError> {
        match self.target(key)? {
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
        match self.target(key)? {
            Target::Document(directory) => {
                if lock(&self.shared.state)
                    .owners
                    .get(key)
                    .is_some_and(|live| live.iter().any(|owner| owner.exclusive))
                {
                    return Err(SeamError::Busy(key.to_string()));
                }
                // A prepared directory exists with its gate and lock files:
                // opening it creates nothing.
                let prepared = matches!(
                    lock(&self.shared.state).prepared.remove(key),
                    Some(Preparation::Ready)
                );
                let owner = Arc::new(open_shared_owner(&directory, !prepared)?);
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

    fn prepare_shared(&self, key: &StorageKey, ready: &ReadyWake) -> Result<(), SeamError> {
        let Target::Document(_) = self.target(key)? else {
            return Ok(());
        };
        let mut state = lock(&self.shared.state);
        if state.owners.contains_key(key) {
            // Already open here: the directory and its files exist.
            return Ok(());
        }
        match state.prepared.remove(key) {
            Some(Preparation::Ready) => {
                state.prepared.insert(key.clone(), Preparation::Ready);
                return Ok(());
            }
            Some(Preparation::Queued(mut waiting)) => {
                waiting.push(Arc::clone(ready));
                state
                    .prepared
                    .insert(key.clone(), Preparation::Queued(waiting));
                return Err(SeamError::Opening(key.to_string()));
            }
            Some(Preparation::Failed(message)) => {
                return Err(SeamError::Failed {
                    context: format!("opening {key}"),
                    message,
                });
            }
            None => {}
        }
        state
            .prepared
            .insert(key.clone(), Preparation::Queued(vec![Arc::clone(ready)]));
        state.prepare_queue.push_back(key.clone());
        drop(state);
        if let Err(error) = self.ensure_worker() {
            let mut state = lock(&self.shared.state);
            state.prepared.remove(key);
            state.prepare_queue.retain(|queued| queued != key);
            drop(state);
            return Err(error);
        }
        self.shared.changed.notify_all();
        Err(SeamError::Opening(key.to_string()))
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
        let path = directory.join(LOAD_HINT_FILE);
        let staged = directory.join(format!("{LOAD_HINT_FILE}.tmp"));
        fs::write(&staged, hint)
            .and_then(|()| fs::rename(&staged, &path))
            .map_err(|e| io_err(format!("writing {}", path.display()), e).into())
    }

    fn read_hint(&self, key: &StorageKey) -> Option<Vec<u8>> {
        let Target::Document(directory) = self.target(key).ok()? else {
            return None;
        };
        fs::read(directory.join(LOAD_HINT_FILE)).ok()
    }
}

impl FsStore {
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
        || state.writing.get(key).is_some_and(|range| covers(*range))
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
/// `create` makes the directory first; a prepared one already exists.
fn open_shared_owner(directory: &Path, create: bool) -> Result<Owner, StoreError> {
    if create {
        ensure_state_dir(directory)?;
    }
    let handle =
        open_directory(directory, false).map_err(|e| io_err("opening state directory", e))?;
    let gate = acquire_gate(&directory.join(CANONICAL_GATE), false)?;
    let writer_lock = acquire_writer_lock(&directory.join(LOCK_FILE));
    if writer_lock.is_none() {
        tracing::warn!(
            directory = %directory.display(),
            "another instance holds the state lock; running on a fresh automerge actor"
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
    sync_directory_ancestors(directory, |path| {
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
        drop(state);
        self.shared.changed.notify_all();
    }
}

/// What the writer thread does next.
enum Work {
    Commit(Taken),
    /// Sync the key's file and ancestors: bytes it read, not wrote.
    Sync(StorageKey),
    /// Open the key's document for a caller that would not wait.
    Prepare(StorageKey),
}

struct Taken {
    key: StorageKey,
    batch: Batch,
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
            Work::Prepare(key) => {
                let result = prepare_in(shared, &key);
                finish_prepare(shared, &key, result);
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
            owner,
            ..
        } = taken;
        drop(owner);
        drop(batch.view_gate.take());
        finish(shared, &key, &batch, result, started);
    }
    guard.finished = true;
}

fn next_work(shared: &Shared) -> Option<Work> {
    let mut state = lock(&shared.state);
    loop {
        // An opener is waiting on these (a new tab, say); a commit is not.
        if let Some(key) = state.prepare_queue.pop_front() {
            return Some(Work::Prepare(key));
        }
        let now = Instant::now();
        let stopping = state.stopping;
        let due = state
            .queue
            .iter()
            .find(|(_, batch)| stopping || batch.urgent || batch.due <= now)
            .map(|(key, _)| key.clone());
        if let Some(key) = due
            && let Some(batch) = state.queue.remove(&key)
        {
            state.writing.insert(key.clone(), (batch.first, batch.last));
            let owner = state
                .owners
                .get(&key)
                .and_then(|live| live.first())
                .cloned();
            let poisoned = state.keys.get(&key).is_some_and(|status| status.poisoned);
            return Some(Work::Commit(Taken {
                key,
                batch,
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

/// Create `key`'s directory with its gate and lock files, and read what an
/// opener reads, so that a lease and its reads, on the caller's thread,
/// create nothing and find the bytes cached. Nothing is held afterwards: no
/// lock is taken, so no other process is kept from becoming primary.
fn prepare_in(shared: &Shared, key: &StorageKey) -> Result<(), StoreError> {
    #[cfg(test)]
    {
        let hold = lock(&shared.faults).prepare_hold.take();
        if let Some(hold) = hold {
            let _ = hold.recv();
        }
    }
    let Target::Document(directory) = target_in(&shared.root, key).map_err(seam_to_store)? else {
        return Ok(());
    };
    ensure_state_dir(&directory)?;
    for name in [CANONICAL_GATE, LOCK_FILE] {
        let path = directory.join(name);
        advisory_lock::open_lock_file(&path)
            .map_err(|e| io_err(format!("creating {}", path.display()), e))?;
    }
    for name in [DOCUMENT_FILE, ACTOR_FILE, LOAD_HINT_FILE] {
        // Only to have them cached: the opener reads them again and reports
        // what it finds.
        let _ = fs::read(directory.join(name));
    }
    Ok(())
}

fn finish_prepare(shared: &Shared, key: &StorageKey, result: Result<(), StoreError>) {
    let outcome = match result {
        Ok(()) => Preparation::Ready,
        Err(error) => Preparation::Failed(error.to_string()),
    };
    let waiting = {
        let mut state = lock(&shared.state);
        match state.prepared.insert(key.clone(), outcome) {
            Some(Preparation::Queued(waiting)) => waiting,
            _ => Vec::new(),
        }
    };
    for ready in waiting {
        ready();
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
    drop(state);
    shared.changed.notify_all();
}

fn finish(
    shared: &Shared,
    key: &StorageKey,
    batch: &Batch,
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
                kind = if batch.op.is_replace() { "full" } else { "append" },
                bytes = batch.op.bytes().len(),
                elapsed_us = started.elapsed().as_micros(),
                "persisted application document"
            );
            status.durable = status.durable.max(batch.last);
            status.synced = true;
            if batch.op.is_replace() {
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
        }
    }
    drop(state);
    shared.changed.notify_all();
}

fn commit(shared: &Shared, taken: &Taken) -> Result<(), StoreError> {
    let batch = &taken.batch;
    if taken.poisoned && !batch.op.is_replace() {
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
        if let Some(writer) = &batch.writer {
            let sidecar = owner.directory.join(ACTOR_FILE);
            tilo_files::native::atomic::write(&sidecar, hex::encode(writer).as_bytes())
                .map_err(|e| io_err(format!("writing {}", sidecar.display()), e))?;
        }
        let document = owner.directory.join(DOCUMENT_FILE);
        return match &batch.op {
            WriteOp::Replace(bytes) => {
                tilo_files::native::atomic::write(&document, bytes)
                    .map_err(|e| io_err(format!("writing {}", document.display()), e))?;
                sync_ancestors(shared, &owner.directory)?;
                owner.check()
            }
            WriteOp::Append(bytes) => append_synced(shared, &document, bytes, false),
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
    match &batch.op {
        WriteOp::Replace(bytes) => {
            // atomic::write requires a durably provisioned parent.
            if !ancestors_synced && let Some(parent) = view.parent() {
                sync_ancestors(shared, parent)?;
            }
            tilo_files::native::atomic::write(view, bytes)
                .map_err(|e| io_err(format!("writing {}", view.display()), e))?;
        }
        WriteOp::Append(bytes) => {
            append_synced(shared, view, bytes, true)?;
            // Unlike replacement, append does not sync its new file's entry.
            if !ancestors_synced && let Some(parent) = view.parent() {
                sync_ancestors(shared, parent)?;
            }
        }
    }
    Ok(())
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

    /// A caller that must not wait on the disk (the compositor opening a new
    /// tab's record) is told to come back while the store's thread opens
    /// the document, is woken when it is done, and then leases a directory
    /// that already exists.
    #[test]
    fn preparing_a_document_leaves_the_disk_to_the_stores_thread_and_wakes_the_caller() {
        let tmp = TempDir::new().unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        let key = record("fresh");
        let Target::Document(directory) = fs.target(&key).unwrap() else {
            panic!("a record is a document");
        };
        let (release, held) = std::sync::mpsc::channel();
        fs.faults().prepare_hold = Some(held);
        let (notify, notified) = std::sync::mpsc::channel();
        let wake: ReadyWake = Arc::new(move || {
            let _ = notify.send(thread::current().id());
        });

        let started = Instant::now();
        assert!(matches!(
            fs.prepare_shared(&key, &wake),
            Err(SeamError::Opening(_))
        ));
        assert!(matches!(
            fs.prepare_shared(&key, &wake),
            Err(SeamError::Opening(_))
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the caller waited for the held disk"
        );
        assert!(!directory.exists(), "the caller created the directory");
        assert!(
            notified.try_recv().is_err(),
            "woken before the work was done"
        );

        release.send(()).unwrap();
        let worker = notified.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_ne!(worker, thread::current().id());
        // Both callers that asked are notified.
        notified.recv_timeout(Duration::from_secs(10)).unwrap();
        for name in [CANONICAL_GATE, LOCK_FILE] {
            assert!(directory.join(name).is_file(), "{name} was not created");
        }
        fs.prepare_shared(&key, &wake).unwrap();
        let lease = fs.lease_shared(&key).unwrap();
        assert!(lease.primary);
        // The lease took the preparation; the key is open now, so asking
        // again needs no work.
        assert!(lock(&fs.shared.state).prepared.is_empty());
        fs.prepare_shared(&key, &wake).unwrap();
    }

    #[test]
    fn a_failed_preparation_is_reported_to_the_next_caller() {
        let tmp = TempDir::new().unwrap();
        // The package directory cannot be created: a file stands there.
        fs::write(tmp.path().join("so.tilo.test"), b"not a directory").unwrap();
        let fs = FsStore::with_debounce(tmp.path(), Duration::ZERO);
        let key = record("blocked");
        let (notify, notified) = std::sync::mpsc::channel();
        let wake: ReadyWake = Arc::new(move || {
            let _ = notify.send(());
        });
        assert!(matches!(
            fs.prepare_shared(&key, &wake),
            Err(SeamError::Opening(_))
        ));
        notified.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(
            fs.prepare_shared(&key, &wake),
            Err(SeamError::Failed { .. })
        ));
        // Reported once; the next call tries again.
        assert!(matches!(
            fs.prepare_shared(&key, &wake),
            Err(SeamError::Opening(_))
        ));
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
