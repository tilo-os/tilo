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

use tilo_action_broker::channel_encryption::{ChannelAccess, ChannelKeyProvider};
use tilo_desktop_model::{RoomId, WorkspaceId};
use tilo_identity_host::custody::HostIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::owner::{
    Change, DocumentRead, FileHost, NativeWorkspaceOwner, open_file_with, owner_file_of,
};
use tilo_user_identity::RevocationList;

use crate::channels;
use crate::sync;

/// The engine's record, under this device's state directory.
const FILE: &str = "rooms.json";

/// The largest document the session reads out of a room.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Someone a workspace has in it: the person, as their user key in hex, the
/// name the workspace knows them by, and the part they take in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The person (user root, hex).
    pub person: String,
    /// The name the workspace knows them by, if it knows one.
    pub name: String,
    /// The part they take in the workspace.
    pub role: MemberRole,
    /// Whether the workspace removed them.
    pub removed: bool,
}

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
    /// Where the rooms' channels find this device's keys, once the channel
    /// service is running: the session opens the rooms before it opens the
    /// service, and every reader of a sealed record goes through this.
    keys: ChannelKeyProvider,
    /// The room transport of this session's collaboration plane, once it
    /// runs: a record the room's hosts send only once asked is asked for
    /// here, and the plane is gone while this is `None`.
    link: std::sync::Mutex<Option<Arc<channels::ChannelLink>>>,
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
            keys: ChannelKeyProvider::default(),
            link: std::sync::Mutex::new(None),
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
    /// A record the room keeps sealed opens with this device's channel keys,
    /// so a room's history reads whole here: the keys a member holds reach
    /// back past the membership it joined in. Reading one is taking part in
    /// its channel ([`Self::join_channel`]): a device that holds none of its
    /// keys yet asks the room for them, and reads the record once they are
    /// here.
    ///
    /// A record the room's hosts send a device only once it asks (an
    /// on-demand record, or a part of a chain behind its head) is asked for
    /// on its first read, and reads as nothing yet until its history arrived
    /// ([`Self::asks_for_record`]).
    ///
    /// # Errors
    /// The room is not held here, this device may not read the record, the
    /// record is sealed and this device holds no keys of it, or the document
    /// cannot be read now.
    pub fn record(&self, room: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        let room = RoomId::parse(room).ok_or("that is not a room")?;
        if self.asks_for_record(room, app, record)? {
            return Ok(None);
        }
        let sealed = self.owner.with_current(room, |view| {
            Ok(view.peer().publication().is_sealed_record(app, record))
        })?;
        if sealed && !self.join_channel(room, app, record)? {
            return Ok(None);
        }
        let reader = self.identity.device_cert.device_public_key;
        let keys = &self.keys;
        self.owner.with_current(room, |view| {
            match view.read_document_with_history(
                reader,
                (app, record),
                None,
                MAX_DOCUMENT_BYTES,
                SystemTime::now(),
                &mut |write| {
                    keys.get()
                        .ok_or("this session's channel service is not running")?
                        .open_history_write(room, write)
                },
            )? {
                DocumentRead::Captured(document) => Ok(Some(document.bytes)),
                DocumentRead::Unchanged(_) | DocumentRead::CheckpointPending => Ok(None),
            }
        })
    }

    /// Take part in the channel of `record` of `room`, whose keys this device
    /// reads it with: the channel service asks the room's devices for access
    /// the first time, and the room's governance answers with the group. Says
    /// whether the keys are here; while they are not, the record reads as
    /// nothing, and the answer arrives as the room's other messages do.
    fn join_channel(&self, room: RoomId, app: &str, record: &str) -> Result<bool, String> {
        let Some(keys) = self.keys.get() else {
            // Without a channel service there is nothing to take part with,
            // and the read says so itself.
            return Ok(true);
        };
        let route = collab_types::ChannelRoute::new(app, record);
        match keys.access_record(room, &route).0 {
            ChannelAccess::Ready(_) => Ok(true),
            ChannelAccess::Waiting | ChannelAccess::Removed => Ok(false),
            ChannelAccess::NoSecretStore => {
                Err("this device has no secret store, so it holds no channel keys".to_owned())
            }
            ChannelAccess::Unavailable => {
                Err("this device takes part in no channel of the record".to_owned())
            }
        }
    }

    /// Ask `room`'s hosts for the record on `channel` the first time it is
    /// read, and say whether it is one they send only once asked (an
    /// on-demand record, or a part of a chain behind its head): its history
    /// then arrives as the room's other documents do, and later reads ask
    /// nobody.
    fn asks_for_record(&self, room: RoomId, app: &str, record: &str) -> Result<bool, String> {
        if neighbour_wire::record_sync(record) == neighbour_wire::RecordSync::Eager {
            return Ok(false);
        }
        let held = self
            .owner
            .with_current(room, |view| Ok(view.peer().document(app, record).is_some()))?;
        // With no plane there is no one to ask: the read answers for itself.
        let Some(link) = self.link.lock_recover().clone().filter(|_| !held) else {
            return Ok(false);
        };
        link.ask_record(room, neighbour_wire::app_document_channel(app, record));
        Ok(true)
    }

    /// The devices the room `room` admits, by the publisher each of their
    /// sessions is known by: the roster its window streams carry between
    /// ([`channels::roster_of`]).
    ///
    /// # Errors
    /// The room is not held here, or its governance cannot be read.
    pub fn roster(&self, room: &str) -> Result<Vec<String>, String> {
        let room = RoomId::parse(room).ok_or("that is not a room")?;
        channels::roster_of(self, room).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| device.to_string())
                .collect()
        })
    }

    /// Who the room `room` has in it, as its governance tells it: every
    /// person the workspace was shared with, by the name it knows them by and
    /// the part they take in it. A workspace joined brings its people with
    /// it, so this is how the desk knows them before any of them is here.
    pub fn people(&self, room: &str) -> Result<Vec<Person>, String> {
        let room = RoomId::parse(room).ok_or("that is not a room")?;
        self.owner.with_current(room, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(SystemTime::now())?;
            Ok(members
                .into_iter()
                .map(|member| Person {
                    person: hex::encode(member.user_public_key),
                    name: member.display_name.unwrap_or_default(),
                    role: member.role,
                    removed: member.removed,
                })
                .collect())
        })
    }

    /// The revocations the room `room`'s governance records, as the room
    /// last admitted them: the devices of any of its people the room refuses
    /// to hear from. `None` when this device holds no room of that name, so
    /// nothing of it is known.
    ///
    /// What is checked against this: whether a shortcut's saver may still
    /// start it here, and whether an app of the room may still be believed
    /// about who saved one.
    #[must_use]
    pub fn revocations(&self, room: &str) -> Option<RevocationList> {
        let room = RoomId::parse(room)?;
        self.owner
            .with_current(room, |view| {
                let mut list = RevocationList::new();
                for entry in view.peer().revocations() {
                    // The room verified every entry when it admitted it: an
                    // entry that does not verify now leaves the room's
                    // refusals unknown rather than trusting the rest.
                    list.insert(entry).map_err(|error| error.to_string())?;
                }
                Ok(list)
            })
            .ok()
    }

    /// Who governs the room `room`, as the id its governance names: the
    /// owner the room was made under, whose devices sign for it. A workspace
    /// joined keeps the owner its host's governance has, never the person who
    /// joined; one made here has this person's.
    ///
    /// # Errors
    /// The room is not held here yet, or its governance cannot be read now.
    pub fn owner_of(&self, room: &str) -> Result<String, String> {
        let room = RoomId::parse(room).ok_or("that is not a room")?;
        self.owner.with_current(room, |view| {
            let owner = view.peer().governance().owner_user_public_key();
            if owner == [0; 32] {
                // A room whose governance names nobody has no owner to carry:
                // a claim made here would be this device's, not the room's.
                return Err("the room's governance names no owner".to_owned());
            }
            Ok(hex::encode(owner))
        })
    }

    /// The engine's owner, for what holds the room's session.
    pub(crate) fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }

    /// Where a read asks the rooms' hosts for a record they send only once
    /// asked: the plane this session's channels travel on.
    pub(crate) fn attach_link(&self, link: Arc<channels::ChannelLink>) {
        *self.link.lock_recover() = Some(link);
    }

    /// Where this device's channels hear its rooms' governance and tell which
    /// devices each admits: the plane's side of them, `None` when this session
    /// runs no channel service.
    pub(crate) fn link(&self) -> Option<Arc<channels::ChannelLink>> {
        self.link.lock_recover().clone()
    }

    /// Where this session's channel service installs this device's keys: the
    /// broker, the room host and every sealed read meet here.
    pub(crate) fn keys(&self) -> ChannelKeyProvider {
        self.keys.clone()
    }

    /// This device's collaboration identity: the key its endpoint and its
    /// channels sign with, and the certificate that says whose it is.
    pub(crate) fn identity(&self) -> &HostIdentity {
        &self.identity
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

/// A room's revocations, as the threads beside the session's loop ask for
/// them: the broker decides on its own thread and the automation host arms
/// shortcuts on the loop, and both refuse a device the room revoked.
pub(crate) type Revocations = Arc<dyn Fn(&str) -> Option<RevocationList> + Send + Sync>;

/// What a room of this device's engine revokes, for the callers that ask
/// from another thread. A session that holds no room knows of none.
pub(crate) fn revocations_of(engine: Option<&Arc<Engine>>) -> Revocations {
    let engine = engine.cloned();
    Arc::new(move |room: &str| engine.as_ref().and_then(|engine| engine.revocations(room)))
}

/// Where this device's rooms are kept under `state`.
fn record_path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}
