//! Exact bounded precommit handoffs for owner-bound application signers.
//! No sequence reservation escapes the serialized durable owner turn.
use crate::{SessionOwner, action_timing::Timing, owner_commit::OwnerCommitHandle};
use collab_types::{DocumentMutation, SignedOp};
use qos_peer_state::{
    NeighbourMsg,
    owner_commit::{
        CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
    },
};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot as response};
use ui::collaboration::{HostDocumentCommit, HostDocumentCommitter, RouteSpec};

const LIMIT: usize = 32;
struct Handoff {
    bytes: Vec<u8>,
    receipt: OwnerCommitReceipt,
    _permit: OwnedSemaphorePermit,
}
#[derive(Default)]
struct State {
    pending: Vec<Handoff>,
    failure: Option<String>,
}
#[derive(Clone)]
pub(crate) struct HostSigning {
    sender: mpsc::Sender<PendingSignature>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    ordinary: OwnerCommitHandle,
}
pub(crate) struct PendingSignature {
    route: RouteSpec,
    certificate: tilo_user_identity::DeviceCert,
    mutation: DocumentMutation,
    started: response::Sender<std::time::Instant>,
    reply: response::Sender<Result<SignedOp, String>>,
    service: HostSigning,
    permit: OwnedSemaphorePermit,
    timing: Timing,
}
struct BoundSigner {
    service: HostSigning,
    route: RouteSpec,
    certificate: tilo_user_identity::DeviceCert,
}
impl HostSigning {
    pub fn desktop_commands(
        &self,
        room: String,
    ) -> Arc<dyn qos_action_broker::services::DesktopCommands> {
        struct Commands {
            room: String,
            owner: OwnerCommitHandle,
        }
        impl qos_action_broker::services::DesktopCommands for Commands {
            fn room(&self) -> &str {
                &self.room
            }
            fn execute(
                &self,
                request: OwnerCommitRequest,
                direct_delivery: bool,
            ) -> qos_action_broker::services::DesktopCommandFuture {
                self.owner
                    .desktop_command_with_delivery(request, direct_delivery)
            }
        }
        Arc::new(Commands {
            room,
            owner: self.ordinary.clone(),
        })
    }
    pub fn new(ordinary: OwnerCommitHandle) -> (Self, mpsc::Receiver<PendingSignature>) {
        let (sender, receiver) = mpsc::channel(LIMIT);
        (
            Self {
                sender,
                state: Arc::default(),
                permits: Arc::new(Semaphore::new(LIMIT)),
                ordinary,
            },
            receiver,
        )
    }
    pub fn bind(
        &self,
        route: RouteSpec,
        certificate: tilo_user_identity::DeviceCert,
    ) -> Arc<dyn HostDocumentCommitter> {
        Arc::new(BoundSigner {
            service: self.clone(),
            route,
            certificate,
        })
    }
    pub fn fail(&self, reason: impl Into<String>) -> String {
        let reason = reason.into();
        if let Ok(mut state) = self.state.lock() {
            state.failure.get_or_insert(reason.clone());
        }
        self.permits.close();
        reason
    }
    #[cfg(feature = "test-support")]
    pub(crate) fn qualification_signing(&self) -> serde_json::Value {
        let pending_exact_receipts = self
            .state
            .lock()
            .map_or(usize::MAX, |state| state.pending.len());
        serde_json::json!({
            "outstanding_handoffs": LIMIT.saturating_sub(self.permits.available_permits()),
            "pending_exact_receipts": pending_exact_receipts,
        })
    }
}
impl HostDocumentCommitter for BoundSigner {
    fn commit(&self, route: &RouteSpec, mutation: DocumentMutation) -> HostDocumentCommit {
        let service = self.service.clone();
        let expected_route = self.route.clone();
        let certificate = self.certificate.clone();
        let route = route.clone();
        Box::pin(async move {
            service.health()?;
            if route != expected_route {
                return Err(service.fail("worker attempted a different owner route"));
            }
            if mutation.encode().map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
                return Err(service.fail("host mutation exceeds bounded payload"));
            }
            let permit = service.permits.clone().acquire_owned().await.map_err(|_| {
                service.health().err().unwrap_or_else(|| {
                    service.fail("host signing handoff unavailable; outcome unknown")
                })
            })?;
            // A previous in-flight request may have retired the service while this
            // request was waiting for exact receipt capacity.
            service.health()?;
            let (reply, receiver) = response::channel();
            let (started, start) = response::channel();
            let timing = Timing::new("signing_wait");
            service
                .sender
                .send(PendingSignature {
                    route,
                    certificate,
                    mutation,
                    started,
                    reply,
                    service: service.clone(),
                    permit,
                    timing: timing.child("signing_queue_to_reply"),
                })
                .await
                .map_err(|_| service.fail("host signer unavailable; outcome unknown"))?;
            timing.mark("enqueued");
            // Reconnect/replay may leave this bounded request queued indefinitely.
            // Only the ready serialized owner turn starts its execution deadline.
            let deadline = start
                .await
                .map_err(|_| service.fail("host signing reply lost; outcome unknown"))?;
            timing.mark("owner_started");
            let response = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                receiver,
            )
            .await;
            match response {
                Ok(Ok(result)) => {
                    timing.mark(if result.is_ok() {
                        "reply_ok"
                    } else {
                        "reply_error"
                    });
                    result
                }
                Ok(Err(_)) => {
                    timing.mark("disconnected");
                    Err(service.fail("host signing reply lost; outcome unknown"))
                }
                Err(_) => {
                    timing.mark("timeout");
                    Err(service.fail("host signing reply lost; outcome unknown"))
                }
            }
        })
    }
}

