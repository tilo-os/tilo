//! Dialling the host: the one [`Dialer`] a session keeps for its host, over the
//! host's carrier ([`tilo_server_client::link`]): iroh on a native device, the
//! browser's WebTransport in a page.
//!
//! Every conversation a session keeps with its host is a link of its own: the
//! workspace's records on [`Purpose::Records`], the actions one device asks
//! another on [`Purpose::Actions`], and the live session on
//! [`Purpose::LiveSession`]. Over iroh a device that holds its key dials as its
//! device key. A device whose key stays held (a page's) speaks under a key
//! derived from its store's channel vault — a reload dials as the same endpoint
//! — and its device signs, for every live session grant, that the endpoint
//! speaks for it. Over WebTransport there is no endpoint: the device proves
//! itself with the server handshake on each link that needs it.

use std::sync::Arc;

use tilo_server_client::Carrier;
use tilo_server_client::link::Dialer;

use super::{LinkError, PeerConfig};
use tilo_server_client::DeviceSigner;

/// The statement a page's device signs to speak through `endpoint` for a day:
/// long enough for a session and its live sessions to outlive one roster change,
/// short enough that a leaked page key is bounded by the page's device.
const DELEGATION_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The dialer a session reaches its host through, and this device's
/// statement that its endpoint speaks for it, kept across reconnects.
#[derive(Default)]
pub(super) struct Dial {
    /// Made on the first dial and kept: a reload, and the links beside the
    /// session, dial the same way.
    dialer: Option<Arc<Dialer>>,
    /// The key an ownership-held device's endpoint speaks as over iroh, derived
    /// from its store's channel vault: a reload dials as the same endpoint.
    key: [u8; 32],
    /// This device's signed statement that the endpoint speaks for it.
    delegation: Option<Vec<u8>>,
    /// The endpoint this device already runs under its key, when it lends
    /// it: the session dials from there instead of binding one of its own.
    /// It is lent as where it lands once bound, since a session may be opened
    /// before it is: a dial made before then binds nothing in its place and
    /// is made again.
    #[cfg(not(target_arch = "wasm32"))]
    lent: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// The lent endpoint is this session's own: it joins the live sessions from it.
    #[cfg(not(target_arch = "wasm32"))]
    lent_own: bool,
    /// The person's relays, held back while the server answers ([`Self::server_reachable`]).
    #[cfg(not(target_arch = "wasm32"))]
    fallback: tilo_endpoint_reachability::Reachability,
}

impl Dial {
    /// The dialer a live session of this session's own opens, and the statement that its endpoint
    /// speaks for the device; `None` when this session runs no live session.
    ///
    /// A session lent the endpoint its device's own session runs joins no live session of its own: the
    /// device has one session in each workspace's live session, and a second one under the same key
    /// shadows the broadcast the device publishes its presence on, so the people around it hear
    /// the cached group and then silence. The device's own session opens the live session instead.
    pub(super) fn live_session(&self) -> Option<(Arc<Dialer>, Vec<u8>)> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.lent.is_some() && !self.lent_own {
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
    pub(super) fn lend(&mut self, endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>, own: bool) {
        self.lent = Some(endpoint);
        self.lent_own = own;
    }

    /// The dialer this session reaches its host through, made on first use from the endpoint
    /// this device lends or one bound under its key; `None` once the session let go of its host.
    ///
    /// A lent endpoint that is not bound yet is waited for: the dial fails as one to make again,
    /// and no endpoint is bound under the device's key in its place, which would take the device's
    /// place at its server.
    pub(super) async fn dialer(
        &mut self,
        config: &PeerConfig,
        signer: &dyn DeviceSigner,
    ) -> Result<Arc<Dialer>, LinkError> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.dialer.is_none()
            && let Some(lent) = &self.lent
        {
            let endpoint = lent.get().cloned().ok_or_else(|| {
                LinkError::Unavailable("this device's endpoint is not bound yet".into())
            })?;
            self.dialer = Some(Arc::new(Dialer::from_endpoint(
                config.host.clone(),
                endpoint,
            )));
        }
        let dialer = self
            .dialer
            .get_or_insert_with(|| {
                Arc::new(Dialer::new(
                    config.host.clone(),
                    Some(signer.endpoint_secret().unwrap_or(self.key)),
                ))
            })
            .clone();
        // Over iroh an ownership-held device's endpoint speaks for it by its signed
        // statement, which every live session grant carries; over WebTransport the
        // device proves itself on each link instead. A page reaches a server behind a
        // NAT over iroh through its packet relay, and signs the same statement.
        if config.host.carrier() != Carrier::WebTransport {
            let endpoint = dialer.endpoint().await?;
            // The person's relays other than the server's, woken only while it is unreachable.
            #[cfg(not(target_arch = "wasm32"))]
            if !self.fallback.holds_relays() && (self.lent.is_none() || self.lent_own) {
                let others: iroh::RelayMap = (tilo_endpoint_reachability::relays().relays)
                    .urls::<Vec<_>>()
                    .into_iter()
                    .filter(|url| Some(url) != config.host.relay())
                    .collect();
                let rest_at_once = Some(std::time::Duration::ZERO);
                self.fallback = tilo_endpoint_reachability::Reachability::defer(
                    &endpoint,
                    &others,
                    tokio::runtime::Handle::current(),
                    rest_at_once,
                );
            }
            sign_delegation(
                signer,
                config.cert.device_public_key,
                *endpoint.id().as_bytes(),
                &mut self.delegation,
            )
            .await?;
        }
        Ok(dialer)
    }

    /// Wake the person's relays while the server is unreachable; whether they run now.
    pub(super) fn server_reachable(&self, reachable: bool) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let why = (!reachable).then_some("its server is unreachable");
            self.fallback
                .need(tilo_endpoint_reachability::Need::Transport, why);
            self.fallback.holds_relays() && !reachable
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (self, reachable);
            false
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
