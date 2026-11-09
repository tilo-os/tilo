//! This device's workspaces: the engine that holds the state of every workspace
//! the person joined, and the enrollment that brings a joined workspace's state
//! here.
//! A workspace's state — its people, what its apps recorded, its history — is held
//! by the peer-state engine beside the desk ([`NativeWorkspaceOwner`]), in a file of
//! its own under this device's state ([`FILE`]). Joining redeems the invitation its
//! door answered with ([`tilo_peer_state::owner::enroll::prepare_and_complete`]),
//! and a late joiner reads the whole history, not only what is recorded from now
//! on. The engine keeps the id each workspace on the desk is held under
//! ([`Slots`]), because an enrollment installs into the workspace made for it.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tilo_action_broker::channel_encryption::{ChannelAccess, ChannelKeyProvider};
use tilo_action_policy::WorkspacePolicy;
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_identity_host::custody::HostIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, PendingInvitation,
};
use tilo_peer_state::owner::{
    Change, DocumentRead, FileHost, NativeWorkspaceOwner, open_file_with, owner_file_of,
};
use tilo_user_identity::RevocationList;

use crate::channels;
use crate::sync;

/// The engine's record, under this device's state directory: the name an earlier build kept it
/// under, so the state it wrote is still read.
const FILE: &str = "rooms.json";

/// The largest document the session reads out of a workspace.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Someone a workspace has in it: the person, as their user key in hex, the name the workspace
/// knows them by, and the part they take in it.
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

/// Why a workspace is not the person's to delete here, when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotDeletable {
    /// This device does not hold it.
    NotHeld,
    /// Someone else is still in it, so it is not this device's to delete.
    OthersInIt,
    /// Another of the person's devices holds it too: deleting it here would not delete it for them.
    OtherDevice,
    /// It belongs to someone else.
    NotOwner,
}

/// An invitation this device made: the link a person hands over, the part it brings whoever redeems
/// it in at, and when it stops working.
#[derive(Clone, Debug)]
pub(crate) struct Issued {
    /// The invitation's id, as the workspace's own records name it.
    pub(crate) invite: String,
    /// The text a device joins with.
    pub(crate) link: String,
    /// The part it brings the person in at.
    pub(crate) role: MemberRole,
    /// When it stops working, in nanoseconds since the Unix epoch.
    pub(crate) expires_at_ns: u64,
}

/// The workspace id each workspace on the desk is held under, as the engine's record names them and
/// the desk names its own.
#[derive(Default)]
pub(crate) struct Slots(std::sync::Mutex<BTreeMap<WorkspaceSlot, WorkspaceId>>);

impl std::fmt::Debug for Slots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.0.lock_recover().iter()).finish()
    }
}

impl Slots {
    fn of(workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>) -> Self {
        Self(std::sync::Mutex::new(workspaces.into_iter().collect()))
    }
}

impl FileHost for Slots {
    fn slot_workspace(&self, slot: WorkspaceSlot) -> Option<WorkspaceId> {
        self.0.lock_recover().get(&slot).copied()
    }

    /// Bind the workspace an enrollment installs into to its workspace id before the record reaches
    /// the disk: a publication refused here installs nothing.
    fn publish(
        &self,
        _next: &tilo_peer_state::owner::NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let bound = match change {
            Change::Workspace {
                workspace,
                into: Some(target),
                ..
            } => Some((target.workspace, workspace)),
            _ => None,
        };
        if let Some((workspace, held)) = bound {
            self.0.lock_recover().insert(workspace, held);
        }
        let written = write();
        if written.is_err()
            && let Some((workspace, _)) = bound
        {
            self.0.lock_recover().remove(&workspace);
        }
        if let Change::Released { workspace } = change {
            self.0.lock_recover().retain(|_, held| *held != workspace);
        }
        written
    }
}

/// What this device may seal a workspace's record with ([`Engine::seal_keys`]): the channel's keys,
/// the wait for them, or this device's own lack of a secret store to hold any.
pub(crate) enum SealKeys {
    /// The keys this device seals the record's writes with.
    Here(channel_crypto::ChannelContentKeys),
    /// The keys are not here yet: the answer arrives as the workspace's other messages do, and the
    /// write waits for it.
    Waiting,
    /// This device has no secret store, so it holds no channel keys of any
    /// workspace: a sealed record is one it can neither write nor read here,
    /// and no workspace refused anything.
    NoSecretStore,
}

