//! Bounded host-local requests serviced by the existing serialized SessionOwner.
//! This mailbox owns no second document writer, signing key or persistence root.
use crate::{SessionOwner, action_timing::Timing};
use qos_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_INFLIGHT: usize = 32;
const COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Queue residence is bounded by permits/payload, not by execution time: the
/// native owner cannot claim work until its retained subscription is ready.
/// Timestamp at the owner turn, so a delayed caller wake gets no fresh budget.
pub(crate) fn execution_deadline() -> std::time::Instant {
    std::time::Instant::now() + COMMIT_TIMEOUT
}

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
    direct_delivery: bool,
    started: Option<oneshot::Sender<std::time::Instant>>,
    reply: Option<oneshot::Sender<Result<OwnerCommitReceipt, String>>>,
    desktop_reply: Option<
        oneshot::Sender<
            Result<OwnerCommitReceipt, qos_peer_state::desktop_command::DesktopCommandError>,
        >,
    >,
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
            let (started, start) = oneshot::channel();
            this.sender
                .try_send(PendingOwnerCommit {
                    request: request.clone(),
                    direct_delivery: false,
                    started: Some(started),
                    reply: Some(reply),
                    desktop_reply: None,
                    health: this.health.clone(),
                    _permit: permit,
                })
                .map_err(|_| {
                    this.health
                        .fail("owner commit queue unavailable; outcome unknown")
                })?;
            let deadline = start
                .await
                .map_err(|_| this.health.fail("owner commit reply lost; outcome unknown"))?;
            let receipt = tokio::time::timeout_at(deadline.into(), response)
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
    fn start_execution(&mut self) {
        if let Some(started) = self.started.take() {
            // This is a milestone, not a cancellation/authorization handshake.
            // Dropped callers still do not cancel an owner's admitted work.
            let _ = started.send(execution_deadline());
        }
    }
    pub fn is_desktop(&self) -> bool {
        self.desktop_reply.is_some()
    }
    pub fn apply_desktop(
        mut self,
        owner: &mut SessionOwner,
    ) -> Result<Option<tilo_relay::peer_actions::DirectedAction>, String> {
        use qos_peer_state::desktop_command::DesktopCommandError;
        let timing = Timing::new("owner_commit_desktop");
        self.start_execution();
        timing.mark("semantic_apply_start");
        let result = self
            .health
            .check()
            .map_err(DesktopCommandError::Uncertain)
            .and_then(|()| {
                if self.direct_delivery {
                    owner.commit_desktop_command_with_delivery(&self.request)
                } else {
                    owner
                        .commit_desktop_command(&self.request)
                        .map(|receipt| (receipt, None))
                }
            });
        timing.mark(if result.is_ok() {
            "semantic_apply_ok"
        } else {
            "semantic_apply_error"
        });
        let health = match &result {
            Err(DesktopCommandError::Uncertain(reason)) => Err(self.health.fail(reason.clone())),
            _ => Ok(()),
        };
        let (result, delivery) = match result {
            Ok((receipt, delivery)) => (Ok(receipt), delivery),
            Err(error) => (Err(error), None),
        };
        timing.mark("reply_start");
        if let Some(reply) = self.desktop_reply.take() {
            let _ = reply.send(result);
        }
        timing.mark("reply_end");
        health.map(|()| delivery)
    }
    /// Called only by the serialized owner loop. A successful reply follows its
    /// aggregate fsync+directory barrier. Dropped callers do not cancel commits.
    pub fn apply(mut self, owner: &mut SessionOwner) -> Result<OwnerCommitReceipt, String> {
        let phase = match &self.request.operation {
            CommitOperation::DesktopCommand { .. } => "owner_commit_invalid_desktop",
            CommitOperation::Document { .. } => "owner_commit_document",
            CommitOperation::EffectReceipt { .. } => "owner_commit_effect_receipt",
        };
        let timing = Timing::new(phase);
        self.start_execution();
        timing.mark("semantic_apply_start");
        let result = self
            .health
            .check()
            .and_then(|()| owner.commit_local_owner_request(&self.request));
        timing.mark(if result.is_ok() {
            "semantic_apply_ok"
        } else {
            "semantic_apply_error"
        });
        if let Err(reason) = &result {
            self.health.fail(reason.clone());
        }
        timing.mark("reply_start");
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result.clone());
        }
        timing.mark("reply_end");
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
    #[cfg(test)]
    pub(crate) fn desktop_command(
        &self,
        request: OwnerCommitRequest,
    ) -> qos_action_broker::services::DesktopCommandFuture {
        self.desktop_command_with_delivery(request, false)
    }
    pub(crate) fn desktop_command_with_delivery(
        &self,
        request: OwnerCommitRequest,
        direct_delivery: bool,
    ) -> qos_action_broker::services::DesktopCommandFuture {
        let this = self.clone();
        Box::pin(async move {
            use qos_peer_state::desktop_command::DesktopCommandError::{Rejected, Uncertain};
            this.health().map_err(Uncertain)?;
            if !matches!(request.operation, CommitOperation::DesktopCommand { .. }) {
                return Err(Rejected("not a desktop command".into()));
            }
            request.binding().map_err(Rejected)?;
            let bytes = postcard::to_stdvec(&request).map_err(|e| Rejected(e.to_string()))?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(Rejected("desktop command exceeds bounded payload".into()));
            }
            let permit = this
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| Rejected("desktop command capacity exhausted".into()))?;
            let (reply, response) = oneshot::channel();
            let (started, start) = oneshot::channel();
            this.sender
                .try_send(PendingOwnerCommit {
                    request: request.clone(),
                    direct_delivery,
                    started: Some(started),
                    reply: None,
                    desktop_reply: Some(reply),
                    health: this.health.clone(),
                    _permit: permit,
                })
                .map_err(|_| {
                    Uncertain(
                        this.health
                            .fail("desktop owner queue unavailable; outcome unknown"),
                    )
                })?;
            let deadline = start.await.map_err(|_| {
                Uncertain(
                    this.health
                        .fail("desktop owner reply lost; outcome unknown"),
                )
            })?;
            let receipt = tokio::time::timeout_at(deadline.into(), response)
                .await
                .map_err(|_| {
                    Uncertain(
                        this.health
                            .fail("desktop owner commit timed out; outcome unknown"),
                    )
                })?
                .map_err(|_| {
                    Uncertain(
                        this.health
                            .fail("desktop owner reply lost; outcome unknown"),
                    )
                })??;
            receipt
                .validate_for(&request)
                .map_err(|e| Uncertain(this.health.fail(e)))?;
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
                let mutation = match collab_types::validate_document_payload_route_versioned(
                    app,
                    &request.room,
                    record,
                    envelope,
                )
                .map_err(|e| e.to_string())?
                {
                    collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) => {
                        decoded.mutation
                    }
                    collab_types::VersionedRoutedDocumentPayload::Encrypted { .. } => {
                        return Err(
                            "local publication admission requires decrypted document state".into(),
                        );
                    }
                };
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
            .commit_mutation_candidate(candidate, self.directory().to_vec())
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
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::DesktopCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: owner.peer().authority().clone(),
                command: BoundCommand {
                    source: CommandSource {
                        room: owner.peer().room().into(),
                        heads: owner
                            .peer()
                            .admitted_room()
                            .unwrap()
                            .clone()
                            .get_heads()
                            .into_iter()
                            .map(|h| h.0)
                            .collect(),
                    },
                    command: DesktopCommand::CreateStrip { name: name.into() },
                },
            },
        }
    }
    #[tokio::test]
    async fn definite_refusal_keeps_mailbox_healthy_but_failed_flush_is_uncertain() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let before = owner.status();
        let refused = tokio::spawn(service.desktop_command(command(&owner, "")));
        assert!(
            mailbox
                .recv()
                .await
                .unwrap()
                .apply_desktop(&mut owner)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            refused.await.unwrap(),
            Err(DesktopCommandError::Rejected(_))
        ));
        assert!(service.health().is_ok());
        assert!(owner.permits_publication());
        assert_eq!(owner.status(), before);
        let request = command(&owner, "Durable");
        let mut receipt = tokio::spawn(service.desktop_command(request.clone()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut receipt)
                .await
                .is_err()
        );
        mailbox
            .recv()
            .await
            .unwrap()
            .apply_desktop(&mut owner)
            .unwrap();
        receipt
            .await
            .unwrap()
            .unwrap()
            .validate_for(&request)
            .unwrap();
        let before = owner.status();
        let uncertain = tokio::spawn(service.desktop_command(command(&owner, "Cannot flush")));
        let pending = mailbox.recv().await.unwrap();
        std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
        std::fs::create_dir(path.join("session.bin")).unwrap();
        assert!(pending.apply_desktop(&mut owner).is_err());
        assert!(matches!(
            uncertain.await.unwrap(),
            Err(DesktopCommandError::Uncertain(_))
        ));
        assert!(service.health().is_err());
        assert!(!owner.permits_publication());
        assert_eq!(owner.status(), before);
    }
    #[tokio::test]
    async fn immediate_delivery_requires_explicit_browser_opt_in() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let waiting = tokio::spawn(service.desktop_command(command(&owner, "Old browser")));
        assert!(
            mailbox
                .recv()
                .await
                .unwrap()
                .apply_desktop(&mut owner)
                .unwrap()
                .is_none(),
            "old tabs cannot decode the new private message"
        );
        waiting.await.unwrap().unwrap();
        let waiting = tokio::spawn(
            service.desktop_command_with_delivery(command(&owner, "New browser"), true),
        );
        assert!(
            mailbox
                .recv()
                .await
                .unwrap()
                .apply_desktop(&mut owner)
                .unwrap()
                .is_some()
        );
        waiting.await.unwrap().unwrap();
        let waiting =
            tokio::spawn(service.desktop_command_with_delivery(command(&owner, ""), true));
        assert!(
            mailbox
                .recv()
                .await
                .unwrap()
                .apply_desktop(&mut owner)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            waiting.await.unwrap(),
            Err(DesktopCommandError::Rejected(_))
        ));
        let path = root.path().join("owner");
        let waiting = tokio::spawn(
            service.desktop_command_with_delivery(command(&owner, "Failed disk"), true),
        );
        let pending = mailbox.recv().await.unwrap();
        std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
        std::fs::create_dir(path.join("session.bin")).unwrap();
        assert!(pending.apply_desktop(&mut owner).is_err());
        assert!(matches!(
            waiting.await.unwrap(),
            Err(DesktopCommandError::Uncertain(_))
        ));
    }
    fn effect(owner: &SessionOwner) -> OwnerCommitRequest {
        let invocation = uuid::Uuid::new_v4();
        let receipt = qos_desktop::WorkspaceEffectReceipt {
            invocation: serde_json::from_value(serde_json::json!(invocation)).unwrap(),
            actor: hex::encode(owner.peer().authority().user_public_key),
            context: Default::default(),
            status: serde_json::from_value(serde_json::json!({"status":"succeeded"})).unwrap(),
            completed_at: 42,
        };
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::EffectReceipt {
                invocation,
                redacted: serde_json::to_vec(&receipt).unwrap(),
            },
        }
    }

    #[tokio::test]
    async fn queued_ordinary_commit_survives_reconnect_longer_than_execution_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let before = owner.status();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let request = effect(&owner);
        let waiting = tokio::spawn(service.commit(request.clone()));
        let pending = mailbox.recv().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
        assert!(
            !waiting.is_finished(),
            "queued ordinary work must survive replay"
        );
        assert!(service.health().is_ok());
        assert_eq!(owner.status(), before);
        pending.apply(&mut owner).unwrap();
        let receipt = waiting.await.unwrap().unwrap();
        receipt.validate_for(&request).unwrap();
        assert_eq!(owner.status().generation, before.generation + 1);
        assert_eq!(receipt.generation, before.generation + 1);
        assert!(service.health().is_ok());
    }

    #[tokio::test]
    async fn queued_desktop_commit_survives_reconnect_longer_than_execution_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let before = owner.status();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let request = command(&owner, "After replay");
        let waiting = tokio::spawn(service.desktop_command(request.clone()));
        let pending = mailbox.recv().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
        assert!(
            !waiting.is_finished(),
            "queued desktop work must survive replay"
        );
        assert!(service.health().is_ok());
        assert_eq!(owner.status(), before);
        pending.apply_desktop(&mut owner).unwrap();
        let receipt = waiting.await.unwrap().unwrap();
        receipt.validate_for(&request).unwrap();
        assert_eq!(owner.status().generation, before.generation + 1);
        assert_eq!(receipt.generation, before.generation + 1);
        assert!(service.health().is_ok());
    }

    #[tokio::test]
    async fn queued_owner_work_exits_on_closed_mailbox_and_dropped_caller_does_not_cancel() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let waiting = tokio::spawn(service.commit(effect(&owner)));
        let pending = mailbox.recv().await.unwrap();
        drop(pending);
        drop(mailbox);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
        let waiting = tokio::spawn(service.desktop_command(command(&owner, "Caller dropped")));
        let pending = mailbox.recv().await.unwrap();
        waiting.abort();
        let _ = waiting.await;
        let before = owner.status().generation;
        pending.apply_desktop(&mut owner).unwrap();
        assert_eq!(owner.status().generation, before + 1);
        assert!(
            service.health().is_ok(),
            "ordinary dropped-caller behavior is unchanged"
        );
    }

    #[tokio::test]
    async fn delayed_start_observation_does_not_restart_ordinary_or_desktop_deadline() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (ordinary, mut ordinary_mailbox) = OwnerCommitMailbox::bounded(2);
        let mut ordinary_waiting = tokio::spawn(ordinary.commit(effect(&owner)));
        let mut ordinary_pending = ordinary_mailbox.recv().await.unwrap();
        let (desktop, mut desktop_mailbox) = OwnerCommitMailbox::bounded(2);
        let mut desktop_waiting =
            tokio::spawn(desktop.desktop_command(command(&owner, "Delayed observation")));
        let mut desktop_pending = desktop_mailbox.recv().await.unwrap();
        ordinary_pending.start_execution();
        desktop_pending.start_execution();
        // Block this current-thread runtime: neither caller can observe Started.
        // This models an owner turn whose synchronous work exhausted its budget.
        std::thread::sleep(std::time::Duration::from_millis(5100));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), &mut ordinary_waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .contains("timed out")
        );
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_millis(250), &mut desktop_waiting)
                .await
                .unwrap()
                .unwrap(),
            Err(DesktopCommandError::Uncertain(_))
        ));
        assert!(ordinary.health().is_err());
        assert!(desktop.health().is_err());
        drop((ordinary_pending, desktop_pending));
    }

    #[tokio::test]
    async fn cancellation_after_started_does_not_cancel_ordinary_or_desktop_commit() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        for desktop in [false, true] {
            let (service, mut mailbox) = OwnerCommitMailbox::bounded(2);
            let caller = service.clone();
            let request = if desktop {
                command(&owner, "Dropped after start")
            } else {
                effect(&owner)
            };
            let waiting = tokio::spawn(async move {
                if desktop {
                    let _ = caller.desktop_command(request).await;
                } else {
                    let _ = caller.commit(request).await;
                }
            });
            let mut pending = mailbox.recv().await.unwrap();
            pending.start_execution();
            waiting.abort();
            let _ = waiting.await;
            let before = owner.status().generation;
            if desktop {
                pending.apply_desktop(&mut owner).unwrap();
            } else {
                pending.apply(&mut owner).unwrap();
            }
            assert_eq!(owner.status().generation, before + 1);
            assert!(
                service.health().is_ok(),
                "dropped-caller baseline must remain unchanged"
            );
        }
    }
}
