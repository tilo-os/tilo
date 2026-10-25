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
//!
//! A member of a workspace the host serves pairs devices through it too:
//! their request waits beside the owner's, and a page holding the member's
//! root lists and answers that member's own requests over HTTPS with
//! root-signed [`MemberApproval`]s ([`CertificationProtocol::member_approve`]).
//!
//! The same approvers answer the questions the host's agents put to their
//! person ([`CertificationProtocol::confirm`]): one invocation each, allowed
//! once or denied, refused when nobody answers in time.

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
    ConfirmationAnswer, MAX_CERTIFY_FRAME_BYTES, MAX_PENDING_CERTIFICATIONS, MemberApproval,
    PendingCertification, PendingConfirmation, PendingDelegate,
};
use crate::frame::{read_frame, read_frame_or_eof, write_frame};
use crate::handshake::{EndpointTrust, LiveIdentity};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_relay_protocol::delegation::{DelegateProposal, Delegation};

/// How long a final answer may wait for the requester to read it.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Why the host will not issue a certificate now, in the words the
/// requesting device reads.
const NO_LONGER_MEMBER: &str =
    "this person is no longer a member of the workspaces this host serves";

/// Whether the host's own root answers a pending request, or it will ask the
/// workspace registry instead. A certificate minted after a member's
/// admission ended would undo the removal, so a member's decision has to say
/// what admission says now. The answer is read before the decision is applied,
/// so no lock is held across the registry read.
/// See [`CertificationProtocol::member_approve`].
#[derive(Clone, Copy)]
pub enum Admission<'a> {
    /// The host owns the request: its root is the host's own.
    Host,
    /// A member's request, and whether they are admitted to a served
    /// workspace now.
    Member {
        /// Whether the person is admitted to one of them.
        admitted: &'a bool,
    },
}

impl Admission<'_> {
    /// Whether the host may issue for a request it queued for a member.
    fn still_admits(self) -> bool {
        match self {
            Self::Host => true,
            Self::Member { admitted } => *admitted,
        }
    }
}

/// One request, and its answer once an approver gave it. A decided request
/// stays until its requester collected the answer or it expired, so an HTTPS
/// requester that polls later still receives it.
struct Pending {
    entry: PendingCertification,
    answer: watch::Sender<Option<CertifyReply>>,
    /// Set for a request the host queued for an agent it supervises.
    agent: Option<DelegateProposal>,
    /// The approved delegation of such a request, once decided.
    delegation: Option<Delegation>,
    /// Whether the request named a member of a workspace this host serves.
    /// Their admission is asked again when the certificate is issued, because
    /// a guest the host no longer serves must not be certified through it.
    member: bool,
}

impl Pending {
    fn decided(&self) -> bool {
        self.answer.borrow().is_some()
    }
}

/// A question an agent waits on, and the person's answer once given.
struct Asked {
    question: PendingConfirmation,
    answer: watch::Sender<Option<ConfirmationAnswer>>,
}

impl Asked {
    fn open(&self, now_ns: u64) -> bool {
        self.question.expires_at_ns > now_ns && self.answer.borrow().is_none()
    }
}

/// A queued request: its digest, pairing code and a receiver of its answer.
type Queued = ([u8; 32], String, watch::Receiver<Option<CertifyReply>>);

/// The pairing invites answered, as (person, invite).
type AnsweredInvites = std::collections::HashSet<([u8; 32], [u8; 16])>;