/// This device's workspaces, and the identity they are held with.
pub struct Engine {
    owner: NativeWorkspaceOwner,
    slots: Arc<Slots>,
    identity: HostIdentity,
    /// Where the workspaces' channels find this device's keys, once the channel service is running:
    /// the session opens the workspaces before it opens the service, and every reader of a sealed
    /// record goes through this.
    keys: ChannelKeyProvider,
    /// The workspace transport of this session's collaboration plane, once it runs: a record the
    /// workspace's hosts send only once asked is asked for here, and the plane is gone while this
    /// is `None` .
    link: std::sync::Mutex<Option<Arc<channels::ChannelLink>>>,
    /// The desk's workspaces this device could not found here: founding one is asked for again only
    /// by a later session, not by every pass over the desk.
    unfounded: std::sync::Mutex<std::collections::BTreeSet<WorkspaceId>>,
    /// This device's workspaces as its other devices reach them, when it keeps a state directory to
    /// hold their frames in.
    #[cfg(not(target_arch = "wasm32"))]
    directed: Option<tilo_peer_state::direct::Owning>,
}

/// How much of the workspaces' sealed history this device keeps to hand their other devices: every
/// frame that passes through it, oldest retired first once it holds this much.
const FRAMES_KEPT: tilo_peer_state::store::Appetite = tilo_peer_state::store::Appetite::Bounded {
    max_bytes: 512 * 1024 * 1024,
};

