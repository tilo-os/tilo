//! `qos-user-identity` — the human-facing identity layer for qOS.
//!
//! # Model
//!
//! Today, every device holds an Ed25519 keypair. That keypair is *the
//! device*; it does not carry any statement about who owns the
//! device. This crate adds a second, human-scoped keypair: the
//! **user root identity**. The root keypair certifies each of the
//! user's devices by signing a [`DeviceCert`] that binds
//! `device_public_key → user_public_key` together with a human-readable
//! label, a capability set, and an optional expiry.
//!
//! Workspaces are owned by a user identity, not by any single device.
//! Any device presenting a valid cert chaining to the workspace
//! owner's `user_public_key` has owner permissions on that workspace.
//! If a device is lost, stolen, or retired, the root identity issues
//! a signed [`RevocationEntry`] that peers use to reject the device
//! going forward — this is the sole mechanism for kicking a
//! previously-trusted device out.
//!
//! # Wire ABI
//!
//! Every signed type flows through the same three-step canonical
//! encoder:
//!
//! 1. Build a *view* of the struct that omits the `signature` field.
//! 2. Serialise the view with `postcard` (little-endian, varint,
//!    field-reorder-sensitive — see `canonical_bytes`).
//! 3. Prepend a fixed byte-constant preimage ("domain tag") and hash
//!    the concatenation with BLAKE3-256. Sign that 32-byte digest
//!    with Ed25519.
//!
//! The domain-tag constants are **frozen wire ABI** and must never
//! change:
//!
//! - [`DEVCERT_PREIMAGE_TAG`]     — `qos-devcert-v1\0`
//! - [`REVOCATION_PREIMAGE_TAG`]  — `qos-revocation-v1\0`
//! - [`ENROLLMENT_PREIMAGE_TAG`]  — `qos-enrollment-v1\0`
//!
//! The tag terminator (`\0`) plus a version marker (`v1`) inside the
//! literal ensure two things: (a) prefix ambiguity is impossible
//! between two tags of different length, and (b) a v2 layout can
//! coexist without silent cross-verification.
//!
//! The exact byte layouts are exercised by
//! `tests::signing_preimage_is_frozen_abi` in this file — those
//! tests must never be relaxed.
//!
//! # Non-goals
//!
//! - No I/O. The crate is pure types + pure functions. Persistence
//!   (loading `/var/lib/qos/user-identity/*`) belongs in `shell/` or
//!   `os/`, not here.
//! - No key recovery / paper share. `threshold-identity` already
//!   solves that; this crate is deliberately single-key so the wire
//!   shape stays simple.
//! - No transport. Peers exchange enrollment tokens out-of-band
//!   (QR code, LAN mdns, typed string); this crate serialises the
//!   token and validates it, but does not send it.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod canonical;
pub mod cert;
pub mod directory;
pub mod enrollment;
pub mod revocation;

pub use canonical::CanonicalError;
pub use cert::{
    DEVCERT_PREIMAGE_TAG, DeviceCapabilities, DeviceCert, DeviceCertError, sign_device_cert,
    verify_device_cert,
};
pub use directory::{
    DirectoryError, DirectoryUpdate, WorkspaceDirectory, WorkspaceId, WorkspaceMeta,
};
pub use enrollment::{
    ENROLLMENT_PREIMAGE_TAG, EnrollmentError, EnrollmentToken, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, verify_enrollment_token,
};
pub use revocation::{
    REVOCATION_PREIMAGE_TAG, RevocationEntry, RevocationError, RevocationList, RevocationReason,
    sign_revocation_entry, verify_revocation_entry,
};

use serde::{Deserialize, Serialize};

/// User's root identity — one keypair, one human.
///
/// The private half is held only on trusted devices (typically the
/// user's laptops); the public half is baked into every workspace
/// and every device cert. See the module docs for how signatures
/// chain up to this key.
///
/// This struct is `Serialize + Deserialize` so it can be persisted
/// to disk (`/var/lib/qos/user-identity/pubkey.bin` — public half
/// only) and shipped in a provisioning ISO. The private half never
/// appears in a serialized `UserIdentity`; it stays in a separate
/// `SigningKey`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentity {
    /// Ed25519 public key. 32 bytes.
    pub public_key: [u8; 32],
    /// Human-readable label — "Quinten's identity", "Ada Lovelace",
    /// etc. Not authoritative; it's only for UI display and can be
    /// changed on any device without invalidating certs.
    pub label: String,
    /// Nanoseconds since the Unix epoch. Advisory only — used for
    /// display ("this identity was created …") and to tie-break in
    /// merges where two devices concurrently mint different
    /// `UserIdentity`s. Not a security field: peers do not reject
    /// certs based on this value.
    pub created_at_ns: u64,
}

