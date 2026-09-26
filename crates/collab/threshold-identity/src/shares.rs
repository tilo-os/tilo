//! Secret-share wrapper and recovery-share serialisation.
//!
//! `frost-core::keys::KeyPackage` carries the full per-participant
//! crypto state (secret `SigningShare`, public `VerifyingShare`, group
//! `VerifyingKey`, identifier, `min_signers`). [`SecretShare`] wraps it
//! for three reasons: (1) custom `Debug` — `KeyPackage::Debug` leaks the
//! secret signing share in panic messages, ours prints `"<redacted>"`;
//! (2) no `Serialize` — deliberately not implemented, so it can't be
//! JSON-logged (the explicit-export form lives in
//! [`RecoveryShare`]/`encrypted_blob`); (3) zeroize on drop, via
//! `frost-core`'s own `ZeroizeOnDrop` on the wrapped `KeyPackage`.
//!
//! [`RecoveryShare`] is the paper-share format: the secret half is the
//! 32-byte signing-share scalar, encoded as 24 BIP39 English words (256
//! bits entropy + 8 bits checksum); the public half (share index,
//! threshold, group public key, verifying share) lives in side fields
//! so the share is self-contained on re-import. BIP39 (not a custom
//! mnemonic) because its wordlist is well-established, has no
//! near-homophones, and most password managers store it as a
//! sequence-of-words note — used purely as an entropy codec, not for its
//! seed-derivation tree.

