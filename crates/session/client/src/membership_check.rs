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
use tilo_identity::identity_store::DeviceIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_user_identity::DeviceCert;

/// The check a host makes before this device commits to a workspace it holds.
pub trait MembershipCheck: collab_types::signer::MaybeSendSync {
    /// Whether this device still owns `workspace` in `owner`.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &DeviceIdentity,
    ) -> Result<(), String>;
}

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
    /// ([`tilo_peer_state::owner::NativeWorkspaceOwner`]), if any. A store
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
    /// Keep `bytes` under `name` beside the record of the workspaces this device holds: a run of
    /// a workspace's outgoing ownership waiting behind what the record keeps itself
    /// ([`tilo_peer_state::owner::Store::put_ownership`]). `None` lets it go. A store without
    /// them holds them for the session only.
    fn save_workspaces_run(&self, _name: &str, _bytes: Option<&[u8]>) -> Result<(), String> {
        Ok(())
    }
    /// The run [`Self::save_workspaces_run`] keeps under `name`, if any.
    fn load_workspaces_run(&self, _name: &str) -> Option<Vec<u8>> {
        None
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
    Mutex<BTreeMap<String, Vec<u8>>>,
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
            5: false,
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

    fn save_workspaces_run(&self, name: &str, bytes: Option<&[u8]>) -> Result<(), String> {
        match bytes {
            Some(bytes) => self
                .4
                .lock_recover()
                .insert(name.to_owned(), bytes.to_vec()),
            None => self.4.lock_recover().remove(name),
        };
        Ok(())
    }

    fn load_workspaces_run(&self, name: &str) -> Option<Vec<u8>> {
        self.4.lock_recover().get(name).cloned()
    }

    fn keeps_workspaces(&self) -> bool {
        self.5
    }

    fn load_workspaces_part(&self, name: &str) -> Option<Vec<u8>> {
        self.6.lock_recover().get(name).cloned()
    }

    fn save_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.6
            .lock_recover()
            .insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn append_workspaces_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.6
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

/// Channel state as "tilo.browser-keys.v8" kept it, when held writes were raw document frames:
/// the peers and reader scopes are read, and each app writes again what it wrote then.
#[derive(Deserialize)]
struct KeySnapshotBeforeTypedWrites {
    keys: SavedKeysBeforeTypedWrites,
    scopes: BTreeMap<WorkspaceId, String>,
}

#[derive(Deserialize)]
struct SavedKeysBeforeTypedWrites {
    peers: BTreeMap<WorkspaceId, Vec<u8>>,
    /// Raw frames of an earlier build's held writes: what an app wrote then is written again by
    /// its app now, so they are read and let go.
    held: Vec<(DocumentRoute, Vec<Vec<u8>>)>,
}

/// Tags stored channel state of this format: state of an earlier format does not open, and the
/// device asks for its channels again.
const SAVED_KEYS_TAG: &[u8] = b"tilo.browser-keys.v9";
const SAVED_KEYS_FRAMES: &[u8] = b"tilo.browser-keys.v8";
const SAVED_KEYS_WITHOUT_SCOPES: &[u8] = b"tilo.browser-channels.v7";

impl StoredKeys {
    /// What `store` kept.
    pub(crate) fn load(store: &dyn PeerStore) -> Self {
        let saved = store
            .load_channels()
            .and_then(|bytes| {
                if let Some(current) = bytes.strip_prefix(SAVED_KEYS_TAG) {
                    return postcard::from_bytes(current).ok();
                }
                if let Some(frames) = bytes.strip_prefix(SAVED_KEYS_FRAMES) {
                    let older: KeySnapshotBeforeTypedWrites = postcard::from_bytes(frames).ok()?;
                    let _ = older.keys.held.len();
                    return Some(KeySnapshot {
                        keys: SavedKeys {
                            peers: older.keys.peers,
                            held: Vec::new(),
                        },
                        scopes: older.scopes,
                    });
                }
                let keys =
                    postcard::from_bytes(bytes.strip_prefix(SAVED_KEYS_WITHOUT_SCOPES)?).ok()?;
                Some(KeySnapshot {
                    keys,
                    scopes: BTreeMap::new(),
                })
            })
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

/// A page's or agent's proof that it still owns a workspace: its host session
/// authenticated this device's certificate, and its host refuses a commit under any other. The
/// replica commits only while the workspace is held under that certificate.
pub(crate) struct SessionMembership;

impl MembershipCheck for SessionMembership {
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &DeviceIdentity,
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

/// One write in a device's ownership of a workspace, as it leaves for the workspace's host.
pub struct Outgoing<'a> {
    /// The ownership entry it is.
    pub id: [u8; 32],
    /// The signed frame, exactly as committed.
    pub bytes: &'a [u8],
    /// The record channel it writes.
    pub channel: [u8; 32],
    /// Whether it appends to the record or stands as its whole.
    pub retention: neighbour_wire::Retention,
}

/// What a connection sent of one of this device's writes, kept until its host answers it: the
/// route it was written to and the changes it carries, which its host's refusal names wherever
/// ownership has moved the write since.
#[derive(Clone, Debug)]
pub struct SentWrite {
    pub route: DocumentRoute,
    pub changes: Vec<[u8; 32]>,
}

impl SentWrite {
    /// What `bytes`, one write of this device's ownership, was written to, and the changes it
    /// carries; `None` for bytes that name no document.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Option<Self> {
        let message = neighbour_wire::NeighbourMsg::decode(bytes).ok()?;
        Some(Self {
            route: DocumentRoute::of_message(&message)?,
            changes: tilo_peer_state::document_frame::claimed_changes(bytes),
        })
    }
}

/// What one connection to a workspace's host has sent of this device's ownership of it: each
/// write leaves once per connection, oldest first, and stays held until the host's
/// answer retires it. A new connection starts with a new one, and sends everything again.
#[derive(Default)]
pub struct Sending {
    sent: std::collections::BTreeSet<[u8; 32]>,
    /// What each write sent was written to, for its host's answer to name.
    sent_writes: BTreeMap<[u8; 32], SentWrite>,
    /// How many writes held the last pass held back: they may not leave now.
    held: usize,
    /// Writes the host was not ready for: each goes again once the host says the workspace is
    /// ready ([`Self::ready`]).
    awaiting_ready: std::collections::BTreeSet<[u8; 32]>,
}

impl Sending {
    /// Send each frame of `workspace`'s this device wrote and this connection has not sent,
    /// through `send`, dropping it from the record once the transport took it by `sent`.
    ///
    /// Returns whether the transport took everything: `false` once `send` took no more.
    ///
    /// # Errors
    /// The record cannot be written after a frame went.
    pub fn drive(
        &mut self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        mut sent: impl FnMut(&[u8]) -> Result<(), String>,
        mut send: impl FnMut(Outgoing<'_>) -> bool,
    ) -> Result<bool, String> {
        let frames = owner.frames(workspace);
        if frames.is_empty() {
            self.held = 0;
            self.sent.clear();
            self.sent_writes.clear();
            self.awaiting_ready.clear();
            return Ok(true);
        }
        let mut entries: Vec<([u8; 32], [u8; 32], neighbour_wire::Retention)> = Vec::new();
        for bytes in &frames {
            let Ok(message) = neighbour_wire::NeighbourMsg::decode(bytes) else {
                continue;
            };
            let Some((channel, retention)) = neighbour_wire::retention(&message, false) else {
                continue;
            };
            entries.push((*blake3::hash(bytes).as_bytes(), channel, retention));
        }
        self.held = 0;
        let queued: Vec<[u8; 32]> = entries.iter().map(|(id, ..)| *id).collect();
        let mut written = None;
        let took = self.pass(
            queued.into_iter(),
            entries
                .iter()
                .zip(&frames)
                .map(|((id, channel, retention), bytes)| Outgoing {
                    id: *id,
                    bytes,
                    channel: *channel,
                    retention: *retention,
                }),
            |write| {
                if !send(write) {
                    return false;
                }
                if let Err(error) = sent(write.bytes) {
                    written = Some(error);
                }
                true
            },
        );
        match written {
            Some(error) => Err(error),
            None => Ok(took),
        }
    }

    /// How many writes held the last [`Self::drive`] held back: a sealed write past its
    /// epoch waits for its renewal, and a write behind its record's compaction for the host's answer.
    #[must_use]
    pub fn held(&self) -> usize {
        self.held
    }

    /// Send each of `writes` this connection has not sent; `queued` is every write still in
    /// ownership, sendable now or not.
    fn pass<'a>(
        &mut self,
        queued: impl Iterator<Item = [u8; 32]>,
        writes: impl Iterator<Item = Outgoing<'a>>,
        mut send: impl FnMut(Outgoing<'a>) -> bool,
    ) -> bool {
        let queued: std::collections::BTreeSet<_> = queued.collect();
        self.sent.retain(|id| queued.contains(id));
        self.sent_writes.retain(|id, _| queued.contains(id));
        self.awaiting_ready.retain(|id| queued.contains(id));
        for write in writes {
            let id = write.id;
            if self.sent.contains(&id) || self.awaiting_ready.contains(&id) {
                continue;
            }
            let named = SentWrite::of(write.bytes);
            if !send(write) {
                return false;
            }
            self.sent.insert(id);
            if let Some(named) = named {
                self.sent_writes.insert(id, named);
            }
        }
        true
    }

    /// Send write `id` again once the host says the workspace is ready, not before: the host
    /// was not ready for it.
    pub fn send_once_ready(&mut self, id: [u8; 32]) {
        self.sent.remove(&id);
        self.awaiting_ready.insert(id);
    }

    /// The host says the workspace is ready on this connection: every write it was not ready
    /// for goes again on the next pass. Whether any was waiting.
    pub fn ready(&mut self) -> bool {
        let waited = !self.awaiting_ready.is_empty();
        self.awaiting_ready.clear();
        waited
    }

    /// The write this connection sent whose id begins with `prefix`, taken out of what it sent,
    /// with what it was written to: the host answered it.
    pub fn answered(&mut self, prefix: &[u8]) -> Option<([u8; 32], Option<SentWrite>)> {
        let id = *self.sent.iter().find(|id| id.starts_with(prefix))?;
        self.sent.remove(&id);
        Some((id, self.sent_writes.remove(&id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(id: u8) -> Outgoing<'static> {
        Outgoing {
            id: [id; 32],
            bytes: b"exact signed bytes",
            channel: [2; 32],
            retention: neighbour_wire::Retention::Delta,
        }
    }

    tilo_test_env::scenario! {
        fn a_connection_sends_what_is_in_ownership_once;

        phase a_connection_sends_each_write_once_and_a_new_one_sends_it_again;
        {
            let mut sending = Sending::default();
            let queued = || std::iter::once([1; 32]);
            assert!(!sending.pass(queued(), std::iter::once(write(1)), |_| false));
            let mut sent = 0;
            for _ in 0..2 {
                assert!(sending.pass(queued(), std::iter::once(write(1)), |taken| {
                    assert_eq!(taken.bytes, b"exact signed bytes");
                    sent += 1;
                    true
                }));
            }
            assert_eq!(
                sent, 1,
                "a write the transport took leaves once per connection"
            );
            assert!(
                Sending::default().pass(queued(), std::iter::once(write(1)), |_| {
                    sent += 1;
                    true
                })
            );
            assert_eq!(sent, 2, "the next connection sends it again");
            sending.pass(std::iter::empty(), std::iter::empty(), |_| {
                panic!("retired")
            });
            assert!(sending.sent.is_empty(), "what left ownership is forgotten");
        }

        phase a_write_held_back_while_in_ownership_is_not_sent_twice;
        {
            let mut sending = Sending::default();
            let mut sent = 0;
            let mut count = |_: Outgoing<'_>| {
                sent += 1;
                true
            };
            sending.pass(
                std::iter::once([3; 32]),
                std::iter::once(write(3)),
                &mut count,
            );
            sending.pass(std::iter::once([3; 32]), std::iter::empty(), &mut count);
            sending.pass(
                std::iter::once([3; 32]),
                std::iter::once(write(3)),
                &mut count,
            );
            assert_eq!(sent, 1);
            assert_eq!(
                sending.answered(&[3; 16]).map(|(id, _)| id),
                Some([3; 32])
            );
            assert!(sending.answered(&[3; 16]).is_none(), "an answer is taken once");
        }
    }

    /// A device that entered a workspace and wrote nothing to it owes it nothing: each pass sends
    /// nothing and asks nothing of its standing. A device whose standing does not hold there (a
    /// key let in that is not on the roster yet) was judged absent again on every pass.
    #[test]
    fn a_device_that_owes_a_workspace_nothing_is_not_judged_on_each_pass() {
        use tilo_peer_state::owner::testing;
        let (intent, bootstrap, _admission, _governor, now) = testing::raw_fixture();
        let directory = tempfile::tempdir().unwrap();
        let owner =
            tilo_peer_state::owner::open_file(&directory.path().join("joined-workspaces.json"))
                .unwrap();
        let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        owner
            .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
            .unwrap();
        let workspace = WorkspaceId::parse(testing::WORKSPACE).unwrap();
        let mut sending = Sending::default();
        for _ in 0..2 {
            assert_eq!(
                sending.drive(
                    &owner,
                    workspace,
                    |_| Err("its standing does not hold".into()),
                    |_| panic!("nothing is owed"),
                ),
                Ok(true),
                "nothing owed asks nothing of the device's standing"
            );
        }
        assert_eq!(sending.held(), 0);
    }

    #[test]
    fn a_write_the_host_was_not_ready_for_goes_again_once_it_is() {
        let mut sending = Sending::default();
        let sent = std::cell::Cell::new(0_u32);
        let count = |_: Outgoing<'_>| {
            sent.set(sent.get().saturating_add(1));
            true
        };
        let queued = || std::iter::once([4; 32]);
        sending.pass(queued(), std::iter::once(write(4)), count);
        assert_eq!(sending.answered(&[4; 16]).map(|(id, _)| id), Some([4; 32]));
        // Not ready: it waits, still held, and is not sent meanwhile.
        sending.send_once_ready([4; 32]);
        sending.pass(queued(), std::iter::once(write(4)), count);
        assert_eq!(
            sent.get(),
            1,
            "a write waiting for the host does not leave before it is ready"
        );
        assert!(sending.ready());
        sending.pass(queued(), std::iter::once(write(4)), count);
        assert_eq!(sent.get(), 2, "once the host is ready, it leaves again");
        assert!(!sending.ready(), "nothing waits any more");
    }

    #[test]
    fn saved_keys_without_document_routes_still_open() -> Result<(), Box<dyn std::error::Error>> {
        let store = MemoryPeerStore::default();
        let workspace = WorkspaceId::new();
        let mut encoded = SAVED_KEYS_WITHOUT_SCOPES.to_vec();
        postcard::to_io(
            &SavedKeys {
                peers: BTreeMap::from([(workspace, b"old sealed keys".to_vec())]),
                held: Vec::new(),
            },
            &mut encoded,
        )?;
        store.save_channels(&encoded)?;
        let reopened = StoredKeys::load(&store);
        if tilo_workspace_keys::KeyStorage::peers(&reopened)
            .read(workspace)?
            .as_deref()
            != Some(b"old sealed keys")
        {
            return Err("the earlier keys changed on reopen".into());
        }
        if !tilo_workspace_keys::KeyStorage::scopes(&reopened).is_empty() {
            return Err("the earlier keys unexpectedly have document scopes".into());
        }
        Ok(())
    }

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
