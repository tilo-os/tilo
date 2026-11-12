//! Whether this device still holds custody of a workspace, as its host checks it.
//!
//! The owner of a workspace commits only what this device still has custody of: a device the
//! workspace removed, or whose certificate ran out, finds out before it commits. Each host keeps
//! its own proof of that — this desktop's signed custody over the collaboration plane, a page's
//! over its own transport.
//!
//! A session's [`PeerStore`] is what a peer keeps between sessions. The device key itself never passes through here: it lives behind a
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
use tilo_identity_host::custody::HostIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_user_identity::DeviceCert;

/// The check a host makes before this device commits to a workspace it holds.
pub trait Custody: collab_types::signer::MaybeSendSync {
    /// Whether this device still holds custody of `workspace` in `owner`.
    ///
    /// # Errors
    /// Why this device may not commit to `workspace` now.
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &HostIdentity,
    ) -> Result<(), String>;
}

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

impl Custody for SessionCustody {
    fn check(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        identity: &HostIdentity,
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

/// One write in a device's custody of a workspace, as it leaves for the workspace's host.
pub struct Outgoing<'a> {
    /// The custody entry it is.
    pub id: [u8; 32],
    /// The signed frame, exactly as committed.
    pub bytes: &'a [u8],
    /// The record channel it writes.
    pub channel: [u8; 32],
    /// Whether it appends to the record or stands as its whole.
    pub retention: neighbour_wire::Retention,
}

/// What one connection to a workspace's host has sent of this device's custody of it: each
/// write leaves once per connection, oldest first, and stays in custody until the host's
/// answer retires it. A new connection starts with a new one, and sends everything again.
#[derive(Default)]
pub struct Sending {
    sent: std::collections::BTreeSet<[u8; 32]>,
}

impl Sending {
    /// Send each write of `workspace`'s custody in `owner` that may leave now and this
    /// connection has not sent, through `send`, once `authorize` accepted the custody as it
    /// stands. A write still in custody that may not leave now stays sent.
    ///
    /// Returns whether the transport took everything: `false` once `send` took no more.
    ///
    /// # Errors
    /// The custody cannot be read, or `authorize` refused it.
    pub fn drive(
        &mut self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        authorize: impl FnOnce(&tilo_peer_state::owner::NativeCommittedView) -> Result<(), String>,
        send: impl FnMut(Outgoing<'_>) -> bool,
    ) -> Result<bool, String> {
        owner.with_outgoing(workspace, tilo_clock::wall(), |view, outgoing| {
            authorize(view)?;
            Ok(self.pass(
                outgoing.queued(),
                outgoing.entries().map(|entry| Outgoing {
                    id: entry.id(),
                    bytes: entry.bytes(),
                    channel: entry.channel(),
                    retention: entry.retention(),
                }),
                send,
            ))
        })
    }

    /// Send each of `writes` this connection has not sent; `queued` is every write still in
    /// custody, sendable now or not.
    fn pass<'a>(
        &mut self,
        queued: impl Iterator<Item = [u8; 32]>,
        writes: impl Iterator<Item = Outgoing<'a>>,
        mut send: impl FnMut(Outgoing<'a>) -> bool,
    ) -> bool {
        let queued: std::collections::BTreeSet<_> = queued.collect();
        self.sent.retain(|id| queued.contains(id));
        for write in writes {
            let id = write.id;
            if self.sent.contains(&id) {
                continue;
            }
            if !send(write) {
                return false;
            }
            self.sent.insert(id);
        }
        true
    }

    /// The write this connection sent whose id begins with `prefix`, taken out of what it sent:
    /// the host answered it.
    pub fn answered(&mut self, prefix: &[u8]) -> Option<[u8; 32]> {
        let id = *self.sent.iter().find(|id| id.starts_with(prefix))?;
        self.sent.remove(&id);
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_channels::ChannelState as _;

    fn write(id: u8) -> Outgoing<'static> {
        Outgoing {
            id: [id; 32],
            bytes: b"exact signed bytes",
            channel: [2; 32],
            retention: neighbour_wire::Retention::Delta,
        }
    }

    #[test]
    fn a_connection_sends_each_write_once_and_a_new_one_sends_it_again() {
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
        assert!(sending.sent.is_empty(), "what left custody is forgotten");
    }

    #[test]
    fn a_write_held_back_while_in_custody_is_not_sent_twice() {
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
        assert_eq!(sending.answered(&[3; 16]), Some([3; 32]));
        assert_eq!(sending.answered(&[3; 16]), None, "an answer is taken once");
    }

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
