//! Client side of server-mediated device certification
//! ([`tilo_server_protocol::certify`]).
//!
//! [`request_certification_at`] delivers a signed request to its server and
//! waits for a checked response. [`request_certification`] runs the same
//! conversation over an endpoint the caller already has. A root-holding device
//! uses [`Approver`] to list and answer pending requests. [`MemberServer`] is where a
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
    ApproverDecision, ApproverRequest, CERTIFY_ALPN, PendingCertification, PendingDelegate,
};
use tilo_server_protocol::certify::{
    ApproverReply, CertifyOpen, CertifyReply, MAX_CERTIFY_FRAME_BYTES, MemberApproval,
};
use tilo_server_protocol::web::LINK_CERTIFY_KEPT;
use tilo_user_identity::{
    CertificationPolicy, CertificationRequest, DeviceCert, RevocationList, verify_device_cert_at,
    verify_issued_cert,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::CertifyError;
use crate::frame::{read_frame, write_frame};
use crate::host::ServerAddress;
use crate::link::{Dialer, Link, Purpose};
#[cfg(not(target_arch = "wasm32"))]
use crate::signer::{DeviceSigner, signed_ack2};

/// Longest a certification request waits for an approver.
pub const CERTIFICATION_WAIT_NS: u64 = 5 * 60 * 1_000_000_000;

/// How far a certificate's issue time may run ahead of this device's clock:
/// the server stamps its own nanosecond clock, and a page's clock is coarser
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
        /// The server's certificate, to pin for later sessions.
        server_cert: DeviceCert,
        /// The name the server gave the device: its own, or the next numbered
        /// one when another device of the person reads by that name already.
        label: String,
    },
    /// An approver declined, or the server refused.
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

async fn read_approver_reply(
    recv: &mut (impl AsyncRead + Unpin),
) -> Result<ApproverReply, CertifyError> {
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

/// Send a signed certification request to the server and wait for its checked
/// answer. The dialer chooses the carrier for this address and pins it.
/// `on_pending` receives the pairing code once the server queues the request.
///
/// # Errors
///
/// The server could not be reached, the exchange failed, or the answer was invalid.
pub async fn request_certification_at(
    server: &ServerAddress,
    request: CertificationRequest,
    on_pending: impl FnOnce(&str),
) -> Result<CertificationOutcome, CertifyError> {
    let dialer = Dialer::new(server.clone(), None);
    let outcome = async {
        let link = dialer.link(Purpose::Certify).await?;
        let (mut send, mut recv) = link.open_bi().await?;
        let outcome = request_over(&mut send, &mut recv, request, on_pending).await;
        link.close("");
        outcome
    }
    .await;
    dialer.close().await;
    outcome
}

/// When to check a certificate issued at `issued_at_ns`, reading `now_ns`.
fn checked_at(now_ns: u64, issued_at_ns: u64) -> u64 {
    if issued_at_ns > now_ns && issued_at_ns.saturating_sub(now_ns) <= ISSUE_SKEW_NS {
        issued_at_ns
    } else {
        now_ns
    }
}

/// Ask the server at `server` to certify the device that signed `request`, and
/// wait for the answer. `on_pending` receives the pairing code once the server
/// has queued the request; show it so the person approving can compare.
///
/// The certificate is checked here against the exact request and the
/// browser [`CertificationPolicy`], and the server certificate must be a valid
/// server certificate of the same user: a server cannot hand back a certificate
/// for another key, another user or wider capabilities.
#[cfg(not(target_arch = "wasm32"))]
pub async fn request_certification(
    endpoint: &Endpoint,
    server: EndpointAddr,
    request: CertificationRequest,
    on_pending: impl FnOnce(&str),
) -> Result<CertificationOutcome, CertifyError> {
    let connection = endpoint
        .connect(server, CERTIFY_ALPN)
        .await
        .map_err(|error| iroh(&error))?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| iroh(&error))?;
    let outcome = request_over(&mut send, &mut recv, request, on_pending).await;
    connection.close(0u32.into(), b"");
    outcome
}

/// [`request_certification`]'s conversation, on a stream already open to the
/// server's certification.
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
            server_cert,
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
            verify_device_cert_at(&server_cert, now)
                .map_err(|error| codec(&format!("server certificate refused: {error}")))?;
            if !server_cert.capabilities.can_own_workspaces {
                return Err(codec(&"the certificate is not a person's device"));
            }
            Ok(CertificationOutcome::Issued {
                cert,
                server_cert,
                label,
            })
        }
        CertifyReply::Denied { reason } => Ok(CertificationOutcome::Denied(reason)),
        CertifyReply::Expired => Ok(CertificationOutcome::Expired),
        CertifyReply::Pending { .. } => Err(codec(&"server sent a second pending reply")),
    }
}

