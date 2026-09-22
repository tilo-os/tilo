//! Debounced background writing for a [`DocStore`].
//!
//! Typing a character must not cost an `fsync`. [`Autosave`] takes the document's delta on
//! the UI thread — cheap, no I/O — hands it to a worker, and the worker coalesces a burst of
//! edits into a single write once the user pauses.
//!
//! Growth is bounded without paying a full rewrite per edit: the handle appends
//! `save_after(persisted_heads)` until the appended volume exceeds the compacted base, then
//! sends one full `save()` and resets — total bytes written stay linear in document size.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash};

use crate::{DocStore, StoreError};

/// Debounce window apps get by default. Long enough to swallow a burst of keystrokes, short
/// enough that a `kill` a moment later still finds the edit on disk.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Below this, compaction isn't worth it — a small document rewrites in microseconds and the
/// append bookkeeping would dominate.
const MIN_COMPACT_BYTES: usize = 64 * 1024;

/// One coherent worker acknowledgement; reading this never commits document operations.
#[derive(Clone, Debug, Default)]
pub struct DurabilitySnapshot {
    pub durable_heads: Option<Vec<ChangeHash>>,
    pub error: Option<String>,
}

#[derive(Default)]
struct WriterStatus {
    snapshot: DurabilitySnapshot,
    unavailable: bool,
}

type Status = Arc<Mutex<WriterStatus>>;

fn status_lock(status: &Status) -> std::sync::MutexGuard<'_, WriterStatus> {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unavailable(status: &Status, reason: &str) -> String {
    let mut state = status_lock(status);
    state.unavailable = true;
    state
        .snapshot
        .error
        .get_or_insert_with(|| reason.to_owned());
    drop(state);
    reason.to_owned()
}

enum Msg {
    Full {
        bytes: Vec<u8>,
        heads: Vec<ChangeHash>,
        checkpoint: bool,
    },
    Append {
        bytes: Vec<u8>,
        heads: Vec<ChangeHash>,
    },
    Flush(Sender<Result<(), String>>),
    Stop,
    #[cfg(test)]
    Crash {
        reached: Sender<()>,
        resume: Receiver<()>,
        unwind: bool,
    },
}

/// A background writer handle. Canonical handles retain the gate and one worker lease even
/// after worker failure; recovery joins the previous worker before starting its successor.
pub struct Autosave {
    tx: Option<Sender<Msg>>,
    join: Option<thread::JoinHandle<()>>,
    document: PathBuf,
    package_id: String,
    sent: Vec<ChangeHash>,
    appended: usize,
    base: usize,
    full_writes: usize,
    appends: usize,
    status: Status,
    canonical: Option<Arc<DocStore>>,
    debounce: Duration,
}

impl Autosave {
    /// Legacy best-effort startup, retaining the disabled-handle fallback.
    pub fn spawn(store: DocStore, debounce: Duration) -> Self {
        let document = store.document_path().to_path_buf();
        let package_id = store.package_id().to_owned();
        let status = Arc::new(Mutex::new(WriterStatus::default()));
        let store = Arc::new(store);
        match spawn_worker(&store, debounce, &status) {
            Ok((tx, join)) => Self {
                tx: Some(tx),
                join: Some(join),
                document,
                package_id,
                sent: Vec::new(),
                appended: 0,
                base: 0,
                full_writes: 0,
                appends: 0,
                status,
                canonical: None,
                debounce,
            },
            Err(e) => Self::disabled(format!("could not spawn the app-state writer thread: {e}")),
        }
    }

