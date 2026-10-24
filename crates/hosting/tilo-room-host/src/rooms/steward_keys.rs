//! The channel keys a host's steward hands the device that governs and
//! stewards a room in the same process.
//!
//! A host that may own workspaces stewards the channels of the rooms it
//! governs ([`super::stewardship`]), whether it is a relay or a person's
//! device: its device created each channel's group, is a member of it and
//! holds its content keys. The same device's session (a device hosting its
//! own rooms) and the relay's own room actions open records under that leaf:
//! a group has one leaf per device, and the steward's is it. So the host
//! hands them the keys it holds, enough to seal and open content, not
//! enough to admit or remove anyone, after the one admission decision
//! [`RoomOperation::StewardedChannelKeys`], taken under the governed room's
//! lock right before the keys are read. The keys never leave the process.

use channel_crypto::ChannelContentKeys;

use super::{GovernedRoom, Rooms, SystemTime};
use crate::room_access::{self, Decision, RelayTrust, RoomOperation};

impl Rooms {
    /// The content keys of `channel`, one of `room`'s channels, for the
    /// device that governs and stewards the room here and runs in this
    /// process beside it: a device hosting its own rooms, whose session
    /// opens their records under the steward's own leaf. `route` is the
    /// channel's record when the device names it; `create` opens the channel
    /// when it has none yet, else only an existing channel is read. `None`
    /// when this host does not govern and steward the room.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision until its keys are read"
    )]
    pub async fn steward_channel_keys(
        &self,
        room: &str,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        create: bool,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let state = self.room(room).await.ok()?;
        let governed = state.lock().await;
        governed.stewardship.as_ref()?;
        if route.is_some_and(|route| !route.proves(room, channel)) {
            return Some(Err("the record is not this channel's".into()));
        }
        Some(
            self.stewarded_keys_locked(&governed, channel, route, create)
                .await,
        )
    }

    /// The steward's keys of `channel` in `room`, after the one admission
    /// decision that hands them out ([`RoomOperation::StewardedChannelKeys`]).
    pub(super) async fn stewarded_keys_locked(
        &self,
        room: &GovernedRoom,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        create: bool,
    ) -> Result<ChannelContentKeys, String> {
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
            tracing::warn!(room = %room.room(), reason = reason.reason(),
                "the steward's channel keys refused");
            return Err(reason.reason().into());
        }
        let steward = room
            .stewardship
            .clone()
            .ok_or("this relay does not steward the room's channels")?;
        if let Some(route) = route {
            steward.learn_route(channel, route);
        }
        let mut stewardship = steward.channels.lock().await;
        if create {
            self.open_record_channel(room, &steward, &mut stewardship, channel)
                .await?;
        }
        let keys = stewardship.existing_keys(channel)?;
        drop(stewardship);
        tracing::debug!(room = %room.room(), epoch = keys.epoch(),
            "handed the steward's channel keys");
        Ok(keys)
    }
    /// Open `channel` of `room` as its steward, creating its group when it
    /// has none, with the devices that may read its record admitted, and
    /// publish that change before anything is written there.
    pub(super) async fn open_record_channel(
        &self,
        room: &GovernedRoom,
        steward: &super::stewardship::Steward,
        stewardship: &mut super::stewardship::Stewardship,
        channel: collab_types::ChannelId,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let allowed = super::readers(
            room.peer(),
            &room.peer().governance().governed_access(now)?,
            Some(steward),
            now,
        )?;
        let messages = stewardship.open_channel(channel, &allowed)?;
        // After the channel changes a settle handed over.
        room.publisher.flushed().await;
        self.publish_channel_governance(room.workspace_id(), messages)
            .await
    }
}
