//! Device revocation — how a user kills a lost or compromised
//! device.
//!
//! A [`RevocationEntry`] is a root-signed statement:
//!
//! > "The device with public key `revoked_device_public_key` should
//! > no longer be trusted as part of my fleet, effective now."
//!
//! Peers holding a valid revocation entry MUST reject any signature
//! made by that device key, under a certificate the same root issued, on
//! or after `revoked_at_ns`. A root speaks only for the devices it
//! certified: its entry naming a key another root certified refuses
//! nothing of that other person's. The
//! revocation list lives in the user's directory CRDT (see
//! [`crate::WorkspaceDirectory`]) so it propagates to every device
//! that syncs with the user, and once acknowledged cannot be
//! "un-revoked" — replacing a revoked device requires enrolling a
//! new one under a fresh device key.
//!
//! The signing preimage constant [`REVOCATION_PREIMAGE_TAG`] is
//! frozen wire ABI.

use std::collections::HashMap;

use collab_types::signer::SignError;
use ed25519_dalek::Verifier;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::cert::DeviceCert;
use crate::sign::StatementSigner;

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
    /// When the revocation took effect. Peers reject the device's
    /// changes issued at or after this timestamp.
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
    /// The key that was asked to sign refused or failed.
    #[error("the key did not sign: {0}")]
    Signer(#[from] SignError),
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

/// Sign a revocation entry with the user's root. Fills
/// `signature` in place.
///
/// # Errors
///
/// Returns [`RevocationError::BadSignature`] if `entry.user_public_key`
/// does not match `root`'s public key, [`RevocationError::Signer`] when the
/// key refuses, or [`RevocationError::Canonical`] if the entry cannot be
/// canonically encoded (effectively unreachable).
pub fn sign_revocation_entry(
    entry: &mut RevocationEntry,
    root: &dyn StatementSigner,
) -> Result<(), RevocationError> {
    if entry.user_public_key != root.public_key() {
        return Err(RevocationError::BadSignature);
    }
    let digest = revocation_signing_digest(entry)?;
    entry.signature = root.sign_digest(&digest)?;
    Ok(())
}

/// [`sign_revocation_entry`] with a key that answers only later: a page's root
/// lives in `WebCrypto` custody and signs through a promise.
///
/// # Errors
///
/// As [`sign_revocation_entry`].
pub async fn sign_revocation_entry_async(
    entry: &mut RevocationEntry,
    root: &dyn collab_types::signer::DeviceSigner,
) -> Result<(), RevocationError> {
    if entry.user_public_key != root.device_public_key() {
        return Err(RevocationError::BadSignature);
    }
    let digest = revocation_signing_digest(entry)?;
    entry.signature = root.sign(digest.to_vec()).await?;
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

/// Whether revocations refuse `cert`, given when each key was first revoked
/// by a root (`revoked_at(root, key)`), asked only of the root that
/// certified the key: its device's revocation counts from its time when `at_ns`
/// names the moment asked about, and at any time otherwise; its stand-in
/// issuer's (see [`DeviceCert::issuer`]) counts when the stand-in was
/// revoked at or before it issued `cert`. The rule
/// [`RevocationList::refuses`] applies, for stores that keep revocations
/// their own way.
#[must_use]
pub fn cert_refused(
    cert: &DeviceCert,
    at_ns: Option<u64>,
    revoked_at: impl Fn(&[u8; 32], &[u8; 32]) -> Option<u64>,
) -> bool {
    let device = revoked_at(&cert.user_public_key, &cert.device_public_key)
        .is_some_and(|revoked| at_ns.is_none_or(|at| at >= revoked));
    // A stand-in vouched for the person's own devices: what it issued before
    // its revocation stays theirs. A grant's minted grants are the grant
    // itself, narrowed, and never outlive it.
    let issuer = cert.issuer.as_ref().is_some_and(|issuer| {
        revoked_at(&issuer.user_public_key, &issuer.device_public_key).is_some_and(|revoked| {
            issuer.capabilities.agent.is_some() || cert.issued_at_ns >= revoked
        })
    });
    device || issuer
}

/// In-memory, verified revocation list.
///
/// Populated by merging every [`RevocationEntry`] a user's directory,
/// a workspace or a host holds; one list may hold several people's.
/// Each entry is kept under the root that signed it and the device it
/// names, and is asked about only for that root: any root can sign an
/// entry naming any key, so another person's entry never reaches a
/// device this root certified. On insertion we (a) verify the
/// signature and (b) keep only the *earliest* `revoked_at_ns` per root
/// and device — a compromised device could try to publish a later
/// revocation to "reset" its cut-off, but only the user root can
/// sign, so this collapses to "the user chose the earliest".
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

    /// True if the root `user_public_key` has revoked its device
    /// `device_public_key` at any point.
    pub fn is_revoked(&self, user_public_key: &[u8; 32], device_public_key: &[u8; 32]) -> bool {
        self.revoked_at(user_public_key, device_public_key)
            .is_some()
    }

    /// True if the root `user_public_key` revoked its device
    /// `device_public_key` at or before `at_ns`. Callers validating a
    /// signed change use this to ask "was the signer already revoked
    /// when they made this change?"
    pub fn is_revoked_at(
        &self,
        user_public_key: &[u8; 32],
        device_public_key: &[u8; 32],
        at_ns: u64,
    ) -> bool {
        self.revoked_at(user_public_key, device_public_key)
            .is_some_and(|revoked| at_ns >= revoked)
    }

    /// Whether this list refuses `cert` at any time: its device was revoked,
    /// or it was issued by a person's stand-in (see [`DeviceCert::issuer`])
    /// at or after the stand-in was revoked. A revoked stand-in approves
    /// nothing from its revocation on, while every device it certified
    /// before stays valid. This is the one check of a certificate against
    /// revocations; prefer it to asking about its device key alone.
    #[must_use]
    pub fn refuses(&self, cert: &DeviceCert) -> bool {
        cert_refused(cert, None, |root, key| self.revoked_at(root, key))
    }

    /// [`Self::refuses`] with the device's own revocation counted only from
    /// its time: whether `cert` is refused for something its device signed
    /// at `at_ns`.
    #[must_use]
    pub fn refuses_at(&self, cert: &DeviceCert, at_ns: u64) -> bool {
        cert_refused(cert, Some(at_ns), |root, key| self.revoked_at(root, key))
    }

    /// When `root` revoked its device `key`, if it did.
    fn revoked_at(&self, root: &[u8; 32], key: &[u8; 32]) -> Option<u64> {
        self.entries
            .get(&(*root, *key))
            .map(|entry| entry.revoked_at_ns)
    }

    /// Iterate over the revocation entries currently held.
    pub fn iter(&self) -> impl Iterator<Item = &RevocationEntry> {
        self.entries.values()
    }

    /// Number of entries in the list: one per root and revoked device.
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

    /// What a root signs to revoke a device is fixed: hosts and the browser
    /// runtime's end-to-end checks (which sign one with a page's root)
    /// compute the same digest.
    #[test]
    fn the_revocation_digest_is_pinned() {
        let entry = RevocationEntry {
            user_public_key: [1; 32],
            revoked_device_public_key: [2; 32],
            revoked_at_ns: 1,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        assert_eq!(
            hex::encode(revocation_signing_digest(&entry).unwrap()),
            "51d3fa67bb622325b1fd36a8930f6ed801df50f5814546dab3c6f522e5bcca5a"
        );
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

            assert!(list.is_revoked(&user_pk, &device_pk));
            assert!(list.is_revoked_at(&user_pk, &device_pk, 100));
            assert!(list.is_revoked_at(&user_pk, &device_pk, 200));
            // Just before the cut-off — not revoked yet.
            assert!(!list.is_revoked_at(&user_pk, &device_pk, 99));
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

    /// Any root can sign an entry naming any key. Another person's entry
    /// naming Ada's device refuses nothing of hers and does not move the
    /// cut-off of her own root's entry; hers still refuses it.
    #[test]
    fn a_revocation_reaches_only_devices_its_own_root_certified() {
        let (ada_root, _) = UserIdentity::generate("Ada", 1);
        let (bob_root, _) = UserIdentity::generate("Bob", 1);
        let ada = ada_root.verifying_key().to_bytes();
        let bob = bob_root.verifying_key().to_bytes();
        let cert = crate::prepare_stand_in_cert(ada, [9; 32], "laptop", 10, 10_000);
        let entry = |root: &ed25519_dalek::SigningKey, at: u64| {
            let mut entry = RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: cert.device_public_key,
                revoked_at_ns: at,
                reason: RevocationReason::Stolen,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, root).unwrap();
            entry
        };

        let mut list = RevocationList::new();
        assert!(list.insert(entry(&bob_root, 20)).unwrap());
        assert!(list.is_revoked(&bob, &cert.device_public_key));
        assert!(!list.is_revoked(&ada, &cert.device_public_key));
        assert!(!list.refuses(&cert), "Bob's entry refuses nothing of Ada's");
        assert!(!list.refuses_at(&cert, 30));

        assert!(
            list.insert(entry(&ada_root, 50)).unwrap(),
            "Ada's own entry is kept beside Bob's"
        );
        assert_eq!(list.len(), 2);
        assert!(list.refuses(&cert));
        assert!(
            !list.refuses_at(&cert, 30),
            "her cut-off is her own, not Bob's earlier one"
        );
        assert!(list.refuses_at(&cert, 50));
    }
}
