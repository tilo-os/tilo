//! Client side of server-mediated device certification
//! ([`tilo_server_protocol::certify`]).
//!
//! [`certify`] is what a device without a certificate runs: it signs one
//! request, presents it to its host and waits for a root holder to hand over
//! a certificate. [`request_certification`] is the same conversation over an
//! endpoint the caller already has, [`Approver`] what a root-holding device
//! runs to list and answer pending requests, and [`MemberHost`] where a
//! page holding a member's root does the same for that member's own devices:
//! over the session it holds, or [`MemberApprover`] before it holds one.

#[cfg(not(target_arch = "wasm32"))]
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
#[cfg(not(target_arch = "wasm32"))]
use iroh_base::EndpointAddr;
#[cfg(not(target_arch = "wasm32"))]
use tilo_server_protocol::auth::{self, Hello, HelloAck};
#[cfg(not(target_arch = "wasm32"))]
use tilo_server_protocol::certify::{
    ApproverDecision, ApproverRequest, CERTIFY_ALPN, ConfirmationAnswer, PendingCertification,
    PendingConfirmation, PendingDelegate,
};
use tilo_server_protocol::certify::{
    ApproverReply, CertifyOpen, CertifyReply, MAX_CERTIFY_FRAME_BYTES, MemberApproval,
};
use tilo_server_protocol::web::LINK_CERTIFY_KEPT;
use tilo_user_identity::{
    CERTIFICATION_CLOCK_ALLOWANCE_NS, CERTIFICATION_SCHEMA_VERSION, CertificationPolicy,
    CertificationRequest, DeviceCapabilities, DeviceCert, RevocationList, verify_device_cert_at,
    verify_issued_cert,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::CertifyError;
use crate::frame::{read_frame, write_frame};
use crate::host::Host;
use crate::link::{Dialer, Link, Purpose};
use crate::signer::DeviceSigner;
#[cfg(not(target_arch = "wasm32"))]
use crate::signer::signed_ack2;

/// Longest a certification request waits for an approver.
pub const CERTIFICATION_WAIT_NS: u64 = 5 * 60 * 1_000_000_000;

/// How far a certificate's issue time may run ahead of this device's clock:
/// the host stamps its own nanosecond clock, and a page's clock is coarser
/// (a browser's is whole milliseconds), so a certificate checked the moment
/// it arrives can look issued in the future.
const ISSUE_SKEW_NS: u64 = 5 * 1_000_000_000;

/// How a certification request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "returned once per certification, never kept in bulk"
)]
pub enum CertificationOutcome {
    /// A checked, root-signed certificate for the requesting device.
    Issued {
        cert: DeviceCert,
        /// The host's certificate, to pin for later sessions.
        host_cert: DeviceCert,
        /// The name the host gave the device: its own, or the next numbered
        /// one when another device of the person reads by that name already.
        label: String,
    },
    /// An approver declined, or the host refused.
    Denied(String),
    /// No approver answered in time.
    Expired,
}