/// A record's bytes, with the heads they were last saved at: what [`Engine::record_since`] answers
/// when the record changed since the caller last read it.
type SavedRecord = Option<(Vec<u8>, [u8; 32])>;

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("workspaces", &self.workspaces())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// This device's workspaces as `owner` holds them, with the identity they are held under: what
    /// a host supplies whichever file, store or record it keeps them in.
    #[must_use]
    pub(crate) fn holding(
        owner: NativeWorkspaceOwner,
        slots: Arc<Slots>,
        identity: HostIdentity,
    ) -> Self {
        Self {
            owner,
            slots,
            identity,
            keys: ChannelKeyProvider::default(),
            link: std::sync::Mutex::new(None),
            unfounded: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            #[cfg(not(target_arch = "wasm32"))]
            directed: None,
        }
    }

    /// Open this device's workspaces under `state`, with the workspaces the desk
    /// already holds. A session that keeps no state (a screenshot, a test without a
    /// state directory) holds no workspace.
    /// # Errors
    /// The record holds no valid state, or the file cannot be written.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(
        identity: HostIdentity,
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>,
    ) -> Result<Self, String> {
        let state = state.ok_or("this session keeps no state, so it holds no workspace")?;
        let path = record_path(state);
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let slots = Arc::new(Slots::of(workspaces));
        let owner = open_file_with(
            &owner_file_of(&path),
            Arc::clone(&slots) as Arc<dyn FileHost>,
        )?;
        let mut engine = Self::holding(owner.clone(), slots, identity);
        // What the workspaces' other devices reach this one at, keeping the frames each brings: a
        // device that holds a workspace is a store device of it.
        engine.directed = match tilo_peer_state::store::Store::open(frames_path(state), FRAMES_KEPT)
        {
            Ok(store) => Some(tilo_peer_state::direct::Owning::new(
                owner,
                engine.identity.device_signing_key.clone(),
                Arc::new(store),
            )),
            Err(error) => {
                tracing::warn!(%error, "this device serves no workspace to its other devices");
                None
            }
        };
        Ok(engine)
    }

    /// This device's engine, when this computer has an identity and a state directory to keep its
    /// workspaces in.
    pub(crate) fn for_this_device(
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = (WorkspaceSlot, WorkspaceId)>,
    ) -> Result<Self, String> {
        let identity = sync::device_identity()
            .ok_or("this computer has no identity yet, so it holds no workspace")?;
        Self::open(identity, state, workspaces)
    }

    /// The workspace ids this device holds.
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.owner.workspaces()
    }

    /// Whether this device holds this workspace.
    #[must_use]
    pub fn holds(&self, workspace: WorkspaceId) -> bool {
        self.owner.contains_workspace(workspace)
    }

    /// What the workspace `workspace` holds of the app `app`'s `record`: the document
    /// as the workspace last admitted it, also what it held before this device joined.
    /// A sealed record opens with this device's channel keys, which reach back past the
    /// membership it joined in; a device that holds none yet asks for them
    /// ([`Self::join_channel`]) and reads the record once they are here. A record the
    /// hosts send only once asked is asked for on its first read.
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record
    /// is sealed and this device holds no keys of it, or the document cannot be read.
    pub fn record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.record_since(workspace, app, record, None)
            .map(|read| read.map(|(bytes, _)| bytes))
    }

    /// [`Self::record`], with the heads the bytes were saved at, and told the heads
    /// `known` this caller already holds: a record whose admitted heads still are those
    /// is answered as unchanged without capturing or saving the document at all.
    /// # Errors
    /// As [`Self::record`].
    pub fn record_since(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        known: Option<[u8; 32]>,
    ) -> Result<SavedRecord, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        if self.asks_for_record(workspace, app, record)? {
            return Ok(None);
        }
        let sealed = self.owner.with_current(workspace, |view| {
            Ok(view.peer().publication().is_sealed_record(app, record))
        })?;
        let joined = if sealed {
            self.join_channel(workspace, app, record)?
        } else {
            true
        };
        if sealed && !joined {
            return Ok(None);
        }
        let reader = self.identity.device_cert.device_public_key;
        // Taken before the workspace's view is held: the channel service's pump settles the groups
        // under its own lock and reads the workspace's governance there, so a read that asked it
        // for keys while holding the view would wait on it as it waits on the view.
        let history = self.keys.get().and_then(|keys| {
            keys.history_keys(workspace, &collab_types::ChannelRoute::new(app, record))
        });
        self.owner.with_current(workspace, |view| {
            match view.read_document_with_history(
                reader,
                (app, record),
                known,
                MAX_DOCUMENT_BYTES,
                SystemTime::now(),
                &mut |write| {
                    let keys = history
                        .as_ref()
                        .ok_or("this session's channel service is not running")?
                        .as_ref()
                        .map_err(Clone::clone)?;
                    keys.open(write)
                },
            )? {
                DocumentRead::Captured(document) => {
                    let revision = document.revision;
                    Ok(Some((document.bytes, revision)))
                }
                DocumentRead::Unchanged(_) | DocumentRead::CheckpointPending => Ok(None),
            }
        })
    }

    /// Every record `workspace` holds here, by its app and id. A workspace this device
    /// does not hold lists nothing.
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed now.
    pub fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| Ok(view.peer().records()))
    }

    /// Take part in the channel of `record` of `workspace` : the channel service asks the
    /// workspace's devices for access the first time, and its governance answers with the group.
    /// Says whether the keys are here; while they are not, the record reads as nothing.
    pub(crate) fn join_channel(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<bool, String> {
        let Some(keys) = self.keys.get() else {
            // Without a channel service there is nothing to take part with,
            // and the read says so itself.
            return Ok(true);
        };
        let route = collab_types::ChannelRoute::new(app, record);
        match keys.access_record(workspace, &route).0 {
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

    /// The keys this device seals writes of `record` of `app` with in `workspace`.
    /// [`SealKeys::Waiting`] says they are not here yet and will arrive as the
    /// workspace's other messages do, which includes a channel the governance does not
    /// name this device in yet. [`SealKeys::NoSecretStore`] is this device's own lack,
    /// not any workspace's verdict.
    /// # Errors
    /// This session has no channel service, or the workspace removed this device from
    /// the record's channel: it holds no keys, and none are coming.
    pub(crate) fn seal_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<SealKeys, String> {
        let Some(keys) = self.keys.get() else {
            return Err("this session's channel service is not running".to_owned());
        };
        let route = collab_types::ChannelRoute::new(app, record);
        Ok(match keys.access_record(workspace, &route).0 {
            ChannelAccess::Ready(keys) => SealKeys::Here(keys),
            // Asked and not admitted yet, or admitted nowhere the governance here can name: either
            // way the keys are not here yet.
            ChannelAccess::Waiting | ChannelAccess::Unavailable => SealKeys::Waiting,
            ChannelAccess::Removed => {
                return Err(format!(
                    "the workspace removed this device from {record}'s channel"
                ));
            }
            ChannelAccess::NoSecretStore => SealKeys::NoSecretStore,
        })
    }

    /// Ask `workspace`'s hosts for the record on `channel` the first time it is read,
    /// and say whether it is one they send only once asked.
    fn asks_for_record(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<bool, String> {
        if neighbour_wire::record_sync(record) == neighbour_wire::RecordSync::Eager {
            return Ok(false);
        }
        let held = self.owner.with_current(workspace, |view| {
            Ok(view.peer().document(app, record).is_some())
        })?;
        // With no plane there is no one to ask: the read answers for itself.
        let Some(link) = self.link.lock_recover().clone().filter(|_| !held) else {
            return Ok(false);
        };
        link.ask_record(workspace, neighbour_wire::app_document_channel(app, record));
        Ok(true)
    }

    /// The devices the workspace `workspace` admits, by the publisher each of their
    /// sessions is known by ([`channels::roster_of`]).
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn roster(&self, workspace: &str) -> Result<Vec<String>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        channels::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| device.to_string())
                .collect()
        })
    }

    /// The devices the workspace admits, as a call's roster names them: whoever its governance
    /// certified and has not removed may reach a call in it.
    pub(crate) fn roster_devices(&self, workspace: &str) -> Result<Vec<[u8; 32]>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        channels::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| *device.as_bytes())
                .collect()
        })
    }

    /// Who the workspace has in it, as its governance tells it: every person it was shared with, by
    /// the name it knows them by and the part they take in it. A joined workspace brings its people
    /// with it, so this is how the desk knows them before any of them is here.
    pub fn people(&self, workspace: &str) -> Result<Vec<Person>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
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

    /// The people of `workspace` and the grants it makes, as its governance records
    /// them: the one source of who shares it, in what part, and what each may do.
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| view.peer().governance().governed_policy())
    }

    /// Whether this device's person may administer `workspace` here: the governance gives their
    /// person the grant action. A workspace decided by another device's governance says no,
    /// whatever this device holds of it.
    #[must_use]
    pub fn administers(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let device = self.identity.device_cert.device_public_key;
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .workspace()
                    .check_workspace_administration(device, SystemTime::now())
                    .is_ok())
            })
            .unwrap_or(false)
    }

    /// The name `workspace` knows `person` by, when it knows one: what an invitation made for them
    /// records as its invitee.
    pub(crate) fn name_of(&self, workspace: &str, person: [u8; 32]) -> Option<String> {
        let person = hex::encode(person);
        self.people(workspace)
            .ok()?
            .into_iter()
            .find(|member| member.person == person)
            .map(|member| member.name)
            .filter(|name| !name.is_empty())
    }

    /// The invitations into `workspace` this device's person issued that are still
    /// redeemable: a link is answered by the device that signed it.
    /// # Errors
    /// The workspace is not held here, or its join ledger cannot be read now.
    pub(crate) fn invitations(
        &self,
        workspace: &str,
        now: SystemTime,
    ) -> Result<Vec<PendingInvitation>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.pending_invitations(workspace, now)
    }

    /// Cancel the invitation `invitation` into `workspace`, as the person who issued it:
    /// the link admits nobody from now on.
    /// # Errors
    /// The workspace is not held here, the invitation was not issued from this device's
    /// person, or the workspace cannot be published.
    pub(crate) fn cancel_invitation(
        &self,
        workspace: &str,
        invitation: &str,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let invite_id = invitation_id(invitation)?;
        let identity = self.identity();
        self.owner.cancel_invitation(
            workspace,
            &CancelInvitationRequest {
                request_id: cancellation_id(invite_id),
                invite_id,
            },
            &identity.device_signing_key,
            now,
        )
    }

    /// Whether this device's person may invite into `workspace` , as its governance answers it.
    /// [`Self::issue_invitation`] asks again, with the part its link hands out.
    #[must_use]
    pub fn may_invite(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let identity = self.identity();
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .governance()
                    .may_invite(&identity.device_signing_key, SystemTime::now()))
            })
            .unwrap_or(false)
    }

    /// Whether this device's person may delete `workspace` : they own it, no one else is in it, and
    /// this device is the only one holding it — so a workspace taken off this device is a workspace
    /// gone everywhere, and not one another of their devices would list again.
    #[must_use]
    pub fn may_delete(&self, workspace: WorkspaceId) -> bool {
        self.not_deletable(workspace).is_none()
    }

    /// Why `workspace` is not this device's person's to delete here, when it is not: what the
    /// surfaces offering deleting say beside a delete they cannot offer.
    #[must_use]
    pub fn not_deletable(&self, workspace: WorkspaceId) -> Option<NotDeletable> {
        if !self.holds(workspace) {
            return Some(NotDeletable::NotHeld);
        }
        let identity = self.identity();
        let person = hex::encode(identity.device_cert.user_public_key);
        let device = identity.device_cert.device_public_key;
        let workspace_id = workspace.to_string();
        let alone = self.people(&workspace_id).is_ok_and(|people| {
            people
                .iter()
                .all(|member| member.removed || member.person == person)
        });
        if !alone {
            return Some(NotDeletable::OthersInIt);
        }
        let only_here = self
            .roster_devices(&workspace_id)
            .is_ok_and(|devices| devices == [device]);
        if !only_here {
            return Some(NotDeletable::OtherDevice);
        }
        if !self
            .owner_of(&workspace_id)
            .is_ok_and(|owner| owner == person)
        {
            return Some(NotDeletable::NotOwner);
        }
        None
    }

    /// Take `workspace` out of this device's record, with everything of it this device
    /// holds. Answers whether the record held it.
    /// # Errors
    /// The record cannot be published.
    pub(crate) fn delete(&self, workspace: WorkspaceId) -> Result<bool, String> {
        self.owner.release_workspace(workspace)
    }

    /// The parts this device's person may hand out in an invitation into `workspace` , from the
    /// least to the most: what the surfaces offering a part to grant list. Empty when the person
    /// may not invite at all, since no part is theirs to hand out then.
    #[must_use]
    pub(crate) fn invitable_roles(&self, workspace: &str) -> Vec<MemberRole> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        let identity = self.identity();
        self.owner
            .with_current(workspace, |view| {
                let now = SystemTime::now();
                Ok([
                    MemberRole::Viewer,
                    MemberRole::Commenter,
                    MemberRole::Editor,
                    MemberRole::Moderator,
                ]
                .into_iter()
                .filter(|role| {
                    view.peer().governance().may_invite_role(
                        &identity.device_signing_key,
                        (*role).into(),
                        now,
                    )
                })
                .collect())
            })
            .unwrap_or_default()
    }

    /// An invitation into `workspace` this device's person makes, at the part `role`
    /// names, redeemable until `expires_in_seconds` from `now`: the link to hand to
    /// whoever is invited. A link is worth as much as the device that signed it being
    /// reachable; when `recipient_user_public_key` names someone, only a device
    /// certified for that person may redeem it.
    /// # Errors
    /// The workspace is not held here, this device's person may not invite into it or
    /// hand out that part, or the workspace cannot be published.
    pub(crate) fn issue_invitation(
        &self,
        workspace: &str,
        role: MemberRole,
        invitee_name: Option<String>,
        recipient_user_public_key: Option<[u8; 32]>,
        expires_in_seconds: u64,
        endpoint: &iroh::EndpointAddr,
        now: SystemTime,
    ) -> Result<Issued, String> {
        let room = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let identity = self.identity();
        // What the joiner dials: this device's endpoint, signed by the device the grant names, as a
        // host of its own workspace.
        let hint = self.own_host_hint(workspace, endpoint)?;
        let now_ns: u64 = now
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
            .try_into()
            .map_err(|_| "this clock is past what an invitation records".to_owned())?;
        let expires_at_ns = now_ns.saturating_add(expires_in_seconds.saturating_mul(1_000_000_000));
        let mut request_id = [0; 32];
        getrandom::fill(&mut request_id).map_err(|error| error.to_string())?;
        let issued = self.owner.issue_invitation(
            room,
            &identity.device_cert,
            IssueInvitationRequest {
                request_id,
                role,
                recipient_user_public_key,
                invitee_name,
                expires_at_ns,
                inviter_name: None,
            },
            vec![hint],
            &identity.device_signing_key,
            now,
        )?;
        Ok(Issued {
            invite: hex::encode(issued.invite_id),
            link: tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation),
            role,
            expires_at_ns,
        })
    }

    /// The revocations the workspace's governance records, as it last admitted them. `None` when
    /// this device holds no such workspace. What is checked against this is whether a shortcut's
    /// saver may still start it here.
    #[must_use]
    pub fn revocations(&self, workspace: &str) -> Option<RevocationList> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| {
                let mut list = RevocationList::new();
                for entry in view.peer().revocations() {
                    // The workspace verified every entry when it admitted it: an entry that does
                    // not verify now leaves its refusals unknown rather than trusting the rest.
                    list.insert(entry).map_err(|error| error.to_string())?;
                }
                Ok(list)
            })
            .ok()
    }

    /// Who governs the workspace, as the id its governance names. A joined workspace
    /// keeps the owner its host's governance has, never the person who joined.
    /// # Errors
    /// The workspace is not held here yet, or its governance cannot be read now.
    pub fn owner_of(&self, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            let owner = view.peer().governance().owner_user_public_key();
            if owner == [0; 32] {
                // A workspace whose governance names nobody has no owner to carry: a claim made
                // here would be this device's, not the workspace's.
                return Err("the workspace's governance names no owner".to_owned());
            }
            Ok(hex::encode(owner))
        })
    }

    /// The verdicts `workspace` holds on writes: this device's own, and the ones other devices
    /// recorded on its writes. Each names the changes the refused write would have brought, so a
    /// history row reads where it is not.
    #[must_use]
    pub fn write_refusals(&self, workspace: &str) -> Vec<neighbour_wire::WriteRefusal> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        self.owner.write_refusals(workspace)
    }

    /// The verdicts other devices recorded on this device's writes, with the workspace each was
    /// filed in, where waiting changes nothing: what the person hears once per verdict, because
    /// their writes stop arriving.
    #[must_use]
    pub fn lasting_refusals(&self) -> Vec<(String, neighbour_wire::WriteRefusal)> {
        let mine = self.identity.device_cert.device_public_key;
        self.workspaces()
            .into_iter()
            .flat_map(|workspace| {
                let name = workspace.to_string();
                self.owner
                    .write_refusals(workspace)
                    .into_iter()
                    .filter(|refusal| refusal.author == mine && refusal.reason.permanent())
                    .map(move |refusal| (name.clone(), refusal))
            })
            .collect()
    }

    /// What each device of `workspace` calls itself, by its key.
    #[must_use]
    pub fn device_labels(&self, workspace: &str) -> std::collections::BTreeMap<[u8; 32], String> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return std::collections::BTreeMap::new();
        };
        self.owner.device_labels(workspace)
    }

    /// The engine's owner, for what holds the workspace's session.
    pub(crate) fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }

    /// What this device answers for the workspaces it holds, beside the planes its endpoint already
    /// serves: the direct link their other devices sync on, and catch up from the frames this one
    /// kept.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn direct_protocols(
        &self,
    ) -> Vec<(Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>)> {
        self.directed.as_ref().map_or_else(Vec::new, |directed| {
            vec![(
                tilo_peer_state::direct::DIRECT_ALPN.to_vec(),
                Box::<dyn iroh::protocol::DynProtocolHandler>::from(directed.clone()),
            )]
        })
    }

    /// Where `workspace` 's host is reached and the device its enrollment pins: the address its
    /// live plane is dialled at.
    pub(crate) fn workspace_host(
        &self,
        workspace: WorkspaceId,
    ) -> Option<([u8; 32], iroh::EndpointAddr)> {
        self.owner.host_addr(workspace)
    }

    /// The device hosting `workspace`: the one whose host orders its commits,
    /// which is where the person's other devices enter it.
    pub(crate) fn host_of(&self, workspace: WorkspaceId) -> Option<[u8; 32]> {
        self.owner
            .with_current(workspace, |view| {
                Ok(view.workspace_host().device_public_key)
            })
            .ok()
    }

    /// Where the hosts of the workspaces this device holds are reached, by workspace id, postcard
    /// encoded: what the person's other devices need to enter a workspace listed here at the device
    /// that hosts it.
    pub(crate) fn host_hints(&self) -> Vec<(String, Vec<u8>)> {
        self.owner
            .workspaces()
            .into_iter()
            .filter_map(|workspace| {
                let hint = self
                    .owner
                    .with_current(workspace, |view| Ok(view.transport_hint().cloned()))
                    .ok()?;
                let bytes = postcard::to_stdvec(&hint).ok()?;
                Some((workspace.to_string(), bytes))
            })
            .collect()
    }

    /// Where a read asks the workspaces' hosts for a record they send only once asked: the plane
    /// this session's channels travel on.
    pub(crate) fn attach_link(&self, link: Arc<channels::ChannelLink>) {
        *self.link.lock_recover() = Some(link);
    }

    /// Where this device's channels hear its workspaces' governance and tell which devices each
    /// admits: the plane's side of them, `None` when this session runs no channel service.
    pub(crate) fn link(&self) -> Option<Arc<channels::ChannelLink>> {
        self.link.lock_recover().clone()
    }

    /// Where this session's channel service installs this device's keys: the broker, the room host
    /// and every sealed read meet here.
    pub(crate) fn keys(&self) -> ChannelKeyProvider {
        self.keys.clone()
    }

    /// This device's collaboration identity: the key its endpoint and its channels sign with, and
    /// the certificate that says whose it is.
    pub(crate) fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    /// Where this device's own host of `workspace` is reached at `endpoint`,
    /// signed by this device: what an invitation names for the joiner, and
    /// what a workspace founded here records of its own host.
    fn own_host_hint(
        &self,
        workspace: &str,
        endpoint: &iroh::EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, String> {
        let workspace_id = tilo_peer_state::join::room_workspace_id(workspace)
            .map_err(|error| error.to_string())?;
        let identity = self.identity();
        let packet_relay_url = endpoint
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            room: workspace.to_owned(),
            workspace_id,
            endpoint_addr: endpoint.clone(),
            host_user_public_key: identity.device_cert.user_public_key,
            host_device_certificate: identity.device_cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&identity.device_signing_key)
        .map_err(|error| error.to_string())
    }

    /// Found `workspace` — made on this desk in `slot` , under `name` — when this device made it
    /// and nothing here holds it yet: who is in it, who may invite and what its apps record become
    /// the workspace's own, hosted here. `endpoint` is where other devices reach this one, which
    /// the workspace's record keeps as its host's address. A workspace this device may not own, or
    /// one that could not be published, stays the desk's own.
    pub(crate) fn found(
        &self,
        slot: WorkspaceSlot,
        workspace: WorkspaceId,
        name: &str,
        endpoint: Option<&iroh::EndpointAddr>,
    ) {
        if self.holds(workspace) || self.unfounded.lock_recover().contains(&workspace) {
            return;
        }
        let identity = self.identity();
        if !identity.device_cert.capabilities.can_own_workspaces {
            return;
        }
        // A session with nowhere other devices reach this one founds the workspace all the same:
        // its records and its governance are held here, and its own host's address is named in the
        // record once this device has one ([`Self::name_own_hosts`]).
        let host_hint = match endpoint {
            Some(endpoint) => match self.own_host_hint(&workspace.to_string(), endpoint) {
                Ok(hint) => Some(hint),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %workspace,
                        "this device's own host of the workspace could not be named"
                    );
                    return;
                }
            },
            None => None,
        };
        match self.owner.create_workspace(
            workspace,
            name,
            &identity.device_cert,
            &identity.device_signing_key,
            host_hint,
            SystemTime::now(),
        ) {
            Ok(_) => self.bind(slot, workspace),
            Err(error) => {
                tracing::warn!(%error, %workspace, "the workspace was not founded here");
                self.unfounded.lock_recover().insert(workspace);
            }
        }
    }

    /// Name where this device's own host of each workspace it holds is reached at `endpoint` , for
    /// one founded before its record said: a workspace founded here is reached at its own host, and
    /// a record that does not say so leaves its records where no device reaches them.
    pub(crate) fn name_own_hosts(&self, endpoint: &iroh::EndpointAddr) {
        for workspace in self.owner.hosts_unnamed() {
            let hint = match self.own_host_hint(&workspace.to_string(), endpoint) {
                Ok(hint) => hint,
                Err(error) => {
                    tracing::warn!(%error, %workspace, "this device's own host could not be named");
                    continue;
                }
            };
            match self.owner.name_host(workspace, hint) {
                Ok(true) => {
                    tracing::info!(%workspace, "the workspace's own host is named in its record");
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "the workspace's own host was not named");
                }
            }
        }
    }

    /// Name the workspace a join is installing into by the workspace id it holds: an enrollment
    /// only installs into the workspace made for it, and says so before the host is asked.
    pub(crate) fn bind(&self, workspace: WorkspaceSlot, workspace_id: WorkspaceId) {
        self.slots.0.lock_recover().insert(workspace, workspace_id);
    }

    /// Let go of a workspace a join did not install into, so this device keeps nothing it does not
    /// hold.
    pub(crate) fn unbind(&self, workspace: WorkspaceSlot) {
        self.slots.0.lock_recover().remove(&workspace);
    }
}

