//! The inbound-document validation pipeline: the checks a signed op passes
//! before a host records it and fans it to the apps on its route.
//!
//! The validator is pure: it takes an [`InboundState`] and a `SignedOp`, and
//! returns `Ok(())` or a structured error. The only side effect — bumping the
//! last-seen sequence number on success — is applied to the borrowed
//! `&mut InboundState`; callers that want to validate without applying pass a
//! clone and discard it.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use web_time::{Duration, SystemTime};

use thiserror::Error;

use tilo_action_policy::{ActionKind, Answer, Authority, Context, Grant, Grantee, TreePath, roles};

use crate::channel::ArtifactId;
use crate::compaction::{CompactionCheckpoint, CompactionError, hash_op_prefix};
use crate::crypto::{Ed25519PublicKey, VerifyError, verify_op};
use crate::op::SignedOp;
use crate::redaction::{OpId, RedactRequest, RedactedMarker, RedactionError, parse_redact_payload};
use crate::role::{ArtifactKind, OpKind, Role};

/// Default upper bound on the in-memory op log; the oldest op is evicted on
/// overflow. 4096 balances "enough scrollback to satisfy a rejoin" against
/// OOM risk; the host's compaction cadence (every 200 ops) sits well below
/// this, so the bound is a backstop for silent hosts.
pub const DEFAULT_MAX_IN_MEMORY_OPS: usize = 4096;

