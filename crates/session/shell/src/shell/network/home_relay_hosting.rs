//! Whether the person's home relay hosts each workspace this device
//! governs, and choosing it: the one place Devices & Sync reads and changes
//! it. What it reads is the room's own list of hosts, as its governor, this
//! device's host, records them ([`tilo_room_host::rooms::Rooms::hosts`]);
//! choosing adds or removes the relay as a hosting member
//! ([`ShellNetwork::add_hosting_member`]). Nothing is kept beside it.

use tilo_actions::remote::home_relay_hosting::{HomeRelayHosting, HostedWorkspace};

use super::{ShellNetwork, personal_continuity_topic_id, room_topic_id};
use crate::shell::state::RoomId;

impl ShellNetwork {
    /// The person's home relay and every workspace this device's host
    /// governs, each with whether the relay is one of its hosts. Blocks on
    /// the host: off the frame loop.
    ///
    /// # Errors
    ///
    /// The person's home relay cannot be read.
    pub fn home_relay_hosting(&self) -> Result<HomeRelayHosting, String> {
        let workspaces: Vec<(RoomId, String)> = {
            let tree = self.tree.read();
            tree.desktop
                .workspaces
                .iter()
                .map(|workspace| (workspace.room(), workspace.name.clone()))
                .collect()
        };
        let Some(host) = self.room_host() else {
            return Ok(HomeRelayHosting::default());
        };
        let runtime = self.runtime_handle()?;
        let mut relay = None;
        let mut hosted = Vec::new();
        for (room, name) in workspaces {
            // A room another host governs is not this device's to choose for.
            let Ok(hosts) = runtime.block_on(host.rooms().hosts(&room.to_string())) else {
                continue;
            };
            if relay.is_none() {
                let Some((_, home)) = self.home_relay_of(room)? else {
                    return Ok(HomeRelayHosting::default());
                };
                relay = Some(home);
            }
            let device = relay
                .as_ref()
                .map(|home| home.expectation.device_public_key);
            hosted.push(HostedWorkspace {
                room: room.to_string(),
                name,
                on_relay: hosts
                    .iter()
                    .any(|hint| Some(hint.host.device_public_key) == device),
            });
        }
        Ok(HomeRelayHosting {
            relay: relay.map(|home| home.label),
            workspaces: hosted,
        })
    }

    /// The workspace this device's own host operations are asked on, and
    /// the person's home relay, as this device acts in `room`; `None` when
    /// the person paired none.
    ///
    /// # Errors
    ///
    /// This device has no identity in `room`, or the home-peer registry
    /// cannot be read.
    pub(super) fn home_relay_of(
        &self,
        room: RoomId,
    ) -> Result<Option<(u64, super::RegisteredHomePeer)>, String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("native identity unavailable")?;
        let (certificate, _) = identity.current_native_material(room_topic_id(room))?;
        let user = certificate.user_public_key;
        let topics = super::relay_topics_for_home_peers(std::iter::empty(), Some(user));
        Ok(super::registered_home_peer_addrs(Some(user), &topics)?
            .into_iter()
            .next()
            .map(|relay| (personal_continuity_topic_id(&user), relay)))
    }

    /// Have the person's home relay host `room` as well, or no longer.
    /// Blocks on the host and the relay: off the frame loop.
    ///
    /// # Errors
    ///
    /// Why the relay was not added or removed.
    pub fn set_home_relay_hosting(&self, room: RoomId, on: bool) -> Result<(), String> {
        if on {
            self.add_hosting_member(room)
        } else {
            self.remove_hosting_member(room)
        }
    }
}
