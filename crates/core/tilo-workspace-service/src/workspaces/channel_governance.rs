//! A host's part in its workspaces' channels: the governance messages it makes
//! (its access request, a channel's genesis, its commits) reach the members
//! that could act on them, and the governance members send for a channel
//! service the host runs waits here, one queue per workspace, until the
//! service takes it.

use super::{Carried, Workspaces};
use tilo_server_protocol::WorkspaceId;

/// Notices one workspace hands its channel service at once; a device asks
/// again when its request is dropped.
const MAX_QUEUED_CHANNEL_NOTICES: usize = 16_384;

impl Workspaces {
    /// Publish channel governance messages as this server's own: each is kept
    /// at the device store and carried on the workspace's link, to the member
    /// devices linked now and to whoever links later. Without it the host's
    /// access request never reaches the key holders that could admit it, nor
    /// its genesis or commits, so a host is never let into a channel a member
    /// opened while every member's request reaches the host.
    ///
    /// # Errors
    ///
    /// The messages could not be kept, or this host holds no such
    /// workspace.
    pub async fn publish_governance(
        &self,
        workspace: &str,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<(), String> {
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let workspace_id = governed.lock().await.workspace_id();
        let own = self.inner.records.device();
        self.keep_channel_rules(workspace_id, own, &messages)
    }

    /// Keep `author`'s channel rules `messages` and carry them on the
    /// workspace's link.
    fn keep_channel_rules(
        &self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        messages: &[neighbour_wire::NeighbourMsg],
    ) -> Result<(), String> {
        for message in messages {
            let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
            self.inner.records.carry(
                workspace_id,
                author,
                &crate::workspace_documents::HostCommit { body },
            )?;
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

    /// Take in `author`'s access request or commit for a channel of `workspace`:
    /// a member's device, or a device of a person the workspace has as a member,
    /// as the person's own laptop is.
    ///
    /// This host holds no channel keys of its own. What a governed device sent,
    /// and only that, it keeps like any other governance, for
    /// whoever takes part later: a device that links reads every channel's
    /// genesis from it, and the host answers a request with what it keeps
    /// ([`tilo_peer_state::direct::DirectSync`]). One whose device's channel
    /// service it runs also queues it for that service, which settles the
    /// groups.
    ///
    /// What the workspace's link already kept and carried
    /// ([`Carried::Kept`]) is not kept again.
    pub(super) async fn take_governance(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        carried: Carried,
    ) -> Result<(), String> {
        if message.governance_workspace() != Some(workspace) {
            return Ok(());
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let (workspace_id, governed_by) = {
            let governed = state.lock().await;
            let peer = governed.peer();
            let governance = peer.governance();
            let access = governance.governed_access(tilo_clock::wall());
            let access = access.as_ref().ok();
            let member = access.is_some_and(|access| access.member_devices.contains(&author));
            // A device is one the workspace governs by the same rule wherever it asks: it is a
            // member's device, or a device of a person the workspace has as a member, as the
            // person's laptop is. What it certifies is its certificate's user key, held here
            // among the devices the workspace knows or the holders it names.
            let their_person = peer
                .certificates()
                .iter()
                .find(|cert| cert.device_public_key == author)
                .map(|cert| cert.user_public_key)
                .or_else(|| {
                    governance
                        .keeping_devices()
                        .into_iter()
                        .find(|hint| hint.host.device_public_key == author)
                        .map(|hint| hint.host.user_public_key)
                });
            let person = their_person.is_some_and(|person| {
                person == governance.owner_user_public_key()
                    || access.is_some_and(|access| access.member_user_roots.contains(&person))
            });
            (governed.workspace_id(), member || person)
        };
        if !governed_by {
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
        if carried != Carried::Kept {
            self.keep_channel_rules(workspace_id, author, &[message])?;
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
                if let Err(error) = workspaces.take_channel_notices(&workspace) {
                    tracing::warn!(%workspace, %error, "the workspace's channels did not take their notices");
                }
            });
        }
        Ok(())
    }

    /// Hand `workspace`'s channel service every queued notice: the workspace's
    /// own governance is retained before the channel governance a service
    /// built on it decides them with.
    pub(super) fn take_channel_notices(&self, workspace: &str) -> Result<(), String> {
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
}
