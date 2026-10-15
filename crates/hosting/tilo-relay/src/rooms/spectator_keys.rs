//! The content keys a public room's spectators open its channels with.
//!
//! A spectator is no channel member: the steward admits only the room's
//! governed member devices, one MLS leaf each, and a spectator's devices
//! come and go with page loads. In a public room anyone may join as a guest
//! and be admitted to every channel, so a visitor who only watches may read
//! what a guest would. The steward grants that to the spectator person, not
//! to each device: it hands any device the person certified, while that
//! device holds a seat in the room, the channel's content keys
//! ([`channel_crypto::ChannelContentKeys`], what an application endpoint
//! gets: enough to open, nothing that admits or removes anyone). Who may
//! take them is one admission decision,
//! [`crate::room_access::RoomOperation::ChannelReadKeys`], taken under the
//! room's lock right before the keys are read. A record a Read Deny keeps
//! from anyone is never opened to spectators, and no channel is ever
//! created for them.

use channel_crypto::ChannelContentKeys;
use tilo_relay_protocol::web::ChannelKeysRequest;

use super::{Rooms, SystemTime, readers};
use crate::room_access::Decision;
use crate::workspace::WorkspaceRuntime;

impl Rooms {
    /// The content keys of `route`'s channel for a spectator, when `admit`
    /// (the spectator's [`crate::room_access::RoomOperation::ChannelReadKeys`]
    /// decision over the room's runtime) admits it.
    ///
    /// # Errors
    ///
    /// Refuses a refused decision, a room this relay does not govern and
    /// steward, a record a Read Deny keeps from anyone, and a channel that
    /// does not exist yet.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision until its keys are read"
    )]
    pub(crate) async fn spectator_keys(
        &self,
        route: &ChannelKeysRequest,
        admit: impl FnOnce(&WorkspaceRuntime) -> Decision,
    ) -> Result<ChannelContentKeys, String> {
        let state = self
            .room(&route.room)
            .await
            .map_err(|error| error.to_string())?;
        let room = state.lock().await;
        let workspace_id = room.workspace_id();
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
        let channel = collab_types::document_channel_id(&route.app_id, &route.room, &route.record);
        let now = SystemTime::now();
        let access = room.peer().governance().governed_access(now)?;
        if readers(room.peer(), &access, room.stewardship.as_deref(), now)?.withholds(channel) {
            return Err(
                "this record is kept from some of the room, and from those who watch".into(),
            );
        }
        let steward = room
            .stewardship
            .clone()
            .ok_or("this relay does not steward the room's channels")?;
        let keys = steward.channels.lock().await.existing_keys(channel)?;
        tracing::debug!(room = %route.room, app = %route.app_id, record = %route.record,
            epoch = keys.epoch(), "handed a spectator the channel's content keys");
        Ok(keys)
    }
}