use frost_ed25519::keys::{KeyPackage, SigningShare, VerifyingShare};
use frost_ed25519::{Identifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_bytes as serde_bytes_codec;

use collab_types::Ed25519PublicKey;

use crate::errors::ThresholdError;

/// Opaque secret-bearing share for one device. Wraps a
/// `frost_ed25519::keys::KeyPackage` (zeroized on drop by `frost-core`
/// itself); our own `Drop` also wipes any future incidental state and
/// guards against a `Serialize` derive ever sneaking onto this type.
pub struct SecretShare {
    pub(crate) inner: KeyPackage,
}

impl SecretShare {
    /// Construct from a verified `KeyPackage` — used by DKG finalisation and
    /// recovery-share import.
    pub(crate) fn from_key_package(kp: KeyPackage) -> Self {
        Self { inner: kp }
    }

    /// Borrow the underlying `KeyPackage`. `pub(crate)` — nothing outside
    /// the crate should touch the raw FROST type; the wrapper exists to prevent that.
    pub(crate) fn key_package(&self) -> &KeyPackage {
        &self.inner
    }

    /// 1-indexed share number, mirroring the underlying `Identifier`.
    /// Surfaces in UI ("Share 2 of 3") and `RecoveryShare::share_index`.
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

/// Paper-recovery share for offline storage. The user transcribes
/// `mnemonic` to paper; the non-secret fields (`share_index`,
/// `threshold`, `total_shares`, `group_public_key`,
/// `verifying_share_bytes`) must also be recorded — public, but needed
/// to reconstruct a `SecretShare` from the mnemonic alone.
///
/// **The mnemonic is the secret.** The other fields can be printed
/// alongside in cleartext.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryShare {
    /// 24-word BIP39 English mnemonic of the 32-byte signing scalar.
    pub mnemonic: String,
    /// 1-indexed share number this share represents.
    pub share_index: u16,
    /// The `t` in t-of-n at generation time, carried so the recovery flow
    /// can sanity-check the share belongs to the group being restored into.
    pub threshold: u16,
    /// The `n` in t-of-n.
    pub total_shares: u16,
    /// The group public key, so `import_recovery_share` can refuse a
    /// mnemonic belonging to a different group.
    pub group_public_key: Ed25519PublicKey,
    /// The 32-byte public verifying-share for this index, needed to rebuild
    /// a `KeyPackage` from the mnemonic alone.
    #[serde(with = "serde_bytes_codec")]
    pub verifying_share_bytes: Vec<u8>,
}

impl RecoveryShare {
    /// Build a recovery share from a `KeyPackage`: the 32-byte signing-share
    /// scalar is serialised then BIP39-encoded into 24 words; remaining
    /// (public) fields are read straight off the `KeyPackage`.
    pub(crate) fn from_key_package(
        kp: &KeyPackage,
        total_shares: u16,
    ) -> Result<Self, ThresholdError> {
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

    /// Decode a recovery share into a usable `SecretShare`. `total_shares`
    /// comes from `self.total_shares`; the caller is responsible for
    /// cross-checking threshold/identifier/group-key against the live
    /// `ThresholdIdentity` being restored into (see
    /// [`crate::identity::ThresholdIdentity::adopt_recovery_share`]).
    ///
    /// # Errors
    ///
    /// Returns [`ThresholdError::BadRecoveryMnemonic`] if the mnemonic
    /// fails to parse or does not decode to exactly 32 bytes of
    /// entropy, or a FROST-internal error if the resulting scalar
    /// does not form a valid key package.
    pub fn into_secret_share(self) -> Result<SecretShare, ThresholdError> {
        // Decode the mnemonic into 32 bytes of entropy. `parse_normalized`
        // (not `parse`) avoids the unicode-normalization feature flag — our
        // BIP39 English wordlist is ASCII, so normalisation is a no-op.
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

/// Encode a `frost-core::Identifier` as a `u16`. FROST identifiers are
/// field scalars; constrained to `1..=u16::MAX` so devices get
/// human-friendly indices, matching `Identifier::try_from::<u16>`.
pub(crate) fn identifier_to_u16(id: &Identifier) -> u16 {
    let bytes = id.serialize();
    // Scalars in frost-ed25519 are 32-byte little-endian; a u16 encodes
    // as `[lo, hi, 0, 0, …]`. Reads the bottom 16 bits — the rest is
    // guaranteed zero by `u16_to_identifier`.
    let lo = u16::from(*bytes.first().unwrap_or(&0));
    let hi = u16::from(*bytes.get(1).unwrap_or(&0));
    lo | (hi << 8)
}

/// Construct a `frost-core::Identifier` from a `u16`. Returns
/// `InvalidShareIndex` if `n == 0` (FROST forbids the zero scalar).
pub(crate) fn u16_to_identifier(n: u16) -> Result<Identifier, ThresholdError> {
    if n == 0 {
        return Err(ThresholdError::InvalidShareIndex(0));
    }
    Identifier::try_from(n).map_err(|_| ThresholdError::InvalidShareIndex(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_index_identifiers_round_trip_and_zero_is_rejected() {
        for n in [1u16, 2, 3, 7, 255, 256, 1024, u16::MAX] {
            let id = u16_to_identifier(n).unwrap();
            assert_eq!(identifier_to_u16(&id), n, "case {n}");
        }

        // Case: zero identifier rejected (FROST forbids the zero scalar).
        let err = u16_to_identifier(0).unwrap_err();
        assert!(matches!(err, ThresholdError::InvalidShareIndex(0)));
    }

    #[test]
    fn secret_share_debug_does_not_leak() {
        // Synthetic KeyPackage via the trusted-dealer path, confirming the
        // Debug form is redacted. FROST requires min_signers ≥ 2, so 2-of-2
        // even though only one share needs inspecting.
        use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
        use rand_core::SeedableRng as _;
        let mut rng = rand_chacha::ChaCha20Rng::from_seed([0x5a; 32]);
        let (shares, _pubkeys) = generate_with_dealer(
            2,
            2,
            IdentifierList::Default,
            crate::rng::FrostRng(&mut rng),
        )
        .unwrap();
        let (_id, share) = shares.into_iter().next().unwrap();
        let kp: KeyPackage = share.try_into().unwrap();
        let wrapped = SecretShare::from_key_package(kp);
        let dbg = format!("{wrapped:?}");
        assert!(
            dbg.contains("<redacted>"),
            "Debug must redact secret: {dbg}"
        );
        assert!(
            !dbg.contains("SigningShare"),
            "Debug leaked field name: {dbg}"
        );
    }
}
