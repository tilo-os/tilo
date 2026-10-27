//! Governor succession — who governs in place of a revoked device.
//!
//! A room a device governs is governed by that one device: it signs the
//! room's governance and stewards its encrypted channels. Revoking it must
//! not leave the room frozen, and must not leave it to whichever of the
//! person's devices answers first. A [`GovernorSuccession`] is the root's
//! statement:
//!
//! > "My device `previous` is revoked (`revocation`); my device `successor`
//! > governs every room `previous` governed."
//!
//! It names no room: one statement moves every room the revoked device
//! governed, wherever it is heard. Peers that pinned `previous` as a room's
//! governor follow it to `successor` ([`current_governor`]) and refuse
//! `previous` from then on, as its revocation already says.
//!
//! The signing preimage constant [`SUCCESSION_PREIMAGE_TAG`] is frozen wire
//! ABI.

use std::collections::BTreeSet;

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::{DeviceCert, RevocationEntry, verify_device_cert, verify_device_cert_at};

/// Signing-domain tag for [`GovernorSuccession`]. Frozen wire ABI.
pub const SUCCESSION_PREIMAGE_TAG: &[u8] = b"tilo-governor-succession-v1\0";

/// Longest chain of successions [`current_governor`] follows.
pub const MAX_SUCCESSIONS: usize = 16;

/// The root's statement that `successor` governs in place of the revoked
/// `previous`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernorSuccession {
    /// The revoked device's certificate: proves it was the root's.
    pub previous: DeviceCert,
    /// The root's revocation of `previous`.
    pub revocation: RevocationEntry,
    /// The device that governs in its place.
    pub successor: DeviceCert,
    /// The root's Ed25519 signature over everything else.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct SuccessionView<'a> {
    previous: &'a DeviceCert,
    revocation: &'a RevocationEntry,
    successor: &'a DeviceCert,
}

/// Why a succession is refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SuccessionError {
    /// A certificate or the revocation does not verify.
    #[error("succession evidence does not verify: {0}")]
    Evidence(String),
    /// The certificates and the revocation are not all one root's, about
    /// the same revoked device.
    #[error("succession mixes devices or roots")]
    Mismatch,
    /// The successor may not govern: it is the revoked device, a foreign
    /// device, or one not certified to own workspaces.
    #[error("successor may not govern")]
    Successor,
    /// The root's signature does not verify.
    #[error("succession signature did not verify")]
    BadSignature,
    /// Canonical-encoding failure.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

impl GovernorSuccession {
    /// Canonical digest the root signs to make this statement.
    ///
    /// # Errors
    ///
    /// The statement cannot be canonically encoded (effectively unreachable).
    pub fn signing_digest(&self) -> Result<[u8; 32], SuccessionError> {
        let view = SuccessionView {
            previous: &self.previous,
            revocation: &self.revocation,
            successor: &self.successor,
        };
        Ok(signing_digest(SUCCESSION_PREIMAGE_TAG, &view)?)
    }

    /// The root statement that `successor` governs in place of `previous`,
    /// which `revocation` revokes, signed with the root key.
    ///
    /// # Errors
    ///
    /// The statement would not verify ([`Self::verify_at`]) at the
    /// successor's issue time.
    pub fn sign(
        previous: DeviceCert,
        revocation: RevocationEntry,
        successor: DeviceCert,
        root: &ed25519_dalek::SigningKey,
    ) -> Result<Self, SuccessionError> {
        let mut succession = Self {
            previous,
            revocation,
            successor,
            signature: [0; 64],
        };
        succession.signature = root.sign(&succession.signing_digest()?).to_bytes();
        succession.verify_at(succession.successor.issued_at_ns)?;
        Ok(succession)
    }

    /// Check that one root revoked `previous` and named `successor`, a
    /// current, native device of theirs certified to own workspaces, to
    /// govern in its place. `previous` is checked by signature only: an
    /// expired governor still needs a successor.
    pub fn verify_at(&self, now_ns: u64) -> Result<(), SuccessionError> {
        let evidence = |error: &dyn std::fmt::Display| SuccessionError::Evidence(error.to_string());
        verify_device_cert(&self.previous).map_err(|error| evidence(&error))?;
        verify_device_cert_at(&self.successor, now_ns).map_err(|error| evidence(&error))?;
        crate::verify_revocation_entry(&self.revocation).map_err(|error| evidence(&error))?;
        let root = self.previous.user_public_key;
        if self.successor.user_public_key != root
            || self.revocation.user_public_key != root
            || self.revocation.revoked_device_public_key != self.previous.device_public_key
        {
            return Err(SuccessionError::Mismatch);
        }
        let capabilities = &self.successor.capabilities;
        if self.successor.device_public_key == self.previous.device_public_key
            || !capabilities.can_own_workspaces
            || capabilities.foreign
        {
            return Err(SuccessionError::Successor);
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&root)
            .map_err(|_| SuccessionError::BadSignature)?;
        key.verify(
            &self.signing_digest()?,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| SuccessionError::BadSignature)
    }
}

