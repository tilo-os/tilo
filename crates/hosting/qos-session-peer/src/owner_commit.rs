//! Bounded host-local requests serviced by the existing serialized SessionOwner.
//! This mailbox owns no second document writer, signing key or persistence root.
use crate::SessionOwner;
use qos_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_INFLIGHT: usize = 32;
const COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default)]
struct Health(Mutex<Option<String>>);
impl Health {
    fn fail(&self, reason: impl Into<String>) -> String {
        let reason = reason.into();
        if let Ok(mut failed) = self.0.lock() {
            failed.get_or_insert(reason.clone());
        }
        reason
    }
    fn check(&self) -> Result<(), String> {
        match &*self
            .0
            .lock()
            .map_err(|_| "owner commit health unavailable")?
        {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
}
#[derive(Clone)]
pub struct OwnerCommitHandle {
    sender: mpsc::Sender<PendingOwnerCommit>,
    permits: Arc<Semaphore>,
    health: Arc<Health>,
}
pub struct OwnerCommitMailbox {
    receiver: mpsc::Receiver<PendingOwnerCommit>,
}
pub struct PendingOwnerCommit {
    request: OwnerCommitRequest,
    reply: Option<oneshot::Sender<Result<OwnerCommitReceipt, String>>>,
    health: Arc<Health>,
    _permit: OwnedSemaphorePermit,
}
impl OwnerCommitMailbox {
    /// The hard ceiling covers queued AND executing requests. Serialized encoded
    /// size bounds each request; pressure is observable and withdraws health.
    pub fn bounded(capacity: usize) -> (OwnerCommitHandle, Self) {
        let capacity = capacity.clamp(1, MAX_INFLIGHT);
        let (sender, receiver) = mpsc::channel(capacity);
        let handle = OwnerCommitHandle {
            sender,
            permits: Arc::new(Semaphore::new(capacity)),
            health: Arc::default(),
        };
        (handle, Self { receiver })
    }
    pub async fn recv(&mut self) -> Option<PendingOwnerCommit> {
        self.receiver.recv().await
    }
}
impl OwnerCommitService for OwnerCommitHandle {
    fn health(&self) -> Result<(), String> {
        self.health.check()?;
        if self.sender.is_closed() {
            return Err(self
                .health
                .fail("owner commit mailbox closed; outcome unknown"));
        }
        Ok(())
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let this = self.clone();
        Box::pin(async move {
            this.health()?;
            let bytes =
                postcard::to_stdvec(&request).map_err(|e| this.health.fail(e.to_string()))?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(this
                    .health
                    .fail("owner commit exceeds bounded payload; outcome unknown"));
            }
            request.binding().map_err(|e| this.health.fail(e))?;
            let permit = this.permits.clone().try_acquire_owned().map_err(|_| {
                this.health
                    .fail("owner commit capacity exhausted; outcome unknown")
            })?;
            let (reply, response) = oneshot::channel();
            this.sender
                .try_send(PendingOwnerCommit {
                    request: request.clone(),
                    reply: Some(reply),
                    health: this.health.clone(),
                    _permit: permit,
                })
                .map_err(|_| {
                    this.health
                        .fail("owner commit queue unavailable; outcome unknown")
                })?;
            let receipt = tokio::time::timeout(COMMIT_TIMEOUT, response)
                .await
                .map_err(|_| this.health.fail("owner commit timed out; outcome unknown"))?
                .map_err(|_| this.health.fail("owner commit reply lost; outcome unknown"))??;
            receipt
                .validate_for(&request)
                .map_err(|e| this.health.fail(e))?;
            Ok(receipt)
        })
    }
}
impl PendingOwnerCommit {
    /// Called only by the serialized owner loop. A successful reply follows its
    /// aggregate fsync+directory barrier. Dropped callers do not cancel commits.
    pub fn apply(mut self, owner: &mut SessionOwner) -> Result<OwnerCommitReceipt, String> {
        let result = self
            .health
            .check()
            .and_then(|()| owner.commit_local_owner_request(&self.request));
        if let Err(reason) = &result {
            self.health.fail(reason.clone());
        }
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result.clone());
        }
        result
    }
}
impl Drop for PendingOwnerCommit {
    fn drop(&mut self) {
        if self.reply.is_some() {
            self.health
                .fail("owner handoff dropped before commit; outcome unknown");
        }
    }
}
impl SessionOwner {
    fn commit_local_owner_request(
        &mut self,
        request: &OwnerCommitRequest,
    ) -> Result<OwnerCommitReceipt, String> {
        use ui::automerge::{
            AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
        };
        let binding = request.binding()?;
        if request.room != self.peer().room() || !self.permits_publication() {
            return Err("owner namespace unavailable".into());
        }
        let mut candidate = self.peer().clone();
        match &request.operation {
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => {
                let outcome = candidate.admit_local_app_document(
                    app,
                    record,
                    *envelope.clone(),
                    web_time::SystemTime::now(),
                )?;
                if outcome.deferred || outcome.incomplete.is_some() {
                    return Err("local publication admission incomplete".into());
                }
                let mutation = collab_types::DocumentMutation::decode(&envelope.payload)
                    .map_err(|e| e.to_string())?;
                let mut proof = AutoCommit::load(
                    &candidate
                        .document(app, record)
                        .ok_or("local record absent")?,
                )
                .map_err(|e| e.to_string())?;
                if qos_peer_state::document::merge_committed(&mut proof, &mutation)
                    .map_err(|e| format!("{e:?}"))?
                    || !proof.get_missing_deps(&[]).is_empty()
                {
                    return Err("committed record does not cover signed publication basis".into());
                }
            }
            CommitOperation::EffectReceipt {
                invocation,
                redacted,
            } => {
                let receipt: qos_desktop::WorkspaceEffectReceipt =
                    serde_json::from_slice(redacted).map_err(|e| e.to_string())?;
                if receipt.invocation.0 != *invocation {
                    return Err("effect receipt invocation mismatch".into());
                }
                let mut doc = candidate
                    .admitted_room()
                    .ok_or("owner governance missing")?
                    .clone();
                doc.set_actor(ui::automerge::ActorId::from(
                    self.identity.native.verifying_key().to_bytes().to_vec(),
                ));
                let object = match doc
                    .get(ROOT, "effect_receipts_v1")
                    .map_err(|e| e.to_string())?
                {
                    Some((Value::Object(ObjType::Map), object)) => object,
                    Some(_) => return Err("invalid canonical effect receipt map".into()),
                    None => doc
                        .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
                        .map_err(|e| e.to_string())?,
                };
                let id = invocation.to_string();
                match doc.get(&object, &id).map_err(|e| e.to_string())? {
                    Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Bytes(prior) if prior == redacted) =>
                        {}
                    Some(_) => return Err("effect receipt is immutable".into()),
                    None => {
                        doc.put(&object, &id, ScalarValue::Bytes(redacted.clone()))
                            .map_err(|e| e.to_string())?;
                    }
                }
                doc.commit();
                let changes = doc
                    .get_changes(&[])
                    .into_iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect();
                candidate.receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        self.identity.native.verifying_key().to_bytes(),
                    ),
                    qos_peer_state::NeighbourMsg::SyncResp {
                        room: request.room.clone(),
                        changes,
                    },
                    web_time::SystemTime::now(),
                )?;
            }
        }
        let committed = self
            .commit_admission(candidate, self.directory().to_vec())
            .map_err(|e| e.to_string())?;
        let aggregate_hash = hex::decode(committed.aggregate_hash)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "invalid aggregate digest")?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: committed.generation,
            aggregate_hash,
        };
        receipt.validate_for(request)?;
        Ok(receipt)
    }
}
