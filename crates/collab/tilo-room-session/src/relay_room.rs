//! A room held over a relay session to one of its hosts, entered without
//! enrollment: a public room by redeeming its standing invitation at a host
//! its owner delegated admission to, or the direct room between two people,
//! whose hosts admit both of them by who they are.
//!
//! [`HeldRoom`] is the room itself: a [`StatePeer`] pinned to the room's
//! governing device, whatever carried its messages. A host keeps one
//! [`HeldRoomRecord`] per room in its own storage ([`HeldRoomStore`]), holds
//! one relay session per room ([`connect`]), hands what arrives to
//! [`HeldRoom::receive`] and publishes what [`HeldRoom::announcements`]
//! returns on every new session.
use crate::RoomId;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use tilo_relay::{RelayClient, RelayCoeditSession, coedit::Coverage};
use tilo_room::{NeighbourMsg, SeedState, StatePeer, TransportOrigin, seed_room};
use tilo_user_identity::{DeviceCert, RevocationList};

/// How a room was entered, which is how a new session enters it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entry {
    /// A public room, entered by redeeming this standing invitation.
    Public {
        /// The invitation's bearer string.
        invite: String,
    },
    /// The direct room with the person whose user root this is. This end
    /// created it and governs it.
    Direct {
        /// The other person's user root.
        other: [u8; 32],
    },
}

/// What a host keeps of one held room.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeldRoomRecord {
    pub room: RoomId,
    pub entry: Entry,
    /// The room's governing device, which the replica is pinned to.
    pub governor: DeviceCert,
    /// The hosts that carry the room, in the order they are tried.
    pub hosts: Vec<iroh::EndpointAddr>,
    /// The replica's durable state.
    pub peer: Vec<u8>,
}

/// Where a host keeps its held rooms, one record per room.
pub trait HeldRoomStore: Send + Sync {
    /// Every held room's record.
    ///
    /// # Errors
    /// A record cannot be read.
    fn held(&self) -> Result<Vec<HeldRoomRecord>, String>;
    /// Make `record` durable, replacing the room's previous one.
    ///
    /// # Errors
    /// The record cannot be written.
    fn save(&self, record: &HeldRoomRecord) -> Result<(), String>;
    /// Forget the room: it is no longer held.
    ///
    /// # Errors
    /// The record cannot be removed.
    fn remove(&self, room: RoomId) -> Result<(), String>;
}

/// A message this end publishes as the retained checkpoint of its channel:
/// `(channel, encoded message)`.
pub type Announcement = ([u8; 32], Vec<u8>);

/// One held room.
#[derive(Clone)]
pub struct HeldRoom {
    room: RoomId,
    entry: Entry,
    hosts: Vec<iroh::EndpointAddr>,
    peer: StatePeer,
}

impl std::fmt::Debug for HeldRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldRoom")
            .field("room", &self.room)
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}

impl HeldRoom {
    /// Hold the public room `invite` names, pinned to the owner device that
    /// minted it, as `own`. Its hosts admit `own` once it redeems `invite`.
    ///
    /// # Errors
    /// `invite` is not a valid standing invitation, names no room or no
    /// minting device, or a certificate does not verify.
    pub fn enter_public(invite: &str, own: &DeviceCert, now: SystemTime) -> Result<Self, String> {
        let parsed = neighbour_wire::invite::validate_invite_token(invite)?;
        if !parsed.standing {
            return Err("only a standing invitation enters a public room".into());
        }
        let room = RoomId::parse(&parsed.room_id).ok_or("the invitation names no room")?;
        let governor = parsed
            .owner_device_cert
            .ok_or("the invitation names no device of the room's owner")?;
        let peer = StatePeer::new(
            &room.to_string(),
            governor.clone(),
            vec![governor, own.clone()],
            RevocationList::new(),
            now,
        )?;
        Ok(Self {
            room,
            entry: Entry::Public {
                invite: invite.to_owned(),
            },
            hosts: parsed.host_addrs,
            peer,
        })
    }

    /// Create the direct room between `own`'s person and `other` (a user
    /// root), governed here, with `other` an Editor. `hosts` carry it.
    ///
    /// # Errors
    /// `other` is `own`'s own person, or governance cannot be written.
    pub fn open_direct(
        other: [u8; 32],
        hosts: Vec<iroh::EndpointAddr>,
        own: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Self, String> {
        let direct = tilo_reference::DirectRoom::between(
            &hex::encode(own.user_public_key),
            &hex::encode(other),
        )
        .ok_or("a direct room is between two different people")?;
        let room_id = direct.room_id();
        let room = RoomId::parse(&room_id).ok_or("the direct room id is not a room id")?;
        let mut peer = StatePeer::new(
            &room_id,
            own.clone(),
            vec![own.clone()],
            RevocationList::new(),
            now,
        )?;
        peer.install_seed(
            SeedState {
                room_changes: seed_room(&room_id, own, std::slice::from_ref(own))?,
                documents: Vec::new(),
            },
            key,
            now,
        )?;
        peer.admit_direct_member(other, key, now)?;
        Ok(Self {
            room,
            entry: Entry::Direct { other },
            hosts,
            peer,
        })
    }

    /// The room `record` holds, as `own` held it.
    ///
    /// # Errors
    /// The record does not restore.
    pub fn restore(
        record: HeldRoomRecord,
        own: &DeviceCert,
        now: SystemTime,
    ) -> Result<Self, String> {
        let peer = StatePeer::restore(
            &record.peer,
            &record.room.to_string(),
            record.governor.clone(),
            vec![record.governor, own.clone()],
            RevocationList::new(),
            now,
        )?;
        Ok(Self {
            room: record.room,
            entry: record.entry,
            hosts: record.hosts,
            peer,
        })
    }

    /// The record to keep for this room now.
    ///
    /// # Errors
    /// The replica cannot be encoded.
    pub fn record(&self) -> Result<HeldRoomRecord, String> {
        Ok(HeldRoomRecord {
            room: self.room,
            entry: self.entry.clone(),
            governor: self.peer.authority().clone(),
            hosts: self.hosts.clone(),
            peer: self.peer.durable()?,
        })
    }

    #[must_use]
    pub fn room(&self) -> RoomId {
        self.room
    }

    #[must_use]
    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    #[must_use]
    pub fn hosts(&self) -> &[iroh::EndpointAddr] {
        &self.hosts
    }

    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }

