//! The channel keys this relay's own room actions seal and open with.
//!
//! A relay that may own workspaces stewards the channels of the rooms it
//! governs ([`super::stewardship`]): its device created each channel's group,
//! is a member of it and holds its content keys. The relay's room actions
//! (`tilo_agent_participant::RoomActions`) join those rooms as that same
//! device, with that same key. They cannot be admitted to a channel as a
//! member of their own: a group has one leaf per device, and the steward's
//! is it. So the device hands its room actions the keys it holds, the way a
//! device's session service hands its applications theirs
//! ([`channel_crypto::ChannelContentKeys`]: enough to seal and open content,
//! not enough to admit or remove anyone).
//!
//! The hand-off is one admission decision,
//! [`RoomOperation::StewardedChannelKeys`], taken under the governed room's
//! lock right before the keys are read: the keys go to the device that
//! governs and stewards the room on this relay, while its certificate is
//! current, and to nothing else. The keys never leave the relay process.
//! Who may make the room actions act at all is decided there: only the relay
//! owner's person, through each record's own door.

use channel_crypto::ChannelContentKeys;
use tilo_browser_peer::DocumentRoute;

use super::{Rooms, SystemTime, readers};
use crate::room_access::{self, Decision, RelayTrust, RoomOperation};

impl Rooms {
    /// The content keys of `route`'s channel, for this relay's own room
    /// actions. `create` opens the channel when the room actions are about
    /// to write a record that has none yet; otherwise only an existing
    /// channel is read and none is ever created. `None` when this relay does
    /// not govern and steward the room: another device stewards it, and
    /// admits the room actions over the channel like any member.
    ///
    /// # Errors
    ///
    /// Refuses a refused decision, and a channel that does not exist when
    /// `create` is false.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision until its keys are read"
    )]
    pub(crate) async fn steward_endpoint_keys(
        &self,
        route: &DocumentRoute,
        create: bool,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let state = self.room(&route.cluster_id).await.ok()?;
        let room = state.lock().await;
        room.stewardship.as_ref()?;
        Some(
            self.steward_endpoint_keys_locked(&room, route, create)
                .await,
        )
    }

    async fn steward_endpoint_keys_locked(
        &self,
        room: &super::GovernedRoom,
        route: &DocumentRoute,
        create: bool,
    ) -> Result<ChannelContentKeys, String> {
        let workspace_id = room.workspace_id();
        if route.workspace_id().ok() != Some(workspace_id) {
            return Err("the record is not in this room".into());
        }
        let workspace = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or("this relay does not hold the room")?;
        let own = crate::auth::AuthenticatedPeer {
            cert: self.inner.host.cert.clone(),
            user_public_key: self.inner.host.cert.user_public_key,
            device_public_key: self.inner.host.cert.device_public_key,
            role: crate::auth::PeerRole::from_cert(&self.inner.host.cert),
        };
        let trust = RelayTrust {
            revocations: &self.inner.host.revocations,
            policy: &self.inner.host.trust_policy,
            own_user: &self.inner.host.cert.user_public_key,
            agents: &self.inner.host.agents,
        };
        let decision = room_access::decide(
            &*workspace.lock().await,
            &trust,
            &own,
            RoomOperation::StewardedChannelKeys,
        );
        if let Decision::Deny(reason) = decision {
            tracing::warn!(room = %route.cluster_id, reason = reason.reason(),
                "room actions: the steward's channel keys refused");
            return Err(reason.reason().into());
        }
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
        let steward = room
            .stewardship
            .clone()
            .ok_or("this relay does not steward the room's channels")?;
        // This relay's own endpoint names the record it writes: the channel
        // is decided by it from its first admission.
        steward.learn_route(
            channel,
            &collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str()),
        );
        let now = SystemTime::now();
        let allowed = readers(
            room.peer(),
            &room.peer().governance().governed_access(now)?,
            Some(&*steward),
            now,
        )?;
        let mut stewardship = steward.channels.lock().await;
        if create {
            let messages = stewardship.open_channel(channel, &allowed)?;
            // After the channel changes a settle handed over.
            room.publisher.flushed().await;
            self.publish_channel_governance(workspace_id, messages)
                .await?;
        }
        let keys = stewardship.existing_keys(channel)?;
        drop(stewardship);
        tracing::debug!(room = %route.cluster_id, app = %route.app_id, record = %route.record,
            epoch = keys.epoch(), "room actions: handed the steward's channel keys");
        Ok(keys)
    }
}
