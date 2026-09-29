//! Automerge documents persisted through a [`DurableStore`].
//!
//! [`DocOwner`] owns one key, exclusively or as one of its shared writers:
//! it leases the key, decodes and validates what is stored without
//! publishing anything, and accepts a candidate only once its exact source
//! bytes are still current. [`DocWriter`] then persists the live document:
//! `save_after(sent_heads)` appends until the appended volume outgrows the
//! compacted base, then one full save. It never blocks and owns no thread:
//! the store accepts each write at once, and the writer maps store revisions
//! back to document heads to report what is actually durable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT};
use durable_store::{
    Durability, DurableStore, Lease, Revision, StorageKey, StoreError as SeamError, WriteOp,
};

use crate::{
    SCHEMA_VERSION, SCHEMA_VERSION_KEY, StoreError, decode_canonical, validate_canonical_schema_of,
};

/// Debounce window [`FsStore`](crate::FsStore) applies by default. Long
/// enough to swallow a burst of keystrokes, short enough that a `kill` a
/// moment later still finds the edit stored.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Below this, compaction isn't worth it — a small document rewrites in
/// microseconds and the append bookkeeping would dominate.
const MIN_COMPACT_BYTES: usize = 64 * 1024;

/// What a writer knows is durable, and any outstanding failure. Reading it
/// never commits document operations.
#[derive(Clone, Debug, Default)]
pub struct DurabilitySnapshot {
    pub durable_heads: Option<Vec<ChangeHash>>,
    pub error: Option<String>,
}

impl From<SeamError> for StoreError {
    fn from(error: SeamError) -> Self {
        match error {
            SeamError::Busy(path) => Self::WriterBusy { path: path.into() },
            SeamError::SourceChanged(path) => Self::SourceChanged { path: path.into() },
            other => Self::Io {
                context: "durable storage".to_owned(),
                source: io::Error::other(other),
            },
        }
    }
}

/// One accepted write the writer has not yet seen resolve.
#[derive(Clone)]
struct InFlight {
    revision: Revision,
    heads: Vec<ChangeHash>,
    replace: bool,
    /// Whether this write also stores the owner's actor id.
    writer: bool,
}

#[derive(Clone)]
struct Accepted {
    heads: Vec<ChangeHash>,
    base: usize,
    durable: Option<Vec<ChangeHash>>,
    pending: Option<InFlight>,
}

/// The owner of one document key: its exclusive owner, or one of its
/// shared writers (see [`DocOwner::acquire_shared`]).
pub struct DocOwner {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    label: PathBuf,
    actor: ActorId,
    actor_stored: Cell<bool>,
    accepted: RefCell<Option<Accepted>>,
    identity: Rc<()>,
    writer_claimed: Cell<bool>,
    primary: bool,
    _lease: Lease,
}

/// A validated decoding candidate, bound to the owner that produced it. No
/// recovered bytes are published until [`DocOwner::accept`].
pub struct StoredLoad {
    doc: AutoCommit,
    source: Vec<u8>,
    repair: Option<Vec<u8>>,
    identity: Rc<()>,
}

impl StoredLoad {
    pub fn document(&self) -> &AutoCommit {
        &self.doc
    }
}

impl DocOwner {
    /// Lease `key` and continue its stored writer identity, minting one when
    /// none is stored yet. The identity is persisted with the first write.
    ///
    /// # Errors
    /// [`StoreError::WriterBusy`] when another owner holds the key; any
    /// failure reading the stored identity.
    pub fn acquire(store: Rc<dyn DurableStore>, key: StorageKey) -> Result<Self, StoreError> {
        let lease = store.lease(&key)?;
        Self::with_lease(store, key, lease, true)
    }

    /// Join the key's shared writers, for documents several processes may
    /// have open at once: their appends interleave and every load merges
    /// them. Only the primary writer continues the stored actor; the others
    /// write under a fresh actor that is never stored.
    ///
    /// # Errors
    /// [`StoreError::WriterBusy`] while an exclusive owner holds the key; any
    /// failure reading the stored identity.
    pub fn acquire_shared(
        store: Rc<dyn DurableStore>,
        key: StorageKey,
    ) -> Result<Self, StoreError> {
        let shared = store.lease_shared(&key)?;
        Self::with_lease(store, key, shared.lease, shared.primary)
    }

