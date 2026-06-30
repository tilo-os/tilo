//! [`ThresholdIdentity`] — the per-device handle to a threshold-shared
//! Ed25519 identity.
//!
//! Holds:
//! - the group's Ed25519 public key (the "I am me" anchor);
//! - the threshold parameters (`t` and `n`);
//! - this device's 1-indexed share number;
//! - this device's secret key material.
//!
//! ## Two internal modes
//!
//! `frost-ed25519`'s key-generation (both `split` and `dkg::part1`)
//! requires `min_signers ≥ 2`. The spec's *first-device* bootstrap
//! is genuinely 1-of-1 — there is only one share-holder. We
//! represent that as a [`InnerKey::SingleDevice`] variant carrying
//! a plain `frost_ed25519::SigningKey`. Signing in this mode
//! produces a regular Ed25519 signature via
//! `SigningKey::sign(rng, message)` — byte-compatible with the
//! threshold output by design.
//!
//! [`InnerKey::Threshold`] takes over once the first DKG runs.
//! From that point any signing operation requires `threshold`
//! shares to combine, and threshold-signing's round-1 / round-2
//! API kicks in.
//!
//! ## Lifecycle
//!
//! ```text
//! bootstrap_single_device  →  ThresholdIdentity {1/1, SingleDevice}
//!         │
//!         │  +second device joins
//!         ▼
//!   DKG 2-of-2  →  ThresholdIdentity {2/2, Threshold}   (new group_public_key)
//!         │
//!         │  +third device joins
//!         ▼
//!   DKG 2-of-3  →  ThresholdIdentity {2/3, Threshold}   (new group_public_key)
//! ```
//!
//! Each DKG transition rotates the group public key. The spec
//! accepts this for Phase 8 (refresh-based enrolment is a
//! forward-compatible upgrade); the UX is responsible for
//! propagating the new key in the user's identity record.

use std::collections::BTreeMap;

use frost_ed25519::keys::PublicKeyPackage;
use rand_core::{CryptoRng, RngCore};

use qos_collab_types::{Ed25519PublicKey, Signature, canonical_bytes};

use crate::dkg::{DkgSession, EnrolmentRound1};
use crate::errors::ThresholdError;
use crate::shares::{RecoveryShare, SecretShare};
use crate::signing::{
    SigningRound1, SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces,
    sign_round1 as low_sign_round1, sign_round2 as low_sign_round2,
};

/// Per-device handle to a threshold-shared Ed25519 identity.
///
/// `group_public_key`, `threshold`, `total_shares`, and
/// `this_share_index` are stable per-device facts the enrolment
/// flow records. `inner` is the per-mode private key material.
pub struct ThresholdIdentity {
    /// The group's verifying key. Byte-compatible with a regular
    /// Ed25519 public key; downstream code that just wants to
    /// verify signatures uses this without knowing anything
    /// about threshold-ness.
    pub group_public_key: Ed25519PublicKey,
    /// `t` in t-of-n. `1` for the single-device bootstrap mode;
    /// the spec uses `2` for v1 once enrolment ran.
    pub threshold: u16,
    /// `n` in t-of-n. Equal to the number of shares currently
    /// outstanding (devices + recovery share, if generated).
    pub total_shares: u16,
    /// 1-indexed share number this device holds. The recovery
    /// share holds a separate index — typically `total_shares`
    /// at the time it was generated.
    pub this_share_index: u16,
    /// Per-mode private state. PRIVATE — `pub(crate)` so the
    /// signing helpers can reach into it; never serialise.
    pub(crate) inner: InnerKey,
}

/// Mode-discriminated key material.
///
/// `pub(crate)` and not exposed in the public API. Callers
/// pattern-match on `threshold == 1` to know which mode they're
/// in; we deliberately don't surface the enum so consumers
/// can't construct synthetic variants that lie about their
/// mode.
// The `Threshold` variant carries a frost-core `PublicKeyPackage`
// (~584 bytes) vs. `SingleDevice`'s 32-byte `SigningKey`. Boxing
// to balance variant sizes would force a heap alloc per
// `ThresholdIdentity::adopt_dkg_outcome` — uncommon op, but the
// `Threshold` form is the steady state and we want random-access
// reads to be pointer-chase free. Inline both.
#[allow(clippy::large_enum_variant)]
pub(crate) enum InnerKey {
    /// 1-of-1 bootstrap: a plain Ed25519 signing key. Signing
    /// goes through `SigningKey::sign`, which produces a
    /// regular RFC-8032 Ed25519 signature.
    SingleDevice(frost_ed25519::SigningKey),
    /// `t`-of-`n` mode (t ≥ 2). Owns the device's secret share
    /// plus the public-key package needed by aggregate.
    Threshold {
        /// This device's secret share. PRIVATE.
        secret_share: SecretShare,
        /// Public-key package from DKG. Used by aggregate.
        public_key_package: PublicKeyPackage,
    },
}

