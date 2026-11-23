//! Host side of server-mediated device certification
//! ([`crate::certify`], ALPN `tilo-device-certify/2`).
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
//! An agent is approved the same way, by any device of the person it acts
//! for: the same command lists the proposal, and the member's root signs the
//! agent's certificate and its delegation together.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use tilo_clock::unix_now_ns;
use tilo_server_client::link::{Link, LinkSend};
use tilo_user_identity::{
    CertificationPolicy, CertificationRequest, DeviceCert, RevocationList, verify_issued_cert,
};
use tokio::sync::watch;

use crate::auth::{self, HandshakeTrust};
use crate::certify::{
    AWAIT_PENDING_NS, ApproverDecision, ApproverReply, ApproverRequest, CertifyOpen, CertifyReply,
    MAX_CERTIFY_FRAME_BYTES, MAX_PENDING_CERTIFICATIONS, MemberApproval, PendingCertification,
    PendingDelegate, PushSubscription,
};
use crate::frame::{read_frame, read_frame_or_eof, write_frame};
use crate::handshake::{EndpointTrust, LiveIdentity};
use crate::workspace_access;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::certify::{MAX_NUMBERED_LABELS, numbered_label};
use tilo_server_protocol::delegation::{DelegateProposal, Delegation};

/// How long a final answer may wait for the requester to read it.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// One request, and its answer once an approver gave it. A decided request
/// stays until its requester collected the answer or it expired, so an
/// approver that answers just before the request expires still reaches the
/// device waiting for it.
struct Pending {
    entry: PendingCertification,
    answer: watch::Sender<Option<CertifyReply>>,
    /// The name this request's device is given when it is issued: the one it
    /// proposed, or the next number of it no other device of the person reads
    /// by.
    label: String,
    /// Set for a request the host queued for an agent it supervises.
    agent: Option<DelegateProposal>,
    /// The approved delegation of such a request, once decided.
    delegation: Option<Delegation>,
}

impl Pending {
    fn decided(&self) -> bool {
        self.answer.borrow().is_some()
    }
}

/// Whether `entry` is a person's own request, still open, naming `user`
/// and `invite`.
#[cfg(feature = "web")]
fn named(entry: &Pending, user: &[u8; 32], invite: &[u8; 16], now_ns: u64) -> bool {
    !entry.decided()
        && entry.agent.is_none()
        && entry.entry.request.user_public_key == *user
        && entry.entry.request.invite == *invite
        && entry.entry.request.expires_at_ns > now_ns
}

/// A queued request: its digest, pairing code and a receiver of its answer.
type Queued = ([u8; 32], String, watch::Receiver<Option<CertifyReply>>);

/// The pairing invites answered, as (person, invite).
type AnsweredInvites = std::collections::HashSet<([u8; 32], [u8; 16])>;

/// The requests a page's sign-in tickets expect (`crate::web::tickets`): queued for the device
/// that approved the tab, even for a person no workspace here lets in yet.
pub(crate) trait TabRequests: Send + Sync {
    /// Whether a tab signing in with a ticket here pairs with `request`: the device that
    /// approved it answers it ([`CertificationProtocol::issue_named`]).
    fn expects(&self, request: &CertificationRequest, now_ns: u64) -> bool;
}

/// Where a person's browsers take pushes ([`crate::web_push`]).
pub trait PushSubscriptions: Send + Sync {
    /// The key a browser subscribes with.
    fn push_key(&self) -> Vec<u8>;
    /// Keep `subscription` as one of `person`'s browsers.
    ///
    /// # Errors
    /// Why it was not kept.
    fn subscribe(&self, person: &[u8; 32], subscription: PushSubscription) -> Result<(), String>;
}

