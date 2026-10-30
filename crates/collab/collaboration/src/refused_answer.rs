//! An answer the broker refused leaves its caller waiting for one that will
//! not come; the invocation fails instead, saying why (an answer outside the
//! action's declared result, say).
//!
//! Deciding and encoding the failure is here; sending it is the broker
//! loop's, which already runs on the shovel's `LocalSet` the channel's
//! futures need.

/// The broker refused `update`, the answer to invocation `id`, saying
/// `refusal`: the update that follows it, the invocation's failure carrying
/// `refusal`, encoded as the channel takes it. None for anything but a
/// success, which leaves nothing unanswered, and for a failure that does not
/// encode, which is said.
pub(super) fn failure(
    id: u64,
    update: &tilo_actions::InvocationUpdate,
    refusal: &impl std::fmt::Display,
) -> Option<String> {
    tracing::warn!(
        %refusal,
        id,
        "collab: the broker refused this invocation update; the bridge stays open"
    );
    let tilo_actions::InvocationUpdate::Succeeded { invocation, .. } = update else {
        return None;
    };
    let failed = tilo_actions::InvocationUpdate::Failed {
        invocation: *invocation,
        error: tilo_actions::ActionFailure {
            code: "answer_refused".into(),
            message: format!("the app's answer was refused: {refusal}"),
            details: None,
            retryable: false,
        },
    };
    serde_json::to_string(&failed)
        .inspect_err(|error| {
            tracing::warn!(%error, id, "collab: encoding a refused answer's failure failed");
        })
        .ok()
}

/// How sending the failure that follows invocation `id`'s refused answer
/// went: one the broker did not take either is said.
pub(super) fn followed(id: u64, sent: Result<(), impl std::fmt::Display>) {
    if let Err(error) = sent {
        tracing::warn!(%error, id, "collab: the refused answer's failure was not taken either");
    }
}
