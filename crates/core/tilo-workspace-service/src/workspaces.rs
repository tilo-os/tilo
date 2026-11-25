//! Workspaces this server hosts.
//!
//! A shared workspace needs one authority that decides who is in it: it issues
//! invitations, admits the device that redeems one, changes roles and removes
//! members, and publishes each decision as signed workspace governance every
//! member verifies. Here that authority is the service. Its own device is the
//! workspace's host: it keeps the workspace's [`StatePeer`], publishes
//! governance as its own retained changes of the workspace, and hands the
//! member list to the service's admission after every decision, so a
//! member it admits can subscribe at once and a member it removes is cut off.
//!
//! Workspaces are administered as their own `tilo.workspace` governance actions,
//! and the service's own operations (creating, listing and entering workspaces)
//! as `tilo.host` actions (`actions`), or, from a page, over HTTPS: by devices
//! of the server's own user, and by other members as far as the workspace's
//! governance lets them (`authority`, one decision for every entry). Any device
//! redeems an invitation over the shared workspace-join protocol, and a page
//! over HTTPS, where someone the workspace lets invite then admits it.

mod actions;
/// The invitation actions a member asks a workspace for, beside its `invite`.
pub use actions::{CANCEL_INVITATION, INVITATIONS};
/// The route a workspace's record is addressed at, as this service's record reads take it: a
/// program answers with its app's route without naming the store's own crates.
pub use tilo_peer_state::document_frame::DocumentRoute;
mod admissions;
mod admitting;
#[cfg(feature = "apps")]
mod agents_list;
pub mod app_files;
mod appended;
#[cfg(feature = "agents")]
mod applications;
// The grant a person gives with an approve of an asked record.
#[cfg(not(target_arch = "wasm32"))]
mod asked_grants;
// The asked calls that wait, kept in their workspaces' records across the host's restarts.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod asked_kept;
mod authored;
mod authority;
pub(crate) mod blocked;
mod branch;
mod once_grants;
pub use branch::{BranchContent, BranchFileKeys, Branched};
mod channel_governance;
mod collaboration;
mod content;
mod pushes;
pub use pushes::content_of;
/// The records of the content a host stopped keeping, which its members read.
pub use refused::Refusal as ContentRefusalRecord;
mod creating;
mod default_apps;
mod delete;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
mod machines;
mod mcp_server;
mod refused;
pub use machines::WorkspaceMachines;
mod names;
pub use names::PersonNames;
mod web_ui;
pub mod webhooks;
pub use web_ui::WebUi;
mod surfaces;
#[cfg(feature = "agents")]
pub use views::ViewReadPolicy;
#[cfg(feature = "agents")]
pub mod terminal;
#[cfg(all(test, feature = "agents"))]
mod terminal_tests;
pub use content::{ContentHeld, ContentKeeper, ContentRefusal, WholeDocuments, WorkspaceContent};
pub(crate) mod device_actions;
mod follow;
mod founding;
pub mod providers;
mod removals;
mod replacing;
pub mod task_programs;
#[cfg(test)]
mod work_tests;
pub use follow::ResourceWrite;
#[cfg(test)]
mod device_rejoin_tests;
mod holding;
#[cfg(test)]
mod member_action_tests;
#[cfg(test)]
mod returning_person_tests;
pub(crate) use holding::{HostWorkspaceRequest, decode_hex, owner, work_takes};
mod links;
mod member;
pub use member::describe;
pub use member::{AdmittedWrite, CONFINED_HOLDS_NONE, MemberDevice, MemberMaterial, holds_copies};
mod run_admission;
pub use run_admission::{Refusal, SessionAdmission};
mod carried;
mod moving_in;
pub use moving_in::{MovedIn, MovingService, MovingWorkspace, moved_out, moving_out};
mod host_invitations;
mod hosts;
use carried::Carried;
mod clock;
pub(crate) use clock::system_time;
mod listing;
pub use listing::Listed;
pub(crate) mod memberships;
/// The operator's kill switch, held while a host offers one.
pub mod mode;
mod own;
mod ownership;
#[cfg(not(target_arch = "wasm32"))]
mod packaged_model;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use packaged_model::keep_surface;
#[cfg(not(target_arch = "wasm32"))]
pub use packaged_model::package_surface;
mod pages;
mod peer_parts;
mod reads;
mod writes;
// The person's mail accounts and model providers, kept where their
// workspace runs: a page has no mail connection and no vault of its own.
#[cfg(not(target_arch = "wasm32"))]
mod model_providers;
#[cfg(not(target_arch = "wasm32"))]
pub mod person_learned;
pub use model_providers::OpenAiSignIn;
#[cfg(not(target_arch = "wasm32"))]
mod person_settings;
#[cfg(not(target_arch = "wasm32"))]
pub mod this_computer;
// A person's own model, asked for whatever this host runs for them.
#[cfg(not(target_arch = "wasm32"))]
pub mod model_answers;
// What the person calls important, what may interrupt them, and what
// they were told.
#[cfg(not(target_arch = "wasm32"))]
mod person_telling;
#[cfg(not(target_arch = "wasm32"))]
pub use person_telling::{Spenders, ToTell};
// Where a person's devices are reached while their Tilo is closed.
#[cfg(not(target_arch = "wasm32"))]
pub use person_telling::addresses as push_addresses;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod asking;
// What the person keeps to sign in with, in that same vault of theirs.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod person_vault;
#[cfg(not(target_arch = "wasm32"))]
pub use person_vault::{NO_CLIPBOARD, SecretClipboard};
// One vault item's secret handed to a process of an app's own that a turn placed here.
#[cfg(not(target_arch = "wasm32"))]
pub mod vault_release;
// The logins a page in a window of an app's own signs in with, kept in that same vault.
#[cfg(not(target_arch = "wasm32"))]
pub mod page_logins;
// The workspace's one vault, Passwords' document, as this device holds it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod vault;
mod vault_entries;
// That vault as Passwords' own window reads and changes it.
#[cfg(not(target_arch = "wasm32"))]
// The accounts of the workspace, signed in with entries of that vault.
#[cfg(not(target_arch = "wasm32"))]
pub mod account_logins;
// The workspace's accounts as the apps this host serves read them.
#[cfg(all(test, any(feature = "agents", feature = "apps")))]
mod account_logins_tests;
#[cfg(not(target_arch = "wasm32"))]
pub mod accounts;
// The website sessions a person selected, each an entry of that vault.
#[cfg(not(target_arch = "wasm32"))]
pub mod website_sessions;
// Browser's asks for those website sessions, decided where the vault opens.
#[cfg(all(not(target_arch = "wasm32"), feature = "apps"))]
pub mod website_session_asks;
#[cfg(not(target_arch = "wasm32"))]
// What a workspace keeps on the host beside its records, and who reaches it.
#[cfg(not(target_arch = "wasm32"))]
mod workspace_stores;
#[cfg(not(target_arch = "wasm32"))]
pub use workspace_stores::{Reach, Store};
// A secret used by reference by a turn this server carries out itself.
#[cfg(not(target_arch = "wasm32"))]
pub mod hosted_use;
// The vault items a workspace's machine hands the programs it starts.
#[cfg(feature = "agents")]
pub mod machine_secrets;
pub(crate) mod record_reads;
pub use record_reads::{DeviceJobReads, OtherDeviceReads, RecordReads};
mod personal;
pub use personal::{PresentDevices, on_presence_change, presence_changed, present_on};
mod weak;
pub use weak::WeakWorkspaces;
mod document_format;
// What native apps kept in files on this device, taken into the workspaces it holds once.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub mod profile_import;
mod record_authority;
mod resources;
pub use resources::RecordFailure;
pub use resources::person_reading;
mod revocations;
pub use own::OwnWorkspaceOffer;
pub(crate) mod sealed_documents;
/// How a workspace service holds a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// The service hosts it: it settles the workspace's changes itself.
    Hosts,
    /// A member copy of a workspace another device serves.
    MemberCopy,
}
pub use crate::documents::device_keys::{
    AskingBehind, DeviceKeyGroups, DeviceKeys, GroupKeys, GroupKeysAnswer, GroupKeysAnswerFuture,
    KeySink, KeysChangedFuture,
};
#[cfg(test)]
mod copy_now_tests;
#[cfg(test)]
mod member_grants_tests;
pub mod membership;
#[cfg(test)]
mod naming_tests;
pub mod person_list;
pub mod person_workspace;
// The person's own workspace's resources, as a part of a session reads and writes them.
#[cfg(not(target_arch = "wasm32"))]
pub mod person_resources;
#[cfg(not(target_arch = "wasm32"))]
pub use person_resources::{Answer, Authoring, Moved, PersonResources, PersonStore};
// The person's region and formats and their zone: what a view rendered here for them shows
// dates in.
mod readings;
pub use readings::HeldCopy;
#[cfg(not(target_arch = "wasm32"))]
pub mod region;
pub mod server_controls;
pub mod servers;
mod shares;
pub mod standing;
#[cfg(test)]
pub(crate) mod tests;
/// A fixture workspace with its governance as actions, for tests outside this module.
#[cfg(all(test, feature = "agents", not(target_arch = "wasm32")))]
pub(crate) use actions::tests::hosted;
mod unattended;
#[cfg(feature = "agents")]
mod views;
mod wait_list;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::store::{Appetite, Store as LinkStore};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::replica::PeerGuard;

use tilo_peer_state::TransportOrigin;
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{MemberListAccess, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub(crate) use actions::ADD_MEMBER as ADD_MEMBER_ACTION;
pub(crate) use actions::{HostActionServer, ServiceWorkspaceActions};
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{AddMemberRequest, CollaborationError, GovernanceCompletion, MemberPlace};
pub use listing::EntryRefusal;
pub use pages::PreAdmitRequest;
#[cfg(feature = "apps")]
mod profile_memberships;
#[cfg(feature = "apps")]
pub use profile_memberships::ProfileMemberships;
use tilo_server_protocol::WorkspaceId;
pub use wait_list::{Mailbox, Secrets as WaitListSecrets, WaitListMail, WaitListed};

// The administration vocabulary (the actions' names and answers), under the
// daemon's historic paths.
pub use tilo_server_protocol::workspaces::{
    ActionInvitation, CREATE_WORKSPACE, CancelInvitationRequest, CreatedWorkspace, ENTER_WORKSPACE,
    EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE,
    IssueInvitationRequest, IssuedInvitation, LIST_WORKSPACES, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest, UNHOST_WORKSPACE, WorkspaceHosted, WorkspaceListing,
    WorkspaceSummary,
};

use crate::registry::WorkspaceRegistry;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// The record's version this build writes, and the one it reads: it keeps no
/// name of its own, the governance's being the one (see [`document_format`]).
const DOCUMENT_VERSION: u32 = 6;
/// How long members' writes the host took in may wait to be written
/// through to the workspace file. A hard stop loses at most these; replaying
/// what the server retains brings them back.
const KEEP_FOLLOWED: Duration = Duration::from_secs(1);

/// Everything a hosted workspace keeps across restarts. Written atomically
/// after each decision, before it is carried or answered; the large parts
/// of `peer` are files of their own ([`peer_parts`]), and the join receipts
/// are in a file appended to beside it ([`appended`]), all written before it
/// ([`document_format::StoredResource`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkspaceDocumentEntry {
    version: u32,
    workspace: String,
    workspace_id: WorkspaceId,
    owner: DeviceCert,
    peer: Vec<peer_parts::PeerPart>,
    collaboration: collaboration::CollaborationState,
}

/// The extension of the file beside a workspace record holding its join
/// receipts ([`appended`]).
const RECEIPTS: &str = "joins";

/// The file of a workspace's join receipts.
type Receipts = appended::Appended<tilo_peer_state::join_log::JoinCompletionRecord>;

/// A workspace this server hosts.
pub(crate) struct HostedWorkspace {
    record: WorkspaceDocumentEntry,
    peer: Replica,
    /// When the workspace was made, ms since the epoch: the time of the first
    /// change its governance holds, read once as it opens. A listing says it,
    /// and a later change to the workspace does not move it.
    made_at_ms: Option<i64>,
    path: PathBuf,
    /// The files of `record.peer`'s large parts.
    parts: peer_parts::PeerParts,
    /// The join receipts of `record.collaboration`, in their own file.
    receipts: Receipts,
    /// The channel keys of the device this host runs on, asked for the
    /// records of this workspace: `None` when that device keeps no channel
    /// keys, and the workspace's records are then held in the clear.
    device: Option<DeviceKeys>,
    /// The owner's pre-admissions and notices.
    admissions: admissions::Admissions,
    /// The addresses people left at its entry without an invitation.
    wait_list: wait_list::WaitList,
    /// The invitations it honours, kept current for the card a page reads
    /// without taking its lock ([`readings::Invitations`]).
    invitations: readings::Invitations,
    /// Whom moderation removed, and when: the public entry keeps them out for
    /// the day, or until the workspace admits them again.
    removals: removals::Removals,
    /// Member writes to follow before compacting a sealed record history
    /// again, after a compaction failed: each attempt opens and merges the
    /// whole history.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    compaction_wait: usize,
    /// The checkpoint this host checked last of each sealed record, by app and record: the one
    /// it kept its check of, so a write after it does not check the same one again.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    checked_checkpoints: std::collections::BTreeMap<(String, String), [u8; 64]>,
    /// Members' writes the host took in that the workspace file lacks.
    unkept: bool,
    /// Whether writing them through is due already ([`Workspaces::keep_soon`]).
    keeping: bool,
    /// The channels of this workspace's records whose key holders this host
    /// is asking to admit it, by app and record: a burst of writes to a
    /// record the host holds no keys of yet asks once
    /// ([`Workspaces::join_soon`]).
    #[cfg(feature = "device-keys")]
    asking: std::collections::BTreeSet<(String, String)>,
    /// Where the workspace's own document stood when its file was last
    /// written, or it was opened. The workspace's link takes a member's change
    /// into the replica without the workspace's lock, so the replica moves past
    /// it between writes; a change taken in that no write holds yet is written
    /// with the next one ([`Workspaces::take_members_change`]).
    kept_heads: Option<Vec<automerge::ChangeHash>>,
    /// This host's own changes of the workspace's document, signed by its
    /// device, that the next settle carries to the workspace's devices
    /// ([`Workspaces::settle`]): only what this device wrote. A member's change
    /// reaches the others as its own device sent it, carried unchanged.
    unsent: Vec<neighbour_wire::NeighbourMsg>,
    /// This host's sessions to the workspace's other holders
    /// ([`Workspaces::dial_holders`]).
    holders: holding::Holders,
    /// The holder that handed the workspace to this host, dialled whether or
    /// not the governance names its address yet.
    handed_by: Option<tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1>,
    /// For a member copy, the address of the device serving the workspace.
    member: Option<member::Member>,
    /// The workspace's resources, in the framework's store.
    resources: resources::Resources,
}

