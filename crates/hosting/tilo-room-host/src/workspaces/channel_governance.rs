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
