//! [`ThresholdIdentity`] — the per-device handle to a threshold-shared
//! Ed25519 identity.
//!
//! Holds the group's Ed25519 public key (the "I am me" anchor), the
//! threshold parameters (`t`, `n`), this device's 1-indexed share
//! number, and its secret key material.
//!
//! ## Two internal modes
//!
//! `frost-ed25519`'s key generation requires `min_signers ≥ 2`, but the
//! first-device bootstrap is genuinely 1-of-1. [`InnerKey::SingleDevice`]
//! carries a plain `ed25519_dalek::SigningKey` for that case; signing
//! produces a regular RFC-8032 Ed25519 signature, byte-compatible with
//! the threshold output by design.
//!
//! The single-device key is a **dalek key, not a frost key**, and that
//! choice is load-bearing: a frost `SigningKey` stores the signing
//! *scalar*, while dalek's `SigningKey::from_bytes` takes a 32-byte
//! *seed* expanded through SHA-512. The on-disk record persists 32
//! bytes loaded via `from_bytes`, so a frost scalar round-tripped
//! through that record would derive a different key whose signatures
//! stop verifying. Holding the seed means
//! [`ThresholdIdentity::single_device_seed`] hands out exactly the bytes
//! the record needs.
//!
//! [`InnerKey::Threshold`] takes over once the first DKG runs; from then
//! any signing requires `threshold` shares combined via the round-1 /
//! round-2 API.
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
//! Each DKG transition rotates the group public key; the UX propagates
//! the new key into the user's identity record.

use std::collections::BTreeMap;

use frost_ed25519::keys::PublicKeyPackage;
use rand_core::{CryptoRng, RngCore};

use collab_types::{Ed25519PublicKey, Signature, canonical_bytes};

use crate::dkg::{DkgSession, EnrolmentRound1};
use crate::errors::ThresholdError;
use crate::shares::{RecoveryShare, SecretShare};
use crate::signing::{
    SigningRound1, SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces,
    sign_round1 as low_sign_round1, sign_round2 as low_sign_round2,
};

/// Per-device handle to a threshold-shared Ed25519 identity.
///
/// `group_public_key`/`threshold`/`total_shares`/`this_share_index` are
/// stable per-device facts the enrolment flow records; `inner` is the
/// per-mode private key material.
pub struct ThresholdIdentity {
    /// The group's verifying key — byte-compatible with a regular Ed25519
    /// public key, so downstream code just verifies without knowing anything
    /// about threshold-ness.
    pub group_public_key: Ed25519PublicKey,
    /// `t` in t-of-n: `1` for single-device bootstrap, `2` for v1 once
    /// enrolment ran.
    pub threshold: u16,
    /// `n` in t-of-n: the number of shares currently outstanding (devices +
    /// recovery share, if generated).
    pub total_shares: u16,
    /// 1-indexed share number this device holds. The recovery share holds a
    /// separate index — typically `total_shares` at generation time.
    pub this_share_index: u16,
    /// Per-mode private state. `pub(crate)` so signing helpers reach in;
    /// never serialise.
    pub(crate) inner: InnerKey,
}

/// Mode-discriminated key material. `pub(crate)`, not in the public API
/// — callers pattern-match on `threshold == 1` rather than seeing the
/// enum, so they can't construct synthetic variants that lie about mode.
// `Threshold` carries a frost-core `PublicKeyPackage` (~584 bytes)
// vs. `SingleDevice`'s 32-byte `SigningKey`; not boxed to balance
// sizes, since `Threshold` is the steady state and reads should stay
// pointer-chase free.
#[allow(clippy::large_enum_variant)]
pub(crate) enum InnerKey {
    /// 1-of-1 bootstrap: a plain Ed25519 signing key in dalek (seed-shaped)
    /// form, so the 32 bytes persisted are the 32 bytes expanded back into
    /// the same key. Produces a regular RFC-8032 signature.
    SingleDevice(ed25519_dalek::SigningKey),
    /// `t`-of-`n` mode (t ≥ 2): the device's secret share plus the
    /// public-key package `aggregate` needs.
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

    /// First-device bootstrap returning the raw key material as
    /// `(seed_bytes, group_public_key)`. Equivalent to
    /// [`Self::bootstrap_single_device`] followed by
    /// [`Self::single_device_seed`]; kept standalone for callers (the
    /// compositor's first-boot hook) that only want bytes to write.
    pub fn bootstrap_single_device_seed<R: CryptoRng + RngCore>(
        rng: &mut R,
    ) -> ([u8; 32], Ed25519PublicKey) {
        let sk = ed25519_dalek::SigningKey::generate(rng);
        let seed = *sk.as_bytes();
        let pk = sk.verifying_key().to_bytes();
        (seed, Ed25519PublicKey::from_bytes(pk))
    }

    /// First-device bootstrap: 1-of-1 mode. Generates a fresh Ed25519
    /// keypair and stores the secret in seed form so
    /// [`Self::single_device_seed`] can hand it back for persistence.
    pub fn bootstrap_single_device<R: CryptoRng + RngCore>(
        rng: &mut R,
    ) -> Result<Self, ThresholdError> {
        let signing_key = ed25519_dalek::SigningKey::generate(rng);
        Ok(Self::from_single_device_key(signing_key))
    }

