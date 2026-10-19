//! Native committed reads stay closed until the real document owner applies them.
use super::{
    Arc, AtomicBool, ByteBudget, ByteReservation, CollabEvent, Duration, InboundBridge, Ordering,
    RouteSpec,
};
use runtime_collab::committed_document::{MAX_DOCUMENT_BYTES, Reply, Request};
use std::collections::BTreeMap;
use tilo_lock::MutexRecover as _;
#[derive(Clone, Default)]
pub(super) struct Reads(
    Arc<std::sync::Mutex<BTreeMap<String, Gate>>>,
    Arc<std::sync::Mutex<BTreeMap<String, CommittedDocument>>>,
    Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    Arc<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Semaphore>>>>,
    Arc<std::sync::Mutex<Option<Vec<String>>>>,
    Arc<Progress>,
);
/// Counts changes a constructing owner waits for (a capture staged, a route
/// becoming publishable), so its bounded wait ends at the change instead of
/// at its next poll.
#[derive(Default)]
struct Progress {
    closed: AtomicBool,
    changes: std::sync::Mutex<u64>,
    changed: std::sync::Condvar,
    /// Wakes a route's reader waiting for its committed read to change when
    /// something here changes instead: closure, a downgrade, a staged
    /// capture, or the owner asking for a fresh read.
    reader: tokio::sync::Notify,
}
#[derive(Clone)]
enum Gate {
    Joining,
    Legacy,
    Required(Arc<AtomicBool>, Arc<std::sync::atomic::AtomicU64>),
}
/// Read-only phase observations for a failed construction barrier. These
/// independently sampled flags are diagnostic metadata, never admission.
pub(super) struct ReadinessDiagnostic {
    pub(super) required: bool,
    pub(super) staged: bool,
    pub(super) applied: bool,
    pub(super) refresh_requested: bool,
}
impl Reads {
    /// Terminal closure retains native requirements and private binding, while
    /// withdrawing every generation before disconnected health is published.
    pub(super) fn close(&self) {
        self.5.closed.store(true, Ordering::Release);
        if let Ok(rows) = self.0.lock() {
            for gate in rows.values() {
                if let Gate::Required(ready, _) = gate {
                    ready.store(false, Ordering::Release);
                }
            }
        }
        if let Ok(mut rows) = self.1.lock() {
            for read in rows.values() {
                read.valid.store(false, Ordering::Release);
            }
            rows.clear();
        }
        if let Ok(mut requests) = self.2.lock() {
            requests.clear();
        }
        self.progressed();
    }
    fn closed(&self) -> bool {
        self.5.closed.load(Ordering::Acquire)
    }
    fn set_ready(&self, ready: &AtomicBool, value: bool) -> bool {
        if !value {
            return ready.swap(false, Ordering::AcqRel);
        }
        let Ok(rows) = self.0.lock() else {
            return ready.swap(false, Ordering::AcqRel);
        };
        let current = !self.closed() && rows.values().any(|gate| {
            matches!(gate, Gate::Required(candidate, _) if std::ptr::eq(candidate.as_ref(), ready))
        });
        // Serialize a positive publication with close's gate withdrawal.
        ready.swap(current, Ordering::AcqRel) != current
    }
    /// How many readiness changes happened so far; pass it to
    /// [`Self::wait_progress`] after checking the state it guards.
    pub(super) fn progress(&self) -> u64 {
        *self.5.changes.lock_recover()
    }
    /// Wait until a readiness change after `seen`, or `bound` elapses.
    pub(super) fn wait_progress(&self, seen: u64, bound: Duration) {
        let changes = self.5.changes.lock_recover();
        let _ = self
            .5
            .changed
            .wait_timeout_while(changes, bound, |changes| *changes == seen);
    }
    pub(super) fn progressed(&self) {
        let mut changes = self.5.changes.lock_recover();
        *changes = changes.wrapping_add(1);
        drop(changes);
        self.5.changed.notify_all();
        self.5.reader.notify_waiters();
    }
    /// A wait for the next [`Self::progressed`] or refresh request. Enable it
    /// before checking the state it guards, so a change in between is kept.
    fn reader_wake(&self) -> tokio::sync::futures::Notified<'_> {
        self.5.reader.notified()
    }
    /// Whether the owner asked for a fresh read, without consuming the ask.
    fn refresh_pending(&self, key: &str) -> bool {
        self.2.lock().is_ok_and(|requests| requests.contains(key))
    }
    pub(super) fn readiness_diagnostic(&self, key: &str) -> Option<ReadinessDiagnostic> {
        let required = self
            .0
            .lock()
            .ok()?
            .get(key)
            .is_some_and(|gate| matches!(gate, Gate::Required(_, _)));
        let staged = self
            .1
            .lock()
            .ok()?
            .get(key)
            .map(|read| read.merged.load(Ordering::Acquire));
        let refresh_requested = self.2.lock().ok()?.contains(key);
        Some(ReadinessDiagnostic {
            required,
            staged: staged.is_some(),
            applied: staged.unwrap_or(false),
            refresh_requested,
        })
    }
    pub(super) fn delivery_generation(&self, key: &str) -> Option<Arc<AtomicBool>> {
        let rows = self.0.lock().ok()?;
        match rows.get(key)? {
            Gate::Required(ready, _) => Some(ready.clone()),
            _ => None,
        }
    }
    pub(super) fn delivery_turn(&self, key: &str) -> Result<Arc<tokio::sync::Semaphore>, String> {
        let mut turns = self.3.lock().map_err(|_| "document turn map poisoned")?;
        Ok(turns
            .entry(key.into())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone())
    }
    pub(super) fn set_document_owner_routes(&self, mut keys: Vec<String>) {
        keys.sort();
        keys.dedup();
        if let Ok(mut owners) = self.4.lock() {
            *owners = Some(keys);
        }
    }
    pub(super) fn document_owner_routes(&self, original: &str) -> Result<Vec<String>, String> {
        self.4
            .lock()
            .map_err(|_| "document owner routes poisoned".into())
            .map(|owners| owners.clone().unwrap_or_else(|| vec![original.into()]))
    }
    #[cfg(test)]
    pub(super) fn close_for_test(&self, key: &str) -> Arc<AtomicBool> {
        self.pending(key).unwrap()
    }
    #[cfg(test)]
    pub(super) fn stage_for_test(&self, key: &str, read: CommittedDocument) {
        let ready = self.pending(key).unwrap();
        self.stage(key, &ready, read).unwrap();
    }
    #[cfg(test)]
    pub(super) fn refresh_for_test(&self, key: &str, read: CommittedDocument) {
        let applied = self.1.lock().unwrap().get(key).unwrap().merged.clone();
        assert!(applied.load(Ordering::Acquire));
        self.clear_stage(key, &applied);
        self.stage_for_test(key, read);
    }
    pub(super) fn publishable(&self, key: &str) -> bool {
        if self.closed() {
            return false;
        }
        self.0
            .lock()
            .ok()
            .and_then(|rows| rows.get(key).cloned())
            .is_some_and(|gate| match gate {
                Gate::Joining => false,
                Gate::Legacy => true,
                Gate::Required(ready, _) => ready.load(Ordering::Acquire),
            })
    }
    pub(super) fn required(&self, key: &str) -> bool {
        self.0
            .lock()
            .is_ok_and(|rows| matches!(rows.get(key), Some(Gate::Required(_, _))))
    }
    pub(super) fn bind_private_window(
        &self,
        key: &str,
        instance: Option<u64>,
    ) -> Result<(), String> {
        let rows = self.0.lock().map_err(|_| "document reads poisoned")?;
        let Some(Gate::Required(_, bound)) = rows.get(key) else {
            return Err("private binding requires admitted native document custody".into());
        };
        bound.store(instance.unwrap_or(0), Ordering::Release);
        Ok(())
    }
    pub(super) fn private_window(&self, key: &str) -> Option<u64> {
        let rows = self.0.lock().ok()?;
        let Gate::Required(_, instance) = rows.get(key)? else {
            return None;
        };
        let instance = instance.load(Ordering::Acquire);
        (instance != 0).then_some(instance)
    }

    pub(super) fn joining(&self, key: &str) -> bool {
        let Ok(mut rows) = self.0.lock() else {
            return false;
        };
        if self.closed() || rows.contains_key(key) || rows.len() >= 64 {
            return false;
        }
        rows.insert(key.into(), Gate::Joining);
        true
    }
    pub(super) fn initial(&self, key: &str) -> Option<CommittedDocument> {
        // Only a whole document can seed an owner; staged changes apply
        // only to the owner that holds their base.
        if let Some(read) = self.1.lock().ok().and_then(|rows| rows.get(key).cloned()) {
            if read.base.is_none() {
                return Some(read);
            }
            // An owner being built needs the whole document: the reader
            // drops these changes and reads it whole.
            #[cfg(any(test, feature = "test-support"))]
            tracing::warn!(
                key,
                "staged committed changes refused for an owner being built"
            );
            read.refuse();
            return None;
        }
        if self.publishable(key) && self.required(key) {
            #[cfg(any(test, feature = "test-support"))]
            tracing::warn!(key, "an owner asked for a whole committed read");
            if let Ok(mut refresh) = self.2.lock() {
                refresh.insert(key.into());
            }
            self.5.reader.notify_waiters();
        }
        None
    }
    /// The current staged capture still awaiting the canonical owner's merge.
    /// Unlike `initial`, observing this handoff does not request a refresh.
    pub(super) fn unapplied(&self, key: &str) -> Option<CommittedDocument> {
        let gates = self.0.lock().ok()?;
        if !matches!(gates.get(key), Some(Gate::Required(_, _))) {
            return None;
        }
        self.1
            .lock()
            .ok()?
            .get(key)
            .filter(|read| {
                read.is_current() && read.base.is_none() && !read.merged.load(Ordering::Acquire)
            })
            .cloned()
    }
    fn stage(
        &self,
        key: &str,
        ready: &Arc<AtomicBool>,
        read: CommittedDocument,
    ) -> Result<(), String> {
        let gates = self.0.lock().map_err(|_| "document reads poisoned")?;
        if self.closed()
            || !matches!(gates.get(key),Some(Gate::Required(current, _)) if Arc::ptr_eq(current,ready))
        {
            return Err("committed route replaced".into());
        }
        let mut rows = self
            .1
            .lock()
            .map_err(|_| "committed read staging poisoned")?;
        if !rows.contains_key(key) && rows.len() >= 64 {
            return Err("committed staging capacity".into());
        }
        rows.insert(key.into(), read);
        drop(rows);
        drop(gates);
        self.progressed();
        Ok(())
    }
    fn clear_stage(&self, key: &str, merged: &Arc<AtomicBool>) {
        if let Ok(mut rows) = self.1.lock()
            && rows
                .get(key)
                .is_some_and(|read| Arc::ptr_eq(&read.merged, merged))
        {
            rows.remove(key);
        }
    }
    pub(super) fn current(&self, key: &str, ready: &Arc<AtomicBool>) -> bool {
        if self.closed() {
            return false;
        }
        self.0.lock().is_ok_and(|rows|matches!(rows.get(key),Some(Gate::Required(current, _)) if Arc::ptr_eq(current,ready)))
    }
    fn discard_stage(&self, key: &str) {
        if let Ok(mut rows) = self.1.lock()
            && let Some(read) = rows.remove(key)
        {
            read.valid.store(false, Ordering::Release);
        }
        if let Ok(mut requests) = self.2.lock() {
            requests.remove(key);
        }
    }
    fn refresh_requested(&self, key: &str) -> bool {
        self.2.lock().is_ok_and(|mut requests| requests.remove(key))
    }
    fn legacy(&self, key: &str) -> Result<(), String> {
        let mut rows = self.0.lock().map_err(|_| "document reads poisoned")?;
        if self.closed() {
            return Err("committed document reader closed".into());
        }
        rows.insert(key.into(), Gate::Legacy);
        drop(rows);
        self.progressed();
        Ok(())
    }
    fn pending(&self, key: &str) -> Result<Arc<AtomicBool>, String> {
        let mut rows = self.0.lock().map_err(|_| "document reads poisoned")?;
        if self.closed() {
            return Err("committed document reader closed".into());
        }
        if let Some(Gate::Required(old, _)) = rows.get(key) {
            old.store(false, Ordering::Release);
        }
        self.discard_stage(key);
        if !rows.contains_key(key) && rows.len() >= 64 {
            return Err("committed document route capacity".into());
        }
        let ready = Arc::new(AtomicBool::new(false));
        let instance = match rows.get(key) {
            Some(Gate::Required(_, instance)) => instance.clone(),
            _ => Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        rows.insert(key.into(), Gate::Required(ready.clone(), instance));
        Ok(ready)
    }
}
/// A disposable capture from the actual admitted SDK channel. It cannot be
/// constructed by an application. Its ACK opens only this route generation.
#[derive(Clone)]
pub struct CommittedDocument {
    data: Arc<Captured>,
    merged: Arc<AtomicBool>,
    /// Wakes the route's reader as soon as the owner merges this capture,
    /// so its confirming read follows the merge at once.
    merge_wake: Arc<tokio::sync::Notify>,
    valid: Arc<AtomicBool>,
    creation: Option<runtime_collab::committed_document::CreationReceipt>,
    revision: [u8; 32],
    initialized: Arc<AtomicBool>,
    /// Set when these are only the changes since the revision with these
    /// heads: they apply only to an owner document holding every one.
    base: Option<Arc<[automerge::ChangeHash]>>,
    /// The owner could not apply the changes (it lacks their base): the
    /// reader then reads the document whole.
    refused: Arc<AtomicBool>,
}
impl std::fmt::Debug for CommittedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommittedDocument")
            .field(
                "bytes",
                &self.data.bytes.lock().map(|bytes| bytes.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}
impl CommittedDocument {
    pub(crate) fn creation_workspace(
        &self,
    ) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
        self.is_current().then_some(())?;
        if self.initialized.load(Ordering::Acquire) {
            return None;
        }
        self.creation.as_ref().map(|receipt| &receipt.workspace)
    }
    pub(crate) fn initialization_request(
        &self,
        kind: &str,
        document: Vec<u8>,
    ) -> Result<Request, String> {
        if self.creation_workspace().is_none() {
            return Err("original application creation receipt unavailable".into());
        }
        let receipt = self
            .creation
            .as_ref()
            .ok_or("creation receipt unavailable")?;
        let request = Request::Initialize {
            token: receipt.token,
            revision: self.revision,
            kind: kind.into(),
            document,
        };
        request.encode().map_err(str::to_owned)?;
        Ok(request)
    }
    pub(crate) fn initialized(&self) {
        self.initialized.store(true, Ordering::Release);
    }
    pub(crate) fn with_mutation<T>(
        &self,
        apply: impl FnOnce(&collab_types::DocumentMutation) -> T,
    ) -> Result<T, String> {
        if !self.is_current() {
            return Err("committed capture route replaced".into());
        }
        let mut bytes = self
            .data
            .bytes
            .lock()
            .map_err(|_| "committed history capture poisoned")?;
        let taken = std::mem::take(&mut *bytes);
        let mutation = if self.base.is_some() {
            collab_types::DocumentMutation::Incremental(taken)
        } else {
            collab_types::DocumentMutation::Snapshot(taken)
        };
        let result = apply(&mutation);
        let (collab_types::DocumentMutation::Snapshot(restored)
        | collab_types::DocumentMutation::Incremental(restored)) = mutation;
        *bytes = restored;
        Ok(result)
    }
    /// Whether these bytes apply to `document`: always for a whole
    /// document; for changes, only when it holds every head of their base,
    /// since Automerge would otherwise hold them back unapplied and the
    /// reader would confirm a document the owner does not show.
    pub(crate) fn applies_to(&self, document: &impl automerge::ReadDoc) -> bool {
        self.base
            .as_ref()
            .is_none_or(|base| document.get_missing_deps(base).is_empty())
    }
    /// The owner cannot apply these changes: the reader drops them and reads
    /// the document whole. Never for a whole document.
    pub(crate) fn refuse(&self) {
        if self.base.is_some() {
            self.refused.store(true, Ordering::Release);
            self.merge_wake.notify_one();
        }
    }
    pub(crate) fn is_current(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
    pub(crate) fn applied(&self) {
        self.merged.store(true, Ordering::Release);
        // A permit is kept when the reader is not waiting yet, so a merge
        // between its check and its wait is never missed.
        self.merge_wake.notify_one();
    }
    /// A current capture of `bytes`: the changes since `base`, or the whole
    /// document when `base` is `None`.
    #[cfg(test)]
    pub(crate) fn for_test(bytes: Vec<u8>, base: Option<Vec<automerge::ChangeHash>>) -> Self {
        let budget = ByteBudget::new();
        let length = bytes.len().max(1);
        Self {
            data: Arc::new(Captured {
                bytes: std::sync::Mutex::new(bytes),
                _reservation: budget.reserve(length, length).unwrap(),
            }),
            merged: Arc::new(AtomicBool::new(false)),
            merge_wake: Arc::new(tokio::sync::Notify::new()),
            valid: Arc::new(AtomicBool::new(true)),
            creation: None,
            revision: [0; 32],
            initialized: Arc::new(AtomicBool::new(false)),
            base: base.map(Into::into),
            refused: Arc::new(AtomicBool::new(false)),
        }
    }
    #[cfg(test)]
    pub(crate) fn was_applied(&self) -> bool {
        self.merged.load(Ordering::Acquire)
    }
}
struct Captured {
    bytes: std::sync::Mutex<Vec<u8>>,
    _reservation: ByteReservation,
}
struct Assembly {
    id: u64,
    token: [u8; 16],
    revision: [u8; 32],
    total: usize,
    bytes: Vec<u8>,
    /// The heads changes start from; `None` for a whole document.
    base: Option<Vec<[u8; 32]>>,
}
impl Assembly {
    fn start(reply: Reply) -> Result<Self, String> {
        let (capture, revision, total, offset, bytes, token, base) = match reply {
            Reply::Chunk {
                capture,
                revision,
                total,
                offset,
                bytes,
                token,
            } => (capture, revision, total, offset, bytes, token, None),
            Reply::Changes {
                capture,
                revision,
                base,
                total,
                bytes,
                token,
            } if !base.is_empty() => (capture, revision, total, 0, bytes, token, Some(base)),
            _ => return Err("committed document initial chunk missing".into()),
        };
        if offset != 0
            || total == 0
            || total as usize > MAX_DOCUMENT_BYTES
            || bytes.len() > total as usize
        {
            return Err("committed document initial bounds".into());
        }
        let mut result = Self {
            id: capture,
            token,
            revision,
            total: total as usize,
            bytes: Vec::new(),
            base,
        };
        result
            .bytes
            .try_reserve_exact(total as usize)
            .map_err(|_| "committed document allocation")?;
        result.bytes.extend(bytes);
        Ok(result)
    }
    fn append(&mut self, reply: Reply) -> Result<(), String> {
        let Reply::Chunk {
            capture,
            revision,
            total,
            offset,
            bytes,
            token,
        } = reply
        else {
            return Err("committed document continuation missing".into());
        };
        if capture != self.id
            || revision != self.revision
            || total as usize != self.total
            || offset as usize != self.bytes.len()
            || bytes.is_empty()
            || self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|end| end > self.total)
        {
            return Err("committed document continuation changed".into());
        }
        self.bytes.extend(bytes);
        self.token = token;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// An agent's connection never asks for a committed document, which the
    /// broker would refuse it; a person's app still reads its own.
    #[test]
    fn only_a_person_route_opens_its_committed_document() {
        assert!(!opens_committed_document("agent"));
        assert!(!opens_committed_document(super::super::SERVICE_PARTICIPANT));
        assert!(opens_committed_document("person"));
    }
    #[tokio::test]
    async fn terminal_reads_cannot_be_republished_or_renewed() {
        let reads = Reads::default();
        let ready = reads.pending("original-native-route").unwrap();
        reads
            .bind_private_window("original-native-route", Some(42))
            .unwrap();
        let read = one_chunk_capture(&ByteBudget::new()).await;
        reads
            .stage("original-native-route", &ready, read.clone())
            .unwrap();
        assert!(read.is_current());
        assert!(reads.set_ready(&ready, true));
        assert!(reads.publishable("original-native-route"));
        reads.close();
        assert!(!read.is_current(), "closure invalidates retained captures");
        assert!(reads.initial("original-native-route").is_none());
        assert!(reads.stage("original-native-route", &ready, read).is_err());
        assert!(
            !reads.set_ready(&ready, true),
            "a late unchanged reply cannot revive readiness"
        );
        assert!(!ready.load(Ordering::Acquire));
        assert!(!reads.publishable("original-native-route"));
        assert!(!reads.current("original-native-route", &ready));
        assert!(
            reads.required("original-native-route"),
            "closure must retain the native requirement"
        );
        assert_eq!(
            reads.private_window("original-native-route"),
            Some(42),
            "closure does not acquire a new private instance"
        );
        assert!(
            reads.pending("original-native-route").is_err(),
            "the terminal reader cannot admit a new generation"
        );
    }

    async fn one_chunk_capture(budget: &ByteBudget) -> CommittedDocument {
        read_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [7; 32],
                total: 1,
                offset: 0,
                bytes: vec![1],
                token: [3; 16],
            },
            budget,
            async |_| Ok(Reply::Cancelled),
        )
        .await
        .unwrap()
        .0
    }
    #[tokio::test(start_paused = true)]
    async fn the_owner_s_merge_ends_the_reader_s_wait_at_once() {
        let budget = ByteBudget::new();
        let read = one_chunk_capture(&budget).await;
        let started = tokio::time::Instant::now();
        let owner = {
            let read = read.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                read.applied();
            })
        };
        until_merged(&read, || true).await;
        owner.await.unwrap();
        // The clock is paused: a wait that slept until its re-check would
        // have advanced it by a whole interval.
        assert!(started.elapsed() < MERGE_RECHECK);
    }
    #[tokio::test(start_paused = true)]
    async fn a_merge_before_the_reader_waits_is_not_missed() {
        let budget = ByteBudget::new();
        let read = one_chunk_capture(&budget).await;
        read.applied();
        let started = tokio::time::Instant::now();
        until_merged(&read, || true).await;
        assert!(started.elapsed() < MERGE_RECHECK);
    }
    #[tokio::test(start_paused = true)]
    async fn an_unmerged_capture_keeps_the_reader_waiting_until_its_route_goes() {
        let budget = ByteBudget::new();
        let read = one_chunk_capture(&budget).await;
        let checks = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        until_merged(&read, || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        })
        .await;
        assert!(!read.merged.load(Ordering::Acquire));
        // Two re-checks passed while the route stayed live.
        assert!(started.elapsed() >= MERGE_RECHECK * 2);
        assert_eq!(checks.get(), 3);
    }
    #[test]
    fn staging_a_capture_ends_the_owner_s_bounded_wait() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let budget = ByteBudget::new();
        let read = runtime.block_on(one_chunk_capture(&budget));
        let reads = Reads::default();
        let ready = reads.pending("route").unwrap();
        let seen = reads.progress();
        let waiter = {
            let reads = reads.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                reads.wait_progress(seen, Duration::from_secs(30));
                started.elapsed()
            })
        };
        reads.stage("route", &ready, read).unwrap();
        assert!(waiter.join().unwrap() < Duration::from_secs(30));
        assert!(reads.initial("route").is_some());
    }
    #[test]
    fn a_wait_after_the_change_it_watches_for_returns_at_once() {
        let reads = Reads::default();
        let seen = reads.progress();
        reads.legacy("route").unwrap();
        let started = std::time::Instant::now();
        reads.wait_progress(seen, Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(reads.publishable("route"));
    }
    #[tokio::test]
    async fn committed_document_downgrade_excludes_unapplied_owner_handoff() {
        use automerge::transaction::Transactable;
        let mut graph = automerge::AutoCommit::new();
        graph
            .put(automerge::ROOT, "body", "retained owner graph")
            .unwrap();
        graph.commit();
        let bytes = graph.save();
        let budget = ByteBudget::new();
        let (read, _) = read_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [7; 32],
                total: u32::try_from(bytes.len()).unwrap(),
                offset: 0,
                bytes,
                token: [3; 16],
            },
            &budget,
            async |_| Ok(Reply::Cancelled),
        )
        .await
        .unwrap();
        let reads = Reads::default();
        let ready = reads.pending("route").unwrap();
        reads.stage("route", &ready, read.clone()).unwrap();
        assert!(reads.unapplied("route").is_some());
        reads.legacy("route").unwrap();
        assert!(
            reads.publishable("route"),
            "existing legacy publication semantics remain unchanged"
        );
        assert!(
            reads.unapplied("route").is_none(),
            "a downgraded gate cannot hand retained bytes to a new owner"
        );
        assert!(
            !read.merged.load(Ordering::Acquire),
            "observing a downgrade cannot acknowledge the capture"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_acknowledgement_within_the_open_deadline_keeps_the_capture() {
        let (read, revision) = read_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [5; 32],
                total: 1,
                offset: 0,
                bytes: vec![1],
                token: [6; 16],
            },
            &ByteBudget::new(),
            async |request| {
                assert!(matches!(request, Request::Cancel { capture: 1, .. }));
                // A loaded broker answers in hundreds of milliseconds.
                tokio::time::sleep(Duration::from_millis(900)).await;
                Ok(Reply::Cancelled)
            },
        )
        .await
        .expect("a complete capture survives a slow but answered acknowledgement");
        assert_eq!(revision, [5; 32]);
        assert!(!read.merged.load(Ordering::Acquire));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_acknowledgement_still_fails_the_capture() {
        let result = read_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [5; 32],
                total: 1,
                offset: 0,
                bytes: vec![1],
                token: [6; 16],
            },
            &ByteBudget::new(),
            async |_| std::future::pending::<Result<Reply, String>>().await,
        )
        .await;
        assert!(
            result.is_err(),
            "an unknown acknowledgement outcome is never accepted"
        );
    }

    #[tokio::test]
    async fn construction_diagnostic_does_not_request_or_consume_owner_refresh() {
        let reads = Reads::default();
        let ready = reads.pending("route").unwrap();
        let (read, _) = read_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [3; 32],
                total: 1,
                offset: 0,
                bytes: vec![1],
                token: [4; 16],
            },
            &ByteBudget::new(),
            async |request| {
                assert!(matches!(request, Request::Cancel { capture: 1, .. }));
                Ok(Reply::Cancelled)
            },
        )
        .await
        .unwrap();
        reads.stage("route", &ready, read.clone()).unwrap();
        let phase = reads.readiness_diagnostic("route").unwrap();
        assert!(phase.required && phase.staged && !phase.applied);
        assert!(!phase.refresh_requested && !reads.publishable("route"));
        read.applied();
        assert!(reads.readiness_diagnostic("route").unwrap().applied);
        reads.clear_stage("route", &read.merged);
        ready.store(true, Ordering::Release);
        let phase = reads.readiness_diagnostic("route").unwrap();
        assert!(!phase.staged && !phase.refresh_requested);
        assert!(reads.publishable("route"));
        assert!(!reads.refresh_requested("route"));
        assert!(reads.initial("route").is_none());
        assert!(
            reads
                .readiness_diagnostic("route")
                .unwrap()
                .refresh_requested
        );
        assert!(reads.refresh_requested("route"));
        assert!(!reads.refresh_requested("route"));
    }

    #[tokio::test]
    async fn committed_small_captures_share_the_existing_byte_budget() {
        use automerge::{ReadDoc, transaction::Transactable};
        let budget = ByteBudget::new();
        let payloads: Vec<_> = (1..=3)
            .map(|capture| {
                let mut graph = automerge::AutoCommit::new();
                graph
                    .put(automerge::ROOT, "body", format!("capture{capture}"))
                    .unwrap();
                graph.commit();
                graph.save()
            })
            .collect();
        let charged: usize = payloads.iter().map(Vec::len).sum();
        let mut retained = Vec::new();
        for (index, payload) in payloads.iter().enumerate() {
            let capture = u64::try_from(index + 1).unwrap();
            let result = read_capture_using(
                Reply::Chunk {
                    capture,
                    revision: [7; 32],
                    total: u32::try_from(payload.len()).unwrap(),
                    offset: 0,
                    bytes: payload.clone(),
                    token: [3; 16],
                },
                &budget,
                async |request| {
                    assert_eq!(
                        request,
                        Request::Cancel {
                            capture,
                            token: [3; 16]
                        }
                    );
                    Ok(Reply::Cancelled)
                },
            )
            .await;
            assert!(
                result.is_ok(),
                "three valid tiny graphs fit the existing byte budget: {:?}",
                result.as_ref().err()
            );
            retained.push(result.unwrap().0);
        }
        assert_eq!(retained.len(), 3);
        for (index, read) in retained.iter().enumerate() {
            read.with_mutation(|mutation| {
                assert_eq!(
                    mutation,
                    &collab_types::DocumentMutation::Snapshot(payloads[index].clone())
                );
                let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                    panic!("full captured graph required")
                };
                let graph = automerge::AutoCommit::load(bytes).unwrap();
                assert_eq!(
                    graph
                        .get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some(format!("capture{}", index + 1).as_str())
                );
            })
            .unwrap();
        }
        let limit = MAX_DOCUMENT_BYTES * 2;
        let remainder = budget.reserve(limit - charged, limit).unwrap();
        assert!(budget.reserve(1, limit).is_none());
        let clone = retained[0].clone();
        retained.remove(0);
        assert!(
            budget.reserve(1, limit).is_none(),
            "the capture clone retains its one lease"
        );
        drop(clone);
        let released = budget
            .reserve(payloads[0].len(), limit)
            .expect("the final capture owner releases its payload bytes");
        assert!(budget.reserve(1, limit).is_none());
        drop(released);
        drop(retained);
        drop(remainder);
        assert!(budget.reserve(limit, limit).is_some());
    }
    #[tokio::test]
    async fn committed_capture_invalid_totals_acknowledge_without_retaining_a_lease() {
        for (total, offset, bytes) in [
            (0, 0, vec![]),
            (u32::try_from(MAX_DOCUMENT_BYTES).unwrap() + 1, 0, vec![1]),
            (1, 0, vec![1, 2]),
            (1, 1, vec![1]),
        ] {
            let budget = ByteBudget::new();
            let mut acknowledged = false;
            let result = read_capture_using(
                Reply::Chunk {
                    capture: 1,
                    revision: [7; 32],
                    total,
                    offset,
                    bytes,
                    token: [3; 16],
                },
                &budget,
                async |request| {
                    assert_eq!(
                        request,
                        Request::Cancel {
                            capture: 1,
                            token: [3; 16]
                        }
                    );
                    acknowledged = true;
                    Ok(Reply::Cancelled)
                },
            )
            .await;
            assert_eq!(result.unwrap_err(), "committed document initial bounds");
            assert!(acknowledged);
            assert!(
                budget
                    .reserve(MAX_DOCUMENT_BYTES * 2, MAX_DOCUMENT_BYTES * 2)
                    .is_some()
            );
        }
    }
    #[test]
    fn committed_document_publication_waits_for_owner_and_old_ack_cannot_reopen() {
        let reads = Reads::default();
        assert!(!reads.publishable("route"));
        let old = reads.pending("route").unwrap();
        let current = reads.pending("route").unwrap();
        old.store(true, Ordering::Release);
        assert!(!reads.publishable("route"));
        current.store(true, Ordering::Release);
        assert!(reads.publishable("route"));
        reads.pending("route").unwrap();
        assert!(!reads.publishable("route"));
        reads.legacy("legacy").unwrap();
        assert!(reads.publishable("legacy"));
    }
    #[test]
    fn native_private_binding_cannot_open_document_or_survive_route_downgrade() {
        let reads = Reads::default();
        assert!(reads.bind_private_window("route", Some(42)).is_err());
        assert!(reads.joining("route"));
        assert!(reads.bind_private_window("route", Some(42)).is_err());
        let old = reads.pending("route").unwrap();
        reads.bind_private_window("route", Some(42)).unwrap();
        assert_eq!(reads.private_window("route"), Some(42));
        assert!(
            !reads.publishable("route"),
            "a window binding cannot acknowledge a document"
        );
        let current = reads.pending("route").unwrap();
        assert_eq!(
            reads.private_window("route"),
            Some(42),
            "document refresh retains the original launch binding"
        );
        old.store(true, Ordering::Release);
        assert!(!reads.publishable("route"));
        current.store(true, Ordering::Release);
        assert!(reads.publishable("route"));
        reads.bind_private_window("route", None).unwrap();
        assert_eq!(reads.private_window("route"), None);
        reads.legacy("route").unwrap();
        assert!(reads.bind_private_window("route", Some(42)).is_err());
        assert_eq!(reads.private_window("route"), None);
    }

    #[test]
    fn committed_document_replacing_route_discards_staged_capture() {
        let reads = Reads::default();
        let ready = reads.pending("route").unwrap();
        let budget = ByteBudget::new();
        let read = CommittedDocument {
            data: Arc::new(Captured {
                bytes: std::sync::Mutex::new(vec![1]),
                _reservation: budget.reserve(1, 1).unwrap(),
            }),
            merged: Arc::new(AtomicBool::new(false)),
            merge_wake: Arc::new(tokio::sync::Notify::new()),
            valid: Arc::new(AtomicBool::new(true)),
            creation: None,
            revision: [0; 32],
            initialized: Arc::new(AtomicBool::new(false)),
            base: None,
            refused: Arc::new(AtomicBool::new(false)),
        };
        reads.stage("route", &ready, read.clone()).unwrap();
        assert!(reads.unapplied("route").is_some());
        read.applied();
        assert!(
            reads.unapplied("route").is_none(),
            "acknowledged capture must not merge again"
        );
        read.merged.store(false, Ordering::Release);
        reads.pending("route").unwrap();
        assert!(
            reads.unapplied("route").is_none(),
            "replaced capture must not reach the owner"
        );
        assert!(
            reads.initial("route").is_none(),
            "old generation must not hydrate the replacement"
        );
        assert!(
            read.with_mutation(|_| ()).is_err(),
            "queued old captures cannot be merged after replacement"
        );
        assert!(
            reads.stage("route", &ready, read).is_err(),
            "old worker cannot restage after replacement"
        );
    }
    #[tokio::test]
    async fn committed_document_allocation_refusal_acknowledges_received_token() {
        let budget = ByteBudget::new();
        let _held = budget
            .reserve(MAX_DOCUMENT_BYTES * 2, MAX_DOCUMENT_BYTES * 2)
            .unwrap();
        let first = Reply::Chunk {
            capture: 1,
            revision: [7; 32],
            total: 1,
            offset: 0,
            bytes: vec![1],
            token: [3; 16],
        };
        let mut sent = Vec::new();
        assert!(
            read_capture_using(first, &budget, async |request| {
                sent.push(request);
                Ok(Reply::Cancelled)
            })
            .await
            .is_err()
        );
        assert_eq!(
            sent,
            vec![Request::Cancel {
                capture: 1,
                token: [3; 16]
            }],
            "refusing destination bytes must release the acknowledged broker response"
        );
    }
    #[test]
    fn committed_document_assembly_refuses_changed_revision_and_offset() {
        let chunk = |revision, offset, bytes| Reply::Chunk {
            capture: 1,
            revision,
            total: 3,
            offset,
            bytes,
            token: [3; 16],
        };
        let mut assembly = Assembly::start(chunk([7; 32], 0, vec![1])).unwrap();
        assert!(assembly.append(chunk([8; 32], 1, vec![2])).is_err());
        assert!(assembly.append(chunk([7; 32], 2, vec![2])).is_err());
        assembly.append(chunk([7; 32], 1, vec![2, 3])).unwrap();
        assert_eq!(assembly.bytes, vec![1, 2, 3]);
    }
}

