//! A host's part in its workspaces' channels: the governance messages it makes
//! (its access request, a channel's genesis, its commits) reach the members
//! that could act on them, and the governance members send for a channel
//! service the host runs waits here, one queue per workspace, until the
//! service takes it.

use std::sync::Arc;

use web_time::SystemTime;

use super::Workspaces;
use super::channel_workspace;

/// Notices one workspace hands its channel service at once; a device asks
/// again when its request is dropped.
const MAX_QUEUED_CHANNEL_NOTICES: usize = 16_384;

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

    /// Queue `message`, a member's channel governance, for `workspace`'s
    /// channel service. True when it is the first since they were last handed
    /// over, so its sender must start the hand-over.
    fn queue_channel_notice(
        &self,
        workspace: &str,
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<bool, String> {
        let mut queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        let waiting = queued.entry(workspace.to_owned()).or_default();
        let first = waiting.len() < MAX_QUEUED_CHANNEL_NOTICES && {
            waiting.push(message);
            waiting.len() == 1
        };
        drop(queued);
        Ok(first)
    }

    /// Take in `author`'s access request or commit for a channel of `workspace`.
    ///
    /// This host holds no channel keys of its own. One whose device's channel
    /// service it runs queues what members send for that service, which settles
    /// the groups; one that is no device's peer keeps what the channels are
    /// made of like any other governance, for whoever takes part; and one
    /// beside a channel service running on the device itself keeps nothing,
    /// since that service hears these on its own sessions.
    pub(super) async fn take_channel_governance(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        if channel_workspace(&message).is_none_or(|named| named != workspace) {
            return Ok(());
        }
        let Some(channels) = &self.inner.host.channels else {
            // What a governed member device sent, and only that.
            let state = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let (workspace_id, member) = {
                let governed = state.lock().await;
                (
                    governed.workspace_id(),
                    governed
                        .peer()
                        .governance()
                        .governed_access(SystemTime::now())
                        .is_ok_and(|access| access.member_devices.contains(&author)),
                )
            };
            if !member {
                return Ok(());
            }
            return self
                .retain_channel_governance(workspace_id, vec![message])
                .await;
        };
        if channels.sink.is_some() {
            self.channel_notice(workspace, message)?;
        }
        Ok(())
    }

    /// Start handing `workspace`'s queued channel governance over. A crowd
    /// joining sends a request each, so notices only queue here; the first
    /// since they were last taken starts a task that hands all of them over
    /// at once.
    fn channel_notice(
        &self,
        workspace: &str,
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        if self.queue_channel_notice(workspace, message)? {
            let workspaces = self.clone();
            let workspace = workspace.to_owned();
            tokio::spawn(async move {
                if let Err(error) = workspaces.take_channel_notices(&workspace).await {
                    tracing::warn!(%workspace, %error, "the room's channels did not take their notices");
                }
            });
        }
        Ok(())
    }

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
    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<Vec<crate::workspace_records::WorkspaceAdmission>, String> {
        let mut admissions = Vec::with_capacity(messages.len());
        for message in messages {
            let commit = channel_commit(&message)?;
            admissions.push(
                self.inner
                    .host
                    .records
                    .enqueue(workspace_id, commit)
                    .await?,
            );
        }
        Ok(admissions)
    }
}

/// A channel governance message, retained as this relay's own.
fn channel_commit(
    message: &neighbour_wire::NeighbourMsg,
) -> Result<crate::workspace_records::HostCommit, String> {
    let (channel, retention) = neighbour_wire::retention(message, false)
        .ok_or("channel governance has no retention channel")?;
    let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    Ok(crate::workspace_records::HostCommit {
        body,
        channel,
        checkpoint: retention == neighbour_wire::Retention::Checkpoint,
    })
}