    fn with_lease(
        store: Rc<dyn DurableStore>,
        key: StorageKey,
        lease: Lease,
        primary: bool,
    ) -> Result<Self, StoreError> {
        let stored = if primary {
            store
                .read(&key)?
                .and_then(|stored| stored.writer)
                .filter(|writer| !writer.is_empty())
        } else {
            None
        };
        // A secondary writer's actor is its own for this session only.
        let actor_stored = stored.is_some() || !primary;
        let actor = stored.map_or_else(ActorId::random, ActorId::from);
        let label = store
            .local_path(&key)
            .unwrap_or_else(|| PathBuf::from(format!("{} {key}", store.namespace())));
        Ok(Self {
            store,
            key,
            label,
            actor,
            actor_stored: Cell::new(actor_stored),
            accepted: RefCell::new(None),
            identity: Rc::new(()),
            writer_claimed: Cell::new(false),
            primary,
            _lease: lease,
        })
    }

    pub fn key(&self) -> &StorageKey {
        &self.key
    }

    pub fn store(&self) -> &Rc<dyn DurableStore> {
        &self.store
    }

    /// Names this document in errors and logs: its local file where the
    /// store keeps one, else the store namespace and key.
    pub fn label(&self) -> &Path {
        &self.label
    }

    /// Whether this owner continues the stored writer identity: always for
    /// an exclusive owner, for one shared writer at a time.
    pub fn is_primary(&self) -> bool {
        self.primary
    }

    /// The actor bytes to store with the next write, until one carrying them
    /// has committed.
    pub(crate) fn unstored_writer(&self) -> Option<Vec<u8>> {
        (!self.actor_stored.get()).then(|| self.actor.to_bytes().to_vec())
    }

    pub(crate) fn note_writer_stored(&self) {
        self.actor_stored.set(true);
    }

    /// The package owning this document, when its key names one.
    pub fn package_id(&self) -> &str {
        self.key.package_id().unwrap_or_default()
    }

    /// The key's stored value, including accepted writes not yet durable.
    ///
    /// # Errors
    /// A failure reading the store.
    pub fn stored(&self) -> Result<Option<durable_store::Stored>, StoreError> {
        Ok(self.store.read(&self.key)?)
    }

    /// Record that `heads` are exactly what the store durably holds, in
    /// `base` bytes, so a [`DocWriter`] started next continues from them
    /// rather than rewriting them.
    pub fn adopt(&self, heads: Vec<ChangeHash>, base: usize) {
        *self.accepted.borrow_mut() = Some(Accepted {
            heads: heads.clone(),
            base,
            durable: Some(heads),
            pending: None,
        });
    }

    /// The Automerge actor every write of this document is attributed to.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    /// A private current-schema candidate. Nothing is published or reset.
    ///
    /// # Errors
    /// Propagates a failure stamping the schema version.
    pub fn new_document(&self) -> Result<AutoCommit, StoreError> {
        let mut doc = AutoCommit::new().with_actor(self.actor.clone());
        doc.put(ROOT, SCHEMA_VERSION_KEY, u64::from(SCHEMA_VERSION))
            .map_err(|source| StoreError::Automerge {
                path: self.label.clone(),
                source: Box::new(source),
            })?;
        Ok(doc)
    }

    /// See [`validate_canonical_schema_of`].
    ///
    /// # Errors
    /// Rejects malformed, conflicting and newer schemas.
    pub fn validate_schema(&self, doc: &AutoCommit) -> Result<(), StoreError> {
        validate_canonical_schema_of(doc, &self.label)
    }

    /// Decode the stored document, dropping a torn tail into a private repair.
    ///
    /// # Errors
    /// Refuses empty, corrupt or unsupported documents without changing them.
    pub fn load(&self) -> Result<Option<StoredLoad>, StoreError> {
        self.load_with(true)
    }

    /// Decode the stored document only when its change graph is complete.
    ///
    /// # Errors
    /// Refuses empty, incomplete, corrupt or unsupported documents.
    pub fn load_complete(&self) -> Result<Option<StoredLoad>, StoreError> {
        self.load_with(false)
    }

    fn load_with(&self, recover_partial: bool) -> Result<Option<StoredLoad>, StoreError> {
        let Some(stored) = self.store.read(&self.key)? else {
            return Ok(None);
        };
        let (doc, repair) = decode_canonical(&stored.bytes, recover_partial, &self.label)?;
        Ok(Some(StoredLoad {
            doc,
            source: stored.bytes,
            repair,
            identity: Rc::clone(&self.identity),
        }))
    }

