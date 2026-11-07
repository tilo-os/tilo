//! The lane on which an app names the paths it is about to write for one of
//! its own invocations, so the host can record the agent behind them
//! ([`runtime_collab::write_attribution`]). It sends no actor: the host takes
//! that from its own custody of the invocation.
use crate::invocation_scope::WriteAttributionReceipt;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::lane::{Lane, Registration};

const MAX_PENDING: usize = 4;

struct Request {
    command: runtime_collab::write_attribution::Request,
    invocation: tilo_actions::InvocationId,
    reply: std::sync::mpsc::SyncSender<Result<(), String>>,
}

pub(crate) struct Endpoint(mpsc::Sender<Request>);

/// The routes this kind of lane serves.
static LANE: Lane<Endpoint> = Lane::new("write attribution");

/// Ask the host on `route` to name the agent behind `invocation` as the
/// cause of the writes to `paths` this app is about to make.
///
/// # Errors
/// Refuses an invalid request, a route without the lane, or a full lane.
pub fn enqueue(
    route: &str,
    invocation: tilo_actions::InvocationId,
    paths: Vec<String>,
) -> Result<WriteAttributionReceipt, String> {
    let command = runtime_collab::write_attribution::Request::new(paths);
    command.validate()?;
    let endpoint = LANE.endpoint(route)?;
    let (reply, receive) = std::sync::mpsc::sync_channel(1);
    endpoint
        .0
        .try_send(Request {
            command,
            invocation,
            reply,
        })
        .map_err(|_| "write attribution lane unavailable or full")?;
    Ok(WriteAttributionReceipt::new(receive))
}

/// Serve the lane for `route` over `channel` while the registration lives.
///
/// # Errors
/// Refuses an ambiguous route or too many of them.
pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration<Endpoint>, String> {
    let (send, mut receive) = mpsc::channel(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    LANE.admit(route, &endpoint)?;
    let worker = tokio::task::spawn_local(async move {
        while let Some(request) = receive.recv().await {
            let result = channel
                .attribute_writes(&request.command, *request.invocation.0.as_bytes())
                .await
                .map_err(|error| error.to_string());
            let _ = request.reply.try_send(result);
        }
    });
    Ok(Registration::new(endpoint, worker))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A route without the lane, or a request naming no clean path, never
    /// reaches the host; an unanswered receipt gives up at its deadline.
    #[test]
    fn requests_need_a_lane_and_clean_paths() {
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        assert!(enqueue("no-such-route", invocation, vec!["/var/workspace/a".into()]).is_err());
        let (send, receive) = mpsc::channel(MAX_PENDING);
        let endpoint = Arc::new(Endpoint(send));
        LANE.admit("files-route".into(), &endpoint).unwrap();
        assert!(enqueue("files-route", invocation, vec!["relative".into()]).is_err());
        let receipt = enqueue("files-route", invocation, vec!["/var/workspace/a".into()]).unwrap();
        drop(receive);
        assert!(
            receipt.wait(std::time::Duration::from_millis(10)).is_err(),
            "nobody answered"
        );
        drop(endpoint);
    }
}
