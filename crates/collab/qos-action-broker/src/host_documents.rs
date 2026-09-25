//! Exact bounded precommit handoffs for host-owned application signers.
//! The serialized durable owner keeps keys and state. This module retains only
//! its exact one-use receipt until the ordinary broker acknowledges the document.
use crate::peer_actions::PolicyTime;
use collab_types::{DocumentMutation, SignedOp};
use qos_peer_state::owner_commit::{
    CommitFuture, CommitOperation, OwnerCommitReceipt, OwnerCommitRequest, OwnerCommitService,
};
use std::sync::{Arc, Mutex, OnceLock};
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
    started: response::Sender<std::time::Instant>,
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
    pub fn register_policy_time(&self, time: PolicyTime) -> Result<(), String> {
        time.now_ns().map_err(|error| error.to_string())?;
        let registered = self.policy_time.get_or_init(|| Some(time.clone()));
        if registered
            .as_ref()
            .is_none_or(|registered| !registered.same_origin(&time))
        {
            return Err("host documents already belong to another policy lifetime".into());
        }
        Ok(())
    }
    /// Stop producers atomically with the final queue admission. Exact receipts
    /// and intrinsic health remain available for settlement after retirement.
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
        let reason = reason.into();
        if let Ok(mut state) = self.state.lock() {
            state.failure.get_or_insert(reason.clone());
        }
        self.permits.close();
        reason
    }
    pub fn outstanding_handoffs(&self) -> usize {
        LIMIT.saturating_sub(self.permits.available_permits())
    }
    pub fn pending_exact_receipts(&self) -> Result<usize, String> {
        Ok(self
            .state
            .lock()
            .map_err(|_| "host handoff unavailable")?
            .pending
            .len())
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
                service.sender.try_send(PendingHostDocument {
                    route,
                    certificate,
                    mutation,
                    authorization,
                    started,
                    reply,
                    service: service.clone(),
                    permit,
                })
            };
            // The shared permit ceiling is no larger than queue capacity, so
            // this send never needs another await after the admission fence.
            sent.map_err(|_| service.fail("host signer unavailable; outcome unknown"))?;
            // Reconnect/replay may leave this bounded request queued indefinitely.
            // Only the ready serialized owner turn starts its execution deadline.
            let deadline = start
                .await
                .map_err(|_| service.fail("host signing reply lost; outcome unknown"))?;
            let response = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                receiver,
            )
            .await;
            match response {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(service.fail("host signing reply lost; outcome unknown")),
                Err(_) => Err(service.fail("host signing reply lost; outcome unknown")),
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

impl PendingHostDocument {
    /// This value exists only before apply consumes it. Started completion and
    /// postcommit receipt custody have different types and cannot enter here.
    pub fn reject_unstarted(
        self,
        deadline: std::time::Instant,
        reason: String,
    ) -> Result<(), String> {
        let health = self.service.health();
        let reason = health.as_ref().err().cloned().unwrap_or(reason);
        let _ = self.started.send(deadline);
        let _ = self.reply.send(Err(reason));
        // A refused, never-started handoff owns no receipt or owner effect.
        // Its existing permit is released; prior service failure stays latched.
        drop(self.permit);
        health
    }
    pub fn matches_route(&self, app: &str, record: &str) -> bool {
        self.route.app_id == app && self.route.record.as_deref().unwrap_or("") == record
    }
    /// Enter only in the ready serialized owner turn. The callback must return
    /// the exact signed envelope and receipt after its actual durability barrier.
    /// Invoke completion after refreshing committed authority and publication.
    pub fn apply(
        self,
        deadline: std::time::Instant,
        commit: impl FnOnce(
            &RouteSpec,
            &DeviceCert,
            DocumentMutation,
        ) -> Result<(SignedOp, OwnerCommitReceipt), String>,
    ) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
        self.apply_fenced(deadline, |route, certificate, mutation| {
            commit(route, certificate, mutation).map_err(HostDocumentTurnError::Uncertain)
        })
    }
    pub fn apply_fenced(
        self,
        deadline: std::time::Instant,
        commit: impl FnOnce(
            &RouteSpec,
            &DeviceCert,
            DocumentMutation,
        ) -> Result<(SignedOp, OwnerCommitReceipt), HostDocumentTurnError>,
    ) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
        self.apply_scoped_fenced(deadline, |route, certificate, mutation, authorization| {
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
        deadline: std::time::Instant,
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
        let _ = started.send(deadline);
        let result: Result<SignedOp, HostDocumentTurnError> = (|| {
            service.health()?;
            let (envelope, receipt) =
                commit(&route, &certificate, mutation, authorization.as_ref())?;
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
            if reply.send(result).is_err() {
                service.fail("host signer response dropped after admission; outcome unknown");
            }
        };
        (Box::new(complete), outcome)
    }
}