/// The certification ALPN handler.
#[derive(Clone)]
pub struct CertificationProtocol {
    identity: LiveIdentity,
    policy: CertificationPolicy,
    pending: Arc<Mutex<HashMap<[u8; 32], Pending>>>,
    /// The pairing invites an approver answered, by person: an invite is
    /// for one device, so a second request naming it is refused.
    answered_invites: Arc<Mutex<AnsweredInvites>>,
    /// The workspaces this host serves, whose members may queue their own
    /// devices' requests; absent in a host that governs none.
    workspaces: Option<crate::registry::WorkspaceRegistry>,
    /// The requests this server's login codes expect.
    codes: Arc<std::sync::OnceLock<Arc<dyn TabRequests>>>,
    /// The workspaces this host governs, whose records keep each person's
    /// devices: a certificate a root issues here joins its person's record.
    records: Arc<std::sync::OnceLock<tilo_workspace_service::workspaces::Workspaces>>,
    /// Moves each time a request is queued or answered: what an approver
    /// waiting for its person's requests to change
    /// ([`ApproverRequest::AwaitPending`]) wakes on.
    moved: Arc<watch::Sender<u64>>,
    /// Keeps where a person's browsers take pushes.
    notifier: Arc<std::sync::OnceLock<Arc<dyn PushSubscriptions>>>,
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
    pub(crate) fn new(
        identity: LiveIdentity,
        workspaces: Option<crate::registry::WorkspaceRegistry>,
    ) -> Self {
        Self {
            identity,
            policy: CertificationPolicy::browser(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            answered_invites: Arc::new(Mutex::new(std::collections::HashSet::new())),
            workspaces,
            codes: Arc::default(),
            records: Arc::default(),
            moved: Arc::new(watch::Sender::new(0)),
            notifier: Arc::default(),
        }
    }

    /// Name what keeps a person's browsers' push subscriptions. Named once; a
    /// later call keeps the first.
    pub fn set_notifier(&self, notifier: Arc<dyn PushSubscriptions>) {
        let _ = self.notifier.set(notifier);
    }

    /// A push subscription of `person`'s, or the key to make one with.
    fn push(&self, person: &[u8; 32], request: ApproverRequest) -> ApproverReply {
        let notifier = self.notifier.get();
        match request {
            ApproverRequest::PushKey => ApproverReply::PushKey(
                notifier
                    .map(|notifier| notifier.push_key())
                    .unwrap_or_default(),
            ),
            ApproverRequest::Notify(subscription) => match notifier {
                None => ApproverReply::Refused {
                    reason: "this host notifies nobody".into(),
                },
                Some(notifier) => match notifier.subscribe(person, subscription) {
                    Ok(()) => ApproverReply::Decided,
                    Err(reason) => ApproverReply::Refused { reason },
                },
            },
            _ => ApproverReply::Refused {
                reason: "not a push request".into(),
            },
        }
    }

    /// Name the requests this server's sign-in tickets expect. Named once; a later call keeps
    /// the first.
    #[cfg(feature = "web")]
    pub(crate) fn set_tab_requests(&self, codes: Arc<dyn TabRequests>) {
        let _ = self.codes.set(codes);
    }

    /// The request waiting here, undecided, that names `user` and `invite`:
    /// what the device that claimed a sign-in code certifies.
    #[cfg(feature = "web")]
    pub(crate) fn waiting_for(
        &self,
        user: &[u8; 32],
        invite: &[u8; 16],
        now_ns: u64,
    ) -> Option<CertificationRequest> {
        self.lock()
            .values()
            .find(|entry| named(entry, user, invite, now_ns))
            .map(|entry| entry.entry.request.clone())
    }

    /// Answer the request waiting here that names `user` and `invite` with
    /// `cert`, which the person's root or one of their devices signed for
    /// exactly that request, as an approver answers it. `take` spends what
    /// expected the request, once `cert` was found to answer it; nothing is
    /// answered when it finds nothing to spend.
    ///
    /// # Errors
    ///
    /// No such request waits, `cert` does not answer it, or `take` found
    /// nothing to spend.
    #[cfg(feature = "web")]
    pub(crate) fn issue_named(
        &self,
        user: &[u8; 32],
        invite: &[u8; 16],
        cert: DeviceCert,
        take: impl FnOnce(&CertificationRequest) -> bool,
        now_ns: u64,
    ) -> Result<CertificationRequest, String> {
        let revocations = self.revocations();
        let mut pending = self.lock();
        let entry = pending
            .values_mut()
            .find(|entry| named(entry, user, invite, now_ns))
            .ok_or("the tab's request is not waiting here")?;
        verify_issued_cert(
            &entry.entry.request,
            &cert,
            &self.policy,
            &revocations,
            now_ns,
        )
        .map_err(|error| error.to_string())?;
        let request = entry.entry.request.clone();
        if !take(&request) {
            return Err("nothing expects this request any more".into());
        }
        self.answered_invites
            .lock_recover()
            .insert((request.user_public_key, request.invite));
        let reply = self.issued(entry, cert);
        self.settle(entry, reply);
        drop(pending);
        Ok(request)
    }

    /// Add the certificate `reply` issues, when it issues one, to its
    /// person's record, under the name the host gave it.
    fn record_issued(&self, reply: &CertifyReply) {
        if let (CertifyReply::Issued { cert, label, .. }, Some(records)) =
            (reply, self.records.get())
        {
            let records = records.clone();
            let cert = cert.clone();
            let label = label.clone();
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return;
            };
            runtime.spawn(async move {
                records.keep_issued_device(&cert, &label).await;
            });
        }
    }

    /// Add every certificate a person's root issues here to that person's
    /// record in `workspaces`. Named once; a later call keeps the first.
    pub(crate) fn keep_issued_in(
        &self,
        workspaces: tilo_workspace_service::workspaces::Workspaces,
    ) {
        let _ = self.records.set(workspaces);
    }

    /// Add `cert`, a certificate the person's root signed here, to the person's
    /// record, so their devices list it and the workspaces here admit it. Idempotent.
    #[cfg(feature = "web")]
    pub(crate) async fn keep_issued_device(&self, cert: &DeviceCert, label: &str) {
        if let Some(records) = self.records.get() {
            records.keep_issued_device(cert, label).await;
        }
    }

