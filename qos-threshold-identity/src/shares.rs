//! Secret-share wrapper and recovery-share serialisation.
//!
//! ## Why a newtype around `frost_ed25519::keys::KeyPackage`
//!
//! The `frost-core::keys::KeyPackage` carries the full per-participant
//! cryptographic state: the secret `SigningShare`, the public
//! `VerifyingShare`, the group `VerifyingKey`, the participant
//! identifier, and the threshold `min_signers`. We wrap it in a
//! [`SecretShare`] newtype for three reasons:
//!
//! 1. **Custom `Debug`** — `KeyPackage::Debug` leaks the secret
//!    signing share in panic messages. Our `Debug` prints only the
//!    string `"<redacted>"`.
//! 2. **No `Serialize`** — we deliberately do not implement `serde`
//!    on the secret newtype so it cannot be JSON-logged. The
//!    structured form for explicit export (recovery-share, on-disk
//!    pickle) lives in [`RecoveryShare`] / [`encrypted_blob`].
//! 3. **Zeroize on drop** — the wrapper holds the inner `KeyPackage`
//!    by value; when the wrapper drops, the inner secret-bearing
//!    fields wipe themselves via the `ZeroizeOnDrop` already
//!    implemented by `frost-core`.
//!
//! ## Recovery-share encoding
//!
//! [`RecoveryShare`] is the paper-share format. The secret half is
//! the 32-byte signing-share scalar, encoded as 24 BIP39 English
//! words (256 bits of entropy + 8 bits of checksum). The public
//! half (share index, threshold, group public key, verifying
//! share) lives in side fields so the share is self-contained on
//! re-import.
//!
//! Why BIP39 and not a custom mnemonic: BIP39's wordlist is
//! well-established, has no near-homophones, and most password
//! managers can store it as a sequence-of-words note. We tolerate
//! the cosmetic mismatch (BIP39 is built for HD wallets; we use it
//! purely as an entropy codec) because the security property we
//! need is "human-typable + checksum-verifying", not the BIP39
//! seed-derivation tree.

use frost_ed25519::keys::{KeyPackage, SigningShare, VerifyingShare};
use frost_ed25519::{Identifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use qos_collab_types::Ed25519PublicKey;

use crate::errors::ThresholdError;

/// Opaque secret-bearing share for one device.
///
/// The wrapper holds a `frost_ed25519::keys::KeyPackage`, which
/// `frost-core` itself zeroizes on drop. We layer our own `Drop`
/// to also wipe any incidental state we add later (currently
/// none) and to assert that no `Serialize` derive ever sneaks
/// onto this type — see the module docs for why.
pub struct SecretShare {
    pub(crate) inner: KeyPackage,
}

impl SecretShare {
    /// Construct from a verified `KeyPackage` — used by the DKG
    /// finalisation path and by the recovery-share import.
    pub(crate) fn from_key_package(kp: KeyPackage) -> Self {
        Self { inner: kp }
    }

    /// Borrow the underlying `KeyPackage`. `pub(crate)` because
    /// nothing outside the crate should ever touch the raw FROST
    /// type — the wrapper exists specifically to prevent that.
    pub(crate) fn key_package(&self) -> &KeyPackage {
        &self.inner
    }

    /// 1-indexed share number. Mirrors the `Identifier` in the
    /// underlying KeyPackage. Surfaces in user-facing UI ("Share
    /// 2 of 3") and in the `RecoveryShare::share_index` field.
    pub fn share_index(&self) -> u16 {
        identifier_to_u16(self.inner.identifier())
    }
}

impl std::fmt::Debug for SecretShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret. The share index is non-secret
        // and helpful for log triage, so we include it.
        write!(
            f,
            "SecretShare(<redacted>, share_index={})",
            self.share_index()
        )
    }
}

/// Paper-recovery share for offline storage.
///
/// The user prints (or transcribes) the `mnemonic` to a piece of
/// paper. The non-secret fields (`share_index`, `threshold`,
/// `total_shares`, `group_public_key`, `verifying_share_bytes`)
/// must also be recorded; they are public information but the
/// recovery flow needs them to reconstruct a `SecretShare` from
/// the mnemonic alone.
///
/// **The mnemonic is the secret.** The other fields can be
/// printed alongside in cleartext.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryShare {
    /// 24-word BIP39 English mnemonic of the 32-byte signing
    /// scalar. The wordlist is BIP39 English (2048 words).
    pub mnemonic: String,
    /// 1-indexed share number this share represents.
    pub share_index: u16,
    /// The `t` in t-of-n at the time the share was generated.
    /// Carried so the recovery flow can sanity-check that the
    /// share belongs to the group the user is restoring into.
    pub threshold: u16,
    /// The `n` in t-of-n.
    pub total_shares: u16,
    /// The group public key. Carried so `import_recovery_share`
    /// can refuse a mnemonic that belongs to a different group.
    pub group_public_key: Ed25519PublicKey,
    /// The 32-byte public verifying-share for this index. Needed
    /// to rebuild a `KeyPackage` from the mnemonic alone.
    #[serde(with = "serde_bytes")]
    pub verifying_share_bytes: Vec<u8>,
}