    /// Accept this owner's exact candidate: publish its repair, if any, and
    /// make the accepted bytes durable before a [`DocWriter`] continues from
    /// them. A store waiting on its host leaves the repair pending instead.
    ///
    /// # Errors
    /// Refuses a candidate from another owner or whose source changed, and
    /// any failure publishing or synchronizing the accepted bytes.
    pub fn accept(&self, mut loaded: StoredLoad) -> Result<AutoCommit, StoreError> {
        if !Rc::ptr_eq(&loaded.identity, &self.identity)
            || self
                .store
                .read(&self.key)?
                .map(|stored| stored.bytes)
                .as_ref()
                != Some(&loaded.source)
        {
            return Err(StoreError::SourceChanged {
                path: self.label.clone(),
            });
        }
        self.validate_schema(&loaded.doc)?;
        loaded.doc.set_actor(self.actor.clone());
        let heads = loaded.doc.get_heads();
        let accepted = if let Some(bytes) = loaded.repair {
            let base = bytes.len();
            let writer = !self.actor_stored.get();
            let revision = self.store.write(
                &self.key,
                WriteOp::Replace(bytes),
                writer.then(|| self.actor.to_bytes()),
            )?;
            match self.store.flush(Some(&self.key)) {
                Ok(()) | Err(SeamError::Pending(_)) => {}
                Err(error) => return Err(error.into()),
            }
            match self.store.durability(&self.key, revision) {
                Durability::Durable => {
                    if writer {
                        self.actor_stored.set(true);
                    }
                    Accepted {
                        heads: heads.clone(),
                        base,
                        durable: Some(heads),
                        pending: None,
                    }
                }
                Durability::Pending => Accepted {
                    heads: heads.clone(),
                    base,
                    durable: None,
                    pending: Some(InFlight {
                        revision,
                        heads,
                        replace: true,
                        writer,
                    }),
                },
                Durability::Failed(message) => {
                    return Err(StoreError::Io {
                        context: format!("repairing {}", self.label.display()),
                        source: io::Error::other(message),
                    });
                }
            }
        } else {
            let durable = match self.store.flush(Some(&self.key)) {
                Ok(()) => Some(heads.clone()),
                // Read, but its host has not committed it yet (a peer's change
                // stored for this record a moment ago): it is the document,
                // durable once that commit lands.
                Err(SeamError::Pending(_)) => None,
                Err(error) => return Err(error.into()),
            };
            Accepted {
                heads,
                base: loaded.source.len(),
                durable,
                pending: None,
            }
        };
        *self.accepted.borrow_mut() = Some(accepted);
        Ok(loaded.doc)
    }
}

/// Persists one owner's live document; see the module docs. Failures are
/// sticky: after one, [`DocWriter::request`] sends nothing until a
/// [`DocWriter::request_checkpoint`] full save becomes durable.
///
/// A [disabled](Self::disabled) writer persists nothing and reports why, so
/// an app without usable storage runs on the same code path.
pub struct DocWriter {
    owner: Option<Rc<DocOwner>>,
    sent: Vec<ChangeHash>,
    appended: usize,
    base: usize,
    full_writes: usize,
    appends: usize,
    progress: RefCell<Progress>,
}

/// What the writer knows about submitted writes. Interior so status and
/// flush work through a shared reference.
#[derive(Default)]
struct Progress {
    inflight: VecDeque<InFlight>,
    durable: Option<Vec<ChangeHash>>,
    error: Option<String>,
}

impl DocWriter {
    /// Claim the owner's one writer, continuing from its accepted document,
    /// or from nothing when the owner holds a new one.
    ///
    /// # Errors
    /// [`StoreError::WriterBusy`] while another writer for this owner lives.
    pub fn try_start(owner: &Rc<DocOwner>) -> Result<Self, StoreError> {
        if owner.writer_claimed.replace(true) {
            return Err(StoreError::WriterBusy {
                path: owner.label.clone(),
            });
        }
        let accepted = owner.accepted.borrow().clone();
        let (sent, base, durable, inflight) = match accepted {
            Some(accepted) => (
                accepted.heads,
                accepted.base,
                accepted.durable,
                accepted.pending.into_iter().collect(),
            ),
            None => (Vec::new(), 0, None, VecDeque::new()),
        };
        Ok(Self {
            owner: Some(Rc::clone(owner)),
            sent,
            appended: 0,
            base,
            full_writes: 0,
            appends: 0,
            progress: RefCell::new(Progress {
                inflight,
                durable,
                error: None,
            }),
        })
    }

