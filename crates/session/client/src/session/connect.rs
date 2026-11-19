//! Dialling the host: the session, and the links beside it.
//!
//! Every link goes through one [`Dialer`] the session keeps for its host, over
//! the host's carrier ([`tilo_server_client::link`]): iroh on a native device,
//! the browser's WebTransport in a page.
//!
//! Over iroh a device that holds its key dials as its device key. A device
//! whose key stays held (a page's, or a test's page-shaped peer) speaks
//! under a key derived from its store's channel vault — a reload dials as the
//! same endpoint — and its device signs, for every live session grant, that the
//! endpoint speaks for it. Over WebTransport there is no endpoint: the device
//! proves itself with the server handshake on each link that needs it.

use std::sync::Arc;
use std::time::Duration;

use tilo_server_client::Carrier;
use tilo_server_client::link::{Dialer, Purpose};

use super::{DeviceSigner, LinkError, PeerConfig, ServerClient, Task};

/// The statement a page's device signs to speak through `endpoint` for a day:
/// long enough for a session and its live sessions to outlive one roster change,
/// short enough that a leaked page key is bounded by the page's device.
const DELEGATION_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Attempts one connect makes before its host's answer is surfaced.
///
/// A burst of devices arriving at once (or a host still finishing its
/// handshake for the device before) can close or time out a handshake that
/// works a moment later. The dial and the handshake are idempotent — a
/// repeated entry is answered again — so it is tried a few times before the
/// session's own backoff takes over.
const CONNECT_ATTEMPTS: u32 = 3;

/// The first wait between those attempts; each doubles up to
/// [`CONNECT_BACKOFF_MAX`], and each is jittered so a burst does not retry
/// in lockstep.
const CONNECT_BACKOFF: Duration = Duration::from_millis(500);
const CONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

/// The dialer a session reaches its host through, and this device's
/// statement that its endpoint speaks for it, kept for reconnects.
#[derive(Default)]
pub(super) struct Dial {
    /// Made on the first connect and kept: a reload, and the links beside the
    /// session, dial the same way.
    dialer: Option<Arc<Dialer>>,
    /// The key an ownership-held device's endpoint speaks as over iroh, derived
    /// from its store's channel vault: a reload dials as the same endpoint.
    key: [u8; 32],
    /// This device's signed statement that the endpoint speaks for it.
    delegation: Option<Vec<u8>>,
    /// What this device asks its server's certification beside the session
    /// (its person's agents' questions, pairing): over WebTransport on one
    /// link kept between asks, since a browser's session costs a socket each.
    asks: Arc<tilo_server_client::KeptAsks>,
    /// The endpoint this device already runs under its key, when it lends
    /// it: the session dials from there instead of binding one of its own.
    /// It is lent as where it lands once bound, since a session may be opened
    /// before it is: a dial made before then binds nothing in its place and
    /// is made again.
    #[cfg(not(target_arch = "wasm32"))]
    lent: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
}

impl Dial {
    /// The dialer this session reaches its host through, with this device's
    /// statement that its endpoint speaks for it (empty where none is
    /// needed); `None` before the first connect.
    /// The dialer a live session of this session's own opens, and the statement that its endpoint
    /// speaks for the device; `None` when this session runs no live session.
    ///
    /// A session lent the endpoint its device's own session runs joins no live session of its own: the
    /// device has one session in each workspace's live session, and a second one under the same key
    /// shadows the broadcast the device publishes its presence on, so the people around it hear
    /// the cached group and then silence. The device's own session opens the live session instead.
    pub(super) fn live_session(&self) -> Option<(Arc<Dialer>, Vec<u8>)> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.lent.is_some() {
            return None;
        }
        let dialer = self.dialer.clone()?;
        Some((dialer, self.delegation.clone().unwrap_or_default()))
    }

    pub(super) fn for_page(vault: &[u8; 32]) -> Self {
        Self {
            key: blake3::derive_key("tilo page endpoint v1", vault),
            ..Self::default()
        }
    }

    /// Dial from `endpoint`, the one this device already runs under its key,
    /// once it is bound.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn lend(&mut self, endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>) {
        self.lent = Some(endpoint);
    }

    /// Let go of the host, waiting for it to hear: a session that ends for
    /// good leaves no socket behind for the host to fan out to until an idle
    /// timeout.
    pub(super) async fn shutdown(&mut self) {
        self.asks.close().await;
        if let Some(dialer) = self.dialer.take() {
            dialer.close().await;
        }
    }
}

