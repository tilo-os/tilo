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

use tilo_action_policy::{Grant, Grantee, TreePath, roles};

use crate::artifact::ArtifactId;
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
    pub artifact_resources: HashMap<ArtifactId, String>,

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

    /// Whose a change of this write is when it names no one: the person an
    /// import step attributes a history kept before every change named its
    /// invoker to. `None` refuses such a change, as every other write.
    pub imported: Option<tilo_action_policy::Acting>,
}

impl Default for InboundState {
    fn default() -> Self {
        Self {
            artifacts: HashMap::new(),
            artifact_apps: HashMap::new(),
            artifact_resources: HashMap::new(),
            artifact_kinds: HashMap::new(),
            workspace: DEFAULT_WORKSPACE.to_owned(),
            surfaces: crate::receipt::Surfaces::default(),
            grants: roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner)),
            last_seq_per_author: HashMap::new(),
            op_log: VecDeque::new(),
            max_in_memory_ops: DEFAULT_MAX_IN_MEMORY_OPS,
            redacted_ops: HashSet::new(),
            original_envelopes: HashMap::new(),
            imported: None,
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
}

/// `collab_admission::validate_admitted_inbound_op` for an op a receiver already admitted as
/// its author's and the member's, when it kept it: its signature, its time
/// and its place in the log are checked, never its grants a second time. A
/// change made for a caller elsewhere is decided where it was kept, with the
/// signed call the change carries; this path cannot see the call again.
///
/// `history` for an op replayed from retained history (`collab_admission::validate_replayed_inbound_op`).
///
/// # Errors
///
/// As `collab_admission::validate_admitted_inbound_op`, but never a policy error.
pub fn validate_kept_inbound_op(
    op: &SignedOp,
    state: &mut InboundState,
    now: SystemTime,
    max_clock_skew: Duration,
    history: bool,
) -> Result<(), ValidationError> {
    verify_inbound_time(op, now, max_clock_skew, history)?;
    validate_log(op, state)
}

/// The signature every inbound op carries, and its timestamp within the
/// window its admission allows ([`inbound_time`]).
///
/// # Errors
/// [`ValidationError::BadOpSignature`] or [`ValidationError::OutsideValidityWindow`].
pub fn verify_inbound_time(
    op: &SignedOp,
    now: SystemTime,
    max_clock_skew: Duration,
    history: bool,
) -> Result<(), ValidationError> {
    verify_op(op).map_err(ValidationError::BadOpSignature)?;
    inbound_time(op, now, max_clock_skew, history)
}

/// An op's timestamp within the window its admission allows: one dated ahead of this clock is
/// refused, and so is a live one dated behind it (`history` exempts an op from that, as retained
/// history is old by nature).
///
/// # Errors
/// [`ValidationError::OutsideValidityWindow`].
pub fn inbound_time(
    op: &SignedOp,
    now: SystemTime,
    max_clock_skew: Duration,
    history: bool,
) -> Result<(), ValidationError> {
    let latest = now.checked_add(max_clock_skew).unwrap_or(now);
    if op.timestamp > latest {
        return Err(ValidationError::OutsideValidityWindow);
    }
    let earliest = now
        .checked_sub(max_clock_skew)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if !history && op.timestamp < earliest {
        return Err(ValidationError::OutsideValidityWindow);
    }
    Ok(())
}

/// Where an op falls in the receiver's log: its redaction payload, its replay
/// number, and whether it arrived already redacted.
///
/// # Errors
/// A malformed or recursive redaction, a replayed sequence number, or an op redacted before it
/// arrived.
pub fn validate_log(op: &SignedOp, state: &mut InboundState) -> Result<(), ValidationError> {
    // A Redact carries a payload whose invariants are checked here rather
    // than in a separate decode pass.
    if op.kind == OpKind::Redaction {
        check_redact_payload(op, state)?;
    }

    // Replay defence: seq must strictly increase per author. A compaction is
    // numbered as the last write of its author it folds, so it restates
    // history rather than following it: taking it in place of exactly those
    // writes is what refuses a stale or partial one. Numbering it past them
    // would make every reader that opened the record by the fold refuse the
    // author's next write as a replay.
    let last = state.last_seq_per_author.get(&op.author).copied();
    if op.kind != OpKind::Compaction
        && let Some(prev) = last
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

    // All checks passed. Bump replay sequence. Never lower it: a compaction
    // is numbered as a write it folds, so its author's mark is already past
    // it.
    let floor = state.last_seq_per_author.entry(op.author).or_insert(0);
    *floor = (*floor).max(op.seq);
    Ok(())
}

