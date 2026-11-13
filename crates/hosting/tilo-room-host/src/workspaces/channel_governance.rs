//! A host's part in its workspaces' channels: the governance messages it makes
//! (its access request, a channel's genesis, its commits) reach the members
//! that could act on them, and the governance members send for a channel
//! service the host runs waits here, one queue per workspace, until the
//! service takes it.

use std::sync::Arc;

use super::{Carried, Workspaces};
use tilo_relay_protocol::WorkspaceId;

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
        workspace_id: WorkspaceId,
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
    /// This host holds no channel keys of its own. What a governed member
    /// device sent, and only that, it keeps like any other governance, for
    /// whoever takes part later: a device that links reads every channel's
    /// genesis from it, and the host answers a request with what it keeps
    /// ([`tilo_peer_state::direct::DirectSync`]). One whose device's channel
    /// service it runs also queues it for that service, which settles the
    /// groups.
    ///
    /// What the relay already kept as the member's own write
    /// ([`Carried::Kept`]) is not kept again as the host's: each member's
    /// request was written to the workspace twice, a second job on its
    /// one-at-a-time queue for every request a crowd sends, and sent twice
    /// to every device that admits others.
    pub(super) async fn take_channel_governance(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        carried: Carried,
    ) -> Result<(), String> {
        // A host's word that a channel has no group is its own to say.
        if message.channel_workspace() != Some(workspace)
            || matches!(
                message,
                neighbour_wire::NeighbourMsg::MlsChannelUnfounded { .. }
            )
        {
            return Ok(());
        }
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
                    .governed_access(tilo_clock::wall())
                    .is_ok_and(|access| access.member_devices.contains(&author)),
            )
        };
        if !member {
            return Ok(());
        }
        if self
            .inner
            .host
            .channels
            .as_ref()
            .is_some_and(|channels| channels.sink.is_some())
        {
            self.channel_notice(workspace, message.clone())?;
        }
        let genesis = matches!(&message, neighbour_wire::NeighbourMsg::MlsGovernance { .. })
            .then(|| message.clone());
        if carried != Carried::Kept {
            self.retain_channel_governance(workspace_id, vec![message])
                .await?;
        }
        if let Some(genesis) = genesis
            && let Some(link) = self.inner.direct.link(workspace_id)
        {
            link.remember_genesis(author, &genesis);
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
        let flushing = crate::timing::start();
        publisher.flushed().await;
        crate::timing::event("channels.notices.flushed", flushing, 0);
        // Taken under the service: notices that queue while it works start the
        // next hand-over, which takes all of them at once.
        let waiting = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?
            .remove(workspace)
            .unwrap_or_default();
        crate::timing::event(
            "channels.notices.taken",
            crate::timing::start(),
            waiting.len() as u64,
        );
        sink.take(waiting);
        Ok(())
    }
    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: WorkspaceId,
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
