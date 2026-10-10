//! The inbound-op validation pipeline, implementing the validation steps
//! specified in `2026-06-29-spaces-channels-and-cryptographic-roles.md`.
//! Each step has a dedicated [`ValidationError`] variant so callers can
//! pattern-match the failure and surface a precise reason in the audit log.
//!
//! The validator is pure: it takes a [`ChannelState`] snapshot and a
//! `SignedOp`, and returns `Ok(())` or a structured error. The only side
//! effect — bumping the last-seen sequence number on success — is applied
//! to the borrowed `&mut ChannelState`; callers that want to
//! validate-without-applying pass a clone and discard it.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use web_time::{Duration, SystemTime};

use thiserror::Error;

use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Grant, Party, Subject, TreePath, roles,
};

use crate::channel::ArtifactId;
use crate::compaction::{CompactionCheckpoint, CompactionError, hash_op_prefix};
use crate::crypto::{Ed25519PublicKey, VerifyError, verify_event, verify_op};
use crate::event::RoleAttestation;
use crate::identity::{Attestation, AttestationId, Claim, Clearance};
use crate::op::SignedOp;
use crate::redaction::{OpId, RedactRequest, RedactedMarker, RedactionError, parse_redact_payload};
use crate::role::{ArtifactKind, OpKind, Role};

/// Default upper bound on the per-channel in-memory op log; the oldest op
/// is evicted on overflow. 4096 balances "enough scrollback to satisfy a
/// rejoin" against OOM risk; the host's compaction cadence (every 200 ops)
/// sits well below this, so the bound is a backstop for silent hosts.
pub const DEFAULT_MAX_IN_MEMORY_OPS: usize = 4096;

