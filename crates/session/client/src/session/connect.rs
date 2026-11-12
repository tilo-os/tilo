//! Dialling the host: the session, and the links beside it.
//!
//! Every link goes through one [`Dialer`] the session keeps for its host, over
//! the host's carrier ([`tilo_relay_client::link`]): iroh on a native device,
//! the browser's WebTransport in a page.
//!
//! Over iroh a device that holds its key dials as its device key. A device
//! whose key stays in custody (a page's, or a test's page-shaped peer) speaks
//! under a key derived from its store's channel vault — a reload dials as the
//! same endpoint — and its device signs, for every plane grant, that the
//! endpoint speaks for it. Over WebTransport there is no endpoint: the device
//! proves itself with the relay handshake on each link that needs it.

use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use tilo_relay_client::Carrier;
use tilo_relay_client::link::{Dialer, Purpose};

use super::{DeviceSigner, LinkError, PeerConfig, RelayClient, Task};

/// The statement a page's device signs to speak through `endpoint` for a day:
/// long enough for a session and its planes to outlive one roster change,
/// short enough that a leaked page key is bounded by the page's device.
#[cfg(not(target_arch = "wasm32"))]
const DELEGATION_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The dialer a session reaches its host through, and this device's
/// statement that its endpoint speaks for it, kept for reconnects.
#[derive(Default)]
pub(super) struct Dial {
    /// Made on the first connect and kept: a reload, and the links beside the
    /// session, dial the same way.
    dialer: Option<Arc<Dialer>>,
    /// The key a custody-held device's endpoint speaks as over iroh, derived
    /// from its store's channel vault: a reload dials as the same endpoint.
    key: [u8; 32],
    /// This device's signed statement that the endpoint speaks for it.
    delegation: Option<Vec<u8>>,
}

impl Dial {
    /// The dialer this session reaches its host through, with this device's
    /// statement that its endpoint speaks for it (empty where none is
    /// needed); `None` before the first connect.
    pub(super) fn plane(&self) -> Option<(Arc<Dialer>, Vec<u8>)> {
        let dialer = self.dialer.clone()?;
        Some((dialer, self.delegation.clone().unwrap_or_default()))
    }

    pub(super) fn for_page(vault: &[u8; 32]) -> Self {
        Self {
            key: blake3::derive_key("tilo page endpoint v1", vault),
            ..Self::default()
        }
    }

    /// Let go of the host, waiting for it to hear: a session that ends for
    /// good leaves no socket behind for the host to fan out to until an idle
    /// timeout.
    pub(super) async fn shutdown(&mut self) {
        if let Some(dialer) = self.dialer.take() {
            dialer.close().await;
        }
    }
}

impl Task {
    /// The session to the host, dialled from the endpoint this session keeps.
    pub(super) async fn connect(&mut self) -> Result<RelayClient, LinkError> {
        let access = self.entry_access().await;
        connect_entering(&self.config, access, self.signer.as_ref(), &mut self.dial).await
    }

    /// Ask the host `approval` from the endpoint this session dials it from,
    /// on a connection of its own beside the session: a page answering its
    /// person's agents, or pairing their devices, binds no endpoint of its
    /// own.
    pub(super) fn ask_member(
        &self,
        approval: Box<tilo_relay_protocol::certify::MemberApproval>,
        done: crate::Answer<
            Result<tilo_relay_protocol::certify::ApproverReply, tilo_relay_client::CertifyError>,
        >,
    ) {
        let Some((dialer, _)) = self.dial.plane() else {
            let _ = done.send(Err(LinkError::Unavailable(
                "the session has not reached its host yet".into(),
            )
            .into()));
            return;
        };
        n0_future::task::spawn(async move {
            let asked = async {
                let link = dialer.link(Purpose::Certify).await?;
                let reply = tilo_relay_client::ask_member(&link, &approval).await;
                link.close("");
                reply
            };
            let _ = done.send(asked.await);
        });
    }
}

/// [`connect_entering`] without an entry's channel request.
#[cfg(test)]
pub(super) async fn connect(
    config: &PeerConfig,
    signer: &dyn DeviceSigner,
    dial: &mut Dial,
) -> Result<RelayClient, LinkError> {
    connect_entering(config, None, signer, dial).await
}

