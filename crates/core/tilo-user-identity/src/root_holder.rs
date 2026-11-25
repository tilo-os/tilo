//! The mark a person's root gives the device that keeps it: the root's signature over the
//! device's key. The person's devices record keeps it, and a change only that root may make
//! carries it, so whoever reads the change checks for themselves that its writer keeps the root.

use crate::DeviceCert;

/// What the person's root signs to mark the device that keeps it, beside that device's key.
const ROOT_HOLDER_DOMAIN: &[u8] = b"tilo-root-holder-v1\0";

/// The digest the person's root signs to mark `device` as the one that keeps it.
#[must_use]
pub fn root_holder_digest(device: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ROOT_HOLDER_DOMAIN);
    hasher.update(device);
    *hasher.finalize().as_bytes()
}

/// Whether `signature` is the root `cert` names signing [`root_holder_digest`] of the key `cert`
/// certifies: whether it marks that device as the one keeping its person's root.
#[must_use]
pub fn keeps_root(cert: &DeviceCert, signature: &[u8; 64]) -> bool {
    ed25519_dalek::VerifyingKey::from_bytes(&cert.user_public_key).is_ok_and(|root| {
        root.verify_strict(
            &root_holder_digest(&cert.device_public_key),
            &ed25519_dalek::Signature::from_bytes(signature),
        )
        .is_ok()
    })
}