/// Errors produced by the inbound-op validator.
///
/// Each variant maps to one of the eight numbered validation steps in the
/// spec (`Crypto` covers signature verification). Variants carry enough
/// context for a structured audit entry — the spec's `OpRejected` event
/// uses the `Display` form as the reason string.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// Step 1: the op references an attestation id the validator has never
    /// seen. Production lazy-fetches and defers; the validator just rejects.
    #[error("unknown attestation id: {0:?}")]
    UnknownAttestation(AttestationId),

    /// Step 2: the attestation was signed by a key not in the channel's
    /// `authorised_signers` — either forged, or its delegation chain isn't
    /// visible to the validator yet.
    #[error("attestation issuer {0:?} is not an authorised channel signer")]
    UnauthorisedAttestation(Ed25519PublicKey),

    /// Step 3: `SignedOp::signature` did not verify against `SignedOp::author`'s
    /// public key — tampered post-signing, or a forgery attempt.
    #[error("op signature did not verify: {0}")]
    BadOpSignature(#[source] VerifyError),

    /// Step 2 sub-case: the attestation's own signature did not verify
    /// against its claimed issuer. Distinct from step 3 for triage.
    #[error("attestation signature did not verify: {0}")]
    BadAttestationSignature(#[source] VerifyError),

    /// Step 4: `op.author` and `attestation.subject` disagree — commonly,
    /// replaying someone else's attestation onto your own op.
    #[error("op author does not match attestation subject")]
    AuthorMismatch,

    /// Step 5: the author's grants do not allow the change this op makes. An
    /// Ask answer is a refusal here: nobody can approve on the wire.
    #[error("the author is not granted this change: {0}")]
    NotGranted(String),

    /// Step 5: the change's app surface is not here yet, so its claims
    /// cannot be decided: keep the op and decide it once the surface is.
    #[error("the app surface this change is decided against is not here yet")]
    SurfaceUnavailable,

    /// The signed document envelope does not match the app/workspace/record
    /// route on which it arrived, or does not carry the typed document
    /// operation shape required by that route.
    #[error("signed document envelope does not match its collaboration route")]
    DocumentRouteMismatch,

    /// Step 5 sub-case: `op.artifact` is unknown to the validator — without
    /// it the op has no place in the tree, so we reject.
    #[error("unknown artifact id: {0:?}")]
    UnknownArtifact(ArtifactId),

    /// Step 6: the op's `timestamp` is outside the attestation's validity
    /// window — expired, pre-dates `valid_from`, or a clock-skew attack.
    #[error("op timestamp is outside attestation validity window")]
    OutsideValidityWindow,

    /// Step 7: the op's `seq` is not strictly greater than the last-seen seq
    /// for the same author — replay or out-of-order delivery.
    #[error("replay or out-of-order seq: {seen:?} >= incoming {incoming}")]
    ReplaySeq {
        /// The last-seen seq for this author, prior to this op.
        seen: u64,
        /// The seq carried by this op.
        incoming: u64,
    },

    /// Step 8a: artifact requires clearance but the author has no `ClearedAt`
    /// claim meeting the floor.
    #[error("insufficient clearance: required {required:?}, author has {found:?}")]
    InsufficientClearance {
        /// The artifact-level (or channel-level, whichever is
        /// stricter) clearance floor.
        required: Clearance,
        /// The highest `ClearedAt` claim found for this author, or
        /// `None` if no such claim exists.
        found: Option<Clearance>,
    },

    /// Step 8b: the author has a `ClearedAt` claim but its issuer isn't in
    /// `accepted_trust_roots` — we refuse a self-signed clearance.
    #[error("clearance claim was issued by {issuer:?}, which is not in the accepted trust roots")]
    UntrustedClearanceIssuer {
        /// The issuer key on the rejected `ClearedAt` claim.
        issuer: Ed25519PublicKey,
    },

    /// Step 5b: a `Redact` op's payload failed to parse or violated a
    /// per-payload invariant. Reported here rather than a separate decode
    /// pass so malformed Redacts land in the same error bucket as any other
    /// invalid op.
    #[error("malformed redact payload: {0}")]
    MalformedRedact(#[source] RedactionError),

    /// Step 5c: the `Redact` op's `target_op_id` resolves to an already-redacted
    /// op. Recursive redaction is rejected — "un-redact is not a concept".
    #[error("recursive redaction: target op id {target:?} is itself a Redact")]
    RecursiveRedaction {
        /// The id of the op the rejected Redact attempted to target.
        target: OpId,
    },

    /// Step 9: the inbound op's id is already in `redacted_ops`. The op-log
    /// entry is rewritten to the redacted marker (so no seq gap forms), but
    /// derived state is **not** updated; the error lets the broker audit-log
    /// `OpArrivedRedacted`.
    #[error("op {target:?} was already redacted; payload swapped for marker")]
    OpAlreadyRedacted {
        /// The id of the op that was already in the redacted set.
        target: OpId,
    },
}

/// The room a [`ChannelState::default`] names, until a host sets its own.
pub const DEFAULT_ROOM: &str = "channel";

/// The validator's snapshot of a channel.
///
/// Built by the broker (or test harness) from cheap accessors, handed in
/// by value. Only `last_seq_per_author` is mutated on success; everything
/// else is read-only. Fields are `pub` because this is a foundation crate
/// downstream code composes in ways we can't predict.
#[derive(Clone, Debug)]
pub struct ChannelState {
    /// Channel-local attestations, keyed by id ("member has role X" grants).
    /// Identity-level attestations (clearance) live separately in
    /// `identity_attestations` — different trust roots, different lifetimes.
    pub attestations: HashMap<AttestationId, RoleAttestation>,

    /// Public keys authorised to issue channel-local attestations. Always
    /// includes the channel owner, plus each `Owner`-role delegation.
    pub authorised_signers: Vec<Ed25519PublicKey>,

    /// Identity-level attestations per known public key (clearance,
    /// employment, …). Used by step 8 to find a `ClearedAt` claim.
    pub identity_attestations: HashMap<Ed25519PublicKey, Vec<Attestation>>,

    /// Trust roots accepted on identity-level attestations. Distinct from
    /// `authorised_signers` — the channel owner is rarely the org's security officer.
    pub accepted_trust_roots: Vec<Ed25519PublicKey>,

    /// Artifact-kind lookup keyed by artifact id, required for step 5.
    pub artifacts: HashMap<ArtifactId, ArtifactKind>,

    /// The app that owns each artifact, if known: the artifact's place in
    /// the room's tree is `room › app › resource`.
    pub artifact_apps: HashMap<ArtifactId, String>,

    /// The record each artifact is, if known: the artifact's node is
    /// `room › app › record`; otherwise the artifact id stands in.
    pub artifact_records: HashMap<ArtifactId, String>,

    /// The record kind each artifact is, if known: which of its app's
    /// resources the record's changes are decided against.
    pub artifact_kinds: HashMap<ArtifactId, String>,

    /// The room this channel is, as the tree names it.
    pub room: String,

    /// The app surfaces changes are decided against by their claims;
    /// without a source, a change is decided as an edit of its record.
    pub surfaces: crate::receipt::Surfaces,

    /// The room's grants, role presets included. Step 5 decides with them.
    pub grants: Vec<Grant>,

    /// Per-artifact classification override, composed with the channel floor
    /// via `max(channel_min, artifact_classification)`.
    pub artifact_classifications: HashMap<ArtifactId, Clearance>,

    /// The channel's `min_clearance` floor.
    pub channel_min_clearance: Option<Clearance>,

    /// Last-seen sequence number per author (step-7 replay defence). Updated
    /// in-place on success; non-destructive callers pass `&mut clone()`.
    pub last_seq_per_author: HashMap<Ed25519PublicKey, u64>,

    /// In-memory op log, bounded by `max_in_memory_ops`, pushed in arrival
    /// order. Oldest entry evicted on overflow, or a matching prefix chopped
    /// in one shot on compaction. Lets a late-joining peer fetch recent
    /// history via `CollabRequest::FetchScrollback`.
    pub op_log: VecDeque<SignedOp>,

    /// Upper bound on `op_log`, defaults to [`DEFAULT_MAX_IN_MEMORY_OPS`].
    /// Configurable per channel — a chat artifact and a kanban artifact have
    /// very different op cadences.
    pub max_in_memory_ops: usize,

    /// Op ids redacted on this channel, consulted by
    /// [`ChannelState::is_redacted`] and the validator's redaction filter.
    /// Survives compaction — a redaction record outlives the op it points to.
    /// In-memory only; not yet persisted to the authoritative event log.
    pub redacted_ops: HashSet<OpId>,

    /// Pre-redaction full envelope of every redacted op, keyed by [`OpId`].
    /// The audit log reconstructs the original `target_op_hash` from these;
    /// compaction consults them so the prefix hash stays stable across
    /// redactions (hash original bytes, not marker bytes). Bounded by the
    /// redaction set, small by construction.
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
            artifact_apps: HashMap::new(),
            artifact_records: HashMap::new(),
            artifact_kinds: HashMap::new(),
            room: DEFAULT_ROOM.to_owned(),
            surfaces: crate::receipt::Surfaces::default(),
            grants: roles::room_role_grants(DEFAULT_ROOM, &Subject::Role(Role::Owner)),
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
    /// Register a role-attestation in both the per-id map and (if the issuer
    /// isn't known) as an authorised signer. Used heavily by tests.
    pub fn register_attestation(&mut self, ra: RoleAttestation) {
        self.attestations.insert(ra.attestation.id, ra);
    }

    /// Record a validated op into the in-memory log.
    ///
    /// Kept separate from [`validate_inbound_op`] so the validator stays pure
    /// — callers opt into logging by calling this afterwards. The oldest
    /// entry is evicted on overflow (FIFO in v1); a compaction checkpoint
    /// supersedes the FIFO bound by chopping a larger range in one shot.
    pub fn record_validated_op(&mut self, op: SignedOp) {
        if self.op_log.len() >= self.max_in_memory_ops && self.max_in_memory_ops > 0 {
            self.op_log.pop_front();
        }
        self.op_log.push_back(op);
    }

    /// The host's op-time enforcement gate: validate an inbound op and,
    /// **only if** it passes, apply it to the op log fan-out reads from.
    ///
    /// The real "apply-and-relay" path a host runs for every inbound signed
    /// op — not a test helper. It couples the [`validate_inbound_op`] gate to
    /// [`ChannelState::record_validated_op`] so "reject at validation ⇒ not
    /// applied, not relayed" holds by construction:
    ///
    /// - `Ok`: the op is pushed to `op_log` — recording is the apply *and*
    ///   the pre-condition for relay.
    /// - `Err`: nothing is recorded and `last_seq_per_author` is untouched.
    ///
    /// Returns the same [`ValidationError`] the validator would, for audit logging.
    ///
    /// # Errors
    ///
    /// Returns the first [`ValidationError`] the validation pipeline hits;
    /// see [`validate_inbound_op`] for the step ordering.
    pub fn apply_and_relay(&mut self, op: &SignedOp) -> Result<(), ValidationError> {
        validate_inbound_op(op, self)?;
        self.record_validated_op(op.clone());
        Ok(())
    }

    /// Apply a host-emitted compaction checkpoint to this channel's op log.
    ///
    /// Returns the number of ops evicted. Fails with
    /// [`CompactionError::PrefixHashMismatch`] if the hash recomputed from
    /// the local log's prefix-up-to-`up_to_seq` doesn't match the
    /// checkpoint's — the log is left untouched then.
    ///
    /// Matches every op with `op.artifact == checkpoint.artifact &&
    /// op.seq <= checkpoint.up_to_seq`; other artifacts are unaffected (the
    /// validator filters on artifact so a future shared-log design doesn't
    /// silently break).
    ///
    /// The prefix hash is computed over matching ops sorted by `seq`
    /// ascending — zero overhead for an in-order log, a negligible sort cost
    /// otherwise (≤ 4096 ops at v1 scale).
    ///
    /// # Errors
    ///
    /// Returns [`CompactionError::PrefixHashMismatch`] if the locally
    /// recomputed prefix hash doesn't match the checkpoint's, or
    /// [`CompactionError::Malformed`] if hashing the prefix itself fails.
    pub fn apply_compaction(
        &mut self,
        checkpoint: &CompactionCheckpoint,
    ) -> Result<usize, CompactionError> {
        // Gather + sort the candidate prefix, substituting the
        // **pre-redaction** envelope for a redacted op — the prefix hash is
        // computed over original bytes so a peer that saw a Redact before a
        // Compaction still verifies the checkpoint (hash original, not marker, bytes).
        let mut prefix: Vec<SignedOp> = self
            .op_log
            .iter()
            .filter(|op| op_in_checkpoint_prefix(op, checkpoint))
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

        // Evict in place. Redact ops themselves survive compaction — they
        // carry the audit record for a removal that may need to stay visible
        // after the targeted op is gone.
        let before = self.op_log.len();
        self.op_log.retain(|op| {
            !(op_in_checkpoint_prefix(op, checkpoint) && op.kind != OpKind::Redaction)
        });
        Ok(before.saturating_sub(self.op_log.len()))
    }

    /// Is this op-id in `redacted_ops`? Consulted by the broker before
    /// fanning a freshly-arrived op out, and by the validator's step 9 to
    /// rewrite a late-arriving op's payload to the redacted marker.
    pub fn is_redacted(&self, op_id: OpId) -> bool {
        self.redacted_ops.contains(&op_id)
    }

    /// Apply a parsed [`RedactRequest`] to this channel's state.
    ///
    /// On success: inserts `req.target_op_id` into `redacted_ops`; walks
    /// `op_log` and, for every matching entry, snapshots the pre-redaction
    /// signing-bytes into `original_signing_bytes`, then rewrites the
    /// entry's `payload` to a [`RedactedMarker::v1`] blob; returns the number
    /// of entries rewritten (zero is valid — compacted away, or arriving later).
    ///
    /// On failure, [`RedactionError`] surfaces verbatim from the caller's
    /// pre-parsed request — this does not re-parse; callers run
    /// [`parse_redact_payload`] first.
    ///
    /// Does **not** add the Redact op itself to `op_log` — the broker's hot
    /// path records the validated op before calling here.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::Malformed`] if serialising the redacted
    /// marker payload fails.
    pub fn apply_redaction(&mut self, req: &RedactRequest) -> Result<usize, RedactionError> {
        let target = req.target_op_id;
        self.redacted_ops.insert(target);
        let marker_bytes = RedactedMarker::v1()
            .canonical()
            .map_err(RedactionError::Malformed)?;
        let mut rewritten: usize = 0;
        for entry in &mut self.op_log {
            if OpId::from_op(entry) == target && entry.kind != OpKind::Redaction {
                // Snapshot the pre-redaction envelope for audit + stable compaction
                // prefix hash. Only the first sighting matters — replays of the same
                // target are idempotent.
                self.original_envelopes
                    .entry(target)
                    .or_insert_with(|| Box::new(entry.clone()));
                entry.payload.clone_from(&marker_bytes);
                rewritten = rewritten.saturating_add(1);
            }
        }
        Ok(rewritten)
    }

    /// Collect up to `max` ops whose `artifact` matches and `seq <
    /// before_seq`, ascending seq order. Used by `FetchScrollback`.
    ///
    /// Also returns whether the log is exhausted for that artifact — the
    /// caller uses that to decide whether to fall back to a peer-to-peer fetch.
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
            // Caller asked for zero ops: `exhausted` is then "is the requested range empty?".
            return (Vec::new(), total == 0);
        }
        let max_usize = max as usize;
        // Take the highest-seq window — late joiners scroll backwards from
        // "before this seq", so the most recent ops are the most useful first batch.
        let start = total.saturating_sub(max_usize);
        let ops: Vec<SignedOp> = matching
            .get(start..)
            .map_or_else(Vec::new, <[SignedOp]>::to_vec);
        let exhausted = ops.len() >= total;
        (ops, exhausted)
    }
}

