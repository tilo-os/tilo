//! Enrollment tokens — the pairing primitive that lets an
//! already-trusted device admit a new device into the user's fleet.
//!
//! # Flow
//!
//! 1. On the trusted device (the "issuer"), the user asks to enrol
//!    a new device. The issuer's software calls
//!    [`mint_enrollment_token`], producing an [`EnrollmentToken`]
//!    signed by the issuer's device signing key. The token embeds
//!    the user's root public key, a fresh nonce, an expiry, the
//!    capabilities the new device will have, and the issuer's own
//!    device pubkey.
//! 2. The user hands the token to the new device out of band — a
//!    QR code, a spoken/typed base32 string, an mDNS handshake,
//!    Bluetooth. The wire form ([`encode_enrollment_token`]) is
//!    base32-Crockford so it survives being read aloud.
//! 3. The new device generates its own device keypair, decodes the
//!    token via [`decode_enrollment_token`] + verifies the
//!    signature under the issuer's device pubkey via
//!    [`verify_enrollment_token`], and posts its device pubkey +
//!    nonce back to the issuer over the same OOB channel (or over
//!    the LAN once discoverable).
//! 4. The issuer forwards the device-pubkey + capabilities to a
//!    device holding the user's *root* private key (which may be
//!    the issuer itself), which signs a [`DeviceCert`] and returns
//!    it. The new device stores its keypair + the cert.
//!
//! # Why sign with the issuer's device key
//!
//! Only the root signing key can produce a `DeviceCert`. Enrollment
//! tokens are a *proof of intent* — "I, this trusted device,
//! authorize a device presenting `nonce` to be enrolled with these
//! capabilities." That intent is what the root-holder cross-checks
//! before issuing the cert. The token expiry keeps a stolen token
//! from being replayable indefinitely.
//!
//! Encoding: the token is postcard-serialised, then base32-Crockford
//! encoded (no ambiguity between `0`/`O` and `1`/`I`/`l`). A modest
//! version-1 token is on the order of 200-260 base32 chars — fine
//! for a QR code, still readable aloud in ~30 seconds.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes, signing_digest};
use crate::cert::DeviceCapabilities;

/// Signing-domain tag for `EnrollmentToken`. Frozen wire ABI:
/// `qos-enrollment-v1` + NUL.
pub const ENROLLMENT_PREIMAGE_TAG: &[u8] = b"qos-enrollment-v1\0";

/// A signed enrollment intent minted by a trusted device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentToken {
    /// The user identity this token enrolls into.
    pub user_public_key: [u8; 32],
    /// Fresh 16-byte nonce. Unique per token; used to correlate the
    /// hand-off from user-visible OOB channel back to the issuer.
    pub nonce: [u8; 16],
    /// Nanoseconds since epoch after which the token is invalid.
    /// Practical presets: 5 minutes for QR pairing, 24 hours for
    /// an "invite" URL.
    pub expires_at_ns: u64,
    /// Capabilities the resulting device cert will carry.
    pub capabilities: DeviceCapabilities,
    /// Device pubkey of the issuer that signed this token.
    pub issuer_device_public_key: [u8; 32],
    /// Ed25519 signature over
    /// `blake3(ENROLLMENT_PREIMAGE_TAG || postcard(view))`.
    #[serde(with = "serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct TokenView<'a> {
    user_public_key: &'a [u8; 32],
    nonce: &'a [u8; 16],
    expires_at_ns: u64,
    capabilities: &'a DeviceCapabilities,
    issuer_device_public_key: &'a [u8; 32],
}

impl EnrollmentToken {
    fn view(&self) -> TokenView<'_> {
        TokenView {
            user_public_key: &self.user_public_key,
            nonce: &self.nonce,
            expires_at_ns: self.expires_at_ns,
            capabilities: &self.capabilities,
            issuer_device_public_key: &self.issuer_device_public_key,
        }
    }

    /// Returns `true` if `now_ns >= self.expires_at_ns`.
    pub fn is_expired_at(&self, now_ns: u64) -> bool {
        now_ns >= self.expires_at_ns
    }
}

