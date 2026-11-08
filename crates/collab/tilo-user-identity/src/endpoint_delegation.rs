//! [`EndpointDelegation`] — a device key vouching for the key its transport
//! speaks as.
//!
//! A device's transport is normally its device key: an iroh endpoint dials
//! under it, so the QUIC handshake and the certificate name the same key. A
//! page cannot do that — it keeps its device key non-extractable in
//! `WebCrypto` and so has no bytes to bind an endpoint from — so it generates
//! a transport key of its own, kept with its other device secrets, and has the
//! device key sign that this transport key speaks for the device until a time
//! it names.
//!
//! This is the same shape as the person root's certificate for a device: a
//! key that is kept says another key may act as it, for a bounded time. A
//! host admits a connection when the endpoint it terminated names the device's
//! own key, or one a valid delegation from that device names.
//!
//! Signing preimage: `blake3(ENDPOINT_DELEGATION_PREIMAGE_TAG ||
//! postcard(view))`, the same pipeline as every other signed type here.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for [`EndpointDelegation`]. Frozen wire ABI.
pub const ENDPOINT_DELEGATION_PREIMAGE_TAG: &[u8] = b"tilo-endpoint-delegation-v1\0";

/// Longest a delegation may be made to last, so a leaked page key is bounded
/// by the device that signed it rather than by the page that kept it.
pub const MAX_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// A device key's statement that `endpoint_public_key` speaks for it until
/// `expires_at_ns`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointDelegation {
    /// The device whose key signed this.
    pub device_public_key: [u8; 32],
    /// The transport key it speaks for.
    pub endpoint_public_key: [u8; 32],
    /// Nanoseconds since the Unix epoch, when the device signed it.
    pub issued_at_ns: u64,
    /// Nanoseconds since the Unix epoch, when the transport key stops
    /// speaking for the device.
    pub expires_at_ns: u64,
    /// Ed25519 signature by the device key over
    /// [`signing_digest`]`(ENDPOINT_DELEGATION_PREIMAGE_TAG, view)`.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Signing view — everything except the signature.