/// The generation of the base `peer`'s workspace document starts from: `0`
/// for one that never started over.
fn base_generation(peer: &StatePeer) -> u64 {
    peer.admitted_workspace()
        .map_or(0, tilo_peer_state::workspace::base::generation)
}

/// Keep exact admission evidence in this host's copy of the workspace. A
/// member of a workspace whose channels are sealed writes its records sealed,
/// and the host follows those writes into its copy: with exact evidence it
/// keeps each one as the record's ciphertext history and acknowledges it in
/// its next change of the workspace. Without it a sealed write only marks the
/// record encrypted, and every later change of the workspace (letting the
/// next person in among them) is refused as incomplete encryption state.
///
/// The copy also writes under the host's own actor
/// ([`tilo_peer_state::workspace_host_actor`]): a device hosting its own workspaces
/// never writes one workspace from two documents under one actor.
fn select_exact_admission(peer: &mut StatePeer) -> Result<(), String> {
    peer.write_as_workspace_host();
    peer.select_exact_admissions()
        .map_err(|error| format!("workspace exceeds exact admission bounds: {error}"))
}

/// What `candidate`, built from `replica`, holds of the workspace's document
/// that `device`'s host wrote itself, as the one frame its device signs and
/// sends: none when it wrote nothing. What else `candidate` took in past
/// `replica` is a member's, which reached it as that member's device sent it.
fn own_changes(
    replica: &StatePeer,
    candidate: &StatePeer,
    device: &[u8; 32],
) -> Option<neighbour_wire::NeighbourMsg> {
    let actor = candidate.workspace_actor_for(device);
    let changes: Vec<Vec<u8>> = candidate
        .workspace_changes_since(&replica.workspace_heads().unwrap_or_default())
        .into_iter()
        .filter(|bytes| {
            automerge::Change::try_from(bytes.as_slice())
                .is_ok_and(|change| *change.actor_id() == actor)
        })
        .collect();
    (!changes.is_empty()).then(|| neighbour_wire::NeighbourMsg::SyncResp {
        changes,
        workspace: candidate.workspace_id().to_owned(),
    })
}

