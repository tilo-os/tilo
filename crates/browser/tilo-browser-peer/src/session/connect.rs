//! Dialling the host: the session's carrier, and the endpoint it speaks from.
//!
//! A device that holds its key dials as its device key, as a native peer does.
//! A page keeps its device key non-extractable, so its endpoint speaks under a
//! key derived from its store's channel vault — a reload dials as the same
//! endpoint — and its device signs, for every plane grant, that the endpoint
//! speaks for it.

use super::{DeviceSigner, Host, HttpsLink, Link, LinkError, PeerConfig, Task};

/// The statement a page's device signs to speak through `endpoint` for a day:
/// long enough for a session and its planes to outlive one roster change,
/// short enough that a leaked page key is bounded by the page's device.
#[cfg(feature = "iroh")]
const DELEGATION_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Where a session dials its host from, and this device's statement that the
/// endpoint speaks for it, kept for reconnects.
#[derive(Default)]
pub(super) struct Dial {
    /// The endpoint this session dials from, bound on the first connect. A
    /// device's is its device key; a page's speaks under [`Self::key`].
    #[cfg(feature = "iroh")]
    endpoint: Option<iroh::Endpoint>,
    /// The key a page's endpoint speaks as, derived from its store's channel
    /// vault: a reload dials as the same endpoint.
    #[cfg(feature = "iroh")]
    key: [u8; 32],
    /// This device's signed statement that the endpoint speaks for it.
    #[cfg(feature = "iroh")]
    delegation: Option<Vec<u8>>,
}

impl Dial {
    /// How a page dials: under a key derived from its store's channel vault, so
    /// a reload — and the planes beside the session — speak as the same
    /// endpoint.
    pub(super) fn for_page(vault: &[u8; 32]) -> Self {
        Self {
            #[cfg(feature = "iroh")]
            key: blake3::derive_key("tilo page endpoint v1", vault),
            ..Self::default()
        }
    }
}

impl Task {
    /// The session to the host over whichever carrier reaches it, dialled
    /// from the endpoint this session keeps.
    pub(super) async fn connect(&mut self) -> Result<Link, LinkError> {
        connect(&self.config, self.signer.as_ref(), &mut self.dial).await
    }
}

/// The session to `config`'s host, dialled from `dial`'s endpoint — bound there
/// on the first connect and kept for reconnects. A device that holds its key
/// dials as its device key, as a native peer does; a page's device key never
/// leaves custody, so its endpoint speaks under its own key for the device,
/// which signs that it does for every plane grant.
#[cfg_attr(
    not(feature = "iroh"),
    allow(unused_variables, reason = "only an iroh link keeps a dial")
)]
pub(super) async fn connect(
    config: &PeerConfig,
    signer: &dyn DeviceSigner,
    dial: &mut Dial,
) -> Result<Link, LinkError> {
    match config.host.clone() {
        Host::Https(client) => HttpsLink::connect_as(
            client,
            &config.cert,
            config.host_cert.device_public_key,
            &config.revocations,
            signer,
            config.background,
        )
        .await
        .map(Link::Https),
        #[cfg(feature = "iroh")]
        Host::Iroh { addr, relay_url } => {
            let relay = relay_url.or_else(|| addr.relay_urls().next().cloned());
            let endpoint = bind_endpoint(
                &mut dial.endpoint,
                signer.device_signing_key().unwrap_or(dial.key),
                relay.clone(),
            )
            .await?;
            let delegation = sign_delegation(
                signer,
                config.cert.device_public_key,
                &endpoint,
                &mut dial.delegation,
            )
            .await?;
            let host = match relay {
                Some(url) => addr.with_relay_url(url),
                None => addr,
            };
            tilo_relay_session::RelayClient::connect_with_signer(
                endpoint.clone(),
                host,
                Some(config.cert.user_public_key),
                Some(config.host_cert.device_public_key),
                config.cert.clone(),
                signer,
                config.revocations.clone(),
            )
            .await
            .map(|client| Link::Iroh {
                client,
                endpoint,
                delegation,
            })
            .map_err(iroh_link_error)
        }
    }
}

/// The endpoint `slot` holds, bound from `key` on the first call and reused
/// after: a page's key is derived from its store's channel vault, so a reload —
/// and the planes beside the session — dial as the same endpoint.
#[cfg(feature = "iroh")]
async fn bind_endpoint(
    slot: &mut Option<iroh::Endpoint>,
    key: [u8; 32],
    relay: Option<iroh_base::RelayUrl>,
) -> Result<iroh::Endpoint, LinkError> {
    if let Some(endpoint) = slot {
        return Ok(endpoint.clone());
    }
    let mut builder = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key));
    if let Some(url) = relay {
        builder = builder.relay_mode(iroh::RelayMode::Custom(url.into()));
    }
    let endpoint = builder
        .bind()
        .await
        .map_err(|error| LinkError::Unavailable(error.to_string()))?;
    *slot = Some(endpoint.clone());
    Ok(endpoint)
}

/// This device's statement that `endpoint` speaks for `device`, for as long as
/// [`DELEGATION_LIFETIME_NS`]: empty for a device whose endpoint is its own
/// key, and the device's own signature otherwise (through custody, which keeps
/// a page's key unextractable).
#[cfg(feature = "iroh")]
async fn sign_delegation(
    signer: &dyn DeviceSigner,
    device: [u8; 32],
    endpoint: &iroh::Endpoint,
    slot: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>, LinkError> {
    if signer.device_signing_key().is_some() {
        return Ok(Vec::new());
    }
    if let Some(delegation) = slot {
        return Ok(delegation.clone());
    }
    let now = tilo_clock::unix_now_ns();
    let unsigned = tilo_user_identity::EndpointDelegation::unsigned(
        device,
        *endpoint.id().as_bytes(),
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

/// How a failed iroh dial reads to the session loop: reachability
/// (`Unavailable`, `Malformed`, retried with backoff) or a host that turned
/// this device away without saying why (`Refused`, which fails the session
/// after `REFUSALS_BEFORE_FAILING` in a row). Every variant is named, so a new
/// client error must be classified here rather than fall into a default.
#[cfg(feature = "iroh")]
pub(super) fn iroh_link_error(error: tilo_relay_session::ClientError) -> LinkError {
    use tilo_relay_session::ClientError;
    match error {
        ClientError::UnsupportedProtocol => {
            LinkError::Unavailable("host does not speak tilo-relay/1".into())
        }
        ClientError::Iroh(reason) => LinkError::Unavailable(reason),
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
