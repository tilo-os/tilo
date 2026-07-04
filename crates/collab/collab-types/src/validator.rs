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
use std::collections::HashSet;
use std::collections::VecDeque;
use std::time::SystemTime;

use thiserror::Error;

use crate::channel::ArtifactId;
use crate::compaction::{CompactionCheckpoint, CompactionError, hash_op_prefix};
use crate::crypto::{Ed25519PublicKey, VerifyError, verify_event, verify_op};
use crate::event::RoleAttestation;
use crate::identity::{Attestation, AttestationId, Claim, Clearance};
use crate::op::SignedOp;
use crate::redaction::{OpId, RedactRequest, RedactedMarker, RedactionError, parse_redact_payload};
use crate::role::{ArtifactKind, OpClass, role_permits};

/// Default upper bound on the per-channel in-memory op log. Once the
/// log exceeds this size the oldest op is evicted on the next push.
/// 4096 strikes a balance between "enough scrollback to satisfy a
/// rejoin" and "won't OOM a long-lived channel". The host's
/// compaction-checkpoint cadence (every 200 ops in the spec) sits well
/// below this so under normal operation the bound is irrelevant; it's
/// a backstop for hosts that have gone silent.
pub const DEFAULT_MAX_IN_MEMORY_OPS: usize = 4096;

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

    /// Step 5b (Phase 9): a `Redact` op's payload could not be parsed
    /// or violated a per-payload invariant (e.g. `reason` too long,
    /// `redacted_by` does not match the envelope's author). Reported
    /// here rather than in a separate decode pass so audit-log entries
    /// for malformed Redacts land in the same `ValidationError` bucket
    /// as every other invalid op.
    #[error("malformed redact payload: {0}")]
    MalformedRedact(#[source] RedactionError),

    /// Step 5c (Phase 9): the `Redact` op's `target_op_id` resolves to
    /// another op that has itself been redacted. Recursive redaction
    /// is rejected — see the design doc §6 for the rationale ("un-
    /// redact is not a concept").
    #[error("recursive redaction: target op id {target:?} is itself a Redact")]
    RecursiveRedaction {
        /// The id of the op the rejected Redact attempted to target.
        target: OpId,
    },

    /// Step 9 (Phase 9): the inbound op's id is already in the
    /// channel's `redacted_ops` set. The op-log entry is rewritten to
    /// the redacted marker (so the seq gap doesn't form) but the
    /// derived state is **not** updated and the validator surfaces
    /// this error so the broker can audit-log a `OpArrivedRedacted`
    /// entry.
    #[error("op {target:?} was already redacted; payload swapped for marker")]
    OpAlreadyRedacted {
        /// The id of the op that was already in the redacted set.
        target: OpId,
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
#[derive(Clone, Debug)]
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

    /// Phase 6: in-memory op log for the channel, bounded by
    /// `max_in_memory_ops`. Validated ops are pushed in arrival order
    /// (typically `seq` order but not guaranteed — out-of-order
    /// arrivals stay where they land). When the log exceeds capacity
    /// the oldest entry is evicted on the next push; when a
    /// compaction checkpoint is applied, the matching prefix is
    /// evicted en masse. The log lets a late-joining peer fetch
    /// recent history via `CollabRequest::FetchScrollback` without
    /// triggering a peer-to-peer cold fetch.
    pub op_log: VecDeque<SignedOp>,

    /// Upper bound on `op_log`. Defaults to
    /// [`DEFAULT_MAX_IN_MEMORY_OPS`]. Configurable per channel because
    /// a chat-log artifact and a kanban artifact have very different
    /// op cadences; a single global default would either OOM the
    /// chat or starve the kanban.
    pub max_in_memory_ops: usize,

    /// Phase 9: the set of op ids that have been redacted on this
    /// channel. Used by [`ChannelState::is_redacted`] +
    /// [`validate_inbound_op`]'s step-9 redaction-aware filter.
    ///
    /// The set survives across compaction (a redaction record is
    /// retained even when the op it points to has been evicted; see
    /// §4 of the design doc). It is in-memory only in v1 — Phase 9.5
    /// adds persistence to the channel's authoritative event log so
    /// a cold-start peer can re-derive the set without re-running the
    /// full op stream.
    pub redacted_ops: HashSet<OpId>,

    /// Phase 9: snapshot of the **pre-redaction full envelope** of
    /// every redacted op, keyed by [`OpId`]. The audit log uses these
    /// to reconstruct the original `target_op_hash` long after the
    /// op-log entry has been rewritten to the redacted marker; the
    /// compaction path also consults them so the prefix hash stays
    /// stable across redactions (see §4 of the design doc — hash
    /// original bytes, not marker bytes).
    ///
    /// Bounded by the redaction set, which is small by construction
    /// (manual compliance acts, not bulk traffic).
    pub original_envelopes: HashMap<OpId, Box<SignedOp>>,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            attestations: HashMap::new(),
            authorised_signers: Vec::new(),
            identity_attestations: HashMap::new(),
            accepted_trust_roots: Vec::new(),
            artifacts: HashMap::new(),
            artifact_classifications: HashMap::new(),
            channel_min_clearance: None,
            last_seq_per_author: HashMap::new(),
            op_log: VecDeque::new(),
            max_in_memory_ops: DEFAULT_MAX_IN_MEMORY_OPS,
            redacted_ops: HashSet::new(),
            original_envelopes: HashMap::new(),
        }
    }
}

