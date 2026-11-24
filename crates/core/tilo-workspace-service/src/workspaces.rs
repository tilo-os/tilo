//! Workspaces this server governs.
//!
//! A shared workspace needs one authority that decides who is in it: it issues
//! invitations, admits the device that redeems one, changes roles and removes
//! members, and publishes each decision as signed workspace governance every
//! member verifies. Here that authority is the service. Its own device is the
//! workspace's governor: it keeps the workspace's [`StatePeer`], publishes
//! governance as its own retained changes of the workspace, and hands the
//! governed member set to the service's admission after every decision, so a
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
mod admissions;
mod admitting;
#[cfg(feature = "agents")]
mod agent_members;
mod agent_roster;
pub mod app_files;
mod appended;
#[cfg(feature = "agents")]
mod applications;
// The grant a person gives with an approve of an asked record.
#[cfg(not(target_arch = "wasm32"))]
mod asked_grants;
// The asked calls that wait, kept in their workspaces' records across the host's restarts.
#[cfg(not(target_arch = "wasm32"))]
mod asked_kept;
mod authored;
mod authority;
pub(crate) mod blocked;
mod branch;
mod moderation_history;
pub use branch::{BranchContent, BranchFileKeys, Branched};
mod channel_governance;
mod collaboration;
mod commit;
mod content;
mod creating;
mod default_apps;
mod delete;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
mod genesis;
mod machines;
mod mcp_server;
pub use machines::WorkspaceMachines;
mod names;
pub use names::PersonNames;
mod web_ui;
pub mod webhooks;
pub use web_ui::WebUi;
mod surfaces;
#[cfg(test)]
mod surfaces_tests;
#[cfg(feature = "agents")]
pub mod terminal;
pub use content::{ContentKeeper, WholeDocuments, WorkspaceContent};
pub(crate) mod device_actions;
mod follow;
mod founding;
pub mod providers;
mod removals;
pub mod task_programs;
#[cfg(test)]
mod work_tests;
pub use follow::ResourceWrite;
#[cfg(test)]
mod device_rejoin_tests;
mod holding;
#[cfg(test)]
mod returning_person_tests;
pub(crate) use holding::{HostWorkspaceRequest, decode_hex, owner, work_takes};
mod links;
mod member;
pub use member::describe;
pub use member::{AdmittedWrite, MemberDevice, MemberMaterial};
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
pub(crate) mod memberships;
/// The operator's kill switch, held while a host offers one.
pub mod mode;
mod own;
mod ownership;
#[cfg(not(target_arch = "wasm32"))]
mod packaged_model;
#[cfg(not(target_arch = "wasm32"))]
pub use packaged_model::{PackagedAction, package_surface};
#[cfg(all(test, not(target_arch = "wasm32")))]
mod packaged_model_runtime_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod packaged_model_tests;
mod pages;
mod peer_parts;
mod reads;
mod tree_identity;
mod writes;
// The person's mail accounts and model providers, kept where their
// workspace runs: a page has no mail connection and no vault of its own.
#[cfg(not(target_arch = "wasm32"))]
pub mod person_learned;
#[cfg(not(target_arch = "wasm32"))]
mod person_models;
pub use person_models::OpenAiSignIn;
#[cfg(not(target_arch = "wasm32"))]
mod person_settings;
#[cfg(not(target_arch = "wasm32"))]
pub mod this_computer;
// The inboxes such a host watches for its people, with no window open.
#[cfg(not(target_arch = "wasm32"))]
mod mail_watch;
#[cfg(not(target_arch = "wasm32"))]
pub use mail_watch::MailListening;
/// The mail servers a test stands in for, for the tests of what hears the mail.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use mail_watch::testing as mail_testing;
// A person's own model, asked for whatever this host runs for them.
#[cfg(not(target_arch = "wasm32"))]
pub mod person_model;
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
// The workspace's one vault, Passwords' document, as this device holds it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod vault;
mod vault_entries;
// The accounts of the workspace, signed in with entries of that vault.
#[cfg(not(target_arch = "wasm32"))]
pub mod account_logins;
#[cfg(all(test, any(feature = "agents", feature = "apps")))]
mod account_logins_tests;
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
mod record_reads;
pub use record_reads::RecordReads;
mod personal;
pub use personal::{PresentDevices, on_presence_change, presence_changed, present_on};
mod weak;
pub use weak::WeakWorkspaces;
mod publication;
use publication::{Change, SinceBase, starts_over, withdraws};
#[cfg(test)]
use publication::{MAX_PUBLICATION_TAIL, MIN_PUBLICATION_TAIL_BYTES, based};
mod document_format;
mod record_authority;
mod resources;
#[cfg(feature = "agents")]
pub use resources::RecordFailure;
mod revocations;
pub use own::OwnWorkspaceOffer;
pub(crate) mod sealed_documents;
/// How a workspace service holds a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// The service governs it: it settles the workspace's changes itself.
    Governs,
    /// A member copy of a workspace another device serves.
    MemberCopy,
}
pub use crate::documents::device_keys::{
    AskingBehind, DeviceKeyGroups, DeviceKeys, GroupKeys, GroupKeysAnswer, GroupKeysAnswerFuture,
    KeySink, KeysChangedFuture,
};
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub use sealed_documents::fixtures as sealed_fixtures;
#[cfg(test)]
mod copy_now_tests;
mod desktop_document;
#[cfg(test)]
mod governed_grants_tests;
pub mod membership;
#[cfg(test)]
mod naming_tests;
pub mod person_workspace;
// The person's own workspace's resources, as a part of a session reads and writes them.
#[cfg(not(target_arch = "wasm32"))]
pub mod person_resources;
#[cfg(not(target_arch = "wasm32"))]
pub use person_resources::{Holds, PersonResources};
// The person's region and formats and their zone: what a view rendered here for them shows
// dates in.
mod readings;
pub use readings::HeldCopy;
#[cfg(not(target_arch = "wasm32"))]
pub mod region;
pub mod servers;
mod shares;
pub mod standing;
#[cfg(test)]
pub(crate) mod tests;
/// A fixture workspace with its governance as actions, for tests outside this module.
#[cfg(all(test, feature = "agents", not(target_arch = "wasm32")))]
pub(crate) use actions::tests::governed;
mod unattended;
mod unbuildable_windows;
#[cfg(feature = "agents")]
mod views;
mod wait_list;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::store::{Appetite, Store as LinkStore};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::replica::PeerGuard;

use tilo_peer_state::TransportOrigin;
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub(crate) use actions::ADD_MEMBER as ADD_MEMBER_ACTION;
pub(crate) use actions::{GovernedServer, ServiceWorkspaceActions};
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
    HostedWorkspace, IssueInvitationRequest, IssuedInvitation, LIST_WORKSPACES, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest, UNHOST_WORKSPACE, WorkspaceListing,
    WorkspaceSummary,
};

use crate::registry::WorkspaceRegistry;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

/// The record's version this build writes, and the one it reads: it keeps no
/// name of its own, the governance's being the one (see [`document_format`]).
const DOCUMENT_VERSION: u32 = 5;
/// How long members' writes the governor took in may wait to be written
/// through to the workspace file. A hard stop loses at most these; replaying
/// what the server retains brings them back.
const KEEP_FOLLOWED: Duration = Duration::from_secs(1);

/// One governance message this workspace has published, in publication order.
/// Its bytes are shared by every copy of the record, so the workspace file's
/// writer knows a publication it wrote before by its allocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Publication {
    #[serde(with = "shared_bytes")]
    bytes: Arc<[u8]>,
    checkpoint: bool,
}

mod shared_bytes;

/// Everything a governed workspace keeps across restarts. Written atomically
/// after each decision, before it is published or answered; the large parts
/// of `peer` are files of their own ([`peer_parts`]), and the publications
/// and join receipts are in files appended to beside it ([`appended`]), all
/// written before it ([`document_format::StoredResource`]). Its own encoding is the
/// version 3 layout, which is what a record holds once its files are read back
/// into it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkspaceDocumentEntry {
    version: u32,
    workspace: String,
    workspace_id: WorkspaceId,
    owner: DeviceCert,
    peer: Vec<peer_parts::PeerPart>,
    generation: u64,
    /// The complete committed publication: the last full checkpoint and every
    /// change after it. Members receive it over the workspace's sync.
    outgoing: Vec<Publication>,
    /// How much of `outgoing` the server has retained, as of the workspace's last
    /// write. Publishing does not write the workspace again: after a crash the
    /// workspace republishes what it published since, which members tolerate.
    published: usize,
    collaboration: collaboration::CollaborationState,
}

