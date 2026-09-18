//! Exact bounded precommit handoffs for owner-bound application signers.
//! No sequence reservation escapes the serialized durable owner turn.
use crate::{SessionOwner, owner_commit::OwnerCommitHandle};
use collab_types::{DocumentMutation, SignedOp};
use qos_peer_state::{
    NeighbourMsg,
    owner_commit::{
        CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
    },
};
use std::sync::{Arc, Mutex, mpsc as response};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use ui::collaboration::{HostDocumentCommitter, RouteSpec};

const LIMIT: usize = 32;
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
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
    certificate: qos_user_identity::DeviceCert,
    mutation: DocumentMutation,
    reply: response::Sender<Result<SignedOp, String>>,
    service: HostSigning,
    permit: OwnedSemaphorePermit,
}
struct BoundSigner {
    service: HostSigning,
    route: RouteSpec,
    certificate: qos_user_identity::DeviceCert,
}
impl HostSigning {
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
        certificate: qos_user_identity::DeviceCert,
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
        reason
    }
}
impl HostDocumentCommitter for BoundSigner {
    fn commit(&self, route: &RouteSpec, mutation: DocumentMutation) -> Result<SignedOp, String> {
        self.service.health()?;
        if route != &self.route {
            return Err(self
                .service
                .fail("worker attempted a different owner route"));
        }
        if mutation.encode().map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err(self.service.fail("host mutation exceeds bounded payload"));
        }
        let permit = self
            .service
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                self.service
                    .fail("host signing handoff capacity exhausted; outcome unknown")
            })?;
        let (reply, receiver) = response::channel();
        self.service
            .sender
            .try_send(PendingSignature {
                route: route.clone(),
                certificate: self.certificate.clone(),
                mutation,
                reply,
                service: self.service.clone(),
                permit,
            })
            .map_err(|_| {
                self.service
                    .fail("host signer unavailable; outcome unknown")
            })?;
        receiver.recv_timeout(DEADLINE).map_err(|_| {
            self.service
                .fail("host signing reply lost; outcome unknown")
        })?
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
            reply,
            service,
            permit,
        } = self;
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
            let committed = owner
                .commit_admission(candidate, owner.directory().to_vec())
                .map_err(|e| e.to_string())?;
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
        let complete = move || {
            if reply.send(result).is_err() {
                service.fail("host signer response dropped after admission; outcome unknown");
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
        let waiting = tokio::task::spawn_blocking(move || signer.commit(&route, mutation));
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
        let writer = precommit(
            &service,
            &mut receiver,
            &mut owner,
            "dev.qos.word-processor",
            "tab:0000000000000001",
        )
        .await;
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
        drop(lost);
        let pending = PendingSignature {
            route: RouteSpec::app("dev.qos.chat", owner.peer().room().parse().unwrap()),
            certificate: owner.identity.native_cert.clone(),
            mutation: DocumentMutation::Snapshot(
                owner.peer().document("dev.qos.chat", "").unwrap(),
            ),
            reply,
            service: service.clone(),
            permit: service.permits.clone().try_acquire_owned().unwrap(),
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
}