impl ChannelState {
    /// Convenience helper: register a role-attestation in both
    /// the per-id map and (if the issuer isn't already known) as
    /// an authorised signer. Used heavily by tests.
    pub fn register_attestation(&mut self, ra: RoleAttestation) {
        self.attestations.insert(ra.attestation.id, ra);
    }

    /// Phase 6: record a validated op into the in-memory log.
    ///
    /// Kept as a separate method from [`validate_inbound_op`] so the
    /// validator stays pure (no log mutation on a successful
    /// validation — callers opt in by calling this afterwards). The
    /// broker's hot path calls it immediately after a green validation;
    /// pure-validator unit tests don't call it at all.
    ///
    /// If the log is at capacity, the oldest entry is evicted before
    /// the new one is pushed. v1 evicts strictly by arrival order
    /// (FIFO); when compaction lands on a prefix, that supersedes
    /// the FIFO bound by chopping a larger range in one shot.
    pub fn record_validated_op(&mut self, op: SignedOp) {
        if self.op_log.len() >= self.max_in_memory_ops && self.max_in_memory_ops > 0 {
            self.op_log.pop_front();
        }
        self.op_log.push_back(op);
    }

    /// Phase 6: apply a host-emitted compaction checkpoint to this
    /// channel's op log.
    ///
    /// Returns the number of ops evicted on success. Failure modes:
    /// - [`CompactionError::PrefixHashMismatch`]: the hash recomputed
    ///   from the local log's prefix-up-to-`up_to_seq` does NOT match
    ///   the checkpoint's `prefix_hash`. The log is left untouched.
    ///
    /// The matching set is "every op in `op_log` with
    /// `op.artifact == checkpoint.artifact && op.seq <= checkpoint.up_to_seq`".
    /// Ops on other artifacts are unaffected (the broker keeps a
    /// single log per artifact in v1, but the validator's contract is
    /// "filter on artifact" so a future shared-log design doesn't
    /// silently break).
    ///
    /// Ordering: the prefix hash is computed over the matching ops
    /// sorted by `seq` ascending. A peer whose log is in `seq` order
    /// (the broker's common case) sees zero overhead; an out-of-order
    /// log pays a `Vec::sort_by_key` cost — negligible at v1 scale
    /// (≤ 4096 ops).
    pub fn apply_compaction(
        &mut self,
        checkpoint: &CompactionCheckpoint,
    ) -> Result<usize, CompactionError> {
        // Gather + sort the candidate prefix. For Phase 9 we substitute
        // the **pre-redaction** envelope when an op has been redacted:
        // the prefix hash is canonically computed over the original
        // bytes so a peer that received a Redact before a Compaction
        // still verifies the checkpoint correctly (design doc §4 —
        // hash original bytes, not marker bytes).
        let mut prefix: Vec<SignedOp> = self
            .op_log
            .iter()
            .filter(|op| {
                op.artifact == checkpoint.artifact && op.seq <= checkpoint.up_to_seq
            })
            .map(|op| {
                let id = OpId::from_op(op);
                if self.redacted_ops.contains(&id)
                    && let Some(original) = self.original_envelopes.get(&id)
                {
                    (**original).clone()
                } else {
                    op.clone()
                }
            })
            .collect();
        prefix.sort_by_key(|op| op.seq);

        let recomputed = hash_op_prefix(&prefix).map_err(CompactionError::Malformed)?;
        if recomputed != checkpoint.prefix_hash {
            return Err(CompactionError::PrefixHashMismatch);
        }

        // Evict in place. Redact ops themselves survive compaction —
        // they carry the audit record for a removal that may need to
        // be visible long after the targeted op is gone (design doc §4).
        let before = self.op_log.len();
        self.op_log.retain(|op| {
            !(op.artifact == checkpoint.artifact
                && op.seq <= checkpoint.up_to_seq
                && op.op_class != OpClass::Redact)
        });
        Ok(before - self.op_log.len())
    }