impl Task {
    /// The session to the host, dialled from the endpoint this session keeps.
    ///
    /// A dial or handshake that fails without a final answer is tried a few
    /// times with jittered, doubling waits before its error reaches the
    /// session loop, which then backs off and retries on its own.
    pub(super) async fn connect(&mut self) -> Result<ServerClient, LinkError> {
        let access = self.entry_access().await;
        let mut attempt: u32 = 0;
        loop {
            let error = match connect_entering(
                &self.config,
                access.clone(),
                self.signer.as_ref(),
                &mut self.dial,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => error,
            };
            attempt = attempt.saturating_add(1);
            if attempt >= CONNECT_ATTEMPTS || !transient(&error) {
                return Err(error);
            }
            n0_future::time::sleep(connect_backoff(attempt)).await;
        }
    }

    /// Ask the host `approval` from the endpoint this session dials it from,
    /// beside the session: a page answering its person's agents, or pairing
    /// their devices, binds no endpoint of its own, and asks on the one link
    /// it keeps for that ([`tilo_server_client::KeptAsks`]).
    pub(super) fn ask_member(
        &self,
        approval: Box<tilo_server_protocol::certify::MemberApproval>,
        done: crate::Answer<
            Result<tilo_server_protocol::certify::ApproverReply, tilo_server_client::CertifyError>,
        >,
    ) {
        let Some((dialer, _)) = self.dial.live_session() else {
            let _ = done.send(Err(LinkError::Unavailable(
                "the session has not reached its host yet".into(),
            )
            .into()));
            return;
        };
        let asks = Arc::clone(&self.dial.asks);
        n0_future::task::spawn(async move {
            let _ = done.send(asks.ask(&dialer, &approval).await);
        });
    }
}

/// [`connect_entering`] without an entry's channel request.
#[cfg(test)]
pub(super) async fn connect(
    config: &PeerConfig,
    signer: &dyn DeviceSigner,
    dial: &mut Dial,
) -> Result<ServerClient, LinkError> {
    connect_entering(config, None, signer, dial).await
}

/// The session to `config`'s host, dialled from `dial`'s endpoint — bound there
/// on the first connect and kept for reconnects. A device that holds its key
/// dials as its device key, as a native peer does; a page's device key never
/// leaves ownership, so its endpoint speaks under its own key for the device,
/// which signs that it does for every live session grant.
///
/// An entry ([`PeerConfig::entering`]) carries `access`, this device's request
/// for the keys of the workspace's channel group.
async fn connect_entering(
    config: &PeerConfig,
    access: Option<collab_types::MlsAccessRequestV1>,
    signer: &dyn DeviceSigner,
    dial: &mut Dial,
) -> Result<ServerClient, LinkError> {
    let host = &config.host;
    // A lent endpoint that is not bound yet is waited for: the dial fails as
    // one to make again, and no endpoint is bound under the device's key in
    // its place, which would take the device's place at its server.
    #[cfg(not(target_arch = "wasm32"))]
    if dial.dialer.is_none()
        && let Some(lent) = &dial.lent
    {
        let endpoint = lent.get().cloned().ok_or_else(|| {
            LinkError::Unavailable("this device's endpoint is not bound yet".into())
        })?;
        dial.dialer = Some(Arc::new(Dialer::from_endpoint(host.clone(), endpoint)));
    }
    let dialer = dial
        .dialer
        .get_or_insert_with(|| {
            Arc::new(Dialer::new(
                host.clone(),
                Some(signer.endpoint_secret().unwrap_or(dial.key)),
            ))
        })
        .clone();
    // Over iroh an ownership-held device's endpoint speaks for it by its signed
    // statement, which every live session grant carries; over WebTransport the
    // device proves itself on each link instead. A page reaches a server behind a
    // NAT over iroh through its packet relay, and signs the same statement.
    if host.carrier() != Carrier::WebTransport {
        let endpoint = dialer.endpoint().await?;
        sign_delegation(
            signer,
            config.cert.device_public_key,
            *endpoint.id().as_bytes(),
            &mut dial.delegation,
        )
        .await?;
    }
    let link = dialer.link(Purpose::Session).await?;
    ServerClient::connect_over_link(
        link,
        Some(config.server_cert.user_public_key),
        Some(config.server_cert.device_public_key),
        tilo_server_session::ClientCredentials {
            cert: config.cert.clone(),
            signer,
            revocations: config.revocations.clone(),
            entering: config
                .entering
                .clone()
                .map(|entering| tilo_server_session::Entry {
                    entering,
                    access,
                    server_cert: config.server_cert.clone(),
                }),
        },
    )
    .await
    .map_err(link_error)
}