    /// The devices this host knows are revoked.
    pub(crate) fn revocations(&self) -> RevocationList {
        self.identity.revocations.read_recover().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Pending>> {
        self.pending.lock_recover()
    }

    /// Admit a request into the pending set, or say why not. Returns its
    /// digest, pairing code and a receiver that sees the answer.
    pub fn enqueue(&self, request: CertificationRequest, now_ns: u64) -> Result<Queued, String> {
        self.enqueue_with(request, None, now_ns, false)
    }

    /// [`Self::enqueue`], also for a member of a workspace this host serves
    /// (`member`: the request names such a member's root), whose own devices
    /// answer it through [`Self::member_approve`].
    pub fn enqueue_member(
        &self,
        request: CertificationRequest,
        now_ns: u64,
        member: bool,
    ) -> Result<Queued, String> {
        self.enqueue_with(request, None, now_ns, member)
    }

    /// Answer a member's root-signed approver command: their own pending
    /// requests, an agent they are asked to approve, or a decision on one of
    /// them.
    pub fn member_approve(&self, approval: MemberApproval, now_ns: u64) -> ApproverReply {
        let refused = |reason: &str| ApproverReply::Refused {
            reason: reason.into(),
        };
        if let Err(reason) = approval.verify(now_ns) {
            return refused(reason);
        }
        let user = approval.user_public_key;
        match approval.request {
            ApproverRequest::ListPending | ApproverRequest::AwaitPending { .. } => {
                ApproverReply::Pending(self.pending_of(&user, now_ns))
            }
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
            ),
            request @ (ApproverRequest::PushKey | ApproverRequest::Notify(_)) => {
                self.push(&user, request)
            }
            // A delegate's request is approved by any device of the person
            // the delegate acts for, its root signing the certificate and the
            // delegation together.
            ApproverRequest::DelegateProposals => {
                ApproverReply::DelegateProposals(self.proposals_of(&user, now_ns))
            }
        }
    }

    /// Answer `entry` with `reply`, keeping a certificate it issues in its person's record.
    fn settle(&self, entry: &Pending, reply: CertifyReply) {
        self.record_issued(&reply);
        entry.answer.send_replace(Some(reply));
        self.moved();
    }

