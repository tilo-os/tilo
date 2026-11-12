//! Native committed reads stay closed until the real document owner applies them.
use super::{
    Arc, AtomicBool, ByteBudget, ByteReservation, CollabEvent, Duration, InboundBridge, Ordering,
    RouteSpec,
};
use runtime_collab::committed_document::{MAX_DOCUMENT_BYTES, Reply, Request};
use std::collections::BTreeMap;
use tilo_lock::MutexRecover as _;
use tilo_sort::CompactSort as _;
#[derive(Clone, Default)]
pub(super) struct Reads(
    Arc<std::sync::Mutex<BTreeMap<String, Gate>>>,
    Arc<std::sync::Mutex<BTreeMap<String, CommittedDocument>>>,
    Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    Arc<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Semaphore>>>>,
    Arc<std::sync::Mutex<Option<DocumentOwners>>>,
    Arc<Progress>,
);
/// The App's live document owner routes and the document generation they
/// belong to. Reopening or replacing the document starts a new generation.
#[derive(Clone)]
pub(super) struct DocumentOwners {
    routes: Vec<String>,
    generation: u64,
}
/// The document owner an accepted invocation arrived for. It may run only
/// while that exact owner is still current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeliveryOwner {
    generation: Option<u64>,
    route: Option<String>,
}
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
    /// The route of a chain's head (`RecordId::chain_head`), which names whichever
    /// part of the chain is the newest and holds nothing of its own: it was asked
    /// for, and no document is ever read or staged here. The part arrives on its
    /// own route.
    ChainHead,
    Required(Arc<AtomicBool>, Arc<std::sync::atomic::AtomicU64>),
    /// The route's join or its first read failed: no read will be staged
    /// for it. The next read of the route joins it again.
    Failed(String),
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
        {
            let rows = self.0.lock_recover();
            for gate in rows.values() {
                if let Gate::Required(ready, _) = gate {
                    ready.store(false, Ordering::Release);
                }
            }
        }
        {
            let mut rows = self.1.lock_recover();
            for read in rows.values() {
                read.valid.store(false, Ordering::Release);
            }
            rows.clear();
        }
        {
            let mut requests = self.2.lock_recover();
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
        let rows = self.0.lock_recover();
        let current = !self.closed() && rows.values().any(|gate| {
            matches!(gate, Gate::Required(candidate, _) if std::ptr::eq(candidate.as_ref(), ready))
        });
        ready.swap(current, Ordering::AcqRel) != current
    }
    /// How many readiness changes happened so far; pass it to
    /// [`Self::wait_progress`] after checking the state it guards.
    pub(super) fn progress(&self) -> u64 {
        *self.5.changes.lock_recover()
    }
    /// Wait until a readiness change after `seen`, or `bound` elapses.
    /// Returns whether the change came.
    pub(super) fn wait_progress(&self, seen: u64, bound: Duration) -> bool {
        let changes = self.5.changes.lock_recover();
        let (changes, waited) = self
            .5
            .changed
            .wait_timeout_while(changes, bound, |changes| *changes == seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(changes);
        !waited.timed_out()
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
    pub(super) fn reader_wake(&self) -> tokio::sync::futures::Notified<'_> {
        self.5.reader.notified()
    }
    /// Whether the owner asked for a fresh read, without consuming the ask.
    fn refresh_pending(&self, key: &str) -> bool {
        self.2.lock_recover().contains(key)
    }
    pub(super) fn readiness_diagnostic(&self, key: &str) -> Option<ReadinessDiagnostic> {
        let required = self
            .0
            .lock_recover()
            .get(key)
            .is_some_and(|gate| matches!(gate, Gate::Required(_, _)));
        let staged = self
            .1
            .lock_recover()
            .get(key)
            .map(|read| read.merged.load(Ordering::Acquire));
        let refresh_requested = self.2.lock_recover().contains(key);
        Some(ReadinessDiagnostic {
            required,
            staged: staged.is_some(),
            applied: staged.unwrap_or(false),
            refresh_requested,
        })
    }
    pub(super) fn delivery_generation(&self, key: &str) -> Option<Arc<AtomicBool>> {
        let rows = self.0.lock_recover();
        match rows.get(key)? {
            Gate::Required(ready, _) => Some(ready.clone()),
            _ => None,
        }
    }
    pub(super) fn delivery_turn(&self, key: &str) -> Result<Arc<tokio::sync::Semaphore>, String> {
        let mut turns = self.3.lock_recover();
        Ok(turns
            .entry(key.into())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone())
    }
    pub(super) fn set_document_owner_routes(&self, mut keys: Vec<String>, generation: u64) {
        keys.compact_sort();
        keys.dedup();
        {
            let mut owners = self.4.lock_recover();
            *owners = Some(DocumentOwners {
                routes: keys,
                generation,
            });
        }
        self.progressed();
    }
    pub(super) fn document_owner_routes(&self, original: &str) -> Vec<String> {
        self.4
            .lock_recover()
            .as_ref()
            .map_or_else(|| vec![original.into()], |owners| owners.routes.clone())
    }
    /// The owner an invocation arriving on `key` now belongs to.
    pub(super) fn delivery_owner(&self, key: &str) -> DeliveryOwner {
        let owners = self.4.lock_recover();
        DeliveryOwner {
            generation: owners.as_ref().map(|owners| owners.generation),
            route: owners
                .as_ref()
                .filter(|owners| owners.routes.iter().any(|route| route == key))
                .map(|_| key.into()),
        }
    }
    /// Whether `owner` is still the current document owner: same document
    /// generation, and its route is still one of the live owner routes.
    pub(super) fn owns(&self, owner: &DeliveryOwner) -> bool {
        let owners = self.4.lock_recover();
        owners.as_ref().map(|owners| owners.generation) == owner.generation
            && owner.route.as_ref().is_none_or(|key| {
                owners
                    .as_ref()
                    .is_some_and(|owners| owners.routes.iter().any(|route| route == key))
            })
    }
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn close_for_test(&self, key: &str) -> Arc<AtomicBool> {
        self.pending(key).unwrap()
    }
    /// Open a route [`Self::close_for_test`] closed, as its applied read does:
    /// whatever waits for it hears it.
    #[cfg(test)]
    pub(crate) fn open_for_test(&self, ready: &AtomicBool) {
        ready.store(true, Ordering::Release);
        self.progressed();
    }
    #[cfg(test)]
    pub(crate) fn legacy_for_test(&self, key: &str) {
        self.legacy(key).unwrap();
    }
    pub(super) fn publishable(&self, key: &str) -> bool {
        if self.closed() {
            return false;
        }
        self.0
            .lock_recover()
            .get(key)
            .cloned()
            .is_some_and(|gate| match gate {
                Gate::Joining | Gate::Failed(_) | Gate::ChainHead => false,
                Gate::Legacy => true,
                Gate::Required(ready, _) => ready.load(Ordering::Acquire),
            })
    }
    pub(super) fn required(&self, key: &str) -> bool {
        self.0
            .lock_recover()
            .get(key)
            .is_some_and(|gate| matches!(gate, Gate::Required(_, _)))
    }
    pub(super) fn bind_private_window(
        &self,
        key: &str,
        instance: Option<u64>,
    ) -> Result<(), String> {
        let rows = self.0.lock_recover();
        let Some(Gate::Required(_, bound)) = rows.get(key) else {
            return Err("private binding requires admitted native document custody".into());
        };
        bound.store(instance.unwrap_or(0), Ordering::Release);
        Ok(())
    }
    pub(super) fn private_window(&self, key: &str) -> Option<u64> {
        let rows = self.0.lock_recover();
        let Gate::Required(_, instance) = rows.get(key)? else {
            return None;
        };
        let instance = instance.load(Ordering::Acquire);
        (instance != 0).then_some(instance)
    }

    /// Whether the route of `key` has not resolved how it opens yet: no
    /// gate, or its join still on its way.
    pub(super) fn resolving(&self, key: &str) -> bool {
        !self.closed() && matches!(self.0.lock_recover().get(key), None | Some(Gate::Joining))
    }

    pub(super) fn joining(&self, key: &str) -> bool {
        let mut rows = self.0.lock_recover();
        let rejoin = matches!(rows.get(key), Some(Gate::Failed(_)));
        if self.closed() || (rows.contains_key(key) && !rejoin) || (!rejoin && rows.len() >= 64) {
            return false;
        }
        rows.insert(key.into(), Gate::Joining);
        true
    }
    /// The route of `key` could not be joined, or its first read failed,
    /// with `error`: a reader waiting for its read is told so at once.
    pub(super) fn join_failed(&self, key: &str, error: String) {
        let mut rows = self.0.lock_recover();
        match rows.get(key) {
            Some(Gate::Joining) => {}
            Some(Gate::Required(ready, _)) => ready.store(false, Ordering::Release),
            _ => return,
        }
        rows.insert(key.into(), Gate::Failed(error));
        drop(rows);
        self.progressed();
    }
    /// Why no read of `key` is coming: its join or first read failed, or it
    /// was never let join (the reader is closed or holds as many routes as
    /// it may). `None` while its read is on its way or here.
    pub(super) fn unreadable(&self, key: &str) -> Option<String> {
        if self.closed() {
            return Some("committed document reader closed".into());
        }
        let rows = self.0.lock_recover();
        match rows.get(key) {
            Some(Gate::Failed(error)) => Some(error.clone()),
            Some(Gate::ChainHead) => {
                Some("a chain's head names its newest part and holds no document".into())
            }
            None => Some("committed document route capacity".into()),
            Some(_) => None,
        }
    }
    /// Whether the broker answered `key`'s read as served by the app's own
    /// channel ([`Self::legacy`]): no committed read will be staged for it.
    pub(super) fn answered_by_own_channel(&self, key: &str) -> bool {
        self.0
            .lock_recover()
            .get(key)
            .is_some_and(|gate| matches!(gate, Gate::Legacy))
    }
    pub(super) fn initial(&self, key: &str) -> Option<CommittedDocument> {
        // Only a whole document can seed an owner; staged changes apply
        // only to the owner that holds their base.
        if let Some(read) = self.1.lock_recover().get(key).cloned() {
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
            self.2.lock_recover().insert(key.into());
            self.5.reader.notify_waiters();
        }
        None
    }
    /// The current staged capture still awaiting the canonical owner's merge.
    /// Unlike `initial`, observing this handoff does not request a refresh.
    pub(super) fn unapplied(&self, key: &str) -> Option<CommittedDocument> {
        let gates = self.0.lock_recover();
        if !matches!(gates.get(key), Some(Gate::Required(_, _))) {
            return None;
        }
        self.1
            .lock_recover()
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
        let gates = self.0.lock_recover();
        if self.closed()
            || !matches!(gates.get(key),Some(Gate::Required(current, _)) if Arc::ptr_eq(current,ready))
        {
            return Err("committed route replaced".into());
        }
        let mut rows = self.1.lock_recover();
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
        let mut rows = self.1.lock_recover();
        if rows
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
        self.0.lock_recover().get(key).is_some_and(
            |gate| matches!(gate, Gate::Required(current, _) if Arc::ptr_eq(current, ready)),
        )
    }
    fn discard_stage(&self, key: &str) {
        {
            let mut rows = self.1.lock_recover();
            if let Some(read) = rows.remove(key) {
                read.valid.store(false, Ordering::Release);
            }
        }
        self.2.lock_recover().remove(key);
    }
    fn refresh_requested(&self, key: &str) -> bool {
        self.2.lock_recover().remove(key)
    }
    fn legacy(&self, key: &str) -> Result<(), String> {
        let mut rows = self.0.lock_recover();
        if self.closed() {
            return Err("committed document reader closed".into());
        }
        rows.insert(key.into(), Gate::Legacy);
        drop(rows);
        self.progressed();
        Ok(())
    }
    /// The route of `key` is a chain's head, asked for and answered: it names the
    /// chain's newest part and holds nothing of its own, so no document is read or
    /// staged for it.
    fn chain_head(&self, key: &str) -> Result<(), String> {
        let mut rows = self.0.lock_recover();
        if self.closed() {
            return Err("committed document reader closed".into());
        }
        rows.insert(key.into(), Gate::ChainHead);
        drop(rows);
        self.progressed();
        Ok(())
    }
    fn pending(&self, key: &str) -> Result<Arc<AtomicBool>, String> {
        let mut rows = self.0.lock_recover();
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
        drop(rows);
        // Whatever waits on the generation this one replaces hears that the
        // read it waited for is no longer this route's.
        self.progressed();
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
            .field("bytes", &self.data.bytes.lock_recover().len())
            .finish_non_exhaustive()
    }
}
impl CommittedDocument {
    pub fn with_mutation<T>(
        &self,
        apply: impl FnOnce(&collab_types::DocumentMutation) -> T,
    ) -> Result<T, String> {
        if !self.is_current() {
            return Err("committed capture route replaced".into());
        }
        let mut bytes = self.data.bytes.lock_recover();
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
    pub fn applies_to(&self, document: &impl automerge::ReadDoc) -> bool {
        self.base
            .as_ref()
            .is_none_or(|base| document.get_missing_deps(base).is_empty())
    }
    /// The owner cannot read this capture: the reader stops waiting on it.
    /// The changes since a revision are dropped and the document is read
    /// whole instead; a whole document leaves nothing further to read, so the
    /// reader says so once and goes on from that revision, the owner keeping
    /// its own document.
    pub fn refuse(&self) {
        self.refused.store(true, Ordering::Release);
        // A permit is kept when the reader is not waiting yet, so a refusal
        // between its check and its wait is never missed.
        self.merge_wake.notify_one();
    }
    /// Whether this is the whole document, rather than the changes since a
    /// revision: the only read an owner that cannot apply it has left.
    pub fn is_whole(&self) -> bool {
        self.base.is_none()
    }
    /// Whether the owner refused this capture ([`Self::refuse`]).
    pub fn was_refused(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }
    pub fn is_current(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
    pub fn applied(&self) {
        self.merged.store(true, Ordering::Release);
        // A permit is kept when the reader is not waiting yet, so a merge
        // between its check and its wait is never missed.
        self.merge_wake.notify_one();
    }
    /// Take this capture into `document`, the owner's copy of the route's
    /// record, through `merge`, the owner's own receive path, which merges
    /// it with whatever the owner made meanwhile: acknowledged once merged
    /// ([`Self::applied`]), so the reader confirms it and the route opens
    /// again. Changes the owner holds no base of, or cannot read, are
    /// refused ([`Self::refuse`]) and said why: the reader reads the
    /// document whole instead. A whole document it cannot read is refused
    /// the same way, and the reader says so once and moves on: the owner
    /// keeps its own document and the route goes on publishing it. `None`
    /// when nothing was merged.
    pub fn take_into<D: automerge::ReadDoc, T, E: std::fmt::Display>(
        &self,
        document: &mut D,
        merge: impl FnOnce(&mut D, &collab_types::DocumentMutation) -> Result<T, E>,
    ) -> Option<T> {
        if !self.is_current() {
            return None;
        }
        if !self.applies_to(document) {
            tracing::warn!("committed changes refused: the owner holds none of their base");
            self.refuse();
            return None;
        }
        match self.with_mutation(|mutation| merge(document, mutation)) {
            Ok(Ok(merged)) => {
                self.applied();
                Some(merged)
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, whole = self.is_whole(), "committed history refused: the owner cannot read it");
                self.refuse();
                None
            }
            // ignored-failure: the route's read was replaced; its reader no longer waits on this one
            Err(_) => None,
        }
    }
    /// A current capture of `bytes`: the changes since `base`, or the whole
    /// document when `base` is `None`.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn for_test(bytes: Vec<u8>, base: Option<Vec<automerge::ChangeHash>>) -> Self {
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
            base: base.map(Into::into),
            refused: Arc::new(AtomicBool::new(false)),
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn was_applied(&self) -> bool {
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
        let reads = Reads::default();
        let read = one_chunk_capture(&budget).await;
        let started = tokio::time::Instant::now();
        let owner = {
            let read = read.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                read.applied();
            })
        };
        until_merged(&read, &reads, || true).await;
        owner.await.unwrap();
        // The clock is paused: a wait that slept on a clock instead of on
        // the owner's answer would have advanced it.
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
    #[tokio::test(start_paused = true)]
    async fn a_merge_before_the_reader_waits_is_not_missed() {
        let budget = ByteBudget::new();
        let reads = Reads::default();
        let read = one_chunk_capture(&budget).await;
        read.applied();
        let started = tokio::time::Instant::now();
        until_merged(&read, &reads, || true).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
    /// A capture the app never takes is waited for, not asked about: the
    /// reader sleeps until something here changes, and hears a route
    /// replaced under it without any clock.
    #[tokio::test(start_paused = true)]
    async fn an_unmerged_capture_waits_for_its_route_to_go_without_a_clock() {
        let budget = ByteBudget::new();
        let reads = Reads::default();
        let read = one_chunk_capture(&budget).await;
        let ready = reads.pending("route").unwrap();
        let started = tokio::time::Instant::now();
        let route_goes = {
            let reads = reads.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                // A new generation: the capture is no longer this route's.
                reads.pending("route").unwrap();
            })
        };
        until_merged(&read, &reads, || reads.current("route", &ready)).await;
        route_goes.await.unwrap();
        assert!(!read.merged.load(Ordering::Acquire));
        assert!(!reads.current("route", &ready));
        // The pause makes a timer wait show as elapsed time; this wait had
        // none to spend.
        assert_eq!(started.elapsed(), Duration::ZERO);
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
            std::thread::spawn(move || reads.wait_progress(seen, tilo_test_env::hang_bound()))
        };
        reads.stage("route", &ready, read).unwrap();
        assert!(waiter.join().unwrap(), "staging woke the waiting owner");
        assert!(reads.initial("route").is_some());
    }
    #[test]
    fn a_wait_after_the_change_it_watches_for_returns_at_once() {
        let reads = Reads::default();
        let seen = reads.progress();
        reads.legacy("route").unwrap();
        assert!(reads.wait_progress(seen, tilo_test_env::hang_bound()));
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
                tokio::time::sleep(Duration::from_millis(900)).await;
                Ok(Reply::Cancelled)
            },
        )
        .await
        .expect("a complete capture survives a slow but answered acknowledgement");
        assert_eq!(revision, [5; 32]);
        assert!(!read.merged.load(Ordering::Acquire));
    }

    /// A route's join reads its first capture for as long as the broker
    /// takes: a minute on this paused clock, past every reader deadline,
    /// and the capture still arrives.
    #[tokio::test(start_paused = true)]
    async fn a_joins_first_capture_waits_for_a_late_broker() {
        let (read, revision) = join_capture_using(
            Reply::Chunk {
                capture: 1,
                revision: [5; 32],
                total: 2,
                offset: 0,
                bytes: vec![1],
                token: [6; 16],
            },
            &ByteBudget::new(),
            async |request| {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(match request {
                    Request::Next { capture: 1, .. } => Reply::Chunk {
                        capture: 1,
                        revision: [5; 32],
                        total: 2,
                        offset: 1,
                        bytes: vec![2],
                        token: [6; 16],
                    },
                    Request::Cancel { capture: 1, .. } => Reply::Cancelled,
                    other => panic!("unexpected request {other:?}"),
                })
            },
        )
        .await
        .expect("a join's capture waits for the broker's answers");
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
        let read = CommittedDocument::for_test(vec![1], None);
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

/// The bytes of committed reads this process holds at once, beyond a single
/// capture larger than that.
const CAPTURE_LIMIT: usize = MAX_DOCUMENT_BYTES * 2;
fn capture_budget() -> &'static ByteBudget {
    static BUDGET: std::sync::OnceLock<ByteBudget> = std::sync::OnceLock::new();
    BUDGET.get_or_init(ByteBudget::new)
}
/// How long an assembled capture waits for the broker to accept its
/// acknowledgement; equal to the polling Open deadline.
const ACKNOWLEDGEMENT_DEADLINE: Duration = Duration::from_secs(2);

