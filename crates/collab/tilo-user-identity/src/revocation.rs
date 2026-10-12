//! Device revocation — how a user kills a lost or compromised
//! device.
//!
//! A [`RevocationEntry`] is a root-signed statement:
//!
//! > "The device with public key `revoked_device_public_key` should
//! > no longer be trusted as part of my fleet, effective now."
//!
//! Peers holding a valid revocation entry MUST reject every signature
//! that device key makes from then on, whatever time the signature claims:
//! a stolen key can back-date what it signs. `revoked_at_ns` records when
//! the person revoked it, for display and audit. The
//! revocation list lives in the user's directory CRDT (see
//! [`crate::WorkspaceDirectory`]) so it propagates to every device
//! that syncs with the user, and once acknowledged cannot be
//! "un-revoked" — replacing a revoked device requires enrolling a
//! new one under a fresh device key.
//!
//! The signing preimage constant [`REVOCATION_PREIMAGE_TAG`] is
//! frozen wire ABI.

use std::collections::HashMap;

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for `RevocationEntry`. Frozen wire ABI:
/// `tilo-revocation-v1` + NUL.
pub const REVOCATION_PREIMAGE_TAG: &[u8] = b"tilo-revocation-v1\0";

/// Why a device was revoked. Advisory — peers reject the device
/// regardless of reason. The value is surfaced in UI ("phone was
/// stolen 2026-06-01") and in audit logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevocationReason {
    /// Device was misplaced but may still be recoverable. Treated
    /// the same as `Stolen` for security purposes.
    Lost,
    /// Device is known to be in someone else's hands.
    Stolen,
    /// Device is being retired (sold, recycled, replaced).
    Retired,
    /// Device's private key is believed to have been extracted or
    /// otherwise leaked.
    Compromised,
}

/// Root-authored revocation entry. Kills a device by public key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationEntry {
    /// User root public key that issued the revocation. 32 bytes.
    /// Bundled in the entry itself for the same reason a
    /// `DeviceCert` bundles its `user_public_key`: peers can verify
    /// the signature without external key lookup.
    pub user_public_key: [u8; 32],
    /// The device public key being revoked. 32 bytes.
    pub revoked_device_public_key: [u8; 32],
    /// When the person revoked the device, for display and audit. The
    /// revocation applies to everything the key signs, whatever time it
    /// claims.
    pub revoked_at_ns: u64,
    /// Why the device was revoked. Advisory.
    pub reason: RevocationReason,
    /// Ed25519 signature covering everything else.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Signing view — everything but the signature.
#[derive(Serialize)]
struct RevocationView<'a> {
    user_public_key: &'a [u8; 32],
    revoked_device_public_key: &'a [u8; 32],
    revoked_at_ns: u64,
    reason: RevocationReason,
}

impl RevocationEntry {
    fn view(&self) -> RevocationView<'_> {
        RevocationView {
            user_public_key: &self.user_public_key,
            revoked_device_public_key: &self.revoked_device_public_key,
            revoked_at_ns: self.revoked_at_ns,
            reason: self.reason,
        }
    }
}

/// Failure modes for revocation-entry signing/verification.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RevocationError {
    /// The user root public key bytes do not decode to a valid
    /// Ed25519 curve point.
    #[error("invalid user root public key")]
    InvalidUserPublicKey,
    /// The signature did not verify.
    #[error("revocation signature did not verify")]
    BadSignature,
    /// Canonical-encoding failure.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Canonical digest a user identity signs to authorise this revocation.
///
/// # Errors
///
/// Returns [`RevocationError::Canonical`] if `entry` cannot be
/// postcard-encoded (effectively unreachable for this crate's types).
pub fn revocation_signing_digest(entry: &RevocationEntry) -> Result<[u8; 32], RevocationError> {
    signing_digest(REVOCATION_PREIMAGE_TAG, &entry.view()).map_err(Into::into)
}

