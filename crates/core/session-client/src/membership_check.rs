//! Whether this device still owns a workspace, as its host checks it.
//!
//! The owner of a workspace commits only what this device still owns: a device the
//! workspace removed, or whose certificate ran out, finds out before it commits. Each host keeps
//! its own proof of that — this desktop's signed ownership over the peer network, a page's
//! over its own transport.
//!
//! A session's [`PeerStore`] is what a peer keeps between sessions. The device key itself never passes through here: it lives behind a
//! [`DeviceSigner`](tilo_server_client::DeviceSigner) (a non-extractable
//! `WebCrypto` key in the page, a `SigningKey` natively). A [`PeerStore`]
//! keeps only public material — the device's certificate, the pinned host
//! certificate — and the commit outbox that must survive a reload.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::owner::DeviceWorkspaces;
use tilo_user_identity::DeviceCert;

/// The durable part of a device's session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerEntry {
    /// This device's root-signed certificate.
    pub cert: Option<DeviceCert>,
    /// The host certificate pinned at certification.
    pub server_cert: Option<DeviceCert>,
}

/// Persistence for a [`PeerEntry`]. The page implements it over `IndexedDB`
/// (strict durability); a save counts only once it is durable.
pub trait PeerStore: tilo_server_client::MaybeSendSync {
    fn load(&self) -> Option<PeerEntry>;
    /// Replace the stored record.
    fn save(&self, record: &PeerEntry) -> Result<(), String>;
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
    /// ([`tilo_peer_state::owner::DeviceWorkspaces`]), if any. A store
    /// without it holds them for the session only.
    fn load_workspaces(&self) -> Option<Vec<u8>> {
        None
    }
    /// Replace the stored record of the workspaces this device holds.
    fn save_workspaces(&self, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// A part of the record of the workspaces this device holds, kept apart
    /// from its index so a publication writes only what changed
    /// ([`tilo_peer_state::owner::Journaled`]). A store without them keeps the
    /// workspaces it holds for the session only.
    fn load_workspaces_part(&self, _name: &str) -> Option<Vec<u8>> {
        None
    }
    /// Replace the part `name` of that record.
    fn save_workspaces_part(&self, _name: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// Append to the part `name` of that record, which need not exist yet.
    fn append_workspaces_part(&self, _name: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
    /// Whether the parts [`Self::save_workspaces_part`] writes are kept at
    /// all: a store that answers neither of the part calls holds the record
    /// whole instead.
    fn keeps_workspaces_parts(&self) -> bool {
        false
    }
    /// Whether [`Self::save_workspaces`] keeps what it is handed for a later session. A store that
    /// keeps nothing is handed nothing: the record of a device's workspaces holds every record it
    /// replicates, and making it after each write costs as much as all of them.
    fn keeps_workspaces(&self) -> bool {
        false
    }
    /// The key channel state is sealed under at rest, kept apart from that
    /// state: in the page, wrapped by a non-extractable `WebCrypto` key. A
    /// store without one seals channel state under a key of the session's
    /// own, so stored state does not open in the next session.
    fn channel_vault(&self) -> Option<[u8; 32]> {
        None
    }
}

/// An in-memory [`PeerStore`] for native peers and tests: by default it keeps the record of the
/// device's workspaces for a session started again over it, as a page's profile does.
pub struct MemoryPeerStore(
    Mutex<Option<PeerEntry>>,
    Mutex<Option<Vec<u8>>>,
    [u8; 32],
    Mutex<Option<Vec<u8>>>,
    bool,
    /// The parts of the record of the workspaces this device holds, by name.
    Mutex<BTreeMap<String, Vec<u8>>>,
);

impl Default for MemoryPeerStore {
    fn default() -> Self {
        Self(
            Mutex::default(),
            Mutex::default(),
            rand::random(),
            Mutex::default(),
            true,
            Mutex::default(),
        )
    }
}

impl MemoryPeerStore {
    /// A store for one session, which no later session starts over: it keeps no record of the
    /// device's workspaces, which the session holds itself while it runs.
    #[must_use]
    pub fn for_session() -> Self {
        Self {
            4: false,
            ..Self::default()
        }
    }
}

impl PeerStore for MemoryPeerStore {
    fn load(&self) -> Option<PeerEntry> {
        self.0.lock_recover().clone()
    }

    fn save(&self, record: &PeerEntry) -> Result<(), String> {
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

    fn load_workspaces(&self) -> Option<Vec<u8>> {
        self.3.lock_recover().clone()
    }

    fn save_workspaces(&self, bytes: &[u8]) -> Result<(), String> {
        *self.3.lock_recover() = Some(bytes.to_vec());
        Ok(())
    }

    fn keeps_workspaces(&self) -> bool {
        self.4
    }

    fn load_workspaces_part(&self, name: &str) -> Option<Vec<u8>> {
        self.5.lock_recover().get(name).cloned()
    }

    fn save_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.5
            .lock_recover()
            .insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn append_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.5
            .lock_recover()
            .entry(name.to_owned())
            .or_default()
            .extend_from_slice(bytes);
        Ok(())
    }

    fn keeps_workspaces_parts(&self) -> bool {
        true
    }
}

/// This device's channel state and the writes held for their keys, kept as one record of its
/// [`PeerStore`] ([`PeerStore::save_channels`]). Each channel's state is sealed under the store's
/// vault before it lands here.
#[derive(Clone, Debug, Default)]
pub(crate) struct StoredKeys(Arc<Stored>);

#[derive(Debug, Default)]
struct Stored {
    saved: Mutex<KeySnapshot>,
    changed: AtomicBool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeySnapshot {
    keys: SavedKeys,
    scopes: BTreeMap<WorkspaceId, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedKeys {
    peers: BTreeMap<WorkspaceId, Vec<u8>>,
    /// Writes held until their route's keys arrive.
    held: Vec<(DocumentRoute, collab_types::DocumentMutation)>,
}

/// Tags stored channel state of this format: state of an earlier format does not open, and the
/// device asks for its channels again.
const SAVED_KEYS_TAG: &[u8] = b"tilo.browser-keys.v9";

impl StoredKeys {
    /// What `store` kept.
    pub(crate) fn load(store: &dyn PeerStore) -> Self {
        let saved = store
            .load_channels()
            .and_then(|bytes| postcard::from_bytes(bytes.strip_prefix(SAVED_KEYS_TAG)?).ok())
            .unwrap_or_default();
        Self(Arc::new(Stored {
            saved: Mutex::new(saved),
            changed: AtomicBool::new(false),
        }))
    }

    /// The writes held for their keys when the state was kept.
    pub(crate) fn held(&self) -> Vec<(DocumentRoute, collab_types::DocumentMutation)> {
        self.0.saved.lock_recover().keys.held.clone()
    }

    /// Keep `held` as the writes waiting for their keys.
    pub(crate) fn hold(&self, held: Vec<(DocumentRoute, collab_types::DocumentMutation)>) {
        self.change(|saved| saved.keys.held = held);
    }

    /// Write what changed since the last save to `store`.
    pub(crate) fn save(&self, store: &dyn PeerStore) -> Result<(), String> {
        if !self.0.changed.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let mut bytes = SAVED_KEYS_TAG.to_vec();
        postcard::to_io(&*self.0.saved.lock_recover(), &mut bytes)
            .map_err(|error| error.to_string())?;
        store.save_channels(&bytes).inspect_err(|_| {
            self.0.changed.store(true, Ordering::Release);
        })
    }

    fn change(&self, change: impl FnOnce(&mut KeySnapshot)) {
        change(&mut self.0.saved.lock_recover());
        self.0.changed.store(true, Ordering::Release);
    }
}

impl tilo_workspace_keys::KeyStorage for StoredKeys {
    fn peers(&self) -> Arc<dyn key_group::PeerStates> {
        Arc::new(self.clone())
    }

    fn held(&self) -> Vec<WorkspaceId> {
        self.0
            .saved
            .lock_recover()
            .keys
            .peers
            .keys()
            .copied()
            .collect()
    }

    fn scopes(&self) -> Vec<neighbour_wire::KeyScope> {
        self.0
            .saved
            .lock_recover()
            .scopes
            .iter()
            .filter_map(|(group, route)| {
                let scope = neighbour_wire::KeyScope::parse(route)?;
                (scope.group() == *group).then_some(scope)
            })
            .collect()
    }

    fn keep_scope(&self, scope: &neighbour_wire::KeyScope) -> Result<(), String> {
        self.change(|saved| {
            saved.scopes.insert(scope.group(), scope.to_string());
        });
        Ok(())
    }
}

impl key_group::PeerStates for StoredKeys {
    fn read(&self, workspace: WorkspaceId) -> key_group::Result<Option<Vec<u8>>> {
        Ok(self
            .0
            .saved
            .lock_recover()
            .keys
            .peers
            .get(&workspace)
            .cloned())
    }

    fn write(&self, workspace: WorkspaceId, sealed: &[u8]) -> key_group::Result<()> {
        self.change(|saved| {
            saved.keys.peers.insert(workspace, sealed.to_vec());
        });
        Ok(())
    }

    fn forget(&self, workspace: WorkspaceId) -> key_group::Result<()> {
        self.change(|saved| {
            saved.keys.peers.remove(&workspace);
            saved.scopes.remove(&workspace);
        });
        Ok(())
    }
}

/// The record of a page's or an agent's workspaces, as its [`PeerStore`] keeps it: the
/// owner's storage for the workspaces a session holds, so what it holds, and the writes it
/// made there, survive a session started again over the same store.
///
/// What the owner decides is admitted into its replica by the link every workspace is synced
/// over ([`tilo_peer_state::direct::DirectSync::owning`]), never by the session itself.
struct WorkspaceStore {
    store: Arc<dyn PeerStore>,
    healthy: AtomicBool,
    /// The record kept as an index and per-workspace parts, so a publication
    /// writes what it changed rather than the record. `None` when the store
    /// keeps the record whole ([`PeerStore::keeps_workspaces_parts`]).
    journaled: std::sync::Mutex<Option<tilo_peer_state::owner::Journaled>>,
    /// Whether the store keeps the record of the workspaces it holds at all.
    keeps: bool,
    /// Whether the store keeps the record's parts at all.
    parts: bool,
}

/// The record's parts as the page's store keeps them
/// ([`PeerStore::load_workspaces_part`] and its siblings).
struct StoreParts(Arc<dyn PeerStore>);

impl tilo_peer_state::owner::JournalParts for StoreParts {
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.load_workspaces_part(name))
    }

    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.save_workspaces_part(name, bytes)
    }

    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.append_workspaces_part(name, bytes)
    }
}

impl WorkspaceStore {
    fn new(store: Arc<dyn PeerStore>) -> Self {
        let keeps = store.keeps_workspaces();
        let parts = keeps && store.keeps_workspaces_parts();
        Self {
            store,
            healthy: AtomicBool::new(true),
            journaled: std::sync::Mutex::default(),
            keeps,
            parts,
        }
    }