impl OwnerCommitService for HostSigning {
    fn health(&self) -> Result<(), String> {
        self.ordinary.health()?;
        if self.sender.is_closed() {
            return Err(self.fail("host signing mailbox closed; outcome unknown"));
        }
        let state = self.state.lock().map_err(|_| "host handoff unavailable")?;
        match &state.failure {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let this = self.clone();
        Box::pin(async move {
            this.health()?;
            let bytes = postcard::to_stdvec(&request).map_err(|e| this.fail(e.to_string()))?;
            let matched = {
                let mut state = this.state.lock().map_err(|_| "host handoff unavailable")?;
                state
                    .pending
                    .iter()
                    .position(|entry| entry.bytes == bytes)
                    .map(|i| state.pending.remove(i))
            };
            if let Some(handoff) = matched {
                handoff
                    .receipt
                    .validate_for(&request)
                    .map_err(|e| this.fail(e))?;
                return Ok(handoff.receipt);
            }
            // Every standalone local document comes from its bound host
            // committer. Unknown, altered and consumed handoffs never enter
            // ordinary admission or allocate another checkpoint sequence.
            if matches!(request.operation, CommitOperation::Document { .. }) {
                return Err(
                    this.fail("document has no pending exact owner receipt; prior outcome unknown")
                );
            }
            this.ordinary.commit(request).await
        })
    }
}
impl PendingSignature {
    pub(crate) fn diagnostic_timing(&self) -> Timing {
        self.timing.child("owner_signature")
    }
    #[cfg(feature = "test-support")]
    pub fn matches_route(&self, app: &str, record: &str) -> bool {
        self.route.app_id == app && self.route.record.as_deref().unwrap_or("") == record
    }
    /// Caller holds committed-authority visibility, refreshes policy and
    /// publishes outgoing state before invoking the returned completion.
    pub fn apply(self, owner: &mut SessionOwner) -> (impl FnOnce() + use<>, Result<(), String>) {
        let Self {
            route,
            certificate,
            mutation,
            started,
            reply,
            service,
            permit,
            timing,
        } = self;
        timing.mark("owner_dequeued");
        // Notification only: losing the caller does not cancel this owner turn.
        let _ = started.send(crate::owner_commit::execution_deadline());
        let result: Result<SignedOp, String> = (|| {
            service.health()?;
            let record = route.record.as_deref().unwrap_or("");
            if owner.peer().room() != route.cluster_id.to_string()
                || certificate != owner.identity.native_cert
                || !owner.peer().certificates().contains(&certificate)
                || owner.peer().document(&route.app_id, record).is_none()
                || !owner.permits_publication()
            {
                return Err("host signer route/certificate is no longer admitted".into());
            }
            owner
                .peer()
                .permissions(certificate.device_public_key, web_time::SystemTime::now())?;
            let mut candidate = owner.peer().clone();
            let NeighbourMsg::AppDocument { envelope, .. } = candidate.signed_mutation(
                &route.app_id,
                record,
                mutation,
                &owner.identity.native,
                web_time::SystemTime::now(),
            )?
            else {
                return Err("owner did not sign an application document".into());
            };
            let request = OwnerCommitRequest {
                room: owner.peer().room().into(),
                operation: CommitOperation::Document {
                    app: route.app_id,
                    record: record.into(),
                    envelope: envelope.clone(),
                },
            };
            timing.mark("admission_start");
            let committed = owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .map_err(|e| e.to_string())?;
            timing.mark("admission_end");
            let receipt = OwnerCommitReceipt {
                binding: request.binding()?,
                generation: committed.generation,
                aggregate_hash: hex::decode(committed.aggregate_hash)
                    .map_err(|e| e.to_string())?
                    .try_into()
                    .map_err(|_| "invalid aggregate hash")?,
            };
            receipt.validate_for(&request)?;
            let bytes = postcard::to_stdvec(&request).map_err(|e| e.to_string())?;
            service
                .state
                .lock()
                .map_err(|_| "host handoff unavailable")?
                .pending
                .push(Handoff {
                    bytes,
                    receipt,
                    _permit: permit,
                });
            Ok(*envelope)
        })();
        if let Err(reason) = &result {
            service.fail(reason.clone());
        }
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        timing.mark(if result.is_ok() {
            "apply_ok"
        } else {
            "apply_error"
        });
        let complete = move || {
            timing.mark("reply_start");
            if reply.send(result).is_err() {
                service.fail("host signer response dropped after admission; outcome unknown");
                timing.mark("reply_dropped");
            } else {
                timing.mark("reply_sent");
            }
        };
        (complete, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner_commit::OwnerCommitMailbox;
    fn request(
        owner: &SessionOwner,
        app: &str,
        record: &str,
        envelope: SignedOp,
    ) -> OwnerCommitRequest {
        OwnerCommitRequest {
            room: owner.peer().room().into(),
            operation: CommitOperation::Document {
                app: app.into(),
                record: record.into(),
                envelope: Box::new(envelope),
            },
        }
    }
    async fn precommit(
        service: &HostSigning,
        receiver: &mut mpsc::Receiver<PendingSignature>,
        owner: &mut SessionOwner,
        app: &str,
        record: &str,
    ) -> SignedOp {
        let route = if record.is_empty() {
            RouteSpec::app(app, owner.peer().room().parse().unwrap())
        } else {
            RouteSpec::record(app, owner.peer().room().parse().unwrap(), record)
        };
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation = DocumentMutation::Snapshot(owner.peer().document(app, record).unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = receiver.recv().await.unwrap();
        let (complete, result) = pending.apply(owner);
        result.unwrap();
        complete();
        waiting.await.unwrap().unwrap()
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_signing_interleaved_exact_receipts_preserve_floor_and_no_replay_exception() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let baseline = owner.outgoing().to_vec();
        let chat_receipt = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
        let writer = precommit(
            &service,
            &mut receiver,
            &mut owner,
            "dev.qos.word-processor",
            "tab:0000000000000001",
        )
        .await;
        assert!(owner.outgoing().starts_with(&baseline));
        assert_eq!(
            owner.outgoing().len() - baseline.len(),
            2,
            "signer path publishes only its touched route"
        );
        assert_eq!(
            owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
            chat_receipt
        );
        assert_eq!(
            &owner.outgoing_kinds()[baseline.len()..],
            &[
                qos_peer_state::transport::Retention::Delta,
                qos_peer_state::transport::Retention::Checkpoint
            ]
        );
        let request_writer = request(
            &owner,
            "dev.qos.word-processor",
            "tab:0000000000000001",
            writer.clone(),
        );
        let first_generation = owner.status().generation;
        let chat = precommit(&service, &mut receiver, &mut owner, "dev.qos.chat", "").await;
        owner.refresh_checkpoint().unwrap();
        let current = owner.status();
        assert!(
            current
                .replay_floors
                .iter()
                .any(|(app, _, author, floor)| app == "dev.qos.word-processor"
                    && author == writer.author.as_bytes()
                    && *floor > writer.seq)
        );
        let receipt = service.commit(request_writer.clone()).await.unwrap();
        receipt.validate_for(&request_writer).unwrap();
        assert_eq!(receipt.generation, first_generation);
        assert_eq!(
            owner.status(),
            current,
            "receipt handoff cannot recommit or allocate another sequence"
        );
        service
            .commit(request(&owner, "dev.qos.chat", "", chat))
            .await
            .unwrap();
        assert_eq!(service.permits.available_permits(), LIMIT);
        // Consumed receipt never creates a replay exemption on another call.
        let outgoing = owner.outgoing().to_vec();
        assert!(service.commit(request_writer).await.is_err());
        assert_eq!(owner.outgoing(), outgoing);
        assert_eq!(owner.status(), current);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_signing_changed_envelope_cannot_consume_precommitted_receipt() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let mut op = precommit(&service, &mut receiver, &mut owner, "dev.qos.chat", "").await;
        let generation = owner.status().generation;
        op.seq += 1;
        let outgoing = owner.outgoing().to_vec();
        let status = owner.status();
        assert!(
            service
                .commit(request(&owner, "dev.qos.chat", "", op))
                .await
                .is_err()
        );
        assert_eq!(owner.outgoing(), outgoing);
        assert_eq!(owner.status(), status);
        assert_eq!(owner.status().generation, generation);
        assert_eq!(
            service.state.lock().unwrap().pending.len(),
            1,
            "unresolved exact handoff must not be evicted"
        );
    }
    #[tokio::test]
    async fn host_signing_lost_response_keeps_committed_uncertainty_and_mailbox_failure_retires_health()
     {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("owner");
        let mut owner = SessionOwner::initialize(&state).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, receiver) = HostSigning::new(handle);
        let (reply, lost) = response::channel();
        let (started, lost_start) = response::channel();
        drop((lost, lost_start));
        let pending = PendingSignature {
            route: RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap()),
            certificate: owner.identity.native_cert.clone(),
            mutation: DocumentMutation::Snapshot(
                owner.peer().document("dev.qos.chat", "").unwrap(),
            ),
            started,
            reply,
            service: service.clone(),
            permit: service.permits.clone().try_acquire_owned().unwrap(),
            timing: Timing::new("signing_queue_to_reply"),
        };
        let before = owner.status().generation;
        let (complete, result) = pending.apply(&mut owner);
        result.unwrap();
        complete();
        assert!(owner.status().generation > before);
        assert!(service.health().unwrap_err().contains("outcome unknown"));
        assert_eq!(service.state.lock().unwrap().pending.len(), 1);
        let committed = owner.status();
        drop(owner);
        assert_eq!(SessionOwner::open(&state).unwrap().status(), committed);
        let (handle, _ordinary2) = OwnerCommitMailbox::bounded(8);
        let (service2, receiver2) = HostSigning::new(handle);
        drop(receiver2);
        assert!(
            service2
                .health()
                .unwrap_err()
                .contains("signing mailbox closed")
        );
        drop(receiver);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_signing_survives_reconnect_longer_than_execution_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let before = owner.status();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        // A disconnected/replaying host cannot enter PendingSignature::apply.
        // Holding the real queued request models that ready-gated owner turn.
        let pending = receiver.recv().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
        assert!(
            !waiting.is_finished(),
            "queued work must not spend its execution deadline"
        );
        assert!(service.health().is_ok());
        assert_eq!(owner.status(), before);
        let (complete, result) = pending.apply(&mut owner);
        result.unwrap();
        complete();
        let signed = waiting.await.unwrap().unwrap();
        let request = request(&owner, "dev.qos.chat", "", signed);
        let committed = owner.status();
        assert_eq!(committed.generation, before.generation + 1);
        let receipt = service.commit(request.clone()).await.unwrap();
        receipt.validate_for(&request).unwrap();
        assert_eq!(owner.status(), committed);
        assert!(
            service.commit(request).await.is_err(),
            "the exact receipt is single-use"
        );
        assert_eq!(owner.status(), committed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn started_signing_timeout_keeps_committed_receipt_and_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = receiver.recv().await.unwrap();
        let before = owner.status().generation;
        let (complete, result) = pending.apply(&mut owner);
        result.unwrap();
        assert_eq!(owner.status().generation, before + 1);
        // The owner barrier happened; deliberately withhold the existing completion.
        let error = tokio::time::timeout(std::time::Duration::from_secs(7), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("outcome unknown"));
        assert!(service.health().is_err());
        assert_eq!(service.state.lock().unwrap().pending.len(), 1);
        complete();
        assert_eq!(service.state.lock().unwrap().pending.len(), 1);
        assert_eq!(owner.status().generation, before + 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_signing_exits_when_owner_mailbox_closes() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = receiver.recv().await.unwrap();
        drop(pending);
        drop(receiver);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(service.health().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signing_uses_owner_timestamp_not_a_fresh_budget_on_start_delivery() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = receiver.recv().await.unwrap();
        // Deliver an owner deadline already elapsed, representing late observation.
        pending
            .started
            .send(std::time::Instant::now() - std::time::Duration::from_millis(1))
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .contains("outcome unknown")
        );
        assert!(service.health().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn saturated_signing_handoff_waits_for_exact_receipt_capacity() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());

        let mut waiting = Vec::with_capacity(LIMIT);
        for _ in 0..LIMIT {
            let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
            let route = route.clone();
            let mutation = mutation.clone();
            waiting.push(tokio::spawn(signer.commit(&route, mutation)));
        }
        let mut pending = Vec::with_capacity(LIMIT);
        for _ in 0..LIMIT {
            pending.push(
                tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                    .await
                    .expect("bounded signer entered its owner queue")
                    .expect("owner queue remains open"),
            );
        }
        assert_eq!(service.permits.available_permits(), 0);
        assert!(service.health().is_ok());

        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let overflow_route = route.clone();
        let overflow_mutation = mutation.clone();
        let mut overflow = tokio::spawn(signer.commit(&overflow_route, overflow_mutation));
        let early =
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut overflow).await;
        if let Ok(early) = early {
            // The current fail-fast implementation reaches this branch. Release
            // every request before reporting RED so no blocking worker is leaked.
            for request in pending {
                let (complete, _) = request.apply(&mut owner);
                complete();
            }
            for task in waiting {
                let _ = task.await;
            }
            panic!(
                "request beyond the exact receipt limit must wait without failing service: {early:?}"
            );
        }
        assert!(service.health().is_ok());

        for request in pending {
            let (complete, result) = request.apply(&mut owner);
            result.expect("queued exact mutation remains admitted");
            complete();
        }
        let mut signed = Vec::with_capacity(LIMIT);
        for task in waiting {
            signed.push(
                task.await
                    .expect("signer task")
                    .expect("queued signer receives its exact operation"),
            );
        }
        assert_eq!(service.permits.available_permits(), 0);

        let first = request(&owner, "dev.qos.chat", "", signed.remove(0));
        service
            .commit(first)
            .await
            .expect("consuming one exact receipt releases one permit");
        let overflow_pending =
            tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                .await
                .expect("waiting signer advances after exact receipt consumption")
                .expect("owner queue remains open");
        let (complete, result) = overflow_pending.apply(&mut owner);
        result.expect("waiting exact mutation remains admitted");
        complete();
        let overflow_op = overflow
            .await
            .expect("waiting signer task")
            .expect("waiting signer receives its exact operation");

        for op in signed {
            service
                .commit(request(&owner, "dev.qos.chat", "", op))
                .await
                .expect("remaining exact receipt");
        }
        service
            .commit(request(&owner, "dev.qos.chat", "", overflow_op))
            .await
            .expect("advanced exact receipt");
        assert_eq!(service.permits.available_permits(), LIMIT);
        assert!(service.health().is_ok());
    }

    #[tokio::test]
    async fn signing_failure_wakes_request_waiting_for_receipt_capacity() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, _receiver) = HostSigning::new(handle);
        let held = (0..LIMIT)
            .map(|_| service.permits.clone().try_acquire_owned().unwrap())
            .collect::<Vec<_>>();
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        service.fail("owner authority retired while signing waited");
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("closed capacity wakes waiter")
            .expect("signer task")
            .unwrap_err();
        assert_eq!(error, "owner authority retired while signing waited");
        drop(held);
    }

    #[tokio::test]
    async fn cancelled_signer_after_enqueue_preserves_owner_effect_and_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("owner");
        let mut owner = SessionOwner::initialize(&state).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = receiver.recv().await.unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());

        let before = owner.status().generation;
        let (complete, result) = pending.apply(&mut owner);
        result.expect("enqueued owner turn remains authoritative after caller cancellation");
        complete();
        assert!(owner.status().generation > before);
        assert!(service.health().unwrap_err().contains("outcome unknown"));
        assert_eq!(service.state.lock().unwrap().pending.len(), 1);
        let committed = owner.status();
        drop(owner);
        assert_eq!(SessionOwner::open(&state).unwrap().status(), committed);
    }
}