/// Run the inbound-op validation pipeline. On success,
/// `channel_state.last_seq_per_author` is updated so subsequent ops with
/// the same or earlier seq are rejected.
///
/// Returns the first failure — not all failures — since the audit log
/// only carries one reason string.
///
/// Steps: 1. resolve `op.attestation_id`; 2. verify the attestation's own
/// signature against an authorised signer; 3. verify the op's signature
/// against the author; 4. `op.author == attestation.subject`; 5.
/// `role_permits(role, kind, kind)`; 5a. capability overrides narrow
/// or extend the role baseline (fail-closed, see [`crate::caps::decide`]);
/// 5b. if `Redact`, parse the payload and reject recursive redaction; 6.
/// timestamp inside the attestation's validity window; 7. monotonic seq;
/// 8. clearance gate; 9. redaction-aware filter for late arrivals.
///
/// # Errors
///
/// Returns the first-failing step's [`ValidationError`] variant — see the
/// step list above and the variant docs for exactly when each fires.
pub fn validate_inbound_op(
    op: &SignedOp,
    channel_state: &mut ChannelState,
) -> Result<(), ValidationError> {
    // Steps 1-4: resolve + verify the attestation chain and op signature.
    let ra = verify_attestation_chain(op, channel_state)?;
    let attestation = &ra.attestation;

    // Step 6: timestamp inside attestation validity window.
    if op.timestamp < attestation.valid_from || op.timestamp > attestation.valid_until {
        return Err(ValidationError::OutsideValidityWindow);
    }

    let subjects = [
        Subject::Person(hex::encode(op.author.0)),
        Subject::Role(ra.role),
    ];
    let grants = channel_state.grants.clone();
    let mut author = Party::new(subjects.to_vec(), &grants);
    if ra.role == Role::Owner {
        author = author.owning(TreePath::room(&channel_state.room));
    }
    validate_authorized_op(op, &Authority::person(author), channel_state)
}