/// Errors produced by the inbound-op validator. Each variant carries enough
/// context for a structured audit entry — an op refused at ingress is logged
/// with its `Display` form as the reason.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// `SignedOp::signature` did not verify against `SignedOp::author` —
    /// tampered post-signing, or a forgery attempt.
    #[error("op signature did not verify: {0}")]
    BadOpSignature(#[source] VerifyError),

    /// The author's grants do not allow the change this op makes. An Ask
    /// answer is a refusal here: nobody can approve on the wire.
    #[error("the author is not granted this change: {0}")]
    NotGranted(String),

    /// The change's app surface is not here yet, so its claims cannot be
    /// decided: keep the op and decide it once the surface is.
    #[error("the app surface this change is decided against is not here yet")]
    SurfaceUnavailable,

    /// The signed document envelope does not match the app/workspace/record
    /// route on which it arrived, or does not carry the typed document
    /// operation shape required by that route.
    #[error("signed document envelope does not match its collaboration route")]
    DocumentRouteMismatch,

    /// `op.artifact` is unknown to the validator — without it the op has no
    /// place in the tree, so we reject.
    #[error("unknown artifact id: {0:?}")]
    UnknownArtifact(ArtifactId),

    /// The op's timestamp is outside the window its admission allows: stale
    /// live traffic, or an op dated ahead of this clock.
    #[error("op timestamp is outside its allowed window")]
    OutsideValidityWindow,

    /// The op's `seq` is not strictly greater than the last-seen seq for the
    /// same author — replay or out-of-order delivery.
    #[error("replay or out-of-order seq: {seen:?} >= incoming {incoming}")]
    ReplaySeq {
        /// The last-seen seq for this author, prior to this op.
        seen: u64,
        /// The seq carried by this op.
        incoming: u64,
    },

    /// A `Redact` op's payload failed to parse or violated a per-payload
    /// invariant. Reported here rather than a separate decode pass so
    /// malformed Redacts land in the same error bucket as any other invalid
    /// op.
    #[error("malformed redact payload: {0}")]
    MalformedRedact(#[source] RedactionError),

    /// The `Redact` op's `target_op_id` resolves to an already-redacted op.
    /// Recursive redaction is rejected — "un-redact is not a concept".
    #[error("recursive redaction: target op id {target:?} is itself a Redact")]
    RecursiveRedaction {
        /// The id of the op the rejected Redact attempted to target.
        target: OpId,
    },

    /// The inbound op's id is already in `redacted_ops`. The op-log entry is
    /// rewritten to the redacted marker (so no seq gap forms), but derived
    /// state is **not** updated; the error lets the broker audit-log
    /// `OpArrivedRedacted`.
    #[error("op {target:?} was already redacted; payload swapped for marker")]
    OpAlreadyRedacted {
        /// The id of the op that was already in the redacted set.
        target: OpId,
    },
}

/// The workspace an [`InboundState::default`] names, until a host sets its own.
pub const DEFAULT_WORKSPACE: &str = "workspace";

/// What inbound ops are validated against and recorded in: the artifact map
/// that places each change in the tree, the grants and app surfaces its
/// claims are decided with, the replay watermarks, and the op log with the
/// redactions applied to it.
///
/// Built by the broker (or a test) from cheap accessors, handed in by value.
/// Only `last_seq_per_author` is mutated on success; everything else is
/// read-only. Fields are `pub` because this is a foundation crate downstream
/// code composes in ways we can't predict.
#[derive(Clone, Debug)]
pub struct InboundState {
    /// Artifact-kind lookup keyed by artifact id, required to decide a change.
    pub artifacts: HashMap<ArtifactId, ArtifactKind>,

    /// The app that owns each artifact, if known: the artifact's place in
    /// the workspace's tree is `workspace › app › resource`.
    pub artifact_apps: HashMap<ArtifactId, String>,

    /// The record each artifact is, if known: the artifact's node is
    /// `workspace › app › record`; otherwise the artifact id stands in.
    pub artifact_records: HashMap<ArtifactId, String>,

    /// The record kind each artifact is, if known: which of its app's
    /// resources the record's changes are decided against.
    pub artifact_kinds: HashMap<ArtifactId, String>,

    /// The workspace this state belongs to, as the tree names it.
    pub workspace: String,

    /// The app surfaces changes are decided against by their claims;
    /// without a source, a change is decided as an edit of its record.
    pub surfaces: crate::receipt::Surfaces,

    /// The workspace's grants, role presets included: the author's changes
    /// are decided with them.
    pub grants: Vec<Grant>,

    /// Last-seen sequence number per author. Updated in-place on success;
    /// non-destructive callers pass `&mut clone()`.
    pub last_seq_per_author: HashMap<Ed25519PublicKey, u64>,

    /// In-memory op log, bounded by `max_in_memory_ops`, pushed in arrival
    /// order. Oldest entry evicted on overflow, or a matching prefix chopped
    /// in one shot on compaction. Lets a late-joining peer fetch recent
    /// history via `CollabRequest::FetchScrollback`.
    pub op_log: VecDeque<SignedOp>,

    /// Upper bound on `op_log`, defaults to [`DEFAULT_MAX_IN_MEMORY_OPS`].
    /// Configurable per workspace — a chat record and a kanban record have
    /// very different op cadences.
    pub max_in_memory_ops: usize,

    /// Op ids redacted here, consulted by [`InboundState::is_redacted`] and
    /// the validator's redaction filter. Survives compaction — a redaction
    /// record outlives the op it points to. In-memory only.
    pub redacted_ops: HashSet<OpId>,

    /// Pre-redaction full envelope of every redacted op, keyed by [`OpId`].
    /// The audit log reconstructs the original `target_op_hash` from these;
    /// compaction consults them so the prefix hash stays stable across
    /// redactions (hash original bytes, not marker bytes). Bounded by the
    /// redaction set, small by construction.
    pub original_envelopes: HashMap<OpId, Box<SignedOp>>,
}

impl Default for InboundState {
    fn default() -> Self {
        Self {
            artifacts: HashMap::new(),
            artifact_apps: HashMap::new(),
            artifact_records: HashMap::new(),
            artifact_kinds: HashMap::new(),
            workspace: DEFAULT_WORKSPACE.to_owned(),
            surfaces: crate::receipt::Surfaces::default(),
            grants: roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner)),
            last_seq_per_author: HashMap::new(),
            op_log: VecDeque::new(),
            max_in_memory_ops: DEFAULT_MAX_IN_MEMORY_OPS,
            redacted_ops: HashSet::new(),
            original_envelopes: HashMap::new(),
        }
    }
}

impl InboundState {
    /// Record a validated op into the in-memory log.
    ///
    /// Kept separate from the validators so they stay pure — callers opt into
    /// logging by calling this afterwards. The oldest entry is evicted on
    /// overflow (FIFO); a compaction checkpoint supersedes the FIFO bound by
    /// chopping a larger range in one shot.
    pub fn record_validated_op(&mut self, op: SignedOp) {
        if self.op_log.len() >= self.max_in_memory_ops && self.max_in_memory_ops > 0 {
            self.op_log.pop_front();
        }
        self.op_log.push_back(op);
    }