fn codec(error: &impl ToString) -> CertifyError {
    CertifyError::Codec(error.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn iroh(error: &impl ToString) -> CertifyError {
    CertifyError::Iroh(error.to_string())
}

async fn read<T: serde::de::DeserializeOwned>(
    recv: &mut (impl AsyncRead + Unpin),
) -> Result<T, CertifyError> {
    postcard::from_bytes(&read_frame(recv, MAX_CERTIFY_FRAME_BYTES).await?)
        .map_err(|error| codec(&error))
}

async fn write<T: serde::Serialize + Sync>(
    send: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<(), CertifyError> {
    write_frame(
        send,
        &postcard::to_allocvec(value).map_err(|error| codec(&error))?,
    )
    .await?;
    Ok(())
}

/// Ask the host at `host` to certify the device behind `signer` as one of
/// `user_public_key`'s devices, labeled `label`, answering the pairing invite
/// `invite` ([0; 16] when none). `on_pending` receives the pairing code to
/// show while the person's other device answers. Resolves to this device's
/// certificate, the host's to pin, and the name the host gave the device (the
/// one asked for, or the next free numbered one).
///
/// # Errors
///
/// The host was not reached or refused, nobody answered in time
/// ([`CertifyError::Expired`]), or what it issued is not a certificate for
/// this device ([`request_certification`] checks it).
pub async fn certify(
    host: &Host,
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<(DeviceCert, DeviceCert, String), CertifyError> {
    let request = signed_request(user_public_key, label, invite, signer).await?;
    let dialer = Dialer::new(host.clone(), None);
    let outcome = async {
        let link = dialer.link(Purpose::Certify).await?;
        let (mut send, mut recv) = link.open_bi().await?;
        let outcome = request_over(&mut send, &mut recv, request, on_pending).await;
        link.close("");
        outcome
    }
    .await;
    dialer.close().await;
    match outcome? {
        CertificationOutcome::Issued {
            cert,
            host_cert,
            label,
        } => Ok((cert, host_cert, label)),
        CertificationOutcome::Denied(reason) => Err(CertifyError::Denied(reason)),
        CertificationOutcome::Expired => Err(CertifyError::Expired),
    }
}

/// The request a device without a certificate signs.
///
/// It is dated [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] before this device's
/// clock. A host from before hosts allowed the clocks of a pairing to differ
/// checks the date exactly, and refused a request dated now whenever its own
/// clock ran a little behind this device's; such a host stays in use while
/// its pages move on. The wait for an approver is counted from now, as
/// before, so the request lasts that much longer than the wait, well inside
/// the longest a request may. With a host that has the allowance, a device
/// up to twice that far ahead still pairs; the expiry is exact everywhere.
async fn signed_request(
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
) -> Result<CertificationRequest, CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let unsigned = CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key,
        device_public_key: signer.device_public_key(),
        device_label: label.to_owned(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite,
        issued_at_ns: now.saturating_sub(CERTIFICATION_CLOCK_ALLOWANCE_NS),
        expires_at_ns: now.saturating_add(CERTIFICATION_WAIT_NS),
        signature: [0; 64],
    };
    let digest = unsigned
        .signing_digest()
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    unsigned
        .with_signature(signature)
        .map_err(|error| CertifyError::Request(error.to_string()))
}

/// When to check a certificate issued at `issued_at_ns`, reading `now_ns`.
fn checked_at(now_ns: u64, issued_at_ns: u64) -> u64 {
    if issued_at_ns > now_ns && issued_at_ns.saturating_sub(now_ns) <= ISSUE_SKEW_NS {
        issued_at_ns
    } else {
        now_ns
    }
}

/// Ask the host at `host` to certify the device that signed `request`, and
/// wait for the answer. `on_pending` receives the pairing code once the host
/// has queued the request; show it so the person approving can compare.
///
/// The certificate is checked here against the exact request and the
/// browser [`CertificationPolicy`], and the host certificate must be a valid
/// host certificate of the same user: a host cannot hand back a certificate
/// for another key, another user or wider capabilities.
#[cfg(not(target_arch = "wasm32"))]
pub async fn request_certification(
    endpoint: &Endpoint,
    host: EndpointAddr,
    request: CertificationRequest,
    on_pending: impl FnOnce(&str),
) -> Result<CertificationOutcome, CertifyError> {
    let connection = endpoint
        .connect(host, CERTIFY_ALPN)
        .await
        .map_err(|error| iroh(&error))?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| iroh(&error))?;
    let outcome = request_over(&mut send, &mut recv, request, on_pending).await;
    connection.close(0u32.into(), b"");
    outcome
}

/// [`request_certification`]'s conversation, on a stream already open to the
/// host's certification.
async fn request_over(
    send: &mut (impl AsyncWrite + Unpin),
    recv: &mut (impl AsyncRead + Unpin),
    request: CertificationRequest,
    on_pending: impl FnOnce(&str),
) -> Result<CertificationOutcome, CertifyError> {
    write(send, &CertifyOpen::Request(request.clone())).await?;
    let mut reply: CertifyReply = read(recv).await?;
    if let CertifyReply::Pending { pairing_code } = &reply {
        on_pending(pairing_code);
        reply = read(recv).await?;
    }
    match reply {
        CertifyReply::Issued {
            cert,
            host_cert,
            label,
        } => {
            let now = tilo_clock::unix_now_ns();
            verify_issued_cert(
                &request,
                &cert,
                &CertificationPolicy::browser(),
                &RevocationList::new(),
                checked_at(now, cert.issued_at_ns),
            )
            .map_err(|error| codec(&format!("issued certificate refused: {error}")))?;
            // A member pairs through any device of a workspace they share, so
            // it may be another person's: it must be a person's device, not
            // necessarily this root's.
            verify_device_cert_at(&host_cert, now)
                .map_err(|error| codec(&format!("host certificate refused: {error}")))?;
            if !host_cert.capabilities.can_own_workspaces {
                return Err(codec(&"the certificate is not a person's device"));
            }
            Ok(CertificationOutcome::Issued {
                cert,
                host_cert,
                label,
            })
        }
        CertifyReply::Denied { reason } => Ok(CertificationOutcome::Denied(reason)),
        CertifyReply::Expired => Ok(CertificationOutcome::Expired),
        CertifyReply::Pending { .. } => Err(codec(&"host sent a second pending reply")),
    }
}

/// An authenticated approver session with a host.
#[cfg(not(target_arch = "wasm32"))]
pub struct Approver {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

#[cfg(not(target_arch = "wasm32"))]
impl Approver {
    /// Open an approver session. `host` pins the host's endpoint, so it may
    /// be any person's device: the owner's admitting devices approve
    /// everything there, any other device of a person it serves answers only
    /// that person's questions, and the host refuses the rest.
    pub async fn connect(
        endpoint: &Endpoint,
        host: EndpointAddr,
        own_cert: DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: &RevocationList,
    ) -> Result<Self, CertifyError> {
        let connection = endpoint
            .connect(host, CERTIFY_ALPN)
            .await
            .map_err(|error| iroh(&error))?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(|error| iroh(&error))?;
        let mut client_nonce = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut client_nonce);
        write(
            &mut send,
            &CertifyOpen::Approver(Hello::new(own_cert, client_nonce)),
        )
        .await?;
        let ack: HelloAck =
            auth::decode_handshake(&read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await?)
                .map_err(|error| codec(&error))?;
        auth::verify_pinned_host_ack(&ack, revocations, tilo_clock::unix_now_ns(), &client_nonce)?;
        write(
            &mut send,
            &signed_ack2(signer, &ack.server_nonce)
                .await
                .map_err(CertifyError::Sign)?,
        )
        .await?;
        Ok(Self {
            connection,
            send,
            recv,
        })
    }

    async fn call(&mut self, request: &ApproverRequest) -> Result<ApproverReply, CertifyError> {
        write(&mut self.send, request).await?;
        read(&mut self.recv).await
    }

    /// Every request currently waiting for an approver.
    pub async fn list_pending(&mut self) -> Result<Vec<PendingCertification>, CertifyError> {
        match self.call(&ApproverRequest::ListPending).await? {
            ApproverReply::Pending(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided
            | ApproverReply::DelegateProposals(_)
            | ApproverReply::Confirmations(_)
            | ApproverReply::PushKey(_) => Err(codec(&"unexpected approver reply")),
        }
    }

    /// What every pending agent request proposes: shown beside the request
    /// and approved with [`ApproverDecision::IssueDelegate`].
    pub async fn list_agent_proposals(&mut self) -> Result<Vec<PendingDelegate>, CertifyError> {
        match self.call(&ApproverRequest::DelegateProposals).await? {
            ApproverReply::DelegateProposals(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided
            | ApproverReply::Pending(_)
            | ApproverReply::Confirmations(_)
            | ApproverReply::PushKey(_) => Err(codec(&"unexpected approver reply")),
        }
    }

    /// Every question an agent of the host is waiting on: shown as the
    /// host's confirmation sheet and answered with [`Self::answer`].
    pub async fn list_confirmations(&mut self) -> Result<Vec<PendingConfirmation>, CertifyError> {
        match self.call(&ApproverRequest::Confirmations).await? {
            ApproverReply::Confirmations(asked) => Ok(asked),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided
            | ApproverReply::Pending(_)
            | ApproverReply::DelegateProposals(_)
            | ApproverReply::PushKey(_) => Err(codec(&"unexpected approver reply")),
        }
    }

    /// Answer question `id` for its one invocation. `Ok(Refused)` means
    /// the question is gone (answered, expired or withdrawn).
    pub async fn answer(
        &mut self,
        id: [u8; 32],
        answer: ConfirmationAnswer,
    ) -> Result<ApproverReply, CertifyError> {
        self.call(&ApproverRequest::Answer { id, answer }).await
    }

    /// Answer one request. `Ok(Refused)` means the host did not accept the
    /// decision (an out-of-policy certificate, or the requester left).
    pub async fn decide(
        &mut self,
        request_digest: [u8; 32],
        decision: ApproverDecision,
    ) -> Result<ApproverReply, CertifyError> {
        self.call(&ApproverRequest::Decide {
            request_digest,
            decision,
        })
        .await
    }

    /// End the session.
    pub fn close(mut self) {
        let _ = self.send.finish();
        self.connection.close(0u32.into(), b"approver done");
    }
}

/// Where a member's root-signed approver commands ([`MemberApproval`]s)
/// reach a host. A device already holding a session to the host asks over
/// that session's endpoint (`tilo_session_client::Link`); a device with
/// none yet binds one of its own ([`MemberApprover`]).
pub trait MemberHost: Sync {
    /// The host's answer to `approval`.
    ///
    /// # Errors
    ///
    /// The host was not reached, or its answer did not decode. A host that
    /// turns the command away answers [`ApproverReply::Refused`].
    fn ask(
        &self,
        approval: &MemberApproval,
    ) -> impl std::future::Future<Output = Result<ApproverReply, CertifyError>> + Send;
}

/// Ask the host `approval` on `link`, a link to its certification
/// ([`Purpose::Certify`]), on a stream of its own.
///
/// # Errors
///
/// As [`MemberHost::ask`].
pub async fn ask_member(
    link: &Link,
    approval: &MemberApproval,
) -> Result<ApproverReply, CertifyError> {
    let (mut send, mut recv) = link.open_bi().await?;
    write(&mut send, &CertifyOpen::Member(approval.clone())).await?;
    read(&mut recv).await
}

/// The link a device keeps to its server's certification, and how many it
/// has dialled: an ask lets go only of the link it failed on.
#[derive(Default)]
struct Kept {
    link: Option<Link>,
    dialled: u64,
}

/// A device's asks of its server's certification ([`MemberApproval`]s), each
/// on a stream of its own on one link the device keeps.
///
/// Over WebTransport every link is a session of its own, and a browser gives
/// each a socket: Chrome gives it back when the session closes, `WebKit` keeps
/// it for as long as its network process lives. A page that asked every two
/// seconds on a link of its own filled that process with sockets until the
/// browser could open nothing. So a page offers to keep the link
/// ([`LINK_CERTIFY_KEPT`]) and asks on it for as long as it stands; a link
/// that failed is let go, and the next ask dials another. A server that does
/// not answer the offer serves one conversation per link and is asked as
/// before. Over iroh nothing is offered and nothing kept: a link there is a
/// connection of the endpoint the device already has, and costs no socket.
#[derive(Default)]
pub struct KeptAsks {
    kept: tokio::sync::Mutex<Kept>,
}

impl KeptAsks {
    /// The server's answer to `approval`, asked through `dialer`.
    ///
    /// # Errors
    ///
    /// The server was not reached, or its answer did not decode. A server
    /// that turns the command away answers [`ApproverReply::Refused`].
    pub async fn ask(
        &self,
        dialer: &Dialer,
        approval: &MemberApproval,
    ) -> Result<ApproverReply, CertifyError> {
        let mut first = true;
        loop {
            let (link, dialled, kept_before) = self.link(dialer).await?;
            let (mut send, mut recv) = match link.open_bi().await {
                Ok(streams) => streams,
                Err(error) => {
                    self.let_go(&link, dialled).await;
                    // A kept link that closed since the last ask heard
                    // nothing of this one: it is asked on a new link, once.
                    if kept_before && std::mem::take(&mut first) {
                        continue;
                    }
                    return Err(error.into());
                }
            };
            let reply = async {
                write(&mut send, &CertifyOpen::Member(approval.clone())).await?;
                // Said whole: the stream ends here, so the link holds no
                // more streams open than asks wait for an answer.
                send.finish();
                read(&mut recv).await
            }
            .await;
            if reply.is_err() || link.protocol() != Some(LINK_CERTIFY_KEPT) {
                self.let_go(&link, dialled).await;
            }
            return reply;
        }
    }

    /// The link to ask on, which dial of this device's it was, and whether it
    /// was kept from an earlier ask: the kept one, else one dialled now,
    /// which is kept when the server answers the offer to.
    async fn link(&self, dialer: &Dialer) -> Result<(Link, u64, bool), CertifyError> {
        let mut kept = self.kept.lock().await;
        if let Some(link) = &kept.link {
            return Ok((link.clone(), kept.dialled, true));
        }
        let link = dialer
            .link_offering(Purpose::Certify, &[LINK_CERTIFY_KEPT])
            .await?;
        kept.dialled = kept.dialled.saturating_add(1);
        if link.protocol() == Some(LINK_CERTIFY_KEPT) {
            kept.link = Some(link.clone());
        }
        Ok((link, kept.dialled, false))
    }

    /// Close `link`, the `dialled`th this device dialled, and keep it no
    /// longer.
    async fn let_go(&self, link: &Link, dialled: u64) {
        let mut kept = self.kept.lock().await;
        if kept.dialled == dialled {
            kept.link = None;
        }
        drop(kept);
        link.close("");
    }

    /// Let go of the kept link: the device asks nothing more.
    pub async fn close(&self) {
        let link = self.kept.lock().await.link.take();
        if let Some(link) = link {
            link.close("");
        }
    }
}

impl Drop for KeptAsks {
    fn drop(&mut self) {
        // A page's session closes only when it is told to: one left open
        // holds its socket for as long as the page lives.
        if let Some(link) = self.kept.get_mut().link.take() {
            link.close("");
        }
    }
}

/// A member's root asking one host its approver commands before the device
/// holds any session there: what a page runs to pair an agent's device
/// through a host it is not yet let into. It reaches the host as [`Host`]
/// says, from one endpoint bound on the first command; over WebTransport on
/// one link it keeps ([`KeptAsks`]), over iroh each command on a connection
/// of its own.
pub struct MemberApprover {
    dialer: Dialer,
    asks: KeptAsks,
}

impl MemberApprover {
    /// Commands for the host at `host`.
    #[must_use]
    pub fn new(host: Host) -> Self {
        Self {
            dialer: Dialer::new(host, None),
            asks: KeptAsks::default(),
        }
    }

    /// Let go of the host, waiting for it to hear.
    pub async fn close(&self) {
        self.asks.close().await;
        self.dialer.close().await;
    }
}

impl MemberHost for MemberApprover {
    async fn ask(&self, approval: &MemberApproval) -> Result<ApproverReply, CertifyError> {
        self.asks.ask(&self.dialer, approval).await
    }
}