/// Validate an op whose author and role were established by a certified
/// device join plus the live workspace roster. This is the production
/// document path: certificate/admission validation happens at the broker
/// boundary, while this function preserves the shared signature, role table,
/// capability, freshness, replay, clearance, and redaction checks.
///
/// Unlike [`validate_inbound_op`], no channel role-attestation is resolved:
/// `author` is the author's authority from the room's live policy. `max_clock_skew`
/// replaces the attestation validity window and bounds how far an admitted
/// device may backdate or future-date an operation.
///
/// # Errors
///
/// Returns [`ValidationError::BadOpSignature`] for a forged envelope,
/// [`ValidationError::OutsideValidityWindow`] outside the clock-skew window,
/// or the same policy/replay/redaction errors as [`validate_inbound_op`].
pub fn validate_admitted_inbound_op(
    op: &SignedOp,
    author: &Authority<'_>,
    channel_state: &mut ChannelState,
    now: SystemTime,
    max_clock_skew: Duration,
) -> Result<(), ValidationError> {
    verify_op(op).map_err(ValidationError::BadOpSignature)?;
    let earliest = now
        .checked_sub(max_clock_skew)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(max_clock_skew).unwrap_or(now);
    if op.timestamp < earliest || op.timestamp > latest {
        return Err(ValidationError::OutsideValidityWindow);
    }

    validate_authorized_op(op, author, channel_state)
}

