//! An answer the broker refused leaves its caller waiting for one that will
//! not come; the invocation fails instead, saying why (an answer outside the
//! action's declared result, say).

/// Follow `update`, the refused answer to invocation `id`, with the
/// invocation's failure carrying `refusal`. Anything but a success is left
/// as it was: there is nothing it would leave unanswered.
#[allow(
    clippy::future_not_send,
    reason = "runs on the worker's local set, as the channel it answers on does"
)]
pub(super) async fn fail(
    channel: &runtime_collab::ChannelHandle,
    id: u64,
    update: &tilo_actions::InvocationUpdate,
    refusal: String,
) {
    let tilo_actions::InvocationUpdate::Succeeded { invocation, .. } = update else {
        return;
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
    match serde_json::to_string(&failed) {
        Ok(failed) => {
            if let Err(error) = channel.update_invocation(id, &failed).await {
                tracing::warn!(%error, id, "collab: the refused answer's failure was not taken either");
            }
        }
        Err(error) => {
            tracing::warn!(%error, id, "collab: encoding a refused answer's failure failed")
        }
    }
}
