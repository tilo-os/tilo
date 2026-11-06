//! This device's workspaces: the engine that holds the state of every
//! workspace the person joined, and the enrollment that brings a joined
//! workspace's state here.
//!
//! A workspace's state — its people, what its apps recorded, its history — is
//! held by the peer-state engine beside the desk ([`NativeWorkspaceOwner`]),
//! in a file of its own under this device's state ([`FILE`]). Joining a
//! workspace redeems the invitation its door answered with
//! ([`tilo_peer_state::owner::enroll::prepare_and_complete`]), which is how
//! this device becomes a member of the workspace and takes in what it held
//! before this device arrived: a late joiner reads the whole history, not only
//! what is recorded from now on.
//!
//! The engine keeps the workspace id each workspace on the desk is held under
//! ([`Slots`]), because an enrollment installs into the workspace made for it
//! and nowhere else.

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
use tilo_peer_state::join::invitations::{CancelInvitationRequest, PendingInvitation};
use tilo_peer_state::owner::{
    Change, DocumentRead, FileHost, NativeWorkspaceOwner, open_file_with, owner_file_of,
};
use tilo_user_identity::RevocationList;

use crate::channels;
use crate::sync;

/// The engine's record, under this device's state directory: the name an
/// earlier build kept it under, so the state it wrote is still read.
const FILE: &str = "rooms.json";

/// The largest document the session reads out of a workspace.
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

/// The workspace id each workspace on the desk is held under, as the engine's
/// record names them and the desk names its own.
#[derive(Default)]
struct Slots(std::sync::Mutex<BTreeMap<WorkspaceSlot, WorkspaceId>>);

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

    /// Bind the workspace an enrollment installs into to its workspace id
    /// before the record reaches the disk: a publication refused here installs
    /// nothing.
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