    /// Start the writer for a new document and submit its first full save.
    ///
    /// # Errors
    /// As [`Self::try_start`], or when the store refuses the first write; the
    /// claim is released either way.
    pub fn start_new(owner: &Rc<DocOwner>, doc: &mut AutoCommit) -> Result<Self, StoreError> {
        let mut writer = Self::try_start(owner)?;
        let heads = doc.get_heads();
        writer.send_full(doc, heads)?;
        Ok(writer)
    }

    /// A writer that persists nothing; [`Self::last_error`] carries `reason`.
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            owner: None,
            sent: Vec::new(),
            appended: 0,
            base: 0,
            full_writes: 0,
            appends: 0,
            progress: RefCell::new(Progress {
                error: Some(reason.into()),
                ..Progress::default()
            }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.owner.is_some()
    }

    /// The owner this writer persists through; `None` when disabled.
    pub fn owner(&self) -> Option<&DocOwner> {
        self.owner.as_deref()
    }

    /// The package whose document this is; empty when disabled.
    pub fn package_id(&self) -> &str {
        self.owner.as_deref().map_or("", DocOwner::package_id)
    }

    /// See [`DocOwner::label`]; empty when disabled.
    pub fn document_path(&self) -> &Path {
        self.owner
            .as_deref()
            .map_or_else(|| Path::new(""), DocOwner::label)
    }

    /// Treat `doc` as exactly what is already stored: its heads are durable
    /// and later requests append only what follows them.
    pub fn seed(&mut self, doc: &mut AutoCommit) {
        let Some(owner) = &self.owner else {
            return;
        };
        self.sent = doc.get_heads();
        self.base = owner
            .stored()
            .ok()
            .flatten()
            .map_or(0, |stored| stored.bytes.len());
        self.appended = 0;
        self.progress.get_mut().durable = Some(self.sent.clone());
    }

    /// Submit the changes `doc` has beyond what was last submitted. Cheap when
    /// nothing changed. Does nothing while a failure is outstanding, so a
    /// failed writer never closes the caller's typing transaction.
    pub fn request(&mut self, doc: &mut AutoCommit) {
        self.settle();
        if self.owner.is_none() || self.progress.get_mut().error.is_some() {
            return;
        }
        let heads = doc.get_heads();
        if heads == self.sent {
            return;
        }
        let result = if self.base == 0 || self.appended >= self.base.max(MIN_COMPACT_BYTES) {
            self.send_full(doc, heads)
        } else {
            let bytes = doc.save_after(&self.sent);
            let len = bytes.len();
            self.submit(WriteOp::Append(bytes), heads).map(|()| {
                self.appended = self.appended.saturating_add(len);
                self.appends = self.appends.saturating_add(1);
            })
        };
        if let Err(error) = result {
            self.refuse(&error);
        }
    }

    /// Submit the full current document regardless of what was submitted
    /// before — the one write that clears an outstanding failure.
    pub fn request_checkpoint(&mut self, doc: &mut AutoCommit) {
        self.settle();
        if self.owner.is_none() {
            return;
        }
        let heads = doc.get_heads();
        if let Err(error) = self.send_full(doc, heads) {
            self.refuse(&error);
        }
    }

    /// [`Self::request`] normally, [`Self::request_checkpoint`] after a failure.
    pub fn save(&mut self, doc: &mut AutoCommit) {
        if self.last_error().is_some() {
            self.request_checkpoint(doc);
        } else {
            self.request(doc);
        }
    }

    fn refuse(&mut self, error: &StoreError) {
        tracing::error!(document = %self.document_path().display(), %error, "app state write refused");
        self.progress.get_mut().error = Some(error.to_string());
    }

    fn send_full(
        &mut self,
        doc: &mut AutoCommit,
        heads: Vec<ChangeHash>,
    ) -> Result<(), StoreError> {
        let bytes = doc.save();
        let len = bytes.len();
        self.submit(WriteOp::Replace(bytes), heads)?;
        self.base = len;
        self.appended = 0;
        self.full_writes = self.full_writes.saturating_add(1);
        Ok(())
    }

    fn submit(&mut self, op: WriteOp, heads: Vec<ChangeHash>) -> Result<(), StoreError> {
        let Some(owner) = &self.owner else {
            return Ok(());
        };
        let writer = !owner.actor_stored.get();
        let replace = op.is_replace();
        let revision = owner
            .store
            .write(&owner.key, op, writer.then(|| owner.actor.to_bytes()))?;
        self.progress.get_mut().inflight.push_back(InFlight {
            revision,
            heads: heads.clone(),
            replace,
            writer,
        });
        self.sent = heads;
        Ok(())
    }

