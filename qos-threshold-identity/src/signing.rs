//! Threshold signing: round-1 commit, round-2 sign, aggregate.
//!
//! Signing in FROST is a two-round protocol. Round 1 is the
//! "commit" phase: every signer locally generates a fresh
//! per-signature nonce and publishes the commitment. Round 2 is
//! the "sign" phase: with the message + everyone's commitments
//! in hand, every signer produces a partial signature. The
//! aggregator folds the partials into a single Ed25519
//! signature.
//!
//! ## Round-2 partial carries the signer's verifying share
//!
//! The aggregator needs a `frost_ed25519::keys::PublicKeyPackage`
//! to verify each partial before combining. A
//! `PublicKeyPackage` requires per-signer verifying shares.
//! Rather than thread that through as an out-of-band argument,
//! we include each signer's verifying share **inside** the
//! [`SigningRound2Partial`] — the share is public information
//! (it is published as part of DKG output), so the wire carries
//! no extra secret material.
//!
//! This lets the public [`aggregate_signature`] entry point be a
//! pure function over `(group_pk, partials, threshold)` — the
//! same shape as the project spec dictates.
//!
//! ## Pre-image is canonical bytes
//!
//! Higher-level callers (the `ThresholdIdentity::sign_message`
//! wrapper) pre-canonicalise the message via
//! `qos_collab_types::canonical_bytes` so the threshold
//! signature is byte-for-byte interchangeable with a non-threshold
//! signature produced by `qos_collab_types::sign_event` over the
//! same logical value. The low-level helpers here take a raw
//! `&[u8]` and document that the caller is responsible for
//! canonicalisation.

use std::collections::BTreeMap;

use frost_ed25519::round1::{SigningCommitments, SigningNonces};
use frost_ed25519::round2::SignatureShare;
use frost_ed25519::SigningPackage;
use frost_ed25519::keys::{PublicKeyPackage, VerifyingShare};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use qos_collab_types::{Ed25519PublicKey, Signature, VerifyError};

use crate::errors::ThresholdError;
use crate::shares::{SecretShare, u16_to_identifier};

/// One signer's round-1 commitment, ready for the wire.
///
/// Carries the sender's share index plus the serialised
/// `frost_ed25519::round1::SigningCommitments`. Commitments leak
/// no information about the signing share; broadcasting is safe.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SigningRound1Commitment {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `SigningCommitments`. Opaque.
    #[serde(with = "serde_bytes")]
    pub commitment_bytes: Vec<u8>,
}

/// One signer's round-1 emission: the wire commitment plus
/// the **private** nonces that must be retained until round 2.
pub struct SigningRound1 {
    /// The wire commitment, broadcast to all other signers.
    pub commitment: SigningRound1Commitment,
    /// Private nonces. Must not leave the device.
    pub nonces: SigningSessionNonces,
}

/// Per-device nonce state held between round 1 and round 2.
///
/// FROST's security guarantee requires single-use nonces. The
/// type deliberately does not implement `Clone`, `Serialize`, or
/// `Default` to make accidental reuse hard.
pub struct SigningSessionNonces {
    pub(crate) inner: SigningNonces,
}

impl std::fmt::Debug for SigningSessionNonces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SigningSessionNonces(<redacted>)")
    }
}

/// One signer's round-2 partial signature.
///
/// Carries enough information for any other party to verify and
/// aggregate without out-of-band coordination:
///
/// - the signed message (canonical bytes);
/// - the full quorum's round-1 commitments and the matching
///   share indices, so the `SigningPackage` is reproducible;
/// - the signer's verifying share, so a `PublicKeyPackage` is
///   reproducible from the quorum's partials alone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SigningRound2Partial {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `SignatureShare`. Opaque.
    #[serde(with = "serde_bytes")]
    pub share_bytes: Vec<u8>,
    /// The signer's verifying share (public). Included so the
    /// aggregator can rebuild a `PublicKeyPackage` without a
    /// separate channel. ~32 bytes.
    #[serde(with = "serde_bytes")]
    pub verifying_share_bytes: Vec<u8>,
    /// The signed message bytes. All partials in one aggregation
    /// must agree on this value.
    #[serde(with = "serde_bytes")]
    pub signed_message_bytes: Vec<u8>,
    /// Share indices participating in this signing run, in BTree
    /// order. All partials must agree.
    pub commitment_indices: Vec<u16>,
    /// Serialised commitments for every signer in
    /// `commitment_indices`, in the same order. All partials
    /// must agree.
    pub commitments_bytes: Vec<Vec<u8>>,
}

