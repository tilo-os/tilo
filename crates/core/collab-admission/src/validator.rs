//! The inbound-document validators that decide an op by the changes it carries, on top of
//! the pure checks of [`collab_types::validator`].

use web_time::{Duration, SystemTime};

use collab_types::validator::{validate_log, verify_inbound_time};
use collab_types::{InboundState, SignedOp, ValidationError};
use tilo_action_policy::{ActionKind, Answer, Context};

/// Validate an op whose author and role were established by a certified
/// device join plus the live workspace roster. This is the production
/// document path: certificate/admission validation happens at the broker
/// boundary, while this function carries the signature, grant, freshness,
/// replay and redaction checks.
///
/// `author` is who signed it, from the workspace's live policy: a change
/// naming its invoker is decided as that invoker, bounded by the signer.
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
    author: &dyn collab_types::receipt::ChangeAuthor,
    state: &mut InboundState,
    now: SystemTime,
    max_clock_skew: Duration,
) -> Result<(), ValidationError> {
    verify_inbound_time(op, now, max_clock_skew, false)?;
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
    author: &dyn collab_types::receipt::ChangeAuthor,
    state: &mut InboundState,
    now: SystemTime,
    max_clock_skew: Duration,
) -> Result<(), ValidationError> {
    verify_inbound_time(op, now, max_clock_skew, true)?;
    validate_authorized_op(op, author, state)
}

/// Checks shared by admitted and replayed operations after their identity and
/// time proofs have passed.
fn validate_authorized_op(
    op: &SignedOp,
    author: &dyn collab_types::receipt::ChangeAuthor,
    state: &mut InboundState,
) -> Result<(), ValidationError> {
    check_op_permission(op, author, state)?;
    validate_log(op, state)
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
    author: &dyn collab_types::receipt::ChangeAuthor,
    state: &InboundState,
) -> Result<(), ValidationError> {
    if !state.artifacts.contains_key(&op.artifact) {
        return Err(ValidationError::UnknownArtifact(op.artifact));
    }
    let artifact = op.artifact.0.to_string();
    let document = collab_types::receipt::WrittenDocument {
        workspace: &state.workspace,
        app: state
            .artifact_apps
            .get(&op.artifact)
            .map_or(UNKNOWN_APP, String::as_str),
        document: state
            .artifact_resources
            .get(&op.artifact)
            .map_or(artifact.as_str(), String::as_str),
    };
    let cx = Context::at(0);
    let allows =
        |invoker: Option<&tilo_member_types::Acting>,
         node: &tilo_action_policy::TreePath,
         kind: ActionKind| { author.answer(invoker, node, kind, &cx) == Answer::Allow };
    match crate::receipt::decide_opened(
        (op, None),
        &document,
        (
            state.artifact_kinds.get(&op.artifact).map(String::as_str),
            None,
            None,
        ),
        None,
        &state.surfaces,
        &allows,
        (state.imported.as_ref(), None),
    ) {
        Ok(collab_types::receipt::Receipt::Admit) => Ok(()),
        Ok(collab_types::receipt::Receipt::Hold) => Err(ValidationError::SurfaceUnavailable),
        Err(refusal) => Err(ValidationError::NotGranted(refusal.to_string())),
    }
}