    /// A request was queued or answered.
    fn moved(&self) {
        self.moved
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Wait until `user`'s waiting requests are other than `known`, their
    /// digests, or [`AWAIT_PENDING_NS`] passed.
    async fn until_moved(&self, user: &[u8; 32], known: &[[u8; 32]]) {
        let known: std::collections::BTreeSet<&[u8; 32]> = known.iter().collect();
        let mut moved = self.moved.subscribe();
        let waiting = async {
            loop {
                let waiting = self.pending_of(user, unix_now_ns());
                if waiting.len() != known.len()
                    || waiting
                        .iter()
                        .any(|entry| !known.contains(&entry.request_digest))
                {
                    return;
                }
                if moved.changed().await.is_err() {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_nanos(AWAIT_PENDING_NS), waiting).await;
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

    /// What the delegate requests `user` waits to answer ask for.
    fn proposals_of(&self, user: &[u8; 32], now_ns: u64) -> Vec<PendingDelegate> {
        self.lock()
            .values()
            .filter(|entry| {
                entry.entry.request.user_public_key == *user
                    && entry.entry.request.expires_at_ns > now_ns
                    && !entry.decided()
            })
            .filter_map(|entry| {
                Some(PendingDelegate {
                    request_digest: entry.entry.request_digest,
                    proposal: entry.agent.clone()?,
                })
            })
            .collect()
    }

    /// Queue a request for an agent this host runs for `request`'s person,
    /// carrying what that person proposed. Only an `IssueDelegate` decision
    /// answers it; wait for it with [`Self::delegate_answer`]. The host
    /// queues it for whoever asked, already admitted to the workspace the
    /// grant is for.
    pub fn enqueue_delegate(
        &self,
        request: CertificationRequest,
        proposal: DelegateProposal,
        now_ns: u64,
    ) -> Result<([u8; 32], String), String> {
        if proposal.delegate.agent_id().is_some_and(str::is_empty)
            || proposal.scope.workspaces.is_empty()
        {
            return Err("a delegate proposal names its delegate and workspaces".into());
        }
        self.enqueue_with(request, Some(proposal), now_ns, true)
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
    ) -> Result<Queued, String> {
        request
            .verify_at(now_ns)
            .map_err(|error| error.to_string())?;
        if request.user_public_key != self.identity.user_public_key && !member {
            return Err("this host serves another user identity".into());
        }
        if self
            .revocations()
            .is_revoked(&request.user_public_key, &request.device_public_key)
        {
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
        // The name the new device reads by: its own when no other device of
        // the person has it, else the next number of it. A device never has
        // to ask again under another name, so the person's device list never
        // shows two alike.
        let label = self.free_label(
            &request.user_public_key,
            request.device_label.trim(),
            now_ns,
        );
        let digest = request.digest().map_err(|error| error.to_string())?;
        let pairing_code = request.pairing_code().map_err(|error| error.to_string())?;
        // Signing in: the request is one a page's sign-in ticket here expects, told by the
        // ticket's own direction rather than from the request's shape. The devices that
        // approve it list it as such.
        #[cfg(feature = "web")]
        let sign_in = request.invite != [0; 16]
            && self
                .codes
                .get()
                .is_some_and(|codes| codes.expects(&request, now_ns));
        #[cfg(not(feature = "web"))]
        let sign_in = false;
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
                    sign_in,
                },
                answer,
                label,
                agent,
                delegation: None,
            },
        );
        drop(pending);
        self.moved();
        Ok((digest, pairing_code, receiver))
    }

    /// The name a device asking under `base` takes: `base` itself when no
    /// other device of `person` reads by it, else the first numbered one
    /// (`Laptop 2`, `Laptop 3`, …). Requests still open are named too, so two
    /// devices pairing at once read apart.
    fn free_label(&self, person: &[u8; 32], base: &str, now_ns: u64) -> String {
        if base.is_empty() {
            return String::new();
        }
        let waiting: Vec<String> = {
            let pending = self.lock();
            pending
                .values()
                .filter(|entry| {
                    !entry.decided()
                        && entry.entry.request.user_public_key == *person
                        && entry.entry.request.expires_at_ns > now_ns
                })
                .map(|entry| entry.label.clone())
                .collect()
        };
        let taken = |name: &str| {
            waiting.iter().any(|label| label == name)
                || self
                    .records
                    .get()
                    .is_some_and(|records| records.device_label_taken(person, name))
        };
        let mut last = numbered_label(base, 0);
        for n in 1..=MAX_NUMBERED_LABELS {
            last = numbered_label(base, n);
            if !taken(&last) {
                return last;
            }
        }
        last
    }

    /// Whether `user` is the person this host runs for, or a member of a
    /// workspace it serves: whose devices it queues, and whose root may answer
    /// for them.
    async fn admits(&self, user: [u8; 32]) -> bool {
        if user == self.identity.user_public_key {
            return true;
        }
        // One answer for who is a member here: each workspace's governance
        // names its people by their roots, whatever devices of theirs it
        // lists now, so a member whose browser forgot the site is still one.
        match &self.workspaces {
            Some(workspaces) => {
                crate::workspace_access::admits_person(
                    workspaces,
                    self.identity.user_public_key,
                    user,
                )
                .await
            }
            None => false,
        }
    }

    /// Whether `cert` is a device of a person this host serves that may keep
    /// their push subscriptions: one of their own (no agent's key, no
    /// stand-in), unrevoked here. The host's own person goes through their
    /// admitting devices alone ([`Self::serve_approver`]).
    async fn person_device(&self, cert: &DeviceCert) -> bool {
        cert.user_public_key != self.identity.user_public_key
            && workspace_access::person_device(cert)
            && !self
                .revocations()
                .is_revoked(&cert.user_public_key, &cert.device_public_key)
            && self.admits(cert.user_public_key).await
    }

    /// One approver command from a member's root, or from a device of theirs
    /// about their push subscriptions, answered once.
    async fn serve_member(
        &self,
        connection: &Link,
        mut send: LinkSend,
        approval: MemberApproval,
    ) -> Result<(), AcceptError> {
        let device_may = match &approval.device {
            None => true,
            Some(cert) => self.person_device(cert).await,
        };
        let reply = if !device_may {
            ApproverReply::Refused {
                reason: "this device approves nothing here".into(),
            }
        } else if self.admits(approval.user_public_key).await {
            if let ApproverRequest::AwaitPending { known } = &approval.request
                && approval.verify(unix_now_ns()).is_ok()
            {
                self.until_moved(&approval.user_public_key, known).await;
            }
            self.member_approve(approval, unix_now_ns())
        } else {
            // Said here as well as to the page: a person who cannot approve
            // their own device is stuck at the entry, and what the page shows
            // them names none of this.
            tracing::warn!(
                person = %hex::encode(approval.user_public_key),
                "a device approval was refused: the person is in no workspace this host serves"
            );
            ApproverReply::Refused {
                reason: "only a member approves devices here".into(),
            }
        };
        write_frame(&mut send, &encode(&reply)?)
            .await
            .map_err(|error| io(&error))?;
        finish(connection, send).await
    }

    async fn serve_requester(
        &self,
        connection: &Link,
        mut send: LinkSend,
        request: CertificationRequest,
    ) -> Result<(), AcceptError> {
        let expires_at_ns = request.expires_at_ns;
        let now = unix_now_ns();
        // A member of a workspace this host serves queues their own devices
        // beside the host's own, answered by their root rather than the
        // owner's. A tab signing in with a ticket pairs with the invite its
        // ticket named before a workspace here lets its person in: the device
        // that approved it is the one that answers, so its request is queued
        // the same way.
        let member = self.admits(request.user_public_key).await
            || self
                .codes
                .get()
                .is_some_and(|codes| request.invite != [0; 16] && codes.expects(&request, now));
        let queued = self.enqueue_member(request, now, member);
        let (digest, pairing_code, answer) = match queued {
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
            () = connection.closed() => {
                self.lock().remove(&digest);
                self.moved();
                return Ok(());
            }
        };
        self.lock().remove(&digest);
        write_frame(&mut send, &encode(&reply)?)
            .await
            .map_err(|error| io(&error))?;
        finish(connection, send).await
    }

    async fn serve_approver(
        &self,
        connection: &Link,
        mut send: LinkSend,
        mut recv: tilo_server_client::link::LinkRecv,
        hello: auth::Hello,
    ) -> Result<(), AcceptError> {
        // The owner's devices that may admit devices approve everything;
        // any other device of a person this host serves answers only that
        // person's questions. Checked before the server signs anything for
        // the approver: the certificate first, then whose device it is.
        let peer = self
            .identity
            .authenticate_hello(
                EndpointTrust::Fixed(HandshakeTrust::AnyCertifiedDevice),
                &hello,
                &mut recv,
                &mut send,
                MAX_CERTIFY_FRAME_BYTES,
            )
            .await
            .map_err(|error| io(&error))?;
        let owner = peer.user_public_key == self.identity.user_public_key
            && peer.cert.capabilities.can_admit_devices;
        if !owner && !self.person_device(&peer.cert).await {
            return Err(io(&"this device approves nothing here"));
        }
        while let Some(frame) = read_frame_or_eof(&mut recv, MAX_CERTIFY_FRAME_BYTES)
            .await
            .map_err(|error| io(&error))?
        {
            let request: ApproverRequest =
                postcard::from_bytes(&frame).map_err(|error| io(&error))?;
            let reply = if owner {
                self.decide(request, unix_now_ns(), &self.identity.user_public_key)
            } else {
                self.answer_for_device(&peer.user_public_key, request)
            };
            write_frame(&mut send, &encode(&reply)?)
                .await
                .map_err(|error| io(&error))?;
        }
        finish(connection, send).await
    }

    /// A command from a device of `person` (not the host's own): their push
    /// subscriptions; nothing of pairing.
    fn answer_for_device(&self, person: &[u8; 32], request: ApproverRequest) -> ApproverReply {
        match request {
            request @ (ApproverRequest::PushKey | ApproverRequest::Notify(_)) => {
                self.push(person, request)
            }
            _ => ApproverReply::Refused {
                reason: "only the person's root pairs devices".into(),
            },
        }
    }

    /// The answer issuing `cert` for `entry`, under the name the server gave it.
    #[cfg(feature = "web")]
    fn issued(&self, entry: &Pending, cert: DeviceCert) -> CertifyReply {
        CertifyReply::Issued {
            cert,
            server_cert: self.identity.cert.clone(),
            label: entry.label.clone(),
        }
    }

    /// Answer `request` from an approver of `approver`'s root, who decides
    /// only requests naming that root.
    fn decide(&self, request: ApproverRequest, now_ns: u64, approver: &[u8; 32]) -> ApproverReply {
        let refused = |reason: &str| ApproverReply::Refused {
            reason: reason.into(),
        };
        match request {
            // The owner's devices see the owner's requests; a member's are
            // answered by the member (`member_approve`).
            ApproverRequest::ListPending | ApproverRequest::AwaitPending { .. } => {
                ApproverReply::Pending(self.pending_of(&self.identity.user_public_key, now_ns))
            }
            ApproverRequest::DelegateProposals => ApproverReply::DelegateProposals(
                self.proposals_of(&self.identity.user_public_key, now_ns),
            ),
            request @ (ApproverRequest::PushKey | ApproverRequest::Notify(_)) => {
                self.push(&self.identity.user_public_key, request)
            }
            ApproverRequest::Decide {
                request_digest,
                decision,
            } => {
                let mut pending = self.lock();
                let Some(entry) = pending.get_mut(&request_digest).filter(|entry| {
                    !entry.decided()
                        && entry.entry.request.expires_at_ns > now_ns
                        && entry.entry.request.user_public_key == *approver
                }) else {
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
                        if let Err(error) = delegation.verify(approver, now_ns) {
                            return refused(&error.to_string());
                        }
                        if let Err(reason) = proposal.admits(&delegation) {
                            return refused(reason);
                        }
                        entry.delegation = Some(delegation);
                        CertifyReply::Issued {
                            cert,
                            server_cert: self.identity.cert.clone(),
                            label: entry.label.clone(),
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
                            server_cert: self.identity.cert.clone(),
                            label: entry.label.clone(),
                        }
                    }
                };
                self.settle(entry, reply);
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
    }
}

async fn finish(connection: &Link, mut send: LinkSend) -> Result<(), AcceptError> {
    send.finish();
    let _ = tokio::time::timeout(CLOSE_GRACE, connection.closed()).await;
    Ok(())
}

impl ProtocolHandler for CertificationProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.serve(&Link::iroh(connection)).await
    }
}

impl CertificationProtocol {
    /// Serve the one conversation `connection` opens with, whichever carrier
    /// it came over: a connection on the certification ALPN, or a
    /// WebTransport session at its path.
    ///
    /// # Errors
    ///
    /// The conversation did not decode, or its stream failed.
    pub(crate) async fn serve(&self, connection: &Link) -> Result<(), AcceptError> {
        let (send, recv) = connection.accept_bi().await.map_err(|error| io(&error))?;
        self.converse(connection, send, recv).await
    }

