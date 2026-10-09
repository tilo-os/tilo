//! Exact bounded precommit handoffs for host-owned application signers.
//! The serialized durable owner keeps keys and state. This module retains only
//! its exact one-use receipt until the ordinary broker acknowledges the document.
use crate::peer_actions::PolicyTime;
use collab_types::{DocumentMutation, SignedOp};
use std::sync::{Arc, Mutex, OnceLock};
use tilo_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
};
use tilo_user_identity::DeviceCert;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot as response};
use ui::collaboration::{HostDocumentCommit, HostDocumentCommitter, RouteSpec};

pub const LIMIT: usize = 32;
/// Only an actual owner refusal before any persistence write is definite.
/// This classifies this document turn, never the whole originating invocation.
pub enum HostDocumentTurnError {
    Refused(String),
    Uncertain(String),
}
impl From<String> for HostDocumentTurnError {
    fn from(error: String) -> Self {
        Self::Uncertain(error)
    }
}
impl From<&str> for HostDocumentTurnError {
    fn from(error: &str) -> Self {
        Self::Uncertain(error.into())
    }
}
struct Handoff {
    bytes: Vec<u8>,
    receipt: OwnerCommitReceipt,
    _permit: OwnedSemaphorePermit,
}
#[derive(Default)]
struct State {
    pending: Vec<Handoff>,
    failure: Option<String>,
    /// Every recorded failure was a started owner turn that stalled.
    only_stalls: bool,
    admission_closed: bool,
}
#[derive(Clone)]
pub struct HostDocuments {
    sender: mpsc::Sender<PendingHostDocument>,
    state: Arc<Mutex<State>>,
    permits: Arc<Semaphore>,
    ordinary: Arc<dyn OwnerCommitService>,
    policy_time: Arc<OnceLock<Option<PolicyTime>>>,
}
pub struct PendingHostDocument {
    route: RouteSpec,
    certificate: DeviceCert,
    mutation: DocumentMutation,
    authorization: Option<ui::invocation_scope::DocumentAuthorization>,
    started: response::Sender<crate::owner_turn::TurnWatch>,
    reply: response::Sender<Result<SignedOp, String>>,
    service: HostDocuments,
    permit: OwnedSemaphorePermit,
}
struct BoundSigner {
    service: HostDocuments,
    route: RouteSpec,
    certificate: DeviceCert,
}
impl HostDocuments {
    pub fn new(
        ordinary: Arc<dyn OwnerCommitService>,
    ) -> (Self, mpsc::Receiver<PendingHostDocument>) {
        let (sender, receiver) = mpsc::channel(LIMIT);
        (
            Self {
                sender,
                state: Arc::default(),
                permits: Arc::new(Semaphore::new(LIMIT)),
                ordinary,
                policy_time: Arc::new(OnceLock::new()),
            },
            receiver,
        )
    }
    pub fn bind(
        &self,
        route: RouteSpec,
        certificate: DeviceCert,
    ) -> Arc<dyn HostDocumentCommitter> {
        self.policy_time.get_or_init(|| None);
        Arc::new(BoundSigner {
            service: self.clone(),
            route,
            certificate,
        })
    }
    /// Stop producers atomically with the final queue admission. Exact receipts
    /// and intrinsic health remain available for settlement after retirement.
    ///
    /// # Errors
    ///
    /// Returns a message when the handoff state is unavailable.
    pub fn close_admission(&self) -> Result<(), String> {
        self.state
            .lock()
            .map_err(|_| "host handoff unavailable")?
            .admission_closed = true;
        self.permits.close();
        Ok(())
    }
    fn check_admission(&self, state: &State) -> Result<(), String> {
        if state.admission_closed {
            return Err("host signing admission retired".into());
        }
        if let Some(time) = self.policy_time.get().and_then(Option::as_ref) {
            time.now_ns().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    fn admission(&self) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "host handoff unavailable")?;
        self.check_admission(&state)
    }
    pub fn fail(&self, reason: impl Into<String>) -> String {
        self.record_failure(reason.into(), false)
    }
    /// Fail closed because a started owner turn stopped making progress.
    fn fail_stalled(&self, reason: impl Into<String>) -> String {
        self.record_failure(reason.into(), true)
    }
    fn record_failure(&self, reason: String, stalled: bool) -> String {
        if let Ok(mut state) = self.state.lock() {
            state.only_stalls = if state.failure.is_none() {
                stalled
            } else {
                state.only_stalls && stalled
            };
            state.failure.get_or_insert_with(|| reason.clone());
        }
        self.permits.close();
        reason
    }
    /// This service's own latched failure, and whether it came only from
    /// stalled owner turns. A stall is resolved once the serialized owner is
    /// running again: the stalled turn has then finished.
    ///
    /// # Errors
    ///
    /// Returns a message when the handoff state is unavailable.
    pub fn own_failure(&self) -> Result<Option<(String, bool)>, String> {
        let state = self.state.lock().map_err(|_| "host handoff unavailable")?;
        Ok(state
            .failure
            .clone()
            .map(|reason| (reason, state.only_stalls)))
    }
}
impl HostDocumentCommitter for BoundSigner {
    fn commit(&self, route: &RouteSpec, mutation: DocumentMutation) -> HostDocumentCommit {
        self.enqueue(route, mutation, None)
    }
    fn commit_scoped(
        &self,
        route: &RouteSpec,
        mutation: DocumentMutation,
        authorization: ui::invocation_scope::DocumentAuthorization,
    ) -> HostDocumentCommit {
        self.enqueue(route, mutation, Some(authorization))
    }
}
impl BoundSigner {
    fn enqueue(
        &self,
        route: &RouteSpec,
        mutation: DocumentMutation,
        authorization: Option<ui::invocation_scope::DocumentAuthorization>,
    ) -> HostDocumentCommit {
        let service = self.service.clone();
        let expected_route = self.route.clone();
        let certificate = self.certificate.clone();
        let route = route.clone();
        Box::pin(async move {
            service.admission()?;
            service.health()?;
            if route != expected_route {
                return Err(service.fail("worker attempted a different owner route"));
            }
            if let Some(authorization) = &authorization {
                authorization.check_binding(&route, &mutation)?;
            }
            if mutation.encode().map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
                return Err(service.fail("host mutation exceeds bounded payload"));
            }
            let permit = service.permits.clone().acquire_owned().await.map_err(|_| {
                service
                    .admission()
                    .err()
                    .or_else(|| service.health().err())
                    .unwrap_or_else(|| {
                        service.fail("host signing handoff unavailable; outcome unknown")
                    })
            })?;
            // A previous in-flight request may have retired the service while this
            // request was waiting for exact receipt capacity.
            service.admission()?;
            service.health()?;
            let (reply, receiver) = response::channel();
            let (started, start) = response::channel();
            let sent = {
                let state = service
                    .state
                    .lock()
                    .map_err(|_| "host handoff unavailable")?;
                service.check_admission(&state)?;
                // The admission fence stays held until the request is queued.
                let sent = service.sender.try_send(PendingHostDocument {
                    route,
                    certificate,
                    mutation,
                    authorization,
                    started,
                    reply,
                    service: service.clone(),
                    permit,
                });
                drop(state);
                // A refused request is dropped here, after the fence is released.
                sent.is_ok()
            };
            // The shared permit ceiling is no larger than queue capacity, so
            // this send never needs another await after the admission fence.
            if !sent {
                return Err(service.fail("host signer unavailable; outcome unknown"));
            }
            // Reconnect/replay may leave this bounded request queued indefinitely.
            // Only the ready serialized owner turn starts, and bounds, its turn.
            let turn = start
                .await
                .map_err(|_| service.fail("host signing reply lost; outcome unknown"))?;
            match turn.wait(receiver).await {
                crate::owner_turn::TurnWait::Answered(result) => result,
                crate::owner_turn::TurnWait::Lost => {
                    Err(service.fail("host signing reply lost; outcome unknown"))
                }
                crate::owner_turn::TurnWait::Stalled => {
                    Err(service.fail_stalled("host signing turn stalled; outcome unknown"))
                }
            }
        })
    }
}

