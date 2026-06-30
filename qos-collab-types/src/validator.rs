//! The inbound-op validation pipeline.
//!
//! This module implements the eight-step pipeline specified in
//! `2026-06-29-spaces-channels-and-cryptographic-roles.md`,
//! "Validation pipeline (per inbound op)". Each step is a
//! standalone check with a dedicated [`ValidationError`] variant
//! so callers can pattern-match the failure and surface a precise
//! reason in the audit log + toast.
//!
//! The validator is pure: it takes a [`ChannelState`] *snapshot*
//! and a `SignedOp`, returns `Ok(())` or a structured error. It
//! does not touch the network, does not lazy-fetch missing
//! attestations, and does not mutate global state. The single side
//! effect — bumping the last-seen sequence number on success — is
//! applied to the borrowed `&mut ChannelState`. Callers that want
//! to validate-without-applying use [`ChannelState::clone`] and
//! discard the modified clone.
//!
//! ## Snapshot vs live state
//!
//! `ChannelState` is the validator's *view* of the channel. The
//! authoritative live state lives in the broker (Phase 3+). The
//! split lets the validator be tested without a broker and lets
//! the broker assemble a snapshot from cheap accessors. The
//! snapshot includes:
//!
//! - the current set of accepted *channel-local* attestations
//!   (member-role grants);
//! - the set of identity-level attestations for each known author
//!   (used by the clearance check);
//! - the set of public keys authorised to *issue* attestations
//!   (channel owner + delegations);
//! - the set of trust roots accepted for identity-level attestations
//!   (e.g. an org's master key, for clearance gates);
//! - the artifact kind for each artifact id (for the role-table
//!   lookup);
//! - the per-author last-seen sequence number (for replay defence);
//! - the channel's `min_clearance` floor;
//! - the per-artifact classification override.

use std::collections::HashMap;
use std::time::SystemTime;

use thiserror::Error;

use crate::channel::ArtifactId;
use crate::crypto::{Ed25519PublicKey, VerifyError, verify_event, verify_op};
use crate::event::RoleAttestation;
use crate::identity::{Attestation, AttestationId, Claim, Clearance};
use crate::op::SignedOp;
use crate::role::{ArtifactKind, role_permits};