impl std::fmt::Debug for InnerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InnerKey::SingleDevice(_) => write!(f, "InnerKey::SingleDevice(<redacted>)"),
            InnerKey::Threshold {
                secret_share,
                public_key_package: _,
            } => f
                .debug_struct("InnerKey::Threshold")
                .field("secret_share", secret_share)
                .field("public_key_package", &"<…>")
                .finish(),
        }
    }
}

impl std::fmt::Debug for ThresholdIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThresholdIdentity")
            .field("group_public_key", &self.group_public_key)
            .field("threshold", &self.threshold)
            .field("total_shares", &self.total_shares)
            .field("this_share_index", &self.this_share_index)
            .field("inner", &self.inner)
            .finish()
    }
}

impl ThresholdIdentity {
    /// Borrow the group public key.
    pub fn group_public_key(&self) -> &Ed25519PublicKey {
        &self.group_public_key
    }

    /// True if this identity is in 1-of-1 bootstrap mode.
    pub fn is_single_device(&self) -> bool {
        matches!(self.inner, InnerKey::SingleDevice(_))
    }

    /// First-device bootstrap: 1-of-1 mode.
    ///
    /// Generates a fresh Ed25519 keypair, stores the secret as a
    /// `SigningKey`. The first DKG run rotates the group public
    /// key per the module docs.
    pub fn bootstrap_single_device<R: CryptoRng + RngCore>(rng: &mut R) -> Result<Self, ThresholdError> {
        let signing_key = frost_ed25519::SigningKey::new(rng);
        let verifying_key = frost_ed25519::VerifyingKey::from(&signing_key);
        let vk_bytes_vec = verifying_key.serialize()?;
        let vk_bytes: [u8; 32] = vk_bytes_vec.as_slice().try_into().map_err(|_| {
            ThresholdError::FrostInternal(
                "verifying key serialise produced non-32-byte output".to_string(),
            )
        })?;
        Ok(Self {
            group_public_key: Ed25519PublicKey::from_bytes(vk_bytes),
            threshold: 1,
            total_shares: 1,
            this_share_index: 1,
            inner: InnerKey::SingleDevice(signing_key),
        })
    }

    /// Sign a serializable value end-to-end. Routes to the
    /// single-device fast path or to the threshold-signing
    /// flow as appropriate.
    ///
    /// In threshold mode (`t > 1`) this only works if the
    /// current device alone constitutes a threshold-sized quorum
    /// — i.e. never, since `t > 1` by construction. The threshold
    /// flow requires the orchestrator to drive multiple devices;
    /// see [`Self::sign_round1`] / [`Self::sign_round2`].
    pub fn sign<V: serde::Serialize, R: CryptoRng + RngCore>(
        &self,
        message: &V,
        rng: &mut R,
    ) -> Result<Signature, ThresholdError> {
        let canon = canonical_bytes(message)?;
        self.sign_raw(&canon, rng)
    }

    /// Single-device or raw-bytes signing path. The single-device
    /// variant signs `bytes` directly via `SigningKey::sign`. In
    /// threshold mode this returns an error pointing the caller
    /// at the round-1 / round-2 API.
    pub fn sign_raw<R: CryptoRng + RngCore>(
        &self,
        bytes: &[u8],
        rng: &mut R,
    ) -> Result<Signature, ThresholdError> {
        match &self.inner {
            InnerKey::SingleDevice(sk) => {
                let sig = sk.sign(rng, bytes);
                let bytes_vec = sig.serialize()?;
                let out: [u8; 64] = bytes_vec.as_slice().try_into().map_err(|_| {
                    ThresholdError::FrostInternal(
                        "single-device signature serialise produced non-64-byte output".to_string(),
                    )
                })?;
                Ok(Signature::from_bytes(out))
            }
            InnerKey::Threshold { .. } => Err(ThresholdError::DkgStateMisuse(
                "single-device sign called on threshold identity; use sign_round1/round2",
            )),
        }
    }