/// The device governing in place of `governor`, following verified
/// `successions` from it; `governor` itself when none names it. Where one
/// root named two successors for the same device, the lowest successor key
/// governs, so every peer holding the same statements agrees. A chain stops
/// at a device seen before and after [`MAX_SUCCESSIONS`] steps.
#[must_use]
pub fn current_governor(successions: &[GovernorSuccession], governor: [u8; 32]) -> [u8; 32] {
    let mut current = governor;
    let mut seen = BTreeSet::from([governor]);
    for _ in 0..MAX_SUCCESSIONS {
        let next = successions
            .iter()
            .filter(|succession| succession.previous.device_public_key == current)
            .map(|succession| succession.successor.device_public_key)
            .min();
        match next {
            Some(next) if seen.insert(next) => current = next,
            _ => break,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceCapabilities, RevocationReason, sign_device_cert, sign_revocation_entry};

    fn device(root: &ed25519_dalek::SigningKey, seed: u8, caps: DeviceCapabilities) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
            device_label: format!("device {seed}"),
            capabilities: caps,
            issued_at_ns: 10,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn revoke(root: &ed25519_dalek::SigningKey, cert: &DeviceCert) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: cert.device_public_key,
            revoked_at_ns: 20,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn the_succession_digest_is_pinned() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let previous = device(&root, 1, DeviceCapabilities::primary());
        let successor = device(&root, 2, DeviceCapabilities::primary());
        let revocation = revoke(&root, &previous);
        let succession = GovernorSuccession {
            previous,
            revocation,
            successor,
            signature: [0; 64],
        };
        assert_eq!(
            hex::encode(succession.signing_digest().unwrap()),
            "38826bbed5d149421075229e224c46fc35bb35794a585ae26c9e8d8304d7413c"
        );
    }

    #[test]
    fn only_the_root_names_a_native_owner_device_to_succeed_its_revoked_one() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let laptop = device(&root, 1, DeviceCapabilities::primary());
        let phone = device(&root, 2, DeviceCapabilities::primary());
        let revocation = revoke(&root, &laptop);

        let succession =
            GovernorSuccession::sign(laptop.clone(), revocation.clone(), phone.clone(), &root)
                .unwrap();
        succession.verify_at(30).unwrap();
        assert_eq!(
            current_governor(std::slice::from_ref(&succession), laptop.device_public_key),
            phone.device_public_key
        );
        assert_eq!(
            current_governor(std::slice::from_ref(&succession), phone.device_public_key),
            phone.device_public_key
        );

        // Another root cannot name a successor, nor be named one.
        assert!(
            GovernorSuccession::sign(laptop.clone(), revocation.clone(), phone.clone(), &other)
                .is_err()
        );
        let stranger = device(&other, 3, DeviceCapabilities::primary());
        assert_eq!(
            GovernorSuccession::sign(laptop.clone(), revocation.clone(), stranger, &root),
            Err(SuccessionError::Mismatch)
        );
        // A device that may not own workspaces does not govern them.
        let server = device(&root, 4, DeviceCapabilities::host());
        assert_eq!(
            GovernorSuccession::sign(laptop.clone(), revocation, server, &root),
            Err(SuccessionError::Successor)
        );
        assert_eq!(
            GovernorSuccession::sign(laptop, revoke(&root, &phone), phone, &root),
            Err(SuccessionError::Mismatch)
        );
        let mut forged = succession;
        forged.successor = device(&root, 5, DeviceCapabilities::primary());
        assert_eq!(forged.verify_at(30), Err(SuccessionError::BadSignature));
    }

    #[test]
    fn successions_chain_and_a_contested_one_resolves_the_same_everywhere() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let [a, b, c, d] =
            [1, 2, 3, 4].map(|seed| device(&root, seed, DeviceCapabilities::primary()));
        let a_to_b =
            GovernorSuccession::sign(a.clone(), revoke(&root, &a), b.clone(), &root).unwrap();
        let b_to_c =
            GovernorSuccession::sign(b.clone(), revoke(&root, &b), c.clone(), &root).unwrap();
        assert_eq!(
            current_governor(&[b_to_c, a_to_b.clone()], a.device_public_key),
            c.device_public_key
        );
        let a_to_d =
            GovernorSuccession::sign(a.clone(), revoke(&root, &a), d.clone(), &root).unwrap();
        let lowest = b.device_public_key.min(d.device_public_key);
        assert_eq!(
            current_governor(&[a_to_b.clone(), a_to_d.clone()], a.device_public_key),
            lowest
        );
        assert_eq!(
            current_governor(&[a_to_d, a_to_b.clone()], a.device_public_key),
            lowest
        );
        let b_to_a =
            GovernorSuccession::sign(b.clone(), revoke(&root, &b), a.clone(), &root).unwrap();
        assert_eq!(
            current_governor(&[a_to_b, b_to_a], a.device_public_key),
            b.device_public_key
        );
    }
}