    /// The relay workspace carrying the room.
    ///
    /// # Errors
    /// The room id is not a room id.
    pub fn workspace(&self) -> Result<u64, String> {
        tilo_room::room_workspace_id(&self.room.to_string()).map_err(|e| e.to_string())
    }

    /// The invitation a new session redeems before it subscribes.
    #[must_use]
    pub fn redeems(&self) -> Option<[u8; 32]> {
        match &self.entry {
            Entry::Public { invite } => Some(neighbour_wire::invite::invite_id(invite)),
            Entry::Direct { .. } => None,
        }
    }

    /// Whether `device` holds a role in the room now.
    #[must_use]
    pub fn admits(&self, device: [u8; 32], now: SystemTime) -> bool {
        self.peer.permissions(device, now).is_ok()
    }

    /// Admit `body`, a message the relay attributed to `author`; `retained`
    /// when it was replayed from the relay's retained state rather than
    /// fanned out live. Returns whether the replica changed and so must be
    /// kept again.
    ///
    /// # Errors
    /// The message does not decode or the room refuses it.
    pub fn receive(
        &mut self,
        author: [u8; 32],
        body: &[u8],
        retained: bool,
        now: SystemTime,
    ) -> Result<bool, String> {
        let message: NeighbourMsg = postcard::from_bytes(body).map_err(|e| e.to_string())?;
        let origin = if retained {
            TransportOrigin::RetainedRelayDelta(author)
        } else {
            TransportOrigin::RelayDelta(author)
        };
        let outcome = self.peer.receive_from(origin, message, now)?;
        Ok(outcome.changed || outcome.metadata_changed || outcome.deferred)
    }

    /// What this end publishes on every new session: its device's hello and,
    /// where it governs the room, the room's governance, each as the
    /// retained checkpoint of its channel.
    ///
    /// # Errors
    /// A message cannot be encoded.
    pub fn announcements(
        &self,
        own: &DeviceCert,
        revocations: &RevocationList,
    ) -> Result<Vec<Announcement>, String> {
        let mut messages = vec![NeighbourMsg::IdentityHello {
            cert: Box::new(own.clone()),
            revocations: revocations
                .iter()
                .filter(|entry| entry.user_public_key == own.user_public_key)
                .cloned()
                .collect(),
        }];
        if self.peer.authority().device_public_key == own.device_public_key {
            messages.push(self.peer.room_message());
        }
        messages
            .into_iter()
            .map(|message| {
                let (channel, _) = tilo_room::transport::retention(&message, true)
                    .ok_or("an announcement has no retained channel")?;
                Ok((
                    channel,
                    postcard::to_stdvec(&message).map_err(|e| e.to_string())?,
                ))
            })
            .collect()
    }

    /// Publish [`Self::announcements`] on `session`.
    ///
    /// # Errors
    /// A message cannot be encoded or the session is gone.
    pub fn announce(
        &self,
        session: &RelayCoeditSession,
        own: &DeviceCert,
        revocations: &RevocationList,
    ) -> Result<(), String> {
        for (channel, body) in self.announcements(own, revocations)? {
            let entry = *blake3::hash(&body).as_bytes();
            session
                .publish_covering_checkpoint(channel, entry, Coverage::Supersedes, body)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Dial the room's hosts in order as `own` and join the room's session:
/// redeeming its standing invitation first for a public room, subscribing
/// straight away for a direct room. Returns the session and the host it
/// reached, which is who a refusal on it comes from.
///
/// # Errors
/// No host could be reached within 10 seconds each, or the room's id is not
/// a room id.
pub async fn connect(
    endpoint: iroh::Endpoint,
    room: &HeldRoom,
    own: &DeviceCert,
    key: &SigningKey,
    revocations: &RevocationList,
) -> Result<(RelayCoeditSession, iroh::EndpointId), String> {
    let workspace = room.workspace()?;
    let mut last = String::from("the room names no host");
    for host in room.hosts() {
        let dialed = tokio::time::timeout(
            Duration::from_secs(10),
            RelayClient::connect_to_pinned_host(
                endpoint.clone(),
                host.clone(),
                own.clone(),
                key.clone(),
                revocations.clone(),
            ),
        )
        .await;
        let client = match dialed {
            Ok(Ok(client)) => client,
            Ok(Err(error)) => {
                last = error.to_string();
                continue;
            }
            Err(_) => {
                last = "the room's host did not answer".into();
                continue;
            }
        };
        let joined = match room.redeems() {
            Some(invite) => {
                RelayCoeditSession::join_redeeming(client, workspace, own.device_public_key, invite)
            }
            None => RelayCoeditSession::join(client, workspace, own.device_public_key),
        };
        return joined
            .map(|session| (session, host.id))
            .map_err(|e| e.to_string());
    }
    Err(last)
}

#[cfg(test)]
mod tests;