/// The extension of the file beside a workspace record holding its
/// publications ([`appended`]).
const PUBLICATIONS: &str = "published";
/// The extension of the file beside a workspace record holding its join
/// receipts ([`appended`]).
const RECEIPTS: &str = "joins";

/// The file of a workspace's join receipts.
type Receipts = appended::Appended<tilo_peer_state::join_log::JoinCompletionRecord>;

/// A workspace this server governs.
pub(crate) struct GovernedWorkspace {
    record: WorkspaceDocumentEntry,
    peer: Replica,
    /// When the workspace was made, ms since the epoch: the time of the first
    /// change its governance holds, read once as it opens. A listing says it,
    /// and a later change to the workspace does not move it.
    made_at_ms: Option<i64>,
    path: PathBuf,
    /// The files of `record.peer`'s large parts.
    parts: peer_parts::PeerParts,
    /// `record.outgoing`, in its own file.
    publications: appended::Appended<Publication>,
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
    /// the day.
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
    /// Members' writes the governor took in that the workspace file lacks.
    unkept: bool,
    /// Whether writing them through is due already ([`Workspaces::keep_soon`]).
    keeping: bool,
    /// The channels of this workspace's records whose key holders this host
    /// is asking to admit it, by app and record: a burst of writes to a
    /// record the host holds no keys of yet asks once
    /// ([`Workspaces::join_soon`]).
    #[cfg(feature = "device-keys")]
    asking: std::collections::BTreeSet<(String, String)>,
    /// Whether something the workspace published since its document last
    /// started over overwrote or deleted a value ([`withdraws`]): a shared
    /// window closed, a member removed, a name changed. A newcomer replayed
    /// that history would read what was taken back before they came, so the
    /// next arrival starts the workspace over. Arrivals and other changes that
    /// only add are appended, and the publication then starts over only on its
    /// cadence ([`starts_over`]). Kept in memory: a workspace just made or read
    /// back from its file counts as having withdrawn something until it next
    /// starts over.
    since_base: SinceBase,
    /// Where the workspace's own document stood when this holder last
    /// published a change of it, or opened it. The replica moves past it
    /// between commits, as the workspace's link takes in a member's change or
    /// writes this holder's own checkpoints of the documents it covers, and the
    /// next commit publishes those changes with its own ([`Self::commit_held`]).
    published_heads: Option<Vec<automerge::ChangeHash>>,
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
    /// Legacy Chat moderation has been read completely and imported in this run.
    moderation_history_checked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The generation of the base `peer`'s workspace document starts from: `0`
/// for one that never started over.
fn base_generation(peer: &StatePeer) -> u64 {
    peer.admitted_workspace()
        .map_or(0, tilo_peer_state::workspace::base::generation)
}

/// The people `peer`'s governance admits now.
fn member_roots(peer: &StatePeer, now: SystemTime) -> std::collections::BTreeSet<[u8; 32]> {
    peer.governance()
        .governed_access(now)
        .map(|access| access.member_user_roots)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

impl GovernedWorkspace {
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
        let (mut publications, published) =
            appended::Appended::open(&path, PUBLICATIONS, stored.filed_publications)?;
        let (receipts, receipted) =
            appended::Appended::open(&path, RECEIPTS, stored.filed_receipts)?;
        let mut record = WorkspaceDocumentEntry::read(stored, published, receipted);
        // Publications an earlier build kept in its layout are written again in today's with the
        // record, the publications file whole, so no later start reads the older one.
        let republished = publication::brought_up(&mut record.outgoing)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if republished {
            publications.unsure();
        }
        // The record names the workspace's owner, a fact of its membership
        // that every holder keeps as it was handed over; the device opening
        // it only writes what this host changes, and is never taken for the
        // owner.
        if record.published > record.outgoing.len() {
            return Err("workspace record is inconsistent".into());
        }
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
        let mut workspace =
            Self::assemble(record, peer, path, parts, (publications, receipts), device)?;
        if let Some(today) = today {
            workspace.record.peer = today;
        }
        if older || republished {
            workspace
                .persist()
                .map_err(|error| format!("workspace written again in today's layout: {error}"))?;
        }
        Ok(workspace)
    }

    /// A workspace of `entry` and its replica `peer`, kept at `path` with
    /// its `parts` and its publications' and receipts' files.
    fn assemble(
        entry: WorkspaceDocumentEntry,
        peer: StatePeer,
        path: PathBuf,
        parts: peer_parts::PeerParts,
        (publications, receipts): (appended::Appended<Publication>, Receipts),
        device: Option<&DeviceKeys>,
    ) -> Result<Self, String> {
        let published_heads = peer.workspace_heads();
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
            moderation_history_checked: std::sync::Arc::default(),
            record: entry,
            peer: Replica::new(peer),
            made_at_ms,
            parts,
            publications,
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
            since_base: SinceBase::Withdrew,
            published_heads,
            holders: holding::Holders::default(),
            invitations,
        };
        workspace.publish_invitations();
        Ok(workspace)
    }

