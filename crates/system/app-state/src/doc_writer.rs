//! Canonical Automerge documents persisted through a [`DurableStore`].
//!
//! [`DocOwner`] is a key's single owner: it leases the key, decodes and
//! validates what is stored without publishing anything, and accepts a
//! candidate only once its exact source bytes are still current. [`DocWriter`]
//! then persists the live document the way [`crate::Autosave`] does —
//! `save_after(sent_heads)` appends until the appended volume outgrows the
//! compacted base, then one full save — but it never blocks and owns no
//! thread: the store accepts each write at once, and the writer maps store
//! revisions back to document heads to report what is actually durable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT};
use durable_store::{
    Durability, DurableStore, Lease, Revision, StorageKey, StoreError as SeamError, WriteOp,
};

use crate::autosave::MIN_COMPACT_BYTES;
use crate::{
    DurabilitySnapshot, SCHEMA_VERSION, SCHEMA_VERSION_KEY, StoreError, decode_canonical,
    validate_canonical_schema_of,
};

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

/// The exclusive owner of one canonical document key.
pub struct DocOwner {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    label: PathBuf,
    actor: ActorId,
    actor_stored: Cell<bool>,
    accepted: RefCell<Option<Accepted>>,
    identity: Rc<()>,
    writer_claimed: Cell<bool>,
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
        let stored = store
            .read(&key)?
            .and_then(|stored| stored.writer)
            .filter(|writer| !writer.is_empty());
        let actor_stored = stored.is_some();
        let actor = stored.map_or_else(ActorId::random, ActorId::from);
        let label = PathBuf::from(format!("{} {key}", store.namespace()));
        Ok(Self {
            store,
            key,
            label,
            actor,
            actor_stored: Cell::new(actor_stored),
            accepted: RefCell::new(None),
            identity: Rc::new(()),
            writer_claimed: Cell::new(false),
            _lease: lease,
        })
    }

    pub fn key(&self) -> &StorageKey {
        &self.key
    }

    pub fn store(&self) -> &Rc<dyn DurableStore> {
        &self.store
    }

    /// Names this document in errors and logs.
    pub fn label(&self) -> &Path {
        &self.label
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
            self.store.flush(Some(&self.key))?;
            Accepted {
                heads: heads.clone(),
                base: loaded.source.len(),
                durable: Some(heads),
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
pub struct DocWriter {
    owner: Rc<DocOwner>,
    sent: Vec<ChangeHash>,
    appended: usize,
    base: usize,
    full_writes: usize,
    appends: usize,
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
            owner: Rc::clone(owner),
            sent,
            appended: 0,
            base,
            full_writes: 0,
            appends: 0,
            inflight,
            durable,
            error: None,
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

    pub fn owner(&self) -> &DocOwner {
        &self.owner
    }

    /// Submit the changes `doc` has beyond what was last submitted. Cheap when
    /// nothing changed. Does nothing while a failure is outstanding, so a
    /// failed writer never closes the caller's typing transaction.
    pub fn request(&mut self, doc: &mut AutoCommit) {
        self.settle();
        if self.error.is_some() {
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
        tracing::error!(document = %self.owner.label.display(), %error, "app state write refused");
        self.error = Some(error.to_string());
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
        let writer = !self.owner.actor_stored.get();
        let replace = op.is_replace();
        let revision = self.owner.store.write(
            &self.owner.key,
            op,
            writer.then(|| self.owner.actor.to_bytes()),
        )?;
        self.inflight.push_back(InFlight {
            revision,
            heads: heads.clone(),
            replace,
            writer,
        });
        self.sent = heads;
        Ok(())
    }

    /// Fold resolved writes into the durable heads and sticky error.
    fn settle(&mut self) {
        while let Some(front) = self.inflight.front() {
            match self.owner.store.durability(&self.owner.key, front.revision) {
                Durability::Pending => break,
                Durability::Durable => {
                    let Some(done) = self.inflight.pop_front() else {
                        break;
                    };
                    if done.writer {
                        self.owner.actor_stored.set(true);
                    }
                    if done.replace {
                        self.error = None;
                    }
                    self.durable = Some(done.heads);
                }
                Durability::Failed(message) => {
                    self.inflight.pop_front();
                    self.error = Some(message);
                }
            }
        }
    }

    /// What is durable and whether a failure is outstanding, without
    /// submitting or closing anything.
    pub fn durability_snapshot(&self) -> DurabilitySnapshot {
        let mut snapshot = DurabilitySnapshot {
            durable_heads: self.durable.clone(),
            error: self.error.clone(),
        };
        for write in &self.inflight {
            match self.owner.store.durability(&self.owner.key, write.revision) {
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
    pub fn try_flush(&mut self) -> Result<(), String> {
        let flushed = self.owner.store.flush(Some(&self.owner.key));
        self.settle();
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        flushed.map_err(|error| error.to_string())?;
        if self.inflight.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} still has uncommitted writes",
                self.owner.label.display()
            ))
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
        if let Err(error) = self.owner.store.flush(Some(&self.owner.key)) {
            tracing::debug!(document = %self.owner.label.display(), %error, "writes outstanding at close");
        }
        self.owner.writer_claimed.set(false);
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

        fn flush(&self, writer: &mut DocWriter) -> Result<(), String> {
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
    fn a_burst_of_edits_is_durable_after_a_flush_and_reopens_as_the_same_writer() {
        for subject in Subject::all() {
            let owner = subject.owner("burst");
            let mut doc = owner.new_document().unwrap();
            let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
            let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
            for (i, ch) in "persisted".chars().enumerate() {
                doc.splice_text(&body, i, 0, &ch.to_string()).unwrap();
                writer.request(&mut doc);
            }
            subject.flush(&mut writer).unwrap();
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

    #[test]
    fn steady_edits_append_and_the_log_is_eventually_compacted() {
        for subject in Subject::all() {
            let owner = subject.owner("growth");
            let mut doc = owner.new_document().unwrap();
            let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&body, 0, 0, "seed").unwrap();
            let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
            subject.flush(&mut writer).unwrap();
            for i in 0..50 {
                doc.splice_text(&body, 4 + i, 0, "y").unwrap();
                writer.request(&mut doc);
            }
            subject.flush(&mut writer).unwrap();
            assert_eq!(writer.full_writes(), 1, "{}", subject.name);
            assert_eq!(writer.appends(), 50, "{}", subject.name);
            // Each single-character change is about a hundred bytes, so this
            // passes the compaction floor.
            for i in 0..1200 {
                doc.splice_text(&body, 54 + i, 0, "z").unwrap();
                writer.request(&mut doc);
            }
            subject.flush(&mut writer).unwrap();
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

    #[test]
    fn an_older_acknowledgement_never_marks_newer_edits_saved() {
        for subject in Subject::all() {
            let owner = subject.owner("status");
            let mut doc = owner.new_document().unwrap();
            let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
            subject.flush(&mut writer).unwrap();
            doc.put(ROOT, "value", 1_u64).unwrap();
            writer.request(&mut doc);
            let older = doc.get_heads();
            subject.flush(&mut writer).unwrap();
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
            subject.flush(&mut writer).unwrap();
            assert_eq!(
                writer.durability_snapshot().durable_heads,
                Some(doc.get_heads()),
                "{}",
                subject.name
            );
        }
    }

    #[test]
    fn a_failed_write_requires_a_full_checkpoint_without_consuming_typing() {
        for subject in Subject::all() {
            if subject.queued.is_none() && subject.faults.is_none() {
                continue; // an immediately durable store cannot fail
            }
            let owner = subject.owner("failure");
            let mut doc = owner.new_document().unwrap();
            doc.put(ROOT, "body", "original").unwrap();
            let mut writer = DocWriter::start_new(&owner, &mut doc).unwrap();
            subject.flush(&mut writer).unwrap();
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
            subject.flush(&mut writer).unwrap();
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

    #[test]
    fn undecodable_bytes_are_refused_and_never_overwritten() {
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

    #[test]
    fn a_torn_tail_is_repaired_only_on_acceptance_by_its_owner() {
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
            let mut writer = DocWriter::try_start(&owner).unwrap();
            subject.flush(&mut writer).unwrap();
            assert_eq!(
                writer.durability_snapshot().durable_heads,
                Some(accepted.get_heads())
            );
            let repaired = subject.store.read(owner.key()).unwrap().unwrap().bytes;
            assert!(AutoCommit::load(&repaired).is_ok(), "{}", subject.name);
        }
    }

    #[test]
    fn a_changed_source_is_never_accepted() {
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

    #[test]
    fn owners_and_writers_are_exclusive() {
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

    #[test]
    fn schema_refusal_leaves_newer_documents_untouched() {
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
