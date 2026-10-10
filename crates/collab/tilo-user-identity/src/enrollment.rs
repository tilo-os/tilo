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
//!    the issuer itself), which signs a [`DeviceCert`](crate::cert::DeviceCert) and returns
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
/// `tilo-enrollment-v2` + NUL.
pub const ENROLLMENT_PREIMAGE_TAG: &[u8] = b"tilo-enrollment-v2\0";
/// Enrollment intent is deliberately short-lived, even when correctly signed.
pub const MAX_ENROLLMENT_TOKEN_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// A signed enrollment intent minted by a trusted device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentToken {
    /// The user identity this token enrolls into.
    pub user_public_key: [u8; 32],
    /// Fresh 16-byte nonce. Unique per token; used to correlate the
    /// hand-off from user-visible OOB channel back to the issuer.
    pub nonce: [u8; 16],
    /// Nanoseconds since epoch when the trusted device minted this token.
    pub issued_at_ns: u64,
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
    #[serde(with = "crate::cert::serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct TokenView<'a> {
    user_public_key: &'a [u8; 32],
    nonce: &'a [u8; 16],
    issued_at_ns: u64,
    expires_at_ns: u64,
    capabilities: &'a DeviceCapabilities,
    issuer_device_public_key: &'a [u8; 32],
}

impl EnrollmentToken {
    fn view(&self) -> TokenView<'_> {
        TokenView {
            user_public_key: &self.user_public_key,
            nonce: &self.nonce,
            issued_at_ns: self.issued_at_ns,
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
    /// Issuance and expiry do not form a bounded forward interval.
    #[error("enrollment token validity must be positive and at most 24 hours")]
    InvalidValidity,
    /// The token claims to have been minted in the future.
    #[error("enrollment token is not yet valid")]
    NotYetValid,
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
///
/// # Errors
///
/// Returns [`EnrollmentError::InvalidValidity`] unless expiry is after
/// issuance and no more than 24 hours later. Returns
/// [`EnrollmentError::Payload`] if the token cannot be canonically encoded
/// for signing (effectively unreachable).
pub fn mint_enrollment_token(
    user_public_key: [u8; 32],
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: u64,
    nonce: [u8; 16],
    issuer_signing_key: &ed25519_dalek::SigningKey,
) -> Result<EnrollmentToken, EnrollmentError> {
    expires_at_ns
        .checked_sub(issued_at_ns)
        .filter(|lifetime| *lifetime > 0 && *lifetime <= MAX_ENROLLMENT_TOKEN_LIFETIME_NS)
        .ok_or(EnrollmentError::InvalidValidity)?;
    let issuer_device_public_key = issuer_signing_key.verifying_key().to_bytes();
    let mut token = EnrollmentToken {
        user_public_key,
        nonce,
        issued_at_ns,
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
///
/// # Errors
///
/// Returns [`EnrollmentError::InvalidValidity`] for an empty, backwards, or
/// overlong validity interval, [`EnrollmentError::NotYetValid`] before the
/// issuance time, [`EnrollmentError::Expired`] at or past expiry,
/// [`EnrollmentError::InvalidIssuerPublicKey`] for an invalid Ed25519 issuer,
/// or [`EnrollmentError::BadSignature`] if verification fails.
pub fn verify_enrollment_token(
    token: &EnrollmentToken,
    now_ns: u64,
) -> Result<(), EnrollmentError> {
    token
        .expires_at_ns
        .checked_sub(token.issued_at_ns)
        .filter(|lifetime| *lifetime > 0 && *lifetime <= MAX_ENROLLMENT_TOKEN_LIFETIME_NS)
        .ok_or(EnrollmentError::InvalidValidity)?;
    if now_ns < token.issued_at_ns {
        return Err(EnrollmentError::NotYetValid);
    }
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

/// Crockford base32: upper-case output without padding; decoding is
/// case-insensitive, reads `O` as `0` and `I`/`L` as `1`, and ignores unused
/// bits in the last symbol.
pub(crate) const CROCKFORD: data_encoding::Encoding = data_encoding_macro::new_encoding! {
    symbols: "0123456789ABCDEFGHJKMNPQRSTVWXYZ",
    translate_from: "abcdefghjkmnpqrstvwxyzOoIiLl",
    translate_to: "ABCDEFGHJKMNPQRSTVWXYZ001111",
    check_trailing_bits: false,
};

/// Decode Crockford base32, tolerating a length no encoder produces (1, 3 or
/// 6 symbols past a full block): such a final symbol holds no whole byte, so
/// it is checked to be a symbol and then dropped, as earlier builds did.
fn decode_crockford(text: &str) -> Option<Vec<u8>> {
    let symbols = text.as_bytes();
    match CROCKFORD.decode(symbols) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind == data_encoding::DecodeKind::Length => {
            let (last, head) = symbols.split_last()?;
            CROCKFORD.decode(&[*last, *last]).ok()?;
            CROCKFORD.decode(head).ok()
        }
        Err(_) => None,
    }
}

/// Encode a token to a printable base32-Crockford string.
///
/// Crockford variant is chosen for out-of-band readability: it drops
/// ambiguous characters (`0`/`O`, `1`/`I`/`l`) and is
/// case-insensitive. A v2 token encodes to roughly 250-260 chars —
/// fits inside a printable QR code with plenty of headroom.
///
/// # Errors
///
/// Returns [`EnrollmentError::Payload`] if `token` cannot be
/// canonically encoded (effectively unreachable).
pub fn encode_enrollment_token(token: &EnrollmentToken) -> Result<String, EnrollmentError> {
    let bytes = canonical_bytes(token)?;
    Ok(CROCKFORD.encode(&bytes))
}

/// Decode a base32-Crockford-encoded token back into an
/// [`EnrollmentToken`]. Rejects malformed base32 and any postcard
/// decode error (including trailing bytes).
///
/// # Errors
///
/// Returns [`EnrollmentError::MalformedBase32`] if `text` is not
/// valid Crockford base32, or [`EnrollmentError::Payload`] if the
/// decoded bytes do not deserialize as an `EnrollmentToken`.
pub fn decode_enrollment_token(text: &str) -> Result<EnrollmentToken, EnrollmentError> {
    let bytes = decode_crockford(text).ok_or(EnrollmentError::MalformedBase32)?;
    let token: EnrollmentToken = from_canonical_bytes(&bytes)?;
    Ok(token)
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

    /// Tokens mint and verify, expiry and issuance/maximum lifetime are enforced, tampered
    /// capabilities fail, and a different issuer fails.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn enrollment_tokens_verify_only_untampered_live_and_from_their_issuer() {
        // Phase: mint_and_verify_round_trip.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            let token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::member(),
                5_000,
                10_000,
                [7u8; 16],
                &issuer_sk,
            )
            .unwrap();
            verify_enrollment_token(&token, 5_000).unwrap();
        }

        // Phase: expiry_enforced.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            let token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::member(),
                5_000,
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

        // Phase: issuance_and_maximum_lifetime_are_enforced.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            assert_eq!(
                mint_enrollment_token(
                    user.public_key,
                    DeviceCapabilities::member(),
                    10,
                    10 + MAX_ENROLLMENT_TOKEN_LIFETIME_NS + 1,
                    [8; 16],
                    &issuer_sk,
                ),
                Err(EnrollmentError::InvalidValidity)
            );
            let token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::member(),
                10,
                20,
                [9; 16],
                &issuer_sk,
            )
            .unwrap();
            assert_eq!(
                verify_enrollment_token(&token, 9),
                Err(EnrollmentError::NotYetValid)
            );