    /// Claim the one canonical writer handle without consuming the caller's owner.
    ///
    /// # Errors
    /// Refuses legacy stores, duplicate handles, and worker startup failures.
    pub fn try_spawn_canonical(
        store: &Arc<DocStore>,
        debounce: Duration,
    ) -> Result<Self, StoreError> {
        let canonical = store.canonical_state()?;
        if store
            .writer_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(StoreError::WriterBusy {
                path: store.document.clone(),
            });
        }
        let status = Arc::new(Mutex::new(WriterStatus {
            snapshot: DurabilitySnapshot {
                durable_heads: canonical.accepted.then(|| store.persisted.clone()),
                error: None,
            },
            unavailable: false,
        }));
        match spawn_worker(store, debounce, &status) {
            Ok((tx, join)) => Ok(Self {
                tx: Some(tx),
                join: Some(join),
                document: store.document.clone(),
                package_id: store.package_id.clone(),
                sent: store.persisted.clone(),
                appended: 0,
                base: if canonical.accepted {
                    file_length(&store.document)
                } else {
                    0
                },
                full_writes: 0,
                appends: 0,
                status,
                canonical: Some(Arc::clone(store)),
                debounce,
            }),
            Err(e) => {
                store
                    .writer_claimed
                    .store(false, std::sync::atomic::Ordering::Release);
                Err(crate::io_err("starting canonical writer", e))
            }
        }
    }

    /// Restart an unavailable canonical worker, joining it before replacement.
    ///
    /// # Errors
    /// Returns a retryable error while old teardown is unfinished, or if startup fails.
    pub fn try_restart_canonical(&mut self) -> Result<bool, String> {
        let store = self.canonical.as_ref().ok_or("canonical writer required")?;
        if !status_lock(&self.status).unavailable
            && self.join.as_ref().is_some_and(|j| !j.is_finished())
        {
            return Ok(false);
        }
        unavailable(
            &self.status,
            "canonical writer unavailable; full checkpoint required",
        );
        if self.join.as_ref().is_some_and(|j| !j.is_finished()) {
            return Err("canonical writer is still stopping; retry after teardown".to_owned());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        self.tx = None;
        match spawn_worker(store, self.debounce, &self.status) {
            Ok((tx, join)) => {
                self.tx = Some(tx);
                self.join = Some(join);
                status_lock(&self.status).unavailable = false;
                Ok(true)
            }
            Err(e) => Err(unavailable(
                &self.status,
                &format!("restarting canonical writer: {e}"),
            )),
        }
    }

    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            tx: None,
            join: None,
            document: PathBuf::new(),
            package_id: String::new(),
            sent: Vec::new(),
            appended: 0,
            base: 0,
            full_writes: 0,
            appends: 0,
            status: Arc::new(Mutex::new(WriterStatus {
                snapshot: DurabilitySnapshot {
                    durable_heads: None,
                    error: Some(reason.into()),
                },
                unavailable: true,
            })),
            canonical: None,
            debounce: DEFAULT_DEBOUNCE,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }
    pub fn document_path(&self) -> &Path {
        &self.document
    }
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Legacy loaded-document baseline. Canonical baselines come only from accepted stores.
    pub fn seed(&mut self, doc: &mut AutoCommit) {
        if self.tx.is_none() || self.canonical.is_some() {
            return;
        }
        self.sent = doc.get_heads();
        self.base = file_length(&self.document);
        self.appended = 0;
        status_lock(&self.status).snapshot.durable_heads = Some(self.sent.clone());
    }

    /// Send healthy incremental changes. Failed canonical writers require an explicit checkpoint.
    pub fn request(&mut self, doc: &mut AutoCommit) {
        if self.tx.is_none() || (self.canonical.is_some() && self.last_error().is_some()) {
            return;
        }
        let heads = doc.get_heads();
        if heads == self.sent {
            return;
        }
        if self.base == 0 || self.appended >= self.base.max(MIN_COMPACT_BYTES) {
            self.send_full(doc, heads, false);
        } else {
            let bytes = doc.save_after(&self.sent);
            let len = bytes.len();
            if self.send(Msg::Append {
                bytes,
                heads: heads.clone(),
            }) {
                self.sent = heads;
                self.appended = self.appended.saturating_add(len);
                self.appends = self.appends.saturating_add(1);
            }
        }
    }

    /// Capture the full current document independent of previously enqueued heads.
    pub fn request_checkpoint(&mut self, doc: &mut AutoCommit) {
        let heads = doc.get_heads();
        self.send_full(doc, heads, true);
    }

    fn send_full(&mut self, doc: &mut AutoCommit, heads: Vec<ChangeHash>, checkpoint: bool) {
        let bytes = doc.save();
        let len = bytes.len();
        if self.send(Msg::Full {
            bytes,
            heads: heads.clone(),
            checkpoint,
        }) {
            self.sent = heads;
            self.base = len;
            self.appended = 0;
            self.full_writes = self.full_writes.saturating_add(1);
        }
    }

    fn send(&self, msg: Msg) -> bool {
        if self.tx.as_ref().is_some_and(|tx| tx.send(msg).is_ok()) {
            return true;
        }
        unavailable(&self.status, "writer channel closed");
        false
    }

    /// Wait for the actual writer result, including channel/worker failure.
    ///
    /// # Errors
    /// Returns the sticky write failure or unavailable-channel error.
    pub fn try_flush(&self) -> Result<(), String> {
        let (ack, done) = mpsc::channel();
        if !self.send(Msg::Flush(ack)) {
            return Err(self
                .last_error()
                .unwrap_or_else(|| "writer unavailable".to_owned()));
        }
        done.recv().unwrap_or_else(|_| {
            Err(unavailable(
                &self.status,
                "writer acknowledgement channel closed",
            ))
        })
    }

    /// Legacy unit-returning flush wrapper.
    pub fn flush(&self) {
        if self.tx.is_some() {
            let _ = self.try_flush();
        }
    }
    pub fn full_writes(&self) -> usize {
        self.full_writes
    }
    pub fn appends(&self) -> usize {
        self.appends
    }
    pub fn last_error(&self) -> Option<String> {
        self.durability_snapshot().error
    }
    pub fn durability_snapshot(&self) -> DurabilitySnapshot {
        status_lock(&self.status).snapshot.clone()
    }
}