/// What the broker's owner commits ask of this device's workspaces: the engine's owner is the
/// canonical copy of every workspace it holds, and what it commits leaves on the plane's session
/// with the workspace's host.
impl tilo_action_broker::owner_commits::OwnerCustody for Engine {
    /// A held workspace whose host takes part in no channels: its records are documents the owner
    /// commits, never sealed channels.
    fn commits_documents(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
            && self
                .owner
                .with_current(workspace_id, |view| {
                    Ok(!view
                        .workspace_host()
                        .capabilities
                        .takes_part_in_room_channels())
                })
                .unwrap_or(false)
    }

    fn projects_room(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
    }

    /// The engine opened its owner before the session started.
    fn opened_owner(&self, _unavailable: &str) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn check_custody(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<(), String> {
        tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace_id)?
            .custody(&self.identity)
            .map(drop)
    }

    fn signing_key(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<ed25519_dalek::SigningKey, String> {
        self.check_custody(owner, workspace_id)?;
        Ok(self.identity.device_signing_key.clone())
    }

    /// Send the owner's outgoing custody of `workspace` to its peers soon: what was just committed
    /// leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace_id: WorkspaceId) {
        if let Some(link) = self.link() {
            link.publish_custody(workspace_id);
        }
    }
}

/// The id a listing reports for an invitation, as its 32 bytes: 64 hex characters.
fn invitation_id(text: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(text).map_err(|error| error.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| "an invitation's id is 64 hex characters".to_owned())
}

