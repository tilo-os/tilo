//! Threshold signing: round-1 commit, round-2 sign, aggregate.
//!
//! FROST signing is two rounds: round 1 ("commit") has every signer
//! generate a fresh per-signature nonce and publish the commitment;
//! round 2 ("sign") has every signer produce a partial signature once
//! the message + all commitments are in hand. The aggregator folds
//! partials into a single Ed25519 signature.
//!
//! [`SigningRound2Partial`] carries the signer's verifying share inline
//! rather than threading a `PublicKeyPackage` through as an out-of-band
//! argument — the share is public (published as DKG output), so this
//! adds no secret material to the wire, and lets [`aggregate_signature`]
//! stay a pure function over `(group_pk, partials, threshold)`.
//!
//! The pre-image is canonical bytes: higher-level callers
//! (`ThresholdIdentity::sign_message`) pre-canonicalise via
//! `collab_types::canonical_bytes`, so the threshold signature is
//! byte-for-byte interchangeable with a non-threshold signature from
//! `collab_types::sign_event` over the same value. Low-level helpers
//! here take a raw `&[u8]`; canonicalisation is the caller's job.

use std::collections::BTreeMap;

use frost_ed25519::SigningPackage;
use frost_ed25519::keys::{PublicKeyPackage, VerifyingShare};
use frost_ed25519::round1::{SigningCommitments, SigningNonces};
use frost_ed25519::round2::SignatureShare;
use rand_core::CryptoRng;
use serde::{Deserialize, Serialize};
use serde_bytes as serde_bytes_codec;

use collab_types::{Ed25519PublicKey, Signature, VerifyError};

use crate::errors::ThresholdError;
use crate::shares::{SecretShare, u16_to_identifier};

/// One signer's round-1 commitment, ready for the wire: the sender's
/// share index plus the serialised `SigningCommitments`. Leaks no
/// information about the signing share — safe to broadcast.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SigningRound1Commitment {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `SigningCommitments`. Opaque.
    #[serde(with = "serde_bytes_codec")]
    pub commitment_bytes: Vec<u8>,
}

/// One signer's round-1 emission: the wire commitment plus the
/// **private** nonces that must be retained until round 2.
pub struct SigningRound1 {
    /// The wire commitment, broadcast to all other signers.
    pub commitment: SigningRound1Commitment,
    /// Private nonces. Must not leave the device.
    pub nonces: SigningSessionNonces,
}

/// Per-device nonce state held between round 1 and round 2. FROST's
/// security guarantee requires single-use nonces, so this type
/// deliberately doesn't implement `Clone`, `Serialize`, or `Default`.
pub struct SigningSessionNonces {
    pub(crate) inner: SigningNonces,
}

impl std::fmt::Debug for SigningSessionNonces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SigningSessionNonces(<redacted>)")
    }
}

/// One signer's round-2 partial signature, carrying enough for any
/// other party to verify and aggregate with no out-of-band
/// coordination: the signed message (canonical bytes); the full
/// quorum's round-1 commitments + matching share indices (so
/// `SigningPackage` is reproducible); and the signer's verifying share
/// (so a `PublicKeyPackage` is reproducible from the partials alone).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SigningRound2Partial {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `SignatureShare`. Opaque.
    #[serde(with = "serde_bytes_codec")]
    pub share_bytes: Vec<u8>,
    /// The signer's verifying share (public), included so the aggregator
    /// can rebuild a `PublicKeyPackage` without a separate channel.
    #[serde(with = "serde_bytes_codec")]
    pub verifying_share_bytes: Vec<u8>,
    /// The signed message bytes — all partials in one aggregation must agree.
    #[serde(with = "serde_bytes_codec")]
    pub signed_message_bytes: Vec<u8>,
    /// Share indices participating in this run, in `BTree` order — all
    /// partials must agree.
    pub commitment_indices: Vec<u16>,
    /// Serialised commitments for every signer in `commitment_indices`, same
    /// order — all partials must agree.
    pub commitments_bytes: Vec<Vec<u8>>,
}