fn capture_budget() -> &'static ByteBudget {
    static BUDGET: std::sync::OnceLock<ByteBudget> = std::sync::OnceLock::new();
    BUDGET.get_or_init(ByteBudget::new)
}
/// How long an assembled capture waits for the broker to accept its
/// acknowledgement; equal to the polling Open deadline.
const ACKNOWLEDGEMENT_DEADLINE: Duration = Duration::from_secs(2);

/// Only local deadlines permit retiring a capture and renewing its read epoch,
/// or retrying a renewal. Broker refusals, invalid replies and authority
/// failures remain terminal.
#[derive(Debug)]
enum CaptureFailure {
    Deadline(&'static str),
    Rejected(String),
}
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline(message) => formatter.write_str(message),
            Self::Rejected(message) => formatter.write_str(message),
        }
    }
}
impl From<String> for CaptureFailure {
    fn from(message: String) -> Self {
        Self::Rejected(message)
    }
}
impl From<&str> for CaptureFailure {
    fn from(message: &str) -> Self {
        Self::Rejected(message.into())
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
async fn read_capture(
    channel: &runtime_collab::ChannelHandle,
    first: Reply,
) -> Result<(CommittedDocument, [u8; 32]), CaptureFailure> {
    capture_using(first, capture_budget(), async |request| {
        channel
            .committed_document(&request)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}
#[cfg(test)]
pub(super) async fn read_capture_using(
    first: Reply,
    budget: &ByteBudget,
    send: impl AsyncFnMut(Request) -> Result<Reply, String>,
) -> Result<(CommittedDocument, [u8; 32]), String> {
    capture_using(first, budget, send)
        .await
        .map_err(|error| error.to_string())
}
async fn capture_using(
    first: Reply,
    budget: &ByteBudget,
    mut send: impl AsyncFnMut(Request) -> Result<Reply, String>,
) -> Result<(CommittedDocument, [u8; 32]), CaptureFailure> {
    let (first, creation) = match first {
        Reply::Created { receipt, chunk } => (chunk.into_reply(), Some(receipt)),
        reply => (reply, None),
    };
    // Reserve before the full-document destination allocation. Every clone of
    // the event retains this one shared reservation until the last owner drops.
    let (first_id, first_token, total) = match &first {
        Reply::Chunk {
            capture,
            token,
            total,
            ..
        }
        | Reply::Changes {
            capture,
            token,
            total,
            ..
        } => (*capture, *token, *total as usize),
        _ => return Err("committed initial capture missing".into()),
    };
    let prepared = (|| {
        if total == 0 || total > MAX_DOCUMENT_BYTES {
            return Err("committed document initial bounds".into());
        }
        // Charge the validated raw payload before allocating its destination.
        // Clones share this one lease; the aggregate limit is unchanged.
        let reservation = budget
            .reserve(total, MAX_DOCUMENT_BYTES * 2)
            .ok_or("SDK committed read capacity")?;
        Ok::<_, String>((reservation, Assembly::start(first)?))
    })();
    let (reservation, mut assembly) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = tokio::time::timeout(
                Duration::from_millis(200),
                send(Request::Cancel {
                    capture: first_id,
                    token: first_token,
                }),
            )
            .await;
            return Err(error.into());
        }
    };
    let id = assembly.id;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        while assembly.bytes.len() < assembly.total {
            let reply = send(Request::Next {
                capture: id,
                offset: assembly.bytes.len() as u32,
                token: assembly.token,
            })
            .await?;
            assembly.append(reply)?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| CaptureFailure::Deadline("committed read timed out"))
    .and_then(|result| result.map_err(CaptureFailure::Rejected));
    if let Err(error) = result {
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            send(Request::Cancel {
                capture: id,
                token: assembly.token,
            }),
        )
        .await;
        return Err(error);
    }
    // The final token is learned only after receiving the entire last chunk.
    // The acknowledgement is an ordinary broker round trip, so it gets the
    // same deadline as the Open that produced the capture. A shorter one
    // fails a complete, verified capture closed whenever the broker answers
    // slowly, which permanently disables the route (#4001).
    let acknowledgement = tokio::time::timeout(
        ACKNOWLEDGEMENT_DEADLINE,
        send(Request::Cancel {
            capture: id,
            token: assembly.token,
        }),
    )
    .await
    .map_err(|_| CaptureFailure::Deadline("committed read acknowledgement timed out"))??;
    if acknowledgement != Reply::Cancelled {
        return Err("committed read acknowledgement invalid".into());
    }
    Ok((
        CommittedDocument {
            data: Arc::new(Captured {
                bytes: std::sync::Mutex::new(assembly.bytes),
                _reservation: reservation,
            }),
            merged: Arc::new(AtomicBool::new(false)),
            merge_wake: Arc::new(tokio::sync::Notify::new()),
            valid: Arc::new(AtomicBool::new(true)),
            creation,
            revision: assembly.revision,
            initialized: Arc::new(AtomicBool::new(false)),
            base: assembly
                .base
                .map(|base| base.into_iter().map(automerge::ChangeHash).collect()),
            refused: Arc::new(AtomicBool::new(false)),
        },
        assembly.revision,
    ))
}
// A parked owner must observe both publication readiness and its withdrawal.
fn set_read_ready(inbound: &InboundBridge, ready: &AtomicBool, value: bool) {
    if inbound.state.document_reads.set_ready(ready, value) {
        inbound.state.document_reads.progressed();
        inbound.state.notify_events();
    }
}

/// Resolve mode before the route can produce any document. Initial history is
/// queued without waiting for the UI owner (which awaits PrimaryReady itself).
/// Retains the original authenticated root and declaration; renewal never
/// reconnects a launcher, changes a route or acquires a different authority.
pub(super) struct NativeReadJoin {
    pub(super) client: runtime_collab::CollabClient,
    pub(super) presentation: Vec<u8>,
    pub(super) participant_handle: String,
    pub(super) participant_kind: String,
    pub(super) record_kind: String,
}
impl NativeReadJoin {
    #[expect(
        clippy::future_not_send,
        reason = "the original capnp root stays on its shovel LocalSet"
    )]
    async fn renew(
        &self,
        spec: &RouteSpec,
        reads: &Reads,
    ) -> Result<runtime_collab::ChannelHandle, CaptureFailure> {
        let key = spec.channel_id();
        let expected_instance = reads.private_window(&key);
        tokio::time::timeout(Duration::from_secs(2), async {
            let channel = self
                .client
                .join_channel_as_kind(
                    &key,
                    &self.presentation,
                    &self.participant_handle,
                    super::broker_participant_kind(&self.participant_kind),
                    &self.record_kind,
                )
                .await
                .map_err(|_| "original committed read renewal was not admitted")?;
            let binding = channel
                .committed_document(&Request::PrivateWindowBinding)
                .await
                .map_err(|_| "renewed committed read custody unavailable")?;
            if binding
                != (Reply::PrivateWindowBinding {
                    instance: expected_instance,
                })
            {
                return Err("renewed committed read launcher instance changed".into());
            }
            Ok::<_, String>(channel)
        })
        .await
        .map_err(|_| CaptureFailure::Deadline("original committed read renewal deadline elapsed"))?
        .map_err(CaptureFailure::Rejected)
    }
}