/// The people `peer`'s governance admits now.
fn member_roots(peer: &StatePeer, now: SystemTime) -> std::collections::BTreeSet<[u8; 32]> {
    peer.governance()
        .member_list_access(now)
        .map(|access| access.member_user_roots)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// What the workspace record kept at `path` holds where its types' rules do not place it, and
/// how many members it has: read from its stored documents without opening it and without
/// writing anything, not even the files an open would make beside it ([`crate::readers_report`]).
/// `surfaces` are the app surfaces the kinds' readers are declared from.
///
/// # Errors
/// The record cannot be read, is of an older layout that opening would rewrite, or one of its
/// stored documents does not read.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn kept_report(
    path: &Path,
    surfaces: &[tilo_actions::surface::Surface],
) -> Result<(String, crate::readers_report::WorkspaceReport), String> {
    // A record of an older layout is brought up by opening it, which writes: a report never
    // writes, so it says what it found instead.
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let (version, _) = postcard::take_from_bytes::<u32>(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if version != DOCUMENT_VERSION {
        return Err(format!(
            "{}: a workspace record of version {version}, which this build opens by writing it",
            path.display()
        ));
    }
    let stored: document_format::StoredResource =
        postcard::from_bytes(&bytes).map_err(|error| {
            format!(
                "{}: {} version {version}: {error}",
                path.display(),
                document_format::FORMAT.name
            )
        })?;
    // The state's parts are read as they are, and a state of an older layout is refused like an
    // old record: opening it writes the parts in today's layout and the file that names them.
    let kept = peer_parts::PeerParts::reading(path)
        .load(&stored.peer)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let (state_version, _) = postcard::take_from_bytes::<u32>(&kept)
        .map_err(|error| format!("{}: peer state: {error}", path.display()))?;
    if state_version != tilo_peer_state::DURABLE_VERSION {
        return Err(format!(
            "{}: a peer state of version {state_version}, which this build opens by writing it",
            path.display()
        ));
    }
    let owner = stored.owner.clone();
    let peer = StatePeer::restore(
        &kept,
        &stored.workspace,
        owner.clone(),
        vec![owner],
        RevocationList::new(),
        tilo_clock::wall(),
    )
    .map_err(|error| format!("{}: peer state: {error}", path.display()))?;
    if !peer.has_governance() {
        return Err(format!(
            "{}: restored workspace has no governance",
            path.display()
        ));
    }
    let (members, name) = workspace_summary(&peer, &stored.workspace);
    let resources = resources::Resources::open(path, stored.workspace_id);
    let mut store = resources.store_mut();
    crate::readers_report::declare_surfaces(&mut store, surfaces);
    Ok((
        name,
        crate::readers_report::WorkspaceReport::of(&store, members),
    ))
}

/// The count of `peer`'s members that stand now, and the name its governance gives the
/// workspace, as [`kept_report`] reports them: what `peer`'s governance says, read under the
/// peer's lock. `workspace` names the workspace its record does.
#[cfg(not(target_arch = "wasm32"))]
fn workspace_summary(peer: &StatePeer, workspace: &str) -> (usize, String) {
    let governance = peer.governance();
    let members = governance
        .member_list(tilo_clock::wall())
        .map_or(0, |members| members.members.len());
    let name = governance
        .workspace_name()
        .unwrap_or_else(|| workspace.to_owned());
    (members, name)
}

impl HostedWorkspace {
    /// Whether this workspace's records are sealed channels its device holds the
    /// keys of: from the moment the device this host runs on takes part in
    /// the workspace's channels.
    fn seals(&self) -> bool {
        self.device.is_some()
    }
    fn open(path: PathBuf, device: Option<&DeviceKeys>) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let (version, _) = postcard::take_from_bytes::<u32>(&bytes).map_err(|error| {
            format!(
                "{}: a workspace record names its version first: {error}",
                path.display()
            )
        })?;
        // A record of any version loads through [`document_format`]'s declaration,
        // which names the one version it brings up: a record of another version
        // is refused with the version it holds, and the file is left as it is.
        let bytes = document_format::FORMAT
            .bring_up(&path, bytes, version)
            .map_err(|error| error.to_string())?;
        let stored: document_format::StoredResource =
            postcard::from_bytes(&bytes).map_err(|error| {
                format!(
                    "{}: {} version {version}: {error}",
                    path.display(),
                    document_format::FORMAT.name
                )
            })?;
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let (receipts, receipted) =
            appended::Appended::open(&path, RECEIPTS, stored.filed_receipts)?;
        // The record names the workspace's owner, a fact of its membership
        // that every holder keeps as it was handed over; the device opening
        // it only writes what this host changes, and is never taken for the
        // owner.
        let record = WorkspaceDocumentEntry::read(stored, receipted);
        let kept = parts
            .load(&record.peer)
            .map_err(|error| format!("peer state parts: {error}"))?;
        // A state an earlier build kept in an older layout is brought up as it is restored, and
        // written again in this build's layout once the workspace is open, so no later start
        // reads the older one.
        let older = postcard::take_from_bytes::<u32>(&kept)
            .is_ok_and(|(version, _)| version < tilo_peer_state::DURABLE_VERSION);
        let mut peer = StatePeer::restore(
            &kept,
            &record.workspace,
            record.owner.clone(),
            vec![record.owner.clone()],
            RevocationList::new(),
            tilo_clock::wall(),
        )
        .map_err(|error| format!("peer state: {error}"))?;
        select_exact_admission(&mut peer)?;
        if !peer.has_governance() {
            return Err("restored workspace has no governance".into());
        }
        drop(kept);
        // The state in today's layout, its parts written before the workspace file that names
        // them; the parts of the older one go once that file is written.
        let today = if older {
            Some(parts.stage(&peer)?)
        } else {
            // Parts an interrupted write left behind.
            parts.collect(&record.peer);
            None
        };
        let mut workspace = Self::assemble(record, peer, path, parts, receipts, device)?;
        if let Some(today) = today {
            workspace.record.peer = today;
        }
        if older {
            workspace
                .persist()
                .map_err(|error| format!("workspace written again in today's layout: {error}"))?;
        }
        Ok(workspace)
    }

    /// A workspace of `entry` and its replica `peer`, kept at `path` with
    /// its `parts` and its receipts' file.
    fn assemble(
        entry: WorkspaceDocumentEntry,
        peer: StatePeer,
        path: PathBuf,
        parts: peer_parts::PeerParts,
        receipts: Receipts,
        device: Option<&DeviceKeys>,
    ) -> Result<Self, String> {
        let kept_heads = peer.workspace_heads();
        let made_at_ms = peer.governance().made_at_ms();
        let invitations = readings::Invitations::default();
        let workspace = Self {
            admissions: admissions::Admissions::open(&path)
                .map_err(|error| format!("admissions: {error}"))?,
            wait_list: wait_list::WaitList::open(&path, invitations.clone())
                .map_err(|error| format!("wait list: {error}"))?,
            removals: removals::Removals::open(&path)
                .map_err(|error| format!("removals: {error}"))?,
            resources: resources::Resources::open(&path, entry.workspace_id),
            record: entry,
            peer: Replica::new(peer),
            made_at_ms,
            parts,
            receipts,
            // Read before the path moves into the workspace.
            handed_by: holding::handed_by(&path),
            member: member::member_of(&path),
            path,
            device: device.cloned(),
            compaction_wait: 0,
            checked_checkpoints: std::collections::BTreeMap::new(),
            unkept: false,
            keeping: false,
            #[cfg(feature = "device-keys")]
            asking: std::collections::BTreeSet::new(),
            kept_heads,
            unsent: Vec::new(),
            holders: holding::Holders::default(),
            invitations,
        };
        workspace.publish_invitations();
        Ok(workspace)
    }

    /// Write the record as it stands ([`document_format::StoredResource`]): the
    /// join receipts added since the last write appended to their file, then
    /// the workspace file naming how many it holds. A write costs what the
    /// change added, not the workspace's size.
    fn persist(&mut self) -> Result<(), String> {
        let write = self.file_write()?;
        if let Err(error) = self.parts.files().write(&write) {
            self.unsure();
            return Err(error);
        }
        self.publish_invitations();
        Ok(())
    }

    /// Keep what a page reads of this workspace's invitations current
    /// ([`readings::Invitations`]): the log's word on each invitation it
    /// keeps, and which of them someone redeemed.
    fn publish_invitations(&self) {
        let log = self.collaboration();
        let offered = log
            .invitations
            .iter()
            .map(|entry| (entry.issued.invite_id, entry.cancellation.is_some()))
            .collect();
        let redeemed = log
            .join_completions
            .iter()
            .map(|completion| completion.bootstrap.invite_id)
            .collect();
        self.invitations.change(|issued| {
            issued.offered = offered;
            issued.redeemed = redeemed;
        });
    }

    /// The files a write of the workspace file found appended may hold
    /// entries the workspace file on disk does not name.
    fn unsure(&mut self) {
        self.receipts.unsure();
    }

    /// The workspace file as the record stands, to write: the join receipts
    /// added since the last write are appended to their file first.
    fn file_write(&mut self) -> Result<peer_parts::FileWrite, String> {
        let log = &self.record.collaboration;
        let filed_receipts = self.receipts.file(&log.join_completions)?;
        let beyond = |filed: u64| usize::try_from(filed).map_err(|error| error.to_string());
        let record = document_format::StoredDocumentRef {
            version: DOCUMENT_VERSION,
            workspace: &self.record.workspace,
            workspace_id: self.record.workspace_id,
            owner: &self.record.owner,
            peer: &self.record.peer,
            collaboration: document_format::LogRef {
                invitations: &log.invitations,
                join_challenges: &log.join_challenges,
                join_completions: log
                    .join_completions
                    .get(beyond(filed_receipts)?..)
                    .unwrap_or_default(),
                membership_challenges: &log.membership_challenges,
                administration: &log.administration,
            },
            filed_receipts,
        };
        let bytes = postcard::to_stdvec(&record).map_err(|error| error.to_string())?;
        Ok(self.parts.file_write(&self.path, bytes, &self.record.peer))
    }

    fn peer(&self) -> PeerGuard<'_> {
        self.peer.lock()
    }

    /// Whether `device` administers the workspace's applications: the device its
    /// governance names to make the compacted base
    /// ([`tilo_peer_state::PeerGovernance::base_maker`]), so every host
    /// agrees on one and the next of the owner's devices takes over when that
    /// one stops serving the workspace; the device it was made on while its
    /// governance names no such device.
    fn administers_applications(&self, device: &DeviceCert) -> bool {
        let hosting_device = self
            .peer()
            .governance()
            .base_maker()
            .unwrap_or(self.record.owner.device_public_key);
        hosting_device == device.device_public_key
    }

    fn workspace(&self) -> &str {
        &self.record.workspace
    }

    const fn workspace_id(&self) -> WorkspaceId {
        self.record.workspace_id
    }

    fn collaboration(&self) -> &collaboration::CollaborationState {
        &self.record.collaboration
    }

    /// The signed invitation the workspace issued `person` that is still
    /// redeemable at `now_ns`: the entry of a workspace that takes people by
    /// invitation hands that one back rather than minting one.
    fn invitation_for(&self, person: [u8; 32], now_ns: u64) -> Option<IssuedInvitation> {
        self.collaboration().invitation_for(person, now_ns)
    }

    /// Record `state` alone: challenges, invitations, receipts. The workspace's
    /// document is unchanged.
    fn commit_state(&mut self, state: collaboration::CollaborationState) -> Result<(), String> {
        let prior = std::mem::replace(&mut self.record.collaboration, state);
        if let Err(error) = self.persist() {
            self.record.collaboration = prior;
            return Err(error);
        }
        Ok(())
    }

    /// Make `change` to the workspace as its copy stands, and keep it as
    /// [`Self::keep`] does, the copy held from the build to the write: a
    /// member's write the workspace's link takes in meanwhile waits, rather
    /// than landing between the copy the change was made on and its write.
    /// Answers what `change` answers; nothing is kept when it changed nothing.
    fn change<T>(
        &mut self,
        key: &SigningKey,
        now: SystemTime,
        change: impl FnOnce(&StatePeer, &mut StatePeer) -> Result<(T, bool), String>,
        state: impl FnOnce(
            &WorkspaceDocumentEntry,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<T, String> {
        let shared = self.peer.shared();
        let mut replica = shared.lock();
        let mut candidate = replica.clone();
        let (answer, changed) = change(&replica, &mut candidate)?;
        if changed {
            self.keep_held(&mut replica, candidate, (key, now), state)?;
        }
        drop(replica);
        Ok(answer)
    }

    /// Keep `candidate`, the workspace's copy with a change made to it, as
    /// the workspace: written with the collaboration state `state` derives
    /// from it, in one write before anything is answered. What this host's
    /// own device wrote in it the next settle carries; nothing else of it is
    /// sent from here.
    fn keep(
        &mut self,
        candidate: StatePeer,
        key: &SigningKey,
        now: SystemTime,
        state: impl FnOnce(
            &WorkspaceDocumentEntry,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<(), String> {
        let shared = self.peer.shared();
        let mut replica = shared.lock();
        self.keep_held(&mut replica, candidate, (key, now), state)
    }

    /// Keep `candidate`, a change built from `replica`, which the caller
    /// holds: a member's write taken into the replica meanwhile waits, so it is
    /// neither lost to the change nor leaves the change unkeepable.
    ///
    /// When the workspace's history is due to start over
    /// ([`StatePeer::compaction_due`]) and this host's device is one of a
    /// person the member list names as its owner or an administrator, it
    /// starts it over from a base it signs ([`StatePeer::compact_workspace`]):
    /// the host acts through its own row, as any of their devices does.
    fn keep_held(
        &mut self,
        replica: &mut StatePeer,
        mut candidate: StatePeer,
        (key, now): (&SigningKey, SystemTime),
        state: impl FnOnce(
            &WorkspaceDocumentEntry,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<(), String> {
        let keeping = crate::timing::start();
        let device = key.verifying_key().to_bytes();
        // The owner changes only by a transfer the workspace's member list
        // replays; any other owner is another workspace.
        if candidate.workspace_id() != replica.workspace_id() || !candidate.keeps_owner_of(replica)
        {
            return Err("a workspace change cannot replace the workspace".into());
        }
        // A base an owner's or administrator's device made, taken in here: what
        // this host wrote against the history it replaced and the base does not
        // hold, it writes again on the base, as its own change.
        if base_generation(&candidate) > base_generation(replica) {
            candidate.carry_onto_base(replica, &device)?;
        }
        let arriving = !member_roots(&candidate, now).is_subset(&member_roots(replica, now));
        let compacts = match self.compacts(&candidate, &device) {
            Some(cert) if candidate.compaction_due(arriving) => {
                candidate.compact_workspace(&cert, key, now)?
            }
            _ => false,
        };
        let own = if compacts {
            Some(candidate.workspace_message())
        } else {
            own_changes(replica, &candidate, &device)
        };
        let mut record = self.record.clone();
        let encoding = crate::timing::start();
        record.peer = self.parts.stage(&candidate)?;
        crate::timing::window("workspace.commit.durable", encoding);
        let deriving = crate::timing::start();
        record.collaboration = state(&record, &candidate)?;
        crate::timing::window("workspace.commit.state", deriving);
        let prior = std::mem::replace(&mut self.record, record);
        let persisting = crate::timing::start();
        let persisted = self.persist();
        crate::timing::window("workspace.commit.persist", persisting);
        if let Err(error) = persisted {
            self.record = prior;
            return Err(error);
        }
        self.kept_heads = candidate.workspace_heads();
        *replica = candidate;
        self.unsent.extend(own);
        crate::timing::event("workspace.governance_commit", keeping, 0);
        Ok(())
    }

    /// The certificate of `device`, this host's, when it starts `candidate`'s
    /// history over once that is due: the device's person is one the member
    /// list names to administer the workspace's history, and the device is the
    /// one its governance names to do it
    /// ([`tilo_peer_state::PeerGovernance::base_maker`]), so two devices do not
    /// each start it over; the device the workspace was made on while its
    /// governance names none.
    fn compacts(&self, candidate: &StatePeer, device: &[u8; 32]) -> Option<DeviceCert> {
        let maker = candidate
            .governance()
            .base_maker()
            .unwrap_or(self.record.owner.device_public_key);
        if maker != *device {
            return None;
        }
        candidate
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == *device)
            .filter(|cert| candidate.administers_history(&cert.user_public_key))
    }

    /// The workspace's name: the one its governance records, which its
    /// founding wrote and every rename writes, wherever the rename was made.
    /// Every listing and every member's device reads this one name.
    fn name(&self) -> String {
        self.peer()
            .governance()
            .workspace_name()
            .unwrap_or_default()
    }

    /// The people the workspace's governance has, its owner among them, as
    /// [`Workspaces::people`] shows them.
    fn summary(&self) -> WorkspaceSummary {
        let list = self
            .peer()
            .governance()
            .member_list(tilo_clock::wall())
            .unwrap_or_default();
        WorkspaceSummary {
            workspace: self.record.workspace.clone(),
            workspace_id: self.record.workspace_id,
            name: self.name(),
            generation: self.peer().governance().generation(),
            members: list.members,
            devices: list.devices,
        }
    }

    /// This workspace as a listing names it: what it is called and which
    /// generation it stands at, without the people a summary answers, which
    /// a listing does not, and when it was made.
    fn listing(&self) -> WorkspaceListing {
        WorkspaceListing {
            workspace: self.record.workspace.clone(),
            workspace_id: self.record.workspace_id,
            name: self.name(),
            generation: self.peer().governance().generation(),
            role: None,
            labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            unattended: false,
            made_at_ms: self.made_at_ms,
        }
    }
}

/// The extension of the marker beside a workspace file this host released.
const RELEASED: &str = "released";

/// What the workspace authority needs from the server hosting its workspaces.
#[derive(Clone)]
pub(crate) struct WorkspaceServices {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::service::MemberListAccessWriter,
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: Arc<std::sync::RwLock<RevocationList>>,
    /// Pre-admissions each workspace keeps, spent ones included.
    pub(crate) max_pre_admissions: usize,
    /// The live handshake trust policy, which an admission is decided with
    /// ([`crate::workspace_access::decide`]), and whom the host's own operations answer.
    pub(crate) trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// The devices and people this host's operator blocks, as the record of
    /// their own workspace says ([`blocked`]): refused on every protocol here.
    pub(crate) blocked: Arc<std::sync::RwLock<blocked::BlockedKeys>>,
    /// The devices this server certified and placed itself, for the windows in its workspaces'
    /// machines ([`crate::environment`]): none of them is a device of a person's own.
    pub(crate) environments: crate::environment::EnvironmentRegistry,
    /// The work this device takes: it serves a workspace placed on it only when the setting
    /// takes the placing person's work.
    pub(crate) work: tilo_server_protocol::WorkSetting,
    /// The relays this device holds back until something needs it reachable.
    pub(crate) reachability: tilo_endpoint_reachability::Reachability,
    /// Whether the endpoint had named where it is reached when the service opened, or names it
    /// only later ([`crate::service::ServiceConfig::reaching`]).
    pub(crate) reaching: tilo_endpoint_reachability::Reaching,
    /// What this keeper says of itself among the holders of the workspaces it serves, which
    /// its signed address carries.
    pub(crate) holder: Arc<std::sync::RwLock<HolderClaim>>,
    /// The channel service of the device this host runs on, when the
    /// embedder runs one: the host seals and opens the records of the
    /// workspaces it hosts with its keys and admits nobody.
    pub(crate) channels: Option<DeviceKeyGroups>,
    /// The device this embedder's member copies act as, when it keeps any:
    /// what the copies kept from an earlier run sync as, from the open on.
    pub(crate) member: Option<Arc<member::MemberDevice>>,
    /// The app surfaces this device knows, which its workspaces' stores judge resource writes
    /// by ([`crate::service::ServiceConfig::surfaces`]).
    pub(crate) surfaces: Arc<collab_types::app_surface::Surfaces>,
    /// The apps this host serves, when its embedder serves any: what its
    /// devices reach on its action service ([`crate::app_serving::Served`]).
    pub(crate) apps: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// The tokens this host's fetch proxy takes, which the host mints for a
    /// page that asks it ([`tilo_server_protocol::workspaces::FETCH_GRANT`]).
    pub(crate) fetch_grants: Arc<crate::fetch_grants::Grants>,
    /// Where this device's native apps kept their files, which its workspaces take in once
    /// ([`crate::service::ServiceConfig::app_files`]).
    #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
    pub(crate) app_files: Arc<profile_import::AppFiles>,
}

impl WorkspaceServices {
    /// What this keeper says of itself among a workspace's holders.
    pub(crate) fn holder(&self) -> HolderClaim {
        self.holder.read_recover().clone()
    }

    /// The key this device's endpoint is keyed by, which its device actions sign as, and the
    /// certificate of that key: the device's own, whose one key signs the records it writes,
    /// serves its links and answers its content.
    pub(crate) fn endpoint_identity(
        &self,
    ) -> (Arc<dyn collab_types::signer::DeviceSigner>, DeviceCert) {
        (Arc::new(self.key.clone()), self.cert.clone())
    }
}

/// Who reads a resource this device publishes of its own ([`Workspaces::publish_own`]), beside
/// this device's own person acting as themselves on it, who always does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnReaders {
    /// Nobody else: what stands on this computer alone, as where its folders stand.
    ThisDevice,
    /// Whoever the person's own workspace lets read its records, by its own read rules: what
    /// the person reads anywhere they are, as the styles the device's packages offer, read by a
    /// page of theirs, a member's view and an agent alike.
    Workspace,
    /// The policy decision at each record's own node: the workspace's owner reads as their own
    /// record (`OwnRecord`), and anyone else only by a member grant naming them there. What a
    /// device's own settings follow: its person reads them from any of their devices, and
    /// whoever they granted a setting reads that setting in their own workspace.
    OwnRecords,
}

/// The workspaces this server hosts.
#[derive(Clone)]
pub struct Workspaces {
    inner: Arc<WorkspacesInner>,
}

/// What a keeper says of itself among the holders of the workspaces it serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HolderClaim {
    /// What it calls itself: its server's name from its person's admin workspace, or its site's
    /// name. Empty for its device's own label.
    pub(crate) label: String,
    /// Whether the machine under it is rented from a cloud provider, whose operator can read
    /// what it holds.
    pub(crate) cloud: bool,
    /// The web origin its site answers at, when it serves one.
    pub(crate) origin: Option<String>,
}

/// The resources this device publishes of its own, by the app they are read at, each with the
/// readers that may see it.
type OwnReadings = std::sync::RwLock<
    std::collections::BTreeMap<
        &'static str,
        (OwnReaders, Arc<dyn tilo_actions::remote::ResourceReads>),
    >,
>;

/// How a hand-on of what this device keeps ended, once it did ([`Workspaces::hand_on_before_leaving`]).
type HandOnEnded = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

struct WorkspacesInner {
    host: WorkspaceServices,
    /// Where the workspaces' live resources are shown, once the session asks for them
    /// ([`Workspaces::show_live`]).
    live: std::sync::OnceLock<crate::live::LiveRows>,
    /// The sealed history of every workspace hosted here, which their
    /// member devices catch up from over the direct link.
    store: Arc<tilo_peer_state::store::Store>,
    /// The direct links of the workspaces hosted here, one per workspace,
    /// answering a device's dial by the workspace it names.
    direct: tilo_peer_state::direct::Serving,
    /// The devices each workspace's link admitted, by workspace: what
    /// [`Workspaces::watch_link_access`] re-checks when what admission reads
    /// changes.
    link_remotes: std::sync::Mutex<
        std::collections::BTreeMap<WorkspaceId, std::collections::BTreeSet<[u8; 32]>>,
    >,
    /// The links whose device signs its own row onto the workspace's member
    /// list before it is a member device's, by workspace and device
    /// ([`tilo_peer_state::direct::LinkScope::lists_itself`]).
    listing_themselves: std::sync::Mutex<std::collections::BTreeSet<(WorkspaceId, [u8; 32])>>,
    /// The agent sessions this device serves, present on it while each connection lasts.
    agent_sessions: personal::AgentSessions,
    /// What the workspaces kept here hold, read and written in process.
    records: crate::workspace_documents::WorkspaceDocuments,
    /// The apps this host serves, when its embedder serves any: what a request
    /// handed to another device is reported to ([`crate::app_serving::Hand`]).
    applications: Option<Arc<dyn crate::app_serving::AppActionServer>>,
    /// The member copies this device holds ([`member`]).
    member: member::Members,
    /// The runs admitted to this service's workspaces, whose calls are
    /// checked against the material each was admitted under
    /// ([`run_admission`]).
    executions: Arc<run_admission::ExecutionPins>,
    /// Whether this service answers the catch-up a device keeping one of its
    /// workspaces asks for as a build before the ask does: not at all
    /// ([`Workspaces::speak_no_catch_up`]).
    #[cfg(any(test, feature = "test-support"))]
    speaks_no_catch_up: std::sync::atomic::AtomicBool,
    /// How many cursors an answer of where this device's store stands
    /// carries, when a test sets it ([`Workspaces::answer_cursors_in_pages_of`]);
    /// `0` keeps the build's own.
    #[cfg(any(test, feature = "test-support"))]
    cursors_per_answer: std::sync::atomic::AtomicUsize,
    /// Where iCloud accounts' calendars are kept, when a test stands a
    /// calendar server of its own in ([`Workspaces::keep_icloud_calendars_at`]).
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    icloud_calendars_at: std::sync::RwLock<Option<String>>,
    /// The credential store a test signs drives in through
    /// ([`Workspaces::cloud_sign_in_through`]); the committed registrations
    /// otherwise.
    #[cfg(any(test, feature = "test-support"))]
    cloud_sign_in: std::sync::RwLock<Option<account_credentials::CredentialStore>>,
    /// What signs an `Apple ID` in, when a test stands a provider of its own
    /// in ([`Workspaces::icloud_sign_in_through`]); Apple's otherwise.
    #[cfg(any(test, feature = "test-support"))]
    icloud_sign_in:
        std::sync::RwLock<Option<std::sync::Arc<dyn account_credentials::icloud::ICloudAuth>>>,
    /// What controls this device's cloud mounts, when a test stands a service of its
    /// own in ([`Workspaces::cloud_control_through`]); the device's files service otherwise.
    #[cfg(any(test, feature = "test-support"))]
    cloud_control: std::sync::RwLock<
        Option<std::sync::Arc<dyn crate::workspaces::person_settings::cloud_control::CloudControl>>,
    >,
    /// Whether this server is about to restart and hands its work to the
    /// other holders: it takes and renews no lease ([`Workspaces::drain`]).
    draining: std::sync::atomic::AtomicBool,
    /// The mode the host runs under, when its operator offers a kill switch: what its entry
    /// admits follows it ([`Workspaces::hold_mode`]).
    mode: std::sync::RwLock<Option<Arc<mode::HostMode>>>,
    dir: PathBuf,
    /// The workspaces hosted here, each under its id spelled out: the one
    /// id a workspace has, which its channels carry too ([`Workspaces::held`]).
    workspaces: Arc<device_actions::WorkspacesMap>,
    /// What each hosted workspace is read through without its lock
    /// ([`Workspaces::reading`]).
    readings: readings::Readings,
    /// The grants given once a call admitted here spent and that are not yet taken back
    /// ([`Workspaces::spend_once`]).
    once_claims: once_grants::OnceClaims,
    /// What this device opened of the sealed resources its own roles read
    /// ([`Workspaces::own_read`]).
    own_opened: reads::OwnOpened,
    /// The language runtime the daemon's program mounted, where it mounted one
    /// ([`Workspaces::provide_tilolang`]): what reads and changes the records of the tilolang
    /// apps built into the build.
    #[cfg(not(target_arch = "wasm32"))]
    tilolang: std::sync::OnceLock<crate::tl_records::Tilolang>,
    /// The action service the workspaces answer their members' devices on, as this
    /// host's own device ([`device_actions`]).
    action_service: tilo_peer_state::actions::ActionService,
    /// The work this host runs for its workspaces' devices: none until an
    /// embedder says what content it keeps ([`Workspaces::run_jobs`]).
    jobs: Arc<device_actions::DeviceWork>,
    /// The runtime an embedder's own writes of a record are carried by: the service's writes
    /// thread ([`crate::service::WorkspaceService::writes_runtime`]), whose thread nothing else
    /// uses, so a write that waits on the key service or a link never waits on the thread that
    /// asked for it — nor on the reactor a record's read was made on.
    record_writes: std::sync::OnceLock<Option<tokio::runtime::Handle>>,
    /// The thread driving that runtime: work it starts itself is never waited for there, since
    /// nothing would run it.
    writes_thread: std::sync::OnceLock<std::thread::ThreadId>,
    /// Who waits for whom while workspaces are created ([`Creating`]).
    creating: creating::Creating,
    /// The workspace UUID after each settled governance decision.
    changes: tokio::sync::broadcast::Sender<String>,
    /// The servers being handed a workspace now, by workspace and device: named among those
    /// keeping it, but not yet saying they keep all of it ([`Workspaces::handing_to`]).
    handing: std::sync::Mutex<std::collections::BTreeSet<(String, [u8; 32])>>,
    /// The hand-ons of what this device keeps, started as an owner takes it
    /// out of a workspace, by workspace: how each ended, once it did
    /// ([`Workspaces::hand_on_before_leaving`]).
    handing_on: std::sync::Mutex<std::collections::HashMap<String, HandOnEnded>>,
    /// Each workspace deleted here, and its server workspace
    /// ([`Workspaces::deletions`]).
    deletions: tokio::sync::broadcast::Sender<(String, WorkspaceId)>,
    /// Each branch made here ([`Workspaces::branches`]).
    branches: tokio::sync::broadcast::Sender<Branched>,
    /// What copies a source's content into a branch of it
    /// ([`Workspaces::branch_content`]).
    branch_content: std::sync::RwLock<Option<Arc<dyn BranchContent>>>,
    /// Where calls that wait for a person's yes are kept as their asked records
    /// ([`Workspaces::asked_store`]).
    asked_records: std::sync::OnceLock<Arc<dyn tilo_actions::remote::asked::AskedRecords>>,
    /// The store those records are kept in, which admits a person's answer to one.
    asked_store: std::sync::OnceLock<Arc<tilo_actions::remote::asked::AskedStore>>,
    /// This host's dispatch, as that store hands it back the calls it keeps.
    asked_dispatch: std::sync::OnceLock<Arc<dyn tilo_actions::remote::asked::AskedDispatch>>,
    /// The dispatch an app's asked use of a vault item is kept under, and the records they are
    /// written through ([`vault_release`]).
    #[cfg(not(target_arch = "wasm32"))]
    vault_asks: std::sync::OnceLock<(
        Arc<dyn tilo_actions::remote::asked::AskedDispatch>,
        Arc<dyn tilo_actions::remote::asked::AskedRecords>,
    )>,
    /// What runs the workspaces' Linux machines, as a branch takes its
    /// source's and a deletion takes the workspace's ([`Workspaces::machines`]).
    machines: std::sync::RwLock<Option<Arc<dyn WorkspaceMachines>>>,
    /// The resources this device publishes of its own, by the app they are read at
    /// ([`Workspaces::publish_own`]).
    own_readings: OwnReadings,
    /// Where the name each person gave themselves is kept ([`Workspaces::person_names`]).
    person_names: std::sync::RwLock<Option<Arc<dyn PersonNames>>>,
    /// What starts and stops this device's own web UI
    /// ([`Workspaces::serve_web_with`]).
    web_ui: std::sync::RwLock<Option<Arc<dyn WebUi>>>,
    /// Each record write of a member the host's copy took in.
    document_writes: tokio::sync::broadcast::Sender<ResourceWrite>,
    /// Moves whenever a person's own settings kept here — their mail
    /// accounts — changed, so a window following them reads them again
    /// ([`person_settings`]).
    person_settings_moved: tokio::sync::watch::Sender<u64>,
    /// Where this device puts a secret its person copies from the vault
    /// ([`person_vault::SecretClipboard`]): none until its embedder says.
    #[cfg(not(target_arch = "wasm32"))]
    secret_clipboard: std::sync::OnceLock<Arc<dyn person_vault::SecretClipboard>>,
    /// What answers people's model calls, each with that person's own
    /// provider, model and key ([`model_answers`]).
    #[cfg(not(target_arch = "wasm32"))]
    models: Arc<model_answers::Models>,
    /// What uses of a vault item by reference keep here ([`hosted_use`]).
    #[cfg(not(target_arch = "wasm32"))]
    vault_uses: hosted_use::Uses,
    /// What its people call important, one writer at a time
    /// ([`person_telling`]).
    #[cfg(not(target_arch = "wasm32"))]
    telling: person_telling::Telling,
    /// What members asking for content are answered from.
    content: content::ContentSlot,
    /// Channel access requests and commits of members waiting for the device's
    /// channel service, by workspace.
    channel_notices: std::sync::Mutex<HashMap<String, Vec<neighbour_wire::NeighbourMsg>>>,
    /// Pre-admitted redemptions waiting for their workspace's next admission
    /// pass, by workspace.
    queued_admissions: std::sync::Mutex<HashMap<String, Vec<pages::QueuedAdmission>>>,
    /// Those waiting for their workspace's next write of the changes it staged,
    /// by workspace.
    queued_writes: std::sync::Mutex<HashMap<String, Vec<collaboration::Written>>>,
    /// How many times a workspace replayed what the store keeps for it
    /// ([`Workspaces::replays`]).
    replays: std::sync::atomic::AtomicU64,
    /// When each workspace was last written to, and a ping after each entry
    /// that changed: the directory's own, beside the entries' governance.
    directory: listing::Directory,
    /// The entry's mail, once whoever runs the server offers it
    /// ([`Workspaces::mail_wait_list`]).
    entry: wait_list::EntrySettings,
    /// Held while a workspace is let go ([`Workspaces::release`]): whoever
    /// asks after it waits until what the host served of it is gone.
    releasing: AsyncMutex<()>,
    /// The workspaces of people's own, once the host's operator offers them
    /// ([`Workspaces::offer_own_workspaces`]).
    own: std::sync::OnceLock<own::OwnWorkspaces>,
    /// The trees this service puts files into when no folder of a device holds them
    /// ([`Workspaces::puts`]).
    #[cfg(all(feature = "files", not(target_arch = "wasm32")))]
    puts: std::sync::OnceLock<Arc<crate::files::Puts>>,
    /// Which people each workspace here lists, by person, as its governance
    /// settled last ([`memberships`]).
    memberships: Arc<memberships::Memberships>,
    /// The owner invitations the host handed out and nobody accepted yet
    /// ([`host_invitations`]).
    owner_invitations: host_invitations::OwnerInvitations,
    /// Each person's devices as their own workspace lists them, by device key:
    /// the name each reads by and whether a revocation here refuses it. Kept
    /// beside the resource so the host answers a pairing synchronously.
    device_names: std::sync::Mutex<HashMap<[u8; 32], personal::DeviceNames>>,
    /// The devices the host's own person runs an agent on, and their open
    /// sessions ([`mcp_server`]).
    mcp_server: mcp_server::McpServer,
    /// The task following where this host answers ([`hosts::Workspaces::follow_own_address`]),
    /// aborted with the host: it holds the endpoint's address watcher, which keeps the
    /// endpoint's socket open until it goes.
    own_address: std::sync::OnceLock<tokio::task::AbortHandle>,
    /// The device's record of every invocation it was asked for, when this host serves one:
    /// what a model call it answers writes what it cost into
    /// ([`Invocations::note_spend`](tilo_actions::remote::invocation_record::Invocations::note_spend)).
    invocations: std::sync::OnceLock<Arc<tilo_actions::remote::invocation_record::Invocations>>,
}

impl Drop for WorkspacesInner {
    fn drop(&mut self) {
        if let Some(following) = self.own_address.get() {
            following.abort();
        }
    }
}

/// One person's place in a hosted workspace, as installed bots compare it
/// between passes to see who joined, left, changed role or was banned.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberMark {
    /// `viewer`, `commenter`, `editor`, `moderator`, or `owner` for an
    /// administrator.
    pub role: String,
    /// They were removed (or left).
    pub removed: bool,
    /// They are kept out until unbanned.
    pub banned: bool,
}

/// The authority governing one workspace, as the workspace's governance refers to it.
#[derive(Clone)]
pub struct WorkspaceAuthority {
    workspaces: std::sync::Weak<WorkspacesInner>,
    workspace: String,
}

impl WorkspaceAuthority {
    /// The workspaces, while the server still hosts them.
    pub fn workspaces(&self) -> Option<Workspaces> {
        self.workspaces.upgrade().map(|inner| Workspaces { inner })
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

impl std::fmt::Debug for Workspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspaces")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

/// Whether `message` is a notice the host keeps already rather than a record write or
/// governance: what the server's own device sends.
fn only_a_notice(message: &neighbour_wire::NeighbourMsg) -> bool {
    message.governance_workspace().is_none()
        && !matches!(
            message,
            neighbour_wire::NeighbourMsg::AppDocument { .. }
                | neighbour_wire::NeighbourMsg::SyncResp { .. }
        )
}

impl Workspaces {
    /// The MCP server as the person reads it: every agent it let in, the reach each was let in
    /// for, and the sessions open now. A device's daemon publishes it to the apps attached to
    /// it as `mcp.server` ([`tilo_actions::remote::mcp_server`]).
    #[must_use]
    pub fn mcp_reading(&self) -> serde_json::Value {
        match serde_json::to_value(self.inner.mcp_server.server()) {
            Ok(reading) => reading,
            Err(error) => {
                tracing::warn!(%error, "the MCP server's reading does not encode");
                serde_json::Value::Null
            }
        }
    }

    /// Moves whenever the MCP server's reading may read differently.
    #[must_use]
    pub fn mcp_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.mcp_server.changes()
    }

    /// What the workspaces kept here hold, read and written in process: what
    /// a service the embedder runs beside them reads and writes through.
    #[must_use]
    pub fn documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.inner.records.clone()
    }

    /// The direct links of the workspaces hosted here, as the host's own
    /// device: a member device of a workspace syncs with it here, and catches
    /// up from the history the host keeps.
    #[must_use]
    pub fn direct(&self) -> tilo_peer_state::direct::Serving {
        self.inner.direct.clone()
    }

    /// Whether `device` holds a link to `workspace_id` here now: one the
    /// workspace's host admitted and has not refused. A device whose links the
    /// host ended stays among the link's remotes, so a later widening can open
    /// them again, but holds no link until then.
    #[must_use]
    pub fn linked(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        self.inner
            .link_remotes
            .lock_recover()
            .get(&workspace_id)
            .is_some_and(|remotes| remotes.contains(device))
            && self
                .inner
                .direct
                .link(workspace_id)
                .is_some_and(|link| link.ended_link(*device).is_none())
    }

    /// In-process access to the records of the workspaces this host keeps, for
    /// a service embedded in the host process.
    #[must_use]
    pub fn workspace_documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.inner.records.clone()
    }

    /// The address this device answers at, as its endpoint has it now.
    pub(super) fn endpoint_addr(&self) -> iroh_base::EndpointAddr {
        self.inner.host.endpoint.addr()
    }

    /// The relays this device holds back until something needs it reachable.
    pub(super) fn reachability(&self) -> &tilo_endpoint_reachability::Reachability {
        &self.inner.host.reachability
    }

    /// The device key this service answers as: what its own hints name, and what founds the
    /// workspaces it creates.
    #[must_use]
    pub fn host_device(&self) -> [u8; 32] {
        self.inner.host.cert.device_public_key
    }

    /// The `https://` origin this keeper's site answers at (`https://eu1.tilo.sh`), when it
    /// serves one: where the public addresses it mints for its members' pictures point. `None`
    /// on a device that serves no site, which serves no picture to the outside.
    #[must_use]
    pub fn public_origin(&self) -> Option<String> {
        self.inner.host.holder().origin
    }

    /// Make this keeper advertise its site at `origin`, as a deployment's
    /// [`crate::service::ServiceConfig::origin`] does, so the public addresses it mints for its
    /// members' pictures point there.
    #[cfg(test)]
    pub fn advertises_at(&self, origin: &str) {
        self.inner
            .host
            .holder
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .origin = Some(origin.to_owned());
    }

    /// Read `workspace`'s replica as it stands now, without waiting for its lock: how it is held
    /// here, and what `read` makes of it. `None` for a workspace not held here. What reads on a
    /// thread of its own reads here, as the device's key service reads who takes part in a group.
    pub fn read_now<R>(
        &self,
        workspace: &str,
        read: impl FnOnce(&StatePeer) -> R,
    ) -> Option<(Held, R)> {
        let reading = self.inner.readings.held(workspace)?;
        let held = if reading.member {
            Held::MemberCopy
        } else {
            Held::Hosts
        };
        Some((held, reading.replica.read(read)))
    }

    /// The trees files are put into where this service holds them: the one writer of a
    /// workspace's tree for a caller that has the content and no folder
    /// ([`crate::files::Puts`]).
    #[cfg(all(feature = "files", not(target_arch = "wasm32")))]
    #[must_use]
    pub fn puts(&self) -> Arc<crate::files::Puts> {
        Arc::clone(self.inner.puts.get_or_init(|| {
            Arc::new(crate::files::Puts::new(
                self.clone(),
                self.inner.host.access.state_root().join("files-puts"),
            ))
        }))
    }

    /// The channel keys of the device this host runs on, when its embedder
    /// runs that device's channel service beside it: the host asks it for the
    /// records of the workspaces it holds and keeps no keys of its own.
    #[must_use]
    pub fn device_keys(&self) -> Option<DeviceKeys> {
        self.inner
            .host
            .channels
            .as_ref()
            .map(|channels| Arc::clone(&channels.keys))
    }

    /// Every record this host holds of `workspace`, by the app whose document
    /// it is and its id: what its members' pages and agents may name there,
    /// whether or not a device's session has been sent it. A public workspace
    /// sends a device its records only when it opens one, so this is what a
    /// service listing the workspace's apps goes by. `None` for a workspace
    /// this host does not hold.
    ///
    /// Read from the replica without the workspace's lock ([`Workspaces::reading`]): the lock is
    /// held across each commit's writes, so under a long change of many files a listing waited
    /// seconds behind them, up to the half minute after which a device takes the server for gone.
    pub async fn records(&self, workspace: &str) -> Option<Vec<(String, String)>> {
        let reading = self.reading(WorkspaceId::parse(workspace)?).await?;
        let peer = reading.replica.lock();
        Some(peer.records())
    }

    /// Open every workspace recorded under `dir` and bind each to the server, pinned.
    #[expect(
        clippy::too_many_lines,
        reason = "opening a workspace walks every step of its state in order"
    )]
    pub(crate) async fn open(
        host: WorkspaceServices,
        dir: PathBuf,
        store: PathBuf,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        // A workspace a host made keeps the certificate it made it with: a host
        // renamed since serves with the record's own, verified.
        let host = record_authority::host_serving(host, &dir);
        let workspaces = Arc::new(device_actions::WorkspacesMap::default());
        // The device actions answer on the endpoint, so they sign as the key it is keyed by, and
        // hand whoever asks the certificate of that key.
        let (signer, answering) = host.endpoint_identity();
        let (action_service, jobs, answers) =
            device_actions::action_service(Arc::clone(&workspaces), signer);
        action_service.answer_under(answering);
        // The apps this host serves answer on the same action service its workspaces' devices
        // reach it on, beside the work and governance it answers itself. The workspace's accounts
        // and its vault are registered there too, for an app's own connection, which reaches
        // them through the broker ([`device_actions::APP_CONNECTION_PROVIDERS`]).
        if let Some(apps) = host.apps.clone() {
            let served =
                crate::app_serving::Served::new(&apps, Arc::clone(&workspaces), &action_service);
            answers.serve_apps(served.clone());
            for provider in device_actions::APP_CONNECTION_PROVIDERS {
                answers.work.providers.provide(
                    provider,
                    provider,
                    providers::Reach::Devices,
                    Arc::new(served.clone()),
                );
            }
        }
        let store =
            LinkStore::open(store, Appetite::StoreAll).map_err(|error| error.to_string())?;
        // What a host from before the device store kept in its own store of
        // changes moves into this one, once, before any workspace is served.
        if let Some(state_root) = dir.parent() {
            crate::history_move::move_into_store(state_root, &store)?;
        }
        let store = Arc::new(store);
        // The host's links admit the device at their other end before they
        // listen, by the one admission rule asked of the workspace's replica
        // ([`Workspaces::admit_link`]); the slot holds these workspaces once
        // they exist, the [`Serving`](tilo_peer_state::direct::Serving) being
        // built before them.
        let admitting = Arc::new(std::sync::OnceLock::new());
        let direct =
            tilo_peer_state::direct::Serving::new().admitting(Workspaces::admitting(&admitting));
        let records = crate::workspace_documents::WorkspaceDocuments::new(
            host.workspaces.clone(),
            Arc::clone(&store),
            direct.clone(),
            host.cert.device_public_key,
            host.cert.user_public_key,
            host.key.clone(),
        );
        #[cfg(not(target_arch = "wasm32"))]
        let models = Arc::new(model_answers::Models::new());
        let memberships = host.access.memberships();
        let this = Self {
            inner: Arc::new(WorkspacesInner {
                live: std::sync::OnceLock::new(),
                #[cfg(any(test, feature = "test-support"))]
                speaks_no_catch_up: std::sync::atomic::AtomicBool::new(false),
                #[cfg(any(test, feature = "test-support"))]
                cursors_per_answer: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
                icloud_calendars_at: std::sync::RwLock::new(None),
                #[cfg(any(test, feature = "test-support"))]
                cloud_sign_in: std::sync::RwLock::new(None),
                #[cfg(any(test, feature = "test-support"))]
                cloud_control: std::sync::RwLock::new(None),
                #[cfg(any(test, feature = "test-support"))]
                icloud_sign_in: std::sync::RwLock::new(None),
                draining: std::sync::atomic::AtomicBool::new(false),
                mode: std::sync::RwLock::new(None),
                store,
                direct,
                link_remotes: std::sync::Mutex::default(),
                listing_themselves: std::sync::Mutex::default(),
                agent_sessions: personal::AgentSessions::default(),
                records,
                member: member::Members::default(),
                executions: Arc::new(run_admission::ExecutionPins::new()),
                dir: dir.clone(),
                action_service,
                jobs,
                record_writes: std::sync::OnceLock::new(),
                writes_thread: std::sync::OnceLock::new(),
                applications: host.apps.clone(),
                workspaces,
                readings: readings::Readings::default(),
                once_claims: once_grants::OnceClaims::default(),
                own_opened: reads::OwnOpened::default(),
                #[cfg(not(target_arch = "wasm32"))]
                tilolang: std::sync::OnceLock::new(),
                creating: creating::Creating::default(),
                changes: tokio::sync::broadcast::channel(64).0,
                handing: std::sync::Mutex::default(),
                handing_on: std::sync::Mutex::default(),
                deletions: tokio::sync::broadcast::channel(64).0,
                branches: tokio::sync::broadcast::channel(64).0,
                branch_content: std::sync::RwLock::default(),
                asked_records: std::sync::OnceLock::new(),
                asked_store: std::sync::OnceLock::new(),
                asked_dispatch: std::sync::OnceLock::new(),
                #[cfg(not(target_arch = "wasm32"))]
                vault_asks: std::sync::OnceLock::new(),
                machines: std::sync::RwLock::default(),
                own_readings: std::sync::RwLock::default(),
                person_names: std::sync::RwLock::default(),
                web_ui: std::sync::RwLock::default(),
                document_writes: tokio::sync::broadcast::channel(256).0,
                person_settings_moved: tokio::sync::watch::channel(0).0,
                #[cfg(not(target_arch = "wasm32"))]
                secret_clipboard: std::sync::OnceLock::new(),
                #[cfg(not(target_arch = "wasm32"))]
                models: Arc::clone(&models),
                #[cfg(not(target_arch = "wasm32"))]
                vault_uses: hosted_use::Uses::proving_within(models.mail_reach()),
                #[cfg(not(target_arch = "wasm32"))]
                telling: person_telling::Telling::default(),
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                replays: std::sync::atomic::AtomicU64::default(),
                directory: listing::Directory::default(),
                host,
                entry: wait_list::EntrySettings::default(),
                releasing: AsyncMutex::new(()),
                own: std::sync::OnceLock::new(),
                #[cfg(all(feature = "files", not(target_arch = "wasm32")))]
                puts: std::sync::OnceLock::new(),
                memberships,
                owner_invitations: host_invitations::OwnerInvitations::open(&dir),
                device_names: std::sync::Mutex::default(),
                mcp_server: mcp_server::McpServer::default(),
                own_address: std::sync::OnceLock::new(),
                invocations: std::sync::OnceLock::new(),
            }),
        };
        if let Some(device) = this.inner.host.member.clone() {
            this.inner.member.set_device(device);
        }
        let _ = admitting.set(this.downgrade());
        this.inner.jobs.claim_in(this.downgrade());
        if let Some(sink) = this
            .inner
            .host
            .channels
            .as_ref()
            .and_then(|channels| channels.sink.as_ref())
        {
            sink.attach(&this);
        }
        let workspaces = Box::pin(this.open_kept(&dir)).await?;
        *this.inner.workspaces.lock().await = workspaces;
        let kept = this.served_kept().await;
        this.serve_kept(&kept).await?;
        // A page reading an invitation answers without a workspace's lock
        // from its first read on.
        for workspace in &kept {
            this.keep_reading(workspace).await;
        }
        this.serve_kept_members().await;
        this.start_following();
        // What its links admitted is re-checked whenever what admission reads
        // narrows: a device that no longer reaches the workspace is let go.
        this.watch_link_access();
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        {
            this.provide(profile_import::PROVIDER, profile_import::Import::of(&this));
            this.import_app_files_soon();
        }
        Ok(this)
    }

    /// Serve each workspace this host kept from its last run: bring it up,
    /// settle its governance, name this host's address in it, and recall what
    /// it left running.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each workspace stays locked through its whole start, up to its revocations"
    )]
    async fn serve_kept(
        &self,
        workspaces: &[Arc<AsyncMutex<HostedWorkspace>>],
    ) -> Result<(), String> {
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            self.bind(&workspace)
                .await
                .map_err(|e| format!("bind: {e}"))?;
            tracing::info!("bound");
            self.settle(&mut workspace)
                .await
                .map_err(|e| format!("settle: {e}"))?;
            tracing::info!("settled");
            // Where members reach the host is advice: a workspace whose
            // governance does not take this host's new address keeps the one
            // it names, and is served all the same.
            if let Err(error) = self.name_own_host(&mut workspace).await {
                tracing::error!(workspace = %workspace.workspace(), %error, "the host's address is not named in the workspace's governance");
            }
            self.recall(&mut workspace)
                .await
                .map_err(|e| format!("recall: {e}"))?;
            tracing::info!("recalled");
            // What the server's log revoked while the workspace was closed.
            self.carry_revocations(&mut workspace, &[]).await?;
            let workspace_id = workspace.workspace().to_owned();
            drop(workspace);
            // A record written where its type's rule does not place it moves into the document
            // its rule gives, so what it holds and its history reach its readers alone.
            if let Err(error) = self.replace_misplaced(&workspace_id).await {
                tracing::warn!(workspace = %workspace_id, %error, "a misplaced record was not moved");
            }
        }
        Ok(())
    }

    /// Follow what a host watches once its workspaces are open: their
    /// activity, the app surfaces their writes wait for, each record's
    /// repair, and the address it answers at, then whatever its hosted
    /// workspaces left running.
    fn start_following(&self) {
        self.follow_activity();
        self.follow_missing_surfaces();
        self.follow_own_address();
        // The keys the operator blocks are a record of their own workspace, read
        // again as it changes ([`blocked`]).
        self.follow_blocked();
        // The servers the person's document names hold the workspaces it
        // says they do, on every device of theirs that holds it.
        self.follow_servers();
        // A server of the person keeps every workspace of their list it is
        // not told to leave.
        self.follow_person_list();
    }

    /// Stop taking work under leases until this process ends: the server is
    /// about to restart, and every lease it lets lapse is taken by another
    /// holder of the workspace. Nothing undoes it; the restart does.
    pub fn drain(&self) {
        self.inner
            .draining
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether this server is draining ([`Workspaces::drain`]): whoever takes
    /// work under a lease here neither takes nor renews it.
    #[must_use]
    pub fn draining(&self) -> bool {
        self.inner
            .draining
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Hold the operator's kill switch: from now on the entry admits nobody new while the mode
    /// pauses joins ([`mode::Mode::allows_joins`]), and the people already in work on.
    pub fn hold_mode(&self, held: Arc<mode::HostMode>) {
        *self
            .inner
            .mode
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(held);
    }

    /// The mode the host admits under now: what its operator set, or normal while it set none.
    #[must_use]
    pub fn mode(&self) -> mode::Mode {
        let held = self
            .inner
            .mode
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        held.map_or_else(mode::Mode::default, |held| held.current())
    }

    /// Stop hosting and serving `workspace`: its workspace admits nobody from
    /// now on, which ends its sessions here, and the host leaves it closed
    /// when it opens again. Its files stay on disk. A device releases a
    /// workspace after moving it to another host, whose hints its members follow,
    /// and a server lets go of one its entry no longer keeps
    /// ([`servers::keeping`]); a member copy stops syncing.
    ///
    /// What this device pushed of the workspace's content and its hosts did
    /// not say they keep yet goes first: the release waits for it, and is
    /// refused, naming how many files are still going, when it does not
    /// arrive in time. Leaving would take the only copy of them along.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, files of it are still on this device
    /// alone, or the release could not be recorded or its access narrowed.
    pub async fn release(&self, workspace: &str) -> Result<(), String> {
        let going = self
            .content_delivered(workspace, hosts::CONTENT_SETTLES_WITHIN)
            .await;
        if going > 0 {
            return Err(format!(
                "{going} files are still going to the workspace's server: this device keeps \
                 the only copy of them until the server says it keeps them"
            ));
        }
        let _releasing = self.inner.releasing.lock().await;
        let hosted = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this host does not hold that workspace")?;
        let mut hosted = hosted.lock().await;
        // Recorded first: a host stopped halfway keeps it released.
        let marker = hosted.path.with_extension(RELEASED);
        tilo_files::native::atomic::write(&marker, b"").map_err(|error| error.to_string())?;
        self.inner.workspaces.lock().await.remove(workspace);
        self.inner.readings.forget(workspace);
        self.inner.direct.release(hosted.workspace_id());
        // A member copy stops syncing with the device serving it.
        self.let_go_member(hosted.workspace_id());
        // Its sessions to the other holders end with it.
        drop(std::mem::take(&mut hosted.holders));
        // Nobody lists a workspace this host serves no more.
        self.inner.memberships.forget(workspace);
        self.stop_serving(hosted.workspace_id()).await;
        tracing::info!(%workspace, workspace_id = %hosted.workspace_id(), "hosted workspace released");
        drop(hosted);
        let _ = self.inner.changes.send(workspace.to_owned());
        // What this device decides for others no longer covers the workspace: whoever follows
        // that, the Files mounts among them, is woken to make what it enforces again. A build
        // without the device's keys decides nothing for others, so it has nobody to wake.
        #[cfg(feature = "device-keys")]
        tilo_identity::policy_change::announce();
        Ok(())
    }

    /// Stop serving the workspace `workspace_id` here: nobody is admitted to it
    /// any more, and then what this host served of it goes, so nothing a
    /// session still carried opens it again. Its files stay for whenever it
    /// is held here again.
    ///
    /// # Errors
    ///
    /// Its access could not be narrowed.
    pub(super) async fn stop_serving(&self, workspace_id: WorkspaceId) {
        self.inner.host.access.stop(workspace_id).await;
        crate::workspace_io::evict(&self.inner.host.workspaces, workspace_id).await;
    }

    /// Every workspace kept here, served from here or taken part in.
    pub async fn ids(&self) -> Vec<WorkspaceId> {
        self.inner
            .workspaces
            .lock()
            .await
            .keys()
            .filter_map(|workspace| WorkspaceId::parse(workspace))
            .collect()
    }

    /// Every workspace this server hosts.
    pub async fn list(&self) -> Vec<WorkspaceSummary> {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut summaries = Vec::with_capacity(workspaces.len());
        for workspace in workspaces {
            let workspace = workspace.lock().await;
            summaries.push(self.people(&workspace));
        }
        summaries
    }

    /// The delegated agents on `workspace`'s roster, acting or not now.    ///
    /// # Errors
    ///
    /// Returns why the workspace's roster could not be read.
    pub async fn agents(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::DelegatedAgent>, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let agents = workspace
            .lock()
            .await
            .peer()
            .governance()
            .delegated_agents(tilo_clock::wall());
        agents.map_err(CollaborationError::new)
    }

    /// The person this server's device belongs to: who it runs the workspace's hosted agents
    /// as, and who it counts what they spend as.
    #[must_use]
    pub fn device_person(&self) -> [u8; 32] {
        self.inner.host.cert.user_public_key
    }

    /// This server's own device: what it signs and serves the workspaces here as.
    #[must_use]
    pub fn device_cert(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// Whether this server's own person administers `hosted`: its signed member list makes
    /// them its owner or one of its administrators. The server writes what a deployment asks
    /// for as that administrator, and the list decides it again on every device, so what only
    /// its owner may change stays theirs, and an owner who takes the grant back is obeyed.
    #[must_use]
    fn administers(&self, hosted: &HostedWorkspace) -> bool {
        let host = hex::encode(self.inner.host.cert.user_public_key);
        hosted
            .peer()
            .governance()
            .member_policy()
            .is_ok_and(|policy| policy.is_owner(&host) || policy.is_administrator(&host))
    }

    /// Whether the server decides `device`'s changes in `workspace_id` by what the member list
    /// gives it now. A workspace not opened here yet is settled by the bind that opens it, so it
    /// counts as deciding them.
    async fn decides_for(&self, workspace_id: WorkspaceId, device: [u8; 32]) -> bool {
        let runtime = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned();
        let Some(runtime) = runtime else {
            return true;
        };
        runtime
            .lock()
            .await
            .device_access
            .as_ref()
            .is_none_or(|device_access| device_access.contains_key(&device))
    }

    /// Whether this server's own person administers `workspace` here
    /// ([`Self::administers`]); `false` for a workspace not held here.
    pub async fn administers_workspace(&self, workspace: &str) -> bool {
        let Ok(hosted) = self.workspace(workspace).await else {
            return false;
        };
        let hosted = hosted.lock().await;
        self.administers(&hosted)
    }

    /// Whether this device's own person writes in `workspace` by its signed member list: their
    /// row there is a role that writes records (an owner's, a moderator's or an editor's).
    pub async fn own_person_writes(&self, workspace: &str) -> bool {
        let Ok(hosted) = self.workspace(workspace).await else {
            return false;
        };
        let person = hex::encode(self.inner.host.cert.user_public_key);
        hosted
            .lock()
            .await
            .peer()
            .governance()
            .member_policy()
            .is_ok_and(|policy| {
                matches!(
                    policy.active_role(&person),
                    Some(
                        tilo_action_policy::Role::Owner
                            | tilo_action_policy::Role::Moderator
                            | tilo_action_policy::Role::Editor
                    )
                )
            })
    }

    /// What answers the workspace-join exchange for these workspaces.
    #[must_use]
    pub fn join_service(&self) -> tilo_peer_state::join_answer::JoinAnswer<Self> {
        tilo_peer_state::join_answer::JoinAnswer(self.clone())
    }

    /// The action service the host answers its workspaces' member devices on, as its
    /// own device: what another device of a workspace asks it where no member
    /// session runs in between ([`device_actions`]).
    #[must_use]
    pub fn action_service(&self) -> tilo_peer_state::actions::ActionService {
        self.inner.action_service.clone()
    }

    /// Where this host hands a request to a device of its workspaces, over the
    /// link that device holds to it ([`crate::app_serving::Hand`]).
    #[must_use]
    pub fn hand(&self) -> crate::app_serving::Hand {
        crate::app_serving::Hand::new(self.action_service(), self.inner.applications.clone())
    }

    /// Take the turns of work this device may for its workspaces' devices over
    /// the content `content` says it keeps ([`crate::work`]): whether a turn is
    /// this device's is [`takes_turn`](crate::work::takes_turn)'s to say, live,
    /// each time.
    pub fn run_jobs(&self, content: Arc<dyn crate::work::KeepsContent>) {
        self.inner.jobs.serve(content);
    }

    /// Take the Linux turns of this device's workspaces' devices on `linux`
    /// ([`crate::work::Turn::Linux`]): a workspace's shell commands, terminals,
    /// machine and Browser.
    pub fn run_linux(&self, linux: Arc<dyn crate::work::HostLinux>) {
        self.inner.jobs.serve_linux(linux);
    }

    /// Answer the calls that name `provider` with `answers`: another device's over the device
    /// action service (`tilo-actions/1`), admitted by the workspace's grants for that device under
    /// the application `provider`, and this device's own through its dispatch, admitted by the
    /// same grants ([`providers`]). `answers` decides no grant itself.
    pub fn provide(
        &self,
        provider: &'static str,
        answers: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    ) {
        self.inner
            .jobs
            .providers
            .provide(provider, provider, providers::Reach::Devices, answers);
    }

    /// Answer the reads of `app` with `reads` from now on: a resource of this device's own (where
    /// its folders stand, the styles its packages offer), read in the person's own workspace by
    /// this device's own person and, as `readers` says, by whoever else that workspace lets read
    /// it ([`record_reads::RecordReads`]).
    pub fn publish_own(
        &self,
        app: &'static str,
        readers: OwnReaders,
        reads: Arc<dyn tilo_actions::remote::ResourceReads>,
    ) {
        self.inner
            .own_readings
            .write_recover()
            .insert(app, (readers, reads));
    }

    /// What answers the reads of `app`, when it is a resource of this device's own, and who reads
    /// it.
    pub(crate) fn own_reading(
        &self,
        app: &str,
    ) -> Option<(OwnReaders, Arc<dyn tilo_actions::remote::ResourceReads>)> {
        self.inner.own_readings.read_recover().get(app).cloned()
    }

    /// The providers registered on this device ([`Self::provide`]): the one registry this device's
    /// dispatch and its action service reach them in.
    #[must_use]
    pub fn providers(&self) -> providers::Providers {
        self.inner.jobs.providers.clone()
    }

    /// Hand this service the language runtime the daemon's program mounted
    /// ([`crate::workspaces::WorkspaceServices`]): from now on its own roles read and change
    /// the records of the tilolang apps built into the build, offer the styles the device's
    /// installed packages declare, and write rich text through it. Called once, as the service
    /// is configured.
    ///
    /// A built-in tilolang app's record is written with no package installed, so its types and
    /// verbs are named here, as an installed app's surface names them: the store judges its
    /// writes by them from the moment the runtime is mounted.
    ///
    /// # Errors
    /// A built-in app's surface does not compile: the service cannot judge its records.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn provide_tilolang(&self, tilolang: crate::tl_records::Tilolang) -> Result<(), String> {
        for (app, bytes) in tilolang.records.surfaces() {
            keep_surface(&self.inner.host.surfaces, &app, &bytes)?;
        }
        let _ = self.inner.tilolang.set(tilolang);
        Ok(())
    }

    /// The language runtime this service's program mounted, where one did.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn tilolang(&self) -> Option<crate::tl_records::Tilolang> {
        self.inner.tilolang.get().cloned()
    }

    /// Answer the workspaces' governance actions their member devices ask on the device action
    /// service through `governance`.
    pub(crate) fn govern_device_actions_with(&self, governance: std::sync::Weak<actions::Shared>) {
        self.inner.jobs.govern_with(governance);
    }

    /// What this device answers a device of its workspaces on the device action service: the
    /// turns of work it takes, and the workspaces' governance. A device whose session answers that
    /// service in the host's place hands these requests here.
    #[must_use]
    pub fn device_actions(&self) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
        device_actions::Answers::over(Arc::clone(&self.inner.jobs))
    }

    /// Workspace UUIDs, each sent after a governance decision on that workspace was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// What moves whenever a workspace is held here that was not before: what a follower of what
    /// the workspaces name reads again on, a device that enters its person's workspace after it
    /// started among them.
    #[must_use]
    pub fn holds_changed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.readings.holds()
    }

    /// Each write to a record a workspace held here took in: a member's write to
    /// a workspace hosted here, and every write the device serving a
    /// member copy here sends it, and this device's own writes into a member
    /// copy. A slow receiver may lag, and then reads again every record it
    /// follows: a write it missed is in the copy, never told again.
    #[must_use]
    pub fn document_writes(&self) -> tokio::sync::broadcast::Receiver<ResourceWrite> {
        self.inner.document_writes.subscribe()
    }

    /// Told whenever a write this device made to a member copy is noted, answered or refused.
    #[must_use]
    pub fn member_writes_answered(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.member.answers_changed()
    }

    /// `app`'s record `record` of `workspace` as the workspace's store holds it, saved; a
    /// document the store placed ([`collab_types::is_store_document`]) by its own id. A document
    /// the workspace keeps outside its store, which the device's own roles write (the desktop's
    /// layout), reads as this device reads it ([`Self::own_read`]). `None` for a record held in
    /// neither.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, the record's document does not read, or it is still
    /// arriving.
    pub async fn resource_document(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        if collab_types::is_store_document(record) {
            let state = self.workspace(workspace).await?;
            return Ok(state.lock().await.store_document(record));
        }
        // A record of a kind its app declares is the store's.
        if let Some(stored) = self
            .stored_record_in(workspace, app, record)
            .await
            .map_err(CollaborationError::new)?
        {
            return Ok(Some(stored));
        }
        match self
            .own_read(workspace, (app, record), &[], usize::MAX)
            .await
            .map_err(CollaborationError::new)?
        {
            tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. } => {
                Ok(Some(bytes))
            }
            tilo_peer_state::workspace_documents::DocumentRead::Absent => Ok(None),
            _ => Err(CollaborationError::new(format!(
                "{record} of {app} is still arriving"
            ))),
        }
    }

    /// Whether `workspace`'s store holds the resource `id` of `kind` now: false for a workspace
    /// not kept here.
    pub async fn store_holds(&self, workspace: &str, kind: &str, id: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        state.lock().await.holds_resource(kind, id)
    }

    /// What the change `hash` of the resource store's document `doc` of `workspace` wrote: each
    /// resource, with what it holds as the change left it ([`tilo_resource_store::Store::wrote`]).
    /// None for a workspace not kept here.
    pub async fn store_wrote(
        &self,
        workspace: &str,
        doc: &str,
        hash: automerge::ChangeHash,
    ) -> Vec<tilo_resource_store::Wrote> {
        let Ok(state) = self.workspace(workspace).await else {
            return Vec::new();
        };
        state.lock().await.store_wrote(doc, hash)
    }

    /// Every resource of `workspace`'s store that `query` finds, by id, with what the store's
    /// index holds of it: the records this device holds, read for its own work (the agent
    /// engine's look round, a wait an automation ends).
    ///
    /// # Errors
    /// The workspace is not kept here: what is not read is never taken for gone.
    pub async fn store_found(
        &self,
        workspace: &str,
        query: collab_types::store::Query,
    ) -> Result<Vec<(String, collab_types::store::Indexed)>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let found = state.lock().await.store_found(query);
        Ok(found)
    }

    /// The resource `id` of `kind` in `workspace`'s store, as the store's index holds it; none
    /// where this device holds no such resource.
    pub async fn store_held(
        &self,
        workspace: &str,
        kind: &str,
        id: &str,
    ) -> Option<collab_types::store::Indexed> {
        let state = self.workspace(workspace).await.ok()?;
        state.lock().await.store_held(kind, id)
    }

    /// The `(app, record)` of every record of `workspace` the host's copy
    /// holds readable: in the clear, or sealed under channels this server
    /// takes part in ([`Self::resource_document`] opens those).
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn document_routes(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let hosted = state.lock().await;
        let mut routes = hosted.peer().plaintext_routes();
        if hosted.seals() {
            routes.extend(hosted.peer().sealed_routes());
        }
        drop(hosted);
        Ok(routes)
    }

    /// [`Self::document_routes`] but each person's private records
    /// ([`tilo_action_policy::roles::is_private_record`]).
    ///
    /// # Errors
    /// As [`Self::document_routes`].
    pub async fn workspace_document_routes(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let mut hosted = state.lock().await;
        let mut routes = hosted.peer().plaintext_routes();
        if hosted.seals() {
            routes.extend(hosted.peer().sealed_routes());
        }
        // The records the workspace's store keeps, by their app and id.
        {
            let store = self.store_of(&mut hosted);
            for (app, kind) in self.inner.host.surfaces.kinds() {
                routes.extend(store.ids_of(&kind).map(|id| (app.clone(), id.to_owned())));
            }
        }
        routes.retain(|(app, record)| !tilo_action_policy::roles::is_private_record(app, record));
        drop(hosted);
        Ok(routes)
    }

    /// The state of `workspace` as this host holds it now: what a channel
    /// service reads who may read the workspace's channels from.
    pub async fn state(&self, workspace: &str) -> Option<StatePeer> {
        let hosted = self.workspace(workspace).await.ok()?;
        let state = hosted.lock().await.peer().clone();
        Some(state)
    }

    /// `workspace` as governance records it now.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn summary(&self, workspace: &str) -> Result<WorkspaceSummary, CollaborationError> {
        let hosted = self.workspace(workspace).await?;
        let hosted = hosted.lock().await;
        Ok(self.people(&hosted))
    }

    /// `hosted`'s people as every list showing them has them: the ones its
    /// governance has, its owner among them — without this host's own person
    /// where it runs workspaces of people's own. There the host runs for
    /// everyone and is no person of theirs, though its device founds each
    /// workspace and so is the owner its policy names.
    fn people(&self, hosted: &HostedWorkspace) -> WorkspaceSummary {
        let mut summary = hosted.summary();
        if self.own().is_some() {
            let host = hex::encode(self.inner.host.cert.user_public_key);
            summary
                .members
                .retain(|member| member.id.person_id() != Some(host.as_str()));
            summary
                .devices
                .retain(|id, _| id.person_id() != Some(host.as_str()));
        }
        summary
    }

    /// `workspace` as a listing names it: its name and generation, read
    /// without its people.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn listing_of(
        &self,
        workspace: &str,
    ) -> Result<WorkspaceListing, CollaborationError> {
        Ok(self.workspace(workspace).await?.lock().await.listing())
    }

    /// Every person `workspace` still keeps, by person (hex root): its members that
    /// nothing removed, the server's own person among them. What the server decides who
    /// acts here by, an agent through the member it acts for, never what a listing shows: that
    /// leaves the server's own account out ([`Self::membership`]).
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn kept_people(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeSet<String>, CollaborationError> {
        Ok(self
            .kept_membership(workspace)
            .await?
            .1
            .into_iter()
            .filter(|(_, mark)| !mark.removed)
            .map(|(person, _)| person)
            .collect())
    }

    /// Every person `workspace`'s governance has, by person (hex root), with the
    /// workspace's governance generation: its owner among them, also where the owner is
    /// this host's own person. What the server decides who acts here by, an agent through
    /// the person it acts for, never what [`Self::membership`] shows a person: that leaves the
    /// server's own account out.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn kept_membership(
        &self,
        workspace: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = tilo_clock::wall();
        let state = self.workspace(workspace).await?;
        let hosted = state.lock().await;
        let reader = self.own_reader();
        let policy = hosted.peer().permissions(reader, now)?.policy;
        let people = hosted.summary();
        drop(hosted);
        Ok((
            people.generation,
            Self::marks(people, |person| policy.is_blocked(person)),
        ))
    }

    /// Every person `workspace` has, as [`Self::people`] lists them, by person
    /// (hex root), with the workspace's governance generation: its owner among
    /// them, also where the owner is this host's own person.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here.
    pub async fn membership(
        &self,
        workspace: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = tilo_clock::wall();
        let state = self.workspace(workspace).await?;
        let hosted = state.lock().await;
        let reader = self.own_reader();
        let policy = hosted.peer().permissions(reader, now)?.policy;
        let people = self.people(&hosted);
        drop(hosted);
        Ok((
            people.generation,
            Self::marks(people, |person| policy.is_blocked(person)),
        ))
    }

    /// `people` as [`Self::membership`] and [`Self::kept_membership`] answer them: each
    /// person's role, whether they were removed, and whether the host's policy keeps them
    /// out now (`banned`).
    fn marks(
        people: WorkspaceSummary,
        banned: impl Fn(&str) -> bool,
    ) -> std::collections::BTreeMap<String, MemberMark> {
        people
            .members
            .into_iter()
            .map(|member| {
                let person = member.id.person_id().unwrap_or_default().to_owned();
                let role = match member.role {
                    Some(_) if member.administrator => "owner",
                    Some(role) => role.key(),
                    None => "",
                };
                let mark = MemberMark {
                    role: role.to_owned(),
                    removed: member.removed,
                    banned: banned(&person),
                };
                (person, mark)
            })
            .collect()
    }

    /// Carry this device's writes of a record on `runtime` from now on
    /// ([`WorkspacesInner::record_writes`]), the runtime of the service's writes thread, which
    /// `thread` drives and nothing else does. The first embedder to name one decides it.
    pub(crate) fn carry_writes_on(
        &self,
        runtime: tokio::runtime::Handle,
        thread: std::thread::ThreadId,
    ) {
        let _ = self.inner.record_writes.set(Some(runtime));
        let _ = self.inner.writes_thread.set(thread);
    }

    /// The runtime a record's writes are carried on: the one the embedder named, else the
    /// reactor that asks.
    #[must_use]
    pub(crate) fn record_writes_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.inner
            .record_writes
            .get()
            .cloned()
            .flatten()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
    }

    /// Carry `work`, this device's write of a record, on the runtime the service named for its
    /// writes ([`WorkspacesInner::record_writes`]) and answer what it answered. A write waits on
    /// the key service or a link, and the reactor that asked for it may be the very thread that
    /// has to run what it awaits — the daemon answering the call this write belongs to: on the
    /// writes thread the write never needs the thread that asked. Where the service named none,
    /// its reactor is all there is, and `work` is awaited where it is.
    ///
    /// # Errors
    /// `work`'s refusal, or the service stopping before it answered.
    pub(crate) async fn carry_write<T: Send + 'static>(
        &self,
        work: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, String> {
        match self.inner.record_writes.get().cloned().flatten() {
            Some(runtime) => runtime
                .spawn(work)
                .await
                .map_err(|_| "the workspace service stopped before it answered".to_owned())?,
            None => work.await,
        }
    /// The runtime the service named for its writes thread, where it named one: the thread
    /// nothing else drives ([`WorkspacesInner::record_writes`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn named_writes_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.inner.record_writes.get().cloned().flatten()
    }

    /// Wait for `future` on the service's writes thread where the service named one, and on a
    /// thread of its own otherwise: what a write made from a synchronous caller waits with.
    ///
    /// What the write starts with [`tokio::spawn`] runs on a runtime whose thread waits on
    /// nothing else, never on a thread of the caller's own runtime, which cannot run it while it
    /// is blocked here.
    ///
    /// # Panics
    ///
    /// The service's writes thread stopped while `future` ran: the service is being dropped, and
    /// the caller's work cannot be answered.
    #[cfg(not(target_arch = "wasm32"))]
    #[expect(
        clippy::expect_used,
        reason = "the writes thread stops only as the service is dropped, which ends its callers"
    )]
    pub(crate) fn on_its_own<F>(&self, future: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let Some(runtime) = self.named_writes_runtime() else {
            return crate::documents::on_its_own(future);
        };
        if self.inner.writes_thread.get() == Some(&std::thread::current().id()) {
            // Already on the writes thread: waiting on it here would keep it from running what
            // the future starts, so the thread waits for the future itself.
            return futures::executor::block_on(future);
        }
        let (done, answer) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _ = done.send(future.await);
        });
        futures::executor::block_on(answer)
            .expect("the service's writes thread stopped while it ran a caller's work")
    }

    /// Wait for `future` with this thread: a page drives no runtime beside it.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn on_its_own<F>(&self, future: F) -> F::Output
    where
        F: std::future::Future,
    {
        futures::executor::block_on(future)
    }

    async fn workspace(
        &self,
        workspace: &str,
    ) -> Result<Arc<AsyncMutex<HostedWorkspace>>, CollaborationError> {
        self.inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or_else(|| CollaborationError::new("this server does not hold that workspace"))
    }

    /// Hold the workspace, admitting only the members its signed list names,
    /// and follow its members' writes: create the records they start and keep the
    /// host's copy of the others current.
    ///
    /// Members' changes stay retained as signed tails: members write directly,
    /// not through the host, so the owner-baseline selection that retires
    /// a native owner's covered tails does not apply.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace is configured under one lock so no subscriber sees it half bound"
    )]
    async fn bind(&self, hosted: &HostedWorkspace) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace = hosted.workspace().to_owned();
        let workspace_id = hosted.workspace_id();
        let runtime_state = crate::workspace_io::get_or_initialize(
            &host.workspaces,
            host.access.state_root().to_path_buf(),
            workspace_id,
            crate::registry::MAX_HELD_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = runtime_state.lock().await;
        runtime.workspace_authority = Some(WorkspaceAuthority {
            workspaces: Arc::downgrade(&self.inner),
            workspace: workspace.clone(),
        });
        if !runtime.followed {
            runtime.followed = true;
            self.follow(workspace);
        }
        Ok(())
    }

    /// Replay what the device store keeps each time an app surface arrives:
    /// the writes held for it, those let go past the held queue's bound
    /// included, are taken in then rather than when the next write lands.
    fn follow(&self, workspace: String) {
        let workspaces = Arc::downgrade(&self.inner);
        let mut arrivals = crate::surfaces::arrivals();
        arrivals.mark_unchanged();
        tokio::spawn(async move {
            while arrivals.changed().await.is_ok() {
                let Some(inner) = workspaces.upgrade() else {
                    break;
                };
                (Self { inner }).replay_soon(workspace.clone());
            }
        });
    }

    /// Replay what the server retains for `workspace` into the host's
    /// copy now: [`Self::replay_soon`] in the caller's own time.
    async fn replay(&self, workspace: &str) {
        self.inner
            .replays
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let workspace_id = state.lock().await.workspace_id();
        let retained = match self.inner.records.retained_settled(workspace_id).await {
            Ok(retained) => retained,
            Err(error) => {
                tracing::warn!(%workspace, %error, "hosted workspace did not replay what its members wrote");
                return;
            }
        };
        #[cfg(feature = "device-keys")]
        self.found_retained(workspace, &state, &retained).await;
        let mut hosted = state.lock().await;
        self.recall_retained(&mut hosted, retained).await;
    }

    /// Ask the key holders of `app`'s `record`'s channel in `workspace` to
    /// admit this host's device ([`Self::join_channel`]), apart from whatever
    /// asked for it: the group's answer arrives as another write of the
    /// workspace, which waits for the writes ahead of it, a notice in this
    /// one's queue included. Once it is admitted, what the server retains of
    /// the record is replayed ([`Self::replay_soon`]), so its copy starts
    /// from its first write. Asked once per record until the first answer.
    #[cfg(feature = "device-keys")]
    fn join_soon(&self, workspace: &mut HostedWorkspace, app: String, record: String) {
        // A store of the workspace's ciphertext is never let into its group.
        if self.holds_as_store(workspace) {
            return;
        }
        if !workspace.asking.insert((app.clone(), record.clone())) {
            return;
        }
        let name = workspace.workspace().to_owned();
        let workspaces = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let Some(inner) = workspaces.upgrade() else {
                return;
            };
            let workspaces = Self { inner };
            // Still asking until what the channel's keys open is replayed:
            // whoever waits for them ([`Self::waits_for_keys`]) reads the
            // record once that is in the host's copy.
            if workspaces.join_keys(&name).await {
                workspaces.replay(&name).await;
            }
            let Ok(workspace) = workspaces.workspace(&name).await else {
                return;
            };
            workspace.lock().await.asking.remove(&(app, record));
        });
    }

    /// Ask to be let into the channel that seals `record` of `app` in
    /// `workspace`, when this host takes part in the workspace's channels:
    /// what a host that reads a record its members write sealed asks for once
    /// it finds the record shut, since its keys arrive only after the
    /// channel's key holders admit it ([`Self::join_soon`]). Nothing when
    /// the workspace is not hosted here; the keys arrive on the channel
    /// service's own time, and the record reads once they have.
    pub async fn ask_to_open(&self, workspace: &str, app: &str, record: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let mut hosted = state.lock().await;
        #[cfg(feature = "device-keys")]
        self.join_soon(&mut hosted, app.to_owned(), record.to_owned());
        #[cfg(not(feature = "device-keys"))]
        {
            let _ = (&mut hosted, app, record);
        }
    }

    /// Whether this host waits to be let into the channel that seals `record`
    /// of `app` in `workspace`: it runs on a device that takes part in the
    /// workspace's channels, and holds none of that channel's keys yet. Once
    /// it holds them, every write of the record reaches it readable and is
    /// followed as it lands, so nothing about the record is worth asking
    /// again. It waits while it is let in and replays what the keys open.
    /// `false` for a workspace not hosted here.
    pub async fn waits_for_keys(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let hosted = state.lock().await;
        #[cfg(feature = "device-keys")]
        let waits = hosted.seals()
            && (hosted.asking.contains(&(app.to_owned(), record.to_owned()))
                || !hosted.holds_keys().await);
        #[cfg(not(feature = "device-keys"))]
        let waits = {
            let _ = (&hosted, app, record);
            false
        };
        drop(hosted);
        waits
    }

    /// Take in `message` if it is a notice rather than a record write: an
    /// access request, a channel commit, the revocations an identity hello
    /// carries, or the workspace's topology. Hands a record write back.
    async fn absorb_notice(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        carried: Carried,
    ) -> Result<Option<neighbour_wire::NeighbourMsg>, String> {
        if message.governance_workspace().is_some() {
            self.take_governance(workspace, author, message, carried)
                .await?;
            return Ok(None);
        }
        match message {
            // The transport authenticated the author, but the entries are
            // the roots' own signatures: who carries them does not matter.
            neighbour_wire::NeighbourMsg::IdentityHello { revocations, .. } => {
                self.take_revocations(revocations).await
            }
            neighbour_wire::NeighbourMsg::SyncResp {
                workspace: ref named,
                ..
            } if named == workspace => self.take_members_change(workspace, author, message).await,
            // A resource's document is the store's, which hands any other write back.
            _ => return self.absorb_resources(workspace, author, message).await,
        }
        .map(|()| None)
    }

    /// Take in the record write `body` of `author`, which the device store
    /// keeps: it came over the workspace's link, or is replayed from the
    /// store ([`Self::absorb_carried`]).
    async fn absorb(&self, workspace: &str, author: [u8; 32], body: &[u8]) -> Result<(), String> {
        self.absorb_carried(workspace, author, body, Carried::Kept)
            .await
    }

    /// How many times this device's workspaces replayed what the store keeps
    /// for them: once each time an app surface arrives or a write let go can
    /// be taken in, so a count that keeps growing is a replay arming itself.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn replays(&self) -> u64 {
        self.inner
            .replays
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// [`Self::absorb`] for a test that hands a member's device's write to the host as the
    /// workspace's link does.
    ///
    /// # Errors
    /// As [`Self::absorb`].
    #[cfg(feature = "test-support")]
    pub async fn absorb_member_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        self.absorb(workspace, author, body).await
    }

    /// Take in the record write `body` of `author`. A record the workspace lacks
    /// is created from it, if `author` is a device of an editor. A write to a
    /// record the workspace has updates this host's copy, written through to
    /// the workspace file soon after ([`Self::keep_soon`]).
    /// Note the surface a change's op names when this host does not keep it: the change is
    /// decided against the surface its op names, the one this host runs the app with or one the
    /// workspace keeps for it ([`crate::surfaces`]), and what it lacks it asks the workspace's
    /// members for, holding the change until one answers.
    fn note_named_surface(&self, workspace: &str, app_id: &str, surface: Option<&[u8; 32]>) {
        if let Some(surface) = surface
            && crate::surfaces::kept(app_id, &hex::encode(surface)).is_none()
        {
            self.note_missing_surface(workspace, app_id, &hex::encode(surface));
        }
    }

    /// The message `body` of `author` carries, or `None` when nothing of it is taken in here: a
    /// notice this host's own device made itself, or one handed to the host's notice service.
    ///
    /// Every member's record write reaches the host here, so this is where one that does not
    /// decode is reported (the content classifier reads the same body before it leaves it to
    /// this). The server's own device writes records through the workspace too: the actions it
    /// runs for members (a visitor linking their Discord account) are committed as that device,
    /// and the host's copy, which the workspace's bots read, must take them in like any
    /// member's. Its notices it made itself, so it has them already. A host told of its device's
    /// channel service retains its own device's channel governance like any member's, for the
    /// other key holders: the host keeps no channels to have it already.
    ///
    /// # Errors
    /// The body does not decode, or the notice it carries is not taken in.
    async fn carried_message(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
        carried: Carried,
        own: [u8; 32],
    ) -> Result<Option<neighbour_wire::NeighbourMsg>, String> {
        let message = neighbour_wire::NeighbourMsg::decode(body)
            .map_err(|error| format!("a member's write does not decode: {error}"))?;
        if author == own && only_a_notice(&message) {
            return Ok(None);
        }
        self.absorb_notice(workspace, author, message, carried)
            .await
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    async fn absorb_carried(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
        carried: Carried,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let Some(message) = self
            .carried_message(
                workspace,
                author,
                body,
                carried,
                host.cert.device_public_key,
            )
            .await?
        else {
            return Ok(());
        };
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = &message
        else {
            return Ok(());
        };
        let (app_id, document_id) = (app_id.clone(), document_id.clone());
        self.note_named_surface(workspace, &app_id, envelope.surface.as_ref());
        let workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let waiting = crate::timing::start();
        let mut workspace = workspace.lock().await;
        crate::timing::window("workspace.follow_wait", waiting);
        if workspace.peer().holds_document(&app_id, &document_id) {
            let refused = self
                .follow_write(&mut workspace, author, message, (app_id, document_id), true)
                .await;
            // This host's own write is answered to whoever made it: a change the workspace
            // refuses here is an error of the write, not a verdict to file for another device.
            // A member's reaches it as their driver's answer, filed where they read it.
            return match refused {
                Some(reason) if author == host.cert.device_public_key => Err(reason.to_string()),
                _ => Ok(()),
            };
        }
        let sealed = envelope.encryption.is_some();
        // Only the record's channel members read a sealed write: a record that
        // starts sealed, as each new part of a chained record does, is followed
        // like any other where this host takes part in its channel. One it may
        // read and holds no keys of yet is joined, and the write is taken in
        // when the group admits this host's device: the record's copy starts
        // from its first write, so it is not kept by not asking.
        #[cfg(feature = "device-keys")]
        let readable = !sealed || workspace.holds_keys().await;
        #[cfg(not(feature = "device-keys"))]
        let readable = !sealed;
        if !readable {
            #[cfg(feature = "device-keys")]
            self.join_soon(&mut workspace, app_id, document_id);
            return Ok(());
        }
        let now = tilo_clock::wall();
        workspace.may_start(author, (&app_id, &document_id), now)?;
        // A record starts from the write that founds it, as its writer's device sent it: this
        // host neither writes a start of its own before it nor folds it into a copy it signs.
        // A record this host's own device founds starts from the write itself: that write is the
        // whole record, and the coverage row that covers the workspace's checkpoints is
        // published with it, as a snapshot this device publishes itself has it
        // ([`StatePeer::cover_own_checkpoint`]).
        let own = author == host.cert.device_public_key;
        workspace.change(
            &host.key,
            now,
            |_, candidate| {
                if own {
                    candidate.cover_own_checkpoint(
                        &app_id,
                        &document_id,
                        envelope.as_ref(),
                        now,
                    )?;
                }
                candidate
                    .receive_from(
                        TransportOrigin::RetainedServerDelta(author),
                        message.clone(),
                        now,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(((), true))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        // The write that starts a document is told like every write after
        // it: an agent a person's first message asks is woken by it.
        self.tell_followers(&workspace, author, &message, false)
            .await;
        self.wrote_frame(workspace.workspace(), &message);
        self.settle(&mut workspace).await
    }

    /// Take `message`, `author`'s change to the workspace's own document, into
    /// the workspace and keep it: a window a member opened, a member removed.
    /// The workspace's admission decides it as every member's device does.
    /// One that builds on history this device lacks waits in the copy for
    /// that history, and its sender is asked for it.
    async fn take_members_change(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        let mut workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?
            .lock_owned()
            .await;
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        // What the workspace's link admitted is in the replica before it comes
        // here, and is on disk only once a write of the workspace holds it: a
        // change already taken in that no write holds yet is written now. Left
        // for whichever change came next, a restart before that dropped it,
        // and every later change of its member built on a change the
        // workspace no longer had.
        let uncommitted = workspace.peer().workspace_heads() != workspace.kept_heads;
        let removed = std::cell::RefCell::new(std::collections::BTreeSet::new());
        let mut author_person = None;
        let changed = workspace.change(
            &host.key,
            now,
            |replica, candidate| {
                let outcome = candidate
                    .receive_from(TransportOrigin::RetainedServerDelta(author), message, now)
                    .map_err(|error| error.to_string())?;
                if !outcome.changed && !uncommitted {
                    return Ok((false, false));
                }
                // A member removed takes every way in the workspace handed them
                // with them: an invitation issued to them before, still
                // unredeemed, would otherwise admit them again at once.
                *removed.borrow_mut() = member_roots(replica, now)
                    .difference(&member_roots(candidate, now))
                    .copied()
                    .collect();
                // The person `author` writes for, when `author` is one of them:
                // who left the workspace themselves is not kept out of it.
                author_person = replica.governance().member_list(now).ok().and_then(|list| {
                    list.member_of(&author)
                        .and_then(tilo_peer_state::Member::root)
                });
                Ok((true, true))
            },
            |record, _| {
                let mut state = record.collaboration.clone();
                state.withdraw_invitations(&removed.borrow());
                Ok(state)
            },
        )?;
        if !changed {
            return Ok(());
        }
        let removed = removed.into_inner();
        workspace.keep_out_removed(&removed, author_person, tilo_clock::unix_now_ns())?;
        let settled = self.settle(&mut workspace).await;
        drop(workspace);
        settled
    }

    /// Take every device row of `workspace` whose certificate has expired out
    /// of its governance: a page's certificate lasts an hour, and this is what
    /// keeps a workspace many people visit from keeping a row per visit.
    /// Returns whether the workspace changed.
    fn forget_expired_devices(&self, workspace: &mut HostedWorkspace) -> Result<bool, String> {
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        let mut candidate = workspace.peer().clone();
        if !candidate
            .governance_mut()
            .forget_expired_devices(&host.key, now)?
        {
            return Ok(false);
        }
        workspace.keep(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        Ok(true)
    }

    /// Take `message`, `author`'s write to the record `(app, record)` the
    /// host keeps, into its copy, compact the workspace's sealed histories
    /// before one is full (past its bound the copy would stop following
    /// the record), tell whoever follows the workspace's documents, and file its
    /// verdict on a write admission refused. Answers the refusal, for the writer this host
    /// itself is.
    async fn follow_write(
        &self,
        workspace: &mut HostedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        (app, record): (String, String),
        compact: bool,
    ) -> Option<tilo_peer_state::AdmissionError> {
        let following = crate::timing::start();
        // A sealed compaction this copy is behind in is taken as the record's base rather than
        // refused ([`HostedWorkspace::follow_kept`]).
        let followed = self
            .follow_kept_checked(workspace, author, message.clone())
            .await;
        // A device with the record's keys checks the checkpoint leading it against the writes it
        // stands in for, and keeps that it did: the writes stay in every store until every such
        // device has.
        #[cfg(feature = "device-keys")]
        if followed.changed
            && let Err(error) = self
                .check_leading_checkpoint(workspace, (app.as_str(), record.as_str()))
                .await
        {
            tracing::warn!(workspace = %workspace.workspace(), %app, %record, %error,
                "this host's check of a record's checkpoint was not kept");
        }
        // The record's keys are not here yet: the retained store keeps the compaction like any
        // sealed frame it cannot read, and a replay takes it once they are. Asking to be let
        // into the record's channel is what brings them here.
        if followed.behind == Some(follow::BehindRefusal::Keys) {
            #[cfg(feature = "device-keys")]
            self.join_soon(workspace, app.clone(), record.clone());
            return followed.refused;
        }
        // Joining the record's channel is what names its app and record, and
        // only a build with device keys joins one.
        #[cfg(not(feature = "device-keys"))]
        let _ = (&app, &record);
        if followed.changed {
            self.landed(workspace, &message);
            self.keep_soon(workspace);
        }
        if followed.replay_wanted {
            self.replay_soon(workspace.workspace().to_owned());
        }
        if let Some(reason) = &followed.refused
            && let Err(error) = self.file_refusal(workspace, &message, reason).await
        {
            tracing::warn!(workspace = %workspace.workspace(), %reason, %error, "the host did not publish a refusal of a record write");
        }
        crate::timing::window("workspace.follow", following);
        #[cfg(feature = "device-keys")]
        if compact {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(workspace).await {
                tracing::warn!(workspace = %workspace.workspace(), %error, "hosted workspace did not compact a sealed record");
            }
            crate::timing::window("workspace.compact", compacting);
        }
        #[cfg(not(feature = "device-keys"))]
        let _ = compact;
        self.tell_followers(workspace, author, &message, false)
            .await;
        self.wrote_frame(workspace.workspace(), &message);
        followed.refused
    }

    /// File this device's verdict on the write `message` carried, which
    /// admission refused as `refusal` says, and publish it as the workspace's
    /// next governance: its author reads it live, and every member keeps it.
    /// Answers nothing about the write: that it did not land is the verdict.
    async fn file_refusal(
        &self,
        workspace: &mut HostedWorkspace,
        message: &neighbour_wire::NeighbourMsg,
        refusal: &tilo_peer_state::AdmissionError,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        let filed = workspace.change(
            &host.key,
            now,
            |_, candidate| {
                let filed = candidate
                    .workspace_mut()
                    .refuse_write(&host.cert, message, refusal, &host.key, now)?;
                Ok((filed, filed))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if !filed {
            return Ok(());
        }
        self.settle(workspace).await
    }

    /// Forget this device's verdicts on the writes of `message`'s route up to
    /// its sequence: a write of that author landed after all, so what its
    /// device refused is not missing. Answers whether the workspace changed.
    fn landed(&self, workspace: &HostedWorkspace, message: &neighbour_wire::NeighbourMsg) -> bool {
        let cleared = workspace.peer().workspace_mut().clear_landed_write(
            &self.inner.host.cert,
            message,
            &self.inner.host.key,
            tilo_clock::wall(),
        );
        match cleared {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(
                    workspace = %workspace.workspace(),
                    %error,
                    "the host did not clear a verdict on a write that landed"
                );
                false
            }
        }
    }

    /// Write through what every workspace's host took in of its members'
    /// writes, for a host that is stopping: the write-through is deferred
    /// ([`Self::keep_soon`]), so a host stopped inside that window would end
    /// with its copies in memory alone. A start replays what the server
    /// retains, but that replay takes a write only into a record the
    /// host already keeps: the record a member's write changed has to be
    /// on disk for a restart to serve it.
    pub async fn keep_what_was_followed(&self) {
        let held = {
            let workspaces = self.inner.workspaces.lock().await;
            workspaces.values().cloned().collect::<Vec<_>>()
        };
        for workspace in held {
            let mut workspace = workspace.lock().await;
            // A copy kept in the background may still be on its way: this
            // one, written now, holds all it held. A write the link took in
            // that the host has not followed yet is in the copy too.
            if workspace.unkept || workspace.parts.pending() || workspace.differs_from_file() {
                workspace.keeping = false;
                workspace.keep_followed();
            }
            workspace.resources.keep();
        }
    }

    /// Bring the server in line with the workspace's kept state: admit exactly
    /// the members its signed list has, then carry this host's own changes of
    /// it not yet carried. Runs after each decision, before the decision is
    /// answered.
    ///
    /// The changes are carried on the workspace's link here, under the
    /// workspace's lock, in the order they were made. A change that could not
    /// be carried is carried by the next settle; a device that links later is
    /// sent the workspace as it stands.
    async fn settle(&self, workspace: &mut HostedWorkspace) -> Result<(), String> {
        let host = &self.inner.host;
        // A page's device certificate lasts an hour: the row it left goes with
        // it, so a workspace many people visit keeps no row per visit.
        if self.forget_expired_devices(workspace)? {
            tracing::debug!(workspace = %workspace.workspace(), "workspace settled without the devices whose certificates expired");
        }
        let accessing = crate::timing::start();
        let now = tilo_clock::wall();
        let access: MemberListAccess = workspace.peer().governance().member_list_access(now)?;
        let (device_access, _) = workspace
            .peer()
            .governance()
            .projection(&access.member_devices, now)?;
        crate::timing::event("workspace.settle.access_read", accessing, 0);
        // Who is in the workspace is what its member list just said: the person
        // listing of every one of them, and the people the handshake trusts, are
        // answered from that, not by reading every workspace this host serves.
        self.inner.memberships.record(
            workspace.workspace(),
            access.member_user_roots.iter().copied(),
        );
        // A holder is no member and no trusted person, and keeps what it is
        // named to keep: the peers that dial it, and that it dials, admit the
        // one device each naming names.
        let now_ns = tilo_clock::unix_now_ns();
        let holders: Vec<[u8; 32]> = workspace
            .peer()
            .governance()
            .keeping_devices()
            .into_iter()
            .filter(|hint| tilo_user_identity::verify_device_cert_at(&hint.host, now_ns).is_ok())
            .map(|hint| hint.host.device_public_key)
            .collect();
        self.inner
            .memberships
            .record_holders(workspace.workspace(), holders);
        // What the workspace keeps as keys its person revoked: refused with how to approve another,
        // and dropped from its access, so every caller is told what to do.
        let revoked = collaboration::revoked_keys(workspace);
        host.access
            .settle(workspace.workspace_id(), access, revoked, device_access)
            .await
            .map_err(|error| error.to_string())?;
        crate::timing::event("workspace.settle.access", accessing, 0);
        let carrying = crate::timing::start();
        // A member copy's own change reaches the device serving it over the
        // copy's link as every write of its does: the records store keeps it
        // for whichever device links next, and the serving device takes it in
        // now. A host carries to its members from that store and has no member
        // link to itself.
        let workspace_id = workspace.workspace_id();
        let forwarded: Vec<_> = if self.member_link(workspace_id).is_some() {
            workspace
                .unsent
                .iter()
                .filter(|message| matches!(message, neighbour_wire::NeighbourMsg::SyncResp { .. }))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        workspace.carry_unsent(&self.inner.records)?;
        for message in forwarded {
            self.forward_member_document(workspace_id, message);
        }
        crate::timing::event("workspace.settle.publish", carrying, 0);
        let observing = crate::timing::start();
        self.dial_holders(workspace);
        self.governance_moved(workspace);
        crate::timing::event("workspace.settle.observe", observing, 0);
        Ok(())
    }

    /// Note `workspace`'s governance as it now stands, then tell whoever
    /// follows the workspace that it moved: the device's channel service
    /// decides under which group a record seals from the governance it reads,
    /// so this host's own actions in `workspace` find it there as soon as the
    /// workspace has settled, and watchers (a persistent workspace's operator)
    /// are told after, nobody listening being fine.
    fn governance_moved(&self, workspace: &HostedWorkspace) {
        let name = workspace.workspace().to_owned();
        if let Some(sink) = self
            .inner
            .host
            .channels
            .as_ref()
            .and_then(|channels| channels.sink.as_ref())
        {
            sink.settled(&name, workspace.peer().clone());
        }
        let _ = self.inner.changes.send(name);
    }

    /// The revocations this host holds from its people's roots, as they stand now.
    #[must_use]
    pub fn revocations(&self) -> RevocationList {
        self.inner.host.revocations.read_recover().clone()
    }
}
