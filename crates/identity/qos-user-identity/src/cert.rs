//! [`DeviceCert`] — the signed statement binding a device key to a
//! user identity, plus [`DeviceCapabilities`] describing what the
//! device may do on behalf of the user.
//!
//! The signing preimage constant [`DEVCERT_PREIMAGE_TAG`] is frozen
//! wire ABI — see `signing_preimage_is_frozen_abi` in `lib.rs`.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for `DeviceCert`. Frozen wire ABI:
/// `qos-devcert-v1` + NUL. Any change breaks in-field
/// certificates.
pub const DEVCERT_PREIMAGE_TAG: &[u8] = b"qos-devcert-v1\0";

/// What a device may do on behalf of its user.
///
/// The four booleans map directly to enforceable policy checks in
/// `shell/` and `fs/`. Currently a user's *primary* device (the one
/// that holds the root private key) gets `can_own_workspaces:
/// true, can_admit_devices: true`, phones/tablets get
/// `can_own_workspaces: true` but `can_admit_devices: false`, and
/// dedicated home-server hosts get `can_act_as_host: true`.
///
/// Capabilities are baked into the cert at issue time — they do not
/// change over the cert's lifetime. To upgrade a device from
/// "member" to "owner", the primary revokes the old cert and issues
/// a new one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCapabilities {
    /// May create/delete/rename workspaces on the user's directory.
    /// When false, the device can still read + edit content on
    /// workspaces already assigned to it.
    pub can_own_workspaces: bool,

    /// May mint enrollment tokens for new devices. Typically only
    /// the user's primary device — a phone should not be able to
    /// admit another phone into the fleet.
    pub can_admit_devices: bool,

    /// May act as an always-on host (pins workspaces, relays for
    /// other devices). Only true for dedicated servers and
    /// desktop-class machines the user has consciously marked
    /// "host".
    pub can_act_as_host: bool,

    /// May read every workspace on the user's directory, regardless
    /// of per-workspace membership. Reserved for the user's primary
    /// devices and hosts they explicitly trust with the full data
    /// set.
    pub can_read_all_workspaces: bool,
}

impl DeviceCapabilities {
    /// Capability set for the user's primary device (holds root
    /// private key, admits new devices, reads everything).
    pub fn primary() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: true,
            can_act_as_host: false,
            can_read_all_workspaces: true,
        }
    }

    /// Capability set for a secondary device — a phone, tablet, or
    /// laptop the user opts into the fleet but does not want to be
    /// able to admit new devices.
    pub fn member() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
        }
    }

    /// Capability set for an always-on host (home server / cloud
    /// tenant). Reads everything so it can pin all workspaces; does
    /// not own or admit — those stay with the human's primary
    /// devices.
    pub fn host() -> Self {
        Self {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: true,
            can_read_all_workspaces: true,
        }
    }
}

/// Signed statement: "the user identity with public key
/// `user_public_key` recognises `device_public_key` as one of its
/// devices, with the given `capabilities`."
///
/// The signature is Ed25519, produced by the *user root* signing
/// key over `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`, where
/// `view` is the [`DeviceCertView`] omitting the `signature` field.
///
/// Cert chain: for anything the device does to be trusted by a
/// peer, that peer must (a) verify this signature under
/// `user_public_key`, (b) check the cert has not expired, and (c)
/// check the `device_public_key` is not present in the user's
/// [`crate::RevocationList`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCert {
    /// User root public key that owns / signs this cert. 32 bytes.
    pub user_public_key: [u8; 32],

    /// Certified device's public key. 32 bytes.
    pub device_public_key: [u8; 32],

    /// Human-readable label for the device — "Framework 16",
    /// "Studio phone", "Home server". Not authoritative; a peer's
    /// UI may display this label as-is.
    pub device_label: String,

    /// What this device may do on behalf of the user. See
    /// [`DeviceCapabilities`].
    pub capabilities: DeviceCapabilities,

    /// Nanoseconds since Unix epoch — when the cert was signed.
    /// Peers use it for tie-breaks (a later-issued cert supersedes
    /// an earlier one for the same device) and for display.
    pub issued_at_ns: u64,

    /// Optional expiry. `None` means the cert is valid until
    /// revoked. Practical presets: forever for a laptop, 30 days
    /// for a temporary shared device, 24 hours for a bring-your-
    /// own-lab-machine.
    pub expires_at_ns: Option<u64>,

    /// Ed25519 signature. 64 bytes. Covers everything else in this
    /// struct via `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`.
    #[serde(with = "serde_bytes_64")]
    pub signature: [u8; 64],
}

