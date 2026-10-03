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
);
#[derive(Clone)]
enum Gate {
    Joining,
    Legacy,
    Required(Arc<AtomicBool>),
}
impl Reads {
    #[cfg(test)]
    pub(super) fn close_for_test(&self, key: &str) -> Arc<AtomicBool> {
        self.pending(key).unwrap()
    }
    pub(super) fn publishable(&self, key: &str) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|rows| rows.get(key).cloned())
            .is_some_and(|gate| match gate {
                Gate::Joining => false,
                Gate::Legacy => true,
                Gate::Required(ready) => ready.load(Ordering::Acquire),
            })
    }
    pub(super) fn required(&self, key: &str) -> bool {
        self.0
            .lock()
            .is_ok_and(|rows| matches!(rows.get(key), Some(Gate::Required(_))))
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
    fn stage(
        &self,
        key: &str,
        ready: &Arc<AtomicBool>,
        read: CommittedDocument,
    ) -> Result<(), String> {
        let gates = self.0.lock().map_err(|_| "document reads poisoned")?;
        if !matches!(gates.get(key),Some(Gate::Required(current)) if Arc::ptr_eq(current,ready)) {
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
    fn current(&self, key: &str, ready: &Arc<AtomicBool>) -> bool {
        self.0.lock().is_ok_and(|rows|matches!(rows.get(key),Some(Gate::Required(current)) if Arc::ptr_eq(current,ready)))
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
        self.0
            .lock()
            .map_err(|_| "document reads poisoned")?
            .insert(key.into(), Gate::Legacy);
        Ok(())
    }
    fn pending(&self, key: &str) -> Result<Arc<AtomicBool>, String> {
        let mut rows = self.0.lock().map_err(|_| "document reads poisoned")?;
        if let Some(Gate::Required(old)) = rows.get(key) {
            old.store(false, Ordering::Release);
        }
        self.discard_stage(key);
        if !rows.contains_key(key) && rows.len() >= 64 {
            return Err("committed document route capacity".into());
        }
        let ready = Arc::new(AtomicBool::new(false));
        rows.insert(key.into(), Gate::Required(ready.clone()));
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
        };
        reads.stage("route", &ready, read.clone()).unwrap();
        reads.pending("route").unwrap();
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
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
async fn read_capture(
    channel: &runtime_collab::ChannelHandle,
    first: Reply,
) -> Result<(CommittedDocument, [u8; 32]), String> {
    read_capture_using(first, capture_budget(), async |request| {
        channel
            .committed_document(&request)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}
async fn read_capture_using(
    first: Reply,
    budget: &ByteBudget,
    mut send: impl AsyncFnMut(Request) -> Result<Reply, String>,
) -> Result<(CommittedDocument, [u8; 32]), String> {
    // Reserve before the full-document destination allocation. Every clone of
    // the event retains this one shared reservation until the last owner drops.
    let (first_id, first_token) = match &first {
        Reply::Chunk { capture, token, .. } => (*capture, *token),
        _ => return Err("committed initial capture missing".into()),
    };
    let prepared = (|| {
        let reservation = budget
            .reserve(MAX_DOCUMENT_BYTES, MAX_DOCUMENT_BYTES * 2)
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
            return Err(error);
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
    .map_err(|_| "committed read timed out".to_string())
    .and_then(|v| v);
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
    let acknowledgement = tokio::time::timeout(
        Duration::from_millis(200),
        send(Request::Cancel {
            capture: id,
            token: assembly.token,
        }),
    )
    .await
    .map_err(|_| "committed read acknowledgement timed out")??;
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
        },
        assembly.revision,
    ))
}
/// Resolve mode before the route can produce any document. Initial history is
/// queued without waiting for the UI owner (which awaits PrimaryReady itself).
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
pub(super) async fn install(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound: &InboundBridge,
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
    let ready = inbound.state.document_reads.pending(&key)?;
    let (initial, revision) = match first {
        Reply::Chunk { .. } => match read_capture(channel, first).await {
            Ok((read, revision)) => (Some(read), Some(revision)),
            Err(_) => (None, None),
        },
        Reply::Unavailable => (None, None),
        _ => return Err("invalid initial committed read mode".into()),
    };
    let channel = channel.clone();
    let spec = spec.clone();
    let inbound = inbound.clone();
    tokio::task::spawn_local(async move {
        let mut current = initial;
        let mut revision = revision;
        loop {
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
                ready.store(false, Ordering::Release);
            }
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                channel.committed_document(&Request::Open {
                    known_revision: if refresh { None } else { revision },
                }),
            )
            .await;
            match result {
                Ok(Ok(Reply::Unchanged { revision: checked })) if Some(checked) == revision => {
                    ready.store(true, Ordering::Release)
                }
                Ok(Ok(reply @ Reply::Chunk { .. })) => {
                    ready.store(false, Ordering::Release);
                    match read_capture(&channel, reply).await {
                        Ok((read, next)) => {
                            current = Some(read);
                            revision = Some(next);
                            continue;
                        }
                        Err(_) => {
                            revision = None;
                        }
                    }
                }
                Ok(Ok(Reply::Unavailable)) => {
                    ready.store(false, Ordering::Release);
                    revision = None;
                }
                _ => {
                    ready.store(false, Ordering::Release);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        ready.store(false, Ordering::Release);
        if inbound.state.document_reads.current(&key, &ready) {
            inbound.state.document_reads.discard_stage(&key);
        }
    });
    Ok(true)
}
