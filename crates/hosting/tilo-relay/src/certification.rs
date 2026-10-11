//! Host side of relay-mediated device certification
//! ([`crate::certify`], ALPN `tilo-device-certify/1`).
//!
//! The host only carries and checks. A requester's signed
//! [`CertificationRequest`] waits here until an approver — a device of the
//! host's user with `can_admit_devices` — answers it with a root-signed
//! certificate or a denial. The host checks the certificate against the exact
//! request, the [`CertificationPolicy`] and the live revocation list before
//! handing it on, so an approver bug cannot certify another key, escalate
//! capabilities or resurrect a revoked device.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use tilo_clock::unix_now_ns;
use tilo_user_identity::{
    CertificationPolicy, CertificationRequest, DeviceCert, RevocationList, verify_issued_cert,
};
use tokio::sync::watch;

use crate::auth::{self, HandshakeTrust};
use crate::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, CertifyOpen, CertifyReply,
    MAX_CERTIFY_FRAME_BYTES, MAX_PENDING_CERTIFICATIONS, PendingAgent, PendingCertification,
};
use crate::frame::{read_frame, read_frame_or_eof, write_frame};
use crate::handshake::{EndpointTrust, LiveIdentity};
use tilo_relay_protocol::agents::{AgentDelegation, AgentProposal};

/// How long a final answer may wait for the requester to read it.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// One request, and its answer once an approver gave it. A decided request
/// stays until its requester collected the answer or it expired, so an HTTPS
/// requester that polls later still receives it.
struct Pending {
    entry: PendingCertification,
    answer: watch::Sender<Option<CertifyReply>>,
    /// Set for a request the host queued for an agent it supervises.
    agent: Option<AgentProposal>,
    /// The approved delegation of such a request, once decided.
    delegation: Option<AgentDelegation>,
}

impl Pending {
    fn decided(&self) -> bool {
        self.answer.borrow().is_some()
    }
}

/// A queued request: its digest, pairing code and a receiver of its answer.
type Queued = ([u8; 32], String, watch::Receiver<Option<CertifyReply>>);

/// The certification ALPN handler.
#[derive(Clone)]
pub struct CertificationProtocol {
    identity: LiveIdentity,
    policy: CertificationPolicy,
    pending: Arc<Mutex<HashMap<[u8; 32], Pending>>>,
}

impl std::fmt::Debug for CertificationProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CertificationProtocol")
    }
}

fn io(error: &impl ToString) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(error.to_string()))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, AcceptError> {
    postcard::to_allocvec(value).map_err(|error| io(&error))
}