    /// Serve every conversation `connection` carries, each on a stream of its
    /// own, until it closes: what a device that keeps its session with the
    /// certification asks on
    /// ([`tilo_server_protocol::web::LINK_CERTIFY_KEPT`]). Each conversation
    /// says who asks by its own signatures, as the one conversation of a
    /// connection does; the session a device keeps says nothing of that.
    /// What still waits when the session closes is answered nothing.
    #[cfg(feature = "web")]
    pub(crate) async fn serve_every(&self, connection: &Link) {
        let mut conversations = tokio::task::JoinSet::new();
        while let Ok((send, recv)) = connection.accept_bi().await {
            // Those that ended are let go of as the next one arrives.
            while conversations.try_join_next().is_some() {}
            let (certification, connection) = (self.clone(), connection.clone());
            conversations.spawn(async move {
                if let Err(error) = certification.converse(&connection, send, recv).await {
                    tracing::debug!(%error, "a certification conversation on a kept session ended");
                }
            });
        }
    }

    /// The conversation a device opened on `send` and `recv`, streams of
    /// `connection`.
    async fn converse(
        &self,
        connection: &Link,
        send: LinkSend,
        mut recv: tilo_server_client::link::LinkRecv,
    ) -> Result<(), AcceptError> {
        let open: CertifyOpen =
            postcard::from_bytes(&read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await?)
                .map_err(|error| io(&error))?;
        match open {
            CertifyOpen::Request(request) => self.serve_requester(connection, send, request).await,
            CertifyOpen::Approver(hello) => {
                self.serve_approver(connection, send, recv, hello).await
            }
            CertifyOpen::Member(approval) => self.serve_member(connection, send, approval).await,
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
    #![allow(
        clippy::significant_drop_tightening,
        reason = "the protocol is the fixture each test drives to its end"
    )]

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, UserIdentity, sign_device_cert};

