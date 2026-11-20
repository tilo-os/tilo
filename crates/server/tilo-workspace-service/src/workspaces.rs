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
#[cfg(feature = "agents")]
mod agent_members;
mod agent_roster;
pub mod app_files;
mod appended;
#[cfg(feature = "agents")]
mod applications;
mod authored;
mod authority;
mod bootstrap;
mod branch;
pub use branch::{BranchContent, Branched};
mod channel_governance;
mod commit;
mod mcp_server;
pub(crate) use bootstrap::member_bootstrap;
mod collaboration;
mod content;
mod creating;
mod delete;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
mod genesis;
mod machines;
pub use machines::WorkspaceMachines;
mod names;
pub use names::PersonNames;
mod surfaces;
pub use content::{ContentKeeper, WholeDocuments, WorkspaceContent};
mod desktop_writes;
mod device_actions;
#[cfg(test)]
mod device_actions_tests;
mod device_keys;
mod follow;
mod founding;
mod removals;
pub use device_actions::{HostJobs, Offer};
pub use follow::ResourceWrite;
#[cfg(test)]
mod device_rejoin_tests;
#[cfg(test)]
mod hard_stop_tests;
#[cfg(test)]
mod removed_keeper_tests;
#[cfg(test)]
mod returning_person_tests;
pub(crate) use desktop_writes::writes_desktop;
mod holding;
pub(crate) use holding::{HostWorkspaceRequest, decode_hex, owners, work_takes};
mod member;
pub use member::{MemberDevice, MemberMaterial, MemberWrite};
mod run_admission;
pub use run_admission::{Refusal, SessionAdmission};
mod host_invitations;
mod hosts;
mod link_follow;
mod session_carry;
pub(crate) use link_follow::carry_admitted;
mod carried;
use carried::Carried;
mod clock;
pub(crate) use clock::system_time;
mod delegated_work;
mod listing;
mod memberships;
mod own;
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
mod writes;
// The person's mail accounts and model providers, kept where their
// workspace runs: a page has no mail connection and no vault of its own.
#[cfg(not(target_arch = "wasm32"))]
pub mod person_learned;
// The person's region and formats and their zone: what a view rendered here for them shows
// dates in.
#[cfg(not(target_arch = "wasm32"))]
mod person_formats;
#[cfg(not(target_arch = "wasm32"))]
mod person_models;
pub use person_models::OpenAiSignIn;
#[cfg(not(target_arch = "wasm32"))]
mod person_settings;
#[cfg(not(target_arch = "wasm32"))]
pub mod person_skills;
// The inboxes such a host watches for its people, with no window open.
#[cfg(not(target_arch = "wasm32"))]
mod mail_watch;
#[cfg(not(target_arch = "wasm32"))]
pub use mail_watch::MailListening;
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
pub mod person_asking;
// What the person keeps to sign in with, in that same vault of theirs.
#[cfg(not(target_arch = "wasm32"))]
mod person_vault;
// A secret used by reference by a turn this server carries out itself.
#[cfg(not(target_arch = "wasm32"))]
pub mod hosted_use;
mod personal;
mod weak;
pub use weak::WeakWorkspaces;
mod publication;
use publication::{Change, SinceBase, starts_over, withdraws};
#[cfg(test)]
use publication::{MAX_PUBLICATION_TAIL, MIN_PUBLICATION_TAIL_BYTES, based};
mod document_format;
mod publisher;
mod record_authority;
#[cfg(feature = "agents")]
pub mod repaired_documents;
mod resources;
mod revocations;
pub use own::OwnWorkspaceOffer;
pub(crate) mod sealed_documents;
pub use device_keys::{
    DeviceKeyGroups, DeviceKeys, GroupKeys, GroupKeysAnswer, GroupKeysAnswerFuture, KeySink,
};
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub use sealed_documents::fixtures as sealed_fixtures;
mod desktop_document;
#[cfg(test)]
mod governed_grants_tests;
#[cfg(test)]
mod naming_tests;
pub mod servers;
mod shares;
#[cfg(test)]
pub(crate) mod tests;
mod unattended;
mod unbuildable_windows;
pub use unattended::{Answer as PersonAnswer, AsksPeople};
mod readings;
#[cfg(feature = "agents")]
mod views;
mod voice;
pub(crate) use voice::carried_call_workspace;
mod wait_list;

use std::collections::HashMap;
use std::path::PathBuf;
// The tests below take it from here, as they take the rest they read.
#[cfg(test)]
use std::path::Path;
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

pub(crate) use actions::{GovernedServer, ServiceWorkspaceActions};
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{AddMemberRequest, CollaborationError, GovernanceCompletion, MemberPlace};
pub use listing::EntryRefusal;
pub use pages::PreAdmitRequest;
use tilo_server_protocol::WorkspaceId;
pub use wait_list::{Mailbox, Secrets as WaitListSecrets, WaitListMail, WaitListed};

// The administration vocabulary (the actions' names and answers), under the
// daemon's historic paths.
pub use tilo_server_session::workspaces::{
    ActionInvitation, CREATE_WORKSPACE, CancelInvitationRequest, CreatedWorkspace, ENTER_WORKSPACE,
    EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE,
    HostedWorkspace, IssueInvitationRequest, IssuedInvitation, LIST_WORKSPACES, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest, UNHOST_WORKSPACE, WorkspaceListing,
    WorkspaceSummary, ask_host, enter_invitation,
};

use crate::peer::WorkspaceRegistry;
use tilo_lock::RwLockRecover as _;

/// The record's version this build writes, and the one it reads: it keeps no
/// name of its own, the governance's being the one (see [`document_format`]).
const DOCUMENT_VERSION: u32 = 5;
/// How long members' writes the governor took in may wait to be written
/// through to the workspace file. A hard stop loses at most these; replaying
/// what the server retains brings them back.
const KEEP_FOLLOWED: Duration = Duration::from_secs(1);
/// Record writes the workspace authority has yet to look at, per workspace.
const DOCUMENT_NOTICES: usize = 256;

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

impl WorkspaceDocumentEntry {
    /// The record `stored` names: `published`, the publications file's, before
    /// the publications it holds, and `receipted`, the receipts file's, before
    /// the receipts its log holds.
    fn read(
        stored: document_format::StoredResource,
        mut published: Vec<Publication>,
        mut receipted: Vec<tilo_peer_state::join_log::JoinCompletionRecord>,
    ) -> Self {
        let mut outgoing = stored.outgoing;
        published.append(&mut outgoing);
        let mut collaboration = stored.collaboration;
        receipted.append(&mut collaboration.join_completions);
        collaboration.join_completions = receipted;
        Self {
            version: DOCUMENT_VERSION,
            workspace: stored.workspace,
            workspace_id: stored.workspace_id,
            owner: stored.owner,
            peer: stored.peer,
            generation: stored.generation,
            outgoing: published,
            published: stored.published,
            collaboration,
        }
    }
}

