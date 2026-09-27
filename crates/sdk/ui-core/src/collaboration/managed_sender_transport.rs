//! Borrowed transport for the managed sender's existing exact-binding calls.
//! Neither an observation nor a transport failure creates a durable receipt.
use super::SdkRoute;
use crate::time::Instant;
use runtime_collab::{PublishedInvocationBinding, PublishedInvocationObservation};

pub(super) enum Request<'a> {
    Invoke {
        invocation: &'a [u8; 16],
        binding: &'a PublishedInvocationBinding,
        revision: &'a str,
        resource: &'a str,
        action: &'a str,
        argument: Option<&'a str>,
        confirmation_json: &'a str,
    },
    Observe {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
    Cancel {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
}

/// Fixed transport outcomes. Neither variant proves that execution did not occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Deadline,
    Unavailable,
}

/// Runs on the actual route's LocalSet; it never creates an ID or retries.
/// The caller still validates the complete lifecycle grammar before retention.
#[allow(
    clippy::future_not_send,
    reason = "Cap'n Proto routes are Rc-based and remain on the shovel LocalSet"
)]
pub(super) async fn send(
    route: &SdkRoute,
    request: Request<'_>,
    deadline: Instant,
) -> Result<PublishedInvocationObservation, Failure> {
    let now = crate::time::now();
    if deadline <= now {
        return Err(Failure::Deadline);
    }
    let deadline = deadline.min(now + crate::time::Duration::from_secs(2));
    let rpc = async {
        match request {
            Request::Invoke {
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
            Request::Observe { host, invocation } => {
                route
                    .channel
                    .published_invocation_observation(host, invocation)
                    .await
            }
            Request::Cancel { host, invocation } => {
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
        return Err(Failure::Deadline);
    }
    result
        .map_err(|_| Failure::Deadline)?
        .map_err(|_| Failure::Unavailable)
}

#[cfg(test)]
#[path = "managed_sender_transport/tests.rs"]
mod tests;