/// Errors produced by the inbound-op validator.
///
/// Each variant maps to exactly one of the eight numbered
/// validation steps in the spec (with `Crypto` covering the
/// underlying signature-verification primitive). Variants carry
/// enough context to log a structured audit entry; the
/// `OpRejected { op_hash, reason, observer }` event in the spec
/// uses the `Display` form as the reason string.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// Step 1: the op references an attestation id the validator
    /// has never seen. In production the broker lazy-fetches and
    /// defers; the validator just rejects.
    #[error("unknown attestation id: {0:?}")]
    UnknownAttestation(AttestationId),

    /// Step 2: the attestation was signed by a key not in the
    /// channel's `authorised_signers`. Either the attestation is
    /// forged, or its issuer's delegation chain isn't visible to
    /// the validator yet.
    #[error("attestation issuer {0:?} is not an authorised channel signer")]
    UnauthorisedAttestation(Ed25519PublicKey),

    /// Step 3: the `SignedOp::signature` did not verify against
    /// `SignedOp::author`'s public key. Either the op was tampered
    /// with post-signing, or a different key tried to forge.
    #[error("op signature did not verify: {0}")]
    BadOpSignature(#[source] VerifyError),

    /// Step 2 sub-case: the attestation's own signature did not
    /// verify against its claimed issuer. Distinct from step 3 so
    /// log triage can tell op-forgery from attestation-forgery.
    #[error("attestation signature did not verify: {0}")]
    BadAttestationSignature(#[source] VerifyError),

    /// Step 4: `op.author` and `attestation.subject` disagree.
    /// Common cause: replaying someone else's attestation onto
    /// your own op.
    #[error("op author does not match attestation subject")]
    AuthorMismatch,

    /// Step 5: the role table forbids this op class for this
    /// (role, artifact-kind, op-class) triple.
    #[error("role does not permit this op class on this artifact kind")]
    RoleForbidsOpClass,

    /// Step 5 sub-case: the artifact referenced by `op.artifact`
    /// is unknown to the validator. Without an artifact kind the
    /// role-table lookup is undefined; we reject.
    #[error("unknown artifact id: {0:?}")]
    UnknownArtifact(ArtifactId),

    /// Step 6: the op's `timestamp` is outside the attestation's
    /// validity window. Either the attestation has expired, or the
    /// op pre-dates the attestation's `valid_from`, or a clock-skew
    /// adversary is shifting timestamps.
    #[error("op timestamp is outside attestation validity window")]
    OutsideValidityWindow,

    /// Step 7: the op's `seq` is not strictly greater than the
    /// last-seen seq for the same author. Replay or
    /// out-of-order delivery.
    #[error("replay or out-of-order seq: {seen:?} >= incoming {incoming}")]
    ReplaySeq {
        /// The last-seen seq for this author, prior to this op.
        seen: u64,
        /// The seq carried by this op.
        incoming: u64,
    },

    /// Step 8a: artifact requires a clearance but the author has
    /// no `ClearedAt` claim that meets the floor.
    #[error("insufficient clearance: required {required:?}, author has {found:?}")]
    InsufficientClearance {
        /// The artifact-level (or channel-level, whichever is
        /// stricter) clearance floor.
        required: Clearance,
        /// The highest `ClearedAt` claim found for this author, or
        /// `None` if no such claim exists.
        found: Option<Clearance>,
    },

    /// Step 8b: the author has a `ClearedAt` claim but the issuer
    /// is not in the channel's `accepted_trust_roots`. We refuse
    /// to honour a self-signed clearance.
    #[error(
        "clearance claim was issued by {issuer:?}, which is not in the accepted trust roots"
    )]
    UntrustedClearanceIssuer {
        /// The issuer key on the rejected `ClearedAt` claim.
        issuer: Ed25519PublicKey,
    },
}

/// The validator's snapshot of a channel.
///
/// Built by the broker (or the test harness) from cheap accessors
/// and handed in by value. The validator mutates only
/// `last_seq_per_author` on a successful validation; everything
/// else is read-only.
///
/// Fields are `pub` rather than constructor-protected because this
/// is a foundation crate and downstream code will compose these in
/// ways we can't predict. The cost of public fields is balanced by
/// the explicit documentation of each invariant below.
#[derive(Clone, Debug, Default)]
pub struct ChannelState {
    /// Channel-local attestations, keyed by id. These are
    /// "member of this channel has role X" grants. Identity-level
    /// attestations (clearance) live separately in
    /// `identity_attestations` because they have different trust
    /// roots and different lifetimes.
    pub attestations: HashMap<AttestationId, RoleAttestation>,

    /// Public keys authorised to issue channel-local attestations.
    /// Always includes the channel's owner; additional entries for
    /// each `Owner`-role delegation.
    pub authorised_signers: Vec<Ed25519PublicKey>,

    /// For each known identity public key, the identity-level
    /// attestations observed (clearance, employment, etc.). Used by
    /// step 8 to find a `ClearedAt` claim for the author.
    pub identity_attestations: HashMap<Ed25519PublicKey, Vec<Attestation>>,

    /// Trust roots whose signatures we accept on identity-level
    /// attestations (e.g. an org's master key for clearance
    /// claims). Distinct from `authorised_signers` because the
    /// channel owner is rarely the same entity as the org's
    /// security officer.
    pub accepted_trust_roots: Vec<Ed25519PublicKey>,

    /// Artifact-kind lookup keyed by artifact id. Required for the
    /// step-5 role-table query.
    pub artifacts: HashMap<ArtifactId, ArtifactKind>,