    /// Write the record as it stands ([`document_format::StoredResource`]): the
    /// publications and join receipts added since the last write appended to
    /// their files, then the workspace file naming how many those hold. A
    /// write costs what the change added, not the workspace's size.
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
        self.publications.unsure();
        self.receipts.unsure();
    }

    /// The workspace file as the record stands, to write: the publications and
    /// join receipts added since the last write are appended to their files
    /// first.
    fn file_write(&mut self) -> Result<peer_parts::FileWrite, String> {
        let published = self.record.published;
        let log = &self.record.collaboration;
        let filed_publications = self.publications.file(&self.record.outgoing)?;
        let filed_receipts = self.receipts.file(&log.join_completions)?;
        let beyond = |filed: u64| usize::try_from(filed).map_err(|error| error.to_string());
        let record = document_format::StoredDocumentRef {
            version: DOCUMENT_VERSION,
            workspace: &self.record.workspace,
            workspace_id: self.record.workspace_id,
            owner: &self.record.owner,
            peer: &self.record.peer,
            generation: self.record.generation,
            outgoing: self
                .record
                .outgoing
                .get(beyond(filed_publications)?..)
                .unwrap_or_default(),
            published,
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
            filed_publications,
            filed_receipts,
        };
        let bytes = postcard::to_stdvec(&record).map_err(|error| error.to_string())?;
        Ok(self.parts.file_write(&self.path, bytes, &self.record.peer))
    }

    fn peer(&self) -> PeerGuard<'_> {
        self.peer.lock()
    }

    /// Whether `device` governs the workspace's applications: the holder its
    /// governance names to make the compacted base
    /// ([`tilo_peer_state::PeerGovernance::base_maker`]), so every holder
    /// agrees on one and the next of the owner's holders takes over when that
    /// one stops holding the workspace; the device it was made on while its
    /// governance names no such holder.
    fn governs_applications(&self, device: &DeviceCert) -> bool {
        let governor = self
            .peer()
            .governance()
            .base_maker()
            .unwrap_or(self.record.owner.device_public_key);
        governor == device.device_public_key
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

    /// Record `state` alone: challenges, invitations, receipts. Governance and
    /// its publication are unchanged.
    fn commit_state(&mut self, state: collaboration::CollaborationState) -> Result<(), String> {
        let prior = std::mem::replace(&mut self.record.collaboration, state);
        if let Err(error) = self.persist() {
            self.record.collaboration = prior;
            return Err(error);
        }
        Ok(())
    }

    /// Make `change` to the workspace's governance and commit it as
    /// [`Self::commit`] does, staged from the replica (which `change` reads as
    /// it stands) held until the change
    /// commits: a member's write the workspace's link takes in meanwhile
    /// waits, rather than landing between the copy the change was made on and
    /// the commit, which would refuse the change for dropping that write.
    /// Answers what `change` answers; nothing is committed when it changed
    /// nothing.
    fn change_held<T>(
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
            self.commit_held(&mut replica, candidate, (key, now), Change::Other, state)?;
        }
        drop(replica);
        Ok(answer)
    }

    /// What `replica` took in or wrote since this holder last published a
    /// change of the workspace's own document, as one publication: a member's
    /// change its link admitted, or this holder's checkpoints of the documents
    /// it covers. It goes out ahead of the change being committed, which builds
    /// on it: a holder reaching this one only over a session is sent nothing
    /// else of it, and would hold every later change back for its history.
    fn published_since(&self, replica: &StatePeer) -> Result<Option<Publication>, String> {
        let Some(published) = &self.published_heads else {
            return Ok(None);
        };
        if replica.workspace_heads().as_ref() == Some(published) {
            return Ok(None);
        }
        let changes = replica.workspace_changes_since(published);
        if changes.is_empty() {
            return Ok(None);
        }
        publication(
            &neighbour_wire::NeighbourMsg::SyncResp {
                changes,
                workspace: replica.workspace_id().to_owned(),
            },
            false,
        )
        .map(Some)
    }

    /// Commit a governance change of `candidate`: sign it as the workspace's next
    /// generation, append its publication, and persist it with the
    /// collaboration state `state` derives from the committed workspace, all in
    /// one write before anything is answered.
    fn commit(
        &mut self,
        candidate: StatePeer,
        key: &SigningKey,
        now: SystemTime,
        state: impl FnOnce(
            &WorkspaceDocumentEntry,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        let shared = self.peer.shared();
        let mut replica = shared.lock();
        self.commit_held(&mut replica, candidate, (key, now), Change::Other, state)
    }

    /// Commit `candidate`, a change built from `replica`, which the caller
    /// holds: a member's write taken into the replica meanwhile waits, so it is
    /// neither lost to the change nor leaves the change uncommittable.
    fn commit_held(
        &mut self,
        replica: &mut StatePeer,
        mut candidate: StatePeer,
        (key, now): (&SigningKey, SystemTime),
        change: Change,
        state: impl FnOnce(
            &WorkspaceDocumentEntry,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        let committing = crate::timing::start();
        // The owner changes only by a transfer the workspace's governance
        // replays; any other owner is another workspace.
        if candidate.workspace_id() != replica.workspace_id() || !candidate.keeps_owner_of(replica)
        {
            return Err("a workspace change cannot replace the workspace".into());
        }
        // A holder that does not make the base takes the one its maker made as
        // the maker's own commit did: the workspace starts over from it here
        // too. Published as a change of the history before it, every holder's
        // check that a publication keeps the workspace's history would refuse
        // it, and the holders would part, each holding back the other's
        // changes for a history neither keeps any more. What this holder wrote
        // against that history and the base does not hold it writes again on
        // the base before anything is signed.
        let adopted = base_generation(&candidate) > base_generation(replica);
        if adopted {
            candidate.carry_onto_base(replica, &key.verifying_key().to_bytes())?;
        }
        let preparing = crate::timing::start();
        prepare_governance(&mut candidate, key, now)?;
        crate::timing::window("workspace.commit.prepare", preparing);
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or("workspace generation exhausted")?;
        let copying = crate::timing::start();
        let mut record = self.record.clone();
        crate::timing::window("workspace.commit.record", copying);
        // A member admitted from now on is given the workspace as it stands,
        // not what was withdrawn before they joined.
        // One device makes the workspace's compacted base, the one its
        // governance names for it, so every holder agrees which, and the next
        // one takes over when that one stops holding the workspace; every
        // other holder publishes only its own changes after it. A workspace
        // naming no holder of its owner's with its keys keeps it with the
        // device that made it.
        let base_maker = candidate
            .governance()
            .base_maker()
            .unwrap_or(record.owner.device_public_key);
        let makes_base = base_maker == key.verifying_key().to_bytes();
        let started_over = makes_base
            && (starts_over(&record.outgoing)
                || (self.since_base == SinceBase::Withdrew
                    && !member_roots(&candidate, now).is_subset(&member_roots(replica, now))));
        let mut since_base = self.since_base;
        if started_over || adopted {
            // Keep unchanged record checkpoints on their own channels; replacing
            // those baselines could discard member writes. A record changed by
            // this commit must still publish at the governance rollover.
            let changed_documents = changed_records(&candidate, replica, (key, now), generation);
            // The workspace's document starts over from a base holding its
            // state as it stands, with none of the history that led there.
            if !adopted {
                candidate.rebase_workspace()?;
            }
            since_base = SinceBase::Added;
            record.outgoing = base_publications(
                &mut candidate,
                replica,
                &changed_documents,
                (key, now),
                generation,
            )?;
            record.published = 0;
        } else {
            let diffing = crate::timing::start();
            record.outgoing.extend(self.published_since(replica)?);
            let messages = durable_messages(&candidate, replica, (key, now), generation);
            for message in &messages {
                if change == Change::Other && withdraws(message) {
                    since_base = SinceBase::Withdrew;
                }
                record.outgoing.push(publication(message, false)?);
            }
            crate::timing::window("workspace.commit.messages", diffing);
        }
        let encoding = crate::timing::start();
        record.peer = self.parts.stage(&candidate)?;
        crate::timing::window("workspace.commit.durable", encoding);
        record.generation = generation;
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
        self.published_heads = candidate.workspace_heads();
        *replica = candidate;
        self.since_base = since_base;
        crate::timing::event(
            "workspace.governance_commit",
            committing,
            self.record.outgoing.len() as u64,
        );
        Ok(generation)
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
            .governed_members(tilo_clock::wall())
            .unwrap_or_default();
        WorkspaceSummary {
            workspace: self.record.workspace.clone(),
            workspace_id: self.record.workspace_id,
            name: self.name(),
            generation: self.record.generation,
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
            generation: self.record.generation,
            role: None,
            labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            unattended: false,
            made_at_ms: self.made_at_ms,
        }
    }
}

use commit::{
    base_publications, changed_records, durable_messages, fold_into_checkpoint, prepare_governance,
    publication, select_exact_admission,
};

/// The extension of the marker beside a workspace file this host released.
const RELEASED: &str = "released";

/// What the workspace authority needs from the server hosting its workspaces.
#[derive(Clone)]
pub(crate) struct WorkspaceServices {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::service::GovernedAccessWriter,
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
    /// Agent delegations, for [`crate::workspace_access::decide`] and for
    /// refusing an agent the administration only a person holds.
    pub(crate) agents: crate::registry::AgentRegistry,
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
    /// workspaces it governs with its keys and admits nobody.
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
}

impl WorkspaceServices {
    /// What this keeper says of itself among a workspace's holders.
    pub(crate) fn holder(&self) -> HolderClaim {
        self.holder.read_recover().clone()
    }
}

/// The workspaces this server governs.
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

struct WorkspacesInner {
    host: WorkspaceServices,
    /// The sealed history of every workspace governed here, which their
    /// member devices catch up from over the direct link.
    store: Arc<tilo_peer_state::store::Store>,
    /// The direct links of the workspaces governed here, one per workspace,
    /// answering a device's dial by the workspace it names.
    direct: tilo_peer_state::direct::Serving,
    /// The devices each workspace's link admitted, by workspace: what
    /// [`Workspaces::watch_link_access`] re-checks when what admission reads
    /// changes.
    link_remotes: std::sync::Mutex<
        std::collections::BTreeMap<WorkspaceId, std::collections::BTreeSet<[u8; 32]>>,
    >,
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
    /// Room for the frames a workspace's link heard and its reader did not
    /// read yet, when a test bounds it ([`Workspaces::bound_link_readers`]);
    /// `0` keeps the link's own.
    #[cfg(test)]
    link_heard_capacity: std::sync::atomic::AtomicUsize,
    /// Whether this service answers the catch-up a device keeping one of its
    /// workspaces asks for as a build before the ask does: not at all
    /// ([`Workspaces::speak_no_catch_up`]).
    #[cfg(any(test, feature = "test-support"))]
    speaks_no_catch_up: std::sync::atomic::AtomicBool,
    /// The content each device handing a workspace over keeps none of
    /// itself, as it handed the list in parts ([`Workspaces::keep_absent`]).
    handed_absent: hosts::HandedAbsent,
    /// How many cursors an answer of where this device's store stands
    /// carries, when a test sets it ([`Workspaces::answer_cursors_in_pages_of`]);
    /// `0` keeps the build's own.
    #[cfg(any(test, feature = "test-support"))]
    cursors_per_answer: std::sync::atomic::AtomicUsize,
    /// Where iCloud accounts' calendars are kept, when a test stands a
    /// calendar server of its own in ([`Workspaces::keep_icloud_calendars_at`]).
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    icloud_calendars_at: std::sync::RwLock<Option<String>>,
    /// Whether this server is about to restart and hands its work to the
    /// other holders: it takes and renews no lease ([`Workspaces::drain`]).
    draining: std::sync::atomic::AtomicBool,
    /// The mode the host runs under, when its operator offers a kill switch: what its entry
    /// admits follows it ([`Workspaces::hold_mode`]).
    mode: std::sync::RwLock<Option<Arc<mode::HostMode>>>,
    dir: PathBuf,
    /// The workspaces governed here, each under its id spelled out: the one
    /// id a workspace has, which its channels carry too ([`Workspaces::held`]).
    workspaces: Arc<device_actions::WorkspacesMap>,
    /// What each governed workspace is read through without its lock
    /// ([`Workspaces::reading`]).
    readings: readings::Readings,
    /// What this device opened of the sealed resources its own roles read
    /// ([`Workspaces::own_read`]).
    own_opened: reads::OwnOpened,
    /// The action service the workspaces answer their members' devices on, as this
    /// host's own device ([`device_actions`]).
    action_service: tilo_peer_state::actions::ActionService,
    /// The work this host runs for its workspaces' devices: none until an
    /// embedder hands it a job runner ([`Workspaces::run_jobs`]).
    jobs: Arc<device_actions::DeviceWork>,
    /// Who waits for whom while workspaces are created ([`Creating`]).
    creating: creating::Creating,
    /// The workspace UUID after each settled governance decision.
    changes: tokio::sync::broadcast::Sender<String>,
    /// The workspace UUID each time this device's copy turns current with
    /// another of its holders ([`Workspaces::holders_reached`]).
    reached: tokio::sync::broadcast::Sender<String>,
    /// Each workspace deleted here, and its server workspace
    /// ([`Workspaces::deletions`]).
    deletions: tokio::sync::broadcast::Sender<(String, WorkspaceId)>,
    /// Each branch made here ([`Workspaces::branches`]).
    branches: tokio::sync::broadcast::Sender<Branched>,
    /// What copies a source's content into a branch of it
    /// ([`Workspaces::branch_content`]).
    branch_content: std::sync::RwLock<Option<Arc<dyn BranchContent>>>,
    /// Where calls that wait for a person's yes are kept as their asked records
    /// ([`Workspaces::keep_asked_in`]).
    asked_records: std::sync::OnceLock<Arc<dyn tilo_actions::remote::asked::AskedRecords>>,
    /// The store those records are kept in, which admits a person's answer to one.
    asked_store: std::sync::OnceLock<Arc<tilo_actions::remote::asked::AskedStore>>,
    /// This host's dispatch, as that store hands it back the calls it keeps.
    asked_dispatch: std::sync::OnceLock<Arc<dyn tilo_actions::remote::asked::AskedDispatch>>,
    /// What runs the workspaces' Linux machines, as a branch takes its
    /// source's and a deletion takes the workspace's ([`Workspaces::machines`]).
    machines: std::sync::RwLock<Option<Arc<dyn WorkspaceMachines>>>,
    /// The resources this device publishes of its own, by the app they are read at
    /// ([`Workspaces::publish_own`]).
    own_readings: std::sync::RwLock<
        std::collections::BTreeMap<&'static str, Arc<dyn tilo_actions::remote::ResourceReads>>,
    >,
    /// Where the name each person gave themselves is kept ([`Workspaces::person_names`]).
    person_names: std::sync::RwLock<Option<Arc<dyn PersonNames>>>,
    /// What starts and stops this device's own web UI
    /// ([`Workspaces::serve_web_with`]).
    web_ui: std::sync::RwLock<Option<Arc<dyn WebUi>>>,
    /// Each record write of a member the governor's copy took in.
    document_writes: tokio::sync::broadcast::Sender<ResourceWrite>,
    /// Moves whenever a person's own settings kept here — their mail
    /// accounts — changed, so a window following them reads them again
    /// ([`person_settings`]).
    person_settings_moved: tokio::sync::watch::Sender<u64>,
    /// What answers people's model calls, each with that person's own
    /// provider, model and key ([`person_model`]).
    #[cfg(not(target_arch = "wasm32"))]
    models: Arc<person_model::Models>,
    /// The sessions this host holds with the mail servers of the accounts
    /// its people keep ([`Workspaces::mail_session`]).
    #[cfg(not(target_arch = "wasm32"))]
    mail_session: std::sync::RwLock<person_settings::Session>,
    /// What uses of a vault item by reference keep here ([`hosted_use`]).
    #[cfg(not(target_arch = "wasm32"))]
    vault_uses: hosted_use::Uses,
    /// The inboxes this host watches, and who listens for them
    /// ([`mail_watch`]).
    #[cfg(not(target_arch = "wasm32"))]
    mail_watches: mail_watch::MailWatches,
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
    /// How many members' windows each workspace could not take in because they
    /// build on history it no longer has
    /// ([`unbuildable_windows::missing_history`]), by workspace.
    unbuildable_windows: std::sync::Mutex<HashMap<String, u64>>,
    /// When each workspace was last written to, and a ping after each entry
    /// that changed: the directory's own, beside the entries' governance.
    directory: listing::Directory,
    /// The entry's wait list, once whoever runs the server offers one
    /// ([`Workspaces::offer_wait_list`], [`Workspaces::mail_wait_list`]).
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
    /// The counter a person's workspace list moves when it changes
    /// ([`personal::PersonalPushes`]).
    personal_pushes: personal::PersonalPushes,
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

/// One person's place in a governed workspace, as installed bots compare it
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

/// What [`Workspaces::refound_document`] did: which record was founded again.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(feature = "agents")]
pub struct Refounded {
    pub app: String,
    pub record: String,
}

/// The authority governing one workspace, as the workspace's governance refers to it.
#[derive(Clone)]
pub struct WorkspaceAuthority {
    workspaces: std::sync::Weak<WorkspacesInner>,
    workspace: String,
}

impl WorkspaceAuthority {
    /// The workspaces, while the server still governs them.
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
    /// What the workspaces kept here hold, read and written in process: what
    /// a service the embedder runs beside them reads and writes through.
    #[must_use]
    pub fn documents(&self) -> crate::workspace_documents::WorkspaceDocuments {
        self.inner.records.clone()
    }

    /// The direct links of the workspaces governed here, as the host's own
    /// device: a member device of a workspace syncs with it here, and catches
    /// up from the history the host keeps.
    #[must_use]
    pub fn direct(&self) -> tilo_peer_state::direct::Serving {
        self.inner.direct.clone()
    }

    /// Whether `device` holds a link to `workspace_id` here now: one the
    /// workspace's host admitted and has not refused.
    #[must_use]
    pub fn linked(&self, workspace_id: WorkspaceId, device: &[u8; 32]) -> bool {
        self.inner
            .link_remotes
            .lock_recover()
            .get(&workspace_id)
            .is_some_and(|remotes| remotes.contains(device))
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
            Held::Governs
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
        // hand whoever asks the certificate of that key: a member device's own where its service
        // writes under a key of its own, else the host's.
        let endpoint_key = *host.endpoint.id().as_bytes();
        let (signer, answering): (Arc<dyn collab_types::signer::DeviceSigner>, _) =
            match &host.member {
                Some(member) if member.cert.device_public_key == endpoint_key => {
                    (Arc::clone(&member.signer), member.cert.clone())
                }
                _ => (Arc::new(host.key.clone()), host.cert.clone()),
            };
        let (action_service, jobs, answers) =
            device_actions::action_service(Arc::clone(&workspaces), signer);
        action_service.answer_under(answering);
        // The apps this host serves answer on the same action service its workspaces' devices
        // reach it on, beside the work and governance it answers itself.
        if let Some(apps) = host.apps.clone() {
            answers.serve_apps(crate::app_serving::Served::new(
                &apps,
                Arc::clone(&workspaces),
                &action_service,
            ));
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
        let models = Arc::new(person_model::Models::new());
        let memberships = host.access.memberships();
        let this = Self {
            inner: Arc::new(WorkspacesInner {
                #[cfg(test)]
                link_heard_capacity: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(any(test, feature = "test-support"))]
                speaks_no_catch_up: std::sync::atomic::AtomicBool::new(false),
                handed_absent: hosts::HandedAbsent::default(),
                #[cfg(any(test, feature = "test-support"))]
                cursors_per_answer: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
                icloud_calendars_at: std::sync::RwLock::new(None),
                draining: std::sync::atomic::AtomicBool::new(false),
                mode: std::sync::RwLock::new(None),
                store,
                direct,
                link_remotes: std::sync::Mutex::default(),
                agent_sessions: personal::AgentSessions::default(),
                records,
                member: member::Members::default(),
                executions: Arc::new(run_admission::ExecutionPins::new()),
                dir: dir.clone(),
                action_service,
                jobs,
                applications: host.apps.clone(),
                workspaces,
                readings: readings::Readings::default(),
                own_opened: reads::OwnOpened::default(),
                creating: creating::Creating::default(),
                changes: tokio::sync::broadcast::channel(64).0,
                reached: tokio::sync::broadcast::Sender::new(64),
                deletions: tokio::sync::broadcast::channel(64).0,
                branches: tokio::sync::broadcast::channel(64).0,
                branch_content: std::sync::RwLock::default(),
                asked_records: std::sync::OnceLock::new(),
                asked_store: std::sync::OnceLock::new(),
                asked_dispatch: std::sync::OnceLock::new(),
                machines: std::sync::RwLock::default(),
                own_readings: std::sync::RwLock::default(),
                person_names: std::sync::RwLock::default(),
                web_ui: std::sync::RwLock::default(),
                document_writes: tokio::sync::broadcast::channel(256).0,
                person_settings_moved: tokio::sync::watch::channel(0).0,
                #[cfg(not(target_arch = "wasm32"))]
                models: Arc::clone(&models),
                #[cfg(not(target_arch = "wasm32"))]
                mail_session: std::sync::RwLock::new(Arc::new(models.mail_session())),
                #[cfg(not(target_arch = "wasm32"))]
                vault_uses: hosted_use::Uses::proving_within(models.mail_reach()),
                #[cfg(not(target_arch = "wasm32"))]
                mail_watches: mail_watch::MailWatches::default(),
                #[cfg(not(target_arch = "wasm32"))]
                telling: person_telling::Telling::default(),
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                unbuildable_windows: std::sync::Mutex::default(),
                directory: listing::Directory::default(),
                host,
                entry: wait_list::EntrySettings::default(),
                releasing: AsyncMutex::new(()),
                own: std::sync::OnceLock::new(),
                #[cfg(all(feature = "files", not(target_arch = "wasm32")))]
                puts: std::sync::OnceLock::new(),
                memberships,
                owner_invitations: host_invitations::OwnerInvitations::open(&dir),
                personal_pushes: personal::PersonalPushes::default(),
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
        let workspaces = Box::pin(this.open_kept(&dir)).await?;
        *this.inner.workspaces.lock().await = workspaces;
        let kept = this.served_kept().await;
        this.serve_kept(&kept).await?;
        // A page reading an invitation answers without a workspace's lock
        // from its first read on.
        for workspace in &kept {
            this.keep_reading(workspace).await;
        }
        // Before any member's session reaches them: what a member reads of
        // the workspace's agents is already all of them.
        this.move_agent_rosters().await;
        this.follow_moderation_history().await;
        // And what a member's Files reads of the workspace's file tree is all of it.
        this.name_kept_trees().await;
        this.serve_kept_members().await;
        this.adopt_listings_file().await?;
        this.start_following();
        // What its links admitted is re-checked whenever what admission reads
        // narrows: a device that no longer reaches the workspace is let go.
        this.watch_link_access();
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
        workspaces: &[Arc<AsyncMutex<GovernedWorkspace>>],
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
            if let Err(error) = self.hand_kept_workspace_over(&mut workspace).await {
                tracing::error!(workspace = %workspace.workspace(), %error, "a workspace this host keeps for a person was not handed to them");
            }
            self.recall(&mut workspace)
                .await
                .map_err(|e| format!("recall: {e}"))?;
            tracing::info!("recalled");
            self.prepare_desktop_document(&mut workspace)
                .await
                .map_err(|e| format!("desktop record: {e}"))?;
            tracing::info!("desktop record ready");
            // What the server's log revoked while the workspace was closed.
            self.carry_revocations(&mut workspace, &[]).await?;
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

    /// The certificate this host serves its workspaces with: its own, or the
    /// one a workspace it opened was made with, when a build renamed the
    /// host's since (see [`record_authority::host_serving`]).
    #[must_use]
    pub fn serving_certificate(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// Stop governing and serving `workspace`: its workspace admits nobody from
    /// now on, which ends its sessions here, and the host leaves it closed
    /// when it opens again. Its files stay on disk. A device releases a
    /// workspace after moving it to another host, whose hints its members follow,
    /// and a server lets go of one its entry no longer keeps
    /// ([`servers::keeping`]); a member copy stops syncing.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, or the release could not be recorded
    /// or its access narrowed.
    pub async fn release(&self, workspace: &str) -> Result<(), String> {
        let _releasing = self.inner.releasing.lock().await;
        let governed = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this host does not hold that workspace")?;
        let mut governed = governed.lock().await;
        // Recorded first: a host stopped halfway keeps it released.
        let marker = governed.path.with_extension(RELEASED);
        tilo_files::native::atomic::write(&marker, b"").map_err(|error| error.to_string())?;
        self.inner.workspaces.lock().await.remove(workspace);
        self.inner.readings.forget(workspace);
        self.inner.direct.release(governed.workspace_id());
        // A member copy stops syncing with the device serving it.
        self.let_go_member(governed.workspace_id());
        // Its sessions to the other holders end with it.
        drop(std::mem::take(&mut governed.holders));
        // Nobody lists a workspace this host serves no more.
        self.inner.memberships.forget(workspace);
        self.stop_serving(governed.workspace_id()).await;
        tracing::info!(%workspace, workspace_id = %governed.workspace_id(), "governed workspace released");
        drop(governed);
        let _ = self.inner.changes.send(workspace.to_owned());
        // No list here names a workspace the host governs no longer.
        self.personal_changed();
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

    /// Every workspace this server governs.
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

    /// List `device` in the workspace carried as `workspace_id`, or forget it
    /// again: a device this host certified and started itself for that
    /// workspace (a browser environment of it), which the workspace holds as a
    /// member device while it runs and not after.
    ///
    /// # Errors
    ///
    /// No workspace of this host is carried as `workspace_id`.
    pub async fn list_device(
        &self,
        workspace_id: WorkspaceId,
        device: [u8; 32],
        listed: bool,
    ) -> Result<(), String> {
        let shared = {
            let registry = self.inner.host.workspaces.lock().await;
            registry.get(&workspace_id).cloned()
        }
        .ok_or_else(|| format!("no workspace of this host is carried as {workspace_id}"))?;
        shared.lock().await.list_environment(device, listed);
        Ok(())
    }

    /// The delegated agents on `workspace`'s roster, acting or not now.    ///
    /// # Errors
    ///
    /// Returns why the workspace's roster could not be read.
    pub async fn agents(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::GovernedAgent>, CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let agents = workspace
            .lock()
            .await
            .peer()
            .governance()
            .governed_agents(tilo_clock::wall());
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

    /// Whether this server's own person administers `governed`: its signed member list makes
    /// them its owner or one of its administrators. The server writes what a deployment asks
    /// for as that administrator, and the list decides it again on every device, so what only
    /// its owner may change stays theirs, and an owner who takes the grant back is obeyed.
    #[must_use]
    fn administers(&self, governed: &GovernedWorkspace) -> bool {
        let host = hex::encode(self.inner.host.cert.user_public_key);
        governed
            .peer()
            .governance()
            .governed_policy()
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
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        self.administers(&governed)
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

    /// Answer one request for `workspace_id`'s records through the app action server this host
    /// serves, as `caller`: the same admission every request takes, so what the host writes for
    /// a person is kept, published and announced to whoever follows the record as any write is.
    /// `None` when this host serves no apps.
    pub(crate) async fn serve_app(
        &self,
        workspace_id: WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Option<tilo_actions::remote::RemoteActionMessage> {
        let applications = self.inner.applications.as_ref()?;
        let answer = applications.serve(workspace_id, caller, message)?;
        answer.await
    }

    /// Where this host hands a request to a device of its workspaces, over the
    /// link that device holds to it ([`crate::app_serving::Hand`]).
    #[must_use]
    pub fn hand(&self) -> crate::app_serving::Hand {
        crate::app_serving::Hand::new(self.action_service(), self.inner.applications.clone())
    }

    /// Run the turns of work this device takes for its workspaces' devices on
    /// `jobs` ([`crate::work`]): whether a turn is this device's is
    /// [`takes_turn`](crate::work::takes_turn)'s to say, live, each time.
    pub fn run_jobs(&self, jobs: Arc<dyn crate::work::HostJobs>) {
        self.inner.jobs.serve(jobs);
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
    /// its folders stand), read in the person's own workspace by this device's own person on this
    /// device alone ([`record_reads::RecordReads`]).
    pub fn publish_own(
        &self,
        app: &'static str,
        reads: Arc<dyn tilo_actions::remote::ResourceReads>,
    ) {
        self.inner.own_readings.write_recover().insert(app, reads);
    }

    /// What answers the reads of `app`, when it is a resource of this device's own.
    pub(crate) fn own_reading(
        &self,
        app: &str,
    ) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        self.inner.own_readings.read_recover().get(app).cloned()
    }

    /// The providers registered on this device ([`Self::provide`]): the one registry this device's
    /// dispatch and its action service reach them in.
    #[must_use]
    pub fn providers(&self) -> providers::Providers {
        self.inner.jobs.providers.clone()
    }

    /// Answer the workspaces' governance actions their member devices ask on the device action
    /// service through `governance`.
    pub(crate) fn govern_device_actions_with(&self, governance: std::sync::Weak<actions::Shared>) {
        self.inner.jobs.govern_with(governance);
    }

    /// Let agents from the person's other devices work here, or not, as a test says
    /// rather than this device's settings.
    #[cfg(any(test, feature = "test-support"))]
    pub fn accept_agents_from_other_devices(&self, accepts: bool) {
        self.inner.jobs.accept_agents_from_other_devices(accepts);
    }

    /// What this device answers a device of its workspaces on the device action service: the
    /// turns of work it takes, and the workspaces' governance. A device whose session answers that
    /// service in the host's place hands these requests here.
    #[must_use]
    pub fn device_actions(&self) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
        Arc::new(device_actions::Answers {
            work: Arc::clone(&self.inner.jobs),
            apps: std::sync::OnceLock::new(),
        })
    }

    /// Workspace UUIDs, each sent after a governance decision on that workspace was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// Each write to a record a workspace held here took in: a member's write to
    /// a workspace this host governs, and every write the device serving a
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

    /// The governor's copy of record `record` (`""` for the primary) of
    /// `app` in `workspace`, saved: a record members write sealed is read with
    /// the keys this server holds as a member of its channel.
    ///
    /// A document the workspace's resource store placed
    /// ([`collab_types::is_store_document`]) is the store's, which the
    /// governor's replica does not hold: it is read from the store.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or a sealed record cannot be opened.
    pub async fn resource_document(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        if self.moderation_history_pending(workspace, app, record) {
            return Err(CollaborationError::new(
                "the workspace's moderation history is still arriving",
            ));
        }
        if collab_types::is_store_document(record) {
            let state = self.workspace(workspace).await?;
            return Ok(state.lock().await.store_document(record));
        }
        // Read without holding the workspace: the record's keys are asked for
        // while every other read, decision and member's write goes on.
        let reading = match WorkspaceId::parse(workspace) {
            Some(id) => self.reading(id).await,
            None => None,
        }
        .ok_or_else(|| CollaborationError::new("this server does not hold that workspace"))?;
        sealed_documents::readable_now(&reading, (app, record))
            .await
            .map_err(CollaborationError::new)
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
        query: tilo_resource_store::Query,
    ) -> Result<Vec<(String, tilo_resource_store::Indexed)>, String> {
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
    ) -> Option<tilo_resource_store::Indexed> {
        let state = self.workspace(workspace).await.ok()?;
        state.lock().await.store_held(kind, id)
    }

    /// The `(app, record)` of every record of `workspace` the governor's copy
    /// holds readable: in the clear, or sealed under channels this server
    /// takes part in ([`Self::resource_document`] opens those).
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn document_routes(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let mut routes = governed.peer().plaintext_routes();
        if governed.seals() {
            routes.extend(governed.peer().sealed_routes());
        }
        drop(governed);
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
        let governed = state.lock().await;
        let mut routes = governed.peer().plaintext_routes();
        if governed.seals() {
            routes.extend(governed.peer().sealed_routes());
        }
        routes.retain(|(app, record)| !tilo_action_policy::roles::is_private_record(app, record));
        drop(governed);
        Ok(routes)
    }

    /// The state of `workspace` as this host holds it now: what a channel
    /// service reads who may read the workspace's channels from.
    pub async fn state(&self, workspace: &str) -> Option<StatePeer> {
        let governed = self.workspace(workspace).await.ok()?;
        let state = governed.lock().await.peer().clone();
        Some(state)
    }

    /// `workspace` as governance records it now.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn summary(&self, workspace: &str) -> Result<WorkspaceSummary, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        Ok(self.people(&governed))
    }

    /// `governed`'s people as every list showing them has them: the ones its
    /// governance has, its owner among them — without this host's own person
    /// where it runs workspaces of people's own. There the host runs for
    /// everyone and is no person of theirs, though its device founds each
    /// workspace and so is the owner its policy names.
    fn people(&self, governed: &GovernedWorkspace) -> WorkspaceSummary {
        let mut summary = governed.summary();
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
    /// The workspace is not governed here.
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
    /// The workspace is not governed here.
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
    /// The workspace is not governed here.
    pub async fn kept_membership(
        &self,
        workspace: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = tilo_clock::wall();
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let people = governed.summary();
        drop(governed);
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
    /// The workspace is not governed here.
    pub async fn membership(
        &self,
        workspace: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = tilo_clock::wall();
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let people = self.people(&governed);
        drop(governed);
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

    async fn workspace(
        &self,
        workspace: &str,
    ) -> Result<Arc<AsyncMutex<GovernedWorkspace>>, CollaborationError> {
        self.inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or_else(|| CollaborationError::new("this server does not hold that workspace"))
    }

    /// The membership answer a page builds its replica of `workspace_id` from
    /// as it joins it: the workspace's roster, its revocations and its
    /// governance as one message, answered for `device`, the link's certified
    /// device. What follows such an answer is admitted like any governance
    /// ([`tilo_peer_state::owner::NativeWorkspaceOwner::install_vouched`]). Built
    /// only for a device a subscription is answered with: it is as large as the
    /// workspace.
    pub(crate) async fn subscriber_bootstrap(
        &self,
        workspace_id: WorkspaceId,
        device: &DeviceCert,
    ) -> Option<Vec<u8>> {
        use tilo_peer_state::join::{JoinBootstrap, JoinTransportResponse};
        use tilo_peer_state::join_log::GovernanceMessages as _;
        let workspace = self.workspace(&workspace_id.to_string()).await.ok()?;
        let governed = workspace.lock().await;
        let peer = governed.peer();
        let now = tilo_clock::wall();
        let role = peer.role(&device.device_public_key, now).ok()?;
        let host = self.inner.host.cert.clone();
        // The roster names what the device holds before the governance it is sent with,
        // or could not take from it: every other member device's certificate arrives
        // with the governance.
        let certified_roster =
            peer.roster_beside_governance(&[device.device_public_key, host.device_public_key], now);
        let governance = peer.governance_messages().ok()?;
        postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(JoinBootstrap {
            version: 1,
            invite_id: [0; 32],
            device_cert: device.clone(),
            role,
            workspace_id: governed.workspace_id(),
            host,
            certified_roster,
            revocations: peer.revocations(),
            records: Vec::new(),
            governance_messages: governance,
            generation: governed.record.generation,
        }))
        .ok()
    }

    /// Hold the workspace, admitting only the members its signed list names,
    /// and follow its members' writes: create the records they start and keep the
    /// governor's copy of the others current.
    ///
    /// Members' changes stay retained as signed tails: members write directly,
    /// not through the governor, so the owner-baseline selection that retires
    /// a native owner's covered tails does not apply.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace is configured under one lock so no subscriber sees it half bound"
    )]
    async fn bind(&self, governed: &GovernedWorkspace) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace = governed.workspace().to_owned();
        let workspace_id = governed.workspace_id();
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

    /// Replay what the server retains for `workspace` into the governor's
    /// copy now: [`Self::replay_soon`] in the caller's own time.
    async fn replay(&self, workspace: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let workspace_id = state.lock().await.workspace_id();
        let retained = match self.inner.records.retained_settled(workspace_id).await {
            Ok(retained) => retained,
            Err(error) => {
                tracing::warn!(%workspace, %error, "governed workspace did not replay what its members wrote");
                return;
            }
        };
        #[cfg(feature = "device-keys")]
        self.found_retained(workspace, &state, &retained).await;
        let mut governed = state.lock().await;
        self.recall_retained(&mut governed, retained).await;
        self.retry_held(&mut governed).await;
    }

    /// Ask the key holders of `app`'s `record`'s channel in `workspace` to
    /// admit this host's device ([`Self::join_channel`]), apart from whatever
    /// asked for it: the group's answer arrives as another write of the
    /// workspace, which waits for the writes ahead of it, a notice in this
    /// one's queue included. Once it is admitted, what the server retains of
    /// the record is replayed ([`Self::replay_soon`]), so its copy starts
    /// from its first write. Asked once per record until the first answer.
    #[cfg(feature = "device-keys")]
    fn join_soon(&self, workspace: &mut GovernedWorkspace, app: String, record: String) {
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
            // record once that is in the governor's copy.
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
    /// the workspace is not governed here; the keys arrive on the channel
    /// service's own time, and the record reads once they have.
    pub async fn ask_to_open(&self, workspace: &str, app: &str, record: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let mut governed = state.lock().await;
        #[cfg(feature = "device-keys")]
        self.join_soon(&mut governed, app.to_owned(), record.to_owned());
        #[cfg(not(feature = "device-keys"))]
        {
            let _ = (&mut governed, app, record);
        }
    }

    /// Whether this host waits to be let into the channel that seals `record`
    /// of `app` in `workspace`: it runs on a device that takes part in the
    /// workspace's channels, and holds none of that channel's keys yet. Once
    /// it holds them, every write of the record reaches it readable and is
    /// followed as it lands, so nothing about the record is worth asking
    /// again. It waits while it is let in and replays what the keys open.
    /// `false` for a workspace not governed here.
    pub async fn waits_for_keys(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let governed = state.lock().await;
        #[cfg(feature = "device-keys")]
        let waits = governed.seals()
            && (governed
                .asking
                .contains(&(app.to_owned(), record.to_owned()))
                || !governed.holds_keys().await);
        #[cfg(not(feature = "device-keys"))]
        let waits = {
            let _ = (&governed, app, record);
            false
        };
        drop(governed);
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
            } if named == workspace => self.absorb_topology(workspace, author, message).await,
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

    /// Take in the record write `body` of `author`. A record the workspace lacks
    /// is created from it, if `author` is a device of an editor: records are
    /// born only from a governor checkpoint. A write to a record the workspace has
    /// updates the governor's copy, written through to the workspace file soon
    /// after ([`Self::keep_soon`]).
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
    /// Every member's record write reaches the governor here, so this is where one that does not
    /// decode is reported (the content classifier reads the same body before it leaves it to
    /// this). The server's own device writes records through the workspace too: the actions it
    /// runs for members (a visitor linking their Discord account) are committed as that device,
    /// and the governor's copy, which the workspace's bots read, must take them in like any
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
        // A record starts from the write that founds it: a write that is the
        // whole record founds the route on its own, and is not preceded by a
        // cleartext genesis the members, who read only what the channel seals,
        // would have to account for. The checkpoint includes the write that
        // created a record founded here in the clear: the server retains a
        // member's changes only as a tail of a checkpoint that resolves their
        // dependencies.
        let whole = envelope.is_whole_resource();
        // A record this host's own device founds starts from the write itself: that write is the
        // whole record, and the coverage row the workspace governs its checkpoints by is
        // published with it, as a snapshot this device publishes itself has it
        // ([`StatePeer::cover_own_checkpoint`]). It is no cleartext genesis the members would
        // have to account for. Any other writer's founding write is a change to the record, and
        // the host starts that one empty first.
        let own = author == host.cert.device_public_key;
        let founding_keys = self
            .founding_keys(&workspace, &app_id, &document_id, whole)
            .await?;
        workspace.change_held(
            &host.key,
            now,
            |_, candidate| {
                if !whole && !own {
                    sealed_documents::refound(
                        candidate,
                        founding_keys.as_ref(),
                        (&app_id, &document_id),
                        automerge::AutoCommit::new().save(),
                        &host.key,
                        now,
                    )?;
                }
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
                // A sealed write stays in the record's sealed history; a
                // readable one is folded into the governor's checkpoint.
                if !sealed {
                    fold_into_checkpoint(candidate, (&app_id, &document_id), &host.key, now)?;
                }
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

    /// Take a member's own windows into the workspace's governance: the changes
    /// a member publishes to the workspace's layout. The workspace's admission decides
    /// them exactly as every member's does (a change to what someone else
    /// made, without the workspace's leave, is refused); the governor commits what
    /// it admitted as the workspace's next generation, which every member then
    /// takes in. Until this, a window someone opened after joining reached
    /// nobody: members take governance only from the governor, and the
    /// governor never took these in, so the member published them again
    /// every few seconds for nothing.
    async fn absorb_topology(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        let workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut workspace = workspace.lock().await;
        if !self.take_topology(&mut workspace, author, message).await? {
            // Held back for history this device lacks: the history the
            // server keeps is taken in again, and the change with it.
            self.replay_soon(workspace.workspace().to_owned());
            return Ok(());
        }
        self.retry_held(&mut workspace).await;
        drop(workspace);
        Ok(())
    }

    /// Try again the changes held back for history (`take_topology`), now
    /// that something landed: those whose history arrived are taken in, the
    /// others held on.
    async fn retry_held(&self, workspace: &mut GovernedWorkspace) {
        loop {
            let held = workspace.holders.take_held();
            if held.is_empty() {
                break;
            }
            let mut took = false;
            for (author, message) in held {
                match self.take_topology(workspace, author, message).await {
                    Ok(taken) => took |= taken,
                    Err(error) => {
                        tracing::warn!(workspace = %workspace.workspace(), %error, "a change held back for its history was refused");
                    }
                }
            }
            if !took {
                break;
            }
        }
        workspace.holders.say_held();
    }

    /// Take `message`, `author`'s change to the workspace's state, into
    /// `workspace`. Whether it was taken: a change another holder sent that
    /// builds on history this device lacks is held back
    /// ([`holding::Holders::hold`]) until that history arrives, and the
    /// workspace's copy status says so; a member's windows built on a
    /// history the workspace started over from are refused.
    async fn take_topology(
        &self,
        workspace: &mut GovernedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<bool, String> {
        let host = &self.inner.host;
        if let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = &message {
            let missing = unbuildable_windows::missing_history(
                workspace.peer().admitted_workspace(),
                changes,
            )?;
            if missing > 0 {
                let from_holder = workspace
                    .peer()
                    .governance()
                    .keeping_devices()
                    .iter()
                    .any(|hint| hint.host.device_public_key == author);
                if from_holder {
                    workspace.holders.hold(author, message);
                    return Ok(false);
                }
                return Err(self.unbuildable_windows(workspace.workspace(), missing));
            }
        }
        let now = tilo_clock::wall();
        // What the workspace's link admitted is in the replica before it comes
        // here, and is on disk only once the governor commits it: a change
        // already taken in that no commit carries yet is committed now. Left
        // for whichever change came next, a restart before that dropped it,
        // and every later change of its member built on a change the
        // workspace no longer had.
        let uncommitted = workspace.peer().workspace_heads() != workspace.published_heads;
        let removed = std::cell::RefCell::new(std::collections::BTreeSet::new());
        let mut author_person = None;
        let changed = workspace.change_held(
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
                author_person = replica
                    .governance()
                    .governed_members(now)
                    .ok()
                    .and_then(|list| {
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
            return Ok(true);
        }
        let removed = removed.into_inner();
        workspace.keep_out_removed(&removed, author_person, tilo_clock::unix_now_ns())?;
        self.settle(workspace).await?;
        Ok(true)
    }

    /// Refound `record` of `app` from what the governor itself can still read,
    /// when its kept history lost the base its deltas were written over.
    ///
    /// A record whose kept checkpoint is gone while deltas over it remain
    /// cannot be read by anyone: the governor's own reader meets changes whose
    /// history it lacks, and every device that joins afterwards is replayed a
    /// tail that builds on changes no frame carries. Nothing a member sends
    /// can repair it — a snapshot of the record as one device holds it is not
    /// the record's history — so the repair is the governor's own: it writes
    /// the record as it stands as the workspace's next checkpoint of the
    /// record, which founds the route again for every device reading it. The
    /// store keeps the frames before it, which a newer checkpoint covers.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the record is not one the governor
    /// holds, or the new checkpoint could not be published.
    #[cfg(feature = "agents")]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace is read under one lock before the checkpoint is built from it"
    )]
    pub async fn refound_document(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Refounded, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let now = tilo_clock::wall();
        let host = &self.inner.host;
        let keys = {
            let workspace = state.lock().await;
            if !workspace.peer().holds_document(app, record) {
                return Err(CollaborationError::new("the record is not held here"));
            }
            let readable = workspace
                .readable_document(app, record)
                .await
                .map_err(CollaborationError::new)?
                .ok_or_else(|| CollaborationError::new("the record does not read"))?;
            let keys = workspace
                .document_keys(app, record)
                .await
                .map_err(CollaborationError::new)?;
            (readable, keys)
        };
        let (readable, sealed) = keys;
        let mut workspace = state.lock().await;
        let mut candidate = workspace.peer().clone();
        // The record as it reads, published as the workspace's next checkpoint
        // of the route: a whole record, sealed where the record is sealed,
        // admitted as this state's own write so the replay that follows
        // carries it.
        sealed_documents::refound(
            &mut candidate,
            sealed.as_ref(),
            (app, record),
            readable,
            &host.key,
            now,
        )
        .map_err(CollaborationError::new)?;
        workspace.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut workspace).await?;
        // The new checkpoint is what a device joining now is replayed: wait
        // for it to be admitted to the host's order and durable before the
        // operator is told the record reads again.
        tracing::warn!(
            workspace = %workspace.workspace(),
            %app,
            %record,
            "a record whose base its history lost was refounded"
        );
        Ok(Refounded {
            app: app.to_owned(),
            record: record.to_owned(),
        })
    }

    /// Take every device row of `workspace` whose certificate has expired out
    /// of its governance: a page's certificate lasts an hour, and this is what
    /// keeps a workspace many people visit from keeping a row per visit.
    /// Returns whether the workspace changed.
    fn forget_expired_devices(&self, workspace: &mut GovernedWorkspace) -> Result<bool, String> {
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        let mut candidate = workspace.peer().clone();
        if !candidate
            .governance_mut()
            .forget_expired_devices(&host.key, now)?
        {
            return Ok(false);
        }
        workspace.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        Ok(true)
    }

    /// Take `message`, `author`'s write to the record `(app, record)` the
    /// governor holds, into its copy, compact the workspace's sealed histories
    /// before one is full (past its bound the copy would stop following
    /// the record), tell whoever follows the workspace's documents, and file its
    /// verdict on a write admission refused. Answers the refusal, for the writer this host
    /// itself is.
    async fn follow_write(
        &self,
        workspace: &mut GovernedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        (app, record): (String, String),
        compact: bool,
    ) -> Option<tilo_peer_state::AdmissionError> {
        let following = crate::timing::start();
        // A sealed compaction this copy is behind in is taken as the record's base rather than
        // refused ([`GovernedWorkspace::follow_kept`]).
        let followed = workspace.follow_kept(author, message.clone()).await;
        // A device with the record's keys checks the checkpoint leading it against the writes it
        // stands in for, and keeps that it did: the writes stay in every store until every such
        // device has.
        #[cfg(feature = "device-keys")]
        if followed.changed {
            if let Err(error) = self
                .check_leading_checkpoint(workspace, (app.as_str(), record.as_str()))
                .await
            {
                tracing::warn!(workspace = %workspace.workspace(), %app, %record, %error,
                    "this host's check of a record's checkpoint was not kept");
            }
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
            tracing::warn!(workspace = %workspace.workspace(), %reason, %error, "the governor did not publish a refusal of a record write");
        }
        crate::timing::window("workspace.follow", following);
        #[cfg(feature = "device-keys")]
        if compact {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(workspace).await {
                tracing::warn!(workspace = %workspace.workspace(), %error, "governed workspace did not compact a sealed record");
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
        workspace: &mut GovernedWorkspace,
        message: &neighbour_wire::NeighbourMsg,
        refusal: &tilo_peer_state::AdmissionError,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        let filed = workspace.change_held(
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
    fn landed(
        &self,
        workspace: &GovernedWorkspace,
        message: &neighbour_wire::NeighbourMsg,
    ) -> bool {
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
                    "the governor did not clear a verdict on a write that landed"
                );
                false
            }
        }
    }

    /// Write through what every workspace's governor took in of its members'
    /// writes, for a host that is stopping: the write-through is deferred
    /// ([`Self::keep_soon`]), so a host stopped inside that window would end
    /// with its copies in memory alone. A start replays what the server
    /// retains, but that replay takes a write only into a record the
    /// governor already holds: the record a member's write changed has to be
    /// on disk for a restart to serve it.
    pub async fn keep_what_was_followed(&self) {
        let held = {
            let workspaces = self.inner.workspaces.lock().await;
            workspaces.values().cloned().collect::<Vec<_>>()
        };
        for workspace in held {
            let mut workspace = workspace.lock().await;
            // A copy kept in the background may still be on its way: this
            // one, written now, holds all it held.
            if workspace.unkept || workspace.parts.pending() {
                workspace.keeping = false;
                workspace.keep_followed();
            }
            workspace.resources.keep();
        }
    }

    /// Bring the server in line with the workspace's committed state: admit exactly
    /// the members its signed list has, then retain every governance change not yet
    /// published. Runs after each
    /// decision, before the decision is answered.
    ///
    /// The changes are kept at the device store and carried on the
    /// workspace's link here, under the workspace's lock, in publication
    /// order. A change that could not be kept is published again by the next
    /// settle.
    async fn settle(&self, workspace: &mut GovernedWorkspace) -> Result<(), String> {
        let host = &self.inner.host;
        // A page's device certificate lasts an hour: the row it left goes with
        // it, so a workspace many people visit keeps no row per visit.
        if self.forget_expired_devices(workspace)? {
            tracing::debug!(workspace = %workspace.workspace(), "workspace settled without the devices whose certificates expired");
        }
        let accessing = crate::timing::start();
        let now = tilo_clock::wall();
        let access: GovernedAccess = workspace.peer().governance().governed_access(now)?;
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
        // What the workspace keeps as keys its person revoked: refused with how to approve another,
        // and dropped from its access, so every caller is told what to do.
        let revoked = collaboration::revoked_keys(workspace);
        host.access
            .settle(workspace.workspace_id(), access, revoked, device_access)
            .await
            .map_err(|error| error.to_string())?;
        crate::timing::event("workspace.settle.access", accessing, 0);
        let publishing = crate::timing::start();
        workspace.publish(&self.inner.records)?;
        crate::timing::event("workspace.settle.publish", publishing, 0);
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
    fn governance_moved(&self, workspace: &GovernedWorkspace) {
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
        // Whose workspaces those are changed: what their devices list.
        self.personal_changed();
    }

    /// The revocations this host holds from its people's roots, as they stand now.
    #[must_use]
    pub fn revocations(&self) -> RevocationList {
        self.inner.host.revocations.read_recover().clone()
    }
}