/// Produce this device's round-1 commitment + private nonces. The
/// commitment is broadcast; the nonces stay local and round 2 consumes
/// them by value.
///
/// # Errors
///
/// Returns an error if the freshly-generated commitments cannot be
/// serialized (effectively unreachable).
pub fn sign_round1<R: CryptoRng + ?Sized>(
    share: &SecretShare,
    rng: &mut R,
) -> Result<SigningRound1, ThresholdError> {
    let kp = share.key_package();
    let (nonces, commitments) =
        frost_ed25519::round1::commit(kp.signing_share(), &mut crate::rng::FrostRng(rng));
    let commitment_bytes = commitments.serialize()?;
    Ok(SigningRound1 {
        commitment: SigningRound1Commitment {
            from_share_index: share.share_index(),
            commitment_bytes,
        },
        nonces: SigningSessionNonces { inner: nonces },
    })
}

/// Produce this device's round-2 partial signature. `message` is the
/// raw bytes to sign (typically `collab_types::canonical_bytes` output);
/// `commitments` must contain one for every signer in the quorum
/// including this device — order doesn't matter.
///
/// # Errors
///
/// Returns an error if a commitment in `commitments` fails to
/// deserialize, or if the underlying FROST round-2 signing operation
/// fails (e.g. a malformed commitment set).
// `nonces` is taken by value deliberately, not left over-eagerly:
// `SigningSessionNonces` doesn't implement `Clone` specifically so a
// single-use nonce can't be reused (FROST security requires this —
// see the type's own doc comment). A `&SigningSessionNonces` parameter
// would let a caller sign twice with the same nonce, which leaks the
// signing key. Do not "fix" this into a reference.
#[allow(
    clippy::needless_pass_by_value,
    reason = "nonces must be consumed exactly once — taking ownership is how single-use is enforced at the type level"
)]
pub fn sign_round2(
    share: &SecretShare,
    nonces: SigningSessionNonces,
    message: &[u8],
    commitments: &[SigningRound1Commitment],
) -> Result<SigningRound2Partial, ThresholdError> {
    let mut sorted = commitments.to_vec();
    sorted.sort_by_key(|c| c.from_share_index);
    let mut by_id = BTreeMap::new();
    let mut commitment_indices = Vec::with_capacity(sorted.len());
    let mut commitments_bytes = Vec::with_capacity(sorted.len());
    for c in &sorted {
        let id = u16_to_identifier(c.from_share_index)?;
        let parsed = SigningCommitments::deserialize(&c.commitment_bytes)?;
        by_id.insert(id, parsed);
        commitment_indices.push(c.from_share_index);
        commitments_bytes.push(c.commitment_bytes.clone());
    }
    let pkg = SigningPackage::new(by_id, message);
    let partial = frost_ed25519::round2::sign(&pkg, &nonces.inner, share.key_package())?;
    // `SignatureShare::serialize` returns `Vec<u8>` directly (not a
    // Result); `VerifyingShare::serialize` is fallible — mirrors
    // frost-ed25519's own signatures so an upstream bump doesn't
    // silently bit-rot here.
    let share_bytes = partial.serialize();
    let verifying_share_bytes = share.key_package().verifying_share().serialize()?;
    Ok(SigningRound2Partial {
        from_share_index: share.share_index(),
        share_bytes,
        verifying_share_bytes,
        signed_message_bytes: message.to_vec(),
        commitment_indices,
        commitments_bytes,
    })
}

