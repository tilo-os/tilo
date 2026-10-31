//! Bounded action-only moderation requests on acknowledged record channels.
use super::CollabRequest;
use crate::invocation_scope::ExternalModerationReply;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{mpsc, oneshot};

type Reply = Result<registry_types::ModerationCase, String>;
const MAX_ROUTES: usize = 16;
const MAX_PENDING: usize = 2;
struct Endpoint(mpsc::Sender<CollabRequest>);
pub(super) struct Registration {
    _endpoint: Arc<Endpoint>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
fn registry() -> &'static Mutex<BTreeMap<String, Weak<Endpoint>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Weak<Endpoint>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

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
    let endpoint = registry()
        .lock()
        .map_err(|_| "external moderation bridge unavailable")?
        .get(route)
        .and_then(Weak::upgrade)
        .ok_or("no acknowledged external moderation route")?;
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

fn admit(route: String, endpoint: &Arc<Endpoint>) -> Result<(), String> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "external moderation bridge unavailable")?;
    entries.retain(|_, endpoint| endpoint.strong_count() != 0);
    if route.is_empty()
        || route.len() > 1024
        || entries.contains_key(&route)
        || entries.len() >= MAX_ROUTES
    {
        return Err("external moderation route is ambiguous or exceeds admission bounds".into());
    }
    entries.insert(route, Arc::downgrade(endpoint));
    Ok(())
}

pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration, String> {
    let (send, mut receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    admit(route, &endpoint)?;
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
    Ok(Registration {
        _endpoint: endpoint,
        worker,
    })
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
    admit(route, &endpoint)?;
    Ok((LaneForTest(endpoint), receive))
}