/// The node of a collaborative record: `workspace › app › resource`.
#[must_use]
pub fn document_node(workspace: &str, app: &str, document: &str) -> TreePath {
    use tilo_actions::document_sync::{DocumentSync, document_sync};
    let resource = |id| {
        let id = if crate::decided_as_app_record(app, id) {
            ""
        } else {
            id
        };
        let path = TreePath::workspace(workspace).app(app);
        let path = match crate::workspace_document::sealed_kind(app, id) {
            Some(kind) => path.kind(kind),
            None => path,
        };
        path.resource(id)
    };
    match document_sync(document) {
        DocumentSync::Part { base, .. }
        | DocumentSync::Chunk { base, .. }
        | DocumentSync::ChainHead { base } => resource(base).item(document),
        DocumentSync::Eager | DocumentSync::OnDemand => resource(document),
    }
}

#[cfg(test)]
mod protected_document_tests {
    use super::*;

    #[test]
    fn a_kind_grant_covers_the_sealed_record_at_the_host() {
        let vault = crate::workspace_document_of_kind("passwords.workspace-vault")
            .and_then(|kept| kept.fixed)
            .expect("the vault's fixed id");
        let node = document_node("workspace", "so.tilo.passwords", vault);
        let kind = TreePath::workspace("workspace")
            .app("so.tilo.passwords")
            .kind("passwords.workspace-vault");
        assert!(kind.covers(&node));
        assert!(kind.resource(vault).covers(&node));
    }

    /// Sealing follows the kind a record's id is bound to, as the kind's owner declares it: an app
    /// whose own records are of no sealed kind seals nothing, whatever it is called.
    #[test]
    fn an_app_name_seals_nothing() {
        let vault = crate::workspace_document_of_kind("passwords.workspace-vault")
            .and_then(|kept| kept.fixed)
            .expect("the vault's fixed id");
        assert!(requires_independent_document_keys(
            "so.tilo.passwords",
            vault
        ));
        assert!(requires_independent_document_keys("so.tilo.notes", vault));
        assert!(!requires_independent_document_keys("so.tilo.passwords", ""));
        assert!(!requires_independent_document_keys(
            "so.tilo.passwords",
            "uuid:00000000-0000-4000-8000-000000000001"
        ));
        assert!(requires_independent_document_keys("so.tilo.mail", ""));
        assert!(requires_independent_document_keys("dev.tilo.mail", ""));
        assert!(!requires_independent_document_keys("so.tilo.notes", ""));
        let history = crate::workspace_document_id("terminal.history");
        assert!(requires_independent_document_keys(
            "so.tilo.terminal",
            &history
        ));
        assert!(requires_independent_document_keys(
            "so.tilo.notes",
            &history
        ));
        assert!(!requires_independent_document_keys(
            "so.tilo.terminal",
            &crate::workspace_document_id("terminal.machine-secrets")
        ));
    }
}

/// The document that decides access to this data, excluding workspace-wide
/// service documents.
pub fn document_access_target<'a>(app: &str, document: &'a str) -> Option<&'a str> {
    use tilo_actions::document_sync::{DocumentSync, document_sync};
    let document = match document_sync(document) {
        DocumentSync::Part { base, .. }
        | DocumentSync::Chunk { base, .. }
        | DocumentSync::ChainHead { base } => base,
        DocumentSync::Eager | DocumentSync::OnDemand => document,
    };
    let document = if crate::decided_as_app_record(app, document) {
        ""
    } else {
        document
    };
    crate::workspace_document(app, document)
        .is_none()
        .then_some(document)
}