/// Who waits for a capture, and so whether a clock may end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureWait {
    /// The route's join reads its first capture: the join waits for the
    /// broker's answers, however long they take, and fails only with the
    /// broker's reason or the connection's end. There is nothing to renew
    /// yet: a join that fails is joined again by the next read.
    Join,
    /// A joined route's reader re-reads a changed record: a deadline
    /// retires the capture and renews the read on the original root.
    Renewing,
}

impl CaptureWait {
    /// `future`, ended by `bound` with `deadline` only for a renewing reader.
    async fn within<T>(
        self,
        bound: Duration,
        deadline: &'static str,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, CaptureFailure> {
        match self {
            Self::Join => Ok(future.await),
            Self::Renewing => tokio::time::timeout(bound, future)
                .await
                .map_err(|_| CaptureFailure::Deadline(deadline)),
        }
    }
}

/// Only local deadlines permit retiring a capture and renewing its read epoch,
/// or retrying a renewal, and only a full local budget makes a read wait and
/// Open again. Broker refusals, invalid replies and authority failures remain
/// terminal.
#[derive(Debug)]
enum CaptureFailure {
    Deadline(&'static str),
    Rejected(String),
    /// This process's committed read budget had no room for a capture of
    /// this many bytes. The broker's capture was cancelled; the read waits
    /// for workspace ([`ByteBudget::until_capacity`]) and Opens again.
    Capacity(usize),
}
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline(message) => formatter.write_str(message),
            Self::Rejected(message) => formatter.write_str(message),
            Self::Capacity(_) => formatter.write_str("SDK committed read capacity"),
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
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn read_capture(
    channel: &runtime_collab::ChannelHandle,
    first: Reply,
    wait: CaptureWait,
) -> Result<(CommittedDocument, [u8; 32]), CaptureFailure> {
    capture_using(first, capture_budget(), wait, async |request| {
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
    capture_using(first, budget, CaptureWait::Renewing, send)
        .await
        .map_err(|error| error.to_string())
}
/// [`read_capture_using`] as a route's join reads its first capture.
#[cfg(test)]
pub(super) async fn join_capture_using(
    first: Reply,
    budget: &ByteBudget,
    send: impl AsyncFnMut(Request) -> Result<Reply, String>,
) -> Result<(CommittedDocument, [u8; 32]), String> {
    capture_using(first, budget, CaptureWait::Join, send)
        .await
        .map_err(|error| error.to_string())
}
async fn capture_using(
    first: Reply,
    budget: &ByteBudget,
    wait: CaptureWait,
    mut send: impl AsyncFnMut(Request) -> Result<Reply, String>,
) -> Result<(CommittedDocument, [u8; 32]), CaptureFailure> {
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
            return Err(CaptureFailure::from("committed document initial bounds"));
        }
        // Charge the validated raw payload before allocating its destination.
        // Clones share this one lease; the aggregate limit is unchanged. A
        // capture alone in the budget always fits, so waiting for room ends.
        let reservation = budget
            .reserve_sole(total, CAPTURE_LIMIT)
            .ok_or(CaptureFailure::Capacity(total))?;
        Ok::<_, CaptureFailure>((reservation, Assembly::start(first)?))
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
            return Err(error);
        }
    };
    let id = assembly.id;
    let result = wait
        .within(Duration::from_secs(3), "committed read timed out", async {
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
    // slowly, which permanently disables the route (#4001). A join waits
    // for it as for every other answer.
    let acknowledgement = wait
        .within(
            ACKNOWLEDGEMENT_DEADLINE,
            "committed read acknowledgement timed out",
            send(Request::Cancel {
                capture: id,
                token: assembly.token,
            }),
        )
        .await??;
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
            base: assembly
                .base
                .map(|base| base.into_iter().map(automerge::ChangeHash).collect()),
            refused: Arc::new(AtomicBool::new(false)),
        },
        assembly.revision,
    ))
}
/// Ask the workspace, through the broker, for a chain's head (`RecordId::chain_head`): a name for
/// whichever part of the chain is the newest, which holds nothing of its own and so is neither read
/// nor followed here. The host sends that part on the part's own route, and the app holds it from
/// there. Answered with nothing, the chain has no part at all, which the app is told
/// ([`CollabEvent::RecordUnavailable`]) so that it starts the first.
pub(super) async fn ask_chain_head(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound: &InboundBridge,
) -> Result<(), String> {
    inbound
        .state
        .document_reads
        .chain_head(&spec.channel_id())?;
    loop {
        match channel
            .committed_document(&Request::Open {
                known_revision: None,
            })
            .await
        {
            // The broker's byte budget is held by other reads: asking again shortly asks
            // for no bytes.
            Ok(Reply::Busy) => tokio::time::sleep(READ_RECHECK).await,
            // The workspace has no part of this chain, or nothing here holds a document of
            // the head at all: the chain starts its first part.
            Ok(Reply::Absent | Reply::Legacy) => {
                if !inbound
                    .send_durable(CollabEvent::RecordUnavailable {
                        route: spec.clone(),
                    })
                    .await
                {
                    return Err("committed read event queue closed".into());
                }
                return Ok(());
            }
            // The newest part, or the host's word about it, is on its way: the part arrives
            // on its own route, and the chain follows it from there.
            Ok(_) => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", crate::ROUTE_JOIN_FAILED)),
        }
    }
}
/// The route of `key` is served by the app's own channel: it is open for
/// documents now, and the owner hears so, as it does a committed read's
/// readiness ([`set_read_ready`]).
fn open_on_own_channel(inbound: &InboundBridge, key: &str) -> Result<(), String> {
    inbound.state.document_reads.legacy(key)?;
    inbound.state.notify_events();
    Ok(())
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

/// The shortest gap between two Opens of one route that something on this
/// side asked for (the owner wanting a fresh read, a route replaced), however
/// often that happens. A change the broker hands over is read at once.
const READ_RECHECK: Duration = Duration::from_millis(200);

/// What ended a reader's wait for its committed read to change.
enum Woke {
    /// The broker's answer: what an Open of the route answers now that it
    /// differs from what the reader holds (a capture of the change, the
    /// record becoming readable).
    Read(Reply),
    /// Something on this side changed (the route closed or was replaced,
    /// or the owner asked for a fresh read), or the wait failed: the next
    /// Open decides, and meets any refusal itself.
    Local,
}

/// Wait, without traffic and without a timer, until an Open of this route
/// would answer differently from the `known` revision the reader holds
/// (`None` after `Unavailable`), and take that answer; or until something on
/// this side changes. The caller handles the answer under its delivery turn,
/// as it does an Open's: only then is readiness published or a capture read.
///
/// No clock ends the wait. An owner that stops answering matters only to a
/// reader that asks it something, and every ask has its deadline. A read
/// nobody asks about arms no timer, so an idle app's shovel sleeps. A wait
/// the broker refuses (one that predates it) leaves the next Open to decide,
/// as a wake from this side does.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn until_changed(
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
        reply = channel.committed_document(&request) => match reply {
            Ok(reply) => Woke::Read(reply),
            // ignored-failure: a refused wait (a broker that predates it) leaves the next Open to decide, and that Open meets any refusal itself
            Err(_) => Woke::Local,
        },
    }
}