/// How often a reader waiting for the owner's merge re-checks that its route
/// is still healthy and current. The merge itself wakes it at once.
const MERGE_RECHECK: Duration = Duration::from_millis(100);

/// The shortest gap between two Opens of one route, however often its
/// document changes, and so how often a reader re-Opens while its broker's
/// `Await` keeps failing.
const READ_RECHECK: Duration = Duration::from_millis(200);

/// How long a reader waits for a change before it confirms its read with an
/// Open anyway. Every change it depends on ends the wait at once; this only
/// bounds how long a broker that stopped answering (a wedged owner) goes
/// unnoticed, which the Open's own deadline then turns into the renewal it
/// always was. 2 requests per 30 s instead of 5 per second.
const AWAIT_KEEPALIVE: Duration = Duration::from_secs(30);

/// What ended a reader's wait for its committed read to change.
#[derive(Debug, PartialEq, Eq)]
enum Woke {
    /// The broker says an Open would now answer differently: a new
    /// revision, a refusal, the record becoming readable.
    Stale,
    /// Something on this side changed: the route closed or was replaced,
    /// or the owner asked for a fresh read.
    Local,
    /// Nothing changed for [`AWAIT_KEEPALIVE`]: confirm the read.
    Keepalive,
    /// The wait failed or was answered with something other than `Stale`:
    /// the next Open decides.
    Refused,
}