/// Failure modes for enrollment.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum EnrollmentError {
    /// Issuer's device public key does not decode.
    #[error("invalid issuer device public key")]
    InvalidIssuerPublicKey,
    /// Signature did not verify under `issuer_device_public_key`.
    #[error("enrollment-token signature did not verify")]
    BadSignature,
    /// Token expiry has passed.
    #[error("enrollment token has expired")]
    Expired,
    /// Base32 decoding of the wire form failed.
    #[error("enrollment token is malformed base32")]
    MalformedBase32,
    /// Postcard decoding of the base32-decoded payload failed.
    #[error("enrollment token payload malformed: {0}")]
    Payload(#[from] CanonicalError),
}

/// Mint an enrollment token signed by an already-trusted device.
///
/// `issuer_signing_key` MUST correspond to a device that holds a
/// valid `DeviceCert` under `user_public_key` with
/// `can_admit_devices == true`. This function does not enforce
/// that policy (it is stateless); the caller wires the check.
pub fn mint_enrollment_token(
    user_public_key: [u8; 32],
    capabilities: DeviceCapabilities,
    expires_at_ns: u64,
    nonce: [u8; 16],
    issuer_signing_key: &ed25519_dalek::SigningKey,
) -> Result<EnrollmentToken, EnrollmentError> {
    let issuer_device_public_key = issuer_signing_key.verifying_key().to_bytes();
    let mut token = EnrollmentToken {
        user_public_key,
        nonce,
        expires_at_ns,
        capabilities,
        issuer_device_public_key,
        signature: [0u8; 64],
    };
    let digest = signing_digest(ENROLLMENT_PREIMAGE_TAG, &token.view())?;
    let sig = issuer_signing_key.sign(&digest);
    token.signature = sig.to_bytes();
    Ok(token)
}

/// Verify an enrollment token's signature + expiry.
///
/// Returns `Ok(())` if the token is cryptographically valid and
/// unexpired at `now_ns`. Does not check the issuer's cert chain
/// or capabilities — that is up to the caller (typically the
/// device holding the root key).
pub fn verify_enrollment_token(
    token: &EnrollmentToken,
    now_ns: u64,
) -> Result<(), EnrollmentError> {
    if token.is_expired_at(now_ns) {
        return Err(EnrollmentError::Expired);
    }
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&token.issuer_device_public_key)
        .map_err(|_| EnrollmentError::InvalidIssuerPublicKey)?;
    let digest = signing_digest(ENROLLMENT_PREIMAGE_TAG, &token.view())?;
    let sig = ed25519_dalek::Signature::from_bytes(&token.signature);
    vk.verify(&digest, &sig)
        .map_err(|_| EnrollmentError::BadSignature)?;
    Ok(())
}

/// Encode a token to a printable base32-Crockford string.
///
/// Crockford variant is chosen for out-of-band readability: it drops
/// ambiguous characters (`0`/`O`, `1`/`I`/`l`) and is
/// case-insensitive. A v1 token encodes to roughly 250-260 chars —
/// fits inside a printable QR code with plenty of headroom.
pub fn encode_enrollment_token(token: &EnrollmentToken) -> Result<String, EnrollmentError> {
    let bytes = canonical_bytes(token)?;
    Ok(base32::encode(base32::Alphabet::Crockford, &bytes))
}

/// Decode a base32-Crockford-encoded token back into an
/// [`EnrollmentToken`]. Rejects malformed base32 and any postcard
/// decode error (including trailing bytes).
pub fn decode_enrollment_token(text: &str) -> Result<EnrollmentToken, EnrollmentError> {
    let bytes = base32::decode(base32::Alphabet::Crockford, text)
        .ok_or(EnrollmentError::MalformedBase32)?;
    let token: EnrollmentToken = from_canonical_bytes(&bytes)?;
    Ok(token)
}