/// This device's workspaces, and the identity they are held with.
pub struct Engine {
    owner: NativeWorkspaceOwner,
    slots: Arc<Slots>,
    identity: HostIdentity,
    /// Where the workspaces' channels find this device's keys, once the
    /// channel service is running: the session opens the workspaces before it
    /// opens the service, and every reader of a sealed record goes through
    /// this.
    keys: ChannelKeyProvider,
    /// The workspace transport of this session's collaboration plane, once it
    /// runs: a record the workspace's hosts send only once asked is asked for
    /// here, and the plane is gone while this is `None`.
    link: std::sync::Mutex<Option<Arc<channels::ChannelLink>>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("workspaces", &self.workspaces())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Open this device's workspaces under `state`, with the workspaces the
    /// desk already holds: an enrollment only ever installs into the workspace
    /// made for it, so the engine knows which one that is before it is asked.
    ///
    /// A session that keeps no state (a screenshot, a test without a state
    /// directory) holds no workspace.
    ///
    /// # Errors
    /// The record holds no valid state, or the file cannot be written.
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
        Ok(Self {
            owner,
            slots,
            identity,
            keys: ChannelKeyProvider::default(),
            link: std::sync::Mutex::new(None),
        })
    }

    /// This device's engine, when this computer has an identity and a state
    /// directory to keep its workspaces in.
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

    /// What the workspace `workspace` holds of the app `app`'s `record`: the
    /// document as the workspace last admitted it, which is what the desk
    /// shows of it — also what it held before this device joined.
    ///
    /// A record the workspace keeps sealed opens with this device's channel
    /// keys, so its history reads whole here: the keys a member holds reach
    /// back past the membership it joined in. Reading one is taking part in
    /// its channel ([`Self::join_channel`]): a device that holds none of its
    /// keys yet asks the workspace for them, and reads the record once they
    /// are here.
    ///
    /// A record the workspace's hosts send a device only once it asks (an
    /// on-demand record, or a part of a chain behind its head) is asked for
    /// on its first read, and reads as nothing yet until its history arrived
    /// ([`Self::asks_for_record`]).
    ///
    /// # Errors
    /// The workspace is not held here, this device may not read the record,
    /// the record is sealed and this device holds no keys of it, or the
    /// document cannot be read now.
    pub fn record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
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
        let keys = &self.keys;
        self.owner.with_current(workspace, |view| {
            match view.read_document_with_history(
                reader,
                (app, record),
                None,
                MAX_DOCUMENT_BYTES,
                SystemTime::now(),
                &mut |write| {
                    keys.get()
                        .ok_or("this session's channel service is not running")?
                        .open_history_write(workspace, write)
                },
            )? {
                DocumentRead::Captured(document) => Ok(Some(document.bytes)),
                DocumentRead::Unchanged(_) | DocumentRead::CheckpointPending => Ok(None),
            }
        })
    }

    /// Take part in the channel of `record` of `workspace`, whose keys this
    /// device reads it with: the channel service asks the workspace's devices
    /// for access the first time, and its governance answers with the group.
    /// Says whether the keys are here; while they are not, the record reads as
    /// nothing, and the answer arrives as the workspace's other messages do.
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

    /// The keys this device seals writes of `record` of `app` with in
    /// `workspace`, a workspace whose records are its members' sealed
    /// channels: `Ok(None)` while the record's channel keys are not here yet,
    /// and the answer arrives as the workspace's other messages do.
    ///
    /// # Errors
    /// This session has no channel service, this device has no secret store,
    /// or it takes part in no channel of the record: it holds no keys, and
    /// none are coming.
    pub(crate) fn seal_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Option<channel_crypto::ChannelContentKeys>, String> {
        let Some(keys) = self.keys.get() else {
            return Err("this session's channel service is not running".to_owned());
        };
        let route = collab_types::ChannelRoute::new(app, record);
        match keys.access_record(workspace, &route).0 {
            ChannelAccess::Ready(keys) => Ok(Some(keys)),
            ChannelAccess::Waiting => Ok(None),
            ChannelAccess::Removed => Err(format!(
                "the workspace removed this device from {record}'s channel"
            )),
            ChannelAccess::NoSecretStore => {
                Err("this device has no secret store, so it holds no channel keys".to_owned())
            }
            ChannelAccess::Unavailable => {
                Err("this device takes part in no channel of the record".to_owned())
            }
        }
    }

    /// Ask `workspace`'s hosts for the record on `channel` the first time it
    /// is read, and say whether it is one they send only once asked (an
    /// on-demand record, or a part of a chain behind its head): its history
    /// then arrives as the workspace's other documents do, and later reads ask
    /// nobody.
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

    /// The devices the workspace `workspace` admits, by the publisher each of
    /// their sessions is known by: the roster its window streams carry between
    /// ([`channels::roster_of`]).
    ///
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

    /// The devices the workspace admits, as a call's roster names them:
    /// whoever its governance certified and has not removed may reach a call
    /// in it.
    pub(crate) fn roster_devices(&self, workspace: &str) -> Result<Vec<[u8; 32]>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        channels::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| *device.as_bytes())
                .collect()
        })
    }

    /// Who the workspace has in it, as its governance tells it: every person
    /// it was shared with, by the name it knows them by and the part they take
    /// in it. A joined workspace brings its people with it, so this is how the
    /// desk knows them before any of them is here.
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

    /// The people of `workspace` and the grants it makes, as its governance
    /// here records them: the one source of who shares it, in what part, and
    /// what each of them may do.
    ///
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| view.peer().governance().governed_policy())
    }

    /// Whether this device's person may administer `workspace` here: the
    /// workspace's governance gives their person the grant action, which is
    /// what a change to its people — letting an app author there — is decided
    /// by. A workspace decided by another device's governance says no here,
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

    /// The invitations into `workspace` this device's person issued that are
    /// still redeemable: the links they made, listed where they made them,
    /// since a link is answered by the device that signed it.
    ///
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

    /// Cancel the invitation `invitation` into `workspace`, named by the id
    /// listing it reports, as the person who issued it: the link admits nobody
    /// from now on. Answers whether the workspace changed here; cancelling the
    /// same link again changes nothing.
    ///
    /// # Errors
    /// The workspace is not held here, the invitation was not issued from this
    /// device's person, or the workspace cannot be published.
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

    /// The revocations the workspace's governance records, as it last admitted
    /// them: the devices of any of its people it refuses to hear from. `None`
    /// when this device holds no such workspace, so nothing of it is known.
    ///
    /// What is checked against this: whether a shortcut's saver may still
    /// start it here, and whether an app of the workspace may still be
    /// believed about who saved one.
    #[must_use]
    pub fn revocations(&self, workspace: &str) -> Option<RevocationList> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| {
                let mut list = RevocationList::new();
                for entry in view.peer().revocations() {
                    // The workspace verified every entry when it admitted it:
                    // an entry that does not verify now leaves its refusals
                    // unknown rather than trusting the rest.
                    list.insert(entry).map_err(|error| error.to_string())?;
                }
                Ok(list)
            })
            .ok()
    }

    /// Who governs the workspace, as the id its governance names: the owner it
    /// was made under, whose devices sign for it. A joined workspace keeps the
    /// owner its host's governance has, never the person who joined; one made
    /// here has this person's.
    ///
    /// # Errors
    /// The workspace is not held here yet, or its governance cannot be read
    /// now.
    pub fn owner_of(&self, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            let owner = view.peer().governance().owner_user_public_key();
            if owner == [0; 32] {
                // A workspace whose governance names nobody has no owner to
                // carry: a claim made here would be this device's, not the
                // workspace's.
                return Err("the workspace's governance names no owner".to_owned());
            }
            Ok(hex::encode(owner))
        })
    }

    /// The verdicts `workspace` holds on writes: this device's own, and the
    /// ones other devices recorded on its writes. Each names the changes the
    /// refused write would have brought, so a history row reads where it is
    /// not.
    #[must_use]
    pub fn write_refusals(&self, workspace: &str) -> Vec<neighbour_wire::WriteRefusal> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        self.owner.write_refusals(workspace)
    }

    /// The verdicts other devices recorded on this device's writes, with the
    /// workspace each was filed in, where waiting changes nothing: what the
    /// person hears once per verdict, because their writes stop arriving.
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

    /// Where `workspace`'s host is reached and the device its enrollment pins:
    /// the address its live plane is dialled at.
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

    /// Where the hosts of the workspaces this device holds are reached, by
    /// workspace id, postcard encoded: what the person's other devices need to
    /// enter a workspace listed here at the device that hosts it.
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

    /// Where a read asks the workspaces' hosts for a record they send only
    /// once asked: the plane this session's channels travel on.
    pub(crate) fn attach_link(&self, link: Arc<channels::ChannelLink>) {
        *self.link.lock_recover() = Some(link);
    }

    /// Where this device's channels hear its workspaces' governance and tell
    /// which devices each admits: the plane's side of them, `None` when this
    /// session runs no channel service.
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

    /// Name the workspace a join is installing into by the workspace id it
    /// holds: an enrollment only installs into the workspace made for it, and
    /// says so before the host is asked.
    pub(crate) fn bind(&self, workspace: WorkspaceSlot, workspace_id: WorkspaceId) {
        self.slots.0.lock_recover().insert(workspace, workspace_id);
    }

    /// Let go of a workspace a join did not install into, so this device
    /// keeps nothing it does not hold.
    pub(crate) fn unbind(&self, workspace: WorkspaceSlot) {
        self.slots.0.lock_recover().remove(&workspace);
    }
}