    /// Per-artifact classification override. Composed with the
    /// channel-level floor via `max(channel_min, artifact_classification)`.
    pub artifact_classifications: HashMap<ArtifactId, Clearance>,

    /// The channel's `min_clearance` floor.
    pub channel_min_clearance: Option<Clearance>,

    /// Last-seen sequence number per author, used for the step-7
    /// replay-defence check. Updated in-place on each successful
    /// validation; callers that want a non-destructive validation
    /// pass `&mut clone()` and drop the clone.
    pub last_seq_per_author: HashMap<Ed25519PublicKey, u64>,
}

impl ChannelState {
    /// Convenience helper: register a role-attestation in both
    /// the per-id map and (if the issuer isn't already known) as
    /// an authorised signer. Used heavily by tests.
    pub fn register_attestation(&mut self, ra: RoleAttestation) {
        self.attestations.insert(ra.attestation.id, ra);
    }
}

/// Run the eight-step inbound-op validation pipeline against the
/// given snapshot. On success, `channel_state.last_seq_per_author`
/// is updated for the op's author so subsequent ops with the same
/// or earlier seq are rejected.
///
/// Returns the first failure encountered; we do not collect all
/// failures because in practice the audit-log entry only carries
/// one reason string and the operator wants the most specific
/// cause.
///
/// Steps:
///
/// 1. Resolve `op.attestation_id` in `channel_state.attestations`.
/// 2. Verify the attestation's own signature against an authorised
///    signer.
/// 3. Verify the op's signature against the author's public key.
/// 4. `op.author == attestation.subject`.
/// 5. `role_permits(role, kind, op_class)` for the resolved triple.
/// 6. `op.timestamp ∈ [attestation.valid_from, attestation.valid_until]`.
/// 7. `op.seq > last_seq_per_author[op.author]`.
/// 8. Clearance gate: if the artifact's effective classification
///    is set, the author needs a matching `ClearedAt` claim
///    issued by an accepted trust root.
pub fn validate_inbound_op(
    op: &SignedOp,
    channel_state: &mut ChannelState,
) -> Result<(), ValidationError> {
    // Step 1: resolve attestation.
    let ra = channel_state
        .attestations
        .get(&op.attestation_id)
        .ok_or(ValidationError::UnknownAttestation(op.attestation_id))?
        .clone();
    let attestation = &ra.attestation;

    // Step 2: verify the attestation traces to an authorised
    // signer. The attestation's `issued_by` must be in the channel's
    // `authorised_signers`, and the attestation's own signature must
    // verify against that key.
    if !channel_state
        .authorised_signers
        .contains(&attestation.issued_by)
    {
        return Err(ValidationError::UnauthorisedAttestation(
            attestation.issued_by,
        ));
    }
    {
        let bytes = attestation
            .signing_bytes()
            .map_err(|e| ValidationError::BadAttestationSignature(VerifyError::Canonical(e)))?;
        verify_event(&bytes, &attestation.signature, &attestation.issued_by)
            .map_err(ValidationError::BadAttestationSignature)?;
    }

    // Step 3: verify the op signature against the author's pubkey.
    verify_op(op).map_err(ValidationError::BadOpSignature)?;

    // Step 4: author matches attestation subject.
    if op.author != attestation.subject {
        return Err(ValidationError::AuthorMismatch);
    }

    // Step 5: role-table lookup. Requires artifact kind.
    let artifact_kind = channel_state
        .artifacts
        .get(&op.artifact)
        .copied()
        .ok_or(ValidationError::UnknownArtifact(op.artifact))?;
    if !role_permits(ra.role, artifact_kind, op.op_class) {
        return Err(ValidationError::RoleForbidsOpClass);
    }

    // Step 6: timestamp inside attestation validity window.
    if op.timestamp < attestation.valid_from || op.timestamp > attestation.valid_until {
        return Err(ValidationError::OutsideValidityWindow);
    }

    // Step 7: monotonic seq.
    let last = channel_state
        .last_seq_per_author
        .get(&op.author)
        .copied();
    if let Some(prev) = last
        && op.seq <= prev
    {
        return Err(ValidationError::ReplaySeq {
            seen: prev,
            incoming: op.seq,
        });
    }

    // Step 8: clearance gate.
    let required = effective_clearance(channel_state, op.artifact);
    if let Some(required) = required {
        let found = highest_cleared_at(
            channel_state,
            op.author,
            op.timestamp,
            &channel_state.accepted_trust_roots,
        );
        match found {
            Some((clearance, _)) if clearance >= required => {
                // OK
            }
            Some((clearance, issuer)) => {
                // We did find a claim but it's either below required
                // or from an untrusted issuer. The
                // highest_cleared_at function only returns claims
                // from accepted trust roots, so if we got here the
                // level is too low. Surface the gap as
                // InsufficientClearance.
                let _ = issuer;
                return Err(ValidationError::InsufficientClearance {
                    required,
                    found: Some(clearance),
                });
            }
            None => {
                // Either no `ClearedAt` claim at all, or all claims
                // are from untrusted issuers. To distinguish for the
                // audit log we re-scan and check whether *any*
                // ClearedAt claim exists from an untrusted issuer.
                if let Some(bad_issuer) =
                    untrusted_cleared_at_issuer(channel_state, op.author, op.timestamp)
                {
                    return Err(ValidationError::UntrustedClearanceIssuer { issuer: bad_issuer });
                }
                return Err(ValidationError::InsufficientClearance {
                    required,
                    found: None,
                });
            }
        }
    }

    // All checks passed. Bump replay sequence.
    channel_state
        .last_seq_per_author
        .insert(op.author, op.seq);
    Ok(())
}