/// Validate an admitted op that a workspace host replayed from its retained,
/// host-ordered history (a late join, a reconnect, a compaction snapshot).
///
/// Identical to [`validate_admitted_inbound_op`] except that an op may be
/// arbitrarily old: history is old by nature, and the host admitted and
/// ordered it when it was fresh. An op dated more than `max_clock_skew` into
/// the future is still refused, and replay protection (per-author sequence,
/// redaction) is unchanged.
///
/// # Errors
///
/// As [`validate_admitted_inbound_op`], with
/// [`ValidationError::OutsideValidityWindow`] only for a future-dated op.
pub fn validate_replayed_inbound_op(
    op: &SignedOp,
    author: &Authority<'_>,
    channel_state: &mut ChannelState,
    now: SystemTime,
    max_clock_skew: Duration,
) -> Result<(), ValidationError> {
    verify_op(op).map_err(ValidationError::BadOpSignature)?;
    let latest = now.checked_add(max_clock_skew).unwrap_or(now);
    if op.timestamp > latest {
        return Err(ValidationError::OutsideValidityWindow);
    }
    validate_authorized_op(op, author, channel_state)
}

/// Checks shared by attestation-authorised and certified-roster-authorised
/// operations after their respective identity/time proofs have passed.
fn validate_authorized_op(
    op: &SignedOp,
    author: &Authority<'_>,
    channel_state: &mut ChannelState,
) -> Result<(), ValidationError> {
    // Step 5: the author's grants.
    check_op_permission(op, author, channel_state)?;

    // Step 5b: Redact payload parsing + recursive-redaction rejection.
    if op.kind == OpKind::Redaction {
        check_redact_payload(op, channel_state)?;
    }

    // Step 7: monotonic seq.
    let last = channel_state.last_seq_per_author.get(&op.author).copied();
    if let Some(prev) = last
        && op.seq <= prev
    {
        return Err(ValidationError::ReplaySeq {
            seen: prev,
            incoming: op.seq,
        });
    }

    // Step 8: clearance gate.
    check_clearance_gate(channel_state, op)?;

    // Step 9: redaction-aware filter. An op whose derived OpId
    // is in `redacted_ops` is the late-arrival case — the redaction
    // reached this peer before the op did. Surfaced as a typed error so
    // the broker can audit-log `OpArrivedRedacted`, push a marker entry
    // so no seq gap forms, and refuse to fan out/apply.
    //
    // `last_seq_per_author` is left alone on this path (like every other
    // error), so a re-broadcast hits the same check idempotently. Redact
    // ops themselves are exempt — a Redact-of-a-Redact lands on step 5b,
    // and a Redact's own id is never in `redacted_ops` by construction.
    if op.kind != OpKind::Redaction {
        let id = OpId::from_op(op);
        if channel_state.redacted_ops.contains(&id) {
            return Err(ValidationError::OpAlreadyRedacted { target: id });
        }
    }

    // All checks passed. Bump replay sequence.
    channel_state.last_seq_per_author.insert(op.author, op.seq);
    Ok(())
}