/// An authenticated approver session with a server.
#[cfg(not(target_arch = "wasm32"))]
pub struct Approver {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

#[cfg(not(target_arch = "wasm32"))]
impl Approver {
    /// Open an approver session. `server` pins the server's endpoint, so it may
    /// be any person's device: the owner's admitting devices approve
    /// everything there, any other device of a person it serves answers only
    /// that person's questions, and the server refuses the rest.
    pub async fn connect(
        endpoint: &Endpoint,
        server: EndpointAddr,
        own_cert: DeviceCert,
        signer: &dyn DeviceSigner,
        revocations: &RevocationList,
    ) -> Result<Self, CertifyError> {
        let connection = endpoint
            .connect(server, CERTIFY_ALPN)
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
        read_approver_reply(&mut self.recv).await
    }

    /// Every request currently waiting for an approver.
    pub async fn list_pending(&mut self) -> Result<Vec<PendingCertification>, CertifyError> {
        match self.call(&ApproverRequest::ListPending).await? {
            ApproverReply::Pending(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided
            | ApproverReply::DelegateProposals(_)
            | ApproverReply::PushKey(_) => Err(codec(&"unexpected approver reply")),
        }
    }

    /// What every pending agent request proposes: shown beside the request
    /// and approved with [`ApproverDecision::IssueDelegate`].
    pub async fn list_agent_proposals(&mut self) -> Result<Vec<PendingDelegate>, CertifyError> {
        match self.call(&ApproverRequest::DelegateProposals).await? {
            ApproverReply::DelegateProposals(pending) => Ok(pending),
            ApproverReply::Refused { reason } => Err(codec(&reason)),
            ApproverReply::Decided | ApproverReply::Pending(_) | ApproverReply::PushKey(_) => {
                Err(codec(&"unexpected approver reply"))
            }
        }
    }

    /// Answer one request. `Ok(Refused)` means the server did not accept the
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
/// reach a server. A device already holding a session to the server asks over
/// that session's endpoint (`tilo_session_client::Link`); a device with
/// none yet binds one of its own ([`MemberApprover`]).
pub trait MemberServer: Sync {
    /// The server's answer to `approval`.
    ///
    /// # Errors
    ///
    /// The server was not reached, or its answer did not decode. A server that
    /// turns the command away answers [`ApproverReply::Refused`].
    fn ask(
        &self,
        approval: &MemberApproval,
    ) -> impl std::future::Future<Output = Result<ApproverReply, CertifyError>> + Send;
}

/// Ask the server `approval` on `link`, a link to its certification
/// ([`Purpose::Certify`]), on a stream of its own.
///
/// # Errors
///
/// As [`MemberServer::ask`].
pub async fn ask_member(
    link: &Link,
    approval: &MemberApproval,
) -> Result<ApproverReply, CertifyError> {
    let (mut send, mut recv) = link.open_bi().await?;
    write(&mut send, &CertifyOpen::Member(approval.clone())).await?;
    read_approver_reply(&mut recv).await
}

/// The link a device keeps to its server's certification, with which of its
/// dials it was, and how many it has dialled: an ask lets go only of the link
/// it failed on. Once closed it keeps none.
#[derive(Default)]
struct Kept {
    link: Option<(Link, u64)>,
    dialled: u64,
    closed: bool,
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
            let (link, dialled, kept_before, kept) = self.link(dialer).await?;
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
                read_approver_reply(&mut recv).await
            }
            .await;
            if reply.is_err() || !kept {
                self.let_go(&link, dialled).await;
            }
            return reply;
        }
    }

    /// The link to ask on, which dial of this device's it was, whether it was
    /// kept from an earlier ask, and whether it is kept now: the kept one, else
    /// one dialled now, which is kept when the server answers the offer to and
    /// no other ask kept one meanwhile.
    ///
    /// The dial is made outside the lock: an ask that waits for its server, or
    /// that its caller stopped polling there, keeps no other ask and no
    /// [`Self::close`] waiting behind it.
    async fn link(&self, dialer: &Dialer) -> Result<(Link, u64, bool, bool), CertifyError> {
        if let Some((link, dialled)) = &self.kept.lock().await.link {
            return Ok((link.clone(), *dialled, true, true));
        }
        let link = dialer
            .link_offering(Purpose::Certify, &[LINK_CERTIFY_KEPT])
            .await?;
        let mut kept = self.kept.lock().await;
        kept.dialled = kept.dialled.saturating_add(1);
        let dialled = kept.dialled;
        let keeps =
            !kept.closed && kept.link.is_none() && link.protocol() == Some(LINK_CERTIFY_KEPT);
        if keeps {
            kept.link = Some((link.clone(), dialled));
        }
        drop(kept);
        Ok((link, dialled, false, keeps))
    }

    /// Close `link`, the `dialled`th this device dialled, and keep it no
    /// longer.
    async fn let_go(&self, link: &Link, dialled: u64) {
        let mut kept = self.kept.lock().await;
        if kept.link.as_ref().is_some_and(|(_, at)| *at == dialled) {
            kept.link = None;
        }
        drop(kept);
        link.close("");
    }