/// Compute the effective classification floor for an artifact:
/// `max(channel.min_clearance, artifact.classification)`. Returns
/// `None` if neither is set.
fn effective_clearance(state: &ChannelState, artifact: ArtifactId) -> Option<Clearance> {
    let channel = state.channel_min_clearance;
    let artifact = state.artifact_classifications.get(&artifact).copied();
    match (channel, artifact) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Return the highest `ClearedAt` clearance held by `author` at
/// `at`, issued by a key in `accepted` — plus the issuer key for
/// audit logging.
fn highest_cleared_at(
    state: &ChannelState,
    author: Ed25519PublicKey,
    at: SystemTime,
    accepted: &[Ed25519PublicKey],
) -> Option<(Clearance, Ed25519PublicKey)> {
    let attestations = state.identity_attestations.get(&author)?;
    let mut best: Option<(Clearance, Ed25519PublicKey)> = None;
    for att in attestations {
        if !accepted.contains(&att.issued_by) {
            continue;
        }
        if at < att.valid_from || at > att.valid_until {
            continue;
        }
        for claim in &att.claims {
            if let Claim::ClearedAt(c) = claim
                && best.map(|(b, _)| c > &b).unwrap_or(true)
            {
                best = Some((*c, att.issued_by));
            }
        }
    }
    best
}

/// Find a `ClearedAt` claim that exists but was issued by a
/// non-accepted trust root. Used to distinguish
/// `UntrustedClearanceIssuer` from `InsufficientClearance` in the
/// error path.
fn untrusted_cleared_at_issuer(
    state: &ChannelState,
    author: Ed25519PublicKey,
    at: SystemTime,
) -> Option<Ed25519PublicKey> {
    let attestations = state.identity_attestations.get(&author)?;
    for att in attestations {
        if state.accepted_trust_roots.contains(&att.issued_by) {
            continue;
        }
        if at < att.valid_from || at > att.valid_until {
            continue;
        }
        for claim in &att.claims {
            if matches!(claim, Claim::ClearedAt(_)) {
                return Some(att.issued_by);
            }
        }
    }
    None
}
