//! The content keys a public workspace's spectators open its channels with.
//!
//! A spectator is no channel member: participants admit only the workspace's
//! governed member devices, one MLS leaf each, and a spectator's devices
//! come and go with page loads. In a public workspace anyone may join as a guest
//! and be admitted to every channel, so a visitor who only watches may read
//! what a guest would. The node grants that to the spectator person, not
//! to each device: it hands any device the person certified, while that
//! device holds a seat in the workspace, the channel's content keys
//! ([`channel_crypto::ChannelContentKeys`], what an application endpoint
//! gets: enough to open, nothing that admits or removes anyone). Who may
//! take them is one admission decision,
//! [`crate::workspace_access::WorkspaceOperation::ChannelReadKeys`], taken under the
//! workspace's lock right before the keys are read. A record a Read Deny keeps
//! from anyone is never opened to spectators, and no channel is ever
//! created for them.

use channel_crypto::ChannelContentKeys;
use tilo_relay_protocol::web::ChannelKeysRequest;

use super::{SystemTime, Workspaces, readers};
use crate::workspace::WorkspaceRuntime;
use crate::workspace_access::Decision;

impl Workspaces {
    /// The content keys of `route`'s channel for a spectator, when `admit`
    /// (the spectator's [`crate::workspace_access::WorkspaceOperation::ChannelReadKeys`]
    /// decision over the workspace's runtime) admits it.
    ///
    /// # Errors
    ///
    /// Refuses a refused decision, a workspace this relay does not govern and take
    /// part in, a record a Read Deny keeps from anyone, and a channel that
    /// does not exist yet.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the decision until its keys are read"
    )]
    pub async fn spectator_keys(
        &self,
        route: &ChannelKeysRequest,
        admit: impl FnOnce(&WorkspaceRuntime) -> Decision,
    ) -> Result<ChannelContentKeys, String> {
        let state = self
            .workspace(&route.workspace)
            .await
            .map_err(|error| error.to_string())?;
        let runtime = state.lock().await;
        let workspace_id = runtime.workspace_id();
        let workspace = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or("this relay does not hold the room")?;
        let decision = admit(&*workspace.lock().await);
        if let Decision::Deny(reason) = decision {
            return Err(reason.reason().into());
        }
        let named = collab_types::ChannelRoute::new(&route.app_id, &route.record);
        let channel = named.channel(&route.workspace);
        let now = SystemTime::now();
        let access = runtime.peer().governance().governed_access(now)?;
        let readers = readers(
            runtime.peer(),
            &access,
            runtime.participation.as_deref(),
            Some(&named),
            now,
        )?;
        if readers.withholds(channel) {
            return Err(
                "this record is kept from some of the room, and from those who watch".into(),
            );
        }
        let participant = runtime
            .participation
            .clone()
            .ok_or("this relay does not take part in the room's channels")?;
        let keys = participant.channels.lock().await.existing_keys(channel)?;
        tracing::debug!(workspace = %route.workspace, app = %route.app_id, record = %route.record,
            epoch = keys.epoch(), "handed a spectator the channel's content keys");
        Ok(keys)
    }
}