/// Wait until the owner merged `read` ([`CommittedDocument::applied`]) or
/// refused its changes ([`CommittedDocument::refuse`]), or something here
/// changed instead: `live` says the route is gone, or `reads` says the read
/// is no longer this reader's. The owner's component factory waits for
/// the confirming read that follows, so the merge ends this wait at once.
///
/// Both are waits on a source, never a timer: an app that takes its time
/// over a capture costs the shovel no wake at all while it does.
async fn until_merged(read: &CommittedDocument, reads: &Reads, live: impl Fn() -> bool) {
    loop {
        // Enabled before the checks, so a change here in between is kept.
        let local = reads.reader_wake();
        tokio::pin!(local);
        local.as_mut().enable();
        if read.merged.load(Ordering::Acquire) || read.refused.load(Ordering::Acquire) || !live() {
            return;
        }
        tokio::select! {
            () = read.merge_wake.notified() => {}
            () = &mut local => {}
        }
    }
}

/// Whether a route joined as `participant_kind` asks the broker for its
/// committed document. The broker serves committed reads only to a process
/// the launcher holds custody of for that app, which an agent never is, so it
/// refuses an agent's open. An agent works through actions and task runs, not
/// its own document; asking would fail its whole connection in a native workspace.
/// A headless service (`CollaborationManager::for_service`) holds no document
/// either: it publishes actions and answers them, and its workspace never holds a
/// record of it to read.
fn opens_committed_document(participant_kind: &str) -> bool {
    !matches!(participant_kind, "agent" | super::SERVICE_PARTICIPANT)
}

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn install(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound: &InboundBridge,
    join: NativeReadJoin,
) -> Result<bool, String> {
    let key = spec.channel_id();
    if !opens_committed_document(join.participant_kind.as_str()) {
        open_on_own_channel(inbound, &key)?;
        return Ok(false);
    }
    let first = channel
        .committed_document(&Request::Open {
            known_revision: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    if first == Reply::Legacy {
        open_on_own_channel(inbound, &key)?;
        return Ok(false);
    }
    let mut ready = inbound.state.document_reads.pending(&key)?;
    // Resolved: the route opens once its history is read and merged. An
    // owner holding changes for it stops treating it as joining.
    inbound.state.notify_events();
    // What the first read waits for before it Opens again, when it could
    // not be read yet: its turn in the broker's queue, or workspace here.
    let mut waiting = None;
    // The workspace's hosts have none of this on-demand record: the app shows it
    // unavailable, said once each time it becomes so.
    let mut absent = false;
    let (initial, revision) = match first {
        Reply::Chunk { .. } => match read_capture(channel, first, CaptureWait::Join).await {
            Ok((read, revision)) => (Some(read), Some(revision)),
            Err(CaptureFailure::Capacity(bytes)) => {
                waiting = Some(Waiting::Capacity(bytes));
                (None, None)
            }
            Err(error) => return Err(error.to_string()),
        },
        Reply::Unavailable => (None, None),
        Reply::Busy => {
            waiting = Some(Waiting::Turn);
            (None, None)
        }
        Reply::Absent => {
            absent = true;
            if !inbound
                .send_durable(CollabEvent::RecordUnavailable {
                    route: spec.clone(),
                })
                .await
            {
                return Err("committed read event queue closed".into());
            }
            (None, None)
        }
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
        let mut last_open = crate::time::now();
        // Whether the broker may hold this channel's place in its byte
        // budget's queue: after `Busy`, or an Open whose answer never came.
        let mut queued = matches!(waiting, Some(Waiting::Turn));
        // The broker's answer to the last wait, which stands for the next
        // Open: the change it handed over.
        let mut answered = None;
        match waiting {
            Some(Waiting::Turn) => {
                if let Woke::Read(reply) =
                    until_changed(&channel, None, &inbound, &key, &ready).await
                {
                    answered = Some(reply);
                }
            }
            Some(Waiting::Capacity(bytes)) => {
                capture_budget().until_capacity(bytes, CAPTURE_LIMIT).await
            }
            None => {}
        }
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
                if !inbound
                    .send_durable(CollabEvent::CommittedDocument {
                        route: spec.clone(),
                        read,
                    })
                    .await
                {
                    break;
                }
                until_merged(&staged, &inbound.state.document_reads, || {
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
                    if staged.is_whole() {
                        // The owner cannot read what the workspace holds of
                        // this record. Handing the same bytes over again
                        // cannot end differently, so the reader keeps the
                        // revision it read and goes on from there: the route
                        // publishes what the owner does hold again, later
                        // changes still reach it, and the capture is said to
                        // be unreadable here: once, for this one.
                        tracing::warn!(
                            route = %key,
                            "committed history could not be read; the owner keeps its own record"
                        );
                        set_read_ready(&inbound, &ready, true);
                    } else {
                        // The owner lacks the changes' base (or a new owner
                        // needs the whole document): read it whole.
                        revision = None;
                    }
                }
            }
            let refresh = inbound.state.document_reads.refresh_requested(&key);
            if refresh {
                set_read_ready(&inbound, &ready, false);
            }
            let known = if refresh { None } else { revision };
            // What the wait was answered with is this Open's answer, unless
            // the owner wants the whole document: then the broker drops the
            // capture this reader never reads.
            let result = if let Some(reply) = answered.take().filter(|_| !refresh) {
                Ok(Ok(reply))
            } else {
                last_open = crate::time::now();
                // A reader holding a revision asks for only the changes since.
                let request = known.map_or(
                    Request::Open {
                        known_revision: None,
                    },
                    |known_revision| Request::OpenChanges { known_revision },
                );
                tokio::time::timeout(Duration::from_secs(2), channel.committed_document(&request))
                    .await
            };
            queued = matches!(result, Ok(Ok(Reply::Busy)) | Err(_));
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
                    absent = false;
                    match read_capture(&channel, reply, CaptureWait::Renewing).await {
                        Ok((read, next)) => {
                            current = Some(read);
                            revision = Some(next);
                            continue;
                        }
                        Err(CaptureFailure::Capacity(bytes)) => {
                            // Load, not a failure: wait for this process
                            // to release committed bytes, then Open again.
                            drop(turn);
                            capture_budget().until_capacity(bytes, CAPTURE_LIMIT).await;
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
                    absent = false;
                    false
                }
                Ok(Ok(Reply::Absent)) => {
                    set_read_ready(&inbound, &ready, false);
                    revision = None;
                    if !std::mem::replace(&mut absent, true)
                        && !inbound
                            .send_durable(CollabEvent::RecordUnavailable {
                                route: spec.clone(),
                            })
                            .await
                    {
                        break;
                    }
                    false
                }
                // The broker's byte budget is held by other reads. This read
                // keeps its place in the queue and what it holds; the Await
                // below answers once its turn has come.
                Ok(Ok(Reply::Busy)) => {
                    set_read_ready(&inbound, &ready, false);
                    false
                }
                Err(_) if !renewed => true,
                _ => {
                    #[cfg(any(test, feature = "test-support"))]
                    tracing::warn!(route = ?spec, "committed document polling stopped");
                    // Collaboration ends here, so say why in every build: the
                    // broker's refusal, a second deadline, or a reply that
                    // fits no read this channel started.
                    match &result {
                        Ok(Err(error)) => tracing::warn!(
                            error = %error,
                            "committed document read refused; collaboration stops"
                        ),
                        Ok(Ok(reply)) => tracing::warn!(
                            reply = reply_kind(reply),
                            reader_holds_revision = known.is_some(),
                            "committed document read answered out of turn; collaboration stops"
                        ),
                        Err(_) => tracing::warn!(
                            "committed document read missed its deadline again; collaboration stops"
                        ),
                    }
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
                if std::mem::take(&mut queued) {
                    leave_queue(&channel);
                }
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
                        Err(CaptureFailure::Rejected(_) | CaptureFailure::Capacity(_)) => {
                            inbound
                                .state
                                .fail_closed("committed read renewal unavailable");
                            break 'read;
                        }
                    }
                }
            }
            drop(turn);
            // Nothing to do until the read changes: the broker hands the
            // change over with its answer. A wake from this side Opens
            // again, no sooner than READ_RECHECK after the last Open.
            match until_changed(&channel, revision, &inbound, &key, &ready).await {
                Woke::Read(reply) => answered = Some(reply),
                Woke::Local => {
                    let since = crate::time::now().saturating_duration_since(last_open);
                    tokio::time::sleep(READ_RECHECK.saturating_sub(since)).await;
                }
            }
        }
        set_read_ready(&inbound, &ready, false);
        if inbound.state.document_reads.current(&key, &ready) {
            inbound.state.document_reads.discard_stage(&key);
        }
        if queued {
            leave_queue(&channel);
        }
    });
    Ok(true)
}