/// This device's statement that `endpoint` speaks for `device`, for as long
/// as [`DELEGATION_LIFETIME_NS`]: empty for a device whose endpoint is its own
/// key, and the device's own signature otherwise (through ownership, which keeps
/// a page's key unextractable).
async fn sign_delegation(
    signer: &dyn DeviceSigner,
    device: [u8; 32],
    endpoint: [u8; 32],
    slot: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>, LinkError> {
    if endpoint == device {
        return Ok(Vec::new());
    }
    if let Some(delegation) = slot {
        return Ok(delegation.clone());
    }
    let now = tilo_clock::unix_now_ns();
    let unsigned = tilo_user_identity::EndpointDelegation::unsigned(
        device,
        endpoint,
        now,
        now.saturating_add(DELEGATION_LIFETIME_NS),
    );
    let digest = unsigned
        .signing_digest()
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    let encoded = serde_json::to_vec(&unsigned.with_signature(signature))
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    *slot = Some(encoded.clone());
    Ok(encoded)
}

/// Whether the same dial can work a moment later: the host was not reached,
/// or it ended a handshake without giving a reason to act on. A refusal it
/// explained, a host that changed, this device's own signing and a version
/// mismatch stay as they are.
pub(super) fn transient(error: &LinkError) -> bool {
    matches!(error, LinkError::Unavailable(_) | LinkError::Refused(_))
}

/// The wait before attempt `attempt` (1 for the first retry): doubling from
/// [`CONNECT_BACKOFF`] up to [`CONNECT_BACKOFF_MAX`], jittered by up to a
/// quarter either way.
pub(super) fn connect_backoff(attempt: u32) -> std::time::Duration {
    let base = CONNECT_BACKOFF_MAX
        .min(CONNECT_BACKOFF.saturating_mul(1u32 << attempt.saturating_sub(1).min(5)));
    CONNECT_BACKOFF_MAX.min(base.mul_f64(0.75 + 0.5 * rand::random::<f64>()))
}

/// How a failed dial reads to the session loop: reachability
/// (`Unavailable`, `Malformed`, retried with backoff) or a host that turned
/// this device away without saying why (`Refused`, which fails the session
/// after `REFUSALS_BEFORE_FAILING` in a row), or one that said why
/// (`DeviceRevoked`, which stops the session at once). Every variant is named,
/// so a new client error must be classified here rather than fall into a
/// default.
pub(super) fn link_error(error: tilo_server_session::ClientError) -> LinkError {
    use tilo_server_session::ClientError;
    match error {
        ClientError::UnsupportedProtocol => {
            LinkError::Unavailable("host does not speak tilo-server/1".into())
        }
        ClientError::Transport(reason) => LinkError::Unavailable(reason),
        ClientError::Sign(reason) => LinkError::Sign(reason),
        ClientError::Codec(reason) => LinkError::Malformed(reason),
        ClientError::PeerVersion { theirs } => LinkError::PeerVersion { theirs },
        // The server said why, and proved the refusal its own.
        ClientError::Refused(tilo_server_protocol::auth::HelloRefusal::DeviceRevoked) => {
            LinkError::DeviceRevoked
        }
        // A server that refuses a device and does not say why closes the
        // handshake stream with nothing said.
        ClientError::NoHandshakeAnswer(reason) => LinkError::Refused(reason),
        // A host that rejects the device either fails its ack (auth) or
        // closes the handshake stream, which surfaces as an I/O error on the
        // next frame read.
        error @ (ClientError::Auth(_) | ClientError::Io(_)) => {
            LinkError::Refused(error.to_string())
        }
    }
}

/// Where member commands reach the host over a session ([`member_server`]): only the session's
/// sender, so it is shared across tasks wherever the session is.
#[derive(Clone)]
pub struct SessionMemberServer(std::sync::Arc<dyn crate::Outbox + Send + Sync>);

/// Ask the host member commands (pairing, agents' questions) over `link`'s session.
#[must_use]
pub fn member_server(link: &crate::Link) -> SessionMemberServer {
    SessionMemberServer(std::sync::Arc::clone(link.outbox()))
}

impl tilo_server_client::MemberServer for SessionMemberServer {
    fn ask(
        &self,
        approval: &tilo_server_protocol::certify::MemberApproval,
    ) -> impl std::future::Future<
        Output = Result<
            tilo_server_protocol::certify::ApproverReply,
            tilo_server_client::CertifyError,
        >,
    > + Send {
        let (done, answer) = crate::answer();
        let said = self
            .0
            .send(crate::Cmd::AskMember(Box::new(approval.clone()), done));
        async move {
            said.map_err(|_| LinkError::Unavailable("peer session is closed".into()))?;
            answer
                .await
                .map_err(|_| LinkError::Unavailable("the session ended".into()))?
        }
    }
}