    /// Begin a fresh DKG run that will produce a new (t, n)
    /// configuration including this device.
    ///
    /// Parameters describe the **new** group:
    /// - `this_share_index_in_new_group` — the slot this device
    ///   will occupy after the DKG completes.
    /// - `new_threshold` — `t` in the new group.
    /// - `new_total_devices` — `n` in the new group.
    ///
    /// Returns the [`DkgSession`] handle plus this device's
    /// round-1 broadcast. The orchestrator drives the remaining
    /// rounds and calls [`Self::adopt_dkg_outcome`] when done.
    /// The old key is unchanged until adoption.
    pub fn begin_enrolment<R: CryptoRng + RngCore>(
        &mut self,
        this_share_index_in_new_group: u16,
        new_threshold: u16,
        new_total_devices: u16,
        rng: &mut R,
    ) -> Result<(DkgSession, EnrolmentRound1), ThresholdError> {
        DkgSession::start(
            this_share_index_in_new_group,
            new_threshold,
            new_total_devices,
            rng,
        )
    }

    /// Commit a completed DKG run into this identity. Replaces
    /// the previous key material.
    pub fn adopt_dkg_outcome(
        &mut self,
        session: DkgSession,
    ) -> Result<(), ThresholdError> {
        let (kp, pkp, threshold, total, share_index, group_pk) = session.into_inner_complete()?;
        self.inner = InnerKey::Threshold {
            secret_share: SecretShare::from_key_package(kp),
            public_key_package: pkp,
        };
        self.threshold = threshold;
        self.total_shares = total;
        self.this_share_index = share_index;
        self.group_public_key = group_pk;
        Ok(())
    }

    /// Round-1 of threshold signing. Errors if the identity is
    /// still in single-device mode.
    pub fn sign_round1<R: CryptoRng + RngCore>(
        &self,
        rng: &mut R,
    ) -> Result<SigningRound1, ThresholdError> {
        let share = self.threshold_share()?;
        low_sign_round1(share, rng)
    }

    /// Round-2 of threshold signing. The message is signed in
    /// its **canonical** byte form. Errors if the identity is
    /// in single-device mode.
    pub fn sign_round2<V: serde::Serialize>(
        &self,
        nonces: SigningSessionNonces,
        message: &V,
        commitments: &[SigningRound1Commitment],
    ) -> Result<SigningRound2Partial, ThresholdError> {
        let canon = canonical_bytes(message)?;
        self.sign_round2_raw(nonces, &canon, commitments)
    }

    /// Round-2 over already-canonical bytes (e.g. the output of
    /// `SignedOp::signing_bytes`).
    pub fn sign_round2_raw(
        &self,
        nonces: SigningSessionNonces,
        message: &[u8],
        commitments: &[SigningRound1Commitment],
    ) -> Result<SigningRound2Partial, ThresholdError> {
        let share = self.threshold_share()?;
        low_sign_round2(share, nonces, message, commitments)
    }

    /// Aggregate partial signatures into a single Ed25519
    /// signature. Errors if the identity is in single-device
    /// mode — there are no partials to combine.
    pub fn aggregate_signature(
        &self,
        partials: &[SigningRound2Partial],
    ) -> Result<Signature, ThresholdError> {
        if matches!(self.inner, InnerKey::SingleDevice(_)) {
            return Err(ThresholdError::DkgStateMisuse(
                "aggregate_signature requires threshold mode",
            ));
        }
        crate::signing::aggregate_signature(&self.group_public_key, partials, self.threshold)
    }

    /// Static aggregator. Useful in coordinator processes that
    /// hold the group public key but not a full
    /// `ThresholdIdentity`.
    pub fn aggregate_signature_static(
        message: &[u8],
        group_pk: &Ed25519PublicKey,
        threshold: u16,
        partials: &[SigningRound2Partial],
    ) -> Result<Signature, ThresholdError> {
        if let Some(first) = partials.first()
            && first.signed_message_bytes != message
        {
            return Err(ThresholdError::DkgStateMisuse(
                "static aggregator: partials disagree with message",
            ));
        }
        crate::signing::aggregate_signature(group_pk, partials, threshold)
    }

