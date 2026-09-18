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
    desktop_reply: Option<oneshot::Sender<Result<OwnerCommitReceipt, qos_peer_state::desktop_command::DesktopCommandError>>>,
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
                    desktop_reply: None,
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
    pub fn is_desktop(&self) -> bool { self.desktop_reply.is_some() }
    pub fn apply_desktop(mut self, owner: &mut SessionOwner) -> Result<(), String> {
        use qos_peer_state::desktop_command::DesktopCommandError;
        let result = self.health.check().map_err(DesktopCommandError::Uncertain)
            .and_then(|()| owner.commit_desktop_command(&self.request));
        let health = match &result {
            Err(DesktopCommandError::Uncertain(reason)) => Err(self.health.fail(reason.clone())),
            _ => Ok(()),
        };
        if let Some(reply) = self.desktop_reply.take() { let _ = reply.send(result); }
        health
    }
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
        if self.reply.is_some() || self.desktop_reply.is_some() {
            self.health
                .fail("owner handoff dropped before commit; outcome unknown");
        }
    }
}

impl OwnerCommitHandle {
    pub(crate) fn desktop_command(&self, request: OwnerCommitRequest) -> qos_action_broker::services::DesktopCommandFuture {
        let this = self.clone();
        Box::pin(async move {
            use qos_peer_state::desktop_command::DesktopCommandError::{Rejected, Uncertain};
            this.health().map_err(Uncertain)?;
            if !matches!(request.operation, CommitOperation::DesktopCommand { .. }) { return Err(Rejected("not a desktop command".into())); }
            request.binding().map_err(Rejected)?;
            let bytes = postcard::to_stdvec(&request).map_err(|e| Rejected(e.to_string()))?;
            if bytes.len() > MAX_REQUEST_BYTES { return Err(Rejected("desktop command exceeds bounded payload".into())); }
            let permit = this.permits.clone().try_acquire_owned().map_err(|_| Rejected("desktop command capacity exhausted".into()))?;
            let (reply, response) = oneshot::channel();
            this.sender.try_send(PendingOwnerCommit { request: request.clone(), reply: None, desktop_reply: Some(reply), health: this.health.clone(), _permit: permit })
                .map_err(|_| Uncertain(this.health.fail("desktop owner queue unavailable; outcome unknown")))?;
            let receipt = tokio::time::timeout(COMMIT_TIMEOUT, response).await
                .map_err(|_| Uncertain(this.health.fail("desktop owner commit timed out; outcome unknown")))?
                .map_err(|_| Uncertain(this.health.fail("desktop owner reply lost; outcome unknown")))??;
            receipt.validate_for(&request).map_err(|e| Uncertain(this.health.fail(e)))?;
            Ok(receipt)
        })
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
            CommitOperation::DesktopCommand { .. } => {
                return Err("desktop commands require their admitted semantic service".into());
            }
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

#[cfg(test)]
mod desktop_mailbox_tests {
    use super::*;
    use qos_peer_state::desktop_command::*;
    fn command(owner: &SessionOwner, name: &str) -> OwnerCommitRequest {
        OwnerCommitRequest { room: owner.peer().room().into(), operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(), caller: owner.peer().authority().clone(), command: BoundCommand {
                source: CommandSource { room: owner.peer().room().into(), heads: owner.peer().admitted_room().unwrap().clone().get_heads().into_iter().map(|h|h.0).collect() },
                command: DesktopCommand::CreateStrip { name: name.into() },
            },
        } }
    }
    #[tokio::test]
    async fn definite_refusal_keeps_mailbox_healthy_but_failed_flush_is_uncertain() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let before = owner.status();
        let refused = tokio::spawn(service.desktop_command(command(&owner, "")));
        mailbox.recv().await.unwrap().apply_desktop(&mut owner).unwrap();
        assert!(matches!(refused.await.unwrap(), Err(DesktopCommandError::Rejected(_))));
        assert!(service.health().is_ok()); assert!(owner.permits_publication());
        assert_eq!(owner.status(), before);
        let request = command(&owner, "Durable");
        let mut receipt = tokio::spawn(service.desktop_command(request.clone()));
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut receipt).await.is_err());
        mailbox.recv().await.unwrap().apply_desktop(&mut owner).unwrap();
        receipt.await.unwrap().unwrap().validate_for(&request).unwrap();
        let before = owner.status();
        let uncertain = tokio::spawn(service.desktop_command(command(&owner, "Cannot flush")));
        let pending = mailbox.recv().await.unwrap();
        std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
        std::fs::create_dir(path.join("session.bin")).unwrap();
        assert!(pending.apply_desktop(&mut owner).is_err());
        assert!(matches!(uncertain.await.unwrap(), Err(DesktopCommandError::Uncertain(_))));
        assert!(service.health().is_err()); assert!(!owner.permits_publication());
        assert_eq!(owner.status(), before);
    }
}
