//! Who saved a shortcut, proven.
//!
//! A shortcut's `saved_by` is authority: its triggers fire on that person's
//! behalf and its unattended runs use the consents they gave in advance. A
//! field anyone writing the room could set is no such thing, and the room's
//! governor merges and relays what members write, so neither the sender of a
//! batch nor an automerge actor id says who wrote a shortcut. The saver's
//! device signs the shortcut instead ([`sign_saved_shortcut`]), over the
//! room it is saved in, and every reader asks [`proven_saver`]: admission for
//! each shortcut a batch adds or changes, whoever sends it, and trigger
//! arming each time it arms, against the room's revocations then.

use automerge::AutoCommit;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _};
use tilo_automation::{SavedProof, Shortcut};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Sign `shortcut` as saved in `room` by the person `device` is certified to,
/// with that device's `key`: sets `saved_by` to the person and attaches the
/// proof.
///
/// # Errors
///
/// `key` is not the certified device's, or the certificate does not encode.
pub fn sign_saved_shortcut(
    room: &str,
    shortcut: &mut Shortcut,
    device: &DeviceCert,
    key: &SigningKey,
) -> Result<(), String> {
    if key.verifying_key().to_bytes() != device.device_public_key {
        return Err("the signing key is not the certified device's".into());
    }
    shortcut.saved_by = Some(hex::encode(device.user_public_key));
    shortcut.saved_proof = None;
    let signature = key.sign(&shortcut.saved_signing_digest(room));
    shortcut.saved_proof = Some(SavedProof {
        device_cert: serde_json::to_value(device).map_err(|error| error.to_string())?,
        signature: hex::encode(signature.to_bytes()),
    });
    Ok(())
}

/// The person who saved `shortcut` in `room`, when its proof shows it at
/// `now_ns`: a certificate that chains to that person's root and is current,
/// a device the room has not revoked, and that device's signature over the
/// shortcut as saved in this room. `None` for a shortcut nobody is named as
/// saving, one without a proof (saved before proofs existed), and any proof
/// that does not hold.
#[must_use]
pub fn proven_saver(
    room: &str,
    shortcut: &Shortcut,
    revocations: &RevocationList,
    now_ns: u64,
) -> Option<String> {
    let saver = shortcut.saved_by.as_deref()?;
    let proof = shortcut.saved_proof.as_ref()?;
    let device: DeviceCert = serde_json::from_value(proof.device_cert.clone()).ok()?;
    if hex::encode(device.user_public_key) != saver {
        return None;
    }
    tilo_user_identity::verify_device_cert_at(&device, now_ns).ok()?;
    if revocations.is_revoked(&device.device_public_key) {
        return None;
    }
    let signature: [u8; 64] = hex::decode(&proof.signature).ok()?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&device.device_public_key)
        .ok()?
        .verify(
            &shortcut.saved_signing_digest(room),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .ok()?;
    Some(saver.to_owned())
}

/// Every device revocation the room `doc` records, each verified under the
/// root of the identity row that carries it. A row or entry that does not
/// verify adds nothing.
#[must_use]
pub fn room_revocations(doc: &AutoCommit) -> RevocationList {
    let mut revocations = RevocationList::new();
    for bytes in super::governance_of(Some(doc)).identities.values() {
        let Ok(row) = super::identity_row(bytes) else {
            continue;
        };
        for entry in row.revocations {
            if entry.user_public_key == row.device_cert.user_public_key {
                let _ = revocations.insert(entry);
            }
        }
    }
    revocations
}

/// Nanoseconds since the Unix epoch at `secs` seconds.
#[cfg(test)]
pub(crate) const fn ns_at(secs: u64) -> u64 {
    secs.saturating_mul(1_000_000_000)
}

#[cfg(test)]
#[path = "saved_shortcut_tests.rs"]
pub(crate) mod tests;