impl UserIdentity {
    /// Construct a `UserIdentity` from a raw Ed25519 public key +
    /// label + creation timestamp. Does not validate the public key
    /// is a valid curve point; that check happens lazily at
    /// signature-verification time (in [`verify_device_cert`], etc.).
    pub fn new(public_key: [u8; 32], label: impl Into<String>, created_at_ns: u64) -> Self {
        Self {
            public_key,
            label: label.into(),
            created_at_ns,
        }
    }

    /// Generate a fresh root identity keypair.
    ///
    /// Returns `(signing_key, UserIdentity)`. The `SigningKey` is
    /// the *root secret*; the `UserIdentity` carries only the
    /// public half + display metadata. Persist the `SigningKey`
    /// separately (encrypted at rest, ideally sealed to a TPM).
    pub fn generate(label: impl Into<String>, created_at_ns: u64) -> (ed25519_dalek::SigningKey, Self) {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let public_key = signing.verifying_key().to_bytes();
        (signing, Self::new(public_key, label, created_at_ns))
    }
}

/// A per-device Ed25519 keypair. Analogous to
/// `/var/lib/qos-fs/peer-key` today but scoped to the new identity
/// model.
///
/// Devices generate a fresh keypair on first boot. The public half
/// is embedded in the [`DeviceCert`] the trusted-device pool signs
/// during enrollment; the private half stays on-device and is used
/// to sign every CRDT change the device authors.
///
/// This struct only carries the public metadata; the secret half
/// is an `ed25519_dalek::SigningKey` held outside — same convention
/// as [`UserIdentity`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Ed25519 public key. 32 bytes.
    pub public_key: [u8; 32],
    /// Human-readable label. Appears in the workspace-directory
    /// UI ("Framework 16 laptop", "Studio phone", "Home server").
    pub label: String,
    /// Nanoseconds since the Unix epoch. Advisory.
    pub created_at_ns: u64,
}

impl DeviceIdentity {
    /// Generate a fresh device keypair.
    pub fn generate(
        label: impl Into<String>,
        created_at_ns: u64,
    ) -> (ed25519_dalek::SigningKey, Self) {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let public_key = signing.verifying_key().to_bytes();
        (
            signing,
            Self {
                public_key,
                label: label.into(),
                created_at_ns,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signing-preimage byte constants are wire ABI. If any of
    /// these three byte sequences change, previously-issued
    /// certificates and revocations stop verifying — that would
    /// silently disconnect every device in the field. Freeze them.
    #[test]
    fn signing_preimage_is_frozen_abi() {
        assert_eq!(cert::DEVCERT_PREIMAGE_TAG, b"qos-devcert-v1\0");
        assert_eq!(cert::DEVCERT_PREIMAGE_TAG.len(), 15);

        assert_eq!(revocation::REVOCATION_PREIMAGE_TAG, b"qos-revocation-v1\0");
        assert_eq!(revocation::REVOCATION_PREIMAGE_TAG.len(), 18);

        assert_eq!(enrollment::ENROLLMENT_PREIMAGE_TAG, b"qos-enrollment-v1\0");
        assert_eq!(enrollment::ENROLLMENT_PREIMAGE_TAG.len(), 18);
    }

    #[test]
    fn user_identity_generate_round_trip() {
        let (_sk, user) = UserIdentity::generate("Alice", 1_720_000_000_000_000_000);
        assert_eq!(user.label, "Alice");
        assert_eq!(user.created_at_ns, 1_720_000_000_000_000_000);
        assert_eq!(user.public_key.len(), 32);

        // Bytes round-trip through canonical postcard encoding.
        let bytes = canonical::canonical_bytes(&user).unwrap();
        let back: UserIdentity = canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(user, back);
    }

    #[test]
    fn device_identity_generate_round_trip() {
        let (_sk, dev) = DeviceIdentity::generate("Framework 16", 1_720_000_000_000_000_000);
        assert_eq!(dev.label, "Framework 16");
        let bytes = canonical::canonical_bytes(&dev).unwrap();
        let back: DeviceIdentity = canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(dev, back);
    }
}