    /// Let go of the kept link: the device asks nothing more.
    pub async fn close(&self) {
        let link = {
            let mut kept = self.kept.lock().await;
            kept.closed = true;
            kept.link.take()
        };
        if let Some((link, _)) = link {
            link.close("");
        }
    }
}

impl Drop for KeptAsks {
    fn drop(&mut self) {
        // A page's session closes only when it is told to: one left open
        // holds its socket for as long as the page lives.
        if let Some((link, _)) = self.kept.get_mut().link.take() {
            link.close("");
        }
    }
}

/// A member's root asking one server its approver commands before the device
/// holds any session there: what a page runs to pair an agent's device
/// through a server it is not yet let into. It reaches the server as [`ServerAddress`]
/// says, from one endpoint bound on the first command; over WebTransport on
/// one link it keeps ([`KeptAsks`]), over iroh each command on a connection
/// of its own.
pub struct MemberApprover {
    dialer: Dialer,
    asks: KeptAsks,
}

impl MemberApprover {
    /// Commands for the server at `server`.
    #[must_use]
    pub fn new(server: ServerAddress) -> Self {
        Self {
            dialer: Dialer::new(server, None),
            asks: KeptAsks::default(),
        }
    }

    /// Let go of the server, waiting for it to hear.
    pub async fn close(&self) {
        self.asks.close().await;
        self.dialer.close().await;
    }
}

impl MemberServer for MemberApprover {
    async fn ask(&self, approval: &MemberApproval) -> Result<ApproverReply, CertifyError> {
        self.asks.ask(&self.dialer, approval).await
    }
}

/// Ask the server at `server` to certify the device behind `signer` as one of
/// `user_public_key`'s devices, labeled `label`, answering the pairing invite
/// `invite` ([0; 16] when none). `on_pending` receives the pairing code to
/// show while the person's other device answers. Resolves to this device's
/// certificate, the server's to pin, and the name the server gave the device (the
/// one asked for, or the next free numbered one).
///
/// # Errors
///
/// The server was not reached or refused, nobody answered in time
/// ([`CertifyError::Expired`]), or what it issued is not a certificate for
/// this device.
pub async fn certify(
    server: &ServerAddress,
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn crate::signer::DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<(DeviceCert, DeviceCert, String), CertifyError> {
    let request = signed_request(user_public_key, label, invite, signer).await?;
    match request_certification_at(server, request, on_pending).await? {
        CertificationOutcome::Issued {
            cert,
            server_cert,
            label,
        } => Ok((cert, server_cert, label)),
        CertificationOutcome::Denied(reason) => Err(CertifyError::Denied(reason)),
        CertificationOutcome::Expired => Err(CertifyError::Expired),
    }
}

/// The request a device without a certificate signs.
///
/// It is dated [`CERTIFICATION_CLOCK_ALLOWANCE_NS`](tilo_user_identity::CERTIFICATION_CLOCK_ALLOWANCE_NS) before this device's
/// clock. A server from before servers allowed the clocks of a pairing to differ
/// checks the date exactly, and refused a request dated now whenever its own
/// clock ran a little behind this device's; such a server stays in use while
/// its pages move on. The wait for an approver is counted from now, as
/// before, so the request lasts that much longer than the wait, well inside
/// the longest a request may. With a server that has the allowance, a device
/// up to twice that far ahead still pairs; the expiry is exact everywhere.
async fn signed_request(
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn crate::signer::DeviceSigner,
) -> Result<CertificationRequest, CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let unsigned = CertificationRequest {
        version: tilo_user_identity::CERTIFICATION_SCHEMA_VERSION,
        user_public_key,
        device_public_key: signer.device_public_key(),
        device_label: label.to_owned(),
        requested: tilo_user_identity::DeviceCapabilities::member(),
        nonce: rand::random(),
        invite,
        issued_at_ns: now.saturating_sub(tilo_user_identity::CERTIFICATION_CLOCK_ALLOWANCE_NS),
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod kept_asks_tests {
    use super::KeptAsks;
    use crate::host::{Carrier, ServerAddress};
    use crate::link::Dialer;
    use iroh_base::{EndpointAddr, SecretKey};
    use std::future::Future as _;
    use std::time::Duration;

    /// A page that stops polling an ask while the ask dials its server holds nothing closing
    /// waits for: the approver closes at once, as a person closing the page expects.
    #[tokio::test]
    async fn closing_waits_for_no_ask_left_paused_on_its_dial() {
        // A server that never answers: its address is a socket nothing reads.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = EndpointAddr::new(SecretKey::from_bytes(&[7; 32]).public())
            .with_ip_addr(silent.local_addr().unwrap());
        let dialer = Dialer::new(ServerAddress::at(addr).over(Carrier::Direct), None);
        let asks = KeptAsks::default();
        let mut dialing = std::pin::pin!(asks.link(&dialer));
        let mut polled = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            dialing.as_mut().poll(&mut polled).is_pending(),
            "the server answers nothing, so the dial waits"
        );
        tokio::time::timeout(Duration::from_secs(5), asks.close())
            .await
            .expect("closing waits for no ask left paused on its dial");
    }
}
