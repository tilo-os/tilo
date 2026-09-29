//! The steward of the encrypted channels of the room this host governs.
//!
//! A room's governing device stewards its channels
//! (`tilo_action_broker::channel_encryption`). For a room a device host
//! governs, that device is the host's native workspace peer: members, a
//! foreign runtime among them, ask it for access over the room's relay
//! session, and it admits every certified device the room's governance lets
//! read. The durable owner still keeps sealed writes only as ciphertext; the
//! channel keys live in this steward's sealed state alone.
use crate::SessionOwner;
use collab_types::{Ed25519PublicKey, Role};
use std::{
    collections::BTreeSet,
    io,
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_action_broker::{
    channel_encryption::{
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        GovernanceTransport, InboundChannelGovernance,
    },
    collab_core::BrokerChannels,
};
use tilo_desktop_model::RoomId;
use tilo_peer_state::{NeighbourMsg, StatePeer};
use tilo_relay::RelayCoeditSession;

/// Sealed channel state, under the owner's root.
const CHANNEL_STATE: &str = "channels";
/// Derives the key sealing that state from the steward's device key, which
/// already sits in the owner's private identity storage.
const VAULT_CONTEXT: &str = "tilo workspace host channel state v1";

type Readers = Arc<Mutex<BTreeSet<Ed25519PublicKey>>>;

/// This host's channel service for its governed room.
pub struct Steward {
    keys: ChannelKeyAccess,
    readers: Readers,
}

impl Steward {
    /// Open the steward's channel state under `owner`'s root.
    ///
    /// # Errors
    /// The owner's room is invalid or the state directory cannot be opened.
    pub fn open(owner: &SessionOwner) -> io::Result<Self> {
        let room = RoomId::parse(owner.peer().room())
            .ok_or_else(|| io::Error::other("the governed room is invalid"))?;
        let device_key = owner.identity.native.clone();
        let readers = Readers::default();
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: owner.root().join(CHANNEL_STATE),
                vault: channel_crypto::VaultKey::new(blake3::derive_key(
                    VAULT_CONTEXT,
                    &device_key.to_bytes(),
                )),
                device_key,
            },
            Box::new(Directory {
                room,
                readers: Arc::clone(&readers),
            }),
            Arc::new(Mutex::new(BrokerChannels::default())),
        )?;
        Ok(Self {
            keys: ChannelKeyAccess::new(service),
            readers,
        })
    }

    /// The channels' keys, which only this steward holds on the host.
    #[must_use]
    pub fn keys(&self) -> &ChannelKeyAccess {
        &self.keys
    }

    /// Run the protocol once over `transport`, against the room's current
    /// governance in `owner`.
    pub fn pump(&self, owner: &SessionOwner, transport: &dyn GovernanceTransport) {
        if let Ok(mut readers) = self.readers.lock() {
            *readers = readers_of(owner.peer(), SystemTime::now());
        }
        self.keys.pump(transport);
    }
}

/// Whether `message` is channel governance, which only the steward reads.
#[must_use]
pub fn is_governance(message: &NeighbourMsg) -> bool {
    matches!(
        message,
        NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. }
    )
}

/// Every device that may read the room's channels at `now`: each certified,
/// unrevoked, unexpired device whose role can read. An always-on host is
/// never a person's reading device.
fn readers_of(peer: &StatePeer, now: SystemTime) -> BTreeSet<Ed25519PublicKey> {
    peer.certificates()
        .into_iter()
        .filter(|cert| !cert.capabilities.can_act_as_host)
        .filter(|cert| {
            matches!(
                peer.permissions(cert.device_public_key, now),
                Ok((
                    Role::Owner | Role::Editor | Role::Commenter | Role::Viewer,
                    _,
                    _
                ))
            )
        })
        .map(|cert| Ed25519PublicKey::from_bytes(cert.device_public_key))
        .collect()
}

struct Directory {
    room: RoomId,
    readers: Readers,
}

impl ChannelDirectory for Directory {
    fn governs(&self, room: RoomId) -> bool {
        room == self.room
    }

    fn steward_of(&self, _room: RoomId) -> Option<Ed25519PublicKey> {
        None
    }

    fn allowed_devices(&self, room: RoomId, _now: u64) -> BTreeSet<Ed25519PublicKey> {
        if room != self.room {
            return BTreeSet::new();
        }
        self.readers
            .lock()
            .map(|readers| readers.clone())
            .unwrap_or_default()
    }
}

/// A room's relay session as the steward's transport. Governance goes to
/// every member as room presence, which the host fans out; it is signed end
/// to end, and the steward redelivers until each device acknowledges.
pub struct RelayGovernance<'a> {
    inbound: Mutex<Vec<InboundChannelGovernance>>,
    session: &'a RelayCoeditSession,
}

impl<'a> RelayGovernance<'a> {
    /// `session`, with the governance it delivered since the last pump.
    #[must_use]
    pub fn new(session: &'a RelayCoeditSession, inbound: Vec<InboundChannelGovernance>) -> Self {
        Self {
            inbound: Mutex::new(inbound),
            session,
        }
    }
}

impl GovernanceTransport for RelayGovernance<'_> {
    fn publish(&self, message: NeighbourMsg) -> bool {
        postcard::to_allocvec(&message)
            .is_ok_and(|bytes| self.session.publish_presence(bytes).is_ok())
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        self.inbound
            .lock()
            .map(|mut inbound| std::mem::take(&mut *inbound))
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "channel_steward_tests.rs"]
mod tests;