    /// Phase 9: is this op-id in the channel's `redacted_ops` set?
    ///
    /// Cheap query consulted by the broker before fanning a freshly-
    /// arrived op out to local apps + by the validator's step-9 to
    /// rewrite a late-arriving op's payload to the redacted marker.
    pub fn is_redacted(&self, op_id: OpId) -> bool {
        self.redacted_ops.contains(&op_id)
    }

    /// Phase 9: apply a parsed [`RedactRequest`] to this channel's
    /// state.
    ///
    /// On success:
    /// - inserts `req.target_op_id` into `redacted_ops`;
    /// - walks `op_log` and, for every matching entry, snapshots the
    ///   pre-redaction signing-bytes into `original_signing_bytes`
    ///   (keyed by id), then rewrites the entry's `payload` to a
    ///   [`RedactedMarker::v1`] canonical-bytes blob;
    /// - returns the number of log entries that were rewritten. Zero
    ///   is valid — the op may have been compacted away before the
    ///   redaction landed, or may arrive later (the validator's step
    ///   9 catches that).
    ///
    /// On failure: [`RedactionError::RedactorMismatch`] or
    /// [`RedactionError::Malformed`] surface verbatim from the caller's
    /// pre-parsed request — `apply_redaction` does not re-parse;
    /// callers run [`parse_redact_payload`] first.
    ///
    /// The function does **not** add the Redact op itself to
    /// `op_log` — the broker's hot path records the validated op
    /// before calling here so this method only handles the
    /// derived-state mutation.
    pub fn apply_redaction(&mut self, req: &RedactRequest) -> Result<usize, RedactionError> {
        let target = req.target_op_id;
        self.redacted_ops.insert(target);
        let marker_bytes = RedactedMarker::v1()
            .canonical()
            .map_err(RedactionError::Malformed)?;
        let mut rewritten = 0usize;
        for entry in self.op_log.iter_mut() {
            if OpId::from_op(entry) == target && entry.op_class != OpClass::Redact {
                // Snapshot the pre-redaction full envelope so the audit
                // log can verify the original `target_op_hash` and the
                // compaction path can keep its prefix hash stable.
                // Only the first sighting matters — replays of the
                // same target are idempotent.
                self.original_envelopes
                    .entry(target)
                    .or_insert_with(|| Box::new(entry.clone()));
                entry.payload = marker_bytes.clone();
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Phase 6: collect up to `max` ops from the in-memory log whose
    /// `artifact` matches and `seq < before_seq`, returned in
    /// ascending seq order. Used by the broker's `FetchScrollback`
    /// handler.
    ///
    /// Also returns whether the log is "exhausted" for that artifact —
    /// i.e. whether the caller has now seen every op the broker
    /// holds for that artifact that's earlier than `before_seq`. The
    /// caller (the requesting peer's broker) uses that flag to
    /// decide whether to fall back to a peer-to-peer fetch.
    pub fn scrollback_for(
        &self,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) -> (Vec<SignedOp>, bool) {
        let mut matching: Vec<SignedOp> = self
            .op_log
            .iter()
            .filter(|op| op.artifact == artifact && op.seq < before_seq)
            .cloned()
            .collect();
        matching.sort_by_key(|op| op.seq);
        let total = matching.len();
        if max == 0 {
            // Caller asked for zero ops. `exhausted` is then "is the
            // requested range empty?".
            return (Vec::new(), total == 0);
        }
        let max_usize = max as usize;
        // Take the highest-seq window — late joiners scroll backwards
        // from "before this seq" so the most recent ops are the most
        // useful first batch.
        let start = total.saturating_sub(max_usize);
        let ops: Vec<SignedOp> = matching[start..].to_vec();
        let exhausted = ops.len() >= total;
        (ops, exhausted)
    }
}

/// Run the inbound-op validation pipeline against the given
/// snapshot. On success, `channel_state.last_seq_per_author` is
/// updated for the op's author so subsequent ops with the same or
/// earlier seq are rejected.
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
/// 5b. **Phase 9, Redact-specific:** if `op.op_class == Redact`,
///     parse the payload as a `RedactRequest`; reject recursive
///     redactions (target id resolves to another Redact in the
///     local op log).
/// 6. `op.timestamp ∈ [attestation.valid_from, attestation.valid_until]`.
/// 7. `op.seq > last_seq_per_author[op.author]`.
/// 8. Clearance gate: if the artifact's effective classification
///    is set, the author needs a matching `ClearedAt` claim
///    issued by an accepted trust root.
/// 9. **Phase 9 redaction-aware filter:** if the op's derived
///    [`OpId`] is already in `channel_state.redacted_ops`, return
///    [`ValidationError::OpAlreadyRedacted`]. The broker treats this
///    specially — pushes a redacted-marker entry into the op log
///    and audit-logs an `OpArrivedRedacted` event — rather than
///    fanning the op out.
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

    // Step 5b (Phase 9): if this is a Redact op, parse the payload and
    // reject recursive redactions. Parsing also catches the
    // envelope-author / payload-redactor mismatch + the reason-length
    // bound; the validator surfaces those as `MalformedRedact` so the
    // audit log preserves the specific reason.
    if op.op_class == OpClass::Redact {
        let req = parse_redact_payload(op).map_err(ValidationError::MalformedRedact)?;
        // Recursive-redaction check. Walk the in-memory op log for an
        // entry whose derived OpId matches `req.target_op_id` and whose
        // op_class is itself `Redact` — that would be a redact-of-a-
        // redact, which we disallow. The walk is bounded by
        // max_in_memory_ops; we don't consult original_envelopes here
        // because a previously-redacted op (whose op_class was also
        // Redact, were that allowed) would have been refused at this
        // very check on first apply, so by induction the cache never
        // contains a Redact.
        for entry in channel_state.op_log.iter() {
            if entry.op_class == OpClass::Redact && OpId::from_op(entry) == req.target_op_id {
                return Err(ValidationError::RecursiveRedaction {
                    target: req.target_op_id,
                });
            }
        }
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

    // Step 9 (Phase 9): redaction-aware filter. An op whose derived
    // OpId is in `redacted_ops` is the late-arrival case described in
    // §7 of the design doc — the redaction reached this peer before
    // the op did, and now the op shows up. The validator surfaces
    // this as a typed error so the broker can:
    //   - record the envelope in the audit log as `OpArrivedRedacted`,
    //   - push a marker-payload entry into the op log so the seq gap
    //     doesn't form,
    //   - refuse to fan out / apply.
    //
    // We don't bump `last_seq_per_author` on this path: failing
    // validations leave the seq counter alone (mirrors every other
    // error variant). A re-broadcast of the same op hits the same
    // check on the next pass, which is the intended idempotent
    // behaviour.
    //
    // Redact ops themselves are exempt from this check: a Redact-of-a-
    // Redact would land on step 5b, and a Redact's own id is never in
    // `redacted_ops` by construction (it would mean redacting a
    // redaction, which the recursive-redaction rule already forbade).
    if op.op_class != OpClass::Redact {
        let id = OpId::from_op(op);
        if channel_state.redacted_ops.contains(&id) {
            return Err(ValidationError::OpAlreadyRedacted { target: id });
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