impl Drop for Autosave {
    fn drop(&mut self) {
        self.flush();
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Stop);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if let Some(store) = self.canonical.take() {
            store
                .writer_claimed
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

fn file_length(path: &Path) -> usize {
    std::fs::metadata(path).map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX))
}

fn spawn_worker(
    store: &Arc<DocStore>,
    debounce: Duration,
    status: &Status,
) -> std::io::Result<(Sender<Msg>, thread::JoinHandle<()>)> {
    #[cfg(test)]
    if store
        .faults
        .spawn_failures
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |n| n.checked_sub(1),
        )
        .is_ok()
    {
        return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
    }
    let (tx, rx) = mpsc::channel();
    let store = Arc::clone(store);
    let status = Arc::clone(status);
    let join = thread::Builder::new()
        .name("app-state-writer".into())
        .spawn(move || writer_loop(&rx, &store, debounce, &status))?;
    Ok((tx, join))
}

#[cfg(test)]
struct ActiveWorker<'a>(&'a crate::TestFaults);
#[cfg(test)]
impl<'a> ActiveWorker<'a> {
    fn new(faults: &'a crate::TestFaults) -> Self {
        let n = faults
            .active_workers
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .saturating_add(1);
        faults
            .max_workers
            .fetch_max(n, std::sync::atomic::Ordering::AcqRel);
        Self(faults)
    }
}
#[cfg(test)]
impl Drop for ActiveWorker<'_> {
    fn drop(&mut self) {
        self.0
            .active_workers
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct ExitGuard<'a> {
    status: &'a Status,
    expected: bool,
}
impl Drop for ExitGuard<'_> {
    fn drop(&mut self) {
        if !self.expected {
            unavailable(
                self.status,
                "writer exited unexpectedly; full checkpoint required",
            );
        }
    }
}

