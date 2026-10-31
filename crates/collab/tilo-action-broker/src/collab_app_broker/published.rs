//! The session's published resources on an app's connection: what a read and
//! an invocation of them answer, and the task that carries a watch.

use std::sync::Arc;

use crate::services::{PublishedResources, Reading, ResourceWatch};
use runtime_collab::wire::{Event, HostFrame, Receipt, Reply};
use tokio::sync::{mpsc, oneshot};

/// What a read of the session's resources answers: the reading and the
/// services' revision it was read at, once the session's loop made the read.
///
/// # Errors
/// The handle is not this connection's, or the session ended first.
pub(super) async fn read(
    asking: Result<oneshot::Receiver<Reading>, String>,
) -> Result<Reply, String> {
    let (revision, reading) = asking?
        .await
        .map_err(|_| "the session ended before it read the resource".to_owned())?;
    Ok(Reply::Snapshot { revision, reading })
}

/// What an invocation of one of the session's resources answers.
///
/// # Errors
/// The handle is not this connection's, or the session ended first.
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
/// with the subscription, and the watch with the task.
pub(super) fn serving(
    published: Arc<dyn PublishedResources>,
    watch: ResourceWatch,
    mut deliveries: mpsc::UnboundedReceiver<Event>,
    frames: mpsc::UnboundedSender<HostFrame>,
    subscription: u64,
) -> tokio::task::AbortHandle {
    tokio::task::spawn_local(async move {
        let _served = ServedWatch { published, watch };
        while let Some(event) = deliveries.recv().await {
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