/// Whether this route has to use independent keys before any device is given
/// the document. Parts and chunks inherit the requirement of their root.
#[must_use]
pub fn requires_independent_document_keys(app: &str, document: &str) -> bool {
    // A call's media keys go only to the people its rule admits ([`crate::calls::media_document`]).
    (app == crate::scope::MEDIA_APP && crate::calls::parse_media_document(document).is_some())
        || document_access_target(app, document)
            .is_some_and(|root| crate::workspace_document::sealed_kind(app, root).is_some())
}

/// Whether a party whose access is `access` may read `document` of `app` in `workspace`, when the
/// app keeps it sealed ([`requires_independent_document_keys`]): what decides whether the app
/// opens there, and which device takes the app's long-lived connection.
#[must_use]
pub fn reads_protected_document(
    workspace: &str,
    app: &str,
    document: &str,
    access: Option<&tilo_action_policy::MemberAccess>,
    now: web_time::SystemTime,
) -> bool {
    use tilo_action_policy::{ActionKind, Answer, Context};
    if !requires_independent_document_keys(app, document) {
        return true;
    }
    let node = document_node(workspace, app, document);
    access.is_some_and(|access| {
        access.answer(&node, ActionKind::Read, &Context::at_time(now)) == Answer::Allow
    })
}

/// The access editor's route: an empty app selects workspace defaults or the default for the
/// app named in `document`. A nonempty app selects one exact document.
fn access_rule_node(workspace: &str, app: &str, document: &str) -> Option<TreePath> {
    let workspace_path = TreePath::workspace(workspace);
    match (app, document) {
        ("", "") => Some(workspace_path.any(tilo_action_policy::Level::App)),
        ("", app) => tilo_app_id::AppId::parse(app)
            .ok()
            .map(|_| workspace_path.app(app)),
        _ => document_access_target(app, document)
            .map(|document| document_node(workspace, app, document)),
    }
}

/// Replace the access rule for one recipient at the selected scope.
pub fn document_access_changes(
    policy: &tilo_action_policy::WorkspacePolicy,
    actor: &str,
    (who, app, document): (&str, &str, &str),
    access: &str,
) -> Result<Vec<tilo_action_policy::Change>, tilo_action_policy::Refusal> {
    use tilo_action_policy::{Answer, Change, Grant, Grantee, KindSet, MemberId, Refusal, Role};
    let path = access_rule_node(&policy.workspace, app, document)
        .ok_or_else(|| Refusal::NotPermitted("this target has no workspace access rules".into()))?;
    let grantee = if who == "everyone" {
        Grantee::role(Role::Viewer)
    } else if let Some(id) = who.strip_prefix("role:") {
        if !policy.custom_roles.contains_key(id) {
            return Err(Refusal::UnknownRole(id.to_owned()));
        }
        Grantee::CustomRole(id.to_owned())
    } else if let Some(person) = who.strip_prefix("member:") {
        if !policy.is_member(person) {
            return Err(Refusal::NotAMember(person.to_owned()));
        }
        Grantee::Member(
            MemberId::parsed_person(person)
                .ok_or_else(|| Refusal::NotAPerson(person.to_owned()))?,
        )
    } else {
        return Err(Refusal::NotPermitted(
            "choose everyone, a role, or a member".into(),
        ));
    };
    let key = format!("document-access:{who}:{path}:");
    let rules = match access {
        "inherit" => Vec::new(),
        "no-access" => vec![(KindSet::ALL, Answer::Deny)],
        "view" => vec![
            (KindSet::READ, Answer::Allow),
            (KindSet::UNDOABLE.union(KindSet::OUTSIDE), Answer::Deny),
        ],
        "edit" => vec![(KindSet::READ_UNDOABLE, Answer::Allow)],
        _ => {
            return Err(Refusal::NotPermitted(
                "choose inherit, no access, view, or edit".into(),
            ));
        }
    };
    // Document delivery uses the exact route. Actions on the published
    // resource include a kind between the app and resource.
    let typed_path = (!app.is_empty()).then(|| {
        let document = document_access_target(app, document).unwrap_or(document);
        TreePath::workspace(&policy.workspace)
            .app(app)
            .any(tilo_action_policy::Level::Kind)
            .resource(crate::receipt::published_resource(document))
    });
    let grants: Vec<_> = rules
        .into_iter()
        .enumerate()
        .flat_map(|(index, (kinds, answer))| {
            std::iter::once((format!("{key}{access}:{index}"), path.clone()))
                .chain(
                    typed_path
                        .as_ref()
                        .map(|typed| (format!("{key}{access}:{index}:action"), typed.clone())),
                )
                .map({
                    let grantee = grantee.clone();
                    move |(id, node)| {
                        Grant::new(
                            id,
                            grantee.clone(),
                            node,
                            kinds,
                            answer,
                            Grantee::person(actor),
                        )
                    }
                })
        })
        .collect();
    let mut changes: Vec<_> = policy
        .grants
        .iter()
        .filter(|grant| {
            grant.grantee == grantee
                && (grant.node == path || typed_path.as_ref() == Some(&grant.node))
                && grant.id.as_str().starts_with(&key)
                && !grants.iter().any(|wanted| wanted.id == grant.id)
        })
        .map(|grant| Change::Revoke(grant.id.clone()))
        .collect();
    changes.extend(
        grants
            .into_iter()
            .filter(|wanted| {
                !policy.grants.iter().any(|grant| {
                    grant.id == wanted.id
                        && grant.grantee == wanted.grantee
                        && grant.node == wanted.node
                        && grant.kinds == wanted.kinds
                        && grant.answer == wanted.answer
                })
            })
            .map(Change::Grant),
    );
    Ok(changes)
}