    fn parts(&self) -> StoreParts {
        StoreParts(Arc::clone(&self.store))
    }

    /// The record this store kept, and the journaled form of it the next
    /// publication writes in.
    fn kept(&self) -> Option<tilo_peer_state::owner::NativeAuthorityStorage> {
        if !self.keeps {
            return None;
        }
        if self.parts {
            match tilo_peer_state::owner::Journaled::open(&self.parts()) {
                Ok(Some((journaled, record))) => {
                    *self.journaled.lock_recover() = Some(journaled);
                    return Some(record);
                }
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(%error, "the workspaces this device holds did not read");
                    return None;
                }
            }
        }
        self.store.load_workspaces().and_then(|bytes| {
            serde_json::from_slice::<tilo_peer_state::owner::NativeAuthorityStorage>(&bytes).ok()
        })
    }
}

impl tilo_peer_state::owner::Store for WorkspaceStore {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, _workspace: WorkspaceId) -> bool {
        !self.healthy()
    }

    fn shows(&self, _workspace: WorkspaceId) -> bool {
        false
    }

    fn publish(
        &self,
        next: &tilo_peer_state::owner::NativeAuthorityStorage,
        change: tilo_peer_state::owner::Change,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let written = if !self.keeps {
            Ok(())
        } else if self.parts {
            let parts = self.parts();
            let mut journaled = self.journaled.lock_recover();
            if journaled.is_none() {
                *journaled = Some(tilo_peer_state::owner::Journaled::start(next));
            }
            match journaled.as_mut() {
                Some(journaled) => journaled.publish(&parts, next, &change),
                None => Ok(()),
            }
        } else {
            serde_json::to_vec(next)
                .map_err(|error| error.to_string())
                .and_then(|bytes| self.store.save_workspaces(&bytes))
        };
        if let Err(error) = written {
            self.healthy.store(false, Ordering::Release);
            return Err(error);
        }
        install();
        Ok(())
    }
}