/// Wait, without traffic and without a timer, until an Open of this route
/// would answer differently from the `known` revision the reader holds
/// (`None` after `Unavailable`), or until something on this side changes.
/// The answer is never a read: the caller Opens, under its delivery turn,
/// and only that Open publishes readiness or hands out bytes.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
async fn until_stale(
    channel: &runtime_collab::ChannelHandle,
    known: Option<[u8; 32]>,
    inbound: &InboundBridge,
    key: &str,
    ready: &Arc<AtomicBool>,
) -> Woke {
    let reads = &inbound.state.document_reads;
    let local = reads.reader_wake();
    tokio::pin!(local);
    // Enabled before looking: a change between the look and the wait wakes
    // it rather than being missed.
    local.as_mut().enable();
    if !inbound.state.is_healthy() || !reads.current(key, ready) || reads.refresh_pending(key) {
        return Woke::Local;
    }
    let request = Request::Await {
        known_revision: known,
    };
    tokio::select! {
        () = &mut local => Woke::Local,
        reply = tokio::time::timeout(AWAIT_KEEPALIVE, channel.committed_document(&request)) => {
            match reply {
                Ok(Ok(Reply::Stale)) => Woke::Stale,
                Ok(_) => Woke::Refused,
                Err(_) => Woke::Keepalive,
            }
        }
    }
}