/// The certification ALPN handler.
#[derive(Clone)]
pub struct CertificationProtocol {
    identity: LiveIdentity,
    policy: CertificationPolicy,
    pending: Arc<Mutex<HashMap<[u8; 32], Pending>>>,
    confirmations: Arc<Mutex<HashMap<[u8; 32], Asked>>>,
    /// The pairing invites an approver answered, by person: an invite is
    /// for one device, so a second request naming it is refused.
    answered_invites: Arc<Mutex<AnsweredInvites>>,
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
            confirmations: Arc::new(Mutex::new(HashMap::new())),
            answered_invites: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    fn revocations(&self) -> RevocationList {
        self.identity.revocations.read_recover().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Pending>> {
        self.pending.lock_recover()
    }

    fn asked(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Asked>> {
        self.confirmations.lock_recover()
    }

    /// Put `question` to the person's approving devices and wait for the
    /// answer to it, until it expires.
    ///
    /// # Errors
    ///
    /// Returns why there is no answer: the question could not be asked, or
    /// nobody answered before it expired.
    pub async fn confirm(
        &self,
        question: PendingConfirmation,
    ) -> Result<ConfirmationAnswer, String> {
        let now = unix_now_ns();
        let id = question.id;
        let wait = Duration::from_nanos(question.expires_at_ns.saturating_sub(now));
        let mut receiver = {
            let mut asked = self.asked();
            asked.retain(|_, entry| entry.open(now));
            if asked.contains_key(&id) {
                return Err("this question is already asked".into());
            }
            if asked.len() >= MAX_PENDING_CERTIFICATIONS {
                return Err("too many questions are waiting for an answer".into());
            }
            let (answer, receiver) = watch::channel(None);
            asked.insert(id, Asked { question, answer });
            receiver
        };
        let answered = tokio::time::timeout(wait, receiver.wait_for(Option::is_some))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|answer| *answer);
        self.asked().remove(&id);
        answered.ok_or_else(|| "nobody answered in time".into())
    }

    /// Admit a request into the pending set, or say why not. Returns its
    /// digest, pairing code and a receiver that sees the answer.
    pub fn enqueue(&self, request: CertificationRequest, now_ns: u64) -> Result<Queued, String> {
        self.enqueue_with(request, None, now_ns, false, None)
    }

    /// [`Self::enqueue`], also for a member of a workspace this host serves
    /// (`member`: the request names such a member's root), whose own devices
    /// answer it through [`Self::member_approve`]. `country` is where a
    /// trusted proxy says the request came from, shown to the approver.
    pub fn enqueue_member(
        &self,
        request: CertificationRequest,
        now_ns: u64,
        member: bool,
        country: Option<String>,
    ) -> Result<Queued, String> {
        self.enqueue_with(request, None, now_ns, member, country)
    }

    /// Answer a member's root-signed approver command: their own pending
    /// requests, or a decision on one of them.
    pub fn member_approve(
        &self,
        approval: MemberApproval,
        now_ns: u64,
        admitted: Admission<'_>,
    ) -> ApproverReply {
        let refused = |reason: &str| ApproverReply::Refused {
            reason: reason.into(),
        };
        if let Err(reason) = approval.verify(now_ns) {
            return refused(reason);
        }
        let user = approval.user_public_key;
        match approval.request {
            ApproverRequest::ListPending => ApproverReply::Pending(self.pending_of(&user, now_ns)),
            ApproverRequest::Decide {
                request_digest,
                decision,
            } => self.decide(
                ApproverRequest::Decide {
                    request_digest,
                    decision,
                },
                now_ns,
                &user,
                admitted,
            ),
            _ => refused("a member approves only their own device requests"),
        }
    }

    /// `user`'s requests still waiting for an answer.
    fn pending_of(&self, user: &[u8; 32], now_ns: u64) -> Vec<PendingCertification> {
        self.lock()
            .values()
            .filter(|entry| {
                entry.entry.request.user_public_key == *user
                    && entry.entry.request.expires_at_ns > now_ns
                    && !entry.decided()
            })
            .map(|entry| entry.entry.clone())
            .collect()
    }

    /// Queue a request for an agent this host supervises, carrying what
    /// the person proposed. Only an `IssueDelegate` decision answers it; wait
    /// for it with [`Self::delegate_answer`].
    pub fn enqueue_delegate(
        &self,
        request: CertificationRequest,
        proposal: DelegateProposal,
        now_ns: u64,
    ) -> Result<([u8; 32], String), String> {
        if proposal.delegate.agent_id().is_some_and(str::is_empty)
            || proposal.delegate.installation().is_some_and(str::is_empty)
            || proposal.scope.rooms.is_empty()
        {
            return Err("a delegate proposal names its delegate and rooms".into());
        }
        self.enqueue_with(request, Some(proposal), now_ns, false, None)
            .map(|(digest, code, _)| (digest, code))
    }

    /// The approver's answer to agent request `digest`: its certificate and
    /// delegation, or why there are none. Waits until decided or expired.
    pub async fn delegate_answer(
        &self,
        digest: &[u8; 32],
    ) -> Result<(DeviceCert, Delegation), String> {
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
        agent: Option<DelegateProposal>,
        now_ns: u64,
        member: bool,
        country: Option<String>,
    ) -> Result<Queued, String> {
        request
            .verify_at(now_ns)
            .map_err(|error| error.to_string())?;
        if request.user_public_key != self.identity.user_public_key && !member {
            return Err("this host serves another user identity".into());
        }
        if self.revocations().is_revoked(&request.device_public_key) {
            return Err("device is revoked".into());
        }
        if request.invite != [0; 16]
            && self
                .answered_invites
                .lock_recover()
                .contains(&(request.user_public_key, request.invite))
        {
            return Err("this pairing link was used already; make a new one".into());
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
                    requested_at_ns: now_ns,
                    country,
                },
                answer,
                agent,
                delegation: None,
                member,
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
    /// delivered answer is removed. A request that expires within `wait`
    /// is answered `Expired` when it expires, not a whole poll later.
    pub async fn wait(&self, digest: &[u8; 32], wait: Duration) -> CertifyReply {
        let Some((receiver, expires_at_ns)) = self
            .lock()
            .get(digest)
            .map(|entry| (entry.answer.subscribe(), entry.entry.request.expires_at_ns))
        else {
            return CertifyReply::Expired;
        };
        let wait = wait.min(Duration::from_nanos(
            expires_at_ns.saturating_sub(unix_now_ns()),
        ));
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
            let reply = self.decide(
                request,
                unix_now_ns(),
                &self.identity.user_public_key,
                Admission::Host,
            );
            write_frame(&mut send, &encode(&reply)?)
                .await
                .map_err(|error| io(&error))?;
        }
        finish(connection, send).await
    }

    /// Every question still waiting for its answer.
    fn questions(&self, now_ns: u64) -> Vec<PendingConfirmation> {
        self.asked()
            .values()
            .filter(|entry| entry.open(now_ns))
            .map(|entry| entry.question.clone())
            .collect()
    }

    /// Hand `answer` to the agent waiting on question `id`, once.
    fn answer(&self, id: [u8; 32], answer: ConfirmationAnswer, now_ns: u64) -> ApproverReply {
        let asked = self.asked();
        let Some(entry) = asked.get(&id).filter(|entry| entry.open(now_ns)) else {
            return ApproverReply::Refused {
                reason: "no such question is waiting".into(),
            };
        };
        entry.answer.send_replace(Some(answer));
        drop(asked);
        ApproverReply::Decided
    }

    /// Answer `request` from an approver of `approver`'s root, who decides
    /// only requests naming that root.
    fn decide(
        &self,
        request: ApproverRequest,
        now_ns: u64,
        approver: &[u8; 32],
        admitted: Admission<'_>,
    ) -> ApproverReply {
        match request {
            // The owner's devices see the owner's requests; a member's are
            // answered by the member (`member_approve`).
            ApproverRequest::ListPending => {
                ApproverReply::Pending(self.pending_of(&self.identity.user_public_key, now_ns))
            }
            ApproverRequest::DelegateProposals => ApproverReply::DelegateProposals(
                self.lock()
                    .values()
                    .filter(|entry| entry.entry.request.expires_at_ns > now_ns && !entry.decided())
                    .filter_map(|entry| {
                        Some(PendingDelegate {
                            request_digest: entry.entry.request_digest,
                            proposal: entry.agent.clone()?,
                        })
                    })
                    .collect(),
            ),
            ApproverRequest::Confirmations => ApproverReply::Confirmations(self.questions(now_ns)),
            ApproverRequest::Answer { id, answer } => self.answer(id, answer, now_ns),
            ApproverRequest::Decide {
                request_digest,
                decision,
            } => self.decide_one(request_digest, decision, now_ns, approver, admitted),
        }
    }

    /// Answer one pending request. The person's admission is asked again
    /// first: an approval is the approver's decision, not a standing grant, so
    /// someone the host no longer serves is refused here rather than
    /// certified. The answer is read before the pending set is locked, so no
    /// caller can hold a lock while it asks the workspace registry.
    fn decide_one(
        &self,
        request_digest: [u8; 32],
        decision: ApproverDecision,
        now_ns: u64,
        approver: &[u8; 32],
        admitted: Admission<'_>,
    ) -> ApproverReply {
        let refused = |reason: &str| ApproverReply::Refused {
            reason: reason.into(),
        };
        if matches!(
            decision,
            ApproverDecision::Issue(_) | ApproverDecision::IssueDelegate { .. }
        ) {
            // Read before the pending set is locked: nothing is held while
            // the caller's answer is applied.
            let member = self
                .lock()
                .get(&request_digest)
                .is_some_and(|entry| entry.member);
            if member && !admitted.still_admits() {
                return refused(NO_LONGER_MEMBER);
            }
        }
        let mut pending = self.lock();
        let Some(entry) = pending
            .get_mut(&request_digest)
            .filter(|entry| !entry.decided() && entry.entry.request.user_public_key == *approver)
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
            ApproverDecision::IssueDelegate { cert, delegation } => {
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
                if delegation.device_public_key != entry.entry.request.device_public_key {
                    return refused("the delegation names another device");
                }
                if let Err(error) = delegation.verify(&self.identity.user_public_key, now_ns) {
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
        let invite = entry.entry.request.invite;
        drop(pending);
        if invite != [0; 16] {
            self.answered_invites
                .lock_recover()
                .insert((*approver, invite));
        }
        ApproverReply::Decided
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

#[cfg(test)]
mod confirmation_tests {

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, UserIdentity, sign_device_cert};

    fn protocol() -> CertificationProtocol {
        let (root, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "home-server".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        CertificationProtocol::new(LiveIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            cert,
            signing_key: key,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
        })
    }

    fn question(id: u8, wait: Duration) -> PendingConfirmation {
        PendingConfirmation {
            id: [id; 32],
            agent_id: "careful".into(),
            agent_label: "Careful".into(),
            action_label: "Clear history".into(),
            resource_label: "Calculator".into(),
            risk: "destructive".into(),
            room: "room".into(),
            expires_at_ns: unix_now_ns().saturating_add(u64::try_from(wait.as_nanos()).unwrap()),
        }
    }

    #[tokio::test]
    async fn a_question_nobody_answers_is_refused_when_it_expires() {
        let protocol = protocol();
        let answer = protocol
            .confirm(question(1, Duration::from_millis(100)))
            .await;
        assert!(answer.is_err(), "{answer:?}");
        assert_eq!(
            protocol.decide(
                ApproverRequest::Confirmations,
                unix_now_ns(),
                &protocol.identity.user_public_key,
                Admission::Host,
            ),
            ApproverReply::Confirmations(Vec::new()),
            "an expired question is no longer shown"
        );
        drop(protocol);
    }

    #[tokio::test]
    async fn an_approver_answers_each_question_once() {
        let protocol = protocol();
        let waiting = tokio::spawn({
            let protocol = protocol.clone();
            async move { protocol.confirm(question(2, Duration::from_secs(30))).await }
        });
        let shown = loop {
            if let ApproverReply::Confirmations(shown) = protocol.decide(
                ApproverRequest::Confirmations,
                unix_now_ns(),
                &protocol.identity.user_public_key,
                Admission::Host,
            ) && !shown.is_empty()
            {
                break shown;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            shown[0].question(),
            "Careful wants to Clear history: Calculator, for you"
        );
        let answer = |answer| ApproverRequest::Answer {
            id: [2; 32],
            answer,
        };
        assert_eq!(
            protocol.decide(
                answer(ConfirmationAnswer::AllowOnce),
                unix_now_ns(),
                &protocol.identity.user_public_key,
                Admission::Host,
            ),
            ApproverReply::Decided
        );
        assert!(matches!(
            protocol.decide(
                answer(ConfirmationAnswer::Deny),
                unix_now_ns(),
                &protocol.identity.user_public_key,
                Admission::Host,
            ),
            ApproverReply::Refused { .. }
        ));
        assert_eq!(waiting.await.unwrap(), Ok(ConfirmationAnswer::AllowOnce));
        drop(protocol);
    }

    fn member_request(
        member: &ed25519_dalek::SigningKey,
        device: &ed25519_dalek::SigningKey,
    ) -> CertificationRequest {
        let now = unix_now_ns();
        CertificationRequest {
            version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
            user_public_key: member.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Phone".into(),
            requested: DeviceCapabilities::member(),
            nonce: [3; 16],
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now.checked_add(60_000_000_000).unwrap(),
            signature: [0; 64],
        }
        .sign(device)
        .unwrap()
    }

    fn signed(member: &ed25519_dalek::SigningKey, request: ApproverRequest) -> MemberApproval {
        use ed25519_dalek::Signer as _;
        let user_public_key = member.verifying_key().to_bytes();
        let issued_at_ns = unix_now_ns();
        let message =
            MemberApproval::signed_message(&user_public_key, issued_at_ns, &request).unwrap();
        MemberApproval {
            user_public_key,
            issued_at_ns,
            request,
            signature: member.sign(&message).to_bytes(),
        }
    }

    #[tokio::test]
    async fn a_polling_requester_hears_of_expiry_when_its_request_expires() {
        let protocol = protocol();
        let device = ed25519_dalek::SigningKey::from_bytes(&[14; 32]);
        let now = unix_now_ns();
        let request = CertificationRequest {
            version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
            user_public_key: protocol.identity.user_public_key,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Phone".into(),
            requested: DeviceCapabilities::member(),
            nonce: [4; 16],
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now.checked_add(200_000_000).unwrap(),
            signature: [0; 64],
        }
        .sign(&device)
        .unwrap();
        let (digest, _, _) = protocol.enqueue(request, now).unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(20),
            protocol.wait(&digest, Duration::from_secs(60)),
        )
        .await
        .expect("the poll ends when the request expires");
        assert_eq!(reply, CertifyReply::Expired);
        drop(protocol);
    }

    #[tokio::test]
    async fn a_member_pairs_a_device_through_a_host_they_share() {
        let protocol = protocol();
        let member = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[12; 32]);
        let request = member_request(&member, &device);
        assert!(
            protocol.enqueue(request.clone(), unix_now_ns()).is_err(),
            "a stranger's request is refused"
        );
        let (digest, code, _) = protocol
            .enqueue_member(request.clone(), unix_now_ns(), true, None)
            .unwrap();
        let owner = protocol.identity.user_public_key;
        let ApproverReply::Pending(listed) = protocol.decide(
            ApproverRequest::ListPending,
            unix_now_ns(),
            &owner,
            Admission::Host,
        ) else {
            panic!("a list");
        };
        assert!(
            listed.is_empty(),
            "the owner never sees a member's requests"
        );
        let ApproverReply::Pending(theirs) = protocol.member_approve(
            signed(&member, ApproverRequest::ListPending),
            unix_now_ns(),
            Admission::Host,
        ) else {
            panic!("a list");
        };
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].pairing_code, code);
        let other = ed25519_dalek::SigningKey::from_bytes(&[13; 32]);
        let deny = ApproverRequest::Decide {
            request_digest: digest,
            decision: ApproverDecision::Deny,
        };
        assert!(
            matches!(
                protocol.member_approve(
                    signed(&other, deny.clone()),
                    unix_now_ns(),
                    Admission::Host,
                ),
                ApproverReply::Refused { .. }
            ),
            "another member cannot answer it"
        );
        assert!(
            matches!(
                protocol.decide(deny, unix_now_ns(), &owner, Admission::Host),
                ApproverReply::Refused { .. }
            ),
            "nor the owner"
        );
        let cert = tilo_user_identity::issue_device_cert(
            &request,
            &member,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            unix_now_ns(),
        )
        .unwrap();
        let issue = ApproverRequest::Decide {
            request_digest: digest,
            decision: ApproverDecision::Issue(cert.clone()),
        };
        assert_eq!(
            protocol.member_approve(signed(&member, issue), unix_now_ns(), Admission::Host,),
            ApproverReply::Decided
        );
        let CertifyReply::Issued {
            cert: delivered, ..
        } = protocol.wait(&digest, Duration::from_secs(1)).await
        else {
            panic!("issued");
        };
        assert_eq!(delivered, cert);
        drop(protocol);
    }

    #[tokio::test]
    async fn a_guest_who_is_no_longer_a_member_is_not_certified() {
        let protocol = protocol();
        let member = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let request = member_request(&member, &device);
        let (digest, _, _) = protocol
            .enqueue_member(request.clone(), unix_now_ns(), true, None)
            .unwrap();
        let cert = tilo_user_identity::issue_device_cert(
            &request,
            &member,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            unix_now_ns(),
        )
        .unwrap();
        let issue = ApproverRequest::Decide {
            request_digest: digest,
            decision: ApproverDecision::Issue(cert),
        };

        // What the approve route asks right before it hands the approval on:
        // this host no longer serves a workspace that admits them.
        let admitted = false;
        let reply = protocol.member_approve(
            signed(&member, issue),
            unix_now_ns(),
            Admission::Member {
                admitted: &admitted,
            },
        );
        let ApproverReply::Refused { reason } = &reply else {
            panic!("a former guest is not certified through this host: {reply:?}");
        };
        assert!(reason.contains("no longer a member"), "{reason}");
    }
}