/// What the broker's owner commits ask of this device's workspaces: the
/// engine's owner is the canonical copy of every workspace it holds, and
/// what it commits leaves on the plane's session with the workspace's host.
impl tilo_action_broker::owner_commits::OwnerCustody for Engine {
    /// A held workspace whose host takes part in no channels: its records are
    /// documents the owner commits, never sealed channels.
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
            .material(&self.identity)
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

    /// Send the owner's outgoing custody of `workspace` to its peers soon:
    /// what was just committed leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace_id: WorkspaceId) {
        if let Some(link) = self.link() {
            link.publish_custody(workspace_id);
        }
    }

    /// Start the record empty as this device, from the kind the app's join
    /// named and the declarations its installed package carries: what lets a
    /// device that does not hold a record yet write the first change of it,
    /// as the workspace's host starts it for an app there.
    fn prepare_record(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
        app: &str,
        record: &str,
        kind: &str,
    ) -> Result<(), String> {
        self.check_custody(owner, workspace_id)?;
        let declared = crate::mount::apps::declared_resource_kinds(app)
            .ok_or_else(|| format!("{app} is not installed on this device"))?;
        let route =
            tilo_peer_state::owner::ApplicationRecord::declared(app, record, kind, &declared)?;
        let (_, cert, _, _) =
            tilo_network_actor::native_ingress::Connect::new(owner.clone(), workspace_id)?
                .material(&self.identity)?;
        let key = self.identity.device_signing_key.clone();
        owner
            .prepare_application_record(workspace_id, &cert, &key, &route, SystemTime::now())
            .map(drop)
    }
}

/// The id a listing reports for an invitation, as its 32 bytes: 64 hex
/// characters.
fn invitation_id(text: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(text).map_err(|error| error.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| "an invitation's id is 64 hex characters".to_owned())
}

/// The id cancelling `invite_id` asks under: the link is what is cancelled, so
/// asking again about the same link is the same request.
fn cancellation_id(invite_id: [u8; 32]) -> [u8; 32] {
    *blake3::Hasher::new_derive_key("Tilo invitation cancellation v1")
        .update(&invite_id)
        .finalize()
        .as_bytes()
}

/// A workspace's revocations, as the threads beside the session's loop ask for
/// them: the broker decides on its own thread and the automation host arms
/// shortcuts on the loop, and both refuse a device the workspace revoked.
pub(crate) type Revocations = Arc<dyn Fn(&str) -> Option<RevocationList> + Send + Sync>;

/// What a workspace of this device's engine revokes, for the callers that ask
/// from another thread. A session that holds no workspace knows of none.
pub(crate) fn revocations_of(engine: Option<&Arc<Engine>>) -> Revocations {
    let engine = engine.cloned();
    Arc::new(move |workspace: &str| {
        engine
            .as_ref()
            .and_then(|engine| engine.revocations(workspace))
    })
}

/// Where this device's workspaces are kept under `state`.
fn record_path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}