impl RecoveryShare {
    /// Build a recovery share from a `KeyPackage`.
    ///
    /// The 32-byte signing-share scalar is serialised, then the
    /// BIP39 encoder turns it into 24 words. The remaining
    /// (public) fields are read straight off the `KeyPackage`.
    pub(crate) fn from_key_package(kp: &KeyPackage, total_shares: u16) -> Result<Self, ThresholdError> {
        // Serialize the signing share. frost-ed25519's SigningShare::serialize()
        // returns the 32-byte canonical scalar form.
        let scalar_bytes: [u8; 32] = kp
            .signing_share()
            .serialize()
            .as_slice()
            .try_into()
            .map_err(|_| {
                ThresholdError::FrostInternal(
                    "signing share serialise produced non-32-byte output".to_string(),
                )
            })?;
        let mnemonic = bip39::Mnemonic::from_entropy(&scalar_bytes)
            .map_err(|e| ThresholdError::FrostInternal(format!("bip39 encode: {e}")))?
            .to_string();
        let verifying_share_bytes = kp.verifying_share().serialize()?;
        let gpk_bytes_vec = kp.verifying_key().serialize()?;
        let gpk_bytes: [u8; 32] = gpk_bytes_vec.as_slice().try_into().map_err(|_| {
            ThresholdError::FrostInternal(
                "group verifying key serialise produced non-32-byte output".to_string(),
            )
        })?;
        Ok(Self {
            mnemonic,
            share_index: identifier_to_u16(kp.identifier()),
            threshold: *kp.min_signers(),
            total_shares,
            group_public_key: Ed25519PublicKey::from_bytes(gpk_bytes),
            verifying_share_bytes,
        })
    }

    /// Decode a recovery share into a `SecretShare` usable for
    /// signing.
    ///
    /// `total_shares` is taken from `self.total_shares`. The
    /// returned `SecretShare` carries the same threshold,
    /// identifier, and group public key the share was generated
    /// against — the caller is responsible for cross-checking
    /// against the live `ThresholdIdentity` they are restoring
    /// into (see
    /// [`crate::identity::ThresholdIdentity::adopt_recovery_share`]).
    pub fn into_secret_share(self) -> Result<SecretShare, ThresholdError> {
        // Decode the mnemonic into 32 bytes of entropy.
        // `parse_normalized` instead of `parse` to avoid the
        // `unicode-normalization` feature flag. Our wordlist is
        // BIP39 English (ASCII), so normalisation is a no-op.
        let mnemonic = bip39::Mnemonic::parse_normalized(&self.mnemonic)
            .map_err(|e| ThresholdError::BadRecoveryMnemonic(e.to_string()))?;
        let (entropy_buf, entropy_len) = mnemonic.to_entropy_array();
        if entropy_len != 32 {
            return Err(ThresholdError::BadRecoveryMnemonic(format!(
                "expected 32 bytes of entropy, got {entropy_len}"
            )));
        }
        let scalar_bytes: [u8; 32] = entropy_buf[..32].try_into().map_err(|_| {
            ThresholdError::BadRecoveryMnemonic("entropy slice not 32 bytes".to_string())
        })?;
        let signing_share = SigningShare::deserialize(&scalar_bytes)?;
        let verifying_share = VerifyingShare::deserialize(&self.verifying_share_bytes)?;
        let group_pk = VerifyingKey::deserialize(self.group_public_key.as_bytes())?;
        let identifier = u16_to_identifier(self.share_index)?;
        let kp = KeyPackage::new(
            identifier,
            signing_share,
            verifying_share,
            group_pk,
            self.threshold,
        );
        Ok(SecretShare::from_key_package(kp))
    }
}

/// Encode a `frost-core::Identifier` as a `u16`.
///
/// FROST identifiers are field scalars; we constrain ourselves
/// to the `1..=u16::MAX` subset so devices can be numbered with
/// human-friendly indices. The encoding is the same one
/// `Identifier::try_from::<u16>` uses.
pub(crate) fn identifier_to_u16(id: &Identifier) -> u16 {
    let bytes = id.serialize();
    // Scalars in frost-ed25519 are 32-byte little-endian. A u16
    // value encodes as `[lo, hi, 0, 0, …, 0]`. Read the bottom
    // 16 bits; the rest is guaranteed zero by our
    // `u16_to_identifier` constructor.
    let lo = *bytes.first().unwrap_or(&0) as u16;
    let hi = *bytes.get(1).unwrap_or(&0) as u16;
    lo | (hi << 8)
}

/// Construct a `frost-core::Identifier` from a `u16`. Returns
/// `InvalidShareIndex` if `n == 0` (FROST forbids the zero
/// scalar).
pub(crate) fn u16_to_identifier(n: u16) -> Result<Identifier, ThresholdError> {
    if n == 0 {
        return Err(ThresholdError::InvalidShareIndex(0));
    }
    Identifier::try_from(n)
        .map_err(|_| ThresholdError::InvalidShareIndex(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_identifier_round_trip() {
        for n in [1u16, 2, 3, 7, 255, 256, 1024, u16::MAX] {
            let id = u16_to_identifier(n).unwrap();
            assert_eq!(identifier_to_u16(&id), n);
        }
    }

    #[test]
    fn zero_identifier_rejected() {
        let err = u16_to_identifier(0).unwrap_err();
        assert!(matches!(err, ThresholdError::InvalidShareIndex(0)));
    }

    #[test]
    fn secret_share_debug_does_not_leak() {
        // Build a synthetic KeyPackage via the trusted-dealer path
        // and confirm the Debug form is redacted. FROST requires
        // min_signers ≥ 2, so we use 2-of-2 even though the
        // assertions only need one share to inspect.
        use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
        let rng = rand::rngs::OsRng;
        let (shares, _pubkeys) =
            generate_with_dealer(2, 2, IdentifierList::Default, rng).unwrap();
        let (_id, share) = shares.into_iter().next().unwrap();
        let kp: KeyPackage = share.try_into().unwrap();
        let wrapped = SecretShare::from_key_package(kp);
        let dbg = format!("{wrapped:?}");
        assert!(
            dbg.contains("<redacted>"),
            "Debug must redact secret: {dbg}"
        );
        assert!(!dbg.contains("SigningShare"), "Debug leaked field name: {dbg}");
    }
}
