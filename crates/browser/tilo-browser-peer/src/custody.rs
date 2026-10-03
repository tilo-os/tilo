//! What a browser device keeps, and how it gets certified.
//!
//! The device key itself never passes through here: it lives behind a
//! [`DeviceSigner`] (a non-extractable `WebCrypto` key in the page, a
//! `SigningKey` natively). A [`PeerStore`] keeps only public material — the
//! device's certificate, the pinned host certificate — and the commit outbox
//! that must survive a reload. There is no root key, no self-issued
//! certificate and no enrollment state machine: [`certify`] signs one request
//! and waits for a root holder to answer it through the host.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_relay_client::DeviceSigner;
use tilo_relay_protocol::certify::CertifyReply;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest, DeviceCapabilities,
    DeviceCert, RevocationList, verify_device_cert_at, verify_issued_cert,
};

use crate::carrier::LinkError;
use crate::session::Host;

/// Longest a certification request waits for an approver.
pub const CERTIFICATION_WAIT_NS: u64 = 5 * 60 * 1_000_000_000;

/// One commit the host has not acknowledged yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Commit id, reused when the commit is resent.
    pub op: [u8; 16],
    /// Relay workspace the commit targets.
    pub workspace_id: u64,
    /// Document route the commit belongs to.
    pub route: crate::document::DocumentRoute,
    /// Retention channel ([`neighbour_wire::retention`]) of the body.
    pub channel: [u8; 32],
    /// Whether the body is a retained checkpoint rather than a delta.
    pub checkpoint: bool,
    /// Encoded `NeighbourMsg` body.
    pub body: Vec<u8>,
}

/// The durable part of a browser peer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// This device's root-signed certificate.
    pub cert: Option<DeviceCert>,
    /// The host certificate pinned at certification.
    pub host_cert: Option<DeviceCert>,
    /// Commits not yet acknowledged, in submission order.
    pub outbox: Vec<OutboxEntry>,
}

/// Persistence for a [`PeerRecord`]. The page implements it over `IndexedDB`
/// (strict durability); a save counts only once it is durable.
pub trait PeerStore: tilo_relay_client::MaybeSendSync {
    /// The stored record, if any.
    fn load(&self) -> Option<PeerRecord>;
    /// Replace the stored record.
    ///
    /// # Errors
    ///
    /// Returns a description when the write did not become durable.
    fn save(&self, record: &PeerRecord) -> Result<(), String>;
    /// The stored channel state (encrypted-channel membership and the
    /// edits held for it), if any. A store without it keeps channel state
    /// for the session only.
    fn load_channels(&self) -> Option<Vec<u8>> {
        None
    }
    /// Replace the stored channel state.
    ///
    /// # Errors
    ///
    /// Returns a description when the write did not become durable.
    fn save_channels(&self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// The key channel state is sealed under at rest, kept apart from that
    /// state: in the page, wrapped by a non-extractable `WebCrypto` key. A
    /// store without one seals channel state under a key of the session's
    /// own, so stored state does not open in the next session.
    fn channel_vault(&self) -> Option<[u8; 32]> {
        None
    }
}

/// An in-memory [`PeerStore`] for native peers and tests.
pub struct MemoryPeerStore(Mutex<Option<PeerRecord>>, Mutex<Option<Vec<u8>>>, [u8; 32]);

impl Default for MemoryPeerStore {
    fn default() -> Self {
        Self(Mutex::default(), Mutex::default(), rand::random())
    }
}

impl PeerStore for MemoryPeerStore {
    fn load(&self) -> Option<PeerRecord> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record.clone());
        Ok(())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        self.1
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        *self
            .1
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bytes.to_vec());
        Ok(())
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.2)
    }
}

