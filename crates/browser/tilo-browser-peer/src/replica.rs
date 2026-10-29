//! Each joined room as its governance admits it: the page holds its rooms
//! in the same joined-room owner native devices hold theirs in
//! ([`tilo_joined_room::NativeWorkspaceOwner`]), kept in the page's
//! storage through its [`PeerStore`].
//!
//! The host sends a subscriber the room's bootstrap ahead of its replay
//! ([`tilo_relay_protocol::Envelope::RoomBootstrap`]): the workspace-join
//! contract's membership answer, the room's governing authority, roster and
//! revocations. The owner holds the room on that answer, vouched for by the
//! host the page authenticated to
//! ([`tilo_joined_room::NativeWorkspaceOwner::install_vouched`]), and admits
//! every governance message the host relays through it: members'
//! certificates and the room document. What the room's governance says is
//! read off that replica: whether it shares its members' layout, who its
//! members are, who takes part in its channels, and whether a member's
//! write reaches the page's apps. The replica decides each document as
//! native devices do (its author's certificate and role, its signature and
//! route, its claims against the app surface it names) and keeps none: the
//! page's apps hold their records.
//!
//! A room's replica is the only source of what its governance says: a
//! workspace with no governance yet (no bootstrap received) admits no
//! member's write and names no channel participants. A page places its
//! person's own workspaces on their host before joining them
//! ([`crate::PeerHandle::join_placed`]), so those have governance too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collab_types::Ed25519PublicKey;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::RoomId;
use tilo_desktop_model::projection::{RoomLayout, room_layout, room_members};
use tilo_joined_room::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, Store,
};
use tilo_peer_state::TransportOrigin;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use crate::channels::Channels;
use crate::custody::PeerStore;
use tilo_peer_state::document_frame::DocumentRoute;

/// The owner's record in the page's [`PeerStore`]. A record the store
/// refused leaves the owner fenced for the rest of the session, as a
/// native owner is after an uncertain write: the next session starts from
/// what the store holds.
struct PageRooms {
    store: Arc<dyn PeerStore>,
    healthy: AtomicBool,
}

impl Store for PageRooms {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, _room: RoomId) -> bool {
        !self.healthy()
    }

    fn slot_room(&self, _slot: tilo_desktop_model::WorkspaceId) -> Option<RoomId> {
        None
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        _change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(next).map_err(|error| error.to_string())?;
        if let Err(error) = self.store.save_rooms(&bytes) {
            self.healthy.store(false, Ordering::Release);
            return Err(error);
        }
        install();
        Ok(())
    }
}

/// The rooms this page joined, by workspace.
pub(crate) struct Replicas {
    /// This device.
    device: DeviceCert,
    owner: NativeWorkspaceOwner,
    rooms: BTreeMap<u64, RoomId>,
}

impl Replicas {
    /// The rooms of `device`, as `store` last kept them. A record that no
    /// longer restores is let go: the host's bootstraps and replays bring
    /// its rooms back.
    pub(crate) fn new(device: DeviceCert, store: Arc<dyn PeerStore>) -> Self {
        let now = SystemTime::now();
        let kept = store
            .load_rooms()
            .and_then(|bytes| serde_json::from_slice::<NativeAuthorityStorage>(&bytes).ok());
        let rooms = Arc::new(PageRooms {
            store,
            healthy: AtomicBool::new(true),
        });
        let owner = kept
            .and_then(|kept| NativeWorkspaceOwner::restore(rooms.clone(), kept, now).ok())
            .filter(|owner| {
                owner.storage().held().all(|(room, _)| {
                    owner
                        .with_current(room, |view| Ok(view.device() == &device))
                        .unwrap_or(false)
                })
            });
        let owner = owner.unwrap_or_else(|| NativeWorkspaceOwner::empty(rooms));
        let rooms = owner
            .storage()
            .held()
            .map(|(room, workspace)| (workspace.0, room))
            .collect();
        Self {
            device,
            owner,
            rooms,
        }
    }