impl CertificationProtocol {
    pub(crate) fn new(identity: LiveIdentity) -> Self {
        Self {
            identity,
            policy: CertificationPolicy::browser(),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn revocations(&self) -> RevocationList {
        self.identity
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admit a request into the pending set, or say why not. Returns its
    /// digest, pairing code and a receiver that sees the answer.
    ///
    /// # Errors
    ///
    /// Returns the reason the request is refused.
    pub fn enqueue(&self, request: CertificationRequest, now_ns: u64) -> Result<Queued, String> {
        self.enqueue_with(request, None, now_ns)
    }

    /// Queue a request for an agent this host supervises, carrying what
    /// the person proposed. Only an `IssueAgent` decision answers it; wait
    /// for it with [`Self::agent_answer`].
    ///
    /// # Errors
    ///
    /// Returns the reason the request is refused.
    pub fn enqueue_agent(
        &self,
        request: CertificationRequest,
        proposal: AgentProposal,
        now_ns: u64,
    ) -> Result<([u8; 32], String), String> {
        if proposal.agent_id.is_empty() || proposal.scope.rooms.is_empty() {
            return Err("an agent proposal names its agent and rooms".into());
        }
        self.enqueue_with(request, Some(proposal), now_ns)
            .map(|(digest, code, _)| (digest, code))
    }

    /// The approver's answer to agent request `digest`: its certificate and
    /// delegation, or why there are none. Waits until decided or expired.
    ///
    /// # Errors
    ///
    /// Returns the denial, or that the request expired or is unknown.
    pub async fn agent_answer(
        &self,
        digest: &[u8; 32],
    ) -> Result<(DeviceCert, AgentDelegation), String> {
        let (receiver, expires_at_ns) = self
            .lock()
            .get(digest)
            .map(|entry| (entry.answer.subscribe(), entry.entry.request.expires_at_ns))
            .ok_or("no such agent request")?;
        let wait = Duration::from_nanos(expires_at_ns.saturating_sub(unix_now_ns()));
        let reply = answered(receiver, wait).await;
        let delegation = self
            .lock()
            .remove(digest)
            .and_then(|entry| entry.delegation);
        match (reply, delegation) {
            (CertifyReply::Issued { cert, .. }, Some(delegation)) => Ok((cert, delegation)),
            (CertifyReply::Denied { reason }, _) => Err(reason),
            (CertifyReply::Issued { .. } | CertifyReply::Pending { .. }, _) => {
                Err("the approver did not sign a delegation".into())
            }
            (CertifyReply::Expired, _) => Err("no approving device answered in time".into()),
        }
    }

    fn enqueue_with(
        &self,
        request: CertificationRequest,
        agent: Option<AgentProposal>,
        now_ns: u64,
    ) -> Result<Queued, String> {
        request
            .verify_at(now_ns)
            .map_err(|error| error.to_string())?;
        if request.user_public_key != self.identity.user_public_key {
            return Err("this host serves another user identity".into());
        }
        if self.revocations().is_revoked(&request.device_public_key) {
            return Err("device is revoked".into());
        }
        let digest = request.digest().map_err(|error| error.to_string())?;
        let pairing_code = request.pairing_code().map_err(|error| error.to_string())?;
        let mut pending = self.lock();
        pending.retain(|_, entry| entry.entry.request.expires_at_ns > now_ns);
        if pending.contains_key(&digest) {
            return Err("request is already pending".into());
        }
        if pending.len() >= MAX_PENDING_CERTIFICATIONS {
            return Err("too many pending certification requests".into());
        }
        let (answer, receiver) = watch::channel(None);
        pending.insert(
            digest,
            Pending {
                entry: PendingCertification {
                    request,
                    request_digest: digest,
                    pairing_code: pairing_code.clone(),
                },
                answer,
                agent,
                delegation: None,
            },
        );
        drop(pending);
        Ok((digest, pairing_code, receiver))
    }

    async fn serve_requester(
        &self,
        connection: &Connection,
        mut send: iroh::endpoint::SendStream,
        request: CertificationRequest,
    ) -> Result<(), AcceptError> {
        let expires_at_ns = request.expires_at_ns;
        let now = unix_now_ns();
        let (digest, pairing_code, answer) = match self.enqueue(request, now) {
            Ok(queued) => queued,
            Err(reason) => {
                write_frame(&mut send, &encode(&CertifyReply::Denied { reason })?)
                    .await
                    .map_err(|error| io(&error))?;
                return finish(connection, send).await;
            }
        };
        write_frame(&mut send, &encode(&CertifyReply::Pending { pairing_code })?)
            .await
            .map_err(|error| io(&error))?;
        let wait = Duration::from_nanos(expires_at_ns.saturating_sub(now));
        let reply = tokio::select! {
            reply = answered(answer, wait) => reply,
            _ = connection.closed() => {
                self.lock().remove(&digest);
                return Ok(());
            }
        };
        self.lock().remove(&digest);
        write_frame(&mut send, &encode(&reply)?)
            .await
            .map_err(|error| io(&error))?;
        finish(connection, send).await
    }

    /// Wait up to `wait` for the answer to request `digest`, for requesters
    /// that poll (HTTPS). `Pending` means not decided yet: poll again. A
    /// delivered answer is removed.
    pub async fn wait(&self, digest: &[u8; 32], wait: Duration) -> CertifyReply {
        let Some(receiver) = self
            .lock()
            .get(digest)
            .map(|entry| entry.answer.subscribe())
        else {
            return CertifyReply::Expired;
        };
        let reply = answered(receiver, wait).await;
        if matches!(reply, CertifyReply::Expired) {
            let pairing_code = self.lock().get(digest).map(|entry| {
                (entry.entry.request.expires_at_ns > unix_now_ns())
                    .then(|| entry.entry.pairing_code.clone())
            });
            if let Some(Some(pairing_code)) = pairing_code {
                return CertifyReply::Pending { pairing_code };
            }
        }
        self.lock().remove(digest);
        reply
    }

    async fn serve_approver(
        &self,
        connection: &Connection,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        hello: auth::Hello,
    ) -> Result<(), AcceptError> {
        // Only the owner's devices that may admit devices approve, checked
        // before the relay signs anything for the approver.
        self.identity
            .authenticate_hello(
                EndpointTrust::Fixed(HandshakeTrust::OwnerDeviceAdmitters),
                &hello,
                &mut recv,
                &mut send,
                MAX_CERTIFY_FRAME_BYTES,
            )
            .await
            .map_err(|error| io(&error))?;
        while let Some(frame) = read_frame_or_eof(&mut recv, MAX_CERTIFY_FRAME_BYTES)
            .await
            .map_err(|error| io(&error))?
        {
            let request: ApproverRequest =
                postcard::from_bytes(&frame).map_err(|error| io(&error))?;
            let reply = self.decide(request, unix_now_ns());
            write_frame(&mut send, &encode(&reply)?)
                .await
                .map_err(|error| io(&error))?;
        }
        finish(connection, send).await
    }

    fn decide(&self, request: ApproverRequest, now_ns: u64) -> ApproverReply {
        let refused = |reason: &str| ApproverReply::Refused {
            reason: reason.into(),
        };
        match request {
            ApproverRequest::ListPending => ApproverReply::Pending(
                self.lock()
                    .values()
                    .filter(|entry| entry.entry.request.expires_at_ns > now_ns && !entry.decided())
                    .map(|entry| entry.entry.clone())
                    .collect(),
            ),
            ApproverRequest::AgentProposals => ApproverReply::AgentProposals(
                self.lock()
                    .values()
                    .filter(|entry| entry.entry.request.expires_at_ns > now_ns && !entry.decided())
                    .filter_map(|entry| {
                        Some(PendingAgent {
                            request_digest: entry.entry.request_digest,
                            proposal: entry.agent.clone()?,
                        })
                    })
                    .collect(),
            ),
            ApproverRequest::Decide {
                request_digest,
                decision,
            } => {
                let mut pending = self.lock();
                let Some(entry) = pending
                    .get_mut(&request_digest)
                    .filter(|entry| !entry.decided())
                else {
                    return refused("no such pending request");
                };
                let reply = match decision {
                    ApproverDecision::Deny => CertifyReply::Denied {
                        reason: "declined by the approving device".into(),
                    },
                    ApproverDecision::Issue(_) if entry.agent.is_some() => {
                        return refused("an agent is approved together with its delegation");
                    }
                    ApproverDecision::IssueAgent { cert, delegation } => {
                        let Some(proposal) = &entry.agent else {
                            return refused("this request is not an agent's");
                        };
                        if let Err(error) = verify_issued_cert(
                            &entry.entry.request,
                            &cert,
                            &CertificationPolicy::agent(),
                            &self.revocations(),
                            now_ns,
                        ) {
                            return refused(&error.to_string());
                        }
                        if delegation.agent_device_public_key
                            != entry.entry.request.device_public_key
                        {
                            return refused("the delegation names another device");
                        }
                        if let Err(error) =
                            delegation.verify(&self.identity.user_public_key, now_ns)
                        {
                            return refused(&error.to_string());
                        }
                        if let Err(reason) = proposal.admits(&delegation) {
                            return refused(reason);
                        }
                        entry.delegation = Some(delegation);
                        CertifyReply::Issued {
                            cert,
                            host_cert: self.identity.cert.clone(),
                        }
                    }
                    ApproverDecision::Issue(cert) => {
                        if let Err(error) = verify_issued_cert(
                            &entry.entry.request,
                            &cert,
                            &self.policy,
                            &self.revocations(),
                            now_ns,
                        ) {
                            return refused(&error.to_string());
                        }
                        CertifyReply::Issued {
                            cert,
                            host_cert: self.identity.cert.clone(),
                        }
                    }
                };
                entry.answer.send_replace(Some(reply));
                drop(pending);
                ApproverReply::Decided
            }
        }
    }
}

async fn finish(
    connection: &Connection,
    mut send: iroh::endpoint::SendStream,
) -> Result<(), AcceptError> {
    send.finish().map_err(|error| io(&error))?;
    let _ = tokio::time::timeout(CLOSE_GRACE, connection.closed()).await;
    Ok(())
}

impl ProtocolHandler for CertificationProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (send, mut recv) = connection.accept_bi().await?;
        let open: CertifyOpen =
            postcard::from_bytes(&read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await?)
                .map_err(|error| io(&error))?;
        match open {
            CertifyOpen::Request(request) => self.serve_requester(&connection, send, request).await,
            CertifyOpen::Approver(hello) => {
                self.serve_approver(&connection, send, recv, hello).await
            }
        }
    }
}

/// The answer `receiver` sees within `wait`, or `Expired`.
async fn answered(
    mut receiver: watch::Receiver<Option<CertifyReply>>,
    wait: Duration,
) -> CertifyReply {
    let decided = tokio::time::timeout(wait, receiver.wait_for(Option::is_some)).await;
    match decided {
        Ok(Ok(reply)) => reply.clone().unwrap_or(CertifyReply::Expired),
        Ok(Err(_)) | Err(_) => CertifyReply::Expired,
    }
}
