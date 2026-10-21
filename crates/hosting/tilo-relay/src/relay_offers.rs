//! Asking a device's relays what their workers run for environments.
//!
//! A device's home-peer registrations say what each relay ran when the
//! device last refreshed them. [`refresh_offers`] asks every one of the
//! person's relays at once, within one time budget, and records the
//! answers in the user's [`RelayOffers`]; Terminal's relay picker reads
//! them over the registrations. A relay that does not answer within the
//! budget keeps what it said before. `tilo-relay-offers` runs it, so the
//! `ush` supervisor can ask without waiting for the answer.

use std::path::Path;
use std::time::Duration;

use tilo_relay_protocol::transport::HomePeerManagementRequest;
use tilo_relay_protocol::{HomePeerRegistration, RelayOffers};
use tilo_user_identity::{IdentityPaths, RevocationList, persist};

/// Largest offers file read.
const MAX_OFFERS_BYTES: u64 = 1024 * 1024;

/// The offers recorded at `path`; nothing when absent or unreadable.
#[must_use]
pub fn load_offers(path: &Path) -> RelayOffers {
    std::fs::metadata(path)
        .ok()
        .filter(|meta| meta.len() <= MAX_OFFERS_BYTES)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Ask each of the identity's own relays for its status, all at once, and
/// record what they said at `offers`. Returns the offers as recorded.
///
/// # Errors
///
/// Returns an error when this device's identity or registrations cannot be
/// read, or the offers cannot be written. A relay that fails to answer is
/// not an error.
pub async fn refresh_offers(
    identity_root: &Path,
    offers: &Path,
    budget: Duration,
    now_ms: u64,
) -> anyhow::Result<RelayOffers> {
    let paths = IdentityPaths::under(identity_root);
    let user = persist::read_public_key(&paths.user_pubkey)?;
    let key = persist::load_device_key(&paths)?;
    let cert = persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| persist::read_device_cert(&paths.device_cert))?;
    let registrations: Vec<HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(identity_root.join("home-peers.json"))?)?;
    let user_hex = hex::encode(user);
    let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
    let mut asking = tokio::task::JoinSet::new();
    for registration in registrations
        .into_iter()
        .filter(|registration| registration.user_public_key.eq_ignore_ascii_case(&user_hex))
    {
        let (endpoint, cert, key) = (endpoint.clone(), cert.clone(), key.clone());
        asking.spawn(async move {
            let service = registration.relay_service_node_id.to_ascii_lowercase();
            let answer =
                tokio::time::timeout(budget, ask(&endpoint, &registration, user, cert, key))
                    .await
                    .ok()
                    .and_then(Result::ok);
            (service, answer)
        });
    }
    let mut recorded = load_offers(offers);
    while let Some(asked) = asking.join_next().await {
        let Ok((service, answer)) = asked else {
            continue;
        };
        let offer = recorded.entry(service).or_default();
        offer.checked_at_ms = now_ms;
        if let Some(environments) = answer {
            offer.environments = environments;
        }
    }
    // A relay that never answered leaves a connection attempt behind, and a
    // graceful close waits seconds for it; the answers are already in hand,
    // so the caller is not held for that.
    tokio::spawn(async move { endpoint.close().await });
    if let Some(parent) = offers.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(offers, &serde_json::to_vec_pretty(&recorded)?)?;
    Ok(recorded)
}

async fn ask(
    endpoint: &iroh::Endpoint,
    registration: &HomePeerRegistration,
    user: [u8; 32],
    cert: tilo_user_identity::DeviceCert,
    key: ed25519_dalek::SigningKey,
) -> anyhow::Result<tilo_relay_protocol::RelayEnvironments> {
    let decode = |value: &str| -> anyhow::Result<[u8; 32]> {
        hex::decode(value)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| anyhow::anyhow!("{value:?} is not a 32-byte key"))
    };
    let device = decode(&registration.device_node_id)?;
    let service = iroh_base::PublicKey::from_bytes(&decode(&registration.relay_service_node_id)?)?;
    let address = registration
        .relay_endpoint
        .clone()
        .filter(|address| address.id == service && !address.is_empty())
        .unwrap_or_else(|| iroh_base::EndpointAddr::new(service));
    let status = crate::client::RelayClient::manage_home_peer(
        endpoint,
        address,
        user,
        device,
        cert,
        key,
        RevocationList::new(),
        HomePeerManagementRequest::Status,
    )
    .await?;
    Ok(status.environments)
}
