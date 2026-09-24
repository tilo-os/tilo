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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use durable_store::{
    Durability, DurableStore, Lease, Revision, StorageKey, StoreError as SeamError, Stored, WriteOp,
};

use crate::{
    ACTOR_FILE, CANONICAL_GATE, DEFAULT_DEBOUNCE, DOCUMENT_FILE, DocStore, StoreError,
    acquire_gate, io_err, open_directory, same_file, sync_directory_ancestors,
    validate_absolute_directory,
};

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
}

#[derive(Default)]
struct State {
    issued: u64,
    queue: BTreeMap<StorageKey, Batch>,
    writing: BTreeMap<StorageKey, (u64, u64)>,
    keys: BTreeMap<StorageKey, KeyStatus>,
    owners: BTreeMap<StorageKey, Arc<Owner>>,
    view_leases: BTreeSet<StorageKey>,
    running: bool,
    stopping: bool,
}

impl State {
    fn busy(&self, key: &StorageKey) -> bool {
        self.queue.contains_key(key) || self.writing.contains_key(key)
    }
}

struct Batch {
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

/// Live handles proving a leased record directory is still the one leased.
struct Owner {
    directory: PathBuf,
    handle: File,
    gate: File,
}

impl Owner {
    fn check(&self) -> Result<(), StoreError> {
        for (opened, path) in [
            (&self.handle, self.directory.clone()),
            (&self.gate, self.directory.join(CANONICAL_GATE)),
        ] {
            if !same_file(opened, &path) {
                return Err(StoreError::SourceChanged { path });
            }
        }
        Ok(())
    }
}

struct FsLease {
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
                if state
                    .owners
                    .get(&self.key)
                    .is_some_and(|live| Arc::ptr_eq(live, owner))
                {
                    state.owners.remove(&self.key);
                }
            }
            None => {
                state.view_leases.remove(&self.key);
            }
        }
    }
}