/// Sign a revocation entry with the user root signing key. Fills
/// `signature` in place.
///
/// # Errors
///
/// Returns [`RevocationError::BadSignature`] if `entry.user_public_key`
/// does not match the public key derived from `root_signing_key`, or
/// [`RevocationError::Canonical`] if the entry cannot be canonically
/// encoded (effectively unreachable).
pub fn sign_revocation_entry(
    entry: &mut RevocationEntry,
    root_signing_key: &ed25519_dalek::SigningKey,
) -> Result<(), RevocationError> {
    let derived = root_signing_key.verifying_key().to_bytes();
    if entry.user_public_key != derived {
        return Err(RevocationError::BadSignature);
    }
    let digest = revocation_signing_digest(entry)?;
    let sig = root_signing_key.sign(&digest);
    entry.signature = sig.to_bytes();
    Ok(())
}

/// Verify a revocation entry's signature.
///
/// # Errors
///
/// Returns [`RevocationError::InvalidUserPublicKey`] if
/// `entry.user_public_key` is not a valid Ed25519 point, or
/// [`RevocationError::BadSignature`] if the signature does not verify.
pub fn verify_revocation_entry(entry: &RevocationEntry) -> Result<(), RevocationError> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&entry.user_public_key)
        .map_err(|_| RevocationError::InvalidUserPublicKey)?;
    let digest = revocation_signing_digest(entry)?;
    let sig = ed25519_dalek::Signature::from_bytes(&entry.signature);
    vk.verify(&digest, &sig)
        .map_err(|_| RevocationError::BadSignature)?;
    Ok(())
}

/// In-memory, verified revocation list for one user identity.
///
/// Populated by merging every [`RevocationEntry`] the user's
/// directory holds. On insertion we (a) verify the signature and
/// (b) keep only the *earliest* `revoked_at_ns` for a given device
/// key — a compromised device could try to publish a later
/// revocation to "reset" its cut-off, but only the user root can
/// sign, so this collapses to "the user chose the earliest".
///
/// Entries are kept per issuing identity: a list may hold several people's
/// revocations, and one identity's entry for a key never replaces another
/// identity's entry for the same key.
#[derive(Clone, Debug, Default)]
pub struct RevocationList {
    entries: HashMap<([u8; 32], [u8; 32]), RevocationEntry>,
}

impl RevocationList {
    /// Fresh empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify + insert an entry. Returns `Ok(true)` if the entry
    /// changed the list (new device or earlier timestamp),
    /// `Ok(false)` if it was a no-op (duplicate).
    ///
    /// Wrong-user signatures return `Err(BadSignature)` — callers
    /// should log + drop, not propagate.
    ///
    /// # Errors
    ///
    /// Returns anything [`verify_revocation_entry`] can return if
    /// `entry`'s signature does not verify.
    pub fn insert(&mut self, entry: RevocationEntry) -> Result<bool, RevocationError> {
        verify_revocation_entry(&entry)?;
        let key = (entry.user_public_key, entry.revoked_device_public_key);
        match self.entries.get(&key) {
            Some(existing) if existing.revoked_at_ns <= entry.revoked_at_ns => Ok(false),
            _ => {
                self.entries.insert(key, entry);
                Ok(true)
            }
        }
    }

    /// True if any identity in the list revoked `device_public_key`. A
    /// conservative question for callers holding one identity's list; a
    /// verifier that knows whose device it checks asks
    /// [`Self::revocation_of`].
    pub fn is_revoked(&self, device_public_key: &[u8; 32]) -> bool {
        self.entries
            .keys()
            .any(|(_, device)| device == device_public_key)
    }

    /// The revocation `user_public_key` signed for `device_public_key`, if
    /// any. A revocation is permanent: it holds for every signature the key
    /// makes, whatever time the signer claims.
    pub fn revocation_of(
        &self,
        user_public_key: &[u8; 32],
        device_public_key: &[u8; 32],
    ) -> Option<&RevocationEntry> {
        self.entries.get(&(*user_public_key, *device_public_key))
    }