#[derive(Serialize)]
struct EndpointDelegationView<'a> {
    device_public_key: &'a [u8; 32],
    endpoint_public_key: &'a [u8; 32],
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl EndpointDelegation {
    /// The delegation a device is about to sign: `endpoint` speaks for
    /// `device` from `issued_at_ns` until `expires_at_ns`.
    #[must_use]
    pub fn unsigned(
        device_public_key: [u8; 32],
        endpoint_public_key: [u8; 32],
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Self {
        Self {
            device_public_key,
            endpoint_public_key,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        }
    }

    fn view(&self) -> EndpointDelegationView<'_> {
        EndpointDelegationView {
            device_public_key: &self.device_public_key,
            endpoint_public_key: &self.endpoint_public_key,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// The digest the device key signs. A signer that keeps the key
    /// unextractable (a page's) signs this and attaches it with
    /// [`Self::with_signature`].
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the delegation cannot be canonically
    /// encoded (effectively unreachable).
    pub fn signing_digest(&self) -> Result<[u8; 32], CanonicalError> {
        signing_digest(ENDPOINT_DELEGATION_PREIMAGE_TAG, &self.view())
    }

    /// This delegation with the device's `signature` attached.
    #[must_use]
    pub fn with_signature(mut self, signature: [u8; 64]) -> Self {
        self.signature = signature;
        self
    }

    /// Whether this delegation is the device's own signature for `endpoint`,
    /// live at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`EndpointDelegationError::WrongEndpoint`] when it speaks for another
    /// key, [`EndpointDelegationError::InvalidValidity`] for an empty or
    /// over-long window, [`EndpointDelegationError::NotYetValid`],
    /// [`EndpointDelegationError::Expired`], or
    /// [`EndpointDelegationError::Signature`].
    pub fn verify(&self, endpoint: &[u8; 32], now_ns: u64) -> Result<(), EndpointDelegationError> {
        if &self.endpoint_public_key != endpoint {
            return Err(EndpointDelegationError::WrongEndpoint);
        }
        let lifetime = self.expires_at_ns.saturating_sub(self.issued_at_ns);
        if lifetime == 0 || lifetime > MAX_LIFETIME_NS {
            return Err(EndpointDelegationError::InvalidValidity);
        }
        if now_ns < self.issued_at_ns {
            return Err(EndpointDelegationError::NotYetValid);
        }
        if now_ns >= self.expires_at_ns {
            return Err(EndpointDelegationError::Expired);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.device_public_key)
            .map_err(|_| EndpointDelegationError::Signature)?;
        let digest = self
            .signing_digest()
            .map_err(|_| EndpointDelegationError::Signature)?;
        key.verify(
            &digest,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| EndpointDelegationError::Signature)
    }
}

/// Sign a delegation from `device` to `endpoint_public_key`, for
/// `lifetime_ns` from `issued_at_ns`.
///
/// # Errors
///
/// [`EndpointDelegationError::InvalidValidity`] for a lifetime that is empty
/// or longer than [`MAX_LIFETIME_NS`].
pub fn sign_endpoint_delegation(
    device: &ed25519_dalek::SigningKey,
    endpoint_public_key: [u8; 32],
    issued_at_ns: u64,
    lifetime_ns: u64,
) -> Result<EndpointDelegation, EndpointDelegationError> {
    let expires_at_ns = issued_at_ns
        .checked_add(lifetime_ns)
        .ok_or(EndpointDelegationError::InvalidValidity)?;
    let mut delegation = EndpointDelegation::unsigned(
        device.verifying_key().to_bytes(),
        endpoint_public_key,
        issued_at_ns,
        expires_at_ns,
    );
    if lifetime_ns == 0 || lifetime_ns > MAX_LIFETIME_NS {
        return Err(EndpointDelegationError::InvalidValidity);
    }
    let digest = delegation
        .signing_digest()
        .map_err(|_| EndpointDelegationError::Signature)?;
    delegation.signature = device.sign(&digest).to_bytes();
    Ok(delegation)
}

/// Whether `endpoint` may speak for the device in `delegation`, live at
/// `now_ns`.
///
/// # Errors
///
/// As [`EndpointDelegation::verify`].
pub fn verify_endpoint_delegation(
    delegation: &EndpointDelegation,
    endpoint: &[u8; 32],
    now_ns: u64,
) -> Result<(), EndpointDelegationError> {
    delegation.verify(endpoint, now_ns)
}

/// Why a delegation does not admit a connection.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum EndpointDelegationError {
    /// It speaks for another transport key.
    #[error("the delegation names another transport key")]
    WrongEndpoint,
    /// The window is empty, backwards or longer than [`MAX_LIFETIME_NS`].
    #[error("the delegation's window is invalid")]
    InvalidValidity,
    /// It is not in force yet.
    #[error("the delegation is not yet in force")]
    NotYetValid,
    /// It ran out.
    #[error("the delegation has expired")]
    Expired,
    /// The device's signature did not verify.
    #[error("the delegation is not the device's signature")]
    Signature,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_720_000_000_000_000_000;
    const HOUR: u64 = 60 * 60 * 1_000_000_000;

    fn keys() -> (ed25519_dalek::SigningKey, [u8; 32]) {
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let page = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        (device, page.verifying_key().to_bytes())
    }

    #[test]
    fn a_device_admits_its_transport_key_and_nothing_else() {
        let (device, page) = keys();
        let delegation = sign_endpoint_delegation(&device, page, NOW, HOUR).unwrap();
        assert_eq!(
            delegation.device_public_key,
            device.verifying_key().to_bytes()
        );
        assert_eq!(verify_endpoint_delegation(&delegation, &page, NOW), Ok(()));
        assert_eq!(
            verify_endpoint_delegation(&delegation, &[3; 32], NOW),
            Err(EndpointDelegationError::WrongEndpoint)
        );
    }

    #[test]
    fn a_delegation_expires_and_never_outlives_its_bound() {
        let (device, page) = keys();
        let delegation = sign_endpoint_delegation(&device, page, NOW, HOUR).unwrap();
        assert_eq!(
            verify_endpoint_delegation(&delegation, &page, NOW + HOUR),
            Err(EndpointDelegationError::Expired)
        );
        assert_eq!(
            verify_endpoint_delegation(&delegation, &page, NOW - 1),
            Err(EndpointDelegationError::NotYetValid)
        );
        assert_eq!(
            sign_endpoint_delegation(&device, page, NOW, MAX_LIFETIME_NS + 1),
            Err(EndpointDelegationError::InvalidValidity)
        );
        assert_eq!(
            sign_endpoint_delegation(&device, page, NOW, 0),
            Err(EndpointDelegationError::InvalidValidity)
        );
    }

    #[test]
    fn another_key_cannot_speak_for_the_device() {
        let (device, page) = keys();
        let mut delegation = sign_endpoint_delegation(&device, page, NOW, HOUR).unwrap();
        delegation.signature = [0; 64];
        assert_eq!(
            verify_endpoint_delegation(&delegation, &page, NOW),
            Err(EndpointDelegationError::Signature)
        );
        // A delegation the page signed for itself is not the device's.
        let forged = sign_endpoint_delegation(
            &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
            page,
            NOW,
            HOUR,
        )
        .unwrap();
        assert_eq!(
            verify_endpoint_delegation(&forged, &page, NOW),
            Ok(()),
            "it is its own device's signature"
        );
        assert_ne!(forged.device_public_key, device.verifying_key().to_bytes());
    }

    #[test]
    fn a_signer_that_keeps_its_key_attaches_the_same_signature() {
        let (device, page) = keys();
        let signed = sign_endpoint_delegation(&device, page, NOW, HOUR).unwrap();
        let digest =
            EndpointDelegation::unsigned(device.verifying_key().to_bytes(), page, NOW, NOW + HOUR)
                .signing_digest()
                .unwrap();
        let attached =
            EndpointDelegation::unsigned(device.verifying_key().to_bytes(), page, NOW, NOW + HOUR)
                .with_signature(device.sign(&digest).to_bytes());
        assert_eq!(attached, signed);
    }

    #[test]
    fn a_delegation_round_trips_as_json() {
        let (device, page) = keys();
        let delegation = sign_endpoint_delegation(&device, page, NOW, HOUR).unwrap();
        let json = serde_json::to_string(&delegation).unwrap();
        assert_eq!(
            serde_json::from_str::<EndpointDelegation>(&json).unwrap(),
            delegation
        );
    }
}
