//! Secret-share wrapper.
//!
//! `frost-core::keys::KeyPackage` carries the full per-participant
//! crypto state (secret `SigningShare`, public `VerifyingShare`, group
//! `VerifyingKey`, identifier, `min_signers`). [`SecretShare`] wraps it
//! for three reasons: (1) custom `Debug` — `KeyPackage::Debug` leaks the
//! secret signing share in panic messages, ours prints `"<redacted>"`;
//! (2) no `Serialize` — deliberately not implemented, so it can't be
//! JSON-logged; (3) zeroize on drop, via `frost-core`'s own
//! `ZeroizeOnDrop` on the wrapped `KeyPackage`.

use frost_ed25519::Identifier;
use frost_ed25519::keys::KeyPackage;

use crate::errors::ThresholdError;

/// Opaque secret-bearing share for one device. Wraps a
/// `frost_ed25519::keys::KeyPackage` (zeroized on drop by `frost-core`
/// itself); our own `Drop` also wipes any future incidental state and
/// guards against a `Serialize` derive ever sneaking onto this type.
pub struct SecretShare {
    pub(crate) inner: KeyPackage,
}

impl SecretShare {
    /// Construct from a verified `KeyPackage` — used by DKG finalisation.
    pub(crate) fn from_key_package(kp: KeyPackage) -> Self {
        Self { inner: kp }
    }

    /// Borrow the underlying `KeyPackage`. `pub(crate)` — nothing outside
    /// the crate should touch the raw FROST type; the wrapper exists to prevent that.
    pub(crate) fn key_package(&self) -> &KeyPackage {
        &self.inner
    }

    /// 1-indexed share number, mirroring the underlying `Identifier`.
    /// Surfaces in UI ("Share 2 of 3").
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
