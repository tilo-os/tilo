//! The channel keys this node hands its own endpoints in the same process.
//!
//! A host that may own workspaces takes part in the channels of the rooms it
//! governs ([`super::participation`]), whether it is a relay or a person's
//! device: its device is a member of each channel's group and holds its
//! content keys. The same device's session (a device hosting its own rooms)
//! and the relay's own room actions (`tilo_agent_participant::RoomActions`)
//! open records as that same device, with that same key, and a group has one
//! leaf per device, the node's. So the host hands them the keys it holds
//! ([`channel_crypto::ChannelContentKeys`]: enough to seal and open content,
//! not enough to admit or remove anyone), after the one admission decision
//! [`RoomOperation::OwnChannelKeys`], taken under the governed room's lock
//! right before the keys are read. The keys never leave the process. Who may
//! make the room actions act at all is decided there: only the relay owner's
//! person, through each record's own door.

use channel_crypto::ChannelContentKeys;
#[cfg(feature = "agents")]
use tilo_browser_peer::DocumentRoute;

use super::{GovernedRoom, Rooms, SystemTime};
use crate::room_access::{self, Decision, RelayTrust, RoomOperation};

impl Rooms {
    /// The content keys of `channel`, one of `room`'s channels, for the
    /// device that governs and takes part in the room here and runs in this
    /// process beside it: a device hosting its own rooms, whose session
    /// opens their records under the node's own leaf. `route` is the
    /// channel's record when the device names it; `create` opens the channel
    /// when it has none yet, else only an existing channel is read. `None`
    /// when this host does not govern and take part in the room.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision until its keys are read"
    )]
    pub async fn own_channel_keys(
        &self,
        room: &str,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        create: bool,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let state = self.room(room).await.ok()?;
        let governed = state.lock().await;
        governed.participation.as_ref()?;
        if route.is_some_and(|route| !route.proves(room, channel)) {
            return Some(Err("the record is not this channel's".into()));
        }
        Some(
            self.own_keys_locked(&governed, channel, route, create)
                .await,
        )
    }

    /// The node's keys of `channel` in `room`, after the one admission
    /// decision that hands them out ([`RoomOperation::OwnChannelKeys`]).
    pub(super) async fn own_keys_locked(
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
            RoomOperation::OwnChannelKeys,
        );
        if let Decision::Deny(reason) = decision {
            tracing::warn!(room = %room.room(), reason = reason.reason(),
                "the node's channel keys refused");
            return Err(reason.reason().into());
        }
        let participant = room
            .participation
            .clone()
            .ok_or("this relay does not take part in the room's channels")?;
        if let Some(route) = route {
            participant.learn_route(channel, route);
        }
        let mut participation = participant.channels.lock().await;
        if create {
            self.open_record_channel(room, &participant, &mut participation, channel)
                .await?;
        }
        let keys = participation.existing_keys(channel)?;
        drop(participation);
        tracing::debug!(room = %room.room(), epoch = keys.epoch(),
            "handed the node's channel keys");
        Ok(keys)
    }
    /// Open `channel` of `room` as the node, creating its group when it
    /// has none, with the devices that may read its record admitted, and
    /// publish that change before anything is written there.
    pub(super) async fn open_record_channel(
        &self,
        room: &GovernedRoom,
        participant: &super::participation::Participant,
        participation: &mut super::participation::Participation,
        channel: collab_types::ChannelId,
    ) -> Result<(), String> {
        let now = SystemTime::now();
        let allowed = super::readers(
            room.peer(),
            &room.peer().governance().governed_access(now)?,
            Some(participant),
            None,
            now,
        )?;
        let messages = participation.open_channel(channel, &allowed)?;
        // After the channel changes a settle handed over.
        room.publisher.flushed().await;
        self.publish_channel_governance(room.workspace_id(), messages)
            .await
    }
}

#[cfg(feature = "agents")]
impl Rooms {
    /// The content keys of `route`'s channel, for this relay's own room
    /// actions. `create` opens the channel when the room actions are about
    /// to write a record that has none yet; otherwise only an existing
    /// channel is read and none is ever created. `None` when this relay does
    /// not govern and take part in the room: a participant admits the room
    /// actions over the channel like any member.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked from the decision until its keys are read"
    )]
    pub async fn own_endpoint_keys(
        &self,
        route: &DocumentRoute,
        create: bool,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let state = self.room(&route.cluster_id).await.ok()?;
        let room = state.lock().await;
        room.participation.as_ref()?;
        Some(self.own_endpoint_keys_locked(&room, route, create).await)
    }

    async fn own_endpoint_keys_locked(
        &self,
        room: &super::GovernedRoom,
        route: &DocumentRoute,
        create: bool,
    ) -> Result<ChannelContentKeys, String> {
        if route.workspace_id().ok() != Some(room.workspace_id()) {
            return Err("the record is not in this room".into());
        }
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
        // This relay's own endpoint names the record it writes: the channel
        // is decided by it from its first admission.
        let route = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        self.own_keys_locked(room, channel, Some(&route), create)
            .await
    }
}