    fn with_room<T>(
        &self,
        workspace_id: u64,
        read: impl FnOnce(&NativeCommittedView) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        let room = self.rooms.get(&workspace_id)?;
        Some(self.owner.with_current(*room, read))
    }

    /// Hold `workspace_id`'s room on the bootstrap the host sent (`bytes`,
    /// a postcard membership answer); returns whether the replica is new.
    /// A replica of the same room and authority stays: a resumed
    /// subscription replays only what it has not received.
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
        let installed = self
            .owner
            .install_vouched(workspace_id, bytes, &self.device, now)?;
        if let Some((room, _)) = self
            .owner
            .storage()
            .held()
            .find(|(_, workspace)| workspace.0 == workspace_id)
        {
            self.rooms.insert(workspace_id, room);
        }
        Ok(installed)
    }

    /// Admit `message`, governance `author` sent to `workspace_id`;
    /// `retained` while the host replays the room. Returns whether it
    /// changed what the room's governance says.
    ///
    /// # Errors
    /// No replica of the room is held, or its governance refuses the
    /// message.
    pub(crate) fn admit(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(room) = self.rooms.get(&workspace_id) else {
            return Err("the workspace has no governance".into());
        };
        let report = self
            .owner
            .receive(*room, origin(author, retained), message, now)?;
        Ok(report.changed || report.metadata_changed)
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
        self.with_room(workspace_id, |view| {
            view.peer()
                .decide_relayed_document(origin(author, retained), message, now)
        })
        .unwrap_or(Ok(false))
    }

    /// `workspace_id`'s room, whether its governance keeps each member's
    /// layout their own, and the members it lists by person id.
    pub(crate) fn layout(
        &self,
        workspace_id: u64,
    ) -> Option<(String, RoomLayout, Option<BTreeSet<String>>)> {
        self.with_room(workspace_id, |view| {
            let peer = view.peer();
            let doc = peer.admitted_room();
            Ok((
                peer.room().to_owned(),
                doc.map(room_layout).unwrap_or_default(),
                doc.and_then(room_members),
            ))
        })?
        .ok()
    }

    /// The devices that may sign and be added to `route`'s channel: those
    /// the room's governance lets read its record, the room's governor and
    /// this device. `None` while the room's governance does not answer for
    /// the route.
    pub(crate) fn participants(&self, route: &DocumentRoute) -> Option<BTreeSet<Ed25519PublicKey>> {
        let workspace_id = route.workspace_id().ok()?;
        let channel = Channels::channel(route);
        let record = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        self.with_room(workspace_id, |view| {
            let peer = view.peer();
            let readers = peer
                .governance()
                .channel_readers(SystemTime::now(), channel, Some(&record))
                .ok()
                .flatten()
                .ok_or("the room does not answer for the route")?;
            let mut participants: BTreeSet<Ed25519PublicKey> = readers
                .into_iter()
                .map(Ed25519PublicKey::from_bytes)
                .collect();
            participants.insert(Ed25519PublicKey::from_bytes(
                peer.authority().device_public_key,
            ));
            participants.insert(Ed25519PublicKey::from_bytes(self.device.device_public_key));
            Ok(participants)
        })?
        .ok()
    }

    /// One line per room for diagnostics: who governs it.
    pub(crate) fn describe(&self) -> String {
        self.rooms
            .keys()
            .filter_map(|workspace_id| {
                self.with_room(*workspace_id, |view| {
                    Ok(format!(
                        "room {}: governed by {}",
                        view.peer().room(),
                        hex::encode(&view.governor().device_public_key[..4])
                    ))
                })?
                .ok()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn origin(author: [u8; 32], retained: bool) -> TransportOrigin {
    if retained {
        TransportOrigin::RetainedRelayDelta(author)
    } else {
        TransportOrigin::RelayDelta(author)
    }
}

/// Decide every room's members' writes against the app surfaces of the
/// apps this page runs: set once, before the page restores its rooms.
#[cfg(feature = "toolkit")]
pub(crate) fn decide_with_page_surfaces() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tilo_peer_state::decide_with_surfaces(page_surfaces(), ui::app_surface::hash_of);
    });
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