enum Target {
    Record(PathBuf),
    View(PathBuf),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            text.get(at..at.checked_add(2)?)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
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
            root,
            shared: Arc::new(Shared {
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

    fn target(&self, key: &StorageKey) -> Result<Target, SeamError> {
        match key {
            StorageKey::Record {
                package,
                workspace,
                record,
            } => Ok(Target::Record(DocStore::room_record_dir_at(
                &self.root, package, workspace, record,
            )?)),
            StorageKey::View {
                device,
                workspace,
                instance,
            } => {
                validate_absolute_directory(&self.root)?;
                for identity in [device, workspace] {
                    if identity.is_empty() {
                        return Err(SeamError::InvalidKey {
                            key: key.to_string(),
                            reason: "view identities must not be empty".into(),
                        });
                    }
                }
                Ok(Target::View(
                    self.root
                        .join(INSTANCE_VIEWS_DIR)
                        .join(hex(device.as_bytes()))
                        .join(hex(workspace.as_bytes()))
                        .join(format!("{instance}.postcard")),
                ))
            }
        }
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
        let (file, directory) = match self.target(key).map_err(seam_to_store)? {
            Target::Record(directory) => (directory.join(DOCUMENT_FILE), directory),
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
            options
                .open(&file)
                .and_then(|f| f.sync_all())
                .map_err(|e| io_err(format!("syncing {}", file.display()), e))?;
        }
        sync_ancestors(&self.shared, &directory)
    }

    #[cfg(test)]
    pub(crate) fn faults(&self) -> MutexGuard<'_, Faults> {
        lock(&self.shared.faults)
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
        if lock(&self.shared.state).busy(key) {
            // Read-your-writes: commit what was accepted before reading it back.
            let _ = self.flush(Some(key));
        }
        let (path, writer) = match target {
            Target::Record(directory) => (
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
            Target::Record(_) => {
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
        while state.queue.keys().chain(state.writing.keys()).any(&matches) {
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

    fn lease(&self, key: &StorageKey) -> Result<Lease, SeamError> {
        match self.target(key)? {
            Target::Record(directory) => {
                let owner = Arc::new(open_owner(&self.shared, &directory)?);
                let mut state = lock(&self.shared.state);
                if state.owners.contains_key(key) {
                    return Err(SeamError::Busy(key.to_string()));
                }
                state.owners.insert(key.clone(), Arc::clone(&owner));
                // A new owner starts from bytes it verifies itself, not from
                // an earlier owner's failures.
                let status = state.keys.entry(key.clone()).or_default();
                status.failed.clear();
                status.poisoned = false;
                status.synced = false;
                drop(state);
                Ok(Box::new(FsLease {
                    key: key.clone(),
                    owner: Some(owner),
                    shared: Arc::downgrade(&self.shared),
                }))
            }
            Target::View(_) => {
                if !lock(&self.shared.state).view_leases.insert(key.clone()) {
                    return Err(SeamError::Busy(key.to_string()));
                }
                Ok(Box::new(FsLease {
                    key: key.clone(),
                    owner: None,
                    shared: Arc::downgrade(&self.shared),
                }))
            }
        }
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

fn open_owner(shared: &Shared, directory: &Path) -> Result<Owner, StoreError> {
    fs::create_dir_all(directory).map_err(|e| io_err("creating canonical directory", e))?;
    let handle =
        open_directory(directory, false).map_err(|e| io_err("opening canonical directory", e))?;
    let gate = acquire_gate(&directory.join(CANONICAL_GATE), true)?;
    let owner = Owner {
        directory: directory.to_path_buf(),
        handle,
        gate,
    };
    owner.check()?;
    sync_ancestors(shared, directory)?;
    owner.check()?;
    Ok(owner)
}

fn sync_ancestors(shared: &Shared, directory: &Path) -> Result<(), StoreError> {
    #[cfg(not(test))]
    let _ = shared;
    sync_directory_ancestors(directory, |path| {
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
    while let Some(taken) = next_batch(shared) {
        guard.current = Some((taken.key.clone(), taken.batch.first, taken.batch.last));
        let started = Instant::now();
        let result = commit(shared, &taken);
        guard.current = None;
        // Release the owner's handles before waking flushers: a caller that
        // then drops its lease must find the gate free.
        let Taken {
            key, batch, owner, ..
        } = taken;
        drop(owner);
        finish(shared, &key, &batch, result, started);
    }
    guard.finished = true;
}

fn next_batch(shared: &Shared) -> Option<Taken> {
    let mut state = lock(&shared.state);
    loop {
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
            let owner = state.owners.get(&key).cloned();
            let poisoned = state.keys.get(&key).is_some_and(|status| status.poisoned);
            return Some(Taken {
                key,
                batch,
                owner,
                poisoned,
            });
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
            atomic_file::write(&sidecar, hex(writer).as_bytes())
                .map_err(|e| io_err(format!("writing {}", sidecar.display()), e))?;
        }
        let document = owner.directory.join(DOCUMENT_FILE);
        return match &batch.op {
            WriteOp::Replace(bytes) => {
                atomic_file::write(&document, bytes)
                    .map_err(|e| io_err(format!("writing {}", document.display()), e))?;
                sync_ancestors(shared, &owner.directory)?;
                owner.check()
            }
            WriteOp::Append(bytes) => append_synced(shared, &document, bytes, false),
        };
    };
    match &batch.op {
        WriteOp::Replace(bytes) => {
            if let Some(parent) = view.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| io_err(format!("creating {}", parent.display()), e))?;
            }
            atomic_file::write(view, bytes)
                .map_err(|e| io_err(format!("writing {}", view.display()), e))
        }
        WriteOp::Append(bytes) => {
            if let Some(parent) = view.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| io_err(format!("creating {}", parent.display()), e))?;
            }
            append_synced(shared, view, bytes, true)
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
    use tempfile::TempDir;

    fn record(name: &str) -> StorageKey {
        StorageKey::record("so.tilo.test", "room-a", name)
    }

    #[test]
    fn passes_the_shared_conformance_suite_and_survives_reopening() {
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

    #[test]
    fn keys_map_onto_the_existing_profile_layout() {
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

    #[test]
    fn record_writes_require_a_lease_and_invalid_keys_are_refused() {
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

    #[test]
    fn a_torn_append_requires_a_replacement_without_duplicating_bytes() {
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

    #[test]
    fn directory_sync_failure_leaves_the_replacement_unacknowledged() {
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

    #[test]
    fn a_replaced_record_directory_is_never_written_or_recreated() {
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

    #[test]
    fn writes_coalesce_until_their_debounce_or_a_flush() {
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

    #[test]
    fn dropping_the_store_commits_accepted_writes() {
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

    #[test]
    fn hex_round_trips_and_rejects_malformed_sidecars() {
        assert_eq!(unhex(&hex(&[0, 0xff, 0x10])), Some(vec![0, 0xff, 0x10]));
        for bad in ["", "a", "zz", "é0"] {
            assert_eq!(unhex(bad), None, "{bad}");
        }
    }
}