            // Verification independently rejects an overlong token even when its
            // contents carry a valid issuer signature.
            let mut overlong = token;
            overlong.expires_at_ns = overlong.issued_at_ns + MAX_ENROLLMENT_TOKEN_LIFETIME_NS + 1;
            let digest = signing_digest(ENROLLMENT_PREIMAGE_TAG, &overlong.view()).unwrap();
            overlong.signature = issuer_sk.sign(&digest).to_bytes();
            assert_eq!(
                verify_enrollment_token(&overlong, overlong.issued_at_ns),
                Err(EnrollmentError::InvalidValidity)
            );
        }

        // Phase: tampered_capabilities_fails.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            let mut token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::member(),
                5_000,
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

        // Phase: signed_by_different_issuer_verify_fails.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            let attacker = issuer(user.public_key);
            let mut token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::member(),
                5_000,
                10_000,
                [7u8; 16],
                &issuer_sk,
            )
            .unwrap();
            // Replace the recorded issuer pubkey with the attacker's;
            // the signature was made under the issuer's key so it
            // won't verify under the attacker's.
            token.issuer_device_public_key = attacker.verifying_key().to_bytes();
            assert_eq!(
                verify_enrollment_token(&token, 5_000).unwrap_err(),
                EnrollmentError::BadSignature
            );
        }
    }

    /// Base32 round-trips, malformed base32 is rejected, and the Crockford wire form is stable.
    #[test]
    fn enrollment_tokens_travel_as_stable_crockford_base32() {
        // Phase: base32_round_trip.
        {
            let (_root, user) = UserIdentity::generate("Alice", 1);
            let issuer_sk = issuer(user.public_key);
            let token = mint_enrollment_token(
                user.public_key,
                DeviceCapabilities::host(),
                5_000,
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

        // Phase: malformed_base32_rejected.
        {
            // Contains characters not in the Crockford alphabet (`!`).
            assert_eq!(
                decode_enrollment_token("!!!!!!!!").unwrap_err(),
                EnrollmentError::MalformedBase32
            );
            // Valid base32 but not a token → payload decode fails.
            let bytes = CROCKFORD.encode(&[0u8, 1, 2, 3]);
            assert!(matches!(
                decode_enrollment_token(&bytes).unwrap_err(),
                EnrollmentError::Payload(_)
            ));
        }

        // Phase: crockford_wire_form_is_stable.
        // Crockford vectors: the encoding is fixed on the wire, and decoding
        // keeps accepting the read-aloud spellings and stray symbols it always
        // accepted.
        {
            assert_eq!(CROCKFORD.encode(b""), "");
            assert_eq!(CROCKFORD.encode(b"f"), "CR");
            assert_eq!(CROCKFORD.encode(b"foobar"), "CSQPYRK1E8");
            assert_eq!(CROCKFORD.encode(&[0xff; 5]), "ZZZZZZZZ");
            assert_eq!(CROCKFORD.encode(&[0, 1, 2, 3]), "000G40R");
            for spelling in [
                "CSQPYRK1E8",
                "csqpyrk1e8",
                "CSQPYRKIE8",
                "CSQPYRKlE8",
                "CSQPYRK1E8Z",
            ] {
                assert_eq!(
                    decode_crockford(spelling).as_deref(),
                    Some(&b"foobar"[..]),
                    "{spelling}"
                );
            }
            assert_eq!(
                decode_crockford("OOOG4OR").as_deref(),
                Some(&[0u8, 1, 2, 3][..])
            );
            assert_eq!(decode_crockford("CS").as_deref(), Some(&b"f"[..]));
            for invalid in ["CSQPYRKUE8", "CSQPYRK1E8!", "CSQPYRK1E=", "!!!!!!!!"] {
                assert_eq!(decode_crockford(invalid), None, "{invalid}");
            }
        }
    }
}