/// The owner holding the workspaces `device`'s session keeps in `store`, as the store last kept
/// them. A record that no longer restores is let go: the links bring its workspaces back.
pub(crate) fn open_owner(device: &DeviceCert, store: Arc<dyn PeerStore>) -> DeviceWorkspaces {
    let now = tilo_clock::wall();
    let workspaces = Arc::new(WorkspaceStore::new(store));
    let kept = workspaces.kept();
    let owner = kept
        .and_then(|kept| DeviceWorkspaces::restore(workspaces.clone(), kept, now).ok())
        .filter(|owner| {
            owner.storage().held().all(|(workspace, _)| {
                owner
                    .with_current(workspace, |view| Ok(view.device() == device))
                    .unwrap_or(false)
            })
        });
    if let Some(owner) = &owner
        && let Some(journaled) = workspaces.journaled.lock_recover().as_mut()
    {
        journaled.resume(owner);
    }
    owner.unwrap_or_else(|| DeviceWorkspaces::empty(workspaces))
}

/// The workspaces `owner` holds, as its session joined them.
pub(crate) fn held_workspaces(owner: &DeviceWorkspaces) -> std::collections::BTreeSet<WorkspaceId> {
    owner
        .storage()
        .held()
        .map(|(workspace, _)| workspace)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_state_and_held_edits_survive_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let store = MemoryPeerStore::default();
        let route = DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: "workspace".into(),
            document: "note".into(),
        };
        let workspace = WorkspaceId::new();
        let held = vec![(
            route,
            collab_types::DocumentMutation::Incremental(b"before the reload".to_vec()),
        )];
        let scope = neighbour_wire::KeyScope::Document {
            workspace,
            app: "so.tilo.notes".into(),
            document: "note".into(),
            readers: None,
        };
        let stored = StoredKeys::load(&store);
        stored.hold(held.clone());
        tilo_workspace_keys::KeyStorage::keep_scope(&stored, &scope)?;
        tilo_workspace_keys::KeyStorage::peers(&stored).write(scope.group(), b"sealed")?;
        stored.save(&store)?;

        let reopened = StoredKeys::load(&store);
        if reopened.held() != held {
            return Err("the held edit changed on reopen".into());
        }
        if tilo_workspace_keys::KeyStorage::peers(&reopened)
            .read(scope.group())?
            .as_deref()
            != Some(&b"sealed"[..])
        {
            return Err("the channel's state changed on reopen".into());
        }
        if tilo_workspace_keys::KeyStorage::held(&reopened) != vec![scope.group()] {
            return Err("the reopened state does not list its channel".into());
        }
        if tilo_workspace_keys::KeyStorage::scopes(&reopened) != vec![scope] {
            return Err("the document scope changed on reopen".into());
        }
        Ok(())
    }
}
