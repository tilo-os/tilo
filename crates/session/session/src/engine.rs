//! This device's rooms: the engine that holds the state of every workspace
//! the person joined, and the enrollment that brings a joined workspace's
//! state here.
//!
//! A workspace is its room, and the room's state — its people, what its
//! apps recorded, its history — is held by the peer-state engine beside the
//! desk ([`NativeWorkspaceOwner`]), in a file of its own under this device's
//! state ([`FILE`]). Joining a workspace redeems the invitation its door
//! answered with ([`tilo_peer_state::owner::enroll::prepare_and_complete`]),
//! which is how this device becomes a member of the room and takes in what
//! the room held before it arrived: a late joiner reads the whole history,
//! not only what is recorded from now on.
//!
//! The engine keeps the room each workspace on the desk is
//! ([`Slots`]), because an enrollment installs into the workspace made for
//! it and nowhere else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_identity_host::custody::HostIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::owner::{
    Change, DocumentRead, FileHost, NativeWorkspaceOwner, open_file_with, owner_file_of,
};

use crate::sync;

/// The engine's record, under this device's state directory.
const FILE: &str = "rooms.json";

/// The largest document the session reads out of a room.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// The room each workspace on the desk is, as the engine's record names its
/// workspaces and the desk names its own.
#[derive(Default)]
struct Slots(std::sync::Mutex<BTreeMap<WorkspaceId, RoomId>>);

impl std::fmt::Debug for Slots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.0.lock_recover().iter()).finish()
    }
}

impl Slots {
    fn of(workspaces: impl IntoIterator<Item = (WorkspaceId, RoomId)>) -> Self {
        Self(std::sync::Mutex::new(workspaces.into_iter().collect()))
    }
}

impl FileHost for Slots {
    fn slot_room(&self, slot: WorkspaceId) -> Option<RoomId> {
        self.0.lock_recover().get(&slot).copied()
    }

    /// Bind the workspace an enrollment installs into to its room before the
    /// record reaches the disk: a publication refused here installs nothing.
    fn publish(
        &self,
        _next: &tilo_peer_state::owner::NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let bound = match change {
            Change::Room {
                room,
                into: Some(target),
                ..
            } => Some((target.workspace, room)),
            _ => None,
        };
        if let Some((workspace, room)) = bound {
            self.0.lock_recover().insert(workspace, room);
        }
        let written = write();
        if written.is_err()
            && let Some((workspace, _)) = bound
        {
            self.0.lock_recover().remove(&workspace);
        }
        if let Change::Released { room } = change {
            self.0.lock_recover().retain(|_, held| *held != room);
        }
        written
    }
}

/// This device's rooms, and the identity they are held with.
pub struct Engine {
    owner: NativeWorkspaceOwner,
    slots: Arc<Slots>,
    identity: HostIdentity,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("rooms", &self.rooms())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Open this device's rooms under `state`, with the workspaces the desk
    /// already holds: an enrollment only ever installs into the workspace
    /// made for it, so the engine knows which one that is before it is asked.
    ///
    /// A session that keeps no state (a screenshot, a test without a state
    /// directory) holds no room.
    ///
    /// # Errors
    /// The record holds no valid state, or the file cannot be written.
    pub fn open(
        identity: HostIdentity,
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceId, RoomId)>,
    ) -> Result<Self, String> {
        let state = state.ok_or("this session keeps no state, so it holds no room")?;
        let path = record_path(state);
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let slots = Arc::new(Slots::of(workspaces));
        let owner = open_file_with(
            &owner_file_of(&path),
            Arc::clone(&slots) as Arc<dyn FileHost>,
        )?;
        Ok(Self {
            owner,
            slots,
            identity,
        })
    }

    /// This device's engine, when this computer has an identity and a state
    /// directory to keep its rooms in.
    pub(crate) fn for_this_device(
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceId, RoomId)>,
    ) -> Result<Self, String> {
        let identity = sync::device_identity()
            .ok_or("this computer has no identity yet, so it holds no room")?;
        Self::open(identity, state, workspaces)
    }

    /// The rooms this device holds.
    pub fn rooms(&self) -> Vec<RoomId> {
        self.owner.rooms()
    }

    /// Whether this device holds the room of a workspace.
    #[must_use]
    pub fn holds(&self, room: RoomId) -> bool {
        self.owner.contains_room(room)
    }

    /// What the room of `room` holds of the app `app`'s `record`: the
    /// document as the room last admitted it, which is what the workspace
    /// shows of it — also what it held before this device joined.
    ///
    /// # Errors
    /// The room is not held here, this device may not read the record, or
    /// the document cannot be read now.
    pub fn record(&self, room: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        let room = RoomId::parse(room).ok_or("that is not a room")?;
        let reader = self.identity.device_cert.device_public_key;
        self.owner.with_current(room, |view| {
            match view.read_document(
                reader,
                app,
                record,
                None,
                MAX_DOCUMENT_BYTES,
                SystemTime::now(),
            )? {
                DocumentRead::Captured(document) => Ok(Some(document.bytes)),
                DocumentRead::Unchanged(_) | DocumentRead::CheckpointPending => Ok(None),
            }
        })
    }

    /// The engine's owner, for what holds the room's session.
    pub(crate) fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }

    /// This device's identity, as its rooms were enrolled with.
    pub(crate) fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    /// The room the workspace `workspace` is, as an enrollment installed it.
    pub(crate) fn room_of(&self, workspace: WorkspaceId) -> Option<RoomId> {
        self.slots.slot_room(workspace)
    }

    /// Name the workspace a join is installing into as the room's: an
    /// enrollment only installs into the workspace made for its room, and
    /// says so before the host is asked.
    pub(crate) fn bind(&self, workspace: WorkspaceId, room: RoomId) {
        self.slots.0.lock_recover().insert(workspace, room);
    }

    /// Let go of a workspace a join did not install into, so this device
    /// keeps no room it does not hold.
    pub(crate) fn unbind(&self, workspace: WorkspaceId) {
        self.slots.0.lock_recover().remove(&workspace);
    }
}

/// Where this device's rooms are kept under `state`.
fn record_path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}