/// The id cancelling `invite_id` asks under: the link is what is cancelled, so asking again about
/// the same link is the same request.
fn cancellation_id(invite_id: [u8; 32]) -> [u8; 32] {
    *blake3::Hasher::new_derive_key("Tilo invitation cancellation v1")
        .update(&invite_id)
        .finalize()
        .as_bytes()
}

/// A workspace's revocations, as the threads beside the session's loop ask for them: the broker
/// decides on its own thread and the automation host arms shortcuts on the loop, and both refuse a
/// device the workspace revoked.
pub(crate) type Revocations = Arc<dyn Fn(&str) -> Option<RevocationList> + Send + Sync>;

/// What a workspace of this device's engine revokes, for the callers that ask from another thread.
/// A session that holds no workspace knows of none.
pub(crate) fn revocations_of(engine: Option<&Arc<Engine>>) -> Revocations {
    let engine = engine.cloned();
    Arc::new(move |workspace: &str| {
        engine
            .as_ref()
            .and_then(|engine| engine.revocations(workspace))
    })
}

/// Where this device's workspaces are kept under `state` .
fn record_path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// Where this device keeps the frames of the workspaces it holds, for the devices of theirs that
/// catch up from it: beside the record that holds the workspaces themselves.
fn frames_path(state: &Path) -> PathBuf {
    state.join("tilo").join("frames")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// A device of a person, certified for their root, with `capabilities`,
    /// and the person's root key.
    fn identity(capabilities: DeviceCapabilities) -> (HostIdentity, [u8; 32]) {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
        let root = person.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".to_owned(),
            capabilities,
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let identity = HostIdentity {
            device_cert: cert,
            device_signing_key: device,
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            custody: None,
        };
        (identity, root)
    }

    /// This device's engine, kept in `state` .
    fn engine(state: &Path, identity: HostIdentity) -> Engine {
        Engine::open(identity, Some(state), Vec::new()).expect("an engine")
    }

    /// Where a desk's own host is reached: its device's endpoint.
    fn endpoint(identity: &HostIdentity) -> iroh::EndpointAddr {
        iroh::EndpointAddr::new(
            iroh::EndpointId::from_bytes(&identity.device_cert.device_public_key)
                .expect("a device endpoint"),
        )
    }

    #[test]
    fn a_desk_workspace_is_founded_under_the_identity_the_desk_made() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, person) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        let slot = WorkspaceSlot(7);

        engine.found(slot, workspace, "Desk", Some(&at));
        // Again, as every pass over the desk does: one workspace, still the one the desk made.
        engine.found(slot, workspace, "Desk", Some(&at));

        assert!(
            engine.holds(workspace),
            "a workspace made on the desk is held here"
        );
        assert_eq!(engine.workspaces(), vec![workspace]);
        assert_eq!(
            engine.owner_of(&workspace.to_string()),
            Ok(hex::encode(person)),
            "the workspace is the person's at this device"
        );
        assert!(
            engine.may_invite(&workspace.to_string()),
            "its owner may invite into it"
        );
    }

    #[test]
    fn a_workspace_this_device_hosts_names_its_own_host_and_commits_here() {
        use tilo_action_broker::owner_commits::OwnerCustody as _;

        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let host = identity.device_cert.device_public_key;
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        let hint = engine
            .owner
            .with_current(workspace, |view| view.transport_hint().cloned())
            .expect("a workspace this device founded names where its host is reached");
        assert_eq!(
            hint.host.device_public_key, host,
            "the host it names is this device, whose host is in this process"
        );
        engine
            .check_custody(&engine.owner, workspace)
            .expect("its own member acts there, its host being in this process");
    }

    #[test]
    fn a_workspace_this_device_hosts_without_an_address_commits_here() {
        use tilo_action_broker::owner_commits::OwnerCustody as _;

        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", None);

        assert!(
            engine.holds(workspace),
            "a session with nowhere other devices reach it founds its desk's workspace"
        );
        assert!(
            engine
                .owner
                .with_current(workspace, |view| Ok(view.transport_hint().is_err()))
                .expect("the workspace is held here"),
            "its record names no host to reach until this device has one"
        );
        engine
            .check_custody(&engine.owner, workspace)
            .expect("its own member acts there, its host being in this process");
    }

    #[test]
    fn a_workspace_made_here_and_held_nowhere_else_may_be_deleted() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();
        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        assert!(
            engine.may_delete(workspace),
            "its owner, in it alone, on its only device, may delete it"
        );
        assert_eq!(
            engine.delete(workspace),
            Ok(true),
            "the record it was held in is taken out"
        );
        assert!(!engine.holds(workspace));
        assert!(!engine.may_delete(workspace));
        assert_eq!(
            engine.delete(workspace),
            Ok(false),
            "a record holding nothing of it has nothing to take out"
        );
    }

    #[test]
    fn a_workspace_this_device_holds_no_record_of_may_not_be_deleted() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let engine = engine(state.path(), identity);

        assert!(!engine.may_delete(WorkspaceId::new()));
    }

    #[test]
    fn a_device_that_may_not_own_workspaces_founds_none() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let mut identity = identity;
        identity.device_cert.capabilities.can_own_workspaces = false;
        let at = endpoint(&identity);
        let engine = engine(state.path(), identity);
        let workspace = WorkspaceId::new();

        engine.found(WorkspaceSlot(7), workspace, "Desk", Some(&at));

        assert!(!engine.holds(workspace));
        assert!(!engine.may_invite(&workspace.to_string()));
    }
}