/// Steps 1-4: resolve the op's attestation, verify it traces to an
/// authorised channel signer, verify the op's own signature, and check the
/// author matches the attestation subject. Returns the resolved
/// [`RoleAttestation`] for the caller to reuse (validity window, role).
fn verify_attestation_chain(
    op: &SignedOp,
    channel_state: &ChannelState,
) -> Result<RoleAttestation, ValidationError> {
    // Step 1: resolve attestation.
    let ra = channel_state
        .attestations
        .get(&op.attestation_id)
        .ok_or(ValidationError::UnknownAttestation(op.attestation_id))?
        .clone();
    let attestation = &ra.attestation;

    // Step 2: verify the attestation traces to an authorised signer —
    // `issued_by` must be in `authorised_signers`, and its own signature
    // must verify against that key.
    if !channel_state
        .authorised_signers
        .contains(&attestation.issued_by)
    {
        return Err(ValidationError::UnauthorisedAttestation(
            attestation.issued_by,
        ));
    }
    let bytes = attestation
        .signing_bytes()
        .map_err(|e| ValidationError::BadAttestationSignature(VerifyError::Canonical(e)))?;
    verify_event(&bytes, &attestation.signature, &attestation.issued_by)
        .map_err(ValidationError::BadAttestationSignature)?;

    // Step 3: verify the op signature against the author's pubkey.
    verify_op(op).map_err(ValidationError::BadOpSignature)?;

    // Step 4: author matches attestation subject.
    if op.author != attestation.subject {
        return Err(ValidationError::AuthorMismatch);
    }

    Ok(ra)
}