    /// Rebuild a 1-of-1 identity from a [`Self::single_device_seed`] output.
    /// The load half of the persistence round-trip: `group_public_key` is
    /// re-derived from the seed rather than read from the record, so a
    /// record whose stored public key disagrees with its secret can't be
    /// resurrected as a self-inconsistent identity.
    pub fn from_single_device_seed(seed: &[u8; 32]) -> Self {
        Self::from_single_device_key(ed25519_dalek::SigningKey::from_bytes(seed))
    }

    fn from_single_device_key(signing_key: ed25519_dalek::SigningKey) -> Self {
        let pk = signing_key.verifying_key().to_bytes();
        Self {
            group_public_key: Ed25519PublicKey::from_bytes(pk),
            threshold: 1,
            total_shares: 1,
            this_share_index: 1,
            inner: InnerKey::SingleDevice(signing_key),
        }
    }

    /// Export this device's 1-of-1 secret as the 32-byte seed the on-disk
    /// record stores. `None` in threshold mode — no single secret exists
    /// there, and a caller needing one is asking the wrong question.
    /// Callers must treat `None` as "not persistable in single-device
    /// format", never write a placeholder.
    ///
    /// **Secret material** — zeroize the returned array or hand it straight
    /// to a `0600` write; never log it.
    pub fn single_device_seed(&self) -> Option<[u8; 32]> {
        match &self.inner {
            InnerKey::SingleDevice(sk) => Some(*sk.as_bytes()),
            InnerKey::Threshold { .. } => None,
        }
    }

    /// Sign a serializable value end-to-end: single-device fast path, or the
    /// threshold-signing flow. In threshold mode (`t > 1`) this alone never
    /// suffices — the orchestrator must drive multiple devices; see
    /// [`Self::sign_round1`]/[`Self::sign_round2`].
    pub fn sign<V: serde::Serialize, R: CryptoRng + RngCore>(
        &self,
        message: &V,
        rng: &mut R,
    ) -> Result<Signature, ThresholdError> {
        let canon = canonical_bytes(message)?;
        self.sign_raw(&canon, rng)
    }

    /// Single-device or raw-bytes signing: the single-device variant signs
    /// directly via `SigningKey::sign`; threshold mode errors, pointing the
    /// caller at round-1/round-2.
    pub fn sign_raw<R: CryptoRng + RngCore>(
        &self,
        bytes: &[u8],
        rng: &mut R,
    ) -> Result<Signature, ThresholdError> {
        let _ = rng; // single-device signing is deterministic (RFC 8032)
        match &self.inner {
            InnerKey::SingleDevice(sk) => {
                use ed25519_dalek::Signer;
                Ok(Signature::from_bytes(sk.sign(bytes).to_bytes()))
            }
            InnerKey::Threshold { .. } => Err(ThresholdError::DkgStateMisuse(
                "single-device sign called on threshold identity; use sign_round1/round2",
            )),
        }
    }

    /// Begin a fresh DKG run producing a new (t, n) configuration including
    /// this device. `this_share_index_in_new_group`/`new_threshold`/
    /// `new_total_devices` describe the new group. Returns the
    /// [`DkgSession`] handle plus this device's round-1 broadcast; the
    /// orchestrator drives the rest and calls [`Self::adopt_dkg_outcome`]
    /// when done. The old key is unchanged until adoption.
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
    pub fn adopt_dkg_outcome(&mut self, session: DkgSession) -> Result<(), ThresholdError> {
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

    /// Generate the paper-recovery share. **Only valid in threshold mode** —
    /// in 1-of-1 mode the "recovery share" would be the device's full key,
    /// defeating the purpose. Caller must enrol a second share first.
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

    /// Decode a BIP39 mnemonic into a `SecretShare`. The spec's literal
    /// signature is `(words: &str) -> Result<SecretShare>`, but a mnemonic
    /// alone lacks the metadata (identifier, verifying share, group VK,
    /// threshold) to rebuild a `KeyPackage` — surfaces a clear error
    /// pointing at [`Self::import_recovery_share_full`].
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

    /// Adopt a recovery-share `SecretShare` as this device's own — disaster
    /// recovery when physical devices are lost but a paper share + one
    /// surviving device remain. Requires threshold mode (an existing
    /// `PublicKeyPackage` to extend); the recovery share must belong to the
    /// same group.
    pub fn adopt_recovery_share(&mut self, share: SecretShare) -> Result<(), ThresholdError> {
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
                // Insert this share's verifying share into the PublicKeyPackage so
                // aggregate has it.
                let mut verifying_shares: BTreeMap<frost_ed25519::Identifier, _> =
                    public_key_package.verifying_shares().clone();
                let id = crate::shares::u16_to_identifier(crate::shares::identifier_to_u16(
                    kp.identifier(),
                ))?;
                verifying_shares.insert(id, *kp.verifying_share());
                let group_vk =
                    frost_ed25519::VerifyingKey::deserialize(self.group_public_key.as_bytes())?;
                *public_key_package =
                    PublicKeyPackage::new(verifying_shares, group_vk, Some(self.threshold));
                // Replace this device's share with the recovery share — the
                // previous secret is zeroized via SecretShare's drop chain.
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