    /// Fold resolved writes into the durable heads and sticky error.
    fn settle(&self) {
        let Some(owner) = &self.owner else {
            return;
        };
        let mut progress = self.progress.borrow_mut();
        while let Some(front) = progress.inflight.front() {
            match owner.store.durability(&owner.key, front.revision) {
                Durability::Pending => break,
                Durability::Durable => {
                    let Some(done) = progress.inflight.pop_front() else {
                        break;
                    };
                    if done.writer {
                        owner.actor_stored.set(true);
                    }
                    if done.replace {
                        progress.error = None;
                    }
                    progress.durable = Some(done.heads);
                }
                Durability::Failed(message) => {
                    progress.inflight.pop_front();
                    progress.error = Some(message);
                }
            }
        }
    }

    /// What is durable and whether a failure is outstanding, without
    /// submitting or closing anything.
    pub fn durability_snapshot(&self) -> DurabilitySnapshot {
        let progress = self.progress.borrow();
        let mut snapshot = DurabilitySnapshot {
            durable_heads: progress.durable.clone(),
            error: progress.error.clone(),
        };
        let Some(owner) = &self.owner else {
            return snapshot;
        };
        for write in &progress.inflight {
            match owner.store.durability(&owner.key, write.revision) {
                Durability::Pending => break,
                Durability::Durable => {
                    snapshot.durable_heads = Some(write.heads.clone());
                    if write.replace {
                        snapshot.error = None;
                    }
                }
                Durability::Failed(message) => snapshot.error = Some(message),
            }
        }
        snapshot
    }

    pub fn last_error(&self) -> Option<String> {
        self.durability_snapshot().error
    }

    /// Ask the store to commit everything submitted and report the outcome.
    ///
    /// # Errors
    /// The outstanding failure, or writes the store could not yet commit.
    pub fn try_flush(&self) -> Result<(), String> {
        let Some(owner) = &self.owner else {
            return Err(self
                .progress
                .borrow()
                .error
                .clone()
                .unwrap_or_else(|| "writer disabled".to_owned()));
        };
        let flushed = owner.store.flush(Some(&owner.key));
        self.settle();
        let progress = self.progress.borrow();
        if let Some(error) = &progress.error {
            return Err(error.clone());
        }
        flushed.map_err(|error| error.to_string())?;
        if progress.inflight.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} still has uncommitted writes",
                owner.label.display()
            ))
        }
    }

    /// [`Self::try_flush`], logging a failure instead of returning it. Does
    /// nothing for a disabled writer.
    pub fn flush(&self) {
        if self.owner.is_none() {
            return;
        }
        if let Err(error) = self.try_flush() {
            tracing::error!(document = %self.document_path().display(), %error, "app state write failed");
        }
    }

    pub fn full_writes(&self) -> usize {
        self.full_writes
    }

    pub fn appends(&self) -> usize {
        self.appends
    }
}

