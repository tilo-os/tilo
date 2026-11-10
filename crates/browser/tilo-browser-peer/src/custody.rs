//! What a peer keeps, and how it keeps it.
//!
//! The device key itself never passes through here: it lives behind a
//! [`DeviceSigner`](tilo_relay_client::DeviceSigner) (a non-extractable
//! `WebCrypto` key in the page, a `SigningKey` natively). A [`PeerStore`]
//! keeps only public material — the device's certificate, the pinned host
//! certificate — and the commit outbox that must survive a reload.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

/// One commit the host has not acknowledged yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Commit id, reused when the commit is resent.
    pub op: [u8; 16],
    /// Relay workspace the commit targets.
    pub workspace_id: u64,
    /// Document route the commit belongs to.
    pub route: tilo_peer_state::document_frame::DocumentRoute,
    /// Retention channel ([`neighbour_wire::retention()`]) of the body.
    pub channel: [u8; 32],
    /// Whether the body is a retained checkpoint rather than a delta.
    pub checkpoint: bool,
    /// Encoded `NeighbourMsg` body.
    pub body: Vec<u8>,
}

/// The durable part of a browser peer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// This device's root-signed certificate.
    pub cert: Option<DeviceCert>,
    /// The host certificate pinned at certification.
    pub host_cert: Option<DeviceCert>,
    /// Commits not yet acknowledged, in submission order.
    pub outbox: Vec<OutboxEntry>,
}

/// Persistence for a [`PeerRecord`]. The page implements it over `IndexedDB`
/// (strict durability); a save counts only once it is durable.
pub trait PeerStore: tilo_relay_client::MaybeSendSync {
    fn load(&self) -> Option<PeerRecord>;
    /// Replace the stored record.
    fn save(&self, record: &PeerRecord) -> Result<(), String>;
    /// The stored channel state (encrypted-channel membership and the
    /// edits held for it), if any. A store without it keeps channel state
    /// for the session only.
    fn load_channels(&self) -> Option<Vec<u8>> {
        None
    }
    /// Replace the stored channel state.
    fn save_channels(&self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// The stored record of the workspaces this device holds
    /// ([`tilo_peer_state::owner::NativeWorkspaceOwner`]), if any. A store
    /// without it holds them for the session only.
    fn load_rooms(&self) -> Option<Vec<u8>> {
        None
    }
    /// Replace the stored record of the workspaces this device holds.
    fn save_rooms(&self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// The key channel state is sealed under at rest, kept apart from that
    /// state: in the page, wrapped by a non-extractable `WebCrypto` key. A
    /// store without one seals channel state under a key of the session's
    /// own, so stored state does not open in the next session.
    fn channel_vault(&self) -> Option<[u8; 32]> {
        None
    }
}

/// An in-memory [`PeerStore`] for native peers and tests.
pub struct MemoryPeerStore(
    Mutex<Option<PeerRecord>>,
    Mutex<Option<Vec<u8>>>,
    [u8; 32],
    Mutex<Option<Vec<u8>>>,
);

impl Default for MemoryPeerStore {
    fn default() -> Self {
        Self(
            Mutex::default(),
            Mutex::default(),
            rand::random(),
            Mutex::default(),
        )
    }
}

impl PeerStore for MemoryPeerStore {
    fn load(&self) -> Option<PeerRecord> {
        self.0.lock_recover().clone()
    }

    fn save(&self, record: &PeerRecord) -> Result<(), String> {
        *self.0.lock_recover() = Some(record.clone());
        Ok(())
    }

    fn load_channels(&self) -> Option<Vec<u8>> {
        self.1.lock_recover().clone()
    }

    fn save_channels(&self, bytes: &[u8]) -> Result<(), String> {
        *self.1.lock_recover() = Some(bytes.to_vec());
        Ok(())
    }

    fn channel_vault(&self) -> Option<[u8; 32]> {
        Some(self.2)
    }

    fn load_rooms(&self) -> Option<Vec<u8>> {
        self.3.lock_recover().clone()
    }

    fn save_rooms(&self, bytes: &[u8]) -> Result<(), String> {
        *self.3.lock_recover() = Some(bytes.to_vec());
        Ok(())
    }
}