/// Why certification did not produce a certificate.
#[derive(Debug, thiserror::Error)]
pub enum CertifyError {
    /// The request could not be built or signed.
    #[error("certification request: {0}")]
    Request(String),
    /// The host could not be reached or answered badly.
    #[error("certification transport: {0}")]
    Transport(#[from] LinkError),
    /// An approver declined, or the host refused.
    #[error("certification denied: {0}")]
    Denied(String),
    /// No approver answered in time.
    #[error("no approver answered the certification request")]
    Expired,
}

/// Ask the user's host to have the root holder certify the device behind
/// `signer`. `on_pending` receives the pairing code to display while
/// waiting. Returns the device certificate and the host certificate to pin,
/// both checked here: the host cannot hand back a certificate for another
/// key, another user or wider capabilities.
///
/// # Errors
///
/// Returns [`CertifyError`] when signing, the transport, or the answer fails.
pub async fn certify(
    host: &Host,
    user_public_key: [u8; 32],
    label: &str,
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<(DeviceCert, DeviceCert), CertifyError> {
    let request = signed_request(user_public_key, label, signer).await?;
    let reply = match host {
        Host::Https(client) => {
            let digest = request
                .digest()
                .map_err(|error| CertifyError::Request(error.to_string()))?;
            let mut reply = crate::carrier::certify_request(client.as_ref(), &request).await?;
            let mut on_pending = Some(on_pending);
            while let CertifyReply::Pending { pairing_code } = &reply {
                if let Some(show) = on_pending.take() {
                    show(pairing_code);
                }
                reply = crate::carrier::certify_wait(client.as_ref(), digest).await?;
            }
            reply
        }
        #[cfg(feature = "iroh")]
        Host::Iroh { addr, relay_url } => {
            let mut builder = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal);
            let mut addr = addr.clone();
            if let Some(url) = relay_url {
                builder = builder.relay_mode(iroh::RelayMode::Custom(url.clone().into()));
                addr = addr.with_relay_url(url.clone());
            }
            let endpoint = builder
                .bind()
                .await
                .map_err(|error| LinkError::Unavailable(error.to_string()))?;
            let outcome = tilo_relay_client::request_certification(
                &endpoint,
                addr,
                request.clone(),
                on_pending,
            )
            .await
            .map_err(|error| LinkError::Refused(error.to_string()))?;
            endpoint.close().await;
            match outcome {
                tilo_relay_client::CertificationOutcome::Issued { cert, host_cert } => {
                    CertifyReply::Issued { cert, host_cert }
                }
                tilo_relay_client::CertificationOutcome::Denied(reason) => {
                    CertifyReply::Denied { reason }
                }
                tilo_relay_client::CertificationOutcome::Expired => CertifyReply::Expired,
            }
        }
    };
    match reply {
        CertifyReply::Issued { cert, host_cert } => {
            check_issued(&request, &cert, &host_cert)?;
            Ok((cert, host_cert))
        }
        CertifyReply::Denied { reason } => Err(CertifyError::Denied(reason)),
        CertifyReply::Expired | CertifyReply::Pending { .. } => Err(CertifyError::Expired),
    }
}

async fn signed_request(
    user_public_key: [u8; 32],
    label: &str,
    signer: &dyn DeviceSigner,
) -> Result<CertificationRequest, CertifyError> {
    let now = crate::clock::now_ns();
    let unsigned = CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key,
        device_public_key: signer.device_public_key(),
        device_label: label.to_owned(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        issued_at_ns: now,
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

/// How far a certificate's issue time may run ahead of this page's clock:
/// the host stamps its own nanosecond clock, and a page's clock is coarser
/// (a browser's is whole milliseconds), so a certificate checked the moment
/// it arrives can look issued in the future.
const ISSUE_SKEW_NS: u64 = 5 * 1_000_000_000;

/// When to check a certificate issued at `issued_at_ns`, reading `now_ns`.
fn checked_at(now_ns: u64, issued_at_ns: u64) -> u64 {
    if issued_at_ns > now_ns && issued_at_ns.saturating_sub(now_ns) <= ISSUE_SKEW_NS {
        issued_at_ns
    } else {
        now_ns
    }
}

fn check_issued(
    request: &CertificationRequest,
    cert: &DeviceCert,
    host_cert: &DeviceCert,
) -> Result<(), CertifyError> {
    let now = crate::clock::now_ns();
    let refused = |reason: String| CertifyError::Denied(reason);
    verify_issued_cert(
        request,
        cert,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        checked_at(now, cert.issued_at_ns),
    )
    .map_err(|error| refused(format!("issued certificate refused: {error}")))?;
    verify_device_cert_at(host_cert, now)
        .map_err(|error| refused(format!("host certificate refused: {error}")))?;
    if host_cert.user_public_key != request.user_public_key
        || !host_cert.capabilities.can_act_as_host
    {
        return Err(refused(
            "host certificate is not a host of this user".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_issued_just_ahead_of_the_page_clock_is_checked_at_its_issue() {
        let now = 1_000_000_000_000;
        assert_eq!(checked_at(now, now + 400_000), now + 400_000);
        assert_eq!(checked_at(now, now - 1), now);
        assert_eq!(checked_at(now, now + ISSUE_SKEW_NS + 1), now);
    }
}