    /// Iterate over the revocation entries currently held.
    pub fn iter(&self) -> impl Iterator<Item = &RevocationEntry> {
        self.entries.values()
    }

    /// Number of revocations in the list, one per identity and device.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no devices have been revoked.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserIdentity;

    fn fresh_entry(reason: RevocationReason) -> (ed25519_dalek::SigningKey, RevocationEntry) {
        let (root_sk, user) = UserIdentity::generate("Alice", 1);
        let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);
        let mut entry = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: dev.public_key,
            revoked_at_ns: 1_000,
            reason,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut entry, &root_sk).unwrap();
        (root_sk, entry)
    }

    /// Revocations sign and verify; a tampered device key or reason fails; a wrong root fails the
    /// pre-sign guard.
    #[test]
    fn revocations_verify_only_untampered_and_from_the_root() {
        // Phase: sign_and_verify.
        {
            let (_root, entry) = fresh_entry(RevocationReason::Lost);
            verify_revocation_entry(&entry).unwrap();
        }

        // Phase: tampered_device_pubkey_fails.
        {
            let (_root, mut entry) = fresh_entry(RevocationReason::Stolen);
            entry.revoked_device_public_key[0] ^= 0x01;
            assert_eq!(
                verify_revocation_entry(&entry).unwrap_err(),
                RevocationError::BadSignature
            );
        }

        // Phase: tampered_reason_fails.
        {
            let (_root, mut entry) = fresh_entry(RevocationReason::Lost);
            entry.reason = RevocationReason::Retired;
            assert_eq!(
                verify_revocation_entry(&entry).unwrap_err(),
                RevocationError::BadSignature
            );
        }

        // Phase: signed_by_wrong_root_fails_pre_sign_guard.
        {
            let (_, alice) = UserIdentity::generate("Alice", 1);
            let (bob_sk, _) = UserIdentity::generate("Bob", 2);
            let mut entry = RevocationEntry {
                user_public_key: alice.public_key,
                revoked_device_public_key: [7u8; 32],
                revoked_at_ns: 100,
                reason: RevocationReason::Retired,
                signature: [0u8; 64],
            };
            assert_eq!(
                sign_revocation_entry(&mut entry, &bob_sk).unwrap_err(),
                RevocationError::BadSignature
            );
        }
    }

    /// Lists dedup with the earliest revocation winning and reject a bad signature.
    #[test]
    fn revocation_lists_dedup_and_reject_bad_signatures() {
        // Phase: list_dedups_earliest_wins.
        {
            let (root_sk, _) = UserIdentity::generate("Alice", 1);
            let user_pk = root_sk.verifying_key().to_bytes();
            let device_pk = [42u8; 32];

            let sign = |at: u64| {
                let mut e = RevocationEntry {
                    user_public_key: user_pk,
                    revoked_device_public_key: device_pk,
                    revoked_at_ns: at,
                    reason: RevocationReason::Lost,
                    signature: [0u8; 64],
                };
                sign_revocation_entry(&mut e, &root_sk).unwrap();
                e
            };

            let mut list = RevocationList::new();
            assert!(list.insert(sign(200)).unwrap());
            // Earlier timestamp arrives second — should overwrite the
            // later one so the "cut-off" is the earlier moment.
            assert!(list.insert(sign(100)).unwrap());
            // Later duplicate is a no-op.
            assert!(!list.insert(sign(300)).unwrap());

            assert!(list.is_revoked(&device_pk));
            assert_eq!(
                list.revocation_of(&user_pk, &device_pk)
                    .map(|entry| entry.revoked_at_ns),
                Some(100)
            );
            assert!(list.revocation_of(&[0; 32], &device_pk).is_none());
        }

        // Phase: list_rejects_bad_signature.
        {
            let (_root, mut entry) = fresh_entry(RevocationReason::Compromised);
            entry.signature[0] ^= 0x01;
            let mut list = RevocationList::new();
            assert_eq!(
                list.insert(entry).unwrap_err(),
                RevocationError::BadSignature
            );
            assert!(list.is_empty());
        }
    }
}
