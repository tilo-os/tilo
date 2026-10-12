//! Borrowed transport for the managed sender's existing exact-binding calls.
//! Neither an observation nor a transport failure creates a durable receipt.
use super::SdkRoute;
use crate::host_worker::managed_sender::{AttemptFailure, RequestView};
use crate::time::Instant;
use runtime_collab::PublishedInvocationObservation;

/// Runs on the actual route's LocalSet; it never creates an ID or retries.
/// The caller still validates the complete lifecycle grammar before retention.
#[allow(
    clippy::future_not_send,
    reason = "Cap'n Proto routes are Rc-based and remain on the shovel LocalSet"
)]
pub(super) async fn send(
    route: &SdkRoute,
    request: RequestView<'_>,
    deadline: Instant,
) -> Result<PublishedInvocationObservation, AttemptFailure> {
    let now = crate::time::now();
    if deadline <= now {
        return Err(AttemptFailure::Deadline);
    }
    let deadline = deadline.min(now + crate::time::Duration::from_secs(2));
    let rpc = async {
        match request {
            RequestView::Invoke {
                invocation,
                binding,
                revision,
                resource,
                action,
                argument,
                confirmation_json,
            } => {
                route
                    .channel
                    .invoke_resolved_published_action_durable(
                        invocation,
                        binding,
                        revision,
                        resource,
                        action,
                        argument,
                        confirmation_json,
                    )
                    .await
            }
            RequestView::Observe { host, invocation } => {
                route
                    .channel
                    .published_invocation_observation(host, invocation)
                    .await
            }
            RequestView::Cancel { host, invocation } => {
                route
                    .channel
                    .cancel_durable_published_invocation(host, invocation)
                    .await
            }
        }
    };
    let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), rpc).await;
    // Timeout polls its inner future first: a ready reply can be observed by
    // a delayed task after expiry. Never expose that reply past our deadline.
    if crate::time::now() >= deadline {
        return Err(AttemptFailure::Deadline);
    }
    result
        .map_err(|_| AttemptFailure::Deadline)?
        .map_err(|_| AttemptFailure::Unavailable)
}

#[cfg(test)]
#[path = "managed_sender_transport/tests.rs"]
mod tests;