impl Drop for DocWriter {
    fn drop(&mut self) {
        let Some(owner) = &self.owner else {
            return;
        };
        if let Err(error) = owner.store.flush(Some(&owner.key)) {
            tracing::debug!(document = %owner.label.display(), %error, "writes outstanding at close");
        }
        owner.writer_claimed.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsStore;
    use automerge::{ObjType, ReadDoc};
    use durable_store::MemoryStore;
    use std::time::Duration;

    /// One store under test and the host loop that commits its writes.
    struct Subject {
        name: &'static str,
        store: Rc<dyn DurableStore>,
        queued: Option<Rc<MemoryStore>>,
        faults: Option<Rc<FsStore>>,
        _dir: Option<tempfile::TempDir>,
    }

    impl Subject {
        fn all() -> Vec<Self> {
            let queued = Rc::new(MemoryStore::queued());
            let tmp = tempfile::tempdir().unwrap();
            let fs = Rc::new(FsStore::with_debounce(tmp.path(), Duration::from_secs(600)));
            vec![
                Self {
                    name: "memory",
                    store: Rc::new(MemoryStore::new()),
                    queued: None,
                    faults: None,
                    _dir: None,
                },
                Self {
                    name: "queued",
                    store: queued.clone(),
                    queued: Some(queued),
                    faults: None,
                    _dir: None,
                },
                Self {
                    name: "fs",
                    store: fs.clone(),
                    queued: None,
                    faults: Some(fs),
                    _dir: Some(tmp),
                },
            ]
        }

        /// The host commits every drained write, reporting `result`.
        fn pump_with(&self, result: &Result<(), String>) {
            if let Some(queued) = &self.queued {
                for write in queued.drain() {
                    queued.acknowledge(write.revision, result.clone()).unwrap();
                }
            }
        }

        fn pump(&self) {
            self.pump_with(&Ok(()));
        }

        fn flush(&self, writer: &DocWriter) -> Result<(), String> {
            self.pump();
            writer.try_flush()
        }

        fn owner(&self, record: &str) -> Rc<DocOwner> {
            Rc::new(
                DocOwner::acquire(
                    Rc::clone(&self.store),
                    StorageKey::record("so.tilo.test", "room", record),
                )
                .unwrap(),
            )
        }

        fn reopen(&self, record: &str) -> AutoCommit {
            let owner = self.owner(record);
            let loaded = owner.load().unwrap().expect("stored document");
            owner.accept(loaded).unwrap()
        }
    }

    fn text(doc: &AutoCommit, key: &str) -> String {
        let (_, id) = doc.get(ROOT, key).unwrap().unwrap();
        doc.text(&id).unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn writer_edits_become_durable_through_appends_checkpoints_and_acknowledgements() {
        // Phase `a_burst_of_edits_is_durable_after_a_flush_and_reopens_as_the_same_writer`.
        {
            for subject in Subject::all() {
                let owner = subject.owner("burst");
                let mut doc = owner.new_document().unwrap();
                let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
                let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
                for (i, ch) in "persisted".chars().enumerate() {
                    doc.splice_text(&body, i, 0, &ch.to_string()).unwrap();
                    writer.request(&mut doc);
                }
                subject.flush(&writer).unwrap();
                assert_eq!(
                    writer.durability_snapshot().durable_heads,
                    Some(doc.get_heads()),
                    "{}",
                    subject.name
                );
                let actor = owner.actor().clone();
                drop((writer, owner));
                let reopened = subject.reopen("burst");
                assert_eq!(text(&reopened, "body"), "persisted", "{}", subject.name);
                assert_eq!(reopened.get_actor(), &actor, "{}", subject.name);
            }
        }
        // Phase `steady_edits_append_and_the_log_is_eventually_compacted`.
        {
            for subject in Subject::all() {
                let owner = subject.owner("growth");
                let mut doc = owner.new_document().unwrap();
                let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
                doc.splice_text(&body, 0, 0, "seed").unwrap();
                let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
                subject.flush(&writer).unwrap();
                for i in 0..50 {
                    doc.splice_text(&body, 4 + i, 0, "y").unwrap();
                    writer.request(&mut doc);
                }
                subject.flush(&writer).unwrap();
                assert_eq!(writer.full_writes(), 1, "{}", subject.name);
                assert_eq!(writer.appends(), 50, "{}", subject.name);
                // Each single-character change is about a hundred bytes, so this
                // passes the compaction floor.
                for i in 0..1200 {
                    doc.splice_text(&body, 54 + i, 0, "z").unwrap();
                    writer.request(&mut doc);
                }
                subject.flush(&writer).unwrap();
                assert!(writer.full_writes() >= 2, "{}", subject.name);
                drop((writer, owner));
                assert_eq!(
                    text(&subject.reopen("growth"), "body").chars().count(),
                    4 + 50 + 1200,
                    "{}",
                    subject.name
                );
            }
        }
        // Phase `an_older_acknowledgement_never_marks_newer_edits_saved`.
        {
            for subject in Subject::all() {
                let owner = subject.owner("status");
                let mut doc = owner.new_document().unwrap();
                let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
                subject.flush(&writer).unwrap();
                doc.put(ROOT, "value", 1_u64).unwrap();
                writer.request(&mut doc);
                let older = doc.get_heads();
                subject.flush(&writer).unwrap();
                doc.put(ROOT, "value", 2_u64).unwrap();
                writer.request(&mut doc);
                if subject.name != "memory" {
                    assert_eq!(
                        writer.durability_snapshot().durable_heads,
                        Some(older),
                        "{}",
                        subject.name
                    );
                }
                subject.flush(&writer).unwrap();
                assert_eq!(
                    writer.durability_snapshot().durable_heads,
                    Some(doc.get_heads()),
                    "{}",
                    subject.name
                );
            }
        }
        // Phase `a_failed_write_requires_a_full_checkpoint_without_consuming_typing`.
        {
            for subject in Subject::all() {
                if subject.queued.is_none() && subject.faults.is_none() {
                    continue; // an immediately durable store cannot fail
                }
                let owner = subject.owner("failure");
                let mut doc = owner.new_document().unwrap();
                doc.put(ROOT, "body", "original").unwrap();
                let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
                subject.flush(&writer).unwrap();
                let acknowledged = writer.durability_snapshot().durable_heads;
                if let Some(fs) = &subject.faults {
                    fs.faults().append_prefix = Some(8);
                }
                doc.put(ROOT, "body", "partial").unwrap();
                writer.request(&mut doc);
                subject.pump_with(&Err("quota".into()));
                assert!(writer.try_flush().is_err(), "{}", subject.name);
                assert_eq!(writer.durability_snapshot().durable_heads, acknowledged);
                doc.put(ROOT, "body", "latest").unwrap();
                let pending = doc.pending_ops();
                writer.request(&mut doc);
                assert_eq!(doc.pending_ops(), pending, "{}", subject.name);
                assert!(writer.try_flush().is_err(), "{}", subject.name);
                writer.save(&mut doc);
                subject.flush(&writer).unwrap();
                assert_eq!(writer.last_error(), None, "{}", subject.name);
                assert_eq!(
                    writer.durability_snapshot().durable_heads,
                    Some(doc.get_heads())
                );
                drop((writer, owner));
                let mut restored = subject.reopen("failure");
                assert_eq!(restored.get_heads(), doc.get_heads(), "{}", subject.name);
                assert_eq!(
                    restored.get_changes(&[]).len(),
                    doc.get_changes(&[]).len(),
                    "{}",
                    subject.name
                );
            }
        }
        // Phase `a_seeded_writer_appends_only_what_follows_the_stored_document`.
        {
            for subject in Subject::all() {
                let owner = subject.owner("seeded");
                let mut doc = owner.new_document().unwrap();
                doc.put(ROOT, "body", "stored").unwrap();
                store_raw_leased(&subject, &owner, doc.save());
                let mut writer = DocWriter::try_start(&owner).unwrap();
                writer.seed(&mut doc);
                doc.put(ROOT, "body", "edited").unwrap();
                writer.request(&mut doc);
                subject.flush(&writer).unwrap();
                assert_eq!(
                    (writer.full_writes(), writer.appends()),
                    (0, 1),
                    "{}",
                    subject.name
                );
            }
        }
    }

    fn with_torn_tail(doc: &mut AutoCommit) -> Vec<u8> {
        let heads = doc.get_heads();
        let mut bytes = doc.save();
        doc.put(ROOT, "body", "torn").unwrap();
        let tail = doc.save_after(&heads);
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        bytes
    }

    fn store_raw(subject: &Subject, record: &str, bytes: Vec<u8>) {
        let key = StorageKey::record("so.tilo.test", "room", record);
        let _lease = subject.store.lease(&key).unwrap();
        subject
            .store
            .write(&key, WriteOp::Replace(bytes), None)
            .unwrap();
        subject.pump();
        subject.store.flush(Some(&key)).unwrap();
    }

    fn store_raw_leased(subject: &Subject, owner: &DocOwner, bytes: Vec<u8>) {
        subject
            .store
            .write(owner.key(), WriteOp::Replace(bytes), None)
            .unwrap();
        subject.pump();
        subject.store.flush(Some(owner.key())).unwrap();
    }

    #[test]
    fn stored_bytes_are_accepted_only_when_decodable_unchanged_and_current() {
        // Phase `undecodable_bytes_are_refused_and_never_overwritten`.
        {
            for subject in Subject::all() {
                for (n, bytes) in [b"not automerge".to_vec(), Vec::new()]
                    .into_iter()
                    .enumerate()
                {
                    let record = format!("corrupt-{n}");
                    store_raw(&subject, &record, bytes.clone());
                    let owner = subject.owner(&record);
                    assert!(owner.load().is_err(), "{}", subject.name);
                    assert!(owner.load_complete().is_err(), "{}", subject.name);
                    assert_eq!(
                        subject.store.read(owner.key()).unwrap().unwrap().bytes,
                        bytes,
                        "{}",
                        subject.name
                    );
                }
            }
        }
        // Phase `a_torn_tail_is_repaired_only_on_acceptance_by_its_owner`.
        {
            for subject in Subject::all() {
                let mut doc = AutoCommit::new();
                doc.put(ROOT, "body", "prefix").unwrap();
                let torn = with_torn_tail(&mut doc);
                store_raw(&subject, "torn", torn.clone());
                let owner = subject.owner("torn");
                assert!(owner.load_complete().is_err(), "{}", subject.name);
                let loaded = owner.load().unwrap().unwrap();
                assert_eq!(
                    subject.store.read(owner.key()).unwrap().unwrap().bytes,
                    torn,
                    "decoding publishes nothing ({})",
                    subject.name
                );
                let other = subject.owner("elsewhere");
                assert!(matches!(
                    other.accept(loaded),
                    Err(StoreError::SourceChanged { .. })
                ));
                let loaded = owner.load().unwrap().unwrap();
                let mut accepted = owner.accept(loaded).unwrap();
                subject.pump();
                let writer = DocWriter::try_start(&owner).unwrap();
                subject.flush(&writer).unwrap();
                assert_eq!(
                    writer.durability_snapshot().durable_heads,
                    Some(accepted.get_heads())
                );
                let repaired = subject.store.read(owner.key()).unwrap().unwrap().bytes;
                assert!(AutoCommit::load(&repaired).is_ok(), "{}", subject.name);
            }
        }
        // Phase `a_changed_source_is_never_accepted`.
        {
            for subject in Subject::all() {
                let mut doc = AutoCommit::new();
                doc.put(ROOT, "body", "original").unwrap();
                store_raw(&subject, "changed", doc.save());
                let owner = subject.owner("changed");
                let loaded = owner.load().unwrap().unwrap();
                owner
                    .store()
                    .write(owner.key(), WriteOp::Replace(b"replacement".to_vec()), None)
                    .unwrap();
                subject.pump();
                assert!(matches!(
                    owner.accept(loaded),
                    Err(StoreError::SourceChanged { .. })
                ));
            }
        }
        // Phase `schema_refusal_leaves_newer_documents_untouched`.
        {
            for subject in Subject::all() {
                let mut doc = AutoCommit::new();
                doc.put(ROOT, SCHEMA_VERSION_KEY, u64::from(SCHEMA_VERSION) + 1)
                    .unwrap();
                let bytes = doc.save();
                store_raw(&subject, "newer", bytes.clone());
                let owner = subject.owner("newer");
                assert!(matches!(owner.load(), Err(StoreError::NewerSchema { .. })));
                assert_eq!(
                    subject.store.read(owner.key()).unwrap().unwrap().bytes,
                    bytes
                );
            }
        }
    }

    #[test]
    fn writer_ownership_is_exclusive_shared_or_disabled() {
        // Phase `owners_and_writers_are_exclusive`.
        {
            for subject in Subject::all() {
                let owner = subject.owner("exclusive");
                assert!(matches!(
                    DocOwner::acquire(
                        Rc::clone(&subject.store),
                        StorageKey::record("so.tilo.test", "room", "exclusive"),
                    ),
                    Err(StoreError::WriterBusy { .. })
                ));
                let writer = DocWriter::try_start(&owner).unwrap();
                assert!(matches!(
                    DocWriter::try_start(&owner),
                    Err(StoreError::WriterBusy { .. })
                ));
                drop(writer);
                assert!(DocWriter::try_start(&owner).is_ok());
            }
        }
        // Phase `only_the_primary_shared_writer_continues_the_stored_actor`.
        {
            for subject in Subject::all() {
                let key = StorageKey::package("so.tilo.shared");
                let primary = Rc::new(
                    DocOwner::acquire_shared(Rc::clone(&subject.store), key.clone()).unwrap(),
                );
                let mut doc = primary.new_document().unwrap();
                let writer = DocWriter::start_new(&primary, &mut doc).unwrap();
                subject.flush(&writer).unwrap();
                let secondary = Rc::new(
                    DocOwner::acquire_shared(Rc::clone(&subject.store), key.clone()).unwrap(),
                );
                assert_ne!(secondary.actor(), primary.actor(), "{}", subject.name);
                let mut other = secondary.new_document().unwrap();
                other.put(ROOT, "from", "secondary").unwrap();
                let second = DocWriter::start_new(&secondary, &mut other).unwrap();
                subject.flush(&second).unwrap();
                let actor = primary.actor().clone();
                drop((writer, second, primary, secondary));
                let reopened = DocOwner::acquire_shared(Rc::clone(&subject.store), key).unwrap();
                assert_eq!(reopened.actor(), &actor, "{}", subject.name);
            }
        }
        // Phase `a_disabled_writer_reports_why_and_persists_nothing`.
        {
            let mut writer = DocWriter::disabled("no storage");
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "k", "v").unwrap();
            writer.request(&mut doc);
            writer.request_checkpoint(&mut doc);
            assert!(!writer.is_enabled());
            assert_eq!(writer.last_error().as_deref(), Some("no storage"));
            assert!(writer.try_flush().is_err());
            assert_eq!(writer.document_path(), Path::new(""));
        }
    }
}