impl OwnerCommitService for HostDocuments {
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
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the matched handoff is consumed by its receipt check; the lint misreads the move"
    )]
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let this = self.clone();
        Box::pin(async move {
            this.health()?;
            let bytes = postcard::to_stdvec(&request).map_err(|e| this.fail(e.to_string()))?;
            let matched = {
                let mut state = this.state.lock().map_err(|_| "host handoff unavailable")?;
                let matched = state
                    .pending
                    .iter()
                    .position(|entry| entry.bytes == bytes)
                    .map(|i| state.pending.remove(i));
                drop(state);
                matched
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

impl PendingHostDocument {
    pub fn matches_route(&self, app: &str, record: &str) -> bool {
        self.route.app_id == app && self.route.record.as_deref().unwrap_or("") == record
    }
    /// Enter only in the ready serialized owner turn. The callback must return
    /// the exact signed envelope and receipt after its actual durability barrier.
    /// Invoke completion after refreshing committed authority and publication.
    pub fn apply(
        self,
        budget: std::time::Duration,
        commit: impl FnOnce(
            &RouteSpec,
            &DeviceCert,
            DocumentMutation,
        ) -> Result<(SignedOp, OwnerCommitReceipt), String>,
    ) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
        self.apply_fenced(budget, |route, certificate, mutation| {
            commit(route, certificate, mutation).map_err(HostDocumentTurnError::Uncertain)
        })
    }
    pub fn apply_fenced(
        self,
        budget: std::time::Duration,
        commit: impl FnOnce(
            &RouteSpec,
            &DeviceCert,
            DocumentMutation,
        ) -> Result<(SignedOp, OwnerCommitReceipt), HostDocumentTurnError>,
    ) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
        self.apply_scoped_fenced(budget, |route, certificate, mutation, authorization| {
            if authorization.is_some() {
                return Err(HostDocumentTurnError::Refused(
                    "host has no final scoped document evaluator".into(),
                ));
            }
            commit(route, certificate, mutation)
        })
    }
    /// The concrete owner must validate scoped custody under its existing turn.
    /// Merely carrying this metadata never authorizes mutation or replay.
    pub fn apply_scoped_fenced(
        self,
        budget: std::time::Duration,
        commit: impl FnOnce(
            &RouteSpec,
            &DeviceCert,
            DocumentMutation,
            Option<&ui::invocation_scope::DocumentAuthorization>,
        ) -> Result<(SignedOp, OwnerCommitReceipt), HostDocumentTurnError>,
    ) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
        let Self {
            route,
            certificate,
            mutation,
            authorization,
            started,
            reply,
            service,
            permit,
        } = self;
        // Losing the waiting caller does not cancel an admitted owner turn.
        // The owner bounds its own turn: each completed step renews `budget`.
        let (progress, turn) = crate::owner_turn::start(budget);
        let _ = started.send(turn);
        let result: Result<SignedOp, HostDocumentTurnError> = (|| {
            service.health()?;
            let (envelope, receipt) =
                commit(&route, &certificate, mutation, authorization.as_ref())?;
            progress.advance();
            let request = OwnerCommitRequest {
                room: route.cluster_id.to_string(),
                operation: CommitOperation::Document {
                    app: route.app_id,
                    record: route.record.unwrap_or_default(),
                    envelope: Box::new(envelope.clone()),
                },
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
            Ok(envelope)
        })();
        if let Err(HostDocumentTurnError::Uncertain(reason)) = &result {
            service.fail(reason.clone());
        }
        let result = result.map_err(|error| match error {
            HostDocumentTurnError::Refused(reason) | HostDocumentTurnError::Uncertain(reason) => {
                reason
            }
        });
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        let complete = move || {
            // The owner turn already finished with a definite result and, on
            // success, an exact receipt. Dropping the waiting worker commit
            // (for example because that worker unmounted) must not latch the
            // whole signer and block every other route.
            if reply.send(result).is_err() {
                tracing::warn!(
                    "host signer response dropped after admission; owner outcome retained"
                );
            }
            // The turn ends only once it has answered.
            drop(progress);
        };
        (Box::new(complete), outcome)
    }
}