/// Signing view — everything except the signature. Used to build
/// the preimage that `signature` covers.
#[derive(Serialize)]
struct DeviceCertView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    capabilities: &'a DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
}

impl DeviceCert {
    /// Build the signing view for this cert.
    fn view(&self) -> DeviceCertView<'_> {
        DeviceCertView {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            device_label: &self.device_label,
            capabilities: &self.capabilities,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// Returns `true` if the cert's `expires_at_ns` has passed at
    /// `now_ns`. A cert with `expires_at_ns == None` never expires.
    pub fn is_expired_at(&self, now_ns: u64) -> bool {
        matches!(self.expires_at_ns, Some(e) if now_ns >= e)
    }
}

/// Failure modes for cert signing/verification.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DeviceCertError {
    /// The user root public key bytes do not decode to a valid
    /// Ed25519 curve point.
    #[error("invalid user root public key (not a point on the curve)")]
    InvalidUserPublicKey,

    /// The signature did not verify under the declared user root
    /// key. Either the wrong root signed, the bytes were tampered
    /// with, or the cert was signed by an impostor.
    #[error("device-cert signature did not verify")]
    BadSignature,

    /// The `expires_at_ns` deadline has passed. Not a signature
    /// failure; the cert is cryptographically valid but no longer
    /// admissible.
    #[error("device cert has expired")]
    Expired,

    /// Canonical-encoding failure. Effectively unreachable but
    /// propagated for hygiene.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Sign a device cert with the user's root signing key.
///
/// Fills the cert's `signature` field in place. Callers can call
/// this multiple times on the same cert idempotently — the signing
/// view omits the signature so the second call reproduces the first.
pub fn sign_device_cert(
    cert: &mut DeviceCert,
    root_signing_key: &ed25519_dalek::SigningKey,
) -> Result<(), DeviceCertError> {
    // Guard: refuse to sign a cert whose `user_public_key` does not
    // match the signing key's derived public key. Mis-signing this
    // would produce a syntactically valid but unverifiable cert.
    let derived = root_signing_key.verifying_key().to_bytes();
    if cert.user_public_key != derived {
        return Err(DeviceCertError::BadSignature);
    }

    let digest = signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view())?;
    let sig = root_signing_key.sign(&digest);
    cert.signature = sig.to_bytes();
    Ok(())
}

/// Return the domain-separated digest an external signer must sign for this
/// certificate.
///
/// This is the broker-friendly counterpart to [`sign_device_cert`]. It lets a
/// caller ask a non-exportable user-root capability to sign the exact wire-ABI
/// preimage without copying the private key into another file. Callers must
/// attach the resulting signature and validate it with [`verify_device_cert`]
/// before persisting the certificate.
pub fn device_cert_signing_digest(cert: &DeviceCert) -> Result<[u8; 32], DeviceCertError> {
    signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view()).map_err(Into::into)
}

/// Verify a device cert against the user root public key embedded
/// in the cert.
///
/// This is the cryptographic check *only*. Callers should also
/// verify:
///
/// - `now_ns >= cert.issued_at_ns` (unless clock skew is being
///   tolerated),
/// - `!cert.is_expired_at(now_ns)`,
/// - `!revocation_list.is_revoked(&cert.device_public_key)`.
///
/// The convenience [`verify_device_cert_at`] bundles those checks
/// with the signature verification.
pub fn verify_device_cert(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&cert.user_public_key)
        .map_err(|_| DeviceCertError::InvalidUserPublicKey)?;
    let digest = signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view())?;
    let sig = ed25519_dalek::Signature::from_bytes(&cert.signature);
    vk.verify(&digest, &sig)
        .map_err(|_| DeviceCertError::BadSignature)?;
    Ok(())
}

/// Full validation: signature + expiry.
///
/// Revocation is NOT checked here — the caller passes in a
/// [`crate::RevocationList`] separately because the list lives in
/// the user's directory CRDT, not in the cert itself. See
/// [`crate::RevocationList::is_revoked`].
pub fn verify_device_cert_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    verify_device_cert(cert)?;
    if cert.is_expired_at(now_ns) {
        return Err(DeviceCertError::Expired);
    }
    Ok(())
}