struct Pending {
    bytes: Vec<u8>,
    heads: Vec<ChangeHash>,
    full: bool,
    checkpoint: bool,
}

fn commit(pending: &mut Option<Pending>, store: &DocStore, status: &Status) -> Result<(), String> {
    if let Some(pending) = pending.take() {
        if store.canonical.is_some()
            && !pending.checkpoint
            && let Some(error) = status_lock(status).snapshot.error.clone()
        {
            return Err(error);
        }
        let started = Instant::now();
        let result = if pending.full {
            store.write_full(&pending.bytes)
        } else {
            store.append_bytes(&pending.bytes)
        };
        let mut state = status_lock(status);
        match result {
            Ok(()) => {
                tracing::debug!(
                    target: "tilo.state",
                    event = "document_persist",
                    kind = if pending.full { "full" } else { "append" },
                    bytes = pending.bytes.len(),
                    elapsed_us = started.elapsed().as_micros(),
                    "persisted application document"
                );
                state.snapshot.durable_heads = Some(pending.heads);
                if pending.checkpoint {
                    state.snapshot.error = None;
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "app state write failed");
                state.snapshot.error = Some(e.to_string());
            }
        }
    }
    let error = status_lock(status).snapshot.error.clone();
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn writer_loop(rx: &Receiver<Msg>, store: &DocStore, debounce: Duration, status: &Status) {
    #[cfg(test)]
    let _active = ActiveWorker::new(&store.faults);
    let mut exit = ExitGuard {
        status,
        expected: false,
    };
    let mut pending: Option<Pending> = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let timeout = deadline.map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match rx.recv_timeout(timeout) {
            Ok(Msg::Full {
                bytes,
                heads,
                checkpoint,
            }) => {
                if store.canonical.is_some()
                    && !checkpoint
                    && status_lock(status).snapshot.error.is_some()
                {
                    continue;
                }
                pending = Some(Pending {
                    bytes,
                    heads,
                    full: true,
                    checkpoint,
                });
                deadline = Instant::now().checked_add(debounce);
            }
            Ok(Msg::Append { bytes, heads }) => {
                if store.canonical.is_some() && status_lock(status).snapshot.error.is_some() {
                    continue;
                }
                if let Some(p) = pending.as_mut() {
                    p.bytes.extend(bytes);
                    p.heads = heads;
                } else {
                    pending = Some(Pending {
                        bytes,
                        heads,
                        full: false,
                        checkpoint: false,
                    });
                }
                deadline = Instant::now().checked_add(debounce);
            }
            Ok(Msg::Flush(ack)) => {
                let result = commit(&mut pending, store, status);
                deadline = None;
                let _ = ack.send(result);
            }
            #[cfg(test)]
            Ok(Msg::Crash {
                reached,
                resume,
                unwind,
            }) => {
                unavailable(status, "injected unexpected worker exit");
                reached.send(()).unwrap();
                resume.recv().unwrap();
                assert!(!unwind, "injected writer unwind");
                return;
            }
            Ok(Msg::Stop) => {
                let _ = commit(&mut pending, store, status);
                exit.expected = true;
                return;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = commit(&mut pending, store, status);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = commit(&mut pending, store, status);
                deadline = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};
    use tempfile::TempDir;

    #[test]
    fn canonical_initial_spawn_failure_and_duplicate_handle_keep_gate() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap());
        store
            .faults
            .spawn_failures
            .store(1, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE),
            Err(StoreError::Io { .. })
        ));
        assert_eq!(Arc::strong_count(&store), 1);
        assert!(!store.document.exists());
        assert!(DocStore::open_canonical_at("so.tilo.test", tmp.path()).is_err());
        assert!(DocStore::open_in("so.tilo.test", tmp.path().to_path_buf()).is_err());
        let saver = Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE).unwrap();
        assert!(matches!(
            Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE),
            Err(StoreError::WriterBusy { .. })
        ));
        drop(saver);
        assert!(Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE).is_ok());
    }

    #[test]
    fn canonical_worker_death_joins_before_restart_and_retains_latest_edits() {
        for unwind in [false, true] {
            let tmp = TempDir::new().unwrap();
            let store = Arc::new(DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap());
            let mut doc = store.new_canonical_document().unwrap();
            doc.put(ROOT, "body", "original").unwrap();
            let mut saver =
                Autosave::try_spawn_canonical(&store, Duration::from_secs(600)).unwrap();
            saver.request(&mut doc);
            saver.try_flush().unwrap();
            let durable = saver.durability_snapshot().durable_heads;
            doc.put(ROOT, "body", "latest").unwrap();
            saver.request(&mut doc);
            let (reached, ready) = mpsc::channel();
            let (release, resume) = mpsc::channel();
            saver
                .tx
                .as_ref()
                .unwrap()
                .send(Msg::Crash {
                    reached,
                    resume,
                    unwind,
                })
                .unwrap();
            ready.recv().unwrap();
            assert!(
                saver.try_restart_canonical().is_err(),
                "do not overlap unfinished teardown"
            );
            assert_eq!(saver.durability_snapshot().durable_heads, durable);
            assert!(saver.last_error().is_some());
            assert!(Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE).is_err());
            assert!(DocStore::open_canonical_at("so.tilo.test", tmp.path()).is_err());
            release.send(()).unwrap();
            while !saver.join.as_ref().unwrap().is_finished() {
                thread::yield_now();
            }
            assert!(saver.try_flush().is_err());
            store
                .faults
                .spawn_failures
                .store(1, std::sync::atomic::Ordering::Release);
            assert!(saver.try_restart_canonical().is_err());
            assert!(saver.join.is_none());
            assert!(saver.last_error().is_some());
            assert!(Autosave::try_spawn_canonical(&store, DEFAULT_DEBOUNCE).is_err());
            assert!(saver.try_restart_canonical().unwrap());
            assert!(
                saver.try_flush().is_err(),
                "an empty restarted queue must not clear failure"
            );
            saver.request_checkpoint(&mut doc);
            saver.try_flush().unwrap();
            let heads = doc.get_heads();
            assert_eq!(
                saver.durability_snapshot().durable_heads,
                Some(heads.clone())
            );
            assert!(saver.last_error().is_none());
            assert_eq!(
                store
                    .faults
                    .max_workers
                    .load(std::sync::atomic::Ordering::Acquire),
                1
            );
            drop(saver);
            drop(store);
            let mut store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
            let loaded = store.load_canonical().unwrap().unwrap();
            let mut restored = store.accept_canonical(loaded).unwrap();
            assert_eq!(restored.get_heads(), heads);
            assert_eq!(
                restored.get(ROOT, "body").unwrap().unwrap().0.to_str(),
                Some("latest")
            );
        }
    }

    #[test]
    fn canonical_partial_append_failure_requires_full_checkpoint_without_duplicates() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap());
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "original").unwrap();
        let mut saver = Autosave::try_spawn_canonical(&store, Duration::from_secs(600)).unwrap();
        saver.request(&mut doc);
        saver.try_flush().unwrap();
        let acknowledged = saver.durability_snapshot().durable_heads;
        *store.faults.append_prefix.lock().unwrap() = Some(8);
        doc.put(ROOT, "body", "partial").unwrap();
        saver.request(&mut doc);
        assert!(saver.try_flush().is_err());
        assert_eq!(saver.durability_snapshot().durable_heads, acknowledged);
        let broken = std::fs::read(store.document_path()).unwrap();
        doc.put(ROOT, "body", "latest").unwrap();
        saver.request(&mut doc);
        assert!(saver.try_flush().is_err());
        assert_eq!(std::fs::read(store.document_path()).unwrap(), broken);
        saver.request_checkpoint(&mut doc);
        saver.try_flush().unwrap();
        let mut restored =
            AutoCommit::load(&std::fs::read(store.document_path()).unwrap()).unwrap();
        assert_eq!(restored.get_heads(), doc.get_heads());
        assert_eq!(restored.get_changes(&[]).len(), doc.get_changes(&[]).len());
    }

    #[test]
    fn canonical_background_ack_and_older_checkpoint_do_not_save_newer_edits() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap());
        let mut doc = store.new_canonical_document().unwrap();
        let mut saver = Autosave::try_spawn_canonical(&store, Duration::ZERO).unwrap();
        let (reached, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        *store.faults.completion.lock().unwrap() = Some((reached, resume));
        saver.request(&mut doc);
        ready.recv().unwrap();
        assert!(saver.durability_snapshot().durable_heads.is_none());
        release.send(()).unwrap();
        while saver.durability_snapshot().durable_heads.is_none() {
            thread::yield_now();
        }
        assert_eq!(
            saver.durability_snapshot().durable_heads,
            Some(doc.get_heads())
        );
        // Fail a real publication barrier, retaining old acknowledged heads.
        *store.faults.sync_failure.lock().unwrap() =
            Some(std::fs::canonicalize(tmp.path()).unwrap());
        doc.put(ROOT, "body", "checkpoint").unwrap();
        saver.request_checkpoint(&mut doc);
        assert!(saver.try_flush().is_err());
        *store.faults.sync_failure.lock().unwrap() = None;
        let (reached, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        *store.faults.completion.lock().unwrap() = Some((reached, resume));
        let older = doc.get_heads();
        saver.request_checkpoint(&mut doc);
        ready.recv().unwrap();
        doc.put(ROOT, "body", "after-checkpoint").unwrap();
        let pending = doc.pending_ops();
        saver.request(&mut doc);
        assert_eq!(
            doc.pending_ops(),
            pending,
            "failed-state request must not consume typing or advance sent"
        );
        release.send(()).unwrap();
        saver.try_flush().unwrap();
        assert_eq!(saver.durability_snapshot().durable_heads, Some(older));
        assert!(doc.pending_ops() > 0);
        saver.request(&mut doc);
        saver.try_flush().unwrap();
        assert_eq!(
            saver.durability_snapshot().durable_heads,
            Some(doc.get_heads())
        );
        let restored = AutoCommit::load(&std::fs::read(store.document_path()).unwrap()).unwrap();
        assert_eq!(
            restored.get(ROOT, "body").unwrap().unwrap().0.to_str(),
            Some("after-checkpoint")
        );
    }

    #[test]
    fn canonical_failed_flush_retains_error_and_latest_snapshot() {
        let tmp = TempDir::new().unwrap();
        let store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "first").unwrap();
        let path = store.document_path().to_path_buf();
        let store = Arc::new(store);
        let mut saver = Autosave::try_spawn_canonical(&store, Duration::from_secs(600)).unwrap();
        saver.request(&mut doc);
        saver.try_flush().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        doc.put(ROOT, "body", "latest").unwrap();
        saver.request(&mut doc);
        saver.flush();
        assert!(saver.last_error().is_some());
        std::fs::remove_dir(&path).unwrap();
        // An explicit retry, even without a new edit, must retain the latest snapshot.
        assert!(!saver.try_restart_canonical().unwrap());
        saver.request_checkpoint(&mut doc);
        saver.flush();
        assert!(
            path.is_file(),
            "failed bytes must not be forgotten at enqueue time"
        );
        let loaded = AutoCommit::load(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            loaded.get(ROOT, "body").unwrap().unwrap().0.to_str(),
            Some("latest")
        );
    }

    fn scratch_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }
        tmp
    }

    #[test]
    #[serial_test::serial]
    fn a_burst_of_edits_lands_on_disk_after_a_flush() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("so.tilo.autosave").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        let mut saver = Autosave::spawn(store, Duration::from_millis(50));
        saver.seed(&mut doc);

        for (i, ch) in "persisted".chars().enumerate() {
            doc.splice_text(&body, i, 0, &ch.to_string()).unwrap();
            saver.request(&mut doc);
        }
        saver.flush();
        assert_eq!(saver.last_error(), None, "no write may have failed");

        let mut reopened = DocStore::open_for("so.tilo.autosave").unwrap();
        let loaded = reopened.load().unwrap().expect("document on disk");
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(loaded.text(&body).unwrap(), "persisted");
    }

    #[test]
    #[serial_test::serial]
    fn dropping_the_handle_flushes() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("so.tilo.autosave-drop").unwrap();
        let mut doc = store.new_document();
        {
            let mut saver = Autosave::spawn(store, Duration::from_secs(600));
            saver.seed(&mut doc);
            doc.put(&ROOT, "k", "v").unwrap();
            saver.request(&mut doc);
            // Debounce is 10 minutes: only Drop's flush can get this out.
        }
        drop(doc);
        let mut reopened = DocStore::open_for("so.tilo.autosave-drop").unwrap();
        let loaded = reopened.load().unwrap().expect("document on disk");
        assert!(matches!(loaded.get(&ROOT, "k"), Ok(Some(_))));
    }

    /// A steady stream of edits must not rewrite the whole document each time — the O(n²) shape
    /// this store exists to avoid. Asserted on the decision the writer actually makes, since a
    /// rewrite and an append produce almost the same file size for a small document.
    #[test]
    #[serial_test::serial]
    fn steady_edits_append_rather_than_rewrite() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("so.tilo.autosave-growth").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "seed").unwrap();
        store.save(&mut doc).unwrap();

        let path = store.document_path().to_path_buf();
        let base = std::fs::metadata(&path).unwrap().len();
        let mut saver = Autosave::spawn(store, Duration::from_millis(10));
        saver.seed(&mut doc);

        for i in 0..50 {
            doc.splice_text(&body, 4 + i, 0, "y").unwrap();
            saver.request(&mut doc);
        }
        saver.flush();

        assert_eq!(
            saver.full_writes(),
            0,
            "50 small edits on an already-saved document must not rewrite it"
        );
        assert_eq!(saver.appends(), 50, "each edit must ship exactly one delta");
        assert_eq!(saver.last_error(), None);

        let grown = std::fs::metadata(&path).unwrap().len();
        assert!(grown > base, "the edits must have reached the file");

        drop((saver, doc));
        let mut reopened = DocStore::open_for("so.tilo.autosave-growth").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(
            loaded.text(&body).unwrap(),
            format!("seed{}", "y".repeat(50))
        );
    }

    /// Once the appended volume outgrows the compacted base, the writer must fold everything
    /// back into one document chunk instead of growing the append log forever.
    #[test]
    #[serial_test::serial]
    fn the_append_log_is_eventually_compacted() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("so.tilo.autosave-compact").unwrap();
        let mut doc = store.new_document();
        let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&body, 0, 0, "seed").unwrap();
        store.save(&mut doc).unwrap();

        let mut saver = Autosave::spawn(store, Duration::from_millis(5));
        saver.seed(&mut doc);
        // MIN_COMPACT_BYTES is 64 KiB and each single-character change chunk is about a hundred
        // bytes, comfortably past the threshold.
        for i in 0..1200 {
            doc.splice_text(&body, 4 + i, 0, "z").unwrap();
            saver.request(&mut doc);
        }
        saver.flush();
        assert!(
            saver.full_writes() >= 1,
            "the append log must be compacted once it outgrows the base \
             (appends: {}, appended bytes exceeded the {} byte floor)",
            saver.appends(),
            MIN_COMPACT_BYTES
        );
        assert_eq!(saver.last_error(), None);

        drop((saver, doc));
        let mut reopened = DocStore::open_for("so.tilo.autosave-compact").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(loaded.text(&body).unwrap().chars().count(), 4 + 1200);
    }
}
