//! Bounded action-only moderation requests on acknowledged record channels.
use super::CollabRequest;
use crate::invocation_scope::ExternalModerationReply;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::lane::{Lane, Registration};

type Reply = Result<registry_types::ModerationCase, String>;
const MAX_PENDING: usize = 2;
pub(crate) struct Endpoint(mpsc::Sender<CollabRequest>);

/// The routes this kind of lane serves.
static LANE: Lane<Endpoint> = Lane::new("external moderation");

pub fn enqueue(
    route: &str,
    invocation: tilo_actions::InvocationId,
    operation: registry_types::ExternalModerationOperation,
) -> Result<oneshot::Receiver<Reply>, String> {
    let command = runtime_collab::external_moderation::Request {
        protocol: runtime_collab::external_moderation::PROTOCOL.into(),
        invocation: invocation.0.to_string(),
        operation,
    };
    command.validate()?;
    if serde_json::to_vec(&command)
        .map_err(|error| error.to_string())?
        .len()
        > runtime_collab::external_moderation::MAX_WIRE_BYTES
    {
        return Err("external moderation exceeds the protocol bound".into());
    }
    let endpoint = LANE.endpoint(route)?;
    let (reply, receive) = ExternalModerationReply::new();
    endpoint
        .0
        .try_send(CollabRequest::ExternalModeration {
            command,
            on_behalf_of: invocation,
            reply,
        })
        .map_err(|_| "external moderation bounded lane unavailable or full")?;
    Ok(receive)
}

pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration<Endpoint>, String> {
    let (send, mut receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    LANE.admit(route, &endpoint)?;
    let worker = tokio::task::spawn_local(async move {
        while let Some(request) = receive.recv().await {
            if let CollabRequest::ExternalModeration {
                command,
                on_behalf_of,
                reply,
            } = request
            {
                execute(&channel, &command, on_behalf_of, &reply).await;
            }
        }
    });
    Ok(Registration::new(endpoint, worker))
}

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn execute(
    channel: &runtime_collab::ChannelHandle,
    command: &runtime_collab::external_moderation::Request,
    on_behalf_of: tilo_actions::InvocationId,
    reply: &ExternalModerationReply,
) {
    if reply.is_closed() {
        return;
    }
    let result = channel
        .external_moderation(command, *on_behalf_of.0.as_bytes())
        .await
        .map_err(|error| error.to_string());
    reply.send(result);
}

/// A lane admitted on `route` for the toolkit's tests, which take its jobs
/// from the returned receiver; dropping it retires the lane.
#[cfg(any(test, feature = "test-support"))]
pub struct LaneForTest(
    #[allow(dead_code, reason = "held to keep the lane admitted")] Arc<Endpoint>,
);

/// Admit a lane on `route` for a test, as the broker worker admits one on an
/// acknowledged record route.
///
/// # Errors
/// When `route` is invalid, already has a live lane, or too many are live.
#[cfg(any(test, feature = "test-support"))]
pub fn admit_for_test(
    route: String,
) -> Result<(LaneForTest, mpsc::Receiver<CollabRequest>), String> {
    let (send, receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    LANE.admit(route, &endpoint)?;
    Ok((LaneForTest(endpoint), receive))
}