/// Wait until the owner merged `read` ([`CommittedDocument::applied`]) or
/// refused its changes ([`CommittedDocument::refuse`]), or `live` says the
/// route is gone. The owner's component factory waits for
/// the confirming read that follows, so the merge ends this wait at once.
async fn until_merged(read: &CommittedDocument, live: impl Fn() -> bool) {
    while !read.merged.load(Ordering::Acquire) && !read.refused.load(Ordering::Acquire) && live() {
        let _ = tokio::time::timeout(MERGE_RECHECK, read.merge_wake.notified()).await;
    }
}

/// Whether a route joined as `participant_kind` asks the broker for its
/// committed document. The broker serves committed reads only to a process
/// the launcher holds custody of for that app, which an agent never is, so it
/// refuses an agent's open. An agent works through actions and task runs, not
/// its own document; asking would fail its whole connection in a native room.
/// A headless service (`CollaborationManager::for_service`) holds no document
/// either: it publishes actions and answers them, and its room never holds a
/// record of it to read.
fn opens_committed_document(participant_kind: &str) -> bool {
    !matches!(participant_kind, "agent" | super::SERVICE_PARTICIPANT)
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
pub(super) async fn install(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound: &InboundBridge,
    join: NativeReadJoin,
) -> Result<bool, String> {
    let key = spec.channel_id();
    if !opens_committed_document(join.participant_kind.as_str()) {
        inbound.state.document_reads.legacy(&key)?;
        return Ok(false);
    }
    let first = channel
        .committed_document(&Request::Open {
            known_revision: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    if first == Reply::Legacy {
        inbound.state.document_reads.legacy(&key)?;
        return Ok(false);
    }
    let mut ready = inbound.state.document_reads.pending(&key)?;
    let (initial, revision) = match first {
        Reply::Chunk { .. } | Reply::Created { .. } => {
            let (read, revision) = read_capture(channel, first)
                .await
                .map_err(|error| error.to_string())?;
            (Some(read), Some(revision))
        }
        Reply::Unavailable => (None, None),
        _ => return Err("invalid initial committed read mode".into()),
    };
    let mut channel = channel.clone();
    let spec = spec.clone();
    let inbound = inbound.clone();
    tokio::task::spawn_local(async move {
        // One renewal per healthy read: a deadline renews unless the previous
        // renewal has not yet produced a ready read. Readiness published on a
        // renewed channel restores the budget, so a runner that stalls twice
        // or a laptop that suspends twice recovers both times, while two
        // deadlines with no successful read between them still close.
        let mut renewed = false;
        let mut current = initial;
        let mut revision = revision;
        let mut last_open;
        'read: loop {
            if !inbound.state.is_healthy() || !inbound.state.document_reads.current(&key, &ready) {
                break;
            }
            // A queued native invocation retains this existing transport turn
            // through the owner's dispatch batch. A refresh cannot withdraw its
            // applied capture between admission delivery and the unchanged guard.
            let Ok(semaphore) = inbound.state.document_reads.delivery_turn(&key) else {
                break;
            };
            let Ok(turn) = semaphore.acquire_owned().await else {
                break;
            };
            if !inbound.state.is_healthy() || !inbound.state.document_reads.current(&key, &ready) {
                break;
            }
            if let Some(read) = current.take() {
                let staged = read.clone();
                if inbound
                    .state
                    .document_reads
                    .stage(&key, &ready, read.clone())
                    .is_err()
                {
                    break;
                }
                if !inbound.send_durable(CollabEvent::CommittedDocument {
                    route: spec.clone(),
                    read,
                }) {
                    break;
                }
                until_merged(&staged, || {
                    inbound.state.is_healthy() && inbound.state.document_reads.current(&key, &ready)
                })
                .await;
                if !inbound.state.is_healthy()
                    || !inbound.state.document_reads.current(&key, &ready)
                {
                    break;
                }
                inbound
                    .state
                    .document_reads
                    .clear_stage(&key, &staged.merged);
                if staged.refused.load(Ordering::Acquire) && !staged.merged.load(Ordering::Acquire)
                {
                    // The owner lacks the changes' base (or a new owner
                    // needs the whole document): read it whole.
                    revision = None;
                }
            }
            let refresh = inbound.state.document_reads.refresh_requested(&key);
            if refresh {
                set_read_ready(&inbound, &ready, false);
            }
            last_open = crate::time::now();
            let known = if refresh { None } else { revision };
            // A reader holding a revision asks for only the changes since.
            let request = known.map_or(
                Request::Open {
                    known_revision: None,
                },
                |known_revision| Request::OpenChanges { known_revision },
            );
            let result =
                tokio::time::timeout(Duration::from_secs(2), channel.committed_document(&request))
                    .await;
            let renew = match result {
                Ok(Ok(Reply::Unchanged { revision: checked })) if Some(checked) == revision => {
                    set_read_ready(&inbound, &ready, true);
                    renewed = false;
                    false
                }
                Ok(Ok(reply))
                    if matches!(reply, Reply::Chunk { .. })
                        || (known.is_some() && matches!(reply, Reply::Changes { .. })) =>
                {
                    set_read_ready(&inbound, &ready, false);
                    match read_capture(&channel, reply).await {
                        Ok((read, next)) => {
                            current = Some(read);
                            revision = Some(next);
                            continue;
                        }
                        Err(error) => {
                            #[cfg(any(test, feature = "test-support"))]
                            tracing::warn!(error = %error, route = ?spec, "committed document capture failed");
                            if matches!(error, CaptureFailure::Deadline(_)) && !renewed {
                                true
                            } else {
                                inbound
                                    .state
                                    .fail_closed("committed document capture outcome unknown");
                                break;
                            }
                        }
                    }
                }
                Ok(Ok(Reply::Unavailable)) => {
                    set_read_ready(&inbound, &ready, false);
                    revision = None;
                    false
                }
                Err(_) if !renewed => true,
                _ => {
                    #[cfg(any(test, feature = "test-support"))]
                    if let Ok(Err(error)) = &result {
                        tracing::warn!(error = %error, route = ?spec, "committed document polling RPC failed");
                    }
                    #[cfg(any(test, feature = "test-support"))]
                    tracing::warn!(
                        timed_out = result.is_err(),
                        rpc_failed = matches!(&result, Ok(Err(_))),
                        unexpected_reply = matches!(&result, Ok(Ok(_))),
                        "committed document polling stopped"
                    );
                    inbound
                        .state
                        .fail_closed("committed document read outcome unknown");
                    set_read_ready(&inbound, &ready, false);
                    break;
                }
            };
            if renew {
                // Neither an expired Open nor a timed-out capture can be
                // replayed on its old handle. Discard unacknowledged bytes,
                // fence delivery, then rejoin once on the authenticated root.
                renewed = true;
                set_read_ready(&inbound, &ready, false);
                if !inbound.state.is_healthy()
                    || !inbound.state.document_reads.current(&key, &ready)
                {
                    break;
                }
                let Ok(next_ready) = inbound.state.document_reads.pending(&key) else {
                    inbound
                        .state
                        .fail_closed("committed read generation unavailable");
                    break;
                };
                ready = next_ready;
                let mut turn = Some(turn);
                loop {
                    match join.renew(&spec, &inbound.state.document_reads).await {
                        Ok(next) => {
                            channel = next;
                            revision = None;
                            #[cfg(any(test, feature = "test-support"))]
                            tracing::warn!("original committed read generation renewed");
                            continue 'read;
                        }
                        // A slow rejoin is an unknown outcome, not a refusal.
                        // Readiness stays withheld and no capture exists in
                        // this generation; release the delivery turn and
                        // rejoin again. Every attempt is a fresh admission
                        // the broker re-verifies, so a revoked peer is still
                        // refused by the first rejoin that is answered.
                        Err(CaptureFailure::Deadline(_)) => {
                            #[cfg(any(test, feature = "test-support"))]
                            tracing::warn!("committed read renewal deadline elapsed; rejoining");
                            drop(turn.take());
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            if !inbound.state.is_healthy()
                                || !inbound.state.document_reads.current(&key, &ready)
                            {
                                break 'read;
                            }
                        }
                        Err(CaptureFailure::Rejected(_)) => {
                            inbound
                                .state
                                .fail_closed("committed read renewal unavailable");
                            break 'read;
                        }
                    }
                }
            }
            drop(turn);
            // Nothing to do until the read changes: wait for the broker to
            // say so instead of asking five times a second. However the wait
            // ends, the next Open decides, no sooner than READ_RECHECK after
            // the last.
            until_stale(&channel, revision, &inbound, &key, &ready).await;
            let since = crate::time::now().saturating_duration_since(last_open);
            tokio::time::sleep(READ_RECHECK.saturating_sub(since)).await;
        }
        set_read_ready(&inbound, &ready, false);
        if inbound.state.document_reads.current(&key, &ready) {
            inbound.state.document_reads.discard_stage(&key);
        }
    });
    Ok(true)
}
