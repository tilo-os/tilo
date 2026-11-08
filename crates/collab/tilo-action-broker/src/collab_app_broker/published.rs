//! The session's published resources on an app's connection: what a read and
//! an invocation of them answer, and the task that carries a watch.

use std::sync::Arc;

use super::AppBrokerState;
use crate::services::{Caller, PublishedResources, Reading, ResourceWatch};
use runtime_collab::wire::{Event, HostFrame, Receipt, Reply};
use tokio::sync::{mpsc, oneshot};

/// Refuse `resource` when the workspace's grants do not let `caller` read it: what
/// its publisher declares the person's own (see [`super::readings`]).
pub(super) fn gate(
    state: &AppBrokerState,
    published: &dyn PublishedResources,
    caller: &Caller,
    resource: &str,
) -> Result<(), String> {
    match published.granted(resource) {
        Some(granted) => super::readings::admit(state, caller, resource, &granted),
        None => Ok(()),
    }
}

/// The decision a watch is held to: what its readings are gated by, decided
/// again at each one, so a grant taken away stops what it already serves.
pub(super) struct ReadingGate {
    pub(super) state: Arc<AppBrokerState>,
    pub(super) published: Arc<dyn PublishedResources>,
    pub(super) caller: Caller,
    pub(super) resource: String,
}

impl ReadingGate {
    fn admit(&self) -> Result<(), String> {
        gate(
            &self.state,
            self.published.as_ref(),
            &self.caller,
            &self.resource,
        )
    }
}

/// What a read of the session's resources answers: the reading and the
/// services' revision it was read at, once the session's loop made the read.
pub(super) async fn read(
    asking: Result<oneshot::Receiver<Reading>, String>,
) -> Result<Reply, String> {
    let (revision, reading) = asking?
        .await
        .map_err(|_| "the session ended before it read the resource".to_owned())?;
    Ok(Reply::Snapshot { revision, reading })
}

/// What an invocation of one of the session's resources answers.
pub(super) async fn invoke(
    asking: Result<oneshot::Receiver<Receipt>, String>,
) -> Result<Reply, String> {
    let receipt = asking?
        .await
        .map_err(|_| "the session ended before it invoked the action".to_owned())?;
    Ok(Reply::Receipt(receipt))
}

/// Carry `watch`'s readings to the app as `subscription`'s events until the
/// subscription ends, answering the task that carries them: the task lives
/// with the subscription, and the watch with the task. A reading the caller
/// may no longer read ends it, with why.
pub(super) fn serving(
    published: Arc<dyn PublishedResources>,
    watch: ResourceWatch,
    mut deliveries: mpsc::UnboundedReceiver<Event>,
    frames: mpsc::UnboundedSender<HostFrame>,
    subscription: u64,
    gate: ReadingGate,
) -> tokio::task::AbortHandle {
    tokio::task::spawn_local(async move {
        let _served = ServedWatch { published, watch };
        while let Some(event) = deliveries.recv().await {
            if let Err(reason) = gate.admit() {
                let _ = frames.send(HostFrame::Ended {
                    subscription,
                    reason,
                });
                break;
            }
            if frames
                .send(HostFrame::Event {
                    subscription,
                    event,
                })
                .is_err()
            {
                break;
            }
        }
    })
    .abort_handle()
}

/// A watch of the session's published resources, held for as long as the task
/// carrying its readings to the app lives: dropped — the app unsubscribed,
/// the connection closed, the session ended — it is unwatched.
struct ServedWatch {
    published: Arc<dyn PublishedResources>,
    watch: ResourceWatch,
}

impl Drop for ServedWatch {
    fn drop(&mut self) {
        self.published.unwatch(self.watch);
    }
}

#[cfg(test)]
#[path = "published_tests.rs"]
mod tests;
