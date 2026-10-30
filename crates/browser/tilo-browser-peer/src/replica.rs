//! Each joined room as its governance admits it: one [`StatePeer`] per
//! room, the replica native devices hold of a room they joined.
//!
//! The host sends a subscriber the room's bootstrap ahead of its replay
//! ([`tilo_relay_protocol::Envelope::RoomBootstrap`]): the workspace-join
//! contract's membership answer, the room's governing authority, roster and
//! revocations a native device enrolls with. The page builds the room's
//! replica from it and admits every governance message the host relays
//! through it: members' certificates and the room document. What the room's
//! governance says is read off that replica: whether it shares its members'
//! layout, who its members are, who takes part in its channels, and whether
//! a member's write reaches the page's apps. The replica decides each
//! document as native devices do (its author's certificate and role, its
//! signature and route, its claims against the app surface it names) and
//! keeps none: the page's apps hold their records.
//!
//! A room's replica is the only source of what its governance says: a
//! workspace with no governance yet (no bootstrap received) admits no
//! member's write and names no channel participants. A page places its
//! person's own workspaces on their host before joining them
//! ([`crate::PeerHandle::join_placed`]), so those have governance too.

use std::collections::{BTreeMap, BTreeSet};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::projection::{RoomLayout, room_layout, room_members};
use tilo_peer_state::join::JoinTransportResponse;
use tilo_peer_state::{StatePeer, TransportOrigin};
use web_time::SystemTime;

use crate::channels::Channels;
use crate::document::DocumentRoute;

/// The rooms this page joined, by workspace.
pub(crate) struct Replicas {
    /// This device.
    device: [u8; 32],
    peers: BTreeMap<u64, StatePeer>,
}

impl Replicas {
    /// The rooms of `device`.
    pub(crate) fn new(device: [u8; 32]) -> Self {
        Self {
            device,
            peers: BTreeMap::new(),
        }
    }

    /// Build `workspace_id`'s replica from the bootstrap the host sent
    /// (`bytes`, a postcard [`JoinTransportResponse`]); returns whether the
    /// replica is new. A replica of the same room and authority stays: a
    /// resumed subscription replays only what it has not received.
    ///
    /// # Errors
    /// The bootstrap is not a membership answer for this device and room,
    /// or the room it names does not build.
    pub(crate) fn bootstrap(
        &mut self,
        workspace_id: u64,
        bytes: &[u8],
        now: SystemTime,
    ) -> Result<bool, String> {
        let (device, room, workspace, authority, roster, revocations) =
            match postcard::from_bytes(bytes).map_err(|error| error.to_string())? {
                JoinTransportResponse::MembershipComplete(bootstrap) => (
                    bootstrap.device_cert,
                    bootstrap.room,
                    bootstrap.workspace_id,
                    bootstrap.authority,
                    bootstrap.certified_roster,
                    bootstrap.revocations,
                ),
                JoinTransportResponse::OwnerMembershipComplete(bootstrap) => (
                    bootstrap.device_cert,
                    bootstrap.room,
                    bootstrap.workspace_id,
                    bootstrap.authority,
                    bootstrap.certified_roster,
                    bootstrap.revocations,
                ),
                _ => return Err("the room's bootstrap is no membership answer".into()),
            };
        if workspace != workspace_id {
            return Err("the room's bootstrap names another room".into());
        }
        if device.device_public_key != self.device {
            return Err("the room's bootstrap names another device".into());
        }
        if self
            .peers
            .get(&workspace_id)
            .is_some_and(|peer| peer.room() == room && peer.authority() == &authority)
        {
            return Ok(false);
        }
        let peer = StatePeer::from_join(&room, authority, roster, &revocations, now)?;
        #[cfg(feature = "toolkit")]
        let peer = {
            let mut peer = peer;
            peer.set_surfaces(page_surfaces());
            peer
        };
        self.peers.insert(workspace_id, peer);
        Ok(true)
    }

    /// Admit `message`, governance `author` sent to `workspace_id`;
    /// `retained` while the host replays the room. Returns whether it
    /// changed what the room's governance says.
    ///
    /// # Errors
    /// No replica of the room is held, or its governance refuses the
    /// message.
    pub(crate) fn admit(
        &mut self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(peer) = self.peers.get_mut(&workspace_id) else {
            return Err("the workspace has no governance".into());
        };
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        let outcome = peer.receive_from(origin, message, now)?;
        Ok(outcome.changed || outcome.metadata_changed)
    }

    /// Decide `message`, a document `author` wrote to `workspace_id`;
    /// `retained` while the host replays the room. `Ok(true)` lets it reach
    /// the page's apps, `Ok(false)` holds it until the room's governance
    /// can decide it.
    ///
    /// # Errors
    /// The room's governance refuses it.
    pub(crate) fn decide_document(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        // Held until the room's bootstrap arrives.
        let Some(peer) = self.peers.get(&workspace_id) else {
            return Ok(false);
        };
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        peer.decide_relayed_document(origin, message, now)
    }

    /// `workspace_id`'s room, whether its governance keeps each member's
    /// layout their own, and the members it lists by person id.
    pub(crate) fn layout(
        &self,
        workspace_id: u64,
    ) -> Option<(String, RoomLayout, Option<BTreeSet<String>>)> {
        let peer = self.peers.get(&workspace_id)?;
        let doc = peer.admitted_room();
        Some((
            peer.room().to_owned(),
            doc.map(room_layout).unwrap_or_default(),
            doc.and_then(room_members),
        ))
    }

    /// The devices that may sign and be added to `route`'s channel: those
    /// the room's governance lets read its record, the room's governor and
    /// this device. `None` while the room's governance does not answer for
    /// the route.
    pub(crate) fn participants(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        let workspace_id = route.workspace_id().ok()?;
        let peer = self.peers.get(&workspace_id)?;
        let channel = Channels::channel(route);
        let record = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        let readers = peer
            .governance()
            .channel_readers(SystemTime::now(), channel, Some(&record))
            .ok()
            .flatten()?;
        let mut participants: BTreeSet<Ed25519PublicKey> = readers
            .into_iter()
            .map(Ed25519PublicKey::from_bytes)
            .collect();
        participants.insert(Ed25519PublicKey::from_bytes(
            peer.authority().device_public_key,
        ));
        participants.insert(Ed25519PublicKey::from_bytes(self.device));
        Some(participants)
    }

    /// One line per room for diagnostics: who governs it.
    pub(crate) fn describe(&self) -> String {
        self.peers
            .values()
            .map(|peer| {
                format!(
                    "room {}: governed by {}",
                    peer.room(),
                    hex::encode(&peer.authority().device_public_key[..4])
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The app surfaces a page decides members' writes against: those of the
/// apps it runs, by the hash a write names. A write naming another version's
/// surface waits until the page runs that version, as it would on a native
/// device that has not fetched it.
#[cfg(feature = "toolkit")]
fn page_surfaces() -> collab_types::Surfaces {
    collab_types::Surfaces::from_source(std::sync::Arc::new(
        collab_types::surface::ContentSurfaces::new(|record, hash| {
            let running = ui::app_surface::hash_of(record.app)?;
            (hex::encode(running) == hash)
                .then(|| ui::app_surface::bytes_of(record.app).map(<[u8]>::to_vec))
                .flatten()
        }),
    ))
}