/// The node of a collaborative record: `room › app › resource`.
#[must_use]
pub fn document_node(room: &str, app: &str, record: &str) -> TreePath {
    TreePath::room(room).app(app).resource(record)
}

/// The app segment of an artifact no app claims.
const UNKNOWN_APP: &str = "-";

/// Step 5: the change the op makes must be allowed by the author's grants.
/// The wire shares no clock with the author, so grants are judged without
/// expiry; the authoring device refused after expiry. Fail-closed: no grant,
/// or one that asks, refuses. Admitting only an outright Allow also means the
/// author's own grants allow it, which is all a privileged action needs
/// (`tilo_action_policy::Verdict::privileged`).
fn check_op_permission(
    op: &SignedOp,
    author: &Authority<'_>,
    channel_state: &ChannelState,
) -> Result<(), ValidationError> {
    if !channel_state.artifacts.contains_key(&op.artifact) {
        return Err(ValidationError::UnknownArtifact(op.artifact));
    }
    let artifact = op.artifact.0.to_string();
    let record = crate::receipt::Record {
        room: &channel_state.room,
        app: channel_state
            .artifact_apps
            .get(&op.artifact)
            .map_or(UNKNOWN_APP, String::as_str),
        record: channel_state
            .artifact_records
            .get(&op.artifact)
            .map_or(artifact.as_str(), String::as_str),
    };
    let allows = |node: &tilo_action_policy::TreePath, kind: ActionKind| {
        author.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    match crate::receipt::decide_op(
        op,
        &record,
        channel_state
            .artifact_kinds
            .get(&op.artifact)
            .map(String::as_str),
        None,
        &channel_state.surfaces,
        &allows,
    ) {
        Ok(crate::receipt::Receipt::Admit) => Ok(()),
        Ok(crate::receipt::Receipt::Hold) => Err(ValidationError::SurfaceUnavailable),
        Err(refusal) => Err(ValidationError::NotGranted(refusal.to_string())),
    }
}

/// Step 5b: if Redact, parse the payload and reject recursive
/// redactions. Parsing also catches the envelope-author/payload-redactor
/// mismatch and the reason-length bound, surfaced as `MalformedRedact`.
fn check_redact_payload(
    op: &SignedOp,
    channel_state: &ChannelState,
) -> Result<(), ValidationError> {
    let req = parse_redact_payload(op).map_err(ValidationError::MalformedRedact)?;
    // Recursive-redaction check: walk the op log for an entry whose
    // derived OpId matches `req.target_op_id` and is itself `Redact` —
    // disallowed. Bounded by max_in_memory_ops; original_envelopes isn't
    // consulted since a previously-redacted Redact would have been
    // refused here on first apply, so by induction the cache never holds one.
    for entry in &channel_state.op_log {
        if entry.kind == OpKind::Redaction && OpId::from_op(entry) == req.target_op_id {
            return Err(ValidationError::RecursiveRedaction {
                target: req.target_op_id,
            });
        }
    }
    Ok(())
}

/// Step 8: clearance gate — is the author's `ClearedAt` claim (if any) from
/// an accepted trust root and at or above the artifact's classification floor?
fn check_clearance_gate(
    channel_state: &ChannelState,
    op: &SignedOp,
) -> Result<(), ValidationError> {
    let Some(required) = effective_clearance(channel_state, op.artifact) else {
        return Ok(());
    };
    let found = highest_cleared_at(
        channel_state,
        op.author,
        op.timestamp,
        &channel_state.accepted_trust_roots,
    );
    match found {
        Some((clearance, _)) if clearance >= required => Ok(()),
        Some((clearance, issuer)) => {
            // Found a claim, but below the required level or from an
            // untrusted issuer — `highest_cleared_at` only returns accepted-root
            // claims, so reaching here means the level itself is too low.
            let _ = issuer;
            Err(ValidationError::InsufficientClearance {
                required,
                found: Some(clearance),
            })
        }
        None => {
            // No `ClearedAt` claim at all, or all from untrusted issuers — to
            // distinguish for the audit log, re-scan for any untrusted-issuer claim.
            if let Some(bad_issuer) =
                untrusted_cleared_at_issuer(channel_state, op.author, op.timestamp)
            {
                return Err(ValidationError::UntrustedClearanceIssuer { issuer: bad_issuer });
            }
            Err(ValidationError::InsufficientClearance {
                required,
                found: None,
            })
        }
    }
}

/// Does `op` fall within `checkpoint`'s prefix — same artifact, `seq` no
/// later than the checkpoint's `up_to_seq`? Shared by both the hash
/// recomputation and the eviction pass in [`ChannelState::apply_compaction`]
/// so the two stay in lock-step by construction.
fn op_in_checkpoint_prefix(op: &SignedOp, checkpoint: &CompactionCheckpoint) -> bool {
    let same_artifact = op.artifact == checkpoint.artifact;
    let within_prefix = op.seq <= checkpoint.up_to_seq;
    same_artifact && within_prefix
}

/// Effective classification floor for an artifact:
/// `max(channel.min_clearance, artifact.classification)`, `None` if neither is set.
fn effective_clearance(state: &ChannelState, artifact: ArtifactId) -> Option<Clearance> {
    let channel = state.channel_min_clearance;
    let artifact = state.artifact_classifications.get(&artifact).copied();
    match (channel, artifact) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Highest `ClearedAt` clearance held by `author` at `at`, issued by a
/// key in `accepted` — plus the issuer key for audit logging.
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
                && best.is_none_or(|(b, _)| c > &b)
            {
                best = Some((*c, att.issued_by));
            }
        }
    }
    best
}

/// Find a `ClearedAt` claim that exists but was issued by a non-accepted
/// trust root — distinguishes `UntrustedClearanceIssuer` from `InsufficientClearance`.
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