    /// Generate the paper-recovery share for this identity.
    /// **Only valid in threshold mode** — in 1-of-1 mode the
    /// "recovery share" would be the device's full key, which
    /// defeats the purpose. Caller is expected to enrol a
    /// second share (DKG to ≥2-of-N) before exporting.
    pub fn export_recovery_share(&self) -> Result<RecoveryShare, ThresholdError> {
        match &self.inner {
            InnerKey::Threshold { secret_share, .. } => {
                RecoveryShare::from_key_package(secret_share.key_package(), self.total_shares)
            }
            InnerKey::SingleDevice(_) => Err(ThresholdError::DkgStateMisuse(
                "recovery share export requires threshold mode (run DKG first)",
            )),
        }
    }

    /// Decode a BIP39 mnemonic into a `SecretShare`. The literal
    /// spec signature is `(words: &str) -> Result<SecretShare>`,
    /// but the BIP39 mnemonic alone does NOT carry enough
    /// metadata (identifier, verifying share, group VK,
    /// threshold) to rebuild a `KeyPackage`. We surface a clear
    /// error pointing callers at
    /// [`Self::import_recovery_share_full`].
    pub fn import_recovery_share(words: &str) -> Result<SecretShare, ThresholdError> {
        let _ = words;
        Err(ThresholdError::BadRecoveryMnemonic(
            "use import_recovery_share_full(&RecoveryShare) — the mnemonic alone \
             lacks the verifying-share/group metadata needed to rebuild a KeyPackage"
                .to_string(),
        ))
    }

    /// Decode a `RecoveryShare` (mnemonic + public metadata)
    /// back into a `SecretShare`.
    pub fn import_recovery_share_full(rs: RecoveryShare) -> Result<SecretShare, ThresholdError> {
        rs.into_secret_share()
    }

    /// Adopt a recovery-share `SecretShare` as this device's
    /// own share. Used in disaster recovery: the user has lost
    /// physical devices but holds the paper share + one
    /// surviving device.
    ///
    /// Requires threshold mode (an existing `PublicKeyPackage`
    /// to extend). The recovery share must belong to the same
    /// group.
    pub fn adopt_recovery_share(
        &mut self,
        share: SecretShare,
    ) -> Result<(), ThresholdError> {
        let kp = share.key_package();
        let kp_group_vec = kp.verifying_key().serialize()?;
        let kp_group: [u8; 32] = kp_group_vec.as_slice().try_into().map_err(|_| {
            ThresholdError::FrostInternal(
                "key package group VK serialise produced non-32-byte output".to_string(),
            )
        })?;
        if &kp_group != self.group_public_key.as_bytes() {
            return Err(ThresholdError::RecoveryMismatch(
                "recovery share belongs to a different group",
            ));
        }
        if *kp.min_signers() != self.threshold {
            return Err(ThresholdError::RecoveryMismatch(
                "recovery share carries a different threshold",
            ));
        }
        match &mut self.inner {
            InnerKey::SingleDevice(_) => Err(ThresholdError::DkgStateMisuse(
                "adopt_recovery_share requires threshold mode",
            )),
            InnerKey::Threshold {
                secret_share,
                public_key_package,
            } => {
                // Insert this share's verifying share into the
                // PublicKeyPackage so aggregate has it.
                let mut verifying_shares: BTreeMap<frost_ed25519::Identifier, _> =
                    public_key_package.verifying_shares().clone();
                let id = crate::shares::u16_to_identifier(crate::shares::identifier_to_u16(
                    kp.identifier(),
                ))?;
                verifying_shares.insert(id, *kp.verifying_share());
                let group_vk =
                    frost_ed25519::VerifyingKey::deserialize(self.group_public_key.as_bytes())?;
                *public_key_package = PublicKeyPackage::new(
                    verifying_shares,
                    group_vk,
                    Some(self.threshold),
                );
                // Replace this device's share with the recovery
                // share. The previous share's secret material is
                // zeroized via SecretShare's drop chain.
                let kp_owned: frost_ed25519::keys::KeyPackage = kp.clone();
                self.this_share_index = crate::shares::identifier_to_u16(kp_owned.identifier());
                *secret_share = SecretShare::from_key_package(kp_owned);
                Ok(())
            }
        }
    }

    /// Crate-internal: borrow this device's `SecretShare`. Errors
    /// in single-device mode.
    fn threshold_share(&self) -> Result<&SecretShare, ThresholdError> {
        match &self.inner {
            InnerKey::Threshold { secret_share, .. } => Ok(secret_share),
            InnerKey::SingleDevice(_) => Err(ThresholdError::DkgStateMisuse(
                "operation requires threshold mode",
            )),
        }
    }
}