mod serde_bytes_64 {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v: serde_bytes::ByteBuf = serde_bytes::ByteBuf::deserialize(d)?;
        let bytes = v.into_vec();
        if bytes.len() != 64 {
            return Err(D::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceIdentity, UserIdentity};

    fn issuer(user_pk: [u8; 32]) -> ed25519_dalek::SigningKey {
        let (sk, _dev) = DeviceIdentity::generate("primary", 100);
        let _ = user_pk;
        sk
    }

    #[test]
    fn mint_and_verify_round_trip() {
        let (_root, user) = UserIdentity::generate("Alice", 1);
        let issuer_sk = issuer(user.public_key);
        let token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::member(),
            10_000,
            [7u8; 16],
            &issuer_sk,
        )
        .unwrap();
        verify_enrollment_token(&token, 5_000).unwrap();
    }

    #[test]
    fn expiry_enforced() {
        let (_root, user) = UserIdentity::generate("Alice", 1);
        let issuer_sk = issuer(user.public_key);
        let token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::member(),
            10_000,
            [7u8; 16],
            &issuer_sk,
        )
        .unwrap();
        assert_eq!(
            verify_enrollment_token(&token, 10_000).unwrap_err(),
            EnrollmentError::Expired
        );
        assert_eq!(
            verify_enrollment_token(&token, 11_000).unwrap_err(),
            EnrollmentError::Expired
        );
    }

    #[test]
    fn tampered_capabilities_fails() {
        let (_root, user) = UserIdentity::generate("Alice", 1);
        let issuer_sk = issuer(user.public_key);
        let mut token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::member(),
            10_000,
            [7u8; 16],
            &issuer_sk,
        )
        .unwrap();
        token.capabilities.can_admit_devices = true;
        assert_eq!(
            verify_enrollment_token(&token, 5_000).unwrap_err(),
            EnrollmentError::BadSignature
        );
    }

    #[test]
    fn base32_round_trip() {
        let (_root, user) = UserIdentity::generate("Alice", 1);
        let issuer_sk = issuer(user.public_key);
        let token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::host(),
            10_000,
            [42u8; 16],
            &issuer_sk,
        )
        .unwrap();
        let encoded = encode_enrollment_token(&token).unwrap();
        // Should be printable ASCII, decidedly short enough for a
        // QR code (base32 makes it ~1.6x the raw byte count; the
        // raw payload here is ~180 bytes → ~290 chars).
        assert!(encoded.is_ascii());
        assert!(encoded.len() < 400);
        // No lowercase (Crockford is upper-case output).
        assert!(
            encoded
                .chars()
                .all(|c| !c.is_ascii_lowercase() || c == '=' || c.is_ascii_digit())
        );

        let back = decode_enrollment_token(&encoded).unwrap();
        assert_eq!(token, back);
        verify_enrollment_token(&back, 5_000).unwrap();
    }

    #[test]
    fn malformed_base32_rejected() {
        // Contains characters not in the Crockford alphabet (`!`).
        assert_eq!(
            decode_enrollment_token("!!!!!!!!").unwrap_err(),
            EnrollmentError::MalformedBase32
        );
        // Valid base32 but not a token → payload decode fails.
        let bytes = base32::encode(base32::Alphabet::Crockford, &[0u8, 1, 2, 3]);
        assert!(matches!(
            decode_enrollment_token(&bytes).unwrap_err(),
            EnrollmentError::Payload(_)
        ));
    }

    #[test]
    fn signed_by_different_issuer_verify_fails() {
        let (_root, user) = UserIdentity::generate("Alice", 1);
        let issuer_sk = issuer(user.public_key);
        let attacker = issuer(user.public_key);
        let mut token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::member(),
            10_000,
            [7u8; 16],
            &issuer_sk,
        )
        .unwrap();
        // Replace the recorded issuer pubkey with the attacker's;
        // the signature was made under the real issuer's key so it
        // won't verify under the attacker's.
        token.issuer_device_public_key = attacker.verifying_key().to_bytes();
        assert_eq!(
            verify_enrollment_token(&token, 5_000).unwrap_err(),
            EnrollmentError::BadSignature
        );
    }
}