/// The session to `config`'s host, dialled from `dial`'s endpoint — bound there
/// on the first connect and kept for reconnects. A device that holds its key
/// dials as its device key, as a native peer does; a page's device key never
/// leaves custody, so its endpoint speaks under its own key for the device,
/// which signs that it does for every plane grant.
///
/// An entry ([`PeerConfig::entering`]) carries `access`, this device's request
/// for the keys of the workspace's channel group.
async fn connect_entering(
    config: &PeerConfig,
    access: Option<collab_types::MlsAccessRequestV1>,
    signer: &dyn DeviceSigner,
    dial: &mut Dial,
) -> Result<RelayClient, LinkError> {
    let host = &config.host;
    let dialer = dial
        .dialer
        .get_or_insert_with(|| {
            Arc::new(Dialer::new(
                host.clone(),
                Some(signer.endpoint_secret().unwrap_or(dial.key)),
            ))
        })
        .clone();
    // Over iroh a custody-held device's endpoint speaks for it by its signed
    // statement, which every plane grant carries; over WebTransport the
    // device proves itself on each link instead.
    #[cfg(not(target_arch = "wasm32"))]
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
    RelayClient::connect_over_link(
        link,
        Some(config.host_cert.user_public_key),
        Some(config.host_cert.device_public_key),
        tilo_relay_session::ClientCredentials {
            cert: config.cert.clone(),
            signer,
            revocations: config.revocations.clone(),
            entering: config
                .entering
                .clone()
                .map(|entering| tilo_relay_session::Entry {
                    entering,
                    access,
                    host_cert: config.host_cert.clone(),
                }),
        },
    )
    .await
    .map_err(link_error)
}

/// This device's statement that `endpoint` speaks for `device`, for as long
/// as [`DELEGATION_LIFETIME_NS`]: empty for a device whose endpoint is its own
/// key, and the device's own signature otherwise (through custody, which keeps
/// a page's key unextractable).
#[cfg(not(target_arch = "wasm32"))]
async fn sign_delegation(
    signer: &dyn DeviceSigner,
    device: [u8; 32],
    endpoint: [u8; 32],
    slot: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>, LinkError> {
    if signer.endpoint_secret().is_some() {
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

/// How a failed dial reads to the session loop: reachability
/// (`Unavailable`, `Malformed`, retried with backoff) or a host that turned
/// this device away without saying why (`Refused`, which fails the session
/// after `REFUSALS_BEFORE_FAILING` in a row). Every variant is named, so a new
/// client error must be classified here rather than fall into a default.
pub(super) fn link_error(error: tilo_relay_session::ClientError) -> LinkError {
    use tilo_relay_session::ClientError;
    match error {
        ClientError::UnsupportedProtocol => {
            LinkError::Unavailable("host does not speak tilo-relay/1".into())
        }
        ClientError::Transport(reason) => LinkError::Unavailable(reason),
        ClientError::Sign(reason) => LinkError::Sign(reason),
        ClientError::Codec(reason) => LinkError::Malformed(reason),
        ClientError::PeerVersion { theirs } => LinkError::PeerVersion { theirs },
        // A host that turns a device away closes the handshake stream
        // without a word.
        ClientError::NoHandshakeAnswer(reason) => LinkError::Refused(reason),
        // A host that rejects the device either fails its ack (auth) or
        // closes the handshake stream, which surfaces as an I/O error on the
        // next frame read.
        error @ (ClientError::Auth(_) | ClientError::Io(_)) => {
            LinkError::Refused(error.to_string())
        }
    }
}

/// Where member commands reach the host over a session ([`member_host`]): only the session's
/// sender, so it is shared across tasks wherever the session is.
#[derive(Clone)]
pub struct SessionMemberHost(std::sync::Arc<dyn crate::Outbox + Send + Sync>);

/// Ask the host member commands (pairing, agents' questions) over `link`'s session.
#[must_use]
pub fn member_host(link: &crate::Link) -> SessionMemberHost {
    SessionMemberHost(std::sync::Arc::clone(link.outbox()))
}

impl tilo_relay_client::MemberHost for SessionMemberHost {
    fn ask(
        &self,
        approval: &tilo_relay_protocol::certify::MemberApproval,
    ) -> impl std::future::Future<
        Output = Result<
            tilo_relay_protocol::certify::ApproverReply,
            tilo_relay_client::CertifyError,
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