    fn protocol() -> CertificationProtocol {
        let (root, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "home-server".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        CertificationProtocol::new(
            LiveIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                cert,
                signing_key: key,
                revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            },
            None,
        )
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
        use tilo_user_identity::StatementSigner as _;
        let user_public_key = member.verifying_key().to_bytes();
        let issued_at_ns = unix_now_ns();
        let message =
            MemberApproval::signed_digest(&user_public_key, issued_at_ns, &request).unwrap();
        MemberApproval {
            user_public_key,
            issued_at_ns,
            request,
            signature: member.sign_digest(&message).unwrap(),
            device: None,
        }
    }

    #[tokio::test]
    async fn a_requester_hears_of_expiry_when_its_request_expires() {
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
        let expires_at_ns = request.expires_at_ns;
        let (_, _, answer) = protocol.enqueue(request, now).unwrap();
        // The requester waits the request's own lifetime, as `serve_requester`
        // does, and no longer.
        let life = Duration::from_nanos(expires_at_ns.saturating_sub(unix_now_ns()));
        let started = tokio::time::Instant::now();
        let reply = tokio::time::timeout(Duration::from_secs(20), answered(answer, life))
            .await
            .expect("the wait ends when the request expires");
        assert_eq!(reply, CertifyReply::Expired);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the request's lifetime bounds the wait, not the caller's patience"
        );
    }

    /// A person waiting for their next device hears of it as it asks: the wait ends with the
    /// request, not with a timer.
    #[tokio::test]
    async fn waiting_for_a_request_ends_when_one_is_queued() {
        let protocol = protocol();
        let member = ed25519_dalek::SigningKey::from_bytes(&[15; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[16; 32]);
        let user = member.verifying_key().to_bytes();
        let waiting = {
            let protocol = protocol.clone();
            tokio::spawn(async move { protocol.until_moved(&user, &[]).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "nothing waits yet");
        let started = tokio::time::Instant::now();
        let (_, code, _) = protocol
            .enqueue_member(member_request(&member, &device), unix_now_ns(), true)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait ends as the request arrives")
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let ApproverReply::Pending(theirs) = protocol.member_approve(
            signed(&member, ApproverRequest::AwaitPending { known: Vec::new() }),
            unix_now_ns(),
        ) else {
            panic!("a list");
        };
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].pairing_code, code);
        // What the approver already holds does not end a wait: only a change does.
        let known = vec![theirs[0].request_digest];
        let unchanged = {
            let protocol = protocol.clone();
            let known = known.clone();
            tokio::spawn(async move { protocol.until_moved(&user, &known).await })
        };
        tokio::task::yield_now().await;
        assert!(!unchanged.is_finished(), "nothing changed yet");
        let deny = ApproverRequest::Decide {
            request_digest: theirs[0].request_digest,
            decision: ApproverDecision::Deny,
        };
        assert_eq!(
            protocol.member_approve(signed(&member, deny), unix_now_ns()),
            ApproverReply::Decided
        );
        tokio::time::timeout(Duration::from_secs(5), unchanged)
            .await
            .expect("the wait ends as the request is answered")
            .unwrap();
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
            .enqueue_member(request.clone(), unix_now_ns(), true)
            .unwrap();
        let owner = protocol.identity.user_public_key;
        let ApproverReply::Pending(listed) =
            protocol.decide(ApproverRequest::ListPending, unix_now_ns(), &owner)
        else {
            panic!("a list");
        };
        assert!(
            listed.is_empty(),
            "the owner never sees a member's requests"
        );
        let ApproverReply::Pending(theirs) =
            protocol.member_approve(signed(&member, ApproverRequest::ListPending), unix_now_ns())
        else {
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
                protocol.member_approve(signed(&other, deny.clone()), unix_now_ns()),
                ApproverReply::Refused { .. }
            ),
            "another member cannot answer it"
        );
        assert!(
            matches!(
                protocol.decide(deny, unix_now_ns(), &owner),
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
            protocol.member_approve(signed(&member, issue), unix_now_ns()),
            ApproverReply::Decided
        );
        let reply = protocol
            .lock()
            .get(&digest)
            .and_then(|entry| entry.answer.borrow().clone());
        let Some(CertifyReply::Issued {
            cert: delivered, ..
        }) = reply
        else {
            panic!("issued");
        };
        assert_eq!(delivered, cert);
    }

    /// A member's agent, asked for by the host on their behalf: their root
    /// lists the proposal and signs the agent's certificate and the
    /// delegation together, and the delegation is theirs, not the host's.
    #[tokio::test]
    async fn a_member_approves_their_own_agent() {
        use tilo_server_protocol::WorkspaceId;
        use tilo_server_protocol::delegation::{Delegate, DelegateScope};

        let protocol = protocol();
        let member = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let agent = ed25519_dalek::SigningKey::from_bytes(&[22; 32]);
        let now = unix_now_ns();
        let request = member_request(&member, &agent);
        let proposal = DelegateProposal {
            delegate: Delegate::Agent {
                agent_id: "scout".into(),
            },
            label: "Scout".into(),
            color: [1, 2, 3],
            scope: DelegateScope {
                workspaces: vec![
                    WorkspaceId::parse("6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21").unwrap(),
                ],
                apps: Vec::new(),
            },
            expires_at_ns: now.saturating_add(60_000_000_000),
        };
        let (digest, _) = protocol
            .enqueue_delegate(request.clone(), proposal.clone(), now)
            .unwrap();
        assert_eq!(
            protocol.decide(
                ApproverRequest::DelegateProposals,
                now,
                &protocol.identity.user_public_key
            ),
            ApproverReply::DelegateProposals(Vec::new()),
            "the host's own approvers see no agent of another person's"
        );
        let ApproverReply::DelegateProposals(listed) =
            protocol.member_approve(signed(&member, ApproverRequest::DelegateProposals), now)
        else {
            panic!("the member's proposals");
        };
        assert_eq!(
            listed,
            vec![PendingDelegate {
                request_digest: digest,
                proposal: proposal.clone()
            }]
        );
        let cert = tilo_user_identity::issue_device_cert(
            &request,
            &member,
            &CertificationPolicy::agent(),
            &RevocationList::new(),
            now,
        )
        .unwrap();
        let delegation = proposal
            .sign(&member, request.device_public_key, now)
            .unwrap();
        let decide = |delegation: Delegation| ApproverRequest::Decide {
            request_digest: digest,
            decision: ApproverDecision::IssueDelegate {
                cert: cert.clone(),
                delegation,
            },
        };
        // A delegation the host's root signed is another person's: refused.
        assert!(matches!(
            protocol.member_approve(
                signed(
                    &member,
                    decide(
                        proposal
                            .sign(
                                &protocol.identity.signing_key,
                                request.device_public_key,
                                now
                            )
                            .unwrap()
                    )
                ),
                now
            ),
            ApproverReply::Refused { .. }
        ));
        assert_eq!(
            protocol.member_approve(signed(&member, decide(delegation.clone())), now),
            ApproverReply::Decided
        );
        let (delivered, delivered_delegation) = protocol.delegate_answer(&digest).await.unwrap();
        assert_eq!(delivered, cert);
        assert_eq!(delivered_delegation, delegation);
        let member_root = member.verifying_key().to_bytes();
        delivered_delegation.verify(&member_root, now).unwrap();
        assert_eq!(
            delivered_delegation.verify(&protocol.identity.user_public_key, now),
            Err(tilo_server_protocol::delegation::DelegationError::WrongUser),
            "the agent is kept by the member, not the host"
        );
    }

    /// Another member cannot answer an agent that is not theirs: they list
    /// none of it and their decision names no such request.
    #[test]
    fn a_member_cannot_approve_another_members_agent() {
        use tilo_server_protocol::WorkspaceId;
        use tilo_server_protocol::delegation::{Delegate, DelegateScope};

        let protocol = protocol();
        let member = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[24; 32]);
        let agent = ed25519_dalek::SigningKey::from_bytes(&[25; 32]);
        let now = unix_now_ns();
        let proposal = DelegateProposal {
            delegate: Delegate::Agent {
                agent_id: "scout".into(),
            },
            label: "Scout".into(),
            color: [1, 2, 3],
            scope: DelegateScope {
                workspaces: vec![
                    WorkspaceId::parse("6a1f8f2e-4c55-4b0e-9f43-0d6c1b7a9e21").unwrap(),
                ],
                apps: Vec::new(),
            },
            expires_at_ns: now.saturating_add(60_000_000_000),
        };
        let (digest, _) = protocol
            .enqueue_delegate(member_request(&member, &agent), proposal, now)
            .unwrap();
        assert_eq!(
            protocol.member_approve(signed(&other, ApproverRequest::DelegateProposals), now),
            ApproverReply::DelegateProposals(Vec::new())
        );
        assert_eq!(
            protocol.member_approve(
                signed(
                    &other,
                    ApproverRequest::Decide {
                        request_digest: digest,
                        decision: ApproverDecision::Deny,
                    }
                ),
                now
            ),
            ApproverReply::Refused {
                reason: "no such pending request".into()
            }
        );
    }

    #[test]
    fn a_computer_asking_under_a_name_another_waits_under_takes_the_next_number() {
        let protocol = protocol();
        let now = unix_now_ns();
        let request = |device: &ed25519_dalek::SigningKey, nonce: [u8; 16]| {
            CertificationRequest {
                version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
                user_public_key: protocol.identity.user_public_key,
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Claude Code on laptop".into(),
                requested: DeviceCapabilities::member(),
                nonce,
                invite: [0; 16],
                issued_at_ns: now,
                expires_at_ns: now.checked_add(60_000_000_000).unwrap(),
                signature: [0; 64],
            }
            .sign(device)
            .unwrap()
        };
        let first = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let second = ed25519_dalek::SigningKey::from_bytes(&[12; 32]);
        protocol.enqueue(request(&first, [1; 16]), now).unwrap();
        protocol.enqueue(request(&second, [2; 16]), now).unwrap();
        let name_of = |key: &ed25519_dalek::SigningKey| {
            protocol
                .lock()
                .values()
                .find(|entry| {
                    entry.entry.request.device_public_key == key.verifying_key().to_bytes()
                })
                .map(|entry| entry.label.clone())
                .expect("queued")
        };
        assert_eq!(name_of(&first), "Claude Code on laptop");
        assert_eq!(
            name_of(&second),
            "Claude Code on laptop 2",
            "the host names two devices set up alike apart"
        );
    }

    /// A page's sign-in tickets expect one invite: the request that names it is listed for the
    /// approving devices as signing a tab in, and a device pairing is not — told by the ticket's
    /// direction, never guessed from a request.
    #[cfg(feature = "web")]
    #[test]
    fn only_a_request_a_sign_in_ticket_expects_is_listed_as_signing_in() {
        struct Expects([u8; 16]);

        impl crate::certification::TabRequests for Expects {
            fn expects(&self, request: &CertificationRequest, _now_ns: u64) -> bool {
                request.invite == self.0
            }
        }

        let protocol = protocol();
        let invite = [31; 16];
        protocol.set_tab_requests(Arc::new(Expects(invite)));
        let now = unix_now_ns();
        let request = |device: &ed25519_dalek::SigningKey, invite: [u8; 16], nonce: [u8; 16]| {
            CertificationRequest {
                version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
                user_public_key: protocol.identity.user_public_key,
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Browser".into(),
                requested: DeviceCapabilities::member(),
                nonce,
                invite,
                issued_at_ns: now,
                expires_at_ns: now.checked_add(60_000_000_000).unwrap(),
                signature: [0; 64],
            }
            .sign(device)
            .unwrap()
        };
        let tab = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let paired = ed25519_dalek::SigningKey::from_bytes(&[33; 32]);
        protocol
            .enqueue(request(&tab, invite, [1; 16]), now)
            .unwrap();
        protocol
            .enqueue(request(&paired, [2; 16], [2; 16]), now)
            .unwrap();
        let ApproverReply::Pending(listed) = protocol.decide(
            ApproverRequest::ListPending,
            now,
            &protocol.identity.user_public_key,
        ) else {
            panic!("a list");
        };
        let sign_in_of = |device: &ed25519_dalek::SigningKey| {
            listed
                .iter()
                .find(|entry| entry.request.device_public_key == device.verifying_key().to_bytes())
                .map(|entry| entry.sign_in)
                .expect("listed")
        };
        assert!(sign_in_of(&tab), "the request a sign-in ticket expects");
        assert!(!sign_in_of(&paired), "a device pairing signs no tab in");
    }
}