    /// Apply a host-emitted compaction checkpoint to this op log.
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
    /// fanning a freshly-arrived op out, and by the validator to rewrite a
    /// late-arriving op's payload to the redacted marker.
    pub fn is_redacted(&self, op_id: OpId) -> bool {
        self.redacted_ops.contains(&op_id)
    }

    /// Apply a parsed [`RedactRequest`] to this state.
    ///
    /// On success: inserts `req.target_op_id` into `redacted_ops`; walks
    /// `op_log` and, for every matching entry, snapshots the pre-redaction
    /// envelope into `original_envelopes`, then rewrites the entry's
    /// `payload` to a [`RedactedMarker::v1`] blob; returns the number of
    /// entries rewritten (zero is valid — compacted away, or arriving later).
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

/// Validate an op whose author and role were established by a certified
/// device join plus the live workspace roster. This is the production
/// document path: certificate/admission validation happens at the broker
/// boundary, while this function carries the signature, grant, freshness,
/// replay and redaction checks.
///
/// `author` is the author's authority from the workspace's live policy.
/// `max_clock_skew` bounds how far an admitted device may backdate or
/// future-date an operation.
///
/// # Errors
///
/// Returns [`ValidationError::BadOpSignature`] for a forged envelope,
/// [`ValidationError::OutsideValidityWindow`] outside the clock-skew window,
/// or a policy, replay or redaction error.
pub fn validate_admitted_inbound_op(
    op: &SignedOp,
    author: &Authority<'_>,
    state: &mut InboundState,
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

    validate_authorized_op(op, author, state)
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
    state: &mut InboundState,
    now: SystemTime,
    max_clock_skew: Duration,
) -> Result<(), ValidationError> {
    verify_op(op).map_err(ValidationError::BadOpSignature)?;
    let latest = now.checked_add(max_clock_skew).unwrap_or(now);
    if op.timestamp > latest {
        return Err(ValidationError::OutsideValidityWindow);
    }
    validate_authorized_op(op, author, state)
}

/// Checks shared by admitted and replayed operations after their identity and
/// time proofs have passed.
fn validate_authorized_op(
    op: &SignedOp,
    author: &Authority<'_>,
    state: &mut InboundState,
) -> Result<(), ValidationError> {
    check_op_permission(op, author, state)?;

    // A Redact carries a payload whose invariants are checked here rather
    // than in a separate decode pass.
    if op.kind == OpKind::Redaction {
        check_redact_payload(op, state)?;
    }

    // Replay defence: seq must strictly increase per author.
    let last = state.last_seq_per_author.get(&op.author).copied();
    if let Some(prev) = last
        && op.seq <= prev
    {
        return Err(ValidationError::ReplaySeq {
            seen: prev,
            incoming: op.seq,
        });
    }

    // Redaction-aware filter. An op whose derived OpId is in `redacted_ops`
    // is the late-arrival case — the redaction reached this peer before the
    // op did. Surfaced as a typed error so the broker can audit-log
    // `OpArrivedRedacted`, push a marker entry so no seq gap forms, and
    // refuse to fan out/apply.
    //
    // `last_seq_per_author` is left alone on this path (like every other
    // error), so a re-broadcast hits the same check idempotently. Redact
    // ops themselves are exempt — a Redact-of-a-Redact lands in
    // `check_redact_payload`, and a Redact's own id is never in
    // `redacted_ops` by construction.
    if op.kind != OpKind::Redaction {
        let id = OpId::from_op(op);
        if state.redacted_ops.contains(&id) {
            return Err(ValidationError::OpAlreadyRedacted { target: id });
        }
    }

    // All checks passed. Bump replay sequence.
    state.last_seq_per_author.insert(op.author, op.seq);
    Ok(())
}

/// The node of a collaborative record: `workspace › app › resource`.
#[must_use]
pub fn document_node(workspace: &str, app: &str, record: &str) -> TreePath {
    TreePath::workspace(workspace).app(app).resource(record)
}

/// Every record `workspace › app › resource` one of `grants` names (at the record
/// or anywhere below it), as `(app, record)`, in grant order and possibly
/// repeated.
///
/// A record's channel id is a hash of its route, so a participant cannot turn
/// a channel back into the record it carries. A grant that names a record
/// gives the route itself, so a participant admitting devices per record can
/// decide a Read Deny on that record from the first admission on, without
/// having seen the record written and without any mapping of its own to
/// lose on a restart. A pattern grant (every resource of an app, any depth)
/// names no record and is not inverted here.
pub fn named_records<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    workspace: &str,
) -> impl Iterator<Item = (&'g str, &'g str)> {
    use tilo_action_policy::{Level, Segment};
    grants.into_iter().filter_map(move |grant| {
        let [
            Segment::Name(Level::Workspace, named_workspace),
            Segment::Name(Level::App, app),
            Segment::Name(Level::Resource, record),
            ..,
        ] = grant.node.segments()
        else {
            return None;
        };
        (named_workspace == workspace).then_some((app.as_str(), record.as_str()))
    })
}