/// Produce this device's round-1 commitment + private nonces.
///
/// The commitment is broadcast; the nonces stay local. Round 2
/// consumes the nonces by value.
pub fn sign_round1<R: CryptoRng + RngCore>(
    share: &SecretShare,
    rng: &mut R,
) -> Result<SigningRound1, ThresholdError> {
    let kp = share.key_package();
    let (nonces, commitments) = frost_ed25519::round1::commit(kp.signing_share(), rng);
    let commitment_bytes = commitments.serialize()?;
    Ok(SigningRound1 {
        commitment: SigningRound1Commitment {
            from_share_index: share.share_index(),
            commitment_bytes,
        },
        nonces: SigningSessionNonces { inner: nonces },
    })
}

/// Produce this device's round-2 partial signature.
///
/// `message` is the raw bytes to sign — typically the output of
/// `qos_collab_types::canonical_bytes` on the semantic value.
/// `commitments` must contain a commitment for every signer in
/// the quorum, including this device's own; order does not
/// matter.
pub fn sign_round2(
    share: &SecretShare,
    nonces: SigningSessionNonces,
    message: &[u8],
    commitments: &[SigningRound1Commitment],
) -> Result<SigningRound2Partial, ThresholdError> {
    // Build a sorted commitment view for deterministic wire form.
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
    // `SignatureShare::serialize` returns `Vec<u8>` directly
    // (not a Result); `VerifyingShare::serialize` is fallible.
    // Mirror frost-ed25519's signatures exactly so a future
    // upstream bump doesn't silently bit-rot here.
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

/// Combine partials into a single Ed25519 signature.
///
/// `group_pk` is the group verifying key (the public face of the
/// threshold identity). `partials` must contain at least
/// `threshold` entries; all entries must agree on the signed
/// message, the commitment set, and the commitment bytes.
///
/// Returns a 64-byte Ed25519 signature byte-compatible with
/// `qos_collab_types::verify_event` against `group_pk`.
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
    let first = partials.first().expect("just checked non-empty");
    for other in &partials[1..] {
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

    // Build the PublicKeyPackage from the partials' verifying
    // shares + the group verifying key.
    let mut verifying_shares = BTreeMap::new();
    for p in partials {
        let id = u16_to_identifier(p.from_share_index)?;
        let vs = VerifyingShare::deserialize(&p.verifying_share_bytes)?;
        verifying_shares.insert(id, vs);
    }
    let group_vk = frost_ed25519::VerifyingKey::deserialize(group_pk.as_bytes())?;
    // `min_signers` is `Option<u16>` in frost-core 3.x for
    // compatibility with older serialised PKPs. We always pass
    // `Some(threshold)` — the field is consulted by
    // `aggregate`'s pre-checks.
    let pkp = PublicKeyPackage::new(verifying_shares, group_vk, Some(threshold));

    // Build the SignatureShare map.
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
/// `VerifyingKey::verify`.
///
/// Provided as a convenience: in production we expect
/// verification to go through `qos_collab_types::verify_event`
/// (which uses `ed25519-dalek`). This helper exists so the
/// crate's own tests can assert FROST round-trip correctness
/// before reaching out to the foreign Ed25519 implementation.
pub fn verify_threshold_signature(
    group_pk: &Ed25519PublicKey,
    message: &[u8],
    signature: &Signature,
) -> Result<(), VerifyError> {
    let vk = frost_ed25519::VerifyingKey::deserialize(group_pk.as_bytes())
        .map_err(|_| VerifyError::InvalidPublicKey)?;
    let sig = frost_ed25519::Signature::deserialize(signature.as_bytes())
        .map_err(|_| VerifyError::BadSignature)?;
    vk.verify(message, &sig).map_err(|_| VerifyError::BadSignature)
}