/// Combine partials into a single Ed25519 signature. `group_pk` is the
/// group verifying key; `partials` must contain at least `threshold`
/// entries, all agreeing on the message, commitment set, and commitment
/// bytes. Returns a 64-byte signature byte-compatible with
/// `collab_types::verify_event` against `group_pk`.
///
/// # Errors
///
/// Returns [`ThresholdError::InsufficientPartials`] if `partials` has
/// fewer than `threshold` entries (this also covers the pathological
/// `threshold == 0` case, so an empty `partials` never reaches the
/// aggregation step below), [`ThresholdError::DkgStateMisuse`] if the
/// partials disagree on the signed message or commitment set, or a
/// FROST deserialization/aggregation error for a malformed partial.
pub fn aggregate_signature(
    group_pk: &Ed25519PublicKey,
    partials: &[SigningRound2Partial],
    threshold: u16,
) -> Result<Signature, ThresholdError> {
    if partials.len() < threshold as usize {
        return Err(ThresholdError::InsufficientPartials {
            have: partials.len(),
            need: threshold as usize,
        });
    }
    let Some(first) = partials.first() else {
        return Err(ThresholdError::InsufficientPartials {
            have: 0,
            need: threshold as usize,
        });
    };
    for other in partials.iter().skip(1) {
        if other.signed_message_bytes != first.signed_message_bytes
            || other.commitment_indices != first.commitment_indices
            || other.commitments_bytes != first.commitments_bytes
        {
            return Err(ThresholdError::DkgStateMisuse(
                "partials disagree on the signed message or commitment set",
            ));
        }
    }

    // Rebuild the SigningPackage (commitments + message).
    let mut commitments_by_id = BTreeMap::new();
    for (idx, cb) in first
        .commitment_indices
        .iter()
        .zip(first.commitments_bytes.iter())
    {
        let id = u16_to_identifier(*idx)?;
        commitments_by_id.insert(id, SigningCommitments::deserialize(cb)?);
    }
    let pkg = SigningPackage::new(commitments_by_id, &first.signed_message_bytes);

    // Build the PublicKeyPackage from the partials' verifying shares +
    // the group verifying key.
    let mut verifying_shares = BTreeMap::new();
    for p in partials {
        let id = u16_to_identifier(p.from_share_index)?;
        let vs = VerifyingShare::deserialize(&p.verifying_share_bytes)?;
        verifying_shares.insert(id, vs);
    }
    let verifying_key = frost_ed25519::VerifyingKey::deserialize(group_pk.as_bytes())?;
    // `min_signers` is `Option<u16>` in frost-core 3.x for backward
    // compatibility with older serialised PKPs; always pass
    // `Some(threshold)` since `aggregate`'s pre-checks consult it.
    let pkp = PublicKeyPackage::new(verifying_shares, verifying_key, Some(threshold));

    let mut shares = BTreeMap::new();
    for p in partials {
        let id = u16_to_identifier(p.from_share_index)?;
        shares.insert(id, SignatureShare::deserialize(&p.share_bytes)?);
    }

    let sig = frost_ed25519::aggregate(&pkg, &shares, &pkp)?;
    let bytes_vec = sig.serialize()?;
    let bytes: [u8; 64] = bytes_vec.as_slice().try_into().map_err(|_| {
        ThresholdError::FrostInternal(
            "aggregated signature serialise produced non-64-byte output".to_string(),
        )
    })?;
    Ok(Signature::from_bytes(bytes))
}

/// Verify a threshold-produced signature via FROST's own
/// `VerifyingKey::verify`. A convenience: production verification goes
/// through `collab_types::verify_event` (ed25519-dalek) — this helper
/// lets the crate's own tests assert FROST round-trip correctness before
/// reaching the foreign Ed25519 implementation.
///
/// # Errors
///
/// Returns [`VerifyError::InvalidPublicKey`] if `group_pk` is not a
/// valid Ed25519 point, or [`VerifyError::BadSignature`] if
/// `signature` is malformed or does not verify against `message`.
pub fn verify_threshold_signature(
    group_pk: &Ed25519PublicKey,
    message: &[u8],
    signature: &Signature,
) -> Result<(), VerifyError> {
    let vk = frost_ed25519::VerifyingKey::deserialize(group_pk.as_bytes())
        .map_err(|_| VerifyError::InvalidPublicKey)?;
    let sig = frost_ed25519::Signature::deserialize(signature.as_bytes())
        .map_err(|_| VerifyError::BadSignature)?;
    vk.verify(message, &sig)
        .map_err(|_| VerifyError::BadSignature)
}
