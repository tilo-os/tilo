//! A host's own part in its workspaces' channels: the governance messages it
//! makes (its access request, a channel's genesis, its commits) reach the
//! members that could act on them.

use std::sync::Arc;

use super::Workspaces;

impl Workspaces {
    /// Publish channel governance messages as this relay's own, once what the
    /// workspace's governance already handed its store is durable: a channel's
    /// group is built on it.
    ///
    /// They are retained, which carries them to the members' relay sessions
    /// and to whoever catches up later, and handed to the workspace's direct
    /// link, which carries them to the member devices it holds. A member
    /// linked directly hears nothing the relay retains: without the link, the
    /// host's access request never reaches the participants that could admit
    /// it, nor its genesis or commits, so a host is never let into a channel
    /// a member opened while every member's request reaches the host. The
    /// link already carries what members send each other; this is the
    /// host's own side of the same exchange.
    ///
    /// # Errors
    ///
    /// The messages could not be retained, or this host holds no such
    /// workspace.
    pub async fn publish_channel_governance(
        &self,
        workspace: &str,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<(), String> {
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let (workspace_id, publisher) = {
            let governed = governed.lock().await;
            (governed.workspace_id(), Arc::clone(&governed.publisher))
        };
        self.retain_channel_governance(workspace_id, messages.clone())
            .await?;
        publisher.flushed().await;
        if let Some(link) = self.inner.direct.link(workspace_id) {
            for message in messages {
                if let Err(error) = link.send(message) {
                    tracing::debug!(%workspace, %error, "channel governance did not reach the workspace's link");
                }
            }
        }
        Ok(())
    }

    /// Retain channel governance messages as this relay's own.
    pub(super) async fn retain_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<(), String> {
        for admission in self
            .enqueue_channel_governance(workspace_id, messages)
            .await?
        {
            admission.durable().await?;
        }
        Ok(())
    }
}

impl Workspaces {
    /// Hand `workspace`'s channel service every queued notice: the workspace's
    /// own governance is retained before the channel governance a service
    /// built on it decides them with.
    pub(super) async fn take_channel_notices(&self, workspace: &str) -> Result<(), String> {
        let sink = self
            .inner
            .host
            .channels
            .as_ref()
            .and_then(|channels| channels.sink.clone());
        let Some(sink) = sink else {
            self.inner
                .channel_notices
                .lock()
                .map_err(|_| "channel notices poisoned")?
                .remove(workspace);
            return Ok(());
        };
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let publisher = {
            let governed = governed.lock().await;
            Arc::clone(&governed.publisher)
        };
        // Whatever settle handed over is retained before the channel
        // governance a service derives from it.
        publisher.flushed().await;
        // Taken under the service: notices that queue while it works start the
        // next hand-over, which takes all of them at once.
        let waiting = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?
            .remove(workspace)
            .unwrap_or_default();
        sink.take(waiting);
        Ok(())
    }
}