/// The file of a workspace's join receipts.
type Receipts = appended::Appended<tilo_peer_state::join_log::JoinCompletionRecord>;

/// A workspace this server governs.
struct GovernedWorkspace {
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
    /// Admits the governance each settle retains to the host's order, after
    /// the workspace's lock is released.
    publisher: Arc<publisher::WorkspacePublisher>,
    /// Member writes to follow before compacting a sealed record history
    /// again, after a compaction failed: each attempt opens and merges the
    /// whole history.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    compaction_wait: usize,
    /// The workspace's shared desktop as members' writes of it are decided,
    /// followed from the first such write ([`desktop_writes`]).
    desktop: Option<tilo_desktop_model::shared_resource::guard::DesktopGuard>,
    /// The segments of members' larger desktop writes, until each is whole.
    desktop_segments: collab_types::DocumentReassembler,
    /// Members' writes the governor took in that the workspace file lacks.
    unkept: bool,
    /// Whether writing them through is due already ([`Workspaces::keep_soon`]).
    keeping: bool,
    /// The channels of this workspace's records whose key holders this host
    /// is asking to admit it, by app and record: a burst of writes to a
    /// record the host holds no keys of yet asks once
    /// ([`Workspaces::join_soon`]).
    #[cfg(any(feature = "agents", feature = "web"))]
    asking: std::collections::BTreeSet<(String, String)>,
    /// The workspace's document started over from a base at an admission, and
    /// what the server retains of it from before is still to be archived,
    /// ahead of the base's publication ([`Workspaces::settle`]).
    rebased: bool,
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
        let parts = peer_parts::PeerParts::open(&path)?;
        let (publications, published) =
            appended::Appended::open(&path, PUBLICATIONS, stored.filed_publications)?;
        let (receipts, receipted) =
            appended::Appended::open(&path, RECEIPTS, stored.filed_receipts)?;
        let record = WorkspaceDocumentEntry::read(stored, published, receipted);
        // The record names the workspace's owner, a fact of its membership
        // that every holder keeps as it was handed over; the device opening
        // it only writes what this host changes, and is never taken for the
        // owner.
        if record.published > record.outgoing.len() {
            return Err("workspace record is inconsistent".into());
        }
        let mut peer = StatePeer::restore(
            &parts
                .load(&record.peer)
                .map_err(|error| format!("peer state parts: {error}"))?,
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
        // Parts an interrupted write left behind.
        parts.collect(&record.peer);
        Self::assemble(record, peer, path, parts, (publications, receipts), device)
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
            publisher: Arc::default(),
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            #[cfg(any(feature = "agents", feature = "web"))]
            asking: std::collections::BTreeSet::new(),
            rebased: false,
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
        // What the publisher could not admit is not published yet, whatever
        // `published` says until the next settle takes the failure back.
        let published = match self.publisher.unretained_from() {
            Some(first) if first < self.record.published => first,
            _ => self.record.published,
        };
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
    pub(super) fn commit_held(
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
        if candidate.workspace_id() != replica.workspace_id()
            || candidate.owner() != replica.owner()
        {
            return Err("a workspace change cannot replace the workspace".into());
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
        let mut rebased = false;
        if started_over {
            // Keep unchanged record checkpoints on their own channels; replacing
            // those baselines could discard member writes. A record changed by
            // this commit must still publish at the governance rollover.
            let changed_documents =
                durable_messages(&mut candidate.clone(), replica, (key, now), generation)?
                    .into_iter()
                    .filter_map(|message| match message {
                        neighbour_wire::NeighbourMsg::AppDocument {
                            app_id,
                            document_id,
                            ..
                        } => Some((app_id, document_id)),
                        _ => None,
                    })
                    .collect::<std::collections::BTreeSet<_>>();
            // The workspace's document starts over from a base holding its
            // state as it stands, with none of the history that led there.
            rebased = candidate.rebase_workspace()?;
            since_base = SinceBase::Added;
            record.outgoing = candidate
                .publication_mut()
                .durable_bootstrap_messages(key, now, generation)?
                .iter()
                .filter(|message| match message {
                    neighbour_wire::NeighbourMsg::AppDocument {
                        app_id,
                        document_id,
                        ..
                    } => changed_documents.contains(&(app_id.clone(), document_id.clone())),
                    _ => true,
                })
                .map(|message| publication(message, true))
                .collect::<Result<_, _>>()?;
            record.published = 0;
        } else {
            let diffing = crate::timing::start();
            record.outgoing.extend(self.published_since(replica)?);
            let messages = durable_messages(&mut candidate, replica, (key, now), generation)?;
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
        self.rebased |= rebased;
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
            labels: tilo_server_session::workspaces::WorkspaceLabels::new(),
            unattended: false,
            made_at_ms: self.made_at_ms,
        }
    }
}

use commit::{durable_messages, prepare_governance, publication, select_exact_admission};

/// The extension of the marker beside a workspace file this host released.
const RELEASED: &str = "released";

/// What the workspace authority needs from the server hosting its workspaces.
#[derive(Clone)]
pub(crate) struct WorkspaceServices {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::service::GovernedAccessWriter,
    pub(crate) records: crate::workspace_documents::WorkspaceDocuments,
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: Arc<std::sync::RwLock<RevocationList>>,
    /// The host's metrics, which the sessions it dials count in too.
    pub(crate) metrics: Arc<crate::metrics::Metrics>,
    /// Pre-admissions each workspace keeps, spent ones included.
    pub(crate) max_pre_admissions: usize,
    /// The live handshake trust policy, for [`crate::workspace_access::decide`]
    /// and for what a host holding a workspace with this one carries.
    pub(crate) trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// Agent delegations, for [`crate::workspace_access::decide`] and for
    /// refusing an agent the administration only a person holds.
    pub(crate) agents: crate::peer::AgentRegistry,
    /// The devices this server certified and placed itself, for the windows in its workspaces'
    /// machines ([`crate::environment`]): none of them is a device of a person's own.
    pub(crate) environments: crate::environment::EnvironmentRegistry,
    /// The work this device takes: it serves a workspace placed on it only when the setting
    /// takes the placing person's work.
    pub(crate) work: tilo_server_protocol::WorkSetting,
    /// The relays this device holds back until something needs it reachable.
    pub(crate) reachability: tilo_endpoint_reachability::Reachability,
    /// What this keeper says of itself among the holders of the workspaces it serves, which
    /// its signed address carries.
    pub(crate) holder: Arc<std::sync::RwLock<HolderClaim>>,
    /// Each device whose session to this host authenticated: a host it
    /// dials and could not reach is dialled again then.
    pub(crate) arrivals: crate::attach::Arrivals,
    /// The channel service of the device this host runs on, when the
    /// embedder runs one: the host seals and opens the records of the
    /// workspaces it governs with its keys and admits nobody.
    pub(crate) channels: Option<DeviceKeyGroups>,
    /// The device this embedder's member copies act as, when it keeps any:
    /// what the copies kept from an earlier run sync as, from the open on.
    pub(crate) member: Option<Arc<member::MemberDevice>>,
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
    /// The workspaces it runs a moderation agent for.
    pub(crate) moderates: std::collections::BTreeSet<String>,
}

struct WorkspacesInner {
    host: WorkspaceServices,
    /// The sealed history of every workspace governed here, which their
    /// member devices catch up from over the direct link.
    store: Arc<tilo_peer_state::store::Store>,
    /// The direct links of the workspaces governed here, one per workspace,
    /// answering a device's dial by the workspace it names.
    direct: tilo_peer_state::direct::Serving,
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
    /// Whether this server is about to restart and hands its work to the
    /// other holders: it takes and renews no lease ([`Workspaces::drain`]).
    draining: std::sync::atomic::AtomicBool,
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
    calls: Arc<voice::Calls>,
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
    /// What asks a person whether a workspace of theirs runs commands
    /// unattended ([`Workspaces::ask_people`]).
    asks_people: std::sync::RwLock<Option<Arc<dyn AsksPeople>>>,
    /// What runs the workspaces' Linux machines, as a branch takes its
    /// source's and a deletion takes the workspace's ([`Workspaces::machines`]).
    machines: std::sync::RwLock<Option<Arc<dyn WorkspaceMachines>>>,
    /// Where the name each person gave themselves is kept ([`Workspaces::person_names`]).
    person_names: std::sync::RwLock<Option<Arc<dyn PersonNames>>>,
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
    /// What the changes a record's repair kept wait for, by record, so a
    /// member's write to it is only tried against them when it could release
    /// one ([`repaired_documents`]).
    #[cfg(feature = "agents")]
    repairs: repaired_documents::Repairs,
    /// Where iCloud accounts' calendars are kept, where a test's calendar
    /// server stands in for iCloud's own ([`person_settings`]).
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    icloud_calendars_at: std::sync::RwLock<Option<String>>,
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
    /// Linked members' writes on their way to the server's sessions, each
    /// workspace's in order, each tried until it lands.
    session_carriers: session_carry::SessionCarriers,
    /// How many members' windows each workspace could not take in because they
    /// build on history it no longer has
    /// ([`unbuildable_windows::missing_history`]), by workspace.
    unbuildable_windows: std::sync::Mutex<HashMap<String, u64>>,
    /// When each workspace was last written to, and a ping after each entry
    /// that changed: the directory's own, beside the entries' governance.
    directory: listing::Directory,
    /// Whether the records its workspaces hold in the clear are sealed: only once
    /// the host's operator decided it (the server's `reseal`).
    resealing: std::sync::atomic::AtomicBool,
    /// The entry's wait list, once whoever runs the server offers one
    /// ([`Workspaces::offer_wait_list`], [`Workspaces::mail_wait_list`]).
    entry: wait_list::EntrySettings,
    /// Held while a workspace is let go ([`Workspaces::release`]): whoever
    /// asks after it waits until what the host served of it is gone.
    releasing: AsyncMutex<()>,
    /// The workspaces of people's own, once the host's operator offers them
    /// ([`Workspaces::offer_own_workspaces`]).
    own: std::sync::OnceLock<own::OwnWorkspaces>,
    /// Which people each workspace here lists, by person, as its governance
    /// settled last ([`memberships`]).
    memberships: memberships::Memberships,
    /// The owner invitations the host handed out and nobody accepted yet
    /// ([`host_invitations`]).
    owner_invitations: host_invitations::OwnerInvitations,
    /// The workspace lists people's devices kept here ([`personal`]).
    personal: personal::PersonalLists,
    /// The sessions of people's devices here, sent their person's workspace
    /// list whenever it changes ([`personal::PersonalPushes`]).
    personal_pushes: personal::PersonalPushes,
    /// How many sessions of each device key are open here now, by key: every
    /// session of every person's, so a record says which of their keys is
    /// here ([`personal`]).
    online: std::sync::Mutex<HashMap<[u8; 32], usize>>,
    /// The devices the host's own person runs an agent on, and their open
    /// sessions ([`mcp_server`]).
    mcp_server: mcp_server::McpServer,
    /// The task following where this host answers ([`hosts::Workspaces::follow_own_address`]),
    /// aborted with the host: it holds the endpoint's address watcher, which keeps the
    /// endpoint's socket open until it goes.
    own_address: std::sync::OnceLock<tokio::task::AbortHandle>,
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

/// What [`Workspaces::refound_document`] did: which record was founded again,
/// and how many retained entries its old history took to the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(feature = "agents")]
pub struct Refounded {
    pub app: String,
    pub record: String,
    /// Entries archived off the channel, beside the replay and never deleted.
    pub archived: usize,
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

    /// An authority over `workspace`, for a test of the decision.
    #[cfg(test)]
    pub(crate) fn governed_by(workspace: &str) -> Self {
        Self {
            workspaces: std::sync::Weak::new(),
            workspace: workspace.to_owned(),
        }
    }
}

impl std::fmt::Debug for Workspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspaces")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl Workspaces {
    /// The address this device answers at, as its endpoint has it now.
    pub(super) fn endpoint_addr(&self) -> iroh_base::EndpointAddr {
        self.inner.host.endpoint.addr()
    }

    /// The relays this device holds back until something needs it reachable.
    pub(super) fn reachability(&self) -> &tilo_endpoint_reachability::Reachability {
        &self.inner.host.reachability
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

    /// The content keys of `route`'s workspace, for this host's own endpoints
    /// (the workspace actions it runs for its members' pages and bots), as the
    /// device it runs on answers: `create` opens the channel for the record the
    /// writes are about to start, and otherwise only an existing channel is
    /// read. `None` when this host runs on a device that takes part in no
    /// channel of the workspace.
    pub async fn own_route_keys(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        create: bool,
    ) -> GroupKeysAnswer {
        let device = self.device_keys()?;
        device
            .group_keys(&route.cluster_id, collab_types::EVERY_MEMBER, create)
            .await
    }

    /// Set what this host offers its workspaces, as the entry its person keeps
    /// of it in their admin workspace says: whether it runs their actions and jobs,
    /// how much storage it offers and how many machines it runs. What devices
    /// offering it work are answered with follows it.
    pub fn set_offer(&self, offer: Offer) {
        self.inner.jobs.set_offer(offer);
    }

    /// What this host offers its workspaces ([`Self::set_offer`]).
    #[must_use]
    pub fn offer(&self) -> Offer {
        self.inner.jobs.offer()
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
        let (action_service, jobs) = device_actions::action_service(
            Arc::clone(&workspaces),
            host.key.clone(),
            host.work.clone(),
        );
        let store =
            LinkStore::open(store, Appetite::StoreAll).map_err(|error| error.to_string())?;
        #[cfg(not(target_arch = "wasm32"))]
        let models = Arc::new(person_model::Models::new());
        let this = Self {
            inner: Arc::new(WorkspacesInner {
                #[cfg(test)]
                link_heard_capacity: std::sync::atomic::AtomicUsize::new(0),
                draining: std::sync::atomic::AtomicBool::new(false),
                store: Arc::new(store),
                direct: tilo_peer_state::direct::Serving::new(),
                member: member::Members::default(),
                executions: Arc::new(run_admission::ExecutionPins::new()),
                dir: dir.clone(),
                action_service,
                jobs,
                workspaces,
                readings: readings::Readings::default(),
                own_opened: reads::OwnOpened::stored_in(dir.clone()),
                calls: Arc::default(),
                creating: creating::Creating::default(),
                changes: tokio::sync::broadcast::channel(64).0,
                reached: tokio::sync::broadcast::Sender::new(64),
                deletions: tokio::sync::broadcast::channel(64).0,
                branches: tokio::sync::broadcast::channel(64).0,
                branch_content: std::sync::RwLock::default(),
                asks_people: std::sync::RwLock::default(),
                machines: std::sync::RwLock::default(),
                person_names: std::sync::RwLock::default(),
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
                #[cfg(feature = "agents")]
                repairs: repaired_documents::Repairs::default(),
                #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
                icloud_calendars_at: std::sync::RwLock::default(),
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                session_carriers: session_carry::SessionCarriers::default(),
                unbuildable_windows: std::sync::Mutex::default(),
                directory: listing::Directory::default(),
                resealing: std::sync::atomic::AtomicBool::new(false),
                host,
                entry: wait_list::EntrySettings::default(),
                releasing: AsyncMutex::new(()),
                own: std::sync::OnceLock::new(),
                memberships: memberships::Memberships::default(),
                owner_invitations: host_invitations::OwnerInvitations::open(&dir),
                personal: personal::PersonalLists::new(&dir),
                personal_pushes: personal::PersonalPushes::default(),
                online: std::sync::Mutex::default(),
                mcp_server: mcp_server::McpServer::default(),
                own_address: std::sync::OnceLock::new(),
            }),
        };
        if let Some(device) = this.inner.host.member.clone() {
            this.inner.member.set_device(device);
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
        // Before any member's session reaches them: what a member reads of
        // the workspace's agents is already all of them.
        this.move_agent_rosters().await;
        this.serve_kept_members().await;
        this.adopt_listings_file().await?;
        this.start_following();
        Ok(this)
    }

    /// Serve each workspace this host kept from its last run: bring it up,
    /// settle its governance, name this host's address in it, and recall what
    /// it left running.
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
            self.keep_link_history(&workspace).await;
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
        self.follow_personal_changes();
        self.follow_own_address();
        // The servers the person's document names hold the workspaces it
        // says they do, on every device of theirs that holds it.
        self.follow_servers();
        #[cfg(feature = "agents")]
        self.follow_repaired_documents();
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
    /// workspace after moving it to another host, whose hints its members follow.
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
        self.inner.direct.release(governed.workspace_id());
        // Its sessions to the other holders end with it, and what it was
        // publishing lands before what this host served of it goes.
        drop(std::mem::take(&mut governed.holders));
        Arc::clone(&governed.publisher).flushed().await;
        self.stop_serving(governed.workspace_id()).await?;
        tracing::info!(%workspace, workspace_id = %governed.workspace_id(), "governed workspace released");
        // Nobody lists a workspace this host governs no longer.
        self.inner.memberships.forget(workspace);
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
    pub(super) async fn stop_serving(&self, workspace_id: WorkspaceId) -> Result<(), String> {
        self.inner
            .host
            .access
            .replace(
                workspace_id,
                GovernedAccess {
                    member_devices: Vec::new(),
                    member_user_roots: Vec::new(),
                },
                Vec::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        crate::workspace_io::evict(&self.inner.host.workspaces, workspace_id).await;
        Ok(())
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
    /// No workspace of this host is carried as `workspace_id`, or its
    /// membership could not be written.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the workspace stays held until its membership is written"
    )]
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
        let mut workspace = shared.lock().await;
        if listed {
            workspace.admit_device(device)
        } else {
            workspace.forget_device(&device)
        }
        .map_err(|error| error.to_string())
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

    /// The device that governs every workspace here: this server's own.
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// Whether this server's own person governs `workspace` here: every
    /// workspace it keeps is theirs, except a workspace of someone else's
    /// own, which that person governs.
    #[must_use]
    pub fn governs(&self, workspace: &str) -> bool {
        let person = self.inner.host.cert.user_public_key;
        self.own()
            .and_then(|own| own.owner_of(workspace))
            .is_none_or(|owner| owner == person)
    }

    /// The governor answering the workspace-join exchange for these workspaces.
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

    /// Run the transform an offloadable action declares for the workspaces
    /// governed here as a compute job on `jobs`: the host then answers the
    /// offers of its workspaces' devices with what `jobs` knows of itself, and
    /// runs the invocations it won through it ([`HostJobs`]).
    pub fn run_jobs(&self, jobs: Arc<dyn HostJobs>) {
        self.inner.jobs.serve(jobs);
    }

    /// Answer the workspaces' governance actions their member devices ask on the device action
    /// service through `governance`.
    pub(crate) fn govern_device_actions_with(&self, governance: std::sync::Weak<actions::Shared>) {
        self.inner.jobs.govern_with(governance);
    }

    /// What this host answers a device of its workspaces on the device action service: the work
    /// it was offered, and the workspaces' governance. A device whose session answers that
    /// service in the host's place hands these requests here.
    #[must_use]
    pub fn device_actions(&self) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
        Arc::new(device_actions::Answers(Arc::clone(&self.inner.jobs)))
    }

    /// The current role of `device` only when workspace Write permission is allowed,
    /// including live capability restrictions; `None` otherwise.
    pub async fn legacy_writer_role(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Option<collab_types::role::Role> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        let now = tilo_clock::wall();
        let member = governed.peer().permissions(device, now).ok()?;
        let role = governed.peer().role(&device, now).ok()?;
        drop(governed);
        member
            .allows(
                &member.policy.node(),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
            .then_some(role)
    }

    /// Workspace UUIDs, each sent after a governance decision on that workspace was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// Each member write to a record the governor's copy took in. A slow
    /// receiver may lag; it then re-reads the records it follows.
    #[must_use]
    pub fn document_writes(&self) -> tokio::sync::broadcast::Receiver<ResourceWrite> {
        self.inner.document_writes.subscribe()
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
        #[cfg(any(feature = "agents", feature = "web"))]
        return sealed_documents::readable_now(&reading, (app, record))
            .await
            .map_err(CollaborationError::new);
        #[cfg(not(any(feature = "agents", feature = "web")))]
        Ok(reading.replica.read(|peer| peer.document(app, record)))
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

    /// [`Self::document_routes`] but the members' own records
    /// ([`collab_types::OwnResource`]): each is that person's alone, no record
    /// of the workspace's.
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
        routes.retain(|(app, record)| governed.peer().own_resource(app, record).is_none());
        drop(governed);
        Ok(routes)
    }

    /// Whether `record` of `app` in `workspace` is a member's own
    /// ([`collab_types::OwnResource`]): their drafts and settings, which the
    /// host keeps sealed to their devices and never reads. `false` for a
    /// workspace not governed here.
    pub async fn own_record(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let governed = state.lock().await;
        governed.peer().own_resource(app, record).is_some()
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
    /// acts here by, an agent through its keeper, never what a listing shows: that
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
    /// its keeper, never what [`Self::membership`] shows a person: that leaves the
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

    /// The workspace-join contract's membership answer for `device`, a
    /// device the workspace just admitted to subscribe ([`member_bootstrap`]).
    pub(crate) async fn subscriber_bootstrap(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Option<(Vec<u8>, crate::peer::GovernanceCertificates)> {
        let workspace = self.workspace(workspace).await.ok()?;
        let workspace = workspace.lock().await;
        member_bootstrap(
            &workspace.peer(),
            &workspace.record.owner,
            workspace.workspace_id(),
            workspace.record.generation,
            device,
        )
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

    /// Hold the workspace, admitting only the members its governance names,
    /// and follow its members' writes: create the records they start and keep the
    /// governor's copy of the others current, as calls bind to chat records.
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
        let open = host.workspaces.lock().await.contains_key(&workspace_id);
        if !open {
            let store = crate::store::WorkspaceStore::open_with_payloads(
                host.access.state_root(),
                workspace_id,
                true,
                host.access.shell_payloads().clone(),
            )
            .map_err(|error| error.to_string())?;
            if store
                .load_snapshot()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                let baseline = governed
                    .peer()
                    .admitted_workspace()
                    .ok_or("workspace governance missing")?
                    .clone()
                    .save();
                store
                    .save_snapshot(&baseline)
                    .map_err(|error| error.to_string())?;
            }
        }
        let runtime_state = crate::workspace_io::get_or_initialize(
            &host.workspaces,
            host.access.state_root().to_path_buf(),
            host.access.shell_payloads(),
            workspace_id,
            crate::workspace_access::AdmissionMode::Governed,
            crate::peer::MAX_HELD_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = runtime_state.lock().await;
        let public = Self::entry_of(governed).is_listed();
        runtime.store.set_documents_when_opened(public);
        // This server's authority serves the workspace: never auto-enrol,
        // even if the workspace was autobound before the authority took it.
        if std::mem::replace(
            &mut runtime.admission,
            crate::workspace_access::AdmissionMode::Governed,
        ) != crate::workspace_access::AdmissionMode::Governed
        {
            crate::authority_epoch::bump();
        }
        // The device the workspace was made on governs it; a copy another
        // server handed over keeps that one's governance.
        runtime.governs_here =
            governed.record.owner.device_public_key == self.inner.host.cert.device_public_key;
        runtime.workspace_authority = Some(WorkspaceAuthority {
            workspaces: Arc::downgrade(&self.inner),
            workspace: workspace.clone(),
        });
        if runtime.governor_tap.is_none() {
            let (tap, notices) = crate::workspace::governor_tap(DOCUMENT_NOTICES);
            runtime.governor_tap = Some(tap);
            self.follow(workspace, notices);
        }
        Ok(())
    }

    /// Follow each record write a member makes, see [`Self::absorb`], and
    /// replay what the server retains each time an app surface arrives: the
    /// writes held for it, those let go past the held queue's bound
    /// included, are taken in then rather than when the next write lands.
    fn follow(
        &self,
        workspace: String,
        mut notices: tokio::sync::mpsc::Receiver<crate::workspace::GovernorNotice>,
    ) {
        let workspaces = Arc::downgrade(&self.inner);
        let mut arrivals = crate::surfaces::arrivals();
        arrivals.mark_unchanged();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    notice = notices.recv() => {
                        let Some(crate::workspace::GovernorNotice { author, body, taken }) = notice else { break };
                        let Some(inner) = workspaces.upgrade() else { break };
                        if let Err(error) = (Self { inner }).absorb_kept(&workspace, author, &body).await {
                            tracing::warn!(%workspace, author = %hex::encode(author), %error, "governed workspace did not follow a record write");
                        }
                        let _ = taken.send(());
                    }
                    Ok(()) = arrivals.changed() => {
                        let Some(inner) = workspaces.upgrade() else { break };
                        (Self { inner }).replay_soon(workspace.clone());
                    }
                }
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
        let retained = match self.inner.host.records.retained_settled(workspace_id).await {
            Ok(retained) => retained,
            Err(error) => {
                tracing::warn!(%workspace, %error, "governed workspace did not replay what its members wrote");
                return;
            }
        };
        #[cfg(any(feature = "agents", feature = "web"))]
        self.found_retained(workspace, &state, &retained).await;
        let mut governed = state.lock().await;
        self.recall_retained(&mut governed, retained).await;
        let mut gone = std::collections::BTreeSet::new();
        self.retry_held(&mut governed, &mut gone).await;
        drop(governed);
        self.retire_kept_agents(workspace, &gone).await;
    }

    /// Found every record of `workspace` that `retained` holds a write of and
    /// this host holds none of, from the record's first retained write
    /// ([`Self::absorb`]): the copy of a record the host was admitted to a
    /// channel of after its members started writing it has to start
    /// somewhere, and its later writes build on the first. A record found
    /// first from a later write would refuse that one for the changes it
    /// lacks, and never be founded.
    #[cfg(any(feature = "agents", feature = "web"))]
    async fn found_retained(
        &self,
        workspace: &str,
        state: &tokio::sync::Mutex<GovernedWorkspace>,
        retained: &[crate::workspace_documents::RetainedBody],
    ) {
        let mut first: std::collections::BTreeMap<
            (String, String),
            &crate::workspace_documents::RetainedBody,
        > = std::collections::BTreeMap::new();
        for retained in retained {
            // What the server's own device wrote the governor holds already, or
            // took in as it came ([`Self::recall_retained`]).
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            let Ok(neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                envelope,
                ..
            }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body)
            else {
                continue;
            };
            // A record the host may read in the clear was founded when its
            // first write was handed to the governor: only a sealed one waits
            // for the group to admit this host's device.
            if envelope.encryption.is_none() {
                continue;
            }
            first.entry((app_id, document_id)).or_insert(retained);
        }
        if first.is_empty() {
            return;
        }
        let missing: Vec<_> = {
            let workspace = state.lock().await;
            first
                .into_iter()
                .filter(|((app, record), _)| !workspace.peer().holds_document(app, record))
                .map(|(_, retained)| retained)
                .collect()
        };
        for retained in missing {
            if let Err(error) = self
                .absorb_kept(workspace, retained.author, &retained.body)
                .await
            {
                tracing::warn!(%workspace, %error, "governed workspace did not found a record its members wrote");
            }
        }
    }

    /// Ask the key holders of `app`'s `record`'s channel in `workspace` to
    /// admit this host's device ([`Self::join_channel`]), apart from whatever
    /// asked for it: the group's answer arrives as another write of the
    /// workspace, which waits for the writes ahead of it, a notice in this
    /// one's queue included. Once it is admitted, what the server retains of
    /// the record is replayed ([`Self::replay_soon`]), so its copy starts
    /// from its first write. Asked once per record until the first answer.
    #[cfg(any(feature = "agents", feature = "web"))]
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
        #[cfg(any(feature = "agents", feature = "web"))]
        self.join_soon(&mut governed, app.to_owned(), record.to_owned());
        #[cfg(not(any(feature = "agents", feature = "web")))]
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
        #[cfg(any(feature = "agents", feature = "web"))]
        let waits = governed.seals()
            && (governed
                .asking
                .contains(&(app.to_owned(), record.to_owned()))
                || !governed.holds_keys().await);
        #[cfg(not(any(feature = "agents", feature = "web")))]
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

    /// [`Self::absorb`] for `body`, a write the server already keeps: one a
    /// member sent over its session, which the governor follows as it is kept.
    async fn absorb_kept(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        self.absorb_carried(workspace, author, body, Carried::Kept)
            .await
    }

    /// Take in the record write `body` of `author`, which reached the host
    /// over the workspace's direct link: [`Self::absorb_carried`].
    async fn absorb(&self, workspace: &str, author: [u8; 32], body: &[u8]) -> Result<(), String> {
        self.absorb_carried(workspace, author, body, Carried::Unkept)
            .await
    }

    /// Take in the record write `body` of `author`. A record the workspace lacks
    /// is created from it, if `author` is a device of an editor: records are
    /// born only from a governor checkpoint. A write to a record the workspace has
    /// updates the governor's copy, written through to the workspace file soon
    /// after ([`Self::keep_soon`]).
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
        // Every member's record write reaches the governor here, so this is
        // where one that does not decode is reported (the content classifier
        // that reads the same body before it leaves it to this).
        let message = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
            .map_err(|error| format!("a member's write does not decode: {error}"))?;
        // The server's own device writes records through the workspace too: the
        // actions it runs for members (a visitor linking their Discord
        // account) are committed as that device, and the governor's copy,
        // which the workspace's bots read, must take them in like any member's.
        // Its notices it made itself, so it has them already.
        // A host told of its device's channel service retains its own
        // device's channel governance like any member's, for the other
        // key holders: the host keeps no channels to have it already.
        if author == host.cert.device_public_key
            && message.governance_workspace().is_none()
            && !matches!(
                message,
                neighbour_wire::NeighbourMsg::AppDocument { .. }
                    | neighbour_wire::NeighbourMsg::SyncResp { .. }
            )
        {
            return Ok(());
        }
        let Some(message) = self
            .absorb_notice(workspace, author, message, carried)
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
        // The change is decided against the surface its op names: the one this
        // host runs the app with, or one the workspace keeps for it
        // ([`crate::surfaces`]). What it lacks it asks the workspace's members
        // for, and the change is held until one answers.
        if let Some(surface) = envelope.surface
            && crate::surfaces::kept(&app_id, &hex::encode(surface)).is_none()
        {
            self.note_missing_surface(workspace, &app_id, &hex::encode(surface));
        }
        let workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let waiting = crate::timing::start();
        let mut workspace = workspace.lock().await;
        crate::timing::window("workspace.follow_wait", waiting);
        if workspace.peer().holds_document(&app_id, &document_id) {
            self.follow_write(&mut workspace, author, message, (app_id, document_id))
                .await;
            return Ok(());
        }
        let sealed = envelope.encryption.is_some();
        // Only the record's channel members read a sealed write: a record that
        // starts sealed, as each new part of a chained record does, is followed
        // like any other where this host takes part in its channel. One it may
        // read and holds no keys of yet is joined, and the write is taken in
        // when the group admits this host's device: the record's copy starts
        // from its first write, so it is not kept by not asking.
        #[cfg(any(feature = "agents", feature = "web"))]
        let readable = !sealed || workspace.holds_keys().await;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let readable = !sealed;
        if !readable {
            #[cfg(any(feature = "agents", feature = "web"))]
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
        workspace.change_held(
            &host.key,
            now,
            |_, candidate| {
                if !whole {
                    candidate.publication_mut().publish_document(
                        &app_id,
                        &document_id,
                        automerge::AutoCommit::new().save(),
                        &host.key,
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
                    let written = candidate
                        .document(&app_id, &document_id)
                        .ok_or("created record missing")?;
                    candidate.publication_mut().publish_document(
                        &app_id,
                        &document_id,
                        written,
                        &host.key,
                        now,
                    )?;
                }
                Ok(((), true))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        // The write that starts a document is told like every write after
        // it: an agent a person's first message asks is woken by it.
        self.tell_followers(&workspace, (app_id, document_id), &message)
            .await;
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
        let mut gone = std::collections::BTreeSet::new();
        if !self
            .take_topology(&mut workspace, author, message, &mut gone)
            .await?
        {
            // Held back for history this device lacks: the history the
            // server keeps is taken in again, and the change with it.
            self.replay_soon(workspace.workspace().to_owned());
            return Ok(());
        }
        self.retry_held(&mut workspace, &mut gone).await;
        let identity = workspace.workspace().to_owned();
        drop(workspace);
        self.retire_kept_agents(&identity, &gone).await;
        Ok(())
    }

    /// Try again the changes held back for history (`take_topology`), now
    /// that something landed: those whose history arrived are taken in, the
    /// others held on. The people those changes removed are added to `gone`.
    async fn retry_held(
        &self,
        workspace: &mut GovernedWorkspace,
        gone: &mut std::collections::BTreeSet<[u8; 32]>,
    ) {
        loop {
            let held = workspace.holders.take_held();
            if held.is_empty() {
                break;
            }
            let mut took = false;
            for (author, message) in held {
                match self.take_topology(workspace, author, message, gone).await {
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
    /// history the workspace started over from are refused. The people the
    /// change removed from the workspace are added to `removed`.
    async fn take_topology(
        &self,
        workspace: &mut GovernedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        gone: &mut std::collections::BTreeSet<[u8; 32]>,
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
        let removed = std::cell::RefCell::new(std::collections::BTreeSet::new());
        let mut author_person = None;
        let changed = workspace.change_held(
            &host.key,
            now,
            |replica, candidate| {
                let outcome = candidate
                    .receive_from(TransportOrigin::RetainedServerDelta(author), message, now)
                    .map_err(|error| error.to_string())?;
                if !outcome.changed {
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
        gone.extend(removed);
        self.settle(workspace).await?;
        Ok(true)
    }

    /// Refound `record` of `app` from what the governor itself can still read,
    /// when its retained history lost the base its deltas were written over.
    ///
    /// A record whose retained checkpoint is gone while deltas over it remain
    /// cannot be read by anyone: the governor's own reader meets changes whose
    /// history it lacks, and every device that joins afterwards is replayed a
    /// tail that builds on changes no frame carries. Nothing a member sends
    /// can repair it — a snapshot of the record as one device holds it is not
    /// the record's history — so the repair is the governor's own: it writes
    /// the record as it stands as the workspace's next checkpoint of the
    /// record, which founds the route again for every device reading it.
    ///
    /// Nothing is deleted. What the channel retained until now is archived
    /// ([`WorkspaceStore::archive_channel`]) under the retained directory,
    /// beside the replay that is read, and out of it: an operator can go back
    /// to it, and the history that made the record unreadable is kept where it
    /// cannot stop the workspace being used.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the record is not one the governor
    /// holds, or the new checkpoint could not be published.
    #[cfg(feature = "agents")]
    pub async fn refound_document(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Refounded, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let workspace_id = governed.lock().await.workspace_id();
        let hosted = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| CollaborationError::new("the workspace is not hosted here"))?;
        let channel = neighbour_wire::app_document_channel(app, record);
        let state = self.workspace(workspace).await?;
        let now = tilo_clock::wall();
        let host = &self.inner.host;
        let (readable, keys) = {
            let workspace = state.lock().await;
            if !workspace.peer().holds_document(app, record) {
                return Err(CollaborationError::new("the record is not held here"));
            }
            let readable = workspace
                .readable_document(app, record)
                .await
                .map_err(CollaborationError::new)?
                .ok_or_else(|| CollaborationError::new("the record does not read"))?;
            let keys = workspace.document_keys(app, record).await;
            drop(workspace);
            (readable, keys)
        };
        // Keep what the channel retained until now beside the replay: never
        // deleted, never read by a device joining after this.
        let archived =
            crate::compaction::owner_run(&hosted, move |store| store.archive_channel(&channel))
                .await
                .map_err(CollaborationError::new)?;
        drop(hosted);
        let mut workspace = state.lock().await;
        let mut candidate = workspace.peer().clone();
        let sealed = match keys {
            Ok(Some(keys)) => Some(keys),
            Ok(None) => None,
            Err(error) => return Err(CollaborationError::new(error)),
        };
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
        host.records.settled(workspace_id).await;
        tracing::warn!(
            workspace = %workspace.workspace(),
            %app,
            %record,
            archived,
            "a record whose base its history lost was refounded; its old history is archived"
        );
        Ok(Refounded {
            app: app.to_owned(),
            record: record.to_owned(),
            archived,
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
    /// verdict on a write admission refused.
    async fn follow_write(
        &self,
        workspace: &mut GovernedWorkspace,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        (app, record): (String, String),
    ) {
        let following = crate::timing::start();
        let followed = workspace.follow(author, message.clone());
        if followed.changed {
            self.landed(workspace, &message);
            self.keep_soon(workspace);
        }
        if followed.replay_wanted {
            self.replay_soon(workspace.workspace().to_owned());
        }
        if let Some(reason) = followed.refused
            && let Err(error) = self.file_refusal(workspace, &message, &reason).await
        {
            tracing::warn!(workspace = %workspace.workspace(), %reason, %error, "the governor did not publish a refusal of a record write");
        }
        crate::timing::window("workspace.follow", following);
        #[cfg(any(feature = "agents", feature = "web"))]
        {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(workspace).await {
                tracing::warn!(workspace = %workspace.workspace(), %error, "governed workspace did not compact a sealed record");
            }
            crate::timing::window("workspace.compact", compacting);
        }
        self.tell_followers(workspace, (app, record), &message)
            .await;
    }

    /// File this device's verdict on the write `message` carried, which
    /// admission refused because `reason`, and publish it as the workspace's
    /// next governance: its author reads it live, and every member keeps it.
    /// Answers nothing about the write: that it did not land is the verdict.
    async fn file_refusal(
        &self,
        workspace: &mut GovernedWorkspace,
        message: &neighbour_wire::NeighbourMsg,
        reason: &str,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let now = tilo_clock::wall();
        let filed = workspace.change_held(
            &host.key,
            now,
            |_, candidate| {
                let filed = candidate
                    .workspace_mut()
                    .refuse_write(&host.cert, message, reason, &host.key, now)?;
                Ok((filed, filed))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if !filed {
            return Ok(());
        }
        self.settle(workspace).await
    }

    /// File this device's verdict on `body`, a message it refused because
    /// `reason`, and publish it as the workspace's next governance: the
    /// write's author reads it live or on catch-up, and every member keeps it.
    /// A body that is no document write (a change to the workspace's topology,
    /// say) names no app record and sequence for a verdict to join, and files
    /// nothing.
    ///
    /// # Errors
    /// Why the verdict was not filed, or `body` is not a message.
    pub(crate) async fn publish_refusal(
        &self,
        workspace: &str,
        body: &[u8],
        reason: &str,
    ) -> Result<(), String> {
        let message: neighbour_wire::NeighbourMsg =
            postcard::from_bytes(body).map_err(|error| error.to_string())?;
        let neighbour_wire::NeighbourMsg::AppDocument { cluster_id, .. } = &message else {
            return Ok(());
        };
        // A change names the workspace whose document it writes, and its verdict is filed there: it
        // is the workspace whose document it refused, and the one its author's session reads the
        // verdict in. A document of a workspace this server does not keep is filed where the change
        // was sent instead, so no refusal of a write is left unfiled.
        let target = if cluster_id != workspace && self.workspace(cluster_id).await.is_ok() {
            cluster_id.as_str()
        } else {
            workspace
        };
        let state = self
            .workspace(target)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        self.file_refusal(&mut governed, &message, reason).await
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
    /// its governed members, then retain every governance change not yet
    /// published, then re-derive its calls' key holders. Runs after each
    /// decision, before the decision is answered.
    ///
    /// The changes are handed to the workspace's [`publisher::WorkspacePublisher`]
    /// here, under the workspace's lock, in publication order, each with its
    /// place reserved so that whoever reads the workspace's retention waits for
    /// it. The publisher admits them to the host's order and waits for them
    /// to be durable after the lock is released: the next decision waits
    /// neither for workspace in the workspace owner's queue, which a crowd fills,
    /// nor behind this one's disk writes (and every member write the host
    /// queued before them). A change the publisher could not admit is
    /// published again by the next settle.
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
        crate::timing::event("workspace.settle.access_read", accessing, 0);
        let members = access.member_devices.clone();
        // Who is in the workspace is what its governance just said: the person
        // listing of every one of them is answered from that, not by reading
        // every workspace this host governs.
        self.inner.memberships.record(
            workspace.workspace(),
            access.member_user_roots.iter().copied(),
        );
        // What the workspace keeps as keys its person revoked: refused with how to approve another,
        // and dropped from its access, so every caller is told what to do.
        let revoked = collaboration::revoked_keys(workspace);
        let dropped = host
            .access
            .replace(workspace.workspace_id(), access, revoked)
            .await
            .map_err(|error| error.to_string())?;
        crate::timing::event("workspace.settle.access", accessing, 0);
        // A device the workspace no longer admits is sent to no one: what it
        // retained to introduce itself goes, unless it brings a revocation
        // this workspace holds nowhere else. Every newcomer is sent the
        // revocations the workspace holds beside its governance, and the
        // hellos that bring any other: a hello can be the only record of a
        // revocation, of a device no workspace row lists, and forgetting it
        // would let that device in again.
        if !dropped.is_empty() {
            let hosted = host
                .workspaces
                .lock()
                .await
                .get(&workspace.workspace_id())
                .cloned();
            if let Some(hosted) = hosted {
                let held = workspace.peer().revocations();
                let forgotten = crate::compaction::owner_run(&hosted, move |store| {
                    let payloads = std::sync::Arc::clone(store.payloads());
                    store.forget_hellos(&dropped, &|body| {
                        payloads.identity(body).is_some_and(|(_, brought)| {
                            brought.iter().any(|entry| !held.contains(entry))
                        })
                    })
                })
                .await?;
                tracing::debug!(workspace = %workspace.workspace(), forgotten, "forgot the hellos of devices the workspace no longer admits");
            }
        }
        let settling = crate::timing::start();
        let device_access = workspace.peer().governance().projection(&members, now)?;
        host.access
            .replace_access(workspace.workspace_id(), device_access)
            .await
            .map_err(|error| error.to_string())?;
        crate::timing::event("workspace.settle.device_access", settling, 0);
        let publishing = crate::timing::start();
        let publisher = Arc::clone(&workspace.publisher);
        if workspace.rebased {
            publisher.flushed().await;
            self.archive_workspace_history(workspace).await?;
            workspace.rebased = false;
        }
        let epoch = publisher.reconcile(&mut workspace.record.published);
        let workspace_id = workspace.workspace_id();
        let (publications, unreadable) = workspace.unpublished(&host.records);
        self.announce(workspace, &publications);
        if let Err(error) = unreadable {
            // What precedes it is still published, in order.
            publisher.push(
                &host.records,
                workspace.workspace(),
                workspace_id,
                epoch,
                publications,
            );
            return Err(error);
        }
        publisher.push(
            &host.records,
            workspace.workspace(),
            workspace_id,
            epoch,
            publications,
        );
        crate::timing::event("workspace.settle.publish", publishing, 0);
        let observing = crate::timing::start();
        self.observe_calls(workspace);
        self.dial_holders(workspace);
        self.governance_moved(workspace);
        crate::timing::event("workspace.settle.observe", observing, 0);
        Ok(())
    }

    /// Hand the governance `workspace` just committed to its direct link: the
    /// link carries it to the member devices it holds, and to this device's
    /// own replica when the workspace is hosted here, so a member's device
    /// follows a decision at once rather than at its next catch-up.
    fn announce(&self, workspace: &GovernedWorkspace, publications: &[publisher::Publication]) {
        if publications.is_empty() {
            return;
        }
        let Some(link) = self.inner.direct.link(workspace.workspace_id()) else {
            return;
        };
        for publication in publications {
            match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&publication.commit.body) {
                Ok(message) => {
                    if let Err(error) = link.send(message) {
                        tracing::debug!(%error, "governance did not reach the workspace's links");
                    }
                }
                Err(error) => tracing::warn!(%error, "a governance publication did not encode"),
            }
        }
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

    /// Archive what the server retains of `workspace`'s document, once every
    /// governance change published before is retained: the workspace starts over
    /// from the base its next publication carries, and the history before it is
    /// no newcomer's to read.
    async fn archive_workspace_history(&self, workspace: &GovernedWorkspace) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace_id = workspace.workspace_id();
        let identity = workspace.workspace().to_owned();
        host.records.settled(workspace_id).await;
        let Some((channel, _)) =
            neighbour_wire::retention(&workspace.peer().workspace_message(), true)
        else {
            return Err("the workspace's document has no retention channel".into());
        };
        let hosted = host.workspaces.lock().await.get(&workspace_id).cloned();
        let workspace = hosted.ok_or("the workspace is not hosted here")?;
        let archived =
            crate::compaction::owner_run(&workspace, move |store| store.archive_channel(&channel))
                .await?;
        drop(workspace);
        tracing::info!(workspace = %identity, archived, "the workspace's history before an admission is archived");
        Ok(())
    }

    fn revocations(&self) -> RevocationList {
        self.inner.host.revocations.read_recover().clone()
    }
}
