//! Native adapter for the shared bounded document precommit handoff.
//! SessionOwner retains keys, mutation admission and the durable barrier.
use crate::{SessionOwner, action_timing::Timing, owner_commit::OwnerCommitHandle};
#[cfg(test)]
use collab_types::{DocumentMutation, SignedOp};
use qos_action_broker::host_documents::{
    HostDocumentTurnError, HostDocuments, PendingHostDocument,
};
use qos_action_broker::peer_actions::PolicyTime;
use qos_peer_state::{
    NeighbourMsg,
    owner_commit::{
        CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
    },
};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use ui::collaboration::{HostDocumentCommitter, RouteSpec};

#[cfg(all(test, feature = "test-support"))]
mod committed_echo_tests;
#[cfg(all(test, feature = "test-support"))]
mod exact_startup_tests;

#[derive(Clone)]
pub(crate) struct HostSigning {
    shared: HostDocuments,
    ordinary: OwnerCommitHandle,
    policy_time: Arc<OnceLock<Option<PolicyTime>>>,
}
pub(crate) struct SignatureMailbox(
    mpsc::Receiver<PendingHostDocument>,
    Arc<OnceLock<Option<PolicyTime>>>,
);
impl SignatureMailbox {
    pub(crate) fn try_recv(&mut self) -> Result<PendingSignature, mpsc::error::TryRecvError> {
        self.0.try_recv().map(|pending| {
            PendingSignature(
                pending,
                Timing::new("signing_queue_to_reply"),
                self.1.get().cloned().flatten(),
            )
        })
    }
    pub async fn recv(&mut self) -> Option<PendingSignature> {
        self.0.recv().await.map(|pending| {
            PendingSignature(
                pending,
                Timing::new("signing_queue_to_reply"),
                self.1.get().cloned().flatten(),
            )
        })
    }
}
pub(crate) struct PendingSignature(PendingHostDocument, Timing, Option<PolicyTime>);
/// Exact signed candidate and worker custody; only the serialized signing turn
/// constructs this final pre-store fence.
pub(crate) struct HostSigningPolicyFence {
    time: PolicyTime,
    request: OwnerCommitRequest,
    certificate: tilo_user_identity::DeviceCert,
}
impl HostSigningPolicyFence {
    pub(crate) fn policy_time(&self) -> &PolicyTime {
        &self.time
    }
    pub(crate) fn revalidate(&self, owner: &SessionOwner) -> Result<(), String> {
        let now = web_time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_nanos(
                self.time.now_ns().map_err(|error| error.to_string())?,
            );
        if !owner.permits_publication()
            || self.request.room != owner.peer().room()
            || self.certificate != owner.identity.native_cert
        {
            return Err("original signing owner custody changed".into());
        }
        owner
            .peer()
            .permissions(self.certificate.device_public_key, now)?;
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = &self.request.operation
        else {
            return Err("signing fence requires exact document".into());
        };
        let outcome =
            owner
                .peer()
                .clone()
                .admit_local_app_document(app, record, *envelope.clone(), now)?;
        if outcome.deferred || outcome.incomplete.is_some() {
            return Err("original signed document current admission incomplete".into());
        }
        Ok(())
    }
}
impl HostSigning {
    pub(crate) fn register_policy_time(&self, time: PolicyTime) -> Result<(), String> {
        time.now_ns().map_err(|error| error.to_string())?;
        if self.policy_time.get().is_some_and(|registered| {
            registered
                .as_ref()
                .is_none_or(|registered| !registered.same_origin(&time))
        }) {
            return Err("host signer already belongs to another policy lifetime".into());
        }
        self.ordinary.register_policy_time(time.clone())?;
        self.shared.register_policy_time(time.clone())?;
        let registered = self.policy_time.get_or_init(|| Some(time.clone()));
        if registered
            .as_ref()
            .is_none_or(|registered| !registered.same_origin(&time))
        {
            return Err("host signer policy registration changed".into());
        }
        Ok(())
    }
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
    pub fn new(ordinary: OwnerCommitHandle) -> (Self, SignatureMailbox) {
        let (shared, receiver) = HostDocuments::new(Arc::new(ordinary.clone()));
        let policy_time = Arc::new(OnceLock::new());
        (
            Self {
                shared,
                ordinary,
                policy_time: policy_time.clone(),
            },
            SignatureMailbox(receiver, policy_time),
        )
    }
    pub fn bind(
        &self,
        route: RouteSpec,
        certificate: tilo_user_identity::DeviceCert,
    ) -> Arc<dyn HostDocumentCommitter> {
        self.policy_time.get_or_init(|| None);
        self.shared.bind(route, certificate)
    }
    pub fn fail(&self, reason: impl Into<String>) -> String {
        self.shared.fail(reason)
    }
    pub(crate) fn close_admission(&self) -> Result<(), String> {
        self.shared.close_admission()
    }
    /// Quiescence never clears a terminal signing failure or consumes a receipt.
    pub(crate) fn retirement_ready(&self) -> Result<bool, String> {
        self.shared.health()?;
        Ok(self.shared.pending_exact_receipts()? == 0 && self.shared.outstanding_handoffs() == 0)
    }
    #[cfg(feature = "test-support")]
    pub(crate) fn qualification_signing(&self) -> serde_json::Value {
        serde_json::json!({
            "outstanding_handoffs": self.shared.outstanding_handoffs(),
            "pending_exact_receipts": self.shared.pending_exact_receipts().unwrap_or(usize::MAX),
        })
    }
}
impl OwnerCommitService for HostSigning {
    fn health(&self) -> Result<(), String> {
        self.shared.health()
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.shared.commit(request)
    }
}
impl PendingSignature {
    pub(crate) fn reject_unstarted(self, reason: String) -> Result<(), String> {
        self.0
            .reject_unstarted(crate::owner_commit::execution_deadline(), reason)
    }
    pub(crate) fn diagnostic_timing(&self) -> Timing {
        self.1.child("owner_signature")
    }
    #[cfg(feature = "test-support")]
    pub fn matches_route(&self, app: &str, record: &str) -> bool {
        self.0.matches_route(app, record)
    }
    pub fn apply(self, owner: &mut SessionOwner) -> (impl FnOnce() + use<>, Result<(), String>) {
        let timing = self.1;
        let policy_time = self.2;
        timing.mark("owner_dequeued");
        self.0.apply_scoped_fenced(
            crate::owner_commit::execution_deadline(),
            |route, certificate, mutation, authorization| {
                if let Some(authorization) = authorization {
                    let time = policy_time.as_ref().ok_or_else(|| HostDocumentTurnError::Refused("startup replay requires original registered owner time".into()))?;
                    match owner.replay_exact_startup_document(route, certificate, &mutation, authorization, time) {
                        Ok(replayed) => return Ok(replayed),
                        // A mounted worker republishes merged remote history
                        // (browser/relay uploads) under required custody. With
                        // no contributor and only already-committed changes it
                        // is no invocation effect: sign it through the ordinary
                        // owner door below. Every other body keeps its refusal.
                        Err(replay_error) => {
                            if let Err(echo_error) = owner.validate_committed_echo(route, &mutation, authorization) {
                                tracing::debug!(%replay_error, %echo_error, "scoped host document refused");
                                return Err(HostDocumentTurnError::Refused(replay_error));
                            }
                        }
                    }
                }
                let now = match &policy_time {
                    Some(time) => web_time::SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(time.now_ns().map_err(|error| HostDocumentTurnError::Refused(error.to_string()))?),
                    None => web_time::SystemTime::now(),
                };
                owner
                    .require_owner_resource_scope()
                    .map_err(|e| e.to_string())?;
                let record = route.record.as_deref().unwrap_or("");
                if owner.peer().room() != route.cluster_id.to_string()
                    || certificate != &owner.identity.native_cert
                    || !owner.peer().certificates().contains(certificate)
                    || owner.peer().document(&route.app_id, record).is_none()
                    || !owner.permits_publication()
                {
                    return Err("host signer route/certificate is no longer admitted".into());
                }
                owner
                    .peer()
                    .permissions(certificate.device_public_key, now)?;
                let mut candidate = owner.peer().clone();
                let NeighbourMsg::AppDocument { envelope, .. } = candidate.signed_mutation(
                    &route.app_id,
                    record,
                    mutation,
                    &owner.identity.native,
                    now,
                )?
                else {
                    return Err("owner did not sign an application document".into());
                };
                let request = OwnerCommitRequest {
                    room: owner.peer().room().into(),
                    operation: CommitOperation::Document {
                        app: route.app_id.clone(),
                        record: record.into(),
                        envelope: envelope.clone(),
                    },
                };
                timing.mark("admission_start");
                if let Some(time) = &policy_time {
                    let now = web_time::SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(time.now_ns().map_err(|error| HostDocumentTurnError::Refused(error.to_string()))?);
                    // Revalidate the unchanged signed envelope against the
                    // genuine pre-commit owner. Legacy owners need not have a
                    // V5 exact index; the normal local door owns these checks.
                    let outcome = owner.peer().clone().admit_local_app_document(
                        &route.app_id, record, *envelope.clone(), now,
                    )?;
                    if outcome.deferred || outcome.incomplete.is_some() {
                        return Err("signed owner mutation lost current admission".into());
                    }
                }
                let committed = match &policy_time {
                    Some(time) => owner.commit_mutation_candidate_with_policy(candidate, owner.directory().to_vec(), crate::owner::OwnerPolicyFence::HostSigning(HostSigningPolicyFence {
                        time: time.clone(), request: request.clone(), certificate: certificate.clone(),
                    })),
                    None => owner.commit_mutation_candidate(candidate, owner.directory().to_vec()),
                }.map_err(|error| {
                    if error.downcast_ref::<crate::owner::OwnerPolicyRefused>().is_some() {
                        HostDocumentTurnError::Refused(error.to_string())
                    } else { HostDocumentTurnError::Uncertain(error.to_string()) }
                })?;
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
                Ok((*envelope, receipt))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner_commit::OwnerCommitMailbox;
    use qos_action_broker::host_documents::LIMIT;
    #[tokio::test]
    async fn retained_policy_signing_final_refusal_is_zero_document_change_not_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let before = owner.status();
        let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
        let (service, mut signatures) = HostSigning::new(ordinary);
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        service
            .register_policy_time(origin.capture().unwrap())
            .unwrap();
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = signatures.recv().await.unwrap();
        let (complete, outcome) = crate::owner::policy_fence::with_before_check(
            move || origin.invalidate(),
            || pending.apply(&mut owner),
        );
        assert!(outcome.is_err());
        complete();
        assert!(waiting.await.unwrap().is_err());
        assert_eq!(owner.status(), before);
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 0);
        assert!(service.retirement_ready().unwrap());
    }
    #[tokio::test]
    async fn retained_policy_signing_failed_write_remains_uncertain() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let before = owner.status();
        let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
        let (service, mut signatures) = HostSigning::new(ordinary);
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        service
            .register_policy_time(origin.capture().unwrap())
            .unwrap();
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = signatures.recv().await.unwrap();
        let (complete, outcome) = crate::store::test_fault::with_fault(
            &path,
            crate::store::test_fault::Point::AfterAtomicWrite,
            || pending.apply(&mut owner),
        );
        assert!(outcome.is_err());
        // Retiring the source after Store was called cannot reclassify I/O as
        // a definite pre-write refusal or clear the intrinsic failure latch.
        origin.invalidate();
        complete();
        assert!(waiting.await.unwrap().is_err());
        assert!(!owner.durability_healthy());
        assert!(service.retirement_ready().is_err());
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 0);
        drop(owner);
        let reopened = SessionOwner::open(&path).unwrap();
        assert_eq!(
            reopened.status().generation,
            before.generation + 1,
            "complete new state may exist despite no acknowledged signing receipt"
        );
        assert!(service.retirement_ready().is_err());
    }
    #[tokio::test]
    async fn policy_retirement_invalid_origin_refuses_waiting_and_late_signers() {
        async fn case(close: bool) {
            let root = tempfile::tempdir().unwrap();
            let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
            let before = owner.status();
            let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
            let (service, mut signatures) = HostSigning::new(ordinary);
            let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
            let origin = clock.new_origin();
            service
                .register_policy_time(origin.capture().unwrap())
                .unwrap();
            let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
            let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
            let mutation =
                DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
            let mut queued = Vec::new();
            let mut replies = Vec::new();
            for _ in 0..LIMIT {
                let mut reply = Box::pin(tokio::task::unconstrained(
                    signer.commit(&route, mutation.clone()),
                ));
                assert!(
                    std::future::Future::poll(
                        reply.as_mut(),
                        &mut std::task::Context::from_waker(std::task::Waker::noop())
                    )
                    .is_pending()
                );
                queued.push(signatures.try_recv().unwrap());
                replies.push(reply);
            }
            let mut blocked = Box::pin(tokio::task::unconstrained(
                signer.commit(&route, mutation.clone()),
            ));
            assert!(
                std::future::Future::poll(
                    blocked.as_mut(),
                    &mut std::task::Context::from_waker(std::task::Waker::noop())
                )
                .is_pending()
            );
            if close {
                service.close_admission().unwrap();
            } else {
                origin.invalidate();
            }
            for pending in queued {
                pending
                    .reject_unstarted("old source retired".into())
                    .unwrap();
            }
            for reply in replies {
                assert!(reply.await.is_err());
            }
            let result = std::future::Future::poll(
                blocked.as_mut(),
                &mut std::task::Context::from_waker(std::task::Waker::noop()),
            );
            let clean = matches!(result, std::task::Poll::Ready(Err(_)));
            if let Ok(pending) = signatures.try_recv() {
                pending
                    .reject_unstarted("cleanup unexpected late queue".into())
                    .unwrap();
            }
            assert!(
                clean,
                "signer waiting on capacity must refuse before entering retired source queue"
            );
            assert!(signer.commit(&route, mutation).await.is_err());
            assert!(signatures.try_recv().is_err());
            assert!(service.retirement_ready().unwrap());
            assert_eq!(owner.status(), before);
        }
        case(false).await;
        case(true).await;
    }
    #[tokio::test]
    async fn policy_retirement_rejects_queued_document_without_owner_effect_or_health_reset() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let before = owner.status();
        let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
        let (service, mut signatures) = HostSigning::new(ordinary);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation.clone()));
        signatures
            .recv()
            .await
            .unwrap()
            .reject_unstarted("old policy origin".into())
            .unwrap();
        assert_eq!(waiting.await.unwrap().unwrap_err(), "old policy origin");
        assert!(service.retirement_ready().unwrap());
        assert_eq!(owner.status(), before);

        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let pending = signatures.recv().await.unwrap();
        service.fail("preexisting signing failure");
        assert!(pending.reject_unstarted("old origin".into()).is_err());
        assert!(waiting.await.unwrap().is_err());
        assert_eq!(service.health().unwrap_err(), "preexisting signing failure");
        assert_eq!(owner.status(), before);
    }
    // This catches a shared adapter that acknowledges before the durable owner
    // barrier, drops the exact binding, or admits the same document twice.
    #[tokio::test]
    async fn owner_neutral_handoff_uses_actual_native_barrier() {
        use qos_action_broker::host_documents::HostDocuments;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owner");
        let mut owner = SessionOwner::initialize(&path).unwrap();
        let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
        let (service, mut pending) = HostDocuments::new(Arc::new(ordinary));
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        let request = pending.recv().await.unwrap();
        let before = owner.status().generation;
        let (complete, outcome) = request.apply(
            crate::owner_commit::execution_deadline(),
            |route, cert, mutation| {
                assert_eq!(cert, &owner.identity.native_cert);
                let mut candidate = owner.peer().clone();
                let NeighbourMsg::AppDocument { envelope, .. } = candidate.signed_mutation(
                    &route.app_id,
                    "",
                    mutation,
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )?
                else {
                    return Err("expected signed document".into());
                };
                let request = OwnerCommitRequest {
                    room: owner.peer().room().into(),
                    operation: CommitOperation::Document {
                        app: route.app_id.clone(),
                        record: "".into(),
                        envelope: envelope.clone(),
                    },
                };
                let committed = owner
                    .commit_mutation_candidate(candidate, owner.directory().to_vec())
                    .map_err(|e| e.to_string())?;
                Ok((
                    *envelope,
                    OwnerCommitReceipt {
                        binding: request.binding()?,
                        generation: committed.generation,
                        aggregate_hash: hex::decode(committed.aggregate_hash)
                            .unwrap()
                            .try_into()
                            .unwrap(),
                    },
                ))
            },
        );
        outcome.unwrap();
        assert!(
            !waiting.is_finished(),
            "durability alone cannot release the deferred response"
        );
        assert_eq!(owner.status().generation, before + 1);
        complete();
        let op = waiting.await.unwrap().unwrap();
        let request = super::tests::request(&owner, "dev.qos.chat", "", op);
        let receipt = service.commit(request.clone()).await.unwrap();
        assert_eq!(receipt.generation, before + 1);
        let committed = owner.status();
        assert!(service.commit(request).await.is_err());
        assert_eq!(owner.status(), committed);
        drop(owner);
        assert_eq!(SessionOwner::open(&path).unwrap().status(), committed);
    }
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
        receiver: &mut SignatureMailbox,
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
    #[tokio::test]
    async fn policy_retirement_cannot_discard_a_committed_exact_receipt() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (ordinary, _mailbox) = OwnerCommitMailbox::bounded(8);
        let (service, mut signatures) = HostSigning::new(ordinary);
        let envelope = precommit(&service, &mut signatures, &mut owner, "dev.qos.chat", "").await;
        let committed = owner.status();
        assert!(!service.retirement_ready().unwrap());
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let waiting = tokio::spawn(signer.commit(&route, mutation));
        signatures
            .recv()
            .await
            .unwrap()
            .reject_unstarted("old policy origin".into())
            .unwrap();
        assert!(waiting.await.unwrap().is_err());
        assert_eq!(owner.status(), committed);
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
        assert!(!service.retirement_ready().unwrap());
        service
            .commit(request(&owner, "dev.qos.chat", "", envelope))
            .await
            .unwrap();
        assert!(service.retirement_ready().unwrap());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_signing_interleaved_exact_receipts_preserve_floor_and_no_replay_exception() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let baseline = outgoing_bytes(&owner).to_vec();
        let chat_receipt = owner.peer().durable_coverage("dev.qos.chat", "").unwrap();
        let writer = precommit(
            &service,
            &mut receiver,
            &mut owner,
            "dev.qos.word-processor",
            "tab:0000000000000001",
        )
        .await;
        assert!(outgoing_bytes(&owner).starts_with(&baseline));
        assert_eq!(
            outgoing_bytes(&owner).len() - baseline.len(),
            2,
            "signer path publishes only its touched route"
        );
        assert_eq!(
            owner.peer().durable_coverage("dev.qos.chat", "").unwrap(),
            chat_receipt
        );
        assert_eq!(
            &outgoing_kinds(&owner)[baseline.len()..],
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
        assert_eq!(LIMIT - service.shared.outstanding_handoffs(), LIMIT);
        // Consumed receipt never creates a replay exemption on another call.
        let outgoing = outgoing_bytes(&owner).to_vec();
        assert!(service.commit(request_writer).await.is_err());
        assert_eq!(outgoing_bytes(&owner), outgoing);
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
        let outgoing = outgoing_bytes(&owner).to_vec();
        let status = owner.status();
        assert!(
            service
                .commit(request(&owner, "dev.qos.chat", "", op))
                .await
                .is_err()
        );
        assert_eq!(outgoing_bytes(&owner), outgoing);
        assert_eq!(owner.status(), status);
        assert_eq!(owner.status().generation, generation);
        assert_eq!(
            service.shared.pending_exact_receipts().unwrap(),
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
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
        let waiting = tokio::spawn(signer.commit(
            &route,
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap()),
        ));
        let pending = receiver.recv().await.unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        let before = owner.status().generation;
        let (complete, result) = pending.apply(&mut owner);
        result.unwrap();
        complete();
        assert!(owner.status().generation > before);
        assert!(service.health().unwrap_err().contains("outcome unknown"));
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
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
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
        complete();
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
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
        let (_complete, _) = pending.0.apply(
            std::time::Instant::now() - std::time::Duration::from_millis(1),
            |_, _, _| Err("elapsed owner turn".into()),
        );
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
        assert_eq!(LIMIT - service.shared.outstanding_handoffs(), 0);
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
        assert_eq!(LIMIT - service.shared.outstanding_handoffs(), 0);

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
        assert_eq!(LIMIT - service.shared.outstanding_handoffs(), LIMIT);
        assert!(service.health().is_ok());
    }

    #[tokio::test]
    async fn signing_failure_wakes_request_waiting_for_receipt_capacity() {
        let root = tempfile::tempdir().unwrap();
        let owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let (handle, _ordinary) = OwnerCommitMailbox::bounded(8);
        let (service, mut receiver) = HostSigning::new(handle);
        let route = RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap());
        let mutation =
            DocumentMutation::Snapshot(owner.peer().document("dev.qos.chat", "").unwrap());
        let mut held = Vec::new();
        let mut queued = Vec::new();
        for _ in 0..LIMIT {
            let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
            queued.push(tokio::spawn(signer.commit(&route, mutation.clone())));
            held.push(receiver.recv().await.unwrap());
        }
        let signer = service.bind(route.clone(), owner.identity.native_cert.clone());
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
        for task in queued {
            assert!(task.await.unwrap().is_err());
        }
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
        assert_eq!(service.shared.pending_exact_receipts().unwrap(), 1);
        let committed = owner.status();
        drop(owner);
        assert_eq!(SessionOwner::open(&state).unwrap().status(), committed);
    }
}

#[cfg(test)]
fn outgoing_bytes(owner: &crate::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
#[cfg(test)]
fn outgoing_kinds(owner: &crate::SessionOwner) -> Vec<qos_peer_state::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}
