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
//! layout, who its members are, and who takes part in its channels.
//!
//! A workspace the host binds for its own person without governing it (a
//! same-owner room) has no governance and no bootstrap: its members are the
//! person's own devices the host admits. There the page takes the devices
//! whose certificates the host relays, of its own person and not revoked by
//! them, and the host when its person certified it to own workspaces.

use std::collections::{BTreeMap, BTreeSet};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::projection::{RoomLayout, room_layout, room_members};
use tilo_peer_state::join::JoinTransportResponse;
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};
use web_time::SystemTime;

use crate::channels::Channels;
use crate::document::DocumentRoute;

/// The rooms this page joined, by workspace.
pub(crate) struct Replicas {
    /// This device.
    device: [u8; 32],
    peers: BTreeMap<u64, StatePeer>,
    /// The workspaces with no governance: their person's devices as the
    /// host relayed them.
    ungoverned: Ungoverned,
}

/// What a page knows of the workspaces the host binds for its person
/// without governing them.
struct Ungoverned {
    /// This device's person.
    user: [u8; 32],
    /// The host, when its person certified it to own workspaces.
    host: Option<[u8; 32]>,
    /// What this device's person revoked.
    revocations: RevocationList,
    devices: BTreeMap<u64, BTreeMap<[u8; 32], DeviceCert>>,
}

impl Ungoverned {
    /// Take in the certificate `author` announced on `workspace_id`, and
    /// what its person revoked; returns whether the devices changed.
    fn hello(
        &mut self,
        workspace_id: u64,
        author: [u8; 32],
        cert: DeviceCert,
        revocations: Vec<RevocationEntry>,
    ) -> Result<bool, String> {
        for entry in revocations {
            if entry.user_public_key == self.user {
                let _ = self.revocations.insert(entry);
            }
        }
        if cert.device_public_key != author || cert.user_public_key != self.user {
            return Err("the certificate is not its sender's own person's".into());
        }
        tilo_user_identity::verify_device_cert(&cert).map_err(|error| error.to_string())?;
        Ok(self
            .devices
            .entry(workspace_id)
            .or_default()
            .insert(author, cert)
            .is_none())
    }

    fn participants(&self, workspace_id: u64) -> BTreeSet<Ed25519PublicKey> {
        self.devices
            .get(&workspace_id)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .chain(&self.host)
            .filter(|device| !self.revocations.is_revoked(device))
            .map(|device| Ed25519PublicKey::from_bytes(*device))
            .collect()
    }
}

impl Replicas {
    /// The rooms of `device`, of the person `user`, reaching them through
    /// the host `host`; `revocations` are what its person revoked.
    pub(crate) fn new(
        device: [u8; 32],
        user: [u8; 32],
        host: &DeviceCert,
        revocations: RevocationList,
    ) -> Self {
        let host = (host.capabilities.can_own_workspaces
            && !host.capabilities.foreign
            && host.user_public_key == user
            && tilo_user_identity::verify_device_cert(host).is_ok())
        .then_some(host.device_public_key);
        Self {
            device,
            peers: BTreeMap::new(),
            ungoverned: Ungoverned {
                user,
                host,
                revocations,
                devices: BTreeMap::new(),
            },
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
            return match message {
                NeighbourMsg::IdentityHello {
                    cert, revocations, ..
                } => self
                    .ungoverned
                    .hello(workspace_id, author, *cert, revocations),
                _ => Err("the workspace has no governance".into()),
            };
        };
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        let outcome = peer.receive_from(origin, message, now)?;
        Ok(outcome.changed || outcome.metadata_changed)
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
    /// this device; in a workspace with no governance, its person's devices
    /// the host relayed and the host. `None` while the room's governance
    /// does not answer for the route.
    pub(crate) fn participants(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        let workspace_id = route.workspace_id().ok()?;
        let Some(peer) = self.peers.get(&workspace_id) else {
            let mut participants = self.ungoverned.participants(workspace_id);
            participants.insert(Ed25519PublicKey::from_bytes(self.device));
            return Some(participants);
        };
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