/// One document rule as shown in the workspace access settings.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DocumentAccessRule {
    pub app: String,
    pub document: String,
    pub recipient: String,
    pub access: String,
}

/// The workspace and app defaults shown beside the workspace's documents, including
/// defaults for apps whose documents are not on this device yet.
#[must_use]
pub fn document_access_scopes(
    policy: &tilo_action_policy::WorkspacePolicy,
    documents: &[(String, String)],
    apps: impl IntoIterator<Item = String>,
) -> Vec<(String, String)> {
    use tilo_action_policy::Level;
    let apps = apps
        .into_iter()
        .chain(
            documents
                .iter()
                .filter(|(app, _)| !app.is_empty())
                .map(|(app, _)| app.clone()),
        )
        .chain(policy.grants.iter().filter_map(|grant| {
            let app = grant.node.id_at(Level::App)?;
            (grant.node == TreePath::workspace(&policy.workspace).app(app)
                && grant.id.as_str().starts_with("document-access:"))
            .then(|| app.to_owned())
        }))
        .filter(|app| tilo_app_id::AppId::parse(app).is_ok())
        .collect::<std::collections::BTreeSet<_>>();
    std::iter::once((String::new(), String::new()))
        .chain(apps.into_iter().map(|app| (String::new(), app)))
        .chain(documents.iter().cloned())
        .collect()
}

/// Access settings selected for the supplied scopes.
pub fn document_access_rules(
    policy: &tilo_action_policy::WorkspacePolicy,
    documents: &[(String, String)],
) -> Vec<DocumentAccessRule> {
    use tilo_action_policy::{Grantee, Role};
    let mut settings = Vec::new();
    for (app, document) in documents {
        let Some(path) = access_rule_node(&policy.workspace, app, document) else {
            continue;
        };
        let document = if app.is_empty() {
            document
        } else {
            document_access_target(app, document).unwrap_or(document)
        };
        for grant in policy.grants.iter().filter(|grant| grant.node == path) {
            let recipient = match &grant.grantee {
                Grantee::Role(Role::Viewer) => "everyone".to_owned(),
                Grantee::CustomRole(role) => format!("role:{role}"),
                other => match other.person_id() {
                    Some(person) => format!("member:{person}"),
                    None => continue,
                },
            };
            let prefix = format!("document-access:{recipient}:{path}:");
            let Some(access) = grant
                .id
                .as_str()
                .strip_prefix(&prefix)
                .and_then(|id| id.strip_suffix(":0"))
            else {
                continue;
            };
            if matches!(access, "no-access" | "view" | "edit") {
                settings.push(DocumentAccessRule {
                    app: app.clone(),
                    document: document.to_owned(),
                    recipient,
                    access: access.to_owned(),
                });
            }
        }
    }
    settings
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