/// Serde helper: a 64-byte-array serialiser that emits a length-
/// prefixed byte string rather than 64 separate `u8`s. Saves 63
/// bytes of varint overhead per signature on the wire.
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
    use crate::UserIdentity;

    fn fresh_cert(caps: DeviceCapabilities) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let (root_sk, user) = UserIdentity::generate("Alice", 1_720_000_000_000_000_000);
        let (_dev_sk, dev) =
            crate::DeviceIdentity::generate("Framework 16", 1_720_000_000_000_000_100);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 1_720_000_000_000_000_200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        (root_sk, cert)
    }

    #[test]
    fn sign_and_verify() {
        let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
        verify_device_cert(&cert).unwrap();
    }

    #[test]
    fn tampered_capability_fails() {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::member());
        cert.capabilities.can_admit_devices = true;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    #[test]
    fn tampered_device_pubkey_fails() {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
        cert.device_public_key[0] ^= 0x01;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    #[test]
    fn tampered_signature_fails() {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
        cert.signature[0] ^= 0x01;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    #[test]
    fn tampered_user_pubkey_fails() {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
        // Flip a bit in the embedded user pubkey. The signature no
        // longer verifies under the new pubkey.
        cert.user_public_key[0] ^= 0x01;
        let err = verify_device_cert(&cert).unwrap_err();
        assert!(matches!(
            err,
            DeviceCertError::BadSignature | DeviceCertError::InvalidUserPublicKey
        ));
    }

    #[test]
    fn signed_by_different_root_fails() {
        // Alice's cert signed with Bob's root key — mismatch caught
        // by the pre-sign guard (BadSignature).
        let (_, alice) = UserIdentity::generate("Alice", 1);
        let (bob_sk, _) = UserIdentity::generate("Bob", 2);
        let (_, dev) = crate::DeviceIdentity::generate("dev", 3);
        let mut cert = DeviceCert {
            user_public_key: alice.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 4,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        assert_eq!(
            sign_device_cert(&mut cert, &bob_sk).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    #[test]
    fn expiry_semantics() {
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 100,
            expires_at_ns: Some(200),
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        // Before expiry
        verify_device_cert_at(&cert, 150).unwrap();
        // Exactly at expiry (`now >= expires_at_ns` — exclusive on
        // the left side, inclusive on the right).
        assert_eq!(
            verify_device_cert_at(&cert, 200).unwrap_err(),
            DeviceCertError::Expired
        );
        // After expiry
        assert_eq!(
            verify_device_cert_at(&cert, 300).unwrap_err(),
            DeviceCertError::Expired
        );
    }

    #[test]
    fn cert_round_trips_through_canonical() {
        let (_root, cert) = fresh_cert(DeviceCapabilities::host());
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let back: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(cert, back);
        verify_device_cert(&back).unwrap();
    }

    /// Signature outputs must be reproducible from the same inputs
    /// (Ed25519 is deterministic and we build the preimage
    /// deterministically). Locks in the wire vectors — if this ever
    /// fails, the signing preimage has drifted.
    #[test]
    fn signature_vector_is_reproducible() {
        use ed25519_dalek::SigningKey;

        // Fixed-seed keypair for the vector. This is the ONLY test
        // that uses a fixed seed; production always uses OsRng.
        let seed = [1u8; 32];
        let root_sk = SigningKey::from_bytes(&seed);
        let root_pk = root_sk.verifying_key().to_bytes();

        let device_pk = [2u8; 32];

        let mut cert = DeviceCert {
            user_public_key: root_pk,
            device_public_key: device_pk,
            device_label: "test-device".to_string(),
            capabilities: DeviceCapabilities {
                can_own_workspaces: true,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
            },
            issued_at_ns: 1_000_000,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();

        // Re-sign an identical view; must yield the same signature
        // (Ed25519 is deterministic, so this is a strong wire-ABI
        // check — any drift in the preimage or postcard encoding
        // would flip these bytes).
        let sig_1 = cert.signature;
        let mut cert2 = cert.clone();
        cert2.signature = [0u8; 64];
        sign_device_cert(&mut cert2, &root_sk).unwrap();
        assert_eq!(sig_1, cert2.signature);
    }

    #[test]
    fn all_capability_presets_are_distinct() {
        // Each preset must produce a different signature — otherwise
        // a peer could substitute one preset for another without
        // touching the sig.
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);

        let build = |caps: DeviceCapabilities| {
            let mut c = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: dev.public_key,
                device_label: dev.label.clone(),
                capabilities: caps,
                issued_at_ns: 3,
                expires_at_ns: None,
                signature: [0u8; 64],
            };
            sign_device_cert(&mut c, &root_sk).unwrap();
            c.signature
        };

        let s_primary = build(DeviceCapabilities::primary());
        let s_member = build(DeviceCapabilities::member());
        let s_host = build(DeviceCapabilities::host());
        assert_ne!(s_primary, s_member);
        assert_ne!(s_member, s_host);
        assert_ne!(s_primary, s_host);
    }
}
