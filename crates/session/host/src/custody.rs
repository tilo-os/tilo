//! What a peer keeps, and how it keeps it.
//!
//! The device key itself never passes through here: it lives behind a
//! [`DeviceSigner`](tilo_relay_client::DeviceSigner) (a non-extractable
//! `WebCrypto` key in the page, a `SigningKey` natively). A [`PeerStore`]
//! keeps only public material — the device's certificate, the pinned host
//! certificate — and the commit outbox that must survive a reload.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use collab_types::{ChannelId, ChannelRoute};
use serde::{Deserialize, Serialize};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::DeviceCert;

/// The durable part of a device's session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// This device's root-signed certificate.
    pub cert: Option<DeviceCert>,
    /// The host certificate pinned at certification.
    pub host_cert: Option<DeviceCert>,
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

/// This device's channel state and the writes held for their keys, kept as one record of its
/// [`PeerStore`] ([`PeerStore::save_channels`]). Each channel's state is sealed under the store's
/// vault before it lands here.
#[derive(Clone, Debug, Default)]
pub(crate) struct StoredChannels(Arc<Stored>);

#[derive(Debug, Default)]
struct Stored {
    saved: Mutex<SavedChannels>,
    changed: AtomicBool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedChannels {
    peers: BTreeMap<(WorkspaceId, [u8; 16]), Vec<u8>>,
    routes: BTreeMap<(WorkspaceId, [u8; 16]), ChannelRoute>,
    /// Writes held until their route's keys arrive.
    held: Vec<(DocumentRoute, Vec<Vec<u8>>)>,
}

/// Tags stored channel state of this format: state of an earlier format does not open, and the
/// device asks for its channels again.
const SAVED_CHANNELS_TAG: &[u8] = b"tilo.browser-channels.v5";

impl StoredChannels {
    /// What `store` kept.
    pub(crate) fn load(store: &dyn PeerStore) -> Self {
        let saved = store
            .load_channels()
            .and_then(|bytes| postcard::from_bytes(bytes.strip_prefix(SAVED_CHANNELS_TAG)?).ok())
            .unwrap_or_default();
        Self(Arc::new(Stored {
            saved: Mutex::new(saved),
            changed: AtomicBool::new(false),
        }))
    }

    /// The writes held for their keys when the state was kept.
    pub(crate) fn held(&self) -> Vec<(DocumentRoute, Vec<Vec<u8>>)> {
        self.0.saved.lock_recover().held.clone()
    }

    /// Keep `held` as the writes waiting for their keys.
    pub(crate) fn hold(&self, held: Vec<(DocumentRoute, Vec<Vec<u8>>)>) {
        self.change(|saved| saved.held = held);
    }

    /// Write what changed since the last save to `store`.
    pub(crate) fn save(&self, store: &dyn PeerStore) -> Result<(), String> {
        if !self.0.changed.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let mut bytes = SAVED_CHANNELS_TAG.to_vec();
        postcard::to_io(&*self.0.saved.lock_recover(), &mut bytes)
            .map_err(|error| error.to_string())?;
        store.save_channels(&bytes).inspect_err(|_| {
            self.0.changed.store(true, Ordering::Release);
        })
    }

    fn change(&self, change: impl FnOnce(&mut SavedChannels)) {
        change(&mut self.0.saved.lock_recover());
        self.0.changed.store(true, Ordering::Release);
    }
}

impl tilo_channels::ChannelState for StoredChannels {
    fn peers(&self, workspace: WorkspaceId) -> Arc<dyn channel_crypto::PeerStates> {
        Arc::new(StoredPeers {
            stored: self.clone(),
            workspace,
        })
    }

    fn route(&self, workspace: WorkspaceId, channel: ChannelId) -> Option<ChannelRoute> {
        let route = (self.0.saved.lock_recover().routes)
            .get(&(workspace, channel.0.into_bytes()))
            .cloned()?;
        route
            .proves(&workspace.to_string(), channel)
            .then_some(route)
    }

    fn keep_route(&self, workspace: WorkspaceId, channel: ChannelId, route: &ChannelRoute) {
        self.change(|saved| {
            saved
                .routes
                .insert((workspace, channel.0.into_bytes()), route.clone());
        });
    }

    fn held(&self) -> Vec<(WorkspaceId, Vec<ChannelId>)> {
        let mut held: BTreeMap<WorkspaceId, Vec<ChannelId>> = BTreeMap::new();
        for (workspace, channel) in self.0.saved.lock_recover().peers.keys() {
            held.entry(*workspace)
                .or_default()
                .push(ChannelId(uuid::Uuid::from_bytes(*channel)));
        }
        held.into_iter().collect()
    }
}

/// One workspace's channels in [`StoredChannels`].
#[derive(Debug)]
struct StoredPeers {
    stored: StoredChannels,
    workspace: WorkspaceId,
}

impl channel_crypto::PeerStates for StoredPeers {
    fn read(&self, channel: ChannelId) -> channel_crypto::Result<Option<Vec<u8>>> {
        let saved = self.stored.0.saved.lock_recover();
        Ok(saved
            .peers
            .get(&(self.workspace, channel.0.into_bytes()))
            .cloned())
    }

    fn write(&self, channel: ChannelId, sealed: &[u8]) -> channel_crypto::Result<()> {
        self.stored.change(|saved| {
            saved
                .peers
                .insert((self.workspace, channel.0.into_bytes()), sealed.to_vec());
        });
        Ok(())
    }

    fn forget(&self, channel: ChannelId) -> channel_crypto::Result<()> {
        self.stored.change(|saved| {
            saved
                .peers
                .remove(&(self.workspace, channel.0.into_bytes()));
            saved
                .routes
                .remove(&(self.workspace, channel.0.into_bytes()));
        });
        Ok(())
    }
}

/// A page's or agent's proof that it still holds custody of a workspace: its host session
/// authenticated this device's certificate, and its host refuses a commit under any other. The
/// replica commits only while the workspace is held under that certificate.
pub(crate) struct SessionCustody;

impl tilo_session_client::custody::Custody for SessionCustody {
    fn check(
        &self,
        owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
        workspace: tilo_desktop_model::WorkspaceId,
        identity: &tilo_identity_host::custody::HostIdentity,
    ) -> Result<(), String> {
        owner.with_current(workspace, |view| {
            if view.device() == &identity.device_cert {
                Ok(())
            } else {
                Err("the workspace is held under another certificate of this device".to_owned())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_channels::ChannelState as _;

    #[test]
    fn channel_state_and_held_edits_survive_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let store = MemoryPeerStore::default();
        let route = DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "workspace".into(),
            record: "note".into(),
        };
        let (workspace, channel) = (WorkspaceId::new(), ChannelId::new());
        let held = vec![(route, vec![b"before the reload".to_vec()])];
        let stored = StoredChannels::load(&store);
        stored.hold(held.clone());
        stored.peers(workspace).write(channel, b"sealed")?;
        stored.save(&store)?;

        let reopened = StoredChannels::load(&store);
        if reopened.held() != held {
            return Err("the held edit changed on reopen".into());
        }
        if reopened.peers(workspace).read(channel)?.as_deref() != Some(&b"sealed"[..]) {
            return Err("the channel's state changed on reopen".into());
        }
        if tilo_channels::ChannelState::held(&reopened) != vec![(workspace, vec![channel])] {
            return Err("the reopened state does not list its channel".into());
        }
        Ok(())
    }
}
