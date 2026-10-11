//! Native committed reads stay closed until the real document owner applies them.
use super::{
    Arc, AtomicBool, ByteBudget, ByteReservation, CollabEvent, Duration, InboundBridge, Ordering,
    RouteSpec,
};
use runtime_collab::committed_document::{MAX_DOCUMENT_BYTES, Reply, Request};
use std::collections::BTreeMap;
#[derive(Clone, Default)]
pub(super) struct Reads(
    Arc<std::sync::Mutex<BTreeMap<String, Gate>>>,
    Arc<std::sync::Mutex<BTreeMap<String, CommittedDocument>>>,
    Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    Arc<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Semaphore>>>>,
    Arc<std::sync::Mutex<Option<Vec<String>>>>,
);
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
        if rows.contains_key(key) || rows.len() >= 64 {
            return false;
        }
        rows.insert(key.into(), Gate::Joining);
        true
    }
    pub(super) fn initial(&self, key: &str) -> Option<CommittedDocument> {
        if let Some(read) = self.1.lock().ok().and_then(|rows| rows.get(key).cloned()) {
            return Some(read);
        }
        if self.publishable(key) && self.required(key) {
            if let Ok(mut refresh) = self.2.lock() {
                refresh.insert(key.into());
            }
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
            .filter(|read| read.is_current() && !read.merged.load(Ordering::Acquire))
            .cloned()
    }
    fn stage(
        &self,
        key: &str,
        ready: &Arc<AtomicBool>,
        read: CommittedDocument,
    ) -> Result<(), String> {
        let gates = self.0.lock().map_err(|_| "document reads poisoned")?;
        if !matches!(gates.get(key),Some(Gate::Required(current, _)) if Arc::ptr_eq(current,ready))
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
    pub(super) fn legacy(&self, key: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "document reads poisoned")?
            .insert(key.into(), Gate::Legacy);
        Ok(())
    }
    fn pending(&self, key: &str) -> Result<Arc<AtomicBool>, String> {
        let mut rows = self.0.lock().map_err(|_| "document reads poisoned")?;
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
    valid: Arc<AtomicBool>,
    creation: Option<runtime_collab::committed_document::CreationReceipt>,
    revision: [u8; 32],
    initialized: Arc<AtomicBool>,
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
        let mutation = collab_types::DocumentMutation::Snapshot(std::mem::take(&mut *bytes));
        let result = apply(&mutation);
        if let collab_types::DocumentMutation::Snapshot(restored) = mutation {
            *bytes = restored;
        }
        Ok(result)
    }
    pub(crate) fn is_current(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }
    pub(crate) fn applied(&self) {
        self.merged.store(true, Ordering::Release);
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
}
impl Assembly {
    fn start(reply: Reply) -> Result<Self, String> {
        let Reply::Chunk {
            capture,
            revision,
            total,
            offset,
            bytes,
            token,
        } = reply
        else {
            return Err("committed document initial chunk missing".into());
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
            valid: Arc::new(AtomicBool::new(true)),
            creation: None,
            revision: [0; 32],
            initialized: Arc::new(AtomicBool::new(false)),
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

/// Only local deadlines permit retiring a capture and renewing its read epoch.
/// Broker refusals, invalid replies and authority failures remain terminal.
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
            valid: Arc::new(AtomicBool::new(true)),
            creation,
            revision: assembly.revision,
            initialized: Arc::new(AtomicBool::new(false)),
        },
        assembly.revision,
    ))
}
// A parked owner must observe both publication readiness and its withdrawal.
fn set_read_ready(inbound: &InboundBridge, ready: &AtomicBool, value: bool) {
    if ready.swap(value, Ordering::AcqRel) != value {
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
    ) -> Result<runtime_collab::ChannelHandle, String> {
        let key = spec.channel_id();
        let expected_instance = reads.private_window(&key);
        tokio::time::timeout(Duration::from_secs(2), async {
            let channel = self
                .client
                .join_channel_as_kind(
                    &key,
                    &self.presentation,
                    &self.participant_handle,
                    &self.participant_kind,
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
            Ok(channel)
        })
        .await
        .map_err(|_| "original committed read renewal deadline elapsed")?
    }
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
    let first = channel
        .committed_document(&Request::Open {
            known_revision: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    let key = spec.channel_id();
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
        let mut renewed = false;
        let mut current = initial;
        let mut revision = revision;
        loop {
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
                let merged = read.merged.clone();
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
                while !merged.load(Ordering::Acquire)
                    && inbound.state.is_healthy()
                    && inbound.state.document_reads.current(&key, &ready)
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if !inbound.state.is_healthy()
                    || !inbound.state.document_reads.current(&key, &ready)
                {
                    break;
                }
                inbound.state.document_reads.clear_stage(&key, &merged);
            }
            let refresh = inbound.state.document_reads.refresh_requested(&key);
            if refresh {
                set_read_ready(&inbound, &ready, false);
            }
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                channel.committed_document(&Request::Open {
                    known_revision: if refresh { None } else { revision },
                }),
            )
            .await;
            let renew = match result {
                Ok(Ok(Reply::Unchanged { revision: checked })) if Some(checked) == revision => {
                    set_read_ready(&inbound, &ready, true);
                    false
                }
                Ok(Ok(reply @ Reply::Chunk { .. })) => {
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
                if let Ok(next) = join.renew(&spec, &inbound.state.document_reads).await {
                    channel = next;
                    revision = None;
                    #[cfg(any(test, feature = "test-support"))]
                    tracing::warn!("original committed read generation renewed");
                    continue;
                }
                inbound
                    .state
                    .fail_closed("committed read renewal unavailable");
                break;
            }
            drop(turn);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        set_read_ready(&inbound, &ready, false);
        if inbound.state.document_reads.current(&key, &ready) {
            inbound.state.document_reads.discard_stage(&key);
        }
    });
    Ok(true)
}