/// What a reader waits for before it Opens again, when its read could not
/// be taken yet. Neither is a refusal.
enum Waiting {
    /// The broker answered `Busy`: its turn in the byte budget's queue,
    /// which the channel's `Await` announces.
    Turn,
    /// This process had no room for a capture of this many bytes.
    Capacity(usize),
}

/// Give up `channel`'s place in the broker's byte budget queue: this reader
/// reads no more on it (it renewed onto another channel, or stopped), and
/// reads behind it must not wait for a turn it would never take. The
/// channel itself lives on with its route.
fn leave_queue(channel: &runtime_collab::ChannelHandle) {
    let channel = channel.clone();
    tokio::task::spawn_local(async move {
        let _ = channel.committed_document(&Request::Leave).await;
    });
}

/// The variant of `reply`, for a log line that must not carry its bytes.
fn reply_kind(reply: &Reply) -> &'static str {
    match reply {
        Reply::Legacy => "legacy",
        Reply::Unavailable => "unavailable",
        Reply::Unchanged { .. } => "unchanged from another revision",
        Reply::Chunk { .. } => "chunk",
        Reply::Cancelled => "cancelled",
        Reply::RetiredCreated => "created",
        Reply::RetiredInitialized => "initialized",
        Reply::PrivateWindowBinding { .. } => "private window binding",
        Reply::RetiredStale => "retired stale",
        Reply::Changes { .. } => "changes",
        Reply::ConnectionSnapshot(_) => "connection snapshot",
        Reply::Busy => "busy",
        Reply::Absent => "absent",
        Reply::Writes { .. } => "writes",
    }
}