/// The record whose channel is `channel` among those `grants` name in
/// `workspace` ([`named_records`]); `None` when no grant names it.
#[must_use]
pub fn named_record_of_channel<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    workspace: &str,
    channel: crate::ChannelId,
) -> Option<(&'g str, &'g str)> {
    named_records(grants, workspace)
        .find(|(app, record)| crate::document_channel_id(app, workspace, record) == channel)
}

/// The app segment of an artifact no app claims.
const UNKNOWN_APP: &str = "-";

/// The change the op makes must be allowed by the author's grants.
/// The wire shares no clock with the author, so grants are judged without
/// expiry; the authoring device refused after expiry. Fail-closed: no grant,
/// or one that asks, refuses. Admitting only an outright Allow also means the
/// author's own grants allow it, which is all a privileged action needs
/// (`tilo_action_policy::Verdict::privileged`).
fn check_op_permission(
    op: &SignedOp,
    author: &Authority<'_>,
    state: &InboundState,
) -> Result<(), ValidationError> {
    if !state.artifacts.contains_key(&op.artifact) {
        return Err(ValidationError::UnknownArtifact(op.artifact));
    }
    let artifact = op.artifact.0.to_string();
    let record = crate::receipt::Record {
        workspace: &state.workspace,
        app: state
            .artifact_apps
            .get(&op.artifact)
            .map_or(UNKNOWN_APP, String::as_str),
        record: state
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
        state.artifact_kinds.get(&op.artifact).map(String::as_str),
        None,
        &state.surfaces,
        &allows,
    ) {
        Ok(crate::receipt::Receipt::Admit) => Ok(()),
        Ok(crate::receipt::Receipt::Hold) => Err(ValidationError::SurfaceUnavailable),
        Err(refusal) => Err(ValidationError::NotGranted(refusal.to_string())),
    }
}

/// If Redact, parse the payload and reject recursive redactions. Parsing also
/// catches the envelope-author/payload-redactor mismatch and the
/// reason-length bound, surfaced as `MalformedRedact`.
fn check_redact_payload(op: &SignedOp, state: &InboundState) -> Result<(), ValidationError> {
    let req = parse_redact_payload(op).map_err(ValidationError::MalformedRedact)?;
    // Recursive-redaction check: walk the op log for an entry whose
    // derived OpId matches `req.target_op_id` and is itself `Redact` —
    // disallowed. Bounded by max_in_memory_ops; original_envelopes isn't
    // consulted since a previously-redacted Redact would have been
    // refused here on first apply, so by induction the cache never holds one.
    for entry in &state.op_log {
        if entry.kind == OpKind::Redaction && OpId::from_op(entry) == req.target_op_id {
            return Err(ValidationError::RecursiveRedaction {
                target: req.target_op_id,
            });
        }
    }
    Ok(())
}

/// Does `op` fall within `checkpoint`'s prefix — same artifact, `seq` no
/// later than the checkpoint's `up_to_seq`? Shared by both the hash
/// recomputation and the eviction pass in [`InboundState::apply_compaction`]
/// so the two stay in lock-step by construction.
fn op_in_checkpoint_prefix(op: &SignedOp, checkpoint: &CompactionCheckpoint) -> bool {
    let same_artifact = op.artifact == checkpoint.artifact;
    let within_prefix = op.seq <= checkpoint.up_to_seq;
    same_artifact && within_prefix
}
